//! Tool schema diff (old vs new) restricted to what the app uses.

use super::{CheckResult, GateCheck};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// One tool as a version advertises it: name and JSON Schema of its input
/// (an object schema with `properties` and `required`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub input_schema: Value,
}

/// Tools the app calls, each with the argument fields it fills in. Fields
/// are dotted paths: `edits.path` names `path` inside the object (or array
/// of objects) `edits`.
pub type UsedTools = BTreeMap<String, BTreeSet<String>>;

/// Lists the tools an installed version offers.
#[async_trait::async_trait]
pub trait ToolCatalog: Send + Sync {
    async fn tools(&self, executable: &Path) -> Result<Vec<ToolSchema>, String>;
}

/// Compares the tool lists of two versions. Harmful when a used tool is
/// gone, a used field is gone or changed JSON type, or any field of a used
/// tool became required. Additions pass and are listed as `+` lines. A used
/// tool neither version offers is no regression of the update and is listed
/// as a `~` line.
pub fn check_tool_schemas(old: &[ToolSchema], new: &[ToolSchema], used: &UsedTools) -> CheckResult {
    let old_by_name: HashMap<&str, &Value> =
        old.iter().map(|t| (t.name.as_str(), &t.input_schema)).collect();
    let new_by_name: HashMap<&str, &Value> =
        new.iter().map(|t| (t.name.as_str(), &t.input_schema)).collect();

    let mut harmful = Vec::new();
    let mut info = Vec::new();
    // Per tool, new top-level fields already explained as rename targets.
    let mut rename_targets: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();

    for (tool, fields) in used {
        let Some(new_schema) = new_by_name.get(tool.as_str()) else {
            if old_by_name.contains_key(tool.as_str()) {
                harmful.push(format!("- {tool}: tool removed"));
            } else {
                info.push(format!("~ {tool}: unavailable in both versions"));
            }
            continue;
        };
        let old_schema = old_by_name.get(tool.as_str()).copied();
        diff_used_tool(tool, old_schema, new_schema, fields, &mut harmful, &mut rename_targets);
    }

    for tool in old {
        if !new_by_name.contains_key(tool.name.as_str()) && !used.contains_key(&tool.name) {
            info.push(format!("- {}: tool removed (unused)", tool.name));
        }
    }

    for tool in new {
        let Some(old_schema) = old_by_name.get(tool.name.as_str()) else {
            info.push(format!("+ {}: tool added", tool.name));
            continue;
        };
        let (Some(old_props), Some(new_props)) =
            (object_properties(old_schema), object_properties(&tool.input_schema))
        else {
            continue;
        };
        let new_required = required_fields(&tool.input_schema);
        let is_used = used.contains_key(&tool.name);
        let targets = rename_targets.get(tool.name.as_str());
        for field in new_props.keys() {
            if old_props.contains_key(field) || targets.is_some_and(|t| t.contains(field)) {
                continue;
            }
            let required = new_required.contains(field.as_str());
            if is_used && required {
                // Reported as harmful by `diff_used_tool`.
                continue;
            }
            let suffix = if required { " (required)" } else { "" };
            info.push(format!("+ {}.{field}: field added{suffix}", tool.name));
        }
    }

    let is_harmful = !harmful.is_empty();
    let mut details = harmful;
    details.extend(info);
    if !is_harmful {
        CheckResult::pass(GateCheck::ToolSchemas, details)
    } else {
        CheckResult::harmful(GateCheck::ToolSchemas, details)
    }
}

fn diff_used_tool<'a>(
    tool: &'a str,
    old_schema: Option<&Value>,
    new_schema: &Value,
    fields: &BTreeSet<String>,
    harmful: &mut Vec<String>,
    rename_targets: &mut BTreeMap<&'a str, BTreeSet<String>>,
) {
    for path in fields {
        let segments: Vec<&str> = path.split('.').collect();
        let old_field = old_schema.and_then(|s| resolve(s, &segments));
        match resolve(new_schema, &segments) {
            None => {
                let rename = old_field.and_then(|old_field| {
                    find_rename(old_schema?, new_schema, &segments, old_field)
                });
                match rename {
                    Some(new_name) => {
                        let parent = &segments[..segments.len() - 1];
                        let new_path = join_path(parent, &new_name);
                        if parent.is_empty() {
                            rename_targets.entry(tool).or_default().insert(new_name);
                        }
                        harmful.push(format!(
                            "- {tool}.{path}: field removed, likely renamed {path} -> {new_path}"
                        ));
                    }
                    None => harmful.push(format!("- {tool}.{path}: field removed")),
                }
            }
            Some(new_field) => {
                let old_type = old_field.and_then(|f| f.get("type"));
                let new_type = new_field.get("type");
                if let (Some(old_type), Some(new_type)) = (old_type, new_type)
                    && old_type != new_type
                {
                    harmful.push(format!("~ {tool}.{path}: type {old_type} -> {new_type}"));
                }
            }
        }
    }

    // Objects whose `required` list matters: the input itself and every
    // object along a used dotted path.
    let mut levels: BTreeSet<Vec<&str>> = BTreeSet::new();
    levels.insert(Vec::new());
    for path in fields {
        let segments: Vec<&str> = path.split('.').collect();
        for end in 1..segments.len() {
            levels.insert(segments[..end].to_vec());
        }
    }
    for level in &levels {
        let Some(new_node) = resolve_node(new_schema, level) else {
            continue;
        };
        let old_required = old_schema
            .and_then(|s| resolve_node(s, level))
            .map(required_fields)
            .unwrap_or_default();
        for field in required_fields(new_node) {
            if !old_required.contains(field) {
                let path = join_path(level, field);
                harmful.push(format!("~ {tool}.{path}: now required"));
            }
        }
    }
}

/// A new sibling property, absent from the old parent, whose schema equals
/// the removed field's schema.
fn find_rename(
    old_schema: &Value,
    new_schema: &Value,
    segments: &[&str],
    old_field: &Value,
) -> Option<String> {
    let parent = &segments[..segments.len() - 1];
    let old_props = object_properties(resolve_node(old_schema, parent)?)?;
    let new_props = object_properties(resolve_node(new_schema, parent)?)?;
    new_props
        .iter()
        .find(|(name, schema)| !old_props.contains_key(name.as_str()) && *schema == old_field)
        .map(|(name, _)| name.clone())
}

fn join_path(parent: &[&str], field: &str) -> String {
    let mut path = parent.join(".");
    if !path.is_empty() {
        path.push('.');
    }
    path.push_str(field);
    path
}

/// The schema of the field at `segments`, descending through object
/// properties and array items.
fn resolve<'v>(schema: &'v Value, segments: &[&str]) -> Option<&'v Value> {
    let (last, parent) = segments.split_last()?;
    object_properties(resolve_node(schema, parent)?)?.get(*last)
}

/// Like [`resolve`], but an empty path is the schema itself.
fn resolve_node<'v>(schema: &'v Value, segments: &[&str]) -> Option<&'v Value> {
    if segments.is_empty() {
        Some(schema)
    } else {
        resolve(schema, segments)
    }
}

/// The object node a schema describes: itself, or its `items` for arrays.
fn object_node(schema: &Value) -> &Value {
    match schema.get("properties") {
        Some(_) => schema,
        None => schema.get("items").unwrap_or(schema),
    }
}

fn object_properties(schema: &Value) -> Option<&Map<String, Value>> {
    object_node(schema).get("properties")?.as_object()
}

fn required_fields(schema: &Value) -> BTreeSet<&str> {
    object_node(schema)
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}
