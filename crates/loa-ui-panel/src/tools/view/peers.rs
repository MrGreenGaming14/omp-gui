//! The peers section: which tools each peer's grant allows.

use super::{card, clipped, note_row, row_pad};
use crate::tools::{Message, PeerChoice, State, ToolId};
use iced::widget::{column, container, pick_list, row, text};
use iced::{Alignment, Element, Fill, Padding};
use loa_ui_kit::style::{self, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole as Sp, TextRole as T, sp, text_size as ts, width};
use loa_ui_kit::widgets::label;

impl State {
    pub(super) fn peers_view(&self) -> Element<'_, Message> {
        let choices: Vec<PeerChoice> = std::iter::once(PeerChoice::All).chain(self.peers.iter().map(|p| PeerChoice::Peer { key: p.key.clone(), name: p.name.clone() })).collect();
        let selected = choices.iter().find(|c| match (c, &self.peer_filter) {
            (PeerChoice::All, None) => true,
            (PeerChoice::Peer { key, .. }, Some(k)) => key == k,
            _ => false,
        }).cloned();
        let pick = pick_list(choices, selected, Message::PeerFilter).text_size(ts(T::Caption)).padding([sp(Sp::Xs), sp(Sp::S)]).style(style::pick);
        let header = row![label("Tools each peer's grant allows", T::Caption, Tone::Muted).width(Fill), pick].spacing(sp(Sp::S)).align_y(Alignment::Center);
        let matrix = self.peer_matrix();
        if matrix.peers.is_empty() { return column![header, card([note_row("No peers")])].spacing(sp(Sp::S)).into() }
        let column_width = width(LayoutRole::LabelColumn) / 2.0;
        let head = row![label("Tool", T::Caption, Tone::Muted).width(Fill)].extend(matrix.peers.iter().map(|p| label(p.name.as_str(), T::Caption, Tone::Muted).wrapping(text::Wrapping::None).width(column_width).into()));
        let rows = matrix.rows.into_iter().map(|line| {
            let name = match &line.tool {
                ToolId::Builtin(name) => name.clone(),
                ToolId::Mcp { server, tool } => format!("{server} / {tool}"),
            };
            let cells = line.allowed.into_iter().map(|a| if a { label("allowed", T::Caption, Tone::Ok) } else { label("no", T::Caption, Tone::Faint) }.width(column_width).into());
            container(row![clipped(label(name, T::Caption, Tone::Normal).wrapping(text::Wrapping::None))].extend(cells).spacing(sp(Sp::S))).padding(Padding::from([sp(Sp::Xs), sp(Sp::M)])).width(Fill).into()
        });
        let mut items = vec![container(head.spacing(sp(Sp::S))).padding(row_pad()).width(Fill).into()];
        items.extend(rows);
        if items.len() == 1 { items.push(note_row("No tools")) }
        column![header, card(items)].spacing(sp(Sp::S)).into()
    }
}
