//! The sidebar's lookup over the [`NodeStatus`] list the app hands to the shell.

use loa_view::shell::NodeStatus;

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
