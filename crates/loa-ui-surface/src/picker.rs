//! An in-panel file picker for document surfaces: browse folders and pick a
//! PDF, Office or image file, or type a path.

use crate::theme;
use iced::widget::{Column, button, column, container, row, scrollable};
use iced::{Center, Element};
use loa_view::theme::Icon;
use loa_ui_kit::style::{self, ButtonKind, Surface};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole, sp, text_size};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Up,
    Enter(PathBuf),
    Choose(PathBuf),
    PathChanged(String),
    PathSubmitted,
}

/// Browses one folder at a time, listing subfolders and the files the
/// document viewer opens.
#[derive(Clone, Debug)]
pub struct FilePicker {
    dir: PathBuf,
    path: String,
    entries: Vec<Entry>,
    error: Option<String>,
    /// Extensions of the files the document viewer opens.
    extensions: Vec<String>,
}

impl FilePicker {
    /// Starts in `dir`, listing files with one of `extensions` (case insensitive).
    pub fn new(dir: impl Into<PathBuf>, extensions: Vec<String>) -> Self {
        let mut picker = Self { dir: PathBuf::new(), path: String::new(), entries: Vec::new(), error: None, extensions };
        picker.enter(dir.into());
        picker
    }

    /// Whether the document viewer opens `path`, by its extension.
    pub fn supported(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()).is_some_and(|ext| self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    fn enter(&mut self, dir: PathBuf) {
        let listed = read(&dir, |path| self.supported(path));
        match listed {
            Ok(entries) => {
                self.path = dir.display().to_string();
                self.dir = dir;
                self.entries = entries;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{}: {e}", dir.display())),
        }
    }

    /// Returns the file the user picked.
    pub fn update(&mut self, message: Message) -> Option<PathBuf> {
        match message {
            Message::Up => {
                if let Some(parent) = self.dir.parent() {
                    self.enter(parent.to_owned());
                }
            }
            Message::Enter(dir) => self.enter(dir),
            Message::Choose(file) => return Some(file),
            Message::PathChanged(path) => self.path = path,
            Message::PathSubmitted => {
                let path = expand_home(self.path.trim());
                if path.is_dir() {
                    self.enter(path);
                } else if path.is_file() && self.supported(&path) {
                    return Some(path);
                } else if path.is_file() {
                    self.error = Some(format!("{} isn't a PDF, Office or image file", path.display()));
                } else {
                    self.error = Some(format!("{} doesn't exist", path.display()));
                }
            }
        }
        None
    }

    pub fn view(&self) -> Element<'_, Message> {
        let up = button(theme::icon(Icon::ChevronUp)).on_press_maybe(self.dir.parent().map(|_| Message::Up));
        let up = up.padding(sp(Sp::Xs)).style(style::button(ButtonKind::Row { selected: false }));
        let path = theme::field("Folder or file path", &self.path, Message::PathChanged).on_submit(Message::PathSubmitted);
        let mut list = Column::new();
        if self.entries.is_empty() {
            list = list.push(container(theme::hint("No folders or documents here")).padding(theme::field_padding()));
        }
        for entry in &self.entries {
            let (icon, message) = if entry.is_dir { (Icon::Folder, Message::Enter(entry.path.clone())) } else { (Icon::FileText, Message::Choose(entry.path.clone())) };
            list = list.push(theme::list_row(row![theme::icon(icon), theme::body(&entry.name)].spacing(sp(Sp::S)).align_y(Center), message));
        }
        // About seven rows of entries show before the list scrolls.
        let height = 7.0 * (text_size(TextRole::Label) + 3.0 * sp(Sp::Xs));
        let list = container(scrollable(list).height(height)).padding(sp(Sp::Xs) / 2.0).style(style::container(Surface::Card));
        let body = column![row![up, path].spacing(sp(Sp::Xs)).align_y(Center), list].spacing(sp(Sp::Xs));
        body.push(self.error.as_deref().map(|e| Element::from(theme::warning(e)))).into()
    }
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map_or_else(|| PathBuf::from(path), |home| PathBuf::from(home).join(rest)),
        None => PathBuf::from(path),
    }
}

/// Visible subfolders, then files `supported` takes, each sorted by name.
fn read(dir: &Path, supported: impl Fn(&Path) -> bool) -> std::io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let path = e.path();
            let is_dir = path.is_dir();
            (is_dir || supported(&path)).then_some(Entry { name, path, is_dir })
        })
        .collect();
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_folders_then_documents_and_picks_a_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("reports")).unwrap();
        std::fs::create_dir(dir.path().join(".hidden")).unwrap();
        std::fs::write(dir.path().join("b.PDF"), b"%PDF-").unwrap();
        std::fs::write(dir.path().join("a.xlsx"), b"").unwrap();
        std::fs::write(dir.path().join("notes.txt"), b"").unwrap();
        std::fs::write(dir.path().join("reports/q1.docx"), b"").unwrap();
        let extensions = ["pdf", "xlsx", "xlsm", "docx", "pptx"].map(String::from).to_vec();
        let mut picker = FilePicker::new(dir.path(), extensions);
        let names: Vec<&str> = picker.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["reports", "a.xlsx", "b.PDF"]);

        picker.update(Message::Enter(dir.path().join("reports")));
        assert_eq!(picker.dir(), dir.path().join("reports"));
        assert_eq!(picker.update(Message::Choose(dir.path().join("reports/q1.docx"))), Some(dir.path().join("reports/q1.docx")));
        picker.update(Message::Up);
        assert_eq!(picker.dir(), dir.path());

        picker.update(Message::PathChanged(dir.path().join("notes.txt").display().to_string()));
        assert_eq!(picker.update(Message::PathSubmitted), None);
        assert!(picker.error().is_some_and(|e| e.contains("isn't a PDF")));
        picker.update(Message::PathChanged(dir.path().join("b.PDF").display().to_string()));
        assert_eq!(picker.update(Message::PathSubmitted), Some(dir.path().join("b.PDF")));
    }
}
