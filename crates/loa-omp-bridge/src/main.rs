//! Stdio MCP server offering omp's tools to a harness. See [`loa_omp_bridge::cli`].

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    loa_omp_bridge::cli::run(std::env::args().skip(1)).await
}
