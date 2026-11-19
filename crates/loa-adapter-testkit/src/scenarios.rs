//! Ready-made live scenarios: a script for [`ScriptedAdapter`] and the driver
//! actions that exercise it. Adapter crates run the same actions against
//! their own fake harness programmed to behave like the script.

use crate::live::Action;
use crate::scripted::{Ask, Script, ScriptedAdapter, Step};
use loa_adapter::{Answer, Prompt};
use loa_core::{DecisionKind, answer};

/// `Prompt::by` and `Answer::by` used by the scenarios.
pub const USER: &str = "contract-user";

/// One live scenario.
#[derive(Clone, Debug)]
pub struct Scenario {
    pub name: &'static str,
    /// What the harness does on each turn, as a script.
    pub turns: Vec<Script>,
    pub actions: Vec<Action>,
}

impl Scenario {
    /// A scripted adapter that plays this scenario's turns.
    pub fn adapter(&self) -> ScriptedAdapter {
        ScriptedAdapter::new(self.turns.clone())
    }
}

/// A prompt by [`USER`].
pub fn prompt(text: &str) -> Prompt {
    Prompt { text: text.to_owned(), attachments: Vec::new(), by: USER.to_owned() }
}

/// Thinking and text streamed token by token, then usage. Ends with a
/// capability probe while idle.
pub fn streaming() -> Scenario {
    let mut turn = Step::think("The user wants a greeting.");
    turn.extend(Step::say("Hello there, how can I help?"));
    turn.push(Step::Usage { input_tokens: 12, output_tokens: 9 });
    Scenario {
        name: "streaming",
        turns: vec![turn],
        actions: vec![Action::Send(prompt("Say hello")), Action::WaitIdle, Action::ProbeCapabilities],
    }
}

/// A tool call whose arguments stream in two parts, then a reply.
pub fn tool_call() -> Scenario {
    let mut turn = Step::say("Listing files.");
    turn.extend([
        Step::CallStart {
            tool: "bash".to_owned(),
            call_id: "call-1".to_owned(),
            title: Some("ls".to_owned()),
            args: "{\"command\":".to_owned(),
        },
        Step::CallArgs("\"ls\"}".to_owned()),
        Step::CallEnd { output: "a.txt\nb.txt\n".to_owned(), is_error: false },
    ]);
    turn.extend(Step::say("There are two files."));
    Scenario { name: "tool_call", turns: vec![turn], actions: vec![Action::Send(prompt("List files")), Action::WaitIdle] }
}

/// A tool call that needs approval; the user allows it once.
pub fn approval() -> Scenario {
    let mut turn = vec![
        Step::CallStart {
            tool: "bash".to_owned(),
            call_id: "call-1".to_owned(),
            title: Some("rm a.txt".to_owned()),
            args: "{\"command\":\"rm a.txt\"}".to_owned(),
        },
        Step::Ask(Ask {
            kind: DecisionKind::Approval,
            request_id: "req-1".to_owned(),
            title: Some("Run rm a.txt?".to_owned()),
            options: Vec::new(),
            body: "rm a.txt".to_owned(),
        }),
        Step::CallEnd { output: String::new(), is_error: false },
    ];
    turn.extend(Step::say("Removed a.txt."));
    let allow = Answer { values: vec![answer::ALLOW_ONCE.to_owned()], reason: None, by: USER.to_owned() };
    Scenario {
        name: "approval",
        turns: vec![turn],
        actions: vec![Action::Send(prompt("Delete a.txt")), Action::Answer(allow), Action::WaitIdle],
    }
}

/// The user interrupts a reply mid stream, a prompt sent meanwhile is
/// refused as busy, and the next turn runs normally.
pub fn interrupt_mid_stream() -> Scenario {
    let first = vec![
        Step::MessageStart { thinking: false },
        Step::Token("Let me ".to_owned()),
        Step::Token("look".to_owned()),
        Step::Hold,
        Step::Token(" further".to_owned()),
        Step::MessageEnd,
    ];
    Scenario {
        name: "interrupt_mid_stream",
        turns: vec![first, Step::say("Ready.")],
        actions: vec![
            Action::Send(prompt("Explain the build")),
            Action::SendWhileBusy(prompt("Also check the tests")),
            Action::InterruptWhileStreaming { after: "Let me look".to_owned() },
            Action::Send(prompt("Are you there?")),
            Action::WaitIdle,
        ],
    }
}

/// Every scenario above.
pub fn all() -> Vec<Scenario> {
    vec![streaming(), tool_call(), approval(), interrupt_mid_stream()]
}
