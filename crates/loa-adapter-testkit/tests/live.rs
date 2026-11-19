//! The live driver over `ScriptedAdapter`: the reference adapter passes every
//! scenario, and adapters with one bug each are caught.

mod common;

use common::{Faulty, golden};
use loa_adapter::{Answer, Capabilities, Feature, SessionConfig};
use loa_adapter_testkit::scenarios::{self, Scenario, USER, prompt};
use loa_adapter_testkit::{Action, Golden, LiveDriver, LiveReport, Rule, TIME_ATTRS, render_golden_masked};
use loa_core::{DecisionKind, Draft, ElementKind, MessageStatus, TurnStatus, Value, answer, attr};
use std::sync::Arc;

fn config() -> SessionConfig {
    SessionConfig {
        session_id: "live-session".to_owned(),
        node: "node-live".to_owned(),
        project: Some("demo".to_owned()),
        ..SessionConfig::default()
    }
}

async fn run(adapter: Faulty, actions: &[Action]) -> LiveReport {
    LiveDriver::new(Arc::new(adapter), config()).run(actions).await
}

async fn run_scenario(scenario: &Scenario) -> LiveReport {
    run(Faulty::new(scenario.adapter()), &scenario.actions).await
}

fn rules(report: &LiveReport) -> Vec<Rule> {
    report.violations.iter().map(|v| v.rule).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_scenario_passes_and_matches_its_golden() {
    for scenario in scenarios::all() {
        let report = run_scenario(&scenario).await;
        assert!(report.is_clean(), "{}: {report}", scenario.name);
        let rendering = render_golden_masked(&report.dom, TIME_ATTRS);
        let path = golden(&format!("live_{}.golden", scenario.name));
        if let Err(err) = Golden::from_env().check(&rendering, &path) {
            panic!("{}: {err}", scenario.name);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_keeps_the_streamed_text_and_cancels() {
    let report = run_scenario(&scenarios::interrupt_mid_stream()).await;
    assert!(report.is_clean(), "{report}");
    let dom = &report.dom;
    let root = dom.root().unwrap();
    let turns: Vec<_> = dom.children_of_kind(root.id, ElementKind::Turn).collect();
    assert_eq!(turns.len(), 2, "the busy prompt must not start a turn");
    assert_eq!(turns[0].str_attr(attr::STATUS), Some(TurnStatus::Cancelled.as_str()));
    let reply = dom.children_of_kind(turns[0].id, ElementKind::Message).nth(1).unwrap();
    assert_eq!(reply.text, "Let me look");
    assert_eq!(reply.str_attr(attr::STATUS), Some(MessageStatus::Cancelled.as_str()));
    assert_eq!(turns[1].str_attr(attr::STATUS), Some(TurnStatus::Done.as_str()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupt_that_leaves_the_message_done_is_caught() {
    fn done_instead_of_cancelled(draft: Draft) -> Draft {
        match draft {
            Draft::Set { node, name, value: Some(Value::Str(s)) } if name == attr::STATUS && s == "cancelled" && node.0 == 4 => {
                Draft::Set { node, name, value: Some(MessageStatus::Done.into()) }
            }
            other => other,
        }
    }
    let scenario = scenarios::interrupt_mid_stream();
    let mut adapter = Faulty::new(scenario.adapter());
    adapter.map = Some(done_instead_of_cancelled);
    let report = run(adapter, &scenario.actions).await;
    let found = report.violations.iter().find(|v| v.rule == Rule::Step);
    assert!(found.is_some_and(|v| v.detail.contains("message is Some(\"done\")")), "{report}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_that_is_not_recorded_is_caught() {
    fn drop_answer(draft: Draft) -> Draft {
        match draft {
            Draft::Set { node, name, value } if name == attr::ANSWER => Draft::Set { node, name: "x-lost".to_owned(), value },
            other => other,
        }
    }
    let scenario = scenarios::approval();
    let mut adapter = Faulty::new(scenario.adapter());
    adapter.map = Some(drop_answer);
    let report = run(adapter, &scenario.actions).await;
    let found = rules(&report);
    assert!(found.contains(&Rule::DecisionAnswer), "{report}");
    assert!(found.contains(&Rule::CloseOrder), "{report}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_answer_reason_and_author_are_checked() {
    let scenario = scenarios::approval();
    let deny = Answer { values: vec![answer::DENY.to_owned()], reason: Some("not now".to_owned()), by: USER.to_owned() };
    let actions = vec![Action::Send(prompt("Delete a.txt")), Action::Answer(deny), Action::WaitIdle];
    let report = run(Faulty::new(scenario.adapter()), &actions).await;
    assert!(report.is_clean(), "{report}");
    let decision = report.dom.iter().find(|n| n.kind == ElementKind::Decision).unwrap();
    assert_eq!(decision.str_attr(attr::REASON), Some("not now"));
    assert_eq!(decision.str_attr(attr::BY), Some(USER));
    assert_eq!(decision.int_attr(attr::CALL), Some(i64::try_from(decision.parent.0).unwrap()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claimed_queue_that_answers_busy_is_caught() {
    let scenario = scenarios::interrupt_mid_stream();
    let mut adapter = Faulty::new(scenario.adapter());
    adapter.caps = Some(Capabilities { queue_mid_turn: true, ..adapter.inner_caps() });
    let report = run(adapter, &scenario.actions).await;
    assert!(rules(&report).contains(&Rule::Capability), "{report}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_claimed_fork_that_is_unsupported_is_caught() {
    let scenario = scenarios::streaming();
    let mut adapter = Faulty::new(scenario.adapter());
    let mut caps = adapter.inner_caps();
    caps.features.push(Feature::Fork);
    adapter.caps = Some(caps);
    let report = run(adapter, &scenario.actions).await;
    let found: Vec<_> = report.violations.iter().filter(|v| v.rule == Rule::Capability).collect();
    assert_eq!(found.len(), 1, "{report}");
    assert!(found[0].detail.contains("fork"), "{report}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_undeclared_decision_kind_is_caught() {
    let scenario = scenarios::approval();
    let mut adapter = Faulty::new(scenario.adapter());
    adapter.caps = Some(Capabilities { decisions: vec![DecisionKind::Question], ..adapter.inner_caps() });
    let report = run(adapter, &scenario.actions).await;
    assert_eq!(rules(&report), vec![Rule::Undeclared], "{report}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_send_that_skips_the_user_message_is_caught() {
    fn drop_user_message(draft: Draft) -> Draft {
        match draft {
            Draft::Insert { parent, index, kind: ElementKind::Message, attrs, text }
                if attrs.get(attr::ROLE).and_then(Value::as_str) == Some("user") =>
            {
                let mut attrs = attrs;
                attrs.insert(attr::HARNESS.to_owned(), Value::from("scripted"));
                Draft::Insert { parent, index, kind: ElementKind::Ext, attrs, text }
            }
            other => other,
        }
    }
    let scenario = scenarios::tool_call();
    let mut adapter = Faulty::new(scenario.adapter());
    adapter.map = Some(drop_user_message);
    let report = run(adapter, &scenario.actions).await;
    assert!(rules(&report).contains(&Rule::TurnShape), "{report}");
}

trait InnerCaps {
    fn inner_caps(&self) -> Capabilities;
}

impl InnerCaps for Faulty {
    fn inner_caps(&self) -> Capabilities {
        use loa_adapter::Adapter;
        self.inner.capabilities()
    }
}
