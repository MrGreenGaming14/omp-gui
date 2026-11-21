//! Conversion between the raw TOML document and [`Config`], keeping unknown keys.

use crate::{CURRENT_VERSION, Config, ConfigError, Result, invalid};
use toml::{Table, Value};

/// A key the schema doesn't know, by its table path, kept for the next save.
/// The schema has no arrays of tables, so a path is table keys only.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UnknownKey {
    path: Vec<String>,
    value: Value,
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
pub(crate) fn decode(mut doc: Table) -> Result<Config> {
    doc.remove("version");
    // Deserializing consumes the document, so ignored values are looked up in a copy.
    let source = doc.clone();
    let mut ignored = Vec::new();
    let mut on_ignored = |path: serde_ignored::Path<'_>| ignored.extend(segments(&path));
    let deserializer = serde_ignored::Deserializer::new(doc, &mut on_ignored);
    let mut config: Config = serde_path_to_error::deserialize(deserializer).map_err(|e| {
        let key = e.path().to_string();
        invalid(if key == "." { "(top level)".to_owned() } else { key }, e.into_inner().message().to_owned())
    })?;
    config.unknown = ignored
        .into_iter()
        .filter_map(|path| Some(UnknownKey { value: lookup(&source, &path)?.clone(), path }))
        .collect();
    config.validate()?;
    Ok(config)
}

/// Writes `config` as TOML text, version and unknown keys included.
pub(crate) fn encode(config: &Config) -> Result<String> {
    let mut doc = Table::try_from(config)?;
    doc.insert("version".to_owned(), Value::Integer(CURRENT_VERSION.into()));
    for key in &config.unknown {
        restore(&mut doc, key);
    }
    Ok(toml::to_string(&doc)?)
}

fn segments(path: &serde_ignored::Path<'_>) -> Option<Vec<String>> {
    use serde_ignored::Path;
    match path {
        Path::Root => Some(Vec::new()),
        Path::Seq { .. } => None,
        Path::Map { parent, key } => {
            let mut v = segments(parent)?;
            v.push(key.clone());
            Some(v)
        }
        Path::Some { parent } | Path::NewtypeStruct { parent } | Path::NewtypeVariant { parent } => segments(parent),
    }
}

fn lookup<'a>(doc: &'a Table, path: &[String]) -> Option<&'a Value> {
    let (last, parents) = path.split_last()?;
    parents.iter().try_fold(doc, |table, key| table.get(key)?.as_table())?.get(last)
}

/// Puts an unknown key back where it was. When its parent no longer exists
/// (the user deleted that server or project), the key goes with it.
fn restore(doc: &mut Table, key: &UnknownKey) {
    let Some((last, parents)) = key.path.split_last() else { return };
    let parent = parents.iter().try_fold(doc, |table, k| table.get_mut(k)?.as_table_mut());
    if let Some(table) = parent {
        table.entry(last.clone()).or_insert_with(|| key.value.clone());
    }
}
