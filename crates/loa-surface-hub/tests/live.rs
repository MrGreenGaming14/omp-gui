//! Opt-in test against the installed `omp` with real model calls: a session
//! gets the surface tools as host tools and screenshots a browser surface.
//! Run with `cargo test -p loa-surface-hub --test live -- --ignored`.
//! `LOA_LIVE_MODEL` picks the model (default `anthropic/claude-haiku-5-5`).

use async_trait::async_trait;
use loa_adapter::{Adapter, HostToolContent, HostToolRouter, HostTools, Prompt, SessionConfig};
use loa_adapter_omp::{OmpAdapter, OmpAdapterConfig};
use loa_core::{ElementKind, MemorySink, attr};
use loa_surface_hub::{HubConfig, SurfaceHub, SurfaceTools};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Records which host tools omp called and what came back.
struct Recording {
    inner: Arc<dyn HostTools>,
    calls: Mutex<Vec<(String, String, Vec<HostToolContent>)>>,
}

#[async_trait]
impl HostTools for Recording {
    async fn call(&self, session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        let result = self.inner.call(session_id, tool, args).await;
        let content = result.iter().cloned().map(HostToolContent::Text).collect();
        self.calls.lock().unwrap().push((tool.to_owned(), args.to_owned(), content));
        result
    }

    async fn call_content(&self, session_id: &str, tool: &str, args: &str) -> Result<Vec<HostToolContent>, String> {
        let result = self.inner.call_content(session_id, tool, args).await;
        self.calls.lock().unwrap().push((tool.to_owned(), args.to_owned(), result.clone().unwrap_or_default()));
        result
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp and Chromium with real model calls"]
async fn an_omp_session_screenshots_a_browser_surface() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = HubConfig::discover(&dir.path().join("hub"));
    if let Some(browser) = &mut config.browser {
        browser.width = 640;
        browser.height = 400;
    }
    let hub = SurfaceHub::new(config);
    assert!(hub.browser_executable().is_some(), "Chromium is installed (Playwright cache)");
    let page = "data:text/html,<title>Live</title><body style='margin:0;height:100vh;background:rgb(0,160,0)'><h1 style='color:white'>GREEN PAGE 4417</h1></body>";
    let opened = hub.open_browser(page).await.unwrap();
    let uri = opened.info.uri.as_str().to_owned();

    let tools = SurfaceTools::new(hub.clone());
    let (specs, handler) = tools.host_tools();
    let recording = Arc::new(Recording { inner: handler, calls: Mutex::new(Vec::new()) });
    let mut router = HostToolRouter::new();
    router.add_all(specs, recording.clone()).unwrap();

    let adapter = OmpAdapter::new(OmpAdapterConfig::new("live-test", dir.path().join("state")));
    let sink = Arc::new(MemorySink::new());
    let mut session_config = SessionConfig {
        session_id: "surface-live".into(),
        node: "live-test".into(),
        cwd: dir.path().to_owned(),
        model: Some(std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into())),
        ..Default::default()
    };
    router.install(&mut session_config);
    let session = adapter.start(session_config, sink.clone()).await.expect("omp starts in rpc mode");
    let prompt = format!(
        "Call the surface_screenshot tool with uri {uri}. Then answer with the main background color of the screenshot in one word."
    );
    session.send(Prompt { text: prompt, by: "live-test".into(), ..Default::default() }).await.unwrap();
    tokio::time::timeout(Duration::from_secs(240), session.wait_idle()).await.expect("turn ends in time").unwrap();

    let calls = recording.calls.lock().unwrap().clone();
    let shot = calls.iter().find(|(tool, _, _)| tool == "surface_screenshot").expect("omp called surface_screenshot");
    assert!(shot.1.contains(&uri), "args {}", shot.1);
    assert!(
        shot.2.iter().any(|c| matches!(c, HostToolContent::Image { data, .. } if data.starts_with(b"\x89PNG"))),
        "the result carried the PNG"
    );
    let dom = sink.dom();
    let call = dom.iter().find(|n| n.kind == ElementKind::Call && n.str_attr(attr::TOOL) == Some("surface_screenshot"));
    assert!(call.is_some(), "the call is in the session DOM");
    let reply: String = dom
        .iter()
        .filter(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some("assistant") && n.str_attr(attr::KIND) != Some("thinking"))
        .map(|n| n.text.as_str())
        .collect();
    eprintln!("assistant: {reply}");
    assert!(reply.to_lowercase().contains("green"), "the model saw the page: {reply:?}");
    session.shutdown().await.unwrap();
    hub.shutdown().await;
}
