//! Opt-in tests of the model screens' backend against the installed `omp`.
//! None of them makes a model call or starts a login, and the user's own
//! settings files are only read; edits go to a temporary agent directory.
//! Run with `cargo test -p loa-ui-models --test live -- --ignored`.
//! `LOA_LIVE_MODEL` (`provider/id`) picks the model to switch to.

use loa_util::exec::which;
use std::sync::Arc;

use loa_adapter::{Adapter, ModelSelection, SessionConfig};
use loa_adapter_omp::{OmpAdapter, OmpAdapterConfig};
use loa_core::{MemorySink, PatchSink};
use loa_models::omp::OmpLaunch;
use loa_ui_models::backend::{Backend, OMP};
use loa_ui_models::settings::{CustomProvider, ProviderAuth, RoleChain};
use loa_ui_models::signin::Auth;
use loa_ui_models::{ModelRef, SessionModel, SessionModelChange, chip, model_change_drafts, picker};

fn live_model() -> ModelRef {
    let selector = std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into());
    ModelRef::parse(&selector).expect("LOA_LIVE_MODEL is provider/id")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp (no model call, read only)"]
async fn real_omp_feeds_the_sign_in_rows_the_picker_and_the_settings_screen() {
    let cwd = tempfile::tempdir().unwrap();
    let backend = Backend::system(OmpLaunch { cwd: Some(cwd.path().to_owned()), ..OmpLaunch::default() });

    let rows = backend.providers().await;
    let anthropic = rows.iter().find(|r| r.key.harness == OMP && r.key.provider == "anthropic").expect("omp lists anthropic");
    assert!(matches!(anthropic.auth, Auth::Login { .. }), "{anthropic:?}");
    for harness in ["claude", "codex"] {
        let row = rows.iter().find(|r| r.key.harness == harness).expect("a row per harness");
        // Neither CLI is installed on this machine: the row says so instead of offering a sign in.
        if which(harness).is_none() {
            assert!(matches!(row.auth, Auth::Unavailable { .. }), "{row:?}");
        }
    }

    let catalog = backend.catalog().await.unwrap();
    let wanted = live_model();
    assert!(catalog.models.iter().any(|m| m.model == wanted), "{wanted} not offered");
    let (mut picker, _) = picker::State::open(catalog.clone(), picker::Target::Session, None);
    let _ = picker.update(picker::Message::Query(wanted.id.clone()));
    let (_, event) = picker.update(picker::Message::Submit);
    // A role resolving to the model (from the user's real config) may be the first hit; either way
    // the pick switches the session to the wanted model.
    assert!(
        matches!(&event, Some(picker::Event::SetSessionModel(SessionModelChange { model, .. })) if *model == wanted),
        "{event:?}"
    );

    let load = backend.load_settings().await.unwrap();
    assert!(load.known_models.contains(&wanted.selector()));
    assert!(!load.settings.providers.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp (no model call); writes only a temporary agent directory"]
async fn settings_saved_from_the_screen_are_validated_by_real_omp() {
    let agent = tempfile::tempdir().unwrap();
    std::fs::write(agent.path().join("config.yml"), "# hand written\ntheme:\n  dark: titanium\n").unwrap();
    let backend = Backend::system(OmpLaunch::default().env("PI_CODING_AGENT_DIR", agent.path()));
    assert_eq!(backend.files().await.unwrap().agent_dir.canonicalize().unwrap(), agent.path().canonicalize().unwrap());

    let load = backend.load_settings().await.unwrap();
    let mut settings = load.settings.clone();
    settings.roles.push(RoleChain {
        role: "smol".into(),
        chain: vec!["anthropic/claude-haiku-5-5:low".into(), "openai-codex/gpt-6-sol".into()],
    });
    settings.custom_providers.push(CustomProvider {
        id: "local-gw".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        api: "openai-completions".into(),
        auth: ProviderAuth::ApiKey,
        api_key_env: "LOCAL_GW_KEY".into(),
        models: vec!["qwen-coder".into()],
    });
    let saved = backend.save_settings(settings.clone()).await.expect("omp accepts the edits");
    assert_eq!(saved.roles, settings.roles);
    assert_eq!(saved.custom_providers, settings.custom_providers);
    let config = std::fs::read_to_string(agent.path().join("config.yml")).unwrap();
    assert!(config.starts_with("# hand written\ntheme:\n  dark: titanium\n"), "{config}");
    assert!(config.contains("smol: anthropic/claude-haiku-5-5:low"), "{config}");
    let models = std::fs::read_to_string(agent.path().join("models.yml")).unwrap();
    assert!(models.contains("apiKey: LOCAL_GW_KEY"), "{models}");

    // omp now lists the custom provider's model.
    let reloaded = backend.load_settings().await.unwrap();
    assert!(reloaded.known_models.contains(&"local-gw/qwen-coder".to_owned()), "{:?}", reloaded.known_models);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp (no model call)"]
async fn picking_a_model_switches_the_live_session_and_its_model_attribute() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = OmpAdapter::new(OmpAdapterConfig::new("live-test", dir.path().join("state")));
    let sink = Arc::new(MemorySink::new());
    let config = SessionConfig {
        session_id: "live-models".into(),
        node: "live-test".into(),
        cwd: dir.path().to_owned(),
        model: Some("anthropic/claude-opus-5-5".into()),
        ..Default::default()
    };
    let session = adapter.start(config, sink.clone()).await.expect("omp starts in rpc mode");
    let mut chip = chip::State::from_dom(&sink.dom());

    let wanted = live_model();
    let change = SessionModelChange { model: wanted.clone(), role: Some("smol".into()) };
    session.set_model(ModelSelection { model: wanted.selector(), role: None }).await.expect("omp switches the model");
    for draft in model_change_drafts(&sink.dom(), &change).unwrap() {
        sink.commit(draft).unwrap();
    }
    for patch in sink.patches() {
        chip.apply(&patch);
    }
    let (_, model) = SessionModel::from_dom(&sink.dom()).expect("a session");
    assert_eq!(model, SessionModel { model: Some(wanted.selector()), role: Some("smol".into()) });
    assert!(chip.label().contains(&wanted.id), "{}", chip.label());
    session.shutdown().await.unwrap();
}
