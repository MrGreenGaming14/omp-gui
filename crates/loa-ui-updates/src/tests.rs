use super::*;
use iced::{Settings, Size};
use iced_test::Simulator;

fn s(v: &str) -> Option<String> {
    Some(v.to_owned())
}

fn product(id: &str, label: &str, kind: Kind) -> Product {
    Product {
        id: id.into(),
        label: label.into(),
        kind,
        source: format!("npm {id}"),
        current: s("1.0.0"),
        installed: Installed::System(format!("/usr/bin/{id}")),
        offer: None,
        newest: s("1.0.0"),
        changelog: String::new(),
        last_check_unix: Some(1_000),
        staged: Vec::new(),
        active: None,
        report: None,
        held: None,
        pending_swap: None,
        pin: None,
        waiting_past: None,
        signature: None,
        restart_required: false,
        error: None,
    }
}

fn check(label: &str, pass: bool, details: &[&str]) -> Check {
    Check { label: label.into(), pass, details: details.iter().map(|d| (*d).to_owned()).collect() }
}

fn held_report() -> Report {
    Report {
        from: "19.0.0".into(),
        to: "19.1.0".into(),
        checks: vec![
            check("Tool schemas", false, &["- edit: tool removed", "+ notebook: tool added"]),
            check("Session format version", true, &["unchanged at 3"]),
            check("Config schema", true, &[]),
            check("RPC protocol version", true, &["offered: 1, 2", "supported: 1..=2", "negotiated: 2"]),
            check("Adapter contract tests", true, &[]),
            check("Changelog", false, &["- removed: drop the `edit` tool"]),
        ],
    }
}

/// omp held by a harmful report, the app with a verified staged update and
/// a rollback, Codex up to date, Claude Code not installed.
fn data() -> Data {
    let mut omp = product("omp", "omp", Kind::Omp);
    omp.source = "GitHub can1357/oh-my-pi".into();
    omp.current = s("19.0.0");
    omp.offer = s("19.1.0");
    omp.newest = s("19.1.0");
    omp.staged = vec!["19.1.0".into()];
    omp.report = Some(held_report());
    omp.held = Some(Held { version: "19.1.0".into(), harmful: 2 });
    omp.changelog = "## 19.1.0\n\n- drop the `edit` tool\n- faster startup\n".into();

    let mut app = product("app", "loa", Kind::App);
    app.source = "GitHub MrGreenGaming14/omp-gui".into();
    app.current = s("0.1.0");
    app.installed = Installed::Running;
    app.offer = s("0.2.0");
    app.newest = s("0.2.0");
    app.staged = vec!["0.2.0".into()];
    app.signature = Some(Signature::Verified);
    app.report = Some(Report { from: "0.1.0".into(), to: "0.2.0".into(), checks: vec![] });
    app.pending_swap = s("0.2.0");

    let mut codex = product("codex", "Codex", Kind::Harness);
    codex.active = Some(Active { current: "1.0.0".into(), previous: s("0.9.0") });
    codex.installed = Installed::Managed("/data/loa/updates/versions/codex/1.0.0/payload/bin/codex".into());

    let mut claude = product("claude", "Claude Code", Kind::Harness);
    claude.current = None;
    claude.installed = Installed::Missing("claude not found".into());
    claude.newest = s("2.0.14");
    claude.offer = s("2.0.14");

    let peers = vec![
        Peer {
            node: "k3y-laptop-0123456789".into(),
            name: s("laptop"),
            version: s("0.1.0"),
            standing: Standing::Behind { majors: 0, minors: 1, patches: 0 },
            protocols: vec![("session".into(), Some(1))],
            can_talk: true,
        },
        Peer {
            node: "k3y-desk".into(),
            name: s("desk"),
            version: s("0.2.0"),
            standing: Standing::Current,
            protocols: vec![("session".into(), Some(2))],
            can_talk: true,
        },
    ];
    Data { products: vec![app, omp, codex, claude], github_requests_left: None, peers, turn_running: false, now_unix: 1_300 }
}

fn simulator(state: &State) -> Simulator<'_, Message> {
    Simulator::with_size(Settings::default(), Size::new(1200.0, 1600.0), state.view())
}

fn deliver(state: &mut State, messages: impl IntoIterator<Item = Message>) -> Vec<Event> {
    messages.into_iter().filter_map(|m| state.update(m).1).collect()
}

#[test]
fn held_update_offers_update_anyway_pin_and_wait() {
    let mut state = State::new(data());
    let mut ui = simulator(&state);
    ui.click("Update anyway to 19.1.0").expect("update anyway");
    ui.click("Pin 19.0.0").expect("pin");
    ui.click("Wait").expect("wait");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(
        deliver(&mut state, messages),
        [
            Event::Choose("omp".into(), Choice::UpdateAnyway),
            Event::Choose("omp".into(), Choice::PinCurrent),
            Event::Choose("omp".into(), Choice::Wait),
        ]
    );
}

#[test]
fn status_lines_follow_each_product() {
    let state = State::new(data());
    let mut ui = simulator(&state);
    ui.find("19.1.0 held: 2 harmful checks. Update anyway, pin or wait.").expect("held line");
    ui.find("Checks passed; ready to swap to 0.2.0").expect("pending line");
    ui.find("Up to date").expect("codex line");
    ui.find("Not installed; latest is 2.0.14").expect("claude line");
    ui.find("19.0.0 → 19.1.0").expect("versions");
    ui.find("Roll back to 0.9.0").expect("rollback");
    ui.find("Swap to 0.2.0").expect("swap");
    ui.find("Verify signature").expect("verify");
    assert!(ui.find("Download 2.0.14").is_err(), "nothing to update while not installed");
}

#[test]
fn swap_waits_for_the_running_turn() {
    let mut data = data();
    data.turn_running = true;
    let state = State::new(data);
    let mut ui = simulator(&state);
    ui.find("Checks passed; 0.2.0 swaps in when the running turn ends").expect("deferred line");
}

#[test]
fn actions_emit_their_events_and_mark_the_product_busy() {
    let mut state = State::new(data());
    let mut ui = simulator(&state);
    ui.click("Swap to 0.2.0").expect("swap");
    ui.click("Roll back to 0.9.0").expect("rollback");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::Swap("app".into()), Event::Rollback("codex".into())]);
    assert_eq!(state.busy("app"), Some(Step::Swapping));
    // A busy product takes no further requests until the app finishes it.
    assert_eq!(state.update(Message::Check("app".into())).1, None);
    state.finish("app");
    assert_eq!(state.update(Message::Check("app".into())).1, Some(Event::Check("app".into())));
    let mut ui = simulator(&state);
    ui.find("Checking feed…").expect("busy label");
}

#[test]
fn offer_steps_go_download_then_gate_checks() {
    let mut data = data();
    let claude = &mut data.products[3];
    claude.current = s("2.0.0");
    claude.installed = Installed::System("/usr/bin/claude".into());
    let mut state = State::new(data.clone());
    let mut ui = simulator(&state);
    ui.find("Update available: 2.0.14").expect("offer line");
    ui.click("Download 2.0.14").expect("download");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::Stage("claude".into())]);

    data.products[3].staged = vec!["2.0.14".into()];
    state.finish("claude");
    state.set_data(data);
    let mut ui = simulator(&state);
    ui.find("2.0.14 downloaded; run the gate checks").expect("staged line");
    ui.click("Run gate checks").expect("gate");
    let messages: Vec<Message> = ui.into_messages().collect();
    assert_eq!(deliver(&mut state, messages), [Event::Gate("claude".into())]);
}

#[test]
fn check_all_marks_every_idle_product_once() {
    let mut state = State::new(data());
    deliver(&mut state, [Message::Gate("omp".into())]);
    assert_eq!(state.update(Message::CheckAll).1, Some(Event::CheckAll));
    assert_eq!(state.busy("omp"), Some(Step::RunningChecks), "a running step is kept");
    assert_eq!(state.busy("codex"), Some(Step::Checking));
    assert_eq!(state.update(Message::CheckAll).1, None, "nothing idle");
    state.finish_all();
    assert_eq!(state.busy("codex"), None);
}

#[test]
fn details_show_gate_report_notes_and_signature() {
    let mut state = State::new(data());
    let mut ui = simulator(&state);
    ui.click("omp").expect("omp row");
    ui.click("loa").expect("app row");
    let messages: Vec<Message> = ui.into_messages().collect();
    deliver(&mut state, messages);
    assert_eq!(state.selected.as_deref(), Some("app"), "one product open at a time");
    deliver(&mut state, [Message::Toggle("omp".into())]);
    let mut ui = simulator(&state);
    ui.find("Gate report 19.0.0 → 19.1.0").expect("report head");
    ui.find("2 of 6 checks found something harmful.").expect("verdict");
    ui.find("- edit: tool removed").expect("tool diff");
    ui.find("Adapter contract tests").expect("contract check");
    ui.find("- faster startup").expect("release notes");
    ui.find("GitHub can1357/oh-my-pi · checked 5 min ago").expect("feed line");
    drop(ui);

    deliver(&mut state, [Message::Toggle("app".into())]);
    let mut ui = simulator(&state);
    ui.find("verified with a trusted key").expect("signature");
    ui.find("No gate checks apply; the signature is checked at swap.").expect("app report");
}

#[test]
fn peers_behind_are_listed() {
    let state = State::new(data());
    let mut ui = simulator(&state);
    ui.find("1 peer is behind.").expect("summary");
    ui.find("behind by 1 minor").expect("laptop");
    ui.find("current").expect("desk");
}

#[test]
fn errors_are_shown_on_their_product() {
    let mut data = data();
    data.products[2].error = Some("feed: https://api.github.com answered HTTP 404".into());
    let state = State::new(data);
    let mut ui = simulator(&state);
    ui.find("feed: https://api.github.com answered HTTP 404").expect("error box");
}
