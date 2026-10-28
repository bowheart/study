//! SVG markup → `.pen` path nodes, for `Generate(frame, "svg", markup)`.
//!
//! The SVG is normalized by usvg (styles, `use`, transforms, basic shapes → paths), scaled to fit
//! the target frame (centered, aspect preserved) and flattened into one path node per shape.

use serde_json::{json, Map, Value};
use skia_safe::Path as SkPath;
use usvg::tiny_skia_path::{PathSegment, Point, Transform};

pub fn to_nodes(markup: &str, frame_w: f32, frame_h: f32) -> Result<Vec<Value>, String> {
    let tree = usvg::Tree::from_str(markup, &usvg::Options::default()).map_err(|e| format!("could not parse the SVG: {e}"))?;
    let size = tree.size();
    let s = (frame_w / size.width()).min(frame_h / size.height());
    let fit = Transform::from_row(s, 0.0, 0.0, s, (frame_w - size.width() * s) / 2.0, (frame_h - size.height() * s) / 2.0);
    let mut out = Vec::new();
    walk(tree.root(), fit, 1.0, &mut out);
    if out.is_empty() {
        return Err("the SVG contains no visible shapes (text, images and filters are not imported; convert text to paths)".into());
    }
    Ok(out)
}

fn walk(g: &usvg::Group, fit: Transform, opacity: f32, out: &mut Vec<Value>) {
    let opacity = opacity * g.opacity().get();
    for node in g.children() {
        match node {
            usvg::Node::Group(g) => walk(g, fit, opacity, out),
            usvg::Node::Path(p) if p.is_visible() => {
                if let Some(v) = path_node(p, fit, opacity, out.len() + 1) {
                    out.push(v);
                }
            }
            _ => {}
        }
    }
}

fn path_node(p: &usvg::Path, fit: Transform, opacity: f32, index: usize) -> Option<Value> {
    let ts = fit.pre_concat(p.abs_transform());
    let d = path_data(p.data(), ts);
    let sk = SkPath::from_svg(&d)?;
    let b = sk.compute_tight_bounds();
    let (w, h) = (b.width().max(0.5), b.height().max(0.5));
    let local = path_data(p.data(), ts.post_concat(Transform::from_translate(-b.left, -b.top)));

    let mut o = Map::new();
    o.insert("type".into(), json!("path"));
    let name = if p.id().is_empty() { format!("Path {index}") } else { p.id().to_string() };
    o.insert("name".into(), json!(name));
    o.insert("x".into(), num(b.left));
    o.insert("y".into(), num(b.top));
    o.insert("width".into(), num(w));
    o.insert("height".into(), num(h));
    o.insert("geometry".into(), json!(local));
    o.insert("viewBox".into(), json!([0, 0, round(w), round(h)]));
    let bbox = [b.left, b.top, w, h];
    if let Some(f) = p.fill() {
        if let Some(fill) = paint(f.paint(), f.opacity().get() * opacity, ts, bbox) {
            o.insert("fill".into(), fill);
        }
        if f.rule() == usvg::FillRule::EvenOdd {
            o.insert("fillRule".into(), json!("evenodd"));
        }
    }
    if let Some(st) = p.stroke() {
        if let Some(fill) = paint(st.paint(), st.opacity().get() * opacity, ts, bbox) {
            let (sx, sy) = ts.get_scale();
            o.insert("stroke".into(), fill);
            o.insert("strokeWidth".into(), num(st.width().get() * (sx * sy).abs().sqrt()));
            o.insert("strokeAlignment".into(), json!("center"));
            o.insert(
                "strokeLinecap".into(),
                json!(match st.linecap() {
                    usvg::LineCap::Butt => "butt",
                    usvg::LineCap::Round => "round",
                    usvg::LineCap::Square => "square",
                }),
            );
            o.insert(
                "strokeLinejoin".into(),
                json!(match st.linejoin() {
                    usvg::LineJoin::Round => "round",
                    usvg::LineJoin::Bevel => "bevel",
                    _ => "miter",
                }),
            );
        }
    }
    if !o.contains_key("fill") && !o.contains_key("stroke") {
        return None;
    }
    Some(Value::Object(o))
}

/// A usvg paint as a `.pen` fill. Gradients are re-expressed in the node's normalized box.
fn paint(p: &usvg::Paint, opacity: f32, ts: Transform, [bx, by, bw, bh]: [f32; 4]) -> Option<Value> {
    let norm = |x: f32, y: f32, gt: Transform| {
        let mut pt = Point::from_xy(x, y);
        ts.pre_concat(gt).map_point(&mut pt);
        ((pt.x - bx) / bw, (pt.y - by) / bh)
    };
    let stops = |stops: &[usvg::Stop]| -> Vec<Value> {
        stops
            .iter()
            .map(|s| json!({"color": hex(s.color(), s.opacity().get() * opacity), "position": round(s.offset().get())}))
            .collect()
    };
    match p {
        usvg::Paint::Color(c) => Some(json!(hex(*c, opacity))),
        usvg::Paint::LinearGradient(g) => {
            let (x1, y1) = norm(g.x1(), g.y1(), g.transform());
            let (x2, y2) = norm(g.x2(), g.y2(), g.transform());
            let (dx, dy) = (x2 - x1, y2 - y1);
            Some(json!({
                "type": "gradient",
                "gradientType": "linear",
                "center": {"x": round((x1 + x2) / 2.0), "y": round((y1 + y2) / 2.0)},
                "size": {"width": 1, "height": round((dx * dx + dy * dy).sqrt())},
                "rotation": round(dx.atan2(dy).to_degrees()),
                "colors": stops(g.stops()),
            }))
        }
        usvg::Paint::RadialGradient(g) => {
            let (cx, cy) = norm(g.cx(), g.cy(), g.transform());
            let (ex, ey) = norm(g.cx() + g.r().get(), g.cy() + g.r().get(), g.transform());
            Some(json!({
                "type": "gradient",
                "gradientType": "radial",
                "center": {"x": round(cx), "y": round(cy)},
                "size": {"width": round(2.0 * (ex - cx).abs()), "height": round(2.0 * (ey - cy).abs())},
                "colors": stops(g.stops()),
            }))
        }
        usvg::Paint::Pattern(_) => None,
    }
}

fn path_data(path: &usvg::tiny_skia_path::Path, ts: Transform) -> String {
    let mut d = String::new();
    let pt = |p: Point, d: &mut String| {
        let mut p = p;
        ts.map_point(&mut p);
        d.push_str(&format!("{} {} ", round(p.x), round(p.y)));
    };
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                d.push('M');
                pt(p, &mut d);
            }
            PathSegment::LineTo(p) => {
                d.push('L');
                pt(p, &mut d);
            }
            PathSegment::QuadTo(a, p) => {
                d.push('Q');
                pt(a, &mut d);
                pt(p, &mut d);
            }
            PathSegment::CubicTo(a, b, p) => {
                d.push('C');
                pt(a, &mut d);
                pt(b, &mut d);
                pt(p, &mut d);
            }
            PathSegment::Close => d.push_str("Z "),
        }
    }
    d.trim_end().to_string()
}

fn hex(c: usvg::Color, alpha: f32) -> String {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    if a == 255 {
        format!("#{:02X}{:02X}{:02X}", c.red, c.green, c.blue)
    } else {
        format!("#{:02X}{:02X}{:02X}{a:02X}", c.red, c.green, c.blue)
    }
}

fn round(v: f32) -> f64 {
    ((v as f64) * 100.0).round() / 100.0
}

fn num(v: f32) -> Value {
    pen::json::number(round(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_fitted_into_the_frame() {
        let svg = r##"<svg viewBox="0 0 20 10" xmlns="http://www.w3.org/2000/svg">
            <rect id="Box" x="10" y="0" width="10" height="10" fill="#ff0000" fill-opacity="0.5"/>
            <circle cx="5" cy="5" r="5" fill="none" stroke="#000" stroke-width="2"/>
        </svg>"##;
        let nodes = to_nodes(svg, 200.0, 200.0).unwrap();
        assert_eq!(nodes.len(), 2);
        // 20×10 scaled ×10 into 200×200, centered vertically (y offset 50).
        let b = &nodes[0];
        assert_eq!((b["name"].as_str(), b["x"].as_f64(), b["y"].as_f64(), b["width"].as_f64()), (Some("Box"), Some(100.0), Some(50.0), Some(100.0)));
        assert_eq!(b["fill"], "#FF000080");
        let c = &nodes[1];
        assert_eq!(c["name"], "Path 2");
        assert_eq!(c["strokeWidth"].as_f64(), Some(20.0));
        assert!(c.get("fill").is_none());
    }

    #[test]
    fn linear_gradients_map_to_the_node_box() {
        let svg = r##"<svg viewBox="0 0 10 10" xmlns="http://www.w3.org/2000/svg">
            <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#000"/></linearGradient></defs>
            <rect width="10" height="10" fill="url(#g)"/>
        </svg>"##;
        let nodes = to_nodes(svg, 100.0, 100.0).unwrap();
        let f = &nodes[0]["fill"];
        assert_eq!(f["rotation"].as_f64(), Some(90.0));
        assert_eq!(f["size"]["height"].as_f64(), Some(1.0));
        assert_eq!(f["center"]["x"].as_f64(), Some(0.5));
    }
}
