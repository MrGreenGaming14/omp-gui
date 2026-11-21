//! The project screen: shared instructions, pinned files, the digest model and every session of
//! the project with its digest and context toggle, over a [`ProjectOverview`] the app loads.
//! Edits change a draft that Save hands to the app; toggling a session updates its row at once
//! and asks the app to record it.

use crate::{action, check, field, scroll, section};
use iced::widget::{Column, Space, button, column, container, row, text_editor};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::icons::tinted;
use loa_view::settings::projects::{DigestState, Intent, PinnedFile, ProjectOverview, ProjectSettings, SessionRow};
use loa_view::theme::Icon;
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, text_size, width};
use loa_ui_kit::widgets::{Msg, Screen, label, list_editor};
use loa_util::time::ago;

/// Decisions shown under a digest summary.
const SHOWN_DECISIONS: usize = 3;

#[derive(Clone, Debug)]
pub enum Local {
    Instructions(text_editor::Action),
    DigestModel(String),
    PinPath(String),
    PinNote(String),
    AddPin,
    RemovePin(usize),
    Save,
    Revert,
    /// A session's "In context" toggle.
    SetIncluded { session: String, included: bool },
}

pub type Message = Msg<Local, Intent>;

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

impl State {
    /// A screen for `project` showing a loading line until [`State::set_overview`].
    pub fn new(project: impl Into<String>) -> Self {
        let (instructions, pinned, digest_model, new_pin_path, new_pin_note) = Default::default();
        Self { project: project.into(), overview: None, now: 0, instructions, pinned, digest_model, new_pin_path, new_pin_note, saving: false, error: None }
    }

    pub fn project(&self) -> &str { &self.project }
    pub fn overview(&self) -> Option<&ProjectOverview> { self.overview.as_ref() }

    /// Shows a fresh overview loaded at `now` (Unix ms); unsaved edits stay, otherwise the draft follows the stored settings.
    pub fn set_overview(&mut self, overview: ProjectOverview, now: i64) {
        let keep = self.is_dirty();
        (self.now, self.overview) = (now, Some(overview));
        if !keep {
            self.reset();
        }
    }

    /// Shows an error from loading or saving.
    pub fn set_error(&mut self, error: impl Into<String>) { (self.saving, self.error) = (false, Some(error.into())) }

    /// The answer to [`Intent::SaveSettings`]: on success the settings become the stored ones.
    pub fn saved(&mut self, result: Result<ProjectSettings, String>) {
        self.saving = false;
        match result {
            Ok(settings) => {
                self.error = None;
                if let Some(o) = &mut self.overview {
                    o.settings = settings;
                }
            }
            Err(error) => self.error = Some(error),
        }
    }

    /// The settings as edited.
    pub fn draft(&self) -> ProjectSettings {
        let model = self.digest_model.trim();
        ProjectSettings { instructions: self.instructions.text().trim_end().to_owned(), pinned: self.pinned.clone(), digest_model: (!model.is_empty()).then(|| model.to_owned()) }
    }

    /// Whether the draft differs from the stored settings.
    pub fn is_dirty(&self) -> bool { self.overview.as_ref().is_some_and(|o| o.settings != self.draft()) }

    fn reset(&mut self) {
        let settings = self.overview.as_ref().map(|o| o.settings.clone()).unwrap_or_default();
        self.instructions = text_editor::Content::with_text(&settings.instructions);
        (self.pinned, self.digest_model) = (settings.pinned, settings.digest_model.unwrap_or_default());
    }

    pub fn view(&self) -> Element<'_, Message> {
        let body: Element<'_, Message> = match &self.overview {
            None => label("Loading project…", TextRole::Label, Tone::Muted).into(),
            Some(o) => column![self.settings_view(), sessions_view(&o.sessions, self.now)].spacing(sp(SpaceRole::Xl)).into(),
        };
        scroll(container(column![self.header(), body].spacing(sp(SpaceRole::Xl)).max_width(width(LayoutRole::ContentWidth))).center_x(Fill))
    }

    fn header(&self) -> Element<'_, Message> {
        let o = self.overview.as_ref();
        let count = o.map(|o| if o.sessions.len() == 1 { "1 session".to_owned() } else { format!("{} sessions", o.sessions.len()) });
        let details: Vec<String> = o.and_then(|o| o.cwd.clone()).into_iter().chain(count).collect();
        let title = row![tinted(Icon::Folder, TextRole::Title, Tone::Normal), label(&self.project, TextRole::Title, Tone::Normal)].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
        let open = Msg::Emit(Intent::NewChat { project: self.project.clone(), cwd: o.and_then(|o| o.cwd.clone()) });
        let new_chat = button(row![tinted(Icon::Plus, TextRole::Body, Tone::Normal), label("New chat", TextRole::Body, Tone::Normal)].spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center));
        let new_chat = new_chat.padding([sp(SpaceRole::Xs), sp(SpaceRole::M)]).style(style::button(ButtonKind::Secondary)).on_press(open);
        row![column![title, label(details.join("  ·  "), TextRole::Caption, Tone::Muted)].spacing(sp(SpaceRole::Xs)), Space::new().width(Fill), new_chat].align_y(Alignment::Center).into()
    }

    fn settings_view(&self) -> Element<'_, Message> {
        let hint = |s| label(s, TextRole::Label, Tone::Muted);
        let editor = text_editor(&self.instructions).placeholder("Conventions, constraints, where things live…").on_action(|a| Msg::Local(Local::Instructions(a)));
        let editor = editor.size(text_size(TextRole::Body)).padding(sp(SpaceRole::S)).height(sp(SpaceRole::Xl) * 6.0).style(style::editor);
        let instructions = column![hint("Given to every chat in this project, like a CLAUDE.md for the whole project."), editor];
        let pin_label = |p: &PinnedFile| p.note.as_ref().map_or_else(|| p.path.clone(), |n| format!("{}  ·  {n}", p.path));
        let pins = list_editor(&self.pinned, pin_label, |i| Msg::Local(Local::RemovePin(i)), None, &self.new_pin_path, |p| Msg::Local(Local::PinPath(p)), Msg::Local(Local::AddPin));
        let note = field("Why it matters (optional)", &self.new_pin_note, |n| Msg::Local(Local::PinNote(n))).on_submit(Msg::Local(Local::AddPin));
        let empty = self.pinned.is_empty().then(|| hint("No pinned files. Type the path of a file every chat should know about."));
        let digest = column![hint("Model or role that writes session digests. Empty uses omp's smol role."), field("@smol", &self.digest_model, |m| Msg::Local(Local::DigestModel(m))).width(width(LayoutRole::LabelColumn) * 2.0)];
        let dirty = self.is_dirty();
        let status = match &self.error {
            Some(error) => Some(label(error.as_str(), TextRole::Caption, Tone::Danger)),
            None => (!dirty && !self.saving).then(|| label("All changes saved", TextRole::Caption, Tone::Muted)),
        };
        let revert = dirty.then(|| action("Revert", ButtonKind::Ghost, (!self.saving).then_some(Msg::Local(Local::Revert))));
        let save = action(if self.saving { "Saving…" } else { "Save" }, ButtonKind::Primary, (dirty && !self.saving).then_some(Msg::Local(Local::Save)));
        let actions = row![status, Space::new().width(Fill), revert, save].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
        let gap = sp(SpaceRole::S);
        column![section("Shared instructions", instructions.spacing(gap)), section("Pinned files", column![empty, pins, note].spacing(gap)), section("Digest model", digest.spacing(gap)), actions].spacing(sp(SpaceRole::L)).into()
    }
}

fn sessions_view(sessions: &[SessionRow], now: i64) -> Element<'_, Message> {
    let hint = label("Each session's digest is shared with new chats in this project. Turn a session off to keep it out.", TextRole::Label, Tone::Muted);
    let rows: Element<'_, Message> = if sessions.is_empty() {
        label("No sessions in this project yet.", TextRole::Label, Tone::Muted).into()
    } else {
        container(Column::with_children(sessions.iter().map(|s| session_row(s, now)))).style(style::container(Surface::Card)).into()
    };
    section("Sessions", column![hint, rows].spacing(sp(SpaceRole::S)))
}

/// A session: its title, a meta line ending in the digest state, the digest, and the context
/// toggle. The whole row opens the session; the toggle takes its own clicks.
fn session_row(s: &SessionRow, now: i64) -> Element<'_, Message> {
    let title = s.title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or("Untitled session");
    let turns = if s.turns == 1 { "1 turn".to_owned() } else { format!("{} turns", s.turns) };
    let meta: Vec<String> = s.harness.clone().into_iter().chain([turns, ago(now, s.last_active)]).collect();
    let (state, tone) = match (s.excluded, s.digest_state, &s.digest_model) {
        (true, _, _) => ("not in context".to_owned(), Tone::Muted),
        (false, DigestState::Fresh, Some(model)) => (format!("digest by {model}"), Tone::Muted),
        (false, DigestState::Fresh, None) => ("digest up to date".to_owned(), Tone::Muted),
        (false, DigestState::Stale, _) => ("digest out of date".to_owned(), Tone::Warn),
        (false, DigestState::Missing, _) => ("no digest yet".to_owned(), Tone::Muted),
    };
    let meta = row![label(meta.join("  ·  "), TextRole::Caption, Tone::Muted), label("  ·  ", TextRole::Caption, Tone::Muted), label(state, TextRole::Caption, tone)];
    let digest = s.digest.iter().flat_map(|d| std::iter::once(label(d.summary.as_str(), TextRole::Label, Tone::Muted)).chain(d.decisions.iter().take(SHOWN_DECISIONS).map(|x| label(format!("• {x}"), TextRole::Caption, Tone::Muted))));
    let body = column![label(title, TextRole::Body, Tone::Normal), meta].extend(digest.map(Into::into)).spacing(sp(SpaceRole::Xs)).width(Fill);
    let session = s.id.clone();
    let include = check(!s.excluded, "In context", move |included| Msg::Local(Local::SetIncluded { session: session.clone(), included }));
    button(row![body, include].spacing(sp(SpaceRole::M)).align_y(Alignment::Center)).padding([sp(SpaceRole::S), sp(SpaceRole::M)]).width(Fill).style(style::button(ButtonKind::Row { selected: false })).on_press(Msg::Emit(Intent::OpenSession(s.id.clone()))).into()
}

impl Screen for State {
    type Local = Local;
    type Event = Intent;

    fn reduce(&mut self, m: Local) -> Option<Intent> {
        match m {
            Local::Instructions(action) => self.instructions.perform(action),
            Local::DigestModel(model) => self.digest_model = model,
            Local::PinPath(path) => self.new_pin_path = path,
            Local::PinNote(note) => self.new_pin_note = note,
            Local::AddPin => {
                let (path, note) = (self.new_pin_path.trim(), self.new_pin_note.trim());
                if !path.is_empty() && !self.pinned.iter().any(|p| p.path == path) {
                    self.pinned.push(PinnedFile { path: path.to_owned(), note: (!note.is_empty()).then(|| note.to_owned()) });
                    (self.new_pin_path, self.new_pin_note) = Default::default();
                }
            }
            Local::RemovePin(i) => {
                if i < self.pinned.len() {
                    self.pinned.remove(i);
                }
            }
            Local::Save if self.is_dirty() && !self.saving => {
                (self.saving, self.error) = (true, None);
                return Some(Intent::SaveSettings { project: self.project.clone(), settings: self.draft() });
            }
            Local::Save => {}
            Local::Revert => {
                self.reset();
                self.error = None;
            }
            Local::SetIncluded { session, included } => {
                let row = self.overview.as_mut()?.sessions.iter_mut().find(|s| s.id == session && s.excluded == included)?;
                row.excluded = !included;
                return Some(Intent::SetExcluded { session, excluded: !included });
            }
        }
        None
    }

    /// Sessions open while listed; a new chat starts in this project and its folder.
    fn guard(&self, e: &Intent) -> bool {
        let o = self.overview.as_ref();
        match e {
            Intent::OpenSession(id) => o.is_some_and(|o| o.sessions.iter().any(|s| &s.id == id)),
            Intent::NewChat { project, cwd } => *project == self.project && *cwd == o.and_then(|o| o.cwd.clone()),
            Intent::SaveSettings { .. } | Intent::SetExcluded { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_view::settings::projects::Digest;
    use loa_ui_kit::testkit::click_label;

    fn overview() -> ProjectOverview {
        let row = |id: &str, title: Option<&str>, turns, excluded, digest_state, digest: Option<&str>, model: Option<&str>| SessionRow {
            id: id.into(), title: title.map(Into::into), harness: Some("omp".into()), last_active: 0, turns, excluded, digest_state,
            digest: digest.map(|d| Digest { summary: d.into(), ..Default::default() }), digest_model: model.map(Into::into),
        };
        let sessions = vec![row("a", Some("Set up iroh"), 3, false, DigestState::Fresh, Some("Picked port 31415."), Some("smol")), row("b", None, 1, true, DigestState::Missing, None, None)];
        ProjectOverview { cwd: Some("/home/u/loa".into()), settings: ProjectSettings { instructions: "Use rustls.".into(), ..Default::default() }, sessions }
    }

    fn loaded() -> State {
        let mut s = State::new("loa");
        s.set_overview(overview(), 3 * 3_600_000);
        s
    }

    #[test]
    fn edits_and_toggles_reduce_to_events() {
        use Local::*;
        let pin = PinnedFile { path: "docs/ARCH.md".into(), note: Some("the contract".into()) };
        let save = Intent::SaveSettings { project: "loa".into(), settings: ProjectSettings { instructions: "Use rustls.".into(), pinned: vec![pin], digest_model: Some("anthropic/claude-haiku".into()) } };
        let excluded = |session: &str, excluded| Intent::SetExcluded { session: session.into(), excluded };
        let toggle = |session: &str, included| Msg::Local(SetIncluded { session: session.into(), included });
        // (messages, events, state afterwards)
        type Case = (Vec<Message>, Vec<Intent>, fn(&State) -> bool);
        let cases: Vec<Case> = vec![
            (vec![Msg::Local(Save)], vec![], |s| !s.is_dirty()),
            ([PinPath(" docs/ARCH.md ".into()), PinNote("the contract".into()), AddPin, PinPath("docs/ARCH.md".into()), AddPin, DigestModel("anthropic/claude-haiku".into()), Save, Save].map(Msg::Local).into(), vec![save], |s| s.saving && s.is_dirty()),
            ([PinPath("src/main.rs".into()), AddPin, RemovePin(5), Revert].map(Msg::Local).into(), vec![], |s| s.draft().pinned.is_empty() && !s.is_dirty()),
            (vec![toggle("a", false), toggle("a", false), toggle("b", true), toggle("zz", true)], vec![excluded("a", true), excluded("b", false)], |s| s.overview.as_ref().unwrap().sessions.iter().all(|r| r.excluded == (r.id == "a"))),
            (vec![Msg::Emit(Intent::OpenSession("a".into())), Msg::Emit(Intent::OpenSession("zz".into())), Msg::Emit(Intent::NewChat { project: "other".into(), cwd: None })], vec![Intent::OpenSession("a".into())], |_| true),
        ];
        for (i, (messages, want, check)) in cases.into_iter().enumerate() {
            let mut s = loaded();
            let events: Vec<Intent> = messages.into_iter().filter_map(|m| s.update(m)).collect();
            assert_eq!(events, want, "case {i}");
            assert!(check(&s), "case {i}");
        }
    }

    #[test]
    fn refreshes_keep_unsaved_edits_and_saves_settle_them() {
        let mut s = loaded();
        s.update(Msg::Local(Local::PinPath("src/net.rs".into())));
        s.update(Msg::Local(Local::AddPin));
        s.set_overview(overview(), 0);
        assert_eq!(s.draft().pinned.len(), 1);
        let Some(Intent::SaveSettings { settings, .. }) = s.update(Msg::Local(Local::Save)) else { panic!("save") };
        s.saved(Ok(settings));
        assert!(!s.is_dirty() && !s.saving);
    }

    #[test]
    fn rows_open_sessions_and_new_chat_starts_in_the_project() {
        let mut s = loaded();
        let mut events: Vec<Intent> = click_label(s.view(), "omp  ·  3 turns  ·  3h ago").into_iter().filter_map(|m| s.update(m)).collect();
        events.extend(click_label(s.view(), "New chat").into_iter().filter_map(|m| s.update(m)));
        assert_eq!(events, [Intent::OpenSession("a".into()), Intent::NewChat { project: "loa".into(), cwd: Some("/home/u/loa".into()) }]);
    }
}
