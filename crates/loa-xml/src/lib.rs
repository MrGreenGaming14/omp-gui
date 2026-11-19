//! XML export of a session DOM for debugging.
//!
//! Export only: there is no parser, and nothing in the app reads this format
//! back. The journal (protobuf) is the source of truth.
//!
//! # Format
//!
//! ```text
//! <?xml version="1.0" encoding="UTF-8"?>
//! <!-- loa session dom: schema 1, seq 5, next nid 6 -->
//! <session nid="1" created.int="1700000000000" harness="omp" id="s1" node="k">
//!   <turn nid="2" status="running">
//!     <message nid="3" role="user">hi</message>
//!   </turn>
//! </session>
//! ```
//!
//! - Tags are [`ElementKind::as_str`](loa_core::ElementKind::as_str). Every
//!   element carries its node id as `nid`, then its attributes sorted by name.
//! - String attributes are written as they are. Any other type adds a
//!   `.int`, `.float`, `.bool`, `.bytes` or `.list` suffix to the name, so the
//!   type is readable without the schema. Bytes are standard base64. Lists are
//!   `[item, item]` where strings are single quoted (`\'` and `\\` escape),
//!   bytes are `bytes(<base64>)`, and floats always show a `.`, an exponent,
//!   `NaN` or `inf`, so they never read as integers.
//! - Attribute names that are not plain XML names, or that hold `.` (the type
//!   separator), get each offending character written as `_x<hex>_`. A `_`
//!   followed by `x` is escaped the same way, so the encoding stays reversible.
//! - Node text is written verbatim between the tags, escaped. An element with
//!   children has its text right after the start tag and its children indented
//!   by two spaces below. Text is never trimmed or re-indented.
//! - `\r` and control characters are written as character references. Control
//!   characters other than tab and newline are not legal XML 1.0; they are kept
//!   anyway because a debug dump must show what the DOM holds.
//! - The same DOM always gives the same bytes: children follow document order
//!   and attributes are sorted.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use loa_core::{Dom, ElementKind, NodeId, SCHEMA_VERSION, Value};
use std::convert::Infallible;
use std::fmt::Write as _;
use std::io;

/// The XML export of `dom` as a string.
pub fn to_xml(dom: &Dom) -> String {
    let mut out = String::with_capacity(64 * dom.len() + 128);
    let Ok(()) = render(dom, &mut out);
    out
}

/// Streams the XML export of `dom` to `writer`. Output is buffered internally,
/// so an unbuffered file or socket is fine.
pub fn write_xml(dom: &Dom, writer: impl io::Write) -> io::Result<()> {
    let mut out = IoOut(io::BufWriter::new(writer));
    render(dom, &mut out)?;
    io::Write::flush(&mut out.0)
}

/// Where rendered text goes: a `String` (infallible) or an `io::Write`.
trait Out {
    type Error;
    fn put(&mut self, text: &str) -> Result<(), Self::Error>;
}

impl Out for String {
    type Error = Infallible;
    fn put(&mut self, text: &str) -> Result<(), Infallible> {
        self.push_str(text);
        Ok(())
    }
}

struct IoOut<W: io::Write>(W);

impl<W: io::Write> Out for IoOut<W> {
    type Error = io::Error;
    fn put(&mut self, text: &str) -> io::Result<()> {
        self.0.write_all(text.as_bytes())
    }
}

/// Pending work of the iterative tree walk. Iterative so deep `<ext>` chains
/// can't overflow the stack.
enum Step {
    Open { id: NodeId, depth: usize },
    Close { kind: ElementKind, depth: usize },
}

fn render<O: Out>(dom: &Dom, out: &mut O) -> Result<(), O::Error> {
    // Reused for every non-string value and every number.
    let mut scratch = String::new();
    out.put("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")?;
    // Numbers only, so the comment can't contain `--`.
    let _ = writeln!(
        scratch,
        "<!-- loa session dom: schema {SCHEMA_VERSION}, seq {}, next nid {} -->",
        dom.seq(),
        dom.next_id()
    );
    out.put(&scratch)?;

    let mut stack: Vec<Step> = dom.root().map(|n| Step::Open { id: n.id, depth: 0 }).into_iter().collect();
    while let Some(step) = stack.pop() {
        match step {
            Step::Close { kind, depth } => {
                indent(out, depth)?;
                out.put("</")?;
                out.put(kind.as_str())?;
                out.put(">\n")?;
            }
            Step::Open { id, depth } => {
                // Ids come from the parent's child list, which a `Dom` keeps
                // consistent with its node map; a miss would be a loa-core bug,
                // and skipping the node is the only sensible output for it.
                let Some(node) = dom.get(id) else { continue };
                indent(out, depth)?;
                out.put("<")?;
                out.put(node.kind.as_str())?;
                scratch.clear();
                let _ = write!(scratch, " nid=\"{}\"", node.id);
                out.put(&scratch)?;
                for (name, value) in &node.attrs {
                    out.put(" ")?;
                    write_name(out, name)?;
                    match value {
                        Value::Str(text) => {
                            out.put("=\"")?;
                            escape(out, text, true)?;
                        }
                        other => {
                            out.put(".")?;
                            out.put(type_suffix(other))?;
                            out.put("=\"")?;
                            scratch.clear();
                            format_value(&mut scratch, other);
                            escape(out, &scratch, true)?;
                        }
                    }
                    out.put("\"")?;
                }
                if node.children.is_empty() {
                    if node.text.is_empty() {
                        out.put("/>\n")?;
                    } else {
                        out.put(">")?;
                        escape(out, &node.text, false)?;
                        out.put("</")?;
                        out.put(node.kind.as_str())?;
                        out.put(">\n")?;
                    }
                } else {
                    out.put(">")?;
                    escape(out, &node.text, false)?;
                    out.put("\n")?;
                    stack.push(Step::Close { kind: node.kind, depth });
                    stack.extend(node.children.iter().rev().map(|&id| Step::Open { id, depth: depth + 1 }));
                }
            }
        }
    }
    Ok(())
}

fn indent<O: Out>(out: &mut O, depth: usize) -> Result<(), O::Error> {
    const SPACES: &str = "                                                                ";
    let mut left = depth * 2;
    while left > 0 {
        let take = left.min(SPACES.len());
        out.put(&SPACES[..take])?;
        left -= take;
    }
    Ok(())
}

fn type_suffix(value: &Value) -> &'static str {
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
        Value::Int(v) => {
            let _ = write!(buf, "{v}");
        }
        // `Debug` always prints a `.` or an exponent for finite floats.
        Value::Float(v) => {
            let _ = write!(buf, "{v:?}");
        }
        Value::Bytes(v) => BASE64.encode_string(v, buf),
        Value::Str(v) => {
            buf.push('\'');
            for c in v.chars() {
                match c {
                    '\'' => buf.push_str("\\'"),
                    '\\' => buf.push_str("\\\\"),
                    c => buf.push(c),
                }
            }
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

/// True when `c` may appear in an attribute name as written. Leaves out `:`
/// (namespaces) and `.` (the type separator).
fn plain_name_char(c: char, first: bool) -> bool {
    c.is_ascii_alphabetic() || c == '_' || (!first && (c.is_ascii_digit() || c == '-'))
}

fn write_name<O: Out>(out: &mut O, name: &str) -> Result<(), O::Error> {
    let bytes = name.as_bytes();
    let needs_escape = name.is_empty()
        || name.char_indices().any(|(i, c)| !plain_name_char(c, i == 0) || (c == '_' && bytes.get(i + 1) == Some(&b'x')));
    if !needs_escape {
        return out.put(name);
    }
    let mut buf = String::with_capacity(name.len() + 8);
    if name.is_empty() {
        // An empty name can't be written; `_x_` is an escape with no
        // characters, and a literal `_x_` name is written `_x005F_x_`.
        buf.push_str("_x_");
    }
    for (i, c) in name.char_indices() {
        let escaped = !plain_name_char(c, i == 0) || (c == '_' && bytes.get(i + 1) == Some(&b'x'));
        if escaped {
            let _ = write!(buf, "_x{:04X}_", u32::from(c));
        } else {
            buf.push(c);
        }
    }
    out.put(&buf)
}

/// Writes `text` with XML escapes, in runs so plain text is one `put`.
fn escape<O: Out>(out: &mut O, text: &str, in_attr: bool) -> Result<(), O::Error> {
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        let replacement: &str = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' if in_attr => "&quot;",
            b'\n' | b'\t' if !in_attr => continue,
            0..=0x1F => CONTROL_REFS[usize::from(b)],
            0x7F => "&#x7F;",
            _ => continue,
        };
        // Every escaped byte is ASCII, so both slice ends are char boundaries.
        out.put(&text[start..i])?;
        out.put(replacement)?;
        start = i + 1;
    }
    out.put(&text[start..])
}

/// Character references for the ASCII control characters, by byte value.
static CONTROL_REFS: [&str; 32] = [
    "&#x00;", "&#x01;", "&#x02;", "&#x03;", "&#x04;", "&#x05;", "&#x06;", "&#x07;", "&#x08;", "&#x09;", "&#x0A;",
    "&#x0B;", "&#x0C;", "&#x0D;", "&#x0E;", "&#x0F;", "&#x10;", "&#x11;", "&#x12;", "&#x13;", "&#x14;", "&#x15;",
    "&#x16;", "&#x17;", "&#x18;", "&#x19;", "&#x1A;", "&#x1B;", "&#x1C;", "&#x1D;", "&#x1E;", "&#x1F;",
];

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, Role, attr};

    fn session() -> (Dom, NodeId) {
        let mut dom = Dom::new();
        let s = dom
            .commit(
                Draft::insert(
                    NodeId::DOCUMENT,
                    ElementKind::Session,
                    [
                        (attr::ID, Value::from("s1")),
                        (attr::HARNESS, "omp".into()),
                        (attr::NODE, "k".into()),
                        (attr::CREATED, Value::Int(1_700_000_000_000)),
                    ],
                    "",
                ),
                1,
            )
            .unwrap()
            .node;
        (dom, s)
    }

    #[test]
    fn golden_session() {
        let (mut dom, s) = session();
        let turn = dom.commit(Draft::insert(s, ElementKind::Turn, [(attr::STATUS, Value::from("running"))], ""), 2);
        let turn = turn.unwrap().node;
        dom.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from(Role::User))], "hi"), 3)
            .unwrap();
        let call = dom.commit(Draft::insert(turn, ElementKind::Call, [(attr::TOOL, Value::from("bash"))], ""), 4);
        let call = call.unwrap().node;
        dom.commit(Draft::insert(call, ElementKind::Input, Vec::<(&str, Value)>::new(), "{\"cmd\":\"ls\"}"), 5)
            .unwrap();
        dom.commit(
            Draft::insert(
                call,
                ElementKind::Decision,
                [
                    (attr::KIND, Value::from("question")),
                    (attr::OPTIONS, Value::from(vec!["Yes".to_owned(), "It's \\ fine".to_owned()])),
                    (attr::MULTI, Value::Bool(false)),
                ],
                "",
            ),
            6,
        )
        .unwrap();
        dom.commit(
            Draft::insert(
                s,
                ElementKind::Ext,
                [
                    (attr::HARNESS, Value::from("claude-code")),
                    ("raw", Value::Bytes(b"\x00\xffhi".to_vec())),
                    ("x-score", Value::Float(1.0)),
                    ("x-mix", Value::List(vec![Value::Int(1), Value::Float(2.5), Value::Bytes(vec![1, 2])])),
                ],
                "a < b && c > \"d\"",
            ),
            7,
        )
        .unwrap();
        let expected = r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- loa session dom: schema 1, seq 7, next nid 8 -->
<session nid="1" created.int="1700000000000" harness="omp" id="s1" node="k">
  <turn nid="2" status="running">
    <message nid="3" role="user">hi</message>
    <call nid="4" tool="bash">
      <input nid="5">{"cmd":"ls"}</input>
      <decision nid="6" kind="question" multi.bool="false" options.list="['Yes', 'It\'s \\ fine']"/>
    </call>
  </turn>
  <ext nid="7" harness="claude-code" raw.bytes="AP9oaQ==" x-mix.list="[1, 2.5, bytes(AQI=)]" x-score.float="1.0">a &lt; b &amp;&amp; c &gt; "d"</ext>
</session>
"#;
        assert_eq!(to_xml(&dom), expected);
        let mut streamed = Vec::new();
        write_xml(&dom, &mut streamed).unwrap();
        assert_eq!(String::from_utf8(streamed).unwrap(), expected);
    }

    #[test]
    fn text_with_children_is_kept_verbatim_before_them() {
        let (mut dom, s) = session();
        let turn = dom.commit(Draft::insert(s, ElementKind::Turn, Vec::<(&str, Value)>::new(), ""), 2).unwrap().node;
        let job = dom.commit(Draft::insert(turn, ElementKind::Job, [(attr::KIND, Value::from("shell"))], ""), 3);
        let job = job.unwrap().node;
        dom.commit(Draft::append(job, "line 1\n\tline 2\r\n"), 4).unwrap();
        dom.commit(Draft::insert(job, ElementKind::Usage, Vec::<(&str, Value)>::new(), ""), 5).unwrap();
        let xml = to_xml(&dom);
        assert!(
            xml.contains("    <job nid=\"3\" kind=\"shell\">line 1\n\tline 2&#x0D;\n\n      <usage nid=\"4\"/>\n    </job>\n"),
            "{xml}"
        );
    }

    #[test]
    fn attribute_values_escape_quotes_newlines_and_controls() {
        let (mut dom, s) = session();
        dom.commit(Draft::set(s, attr::TITLE, "say \"hi\"\n\tto <you> & \u{1}me"), 2).unwrap();
        let xml = to_xml(&dom);
        assert!(xml.contains(r#"title="say &quot;hi&quot;&#x0A;&#x09;to &lt;you&gt; &amp; &#x01;me""#), "{xml}");
    }

    #[test]
    fn odd_attribute_names_are_encoded_reversibly() {
        let (mut dom, s) = session();
        let attrs = [
            ("weird name", Value::from("a")),
            ("1st", "b".into()),
            ("ns:a.b", "c".into()),
            ("_xA", "d".into()),
            ("ok_name-2", "e".into()),
            ("", "f".into()),
            (attr::HARNESS, "omp".into()),
        ];
        dom.commit(Draft::insert(s, ElementKind::Ext, attrs, ""), 2).unwrap();
        let xml = to_xml(&dom);
        for written in [
            r#"weird_x0020_name="a""#,
            r#"_x0031_st="b""#,
            r#"ns_x003A_a_x002E_b="c""#,
            r#"_x005F_xA="d""#,
            r#"ok_name-2="e""#,
            r#" _x_="f""#,
        ] {
            assert!(xml.contains(written), "{written} missing in {xml}");
        }
    }

    #[test]
    fn floats_never_read_as_integers() {
        let (mut dom, s) = session();
        let floats = vec![Value::Float(3.0), Value::Float(1e300), Value::Float(f64::NAN), Value::Float(-0.5)];
        dom.commit(Draft::set(s, "x-f", Value::List(floats)), 2).unwrap();
        assert!(to_xml(&dom).contains(r#"x-f.list="[3.0, 1e300, NaN, -0.5]""#));
    }

    #[test]
    fn output_is_identical_for_equal_doms_built_separately() {
        // Each `Dom` has its own randomly seeded hash map, so iteration order
        // of the node map differs between instances; the export must not.
        let build = || {
            let (mut dom, s) = session();
            for i in 0..50 {
                let turn = dom
                    .commit(Draft::insert(s, ElementKind::Turn, [(attr::ID, Value::from(format!("t{i}")))], ""), 2)
                    .unwrap()
                    .node;
                dom.commit(Draft::insert_at(turn, 0, ElementKind::Todo, [("x-z", Value::Int(i)), ("x-a", Value::Int(-i))], ""), 3)
                    .unwrap();
            }
            dom
        };
        let first = to_xml(&build());
        for _ in 0..5 {
            assert_eq!(to_xml(&build()), first);
        }
    }

    #[test]
    fn empty_dom_has_only_the_header() {
        assert_eq!(
            to_xml(&Dom::new()),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- loa session dom: schema 1, seq 0, next nid 1 -->\n"
        );
    }
}
