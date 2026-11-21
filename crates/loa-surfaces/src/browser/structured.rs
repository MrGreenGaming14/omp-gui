//! Structured reads of a page for the agent: URL and title, readable text,
//! and an outline of the interactive elements.

use crate::browser::{BrowserError, Result};
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::accessibility::{AxNode, GetFullAxTreeParams};
use chromiumoxide::cdp::browser_protocol::dom_snapshot::{CaptureSnapshotParams, DocumentSnapshot};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Accessibility roles the agent can act on.
const INTERACTIVE_ROLES: [&str; 18] = [
    "button",
    "checkbox",
    "combobox",
    "link",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "radio",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "textbox",
    "treeitem",
    "MenuListPopup",
];

/// The current page location and the title shown in its browser tab.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageInfo {
    pub url: String,
    pub title: String,
}

/// A box in surface coordinates (pixels from the top left of the surface).
/// Parts outside the surface are scrolled out of view.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Bounds {
    /// The point a click on this element should go to.
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

/// One interactive element: its accessibility role and name and where it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutlineNode {
    pub role: String,
    pub name: String,
    pub bounds: Bounds,
    /// Whether any part of it is on the surface right now.
    pub visible: bool,
}

pub(crate) async fn page_info(page: &Page) -> Result<PageInfo> {
    let (url, title) = futures::try_join!(page.url(), page.get_title())?;
    Ok(PageInfo { url: url.unwrap_or_default(), title: title.unwrap_or_default() })
}

/// The rendered text of the page as a reader sees it: no markup, scripts,
/// styles or hidden elements.
pub(crate) async fn page_text(page: &Page) -> Result<String> {
    let text = page
        .evaluate_expression("document.body ? document.body.innerText : ''")
        .await?
        .into_value::<String>()
        .map_err(|e| BrowserError::Script(e.to_string()))?;
    Ok(text)
}

/// Interactive elements of the main frame in document order, from Chrome's
/// accessibility tree, placed with layout boxes from a DOM snapshot.
pub(crate) async fn outline(page: &Page, surface: (u32, u32)) -> Result<Vec<OutlineNode>> {
    let ax = page.execute(GetFullAxTreeParams::default()).await?.result.nodes;
    // The generated CDP request omits an empty computedStyles vector, but
    // Chrome requires the field even when the caller only needs layout boxes.
    let snapshot = page.execute(CaptureSnapshotParams::new(vec!["display".into()])).await?.result;
    let Some(document) = snapshot.documents.first() else {
        return Ok(Vec::new());
    };
    let boxes = layout_boxes(document);
    Ok(ax.iter().filter_map(|node| outline_node(node, &boxes, surface)).collect())
}

/// Layout box of every laid out node by backend node id, in viewport
/// coordinates.
fn layout_boxes(document: &DocumentSnapshot) -> HashMap<i64, Bounds> {
    let (scroll_x, scroll_y) = (document.scroll_offset_x.unwrap_or(0.0), document.scroll_offset_y.unwrap_or(0.0));
    let Some(backend_ids) = document.nodes.backend_node_id.as_ref() else {
        return HashMap::new();
    };
    let layout = &document.layout;
    let mut boxes = HashMap::with_capacity(layout.node_index.len());
    for (node, rect) in layout.node_index.iter().zip(&layout.bounds) {
        let (Some(id), [x, y, width, height]) = (usize::try_from(*node).ok().and_then(|n| backend_ids.get(n)), rect.inner().as_slice())
        else {
            continue;
        };
        // Text runs of an element come later with the same node; keep the element's own box.
        boxes.entry(*id.inner()).or_insert(Bounds {
            x: (x - scroll_x) as f32,
            y: (y - scroll_y) as f32,
            width: *width as f32,
            height: *height as f32,
        });
    }
    boxes
}

fn outline_node(node: &AxNode, boxes: &HashMap<i64, Bounds>, surface: (u32, u32)) -> Option<OutlineNode> {
    if node.ignored {
        return None;
    }
    let role = node.role.as_ref()?.value.as_ref()?.as_str()?;
    if !INTERACTIVE_ROLES.contains(&role) {
        return None;
    }
    let bounds = *boxes.get(node.backend_dom_node_id.as_ref()?.inner())?;
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return None;
    }
    let name = node.name.as_ref().and_then(|n| n.value.as_ref()).and_then(|v| v.as_str()).unwrap_or_default();
    let visible = bounds.x < surface.0 as f32
        && bounds.y < surface.1 as f32
        && bounds.x + bounds.width > 0.0
        && bounds.y + bounds.height > 0.0;
    Some(OutlineNode { role: role.to_owned(), name: name.trim().to_owned(), bounds, visible })
}
