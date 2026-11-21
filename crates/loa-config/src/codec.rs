//! Conversion between the raw TOML document and [`Config`], keeping unknown keys.

use crate::{Config, ConfigError, Result, invalid};
use toml::{Table, Value};

/// A key the schema doesn't know, with its value, kept for the next save.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UnknownKey {
    path: Vec<Segment>,
    value: Value,
}

#[derive(Clone, Debug, PartialEq)]
enum Segment {
    Key(String),
    Index(usize),
}

/// Reads the top level `version` of a raw document.
pub(crate) fn version_of(doc: &Table) -> Result<u32> {
    let value = doc.get("version").ok_or(ConfigError::MissingVersion)?;
    value
        .as_integer()
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| invalid("version", format!("must be a non-negative integer, found {value}")))
}

/// Turns a document of the current version into a validated [`Config`].
pub(crate) fn decode(doc: Table) -> Result<Config> {
    // The ignored keys are found during deserialization, which consumes the
    // document, so their values are looked up in this copy afterwards.
    let source = doc.clone();
    let mut ignored = Vec::new();
    let mut on_ignored = |path: serde_ignored::Path<'_>| ignored.push(segments(&path));
    let deserializer = serde_ignored::Deserializer::new(doc, &mut on_ignored);
    let mut config: Config = serde_path_to_error::deserialize(deserializer).map_err(|e| {
        let key = match e.path().to_string() {
            root if root == "." => "(top level)".to_owned(),
            key => key,
        };
        invalid(key, e.into_inner().message().to_owned())
    })?;
    config.unknown = ignored
        .into_iter()
        .filter_map(|path| {
            let value = lookup(&source, &path)?.clone();
            Some(UnknownKey { path, value })
        })
        .collect();
    config.validate()?;
    Ok(config)
}

/// Writes `config` as TOML text, unknown keys included.
pub(crate) fn encode(config: &Config) -> Result<String> {
    let mut doc = Table::try_from(config)?;
    for key in &config.unknown {
        restore(&mut doc, key);
    }
    Ok(toml::to_string(&doc)?)
}

fn segments(path: &serde_ignored::Path<'_>) -> Vec<Segment> {
    use serde_ignored::Path;
    match path {
        Path::Root => Vec::new(),
        Path::Seq { parent, index } => {
            let mut v = segments(parent);
            v.push(Segment::Index(*index));
            v
        }
        Path::Map { parent, key } => {
            let mut v = segments(parent);
            v.push(Segment::Key(key.clone()));
            v
        }
        Path::Some { parent } | Path::NewtypeStruct { parent } | Path::NewtypeVariant { parent } => {
            segments(parent)
        }
    }
}

fn lookup<'a>(doc: &'a Table, path: &[Segment]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let Segment::Key(key) = first else { return None };
    let mut value = doc.get(key)?;
    for segment in rest {
        value = match (segment, value) {
            (Segment::Key(key), Value::Table(t)) => t.get(key)?,
            (Segment::Index(i), Value::Array(a)) => a.get(*i)?,
            _ => return None,
        };
    }
    Some(value)
}

/// Puts an unknown key back where it was. When its parent no longer exists
/// (the user deleted that server or project), the key goes with it.
fn restore(doc: &mut Table, key: &UnknownKey) {
    let Some((Segment::Key(last), parents)) = key.path.split_last() else { return };
    let mut table = doc;
    let mut parents = parents.iter();
    while let Some(segment) = parents.next() {
        let mut value = match segment {
            Segment::Key(k) => table.get_mut(k),
            Segment::Index(_) => None,
        };
        // Arrays of tables: follow indices until the next table.
        loop {
            match value {
                Some(Value::Table(t)) => {
                    table = t;
                    break;
                }
                Some(Value::Array(a)) => match parents.next() {
                    Some(Segment::Index(i)) => value = a.get_mut(*i),
                    _ => return,
                },
                _ => return,
            }
        }
    }
    if !table.contains_key(last) {
        table.insert(last.clone(), key.value.clone());
    }
}
