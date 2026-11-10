//! The viewer behind the `study_*` exports: one document, a camera, selection, and the
//! agent-presence overlays, drawn like the desktop window's canvas (`crates/study/src/gui.rs`).
//!
//! Files the document refers to (images, shaders, script sources, imported `.pen` files) live in
//! Emscripten's in-memory filesystem under `/doc`, at the paths the page knows them by. The page
//! asks `study_file_refs` which are missing and supplies them before `study_load`. Script nodes are
//! evaluated by the page in a sandboxed worker: `study_script_jobs` hands out the wrapped code and
//! `study_script_result` brings the output back.

#[path = "changes.rs"]
mod changes;

use changes::{Changes, Side};
use pen::imports::Imports;
use pen::layout::{compute_bounds, layout};
use pen::scene::{compile_root_with, graft, Kind, Rect, RootScene, NONE};
use pen::vars::Vars;
use pen::{Document, NodeId};
use render::controls::{Control, Crumb, Show};
use render::overlay::{draw_nib, draw_scan, draw_working, Label};
use render::{Renderer, View};
use serde_json::{json, Map, Value};
use skia_safe::gpu::{self, gl::FramebufferInfo, DirectContext};
use skia_safe::{Canvas, Color, Color4f, Font, FontStyle, Paint, PaintStyle, PathBuilder, Point, Surface};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn emscripten_GetProcAddress(name: *const c_char) -> *const c_void;
}

/// Loads WebGL function pointers for Skia; once, at startup.
pub fn init_gl() {
    gl::load_with(|name| {
        let name = CString::new(name).unwrap();
        unsafe { emscripten_GetProcAddress(name.as_ptr()) }
    });
}

/// Root of the page's files in the in-memory filesystem.
const FILES: &str = "/doc";
/// Sweep, then linger and fade (see `draw_scan`).
const SCAN_SECS: f32 = 2.2;
/// How long a frame keeps glowing after the agent last touched it, and its fade-out.
const WORK_HOLD: f32 = 20.0;
const WORK_FADE: f32 = 2.5;
const UI_FAMILY: &str = "Inter";
/// Longest edge of a node snapshot, like the MCP screenshot tool's.
const SNAPSHOT_EDGE: f32 = 1568.0;

#[derive(Clone, Copy)]
struct Camera {
    x: f32,
    y: f32,
    zoom: f32,
}

/// Frame titles and badges, in Inter once it has downloaded (the page has no system fonts).
struct UiFont(Option<Font>);

impl Label for UiFont {
    fn measure(&self, s: &str) -> f32 {
        self.0.as_ref().map_or(0.0, |f| f.measure_str(s, None).0)
    }

    fn draw(&self, canvas: &Canvas, s: &str, at: (f32, f32), paint: &Paint) {
        if let Some(f) = &self.0 {
            canvas.draw_str(s, at, f, paint);
        }
    }
}

/// Script node runs, keyed by `pen::script::Job::key`.
#[derive(Default)]
struct ScriptRuns {
    done: HashMap<u64, Result<Vec<Value>, String>>,
    /// Jobs the page hasn't picked up yet: key and code.
    queue: Vec<(u64, String)>,
    asked: HashSet<u64>,
    /// Set when results arrived; the next frame rebuilds each layer's script roots.
    dirty: bool,
}

impl ScriptRuns {
    /// Runs the page started that haven't come back.
    fn outstanding(&self) -> bool {
        !self.queue.is_empty() || self.asked.iter().any(|k| !self.done.contains_key(k))
    }
}

/// A document and its laid-out top-level nodes. The viewer shows one; `compare` and the
/// overlay lay out another the same way.
struct Layer {
    doc: Document,
    vars: Vars,
    imports: Imports,
    scenes: HashMap<NodeId, RootScene>,
    /// Roots that contain script nodes (rebuilt when results arrive).
    script_roots: HashSet<NodeId>,
    /// Folder its relative files resolve from.
    base: PathBuf,
}

impl Layer {
    /// Parses a document kept at page path `path`; `build` lays it out.
    fn open(text: &str, path: &str, warnings: &mut Vec<String>) -> Result<Layer, String> {
        let mut doc = Document::parse(text).map_err(|e| format!("{e:#}"))?;
        let file = Path::new(FILES).join(clean(path).unwrap_or_else(|| "document.pen".into()));
        let base = file.parent().map(Path::to_path_buf).unwrap_or_else(|| FILES.into());
        let _ = std::fs::create_dir_all(&base);
        doc.path = Some(file);
        let imports = Imports::load(&doc);
        warnings.extend(imports.errors.iter().cloned());
        let mut vars = Vars::new(&doc);
        vars.add_imported(&imports.variables);
        Ok(Layer { doc, vars, imports, scenes: HashMap::new(), script_roots: HashSet::new(), base })
    }

    /// Compiles and lays out the given roots (all when `None`), running script nodes.
    fn build(&mut self, only: Option<Vec<NodeId>>, renderer: &mut Renderer, runs: &mut ScriptRuns, warnings: &mut Vec<String>) {
        let roots = only.unwrap_or_else(|| self.doc.roots.clone());
        if roots.len() == self.doc.roots.len() {
            self.scenes.clear();
        }
        for r in roots {
            let mut s = compile_root_with(&self.doc, &self.vars, r, Some(&self.imports));
            layout(&mut s, &mut renderer.text);
            if run_scripts(&mut s, &self.doc, &self.vars, &self.imports, &self.base, runs, warnings) {
                self.script_roots.insert(r);
                layout(&mut s, &mut renderer.text);
            }
            compute_bounds(&mut s);
            self.scenes.insert(r, s);
        }
    }

    fn rebuild_scripts(&mut self, renderer: &mut Renderer, runs: &mut ScriptRuns, warnings: &mut Vec<String>) {
        if !self.script_roots.is_empty() {
            let roots = self.script_roots.iter().copied().collect();
            self.build(Some(roots), renderer, runs, warnings);
        }
    }

    fn relayout(&mut self, renderer: &mut Renderer) {
        for s in self.scenes.values_mut() {
            layout(s, &mut renderer.text);
            compute_bounds(s);
        }
    }

    /// Top-level scenes in document order.
    fn ordered(&self) -> Vec<&RootScene> {
        self.doc.roots.iter().filter_map(|r| self.scenes.get(r)).collect()
    }

    fn bounds(&self) -> Option<Rect> {
        self.ordered().iter().filter_map(|s| s.bounds.first().copied()).filter(|b| b.w >= 0.0).reduce(|a, b| a.union(&b))
    }

    /// Scene and index for a node id or instance path.
    fn locate(&self, key: &str) -> Option<(&RootScene, u32)> {
        let n = self.doc.find(key.split('/').next()?)?;
        if !self.doc.node(n).attached {
            return None;
        }
        let scene = self.scenes.get(&self.doc.root_of(n))?;
        let i = if key.contains('/') { scene.lookup(key, &self.doc)? } else { scene.lookup_node(n)? };
        Some((scene, i))
    }

    fn version(&self) -> pen::diff::Version<'_> {
        pen::diff::Version { doc: &self.doc, scenes: &self.scenes }
    }
}

/// A persistent outline and tint on some nodes (`highlight`), separate from selection and hover.
struct Highlight {
    kind: String,
    color: Color4f,
    keys: Vec<String>,
}

/// The colour for a highlight kind: `added`, `changed`, `removed`, or `#rgb`/`#rrggbb`.
fn highlight_color(kind: &str) -> Option<Color4f> {
    let rgb = |r: u8, g: u8, b: u8| Some(Color4f::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0));
    match kind {
        "added" => rgb(0x16, 0xA3, 0x4A),
        "changed" => rgb(0xF5, 0x9E, 0x0B),
        "removed" => rgb(0xDC, 0x26, 0x26),
        _ => {
            let hex = kind.strip_prefix('#')?;
            let hex: String = if hex.len() == 3 { hex.chars().flat_map(|c| [c, c]).collect() } else { hex.to_string() };
            if hex.len() != 6 {
                return None;
            }
            let n = u32::from_str_radix(&hex, 16).ok()?;
            rgb((n >> 16) as u8, (n >> 8) as u8, n as u8)
        }
    }
}

pub struct Viewer {
    gr: DirectContext,
    fb: FramebufferInfo,
    surface: Surface,
    /// Canvas size in CSS pixels, and device pixels per CSS pixel.
    size: (f32, f32),
    dpr: f32,
    /// The document shown.
    main: Option<Layer>,
    /// Another document drawn translucently over it (`setOverlay`), and its opacity.
    overlay: Option<(Layer, f32)>,
    /// The other version while `compare` waits for its fonts and scripts.
    other: Option<Layer>,
    highlights: Vec<Highlight>,
    /// The changes layout (`showChanges`): rows of frames that differ from another version.
    changes: Option<Changes>,
    renderer: Renderer,
    scripts: ScriptRuns,
    /// Problems to report to the page (script and import failures).
    warnings: Vec<String>,
    cam: Camera,
    fitted: bool,
    dark: bool,
    started: Instant,
    anims: Vec<(String, Instant, f32)>,
    scans: Vec<(String, Instant)>,
    working: HashMap<String, (Instant, Instant)>,
    /// Selected nodes, all on one side: the shown document, or the changes layout's `before`.
    selection: Vec<String>,
    sel_side: Side,
    hover: Option<(String, Side)>,
    /// Built-in controls to show, and the one under the pointer.
    show: Show,
    ctrl_hover: Option<Control>,
    ui: UiFont,
    /// The last string handed to the page (it reads it before the next call).
    out: CString,
    /// The last binary result (a snapshot), read the same way.
    blob: Vec<u8>,
}

impl Viewer {
    fn new(w: i32, h: i32, dpr: f32) -> Viewer {
        let interface = gpu::gl::Interface::new_native().expect("WebGL interface");
        let mut gr = gpu::direct_contexts::make_gl(interface, None).expect("Skia GPU context");
        let mut fbo: gl::types::GLint = 0;
        unsafe { gl::GetIntegerv(gl::FRAMEBUFFER_BINDING, &mut fbo) };
        let fb = FramebufferInfo { fboid: fbo as u32, format: gpu::gl::Format::RGBA8.into(), protected: gpu::Protected::No };
        let surface = make_surface(&mut gr, fb, w, h);
        let mut renderer = Renderer::new();
        renderer.base_dir = FILES.into();
        renderer.text.fonts.request(UI_FAMILY, 400, false);
        Viewer {
            gr,
            fb,
            surface,
            size: (w as f32 / dpr, h as f32 / dpr),
            dpr,
            main: None,
            overlay: None,
            other: None,
            highlights: Vec::new(),
            changes: None,
            renderer,
            scripts: ScriptRuns::default(),
            warnings: Vec::new(),
            cam: Camera { x: 0.0, y: 0.0, zoom: 1.0 },
            fitted: false,
            dark: false,
            started: Instant::now(),
            anims: Vec::new(),
            scans: Vec::new(),
            working: HashMap::new(),
            selection: Vec::new(),
            sel_side: Side::After,
            hover: None,
            show: Show::default(),
            ctrl_hover: None,
            ui: UiFont(None),
            out: CString::default(),
            blob: Vec::new(),
        }
    }

    fn resize(&mut self, w: i32, h: i32, dpr: f32) {
        self.surface = make_surface(&mut self.gr, self.fb, w, h);
        let size = (w as f32 / dpr, h as f32 / dpr);
        let resized = size != self.size;
        self.size = size;
        self.dpr = dpr;
        if resized {
            self.place_rows();
        }
    }

    /// Replaces the document; `path` is where the page keeps it (relative files resolve from
    /// there). The camera stays put, except on the first load, which fits.
    fn load(&mut self, text: &str, path: &str) -> Result<(), String> {
        let mut layer = Layer::open(text, path, &mut self.warnings)?;
        layer.build(None, &mut self.renderer, &mut self.scripts, &mut self.warnings);
        self.renderer.base_dir = layer.base.clone();
        self.main = Some(layer);
        self.refresh_changes();
        let selection = std::mem::take(&mut self.selection);
        self.selection = selection.into_iter().filter(|k| self.locate_on(self.sel_side, k).is_some()).collect();
        if self.hover.as_ref().is_some_and(|(k, side)| self.locate_on(*side, k).is_none()) {
            self.hover = None;
        }
        Ok(())
    }

    fn bounds(&self) -> Option<Rect> {
        self.main.as_ref()?.bounds()
    }

    /// Persistent outlines: `keys` for `kind` replace that kind's earlier ones; no kind and no
    /// keys clears them all. False for an unknown kind.
    fn highlight(&mut self, kind: &str, keys: Vec<String>) -> bool {
        if kind.is_empty() && keys.is_empty() {
            self.highlights.clear();
            return true;
        }
        let kind = if kind.is_empty() { "changed" } else { kind };
        let Some(color) = highlight_color(kind) else { return false };
        self.highlights.retain(|h| h.kind != kind);
        if !keys.is_empty() {
            self.highlights.push(Highlight { kind: kind.to_string(), color, keys });
        }
        true
    }

    /// Starts comparing the shown document with another version (`text` at page path `path`):
    /// lays it out; `compare_result` reports once its fonts and scripts are in.
    fn compare_start(&mut self, text: &str, path: &str) -> Result<(), String> {
        let mut layer = Layer::open(text, path, &mut self.warnings)?;
        layer.build(None, &mut self.renderer, &mut self.scripts, &mut self.warnings);
        self.other = Some(layer);
        Ok(())
    }

    /// Whether downloads or script runs that could change a layout are still out.
    fn settling(&self) -> bool {
        self.renderer.text.fonts.has_pending() || self.scripts.outstanding()
    }

    /// What changed from the other version (`before`) to the shown one, as JSON.
    fn compare_result(&mut self) -> Option<Value> {
        let other = self.other.take()?;
        let main = self.main.as_ref()?;
        let d = pen::diff::compare(&other.version(), &main.version());
        let rect = |r: Rect| json!({"x": r.x, "y": r.y, "width": r.w, "height": r.h});
        let changed: Vec<Value> = d
            .changed
            .iter()
            .map(|c| {
                json!({
                    "id": c.id,
                    "props": c.props,
                    "moved": c.moved,
                    "bounds": c.bounds.map(|(a, b)| json!({"before": rect(a), "after": rect(b)})),
                })
            })
            .collect();
        Some(json!({"added": d.added, "removed": d.removed, "changed": changed, "frames": d.frames}))
    }

    /// Draws another version over the shown one at `opacity` (`text` empty: removes it).
    fn set_overlay(&mut self, text: &str, path: &str, opacity: f32) -> Result<(), String> {
        if text.is_empty() {
            self.overlay = None;
            return Ok(());
        }
        let mut layer = Layer::open(text, path, &mut self.warnings)?;
        layer.build(None, &mut self.renderer, &mut self.scripts, &mut self.warnings);
        self.overlay = Some((layer, opacity.clamp(0.0, 1.0)));
        Ok(())
    }

    fn fit(&mut self, r: Option<Rect>) {
        if self.changes.is_some() {
            return self.fit_rows(r);
        }
        let Some(r) = r.or_else(|| self.bounds()) else { return };
        let (w, h) = self.size;
        let margin = 48.0;
        let zoom = ((w - 2.0 * margin) / r.w.max(1.0)).min((h - 2.0 * margin) / r.h.max(1.0)).clamp(0.002, 1.0);
        self.cam = Camera { x: r.x + r.w / 2.0 - w / zoom / 2.0, y: r.y + r.h / 2.0 - h / zoom / 2.0, zoom };
        self.fitted = true;
    }

    fn zoom_at(&mut self, factor: f32, sx: f32, sy: f32) {
        let (dx, dy) = (self.cam.x + sx / self.cam.zoom, self.cam.y + sy / self.cam.zoom);
        let zoom = (self.cam.zoom * factor).clamp(0.001, 256.0);
        self.cam = Camera { x: dx - sx / zoom, y: dy - sy / zoom, zoom };
        self.clamp_cam();
    }

    fn viewport(&self) -> Rect {
        Rect::new(self.cam.x, self.cam.y, self.size.0 / self.cam.zoom, self.size.1 / self.cam.zoom)
    }

    /// Scene and index for a node id or instance path.
    fn locate(&self, key: &str) -> Option<(&RootScene, u32)> {
        self.main.as_ref()?.locate(key)
    }

    /// A node's corners in its document's space (rotation included).
    fn corners(&self, side: Side, key: &str) -> Option<[(f32, f32); 4]> {
        let (s, i) = self.locate_on(side, key)?;
        let (m, w, h) = s.transform(i);
        Some([m.map(0.0, 0.0), m.map(w, 0.0), m.map(w, h), m.map(0.0, h)])
    }

    /// Bounding box of a node in its document's space, rotation included.
    fn doc_box(&self, side: Side, key: &str) -> Option<Rect> {
        let pts = self.corners(side, key)?;
        let (l, t) = pts.iter().fold((f32::MAX, f32::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
        let (r, b) = pts.iter().fold((f32::MIN, f32::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
        Some(Rect::new(l, t, r - l, b - t))
    }

    /// Deepest selectable node under a point in CSS pixels, and which version it's in.
    fn hit(&self, sx: f32, sy: f32) -> Option<(String, Side)> {
        if self.changes.is_some() {
            return self.hit_rows(sx, sy);
        }
        let main = self.main.as_ref()?;
        let (x, y) = (self.cam.x + sx / self.cam.zoom, self.cam.y + sy / self.cam.zoom);
        main.ordered().into_iter().rev().find_map(|s| s.hit(x, y).map(|i| (s.nodes[i as usize].key().to_string(), Side::After)))
    }

    /// The desktop's click: select what's under the point (`toggle` adds/removes it). The
    /// selection stays on one side of the changes layout: a click on the other side replaces it.
    fn click(&mut self, sx: f32, sy: f32, toggle: bool) {
        let hit = self.hit(sx, sy);
        if let Some((_, side)) = &hit {
            if *side != self.sel_side {
                self.selection.clear();
                self.sel_side = *side;
            }
        }
        match (hit.map(|h| h.0), toggle) {
            (Some(k), true) => match self.selection.iter().position(|s| *s == k) {
                Some(i) => {
                    self.selection.remove(i);
                }
                None => self.selection.push(k),
            },
            (Some(k), false) => self.selection = vec![k],
            (None, false) => self.selection.clear(),
            (None, true) => {}
        }
    }

    /// Moves the selection to parents (`up`) or first children.
    fn select_relative(&mut self, up: bool) {
        let mut next = Vec::new();
        for k in &self.selection {
            let Some((s, i)) = self.locate_on(self.sel_side, k) else { continue };
            let n = &s.nodes[i as usize];
            let target = if up {
                (n.parent != NONE).then(|| s.nodes[n.parent as usize].key().to_string())
            } else {
                n.children.iter().find(|&&c| s.nodes[c as usize].enabled && !s.nodes[c as usize].key().is_empty()).map(|&c| s.nodes[c as usize].key().to_string())
            };
            next.push(target.unwrap_or_else(|| k.clone()));
        }
        next.dedup();
        self.selection = next;
    }

    /// What the page needs to know about a node: identity, place in the tree, and where it is.
    fn node_info(&self, key: &str, side: Side) -> Option<Value> {
        let doc = &self.layer(side)?.doc;
        let (s, i) = self.locate_on(side, key)?;
        let n = &s.nodes[i as usize];
        let b = self.doc_box(side, key)?;
        // Not drawn in the changes layout when its frame has no row.
        let screen = self.doc_xf(side, key).map(|xf| {
            let r = xf.rect(b);
            json!({"x": r.x, "y": r.y, "width": r.w, "height": r.h})
        });
        // Nodes from imported files have no source in this document; their compiled kind says.
        let ty = n.src.map(|src| doc.node(src).ty().to_string()).unwrap_or_else(|| kind_name(&n.kind).into());
        let parent = (n.parent != NONE).then(|| s.nodes[n.parent as usize].key().to_string());
        let root = s.root().map(|r| r.key().to_string());
        let component = key.contains('/').then(|| key.split('/').next().unwrap_or("").to_string());
        Some(json!({
            "id": key,
            "name": n.name.as_deref(),
            "type": ty,
            "parent": parent,
            "root": root,
            "instance": component,
            "side": side.name(),
            "bounds": {"x": b.x, "y": b.y, "width": b.w, "height": b.h},
            "screen": screen,
        }))
    }

    /// The node's own JSON from the document (inside an instance: the component's node).
    fn node_json(&self, key: &str, side: Side) -> Option<Value> {
        let doc = &self.layer(side)?.doc;
        let (s, i) = self.locate_on(side, key)?;
        Some(doc.to_value(s.nodes[i as usize].src?))
    }

    /// PNG of a node and what's drawn over it, like the MCP screenshot tool.
    fn snapshot(&mut self, key: &str, scale: Option<f32>, side: Side) -> Option<Vec<u8>> {
        let region = self.doc_box(side, key)?;
        let scale = scale.unwrap_or((SNAPSHOT_EDGE / region.w.max(region.h)).min(2.0));
        let layer = match side {
            Side::After => self.main.as_ref()?,
            Side::Before => &self.changes.as_ref()?.before,
        };
        let scenes = layer.ordered();
        let base = std::mem::replace(&mut self.renderer.base_dir, layer.base.clone());
        self.renderer.layer = 3;
        let img = render::offscreen::render_region(&mut self.renderer, &scenes, region, scale, None);
        self.renderer.layer = 0;
        self.renderer.base_dir = base;
        render::offscreen::encode(&img?, "png", None)
    }

    /// The last selected node and its ancestors, for the breadcrumb.
    fn crumbs(&self) -> Vec<Crumb> {
        let Some((s, i)) = self.selection.last().and_then(|k| self.locate_on(self.sel_side, k)) else { return Vec::new() };
        s.ancestors(i)
            .into_iter()
            .map(|a| {
                let n = &s.nodes[a as usize];
                Crumb { key: n.key().to_string(), label: n.name.as_deref().unwrap_or(&n.id).to_string() }
            })
            .collect()
    }

    /// The built-in controls, laid out for the canvas (CSS px).
    fn controls(&self) -> render::controls::Layout {
        let show = Show { zoom: self.show.zoom && self.main.is_some(), ..self.show };
        render::controls::layout(show, self.size, &self.crumbs(), self.cam.zoom, &self.ui)
    }

    /// Acts on a built-in control; returns what it was, for the page.
    fn press(&mut self, c: Control) -> Value {
        let (w, h) = self.size;
        match c {
            Control::Crumb(i) => {
                if let Some(k) = self.crumbs().get(i) {
                    self.selection = vec![k.key.clone()];
                }
                json!({"control": "breadcrumb"})
            }
            Control::ZoomOut => {
                self.zoom_at(1.0 / render::controls::ZOOM_STEP, w / 2.0, h / 2.0);
                json!({"control": "zoom"})
            }
            Control::ZoomIn => {
                self.zoom_at(render::controls::ZOOM_STEP, w / 2.0, h / 2.0);
                json!({"control": "zoom"})
            }
            Control::ZoomLevel => {
                self.fit(None);
                json!({"control": "zoom"})
            }
            Control::Light | Control::Dark => {
                self.dark = c == Control::Dark;
                json!({"control": "theme", "dark": self.dark})
            }
        }
    }

    /// Nodes an agent read: a scan line sweeps each, staggered.
    fn scan(&mut self, mut keys: Vec<String>) {
        let mut seen = HashSet::new();
        keys.retain(|k| seen.insert(k.clone()) && self.on_screen(Side::After, k));
        let now = Instant::now();
        for (i, key) in keys.into_iter().take(24).enumerate() {
            self.scans.push((key, now + Duration::from_millis(140 * i as u64)));
        }
    }

    /// Top-level frames an agent changed: they glow until it has left them alone for a while.
    fn touch(&mut self, keys: Vec<String>) {
        let now = Instant::now();
        for k in keys {
            let w = self.working.entry(k).or_insert((now, now));
            if now.duration_since(w.1).as_secs_f32() > WORK_HOLD + WORK_FADE {
                w.0 = now;
            }
            w.1 = now;
        }
    }

    /// Paths an agent created: the pen draws them in, one after another.
    fn draw_in(&mut self, keys: Vec<String>) {
        let screen = Rect::new(0.0, 0.0, self.size.0, self.size.1);
        let items: Vec<(String, f32)> = keys
            .into_iter()
            .filter_map(|k| {
                let r = self.screen_box(Side::After, &k)?;
                let px = r.w + r.h;
                (r.intersects(&screen) && px > 2.0).then(|| (k, (0.3 + px / 900.0).clamp(0.3, 1.1)))
            })
            .collect();
        let total: f32 = items.iter().map(|(_, d)| d * 0.7).sum();
        let squeeze = if total > 5.0 { 5.0 / total } else { 1.0 };
        let mut at = Instant::now() + Duration::from_millis(80);
        if let Some(last) = self.anims.iter().map(|(_, s, d)| *s + Duration::from_secs_f32(*d)).max() {
            at = at.max(last);
        }
        for (k, d) in items {
            let dur = (d * squeeze).max(0.08);
            self.anims.push((k, at, dur));
            at += Duration::from_secs_f32(dur * 0.7);
        }
    }

    /// Takes in what arrived and draws a frame. True if another should follow.
    fn frame(&mut self) -> bool {
        self.pump();
        self.draw()
    }

    /// Takes in finished downloads and script results and lays out again what they affect.
    fn pump(&mut self) {
        let rebuild = std::mem::take(&mut self.scripts.dirty);
        let (fonts, _) = self.renderer.poll();
        let (renderer, scripts, warnings) = (&mut self.renderer, &mut self.scripts, &mut self.warnings);
        let layers = self.main.iter_mut().chain(self.overlay.iter_mut().map(|(l, _)| l)).chain(self.other.iter_mut()).chain(self.changes.iter_mut().map(|c| &mut c.before));
        for l in layers {
            if rebuild {
                l.rebuild_scripts(renderer, scripts, warnings);
            }
            if fonts {
                l.relayout(renderer);
            }
        }
        if rebuild || fonts {
            self.refresh_changes();
        }
        if self.ui.0.is_none() {
            // Asked again each frame: its stylesheet arriving doesn't load the font by itself.
            self.renderer.text.fonts.request(UI_FAMILY, 400, false);
            // Until Inter arrives, the lookup falls back to an empty typeface.
            let tf = self.renderer.text.fonts.collection.find_typefaces(&[UI_FAMILY], FontStyle::normal());
            self.ui = UiFont(tf.into_iter().find(|t| t.family_name() == UI_FAMILY).map(|t| Font::new(t, 12.0)));
        }
    }

    /// Draws a frame. Returns true if another should follow (animations, loading thumbnails).
    fn draw(&mut self) -> bool {
        if !self.fitted {
            self.fit(None);
        }
        let now = Instant::now();
        let cam = self.cam;
        let (ww, wh) = self.size;
        let sk = |r: Rect| skia_safe::Rect::from_xywh(r.x, r.y, r.w, r.h);

        self.anims.retain(|(_, s, d)| now < *s + Duration::from_secs_f32(*d));
        let mut reveal: HashMap<u32, HashMap<u32, f32>> = HashMap::new();
        for (key, start, dur) in &self.anims {
            let Some((s, i)) = self.locate(key) else { continue };
            let Some(root) = s.doc_root else { continue };
            let t = if now < *start { 0.0 } else { (now - *start).as_secs_f32() / dur };
            reveal.entry(root.0).or_default().insert(i, t.min(1.0));
        }
        self.scans.retain(|(_, s)| now < *s + Duration::from_secs_f32(SCAN_SECS));
        let scans: Vec<_> = self
            .scans
            .iter()
            .filter(|(_, s)| now >= *s)
            .filter_map(|(k, s)| Some((sk(self.screen_box(Side::After, k)?), (now - *s).as_secs_f32() / SCAN_SECS)))
            .collect();
        self.working.retain(|_, (_, last)| now.duration_since(*last).as_secs_f32() < WORK_HOLD + WORK_FADE);
        let glows: Vec<_> = self
            .working
            .iter()
            .filter_map(|(k, (first, last))| {
                let (s, i) = self.locate(k)?;
                let xf = self.doc_xf(Side::After, k)?;
                let fade_in = (now.duration_since(*first).as_secs_f32() / 0.4).min(1.0);
                let idle = now.duration_since(*last).as_secs_f32();
                let fade_out = 1.0 - ((idle - WORK_HOLD) / WORK_FADE).clamp(0.0, 1.0);
                Some((sk(xf.rect(s.abs_rect(i))), s.nodes[i as usize].radius[0] * xf.scale(), fade_in * fade_out))
            })
            .collect();
        // Selection and hover outlines, in screen space, with the selection's name and size.
        let side = self.sel_side;
        let hover = self.hover.as_ref().filter(|(h, s)| !(*s == side && self.selection.contains(h))).and_then(|(h, s)| self.screen_corners(*s, h));
        let selected: Vec<_> = self
            .selection
            .iter()
            .filter_map(|k| {
                let (s, i) = self.locate_on(side, k)?;
                let n = &s.nodes[i as usize];
                let label = format!("{}  {}×{}", n.name.as_deref().unwrap_or(&n.id), n.rect.w.round(), n.rect.h.round());
                Some((self.screen_corners(side, k)?, label))
            })
            .collect();
        let mut highlights = self.change_marks();
        highlights.extend(self.highlights.iter().map(|h| (h.color, h.keys.iter().filter_map(|k| self.screen_corners(Side::After, k)).collect::<Vec<_>>())));
        let clock = now.duration_since(self.started).as_secs_f32();
        let controls = self.controls();

        let viewport = self.viewport();
        let canvas = self.surface.canvas();
        canvas.clear(if self.dark { Color::from_rgb(0x1E, 0x1E, 0x20) } else { Color::from_rgb(0xEC, 0xEC, 0xEC) });
        canvas.save();
        canvas.scale((self.dpr, self.dpr));
        if let Some(main) = &self.main {
            self.renderer.begin_frame();
            self.renderer.reveal = reveal;
            if let Some(ch) = &self.changes {
                changes::draw_rows(canvas, &mut self.renderer, main, ch, cam, self.dpr, self.size);
            } else {
                canvas.save();
                canvas.scale((cam.zoom, cam.zoom));
                canvas.translate((-cam.x, -cam.y));
                let view = View { scale: cam.zoom * self.dpr, viewport, lod: true };
                for s in main.ordered() {
                    self.renderer.draw_scene(canvas, s, &view);
                }
                if let Some((over, opacity)) = &self.overlay {
                    // Its own thumbnails and files; no draw-in animation.
                    let reveal = std::mem::take(&mut self.renderer.reveal);
                    self.renderer.layer = 1;
                    self.renderer.base_dir = over.base.clone();
                    canvas.save_layer_alpha_f(None, *opacity);
                    for s in over.ordered() {
                        self.renderer.draw_scene(canvas, s, &view);
                    }
                    canvas.restore();
                    self.renderer.layer = 0;
                    self.renderer.base_dir = main.base.clone();
                    self.renderer.reveal = reveal;
                }
                canvas.restore();
            }
            self.renderer.text.end_frame();

            for (color, quads) in &highlights {
                let fill = Paint::new(Color4f::new(color.r, color.g, color.b, 0.12), None);
                for q in quads {
                    let mut b = PathBuilder::new();
                    b.move_to(q[0]);
                    for p in &q[1..] {
                        b.line_to(*p);
                    }
                    b.close();
                    canvas.draw_path(&b.detach(), &fill);
                    draw_quad(canvas, q, *color, 2.0);
                }
            }

            let label = Paint::new(if self.dark { Color4f::new(0.74, 0.74, 0.77, 1.0) } else { Color4f::new(0.42, 0.42, 0.45, 1.0) }, None);
            // Frame titles (the changes layout's rows get theirs from the page).
            for s in main.ordered().into_iter().filter(|_| self.changes.is_none()) {
                let Some(n) = s.root() else { continue };
                if !n.enabled || !matches!(n.kind, Kind::Frame { .. }) {
                    continue;
                }
                let (sx, sy, sw) = ((n.rect.x - cam.x) * cam.zoom, (n.rect.y - cam.y) * cam.zoom, n.rect.w * cam.zoom);
                if sw < 40.0 || sy < -4.0 || sy > wh + 20.0 || sx > ww || sx + sw < 0.0 {
                    continue;
                }
                let text = ellipsize(&self.ui, n.name.as_deref().unwrap_or(&n.id), sw);
                self.ui.draw(canvas, &text, (sx, sy - 6.0), &label);
            }
            if let Some(q) = hover {
                draw_quad(canvas, &q, Color4f::new(0.10, 0.55, 1.0, 0.55), 1.0);
            }
            for (q, text) in &selected {
                let color = Color4f::new(0.10, 0.55, 1.0, 1.0);
                draw_quad(canvas, q, color, 1.5);
                let (x, y) = q[3];
                let tw = self.ui.measure(text);
                canvas.draw_round_rect(skia_safe::Rect::from_xywh(x, y + 4.0, tw + 10.0, 17.0), 3.0, 3.0, &Paint::new(color, None));
                self.ui.draw(canvas, text, (x + 5.0, y + 16.5), &Paint::new(Color4f::new(1.0, 1.0, 1.0, 1.0), None));
            }
            for (r, radius, strength) in &glows {
                draw_working(canvas, *r, *radius, *strength, clock, &self.ui);
            }
            for (r, t) in &scans {
                draw_scan(canvas, *r, *t);
            }
            if let Some((p, _)) = self.renderer.tips.last() {
                draw_nib(canvas, p.x / self.dpr, p.y / self.dpr);
            }
        }
        controls.draw(canvas, self.dark, self.ctrl_hover, &self.ui);
        canvas.restore();
        if let Err(e) = self.gr.flush_and_submit_surface(&mut self.surface, None) {
            eprintln!("study: frame not submitted: {e}");
        }
        self.renderer.needs_redraw
            || self.renderer.shaders.uses_time
            || !self.anims.is_empty()
            || !self.scans.is_empty()
            || !self.working.is_empty()
    }
}

/// Runs a scene's script nodes with results the page has already computed, queueing the rest.
/// Returns true if the scene has script nodes.
fn run_scripts(s: &mut RootScene, doc: &Document, vars: &Vars, imports: &Imports, base: &Path, runs: &mut ScriptRuns, warnings: &mut Vec<String>) -> bool {
    let jobs: Vec<(u32, Arc<str>, Arc<Map<String, Value>>, f32, f32)> = s
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| match &n.kind {
            Kind::Script { uri, inputs, .. } if n.enabled => Some((i as u32, uri.clone(), inputs.clone(), n.rect.w, n.rect.h)),
            _ => None,
        })
        .collect();
    for (i, uri, inputs, w, h) in &jobs {
        let job = std::fs::read_to_string(base.join(&**uri))
            .map_err(|_| format!("{uri}: the page didn't provide this script (see `readFile`)"))
            .and_then(|src| pen::script::prepare(&src, *w, *h, inputs).map_err(|e| format!("{uri}: {e}")));
        let result = match job {
            Ok(job) => match runs.done.get(&job.key) {
                Some(r) => Some(r.clone().map_err(|e| format!("{uri}: {e}"))),
                None => {
                    if runs.asked.insert(job.key) {
                        runs.queue.push((job.key, job.code));
                    }
                    None
                }
            },
            Err(e) => Some(Err(e)),
        };
        match result {
            Some(Ok(nodes)) => graft(s, doc, vars, Some(imports), *i, &nodes),
            Some(Err(e)) => {
                s.nodes[*i as usize].kind = Kind::Missing;
                if !warnings.contains(&e) {
                    warnings.push(e);
                }
            }
            // Running in the page; the node stays empty until the result arrives.
            None => {}
        }
    }
    !jobs.is_empty()
}

fn draw_quad(canvas: &Canvas, q: &[(f32, f32); 4], color: Color4f, width: f32) {
    let mut b = PathBuilder::new();
    b.move_to(q[0]);
    for p in &q[1..] {
        b.line_to(*p);
    }
    b.close();
    let mut p = Paint::new(color, None);
    p.set_style(PaintStyle::Stroke);
    p.set_stroke_width(width);
    p.set_anti_alias(true);
    canvas.draw_path(&b.detach(), &p);
}

fn make_surface(gr: &mut DirectContext, fb: FramebufferInfo, w: i32, h: i32) -> Surface {
    let target = gpu::backend_render_targets::make_gl((w.max(1), h.max(1)), 0, 8, fb);
    gpu::surfaces::wrap_backend_render_target(gr, &target, gpu::SurfaceOrigin::BottomLeft, skia_safe::ColorType::RGBA8888, None, None)
        .expect("Skia surface")
}

fn ellipsize(font: &UiFont, s: &str, max: f32) -> String {
    if font.measure(s) <= max {
        return s.to_string();
    }
    let mut out = s.to_string();
    while !out.is_empty() && font.measure(&format!("{out}…")) > max {
        out.pop();
    }
    format!("{out}…")
}

/// A page path without `.`/`..` segments, or `None` if it climbs above the root.
fn clean(path: &str) -> Option<PathBuf> {
    let path = path.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/").into())
}

/// Extensions of files a document can refer to (images, shader and script sources, imports).
const FILE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "svg", "avif", "js", "mjs", "pen", "frag", "glsl", "sksl", "fs", "shader"];

/// Relative files `text` (the JSON of the document at page path `path`) refers to and that
/// aren't in the filesystem yet. Imported documents already provided are scanned too.
fn file_refs(text: &str, path: &str, missing: &mut BTreeSet<String>, seen: &mut HashSet<String>) {
    if !seen.insert(path.to_string()) {
        return;
    }
    let dir = Path::new(path).parent().map(|d| d.to_string_lossy().replace('\\', "/")).unwrap_or_default();
    for lit in string_literals(text) {
        let lower = lit.to_ascii_lowercase();
        let looks_like_file = lit.len() < 400
            && !lit.contains(char::is_whitespace)
            && !lit.starts_with('/')
            && !lit.starts_with('#')
            && !lit.starts_with('$')
            && !lower.contains(':')
            && lower.rsplit_once('.').is_some_and(|(_, ext)| FILE_EXTS.contains(&ext));
        if !looks_like_file {
            continue;
        }
        let Some(rel) = clean(&format!("{dir}/{lit}")) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let file = Path::new(FILES).join(&rel);
        match std::fs::read_to_string(&file) {
            Ok(sub) if rel.ends_with(".pen") => file_refs(&sub, &rel, missing, seen),
            Ok(_) => {}
            Err(_) if file.exists() => {}
            Err(_) => {
                missing.insert(rel);
            }
        }
    }
}

/// The contents of the JSON string literals in `text` (escapes other than `\/` and `\\` kept).
fn string_literals(text: &str) -> impl Iterator<Item = String> + '_ {
    let bytes = text.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            if bytes[i] != b'"' {
                i += 1;
                continue;
            }
            let start = i + 1;
            let mut j = start;
            let mut escaped = false;
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    escaped = true;
                    j += 1;
                }
                j += 1;
            }
            i = j + 1;
            let s = &text[start..j.min(bytes.len())];
            return Some(if escaped { s.replace("\\/", "/").replace("\\\\", "\\") } else { s.to_string() });
        }
        None
    })
}

// ------------------------------------------------------------------ exports

fn viewer<'a>(v: *mut Viewer) -> &'a mut Viewer {
    unsafe { v.as_mut() }.expect("study: null viewer")
}

fn bytes<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if len == 0 {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

fn string(ptr: *const u8, len: usize) -> String {
    String::from_utf8_lossy(bytes(ptr, len)).into_owned()
}

fn keys(ptr: *const u8, len: usize) -> Vec<String> {
    serde_json::from_slice(bytes(ptr, len)).unwrap_or_default()
}

/// Hands a string to the page (null for `None`); valid until the next call that returns one.
fn give(v: &mut Viewer, s: Option<String>) -> *const c_char {
    match s {
        Some(s) => {
            v.out = CString::new(s.replace('\0', "")).unwrap_or_default();
            v.out.as_ptr()
        }
        None => std::ptr::null(),
    }
}

/// Buffers the page writes strings and file contents into.
#[unsafe(no_mangle)]
pub extern "C" fn study_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn study_dealloc(ptr: *mut u8, len: usize) {
    drop(unsafe { Vec::from_raw_parts(ptr, 0, len.max(1)) });
}

/// Creates a viewer on the page's current WebGL2 context; `w`×`h` in device pixels.
#[unsafe(no_mangle)]
pub extern "C" fn study_new(w: i32, h: i32, dpr: f32) -> *mut Viewer {
    Box::into_raw(Box::new(Viewer::new(w, h, dpr)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn study_destroy(v: *mut Viewer) {
    if !v.is_null() {
        drop(unsafe { Box::from_raw(v) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn study_resize(v: *mut Viewer, w: i32, h: i32, dpr: f32) {
    viewer(v).resize(w, h, dpr);
}

/// Files the document (UTF-8 JSON, at page path `path`) refers to that the page hasn't provided:
/// a JSON array of page paths.
#[unsafe(no_mangle)]
pub extern "C" fn study_file_refs(v: *mut Viewer, ptr: *const u8, len: usize, path: *const u8, path_len: usize) -> *const c_char {
    let path = clean(&string(path, path_len)).map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_else(|| "document.pen".into());
    let text = String::from_utf8_lossy(bytes(ptr, len));
    let mut missing = BTreeSet::new();
    file_refs(&text, &path, &mut missing, &mut HashSet::new());
    let list = serde_json::to_string(&missing).unwrap_or_default();
    give(viewer(v), Some(list))
}

/// Loads `.pen` JSON (UTF-8) kept at page path `path`. Returns null on success, else an error.
#[unsafe(no_mangle)]
pub extern "C" fn study_load(v: *mut Viewer, ptr: *const u8, len: usize, path: *const u8, path_len: usize) -> *const c_char {
    let v = viewer(v);
    let text = String::from_utf8_lossy(bytes(ptr, len)).into_owned();
    let err = v.load(&text, &string(path, path_len)).err();
    give(v, err)
}

/// Provides a file at a page path (images, shaders, scripts, imported documents).
#[unsafe(no_mangle)]
pub extern "C" fn study_add_file(v: *mut Viewer, path: *const u8, path_len: usize, data: *const u8, len: usize) -> bool {
    let v = viewer(v);
    let Some(rel) = clean(&string(path, path_len)) else { return false };
    let file = Path::new(FILES).join(rel);
    let ok = file.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok()) && std::fs::write(&file, bytes(data, len)).is_ok();
    v.renderer.images.forget(&file.to_string_lossy());
    ok
}

#[unsafe(no_mangle)]
pub extern "C" fn study_frame(v: *mut Viewer) -> bool {
    viewer(v).frame()
}

/// Moves the view by (dx, dy) CSS pixels.
#[unsafe(no_mangle)]
pub extern "C" fn study_pan(v: *mut Viewer, dx: f32, dy: f32) {
    let v = viewer(v);
    v.cam.x -= dx / v.cam.zoom;
    v.cam.y -= dy / v.cam.zoom;
    v.clamp_cam();
}

/// Zooms by `factor` around (x, y) in CSS pixels.
#[unsafe(no_mangle)]
pub extern "C" fn study_zoom(v: *mut Viewer, factor: f32, x: f32, y: f32) {
    viewer(v).zoom_at(factor, x, y);
}

/// Fits the given nodes (JSON array of ids), or everything for an empty list.
#[unsafe(no_mangle)]
pub extern "C" fn study_fit(v: *mut Viewer, ptr: *const u8, len: usize) {
    let v = viewer(v);
    let boxes = keys(ptr, len).iter().filter_map(|k| if v.changes.is_some() { v.list_box(k) } else { v.doc_box(Side::After, k) }).collect::<Vec<_>>();
    let r = boxes.into_iter().reduce(|a, b| a.union(&b));
    v.fit(r);
}

/// `{"x", "y", "zoom"}`: the document point at the canvas's top-left, and CSS pixels per unit.
#[unsafe(no_mangle)]
pub extern "C" fn study_camera(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let c = v.cam;
    give(v, Some(json!({"x": c.x, "y": c.y, "zoom": c.zoom}).to_string()))
}

#[unsafe(no_mangle)]
pub extern "C" fn study_set_camera(v: *mut Viewer, x: f32, y: f32, zoom: f32) {
    let v = viewer(v);
    v.cam = Camera { x, y, zoom: zoom.clamp(0.001, 256.0) };
    v.fitted = true;
    v.clamp_cam();
}

#[unsafe(no_mangle)]
pub extern "C" fn study_set_dark(v: *mut Viewer, dark: bool) {
    viewer(v).dark = dark;
}

/// Fallback font families (JSON array of Google Fonts names) for characters a document's fonts lack.
#[unsafe(no_mangle)]
pub extern "C" fn study_set_fallback_fonts(v: *mut Viewer, ptr: *const u8, len: usize) {
    viewer(v).renderer.text.fonts.set_fallbacks(keys(ptr, len));
}

/// Pointer position in CSS pixels: `@mouse` shaders and the hover outline. Returns true if the
/// hovered node changed.
#[unsafe(no_mangle)]
pub extern "C" fn study_pointer(v: *mut Viewer, x: f32, y: f32, track_hover: bool) -> bool {
    let v = viewer(v);
    v.renderer.mouse = Some(Point::new(x * v.dpr, y * v.dpr));
    let controls = v.controls();
    v.ctrl_hover = controls.hit(x, y);
    let hover = if track_hover && !controls.covers(x, y) { v.hit(x, y) } else { None };
    let changed = hover != v.hover;
    v.hover = hover;
    changed
}

/// Which built-in controls to draw: the breadcrumb, the zoom buttons, the light/dark switch.
#[unsafe(no_mangle)]
pub extern "C" fn study_set_controls(v: *mut Viewer, breadcrumb: bool, zoom: bool, theme: bool) {
    viewer(v).show = Show { breadcrumb, zoom, theme };
}

/// Whether a point (CSS px) is on a built-in control's panel; the canvas ignores it there.
#[unsafe(no_mangle)]
pub extern "C" fn study_on_controls(v: *mut Viewer, x: f32, y: f32) -> bool {
    viewer(v).controls().covers(x, y)
}

/// Whether the pointer is over a clickable control (for the cursor).
#[unsafe(no_mangle)]
pub extern "C" fn study_over_control(v: *mut Viewer) -> bool {
    viewer(v).ctrl_hover.is_some()
}

/// A click at (x, y) CSS px on a built-in control: acts on it and returns JSON saying which kind
/// (`{"control": "breadcrumb" | "zoom" | "theme", "dark"?}`), or null if there's none there.
#[unsafe(no_mangle)]
pub extern "C" fn study_press(v: *mut Viewer, x: f32, y: f32) -> *const c_char {
    let v = viewer(v);
    let Some(c) = v.controls().hit(x, y) else { return std::ptr::null() };
    let what = v.press(c).to_string();
    give(v, Some(what))
}

/// The breadcrumb: the last selected node and its ancestors, top-level frame first (JSON ids).
#[unsafe(no_mangle)]
pub extern "C" fn study_breadcrumb(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let ids: Vec<String> = v.crumbs().into_iter().map(|c| c.key).collect();
    give(v, serde_json::to_string(&ids).ok())
}

#[unsafe(no_mangle)]
pub extern "C" fn study_pointer_left(v: *mut Viewer) -> bool {
    let v = viewer(v);
    v.renderer.mouse = None;
    v.hover.take().is_some()
}

/// `{"id", "side"}` of a node and the version it's in (`before` only in the changes layout).
fn side_key(h: Option<(String, Side)>) -> Option<String> {
    h.map(|(id, side)| json!({"id": id, "side": side.name()}).to_string())
}

/// The hovered node as `{"id", "side"}`, or null.
#[unsafe(no_mangle)]
pub extern "C" fn study_hovered(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let h = side_key(v.hover.clone());
    give(v, h)
}

/// The node under a point in CSS pixels as `{"id", "side"}`, or null.
#[unsafe(no_mangle)]
pub extern "C" fn study_hit(v: *mut Viewer, x: f32, y: f32) -> *const c_char {
    let v = viewer(v);
    let h = side_key(v.hit(x, y));
    give(v, h)
}

/// A click at (x, y) CSS pixels, selecting like the desktop app (`toggle` = Shift).
#[unsafe(no_mangle)]
pub extern "C" fn study_click(v: *mut Viewer, x: f32, y: f32, toggle: bool) {
    viewer(v).click(x, y, toggle);
}

/// Selection to parents (`up`) or first children.
#[unsafe(no_mangle)]
pub extern "C" fn study_select_relative(v: *mut Viewer, up: bool) {
    viewer(v).select_relative(up);
}

/// Replaces the selection (JSON array of ids) on a side (1: the changes layout's `before`, else
/// the shown document); unknown ids are dropped.
#[unsafe(no_mangle)]
pub extern "C" fn study_select(v: *mut Viewer, ptr: *const u8, len: usize, side: i32) {
    let v = viewer(v);
    let side = Side::from_arg(side);
    let mut list = keys(ptr, len);
    list.retain(|k| v.locate_on(side, k).is_some());
    list.dedup();
    v.selection = list;
    v.sel_side = side;
}

/// The selection, as JSON `{"ids": [...], "side"}`.
#[unsafe(no_mangle)]
pub extern "C" fn study_selection(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let s = json!({"ids": v.selection, "side": v.sel_side.name()}).to_string();
    give(v, Some(s))
}

/// JSON describing a node (see `Viewer::node_info`) on a side (as in `study_select`), or null.
#[unsafe(no_mangle)]
pub extern "C" fn study_node_info(v: *mut Viewer, ptr: *const u8, len: usize, side: i32) -> *const c_char {
    let v = viewer(v);
    let info = v.node_info(&string(ptr, len), Side::from_arg(side)).map(|i| i.to_string());
    give(v, info)
}

/// The node's JSON in its document, or null.
#[unsafe(no_mangle)]
pub extern "C" fn study_node_json(v: *mut Viewer, ptr: *const u8, len: usize, side: i32) -> *const c_char {
    let v = viewer(v);
    let j = v.node_json(&string(ptr, len), Side::from_arg(side)).map(|j| j.to_string());
    give(v, j)
}

/// Renders a node to PNG (`scale` ≤ 0: the screenshot tool's size). Read it with
/// `study_blob_ptr`/`study_blob_len`; false if the node doesn't exist.
#[unsafe(no_mangle)]
pub extern "C" fn study_snapshot(v: *mut Viewer, ptr: *const u8, len: usize, scale: f32, side: i32) -> bool {
    let v = viewer(v);
    let png = v.snapshot(&string(ptr, len), (scale > 0.0).then_some(scale), Side::from_arg(side));
    v.blob = png.unwrap_or_default();
    !v.blob.is_empty()
}

#[unsafe(no_mangle)]
pub extern "C" fn study_blob_ptr(v: *mut Viewer) -> *const u8 {
    viewer(v).blob.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn study_blob_len(v: *mut Viewer) -> usize {
    viewer(v).blob.len()
}

/// Agent presence, as a JSON array of node ids: what it read (scan), the top-level frames it
/// changed (glow), and paths it created (pen draw-in).
#[unsafe(no_mangle)]
pub extern "C" fn study_scan(v: *mut Viewer, ptr: *const u8, len: usize) {
    viewer(v).scan(keys(ptr, len));
}

#[unsafe(no_mangle)]
pub extern "C" fn study_touch(v: *mut Viewer, ptr: *const u8, len: usize) {
    viewer(v).touch(keys(ptr, len));
}

#[unsafe(no_mangle)]
pub extern "C" fn study_draw_in(v: *mut Viewer, ptr: *const u8, len: usize) {
    viewer(v).draw_in(keys(ptr, len));
}

/// Outlines nodes (JSON array of ids) in a highlight `kind`: `added`, `changed`, `removed` or a
/// `#rrggbb` colour. They replace that kind's earlier ones; an empty kind with no ids clears all.
/// False for an unknown kind.
#[unsafe(no_mangle)]
pub extern "C" fn study_highlight(v: *mut Viewer, kind: *const u8, kind_len: usize, ptr: *const u8, len: usize) -> bool {
    viewer(v).highlight(&string(kind, kind_len), keys(ptr, len))
}

/// Lays out another version of the document (UTF-8 JSON at page path `path`) to compare the shown
/// one with. Returns null, or an error. Read the result with `study_compare_result` once
/// `study_settling` is false.
#[unsafe(no_mangle)]
pub extern "C" fn study_compare_start(v: *mut Viewer, ptr: *const u8, len: usize, path: *const u8, path_len: usize) -> *const c_char {
    let v = viewer(v);
    let text = String::from_utf8_lossy(bytes(ptr, len)).into_owned();
    let err = v.compare_start(&text, &string(path, path_len)).err();
    give(v, err)
}

/// Whether font downloads or script runs that could change a layout are still out.
#[unsafe(no_mangle)]
pub extern "C" fn study_settling(v: *mut Viewer) -> bool {
    viewer(v).settling()
}

/// Takes in downloads and script results without drawing, for when the page has no frames (a
/// background tab) but is waiting on them.
#[unsafe(no_mangle)]
pub extern "C" fn study_pump(v: *mut Viewer) {
    viewer(v).pump();
}

/// What changed from the version given to `study_compare_start` to the shown document, as JSON
/// `{"added", "removed", "changed": [{"id", "props", "moved", "bounds"}], "frames"}`, or null.
#[unsafe(no_mangle)]
pub extern "C" fn study_compare_result(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let r = v.compare_result().map(|r| r.to_string());
    give(v, r)
}

/// Draws another document (UTF-8 JSON at page path `path`) over the shown one at `opacity`; an
/// empty document removes it. Returns null, or an error.
#[unsafe(no_mangle)]
pub extern "C" fn study_set_overlay(v: *mut Viewer, ptr: *const u8, len: usize, path: *const u8, path_len: usize, opacity: f32) -> *const c_char {
    let v = viewer(v);
    let text = String::from_utf8_lossy(bytes(ptr, len)).into_owned();
    let err = v.set_overlay(&text, &string(path, path_len), opacity).err();
    give(v, err)
}

#[unsafe(no_mangle)]
pub extern "C" fn study_set_overlay_opacity(v: *mut Viewer, opacity: f32) {
    let v = viewer(v);
    if let Some((_, o)) = &mut v.overlay {
        *o = opacity.clamp(0.0, 1.0);
    }
    if let Some(ch) = &mut v.changes {
        ch.opts.opacity = opacity.clamp(0.0, 1.0);
    }
}

/// Shows the changes layout against another version (UTF-8 JSON at page path `path`) with
/// options (JSON, see `changes::Options::parse`). Returns null, or an error.
#[unsafe(no_mangle)]
pub extern "C" fn study_show_changes(v: *mut Viewer, ptr: *const u8, len: usize, path: *const u8, path_len: usize, opts: *const u8, opts_len: usize) -> *const c_char {
    let v = viewer(v);
    let text = String::from_utf8_lossy(bytes(ptr, len)).into_owned();
    let opts: Value = serde_json::from_slice(bytes(opts, opts_len)).unwrap_or(Value::Null);
    let err = v.show_changes(&text, &string(path, path_len), &opts).err();
    give(v, err)
}

/// Replaces the changes layout's options (JSON). Returns null, or an error.
#[unsafe(no_mangle)]
pub extern "C" fn study_changes_options(v: *mut Viewer, opts: *const u8, opts_len: usize) -> *const c_char {
    let v = viewer(v);
    let opts: Value = serde_json::from_slice(bytes(opts, opts_len)).unwrap_or(Value::Null);
    let err = v.set_changes_options(&opts).err();
    give(v, err)
}

/// Back to the whole document.
#[unsafe(no_mangle)]
pub extern "C" fn study_hide_changes(v: *mut Viewer) {
    viewer(v).hide_changes();
}

/// Bumped whenever the rows' sizes or places change (0 outside the changes layout).
#[unsafe(no_mangle)]
pub extern "C" fn study_changes_version(v: *mut Viewer) -> u32 {
    viewer(v).changes_version()
}

/// The rows as JSON, on screen; null outside the changes layout.
#[unsafe(no_mangle)]
pub extern "C" fn study_changes_rows(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let rows = v.rows_json().map(|r| r.to_string());
    give(v, rows)
}

/// The row at (x, y) CSS px as JSON, with `side`, or null.
#[unsafe(no_mangle)]
pub extern "C" fn study_row_at(v: *mut Viewer, x: f32, y: f32) -> *const c_char {
    let v = viewer(v);
    let row = v.row_at(x, y).map(|r| r.to_string());
    give(v, row)
}

/// Scrolls a row (by top-level id) to the top. False if there's no such row.
#[unsafe(no_mangle)]
pub extern "C" fn study_scroll_to_row(v: *mut Viewer, ptr: *const u8, len: usize) -> bool {
    viewer(v).scroll_to_row(&string(ptr, len))
}

/// Problems since the last call (script and import failures), as a JSON array, or null.
#[unsafe(no_mangle)]
pub extern "C" fn study_warnings(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let w = std::mem::take(&mut v.warnings);
    give(v, (!w.is_empty()).then(|| serde_json::to_string(&w).unwrap_or_default()))
}

/// Script runs to start, as JSON `[[key, code], ...]` (`code` evaluates to the output's JSON).
#[unsafe(no_mangle)]
pub extern "C" fn study_script_jobs(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let jobs: Vec<(String, String)> = std::mem::take(&mut v.scripts.queue).into_iter().map(|(k, c)| (k.to_string(), c)).collect();
    give(v, (!jobs.is_empty()).then(|| serde_json::to_string(&jobs).unwrap_or_default()))
}

/// A script run's outcome: its output JSON, or an error message when `ok` is false.
#[unsafe(no_mangle)]
pub extern "C" fn study_script_result(v: *mut Viewer, key: *const u8, key_len: usize, ok: bool, ptr: *const u8, len: usize) {
    let v = viewer(v);
    let Ok(key) = string(key, key_len).parse::<u64>() else { return };
    let text = string(ptr, len);
    let result = if ok { pen::script::parse_output(&text) } else { Err(text) };
    v.scripts.done.insert(key, result);
    v.scripts.dirty = true;
}

/// Downloads the renderer wants (fonts, icons, images), as JSON `[[id, url], ...]`.
#[unsafe(no_mangle)]
pub extern "C" fn study_requests(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let list = render::net::take_requests();
    give(v, (!list.is_empty()).then(|| serde_json::to_string(&list).unwrap_or_default()))
}

/// Answers request `id`: `status` 200 with the body, an HTTP error status, or 0 for a network error.
#[unsafe(no_mangle)]
pub extern "C" fn study_fulfill(id: u32, status: u32, ptr: *const u8, len: usize) {
    let reply = match status {
        200..=299 => Ok(bytes(ptr, len).to_vec()),
        0 => Err(None),
        s => Err(Some(s as u16)),
    };
    render::net::fulfill(id, reply);
}

/// The `.pen` type a compiled node came from.
fn kind_name(k: &Kind) -> &'static str {
    match k {
        Kind::Frame { .. } => "frame",
        Kind::Group => "group",
        Kind::Rect => "rectangle",
        Kind::Ellipse { .. } => "ellipse",
        Kind::Polygon { .. } => "polygon",
        Kind::Path { .. } => "path",
        Kind::Text(_) => "text",
        Kind::Icon { .. } => "icon_font",
        Kind::Note(..) => "note",
        Kind::Script { .. } => "script",
        Kind::Missing => "unknown",
    }
}
