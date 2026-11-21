//! The session's model as its `<session>` element records it (`model`, `model_role`), and the
//! draft recording which role it follows.

use super::ModelRef;
use loa_core::{Attrs, Dom, Draft, ElementKind, NodeId, Value, attr};

/// The `model` and `model_role` attributes of a `<session>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionModel {
    /// Model selector, `provider/id` or a bare id some harnesses write.
    pub model: Option<String>,
    /// The role the model was picked through, when it follows one.
    pub role: Option<String>,
}

impl SessionModel {
    /// The root `<session>` of `dom` and its model; `None` without a session.
    pub fn from_dom(dom: &Dom) -> Option<(NodeId, Self)> {
        let root = dom.root().filter(|n| n.kind == ElementKind::Session)?;
        Some((root.id, Self::from_attrs(&root.attrs)))
    }

    pub(crate) fn from_attrs(attrs: &Attrs) -> Self {
        let read = |name| attrs.get(name).and_then(Value::as_str).map(str::to_owned);
        Self { model: read(attr::MODEL), role: read(attr::MODEL_ROLE) }
    }
}

/// A model picked for the session: through `role` it follows that role, without one it follows none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionModelChange {
    pub model: ModelRef,
    pub role: Option<String>,
}

/// The draft recording that the session in `dom` follows `role` (or no role); `None` when that is
/// recorded already or there is no session. The harness records the model itself.
pub fn role_draft(dom: &Dom, role: Option<&str>) -> Option<Draft> {
    let (session, current) = SessionModel::from_dom(dom)?;
    match (role, current.role.as_deref()) {
        (new, old) if new == old => None,
        (Some(new), _) => Some(Draft::set(session, attr::MODEL_ROLE, new)),
        (None, _) => Some(Draft::clear(session, attr::MODEL_ROLE)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use loa_core::{MemorySink, PatchSink};

    /// A sink holding a `<session>` root with the given model attributes.
    pub(crate) fn session_sink(model: Option<&str>, role: Option<&str>) -> MemorySink {
        let sink = MemorySink::new();
        let mut attrs: Vec<(&str, Value)> = vec![(attr::ID, "s-1".into()), (attr::HARNESS, "omp".into()), (attr::NODE, "node".into()), (attr::CREATED, Value::Int(1))];
        attrs.extend(model.map(|m| (attr::MODEL, m.into())).into_iter().chain(role.map(|r| (attr::MODEL_ROLE, r.into()))));
        sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")).unwrap();
        sink
    }

    #[test]
    fn role_drafts_record_only_changes() {
        // (recorded role, picked role, role afterwards, drafts written)
        let cases = [(None, Some("default"), Some("default"), 1), (Some("default"), None, None, 1), (Some("default"), Some("plan"), Some("plan"), 1), (Some("plan"), Some("plan"), Some("plan"), 0), (None, None, None, 0)];
        for (recorded, picked, after, writes) in cases {
            let sink = session_sink(Some("anthropic/claude"), recorded);
            let draft = role_draft(&sink.dom(), picked);
            assert_eq!(draft.is_some() as usize, writes, "{recorded:?} -> {picked:?}");
            if let Some(d) = draft {
                sink.commit(d).unwrap();
            }
            assert_eq!(SessionModel::from_dom(&sink.dom()).unwrap().1, SessionModel { model: Some("anthropic/claude".into()), role: after.map(Into::into) });
        }
        assert_eq!(role_draft(&Dom::new(), Some("default")), None, "no session, nothing to record");
    }
}
