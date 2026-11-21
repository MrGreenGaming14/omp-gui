//! The pieces the app wires: interactive shells for the terminal pane, the
//! loopback HTTP `remote` server, host tools for omp, and the prompt queue
//! behind host key and approval cards. Against in-process russh servers.

mod support;

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use loa_adapter::{HostTools, McpTransport};
use loa_remote::audit::{Action, AuditLog, AuditQuery, Outcome};
use loa_remote::connection::ConnectorConfig;
use loa_remote::host_tools::{RemoteHostTools, specs};
use loa_remote::hosts::{Host, HostRegistry};
use loa_remote::http::RemoteHttp;
use loa_remote::limits::HostLimits;
use loa_remote::ports::{Approval, Prompt, PromptQueue};
use loa_remote::pty::ShellRead;
use loa_remote::{RemoteService, RemoteServiceConfig, TERMINAL_SESSION};
use support::*;

const WAIT: Duration = Duration::from_secs(10);

fn open_limits() -> HostLimits {
    HostLimits {
        allowed_paths: vec!["/".into()],
        auto_approve_writes: true,
        auto_approve_commands: true,
        ..HostLimits::default()
    }
}

async fn setup(limits: HostLimits) -> (FakeAgent, TestServer, Harness) {
    let dir = tempfile::tempdir().unwrap();
    let agent = FakeAgent::start(dir.path()).await;
    let server = TestServer::start(&agent.public()).await;
    let h = Harness::new(dir, Some(&agent), &server.known_hosts_line(), false, vec![]);
    let mut host = h.add("box", &server);
    host.limits = limits;
    h.hosts.insert(host);
    (agent, server, h)
}

/// Reads output until `needle` shows up.
async fn read_until(reader: &mut loa_remote::pty::ShellReader, needle: &str) -> String {
    let mut seen = String::new();
    tokio::time::timeout(WAIT, async {
        while !seen.contains(needle) {
            match reader.read().await {
                ShellRead::Data(data) => seen.push_str(&String::from_utf8_lossy(&data)),
                ShellRead::Closed { .. } => panic!("closed before {needle:?}; saw {seen:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {needle:?} in {seen:?}"));
    seen
}

#[tokio::test]
async fn a_shell_opens_a_pty_takes_input_resizes_and_reports_its_exit() {
    let (_agent, server, h) = setup(HostLimits::default()).await;
    let shell = h.service.open_shell("box", 120, 40).await.unwrap();
    let (mut reader, mut writer) = (shell.reader, shell.writer);
    assert_eq!(server.stats.ptys.lock().unwrap().as_slice(), [("xterm-256color".to_string(), 120, 40)]);
    read_until(&mut reader, "welcome\r\n$ ").await;

    writer.write(Bytes::from_static(b"ls\r")).await.unwrap();
    read_until(&mut reader, "you said: ls").await;
    writer.resize(80, 24).await.unwrap();
    writer.write(Bytes::from_static(b"exit\r")).await.unwrap();
    let end = tokio::time::timeout(WAIT, async {
        loop {
            if let ShellRead::Closed { exit_status } = reader.read().await {
                return exit_status;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(end, Some(7));
    assert_eq!(reader.read().await, ShellRead::Closed { exit_status: Some(7) }, "stays closed");
    assert_eq!(server.stats.resizes.lock().unwrap().as_slice(), [(80, 24)]);

    let row = h.audit.last().unwrap().unwrap();
    assert_eq!((row.action, row.outcome, row.session_id.as_str()), (Action::Terminal, Outcome::Ok, TERMINAL_SESSION));
    assert!(matches!(h.service.open_shell("nowhere", 80, 24).await, Err(loa_remote::Error::UnknownHost(_))));
}

#[tokio::test]
async fn the_http_server_serves_each_session_by_its_own_token() {
    use loa_mcp::{HttpSpec, Manager, Options, ServerSpec, Transport};

    let (_agent, _server, h) = setup(open_limits()).await;
    let http = RemoteHttp::start(h.service.clone()).await.unwrap();
    let spec = http.session_spec("claude-session").unwrap();
    assert_eq!(spec.name, "remote");
    let McpTransport::Http { url, headers } = spec.transport else { panic!("an HTTP spec") };
    assert_eq!(url, http.url());

    let manager = Manager::new(Options::default()).unwrap();
    let client = ServerSpec::new("remote", Transport::Http(HttpSpec::new(url.clone(), headers).unwrap())).unwrap();
    let tools = manager.probe(&client).await.unwrap();
    let mut names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["remote_bash", "remote_file"]);

    manager.add(client).unwrap();
    let mut status = manager.watch("remote").unwrap();
    tokio::time::timeout(WAIT, status.wait_for(|s| *s == loa_mcp::Status::Running)).await.unwrap().unwrap();
    let args = serde_json::json!({"host": "box", "command": "echo over http"});
    let result = manager
        .call_tool("remote", "remote_bash", args.as_object().cloned(), &loa_mcp::AllowAll)
        .await
        .unwrap();
    assert_eq!(result.structured_content.unwrap()["stdout"], "over http\n");
    let row = h.audit.last().unwrap().unwrap();
    assert_eq!(row.session_id, "claude-session", "the token names the session");

    let stranger = ServerSpec::new(
        "stranger",
        Transport::Http(HttpSpec::new(url.clone(), [("Authorization".to_owned(), "Bearer nope".to_owned())]).unwrap()),
    )
    .unwrap();
    assert!(manager.probe(&stranger).await.is_err(), "an unknown token is refused");
    http.forget("claude-session");
    let revoked = ServerSpec::new("revoked", Transport::Http(HttpSpec::new(url, []).unwrap())).unwrap();
    assert!(manager.probe(&revoked).await.is_err());
    manager.shutdown().await;
    http.shutdown().await;
}

#[tokio::test]
async fn host_tools_run_with_the_same_limits_and_audit() {
    let (_agent, _server, h) = setup(HostLimits { commands: false, ..open_limits() }).await;
    let names: Vec<_> = specs().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["remote_bash", "remote_file"]);
    assert!(specs()[0].input_schema.contains("\"command\""));

    let tools = RemoteHostTools::new(h.service.clone());
    let written = tools
        .call("omp-session", "remote_file", r#"{"host":"box","op":"write","path":"/srv/a.txt","content":"hi"}"#)
        .await
        .unwrap();
    assert!(written.contains("\"op\": \"write\""), "{written}");
    let read = tools
        .call("omp-session", "remote_file", r#"{"host":"box","op":"read","path":"/srv/a.txt"}"#)
        .await
        .unwrap();
    assert!(read.contains("\"content\": \"hi\""), "{read}");
    let refused = tools
        .call("omp-session", "remote_bash", r#"{"host":"box","command":"uptime"}"#)
        .await
        .unwrap_err();
    assert!(refused.contains("commands are not allowed"), "{refused}");
    assert!(tools.call("omp-session", "remote_bash", "[1]").await.is_err());

    let rows = h
        .audit
        .query(&AuditQuery { session_id: Some("omp-session".into()), ..AuditQuery::default() })
        .unwrap();
    let kinds: Vec<_> = rows.iter().map(|r| (r.action, r.outcome)).collect();
    assert_eq!(
        kinds,
        [(Action::Command, Outcome::Refused), (Action::Read, Outcome::Ok), (Action::Write, Outcome::Ok)]
    );
}

#[tokio::test]
async fn host_keys_and_approvals_wait_in_the_prompt_queue() {
    let dir = tempfile::tempdir().unwrap();
    let agent = FakeAgent::start(dir.path()).await;
    let server = TestServer::start(&agent.public()).await;
    let known_hosts = dir.path().join("known_hosts");
    let hosts = Arc::new(HostRegistry::new());
    let mut host = Host::manual("box", "127.0.0.1", Some(server.port()), USER);
    host.limits = HostLimits { allowed_paths: vec!["/".into()], ..HostLimits::default() };
    hosts.insert(host);
    let audit = Arc::new(AuditLog::in_memory().unwrap());
    let prompts = Arc::new(PromptQueue::new());
    let service = Arc::new(RemoteService::new(
        hosts,
        audit,
        prompts.clone(),
        prompts.clone(),
        RemoteServiceConfig {
            known_hosts: vec![known_hosts.clone()],
            known_hosts_write: known_hosts.clone(),
            connector: ConnectorConfig::with_agent(Some(agent.socket.clone())),
        },
    ));
    let tools = RemoteHostTools::new(service);
    let call = tokio::spawn(async move {
        tools.call("s1", "remote_bash", r#"{"host":"box","command":"echo approved"}"#).await
    });

    let mut changes = prompts.subscribe();
    let mut next = async || {
        tokio::time::timeout(WAIT, async {
            loop {
                if let Some(p) = prompts.pending().into_iter().next() {
                    return p;
                }
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap()
    };
    // The approval comes first (before connecting), then the new host key.
    let approval = next().await;
    assert!(matches!(&approval.prompt, Prompt::Approval(r) if r.target == "echo approved" && r.session_id == "s1"));
    assert!(prompts.answer_approval(approval.id, Approval::AllowOnce));
    let key = next().await;
    let Prompt::HostKey(prompt) = &key.prompt else { panic!("a host key prompt, got {key:?}") };
    assert_eq!(prompt.port, server.port());
    assert!(prompts.answer_host_key(key.id, true));

    let out = call.await.unwrap().unwrap();
    assert!(out.contains("approved"), "{out}");
    assert!(std::fs::read_to_string(&known_hosts).unwrap().contains(&format!("[127.0.0.1]:{}", server.port())));
    assert!(prompts.pending().is_empty());
}
