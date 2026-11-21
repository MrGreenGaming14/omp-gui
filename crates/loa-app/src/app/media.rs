//! Images and files that reach the composer from outside it: a paste whose clipboard holds an
//! image, and files dropped on the window.

use std::path::{Path, PathBuf};

use iced::Task;
use loa_ui_session::composer;

use super::{App, Message};

/// A pasted image saved to disk, with its size in pixels.
#[derive(Clone, Debug)]
pub struct Pasted {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
}

impl App {
    /// Ctrl+V in the composer: when the clipboard holds an image, saves it under
    /// `<data>/pasted` and attaches it. The editor pastes any text itself.
    pub(super) fn paste_image(&mut self) -> Task<Message> {
        let dir = self.ctx.services.storage.dirs.data.join("pasted");
        Task::perform(
            async move { tokio::task::spawn_blocking(move || read_clipboard_image(&dir)).await.unwrap_or_else(|error| Err(error.to_string())) },
            Message::ImagePasted,
        )
    }

    pub(super) fn image_pasted(&mut self, pasted: Result<Option<Pasted>, String>) -> Task<Message> {
        match pasted {
            Ok(Some(Pasted { path, width, height })) => self.ctx.composer.attach_image(composer::pasted(path, Some((width, height)))).map(Message::Composer),
            Ok(None) => Task::none(),
            Err(error) => {
                self.ctx.notify(format!("Can't paste the image: {error}"));
                Task::none()
            }
        }
    }

    /// A file dropped on the window: an image is attached with its card, anything else is
    /// mentioned by path.
    pub(super) fn file_dropped(&mut self, path: PathBuf) -> Task<Message> {
        let task = if composer::is_image(&path) {
            let size = image::image_dimensions(&path).ok();
            self.ctx.composer.attach_image(composer::pasted(path, size))
        } else {
            self.ctx.composer.mention_path(&path.to_string_lossy())
        };
        task.map(Message::Composer)
    }
}

/// The clipboard's image saved as a PNG in `dir`; `None` when it holds none.
fn read_clipboard_image(dir: &Path) -> Result<Option<Pasted>, String> {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        // No clipboard to read images from here: the editor's text paste is all there is.
        return Ok(None);
    };
    let image = match clipboard.get_image() {
        Ok(image) => image,
        Err(arboard::Error::ContentNotAvailable | arboard::Error::ConversionFailure) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let (width, height) = (u32::try_from(image.width).map_err(|e| e.to_string())?, u32::try_from(image.height).map_err(|e| e.to_string())?);
    let pixels = image::RgbaImage::from_raw(width, height, image.bytes.into_owned()).ok_or("the clipboard image has the wrong size")?;
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let path = dir.join(format!("{}.png", loa_runtime::session::new_session_id()));
    pixels.save_with_format(&path, image::ImageFormat::Png).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(Some(Pasted { path, width, height }))
}
