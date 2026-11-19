//! Scripted Claude Code stream transcripts through the event mapper.

use loa_adapter::{AdapterError, Answer, SessionConfig, SessionState};
use loa_adapter_claude::events::{Mapper, Output};
use loa_core::{Dom, Draft, ElementKind, MemorySink, Node, NodeId, PatchSink, Value as V, answer, attr};
use serde_json::{Value, json};
use std::sync::Arc;

fn config() -> SessionConfig {
    SessionConfig {
        session_id: "app-session".into(),
        node: "node-key".into(),
        cwd: "/work/demo".into(),
        model: Some("claude-sonnet-x".into()),
        ..SessionConfig::default()
    }
}

fn setup() -> (Arc<MemorySink>, Mapper) {
    let sink = Arc::new(MemorySink::new());
    let mapper = Mapper::create(sink.clone(), &config()).unwrap();
    (sink, mapper)
}

fn feed(mapper: &mut Mapper, lines: &[Value]) -> Vec<Output> {
    lines.iter().filter_map(|l| mapper.handle(l.clone()).unwrap()).collect()
}

fn all(dom: &Dom, kind: ElementKind) -> Vec<&Node> {
    dom.iter().filter(|n| n.kind == kind).collect()
}

fn one(dom: &Dom, kind: ElementKind) -> &Node {
    let found = all(dom, kind);
    assert_eq!(found.len(), 1, "expected one <{kind}>, got {found:?}");
    found[0]
}

fn status(node: &Node) -> &str {
    node.str_attr(attr::STATUS).unwrap_or_default()
}

fn ev(event: Value) -> Value {
    json!({"type": "stream_event", "uuid": "e", "session_id": "cc-1", "parent_tool_use_id": null, "event": event})
}

fn init() -> Value {
    json!({"type": "system", "subtype": "init", "session_id": "cc-1", "model": "claude-opus-x", "permissionMode": "default", "tools": ["Bash"]})
}

fn result_ok() -> Value {
    json!({
        "type": "result", "subtype": "success", "is_error": false, "duration_ms": 1200, "num_turns": 2,
        "session_id": "cc-1", "total_cost_usd": 0.25, "terminal_reason": "completed",
        "usage": {"input_tokens": 100, "output_tokens": 40, "cache_read_input_tokens": 7, "cache_creation_input_tokens": 3}
    })
}

fn tool_result(id: &str, content: Value, is_error: bool) -> Value {
    json!({"type": "user", "parent_tool_use_id": null, "session_id": "cc-1", "uuid": "u-r",
        "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error}]}})
}

fn assistant(id: &str, uuid: &str, block: Value) -> Value {
    json!({"type": "assistant", "parent_tool_use_id": null, "session_id": "cc-1", "uuid": uuid,
        "message": {"id": id, "type": "message", "role": "assistant", "model": "claude-opus-x", "content": [block]}})
}

fn tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({"type": "tool_use", "id": id, "name": name, "input": input})
}

fn can_use_tool(request_id: &str, tool: &str, tool_use_id: &str, input: Value) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": {
        "subtype": "can_use_tool", "tool_name": tool, "input": input, "tool_use_id": tool_use_id,
        "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": tool, "ruleContent": "ls:*"}], "behavior": "allow", "destination": "session"}]
    }})
}

fn reply(output: Option<Value>) -> Value {
    output.expect("the request is complete, so a response is due")["response"]["response"].clone()
}

fn ans(values: &[&str], reason: Option<&str>) -> Answer {
    Answer { values: values.iter().map(|v| (*v).to_owned()).collect(), reason: reason.map(str::to_owned), by: "alice".into() }
}

/// A turn that streams thinking, text and a tool call, runs the tool and ends.
#[test]
fn a_streamed_turn_builds_messages_calls_results_and_usage() {
    let (sink, mut mapper) = setup();
    feed(&mut mapper, &[init()]);
    mapper.open_turn("list the files", "alice", "prompt-1").unwrap();
    assert_eq!(mapper.state(), SessionState::Running);

    feed(&mut mapper, &[
        ev(json!({"type": "message_start", "message": {"id": "msg_1", "model": "claude-opus-x", "usage": {}}})),
        ev(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}})),
        ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Need to "}})),
        ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "look."}})),
        ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}})),
    ]);
    sink.with_dom(|dom| {
        let thinking = all(dom, ElementKind::Message).into_iter().find(|m| m.str_attr(attr::KIND) == Some("thinking")).unwrap();
        assert_eq!(thinking.text, "Need to look.");
        assert_eq!(status(thinking), "streaming");
    });
    feed(&mut mapper, &[
        ev(json!({"type": "content_block_stop", "index": 0})),
        assistant("msg_1", "a-1", json!({"type": "thinking", "thinking": "Need to look.", "signature": "sig"})),
        ev(json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}})),
        ev(json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Let me "}})),
        ev(json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "check."}})),
    ]);
    sink.with_dom(|dom| {
        let streaming: Vec<_> = all(dom, ElementKind::Message).into_iter().filter(|m| status(m) == "streaming").collect();
        assert_eq!(streaming.len(), 1);
        assert_eq!(streaming[0].text, "Let me check.");
    });
    feed(&mut mapper, &[
        ev(json!({"type": "content_block_stop", "index": 1})),
        assistant("msg_1", "a-2", json!({"type": "text", "text": "Let me check."})),
        ev(json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {}}})),
        ev(json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"command\":"}})),
        ev(json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "\"ls -la\"}"}})),
        ev(json!({"type": "content_block_stop", "index": 2})),
        assistant("msg_1", "a-3", tool_use("toolu_1", "Bash", json!({"command": "ls -la"}))),
        ev(json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 30}})),
        ev(json!({"type": "message_stop"})),
        tool_result("toolu_1", json!("a.txt\nb.txt"), false),
        ev(json!({"type": "message_start", "message": {"id": "msg_2", "model": "claude-opus-x"}})),
        ev(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})),
        ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Two files."}})),
        ev(json!({"type": "content_block_stop", "index": 0})),
        assistant("msg_2", "a-4", json!({"type": "text", "text": "Two files."})),
        result_ok(),
    ]);
    assert_eq!(mapper.state(), SessionState::Idle);

    let dom = sink.dom();
    let session = dom.root().unwrap();
    assert_eq!(session.str_attr(attr::NATIVE_ID), Some("cc-1"));
    assert_eq!(session.str_attr(attr::MODEL), Some("claude-opus-x"));
    let turn = one(&dom, ElementKind::Turn);
    assert_eq!(status(turn), "done");
    assert!(turn.int_attr(attr::ENDED).is_some());

    let messages = all(&dom, ElementKind::Message);
    let summary: Vec<_> = messages
        .iter()
        .map(|m| (m.str_attr(attr::ROLE).unwrap(), m.str_attr(attr::KIND), m.text.as_str(), status(m), m.str_attr(attr::ID)))
        .collect();
    assert_eq!(summary, vec![
        ("user", None, "list the files", "done", Some("prompt-1")),
        ("assistant", Some("thinking"), "Need to look.", "done", Some("a-1")),
        ("assistant", None, "Let me check.", "done", Some("a-2")),
        ("assistant", None, "Two files.", "done", Some("a-4")),
    ]);
    assert_eq!(messages[1].str_attr(attr::MODEL), Some("claude-opus-x"));

    let call = one(&dom, ElementKind::Call);
    assert_eq!(call.str_attr(attr::TOOL), Some("Bash"));
    assert_eq!(call.str_attr(attr::CALL_ID), Some("toolu_1"));
    assert_eq!(call.str_attr(attr::TITLE), Some("ls -la"));
    assert_eq!(status(call), "done");
    let input = one(&dom, ElementKind::Input);
    assert_eq!(serde_json::from_str::<Value>(&input.text).unwrap(), json!({"command": "ls -la"}));
    let result = one(&dom, ElementKind::Result);
    assert_eq!(result.text, "a.txt\nb.txt");
    assert_eq!(result.bool_attr(attr::IS_ERROR), Some(false));

    let usage = one(&dom, ElementKind::Usage);
    assert_eq!(usage.parent, turn.id);
    assert_eq!(usage.int_attr(attr::INPUT_TOKENS), Some(100));
    assert_eq!(usage.int_attr(attr::OUTPUT_TOKENS), Some(40));
    assert_eq!(usage.int_attr(attr::CACHE_READ_TOKENS), Some(7));
    assert_eq!(usage.int_attr(attr::CACHE_WRITE_TOKENS), Some(3));
    assert_eq!(usage.attr(attr::COST), Some(&V::Float(0.25)));
    assert_eq!(usage.int_attr(attr::DURATION_MS), Some(1200));
}

/// Without `--include-partial-messages` only complete messages arrive.
#[test]
fn complete_messages_without_stream_events_still_map() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("read it", "alice", "p").unwrap();
    feed(&mut mapper, &[
        assistant("m1", "a-1", json!({"type": "text", "text": "Reading."})),
        assistant("m1", "a-2", tool_use("toolu_9", "Read", json!({"file_path": "/work/demo/README.md"}))),
        tool_result("toolu_9", json!([{"type": "text", "text": "line one"}, {"type": "text", "text": "line two"}]), false),
        assistant("m2", "a-3", json!({"type": "redacted_thinking", "data": "opaque"})),
        tool_result("toolu_404", json!("orphan"), true),
        json!({"type": "result", "subtype": "error_max_turns", "is_error": true, "session_id": "cc-1"}),
    ]);
    let dom = sink.dom();
    let call = one(&dom, ElementKind::Call);
    assert_eq!(call.str_attr(attr::TITLE), Some("/work/demo/README.md"));
    assert_eq!(serde_json::from_str::<Value>(&one(&dom, ElementKind::Input).text).unwrap()["file_path"], "/work/demo/README.md");
    assert_eq!(one(&dom, ElementKind::Result).text, "line one\nline two");
    let assistant_text: Vec<_> =
        all(&dom, ElementKind::Message).into_iter().filter(|m| m.str_attr(attr::ROLE) == Some("assistant")).collect();
    assert_eq!(assistant_text.len(), 1);
    assert_eq!(assistant_text[0].text, "Reading.");
    let exts: Vec<_> = all(&dom, ElementKind::Ext).into_iter().map(|e| e.str_attr(attr::TYPE).unwrap().to_owned()).collect();
    assert_eq!(exts, ["block:redacted_thinking", "tool_result"]);
    assert_eq!(status(one(&dom, ElementKind::Turn)), "error");
}

#[test]
fn approval_allow_for_session_sends_the_cli_suggested_rules() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("list", "alice", "p").unwrap();
    feed(&mut mapper, &[assistant("m1", "a-1", tool_use("toolu_1", "Bash", json!({"command": "ls"})))]);
    let out = feed(&mut mapper, &[can_use_tool("req-7", "Bash", "toolu_1", json!({"command": "ls"}))]);
    assert!(out.is_empty(), "nothing goes back to the CLI until the user answers");
    assert_eq!(mapper.state(), SessionState::Waiting);

    let dom = sink.dom();
    let decision = one(&dom, ElementKind::Decision);
    let call = one(&dom, ElementKind::Call);
    assert_eq!(decision.parent, call.id);
    assert_eq!(decision.str_attr(attr::KIND), Some("approval"));
    assert_eq!(decision.str_attr(attr::REQUEST_ID), Some("req-7"));
    assert_eq!(decision.int_attr(attr::CALL), Some(i64::try_from(call.id.0).unwrap()));
    assert_eq!(status(one(&dom, ElementKind::Turn)), "waiting");

    let line = mapper.answer(decision.id, ans(&[answer::ALLOW_SESSION], None)).unwrap();
    let line = line.unwrap();
    assert_eq!(line["type"], "control_response");
    assert_eq!(line["response"]["request_id"], "req-7");
    assert_eq!(line["response"]["subtype"], "success");
    let response = &line["response"]["response"];
    assert_eq!(response["behavior"], "allow");
    assert_eq!(response["updatedInput"], json!({"command": "ls"}));
    assert_eq!(response["updatedPermissions"][0]["rules"][0]["ruleContent"], "ls:*");

    let dom = sink.dom();
    let decision = one(&dom, ElementKind::Decision);
    assert_eq!(status(decision), "answered");
    assert_eq!(decision.attr(attr::ANSWER), Some(&V::from(vec!["allow_session".to_owned()])));
    assert_eq!(decision.str_attr(attr::BY), Some("alice"));
    assert_eq!(status(one(&dom, ElementKind::Turn)), "running");
    assert_eq!(mapper.state(), SessionState::Running);

    // The same decision can't be answered twice.
    assert!(matches!(mapper.answer(decision.id, ans(&[answer::DENY], None)), Err(AdapterError::UnknownDecision(_))));
}

#[test]
fn approval_deny_passes_the_reason_and_bad_answers_are_refused() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("rm", "alice", "p").unwrap();
    feed(&mut mapper, &[
        assistant("m1", "a-1", tool_use("toolu_1", "Bash", json!({"command": "rm -rf build"}))),
        can_use_tool("req-1", "Bash", "toolu_1", json!({"command": "rm -rf build"})),
    ]);
    let decision = sink.with_dom(|dom| one(dom, ElementKind::Decision).id);
    assert!(matches!(mapper.answer(decision, ans(&["approve"], None)), Err(AdapterError::Protocol(_))));
    assert_eq!(sink.with_dom(|dom| status(dom.get(decision).unwrap()).to_owned()), "waiting");

    let response = reply(mapper.answer(decision, ans(&[answer::DENY], Some("not in this repo"))).unwrap());
    assert_eq!(response, json!({"behavior": "deny", "message": "not in this repo"}));
    assert_eq!(sink.with_dom(|dom| dom.get(decision).unwrap().str_attr(attr::REASON).map(str::to_owned)), Some("not in this repo".into()));
}

#[test]
fn ask_user_question_answers_every_question_in_one_response() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("set it up", "alice", "p").unwrap();
    let input = json!({"questions": [
        {"question": "Which database?", "header": "Database", "multiSelect": false,
         "options": [{"label": "SQLite", "description": "file"}, {"label": "Postgres", "description": "server"}]},
        {"question": "Which parts?", "header": "Parts", "multiSelect": true,
         "options": [{"label": "API", "description": ""}, {"label": "CLI", "description": ""}, {"label": "Docs", "description": ""}]}
    ]});
    feed(&mut mapper, &[
        assistant("m1", "a-1", tool_use("toolu_q", "AskUserQuestion", input.clone())),
        can_use_tool("req-q", "AskUserQuestion", "toolu_q", input.clone()),
    ]);
    let decisions: Vec<(NodeId, String, Option<bool>, Vec<String>)> = sink.with_dom(|dom| {
        all(dom, ElementKind::Decision)
            .into_iter()
            .map(|d| {
                assert_eq!(d.str_attr(attr::KIND), Some("question"));
                let options = d.attr(attr::OPTIONS).unwrap().str_items().map(str::to_owned).collect();
                (d.id, d.text.clone(), d.bool_attr(attr::MULTI), options)
            })
            .collect()
    });
    assert_eq!(decisions.len(), 2);
    assert_eq!(decisions[0].1, "Which database?");
    assert_eq!(decisions[0].3, ["SQLite", "Postgres"]);
    assert_eq!(decisions[1].2, Some(true));

    assert_eq!(mapper.answer(decisions[0].0, ans(&["Postgres"], None)).unwrap(), None);
    assert_eq!(mapper.state(), SessionState::Waiting);
    let response = reply(mapper.answer(decisions[1].0, ans(&["API", "Docs"], None)).unwrap());
    assert_eq!(response["behavior"], "allow");
    assert_eq!(response["updatedInput"]["questions"], input["questions"]);
    assert_eq!(response["updatedInput"]["answers"], json!({"Which database?": "Postgres", "Which parts?": "API, Docs"}));
    assert_eq!(mapper.state(), SessionState::Running);
}

#[test]
fn exit_plan_mode_edit_runs_the_edited_plan_and_leaves_plan_mode() {
    let sink = Arc::new(MemorySink::new());
    let mut cfg = config();
    cfg.mode = Some(loa_core::SessionMode::Plan);
    let mut mapper = Mapper::create(sink.clone(), &cfg).unwrap();
    mapper.open_turn("plan it", "alice", "p").unwrap();
    let input = json!({"plan": "1. Write tests\n2. Ship"});
    feed(&mut mapper, &[
        assistant("m1", "a-1", tool_use("toolu_p", "ExitPlanMode", input.clone())),
        can_use_tool("req-p", "ExitPlanMode", "toolu_p", input),
    ]);
    let decision = sink.with_dom(|dom| {
        let d = one(dom, ElementKind::Decision);
        assert_eq!(d.str_attr(attr::KIND), Some("plan"));
        assert_eq!(d.text, "1. Write tests\n2. Ship");
        d.id
    });
    let response = reply(mapper.answer(decision, ans(&[answer::EDIT], Some("1. Write tests first"))).unwrap());
    assert_eq!(response, json!({"behavior": "allow", "updatedInput": {"plan": "1. Write tests first"}}));
    assert_eq!(sink.with_dom(|dom| dom.root().unwrap().str_attr(attr::MODE).map(str::to_owned)), Some("normal".into()));
}

#[test]
fn exit_plan_mode_reject_denies_with_the_feedback() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("plan it", "alice", "p").unwrap();
    feed(&mut mapper, &[can_use_tool("req-p", "ExitPlanMode", "toolu_unknown", json!({"plan": "x"}))]);
    let decision = sink.with_dom(|dom| {
        let d = one(dom, ElementKind::Decision);
        assert_eq!(dom.get(d.parent).unwrap().kind, ElementKind::Turn, "no call known, so the decision sits under the turn");
        d.id
    });
    let response = reply(mapper.answer(decision, ans(&[answer::REJECT], Some("cover the migration"))).unwrap());
    assert_eq!(response, json!({"behavior": "deny", "message": "cover the migration"}));
}

#[test]
fn interrupt_cancels_open_work_and_drops_the_aborted_turns_late_events() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("long task", "alice", "p").unwrap();
    feed(&mut mapper, &[
        ev(json!({"type": "message_start", "message": {"id": "msg_1", "model": "m"}})),
        ev(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})),
        ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Working"}})),
        assistant("m0", "a-0", tool_use("toolu_1", "Bash", json!({"command": "make"}))),
        can_use_tool("req-1", "Bash", "toolu_1", json!({"command": "make"})),
    ]);
    mapper.interrupt().unwrap();
    assert_eq!(mapper.state(), SessionState::Idle);
    {
        let dom = sink.dom();
        assert_eq!(status(one(&dom, ElementKind::Turn)), "cancelled");
        assert_eq!(status(one(&dom, ElementKind::Call)), "cancelled");
        assert_eq!(status(one(&dom, ElementKind::Decision)), "cancelled");
        let reply = all(&dom, ElementKind::Message).into_iter().find(|m| m.str_attr(attr::ROLE) == Some("assistant")).unwrap();
        assert_eq!(status(reply), "cancelled");
        assert_eq!(reply.text, "Working");
    }
    feed(&mut mapper, &[
        ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": " more"}})),
        tool_result("toolu_1", json!("interrupted"), true),
        json!({"type": "result", "subtype": "error_during_execution", "is_error": true, "terminal_reason": "aborted_streaming"}),
    ]);
    {
        let dom = sink.dom();
        let reply = all(&dom, ElementKind::Message).into_iter().find(|m| m.str_attr(attr::ROLE) == Some("assistant")).unwrap();
        assert_eq!(reply.text, "Working", "deltas after the interrupt are dropped");
        assert_eq!(status(one(&dom, ElementKind::Call)), "cancelled", "a late result keeps the call cancelled");
        assert_eq!(status(one(&dom, ElementKind::Turn)), "cancelled");
    }

    // The next prompt is a fresh turn that the next result closes.
    mapper.open_turn("again", "alice", "p2").unwrap();
    feed(&mut mapper, &[assistant("m2", "a-2", json!({"type": "text", "text": "ok"})), result_ok()]);
    let dom = sink.dom();
    let turns = all(&dom, ElementKind::Turn);
    assert_eq!(turns.len(), 2);
    assert_eq!(status(turns[0]), "cancelled");
    assert_eq!(status(turns[1]), "done");
}

#[test]
fn a_result_arriving_after_interrupt_does_not_close_the_next_turn() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("first", "alice", "p1").unwrap();
    mapper.interrupt().unwrap();
    mapper.open_turn("second", "alice", "p2").unwrap();
    feed(&mut mapper, &[json!({"type": "result", "subtype": "error_during_execution", "is_error": true, "terminal_reason": "aborted_streaming"})]);
    // That result belonged to the first turn.
    assert_eq!(mapper.state(), SessionState::Running);
    feed(&mut mapper, &[result_ok()]);
    let dom = sink.dom();
    let turns = all(&dom, ElementKind::Turn);
    assert_eq!((status(turns[0]), status(turns[1])), ("cancelled", "done"));
}

#[test]
fn subagent_calls_become_jobs_with_their_output() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("research", "alice", "p").unwrap();
    feed(&mut mapper, &[
        assistant("m1", "a-1", tool_use("toolu_t", "Task", json!({"description": "Survey crates", "prompt": "look", "subagent_type": "Explore"}))),
        json!({"type": "system", "subtype": "task_started", "task_id": "task-1", "tool_use_id": "toolu_t", "description": "Survey crates"}),
        json!({"type": "assistant", "parent_tool_use_id": "toolu_t", "uuid": "s-1",
            "message": {"id": "sm", "role": "assistant", "content": [{"type": "text", "text": "Found three."}, {"type": "tool_use", "id": "x", "name": "Grep", "input": {}}]}}),
        tool_result("toolu_t", json!("Summary: three crates"), false),
        result_ok(),
    ]);
    let dom = sink.dom();
    let job = one(&dom, ElementKind::Job);
    let call = one(&dom, ElementKind::Call);
    assert_eq!(job.parent, call.id);
    assert_eq!(job.str_attr(attr::KIND), Some("subagent"));
    assert_eq!(job.str_attr(attr::AGENT), Some("Explore"));
    assert_eq!(job.str_attr(attr::TITLE), Some("Survey crates"));
    assert_eq!(job.text, "Found three.\n");
    assert_eq!(status(job), "done");
    // Subagent messages are not messages of the main turn.
    assert_eq!(all(&dom, ElementKind::Message).len(), 1);
}

#[test]
fn background_shell_tasks_are_jobs_until_their_notification() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("serve", "alice", "p").unwrap();
    feed(&mut mapper, &[
        assistant("m1", "a-1", tool_use("toolu_b", "Bash", json!({"command": "cargo watch", "run_in_background": true}))),
        tool_result("toolu_b", json!("started bash_1"), false),
        json!({"type": "system", "subtype": "task_started", "task_id": "bash_1", "tool_use_id": "toolu_b", "task_type": "local_bash", "description": "cargo watch"}),
        result_ok(),
    ]);
    assert_eq!(sink.with_dom(|dom| status(one(dom, ElementKind::Job)).to_owned()), "running");
    feed(&mut mapper, &[json!({"type": "system", "subtype": "task_notification", "task_id": "bash_1", "status": "failed", "summary": "exit 101", "output_file": "/tmp/o"})]);
    let dom = sink.dom();
    let job = one(&dom, ElementKind::Job);
    assert_eq!(job.str_attr(attr::KIND), Some("shell"));
    assert_eq!(status(job), "error");
    assert_eq!(job.text, "exit 101");
}

#[test]
fn compaction_records_covered_turns_and_the_summary() {
    let (sink, mut mapper) = setup();
    for prompt in ["one", "two"] {
        mapper.open_turn(prompt, "alice", prompt).unwrap();
        feed(&mut mapper, &[result_ok()]);
    }
    mapper.open_turn("/compact", "alice", "c").unwrap();
    feed(&mut mapper, &[
        json!({"type": "system", "subtype": "compact_boundary", "compact_metadata": {"trigger": "manual", "pre_tokens": 52000}}),
        json!({"type": "user", "isCompactSummary": true, "parent_tool_use_id": null, "message": {"role": "user", "content": "Summary of the work so far."}}),
        result_ok(),
    ]);
    let dom = sink.dom();
    let turns: Vec<i64> = all(&dom, ElementKind::Turn).iter().map(|t| i64::try_from(t.id.0).unwrap()).collect();
    let compaction = one(&dom, ElementKind::Compaction);
    assert_eq!(compaction.parent, dom.root().unwrap().id);
    assert_eq!(compaction.text, "Summary of the work so far.");
    assert_eq!(compaction.int_attr(attr::TOKENS_BEFORE), Some(52000));
    assert_eq!(compaction.attr(attr::COVERS), Some(&V::List(vec![V::Int(turns[0]), V::Int(turns[1])])));
}

#[test]
fn a_cancelled_permission_request_cancels_its_decision() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("x", "alice", "p").unwrap();
    feed(&mut mapper, &[can_use_tool("req-1", "Write", "toolu_w", json!({"file_path": "/a"}))]);
    assert_eq!(mapper.state(), SessionState::Waiting);
    feed(&mut mapper, &[json!({"type": "control_cancel_request", "request_id": "req-1"})]);
    assert_eq!(mapper.state(), SessionState::Running);
    let dom = sink.dom();
    assert_eq!(status(one(&dom, ElementKind::Decision)), "cancelled");
    assert_eq!(status(one(&dom, ElementKind::Turn)), "running");
}

#[test]
fn control_messages_come_out_for_the_driver() {
    let (_sink, mut mapper) = setup();
    let out = feed(&mut mapper, &[
        json!({"type": "control_request", "request_id": "r1", "request": {"subtype": "mcp_message", "server_name": "loa-host", "message": {"jsonrpc": "2.0", "id": 1, "method": "tools/list"}}}),
        json!({"type": "control_response", "response": {"subtype": "success", "request_id": "loa_1", "response": {"ok": true}}}),
        json!({"type": "control_response", "response": {"subtype": "error", "request_id": "loa_2", "error": "no such model"}}),
        json!({"type": "control_request", "request_id": "r2", "request": {"subtype": "hook_callback", "callback_id": "h"}}),
    ]);
    assert_eq!(out[0], Output::Mcp { request_id: "r1".into(), server: "loa-host".into(), message: json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}) });
    assert_eq!(out[1], Output::ControlResponse { request_id: "loa_1".into(), result: Ok(json!({"ok": true})) });
    assert_eq!(out[2], Output::ControlResponse { request_id: "loa_2".into(), result: Err("no such model".into()) });
    let Output::Write(line) = &out[3] else { panic!("an unknown control request is refused") };
    assert_eq!(line["response"]["subtype"], "error");
    assert_eq!(line["response"]["request_id"], "r2");
}

#[test]
fn a_turn_claude_starts_on_its_own_gets_a_turn_without_a_prompt() {
    let (sink, mut mapper) = setup();
    feed(&mut mapper, &[assistant("m1", "a-1", json!({"type": "text", "text": "The build finished."})), result_ok()]);
    let dom = sink.dom();
    let turn = one(&dom, ElementKind::Turn);
    assert_eq!(status(turn), "done");
    assert_eq!(one(&dom, ElementKind::Message).str_attr(attr::ROLE), Some("assistant"));
}

#[test]
fn unknown_events_are_kept_as_ext() {
    let (sink, mut mapper) = setup();
    let raw = json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}});
    feed(&mut mapper, &[raw.clone(), json!({"type": "keep_alive"})]);
    let dom = sink.dom();
    let ext = one(&dom, ElementKind::Ext);
    assert_eq!(ext.str_attr(attr::HARNESS), Some("claude-code"));
    assert_eq!(ext.str_attr(attr::TYPE), Some("rate_limit_event"));
    assert_eq!(serde_json::from_str::<Value>(&ext.text).unwrap(), raw);
}

#[test]
fn attaching_cancels_a_turn_left_running_by_a_dead_process() {
    let sink = Arc::new(MemorySink::new());
    let mut mapper = Mapper::create(sink.clone(), &config()).unwrap();
    mapper.open_turn("x", "alice", "p").unwrap();
    feed(&mut mapper, &[
        ev(json!({"type": "message_start", "message": {"id": "msg_1", "model": "m"}})),
        ev(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": "half"}})),
        assistant("m0", "a-0", tool_use("toolu_1", "Bash", json!({"command": "make"}))),
        can_use_tool("req-1", "Bash", "toolu_1", json!({"command": "make"})),
    ]);
    drop(mapper);

    let dom = sink.dom();
    let resumed = Arc::new(MemorySink::from_dom(dom.clone()));
    let mapper = Mapper::attach(resumed.clone(), &dom).unwrap();
    assert_eq!(mapper.state(), SessionState::Idle);
    let dom = resumed.dom();
    assert_eq!(status(one(&dom, ElementKind::Turn)), "cancelled");
    assert_eq!(status(one(&dom, ElementKind::Call)), "cancelled");
    assert_eq!(status(one(&dom, ElementKind::Decision)), "cancelled");
    let streamed = all(&dom, ElementKind::Message).into_iter().find(|m| m.text == "half").unwrap();
    assert_eq!(status(streamed), "cancelled");
}

#[test]
fn a_prompt_while_a_turn_runs_is_busy() {
    let (sink, mut mapper) = setup();
    mapper.open_turn("first", "alice", "p1").unwrap();
    assert!(matches!(mapper.open_turn("second", "alice", "p2"), Err(AdapterError::Busy)));
    assert_eq!(sink.with_dom(|dom| all(dom, ElementKind::Turn).len()), 1);
    // The sink still accepts unrelated drafts: the mapper didn't leave a half insert.
    let root = sink.with_dom(|dom| dom.root().unwrap().id);
    sink.commit(Draft::set(root, attr::TITLE, "t")).unwrap();
}
