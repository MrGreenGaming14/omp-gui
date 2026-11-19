//! The sessions area against the installed omp with real model calls.
//! Run with `cargo test -p loa-app --lib areas::sessions::live -- --ignored`.

use std::time::Duration;

use loa_adapter::Prompt;
use loa_core::{ElementKind, Role, attr};
use loa_ui_composer as composer;
use loa_ui_palette as palette;
use loa_ui_shell as shell;

use super::Message as SessionsMessage;
use crate::app::{App, Message};
use crate::area::Request;
use crate::areas::harness::tests::{Live, active_dom, ended, live_model, live_session, turn_status, turn_text};

const TURN: Duration = Duration::from_secs(240);

/// Sends `text` to the active session and waits for its turn `index` to end.
fn turn(live: &mut Live, app: &mut App, text: &str, index: usize) -> String {
    let by = app.ctx.services.node.key.clone();
    app.ctx.request(Request::Prompt(Prompt { text: text.to_owned(), by, ..Prompt::default() }));
    live.update(app, Message::Sessions(SessionsMessage::Done(Ok(()))));
    live.until(app, "the turn", TURN, |app| ended(turn_status(active_dom(app), index)));
    assert_eq!(turn_status(active_dom(app), index), Some(loa_core::TurnStatus::Done), "notice: {:?}", app.ctx.notice());
    turn_text(active_dom(app), index, "assistant")
}

/// Waits for a new active session other than `others` to run, then
/// switches it to the live model.
fn branch_running(live: &mut Live, app: &mut App, others: &[&str]) -> String {
    live.until(app, "the branch to start", Duration::from_secs(120), |app| {
        app.ctx.sessions.active().is_some_and(|session| !others.contains(&session.id().as_str()) && session.harness().is_some())
    });
    let model = live_model();
    let (provider, name) = model.split_once('/').unwrap_or(("", &model));
    let option = composer::ModelOption { id: model.clone(), name: name.to_owned(), provider: provider.to_owned(), role: None };
    live.update(app, Message::Composer(composer::Message::ModelPicked(option)));
    live.until(app, "the model switch", Duration::from_secs(30), |app| {
        active_dom(app).root().and_then(|root| root.str_attr(attr::MODEL)) == Some(model.as_str())
    });
    app.ctx.sessions.active_id().unwrap().to_string()
}

#[test]
#[ignore = "runs the installed omp with real model calls (LOA_LIVE_MODEL, default anthropic/claude-haiku-5-5)"]
fn live_omp_session_forks_from_the_palette_and_rewinds_from_a_user_message() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::new();
    let mut app = live_session(dir.path(), &mut live);
    let parent = app.ctx.sessions.active_id().unwrap().to_string();
    turn(&mut live, &mut app, "Remember the code word HERON. Reply only with OK.", 0);
    turn(&mut live, &mut app, "The code word is now OTTER. Reply only with OK.", 1);

    let task = app.command(&palette::CommandId::new("sessions.fork"));
    live.spawn(task);
    let fork = branch_running(&mut live, &mut app, &[&parent]);
    assert_eq!(active_dom(&app).root().and_then(|root| root.str_attr(attr::PARENT_SESSION)), Some(parent.as_str()));
    let reply = turn(&mut live, &mut app, "What is the current code word? Reply with just the word.", 0);
    assert!(reply.to_uppercase().contains("OTTER"), "the fork remembers: {reply:?}");

    live.update(&mut app, Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::OpenSession(parent.clone()))));
    live.until(&mut app, "the parent", Duration::from_secs(10), |app| app.ctx.sessions.is_active(&parent));
    let dom = active_dom(&app);
    let second = dom
        .iter()
        .filter(|node| node.kind == ElementKind::Message && node.str_attr(attr::ROLE) == Some(Role::User.as_str()))
        .find(|node| node.text.contains("OTTER"))
        .map(|node| node.id)
        .expect("the second prompt");
    live.update(&mut app, Message::Chat(loa_ui_chat::Message::Rewind(second)));
    let rewound = branch_running(&mut live, &mut app, &[&parent, &fork]);
    assert_eq!(active_dom(&app).root().and_then(|root| root.str_attr(attr::PARENT_SESSION)), Some(parent.as_str()));
    let reply = turn(&mut live, &mut app, "What is the code word? Reply with just the word.", 0);
    assert!(reply.to_uppercase().contains("HERON") && !reply.to_uppercase().contains("OTTER"), "rewound before OTTER: {reply:?}");

    // The Session tree of the parent shows both branches.
    live.update(&mut app, Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::OpenSession(parent.clone()))));
    live.until(&mut app, "the parent", Duration::from_secs(10), |app| app.ctx.sessions.is_active(&parent));
    let rows: Vec<String> = app.areas.sessions.tree.view.rows().map(|row| format!("{:?}", row.kind)).collect();
    assert!(rows.iter().any(|row| row.contains(&fork)) && rows.iter().any(|row| row.contains(&rewound)), "{rows:?}");
}

#[test]
#[ignore = "runs the installed omp with real model calls (LOA_LIVE_MODEL, default anthropic/claude-haiku-5-5)"]
fn live_omp_session_compacts_with_the_compact_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::new();
    let mut app = live_session(dir.path(), &mut live);
    // omp keeps the newest ~20k tokens, so give it more than that to compact.
    let filler: String =
        (0..2500).map(|i| format!("Inventory note {i}: shelf {} holds {} crates of item {i}.\n", i % 40, i * 7 % 97)).collect();
    turn(&mut live, &mut app, &format!("Here are my warehouse notes:\n{filler}\nAlso, my favourite colour is teal. Reply only with OK."), 0);
    turn(&mut live, &mut app, "My favourite number is 7. Reply only with OK.", 1);

    for c in "/compact".chars() {
        live.update(&mut app, Message::Composer(composer::Message::Edit(iced::widget::text_editor::Action::Edit(
            iced::widget::text_editor::Edit::Insert(c),
        ))));
    }
    live.update(&mut app, Message::Composer(composer::Message::Submit));
    live.until(&mut app, "the <compaction>", TURN, |app| active_dom(app).iter().any(|node| node.kind == ElementKind::Compaction));
    let summary = active_dom(&app).iter().find(|node| node.kind == ElementKind::Compaction).unwrap().text.clone();
    assert!(!summary.trim().is_empty(), "omp wrote a summary");
    let reply = turn(&mut live, &mut app, "What is my favourite colour? Reply with one word.", 2);
    assert!(reply.to_lowercase().contains("teal"), "after compaction omp answered {reply:?}");
}

#[test]
#[ignore = "runs the installed omp (its /dirs command makes no model call)"]
fn live_omp_lists_its_commands_in_the_composer_and_runs_one_typed_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::new();
    let mut app = live_session(dir.path(), &mut live);
    live.until(&mut app, "omp's command list", Duration::from_secs(60), |app| {
        app.ctx.composer.commands().iter().any(|command| command.name == "dirs")
    });
    let dirs = app.ctx.composer.commands().iter().find(|command| command.name == "dirs").unwrap();
    assert!(dirs.description.starts_with("omp: "), "{dirs:?}");
    assert!(app.ctx.composer.commands().iter().any(|command| command.name == "fork"), "the app's own stay listed");

    live.prompt(&mut app, "/dirs");
    live.until(&mut app, "the command's output", Duration::from_secs(30), |app| {
        ended(turn_status(active_dom(app), 0)) && !turn_text(active_dom(app), 0, "assistant").is_empty()
    });
    assert_eq!(turn_status(active_dom(&app), 0), Some(loa_core::TurnStatus::Done));
    let output = turn_text(active_dom(&app), 0, "assistant");
    assert!(output.contains("Workspace directories"), "omp's output: {output:?}");
    assert!(!app.ctx.composer.is_busy());
}
