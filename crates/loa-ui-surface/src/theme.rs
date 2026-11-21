//! The Surfaces tab's looks, read from kit tokens, and the [`SurfaceKindStyle`] of each provider.

use iced::widget::{Button, TextInput, button, container, svg::Svg, text, text_input};
use iced::{Element, Fill, Padding, Theme};
use loa_ui_kit::icons::tinted;
use loa_view::surface::port::DocumentPalette;
use loa_view::surface::{DOCUMENT_SERVER, SurfaceMode};
use loa_view::theme::Icon;
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{ColorRole as C, SpaceRole as Sp, TextRole, Variant, builtin_tokens, pad_x, sp, text_size, tokens};
use loa_ui_kit::widgets::label;

/// Provider names of the built-in surface servers.
pub const BROWSER: &str = "browser";
pub const VM: &str = "vm";
pub const MCP: &str = "mcp";

/// How surfaces of one provider are drawn and titled; looked up by provider name, so a theme
/// can remap it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SurfaceKindStyle {
    pub icon: Icon,
    /// The launcher section that opens this kind.
    pub title: &'static str,
}

const KINDS: [(&str, SurfaceKindStyle); 4] = [
    (BROWSER, SurfaceKindStyle { icon: Icon::Globe, title: "Browser" }),
    (DOCUMENT_SERVER, SurfaceKindStyle { icon: Icon::FileText, title: "Document" }),
    (VM, SurfaceKindStyle { icon: Icon::Monitor, title: "VM or desktop (VNC)" }),
    (MCP, SurfaceKindStyle { icon: Icon::Wrench, title: "From MCP servers" }),
];

impl SurfaceKindStyle {
    /// The style of `provider`'s surfaces; providers without one (MCP servers) go by `mode`.
    pub fn of(provider: &str, mode: SurfaceMode) -> Self {
        KINDS.iter().find(|(name, _)| *name == provider).map_or_else(
            || match mode {
                SurfaceMode::Document => KINDS[1].1,
                SurfaceMode::Pixel => SurfaceKindStyle { icon: Icon::Square, title: "" },
            },
            |(_, style)| *style,
        )
    }
}

/// Row and field text.
pub fn body<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    label(s, TextRole::Label, Tone::Normal)
}

/// Hints and captions.
pub fn hint<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    label(s, TextRole::Caption, Tone::Muted)
}

/// Problems the user can act on.
pub fn warning<'a>(s: impl text::IntoFragment<'a>) -> text::Text<'a> {
    label(s, TextRole::Caption, Tone::Warn)
}

/// A one-line badge, e.g. the codec and latency of a pixel tab.
pub fn chip<'a, M: 'a>(s: impl text::IntoFragment<'a>, tone: Tone) -> Element<'a, M> {
    let text = label(s, TextRole::Caption, tone).wrapping(text::Wrapping::None);
    container(text).padding(pad_x(Sp::Xs)).style(style::container(Surface::Badge)).into()
}

/// A muted icon at body size.
pub fn icon<'a>(icon: Icon) -> Svg<'a, Theme> {
    tinted(icon, TextRole::Body, Tone::Muted)
}

/// Padding of fields and form buttons.
pub fn field_padding() -> Padding {
    Padding::from([sp(Sp::Xs), sp(Sp::S)])
}

pub fn field<'a, M: Clone + 'a>(placeholder: &str, value: &str, on_input: impl Fn(String) -> M + 'a) -> TextInput<'a, M> {
    text_input(placeholder, value).on_input(on_input).size(text_size(TextRole::Label)).padding(field_padding()).style(style::input)
}

/// The action button of a form.
pub fn action<'a, M: Clone + 'a>(s: &'a str, on: Option<M>) -> Button<'a, M> {
    button(text(s).size(text_size(TextRole::Label))).on_press_maybe(on).padding([sp(Sp::Xs), sp(Sp::M)]).style(style::button(ButtonKind::Accent))
}

/// One option of a choice row, highlighted while `selected`.
pub fn choice<'a, M: Clone + 'a>(s: &'a str, selected: bool, on: Option<M>) -> Button<'a, M> {
    let look = style::button(ButtonKind::Row { selected });
    button(text(s).size(text_size(TextRole::Caption))).on_press_maybe(on).padding(pad_x(Sp::S)).style(look)
}

/// A full-width list row.
pub fn list_row<'a, M: Clone + 'a>(content: impl Into<Element<'a, M>>, on: M) -> Button<'a, M> {
    button(content).on_press(on).width(Fill).padding(field_padding()).style(style::button(ButtonKind::Row { selected: false }))
}

/// How document pages are drawn: in the light token set in either theme, since documents are
/// paper like the PDF pages beside them.
pub fn document_palette() -> DocumentPalette {
    let active = tokens();
    let t = if active.is_dark() { builtin_tokens(Variant::Light) } else { active };
    let c = |role| t.color(role).into_rgba8();
    DocumentPalette { page: c(C::Card), text: c(C::Text), rule: c(C::Text3), grid: c(C::Line), header: c(C::Hover), header_text: c(C::Text2) }
}
