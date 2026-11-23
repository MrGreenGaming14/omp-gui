use crate::digest::{digest_of, digest_state};
use crate::digester::{Digester, PromptLimits, build_prompt, parse_answer};
use crate::rank::{Bm25, Candidate, RankParams, Recency, Scorer, Weighted, rank_and_pack};
use crate::read::{ReadRequest, read_subtree};
use crate::rebuild::{Decision, RebuildDriver, SessionWriter, decide};
use crate::search::{AttrFilter, SearchQuery, TextMode, search};
use crate::settings::exclude_draft;
use crate::source::{Access, SessionSource, visible_sessions};
use crate::testing::{Builder, Fake, MemorySource, Only, live};
use crate::*;
use loa_bus::{Bus, JournalOptions, Store};
use loa_core::{ElementKind as K, NodeId};
use loa_contracts::harness::HostTools;
use loa_settings::projects::{DigestState, ProjectSettings};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// a: an older session with a todo and a cargo call; b: newer, digested; c: another project.
fn fixture() -> (MemorySource, Builder, Builder) {
    let a = Builder::new("a", "loa", 100);
    let t = a.turn("Set up the Iroh endpoint", "Bound it to port 31415.", 110);
    a.call(t, "bash", "cargo test -p net", "ok: 12 passed");
    a.commit(loa_core::Draft::insert(a.root, K::Todo, [(loa_core::attr::STATE, loa_core::Value::from("pending"))], "write docs"));
    let b = Builder::new("b", "loa", 200);
    b.turn("Theme the sidebar", "Dark tokens done.", 210);
    b.digest("Themed the sidebar with dark tokens.");
    let c = Builder::new("c", "web", 300);
    c.turn("iroh elsewhere", "no", 310);
    (MemorySource::with([&a, &b, &c]), a, b)
}

async fn ids(source: &MemorySource, access: &dyn Access) -> Vec<String> {
    visible_sessions(source, access, "loa").await.unwrap().into_iter().map(|s| s.id).collect()
}

#[tokio::test]
async fn access_decides_what_every_path_sees() {
    let (source, ..) = fixture();
    for (access, visible) in [(&LocalOnly as &dyn Access, vec!["b", "a"]), (&Only(&["a"]), vec!["a"]), (&Only(&[]), vec![])] {
        assert_eq!(ids(&source, access).await, visible);
        let query = SearchQuery { text: Some("iroh".into()), ..Default::default() };
        let hits = search(&source, access, "loa", &query).await.unwrap().hits;
        assert!(hits.iter().all(|h| visible.contains(&h.session.as_str())));
        let read = read_subtree(&source, access, "loa", &ReadRequest { session: "b".into(), node: None, offset: 0, max_bytes: 0 }).await;
        assert_eq!(read.is_ok(), visible.contains(&"b"), "{read:?}");
    }
    let other = ReadRequest { session: "c".into(), node: None, offset: 0, max_bytes: 0 };
    assert!(matches!(read_subtree(&source, &LocalOnly, "loa", &other).await, Err(Error::SessionNotFound(_))));
}

#[tokio::test]
async fn host_tools_search_and_read_respect_session_boundaries() {
    let (source, ..) = fixture();
    let source = Arc::new(source);
    let tools = crate::tools::tool_set(source.clone(), Arc::new(LocalOnly));

    let result = HostTools::call(&tools, "b", "project_search", r#"{"query":"iroh"}"#).await.unwrap();
    let page: serde_json::Value = serde_json::from_str(&result).unwrap();
    let hits = page["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["session"], "a");
    let args = serde_json::json!({"session": hits[0]["session"], "node": hits[0]["node"]});
    let text = HostTools::call(&tools, "b", "project_read", &args.to_string()).await.unwrap();
    assert!(text.contains("Iroh endpoint"));

    let own = HostTools::call(&tools, "b", "project_search", r#"{"query":"dark tokens"}"#).await.unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&own).unwrap()["total"], 0);
    let own = HostTools::call(&tools, "b", "project_search", r#"{"query":"dark tokens","sessions":["b"]}"#).await.unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&own).unwrap()["total"].as_u64().unwrap() > 0);

    let restricted = crate::tools::tool_set(source, Arc::new(Only(&["b"])));
    let denied = HostTools::call(&restricted, "b", "project_search", r#"{"query":"iroh"}"#).await.unwrap();
    assert_eq!(serde_json::from_str::<serde_json::Value>(&denied).unwrap()["total"], 0);
    assert!(HostTools::call(&restricted, "b", "project_read", &args.to_string()).await.is_err());
    assert!(HostTools::call(&tools, "b", "project_read", r#"{"session":"c"}"#).await.is_err());
    for (tool, args) in [
        ("project_search", "{"),
        ("project_search", r#"{"element":"not-an-element"}"#),
        ("project_search", r#"{"unexpected":true}"#),
        ("project_read", r#"{"node":1}"#),
        ("project_read", r#"{"session":"a","node":"wrong"}"#),
    ] {
        assert!(HostTools::call(&tools, "b", tool, args).await.is_err(), "{tool}: {args}");
    }
}

#[tokio::test]
async fn search_cases() {
    let (source, ..) = fixture();
    let q = |text: Option<&str>, mode, kind, attrs: &[(&str, Option<&str>)]| SearchQuery {
        text: text.map(Into::into),
        mode,
        kind,
        attrs: attrs.iter().map(|(n, v)| AttrFilter { name: (*n).into(), value: v.map(Into::into) }).collect(),
        ..Default::default()
    };
    let (phrase, all) = (TextMode::Phrase, TextMode::AllWords);
    let cases = [
        (q(Some("IROH endpoint"), phrase, None, &[]), vec![("a", K::Message)]),
        (q(Some("port endpoint"), phrase, None, &[]), vec![]),
        (q(Some("31415 bound"), all, None, &[]), vec![("a", K::Message)]),
        (q(Some("12 passed"), phrase, Some(K::Call), &[]), vec![("a", K::Call)]),
        (q(None, phrase, Some(K::Todo), &[]), vec![("a", K::Todo)]),
        (q(None, phrase, None, &[("tool", Some("BA"))]), vec![("a", K::Call)]),
        (q(Some("dark tokens"), phrase, Some(K::Digest), &[]), vec![("b", K::Digest)]),
    ];
    for (query, want) in cases {
        let page = search(&source, &LocalOnly, "loa", &query).await.unwrap();
        let got: Vec<(&str, K)> = page.hits.iter().map(|h| (h.session.as_str(), h.kind)).collect();
        assert_eq!(got, want, "{query:?}");
    }
    assert!(matches!(search(&source, &LocalOnly, "loa", &q(Some("  "), phrase, None, &[])).await, Err(Error::Arguments(_))));
    // Pages cover every hit exactly once.
    let mut query = q(None, phrase, Some(K::Message), &[]);
    query.limit = 3;
    let (mut seen, mut pages) = (Vec::new(), 0);
    loop {
        let page = search(&source, &LocalOnly, "loa", &query).await.unwrap();
        assert_eq!(page.total, 4);
        seen.extend(page.hits.into_iter().map(|h| (h.session, h.node)));
        pages += 1;
        match page.next_cursor {
            Some(next) => query.cursor = Some(next),
            None => break,
        }
    }
    seen.dedup();
    assert_eq!((seen.len(), pages), (4, 2));
}

#[tokio::test]
async fn read_pages_cover_the_rendering_exactly() {
    let a = Builder::new("a", "loa", 1);
    let t = a.turn("héllo wörld ✓ ".repeat(40).as_str(), "ok", 2);
    let source = MemorySource::with([&a]);
    let whole = crate::read::render_subtree(&a.dom(), t);
    for max_bytes in [1, 7, 64, 0, 100_000] {
        let (mut text, mut offset) = (String::new(), Some(0));
        while let Some(at) = offset {
            let request = ReadRequest { session: "a".into(), node: Some(t), offset: at, max_bytes };
            let page = read_subtree(&source, &LocalOnly, "loa", &request).await.unwrap();
            assert_eq!((page.offset, page.total_bytes), (at, whole.len()));
            assert!(!page.text.is_empty() && page.text.len() <= crate::read::PAGES.size(max_bytes).max(4));
            text += &page.text;
            offset = page.next_offset;
        }
        assert_eq!(text, whole, "max_bytes {max_bytes}");
    }
    let missing = ReadRequest { session: "a".into(), node: Some(NodeId(9_999)), offset: 0, max_bytes: 0 };
    assert!(matches!(read_subtree(&source, &LocalOnly, "loa", &missing).await, Err(Error::NodeNotFound { .. })));
}

fn candidate(session: &str, summary: &str, last_active: i64, excluded: bool) -> Candidate {
    let b = Builder::new(session, "loa", last_active);
    b.turn("x", "y", last_active);
    b.digest(summary);
    Candidate { excluded, ..Candidate::from_dom(&b.dom()).unwrap() }
}

#[test]
fn rank_cases() {
    let day = 24 * 3600 * 1000;
    let pool = || {
        vec![
            candidate("old", "Configured the iroh relay and iroh endpoint", 0, false),
            candidate("new", "Styled buttons", 10 * day, false),
            candidate("off", "iroh iroh iroh", 10 * day, true),
        ]
    };
    let order = |query: &str, weight: f64, budget: usize| {
        let params = RankParams { relevance_weight: weight, ..RankParams::new(10 * day, budget) };
        rank_and_pack(pool(), query, &params).into_iter().map(|r| r.candidate.session).collect::<Vec<_>>()
    };
    assert_eq!(order("iroh", 1.0, 10_000), ["old", "new"]);
    assert_eq!(order("iroh", 0.0, 10_000), ["new", "old"]);
    assert_eq!(order("", 0.7, 10_000), ["new", "old"]);
    assert_eq!(order("iroh", 1.0, 1), Vec::<String>::new());
    let (c, q) = (candidate("x", "iroh", 0, false), vec!["iroh".to_owned()]);
    let half = Recency { now: day, half_life_ms: day };
    let mix = Weighted(vec![(0.5, Box::new(Bm25::fit(std::slice::from_ref(&c), &q))), (0.5, Box::new(half))]);
    assert!((mix.score(&q, &c) - 0.75).abs() < 1e-9);
}

#[tokio::test(start_paused = true)]
async fn digest_staleness_and_rebuilds() {
    let s = Builder::new("s", "loa", 1);
    assert_eq!((decide(&s.dom()), digest_state(&s.dom())), (Decision::Empty, DigestState::Missing));
    s.turn("Fix the build", "Done.", 2);
    assert_eq!(decide(&s.dom()), Decision::Rebuild);
    s.digest("Fixed it.");
    assert_eq!((decide(&s.dom()), digest_state(&s.dom())), (Decision::UpToDate, DigestState::Fresh));
    s.turn("More", "Sure.", 3);
    assert_eq!(digest_state(&s.dom()), DigestState::Stale);
    s.commit(exclude_draft(&s.dom(), true).unwrap());
    assert_eq!(decide(&s.dom()), Decision::Excluded);
    s.commit(exclude_draft(&s.dom(), false).unwrap());

    // A burst of triggers costs one call; a failure keeps the old digest, flagged stale.
    for fail in [false, true] {
        let source = Arc::new(MemorySource::with([&s]));
        let fake = Arc::new(Fake { fail, ..Default::default() });
        let digester = Digester::new(Arc::new(fake.clone()), PromptLimits::default());
        let driver = RebuildDriver { source: source.clone(), writer: source, digester, debounce: Duration::from_millis(20), settings: None };
        let (tx, mut out) = driver.spawn();
        (0..3).for_each(|_| tx.send("s".into()).unwrap());
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(20)).await;
        let outcome = out.recv().await.unwrap();
        assert_eq!((fake.calls.load(Ordering::SeqCst), matches!(outcome, RebuildOutcome::Failed { .. })), (1, fail));
        let digest = digest_of(&s.dom()).unwrap();
        assert_eq!(digest.content.summary, "digest 1");
        assert_eq!(digest_state(&s.dom()), if fail { DigestState::Stale } else { DigestState::Fresh });
        if !fail {
            s.turn("Again", "Ok.", 4);
        }
    }
}

#[test]
fn digest_answers_are_validated() {
    let s = Builder::new("s", "loa", 1);
    let t1 = s.turn("one", "a", 2);
    s.turn("two", "b", 3);
    let prompt = build_prompt(&s.dom(), &PromptLimits::default()).unwrap();
    assert!(prompt.text.contains("### T2"));
    let answer = |summary: &str, pointers: &str| format!(r#"x {{"summary":"{summary}","decisions":[" d "],"files":[],"open_items":[],"pointers":[{pointers}]}} y"#);
    let ok = parse_answer(&answer("did it", r#""T1","t1""#), &prompt).unwrap();
    assert_eq!((ok.pointers, ok.decisions), (vec![t1], vec!["d".to_owned()]));
    for bad in [answer("", ""), answer("s", r#""T3""#), answer("s", r#""X1""#), "no json".into()] {
        assert!(matches!(parse_answer(&bad, &prompt), Err(Error::InvalidAnswer(_))), "{bad}");
    }
}

#[tokio::test]
async fn live_sessions_read_and_write_live_and_stored_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path(), JournalOptions::default()).unwrap();
    let bus = Arc::new(Bus::new());
    drop(live(&bus, &store, "closed", Some("loa"), "/home/u/loa", 1));
    drop(bus.close("closed"));
    let open = live(&bus, &store, "open", Some("loa"), "/home/u/loa", 1);
    // A session without a PROJECT attr belongs to no project, whatever its cwd.
    let _stray = live(&bus, &store, "stray", None, "/home/u/loa", 1);
    let _web = live(&bus, &store, "web", None, "/home/u/web", 1);
    let sessions = LiveSessions::new(bus, store.clone());
    let mut listed: Vec<String> = sessions.list("loa").await.unwrap().into_iter().map(|i| i.id).collect();
    listed.sort();
    assert_eq!(listed, ["closed", "open"]);
    assert!(sessions.list("web").await.unwrap().is_empty());
    assert!(sessions.load("../etc").await.unwrap().is_none());
    let dom = sessions.load("closed").await.unwrap().unwrap();
    SessionWriter::commit(&sessions, "closed", exclude_draft(&dom, true).unwrap()).unwrap();
    assert!(is_excluded(&store.open("closed").unwrap().0.load().unwrap()));
    let dom = sessions.load("open").await.unwrap().unwrap();
    sessions.commit("open", exclude_draft(&dom, true).unwrap()).await.unwrap();
    assert!(open.with_dom(is_excluded));
}

#[tokio::test]
async fn file_store_names_lists_projects_and_skips_strays() {
    use crate::settings::SettingsStore;
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    let store = FileSettingsStore::new(&projects);
    assert!(store.names().await.unwrap().is_empty());
    let saved = ["my/proj", "with space", "a.b", ".", "héllo", "loa"];
    for name in saved {
        store.save(name, &ProjectSettings::default()).await.unwrap();
    }
    let mut expected = saved.to_vec();
    expected.sort();
    assert_eq!(store.names().await.unwrap(), expected);
    // Strays: no `.json` suffix, a bad escape, a non-canonical escape, a directory.
    std::fs::write(projects.join("notes.txt"), "x").unwrap();
    std::fs::write(projects.join("bad%zz.json"), "{}").unwrap();
    std::fs::write(projects.join("my%2fproj.json"), "{}").unwrap();
    std::fs::create_dir(projects.join("dir.json")).unwrap();
    assert_eq!(store.names().await.unwrap(), expected);
    // The folder round-trips; removing forgets a project and tolerates a missing one.
    let settings = ProjectSettings { folder: Some("/work/my-proj".into()), ..ProjectSettings::default() };
    store.save("my/proj", &settings).await.unwrap();
    assert_eq!(FileSettingsStore::new(&projects).load("my/proj").await.unwrap(), settings);
    store.remove("my/proj").await.unwrap();
    store.remove("my/proj").await.unwrap();
    assert_eq!(store.load("my/proj").await.unwrap(), ProjectSettings::default());
}

/// Replaces `project`'s ideas notes with `text` over the file as it is now.
async fn write_ideas(store: &FileSettingsStore, project: &str, text: &str) {
    use crate::settings::SettingsStore;
    let seen = store.load_ideas(project).await.unwrap().file;
    store.save_ideas(project, seen, text).await.unwrap();
}

#[tokio::test]
async fn file_store_keeps_each_projects_ideas_next_to_its_settings() {
    use crate::settings::SettingsStore;
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    let store = FileSettingsStore::new(&projects);
    assert_eq!(store.load_ideas("loa").await.unwrap().text, "", "no notes yet");
    let windows = "# Ideas\r\n\r\n- try iroh relays\r\n";
    store.save("loa", &ProjectSettings::default()).await.unwrap();
    write_ideas(&store, "loa", windows).await;
    write_ideas(&store, "my/proj", "- web only").await;
    assert_eq!(std::fs::read(projects.join("loa.ideas.md")).unwrap(), windows.as_bytes(), "a Markdown file, byte for byte");
    assert_eq!(store.load_ideas("loa").await.unwrap().text, windows);
    assert_eq!(store.load_ideas("my/proj").await.unwrap().text, "- web only");
    assert_eq!(store.names().await.unwrap(), ["loa"], "notes alone don't make a project");
    std::fs::write(projects.join("bad.ideas.md"), [0xff, 0xfe]).unwrap();
    let error = store.load_ideas("bad").await.unwrap_err().to_string();
    assert!(error.contains("bad.ideas.md"), "notes that aren't UTF-8 are reported, not mangled: {error}");

    // The share toggle is off for settings written before it existed, and stays out of the file while off.
    std::fs::write(projects.join("old.json"), r#"{"instructions": "Use rustls."}"#).unwrap();
    assert!(!store.load("old").await.unwrap().ideas_in_context);
    assert!(!std::fs::read_to_string(projects.join("loa.json")).unwrap().contains("ideas_in_context"));

    store.remove("loa").await.unwrap();
    assert!(!projects.join("loa.ideas.md").exists() && !projects.join("loa.json").exists());
    assert_eq!(store.load_ideas("my/proj").await.unwrap().text, "- web only", "other projects keep theirs");
    std::fs::create_dir(projects.join("locked.ideas.md")).unwrap();
    let error = store.remove("locked").await.unwrap_err().to_string();
    assert!(error.contains("locked.ideas.md"), "a failed delete names the file: {error}");
}

#[tokio::test]
async fn saving_ideas_never_replaces_an_edit_made_outside_since_they_were_loaded() {
    use crate::settings::SettingsStore;
    let dir = tempfile::tempdir().unwrap();
    let store = FileSettingsStore::new(dir.path());
    let path = dir.path().join("loa.ideas.md");
    let missing = store.load_ideas("loa").await.unwrap().file;
    std::fs::write(&path, "written outside").unwrap();
    assert!(matches!(store.save_ideas("loa", missing, "typed in Loa").await, Err(Error::ChangedOnDisk { path: changed }) if changed == path));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "written outside", "a file created since stays too");

    let seen = store.load_ideas("loa").await.unwrap().file;
    let saved = store.save_ideas("loa", seen.clone(), "first save").await.unwrap();
    let saved = store.save_ideas("loa", saved, "second save").await.unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "second save", "the file as written is the base of the next save");
    assert!(matches!(store.save_ideas("loa", seen, "stale").await, Err(Error::ChangedOnDisk { .. })));
    std::fs::write(&path, "edited outside again").unwrap();
    assert!(matches!(store.save_ideas("loa", saved.clone(), "stale").await, Err(Error::ChangedOnDisk { .. })));
    assert!(store.save_ideas("other", saved, "elsewhere").await.is_err(), "a project's file is only saved as its own");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited outside again");
    assert!(!dir.path().join("other.ideas.md").exists());
}

#[tokio::test]
async fn ideas_join_the_context_only_while_shared_and_count_against_the_budget() {
    use crate::context::{ContextRequest, build_context, render_markdown};
    use crate::settings::SettingsStore;
    let (source, ..) = fixture();
    let dir = tempfile::tempdir().unwrap();
    let store = FileSettingsStore::new(dir.path());
    let request = ContextRequest { project: "loa", session: None, query: "", budget_tokens: 4_000, now: 1_000 };
    let notes = format!("- try iroh relays\n{}", "x".repeat(4 * 4_000));
    write_ideas(&store, "loa", &notes).await;

    let off = build_context(&source, &LocalOnly, &store, &request).await.unwrap();
    assert_eq!((off.ideas.as_deref(), off.digests.len()), (None, 1), "not shared by default");
    assert!(!render_markdown(&off).contains("try iroh relays"));

    store.save("loa", &ProjectSettings { instructions: "Use rustls.".into(), ideas_in_context: true, ..ProjectSettings::default() }).await.unwrap();
    let on = build_context(&source, &LocalOnly, &store, &request).await.unwrap();
    assert!(render_markdown(&on).contains("## Ideas and notes\n\nThe user's own development ideas and notes for this project.\n\n- try iroh relays\n"));
    assert!(on.digests.is_empty(), "the notes take the budget the digest needed");

    write_ideas(&store, "loa", " \r\n").await;
    let blank = build_context(&source, &LocalOnly, &store, &request).await.unwrap();
    assert_eq!((blank.ideas, blank.digests.len()), (None, 1), "blank notes add nothing");

    std::fs::write(dir.path().join("loa.ideas.md"), [b'-', b' ', 0x93, b'q', 0x94]).unwrap();
    let unreadable = build_context(&source, &LocalOnly, &store, &request).await.unwrap();
    assert_eq!((unreadable.ideas.as_deref(), unreadable.digests.len()), (None, 1), "notes that aren't UTF-8 are left out, the rest goes in");
    assert!(render_markdown(&unreadable).contains("Use rustls."));
}

#[tokio::test]
async fn unreadable_ideas_still_show_the_rest_of_the_project() {
    let env = env();
    let _a = open(&env, "a", "loa", "/home/u/loa", 10);
    std::fs::create_dir_all(env._dir.path().join("projects")).unwrap();
    std::fs::write(env._dir.path().join("projects").join("loa.ideas.md"), [0xff, 0xfe, b'h', 0, b'i', 0]).unwrap();
    let overview = env.service.overview("loa").await.unwrap();
    assert!(overview.ideas.is_err_and(|error| error.contains("loa.ideas.md")));
    assert_eq!(overview.sessions.len(), 1);
}

/// A project service over live sessions and file settings in a temp dir.
struct Env {
    _dir: tempfile::TempDir,
    bus: Arc<Bus>,
    store: Store,
    service: ProjectService,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("sessions"), JournalOptions::default()).unwrap();
    let bus = Arc::new(Bus::new());
    let sessions = LiveSessions::new(bus.clone(), store.clone());
    let settings = Arc::new(FileSettingsStore::new(dir.path().join("projects")));
    let (service, _) = ProjectService::start(sessions, settings, Arc::new(LocalOnly), Arc::new(Arc::new(Fake::default())), ServiceConfig::default());
    Env { _dir: dir, bus, store, service }
}

fn open(env: &Env, id: &str, project: &str, cwd: &str, at: i64) -> Arc<loa_bus::SessionWriter> {
    live(&env.bus, &env.store, id, Some(project), cwd, at)
}

fn in_folder(folder: &str) -> ProjectSettings {
    ProjectSettings { folder: Some(std::path::PathBuf::from(folder)), ..ProjectSettings::default() }
}

fn rows(projects: &[ProjectSummary]) -> Vec<(&str, usize, Option<&str>)> {
    projects.iter().map(|p| (p.name.as_str(), p.sessions, p.cwd.as_deref())).collect()
}

#[tokio::test]
async fn sessions_without_a_project_belong_to_no_project() {
    let env = env();
    let _a = open(&env, "a", "loa", "/home/u/loa", 10);
    let _b = open(&env, "b", "loa", "/home/u/scratch", 20);
    env.service.move_session("b", None).await.unwrap();
    assert_eq!(rows(&env.service.projects().await.unwrap()), [("loa", 1, Some("/home/u/loa"))]);
}

#[tokio::test]
async fn created_projects_list_with_their_folder() {
    let env = env();
    let _a = open(&env, "a", "loa", "/home/u/loa", 10);
    env.service.create("  docs  ", &in_folder("/home/u/docs")).await.unwrap();
    env.service.create("empty", &ProjectSettings::default()).await.unwrap();
    // A project only sessions named until now becomes a stored one.
    env.service.create("loa", &in_folder("/srv/loa")).await.unwrap();
    let projects = env.service.projects().await.unwrap();
    assert_eq!(rows(&projects), [("loa", 1, Some("/srv/loa")), ("docs", 0, Some("/home/u/docs")), ("empty", 0, None)]);
}

#[tokio::test]
async fn create_rejects_a_blank_or_taken_name() {
    let env = env();
    assert!(matches!(env.service.create("  ", &ProjectSettings::default()).await, Err(Error::EmptyName)));
    assert!(env.service.projects().await.unwrap().is_empty());
    env.service.create("docs", &in_folder("/home/u/docs")).await.unwrap();
    let again = env.service.create(" docs ", &ProjectSettings::default()).await;
    assert!(matches!(again, Err(Error::ProjectExists(name)) if name == "docs"));
    assert_eq!(env.service.overview("docs").await.unwrap().cwd.as_deref(), Some("/home/u/docs"), "left as it was");
}

#[tokio::test]
async fn move_session_sets_and_clears_the_project() {
    let env = env();
    let _a = open(&env, "a", "loa", "/home/u/loa", 10);
    env.service.move_session("a", Some("web")).await.unwrap();
    let moved = env.service.projects().await.unwrap();
    let names: Vec<(&str, usize)> = moved.iter().map(|p| (p.name.as_str(), p.sessions)).collect();
    assert_eq!(names, [("web", 1)]);
    env.service.move_session("a", None).await.unwrap();
    assert!(env.service.projects().await.unwrap().is_empty());
    assert!(matches!(env.service.move_session("nope", Some("web")).await, Err(Error::SessionNotFound(_))));
}

#[tokio::test]
async fn a_project_in_the_temp_folder_gets_its_own_folder_inside_it() {
    let env = env();
    let temp = std::env::temp_dir();
    let folder = env.service.create("scratch", &in_folder(temp.to_str().unwrap())).await.unwrap().unwrap();
    assert_eq!(folder.parent(), Some(temp.as_path()));
    assert!(folder.is_dir(), "the isolated folder is made");
    assert_eq!(env.service.overview("scratch").await.unwrap().settings.folder.as_ref(), Some(&folder), "and stored");

    // Choosing the temp folder again isolates again; any other folder is kept as is.
    let again = env.service.set_folder("scratch", temp.clone()).await.unwrap();
    assert_ne!(again, folder);
    assert_eq!(again.parent(), Some(temp.as_path()));
    let elsewhere = tempfile::tempdir().unwrap();
    let kept = env.service.set_folder("scratch", elsewhere.path().to_path_buf()).await.unwrap();
    assert_eq!(kept, elsewhere.path());
    for dir in [folder, again] {
        std::fs::remove_dir(dir).unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_project_folder_links_to_its_sessions_directories_and_moves() {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    let env = env();
    let root = tempfile::tempdir().unwrap();
    let dir = |name: &str| {
        let dir = root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    let (home, api, web, moved) = (dir("home"), dir("api"), dir("web"), dir("moved"));
    let linked = |folder: &Path| -> BTreeSet<PathBuf> {
        let Ok(entries) = std::fs::read_dir(folder.join(links::LINKS_DIR)) else { return BTreeSet::new() };
        entries.filter_map(|entry| std::fs::read_link(entry.unwrap().path()).ok()).collect()
    };
    env.service.create("launch", &ProjectSettings { folder: Some(home.clone()), ..ProjectSettings::default() }).await.unwrap();
    let _a = open(&env, "a", "launch", api.to_str().unwrap(), 10);
    let _b = open(&env, "b", "launch", web.to_str().unwrap(), 20);
    let _c = open(&env, "c", "launch", home.to_str().unwrap(), 30);

    env.service.projects().await.unwrap();
    assert_eq!(linked(&home), BTreeSet::from([api.clone(), web.clone()]));

    env.service.move_session("b", None).await.unwrap();
    env.service.projects().await.unwrap();
    assert_eq!(linked(&home), BTreeSet::from([api.clone()]), "a session leaving takes its link");

    env.service.set_folder("launch", moved.clone()).await.unwrap();
    assert!(!home.join(links::LINKS_DIR).exists(), "the old folder's links are removed");
    env.service.projects().await.unwrap();
    assert_eq!(linked(&moved), BTreeSet::from([api, home]), "the old folder is now a session directory like any");

    // Deleting keeps the sessions, in no project, and takes the links.
    assert_eq!(env.service.delete("launch").await.unwrap(), 2);
    assert!(!moved.join(links::LINKS_DIR).exists());
    assert!(env.service.projects().await.unwrap().is_empty(), "neither stored nor named by a session");
    assert_eq!(env.service.overview("launch").await.unwrap().sessions.len(), 0);
    let sessions = LiveSessions::new(env.bus.clone(), env.store.clone());
    assert!(SessionSource::load(&sessions, "a").await.unwrap().is_some(), "sessions are kept");
}

#[cfg(unix)]
mod link_folders {
    use crate::links::*;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder.join(LINKS_DIR)).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn links_follow_the_session_directories() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let (api, other_api, web) = (root.path().join("a/api"), root.path().join("b/api"), root.path().join("web"));
        for dir in [&project, &api, &other_api, &web] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let missing = root.path().join("gone");

        let dirs = BTreeSet::from([project.clone(), api.clone(), other_api.clone(), missing]);
        sync(&project, &dirs).unwrap();
        assert_eq!(names(&project), [".gitignore", "api", "api-2"], "no self link, no missing dir, clashes numbered");
        assert_eq!(std::fs::read_to_string(project.join(LINKS_DIR).join(".gitignore")).unwrap(), "*\n");
        let api_link = std::fs::read_link(project.join(LINKS_DIR).join("api")).unwrap();
        assert!(api_link == api || api_link == other_api);

        sync(&project, &BTreeSet::from([api.clone(), web.clone()])).unwrap();
        assert_eq!(names(&project).len(), 3, "{:?}", names(&project));
        assert!(names(&project).contains(&"web".to_owned()));
        let linked: BTreeSet<PathBuf> =
            std::fs::read_dir(project.join(LINKS_DIR)).unwrap().filter_map(|entry| std::fs::read_link(entry.unwrap().path()).ok()).collect();
        assert_eq!(linked, BTreeSet::from([api, web]), "the dropped directory's link is gone");

        sync(&project, &BTreeSet::new()).unwrap();
        assert!(!project.join(LINKS_DIR).exists(), "no sessions left: no folder");
    }

    #[test]
    fn remove_keeps_the_users_own_files() {
        let root = tempfile::tempdir().unwrap();
        let (project, api) = (root.path().join("project"), root.path().join("api"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&api).unwrap();
        sync(&project, &BTreeSet::from([api.clone()])).unwrap();
        std::fs::write(project.join(LINKS_DIR).join("notes.md"), "mine").unwrap();
        remove(&project).unwrap();
        assert_eq!(names(&project), ["notes.md"]);
        assert!(api.is_dir(), "the linked directory itself is untouched");
    }
}
