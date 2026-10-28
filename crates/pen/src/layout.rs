//! Pen's flex layout: single-axis, no wrapping, `fit_content` / `fill_container`.
//!
//! Widths are resolved first (bottom-up intrinsic sizes, then top-down fills),
//! then heights (text height depends on the final width), then positions.
//! A `fill_container` child contributes nothing to a `fit_content` parent on
//! that axis; if every flow child fills, the parent uses its fallback size.

use crate::scene::{Kind, Rect, RootScene, SNode, NONE};
use crate::style::*;

pub trait TextMeasure {
    /// Measures text laid out at `width` (`None` = no wrapping). Returns `[width, height]`.
    fn measure(&mut self, text: &TextData, width: Option<f32>) -> [f32; 2];
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    W,
    H,
}

struct Ctx<'a> {
    nodes: &'a mut Vec<SNode>,
    m: &'a mut dyn TextMeasure,
    memo: Vec<f32>,
}

pub fn layout(scene: &mut RootScene, m: &mut dyn TextMeasure) {
    if scene.nodes.is_empty() {
        return;
    }
    let n = scene.nodes.len();
    let mut cx = Ctx { nodes: &mut scene.nodes, m, memo: vec![f32::NAN; n] };
    for axis in [Axis::W, Axis::H] {
        cx.memo.iter_mut().for_each(|v| *v = f32::NAN);
        let size = cx.root_size(0, axis);
        cx.assign(0, axis, size);
    }
    let root = &mut cx.nodes[0];
    root.rect.x = root.x;
    root.rect.y = root.y;
    cx.place(0);
    scene.laid_out = true;
    static STAMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    scene.stamp = STAMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

impl Ctx<'_> {
    fn sizing(&self, i: u32, axis: Axis) -> Sizing {
        let n = &self.nodes[i as usize];
        if axis == Axis::W {
            n.width
        } else {
            n.height
        }
    }

    fn in_flow(&self, c: u32) -> bool {
        let n = &self.nodes[c as usize];
        n.enabled && !n.absolute
    }

    fn root_size(&mut self, i: u32, axis: Axis) -> f32 {
        match self.sizing(i, axis) {
            Sizing::Fill(Some(f)) => f,
            _ => self.intrinsic(i, axis),
        }
    }

    /// Size a node wants on `axis` when its parent doesn't dictate it.
    fn intrinsic(&mut self, i: u32, axis: Axis) -> f32 {
        let memo = self.memo[i as usize];
        if !memo.is_nan() {
            return memo;
        }
        let v = match self.sizing(i, axis) {
            Sizing::Fixed(v) => v.max(0.0),
            Sizing::Fit(fallback) => self.content(i, axis, fallback),
            Sizing::Fill(fallback) => match fallback {
                Some(f) => f,
                None => self.content(i, axis, None),
            },
        };
        self.memo[i as usize] = v;
        v
    }

    /// Size derived from content: children for containers, glyphs for text.
    fn content(&mut self, i: u32, axis: Axis, fallback: Option<f32>) -> f32 {
        let n = &self.nodes[i as usize];
        match &n.kind {
            Kind::Text(t) => {
                let t = t.clone();
                let width = if axis == Axis::H && t.growth != TextGrowth::Auto { Some(n.rect.w) } else { None };
                let [w, h] = self.m.measure(&t, width);
                if axis == Axis::W {
                    w
                } else {
                    h
                }
            }
            Kind::Note(_, t) => {
                let t = t.clone();
                let [w, h] = self.m.measure(&t, None);
                if axis == Axis::W {
                    w + 32.0
                } else {
                    h + 32.0
                }
            }
            Kind::Frame { .. } | Kind::Group | Kind::Script { .. } => {
                let dir = n.layout;
                let pad = n.padding;
                let gap = n.gap;
                let children = n.children.clone();
                let (pad_a, pad_b) = if axis == Axis::W { (pad[3], pad[1]) } else { (pad[0], pad[2]) };
                let main = matches!((dir, axis), (Dir::Horizontal, Axis::W) | (Dir::Vertical, Axis::H));
                if dir == Dir::None {
                    // Absolutely positioned children: extent from the origin.
                    let mut extent: f32 = 0.0;
                    let mut any = false;
                    for c in children {
                        if !self.nodes[c as usize].enabled {
                            continue;
                        }
                        any = true;
                        let pos = if axis == Axis::W { self.nodes[c as usize].x } else { self.nodes[c as usize].y };
                        let s = self.intrinsic(c, axis);
                        extent = extent.max(pos + s);
                    }
                    return if any { extent } else { fallback.unwrap_or(0.0) };
                }
                let flow: Vec<u32> = children.into_iter().filter(|&c| self.in_flow(c)).collect();
                let sized: Vec<u32> = flow.iter().copied().filter(|&c| !self.sizing(c, axis).is_fill()).collect();
                if sized.is_empty() {
                    if let Some(f) = fallback {
                        return f;
                    }
                }
                let mut total: f32 = 0.0;
                for &c in &sized {
                    let s = self.intrinsic(c, axis);
                    total = if main { total + s } else { total.max(s) };
                }
                if main && flow.len() > 1 {
                    total += gap * (flow.len() - 1) as f32;
                }
                total + pad_a + pad_b
            }
            _ => fallback.unwrap_or(0.0),
        }
    }

    /// Fixes node `i`'s size on `axis` and distributes space to its children.
    fn assign(&mut self, i: u32, axis: Axis, size: f32) {
        let size = size.max(0.0);
        {
            let n = &mut self.nodes[i as usize];
            if axis == Axis::W {
                n.rect.w = size;
            } else {
                n.rect.h = size;
            }
        }
        let n = &self.nodes[i as usize];
        if !n.is_container() || n.children.is_empty() {
            return;
        }
        let dir = n.layout;
        let pad = n.padding;
        let gap = n.gap;
        let children = n.children.clone();
        let inner = if axis == Axis::W { size - pad[1] - pad[3] } else { size - pad[0] - pad[2] };
        let main = matches!((dir, axis), (Dir::Horizontal, Axis::W) | (Dir::Vertical, Axis::H));

        // Space left for fill children on the main axis.
        let mut fill_share = 0.0;
        if dir != Dir::None && main {
            let flow: Vec<u32> = children.iter().copied().filter(|&c| self.in_flow(c)).collect();
            let mut used = gap * flow.len().saturating_sub(1) as f32;
            let mut fills = 0;
            for &c in &flow {
                if self.sizing(c, axis).is_fill() {
                    fills += 1;
                } else {
                    used += self.intrinsic(c, axis);
                }
            }
            if fills > 0 {
                fill_share = ((inner - used) / fills as f32).max(0.0);
            }
        }

        for c in children {
            let flowing = self.in_flow(c);
            let s = match self.sizing(c, axis) {
                Sizing::Fill(_) if flowing && dir != Dir::None => {
                    if main {
                        fill_share
                    } else {
                        inner
                    }
                }
                Sizing::Fill(Some(f)) => f,
                _ => self.intrinsic(c, axis),
            };
            self.assign(c, axis, s);
        }
    }

    fn place(&mut self, i: u32) {
        let n = &self.nodes[i as usize];
        if !n.is_container() || n.children.is_empty() {
            return;
        }
        let dir = n.layout;
        let pad = n.padding;
        let gap = n.gap;
        let justify = n.justify;
        let align = n.align;
        let (w, h) = (n.rect.w, n.rect.h);
        let children = n.children.clone();

        if dir == Dir::None {
            for &c in &children {
                let cn = &mut self.nodes[c as usize];
                cn.rect.x = cn.x;
                cn.rect.y = cn.y;
            }
        } else {
            let horizontal = dir == Dir::Horizontal;
            let flow: Vec<u32> = children.iter().copied().filter(|&c| self.in_flow(c)).collect();
            let main_size = |n: &SNode| if horizontal { n.rect.w } else { n.rect.h };
            let cross_size = |n: &SNode| if horizontal { n.rect.h } else { n.rect.w };
            let (main_start, main_inner) = if horizontal { (pad[3], w - pad[1] - pad[3]) } else { (pad[0], h - pad[0] - pad[2]) };
            let (cross_start, cross_inner) = if horizontal { (pad[0], h - pad[0] - pad[2]) } else { (pad[3], w - pad[1] - pad[3]) };
            let count = flow.len();
            let used: f32 = flow.iter().map(|&c| main_size(&self.nodes[c as usize])).sum::<f32>()
                + gap * count.saturating_sub(1) as f32;
            let free = main_inner - used;
            let (mut cursor, step) = match justify {
                Justify::Start => (0.0, gap),
                Justify::Center => (free / 2.0, gap),
                Justify::End => (free, gap),
                Justify::SpaceBetween if count > 1 && free > 0.0 => (0.0, gap + free / (count - 1) as f32),
                Justify::SpaceAround if count > 0 && free > 0.0 => {
                    let each = free / count as f32;
                    (each / 2.0, gap + each)
                }
                Justify::SpaceBetween | Justify::SpaceAround => (0.0, gap),
            };
            cursor += main_start;
            for &c in &flow {
                let cn = &mut self.nodes[c as usize];
                let cs = cross_size(cn);
                let cross = cross_start
                    + match align {
                        Align::Start => 0.0,
                        Align::Center => (cross_inner - cs) / 2.0,
                        Align::End => cross_inner - cs,
                    };
                if horizontal {
                    cn.rect.x = cursor;
                    cn.rect.y = cross;
                } else {
                    cn.rect.x = cross;
                    cn.rect.y = cursor;
                }
                cursor += main_size(cn) + step;
            }
            for &c in &children {
                let cn = &mut self.nodes[c as usize];
                if cn.absolute || !cn.enabled {
                    cn.rect.x = cn.x;
                    cn.rect.y = cn.y;
                }
            }
        }
        for c in children {
            self.place(c);
        }
    }
}

/// How far a node sticks out of its parent, for `ctx.problems`.
pub fn clip_problem(scene: &RootScene, i: u32) -> Option<&'static str> {
    let n = &scene.nodes[i as usize];
    if n.parent == NONE || !n.enabled {
        return None;
    }
    let p = &scene.nodes[n.parent as usize];
    let (pw, ph) = (p.rect.w, p.rect.h);
    let r = n.rect;
    const EPS: f32 = 0.5;
    let inside = r.x >= -EPS && r.y >= -EPS && r.right() <= pw + EPS && r.bottom() <= ph + EPS;
    if inside {
        return None;
    }
    let outside = r.right() <= 0.0 || r.bottom() <= 0.0 || r.x >= pw || r.y >= ph;
    Some(if outside { "fully clipped" } else { "partially clipped" })
}

/// 2D affine transform: x' = a·x + c·y + e, y' = b·x + d·y + f.
#[derive(Clone, Copy, Debug)]
pub struct Affine([f32; 6]);

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

    pub fn then(&self, o: &Affine) -> Affine {
        // self ∘ o: apply o first, then self.
        let [a, b, c, d, e, f] = self.0;
        let [oa, ob, oc, od, oe, of] = o.0;
        Affine([
            a * oa + c * ob,
            b * oa + d * ob,
            a * oc + c * od,
            b * oc + d * od,
            a * oe + c * of + e,
            b * oe + d * of + f,
        ])
    }

    pub fn invert(&self) -> Option<Affine> {
        let [a, b, c, d, e, f] = self.0;
        let det = a * d - b * c;
        if det.abs() < 1e-12 {
            return None;
        }
        let (ia, ib, ic, id) = (d / det, -b / det, -c / det, a / det);
        Some(Affine([ia, ib, ic, id, -(ia * e + ic * f), -(ib * e + id * f)]))
    }

    pub fn translation(&self) -> (f32, f32) {
        (self.0[4], self.0[5])
    }

    pub fn map(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.0;
        (a * x + c * y + e, b * x + d * y + f)
    }

    pub fn map_rect(&self, r: &Rect) -> Rect {
        let pts = [self.map(r.x, r.y), self.map(r.right(), r.y), self.map(r.x, r.bottom()), self.map(r.right(), r.bottom())];
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// The local transform of a node relative to its parent: position, CCW rotation, flips.
    pub fn of_node(n: &SNode) -> Affine {
        let mut m = Affine([1.0, 0.0, 0.0, 1.0, n.rect.x, n.rect.y]);
        if n.rotation != 0.0 {
            let (s, c) = (-n.rotation).to_radians().sin_cos();
            m = m.then(&Affine([c, s, -s, c, 0.0, 0.0]));
        }
        if n.flip_x || n.flip_y {
            let sx = if n.flip_x { -1.0 } else { 1.0 };
            let sy = if n.flip_y { -1.0 } else { 1.0 };
            let tx = if n.flip_x { n.rect.w } else { 0.0 };
            let ty = if n.flip_y { n.rect.h } else { 0.0 };
            m = m.then(&Affine([sx, 0.0, 0.0, sy, tx, ty]));
        }
        m
    }
}

/// How far a node's paint can reach beyond its box (strokes, shadows, blur, glyph overhang).
pub fn visual_outset(n: &SNode) -> f32 {
    let mut o: f32 = 0.0;
    if let Some(s) = &n.stroke {
        o = o.max(match s.align {
            StrokeAlign::Inner => 0.0,
            StrokeAlign::Center => s.max_width() / 2.0,
            StrokeAlign::Outer => s.max_width(),
        });
    }
    for e in &n.effects {
        o = o.max(match e {
            Effect::Blur { radius } => radius * 1.5,
            Effect::BackgroundBlur { .. } => 0.0,
            Effect::Shadow { inner: false, offset, spread, blur, .. } => offset[0].abs().max(offset[1].abs()) + spread + blur * 1.5,
            Effect::Shadow { .. } => 0.0,
        });
    }
    if let Kind::Text(t) = &n.kind {
        o = o.max(t.style.size * 0.5);
    }
    o
}

pub fn compute_bounds(scene: &mut RootScene) {
    let mut out = vec![Rect::new(0.0, 0.0, -1.0, -1.0); scene.nodes.len()];
    if !scene.nodes.is_empty() {
        bounds_rec(&scene.nodes, 0, &Affine::IDENTITY, &mut out);
    }
    scene.bounds = out;
}

fn bounds_rec(nodes: &[SNode], i: u32, parent: &Affine, out: &mut [Rect]) -> Option<Rect> {
    let n = &nodes[i as usize];
    if !n.enabled {
        return None;
    }
    let m = parent.then(&Affine::of_node(n));
    let own = m.map_rect(&Rect::new(0.0, 0.0, n.rect.w, n.rect.h).outset(visual_outset(n)));
    let mut acc = own;
    for &c in &n.children {
        if let Some(cb) = bounds_rec(nodes, c, &m, out) {
            if !n.clips() {
                acc = acc.union(&cb);
            }
        }
    }
    out[i as usize] = acc;
    Some(acc)
}
