//! The omp-backed gate ports of the updater. Probes read an installed omp without any model call
//! (tool schemas from `get_state`, the session format from a forked golden session, config keys
//! from `config list --json`, golden round trips over RPC); every process runs in a fresh
//! temporary directory, so the user's sessions are never touched. Changelogs are reviewed by
//! a one-shot call on the `smol` role.

use async_trait::async_trait;
use loa_contracts::Summarizer;
use loa_harness::kit::rpc::RpcError;
use loa_harness::omp::OneShot;
use loa_harness::omp::rpc::{CancellationResult, Command, RpcClient, SessionState, SpawnOptions};
use loa_updater::gates::{
    ChangelogReview, ChangelogSummarizer, ConfigSchema, ContractFailure, ContractRunner, FormatProbe, GatePorts, OmpGates, Probe,
    REVIEW_PROMPT, RpcProbe, ToolCatalog, ToolSchema, parse_review,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;

/// Golden omp sessions (JSONL) the app must keep reading the same way.
const GOLDENS: &[(&str, &str)] = &[("basic", include_str!("golden/basic.jsonl")), ("branched", include_str!("golden/branched.jsonl"))];

/// Flags that keep probes from loading project context, skills, rules, language servers or extensions.
const QUIET: [&str; 6] = ["--no-context-files", "--no-rules", "--no-skills", "--no-title", "--no-lsp", "--no-extensions"];

/// Plugs the omp probes and the smol reviewer into the updater.
#[derive(Debug)]
pub struct OmpPorts;

impl GatePorts for OmpPorts {
    fn probe(&self, gates: &OmpGates, timeout: Duration) -> Box<dyn Probe> {
        Box::new(OmpProbe { tools: gates.used_tools.keys().cloned().collect(), timeout })
    }

    fn reviewer(&self, reviewer: &Path, model: &str, timeout: Duration) -> Box<dyn ChangelogSummarizer> {
        Box::new(SmolReviewer(OneShot::new(reviewer, std::env::temp_dir()).with_timeout(timeout).with_model(model)))
    }
}

/// Reviews changelogs through a one-shot model call and parses the reply strictly.
pub struct SmolReviewer<S>(pub S);

#[async_trait]
impl<S: Summarizer> ChangelogSummarizer for SmolReviewer<S> {
    async fn review(&self, changelog: &str) -> Result<ChangelogReview, String> {
        parse_review(&self.0.summarize(REVIEW_PROMPT, changelog).await?)
    }
}

/// Reads tool schemas, session format, config keys, RPC versions and golden round trips from an omp.
#[derive(Clone, Debug)]
pub struct OmpProbe {
    /// Tools loaded when listing schemas; tools a version lacks are left out of its list.
    pub tools: Vec<String>,
    /// Limit for one probe, process start included.
    pub timeout: Duration,
}

/// A process resumed on a golden session in its own directory, which must outlive it.
struct Resumed {
    rpc: RpcClient,
    file: PathBuf,
    _dir: TempDir,
}

impl OmpProbe {
    async fn spawn(&self, executable: &Path, args: &[String], cwd: &Path) -> Result<RpcClient, RpcError> {
        let mut opts = SpawnOptions::new(executable);
        opts.args = args.iter().map(Into::into).collect();
        opts.cwd = Some(cwd.to_owned());
        opts.ready_timeout = self.timeout;
        opts.negotiate = false;
        RpcClient::spawn(opts).await.map(|(rpc, _)| rpc)
    }

    /// Runs `work` within the probe's time limit.
    async fn timed<T>(&self, work: impl Future<Output = Result<T, String>>) -> Result<T, String> {
        tokio::time::timeout(self.timeout, work).await.map_err(|_| format!("omp did not answer within {:?}", self.timeout))?
    }

    async fn resume(&self, executable: &Path, name: &str, jsonl: &str) -> Result<Resumed, String> {
        let dir = workdir()?;
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).map_err(|e| format!("creating {}: {e}", sessions.display()))?;
        let file = sessions.join(format!("{name}.jsonl"));
        std::fs::write(&file, with_cwd(jsonl, dir.path())?).map_err(|e| format!("writing {}: {e}", file.display()))?;
        let mut args: Vec<String> = QUIET.map(String::from).into();
        args.extend(["--session-dir".into(), path_arg(&sessions)?, "--resume".into(), path_arg(&file)?]);
        let rpc = self.spawn(executable, &args, dir.path()).await.map_err(|e| e.to_string())?;
        Ok(Resumed { rpc, file, _dir: dir })
    }

    /// Forks the resumed session and returns the lines of the file the fork wrote.
    async fn fork(resumed: &Resumed) -> Result<Vec<Value>, String> {
        resumed.rpc.request::<CancellationResult>(Command::Fork { entry_id: None }).await.map_err(|e| format!("fork: {e}"))?;
        let state = resumed.rpc.request::<SessionState>(Command::GetState).await.map_err(|e| format!("get_state: {e}"))?;
        let file = state.session_file.ok_or("get_state after fork has no sessionFile")?;
        if Path::new(&file) == resumed.file {
            return Err("fork did not switch to a new session file".to_owned());
        }
        parse_lines(&std::fs::read_to_string(&file).map_err(|e| format!("reading forked {file}: {e}"))?)
    }

    async fn round_trip(&self, executable: &Path, name: &str, jsonl: &str) -> Result<Vec<ContractFailure>, String> {
        let expected = entries(jsonl)?;
        let resumed = self.resume(executable, name, jsonl).await?;
        let data = resumed.rpc.request::<Value>(Command::GetEntries).await.map_err(|e| format!("get_entries: {e}"))?;
        let forked = Self::fork(&resumed).await;
        resumed.rpc.kill();
        let read = data.get("entries").and_then(Value::as_array).ok_or("get_entries has no entries")?;
        let leaf = data.get("leafId").and_then(Value::as_str);
        let forked: Vec<Value> = forked?.into_iter().filter(is_entry).collect();

        let mut failures = Vec::new();
        let mut fail = |test: &str, diff: Vec<String>| {
            if !diff.is_empty() {
                failures.push(ContractFailure { test: format!("{name}: {test}"), diff: diff.join("\n") });
            }
        };
        fail("read back", diff_entries(&expected, read));
        let expected_leaf = expected.last().and_then(entry_id);
        if leaf != expected_leaf {
            fail("leaf", vec![format!("- leaf {}", expected_leaf.unwrap_or("none")), format!("+ leaf {}", leaf.unwrap_or("none"))]);
        }
        // A fork must carry the active branch; it may carry the other branches too, unchanged.
        let required: BTreeSet<&str> = branch(&expected, expected_leaf).into_iter().filter_map(entry_id).collect();
        let carried: BTreeSet<&str> = forked.iter().filter_map(entry_id).collect();
        let expected_fork: Vec<Value> =
            expected.iter().filter(|e| entry_id(e).is_some_and(|id| required.contains(id) || carried.contains(id))).cloned().collect();
        fail("fork round trip", diff_entries(&expected_fork, &forked));
        Ok(failures)
    }
}

#[async_trait]
impl ToolCatalog for OmpProbe {
    async fn tools(&self, executable: &Path) -> Result<Vec<ToolSchema>, String> {
        let dir = workdir()?;
        let mut wanted = self.tools.clone();
        // Each refusal names one tool the version lacks; drop it and retry.
        while !wanted.is_empty() {
            let args = ["--no-session".to_owned(), "--tools".to_owned(), wanted.join(",")];
            match self.spawn(executable, &args, dir.path()).await {
                Ok(rpc) => {
                    let state = self.timed(async { rpc.request::<Value>(Command::GetState).await.map_err(|e| e.to_string()) }).await;
                    rpc.kill();
                    return parse_dump_tools(&state?);
                }
                Err(RpcError::Exited { stderr, .. }) => match refused_tool(&stderr) {
                    Some(tool) if wanted.contains(&tool) => wanted.retain(|t| *t != tool),
                    _ => return Err(format!("omp refused to start: {stderr}")),
                },
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(Vec::new())
    }
}

#[async_trait]
impl FormatProbe for OmpProbe {
    async fn session_version(&self, executable: &Path) -> Result<u32, String> {
        let (name, jsonl) = GOLDENS[0];
        let forked = self
            .timed(async {
                let resumed = self.resume(executable, name, jsonl).await?;
                let forked = Self::fork(&resumed).await;
                resumed.rpc.kill();
                forked
            })
            .await?;
        let header = forked.into_iter().find(|l| l["type"] == "session").ok_or("forked session file has no session header")?;
        let version = header["version"].as_u64().ok_or("session header has no version")?;
        u32::try_from(version).map_err(|_| format!("session version {version} out of range"))
    }

    async fn config_keys(&self, executable: &Path) -> Result<ConfigSchema, String> {
        let dir = workdir()?;
        let mut cmd = tokio::process::Command::new(executable);
        cmd.args(["config", "list", "--json"]).current_dir(dir.path());
        let stdout = self.timed(loa_util::exec::run_checked(cmd)).await.map_err(|e| format!("config list: {e}"))?;
        parse_config_list(stdout.as_bytes())
    }
}

#[async_trait]
impl RpcProbe for OmpProbe {
    /// Reads the `ready` frame of `--mode rpc --no-session`; nothing is sent, so no model is called.
    async fn protocol_versions(&self, executable: &Path) -> Result<Vec<u32>, String> {
        let dir = workdir()?;
        match self.spawn(executable, &["--no-session".to_owned()], dir.path()).await {
            Ok(rpc) => {
                rpc.kill();
                Ok(rpc.ready().offered_versions())
            }
            Err(RpcError::Version { offered, .. }) => Ok(offered),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[async_trait]
impl ContractRunner for OmpProbe {
    async fn run(&self, executable: &Path) -> Result<Vec<ContractFailure>, String> {
        let runs = GOLDENS.iter().map(|(name, jsonl)| self.timed(self.round_trip(executable, name, jsonl)));
        let mut failures = Vec::new();
        for ((name, _), result) in GOLDENS.iter().zip(futures::future::join_all(runs).await) {
            failures.extend(result.map_err(|e| format!("{name}: {e}"))?);
        }
        Ok(failures)
    }
}

/// Tool schemas out of `get_state` data.
fn parse_dump_tools(state: &Value) -> Result<Vec<ToolSchema>, String> {
    let tools = state.get("dumpTools").and_then(Value::as_array).ok_or("get_state has no dumpTools")?;
    let schema = |tool: &Value| {
        let name = tool.get("name").and_then(Value::as_str).ok_or("dumpTools entry without a name")?;
        let input_schema = tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"}));
        Ok(ToolSchema { name: name.to_owned(), input_schema })
    };
    tools.iter().map(schema).collect()
}

/// The tool omp names when refusing `--tools`.
fn refused_tool(stderr: &str) -> Option<String> {
    const MARKERS: [&str; 2] = ["Unknown tool in --tools:", "Built-in tool unavailable in this session:"];
    stderr.lines().find_map(|line| {
        let rest = MARKERS.iter().find_map(|m| line.split_once(m).map(|(_, rest)| rest))?;
        let name = rest.trim().trim_end_matches('.').trim();
        (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| name.to_owned())
    })
}

/// Keys of `config list --json`; `record` keys are open.
fn parse_config_list(stdout: &[u8]) -> Result<ConfigSchema, String> {
    let map: Map<String, Value> = serde_json::from_slice(stdout).map_err(|e| format!("config list is not a JSON object: {e}"))?;
    let open = map.iter().filter(|(_, info)| info["type"] == "record").map(|(key, _)| key.clone()).collect();
    Ok(ConfigSchema { keys: map.into_iter().map(|(key, _)| key).collect(), open, renamed: BTreeMap::new() })
}

fn parse_lines(text: &str) -> Result<Vec<Value>, String> {
    let line = |l: &str| serde_json::from_str(l).map_err(|e| format!("invalid session line: {e}"));
    text.lines().filter(|l| !l.trim().is_empty()).map(line).collect()
}

fn entry_id(entry: &Value) -> Option<&str> {
    entry.get("id")?.as_str()
}

/// Tree entries: every line with an id but the header and title.
fn is_entry(line: &Value) -> bool {
    line.get("id").is_some() && !matches!(line["type"].as_str(), Some("session" | "title"))
}

fn entries(jsonl: &str) -> Result<Vec<Value>, String> {
    Ok(parse_lines(jsonl)?.into_iter().filter(is_entry).collect())
}

/// The golden text with its header's `cwd` set to `dir`.
fn with_cwd(jsonl: &str, dir: &Path) -> Result<String, String> {
    let mut out = String::with_capacity(jsonl.len());
    for mut line in parse_lines(jsonl)? {
        if line["type"] == "session"
            && let Some(header) = line.as_object_mut()
        {
            header.insert("cwd".to_owned(), Value::String(path_arg(dir)?));
        }
        out.push_str(&line.to_string());
        out.push('\n');
    }
    Ok(out)
}

/// Entries from the root to `leaf`, root first.
fn branch<'v>(entries: &'v [Value], leaf: Option<&str>) -> Vec<&'v Value> {
    let by_id: BTreeMap<&str, &Value> = entries.iter().filter_map(|e| Some((entry_id(e)?, e))).collect();
    let (mut path, mut seen, mut at) = (Vec::new(), BTreeSet::new(), leaf);
    while let Some(entry) = at.and_then(|id| by_id.get(id)).filter(|_| seen.insert(at)) {
        path.push(*entry);
        at = entry["parentId"].as_str();
    }
    path.reverse();
    path
}

/// Differences between expected and actual entries, by id: added fields pass; missing, changed and extra entries don't.
fn diff_entries(expected: &[Value], actual: &[Value]) -> Vec<String> {
    let actual_by_id: BTreeMap<&str, &Value> = actual.iter().filter_map(|e| Some((entry_id(e)?, e))).collect();
    let kind = |e: &Value| e["type"].as_str().unwrap_or("?").to_owned();
    let mut lines = Vec::new();
    for entry in expected {
        let id = entry_id(entry).unwrap_or("?");
        match actual_by_id.get(id) {
            None => lines.push(format!("- entry {id} ({}) missing", kind(entry))),
            Some(found) => subset_diff(entry, found, id, &mut lines),
        }
    }
    let expected_ids: BTreeSet<&str> = expected.iter().filter_map(entry_id).collect();
    let extra = actual.iter().filter_map(|e| Some((entry_id(e)?, e))).filter(|(id, _)| !expected_ids.contains(id));
    lines.extend(extra.map(|(id, e)| format!("+ entry {id} ({}) unexpected", kind(e))));
    lines
}

/// A line for every place `actual` lacks what `expected` holds; objects may add keys, arrays match element-wise.
fn subset_diff(expected: &Value, actual: &Value, path: &str, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(want), Value::Object(have)) => {
            for (key, value) in want {
                let at = format!("{path}.{key}");
                match have.get(key) {
                    Some(found) => subset_diff(value, found, &at, out),
                    None => out.push(format!("- {at}: missing (was {})", short(value))),
                }
            }
        }
        (Value::Array(want), Value::Array(have)) if want.len() == have.len() => {
            for (index, (w, h)) in want.iter().zip(have).enumerate() {
                subset_diff(w, h, &format!("{path}[{index}]"), out);
            }
        }
        _ if expected == actual => {}
        _ => out.push(format!("~ {path}: {} -> {}", short(expected), short(actual))),
    }
}

fn short(value: &Value) -> String {
    let text = value.to_string();
    match text.char_indices().nth(80) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
}

fn workdir() -> Result<TempDir, String> {
    tempfile::Builder::new().prefix("loa-updater-probe-").tempdir().map_err(|e| format!("creating a probe directory: {e}"))
}

fn path_arg(path: &Path) -> Result<String, String> {
    path.to_str().map(str::to_owned).ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_and_config_lists_parse() {
        let unavailable = "Error: Built-in tool unavailable in this session: ast_grep.\nRun `omp --help`";
        assert_eq!(refused_tool(unavailable).as_deref(), Some("ast_grep"));
        assert_eq!(refused_tool("Error: Unknown tool in --tools: bogus.").as_deref(), Some("bogus"));
        assert_eq!(refused_tool("Error: something else"), None);
        let list = br#"{"modelRoles":{"type":"record","value":{}},"memory.backend":{"type":"enum","value":"off"}}"#;
        let schema = parse_config_list(list).unwrap();
        assert!(schema.accepts("modelRoles.smol") && schema.accepts("memory.backend") && !schema.accepts("memory.backend.x"));
        assert!(parse_config_list(b"[]").is_err());
    }

    #[tokio::test]
    async fn ready_probe_reads_offered_versions_in_and_out_of_range() {
        let dir = tempfile::tempdir().unwrap();
        let probe = OmpProbe { tools: Vec::new(), timeout: Duration::from_secs(20) };
        for (i, versions) in ["[1,2]", "[3]"].into_iter().enumerate() {
            let (exe, seen) = (dir.path().join(format!("omp{i}")), dir.path().join(format!("args{i}")));
            let ready = format!(r#"{{"type":"ready","protocolVersion":1,"supportedProtocolVersions":{versions}}}"#);
            let body = format!("#!/bin/sh\necho \"$@\" > '{}'\necho '{ready}'\nread ignored\n", seen.display());
            loa_util::exec::write_executable(&exe, body).unwrap();
            let offered = probe.protocol_versions(&exe).await.unwrap();
            assert_eq!(serde_json::to_string(&offered).unwrap(), versions);
            assert_eq!(std::fs::read_to_string(&seen).unwrap().trim(), "--mode rpc --no-session");
        }
    }

    #[test]
    fn diff_allows_added_fields_and_reports_changes() {
        let expected = vec![json!({"id": "a", "type": "message", "message": {"content": [{"text": "hi"}]}})];
        let added = vec![json!({"id": "a", "type": "message", "runtime": {}, "message": {"content": [{"text": "hi", "x": 1}]}})];
        assert!(diff_entries(&expected, &added).is_empty());
        let changed = vec![json!({"id": "a", "type": "message", "message": {"content": [{"text": "bye"}]}}), json!({"id": "b", "type": "custom"})];
        assert_eq!(diff_entries(&expected, &changed), ["~ a.message.content[0].text: \"hi\" -> \"bye\"", "+ entry b (custom) unexpected"]);
        assert_eq!(diff_entries(&expected, &[]), ["- entry a (message) missing"]);
    }

    #[test]
    fn goldens_parse_branch_and_take_the_cwd() {
        for (name, jsonl) in GOLDENS {
            let entries = entries(jsonl).unwrap();
            let path = branch(&entries, entries.last().and_then(entry_id));
            assert_eq!((path.last().map(|e| e["id"].clone()), &path[0]["parentId"]), (entries.last().map(|e| e["id"].clone()), &Value::Null), "{name}");
        }
        let branched = entries(GOLDENS[1].1).unwrap();
        let ids: Vec<&str> = branch(&branched, Some("c0000010")).into_iter().filter_map(entry_id).collect();
        assert!(!ids.contains(&"c0000003"), "off-branch entries are not on the path: {ids:?}");
        let text = with_cwd(GOLDENS[0].1, Path::new("/tmp/x")).unwrap();
        assert_eq!(serde_json::from_str::<Value>(text.lines().next().unwrap()).unwrap()["cwd"], "/tmp/x");
    }

    struct Canned(&'static str);

    #[async_trait]
    impl Summarizer for Canned {
        async fn summarize(&self, instructions: &str, input: &str) -> Result<String, String> {
            assert!(instructions.ends_with("Changelog:\n\n") && input.contains("- fix"));
            Ok(self.0.to_owned())
        }
    }

    #[tokio::test]
    async fn smol_reviewer_parses_the_reply_and_rejects_junk() {
        let review = SmolReviewer(Canned(r#"{"breaking": ["edit: path renamed"], "removed": []}"#));
        assert_eq!(review.review("## 2.0.0\n\n- fix").await.unwrap().breaking, ["edit: path renamed"]);
        assert!(SmolReviewer(Canned("Sure! Nothing breaking here.")).review("- fix").await.is_err());
    }
}
