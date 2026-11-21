//! Deterministic XML export of a session DOM for debugging (export only).
//! Elements carry `nid` then attributes sorted by name; non-string values add a
//! `.int`/`.float`/`.bool`/`.bytes`/`.list` suffix (bytes as base64, lists as
//! `[1, 'str', bytes(..)]`); odd name characters become `_x<hex>_`; text is
//! escaped verbatim before any children, which indent by two spaces.

use crate::{Dom, ElementKind, NodeId, SCHEMA_VERSION, Value};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use std::fmt::{self, Write};

/// The XML export of `dom` as a string.
pub fn to_string(dom: &Dom) -> String {
    let mut out = String::with_capacity(64 * dom.len() + 128);
    let _ = write(dom, &mut out);
    out
}

/// Pending work of the walk; iterative so deep `<ext>` chains can't overflow.
enum Step {
    Open(NodeId, usize),
    Close(ElementKind, usize),
}

/// Writes the XML export of `dom` to `out`.
pub fn write(dom: &Dom, out: &mut impl Write) -> fmt::Result {
    out.write_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")?;
    writeln!(out, "<!-- loa session dom: schema {SCHEMA_VERSION}, seq {}, next nid {} -->", dom.seq(), dom.next_id())?;
    let mut scratch = String::new();
    let mut stack: Vec<Step> = dom.root().map(|n| Step::Open(n.id, 0)).into_iter().collect();
    while let Some(step) = stack.pop() {
        let (id, depth) = match step {
            Step::Close(kind, depth) => {
                writeln!(out, "{:w$}</{kind}>", "", w = depth * 2)?;
                continue;
            }
            Step::Open(id, depth) => (id, depth),
        };
        let Some(node) = dom.get(id) else { continue };
        write!(out, "{:w$}<{} nid=\"{}\"", "", node.kind, node.id, w = depth * 2)?;
        for (name, value) in &node.attrs {
            out.write_char(' ')?;
            write_name(out, name)?;
            if let Value::Str(text) = value {
                out.write_str("=\"")?;
                escape(out, text, true)?;
            } else {
                write!(out, ".{}=\"", suffix(value))?;
                scratch.clear();
                format_value(&mut scratch, value);
                escape(out, &scratch, true)?;
            }
            out.write_char('"')?;
        }
        match (node.children.is_empty(), node.text.is_empty()) {
            (true, true) => out.write_str("/>\n")?,
            (true, false) => {
                out.write_char('>')?;
                escape(out, &node.text, false)?;
                writeln!(out, "</{}>", node.kind)?;
            }
            (false, _) => {
                out.write_char('>')?;
                escape(out, &node.text, false)?;
                out.write_char('\n')?;
                stack.push(Step::Close(node.kind, depth));
                stack.extend(node.children.iter().rev().map(|&id| Step::Open(id, depth + 1)));
            }
        }
    }
    Ok(())
}

fn suffix(value: &Value) -> &'static str {
    match value {
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Str(_) => "str",
        Value::Bytes(_) => "bytes",
        Value::List(_) => "list",
    }
}

/// Formats a non-string attribute value, or a list item, into `buf`.
fn format_value(buf: &mut String, value: &Value) {
    match value {
        Value::Bool(v) => buf.push_str(if *v { "true" } else { "false" }),
        Value::Int(v) => drop(write!(buf, "{v}")),
        // `Debug` always prints a `.` or an exponent for finite floats.
        Value::Float(v) => drop(write!(buf, "{v:?}")),
        Value::Bytes(v) => BASE64.encode_string(v, buf),
        Value::Str(v) => {
            buf.push('\'');
            buf.push_str(&v.replace('\\', "\\\\").replace('\'', "\\'"));
            buf.push('\'');
        }
        Value::List(items) => {
            buf.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    buf.push_str(", ");
                }
                if let Value::Bytes(bytes) = item {
                    buf.push_str("bytes(");
                    BASE64.encode_string(bytes, buf);
                    buf.push(')');
                } else {
                    format_value(buf, item);
                }
            }
            buf.push(']');
        }
    }
}

/// Writes `name`, escaping what a plain XML name can't hold. Leaves out `:`
/// (namespaces) and `.` (the type separator); `_x_` stands for the empty name.
fn write_name(out: &mut impl Write, name: &str) -> fmt::Result {
    if name.is_empty() {
        return out.write_str("_x_");
    }
    let bytes = name.as_bytes();
    for (i, c) in name.char_indices() {
        let plain = c.is_ascii_alphabetic() || c == '_' || (i > 0 && (c.is_ascii_digit() || c == '-'));
        if !plain || (c == '_' && bytes.get(i + 1) == Some(&b'x')) {
            write!(out, "_x{:04X}_", u32::from(c))?;
        } else {
            out.write_char(c)?;
        }
    }
    Ok(())
}

/// Writes `text` with XML escapes, in runs so plain text is one write.
fn escape(out: &mut impl Write, text: &str, in_attr: bool) -> fmt::Result {
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        let named = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' if in_attr => "&quot;",
            b'\n' | b'\t' if !in_attr => continue,
            0..=0x1F | 0x7F => "",
            _ => continue,
        };
        // Every escaped byte is ASCII, so both slice ends are char boundaries.
        out.write_str(&text[start..i])?;
        match named {
            "" => write!(out, "&#x{b:02X};")?,
            named => out.write_str(named)?,
        }
        start = i + 1;
    }
    out.write_str(&text[start..])
}
