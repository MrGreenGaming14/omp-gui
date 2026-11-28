//! Writes the `projects/*` golden fixtures from the Rust `loa-projects` crate.
//! Usage: `cargo run -- <out dir>` (normally `cpp/tests/golden/projects`).

use async_trait::async_trait;
use loa_contracts::harness::HostTools;
use loa_core::{Dom, Draft, ElementKind as K, MemorySink, NodeId, PatchSink, Value, attr};
use loa_projects::context::{ContextRequest, build_context, render_markdown};
use loa_projects::digest::write_drafts;
use loa_projects::digester::{PromptLimits, build_prompt};
use loa_projects::settings::SettingsStore;
use loa_projects::source::{SessionInfo, SessionSource};
use loa_projects::{FileSettingsStore, LocalOnly};
use loa_settings::projects::{DigestContent, FileNote, PinnedFile, ProjectSettings};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct Builder {
    sink: Arc<MemorySink>,
    root: NodeId,
}

impl Builder {
    fn new(id: &str, project: &str, created: i64) -> Self {
        let sink = Arc::new(MemorySink::new());
        let attrs = [(attr::ID, Value::from(id)), (attr::HARNESS, "omp".into()), (attr::NODE, "node-a".into()), (attr::CREATED, created.into()), (attr::PROJECT, project.into())];
        let root = sink.commit_at(Draft::insert(NodeId::DOCUMENT, K::Session, attrs, ""), created).unwrap().node;
        Self { sink, root }
    }
    fn commit(&self, draft: Draft) -> NodeId {
        self.sink.commit(draft).unwrap().node
    }
    fn turn(&self, user: &str, assistant: &str, ended: i64) -> NodeId {
        let turn = self.commit(Draft::insert(self.root, K::Turn, [(attr::STATUS, Value::from("done")), (attr::ENDED, ended.into())], ""));
        self.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("user"))], user));
        self.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("assistant"))], assistant));
        turn
    }
    fn call(&self, turn: NodeId, tool: &str, input: &str, result: &str) -> NodeId {
        let call = self.commit(Draft::insert(turn, K::Call, [(attr::TOOL, Value::from(tool))], ""));
        self.commit(Draft::insert(call, K::Input, [] as [(&str, Value); 0], input));
        self.commit(Draft::insert(call, K::Result, [] as [(&str, Value); 0], result));
        call
    }
    fn write_digest(&self, content: &DigestContent, model: Option<&str>) {
        let dom = self.dom();
        write_drafts(&dom, content, dom.seq(), model, 0).into_iter().for_each(|d| {
            self.commit(d);
        });
    }
    fn digest(&self, summary: &str) {
        self.write_digest(&DigestContent { summary: summary.into(), ..Default::default() }, None);
    }
    fn dom(&self) -> Dom {
        self.sink.dom()
    }
}

#[derive(Default)]
struct MemorySource(Mutex<BTreeMap<String, Arc<MemorySink>>>);

impl MemorySource {
    fn with<'a>(builders: impl IntoIterator<Item = &'a Builder>) -> Self {
        let source = Self::default();
        for b in builders {
            let id = SessionInfo::from_dom(&b.dom()).unwrap().id;
            source.0.lock().unwrap().insert(id, b.sink.clone());
        }
        source
    }
}

#[async_trait]
impl SessionSource for MemorySource {
    async fn list(&self, project: &str) -> loa_projects::Result<Vec<SessionInfo>> {
        let infos = self.0.lock().unwrap().values().filter_map(|s| SessionInfo::from_dom(&s.dom())).collect::<Vec<_>>();
        Ok(infos.into_iter().filter(|i| i.project == project).collect())
    }
    async fn load(&self, session: &str) -> loa_projects::Result<Option<Arc<Dom>>> {
        Ok(self.0.lock().unwrap().get(session).map(|s| Arc::new(s.dom())))
    }
}

/// The `tests.rs` fixture plus a session `d` with a full digest written twice.
fn fixture() -> (MemorySource, Builder) {
    let a = Builder::new("a", "loa", 100);
    let t = a.turn("Set up the Iroh endpoint", "Bound it to port 31415.", 110);
    a.call(t, "bash", "cargo test -p net", "ok: 12 passed");
    a.commit(Draft::insert(a.root, K::Todo, [(attr::STATE, Value::from("pending"))], "write docs"));
    let b = Builder::new("b", "loa", 200);
    b.turn("Theme the sidebar", "Dark tokens done.", 210);
    b.digest("Themed the sidebar with dark tokens.");
    let c = Builder::new("c", "web", 300);
    c.turn("iroh elsewhere", "no", 310);
    let d = Builder::new("d", "loa", 50);
    let t1 = d.turn("Write the relay docs", "Wrote docs/relay.md with a \"quoted\" note.", 60);
    d.turn("héllo wörld ✓", "multi\nline\n  reply", 70);
    d.digest("first");
    let full = DigestContent {
        summary: "Documented the relay.\nTwo lines.".into(),
        decisions: vec!["Use a self hosted relay".into(), "Keep port 31415".into()],
        files: vec![FileNote { path: "docs/relay.md".into(), note: "written".into() }, FileNote { path: "README.md".into(), note: String::new() }],
        open_items: vec!["link from the README".into()],
        pointers: vec![t1],
    };
    d.write_digest(&full, Some("smol"));
    (MemorySource::with([&a, &b, &c, &d]), a)
}

fn write(out: &Path, name: &str, bytes: impl AsRef<[u8]>) {
    std::fs::write(out.join(name), bytes).unwrap();
}

#[tokio::main]
async fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("usage: golden_gen <out dir>"));
    std::fs::create_dir_all(&out).unwrap();
    let scratch = std::env::temp_dir().join(format!("loa-projects-golden-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();

    // File names: one fresh store per name, so the single file is that name's.
    let mut names = String::new();
    for name in ["loa", "my/proj", "with space", "a.b", ".", "..", "héllo", "dash-under_score", "50%", "Ünïcødé ✓", "tab\tand~tilde"] {
        let dir = scratch.join("names");
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileSettingsStore::new(&dir);
        store.save(name, &ProjectSettings::default()).await.unwrap();
        let file = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().file_name();
        names += &format!("{name}\t{}\n", file.to_str().unwrap());
    }
    write(&out, "filenames.tsv", names);

    // Settings and ideas files.
    let dir = scratch.join("settings");
    let store = FileSettingsStore::new(&dir);
    let full = ProjectSettings {
        instructions: "Use rustls.\nNo \"unsafe\".".into(),
        pinned: vec![PinnedFile { path: "src/lib.rs".into(), note: Some("entry point".into()) }, PinnedFile { path: "Cargo.toml".into(), note: None }],
        digest_model: Some("smol".into()),
        folder: Some("/work/loa ✓".into()),
        ideas_in_context: true,
    };
    store.save("full", &full).await.unwrap();
    store.save("default", &ProjectSettings::default()).await.unwrap();
    write(&out, "settings.full.json", std::fs::read(dir.join("full.json")).unwrap());
    write(&out, "settings.default.json", std::fs::read(dir.join("default.json")).unwrap());
    let seen = store.load_ideas("full").await.unwrap().file;
    store.save_ideas("full", seen, "# Ideas\r\n\r\n- try iroh relays ✓\r\n- no trailing newline").await.unwrap();
    write(&out, "ideas.md", std::fs::read(dir.join("full.ideas.md")).unwrap());

    // Session fixture: project_read and project_search outputs, the digest prompt and the context.
    let (source, a) = fixture();
    let source = Arc::new(source);
    let tools = loa_projects::tools::tool_set(source.clone(), Arc::new(LocalOnly));
    for spec in tools.specs() {
        write(&out, &format!("{}.schema.json", spec.name), &spec.input_schema);
    }
    let calls: &[(&str, &str, &str)] = &[
        ("read-a.txt", "project_read", r#"{"session":"a"}"#),
        ("read-a-turn-64.txt", "project_read", r#"{"session":"a","node":2,"max_bytes":64}"#),
        ("read-a-turn-64-next.txt", "project_read", r#"{"session":"a","node":2,"max_bytes":64,"offset":63}"#),
        ("read-b.txt", "project_read", r#"{"session":"b"}"#),
        ("read-d.txt", "project_read", r#"{"session":"d"}"#),
        ("search-iroh.json", "project_search", r#"{"query":"iroh"}"#),
        ("search-all-words.json", "project_search", r#"{"query":"31415 bound","mode":"all_words"}"#),
        ("search-call.json", "project_search", r#"{"query":"12 passed","element":"call"}"#),
        ("search-attr.json", "project_search", r#"{"attributes":{"tool":"BA","role":null}}"#),
        ("search-tool-attr.json", "project_search", r#"{"attributes":{"tool":"BA"}}"#),
        ("search-messages-page.json", "project_search", r#"{"element":"message","limit":3}"#),
        ("search-messages-page2.json", "project_search", r#"{"element":"message","limit":3,"cursor":"3"}"#),
        ("search-digest.json", "project_search", r#"{"query":"relay","element":"digest","sessions":["d","b"]}"#),
        ("search-unicode.json", "project_search", r#"{"query":"WÖRLD"}"#),
        ("search-own.json", "project_search", r#"{"query":"dark tokens","sessions":["b"]}"#),
    ];
    for (file, tool, args) in calls {
        let text = HostTools::call(&tools, "b", tool, args).await.unwrap();
        write(&out, file, text);
    }
    let prompt = build_prompt(&a.dom(), &PromptLimits::default()).unwrap();
    write(&out, "prompt-a.txt", &prompt.text);

    let dir = scratch.join("context");
    let store = FileSettingsStore::new(&dir);
    store.save("loa", &ProjectSettings { instructions: "Use rustls.".into(), pinned: full.pinned.clone(), ideas_in_context: true, ..ProjectSettings::default() }).await.unwrap();
    let seen = store.load_ideas("loa").await.unwrap().file;
    store.save_ideas("loa", seen, "- try iroh relays\n").await.unwrap();
    let request = ContextRequest { project: "loa", session: Some("a"), query: "relay docs", budget_tokens: 4_000, now: 1_000 };
    let packed = build_context(&*source, &LocalOnly, &store, &request).await.unwrap();
    write(&out, "context.md", render_markdown(&packed));

    // `.omgui` links: targets relative to the scratch root.
    let root = scratch.join("links");
    let project = root.join("project");
    let dirs: Vec<PathBuf> = ["project", "a/api", "b/api", "web", "root/x y"].iter().map(|d| root.join(d)).collect();
    for dir in &dirs {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut wanted: BTreeSet<PathBuf> = dirs.into_iter().collect();
    wanted.insert(root.join("gone"));
    loa_projects::links::sync(&project, &wanted).unwrap();
    let links = project.join(loa_projects::links::LINKS_DIR);
    let mut lines: Vec<String> = std::fs::read_dir(&links)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            match std::fs::read_link(entry.path()) {
                Ok(target) => format!("{name} -> {}", target.strip_prefix(&root).unwrap().display()),
                Err(_) => format!("{name} = {:?}", std::fs::read_to_string(entry.path()).unwrap()),
            }
        })
        .collect();
    lines.sort();
    write(&out, "links.txt", lines.join("\n") + "\n");

    std::fs::remove_dir_all(&scratch).unwrap();
}
