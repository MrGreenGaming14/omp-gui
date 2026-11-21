//! A peer's shared folder opened on this node: its files listed over
//! `omp/folder/1`, folders entered, a file opened in a preview, and with a
//! write grant edited and saved back to the peer.

use iced::widget::{button, column, container, row, scrollable, text_editor};
use iced::{Alignment, Element, Fill, Task};
use loa_p2p::{FolderEntry, Node};
use loa_ui_kit::icons::tinted;
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, text_size as ts, width};
use loa_ui_kit::widgets::{card, icon_button, label, text_button};
use loa_view::theme::Icon;

use super::{Message, State};
use crate::area::Ctx;

#[derive(Clone, Debug)]
pub enum FolderMessage {
    /// Enter the subfolder of the current folder with this name.
    Enter(String),
    /// Go to the folder `depth` levels below the shared folder (0 is the shared folder).
    Crumb(usize),
    Refresh,
    Listed { path: String, result: Result<Vec<FolderEntry>, String> },
    /// Open the file of the current folder with this name.
    Open(String),
    Opened { path: String, result: Result<Vec<u8>, String> },
    Edit(text_editor::Action),
    Save,
    Saved { path: String, result: Result<(), String> },
    CloseFile,
    Close,
}

/// What an opened file shows.
pub enum Preview {
    Text(text_editor::Content),
    /// Not UTF-8 text: only its size.
    Binary(usize),
}

pub struct OpenFile {
    /// Relative to the shared folder.
    pub path: String,
    pub preview: Preview,
    /// Edited since it was opened or saved.
    pub dirty: bool,
    saving: bool,
}

/// The shared folder on screen.
pub struct Browser {
    node: Node,
    pub(crate) peer: String,
    peer_name: String,
    /// The granted folder, absolute on the peer.
    pub(crate) folder: String,
    /// The folder shown, relative to `folder`; empty for the folder itself.
    pub(crate) path: String,
    pub(crate) entries: Vec<FolderEntry>,
    pub(crate) can_write: bool,
    pub(crate) file: Option<OpenFile>,
    /// A listing is on its way.
    loading: bool,
}

/// `name` inside the relative folder `dir`.
fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_owned() } else { format!("{dir}/{name}") }
}

fn size_text(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KiB", b as f64 / 1024.0),
        b => format!("{:.1} MiB", b as f64 / (1024.0 * 1024.0)),
    }
}

impl Browser {
    pub fn new(node: Node, peer: String, peer_name: String, folder: String, entries: Vec<FolderEntry>, can_write: bool) -> Self {
        Self { node, peer, peer_name, folder, path: String::new(), entries, can_write, file: None, loading: false }
    }

    fn list(&mut self, path: String) -> Task<Message> {
        self.loading = true;
        let (node, peer, folder) = (self.node.clone(), self.peer.clone(), self.folder.clone());
        Task::perform(
            async move {
                let result = node.list_folder(&peer, &folder, &path).await.map_err(|error| error.to_string());
                (path, result)
            },
            |(path, result)| Message::Folder(FolderMessage::Listed { path, result }),
        )
    }

    pub fn view(&self) -> Element<'_, FolderMessage> {
        let access = if self.can_write { "read, write" } else { "read only" };
        let title = column![label(self.folder.as_str(), TextRole::Title, Tone::Normal), label(format!("Shared by {} · {access}", self.peer_name), TextRole::Caption, Tone::Muted)];
        let actions = [text_button("Refresh", ButtonKind::Secondary, Some(FolderMessage::Refresh)), icon_button(Icon::X, "Close", Some(FolderMessage::Close))];
        let header = row![title.spacing(sp(SpaceRole::Xs)).width(Fill)].extend(actions).spacing(sp(SpaceRole::M)).align_y(Alignment::Center);

        let parts: Vec<&str> = self.path.split('/').filter(|part| !part.is_empty()).collect();
        let root = self.folder.rsplit('/').find(|part| !part.is_empty()).unwrap_or(self.folder.as_str());
        let crumbs = std::iter::once(root).chain(parts.iter().copied()).enumerate().map(|(depth, part)| {
            let chevron = (depth > 0).then(|| tinted(Icon::ChevronRight, TextRole::Caption, Tone::Muted));
            let crumb = button(label(part, TextRole::Label, Tone::Normal)).padding(sp(SpaceRole::Xs) / 2.0).style(style::button(ButtonKind::Row { selected: depth == parts.len() }));
            row![].push(chevron).push(crumb.on_press(FolderMessage::Crumb(depth))).align_y(Alignment::Center).into()
        });
        let crumbs = row(crumbs).spacing(sp(SpaceRole::Xs) / 2.0);

        let empty = self.entries.is_empty().then(|| label(if self.loading { "Listing…" } else { "This folder is empty" }, TextRole::Label, Tone::Muted));
        let entries = self.entries.iter().map(|entry| {
            let selected = self.file.as_ref().is_some_and(|file| file.path == join(&self.path, &entry.name));
            let (glyph, detail, message) = match entry.dir {
                true => (Icon::Folder, String::new(), FolderMessage::Enter(entry.name.clone())),
                false => (Icon::FileText, size_text(entry.size), FolderMessage::Open(entry.name.clone())),
            };
            let content = row![tinted(glyph, TextRole::Body, Tone::Muted), label(entry.name.as_str(), TextRole::Label, Tone::Normal).width(Fill), label(detail, TextRole::Caption, Tone::Muted)];
            let content = content.spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
            button(content).width(Fill).padding([sp(SpaceRole::Xs), sp(SpaceRole::S)]).style(style::button(ButtonKind::Row { selected })).on_press(message).into()
        });
        let list = column![].push(empty).extend(entries).spacing(sp(SpaceRole::Xs) / 2.0);
        let files = card(scrollable(list).height(Fill)).padding(sp(SpaceRole::Xs)).width(width(LayoutRole::ListWidth)).height(Fill);

        let preview = match &self.file {
            None => card(label("Pick a file to open it here", TextRole::Label, Tone::Muted)).center(Fill),
            Some(file) => {
                let editable = self.can_write && matches!(file.preview, Preview::Text(_));
                let state: Element<'_, FolderMessage> = match editable {
                    true => text_button(if file.saving { "Saving…" } else if file.dirty { "Save" } else { "Saved" }, ButtonKind::Primary, (file.dirty && !file.saving).then_some(FolderMessage::Save)),
                    false => label("View only", TextRole::Caption, Tone::Muted).into(),
                };
                let bar = row![tinted(Icon::FileText, TextRole::Body, Tone::Muted), label(file.path.as_str(), TextRole::Label, Tone::Normal).width(Fill), state, icon_button(Icon::X, "Close file", Some(FolderMessage::CloseFile))];
                let body: Element<'_, FolderMessage> = match &file.preview {
                    Preview::Text(content) => {
                        let editor = text_editor(content).size(ts(TextRole::Label)).font(iced::Font::MONOSPACE).height(Fill).padding(sp(SpaceRole::S));
                        if editable { editor.on_action(FolderMessage::Edit).into() } else { editor.into() }
                    }
                    Preview::Binary(size) => container(label(format!("Binary file, {}; no text preview", size_text(*size as u64)), TextRole::Label, Tone::Muted)).center(Fill).into(),
                };
                card(column![bar.spacing(sp(SpaceRole::S)).align_y(Alignment::Center), body].spacing(sp(SpaceRole::S))).padding(sp(SpaceRole::S)).height(Fill)
            }
        };
        let panes = row![files, container(preview).width(Fill).height(Fill)].spacing(sp(SpaceRole::M)).height(Fill);
        column![header, crumbs, panes].spacing(sp(SpaceRole::M)).padding([sp(SpaceRole::L), sp(SpaceRole::Xl)]).into()
    }
}

impl State {
    pub(super) fn folder_update(&mut self, message: FolderMessage, ctx: &mut Ctx) -> Task<Message> {
        let Some(browser) = &mut self.folder else { return Task::none() };
        match message {
            FolderMessage::Enter(name) => {
                let path = join(&browser.path, &name);
                browser.list(path)
            }
            FolderMessage::Crumb(depth) => {
                let path: Vec<&str> = browser.path.split('/').filter(|part| !part.is_empty()).take(depth).collect();
                let path = path.join("/");
                browser.list(path)
            }
            FolderMessage::Refresh => {
                let path = browser.path.clone();
                browser.list(path)
            }
            FolderMessage::Listed { path, result } => {
                browser.loading = false;
                match result {
                    Ok(entries) => {
                        browser.path = path;
                        browser.entries = entries;
                    }
                    Err(error) => ctx.notify(format!("{} didn't list the folder: {error}", browser.peer_name)),
                }
                Task::none()
            }
            FolderMessage::Open(name) => {
                let path = join(&browser.path, &name);
                let (node, peer, folder) = (browser.node.clone(), browser.peer.clone(), browser.folder.clone());
                Task::perform(
                    async move {
                        let result = node.read_folder_file(&peer, &folder, &path).await.map_err(|error| error.to_string());
                        (path, result)
                    },
                    |(path, result)| Message::Folder(FolderMessage::Opened { path, result }),
                )
            }
            FolderMessage::Opened { path, result } => {
                match result {
                    Ok(bytes) => {
                        let preview = match String::from_utf8(bytes) {
                            Ok(text) if !text.contains('\0') => Preview::Text(text_editor::Content::with_text(&text)),
                            Ok(text) => Preview::Binary(text.len()),
                            Err(error) => Preview::Binary(error.as_bytes().len()),
                        };
                        browser.file = Some(OpenFile { path, preview, dirty: false, saving: false });
                    }
                    Err(error) => ctx.notify(format!("{} didn't open {path}: {error}", browser.peer_name)),
                }
                Task::none()
            }
            FolderMessage::Edit(action) => {
                let can_write = browser.can_write;
                if let Some(OpenFile { preview: Preview::Text(content), dirty, .. }) = &mut browser.file {
                    let edit = action.is_edit();
                    // Without a write grant the preview only scrolls and selects.
                    if !edit || can_write {
                        content.perform(action);
                        *dirty |= edit;
                    }
                }
                Task::none()
            }
            FolderMessage::Save => {
                let Some(file) = &mut browser.file else { return Task::none() };
                let Preview::Text(content) = &file.preview else { return Task::none() };
                if !browser.can_write || file.saving {
                    return Task::none();
                }
                file.saving = true;
                let (node, peer, folder, path) = (browser.node.clone(), browser.peer.clone(), browser.folder.clone(), file.path.clone());
                let bytes = content.text().into_bytes();
                Task::perform(
                    async move {
                        let result = node.write_folder_file(&peer, &folder, &path, &bytes).await.map_err(|error| error.to_string());
                        (path, result)
                    },
                    |(path, result)| Message::Folder(FolderMessage::Saved { path, result }),
                )
            }
            FolderMessage::Saved { path, result } => {
                if let Some(file) = browser.file.as_mut().filter(|file| file.path == path) {
                    file.saving = false;
                    if result.is_ok() {
                        file.dirty = false;
                    }
                }
                match result {
                    Ok(()) => {
                        ctx.notify(format!("Saved {path} on {}", browser.peer_name));
                        let current = browser.path.clone();
                        browser.list(current)
                    }
                    Err(error) => {
                        ctx.notify(format!("{} didn't save {path}: {error}", browser.peer_name));
                        Task::none()
                    }
                }
            }
            FolderMessage::CloseFile => {
                browser.file = None;
                Task::none()
            }
            FolderMessage::Close => {
                self.folder = None;
                ctx.close_screen();
                Task::none()
            }
        }
    }
}
