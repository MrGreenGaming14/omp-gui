//! Lazy file tree over a root directory.
//!
//! Directories are listed through the [`FileSystem`] port only when first
//! expanded. Nodes live in a flat arena; the visible rows are cached and
//! rebuilt only when expansion or the listing changes.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};

use iced::keyboard::Key;
use iced::keyboard::key::Named;
use iced::widget::operation::{self, AbsoluteOffset};
use iced::widget::scrollable::Viewport;
use iced::widget::{Id, Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length, Task};
use loa_ui_kit::icons::{Icon, icon, muted};

use loa_ui_kit::style::{self as kit, ButtonKind, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, sp, text_size as ts};

/// Tallest the sidebar's compact file list grows before it scrolls.
pub const COMPACT_MAX_HEIGHT: f32 = 240.0;

/// Failure to list a directory.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Ignore(#[from] ignore::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// One child of a listed directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
}

/// Port the tree reads directories through.
pub trait FileSystem: Send + Sync {
    /// Lists the direct children of `dir`, already filtered (ignore rules,
    /// `.git`). Order does not matter; the tree sorts.
    fn list(&self, dir: &Path) -> Result<Vec<Entry>>;
}

/// [`FileSystem`] on the local disk honoring `.gitignore`, `.ignore` and the
/// git exclude files, showing dotfiles and always hiding `.git`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFs;

impl FileSystem for LocalFs {
    fn list(&self, dir: &Path) -> Result<Vec<Entry>> {
        let walk = ignore::WalkBuilder::new(dir)
            .max_depth(Some(1))
            .hidden(false)
            .require_git(false)
            .parents(true)
            .filter_entry(|entry| entry.file_name() != ".git")
            .build();
        let mut entries = Vec::new();
        for item in walk {
            let entry = match item {
                Ok(entry) => entry,
                // A bad glob in an ignore file or one unreadable child does
                // not make the rest of the directory unlistable.
                Err(err) if err.is_partial() || err.depth().is_some_and(|depth| depth > 0) => continue,
                Err(err) => return Err(err.into()),
            };
            if entry.depth() == 0 {
                continue;
            }
            let is_dir = match entry.file_type() {
                Some(kind) if kind.is_symlink() => entry.path().is_dir(),
                Some(kind) => kind.is_dir(),
                None => false,
            };
            entries.push(Entry { name: entry.file_name().to_string_lossy().into_owned(), is_dir });
        }
        Ok(entries)
    }
}

/// Keyboard navigation commands, routed by the app while the tree has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    Up,
    Down,
    Left,
    Right,
    Activate,
    Home,
    End,
}

/// Maps a key to a tree navigation command: arrows, Enter, Home, End.
pub fn nav_for_key(key: &Key) -> Option<Nav> {
    match key {
        Key::Named(Named::ArrowUp) => Some(Nav::Up),
        Key::Named(Named::ArrowDown) => Some(Nav::Down),
        Key::Named(Named::ArrowLeft) => Some(Nav::Left),
        Key::Named(Named::ArrowRight) => Some(Nav::Right),
        Key::Named(Named::Enter) => Some(Nav::Activate),
        Key::Named(Named::Home) => Some(Nav::Home),
        Key::Named(Named::End) => Some(Nav::End),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Nav(Nav),
    /// A row was clicked; carries the node index.
    Click(usize),
    Scrolled(Viewport),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Open this file (absolute path).
    Open(PathBuf),
}

/// Session touch marker of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    None,
    /// A touched path lies under this directory.
    Contains,
    Touched,
}

/// One file or directory. Index 0 is the root, which is never shown.
struct Node {
    /// Parent node index; the root points at itself.
    parent: usize,
    /// Root is 0, its children 1.
    depth: usize,
    name: String,
    /// Path relative to the root; empty for the root.
    rel: PathBuf,
    is_dir: bool,
    expanded: bool,
    /// Children are pushed contiguously when the directory is listed.
    children: Option<Range<usize>>,
    error: Option<String>,
    mark: Mark,
}

impl Node {
    fn root() -> Self {
        Self {
            parent: 0,
            depth: 0,
            name: String::new(),
            rel: PathBuf::new(),
            is_dir: true,
            expanded: true,
            children: None,
            error: None,
            mark: Mark::None,
        }
    }
}

/// One visible line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Node(usize),
    /// Load error line under this directory node.
    Error(usize),
}

/// File tree tab.
pub struct State {
    root: PathBuf,
    fs: Box<dyn FileSystem>,
    nodes: Vec<Node>,
    rows: Vec<Row>,
    /// Selected node index; always a visible node.
    selected: Option<usize>,
    /// Position of the selected node in `rows`.
    selected_row: Option<usize>,
    /// Root-relative touched paths.
    touched: HashSet<PathBuf>,
    /// Every proper ancestor of a touched path.
    touched_dirs: HashSet<PathBuf>,
    scroll_id: Id,
    scroll_offset: f32,
    viewport_height: f32,
}

impl State {
    /// Creates the tree and lists the root's children.
    pub fn new(root: PathBuf, fs: Box<dyn FileSystem>) -> Self {
        let mut state = Self {
            root,
            fs,
            nodes: vec![Node::root()],
            rows: Vec::new(),
            selected: None,
            selected_row: None,
            touched: HashSet::new(),
            touched_dirs: HashSet::new(),
            scroll_id: Id::unique(),
            scroll_offset: 0.0,
            viewport_height: 0.0,
        };
        state.load(0);
        state.rebuild_rows();
        state
    }

    /// Root directory of the tree.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Roots the tree at `root`: forgets every loaded node, the selection
    /// and the touched files, and lists the new root's children.
    pub fn set_root(&mut self, root: PathBuf) {
        self.root = root;
        self.nodes = vec![Node::root()];
        self.selected = None;
        self.selected_row = None;
        self.touched.clear();
        self.touched_dirs.clear();
        self.scroll_offset = 0.0;
        self.load(0);
        self.rebuild_rows();
    }

    /// Root-relative paths of the visible rows, top to bottom.
    pub fn visible(&self) -> impl Iterator<Item = &Path> {
        self.rows.iter().filter_map(|row| match *row {
            Row::Node(index) => Some(self.nodes[index].rel.as_path()),
            Row::Error(_) => None,
        })
    }

    /// Root-relative path of the selected row.
    pub fn selected_path(&self) -> Option<&Path> {
        self.selected.map(|index| self.nodes[index].rel.as_path())
    }

    /// Replaces the set of files the session touched. Absolute paths outside
    /// the root are ignored.
    pub fn set_touched(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.touched.clear();
        self.touched_dirs.clear();
        for path in paths {
            let rel = if path.is_absolute() {
                match path.strip_prefix(&self.root) {
                    Ok(rel) => rel.to_path_buf(),
                    Err(_) => continue,
                }
            } else {
                path
            };
            for ancestor in rel.ancestors().skip(1) {
                // Ancestors of a recorded dir are recorded too.
                if self.touched_dirs.contains(ancestor) {
                    break;
                }
                self.touched_dirs.insert(ancestor.to_path_buf());
            }
            self.touched.insert(rel);
        }
        for node in &mut self.nodes {
            node.mark = mark_for(&self.touched, &self.touched_dirs, &node.rel, node.is_dir);
        }
    }

    /// Re-lists every loaded directory, keeping expansion and the selection
    /// where the paths still exist.
    pub fn refresh(&mut self) {
        let old = std::mem::take(&mut self.nodes);
        let mut dirs: HashMap<PathBuf, bool> = HashMap::new();
        let mut selected_path = None;
        for (index, node) in old.into_iter().enumerate().skip(1) {
            if Some(index) == self.selected {
                selected_path = Some(node.rel.clone());
            }
            if node.is_dir && (node.expanded || node.children.is_some()) {
                dirs.insert(node.rel, node.expanded);
            }
        }
        self.nodes.push(Node::root());
        self.load(0);
        self.selected = None;
        let mut index = 1;
        while index < self.nodes.len() {
            if self.nodes[index].is_dir
                && let Some(expanded) = dirs.remove(&self.nodes[index].rel)
            {
                self.nodes[index].expanded = expanded;
                self.load(index);
            }
            if selected_path.as_deref() == Some(self.nodes[index].rel.as_path()) {
                self.selected = Some(index);
            }
            index += 1;
        }
        self.rebuild_rows();
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Nav(nav) => {
                let event = self.navigate(nav);
                (self.reveal_selected(), event)
            }
            Message::Click(index) => {
                if index == 0 || index >= self.nodes.len() || !self.select_node(index) {
                    return (Task::none(), None);
                }
                (Task::none(), self.activate(index))
            }
            Message::Scrolled(viewport) => {
                self.scroll_offset = viewport.absolute_offset().y;
                self.viewport_height = viewport.bounds().height;
                (Task::none(), None)
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        scrollable(column(self.row_elements()).width(Length::Fill).padding(iced::Padding::ZERO.left(sp(Sp::S)).right(sp(Sp::S))))
            .id(self.scroll_id.clone())
            .on_scroll(Message::Scrolled)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// The same rows and messages as [`State::view`], sized to its content up
    /// to [`COMPACT_MAX_HEIGHT`], for embedding in a sidebar. Its scrollable
    /// has no id and does not report scrolling, so it never disturbs the
    /// keyboard reveal of the Files tab.
    pub fn view_compact(&self) -> Element<'_, Message> {
        let rows = column(self.row_elements()).width(Length::Fill).padding(iced::Padding::ZERO.left(sp(Sp::S)).right(sp(Sp::S)));
        container(scrollable(rows).width(Length::Fill)).max_height(COMPACT_MAX_HEIGHT).width(Length::Fill).into()
    }

    fn row_elements(&self) -> impl Iterator<Item = Element<'_, Message>> {
        self.rows.iter().map(|row| match *row {
            Row::Node(index) => self.node_row(index),
            Row::Error(index) => self.error_row(index),
        })
    }

    fn node_row(&self, index: usize) -> Element<'_, Message> {
        let node = &self.nodes[index];
        let chevron: Element<'_, Message> = match (node.is_dir, node.expanded) {
            (false, _) => Space::new().width(crate::indent()).into(),
            (true, false) => muted(Icon::ChevronRight, ts(T::Caption)).width(crate::indent()).into(),
            (true, true) => muted(Icon::ChevronDown, ts(T::Caption)).width(crate::indent()).into(),
        };
        let kind = if node.is_dir { Icon::Folder } else { Icon::FileText };
        let marker: Element<'_, Message> = match node.mark {
            Mark::None => Space::new().into(),
            Mark::Contains => muted(Icon::CircleDot, ts(T::Caption)).into(),
            Mark::Touched => icon(Icon::CircleDot, ts(T::Caption)).into(),
        };
        let content = row![
            Space::new().width((node.depth - 1) as f32 * crate::indent()),
            chevron,
            icon(kind, ts(T::Body)),
            text(node.name.as_str())
                .size(ts(T::Body))
                .wrapping(text::Wrapping::None)
                .width(Length::Fill),
            marker,
        ]
        .spacing(sp(Sp::S))
        .align_y(Alignment::Center)
        .height(Length::Fill);
        button(content)
            .width(Length::Fill)
            .height(crate::list_row_height())
            .padding(iced::Padding::ZERO.left(sp(Sp::S)).right(sp(Sp::S)))
            .style(kit::button(ButtonKind::Row { selected: self.selected == Some(index) }))
            .on_press(Message::Click(index))
            .into()
    }

    fn error_row(&self, index: usize) -> Element<'_, Message> {
        let node = &self.nodes[index];
        let message = node.error.as_deref().unwrap_or_default();
        let content = row![
            Space::new().width(node.depth as f32 * crate::indent() + crate::indent() + sp(Sp::Xl) + sp(Sp::S)),
            text(message)
                .size(ts(T::Body))
                .wrapping(text::Wrapping::None)
                .style(kit::text(Tone::Danger)),
        ]
        .align_y(Alignment::Center)
        .height(Length::Fill);
        container(content)
            .width(Length::Fill)
            .height(crate::list_row_height())
            .padding(iced::Padding::ZERO.left(sp(Sp::S)).right(sp(Sp::S)))
            .into()
    }

    /// Lists a directory node and appends its children to the arena.
    fn load(&mut self, dir: usize) {
        let rel = &self.nodes[dir].rel;
        let listed = if rel.as_os_str().is_empty() {
            self.fs.list(&self.root)
        } else {
            self.fs.list(&self.root.join(rel))
        };
        match listed {
            Ok(mut entries) => {
                entries.sort_unstable_by(compare_entries);
                let depth = self.nodes[dir].depth + 1;
                let first = self.nodes.len();
                self.nodes.reserve(entries.len());
                for Entry { name, is_dir } in entries {
                    let rel = self.nodes[dir].rel.join(&name);
                    let mark = mark_for(&self.touched, &self.touched_dirs, &rel, is_dir);
                    self.nodes.push(Node {
                        parent: dir,
                        depth,
                        name,
                        rel,
                        is_dir,
                        expanded: false,
                        children: None,
                        error: None,
                        mark,
                    });
                }
                let end = self.nodes.len();
                let node = &mut self.nodes[dir];
                node.children = Some(first..end);
                node.error = None;
            }
            Err(err) => {
                let node = &mut self.nodes[dir];
                node.children = None;
                node.error = Some(err.to_string());
            }
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let mut stack = vec![0];
        while let Some(index) = stack.pop() {
            let node = &self.nodes[index];
            if index != 0 {
                self.rows.push(Row::Node(index));
            }
            if node.is_dir && node.expanded {
                if node.error.is_some() {
                    self.rows.push(Row::Error(index));
                }
                if let Some(children) = &node.children {
                    stack.extend(children.clone().rev());
                }
            }
        }
        self.selected_row =
            self.selected.and_then(|selected| self.rows.iter().position(|row| *row == Row::Node(selected)));
        if self.selected_row.is_none() {
            self.selected = None;
        }
    }

    fn set_expanded(&mut self, index: usize, expanded: bool) {
        if expanded && self.nodes[index].children.is_none() {
            self.load(index);
        }
        self.nodes[index].expanded = expanded;
        self.rebuild_rows();
    }

    /// Toggles a directory or opens a file.
    fn activate(&mut self, index: usize) -> Option<Event> {
        let node = &self.nodes[index];
        if node.is_dir {
            let expanded = node.expanded;
            self.set_expanded(index, !expanded);
            None
        } else {
            Some(Event::Open(self.root.join(&node.rel)))
        }
    }

    fn navigate(&mut self, nav: Nav) -> Option<Event> {
        let len = self.rows.len();
        match nav {
            Nav::Down => {
                let from = self.selected_row.map_or(0, |row| row + 1);
                self.select_first_node_row(from..len);
            }
            Nav::Up => match self.selected_row {
                Some(row) => self.select_first_node_row((0..row).rev()),
                None => self.select_first_node_row((0..len).rev()),
            },
            Nav::Home => self.select_first_node_row(0..len),
            Nav::End => self.select_first_node_row((0..len).rev()),
            Nav::Right => {
                let index = self.selected?;
                let node = &self.nodes[index];
                if !node.is_dir {
                    return None;
                }
                if !node.expanded {
                    self.set_expanded(index, true);
                } else if let Some(children) = &node.children
                    && !children.is_empty()
                {
                    let first = children.start;
                    self.select_node(first);
                }
            }
            Nav::Left => {
                let index = self.selected?;
                let node = &self.nodes[index];
                if node.is_dir && node.expanded {
                    self.set_expanded(index, false);
                } else if node.parent != 0 {
                    let parent = node.parent;
                    self.select_node(parent);
                }
            }
            Nav::Activate => return self.activate(self.selected?),
        }
        None
    }

    /// Selects the first node row among `rows`, keeping the selection when
    /// there is none.
    fn select_first_node_row(&mut self, mut rows: impl Iterator<Item = usize>) {
        let found = rows.find_map(|row| match self.rows[row] {
            Row::Node(index) => Some((row, index)),
            Row::Error(_) => None,
        });
        if let Some((row, index)) = found {
            self.selected = Some(index);
            self.selected_row = Some(row);
        }
    }

    /// Selects a node if it is visible; returns whether it is.
    fn select_node(&mut self, index: usize) -> bool {
        match self.rows.iter().position(|row| *row == Row::Node(index)) {
            Some(row) => {
                self.selected = Some(index);
                self.selected_row = Some(row);
                true
            }
            None => false,
        }
    }

    /// Scrolls so the selected row is inside the viewport.
    fn reveal_selected(&mut self) -> Task<Message> {
        let Some(row) = self.selected_row else {
            return Task::none();
        };
        let top = row as f32 * crate::list_row_height();
        let bottom = top + crate::list_row_height();
        let target = if top < self.scroll_offset {
            top
        } else if self.viewport_height > 0.0 && bottom > self.scroll_offset + self.viewport_height {
            bottom - self.viewport_height
        } else {
            return Task::none();
        };
        self.scroll_offset = target;
        operation::scroll_to(self.scroll_id.clone(), AbsoluteOffset { x: None, y: Some(target) })
    }
}

/// Directories first, then by name ignoring case.
fn compare_entries(a: &Entry, b: &Entry) -> Ordering {
    b.is_dir
        .cmp(&a.is_dir)
        .then_with(|| {
            a.name
                .chars()
                .flat_map(char::to_lowercase)
                .cmp(b.name.chars().flat_map(char::to_lowercase))
        })
        .then_with(|| a.name.cmp(&b.name))
}

fn mark_for(touched: &HashSet<PathBuf>, touched_dirs: &HashSet<PathBuf>, rel: &Path, is_dir: bool) -> Mark {
    if touched.contains(rel) {
        Mark::Touched
    } else if is_dir && touched_dirs.contains(rel) {
        Mark::Contains
    } else {
        Mark::None
    }
}

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
        fn put(&self, dir: &str, entries: &[(&str, bool)]) {
            let entries = entries.iter().map(|&(name, is_dir)| Entry { name: name.to_owned(), is_dir }).collect();
            self.tree.lock().unwrap().insert(PathBuf::from(dir), entries);
        }

        fn calls(&self, dir: &str) -> usize {
            self.calls.lock().unwrap().get(Path::new(dir)).copied().unwrap_or(0)
        }
    }

    impl FileSystem for Arc<FakeFs> {
        fn list(&self, dir: &Path) -> Result<Vec<Entry>> {
            *self.calls.lock().unwrap().entry(dir.to_path_buf()).or_default() += 1;
            self.tree
                .lock()
                .unwrap()
                .get(dir)
                .cloned()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound).into())
        }
    }

    fn fixture() -> (State, Arc<FakeFs>) {
        let fs = Arc::new(FakeFs::default());
        fs.put("/r", &[("b.txt", false), ("src", true), ("a", true), ("Z.md", false)]);
        fs.put("/r/src", &[("main.rs", false), ("lib", true)]);
        fs.put("/r/src/lib", &[("x.rs", false)]);
        fs.put("/r/a", &[]);
        let state = State::new(PathBuf::from("/r"), Box::new(Arc::clone(&fs)));
        (state, fs)
    }

    fn visible(state: &State) -> Vec<&str> {
        state.visible().map(|path| path.to_str().unwrap()).collect()
    }

    fn selected(state: &State) -> Option<&str> {
        state.selected_path().map(|path| path.to_str().unwrap())
    }

    fn nav(state: &mut State, nav: Nav) -> Option<Event> {
        state.update(Message::Nav(nav)).1
    }

    fn index_of(state: &State, rel: &str) -> usize {
        state.nodes.iter().position(|node| node.rel == Path::new(rel)).unwrap()
    }

    fn click(state: &mut State, rel: &str) -> Option<Event> {
        let index = index_of(state, rel);
        state.update(Message::Click(index)).1
    }

    fn mark(state: &State, rel: &str) -> Mark {
        state.nodes[index_of(state, rel)].mark
    }

    #[test]
    fn view_compact_draws_the_visible_rows() {
        let (mut state, _) = fixture();
        assert_eq!(state.row_elements().count(), state.visible().count());
        click(&mut state, "src");
        assert_eq!(state.row_elements().count(), state.visible().count());
        let _ = state.view_compact();
    }

    #[test]
    fn set_root_replaces_the_tree() {
        let (mut state, fs) = fixture();
        click(&mut state, "src");
        fs.put("/q", &[("only.txt", false)]);
        state.set_root(PathBuf::from("/q"));
        assert_eq!(state.root(), Path::new("/q"));
        assert_eq!(visible(&state), ["only.txt"]);
        assert_eq!(selected(&state), None);
        assert_eq!(fs.calls("/q"), 1);
    }

    #[test]
    fn dirs_sort_before_files_case_insensitively() {
        let (state, _) = fixture();
        assert_eq!(visible(&state), ["a", "src", "b.txt", "Z.md"]);
    }

    #[test]
    fn child_dir_is_listed_on_first_expand_only() {
        let (mut state, fs) = fixture();
        assert_eq!(fs.calls("/r"), 1);
        assert_eq!(fs.calls("/r/src"), 0);

        click(&mut state, "src");
        assert_eq!(fs.calls("/r/src"), 1);
        assert_eq!(visible(&state), ["a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"]);
        assert_eq!(fs.calls("/r/src/lib"), 0);

        click(&mut state, "src");
        assert_eq!(visible(&state), ["a", "src", "b.txt", "Z.md"]);
        click(&mut state, "src");
        assert_eq!(fs.calls("/r/src"), 1);
        assert_eq!(visible(&state), ["a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"]);
    }

    #[test]
    fn keyboard_navigation_follows_tree_semantics() {
        let (mut state, _) = fixture();
        nav(&mut state, Nav::Down);
        assert_eq!(selected(&state), Some("a"));
        nav(&mut state, Nav::Down);
        assert_eq!(selected(&state), Some("src"));

        nav(&mut state, Nav::Right);
        assert_eq!(selected(&state), Some("src"));
        assert_eq!(visible(&state), ["a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"]);
        nav(&mut state, Nav::Right);
        assert_eq!(selected(&state), Some("src/lib"));
        nav(&mut state, Nav::Right);
        nav(&mut state, Nav::Down);
        assert_eq!(selected(&state), Some("src/lib/x.rs"));

        nav(&mut state, Nav::Left);
        assert_eq!(selected(&state), Some("src/lib"));
        nav(&mut state, Nav::Left);
        assert_eq!(visible(&state), ["a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"]);
        nav(&mut state, Nav::Down);
        assert_eq!(selected(&state), Some("src/main.rs"));
        nav(&mut state, Nav::Left);
        assert_eq!(selected(&state), Some("src"));
        nav(&mut state, Nav::Left);
        assert_eq!(visible(&state), ["a", "src", "b.txt", "Z.md"]);

        nav(&mut state, Nav::Up);
        assert_eq!(selected(&state), Some("a"));
        nav(&mut state, Nav::Up);
        assert_eq!(selected(&state), Some("a"));
        nav(&mut state, Nav::End);
        assert_eq!(selected(&state), Some("Z.md"));
        nav(&mut state, Nav::Down);
        assert_eq!(selected(&state), Some("Z.md"));
        nav(&mut state, Nav::Home);
        assert_eq!(selected(&state), Some("a"));
    }

    #[test]
    fn activate_opens_file_by_absolute_path_and_toggles_dir() {
        let (mut state, _) = fixture();
        nav(&mut state, Nav::Down);
        nav(&mut state, Nav::Down);
        assert_eq!(nav(&mut state, Nav::Activate), None);
        assert_eq!(visible(&state), ["a", "src", "src/lib", "src/main.rs", "b.txt", "Z.md"]);

        nav(&mut state, Nav::Down);
        nav(&mut state, Nav::Down);
        assert_eq!(nav(&mut state, Nav::Activate), Some(Event::Open(PathBuf::from("/r/src/main.rs"))));
        assert_eq!(click(&mut state, "Z.md"), Some(Event::Open(PathBuf::from("/r/Z.md"))));
        assert_eq!(selected(&state), Some("Z.md"));
    }

    #[test]
    fn touched_marks_file_and_its_ancestor_dirs() {
        let (mut state, _) = fixture();
        state.set_touched([PathBuf::from("/r/src/lib/x.rs"), PathBuf::from("b.txt"), PathBuf::from("/elsewhere/a")]);
        assert_eq!(mark(&state, "b.txt"), Mark::Touched);
        assert_eq!(mark(&state, "src"), Mark::Contains);
        assert_eq!(mark(&state, "a"), Mark::None);
        assert_eq!(mark(&state, "Z.md"), Mark::None);

        click(&mut state, "src");
        click(&mut state, "src/lib");
        assert_eq!(mark(&state, "src/lib"), Mark::Contains);
        assert_eq!(mark(&state, "src/lib/x.rs"), Mark::Touched);
        assert_eq!(mark(&state, "src/main.rs"), Mark::None);

        state.set_touched([PathBuf::from("src/main.rs")]);
        assert_eq!(mark(&state, "src/lib"), Mark::None);
        assert_eq!(mark(&state, "src/lib/x.rs"), Mark::None);
        assert_eq!(mark(&state, "b.txt"), Mark::None);
        assert_eq!(mark(&state, "src"), Mark::Contains);
    }

    #[test]
    fn refresh_picks_up_new_file_and_keeps_expansion_and_selection() {
        let (mut state, fs) = fixture();
        click(&mut state, "src");
        click(&mut state, "src/main.rs");
        fs.put("/r/src", &[("main.rs", false), ("lib", true), ("new.rs", false)]);

        state.refresh();
        assert_eq!(visible(&state), ["a", "src", "src/lib", "src/main.rs", "src/new.rs", "b.txt", "Z.md"]);
        assert_eq!(selected(&state), Some("src/main.rs"));

        fs.put("/r/src", &[("lib", true)]);
        state.refresh();
        assert_eq!(visible(&state), ["a", "src", "src/lib", "b.txt", "Z.md"]);
        assert_eq!(selected(&state), None);
    }

    #[test]
    fn local_fs_honors_gitignore_and_hides_dot_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join(".gitignore"), "ignored.txt\nbuild/\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "").unwrap();
        std::fs::write(root.join("keep.txt"), "").unwrap();
        std::fs::write(root.join(".dotfile"), "").unwrap();
        std::fs::create_dir_all(root.join("build")).unwrap();
        std::fs::write(root.join("build/out"), "").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/ignored.txt"), "").unwrap();
        std::fs::write(root.join("sub/kept.rs"), "").unwrap();

        let mut top = LocalFs.list(root).unwrap();
        top.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        let expected = [(".dotfile", false), (".gitignore", false), ("keep.txt", false), ("sub", true)];
        let expected: Vec<Entry> =
            expected.iter().map(|&(name, is_dir)| Entry { name: name.to_owned(), is_dir }).collect();
        assert_eq!(top, expected);

        let sub = LocalFs.list(&root.join("sub")).unwrap();
        assert_eq!(sub, [Entry { name: "kept.rs".to_owned(), is_dir: false }]);
    }
}
