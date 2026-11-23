//! Project context: shared instructions, the ideas notes when shared, pinned files and ranked digests, as markdown.

use crate::Result;
use crate::rank::{Candidate, RankParams, Ranked, estimate_tokens, rank_and_pack};
use crate::settings::SettingsStore;
use loa_settings::projects::ProjectSettings;
use crate::source::{Access, SessionSource, visible_sessions};
use std::fmt::Write as _;

#[derive(Clone, Debug)]
pub struct ContextRequest<'a> {
    pub project: &'a str,
    /// The chat itself, left out of its own context.
    pub session: Option<&'a str>,
    /// The chat's first prompt or title.
    pub query: &'a str,
    /// In estimated tokens. Instructions, shared ideas notes and pins always go in; digests get what is left.
    pub budget_tokens: usize,
    /// Unix milliseconds, for recency.
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackedContext {
    pub project: String,
    pub settings: ProjectSettings,
    /// The ideas notes, when the project shares them with chats and they aren't blank.
    pub ideas: Option<String>,
    /// Best first.
    pub digests: Vec<Ranked>,
}

/// Ranks the visible sessions' digests and packs them with the settings into the budget. Shared ideas notes that
/// can't be read are left out; the rest of the context still goes in.
pub async fn build_context(source: &dyn SessionSource, access: &dyn Access, store: &dyn SettingsStore, request: &ContextRequest<'_>) -> Result<PackedContext> {
    let settings = store.load(request.project).await?;
    let ideas = if settings.ideas_in_context {
        let ideas = store.load_ideas(request.project).await;
        let ideas = ideas.inspect_err(|error| tracing::warn!(project = request.project, %error, "leaving unreadable ideas notes out of the project context"));
        ideas.ok().map(|ideas| ideas.text).filter(|text| !text.trim().is_empty())
    } else {
        None
    };
    let mut candidates = Vec::new();
    for info in visible_sessions(source, access, request.project).await? {
        if request.session != Some(info.id.as_str())
            && let Some(dom) = source.load(&info.id).await?
        {
            candidates.extend(Candidate::from_dom(&dom));
        }
    }
    let header = estimate_tokens(&render_header(request.project, &settings, ideas.as_deref(), true));
    let digests = rank_and_pack(candidates, request.query, &RankParams::new(request.now, request.budget_tokens.saturating_sub(header)));
    Ok(PackedContext { project: request.project.to_owned(), settings, ideas, digests })
}

fn render_header(project: &str, settings: &ProjectSettings, ideas: Option<&str>, with_digests: bool) -> String {
    let mut out = format!("# Project context: {project}\n");
    let instructions = settings.instructions.trim();
    if !instructions.is_empty() {
        let _ = write!(out, "\n## Shared instructions\n\n{instructions}\n");
    }
    if let Some(ideas) = ideas {
        let _ = write!(out, "\n## Ideas and notes\n\nThe user's own development ideas and notes for this project.\n\n{}\n", ideas.trim());
    }
    if !settings.pinned.is_empty() {
        out.push_str("\n## Pinned files\n\n");
    }
    for file in &settings.pinned {
        let note = file.note.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(|n| format!(": {n}"));
        let _ = writeln!(out, "- `{}`{}", file.path, note.unwrap_or_default());
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
    let title = candidate.title.as_deref().unwrap_or("Untitled session");
    let mut out = format!("\n### {title} (session `{}`)\n\n", candidate.session);
    if candidate.digest.stale {
        out.push_str("_This digest may be out of date._\n\n");
    }
    let _ = writeln!(out, "{}", d.summary.trim());
    let files: Vec<String> = d.files.iter().map(|f| if f.note.is_empty() { format!("`{}`", f.path) } else { format!("`{}`: {}", f.path, f.note) }).collect();
    for (heading, items) in [("Decisions", &d.decisions), ("Files", &files), ("Open items", &d.open_items)] {
        if !items.is_empty() {
            let _ = write!(out, "\n{heading}:\n");
        }
        for item in items {
            let _ = writeln!(out, "- {item}");
        }
    }
    if !d.pointers.is_empty() {
        let ids: Vec<String> = d.pointers.iter().map(ToString::to_string).collect();
        let _ = write!(out, "\nTurns: {}\n", ids.join(", "));
    }
    out
}

pub fn render_markdown(context: &PackedContext) -> String {
    let mut out = render_header(&context.project, &context.settings, context.ideas.as_deref(), !context.digests.is_empty());
    context.digests.iter().for_each(|d| out.push_str(&d.section));
    out
}
