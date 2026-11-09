//! Fonts in the browser. Google Fonts serves browsers WOFF2 split into per-script subsets, each
//! with a `unicode-range`. As a browser would, only the subsets a text needs are loaded. The base
//! (Latin) subset registers under the family's own name and the others under `Family#subset`.
//! `families()` lists them all, so SkParagraph falls through them per character.
//!
//! Characters that no subset of the family covers fall back to `fallbacks` (Noto families by
//! default), much as the desktop app falls back to system fonts. A stylesheet without
//! `unicode-range` (one complete font, for example from a proxy that hides the browser's user
//! agent) is a single subset that covers everything.

use super::{Fonts, Key};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};

/// Tried in order for characters a document's font lacks. Han characters resolve to the first
/// CJK family listed.
pub const DEFAULT_FALLBACKS: &[&str] = &[
    "Noto Sans",
    "Noto Sans Symbols",
    "Noto Sans Symbols 2",
    "Noto Sans Math",
    "Noto Color Emoji",
    "Noto Sans SC",
    "Noto Sans TC",
    "Noto Sans JP",
    "Noto Sans KR",
    "Noto Sans Arabic",
    "Noto Sans Hebrew",
    "Noto Sans Devanagari",
    "Noto Sans Bengali",
    "Noto Sans Tamil",
    "Noto Sans Thai",
    "Noto Sans Armenian",
    "Noto Sans Georgian",
    "Noto Sans Ethiopic",
];

enum Arrival {
    Css(Key, Option<String>),
    Subset(Key, usize, Option<Vec<u8>>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Load {
    Idle,
    Pending,
    Done,
}

struct Subset {
    name: String,
    url: String,
    /// Code point ranges from `unicode-range`. Without them (one whole font), coverage is known
    /// only once the font has loaded and its character map can be asked.
    ranges: Vec<(u32, u32)>,
    load: Load,
    typeface: Option<skia_safe::Typeface>,
}

impl Subset {
    /// Whether this subset has `ch`. Until it loads, its ranges say (a rangeless subset claims
    /// everything, so it gets loaded and then asked). Once loaded, its glyphs decide too: ranges
    /// are per subset, and a font can lack characters its range spans.
    fn covers(&self, ch: char) -> bool {
        let in_range = self.ranges.is_empty() || self.ranges.iter().any(|&(a, b)| (a..=b).contains(&(ch as u32)));
        in_range
            && match (&self.typeface, self.load) {
                (Some(tf), _) => tf.unichar_to_glyph(ch as i32) != 0,
                (None, Load::Done) => false,
                (None, _) => true,
            }
    }
}

enum Cut {
    Pending,
    Ready(Vec<Subset>),
    Missing,
}

pub struct Web {
    cuts: HashMap<Key, Cut>,
    /// Extra subset family names registered per family, in arrival order.
    aliases: HashMap<String, Vec<String>>,
    /// Families with at least one subset registered.
    loaded: HashSet<String>,
    pub fallbacks: Vec<String>,
    tx: Sender<Arrival>,
    rx: Receiver<Arrival>,
}

impl Web {
    pub fn new() -> Web {
        let (tx, rx) = channel();
        Web {
            cuts: HashMap::new(),
            aliases: HashMap::new(),
            loaded: HashSet::new(),
            fallbacks: DEFAULT_FALLBACKS.iter().map(|s| s.to_string()).collect(),
            tx,
            rx,
        }
    }
}

fn base(subsets: &[Subset]) -> usize {
    subsets.iter().position(|s| s.name == "latin").unwrap_or(0)
}

impl Fonts {
    pub(super) fn web_request(&mut self, family: &str, weight: u16, italic: bool, text: &str) {
        let weight = (weight.clamp(100, 900) + 50) / 100 * 100;
        let Some(mut rest) = self.need(&(family.to_string(), weight, italic), text) else { return };
        // The regular cut is what Skia falls back to when a weight doesn't exist.
        if weight != 400 || italic {
            self.need(&(family.to_string(), 400, false), text);
        }
        if self.web.fallbacks.iter().any(|f| f == family) {
            return;
        }
        // Fallback fonts load in their regular cut only: several (emoji, symbols) have no other.
        for f in self.web.fallbacks.clone() {
            if rest.is_empty() {
                break;
            }
            if let Some(r) = self.need(&(f, 400, false), &rest) {
                rest = r;
            }
        }
    }

    /// Starts loading the subsets of a cut that `text` needs, plus its base subset. Returns the
    /// characters the cut can't cover, or `None` while its stylesheet is still on its way.
    fn need(&mut self, key: &Key, text: &str) -> Option<String> {
        let Some(cut) = self.web.cuts.get_mut(key) else {
            self.web.cuts.insert(key.clone(), Cut::Pending);
            self.fetch_css(key.clone());
            return None;
        };
        let subsets = match cut {
            Cut::Pending => return None,
            Cut::Missing => return Some(text.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect()),
            Cut::Ready(s) => s,
        };
        let first = base(subsets);
        let mut want = vec![first];
        let mut rest = String::new();
        for ch in text.chars() {
            if ch.is_whitespace() || ch.is_control() || subsets[first].covers(ch) {
                continue;
            }
            match subsets.iter().position(|s| s.covers(ch)) {
                Some(i) => want.push(i),
                None if !rest.contains(ch) => rest.push(ch),
                None => {}
            }
        }
        want.sort_unstable();
        want.dedup();
        let mut start = Vec::new();
        for i in want {
            if subsets[i].load == Load::Idle {
                subsets[i].load = Load::Pending;
                start.push((i, subsets[i].url.clone()));
            }
        }
        for (i, url) in start {
            self.pending += 1;
            let tx = self.web.tx.clone();
            let key = key.clone();
            crate::net::get(url, 64 << 20, move |reply| {
                let _ = tx.send(Arrival::Subset(key, i, reply.ok()));
            });
        }
        Some(rest)
    }

    fn fetch_css(&mut self, key: Key) {
        self.pending += 1;
        let tx = self.web.tx.clone();
        let fam = key.0.replace(' ', "+");
        let spec = if key.2 { format!("ital,wght@1,{}", key.1) } else { format!("wght@{}", key.1) };
        crate::net::get(format!("https://fonts.googleapis.com/css2?family={fam}:{spec}"), 1 << 20, move |reply| {
            let css = reply.ok().map(|b| String::from_utf8_lossy(&b).into_owned());
            let _ = tx.send(Arrival::Css(key, css));
        });
    }

    /// Takes finished downloads. True if layout should rerun: new typefaces, or a stylesheet that
    /// tells texts which subsets to load.
    pub(super) fn web_poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(arrival) = self.web.rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match arrival {
                Arrival::Css(key, css) => {
                    let subsets = css.map(|c| parse(&c)).filter(|s| !s.is_empty());
                    self.web.cuts.insert(key, subsets.map_or(Cut::Missing, Cut::Ready));
                    changed = true;
                }
                Arrival::Subset(key, i, bytes) => {
                    let Some(Cut::Ready(subsets)) = self.web.cuts.get_mut(&key) else { continue };
                    subsets[i].load = Load::Done;
                    // Even a failed subset changes what texts fall back to.
                    changed = true;
                    let name = if i == base(subsets) { key.0.clone() } else { format!("{}#{}", key.0, subsets[i].name) };
                    let Some(tf) = bytes.and_then(|b| self.add_typeface(&name, &b)) else { continue };
                    if let Some(Cut::Ready(subsets)) = self.web.cuts.get_mut(&key) {
                        subsets[i].typeface = Some(tf);
                    }
                    if name != key.0 {
                        let list = self.web.aliases.entry(key.0.clone()).or_default();
                        if !list.contains(&name) {
                            list.push(name);
                        }
                    }
                    self.web.loaded.insert(key.0);
                }
            }
        }
        changed
    }

    pub(super) fn web_families(&self, family: &str) -> Vec<String> {
        let mut out = vec![family.to_string()];
        let add = |f: &str, out: &mut Vec<String>| out.extend(self.web.aliases.get(f).into_iter().flatten().cloned());
        add(family, &mut out);
        for f in &self.web.fallbacks {
            if f != family && self.web.loaded.contains(f) {
                out.push(f.clone());
                add(f, &mut out);
            }
        }
        out
    }
}

/// The `@font-face` rules of a Google Fonts stylesheet, named by the comment before each.
fn parse(css: &str) -> Vec<Subset> {
    let mut out = Vec::new();
    let mut name = String::new();
    for (i, part) in css.split("@font-face").enumerate() {
        let tail = if i == 0 {
            part
        } else {
            let (body, tail) = part.split_once('}').unwrap_or((part, ""));
            if let Some(url) = between(body, "url(", ")") {
                out.push(Subset {
                    name: if name.is_empty() { out.len().to_string() } else { name.clone() },
                    url: url.trim_matches(|c| c == '\x27' || c == '"').to_string(),
                    ranges: between(body, "unicode-range:", ";").map(parse_ranges).unwrap_or_default(),
                    load: Load::Idle,
                    typeface: None,
                });
            }
            tail
        };
        name = tail.rsplit_once("/*").and_then(|(_, c)| c.split_once("*/")).map(|(n, _)| n.trim().to_string()).unwrap_or_default();
    }
    out
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = start + s[start..].find(close)?;
    Some(s[start..end].trim())
}

/// `U+0000-00FF, U+0131, U+4??` → inclusive ranges.
fn parse_ranges(s: &str) -> Vec<(u32, u32)> {
    s.split(',')
        .filter_map(|r| {
            let r = r.trim().trim_start_matches(['U', 'u']).trim_start_matches('+');
            let (a, b) = match r.split_once('-') {
                Some((a, b)) => (a.to_string(), b.to_string()),
                None => (r.replace('?', "0"), r.replace('?', "F")),
            };
            Some((u32::from_str_radix(&a, 16).ok()?, u32::from_str_radix(&b, 16).ok()?))
        })
        .collect()
}
