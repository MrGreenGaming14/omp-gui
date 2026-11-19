//! Measures input to frame latency of the Chrome screencast on this machine.
//!
//! `cargo run --release -p loa-surface-browser --example screencast_latency -- [samples] [--no-sandbox] [--chrome PATH]`

use loa_surface_browser::{BrowserConfig, BrowserServer, bench};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut samples = 200;
    let mut sandbox = true;
    let mut chrome = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-sandbox" => sandbox = false,
            "--chrome" => chrome = args.next(),
            n => samples = n.parse()?,
        }
    }
    let mut config = match chrome {
        Some(path) => BrowserConfig::new(path),
        None => BrowserConfig::discover()?,
    };
    config.sandbox = sandbox;
    println!("chrome: {}", config.executable.display());
    println!("surface: {}x{}, jpeg quality {}", config.width, config.height, config.jpeg_quality);
    let server = BrowserServer::launch(config).await?;
    let report = bench::click_latency(&server, samples).await;
    server.shutdown().await?;
    println!("{}", report?);
    Ok(())
}
