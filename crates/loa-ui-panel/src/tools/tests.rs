use super::*;
use loa_contracts::harness::McpTransport;
use std::collections::BTreeMap;

fn tool(name: &str) -> ToolInfo { ToolInfo::new(name, format!("{name} tool")) }

fn panel() -> State {
    let mut s = State::new();
    s.set_builtin(BuiltinTools { harness: "omp".into(), harness_tools: vec![tool("bash"), tool("read")], app_tools: vec![tool("project_search"), tool("project_read"), tool("sandbox_task")], harness_note: None });
    let server = |name: &str, kind, status, tools: &[&str]| McpServer { name: name.into(), transport: McpTransport::Stdio { command: name.into(), args: vec![], env: BTreeMap::new() }, status, kind, tools: tools.iter().copied().map(tool).collect() };
    s.set_servers(vec![server("remote", ServerKind::AppHosted, ServerStatus::Running, &["remote_bash", "remote_file"]), server("browser", ServerKind::Surface, ServerStatus::Stopped, &["surface_click"]), server("git", ServerKind::External, ServerStatus::Error("exited with 1".into()), &["git_log"])]);
    s.set_projects(vec![project("a"), project("b")]);
    s
}

fn send(s: &mut State, messages: impl IntoIterator<Item = Message>) -> Vec<Event> { messages.into_iter().filter_map(|m| s.update(m).1).collect() }

fn enabled(s: &State, id: &ToolId) -> bool {
    let c = s.catalog();
    c.builtin.iter().flat_map(|g| &g.tools).chain(c.servers.iter().flat_map(|v| &v.tools)).find(|r| &r.id == id).is_some_and(|r| r.enabled)
}

fn project(id: &str) -> ProjectRef { ProjectRef { id: id.into(), name: format!("Project {id}") } }
fn host(alias: &str) -> RemoteHost { RemoteHost { alias: alias.into(), hostname: format!("{alias}.lan"), user: None, port: None, proxy_jump: Some("bastion".into()), limits: HostLimits { allowed_paths: vec!["/srv".into()], read_only: true, ..Default::default() } } }

#[test]
fn toggles_are_scoped_to_the_selected_project() {
    let mut s = panel();
    let bash = ToolId::builtin("bash");
    assert_eq!(send(&mut s, [Message::ToolToggled(bash.clone(), false), Message::ServerToggled("remote".into())]), [], "nothing toggles without a project");
    let events = send(&mut s, [Message::ProjectSelected(project("a")), Message::ToolToggled(bash.clone(), false)]);
    assert_eq!(events, [Event::ProjectSelected("a".into()), Event::SetToolsEnabled { project: "a".into(), tools: vec![bash.clone()], enabled: false }]);
    send(&mut s, [Message::ProjectSelected(project("b"))]);
    assert!(enabled(&s, &bash), "project b keeps its own toggles");
    send(&mut s, [Message::ProjectSelected(project("a"))]);
    assert!(!enabled(&s, &bash));
    s.set_disabled_tools("a", [ToolId::mcp("remote", "remote_bash")]);
    let all = vec![ToolId::mcp("remote", "remote_bash"), ToolId::mcp("remote", "remote_file")];
    for (on, toggle) in [(true, Toggle::On), (false, Toggle::Off)] {
        assert_eq!(send(&mut s, [Message::ServerToggled("remote".into())]), [Event::SetToolsEnabled { project: "a".into(), tools: all.clone(), enabled: on }]);
        assert_eq!((s.catalog().servers[0].toggle, s.catalog().servers[1].toggle), (toggle, Toggle::On));
    }
}

#[test]
fn forms_and_actions_emit_events() {
    let mut s = panel();
    s.set_hosts(vec![host("build"), host("db")]);
    let edit = |f, k: &'static str, v: &str| Message::FormEdit(f, k, v.into());
    let server = |action| Message::Server("remote".into(), action);
    assert_eq!(send(&mut s, [server(ServerAction::Remove), server(ServerAction::Restart), Message::Server("git".into(), ServerAction::Remove)]), [Event::Server { name: "remote".into(), action: ServerAction::Restart }, Event::Server { name: "git".into(), action: ServerAction::Remove }]);
    send(&mut s, [Message::FormOpen(FormId::Server), edit(FormId::Server, "name", "git"), Message::TransportPicked(TransportKind::Http), edit(FormId::Server, "url", "localhost:3000"), Message::FormSubmit(FormId::Server)]);
    assert_eq!(s.add_server().unwrap().errors.keys().collect::<Vec<_>>(), [&"name", &"url"]);
    send(&mut s, [edit(FormId::Server, "name", "docs")]);
    assert_eq!(s.add_server().unwrap().error("name"), None, "editing clears the field's error");
    let added = send(&mut s, [edit(FormId::Server, "url", "http://localhost:3000/mcp"), Message::FormSubmit(FormId::Server)]);
    assert!(matches!(&added[..], [Event::AddServer(spec)] if spec.name == "docs") && s.add_server().is_none());
    send(&mut s, [Message::FormOpen(FormId::Host), edit(FormId::Host, "alias", "build"), edit(FormId::Host, "hostname", "10.0.0.2"), Message::FormSubmit(FormId::Host)]);
    assert!(s.add_host().unwrap().error("alias").is_some(), "the alias is taken");
    assert!(matches!(&send(&mut s, [edit(FormId::Host, "alias", "gpu"), Message::FormSubmit(FormId::Host)])[..], [Event::AddHost(h)] if h.alias == "gpu" && h.hostname == "10.0.0.2"));
    let limits = [Message::EditLimits("build".into()), Message::LimitSet(Limit::ReadOnly, false), Message::LimitSet(Limit::Commands, true), Message::LimitSet(Limit::AutoApprove, true), edit(FormId::Limits, "path", "logs"), Message::LimitsAddPath];
    send(&mut s, limits);
    assert_eq!(s.limits_draft().unwrap().path_error, Some(FieldError::RelativePath));
    let saved = send(&mut s, [edit(FormId::Limits, "path", "/var/log"), Message::LimitsAddPath, Message::LimitsRemovePath(0), Message::FormSubmit(FormId::Limits)]);
    assert_eq!(saved, [Event::SaveHostLimits { host: "build".into(), limits: HostLimits { allowed_paths: vec!["/var/log".into()], read_only: false, commands_allowed: true, auto_approve: true } }]);
    let simple = [(Message::ImportSshHosts, Event::ImportSshHosts), (Message::OpenTerminal("db".into()), Event::OpenTerminal("db".into())), (Message::ShowAudit("db".into()), Event::LoadAudit("db".into())), (Message::RemoveHost("db".into()), Event::RemoveHost("db".into()))];
    for (m, e) in simple { assert_eq!(send(&mut s, [m]), [e]) }
}

#[test]
fn imports_and_peer_filters_follow_the_app() {
    let mut s = panel();
    let candidate = |source, name: &str| ImportCandidate { source, path: "mcp.json".into(), server: loa_contracts::harness::McpServerSpec { name: name.into(), transport: McpTransport::Stdio { command: name.into(), args: vec![], env: BTreeMap::new() } } };
    assert_eq!(send(&mut s, [Message::ImportScan]), [Event::ScanImports]);
    s.set_import_candidates(vec![candidate(ImportSource::Claude, "git"), candidate(ImportSource::Cursor, "fetch"), candidate(ImportSource::Codex, "search")]);
    assert_eq!(s.imports().unwrap().iter().map(|e| (e.exists, e.selected)).collect::<Vec<_>>(), [(true, false), (false, true), (false, true)]);
    assert_eq!(send(&mut s, [Message::ImportSelect(2, false), Message::ImportSubmit]), [Event::ImportServers(vec![candidate(ImportSource::Cursor, "fetch")])]);
    s.set_import_outcome(ImportSource::Cursor, "fetch", ImportOutcome::Imported);
    assert_eq!((s.imports().unwrap()[1].selected, send(&mut s, [Message::ImportSubmit])), (false, vec![]), "imported entries aren't imported twice");
    let peer = |key: &str, rules| PeerTools { key: key.into(), name: key.into(), rules };
    s.set_peers(vec![peer("alice", vec![ToolRule::Server("remote".into())]), peer("bob", vec![ToolRule::AllBuiltin])]);
    send(&mut s, [Message::PeerFilter(PeerChoice::Peer { key: "alice".into(), name: "alice".into() })]);
    assert_eq!(s.peer_matrix().rows.iter().map(|r| r.tool.name()).collect::<Vec<_>>(), ["remote_bash", "remote_file"]);
    s.set_peers(vec![peer("bob", vec![])]);
    assert_eq!(s.peer_matrix().peers[0].name, "bob", "a vanished peer resets the filter");
}

/// The one message a click on `label` produces in a narrow render.
fn click(s: &State, label: &str) -> Message {
    let mut ui = iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(400.0, 2000.0), s.view());
    ui.click(label).unwrap_or_else(|e| panic!("click {label:?}: {e}"));
    let mut messages: Vec<_> = ui.into_messages().collect();
    assert_eq!(messages.len(), 1, "one message from {label:?}");
    messages.remove(0)
}

fn shows(s: &State, label: &str) -> bool { iced_test::Simulator::with_size(iced::Settings::default(), iced::Size::new(400.0, 2000.0), s.view()).find(label).is_ok() }

#[test]
fn every_section_renders_narrow_and_its_controls_click() {
    let mut s = panel();
    s.select_project("a");
    s.set_hosts(vec![host("db")]);
    s.set_prompts(vec![RemotePrompt { id: 9, kind: PromptKind::Approval, title: "Run a command on db?".into(), body: "uptime".into() }]);
    let detail = ToolDetail { label: "docker".into(), state: DetailState::Ready, text: "Docker 27.3".into() };
    let mut builtin = BuiltinTools { harness: "omp".into(), harness_tools: (0..20).map(|i| tool(&format!("tool_{i:02}"))).collect(), app_tools: vec![tool("sandbox_task").with_details(vec![detail])], harness_note: None };
    s.set_builtin(builtin.clone());
    assert!(shows(&s, "tool_00") && !shows(&s, "tool_19") && !shows(&s, "Docker 27.3"), "long lists and details start closed");
    for label in ["Show all 20", "sandbox_task"] {
        let m = click(&s, label);
        send(&mut s, [m]);
    }
    assert!(shows(&s, "tool_19") && shows(&s, "Docker 27.3"));
    send(&mut s, [Message::FilterChanged("remote".into()), Message::SectionSelected(Section::Servers)]);
    assert!(!shows(&s, "Restart"), "app hosted servers have no run controls");
    let m = click(&s, "Tools: on");
    assert!(matches!(&send(&mut s, [m])[..], [Event::SetToolsEnabled { enabled: false, .. }]));
    send(&mut s, [Message::FilterChanged("git".into())]);
    let m = click(&s, "Start");
    assert_eq!(send(&mut s, [m]), [Event::Server { name: "git".into(), action: ServerAction::Start }]);
    send(&mut s, [Message::FilterChanged(String::new()), Message::SectionSelected(Section::Hosts), Message::ShowAudit("db".into()), Message::EditLimits("db".into())]);
    assert!(shows(&s, "Remote hosts (1)") && shows(&s, "Edit limits"));
    let m = click(&s, "Allow for session");
    assert_eq!((send(&mut s, [m]), s.prompts().len()), (vec![Event::AnswerPrompt { id: 9, answer: PromptAnswer::AllowSession }], 0));
    send(&mut s, [Message::SectionSelected(Section::Peers)]);
    assert!(shows(&s, "No peers"));
    builtin.harness_note = Some("Loading".into());
    s.set_builtin(builtin);
    send(&mut s, [Message::SectionSelected(Section::Builtin)]);
    assert!(shows(&s, "Loading"));
}
