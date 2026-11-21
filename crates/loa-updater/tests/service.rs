//! The updater end to end against a local feed: check, stage, gate, hold choices, swaps between
//! turns, rollback, npm packages, unpack safety, app signatures and persistence.

mod support;

use flate2::write::GzEncoder;
use loa_updater::decision::HoldChoice;
use loa_updater::gates::{GateCheck, Verdict};
use loa_updater::swap::{SwapOutcome, Swapper};
use loa_updater::unpack::{TarGz, Unpacker};
use loa_updater::{Error, Feed, Gates, Installed, InstalledStatus, PrereleasePolicy, Product, ProductKind, ProductSpec, SignatureStatus};
use loa_updater::{TurnGate, Updater, UpdaterConfig};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use support::{FakeFeed, FilePorts, key, sign_digest, stage_fixture, v};

fn script(path: PathBuf, body: &str) -> PathBuf {
    loa_util::exec::write_executable(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    path
}

fn tool(version: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho \"tool {version}\"\n").into_bytes()
}

/// A tar entry: its path and a symlink target (`Ok`) or file contents (`Err`).
type Entry<'a> = (&'a str, Result<&'a str, &'a [u8]>);

/// A gzipped tar of `entries`.
fn tgz(entries: &[Entry]) -> Vec<u8> {
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), flate2::Compression::fast()));
    for (path, entry) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        match entry {
            Ok(target) => {
                header.set_size(0);
                header.set_entry_type(tar::EntryType::Symlink);
                tar.append_link(&mut header, path, target).unwrap();
            }
            Err(data) => {
                header.set_size(data.len() as u64);
                tar.append_data(&mut header, path, *data).unwrap();
            }
        }
    }
    tar.into_inner().unwrap().finish().unwrap()
}

fn spec(id: &str, kind: ProductKind, feed: Feed, asset: Option<&str>, installed: Installed, gates: Gates) -> ProductSpec {
    let product = Product { id: id.into(), kind, feed, prereleases: PrereleasePolicy::Stable, asset: asset.map(Into::into) };
    ProductSpec { product, label: id.into(), installed, executable: None, gates }
}

fn github(repo: &str) -> Feed {
    Feed::GitHub { owner: "acme".into(), repo: repo.into() }
}

/// `tool` 1.0.0 on the system with a changelog gate, an `omp` reviewer answering from `review.json`, and npm `pkg`.
async fn setup(dir: &Path) -> (FakeFeed, UpdaterConfig) {
    let feed = FakeFeed::serve().await;
    let review = dir.join("review.json");
    std::fs::write(&review, r#"{"breaking": [], "removed": []}"#).unwrap();
    let products = vec![
        spec("tool", ProductKind::Harness("tool".into()), github("tool"), Some("tool-{version}"), Installed::Executable(script(dir.join("tool"), "echo 'tool 1.0.0'")), Gates::Changelog),
        spec("omp", ProductKind::Omp, github("omp"), None, Installed::Executable(script(dir.join("omp"), "echo omp/9.0.0")), Gates::Changelog),
        spec("pkg", ProductKind::Harness("pkg".into()), Feed::Npm { package: "pkg".into() }, None, Installed::Executable(dir.join("absent")), Gates::None),
    ];
    let mut config = UpdaterConfig::new(dir.join("updates"), products);
    config.github_api = feed.base.clone();
    config.npm_registry = format!("{}/npm", feed.base);
    config.ports = Some(std::sync::Arc::new(FilePorts(review)));
    (feed, config)
}

#[tokio::test]
async fn harness_update_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    let (feed, config) = setup(dir.path()).await;
    let review = |removed: &str| std::fs::write(dir.path().join("review.json"), format!(r#"{{"breaking": [], "removed": [{removed}]}}"#)).unwrap();
    feed.release("tool", "1.1.0", "- tool: `--fast` flag removed", &tool("1.1.0"), &[]);
    let updater = Updater::open(config.clone()).unwrap();
    let status = |id: &str| updater.overview().products.into_iter().find(|p| p.id == id).unwrap();

    updater.detect().await;
    assert_eq!(updater.check("tool").await.unwrap().map(|r| r.version), Some(v("1.1.0")));
    assert!(updater.check("tool").await.unwrap().is_some(), "a 304 keeps the cached offer");
    assert_eq!(*feed.not_modified.lock().unwrap(), 1);
    let tool_status = status("tool");
    assert_eq!((tool_status.current, tool_status.newest), (Some(v("1.0.0")), Some(v("1.1.0"))));
    assert!(matches!(tool_status.installed, InstalledStatus::System(_)) && tool_status.changelog.contains("`--fast`"));

    assert!(matches!(updater.gate("tool").await, Err(Error::NotStaged { .. })));
    assert!(status("tool").error.is_some_and(|e| e.contains("not staged")));
    assert_eq!(updater.stage("tool").await.unwrap(), v("1.1.0"));
    assert_eq!(status("tool").error, None, "a success clears the error");

    // The reviewer flags the removal: held. Wait hides 1.1.0 until unpinned; pinning keeps 1.0.0.
    review(r#""tool: `--fast` flag removed""#);
    let report = updater.gate("tool").await.unwrap();
    assert_eq!(report.harmful().map(|c| c.check).collect::<Vec<_>>(), [GateCheck::Changelog]);
    assert_eq!(status("tool").held.map(|h| h.version), Some(v("1.1.0")));
    updater.choose("tool", HoldChoice::Wait).unwrap();
    assert_eq!((status("tool").offer, status("tool").waiting_past), (None, Some(v("1.1.0"))));
    updater.unpin("tool").unwrap();
    updater.gate("tool").await.unwrap();
    updater.choose("tool", HoldChoice::PinCurrent).unwrap();
    assert_eq!((status("tool").offer, status("tool").pin), (None, Some(v("1.0.0"))));
    assert!(matches!(updater.set_pins(BTreeMap::from([("nope".into(), v("1.0.0"))])), Err(Error::UnknownProduct(_))));
    updater.set_pins(BTreeMap::new()).unwrap();

    // Update anyway: deferred while a turn runs, swapped after.
    updater.gate("tool").await.unwrap();
    assert_eq!(updater.choose("tool", HoldChoice::UpdateAnyway).unwrap(), Some(v("1.1.0")));
    let turns = TurnGate::default();
    turns.begin();
    assert!(matches!(updater.apply_pending(&turns).as_slice(), [(_, Ok(SwapOutcome::Deferred))]));
    turns.end();
    assert!(matches!(updater.apply_pending(&turns).as_slice(), [(_, Ok(SwapOutcome::Swapped { from: None, .. }))]));
    let managed = updater.executable("tool").unwrap();
    assert!(managed.starts_with(dir.path().join("updates")));
    assert_eq!((status("tool").current, status("tool").installed), (Some(v("1.1.0")), InstalledStatus::Managed(managed)));

    // A clean 1.2.0 is pending at once; rollback goes back to 1.1.0.
    review("");
    feed.release("tool", "1.2.0", "- faster", &tool("1.2.0"), &[]);
    updater.check("tool").await.unwrap();
    updater.stage("tool").await.unwrap();
    let report = updater.gate("tool").await.unwrap();
    assert_eq!(report.checks.iter().map(|c| c.verdict).collect::<Vec<_>>(), [Verdict::Pass]);
    assert_eq!(updater.swap("tool", &turns).unwrap(), SwapOutcome::Swapped { from: Some(v("1.1.0")), to: v("1.2.0") });
    assert_eq!(updater.rollback("tool", &turns).unwrap(), SwapOutcome::Swapped { from: Some(v("1.2.0")), to: v("1.1.0") });

    // npm: deprecated versions and versions above `latest` are skipped; the tarball's `package/` is stripped.
    let pkg = |text: &[u8]| tgz(&[("package/bin/pkg", Err(text)), ("package/bin/alias", Ok("pkg"))]);
    feed.npm("pkg", "1.0.0", &[("0.9.0", &pkg(b"old"), false), ("1.0.0", &pkg(b"new"), false), ("1.1.0", &pkg(b"x"), true), ("2.0.0", &pkg(b"y"), false)]);
    assert_eq!(updater.check("pkg").await.unwrap().map(|r| r.version), Some(v("1.0.0")));
    updater.stage("pkg").await.unwrap();
    let payload = dir.path().join("updates/versions/pkg/1.0.0/payload/bin");
    assert_eq!((std::fs::read(payload.join("pkg")).unwrap(), std::fs::read(payload.join("alias")).unwrap()), (b"new".to_vec(), b"new".to_vec()));

    // Decisions, reports and offers survive a restart.
    let status = &Updater::open(config).unwrap().overview().products[0];
    assert_eq!((status.current.clone(), status.offer.as_ref().map(|r| r.version.clone())), (Some(v("1.1.0")), Some(v("1.2.0"))));
    assert!(status.report.as_ref().is_some_and(|r| r.is_clean()) && status.last_check_unix.is_some());
}

#[test]
fn archives_cannot_escape_the_payload() {
    let dir = tempfile::tempdir().unwrap();
    let cases: [&[Entry]; 3] =
        [&[("link", Ok("../../etc/passwd"))], &[("link", Ok("/etc/passwd"))], &[("a/link", Ok("../b/../../x"))]];
    for (i, entries) in cases.into_iter().enumerate() {
        let artifact = dir.path().join(format!("{i}.tgz"));
        std::fs::write(&artifact, tgz(entries)).unwrap();
        let error = TarGz { strip: 0 }.unpack(&artifact, &dir.path().join(format!("payload{i}"))).unwrap_err();
        assert!(matches!(error, Error::UnsafeEntry { .. }), "{i}: {error}");
    }
}

#[cfg(unix)]
#[test]
fn symlink_parents_cannot_escape_to_a_staged_artifact() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("attack.tgz");
    std::fs::write(&archive, tgz(&[("alias", Ok(".")), ("alias/launch", Ok("../artifact/tool-1.0.0"))])).unwrap();
    let error = TarGz { strip: 0 }.unpack(&archive, &dir.path().join("unpacked")).unwrap_err();
    assert!(matches!(error, Error::UnsafeEntry { .. }), "{error}");

    // Reject payloads staged by an older version too, when resolving their executable.
    let root = dir.path().join("updates");
    stage_fixture(&root, "tool", "1.0.0", b"artifact", None);
    let payload = root.join("versions/tool/1.0.0/payload");
    symlink(".", payload.join("alias")).unwrap();
    symlink("../artifact/tool-1.0.0", payload.join("launch")).unwrap();
    let mut product = spec("tool", ProductKind::Harness("tool".into()), github("tool"), None, Installed::Executable("tool".into()), Gates::None);
    product.executable = Some("alias/launch".into());
    let updater = Updater::open(UpdaterConfig::new(root.clone(), vec![product])).unwrap();
    Swapper::new(root).try_swap("tool", &v("1.0.0"), None, &TurnGate::default()).unwrap();
    assert!(updater.executable("tool").is_none());
}

#[tokio::test]
async fn app_updates_need_a_trusted_signature() {
    let dir = tempfile::tempdir().unwrap();
    let (feed, mut config) = setup(dir.path()).await;
    let app = spec("app", ProductKind::App, github("app"), Some("app-{version}"), Installed::Running(v("0.1.0")), Gates::None);
    config.products = vec![app];
    config.trusted_keys = vec![key(7).verifying_key()];
    let bytes = b"#!/bin/sh\necho loa 0.2.0\n";
    feed.release("app", "0.2.0", "- faster", bytes, &[("app-0.2.0.sig", sign_digest(bytes, &key(7)))]);
    let updater = Updater::open(config.clone()).unwrap();
    updater.check("app").await.unwrap();
    updater.stage("app").await.unwrap();
    assert!(updater.gate("app").await.unwrap().checks.is_empty(), "app updates rely on signatures");
    assert_eq!(updater.overview().products[0].signature, Some(SignatureStatus::Verified));
    assert!(matches!(updater.swap("app", &TurnGate::default()).unwrap(), SwapOutcome::Swapped { .. }));
    let status = &updater.overview().products[0];
    assert!(status.restart_required && status.current == Some(v("0.1.0")), "the old app runs until restart");

    // Unsigned, untrusted and unconfigured: never swapped.
    stage_fixture(&config.root, "app", "0.3.0", b"unsigned", None);
    stage_fixture(&config.root, "app", "0.4.0", b"stranger", Some(&key(9)));
    let swapper = Swapper::new(&config.root);
    let trusted = [key(7).verifying_key()];
    let idle = TurnGate::default();
    assert!(matches!(swapper.try_swap("app", &v("0.3.0"), Some(&trusted), &idle), Err(Error::Unsigned(_))));
    assert!(matches!(swapper.try_swap("app", &v("0.4.0"), Some(&trusted), &idle), Err(Error::Signature(_))));
    assert!(matches!(swapper.try_swap("app", &v("0.4.0"), None, &idle), Err(Error::Signature(_))));
}
