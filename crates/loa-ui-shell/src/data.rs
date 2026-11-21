//! Plain data the app hands to the shell: node status and usage.

/// The sidebar footer's usage meter: the used share (0 to 1) of the tightest limit and its window, e.g. "7d".
#[derive(Debug, Clone, PartialEq)]
pub struct UsageMeter { pub fraction: f32, pub label: String }

/// Which machine's sessions the sidebar lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeFilter { All, Node(String) }

impl NodeFilter {
    pub fn matches(&self, node: &str) -> bool { if let Self::Node(key) = self { key == node } else { true } }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStatus {
    /// Public key, as written in `<session node>`.
    pub key: String,
    pub name: String,
    pub online: bool,
    /// This is the node the app runs on.
    pub local: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nodes(Vec<NodeStatus>);

impl Nodes {
    pub fn new(list: Vec<NodeStatus>) -> Self { Self(list) }
    pub fn get(&self, key: &str) -> Option<&NodeStatus> { self.0.iter().find(|n| n.key == key) }
    /// The node's name, or the first eight characters of an unknown key.
    pub fn name_of<'a>(&'a self, key: &'a str) -> &'a str { self.get(key).map_or_else(|| key.char_indices().nth(8).map_or(key, |(cut, _)| &key[..cut]), |n| n.name.as_str()) }
    pub fn is_online(&self, key: &str) -> bool { self.get(key).is_some_and(|n| n.online) }
    pub fn iter(&self) -> impl Iterator<Item = &NodeStatus> { self.0.iter() }
}
