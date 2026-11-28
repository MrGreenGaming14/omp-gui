//! Writes the updater golden fixtures: `cargo run -- <out dir>` (normally `cpp/tests/golden/updater`).

use ed25519_dalek::{Signer, SigningKey};
use loa_settings::updates::{ArchiveFormat, Artifact, CheckResult, Checksum, GateCheck, GateReport, HeldUpdate, Release};
use loa_updater::model::CachedFeed;
use loa_updater::stage::{StagedManifest, version_dir};
use loa_updater::state::UpdaterState;
use loa_updater::swap::{Activity, Swapper};
use semver::Version;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn sig_text(bytes: &[u8], key: &SigningKey) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    format!("{}\n", hex::encode(key.sign(&digest).to_bytes()))
}

fn json<T: serde::Serialize>(path: &Path, value: &T) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    loa_util::fs::write_json_atomic(path, value).unwrap();
}

struct Idle;
impl Activity for Idle {
    fn turn_running(&self) -> bool {
        false
    }
}

/// A staged raw `product` `version` (artifact and payload file `<product>-<version>`), signed by `signer`.
fn stage(root: &Path, product: &str, version: &str, bytes: &[u8], signer: &SigningKey) {
    let dir = version_dir(root, product, &v(version));
    let name = format!("{product}-{version}");
    for sub in ["artifact", "payload"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
        std::fs::write(dir.join(sub).join(&name), bytes).unwrap();
    }
    std::fs::write(dir.join("artifact").join(format!("{name}.sig")), sig_text(bytes, signer)).unwrap();
    let manifest = StagedManifest {
        product: product.into(),
        version: v(version),
        artifact_name: name.clone(),
        sha256: hex::encode(Sha256::digest(bytes)),
        size: bytes.len() as u64,
        signature_name: Some(format!("{name}.sig")),
    };
    json(&dir.join("staged.json"), &manifest);
}

fn report(to: &str, clean: bool) -> GateReport {
    let check = if clean {
        CheckResult::pass(GateCheck::Changelog, vec!["no release notes published after 1.0.0".into()])
    } else {
        CheckResult::harmful(GateCheck::ToolSchemas, vec!["- edit: tool removed".into(), "+ search: tool added".into()])
    };
    GateReport { product: "omp".into(), from: v("1.0.0"), to: v(to), checks: vec![check] }
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("usage: golden_gen <out dir>"));
    let _ = std::fs::remove_dir_all(&out);

    // staged.json manifests.
    let manifest = StagedManifest {
        product: "app".into(),
        version: v("0.2.0-beta.1+build.5"),
        artifact_name: "loa-app-linux-x64.tar.gz".into(),
        sha256: hex::encode(Sha256::digest(b"artifact")),
        size: 8,
        signature_name: Some("loa-app-linux-x64.tar.gz.sig".into()),
    };
    json(&out.join("staged.json"), &manifest);
    json(&out.join("staged-unsigned.json"), &StagedManifest { signature_name: None, ..manifest });

    // active.json, first swap and later.
    json(&out.join("active.json"), &loa_settings::updates::Active { current: v("1.2.0"), previous: Some(v("1.1.0")) });
    json(&out.join("active-first.json"), &loa_settings::updates::Active { current: v("1.1.0"), previous: None });

    // feeds.json: the feed cache keyed by `cache_key`.
    let gh = Release {
        product: "omp".into(),
        version: v("9.1.0"),
        notes: "- faster\n- `grep` removed".into(),
        published_at: Some("2026-01-02T03:04:05Z".into()),
        artifact: Some(Artifact {
            name: "omp-linux-x64".into(),
            url: "https://github.com/can1357/oh-my-pi/releases/download/v9.1.0/omp-linux-x64".into(),
            size: Some(123456789),
            checksum: Some(Checksum::Sha256(hex::encode(Sha256::digest(b"omp")))),
            format: ArchiveFormat::Raw,
            signature_url: Some("https://github.com/can1357/oh-my-pi/releases/download/v9.1.0/omp-linux-x64.sig".into()),
        }),
    };
    let old = Release { version: v("9.0.0"), notes: String::new(), published_at: None, artifact: None, ..gh.clone() };
    let npm = Release {
        product: "claude".into(),
        version: v("2.0.1"),
        notes: String::new(),
        published_at: Some("2026-02-03T04:05:06.789Z".into()),
        artifact: Some(Artifact {
            name: "claude-code-2.0.1.tgz".into(),
            url: "https://registry.npmjs.org/@anthropic-ai/claude-code/-/claude-code-2.0.1.tgz".into(),
            size: None,
            checksum: Some(Checksum::Sha512("ab".repeat(64))),
            format: ArchiveFormat::NpmTarball,
            signature_url: None,
        }),
    };
    let feeds = BTreeMap::from([
        ("omp|github:can1357/oh-my-pi|stable|omp-linux-x64".to_owned(), CachedFeed { etag: Some("W/\"abc\"".into()), releases: vec![old, gh] }),
        ("claude|npm:@anthropic-ai/claude-code|stable|".to_owned(), CachedFeed { etag: None, releases: vec![npm] }),
        ("zip|github:acme/zip|include|zip-{version}.zip".to_owned(), CachedFeed::default()),
    ]);
    json(&out.join("feeds.json"), &feeds);

    // state.json with every decision kind.
    let mut state = UpdaterState::default();
    state.pins.insert("codex".into(), v("0.5.0"));
    state.held.insert("omp".into(), HeldUpdate { version: v("9.1.0"), report: report("9.1.0", false), held_at_unix: 1_760_000_000 });
    state.waits.insert("claude".into(), v("2.0.0"));
    state.last_check_unix.insert("omp".into(), 1_760_000_123);
    state.last_check_unix.insert("app".into(), 1_760_000_124);
    state.pending_swap.insert("opencode".into(), v("1.0.0-rc.1"));
    state.reports.insert("omp".into(), report("9.1.0", false));
    state.reports.insert("opencode".into(), report("1.0.0-rc.1", true));
    json(&out.join("state.json"), &state);

    // The default catalog as a C++ reader sees it on linux-x64.
    json(&out.join("products.json"), &loa_updater::default_products(v("0.1.0")));

    // A Rust-made signature over a raw artifact, and the keys.
    let sig = out.join("sig");
    std::fs::create_dir_all(&sig).unwrap();
    let bytes = b"#!/bin/sh\necho loa 0.2.0\n";
    std::fs::write(sig.join("app-0.2.0"), bytes).unwrap();
    std::fs::write(sig.join("app-0.2.0.sig"), sig_text(bytes, &key(7))).unwrap();
    std::fs::write(sig.join("trusted.hex"), format!("{}\n", hex::encode(key(7).verifying_key().to_bytes()))).unwrap();
    std::fs::write(sig.join("stranger.hex"), format!("{}\n", hex::encode(key(9).verifying_key().to_bytes()))).unwrap();

    // A Rust-written updater root: two signed app versions swapped in, then a state file.
    let root = out.join("root");
    stage(&root, "app", "0.1.0", b"#!/bin/sh\necho loa 0.1.0\n", &key(7));
    stage(&root, "app", "0.2.0", bytes, &key(7));
    let swapper = Swapper::new(&root);
    let trusted = [key(7).verifying_key()];
    swapper.try_swap("app", &v("0.1.0"), Some(&trusted), &Idle).unwrap();
    swapper.try_swap("app", &v("0.2.0"), Some(&trusted), &Idle).unwrap();
    let mut state = UpdaterState::default();
    state.last_check_unix.insert("app".into(), 1_760_000_000);
    state.save(&root).unwrap();
}
