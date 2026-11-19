//! Declared capabilities against what the adapter does.
//!
//! [`check_capabilities`] covers what needs no running harness: the schema
//! version, the harness id, and that `import` and `export` return
//! `Unsupported` exactly when undeclared. Session features (`fork`,
//! `set_model`, `set_mode`, `compact`) are probed by
//! [`crate::live::Action::ProbeCapabilities`]. [`check_produced`] checks a DOM
//! against the adapter that wrote it.

use crate::violation::{Rule, Violation};
use loa_adapter::{Adapter, AdapterError, Capabilities};
use loa_core::{
    DecisionKind, Dom, Draft, ElementKind, MemorySink, NodeId, PatchSink, SCHEMA_VERSION, Value, attr, now_ms,
};
use std::path::Path;

/// Checks the static capabilities of `adapter`. `native` is a session file
/// the adapter can import, required to prove a declared import works; without
/// it a declared import is only checked not to be `Unsupported`. `scratch` is
/// an empty directory the export probes may write in.
pub fn check_capabilities(adapter: &dyn Adapter, native: Option<&Path>, scratch: &Path) -> Vec<Violation> {
    let mut out = Vec::new();
    let caps = adapter.capabilities();
    let harness = adapter.harness();
    if caps.schema_version != SCHEMA_VERSION {
        out.push(Violation::new(
            Rule::SchemaVersion,
            None,
            format!("the adapter emits schema {}, loa-core is at {SCHEMA_VERSION}", caps.schema_version),
        ));
    }
    if harness.is_empty() || !harness.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        out.push(Violation::new(Rule::Harness, None, format!("harness id {harness:?} isn't a lowercase slug")));
    }
    let mut seen = Vec::new();
    for kind in &caps.decisions {
        if seen.contains(kind) {
            out.push(Violation::new(Rule::Capability, None, format!("decision kind {kind} is declared twice")));
        }
        seen.push(*kind);
    }

    // Import.
    let sink = MemorySink::new();
    let missing = scratch.join("no-such-session");
    let imported = adapter.import(native.unwrap_or(&missing), &sink);
    match (caps.import, native, &imported) {
        (false, _, result) => expect_unsupported(harness, "import", result, &mut out),
        (true, Some(path), Err(err)) => out.push(Violation::new(
            Rule::Capability,
            None,
            format!("import is declared but importing {} failed: {err}", path.display()),
        )),
        (true, None, Ok(())) => out.push(Violation::new(
            Rule::Capability,
            None,
            format!("importing the missing path {} succeeded", missing.display()),
        )),
        (true, _, result) => expect_supported("import", result, &mut out),
    }
    if !caps.import && !sink.with_dom(Dom::is_empty) {
        out.push(Violation::new(Rule::Capability, None, "an unsupported import wrote to the sink"));
    }

    // Export: the imported DOM when there is one, else a bare session.
    let dom = if caps.import && imported.is_ok() && native.is_some() { sink.dom() } else { bare_session(harness) };
    let target = scratch.join("export-probe");
    let exported = adapter.export(&dom, &target);
    match (caps.export, &exported) {
        (false, result) => {
            expect_unsupported(harness, "export", result, &mut out);
            if target.exists() {
                out.push(Violation::new(Rule::Capability, None, "an unsupported export created its output"));
            }
        }
        (true, Err(err)) if native.is_some() && caps.import => out.push(Violation::new(
            Rule::Capability,
            None,
            format!("export is declared but exporting the imported session failed: {err}"),
        )),
        (true, result) => expect_supported("export", result, &mut out),
    }
    out
}

/// Rules on a DOM that depend on the adapter that wrote it: the session names
/// the adapter's harness, and every decision kind is declared.
pub fn check_produced(dom: &Dom, harness: &str, caps: &Capabilities) -> Vec<Violation> {
    let mut out = Vec::new();
    if let Some(root) = dom.root()
        && root.kind == ElementKind::Session
        && root.str_attr(attr::HARNESS) != Some(harness)
    {
        out.push(Violation::new(
            Rule::Harness,
            root.id,
            format!("<session harness={:?}> from the {harness} adapter", root.str_attr(attr::HARNESS)),
        ));
    }
    for node in dom.iter().filter(|n| n.kind == ElementKind::Decision) {
        let kind = node.str_attr(attr::KIND).and_then(DecisionKind::parse);
        if let Some(kind) = kind
            && !caps.decisions.contains(&kind)
        {
            out.push(Violation::new(Rule::Undeclared, node.id, format!("a {kind} decision, which isn't declared")));
        }
    }
    out
}

/// `result` must be `Unsupported` naming `harness`.
pub(crate) fn expect_unsupported<T>(
    harness: &str,
    what: &str,
    result: &Result<T, AdapterError>,
    out: &mut Vec<Violation>,
) {
    match result {
        Err(AdapterError::Unsupported { harness: named, what: feature }) if *named == harness && *feature == what => {}
        Err(AdapterError::Unsupported { harness: named, what: feature }) => out.push(Violation::new(
            Rule::Capability,
            None,
            format!("{what}: Unsupported names harness {named:?} and feature {feature:?}, expected {harness:?} and {what:?}"),
        )),
        Err(err) => out.push(Violation::new(
            Rule::Capability,
            None,
            format!("{what} isn't declared but returned `{err}` instead of Unsupported"),
        )),
        Ok(_) => out.push(Violation::new(Rule::Capability, None, format!("{what} isn't declared but succeeded"))),
    }
}

/// `result` must not be `Unsupported`.
pub(crate) fn expect_supported<T>(what: &str, result: &Result<T, AdapterError>, out: &mut Vec<Violation>) {
    if let Err(err @ AdapterError::Unsupported { .. }) = result {
        out.push(Violation::new(Rule::Capability, None, format!("{what} is declared but returned `{err}`")));
    }
}

/// A DOM holding only a `<session>` root, for probing `export`.
fn bare_session(harness: &str) -> Dom {
    let sink = MemorySink::new();
    let attrs = [
        (attr::ID, Value::from("contract-probe")),
        (attr::HARNESS, Value::from(harness)),
        (attr::NODE, Value::from("contract-probe")),
        (attr::CREATED, Value::Int(now_ms())),
        (attr::SCHEMA, Value::Int(i64::from(SCHEMA_VERSION))),
    ];
    // A session root with every required attribute always applies to an empty DOM.
    let _ = sink.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""));
    sink.dom()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_must_name_the_undeclared_operation() {
        let mut violations = Vec::new();
        let result: Result<(), AdapterError> =
            Err(AdapterError::Unsupported { harness: "scripted", what: "fork" });
        expect_unsupported("scripted", "import", &result, &mut violations);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, Rule::Capability);
        assert!(violations[0].detail.contains("expected \"scripted\" and \"import\""));
    }
}
