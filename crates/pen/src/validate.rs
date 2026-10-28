//! Schema checks for node data coming from `execute`. Errors abort the call;
//! warnings are reported back so they can be fixed in the next call.

use serde_json::Value;

const ENTITY: &[&str] = &[
    "type", "id", "name", "context", "reusable", "theme", "enabled", "opacity", "flipX", "flipY", "layoutPosition", "metadata", "rotation", "x",
    "y",
];
const SIZE: &[&str] = &["width", "height"];
const GRAPHICS: &[&str] = &["fill", "stroke", "strokeWidth", "strokeLinecap", "strokeLinejoin", "strokeAlignment", "effect"];
const LAYOUT: &[&str] = &["layout", "gap", "layoutIncludeStroke", "padding", "justifyContent", "alignItems"];
const TEXT_STYLE: &[&str] = &[
    "fontFamily", "fontSize", "fontWeight", "letterSpacing", "fontStyle", "underline", "lineHeight", "textAlign", "textAlignVertical",
    "strikethrough", "href",
];

fn allowed(ty: &str) -> Option<Vec<&'static str>> {
    let mut v: Vec<&str> = ENTITY.to_vec();
    match ty {
        "frame" => {
            v.extend(SIZE);
            v.extend(GRAPHICS);
            v.extend(LAYOUT);
            v.extend(["cornerRadius", "clip", "placeholder", "slot", "children"]);
        }
        "group" => v.extend(["children", "effect"]),
        "rectangle" => {
            v.extend(SIZE);
            v.extend(GRAPHICS);
            v.push("cornerRadius");
        }
        "ellipse" => {
            v.extend(SIZE);
            v.extend(GRAPHICS);
            v.extend(["innerRadius", "startAngle", "sweepAngle"]);
        }
        "polygon" => {
            v.extend(SIZE);
            v.extend(GRAPHICS);
            v.extend(["polygonCount", "cornerRadius"]);
        }
        "path" => {
            v.extend(SIZE);
            v.extend(GRAPHICS);
            v.extend(["fillRule", "geometry", "viewBox"]);
        }
        "text" => {
            v.extend(SIZE);
            v.extend(GRAPHICS);
            v.extend(TEXT_STYLE);
            v.extend(["content", "textGrowth"]);
        }
        "note" | "context" | "prompt" => {
            v.extend(SIZE);
            v.extend(TEXT_STYLE);
            v.extend(["content", "model"]);
        }
        "icon" => {
            v.extend(SIZE);
            v.extend(["effect", "library", "icon", "weight", "fill"]);
        }
        "icon_font" => {
            v.extend(SIZE);
            v.extend(["effect", "iconFontFamily", "iconFontName", "weight", "fill"]);
        }
        "script" => {
            v.extend(SIZE);
            v.extend(["clip", "scriptUri", "inputs"]);
        }
        _ => return None,
    }
    Some(v)
}

#[derive(Default, Debug)]
pub struct Report {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl Report {
    pub fn merge(&mut self, o: Report) {
        self.errors.extend(o.errors);
        self.warnings.extend(o.warnings);
    }
}

fn label(v: &Value) -> String {
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("node");
    match v.get("name").and_then(Value::as_str) {
        Some(n) => format!("{ty} '{n}'"),
        None => ty.to_string(),
    }
}

fn is_size(v: &Value) -> bool {
    match v {
        Value::Number(_) => true,
        Value::String(s) => {
            let s = s.trim();
            s.starts_with('$') || {
                let word = s.split('(').next().unwrap_or("");
                matches!(word, "fit_content" | "fill_container")
                    && (!s.contains('(') || s.ends_with(')') && s[word.len() + 1..s.len() - 1].trim().parse::<f64>().is_ok())
            }
        }
        _ => false,
    }
}

fn enum_check(r: &mut Report, v: &Value, who: &str, key: &str, options: &[&str]) {
    if let Some(x) = v.get(key) {
        match x.as_str() {
            Some(s) if options.contains(&s) || s.starts_with('$') => {}
            _ => r.errors.push(format!("{who}: invalid {key} {x} (expected one of: {})", options.join(", "))),
        }
    }
}

/// Checks node data (recursively through `children`). `parent_layout` is the
/// parent frame's layout direction, if known, for x/y and fill checks.
pub fn check(v: &Value, parent_layout: Option<&str>, partial: bool) -> Report {
    let mut r = Report::default();
    let Some(obj) = v.as_object() else {
        r.errors.push(format!("node data must be an object, got {v}"));
        return r;
    };
    let who = label(v);
    let ty = obj.get("type").and_then(Value::as_str);
    if ty.is_none() && !partial {
        r.errors.push(format!("{who}: missing `type`"));
        return r;
    }
    if let Some(ty) = ty {
        if ty == "ref" {
            if !partial && obj.get("ref").and_then(Value::as_str).is_none() {
                r.errors.push(format!("{who}: ref nodes need a `ref` id"));
            }
        } else if let Some(keys) = allowed(ty) {
            for k in obj.keys() {
                if k == "margin" {
                    r.errors.push(format!("{who}: `margin` is not supported; wrap the node in a frame with padding"));
                } else if !keys.contains(&k.as_str()) {
                    r.warnings.push(format!("{who}: property `{k}` is not supported on {ty} and is ignored"));
                }
            }
        } else if !partial {
            r.errors.push(format!(
                "{who}: unknown type '{ty}' (frame, group, rectangle, ellipse, polygon, path, text, note, prompt, context, icon, script, ref)"
            ));
        }
    }
    for key in ["width", "height"] {
        if let Some(x) = obj.get(key) {
            if !is_size(x) {
                r.errors.push(format!("{who}: invalid {key} {x}: use a number, a $variable, fit_content or fill_container (no %, vh or calc)"));
            }
        }
    }
    enum_check(&mut r, v, &who, "layout", &["none", "vertical", "horizontal"]);
    enum_check(&mut r, v, &who, "justifyContent", &["start", "center", "end", "space_between", "space_around"]);
    enum_check(&mut r, v, &who, "alignItems", &["start", "center", "end"]);
    enum_check(&mut r, v, &who, "textGrowth", &["auto", "fixed-width", "fixed-width-height"]);
    enum_check(&mut r, v, &who, "layoutPosition", &["auto", "absolute"]);
    enum_check(&mut r, v, &who, "textAlign", &["left", "center", "right", "justify"]);
    enum_check(&mut r, v, &who, "textAlignVertical", &["top", "middle", "bottom"]);
    enum_check(&mut r, v, &who, "strokeAlignment", &["inner", "center", "outer"]);
    if let Some(id) = obj.get("id").and_then(Value::as_str) {
        if id.contains('/') {
            r.errors.push(format!("{who}: ids must not contain '/'"));
        }
    }

    if !partial {
        if ty == Some("text") {
            if obj.get("fill").is_none() {
                r.warnings.push(format!("{who}: text has no `fill` and will be invisible"));
            }
            let growth = obj.get("textGrowth").and_then(Value::as_str).unwrap_or("auto");
            if growth == "auto" && (obj.contains_key("width") || obj.contains_key("height")) {
                r.warnings.push(format!("{who}: width/height are ignored with textGrowth auto; set textGrowth to fixed-width to wrap"));
            }
            if growth != "auto" && !obj.contains_key("width") {
                r.warnings.push(format!("{who}: textGrowth {growth} needs a width"));
            }
        }
        let absolute = obj.get("layoutPosition").and_then(Value::as_str) == Some("absolute");
        if matches!(parent_layout, Some("horizontal" | "vertical")) && !absolute && (obj.contains_key("x") || obj.contains_key("y")) {
            r.warnings.push(format!("{who}: x/y are ignored inside a flex layout parent"));
        }
        if matches!(parent_layout, Some("none")) || absolute {
            for key in ["width", "height"] {
                if obj.get(key).and_then(Value::as_str).is_some_and(|s| s.starts_with("fill_container") && !s.contains('(')) {
                    r.warnings.push(format!("{who}: {key} fill_container needs a flex layout parent; it falls back to 0 here"));
                }
            }
        }
        if ty.is_some_and(|t| t != "frame") {
            for key in ["layout", "padding", "gap"] {
                if obj.contains_key(key) && ty != Some("ref") {
                    r.warnings.push(format!("{who}: `{key}` only applies to frames"));
                }
            }
        }
    }

    if let Some(children) = obj.get("children") {
        let can = matches!(ty, Some("frame" | "group")) || partial;
        match children {
            Value::Array(list) if can => {
                let layout = if ty == Some("frame") { obj.get("layout").and_then(Value::as_str).or(Some("horizontal")) } else { Some("none") };
                for c in list {
                    r.merge(check(c, layout, false));
                }
            }
            Value::Array(_) => r.errors.push(format!("{who}: only frames and groups can have children")),
            _ => r.errors.push(format!("{who}: `children` must be an array")),
        }
    }
    r
}
