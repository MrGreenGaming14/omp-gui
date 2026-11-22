//! OpenSSH config: the `Host` alias scan (following `Include`) and `ssh -G` resolution.

use crate::exec::NoWindow;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What `ssh -G` reports for one host.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolvedHost {
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub proxy_jump: Option<String>,
    pub identity_agent: Option<String>,
}

/// Concrete (no wildcard, not negated) `Host` aliases of `config` and its includes, lowercased, in file order.
pub fn aliases(config: &Path) -> Vec<String> {
    let mut out = Vec::new();
    scan(config, config.parent().unwrap_or(Path::new(".")), 0, &mut out);
    out
}

fn scan(file: &Path, base: &Path, depth: usize, out: &mut Vec<String>) {
    let Ok(text) = std::fs::read_to_string(file) else { return };
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (key, rest) = line.split_once(|c: char| c.is_whitespace() || c == '=').unwrap_or((line, ""));
        let words = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=').split_whitespace().map(|w| w.trim_matches('"'));
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                for word in words.filter(|w| !w.starts_with('!') && !w.contains(['*', '?'])).map(str::to_ascii_lowercase) {
                    if !out.contains(&word) {
                        out.push(word);
                    }
                }
            }
            "include" if depth < 16 => {
                for pattern in words {
                    let path = match pattern.strip_prefix("~/") {
                        Some(rest) => crate::fs::home().unwrap_or_default().join(rest),
                        None => base.join(pattern),
                    };
                    glob(&path).iter().for_each(|p| scan(p, base, depth + 1, out));
                }
            }
            _ => {}
        }
    }
}

/// Expands `*` and `?` in any component of `pattern`, sorted; matches nothing when nothing exists.
fn glob(pattern: &Path) -> Vec<PathBuf> {
    let mut current = vec![PathBuf::new()];
    for part in pattern.components().map(|c| c.as_os_str().to_string_lossy().into_owned()) {
        if !part.contains(['*', '?']) {
            current.iter_mut().for_each(|p| p.push(&part));
            continue;
        }
        current = current
            .iter()
            .flat_map(|dir| {
                let names = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned());
                let mut names: Vec<String> = names.filter(|n| !n.starts_with('.') && wildcard_match(&part, n)).collect();
                names.sort();
                names.into_iter().map(|n| dir.join(n)).collect::<Vec<_>>()
            })
            .collect();
    }
    current.into_iter().filter(|p| p.is_file()).collect()
}

/// `*` and `?` glob match, as OpenSSH's `match_pattern`.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti, mut star) = (0, 0, None);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            (pi, ti) = (pi + 1, ti + 1);
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            (pi, ti, star) = (sp + 1, st + 1, Some((sp, st + 1)));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

/// Resolves `alias` with the `ssh` on `PATH` (`ssh -G alias`).
pub fn resolve(alias: &str) -> io::Result<ResolvedHost> {
    resolve_with(Path::new("ssh"), &[], alias)
}

/// Runs `program -G args.. -- host` and parses its output; a missing program is a clear `NotFound`.
pub fn resolve_with(program: &Path, args: &[String], host: &str) -> io::Result<ResolvedHost> {
    let output = Command::new(program).no_window().arg("-G").args(args).arg("--").arg(host).output().map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => io::Error::new(
            e.kind(),
            format!("no OpenSSH client found (`{}` is not installed or not on PATH); install OpenSSH to use Remote hosts", program.display()),
        ),
        _ => e,
    })?;
    if !output.status.success() {
        return Err(io::Error::other(format!("ssh -G {host}: {}", String::from_utf8_lossy(&output.stderr).trim())));
    }
    let resolved = parse(&String::from_utf8_lossy(&output.stdout));
    if resolved.hostname.is_empty() || resolved.port == 0 {
        return Err(io::Error::other(format!("ssh -G {host} printed no hostname or port")));
    }
    Ok(resolved)
}

/// Parses `ssh -G` output (`key value` lines); `none` means unset.
pub fn parse(text: &str) -> ResolvedHost {
    let mut out = ResolvedHost::default();
    for (key, value) in text.lines().filter_map(|l| l.trim().split_once(' ')) {
        let set = (!value.eq_ignore_ascii_case("none")).then(|| value.to_owned());
        match key {
            "hostname" => out.hostname = value.to_owned(),
            "user" => out.user = value.to_owned(),
            "port" => out.port = value.parse().unwrap_or(0),
            "proxyjump" => out.proxy_jump = set,
            "identityagent" => out.identity_agent = set,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_follow_includes_and_ssh_g_output_parses() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("conf.d")).unwrap();
        std::fs::write(dir.path().join("conf.d/a.conf"), "Host inc-b inc-a\n").unwrap();
        let config = dir.path().join("config");
        std::fs::write(&config, "Host Db web*  !bad\n  HostName x\nInclude conf.d/*.conf\nMatch host y\nHost=db last\n").unwrap();
        assert_eq!(aliases(&config), ["db", "inc-b", "inc-a", "last"]);
        assert!(aliases(&dir.path().join("missing")).is_empty());
        for (pattern, text, want) in [("*.lan", "db.lan", true), ("d?", "db", true), ("*.lan", "db.lan.x", false), ("a*b*c", "aXbYc", true)] {
            assert_eq!(wildcard_match(pattern, text), want, "{pattern} {text}");
        }
        let got = parse("user me\nhostname 10.0.0.1\nport 2222\nproxyjump j1,j2\nidentityagent none\nforwardagent no\n");
        let want = ResolvedHost { hostname: "10.0.0.1".into(), user: "me".into(), port: 2222, proxy_jump: Some("j1,j2".into()), identity_agent: None };
        assert_eq!(got, want);
        let missing = resolve_with(&dir.path().join("no-ssh"), &[], "h").unwrap_err();
        assert!(missing.to_string().contains("no OpenSSH client"), "{missing}");
    }
}
