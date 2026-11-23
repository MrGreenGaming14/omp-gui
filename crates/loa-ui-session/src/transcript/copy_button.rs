use iced::widget::{button, text};
use iced::Element;
use loa_ui_kit::style::{self as kit, ButtonKind};
use loa_ui_kit::theme::{SpaceRole as Sp, sp};
use loa_ui_kit::theme::{TextRole as T, text_size};

use super::Message;

pub(super) fn copy_button<'a>(label: &'static str, value: &'a str) -> Element<'a, Message> {
    copy_button_with(label, move || value.to_owned())
}

pub(super) fn copy_button_with<'a>(label: &'static str, value: impl Fn() -> String + 'a) -> Element<'a, Message> {
    button(text(label).size(text_size(T::Caption)))
        .style(kit::button(ButtonKind::Ghost))
        .padding([sp(Sp::Xs), sp(Sp::S)])
        .on_press_with(move || Message::Copy(value()))
        .into()
}
