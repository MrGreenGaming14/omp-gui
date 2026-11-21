//! The Usage window: provider limits and saved resets from `omp usage --json`,
//! the same data omp's `/usage` prints.
//!
//! Providers start collapsed, each summarized by its tightest limit; a click
//! expands its accounts. The tightest limit across providers also feeds the
//! sidebar's usage meter.

use loa_util::time::{ago, now_ms, until};
use std::collections::BTreeSet;

use iced::widget::{Column, button, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Fill, Length, Task};
use loa_ui_icons::{Icon, muted};
use loa_ui_shell::UsageMeter;
use loa_ui_shell::theme::style;
use serde_json::Value;

/// Width of the window.
const WIDTH: f32 = 660.0;

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

/// A provider with its accounts.
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

/// A usage snapshot.
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
    let generated_at = root["generatedAt"].as_i64().unwrap_or_default();
    let mut providers: Vec<Provider> = Vec::new();
    for report in root["reports"].as_array().into_iter().flatten() {
        let id = report["provider"].as_str().unwrap_or("unknown").to_owned();
        let metadata = &report["metadata"];
        let label = ["orgName", "email", "accountId"]
            .iter()
            .find_map(|key| metadata[*key].as_str().filter(|value| !value.is_empty()))
            .unwrap_or("Account")
            .to_owned();
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
    if let Some(capacity) = root["capacity"].as_object() {
        for provider in &mut providers {
            let Some(windows) = capacity.get(&provider.id).and_then(Value::as_array) else { continue };
            let parts: Vec<String> = windows
                .iter()
                .map(|window| {
                    let name = window["window"].as_str().unwrap_or("");
                    let meter = window["meter"].as_str().map(|meter| format!(" {meter}")).unwrap_or_default();
                    let used = window["usedAccounts"].as_f64().unwrap_or(0.0);
                    let accounts = window["accounts"].as_u64().unwrap_or(0);
                    format!("{name}{meter} {used:.2} of {accounts} used")
                })
                .collect();
            if !parts.is_empty() {
                provider.capacity = Some(parts.join(" · "));
            }
        }
    }
    Ok(Usage { generated_at, providers })
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

/// Runs `omp usage --json`.
pub async fn fetch() -> Result<Usage, String> {
    let output = tokio::process::Command::new("omp")
        .args(["usage", "--json"])
        .output()
        .await
        .map_err(|error| format!("Couldn't run omp usage: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("omp usage failed: {}", stderr.trim()));
    }
    parse(&String::from_utf8_lossy(&output.stdout))
}

/// Spends a saved reset with `omp usage reset <target>`.
pub async fn spend(target: String) -> Result<(), String> {
    let output = tokio::process::Command::new("omp")
        .args(["usage", "reset", &target])
        .output()
        .await
        .map_err(|error| format!("Couldn't run omp usage reset: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("omp usage reset failed: {}", String::from_utf8_lossy(&output.stderr).trim()))
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

#[derive(Debug, Default)]
pub struct State {
    usage: Option<Result<Usage, String>>,
    loading: bool,
    open: BTreeSet<String>,
    spending: Option<String>,
}

impl State {
    /// Starts a fetch.
    pub fn refresh(&mut self) -> Task<Message> {
        self.loading = true;
        Task::perform(fetch(), Message::Loaded)
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
                (Task::perform(spend(target), Message::Spent), None)
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

    /// Draws the window.
    pub fn view(&self) -> Element<'_, Message> {
        let now = now_ms();
        let updated = match &self.usage {
            _ if self.loading => "Updating…".to_owned(),
            Some(Ok(usage)) => format!("Updated {}", ago(now, usage.generated_at)),
            _ => String::new(),
        };
        let header = row![
            text("Usage").size(18).font(semibold()).width(Fill),
            text(updated).size(12).style(style::faint),
            button(text("Refresh").size(13)).on_press(Message::Refresh).style(style::ghost_button).padding([5, 10]),
            button(muted(Icon::X, 15.0)).on_press(Message::Close).style(style::ghost_button).padding(6),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let mut body = Column::new();
        match &self.usage {
            None => body = body.push(container(text("Reading usage from omp…").style(style::muted)).padding(20)),
            Some(Err(error)) => body = body.push(container(text(error.as_str()).style(style::muted)).padding(20)),
            Some(Ok(usage)) if usage.providers.is_empty() => {
                body = body.push(container(text("No signed-in providers report usage.").style(style::muted)).padding(20))
            }
            Some(Ok(usage)) => {
                for provider in &usage.providers {
                    body = body.push(iced::widget::rule::horizontal(1).style(style::divider));
                    body = body.push(self.provider(provider, now));
                }
            }
        }
        body = body.push(iced::widget::rule::horizontal(1).style(style::divider));
        body = body.push(
            container(text("Same numbers as omp’s /usage. Resets apply to an account, not a session.").size(12).style(style::faint))
                .padding([12, 20]),
        );

        container(column![container(header).padding([14, 14]), scrollable(body).height(Length::Shrink)])
            .style(style::window)
            .width(WIDTH)
            .max_height(760)
            .into()
    }

    fn provider<'a>(&'a self, provider: &'a Provider, now: i64) -> Element<'a, Message> {
        let open = self.open.contains(&provider.id);
        let peak = provider.peak().unwrap_or(0.0);
        let accounts = match provider.accounts.len() {
            1 => "1 account".to_owned(),
            n => format!("{n} accounts"),
        };
        let header = button(
            row![
                muted(if open { Icon::ChevronDown } else { Icon::ChevronRight }, 14.0),
                column![text(provider.name.as_str()).size(14).font(semibold()), text(accounts).size(12).style(style::faint)]
                    .width(Fill),
                meter(peak, 90.0),
                container(text!("Highest {:.0}% used", peak * 100.0).size(13).style(if peak >= 0.5 { style::waiting_text } else { style::muted }))
                    .width(130)
                    .align_right(130),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .on_press(Message::Toggle(provider.id.clone()))
        .style(style::row)
        .padding([10, 14])
        .width(Fill);

        let mut section = column![header].spacing(8);
        if open {
            let mut accounts = Column::new().spacing(10);
            for account in &provider.accounts {
                accounts = accounts.push(self.account(account, now));
            }
            if let Some(capacity) = &provider.capacity {
                accounts = accounts.push(text(capacity.as_str()).size(12).style(style::faint));
            }
            section = section.push(container(accounts).padding([0, 20]));
        }
        container(section).padding([4, 6]).into()
    }

    fn account<'a>(&'a self, account: &'a Account, now: i64) -> Element<'a, Message> {
        let (left, now_usable) = account.resets;
        let resets = match (left, now_usable) {
            (0, _) => "No saved resets".to_owned(),
            (1, 0) => "1 saved reset".to_owned(),
            (n, 0) => format!("{n} saved resets"),
            (n, usable) => format!("{n} saved resets, {usable} usable now"),
        };
        let mut head = row![text(account.label.as_str()).size(14).width(Fill), text(resets).size(12).style(style::muted)]
            .spacing(10)
            .align_y(Alignment::Center);
        if now_usable > 0
            && let Some(target) = &account.reset_target
        {
            let busy = self.spending.as_deref() == Some(target.as_str());
            head = head.push(
                button(text(if busy { "Using…" } else { "Use a reset" }).size(12))
                    .on_press_maybe((!busy).then(|| Message::Spend(target.clone())))
                    .style(style::secondary_button)
                    .padding([4, 10]),
            );
        }
        let mut limits = Column::new().spacing(12);
        for limit in &account.limits {
            let (pct, color): (String, fn(&iced::Theme) -> text::Style) = match limit.fraction {
                Some(fraction) => (format!("{:.0}% used", fraction * 100.0), if fraction >= 0.5 { style::waiting_text } else { style::muted }),
                None => ("Not reported".to_owned(), style::faint),
            };
            let mut item = column![
                row![text(limit.label.as_str()).size(13).width(Fill), text(pct).size(13).style(color)],
                meter(limit.fraction.unwrap_or(0.0), f32::INFINITY),
            ]
            .spacing(5);
            if let (Some(at), Some(_)) = (limit.resets_at, limit.fraction) {
                item = item.push(text!("Resets in {}", until(at - now)).size(12).style(style::faint));
            }
            limits = limits.push(item);
        }
        container(column![head, limits].spacing(12)).style(style::card).padding(14).width(Fill).into()
    }
}

fn meter<'a>(fraction: f32, width: f32) -> Element<'a, Message> {
    let fraction = fraction.clamp(0.0, 1.0);
    let track = |w: Length| container(space()).width(w).height(6).style(style::meter_track);
    let fill = container(space()).width(Length::FillPortion(((fraction * 1000.0) as u16).max(if fraction > 0.0 { 10 } else { 0 })))
        .height(6)
        .style(style::meter_fill(fraction));
    let rest = space().width(Length::FillPortion(1000 - ((fraction * 1000.0) as u16).min(1000)));
    let bar = iced::widget::stack![track(Length::Fill), row![fill, rest]];
    let width = if width.is_finite() { Length::Fixed(width) } else { Length::Fill };
    container(bar).width(width).height(6).into()
}

fn semibold() -> iced::Font {
    iced::Font { weight: iced::font::Weight::Bold, ..iced::Font::default() }
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
    }

    #[test]
    fn meter_takes_the_tightest_limit() {
        let usage = parse(SAMPLE).expect("parse");
        assert_eq!(usage.meter(), Some(UsageMeter { fraction: 0.51, label: "7d".into() }));
    }

    #[test]
    fn providers_start_collapsed_and_toggle() {
        let mut state = State::default();
        let _ = state.update(Message::Loaded(parse(SAMPLE)));
        assert!(state.open.is_empty());
        let _ = state.update(Message::Toggle("anthropic".into()));
        assert!(state.open.contains("anthropic"));
        let _ = state.update(Message::Toggle("anthropic".into()));
        assert!(state.open.is_empty());
    }
}
