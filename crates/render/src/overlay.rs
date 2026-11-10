//! Agent-presence overlays drawn in screen space over the canvas, shared by the desktop window
//! and the web viewer: the pen nib that draws paths in, the scan sweep over nodes an agent reads,
//! and the glow around frames it's editing.

use skia_safe::{Canvas, Color4f, Matrix, Paint, PaintStyle, Path as SkPath};

/// Text for overlay labels (the window's fallback-aware UI font, or a plain font on the web).
pub trait Label {
    fn measure(&self, s: &str) -> f32;
    fn draw(&self, canvas: &Canvas, s: &str, at: (f32, f32), paint: &Paint);
}

/// A top-level frame's title above its top-left corner, in screen space (CSS px).
pub struct Title {
    /// The frame's id.
    pub key: String,
    /// The name as shown (ellipsized to the frame's width).
    pub text: String,
    /// Its baseline.
    pub at: (f32, f32),
    /// Where a click selects the frame.
    pub hit: skia_safe::Rect,
}

/// Titles of the frames on screen, from each frame's id, name and on-screen top-left and width.
/// Frames narrower than 40 px get none. The same boxes are drawn and clicked.
pub fn frame_titles<'a>(frames: impl IntoIterator<Item = (&'a str, &'a str, f32, f32, f32)>, view: (f32, f32), font: &impl Label) -> Vec<Title> {
    let (ww, wh) = view;
    frames
        .into_iter()
        .filter(|&(_, _, sx, sy, sw)| !(sw < 40.0 || sy < -4.0 || sy > wh + 20.0 || sx > ww || sx + sw < 0.0))
        .map(|(key, name, sx, sy, sw)| {
            let text = ellipsize(font, name, sw);
            let w = font.measure(&text).max(12.0);
            Title { key: key.to_string(), at: (sx, sy - 6.0), hit: skia_safe::Rect::from_xywh(sx, sy - 21.0, w, 19.0), text }
        })
        .collect()
}

/// `s`, cut short with an ellipsis to fit `max` px.
pub fn ellipsize(font: &impl Label, s: &str, max: f32) -> String {
    if font.measure(s) <= max {
        return s.to_string();
    }
    let mut out = s.to_string();
    while !out.is_empty() && font.measure(&format!("{out}…")) > max {
        out.pop();
    }
    format!("{out}…")
}

/// The calligraphy pen that draws new paths in: study's logo nib, tip at (x, y).
pub fn draw_nib(canvas: &Canvas, x: f32, y: f32) {
    const NIB: &str = "M50 88 C44 76 31 61 31 47 L31 31 C31 25 35 21 41 21 L59 21 C65 21 69 25 69 31 L69 47 C69 61 56 76 50 88 Z \
                       M50 41 A6 6 0 1 0 50.01 41 Z M48.8 53 L51.2 53 L50.5 82 L49.5 82 Z";
    let Some(path) = SkPath::from_svg(NIB) else { return };
    let mut m = Matrix::translate((x, y));
    m.pre_rotate(32.0, None);
    m.pre_scale((0.5, 0.5), None);
    m.pre_translate((-50.0, -88.0));
    let mut path = path.make_transform(&m);
    path.set_fill_type(skia_safe::PathFillType::EvenOdd);
    let mut shadow = Paint::new(Color4f::new(0.0, 0.0, 0.0, 0.25), None);
    shadow.set_anti_alias(true);
    shadow.set_mask_filter(skia_safe::MaskFilter::blur(skia_safe::BlurStyle::Normal, 2.0, None));
    canvas.save();
    canvas.translate((1.5, 2.0));
    canvas.draw_path(&path, &shadow);
    canvas.restore();
    let mut fill = Paint::new(Color4f::new(0.97, 0.78, 0.42, 1.0), None);
    fill.set_anti_alias(true);
    canvas.draw_path(&path, &fill);
    let mut edge = Paint::new(Color4f::new(0.36, 0.24, 0.08, 1.0), None);
    edge.set_anti_alias(true);
    edge.set_style(PaintStyle::Stroke);
    edge.set_stroke_width(1.0);
    canvas.draw_path(&path, &edge);
}

const ACCENT: (f32, f32, f32) = (0.23, 0.51, 0.96);

pub fn accent(a: f32) -> Color4f {
    Color4f::new(ACCENT.0, ACCENT.1, ACCENT.2, a)
}

/// The "agent is reading this" effect, `t` in 0..1: a scan line sweeps down the node's box
/// (first 55%), then the highlighted box lingers and fades.
pub fn draw_scan(canvas: &Canvas, r: skia_safe::Rect, t: f32) {
    let sweep = (t / 0.55).min(1.0);
    let ease = 1.0 - (1.0 - sweep).powi(3);
    let fade = if t < 0.55 { 1.0 } else { 1.0 - ((t - 0.55) / 0.45).powi(2) };
    let y = r.top + ease * r.height();
    canvas.save();
    canvas.clip_rect(r, None, true);
    let mut tint = Paint::new(accent(0.10 * fade), None);
    tint.set_anti_alias(true);
    canvas.draw_rect(skia_safe::Rect::new(r.left, r.top, r.right, y), &tint);
    if sweep < 1.0 {
        let trail = (r.height() * 0.4).clamp(16.0, 140.0);
        let colors = [accent(0.0).to_color(), accent(0.45).to_color()];
        #[allow(deprecated)]
        let shader = skia_safe::gradient_shader::linear(((r.left, y - trail), (r.left, y)), colors.as_slice(), None, skia_safe::TileMode::Clamp, None, None);
        let mut band = Paint::default();
        band.set_shader(shader);
        canvas.draw_rect(skia_safe::Rect::new(r.left, y - trail, r.right, y), &band);
        let mut glow = Paint::new(accent(0.8), None);
        glow.set_anti_alias(true);
        glow.set_mask_filter(skia_safe::MaskFilter::blur(skia_safe::BlurStyle::Normal, 3.0, None));
        canvas.draw_rect(skia_safe::Rect::new(r.left, y - 2.0, r.right, y + 2.0), &glow);
        let mut line = Paint::new(accent(1.0), None);
        line.set_anti_alias(true);
        line.set_stroke_width(2.0);
        canvas.draw_line((r.left, y), (r.right, y), &line);
    }
    canvas.restore();
    let mut edge = Paint::new(accent(0.9 * fade), None);
    edge.set_anti_alias(true);
    edge.set_style(PaintStyle::Stroke);
    edge.set_stroke_width(1.5);
    canvas.draw_rect(r, &edge);
}

/// "The agent is working here": a softly pulsing glow around a top-level frame, plus a small
/// badge above its top-right corner.
pub fn draw_working(canvas: &Canvas, r: skia_safe::Rect, radius: f32, strength: f32, clock: f32, font: &dyn Label) {
    if strength <= 0.0 {
        return;
    }
    let pulse = 0.7 + 0.3 * (clock * std::f32::consts::TAU / 1.8).sin();
    let rr = skia_safe::RRect::new_rect_xy(r, radius, radius);
    for (width, sigma, alpha) in [(10.0, 14.0, 0.45), (4.0, 5.0, 0.6)] {
        let mut p = Paint::new(accent(alpha * pulse * strength), None);
        p.set_anti_alias(true);
        p.set_style(PaintStyle::Stroke);
        p.set_stroke_width(width);
        p.set_mask_filter(skia_safe::MaskFilter::blur(skia_safe::BlurStyle::Outer, sigma, None));
        canvas.draw_rrect(rr, &p);
    }
    let mut edge = Paint::new(accent(0.85 * strength), None);
    edge.set_anti_alias(true);
    edge.set_style(PaintStyle::Stroke);
    edge.set_stroke_width(1.5);
    canvas.draw_rrect(rr, &edge);

    let label = "Claude is working";
    let tw = font.measure(label);
    let badge = skia_safe::Rect::from_xywh(r.right - tw - 30.0, r.top - 26.0, tw + 30.0, 20.0);
    if badge.left < r.left {
        return;
    }
    let mut bg = Paint::new(accent(strength), None);
    bg.set_anti_alias(true);
    canvas.draw_round_rect(badge, 10.0, 10.0, &bg);
    let mut dot = Paint::new(Color4f::new(1.0, 1.0, 1.0, strength * (0.45 + 0.55 * pulse)), None);
    dot.set_anti_alias(true);
    canvas.draw_circle((badge.left + 11.0, badge.center_y()), 3.5, &dot);
    font.draw(canvas, label, (badge.left + 20.0, badge.center_y() + 4.0), &Paint::new(Color4f::new(1.0, 1.0, 1.0, strength), None));
}

