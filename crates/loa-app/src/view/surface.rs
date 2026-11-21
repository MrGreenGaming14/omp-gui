//! The surface hub's state as the Surfaces launcher shows it, and the capture choice back.

use loa_surfaces::{CaptureMode, Listed, SurfaceHub};
use loa_view::surface as view;

pub fn capture_to_view(capture: CaptureMode) -> view::Capture {
    match capture {
        CaptureMode::Screencast => view::Capture::Screencast,
        CaptureMode::VirtualDisplay => view::Capture::VirtualDisplay,
    }
}

pub fn capture_from_view(capture: view::Capture) -> CaptureMode {
    match capture {
        view::Capture::Screencast => CaptureMode::Screencast,
        view::Capture::VirtualDisplay => CaptureMode::VirtualDisplay,
    }
}

/// What the launcher says about the browser.
pub fn browser_status(hub: &SurfaceHub) -> view::BrowserStatus {
    match hub.browser_executable() {
        Some(executable) => view::BrowserStatus::Ready { executable: executable.to_owned(), sandboxed: hub.browser_sandboxed() },
        None => view::BrowserStatus::Missing,
    }
}

/// What the launcher says about the browser capture.
pub fn capture_status(hub: &SurfaceHub, pending: bool) -> view::CaptureStatus {
    view::CaptureStatus {
        selected: capture_to_view(hub.capture()),
        virtual_display_missing: hub.config().virtual_display.as_ref().err().cloned(),
        pending,
    }
}

/// A listed surface the launcher offers.
pub fn available(listed: Listed) -> view::AvailableSurface {
    view::AvailableSurface { server: listed.origin.server().to_owned(), info: listed.info }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_round_trips() {
        for capture in [CaptureMode::Screencast, CaptureMode::VirtualDisplay] {
            assert_eq!(capture_from_view(capture_to_view(capture)), capture);
        }
    }
}
