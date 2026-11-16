//! DOM capture → `.pen` nodes.
//!
//! The capture script reports page coordinates and raw computed styles; this module decides
//! what each element becomes and where it sits. Containers keep absolute positions unless the
//! children form a clean single-axis run, in which case they become flex layouts so the result
//! stays editable. Anything ambiguous falls back to absolute positions, which cannot drift.

use super::css;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    #[serde(default)]
    pub w: f64,
    #[serde(default)]
    pub h: f64,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct Size {
    #[serde(default)]
    pub w: f64,
    #[serde(default)]
    pub h: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DomNode {
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub rect: Rect,
    /// The text's ink box, when it differs from the element box (single-line labels).
    #[serde(default, rename = "textRect")]
    pub text_rect: Option<Rect>,
    /// The widest rendered line, for balanced text.
    #[serde(default, rename = "lineWidth")]
    pub line_width: f64,
    #[serde(default)]
    pub cs: HashMap<String, String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub src: Option<String>,
    #[serde(default)]
    pub href: Option<String>,
    #[serde(default)]
    pub svg: Option<String>,
    #[serde(default)]
    pub kids: Vec<DomNode>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Page {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub size: Size,
    /// The page canvas colour (body's background when it propagates to the viewport).
    #[serde(default)]
    pub canvas: Option<String>,
    pub root: DomNode,
}

pub struct Imported {
    pub node: Value,
    pub nodes: usize,
    pub images: usize,
    pub warnings: Vec<String>,
}

impl DomNode {
    fn cs(&self, key: &str) -> &str {
        self.cs.get(key).map(String::as_str).unwrap_or("")
    }

    fn px(&self, key: &str) -> Option<f32> {
        css::px(self.cs(key))
    }

    fn is_flex(&self) -> bool {
        self.cs("display").contains("flex")
    }
}

/// Builds the top-level frame. `name` overrides the page title.
pub fn convert(page: &Page, name: Option<&str>, resolve_image: &mut dyn FnMut(&str) -> Option<String>) -> Imported {
    let mut cx = Ctx { resolve_image, images: 0, nodes: 0, warnings: Vec::new() };
    let mut root = element(&page.root, None, true, &mut cx).unwrap_or_else(|| json!({
        "type": "frame",
        "layout": "none",
    }));
    if let Some(o) = root.as_object_mut() {
        if o.get("fill").is_none() {
            if let Some(c) = page.canvas.as_deref().and_then(css::color) {
                o.insert("fill".into(), json!(c));
            }
        }
        o.insert("name".into(), json!(name.filter(|n| !n.is_empty()).unwrap_or(&page.title).chars().take(80).collect::<String>()));
        let w = if page.size.w > 0.0 { page.size.w } else { page.root.rect.w };
        let h = if page.size.h > 0.0 { page.size.h } else { page.root.rect.h };
        o.insert("width".into(), json!(round(w)));
        o.insert("height".into(), json!(round(h)));
        o.insert("layout".into(), json!("none"));
        o.insert("x".into(), json!(0));
        o.insert("y".into(), json!(0));
    }
    if page.title.is_empty() && page.url.is_empty() {
        cx.warnings.push("the page reported no title".into());
    }
    Imported { node: root, nodes: cx.nodes, images: cx.images, warnings: cx.warnings }
}

struct Ctx<'a> {
    resolve_image: &'a mut dyn FnMut(&str) -> Option<String>,
    images: usize,
    nodes: usize,
    warnings: Vec<String>,
}

fn element(n: &DomNode, parent: Option<&DomNode>, is_root: bool, cx: &mut Ctx) -> Option<Value> {
    if n.tag == "#text" {
        return n.text.as_deref().map(|t| text_node(n, t));
    }
    if let Some(markup) = &n.svg {
        let (w, h) = (n.rect.w.max(1.0), n.rect.h.max(1.0));
        match crate::svg::to_nodes(markup, w as f32, h as f32) {
            Ok(paths) => {
                cx.nodes += paths.len() + 1;
                return Some(json!({
                    "type": "frame",
                    "name": clean(&n.name, "Icon"),
                    "layout": "none",
                    "width": round(w),
                    "height": round(h),
                    "clip": true,
                    "children": paths,
                }));
            }
            Err(e) => {
                cx.warnings.push(format!("inline SVG in '{}' was skipped: {e}", clean(&n.name, "svg")));
                return None;
            }
        }
    }
    if let Some(src) = &n.src {
        return image_box(n, src, cx);
    }
    if let Some(text) = &n.text {
        if n.kids.is_empty() && !has_box(n) {
            return Some(text_node(n, text));
        }
    }
    if n.tag == "hr" {
        cx.nodes += 1;
        return Some(json!({
            "type": "rectangle",
            "name": clean(&n.name, "Rule"),
            "layout": "none",
            "width": round(n.rect.w.max(1.0)),
            "height": round(n.rect.h.max(1.0)),
            "fill": css::color(n.cs("borderTopColor")).unwrap_or_else(|| "#00000026".into()),
        }));
    }
    frame(n, is_root, cx)
}

/// Text inside an element is centred when the element is a flex box with `align-items: center`
/// and is taller than one line; the caller uses it to keep the same vertical placement.
fn text_node(n: &DomNode, raw: &str) -> Value {
    let text = transform(raw, n.cs("textTransform"));
    let font_size = n.px("fontSize").unwrap_or(16.0).max(1.0);
    let line = css::line_height(n.cs("lineHeight"), font_size).unwrap_or(1.2);
    // One line = the ink is one line high. Such a label is positioned by its ink box, so it
    // keeps the browser's centring even inside a wider box (a fixed-width box would depend on
    // alignment, which pen applies per paragraph rather than to a single hug line).
    let ink_h = n.text_rect.map(|r| r.h).unwrap_or(n.rect.h);
    let single = n.cs("whiteSpace").contains("nowrap") || ink_h <= (font_size * line * 1.4 + 1.0) as f64;
    let mut o = Map::new();
    o.insert("type".into(), json!("text"));
    o.insert("name".into(), json!(clean(&n.name, &text.chars().take(24).collect::<String>())));
    o.insert("content".into(), json!(text));
    o.insert("fontFamily".into(), json!(css::font_family(n.cs("fontFamily")).unwrap_or_else(|| "Inter".into())));
    o.insert("fontSize".into(), json!(round(font_size as f64)));
    if let Some(w) = css::font_weight(n.cs("fontWeight")) {
        o.insert("fontWeight".into(), json!(w));
    }
    if n.cs("fontStyle").contains("italic") {
        o.insert("fontStyle".into(), json!("italic"));
    }
    o.insert("lineHeight".into(), json!(css::round2(line)));
    if let Some(ls) = css::letter_spacing(n.cs("letterSpacing")) {
        o.insert("letterSpacing".into(), json!(css::round2(ls)));
    }
    let rtl = n.cs("direction") == "rtl";
    let align = match n.cs("textAlign") {
        "center" => "center",
        "left" => "left",
        "right" => "right",
        "justify" => "justify",
        "end" => if rtl { "left" } else { "right" },
        _ => if rtl { "right" } else { "left" },
    };
    o.insert("textAlign".into(), json!(align));
    let mut fill = css::color(n.cs("color"));
    if fill.is_none() {
        // `background-clip: text` sets the colour transparent and paints a gradient through the
        // glyphs; pen has no such mode, so the gradient's first stop becomes the text colour.
        if let Some(g) = css::gradient(n.cs("backgroundImage")) {
            fill = g["colors"][0]["color"].as_str().map(str::to_string);
        }
    }
    if let Some(fill) = fill {
        o.insert("fill".into(), json!(fill));
    }
    let deco = n.cs("textDecorationLine");
    if deco.contains("underline") {
        o.insert("underline".into(), json!(true));
    }
    if deco.contains("line-through") {
        o.insert("strikethrough".into(), json!(true));
    }
    if let Some(href) = &n.href {
        o.insert("href".into(), json!(href));
    }
    // Browsers centre a line's ink inside its line box (half-leading), and flex boxes centre
    // it in the element. A fixed box of the same height with the text vertically centred
    // reproduces that in pen.
    let ink = n.text_rect.unwrap_or(n.rect);
    if single {
        o.insert("width".into(), json!(round((ink.w * 1.02 + 4.0).max(4.0))));
        o.insert("height".into(), json!(round(n.rect.h.max((font_size * line) as f64))));
        o.insert("textAlign".into(), json!("center"));
        o.insert("x".into(), json!(round(ink.x - n.rect.x - 2.0)));
    } else {
        let balanced = n.cs("textWrap").contains("balance") || n.cs("textWrap").contains("pretty");
        if balanced && n.line_width > 0.0 {
            // A little slack: study's Arabic/Latin advances can be a hair wider than Blink's,
            // and one px short would push the last word onto a new line.
            o.insert("width".into(), json!(round(n.line_width * 1.03 + 4.0)));
            o.insert("x".into(), json!(round(ink.x - n.rect.x)));
        } else {
            o.insert("width".into(), json!(round(n.rect.w.max(1.0))));
            o.insert("x".into(), json!(0));
        }
        o.insert("height".into(), json!(round(n.rect.h.max((font_size * line) as f64))));
        o.insert("textAlign".into(), json!(align));
    }
    o.insert("textGrowth".into(), json!("fixed-width-height"));
    o.insert("textAlignVertical".into(), json!("middle"));
    o.insert("y".into(), json!(0));
    Value::Object(o)
}

fn image_box(n: &DomNode, src: &str, cx: &mut Ctx) -> Option<Value> {
    let mode = match n.cs("objectFit") {
        "contain" => "fit",
        "fill" => "stretch",
        _ => "fill",
    };
    let mut o = Map::new();
    o.insert("type".into(), json!("rectangle"));
    o.insert("name".into(), json!(clean(&n.name, "Image")));
    o.insert("width".into(), json!(round(n.rect.w.max(1.0))));
    o.insert("height".into(), json!(round(n.rect.h.max(1.0))));
    if let Some(radius) = css::radius(n.cs("borderRadius"), n.rect.w as f32, n.rect.h as f32) {
        o.insert("cornerRadius".into(), radius);
    }
    match (cx.resolve_image)(src) {
        Some(url) => {
            cx.images += 1;
            o.insert("fill".into(), json!({"type": "image", "url": url, "mode": mode}));
        }
        None => {
            cx.warnings.push(format!("image could not be fetched: {src}"));
            o.insert("fill".into(), json!("#E5E7EB"));
        }
    }
    Some(Value::Object(o))
}

fn frame(n: &DomNode, is_root: bool, cx: &mut Ctx) -> Option<Value> {
    let mut o = Map::new();
    o.insert("type".into(), json!("frame"));
    o.insert("name".into(), json!(clean(&n.name, &n.tag)));
    o.insert("width".into(), json!(round(n.rect.w.max(1.0))));
    o.insert("height".into(), json!(round(n.rect.h.max(1.0))));

    // Background: colour first, then the image or gradient painted over it.
    let mut fills = Vec::new();
    if let Some(c) = css::color(n.cs("backgroundColor")) {
        fills.push(json!(c));
    }
    let bg = n.cs("backgroundImage");
    if bg != "none" && !bg.is_empty() {
        if let Some(g) = css::gradient(bg) {
            fills.push(g);
        } else if let Some(url) = css::first_url(bg) {
            let mode = match n.cs("backgroundSize") {
                "contain" => "fit",
                "100% 100%" => "stretch",
                _ => "fill",
            };
            match (cx.resolve_image)(&url) {
                Some(path) => {
                    cx.images += 1;
                    fills.push(json!({"type": "image", "url": path, "mode": mode}));
                }
                None => cx.warnings.push(format!("background image could not be fetched: {url}")),
            }
        }
    }
    match fills.len() {
        0 => {}
        1 => {
            o.insert("fill".into(), fills.pop().unwrap());
        }
        _ => {
            // pen's `fill` takes one paint or an array, bottom to top.
            o.insert("fill".into(), Value::Array(fills));
        }
    }

    if let Some(radius) = css::radius(n.cs("borderRadius"), n.rect.w as f32, n.rect.h as f32) {
        o.insert("cornerRadius".into(), radius);
    }
    let sides = ["borderTopWidth", "borderRightWidth", "borderBottomWidth", "borderLeftWidth"];
    let widths: Vec<f32> = sides.iter().map(|k| n.px(k).unwrap_or(0.0)).collect();
    if widths.iter().any(|w| *w > 0.0) {
        let color = css::color(n.cs("borderTopColor")).unwrap_or_else(|| "#000000".into());
        o.insert("stroke".into(), json!(color));
        if widths.iter().all(|w| (*w - widths[0]).abs() < 0.01) {
            o.insert("strokeWidth".into(), json!(css::round2(widths[0])));
        } else {
            o.insert(
                "strokeWidth".into(),
                json!({"top": css::round2(widths[0]), "right": css::round2(widths[1]), "bottom": css::round2(widths[2]), "left": css::round2(widths[3])}),
            );
        }
        o.insert("strokeAlignment".into(), json!("inner"));
    }
    let shadows = css::shadows(n.cs("boxShadow"));
    if !shadows.is_empty() {
        o.insert("effect".into(), if shadows.len() == 1 { shadows[0].clone() } else { Value::Array(shadows) });
    }
    if let Some(op) = n.px("opacity") {
        if op < 0.999 {
            o.insert("opacity".into(), json!(css::round2(op)));
        }
    }
    if matches!(n.cs("overflow"), "hidden" | "clip" | "auto" | "scroll") {
        o.insert("clip".into(), json!(true));
    }

    // Layout: a clean single-axis run becomes flex; anything else keeps absolute positions.
    let layout = if !is_root { flex(n) } else { None };
    match &layout {
        Some(f) => {
            o.insert("layout".into(), json!(f.direction));
            o.insert("gap".into(), json!(css::round2(f.gap)));
            o.insert("padding".into(), json!(f.padding.map(css::round2)));
            o.insert("justifyContent".into(), json!(f.justify));
            o.insert("alignItems".into(), json!(f.align));
        }
        None => {
            o.insert("layout".into(), json!("none"));
        }
    }

    // A box that carries text of its own keeps the text as a child, so the background,
    // border and radius survive the import.
    let mut kids: Vec<(Option<&DomNode>, Value)> = Vec::new();
    if let Some(text) = &n.text {
        if !text.trim().is_empty() {
            kids.push((None, text_node(n, text)));
        }
    }
    for kid in &n.kids {
        if let Some(v) = element(kid, Some(n), false, cx) {
            kids.push((Some(kid), v));
        }
    }
    if layout.is_some() {
        // A flex parent positions its children; x/y on them would only warn.
        for (_, kid) in kids.iter_mut() {
            if let Some(o) = kid.as_object_mut() {
                o.remove("x");
                o.remove("y");
            }
        }
    }
    if layout.is_none() {
        // Absolute children sit where the browser put them, relative to this frame.
        for (src, kid) in kids.iter_mut() {
            // Text carries its own inset from text_node; everything else sits at its rect.
            let (ix, iy) = (kid.get("x").and_then(Value::as_f64).unwrap_or(0.0), kid.get("y").and_then(Value::as_f64).unwrap_or(0.0));
            let (x, y) = match src {
                Some(d) => (d.rect.x - n.rect.x + ix, d.rect.y - n.rect.y + iy),
                None => (ix, iy),
            };
            if let Some(o) = kid.as_object_mut() {
                o.insert("x".into(), json!(round(x)));
                o.insert("y".into(), json!(round(y)));
            }
        }
    }
    if !kids.is_empty() {
        o.insert("children".into(), Value::Array(kids.into_iter().map(|(_, v)| v).collect()));
    }
    cx.nodes += 1;
    Some(Value::Object(o))
}

struct FlexLayout {
    direction: &'static str,
    gap: f32,
    padding: [f32; 4],
    justify: &'static str,
    align: &'static str,
}

/// Flex only when the children form one clean run: ordered, non-overlapping on the main axis,
/// equal gaps, and the same start on the cross axis. Otherwise the caller keeps absolute
/// positions, which reproduce the page exactly.
fn flex(n: &DomNode) -> Option<FlexLayout> {
    if !n.is_flex() || n.kids.len() < 2 {
        return None;
    }
    let dir = n.cs("flexDirection");
    let horizontal = match dir {
        "row" | "row-reverse" => dir == "row",
        "column" | "column-reverse" => dir != "column",
        _ => return None,
    };
    let kids: Vec<&DomNode> = n.kids.iter().filter(|k| k.tag != "#text" || k.text.is_some()).collect();
    if kids.len() < 2 {
        return None;
    }
    let (ps, pe) = if horizontal { (n.rect.x, n.rect.x + n.rect.w) } else { (n.rect.y, n.rect.y + n.rect.h) };
    let (cs_s, cs_e) = if horizontal { (n.rect.y, n.rect.y + n.rect.h) } else { (n.rect.x, n.rect.x + n.rect.w) };
    let principal = |r: Rect| {
        if horizontal {
            (r.x, r.x + r.w, r.y, r.y + r.h)
        } else {
            (r.y, r.y + r.h, r.x, r.x + r.w)
        }
    };

    let mut gaps = Vec::new();
    let mut prev: Option<f64> = None;
    let mut cross_min = f64::INFINITY;
    let mut cross_max = f64::NEG_INFINITY;
    for k in &kids {
        let (s, e, c0, c1) = principal(k.rect);
        if let Some(p) = prev {
            if s < p - 0.75 {
                return None;
            }
            gaps.push(s - p);
        }
        prev = Some(e);
        cross_min = cross_min.min(c0);
        cross_max = cross_max.max(c1);
    }
    if cross_max - cross_min > (cs_e - cs_s) + 1.0 {
        return None;
    }
    let gap = if gaps.is_empty() { 0.0 } else { gaps.iter().sum::<f64>() / gaps.len() as f64 };
    if gaps.iter().any(|g| (g - gap).abs() > 1.0) {
        return None;
    }
    let first = &kids[0];
    let last = &kids[kids.len() - 1];
    let (fs, _, _, _) = principal(first.rect);
    let (_, le, _, _) = principal(last.rect);
    let mut pad_start = fs - ps;
    let mut pad_end = pe - le;
    let mut pad_cross_start = cross_min - cs_s;
    let mut pad_cross_end = cs_e - cross_max;
    if pad_start < -1.0 || pad_end < -1.0 || pad_cross_start < -1.0 || pad_cross_end < -1.0 {
        return None;
    }
    // Text boxes report ink bounds, not layout bounds, so a small negative cross padding is
    // normal; clamp rather than give up.
    let clamp0 = |v: f64| v.max(0.0) as f32;
    pad_start = pad_start.max(0.0);
    pad_end = pad_end.max(0.0);
    pad_cross_start = pad_cross_start.max(0.0);
    pad_cross_end = pad_cross_end.max(0.0);
    let justify = match n.cs("justifyContent") {
        "space-between" if gap > 0.0 => "space_between",
        "space-around" if gap > 0.0 => "space_around",
        "center" => "center",
        "flex-end" | "end" => "end",
        _ => "start",
    };
    let align = match n.cs("alignItems") {
        "center" => "center",
        "flex-end" | "end" => "end",
        _ => "start",
    };
    let padding = if horizontal {
        [clamp0(pad_cross_start), clamp0(pad_end), clamp0(pad_cross_end), clamp0(pad_start)]
    } else {
        [clamp0(pad_start), clamp0(pad_cross_end), clamp0(pad_end), clamp0(pad_cross_start)]
    };
    Some(FlexLayout { direction: if horizontal { "horizontal" } else { "vertical" }, gap: gap as f32, padding, justify, align })
}

fn transform(s: &str, how: &str) -> String {
    match how {
        "uppercase" => s.to_uppercase(),
        "lowercase" => s.to_lowercase(),
        "capitalize" => s.split(' ').map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }).collect::<Vec<_>>().join(" "),
        _ => s.to_string(),
    }
}

/// Whether the element paints something of its own (and so must stay a frame).
fn has_box(n: &DomNode) -> bool {
    css::color(n.cs("backgroundColor")).is_some()
        || !matches!(n.cs("backgroundImage"), "" | "none")
        || !matches!(n.cs("boxShadow"), "" | "none")
        || !matches!(n.cs("borderRadius"), "" | "0px" | "0")
        || ["borderTopWidth", "borderRightWidth", "borderBottomWidth", "borderLeftWidth"]
            .iter()
            .any(|k| n.px(k).unwrap_or(0.0) > 0.0)
}

fn clean(name: &Option<String>, fallback: &str) -> String {
    let s = name.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(fallback);
    s.chars().take(80).collect()
}

fn round(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: Value) -> Page {
        serde_json::from_value(v).expect("page json")
    }

    fn no_images(_: &str) -> Option<String> {
        None
    }

    fn boxed(x: f64, y: f64, w: f64, h: f64, cs: Value, kids: Value) -> Value {
        json!({"tag": "div", "name": "Box", "rect": {"x": x, "y": y, "w": w, "h": h}, "cs": cs, "kids": kids})
    }

    fn names(v: &Value) -> Vec<String> {
        v["children"].as_array().map(|a| a.iter().map(|c| c["name"].as_str().unwrap_or("").to_string()).collect()).unwrap_or_default()
    }

    #[test]
    fn absolute_children_keep_their_positions() {
        let page = parse(json!({
            "title": "Page", "size": {"w": 800.0, "h": 600.0},
            "root": boxed(0.0, 0.0, 800.0, 600.0, json!({}), json!([
                boxed(10.0, 20.0, 100.0, 50.0, json!({}), json!([])),
            ])),
        }));
        let out = convert(&page, None, &mut no_images);
        assert_eq!(out.node["name"], "Page");
        assert_eq!(out.node["width"], 800.0);
        assert_eq!(out.node["layout"], "none");
        let kid = &out.node["children"][0];
        assert_eq!((kid["x"].as_f64(), kid["y"].as_f64()), (Some(10.0), Some(20.0)));
    }

    #[test]
    fn clean_rows_become_flex() {
        let page = parse(json!({
            "title": "Row", "size": {"w": 600.0, "h": 300.0},
            "root": boxed(0.0, 0.0, 600.0, 300.0, json!({}), json!([
                boxed(100.0, 50.0, 400.0, 100.0, json!({"display": "flex", "flexDirection": "row"}), json!([
                    boxed(100.0, 50.0, 100.0, 40.0, json!({}), json!([])),
                    boxed(220.0, 50.0, 100.0, 40.0, json!({}), json!([])),
                    boxed(340.0, 50.0, 100.0, 40.0, json!({}), json!([])),
                ])),
            ])),
        }));
        let out = convert(&page, None, &mut no_images);
        let row = &out.node["children"][0];
        assert_eq!(row["layout"], "horizontal");
        assert_eq!(row["gap"], 20.0);
        assert_eq!(row["padding"], json!([0.0, 60.0, 60.0, 0.0]));
        assert_eq!(row["justifyContent"], "start");
        let kid = &row["children"][0];
        assert!(kid.get("x").is_none(), "flex children carry no x");
    }

    #[test]
    fn uneven_gaps_fall_back_to_absolute() {
        let page = parse(json!({
            "title": "Row", "size": {"w": 600.0, "h": 300.0},
            "root": boxed(0.0, 0.0, 600.0, 300.0, json!({}), json!([
                boxed(0.0, 0.0, 600.0, 100.0, json!({"display": "flex", "flexDirection": "row"}), json!([
                    boxed(0.0, 0.0, 100.0, 40.0, json!({}), json!([])),
                    boxed(120.0, 0.0, 100.0, 40.0, json!({}), json!([])),
                    boxed(250.0, 0.0, 100.0, 40.0, json!({}), json!([])),
                ])),
            ])),
        }));
        let out = convert(&page, None, &mut no_images);
        assert_eq!(out.node["children"][0]["layout"], "none");
    }

    #[test]
    fn text_blocks_wrap_at_the_measured_width() {
        let page = parse(json!({
            "title": "Text", "size": {"w": 400.0, "h": 200.0},
            "root": boxed(0.0, 0.0, 400.0, 200.0, json!({}), json!([
                {"tag": "p", "name": "Body", "rect": {"x": 10.0, "y": 10.0, "w": 300.0, "h": 48.0},
                 "cs": {"fontSize": "16px", "lineHeight": "24px", "color": "rgb(17, 17, 17)", "fontFamily": "Inter, sans-serif", "direction": "rtl"},
                 "text": "some words that wrap"},
            ])),
        }));
        let out = convert(&page, None, &mut no_images);
        let t = &out.node["children"][0];
        assert_eq!(t["type"], "text");
        assert_eq!(t["textGrowth"], "fixed-width-height");
        assert_eq!(t["width"], 300.0);
        assert_eq!(t["textAlign"], "right");
        assert_eq!(t["lineHeight"], 1.5);
        assert_eq!(t["fill"], "#111111");
        assert_eq!(t["fontFamily"], "Inter");
    }

    #[test]
    fn single_words_do_not_wrap() {
        let page = parse(json!({
            "title": "Text", "size": {"w": 400.0, "h": 200.0},
            "root": boxed(0.0, 0.0, 400.0, 200.0, json!({}), json!([
                {"tag": "span", "name": "Word", "rect": {"x": 0.0, "y": 0.0, "w": 80.0, "h": 20.0},
                 "cs": {"fontSize": "14px"}, "text": "قبس"},
            ])),
        }));
        let out = convert(&page, None, &mut no_images);
        let word = &out.node["children"][0];
        assert_eq!(word["textGrowth"], "fixed-width-height");
        assert_eq!(word["textAlign"], "center");
        assert!((word["width"].as_f64().unwrap() - 80.0).abs() < 8.0);
    }

    #[test]
    fn rtl_start_aligns_right() {
        let page = parse(json!({
            "title": "Text", "size": {"w": 400.0, "h": 200.0},
            "root": boxed(0.0, 0.0, 400.0, 200.0, json!({}), json!([
                {"tag": "p", "name": "Arabic", "rect": {"x": 0.0, "y": 0.0, "w": 300.0, "h": 60.0},
                 "cs": {"fontSize": "16px", "lineHeight": "20px", "direction": "rtl", "textAlign": "start"},
                 "text": "مرحبا بالعالم، هذه فقرة من سطرين أو ثلاثة"},
            ])),
        }));
        let out = convert(&page, None, &mut no_images);
        assert_eq!(out.node["children"][0]["textAlign"], "right");
    }

    #[test]
    fn images_map_object_fit() {
        let page = parse(json!({
            "title": "Image", "size": {"w": 200.0, "h": 200.0},
            "root": boxed(0.0, 0.0, 200.0, 200.0, json!({}), json!([
                {"tag": "img", "name": "Photo", "rect": {"x": 0.0, "y": 0.0, "w": 120.0, "h": 80.0},
                 "cs": {"objectFit": "cover"}, "src": "https://example.test/a.jpg"},
            ])),
        }));
        let mut resolve = |_: &str| Some("doc.assets/a.jpg".to_string());
        let out = convert(&page, None, &mut resolve);
        let img = &out.node["children"][0];
        assert_eq!(img["fill"]["mode"], "fill");
        assert_eq!(img["fill"]["url"], "doc.assets/a.jpg");
        assert_eq!(out.images, 1);
    }

    #[test]
    fn background_colour_sits_under_the_gradient() {
        let page = parse(json!({
            "title": "Card", "size": {"w": 400.0, "h": 200.0},
            "root": boxed(0.0, 0.0, 400.0, 200.0, json!({}), json!([
                boxed(0.0, 0.0, 200.0, 100.0, json!({
                    "backgroundColor": "rgba(10, 20, 30, 0.5)",
                    "backgroundImage": "linear-gradient(90deg, #ffffff, #000000)",
                }), json!([])),
            ])),
        }));
        let out = convert(&page, None, &mut no_images);
        let fills = out.node["children"][0]["fill"].as_array().unwrap();
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0], "#0A141E80");
        assert_eq!(fills[1]["rotation"], 90.0);
    }

    #[test]
    fn shadows_become_effects() {
        let page = parse(json!({
            "title": "Card", "size": {"w": 400.0, "h": 200.0},
            "root": boxed(0.0, 0.0, 400.0, 200.0, json!({}), json!([
                boxed(0.0, 0.0, 200.0, 100.0, json!({
                    "boxShadow": "rgba(0, 0, 0, 0.2) 0px 6px 18px 0px",
                    "borderRadius": "12px",
                }), json!([])),
            ])),
        }));
        let out = convert(&page, None, &mut no_images);
        let card = &out.node["children"][0];
        assert_eq!(card["effect"]["blur"], 18.0);
        assert_eq!(card["cornerRadius"], 12.0);
    }
}
