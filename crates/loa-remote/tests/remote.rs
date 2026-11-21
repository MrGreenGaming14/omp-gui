//! End to end: the service, its tools and the prompt queue against in-process russh servers.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use loa_contracts::harness::{HostTools, McpTransport};
use loa_remote::audit::{Action, AuditQuery, AuditRecord, AuditSink, Outcome};
use loa_remote::hosts::{AgentSocket, Host};
use loa_remote::limits::HostLimits;
use loa_remote::ports::{Approval, HostKeyChange, Prompt, PromptQueue};
use loa_remote::pty::ShellRead;
use loa_remote::resolve::Static;
use loa_remote::{BashRequest, EntryKind, Error, ExecEnd, FileOp, FileOutput, FileRequest, RemoteService, TERMINAL_SESSION, WriteMode};
use support::*;
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(10);

fn bash(command: &str) -> BashRequest {
    BashRequest { host: "box".into(), command: command.into(), cwd: None, timeout_secs: None }
}

fn file(path: &str, op: FileOp) -> FileRequest {
    FileRequest { host: "box".into(), path: path.into(), op }
}

fn write(content: &str) -> FileOp {
    FileOp::Write { content: content.into() }
}

fn edit(old: &str, new: &str) -> FileOp {
    FileOp::Edit { old: old.into(), new: new.into() }
}

fn open() -> HostLimits {
    HostLimits { allowed_paths: vec!["/".into()], auto_approve_writes: true, auto_approve_commands: true, ..HostLimits::default() }
}

fn last(rig: &Rig) -> AuditRecord {
    rig.audit.last().unwrap().unwrap()
}

fn ct() -> CancellationToken {
    CancellationToken::new()
}

#[tokio::test]
async fn bash_captures_caps_quotes_reuses_and_audits() {
    let rig = setup(HostLimits { max_output_bytes: 1000, ..open() }).build().await;
    let out = rig.service.bash("s1", bash("echo hello"), &ct()).await.unwrap();
    assert_eq!((out.stdout.as_str(), out.exit_status), ("hello\n", Some(0)));
    let out = rig.service.bash("s1", bash("fail"), &ct()).await.unwrap();
    assert_eq!((out.stderr.as_str(), out.exit_status), ("boom\n", Some(3)));
    let row = last(&rig);
    assert_eq!((row.action, row.outcome, row.target.as_str(), row.session_id.as_str()), (Action::Command, Outcome::Ok, "fail", "s1"));
    assert!(row.detail.starts_with("exit 3"), "{}", row.detail);
    let out = rig.service.bash("s1", bash("big 5000"), &ct()).await.unwrap();
    assert_eq!((out.stdout.len(), out.stdout_dropped), (1000, 4000));
    rig.service.bash("s1", BashRequest { cwd: Some("/srv/it's here".into()), ..bash("pwd") }, &ct()).await.unwrap();
    assert_eq!(rig.server.log.commands().last().unwrap(), "cd '/srv/it'\\''s here' && pwd");
    assert_eq!(rig.server.connections(), 1, "commands reuse the connection");
}

#[tokio::test]
async fn cancel_and_timeout_kill_a_hanging_command() {
    let rig = setup(open()).build().await;
    let (cancel, service) = (ct(), rig.service.clone());
    let task = tokio::spawn({
        let cancel = cancel.clone();
        async move { service.bash("s", bash("hang"), &cancel).await }
    });
    let changed = rig.server.log.changed.notified();
    if rig.server.log.commands().is_empty() {
        changed.await;
    }
    cancel.cancel();
    assert_eq!(task.await.unwrap().unwrap().end, ExecEnd::Cancelled);
    assert_eq!(last(&rig).outcome, Outcome::Cancelled);
    let out = rig.service.bash("s", BashRequest { timeout_secs: Some(1), ..bash("hang") }, &ct()).await.unwrap();
    assert_eq!((out.end, last(&rig).outcome), (ExecEnd::TimedOut, Outcome::TimedOut));
    assert!(rig.server.log.has(&Seen::Signal("KILL".into())));
}

#[tokio::test]
async fn proxyjump_chain_resolves_hop_by_hop_and_tunnels_through_each() {
    let rig = Setup { accept_keys: true, ..setup(open()) }.build().await;
    let inner = TestServer::start(&rig.agent.public()).await;
    let target = TestServer::start(&rig.agent.public()).await;
    let ssh = Static::new(USER).with("outer", "127.0.0.1", rig.server.port, None);
    let ssh = ssh.with("inner", "127.0.0.1", inner.port, Some("outer")).with("target", "127.0.0.1", target.port, Some("inner"));
    let entries: BTreeMap<_, _> = [("target".to_owned(), loa_remote::config::entry_for_alias("target"))].into();
    let (hosts, failed) = loa_remote::config::load(&entries, &ssh);
    assert!(failed.is_empty());
    rig.hosts.insert(Host { limits: open(), ..hosts[0].clone() });
    let out = rig.service.bash("s", BashRequest { host: "target".into(), ..bash("echo through") }, &ct()).await.unwrap();
    assert_eq!(out.stdout, "through\n");
    assert!(rig.server.log.has(&Seen::Forward("127.0.0.1".into(), inner.port.into())) && rig.server.log.commands().is_empty());
    assert!(inner.log.has(&Seen::Forward("127.0.0.1".into(), target.port.into())));
    assert_eq!(target.log.commands(), ["echo through"]);
    let prompted: Vec<u16> = rig.decider.1.lock().unwrap().iter().map(|p| p.port).collect();
    assert_eq!(prompted, [inner.port, target.port], "only the unknown hops are asked about");
}

#[tokio::test]
async fn new_host_keys_are_asked_once_and_changed_ones_refused() {
    let rig = Setup { known: Some(String::new()), accept_keys: true, ..setup(open()) }.build().await;
    rig.service.bash("s", bash("echo 1"), &ct()).await.unwrap();
    assert_eq!(std::fs::read_to_string(&rig.known_hosts).unwrap(), rig.server.known_hosts_line());
    rig.service.disconnect("box").await;
    rig.service.bash("s", bash("echo 2"), &ct()).await.unwrap();
    let prompts = rig.decider.1.lock().unwrap().clone();
    assert_eq!((rig.server.connections(), prompts.len(), prompts[0].change.clone()), (2, 1, HostKeyChange::New));
    assert_eq!(prompts[0].fingerprint, loa_remote::known_hosts::fingerprint(rig.server.host_key.public_key().key_data()));

    let old = random_key();
    let rig = setup(open()).build().await;
    let known = format!("[127.0.0.1]:{} {}\n", rig.server.port, old.public_key().to_openssh().unwrap());
    std::fs::write(&rig.known_hosts, &known).unwrap();
    let err = rig.service.bash("s", bash("echo pwned"), &ct()).await.unwrap_err();
    assert!(matches!(&err, Error::HostKeyRejected { reason, .. } if reason.contains("CHANGED")), "{err}");
    let HostKeyChange::Changed { previous } = rig.decider.1.lock().unwrap()[0].change.clone() else { panic!("not flagged as changed") };
    assert_eq!(previous[0].fingerprint, loa_remote::known_hosts::fingerprint(old.public_key().key_data()));
    assert!(rig.server.log.commands().is_empty());
    assert_eq!((std::fs::read_to_string(&rig.known_hosts).unwrap(), last(&rig).outcome), (known, Outcome::Error));
}

#[tokio::test]
async fn only_agent_identities_authenticate() {
    let rig = Setup { authorized: false, ..setup(open()) }.build().await;
    let err = rig.service.bash("s", bash("echo"), &ct()).await.unwrap_err();
    assert!(matches!(err, Error::AuthFailed { tried: 1, .. }), "{err}");
    let rig = Setup { agent: false, ..setup(open()) }.build().await;
    assert!(matches!(rig.service.bash("s", bash("echo"), &ct()).await, Err(Error::NoAgent { .. })));
    // IdentityAgent wins over the (missing) default socket.
    let mut host = rig.hosts.get("box").unwrap();
    host.target.agent = AgentSocket::Path(rig.agent.socket.clone());
    rig.hosts.insert(host);
    assert_eq!(rig.service.bash("s", bash("echo ok"), &ct()).await.unwrap().stdout, "ok\n");
}

#[tokio::test]
async fn files_write_atomically_then_edit_read_and_list() {
    let rig = setup(open()).build().await;
    rig.server.fs.lock().unwrap().put("/srv/app.conf", b"port = 1\nname = a\n");
    let out = rig.service.file("s", file("/srv/app.conf", write("port = 2\nname = a\n")), &ct()).await.unwrap();
    assert!(matches!(out, FileOutput::Write { mode: WriteMode::Atomic, bytes: 18, .. }));
    assert_eq!(rig.server.fs.lock().unwrap().0.range("/srv/".to_owned()..).collect::<Vec<_>>(), [(&"/srv/app.conf".to_owned(), &Node::File(b"port = 2\nname = a\n".to_vec(), 0o640))]);
    rig.service.file("s", file("/srv/app.conf", edit("port = 2", "port = 3")), &ct()).await.unwrap();
    for (old, error) in [(" = ", "occurs 2 times"), ("missing", "not found")] {
        let err = rig.service.file("s", file("/srv/app.conf", edit(old, "x")), &ct()).await.unwrap_err();
        assert!(err.to_string().contains(error), "{err}");
    }
    rig.server.fs.lock().unwrap().put("/srv/blob", &[0xff, 0x00, 0xfe]);
    rig.server.fs.lock().unwrap().0.insert("/srv/sub".into(), Node::Dir);
    for (path, want) in [("/srv/app.conf", ("port = 3\nname = a\n", "utf8")), ("/srv/blob", ("/wD+", "base64"))] {
        let FileOutput::Read { content, encoding, .. } = rig.service.file("s", file(path, FileOp::Read), &ct()).await.unwrap() else { panic!() };
        assert_eq!((content.as_str(), encoding), want);
    }
    let FileOutput::List { entries, .. } = rig.service.file("s", file("/srv", FileOp::List), &ct()).await.unwrap() else { panic!() };
    let got: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(got, [("app.conf", EntryKind::File), ("blob", EntryKind::File), ("sub", EntryKind::Dir)]);
    assert_eq!(rig.server.connections(), 1);

    // Without posix-rename a new file is still renamed into place, an existing one is rewritten.
    let rig = Setup { posix_rename: false, ..setup(open()) }.build().await;
    for (content, mode) in [("a", WriteMode::Atomic), ("bb", WriteMode::InPlace)] {
        let out = rig.service.file("s", file("/srv/new", write(content)), &ct()).await.unwrap();
        assert!(matches!(out, FileOutput::Write { mode: m, .. } if m == mode), "{out:?}");
    }
    assert_eq!(rig.server.fs.lock().unwrap().0.range("/srv/".to_owned()..).count(), 1);
    assert_eq!(rig.server.fs.lock().unwrap().get("/srv/new"), Some(&b"bb"[..]));
}

#[tokio::test]
async fn limits_refuse_before_connecting_and_every_refusal_is_audited() {
    let read_only = HostLimits { read_only: true, ..open() };
    let no_commands = HostLimits { commands: false, ..open() };
    let small = HostLimits { max_file_bytes: 4, ..open() };
    let srv = HostLimits { allowed_paths: vec!["/srv".into()], ..open() };
    let (all, none) = (open(), HostLimits::default());
    let unknown = |r: FileRequest| FileRequest { host: "nope".into(), ..r };
    let cases: Vec<(&HostLimits, Result<BashRequest, FileRequest>, Action)> = vec![
        (&read_only, Err(file("/srv/x", write("x"))), Action::Write),
        (&read_only, Ok(bash("echo")), Action::Command),
        (&no_commands, Ok(bash("echo")), Action::Command),
        (&all, Ok(BashRequest { host: "nope".into(), ..bash("echo") }), Action::Command),
        (&all, Err(unknown(file("/srv/x", FileOp::Read))), Action::Read),
        (&small, Err(file("/srv/new", write("12345"))), Action::Write),
        (&srv, Err(file("/srv/../etc/shadow", FileOp::Read)), Action::Read),
        (&srv, Ok(BashRequest { cwd: Some("/etc".into()), ..bash("pwd") }), Action::Command),
        (&none, Err(file("/srv/x", FileOp::List)), Action::List),
    ];
    for (i, (limits, request, action)) in cases.into_iter().enumerate() {
        let rig = setup(limits.clone()).build().await;
        let err = match request {
            Ok(r) => rig.service.bash("s9", r, &ct()).await.map(drop),
            Err(r) => rig.service.file("s9", r, &ct()).await.map(drop),
        };
        let row = last(&rig);
        let refused = matches!(err, Err(Error::Refused(_) | Error::UnknownHost(_) | Error::TooLarge { .. }));
        assert!(refused && row.session_id == "s9" && row.action == action, "case {i}: {err:?} {row:?}");
        assert_eq!(rig.server.connections(), 0, "case {i}: nothing may be sent");
    }
}

#[tokio::test]
async fn symlinks_cannot_escape_the_allowed_paths() {
    let rig = setup(HostLimits { allowed_paths: vec!["/srv".into(), "/www".into()], ..open() }).build().await;
    {
        let mut fs = rig.server.fs.lock().unwrap();
        for dir in ["/etc", "/data", "/data/www"] {
            fs.0.insert(dir.into(), Node::Dir);
        }
        fs.put("/etc/shadow", b"secret");
        fs.put("/data/ok.txt", b"fine");
        fs.put("/data/www/real.html", b"v1");
        for (link, to) in [("/srv/etc", "/etc"), ("/srv/shadow", "../etc/shadow"), ("/srv/data", "/data"), ("/www", "/data/www"), ("/data/www/index.html", "real.html")] {
            fs.link(link, to);
        }
    }
    for (path, op) in [("/srv/shadow", FileOp::Read), ("/srv/etc/passwd", write("root::0:0")), ("/srv/etc", FileOp::List), ("/srv/data/ok.txt", FileOp::Read)] {
        let result = rig.service.file("s", file(path, op), &ct()).await;
        assert!(matches!(result, Err(Error::Refused(_))), "{path}: {result:?}");
    }
    assert_eq!(last(&rig).target, "/srv/data/ok.txt -> /data/ok.txt");
    assert!(matches!(rig.service.bash("s", BashRequest { cwd: Some("/srv/etc".into()), ..bash("pwd") }, &ct()).await, Err(Error::Refused(_))));
    assert!(rig.server.log.commands().is_empty() && rig.server.fs.lock().unwrap().get("/etc/passwd").is_none());
    // An allowed prefix that is itself a symlink, and a link inside it, are followed.
    let out = rig.service.file("s", file("/www/index.html", write("v2")), &ct()).await.unwrap();
    assert!(matches!(&out, FileOutput::Write { path, .. } if path == "/data/www/real.html"), "{out:?}");
    let fs = rig.server.fs.lock().unwrap();
    assert_eq!((fs.get("/data/www/real.html"), fs.0.get("/data/www/index.html")), (Some(&b"v2"[..]), Some(&Node::Symlink("real.html".into()))));
}

#[tokio::test]
async fn approvals_gate_writes_and_commands_per_session_and_class() {
    let limits = HostLimits { allowed_paths: vec!["/".into()], ..HostLimits::default() };
    let approvals = vec![Approval::Deny { reason: Some("not now".into()) }, Approval::AllowSession, Approval::AllowOnce];
    let rig = Setup { approvals, ..setup(limits) }.build().await;
    rig.server.fs.lock().unwrap().put("/srv/f", b"orig");
    let steps: [(&str, Result<BashRequest, FileOp>, bool, usize); 6] = [
        ("s1", Err(write("A")), false, 1),
        ("s1", Err(write("B")), true, 2),
        ("s1", Err(edit("B", "C")), true, 2),
        ("s1", Ok(bash("echo x")), true, 3),
        ("s2", Err(write("D")), false, 4),
        ("s3", Err(FileOp::Read), true, 4),
    ];
    for (i, (session, request, ok, asked)) in steps.into_iter().enumerate() {
        let result = match request {
            Ok(r) => rig.service.bash(session, r, &ct()).await.map(drop),
            Err(op) => rig.service.file(session, file("/srv/f", op), &ct()).await.map(drop),
        };
        assert_eq!((result.is_ok(), rig.approver.1.lock().unwrap().len()), (ok, asked), "step {i}: {result:?}");
    }
    assert_eq!(rig.server.fs.lock().unwrap().get("/srv/f"), Some(&b"C"[..]));
    let denied = rig.audit.query(&AuditQuery { outcome: Some(Outcome::Denied), ..AuditQuery::default() }).unwrap();
    assert_eq!(denied.iter().map(|r| (r.session_id.as_str(), r.detail.as_str())).collect::<Vec<_>>(), [("s2", "denied by the user"), ("s1", "denied by the user: not now")]);
    let request = rig.approver.1.lock().unwrap()[2].clone();
    assert_eq!((request.action, request.target.as_str()), (Action::Command, "echo x"));
}

#[tokio::test]
async fn shells_open_a_pty_take_input_resize_and_report_their_exit() {
    let rig = setup(HostLimits::default()).build().await;
    let shell = rig.service.open_shell("box", 120, 40).await.unwrap();
    let (mut reader, mut writer) = (shell.reader, shell.writer);
    let mut seen = String::new();
    let mut until = async |needle: &str| {
        while !seen.contains(needle) {
            match tokio::time::timeout(WAIT, reader.read()).await.unwrap() {
                ShellRead::Data(data) => seen.push_str(&String::from_utf8_lossy(&data)),
                ShellRead::Closed { exit_status } => return exit_status,
            }
        }
        None
    };
    until("welcome\r\n$ ").await;
    writer.write(Bytes::from_static(b"ls\r")).await.unwrap();
    until("you said: ls").await;
    writer.resize(80, 24).await.unwrap();
    writer.write(Bytes::from_static(b"exit\r")).await.unwrap();
    assert_eq!(until("never").await, Some(7));
    assert_eq!(reader.read().await, ShellRead::Closed { exit_status: Some(7) }, "stays closed");
    assert!(rig.server.log.has(&Seen::Pty("xterm-256color".into(), 120, 40)) && rig.server.log.has(&Seen::Resize(80, 24)));
    let row = last(&rig);
    assert_eq!((row.action, row.outcome, row.session_id.as_str()), (Action::Terminal, Outcome::Ok, TERMINAL_SESSION));
    assert!(matches!(rig.service.open_shell("nowhere", 80, 24).await, Err(Error::UnknownHost(_))));
}

#[tokio::test]
async fn tools_serve_host_tool_calls_and_http_sessions_with_the_same_audit() {
    use loa_mcp::{AllowAll, HttpSpec, Manager, Options, ServerSpec, Status, Transport};
    let rig = setup(HostLimits { commands: false, ..open() }).build().await;
    let tools = loa_remote::tools::tool_set(rig.service.clone());
    assert_eq!(tools.specs().iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["remote_bash", "remote_file"]);
    let calls = [
        ("remote_file", r#"{"host":"box","op":"write","path":"/srv/a.txt","content":"aGk=","encoding":"base64"}"#, Ok("\"op\": \"write\"")),
        ("remote_file", r#"{"host":"box","op":"read","path":"/srv/a.txt"}"#, Ok("\"content\": \"hi\"")),
        ("remote_file", r#"{"host":"box","op":"edit","path":"/srv/a.txt","old":"h"}"#, Err("`new` is required for op `edit`")),
        ("remote_bash", r#"{"host":"box","command":"uptime"}"#, Err("commands are not allowed")),
        ("remote_bash", "[1]", Err("invalid arguments")),
    ];
    for (tool, args, want) in calls {
        let got = HostTools::call(&tools, "omp-session", tool, args).await;
        assert!(match (&got, want) { (Ok(g), Ok(w)) | (Err(g), Err(w)) => g.contains(w), _ => false }, "{tool} {args}: {got:?}");
    }
    let rows = rig.audit.query(&AuditQuery { session_id: Some("omp-session".into()), ..AuditQuery::default() }).unwrap();
    let kinds: Vec<_> = rows.iter().map(|r| (r.action, r.outcome)).collect();
    assert_eq!(kinds, [(Action::Command, Outcome::Refused), (Action::Read, Outcome::Ok), (Action::Write, Outcome::Ok)]);

    let rig = setup(open()).build().await;
    let http = loa_remote::http::RemoteHttp::start(loa_remote::tools::tool_set(rig.service.clone())).await.unwrap();
    let McpTransport::Http { url, headers } = http.session_spec("claude-session").unwrap().transport else { panic!("an HTTP spec") };
    let manager = Manager::new(Options::default()).unwrap();
    manager.add(ServerSpec::new("remote", Transport::Http(HttpSpec::new(url.clone(), headers).unwrap())).unwrap()).unwrap();
    let mut status = manager.watch("remote").unwrap();
    tokio::time::timeout(WAIT, status.wait_for(|s| *s == Status::Running)).await.unwrap().unwrap();
    let args = serde_json::json!({"host": "box", "command": "echo over http"});
    let result = manager.call_tool("remote", "remote_bash", args.as_object().cloned(), &AllowAll).await.unwrap();
    assert_ne!(result.is_error, Some(true));
    assert_eq!(last(&rig).session_id, "claude-session", "the token names the session");
    http.forget("claude-session");
    let stranger = [("Authorization".to_owned(), "Bearer nope".to_owned())];
    assert!(manager.probe(&ServerSpec::new("x", Transport::Http(HttpSpec::new(url, stranger).unwrap())).unwrap()).await.is_err());
    manager.shutdown().await;
    http.shutdown().await;
}

#[tokio::test]
async fn the_prompt_queue_asks_for_the_approval_then_the_new_host_key() {
    let rig = Setup { known: Some(String::new()), ..setup(HostLimits { allowed_paths: vec!["/".into()], ..HostLimits::default() }) }.build().await;
    let prompts = Arc::new(PromptQueue::new());
    let config = loa_remote::RemoteServiceConfig {
        known_hosts: vec![rig.known_hosts.clone()],
        known_hosts_write: rig.known_hosts.clone(),
        connector: loa_remote::connection::ConnectorConfig::with_agent(Some(rig.agent.socket.clone())),
    };
    let service = Arc::new(RemoteService::new(rig.hosts.clone(), rig.audit.clone(), prompts.clone(), prompts.clone(), config));
    let tools = loa_remote::tools::tool_set(service);
    let call = tokio::spawn(async move { HostTools::call(&tools, "s1", "remote_bash", r#"{"host":"box","command":"echo approved"}"#).await });
    let mut changes = prompts.subscribe();
    let mut next = async || loop {
        if let Some(p) = prompts.pending().into_iter().next() {
            return p;
        }
        tokio::time::timeout(WAIT, changes.changed()).await.unwrap().unwrap();
    };
    let approval = next().await;
    assert!(matches!(&approval.prompt, Prompt::Approval(r) if r.target == "echo approved" && r.session_id == "s1"));
    assert!(prompts.answer(approval.id, Approval::AllowOnce));
    let key = next().await;
    assert!(matches!(&key.prompt, Prompt::HostKey(p) if p.port == rig.server.port));
    assert!(prompts.answer(key.id, true));
    assert!(call.await.unwrap().unwrap().contains("approved"));
    assert!(std::fs::read_to_string(&rig.known_hosts).unwrap().contains(&format!("[127.0.0.1]:{}", rig.server.port)));
    assert!(prompts.pending().is_empty());
}
