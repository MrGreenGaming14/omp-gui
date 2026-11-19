//! Opens login URLs in the user's browser through the platform opener.

use std::process::{Command, Stdio};

use crate::login::UrlOpener;

/// [`UrlOpener`] over the platform's own opener: `xdg-open` on Linux and the
/// BSDs, `open` on macOS, `start` on Windows. Returns as soon as the opener
/// started; it is reaped on a background thread.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemUrlOpener;

impl SystemUrlOpener {
    fn command(url: &str) -> Command {
        if cfg!(target_os = "macos") {
            let mut command = Command::new("open");
            command.arg(url);
            command
        } else if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/C", "start", ""]).arg(url);
            command
        } else {
            let mut command = Command::new("xdg-open");
            command.arg(url);
            command
        }
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
}
