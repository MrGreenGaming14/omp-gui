//! Document mode: the file arrives once and is rendered here, so scrolling
//! and zooming never touch the surface stream.

use iced::Size;
use iced::widget::image::Handle;
use loa_surface::Document;
use std::ops::Range;

/// Space between pages, in panel pixels.
pub const PAGE_GAP: f32 = 8.0;

/// Pages past the visible ones that are rendered ahead.
const LOOKAHEAD: usize = 1;

/// Rendered pages further than this from the visible ones are dropped.
const KEEP: usize = 3;

pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 4.0;
pub const ZOOM_STEP: f32 = 1.25;

#[derive(Debug, thiserror::Error)]
pub enum PageError {
    #[error("no viewer for {0}")]
    Unsupported(String),
    #[error("page {0} doesn't exist")]
    NoPage(usize),
    #[error("{0}")]
    Render(String),
}

/// One rendered page, 8 bit RGBA, row major, no padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaPage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Port: renders the pages of one open document (PDF, Office). Backed by the
/// document libraries the app links in.
pub trait PageRenderer: Send {
    fn page_count(&self) -> usize;

    /// Size of page `index` at zoom 1, in panel pixels.
    fn page_size(&self, index: usize) -> Option<Size>;

    /// Renders page `index` at `scale` times its zoom 1 size.
    fn render(&self, index: usize, scale: f32) -> Result<RgbaPage, PageError>;
}

/// Port: opens a [`PageRenderer`] for a document's MIME type.
pub trait PageRenderers: Send + Sync {
    /// `PageError::Unsupported` when no renderer handles `document.mime`.
    fn open(&self, document: &Document) -> Result<Box<dyn PageRenderer>, PageError>;
}

/// Pages whose vertical extent meets `top..bottom`, for pages stacked with
/// `gap` between them.
pub fn visible_pages(
    heights: impl IntoIterator<Item = f32>,
    gap: f32,
    top: f32,
    bottom: f32,
) -> Range<usize> {
    let mut y = 0.0;
    let mut first = None;
    let mut end = 0;
    for (i, h) in heights.into_iter().enumerate() {
        let (page_top, page_bottom) = (y, y + h);
        if page_top >= bottom {
            break;
        }
        if page_bottom > top {
            first.get_or_insert(i);
            end = i + 1;
        }
        y = page_bottom + gap;
    }
    first.map_or(0..0, |first| first..end)
}

/// A rendered page and the zoom it was rendered at.
#[derive(Clone, Debug)]
pub struct Rendered {
    pub zoom: f32,
    pub handle: Handle,
}

/// A paged document and its rendered pages.
pub struct Pages {
    renderer: Box<dyn PageRenderer>,
    sizes: Vec<Size>,
    rendered: Vec<Option<Rendered>>,
}

impl std::fmt::Debug for Pages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pages")
            .field("sizes", &self.sizes)
            .finish_non_exhaustive()
    }
}

impl Pages {
    pub fn new(renderer: Box<dyn PageRenderer>) -> Result<Self, PageError> {
        let sizes = (0..renderer.page_count())
            .map(|i| renderer.page_size(i).ok_or(PageError::NoPage(i)))
            .collect::<Result<Vec<_>, _>>()?;
        let rendered = vec![None; sizes.len()];
        Ok(Self {
            renderer,
            sizes,
            rendered,
        })
    }

    /// Zoom 1 sizes of every page.
    pub fn sizes(&self) -> &[Size] {
        &self.sizes
    }

    /// The rendered picture of page `index`, if any. It may be from an
    /// earlier zoom; the view scales it until the new render lands.
    pub fn rendered(&self, index: usize) -> Option<&Rendered> {
        self.rendered.get(index)?.as_ref()
    }

    /// Renders the pages showing in `top..bottom` at `zoom` (plus a page
    /// ahead each way) and drops pages far from view.
    pub fn refresh(&mut self, zoom: f32, top: f32, bottom: f32) -> Result<(), PageError> {
        let visible = visible_pages(
            self.sizes.iter().map(|s| s.height * zoom),
            PAGE_GAP,
            top,
            bottom,
        );
        let wanted = visible.start.saturating_sub(LOOKAHEAD)
            ..(visible.end + LOOKAHEAD).min(self.sizes.len());
        let kept = visible.start.saturating_sub(KEEP)..visible.end + KEEP;
        for (i, slot) in self.rendered.iter_mut().enumerate() {
            if !kept.contains(&i) {
                *slot = None;
            }
        }
        for i in wanted {
            if self.rendered[i].as_ref().is_some_and(|r| r.zoom == zoom) {
                continue;
            }
            let page = self.renderer.render(i, zoom)?;
            let handle = Handle::from_rgba(page.width, page.height, page.pixels);
            self.rendered[i] = Some(Rendered { zoom, handle });
        }
        Ok(())
    }
}

/// The size of an image document, read from its header.
pub fn image_size(data: &[u8]) -> Result<Size, PageError> {
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| PageError::Render(e.to_string()))?
        .into_dimensions()
        .map_err(|e| PageError::Render(e.to_string()))?;
    Ok(Size::new(w as f32, h as f32))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A renderer with fixed size pages that records what it renders.
    pub(crate) struct FakePages {
        pub pages: usize,
        pub size: Size,
        pub renders: Arc<Mutex<Vec<(usize, f32)>>>,
    }

    impl PageRenderer for FakePages {
        fn page_count(&self) -> usize {
            self.pages
        }

        fn page_size(&self, index: usize) -> Option<Size> {
            (index < self.pages).then_some(self.size)
        }

        fn render(&self, index: usize, scale: f32) -> Result<RgbaPage, PageError> {
            self.renders.lock().unwrap().push((index, scale));
            let (w, h) = (
                (self.size.width * scale) as u32,
                (self.size.height * scale) as u32,
            );
            Ok(RgbaPage {
                width: w,
                height: h,
                pixels: vec![255; w as usize * h as usize * 4],
            })
        }
    }

    #[test]
    fn visible_pages_respect_gaps() {
        let heights = [100.0; 5];
        // Pages at 0..100, 108..208, 216..316, ...
        assert_eq!(visible_pages(heights, 8.0, 0.0, 50.0), 0..1);
        assert_eq!(visible_pages(heights, 8.0, 100.0, 108.0), 0..0);
        assert_eq!(visible_pages(heights, 8.0, 150.0, 220.0), 1..3);
        assert_eq!(visible_pages(heights, 8.0, 0.0, 10_000.0), 0..5);
    }

    #[test]
    fn scrolling_renders_only_pages_near_view() {
        let renders = Arc::new(Mutex::new(Vec::new()));
        let fake = FakePages {
            pages: 20,
            size: Size::new(50.0, 100.0),
            renders: renders.clone(),
        };
        let mut pages = Pages::new(Box::new(fake)).unwrap();
        pages.refresh(1.0, 0.0, 150.0).unwrap();
        // Pages 0 and 1 show; page 2 is rendered ahead.
        assert_eq!(*renders.lock().unwrap(), [(0, 1.0), (1, 1.0), (2, 1.0)]);
        renders.lock().unwrap().clear();
        // Scroll to page 10: earlier pages are not rendered again, far ones dropped.
        pages.refresh(1.0, 1080.0, 1180.0).unwrap();
        assert_eq!(*renders.lock().unwrap(), [(9, 1.0), (10, 1.0), (11, 1.0)]);
        assert!(pages.rendered(0).is_none());
        assert!(pages.rendered(10).is_some());
    }

    #[test]
    fn zoom_rerenders_the_visible_pages_at_the_new_scale() {
        let renders = Arc::new(Mutex::new(Vec::new()));
        let fake = FakePages {
            pages: 3,
            size: Size::new(50.0, 100.0),
            renders: renders.clone(),
        };
        let mut pages = Pages::new(Box::new(fake)).unwrap();
        pages.refresh(1.0, 0.0, 50.0).unwrap();
        renders.lock().unwrap().clear();
        pages.refresh(2.0, 0.0, 50.0).unwrap();
        assert_eq!(*renders.lock().unwrap(), [(0, 2.0), (1, 2.0)]);
        assert_eq!(pages.rendered(0).unwrap().zoom, 2.0);
    }
}
