//! The launcher: what is being opened, then one section per way to open a surface.

use super::{Available, BrowserStatus, Capture, Message, Panel};
use crate::theme::{self, SurfaceKindStyle};
use iced::widget::{column, container, row, scrollable, space};
use iced::{Center, Element, Fill};
use loa_contracts::surface::SurfaceMode;
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole, sp};
use loa_ui_kit::widgets::label;

impl Panel {
    pub(super) fn launcher(&self) -> Element<'_, Message> {
        let mut content = column![].push(self.status()).push(self.browser_section());
        #[cfg(feature = "remote-video")]
        {
            content = content.push(crate::video::live_video_section(&self.video));
        }
        content = content.push(self.document_section()).push(self.vm_section()).push(self.declared_section());
        scrollable(content.spacing(sp(Sp::L)).padding([sp(Sp::M), sp(Sp::L)])).height(Fill).into()
    }

    /// What is being opened, or why opening failed.
    fn status(&self) -> Option<Element<'_, Message>> {
        if let Some(error) = &self.error {
            let error = row![tinted(Icon::TriangleAlert, TextRole::Body, Tone::Danger), label(error, TextRole::Label, Tone::Danger)].spacing(sp(Sp::S));
            return Some(container(error).padding(theme::field_padding()).width(Fill).style(style::container(Surface::ErrorBox)).into());
        }
        self.pending.as_ref().map(|pending| theme::hint(format!("{pending}…")).into())
    }

    fn browser_section(&self) -> Element<'_, Message> {
        let ready = matches!(self.browser, BrowserStatus::Ready { .. });
        let url = theme::field("Search or enter a URL", &self.url, Message::Url);
        let url = if ready { url.on_submit(Message::OpenUrl) } else { url };
        let open = theme::action("Open", (ready && !self.url.trim().is_empty()).then_some(Message::OpenUrl));
        let note = match &self.browser {
            BrowserStatus::Ready { executable, sandboxed } => {
                let sandbox = if *sandboxed { "" } else { ", without Chrome's sandbox (this kernel restricts user namespaces)" };
                let kind = if self.capture.selected == Capture::VirtualDisplay { "Headed" } else { "Headless" };
                theme::hint(format!("{kind} {}{sandbox}", browser_name(executable)))
            }
            BrowserStatus::Missing => theme::warning("No Chrome or Chromium installed. Install one, or run `npx playwright install chromium`."),
        };
        let form = column![row![url, open].spacing(sp(Sp::Xs)).align_y(Center), note, self.capture_choice()];
        section(SurfaceKindStyle::of(theme::BROWSER, SurfaceMode::Pixel), form.spacing(sp(Sp::Xs)))
    }

    /// The two browser captures; the virtual display only where it runs.
    fn capture_choice(&self) -> Element<'_, Message> {
        let missing = self.capture.virtual_display_missing.as_deref();
        let choice = |label, capture, enabled: bool| theme::choice(label, self.capture.selected == capture, enabled.then_some(Message::Capture(capture)));
        let choices = row![
            theme::hint("Capture"),
            choice("Headless screencast", Capture::Screencast, true),
            choice("Headed on a virtual display", Capture::VirtualDisplay, missing.is_none()),
        ];
        let note = match (missing, self.capture.selected, self.capture.pending) {
            (Some(why), _, _) => Some(theme::warning(format!("Virtual display unavailable: {why}"))),
            (None, _, true) => Some(theme::hint("The open browser tabs keep the current capture; new tabs use this one once they close.")),
            (None, Capture::VirtualDisplay, false) => Some(theme::hint("Each tab runs headed Chrome on its own Xvfb display, captured raw.")),
            (None, Capture::Screencast, false) => None,
        };
        column![choices.spacing(sp(Sp::Xs)).align_y(Center)].push(note.map(Element::from)).spacing(sp(Sp::Xs)).into()
    }

    fn document_section(&self) -> Element<'_, Message> {
        let formats = theme::hint("PDF, XLSX, DOCX, PPTX, PNG, JPEG, GIF, WebP and BMP. Pages render here, so scrolling and zoom stay local.");
        let body = column![self.picker.view().map(Message::Picker), formats].spacing(sp(Sp::Xs));
        section(SurfaceKindStyle::of(loa_surfaces::docs::SERVER_NAME, SurfaceMode::Document), body)
    }

    fn vm_section(&self) -> Element<'_, Message> {
        let address = theme::field("host:port, e.g. localhost:5900", &self.address, Message::Address).on_submit(Message::Connect);
        let password = theme::field("Password (if the server asks)", &self.password, Message::Password).on_submit(Message::Connect).secure(true);
        let connect = theme::action("Connect", (!self.address.trim().is_empty()).then_some(Message::Connect));
        let vms: Vec<&Available> = self.available.iter().filter(|a| a.server == theme::VM).collect();
        let form = column![address, row![password, connect].spacing(sp(Sp::Xs)).align_y(Center)];
        let form = form.push((!vms.is_empty()).then(|| available_rows(vms))).spacing(sp(Sp::Xs));
        section(SurfaceKindStyle::of(theme::VM, SurfaceMode::Pixel), form)
    }

    fn declared_section(&self) -> Element<'_, Message> {
        let declared: Vec<&Available> = self.available.iter().filter(|a| a.server != theme::VM).collect();
        let body = if declared.is_empty() {
            theme::hint("No MCP server declares a surface. Surface servers added in Tools show up here.").into()
        } else {
            available_rows(declared)
        };
        section(SurfaceKindStyle::of(theme::MCP, SurfaceMode::Pixel), body)
    }
}

fn available_rows(available: Vec<&Available>) -> Element<'_, Message> {
    column(available.into_iter().map(|available| {
        let info = &available.info;
        let size = if info.width > 0 { format!("{}×{}", info.width, info.height) } else { info.mode.as_str().to_owned() };
        let row = row![theme::body(&info.name), theme::chip(available.server.as_str(), Tone::Faint), space::horizontal(), theme::hint(size)];
        theme::list_row(row.spacing(sp(Sp::S)).align_y(Center), Message::OpenAvailable(info.uri.clone())).into()
    }))
    .into()
}

/// A launcher section: the kind's icon and title over `body`.
pub(crate) fn section<'a>(kind: SurfaceKindStyle, body: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    let title = row![theme::icon(kind.icon), theme::body(kind.title)].spacing(sp(Sp::S)).align_y(Center);
    column![title, body.into()].spacing(sp(Sp::S)).into()
}

/// How the launcher names the browser: Playwright's builds by their kind, others by their
/// executable name.
fn browser_name(executable: &std::path::Path) -> String {
    let path = executable.to_string_lossy();
    if path.contains("ms-playwright/chromium_headless_shell") {
        return "Chromium headless shell (Playwright)".to_owned();
    }
    if path.contains("ms-playwright/chromium") {
        return "Chromium (Playwright)".to_owned();
    }
    executable.file_name().map_or_else(|| path.into_owned(), |n| n.to_string_lossy().into_owned())
}
