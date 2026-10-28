//! HTML export from the resolved scene. Flex layout maps directly onto CSS flexbox;
//! Pen strokes never affect layout, so they become box-shadows/outlines, not borders.

use crate::engine::Engine;
use anyhow::{anyhow, Result};
use pen::scene::{Kind, RootScene, SNode};
use pen::style::*;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::Path;

struct Gen<'a> {
    tailwind: bool,
    names: bool,
    ids: bool,
    fonts: BTreeSet<(String, u16)>,
    out: String,
    engine: &'a mut Engine,
    pen_dir: std::path::PathBuf,
    out_dir: std::path::PathBuf,
}

pub fn export(engine: &mut Engine, ids: &[String], tailwind: bool, opts: &Value, out_dir: &Path) -> Result<String> {
    let scaffold = opts.get("includeHtmlScaffold").and_then(Value::as_bool).unwrap_or(true);
    let pen_dir = engine.path().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let mut g = Gen {
        tailwind,
        names: opts.get("includeLayerNames").and_then(Value::as_bool).unwrap_or(true),
        ids: opts.get("includeLayerIds").and_then(Value::as_bool).unwrap_or(false),
        fonts: BTreeSet::new(),
        out: String::new(),
        engine,
        pen_dir,
        out_dir: out_dir.to_path_buf(),
    };
    for id in ids {
        let (root, idx) = {
            let (s, i) = g.engine.locate(id).ok_or_else(|| anyhow!("no rendered node '{id}'"))?;
            (s.doc_root, i)
        };
        let root = root.ok_or_else(|| anyhow!("no scene"))?;
        let scene = g.engine.scenes.get(&root).cloned().ok_or_else(|| anyhow!("no scene"))?;
        g.node(&scene, idx, None, 0);
    }
    if !scaffold {
        return Ok(g.out);
    }
    let mut fonts = String::new();
    if !g.fonts.is_empty() {
        let mut fams: std::collections::BTreeMap<&str, BTreeSet<u16>> = Default::default();
        for (f, w) in &g.fonts {
            fams.entry(f.as_str()).or_default().insert(*w);
        }
        let q: Vec<String> = fams
            .iter()
            .map(|(f, ws)| format!("family={}:wght@{}", f.replace(' ', "+"), ws.iter().map(|w| w.to_string()).collect::<Vec<_>>().join(";")))
            .collect();
        fonts = format!(
            "  <link rel=\"preconnect\" href=\"https://fonts.googleapis.com\">\n  <link rel=\"stylesheet\" href=\"https://fonts.googleapis.com/css2?{}&display=swap\">\n",
            q.join("&")
        );
    }
    let tw = if tailwind { "  <script src=\"https://cdn.jsdelivr.net/npm/@tailwindcss/browser@4.1.11\"></script>\n" } else { "" };
    Ok(format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  <title>{}</title>\n{fonts}{tw}  <style>\n    * {{ box-sizing: border-box; }}\n    body {{ margin: 0; }}\n    p {{ margin: 0; }}\n  </style>\n</head>\n<body>\n{}</body>\n</html>\n",
        esc(&ids.join(", ")),
        g.out
    ))
}

fn px(v: f32) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r == r.trunc() {
        format!("{}px", r as i64)
    } else {
        format!("{r}px")
    }
}

fn color(c: Rgba) -> String {
    c.hex()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// CSS for a fill list (topmost first for CSS layering).
fn backgrounds(g: &Gen, fills: &[Paint]) -> Option<String> {
    let mut layers = Vec::new();
    let mut base: Option<String> = None;
    for (i, f) in fills.iter().enumerate().rev() {
        let top_solid = i == 0;
        match f {
            Paint::Solid { color: c, .. } if top_solid && layers.is_empty() => base = Some(color(*c)),
            Paint::Solid { color: c, .. } => layers.push(format!("linear-gradient({0}, {0})", color(*c))),
            Paint::Gradient { kind, stops, center, size, rotation, opacity, .. } => {
                let s: Vec<String> = stops.iter().map(|(p, c)| format!("{} {}%", color(c.with_alpha(*opacity)), (p * 100.0).round())).collect();
                layers.push(match kind {
                    GradientKind::Linear => format!("linear-gradient({}deg, {})", ((180.0 - rotation) % 360.0 + 360.0) % 360.0, s.join(", ")),
                    GradientKind::Radial => format!(
                        "radial-gradient(ellipse {}% {}% at {}% {}%, {})",
                        size[0] * 50.0,
                        size[1] * 50.0,
                        center[0] * 100.0,
                        center[1] * 100.0,
                        s.join(", ")
                    ),
                    GradientKind::Angular => {
                        format!("conic-gradient(from {}deg at {}% {}%, {})", -rotation, center[0] * 100.0, center[1] * 100.0, s.join(", "))
                    }
                });
            }
            Paint::Image { url, mode, .. } => {
                let size = match mode {
                    ImageMode::Stretch => "100% 100%",
                    ImageMode::Fill => "cover",
                    ImageMode::Fit => "contain",
                };
                layers.push(format!("url(\"{}\") center / {size} no-repeat", g.asset_url(url)));
            }
            // Shaders can't be expressed in CSS; the fills beneath them show through.
            Paint::Shader { .. } => {}
            other => {
                let c = other.average();
                layers.push(format!("linear-gradient({0}, {0})", color(c)));
            }
        }
    }
    layers.reverse();
    match (layers.is_empty(), base) {
        (true, None) => None,
        (true, Some(b)) => Some(b),
        (false, b) => {
            let mut s = layers.join(", ");
            if let Some(b) = b {
                s = format!("{s}, {b}");
            }
            Some(s)
        }
    }
}

impl Gen<'_> {
    fn asset_url(&self, url: &str) -> String {
        if url.contains("://") {
            return url.to_string();
        }
        let abs = self.pen_dir.join(url);
        relative(&self.out_dir, &abs).unwrap_or_else(|| url.to_string())
    }

    fn node(&mut self, scene: &RootScene, i: u32, parent: Option<&SNode>, depth: usize) {
        let n = &scene.nodes[i as usize];
        if !n.enabled {
            return;
        }
        let mut css: Vec<(String, String)> = Vec::new();
        let mut push = |k: &str, v: String| css.push((k.to_string(), v));
        let parent_flex = parent.map(|p| p.layout != Dir::None).unwrap_or(false);
        let is_root = parent.is_none();

        // Position
        if is_root {
            push("position", "relative".into());
        } else if !parent_flex || n.absolute {
            push("position", "absolute".into());
            push("left", px(n.rect.x));
            push("top", px(n.rect.y));
        }
        if n.rotation != 0.0 {
            push("transform", format!("rotate({}deg)", -n.rotation));
            push("transform-origin", "top left".into());
        }

        // Size
        let horizontal_parent = parent.map(|p| p.layout == Dir::Horizontal).unwrap_or(false);
        for (axis, sizing, value) in [("width", n.width, n.rect.w), ("height", n.height, n.rect.h)] {
            let main = parent_flex && !n.absolute && ((axis == "width") == horizontal_parent);
            let text_auto = matches!(&n.kind, Kind::Text(t) if t.growth == TextGrowth::Auto || (axis == "height" && t.growth == TextGrowth::FixedWidth));
            match sizing {
                Sizing::Fill(_) if parent_flex && !n.absolute => {
                    if main {
                        push("flex", "1 1 0".into());
                        push(if axis == "width" { "min-width" } else { "min-height" }, "0".into());
                    } else {
                        push("align-self", "stretch".into());
                    }
                }
                Sizing::Fit(_) if n.is_container() || text_auto => {
                    if parent_flex {
                        push("flex-shrink", "0".into());
                    }
                }
                _ if text_auto => {}
                _ => {
                    push(axis, px(value));
                    if parent_flex && main {
                        push("flex-shrink", "0".into());
                    }
                }
            }
        }

        // Layout
        if matches!(n.kind, Kind::Frame { .. }) {
            if n.layout != Dir::None {
                push("display", "flex".into());
                if n.layout == Dir::Vertical {
                    push("flex-direction", "column".into());
                }
                if n.gap != 0.0 {
                    push("gap", px(n.gap));
                }
                let jc = match n.justify {
                    Justify::Start => None,
                    Justify::Center => Some("center"),
                    Justify::End => Some("flex-end"),
                    Justify::SpaceBetween => Some("space-between"),
                    Justify::SpaceAround => Some("space-around"),
                };
                if let Some(j) = jc {
                    push("justify-content", j.into());
                }
                push(
                    "align-items",
                    match n.align {
                        Align::Start => "flex-start",
                        Align::Center => "center",
                        Align::End => "flex-end",
                    }
                    .into(),
                );
            } else if !is_root {
                // Children are absolutely positioned inside.
            }
            if n.layout == Dir::None || n.children.iter().any(|&c| scene.nodes[c as usize].absolute) {
                if !is_root && !(!parent_flex || n.absolute) {
                    push("position", "relative".into());
                }
            }
            let [t, r, b, l] = n.padding;
            if [t, r, b, l].iter().any(|&v| v != 0.0) {
                push("padding", format!("{} {} {} {}", px(t), px(r), px(b), px(l)));
            }
            if n.clips() {
                push("overflow", "hidden".into());
            }
        }

        let arc = matches!(n.kind, Kind::Ellipse { inner, sweep, .. } if inner > 0.0 || sweep.abs() < 360.0);
        // Paint
        let is_text = matches!(n.kind, Kind::Text(_));
        if !is_text && !arc && !matches!(n.kind, Kind::Icon { .. } | Kind::Path { .. }) {
            if let Some(bg) = backgrounds(self, &n.fills) {
                push("background", bg);
            }
        }
        let radius = match n.kind {
            Kind::Ellipse { .. } if arc => None,
            Kind::Ellipse { .. } => Some("50%".to_string()),
            _ if n.radius.iter().any(|&r| r > 0.0) => Some(if n.radius.iter().all(|&r| r == n.radius[0]) {
                px(n.radius[0])
            } else {
                n.radius.iter().map(|&r| px(r)).collect::<Vec<_>>().join(" ")
            }),
            _ => None,
        };
        if let Some(r) = radius {
            push("border-radius", r);
        }
        let mut shadows = Vec::new();
        if let Some(s) = &n.stroke {
            if !is_text && !matches!(n.kind, Kind::Path { .. }) {
                let c = color(s.paints.first().map(|p| p.average()).unwrap_or(Rgba::BLACK));
                match (s.align, s.is_uniform()) {
                    (StrokeAlign::Inner, true) => shadows.push(format!("inset 0 0 0 {} {c}", px(s.width[0]))),
                    (_, false) => {
                        let [t, r, b, l] = s.width;
                        if t > 0.0 {
                            shadows.push(format!("inset 0 {} 0 0 {c}", px(t)));
                        }
                        if b > 0.0 {
                            shadows.push(format!("inset 0 {} 0 0 {c}", px(-b)));
                        }
                        if l > 0.0 {
                            shadows.push(format!("inset {} 0 0 0 {c}", px(l)));
                        }
                        if r > 0.0 {
                            shadows.push(format!("inset {} 0 0 0 {c}", px(-r)));
                        }
                    }
                    (StrokeAlign::Center, true) => {
                        push("outline", format!("{} solid {c}", px(s.width[0])));
                        push("outline-offset", px(-s.width[0] / 2.0));
                    }
                    (StrokeAlign::Outer, true) => push("outline", format!("{} solid {c}", px(s.width[0]))),
                }
            }
        }
        for e in &n.effects {
            match e {
                Effect::Shadow { inner, offset, spread, blur, color: c, .. } => shadows.push(format!(
                    "{}{} {} {} {} {}",
                    if *inner { "inset " } else { "" },
                    px(offset[0]),
                    px(offset[1]),
                    px(*blur),
                    px(*spread),
                    color(*c)
                )),
                Effect::Blur { radius } => push("filter", format!("blur({})", px(radius / 2.0))),
                Effect::BackgroundBlur { radius } => push("backdrop-filter", format!("blur({})", px(radius / 2.0))),
            }
        }
        if !shadows.is_empty() && !is_text {
            push("box-shadow", shadows.join(", "));
        }
        if n.opacity < 1.0 {
            push("opacity", format!("{}", (n.opacity * 1000.0).round() / 1000.0));
        }

        // Content
        let mut inner = String::new();
        let mut tag = "div";
        match &n.kind {
            Kind::Text(t) | Kind::Note(_, t) => {
                tag = "p";
                let s = &t.style;
                self.fonts.insert((s.family.clone(), s.weight));
                push("font-family", format!("'{}', sans-serif", s.family));
                push("font-size", px(s.size));
                if s.weight != 400 {
                    push("font-weight", s.weight.to_string());
                }
                if s.italic {
                    push("font-style", "italic".into());
                }
                if s.letter_spacing != 0.0 {
                    push("letter-spacing", px(s.letter_spacing));
                }
                if let Some(lh) = s.line_height {
                    push("line-height", format!("{lh}"));
                }
                match s.align {
                    TextAlign::Left => {}
                    TextAlign::Center => push("text-align", "center".into()),
                    TextAlign::Right => push("text-align", "right".into()),
                    TextAlign::Justify => push("text-align", "justify".into()),
                }
                let mut deco = Vec::new();
                if s.underline {
                    deco.push("underline");
                }
                if s.strikethrough {
                    deco.push("line-through");
                }
                if !deco.is_empty() {
                    push("text-decoration", deco.join(" "));
                }
                push("white-space", if t.growth == TextGrowth::Auto { "pre" } else { "pre-wrap" }.into());
                match n.fills.first() {
                    Some(Paint::Solid { color: c, .. }) => push("color", color(*c)),
                    Some(other) => {
                        if let Some(bg) = backgrounds(self, std::slice::from_ref(other)) {
                            push("background", bg);
                            push("-webkit-background-clip", "text".into());
                            push("background-clip", "text".into());
                            push("color", "transparent".into());
                        }
                    }
                    None => push("color", "transparent".into()),
                }
                if t.growth == TextGrowth::FixedWidthHeight && s.valign != VAlign::Top {
                    push("display", "flex".into());
                    push("flex-direction", "column".into());
                    push("justify-content", if s.valign == VAlign::Middle { "center" } else { "flex-end" }.into());
                    inner = format!("<span>{}</span>", esc(&t.content));
                } else {
                    inner = esc(&t.content);
                }
            }
            Kind::Icon { library, name, .. } => {
                let c = n.fills.first().map(|p| p.average()).unwrap_or(Rgba::BLACK);
                if let Some(svg) = self.engine.renderer.icons.svg_text(library, name) {
                    let svg = svg.replace("currentColor", &color(c));
                    let svg = set_svg_size(&svg, n.rect.w, n.rect.h);
                    inner = svg.trim().to_string();
                    push("display", "flex".into());
                } else {
                    // Font-based icon sets (Material Symbols): ligature text.
                    self.fonts.insert((library.clone(), 400));
                    push("font-family", format!("'{library}'"));
                    push("font-size", px(n.rect.w.min(n.rect.h)));
                    push("line-height", "1".into());
                    push("color", color(c));
                    inner = esc(name);
                }
            }
            Kind::Ellipse { inner: ratio, start, sweep } if arc => {
                let fill = n.fills.first().map(|p| color(p.average())).unwrap_or_else(|| "none".into());
                inner = format!(
                    "<svg width=\"{}\" height=\"{}\" style=\"display:block\"><path d=\"{}\" fill=\"{fill}\" fill-rule=\"evenodd\"/></svg>",
                    n.rect.w,
                    n.rect.h,
                    arc_path(n.rect.w, n.rect.h, *ratio, *start, *sweep)
                );
            }
            Kind::Path { geometry, view_box, even_odd } => {
                let vb = view_box.map(|v| format!("{} {} {} {}", v[0], v[1], v[2], v[3])).unwrap_or_else(|| format!("0 0 {} {}", n.rect.w, n.rect.h));
                let fill = n.fills.first().map(|p| color(p.average())).unwrap_or_else(|| "none".into());
                let stroke = n
                    .stroke
                    .as_ref()
                    .map(|s| {
                        format!(
                            " stroke=\"{}\" stroke-width=\"{}\" vector-effect=\"non-scaling-stroke\"",
                            color(s.paints.first().map(|p| p.average()).unwrap_or(Rgba::BLACK)),
                            s.max_width()
                        )
                    })
                    .unwrap_or_default();
                inner = format!(
                    "<svg width=\"{}\" height=\"{}\" viewBox=\"{vb}\" preserveAspectRatio=\"none\" style=\"display:block\"><path d=\"{}\" fill=\"{fill}\"{}{stroke}/></svg>",
                    n.rect.w,
                    n.rect.h,
                    esc(geometry),
                    if *even_odd { " fill-rule=\"evenodd\"" } else { "" }
                );
            }
            _ => {}
        }

        // Emit
        let indent = "  ".repeat(depth + 1);
        let mut attrs = String::new();
        if self.names {
            if let Some(name) = &n.name {
                let _ = write!(attrs, " data-name=\"{}\"", esc(name));
            }
        }
        if self.ids {
            let _ = write!(attrs, " data-id=\"{}\"", esc(n.key()));
        }
        let style = if self.tailwind {
            let classes: Vec<String> = css.iter().map(|(k, v)| tw_class(k, v)).collect();
            format!(" class=\"{}\"", esc(&classes.join(" ")))
        } else {
            format!(" style=\"{}\"", esc(&css.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("; ")))
        };
        let children: Vec<u32> = n.children.clone();
        if children.is_empty() {
            let _ = writeln!(self.out, "{indent}<{tag}{attrs}{style}>{inner}</{tag}>");
        } else {
            let _ = writeln!(self.out, "{indent}<{tag}{attrs}{style}>{inner}");
            let me = n.clone();
            for c in children {
                self.node(scene, c, Some(&me), depth + 1);
            }
            let _ = writeln!(self.out, "{indent}</{tag}>");
        }
    }
}

/// Maps a CSS declaration to a Tailwind class (named utilities where obvious, arbitrary values otherwise).
fn tw_class(k: &str, v: &str) -> String {
    let named = match (k, v) {
        ("display", "flex") => Some("flex"),
        ("flex-direction", "column") => Some("flex-col"),
        ("position", "absolute") => Some("absolute"),
        ("position", "relative") => Some("relative"),
        ("overflow", "hidden") => Some("overflow-hidden"),
        ("align-items", "flex-start") => Some("items-start"),
        ("align-items", "center") => Some("items-center"),
        ("align-items", "flex-end") => Some("items-end"),
        ("justify-content", "center") => Some("justify-center"),
        ("justify-content", "flex-end") => Some("justify-end"),
        ("justify-content", "space-between") => Some("justify-between"),
        ("justify-content", "space-around") => Some("justify-around"),
        ("flex", "1 1 0") => Some("flex-1"),
        ("flex-shrink", "0") => Some("shrink-0"),
        ("align-self", "stretch") => Some("self-stretch"),
        ("min-width", "0") => Some("min-w-0"),
        ("min-height", "0") => Some("min-h-0"),
        ("font-style", "italic") => Some("italic"),
        ("white-space", "pre") => Some("whitespace-pre"),
        ("white-space", "pre-wrap") => Some("whitespace-pre-wrap"),
        ("text-align", "center") => Some("text-center"),
        ("text-align", "right") => Some("text-right"),
        ("text-align", "justify") => Some("text-justify"),
        ("transform-origin", "top left") => Some("origin-top-left"),
        _ => None,
    };
    if let Some(n) = named {
        return n.to_string();
    }
    let a = v.replace(' ', "_");
    match k {
        "width" => format!("w-[{a}]"),
        "height" => format!("h-[{a}]"),
        "left" => format!("left-[{a}]"),
        "top" => format!("top-[{a}]"),
        "gap" => format!("gap-[{a}]"),
        "padding" => format!("p-[{a}]"),
        "border-radius" => format!("rounded-[{a}]"),
        "opacity" => format!("opacity-[{a}]"),
        "font-size" => format!("text-[{a}]"),
        "font-weight" => format!("font-[{a}]"),
        "letter-spacing" => format!("tracking-[{a}]"),
        "line-height" => format!("leading-[{a}]"),
        "color" => format!("text-[{a}]"),
        "background" if v.starts_with('#') => format!("bg-[{a}]"),
        _ => format!("[{k}:{a}]"),
    }
}

fn set_svg_size(svg: &str, w: f32, h: f32) -> String {
    let Some(start) = svg.find("<svg") else { return svg.to_string() };
    let end = start + svg[start..].find('>').unwrap_or(0);
    let mut head = svg[start..end].to_string();
    for attr in ["width", "height"] {
        if let Some(i) = head.find(&format!(" {attr}=\"")) {
            let j = i + attr.len() + 3;
            if let Some(k) = head[j..].find('"') {
                head.replace_range(i..j + k + 1, "");
            }
        }
    }
    head.push_str(&format!(" width=\"{w}\" height=\"{h}\""));
    format!("{}{}{}", &svg[..start], head, &svg[end..])
}

fn relative(from_dir: &Path, to: &Path) -> Option<String> {
    let from: Vec<_> = from_dir.components().collect();
    let to_c: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to_c).take_while(|(a, b)| a == b).count();
    if common == 0 {
        return None;
    }
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), from.len() - common).collect();
    parts.extend(to_c[common..].iter().map(|c| c.as_os_str().to_string_lossy().to_string()));
    Some(parts.join("/"))
}

/// SVG path data for an ellipse arc / ring sector (angles CCW from +x, like the .pen schema).
fn arc_path(w: f32, h: f32, inner: f32, start: f32, sweep: f32) -> String {
    let (cx, cy, rx, ry) = (w / 2.0, h / 2.0, w / 2.0, h / 2.0);
    let (irx, iry) = (rx * inner.clamp(0.0, 1.0), ry * inner.clamp(0.0, 1.0));
    let pt = |rx: f32, ry: f32, a: f32| {
        let r = a.to_radians();
        (cx + rx * r.cos(), cy - ry * r.sin())
    };
    let sweep = sweep.clamp(-360.0, 360.0);
    if sweep.abs() >= 360.0 {
        let ring = |rx: f32, ry: f32| format!("M {} {} a {rx} {ry} 0 1 0 {} 0 a {rx} {ry} 0 1 0 {} 0 Z", cx - rx, cy, 2.0 * rx, -2.0 * rx);
        return format!("{} {}", ring(rx, ry), if inner > 0.0 { ring(irx, iry) } else { String::new() });
    }
    let large = if sweep.abs() > 180.0 { 1 } else { 0 };
    // CCW on screen is SVG's negative sweep direction.
    let sf = if sweep > 0.0 { 0 } else { 1 };
    let (x0, y0) = pt(rx, ry, start);
    let (x1, y1) = pt(rx, ry, start + sweep);
    let mut d = format!("M {x0} {y0} A {rx} {ry} 0 {large} {sf} {x1} {y1} ");
    if inner > 0.0 {
        let (ix1, iy1) = pt(irx, iry, start + sweep);
        let (ix0, iy0) = pt(irx, iry, start);
        d += &format!("L {ix1} {iy1} A {irx} {iry} 0 {large} {} {ix0} {iy0} Z", 1 - sf);
    } else {
        d += &format!("L {cx} {cy} Z");
    }
    d
}
