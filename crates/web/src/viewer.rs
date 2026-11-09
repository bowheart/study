//! The viewer behind the `study_*` exports: one document, a camera, selection, and the
//! agent-presence overlays, drawn like the desktop window's canvas (`crates/study/src/gui.rs`).
//!
//! Files the document refers to (images, shaders, script sources, imported `.pen` files) live in
//! Emscripten's in-memory filesystem under `/doc`, at the paths the page knows them by. The page
//! asks `study_file_refs` which are missing and supplies them before `study_load`. Script nodes are
//! evaluated by the page in a sandboxed worker: `study_script_jobs` hands out the wrapped code and
//! `study_script_result` brings the output back.

use pen::imports::Imports;
use pen::layout::{compute_bounds, layout};
use pen::scene::{compile_root_with, graft, Kind, Rect, RootScene, NONE};
use pen::vars::Vars;
use pen::{Document, NodeId};
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
    /// Roots that contain script nodes (rebuilt when results arrive).
    roots: HashSet<NodeId>,
    /// Set when results arrived; the next frame rebuilds `roots`.
    dirty: bool,
}

pub struct Viewer {
    gr: DirectContext,
    fb: FramebufferInfo,
    surface: Surface,
    /// Canvas size in CSS pixels, and device pixels per CSS pixel.
    size: (f32, f32),
    dpr: f32,
    doc: Option<Document>,
    vars: Option<Vars>,
    imports: Imports,
    scenes: HashMap<NodeId, RootScene>,
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
    selection: Vec<String>,
    hover: Option<String>,
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
            doc: None,
            vars: None,
            imports: Imports::default(),
            scenes: HashMap::new(),
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
            hover: None,
            ui: UiFont(None),
            out: CString::default(),
            blob: Vec::new(),
        }
    }

    fn resize(&mut self, w: i32, h: i32, dpr: f32) {
        self.surface = make_surface(&mut self.gr, self.fb, w, h);
        self.size = (w as f32 / dpr, h as f32 / dpr);
        self.dpr = dpr;
    }

    /// Replaces the document; `path` is where the page keeps it (relative files resolve from
    /// there). The camera stays put, except on the first load, which fits.
    fn load(&mut self, text: &str, path: &str) -> Result<(), String> {
        let mut doc = Document::parse(text).map_err(|e| format!("{e:#}"))?;
        let file = Path::new(FILES).join(clean(path).unwrap_or_else(|| "document.pen".into()));
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_else(|| FILES.into());
        let _ = std::fs::create_dir_all(&dir);
        doc.path = Some(file);
        self.renderer.base_dir = dir;
        self.imports = Imports::load(&doc);
        self.warnings.extend(self.imports.errors.iter().cloned());
        let mut vars = Vars::new(&doc);
        vars.add_imported(&self.imports.variables);
        self.vars = Some(vars);
        self.doc = Some(doc);
        self.scripts.roots.clear();
        self.rebuild(None);
        let selection = std::mem::take(&mut self.selection);
        self.selection = selection.into_iter().filter(|k| self.locate(k).is_some()).collect();
        if self.hover.as_ref().is_some_and(|k| self.locate(k).is_none()) {
            self.hover = None;
        }
        Ok(())
    }

    /// Compiles and lays out the given roots (all when `None`), running script nodes.
    fn rebuild(&mut self, only: Option<Vec<NodeId>>) {
        let (Some(doc), Some(vars)) = (&self.doc, &self.vars) else { return };
        let roots = only.unwrap_or_else(|| doc.roots.clone());
        if roots.len() == doc.roots.len() {
            self.scenes.clear();
        }
        let base = self.renderer.base_dir.clone();
        for r in roots {
            let mut s = compile_root_with(doc, vars, r, Some(&self.imports));
            layout(&mut s, &mut self.renderer.text);
            if run_scripts(&mut s, doc, vars, &self.imports, &base, &mut self.scripts, &mut self.warnings) {
                self.scripts.roots.insert(r);
                layout(&mut s, &mut self.renderer.text);
            }
            compute_bounds(&mut s);
            self.scenes.insert(r, s);
        }
    }

    fn relayout(&mut self) {
        for s in self.scenes.values_mut() {
            layout(s, &mut self.renderer.text);
            compute_bounds(s);
        }
    }

    fn bounds(&self) -> Option<Rect> {
        let doc = self.doc.as_ref()?;
        doc.roots.iter().filter_map(|r| self.scenes.get(r)?.bounds.first().copied()).filter(|b| b.w >= 0.0).reduce(|a, b| a.union(&b))
    }

    fn fit(&mut self, r: Option<Rect>) {
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
    }

    fn viewport(&self) -> Rect {
        Rect::new(self.cam.x, self.cam.y, self.size.0 / self.cam.zoom, self.size.1 / self.cam.zoom)
    }

    /// Scene and index for a node id or instance path.
    fn locate(&self, key: &str) -> Option<(&RootScene, u32)> {
        let doc = self.doc.as_ref()?;
        let n = doc.find(key.split('/').next()?)?;
        if !doc.node(n).attached {
            return None;
        }
        let scene = self.scenes.get(&doc.root_of(n))?;
        let i = if key.contains('/') { scene.lookup(key, doc)? } else { scene.lookup_node(n)? };
        Some((scene, i))
    }

    fn abs_rect(&self, key: &str) -> Option<Rect> {
        let (s, i) = self.locate(key)?;
        Some(s.abs_rect(i))
    }

    /// A node's corners in document space (rotation included).
    fn corners(&self, key: &str) -> Option<[(f32, f32); 4]> {
        let (s, i) = self.locate(key)?;
        let (m, w, h) = s.transform(i);
        Some([m.map(0.0, 0.0), m.map(w, 0.0), m.map(w, h), m.map(0.0, h)])
    }

    /// Bounding box of a node in document space, rotation included.
    fn doc_box(&self, key: &str) -> Option<Rect> {
        let pts = self.corners(key)?;
        let (l, t) = pts.iter().fold((f32::MAX, f32::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
        let (r, b) = pts.iter().fold((f32::MIN, f32::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
        Some(Rect::new(l, t, r - l, b - t))
    }

    fn to_screen(&self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.cam.x) * self.cam.zoom, (y - self.cam.y) * self.cam.zoom)
    }

    /// Deepest selectable node under a point in CSS pixels.
    fn hit(&self, sx: f32, sy: f32) -> Option<String> {
        let doc = self.doc.as_ref()?;
        let (x, y) = (self.cam.x + sx / self.cam.zoom, self.cam.y + sy / self.cam.zoom);
        doc.roots.iter().rev().filter_map(|r| self.scenes.get(r)).find_map(|s| s.hit(x, y).map(|i| s.nodes[i as usize].key().to_string()))
    }

    /// The desktop's click: select what's under the point (`toggle` adds/removes it).
    fn click(&mut self, sx: f32, sy: f32, toggle: bool) {
        match (self.hit(sx, sy), toggle) {
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
            let Some((s, i)) = self.locate(k) else { continue };
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
    fn node_info(&self, key: &str) -> Option<Value> {
        let doc = self.doc.as_ref()?;
        let (s, i) = self.locate(key)?;
        let n = &s.nodes[i as usize];
        let b = self.doc_box(key)?;
        let (sx, sy) = self.to_screen(b.x, b.y);
        let z = self.cam.zoom;
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
            "bounds": {"x": b.x, "y": b.y, "width": b.w, "height": b.h},
            "screen": {"x": sx, "y": sy, "width": b.w * z, "height": b.h * z},
        }))
    }

    /// The node's own JSON from the document (inside an instance: the component's node).
    fn node_json(&self, key: &str) -> Option<Value> {
        let doc = self.doc.as_ref()?;
        let (s, i) = self.locate(key)?;
        Some(doc.to_value(s.nodes[i as usize].src?))
    }

    /// PNG of a node and what's drawn over it, like the MCP screenshot tool.
    fn snapshot(&mut self, key: &str, scale: Option<f32>) -> Option<Vec<u8>> {
        let region = self.doc_box(key)?;
        let scale = scale.unwrap_or((SNAPSHOT_EDGE / region.w.max(region.h)).min(2.0));
        let doc = self.doc.as_ref()?;
        let scenes: Vec<&RootScene> = doc.roots.iter().filter_map(|r| self.scenes.get(r)).collect();
        let img = render::offscreen::render_region(&mut self.renderer, &scenes, region, scale, None)?;
        render::offscreen::encode(&img, "png", None)
    }

    /// Nodes an agent read: a scan line sweeps each, staggered.
    fn scan(&mut self, mut keys: Vec<String>) {
        let view = self.viewport();
        let mut seen = HashSet::new();
        keys.retain(|k| seen.insert(k.clone()) && self.abs_rect(k).is_some_and(|r| r.intersects(&view)));
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
        let view = self.viewport();
        let zoom = self.cam.zoom;
        let items: Vec<(String, f32)> = keys
            .into_iter()
            .filter_map(|k| {
                let r = self.abs_rect(&k)?;
                let px = (r.w + r.h) * zoom;
                (r.intersects(&view) && px > 2.0).then(|| (k, (0.3 + px / 900.0).clamp(0.3, 1.1)))
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

    /// Draws a frame. Returns true if another should follow (animations, loading thumbnails).
    fn frame(&mut self) -> bool {
        if std::mem::take(&mut self.scripts.dirty) {
            let roots: Vec<NodeId> = self.scripts.roots.iter().copied().collect();
            self.rebuild(Some(roots));
        }
        let (fonts, _) = self.renderer.poll();
        if fonts {
            self.relayout();
        }
        if self.ui.0.is_none() {
            // Asked again each frame: its stylesheet arriving doesn't load the font by itself.
            self.renderer.text.fonts.request(UI_FAMILY, 400, false);
            // Until Inter arrives, the lookup falls back to an empty typeface.
            let tf = self.renderer.text.fonts.collection.find_typefaces(&[UI_FAMILY], FontStyle::normal());
            self.ui = UiFont(tf.into_iter().find(|t| t.family_name() == UI_FAMILY).map(|t| Font::new(t, 12.0)));
        }
        if !self.fitted {
            self.fit(None);
        }
        let now = Instant::now();
        let cam = self.cam;
        let (ww, wh) = self.size;
        let to_screen = |r: Rect| skia_safe::Rect::from_xywh((r.x - cam.x) * cam.zoom, (r.y - cam.y) * cam.zoom, r.w * cam.zoom, r.h * cam.zoom);

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
            .filter_map(|(k, s)| Some((to_screen(self.doc_box(k)?), (now - *s).as_secs_f32() / SCAN_SECS)))
            .collect();
        self.working.retain(|_, (_, last)| now.duration_since(*last).as_secs_f32() < WORK_HOLD + WORK_FADE);
        let glows: Vec<_> = self
            .working
            .iter()
            .filter_map(|(k, (first, last))| {
                let (s, i) = self.locate(k)?;
                let fade_in = (now.duration_since(*first).as_secs_f32() / 0.4).min(1.0);
                let idle = now.duration_since(*last).as_secs_f32();
                let fade_out = 1.0 - ((idle - WORK_HOLD) / WORK_FADE).clamp(0.0, 1.0);
                Some((to_screen(s.abs_rect(i)), s.nodes[i as usize].radius[0] * cam.zoom, fade_in * fade_out))
            })
            .collect();
        // Selection and hover outlines, in screen space, with the selection's name and size.
        let outline = |key: &str| -> Option<[(f32, f32); 4]> { Some(self.corners(key)?.map(|(x, y)| ((x - cam.x) * cam.zoom, (y - cam.y) * cam.zoom))) };
        let hover = self.hover.as_ref().filter(|h| !self.selection.contains(h)).and_then(|h| outline(h));
        let selected: Vec<_> = self
            .selection
            .iter()
            .filter_map(|k| {
                let (s, i) = self.locate(k)?;
                let n = &s.nodes[i as usize];
                let label = format!("{}  {}×{}", n.name.as_deref().unwrap_or(&n.id), n.rect.w.round(), n.rect.h.round());
                Some((outline(k)?, label))
            })
            .collect();
        let clock = now.duration_since(self.started).as_secs_f32();

        let viewport = self.viewport();
        let canvas = self.surface.canvas();
        canvas.clear(if self.dark { Color::from_rgb(0x1E, 0x1E, 0x20) } else { Color::from_rgb(0xEC, 0xEC, 0xEC) });
        canvas.save();
        canvas.scale((self.dpr, self.dpr));
        if let Some(doc) = &self.doc {
            canvas.save();
            canvas.scale((cam.zoom, cam.zoom));
            canvas.translate((-cam.x, -cam.y));
            let view = View { scale: cam.zoom * self.dpr, viewport, lod: true };
            self.renderer.begin_frame();
            self.renderer.reveal = reveal;
            for r in &doc.roots {
                if let Some(s) = self.scenes.get(r) {
                    self.renderer.draw_scene(canvas, s, &view);
                }
            }
            self.renderer.text.end_frame();
            canvas.restore();

            let label = Paint::new(if self.dark { Color4f::new(0.74, 0.74, 0.77, 1.0) } else { Color4f::new(0.42, 0.42, 0.45, 1.0) }, None);
            for r in &doc.roots {
                let Some(n) = self.scenes.get(r).and_then(|s| s.root()) else { continue };
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
    let r = keys(ptr, len).iter().filter_map(|k| v.doc_box(k)).reduce(|a, b| a.union(&b));
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
    let hover = if track_hover { v.hit(x, y) } else { None };
    let changed = hover != v.hover;
    v.hover = hover;
    changed
}

#[unsafe(no_mangle)]
pub extern "C" fn study_pointer_left(v: *mut Viewer) -> bool {
    let v = viewer(v);
    v.renderer.mouse = None;
    v.hover.take().is_some()
}

#[unsafe(no_mangle)]
pub extern "C" fn study_hovered(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let h = v.hover.clone();
    give(v, h)
}

/// The node under a point in CSS pixels, or null.
#[unsafe(no_mangle)]
pub extern "C" fn study_hit(v: *mut Viewer, x: f32, y: f32) -> *const c_char {
    let v = viewer(v);
    let h = v.hit(x, y);
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

/// Replaces the selection (JSON array of ids); unknown ids are dropped.
#[unsafe(no_mangle)]
pub extern "C" fn study_select(v: *mut Viewer, ptr: *const u8, len: usize) {
    let v = viewer(v);
    let mut list = keys(ptr, len);
    list.retain(|k| v.locate(k).is_some());
    list.dedup();
    v.selection = list;
}

/// The selection, as a JSON array of ids.
#[unsafe(no_mangle)]
pub extern "C" fn study_selection(v: *mut Viewer) -> *const c_char {
    let v = viewer(v);
    let s = serde_json::to_string(&v.selection).ok();
    give(v, s)
}

/// JSON describing a node (see `Viewer::node_info`), or null if it isn't in the document.
#[unsafe(no_mangle)]
pub extern "C" fn study_node_info(v: *mut Viewer, ptr: *const u8, len: usize) -> *const c_char {
    let v = viewer(v);
    let info = v.node_info(&string(ptr, len)).map(|i| i.to_string());
    give(v, info)
}

/// The node's JSON in the document, or null.
#[unsafe(no_mangle)]
pub extern "C" fn study_node_json(v: *mut Viewer, ptr: *const u8, len: usize) -> *const c_char {
    let v = viewer(v);
    let j = v.node_json(&string(ptr, len)).map(|j| j.to_string());
    give(v, j)
}

/// Renders a node to PNG (`scale` ≤ 0: the screenshot tool's size). Read it with
/// `study_blob_ptr`/`study_blob_len`; false if the node doesn't exist.
#[unsafe(no_mangle)]
pub extern "C" fn study_snapshot(v: *mut Viewer, ptr: *const u8, len: usize, scale: f32) -> bool {
    let v = viewer(v);
    let png = v.snapshot(&string(ptr, len), (scale > 0.0).then_some(scale));
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
