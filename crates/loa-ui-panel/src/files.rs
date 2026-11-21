//! The files tab: a lazy tree over a root directory. Directories are listed through the
//! [`FileSystem`] port when first expanded; nodes live in a flat arena and the visible rows
//! are rebuilt only when expansion or a listing changes.

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Fill, Padding, Task};
use loa_ui_kit::icons::{Icon, icon, muted};
use loa_ui_kit::style::{self as kit, ButtonKind, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, sp, text_size as ts};
use loa_ui_kit::widgets::{PanelPane, label};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

/// Tallest the sidebar's compact list grows before it scrolls.
pub const COMPACT_MAX_HEIGHT: f32 = 240.0;

/// A listing failure; io errors convert into it.
pub type Error = ignore::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// Port the tree reads directories through.
pub trait FileSystem: Send + Sync {
    /// The direct children of `dir`, already filtered; the tree sorts them.
    fn list(&self, dir: &Path) -> Result<Vec<Entry>>;
}

/// The local disk, honouring `.gitignore`, `.ignore` and git excludes, showing dotfiles and
/// hiding `.git`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFs;

impl FileSystem for LocalFs {
    fn list(&self, dir: &Path) -> Result<Vec<Entry>> {
        let walk = ignore::WalkBuilder::new(dir).max_depth(Some(1)).hidden(false).require_git(false).parents(true).filter_entry(|e| e.file_name() != ".git").build();
        let mut entries = Vec::new();
        for item in walk {
            let entry = match item {
                // A bad glob or one unreadable child leaves the rest listable.
                Err(e) if e.is_partial() || e.depth().is_some_and(|d| d > 0) => continue,
                Err(e) => return Err(e),
                Ok(e) if e.depth() == 0 => continue,
                Ok(e) => e,
            };
            let is_dir = entry.file_type().is_some_and(|k| if k.is_symlink() { entry.path().is_dir() } else { k.is_dir() });
            entries.push(Entry { name: entry.file_name().to_string_lossy().into_owned(), is_dir });
        }
        Ok(entries)
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    /// A row was clicked: the node index.
    Click(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Open this file (absolute path).
    Open(PathBuf),
}

/// How a node relates to the files the session touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    None,
    /// A touched path lies under this directory.
    Contains,
    Touched,
}

/// A file or directory; node 0 is the hidden root.
struct Node {
    depth: usize,
    name: String,
    /// Relative to the root.
    rel: PathBuf,
    is_dir: bool,
    expanded: bool,
    /// Children are pushed contiguously when listed.
    children: Option<Range<usize>>,
    error: Option<String>,
    mark: Mark,
}

impl Node {
    fn root() -> Self { Self { depth: 0, name: String::new(), rel: PathBuf::new(), is_dir: true, expanded: true, children: None, error: None, mark: Mark::None } }
}

/// A visible line: a node, or the load error under a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Node(usize),
    Error(usize),
}

pub struct State {
    root: PathBuf,
    fs: Box<dyn FileSystem>,
    nodes: Vec<Node>,
    rows: Vec<Row>,
    selected: Option<usize>,
    touched: HashSet<PathBuf>,
    /// Every proper ancestor of a touched path.
    touched_dirs: HashSet<PathBuf>,
}

impl State {
    /// The tree with the root's children listed.
    pub fn new(root: PathBuf, fs: Box<dyn FileSystem>) -> Self {
        let mut s = Self { root, fs, nodes: vec![Node::root()], rows: Vec::new(), selected: None, touched: HashSet::new(), touched_dirs: HashSet::new() };
        s.load(0);
        s.rebuild_rows();
        s
    }

    pub fn root(&self) -> &Path { &self.root }
    /// Re-roots the tree, forgetting nodes, selection and touched files.
    pub fn set_root(&mut self, root: PathBuf) { *self = Self::new(root, std::mem::replace(&mut self.fs, Box::new(LocalFs))) }
    /// Root-relative paths of the visible rows, top to bottom.
    pub fn visible(&self) -> impl Iterator<Item = &Path> { self.rows.iter().filter_map(|r| if let Row::Node(i) = *r { Some(self.nodes[i].rel.as_path()) } else { None }) }
    pub fn selected_path(&self) -> Option<&Path> { self.selected.map(|i| self.nodes[i].rel.as_path()) }

    /// Replaces the files the session touched; absolute paths outside the root are ignored.
    pub fn set_touched(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.touched.clear();
        self.touched_dirs.clear();
        for path in paths {
            let Some(rel) = (if path.is_absolute() { path.strip_prefix(&self.root).ok().map(Path::to_path_buf) } else { Some(path) }) else { continue };
            // Ancestors of a recorded dir are recorded too.
            for a in rel.ancestors().skip(1) { if !self.touched_dirs.insert(a.to_path_buf()) { break } }
            self.touched.insert(rel);
        }
        let marks: Vec<_> = self.nodes.iter().map(|n| self.mark(&n.rel, n.is_dir)).collect();
        self.nodes.iter_mut().zip(marks).for_each(|(n, m)| n.mark = m);
    }

    fn mark(&self, rel: &Path, is_dir: bool) -> Mark {
        if self.touched.contains(rel) { Mark::Touched } else if is_dir && self.touched_dirs.contains(rel) { Mark::Contains } else { Mark::None }
    }

    /// Re-lists every loaded directory, keeping expansion and selection where paths remain.
    pub fn refresh(&mut self) {
        let old = std::mem::replace(&mut self.nodes, vec![Node::root()]);
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
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let Message::Click(i) = message;
        if i == 0 || !self.rows.contains(&Row::Node(i)) { return (Task::none(), None) }
        self.selected = Some(i);
        let node = &self.nodes[i];
        if !node.is_dir { return (Task::none(), Some(Event::Open(self.root.join(&node.rel)))) }
        if !node.expanded && node.children.is_none() { self.load(i) }
        self.nodes[i].expanded = !self.nodes[i].expanded;
        self.rebuild_rows();
        (Task::none(), None)
    }

    pub fn view(&self) -> Element<'_, Message> { scrollable(self.list()).width(Fill).height(Fill).into() }
    /// The same rows sized to their content up to [`COMPACT_MAX_HEIGHT`], for the sidebar.
    pub fn view_compact(&self) -> Element<'_, Message> { container(scrollable(self.list()).width(Fill)).max_height(COMPACT_MAX_HEIGHT).width(Fill).into() }

    fn list(&self) -> iced::widget::Column<'_, Message> {
        let rows = self.rows.iter().map(|r| match *r {
            Row::Node(i) => self.node_row(i),
            Row::Error(i) => {
                let n = &self.nodes[i];
                let gap = Space::new().width((n.depth + 1) as f32 * crate::indent() + sp(Sp::Xl) + sp(Sp::S));
                let line = row![gap, label(n.error.as_deref().unwrap_or_default(), T::Body, Tone::Danger).wrapping(text::Wrapping::None)];
                container(line.align_y(Alignment::Center).height(Fill)).width(Fill).height(crate::list_row_height()).padding(pad()).into()
            }
        });
        column(rows).width(Fill).padding(pad())
    }

    fn node_row(&self, i: usize) -> Element<'_, Message> {
        let n = &self.nodes[i];
        let small = |i| muted(i, ts(T::Caption)).width(crate::indent()).into();
        let chevron: Element<'_, Message> = match (n.is_dir, n.expanded) {
            (false, _) => Space::new().width(crate::indent()).into(),
            (true, false) => small(Icon::ChevronRight),
            (true, true) => small(Icon::ChevronDown),
        };
        let marker: Element<'_, Message> = match n.mark {
            Mark::None => Space::new().into(),
            Mark::Contains => muted(Icon::CircleDot, ts(T::Caption)).into(),
            Mark::Touched => icon(Icon::CircleDot, ts(T::Caption)).into(),
        };
        let name = text(n.name.as_str()).size(ts(T::Body)).wrapping(text::Wrapping::None).width(Fill);
        let content = row![Space::new().width((n.depth - 1) as f32 * crate::indent()), chevron, icon(if n.is_dir { Icon::Folder } else { Icon::FileText }, ts(T::Body)), name, marker];
        let style = kit::button(ButtonKind::Row { selected: self.selected == Some(i) });
        button(content.spacing(sp(Sp::S)).align_y(Alignment::Center).height(Fill)).width(Fill).height(crate::list_row_height()).padding(pad()).style(style).on_press(Message::Click(i)).into()
    }

    /// Lists a directory node and appends its children, directories first, by name ignoring case.
    fn load(&mut self, dir: usize) {
        let rel = self.nodes[dir].rel.clone();
        match self.fs.list(&if rel.as_os_str().is_empty() { self.root.clone() } else { self.root.join(&rel) }) {
            Ok(mut entries) => {
                entries.sort_unstable_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.name.cmp(&b.name)));
                let (first, depth) = (self.nodes.len(), self.nodes[dir].depth + 1);
                for Entry { name, is_dir } in entries {
                    let rel = rel.join(&name);
                    let mark = self.mark(&rel, is_dir);
                    self.nodes.push(Node { depth, name, rel, is_dir, expanded: false, children: None, error: None, mark });
                }
                (self.nodes[dir].children, self.nodes[dir].error) = (Some(first..self.nodes.len()), None);
            }
            Err(e) => (self.nodes[dir].children, self.nodes[dir].error) = (None, Some(e.to_string())),
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

impl PanelPane for State {
    type Message = Message;
    fn id(&self) -> &str { "files" }
    fn title(&self) -> &str { "Files" }
    fn icon(&self) -> Icon { Icon::Folder }
    fn view(&self) -> Element<'_, Message> { State::view(self) }
}

fn pad() -> Padding { Padding::ZERO.left(sp(Sp::S)).right(sp(Sp::S)) }

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeFs {
        tree: Mutex<HashMap<PathBuf, Vec<Entry>>>,
        calls: Mutex<HashMap<PathBuf, usize>>,
    }

    impl FakeFs {
        fn put(&self, dir: &str, entries: &[(&str, bool)]) { self.tree.lock().unwrap().insert(dir.into(), entries.iter().map(|&(n, d)| Entry { name: n.into(), is_dir: d }).collect()); }
        fn calls(&self, dir: &str) -> usize { self.calls.lock().unwrap().get(Path::new(dir)).copied().unwrap_or(0) }
    }

    impl FileSystem for Arc<FakeFs> {
        fn list(&self, dir: &Path) -> Result<Vec<Entry>> {
            *self.calls.lock().unwrap().entry(dir.to_path_buf()).or_default() += 1;
            self.tree.lock().unwrap().get(dir).cloned().ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound).into())
        }
    }

    fn fixture() -> (State, Arc<FakeFs>) {
        let fs = Arc::new(FakeFs::default());
        fs.put("/r", &[("b.txt", false), ("src", true), ("a", true), ("Z.md", false)]);
        fs.put("/r/src", &[("main.rs", false), ("lib", true)]);
        fs.put("/r/src/lib", &[("x.rs", false)]);
        fs.put("/r/a", &[]);
        (State::new("/r".into(), Box::new(Arc::clone(&fs))), fs)
    }

    fn visible(s: &State) -> Vec<&str> { s.visible().map(|p| p.to_str().unwrap()).collect() }

    fn click(s: &mut State, rel: &str) -> Option<Event> {
        let i = s.nodes.iter().position(|n| n.rel == Path::new(rel)).unwrap();
        s.update(Message::Click(i)).1
    }

    fn mark(s: &State, rel: &str) -> Mark { s.nodes.iter().find(|n| n.rel == Path::new(rel)).unwrap().mark }

    #[test]
    fn directories_list_lazily_sort_first_and_open_files() {
        let (mut s, fs) = fixture();
        assert_eq!((visible(&s), fs.calls("/r"), fs.calls("/r/src")), (vec!["a", "src", "b.txt", "Z.md"], 1, 0));
        assert_eq!(click(&mut s, "src"), None);
        assert_eq!((visible(&s), fs.calls("/r/src"), fs.calls("/r/src/lib")), (vec!["a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"], 1, 0));
        click(&mut s, "src");
        click(&mut s, "src");
        assert_eq!((visible(&s).len(), fs.calls("/r/src")), (6, 1), "a re-expanded directory isn't listed again");
        assert_eq!(click(&mut s, "src/main.rs"), Some(Event::Open("/r/src/main.rs".into())));
        assert_eq!(s.selected_path(), Some(Path::new("src/main.rs")));
        fs.put("/r/src", &[("main.rs", false), ("lib", true), ("new.rs", false)]);
        s.refresh();
        assert_eq!((visible(&s), s.selected_path()), (vec!["a", "src", "src/lib", "src/main.rs", "src/new.rs", "b.txt", "Z.md"], Some(Path::new("src/main.rs"))));
        fs.put("/r/src", &[("lib", true)]);
        s.refresh();
        assert_eq!((visible(&s).len(), s.selected_path()), (5, None));
        fs.put("/q", &[("only.txt", false)]);
        s.set_root("/q".into());
        assert_eq!((visible(&s), s.selected_path()), (vec!["only.txt"], None));
        let _ = s.view_compact();
    }

    #[test]
    fn touched_marks_files_and_their_ancestors() {
        let (mut s, _) = fixture();
        s.set_touched(["/r/src/lib/x.rs".into(), "b.txt".into(), "/elsewhere/a".into()]);
        click(&mut s, "src");
        click(&mut s, "src/lib");
        let marks = [("b.txt", Mark::Touched), ("src", Mark::Contains), ("src/lib", Mark::Contains), ("src/lib/x.rs", Mark::Touched), ("a", Mark::None), ("src/main.rs", Mark::None)];
        for (rel, m) in marks { assert_eq!(mark(&s, rel), m, "{rel}") }
        s.set_touched(["src/main.rs".into()]);
        assert_eq!([mark(&s, "src/lib"), mark(&s, "b.txt"), mark(&s, "src")], [Mark::None, Mark::None, Mark::Contains]);
    }

    #[test]
    fn local_fs_honors_gitignore_and_hides_dot_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for d in ["build", ".git", "sub"] { std::fs::create_dir_all(root.join(d)).unwrap() }
        for (f, body) in [(".gitignore", "ignored.txt\nbuild/\n"), ("ignored.txt", ""), ("keep.txt", ""), (".dotfile", ""), ("build/out", ""), (".git/HEAD", ""), ("sub/ignored.txt", ""), ("sub/kept.rs", "")] {
            std::fs::write(root.join(f), body).unwrap();
        }
        let mut top: Vec<_> = LocalFs.list(root).unwrap().into_iter().map(|e| (e.name, e.is_dir)).collect();
        top.sort();
        assert_eq!(top, [(".dotfile".into(), false), (".gitignore".into(), false), ("keep.txt".into(), false), ("sub".into(), true)]);
        assert_eq!(LocalFs.list(&root.join("sub")).unwrap(), [Entry { name: "kept.rs".into(), is_dir: false }]);
    }
}
