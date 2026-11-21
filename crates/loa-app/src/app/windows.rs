//! Configs (a center screen), Settings (a window over the app) and detached OS windows: a panel
//! tab or a subagent's session in a window of its own.

use std::path::{Path, PathBuf};

use iced::widget::{column, container, row, rule, scrollable, text};
use iced::{Alignment, Element, Fill, Length, Size, Task, window};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, width};
use loa_ui_kit::widgets::{NavItem, card, icon_button, label, mono_block, section_header, side_nav, text_button, two_line_row};
use loa_ui_kit::icons::tinted;
use loa_ui_settings::models::Page;
use loa_view::theme::{Icon, ThemeRef};

use super::{App, Message};
use crate::area::Area;
use crate::areas;

/// A section of Configs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigsTab {
    Models,
    Tools,
    Agents,
    Skills,
    Rules,
}

impl ConfigsTab {
    const ALL: [ConfigsTab; 5] = [ConfigsTab::Models, ConfigsTab::Agents, ConfigsTab::Skills, ConfigsTab::Tools, ConfigsTab::Rules];

    fn blurb(self) -> &'static str {
        match self {
            ConfigsTab::Models => "Which model and effort each role runs on, and where a role falls back when its model fails.",
            ConfigsTab::Tools => "Built-in tools, MCP servers and remote hosts the harness may call.",
            ConfigsTab::Agents => "Agent definitions omp can delegate to. Each is a Markdown file with front matter.",
            ConfigsTab::Skills => "Skills load when a task matches their description.",
            ConfigsTab::Rules => "Instruction files loaded into every session, in this order.",
        }
    }

    /// Where omp reads this tab's files from.
    fn places(self) -> &'static str {
        match self {
            ConfigsTab::Agents => ".omp/agents/ or ~/.omp/agent/agents/",
            ConfigsTab::Skills => ".omp/skills/<name>/SKILL.md or ~/.omp/agent/skills/",
            _ => "RULES.md, AGENTS.md or ~/.omp/agent/RULES.md",
        }
    }
}

impl NavItem for ConfigsTab {
    fn label(self) -> &'static str {
        match self {
            ConfigsTab::Models => "Model roles",
            ConfigsTab::Agents => "Agents",
            ConfigsTab::Skills => "Skills",
            ConfigsTab::Tools => "Tools & MCP",
            ConfigsTab::Rules => "Rules",
        }
    }

    fn group(self) -> Option<&'static str> {
        Some(match self {
            ConfigsTab::Models => "Models",
            ConfigsTab::Agents | ConfigsTab::Skills => "Delegation",
            ConfigsTab::Tools | ConfigsTab::Rules => "Behavior",
        })
    }
}

/// A section of Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Peers,
    Harnesses,
    Updates,
}

impl SettingsTab {
    const ALL: [SettingsTab; 4] = [SettingsTab::General, SettingsTab::Peers, SettingsTab::Harnesses, SettingsTab::Updates];
}

impl NavItem for SettingsTab {
    fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "General",
            SettingsTab::Peers => "Peers & sharing",
            SettingsTab::Harnesses => "Harnesses",
            SettingsTab::Updates => "Updates",
        }
    }
}

/// A Markdown definition file omp reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocFile {
    pub name: String,
    pub path: PathBuf,
    /// "This project" or "All projects".
    pub scope: &'static str,
    pub text: String,
}

/// The agent, skill and rule files of the current tab.
#[derive(Debug, Default)]
pub struct Docs {
    pub files: Vec<DocFile>,
    pub selected: usize,
    pub full: bool,
}

/// Lists the definition files of `tab`, project scope first.
pub fn scan(tab: ConfigsTab, project: &Path, home: Option<&Path>) -> Vec<DocFile> {
    let global = home.map(|home| home.join(".omp/agent"));
    // The files in `dir` (or `<dir>/*/SKILL.md` with `skill`), named by stem or folder.
    let list = |dir: &Path, skill: bool| -> Vec<(PathBuf, String)> {
        let entries = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|entry| entry.path());
        let mut files: Vec<_> = match skill {
            true => entries.map(|path| path.join("SKILL.md")).filter(|path| path.is_file()).collect(),
            false => entries.filter(|path| path.extension().is_some_and(|ext| ext == "md")).collect(),
        };
        files.sort();
        let name = |path: &Path| if skill { path.parent().and_then(Path::file_name) } else { path.file_stem() }.map(|name| name.to_string_lossy().into_owned());
        files.into_iter().map(|path| (path.clone(), name(&path).unwrap_or_default())).collect()
    };
    let mut found: Vec<(PathBuf, String, &'static str)> = Vec::new();
    let mut add = |files: Vec<(PathBuf, String)>, scope| found.extend(files.into_iter().map(|(path, name)| (path, name, scope)));
    match tab {
        ConfigsTab::Agents | ConfigsTab::Skills => {
            let (local, shared, skill) = if tab == ConfigsTab::Agents { (".omp/agents", "agents", false) } else { (".omp/skills", "skills", true) };
            add(list(&project.join(local), skill), "This project");
            if let Some(global) = &global {
                add(list(&global.join(shared), skill), "All projects");
            }
        }
        ConfigsTab::Rules => {
            add(global.iter().map(|global| (global.join("RULES.md"), "RULES.md".to_owned())).collect(), "All projects");
            add(["RULES.md", "AGENTS.md", "CLAUDE.md"].map(|name| (project.join(name), name.to_owned())).to_vec(), "This project");
        }
        ConfigsTab::Models | ConfigsTab::Tools => {}
    }
    found.into_iter().filter_map(|(path, name, scope)| Some(DocFile { text: std::fs::read_to_string(&path).ok()?, name, path, scope })).collect()
}

/// A window drawn over the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Usage,
    Settings(SettingsTab),
}

/// What a detached window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detached {
    /// A panel tab, by id.
    Tab(String),
    /// A session (a subagent's), by library id.
    Session(String),
}

impl Detached {
    fn size(&self) -> Size {
        match self {
            Detached::Tab(tab) if !tab.starts_with("surface") => Size::new(width(LayoutRole::DetachWidth), width(LayoutRole::DetachHeight)),
            _ => Size::new(width(LayoutRole::PopupWidth), width(LayoutRole::TallHeight)),
        }
    }
}

/// The app's windows: the overlay, Configs and the detached ones.
#[derive(Default)]
pub(crate) struct Windows {
    pub overlay: Option<Overlay>,
    pub configs: Option<ConfigsTab>,
    pub docs: Docs,
    main: Option<window::Id>,
    detached: Vec<(window::Id, Detached)>,
}

#[derive(Debug, Clone)]
pub enum WindowMessage {
    OpenConfigs(ConfigsTab),
    CloseConfigs,
    PickDoc(usize),
    ToggleDocFull,
    OpenSettings(SettingsTab),
    CloseSettings,
    SetTheme(ThemeRef),
    /// Shows a panel tab in its own window.
    Detach(String),
    /// Brings a detached tab's window forward.
    Show(String),
    /// Puts a detached tab back in the panel.
    ReattachTab(String),
    /// A detached window's Reattach button.
    Reattach(window::Id),
    /// A window closed (the main one or a detached one).
    Closed(window::Id),
}

/// Settings of a new window of `size`.
fn settings(size: Size) -> window::Settings {
    window::Settings { size, min_size: Some(Size::new(width(LayoutRole::MinWidth), width(LayoutRole::MinHeight))), ..window::Settings::default() }
}

impl App {
    /// Opens the main window, once, when the daemon boots.
    pub fn open_main(&mut self) -> Task<Message> {
        let (id, open) = window::open(settings(Size::new(width(LayoutRole::MainWidth), width(LayoutRole::MainHeight))));
        self.windows.main = Some(id);
        open.discard()
    }

    pub(super) fn window_update(&mut self, message: WindowMessage) -> Task<Message> {
        let windows = &mut self.windows;
        match message {
            WindowMessage::OpenConfigs(tab) => {
                (windows.configs, windows.overlay) = (Some(tab), None);
                windows.docs = Docs { files: scan(tab, self.ctx.runtime.cwd(), loa_util::fs::home().as_deref()), selected: 0, full: false };
                let load = match tab {
                    ConfigsTab::Models => self.update(Message::area(areas::models::Message::Show(Page::Settings))),
                    _ => Task::none(),
                };
                // Areas open their own screen when they load; Configs draws them instead.
                self.ctx.close_screen();
                load
            }
            WindowMessage::CloseConfigs => {
                windows.configs = None;
                Task::none()
            }
            WindowMessage::PickDoc(index) => {
                windows.docs.selected = index.min(windows.docs.files.len().saturating_sub(1));
                Task::none()
            }
            WindowMessage::ToggleDocFull => {
                windows.docs.full = !windows.docs.full;
                Task::none()
            }
            WindowMessage::OpenSettings(tab) => {
                windows.overlay = Some(Overlay::Settings(tab));
                let load = match tab {
                    SettingsTab::Harnesses => self.update(Message::area(areas::models::Message::Show(Page::SignIn))),
                    SettingsTab::Updates => self.update(Message::area(areas::updater::Message::Open)),
                    SettingsTab::Peers => self.update(Message::area(areas::peers::Message::Refresh)),
                    SettingsTab::General => Task::none(),
                };
                self.ctx.close_screen();
                load
            }
            WindowMessage::CloseSettings => {
                windows.overlay = None;
                Task::none()
            }
            WindowMessage::SetTheme(preference) => self.update(Message::Shell(loa_ui_shell::Message::SetTheme(preference))),
            WindowMessage::Detach(tab) => self.detach(Detached::Tab(tab)),
            WindowMessage::Show(tab) => match self.detached_window(&Detached::Tab(tab)) {
                Some(id) => window::gain_focus(id),
                None => Task::none(),
            },
            WindowMessage::ReattachTab(tab) => match self.detached_window(&Detached::Tab(tab)) {
                Some(id) => self.window_update(WindowMessage::Reattach(id)),
                None => Task::none(),
            },
            WindowMessage::Reattach(id) => {
                let Some(what) = self.forget_window(id) else { return Task::none() };
                let show = if let Detached::Tab(tab) = what { tab } else { "agents".to_owned() };
                self.ctx.shell.show_tab(&show);
                Task::batch([window::close(id), self.ctx.shell.reveal_active_tab()])
            }
            // The main window quits the app; a detached one gives its view back to the panel.
            WindowMessage::Closed(id) if windows.main == Some(id) => iced::exit(),
            WindowMessage::Closed(id) => {
                self.forget_window(id);
                Task::none()
            }
        }
    }

    fn detached_window(&self, what: &Detached) -> Option<window::Id> {
        self.windows.detached.iter().find(|(_, shown)| shown == what).map(|(id, _)| *id)
    }

    /// Shows `what` in its own window, or brings its window forward.
    pub(super) fn detach(&mut self, what: Detached) -> Task<Message> {
        if let Some(id) = self.detached_window(&what) {
            return window::gain_focus(id);
        }
        let (id, open) = window::open(settings(what.size()));
        self.windows.detached.push((id, what));
        self.sync_detached();
        open.discard()
    }

    /// Drops detached window `id` and what it showed.
    fn forget_window(&mut self, id: window::Id) -> Option<Detached> {
        let index = self.windows.detached.iter().position(|(window, _)| *window == id)?;
        let (_, what) = self.windows.detached.remove(index);
        if let Detached::Session(session) = &what {
            self.sessions.detached.remove(session);
        }
        self.sync_detached();
        Some(what)
    }

    fn sync_detached(&mut self) {
        let tabs = self.windows.detached.iter().filter_map(|(_, what)| if let Detached::Tab(tab) = what { Some(tab.clone()) } else { None }).collect();
        self.ctx.shell.set_detached(tabs);
    }

    /// Loads a session without making it active and shows it in a window.
    pub(super) fn detach_session(&mut self, session: &str) -> Task<Message> {
        if let Err(error) = self.ctx.runtime.load_session(session) {
            self.ctx.notify(error);
            return Task::none();
        }
        if let Some(open) = self.ctx.runtime.sessions().get(session) {
            let theme = self.theme();
            self.sessions.detached.entry(session.to_owned()).or_insert_with(|| loa_ui_session::transcript::State::from_dom(open.dom(), &theme));
        }
        self.detach(Detached::Session(session.to_owned()))
    }

    /// The title of window `id`.
    pub fn title(&self, id: window::Id) -> String {
        match self.windows.detached.iter().find(|(window, _)| *window == id) {
            Some((_, what)) => format!("{} — Loa", self.detached_title(what)),
            None => "Loa".to_owned(),
        }
    }

    fn detached_title(&self, what: &Detached) -> String {
        match what {
            Detached::Tab(tab) => self.ctx.shell.tabs().iter().find(|shown| shown.id == *tab).map_or(tab.clone(), |shown| shown.title.clone()),
            Detached::Session(session) => self.ctx.shell.sidebar().session(session).map_or_else(|| "Subagent".to_owned(), |summary| summary.title.clone()),
        }
    }

    /// Draws a window: the app in the main one, the detached view elsewhere.
    pub fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        let Some((_, what)) = self.windows.detached.iter().find(|(window, _)| *window == id) else { return self.view() };
        let body = match what {
            Detached::Tab(tab) => self.view_tab(tab),
            Detached::Session(session) => self.detached_session(session),
        };
        let header = row![tinted(Icon::ExternalLink, TextRole::Body, Tone::Muted), label(self.detached_title(what), TextRole::Body, Tone::Normal).width(Fill)]
            .push(text_button("Reattach", ButtonKind::Ghost, Some(Message::Window(WindowMessage::Reattach(id)))))
            .spacing(sp(SpaceRole::S))
            .align_y(Alignment::Center);
        let header = container(header).padding([sp(SpaceRole::Xs), sp(SpaceRole::M)]).style(style::container(Surface::Side));
        container(column![header, rule::horizontal(1).style(style::rule), container(body).width(Fill).height(Fill)]).style(style::container(Surface::Ground)).width(Fill).height(Fill).into()
    }

    fn detached_session(&self, session: &str) -> Element<'_, Message> {
        let center = |s| container(label(s, TextRole::Body, Tone::Muted)).center(Fill).into();
        let Some(open) = self.ctx.runtime.sessions().get(session) else { return center("This session is no longer loaded") };
        let Some(chat) = self.sessions.detached.get(session) else { return center("Loading…") };
        let owned = session.to_owned();
        chat.view(open.dom(), move |message| Message::DetachedChat(owned.clone(), message), |slot| self.decision_card(open.id().as_str(), slot.node))
    }

    /// The Configs screen, drawn in the center.
    pub(super) fn configs_view(&self, tab: ConfigsTab) -> Element<'_, Message> {
        let documents = matches!(tab, ConfigsTab::Agents | ConfigsTab::Skills | ConfigsTab::Rules);
        if self.windows.docs.full && documents {
            return self.docs_view(tab);
        }
        let ctx = &self.ctx;
        let content = match tab {
            ConfigsTab::Models => self.areas.get::<areas::models::State>().page_body(Page::Settings).map(Message::area),
            ConfigsTab::Tools => self.areas.get::<areas::tools::State>().view_tab("tools", ctx).map_or_else(|| text("").into(), |view| view.map(Message::area)),
            _ => self.docs_view(tab),
        };
        let close = icon_button(Icon::X, "Close", Some(Message::Window(WindowMessage::CloseConfigs)));
        let body = column![section_header(tab.label(), [close]), label(tab.blurb(), TextRole::Label, Tone::Muted), container(content).height(Fill)];
        side_nav(None, &ConfigsTab::ALL, tab, |tab| Message::Window(WindowMessage::OpenConfigs(tab)), body.spacing(sp(SpaceRole::M)).into())
    }

    fn docs_view(&self, tab: ConfigsTab) -> Element<'_, Message> {
        let docs = &self.windows.docs;
        let Some(file) = docs.files.get(docs.selected) else {
            return card(label(format!("Nothing yet. omp reads these from {}.", tab.places()), TextRole::Body, Tone::Muted)).width(Fill).into();
        };
        let rows = docs.files.iter().enumerate().map(|(index, file)| two_line_row(file.name.as_str(), file.scope, index == docs.selected, Message::Window(WindowMessage::PickDoc(index))));
        let list = column(rows).spacing(sp(SpaceRole::Xs)).width(width(LayoutRole::NavWidth));
        let full = if docs.full { "Exit full screen" } else { "Full screen" };
        let toolbar = row![label(file.path.display().to_string(), TextRole::Caption, Tone::Muted).width(Fill), label(format!("{} lines", file.text.lines().count()), TextRole::Caption, Tone::Faint)]
            .push(text_button(full, ButtonKind::Ghost, Some(Message::Window(WindowMessage::ToggleDocFull))))
            .spacing(sp(SpaceRole::M))
            .align_y(Alignment::Center);
        let content = scrollable(mono_block(file.text.as_str())).direction(scrollable::Direction::Both { vertical: Default::default(), horizontal: Default::default() });
        let viewer = column![toolbar, content.height(if docs.full { Length::Fill } else { Length::Fixed(width(LayoutRole::ListWidth)) })].spacing(sp(SpaceRole::S)).width(Fill);
        if docs.full {
            return container(viewer).padding(sp(SpaceRole::Xl)).style(style::container(Surface::Ground)).width(Fill).height(Fill).into();
        }
        row![list, viewer].spacing(sp(SpaceRole::L)).into()
    }

    /// The Settings window, drawn over the app.
    pub(super) fn settings_view(&self, tab: SettingsTab) -> Element<'_, Message> {
        let ctx = &self.ctx;
        let missing = || text("").into();
        let content = match tab {
            SettingsTab::General => self.general_settings(),
            SettingsTab::Peers => self.areas.get::<areas::peers::State>().view_tab("peers", ctx).map_or_else(missing, |view| view.map(Message::area)),
            SettingsTab::Harnesses => self.areas.get::<areas::models::State>().page_body(Page::SignIn).map(Message::area),
            SettingsTab::Updates => self.areas.get::<areas::updater::State>().view_screen(ctx).map_or_else(missing, |view| view.map(Message::area)),
        };
        let close = icon_button(Icon::X, "Close", Some(Message::Window(WindowMessage::CloseSettings)));
        let body = column![section_header(tab.label(), [close]), container(content).height(Fill)].spacing(sp(SpaceRole::M));
        let nav = side_nav(Some("Settings"), &SettingsTab::ALL, tab, |tab| Message::Window(WindowMessage::OpenSettings(tab)), body.into());
        container(nav).style(style::container(Surface::Popup)).width(width(LayoutRole::WindowWidth)).height(width(LayoutRole::WindowHeight)).clip(true).into()
    }

    fn general_settings(&self) -> Element<'_, Message> {
        let theme = self.ctx.shell.theme_state();
        let current = theme.preference().as_str();
        let choices = theme.themes().into_iter().map(|(id, name)| {
            let kind = if id == current { ButtonKind::Primary } else { ButtonKind::Secondary };
            text_button(name, kind, Some(Message::Window(WindowMessage::SetTheme(ThemeRef(id)))))
        });
        let help = column![label("Theme", TextRole::Body, Tone::Normal), label("Follows your system unless set", TextRole::Label, Tone::Faint)].width(Fill);
        let setting = row![help, row(choices).spacing(sp(SpaceRole::Xs))].spacing(sp(SpaceRole::L)).align_y(Alignment::Center);
        column![setting, rule::horizontal(1).style(style::rule)].spacing(sp(SpaceRole::M)).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_lists_project_files_before_global_ones() {
        let project = tempfile::tempdir().expect("project");
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(project.path().join(".omp/agents")).unwrap();
        std::fs::write(project.path().join(".omp/agents/reviewer.md"), "---\nname: reviewer\n---\n").unwrap();
        std::fs::create_dir_all(home.path().join(".omp/agent/agents")).unwrap();
        std::fs::write(home.path().join(".omp/agent/agents/explore.md"), "explore").unwrap();
        std::fs::create_dir_all(home.path().join(".omp/agent/skills/git-workflow")).unwrap();
        std::fs::write(home.path().join(".omp/agent/skills/git-workflow/SKILL.md"), "skill").unwrap();
        std::fs::write(project.path().join("AGENTS.md"), "rules").unwrap();

        let agents = scan(ConfigsTab::Agents, project.path(), Some(home.path()));
        let names: Vec<_> = agents.iter().map(|f| (f.name.as_str(), f.scope)).collect();
        assert_eq!(names, [("reviewer", "This project"), ("explore", "All projects")]);
        let skills = scan(ConfigsTab::Skills, project.path(), Some(home.path()));
        assert_eq!(skills.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["git-workflow"]);
        let rules = scan(ConfigsTab::Rules, project.path(), Some(home.path()));
        assert_eq!(rules.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["AGENTS.md"]);
    }
}
