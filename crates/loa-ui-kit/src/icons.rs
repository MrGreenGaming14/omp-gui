//! Lucide line icons (ISC, see `LICENSE-lucide`; the SVGs under `icons/` are copied from
//! `lucide-static`) as iced SVG widgets tinted by tone. Handles are built once, indexed by
//! discriminant, so drawing an icon never re-hashes its SVG.

use crate::style::Tone;
use crate::theme::{TextRole, for_theme};
use iced::widget::svg::{self, Handle, Svg};
use iced::{Color, Length, Theme};
use std::sync::LazyLock;

/// Declares [`Icon`] and its SVG table in one list.
macro_rules! icons {
    ($($name:ident = $file:literal),+ $(,)?) => {
        /// An icon the app uses.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon { $($name),+ }
        const SVGS: &[&[u8]] = &[$(include_bytes!(concat!("../icons/", $file, ".svg"))),+];
    };
}

icons! {
    ArrowUp = "arrow-up", Bot = "bot", Brain = "brain", Check = "check", ChevronDown = "chevron-down",
    ChevronLeft = "chevron-left", ChevronRight = "chevron-right", ChevronUp = "chevron-up", CircleDot = "circle-dot",
    Cpu = "cpu", ExternalLink = "external-link", FileText = "file-text", Files = "files", Folder = "folder",
    Gauge = "gauge", GitCompare = "git-compare", GitFork = "git-fork", Globe = "globe", Maximize = "maximize-2",
    Minimize = "minimize-2", Monitor = "monitor", PanelRight = "panel-right", PanelRightClose = "panel-right-close",
    Pin = "pin", Plus = "plus", Search = "search", Settings = "settings-2", Shield = "shield",
    SlidersHorizontal = "sliders-horizontal", Sparkles = "sparkles", Square = "square", Terminal = "terminal",
    TriangleAlert = "triangle-alert", Wrench = "wrench", X = "x",
}

static HANDLES: LazyLock<Vec<Handle>> = LazyLock::new(|| SVGS.iter().map(|b| Handle::from_memory(*b)).collect());

impl Icon {
    pub fn handle(self) -> Handle { HANDLES[self as usize].clone() }
}

/// `icon` sized like `size` text, in `tone`.
pub fn tinted<'a>(icon: Icon, size: TextRole, tone: Tone) -> Svg<'a, Theme> {
    sized(icon, crate::theme::tokens().text(size)).style(crate::style::tint(tone))
}

/// `icon` at `size` logical pixels, in the text colour.
pub fn icon<'a>(icon: Icon, size: f32) -> Svg<'a, Theme> { sized(icon, size).style(crate::style::tint(Tone::Normal)) }

/// `icon` at `size` in the text colour at the muted alpha, for chevrons and row icons.
pub fn muted<'a>(icon: Icon, size: f32) -> Svg<'a, Theme> {
    sized(icon, size).style(|theme: &Theme, _| {
        let t = for_theme(theme);
        svg::Style { color: Some(t.color(Tone::Normal.role()).scale_alpha(t.alpha(crate::theme::AlphaRole::Muted))) }
    })
}

/// `icon` at `size` in a fixed colour.
pub fn colored<'a>(icon: Icon, size: f32, color: Color) -> Svg<'a, Theme> { sized(icon, size).style(move |_: &Theme, _| svg::Style { color: Some(color) }) }

fn sized<'a>(icon: Icon, size: f32) -> Svg<'a, Theme> { Svg::new(icon.handle()).width(Length::Fixed(size)).height(Length::Fixed(size)) }
