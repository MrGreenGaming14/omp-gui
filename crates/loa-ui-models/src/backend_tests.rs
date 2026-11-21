// Service integration tests use the actual model screens.
#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use async_trait::async_trait;
    use loa_util::LockExt;
    use loa_harness::claude::account::ClaudeCode;
    use loa_harness::codex::account::Codex;
    use loa_models::keys::EnvironmentPresence;
    use loa_models::login::{Auth, LoginUpdate, ProviderKey, UrlOpener};
    use loa_harness::omp::account::OmpFiles;
    use loa_harness::account_support::process::TokioProcessRunner;
    use loa_contracts::harness::model_rpc::{self as rpc, LoginProvider, LoginUiRequest, UiResponse};
    use loa_models::service::{Backend, Parts, OMP};
    use loa_models::settings::RoleChain;
    use loa_models::testing::{FakeOmp as FakeRpc, LoginScript};
    use loa_contracts::harness::model_error::SettingsValidator;
    use crate::ModelRef;
    use crate::signin;

    fn model(provider: &str, id: &str) -> rpc::ModelInfo {
        rpc::ModelInfo {
            id: id.into(), name: id.to_uppercase(), provider: provider.into(),
            api: String::new(), reasoning: false, input: Vec::new(),
            cost: rpc::ModelCost::default(), context_window: None, max_tokens: None,
            thinking: None, extra: Default::default(),
        }
    }


    /// Accepts every file unless it contains `reject-me`.
    struct Validator;
    #[async_trait]
    impl SettingsValidator for Validator {
        async fn check_config(&self, text: &str, _: &[(String, serde_json::Value)]) -> loa_contracts::harness::model_error::Result<()> {
            if text.contains("reject-me") {
                return Err(loa_contracts::harness::model_error::Error::Rejected { what: "config.yml".into(), message: "bad role".into() });
            }
            Ok(())
        }
        async fn check_models(&self, _: &str) -> loa_contracts::harness::model_error::Result<()> {
            Ok(())
        }
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
        fn contains(&self, name: &str) -> bool {
            self.0.contains(name)
        }
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        loa_util::exec::write_executable(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        path
    }

    /// A `claude` that is signed out until its login (URL, pasted code) ran.
    fn fake_claude(dir: &Path) -> ClaudeCode {
        let program = script(
            dir,
            "claude",
            r#"dir=$(dirname "$0")
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
exit 64"#,
        );
        ClaudeCode::new(program, dir.join("claude-settings.json"), Arc::new(TokioProcessRunner))
    }

    fn provider(id: &str, name: &str, available: bool, authenticated: bool) -> LoginProvider {
        LoginProvider { id: id.into(), name: name.into(), available, authenticated }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        backend: Backend,
        rpc: Arc<FakeRpc>,
        opener: Arc<Opener>,
    }

    fn fixture(rpc: FakeRpc, config: &str, models: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yml"), config).unwrap();
        std::fs::write(dir.path().join("models.yml"), models).unwrap();
        let rpc = Arc::new(rpc);
        let opener = Arc::new(Opener::default());
        let codex = Codex::new(dir.path().join("no-codex-here"), dir.path().join("config.toml"), Arc::new(TokioProcessRunner));
        let backend = Backend::new(Parts {
            rpc: rpc.clone(),
            files: OmpFiles::new(dir.path()),
            validator: Arc::new(Validator),
            opener: opener.clone(),
            harnesses: vec![Arc::new(fake_claude(dir.path())), Arc::new(codex)],
            environment: Arc::new(Env(["GW_KEY", "OPENAI_API_KEY"].into())),
        });
        Fixture { dir, backend, rpc, opener }
    }

    fn omp_rpc() -> FakeRpc {
        let mut rpc = FakeRpc::default();
        rpc.providers = vec![
                provider("google-antigravity", "Antigravity", false, false),
                provider("anthropic", "Anthropic (Claude Pro/Max)", true, true),
                provider("openai-codex", "ChatGPT Plus/Pro (Codex Subscription)", true, false),
            ];
        rpc.models = vec![model("anthropic", "claude-x"), model("openai-codex", "gpt-y")];
        rpc.login_frames = vec![
                LoginUiRequest::OpenUrl {
                    id: "u1".into(),
                    url: "https://auth.openai.example/authorize".into(),
                    launch_url: Some("http://127.0.0.1:1455/l".into()),
                    instructions: Some("Sign in with ChatGPT".into()),
                },
                LoginUiRequest::Input {
                    id: "u2".into(),
                    title: "Paste the redirect URL".into(),
                    placeholder: None,
                    timeout: None,
                },
            ];
        rpc.script = LoginScript::AwaitResponse;
        rpc
    }

    #[tokio::test]
    async fn providers_list_omp_logins_api_keys_and_other_harnesses() {
        let f = fixture(omp_rpc(), "", "providers:\n  gw:\n    baseUrl: https://gw.example/v1\n    apiKey: GW_KEY\n");
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
        assert_eq!(
            summary,
            [
                ("omp", "anthropic", "signed in"),
                ("omp", "openai-codex", "signed out"),
                ("omp", "google-antigravity", "unavailable"),
                ("omp API keys", "gw", "key set"),
                ("omp API keys", "openai", "key set"),
                ("Claude Code", "claude", "signed out"),
                ("Codex", "codex", "unavailable"),
            ]
        );
        let Auth::ApiKey { configured_via, instructions } = &rows[3].auth else { unreachable!() };
        assert_eq!(configured_via.as_deref(), Some("GW_KEY (models.yml)"));
        assert!(instructions.contains("GW_KEY"), "{instructions}");
        let Auth::Unavailable { reason } = &rows[6].auth else { unreachable!() };
        assert!(reason.contains("codex"), "{reason}");
    }

    #[tokio::test]
    async fn the_sign_in_screen_runs_an_omp_oauth_login_to_success() {
        let f = fixture(omp_rpc(), "", "");
        let mut screen = signin::State::new(f.backend.providers().await);
        let key = ProviderKey::new(OMP, "openai-codex");
        let (_, event) = screen.update(signin::Message::SignIn(key.clone()));
        assert_eq!(event, Some(signin::Event::SignIn(key.clone())));

        let mut updates = f.backend.login(key.clone());
        let mut seen = Vec::new();
        while let Some(update) = updates.recv().await {
            screen.login_update(&key, update.clone());
            if let LoginUpdate::Prompt { .. } = update {
                let _ = screen.update(signin::Message::PromptInput(key.clone(), "http://127.0.0.1:1455/cb?code=7".into()));
                let (_, event) = screen.update(signin::Message::SubmitPrompt(key.clone()));
                let Some(signin::Event::SubmitInput { key, text }) = event else { panic!("{event:?}") };
                f.backend.submit_input(&key, text);
            }
            seen.push(update);
        }
        assert_eq!(
            seen,
            [
                LoginUpdate::Url {
                    url: "https://auth.openai.example/authorize".into(),
                    launch_url: Some("http://127.0.0.1:1455/l".into()),
                    instructions: Some("Sign in with ChatGPT".into()),
                },
                LoginUpdate::Prompt { message: "Paste the redirect URL".into(), placeholder: None },
                LoginUpdate::Succeeded { account: None },
            ]
        );
        assert_eq!(*f.opener.0.locked(), ["https://auth.openai.example/authorize"]);
        assert_eq!(
            *f.rpc.responses.locked(),
            [UiResponse::Value { id: "u2".into(), value: "http://127.0.0.1:1455/cb?code=7".into() }]
        );
        assert_eq!(screen.login_state(&key), &signin::LoginState::Succeeded { account: None });
        assert!(matches!(
            screen.providers().iter().find(|p| p.key == key).unwrap().auth,
            Auth::Login { signed_in: true, .. }
        ));
    }

    #[tokio::test]
    async fn cancelling_an_omp_login_ends_its_updates() {
        let mut rpc = omp_rpc();
        rpc.script = LoginScript::Hang;
        let f = fixture(rpc, "", "");
        f.backend.providers().await;
        let key = ProviderKey::new(OMP, "openai-codex");
        let mut updates = f.backend.login(key.clone());
        let mut seen = Vec::new();
        while let Some(update) = updates.recv().await {
            if matches!(update, LoginUpdate::Url { .. }) {
                f.backend.cancel(&key);
            }
            seen.push(update);
        }
        assert!(seen.iter().all(|u| !matches!(u, LoginUpdate::Succeeded { .. } | LoginUpdate::Failed(_))), "{seen:?}");
    }

    #[tokio::test]
    async fn a_harness_login_runs_its_cli_and_checks_the_sign_in_again() {
        let f = fixture(omp_rpc(), "", "");
        let key = ProviderKey::new("claude", "claude");
        let mut updates = f.backend.login(key.clone());
        let mut seen = Vec::new();
        while let Some(update) = updates.recv().await {
            if matches!(update, LoginUpdate::Prompt { .. }) {
                f.backend.submit_input(&key, "abc".into());
            }
            seen.push(update);
        }
        assert_eq!(
            seen,
            [
                LoginUpdate::Url { url: "https://claude.example/oauth?state=1".into(), launch_url: None, instructions: None },
                LoginUpdate::Prompt { message: "Paste code here:".into(), placeholder: None },
                LoginUpdate::Progress("Login successful for abc".into()),
                LoginUpdate::Succeeded { account: None },
            ]
        );
        let rows = f.backend.providers().await;
        let claude = rows.iter().find(|r| r.key == key).unwrap();
        assert_eq!(claude.auth, Auth::Login { signed_in: true, account: None });
    }

    #[tokio::test]
    async fn the_catalog_and_role_assignment_use_omp_config() {
        let f = fixture(omp_rpc(), "modelRoles:\n  smol: anthropic/claude-x:low # fast\n", "");
        let catalog = f.backend.catalog().await.unwrap();
        assert_eq!(catalog.models.len(), 2);
        assert_eq!(catalog.models[0].provider_name, "Anthropic (Claude Pro/Max)");
        assert_eq!(catalog.roles[0].model, ModelRef::new("anthropic", "claude-x"));

        f.backend.assign_role("smol".into(), ModelRef::new("openai-codex", "gpt-y")).await.unwrap();
        let text = std::fs::read_to_string(f.dir.path().join("config.yml")).unwrap();
        assert_eq!(text, "modelRoles:\n  smol: openai-codex/gpt-y:low # fast\n");
        let catalog = f.backend.catalog().await.unwrap();
        assert_eq!(catalog.roles[0].model, ModelRef::new("openai-codex", "gpt-y"));
    }

    #[tokio::test]
    async fn catalog_resolves_role_aliases_and_ignores_unavailable_models() {
        let f = fixture(omp_rpc(), "modelRoles:\n  fast: anthropic/claude-x\n  alias: '@fast'\n  missing: absent/model\n", "");
        let catalog = f.backend.catalog().await.unwrap();
        let alias = catalog.roles.iter().find(|role| role.role == "alias").unwrap();
        assert_eq!(alias.model, ModelRef::new("anthropic", "claude-x"));
        assert!(catalog.roles.iter().all(|role| role.role != "missing"));
    }

    #[tokio::test]
    async fn the_settings_screen_saves_through_omp_validation() {
        let config = "# keep me\nmodelRoles:\n  default: anthropic/claude-x\n";
        let f = fixture(omp_rpc(), config, "");
        let load = f.backend.load_settings().await.unwrap();
        assert_eq!(load.known_models, ["anthropic/claude-x", "openai-codex/gpt-y"]);
        let mut screen = crate::settings::State::new(load.settings, load.known_models);
        let _ = screen.update(crate::settings::Message::AddChainEntry(0, "openai-codex/gpt-y".into()));
        let (_, event) = screen.update(crate::settings::Message::Save);
        let Some(crate::settings::Event::Save(settings)) = event else { panic!("{event:?}") };
        let saved = f.backend.save_settings(settings).await.unwrap();
        screen.saved(saved.clone());
        assert!(!screen.is_dirty());
        assert_eq!(
            saved.roles,
            [RoleChain { role: "default".into(), chain: vec!["anthropic/claude-x".into(), "openai-codex/gpt-y".into()] }]
        );
        let text = std::fs::read_to_string(f.dir.path().join("config.yml")).unwrap();
        assert!(text.starts_with("# keep me\n"), "{text}");
        assert!(text.contains("fallbackChains:\n    default:\n      - openai-codex/gpt-y\n"), "{text}");

        // omp's rejection is shown and nothing is written.
        let _ = screen.update(crate::settings::Message::NewRoleInput("reject-me".into()));
        let _ = screen.update(crate::settings::Message::AddRole);
        let _ = screen.update(crate::settings::Message::AddChainEntry(1, "anthropic/claude-x".into()));
        let (_, event) = screen.update(crate::settings::Message::Save);
        let Some(crate::settings::Event::Save(settings)) = event else { panic!("{event:?}") };
        let errors = f.backend.save_settings(settings).await.unwrap_err();
        assert_eq!(errors[0].location, "config.yml");
        screen.set_errors(errors);
        assert_eq!(std::fs::read_to_string(f.dir.path().join("config.yml")).unwrap(), text);

        // A change made outside the app since the load is not overwritten.
        std::fs::write(f.dir.path().join("config.yml"), "modelRoles: {}\n").unwrap();
        let mut settings = saved.clone();
        settings.roles[0].chain.pop();
        let errors = f.backend.save_settings(settings).await.unwrap_err();
        assert!(errors[0].message.contains("changed on disk"), "{errors:?}");
    }

    #[tokio::test]
    async fn the_settings_screen_edits_the_other_harnesses_own_configs() {
        let f = fixture(omp_rpc(), "", "");
        std::fs::write(f.dir.path().join("config.toml"), "# codex\nmodel = \"gpt-5\"\n").unwrap();
        let load = f.backend.load_settings().await.unwrap();
        let names: Vec<&str> = load.settings.harnesses.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, ["Claude Code", "Codex"]);
        assert_eq!(load.settings.harnesses[1].model, "gpt-5");
        let mut screen = crate::settings::State::new(load.settings, load.known_models);
        let _ = screen.update(crate::settings::Message::HarnessModelChanged(0, "opus".into()));
        let _ = screen.update(crate::settings::Message::HarnessEffortChanged(0, "high".into()));
        let _ = screen.update(crate::settings::Message::HarnessEffortChanged(1, "xhigh".into()));
        let (_, event) = screen.update(crate::settings::Message::Save);
        let Some(crate::settings::Event::Save(settings)) = event else { panic!("{event:?}") };
        let saved = f.backend.save_settings(settings).await.unwrap();
        assert_eq!(saved.harnesses[0].model, "opus");
        assert_eq!(saved.harnesses[0].effort, "high");
        assert_eq!(saved.harnesses[1].effort, "xhigh");
        let codex = std::fs::read_to_string(f.dir.path().join("config.toml")).unwrap();
        assert_eq!(codex, "# codex\nmodel = \"gpt-5\"\nmodel_reasoning_effort = \"xhigh\"\n");
        let claude = std::fs::read_to_string(f.dir.path().join("claude-settings.json")).unwrap();
        assert!(claude.contains("\"model\": \"opus\""), "{claude}");

        let mut bad = saved.clone();
        bad.harnesses[1].model = "gpt 6".into();
        let errors = f.backend.save_settings(bad).await.unwrap_err();
        assert_eq!(errors[0].location, "Codex");
    }
}
