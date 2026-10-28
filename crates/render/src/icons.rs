//! Icon libraries. SVG libraries are fetched once from npm and unpacked into the
//! data dir; Material Symbols are drawn as font ligatures (see `draw.rs`).

use skia_safe::{svg, Canvas, FontMgr};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

struct Library {
    package: &'static str,
    version: &'static str,
    /// Directory inside the package tarball holding `<name>.svg` files.
    prefix: &'static str,
}

fn library(name: &str) -> Option<Library> {
    Some(match name.to_ascii_lowercase().as_str() {
        "lucide" => Library { package: "lucide-static", version: "1.62.0", prefix: "package/icons/" },
        "feather" => Library { package: "feather-icons", version: "4.29.2", prefix: "package/dist/icons/" },
        "phosphor" => Library { package: "@phosphor-icons/core", version: "2.1.1", prefix: "package/assets/regular/" },
        _ => return None,
    })
}

pub fn is_font_library(name: &str) -> bool {
    name.starts_with("Material Symbols")
}

#[derive(PartialEq, Eq)]
enum State {
    Ready,
    Pending,
    Failed,
}

pub struct Icons {
    dir: PathBuf,
    libs: HashMap<String, State>,
    doms: HashMap<(String, String, u32), Option<(svg::Dom, f32, f32)>>,
    tx: Sender<(String, bool)>,
    rx: Receiver<(String, bool)>,
}

impl Icons {
    pub fn new() -> Icons {
        let (tx, rx) = channel();
        Icons { dir: crate::data_dir().join("icons"), libs: HashMap::new(), doms: HashMap::new(), tx, rx }
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((lib, ok)) = self.rx.try_recv() {
            self.libs.insert(lib, if ok { State::Ready } else { State::Failed });
            self.doms.clear();
            changed = true;
        }
        changed
    }

    pub fn has_pending(&self) -> bool {
        self.libs.values().any(|s| *s == State::Pending)
    }

    pub fn wait(&mut self, timeout: std::time::Duration) {
        let deadline = std::time::Instant::now() + timeout;
        while self.has_pending() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.rx.recv_timeout(left) {
                Ok((lib, ok)) => {
                    self.libs.insert(lib, if ok { State::Ready } else { State::Failed });
                    self.doms.clear();
                }
                Err(_) => break,
            }
        }
    }

    /// Starts fetching a library if needed. Returns true when it's ready on disk.
    pub fn ensure(&mut self, lib: &str) -> bool {
        let key = lib.to_ascii_lowercase();
        if let Some(s) = self.libs.get(&key) {
            return *s == State::Ready;
        }
        let Some(spec) = library(&key) else {
            self.libs.insert(key, State::Failed);
            return false;
        };
        let dir = self.dir.join(&key);
        if dir.join(".complete").exists() {
            self.libs.insert(key, State::Ready);
            return true;
        }
        self.libs.insert(key.clone(), State::Pending);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let ok = fetch(&spec, &dir).is_ok();
            let _ = tx.send((key, ok));
        });
        false
    }

    /// Draws an icon scaled to fit `w`×`h`, tinted with `color`.
    pub fn draw(&mut self, canvas: &Canvas, lib: &str, name: &str, color: u32, w: f32, h: f32, mgr: &FontMgr) -> bool {
        if !self.ensure(lib) {
            return false;
        }
        let key = (lib.to_ascii_lowercase(), name.to_string(), color);
        if !self.doms.contains_key(&key) {
            let dom = self.load(&key.0, name, color, mgr);
            self.doms.insert(key.clone(), dom);
        }
        let Some((dom, vw, vh)) = self.doms.get_mut(&key).unwrap() else { return false };
        let s = (w / *vw).min(h / *vh);
        canvas.save();
        canvas.translate(((w - *vw * s) / 2.0, (h - *vh * s) / 2.0));
        canvas.scale((s, s));
        dom.render(canvas);
        canvas.restore();
        true
    }

    fn load(&self, lib: &str, name: &str, color: u32, mgr: &FontMgr) -> Option<(svg::Dom, f32, f32)> {
        let file = self.dir.join(lib).join(format!("{}.svg", sanitize(name)));
        let text = std::fs::read_to_string(file).ok()?;
        let hex = format!("#{:06X}", color & 0xFFFFFF);
        let alpha = ((color >> 24) & 0xFF) as f32 / 255.0;
        let mut text = text.replace("currentColor", &hex);
        if alpha < 1.0 {
            text = text.replacen("<svg", &format!("<svg opacity=\"{alpha}\""), 1);
        }
        let (vw, vh) = view_box(&text).unwrap_or((24.0, 24.0));
        let mut dom = svg::Dom::from_bytes(text.as_bytes(), mgr.clone()).ok()?;
        dom.set_container_size((vw, vh));
        Some((dom, vw, vh))
    }

    /// Raw SVG text for an icon (for exports).
    pub fn svg_text(&mut self, lib: &str, name: &str) -> Option<String> {
        if !self.ensure(lib) {
            return None;
        }
        std::fs::read_to_string(self.dir.join(lib.to_ascii_lowercase()).join(format!("{}.svg", sanitize(name)))).ok()
    }
}

impl Default for Icons {
    fn default() -> Self {
        Self::new()
    }
}

fn sanitize(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect::<String>().to_ascii_lowercase()
}

fn view_box(svg: &str) -> Option<(f32, f32)> {
    let start = svg.find("viewBox=\"")? + 9;
    let end = start + svg[start..].find('"')?;
    let nums: Vec<f32> = svg[start..end].split([' ', ',']).filter_map(|s| s.parse().ok()).collect();
    (nums.len() == 4).then(|| (nums[2], nums[3]))
}

fn fetch(lib: &Library, dir: &Path) -> anyhow::Result<()> {
    let base = lib.package.rsplit('/').next().unwrap();
    let url = format!("https://registry.npmjs.org/{}/-/{}-{}.tgz", lib.package, base, lib.version);
    let mut bytes = Vec::new();
    ureq::get(&url).call()?.body_mut().with_config().limit(256 << 20).reader().read_to_end(&mut bytes)?;
    std::fs::create_dir_all(dir)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..]));
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().to_string();
        let Some(rest) = path.strip_prefix(lib.prefix) else { continue };
        if rest.contains('/') || !rest.ends_with(".svg") {
            continue;
        }
        let mut svg = String::new();
        entry.read_to_string(&mut svg)?;
        std::fs::write(dir.join(rest.to_ascii_lowercase()), svg)?;
    }
    std::fs::write(dir.join(".complete"), lib.version)?;
    Ok(())
}
