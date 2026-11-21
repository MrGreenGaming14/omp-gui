//! Shared fixtures: a fake GitHub and npm feed, signing keys, staged versions and gate ports.

use axum::Router;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use ed25519_dalek::{Signer, SigningKey};
use loa_updater::gates::{ChangelogReview, ChangelogSummarizer, GatePorts, OmpGates, Probe, parse_review};
use loa_updater::stage::{StagedManifest, version_dir};
use semver::Version;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

pub fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// The `.sig` text of `bytes`: the hex signature over their SHA-256.
pub fn sign_digest(bytes: &[u8], key: &SigningKey) -> Vec<u8> {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    format!("{}\n", hex::encode(key.sign(&digest).to_bytes())).into_bytes()
}

/// Writes `<artifact>.sig` next to `artifact`.
pub fn sign_artifact(artifact: &Path, key: &SigningKey) -> PathBuf {
    let sig = artifact.with_file_name(format!("{}.sig", artifact.file_name().unwrap().to_string_lossy()));
    std::fs::write(&sig, sign_digest(&std::fs::read(artifact).unwrap(), key)).unwrap();
    sig
}

/// A fully staged `product` `version` holding the raw file `bytes`, signed by `signer` when given.
pub fn stage_fixture(root: &Path, product: &str, version: &str, bytes: &[u8], signer: Option<&SigningKey>) {
    let dir = version_dir(root, product, &v(version));
    let name = format!("{product}-{version}");
    for sub in ["artifact", "payload"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
        std::fs::write(dir.join(sub).join(&name), bytes).unwrap();
    }
    let signature_name = signer.map(|k| sign_artifact(&dir.join("artifact").join(&name), k)).map(|_| format!("{name}.sig"));
    let manifest = StagedManifest {
        product: product.into(),
        version: v(version),
        artifact_name: name,
        sha256: hex::encode(Sha256::digest(bytes)),
        size: bytes.len() as u64,
        signature_name,
    };
    std::fs::write(dir.join("staged.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
}

/// Extension of a staged release asset: a raw file is launched as is, and Windows only launches batch files and
/// `.exe`s, so the fixture is a `.cmd` there.
pub const ASSET_EXT: &str = if cfg!(windows) { ".cmd" } else { "" };

/// GitHub releases per repo and npm packuments per package, with ETags, served on localhost.
#[derive(Clone, Default)]
pub struct FakeFeed {
    pub base: String,
    lists: Arc<Mutex<BTreeMap<String, Value>>>,
    files: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    pub not_modified: Arc<Mutex<u32>>,
}

async fn list(UrlPath(key): UrlPath<String>, State(feed): State<FakeFeed>, headers: HeaderMap) -> Response {
    let lists = feed.lists.lock().unwrap();
    let Some(body) = lists.get(&key) else { return StatusCode::NOT_FOUND.into_response() };
    let etag = format!("\"{}\"", body.to_string().len());
    if headers.get("if-none-match").is_some_and(|tag| *tag == *etag) {
        *feed.not_modified.lock().unwrap() += 1;
        return StatusCode::NOT_MODIFIED.into_response();
    }
    ([("etag", etag)], body.to_string()).into_response()
}

async fn file(UrlPath(name): UrlPath<String>, State(feed): State<FakeFeed>) -> Response {
    feed.files.lock().unwrap().get(&name).cloned().map_or_else(|| StatusCode::NOT_FOUND.into_response(), IntoResponse::into_response)
}

impl FakeFeed {
    pub async fn serve() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let feed = Self { base: format!("http://{}", listener.local_addr().unwrap()), ..Self::default() };
        let router = Router::new()
            .route("/repos/acme/{key}/releases", get(list))
            .route("/npm/{key}", get(list))
            .route("/files/{name}", get(file))
            .with_state(feed.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        feed
    }

    fn file(&self, name: String, bytes: Vec<u8>) -> String {
        self.files.lock().unwrap().insert(name.clone(), bytes);
        format!("{}/files/{name}", self.base)
    }

    /// Publishes GitHub release `version` of `repo` with asset `<repo>-<version>` (plus [`ASSET_EXT`]) and optional
    /// extra assets.
    pub fn release(&self, repo: &str, version: &str, notes: &str, bytes: &[u8], extra: &[(&str, Vec<u8>)]) {
        let name = format!("{repo}-{version}{ASSET_EXT}");
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        let mut assets = vec![json!({"name": name, "browser_download_url": self.file(name.clone(), bytes.to_vec()), "size": bytes.len(), "digest": digest})];
        assets.extend(extra.iter().map(|(n, b)| json!({"name": n, "browser_download_url": self.file(format!("{version}-{n}"), b.clone())})));
        let mut lists = self.lists.lock().unwrap();
        let releases = lists.entry(repo.to_owned()).or_insert_with(|| json!([]));
        releases.as_array_mut().unwrap().push(json!({"tag_name": format!("v{version}"), "body": notes, "assets": assets}));
    }

    /// Publishes npm `package` with `latest` and one tarball per version; `deprecated` versions are marked so.
    pub fn npm(&self, package: &str, latest: &str, versions: &[(&str, &[u8], bool)]) {
        let entries: serde_json::Map<String, Value> = versions
            .iter()
            .map(|(version, tgz, deprecated)| {
                let integrity = format!("sha512-{}", base64::Engine::encode(&base64::engine::general_purpose::STANDARD, sha2::Sha512::digest(tgz)));
                let tarball = self.file(format!("{package}-{version}.tgz"), tgz.to_vec());
                let mut entry = json!({"dist": {"tarball": tarball, "integrity": integrity}});
                if *deprecated {
                    entry["deprecated"] = json!("broken");
                }
                ((*version).to_owned(), entry)
            })
            .collect();
        self.lists.lock().unwrap().insert(package.to_owned(), json!({"dist-tags": {"latest": latest}, "versions": entries}));
    }
}

/// Gate ports whose reviewer answers with the contents of a file; there is no omp probe.
#[derive(Debug)]
pub struct FilePorts(pub PathBuf);

impl GatePorts for FilePorts {
    fn probe(&self, _: &OmpGates, _: Duration) -> Box<dyn Probe> {
        unreachable!("no product here has omp gates")
    }

    fn reviewer(&self, _: &Path, _: &str, _: Duration) -> Box<dyn ChangelogSummarizer> {
        Box::new(FileReviewer(self.0.clone()))
    }
}

struct FileReviewer(PathBuf);

#[async_trait::async_trait]
impl ChangelogSummarizer for FileReviewer {
    async fn review(&self, _: &str) -> Result<ChangelogReview, String> {
        parse_review(&std::fs::read_to_string(&self.0).unwrap())
    }
}
