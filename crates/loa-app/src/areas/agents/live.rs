//! A saved agent against the installed omp, with no model call.
//! Run with `cargo test -p loa-app --lib areas::agents::live -- --ignored`.
//!
//! omp refuses to start without a model, so it runs in its own agent
//! directory with one placeholder model that is never called; the user's omp
//! sign-ins and settings are never read. `get_state` in RPC mode reports the
//! system prompt and the tool dump, which list the task agents omp found.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use loa_ui_settings::agents::Local;

use super::tests::{fill, omp_app, open, save_new};
use super::*;

const REPLY: Duration = Duration::from_secs(60);

fn write_models(agent_dir: &Path) {
    let models = serde_json::json!({"providers": {"loa-test": {
        "baseUrl": "http://127.0.0.1:9/v1",
        "apiKey": "LOA_TEST_UNUSED_KEY",
        "api": "openai-completions",
        "models": [{"id": "never-called", "name": "never called", "reasoning": false, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 8192, "maxTokens": 1024}],
    }}});
    std::fs::write(agent_dir.join("models.yml"), serde_json::to_vec_pretty(&models).unwrap()).unwrap();
}

/// Starts omp in RPC mode over `agent_dir` and returns the text of its
/// `get_state` response.
fn omp_state(agent_dir: &Path, cwd: &Path) -> String {
    let mut child = Command::new("omp")
        .args(["--mode", "rpc", "--no-session", "--no-extensions", "--no-skills", "--offline", "--model", "loa-test/never-called"])
        .current_dir(cwd)
        .env("PI_CODING_AGENT_DIR", agent_dir)
        .env("LOA_TEST_UNUSED_KEY", "unused")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("omp on PATH");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (lines, received) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    writeln!(stdin, "{}", serde_json::json!({"type": "get_state", "id": "s1"})).unwrap();
    stdin.flush().unwrap();
    let mut state = None;
    while let Ok(line) = received.recv_timeout(REPLY) {
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if frame["id"] == "s1" {
            assert_eq!(frame["success"], true, "{line}");
            state = Some(line);
            break;
        }
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    state.expect("omp answered get_state")
}

#[test]
#[ignore = "runs the installed omp (needs only `omp` on PATH; no model call, no credentials)"]
fn live_omp_lists_a_saved_agent_and_not_a_disabled_one() {
    let (mut test, agent_dir) = omp_app("omp");
    std::fs::create_dir_all(&agent_dir).unwrap();
    write_models(&agent_dir);

    open(&mut test);
    fill(&mut test, Local::Name("loa-live-enabled".into()));
    fill(&mut test, Local::Description("Saved by the app".into()));
    fill(&mut test, Local::Tool("read".into(), true));
    fill(&mut test, Local::Save);
    test.app.ctx.close_screen();
    save_new(&mut test, "loa-live-disabled", "Saved then disabled");
    fill(&mut test, Local::Toggle);

    let (files, broken) = agents::list(&agent_dir, None);
    assert!(broken.is_empty());
    let enabled = files.iter().find(|file| file.definition.name() == "loa-live-enabled").expect("saved");
    assert_eq!(enabled.definition.tools(), Some(vec!["read".to_owned()]));
    assert!(files.iter().any(|file| file.definition.name() == "loa-live-disabled" && !file.enabled));

    let state = omp_state(&agent_dir, test.dir.path());
    assert!(state.contains("loa-live-enabled"), "omp lists the saved agent");
    assert!(!state.contains("loa-live-disabled"), "omp ignores the disabled agent");
}
