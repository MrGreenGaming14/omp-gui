use std::collections::BTreeMap;

use loa_contracts::harness::McpTransport;

use super::*;

fn tool(name: &str) -> ToolInfo {
    ToolInfo::new(name, format!("{name} tool"))
}

fn server(name: &str, kind: ServerKind, status: ServerStatus, tools: &[&str]) -> McpServer {
    McpServer {
        name: name.into(),
        transport: McpTransport::Stdio {
            command: name.into(),
            args: vec![],
            env: BTreeMap::new(),
        },
        status,
        kind,
        tools: tools.iter().copied().map(tool).collect(),
    }
}

fn project(id: &str) -> ProjectRef {
    ProjectRef {
        id: id.into(),
        name: format!("Project {id}"),
    }
}

fn panel() -> State {
    let mut state = State::new();
    state.set_builtin(BuiltinTools {
        harness: "omp".into(),
        harness_tools: vec![tool("bash"), tool("read")],
        app_tools: vec![
            tool("project_search"),
            tool("project_read"),
            tool("sandbox_task"),
        ],
        harness_note: None,
    });
    state.set_servers(vec![
        server(
            "remote",
            ServerKind::AppHosted,
            ServerStatus::Running,
            &["remote_bash", "remote_file"],
        ),
        server(
            "browser",
            ServerKind::Surface,
            ServerStatus::Stopped,
            &["surface_click"],
        ),
        server(
            "git",
            ServerKind::External,
            ServerStatus::Error("exited with 1".into()),
            &["git_log"],
        ),
    ]);
    state.set_projects(vec![project("a"), project("b")]);
    state
}

fn send(state: &mut State, message: Message) -> Option<Event> {
    state.update(message).1
}

fn enabled(state: &State, id: &ToolId) -> bool {
    let catalog = state.catalog();
    catalog
        .builtin
        .iter()
        .flat_map(|g| &g.tools)
        .chain(catalog.servers.iter().flat_map(|s| &s.tools))
        .find(|row| &row.id == id)
        .map(|row| row.enabled)
        .unwrap()
}

#[test]
fn tool_toggles_are_scoped_to_the_selected_project() {
    let mut state = panel();
    assert_eq!(
        send(&mut state, Message::ProjectSelected(project("a"))),
        Some(Event::ProjectSelected("a".into()))
    );
    let bash = ToolId::builtin("bash");
    assert_eq!(
        send(&mut state, Message::ToolToggled(bash.clone(), false)),
        Some(Event::SetToolsEnabled {
            project: "a".into(),
            tools: vec![bash.clone()],
            enabled: false
        })
    );
    assert!(!enabled(&state, &bash));

    send(&mut state, Message::ProjectSelected(project("b")));
    assert!(enabled(&state, &bash), "project b keeps its own toggles");

    send(&mut state, Message::ProjectSelected(project("a")));
    assert!(!enabled(&state, &bash));
}

#[test]
fn toggles_do_nothing_without_a_project() {
    let mut state = panel();
    assert_eq!(
        send(
            &mut state,
            Message::ToolToggled(ToolId::builtin("bash"), false)
        ),
        None
    );
    assert_eq!(
        send(&mut state, Message::ServerToggled("remote".into())),
        None
    );
    assert!(enabled(&state, &ToolId::builtin("bash")));
}

#[test]
fn server_toggle_turns_mixed_on_then_all_off() {
    let mut state = panel();
    state.select_project("a");
    state.set_disabled_tools("a", [ToolId::mcp("remote", "remote_bash")]);
    assert_eq!(state.catalog().servers[0].toggle, Toggle::Mixed);

    let all = vec![
        ToolId::mcp("remote", "remote_bash"),
        ToolId::mcp("remote", "remote_file"),
    ];
    assert_eq!(
        send(&mut state, Message::ServerToggled("remote".into())),
        Some(Event::SetToolsEnabled {
            project: "a".into(),
            tools: all.clone(),
            enabled: true
        })
    );
    assert_eq!(state.catalog().servers[0].toggle, Toggle::On);

    assert_eq!(
        send(&mut state, Message::ServerToggled("remote".into())),
        Some(Event::SetToolsEnabled {
            project: "a".into(),
            tools: all,
            enabled: false
        })
    );
    assert_eq!(state.catalog().servers[0].toggle, Toggle::Off);
    // Other servers are untouched.
    assert_eq!(state.catalog().servers[1].toggle, Toggle::On);
}

#[test]
fn app_hosted_server_cannot_be_removed_but_others_can() {
    let mut state = panel();
    assert_eq!(
        send(
            &mut state,
            Message::Server("remote".into(), ServerAction::Remove)
        ),
        None
    );
    assert_eq!(
        send(
            &mut state,
            Message::Server("remote".into(), ServerAction::Restart)
        ),
        Some(Event::Server {
            name: "remote".into(),
            action: ServerAction::Restart
        })
    );
    assert_eq!(
        send(
            &mut state,
            Message::Server("git".into(), ServerAction::Remove)
        ),
        Some(Event::Server {
            name: "git".into(),
            action: ServerAction::Remove
        })
    );
}

#[test]
fn add_server_reports_errors_then_emits_the_spec() {
    let mut state = panel();
    send(&mut state, Message::AddServerOpen);
    send(&mut state, Message::AddServerName("git".into()));
    send(&mut state, Message::AddServerKind(TransportKind::Http));
    send(&mut state, Message::AddServerUrl("localhost:3000".into()));
    assert_eq!(send(&mut state, Message::AddServerSubmit), None);
    let errors = state.add_server_errors().unwrap();
    assert_eq!(errors.name, Some(FieldError::DuplicateServer));
    assert_eq!(errors.target, Some(FieldError::BadScheme));

    send(&mut state, Message::AddServerName("docs".into()));
    assert_eq!(
        state.add_server_errors().unwrap().name,
        None,
        "editing clears the field's error"
    );
    send(
        &mut state,
        Message::AddServerUrl("http://localhost:3000/mcp".into()),
    );
    let event = send(&mut state, Message::AddServerSubmit);
    assert_eq!(
        event,
        Some(Event::AddServer(loa_contracts::harness::McpServerSpec {
            name: "docs".into(),
            transport: McpTransport::Http {
                url: "http://localhost:3000/mcp".into(),
                headers: BTreeMap::new()
            },
        }))
    );
    assert!(
        state.add_server_errors().is_none(),
        "form closes after a valid submit"
    );
}

fn candidate(source: ImportSource, name: &str) -> ImportCandidate {
    ImportCandidate {
        source,
        path: format!("{}/mcp.json", source.label()),
        server: loa_contracts::harness::McpServerSpec {
            name: name.into(),
            transport: McpTransport::Stdio {
                command: name.into(),
                args: vec![],
                env: BTreeMap::new(),
            },
        },
    }
}

#[test]
fn import_lists_candidates_and_records_per_entry_results() {
    let mut state = panel();
    assert_eq!(
        send(&mut state, Message::ImportScan),
        Some(Event::ScanImports)
    );
    state.set_import_candidates(vec![
        candidate(ImportSource::Claude, "git"),
        candidate(ImportSource::Cursor, "fetch"),
        candidate(ImportSource::Codex, "search"),
    ]);
    let entries = state.imports().unwrap();
    assert!(
        entries[0].exists && !entries[0].selected,
        "already registered names start unselected"
    );
    assert!(entries[1].selected && entries[2].selected);

    send(&mut state, Message::ImportSelect(2, false));
    assert_eq!(
        send(&mut state, Message::ImportSubmit),
        Some(Event::ImportServers(vec![candidate(
            ImportSource::Cursor,
            "fetch"
        )]))
    );

    state.set_import_outcome(ImportSource::Cursor, "fetch", ImportOutcome::Imported);
    state.set_import_outcome(
        ImportSource::Claude,
        "git",
        ImportOutcome::Skipped("exists".into()),
    );
    let entries = state.imports().unwrap();
    assert_eq!(entries[1].outcome, Some(ImportOutcome::Imported));
    assert!(
        !entries[1].selected,
        "imported entries are not imported twice"
    );
    assert_eq!(
        entries[0].outcome,
        Some(ImportOutcome::Skipped("exists".into()))
    );
    assert_eq!(entries[2].outcome, None);
    assert_eq!(
        send(&mut state, Message::ImportSubmit),
        None,
        "nothing left selected"
    );
}

fn host(alias: &str) -> RemoteHost {
    RemoteHost {
        alias: alias.into(),
        hostname: format!("{alias}.lan"),
        user: None,
        port: None,
        proxy_jump: Some("bastion".into()),
        limits: HostLimits {
            allowed_paths: vec!["/srv".into()],
            read_only: true,
            commands_allowed: false,
            auto_approve: false,
        },
    }
}

#[test]
fn host_limits_are_edited_and_saved() {
    let mut state = panel();
    state.set_hosts(vec![host("build"), host("db")]);
    send(&mut state, Message::EditLimits("build".into()));
    send(&mut state, Message::LimitsReadOnly(false));
    send(&mut state, Message::LimitsCommands(true));
    send(&mut state, Message::LimitsAutoApprove(true));
    send(&mut state, Message::LimitsNewPath("logs".into()));
    send(&mut state, Message::LimitsAddPath);
    assert_eq!(
        state.limits_draft().unwrap().path_error,
        Some(FieldError::RelativePath)
    );
    send(&mut state, Message::LimitsNewPath("/var/log".into()));
    send(&mut state, Message::LimitsAddPath);
    send(&mut state, Message::LimitsRemovePath(0));
    assert_eq!(
        send(&mut state, Message::LimitsSave),
        Some(Event::SaveHostLimits {
            host: "build".into(),
            limits: HostLimits {
                allowed_paths: vec!["/var/log".into()],
                read_only: false,
                commands_allowed: true,
                auto_approve: true,
            },
        })
    );
    assert!(state.limits_draft().is_none());
}

#[test]
fn host_actions_emit_events() {
    let mut state = panel();
    state.set_hosts(vec![host("build")]);
    assert_eq!(
        send(&mut state, Message::ImportSshHosts),
        Some(Event::ImportSshHosts)
    );
    assert_eq!(
        send(&mut state, Message::OpenTerminal("build".into())),
        Some(Event::OpenTerminal("build".into()))
    );
    assert_eq!(
        send(&mut state, Message::ShowAudit("build".into())),
        Some(Event::LoadAudit("build".into()))
    );

    send(&mut state, Message::AddHostOpen);
    send(&mut state, Message::AddHostAlias("build".into()));
    send(&mut state, Message::AddHostHostname("10.0.0.2".into()));
    assert_eq!(send(&mut state, Message::AddHostSubmit), None);
    assert_eq!(
        state.add_host_errors().unwrap().alias,
        Some(FieldError::DuplicateHost)
    );
    send(&mut state, Message::AddHostAlias("gpu".into()));
    let Some(Event::AddHost(added)) = send(&mut state, Message::AddHostSubmit) else {
        panic!("add host")
    };
    assert_eq!(added.alias, "gpu");
    assert_eq!(added.hostname, "10.0.0.2");
}

#[test]
fn peer_filter_selects_one_peer() {
    let mut state = panel();
    state.set_peers(vec![
        PeerTools {
            key: "k1".into(),
            name: "alice".into(),
            rules: vec![ToolRule::Server("remote".into())],
        },
        PeerTools {
            key: "k2".into(),
            name: "bob".into(),
            rules: vec![ToolRule::AllBuiltin],
        },
    ]);
    assert_eq!(state.peer_matrix().peers.len(), 2);
    send(
        &mut state,
        Message::PeerFilter(PeerChoice::Peer {
            key: "k1".into(),
            name: "alice".into(),
        }),
    );
    let matrix = state.peer_matrix();
    assert_eq!(matrix.peers.len(), 1);
    let tools: Vec<_> = matrix.rows.iter().map(|r| r.tool.clone()).collect();
    assert_eq!(
        tools,
        [
            ToolId::mcp("remote", "remote_bash"),
            ToolId::mcp("remote", "remote_file")
        ]
    );

    // The filtered peer disappearing resets the filter instead of showing nothing.
    state.set_peers(vec![PeerTools {
        key: "k2".into(),
        name: "bob".into(),
        rules: vec![],
    }]);
    assert_eq!(state.peer_matrix().peers.len(), 1);
    assert_eq!(state.peer_matrix().peers[0].name, "bob");
}

/// Clicks a button by its text in a headless render and returns the message it produced.
fn click(state: &State, label: &str) -> Message {
    let mut ui = iced_test::simulator(state.view());
    ui.click(label)
        .unwrap_or_else(|e| panic!("click {label:?}: {e}"));
    let mut messages: Vec<_> = ui.into_messages().collect();
    assert_eq!(messages.len(), 1, "one message from {label:?}");
    messages.remove(0)
}

#[test]
fn rendered_server_controls_emit_actions() {
    let mut state = panel();
    state.select_project("a");
    state.set_disabled_tools("a", [ToolId::mcp("remote", "remote_file")]);
    send(&mut state, Message::SectionSelected(Section::Servers));
    send(&mut state, Message::FilterChanged("remote".into()));

    let message = click(&state, "Tools: mixed");
    assert_eq!(
        send(&mut state, message),
        Some(Event::SetToolsEnabled {
            project: "a".into(),
            tools: vec![
                ToolId::mcp("remote", "remote_bash"),
                ToolId::mcp("remote", "remote_file")
            ],
            enabled: true,
        })
    );

    // App-hosted servers run with the app: no run controls.
    assert!(
        iced_test::simulator(state.view()).find("Restart").is_err(),
        "remote is app hosted"
    );

    // git exited with an error: it can be started again.
    send(&mut state, Message::FilterChanged("git".into()));
    let message = click(&state, "Start");
    assert_eq!(
        send(&mut state, message),
        Some(Event::Server {
            name: "git".into(),
            action: ServerAction::Start
        })
    );
}

#[test]
fn tool_details_render_as_sub_rows() {
    let mut state = panel();
    state.set_builtin(BuiltinTools {
        harness: "omp".into(),
        harness_tools: vec![tool("bash")],
        app_tools: vec![tool("sandbox_task").with_details(vec![
            ToolDetail {
                label: "docker".into(),
                state: DetailState::Ready,
                text: "Docker 27.3".into(),
            },
            ToolDetail {
                label: "firecracker".into(),
                state: DetailState::Unavailable,
                text: "no /dev/kvm".into(),
            },
            ToolDetail {
                label: "ssh".into(),
                state: DetailState::NotConfigured,
                text: "no hosts".into(),
            },
        ])],
        harness_note: None,
    });
    // Closed, the row shows the tool and its description, not the details.
    assert!(iced_test::simulator(state.view()).find("Docker 27.3").is_err(), "details start closed");

    // A click on the row opens its details inline.
    let message = click(&state, "sandbox_task");
    send(&mut state, message);
    {
        let mut ui = iced_test::simulator(state.view());
        for label in ["sandbox_task tool", "docker", "Docker 27.3", "firecracker", "no /dev/kvm", "no hosts"] {
            ui.find(label).unwrap_or_else(|e| panic!("{label:?} rendered: {e}"));
        }
    }

    // A second click closes them.
    let message = click(&state, "sandbox_task");
    send(&mut state, message);
    assert!(iced_test::simulator(state.view()).find("Docker 27.3").is_err(), "details closed again");
}

#[test]
fn long_lists_show_a_preview_until_opened_and_filtering_shows_all() {
    let mut state = panel();
    let names: Vec<String> = (0..20).map(|i| format!("tool_{i:02}")).collect();
    state.set_builtin(BuiltinTools {
        harness: "omp".into(),
        harness_tools: names.iter().map(|n| tool(n)).collect(),
        app_tools: vec![tool("sandbox_task")],
        harness_note: None,
    });
    {
        let mut ui = iced_test::simulator(state.view());
        ui.find("tool_00").unwrap();
        assert!(ui.find("tool_19").is_err(), "the long list is cut");
    }
    let message = click(&state, "Show all 20");
    send(&mut state, message);
    iced_test::simulator(state.view()).find("tool_19").unwrap();
    // "Show fewer" sits below the fold of the test window; send its message.
    send(&mut state, Message::BlockOpen("harness:all".into(), false));
    assert!(iced_test::simulator(state.view()).find("tool_19").is_err());

    // A filter shows every match.
    send(&mut state, Message::FilterChanged("tool_1".into()));
    iced_test::simulator(state.view()).find("tool_19").unwrap();
    send(&mut state, Message::FilterChanged(String::new()));

    // The header closes and opens its card.
    let message = click(&state, "App tools");
    send(&mut state, message);
    assert!(iced_test::simulator(state.view()).find("sandbox_task").is_err(), "app tools closed");
    let message = click(&state, "App tools");
    send(&mut state, message);
    iced_test::simulator(state.view()).find("sandbox_task").unwrap();
}

#[test]
fn remote_prompts_render_as_cards_and_answer_by_id() {
    let mut state = panel();
    state.set_prompts(vec![
        RemotePrompt {
            id: 4,
            kind: PromptKind::HostKey,
            title: "Trust the host key of db?".into(),
            body: "db.lan presented ssh-ed25519 key\nSHA256:abc".into(),
        },
        RemotePrompt {
            id: 9,
            kind: PromptKind::Approval,
            title: "Run a command on db?".into(),
            body: "uptime".into(),
        },
    ]);
    send(&mut state, Message::SectionSelected(Section::Hosts));
    {
        let mut ui = iced_test::simulator(state.view());
        ui.find("Remote hosts (2)").unwrap();
        ui.find("Trust the host key of db?").unwrap();
    }
    let message = click(&state, "Trust");
    assert_eq!(
        send(&mut state, message),
        Some(Event::AnswerPrompt { id: 4, answer: PromptAnswer::Trust })
    );
    let message = click(&state, "Allow for session");
    assert_eq!(
        send(&mut state, message),
        Some(Event::AnswerPrompt { id: 9, answer: PromptAnswer::AllowSession })
    );
    assert!(state.prompts().is_empty());
    assert_eq!(send(&mut state, Message::AnswerPrompt(9, PromptAnswer::Deny)), None, "answered once");
}

#[test]
fn every_section_renders_in_a_narrow_panel() {
    let mut state = panel();
    state.select_project("a");
    state.set_hosts(vec![host("db")]);
    send(&mut state, Message::ShowAudit("db".into()));
    send(&mut state, Message::EditLimits("db".into()));
    for (section, label) in [
        (Section::Builtin, "project_search"),
        (Section::Servers, "remote_bash"),
        (Section::Hosts, "Edit limits"),
        (Section::Peers, "No peers"),
    ] {
        send(&mut state, Message::SectionSelected(section));
        let mut ui = iced_test::Simulator::with_size(iced_test::core::Settings::default(), (340.0, 2000.0), state.view());
        ui.find(label).unwrap_or_else(|e| panic!("{label:?} in {section:?}: {e}"));
    }
}
