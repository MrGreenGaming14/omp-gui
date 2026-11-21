//! The registry of the right panel panes the app owns: each is a kit `PanelPane` on the app,
//! named, iconed and drawn by itself, its messages lifted into the app's. Area panes follow
//! through their areas.

use super::{App, Message};
use crate::area::PanelTab;
use iced::Element;
use loa_ui_kit::widgets::PanelPane;

/// A pane the registry holds.
pub(super) trait Pane {
    fn tab(&self, app: &App) -> PanelTab;
    fn view<'a>(&self, app: &'a App) -> Element<'a, Message>;
}

/// A pane state on the app and how its messages reach the app.
struct Owned<P: PanelPane + 'static> {
    get: fn(&App) -> &P,
    lift: fn(P::Message) -> Message,
}

impl<P: PanelPane + 'static> Pane for Owned<P> {
    fn tab(&self, app: &App) -> PanelTab { PanelTab::of((self.get)(app), true) }
    fn view<'a>(&self, app: &'a App) -> Element<'a, Message> { (self.get)(app).view().map(self.lift) }
}

/// The diff, files and terminal panes, in tab order.
pub(super) const REGISTRY: &[&dyn Pane] = &[&Owned { get: |a| &a.diff, lift: Message::Diff }, &Owned { get: |a| &a.files, lift: Message::Files }, &Owned { get: |a| &a.terminal, lift: Message::Terminal }];

/// The registered pane whose tab is `id`.
pub(super) fn find(app: &App, id: &str) -> Option<&'static dyn Pane> { REGISTRY.iter().copied().find(|p| p.tab(app).id == id) }
