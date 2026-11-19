//! Schema v1 is the first real version, so these tests register test-only
//! steps to prove the chain, the backup and the refusal to downgrade.

use loa_config::{CURRENT_VERSION, ConfigError, ConfigStore, Migrator, Step};
use std::fs;
use toml::{Table, Value};

/// A pre-v1 layout: servers lived under `[servers]` and the port under `[p2p]`.
const LEGACY_V0: &str = r#"version = 0
theme = "light"

[servers.files]
command = "mcp-files"
x-note = "kept"

[p2p]
port = 4000
"#;

fn v0_to_v1(doc: &mut Table) -> Result<(), String> {
    if let Some(servers) = doc.remove("servers") {
        doc.insert("mcp_servers".into(), servers);
    }
    if let Some(Value::Table(mut p2p)) = doc.remove("p2p") {
        let mut network = Table::new();
        if let Some(port) = p2p.remove("port") {
            network.insert("port".into(), port);
        }
        doc.insert("network".into(), Value::Table(network));
    }
    Ok(())
}

fn legacy_migrator() -> Migrator {
    Migrator::new(CURRENT_VERSION).step(Step { from: 0, summary: "rename servers and p2p", apply: v0_to_v1 })
}

fn store_with(text: &str, migrator: Migrator) -> (tempfile::TempDir, ConfigStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ConfigStore::in_dir(dir.path()).with_migrator(migrator);
    fs::write(store.path(), text).unwrap();
    (dir, store)
}

fn dir_entries(store: &ConfigStore) -> Vec<String> {
    let dir = store.path().parent().unwrap();
    let mut names: Vec<String> =
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    names
}

#[test]
fn old_file_is_migrated_backed_up_and_replaced() {
    let (_dir, store) = store_with(LEGACY_V0, legacy_migrator());
    let loaded = store.load().unwrap();

    assert_eq!(loaded.migrated_from, Some(0));
    assert_eq!(loaded.config.version, CURRENT_VERSION);
    assert_eq!(loaded.config.network.port, 4000);
    assert!(loaded.config.mcp_servers.contains_key("files"));

    let backup = loaded.backup.unwrap();
    assert_eq!(backup.file_name().unwrap(), "config.toml.v0.bak");
    assert_eq!(fs::read_to_string(&backup).unwrap(), LEGACY_V0, "backup must be the original bytes");

    let on_disk: Table = fs::read_to_string(store.path()).unwrap().parse().unwrap();
    assert_eq!(on_disk["version"].as_integer(), Some(i64::from(CURRENT_VERSION)));
    assert_eq!(on_disk["network"]["port"].as_integer(), Some(4000));
    assert_eq!(on_disk["mcp_servers"]["files"]["x-note"].as_str(), Some("kept"));
    assert!(on_disk.get("servers").is_none());

    // The next load finds a current file: no migration, no new backup.
    let again = store.load().unwrap();
    assert_eq!((again.migrated_from, again.backup), (None, None));
    assert_eq!(again.config, loaded.config);
    assert_eq!(dir_entries(&store), ["config.toml", "config.toml.v0.bak"]);
}

#[test]
fn an_existing_backup_is_never_overwritten() {
    let (_dir, store) = store_with(LEGACY_V0, legacy_migrator());
    store.load().unwrap();
    let second = LEGACY_V0.replace("4000", "5000");
    fs::write(store.path(), &second).unwrap();
    let loaded = store.load().unwrap();
    let backup = loaded.backup.unwrap();
    assert_eq!(backup.file_name().unwrap(), "config.toml.v0.bak.1");
    assert_eq!(fs::read_to_string(backup).unwrap(), second);
    assert_eq!(
        fs::read_to_string(store.path().with_file_name("config.toml.v0.bak")).unwrap(),
        LEGACY_V0
    );
}

/// v1 -> v2 moves `theme` into `[ui]`; v2 -> v3 renames `ui.theme` to
/// `ui.scheme`. The second step only works on the first one's output.
fn chain() -> Migrator {
    fn to_v2(doc: &mut Table) -> Result<(), String> {
        let theme = doc.remove("theme").ok_or("v1 config without theme")?;
        let mut ui = Table::new();
        ui.insert("theme".into(), theme);
        doc.insert("ui".into(), Value::Table(ui));
        Ok(())
    }
    fn to_v3(doc: &mut Table) -> Result<(), String> {
        let ui = doc.get_mut("ui").and_then(Value::as_table_mut).ok_or("v2 config without [ui]")?;
        let theme = ui.remove("theme").ok_or("v2 config without ui.theme")?;
        ui.insert("scheme".into(), theme);
        Ok(())
    }
    Migrator::new(3)
        .step(Step { from: 2, summary: "rename ui.theme", apply: to_v3 })
        .step(Step { from: 1, summary: "move theme into [ui]", apply: to_v2 })
}

#[test]
fn steps_run_in_version_order_and_bump_the_version() {
    let mut doc: Table = "version = 1\ntheme = \"dark\"\nkeep = 7\n".parse().unwrap();
    assert_eq!(chain().migrate(&mut doc).unwrap(), Some(1));
    let expected: Table = "version = 3\nkeep = 7\n[ui]\nscheme = \"dark\"\n".parse().unwrap();
    assert_eq!(doc, expected);

    let mut v2: Table = "version = 2\n[ui]\ntheme = \"light\"\n".parse().unwrap();
    assert_eq!(chain().migrate(&mut v2).unwrap(), Some(2));
    assert_eq!(v2["ui"]["scheme"].as_str(), Some("light"));

    let mut current = expected.clone();
    assert_eq!(chain().migrate(&mut current).unwrap(), None);
    assert_eq!(current, expected);
}

#[test]
fn a_gap_in_the_chain_is_reported() {
    let migrator = Migrator::new(3).step(Step { from: 1, summary: "only step", apply: |_| Ok(()) });
    let mut doc: Table = "version = 1\n".parse().unwrap();
    assert!(matches!(migrator.migrate(&mut doc), Err(ConfigError::MissingMigration { from: 2 })));
    let mut doc: Table = "version = 0\n".parse().unwrap();
    assert!(matches!(Migrator::standard().migrate(&mut doc), Err(ConfigError::MissingMigration { from: 0 })));
}

#[test]
fn newer_config_is_refused_and_left_alone() {
    let newer = format!("version = {}\ntheme = \"dark\"\n", CURRENT_VERSION + 1);
    let (_dir, store) = store_with(&newer, Migrator::standard());
    let err = store.load().unwrap_err();
    assert!(
        matches!(err, ConfigError::TooNew { found, supported } if found == CURRENT_VERSION + 1 && supported == CURRENT_VERSION),
        "{err}"
    );
    assert!(err.to_string().contains("update the app"));
    assert_eq!(fs::read_to_string(store.path()).unwrap(), newer);
    assert_eq!(dir_entries(&store), ["config.toml"]);
}

#[test]
fn failed_step_leaves_the_file_untouched_without_backup() {
    let migrator = Migrator::new(CURRENT_VERSION)
        .step(Step { from: 0, summary: "always fails", apply: |_| Err("layout not recognised".into()) });
    let (_dir, store) = store_with(LEGACY_V0, migrator);
    let err = store.load().unwrap_err();
    assert!(matches!(&err, ConfigError::Migration { from: 0, reason } if reason == "layout not recognised"));
    assert_eq!(fs::read_to_string(store.path()).unwrap(), LEGACY_V0);
    assert_eq!(dir_entries(&store), ["config.toml"]);
}

#[test]
fn migration_producing_an_invalid_config_is_not_written() {
    let migrator = Migrator::new(CURRENT_VERSION).step(Step {
        from: 0,
        summary: "forgets to move p2p",
        apply: |doc| {
            doc.insert("network".into(), Value::Integer(1));
            Ok(())
        },
    });
    let (_dir, store) = store_with(LEGACY_V0, migrator);
    let err = store.load().unwrap_err();
    assert!(matches!(&err, ConfigError::Invalid { key, .. } if key == "network"), "{err}");
    assert_eq!(fs::read_to_string(store.path()).unwrap(), LEGACY_V0);
    assert_eq!(dir_entries(&store), ["config.toml"]);
}
