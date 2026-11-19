//! End to end: the service against in-process russh servers and a fake agent.

mod support;

use std::sync::Arc;

use loa_remote::audit::{Action, AuditQuery, Outcome};
use loa_remote::hosts::Host;
use loa_remote::limits::HostLimits;
use loa_remote::ports::{Approval, HostKeyChange};
use loa_remote::ssh_config::import_hosts;
use loa_remote::{
    BashRequest, EntryKind, Error, FileOp, FileOutput, FileRequest, WriteMode,
};
use support::*;
use tokio_util::sync::CancellationToken;

fn bash(host: &str, command: &str) -> BashRequest {
    BashRequest {
        host: host.into(),
        command: command.into(),
        cwd: None,
        timeout_secs: None,
    }
}

fn file(host: &str, path: &str, op: FileOp) -> FileRequest {
    FileRequest {
        host: host.into(),
        path: path.into(),
        op,
    }
}

fn write(content: &str) -> FileOp {
    FileOp::Write {
        content: content.as_bytes().to_vec(),
    }
}

fn open_limits() -> HostLimits {
    HostLimits {
        auto_approve_writes: true,
        auto_approve_commands: true,
        ..HostLimits::default()
    }
}

/// Agent, one server whose key is already trusted, a harness, host "box".
async fn setup(limits: HostLimits, approvals: Vec<Approval>) -> (FakeAgent, TestServer, Harness) {
    let dir = tempfile::tempdir().unwrap();
    let agent = FakeAgent::start(dir.path()).await;
    let server = TestServer::start(&agent.public()).await;
    let h = Harness::new(dir, Some(&agent), &server.known_hosts_line(), false, approvals);
    let mut host = h.add("box", &server);
    host.limits = limits;
    h.hosts.insert(host);
    (agent, server, h)
}

fn last(h: &Harness) -> loa_remote::audit::AuditRecord {
    h.audit.last().unwrap().unwrap()
}

#[tokio::test]
async fn bash_returns_streams_exit_status_and_reuses_the_connection() {
    let (_agent, server, h) = setup(open_limits(), vec![]).await;
    let ct = CancellationToken::new();
    let out = h.service.bash("s1", bash("box", "echo hello"), &ct).await.unwrap();
    assert_eq!(out.stdout, "hello\n");
    assert_eq!(out.exit_status, Some(0));
    let out = h.service.bash("s1", bash("box", "fail"), &ct).await.unwrap();
    assert_eq!(out.stderr, "boom\n");
    assert_eq!(out.exit_status, Some(3));
    assert_eq!(server.connections(), 1, "second command must reuse the connection");

    let row = last(&h);
    assert_eq!(
        (row.action, row.outcome, row.target.as_str(), row.session_id.as_str()),
        (Action::Command, Outcome::Ok, "fail", "s1")
    );
    assert!(row.detail.starts_with("exit 3"), "{}", row.detail);
}

#[tokio::test]
async fn bash_output_is_capped_and_cwd_is_quoted() {
    let limits = HostLimits {
        max_output_bytes: 1000,
        ..open_limits()
    };
    let (_agent, server, h) = setup(limits, vec![]).await;
    let ct = CancellationToken::new();
    let out = h.service.bash("s", bash("box", "big 5000"), &ct).await.unwrap();
    assert_eq!(out.stdout.len(), 1000);
    assert_eq!(out.stdout_dropped, 4000);
    let mut req = bash("box", "pwd");
    req.cwd = Some("/srv/it's here".into());
    h.service.bash("s", req, &ct).await.unwrap();
    assert_eq!(
        server.stats.commands.lock().unwrap().last().unwrap(),
        "cd '/srv/it'\\''s here' && pwd"
    );
}

#[tokio::test]
async fn bash_cancel_and_timeout_stop_a_hanging_command() {
    let (_agent, server, h) = setup(open_limits(), vec![]).await;
    let ct = CancellationToken::new();
    let service = h.service.clone();
    let task = {
        let ct = ct.clone();
        tokio::spawn(async move { service.bash("s", bash("box", "hang"), &ct).await })
    };
    server.stats.command_seen.notified().await;
    ct.cancel();
    let out = task.await.unwrap().unwrap();
    assert_eq!(out.end, loa_remote::ExecEnd::Cancelled);
    assert_eq!(last(&h).outcome, Outcome::Cancelled);

    let mut req = bash("box", "hang");
    req.timeout_secs = Some(1);
    let out = h
        .service
        .bash("s", req, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.end, loa_remote::ExecEnd::TimedOut);
    assert_eq!(last(&h).outcome, Outcome::TimedOut);
    assert!(server.stats.signals.lock().unwrap().iter().any(|s| s == "KILL"));
}

#[tokio::test]
async fn proxyjump_chain_from_ssh_config_reaches_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let agent = FakeAgent::start(dir.path()).await;
    let outer = TestServer::start(&agent.public()).await;
    let inner = TestServer::start(&agent.public()).await;
    let target = TestServer::start(&agent.public()).await;
    let config = dir.path().join("ssh_config");
    std::fs::write(
        &config,
        format!(
            "Host target\n  HostName 127.0.0.1\n  Port {}\n  ProxyJump inner\n\
             Host inner\n  HostName 127.0.0.1\n  Port {}\n  ProxyJump outer\n\
             Host outer\n  HostName 127.0.0.1\n  Port {}\n\
             Host *\n  User {USER}\n",
            target.port(),
            inner.port(),
            outer.port()
        ),
    )
    .unwrap();
    // Only the outer hop's key is known; the others are accepted at the prompt.
    let h = Harness::new(dir, Some(&agent), &outer.known_hosts_line(), true, vec![]);
    let failed = h.hosts.import(
        import_hosts(&config, h.dir.path(), "nobody").unwrap(),
        &open_limits(),
    );
    assert!(failed.is_empty());
    let host = h.hosts.get("target").unwrap();
    assert_eq!(host.jumps.len(), 2);

    let out = h
        .service
        .bash("s", bash("target", "echo through"), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.stdout, "through\n");
    assert_eq!(
        outer.stats.forwards.lock().unwrap().as_slice(),
        &[("127.0.0.1".to_string(), u32::from(inner.port()))]
    );
    assert_eq!(
        inner.stats.forwards.lock().unwrap().as_slice(),
        &[("127.0.0.1".to_string(), u32::from(target.port()))]
    );
    assert!(outer.stats.commands.lock().unwrap().is_empty());
    assert_eq!(target.stats.commands.lock().unwrap().as_slice(), ["echo through"]);
    let prompted: Vec<u16> = h.decider.prompts.lock().unwrap().iter().map(|p| p.port).collect();
    assert_eq!(prompted, vec![inner.port(), target.port()]);
}

#[tokio::test]
async fn new_host_key_is_prompted_saved_on_accept_and_not_asked_again() {
    let dir = tempfile::tempdir().unwrap();
    let agent = FakeAgent::start(dir.path()).await;
    let server = TestServer::start(&agent.public()).await;
    let h = Harness::new(dir, Some(&agent), "", true, vec![]);
    let mut host = h.add("box", &server);
    host.limits = open_limits();
    h.hosts.insert(host);
    let ct = CancellationToken::new();
    h.service.bash("s", bash("box", "echo 1"), &ct).await.unwrap();
    {
        let prompts = h.decider.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].change, HostKeyChange::New);
        assert_eq!(prompts[0].key_type, "ssh-ed25519");
        assert_eq!(
            prompts[0].fingerprint,
            loa_remote::known_hosts::fingerprint(server.host_key.public_key().key_data())
        );
    }
    let saved = std::fs::read_to_string(&h.known_hosts).unwrap();
    assert_eq!(saved, server.known_hosts_line());
    h.service.disconnect("box").await;
    h.service.bash("s", bash("box", "echo 2"), &ct).await.unwrap();
    assert_eq!(server.connections(), 2);
    assert_eq!(h.decider.prompts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn changed_host_key_is_flagged_and_refused_without_running_anything() {
    let dir = tempfile::tempdir().unwrap();
    let agent = FakeAgent::start(dir.path()).await;
    let server = TestServer::start(&agent.public()).await;
    let old = random_key();
    let known = format!(
        "[127.0.0.1]:{} {}\n",
        server.port(),
        old.public_key().to_openssh().unwrap()
    );
    let h = Harness::new(dir, Some(&agent), &known, false, vec![]);
    let mut host = h.add("box", &server);
    host.limits = open_limits();
    h.hosts.insert(host);

    let err = h
        .service
        .bash("s", bash("box", "echo pwned"), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::HostKeyRejected { .. }), "{err}");
    assert!(err.to_string().contains("CHANGED"), "{err}");
    let prompts = h.decider.prompts.lock().unwrap();
    match &prompts[0].change {
        HostKeyChange::Changed { previous } => {
            assert_eq!(
                previous[0].fingerprint,
                loa_remote::known_hosts::fingerprint(old.public_key().key_data())
            );
        }
        other => panic!("expected Changed, got {other:?}"),
    }
    assert!(server.stats.commands.lock().unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(&h.known_hosts).unwrap(), known);
    let row = last(&h);
    assert_eq!((row.action, row.outcome), (Action::Command, Outcome::Error));
}

#[tokio::test]
async fn auth_uses_only_agent_identities() {
    let dir = tempfile::tempdir().unwrap();
    let agent = FakeAgent::start(dir.path()).await;
    let stranger = random_key();
    let server = TestServer::start(stranger.public_key()).await;
    let h = Harness::new(dir, Some(&agent), &server.known_hosts_line(), false, vec![]);
    let mut host = h.add("box", &server);
    host.limits = open_limits();
    h.hosts.insert(host);
    let err = h
        .service
        .bash("s", bash("box", "echo"), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::AuthFailed { tried: 1, .. }), "{err}");

    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start(stranger.public_key()).await;
    let h = Harness::new(dir, None, &server.known_hosts_line(), false, vec![]);
    let mut host = h.add("box", &server);
    host.limits = open_limits();
    h.hosts.insert(host);
    let err = h
        .service
        .bash("s", bash("box", "echo"), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NoAgent { .. }), "{err}");
}

#[tokio::test]
async fn identity_agent_from_config_overrides_the_default_socket() {
    let dir = tempfile::tempdir().unwrap();
    let default_agent = FakeAgent::start(dir.path()).await;
    let host_agent = FakeAgent::start(dir.path()).await;
    let server = TestServer::start(&host_agent.public()).await;
    let h = Harness::new(dir, Some(&default_agent), &server.known_hosts_line(), false, vec![]);
    let mut host = Host::manual("box", "127.0.0.1", Some(server.port()), USER);
    host.target.agent = loa_remote::ssh_config::AgentSocket::Path(host_agent.socket.clone());
    host.limits = open_limits();
    h.hosts.insert(host);
    let out = h
        .service
        .bash("s", bash("box", "echo ok"), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.stdout, "ok\n");
}

#[tokio::test]
async fn file_write_is_atomic_then_read_list_and_edit() {
    let (_agent, server, h) = setup(open_limits(), vec![]).await;
    let ct = CancellationToken::new();
    server.fs.lock().unwrap().put("/srv/app.conf", b"port = 1\nname = a\n");

    let out = h
        .service
        .file("s", file("box", "/srv/app.conf", write("port = 2\nname = a\n")), &ct)
        .await
        .unwrap();
    assert!(matches!(out, FileOutput::Write { mode: WriteMode::Atomic, bytes: 18, .. }));
    {
        let fs = server.fs.lock().unwrap();
        assert_eq!(fs.get("/srv/app.conf").unwrap(), b"port = 2\nname = a\n");
        assert_eq!(
            fs.nodes.keys().filter(|k| k.starts_with("/srv/")).count(),
            1,
            "temp file left behind: {:?}",
            fs.nodes.keys().collect::<Vec<_>>()
        );
        match fs.nodes.get("/srv/app.conf") {
            Some(Node::File { mode, .. }) => assert_eq!(*mode, 0o640, "permissions kept"),
            other => panic!("{other:?}"),
        }
    }

    let edit = |old: &str, new: &str| {
        file(
            "box",
            "/srv/app.conf",
            FileOp::Edit {
                old: old.into(),
                new: new.into(),
            },
        )
    };
    h.service.file("s", edit("port = 2", "port = 3"), &ct).await.unwrap();
    let err = h.service.file("s", edit(" = ", "="), &ct).await.unwrap_err();
    assert!(err.to_string().contains("occurs 2 times"), "{err}");
    let err = h.service.file("s", edit("missing", "x"), &ct).await.unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");

    match h.service.file("s", file("box", "/srv/app.conf", FileOp::Read), &ct).await.unwrap() {
        FileOutput::Read { content, encoding, .. } => {
            assert_eq!((content.as_str(), encoding), ("port = 3\nname = a\n", "utf8"));
        }
        other => panic!("{other:?}"),
    }
    server.fs.lock().unwrap().put("/srv/blob", &[0xff, 0x00, 0xfe]);
    match h.service.file("s", file("box", "/srv/blob", FileOp::Read), &ct).await.unwrap() {
        FileOutput::Read { content, encoding, .. } => {
            assert_eq!((content.as_str(), encoding), ("/wD+", "base64"));
        }
        other => panic!("{other:?}"),
    }
    server.fs.lock().unwrap().mkdir("/srv/sub");
    match h.service.file("s", file("box", "/srv", FileOp::List), &ct).await.unwrap() {
        FileOutput::List { entries, .. } => {
            let got: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
            assert_eq!(
                got,
                vec![
                    ("app.conf", EntryKind::File),
                    ("blob", EntryKind::File),
                    ("sub", EntryKind::Dir)
                ]
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(server.connections(), 1);
}

#[tokio::test]
async fn write_without_posix_rename_falls_back_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let agent = FakeAgent::start(dir.path()).await;
    let server = TestServer::start_with(&agent.public(), false).await;
    let h = Harness::new(dir, Some(&agent), &server.known_hosts_line(), false, vec![]);
    let mut host = h.add("box", &server);
    host.limits = open_limits();
    h.hosts.insert(host);
    let ct = CancellationToken::new();
    // New file: plain rename onto a free name is still atomic.
    let out = h.service.file("s", file("box", "/srv/new", write("a")), &ct).await.unwrap();
    assert!(matches!(out, FileOutput::Write { mode: WriteMode::Atomic, .. }));
    // Existing file: SFTPv3 rename refuses to overwrite.
    let out = h.service.file("s", file("box", "/srv/new", write("bb")), &ct).await.unwrap();
    assert!(matches!(out, FileOutput::Write { mode: WriteMode::InPlace, .. }));
    let fs = server.fs.lock().unwrap();
    assert_eq!(fs.get("/srv/new").unwrap(), b"bb");
    assert_eq!(fs.nodes.keys().filter(|k| k.starts_with("/srv/")).count(), 1);
}

#[tokio::test]
async fn read_only_host_refuses_before_connecting_and_audits() {
    let limits = HostLimits {
        read_only: true,
        ..open_limits()
    };
    let (_agent, server, h) = setup(limits, vec![]).await;
    let ct = CancellationToken::new();
    let err = h.service.file("s9", file("box", "/srv/x", write("x")), &ct).await.unwrap_err();
    assert!(matches!(err, Error::Refused(_)));
    let err = h.service.bash("s9", bash("box", "echo"), &ct).await.unwrap_err();
    assert!(matches!(err, Error::Refused(_)));
    assert_eq!(server.connections(), 0, "nothing may be sent for refused actions");
    let rows = h
        .audit
        .query(&AuditQuery {
            session_id: Some("s9".into()),
            ..Default::default()
        })
        .unwrap();
    let got: Vec<_> = rows.iter().map(|r| (r.action, r.outcome, r.target.as_str())).collect();
    assert_eq!(
        got,
        vec![
            (Action::Command, Outcome::Refused, "echo"),
            (Action::Write, Outcome::Refused, "/srv/x"),
        ]
    );
}

#[tokio::test]
async fn commands_disabled_and_unknown_host_are_refused() {
    let limits = HostLimits {
        commands: false,
        ..open_limits()
    };
    let (_agent, server, h) = setup(limits, vec![]).await;
    let ct = CancellationToken::new();
    assert!(matches!(
        h.service.bash("s", bash("box", "echo"), &ct).await,
        Err(Error::Refused(_))
    ));
    assert!(matches!(
        h.service.bash("s", bash("nope", "echo"), &ct).await,
        Err(Error::UnknownHost(_))
    ));
    assert_eq!(last(&h).outcome, Outcome::Refused);
    assert_eq!(last(&h).host, "nope");
    assert_eq!(server.connections(), 0);
}

#[tokio::test]
async fn allowed_paths_block_dotdot_and_symlink_escapes() {
    let limits = HostLimits {
        allowed_paths: vec!["/srv".into()],
        ..open_limits()
    };
    let (_agent, server, h) = setup(limits, vec![]).await;
    {
        let mut fs = server.fs.lock().unwrap();
        fs.mkdir("/etc");
        fs.put("/etc/shadow", b"secret");
        fs.symlink("/srv/etc", "/etc");
        fs.symlink("/srv/shadow", "../etc/shadow");
        fs.mkdir("/data");
        fs.put("/data/ok.txt", b"fine");
        fs.symlink("/srv/data", "/data");
    }
    let ct = CancellationToken::new();
    let refused = |r: Result<FileOutput, Error>| matches!(r, Err(Error::Refused(_)));

    assert!(refused(h.service.file("s", file("box", "/srv/../etc/shadow", FileOp::Read), &ct).await));
    assert_eq!(server.connections(), 0, "lexical escape refused before connecting");
    assert!(refused(h.service.file("s", file("box", "/srv/shadow", FileOp::Read), &ct).await));
    assert!(refused(
        h.service.file("s", file("box", "/srv/etc/passwd", write("root::0:0")), &ct).await
    ));
    assert!(refused(h.service.file("s", file("box", "/srv/etc", FileOp::List), &ct).await));
    assert!(server.fs.lock().unwrap().get("/etc/passwd").is_none());
    assert!(refused(h.service.file("s", file("box", "/srv/data/ok.txt", FileOp::Read), &ct).await));
    assert_eq!(server.fs.lock().unwrap().get("/etc/shadow").unwrap(), b"secret");

    let row = last(&h);
    assert_eq!(row.outcome, Outcome::Refused);
    assert_eq!(row.target, "/srv/data/ok.txt -> /data/ok.txt");
    let mut command = bash("box", "pwd");
    command.cwd = Some("/srv/etc".into());
    assert!(matches!(h.service.bash("s", command, &ct).await, Err(Error::Refused(_))));
    assert!(server.stats.commands.lock().unwrap().is_empty());
}

#[tokio::test]
async fn symlinked_allowed_prefix_and_symlinked_file_inside_it() {
    let limits = HostLimits {
        allowed_paths: vec!["/srv/www".into()],
        ..open_limits()
    };
    let (_agent, server, h) = setup(limits, vec![]).await;
    {
        let mut fs = server.fs.lock().unwrap();
        fs.mkdir("/data");
        fs.mkdir("/data/www");
        fs.put("/data/www/real.html", b"v1");
        fs.symlink("/srv/www", "/data/www");
        fs.symlink("/data/www/index.html", "real.html");
    }
    let ct = CancellationToken::new();
    let out = h
        .service
        .file("s", file("box", "/srv/www/index.html", write("v2")), &ct)
        .await
        .unwrap();
    match out {
        FileOutput::Write { path, .. } => assert_eq!(path, "/data/www/real.html"),
        other => panic!("{other:?}"),
    }
    let fs = server.fs.lock().unwrap();
    assert_eq!(fs.get("/data/www/real.html").unwrap(), b"v2");
    assert!(
        matches!(fs.nodes.get("/data/www/index.html"), Some(Node::Symlink(_))),
        "the symlink must stay a symlink"
    );
}

#[tokio::test]
async fn approvals_gate_writes_and_commands() {
    let (_agent, server, h) = setup(
        HostLimits::default(),
        vec![
            Approval::Deny {
                reason: Some("not now".into()),
            },
            Approval::AllowSession,
            Approval::AllowOnce,
        ],
    )
    .await;
    let ct = CancellationToken::new();
    server.fs.lock().unwrap().put("/srv/f", b"orig");

    let err = h.service.file("s1", file("box", "/srv/f", write("A")), &ct).await.unwrap_err();
    assert!(matches!(&err, Error::Denied(Some(r)) if r == "not now"));
    assert_eq!(server.fs.lock().unwrap().get("/srv/f").unwrap(), b"orig");
    assert_eq!(last(&h).outcome, Outcome::Denied);

    h.service.file("s1", file("box", "/srv/f", write("B")), &ct).await.unwrap();
    // Allowed for the session: the edit is not asked again.
    h.service
        .file("s1", file("box", "/srv/f", FileOp::Edit { old: "B".into(), new: "C".into() }), &ct)
        .await
        .unwrap();
    assert_eq!(h.approver.asked(), 2);
    // Commands are a separate class.
    h.service.bash("s1", bash("box", "echo x"), &ct).await.unwrap();
    assert_eq!(h.approver.asked(), 3);
    // Another session is asked again (and the queue is empty: deny).
    assert!(h.service.file("s2", file("box", "/srv/f", write("D")), &ct).await.is_err());
    assert_eq!(h.approver.asked(), 4);
    assert_eq!(server.fs.lock().unwrap().get("/srv/f").unwrap(), b"C");
    {
        let requests = h.approver.requests.lock().unwrap();
        let req = &requests[2];
        assert_eq!((req.action, req.target.as_str()), (Action::Command, "echo x"));
    }
    // Reads never ask.
    h.service.file("s3", file("box", "/srv/f", FileOp::Read), &ct).await.unwrap();
    assert_eq!(h.approver.asked(), 4);
}

#[tokio::test]
async fn oversize_files_are_refused() {
    let limits = HostLimits {
        max_file_bytes: 4,
        ..open_limits()
    };
    let (_agent, server, h) = setup(limits, vec![]).await;
    server.fs.lock().unwrap().put("/srv/big", b"0123456789");
    let ct = CancellationToken::new();
    assert!(matches!(
        h.service.file("s", file("box", "/srv/big", FileOp::Read), &ct).await,
        Err(Error::TooLarge { size: 10, .. })
    ));
    assert!(matches!(
        h.service.file("s", file("box", "/srv/new", write("12345")), &ct).await,
        Err(Error::TooLarge { .. })
    ));
}

#[tokio::test]
async fn in_process_mcp_exposes_tools_and_carries_session_into_audit() {
    use loa_remote::mcp::{REMOTE_BASH, REMOTE_FILE, RemoteMcp};
    use rmcp::model::CallToolRequestParams;
    use rmcp::service::ServiceExt;

    let (_agent, _server, h) = setup(open_limits(), vec![]).await;
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let (server, client) = tokio::join!(
        RemoteMcp::new(Arc::clone(&h.service), "mcp-session").serve_io(server_io),
        ().serve(client_io)
    );
    let _server = server.unwrap();
    let client = client.unwrap();
    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 2);
    for (name, required) in [(REMOTE_BASH, "command"), (REMOTE_FILE, "path")] {
        let tool = tools.iter().find(|tool| tool.name == name).unwrap();
        assert!(tool.input_schema["properties"].get(required).is_some());
    }

    let mut args = CallToolRequestParams::new(REMOTE_BASH);
    args.arguments = serde_json::json!({"host":"box","command":"echo hello"})
        .as_object()
        .cloned();
    let result = client.call_tool(args).await.unwrap();
    assert_ne!(result.is_error, Some(true));
    assert_eq!(result.structured_content.unwrap()["stdout"], "hello\n");

    let mut args = CallToolRequestParams::new(REMOTE_FILE);
    args.arguments = serde_json::json!({"host":"box","op":"write","path":"/srv/mcp","content":"hello"})
        .as_object()
        .cloned();
    let result = client.call_tool(args).await.unwrap();
    assert_ne!(result.is_error, Some(true));

    let rows = h.audit.query(&AuditQuery {
        session_id: Some("mcp-session".into()),
        ..AuditQuery::default()
    }).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].action, rows[1].action), (Action::Write, Action::Command));
}
