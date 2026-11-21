//! Document mode: the file arrives once and is rendered here with the docs page renderers, so
//! scrolling and zooming never touch the surface.

use super::{Event, Message, Notice, SurfaceMode, TabCtx};
use crate::panel::Options;
use crate::theme;
use iced::widget::{button, column, container, image, row, scrollable, space, text};
use iced::widget::image::Handle;
use iced::{Center, Element, Fill, Size, Task};
use loa_contracts::surface::Document;
use loa_surfaces::docs::{Fonts, Format, Paged, open_pages};
use loa_ui_kit::style::{self, ButtonKind, Surface};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole, pad_x, sp, text_size};
use std::ops::Range;
use std::sync::Arc;

/// Pages past the visible ones that are rendered ahead.
const LOOKAHEAD: usize = 1;

/// Rendered pages further than this from the visible ones are dropped.
const KEEP: usize = 3;

pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 4.0;
pub const ZOOM_STEP: f32 = 1.25;

enum View {
    Loading,
    Image { handle: Handle, size: Size },
    Pages(Pages),
    Failed(String),
}

pub struct DocumentMode {
    view: View,
    fonts: Fonts,
    zoom: f32,
    top: f32,
    viewport: Size,
}

impl DocumentMode {
    pub(super) fn open(tab: &TabCtx, options: &Options) -> (Self, Task<Message>) {
        let source = tab.surface.clone();
        let fetch = Task::future(async move { Message::Document(source.document().await.map(Arc::new).map_err(|e| e.to_string())) });
        let viewport = Size::new(tab.info.width as f32, tab.info.height as f32);
        (Self { view: View::Loading, fonts: options.fonts.clone(), zoom: 1.0, top: 0.0, viewport }, fetch)
    }

    /// Pages in the loaded document, 1 for an image, `None` while loading or when it couldn't
    /// be opened.
    pub fn page_count(&self) -> Option<usize> {
        match &self.view {
            View::Image { .. } => Some(1),
            View::Pages(pages) => Some(pages.sizes.len()),
            View::Loading | View::Failed(_) => None,
        }
    }

    /// Paged documents' pages, `None` until loaded or for images.
    pub fn pages(&self) -> Option<&Pages> {
        match &self.view {
            View::Pages(pages) => Some(pages),
            _ => None,
        }
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    fn open_document(&self, document: &Document) -> Result<View, String> {
        if document.mime.starts_with("image/") {
            let (w, h) = loa_surfaces::pixels::image_size(&document.data).map_err(|e| e.to_string())?;
            return Ok(View::Image { handle: Handle::from_bytes(document.data.clone()), size: Size::new(w as f32, h as f32) });
        }
        let format = Format::from_mime(&document.mime).or_else(|| Format::sniff(&document.data));
        let format = format.ok_or_else(|| format!("no viewer for {}", document.mime))?;
        let paged = open_pages(format, &document.data).map_err(|e| e.to_string())?;
        Pages::new(paged, self.fonts.clone()).map(View::Pages)
    }

    fn page_body(&self) -> Element<'_, Message> {
        let zoomed = |size: Size| (size.width * self.zoom, size.height * self.zoom);
        let content: Element<'_, Message> = match &self.view {
            View::Loading => return container(theme::hint("Loading")).center(Fill).into(),
            View::Failed(e) => return container(theme::warning(e.as_str())).center(Fill).into(),
            View::Image { handle, size } => {
                let (w, h) = zoomed(*size);
                image(handle.clone()).width(w).height(h).into()
            }
            View::Pages(pages) => {
                let items = pages.sizes.iter().enumerate().map(|(i, size)| {
                    let (w, h) = zoomed(*size);
                    match pages.rendered(i) {
                        Some((_, handle)) => image(handle.clone()).width(w).height(h).into(),
                        None => container(space()).width(w).height(h).style(style::container(Surface::Well)).into(),
                    }
                });
                column(items).spacing(page_gap()).into()
            }
        };
        let both = scrollable::Direction::Both { vertical: scrollable::Scrollbar::default(), horizontal: scrollable::Scrollbar::default() };
        scrollable(content)
            .direction(both)
            .style(style::scrollbar(false))
            .on_scroll(|v| Message::DocumentScrolled { top: v.absolute_offset().y, viewport: v.bounds().size() })
            .width(Fill)
            .height(Fill)
            .into()
    }
}

impl SurfaceMode for DocumentMode {
    fn update(&mut self, message: Message, tab: &mut TabCtx) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Document(document) => {
                self.view = match document.and_then(|document| self.open_document(&document)) {
                    Ok(view) => view,
                    Err(e) => {
                        tab.notice = Some(Notice::Error(e.clone()));
                        View::Failed(e)
                    }
                }
            }
            Message::DocumentScrolled { top, viewport } => (self.top, self.viewport) = (top, viewport),
            Message::ZoomIn => self.zoom = (self.zoom * ZOOM_STEP).min(MAX_ZOOM),
            Message::ZoomOut => self.zoom = (self.zoom / ZOOM_STEP).max(MIN_ZOOM),
            Message::ZoomReset => self.zoom = 1.0,
            _ => return (Task::none(), None),
        }
        if let View::Pages(pages) = &mut self.view
            && let Err(e) = pages.refresh(self.zoom, self.top, self.top + self.viewport.height)
        {
            tab.notice = Some(Notice::Error(e));
        }
        (Task::none(), None)
    }

    fn header<'a>(&'a self, _: &'a TabCtx) -> Vec<Element<'a, Message>> {
        let zoom = |label: String, message| {
            let look = style::button(ButtonKind::Row { selected: false });
            button(text(label).size(text_size(TextRole::Label))).on_press(message).padding(pad_x(Sp::S)).style(look)
        };
        let reset = zoom(format!("{:.0}%", self.zoom * 100.0), Message::ZoomReset);
        vec![row![zoom("−".into(), Message::ZoomOut), reset, zoom("+".into(), Message::ZoomIn)].align_y(Center).into()]
    }

    fn view(&self) -> Element<'_, Message> {
        self.page_body()
    }

    fn document(&self) -> Option<&DocumentMode> {
        Some(self)
    }
}

/// Space between pages, in panel pixels.
fn page_gap() -> f32 {
    sp(Sp::S)
}

/// A paged document and its rendered pages, each with the zoom it was rendered at.
pub struct Pages {
    paged: Box<dyn Paged>,
    fonts: Fonts,
    sizes: Vec<Size>,
    rendered: Vec<Option<(f32, Handle)>>,
}

impl Pages {
    fn new(paged: Box<dyn Paged>, fonts: Fonts) -> Result<Self, String> {
        let sizes = (0..paged.page_count())
            .map(|i| paged.page_size(i).map(|s| Size::new(s.width, s.height)).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { paged, fonts, rendered: vec![None; sizes.len()], sizes })
    }

    /// The rendered page `index` and its zoom, if any. It may be from an earlier zoom; the view
    /// scales it until the new render lands.
    pub fn rendered(&self, index: usize) -> Option<&(f32, Handle)> {
        self.rendered.get(index)?.as_ref()
    }

    /// Renders the pages showing in `top..bottom` at `zoom` (plus a page ahead each way) and
    /// drops pages far from view.
    fn refresh(&mut self, zoom: f32, top: f32, bottom: f32) -> Result<(), String> {
        let visible = visible_pages(self.sizes.iter().map(|s| s.height * zoom), page_gap(), top, bottom);
        let wanted = visible.start.saturating_sub(LOOKAHEAD)..(visible.end + LOOKAHEAD).min(self.sizes.len());
        let kept = visible.start.saturating_sub(KEEP)..visible.end + KEEP;
        for (_, slot) in self.rendered.iter_mut().enumerate().filter(|(i, _)| !kept.contains(i)) {
            *slot = None;
        }
        let opts = theme::render_opts();
        for i in wanted {
            if self.rendered[i].as_ref().is_some_and(|(z, _)| *z == zoom) {
                continue;
            }
            let page = self.paged.render(i, zoom, &self.fonts, &opts).map_err(|e| e.to_string())?;
            self.rendered[i] = Some((zoom, Handle::from_rgba(page.width, page.height, page.data)));
        }
        Ok(())
    }
}

/// Pages whose vertical extent meets `top..bottom`, stacked with `gap` between them.
fn visible_pages(heights: impl IntoIterator<Item = f32>, gap: f32, top: f32, bottom: f32) -> Range<usize> {
    let (mut y, mut first, mut end) = (0.0, None, 0);
    for (i, h) in heights.into_iter().enumerate() {
        if y >= bottom {
            break;
        }
        if y + h > top {
            first.get_or_insert(i);
            end = i + 1;
        }
        y += h + gap;
    }
    first.map_or(0..0, |first| first..end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_surfaces::docs::{DocsError, PageSize, RenderOpts, Rgba};
    use std::sync::Mutex;

    /// Pages of 50x100 that record what is rendered.
    struct FakePages(usize, Arc<Mutex<Vec<(usize, f32)>>>);

    impl Paged for FakePages {
        fn page_count(&self) -> usize {
            self.0
        }

        fn page_size(&self, _: usize) -> Result<PageSize, DocsError> {
            Ok(PageSize { width: 50.0, height: 100.0 })
        }

        fn render(&self, index: usize, scale: f32, _: &Fonts, _: &RenderOpts) -> Result<Rgba, DocsError> {
            self.1.lock().unwrap().push((index, scale));
            Ok(Rgba::new((50.0 * scale) as u32, (100.0 * scale) as u32))
        }
    }

    #[test]
    fn visible_pages_respect_gaps() {
        // Pages at 0..100, 108..208, 216..316, ...
        for (top, bottom, want) in [(0.0, 50.0, 0..1), (100.0, 108.0, 0..0), (150.0, 220.0, 1..3), (0.0, 10_000.0, 0..5)] {
            assert_eq!(visible_pages([100.0; 5], 8.0, top, bottom), want, "{top}..{bottom}");
        }
    }

    #[test]
    fn only_pages_near_view_render_and_zoom_rerenders_them() {
        let renders = Arc::new(Mutex::new(Vec::new()));
        let mut pages = Pages::new(Box::new(FakePages(20, renders.clone())), Fonts::none()).unwrap();
        let mut step = |zoom, top, bottom| {
            pages.refresh(zoom, top, bottom).unwrap();
            std::mem::take(&mut *renders.lock().unwrap())
        };
        // Pages 0 and 1 show; page 2 is rendered ahead.
        assert_eq!(step(1.0, 0.0, 150.0), [(0, 1.0), (1, 1.0), (2, 1.0)]);
        // At page 10, earlier pages are not rendered again.
        assert_eq!(step(1.0, 1080.0, 1180.0), [(9, 1.0), (10, 1.0), (11, 1.0)]);
        assert_eq!(step(2.0, 2160.0, 2200.0), [(9, 2.0), (10, 2.0), (11, 2.0)]);
        assert!(pages.rendered(0).is_none(), "far pages are dropped");
        assert_eq!(pages.rendered(10).map(|(zoom, _)| *zoom), Some(2.0));
    }
}
