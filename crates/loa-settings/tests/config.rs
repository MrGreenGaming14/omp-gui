use loa_settings::app::{Config, ConfigError, ConfigStore, Relay, ThemeRef};
use loa_contracts::harness::{McpTransport, SessionConfig};
use loa_core::DecisionKind;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const FULL: &str = include_str!("fixtures/full.toml");

fn store_with(text: &str) -> (tempfile::TempDir, ConfigStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(&dir.path().join("nested"));
    fs::create_dir_all(dir.path().join("nested")).unwrap();
    fs::write(store.path(), text).unwrap();
    (dir, store)
}

fn load_err(text: &str) -> ConfigError {
    store_with(text).1.load().expect_err("config should be rejected")
}

/// Reads the file through a new store, so the answer comes from disk and
/// not from the store's read cache.
fn load_from_disk(store: &ConfigStore) -> Config {
    ConfigStore::new(store.path()).load().unwrap()
}

#[test]
fn full_config_loads_into_typed_settings_and_round_trips() {
    let (_dir, store) = store_with(FULL);
    let c = store.load().unwrap();
    assert_eq!(c.appearance.theme, ThemeRef("dark".into()));
    assert!(c.project_tabs);
    assert_eq!(c.harness.executable("omp"), Some(Path::new("/opt/omp/bin/omp")));
    assert_eq!((c.network.port, &c.network.relay), (4242, &Relay::Url("https://relay.example.test".into())));
    assert!(!c.network.discovery.mdns);
    assert_eq!(c.updater.check_interval(), Duration::from_secs(3600));
    assert_eq!((c.updater.pins["omp"].as_str(), c.updater.github_token.as_deref()), ("19.0.0", Some("ghp_test")));
    let approval = c.unattended.for_kind(DecisionKind::Approval).unwrap();
    assert_eq!((approval.timeout_secs, approval.answer.as_str()), (120, "deny"));
    assert!(c.unattended.for_kind(DecisionKind::Question).is_none());
    assert!(!c.surfaces.video);
    assert!(matches!(&c.mcp_servers["files"].transport, McpTransport::Stdio { args, env, .. } if args.len() == 2 && env["LOG"] == "debug"));
    assert!(matches!(&c.mcp_servers["search"].transport, McpTransport::Http { url, .. } if url.ends_with("/sse")));
    assert!(!c.mcp_servers["old"].enabled);
    let host = &c.remote_hosts["build"];
    assert_eq!((host.port, host.read_only, host.commands, host.auto_approve), (Some(2222), false, true, false));
    let project = c.project("loa");
    assert_eq!(project.token_budget, 12000);
    assert!(project.disabled_tools.contains("bash") && !project.mcp_tool_enabled("files", "delete"));
    assert!(project.mcp_tool_enabled("files", "read"));

    store.save(&c).unwrap();
    assert_eq!(load_from_disk(&store), c);
    let raw: toml::Table = fs::read_to_string(store.path()).unwrap().parse().unwrap();
    assert_eq!(raw, FULL.parse::<toml::Table>().unwrap(), "every key survives a round trip unchanged");
}

#[test]
fn missing_file_and_sections_get_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(dir.path());
    assert_eq!(store.load().unwrap(), Config::default());
    assert!(!store.path().exists());

    let (_dir, store) = store_with("version = 1\n[remote_hosts.box]\nhost = \"box\"\n");
    let c = store.load().unwrap();
    assert_eq!((c.network.port, &c.network.relay, c.harness.default.as_str()), (31415, &Relay::Default, "omp"));
    assert_eq!(c.appearance.theme.as_str(), ThemeRef::SYSTEM);
    assert!(c.surfaces.video && c.network.discovery.tailscale);
    let host = &c.remote_hosts["box"];
    assert!(host.read_only && !host.commands && !host.auto_approve);
    assert_eq!(c.project("never-configured").digest_role, "smol");
}

#[test]
fn unknown_keys_survive_load_edit_save() {
    let text = include_str!("fixtures/unknown.toml");
    let (_dir, store) = store_with(text);
    store
        .edit(|c| {
            c.appearance.theme = ThemeRef("light".into());
            c.mcp_servers.remove("drop");
        })
        .unwrap();
    let raw: toml::Table = fs::read_to_string(store.path()).unwrap().parse().unwrap();
    assert_eq!(raw["theme"].as_str(), Some("light"));
    assert_eq!(raw["future_flag"].as_bool(), Some(true));
    assert_eq!(raw["future_section"]["level"].as_integer(), Some(3));
    assert!(matches!(raw["future_section"]["when"], toml::Value::Datetime(_)));
    assert_eq!(raw["network"]["x-mtu"].as_integer(), Some(1200));
    assert_eq!(raw["unattended"]["plan"]["escalate_to"].as_str(), Some("owner"));
    assert_eq!(raw["mcp_servers"]["keep"]["x-color"].as_str(), Some("red"));
    assert!(raw["mcp_servers"].get("drop").is_none(), "deleted server came back: {raw}");
    assert_eq!(raw["projects"]["p"]["future_digest_mode"].as_str(), Some("dense"));

    let again = load_from_disk(&store);
    store.save(&again).unwrap();
    assert_eq!(raw, fs::read_to_string(store.path()).unwrap().parse::<toml::Table>().unwrap());
}

#[test]
fn invalid_values_name_the_offending_key() {
    let cases: &[(&str, &str)] = &[
        ("[network]\nport = \"high\"", "network.port"),
        ("[network]\nport = 0", "network.port"),
        ("[network]\nrelay = \"ftp://relay\"", "network.relay"),
        ("theme = 5", "theme"),
        ("[harness]\ndefault = \"\"", "harness.default"),
        ("[harness]\nexecutables = { omp = \"\" }", "harness.executables.omp"),
        ("[mcp_servers.s]\ncommand = \"a\"\nurl = \"https://b\"", "mcp_servers.s"),
        ("[mcp_servers.s]\nenabled = true", "mcp_servers.s"),
        ("[mcp_servers.s]\ncommand = \" \"", "mcp_servers.s.command"),
        ("[mcp_servers.s]\nurl = \"localhost:80\"", "mcp_servers.s.url"),
        ("[mcp_servers.s]\ncommand = \"a\"\nargs = 5", "mcp_servers.s.args"),
        ("[remote_hosts.h]\nuser = \"me\"", "remote_hosts.h"),
        ("[remote_hosts.h]\nhost = \"h\"\nport = 0", "remote_hosts.h.port"),
        ("[remote_hosts.h]\nhost = \"h\"\nallowed_paths = [\"srv\"]", "remote_hosts.h.allowed_paths"),
        ("[unattended.approval]\ntimeout_secs = 5\nanswer = \"maybe\"", "unattended.approval.answer"),
        ("[unattended.plan]\ntimeout_secs = 5\nanswer = \"edit\"", "unattended.plan.answer"),
        ("[unattended.form]\ntimeout_secs = 5\nanswer = \"[1]\"", "unattended.form.answer"),
        ("[unattended.question]\ntimeout_secs = 0\nanswer = \"yes\"", "unattended.question.timeout_secs"),
        ("[projects.p]\ntoken_budget = 0", "projects.p.token_budget"),
        ("[updater]\ncheck_interval_secs = 0", "updater.check_interval_secs"),
        ("version = \"one\"", "version"),
        ("version = -1", "version"),
    ];
    for (snippet, key) in cases {
        let text = if snippet.starts_with("version") { format!("{snippet}\n") } else { format!("version = 1\n{snippet}\n") };
        let err = load_err(&text);
        let message = err.to_string();
        let ConfigError::Invalid { key: found, .. } = err else { panic!("expected an invalid key for {text}, got {err}") };
        assert_eq!(found, *key, "for config:\n{text}");
        assert!(message.contains(key), "message {message:?} should name {key}");
    }
}

#[test]
fn saved_files_hold_only_what_is_set() {
    let cases: &[(&str, &str, bool)] = &[
        ("", "version = 1", true),
        ("", "theme = \"system\"", true),
        ("", "project_tabs", false),
        ("", "github_token", false),
        ("", "trusted_keys", false),
        ("", "pinned", false),
        ("project_tabs = true", "project_tabs = true", true),
        ("theme = \"light\"", "theme = \"light\"", true),
        ("[theme]\nname = \"dark\"", "theme = \"dark\"", true),
        ("[theme]\nname = \"nord\"\naccent = \"#88c0d0\"", "accent = \"#88c0d0\"", true),
        ("[network]\nrelay = \"off\"", "relay = \"off\"", true),
        ("[updater]\ntrusted_keys = [\"ab\"]", "trusted_keys = [\"ab\"]", true),
        ("[surfaces]\nbrowser_capture = \"virtual-display\"", "browser_capture = \"virtual-display\"", true),
        ("[mcp_servers.s]\ncommand = \"a\"", "args", false),
    ];
    for (snippet, needle, present) in cases {
        let (_dir, store) = store_with(&format!("version = 1\n{snippet}\n"));
        let c = store.load().unwrap();
        store.save(&c).unwrap();
        let saved = fs::read_to_string(store.path()).unwrap();
        assert_eq!(saved.contains(needle), *present, "{needle:?} after loading {snippet:?}:\n{saved}");
        assert_eq!(load_from_disk(&store), c);
    }
}

#[test]
fn refused_files_are_left_alone() {
    assert!(matches!(load_err("theme = \"dark\"\n"), ConfigError::MissingVersion));
    assert!(matches!(load_err("version = 1\n[network\n"), ConfigError::Parse(_)));
    assert!(matches!(load_err("version = 99\n"), ConfigError::TooNew { found: 99, .. }));
    assert!(matches!(load_err("version = 0\n"), ConfigError::MissingMigration { from: 0 }));

    let (_dir, store) = store_with(FULL);
    let err = store.edit(|c| c.network.port = 0).unwrap_err();
    assert!(matches!(err, ConfigError::Invalid { key, .. } if key == "network.port"));
    assert_eq!(fs::read_to_string(store.path()).unwrap(), FULL);
}

#[test]
fn load_sees_external_edit() {
    let (_dir, store) = store_with(FULL);
    let c = store.load().unwrap();
    store.save(&c).unwrap();
    assert_eq!(store.load().unwrap(), c);

    // A longer file, so the length alone shows the edit.
    let saved = fs::read_to_string(store.path()).unwrap();
    let edited = format!("version = 1\ntheme = \"light\"\n# {}\n", "x".repeat(saved.len()));
    fs::write(store.path(), edited).unwrap();
    assert_eq!(store.load().unwrap().appearance.theme, ThemeRef("light".into()));
}

#[test]
fn edit_saves_only_changes_and_watch_counts_saves() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(&dir.path().join("new"));
    let saves = store.clone().watch();
    assert!(!store.edit(|c| c.project_tabs).unwrap());
    assert!(!store.path().exists(), "an edit that changes nothing doesn't save");
    store.edit(|c| c.pinned.insert("s1".into())).unwrap();
    assert_eq!(*saves.borrow(), 1);
    let names: Vec<PathBuf> = fs::read_dir(store.path().parent().unwrap()).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(names, [store.path()], "no temp files are left");
}

#[test]
fn apply_to_session_adds_enabled_servers_toggles_and_executable() {
    let (_dir, store) = store_with(FULL);
    let c = store.load().unwrap();
    let mut session = SessionConfig { project: Some("loa".into()), ..SessionConfig::default() };
    session.disabled_tools.push("task".into());
    c.apply_to_session("omp", &mut session);
    let names: Vec<&str> = session.mcp_servers.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["files", "search"], "disabled servers must not reach the harness");
    assert_eq!(session.disabled_tools, ["task", "bash"]);
    assert_eq!(session.executable.as_deref(), Some(Path::new("/opt/omp/bin/omp")));

    let staged = PathBuf::from("/staging/omp");
    let mut session = SessionConfig { executable: Some(staged.clone()), ..SessionConfig::default() };
    c.apply_to_session("omp", &mut session);
    assert_eq!(session.executable, Some(staged), "the updater's staged executable wins");
}

/// Writes the golden TOML fixtures for the C++ port into `$LOA_GOLDEN_OUT/settings`.
#[test]
#[ignore = "writes golden files; run with LOA_GOLDEN_OUT set"]
fn write_golden_toml() {
    let out = PathBuf::from(std::env::var("LOA_GOLDEN_OUT").unwrap()).join("settings");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("config.toml"), FULL).unwrap();
    fs::write(out.join("unknown.toml"), include_str!("fixtures/unknown.toml")).unwrap();
}
