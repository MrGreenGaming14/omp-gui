//! The bridge as the MCP server a session of another harness is given.

use std::collections::BTreeMap;
use std::path::Path;

use loa_adapter::{McpServerSpec, McpTransport};

use crate::OmpBridgeConfig;

/// MCP server name harnesses see; their tool names become `mcp__omp__read`.
pub const SERVER_NAME: &str = "omp";

/// A stdio server spec that runs the bridge for `config`: `program` with
/// `prefix` arguments, then the bridge's own (see [`crate::cli::args`]).
/// The app passes its own executable and `["omp-bridge"]`; the standalone
/// binary needs no prefix.
pub fn session_spec(program: &Path, prefix: &[&str], config: &OmpBridgeConfig) -> McpServerSpec {
    let mut args: Vec<String> = prefix.iter().map(|arg| (*arg).to_owned()).collect();
    args.extend(crate::cli::args(config));
    let env = config
        .env
        .iter()
        .map(|(key, value)| (key.to_string_lossy().into_owned(), value.to_string_lossy().into_owned()))
        .collect::<BTreeMap<_, _>>();
    McpServerSpec {
        name: SERVER_NAME.to_owned(),
        transport: McpTransport::Stdio { command: program.to_string_lossy().into_owned(), args, env },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_runs_the_app_subcommand_with_the_bridge_arguments() {
        let config = OmpBridgeConfig::new("/work", "/state/s1");
        let spec = session_spec(Path::new("/usr/bin/loa-app"), &["omp-bridge"], &config);
        assert_eq!(spec.name, "omp");
        let McpTransport::Stdio { command, args, .. } = spec.transport else { panic!("stdio") };
        assert_eq!(command, "/usr/bin/loa-app");
        assert_eq!(args[0], "omp-bridge");
        let parsed = crate::cli::parse(args.into_iter().skip(1)).unwrap();
        assert_eq!(parsed.cwd, config.cwd);
        assert_eq!(parsed.tools, config.tools);
    }
}
