//! Gate ports backed by an installed omp, read without any model call:
//!
//! - tool schemas: `get_state`'s `dumpTools` of an RPC process started with
//!   `--tools <the tools the app uses>`;
//! - session format: the `version` in the header of the file a `fork` of a
//!   golden session writes;
//! - config schema: `omp config list --json`, records being open keys;
//! - contract tests: golden sessions resumed over RPC, read back with
//!   `get_entries` and forked to a new file, both compared with the golden.
//!
//! Every process runs in a fresh temporary directory with its sessions kept
//! there, so the user's sessions are never touched.

use super::contract::{ContractFailure, ContractRunner};
use super::formats::{ConfigSchema, FormatProbe};
use super::tools::{ToolCatalog, ToolSchema};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::Instant;

/// A golden session: an omp session file (JSONL) the app must keep reading
/// the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoldenSession {
    pub name: String,
    pub jsonl: String,
}

impl GoldenSession {
    pub fn new(name: impl Into<String>, jsonl: impl Into<String>) -> Self {
        Self { name: name.into(), jsonl: jsonl.into() }
    }
}

/// The golden sessions shipped with the updater: a tool round trip with
/// model and thinking changes, and a branched session with a compaction and
/// a custom message.
pub fn builtin_goldens() -> Vec<GoldenSession> {
    vec![
        GoldenSession::new("basic", include_str!("../../golden/basic.jsonl")),
        GoldenSession::new("branched", include_str!("../../golden/branched.jsonl")),
    ]
}

/// Flags that keep probe processes from loading project context, skills,
/// rules, language servers or extensions, or naming sessions.
const QUIET: [&str; 6] = [
    "--no-context-files",
    "--no-rules",
    "--no-skills",
    "--no-title",
    "--no-lsp",
    "--no-extensions",
];

/// Reads tool schemas, session format, config keys and golden session round
/// trips from an installed omp.
#[derive(Clone, Debug)]
pub struct OmpProbe {
    /// Tools loaded when listing schemas. Tools a version doesn't offer are
    /// left out of its list.
    pub tools: Vec<String>,
    pub goldens: Vec<GoldenSession>,
    /// Limit for one probe, process start included.
    pub timeout: Duration,
}

impl OmpProbe {
    pub fn new(tools: impl IntoIterator<Item = impl Into<String>>, timeout: Duration) -> Self {
        Self { tools: tools.into_iter().map(Into::into).collect(), goldens: builtin_goldens(), timeout }
    }

    async fn tool_schemas(&self, executable: &Path) -> Result<Vec<ToolSchema>, String> {
        let deadline = Instant::now() + self.timeout;
        let dir = workdir()?;
        let mut wanted = self.tools.clone();
        // Each refusal names one tool the version lacks; drop it and retry.
        for _ in 0..=self.tools.len() {
            if wanted.is_empty() {
                return Ok(Vec::new());
            }
            let args = vec!["--no-session".to_owned(), "--tools".to_owned(), wanted.join(",")];
            match Rpc::start(executable, &args, dir.path(), deadline).await {
                Ok(mut rpc) => {
                    let state = rpc.call("get_state", json!({})).await;
                    rpc.close().await;
                    return parse_dump_tools(&state?);
                }
                Err(Start::Exited(stderr)) => match refused_tool(&stderr) {
                    Some(tool) if wanted.contains(&tool) => {
                        wanted.retain(|t| *t != tool);
                    }
                    _ => return Err(format!("omp refused to start: {}", stderr.trim())),
                },
                Err(Start::Failed(e)) => return Err(e),
            }
        }
        Err("omp kept refusing the tool list".to_owned())
    }

    /// Resumes `golden` in a fresh directory. Returns the process and the
    /// directory, which must outlive it.
    async fn resume(
        &self,
        executable: &Path,
        golden: &GoldenSession,
        deadline: Instant,
    ) -> Result<(Rpc, tempfile::TempDir), String> {
        let dir = workdir()?;
        let sessions = dir.path().join("sessions");
        std::fs::create_dir_all(&sessions).map_err(|e| format!("creating {}: {e}", sessions.display()))?;
        let file = sessions.join(format!("{}.jsonl", golden.name));
        let text = with_cwd(&golden.jsonl, dir.path())?;
        std::fs::write(&file, text).map_err(|e| format!("writing {}: {e}", file.display()))?;
        let mut args: Vec<String> = QUIET.map(String::from).into();
        args.extend([
            "--session-dir".to_owned(),
            path_arg(&sessions)?,
            "--resume".to_owned(),
            path_arg(&file)?,
        ]);
        let rpc = Rpc::start(executable, &args, dir.path(), deadline).await.map_err(Start::into_message)?;
        Ok((rpc, dir))
    }

    /// Forks the resumed session and returns the lines of the file the fork
    /// wrote.
    async fn fork(rpc: &mut Rpc, original: &Path) -> Result<Vec<Value>, String> {
        rpc.call("fork", json!({})).await?;
        let state = rpc.call("get_state", json!({})).await?;
        let file = state
            .get("sessionFile")
            .and_then(Value::as_str)
            .ok_or("get_state after fork has no sessionFile")?;
        if Path::new(file) == original {
            return Err("fork did not switch to a new session file".to_owned());
        }
        let text = std::fs::read_to_string(file).map_err(|e| format!("reading forked {file}: {e}"))?;
        parse_lines(&text)
    }

    async fn round_trip(&self, executable: &Path, golden: &GoldenSession) -> Result<Vec<ContractFailure>, String> {
        let deadline = Instant::now() + self.timeout;
        let expected = golden_entries(golden)?;
        let (mut rpc, dir) = self.resume(executable, golden, deadline).await?;
        let original = dir.path().join("sessions").join(format!("{}.jsonl", golden.name));
        let read = async {
            let data = rpc.call("get_entries", json!({})).await?;
            let entries = data.get("entries").and_then(Value::as_array).cloned().ok_or("get_entries has no entries")?;
            let leaf = data.get("leafId").and_then(Value::as_str).map(str::to_owned);
            let forked = Self::fork(&mut rpc, &original).await?;
            Ok::<_, String>((entries, leaf, forked))
        }
        .await;
        rpc.close().await;
        let (entries, leaf, forked) = read?;

        let mut failures = Vec::new();
        let diff = diff_entries(&expected, &entries);
        if !diff.is_empty() {
            failures.push(ContractFailure { test: format!("{}: read back", golden.name), diff: diff.join("\n") });
        }
        let expected_leaf = expected.last().and_then(|e| e.get("id")).and_then(Value::as_str);
        if leaf.as_deref() != expected_leaf {
            failures.push(ContractFailure {
                test: format!("{}: leaf", golden.name),
                diff: format!("- leaf {}\n+ leaf {}", expected_leaf.unwrap_or("none"), leaf.as_deref().unwrap_or("none")),
            });
        }
        // A fork must carry the active branch; it may carry the other
        // branches too, unchanged.
        let forked_entries: Vec<Value> = forked.into_iter().filter(is_entry).collect();
        let required: BTreeSet<String> =
            branch(&expected, expected_leaf).iter().filter_map(entry_id).map(str::to_owned).collect();
        let carried: BTreeSet<&str> = forked_entries.iter().filter_map(entry_id).collect();
        let expected_fork: Vec<Value> = expected
            .iter()
            .filter(|e| entry_id(e).is_some_and(|id| required.contains(id) || carried.contains(id)))
            .cloned()
            .collect();
        let diff = diff_entries(&expected_fork, &forked_entries);
        if !diff.is_empty() {
            failures.push(ContractFailure { test: format!("{}: fork round trip", golden.name), diff: diff.join("\n") });
        }
        Ok(failures)
    }
}

#[async_trait::async_trait]
impl ToolCatalog for OmpProbe {
    async fn tools(&self, executable: &Path) -> Result<Vec<ToolSchema>, String> {
        self.tool_schemas(executable).await
    }
}

#[async_trait::async_trait]
impl FormatProbe for OmpProbe {
    async fn session_version(&self, executable: &Path) -> Result<u32, String> {
        let golden = self.goldens.first().ok_or("no golden session to fork")?;
        let deadline = Instant::now() + self.timeout;
        let (mut rpc, dir) = self.resume(executable, golden, deadline).await?;
        let original = dir.path().join("sessions").join(format!("{}.jsonl", golden.name));
        let forked = Self::fork(&mut rpc, &original).await;
        rpc.close().await;
        let header = forked?
            .into_iter()
            .find(|line| line.get("type").and_then(Value::as_str) == Some("session"))
            .ok_or("forked session file has no session header")?;
        let version = header.get("version").and_then(Value::as_u64).ok_or("session header has no version")?;
        u32::try_from(version).map_err(|_| format!("session version {version} out of range"))
    }

    async fn config_keys(&self, executable: &Path) -> Result<ConfigSchema, String> {
        let dir = workdir()?;
        let child = loa_exec::retry_busy(|| {
            Command::new(loa_exec::resolve(executable))
                .args(["config", "list", "--json"])
                .current_dir(dir.path())
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
        })
        .map_err(|e| format!("cannot start {}: {e}", executable.display()))?;
        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .map_err(|_| format!("config list did not finish within {:?}", self.timeout))?
            .map_err(|e| format!("running config list: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "config list exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        parse_config_list(&output.stdout)
    }
}

#[async_trait::async_trait]
impl ContractRunner for OmpProbe {
    async fn run(&self, executable: &Path) -> Result<Vec<ContractFailure>, String> {
        if self.goldens.is_empty() {
            return Err("no golden sessions".to_owned());
        }
        let runs = self.goldens.iter().map(|golden| self.round_trip(executable, golden));
        let mut failures = Vec::new();
        for (golden, result) in self.goldens.iter().zip(futures::future::join_all(runs).await) {
            failures.extend(result.map_err(|e| format!("{}: {e}", golden.name))?);
        }
        Ok(failures)
    }
}

/// Tool schemas out of `get_state` data.
fn parse_dump_tools(state: &Value) -> Result<Vec<ToolSchema>, String> {
    let tools = state.get("dumpTools").and_then(Value::as_array).ok_or("get_state has no dumpTools")?;
    tools
        .iter()
        .map(|tool| {
            let name = tool.get("name").and_then(Value::as_str).ok_or("dumpTools entry without a name")?;
            let input_schema = tool.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"}));
            Ok(ToolSchema { name: name.to_owned(), input_schema })
        })
        .collect()
}

/// The tool omp names when refusing `--tools`: `Unknown tool in --tools: x.`
/// or `Built-in tool unavailable in this session: x.`
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
    let map: Map<String, Value> =
        serde_json::from_slice(stdout).map_err(|e| format!("config list is not a JSON object: {e}"))?;
    let open = map
        .iter()
        .filter(|(_, info)| info.get("type").and_then(Value::as_str) == Some("record"))
        .map(|(key, _)| key.clone())
        .collect();
    Ok(ConfigSchema { keys: map.into_iter().map(|(key, _)| key).collect(), open, renamed: BTreeMap::new() })
}

fn parse_lines(text: &str) -> Result<Vec<Value>, String> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|e| format!("invalid session line: {e}")))
        .collect()
}

fn entry_id(entry: &Value) -> Option<&str> {
    entry.get("id")?.as_str()
}

/// Lines that are tree entries: everything but the header and title lines.
fn is_entry(line: &Value) -> bool {
    line.get("id").is_some() && !matches!(line.get("type").and_then(Value::as_str), Some("session" | "title"))
}

fn golden_entries(golden: &GoldenSession) -> Result<Vec<Value>, String> {
    Ok(parse_lines(&golden.jsonl)?.into_iter().filter(is_entry).collect())
}

/// The golden text with its header's `cwd` set to `dir`.
fn with_cwd(jsonl: &str, dir: &Path) -> Result<String, String> {
    let mut out = String::with_capacity(jsonl.len());
    for mut line in parse_lines(jsonl)? {
        if line.get("type").and_then(Value::as_str) == Some("session")
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
fn branch(entries: &[Value], leaf: Option<&str>) -> Vec<Value> {
    let by_id: BTreeMap<&str, &Value> =
        entries.iter().filter_map(|e| Some((e.get("id")?.as_str()?, e))).collect();
    let mut path = Vec::new();
    let mut at = leaf;
    let mut seen = BTreeSet::new();
    while let Some(id) = at {
        let Some(entry) = by_id.get(id) else { break };
        if !seen.insert(id) {
            break;
        }
        path.push((*entry).clone());
        at = entry.get("parentId").and_then(Value::as_str);
    }
    path.reverse();
    path
}

/// Differences between expected and actual entries, matched by id. Fields
/// the actual entries add are allowed; missing, changed and extra entries
/// are not.
fn diff_entries(expected: &[Value], actual: &[Value]) -> Vec<String> {
    let actual_by_id: BTreeMap<&str, &Value> =
        actual.iter().filter_map(|e| Some((e.get("id")?.as_str()?, e))).collect();
    let mut lines = Vec::new();
    for entry in expected {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or("?");
        let kind = entry.get("type").and_then(Value::as_str).unwrap_or("?");
        match actual_by_id.get(id) {
            None => lines.push(format!("- entry {id} ({kind}) missing")),
            Some(found) => subset_diff(entry, found, id, &mut lines),
        }
    }
    let expected_ids: BTreeSet<&str> =
        expected.iter().filter_map(|e| e.get("id")?.as_str()).collect();
    for entry in actual {
        if let Some(id) = entry.get("id").and_then(Value::as_str)
            && !expected_ids.contains(id)
        {
            let kind = entry.get("type").and_then(Value::as_str).unwrap_or("?");
            lines.push(format!("+ entry {id} ({kind}) unexpected"));
        }
    }
    lines
}

/// Appends a line for every place `actual` doesn't hold what `expected`
/// holds. Objects may carry extra keys; arrays must match element-wise.
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

fn workdir() -> Result<tempfile::TempDir, String> {
    tempfile::Builder::new()
        .prefix("loa-updater-probe-")
        .tempdir()
        .map_err(|e| format!("creating a probe directory: {e}"))
}

fn path_arg(path: &Path) -> Result<String, String> {
    path.to_str().map(str::to_owned).ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

/// Why an RPC process didn't come up.
enum Start {
    /// It exited before its ready frame, with this stderr.
    Exited(String),
    Failed(String),
}

impl Start {
    fn into_message(self) -> String {
        match self {
            Self::Exited(stderr) => format!("omp exited before it was ready: {}", stderr.trim()),
            Self::Failed(message) => message,
        }
    }
}

/// A minimal JSON-lines RPC exchange with an omp process.
struct Rpc {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    deadline: Instant,
}

impl Rpc {
    /// Starts `executable --mode rpc <args>` in `cwd` and waits for its
    /// `ready` frame.
    async fn start(executable: &Path, args: &[String], cwd: &Path, deadline: Instant) -> Result<Self, Start> {
        let mut child = loa_exec::retry_busy(|| {
            Command::new(loa_exec::resolve(executable))
                .args(["--mode", "rpc"])
                .args(args)
                .current_dir(cwd)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
        })
        .map_err(|e| Start::Failed(format!("cannot start {}: {e}", executable.display())))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| Start::Failed("stdout not captured".into()))?;
        let mut stderr = child.stderr.take().ok_or_else(|| Start::Failed("stderr not captured".into()))?;
        // Drained concurrently so a chatty process never blocks on a full pipe.
        let stderr_task = tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        });
        let mut rpc = Self { child, stdin, lines: BufReader::new(stdout).lines(), next_id: 0, deadline };
        loop {
            match rpc.next_frame().await {
                Ok(Some(frame)) if frame.get("type").and_then(Value::as_str) == Some("ready") => {
                    // stderr keeps draining in its task until the process ends.
                    return Ok(rpc);
                }
                Ok(Some(_)) => continue,
                Ok(None) => {
                    let _ = rpc.child.wait().await;
                    let stderr = stderr_task.await.unwrap_or_default();
                    return Err(Start::Exited(stderr));
                }
                Err(e) => {
                    rpc.close().await;
                    return Err(Start::Failed(e));
                }
            }
        }
    }

    /// The next JSON frame, `None` at end of output.
    async fn next_frame(&mut self) -> Result<Option<Value>, String> {
        loop {
            let line = tokio::time::timeout_at(self.deadline, self.lines.next_line())
                .await
                .map_err(|_| "omp did not answer in time".to_owned())?
                .map_err(|e| format!("reading omp output: {e}"))?;
            let Some(line) = line else { return Ok(None) };
            if line.trim().is_empty() {
                continue;
            }
            // Non-JSON lines (banners, logs) are skipped.
            if let Ok(frame) = serde_json::from_str::<Value>(&line) {
                return Ok(Some(frame));
            }
        }
    }

    /// Sends a command and returns the `data` of its successful response.
    async fn call(&mut self, kind: &str, fields: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = format!("loa-updater-{}", self.next_id);
        let mut command = match fields {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        command.insert("id".to_owned(), Value::String(id.clone()));
        command.insert("type".to_owned(), Value::String(kind.to_owned()));
        let mut line = Value::Object(command).to_string();
        line.push('\n');
        let stdin = self.stdin.as_mut().ok_or("omp stdin is closed")?;
        stdin.write_all(line.as_bytes()).await.map_err(|e| format!("writing to omp: {e}"))?;
        stdin.flush().await.map_err(|e| format!("writing to omp: {e}"))?;
        loop {
            let frame = self.next_frame().await?.ok_or_else(|| format!("omp exited before answering {kind}"))?;
            if frame.get("type").and_then(Value::as_str) != Some("response")
                || frame.get("id").and_then(Value::as_str) != Some(&id)
            {
                continue;
            }
            if frame.get("success").and_then(Value::as_bool) == Some(true) {
                return Ok(frame.get("data").cloned().unwrap_or(Value::Null));
            }
            let error = frame.get("error").map(|e| e.as_str().map_or_else(|| e.to_string(), str::to_owned));
            return Err(format!("{kind} failed: {}", error.unwrap_or_else(|| "no error message".into())));
        }
    }

    /// Kills the process and waits for it.
    async fn close(mut self) {
        // Best effort: the process may already have exited.
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        self.stdin = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refused_tool_names() {
        assert_eq!(
            refused_tool("Error: Built-in tool unavailable in this session: ast_grep.\nRun `omp --help`"),
            Some("ast_grep".to_owned())
        );
        assert_eq!(refused_tool("Error: Unknown tool in --tools: bogus.\nBuilt-in tools: read"), Some("bogus".to_owned()));
        assert_eq!(refused_tool("Error: something else"), None);
    }

    #[test]
    fn config_list_records_are_open() {
        let schema = parse_config_list(
            br#"{"modelRoles":{"type":"record","value":{}},"memory.backend":{"type":"enum","value":"off"}}"#,
        )
        .unwrap();
        assert!(schema.accepts("modelRoles.smol"));
        assert!(schema.accepts("memory.backend"));
        assert!(!schema.accepts("memory.backend.x"));
        assert!(parse_config_list(b"[]").is_err());
    }

    #[test]
    fn diff_allows_added_fields_and_reports_changes() {
        let expected = vec![json!({"id": "a", "type": "message", "message": {"content": [{"text": "hi"}]}})];
        let added = vec![json!({"id": "a", "type": "message", "runtime": {}, "message": {"content": [{"text": "hi", "x": 1}]}})];
        assert!(diff_entries(&expected, &added).is_empty());
        let changed = vec![
            json!({"id": "a", "type": "message", "message": {"content": [{"text": "bye"}]}}),
            json!({"id": "b", "type": "custom"}),
        ];
        assert_eq!(
            diff_entries(&expected, &changed),
            ["~ a.message.content[0].text: \"hi\" -> \"bye\"", "+ entry b (custom) unexpected"]
        );
        assert_eq!(diff_entries(&expected, &[]), ["- entry a (message) missing"]);
    }

    #[test]
    fn goldens_parse_and_branch_follows_parents() {
        for golden in builtin_goldens() {
            let entries = golden_entries(&golden).unwrap();
            assert!(!entries.is_empty(), "{}", golden.name);
            let leaf = entries.last().unwrap()["id"].as_str().unwrap().to_owned();
            let path = branch(&entries, Some(&leaf));
            assert_eq!(path.last().unwrap()["id"], leaf.as_str());
            assert_eq!(path.first().unwrap()["parentId"], Value::Null);
        }
        let branched = golden_entries(&builtin_goldens()[1]).unwrap();
        let path = branch(&branched, Some("c0000010"));
        let ids: Vec<&str> = path.iter().map(|e| e["id"].as_str().unwrap()).collect();
        assert!(!ids.contains(&"c0000003"), "off-branch entries are not on the path: {ids:?}");
    }

    #[test]
    fn header_cwd_is_replaced() {
        let text = with_cwd(&builtin_goldens()[0].jsonl, Path::new("/tmp/x")).unwrap();
        let header: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(header["cwd"], "/tmp/x");
    }
}
