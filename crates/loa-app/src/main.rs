// No console window behind the desktop app. `omp-bridge` still speaks over
// the stdio pipes its parent hands it.
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

use iced::Font;
use loa_app::app::App;

fn main() -> ExitCode {
    // `loa omp-bridge …` serves omp's tools over stdio MCP to another
    // harness; sessions launch the app binary this way.
    if std::env::args().nth(1).as_deref() == Some("omp-bridge") {
        return match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime.block_on(loa_omp_bridge::cli::run(std::env::args().skip(2))),
            Err(error) => {
                eprintln!("omp-bridge: can't start the async runtime: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let result = iced::application(App::new, App::update, App::view)
        .title("Loa")
        .theme(App::theme)
        .subscription(App::subscription)
        .default_font(Font::with_name("Noto Sans"))
        .window_size((1440.0, 900.0))
        .run();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
