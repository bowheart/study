//! Image fills: local files decode on first use, remote URLs download in the background.

use skia_safe::{Data, Image};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};

enum State {
    Ready(Image),
    Pending,
    Failed,
}

pub struct Images {
    cache: HashMap<String, State>,
    tx: Sender<(String, Option<Vec<u8>>)>,
    rx: Receiver<(String, Option<Vec<u8>>)>,
}

impl Images {
    pub fn new() -> Images {
        let (tx, rx) = channel();
        Images { cache: HashMap::new(), tx, rx }
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
            std::thread::spawn(move || {
                let data = ureq::get(&url)
                    .call()
                    .ok()
                    .and_then(|mut r| r.body_mut().with_config().limit(64 << 20).read_to_vec().ok());
                let _ = tx.send((url, data));
            });
            return None;
        }
        let img = std::fs::read(key).ok().and_then(|b| Image::from_encoded(Data::new_copy(&b)));
        let out = img.clone();
        self.cache.insert(key.to_string(), img.map(State::Ready).unwrap_or(State::Failed));
        out
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((url, data)) = self.rx.try_recv() {
            let img = data.and_then(|b| Image::from_encoded(Data::new_copy(&b)));
            self.cache.insert(url, img.map(State::Ready).unwrap_or(State::Failed));
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
                Ok((url, data)) => {
                    let img = data.and_then(|b| Image::from_encoded(Data::new_copy(&b)));
                    self.cache.insert(url, img.map(State::Ready).unwrap_or(State::Failed));
                }
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
