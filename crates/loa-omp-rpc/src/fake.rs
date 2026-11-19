//! A scripted stand-in for the `omp` executable, for tests of this crate and
//! of crates built on it. It speaks the RPC transport (one JSON frame per
//! line) and replays a transcript.
//!
//! Script lines:
//! - `> {json}`: write the frame to stdout. `{{name}}` is replaced by an id
//!   captured earlier.
//! - `< type [name]`: read one stdin frame, fail (exit 3) unless its `type` is
//!   `type`, and capture its `id` as `name`.
//! - `stderr text`: write `text` to stderr.
//! - `exit code`: exit with `code`.
//! - `# …` and blank lines are ignored.
//!
//! After the last line the fake reads stdin until EOF, then exits 0. It
//! records its arguments and every stdin frame for assertions.

use std::io;
use std::path::{Path, PathBuf};

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
      if [[ "$line" != *"\"type\":\"$want\""* ]]; then echo "fake omp: expected $want, got $line" >&2; exit 3; fi
      if [[ -n "$name" && "$line" =~ \"id\":\"([^\"]*)\" ]]; then ids[$name]="${BASH_REMATCH[1]}"; fi ;;
    'stderr '*) echo "${step:7}" >&2 ;;
    'exit '*) exit "${step:5}" ;;
  esac
done
while IFS= read -r line; do printf '%s\n' "$line" >> "$dir/stdin.log"; done
exit 0
"#;

/// The ready frame of omp 19 (protocol v1 and v2).
pub const READY_V2: &str = r#"{"type":"ready","protocolVersion":1,"supportedProtocolVersions":[1,2],"maxFrameBytes":1048576,"maxReassembledFrameBytes":67108864,"supportsBashExcludeFromContext":true}"#;

/// Script lines for omp's startup with v2 negotiation, as the client performs it.
pub fn handshake_v2() -> String {
    format!(
        "> {READY_V2}\n< negotiate_protocol neg\n> {{\"type\":\"response\",\"id\":\"{{{{neg}}}}\",\"command\":\"negotiate_protocol\",\"success\":true,\"data\":{{\"protocolVersion\":2}}}}\n"
    )
}

/// An installed fake `omp`.
#[derive(Debug, Clone)]
pub struct FakeOmp {
    dir: PathBuf,
    executable: PathBuf,
}

impl FakeOmp {
    /// Writes the fake into `dir` (which must exist) with `script` as its transcript.
    pub fn install(dir: &Path, script: &str) -> io::Result<Self> {
        let dir = dir.to_path_buf();
        let executable = dir.join("omp");
        let body = SCRIPT.replace("@DIR@", &dir.display().to_string());
        std::fs::write(dir.join("script"), script)?;
        std::fs::write(dir.join("version"), "19.0.0\n")?;
        // Written by a separate process so no fork of this one can hold it
        // open for writing when it runs ("Text file busy").
        loa_exec::write_executable(&executable, body)?;
        Ok(Self { dir, executable })
    }

    /// Sets what `omp --version` prints.
    pub fn set_version(&self, version: &str) -> io::Result<()> {
        std::fs::write(self.dir.join("version"), format!("{version}\n"))
    }

    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The arguments of the last run.
    pub fn args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("args"))
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }

    /// Every frame the last run read from stdin, parsed.
    pub fn received(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir.join("stdin.log"))
            .map(|text| text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect())
            .unwrap_or_default()
    }
}
