//! Golden fixtures: a stable text rendering of a DOM compared with a checked
//! in file.
//!
//! The rendering is one line per element in document order, indented by
//! depth, with attributes sorted by name and text as a JSON string on the
//! line below. Nodes are numbered by document position (`#3`) instead of by
//! id, and attributes holding node ids (`call`, `covers`, `pointers`) are
//! rendered as those numbers, so two DOMs with the same content render the
//! same even when their ids differ.

use crate::{Error, Result};
use loa_core::{Dom, NodeId, Value, attr};
use similar::TextDiff;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

/// Attributes holding unix milliseconds. Live sessions stamp them with the
/// current time, so their goldens mask them.
pub const TIME_ATTRS: &[&str] = &[attr::CREATED, attr::STARTED, attr::ENDED];

/// Environment variable that makes [`Golden::from_env`] rewrite golden files.
pub const BLESS_VAR: &str = "LOA_BLESS";

/// Renders `dom` with every attribute.
pub fn render_golden(dom: &Dom) -> String {
    render_golden_masked(dom, &[])
}

/// Renders `dom` with the values of the `masked` attributes replaced by `*`.
pub fn render_golden_masked(dom: &Dom, masked: &[&str]) -> String {
    let ordinals: HashMap<NodeId, usize> = dom.iter().enumerate().map(|(i, n)| (n.id, i + 1)).collect();
    let root_depth = |id: NodeId| dom.ancestors(id).count();
    let mut out = String::new();
    for node in dom.iter() {
        let indent = "  ".repeat(root_depth(node.id));
        let _ = write!(out, "{indent}#{} <{}", ordinals[&node.id], node.kind);
        for (name, value) in &node.attrs {
            out.push(' ');
            out.push_str(name);
            out.push('=');
            if masked.contains(&name.as_str()) {
                out.push('*');
            } else if is_ref(name) {
                render_ref(value, &ordinals, &mut out);
            } else {
                render_value(value, &mut out);
            }
        }
        out.push_str(">\n");
        if !node.text.is_empty() {
            let _ = writeln!(out, "{indent}  {}", serde_json::Value::from(node.text.as_str()));
        }
    }
    out
}

fn is_ref(name: &str) -> bool {
    matches!(name, attr::CALL | attr::COVERS | attr::POINTERS)
}

fn render_ref(value: &Value, ordinals: &HashMap<NodeId, usize>, out: &mut String) {
    match value {
        Value::Int(id) => match u64::try_from(*id).ok().and_then(|id| ordinals.get(&NodeId(id))) {
            Some(ord) => {
                let _ = write!(out, "#{ord}");
            }
            None => {
                let _ = write!(out, "#?{id}");
            }
        },
        Value::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render_ref(item, ordinals, out);
            }
            out.push(']');
        }
        other => render_value(other, out),
    }
}

fn render_value(value: &Value, out: &mut String) {
    match value {
        Value::Bool(v) => {
            let _ = write!(out, "{v}");
        }
        Value::Int(v) => {
            let _ = write!(out, "{v}");
        }
        Value::Float(v) => {
            let _ = write!(out, "{v:?}");
        }
        Value::Str(v) => {
            let _ = write!(out, "{}", serde_json::Value::from(v.as_str()));
        }
        Value::Bytes(v) => {
            out.push_str("0x");
            for b in v {
                let _ = write!(out, "{b:02x}");
            }
        }
        Value::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                render_value(item, out);
            }
            out.push(']');
        }
    }
}

/// A unified diff of two renderings, empty when they are equal.
pub fn diff_renderings(expected: &str, actual: &str, expected_name: &str, actual_name: &str) -> String {
    if expected == actual {
        return String::new();
    }
    TextDiff::from_lines(expected, actual).unified_diff().context_radius(3).header(expected_name, actual_name).to_string()
}

/// Compares renderings with golden files, or rewrites them when blessing.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Golden {
    bless: bool,
}

impl Golden {
    /// `bless` rewrites golden files instead of comparing.
    pub fn new(bless: bool) -> Self {
        Self { bless }
    }

    /// Blesses when [`BLESS_VAR`] is set to anything but empty or `0`.
    pub fn from_env() -> Self {
        let bless = std::env::var_os(BLESS_VAR).is_some_and(|v| !v.is_empty() && v != "0");
        Self { bless }
    }

    /// Compares `actual` with the file at `path`. When blessing, writes it
    /// (creating parent directories) and succeeds.
    pub fn check(&self, actual: &str, path: &Path) -> Result<()> {
        if self.bless {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|source| Error::io(dir, source))?;
            }
            return std::fs::write(path, actual).map_err(|source| Error::io(path, source));
        }
        let expected = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::GoldenMissing { path: path.to_owned() });
            }
            Err(source) => return Err(Error::io(path, source)),
        };
        // Checkouts may turn `\n` into `\r\n`; the rendering never contains `\r`.
        let expected = expected.replace("\r\n", "\n");
        if expected == actual {
            return Ok(());
        }
        let diff = diff_renderings(&expected, actual, "golden", "actual");
        Err(Error::GoldenMismatch { path: path.to_owned(), diff })
    }

    /// Renders `dom` and checks it against `path`.
    pub fn check_dom(&self, dom: &Dom, path: &Path) -> Result<()> {
        self.check(&render_golden(dom), path)
    }
}
