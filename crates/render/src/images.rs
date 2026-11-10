//! Image fills: local files decode on first use, remote URLs download in the background.

use skia_safe::{Data, Image};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};

enum State {
    Ready(Image),
    Pending,
    Failed,
}

type Arrival = (String, crate::net::Reply);

pub struct Images {
    cache: HashMap<String, State>,
    tx: Sender<Arrival>,
    rx: Receiver<Arrival>,
    /// Images that couldn't be shown since the last `take_failures`: key and why.
    failures: Vec<(String, String)>,
}

impl Images {
    pub fn new() -> Images {
        let (tx, rx) = channel();
        Images { cache: HashMap::new(), tx, rx, failures: Vec::new() }
    }

    /// `key` is an absolute file path or an http(s) URL.
    pub fn get(&mut self, key: &str) -> Option<Image> {
        if let Some(s) = self.cache.get(key) {
            return match s {
                State::Ready(i) => Some(i.clone()),
                _ => None,
            };
        }
        if key.starts_with("http://") || key.starts_with("https://") {
            self.cache.insert(key.to_string(), State::Pending);
            let tx = self.tx.clone();
            let url = key.to_string();
            crate::net::get(url.clone(), 64 << 20, move |reply| {
                let _ = tx.send((url, reply));
            });
            return None;
        }
        let read = pen::access::check_read(std::path::Path::new(key)).map_err(|e| e.to_string()).and_then(|_| std::fs::read(key).map_err(|_| "not found".to_string()));
        let img = match read {
            Ok(bytes) => decode(&bytes).ok_or_else(|| "not an image study can decode".to_string()),
            Err(e) => Err(e),
        };
        self.settle(key.to_string(), img.clone())
    }

    fn settle(&mut self, key: String, img: Result<Image, String>) -> Option<Image> {
        let out = img.as_ref().ok().cloned();
        match img {
            Ok(i) => {
                self.cache.insert(key, State::Ready(i));
            }
            Err(why) => {
                self.failures.push((key.clone(), why));
                self.cache.insert(key, State::Failed);
            }
        }
        out
    }

    fn arrived(&mut self, (url, reply): Arrival) {
        let img = match reply {
            Ok(bytes) => decode(&bytes).ok_or_else(|| "not an image study can decode".to_string()),
            Err(Some(status)) => Err(format!("download failed (HTTP {status})")),
            Err(None) => Err("download failed (network error)".to_string()),
        };
        self.settle(url, img);
    }

    /// Images that failed to load since the last call: key (path or URL) and the reason.
    pub fn take_failures(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.failures)
    }

    /// Drops a cached image, so a replaced file is read again.
    pub fn forget(&mut self, key: &str) {
        self.cache.remove(key);
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(arrival) = self.rx.try_recv() {
            self.arrived(arrival);
            changed = true;
        }
        changed
    }

    pub fn has_pending(&self) -> bool {
        self.cache.values().any(|s| matches!(s, State::Pending))
    }

    pub fn wait(&mut self, timeout: std::time::Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while self.has_pending() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(arrival) => self.arrived(arrival),
                Err(_) => break,
            }
        }
    }
}

impl Default for Images {
    fn default() -> Self {
        Self::new()
    }
}

/// Decodes PNG/JPEG/GIF/BMP/ICO with Skia, and WebP (which the prebuilt Skia lacks) in Rust.
pub fn decode(bytes: &[u8]) -> Option<Image> {
    if let Some(img) = Image::from_encoded(Data::new_copy(bytes)) {
        return Some(img);
    }
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return None;
    }
    let mut dec = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let (w, h) = dec.dimensions();
    let mut buf = vec![0u8; dec.output_buffer_size()?];
    dec.read_image(&mut buf).ok()?;
    let rgba = if dec.has_alpha() { buf } else { buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect() };
    let info = skia_safe::ImageInfo::new((w as i32, h as i32), skia_safe::ColorType::RGBA8888, skia_safe::AlphaType::Unpremul, None);
    skia_safe::images::raster_from_data(&info, Data::new_copy(&rgba), w as usize * 4)
}
