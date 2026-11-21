//! The unattended settings section: on/off, then per kind the default answer and its timeout.

use super::card::kind_for;
use super::unattended::{Duration, KINDS, Message, TIMEOUTS, Unattended};
use iced::alignment::Vertical;
use iced::widget::{column, pick_list, row, space, text, text_input, toggler};
use iced::{Element, Fill, Length};
use loa_core::DecisionKind;
use loa_ui_kit::style::{self as kit, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, sp, text_size as ts};
use std::fmt;

const CONTROL_WIDTH: f32 = 240.0;
const TIMEOUT_WIDTH: f32 = 96.0;

/// A default answer with its label; `None` waits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preset {
    pub answer: Option<String>,
    label: &'static str,
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.label) }
}

pub fn view(policy: &Unattended) -> Element<'_, Message> {
    let toggle = row![text("Answer waiting prompts after a timeout").size(ts(T::Label)).width(Fill), toggler(policy.enabled()).on_toggle(Message::Enabled).size(ts(T::Heading))];
    let note = "For unattended runs: a prompt nobody answers in time gets the default set for its kind, recorded as answered by timeout. A kind set to wait is never answered for you.";
    let head = column![toggle.spacing(sp(Sp::M)).align_y(Vertical::Center), text(note).size(ts(T::Caption)).style(kit::text(Tone::Muted))];
    head.extend(KINDS.map(|kind| kind_row(policy, kind))).spacing(sp(Sp::M)).into()
}

fn kind_row(policy: &Unattended, kind: DecisionKind) -> Element<'_, Message> {
    let default = policy.0.for_kind(kind);
    let presets = kind_for(kind).presets();
    let control: Element<'_, Message> = if presets.is_empty() {
        let input = text_input("Option or typed answer; empty waits", default.map_or("", |d| d.answer.as_str()));
        let input = input.on_input(move |typed| Message::Answer(kind, (!typed.trim().is_empty()).then_some(typed)));
        input.size(ts(T::Label)).padding([sp(Sp::Xs), sp(Sp::S)]).style(kit::input).width(Length::Fixed(CONTROL_WIDTH)).into()
    } else {
        let wait = Preset { answer: None, label: "Wait for someone" };
        let choices: Vec<Preset> = std::iter::once(wait).chain(presets.into_iter().map(|(answer, label)| Preset { answer: Some(answer.to_owned()), label })).collect();
        let selected = default.map(|d| d.answer.as_str());
        let current = choices.iter().find(|c| c.answer.as_deref() == selected).cloned().unwrap_or(Preset { answer: selected.map(str::to_owned), label: "Value from the config file" });
        let pick = pick_list(choices, Some(current), move |choice: Preset| Message::Answer(kind, choice.answer));
        pick.text_size(ts(T::Label)).style(kit::pick).width(Length::Fixed(CONTROL_WIDTH)).into()
    };
    let timeout: Element<'_, Message> = match default {
        Some(d) => {
            let mut timeouts: Vec<Duration> = TIMEOUTS.map(Duration).to_vec();
            if !TIMEOUTS.contains(&d.timeout_secs) {
                timeouts.push(Duration(d.timeout_secs));
            }
            pick_list(timeouts, Some(Duration(d.timeout_secs)), move |t| Message::Timeout(kind, t)).text_size(ts(T::Label)).style(kit::pick).width(Length::Fixed(TIMEOUT_WIDTH)).into()
        }
        None => space().width(Length::Fixed(TIMEOUT_WIDTH)).into(),
    };
    row![text(kind_for(kind).settings_label()).size(ts(T::Label)).width(Fill), control, timeout].spacing(sp(Sp::M)).align_y(Vertical::Center).into()
}
