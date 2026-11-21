//! The model picker: a searchable popup of the available models grouped by
//! provider, showing which roles each model serves. It picks a model for the
//! session (directly or through a role) or assigns a model to a role.

use crate::ModelRef;
pub use loa_models::selector::Catalog;
#[cfg(test)]
use loa_models::selector::{ModelInfo, RoleAssignment};
use crate::session::SessionModelChange;
use crate::style;
use iced::event::{self, Event as RuntimeEvent};
use iced::keyboard::{self, key::Named};
use iced::widget::operation::{self, RelativeOffset};
use iced::widget::{button, column, container, row, scrollable, space, text, text_input};
use iced::{Element, Fill, Subscription, Task};
use loa_ui_kit::icons::Icon;

const SEARCH_ID: &str = "loa-model-picker-search";
const LIST_ID: &str = "loa-model-picker-list";

/// What a pick changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// The current session's model. Roles are offered as rows too: picking
    /// one makes the session follow that role.
    Session,
    /// The model a role resolves to.
    Role(String),
}


/// One selectable line of the filtered list.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// Index into [`Catalog::roles`].
    Role(usize),
    /// Index into [`Catalog::models`].
    Model(usize),
}

/// A run of consecutive rows under one heading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group<'a> {
    pub title: &'a str,
    /// Index of the group's first row in [`State::rows`].
    pub start: usize,
    pub rows: &'a [Row],
}

#[derive(Clone, Debug)]
pub enum Message {
    Query(String),
    Up,
    Down,
    /// Picks the selected row.
    Submit,
    /// Picks the row at this index of [`State::rows`].
    Pick(usize),
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Record the change on the session (see [`crate::model_change_drafts`])
    /// and tell the harness (omp `set_model`).
    SetSessionModel(SessionModelChange),
    /// Assign `model` to `role` in the harness config.
    SetRoleModel { role: String, model: ModelRef },
    /// The picker was dismissed without a pick.
    Close,
}

#[derive(Clone, Debug)]
pub struct State {
    catalog: Catalog,
    target: Target,
    /// The model the target currently uses, highlighted and selected first.
    current: Option<ModelRef>,
    /// Lowercase search text per model: ids, names and the roles it serves.
    model_haystacks: Vec<String>,
    /// Display order of each model's provider: first appearance in the catalog.
    provider_rank: Vec<usize>,
    query: String,
    rows: Vec<Row>,
    selected: usize,
}

impl State {
    /// Opens the picker. The returned task focuses the search field.
    pub fn open(catalog: Catalog, target: Target, current: Option<ModelRef>) -> (Self, Task<Message>) {
        let mut providers: Vec<&str> = Vec::new();
        let provider_rank = catalog
            .models
            .iter()
            .map(|m| {
                let provider = m.model.provider.as_str();
                providers.iter().position(|p| *p == provider).unwrap_or_else(|| {
                    providers.push(provider);
                    providers.len() - 1
                })
            })
            .collect();
        let model_haystacks = catalog
            .models
            .iter()
            .map(|m| {
                let mut hay = format!("{} {} {} {}", m.model.provider, m.provider_name, m.model.id, m.name);
                for role in catalog.roles.iter().filter(|r| r.model == m.model) {
                    hay.push(' ');
                    hay.push_str(&role.role);
                }
                hay.to_lowercase()
            })
            .collect();
        let mut state = Self {
            catalog,
            target,
            current,
            model_haystacks,
            provider_rank,
            query: String::new(),
            rows: Vec::new(),
            selected: 0,
        };
        state.refilter();
        state.selected = state.current_row().unwrap_or(0);
        (state, operation::focus(SEARCH_ID))
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    /// The rows matching the query, in display order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Index into [`State::rows`] of the highlighted row.
    pub fn selected(&self) -> Option<usize> {
        (!self.rows.is_empty()).then_some(self.selected)
    }

    /// The rows split under their headings: "Roles" first when picking for
    /// the session, then one group per provider.
    pub fn groups(&self) -> Vec<Group<'_>> {
        let mut groups: Vec<Group<'_>> = Vec::new();
        for (index, row) in self.rows.iter().enumerate() {
            let title = match *row {
                Row::Role(_) => "Roles",
                Row::Model(m) => self.catalog.models[m].provider_display_name(),
            };
            match groups.last_mut() {
                Some(group) if group.title == title => group.rows = &self.rows[group.start..=index],
                _ => groups.push(Group { title, start: index, rows: &self.rows[index..=index] }),
            }
        }
        groups
    }

    /// Roles currently resolving to `model`.
    pub fn roles_of<'a>(&'a self, model: &'a ModelRef) -> impl Iterator<Item = &'a str> + 'a {
        self.catalog.roles.iter().filter(move |r| r.model == *model).map(|r| r.role.as_str())
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Query(query) => {
                self.query = query;
                self.refilter();
                self.selected = 0;
                (self.scroll_to_selected(), None)
            }
            Message::Up => {
                self.selected = self.selected.saturating_sub(1);
                (self.scroll_to_selected(), None)
            }
            Message::Down => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                (self.scroll_to_selected(), None)
            }
            Message::Submit => (Task::none(), self.pick(self.selected)),
            Message::Pick(index) => (Task::none(), self.pick(index)),
            Message::Close => (Task::none(), Some(Event::Close)),
        }
    }

    /// Arrow keys move the selection and Escape closes, also while the search
    /// field has focus. Enter is the search field's submit.
    pub fn subscription(&self) -> Subscription<Message> {
        event::listen_with(|event, _status, _window| match event {
            RuntimeEvent::Keyboard(keyboard::Event::KeyPressed { key: keyboard::Key::Named(named), .. }) => {
                match named {
                    Named::ArrowUp => Some(Message::Up),
                    Named::ArrowDown => Some(Message::Down),
                    Named::Escape => Some(Message::Close),
                    _ => None,
                }
            }
            _ => None,
        })
    }

    pub fn view(&self) -> Element<'_, Message> {
        let heading = match &self.target {
            Target::Session => "Model for this session".to_owned(),
            Target::Role(role) => format!("Model for role {role}"),
        };
        let search = style::search_box(
            text_input("Search models", &self.query).id(SEARCH_ID).on_input(Message::Query).on_submit(Message::Submit),
        );

        let mut list = column![].spacing(2);
        if self.rows.is_empty() {
            list = list.push(container(text("No models match").size(style::NOTE).style(style::muted)).padding([4.0, 8.0]));
        }
        for group in self.groups() {
            list = list.push(container(text(group.title).size(style::SMALL).style(style::muted)).padding([6.0, 8.0]));
            for (offset, row) in group.rows.iter().enumerate() {
                list = list.push(self.row_view(group.start + offset, *row));
            }
        }

        let content = column![
            row![text(heading).size(style::BODY), space::horizontal(), style::icon_button(Icon::X).on_press(Message::Close)]
                .align_y(iced::Center),
            search,
            scrollable(list).id(LIST_ID).height(360).direction(style::thin_scrollbar()).style(style::scrollbar),
        ]
        .spacing(8);
        container(content).padding(8).width(420).style(style::popup).into()
    }

    fn row_view(&self, index: usize, row: Row) -> Element<'_, Message> {
        let (title, detail, is_current): (&str, String, bool) = match row {
            Row::Role(r) => {
                let role = &self.catalog.roles[r];
                (&role.role, role.model.selector(), false)
            }
            Row::Model(m) => {
                let info = &self.catalog.models[m];
                let roles: Vec<&str> = self.roles_of(&info.model).collect();
                let detail = if roles.is_empty() {
                    info.model.id.clone()
                } else {
                    format!("{} · {}", info.model.id, roles.join(", "))
                };
                (info.display_name(), detail, self.current.as_ref() == Some(&info.model))
            }
        };
        let mut line = row![
            text(title).size(style::BODY),
            space::horizontal(),
            text(detail).size(style::SMALL).style(style::muted)
        ]
        .spacing(8)
        .align_y(iced::Center);
        if is_current {
            line = line.push(text("current").size(style::SMALL).style(text::success));
        }
        let look = if index == self.selected { style::selected } else { style::flat };
        button(line).width(Fill).padding([6.0, 8.0]).style(look).on_press(Message::Pick(index)).into()
    }

    fn pick(&self, index: usize) -> Option<Event> {
        let event = match (self.rows.get(index)?, &self.target) {
            (Row::Role(r), Target::Session) => {
                let role = &self.catalog.roles[*r];
                Event::SetSessionModel(SessionModelChange { model: role.model.clone(), role: Some(role.role.clone()) })
            }
            (Row::Model(m), Target::Session) => {
                let model = self.catalog.models[*m].model.clone();
                Event::SetSessionModel(SessionModelChange { model, role: None })
            }
            (Row::Model(m), Target::Role(role)) => {
                Event::SetRoleModel { role: role.clone(), model: self.catalog.models[*m].model.clone() }
            }
            // Role rows exist only for the session target (see `refilter`).
            (Row::Role(_), Target::Role(_)) => return None,
        };
        Some(event)
    }

    fn current_row(&self) -> Option<usize> {
        let current = self.current.as_ref()?;
        self.rows.iter().position(|row| matches!(row, Row::Model(m) if self.catalog.models[*m].model == *current))
    }

    /// Recomputes the visible rows: every whitespace separated term of the
    /// query must appear (case-insensitively) in the row's search text.
    fn refilter(&mut self) {
        let query = self.query.to_lowercase();
        let terms: Vec<&str> = query.split_whitespace().collect();
        let matches = |hay: &str| terms.iter().all(|t| hay.contains(t));
        self.rows.clear();
        if self.target == Target::Session {
            for (index, role) in self.catalog.roles.iter().enumerate() {
                let hay = format!("{} {}", role.role, role.model).to_lowercase();
                if matches(&hay) {
                    self.rows.push(Row::Role(index));
                }
            }
        }
        let first_model = self.rows.len();
        self.rows.extend(
            self.model_haystacks.iter().enumerate().filter(|(_, hay)| matches(hay)).map(|(index, _)| Row::Model(index)),
        );
        let rank = &self.provider_rank;
        self.rows[first_model..].sort_by_key(|row| match row {
            Row::Model(m) => rank[*m],
            Row::Role(_) => 0,
        });
    }

    fn scroll_to_selected(&self) -> Task<Message> {
        let y = match self.rows.len() {
            0 | 1 => 0.0,
            len => self.selected as f32 / (len - 1) as f32,
        };
        operation::snap_to(LIST_ID, RelativeOffset { x: None, y: Some(y) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(provider: &str, provider_name: &str, id: &str, name: &str) -> ModelInfo {
        ModelInfo { model: ModelRef::new(provider, id), name: name.into(), provider_name: provider_name.into() }
    }

    fn catalog() -> Catalog {
        Catalog {
            models: vec![
                info("anthropic", "Anthropic", "claude-sonnet", "Claude Sonnet"),
                info("openai", "OpenAI", "gpt-5", "GPT-5"),
                info("anthropic", "Anthropic", "claude-haiku", "Claude Haiku"),
                info("openai", "OpenAI", "gpt-5-mini", "GPT-5 mini"),
            ],
            roles: vec![
                RoleAssignment { role: "default".into(), model: ModelRef::new("anthropic", "claude-sonnet") },
                RoleAssignment { role: "smol".into(), model: ModelRef::new("anthropic", "claude-haiku") },
            ],
        }
    }

    fn titles(state: &State) -> Vec<(String, Vec<String>)> {
        state
            .groups()
            .iter()
            .map(|g| {
                let rows = g
                    .rows
                    .iter()
                    .map(|row| match *row {
                        Row::Role(r) => state.catalog.roles[r].role.clone(),
                        Row::Model(m) => state.catalog.models[m].model.id.clone(),
                    })
                    .collect();
                (g.title.to_owned(), rows)
            })
            .collect()
    }

    fn strs(group: &str, rows: &[&str]) -> (String, Vec<String>) {
        (group.to_owned(), rows.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn models_are_grouped_by_provider_in_catalog_order() {
        let (state, _) = State::open(catalog(), Target::Session, None);
        assert_eq!(
            titles(&state),
            vec![
                strs("Roles", &["default", "smol"]),
                strs("Anthropic", &["claude-sonnet", "claude-haiku"]),
                strs("OpenAI", &["gpt-5", "gpt-5-mini"]),
            ]
        );
    }

    #[test]
    fn search_matches_every_term_case_insensitively() {
        let (mut state, _) = State::open(catalog(), Target::Session, None);
        let _ = state.update(Message::Query("OPENAI mini".into()));
        assert_eq!(titles(&state), vec![strs("OpenAI", &["gpt-5-mini"])]);
    }

    #[test]
    fn search_finds_models_by_the_role_they_serve() {
        let (mut state, _) = State::open(catalog(), Target::Role("plan".into()), None);
        let _ = state.update(Message::Query("smol".into()));
        assert_eq!(titles(&state), vec![strs("Anthropic", &["claude-haiku"])]);
    }

    #[test]
    fn roles_served_are_listed_per_model() {
        let (state, _) = State::open(catalog(), Target::Session, None);
        let sonnet = ModelRef::new("anthropic", "claude-sonnet");
        assert_eq!(state.roles_of(&sonnet).collect::<Vec<_>>(), ["default"]);
        assert_eq!(state.roles_of(&ModelRef::new("openai", "gpt-5")).count(), 0);
    }

    #[test]
    fn keyboard_navigation_clamps_and_enter_picks() {
        let (mut state, _) = State::open(catalog(), Target::Session, None);
        assert_eq!(state.selected(), Some(0));
        let _ = state.update(Message::Up);
        assert_eq!(state.selected(), Some(0));
        for _ in 0..10 {
            let _ = state.update(Message::Down);
        }
        assert_eq!(state.selected(), Some(5));
        let (_, event) = state.update(Message::Submit);
        assert_eq!(
            event,
            Some(Event::SetSessionModel(SessionModelChange { model: ModelRef::new("openai", "gpt-5-mini"), role: None }))
        );
    }

    #[test]
    fn picking_a_role_row_makes_the_session_follow_the_role() {
        let (mut state, _) = State::open(catalog(), Target::Session, None);
        let (_, event) = state.update(Message::Pick(1));
        assert_eq!(
            event,
            Some(Event::SetSessionModel(SessionModelChange {
                model: ModelRef::new("anthropic", "claude-haiku"),
                role: Some("smol".into()),
            }))
        );
    }

    #[test]
    fn role_target_offers_only_models_and_assigns_the_role() {
        let (mut state, _) =
            State::open(catalog(), Target::Role("plan".into()), Some(ModelRef::new("openai", "gpt-5")));
        assert!(state.rows().iter().all(|r| matches!(r, Row::Model(_))));
        assert_eq!(state.selected(), Some(2), "opens on the role's current model");
        let (_, event) = state.update(Message::Submit);
        assert_eq!(event, Some(Event::SetRoleModel { role: "plan".into(), model: ModelRef::new("openai", "gpt-5") }));
    }

    #[test]
    fn filtering_resets_selection_and_empty_result_picks_nothing() {
        let (mut state, _) = State::open(catalog(), Target::Session, Some(ModelRef::new("openai", "gpt-5-mini")));
        assert_eq!(state.selected(), Some(5));
        let _ = state.update(Message::Query("gpt".into()));
        assert_eq!(state.selected(), Some(0));
        let (_, event) = state.update(Message::Submit);
        assert_eq!(
            event,
            Some(Event::SetSessionModel(SessionModelChange { model: ModelRef::new("openai", "gpt-5"), role: None }))
        );
        let _ = state.update(Message::Query("nothing like this".into()));
        assert_eq!(state.selected(), None);
        let (_, event) = state.update(Message::Submit);
        assert_eq!(event, None);
    }
}
