//! CI input-to-pixels gate, using the synthetic surface on headless machines.

use loa_surface_bench::{bench::{self, BenchConfig}, report::LatencyReport, synthetic::{SyntheticConfig, SyntheticSurface}};
use std::{env, process::ExitCode, time::Duration};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("latency-gate: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> loa_surface_bench::Result<bool> {
    let mut config = SyntheticConfig::default();
    let mut options = BenchConfig::default();
    let mut budget_ms = 50u64;
    let mut json = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "--json" => { json = true; continue; }
            "--help" | "-h" => {
                println!("Usage: latency-gate [--iterations N] [--budget-ms N] [--render-delay-ms N] [--encode-delay-ms N] [--json]");
                return Ok(true);
            }
            "--iterations" | "--budget-ms" | "--render-delay-ms" | "--encode-delay-ms" => args.next().ok_or_else(|| loa_surface_bench::BenchError::Invalid(format!("missing value for {arg}")))?,
            _ => return Err(loa_surface_bench::BenchError::Invalid(format!("unknown option {arg}"))),
        };
        let parsed: u64 = value.parse().map_err(|_| loa_surface_bench::BenchError::Invalid(format!("{arg} must be a nonnegative integer")))?;
        match arg.as_str() {
            "--iterations" => options.iterations = usize::try_from(parsed).map_err(|_| loa_surface_bench::BenchError::Invalid("iteration count does not fit usize".into()))?,
            "--budget-ms" => budget_ms = parsed,
            "--render-delay-ms" => config.render_delay = Duration::from_millis(parsed),
            "--encode-delay-ms" => config.encode_delay = Duration::from_millis(parsed),
            _ => unreachable!("only parsed options reach this match"),
        }
    }
    if options.iterations == 0 {
        return Err(loa_surface_bench::BenchError::Invalid("at least one iteration is required".into()));
    }
    let budget = Duration::from_millis(budget_ms);
    let surface = SyntheticSurface::new(config)?;
    let result = bench::run(&surface, &options, Some(&surface)).await?;
    let report = LatencyReport::from_result(&result, budget)?;
    print!("{}", report.table());
    if json {
        println!("{}", report.json()?);
    }
    if !report.passed {
        eprintln!("latency-gate: p95 {:.2} ms exceeds {:.2} ms budget", report.input_to_pixels.p95_us as f64 / 1000.0, report.budget_ms);
    }
    Ok(report.passed)
}
