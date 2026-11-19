//! Project context: shared instructions, pinned files and ranked digests,
//! rendered as markdown and delivered the way the harness takes it.

use crate::rank::{Candidate, RankParams, Ranked, estimate_tokens, rank_and_pack};
use crate::settings::{ProjectSettings, SettingsStore};
use crate::source::{Access, Requester, SessionSource, visible_sessions};
use crate::Result;
use loa_adapter::ContextDelivery;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// File name of the context file written for harnesses that read one.
pub const CONTEXT_FILE_NAME: &str = "loa-project-context.md";

/// MIME type of the rendered context.
pub const CONTEXT_MIME: &str = "text/markdown";

/// What to build context for.
#[derive(Clone, Debug)]
pub struct ContextRequest<'a> {
    pub project: &'a str,
    /// Who the chat runs for. Only sessions it may open contribute digests.
    pub requester: &'a Requester,
    /// The chat itself, left out of its own context.
    pub session: Option<&'a str>,
    /// The chat's first prompt or title.
    pub query: &'a str,
    /// Total budget for the rendered context, in estimated tokens. Shared
    /// instructions and pinned files are always included; digests get what
    /// is left.
    pub budget_tokens: usize,
    /// Unix milliseconds, for recency.
    pub now: i64,
}

/// The context of one chat, ready to render.
#[derive(Clone, Debug, PartialEq)]
pub struct PackedContext {
    pub project: String,
    pub settings: ProjectSettings,
    /// Packed digests, best first.
    pub digests: Vec<Ranked>,
}

/// Gathers the visible sessions' digests, ranks them and packs them with the
/// project settings into the budget.
pub async fn build_context(
    source: &dyn SessionSource,
    access: &dyn Access,
    settings: &dyn SettingsStore,
    request: &ContextRequest<'_>,
) -> Result<PackedContext> {
    let settings = settings.load(request.project).await?;
    let mut candidates = Vec::new();
    for info in visible_sessions(source, access, request.requester, request.project).await? {
        if request.session == Some(info.id.as_str()) {
            continue;
        }
        if let Some(dom) = source.load(&info.id).await?
            && let Some(candidate) = Candidate::from_dom(&dom)
        {
            candidates.push(candidate);
        }
    }
    let header = estimate_tokens(&render_header(request.project, &settings, true));
    let params = RankParams {
        exclude_session: request.session.map(str::to_owned),
        ..RankParams::new(request.now, request.budget_tokens.saturating_sub(header))
    };
    let digests = rank_and_pack(candidates, request.query, &params);
    Ok(PackedContext { project: request.project.to_owned(), settings, digests })
}

fn render_header(project: &str, settings: &ProjectSettings, with_digests: bool) -> String {
    let mut out = format!("# Project context: {project}\n");
    let instructions = settings.instructions.trim();
    if !instructions.is_empty() {
        let _ = write!(out, "\n## Shared instructions\n\n{instructions}\n");
    }
    if !settings.pinned.is_empty() {
        out.push_str("\n## Pinned files\n\n");
        for file in &settings.pinned {
            match file.note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                Some(note) => {
                    let _ = writeln!(out, "- `{}`: {note}", file.path);
                }
                None => {
                    let _ = writeln!(out, "- `{}`", file.path);
                }
            }
        }
    }
    if with_digests {
        out.push_str(
            "\n## Other sessions in this project\n\nDigests of other sessions. Call `project_search` to search \
             them, and `project_read` with a session id (and a node id from the turns listed) to read the full record.\n",
        );
    }
    out
}

/// The markdown section for one digest.
pub fn render_digest_section(candidate: &Candidate) -> String {
    let d = &candidate.digest.content;
    let mut out = String::new();
    let title = candidate.title.as_deref().unwrap_or("Untitled session");
    let _ = writeln!(out, "\n### {title} (session `{}`)\n", candidate.session);
    if candidate.digest.stale {
        out.push_str("_This digest may be out of date._\n\n");
    }
    let _ = writeln!(out, "{}", d.summary.trim());
    let list = |out: &mut String, heading: &str, items: &mut dyn Iterator<Item = String>| {
        let mut first = true;
        for item in items {
            if first {
                let _ = write!(out, "\n{heading}:\n");
                first = false;
            }
            let _ = writeln!(out, "- {item}");
        }
    };
    list(&mut out, "Decisions", &mut d.decisions.iter().cloned());
    list(
        &mut out,
        "Files",
        &mut d.files.iter().map(|f| if f.note.is_empty() { format!("`{}`", f.path) } else { format!("`{}`: {}", f.path, f.note) }),
    );
    list(&mut out, "Open items", &mut d.open_items.iter().cloned());
    if !d.pointers.is_empty() {
        let ids: Vec<String> = d.pointers.iter().map(ToString::to_string).collect();
        let _ = write!(out, "\nTurns: {}\n", ids.join(", "));
    }
    out
}

/// The context as markdown.
pub fn render_markdown(context: &PackedContext) -> String {
    let mut out = render_header(&context.project, &context.settings, !context.digests.is_empty());
    for digest in &context.digests {
        out.push_str(&digest.section);
    }
    out
}

/// An MCP resource carrying the context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    pub mime_type: &'static str,
    pub text: String,
}

/// How the context reached the harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivered {
    /// Text for `SessionConfig::context`, added to the system prompt.
    SystemPrompt(String),
    /// The file written; the harness reads it as a context file.
    ContextFile(PathBuf),
    /// A resource for the app's MCP server to serve.
    McpResource(McpResource),
}

/// Delivers `markdown` the first way the harness declares (`ways` is best
/// first, as in `Capabilities::context_delivery`). A context file is written
/// atomically to `context_dir`/[`CONTEXT_FILE_NAME`]. `None` when the
/// harness declares no way.
pub fn deliver(
    markdown: String,
    ways: &[ContextDelivery],
    project: &str,
    context_dir: &Path,
) -> Result<Option<Delivered>> {
    let Some(way) = ways.first() else { return Ok(None) };
    Ok(Some(match way {
        ContextDelivery::SystemPrompt => Delivered::SystemPrompt(markdown),
        ContextDelivery::ContextFile => {
            std::fs::create_dir_all(context_dir)?;
            let path = context_dir.join(CONTEXT_FILE_NAME);
            let mut file = tempfile::NamedTempFile::new_in(context_dir)?;
            std::io::Write::write_all(&mut file, markdown.as_bytes())?;
            file.persist(&path).map_err(|e| e.error)?;
            Delivered::ContextFile(path)
        }
        ContextDelivery::McpResource => Delivered::McpResource(McpResource {
            uri: resource_uri(project),
            name: format!("Project context: {project}"),
            mime_type: CONTEXT_MIME,
            text: markdown,
        }),
    }))
}

/// `loa://projects/<project>/context`, the project id percent encoded.
pub fn resource_uri(project: &str) -> String {
    let mut uri = String::from("loa://projects/");
    for byte in project.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            uri.push(byte as char);
        } else {
            let _ = write!(uri, "%{byte:02X}");
        }
    }
    uri.push_str("/context");
    uri
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::{DigestContent, FileNote, write_drafts};
    use crate::settings::{MemorySettingsStore, PinnedFile, exclude_draft};
    use crate::source::testing::{Builder, MemorySource, TestAccess, peer};
    use loa_core::{Draft, attr};

    const DAY: i64 = 24 * 3600 * 1000;

    fn digested(id: &str, created: i64, summary: &str) -> Builder {
        let b = Builder::new(id, "proj", created);
        b.commit(Draft::set(b.root, attr::TITLE, format!("Title {id}")));
        b.turn("work", "done", created);
        let content = DigestContent {
            summary: summary.into(),
            decisions: vec!["keep 31415".into()],
            files: vec![FileNote { path: "src/net.rs".into(), note: "listener".into() }],
            open_items: vec!["tcp fallback".into()],
            pointers: vec![b.dom().root().unwrap().children[0]],
        };
        for d in write_drafts(&b.dom(), &content, b.dom().seq(), None, created) {
            b.commit(d);
        }
        b
    }

    async fn settings() -> MemorySettingsStore {
        let store = MemorySettingsStore::new();
        let settings = ProjectSettings {
            instructions: "Use rustls, never openssl.".into(),
            pinned: vec![PinnedFile { path: "docs/ARCHITECTURE.md".into(), note: Some("the code contract".into()) }],
            digest_model: None,
        };
        store.save("proj", &settings).await.unwrap();
        store
    }

    #[tokio::test]
    async fn context_has_instructions_pins_and_digests_with_session_ids() {
        let a = digested("a", DAY, "Set up the iroh endpoint on port 31415.");
        let me = digested("me", DAY, "The chat itself.");
        let source = MemorySource::with([a.dom(), me.dom()]);
        let request = ContextRequest {
            project: "proj",
            requester: &Requester::Local,
            session: Some("me"),
            query: "iroh port",
            budget_tokens: 4_000,
            now: 2 * DAY,
        };
        let packed = build_context(&source, &TestAccess::default(), &settings().await, &request).await.unwrap();
        let md = render_markdown(&packed);
        assert!(md.contains("Use rustls, never openssl."));
        assert!(md.contains("- `docs/ARCHITECTURE.md`: the code contract"));
        assert!(md.contains("### Title a (session `a`)"));
        assert!(md.contains("Set up the iroh endpoint"));
        assert!(md.contains("- `src/net.rs`: listener"));
        assert!(md.contains("project_read"));
        assert!(!md.contains("The chat itself."));
        assert!(estimate_tokens(&md) <= 4_000);
    }

    #[tokio::test]
    async fn excluded_session_never_reaches_context() {
        let a = digested("a", DAY, "Visible work.");
        let x = digested("x", DAY, "Secret excluded work.");
        x.commit(exclude_draft(&x.dom(), true).unwrap());
        let source = MemorySource::with([a.dom(), x.dom()]);
        let request = ContextRequest {
            project: "proj",
            requester: &Requester::Local,
            session: None,
            query: "work",
            budget_tokens: 4_000,
            now: 2 * DAY,
        };
        let md = render_markdown(&build_context(&source, &TestAccess::default(), &settings().await, &request).await.unwrap());
        assert!(md.contains("Visible work."));
        assert!(!md.contains("Secret excluded work."));
    }

    #[tokio::test]
    async fn future_sessions_grant_never_yields_older_digests() {
        let old = digested("old", DAY, "Old work on the terminal.");
        let new = digested("new", 5 * DAY, "New work on the terminal.");
        let source = MemorySource::with([old.dom(), new.dom()]);
        let peer = peer();
        let request = ContextRequest {
            project: "proj",
            requester: &peer,
            session: None,
            query: "old terminal work",
            budget_tokens: 4_000,
            now: 6 * DAY,
        };
        let access = TestAccess::future_since(3 * DAY);
        let packed = build_context(&source, &access, &settings().await, &request).await.unwrap();
        let sessions: Vec<&str> = packed.digests.iter().map(|d| d.candidate.session.as_str()).collect();
        assert_eq!(sessions, ["new"]);
        assert!(!render_markdown(&packed).contains("Old work"));
    }

    #[tokio::test]
    async fn denied_session_does_not_change_ranking_of_visible_ones() {
        let a = digested("a", DAY, "Terminal scrolling fix.");
        let b = digested("b", DAY, "Terminal colors and fonts.");
        // The denied session would change BM25 document frequencies if it were scored.
        let d = digested("d", DAY, "Terminal terminal terminal scrolling scrolling.");
        let request = |peer| ContextRequest {
            project: "proj",
            requester: peer,
            session: None,
            query: "terminal scrolling",
            budget_tokens: 4_000,
            now: 2 * DAY,
        };
        let peer = peer();
        let access = TestAccess::only(&["a", "b"]);
        let store = settings().await;
        let with = MemorySource::with([a.dom(), b.dom(), d.dom()]);
        let without = MemorySource::with([a.dom(), b.dom()]);
        let seen = build_context(&with, &access, &store, &request(&peer)).await.unwrap();
        let baseline = build_context(&without, &access, &store, &request(&peer)).await.unwrap();
        assert_eq!(seen, baseline);
        assert_eq!(render_markdown(&seen), render_markdown(&baseline));
    }

    #[test]
    fn delivers_each_way_the_harness_declares() {
        let dir = tempfile::tempdir().unwrap();
        let md = "# Project context: p\n".to_owned();
        assert_eq!(
            deliver(md.clone(), &[ContextDelivery::SystemPrompt, ContextDelivery::ContextFile], "p", dir.path()).unwrap(),
            Some(Delivered::SystemPrompt(md.clone()))
        );
        let target = dir.path().join("ctx");
        let Some(Delivered::ContextFile(path)) = deliver(md.clone(), &[ContextDelivery::ContextFile], "p", &target).unwrap()
        else {
            panic!("expected a context file");
        };
        assert_eq!(path, target.join(CONTEXT_FILE_NAME));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), md);
        // A second delivery replaces the file.
        deliver("new".into(), &[ContextDelivery::ContextFile], "p", &target).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let Some(Delivered::McpResource(resource)) = deliver(md.clone(), &[ContextDelivery::McpResource], "my proj", dir.path()).unwrap()
        else {
            panic!("expected a resource");
        };
        assert_eq!(resource.uri, "loa://projects/my%20proj/context");
        assert_eq!(resource.text, md);
        assert_eq!(deliver(md, &[], "p", dir.path()).unwrap(), None);
    }
}
