//! Opens login URLs in the user's browser through the platform opener.

use std::process::{Command, Stdio};

use loa_contracts::harness::UrlOpener;

/// [`UrlOpener`] over the platform's own opener: `xdg-open` on Linux and the
/// BSDs, `open` on macOS, `rundll32 url.dll,FileProtocolHandler` on Windows
/// (no shell, so `&` in the URL is not interpreted). Returns as soon as the
/// opener started; it is reaped on a background thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemUrlOpener;

/// Platform families with distinct opener commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Windows,
    Mac,
    Other,
}

impl Os {
    fn current() -> Self {
        if cfg!(target_os = "macos") {
            Os::Mac
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Other
        }
    }
}

/// Program and arguments that open `url`; the URL is always one argument.
fn command_for(os: Os, url: &str) -> (&'static str, Vec<String>) {
    match os {
        Os::Mac => ("open", vec![url.to_string()]),
        Os::Windows => ("rundll32.exe", vec!["url.dll,FileProtocolHandler".to_string(), url.to_string()]),
        Os::Other => ("xdg-open", vec![url.to_string()]),
    }
}

impl SystemUrlOpener {
    fn command(url: &str) -> Command {
        let (program, args) = command_for(Os::current(), url);
        let mut command = Command::new(program);
        command.args(args);
        command
    }
}

impl UrlOpener for SystemUrlOpener {
    fn open_url(&self, url: &str) -> Result<(), String> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(format!("refusing to open a non-web URL: {url}"));
        }
        let mut command = Self::command(url);
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let mut child = command.spawn().map_err(|e| format!("cannot start the browser opener: {e}"))?;
        std::thread::spawn(move || child.wait());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_urls_are_opened() {
        assert!(SystemUrlOpener.open_url("file:///etc/passwd").is_err());
        assert!(SystemUrlOpener.open_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn windows_uses_rundll32_with_one_url_argument() {
        let url = "https://example.com/a?x=1&y=2";
        let (program, args) = command_for(Os::Windows, url);
        assert_eq!(program, "rundll32.exe");
        assert_eq!(args, ["url.dll,FileProtocolHandler", url]);
        assert!(!args.iter().any(|a| a.eq_ignore_ascii_case("cmd") || a == "/C" || a == "start"));
    }

    #[test]
    fn unix_openers_are_unchanged() {
        let url = "https://example.com/a?x=1&y=2";
        assert_eq!(command_for(Os::Mac, url), ("open", vec![url.to_string()]));
        assert_eq!(command_for(Os::Other, url), ("xdg-open", vec![url.to_string()]));
    }
}
