use std::process::ExitCode;

use iced::Font;
use loa_app::app::App;

fn main() -> ExitCode {
    // `loa omp-bridge …` serves omp's tools over stdio MCP to another
    // harness; sessions launch the app binary this way.
    if std::env::args().nth(1).as_deref() == Some("omp-bridge") {
        return match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime.block_on(loa_mcp::bridge::run_cli(std::env::args().skip(2))),
            Err(error) => {
                eprintln!("omp-bridge: can't start the async runtime: {error}");
                ExitCode::FAILURE
            }
        };
    }
    // A daemon, so panel tabs and subagents can open in windows of their own.
    let result = iced::daemon(App::boot, App::update, App::view_window)
        .title(App::title)
        .theme(|app: &App, _| app.theme())
        .subscription(App::subscription)
        .default_font(Font::with_name("Noto Sans"))
        .run();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
