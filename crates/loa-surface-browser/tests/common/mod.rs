//! Shared test helpers: a local browser, an in-process HTTP server, and
//! waiting on frames.

#![allow(dead_code)]

use loa_surface::{Frame, Input, InputEvent, Modifiers, MouseButton, Surface};
use loa_surface_browser::{BrowserConfig, BrowserServer, find_chrome};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast::{self, error::RecvError};

/// Upper bound for anything a test waits on.
pub const PATIENCE: Duration = Duration::from_secs(15);

pub fn chrome() -> Option<PathBuf> {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let found = find_chrome(path.as_deref(), home.as_deref(), false);
    if found.is_none() {
        eprintln!("SKIPPED: no Chrome or Chromium found on PATH or in ~/.cache/ms-playwright");
    }
    found
}

/// A screencast server with tabs of `width` x `height`, or `None` (test
/// skipped) when no browser is installed.
pub async fn server(width: u32, height: u32) -> Option<BrowserServer> {
    let mut config = BrowserConfig::new(chrome()?);
    config.sandbox = false;
    config.width = width;
    config.height = height;
    Some(BrowserServer::launch(config).await.expect("launch chrome"))
}

/// `data:` URL of an HTML document.
pub fn html(body: &str) -> String {
    let mut url = String::from("data:text/html,");
    for byte in body.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~!*'();:@=+$,/?".contains(&byte) {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

/// Serves `pages` (path, HTML) on 127.0.0.1 port 0. Returns the base URL.
pub async fn serve(pages: Vec<(&'static str, &'static str)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let pages = Arc::new(pages);
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let pages = pages.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&buf[..n]),
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let response = match pages.iter().find(|(p, _)| *p == path) {
                    Some((_, body)) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned(),
                };
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    base
}

/// Sends inputs with increasing sequence numbers.
pub struct Driver<'a> {
    pub surface: &'a dyn Surface,
    seq: AtomicU64,
}

impl<'a> Driver<'a> {
    pub fn new(surface: &'a dyn Surface) -> Self {
        Self { surface, seq: AtomicU64::new(0) }
    }

    pub async fn send(&self, input: Input) -> u64 {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        self.surface.input(InputEvent { seq, sent_us: 0, input }).await.expect("input");
        seq
    }

    pub async fn button(&self, x: f32, y: f32, button: MouseButton, pressed: bool) -> u64 {
        self.send(Input::MouseButton { x, y, button, pressed, modifiers: Modifiers::default() }).await
    }

    pub async fn click(&self, x: f32, y: f32) -> u64 {
        self.button(x, y, MouseButton::Left, true).await;
        self.button(x, y, MouseButton::Left, false).await
    }

    pub async fn key(&self, key: &str, code: &str, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.send(Input::Key { key: key.into(), code: code.into(), pressed, modifiers }).await;
        }
    }

    pub async fn text(&self, text: &str) {
        self.send(Input::Text { text: text.into() }).await;
    }
}

/// Next frame, skipping over lag.
pub async fn next_frame(frames: &mut broadcast::Receiver<Arc<Frame>>) -> Arc<Frame> {
    loop {
        match tokio::time::timeout(PATIENCE, frames.recv()).await.expect("no frame in time") {
            Ok(frame) => return frame,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => panic!("frame stream closed"),
        }
    }
}

/// Decodes a JPEG frame to `(width, height, rgba)`.
pub fn decode(frame: &Frame) -> (usize, usize, Vec<u8>) {
    let mut decoder = jpeg_decoder::Decoder::new(frame.data.as_ref());
    let rgb = decoder.decode().expect("frame is a JPEG");
    let info = decoder.info().unwrap();
    assert_eq!(info.pixel_format, jpeg_decoder::PixelFormat::RGB24);
    let rgba = rgb.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    (usize::from(info.width), usize::from(info.height), rgba)
}

/// RGB at `(x, y)` of a decoded frame.
pub fn pixel(decoded: &(usize, usize, Vec<u8>), x: usize, y: usize) -> [u8; 3] {
    let i = (y * decoded.0 + x) * 4;
    [decoded.2[i], decoded.2[i + 1], decoded.2[i + 2]]
}

pub fn close_to(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) < 40)
}
