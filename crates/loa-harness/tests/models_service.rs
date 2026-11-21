//! The models service over the real harness accounts: omp's account on a fake omp RPC, a
//! scripted `claude`, a missing `codex` and a temp agent dir: provider rows, logins and the
//! other harnesses' model settings.

use async_trait::async_trait;
use loa_harness::account_support::process::TokioProcessRunner;
use loa_harness::claude::account::ClaudeCode;
use loa_harness::codex::account::Codex;
use loa_harness::omp::account::{OmpAccount, OmpLaunch};
use loa_settings::models::files::OmpFiles;
use loa_settings::models::keys::EnvironmentPresence;
use loa_settings::models::login::{Auth, LoginUpdate, ProviderKey, UrlOpener};
use loa_settings::models::rpc::{self, LoginProvider, LoginUiRequest, UiResponse};
use loa_models::service::{Backend, FilesSource, OMP, Parts};
use loa_settings::models::settings::SettingsValidator;
use loa_models::testing::{FakeOmp, LoginScript};
use loa_util::LockExt;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Accepts every file.
struct Validator;

#[async_trait]
impl SettingsValidator for Validator {
    async fn check_config(&self, _: &str, _: &[(String, serde_json::Value)]) -> loa_settings::models::settings::Result<()> { Ok(()) }
    async fn check_models(&self, _: &str) -> loa_settings::models::settings::Result<()> { Ok(()) }
}

#[derive(Default)]
struct Opener(Mutex<Vec<String>>);

impl UrlOpener for Opener {
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.0.locked().push(url.to_owned());
        Ok(())
    }
}

struct Env(HashSet<&'static str>);

impl EnvironmentPresence for Env {
    fn contains(&self, name: &str) -> bool { self.0.contains(name) }
}

/// A `claude` that is signed out until its login (URL, pasted code) ran.
const CLAUDE: &str = r#"#!/bin/sh
dir=$(dirname "$0")
if [ "$1 $2" = "auth status" ]; then
  if [ -f "$dir/signed-in" ]; then echo '{"loggedIn":true,"authMethod":"claude.ai"}'; exit 0; fi
  echo '{"loggedIn":false}'; exit 1
fi
if [ "$1 $2" = "auth login" ]; then
  echo "Opening https://claude.example/oauth?state=1 in your browser"
  printf 'Paste code here: '
  read code
  touch "$dir/signed-in"
  echo "Login successful for $code"
  exit 0
fi
exit 64
"#;

struct Fixture {
    dir: tempfile::TempDir,
    backend: Backend,
    rpc: Arc<FakeOmp>,
    opener: Arc<Opener>,
}

fn fixture(rpc: FakeOmp, config: &str, models: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name);
    std::fs::write(path("config.yml"), config).unwrap();
    std::fs::write(path("models.yml"), models).unwrap();
    loa_util::exec::write_executable(&path("claude"), CLAUDE).unwrap();
    let (rpc, opener) = (Arc::new(rpc), Arc::new(Opener::default()));
    let claude = ClaudeCode::new(path("claude"), path("claude-settings.json"), Arc::new(TokioProcessRunner));
    let codex = Codex::new(path("no-codex-here"), path("config.toml"), Arc::new(TokioProcessRunner));
    let omp = OmpAccount { rpc: rpc.clone(), opener: opener.clone(), launch: OmpLaunch::default() };
    let backend = Backend::new(Parts {
        rpc: rpc.clone(),
        omp_account: Arc::new(omp),
        files: FilesSource::Fixed(OmpFiles::new(dir.path())),
        validator: Arc::new(Validator),
        opener: opener.clone(),
        harnesses: vec![Arc::new(claude), Arc::new(codex)],
        environment: Arc::new(Env(["GW_KEY", "OPENAI_API_KEY"].into())),
    });
    Fixture { dir, backend, rpc, opener }
}

fn omp_rpc(script: LoginScript) -> FakeOmp {
    let provider = |id: &str, name: &str, available, authenticated| LoginProvider { id: id.into(), name: name.into(), available, authenticated };
    let model = |provider: &str, id: &str| rpc::ModelInfo {
        id: id.into(), name: id.to_uppercase(), provider: provider.into(), api: String::new(), reasoning: false, input: Vec::new(),
        cost: rpc::ModelCost::default(), context_window: None, max_tokens: None, thinking: None, extra: Default::default(),
    };
    let mut rpc = FakeOmp::default();
    rpc.providers = vec![provider("google-antigravity", "Antigravity", false, false), provider("anthropic", "Anthropic (Claude Pro/Max)", true, true), provider("openai-codex", "ChatGPT Plus/Pro (Codex Subscription)", true, false)];
    rpc.models = vec![model("anthropic", "claude-x"), model("openai-codex", "gpt-y")];
    rpc.login_frames = vec![
        LoginUiRequest::OpenUrl { id: "u1".into(), url: "https://auth.openai.example/authorize".into(), launch_url: Some("http://127.0.0.1:1455/l".into()), instructions: Some("Sign in with ChatGPT".into()) },
        LoginUiRequest::Input { id: "u2".into(), title: "Paste the redirect URL".into(), placeholder: None, timeout: None },
    ];
    rpc.script = script;
    rpc
}

/// Runs `key`'s login to its end, answering each prompt with `answer` and cancelling at the URL
/// when `cancel`; returns every update.
async fn login(f: &Fixture, key: &ProviderKey, answer: &str, cancel: bool) -> Vec<LoginUpdate> {
    let mut updates = f.backend.login(key.clone());
    let mut seen = Vec::new();
    while let Some(update) = updates.recv().await {
        match &update {
            LoginUpdate::Url { .. } if cancel => f.backend.cancel(key),
            LoginUpdate::Prompt { .. } => f.backend.submit_input(key, answer.to_owned()),
            _ => {}
        }
        seen.push(update);
    }
    seen
}

#[tokio::test]
async fn providers_list_omp_logins_api_keys_and_other_harnesses() {
    let f = fixture(omp_rpc(LoginScript::AwaitResponse), "", "providers:\n  gw:\n    baseUrl: https://gw.example/v1\n    apiKey: GW_KEY\n");
    let rows = f.backend.providers().await;
    let summary: Vec<(&str, &str, &str)> = rows
        .iter()
        .map(|r| {
            let state = match &r.auth {
                Auth::Login { signed_in: true, .. } => "signed in",
                Auth::Login { signed_in: false, .. } => "signed out",
                Auth::ApiKey { configured_via: Some(_), .. } => "key set",
                Auth::ApiKey { configured_via: None, .. } => "no key",
                Auth::Unavailable { .. } => "unavailable",
            };
            (r.harness_name.as_str(), r.key.provider.as_str(), state)
        })
        .collect();
    let want = [("omp", "anthropic", "signed in"), ("omp", "openai-codex", "signed out"), ("omp", "google-antigravity", "unavailable"), ("omp API keys", "gw", "key set"), ("omp API keys", "openai", "key set"), ("Claude Code", "claude-code", "signed out"), ("Codex", "codex", "unavailable")];
    assert_eq!(summary, want);
    let Auth::ApiKey { configured_via, instructions } = &rows[3].auth else { unreachable!() };
    assert_eq!(configured_via.as_deref(), Some("GW_KEY (models.yml)"), "the key's source by name, never its value");
    assert!(instructions.contains("GW_KEY"), "{instructions}");
    let Auth::Unavailable { reason } = &rows[6].auth else { unreachable!() };
    assert!(reason.contains("codex"), "{reason}");
}

#[tokio::test]
async fn logins_relay_urls_prompts_and_cancellation() {
    let f = fixture(omp_rpc(LoginScript::AwaitResponse), "", "");
    f.backend.providers().await;
    let omp = ProviderKey::new(OMP, "openai-codex");
    let url = LoginUpdate::Url { url: "https://auth.openai.example/authorize".into(), launch_url: Some("http://127.0.0.1:1455/l".into()), instructions: Some("Sign in with ChatGPT".into()) };
    let prompt = LoginUpdate::Prompt { message: "Paste the redirect URL".into(), placeholder: None };
    assert_eq!(login(&f, &omp, "http://127.0.0.1:1455/cb?code=7", false).await, [url, prompt, LoginUpdate::Succeeded { account: None }]);
    assert_eq!(*f.opener.0.locked(), ["https://auth.openai.example/authorize"]);
    assert_eq!(*f.rpc.responses.locked(), [UiResponse::Value { id: "u2".into(), value: "http://127.0.0.1:1455/cb?code=7".into() }]);

    let claude = ProviderKey::new("claude-code", "claude-code");
    let cli = [
        LoginUpdate::Url { url: "https://claude.example/oauth?state=1".into(), launch_url: None, instructions: None },
        LoginUpdate::Prompt { message: "Paste code here:".into(), placeholder: None },
        LoginUpdate::Progress("Login successful for abc".into()),
        LoginUpdate::Succeeded { account: None },
    ];
    assert_eq!(login(&f, &claude, "abc", false).await, cli);
    let rows = f.backend.providers().await;
    assert_eq!(rows.iter().find(|r| r.key == claude).unwrap().auth, Auth::Login { signed_in: true, account: None }, "the CLI's sign in is read again");

    let hanging = fixture(omp_rpc(LoginScript::Hang), "", "");
    hanging.backend.providers().await;
    let seen = login(&hanging, &omp, "", true).await;
    assert!(seen.iter().all(|u| !matches!(u, LoginUpdate::Succeeded { .. } | LoginUpdate::Failed(_))), "{seen:?}");
}

#[tokio::test]
async fn settings_edit_the_other_harnesses_own_configs() {
    let f = fixture(omp_rpc(LoginScript::Hang), "", "");
    std::fs::write(f.dir.path().join("config.toml"), "# codex\nmodel = \"gpt-5\"\n").unwrap();
    let mut settings = f.backend.load_settings().await.unwrap().settings;
    let names: Vec<(&str, &str)> = settings.harnesses.iter().map(|h| (h.name.as_str(), h.model.as_str())).collect();
    assert_eq!(names, [("Claude Code", ""), ("Codex", "gpt-5")]);
    (settings.harnesses[0].model, settings.harnesses[0].effort, settings.harnesses[1].effort) = ("opus".into(), "high".into(), "xhigh".into());
    let saved = f.backend.save_settings(settings).await.unwrap();
    assert_eq!((saved.harnesses[0].model.as_str(), saved.harnesses[0].effort.as_str(), saved.harnesses[1].effort.as_str()), ("opus", "high", "xhigh"));
    let codex = std::fs::read_to_string(f.dir.path().join("config.toml")).unwrap();
    assert_eq!(codex, "# codex\nmodel = \"gpt-5\"\nmodel_reasoning_effort = \"xhigh\"\n");
    let claude = std::fs::read_to_string(f.dir.path().join("claude-settings.json")).unwrap();
    assert!(claude.contains("\"model\": \"opus\""), "{claude}");
    let mut bad = saved;
    bad.harnesses[1].model = "gpt 6".into();
    assert_eq!(f.backend.save_settings(bad).await.unwrap_err()[0].location, "Codex");
}
