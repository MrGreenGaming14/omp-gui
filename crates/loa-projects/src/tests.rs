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
    drop(live(&bus, &store, "closed", "/home/u/loa"));
    drop(bus.close("closed"));
    let open = live(&bus, &store, "open", "/home/u/loa");
    let sessions = LiveSessions::new(bus, store.clone());
    let mut listed: Vec<String> = sessions.list("loa").await.unwrap().into_iter().map(|i| i.id).collect();
    listed.sort();
    assert_eq!(listed, ["closed", "open"]);
    assert!(sessions.load("../etc").await.unwrap().is_none());
    let dom = sessions.load("closed").await.unwrap().unwrap();
    SessionWriter::commit(&sessions, "closed", exclude_draft(&dom, true).unwrap()).unwrap();
    assert!(is_excluded(&store.open("closed").unwrap().0.load().unwrap()));
    let dom = sessions.load("open").await.unwrap().unwrap();
    sessions.commit("open", exclude_draft(&dom, true).unwrap()).await.unwrap();
    assert!(open.with_dom(is_excluded));
}
