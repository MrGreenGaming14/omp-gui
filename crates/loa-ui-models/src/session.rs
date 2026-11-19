//! The session's current model as recorded on the `<session>` element, and the
//! drafts that record a model change there.

use crate::catalog::ModelRef;
use crate::{Error, Result};
use loa_core::{Attrs, Dom, Draft, ElementKind, NodeId, Value, attr};

/// The `model` and `model_role` attributes of a `<session>`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionModel {
    /// Model selector, `provider/id` when written by this crate. Adapters may
    /// write a bare id.
    pub model: Option<String>,
    /// The role the model was picked through, when it follows one.
    pub role: Option<String>,
}

impl SessionModel {
    /// Reads the root `<session>` of `dom`. `None` when the DOM has no session.
    pub fn from_dom(dom: &Dom) -> Option<(NodeId, Self)> {
        let root = dom.root().filter(|n| n.kind == ElementKind::Session)?;
        Some((root.id, Self::from_attrs(&root.attrs)))
    }

    pub(crate) fn from_attrs(attrs: &Attrs) -> Self {
        let read = |name| attrs.get(name).and_then(Value::as_str).map(str::to_owned);
        Self { model: read(attr::MODEL), role: read(attr::MODEL_ROLE) }
    }
}

/// A model picked for the session, optionally through a role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionModelChange {
    pub model: ModelRef,
    /// `Some` when the user picked a role: the session follows that role.
    /// `None` when the user picked a model directly: the session no longer
    /// follows a role.
    pub role: Option<String>,
}

/// The drafts that record `change` on the `<session>` of `dom`: `set model`
/// and `set model_role` (or clearing `model_role` for a direct pick). Values
/// already in place produce no draft, so re-picking the current model writes
/// nothing to the journal.
pub fn model_change_drafts(dom: &Dom, change: &SessionModelChange) -> Result<Vec<Draft>> {
    let (session, current) = SessionModel::from_dom(dom).ok_or(Error::NoSession)?;
    let mut drafts = Vec::with_capacity(2);
    let selector = change.model.selector();
    if current.model.as_deref() != Some(selector.as_str()) {
        drafts.push(Draft::set(session, attr::MODEL, selector));
    }
    match (&change.role, &current.role) {
        (Some(new), Some(old)) if new == old => {}
        (Some(new), _) => drafts.push(Draft::set(session, attr::MODEL_ROLE, new.as_str())),
        (None, Some(_)) => drafts.push(Draft::clear(session, attr::MODEL_ROLE)),
        (None, None) => {}
    }
    Ok(drafts)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use loa_core::{MemorySink, PatchSink};

    /// A sink holding a `<session>` root with the given model attributes.
    pub(crate) fn session_sink(model: Option<&str>, role: Option<&str>) -> MemorySink {
        let sink = MemorySink::new();
        let mut attrs: Vec<(&str, Value)> = vec![
            (attr::ID, "s-1".into()),
            (attr::HARNESS, "omp".into()),
            (attr::NODE, "node-key".into()),
            (attr::CREATED, Value::Int(1)),
        ];
        attrs.extend(model.map(|m| (attr::MODEL, m.into())));
        attrs.extend(role.map(|r| (attr::MODEL_ROLE, r.into())));
        sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, "")).unwrap();
        sink
    }

    fn apply(sink: &MemorySink, change: &SessionModelChange) {
        let drafts = model_change_drafts(&sink.dom(), change).unwrap();
        for draft in drafts {
            sink.commit(draft).unwrap();
        }
    }

    #[test]
    fn picking_a_role_records_model_and_role() {
        let sink = session_sink(Some("anthropic/claude-haiku"), None);
        let change = SessionModelChange {
            model: ModelRef::new("anthropic", "claude-sonnet"),
            role: Some("default".into()),
        };
        apply(&sink, &change);
        let (_, model) = SessionModel::from_dom(&sink.dom()).unwrap();
        assert_eq!(
            model,
            SessionModel { model: Some("anthropic/claude-sonnet".into()), role: Some("default".into()) }
        );
    }

    #[test]
    fn picking_a_model_directly_drops_the_role() {
        let sink = session_sink(Some("anthropic/claude-sonnet"), Some("default"));
        let change = SessionModelChange { model: ModelRef::new("openai", "gpt-5"), role: None };
        apply(&sink, &change);
        let (_, model) = SessionModel::from_dom(&sink.dom()).unwrap();
        assert_eq!(model, SessionModel { model: Some("openai/gpt-5".into()), role: None });
    }

    #[test]
    fn repicking_the_current_model_writes_nothing() {
        let sink = session_sink(Some("anthropic/claude-sonnet"), Some("default"));
        let change = SessionModelChange {
            model: ModelRef::new("anthropic", "claude-sonnet"),
            role: Some("default".into()),
        };
        assert!(model_change_drafts(&sink.dom(), &change).unwrap().is_empty());
    }

    #[test]
    fn switching_role_to_same_model_only_sets_the_role() {
        let sink = session_sink(Some("anthropic/claude-sonnet"), Some("default"));
        let change =
            SessionModelChange { model: ModelRef::new("anthropic", "claude-sonnet"), role: Some("plan".into()) };
        apply(&sink, &change);
        assert_eq!(sink.patches().len(), 2, "session insert plus one set");
        let (_, model) = SessionModel::from_dom(&sink.dom()).unwrap();
        assert_eq!(model.role.as_deref(), Some("plan"));
    }

    #[test]
    fn a_dom_without_session_is_an_error() {
        let change = SessionModelChange { model: ModelRef::new("a", "b"), role: None };
        assert_eq!(model_change_drafts(&Dom::new(), &change), Err(Error::NoSession));
    }
}
