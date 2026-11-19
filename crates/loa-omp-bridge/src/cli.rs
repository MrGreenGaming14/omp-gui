//! Command line of the stdio bridge, shared by the `loa-omp-bridge` binary
//! and the app's `omp-bridge` subcommand.
//!
//! ```text
//! --state-dir DIR [--cwd DIR] [--omp PATH] [--tools a,b,...] [--config FILE]...
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use crate::{OmpBridgeConfig, OmpExecutor, serve_stdio};

pub const USAGE: &str = "usage: loa-omp-bridge --state-dir DIR [--cwd DIR] [--omp PATH] \
[--tools a,b,...] [--config FILE]...";

/// Reads the arguments (without the program name or subcommand).
pub fn parse(mut args: impl Iterator<Item = String>) -> Result<OmpBridgeConfig, String> {
    let mut state_dir = None;
    let mut cwd = None;
    let mut program = None;
    let mut tools = None;
    let mut overlays = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--state-dir" => state_dir = Some(PathBuf::from(value()?)),
            "--cwd" => cwd = Some(PathBuf::from(value()?)),
            "--omp" => program = Some(PathBuf::from(value()?)),
            "--tools" => {
                tools = Some(value()?.split(',').map(str::to_owned).collect::<Vec<_>>());
            }
            "--config" => overlays.push(PathBuf::from(value()?)),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    let state_dir = state_dir.ok_or("--state-dir is required")?;
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir().map_err(|error| error.to_string())?,
    };
    let mut config = OmpBridgeConfig::new(cwd, state_dir);
    if let Some(program) = program {
        config.program = program;
    }
    if let Some(tools) = tools {
        config.tools = tools;
    }
    config.config_overlays = overlays;
    Ok(config)
}

/// The arguments [`parse`] reads back into `config`.
pub fn args(config: &OmpBridgeConfig) -> Vec<String> {
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let mut out = vec![
        "--state-dir".to_owned(),
        path(&config.state_dir),
        "--cwd".to_owned(),
        path(&config.cwd),
        "--omp".to_owned(),
        path(&config.program),
        "--tools".to_owned(),
        config.tools.join(","),
    ];
    for overlay in &config.config_overlays {
        out.push("--config".to_owned());
        out.push(path(overlay));
    }
    out
}

/// Runs the bridge on stdin and stdout until the client leaves.
pub async fn run(args: impl Iterator<Item = String>) -> ExitCode {
    let config = match parse(args) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match OmpExecutor::start(config).await {
        Ok(executor) => serve_stdio(Arc::new(executor)).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("loa-omp-bridge: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_round_trip() {
        let mut config = OmpBridgeConfig::new("/work", "/state");
        config.program = "/opt/omp".into();
        config.tools = vec!["read".into(), "edit".into()];
        config.config_overlays = vec!["/home/me/.omp/agent/config.yml".into()];
        let parsed = parse(args(&config).into_iter()).unwrap();
        assert_eq!(parsed.cwd, config.cwd);
        assert_eq!(parsed.state_dir, config.state_dir);
        assert_eq!(parsed.program, config.program);
        assert_eq!(parsed.tools, config.tools);
        assert_eq!(parsed.config_overlays, config.config_overlays);
        assert!(parse(["--cwd".to_owned()].into_iter()).is_err());
        assert!(parse(["--bogus".to_owned()].into_iter()).is_err());
    }
}
