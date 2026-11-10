//! The changes layout (`showChanges`): the top-level frames that differ between two versions, one
//! row each, the older version on the left and the shown one on the right (or stacked translucently,
//! `onion`), drawn by the viewer's one renderer.
//!
//! Rows live in *list space*: CSS pixels at zoom 1, as wide as the canvas. The camera scrolls and
//! zooms over it like it does over a document. Each side of a row draws its frame from the real,
//! fully laid-out document, scaled and moved into its cell, so variables, components, imports,
//! fonts and scripts are exactly as in a normal load, and node ids stay the documents' own.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Side {
    Before,
    After,
}

impl Side {
    pub(super) fn name(self) -> &'static str {
        match self {
            Side::Before => "before",
            Side::After => "after",
        }
    }

    /// From the exports' `side` argument: 1 is `before`, anything else `after`.
    pub(super) fn from_arg(side: i32) -> Side {
        if side == 1 { Side::Before } else { Side::After }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Side,
    Onion,
}

#[derive(Clone, Copy, PartialEq)]
enum Fit {
    Width,
    Contain,
}

pub(super) struct Options {
    mode: Mode,
    pub(super) opacity: f32,
    fit: Fit,
    max_row_height: Option<f32>,
    gap: f32,
    gutter: f32,
    padding: f32,
    /// Top-level ids to show, or the frames `compare` reports.
    frames: Option<Vec<String>>,
    /// Outline colours: added, changed, removed.
    colors: [Color4f; 3],
}

impl Options {
    /// `{mode, opacity, fit, maxRowHeight, gap, gutter, padding, frames, colors}`, all optional.
    pub(super) fn parse(v: &Value) -> Result<Options, String> {
        let num = |k: &str, d: f32| v.get(k).and_then(Value::as_f64).map_or(d, |n| n as f32).max(0.0);
        let mode = match v.get("mode").and_then(Value::as_str).unwrap_or("side") {
            "side" => Mode::Side,
            "onion" => Mode::Onion,
            m => return Err(format!("study: unknown changes mode \"{m}\" (\"side\" or \"onion\")")),
        };
        let fit = match v.get("fit").and_then(Value::as_str).unwrap_or("width") {
            "width" => Fit::Width,
            "contain" => Fit::Contain,
            f => return Err(format!("study: unknown fit \"{f}\" (\"width\" or \"contain\")")),
        };
        let mut colors = [highlight_color("added").unwrap(), highlight_color("changed").unwrap(), highlight_color("removed").unwrap()];
        for (i, k) in ["added", "changed", "removed"].iter().enumerate() {
            if let Some(c) = v.get("colors").and_then(|c| c.get(k)).and_then(Value::as_str) {
                colors[i] = highlight_color(c).filter(|_| c.starts_with('#')).ok_or_else(|| format!("study: colors.{k} must be #rrggbb, not \"{c}\""))?;
            }
        }
        let frames = v.get("frames").and_then(Value::as_array).map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).collect());
        Ok(Options {
            mode,
            opacity: num("opacity", 0.5).min(1.0),
            fit,
            max_row_height: v.get("maxRowHeight").and_then(Value::as_f64).map(|n| (n as f32).max(1.0)),
            gap: num("gap", 32.0),
            gutter: num("gutter", 24.0),
            padding: num("padding", 24.0),
            frames,
            colors,
        })
    }
}

/// Where one side of a row is drawn.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Place {
    /// The top-level node, in that side's document.
    pub(super) root: NodeId,
    /// Its box in its document (rotation included).
    frame: Rect,
    /// Where it's drawn, in list space.
    at: Rect,
    scale: f32,
}

impl Place {
    /// Document space to list space.
    fn xf(&self) -> Xf {
        Xf { k: self.scale, dx: self.at.x - self.frame.x * self.scale, dy: self.at.y - self.frame.y * self.scale }
    }
}

/// A row before it's placed.
#[derive(Clone)]
struct Spec {
    id: String,
    name: Option<String>,
    kind: &'static str,
    props: Vec<String>,
    moved: bool,
    before: Option<(NodeId, Rect)>,
    after: Option<(NodeId, Rect)>,
}

#[derive(PartialEq)]
pub(super) struct Row {
    id: String,
    name: Option<String>,
    kind: &'static str,
    props: Vec<String>,
    moved: bool,
    before: Option<Place>,
    after: Option<Place>,
    /// Its band in list space.
    y: f32,
    h: f32,
}

impl Row {
    fn place(&self, side: Side) -> Option<&Place> {
        match side {
            Side::Before => self.before.as_ref(),
            Side::After => self.after.as_ref(),
        }
    }
}

pub(super) struct Changes {
    pub(super) before: Layer,
    pub(super) opts: Options,
    specs: Vec<Spec>,
    rows: Vec<Row>,
    /// Outlines per side: colour and keys.
    marks: Vec<(Side, Color4f, Vec<String>)>,
    /// List width (the canvas's when laid out), content height, column width and lefts.
    width: f32,
    height: f32,
    col_w: f32,
    cols: [f32; 2],
    /// Bumped whenever row geometry changes (the page's `layoutchange`).
    pub(super) version: u32,
    /// The camera to go back to, and whether it had been fitted.
    saved: (Camera, bool),
}

impl Changes {
    pub(super) fn new(before: Layer, opts: Options, saved: (Camera, bool)) -> Changes {
        Changes { before, opts, specs: Vec::new(), rows: Vec::new(), marks: Vec::new(), width: 0.0, height: 0.0, col_w: 0.0, cols: [0.0; 2], version: 0, saved }
    }

    fn onion(&self) -> bool {
        self.opts.mode == Mode::Onion
    }

    /// The region a side of a row is clipped to (and hit-tested in), in list space.
    fn cell(&self, side: Side, r: &Row) -> Rect {
        let col = if self.onion() || side == Side::Before { self.cols[0] } else { self.cols[1] };
        let (gx, gy) = (self.opts.gutter / 2.0, self.opts.gap / 2.0);
        Rect::new(col - gx, r.y - gy, self.col_w + 2.0 * gx, r.h + 2.0 * gy)
    }

    /// Places the rows for a canvas `width` × `view_h` CSS px. True if anything moved.
    fn place(&mut self, width: f32, view_h: f32) -> bool {
        let o = &self.opts;
        let (p, onion) = (o.padding, o.mode == Mode::Onion);
        let col_w = if onion { width - 2.0 * p } else { (width - 2.0 * p - o.gutter) / 2.0 }.max(1.0);
        let cols = [p, if onion { p } else { p + col_w + o.gutter }];
        let limit = match o.fit {
            Fit::Contain => Some(o.max_row_height.unwrap_or((view_h - 2.0 * p).max(40.0))),
            Fit::Width => o.max_row_height,
        };
        let mut y = p;
        let mut rows = Vec::with_capacity(self.specs.len());
        for s in &self.specs {
            let boxes = [s.before.map(|b| b.1), s.after.map(|a| a.1)];
            let w = boxes.iter().flatten().map(|b| b.w).fold(1.0f32, f32::max);
            let h = boxes.iter().flatten().map(|b| b.h).fold(1.0f32, f32::max);
            let mut scale = col_w / w;
            if let Some(l) = limit {
                scale = scale.min(l / h);
            }
            let off = (col_w - w * scale) / 2.0;
            let place = |b: Option<(NodeId, Rect)>, col: f32| {
                b.map(|(root, frame)| Place { root, frame, at: Rect::new(col + off, y, frame.w * scale, frame.h * scale), scale })
            };
            rows.push(Row {
                id: s.id.clone(),
                name: s.name.clone(),
                kind: s.kind,
                props: s.props.clone(),
                moved: s.moved,
                before: place(s.before, cols[0]),
                after: place(s.after, cols[1]),
                y,
                h: h * scale,
            });
            y += h * scale + o.gap;
        }
        let height = if rows.is_empty() { 2.0 * p } else { y - o.gap + p };
        let changed = rows != self.rows || height != self.height || width != self.width;
        (self.rows, self.width, self.height, self.col_w, self.cols) = (rows, width, height, col_w, cols);
        if changed {
            self.version += 1;
        }
        changed
    }

    /// Compares the versions again and rebuilds the rows' contents (not their places).
    fn diff(&mut self, main: &Layer) {
        let d = pen::diff::compare(&self.before.version(), &main.version());
        let (a, b) = (&main.doc, &self.before.doc);
        let index = |doc: &Document, id: &str| doc.roots.iter().position(|r| doc.node(*r).id() == id);
        let frames = self.opts.frames.clone().unwrap_or_else(|| d.frames.clone());
        let mut keyed: Vec<((i64, u8, usize), Spec)> = Vec::new();
        for id in frames {
            if keyed.iter().any(|(_, s)| s.id == id) {
                continue;
            }
            let (ai, bi) = (index(a, &id), index(b, &id));
            let side = |l: &Layer, i: Option<usize>| -> Option<(NodeId, Rect)> {
                let root = l.doc.roots[i?];
                Some((root, l.frame_box(root)?))
            };
            let (after, before) = (side(main, ai), side(&self.before, bi));
            if after.is_none() && before.is_none() {
                continue;
            }
            // Document order: the shown version's, with removed frames after the frame that
            // preceded them before.
            let key = match (ai, bi) {
                (Some(ai), _) => (ai as i64, 0, 0),
                (None, Some(bi)) => {
                    let anchor = b.roots[..bi].iter().rev().find_map(|r| index(a, b.node(*r).id()));
                    (anchor.map_or(-1, |i| i as i64), 1, bi)
                }
                (None, None) => continue,
            };
            let kind = match (after.is_some(), before.is_some()) {
                (true, false) => "added",
                (false, true) => "removed",
                _ if d.frames.contains(&id) => "changed",
                _ => "unchanged",
            };
            let change = d.changed.iter().find(|c| c.id == id);
            let name = ai.map(|i| a.roots[i]).map(|r| a.node(r)).or_else(|| bi.map(|i| b.node(b.roots[i]))).and_then(|n| n.name().map(String::from));
            let spec = Spec {
                id,
                name,
                kind,
                props: change.map(|c| c.props.clone()).unwrap_or_default(),
                moved: change.is_some_and(|c| c.moved),
                before,
                after,
            };
            keyed.push((key, spec));
        }
        keyed.sort_by_key(|(k, _)| *k);
        self.specs = keyed.into_iter().map(|(_, s)| s).collect();
        let [added, changed, removed] = self.opts.colors;
        // A row's own frame only when its own properties changed: that it grew or moved, its
        // header says, and tinting all of it would hide what changed inside.
        let row_frame = |id: &str| self.specs.iter().any(|s| s.id == id);
        let changed_ids = d.changed.iter().filter(|c| !c.props.is_empty() || !row_frame(&c.id)).map(|c| c.id.clone()).collect();
        self.marks = vec![
            (Side::After, added, d.added),
            (Side::After, changed, changed_ids),
            (Side::Before, removed, d.removed),
        ];
    }
}

/// A scale and offset: `out = in * k + d`.
#[derive(Clone, Copy)]
pub(super) struct Xf {
    k: f32,
    dx: f32,
    dy: f32,
}

impl Xf {
    pub(super) fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.k + self.dx, y * self.k + self.dy)
    }
    pub(super) fn rect(&self, r: Rect) -> Rect {
        let (x, y) = self.map(r.x, r.y);
        Rect::new(x, y, r.w * self.k, r.h * self.k)
    }
    pub(super) fn scale(&self) -> f32 {
        self.k
    }
    fn invert(&self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.dx) / self.k, (y - self.dy) / self.k)
    }
    /// This, then `o`.
    fn then(&self, o: &Xf) -> Xf {
        Xf { k: self.k * o.k, dx: self.dx * o.k + o.dx, dy: self.dy * o.k + o.dy }
    }
}

fn intersect(a: &Rect, b: &Rect) -> Option<Rect> {
    let (x, y) = (a.x.max(b.x), a.y.max(b.y));
    let (r, btm) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    (r > x && btm > y).then(|| Rect::new(x, y, r - x, btm - y))
}

fn rect_json(r: Rect) -> Value {
    json!({"x": r.x, "y": r.y, "width": r.w, "height": r.h})
}

impl Layer {
    /// A top-level node's box in document space, rotation included.
    pub(super) fn frame_box(&self, root: NodeId) -> Option<Rect> {
        let s = self.scenes.get(&root)?;
        if s.nodes.is_empty() {
            return None;
        }
        let (m, w, h) = s.transform(0);
        let pts = [m.map(0.0, 0.0), m.map(w, 0.0), m.map(w, h), m.map(0.0, h)];
        let (l, t) = pts.iter().fold((f32::MAX, f32::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
        let (r, b) = pts.iter().fold((f32::MIN, f32::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
        Some(Rect::new(l, t, r - l, b - t))
    }
}

impl Viewer {
    pub(super) fn layer(&self, side: Side) -> Option<&Layer> {
        match side {
            Side::After => self.main.as_ref(),
            Side::Before => self.changes.as_ref().map(|c| &c.before),
        }
    }

    pub(super) fn locate_on(&self, side: Side, key: &str) -> Option<(&RootScene, u32)> {
        self.layer(side)?.locate(key)
    }

    /// Canvas space (document, or list space in the changes layout) to CSS px.
    pub(super) fn cam_xf(&self) -> Xf {
        Xf { k: self.cam.zoom, dx: -self.cam.x * self.cam.zoom, dy: -self.cam.y * self.cam.zoom }
    }

    /// Document space of a node's side to CSS px; None if it isn't shown.
    pub(super) fn doc_xf(&self, side: Side, key: &str) -> Option<Xf> {
        let Some(ch) = &self.changes else { return (side == Side::After).then(|| self.cam_xf()) };
        let (s, _) = self.locate_on(side, key)?;
        let root = s.doc_root?;
        let place = ch.rows.iter().find_map(|r| r.place(side).filter(|p| p.root == root))?;
        Some(place.xf().then(&self.cam_xf()))
    }

    /// A node's corners on screen (CSS px), rotation included.
    pub(super) fn screen_corners(&self, side: Side, key: &str) -> Option<[(f32, f32); 4]> {
        let xf = self.doc_xf(side, key)?;
        let (s, i) = self.locate_on(side, key)?;
        let (m, w, h) = s.transform(i);
        Some([m.map(0.0, 0.0), m.map(w, 0.0), m.map(w, h), m.map(0.0, h)].map(|(x, y)| xf.map(x, y)))
    }

    /// A node's bounding box on screen (CSS px).
    pub(super) fn screen_box(&self, side: Side, key: &str) -> Option<Rect> {
        let pts = self.screen_corners(side, key)?;
        let (l, t) = pts.iter().fold((f32::MAX, f32::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
        let (r, b) = pts.iter().fold((f32::MIN, f32::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
        Some(Rect::new(l, t, r - l, b - t))
    }

    /// Whether a node is at least partly on screen.
    pub(super) fn on_screen(&self, side: Side, key: &str) -> bool {
        let screen = Rect::new(0.0, 0.0, self.size.0, self.size.1);
        self.screen_box(side, key).is_some_and(|r| r.intersects(&screen))
    }

    /// The deepest selectable node under a point (CSS px) in the changes layout.
    pub(super) fn hit_rows(&self, sx: f32, sy: f32) -> Option<(String, Side)> {
        let ch = self.changes.as_ref()?;
        let (lx, ly) = self.cam_xf().invert(sx, sy);
        let sides: &[Side] = if ch.onion() { &[Side::After] } else { &[Side::Before, Side::After] };
        for r in &ch.rows {
            for &side in sides {
                let Some(p) = r.place(side) else { continue };
                if !ch.cell(side, r).contains(lx, ly) {
                    continue;
                }
                let s = self.layer(side)?.scenes.get(&p.root)?;
                let (x, y) = p.xf().invert(lx, ly);
                if let Some(i) = s.hit(x, y) {
                    return Some((s.nodes[i as usize].key().to_string(), side));
                }
            }
        }
        None
    }

    /// Enters the changes layout against `before` (`text` at page path `path`).
    pub(super) fn show_changes(&mut self, text: &str, path: &str, opts: &Value) -> Result<(), String> {
        let opts = Options::parse(opts)?;
        let mut before = Layer::open(text, path, &mut self.warnings)?;
        before.build(None, &mut self.renderer, &mut self.scripts, &mut self.warnings);
        let entering = self.changes.is_none();
        let (saved, version) = self.changes.take().map_or(((self.cam, self.fitted), 0), |c| (c.saved, c.version));
        let mut ch = Changes::new(before, opts, saved);
        ch.version = version;
        self.changes = Some(ch);
        if self.sel_side == Side::Before {
            self.selection.clear();
        }
        if self.hover.as_ref().is_some_and(|h| h.1 == Side::Before) {
            self.hover = None;
        }
        if entering {
            self.cam = Camera { x: 0.0, y: 0.0, zoom: 1.0 };
            self.fitted = true;
        }
        self.refresh_changes();
        Ok(())
    }

    /// Back to the whole document, where the camera was.
    pub(super) fn hide_changes(&mut self) {
        let Some(ch) = self.changes.take() else { return };
        (self.cam, self.fitted) = ch.saved;
        if self.sel_side == Side::Before {
            self.selection.clear();
            self.sel_side = Side::After;
        }
        if self.hover.as_ref().is_some_and(|h| h.1 == Side::Before) {
            self.hover = None;
        }
    }

    pub(super) fn set_changes_options(&mut self, opts: &Value) -> Result<(), String> {
        let opts = Options::parse(opts)?;
        let Some(ch) = &mut self.changes else { return Ok(()) };
        ch.opts = opts;
        if ch.onion() && self.sel_side == Side::Before {
            self.selection.clear();
            self.sel_side = Side::After;
        }
        self.refresh_changes();
        Ok(())
    }

    pub(super) fn changes_version(&self) -> u32 {
        self.changes.as_ref().map_or(0, |c| c.version)
    }

    /// Compares again and re-places the rows: after a load, layout changes, or new options.
    pub(super) fn refresh_changes(&mut self) {
        let (Some(main), Some(ch)) = (&self.main, &mut self.changes) else { return };
        ch.diff(main);
        self.place_rows();
    }

    /// Places the rows for the canvas size (after a resize).
    pub(super) fn place_rows(&mut self) {
        let size = self.size;
        let Some(ch) = &mut self.changes else { return };
        ch.place(size.0, size.1);
        self.clamp_cam();
    }

    /// In the changes layout, keeps the rows in view: no scrolling past either end, and centred
    /// when zoomed out narrower than the canvas.
    pub(super) fn clamp_cam(&mut self) {
        let Some(ch) = &self.changes else { return };
        let (vw, vh) = (self.size.0 / self.cam.zoom, self.size.1 / self.cam.zoom);
        let c = &mut self.cam;
        c.x = if vw >= ch.width { (ch.width - vw) / 2.0 } else { c.x.clamp(0.0, ch.width - vw) };
        c.y = if vh >= ch.height { 0.0 } else { c.y.clamp(0.0, ch.height - vh) };
    }

    fn row_json(&self, r: &Row) -> Value {
        let ch = self.changes.as_ref().unwrap();
        let xf = self.cam_xf();
        let band = Rect::new(ch.opts.padding, r.y, ch.width - 2.0 * ch.opts.padding, r.h);
        json!({
            "id": r.id,
            "name": r.name,
            "kind": r.kind,
            "props": r.props,
            "moved": r.moved,
            "before": r.before.map(|p| rect_json(xf.rect(p.at))),
            "after": r.after.map(|p| rect_json(xf.rect(p.at))),
            "bounds": rect_json(xf.rect(band)),
        })
    }

    /// The rows, positioned on screen (CSS px), as JSON; None outside the changes layout.
    pub(super) fn rows_json(&self) -> Option<Value> {
        let ch = self.changes.as_ref()?;
        Some(Value::Array(ch.rows.iter().map(|r| self.row_json(r)).collect()))
    }

    /// The row at a point (CSS px), with the side it's on (`null` in the onion mode).
    pub(super) fn row_at(&self, sx: f32, sy: f32) -> Option<Value> {
        let ch = self.changes.as_ref()?;
        let (lx, ly) = self.cam_xf().invert(sx, sy);
        let gy = ch.opts.gap / 2.0;
        let r = ch.rows.iter().find(|r| ly >= r.y - gy && ly < r.y + r.h + gy)?;
        let mut v = self.row_json(r);
        let side = (!ch.onion()).then(|| if lx < ch.cols[1] - ch.opts.gutter / 2.0 { "before" } else { "after" });
        v["side"] = json!(side);
        Some(v)
    }

    /// Scrolls a row to the top. False if there's no such row.
    pub(super) fn scroll_to_row(&mut self, id: &str) -> bool {
        let Some(ch) = &self.changes else { return false };
        let Some(r) = ch.rows.iter().find(|r| r.id == id) else { return false };
        self.cam.y = r.y - ch.opts.padding / self.cam.zoom;
        self.clamp_cam();
        true
    }

    /// The camera for the zoom control's "fit": the rows at their laid-out size.
    pub(super) fn fit_rows(&mut self, r: Option<Rect>) {
        match r {
            Some(r) => {
                let (w, h) = self.size;
                let margin = 48.0;
                let zoom = ((w - 2.0 * margin) / r.w.max(1.0)).min((h - 2.0 * margin) / r.h.max(1.0)).clamp(0.05, 8.0);
                self.cam = Camera { x: r.x + r.w / 2.0 - w / zoom / 2.0, y: r.y + r.h / 2.0 - h / zoom / 2.0, zoom };
            }
            None => self.cam = Camera { x: 0.0, y: self.cam.y, zoom: 1.0 },
        }
        self.fitted = true;
        self.clamp_cam();
    }

    /// Nodes' box in list space (`zoomTo` in the changes layout), from the shown side.
    pub(super) fn list_box(&self, key: &str) -> Option<Rect> {
        let r = self.screen_box(Side::After, key).or_else(|| self.screen_box(Side::Before, key))?;
        let (x, y) = self.cam_xf().invert(r.x, r.y);
        Some(Rect::new(x, y, r.w / self.cam.zoom, r.h / self.cam.zoom))
    }

    /// Outlines to draw in the changes layout, on screen.
    pub(super) fn change_marks(&self) -> Vec<(Color4f, Vec<[(f32, f32); 4]>)> {
        let Some(ch) = &self.changes else { return Vec::new() };
        ch.marks.iter().map(|(side, color, keys)| (*color, keys.iter().filter_map(|k| self.screen_corners(*side, k)).collect())).collect()
    }
}

/// Draws the rows: each side's frame from its document, scaled into its cell.
pub(super) fn draw_rows(canvas: &Canvas, renderer: &mut Renderer, main: &Layer, ch: &Changes, cam: Camera, dpr: f32, size: (f32, f32)) {
    canvas.save();
    canvas.scale((cam.zoom, cam.zoom));
    canvas.translate((-cam.x, -cam.y));
    let visible = Rect::new(cam.x, cam.y, size.0 / cam.zoom, size.1 / cam.zoom);
    let reveal = std::mem::take(&mut renderer.reveal);
    let side = |renderer: &mut Renderer, r: &Row, s: Side| {
        let (Some(p), layer) = (r.place(s), if s == Side::Before { &ch.before } else { main }) else { return };
        let Some(scene) = layer.scenes.get(&p.root) else { return };
        let cell = ch.cell(s, r);
        let Some(shown) = intersect(&cell, &visible) else { return };
        let xf = p.xf();
        let (x, y) = xf.invert(shown.x, shown.y);
        let viewport = Rect::new(x, y, shown.w / p.scale, shown.h / p.scale);
        canvas.save();
        canvas.clip_rect(skia_safe::Rect::from_xywh(cell.x, cell.y, cell.w, cell.h), None, true);
        canvas.translate((xf.dx, xf.dy));
        canvas.scale((p.scale, p.scale));
        // Thumbnails apart from the shown document's; the shown side alone gets draw-in.
        renderer.layer = if s == Side::Before { 2 } else { 0 };
        renderer.base_dir = layer.base.clone();
        renderer.draw_scene(canvas, scene, &View { scale: cam.zoom * p.scale * dpr, viewport, lod: true });
        canvas.restore();
    };
    for r in &ch.rows {
        if r.y - ch.opts.gap > visible.bottom() || r.y + r.h + ch.opts.gap < visible.y {
            continue;
        }
        if ch.onion() {
            renderer.reveal = reveal.clone();
            side(renderer, r, Side::After);
            renderer.reveal.clear();
            canvas.save_layer_alpha_f(None, ch.opts.opacity);
            side(renderer, r, Side::Before);
            canvas.restore();
        } else {
            side(renderer, r, Side::Before);
            renderer.reveal = reveal.clone();
            side(renderer, r, Side::After);
            renderer.reveal.clear();
        }
    }
    renderer.reveal = reveal;
    renderer.layer = 0;
    renderer.base_dir = main.base.clone();
    canvas.restore();
}
