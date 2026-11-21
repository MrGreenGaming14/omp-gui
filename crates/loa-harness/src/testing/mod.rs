//! Test doubles for harness tests: a scripted stand-in process, a sink that
//! can be awaited, the DOM convention checks, and the transcript-replay and
//! native round-trip runners.

mod dom;

pub use dom::{Phase, Rule, Violation, check_dom, check_dom_in, check_history};

use crate::kit::{EventMapper, NativeCodec};
use loa_core::{CommitError, Dom, DomWriter, Draft, MemorySink, Patch, PatchSink};
use serde::de::DeserializeOwned;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::watch;

/// A [`MemorySink`] that announces every commit's seq.
#[derive(Default)]
pub struct WatchSink {
    pub inner: MemorySink,
    pub seq: watch::Sender<u64>,
}

impl PatchSink for WatchSink {
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError> {
        let patch = self.inner.commit_at(draft, ts)?;
        self.seq.send_replace(patch.seq);
        Ok(patch)
    }
}

impl WatchSink {
    /// Waits until `done` holds for the DOM; panics after 20 seconds.
    pub async fn until(&self, what: &str, done: impl Fn(&Dom) -> bool) {
        let mut seq = self.seq.subscribe();
        let wait = async {
            while !self.inner.with_dom(&done) {
                seq.changed().await.expect("the sender lives in self");
            }
        };
        if tokio::time::timeout(Duration::from_secs(20), wait).await.is_err() {
            panic!("timed out waiting for {what}:\n{}", self.inner.with_dom(loa_core::xml::to_string));
        }
    }
}

/// Feeds every line of `fixture` (one JSON event each, `#` comments allowed)
/// to `mapper`, writing to `sink`. Returns the replies in order.
pub fn replay<M>(mut mapper: M, sink: &dyn PatchSink, fixture: &Path) -> Vec<M::Reply>
where
    M: EventMapper,
    M::Event: DeserializeOwned,
{
    let text = std::fs::read_to_string(fixture).unwrap_or_else(|e| panic!("{}: {e}", fixture.display()));
    let w = DomWriter::new(sink).at(0);
    let lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
    lines
        .flat_map(|line| {
            let event = serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}"));
            mapper.apply(event, &w).unwrap_or_else(|e| panic!("{line}: {e}"))
        })
        .collect()
}

/// Asserts `actual` equals the golden file at `path`; `LOA_BLESS=1` rewrites it.
pub fn golden(path: &Path, actual: &str) {
    if std::env::var_os("LOA_BLESS").is_some() {
        std::fs::write(path, actual).unwrap();
    }
    let expected = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(actual, expected, "{} differs (LOA_BLESS=1 rewrites it)", path.display());
}

/// Imports `native`, exports it and imports the export again; both DOMs
/// must render the same.
pub fn round_trip<C: NativeCodec>(native: &Path) {
    let import = |path: &Path| {
        let sink = MemorySink::new();
        C::import(path, &sink).unwrap_or_else(|e| panic!("importing {}: {e}", path.display()));
        sink.dom()
    };
    let first = import(native);
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join(native.file_name().unwrap_or("session".as_ref()));
    C::export(&first, &out).unwrap_or_else(|e| panic!("exporting {}: {e}", native.display()));
    assert_eq!(loa_core::xml::to_string(&import(&out)), loa_core::xml::to_string(&first), "{}", native.display());
}

const SCRIPT: &str = r#"#!/usr/bin/env bash
dir='@DIR@'
if [[ "$1" == "--version" ]]; then cat "$dir/version"; exit 0; fi
printf '%s\n' "$@" > "$dir/args"
: > "$dir/stdin.log"
declare -A ids
exec 3< "$dir/script"
while IFS= read -r step <&3 || [[ -n "$step" ]]; do
  case "$step" in
    '' | '#'*) ;;
    '> '*)
      out="${step:2}"
      for k in "${!ids[@]}"; do out="${out//"{{$k}}"/${ids[$k]}}"; done
      printf '%s\n' "$out" ;;
    '< '*)
      spec="${step:2}"; want="${spec%% *}"; name=""
      if [[ "$spec" == *' '* ]]; then name="${spec#* }"; fi
      if ! IFS= read -r line; then exit 0; fi
      printf '%s\n' "$line" >> "$dir/stdin.log"
      if [[ "$line" != *"\"type\":\"$want\""* && "$line" != *"\"method\":\"$want\""* ]]; then
        echo "fake harness: expected $want, got $line" >&2; exit 3
      fi
      if [[ -n "$name" && "$line" =~ \"id\":\"?([^\",}]*) ]]; then ids[$name]="${BASH_REMATCH[1]}"; fi ;;
    'stderr '*) echo "${step:7}" >&2 ;;
    'exit '*) exit "${step:5}" ;;
  esac
done
while IFS= read -r line; do printf '%s\n' "$line" >> "$dir/stdin.log"; done
exit 0
"#;

/// A scripted stand-in for a harness executable speaking line-delimited
/// JSON on stdio. Script lines:
/// - `> {json}`: write the frame to stdout; `{{name}}` is replaced by an id captured earlier.
/// - `< kind [name]`: read one stdin frame, fail (exit 3) unless its `type`
///   or `method` is `kind`, and capture its `id` as `name`.
/// - `stderr text`, `exit code`; `# …` and blank lines are ignored.
///
/// After the last line it reads stdin until EOF, then exits 0. It records
/// its arguments and every stdin frame for assertions.
#[derive(Debug, Clone)]
pub struct FakeProcess {
    dir: PathBuf,
    executable: PathBuf,
}

impl FakeProcess {
    /// Writes the fake into `dir` (which must exist) with `script` as its transcript.
    pub fn install(dir: &Path, script: &str) -> io::Result<Self> {
        let executable = dir.join("harness");
        std::fs::write(dir.join("script"), script)?;
        std::fs::write(dir.join("version"), "19.0.0\n")?;
        // Written by a separate process so no fork of this one can hold it
        // open for writing when it runs ("Text file busy").
        loa_util::exec::write_executable(&executable, SCRIPT.replace("@DIR@", &dir.display().to_string()))?;
        Ok(Self { dir: dir.to_path_buf(), executable })
    }

    /// Sets what `--version` prints.
    pub fn set_version(&self, version: &str) -> io::Result<()> {
        std::fs::write(self.dir.join("version"), format!("{version}\n"))
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The arguments of the last run.
    pub fn args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("args")).map(|t| t.lines().map(str::to_owned).collect()).unwrap_or_default()
    }

    /// Every frame the last run read from stdin, parsed.
    pub fn received(&self) -> Vec<serde_json::Value> {
        let text = std::fs::read_to_string(self.dir.join("stdin.log")).unwrap_or_default();
        text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()
    }
}

/// The ready frame of omp 19 (protocol v1 and v2).
#[cfg(feature = "omp")]
pub const READY_V2: &str = r#"{"type":"ready","protocolVersion":1,"supportedProtocolVersions":[1,2],"maxFrameBytes":1048576,"maxReassembledFrameBytes":67108864,"supportsBashExcludeFromContext":true}"#;

/// Script lines for omp's startup with v2 negotiation, as the client performs it.
#[cfg(feature = "omp")]
pub fn handshake_v2() -> String {
    format!(
        "> {READY_V2}\n< negotiate_protocol neg\n> {{\"type\":\"response\",\"id\":\"{{{{neg}}}}\",\"command\":\"negotiate_protocol\",\"success\":true,\"data\":{{\"protocolVersion\":2}}}}\n"
    )
}
