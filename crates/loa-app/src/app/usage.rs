//! The Usage window: provider limits and saved resets from `omp usage --json`.

use loa_util::time::{ago, now_ms, until};
use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use iced::widget::{column, container, row, rule, scrollable};
use iced::{Alignment, Element, Fill, Task};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, width};
use loa_ui_kit::widgets::{card, collapsible, icon_button, label, meter, section_header, text_button};
use loa_ui_kit::Icon;
use loa_ui_shell::UsageMeter;
use serde_json::Value;

/// Where usage comes from and where saved resets are spent.
#[async_trait]
pub trait UsageSource: Send + Sync {
    async fn fetch(&self) -> Result<Usage, String>;
    /// Spends a saved reset of account `target` (`<provider>/<credential id>`).
    async fn spend(&self, target: &str) -> Result<(), String>;
}

/// [`UsageSource`] over the `omp usage` command.
pub struct OmpUsage;

#[async_trait]
impl UsageSource for OmpUsage {
    async fn fetch(&self) -> Result<Usage, String> {
        parse(&omp(&["usage", "--json"]).await?)
    }

    async fn spend(&self, target: &str) -> Result<(), String> {
        omp(&["usage", "reset", target]).await.map(drop)
    }
}

/// Runs `omp <args>`; its stdout, or its stderr as the error.
async fn omp(args: &[&str]) -> Result<String, String> {
    let what = args[..2].join(" ");
    let output = tokio::process::Command::new("omp").args(args).output().await.map_err(|error| format!("Couldn't run omp {what}: {error}"))?;
    if !output.status.success() {
        return Err(format!("omp {what} failed: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// One limit window of an account, e.g. "Claude 7 Day" at 51%.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    pub label: String,
    /// Short window name for the meter, e.g. "7d".
    pub window: String,
    /// Used share, 0 to 1; `None` when the provider did not report it.
    pub fraction: Option<f32>,
    /// Unix milliseconds of the next reset.
    pub resets_at: Option<i64>,
}

/// One signed-in account of a provider.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub label: String,
    /// `<provider>/<credential id>`, what `omp usage reset` takes.
    pub reset_target: Option<String>,
    pub limits: Vec<Limit>,
    /// Saved resets left, and how many of them can be spent now.
    pub resets: (u64, u64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub accounts: Vec<Account>,
    /// omp's capacity line, e.g. "5h 0.11 of 2 used · 7d 0.71 of 2 used".
    pub capacity: Option<String>,
}

impl Provider {
    /// The highest used share across this provider's limits.
    pub fn peak(&self) -> Option<f32> {
        self.accounts.iter().flat_map(|account| account.limits.iter().filter_map(|limit| limit.fraction)).reduce(f32::max)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Usage {
    pub generated_at: i64,
    pub providers: Vec<Provider>,
}

impl Usage {
    /// The sidebar meter: the tightest limit across every provider.
    pub fn meter(&self) -> Option<UsageMeter> {
        self.providers
            .iter()
            .flat_map(|provider| provider.accounts.iter())
            .flat_map(|account| account.limits.iter())
            .filter_map(|limit| limit.fraction.map(|fraction| (fraction, limit.window.clone())))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(fraction, label)| UsageMeter { fraction, label })
    }
}

/// Reads `omp usage --json`.
pub fn parse(json: &str) -> Result<Usage, String> {
    let root: Value = serde_json::from_str(json).map_err(|error| format!("omp usage printed invalid JSON: {error}"))?;
    let mut providers: Vec<Provider> = Vec::new();
    for report in root["reports"].as_array().into_iter().flatten() {
        let id = report["provider"].as_str().unwrap_or("unknown").to_owned();
        let metadata = &report["metadata"];
        let label = ["orgName", "email", "accountId"].iter().find_map(|key| metadata[*key].as_str().filter(|value| !value.is_empty())).unwrap_or("Account").to_owned();
        let reset_target = metadata["credentialId"].as_str().map(|credential| format!("{id}/{credential}"));
        let limits = report["limits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|limit| Limit {
                label: limit["label"].as_str().unwrap_or("Limit").to_owned(),
                window: limit["window"]["id"].as_str().unwrap_or("").to_owned(),
                fraction: limit["amount"]["usedFraction"].as_f64().map(|fraction| fraction as f32),
                resets_at: limit["window"]["resetsAt"].as_i64(),
            })
            .collect();
        let credits = &report["resetCredits"];
        let resets = (credits["availableCount"].as_u64().unwrap_or(0), credits["redeemableCount"].as_u64().unwrap_or(0));
        let account = Account { label, reset_target, limits, resets };
        match providers.iter_mut().find(|provider| provider.id == id) {
            Some(provider) => provider.accounts.push(account),
            None => providers.push(Provider { name: provider_name(&id), id, accounts: vec![account], capacity: None }),
        }
    }
    for provider in &mut providers {
        let Some(windows) = root["capacity"].get(&provider.id).and_then(Value::as_array) else { continue };
        let parts: Vec<String> = windows
            .iter()
            .map(|window| {
                let meter = window["meter"].as_str().map(|meter| format!(" {meter}")).unwrap_or_default();
                let (used, accounts) = (window["usedAccounts"].as_f64().unwrap_or(0.0), window["accounts"].as_u64().unwrap_or(0));
                format!("{}{meter} {used:.2} of {accounts} used", window["window"].as_str().unwrap_or(""))
            })
            .collect();
        provider.capacity = (!parts.is_empty()).then(|| parts.join(" · "));
    }
    Ok(Usage { generated_at: root["generatedAt"].as_i64().unwrap_or_default(), providers })
}

fn provider_name(id: &str) -> String {
    match id {
        "anthropic" => "Anthropic".into(),
        "openai-codex" => "OpenAI Codex".into(),
        "openai" => "OpenAI".into(),
        "google" | "gemini" => "Google".into(),
        other => {
            let mut chars = other.chars();
            chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Refresh,
    Loaded(Result<Usage, String>),
    Toggle(String),
    Spend(String),
    Spent(Result<(), String>),
    Close,
}

/// What the app must act on.
pub enum Event {
    Close,
    Meter(Option<UsageMeter>),
}

pub struct State {
    source: Arc<dyn UsageSource>,
    usage: Option<Result<Usage, String>>,
    loading: bool,
    open: BTreeSet<String>,
    spending: Option<String>,
}

impl State {
    pub fn new(source: Arc<dyn UsageSource>) -> Self {
        Self { source, usage: None, loading: false, open: BTreeSet::new(), spending: None }
    }

    /// Starts a fetch.
    pub fn refresh(&mut self) -> Task<Message> {
        self.loading = true;
        let source = Arc::clone(&self.source);
        Task::perform(async move { source.fetch().await }, Message::Loaded)
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Refresh => (self.refresh(), None),
            Message::Loaded(result) => {
                self.loading = false;
                let meter = result.as_ref().ok().and_then(Usage::meter);
                self.usage = Some(result);
                (Task::none(), Some(Event::Meter(meter)))
            }
            Message::Toggle(provider) => {
                if !self.open.remove(&provider) {
                    self.open.insert(provider);
                }
                (Task::none(), None)
            }
            Message::Spend(target) => {
                self.spending = Some(target.clone());
                let source = Arc::clone(&self.source);
                (Task::perform(async move { source.spend(&target).await }, Message::Spent), None)
            }
            Message::Spent(result) => {
                self.spending = None;
                if let Err(error) = result {
                    self.usage = Some(Err(error));
                }
                (self.refresh(), None)
            }
            Message::Close => (Task::none(), Some(Event::Close)),
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let now = now_ms();
        let updated = match &self.usage {
            _ if self.loading => "Updating…".to_owned(),
            Some(Ok(usage)) => format!("Updated {}", ago(now, usage.generated_at)),
            _ => String::new(),
        };
        let actions = [
            label(updated, TextRole::Caption, Tone::Faint).into(),
            text_button("Refresh", ButtonKind::Ghost, Some(Message::Refresh)),
            icon_button(Icon::X, "Close", Some(Message::Close)),
        ];
        let note = |s: &str| -> Element<'_, Message> { label(s.to_owned(), TextRole::Body, Tone::Muted).into() };
        let body: Vec<Element<'_, Message>> = match &self.usage {
            None => vec![note("Reading usage from omp…")],
            Some(Err(error)) => vec![note(error.as_str())],
            Some(Ok(usage)) if usage.providers.is_empty() => vec![note("No signed-in providers report usage.")],
            Some(Ok(usage)) => usage.providers.iter().map(|provider| self.provider(provider, now)).collect(),
        };
        let footer = label("Same numbers as omp’s /usage. Resets apply to an account, not a session.", TextRole::Caption, Tone::Faint);
        let body = column(body).push(rule::horizontal(1).style(style::rule)).push(footer).spacing(sp(SpaceRole::M));
        let window = column![section_header("Usage", actions), scrollable(body)].spacing(sp(SpaceRole::M));
        container(window).padding(sp(SpaceRole::L)).style(style::container(Surface::Popup)).width(width(LayoutRole::DialogWidth)).max_height(width(LayoutRole::TallHeight)).into()
    }

    fn provider<'a>(&'a self, provider: &'a Provider, now: i64) -> Element<'a, Message> {
        let peak = provider.peak().unwrap_or(0.0);
        let accounts = match provider.accounts.len() {
            1 => "1 account".to_owned(),
            n => format!("{n} accounts"),
        };
        let title = format!("{} · {accounts}", provider.name);
        let summary = row![
            container(meter(peak)).width(width(LayoutRole::LabelColumn) / 2.0),
            label(format!("Highest {:.0}% used", peak * 100.0), TextRole::Label, if peak >= 0.5 { Tone::Warn } else { Tone::Muted }),
        ];
        let body = || {
            let accounts = column(provider.accounts.iter().map(|account| self.account(account, now)));
            let capacity = provider.capacity.as_deref().map(|capacity| label(capacity, TextRole::Caption, Tone::Faint));
            accounts.push(capacity).spacing(sp(SpaceRole::S)).into()
        };
        let head = collapsible(title, self.open.contains(&provider.id), Message::Toggle(provider.id.clone()), body);
        column![summary.spacing(sp(SpaceRole::S)).align_y(Alignment::Center), head].spacing(sp(SpaceRole::Xs)).into()
    }

    fn account<'a>(&'a self, account: &'a Account, now: i64) -> Element<'a, Message> {
        let (left, usable) = account.resets;
        let resets = match (left, usable) {
            (0, _) => "No saved resets".to_owned(),
            (1, 0) => "1 saved reset".to_owned(),
            (n, 0) => format!("{n} saved resets"),
            (n, usable) => format!("{n} saved resets, {usable} usable now"),
        };
        let spend = account.reset_target.as_ref().filter(|_| usable > 0).map(|target| {
            let busy = self.spending.as_deref() == Some(target.as_str());
            text_button(if busy { "Using…" } else { "Use a reset" }, ButtonKind::Secondary, (!busy).then(|| Message::Spend(target.clone())))
        });
        let head = row![label(account.label.as_str(), TextRole::Body, Tone::Normal).width(Fill), label(resets, TextRole::Caption, Tone::Muted)].push(spend);
        let limits = account.limits.iter().map(|limit| {
            let (used, tone) = match limit.fraction {
                Some(fraction) => (format!("{:.0}% used", fraction * 100.0), if fraction >= 0.5 { Tone::Warn } else { Tone::Muted }),
                None => ("Not reported".to_owned(), Tone::Faint),
            };
            let resets = limit.resets_at.filter(|_| limit.fraction.is_some()).map(|at| label(format!("Resets in {}", until(at - now)), TextRole::Caption, Tone::Faint));
            let title = row![label(limit.label.as_str(), TextRole::Label, Tone::Normal).width(Fill), label(used, TextRole::Label, tone)];
            column![title, meter(limit.fraction.unwrap_or(0.0))].push(resets).spacing(sp(SpaceRole::Xs)).into()
        });
        let head = head.spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
        card(column![head].extend(limits).spacing(sp(SpaceRole::M))).width(Fill).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "generatedAt": 1000,
      "reports": [
        {"provider": "anthropic", "metadata": {"orgName": "Personal", "credentialId": "c1"},
         "limits": [
           {"label": "Claude 5 Hour", "window": {"id": "5h", "resetsAt": 5000}, "amount": {"usedFraction": 0.03}},
           {"label": "Claude 7 Day", "window": {"id": "7d", "resetsAt": 9000}, "amount": {"usedFraction": 0.51}}
         ],
         "resetCredits": {"availableCount": 3, "redeemableCount": 1}},
        {"provider": "anthropic", "metadata": {"email": "work@example.com"},
         "limits": [{"label": "Claude 7 Day (Fable)", "window": {"id": "7d"}, "amount": {}}],
         "resetCredits": {"availableCount": 0, "redeemableCount": 0}},
        {"provider": "openai-codex", "metadata": {"email": "me@example.com"},
         "limits": [{"label": "7 days", "window": {"id": "7d"}, "amount": {"usedFraction": 0.05}}]}
      ],
      "capacity": {"anthropic": [{"window": "7d", "accounts": 2, "usedAccounts": 0.7}]}
    }"#;

    #[test]
    fn groups_accounts_by_provider_and_reads_limits() {
        let usage = parse(SAMPLE).expect("parse");
        let names: Vec<_> = usage.providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Anthropic", "OpenAI Codex"]);
        let anthropic = &usage.providers[0];
        assert_eq!(anthropic.accounts.len(), 2);
        assert_eq!(anthropic.accounts[0].label, "Personal");
        assert_eq!(anthropic.accounts[0].reset_target.as_deref(), Some("anthropic/c1"));
        assert_eq!(anthropic.accounts[0].resets, (3, 1));
        assert_eq!(anthropic.accounts[1].limits[0].fraction, None, "unreported limits stay unknown");
        assert_eq!(anthropic.capacity.as_deref(), Some("7d 0.70 of 2 used"));
        assert_eq!(anthropic.peak(), Some(0.51));
        assert_eq!(usage.meter(), Some(UsageMeter { fraction: 0.51, label: "7d".into() }), "the meter takes the tightest limit");
    }
}
