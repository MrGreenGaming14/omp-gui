use ed25519_dalek::SigningKey;
use loa_updater::StagedManifest;
use loa_updater::layout::{ARTIFACT_DIR, MANIFEST_FILE, PAYLOAD_DIR, version_dir};
use loa_updater::signature::{SignatureError, artifact_digest, sign_artifact};
use loa_updater::swap::{Active, Activity, SwapError, SwapOutcome, Swapper};
use semver::Version;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Activity driven by the test: `true` while a turn runs.
struct Turns(AtomicBool);

impl Turns {
    fn new(running: bool) -> Self {
        Self(AtomicBool::new(running))
    }

    fn set(&self, running: bool) {
        self.0.store(running, Ordering::Relaxed);
    }
}

impl Activity for Turns {
    fn turn_running(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

/// Writes a fully staged version of `product`, signed with `key` if given.
fn stage(root: &Path, product: &str, version: &str, key: Option<&SigningKey>) {
    let version = v(version);
    let dir = version_dir(root, product, &version);
    fs::create_dir_all(dir.join(PAYLOAD_DIR)).unwrap();
    fs::write(dir.join(PAYLOAD_DIR).join("bin"), format!("{product} {version}")).unwrap();
    fs::create_dir_all(dir.join(ARTIFACT_DIR)).unwrap();
    let name = format!("{product}-{version}.tar.gz");
    let artifact = dir.join(ARTIFACT_DIR).join(&name);
    fs::write(&artifact, format!("artifact of {product} {version}")).unwrap();
    let signature_name = key.map(|key| {
        sign_artifact(key, &artifact).unwrap();
        format!("{name}.sig")
    });
    let manifest = StagedManifest {
        product: product.into(),
        version,
        artifact_name: name,
        sha256: hex::encode(artifact_digest(&artifact).unwrap()),
        size: fs::metadata(&artifact).unwrap().len(),
        signature_name,
    };
    fs::write(dir.join(MANIFEST_FILE), serde_json::to_vec(&manifest).unwrap()).unwrap();
}

fn active(current: &str, previous: Option<&str>) -> Option<Active> {
    Some(Active { current: v(current), previous: previous.map(v) })
}

#[test]
fn swap_is_deferred_while_a_turn_runs() {
    let root = tempfile::tempdir().unwrap();
    stage(root.path(), "omp", "1.0.0", None);
    stage(root.path(), "omp", "1.1.0", None);
    let swapper = Swapper::new(root.path());
    let turns = Turns::new(true);

    assert_eq!(swapper.try_swap("omp", &v("1.0.0"), None, &turns).unwrap(), SwapOutcome::Deferred);
    assert_eq!(swapper.active("omp").unwrap(), None);

    turns.set(false);
    swapper.try_swap("omp", &v("1.0.0"), None, &turns).unwrap();
    turns.set(true);
    assert_eq!(swapper.try_swap("omp", &v("1.1.0"), None, &turns).unwrap(), SwapOutcome::Deferred);
    assert_eq!(swapper.active("omp").unwrap(), active("1.0.0", None));
}

#[test]
fn swap_records_previous_and_keeps_it_installed() {
    let root = tempfile::tempdir().unwrap();
    stage(root.path(), "omp", "1.0.0", None);
    stage(root.path(), "omp", "1.1.0", None);
    let swapper = Swapper::new(root.path());
    let idle = Turns::new(false);

    assert_eq!(
        swapper.try_swap("omp", &v("1.0.0"), None, &idle).unwrap(),
        SwapOutcome::Swapped { from: None, to: v("1.0.0") }
    );
    assert_eq!(
        swapper.try_swap("omp", &v("1.1.0"), None, &idle).unwrap(),
        SwapOutcome::Swapped { from: Some(v("1.0.0")), to: v("1.1.0") }
    );
    assert_eq!(swapper.active("omp").unwrap(), active("1.1.0", Some("1.0.0")));
    let payload = swapper.active_dir("omp").unwrap().unwrap();
    assert_eq!(fs::read_to_string(payload.join("bin")).unwrap(), "omp 1.1.0");
    assert!(version_dir(root.path(), "omp", &v("1.0.0")).join(PAYLOAD_DIR).is_dir());
    assert_eq!(
        swapper.try_swap("omp", &v("1.1.0"), None, &idle).unwrap(),
        SwapOutcome::AlreadyActive
    );
}

#[test]
fn third_swap_prunes_oldest_but_keeps_newer_staged() {
    let root = tempfile::tempdir().unwrap();
    for version in ["1.0.0", "1.1.0", "1.2.0", "1.3.0"] {
        stage(root.path(), "omp", version, None);
    }
    let temp = root.path().join("versions/omp/.staging-1.4.0");
    fs::create_dir_all(&temp).unwrap();
    let swapper = Swapper::new(root.path());
    let idle = Turns::new(false);

    for version in ["1.0.0", "1.1.0", "1.2.0"] {
        swapper.try_swap("omp", &v(version), None, &idle).unwrap();
    }
    assert_eq!(swapper.active("omp").unwrap(), active("1.2.0", Some("1.1.0")));
    let exists = |version: &str| version_dir(root.path(), "omp", &v(version)).exists();
    assert!(!exists("1.0.0"));
    assert!(exists("1.1.0"));
    assert!(exists("1.2.0"));
    assert!(exists("1.3.0"));
    assert!(temp.is_dir());
}

#[test]
fn keep_retains_extra_old_versions() {
    let root = tempfile::tempdir().unwrap();
    for version in ["1.0.0", "1.1.0", "1.2.0", "1.3.0"] {
        stage(root.path(), "omp", version, None);
    }
    let swapper = Swapper { root: root.path().into(), keep: 1, app_product: "app".into() };
    let idle = Turns::new(false);
    for version in ["1.0.0", "1.1.0", "1.2.0", "1.3.0"] {
        swapper.try_swap("omp", &v(version), None, &idle).unwrap();
    }
    let exists = |version: &str| version_dir(root.path(), "omp", &v(version)).exists();
    assert!(!exists("1.0.0"));
    assert!(exists("1.1.0"));
}

#[test]
fn unstaged_version_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let dir = version_dir(root.path(), "omp", &v("2.0.0"));
    fs::create_dir_all(dir.join(PAYLOAD_DIR)).unwrap();
    let swapper = Swapper::new(root.path());
    let err = swapper.try_swap("omp", &v("2.0.0"), None, &Turns::new(false)).unwrap_err();
    assert!(matches!(err, SwapError::NotStaged { .. }), "{err}");
    assert_eq!(swapper.active("omp").unwrap(), None);
}

#[test]
fn app_swap_with_bad_signature_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7; 32]);
    let trusted = [key.verifying_key()];
    stage(root.path(), "app", "1.0.0", Some(&key));
    stage(root.path(), "app", "1.1.0", Some(&key));
    stage(root.path(), "app", "1.2.0", None);
    let swapper = Swapper::new(root.path());
    let idle = Turns::new(false);
    swapper.try_swap("app", &v("1.0.0"), Some(&trusted), &idle).unwrap();

    let artifact =
        version_dir(root.path(), "app", &v("1.1.0")).join(ARTIFACT_DIR).join("app-1.1.0.tar.gz");
    let mut bytes = fs::read(&artifact).unwrap();
    bytes[0] ^= 0x01;
    fs::write(&artifact, bytes).unwrap();
    let err = swapper.try_swap("app", &v("1.1.0"), Some(&trusted), &idle).unwrap_err();
    assert!(matches!(err, SwapError::Signature(_)), "{err}");
    assert_eq!(swapper.active("app").unwrap(), active("1.0.0", None));

    let err = swapper.try_swap("app", &v("1.2.0"), Some(&trusted), &idle).unwrap_err();
    assert!(matches!(err, SwapError::Signature(SignatureError::Unsigned { .. })), "{err}");
    assert_eq!(swapper.active("app").unwrap(), active("1.0.0", None));
}

#[test]
fn app_cannot_bypass_signature_check_by_omitting_keys() {
    let root = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7; 32]);
    stage(root.path(), "app", "1.0.0", Some(&key));
    let swapper = Swapper::new(root.path());
    let err = swapper.try_swap("app", &v("1.0.0"), None, &Turns::new(false)).unwrap_err();
    assert!(matches!(err, SwapError::Signature(SignatureError::Untrusted { .. })), "{err}");
    assert_eq!(swapper.active("app").unwrap(), None);
}

#[test]
fn rollback_restores_previous_and_can_be_reapplied() {
    let root = tempfile::tempdir().unwrap();
    stage(root.path(), "omp", "1.0.0", None);
    stage(root.path(), "omp", "1.1.0", None);
    let swapper = Swapper::new(root.path());
    let turns = Turns::new(false);

    swapper.try_swap("omp", &v("1.0.0"), None, &turns).unwrap();
    let err = swapper.rollback("omp", None, &turns).unwrap_err();
    assert!(matches!(err, SwapError::NoPrevious { .. }), "{err}");

    swapper.try_swap("omp", &v("1.1.0"), None, &turns).unwrap();
    turns.set(true);
    assert_eq!(swapper.rollback("omp", None, &turns).unwrap(), SwapOutcome::Deferred);
    assert_eq!(swapper.active("omp").unwrap(), active("1.1.0", Some("1.0.0")));

    turns.set(false);
    assert_eq!(
        swapper.rollback("omp", None, &turns).unwrap(),
        SwapOutcome::Swapped { from: Some(v("1.1.0")), to: v("1.0.0") }
    );
    assert_eq!(swapper.active("omp").unwrap(), active("1.0.0", Some("1.1.0")));
    let payload = swapper.active_dir("omp").unwrap().unwrap();
    assert_eq!(fs::read_to_string(payload.join("bin")).unwrap(), "omp 1.0.0");

    swapper.try_swap("omp", &v("1.1.0"), None, &turns).unwrap();
    assert_eq!(swapper.active("omp").unwrap(), active("1.1.0", Some("1.0.0")));
}
