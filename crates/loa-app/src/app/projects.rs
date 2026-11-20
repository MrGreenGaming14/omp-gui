//! The project tab strip.

use super::*;

impl App {
    /// Makes `project` the active one, with its default folder `cwd` when it
    /// has one: new sessions, new terminals, the Files and Diff tabs follow,
    /// every area is told and the choice is saved. Running sessions and open
    /// terminals keep their folder.
    pub(super) fn activate_project(&mut self, project: String, cwd: Option<PathBuf>) -> Task<Message> {
        self.ctx.shell.set_active_project(Some(project.clone()));
        self.add_project_tab(&project);
        let cwd = cwd.unwrap_or_else(|| self.ctx.services.cwd.clone());
        let services = &mut self.ctx.services;
        if services.project.as_deref() == Some(project.as_str()) && services.cwd == cwd {
            return Task::none();
        }
        services.set_active(project.clone(), Some(cwd.clone()));
        self.files.set_root(cwd.clone());
        // The composer's folder bar, the folder menu and the terminal follow
        // too, as they do for a folder picked in the composer.
        self.recent_folders.retain(|folder| *folder != cwd);
        self.recent_folders.insert(0, cwd.clone());
        self.recent_folders.truncate(8);
        self.ctx.composer.set_workspace(Some(workspace(&cwd)));
        self.refresh_folders();
        let terminal = self.terminal_follow(cwd.clone());
        let saved = loa_config::ActiveProject { name: project.clone(), cwd: cwd.clone() };
        self.save_ui(|config| config.active_project = Some(saved));
        let tasks = with_areas!(each_area!(self.areas, mut |area, wrap| area
            .project_activated(&project, &cwd, &mut self.ctx)
            .map(wrap)));
        Task::batch(tasks.into_iter().chain([terminal, working_diff(cwd), self.settle()]))
    }

    /// Gives `project` a tab in the project strip when it has none.
    fn add_project_tab(&mut self, project: &str) {
        if !self.open_projects.iter().any(|open| open == project) {
            self.open_projects.push(project.to_owned());
            self.ctx.shell.set_open_projects(self.open_projects.clone());
        }
    }

    /// Drops the tab of `project`. Its sessions stay open, and it gets a tab
    /// again only when it is activated again.
    pub(super) fn close_project_tab(&mut self, project: &str) {
        self.open_projects.retain(|open| open != project);
        self.ctx.shell.set_open_projects(self.open_projects.clone());
    }

    /// Gives a tab to the project of every session opened since the last
    /// call. A session whose journal has no root yet is looked at again next
    /// time.
    pub(super) fn sync_project_tabs(&mut self) {
        let mut seen = Vec::new();
        for session in self.ctx.sessions.iter() {
            if self.tabbed_sessions.contains(session.id().as_str()) {
                continue;
            }
            let Some(root) = session.dom().root() else {
                continue;
            };
            seen.push((session.id().to_string(), root.str_attr(attr::PROJECT).map(str::to_owned)));
        }
        for (id, project) in seen {
            self.tabbed_sessions.insert(id);
            if let Some(project) = project.filter(|project| !project.is_empty()) {
                self.add_project_tab(&project);
            }
        }
    }

    /// Activates the project tab after (or before) the active project's,
    /// wrapping around, the way a click on that tab does.
    pub(super) fn step_project_tab(&mut self, forward: bool) -> Task<Message> {
        if !self.ctx.shell.project_tabs() {
            self.ctx.notify("Project tabs are off; turn them on from the palette");
            return Task::none();
        }
        let tabs = &self.open_projects;
        let Some(last) = tabs.len().checked_sub(1) else {
            self.ctx.notify("No project tabs are open");
            return Task::none();
        };
        let current = tabs.iter().position(|open| self.ctx.services.project.as_deref() == Some(open.as_str()));
        let index = match (current, forward) {
            (Some(index), true) => if index == last { 0 } else { index + 1 },
            (Some(index), false) => index.checked_sub(1).unwrap_or(last),
            (None, true) => 0,
            (None, false) => last,
        };
        let project = tabs[index].clone();
        self.update(Message::Projects(areas::projects::Message::Open(project)))
    }

    /// The composer's project chip. The shown session moves to `project`;
    /// with none shown, new sessions join it.
    pub(super) fn project_picked(&mut self, project: Option<String>) -> Task<Message> {
        if let Some(session) = self.ctx.sessions.active() {
            let session = session.id().to_string();
            return self.update(Message::Projects(areas::projects::Message::Move { session, project }));
        }
        match project {
            Some(project) => self.update(Message::Projects(areas::projects::Message::Activate(project))),
            None => {
                self.clear_project();
                Task::none()
            }
        }
    }

    /// No project is active: new sessions join none.
    fn clear_project(&mut self) {
        self.ctx.services.project = None;
        self.ctx.shell.set_active_project(None);
        self.save_ui(|config| config.active_project = None);
    }

    /// Drops every trace of the deleted `project`: its tab, its pin, and its
    /// place as the active project.
    pub(super) fn forget_project(&mut self, project: &str) -> Task<Message> {
        self.close_project_tab(project);
        let mut pins = self.ctx.shell.sidebar().pinned_projects().clone();
        if pins.remove(project) {
            self.ctx.shell.set_pinned_projects(pins.clone());
            self.save_ui(|config| config.pinned_projects = pins);
        }
        if self.ctx.services.project.as_deref() == Some(project) {
            self.clear_project();
        }
        Task::none()
    }
}
