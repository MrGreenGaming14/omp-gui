//! Decision cards answering a real omp: approvals and `ask` questions reach
//! the [`Desk`] as cards, and the card's answers go back through
//! `HarnessSession::answer`.
//! Run with `cargo test -p loa-ui-decisions --test live -- --ignored`.
//! `LOA_LIVE_MODEL` picks the model (default `anthropic/claude-haiku-5-5`).

use iced::Theme;
use iced_test::simulator;
use loa_adapter::{Adapter, HarnessSession, Prompt, SessionConfig};
use loa_adapter_omp::{OmpAdapter, OmpAdapterConfig};
use loa_core::{DecisionKind, Dom, ElementKind, MemorySink, answer, attr, now_ms};
use loa_ui_decisions::desk::{Event, Message};
use loa_ui_decisions::{Decision, Desk, DesktopNotifier, Shortcut, Unattended};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const TURN_TIMEOUT: Duration = Duration::from_secs(240);
const ME: &str = "node-live";

struct Live {
    sink: Arc<MemorySink>,
    session: Box<dyn HarnessSession>,
    desk: Desk,
    id: String,
}

impl Live {
    async fn start(dir: &Path, id: &str, approval_mode: Option<&str>) -> Self {
        let mut adapter_config = OmpAdapterConfig::new(ME, dir.join("state"));
        adapter_config.approval_mode = approval_mode.map(str::to_owned);
        let adapter = OmpAdapter::new(adapter_config);
        let sink = Arc::new(MemorySink::new());
        let config = SessionConfig {
            session_id: id.into(),
            node: ME.into(),
            cwd: dir.to_owned(),
            model: Some(std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into())),
            ..Default::default()
        };
        let session = adapter.start(config, sink.clone()).await.expect("omp starts in rpc mode");
        let mut desk = Desk::new(ME, &Theme::Dark, Unattended::default(), DesktopNotifier::new("loa"));
        desk.set_active(Some(id.into()));
        Self { sink, session, desk, id: id.into() }
    }

    /// Sends `text` and runs the turn to its end. Every card that waits is
    /// shown to `answer`, which drives the desk like a user would; the
    /// answers it produces go to the harness.
    async fn turn(&mut self, text: &str, mut answer: impl FnMut(&mut Desk, &Decision) -> Vec<Event>) {
        self.session.send(Prompt { text: text.into(), by: ME.into(), ..Default::default() }).await.expect("prompt accepted");
        tokio::time::timeout(TURN_TIMEOUT, async {
            loop {
                tokio::select! {
                    done = self.session.wait_idle() => { done.expect("turn ended"); break; }
                    () = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
                let dom = self.sink.dom();
                self.desk.sync(&dom, now_ms());
                let Some(focused) = self.desk.focused() else { continue };
                let decision = Decision::from_dom(&dom, focused).expect("a decision");
                // The card renders with its answers.
                {
                    let card = self.desk.view_card(&self.id, focused).expect("a card for the waiting decision");
                    let label = match decision.kind {
                        DecisionKind::Approval => "Allow once",
                        DecisionKind::Question | DecisionKind::Form => "Skip",
                        DecisionKind::Plan => "Approve",
                    };
                    simulator(card).find(label).expect("the card offers its answers");
                }
                for event in answer(&mut self.desk, &decision) {
                    let Event::Answer { session, decision, answer } = event else { continue };
                    assert_eq!(session, self.id);
                    assert_eq!(answer.by, ME);
                    if let Err(e) = self.session.answer(decision, answer).await {
                        self.desk.answer_failed(&session, decision);
                        panic!("omp refused the answer: {e}");
                    }
                }
            }
        })
        .await
        .expect("turn finished in time");
        self.desk.sync(&self.sink.dom(), now_ms());
    }

    fn dom(&self) -> Dom {
        self.sink.dom()
    }
}

fn decisions(dom: &Dom) -> Vec<Decision> {
    Decision::all(dom).collect()
}

fn results(dom: &Dom) -> String {
    dom.iter().filter(|n| n.kind == ElementKind::Result).map(|n| n.text.as_str()).collect::<Vec<_>>().join("\n")
}

fn assistant_text(dom: &Dom) -> String {
    dom.iter()
        .filter(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some("assistant") && n.str_attr(attr::KIND) != Some("thinking"))
        .map(|n| n.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn card(desk: &mut Desk, id: &str, message: loa_ui_decisions::Message) -> Vec<Event> {
    desk.update(Message::Card(id.into(), message))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn approving_a_bash_call_on_its_card_runs_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::start(dir.path(), "live-approve", Some("always-ask")).await;
    let mut shown = 0;
    live.turn(
        "Run the shell command `echo loa-approved-7731 > approved.txt` with your bash tool, then say done.",
        |desk, decision| {
            assert_eq!(decision.kind, DecisionKind::Approval, "{decision:?}");
            shown += 1;
            assert_eq!(desk.badge("live-approve"), 1, "the waiting badge counts the card");
            // Number key 1 then Enter: allow once.
            desk.update(Message::Shortcut(Shortcut::Pick(0)));
            desk.update(Message::Shortcut(Shortcut::Confirm))
        },
    )
    .await;
    assert!(shown >= 1, "omp asked for approval");
    let dom = live.dom();
    let written = std::fs::read_to_string(dir.path().join("approved.txt")).expect("the approved command ran");
    assert_eq!(written.trim(), "loa-approved-7731");
    for d in decisions(&dom) {
        assert_eq!((d.status.as_str(), d.answer.as_slice(), d.by.as_deref()), ("answered", [answer::ALLOW_ONCE.to_owned()].as_slice(), Some(ME)));
    }
    assert_eq!(live.desk.total(), 0, "no badge once answered");
    live.session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn denying_with_a_reason_on_the_card_stops_the_call() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::start(dir.path(), "live-deny", Some("always-ask")).await;
    let id = live.id.clone();
    live.turn(
        "Run the shell command `touch denied.txt` with your bash tool. If it is denied, do not retry; just tell me why it was denied.",
        |desk, decision| {
            assert_eq!(decision.kind, DecisionKind::Approval, "{decision:?}");
            card(desk, &id, loa_ui_decisions::Message::Text(decision.id, "the codeword is pelican".into()));
            // Enter in the reason field denies with the reason.
            card(desk, &id, loa_ui_decisions::Message::SubmitText(decision.id))
        },
    )
    .await;
    let dom = live.dom();
    assert!(!dir.path().join("denied.txt").exists(), "the denied command did not run");
    let all = decisions(&dom);
    assert!(!all.is_empty(), "omp asked for approval");
    let first = &all[0];
    assert_eq!(first.status.as_str(), "answered");
    assert_eq!(first.answer, [answer::DENY]);
    assert_eq!(first.reason.as_deref(), Some("the codeword is pelican"));
    assert_eq!(first.by.as_deref(), Some(ME));
    let said = assistant_text(&dom).to_lowercase();
    assert!(said.contains("pelican"), "the model got the reason: {said:?}");
    live.session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn an_ask_question_is_answered_on_its_card() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::start(dir.path(), "live-ask", None).await;
    let id = live.id.clone();
    let mut asked = None;
    live.turn(
        "Use your ask tool to ask me exactly one question, \"Which color?\", with the two options red and green. \
         After I answer, reply with only the color I picked.",
        |desk, decision| {
            assert_eq!(decision.kind, DecisionKind::Question, "{decision:?}");
            let green = decision.options.iter().position(|o| o.to_lowercase().contains("green")).expect("a green option");
            asked = Some(decision.clone());
            card(desk, &id, loa_ui_decisions::Message::Choose(decision.id, green))
        },
    )
    .await;
    let asked = asked.expect("omp asked through the card");
    let dom = live.dom();
    let recorded = Decision::from_dom(&dom, asked.id).unwrap();
    assert_eq!(recorded.status.as_str(), "answered");
    assert!(recorded.answer[0].to_lowercase().contains("green"), "{recorded:?}");
    assert_eq!(recorded.by.as_deref(), Some(ME));
    let said = assistant_text(&dom).to_lowercase();
    assert!(said.contains("green") && !said.contains("red"), "assistant said {said:?}; results {}", results(&dom));
    live.session.shutdown().await.unwrap();
}

/// omp's `ask` tool asks each question as its own `select`; a multi select
/// question comes back after every pick until "Done selecting".
#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn an_ask_with_several_questions_runs_card_by_card_with_number_keys() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::start(dir.path(), "live-ask-many", None).await;
    let mut languages = vec!["rust", "zig", "done selecting"].into_iter();
    let mut cards = Vec::new();
    live.turn(
        "Use your ask tool ONCE with exactly two questions in the same call: \
         \"Which color?\" (single choice, options red and green) and \
         \"Which languages?\" (multi select, options rust, go and zig). \
         After I answer, reply with only the color and the languages I picked, comma separated.",
        |desk, decision| {
            assert_eq!(decision.kind, DecisionKind::Question, "{decision:?}");
            cards.push(decision.id);
            let wanted = if decision.body.to_lowercase().contains("color") {
                "green"
            } else {
                languages.next().expect("at most three picks")
            };
            let index = decision
                .options
                .iter()
                .position(|o| o.to_lowercase().contains(wanted))
                .unwrap_or_else(|| panic!("no {wanted} option in {decision:?}"));
            desk.update(Message::Shortcut(Shortcut::Pick(index)));
            desk.update(Message::Shortcut(Shortcut::Confirm))
        },
    )
    .await;
    assert!(cards.len() >= 2, "both questions came as cards: {cards:?}");
    let dom = live.dom();
    for id in cards {
        let recorded = Decision::from_dom(&dom, id).unwrap();
        assert_eq!(recorded.status.as_str(), "answered", "{recorded:?}");
        assert_eq!(recorded.by.as_deref(), Some(ME));
    }
    let said = assistant_text(&dom).to_lowercase();
    assert!(said.contains("green") && said.contains("rust") && said.contains("zig") && !said.contains("red"), "assistant said {said:?}");
    live.session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp with real model calls"]
async fn a_typed_answer_on_an_ask_card_reaches_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let mut live = Live::start(dir.path(), "live-ask-typed", None).await;
    let id = live.id.clone();
    let mut asked = None;
    live.turn(
        "Use your ask tool to ask me exactly one question, \"Which color?\", with the two options red and green. \
         After I answer, reply with only the color I gave.",
        |desk, decision| {
            assert_eq!(decision.kind, DecisionKind::Question, "{decision:?}");
            asked = Some(decision.id);
            card(desk, &id, loa_ui_decisions::Message::Text(decision.id, "purple".into()));
            card(desk, &id, loa_ui_decisions::Message::SubmitText(decision.id))
        },
    )
    .await;
    let dom = live.dom();
    let recorded = Decision::from_dom(&dom, asked.expect("omp asked through the card")).unwrap();
    assert_eq!(recorded.status.as_str(), "answered", "{recorded:?}");
    assert_eq!(recorded.by.as_deref(), Some(ME));
    let said = assistant_text(&dom).to_lowercase();
    assert!(said.contains("purple"), "assistant said {said:?}");
    live.session.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "raises a real desktop notification over the session D-Bus"]
async fn a_waiting_card_in_the_background_raises_a_desktop_notification() {
    use loa_core::{Draft, NodeId, Value};
    let mut dom = Dom::new();
    let root = dom
        .commit(
            Draft::insert(
                NodeId::DOCUMENT,
                ElementKind::Session,
                [(attr::ID, Value::from("live-notify")), (attr::HARNESS, "omp".into()), (attr::NODE, ME.into()), (attr::CREATED, 0i64.into()), (attr::TITLE, "loa live test".into())],
                "",
            ),
            0,
        )
        .unwrap()
        .node;
    let mut desk = Desk::new(ME, &Theme::Dark, Unattended::default(), DesktopNotifier::new("loa"));
    desk.update(Message::Focused(false));
    dom.commit(Draft::insert(root, ElementKind::Decision, [(attr::KIND, Value::from("question"))], "Which color?"), 1).unwrap();
    let notices = desk.sync(&dom, now_ms());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].heading(), "loa live test has a question");
    DesktopNotifier::new("loa").notify(&notices[0]).await.expect("the notification daemon took it");
}
