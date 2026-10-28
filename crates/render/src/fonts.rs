//! Font loading: Google Fonts (downloaded once, cached on disk) with system fallback.

use skia_safe::textlayout::{FontCollection, TypefaceFontProvider};
use skia_safe::FontMgr;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Loaded,
    Pending,
    Missing,
}

type Key = (String, u16, bool);

pub struct Fonts {
    provider: TypefaceFontProvider,
    pub collection: FontCollection,
    mgr: FontMgr,
    state: HashMap<Key, State>,
    tx: Sender<(Key, Option<Vec<u8>>)>,
    rx: Receiver<(Key, Option<Vec<u8>>)>,
    pending: usize,
    /// Bumped whenever new typefaces become available, so callers can relayout.
    pub generation: u64,
    dir: PathBuf,
}

impl Fonts {
    pub fn new() -> Fonts {
        let provider = TypefaceFontProvider::new();
        let mgr = FontMgr::new();
        let mut collection = FontCollection::new();
        collection.set_asset_font_manager(Some(provider.clone().into()));
        collection.set_default_font_manager(mgr.clone(), None);
        collection.enable_font_fallback();
        let (tx, rx) = channel();
        let dir = crate::data_dir().join("fonts");
        let _ = std::fs::create_dir_all(&dir);
        Fonts { provider, collection, mgr, state: HashMap::new(), tx, rx, pending: 0, generation: 0, dir }
    }

    /// Makes sure a family/weight/style is loaded or on its way. Cheap when already known.
    pub fn request(&mut self, family: &str, weight: u16, italic: bool) {
        let weight = (weight.clamp(100, 900) + 50) / 100 * 100;
        self.request_exact(family, weight, italic);
        // The regular cut is what Skia falls back to when a weight doesn't exist.
        if weight != 400 || italic {
            self.request_exact(family, 400, false);
        }
    }

    fn request_exact(&mut self, family: &str, weight: u16, italic: bool) {
        let key = (family.to_string(), weight, italic);
        if self.state.contains_key(&key) {
            return;
        }
        let file = self.file_for(&key);
        if let Ok(bytes) = std::fs::read(&file) {
            let state = if bytes.is_empty() { State::Missing } else { self.register(&key, &bytes) };
            self.state.insert(key, state);
            return;
        }
        self.state.insert(key.clone(), State::Pending);
        self.pending += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let data = match download(&key.0, key.1, key.2) {
                // Only cache definitive answers; network failures retry next session.
                Ok(data) => {
                    let _ = std::fs::write(&file, data.as_deref().unwrap_or(&[]));
                    data
                }
                Err(_) => None,
            };
            let _ = tx.send((key, data));
        });
    }

    fn file_for(&self, key: &Key) -> PathBuf {
        let safe: String = key.0.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        self.dir.join(format!("{safe}-{}{}.ttf", key.1, if key.2 { "i" } else { "" }))
    }

    fn register(&mut self, key: &Key, bytes: &[u8]) -> State {
        match self.mgr.new_from_data(skia_safe::Data::new_copy(bytes), None) {
            Some(tf) => {
                self.provider.register_typeface(tf, Some(key.0.as_str()));
                self.collection.clear_caches();
                self.generation += 1;
                State::Loaded
            }
            None => State::Missing,
        }
    }

    /// Registers finished downloads. Returns true if any new font arrived.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((key, data)) = self.rx.try_recv() {
            changed |= self.finish(key, data);
        }
        changed
    }

    fn finish(&mut self, key: Key, data: Option<Vec<u8>>) -> bool {
        self.pending = self.pending.saturating_sub(1);
        let state = match &data {
            Some(bytes) if !bytes.is_empty() => self.register(&key, bytes),
            _ => State::Missing,
        };
        self.state.insert(key, state);
        state == State::Loaded
    }

    pub fn has_pending(&self) -> bool {
        self.pending > 0
    }

    /// Blocks until outstanding downloads finish (or the timeout passes).
    pub fn wait(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut changed = false;
        while self.pending > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match self.rx.recv_timeout(left) {
                Ok((key, data)) => changed |= self.finish(key, data),
                Err(_) => break,
            }
        }
        changed
    }

    pub fn font_mgr(&self) -> &FontMgr {
        &self.mgr
    }
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

/// Fetches a static TTF from Google Fonts. Without a browser user agent, the CSS API serves TrueType.
fn download(family: &str, weight: u16, italic: bool) -> Result<Option<Vec<u8>>, ureq::Error> {
    let fam = family.replace(' ', "+");
    let spec = if italic { format!("ital,wght@1,{weight}") } else { format!("wght@{weight}") };
    let css_url = format!("https://fonts.googleapis.com/css2?family={fam}:{spec}");
    let css = match ureq::get(&css_url).call() {
        Ok(mut r) => r.body_mut().read_to_string()?,
        Err(ureq::Error::StatusCode(400 | 404)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let Some(url) = css.split("url(").nth(1).and_then(|s| s.split(')').next()) else { return Ok(None) };
    let url = url.trim_matches(|c| c == '\x27' || c == '"');
    let bytes = ureq::get(url).call()?.body_mut().with_config().limit(64 << 20).read_to_vec()?;
    Ok(Some(bytes))
}
