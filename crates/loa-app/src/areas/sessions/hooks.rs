//! Rules and the advisor for harnesses without their own: rules join each prompt, the small
//! model reviews each finished turn.

use loa_util::LockExt;
use std::path::{Path, PathBuf};

use iced::Task;
use loa_bus::SessionId;
use loa_contracts::harness::{Feature, Prompt, SessionConfig};
use loa_core::{NodeId, attr};
use loa_session::hooks::{self, Advisor, TurnHooks};

use super::{Message, SMALL_MODEL, State, capabilities};
use crate::area::{Ctx, Target, perform};

impl State {
    /// The rules text for a session working in `cwd`.
    fn rules(&self, cwd: &Path) -> Option<String> {
        hooks::rules_context(&hooks::discover_rules(cwd, &self.home))
    }

    /// Starts the turn hooks of a new session whose harness has no rules or advisor.
    pub(super) fn start_hooks(&self, config: &mut SessionConfig, target: &Target<'_>) {
        if !target.capabilities.has(Feature::AdvisorRules) {
            let hooks = TurnHooks::start(config, self.rules(&config.cwd));
            self.hooks.locked().insert(config.session_id.clone(), hooks);
        }
    }

    /// Adds the rules (and an advisor note) to `prompt` before it goes to `session`.
    pub(super) fn hook_prompt(&self, ctx: &Ctx, session: &SessionId, prompt: &mut Prompt) {
        let Some(open) = ctx.sessions.get(session.as_str()) else { return };
        if capabilities(ctx, open.harness_id().unwrap_or_default()).has(Feature::AdvisorRules) {
            return;
        }
        let cwd = open.dom().root().and_then(|root| root.str_attr(attr::CWD)).map_or_else(|| ctx.services.cwd.clone(), PathBuf::from);
        let rules = self.rules(&cwd);
        let mut hooks = self.hooks.locked();
        let hooks = hooks.entry(session.to_string()).or_insert_with(|| TurnHooks::resumed(rules.clone()));
        prompt.text = hooks.before_turn(rules, &prompt.text);
    }

    /// Asks the advisor about a finished turn of a session whose harness has no advisor.
    pub(super) fn advise(&self, ctx: &Ctx, session: &SessionId, turn: NodeId) -> Task<Message> {
        let (Some(small), Some(open)) = (self.small.clone(), ctx.sessions.get(session.as_str())) else { return Task::none() };
        if capabilities(ctx, open.harness_id().unwrap_or_default()).has(Feature::AdvisorRules) {
            return Task::none();
        }
        let rules = self.hooks.locked().get(session.as_str()).and_then(|hooks| hooks.rules().map(str::to_owned));
        let (dom, id) = (open.dom().clone(), session.clone());
        perform(async move { Advisor::new(small.on(Some(SMALL_MODEL)).0).review(&dom, turn, rules.as_deref()).await }, move |result| {
            Message::Advised { session: id.clone(), turn, result }
        })
    }

    /// Records the advisor's note on `turn`; the next prompt carries it.
    pub(super) fn advised(&mut self, ctx: &mut Ctx, session: &SessionId, turn: NodeId, result: Result<Option<String>, String>) {
        let Some(note) = ctx.report("The advisor failed", result).flatten() else { return };
        if let Some(open) = ctx.sessions.get(session.as_str())
            && let Err(error) = hooks::record_note(open.writer().as_ref(), turn, &note)
        {
            ctx.notify(format!("Can't record the advisor's note: {error}"));
        }
        if let Some(hooks) = self.hooks.locked().get_mut(session.as_str()) {
            hooks.advise(note);
        }
    }
}
