use loa_adapter::{McpTransport, SessionConfig};
use loa_config::{
    Config, ConfigError, ConfigStore, NativeConfig, NativeConfigError, NativeConfigWriter, Relay,
    Theme, config_path,
};
use loa_core::DecisionKind;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const FULL: &str = r#"
version = 1
theme = "dark"

[harness]
default = "claude"
executables = { omp = "/opt/omp/bin/omp" }

[network]
port = 4242
relay = "https://relay.example.test"
discovery = { mdns = false, tailscale = true }

[updater]
check_interval_secs = 3600
pins = { omp = "19.0.0" }

[unattended.approval]
timeout_secs = 120
answer = "deny"
reason = "nobody was around"

[unattended.form]
timeout_secs = 30
answer = '{"name": "x"}'

[mcp_servers.files]
command = "mcp-files"
args = ["--root", "/srv"]
env = { LOG = "debug" }

[mcp_servers.search]
url = "https://mcp.example.test/sse"
headers = { "X-Team" = "a" }

[mcp_servers.old]
enabled = false
command = "old-server"

[remote_hosts.build]
host = "build.example.test"
user = "ci"
port = 2222
proxy_jump = "bastion"
allowed_paths = ["/srv/app"]
read_only = false
commands = true

[projects.loa]
instructions = "Use iced."
pinned_files = ["docs/design/README.md"]
digest_role = "fast"
token_budget = 12000
disabled_tools = ["bash"]
disabled_mcp_tools = { files = ["delete"] }
"#;

fn store_with(text: &str) -> (tempfile::TempDir, ConfigStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(dir.path());
    fs::write(store.path(), text).unwrap();
    (dir, store)
}

fn load_err(text: &str) -> ConfigError {
    let (_dir, store) = store_with(text);
    store.load().expect_err("config should be rejected")
}

fn invalid_key(err: ConfigError) -> String {
    match err {
        ConfigError::Invalid { key, .. } => key,
        other => panic!("expected an invalid key error, got {other}"),
    }
}

#[test]
fn full_config_loads_into_typed_settings_and_round_trips() {
    let (_dir, store) = store_with(FULL);
    let loaded = store.load().unwrap();
    assert_eq!(loaded.migrated_from, None);
    let c = loaded.config;

    assert_eq!(c.theme, Theme::Dark);
    assert_eq!(c.harness.default, "claude");
    assert_eq!(c.harness.executable("omp"), Some(Path::new("/opt/omp/bin/omp")));
    assert_eq!(c.network.port, 4242);
    assert_eq!(c.network.relay, Relay::Url("https://relay.example.test".into()));
    assert!(!c.network.discovery.mdns);
    assert_eq!(c.updater.check_interval(), Duration::from_secs(3600));
    assert_eq!(c.updater.pin("omp"), Some("19.0.0"));

    let approval = c.unattended.for_kind(DecisionKind::Approval).unwrap();
    assert_eq!((approval.timeout(), approval.answer.as_str()), (Duration::from_secs(120), "deny"));
    assert!(c.unattended.for_kind(DecisionKind::Question).is_none());

    match &c.mcp_servers["files"].transport {
        McpTransport::Stdio { command, args, env } => {
            assert_eq!(command, "mcp-files");
            assert_eq!(args, &["--root", "/srv"]);
            assert_eq!(env["LOG"], "debug");
        }
        other => panic!("files should be stdio, got {other:?}"),
    }
    assert!(matches!(&c.mcp_servers["search"].transport, McpTransport::Http { url, .. } if url.ends_with("/sse")));
    assert!(!c.mcp_servers["old"].enabled);

    let host = &c.remote_hosts["build"];
    assert_eq!((host.port, host.read_only, host.commands, host.auto_approve), (Some(2222), false, true, false));

    let project = c.project("loa");
    assert_eq!(project.token_budget, 12000);
    assert!(!project.tool_enabled("bash"));
    assert!(!project.mcp_tool_enabled("files", "delete"));
    assert!(project.mcp_tool_enabled("files", "read"));

    store.save(&c).unwrap();
    assert_eq!(store.load().unwrap().config, c);
}

#[test]
fn missing_sections_and_projects_get_design_defaults() {
    let (_dir, store) = store_with("version = 1\n[remote_hosts.box]\nhost = \"box\"\n");
    let c = store.load().unwrap().config;
    assert_eq!(c.network.port, 31415);
    assert_eq!(c.network.relay, Relay::Default);
    assert_eq!(c.harness.default, "omp");
    let host = &c.remote_hosts["box"];
    assert!(host.read_only && !host.commands && !host.auto_approve);
    let project = c.project("never-configured");
    assert_eq!(project.digest_role, "smol");
    assert!(project.tool_enabled("bash"));
}

#[test]
fn missing_file_loads_defaults_without_creating_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(&dir.path().join("loa"));
    let loaded = store.load().unwrap();
    assert_eq!(loaded.config, Config::default());
    assert!(!store.path().exists());
}

#[test]
fn unknown_keys_survive_load_edit_save() {
    let text = r#"
version = 1
future_flag = true

[future_section]
level = 3
when = 2026-01-02T03:04:05Z

[network]
port = 31415
x-mtu = 1200

[unattended.plan]
timeout_secs = 60
answer = "reject"
escalate_to = "owner"

[mcp_servers.keep]
command = "keep"
x-color = "red"

[mcp_servers.drop]
command = "drop"
x-color = "blue"

[projects.p]
future_digest_mode = "dense"
"#;
    let (_dir, store) = store_with(text);
    let mut c = store.load().unwrap().config;
    c.theme = Theme::Light;
    c.mcp_servers.remove("drop");
    store.save(&c).unwrap();

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

    // A second cycle keeps them too.
    let again = store.load().unwrap().config;
    store.save(&again).unwrap();
    let raw2: toml::Table = fs::read_to_string(store.path()).unwrap().parse().unwrap();
    assert_eq!(raw, raw2);
}

#[test]
fn invalid_values_name_the_offending_key() {
    let cases: &[(&str, &str)] = &[
        ("version = 1\n[network]\nport = \"high\"\n", "network.port"),
        ("version = 1\n[network]\nport = 0\n", "network.port"),
        ("version = 1\n[network]\nrelay = \"ftp://relay\"\n", "network.relay"),
        ("version = 1\ntheme = \"blue\"\n", "theme"),
        ("version = 1\n[harness]\ndefault = \"\"\n", "harness.default"),
        ("version = 1\n[mcp_servers.s]\ncommand = \"a\"\nurl = \"https://b\"\n", "mcp_servers.s"),
        ("version = 1\n[mcp_servers.s]\nenabled = true\n", "mcp_servers.s"),
        ("version = 1\n[mcp_servers.s]\nurl = \"localhost:80\"\n", "mcp_servers.s"),
        ("version = 1\n[mcp_servers.s]\ncommand = \"a\"\nargs = 5\n", "mcp_servers.s.args"),
        ("version = 1\n[remote_hosts.h]\nuser = \"me\"\n", "remote_hosts.h"),
        ("version = 1\n[remote_hosts.h]\nhost = \"h\"\nallowed_paths = [\"srv\"]\n", "remote_hosts.h.allowed_paths"),
        ("version = 1\n[remote_hosts.h]\nhost = \"h\"\nallowed_paths = [\"/srv/../etc\"]\n", "remote_hosts.h.allowed_paths"),
        ("version = 1\n[unattended.approval]\ntimeout_secs = 5\nanswer = \"maybe\"\n", "unattended.approval.answer"),
        ("version = 1\n[unattended.plan]\ntimeout_secs = 5\nanswer = \"edit\"\n", "unattended.plan.answer"),
        ("version = 1\n[unattended.form]\ntimeout_secs = 5\nanswer = \"[1]\"\n", "unattended.form.answer"),
        ("version = 1\n[unattended.question]\ntimeout_secs = 0\nanswer = \"yes\"\n", "unattended.question.timeout_secs"),
        ("version = 1\n[projects.p]\ntoken_budget = 0\n", "projects.p.token_budget"),
        ("version = 1\n[updater]\ncheck_interval_secs = 0\n", "updater.check_interval_secs"),
        ("version = \"one\"\n", "version"),
        ("version = -1\n", "version"),
    ];
    for (text, key) in cases {
        let err = load_err(text);
        let message = err.to_string();
        assert_eq!(invalid_key(err), *key, "for config:\n{text}");
        assert!(message.contains(key), "message {message:?} should name {key}");
    }
}

#[test]
fn file_without_version_or_with_bad_toml_is_refused() {
    assert!(matches!(load_err("theme = \"dark\"\n"), ConfigError::MissingVersion));
    assert!(matches!(load_err("version = 1\n[network\n"), ConfigError::Parse(_)));
}

#[test]
fn save_creates_the_directory_and_leaves_no_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("nested").join("loa");
    let store = ConfigStore::in_dir(&config_dir);
    let mut c = Config::default();
    c.theme = Theme::Dark;
    store.save(&c).unwrap();
    let names: Vec<PathBuf> = fs::read_dir(&config_dir).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(names, vec![config_path(&config_dir)]);
    assert_eq!(store.load().unwrap().config.theme, Theme::Dark);
}

#[test]
fn saving_an_invalid_config_keeps_the_old_file() {
    let (_dir, store) = store_with(FULL);
    let mut c = store.load().unwrap().config;
    c.network.port = 0;
    let err = store.save(&c).unwrap_err();
    assert_eq!(invalid_key(err), "network.port");
    assert_eq!(fs::read_to_string(store.path()).unwrap(), FULL);
}

#[test]
fn remote_host_limits_stay_inside_allowed_trees() {
    let (_dir, store) = store_with(
        "version = 1\n[remote_hosts.h]\nhost = \"h\"\nallowed_paths = [\"/srv/app\", \"/tmp/\"]\n",
    );
    let mut host = store.load().unwrap().config.remote_hosts["h"].clone();
    assert!(host.allows_read("/srv/app"));
    assert!(host.allows_read("/srv/app/src/./main.rs"));
    assert!(host.allows_read("/tmp/x"));
    assert!(!host.allows_read("/srv/app2/secret"), "a sibling with the same prefix is outside");
    assert!(!host.allows_read("/srv/app/../../etc/passwd"));
    assert!(!host.allows_read("srv/app/file"));
    assert!(!host.allows_read("/srv"));
    assert!(!host.allows_write("/srv/app/file"), "hosts are read only unless allowed");
    host.read_only = false;
    assert!(host.allows_write("/srv/app/file"));
    assert!(!host.allows_write("/etc/passwd"));
}

#[test]
fn apply_to_session_adds_enabled_servers_toggles_and_executable() {
    let (_dir, store) = store_with(FULL);
    let c = store.load().unwrap().config;

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
    assert!(session.disabled_tools.is_empty());
}

/// cwd, enabled servers, disabled built in tools, disabled tools of `files`.
type Written = (PathBuf, Vec<String>, Vec<String>, Vec<String>);

/// Records what it was asked to write, like an adapter's native writer would.
#[derive(Default)]
struct RecordingWriter {
    written: Mutex<Vec<Written>>,
}

impl NativeConfigWriter for RecordingWriter {
    fn harness(&self) -> &'static str {
        "recording"
    }

    fn write(&self, cwd: &Path, config: &NativeConfig<'_>) -> Result<(), NativeConfigError> {
        let servers = config.mcp_servers.iter().map(|(name, _)| (*name).to_owned()).collect();
        let tools = config.disabled_tools.iter().cloned().collect();
        let mcp_off = config.disabled_mcp_tools("files").map(str::to_owned).collect();
        self.written.lock().unwrap().push((cwd.to_path_buf(), servers, tools, mcp_off));
        Ok(())
    }
}

#[test]
fn native_view_gives_writers_enabled_servers_and_project_toggles() {
    let (_dir, store) = store_with(FULL);
    let c = store.load().unwrap().config;
    let writer = RecordingWriter::default();
    let writers: [&dyn NativeConfigWriter; 1] = [&writer];
    for w in writers {
        w.write(Path::new("/work/loa"), &c.native_view(w.harness(), Some("loa"))).unwrap();
        w.write(Path::new("/work/other"), &c.native_view(w.harness(), None)).unwrap();
    }
    let view = c.native_view("recording", Some("loa"));
    assert!(!view.mcp_tool_enabled("files", "delete"));
    assert!(view.mcp_tool_enabled("search", "delete"));

    let written = writer.written.into_inner().unwrap();
    assert_eq!(
        written[0],
        (PathBuf::from("/work/loa"), vec!["files".into(), "search".into()], vec!["bash".into()], vec!["delete".into()])
    );
    assert_eq!(written[1], (PathBuf::from("/work/other"), vec!["files".into(), "search".into()], vec![], vec![]));
}
