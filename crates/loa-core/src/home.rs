//! The user's home directory.

use std::path::PathBuf;

/// The user's home directory: `$HOME` when set and non-empty, else the
/// platform's real home (`std::env::home_dir`, which is the profile directory
/// on Windows, where `$HOME` is normally unset).
///
/// Honouring `$HOME` first keeps Unix behaviour unchanged and lets tests and
/// sandboxes redirect it.
pub fn home_dir() -> Option<PathBuf> {
    resolve(std::env::var_os("HOME"))
}

fn resolve(home_var: Option<std::ffi::OsString>) -> Option<PathBuf> {
    home_var.filter(|h| !h.is_empty()).map(PathBuf::from).or_else(std::env::home_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_variable_wins() {
        assert_eq!(resolve(Some("/somewhere/else".into())), Some(PathBuf::from("/somewhere/else")));
    }

    #[test]
    fn empty_or_unset_falls_back_to_the_platform_home() {
        let platform = std::env::home_dir();
        assert_eq!(resolve(Some("".into())), platform);
        assert_eq!(resolve(None), platform);
    }
}
