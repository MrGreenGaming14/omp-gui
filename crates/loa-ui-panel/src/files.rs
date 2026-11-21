//! The files tab: a lazy tree listed through the [`FileSystem`] port on first expansion, nodes in a flat arena.

use crate::{indent, list_row_height};
use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Fill, Task};
use loa_ui_kit::icons::{icon, muted};
use loa_view::panel::FilesIntent;
use loa_view::panel::port::{Entry, FileSystem};
use loa_view::theme::Icon;
use loa_ui_kit::style::{self as kit, ButtonKind, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_x, sp, text_size as ts};
use loa_ui_kit::widgets::label;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

/// The local disk, honouring `.gitignore`, `.ignore` and git excludes, showing dotfiles and hiding `.git`; errors read as the walker words them.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFs;

impl FileSystem for LocalFs {
    fn list(&self, dir: &Path) -> Result<Vec<Entry>, String> {
        let walk = ignore::WalkBuilder::new(dir).max_depth(Some(1)).hidden(false).require_git(false).parents(true).filter_entry(|e| e.file_name() != ".git").build();
        let mut entries = Vec::new();
        for item in walk {
            let entry = match item {
                // A bad glob or one unreadable child leaves the rest listable.
                Err(e) if e.is_partial() || e.depth().is_some_and(|d| d > 0) => continue,
                Err(e) => return Err(e.to_string()),
                Ok(e) if e.depth() == 0 => continue,
                Ok(e) => e,
            };
            let is_dir = entry.file_type().is_some_and(|k| if k.is_symlink() { entry.path().is_dir() } else { k.is_dir() });
            entries.push(Entry { name: entry.file_name().to_string_lossy().into_owned(), is_dir });
        }
        Ok(entries)
    }
}

/// `Click` carries the clicked row's node index.
#[derive(Debug, Clone)]
pub enum Message { Click(usize) }
/// A file or directory, `rel` to the root; node 0 is the hidden root. Listed children are contiguous.
#[derive(Default)]
struct Node { depth: usize, name: String, rel: PathBuf, is_dir: bool, expanded: bool, children: Option<Range<usize>>, error: Option<String> }
fn root_node() -> Node { Node { is_dir: true, expanded: true, ..Node::default() } }
/// A visible line: a node, or the load error under a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row { Node(usize), Error(usize) }
pub struct State { root: PathBuf, fs: Box<dyn FileSystem>, nodes: Vec<Node>, rows: Vec<Row>, selected: Option<usize> }

impl State {
    /// The tree with the root's children listed.
    pub fn new(root: PathBuf, fs: Box<dyn FileSystem>) -> Self {
        let mut s = Self { root, fs, nodes: vec![root_node()], rows: Vec::new(), selected: None };
        s.load(0);
        s.rebuild_rows();
        s
    }
    pub fn root(&self) -> &Path { &self.root }
    /// Re-roots the tree, forgetting nodes and selection.
    pub fn set_root(&mut self, root: PathBuf) { *self = Self::new(root, std::mem::replace(&mut self.fs, Box::new(LocalFs))) }
    /// Root-relative paths of the visible rows, top to bottom.
    pub fn visible(&self) -> impl Iterator<Item = &Path> { self.rows.iter().filter_map(|r| if let Row::Node(i) = *r { Some(self.nodes[i].rel.as_path()) } else { None }) }
    /// Re-lists every loaded directory, keeping expansion and selection where paths remain.
    pub fn refresh(&mut self) {
        let old = std::mem::replace(&mut self.nodes, vec![root_node()]);
        let selected = self.selected.take().map(|i| old[i].rel.clone());
        let mut dirs: HashMap<PathBuf, bool> = old.into_iter().skip(1).filter(|n| n.is_dir && (n.expanded || n.children.is_some())).map(|n| (n.rel, n.expanded)).collect();
        self.load(0);
        let mut i = 1;
        while i < self.nodes.len() {
            if let Some(expanded) = self.nodes[i].is_dir.then(|| dirs.remove(&self.nodes[i].rel)).flatten() {
                self.nodes[i].expanded = expanded;
                self.load(i);
            }
            if selected.as_deref() == Some(self.nodes[i].rel.as_path()) { self.selected = Some(i) }
            i += 1;
        }
        self.rebuild_rows();
    }
    /// A click selects a row, toggles a directory and opens a file.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<FilesIntent>) {
        let Message::Click(i) = message;
        if i == 0 || !self.rows.contains(&Row::Node(i)) { return (Task::none(), None) }
        self.selected = Some(i);
        let node = &self.nodes[i];
        if !node.is_dir { return (Task::none(), Some(FilesIntent::Open(self.root.join(&node.rel)))) }
        if !node.expanded && node.children.is_none() { self.load(i) }
        self.nodes[i].expanded = !self.nodes[i].expanded;
        self.rebuild_rows();
        (Task::none(), None)
    }
    pub fn view(&self) -> Element<'_, Message> { scrollable(self.list()).width(Fill).height(Fill).into() }
    fn list(&self) -> iced::widget::Column<'_, Message> {
        let rows = self.rows.iter().map(|r| match *r {
            Row::Node(i) => self.node_row(i),
            Row::Error(i) => {
                let n = &self.nodes[i];
                let gap = Space::new().width((n.depth + 1) as f32 * indent() + sp(Sp::Xl) + sp(Sp::S));
                let line = row![gap, label(n.error.as_deref().unwrap_or_default(), T::Body, Tone::Danger).wrapping(text::Wrapping::None)];
                container(line.align_y(Alignment::Center).height(Fill)).width(Fill).height(list_row_height()).padding(pad_x(Sp::S)).into()
            }
        });
        column(rows).width(Fill).padding(pad_x(Sp::S))
    }
    fn node_row(&self, i: usize) -> Element<'_, Message> {
        let n = &self.nodes[i];
        let chevron: Element<'_, Message> = match (n.is_dir, n.expanded) {
            (false, _) => Space::new().width(indent()).into(),
            (true, open) => muted(if open { Icon::ChevronDown } else { Icon::ChevronRight }, ts(T::Caption)).width(indent()).into(),
        };
        let name = text(n.name.as_str()).size(ts(T::Body)).wrapping(text::Wrapping::None).width(Fill);
        let content = row![Space::new().width((n.depth - 1) as f32 * indent()), chevron, icon(if n.is_dir { Icon::Folder } else { Icon::FileText }, ts(T::Body)), name];
        let style = kit::button(ButtonKind::Row { selected: self.selected == Some(i) });
        button(content.spacing(sp(Sp::S)).align_y(Alignment::Center).height(Fill)).width(Fill).height(list_row_height()).padding(pad_x(Sp::S)).style(style).on_press(Message::Click(i)).into()
    }
    /// Lists a directory node and appends its children, directories first, by name ignoring case.
    fn load(&mut self, dir: usize) {
        let rel = self.nodes[dir].rel.clone();
        match self.fs.list(&self.root.join(&rel)) {
            Ok(mut entries) => {
                entries.sort_unstable_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name)));
                let (first, depth) = (self.nodes.len(), self.nodes[dir].depth + 1);
                self.nodes.extend(entries.into_iter().map(|Entry { name, is_dir }| Node { depth, rel: rel.join(&name), name, is_dir, ..Node::default() }));
                (self.nodes[dir].children, self.nodes[dir].error) = (Some(first..self.nodes.len()), None);
            }
            Err(e) => (self.nodes[dir].children, self.nodes[dir].error) = (None, Some(e)),
        }
    }
    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let mut stack = vec![0];
        while let Some(i) = stack.pop() {
            let n = &self.nodes[i];
            if i != 0 { self.rows.push(Row::Node(i)) }
            if n.is_dir && n.expanded {
                if n.error.is_some() { self.rows.push(Row::Error(i)) }
                stack.extend(n.children.clone().into_iter().flatten().rev());
            }
        }
        if self.selected.is_some_and(|s| !self.rows.contains(&Row::Node(s))) { self.selected = None }
    }
}

panel_pane!("files", "Files", Folder);

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Listings by directory, with how often each was listed.
    type Dirs = HashMap<PathBuf, (Vec<Entry>, usize)>;

    /// Directories by path, counting how often each is listed; clones share them.
    #[derive(Clone, Default)]
    pub(crate) struct FakeFs(Arc<Mutex<Dirs>>);

    /// An absolute path for the platform: `/r` is `C:/r` on Windows, where a path with no drive
    /// is relative.
    pub(crate) fn abs(path: &str) -> PathBuf { if cfg!(windows) { PathBuf::from(format!("C:{path}")) } else { PathBuf::from(path) } }

    impl FakeFs {
        pub(crate) fn put(&self, dir: &str, entries: &[(&str, bool)]) { self.0.lock().unwrap().entry(abs(dir)).or_default().0 = entries.iter().map(|&(n, d)| Entry { name: n.into(), is_dir: d }).collect() }
        fn calls(&self, dir: &str) -> usize { self.0.lock().unwrap().get(&abs(dir)).map_or(0, |d| d.1) }
    }

    impl FileSystem for FakeFs {
        fn list(&self, dir: &Path) -> Result<Vec<Entry>, String> {
            let mut dirs = self.0.lock().unwrap();
            let d = dirs.get_mut(dir).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound).to_string())?;
            d.1 += 1;
            Ok(d.0.clone())
        }
    }
    #[test]
    fn directories_list_lazily_sort_first_and_open_files() {
        let fs = FakeFs::default();
        for (dir, entries) in [("/r", &[("b.txt", false), ("src", true), ("a", true), ("Z.md", false)][..]), ("/r/src", &[("main.rs", false), ("lib", true)]), ("/r/a", &[])] { fs.put(dir, entries) }
        let mut s = State::new(abs("/r"), Box::new(fs.clone()));
        let click = |s: &mut State, rel: &str| s.update(Message::Click(s.nodes.iter().position(|n| n.rel == Path::new(rel)).unwrap())).1;
        // Relative paths shown with `/` so expectations read the same on every platform.
        fn visible(s: &State) -> Vec<String> { s.visible().map(|p| p.to_str().unwrap().replace(std::path::MAIN_SEPARATOR, "/")).collect() }
        assert_eq!((visible(&s), fs.calls("/r"), fs.calls("/r/src")), (["a", "src", "b.txt", "Z.md"].map(String::from).to_vec(), 1, 0));
        let steps: [(&str, Option<FilesIntent>, &[&str], usize); 4] = [
            ("src", None, &["a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"], 1),
            ("src", None, &["a", "src", "b.txt", "Z.md"], 1),
            ("src", None, &["a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"], 1),
            ("src/main.rs", Some(FilesIntent::Open(abs("/r/src/main.rs"))), &["a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"], 1),
        ];
        for (rel, event, want, calls) in steps { assert_eq!((click(&mut s, rel), visible(&s), fs.calls("/r/src")), (event, want.iter().map(|w| w.to_string()).collect::<Vec<_>>(), calls), "{rel}") }
        fs.put("/r/src", &[("main.rs", false), ("new.rs", false)]);
        s.refresh();
        assert_eq!((visible(&s).len(), s.selected.map(|i| s.nodes[i].rel.clone())), (6, Some("src/main.rs".into())), "the selection and expansion survive a refresh");
    }
}
