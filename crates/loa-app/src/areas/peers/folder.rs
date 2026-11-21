//! A peer's shared folder opened on this node: its files listed over
//! `omp/folder/1`, folders entered, a file opened in a preview, and with a
//! write grant edited and saved back to the peer. The peer checks every
//! request against its grant and keeps it inside the folder.

use iced::widget::{button, column, container, row, scrollable, space, text, text_editor};
use iced::{Alignment, Element, Fill, Length, Task};
use loa_node::{FolderEntry, Node};
use loa_ui_kit::icons::{Icon, icon, muted};
use loa_ui_shell::theme::style;

use super::{Message, State};
use crate::area::Ctx;

/// Width of the file list beside the preview.
const LIST_WIDTH: f32 = 380.0;

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
        let header = row![
            column![
                text(self.folder.as_str()).size(20),
                text(format!("Shared by {} · {access}", self.peer_name)).size(12).style(style::muted),
            ]
            .spacing(4)
            .width(Fill),
            button(text("Refresh").size(13)).style(button::secondary).on_press(FolderMessage::Refresh),
            button(icon(Icon::X, 16.0)).style(button::text).on_press(FolderMessage::Close),
        ]
        .spacing(12)
        .align_y(Alignment::Center);

        let parts: Vec<&str> = self.path.split('/').filter(|part| !part.is_empty()).collect();
        let root_name = self.folder.rsplit('/').find(|part| !part.is_empty()).unwrap_or(self.folder.as_str());
        let mut crumbs = row![crumb(root_name, 0, parts.is_empty())].spacing(2).align_y(Alignment::Center);
        for (index, part) in parts.iter().enumerate() {
            crumbs = crumbs.push(muted(Icon::ChevronRight, 12.0)).push(crumb(part, index + 1, index + 1 == parts.len()));
        }

        let mut list = column![].spacing(2);
        if self.entries.is_empty() {
            let empty = if self.loading { "Listing…" } else { "This folder is empty" };
            list = list.push(container(text(empty).size(13).style(style::muted)).padding([8, 10]));
        }
        for entry in &self.entries {
            let selected = self.file.as_ref().is_some_and(|file| file.path == join(&self.path, &entry.name));
            let (glyph, detail, message) = if entry.dir {
                (Icon::Folder, String::new(), FolderMessage::Enter(entry.name.clone()))
            } else {
                (Icon::FileText, size_text(entry.size), FolderMessage::Open(entry.name.clone()))
            };
            list = list.push(
                button(
                    row![
                        muted(glyph, 14.0),
                        text(entry.name.as_str()).size(13).width(Fill),
                        text(detail).size(12).style(style::muted),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                )
                .width(Fill)
                .padding([6, 10])
                .style(style::row_for(selected))
                .on_press(message),
            );
        }
        let files = container(scrollable(list).height(Fill)).width(Length::Fixed(LIST_WIDTH)).height(Fill).padding(4).style(style::card);

        let preview: Element<'_, FolderMessage> = match &self.file {
            None => container(text("Pick a file to open it here").size(13).style(style::muted)).center(Fill).style(style::card).into(),
            Some(file) => {
                let mut bar = row![
                    muted(Icon::FileText, 14.0),
                    text(file.path.as_str()).size(13).width(Fill),
                ]
                .spacing(8)
                .align_y(Alignment::Center);
                let editable = self.can_write && matches!(file.preview, Preview::Text(_));
                if editable {
                    let label = if file.saving { "Saving…" } else if file.dirty { "Save" } else { "Saved" };
                    bar = bar.push(
                        button(text(label).size(12))
                            .style(button::primary)
                            .padding([4, 10])
                            .on_press_maybe((file.dirty && !file.saving).then_some(FolderMessage::Save)),
                    );
                } else {
                    bar = bar.push(text("View only").size(12).style(style::muted));
                }
                bar = bar.push(button(icon(Icon::X, 14.0)).style(button::text).on_press(FolderMessage::CloseFile));
                let body: Element<'_, FolderMessage> = match &file.preview {
                    Preview::Text(content) => {
                        let editor = text_editor(content).size(13).font(iced::Font::MONOSPACE).height(Fill).padding(10);
                        if editable { editor.on_action(FolderMessage::Edit).into() } else { editor.into() }
                    }
                    Preview::Binary(size) => container(
                        text(format!("Binary file, {}; no text preview", size_text(*size as u64))).size(13).style(style::muted),
                    )
                    .center(Fill)
                    .into(),
                };
                container(column![bar, body].spacing(8)).padding(10).height(Fill).style(style::card).into()
            }
        };

        column![
            header,
            crumbs,
            row![files, container(preview).width(Fill).height(Fill)].spacing(12).height(Fill),
            space().height(4),
        ]
        .spacing(12)
        .padding([16, 24])
        .into()
    }
}

fn crumb(label: &str, depth: usize, current: bool) -> Element<'_, FolderMessage> {
    button(text(label).size(13)).padding([2, 6]).style(style::row_for(current)).on_press(FolderMessage::Crumb(depth)).into()
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
