//! Paints laid-out scenes with Skia, with viewport culling and level-of-detail.

use crate::icons::is_font_library;
use crate::images::Images;
use crate::shaders::{Built, Ctx, Shaders};
use crate::text::TextEngine;
use crate::Icons;
use pen::scene::{Kind, NoteKind, Rect, RootScene, SNode};
use pen::style::*;
use skia_safe::canvas::SaveLayerRec;
#[allow(deprecated)]
use skia_safe::gradient_shader;
use skia_safe::textlayout::TextShadow;
use skia_safe::{
    image_filters, BlurStyle, Canvas, ClipOp, Color, Color4f, MaskFilter, Matrix, Paint as SkPaint, PaintStyle, Path,
    PathBuilder, PathDirection, PathFillType, Point, RRect, Rect as SkRect, SamplingOptions, Shader, TileMode,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Below this on-screen size (px), containers are drawn as a single flat rect.
const LOD_FLATTEN_PX: f32 = 2.0;
/// Below this on-screen font size (px), text is drawn as gray bars.
const LOD_GREEK_PX: f32 = 2.5;
/// Cached thumbnails are built once, so they can afford more detail than live frames.
const THUMB_FLATTEN_PX: f32 = 1.0;
const THUMB_GREEK_PX: f32 = 1.25;
/// Share of a reveal animation spent tracing the outline; the rest fades the fill in.
const TRACE: f32 = 0.72;

pub struct View {
    /// Screen pixels per document unit.
    pub scale: f32,
    /// Visible region in document coordinates.
    pub viewport: Rect,
    /// Use level-of-detail simplifications (off for exports/screenshots).
    pub lod: bool,
}

pub struct Renderer {
    pub text: TextEngine,
    pub icons: Icons,
    pub images: Images,
    pub shaders: Shaders,
    paths: HashMap<(u64, u32, u32), Path>,
    /// Directory relative image/shader URLs resolve against.
    pub base_dir: PathBuf,
    /// Number of nodes painted in the last frame (for diagnostics).
    pub drawn: usize,
    thumbs: HashMap<u32, Thumb>,
    thumb_bytes: usize,
    frame: u64,
    /// Time left this frame for building thumbnails.
    thumb_budget: Option<std::time::Instant>,
    /// Set when some thumbnails were deferred; the caller should redraw soon.
    pub needs_redraw: bool,
    /// True while rendering a thumbnail (finer level-of-detail).
    fine: bool,
    /// Nodes being drawn in, by root id then scene index: progress 0..1.
    pub reveal: HashMap<u32, HashMap<u32, f32>>,
    /// Pen tips of in-progress reveals this frame: device position and direction.
    pub tips: Vec<(Point, Point)>,
    /// Pointer position in device pixels, for `@mouse` shaders.
    pub mouse: Option<Point>,
    sdfs: HashMap<(u64, u32, u32), Option<skia_safe::Image>>,
    /// Roots with time- or pointer-driven shaders: never drawn from cached thumbnails.
    animated: HashSet<u32>,
    current_root: Option<u32>,
}

/// A cached raster of a whole top-level frame, used when it is small on screen.
struct Thumb {
    image: skia_safe::Image,
    scale: f32,
    stamp: u64,
    used: u64,
}

/// Roots smaller than this on screen (px) are drawn from cached thumbnails.
const THUMB_MAX_PX: f32 = 640.0;
const THUMB_CACHE_BYTES: usize = 384 << 20;
const THUMB_BUDGET: std::time::Duration = std::time::Duration::from_millis(14);

enum Shape {
    RRect(RRect),
    Path(Path),
}

impl Shape {
    fn draw(&self, canvas: &Canvas, paint: &SkPaint) {
        match self {
            Shape::RRect(r) => {
                canvas.draw_rrect(r, paint);
            }
            Shape::Path(p) => {
                canvas.draw_path(p, paint);
            }
        }
    }
    fn clip(&self, canvas: &Canvas, op: ClipOp) {
        match self {
            Shape::RRect(r) => {
                canvas.clip_rrect(r, op, true);
            }
            Shape::Path(p) => {
                canvas.clip_path(p, op, true);
            }
        }
    }
    fn path(&self) -> Path {
        match self {
            Shape::RRect(r) => Path::rrect(*r, None),
            Shape::Path(p) => p.clone(),
        }
    }
    fn is_closed_area(&self) -> bool {
        true
    }
}

impl Renderer {
    pub fn new() -> Renderer {
        Renderer {
            text: TextEngine::new(),
            icons: Icons::new(),
            images: Images::new(),
            shaders: Shaders::default(),
            paths: HashMap::new(),
            base_dir: PathBuf::from("."),
            drawn: 0,
            thumbs: HashMap::new(),
            thumb_bytes: 0,
            frame: 0,
            thumb_budget: None,
            needs_redraw: false,
            fine: false,
            reveal: HashMap::new(),
            tips: Vec::new(),
            mouse: None,
            sdfs: HashMap::new(),
            animated: HashSet::new(),
            current_root: None,
        }
    }

    /// Collects finished background loads. Returns (fonts changed → relayout, anything changed → redraw).
    pub fn poll(&mut self) -> (bool, bool) {
        let fonts = self.text.fonts.poll();
        if fonts {
            self.text.invalidate();
        }
        let other = self.icons.poll() | self.images.poll();
        if other || fonts {
            self.thumbs.clear();
            self.thumb_bytes = 0;
        }
        (fonts, fonts || other)
    }

    pub fn has_pending(&self) -> bool {
        self.text.fonts.has_pending() || self.icons.has_pending() || self.images.has_pending()
    }

    /// Blocks until fonts/icons/images in flight have arrived. Returns true if fonts changed.
    pub fn wait_for_assets(&mut self, timeout: std::time::Duration) -> bool {
        let fonts = self.text.fonts.wait(timeout);
        self.icons.wait(timeout);
        self.images.wait(timeout);
        if fonts {
            self.text.invalidate();
        }
        fonts
    }

    /// Call once per window frame, before drawing scenes.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.drawn = 0;
        self.needs_redraw = false;
        self.tips.clear();
        self.shaders.uses_time = false;
        self.shaders.uses_mouse = false;
        self.thumb_budget = Some(std::time::Instant::now() + THUMB_BUDGET);
    }

    pub fn draw_scene(&mut self, canvas: &Canvas, scene: &RootScene, view: &View) {
        if scene.nodes.is_empty() || scene.bounds.is_empty() {
            return;
        }
        let b = scene.bounds[0];
        if !b.intersects(&view.viewport) {
            return;
        }
        let on_screen = b.w.max(b.h) * view.scale;
        self.current_root = scene.doc_root.map(|r| r.0);
        if view.lod && self.thumb_budget.is_some() && on_screen < THUMB_MAX_PX && on_screen > LOD_FLATTEN_PX && scene.nodes.len() > 64 && !self.revealing(scene) && !self.current_root.is_some_and(|r| self.animated.contains(&r)) {
            if self.draw_thumb(canvas, scene, view) {
                return;
            }
        }
        self.node(canvas, scene, 0, view);
    }

    /// Draws a root from its cached raster, building it if time allows. False = draw normally.
    fn draw_thumb(&mut self, canvas: &Canvas, scene: &RootScene, view: &View) -> bool {
        let Some(root) = scene.doc_root else { return false };
        let b = scene.bounds[0];
        // Power-of-two scale buckets so small zoom changes reuse the same raster.
        let want = 2f32.powf(view.scale.log2().ceil());
        let fresh = self.thumbs.get(&root.0).is_some_and(|t| t.stamp == scene.stamp && t.scale >= want * 0.99 && t.scale <= want * 2.01);
        if !fresh {
            let over_budget = self.thumb_budget.is_some_and(|d| std::time::Instant::now() > d);
            if over_budget {
                // Placeholder this frame: the stale thumb if any, else a flat block.
                self.needs_redraw = true;
                if let Some(t) = self.thumbs.get(&root.0) {
                    if t.stamp == scene.stamp {
                        let img = t.image.clone();
                        blit(canvas, &img, b);
                        return true;
                    }
                }
                if let Some(c) = lod_color(scene, 0) {
                    canvas.draw_rect(SkRect::from_xywh(b.x, b.y, b.w, b.h), &SkPaint::new(c, None));
                }
                return true;
            }
            let (w, h) = ((b.w * want).ceil().max(1.0) as i32, (b.h * want).ceil().max(1.0) as i32);
            let info = skia_safe::ImageInfo::new_n32_premul((w, h), None);
            let Some(mut surface) = canvas.new_surface(&info, None).or_else(|| skia_safe::surfaces::raster(&info, None, None)) else { return false };
            {
                let c = surface.canvas();
                c.clear(Color::TRANSPARENT);
                c.scale((want, want));
                c.translate((-b.x, -b.y));
                let v = View { scale: want, viewport: b, lod: true };
                self.fine = true;
                self.node(c, scene, 0, &v);
                self.fine = false;
            }
            let image = surface.image_snapshot();
            if let Some(old) = self.thumbs.insert(root.0, Thumb { image, scale: want, stamp: scene.stamp, used: self.frame }) {
                self.thumb_bytes = self.thumb_bytes.saturating_sub(old.image.width() as usize * old.image.height() as usize * 4);
            }
            self.thumb_bytes += (w * h * 4) as usize;
            self.evict_thumbs();
        }
        let t = self.thumbs.get_mut(&root.0).unwrap();
        t.used = self.frame;
        let img = t.image.clone();
        blit(canvas, &img, b);
        true
    }

    fn evict_thumbs(&mut self) {
        if self.thumb_bytes <= THUMB_CACHE_BYTES {
            return;
        }
        let mut by_age: Vec<(u64, u32)> = self.thumbs.iter().map(|(k, t)| (t.used, *k)).collect();
        by_age.sort();
        for (_, k) in by_age {
            if self.thumb_bytes <= THUMB_CACHE_BYTES * 3 / 4 {
                break;
            }
            if let Some(t) = self.thumbs.remove(&k) {
                self.thumb_bytes = self.thumb_bytes.saturating_sub(t.image.width() as usize * t.image.height() as usize * 4);
            }
        }
    }

    /// Draws one node (and its subtree) by scene index, at its own position.
    pub fn draw_node(&mut self, canvas: &Canvas, scene: &RootScene, i: u32, view: &View) {
        self.node(canvas, scene, i, view);
    }

    fn node(&mut self, canvas: &Canvas, scene: &RootScene, i: u32, view: &View) {
        let n = &scene.nodes[i as usize];
        if !n.enabled {
            return;
        }
        let b = scene.bounds[i as usize];
        if !b.intersects(&view.viewport) {
            return;
        }
        let extent = b.w.max(b.h) * view.scale;
        if view.lod && extent < 0.4 {
            return;
        }
        self.drawn += 1;
        let (w, h) = (n.rect.w, n.rect.h);

        canvas.save();
        canvas.translate((n.rect.x, n.rect.y));
        if n.rotation != 0.0 {
            canvas.rotate(-n.rotation, None);
        }
        if n.flip_x || n.flip_y {
            canvas.translate((if n.flip_x { w } else { 0.0 }, if n.flip_y { h } else { 0.0 }));
            canvas.scale((if n.flip_x { -1.0 } else { 1.0 }, if n.flip_y { -1.0 } else { 1.0 }));
        }

        // Tiny subtrees: one flat rect in the dominant color.
        // Also flatten containers that are slivers on screen (rows a pixel tall, etc.).
        let (flatten_px, sliver_px) = if self.fine { (THUMB_FLATTEN_PX, 0.5) } else { (LOD_FLATTEN_PX, 1.0) };
        let sliver = !n.children.is_empty() && b.w.min(b.h) * view.scale < sliver_px;
        if view.lod && (extent < flatten_px || sliver) && (!n.children.is_empty() || matches!(n.kind, Kind::Text(_))) {
            if let Some(c) = lod_color(scene, i) {
                let mut p = SkPaint::new(c, None);
                p.set_anti_alias(false);
                canvas.draw_rect(SkRect::from_wh(w.max(1.0 / view.scale), h.max(1.0 / view.scale)), &p);
            }
            canvas.restore();
            return;
        }

        let blur = n.effects.iter().find_map(|e| match e {
            Effect::Blur { radius } if *radius > 0.0 => Some(*radius),
            _ => None,
        });
        let layered = n.opacity < 1.0 || blur.is_some();
        if layered {
            let mut lp = SkPaint::default();
            lp.set_alpha_f(n.opacity);
            if let Some(r) = blur {
                lp.set_image_filter(image_filters::blur((r / 2.0, r / 2.0), TileMode::Decal, None, None));
            }
            canvas.save_layer(&SaveLayerRec::default().paint(&lp));
        }

        let shape = self.shape(n);
        let reveal = scene.doc_root.and_then(|r| self.reveal.get(&r.0)).and_then(|m| m.get(&i)).copied();
        if let (Some(t), Some(shape)) = (reveal, &shape) {
            if t > 0.0 {
                self.draw_reveal(canvas, n, shape, t, view);
            }
            if layered {
                canvas.restore();
            }
            canvas.restore();
            return;
        }
        if let Some(shape) = &shape {
            self.outer_shadows(canvas, n, shape);
            self.background_blur(canvas, n, shape);
            for f in &n.fills {
                self.fill(canvas, f, shape, w, h);
            }
            self.inner_shadows(canvas, n, shape);
        }

        match &n.kind {
            Kind::Text(t) => self.text_content(canvas, n, t, view),
            Kind::Icon { library, name, weight } => self.icon(canvas, n, library, name, *weight),
            Kind::Note(kind, t) => self.note(canvas, n, *kind, t),
            Kind::Missing => {
                let mut p = SkPaint::new(Color4f::new(0.9, 0.3, 0.3, 0.6), None);
                p.set_style(PaintStyle::Stroke);
                p.set_stroke_width(1.0 / view.scale);
                canvas.draw_rect(SkRect::from_wh(w, h), &p);
            }
            _ => {}
        }

        if !n.children.is_empty() {
            let clip = n.clips();
            if clip {
                canvas.save();
                match &shape {
                    Some(s) => s.clip(canvas, ClipOp::Intersect),
                    None => {
                        canvas.clip_rect(SkRect::from_wh(w, h), ClipOp::Intersect, true);
                    }
                }
            }
            for &c in &n.children {
                self.node(canvas, scene, c, view);
            }
            if clip {
                canvas.restore();
            }
        }

        if let (Some(stroke), Some(shape)) = (&n.stroke, &shape) {
            if !matches!(n.kind, Kind::Text(_)) {
                self.stroke(canvas, n, stroke, shape);
            }
        }

        if layered {
            canvas.restore();
        }
        canvas.restore();
    }

    fn revealing(&self, scene: &RootScene) -> bool {
        scene.doc_root.is_some_and(|r| self.reveal.contains_key(&r.0))
    }

    /// A shape being drawn in: its outline traced up to `t`, then the fill fading in.
    fn draw_reveal(&mut self, canvas: &Canvas, n: &SNode, shape: &Shape, t: f32, view: &View) {
        let (w, h) = (n.rect.w, n.rect.h);
        let path = match shape {
            Shape::Path(p) => p.clone(),
            Shape::RRect(r) => Path::rrect(*r, None),
        };
        let trace = (t / TRACE).min(1.0);
        let fill = ((t - TRACE) / (1.0 - TRACE)).clamp(0.0, 1.0);
        if fill > 0.0 {
            canvas.save_layer_alpha_f(None, fill);
            for f in &n.fills {
                self.fill(canvas, f, shape, w, h);
            }
            canvas.restore();
        }
        let stroked = n.stroke.as_ref().filter(|s| s.max_width() > 0.0 && !s.paints.is_empty());
        if let (Some(s), true) = (stroked, trace >= 1.0) {
            self.stroke(canvas, n, s, shape);
            return;
        }
        // The ink line: the node's own stroke, or a hairline in its fill color that fades as the fill lands.
        let (mut ink, width) = match stroked {
            Some(s) => (self.sk_paint(&s.paints[0], w, h).unwrap_or_default(), s.max_width()),
            None => {
                let c = n.fills.last().map(|f| f.average().with_alpha(1.0)).unwrap_or(Rgba([0.1, 0.1, 0.1, 1.0]));
                (SkPaint::new(c4(c), None), 1.5 / view.scale)
            }
        };
        if stroked.is_none() {
            ink.set_alpha_f(ink.alpha_f() * (1.0 - fill));
        }
        ink.set_anti_alias(true);
        ink.set_style(PaintStyle::Stroke);
        ink.set_stroke_width(width);
        ink.set_stroke_cap(skia_safe::PaintCap::Round);
        ink.set_stroke_join(skia_safe::PaintJoin::Round);
        let contours: Vec<_> = skia_safe::ContourMeasureIter::new(&path, false, None).collect();
        let total: f32 = contours.iter().map(|c| c.length()).sum();
        let mut left = trace * total;
        let mut partial = PathBuilder::new();
        let mut tip = None;
        for c in &contours {
            if left <= 0.0 {
                break;
            }
            let d = left.min(c.length());
            let _ = c.get_segment(0.0, d, &mut partial, true);
            tip = c.pos_tan(d);
            left -= c.length();
        }
        canvas.draw_path(&partial.detach(), &ink);
        if let (Some((p, dir)), true) = (tip, trace < 1.0) {
            let m = canvas.local_to_device_as_3x3();
            self.tips.push((m.map_point(p), m.map_vector(dir)));
        }
    }

    // ------------------------------------------------------------------ geometry

    fn shape(&mut self, n: &SNode) -> Option<Shape> {
        let (w, h) = (n.rect.w, n.rect.h);
        let r = SkRect::from_wh(w, h);
        Some(match &n.kind {
            Kind::Frame { .. } | Kind::Rect | Kind::Note(..) => Shape::RRect(rrect(r, n.radius)),
            Kind::Ellipse { inner, start, sweep } => Shape::Path(ellipse_path(r, *inner, *start, *sweep)),
            Kind::Polygon { count } => Shape::Path(polygon_path(r, *count, n.radius[0])),
            Kind::Path { geometry, view_box, even_odd } => {
                let key = (hash_str(geometry) ^ view_box.map(|v| hash_str(&format!("{v:?}"))).unwrap_or(0), w.to_bits(), h.to_bits());
                if !self.paths.contains_key(&key) {
                    let p = svg_path(geometry, *view_box, w, h);
                    if self.paths.len() > 20000 {
                        self.paths.clear();
                    }
                    self.paths.insert(key, p);
                }
                let mut p = self.paths[&key].clone();
                p.set_fill_type(if *even_odd { PathFillType::EvenOdd } else { PathFillType::Winding });
                Shape::Path(p)
            }
            Kind::Missing => Shape::RRect(RRect::new_rect(r)),
            Kind::Script { .. } => return None,
            Kind::Text(_) | Kind::Icon { .. } | Kind::Group => return None,
        })
    }

    // ------------------------------------------------------------------ paints

    fn sk_paint(&mut self, p: &Paint, w: f32, h: f32) -> Option<SkPaint> {
        let mut out = SkPaint::default();
        out.set_anti_alias(true);
        match p {
            Paint::Solid { color, blend } => {
                out.set_color4f(c4(*color), None);
                out.set_blend_mode(sk_blend(*blend));
            }
            Paint::Gradient { kind, stops, center, size, rotation, opacity, blend } => {
                out.set_shader(gradient(*kind, stops, *center, *size, *rotation, *opacity, w, h)?);
                out.set_blend_mode(sk_blend(*blend));
            }
            Paint::Image { url, mode, opacity, blend } => {
                let key = self.resolve_url(url);
                let img = self.images.get(&key)?;
                let (iw, ih) = (img.width() as f32, img.height() as f32);
                let m = match mode {
                    ImageMode::Stretch => Matrix::scale((w / iw, h / ih)),
                    ImageMode::Fill | ImageMode::Fit => {
                        let s = if *mode == ImageMode::Fill { (w / iw).max(h / ih) } else { (w / iw).min(h / ih) };
                        let mut m = Matrix::translate(((w - iw * s) / 2.0, (h - ih * s) / 2.0));
                        m.pre_scale((s, s), None);
                        m
                    }
                };
                let tile = if *mode == ImageMode::Fit { TileMode::Decal } else { TileMode::Clamp };
                out.set_shader(img.to_shader((tile, tile), SamplingOptions::new(skia_safe::FilterMode::Linear, skia_safe::MipmapMode::Linear), &m)?);
                out.set_alpha_f(*opacity);
                out.set_blend_mode(sk_blend(*blend));
            }
            Paint::Shader { url, uniforms, opacity, blend } => {
                // Strokes and other non-fill uses: no SDF, no backdrop.
                let path = PathBuf::from(self.resolve_url(url));
                let (base, images) = (self.base_dir.clone(), &mut self.images);
                let mut image = |u: &str| images.get(&resolve(&base, u));
                let ctx = Ctx { w, h, mouse: None, image: &mut image, sdf: &mut || None };
                let Built::Shader(s) = self.shaders.shader(path, uniforms, ctx)? else { return None };
                out.set_shader(s);
                out.set_alpha_f(*opacity);
                out.set_blend_mode(sk_blend(*blend));
            }
            Paint::Mesh { columns, colors, opacity, blend, .. } => {
                // Only for strokes; fills draw real patches (`mesh_fill`).
                let first = *colors.first()?;
                let last = *colors.last()?;
                let cols = (*columns).max(1) as usize;
                let tr = colors.get(cols - 1).copied().unwrap_or(first);
                let stops = vec![(0.0, first), (0.5, tr), (1.0, last)];
                out.set_shader(gradient(GradientKind::Linear, &stops, [0.5, 0.5], [1.0, 1.4], 135.0, *opacity, w, h)?);
                out.set_blend_mode(sk_blend(*blend));
            }
        }
        Some(out)
    }

    fn resolve_url(&self, url: &str) -> String {
        resolve(&self.base_dir, url)
    }

    /// One fill of a shape. Shaders and mesh gradients need more than a paint.
    fn fill(&mut self, canvas: &Canvas, f: &Paint, shape: &Shape, w: f32, h: f32) {
        match f {
            Paint::Shader { url, uniforms, opacity, blend } => self.shader_fill(canvas, url, uniforms, *opacity, *blend, shape, w, h),
            Paint::Mesh { columns, rows, colors, points, opacity, blend } => {
                canvas.save();
                shape.clip(canvas, ClipOp::Intersect);
                mesh(canvas, *columns, *rows, colors, points, *opacity, *blend, w, h);
                canvas.restore();
            }
            _ => {
                if let Some(p) = self.sk_paint(f, w, h) {
                    shape.draw(canvas, &p);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn shader_fill(&mut self, canvas: &Canvas, url: &str, uniforms: &[(String, Uniform)], opacity: f32, blend: BlendMode, shape: &Shape, w: f32, h: f32) {
        let path = PathBuf::from(self.resolve_url(url));
        let to_device = canvas.local_to_device_as_3x3();
        let mouse = self.mouse.and_then(|d| to_device.invert().map(|m| m.map_point(d))).map(|p| (p.x, p.y));
        let outline = shape.path();
        let sdf_key = (hash_str(&outline.to_svg()), w.to_bits(), h.to_bits());
        let (base, images, sdfs) = (self.base_dir.clone(), &mut self.images, &mut self.sdfs);
        let mut image = |u: &str| images.get(&resolve(&base, u));
        let mut sdf = || sdfs.entry(sdf_key).or_insert_with(|| crate::shaders::sdf_image(&outline, w, h)).clone();
        let (time, mouse_used) = (std::mem::take(&mut self.shaders.uses_time), std::mem::take(&mut self.shaders.uses_mouse));
        let built = self.shaders.shader(path, uniforms, Ctx { w, h, mouse, image: &mut image, sdf: &mut sdf });
        if self.shaders.uses_time || self.shaders.uses_mouse {
            if let Some(r) = self.current_root {
                self.animated.insert(r);
            }
        }
        self.shaders.uses_time |= time;
        self.shaders.uses_mouse |= mouse_used;
        match built {
            Some(Built::Shader(s)) => {
                let mut p = SkPaint::default();
                p.set_anti_alias(true);
                p.set_shader(s);
                p.set_alpha_f(opacity);
                p.set_blend_mode(sk_blend(blend));
                shape.draw(canvas, &p);
            }
            Some(Built::Backdrop { mut builder, child }) => {
                // The filter runs in device space; the shader works in the node's local space.
                let Some(to_local) = to_device.invert() else { return };
                let _ = builder.set_uniform_float("study_to_local", &mat3(&to_local));
                let _ = builder.set_uniform_float("study_from_local", &mat3(&to_device));
                let Some(filter) = image_filters::runtime_shader(&builder, &child, None) else { return };
                canvas.save();
                shape.clip(canvas, ClipOp::Intersect);
                let mut lp = SkPaint::default();
                lp.set_alpha_f(opacity);
                lp.set_blend_mode(sk_blend(blend));
                let bounds = SkRect::from_wh(w, h);
                canvas.save_layer(&SaveLayerRec::default().bounds(&bounds).backdrop(&filter).paint(&lp));
                canvas.restore();
                canvas.restore();
            }
            None => {}
        }
    }

    // ------------------------------------------------------------------ strokes

    fn stroke(&mut self, canvas: &Canvas, n: &SNode, s: &Stroke, shape: &Shape) {
        let (w, h) = (n.rect.w, n.rect.h);
        let paints: Vec<SkPaint> = s.paints.iter().filter_map(|p| self.sk_paint(p, w, h)).collect();
        if paints.is_empty() {
            return;
        }
        let open_path = matches!(n.kind, Kind::Path { .. });
        match shape {
            Shape::RRect(rr) if !s.is_uniform() => {
                // Per-side widths: fill the ring between two rounded rects.
                let [t, r, b, l] = s.width;
                let (outer, inner) = match s.align {
                    StrokeAlign::Inner => (*rr, inset_rrect(rr, [t, r, b, l])),
                    StrokeAlign::Center => {
                        (inset_rrect(rr, [-t / 2.0, -r / 2.0, -b / 2.0, -l / 2.0]), inset_rrect(rr, [t / 2.0, r / 2.0, b / 2.0, l / 2.0]))
                    }
                    StrokeAlign::Outer => (inset_rrect(rr, [-t, -r, -b, -l]), *rr),
                };
                for mut p in paints {
                    p.set_style(PaintStyle::Fill);
                    canvas.draw_drrect(outer, inner, &p);
                }
            }
            Shape::RRect(rr) => {
                let sw = s.width[0];
                let rect = match s.align {
                    StrokeAlign::Inner => inset_rrect(rr, [sw / 2.0; 4]),
                    StrokeAlign::Center => *rr,
                    StrokeAlign::Outer => inset_rrect(rr, [-sw / 2.0; 4]),
                };
                for mut p in paints {
                    p.set_style(PaintStyle::Stroke);
                    p.set_stroke_width(sw);
                    canvas.draw_rrect(rect, &p);
                }
            }
            Shape::Path(path) => {
                let sw = s.max_width();
                let align = if open_path { StrokeAlign::Center } else { s.align };
                canvas.save();
                let width = match align {
                    StrokeAlign::Center => sw,
                    StrokeAlign::Inner => {
                        canvas.clip_path(path, ClipOp::Intersect, true);
                        sw * 2.0
                    }
                    StrokeAlign::Outer => {
                        canvas.clip_path(path, ClipOp::Difference, true);
                        sw * 2.0
                    }
                };
                for mut p in paints {
                    p.set_style(PaintStyle::Stroke);
                    p.set_stroke_width(width);
                    p.set_stroke_cap(match s.cap {
                        Cap::Butt => skia_safe::PaintCap::Butt,
                        Cap::Round => skia_safe::PaintCap::Round,
                        Cap::Square => skia_safe::PaintCap::Square,
                    });
                    p.set_stroke_join(match s.join {
                        Join::Miter => skia_safe::PaintJoin::Miter,
                        Join::Round => skia_safe::PaintJoin::Round,
                        Join::Bevel => skia_safe::PaintJoin::Bevel,
                    });
                    canvas.draw_path(path, &p);
                }
                canvas.restore();
            }
        }
    }

    // ------------------------------------------------------------------ effects

    fn outer_shadows(&mut self, canvas: &Canvas, n: &SNode, shape: &Shape) {
        for e in &n.effects {
            let Effect::Shadow { inner: false, offset, spread, blur, color, blend } = e else { continue };
            let mut p = SkPaint::new(c4(*color), None);
            p.set_anti_alias(true);
            p.set_blend_mode(sk_blend(*blend));
            if *blur > 0.0 {
                p.set_mask_filter(MaskFilter::blur(BlurStyle::Normal, blur / 2.0, false));
            }
            canvas.save();
            // Like CSS box-shadow, the shadow doesn't show through the node itself.
            if shape.is_closed_area() {
                shape.clip(canvas, ClipOp::Difference);
            }
            canvas.translate((offset[0], offset[1]));
            match shape {
                Shape::RRect(rr) => {
                    canvas.draw_rrect(inset_rrect(rr, [-spread; 4]), &p);
                }
                Shape::Path(path) => {
                    if *spread != 0.0 {
                        let mut sp = p.clone();
                        sp.set_style(PaintStyle::StrokeAndFill);
                        sp.set_stroke_width(spread * 2.0);
                        canvas.draw_path(path, &sp);
                    } else {
                        canvas.draw_path(path, &p);
                    }
                }
            }
            canvas.restore();
        }
    }

    fn inner_shadows(&mut self, canvas: &Canvas, n: &SNode, shape: &Shape) {
        for e in &n.effects {
            let Effect::Shadow { inner: true, offset, spread, blur, color, blend } = e else { continue };
            let mut p = SkPaint::new(c4(*color), None);
            p.set_anti_alias(true);
            p.set_blend_mode(sk_blend(*blend));
            if *blur > 0.0 {
                p.set_mask_filter(MaskFilter::blur(BlurStyle::Normal, blur / 2.0, false));
            }
            canvas.save();
            shape.clip(canvas, ClipOp::Intersect);
            // Everything outside the (offset, shrunk) shape casts the shadow inward.
            let hole = match shape {
                Shape::RRect(rr) => Path::rrect(inset_rrect(rr, [*spread; 4]), None),
                Shape::Path(path) => path.clone(),
            };
            let big = SkRect::from_xywh(-1e4, -1e4, 2e4 + n.rect.w, 2e4 + n.rect.h);
            let mut b = PathBuilder::new();
            b.add_rect(big, PathDirection::CW, None);
            b.add_path(&hole.make_transform(&Matrix::translate((offset[0], offset[1]))), None);
            b.set_fill_type(PathFillType::EvenOdd);
            canvas.draw_path(&b.detach(), &p);
            canvas.restore();
        }
    }

    fn background_blur(&mut self, canvas: &Canvas, n: &SNode, shape: &Shape) {
        for e in &n.effects {
            let Effect::BackgroundBlur { radius } = e else { continue };
            let Some(filter) = image_filters::blur((radius / 2.0, radius / 2.0), TileMode::Clamp, None, None) else { continue };
            canvas.save();
            shape.clip(canvas, ClipOp::Intersect);
            canvas.save_layer(&SaveLayerRec::default().backdrop(&filter));
            canvas.restore();
            canvas.restore();
        }
    }

    // ------------------------------------------------------------------ content

    fn text_content(&mut self, canvas: &Canvas, n: &SNode, t: &TextData, view: &View) {
        let Some(fill) = n.fills.first() else { return };
        let (w, h) = (n.rect.w, n.rect.h);
        let greek_px = if self.fine { THUMB_GREEK_PX } else { LOD_GREEK_PX };
        if view.lod && t.style.size * view.scale < greek_px {
            let mut p = SkPaint::new(c4(fill.average().with_alpha(0.35)), None);
            p.set_anti_alias(false);
            let lh = t.style.size * t.style.line_height.unwrap_or(1.2);
            let lines = (h / lh).round().max(1.0) as usize;
            for k in 0..lines {
                let y = k as f32 * lh + lh * 0.25;
                canvas.draw_rect(SkRect::from_xywh(0.0, y, w, lh * 0.5), &p);
            }
            return;
        }
        let Some(paint) = self.sk_paint(fill, w, h) else { return };
        let cacheable = matches!(fill, Paint::Solid { .. });
        let shadows: Vec<TextShadow> = n
            .effects
            .iter()
            .filter_map(|e| match e {
                Effect::Shadow { inner: false, offset, blur, color, .. } => {
                    Some(TextShadow::new(Color::from(color.to_u32()), (offset[0], offset[1]), (*blur / 2.0) as f64))
                }
                _ => None,
            })
            .collect();
        let width = if t.growth == TextGrowth::Auto { None } else { Some(w) };
        let para = self.text.paragraph(t, width, &paint, cacheable, &shadows);
        let ph = para.height();
        let dy = if t.growth == TextGrowth::FixedWidthHeight {
            match t.style.valign {
                VAlign::Top => 0.0,
                VAlign::Middle => (h - ph) / 2.0,
                VAlign::Bottom => h - ph,
            }
        } else {
            0.0
        };
        para.paint(canvas, (0.0, dy));
    }

    fn icon(&mut self, canvas: &Canvas, n: &SNode, library: &str, name: &str, weight: Option<f32>) {
        let (w, h) = (n.rect.w, n.rect.h);
        let color = n.fills.first().map(|f| f.average()).unwrap_or(Rgba::BLACK);
        if is_font_library(library) {
            let size = w.min(h);
            let t = TextData {
                content: name.to_string(),
                style: TextStyle { family: library.to_string(), size, weight: weight.unwrap_or(400.0) as u16, line_height: Some(1.0), ..Default::default() },
                growth: TextGrowth::Auto,
            };
            let paint = SkPaint::new(c4(color), None);
            let para = self.text.paragraph(&t, None, &paint, true, &[]);
            let (pw, ph) = (para.max_intrinsic_width(), para.height());
            para.paint(canvas, ((w - pw) / 2.0, (h - ph) / 2.0));
            return;
        }
        let mgr = self.text.fonts.font_mgr().clone();
        if !self.icons.draw(canvas, library, name, color.to_u32(), w, h, &mgr) {
            // Unknown icon (or still loading): a faint placeholder.
            let mut p = SkPaint::new(c4(color.with_alpha(0.15)), None);
            p.set_anti_alias(true);
            canvas.draw_round_rect(SkRect::from_wh(w, h), w * 0.2, h * 0.2, &p);
        }
    }

    fn note(&mut self, canvas: &Canvas, n: &SNode, kind: NoteKind, t: &TextData) {
        let (w, h) = (n.rect.w, n.rect.h);
        let bg = match kind {
            NoteKind::Note => Color4f::new(1.0, 0.95, 0.70, 1.0),
            NoteKind::Prompt => Color4f::new(0.86, 0.92, 1.0, 1.0),
            NoteKind::Context => Color4f::new(0.90, 0.97, 0.90, 1.0),
        };
        if n.fills.is_empty() {
            let mut p = SkPaint::new(bg, None);
            p.set_anti_alias(true);
            canvas.draw_round_rect(SkRect::from_wh(w, h), 6.0, 6.0, &p);
        }
        let paint = SkPaint::new(Color4f::new(0.15, 0.15, 0.15, 1.0), None);
        let mut td = t.clone();
        td.growth = TextGrowth::FixedWidth;
        let para = self.text.paragraph(&td, Some((w - 32.0).max(1.0)), &paint, true, &[]);
        canvas.save();
        canvas.clip_rect(SkRect::from_wh(w, h), ClipOp::Intersect, true);
        para.paint(canvas, (16.0, 16.0));
        canvas.restore();
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Representative color for a flattened subtree: its own fill, else its first child's.
fn lod_color(scene: &RootScene, i: u32) -> Option<Color4f> {
    let mut cur = i;
    for _ in 0..4 {
        let n = &scene.nodes[cur as usize];
        let fill = match &n.kind {
            Kind::Text(_) => n.fills.first().map(|f| f.average().with_alpha(0.35)),
            _ => n.fills.last().map(|f| f.average()),
        };
        if let Some(c) = fill.filter(|c| c.0[3] > 0.01) {
            return Some(c4(c.with_alpha(n.opacity)));
        }
        cur = *n.children.iter().find(|&&c| scene.nodes[c as usize].enabled)?;
    }
    None
}

fn c4(c: Rgba) -> Color4f {
    Color4f::new(c.0[0], c.0[1], c.0[2], c.0[3])
}

fn rrect(r: SkRect, radius: [f32; 4]) -> RRect {
    if radius.iter().all(|&v| v <= 0.0) {
        return RRect::new_rect(r);
    }
    let v = |x: f32| Point::new(x.max(0.0), x.max(0.0));
    RRect::new_rect_radii(r, &[v(radius[0]), v(radius[1]), v(radius[2]), v(radius[3])])
}

/// Insets a rounded rect per side (negative = outset), adjusting corner radii to stay concentric.
fn inset_rrect(rr: &RRect, [t, r, b, l]: [f32; 4]) -> RRect {
    let rect = rr.rect();
    let new = SkRect::new(rect.left + l, rect.top + t, rect.right - r, rect.bottom - b);
    let adj = |p: Point, dx: f32, dy: f32| {
        if p.x <= 0.0 && p.y <= 0.0 {
            Point::new(0.0, 0.0)
        } else {
            Point::new((p.x - dx).max(0.0), (p.y - dy).max(0.0))
        }
    };
    use skia_safe::rrect::Corner;
    let radii = [
        adj(rr.radii(Corner::UpperLeft), l, t),
        adj(rr.radii(Corner::UpperRight), r, t),
        adj(rr.radii(Corner::LowerRight), r, b),
        adj(rr.radii(Corner::LowerLeft), l, b),
    ];
    if new.width() <= 0.0 || new.height() <= 0.0 {
        return RRect::new_rect(SkRect::new(new.left, new.top, new.left.max(new.right), new.top.max(new.bottom)));
    }
    RRect::new_rect_radii(new, &radii)
}

fn ellipse_path(r: SkRect, inner: f32, start: f32, sweep: f32) -> Path {
    let mut b = PathBuilder::new();
    let inner = inner.clamp(0.0, 1.0);
    let full = sweep.abs() >= 360.0;
    let (cx, cy) = (r.center_x(), r.center_y());
    let inner_rect = SkRect::from_xywh(cx - r.width() * inner / 2.0, cy - r.height() * inner / 2.0, r.width() * inner, r.height() * inner);
    // Pen angles are CCW from the +x axis; Skia's are clockwise (y down).
    let (s, sw) = (-start, -sweep.clamp(-360.0, 360.0));
    if full {
        b.add_oval(r, PathDirection::CW, None);
        if inner > 0.0 {
            b.add_oval(inner_rect, PathDirection::CCW, None);
            b.set_fill_type(PathFillType::EvenOdd);
        }
    } else if inner > 0.0 {
        b.arc_to(r, s, sw, true);
        b.arc_to(inner_rect, s + sw, -sw, false);
        b.close();
    } else {
        b.move_to((cx, cy));
        b.arc_to(r, s, sw, false);
        b.close();
    }
    b.detach()
}

fn polygon_path(r: SkRect, count: u32, corner: f32) -> Path {
    let mut b = PathBuilder::new();
    let (cx, cy, rx, ry) = (r.center_x(), r.center_y(), r.width() / 2.0, r.height() / 2.0);
    for k in 0..count {
        let a = (-90.0 + k as f32 * 360.0 / count as f32).to_radians();
        let p = (cx + rx * a.cos(), cy + ry * a.sin());
        if k == 0 {
            b.move_to(p);
        } else {
            b.line_to(p);
        }
    }
    b.close();
    let path = b.detach();
    if corner > 0.0 {
        if let Some(effect) = skia_safe::PathEffect::corner_path(corner) {
            let mut paint = SkPaint::default();
            paint.set_path_effect(effect);
            let mut out = PathBuilder::new();
            if skia_safe::path_utils::fill_path_with_paint(&path, &paint, &mut out, None, None) {
                return out.detach();
            }
        }
    }
    path
}

fn svg_path(geometry: &str, view_box: Option<[f32; 4]>, w: f32, h: f32) -> Path {
    let Some(path) = Path::from_svg(geometry) else { return Path::new() };
    let vb = view_box.unwrap_or_else(|| {
        let b = path.compute_tight_bounds();
        [b.left, b.top, b.width(), b.height()]
    });
    let sx = if vb[2] > 0.0 { w / vb[2] } else { 1.0 };
    let sy = if vb[3] > 0.0 { h / vb[3] } else { 1.0 };
    let mut m = Matrix::scale((sx, sy));
    m.pre_translate((-vb[0], -vb[1]));
    path.make_transform(&m)
}

#[allow(clippy::too_many_arguments, deprecated)]
fn gradient(kind: GradientKind, stops: &[(f32, Rgba)], center: [f32; 2], size: [f32; 2], rotation: f32, opacity: f32, w: f32, h: f32) -> Option<Shader> {
    if stops.is_empty() {
        return None;
    }
    let colors: Vec<Color> = stops.iter().map(|s| Color::from(s.1.with_alpha(opacity).to_u32())).collect();
    let pos: Vec<f32> = stops.iter().map(|s| s.0.clamp(0.0, 1.0)).collect();
    let mut m = Matrix::scale((w, h));
    m.pre_translate((center[0], center[1]));
    m.pre_rotate(-rotation, None);
    match kind {
        GradientKind::Linear => {
            // At 0° position 0 sits at the top, 1 at the bottom; rotation turns that axis CCW.
            let half = size[1] / 2.0;
            gradient_shader::linear(((0.0, -half), (0.0, half)), colors.as_slice(), pos.as_slice(), TileMode::Clamp, None, &m)
        }
        GradientKind::Radial => {
            m.pre_scale((size[0] / 2.0, size[1] / 2.0), None);
            gradient_shader::radial((0.0, 0.0), 1.0, colors.as_slice(), pos.as_slice(), TileMode::Clamp, None, &m)
        }
        GradientKind::Angular => {
            m.pre_rotate(-90.0, None);
            gradient_shader::sweep((0.0, 0.0), colors.as_slice(), pos.as_slice(), TileMode::Clamp, None, None, &m)
        }
    }
}

fn sk_blend(b: BlendMode) -> skia_safe::BlendMode {
    use skia_safe::BlendMode as B;
    match b {
        BlendMode::Normal => B::SrcOver,
        BlendMode::Darken => B::Darken,
        BlendMode::Multiply => B::Multiply,
        BlendMode::LinearBurn => B::Multiply,
        BlendMode::ColorBurn => B::ColorBurn,
        BlendMode::Light => B::Lighten,
        BlendMode::Screen => B::Screen,
        BlendMode::LinearDodge => B::Plus,
        BlendMode::ColorDodge => B::ColorDodge,
        BlendMode::Overlay => B::Overlay,
        BlendMode::SoftLight => B::SoftLight,
        BlendMode::HardLight => B::HardLight,
        BlendMode::Difference => B::Difference,
        BlendMode::Exclusion => B::Exclusion,
        BlendMode::Hue => B::Hue,
        BlendMode::Saturation => B::Saturation,
        BlendMode::Color => B::Color,
        BlendMode::Luminosity => B::Luminosity,
    }
}

fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn blit(canvas: &Canvas, img: &skia_safe::Image, b: Rect) {
    let mut p = SkPaint::default();
    p.set_anti_alias(true);
    canvas.draw_image_rect_with_sampling_options(
        img,
        None,
        SkRect::from_xywh(b.x, b.y, b.w, b.h),
        SamplingOptions::new(skia_safe::FilterMode::Linear, skia_safe::MipmapMode::None),
        &p,
    );
}

fn resolve(base: &std::path::Path, url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") || std::path::Path::new(url).is_absolute() {
        return url.to_string();
    }
    base.join(url).to_string_lossy().to_string()
}

/// A Skia matrix as an SkSL `float3x3` uniform (column-major).
fn mat3(m: &Matrix) -> [f32; 9] {
    [m[0], m[3], m[6], m[1], m[4], m[7], m[2], m[5], m[8]]
}

/// A mesh gradient: a grid of Coons patches through the vertices, with explicit Bezier handles
/// where given and smooth (Catmull-Rom) ones elsewhere. Points and handles are normalized.
#[allow(clippy::too_many_arguments)]
fn mesh(canvas: &Canvas, columns: u32, rows: u32, colors: &[Rgba], points: &[MeshPoint], opacity: f32, blend: BlendMode, w: f32, h: f32) {
    let (cols, rows) = (columns.max(2) as usize, rows.max(2) as usize);
    let grid: Vec<MeshPoint> = if points.len() == cols * rows {
        points.to_vec()
    } else {
        (0..rows * cols)
            .map(|i| MeshPoint { pos: [(i % cols) as f32 / (cols - 1) as f32, (i / cols) as f32 / (rows - 1) as f32], ..Default::default() })
            .collect()
    };
    let at = |c: usize, r: usize| grid[r * cols + c];
    let px = |p: [f32; 2]| Point::new(p[0] * w, p[1] * h);
    let pos = |c: usize, r: usize| px(at(c, r).pos);
    // Tangent along a row (dx) or column (dy) for auto handles.
    let tangent = |c: usize, r: usize, horizontal: bool| -> Point {
        let (prev, next) = if horizontal {
            (pos(c.saturating_sub(1), r), pos((c + 1).min(cols - 1), r))
        } else {
            (pos(c, r.saturating_sub(1)), pos(c, (r + 1).min(rows - 1)))
        };
        let span = if horizontal { (c + 1).min(cols - 1) - c.saturating_sub(1) } else { (r + 1).min(rows - 1) - r.saturating_sub(1) };
        (next - prev) * (1.0 / (3.0 * span.max(1) as f32))
    };
    let handle = |c: usize, r: usize, explicit: Option<[f32; 2]>, horizontal: bool, sign: f32| -> Point {
        match explicit {
            Some(o) => pos(c, r) + Point::new(o[0] * w, o[1] * h),
            None => pos(c, r) + tangent(c, r, horizontal) * sign,
        }
    };
    let right = |c, r| handle(c, r, at(c, r).right, true, 1.0);
    let left = |c, r| handle(c, r, at(c, r).left, true, -1.0);
    let bottom = |c, r| handle(c, r, at(c, r).bottom, false, 1.0);
    let top = |c, r| handle(c, r, at(c, r).top, false, -1.0);
    let color = |c: usize, r: usize| c4(colors.get(r * cols + c).copied().unwrap_or(Rgba([0.5, 0.5, 0.5, 1.0]))).to_color();
    let mut paint = SkPaint::new(Color4f::new(1.0, 1.0, 1.0, opacity), None);
    paint.set_anti_alias(true);
    paint.set_blend_mode(sk_blend(blend));
    for r in 0..rows - 1 {
        for c in 0..cols - 1 {
            let cubics = [
                pos(c, r), right(c, r), left(c + 1, r), pos(c + 1, r),
                bottom(c + 1, r), top(c + 1, r + 1), pos(c + 1, r + 1),
                left(c + 1, r + 1), right(c, r + 1), pos(c, r + 1),
                top(c, r + 1), bottom(c, r),
            ];
            let cs = [color(c, r), color(c + 1, r), color(c + 1, r + 1), color(c, r + 1)];
            canvas.draw_patch(&cubics, &cs, None, skia_safe::BlendMode::Modulate, &paint);
        }
    }
}
