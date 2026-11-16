//! CSS values from `getComputedStyle` as `.pen` style values.
//!
//! Everything here is pure: the capture script hands over the raw computed strings and this
//! module turns them into colours, fills, shadows, radii and text metrics. Keeping the parsing
//! out of JavaScript makes it testable and lets the importer stay a plain data transform.

use serde_json::{json, Value};

/// `rgb()`, `rgba()`, `hsl()`, `#hex` and `transparent` as `#RRGGBB` / `#RRGGBBAA`.
/// Chrome may report wide-gamut colours as `color(srgb r g b / a)`; those convert too.
pub fn color(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("transparent") || s.eq_ignore_ascii_case("none") {
        return None;
    }
    if let Some(hex) = s.strip_prefix('#') {
        let h: Vec<char> = hex.chars().collect();
        let (r, g, b, a) = match h.len() {
            3 => (dup(h[0]), dup(h[1]), dup(h[2]), "FF".to_string()),
            4 => (dup(h[0]), dup(h[1]), dup(h[2]), dup(h[3])),
            6 => (hex[0..2].to_string(), hex[2..4].to_string(), hex[4..6].to_string(), "FF".to_string()),
            8 => (hex[0..2].to_string(), hex[2..4].to_string(), hex[4..6].to_string(), hex[6..8].to_string()),
            _ => return None,
        };
        let (r, g, b, a) = (r.to_uppercase(), g.to_uppercase(), b.to_uppercase(), a.to_uppercase());
        return Some(if a == "FF" { format!("#{r}{g}{b}") } else { format!("#{r}{g}{b}{a}") });
    }
    let lower = s.to_ascii_lowercase();
    let (name, inner) = match lower.find('(') {
        Some(i) if lower.ends_with(')') => (&lower[..i], &s[i + 1..s.len() - 1]),
        _ => (lower.as_str(), ""),
    };
    let parts: Vec<&str> = inner.split(|c: char| c == ',' || c == '/' || c.is_whitespace()).filter(|p| !p.is_empty()).collect();
    match name {
        "rgb" | "rgba" => {
            if parts.len() < 3 {
                return None;
            }
            let r = channel(parts[0])?;
            let g = channel(parts[1])?;
            let b = channel(parts[2])?;
            let a = if parts.len() > 3 { alpha(parts[3])? } else { 255.0 };
            rgba(r, g, b, a)
        }
        "color" => {
            // color(srgb 0.1 0.2 0.3 / 0.4), color(display-p3 ...)
            let space = parts.first()?.to_ascii_lowercase();
            if parts.len() < 4 {
                return None;
            }
            let c = |p: &str| p.parse::<f32>().unwrap_or(0.0);
            let a = if parts.len() > 4 { alpha(parts[4])? } else { 255.0 };
            match space.as_str() {
                "srgb" => rgba(c(parts[1]) * 255.0, c(parts[2]) * 255.0, c(parts[3]) * 255.0, a),
                "display-p3" => {
                    let lin = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
                    let (r, g, b) = (lin(c(parts[1])), lin(c(parts[2])), lin(c(parts[3])));
                    let x = 1.2249 * r - 0.2247 * g;
                    let y = -0.0420 * r + 1.0419 * g;
                    let z = -0.0197 * r - 0.0786 * g + 1.0983 * b;
                    let gam = |v: f32| if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 };
                    rgba(gam(x) * 255.0, gam(y) * 255.0, gam(z) * 255.0, a)
                }
                _ => None,
            }
        }
        "oklch" | "oklab" => {
            if parts.len() < 3 {
                return None;
            }
            let l = if let Some(p) = parts[0].strip_suffix('%') { p.parse::<f32>().ok()? / 100.0 } else { parts[0].parse::<f32>().ok()? };
            let (a, b) = if name == "oklch" {
                let c = if let Some(p) = parts[1].strip_suffix('%') { p.parse::<f32>().ok()? / 100.0 * 0.4 } else { parts[1].parse::<f32>().ok()? };
                let h = parts[2].trim_end_matches("deg").parse::<f32>().ok()?.to_radians();
                (c * h.cos(), c * h.sin())
            } else {
                (parts[1].parse::<f32>().ok()?, parts[2].parse::<f32>().ok()?)
            };
            let a_alpha = if parts.len() > 3 { alpha(parts[3])? } else { 255.0 };
            let (r, g, bl) = oklab_to_srgb(l, a, b);
            rgba(r, g, bl, a_alpha)
        }
        "lab" | "lch" => {
            if parts.len() < 3 {
                return None;
            }
            let l = parts[0].parse::<f32>().ok()?;
            let (a, b) = if name == "lch" {
                let c = parts[1].parse::<f32>().ok()?;
                let h = parts[2].trim_end_matches("deg").parse::<f32>().ok()?.to_radians();
                (c * h.cos(), c * h.sin())
            } else {
                (parts[1].parse::<f32>().ok()?, parts[2].parse::<f32>().ok()?)
            };
            let a_alpha = if parts.len() > 3 { alpha(parts[3])? } else { 255.0 };
            let (r, g, bl) = lab_to_srgb(l, a, b);
            rgba(r, g, bl, a_alpha)
        }
        "hsl" | "hsla" => {
            if parts.len() < 3 {
                return None;
            }
            let h = parts[0].trim_end_matches("deg").parse::<f32>().ok()?;
            let s = parts[1].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
            let l = parts[2].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
            let a = if parts.len() > 3 { alpha(parts[3])? } else { 255.0 };
            let (r, g, b) = hsl_to_rgb(h, s, l);
            rgba(r, g, b, a)
        }
        _ => None,
    }
}

fn dup(c: char) -> String {
    format!("{c}{c}")
}

fn channel(p: &str) -> Option<f32> {
    if let Some(pct) = p.strip_suffix('%') {
        pct.parse::<f32>().ok().map(|v| v * 2.55)
    } else {
        p.parse::<f32>().ok()
    }
}

fn alpha(p: &str) -> Option<f32> {
    if let Some(pct) = p.strip_suffix('%') {
        pct.parse::<f32>().ok().map(|v| v * 2.55)
    } else {
        p.parse::<f32>().ok().map(|v| v * 255.0)
    }
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> Option<String> {
    let c = |v: f32| v.clamp(0.0, 255.0).round() as u8;
    let a = a.clamp(0.0, 255.0).round() as u8;
    if a == 0 {
        return None;
    }
    Some(if a == 255 {
        format!("#{:02X}{:02X}{:02X}", c(r), c(g), c(b))
    } else {
        format!("#{:02X}{:02X}{:02X}{:02X}", c(r), c(g), c(b), a)
    })
}

/// OKLab (as produced by `oklch()` and `oklab()`) to gamma-encoded sRGB in 0..255.
fn oklab_to_srgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let bl = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;
    let gam = |v: f32| {
        let v = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 };
        v.clamp(0.0, 1.0) * 255.0
    };
    (gam(r), gam(g), gam(bl))
}

/// CIE Lab (D50, as CSS `lab()` uses) to gamma-encoded sRGB.
fn lab_to_srgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let inv = |t: f32| {
        let t3 = t * t * t;
        if t3 > 0.008_856 { t3 } else { (t - 16.0 / 116.0) / 7.787 }
    };
    let (x, y, z) = (0.964_22 * inv(fx), inv(fy), 0.825_21 * inv(fz));
    // D50 → D65 (Bradford), then XYZ → linear sRGB.
    let x65 = 0.955_576_6 * x - 0.023_039_3 * y + 0.063_163_6 * z;
    let y65 = -0.028_289_5 * x + 1.009_941_6 * y + 0.021_007_7 * z;
    let z65 = 0.012_298_2 * x - 0.020_483 * y + 1.329_909_8 * z;
    let r = 3.240_454_2 * x65 - 1.537_138_5 * y65 - 0.498_531_4 * z65;
    let g = -0.969_266 * x65 + 1.876_010_8 * y65 + 0.041_556 * z65;
    let bl = 0.055_643_4 * x65 - 0.204_025_9 * y65 + 1.057_225_2 * z65;
    let gam = |v: f32| {
        let v = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 };
        v.clamp(0.0, 1.0) * 255.0
    };
    (gam(r), gam(g), gam(bl))
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = h.rem_euclid(360.0) / 360.0;
    if s == 0.0 {
        let v = l * 255.0;
        return (v, v, v);
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |t: f32| {
        let t = t.rem_euclid(1.0);
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        v * 255.0
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

/// A length in px: `12px`, `12`, `0`. Percentages return `None` (the caller decides against what).
pub fn px(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.is_empty() || s == "normal" || s == "auto" || s == "none" {
        return None;
    }
    let n = s.strip_suffix("px").unwrap_or(s).trim();
    n.parse::<f32>().ok()
}

/// The first usable family from a CSS `font-family` list, or `None` when only generics remain.
pub fn font_family(css: &str) -> Option<String> {
    const GENERIC: &[&str] = &[
        "system-ui", "-apple-system", "blinkmacsystemfont", "segoe ui", "helvetica neue", "arial", "sans-serif", "serif",
        "monospace", "cursive", "fantasy", "ui-sans-serif", "ui-serif", "ui-monospace", "ui-rounded", "inherit", "initial",
        "unset", "revert",
    ];
    for fam in css.split(',') {
        let fam = fam.trim().trim_matches(|c| c == '"' || c == '\'').trim();
        if fam.is_empty() || fam.starts_with("var(") {
            continue;
        }
        if !GENERIC.contains(&fam.to_ascii_lowercase().as_str()) {
            return Some(fam.to_string());
        }
    }
    None
}

pub fn font_weight(css: &str) -> Option<String> {
    let s = css.trim().to_ascii_lowercase();
    match s.as_str() {
        "" | "normal" | "400" => None,
        "bold" => Some("700".into()),
        _ => s.parse::<u16>().ok().map(|n| n.to_string()),
    }
}

/// `line-height: 24px` at 16px → `1.5`. Unitless values pass through; `normal` returns `None`.
pub fn line_height(css: &str, font_size: f32) -> Option<f32> {
    let s = css.trim();
    if s.is_empty() || s == "normal" || font_size <= 0.0 {
        return None;
    }
    if let Some(n) = px(s) {
        return Some(n / font_size);
    }
    s.parse::<f32>().ok().filter(|v| *v > 0.0)
}

pub fn letter_spacing(css: &str) -> Option<f32> {
    if css.trim() == "normal" {
        return None;
    }
    px(css).filter(|v| *v != 0.0)
}

/// `border-radius: 8px` / `8px 8px 0 0` / `50%` as a pen radius (number or [tl, tr, br, bl]).
pub fn radius(css: &str, w: f32, h: f32) -> Option<Value> {
    let s = css.trim();
    if s.is_empty() || s == "0px" || s == "0" {
        return None;
    }
    // Each corner may be an ellipse ("10px / 20px"); the first value is the one that shows.
    let flat = s.split('/').next().unwrap_or(s);
    let vals: Vec<f32> = flat
        .split_whitespace()
        .filter_map(|v| {
            if let Some(p) = v.strip_suffix('%') {
                p.parse::<f32>().ok().map(|n| n / 100.0 * w.min(h))
            } else {
                px(v)
            }
        })
        .collect();
    if vals.is_empty() || vals.iter().all(|v| *v <= 0.0) {
        return None;
    }
    let corners = match vals.len() {
        1 => return Some(json!(round2(vals[0]))),
        2 => vec![vals[0], vals[1], vals[0], vals[1]],
        3 => vec![vals[0], vals[1], vals[2], vals[1]],
        _ => vec![vals[0], vals[1], vals[2], vals[3]],
    };
    Some(json!(corners.iter().map(|v| round2(*v)).collect::<Vec<_>>()))
}

/// Splits a comma-separated CSS value list, ignoring commas inside parentheses.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// A CSS `linear-gradient()` or `radial-gradient()` as a pen gradient fill.
///
/// CSS measures the angle clockwise from "to top"; pen measures counter-clockwise from
/// "position 0 at the top", so `180deg` (top → bottom) is pen 0 and `90deg` is pen 90.
pub fn gradient(css: &str) -> Option<Value> {
    let lower = css.trim().to_ascii_lowercase();
    let (kind, start) = if lower.starts_with("linear-gradient(") {
        ("linear", "linear-gradient(".len())
    } else if lower.starts_with("radial-gradient(") {
        ("radial", "radial-gradient(".len())
    } else {
        return None;
    };
    let inner = css.trim().get(start..)?.strip_suffix(')')?;
    let mut args = split_args(inner);
    if args.is_empty() {
        return None;
    }
    let mut angle = 180.0f32;
    let mut center = (0.5f32, 0.5f32);
    let first = args[0].to_ascii_lowercase();
    let looks_like_stop = color(&args[0]).is_some();
    if !looks_like_stop {
        if kind == "linear" {
            angle = angle_of(&first)?;
        } else if let Some(at) = first.find(" at ") {
            center = position(&first[at + 4..])?;
        }
        args.remove(0);
    }
    let parsed: Vec<(String, Option<f32>)> = args.iter().filter_map(|arg| split_stop(arg)).collect();
    let stops = resolve_stops(parsed);
    if stops.len() < 2 {
        return None;
    }
    if kind == "linear" {
        Some(json!({
            "type": "gradient",
            "gradientType": "linear",
            "rotation": round2((180.0 - angle).rem_euclid(360.0)),
            "colors": stops,
        }))
    } else {
        Some(json!({
            "type": "gradient",
            "gradientType": "radial",
            "center": {"x": round2(center.0), "y": round2(center.1)},
            "colors": stops,
        }))
    }
}

fn split_stop(arg: &str) -> Option<(String, Option<f32>)> {
    let mut depth = 0i32;
    let mut split = arg.len();
    for (i, c) in arg.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ' ' if depth == 0 => {
                // The colour is the first token; a position, if any, follows it.
                let rest = arg[i..].trim();
                if rest.is_empty() {
                    continue;
                }
                split = i;
                break;
            }
            _ => {}
        }
    }
    let c = color(arg[..split].trim())?;
    let rest = arg[split..].trim();
    let pos = if rest.is_empty() {
        None
    } else if let Some(p) = rest.strip_suffix('%') {
        p.parse::<f32>().ok().map(|v| v / 100.0)
    } else {
        px(rest)
    };
    Some((c, pos))
}

fn resolve_stops(mut stops: Vec<(String, Option<f32>)>) -> Vec<Value> {
    if stops.is_empty() {
        return Vec::new();
    }
    if stops.len() == 1 {
        stops.push(stops[0].clone());
    }
    let n = stops.len();
    if stops[0].1.is_none() {
        stops[0].1 = Some(0.0);
    }
    if stops[n - 1].1.is_none() {
        stops[n - 1].1 = Some(1.0);
    }
    let mut i = 0;
    while i < n {
        if stops[i].1.is_some() {
            i += 1;
            continue;
        }
        let start = i - 1;
        let mut end = i;
        while end < n && stops[end].1.is_none() {
            end += 1;
        }
        let (a, b) = (stops[start].1.unwrap(), stops[end].1.unwrap_or(1.0));
        let steps = (end - start) as f32;
        for k in start + 1..end {
            stops[k].1 = Some(a + (b - a) * ((k - start) as f32 / steps));
        }
        i = end + 1;
    }
    stops
        .into_iter()
        .map(|(c, p)| json!({"color": c, "position": round2(p.unwrap_or(0.0).clamp(0.0, 1.0))}))
        .collect()
}

fn angle_of(s: &str) -> Option<f32> {
    let s = s.trim();
    if let Some(dir) = s.strip_prefix("to ") {
        return Some(match dir.trim() {
            "top" => 0.0,
            "right" => 90.0,
            "bottom" => 180.0,
            "left" => 270.0,
            "top right" | "right top" => 45.0,
            "bottom right" | "right bottom" => 135.0,
            "bottom left" | "left bottom" => 225.0,
            "top left" | "left top" => 315.0,
            _ => 180.0,
        });
    }
    if let Some(deg) = s.strip_suffix("deg") {
        return deg.parse::<f32>().ok();
    }
    if let Some(turn) = s.strip_suffix("turn") {
        return turn.parse::<f32>().ok().map(|v| v * 360.0);
    }
    if let Some(rad) = s.strip_suffix("rad") {
        return rad.parse::<f32>().ok().map(|v| v.to_degrees());
    }
    None
}

fn position(s: &str) -> Option<(f32, f32)> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    let one = |p: &str| -> f32 {
        if let Some(pct) = p.strip_suffix('%') {
            pct.parse::<f32>().unwrap_or(50.0) / 100.0
        } else {
            match p {
                "left" | "top" => 0.0,
                "right" | "bottom" => 1.0,
                _ => 0.5,
            }
        }
    };
    Some(match parts.len() {
        0 => (0.5, 0.5),
        1 => (one(parts[0]), 0.5),
        _ => (one(parts[0]), one(parts[1])),
    })
}

/// Box shadows as pen `shadow` effects. Inset shadows map to `shadowType: "inner"`.
pub fn shadows(css: &str) -> Vec<Value> {
    if css.trim() == "none" {
        return Vec::new();
    }
    split_args(css)
        .into_iter()
        .filter_map(|one| {
            let mut inner = false;
            let mut c = None;
            let mut nums = Vec::new();
            // A colour may be a function with spaces in it (`rgb(0 0 0 / 10%)`, `lab(...)`), so
            // tokens are split at top-level whitespace only.
            for token in top_tokens(&one) {
                match token {
                    "inset" => {
                        inner = true;
                        continue;
                    }
                    _ => {}
                }
                if let Some(col) = color(token) {
                    c = Some(col);
                } else if let Some(v) = px(token) {
                    nums.push(v);
                }
            }
            if nums.len() < 2 {
                return None;
            }
            let color = c.unwrap_or_else(|| "#00000026".into());
            Some(json!({
                "type": "shadow",
                "shadowType": if inner { "inner" } else { "outer" },
                "offset": {"x": round2(nums[0]), "y": round2(nums[1])},
                "blur": round2(*nums.get(2).unwrap_or(&0.0)),
                "spread": round2(*nums.get(3).unwrap_or(&0.0)),
                "color": color,
            }))
        })
        .collect()
}

/// Whitespace-separated tokens that keep parenthesised spans together.
fn top_tokens(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        match c {
            '(' => {
                depth += 1;
                start.get_or_insert(i);
            }
            ')' => {
                depth -= 1;
                if depth < 0 {
                    depth = 0;
                }
                start.get_or_insert(i);
            }
            c if c.is_whitespace() && depth == 0 => {
                if let Some(st) = start.take() {
                    out.push(s[st..i].trim());
                }
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(st) = start {
        out.push(s[st..].trim());
    }
    out.into_iter().filter(|t| !t.is_empty()).collect()
}

/// The first `url(...)` in a `background-image` list, unquoted.
pub fn first_url(bg: &str) -> Option<String> {
    let i = bg.find("url(")?;
    let rest = bg[i + 4..].trim_start();
    let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'');
    let body = quote.map(|q| rest[1..].split(q).next().unwrap_or("")).unwrap_or_else(|| rest.split(')').next().unwrap_or(""));
    let url = body.trim();
    if url.is_empty() {
        None
    } else {
        Some(url.to_string())
    }
}

pub fn round2(v: f32) -> f64 {
    ((v as f64) * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(color("rgb(255, 0, 0)").as_deref(), Some("#FF0000"));
        assert_eq!(color("rgba(0, 0, 0, 0.5)").as_deref(), Some("#00000080"));
        assert_eq!(color("#abc").as_deref(), Some("#AABBCC"));
        assert_eq!(color("#aabbccdd").as_deref(), Some("#AABBCCDD"));
        assert_eq!(color("hsl(0, 100%, 50%)").as_deref(), Some("#FF0000"));
        assert_eq!(color("color(srgb 1 0.5 0 / 0.5)").as_deref(), Some("#FF800080"));
        // Tailwind v4 ships oklch colours; `oklch(0.5 0 0)` is mid grey.
        assert_eq!(color("oklch(0.5 0 0)").as_deref(), Some("#636363"));
        assert_eq!(color("oklch(0.7 0.15 200)").as_deref().map(|c| c.len()), Some(7));
        // Chrome reports Tailwind v4 colours as lab(); dark grey in, dark grey out.
        let dark = color("lab(1.90334 0.278696 -5.48866)").unwrap();
        assert_eq!(dark.len(), 7);
        assert!(u8::from_str_radix(&dark[1..3], 16).unwrap() < 12, "{dark}");
        assert_eq!(color("lab(100 0 0)").as_deref(), Some("#FFFFFF"));
        assert_eq!(color("transparent"), None);
    }

    #[test]
    fn gradient_directions_match_pen() {
        let g = gradient("linear-gradient(180deg, #fff 0%, #000 100%)").unwrap();
        assert_eq!(g["rotation"], json!(0.0));
        let g = gradient("linear-gradient(90deg, #fff, #000)").unwrap();
        assert_eq!(g["rotation"], json!(90.0));
        let g = gradient("linear-gradient(to right, #fff, #000)").unwrap();
        assert_eq!(g["rotation"], json!(90.0));
        let g = gradient("linear-gradient(to top, #fff, #000)").unwrap();
        assert_eq!(g["rotation"], json!(180.0));
    }

    #[test]
    fn gradient_stops_get_positions() {
        let g = gradient("linear-gradient(#fff, #f00, #000)").unwrap();
        let stops = g["colors"].as_array().unwrap();
        assert_eq!(stops.len(), 3);
        assert_eq!(stops[1]["position"], json!(0.5));
    }

    #[test]
    fn shadows_and_radius() {
        let s = shadows("rgba(0, 0, 0, 0.1) 0px 4px 12px 0px");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0]["blur"], json!(12.0));
        assert_eq!(s[0]["color"], json!("#0000001A"));
        // Space-separated colour syntax: the numbers inside the function are not lengths.
        let s = shadows("lab(1.9 0.3 -5.5) 0px 6px 18px 2px");
        assert_eq!(s[0]["offset"]["y"], json!(6.0));
        assert_eq!(s[0]["blur"], json!(18.0));
        assert_eq!(s[0]["spread"], json!(2.0));
        assert_eq!(shadows("inset rgb(0 0 0 / 10%) 0px 2px 4px")[0]["shadowType"], "inner");
        assert_eq!(radius("8px", 100.0, 100.0).unwrap(), json!(8.0));
        assert_eq!(radius("8px 4px 0px 0px", 100.0, 100.0).unwrap(), json!([8.0, 4.0, 0.0, 0.0]));
        assert_eq!(radius("50%", 40.0, 20.0).unwrap(), json!(10.0));
    }

    #[test]
    fn text_metrics() {
        assert_eq!(font_family("\"Inter\", system-ui, sans-serif").as_deref(), Some("Inter"));
        assert_eq!(font_family("system-ui, -apple-system, sans-serif"), None);
        assert_eq!(line_height("24px", 16.0), Some(1.5));
        assert_eq!(line_height("normal", 16.0), None);
        assert_eq!(font_weight("600").as_deref(), Some("600"));
        assert_eq!(font_weight("normal"), None);
    }
}
