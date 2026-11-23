//! Project-scoped history discovery for omp's JSONL session files.

use serde_json::{Map, Value as Json};
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::{File, Metadata};
use std::io::{self, BufRead, BufReader, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Maximum prefix bytes inspected per file so large transcripts stay bounded.
const MAX_PREFIX_BYTES: u64 = 64 * 1024;

/// Metadata for one native omp session matching a project folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSession {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    /// File modification timestamp in Unix milliseconds.
    pub modified: i64,
}

/// Lists native omp sessions under `<agent_dir>/sessions/<project>/*.jsonl`
/// whose header `cwd` canonicalizes to `folder`, sorted newest first with
/// deterministic tie-breaking and deduplicated by native session `id`.
pub fn project_history(agent_dir: &Path, folder: &Path) -> io::Result<Vec<ProjectSession>> {
    let mut sessions = matching_sessions(agent_dir, folder)?;
    sessions.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut seen = HashSet::with_capacity(sessions.len());
    sessions.retain(|session| seen.insert(session.id.clone()));
    Ok(sessions)
}

/// Inspects a single omp session file at `path` (including copied paths) and
/// returns its metadata if its header is valid and its `cwd` canonicalizes to
/// `folder`.
pub fn project_session(path: &Path, folder: &Path) -> io::Result<Option<ProjectSession>> {
    let canonical_folder = canonicalize_path(folder)?;
    inspect_session(path, &canonical_folder)
}

/// Deletes the native omp session `id` that [`project_history`] lists for `folder`: for every session file with that
/// id, first its same-stem sidecar directory (artifacts and subagent sessions; a symlink there is left alone), then
/// the file. A sidecar that can't be removed (held open on Windows) stops the delete before its file goes, so the
/// session stays listed and can be deleted again. Errors name the path that failed; an `id` not listed for `folder`
/// is [`ErrorKind::NotFound`].
pub fn delete_project_session(agent_dir: &Path, folder: &Path, id: &str) -> io::Result<()> {
    let mut sessions = matching_sessions(agent_dir, folder)?;
    sessions.retain(|session| session.id == id);
    if sessions.is_empty() {
        return Err(io::Error::new(ErrorKind::NotFound, format!("no saved OMP session {id} for {}", folder.display())));
    }
    remove_sessions(&sessions, |dir| std::fs::remove_dir_all(dir))
}

/// Removes each session's sidecar directory with `remove_dir`, then its file; tests pass a `remove_dir` that fails.
fn remove_sessions(sessions: &[ProjectSession], remove_dir: impl Fn(&Path) -> io::Result<()>) -> io::Result<()> {
    fn context(path: &Path) -> impl FnOnce(io::Error) -> io::Error + '_ {
        move |error| io::Error::new(error.kind(), format!("can't remove {}: {error}", path.display()))
    }
    for session in sessions {
        let sidecar = session.path.with_extension("");
        if std::fs::symlink_metadata(&sidecar).is_ok_and(|meta| meta.is_dir()) {
            remove_dir(&sidecar).map_err(context(&sidecar))?;
        }
        std::fs::remove_file(&session.path).map_err(context(&session.path))?;
    }
    Ok(())
}

/// Every session under `<agent_dir>/sessions/<project>/*.jsonl` whose header `cwd` canonicalizes to `folder`.
fn matching_sessions(agent_dir: &Path, folder: &Path) -> io::Result<Vec<ProjectSession>> {
    let sessions_dir = agent_dir.join("sessions");
    let subdirs = match std::fs::read_dir(&sessions_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let canonical_folder = canonicalize_path(folder)?;
    let mut sessions = Vec::new();
    for subdir in subdirs {
        let Ok(subdir) = subdir else { continue };
        if !subdir.file_type().is_ok_and(|kind| kind.is_dir()) { continue; }
        let entries = match std::fs::read_dir(subdir.path()) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::debug!(path = %subdir.path().display(), %error, "skipping an unreadable OMP history directory");
                continue;
            }
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) { continue; }
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") { continue; }
            match inspect_session(&path, &canonical_folder) {
                Ok(Some(session)) => sessions.push(session),
                Ok(None) => {}
                Err(error) => tracing::debug!(path = %path.display(), %error, "skipping an unreadable OMP session"),
            }
        }
    }
    Ok(sessions)
}

fn inspect_session(path: &Path, canonical_folder: &Path) -> io::Result<Option<ProjectSession>> {
    let file = File::open(path)?;
    let Some((title_slot, header)) = read_header(&file)? else {
        return Ok(None);
    };
    let Some(id) = non_empty_str(&header, "id") else {
        return Ok(None);
    };
    let Some(cwd) = header.get("cwd").and_then(Json::as_str) else {
        return Ok(None);
    };
    let cwd_path = as_cwd_path(cwd);
    if !cwd_path.is_absolute() {
        return Ok(None);
    }
    let canonical_cwd = match canonicalize_path(&cwd_path) {
        Ok(path) => path,
        Err(err) if matches!(err.kind(), ErrorKind::NotFound | ErrorKind::InvalidInput) => return Ok(None),
        Err(err) => return Err(err),
    };
    if canonical_cwd != canonical_folder {
        return Ok(None);
    }
    let meta = file.metadata()?;
    let title = title_slot
        .as_ref()
        .and_then(|slot| non_empty_str(slot, "title"))
        .or_else(|| non_empty_str(&header, "title"))
        .map(str::to_owned)
        .unwrap_or_else(|| file_stem(path));
    Ok(Some(ProjectSession {
        id: id.to_owned(),
        path: path.to_path_buf(),
        title,
        modified: modified_unix_ms(&meta),
    }))
}

fn read_header(file: &File) -> io::Result<Option<(Option<Map<String, Json>>, Map<String, Json>)>> {
    let mut reader = BufReader::new(file.take(MAX_PREFIX_BYTES));
    let mut buf = Vec::new();
    let mut title_slot: Option<Map<String, Json>> = None;

    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            return Ok(None);
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            return Ok(None);
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(Json::Object(obj)) = serde_json::from_str::<Json>(trimmed) else {
            continue;
        };
        if title_slot.is_none() && str_field(&obj, "type") == Some("title") {
            title_slot = Some(obj);
            continue;
        }
        if str_field(&obj, "type") != Some("session") {
            return Ok(None);
        }
        return Ok(Some((title_slot, obj)));
    }
}

fn as_cwd_path(cwd: &str) -> Cow<'_, Path> {
    #[cfg(windows)]
    if (cwd.starts_with(r"\\?\") || cwd.starts_with("//?/")) && cwd.contains('/') {
        return Cow::Owned(PathBuf::from(cwd.replace('/', r"\")));
    }
    Cow::Borrowed(Path::new(cwd))
}

fn canonicalize_path(path: &Path) -> io::Result<PathBuf> {
    #[cfg(windows)]
    if let Some(raw) = path.to_str()
        && (raw.starts_with(r"\\?\") || raw.starts_with("//?/"))
        && raw.contains('/')
    {
        return loa_util::fs::canonicalize(raw.replace('/', r"\"));
    }
    loa_util::fs::canonicalize(path)
}

fn str_field<'a>(map: &'a Map<String, Json>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Json::as_str)
}

fn non_empty_str<'a>(map: &'a Map<String, Json>, key: &str) -> Option<&'a str> {
    str_field(map, key).filter(|value| !value.trim().is_empty())
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn modified_unix_ms(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use std::time::{Duration, SystemTime};

    fn write_session_file(path: &Path, lines: &[&str], mtime_secs: Option<u64>) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = File::create(path).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        if let Some(secs) = mtime_secs {
            file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
                .unwrap();
        }
    }

    #[test]
    fn title_precedence_prefix_header_and_stem_and_bounded_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let cwd = project.to_string_lossy().into_owned();

        let from_prefix = agent_dir.join("sessions/proj/a.jsonl");
        write_session_file(
            &from_prefix,
            &[
                &json!({"type": "title", "title": "Prefix Title"}).to_string(),
                &json!({"type": "session", "version": 3, "id": "s1", "cwd": cwd, "title": "Header Title"}).to_string(),
            ],
            None,
        );
        assert_eq!(project_session(&from_prefix, &project).unwrap().unwrap().title, "Prefix Title");

        let empty_prefix_uses_header = agent_dir.join("sessions/proj/b.jsonl");
        write_session_file(
            &empty_prefix_uses_header,
            &[
                &json!({"type": "title", "title": ""}).to_string(),
                &json!({"type": "session", "version": 3, "id": "s2", "cwd": cwd, "title": "Header Title"}).to_string(),
            ],
            None,
        );
        assert_eq!(
            project_session(&empty_prefix_uses_header, &project).unwrap().unwrap().title,
            "Header Title"
        );

        let header_only_uses_stem = agent_dir.join("sessions/proj/stem-name.jsonl");
        write_session_file(
            &header_only_uses_stem,
            &[&json!({"type": "session", "version": 3, "id": "s3", "cwd": cwd}).to_string()],
            None,
        );
        assert_eq!(
            project_session(&header_only_uses_stem, &project).unwrap().unwrap().title,
            "stem-name"
        );

        // Trailing huge/corrupt body past the prefix must not be read or break inspection.
        let large_tail = agent_dir.join("sessions/proj/large-tail.jsonl");
        {
            let mut file = File::create(&large_tail).unwrap();
            writeln!(file, "{}", json!({"type": "session", "version": 3, "id": "s4", "cwd": cwd, "title": "Bounded"})).unwrap();
            file.write_all(&vec![b'x'; 256 * 1024]).unwrap();
            file.write_all(&[0xff, 0xfe]).unwrap();
        }
        assert_eq!(project_session(&large_tail, &project).unwrap().unwrap().title, "Bounded");
    }

    #[test]
    fn rejects_malformed_and_unsupported_headers() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let cwd = project.to_string_lossy().into_owned();

        let cases: &[(&str, &[&str])] = &[
            ("empty.jsonl", &[]),
            ("not-json.jsonl", &["not json at all"]),
            ("array-only.jsonl", &["[1, 2, 3]"]),
            ("title-only.jsonl", &[r#"{"type":"title","title":"Orphan"}"#]),
            (
                "title-then-message.jsonl",
                &[r#"{"type":"title","title":"T"}"#, r#"{"type":"message","id":"m1"}"#],
            ),
            (
                "message-first.jsonl",
                &[
                    r#"{"type":"message","id":"m1"}"#,
                    &json!({"type": "session", "id": "s", "cwd": cwd}).to_string(),
                ],
            ),
            (
                "double-title.jsonl",
                &[
                    r#"{"type":"title","title":"A"}"#,
                    r#"{"type":"title","title":"B"}"#,
                    &json!({"type": "session", "id": "s", "cwd": cwd}).to_string(),
                ],
            ),
            ("missing-id.jsonl", &[&json!({"type": "session", "cwd": cwd}).to_string()]),
            ("empty-id.jsonl", &[&json!({"type": "session", "id": "  ", "cwd": cwd}).to_string()]),
            ("missing-cwd.jsonl", &[r#"{"type":"session","id":"s1"}"#]),
            ("relative-cwd.jsonl", &[r#"{"type":"session","id":"s2","cwd":"relative/project"}"#]),
        ];

        for (name, lines) in cases {
            let path = dir.path().join(name);
            write_session_file(&path, lines, None);
            assert_eq!(project_session(&path, &project).unwrap(), None, "{name}");
        }

        let invalid_utf8 = dir.path().join("invalid-utf8.jsonl");
        std::fs::write(&invalid_utf8, [0xff, 0xfe, b'\n']).unwrap();
        assert_eq!(project_session(&invalid_utf8, &project).unwrap(), None);

        let missing_cwd_dir = dir.path().join("deleted-cwd.jsonl");
        let missing_cwd = dir.path().join("does-not-exist").to_string_lossy().into_owned();
        write_session_file(
            &missing_cwd_dir,
            &[&json!({"type": "session", "id": "s3", "cwd": missing_cwd}).to_string()],
            None,
        );
        assert_eq!(project_session(&missing_cwd_dir, &project).unwrap(), None);
    }

    #[test]
    fn matches_exact_folder_not_parent_child_or_sibling_and_ignores_other_depths() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let parent = dir.path().join("workspace");
        let target = parent.join("app");
        let child = target.join("sub");
        let sibling = parent.join("app-two");
        for path in [&target, &child, &sibling] {
            std::fs::create_dir_all(path).unwrap();
        }

        let write = |rel: &str, id: &str, cwd: &Path| {
            let line = json!({"type": "session", "version": 3, "id": id, "cwd": cwd.to_string_lossy()});
            write_session_file(&agent_dir.join("sessions").join(rel), &[&line.to_string()], None);
        };
        write("a/exact.jsonl", "exact", &target);
        write("a/parent.jsonl", "parent", &parent);
        write("a/child.jsonl", "child", &child);
        write("b/sibling.jsonl", "sibling", &sibling);
        write("stray.jsonl", "stray", &target);
        write("a/nested/deep.jsonl", "deep", &target);
        write("a/not-jsonl.txt", "txt", &target);

        let found = project_history(&agent_dir, &target).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "exact");
        assert_eq!(found[0].path, agent_dir.join("sessions/a/exact.jsonl"));
    }

    #[test]
    fn normalizes_dot_segments_and_copied_paths() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let target = dir.path().join("workspace/proj");
        let sibling = dir.path().join("workspace/other");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let dot_cwd = sibling.join("../proj/.");
        let session_path = agent_dir.join("sessions/p/one.jsonl");
        write_session_file(
            &session_path,
            &[&json!({"type": "session", "version": 3, "id": "s1", "cwd": dot_cwd.to_string_lossy(), "title": "Dot"}).to_string()],
            Some(1_700_000_000),
        );

        let copied = dir.path().join("copied-elsewhere.jsonl");
        std::fs::copy(&session_path, &copied).unwrap();

        let from_history = project_history(&agent_dir, &target).unwrap();
        assert_eq!(from_history.len(), 1);
        let from_copy = project_session(&copied, &target).unwrap().unwrap();
        assert_eq!(from_copy.id, from_history[0].id);
        assert_eq!(from_copy.title, "Dot");
        assert_eq!(from_copy.path, copied);
    }

    #[cfg(unix)]
    #[test]
    fn normalizes_unix_symlink_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("link");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let session_path = dir.path().join("s.jsonl");
        write_session_file(
            &session_path,
            &[&json!({"type": "session", "version": 3, "id": "sym", "cwd": link.to_string_lossy()}).to_string()],
            None,
        );
        assert_eq!(project_session(&session_path, &real).unwrap().unwrap().id, "sym");
        assert_eq!(project_session(&session_path, &link).unwrap().unwrap().id, "sym");
    }

    #[cfg(windows)]
    #[test]
    fn normalizes_windows_path_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("WinProj");
        std::fs::create_dir_all(&target).unwrap();
        let canonical = loa_util::fs::canonicalize(&target).unwrap();
        let canonical_str = canonical.to_string_lossy().into_owned();

        let aliases = [
            canonical_str.to_ascii_lowercase(),
            canonical_str.replace('\\', "/"),
            format!(r"{}\", canonical_str),
            format!(r"\\?\{canonical_str}"),
            format!("//?/{}", canonical_str.replace('\\', "/")),
        ];

        for (index, alias) in aliases.iter().enumerate() {
            let path = dir.path().join(format!("alias-{index}.jsonl"));
            write_session_file(
                &path,
                &[&json!({"type": "session", "version": 3, "id": format!("w{index}"), "cwd": alias}).to_string()],
                None,
            );
            assert_eq!(
                project_session(&path, &target).unwrap().map(|s| s.id),
                Some(format!("w{index}")),
                "alias failed: {alias}"
            );
        }

        let verbatim_folder = PathBuf::from(format!(r"\\?\{canonical_str}"));
        let path = dir.path().join("plain.jsonl");
        write_session_file(
            &path,
            &[&json!({"type": "session", "version": 3, "id": "plain", "cwd": canonical_str}).to_string()],
            None,
        );
        assert_eq!(
            project_session(&path, &verbatim_folder).unwrap().map(|s| s.id),
            Some("plain".to_owned())
        );
    }

    #[test]
    fn orders_newest_first_with_deterministic_tie_and_dedups_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let cwd = project.to_string_lossy().into_owned();

        let make = |rel: &str, id: &str, title: &str, secs: u64| {
            let line = json!({"type": "session", "version": 3, "id": id, "cwd": cwd, "title": title});
            write_session_file(&agent_dir.join("sessions").join(rel), &[&line.to_string()], Some(secs));
        };

        make("d1/dup-old.jsonl", "dup", "Older copy", 1_700_000_010);
        make("d2/dup-new.jsonl", "dup", "Newer copy", 1_700_000_030);
        make("d1/tie-b.jsonl", "tie-b", "Tie B", 1_700_000_020);
        make("d1/tie-a.jsonl", "tie-a", "Tie A", 1_700_000_020);
        make("d1/oldest.jsonl", "oldest", "Oldest", 1_700_000_005);

        let history = project_history(&agent_dir, &project).unwrap();
        let summary: Vec<(&str, &str, i64)> = history
            .iter()
            .map(|s| (s.id.as_str(), s.title.as_str(), s.modified))
            .collect();
        assert_eq!(
            summary,
            [
                ("dup", "Newer copy", 1_700_000_030_000),
                ("tie-a", "Tie A", 1_700_000_020_000),
                ("tie-b", "Tie B", 1_700_000_020_000),
                ("oldest", "Oldest", 1_700_000_005_000),
            ]
        );
    }

    #[test]
    fn missing_sessions_dir_is_empty_and_real_io_errors_propagate() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();

        let empty_agent = dir.path().join("missing-agent");
        assert!(project_history(&empty_agent, &project).unwrap().is_empty());

        let missing_file = dir.path().join("nope.jsonl");
        assert_eq!(project_session(&missing_file, &project).unwrap_err().kind(), ErrorKind::NotFound);

        let broken_agent = dir.path().join("broken-agent");
        std::fs::create_dir_all(&broken_agent).unwrap();
        std::fs::write(broken_agent.join("sessions"), "not a dir").unwrap();
        assert!(project_history(&broken_agent, &project).is_err());
    }

    #[test]
    fn deleting_removes_every_copy_and_its_sidecar_but_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agent");
        let (project, other) = (dir.path().join("project"), dir.path().join("other"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let write = |rel: &str, id: &str, cwd: &Path| {
            let path = agent_dir.join("sessions").join(rel);
            let line = json!({"type": "session", "version": 3, "id": id, "cwd": cwd.to_string_lossy()});
            write_session_file(&path, &[&line.to_string()], None);
            path
        };
        let sidecar = |session: &Path| {
            let dir = session.with_extension("");
            std::fs::create_dir_all(dir.join("subagents")).unwrap();
            std::fs::write(dir.join("subagents").join("task.jsonl"), "{}\n").unwrap();
            dir
        };
        let doomed = write("p/2026-10-20T17-54-21-116Z_doomed.jsonl", "doomed", &project);
        let doomed_sidecar = sidecar(&doomed);
        let copy = write("q/2026-10-21T08-00-00-000Z_doomed.jsonl", "doomed", &project);
        let kept = write("p/2026-10-19T09-00-00-000Z_kept.jsonl", "kept", &project);
        let kept_sidecar = sidecar(&kept);
        let foreign = write("o/2026-10-18T09-00-00-000Z_foreign.jsonl", "foreign", &other);
        let not_a_sidecar = copy.with_extension("");
        std::fs::write(&not_a_sidecar, "same stem, but a file").unwrap();

        delete_project_session(&agent_dir, &project, "doomed").unwrap();
        assert!(!doomed.exists() && !copy.exists() && !doomed_sidecar.exists());
        assert!(not_a_sidecar.is_file(), "only a same-stem directory is a sidecar");
        assert!(kept.is_file() && kept_sidecar.join("subagents").join("task.jsonl").is_file());
        let listed: Vec<_> = project_history(&agent_dir, &project).unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(listed, ["kept"]);

        for (folder, id) in [(&project, "foreign"), (&project, "doomed"), (&other, "kept")] {
            assert_eq!(delete_project_session(&agent_dir, folder, id).unwrap_err().kind(), ErrorKind::NotFound, "{id}");
        }
        assert!(foreign.is_file() && kept.is_file(), "ids resolve only within the folder's own history");
    }

    /// `(agent_dir, project, session file, sidecar)`: one listed session whose sidecar holds a subagent file.
    fn session_with_sidecar(dir: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let (agent_dir, project) = (dir.join("agent"), dir.join("project"));
        std::fs::create_dir_all(&project).unwrap();
        let session = agent_dir.join("sessions").join("p").join("2026-10-20T17-54-21-116Z_stuck.jsonl");
        let line = json!({"type": "session", "version": 3, "id": "stuck", "cwd": project.to_string_lossy()});
        write_session_file(&session, &[&line.to_string()], None);
        let sidecar = session.with_extension("");
        std::fs::create_dir_all(sidecar.join("subagents")).unwrap();
        std::fs::write(sidecar.join("subagents").join("task.jsonl"), "{}\n").unwrap();
        (agent_dir, project, session, sidecar)
    }

    #[test]
    fn a_sidecar_that_cant_be_removed_keeps_its_session_listed_for_another_try() {
        let dir = tempfile::tempdir().unwrap();
        let (agent_dir, project, session, sidecar) = session_with_sidecar(dir.path());
        let sessions = matching_sessions(&agent_dir, &project).unwrap();
        let error = remove_sessions(&sessions, |_| Err(io::Error::new(ErrorKind::PermissionDenied, "held open"))).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error.to_string().contains(&sidecar.display().to_string()), "{error}");
        assert!(session.is_file() && sidecar.is_dir(), "the file stays while its sidecar does");
        let listed: Vec<_> = project_history(&agent_dir, &project).unwrap().into_iter().map(|s| s.id).collect();
        assert_eq!(listed, ["stuck"]);

        delete_project_session(&agent_dir, &project, "stuck").unwrap();
        assert!(!session.exists() && !sidecar.exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_sidecar_file_held_open_on_windows_fails_the_delete_and_keeps_the_session() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (agent_dir, project, session, sidecar) = session_with_sidecar(dir.path());
        let held = std::fs::OpenOptions::new().read(true).share_mode(0).open(sidecar.join("subagents").join("task.jsonl")).unwrap();
        let error = delete_project_session(&agent_dir, &project, "stuck").unwrap_err();
        assert!(error.to_string().contains(&sidecar.display().to_string()), "{error}");
        assert!(session.is_file(), "the session stays listed");
        drop(held);
        delete_project_session(&agent_dir, &project, "stuck").unwrap();
        assert!(!session.exists() && !sidecar.exists());
    }
}
