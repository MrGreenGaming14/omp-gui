//! The composer chip that shows the session's role and model, for example
//! "default · claude-sonnet". Built from the `<session>` attributes `model` and
//! `model_role` and kept current by applying the session's patches.

use crate::session::SessionModel;
use crate::style;
use iced::widget::{button, container, row, text, tooltip};
use iced::{Element, Task};
use loa_core::{Dom, ElementKind, NodeId, Op, Patch, Value, attr};
use loa_ui_kit::icons::Icon;

/// Separator between role and model in the chip label.
const SEPARATOR: &str = " · ";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    session: Option<NodeId>,
    current: SessionModel,
}

#[derive(Clone, Debug)]
pub enum Message {
    Pressed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The user wants to change the model: open the picker for the session.
    OpenPicker,
}

impl State {
    /// A chip for the session in `dom`. Empty when the DOM has no session yet.
    pub fn from_dom(dom: &Dom) -> Self {
        match SessionModel::from_dom(dom) {
            Some((id, current)) => Self { session: Some(id), current },
            None => Self::default(),
        }
    }

    /// Follows one committed patch of the session. Returns whether the chip
    /// changed.
    pub fn apply(&mut self, patch: &Patch) -> bool {
        match &patch.op {
            Op::Insert { parent, kind: ElementKind::Session, attrs, .. } if *parent == NodeId::DOCUMENT => {
                self.session = Some(patch.node);
                self.current = SessionModel::from_attrs(attrs);
                true
            }
            Op::Set { name, value } if Some(patch.node) == self.session => {
                let slot = match name.as_str() {
                    attr::MODEL => &mut self.current.model,
                    attr::MODEL_ROLE => &mut self.current.role,
                    _ => return false,
                };
                let new = value.as_ref().and_then(Value::as_str);
                if slot.as_deref() == new {
                    return false;
                }
                *slot = new.map(str::to_owned);
                true
            }
            Op::Remove if Some(patch.node) == self.session => {
                *self = Self::default();
                true
            }
            _ => false,
        }
    }

    /// The model and role the chip shows.
    pub fn current(&self) -> &SessionModel {
        &self.current
    }

    /// The chip text: `role · model` with the model id without its provider,
    /// just the model when the session follows no role, and "Model" when the
    /// session has no model yet.
    pub fn label(&self) -> String {
        let Some(model) = self.current.model.as_deref() else {
            return "Model".to_owned();
        };
        let short = loa_contracts::harness::selector::ModelRef::split(model).map_or(model, |(_, id)| id);
        match self.current.role.as_deref() {
            Some(role) => format!("{role}{SEPARATOR}{short}"),
            None => short.to_owned(),
        }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Pressed => (Task::none(), Some(Event::OpenPicker)),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let chip = button(
            row![text(self.label()).size(style::BODY), loa_ui_kit::icons::muted(Icon::ChevronDown, 12.0)]
                .spacing(4)
                .align_y(iced::Center),
        )
        .padding([4.0, 6.0])
        .style(style::flat)
        .on_press(Message::Pressed);
        let detail = match (&self.current.role, &self.current.model) {
            (Some(role), Some(model)) => format!("Role {role}, model {model}"),
            (None, Some(model)) => format!("Model {model}"),
            _ => "Choose a model".to_owned(),
        };
        let detail = container(text(detail).size(style::SMALL)).padding([4.0, 8.0]).style(style::popup);
        tooltip(chip, detail, tooltip::Position::Top).into()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::model_change_drafts;
    use crate::session::tests::session_sink;
    use crate::{ModelRef, SessionModelChange};
    use loa_core::{Draft, PatchSink};

    #[test]
    fn label_shows_role_and_model_without_provider() {
        let sink = session_sink(Some("anthropic/claude-sonnet"), Some("default"));
        assert_eq!(State::from_dom(&sink.dom()).label(), "default · claude-sonnet");
    }

    #[test]
    fn label_without_role_or_model() {
        let sink = session_sink(Some("openrouter/anthropic/claude"), None);
        assert_eq!(State::from_dom(&sink.dom()).label(), "anthropic/claude");
        let sink = session_sink(None, None);
        assert_eq!(State::from_dom(&sink.dom()).label(), "Model");
    }

    #[test]
    fn follows_patches_from_session_insert_through_model_changes() {
        let sink = session_sink(Some("anthropic/claude-sonnet"), Some("default"));
        let mut chip = State::default();
        for patch in sink.patches() {
            chip.apply(&patch);
        }
        assert_eq!(chip.label(), "default · claude-sonnet");

        let change = SessionModelChange { model: ModelRef::new("openai", "gpt-5"), role: Some("slow".into()) };
        for draft in model_change_drafts(&sink.dom(), &change).unwrap() {
            assert!(chip.apply(&sink.commit(draft).unwrap()));
        }
        assert_eq!(chip.label(), "slow · gpt-5");

        let direct = SessionModelChange { model: ModelRef::new("openai", "gpt-5-mini"), role: None };
        for draft in model_change_drafts(&sink.dom(), &direct).unwrap() {
            chip.apply(&sink.commit(draft).unwrap());
        }
        assert_eq!(chip.label(), "gpt-5-mini");
        assert_eq!(chip, State::from_dom(&sink.dom()), "patch-fed chip matches a fresh read");
    }

    #[test]
    fn ignores_patches_for_other_nodes_and_attributes() {
        let sink = session_sink(Some("anthropic/claude-sonnet"), Some("default"));
        let session = sink.dom().root().unwrap().id;
        let mut chip = State::from_dom(&sink.dom());
        let turn = sink
            .commit(Draft::insert(session, ElementKind::Turn, [(attr::STATUS, Value::from("running"))], ""))
            .unwrap();
        assert!(!chip.apply(&turn));
        assert!(!chip.apply(&sink.commit(Draft::set(session, attr::TITLE, "Hello")).unwrap()));
        assert_eq!(chip.label(), "default · claude-sonnet");
    }

    #[test]
    fn removing_the_session_resets_the_chip() {
        let sink = session_sink(Some("anthropic/claude-sonnet"), None);
        let mut chip = State::from_dom(&sink.dom());
        let session = sink.dom().root().unwrap().id;
        assert!(chip.apply(&sink.commit(Draft::remove(session)).unwrap()));
        assert_eq!(chip.label(), "Model");
    }
}
