//! A project's settings and overview as [`loa_view::settings::projects`] views, and edited
//! settings back as domain values.

use loa_settings::projects::{DigestState, PinnedFile, ProjectOverview, ProjectSettings, SessionRow};
use loa_view::settings::projects as view;
use std::path::PathBuf;

pub fn settings_to_view(s: &ProjectSettings) -> view::ProjectSettings {
    view::ProjectSettings {
        instructions: s.instructions.clone(),
        pinned: s.pinned.iter().map(|p| view::PinnedFile { path: p.path.clone(), note: p.note.clone() }).collect(),
        digest_model: s.digest_model.clone(),
        folder: s.folder.as_ref().map(|folder| folder.display().to_string()),
    }
}

pub fn settings_from_view(s: view::ProjectSettings) -> ProjectSettings {
    let pinned = s.pinned.into_iter().map(|p| PinnedFile { path: p.path, note: p.note }).collect();
    ProjectSettings { instructions: s.instructions, pinned, digest_model: s.digest_model, folder: s.folder.map(PathBuf::from) }
}

fn session_to_view(s: &SessionRow) -> view::SessionRow {
    view::SessionRow {
        id: s.id.clone(),
        title: s.title.clone(),
        harness: s.harness.clone(),
        last_active: s.last_active,
        turns: s.turns,
        excluded: s.excluded,
        digest_state: match s.digest_state {
            DigestState::Missing => view::DigestState::Missing,
            DigestState::Fresh => view::DigestState::Fresh,
            DigestState::Stale => view::DigestState::Stale,
        },
        digest: s.digest.as_ref().map(|d| view::Digest { summary: d.summary.clone(), decisions: d.decisions.clone() }),
        digest_model: s.digest_model.clone(),
    }
}

pub fn overview_to_view(o: &ProjectOverview) -> view::ProjectOverview {
    view::ProjectOverview { cwd: o.cwd.clone(), settings: settings_to_view(&o.settings), sessions: o.sessions.iter().map(session_to_view).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_settings_round_trip() {
        let settings = ProjectSettings {
            instructions: "Use rustls.\nNo unsafe.".into(),
            pinned: vec![PinnedFile { path: "docs/ARCH.md".into(), note: Some("the contract".into()) }, PinnedFile { path: "src/main.rs".into(), note: None }],
            digest_model: Some("anthropic/claude-haiku".into()),
            folder: Some("/work/my-proj".into()),
        };
        assert_eq!(settings_from_view(settings_to_view(&settings)), settings);
        assert_eq!(settings_from_view(settings_to_view(&ProjectSettings::default())), ProjectSettings::default());
    }
}
