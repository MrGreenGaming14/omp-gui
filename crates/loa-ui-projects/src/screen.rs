//! The project screen: shared instructions, pinned files, the digest model
//! and every session of the project with its digest and exclude toggle.
//!
//! The screen holds a draft of the project settings. Edits change the draft;
//! Save hands it to the app as [`Event::SaveSettings`]. Toggling a session
//! updates the row at once and asks the app to record it with
//! [`Event::SetExcluded`]; the app answers with a fresh overview.

use crate::style;
use iced::widget::{
    Column, button, column, container, row, rule, scrollable, space, text, text_editor, text_input, toggler,
};
use iced::{Alignment, Element, Fill, Length};
use loa_projects::{DigestState, PinnedFile, ProjectOverview, ProjectSettings, SessionRow};
use loa_ui_icons::Icon;

/// Width of the screen's content column.
const CONTENT_WIDTH: f32 = 760.0;

/// Decisions shown under a digest summary.
const SHOWN_DECISIONS: usize = 3;

#[derive(Debug)]
pub struct State {
    project: String,
    overview: Option<ProjectOverview>,
    /// Unix milliseconds the overview was loaded at, for "3h ago".
    now: i64,
    instructions: text_editor::Content,
    pinned: Vec<PinnedFile>,
    digest_model: String,
    new_pin_path: String,
    new_pin_note: String,
    saving: bool,
    error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Message {
    InstructionsEdited(text_editor::Action),
    DigestModelChanged(String),
    PinPathChanged(String),
    PinNoteChanged(String),
    AddPin,
    RemovePin(usize),
    Save,
    Revert,
    /// The "In context" toggle of a session row.
    SetIncluded { session: String, included: bool },
    OpenSession(String),
    NewChat,
}

/// What the app does for the screen.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Store these settings; answer with [`State::saved`].
    SaveSettings { project: String, settings: ProjectSettings },
    /// Record the session's exclude toggle; answer with a fresh overview.
    SetExcluded { session: String, excluded: bool },
    OpenSession(String),
    /// Start a chat in this project, in its working directory when known.
    NewChat { project: String, cwd: Option<String> },
}

impl State {
    /// A screen for `project` that shows a loading line until
    /// [`State::set_overview`].
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            overview: None,
            now: 0,
            instructions: text_editor::Content::new(),
            pinned: Vec::new(),
            digest_model: String::new(),
            new_pin_path: String::new(),
            new_pin_note: String::new(),
            saving: false,
            error: None,
        }
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn overview(&self) -> Option<&ProjectOverview> {
        self.overview.as_ref()
    }

    /// Shows a freshly loaded overview, `now` in Unix milliseconds. Unsaved
    /// edits to the settings are kept; otherwise the draft follows the
    /// stored settings.
    pub fn set_overview(&mut self, overview: ProjectOverview, now: i64) {
        let keep_draft = self.is_dirty();
        self.now = now;
        self.overview = Some(overview);
        if !keep_draft {
            self.reset_draft();
        }
    }

    /// Shows an error from loading or saving.
    pub fn set_error(&mut self, error: impl Into<String>) {
        self.saving = false;
        self.error = Some(error.into());
    }

    /// The answer to [`Event::SaveSettings`]: on success the saved settings
    /// become the stored ones.
    pub fn saved(&mut self, result: Result<ProjectSettings, String>) {
        self.saving = false;
        match result {
            Ok(settings) => {
                self.error = None;
                if let Some(overview) = &mut self.overview {
                    overview.settings = settings;
                }
            }
            Err(error) => self.error = Some(error),
        }
    }

    /// The settings as edited. The default folder isn't edited here and is
    /// kept as stored.
    pub fn draft(&self) -> ProjectSettings {
        let model = self.digest_model.trim();
        ProjectSettings {
            instructions: self.instructions.text().trim_end().to_owned(),
            pinned: self.pinned.clone(),
            digest_model: (!model.is_empty()).then(|| model.to_owned()),
            folder: self.overview.as_ref().and_then(|overview| overview.settings.folder.clone()),
        }
    }

    /// Whether the draft differs from the stored settings.
    pub fn is_dirty(&self) -> bool {
        self.overview.as_ref().is_some_and(|o| o.settings != self.draft())
    }

    fn reset_draft(&mut self) {
        let settings = self.overview.as_ref().map(|o| o.settings.clone()).unwrap_or_default();
        self.instructions = text_editor::Content::with_text(&settings.instructions);
        self.pinned = settings.pinned;
        self.digest_model = settings.digest_model.unwrap_or_default();
    }

    pub fn update(&mut self, message: Message) -> Option<Event> {
        match message {
            Message::InstructionsEdited(action) => self.instructions.perform(action),
            Message::DigestModelChanged(model) => self.digest_model = model,
            Message::PinPathChanged(path) => self.new_pin_path = path,
            Message::PinNoteChanged(note) => self.new_pin_note = note,
            Message::AddPin => {
                let path = self.new_pin_path.trim();
                if !path.is_empty() && !self.pinned.iter().any(|p| p.path == path) {
                    let note = self.new_pin_note.trim();
                    self.pinned.push(PinnedFile {
                        path: path.to_owned(),
                        note: (!note.is_empty()).then(|| note.to_owned()),
                    });
                    self.new_pin_path.clear();
                    self.new_pin_note.clear();
                }
            }
            Message::RemovePin(index) => {
                if index < self.pinned.len() {
                    self.pinned.remove(index);
                }
            }
            Message::Save => {
                if self.is_dirty() && !self.saving {
                    self.saving = true;
                    self.error = None;
                    return Some(Event::SaveSettings { project: self.project.clone(), settings: self.draft() });
                }
            }
            Message::Revert => {
                self.reset_draft();
                self.error = None;
            }
            Message::SetIncluded { session, included } => {
                let row = self.overview.as_mut()?.sessions.iter_mut().find(|s| s.id == session)?;
                if row.excluded == included {
                    row.excluded = !included;
                    return Some(Event::SetExcluded { session, excluded: !included });
                }
            }
            Message::OpenSession(session) => return Some(Event::OpenSession(session)),
            Message::NewChat => {
                let cwd = self.overview.as_ref().and_then(|o| o.cwd.clone());
                return Some(Event::NewChat { project: self.project.clone(), cwd });
            }
        }
        None
    }

    pub fn view(&self) -> Element<'_, Message> {
        let mut content = column![self.header()].spacing(28).max_width(CONTENT_WIDTH);
        match &self.overview {
            None => content = content.push(text("Loading project…").size(style::NOTE).style(style::muted)),
            Some(overview) => {
                content = content.push(self.settings_view()).push(sessions_view(&overview.sessions, self.now));
            }
        }
        scrollable(container(content).padding([28.0, 32.0]).center_x(Fill))
            .direction(style::thin_scrollbar())
            .style(style::scrollbar)
            .height(Fill)
            .into()
    }

    fn header(&self) -> Element<'_, Message> {
        let cwd = self.overview.as_ref().and_then(|o| o.cwd.as_deref());
        let count = self.overview.as_ref().map(|o| o.sessions.len());
        let mut details = Vec::new();
        if let Some(cwd) = cwd {
            details.push(cwd.to_owned());
        }
        if let Some(count) = count {
            details.push(if count == 1 { "1 session".to_owned() } else { format!("{count} sessions") });
        }
        let title = column![
            row![loa_ui_icons::icon(Icon::Folder, 20.0), text(&self.project).size(style::TITLE)]
                .spacing(10)
                .align_y(Alignment::Center),
            text(details.join("  ·  ")).size(style::SMALL).style(style::muted),
        ]
        .spacing(4);
        let new_chat = button(
            row![loa_ui_icons::icon(Icon::Plus, 14.0), text("New chat").size(style::BODY)]
                .spacing(6)
                .align_y(Alignment::Center),
        )
        .padding([6.0, 12.0])
        .style(style::flat)
        .on_press(Message::NewChat);
        row![title, space::horizontal(), new_chat].align_y(Alignment::Center).into()
    }

    fn settings_view(&self) -> Element<'_, Message> {
        let instructions = column![
            section("Shared instructions"),
            hint("Given to every chat in this project, like a CLAUDE.md for the whole project."),
            text_editor(&self.instructions)
                .placeholder("Conventions, constraints, where things live…")
                .on_action(Message::InstructionsEdited)
                .size(style::BODY)
                .padding(10)
                .height(Length::Fixed(140.0))
                .style(style::editor),
        ]
        .spacing(8);

        let mut pins = Column::new().spacing(0);
        for (index, pin) in self.pinned.iter().enumerate() {
            if index > 0 {
                pins = pins.push(rule::horizontal(1).style(style::divider));
            }
            let mut label = row![loa_ui_icons::muted(Icon::FileText, 14.0), text(&pin.path).size(style::BODY)]
                .spacing(8)
                .align_y(Alignment::Center);
            if let Some(note) = &pin.note {
                label = label.push(text(note).size(style::SMALL).style(style::muted));
            }
            pins = pins.push(
                row![
                    label,
                    space::horizontal(),
                    button(loa_ui_icons::muted(Icon::X, 12.0)).padding(4).style(style::flat).on_press(Message::RemovePin(index)),
                ]
                .align_y(Alignment::Center)
                .padding([6.0, 10.0]),
            );
        }
        let mut pinned = column![section("Pinned files")].spacing(8);
        if self.pinned.is_empty() {
            pinned = pinned.push(hint("No pinned files. Pin the files every chat should know about."));
        } else {
            pinned = pinned.push(container(pins).style(style::card));
        }
        let can_pin = !self.new_pin_path.trim().is_empty();
        pinned = pinned.push(
            row![
                text_input("path/to/file", &self.new_pin_path)
                    .on_input(Message::PinPathChanged)
                    .on_submit_maybe(can_pin.then_some(Message::AddPin))
                    .size(style::BODY)
                    .padding([6.0, 8.0])
                    .style(style::input)
                    .width(Length::FillPortion(3)),
                text_input("Why it matters (optional)", &self.new_pin_note)
                    .on_input(Message::PinNoteChanged)
                    .on_submit_maybe(can_pin.then_some(Message::AddPin))
                    .size(style::BODY)
                    .padding([6.0, 8.0])
                    .style(style::input)
                    .width(Length::FillPortion(4)),
                button(row![loa_ui_icons::icon(Icon::Plus, 12.0), text("Pin").size(style::SMALL)].spacing(6).align_y(Alignment::Center))
                    .padding([6.0, 10.0])
                    .style(style::flat)
                    .on_press_maybe(can_pin.then_some(Message::AddPin)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );

        let digest_model = column![
            section("Digest model"),
            hint("Model or role that writes session digests. Empty uses omp's smol role."),
            text_input("@smol", &self.digest_model)
                .on_input(Message::DigestModelChanged)
                .size(style::BODY)
                .padding([6.0, 8.0])
                .style(style::input)
                .width(Length::Fixed(320.0)),
        ]
        .spacing(8);

        let dirty = self.is_dirty();
        let mut actions = row![].spacing(8).align_y(Alignment::Center);
        if let Some(error) = &self.error {
            actions = actions.push(text(error).size(style::SMALL).style(style::danger));
        } else if !dirty && !self.saving {
            actions = actions.push(text("All changes saved").size(style::SMALL).style(style::muted));
        }
        actions = actions.push(space::horizontal());
        if dirty {
            actions = actions.push(
                button(text("Revert").size(style::SMALL))
                    .padding([6.0, 10.0])
                    .style(style::flat)
                    .on_press_maybe((!self.saving).then_some(Message::Revert)),
            );
        }
        actions = actions.push(
            button(text(if self.saving { "Saving…" } else { "Save" }).size(style::SMALL))
                .padding([6.0, 14.0])
                .style(button::primary)
                .on_press_maybe((dirty && !self.saving).then_some(Message::Save)),
        );

        column![instructions, pinned, digest_model, actions].spacing(24).into()
    }
}

fn section<'a>(title: &str) -> text::Text<'a> {
    text(title.to_uppercase()).size(style::SMALL).style(style::muted)
}

fn hint(line: &str) -> text::Text<'_> {
    text(line).size(style::NOTE).style(style::muted)
}

fn sessions_view(sessions: &[SessionRow], now: i64) -> Element<'_, Message> {
    let mut list = column![
        section("Sessions"),
        hint("Each session's digest is shared with new chats in this project. Turn a session off to keep it out."),
    ]
    .spacing(8);
    if sessions.is_empty() {
        return list.push(hint("No sessions in this project yet.")).into();
    }
    let mut rows = Column::new();
    for (index, session) in sessions.iter().enumerate() {
        if index > 0 {
            rows = rows.push(rule::horizontal(1).style(style::divider));
        }
        rows = rows.push(session_row(session, now));
    }
    list = list.push(container(rows).padding(1).style(style::card));
    list.into()
}

fn session_row(session: &SessionRow, now: i64) -> Element<'_, Message> {
    let title = session.title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or("Untitled session");
    let mut meta = Vec::new();
    if let Some(harness) = &session.harness {
        meta.push(harness.clone());
    }
    meta.push(if session.turns == 1 { "1 turn".to_owned() } else { format!("{} turns", session.turns) });
    meta.push(ago(now, session.last_active));
    // The digest state is plain text at the end of the meta line, only
    // tinted when it needs attention.
    let (state, state_style): (String, fn(&iced::Theme) -> text::Style) = if session.excluded {
        ("not in context".into(), style::muted)
    } else {
        match session.digest_state {
            DigestState::Fresh => match &session.digest_model {
                Some(model) => (format!("digest by {model}"), style::muted),
                None => ("digest up to date".into(), style::muted),
            },
            DigestState::Stale => ("digest out of date".into(), style::warning),
            DigestState::Missing => ("no digest yet".into(), style::muted),
        }
    };
    let meta = row![
        text(meta.join("  ·  ")).size(style::SMALL).style(style::muted),
        text("  ·  ").size(style::SMALL).style(style::muted),
        text(state).size(style::SMALL).style(state_style),
    ];
    let mut body = column![text(title).size(style::BODY).wrapping(text::Wrapping::None), meta].spacing(3).width(Fill);
    if let Some(digest) = &session.digest {
        body = body.push(text(&digest.summary).size(style::NOTE).style(style::muted));
        for decision in digest.decisions.iter().take(SHOWN_DECISIONS) {
            body = body.push(text(format!("• {decision}")).size(style::SMALL).style(style::muted));
        }
    }
    let include = toggler(!session.excluded)
        .label("In context")
        .text_size(style::SMALL)
        .size(14)
        .on_toggle(|included| Message::SetIncluded { session: session.id.clone(), included });
    // The whole row opens the session; the toggle takes its own clicks.
    button(row![body, include].spacing(16).align_y(Alignment::Center))
        .padding([10.0, 12.0])
        .width(Fill)
        .style(style::list_row)
        .on_press(Message::OpenSession(session.id.clone()))
        .into()
}

/// "just now", "5m ago", "3h ago", "2d ago".
fn ago(now: i64, then: i64) -> String {
    let minutes = (now - then).max(0) / 60_000;
    match minutes {
        0 => "just now".to_owned(),
        m if m < 60 => format!("{m}m ago"),
        m if m < 60 * 24 => format!("{}h ago", m / 60),
        m => format!("{}d ago", m / (60 * 24)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_projects::DigestContent;

    fn overview() -> ProjectOverview {
        ProjectOverview {
            name: "loa".into(),
            cwd: Some("/home/u/loa".into()),
            settings: ProjectSettings { instructions: "Use rustls.".into(), ..Default::default() },
            sessions: vec![
                SessionRow {
                    id: "a".into(),
                    title: Some("Set up iroh".into()),
                    harness: Some("omp".into()),
                    created: 0,
                    last_active: 0,
                    turns: 3,
                    excluded: false,
                    digest_state: DigestState::Fresh,
                    digest: Some(DigestContent { summary: "Picked port 31415.".into(), ..Default::default() }),
                    digest_model: Some("smol".into()),
                },
                SessionRow {
                    id: "b".into(),
                    title: None,
                    harness: Some("omp".into()),
                    created: 0,
                    last_active: 0,
                    turns: 1,
                    excluded: true,
                    digest_state: DigestState::Missing,
                    digest: None,
                    digest_model: None,
                },
            ],
        }
    }

    fn loaded() -> State {
        let mut state = State::new("loa");
        state.set_overview(overview(), 3 * 3_600_000);
        state
    }

    #[test]
    fn editing_and_saving_settings_emits_the_draft() {
        let mut state = loaded();
        assert!(!state.is_dirty());
        assert_eq!(state.update(Message::Save), None);
        state.update(Message::PinPathChanged(" docs/ARCH.md ".into()));
        state.update(Message::PinNoteChanged("the contract".into()));
        state.update(Message::AddPin);
        state.update(Message::DigestModelChanged("anthropic/claude-haiku-5-5".into()));
        assert!(state.is_dirty());
        let Some(Event::SaveSettings { project, settings }) = state.update(Message::Save) else {
            panic!("expected a save");
        };
        assert_eq!(project, "loa");
        assert_eq!(settings.instructions, "Use rustls.");
        assert_eq!(settings.pinned, vec![PinnedFile { path: "docs/ARCH.md".into(), note: Some("the contract".into()) }]);
        assert_eq!(settings.digest_model.as_deref(), Some("anthropic/claude-haiku-5-5"));
        // A second press while saving does nothing.
        assert_eq!(state.update(Message::Save), None);
        state.saved(Ok(settings));
        assert!(!state.is_dirty());
    }

    #[test]
    fn unsaved_edits_survive_a_refresh_and_revert_restores() {
        let mut state = loaded();
        state.update(Message::PinPathChanged("src/main.rs".into()));
        state.update(Message::AddPin);
        state.set_overview(overview(), 0);
        assert_eq!(state.draft().pinned.len(), 1);
        state.update(Message::Revert);
        assert!(state.draft().pinned.is_empty());
        assert!(!state.is_dirty());
    }

    #[test]
    fn toggling_a_session_updates_the_row_and_asks_the_app() {
        let mut state = loaded();
        let event = state.update(Message::SetIncluded { session: "a".into(), included: false });
        assert_eq!(event, Some(Event::SetExcluded { session: "a".into(), excluded: true }));
        assert!(state.overview().unwrap().sessions[0].excluded);
        let event = state.update(Message::SetIncluded { session: "b".into(), included: true });
        assert_eq!(event, Some(Event::SetExcluded { session: "b".into(), excluded: false }));
        assert_eq!(state.update(Message::SetIncluded { session: "zz".into(), included: true }), None);
    }

    #[test]
    fn new_chat_and_open_session_go_to_the_app() {
        let mut state = loaded();
        assert_eq!(
            state.update(Message::NewChat),
            Some(Event::NewChat { project: "loa".into(), cwd: Some("/home/u/loa".into()) })
        );
        assert_eq!(state.update(Message::OpenSession("a".into())), Some(Event::OpenSession("a".into())));
    }

    #[test]
    fn view_shows_settings_sessions_and_digests() {
        let state = loaded();
        let mut ui = iced_test::simulator(state.view());
        let header = "Shared instructions".to_uppercase();
        for label in [header.as_str(), "Set up iroh", "Picked port 31415.", "digest by smol", "not in context", "Untitled session", "omp  ·  3 turns  ·  3h ago"] {
            assert!(ui.find(label).is_ok(), "missing {label}");
        }
        // The whole row opens its session, not only the title.
        let mut ui = iced_test::simulator(state.view());
        ui.click("omp  ·  3 turns  ·  3h ago").unwrap();
        assert!(ui.into_messages().any(|message| matches!(message, Message::OpenSession(_))));
        let mut ui = iced_test::simulator(state.view());
        ui.click("New chat").unwrap();
        assert!(matches!(ui.into_messages().next(), Some(Message::NewChat)));
    }
}
