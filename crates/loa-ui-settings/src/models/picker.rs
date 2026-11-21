//! The model picker: a searchable popup of the models grouped by provider, showing the roles
//! each serves. It picks the session's model (directly or through a role) or a role's model.

use super::session::SessionModelChange;
use super::{Catalog, ModelRef};
use iced::event::{self, Event as RuntimeEvent};
use iced::keyboard::{self, key::Named};
use iced::widget::operation::{self, RelativeOffset};
use iced::widget::{Column, column, container, row, scrollable};
use iced::{Alignment, Element, Subscription, Task};
use loa_ui_kit::Icon;
use loa_ui_kit::style::{self, Surface, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, width};
use loa_ui_kit::widgets::{Msg, Screen, icon_button, label, two_line_row};

const SEARCH_ID: &str = "loa-model-picker-search";
const LIST_ID: &str = "loa-model-picker-list";

/// What a pick changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The session's model; roles are rows too, and picking one makes the session follow it.
    Session,
    /// The model a role resolves to.
    Role(String),
}

/// One line of the filtered list: an index into the catalog's roles or models.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Role(usize),
    Model(usize),
}

#[derive(Clone, Debug)]
pub enum Local {
    Query(String),
    Up,
    Down,
    /// Picks the selected row.
    Submit,
    /// Picks this row of [`State::rows`].
    Pick(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Switch the session's model (see [`super::role_draft`] for the role).
    SetSessionModel(SessionModelChange),
    /// Assign `model` to `role` in the harness config.
    SetRoleModel { role: String, model: ModelRef },
    /// Dismissed without a pick.
    Close,
}

pub type Message = Msg<Local, Event>;

#[derive(Clone, Debug)]
pub struct State {
    catalog: Catalog,
    target: Target,
    /// The model the target uses now, highlighted and selected first.
    current: Option<ModelRef>,
    /// Lowercase search text per model: ids, names and the roles it serves.
    haystacks: Vec<String>,
    /// Display order of each model's provider: its first appearance in the catalog.
    provider_rank: Vec<usize>,
    query: String,
    rows: Vec<Row>,
    selected: usize,
}

impl State {
    /// Opens the picker; the task focuses the search field.
    pub fn open(catalog: Catalog, target: Target, current: Option<ModelRef>) -> (Self, Task<Message>) {
        let mut providers: Vec<&str> = Vec::new();
        let provider_rank = catalog.models.iter().map(|m| providers.iter().position(|p| *p == m.model.provider).unwrap_or_else(|| { providers.push(&m.model.provider); providers.len() - 1 })).collect();
        let haystacks = catalog.models.iter().map(|m| {
            let roles = catalog.roles.iter().filter(|r| r.model == m.model).map(|r| r.role.as_str());
            [m.model.provider.as_str(), m.provider_name.as_str(), m.model.id.as_str(), m.name.as_str()].into_iter().chain(roles).collect::<Vec<_>>().join(" ").to_lowercase()
        }).collect();
        let mut state = Self { catalog, target, current, haystacks, provider_rank, query: String::new(), rows: Vec::new(), selected: 0 };
        state.refilter();
        state.selected = state.current.as_ref().and_then(|c| state.rows.iter().position(|r| matches!(r, Row::Model(m) if state.catalog.models[*m].model == *c))).unwrap_or(0);
        (state, operation::focus(SEARCH_ID))
    }

    pub fn target(&self) -> &Target { &self.target }
    pub fn rows(&self) -> &[Row] { &self.rows }
    /// Index into [`State::rows`] of the highlighted row.
    pub fn selected(&self) -> Option<usize> { (!self.rows.is_empty()).then_some(self.selected) }

    /// The rows under their headings: "Roles" first when picking for the session, then one
    /// group per provider, as (title, first row index, rows).
    pub fn groups(&self) -> Vec<(&str, usize, &[Row])> {
        let mut groups: Vec<(&str, usize, &[Row])> = Vec::new();
        for (i, row) in self.rows.iter().enumerate() {
            let title = match *row {
                Row::Role(_) => "Roles",
                Row::Model(m) => self.catalog.models[m].provider_display_name(),
            };
            match groups.last_mut() {
                Some(g) if g.0 == title => g.2 = &self.rows[g.1..=i],
                _ => groups.push((title, i, &self.rows[i..=i])),
            }
        }
        groups
    }

    /// Roles resolving to `model`.
    pub fn roles_of<'a>(&'a self, model: &'a ModelRef) -> impl Iterator<Item = &'a str> + 'a { self.catalog.roles.iter().filter(move |r| r.model == *model).map(|r| r.role.as_str()) }

    /// Keeps the selected row in view after a move.
    pub fn scroll(&self) -> Task<Message> {
        let y = if self.rows.len() < 2 { 0.0 } else { self.selected as f32 / (self.rows.len() - 1) as f32 };
        operation::snap_to(LIST_ID, RelativeOffset { x: None, y: Some(y) })
    }

    /// Arrow keys move the selection and Escape closes, also while the search field has focus.
    pub fn subscription(&self) -> Subscription<Message> {
        event::listen_with(|event, _, _| match event {
            RuntimeEvent::Keyboard(keyboard::Event::KeyPressed { key: keyboard::Key::Named(named), .. }) => match named {
                Named::ArrowUp => Some(Msg::Local(Local::Up)),
                Named::ArrowDown => Some(Msg::Local(Local::Down)),
                Named::Escape => Some(Msg::Emit(Event::Close)),
                _ => None,
            },
            _ => None,
        })
    }

    pub fn view(&self) -> Element<'_, Message> {
        let heading = match &self.target {
            Target::Session => "Model for this session".to_owned(),
            Target::Role(role) => format!("Model for role {role}"),
        };
        let search = crate::field("Search models", &self.query, |q| Msg::Local(Local::Query(q))).id(SEARCH_ID).on_submit(Msg::Local(Local::Submit));
        let groups = self.groups().into_iter().map(|(title, start, rows)| {
            let rows = rows.iter().enumerate().map(|(offset, row)| self.row_view(start + offset, *row));
            column![label(title, TextRole::Caption, Tone::Muted)].extend(rows).spacing(sp(SpaceRole::Xs) / 2.0).into()
        });
        let empty = self.rows.is_empty().then(|| label("No models match", TextRole::Label, Tone::Muted));
        let list = Column::with_children(groups).push(empty).spacing(sp(SpaceRole::S));
        let head = row![label(heading, TextRole::Body, Tone::Normal), iced::widget::Space::new().width(iced::Fill), icon_button(Icon::X, "Close", Some(Msg::Emit(Event::Close)))].align_y(Alignment::Center);
        let list = scrollable(list).id(LIST_ID).height(width(LayoutRole::ListWidth)).style(style::scrollbar(false));
        let body = column![head, search, list].spacing(sp(SpaceRole::S));
        container(body).padding(sp(SpaceRole::S)).width(width(LayoutRole::PopupWidth)).style(style::container(Surface::Popup)).into()
    }

    fn row_view(&self, index: usize, row: Row) -> Element<'_, Message> {
        let (title, detail) = match row {
            Row::Role(r) => (self.catalog.roles[r].role.as_str(), self.catalog.roles[r].model.selector()),
            Row::Model(m) => {
                let info = &self.catalog.models[m];
                let roles = self.roles_of(&info.model).collect::<Vec<_>>().join(", ");
                let current = if self.current.as_ref() == Some(&info.model) { " · current" } else { "" };
                (info.display_name(), if roles.is_empty() { format!("{}{current}", info.model.id) } else { format!("{} · {roles}{current}", info.model.id) })
            }
        };
        two_line_row(title, detail, index == self.selected, Msg::Local(Local::Pick(index)))
    }

    fn pick(&self, index: usize) -> Option<Event> {
        Some(match (*self.rows.get(index)?, &self.target) {
            (Row::Role(r), Target::Session) => {
                let role = &self.catalog.roles[r];
                Event::SetSessionModel(SessionModelChange { model: role.model.clone(), role: Some(role.role.clone()) })
            }
            (Row::Model(m), Target::Session) => Event::SetSessionModel(SessionModelChange { model: self.catalog.models[m].model.clone(), role: None }),
            (Row::Model(m), Target::Role(role)) => Event::SetRoleModel { role: role.clone(), model: self.catalog.models[m].model.clone() },
            // Role rows exist only for the session target.
            (Row::Role(_), Target::Role(_)) => return None,
        })
    }

    /// The rows every whitespace separated query term appears in (case-insensitively).
    fn refilter(&mut self) {
        let query = self.query.to_lowercase();
        let matches = |hay: &str| query.split_whitespace().all(|t| hay.contains(t));
        let roles = self.catalog.roles.iter().enumerate().filter(|(_, r)| self.target == Target::Session && matches(&format!("{} {}", r.role, r.model).to_lowercase()));
        let mut models: Vec<usize> = (0..self.haystacks.len()).filter(|&i| matches(&self.haystacks[i])).collect();
        models.sort_by_key(|&i| self.provider_rank[i]);
        self.rows = roles.map(|(i, _)| Row::Role(i)).chain(models.into_iter().map(Row::Model)).collect();
    }
}

impl Screen for State {
    type Local = Local;
    type Event = Event;

    fn reduce(&mut self, m: Local) -> Option<Event> {
        match m {
            Local::Query(query) => {
                self.query = query;
                self.refilter();
                self.selected = 0;
            }
            Local::Up => self.selected = self.selected.saturating_sub(1),
            Local::Down => self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1)),
            Local::Submit => return self.pick(self.selected),
            Local::Pick(index) => return self.pick(index),
        }
        None
    }

    /// Only closing is emitted directly.
    fn guard(&self, e: &Event) -> bool { *e == Event::Close }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_settings::models::selector::{ModelInfo, RoleAssignment};
    use loa_ui_kit::testkit::click_label;

    fn catalog() -> Catalog {
        let info = |provider: &str, provider_name: &str, id: &str, name: &str| ModelInfo { model: ModelRef::new(provider, id), name: name.into(), provider_name: provider_name.into() };
        let role = |role: &str, id: &str| RoleAssignment { role: role.into(), model: ModelRef::new("anthropic", id) };
        let models = vec![info("anthropic", "Anthropic", "claude-sonnet", "Claude Sonnet"), info("openai", "OpenAI", "gpt-5", "GPT-5"), info("anthropic", "Anthropic", "claude-haiku", "Claude Haiku"), info("openai", "OpenAI", "gpt-5-mini", "GPT-5 mini")];
        Catalog { models, roles: vec![role("default", "claude-sonnet"), role("smol", "claude-haiku")] }
    }

    fn titles(s: &State) -> Vec<(String, Vec<String>)> {
        let name = |r: &Row| match *r { Row::Role(i) => s.catalog.roles[i].role.clone(), Row::Model(i) => s.catalog.models[i].model.id.clone() };
        s.groups().into_iter().map(|(t, _, rows)| (t.to_owned(), rows.iter().map(name).collect())).collect()
    }

    fn group(title: &str, rows: &[&str]) -> (String, Vec<String>) { (title.into(), rows.iter().map(|r| (*r).into()).collect()) }
    fn session(provider: &str, id: &str, role: Option<&str>) -> Option<Event> { Some(Event::SetSessionModel(SessionModelChange { model: ModelRef::new(provider, id), role: role.map(Into::into) })) }

    #[test]
    fn search_groups_by_provider_and_finds_roles() {
        let all = vec![group("Roles", &["default", "smol"]), group("Anthropic", &["claude-sonnet", "claude-haiku"]), group("OpenAI", &["gpt-5", "gpt-5-mini"])];
        let cases = [(Target::Session, "", all), (Target::Session, "OPENAI mini", vec![group("OpenAI", &["gpt-5-mini"])]), (Target::Role("plan".into()), "smol", vec![group("Anthropic", &["claude-haiku"])])];
        for (target, query, want) in cases {
            let (mut s, _) = State::open(catalog(), target, None);
            s.update(Msg::Local(Local::Query(query.into())));
            assert_eq!(titles(&s), want, "{query}");
        }
    }

    #[test]
    fn keys_and_picks_reduce_to_model_changes() {
        // (target, current model, messages, selected afterwards, last event)
        let local = |l| Msg::Local(l);
        type Case = (Target, Option<ModelRef>, Vec<Message>, Option<usize>, Option<Event>);
        let cases: Vec<Case> = vec![
            (Target::Session, None, vec![local(Local::Up)], Some(0), None),
            (Target::Session, None, std::iter::repeat_n(local(Local::Down), 10).chain([local(Local::Submit)]).collect(), Some(5), session("openai", "gpt-5-mini", None)),
            (Target::Session, None, vec![local(Local::Pick(1))], Some(0), session("anthropic", "claude-haiku", Some("smol"))),
            (Target::Role("plan".into()), Some(ModelRef::new("openai", "gpt-5")), vec![local(Local::Submit)], Some(2), Some(Event::SetRoleModel { role: "plan".into(), model: ModelRef::new("openai", "gpt-5") })),
            (Target::Session, Some(ModelRef::new("openai", "gpt-5-mini")), vec![local(Local::Query("gpt".into())), local(Local::Submit)], Some(0), session("openai", "gpt-5", None)),
            (Target::Session, None, vec![local(Local::Query("nothing like this".into())), local(Local::Submit)], None, None),
            (Target::Session, None, vec![Msg::Emit(Event::Close)], Some(0), Some(Event::Close)),
        ];
        for (i, (target, current, messages, selected, event)) in cases.into_iter().enumerate() {
            let (mut s, _) = State::open(catalog(), target, current);
            let last = messages.into_iter().map(|m| s.update(m)).last().flatten();
            assert_eq!((s.selected(), last), (selected, event), "case {i}");
        }
    }

    #[test]
    fn clicking_a_model_switches_the_session() {
        let (mut s, _) = State::open(catalog(), Target::Session, None);
        let events: Vec<Event> = click_label(s.view(), "GPT-5 mini").into_iter().filter_map(|m| s.update(m)).collect();
        assert_eq!(events, [session("openai", "gpt-5-mini", None).unwrap()]);
    }
}
