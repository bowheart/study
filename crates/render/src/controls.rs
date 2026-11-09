//! The canvas's own controls, shared by the desktop window and the web viewer: a breadcrumb of the
//! selected node and its ancestors (top left, click one to select it), and zoom buttons with a
//! light/dark switch (bottom right). Callers lay them out each frame from the current state,
//! draw them over the canvas, and route clicks to `Layout::hit` before the canvas sees them.

use crate::overlay::Label;
use skia_safe::{Canvas, Color4f, Contains, Paint, PaintStyle, Path, PathOp, RRect, Rect};

/// One breadcrumb entry: a node's id (or instance path) and what to call it.
#[derive(Clone, Debug, PartialEq)]
pub struct Crumb {
    pub key: String,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Index into the crumbs.
    Crumb(usize),
    ZoomOut,
    /// The zoom percentage: fits everything.
    ZoomLevel,
    ZoomIn,
    Light,
    Dark,
}

/// Which controls to show.
#[derive(Clone, Copy, Debug)]
pub struct Show {
    pub breadcrumb: bool,
    pub zoom: bool,
    pub theme: bool,
}

impl Default for Show {
    fn default() -> Show {
        Show { breadcrumb: true, zoom: true, theme: true }
    }
}

/// Zoom step of the − and + buttons.
pub const ZOOM_STEP: f32 = 1.25;

const H: f32 = 28.0;
const MARGIN: f32 = 12.0;
const SEP: &str = "›";
const MAX_LABEL: f32 = 180.0;

enum Piece {
    /// Crumb index, shown text, box.
    Crumb(usize, String, Rect),
    /// Collapsed middle crumbs.
    Ellipsis(Rect),
    Sep(Rect),
}

pub struct Layout {
    crumb_bar: Option<Rect>,
    pieces: Vec<Piece>,
    zoom_bar: Option<Rect>,
    zoom: Option<([Rect; 3], String)>,
    theme: Option<Rect>,
}

/// Lays the controls out for a canvas of `size` (logical pixels).
pub fn layout(show: Show, size: (f32, f32), crumbs: &[Crumb], zoom: f32, font: &dyn Label) -> Layout {
    let (ww, wh) = size;
    let mut out = Layout { crumb_bar: None, pieces: Vec::new(), zoom_bar: None, zoom: None, theme: None };

    // Bottom right, right to left: zoom, then the theme switch.
    let mut right = ww - MARGIN;
    let top = wh - MARGIN - H;
    if show.zoom {
        let text = format!("{:.0}%", zoom * 100.0);
        let level_w = font.measure("0000%").max(font.measure(&text)) + 12.0;
        let w = H + level_w + H;
        let bar = Rect::from_xywh(right - w, top, w, H);
        let minus = Rect::from_xywh(bar.left, top, H, H);
        let level = Rect::from_xywh(minus.right, top, level_w, H);
        let plus = Rect::from_xywh(level.right, top, H, H);
        out.zoom_bar = Some(bar);
        out.zoom = Some(([minus, level, plus], text));
        right = bar.left - 8.0;
    }
    if show.theme {
        out.theme = Some(Rect::from_xywh(right - 56.0, top, 56.0, H));
    }

    // Top left: root › … › parent › selected, collapsing the middle when it doesn't fit.
    if show.breadcrumb && !crumbs.is_empty() {
        let labels: Vec<String> = crumbs.iter().map(|c| ellipsize(font, &c.label, MAX_LABEL)).collect();
        let sep_w = font.measure(SEP) + 12.0;
        let width = |idx: &[Option<usize>]| -> f32 {
            idx.iter().map(|i| i.map_or(font.measure("…"), |i| font.measure(&labels[i])) + 2.0 * 6.0).sum::<f32>() + sep_w * (idx.len().saturating_sub(1)) as f32 + 2.0 * 4.0
        };
        let max = (ww - 2.0 * MARGIN).max(120.0);
        let mut shown: Vec<Option<usize>> = (0..crumbs.len()).map(Some).collect();
        // Collapse crumbs after the first into "…" until it fits, keeping the selected node and
        // its parent.
        if width(&shown) > max && crumbs.len() > 3 {
            for start in 2..crumbs.len() - 1 {
                shown = [Some(0), None].into_iter().chain((start..crumbs.len()).map(Some)).collect();
                if width(&shown) <= max {
                    break;
                }
            }
        }
        let bar = Rect::from_xywh(MARGIN, MARGIN, width(&shown).min(max), H);
        let mut x = bar.left + 4.0;
        for (n, item) in shown.iter().enumerate() {
            if n > 0 {
                out.pieces.push(Piece::Sep(Rect::from_xywh(x, bar.top, sep_w, H)));
                x += sep_w;
            }
            match item {
                Some(i) => {
                    let w = font.measure(&labels[*i]) + 12.0;
                    out.pieces.push(Piece::Crumb(*i, labels[*i].clone(), Rect::from_xywh(x, bar.top + 3.0, w, H - 6.0)));
                    x += w;
                }
                None => {
                    let w = font.measure("…") + 12.0;
                    out.pieces.push(Piece::Ellipsis(Rect::from_xywh(x, bar.top + 3.0, w, H - 6.0)));
                    x += w;
                }
            }
        }
        out.crumb_bar = Some(bar);
    }
    out
}

impl Layout {
    /// The control under a point, if any.
    pub fn hit(&self, x: f32, y: f32) -> Option<Control> {
        for p in &self.pieces {
            if let Piece::Crumb(i, _, r) = p {
                if r.contains(skia_safe::Point::new(x, y)) {
                    return Some(Control::Crumb(*i));
                }
            }
        }
        if let Some(([minus, level, plus], _)) = &self.zoom {
            for (r, c) in [(minus, Control::ZoomOut), (level, Control::ZoomLevel), (plus, Control::ZoomIn)] {
                if r.contains(skia_safe::Point::new(x, y)) {
                    return Some(c);
                }
            }
        }
        let t = self.theme?;
        t.contains(skia_safe::Point::new(x, y)).then(|| if x < t.center_x() { Control::Light } else { Control::Dark })
    }

    /// Whether a point is on any control's panel (so the canvas shouldn't react to it).
    pub fn covers(&self, x: f32, y: f32) -> bool {
        [self.crumb_bar, self.zoom_bar, self.theme].iter().flatten().any(|r| r.contains(skia_safe::Point::new(x, y)))
    }

    pub fn draw(&self, canvas: &Canvas, dark: bool, hover: Option<Control>, font: &dyn Label) {
        let c = Colors::new(dark);
        let selected = self.pieces.iter().filter_map(|p| if let Piece::Crumb(i, ..) = p { Some(*i) } else { None }).max();
        if let Some(bar) = self.crumb_bar {
            panel(canvas, bar, &c);
            canvas.save();
            canvas.clip_rect(bar, None, true);
            for p in &self.pieces {
                match p {
                    Piece::Crumb(i, text, r) => {
                        let current = Some(*i) == selected;
                        if hover == Some(Control::Crumb(*i)) || current {
                            let mut bg = Paint::new(if current { c.accent_bg } else { c.hover }, None);
                            bg.set_anti_alias(true);
                            canvas.draw_round_rect(*r, 5.0, 5.0, &bg);
                        }
                        let fg = Paint::new(if current { c.accent } else { c.text }, None);
                        font.draw(canvas, text, (r.left + 6.0, r.center_y() + 4.0), &fg);
                    }
                    Piece::Ellipsis(r) => font.draw(canvas, "…", (r.left + 6.0, r.center_y() + 4.0), &Paint::new(c.dim, None)),
                    Piece::Sep(r) => {
                        let w = font.measure(SEP);
                        font.draw(canvas, SEP, (r.center_x() - w / 2.0, r.center_y() + 4.0), &Paint::new(c.dim, None));
                    }
                }
            }
            canvas.restore();
        }
        if let (Some(bar), Some(([minus, level, plus], text))) = (self.zoom_bar, &self.zoom) {
            panel(canvas, bar, &c);
            for (r, ctl) in [(*minus, Control::ZoomOut), (*level, Control::ZoomLevel), (*plus, Control::ZoomIn)] {
                if hover == Some(ctl) {
                    let mut bg = Paint::new(c.hover, None);
                    bg.set_anti_alias(true);
                    canvas.draw_round_rect(r.with_inset((3.0, 3.0)), 5.0, 5.0, &bg);
                }
            }
            let mut stroke = Paint::new(c.text, None);
            stroke.set_anti_alias(true);
            stroke.set_style(PaintStyle::Stroke);
            stroke.set_stroke_width(1.5);
            stroke.set_stroke_cap(skia_safe::PaintCap::Round);
            let (mx, my) = (minus.center_x(), minus.center_y());
            canvas.draw_line((mx - 5.0, my), (mx + 5.0, my), &stroke);
            let (px, py) = (plus.center_x(), plus.center_y());
            canvas.draw_line((px - 5.0, py), (px + 5.0, py), &stroke);
            canvas.draw_line((px, py - 5.0), (px, py + 5.0), &stroke);
            let tw = font.measure(text);
            font.draw(canvas, text, (level.center_x() - tw / 2.0, level.center_y() + 4.0), &Paint::new(c.text, None));
        }
        if let Some(r) = self.theme {
            theme_switch(canvas, r, dark, hover, &c);
        }
    }
}

struct Colors {
    panel: Color4f,
    border: Color4f,
    text: Color4f,
    dim: Color4f,
    hover: Color4f,
    accent: Color4f,
    accent_bg: Color4f,
    knob: Color4f,
}

impl Colors {
    fn new(dark: bool) -> Colors {
        if dark {
            Colors {
                panel: Color4f::new(0.16, 0.16, 0.18, 0.96),
                border: Color4f::new(1.0, 1.0, 1.0, 0.08),
                text: Color4f::new(0.90, 0.90, 0.93, 1.0),
                dim: Color4f::new(0.55, 0.55, 0.60, 1.0),
                hover: Color4f::new(1.0, 1.0, 1.0, 0.08),
                accent: Color4f::new(0.55, 0.74, 1.0, 1.0),
                accent_bg: Color4f::new(0.23, 0.51, 0.96, 0.22),
                knob: Color4f::new(1.0, 1.0, 1.0, 0.16),
            }
        } else {
            Colors {
                panel: Color4f::new(1.0, 1.0, 1.0, 0.96),
                border: Color4f::new(0.0, 0.0, 0.0, 0.10),
                text: Color4f::new(0.15, 0.15, 0.17, 1.0),
                dim: Color4f::new(0.55, 0.55, 0.58, 1.0),
                hover: Color4f::new(0.0, 0.0, 0.0, 0.06),
                accent: Color4f::new(0.10, 0.40, 0.90, 1.0),
                accent_bg: Color4f::new(0.23, 0.51, 0.96, 0.12),
                knob: Color4f::new(0.0, 0.0, 0.0, 0.08),
            }
        }
    }
}

/// A floating card behind a control group.
fn panel(canvas: &Canvas, r: Rect, c: &Colors) {
    let rr = RRect::new_rect_xy(r, 8.0, 8.0);
    let mut shadow = Paint::new(Color4f::new(0.0, 0.0, 0.0, 0.10), None);
    shadow.set_anti_alias(true);
    shadow.set_mask_filter(skia_safe::MaskFilter::blur(skia_safe::BlurStyle::Normal, 4.0, None));
    canvas.draw_rrect(rr.with_offset((0.0, 1.0)), &shadow);
    let mut fill = Paint::new(c.panel, None);
    fill.set_anti_alias(true);
    canvas.draw_rrect(rr, &fill);
    let mut edge = Paint::new(c.border, None);
    edge.set_anti_alias(true);
    edge.set_style(PaintStyle::Stroke);
    edge.set_stroke_width(1.0);
    canvas.draw_rrect(rr, &edge);
}

/// Sun | moon, with the current theme's half raised.
fn theme_switch(canvas: &Canvas, r: Rect, dark: bool, hover: Option<Control>, c: &Colors) {
    panel(canvas, r, c);
    let half = r.width() / 2.0;
    let side = |left: bool| Rect::from_xywh(r.left + if left { 0.0 } else { half } + 3.0, r.top + 3.0, half - 6.0, r.height() - 6.0);
    let mut p = Paint::new(c.knob, None);
    p.set_anti_alias(true);
    canvas.draw_round_rect(side(!dark), 5.0, 5.0, &p);
    let hovered = match hover {
        Some(Control::Light) if dark => Some(side(true)),
        Some(Control::Dark) if !dark => Some(side(false)),
        _ => None,
    };
    if let Some(h) = hovered {
        p.set_color4f(c.hover, None);
        canvas.draw_round_rect(h, 5.0, 5.0, &p);
    }

    let (sx, sy) = (r.left + half / 2.0, r.center_y());
    let mut sun = Paint::new(if dark { c.dim } else { c.text }, None);
    sun.set_anti_alias(true);
    canvas.draw_circle((sx, sy), 3.0, &sun);
    sun.set_style(PaintStyle::Stroke);
    sun.set_stroke_width(1.2);
    sun.set_stroke_cap(skia_safe::PaintCap::Round);
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::FRAC_PI_4;
        let (cs, sn) = (a.cos(), a.sin());
        canvas.draw_line((sx + cs * 4.8, sy + sn * 4.8), (sx + cs * 6.4, sy + sn * 6.4), &sun);
    }
    let (mx, my) = (r.left + half * 1.5, r.center_y());
    let mut moon = Path::circle((mx, my), 5.0, None);
    if let Some(m) = moon.op(&Path::circle((mx + 3.2, my - 2.4), 4.2, None), PathOp::Difference) {
        moon = m;
    }
    let mut mp = Paint::new(if dark { c.text } else { c.dim }, None);
    mp.set_anti_alias(true);
    canvas.draw_path(&moon, &mp);
}

fn ellipsize(font: &dyn Label, s: &str, max: f32) -> String {
    if font.measure(s) <= max {
        return s.to_string();
    }
    let mut out = s.to_string();
    while !out.is_empty() && font.measure(&format!("{out}…")) > max {
        out.pop();
    }
    format!("{out}…")
}
