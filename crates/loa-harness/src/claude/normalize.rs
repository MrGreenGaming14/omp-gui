//! Normalize malformed optional fields only after strict decoding fails.
use serde_json::{Map, Value, json};

fn fields(obj: &mut Map<String, Value>, names: &[&str], valid: fn(&Value) -> bool) {
    for name in names {
        if obj.get(*name).is_some_and(|v| !valid(v)) { obj.remove(*name); }
    }
}
fn strings(obj: &mut Map<String, Value>, names: &[&str]) { fields(obj, names, Value::is_string); }
fn object(value: &mut Value) -> Option<&mut Map<String, Value>> { value.as_object_mut() }
fn block(value: &mut Value) {
    let Some(b) = object(value) else { *value = json!({"type": ""}); return };
    strings(b, &["type", "text", "thinking", "partial_json", "id", "name", "tool_use_id"]);
    b.entry("type").or_insert_with(|| "".into());
    fields(b, &["is_error"], Value::is_boolean);
    if let Some(Value::Array(items)) = b.get_mut("content") { for item in items { block(item); } }
}
/// Produces a decodeable equivalent for the old field-by-field optional lookups.
pub fn normalize(raw: &Value) -> Value {
    let mut value = raw.clone();
    let Some(o) = object(&mut value) else { return value };
    let kind = o.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
    strings(o, &["subtype", "session_id", "model", "permissionMode", "task_id", "tool_use_id", "task_type", "description", "status", "summary", "uuid", "terminal_reason", "request_id"]);
    fields(o, &["isReplay", "isCompactSummary", "is_error"], Value::is_boolean);
    fields(o, &["duration_ms"], |v| v.as_i64().is_some());
    fields(o, &["total_cost_usd"], |v| v.as_f64().is_some());
    for name in ["compact_metadata", "patch", "usage"] {
        if o.get(name).is_some_and(|v| !v.is_object()) { o.remove(name); }
        if let Some(v) = o.get_mut(name).and_then(object) {
            strings(v, &["status"]);
            fields(v, &["pre_tokens", "input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"], |v| v.as_i64().is_some());
        }
    }
    if let Some(parent) = o.get_mut("parent_tool_use_id") && !parent.is_null() && !parent.is_string() {
        if kind == "assistant" { *parent = Value::Null; } else { *parent = "".into(); }
    }
    if let Some(body) = o.get_mut("message") {
        if let Some(body) = object(body) {
            strings(body, &["id", "model"]);
            if let Some(Value::Array(blocks)) = body.get_mut("content") { for item in blocks { block(item); } }
        } else { *body = Value::Null; }
    }
    if let Some(event) = o.get_mut("event") {
        if let Some(event) = object(event) {
            strings(event, &["type"]);
            let ty = event.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
            if ty.starts_with("content_block_") && event.get("index").and_then(Value::as_u64).is_none() { event.insert("type".into(), "".into()); }
            if ty == "message_start" {
                if !event.get("message").is_some_and(Value::is_object) { event.insert("message".into(), json!({})); }
                if let Some(msg) = event.get_mut("message").and_then(object) { strings(msg, &["id", "model"]); }
            }
            for field in ["content_block", "delta"] {
                if let Some(v) = event.get_mut(field) { block(v); }
                else if (field == "content_block" && ty == "content_block_start") || (field == "delta" && ty == "content_block_delta") { event.insert("type".into(), "".into()); }
            }
            event.entry("type").or_insert_with(|| "".into());
        } else { *event = json!({"type": ""}); }
    } else if kind == "stream_event" { o.insert("event".into(), json!({"type": ""})); }
    if let Some(request) = o.get_mut("request") {
        if let Some(request) = object(request) {
            strings(request, &["subtype", "tool_name", "tool_use_id", "title", "description", "decision_reason", "blocked_path", "server_name"]);
            request.entry("subtype").or_insert_with(|| "".into());
        } else { *request = json!({"subtype": ""}); }
    }
    value
}
