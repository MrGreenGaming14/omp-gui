//! Stdio MCP server offering omp's tools to a harness.
//!
//! ```text
//! loa-omp-bridge --state-dir DIR [--cwd DIR] [--omp PATH] [--tools a,b,...] [--config FILE]...
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use loa_omp_bridge::{OmpBridgeConfig, OmpExecutor, serve_stdio};

const USAGE: &str = "usage: loa-omp-bridge --state-dir DIR [--cwd DIR] [--omp PATH] \
[--tools a,b,...] [--config FILE]...";

fn parse(mut args: impl Iterator<Item = String>) -> Result<OmpBridgeConfig, String> {
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

#[tokio::main]
async fn main() -> ExitCode {
    let config = match parse(std::env::args().skip(1)) {
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
