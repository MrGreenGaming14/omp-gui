//! The composer chip showing the session's role and model ("default · claude-sonnet"), kept
//! current by applying the session's patches.

use super::session::SessionModel;
use iced::widget::{button, container, row, tooltip};
use iced::{Alignment, Element};
use loa_core::{Dom, ElementKind, NodeId, Op, Patch, Value, attr};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole, TextRole, sp};
use loa_ui_kit::widgets::{Msg, Screen, label};
use std::convert::Infallible;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    session: Option<NodeId>,
    current: SessionModel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Open the model picker for the session.
    OpenPicker,
}

pub type Message = Msg<Infallible, Event>;

impl State {
    /// A chip for the session in `dom`; empty while it has none.
    pub fn from_dom(dom: &Dom) -> Self { SessionModel::from_dom(dom).map_or_else(Self::default, |(id, current)| Self { session: Some(id), current }) }

    pub fn current(&self) -> &SessionModel { &self.current }

    /// Follows one committed patch of the session; returns whether the chip changed.
    pub fn apply(&mut self, patch: &Patch) -> bool {
        match &patch.op {
            Op::Insert { parent, kind: ElementKind::Session, attrs, .. } if *parent == NodeId::DOCUMENT => {
                *self = Self { session: Some(patch.node), current: SessionModel::from_attrs(attrs) };
                true
            }
            Op::Set { name, value } if Some(patch.node) == self.session => {
                let slot = match name.as_str() {
                    attr::MODEL => &mut self.current.model,
                    attr::MODEL_ROLE => &mut self.current.role,
                    _ => return false,
                };
                let new = value.as_ref().and_then(Value::as_str);
                let changed = slot.as_deref() != new;
                *slot = new.map(str::to_owned);
                changed
            }
            Op::Remove if Some(patch.node) == self.session => {
                *self = Self::default();
                true
            }
            _ => false,
        }
    }

    /// `role · model` without the model's provider, the model alone without a role, "Model" without one.
    pub fn label(&self) -> String {
        let Some(model) = self.current.model.as_deref() else { return "Model".to_owned() };
        let short = super::ModelRef::split(model).map_or(model, |(_, id)| id);
        self.current.role.as_ref().map_or_else(|| short.to_owned(), |role| format!("{role} · {short}"))
    }

    pub fn view(&self) -> Element<'_, Message> {
        let chip = row![label(self.label(), TextRole::Body, Tone::Normal), tinted(Icon::ChevronDown, TextRole::Caption, Tone::Muted)].spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center);
        let chip = button(chip).padding([sp(SpaceRole::Xs), sp(SpaceRole::S)]).style(style::button(ButtonKind::Flat)).on_press(Msg::Emit(Event::OpenPicker));
        let detail = match (&self.current.role, &self.current.model) {
            (Some(role), Some(model)) => format!("Role {role}, model {model}"),
            (None, Some(model)) => format!("Model {model}"),
            _ => "Choose a model".to_owned(),
        };
        let detail = container(label(detail, TextRole::Caption, Tone::Normal)).padding([sp(SpaceRole::Xs), sp(SpaceRole::S)]).style(style::container(Surface::Tooltip));
        tooltip(chip, detail, tooltip::Position::Top).into()
    }
}

impl Screen for State {
    type Local = Infallible;
    type Event = Event;
    fn reduce(&mut self, m: Infallible) -> Option<Event> { match m {} }
    fn guard(&self, _: &Event) -> bool { true }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::role_draft;
    use crate::models::session::tests::session_sink;
    use loa_core::{Draft, PatchSink};
    use loa_ui_kit::testkit::click_label;

    #[test]
    fn label_follows_patches_from_insert_to_remove() {
        let labels = [(Some("anthropic/claude-sonnet"), Some("default"), "default · claude-sonnet"), (Some("openrouter/anthropic/claude"), None, "anthropic/claude"), (None, None, "Model")];
        for (model, role, want) in labels {
            assert_eq!(State::from_dom(&session_sink(model, role).dom()).label(), want);
        }
        let sink = session_sink(Some("anthropic/claude-sonnet"), Some("default"));
        let mut chip = State::default();
        for p in sink.patches() {
            chip.apply(&p);
        }
        let session = sink.dom().root().unwrap().id;
        assert!(chip.apply(&sink.commit(Draft::set(session, attr::MODEL, "openai/gpt-5")).unwrap()));
        assert!(chip.apply(&sink.commit(role_draft(&sink.dom(), Some("slow")).unwrap()).unwrap()));
        assert_eq!((chip.label(), &chip), ("slow · gpt-5".to_owned(), &State::from_dom(&sink.dom())), "patch-fed chip matches a fresh read");
        let turn = sink.commit(Draft::insert(session, ElementKind::Turn, [(attr::STATUS, Value::from("running"))], "")).unwrap();
        assert!(!chip.apply(&turn) && !chip.apply(&sink.commit(Draft::set(session, attr::TITLE, "Hello")).unwrap()));
        assert!(chip.apply(&sink.commit(Draft::remove(session)).unwrap()));
        assert_eq!(chip.label(), "Model");
    }

    #[test]
    fn clicking_the_chip_opens_the_picker() {
        let mut chip = State::from_dom(&session_sink(Some("openai/gpt-5"), None).dom());
        let events: Vec<Event> = click_label(chip.view(), "gpt-5").into_iter().filter_map(|m| chip.update(m)).collect();
        assert_eq!(events, [Event::OpenPicker]);
    }
}
