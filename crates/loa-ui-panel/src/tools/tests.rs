use super::form::error::*;
use super::*;
use loa_contracts::harness::McpTransport;
use loa_ui_kit::testkit::click_label;
use std::collections::BTreeMap;

fn tool(name: &str) -> ToolInfo { ToolInfo::new(name, format!("{name} tool")) }
fn project(id: &str) -> ProjectRef { ProjectRef { id: id.into(), name: format!("Project {id}") } }
fn host(alias: &str) -> RemoteHost { RemoteHost { alias: alias.into(), hostname: format!("{alias}.lan"), user: None, port: None, proxy_jump: None, limits: HostLimits { allowed_paths: vec!["/srv".into()], read_only: true, ..Default::default() } } }
fn filled<F: Form + Default>(mut form: F, values: &[(&str, &str)]) -> F {
    values.iter().for_each(|(k, v)| form.set(k, v).unwrap());
    form
}
fn send(s: &mut State, messages: impl IntoIterator<Item = Message>) -> Vec<Event> { messages.into_iter().filter_map(|m| s.update(m).1).collect() }
fn errors(list: &[(FieldId, &str)]) -> BTreeMap<FieldId, String> { list.iter().map(|(k, e)| (*k, e.to_string())).collect() }
fn names<'a>(c: &Catalog<'a>) -> (Vec<Vec<&'a str>>, Vec<Vec<&'a str>>) {
    let n = |rows: &[catalog::ToolRow<'a>]| rows.iter().map(|r| r.info.name.as_str()).collect();
    (c.builtin.iter().map(|g| n(&g.tools)).collect(), c.servers.iter().map(|v| n(&v.tools)).collect())
}

fn panel() -> State {
    let mut s = State::new();
    s.set_builtin(BuiltinTools { harness: "omp".into(), harness_tools: vec![tool("bash"), tool("read")], app_tools: vec![tool("project_search"), tool("sandbox_task")], harness_note: None });
    let server = |name: &str, kind, status, tools: &[&str]| McpServer { name: name.into(), transport: McpTransport::Stdio { command: name.into(), args: vec![], env: BTreeMap::new() }, status, kind, tools: tools.iter().copied().map(tool).collect() };
    s.set_servers(vec![server("remote", ServerKind::AppHosted, ServerStatus::Running, &["remote_bash", "remote_file"]), server("browser", ServerKind::Surface, ServerStatus::Stopped, &["surface_click", "read"]), server("git", ServerKind::External, ServerStatus::Error("exited".into()), &["git_log"])]);
    s.set_projects(vec![project("a"), project("b")]);
    s.set_hosts(vec![host("build"), host("db")]);
    s
}

#[test]
fn forms_validate_per_field() {
    use TransportKind::{Http, Stdio};
    let server = |name: &str, kind, target: &str| filled(AddServerForm { kind, ..Default::default() }, &[("name", name), ("command", target), ("url", target)]);
    let bad = [
        (server("fs", Stdio, "   "), errors(&[("command", EMPTY)])),
        (server("fs", Stdio, "npx 'oops"), errors(&[("command", UNBALANCED)])),
        (server("fs", Stdio, "A=1 B=2"), errors(&[("command", NO_COMMAND)])),
        (server("my server", Stdio, "x"), errors(&[("name", BAD_NAME)])),
        (server("", Stdio, "'x"), errors(&[("name", EMPTY), ("command", UNBALANCED)])),
        (server("git", Http, "localhost:3000"), errors(&[("name", DUPLICATE_SERVER), ("url", BAD_SCHEME)])),
        (server("w", Http, "https:///mcp"), errors(&[("url", NO_HOST)])),
        (server("w", Http, "https://host:99999/"), errors(&[("url", BAD_PORT)])),
        (server("w", Http, "https://ho st/"), errors(&[("url", WHITESPACE)])),
    ];
    for (f, want) in bad { assert_eq!(f.validate(&["git".into()]).unwrap_err(), want, "{f:?}") }
    for url in [" https://mcp.example.com:8443/sse ", "HTTP://[::1]:3000/mcp", "http://user@localhost/mcp"] { assert!(server("w", Http, url).validate(&[]).is_ok(), "{url}") }
    let McpTransport::Stdio { command, args, env } = server("fs", Stdio, r#"API_KEY=a=b npx -y "@scope/server fs" a\ b"#).validate(&[]).unwrap().transport else { panic!("stdio") };
    assert_eq!((command.as_str(), args, env.get("API_KEY").map(String::as_str)), ("npx", vec!["-y".to_owned(), "@scope/server fs".into(), "a b".into()], Some("a=b")));
    let host_form = |a, h, u, p| filled(AddHostForm::default(), &[("alias", a), ("hostname", h), ("user", u), ("port", p)]);
    let ok = host_form("gpu", "gpu.lan", "ci", "2222").validate(&["build".into()]).unwrap();
    assert_eq!((ok.port, ok.user.as_deref(), ok.limits.read_only, ok.limits.commands_allowed), (Some(2222), Some("ci"), true, false));
    assert_eq!(host_form("build", "", "a b", "0").validate(&["build".into()]).unwrap_err(), errors(&[("alias", DUPLICATE_HOST), ("hostname", EMPTY), ("user", WHITESPACE), ("port", BAD_PORT)]));
    let mut draft = LimitsDraft::new(&host("h"));
    for (path, error) in [("var/log", Some(RELATIVE_PATH)), ("/srv", Some(DUPLICATE_PATH)), (" ~/work ", None)] {
        draft.new_path = path.into();
        draft.add_path();
        assert_eq!(draft.path_error, error);
    }
    assert_eq!(draft.limits.allowed_paths, ["/srv", "~/work"]);
}

#[test]
fn messages_emit_events_and_scope_toggles() {
    let mut s = panel();
    let (bash, edit) = (ToolId::builtin("bash"), |f, k: &'static str, v: &str| Message::FormEdit(f, k, v.into()));
    let remote = vec![ToolId::mcp("remote", "remote_bash"), ToolId::mcp("remote", "remote_file")];
    let cases: Vec<(Vec<Message>, Vec<Event>)> = vec![
        (vec![Message::ToolToggled(bash.clone(), false)], vec![]),
        (vec![Message::ProjectSelected(project("a")), Message::ToolToggled(bash.clone(), false)], vec![Event::ProjectSelected("a".into()), Event::SetToolsEnabled { project: "a".into(), tools: vec![bash], enabled: false }]),
        (vec![Message::ServerToggled("remote".into())], vec![Event::SetToolsEnabled { project: "a".into(), tools: remote, enabled: false }]),
        (vec![Message::Server("remote".into(), ServerAction::Remove), Message::Server("git".into(), ServerAction::Remove)], vec![Event::Server { name: "git".into(), action: ServerAction::Remove }]),
        (vec![Message::FormOpen(FormId::Server), edit(FormId::Server, "name", "git"), Message::TransportPicked(TransportKind::Http), edit(FormId::Server, "url", "x"), Message::FormSubmit(FormId::Server)], vec![]),
        (vec![edit(FormId::Server, "name", "docs"), edit(FormId::Server, "url", "http://h/mcp"), Message::FormSubmit(FormId::Server)], vec![Event::AddServer(McpServerSpec { name: "docs".into(), transport: McpTransport::Http { url: "http://h/mcp".into(), headers: BTreeMap::new() } })]),
        (vec![Message::FormOpen(FormId::Host), edit(FormId::Host, "alias", "build"), edit(FormId::Host, "hostname", "h"), Message::FormSubmit(FormId::Host), Message::FormCancel(FormId::Host)], vec![]),
        (vec![Message::EditLimits("build".into()), Message::LimitSet(Limit::Commands, true), edit(FormId::Limits, "path", "/var"), Message::LimitsAddPath, Message::LimitsRemovePath(0), Message::FormSubmit(FormId::Limits)],
            vec![Event::SaveHostLimits { host: "build".into(), limits: HostLimits { allowed_paths: vec!["/var".into()], read_only: true, commands_allowed: true, auto_approve: false } }]),
        (vec![Message::ImportScan, Message::ImportSubmit, Message::OpenTerminal("db".into()), Message::ShowAudit("db".into()), Message::RemoveHost("db".into())], vec![Event::ScanImports, Event::OpenTerminal("db".into()), Event::LoadAudit("db".into()), Event::RemoveHost("db".into())]),
    ];
    for (messages, want) in cases { assert_eq!(send(&mut s, messages), want) }
    assert!(s.add_host.as_ref().is_none() && s.add_server.is_none());
    send(&mut s, [Message::ProjectSelected(project("b"))]);
    assert_eq!(s.disabled_tools(), None, "project b keeps its own toggles");
    let candidate = |source, name: &str| ImportCandidate { source, path: "mcp.json".into(), server: McpServerSpec { name: name.into(), transport: McpTransport::Stdio { command: name.into(), args: vec![], env: BTreeMap::new() } } };
    s.set_import_candidates(vec![candidate(ImportSource::Claude, "git"), candidate(ImportSource::Cursor, "fetch")]);
    assert_eq!(send(&mut s, [Message::ImportSubmit]), [Event::ImportServers(vec![candidate(ImportSource::Cursor, "fetch")])], "existing servers start deselected");
    s.set_import_outcome(ImportSource::Cursor, "fetch", ImportOutcome::Imported);
    assert_eq!(send(&mut s, [Message::ImportSubmit]), [], "imported entries aren't imported twice");
}

type Groups<'a> = Vec<Vec<&'a str>>;

#[test]
fn catalog_filters_and_peer_matrix_follow_grants() {
    let mut s = panel();
    let cases: [(&str, Groups, Groups); 4] = [
        ("PROJECT_", vec![vec!["project_search"]], vec![]),
        ("brow", vec![], vec![vec!["surface_click", "read"]]),
        ("remote_f", vec![], vec![vec!["remote_file"]]),
        ("read", vec![vec!["read"]], vec![vec!["read"]]),
    ];
    for (f, b, v) in cases {
        send(&mut s, [Message::FilterChanged(f.into())]);
        assert_eq!(names(&s.catalog()), (b, v), "{f}");
    }
    s.select_project("a");
    s.set_disabled_tools("a", [ToolId::mcp("browser", "read")]);
    let c = s.catalog();
    assert_eq!((c.builtin[0].tools[0].enabled, c.servers[0].toggle, c.servers[0].tools[0].enabled), (true, Toggle::Mixed, false), "MCP toggles leave built in tools alone");
    let peer = |k: &str, rules| PeerTools { key: k.into(), name: k.into(), rules };
    s.set_peers(vec![peer("alice", vec![ToolRule::All]), peer("bob", vec![ToolRule::Tool(ToolId::builtin("read")), ToolRule::Server("browser".into())]), peer("carol", vec![ToolRule::AllBuiltin])]);
    send(&mut s, [Message::FilterChanged(String::new())]);
    let m = s.peer_matrix();
    let row = |id: ToolId| m.rows.iter().find(|r| r.tool == id).unwrap().allowed.clone();
    for (id, want) in [(ToolId::builtin("bash"), [true, false, true]), (ToolId::builtin("read"), [true, true, true]), (ToolId::mcp("browser", "read"), [true, true, false]), (ToolId::mcp("git", "git_log"), [true, false, false])] { assert_eq!(row(id), want) }
    send(&mut s, [Message::PeerFilter(PeerChoice::Peer { key: "bob".into(), name: "bob".into() })]);
    assert_eq!(s.peer_matrix().rows.iter().map(|r| r.tool.name()).collect::<Vec<_>>(), ["read", "surface_click", "read"]);
    s.set_peers(vec![peer("carol", vec![])]);
    assert_eq!(s.peer_matrix().peers[0].name, "carol", "a vanished peer resets the filter");
}

#[test]
fn every_section_renders_and_its_controls_click() {
    let mut s = panel();
    s.select_project("a");
    s.set_prompts(vec![RemotePrompt { id: 9, kind: PromptKind::Approval, title: "Run?".into(), body: "uptime".into() }]);
    s.set_builtin(BuiltinTools { harness: "omp".into(), harness_tools: (0..20).map(|i| tool(&format!("tool_{i:02}"))).collect(), app_tools: vec![], harness_note: None });
    let steps: [(Section, &str, Option<Event>); 5] = [
        (Section::Builtin, "Show all 20", None),
        (Section::Servers, "Start", Some(Event::Server { name: "browser".into(), action: ServerAction::Start })),
        (Section::Servers, "Tools: on", Some(Event::SetToolsEnabled { project: "a".into(), tools: vec![ToolId::mcp("remote", "remote_bash"), ToolId::mcp("remote", "remote_file")], enabled: false })),
        (Section::Hosts, "Allow for session", Some(Event::AnswerPrompt { id: 9, answer: PromptAnswer::AllowSession })),
        (Section::Peers, "No peers", None),
    ];
    for (section, label, event) in steps {
        send(&mut s, [Message::SectionSelected(section)]);
        let clicked = click_label(s.view(), label);
        assert_eq!(send(&mut s, clicked).into_iter().next(), event, "{label}");
    }
}
