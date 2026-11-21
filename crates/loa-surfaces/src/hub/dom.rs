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
