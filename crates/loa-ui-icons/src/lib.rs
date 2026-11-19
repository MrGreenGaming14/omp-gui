//! Lucide line icons (ISC, see `LICENSE-lucide`) as iced SVG widgets.
//!
//! Icons are tinted with a theme color, so they follow the dark and light
//! themes like text does. Handles are built once and shared, so drawing an
//! icon never re-hashes its SVG.

use iced::widget::svg::{self, Handle, Svg};
use iced::{Color, Length, Theme};
use std::sync::LazyLock;

/// An icon the app uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    ArrowUp,
    Check,
    ChevronDown,
    ChevronRight,
    ChevronUp,
    CircleDot,
    Cpu,
    FileText,
    Files,
    Folder,
    GitCompare,
    PanelRight,
    PanelRightClose,
    Plus,
    Search,
    SlidersHorizontal,
    Square,
    Terminal,
    TriangleAlert,
    Wrench,
    X,
}

const ALL: [(Icon, &[u8]); 21] = [
    (Icon::ArrowUp, include_bytes!("../icons/arrow-up.svg")),
    (Icon::Check, include_bytes!("../icons/check.svg")),
    (Icon::ChevronDown, include_bytes!("../icons/chevron-down.svg")),
    (Icon::ChevronRight, include_bytes!("../icons/chevron-right.svg")),
    (Icon::ChevronUp, include_bytes!("../icons/chevron-up.svg")),
    (Icon::CircleDot, include_bytes!("../icons/circle-dot.svg")),
    (Icon::Cpu, include_bytes!("../icons/cpu.svg")),
    (Icon::FileText, include_bytes!("../icons/file-text.svg")),
    (Icon::Files, include_bytes!("../icons/files.svg")),
    (Icon::Folder, include_bytes!("../icons/folder.svg")),
    (Icon::GitCompare, include_bytes!("../icons/git-compare.svg")),
    (Icon::PanelRight, include_bytes!("../icons/panel-right.svg")),
    (Icon::PanelRightClose, include_bytes!("../icons/panel-right-close.svg")),
    (Icon::Plus, include_bytes!("../icons/plus.svg")),
    (Icon::Search, include_bytes!("../icons/search.svg")),
    (Icon::SlidersHorizontal, include_bytes!("../icons/sliders-horizontal.svg")),
    (Icon::Square, include_bytes!("../icons/square.svg")),
    (Icon::Terminal, include_bytes!("../icons/terminal.svg")),
    (Icon::TriangleAlert, include_bytes!("../icons/triangle-alert.svg")),
    (Icon::Wrench, include_bytes!("../icons/wrench.svg")),
    (Icon::X, include_bytes!("../icons/x.svg")),
];

static HANDLES: LazyLock<[Handle; ALL.len()]> =
    LazyLock::new(|| ALL.map(|(_, bytes)| Handle::from_memory(bytes)));

impl Icon {
    fn handle(self) -> Handle {
        // `ALL` lists every variant, so the position is always found.
        let index = ALL.iter().position(|(icon, _)| *icon == self).unwrap_or(0);
        HANDLES[index].clone()
    }
}

/// `icon` at `size` logical pixels, in the theme's text color.
pub fn icon<'a>(icon: Icon, size: f32) -> Svg<'a, Theme> {
    sized(icon, size).style(|theme: &Theme, _| svg::Style { color: Some(theme.palette().text) })
}

/// `icon` at `size`, in the text color at reduced opacity, for secondary marks
/// such as chevrons and row icons.
pub fn muted<'a>(icon: Icon, size: f32) -> Svg<'a, Theme> {
    sized(icon, size).style(|theme: &Theme, _| svg::Style { color: Some(theme.palette().text.scale_alpha(0.55)) })
}

/// `icon` at `size` in a fixed color, e.g. a status color from the palette.
pub fn colored<'a>(icon: Icon, size: f32, color: Color) -> Svg<'a, Theme> {
    sized(icon, size).style(move |_: &Theme, _| svg::Style { color: Some(color) })
}

fn sized<'a>(icon: Icon, size: f32) -> Svg<'a, Theme> {
    Svg::new(icon.handle()).width(Length::Fixed(size)).height(Length::Fixed(size))
}
