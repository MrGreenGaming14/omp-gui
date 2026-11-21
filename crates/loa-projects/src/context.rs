//! Project context: shared instructions, pinned files and ranked digests, as markdown.

use crate::Result;
use crate::rank::{Candidate, RankParams, Ranked, estimate_tokens, rank_and_pack};
use crate::settings::{ProjectSettings, SettingsStore};
use crate::source::{Access, SessionSource, visible_sessions};
use std::fmt::Write as _;

#[derive(Clone, Debug)]
pub struct ContextRequest<'a> {
    pub project: &'a str,
    /// The chat itself, left out of its own context.
    pub session: Option<&'a str>,
    /// The chat's first prompt or title.
    pub query: &'a str,
    /// In estimated tokens. Instructions and pins always go in; digests get what is left.
    pub budget_tokens: usize,
    /// Unix milliseconds, for recency.
    pub now: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PackedContext {
    pub project: String,
    pub settings: ProjectSettings,
    /// Best first.
    pub digests: Vec<Ranked>,
}

/// Ranks the visible sessions' digests and packs them with the settings into the budget.
pub async fn build_context(source: &dyn SessionSource, access: &dyn Access, settings: &dyn SettingsStore, request: &ContextRequest<'_>) -> Result<PackedContext> {
    let settings = settings.load(request.project).await?;
    let mut candidates = Vec::new();
    for info in visible_sessions(source, access, request.project).await? {
        if request.session != Some(info.id.as_str())
            && let Some(dom) = source.load(&info.id).await?
        {
            candidates.extend(Candidate::from_dom(&dom));
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
    let mut out = render_header(&context.project, &context.settings, !context.digests.is_empty());
    context.digests.iter().for_each(|d| out.push_str(&d.section));
    out
}
