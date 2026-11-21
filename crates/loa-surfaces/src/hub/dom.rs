//! `<surface>` elements: a surface opened during a session, recorded under
//! the turn or tool call that opened it, or the session itself.

use crate::hub::service::Origin;
use loa_core::{Draft, ElementKind, NodeId, SurfaceStatus, Value, attr};
use loa_contracts::surface::SurfaceInfo;

/// Inserts `<surface uri mode name width height server status="open">`
/// under `parent` (a session, turn or call node).
pub fn opened(parent: NodeId, info: &SurfaceInfo, origin: &Origin) -> Draft {
    let mut attrs = vec![
        (attr::URI, Value::from(info.uri.as_str())),
        (attr::MODE, Value::from(info.mode.as_str())),
        (attr::NAME, Value::from(info.name.as_str())),
        (attr::SERVER, Value::from(origin.server())),
        (attr::STATUS, Value::from(SurfaceStatus::Open.as_str())),
    ];
    if info.width > 0 && info.height > 0 {
        attrs.push((attr::WIDTH, Value::Int(i64::from(info.width))));
        attrs.push((attr::HEIGHT, Value::Int(i64::from(info.height))));
    }
    Draft::insert(parent, ElementKind::Surface, attrs, "")
}

/// Marks the `<surface>` element `node` closed.
pub fn closed(node: NodeId) -> Draft {
    Draft::set(node, attr::STATUS, SurfaceStatus::Closed.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Dom, MemorySink, PatchSink};
    use loa_contracts::surface::{SurfaceMode, SurfaceUri};

    #[test]
    fn opened_and_closed_surfaces_validate_against_the_schema() {
        let sink = MemorySink::default();
        let session = sink
            .commit(Draft::insert(
                NodeId::DOCUMENT,
                ElementKind::Session,
                [
                    (attr::ID, Value::from("s1")),
                    (attr::HARNESS, Value::from("omp")),
                    (attr::NODE, Value::from("node")),
                    (attr::CREATED, Value::Int(1)),
                ],
                "",
            ))
            .unwrap()
            .node;
        let info = SurfaceInfo {
            uri: SurfaceUri::new("browser", "tab1").unwrap(),
            name: "Example".into(),
            width: 1280,
            height: 720,
            mode: SurfaceMode::Pixel,
            mime: None,
        };
        let node = sink.commit(opened(session, &info, &Origin::Browser)).unwrap().node;
        sink.commit(closed(node)).unwrap();
        let dom: Dom = sink.dom();
        let surface = dom.get(node).unwrap();
        assert_eq!(surface.kind, ElementKind::Surface);
        assert_eq!(surface.attrs.get(attr::URI).and_then(Value::as_str), Some("surface://browser/tab1"));
        assert_eq!(surface.attrs.get(attr::SERVER).and_then(Value::as_str), Some("browser"));
        assert_eq!(surface.attrs.get(attr::STATUS).and_then(Value::as_str), Some("closed"));
        assert_eq!(surface.attrs.get(attr::WIDTH), Some(&Value::Int(1280)));
    }
}
