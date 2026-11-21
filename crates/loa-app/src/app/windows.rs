//! Configs and Settings, each a window over the app: a side menu over
//! existing area screens plus the agent, skill and rule files omp reads.
//!
//! Configs is for what shapes the agents: model roles, tools and MCP
//! servers, agent definitions, skills and rules, at project or global scope.
//! Settings is for the app itself: theme, peers, harness sign-in, updates.

use std::path::{Path, PathBuf};

use iced::widget::{Column, button, column, container, row, rule, scrollable, space, text};
use iced::{Alignment, Element, Fill, Length, Task};
use loa_ui_icons::{Icon, muted};
use loa_ui_models::screen::Page;
use loa_ui_shell::theme::style;
use loa_ui_shell::{Preference, layout};

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
    const ALL: [(ConfigsTab, &'static str, &'static str); 5] = [
        (ConfigsTab::Models, "Model roles", "Models"),
        (ConfigsTab::Agents, "Agents", "Delegation"),
        (ConfigsTab::Skills, "Skills", "Delegation"),
        (ConfigsTab::Tools, "Tools & MCP", "Behavior"),
        (ConfigsTab::Rules, "Rules", "Behavior"),
    ];

    fn title(self) -> &'static str {
        Self::ALL.iter().find(|(tab, ..)| *tab == self).map_or("", |(_, title, _)| title)
    }

    fn blurb(self) -> &'static str {
        match self {
            ConfigsTab::Models => "Which model and effort each role runs on, and where a role falls back when its model fails.",
            ConfigsTab::Tools => "Built-in tools, MCP servers and remote hosts the harness may call.",
            ConfigsTab::Agents => "Agent definitions omp can delegate to. Each is a Markdown file with front matter.",
            ConfigsTab::Skills => "Skills load when a task matches their description.",
            ConfigsTab::Rules => "Instruction files loaded into every session, in this order.",
        }
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
    const ALL: [(SettingsTab, &'static str); 4] = [
        (SettingsTab::General, "General"),
        (SettingsTab::Peers, "Peers & sharing"),
        (SettingsTab::Harnesses, "Harnesses"),
        (SettingsTab::Updates, "Updates"),
    ];
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

/// Lists the definition files for a tab, project scope first.
pub fn scan(tab: ConfigsTab, project: &Path, home: Option<&Path>) -> Vec<DocFile> {
    let global = home.map(|home| home.join(".omp/agent"));
    let mut out = Vec::new();
    let mut push = |path: PathBuf, name: String, scope: &'static str| {
        if let Ok(text) = std::fs::read_to_string(&path) {
            out.push(DocFile { name, path, scope, text });
        }
    };
    let markdown_in = |dir: &Path| -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
            .collect();
        files.sort();
        files
    };
    let skill_dirs = |dir: &Path| -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("SKILL.md"))
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        files
    };
    let stem = |path: &Path| path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let parent_name = |path: &Path| {
        path.parent().and_then(Path::file_name).map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
    };
    match tab {
        ConfigsTab::Agents => {
            for path in markdown_in(&project.join(".omp/agents")) {
                let name = stem(&path);
                push(path, name, "This project");
            }
            if let Some(global) = &global {
                for path in markdown_in(&global.join("agents")) {
                    let name = stem(&path);
                    push(path, name, "All projects");
                }
            }
        }
        ConfigsTab::Skills => {
            for path in skill_dirs(&project.join(".omp/skills")) {
                let name = parent_name(&path);
                push(path, name, "This project");
            }
            if let Some(global) = &global {
                for path in skill_dirs(&global.join("skills")) {
                    let name = parent_name(&path);
                    push(path, name, "All projects");
                }
            }
        }
        ConfigsTab::Rules => {
            if let Some(global) = &global {
                push(global.join("RULES.md"), "RULES.md".into(), "All projects");
            }
            for name in ["RULES.md", "AGENTS.md", "CLAUDE.md"] {
                push(project.join(name), name.into(), "This project");
            }
        }
        ConfigsTab::Models | ConfigsTab::Tools => {}
    }
    out
}

fn home() -> Option<PathBuf> {
    loa_core::home_dir()
}

#[derive(Debug, Clone)]
pub enum WindowMessage {
    OpenConfigs(ConfigsTab),
    PickDoc(usize),
    ToggleDocFull,
    OpenSettings(SettingsTab),
    CloseOverlay,
    SetTheme(Preference),
}

impl App {
    pub(super) fn window_update(&mut self, message: WindowMessage) -> Task<Message> {
        match message {
            WindowMessage::OpenConfigs(tab) => {
                self.overlay = Some(Overlay::Configs(tab));
                self.docs = Docs { files: scan(tab, &self.ctx.services.cwd, home().as_deref()), selected: 0, full: false };
                let load = match tab {
                    ConfigsTab::Models => self.update(Message::Models(areas::models::Message::Show(Page::Settings))),
                    _ => Task::none(),
                };
                // Areas open their own screen when they load; Configs draws them instead.
                self.ctx.close_screen();
                load
            }
            WindowMessage::PickDoc(index) => {
                self.docs.selected = index.min(self.docs.files.len().saturating_sub(1));
                Task::none()
            }
            WindowMessage::ToggleDocFull => {
                self.docs.full = !self.docs.full;
                Task::none()
            }
            WindowMessage::OpenSettings(tab) => {
                self.overlay = Some(Overlay::Settings(tab));
                let load = match tab {
                    SettingsTab::Harnesses => self.update(Message::Models(areas::models::Message::Show(Page::SignIn))),
                    SettingsTab::Updates => self.update(Message::Updater(areas::updater::Message::Open)),
                    SettingsTab::Peers => self.update(Message::Peers(areas::peers::Message::Refresh)),
                    SettingsTab::General => Task::none(),
                };
                self.ctx.close_screen();
                load
            }
            WindowMessage::CloseOverlay => {
                self.overlay = None;
                Task::none()
            }
            WindowMessage::SetTheme(preference) => {
                self.update(Message::Shell(layout::Message::SetTheme(preference)))
            }
        }
    }

    /// The Configs window, drawn over the app.
    pub(super) fn configs_view(&self, tab: ConfigsTab) -> Element<'_, Message> {
        let wrap = |message: WindowMessage| Message::Window(message);
        let mut nav = Column::new().spacing(2).padding([16, 10]).width(200);
        let mut group = "";
        for (item, title, heading) in ConfigsTab::ALL {
            if heading != group {
                group = heading;
                nav = nav.push(container(text(heading).size(12).style(style::faint)).padding([10, 10]));
            }
            nav = nav.push(
                button(text(title).size(14))
                    .on_press(wrap(WindowMessage::OpenConfigs(item)))
                    .style(style::row_for(item == tab))
                    .padding([7, 10])
                    .width(Fill),
            );
        }

        let header = row![
            column![text(tab.title()).size(20).font(semibold()), text(tab.blurb()).size(13).style(style::muted)]
                .spacing(4)
                .width(Fill),
            button(muted(Icon::X, 15.0)).on_press(wrap(WindowMessage::CloseOverlay)).style(style::ghost_button).padding(6),
        ]
        .spacing(12)
        .align_y(Alignment::Start);

        let ctx = &self.ctx;
        let body: Element<'_, Message> = match tab {
            ConfigsTab::Models => self.areas.models.page_body(Page::Settings).map(Message::Models),
            ConfigsTab::Tools => {
                self.areas.tools.view_tab("tools", ctx).map(|view| view.map(Message::Tools)).unwrap_or_else(|| text("").into())
            }
            ConfigsTab::Agents | ConfigsTab::Skills | ConfigsTab::Rules => self.docs_view(tab),
        };

        if self.docs.full && matches!(tab, ConfigsTab::Agents | ConfigsTab::Skills | ConfigsTab::Rules) {
            return window_frame(self.docs_view(tab));
        }

        window_frame(row![
            container(nav).style(style::side).height(Fill),
            rule::vertical(1).style(style::divider),
            column![container(header).padding([20, 28]), container(body).width(Fill).height(Fill).padding([0, 28])].width(Fill),
        ])
    }

    fn docs_view(&self, tab: ConfigsTab) -> Element<'_, Message> {
        let wrap = |message: WindowMessage| Message::Window(message);
        let docs = &self.docs;
        if docs.files.is_empty() {
            let place = match tab {
                ConfigsTab::Agents => ".omp/agents/ or ~/.omp/agent/agents/",
                ConfigsTab::Skills => ".omp/skills/<name>/SKILL.md or ~/.omp/agent/skills/",
                _ => "RULES.md, AGENTS.md or ~/.omp/agent/RULES.md",
            };
            return container(text!("Nothing yet. omp reads these from {place}.").size(14).style(style::muted))
                .style(style::card)
                .padding(20)
                .width(Fill)
                .into();
        }
        let mut list = Column::new().spacing(2).width(240);
        for (index, file) in docs.files.iter().enumerate() {
            list = list.push(
                button(column![text(file.name.as_str()).size(14), text(file.scope).size(12).style(style::faint)].spacing(2))
                    .on_press(wrap(WindowMessage::PickDoc(index)))
                    .style(style::row_for(index == docs.selected))
                    .padding([8, 12])
                    .width(Fill),
            );
        }
        let file = &docs.files[docs.selected.min(docs.files.len() - 1)];
        let lines = file.text.lines().count();
        let toolbar = row![
            text(file.path.display().to_string()).size(12).font(iced::Font::MONOSPACE).style(style::muted).width(Fill),
            text!("{lines} lines").size(12).style(style::faint),
            button(row![muted(if docs.full { Icon::Minimize } else { Icon::Maximize }, 13.0), text(if docs.full { "Exit full screen" } else { "Full screen" }).size(13)].spacing(6).align_y(Alignment::Center))
                .on_press(wrap(WindowMessage::ToggleDocFull))
                .style(style::ghost_button)
                .padding([5, 8]),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let content = scrollable(container(text(file.text.as_str()).size(13).font(iced::Font::MONOSPACE)).padding(16).width(Fill))
            .direction(scrollable::Direction::Both { vertical: Default::default(), horizontal: Default::default() })
            .height(if docs.full { Length::Fill } else { Length::Fixed(420.0) });
        let viewer = column![toolbar, container(content).style(style::side).width(Fill)].spacing(8).width(Fill);
        if docs.full {
            return container(viewer).padding(24).style(style::surface).width(Fill).height(Fill).into();
        }
        row![list, viewer].spacing(20).into()
    }

    /// The Settings window, drawn over the app.
    pub(super) fn settings_view(&self, tab: SettingsTab) -> Element<'_, Message> {
        let wrap = |message: WindowMessage| Message::Window(message);
        let mut nav = Column::new().spacing(2).padding([18, 10]).width(200);
        nav = nav.push(container(text("Settings").size(18).font(semibold())).padding([0, 10]));
        nav = nav.push(space().height(8));
        for (item, title) in SettingsTab::ALL {
            nav = nav.push(
                button(text(title).size(14))
                    .on_press(wrap(WindowMessage::OpenSettings(item)))
                    .style(style::row_for(item == tab))
                    .padding([7, 10])
                    .width(Fill),
            );
        }
        let title = SettingsTab::ALL.iter().find(|(item, _)| *item == tab).map_or("", |(_, title)| title);
        let header = row![
            text(title).size(16).font(semibold()).width(Fill),
            button(muted(Icon::X, 15.0)).on_press(wrap(WindowMessage::CloseOverlay)).style(style::ghost_button).padding(6),
        ]
        .align_y(Alignment::Center);
        let ctx = &self.ctx;
        let body: Element<'_, Message> = match tab {
            SettingsTab::General => self.general_settings(),
            SettingsTab::Peers => {
                self.areas.peers.view_tab("peers", ctx).map(|view| view.map(Message::Peers)).unwrap_or_else(|| text("").into())
            }
            SettingsTab::Harnesses => self.areas.models.page_body(Page::SignIn).map(Message::Models),
            SettingsTab::Updates => {
                self.areas.updater.view_screen(ctx).map(|view| view.map(Message::Updater)).unwrap_or_else(|| text("").into())
            }
        };
        window_frame(row![
            container(nav).style(style::side).height(Fill),
            column![container(header).padding([16, 20]), container(body).padding([0, 20]).width(Fill).height(Fill)].width(Fill),
        ])
    }

    fn general_settings(&self) -> Element<'_, Message> {
        let current = self.ctx.shell.theme_preference();
        let mut choices = row![].spacing(6);
        for preference in Preference::ALL {
            choices = choices.push(
                button(text(preference.to_string()).size(13))
                    .on_press(Message::Window(WindowMessage::SetTheme(preference)))
                    .style(if preference == current { style::primary_button } else { style::secondary_button })
                    .padding([6, 12]),
            );
        }
        let setting = |label: &'static str, help: &'static str, control: Element<'static, Message>| -> Element<'static, Message> {
            row![column![text(label).size(14), text(help).size(13).style(style::faint)].spacing(2).width(Fill), control]
                .spacing(16)
                .align_y(Alignment::Center)
                .into()
        };
        column![
            setting("Theme", "Follows your system unless set", choices.into()),
            rule::horizontal(1).style(style::divider),
        ]
        .spacing(14)
        .into()
    }
}

/// A window drawn over the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Usage,
    Configs(ConfigsTab),
    Settings(SettingsTab),
}

/// The frame both windows draw their content in.
fn window_frame<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content).style(style::window).width(900).height(640).clip(true).into()
}

pub(crate) fn semibold() -> iced::Font {
    iced::Font { weight: iced::font::Weight::Bold, ..iced::Font::default() }
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
