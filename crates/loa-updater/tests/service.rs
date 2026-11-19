//! The updater service end to end against a local feed and scripted
//! executables: check, stage, gate, hold choices, swap between turns,
//! rollback, app signatures and persistence.

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, Response, StatusCode};
use axum::routing::get;
use ed25519_dalek::{Signer, SigningKey};
use loa_updater::decision::HoldChoice;
use loa_updater::gates::{GateCheck, Verdict};
use loa_updater::swap::SwapOutcome;
use loa_updater::{
    Feed, Gates, Installed, InstalledStatus, PrereleasePolicy, Product, ProductKind, ProductSpec,
    SignatureStatus, TurnGate, Updater, UpdaterConfig, UpdaterError,
};
use semver::Version;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

/// Releases and files the fake GitHub serves, changeable during a test.
#[derive(Clone, Default)]
struct Feed2 {
    releases: Arc<Mutex<serde_json::Value>>,
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    etag: Arc<Mutex<u32>>,
    conditional_hits: Arc<Mutex<u32>>,
}

async fn releases(State(feed): State<Feed2>, headers: HeaderMap) -> Response<Body> {
    let etag = format!("\"{}\"", *feed.etag.lock().unwrap());
    if headers.get("if-none-match").and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        *feed.conditional_hits.lock().unwrap() += 1;
        return Response::builder().status(StatusCode::NOT_MODIFIED).body(Body::empty()).unwrap();
    }
    let body = feed.releases.lock().unwrap().to_string();
    Response::builder().header("etag", etag).body(Body::from(body)).unwrap()
}

async fn file(UrlPath(name): UrlPath<String>, State(feed): State<Feed2>) -> Response<Body> {
    match feed.files.lock().unwrap().get(&name) {
        Some(bytes) => Response::builder().body(Body::from(bytes.clone())).unwrap(),
        None => Response::builder().status(StatusCode::NOT_FOUND).body(Body::empty()).unwrap(),
    }
}

impl Feed2 {
    async fn serve(&self) -> String {
        let router = Router::new()
            .route("/repos/acme/{repo}/releases", get(releases))
            .route("/files/{name}", get(file))
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        base
    }

    /// Publishes `version` with `bytes` as its asset `name`, plus `extra`
    /// assets (signatures).
    fn publish(&self, base: &str, version: &str, notes: &str, name: &str, bytes: &[u8], extra: &[(&str, Vec<u8>)]) {
        let mut files = self.files.lock().unwrap();
        let key = format!("{version}-{name}");
        files.insert(key.clone(), bytes.to_vec());
        let mut assets = vec![serde_json::json!({
            "name": name,
            "browser_download_url": format!("{base}/files/{key}"),
            "size": bytes.len(),
            "digest": format!("sha256:{}", hex::encode(Sha256::digest(bytes))),
        })];
        for (extra_name, extra_bytes) in extra {
            let key = format!("{version}-{extra_name}");
            files.insert(key.clone(), extra_bytes.clone());
            assets.push(serde_json::json!({"name": extra_name, "browser_download_url": format!("{base}/files/{key}")}));
        }
        let mut list = self.releases.lock().unwrap();
        if !list.is_array() {
            *list = serde_json::json!([]);
        }
        list.as_array_mut().unwrap().push(serde_json::json!({"tag_name": format!("v{version}"), "body": notes, "assets": assets}));
        *self.etag.lock().unwrap() += 1;
    }
}

fn script(path: &Path, body: &str) -> PathBuf {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_path_buf()
}

/// A tool that reports `version`.
fn tool_bytes(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"tool {version}\"\n").into_bytes()
}

struct Setup {
    _dir: tempfile::TempDir,
    root: PathBuf,
    feed: Feed2,
    base: String,
    review: PathBuf,
    config: UpdaterConfig,
}

/// A harness product `tool` (changelog gate) at 1.0.0 on the system, and an
/// `omp` reviewer whose reply is read from `review`.
async fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("updates");
    let review = dir.path().join("review.json");
    std::fs::write(&review, r#"{"breaking": [], "removed": []}"#).unwrap();
    let system_tool = script(&dir.path().join("tool"), "echo 'tool 1.0.0'");
    let reviewer = script(
        &dir.path().join("omp"),
        &format!("if [ \"$1\" = --version ]; then echo omp/9.0.0; else cat '{}'; fi", review.display()),
    );
    let feed = Feed2::default();
    let base = feed.serve().await;
    let tool = ProductSpec {
        product: Product {
            id: "tool".into(),
            kind: ProductKind::Harness("tool".into()),
            feed: Feed::GitHub { owner: "acme".into(), repo: "tool".into() },
            prereleases: PrereleasePolicy::Stable,
            asset: Some("tool-{version}".into()),
        },
        label: "Tool".into(),
        installed: Installed::Executable(system_tool),
        executable: None,
        gates: Gates::Changelog,
    };
    let omp = ProductSpec {
        product: Product {
            id: "omp".into(),
            kind: ProductKind::Omp,
            feed: Feed::GitHub { owner: "acme".into(), repo: "omp".into() },
            prereleases: PrereleasePolicy::Stable,
            asset: None,
        },
        label: "omp".into(),
        installed: Installed::Executable(reviewer),
        executable: None,
        gates: Gates::Changelog,
    };
    let mut config = UpdaterConfig::new(&root, vec![tool, omp]);
    config.github_api = base.clone();
    Setup { _dir: dir, root, feed, base, review, config }
}

#[tokio::test]
async fn check_stage_gate_hold_choices_swap_and_rollback() {
    let s = setup().await;
    s.feed.publish(&s.base, "1.1.0", "- tool: `--fast` flag removed", "tool-1.1.0", &tool_bytes("1.1.0"), &[]);
    let updater = Updater::open(s.config.clone()).unwrap();

    updater.detect().await;
    let offer = updater.check("tool").await.unwrap().expect("1.1.0 offered");
    assert_eq!(offer.version, v("1.1.0"));
    assert!(updater.check("tool").await.unwrap().is_some(), "a 304 keeps the cached offer");
    assert_eq!(*s.feed.conditional_hits.lock().unwrap(), 1, "second check was conditional");
    let status = &updater.overview().products[0];
    assert_eq!(status.current, Some(v("1.0.0")));
    assert!(matches!(status.installed, InstalledStatus::System(_)));
    assert_eq!(status.newest, Some(v("1.1.0")));
    assert!(status.changelog.contains("`--fast` flag removed"), "{}", status.changelog);
    assert!(status.last_check_unix.is_some());

    assert!(matches!(updater.gate("tool").await, Err(UpdaterError::NotStaged { .. })));
    assert_eq!(updater.stage("tool").await.unwrap(), v("1.1.0"));
    assert_eq!(updater.overview().products[0].staged, vec![v("1.1.0")]);

    // The reviewer flags the removal: the update is held.
    std::fs::write(&s.review, r#"{"breaking": [], "removed": ["tool: `--fast` flag removed"]}"#).unwrap();
    let report = updater.gate("tool").await.unwrap();
    assert!(!report.is_clean());
    let changelog = report.checks.iter().find(|c| c.check == GateCheck::Changelog).unwrap();
    assert_eq!(changelog.verdict, Verdict::Harmful);
    let status = &updater.overview().products[0];
    assert_eq!(status.held.as_ref().map(|h| h.version.clone()), Some(v("1.1.0")));
    assert_eq!(status.pending_swap, None);

    // Wait: 1.1.0 is no longer offered; unpinning offers it again.
    updater.choose("tool", HoldChoice::Wait).unwrap();
    let status = &updater.overview().products[0];
    assert_eq!((status.offer.as_ref(), status.waiting_past.clone()), (None, Some(v("1.1.0"))));
    updater.unpin("tool").unwrap();
    assert!(updater.overview().products[0].offer.is_some());

    // Pin the current version: nothing else is offered.
    updater.gate("tool").await.unwrap();
    updater.choose("tool", HoldChoice::PinCurrent).unwrap();
    let status = &updater.overview().products[0];
    assert_eq!((status.offer.as_ref(), status.pin.clone()), (None, Some(v("1.0.0"))));
    updater.unpin("tool").unwrap();

    // Update anyway: pending, deferred while a turn runs, swapped after.
    updater.gate("tool").await.unwrap();
    assert_eq!(updater.choose("tool", HoldChoice::UpdateAnyway).unwrap(), Some(v("1.1.0")));
    let turns = TurnGate::default();
    turns.begin();
    let outcomes = updater.apply_pending(&turns);
    assert!(matches!(outcomes.as_slice(), [(id, Ok(SwapOutcome::Deferred))] if id == "tool"), "{outcomes:?}");
    assert_eq!(updater.overview().products[0].pending_swap, Some(v("1.1.0")));
    turns.end();
    let outcomes = updater.apply_pending(&turns);
    assert!(
        matches!(outcomes.as_slice(), [(_, Ok(SwapOutcome::Swapped { from: None, to }))] if *to == v("1.1.0")),
        "{outcomes:?}"
    );
    let managed = updater.executable("tool").unwrap();
    assert!(managed.starts_with(&s.root), "{}", managed.display());
    let status = &updater.overview().products[0];
    assert_eq!(status.current, Some(v("1.1.0")));
    assert_eq!(status.installed, InstalledStatus::Managed(managed.clone()));
    assert_eq!(status.pending_swap, None);

    // A clean 1.2.0 swaps straight away; rollback goes back to 1.1.0.
    std::fs::write(&s.review, r#"{"breaking": [], "removed": []}"#).unwrap();
    s.feed.publish(&s.base, "1.2.0", "- faster startup", "tool-1.2.0", &tool_bytes("1.2.0"), &[]);
    assert_eq!(updater.check("tool").await.unwrap().map(|r| r.version), Some(v("1.2.0")));
    updater.stage("tool").await.unwrap();
    assert!(updater.gate("tool").await.unwrap().is_clean());
    assert_eq!(updater.overview().products[0].pending_swap, Some(v("1.2.0")));
    assert_eq!(
        updater.swap("tool", &turns).unwrap(),
        SwapOutcome::Swapped { from: Some(v("1.1.0")), to: v("1.2.0") }
    );
    turns.begin();
    assert_eq!(updater.rollback("tool", &turns).unwrap(), SwapOutcome::Deferred);
    turns.end();
    assert_eq!(
        updater.rollback("tool", &turns).unwrap(),
        SwapOutcome::Swapped { from: Some(v("1.2.0")), to: v("1.1.0") }
    );
    assert_eq!(updater.overview().products[0].current, Some(v("1.1.0")));

    // Everything but in-memory errors survives a restart, offers included.
    let reopened = Updater::open(s.config.clone()).unwrap();
    let status = &reopened.overview().products[0];
    assert_eq!(status.current, Some(v("1.1.0")));
    assert_eq!(status.offer.as_ref().map(|r| r.version.clone()), Some(v("1.2.0")));
    assert!(status.report.as_ref().is_some_and(|r| r.is_clean()));
    assert!(status.last_check_unix.is_some());
}

#[tokio::test]
async fn pins_from_the_config_replace_the_stored_pins() {
    let s = setup().await;
    s.feed.publish(&s.base, "1.1.0", "- faster", "tool-1.1.0", &tool_bytes("1.1.0"), &[]);
    let updater = Updater::open(s.config.clone()).unwrap();
    updater.detect().await;
    updater.check("tool").await.unwrap();
    assert!(matches!(
        updater.set_pins(BTreeMap::from([("nope".to_owned(), v("1.0.0"))])),
        Err(UpdaterError::UnknownProduct(_))
    ));

    updater.set_pins(BTreeMap::from([("tool".to_owned(), v("1.0.0"))])).unwrap();
    let status = &Updater::open(s.config.clone()).unwrap().overview().products[0];
    assert_eq!((status.pin.clone(), status.offer.as_ref()), (Some(v("1.0.0")), None), "persisted");

    updater.set_pins(BTreeMap::new()).unwrap();
    let status = &updater.overview().products[0];
    assert_eq!((status.pin.clone(), status.offer.as_ref().map(|r| r.version.clone())), (None, Some(v("1.1.0"))));
}

#[tokio::test]
async fn product_without_release_notes_passes_changelog_and_errors_are_shown() {
    let s = setup().await;
    s.feed.publish(&s.base, "2.0.0", "", "tool-2.0.0", &tool_bytes("2.0.0"), &[]);
    let updater = Updater::open(s.config.clone()).unwrap();
    assert!(matches!(updater.stage("nope").await, Err(UpdaterError::UnknownProduct(_))));
    assert!(matches!(updater.stage("tool").await, Err(UpdaterError::NothingOffered(_))));
    assert!(updater.overview().products[0].error.as_deref().is_some_and(|e| e.contains("check its feed")));
    updater.check("tool").await.unwrap();
    assert_eq!(updater.overview().products[0].error, None, "a success clears the error");
    updater.stage("tool").await.unwrap();
    let report = updater.gate("tool").await.unwrap();
    assert!(report.is_clean(), "{report}");
    assert_eq!(report.checks[0].details, ["no release notes published after 1.0.0"]);
}

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

fn signature_of(bytes: &[u8], key: &SigningKey) -> Vec<u8> {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    format!("{}\n", hex::encode(key.sign(&digest).to_bytes())).into_bytes()
}

async fn app_setup(signed_by: Option<&SigningKey>) -> (Setup, Updater) {
    let mut s = setup().await;
    let bytes = b"#!/bin/sh\necho loa 0.2.0\n".to_vec();
    let extra: Vec<(&str, Vec<u8>)> =
        signed_by.map(|key| vec![("loa-app-0.2.0.sig", signature_of(&bytes, key))]).unwrap_or_default();
    s.feed.publish(&s.base, "0.2.0", "- faster", "loa-app-0.2.0", &bytes, &extra);
    s.config.products.insert(
        0,
        ProductSpec {
            product: Product {
                id: "app".into(),
                kind: ProductKind::App,
                feed: Feed::GitHub { owner: "acme".into(), repo: "app".into() },
                prereleases: PrereleasePolicy::Stable,
                asset: Some("loa-app-{version}".into()),
            },
            label: "loa".into(),
            installed: Installed::Running(v("0.1.0")),
            executable: None,
            gates: Gates::None,
        },
    );
    s.config.trusted_keys = vec![signing_key().verifying_key()];
    let updater = Updater::open(s.config.clone()).unwrap();
    updater.check("app").await.unwrap();
    updater.stage("app").await.unwrap();
    let report = updater.gate("app").await.unwrap();
    assert!(report.is_clean() && report.checks.is_empty(), "app updates rely on signatures");
    (s, updater)
}

#[tokio::test]
async fn signed_app_update_verifies_swaps_and_asks_for_restart() {
    let (_s, updater) = app_setup(Some(&signing_key())).await;
    let status = &updater.overview().products[0];
    assert_eq!(status.signature, Some(SignatureStatus::Verified));
    assert_eq!(status.installed, InstalledStatus::Running);
    let turns = TurnGate::default();
    assert!(matches!(updater.swap("app", &turns).unwrap(), SwapOutcome::Swapped { .. }));
    let status = &updater.overview().products[0];
    assert!(status.restart_required);
    assert_eq!(status.current, Some(v("0.1.0")), "still running the old app until restart");
}

#[tokio::test]
async fn unsigned_or_untrusted_app_update_never_swaps() {
    let (_s, updater) = app_setup(None).await;
    assert_eq!(updater.overview().products[0].signature, Some(SignatureStatus::Unsigned));
    let turns = TurnGate::default();
    assert!(matches!(updater.swap("app", &turns), Err(UpdaterError::Swap(_))));
    let status = &updater.overview().products[0];
    assert!(status.error.as_deref().is_some_and(|e| e.contains("signature")), "{:?}", status.error);
    assert_eq!(status.active, None);

    let stranger = SigningKey::from_bytes(&[9; 32]);
    let (_s, updater) = app_setup(Some(&stranger)).await;
    assert!(matches!(updater.overview().products[0].signature, Some(SignatureStatus::Rejected(_))));
    assert!(updater.swap("app", &turns).is_err());
}
