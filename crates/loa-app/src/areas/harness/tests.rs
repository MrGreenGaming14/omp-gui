//! The harness area driven through `App::update`: availability probes, the
//! saved default harness and executable overrides.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use iced::Task;
use iced::futures::StreamExt;
use iced_runtime::Action as RuntimeAction;
use loa_contracts::harness::{
    Adapter, Availability, AvailabilityStatus, Capabilities, HarnessSession, Registry, Result as AdapterResult,
    SessionConfig,
};
use loa_core::{Dom, PatchSink};
use loa_identity::NodeIdentity;
use loa_ui_composer as composer;
use loa_ui_palette as palette;

use super::{Message as HarnessMessage, TAB};
use crate::app::{App, Message};
use crate::area::Area;
use crate::services::{CHECKING, Dirs, Harnesses, Node, Services};

/// A harness that only reports `availability`.
struct Probed {
    id: &'static str,
    availability: Availability,
}

#[async_trait::async_trait]
impl Adapter for Probed {
    fn harness(&self) -> &'static str {
        self.id
    }

    async fn availability(&self) -> Availability {
        self.availability.clone()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    async fn start(&self, _: SessionConfig, _: Arc<dyn PatchSink>) -> AdapterResult<Box<dyn HarnessSession>> {
        Err(loa_contracts::harness::AdapterError::Unsupported { harness: self.id, what: "sessions" })
    }

    async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> AdapterResult<Box<dyn HarnessSession>> {
        Err(loa_contracts::harness::AdapterError::Unsupported { harness: self.id, what: "sessions" })
    }
}

fn node() -> Node {
    Node::new(NodeIdentity::from_secret_bytes(&[5; 32]), "testhost".into())
}

fn app(dir: &Path, harnesses: Harnesses) -> App {
    App::build(Services::new(node(), Dirs::new(dir.join("loa")), harnesses, dir.to_owned()).unwrap()).0
}

/// `ready` reports version 1.2.3 and `gone` isn't installed; both start out
/// listed as available, as a CLI on `PATH` would.
fn probed_harnesses() -> Harnesses {
    let mut harnesses = Harnesses { registry: Registry::new(), list: Vec::new() };
    let ready = Availability::ready(Some("/opt/ready/bin/ready".into()), Some("1.2.3".into()));
    let gone = Availability::unavailable(AvailabilityStatus::NotInstalled, "`gone` not found");
    for (id, availability) in [("ready", ready), ("gone", gone)] {
        harnesses.add(Arc::new(Probed { id, availability }), id, true, Some(CHECKING.into()), None);
    }
    harnesses
}

/// Runs `task` the way the iced runtime would, feeding every message it
/// produces back through `App::update`, until no task is left.
fn run(app: &mut App, task: Task<Message>) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        let Some(stream) = iced_runtime::task::into_stream(task) else { continue };
        let outputs = runtime.block_on(async {
            let outputs = stream.filter_map(|action| async move {
                match action {
                    RuntimeAction::Output(message) => Some(message),
                    _ => None,
                }
            });
            tokio::time::timeout(Duration::from_secs(20), outputs.collect::<Vec<_>>()).await.expect("task finished")
        });
        pending.extend(outputs.into_iter().map(|message| app.update(message)));
    }
}

fn update(app: &mut App, message: Message) {
    let task = app.update(message);
    run(app, task);
}

/// Runs the palette command titled `title` as a user would: open, type,
/// submit.
fn palette_command(app: &mut App, title: &str) {
    update(app, Message::Palette(palette::Message::Open));
    update(app, Message::Palette(palette::Message::QueryChanged(title.to_owned())));
    update(app, Message::Palette(palette::Message::Submit));
}

/// The harness `id` as the composer's picker lists it.
fn picker_option(app: &App, id: &str) -> composer::HarnessOption {
    app.ctx.services.harnesses.options().into_iter().find(|option| option.id == id).unwrap()
}

#[test]
fn probes_set_availability_in_the_picker_palette_and_harnesses_tab() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path(), probed_harnesses());
    palette_command(&mut app, "Recheck harness availability");

    assert_eq!(app.ctx.shell.active_tab(), Some(TAB), "the command shows the Harnesses tab");
    let ready = picker_option(&app, "ready");
    assert!(ready.available);
    assert_eq!(ready.detail.as_deref(), Some("1.2.3"));
    let gone = picker_option(&app, "gone");
    assert!(!gone.available, "the probe overrides the PATH guess");
    assert_eq!(gone.detail.as_deref(), Some("`gone` not found"));
    assert_eq!(app.ctx.composer.harness().map(|option| option.id.as_str()), Some("ready"));
    let harness = app.ctx.services.harnesses.get("ready").unwrap();
    assert_eq!(harness.executable.as_deref(), Some(Path::new("/opt/ready/bin/ready")), "sessions run the probed executable");

    let titles: Vec<String> = app.areas.harness.palette_commands(&app.ctx).into_iter().map(|command| command.title).collect();
    assert!(titles.contains(&"Switch harness: ready".to_owned()), "{titles:?}");
    assert!(!titles.iter().any(|title| title.contains("gone")), "{titles:?}");

    let tab = app.areas.harness.view_tab(TAB, &app.ctx).expect("Harnesses tab");
    let mut ui = iced_test::simulator(tab);
    for label in ["Ready", "Not installed", "`gone` not found", "Version 1.2.3", "/opt/ready/bin/ready", "New sessions"] {
        assert!(ui.find(label).is_ok(), "the tab shows {label:?}");
    }
}

#[test]
fn an_unavailable_harness_cannot_be_picked() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path(), probed_harnesses());
    update(&mut app, Message::Harness(HarnessMessage::Recheck));
    // The composer's picker lists it greyed out and ignores the pick.
    update(&mut app, Message::Composer(composer::Message::HarnessPicked("gone".into())));
    assert_eq!(app.ctx.sessions.harness.as_deref(), Some("ready"));
    // A switch command for it is refused with a notice.
    update(&mut app, Message::Harness(HarnessMessage::Pick("gone".into())));
    assert_eq!(app.ctx.sessions.harness.as_deref(), Some("ready"));
    assert_eq!(app.ctx.notice(), Some("gone is unavailable"));
    assert!(app.ctx.sessions.active().is_none(), "nothing started");
}

#[test]
fn the_picked_harness_is_saved_as_the_default_for_the_next_launch() {
    let dir = tempfile::tempdir().unwrap();
    let mut harnesses = probed_harnesses();
    let other = Availability::ready(None, Some("2.0".into()));
    harnesses.add(Arc::new(Probed { id: "other", availability: other }), "other", true, None, None);
    let mut first = app(dir.path(), harnesses);
    update(&mut first, Message::Harness(HarnessMessage::Recheck));
    assert_eq!(first.ctx.sessions.harness.as_deref(), Some("ready"));
    // Picking starts a session on it; this fake can't run one.
    update(&mut first, Message::Composer(composer::Message::HarnessPicked("other".into())));
    let saved = first.ctx.services.config.load().unwrap().harness;
    assert_eq!(saved.default, "other");

    let mut harnesses = probed_harnesses();
    let other = Availability::ready(None, Some("2.0".into()));
    harnesses.add(Arc::new(Probed { id: "other", availability: other }), "other", true, None, None);
    let second = app(dir.path(), harnesses);
    assert_eq!(second.ctx.sessions.harness.as_deref(), Some("other"));
    assert_eq!(second.ctx.composer.harness().map(|option| option.id.as_str()), Some("other"));
}

#[cfg(unix)]
#[test]
fn an_executable_set_in_the_tab_is_saved_registered_and_probed() {
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join("my-codex");
    loa_util::exec::write_executable(&codex, "#!/bin/sh\necho 'codex-cli 0.99.1'\n").unwrap();
    let this = node();
    let key = this.key.clone();
    let dirs = Dirs::new(dir.path().join("loa"));
    let omp = dirs.omp.clone();
    let services = Services::new(this, dirs, Harnesses::detect(&key, &omp), dir.path().to_owned()).unwrap();
    let mut app = App::build(services).0;
    let id = loa_adapter_codex::HARNESS;

    update(&mut app, Message::Harness(HarnessMessage::EditExecutable(id.into(), codex.display().to_string())));
    update(&mut app, Message::Harness(HarnessMessage::SaveExecutable(id.into())));

    let saved = app.ctx.services.config.load().unwrap().harness;
    assert_eq!(saved.executable(id), Some(codex.as_path()));
    let harness = app.ctx.services.harnesses.get(id).unwrap();
    assert_eq!(harness.configured.as_deref(), Some(codex.as_path()));
    assert!(harness.option.available, "{:?}", harness.availability);
    assert_eq!(harness.availability.as_ref().and_then(|availability| availability.version.as_deref()), Some("0.99.1"));
    assert_eq!(harness.executable.as_deref(), Some(codex.as_path()));

    // A new launch registers Codex on the saved executable.
    let services = Services::new(node(), Dirs::new(dir.path().join("loa")), Harnesses::detect(&key, &omp), dir.path().to_owned()).unwrap();
    let relaunched = App::build(services).0;
    assert_eq!(relaunched.ctx.services.harnesses.get(id).unwrap().configured.as_deref(), Some(codex.as_path()));

    // Clearing the field restores the default and forgets the override.
    update(&mut app, Message::Harness(HarnessMessage::EditExecutable(id.into(), String::new())));
    update(&mut app, Message::Harness(HarnessMessage::SaveExecutable(id.into())));
    assert_eq!(app.ctx.services.config.load().unwrap().harness.executable(id), None);
    let harness = app.ctx.services.harnesses.get(id).unwrap();
    assert_eq!(harness.configured, None);
    assert_ne!(harness.executable.as_deref(), Some(codex.as_path()));
}
