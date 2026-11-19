//! The updater area driven through `App::update` against a local release
//! feed and scripted executables: the product `fake` is the fake harness's
//! own CLI, so a swap changes what its new sessions start.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use loa_adapter::Capabilities;
use loa_bus::Policy;
use loa_ui_decisions::desk;
use loa_updater::{Feed, Gates, Installed, PrereleasePolicy, Product, ProductSpec};

use super::*;
use crate::app::tests::{deliver, fake_app, prompt, update};
use crate::app::{App, Message as AppMessage};

/// Releases of one GitHub repository and their files, served like GitHub's
/// API from a thread of its own (each `update` runs tasks on a fresh
/// runtime).
#[derive(Default)]
struct Served {
    /// Path of the release list, `/repos/<owner>/<repo>/releases`.
    releases_path: String,
    releases: Mutex<Vec<String>>,
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    etag: AtomicU32,
}

impl Served {
    /// Serves `owner/repo`'s releases; returns the server and its base URL.
    fn start(owner: &str, repo: &str) -> (Arc<Self>, String) {
        let served = Arc::new(Self { releases_path: format!("/repos/{owner}/{repo}/releases"), ..Self::default() });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let feed = Arc::clone(&served);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                respond(&feed, stream);
            }
        });
        (served, base)
    }

    /// Publishes `version` with `bytes` as its asset `name`.
    fn publish(&self, base: &str, version: &str, notes: &str, name: &str, bytes: Vec<u8>) {
        let file = std::env::temp_dir().join(format!("loa-updater-test-{}-{version}-{name}", std::process::id()));
        std::fs::write(&file, &bytes).unwrap();
        let digest: String =
            loa_updater::signature::artifact_digest(&file).unwrap().iter().map(|b| format!("{b:02x}")).collect();
        std::fs::remove_file(&file).unwrap();
        let key = format!("{version}-{name}");
        self.files.lock().unwrap().insert(key.clone(), bytes);
        let release = format!(
            r#"{{"tag_name":"v{version}","body":{notes:?},"assets":[{{"name":"{name}","browser_download_url":"{base}/files/{key}","digest":"sha256:{digest}"}}]}}"#
        );
        self.releases.lock().unwrap().push(release);
        self.etag.fetch_add(1, Ordering::SeqCst);
    }
}

struct Fixture {
    served: Arc<Served>,
    base: String,
    review: PathBuf,
    config: UpdaterConfig,
}

fn script(path: &Path, body: &str) -> PathBuf {
    loa_exec::write_executable(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    path.to_path_buf()
}

impl Fixture {
    /// `fake` 1.0.0 on the system (changelog gate) and an `omp` reviewer
    /// whose answer is read from `review`.
    fn new(dir: &Path) -> Self {
        let (served, base) = Served::start("acme", "fake");
        let review = dir.join("review.json");
        std::fs::write(&review, r#"{"breaking": [], "removed": []}"#).unwrap();
        let system = script(&dir.join("fake-cli"), "echo 'fake 1.0.0'");
        let reviewer = script(
            &dir.join("omp"),
            &format!("if [ \"$1\" = --version ]; then echo omp/9.0.0; else cat '{}'; fi", review.display()),
        );
        let spec = |id: &str, installed: PathBuf, asset: Option<String>| ProductSpec {
            product: Product {
                id: id.into(),
                kind: loa_updater::ProductKind::Harness(id.into()),
                feed: Feed::GitHub { owner: "acme".into(), repo: id.into() },
                prereleases: PrereleasePolicy::Stable,
                asset,
            },
            label: id.into(),
            installed: Installed::Executable(installed),
            executable: None,
            gates: Gates::Changelog,
        };
        let mut config = UpdaterConfig::new(
            dir.join("updates"),
            vec![spec("fake", system, Some("fake-{version}".into())), spec("omp", reviewer, None)],
        );
        config.github_api = base.clone();
        Self { served, base, review, config }
    }

    /// Publishes `fake` `version`: a CLI that reports it.
    fn publish(&self, version: &str, notes: &str) {
        let bytes = format!("#!/bin/sh\necho \"fake {version}\"\n").into_bytes();
        self.served.publish(&self.base, version, notes, &format!("fake-{version}"), bytes);
    }

    fn review(&self, removed: &[&str]) {
        let removed: Vec<String> = removed.iter().map(|entry| format!("{entry:?}")).collect();
        std::fs::write(&self.review, format!(r#"{{"breaking": [], "removed": [{}]}}"#, removed.join(","))).unwrap();
    }
}

/// Answers one request: the release list (conditionally) or a file.
fn respond(served: &Served, stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("/").split('?').next().unwrap_or("/").to_owned();
    let mut if_none_match = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("if-none-match")
        {
            if_none_match = Some(value.trim().to_owned());
        }
    }
    let etag = format!("\"{}\"", served.etag.load(Ordering::SeqCst));
    let (status, extra, body) = if path == served.releases_path {
        if if_none_match.as_deref() == Some(etag.as_str()) {
            ("304 Not Modified", String::new(), Vec::new())
        } else {
            let list = format!("[{}]", served.releases.lock().unwrap().join(","));
            ("200 OK", format!("ETag: {etag}\r\nContent-Type: application/json\r\n"), list.into_bytes())
        }
    } else if path.starts_with("/repos/") {
        ("200 OK", "Content-Type: application/json\r\n".to_owned(), b"[]".to_vec())
    } else if let Some(bytes) = path.strip_prefix("/files/").and_then(|name| served.files.lock().unwrap().get(name).cloned()) {
        ("200 OK", String::new(), bytes)
    } else {
        ("404 Not Found", String::new(), Vec::new())
    };
    let mut stream = stream;
    let head = format!("HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(&body));
}

fn app_with(dir: &Path, fixture: &Fixture) -> App {
    let mut app = fake_app(dir, &["fake"]);
    let updater = Updater::open(fixture.config.clone()).unwrap();
    app.areas.updater.replace_updater(updater, &mut app.ctx);
    app
}

fn screen(app: &mut App, message: updates::Message) {
    update(app, AppMessage::Updater(Message::Screen(message)));
}

fn fake(app: &App) -> updates::Product {
    app.areas.updater.screen().data.products.iter().find(|p| p.id == "fake").cloned().expect("listed")
}

fn notice(app: &App) -> String {
    app.ctx.notice().unwrap_or_default().to_owned()
}

/// Opens the palette, types `query` and runs the first match.
fn palette(app: &mut App, query: &str) {
    update(app, AppMessage::Palette(palette::Message::Open));
    update(app, AppMessage::Palette(palette::Message::QueryChanged(query.to_owned())));
    update(app, AppMessage::Palette(palette::Message::Submit));
}

#[test]
fn updates_screen_checks_stages_gates_holds_pins_and_swaps_between_turns() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(dir.path());
    fixture.publish("1.1.0", "- fake: `--fast` flag removed");
    let mut app = app_with(dir.path(), &fixture);
    assert_eq!(app.ctx.shell.updates(), 0, "nothing checked yet");

    palette(&mut app, "Check for updates");
    assert_eq!(app.ctx.screen(), Some(AreaId::Updater));
    let product = fake(&app);
    assert_eq!((product.current.as_deref(), product.offer.as_deref()), (Some("1.0.0"), Some("1.1.0")));
    assert!(product.changelog.contains("`--fast` flag removed"), "{}", product.changelog);
    assert!(app.areas.updater.screen().busy("fake").is_none(), "the check finished");
    assert_eq!(app.ctx.shell.updates(), 1, "the sidebar lists the update");

    // The sidebar row opens the screen.
    app.ctx.close_screen();
    update(&mut app, AppMessage::Shell(shell_sidebar(loa_ui_shell::sidebar::Message::OpenUpdates)));
    assert_eq!(app.ctx.screen(), Some(AreaId::Updater));

    screen(&mut app, updates::Message::Stage("fake".into()));
    assert_eq!(fake(&app).staged, vec!["1.1.0".to_owned()]);

    // The changelog review flags the removal: held, with the three choices.
    fixture.review(&["fake: `--fast` flag removed"]);
    screen(&mut app, updates::Message::Gate("fake".into()));
    let product = fake(&app);
    assert_eq!(product.held, Some(updates::Held { version: "1.1.0".into(), harmful: 1 }));
    assert!(product.report.is_some_and(|report| report.checks.iter().any(|check| !check.pass)));
    assert!(notice(&app).contains("held"), "{}", notice(&app));

    // Pin current: written to the central config; unpin removes it.
    screen(&mut app, updates::Message::Choose("fake".into(), updates::Choice::PinCurrent));
    let pins = app.ctx.services.config.load().unwrap().config.updater.pins;
    assert_eq!(pins.get("fake").map(String::as_str), Some("1.0.0"));
    assert_eq!((fake(&app).pin.as_deref(), fake(&app).offer), (Some("1.0.0"), None));
    screen(&mut app, updates::Message::Unpin("fake".into()));
    assert!(app.ctx.services.config.load().unwrap().config.updater.pins.is_empty());
    assert_eq!(fake(&app).offer.as_deref(), Some("1.1.0"));

    // Update anyway while a turn runs: the swap waits for the turn to end.
    screen(&mut app, updates::Message::Gate("fake".into()));
    update(&mut app, AppMessage::NewSession);
    let session = app.ctx.sessions.active().expect("started");
    let id = session.id().clone();
    let (_, mut feed) = session.writer().subscribe(Policy::every_patch(1024));
    prompt(&mut app, "ask");
    deliver(&mut app, &id, &mut feed);
    assert!(app.areas.updater.turn_running());
    screen(&mut app, updates::Message::Choose("fake".into(), updates::Choice::UpdateAnyway));
    assert!(notice(&app).contains("swaps in once the running turn ends"), "{}", notice(&app));
    assert_eq!(fake(&app).pending_swap.as_deref(), Some("1.1.0"));
    assert_eq!(fake(&app).current.as_deref(), Some("1.0.0"));

    let decision = app.areas.decisions.focused().expect("the turn waits on a decision");
    let choose = desk::Message::Card(id.to_string(), loa_ui_decisions::Message::Choose(decision, 0));
    update(&mut app, AppMessage::Decisions(crate::areas::decisions::Message::Desk(choose)));
    deliver(&mut app, &id, &mut feed);
    assert!(!app.areas.updater.turn_running(), "turn ended");
    assert!(notice(&app).contains("fake is now 1.1.0"), "{}", notice(&app));
    let product = fake(&app);
    assert_eq!((product.current.as_deref(), product.pending_swap), (Some("1.1.0"), None));
    let updates::Installed::Managed(managed) = product.installed else { panic!("managed: {:?}", product.installed) };

    // New sessions of the harness start the swapped in version.
    let capabilities = Capabilities::default();
    let mut config = SessionConfig::default();
    app.areas.updater.contribute(&mut config, &Target { harness: "fake", capabilities: &capabilities, resume: false }, &app.ctx);
    assert_eq!(config.executable, Some(PathBuf::from(&managed)));

    // A clean release swaps in at once; rollback goes back one version.
    fixture.review(&[]);
    fixture.publish("1.2.0", "- faster startup");
    screen(&mut app, updates::Message::Check("fake".into()));
    assert_eq!(fake(&app).offer.as_deref(), Some("1.2.0"));
    screen(&mut app, updates::Message::Stage("fake".into()));
    screen(&mut app, updates::Message::Gate("fake".into()));
    assert!(notice(&app).contains("fake is now 1.2.0"), "{}", notice(&app));
    assert_eq!(fake(&app).active.map(|active| active.previous), Some(Some("1.1.0".into())));
    screen(&mut app, updates::Message::Rollback("fake".into()));
    assert!(notice(&app).contains("fake is now 1.1.0"), "{}", notice(&app));
    assert_eq!(fake(&app).current.as_deref(), Some("1.1.0"));
}

fn shell_sidebar(message: loa_ui_shell::sidebar::Message) -> loa_ui_shell::Message {
    loa_ui_shell::Message::Sidebar(message)
}

#[test]
fn peers_behind_come_from_the_peers_the_node_knows() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(dir.path());
    let mut app = app_with(dir.path(), &fixture);
    let key = |seed: u8| {
        let identity = loa_identity::NodeIdentity::from_secret_bytes(&[seed; 32]);
        loa_node::NodeId::from_bytes(identity.public_key().as_bytes()).unwrap()
    };
    let peer = |seed: u8, name: &str, version: Option<&str>, protocols: &[&str]| loa_node::PeerInfo {
        key: key(seed),
        name: Some(name.into()),
        addresses: Vec::new(),
        source: loa_node::DiscoverySource::Manual,
        online: true,
        last_seen_ms: None,
        app_version: version.map(str::to_owned),
        transport: None,
        shared_protocol: None,
        protocols: protocols.iter().map(|p| (*p).to_owned()).collect(),
    };
    let ours = loa_node::local_protocols();
    let ours: Vec<&str> = ours.iter().map(String::as_str).collect();
    app.ctx.peers = vec![
        peer(1, "old-box", Some("0.0.1-alpha"), &ours),
        peer(2, "same-box", Some(env!("CARGO_PKG_VERSION")), &ours),
        peer(3, "never-seen", Some("0.0.1"), &[]),
    ];

    update(&mut app, AppMessage::Updater(Message::Open));
    let peers = &app.areas.updater.screen().data.peers;
    let names: Vec<_> = peers.iter().map(|p| p.name.as_deref().unwrap_or_default()).collect();
    assert_eq!(names, ["old-box", "same-box"], "only peers whose hello was seen");
    assert!(matches!(peers[0].standing, updates::Standing::Behind { .. }), "{:?}", peers[0].standing);
    assert!(peers.iter().all(|p| p.can_talk));
    assert_eq!(peers[1].standing, updates::Standing::Current);
}

/// Like `update`, without the helper's 10 s limit per task: live feeds,
/// probes and the changelog review take longer.
fn update_slowly(app: &mut App, message: AppMessage) {
    use iced::futures::StreamExt;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut pending = vec![app.update(message)];
    while let Some(task) = pending.pop() {
        let Some(stream) = iced_runtime::task::into_stream(task) else { continue };
        let outputs = runtime.block_on(
            stream
                .filter_map(|action| async move {
                    match action {
                        iced_runtime::Action::Output(message) => Some(message),
                        _ => None,
                    }
                })
                .collect::<Vec<_>>(),
        );
        for message in outputs {
            pending.push(app.update(message));
        }
    }
}

fn installed_omp_version(omp: &Path) -> Version {
    let output = std::process::Command::new(omp).arg("--version").output().expect("omp runs");
    loa_updater::installed::parse_version(&String::from_utf8_lossy(&output.stdout)).expect("omp reports its version")
}

fn row(app: &App, id: &str) -> updates::Product {
    app.areas.updater.screen().data.products.iter().find(|p| p.id == id).cloned().expect("listed")
}

#[test]
#[ignore = "live: reads the real npm and GitHub feeds and runs the installed omp; the changelog review calls a model"]
fn live_updates_screen_reads_real_feeds_and_gates_the_installed_omp_as_old_and_new() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = fake_app(dir.path(), &["fake"]);
    let version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
    let omp = loa_updater::installed::find_executable(Path::new("omp")).expect("omp on PATH");
    let installed = installed_omp_version(&omp);

    // The real feeds through the screen, with omp read from npm.
    let mut products = default_products(version.clone());
    let spec = products.iter_mut().find(|spec| spec.product.id == "omp").unwrap();
    spec.product.feed = Feed::Npm { package: "@oh-my-pi/pi-coding-agent".into() };
    spec.product.asset = None;
    let updater = Updater::open(UpdaterConfig::new(dir.path().join("live-feeds"), products)).unwrap();
    app.areas.updater.replace_updater(updater, &mut app.ctx);
    update_slowly(&mut app, AppMessage::Updater(Message::Screen(updates::Message::CheckAll)));
    for product in &app.areas.updater.screen().data.products {
        println!(
            "{}: {} current {:?} newest {:?} offer {:?} error {:?}",
            product.id, product.source, product.current, product.newest, product.offer, product.error
        );
    }
    let omp_row = row(&app, "omp");
    assert_eq!(omp_row.source, "npm @oh-my-pi/pi-coding-agent");
    assert_eq!(omp_row.current, Some(installed.to_string()));
    assert_eq!(omp_row.error, None);
    let newest = Version::parse(omp_row.newest.as_deref().expect("npm lists omp")).unwrap();
    assert!(newest >= installed, "npm's newest {newest} is older than the installed {installed}");
    for harness in ["claude", "codex", "opencode"] {
        let harness = row(&app, harness);
        assert!(harness.newest.is_some() && harness.error.is_none(), "{harness:?}");
    }
    // A second check revalidates with the stored ETag and keeps the list.
    update_slowly(&mut app, AppMessage::Updater(Message::Screen(updates::Message::Check("omp".into()))));
    assert_eq!(row(&app, "omp").newest, Some(newest.to_string()));

    // Gates with the installed omp as both old and new: a wrapper around it
    // is published as the next patch on a local GitHub feed and staged.
    let next = Version::new(installed.major, installed.minor, installed.patch + 1);
    let (served, base) = Served::start("can1357", "oh-my-pi");
    let wrapper = format!("#!/bin/sh\nexec '{}' \"$@\"\n", omp.display()).into_bytes();
    let notes = "### Fixed\n- The `read` tool no longer cuts long lines short\n- Faster startup when many skills are installed";
    served.publish(&base, &next.to_string(), notes, &format!("omp-{}", loa_updater::service::platform()), wrapper);
    let spec = default_products(version).into_iter().find(|spec| spec.product.id == "omp").unwrap();
    let mut config = UpdaterConfig::new(dir.path().join("live-gates"), vec![spec]);
    config.github_api = base;
    config.review_model = std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".to_owned());
    app.areas.updater.replace_updater(Updater::open(config).unwrap(), &mut app.ctx);
    update_slowly(&mut app, AppMessage::Updater(Message::Screen(updates::Message::Check("omp".into()))));
    assert_eq!(row(&app, "omp").offer, Some(next.to_string()));
    update_slowly(&mut app, AppMessage::Updater(Message::Screen(updates::Message::Stage("omp".into()))));
    assert_eq!(row(&app, "omp").staged, vec![next.to_string()]);
    update_slowly(&mut app, AppMessage::Updater(Message::Screen(updates::Message::Gate("omp".into()))));

    let omp_row = row(&app, "omp");
    let report = omp_row.report.clone().expect("gate report");
    for check in &report.checks {
        println!("{} {}: {}", if check.pass { "pass" } else { "HARMFUL" }, check.label, check.details.join(" | "));
    }
    println!("notice: {:?}", app.ctx.notice());
    assert_eq!(report.checks.len(), 6, "every omp check ran");
    for check in report.checks.iter().filter(|check| check.label != "Changelog") {
        assert!(check.pass, "{} must pass for the same omp: {:?}", check.label, check.details);
    }
    if report.checks.iter().all(|check| check.pass) {
        assert!(notice(&app).contains(&format!("omp is now {next}")), "{}", notice(&app));
    } else {
        assert_eq!(omp_row.held.map(|held| held.version), Some(next.to_string()));
    }
    assert_eq!(installed_omp_version(&omp), installed, "the installed omp is untouched");
}
