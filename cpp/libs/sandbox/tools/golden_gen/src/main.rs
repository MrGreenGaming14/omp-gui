use loa_sandbox::proto::{self, ExecRequest, ExitStatus, Request, Response};
use std::path::PathBuf;

fn golden(dir: PathBuf) {
    std::fs::create_dir_all(&dir).unwrap();
    let w = |name: &str, bytes: Vec<u8>| std::fs::write(dir.join(format!("{name}.bin")), bytes).unwrap();
    w("request_exec_shell", proto::frame(&Request::Exec { id: 1, exec: ExecRequest::shell("printf \"$1\"", &["out"]) }));
    w("request_exec_full", proto::frame(&Request::Exec { id: 300, exec: ExecRequest {
        argv: vec!["tar".into(), "-cf".into(), "-".into()],
        cwd: Some("sub/dir".into()),
        env: vec![("A".into(), "1".into()), ("LONG".into(), "x".repeat(200))],
        stdin: Some((0u8..=255).collect()),
        timeout_ms: Some(30_000_000_000),
    }}));
    w("request_exec_default", proto::frame(&Request::Exec { id: 0, exec: ExecRequest::default() }));
    w("request_shutdown", proto::frame(&Request::Shutdown));
    w("response_ready", proto::frame(&Response::Ready { version: proto::VERSION }));
    w("response_stdout", proto::frame(&Response::Stdout { id: 7, data: b"out\n".to_vec() }));
    w("response_stderr", proto::frame(&Response::Stderr { id: 128, data: vec![0; 300] }));
    w("response_exit_code", proto::frame(&Response::Exit { id: 2, status: ExitStatus { code: Some(-7), signal: None, timed_out: false } }));
    w("response_exit_signal", proto::frame(&Response::Exit { id: 70000, status: ExitStatus { code: None, signal: Some(9), timed_out: true } }));
    w("response_error", proto::frame(&Response::Error { id: u32::MAX, message: "spawn: No such file or directory (os error 2)".into() }));
    let mut hello = Vec::new();
    proto::write_hello(&mut hello).unwrap();
    w("hello", hello);
}

#[tokio::main]
async fn client(stub: PathBuf) {
    use loa_sandbox::provider::{SandboxProvider, SandboxSpec, local::LocalProvider};
    use loa_sandbox::client::StubClient;
    let root = tempfile::tempdir().unwrap();
    let spec = SandboxSpec { name: "x".into(), workspace: root.path().into(), env: Vec::new(), options: Default::default() };
    let mut sandbox = LocalProvider::new(stub).start(&spec).await.unwrap();
    let client = StubClient::connect(sandbox.take_channel().unwrap()).await.unwrap();
    let out = client.exec(ExecRequest::shell("printf \"$1\"; printf err >&2; exit 7", &["out"])).await.unwrap();
    assert_eq!((out.stdout.as_slice(), out.stderr.as_slice(), out.status.code), (&b"out"[..], &b"err"[..], Some(7)));
    let out = client.exec(ExecRequest { timeout_ms: Some(30), ..ExecRequest::shell("while :; do :; done", &[]) }).await.unwrap();
    assert!(out.status.timed_out);
    let err = client.run(ExecRequest { argv: vec!["missing-program".into()], ..Default::default() }).await.unwrap_err();
    assert!(err.to_string().contains("spawn"));
    client.shutdown().await.unwrap();
    sandbox.stop().await.unwrap();
    println!("rust client accepted the stub");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args[1].as_str() {
        "golden" => golden(PathBuf::from(&args[2])),
        "client" => client(PathBuf::from(&args[2])),
        _ => panic!("mode"),
    }
}
