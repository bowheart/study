//! Font loading: Google Fonts (downloaded once, cached on disk) with system fallback. The web
//! build loads per-script subsets with Noto fallbacks instead (see `web.rs`), as WOFF2 or WOFF.

#[cfg(target_os = "emscripten")]
mod web;
#[cfg(test)]
mod woff_tests;

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
    #[cfg(target_os = "emscripten")]
    web: web::Web,
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
        Fonts {
            provider,
            collection,
            mgr,
            state: HashMap::new(),
            tx,
            rx,
            pending: 0,
            generation: 0,
            dir,
            #[cfg(target_os = "emscripten")]
            web: web::Web::new(),
        }
    }

    /// Makes sure a family/weight/style is loaded or on its way. Cheap when already known.
    pub fn request(&mut self, family: &str, weight: u16, italic: bool) {
        #[cfg(target_os = "emscripten")]
        return self.web_request(family, weight, italic, "");
        #[allow(unreachable_code)]
        let weight = (weight.clamp(100, 900) + 50) / 100 * 100;
        self.request_exact(family, weight, italic);
        // The regular cut is what Skia falls back to when a weight doesn't exist.
        if weight != 400 || italic {
            self.request_exact(family, 400, false);
        }
    }

    /// Like `request`, for a specific text: the web build loads the script subsets and fallback
    /// fonts its characters need.
    pub fn request_text(&mut self, family: &str, weight: u16, italic: bool, text: &str) {
        #[cfg(target_os = "emscripten")]
        return self.web_request(family, weight, italic, text);
        #[cfg(not(target_os = "emscripten"))]
        {
            let _ = text;
            self.request(family, weight, italic);
        }
    }

    /// The font families to shape `family` with, in order (the web build adds subsets and fallbacks).
    pub fn families(&self, family: &str) -> Vec<String> {
        #[cfg(target_os = "emscripten")]
        return self.web_families(family);
        #[cfg(not(target_os = "emscripten"))]
        vec![family.to_string()]
    }

    /// Replaces the fallback families (web build; the desktop uses the system's fonts).
    #[cfg(target_os = "emscripten")]
    pub fn set_fallbacks(&mut self, families: Vec<String>) {
        self.web.fallbacks = families;
    }

    /// Fonts that failed to load since the last call, as messages (web build).
    #[cfg(target_os = "emscripten")]
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.web.warnings)
    }

    fn request_exact(&mut self, family: &str, weight: u16, italic: bool) {
        let key = (family.to_string(), weight, italic);
        if self.state.contains_key(&key) {
            return;
        }
        let file = self.file_for(&key);
        if let Ok(bytes) = std::fs::read(&file) {
            let state = if bytes.is_empty() { State::Missing } else { self.register(&key.0, &bytes) };
            self.state.insert(key, state);
            return;
        }
        self.state.insert(key.clone(), State::Pending);
        self.pending += 1;
        let tx = self.tx.clone();
        download(&key.0.clone(), key.1, key.2, move |result| {
            let data = match result {
                // Only cache definitive answers; network failures retry next session.
                Ok(data) => {
                    let _ = std::fs::write(&file, data.as_deref().unwrap_or(&[]));
                    data
                }
                Err(()) => None,
            };
            let _ = tx.send((key, data));
        });
    }

    fn file_for(&self, key: &Key) -> PathBuf {
        let safe: String = key.0.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        self.dir.join(format!("{safe}-{}{}.ttf", key.1, if key.2 { "i" } else { "" }))
    }

    fn register(&mut self, family: &str, bytes: &[u8]) -> State {
        self.add_typeface(family, bytes).map_or(State::Missing, |_| State::Loaded)
    }

    /// A typeface from font file bytes, without making it available to documents.
    /// TrueType, OpenType, WOFF2 and WOFF are read.
    pub fn typeface(&self, bytes: &[u8]) -> Option<skia_safe::Typeface> {
        // Browsers get WOFF2, or WOFF for some user agents, from Google Fonts. FreeType reads
        // neither, so they're unpacked with wuff (a port of Google's own woff2 decoder).
        let decoded = if bytes.starts_with(b"wOF2") {
            Some(wuff::decompress_woff2(bytes).ok()?)
        } else if bytes.starts_with(b"wOFF") {
            Some(wuff::decompress_woff1(bytes).ok()?)
        } else {
            None
        };
        let bytes = decoded.as_deref().unwrap_or(bytes);
        self.mgr.new_from_data(skia_safe::Data::new_copy(bytes), None)
    }

    fn add_typeface(&mut self, family: &str, bytes: &[u8]) -> Option<skia_safe::Typeface> {
        let tf = self.typeface(bytes)?;
        self.provider.register_typeface(tf.clone(), Some(family));
        self.collection.clear_caches();
        self.generation += 1;
        Some(tf)
    }

    /// Registers finished downloads. Returns true if any new font arrived.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((key, data)) = self.rx.try_recv() {
            changed |= self.finish(key, data);
        }
        #[cfg(target_os = "emscripten")]
        {
            changed |= self.web_poll();
        }
        changed
    }

    fn finish(&mut self, key: Key, data: Option<Vec<u8>>) -> bool {
        self.pending = self.pending.saturating_sub(1);
        let state = match &data {
            Some(bytes) if !bytes.is_empty() => self.register(&key.0, bytes),
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

/// The font file in a Google Fonts stylesheet. Other clients get one full TrueType file; browsers
/// get WOFF2 split by script (`/* latin */`, `/* cyrillic */`, ...), of which Latin is used.
fn font_url(css: &str) -> Option<String> {
    let block = css.split("/* ").find(|b| b.starts_with("latin */")).unwrap_or(css);
    let url = block.split("url(").nth(1)?.split(')').next()?;
    Some(url.trim_matches(|c| c == '\x27' || c == '"').to_string())
}

/// Fetches a static font from Google Fonts: `Ok(None)` when the family/cut doesn't exist,
/// `Err` on network failure. Without a browser user agent, the CSS API serves TrueType.
fn download(family: &str, weight: u16, italic: bool, done: impl FnOnce(Result<Option<Vec<u8>>, ()>) + Send + 'static) {
    let fam = family.replace(' ', "+");
    let spec = if italic { format!("ital,wght@1,{weight}") } else { format!("wght@{weight}") };
    let css_url = format!("https://fonts.googleapis.com/css2?family={fam}:{spec}");
    crate::net::get(css_url, 1 << 20, move |reply| match reply {
        Ok(css) => {
            let Some(url) = font_url(&String::from_utf8_lossy(&css)) else { return done(Ok(None)) };
            crate::net::get(url, 64 << 20, move |reply| done(reply.map(Some).map_err(|_| ())));
        }
        Err(Some(400 | 404)) => done(Ok(None)),
        Err(_) => done(Err(())),
    });
}
