//! Compiles document roots into flat, fully-resolved scene trees: variables and
//! themes are resolved, component instances (`ref`) are expanded, and legacy
//! property forms are normalized. Layout then fills in each node's `rect`.

use crate::imports::Imports;
use crate::style::*;
use crate::vars::{apply_theme, Theme, Vars};
use crate::{Document, NodeId, Props};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const NONE: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.right() && py >= self.y && py <= self.bottom()
    }
    pub fn outset(&self, d: f32) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    Note,
    Prompt,
    Context,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Frame { clip: bool, placeholder: bool },
    Group,
    Rect,
    Ellipse { inner: f32, start: f32, sweep: f32 },
    Polygon { count: u32 },
    Path { geometry: Arc<str>, view_box: Option<[f32; 4]>, even_odd: bool },
    Text(Box<TextData>),
    Icon { library: String, name: String, weight: Option<f32> },
    Note(NoteKind, Box<TextData>),
    /// Generates its children from a JS file (`scriptUri`) after layout; see `graft`.
    Script { uri: Arc<str>, inputs: Arc<serde_json::Map<String, Value>>, clip: bool },
    /// A `ref` whose component could not be found (or is cyclic).
    Missing,
}

#[derive(Clone, Debug)]
pub struct SNode {
    /// Document node this was compiled from (`None` for nodes that only exist in overrides).
    pub src: Option<NodeId>,
    /// Full id path for nodes inside component instances, e.g. `inst/child`.
    pub path: Option<Box<str>>,
    pub id: Box<str>,
    pub name: Option<Box<str>>,
    pub kind: Kind,
    pub parent: u32,
    pub children: Vec<u32>,
    pub enabled: bool,
    pub x: f32,
    pub y: f32,
    pub width: Sizing,
    pub height: Sizing,
    pub absolute: bool,
    pub layout: Dir,
    pub gap: f32,
    /// top, right, bottom, left
    pub padding: [f32; 4],
    pub justify: Justify,
    pub align: Align,
    pub fills: Vec<Paint>,
    pub stroke: Option<Box<Stroke>>,
    pub effects: Vec<Effect>,
    pub opacity: f32,
    pub rotation: f32,
    pub flip_x: bool,
    pub flip_y: bool,
    /// top-left, top-right, bottom-right, bottom-left
    pub radius: [f32; 4],
    pub reusable: bool,
    /// Computed by layout, relative to the parent.
    pub rect: Rect,
}

impl SNode {
    pub fn key(&self) -> &str {
        self.path.as_deref().unwrap_or(&self.id)
    }
    pub fn is_container(&self) -> bool {
        matches!(self.kind, Kind::Frame { .. } | Kind::Group | Kind::Script { .. })
    }
    pub fn clips(&self) -> bool {
        matches!(self.kind, Kind::Frame { clip: true, .. } | Kind::Script { clip: true, .. })
    }
}

/// One compiled document root.
#[derive(Clone, Debug, Default)]
pub struct RootScene {
    pub doc_root: Option<NodeId>,
    pub nodes: Vec<SNode>,
    /// Components (doc nodes) this root's instances depend on.
    pub uses: HashSet<NodeId>,
    by_src: HashMap<NodeId, u32>,
    by_path: HashMap<Box<str>, u32>,
    pub laid_out: bool,
    /// Absolute (document-space) bounds of each node and its visible descendants.
    pub bounds: Vec<Rect>,
    /// Changes every time the scene is laid out (keys render caches).
    pub stamp: u64,
}

impl RootScene {
    pub fn root(&self) -> Option<&SNode> {
        self.nodes.first()
    }
    /// Scene index of a document node or instance path.
    pub fn lookup(&self, key: &str, doc: &Document) -> Option<u32> {
        if key.contains('/') {
            return self.by_path.get(key).copied();
        }
        let n = doc.find(key)?;
        self.by_src.get(&n).copied()
    }
    pub fn lookup_node(&self, n: NodeId) -> Option<u32> {
        self.by_src.get(&n).copied()
    }
    /// Absolute (document-space) offset of a node's parent, ignoring rotation.
    pub fn abs_origin(&self, mut i: u32) -> (f32, f32) {
        let (mut x, mut y) = (0.0, 0.0);
        while i != NONE {
            let n = &self.nodes[i as usize];
            x += n.rect.x;
            y += n.rect.y;
            i = n.parent;
        }
        (x, y)
    }
    pub fn abs_rect(&self, i: u32) -> Rect {
        let n = &self.nodes[i as usize];
        let (px, py) = self.abs_origin(n.parent);
        Rect::new(px + n.rect.x, py + n.rect.y, n.rect.w, n.rect.h)
    }
}

/// Where a node's data comes from: the document, or raw JSON from an override.
#[derive(Clone, Copy)]
enum Src<'a> {
    Doc(NodeId),
    Json(&'a Value),
}

/// Descendant overrides active inside an instance, keyed by path relative to that instance.
type Overrides<'a> = HashMap<String, Vec<&'a Value>>;

struct Inst<'a> {
    prefix: String,
    overrides: Overrides<'a>,
}

pub struct Compiler<'a> {
    doc: &'a Document,
    vars: &'a Vars,
    imports: Option<&'a Imports>,
    out: RootScene,
    ref_stack: Vec<NodeId>,
    /// Depth of instances of imported components (they have no NodeId for cycle checks).
    import_depth: u32,
}

pub fn compile_root(doc: &Document, vars: &Vars, root: NodeId) -> RootScene {
    compile_root_with(doc, vars, root, None)
}

/// Compiles a top-level node; `imports` resolves refs to components in other files.
pub fn compile_root_with(doc: &Document, vars: &Vars, root: NodeId, imports: Option<&Imports>) -> RootScene {
    let mut c = Compiler { doc, vars, imports, out: RootScene { doc_root: Some(root), ..Default::default() }, ref_stack: Vec::new(), import_depth: 0 };
    let theme = vars.default_theme.clone();
    c.node(Src::Doc(root), NONE, &theme, None);
    c.out
}

/// Compiles nodes generated at runtime (a script's output) as the children of `parent`,
/// replacing any it had. They have no ids in the document.
pub fn graft(scene: &mut RootScene, doc: &Document, vars: &Vars, imports: Option<&Imports>, parent: u32, nodes: &[Value]) {
    let out = std::mem::take(scene);
    let mut c = Compiler { doc, vars, imports, out, ref_stack: Vec::new(), import_depth: 0 };
    let theme = vars.default_theme.clone();
    let list: Vec<u32> = nodes.iter().filter_map(|v| c.node(Src::Json(v), parent, &theme, None)).collect();
    c.out.nodes[parent as usize].children = list;
    *scene = c.out;
}

impl<'a> Compiler<'a> {
    fn node(&mut self, src: Src<'a>, parent: u32, theme: &Theme, inst: Option<&Inst<'a>>) -> Option<u32> {
        let doc = self.doc;
        let (mut props, mut kids): (Cow<'a, Props>, Kids<'a>) = match src {
            Src::Doc(n) => {
                let node = doc.node(n);
                (Cow::Borrowed(&node.props), Kids::Doc(&node.children))
            }
            Src::Json(v) => (Cow::Owned(v.as_object().cloned().unwrap_or_default()), json_kids(v)),
        };
        let mut src_id = match src {
            Src::Doc(n) => Some(n),
            Src::Json(_) => None,
        };
        let own_id = props.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        // Tracked separately so instance overrides can borrow it for 'a.
        let mut descendants: Option<&'a Value> = match src {
            Src::Doc(n) => doc.node(n).props.get("descendants"),
            Src::Json(v) => v.get("descendants"),
        };

        // Descendant overrides from enclosing instances.
        if let Some(inst) = inst {
            if let Some(list) = inst.overrides.get(&own_id) {
                for &ov in list {
                    let Some(o) = ov.as_object() else { continue };
                    if o.contains_key("type") {
                        props = Cow::Owned(o.clone());
                        kids = json_kids(ov);
                        src_id = None;
                        descendants = o.get("descendants");
                    } else {
                        if let Some(d) = o.get("descendants") {
                            descendants = Some(d);
                        }
                        let p = props.to_mut();
                        for (k, v) in o {
                            if k == "children" {
                                kids = json_kids(ov);
                            } else if k != "id" {
                                p.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
            }
        }

        let path: Option<Box<str>> = inst.map(|i| format!("{}{}", i.prefix, own_id).into_boxed_str());
        let theme_owned = apply_theme(theme, props.get("theme"));
        let theme = theme_owned.as_ref().unwrap_or(theme);

        // Component instance: merge the component with the ref's own properties.
        if props.get("type").and_then(Value::as_str) == Some("ref") {
            let target = props.get("ref").and_then(Value::as_str).unwrap_or("");
            let local = doc.find(target).filter(|c| !self.ref_stack.contains(c));
            let imported = match local {
                None if self.import_depth < 32 => self.imports.and_then(|i| i.node(target)),
                _ => None,
            };
            if local.is_none() && imported.is_none() {
                let idx = self.push(&props, Kind::Missing, src_id, path, parent, theme);
                return Some(idx);
            }
            // The component's own properties and children, from this file or an import.
            let (comp_props, comp_children): (Cow<'a, Props>, Kids<'a>) = match (local, imported) {
                (Some(c), _) => {
                    self.out.uses.insert(c);
                    (Cow::Borrowed(&doc.node(c).props), Kids::Doc(&doc.node(c).children))
                }
                (None, Some(v)) => (Cow::Owned(v.as_object().cloned().unwrap_or_default()), json_kids(v)),
                (None, None) => unreachable!(),
            };
            let mut merged = comp_props.into_owned();
            merged.shift_remove("children");
            for (k, v) in props.iter() {
                if matches!(k.as_str(), "type" | "ref" | "descendants" | "reusable" | "children") {
                    continue;
                }
                merged.insert(k.clone(), v.clone());
            }
            merged.shift_remove("reusable");
            let my_path = path.as_deref().map(str::to_string).unwrap_or_else(|| own_id.clone());
            let mut overrides: Overrides<'a> = HashMap::new();
            if let Some(Value::Object(d)) = descendants {
                for (k, v) in d {
                    overrides.entry(k.clone()).or_default().push(v);
                }
            }
            if let Some(outer) = inst {
                let pre = format!("{own_id}/");
                for (k, list) in &outer.overrides {
                    if let Some(rest) = k.strip_prefix(&pre) {
                        overrides.entry(rest.to_string()).or_default().extend(list.iter().copied());
                    }
                }
            }
            // Override children of the instance root itself, if given.
            let comp_kids = match kids {
                Kids::Json(list) if props.contains_key("children") => Kids::Json(list),
                _ => comp_children,
            };
            let new_inst = Inst { prefix: format!("{my_path}/"), overrides };
            let kind = self.kind(&merged, theme);
            let idx = self.push(&merged, kind, src_id.or(local), path, parent, theme);
            self.out.nodes[idx as usize].reusable = false;
            if inst.is_none() {
                self.out.nodes[idx as usize].src = src_id;
            }
            match local {
                Some(c) => self.ref_stack.push(c),
                None => self.import_depth += 1,
            }
            self.children(idx, comp_kids, theme, Some(&new_inst));
            match local {
                Some(_) => drop(self.ref_stack.pop()),
                None => self.import_depth -= 1,
            }
            return Some(idx);
        }

        let kind = self.kind(&props, theme);
        let idx = self.push(&props, kind, src_id, path, parent, theme);
        self.children(idx, kids, theme, inst);
        Some(idx)
    }

    fn children(&mut self, idx: u32, kids: Kids<'a>, theme: &Theme, inst: Option<&Inst<'a>>) {
        if !self.out.nodes[idx as usize].is_container() {
            return;
        }
        let mut list = Vec::new();
        match kids {
            Kids::Doc(ids) => {
                for &c in ids {
                    if let Some(ci) = self.node(Src::Doc(c), idx, theme, inst) {
                        list.push(ci);
                    }
                }
            }
            Kids::Json(vals) => {
                for v in vals {
                    if let Some(ci) = self.node(Src::Json(v), idx, theme, inst) {
                        list.push(ci);
                    }
                }
            }
        }
        self.out.nodes[idx as usize].children = list;
    }

    fn push(&mut self, p: &Props, kind: Kind, src: Option<NodeId>, path: Option<Box<str>>, parent: u32, theme: &Theme) -> u32 {
        let idx = self.out.nodes.len() as u32;
        let node = build_node(p, kind, src, path, parent, self.vars, theme);
        if let Some(path) = &node.path {
            self.out.by_path.insert(path.clone(), idx);
        } else if let Some(s) = src {
            self.out.by_src.insert(s, idx);
        }
        self.out.nodes.push(node);
        idx
    }

    fn kind(&self, p: &Props, theme: &Theme) -> Kind {
        let v = self.vars;
        let num = |k: &str| v.num(p.get(k), theme);
        match p.get("type").and_then(Value::as_str).unwrap_or("") {
            "frame" => Kind::Frame {
                clip: v.bool(p.get("clip"), theme).unwrap_or(false),
                placeholder: p.get("placeholder").and_then(Value::as_bool).unwrap_or(false),
            },
            "group" => Kind::Group,
            "rectangle" => Kind::Rect,
            "ellipse" => Kind::Ellipse {
                inner: num("innerRadius").unwrap_or(0.0),
                start: num("startAngle").unwrap_or(0.0),
                sweep: num("sweepAngle").unwrap_or(360.0),
            },
            "polygon" => Kind::Polygon { count: num("polygonCount").unwrap_or(3.0).max(3.0) as u32 },
            "path" => Kind::Path {
                geometry: p.get("geometry").and_then(Value::as_str).unwrap_or("").into(),
                view_box: p.get("viewBox").and_then(Value::as_array).and_then(|a| {
                    let f: Vec<f32> = a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect();
                    (f.len() == 4).then(|| [f[0], f[1], f[2], f[3]])
                }),
                even_odd: p.get("fillRule").and_then(Value::as_str) == Some("evenodd"),
            },
            "text" => Kind::Text(Box::new(text_data(p, v, theme))),
            "note" => Kind::Note(NoteKind::Note, Box::new(text_data(p, v, theme))),
            "prompt" => Kind::Note(NoteKind::Prompt, Box::new(text_data(p, v, theme))),
            "context" => Kind::Note(NoteKind::Context, Box::new(text_data(p, v, theme))),
            "icon" => Kind::Icon {
                library: v.str(p.get("library"), theme).unwrap_or("lucide").to_string(),
                name: v.str(p.get("icon"), theme).unwrap_or("").to_string(),
                weight: num("weight"),
            },
            "icon_font" => Kind::Icon {
                library: v.str(p.get("iconFontFamily"), theme).unwrap_or("lucide").to_string(),
                name: v.str(p.get("iconFontName"), theme).unwrap_or("").to_string(),
                weight: num("weight"),
            },
            "script" => Kind::Script {
                uri: v.str(p.get("scriptUri"), theme).unwrap_or("").into(),
                inputs: Arc::new(
                    p.get("inputs")
                        .and_then(Value::as_object)
                        .map(|o| o.iter().map(|(k, x)| (k.clone(), v.resolve_deep(x, theme))).collect())
                        .unwrap_or_default(),
                ),
                clip: v.bool(p.get("clip"), theme).unwrap_or(false),
            },
            _ => Kind::Rect,
        }
    }
}

#[derive(Clone, Copy)]
enum Kids<'a> {
    Doc(&'a [NodeId]),
    Json(&'a [Value]),
}

fn json_kids(v: &Value) -> Kids<'_> {
    match v.get("children") {
        Some(Value::Array(a)) => Kids::Json(a),
        _ => Kids::Json(&[]),
    }
}

fn text_data(p: &Props, v: &Vars, theme: &Theme) -> TextData {
    let mut s = TextStyle::default();
    if let Some(f) = v.str(p.get("fontFamily"), theme) {
        if !f.is_empty() {
            s.family = f.to_string();
        }
    }
    if let Some(f) = v.num(p.get("fontSize"), theme) {
        s.size = f;
    }
    match v.resolve(p.get("fontWeight").unwrap_or(&Value::Null), theme) {
        Value::String(w) => s.weight = parse_weight(w),
        Value::Number(n) => s.weight = n.as_f64().unwrap_or(400.0) as u16,
        _ => {}
    }
    s.italic = v.str(p.get("fontStyle"), theme).is_some_and(|f| f.contains("italic") || f.contains("oblique"));
    s.letter_spacing = v.num(p.get("letterSpacing"), theme).unwrap_or(0.0);
    s.line_height = v.num(p.get("lineHeight"), theme).filter(|l| *l > 0.0);
    s.align = match p.get("textAlign").and_then(Value::as_str) {
        Some("center") => TextAlign::Center,
        Some("right") => TextAlign::Right,
        Some("justify") => TextAlign::Justify,
        _ => TextAlign::Left,
    };
    s.valign = match p.get("textAlignVertical").and_then(Value::as_str) {
        Some("middle") => VAlign::Middle,
        Some("bottom") => VAlign::Bottom,
        _ => VAlign::Top,
    };
    s.underline = v.bool(p.get("underline"), theme).unwrap_or(false);
    s.strikethrough = v.bool(p.get("strikethrough"), theme).unwrap_or(false);
    let content = match v.resolve(p.get("content").unwrap_or(&Value::Null), theme) {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    };
    let growth = match p.get("textGrowth").and_then(Value::as_str) {
        Some("fixed-width") => TextGrowth::FixedWidth,
        Some("fixed-width-height") => TextGrowth::FixedWidthHeight,
        _ => TextGrowth::Auto,
    };
    TextData { content, style: s, growth }
}

fn build_node(p: &Props, kind: Kind, src: Option<NodeId>, path: Option<Box<str>>, parent: u32, v: &Vars, theme: &Theme) -> SNode {
    let num = |k: &str| v.num(p.get(k), theme);
    let resolved = |k: &str| p.get(k).map(|x| v.resolve(x, theme));

    let (dw, dh) = match &kind {
        Kind::Frame { .. } | Kind::Group => (Sizing::Fit(None), Sizing::Fit(None)),
        Kind::Icon { .. } => (Sizing::Fixed(24.0), Sizing::Fixed(24.0)),
        Kind::Note(..) => (Sizing::Fixed(240.0), Sizing::Fixed(160.0)),
        _ => (Sizing::Fixed(0.0), Sizing::Fixed(0.0)),
    };
    let mut width = parse_sizing(resolved("width"), dw);
    let mut height = parse_sizing(resolved("height"), dh);
    if let Kind::Text(t) = &kind {
        match t.growth {
            TextGrowth::Auto => {
                width = Sizing::Fit(None);
                height = Sizing::Fit(None);
            }
            TextGrowth::FixedWidth => {
                if matches!(width, Sizing::Fixed(0.0)) && p.get("width").is_none() {
                    width = Sizing::Fit(None);
                }
                height = Sizing::Fit(None);
            }
            TextGrowth::FixedWidthHeight => {}
        }
    }

    let layout = match &kind {
        Kind::Frame { .. } => match p.get("layout").and_then(Value::as_str) {
            Some("none") => Dir::None,
            Some("vertical") => Dir::Vertical,
            _ => Dir::Horizontal,
        },
        _ => Dir::None,
    };
    let padding = match resolved("padding") {
        Some(Value::Array(a)) => {
            let f: Vec<f32> = a.iter().map(|x| v.num(Some(x), theme).unwrap_or(0.0)).collect();
            match f.len() {
                1 => [f[0]; 4],
                2 => [f[0], f[1], f[0], f[1]],
                3 => [f[0], f[1], f[2], f[1]],
                4 => [f[0], f[1], f[2], f[3]],
                _ => [0.0; 4],
            }
        }
        Some(x) => [v.num(Some(x), theme).unwrap_or(0.0); 4],
        None => [0.0; 4],
    };
    let justify = match p.get("justifyContent").and_then(Value::as_str) {
        Some("center") => Justify::Center,
        Some("end") => Justify::End,
        Some("space_between") => Justify::SpaceBetween,
        Some("space_around") => Justify::SpaceAround,
        _ => Justify::Start,
    };
    let align = match p.get("alignItems").and_then(Value::as_str) {
        Some("center") => Align::Center,
        Some("end") => Align::End,
        _ => Align::Start,
    };
    let radius = match resolved("cornerRadius") {
        Some(Value::Array(a)) => {
            let f: Vec<f32> = a.iter().map(|x| v.num(Some(x), theme).unwrap_or(0.0)).collect();
            if f.len() == 4 {
                [f[0], f[1], f[2], f[3]]
            } else {
                [f.first().copied().unwrap_or(0.0); 4]
            }
        }
        Some(x) => [v.num(Some(x), theme).unwrap_or(0.0); 4],
        None => [0.0; 4],
    };

    let fills = match &kind {
        Kind::Group => Vec::new(),
        _ => parse_fills(p.get("fill"), v, theme),
    };
    let stroke = parse_stroke(p, v, theme, &kind);
    let effects = parse_effects(p.get("effect"), v, theme);

    SNode {
        src,
        path,
        id: p.get("id").and_then(Value::as_str).unwrap_or("").into(),
        name: p.get("name").and_then(Value::as_str).map(Into::into),
        kind,
        parent,
        children: Vec::new(),
        enabled: v.bool(p.get("enabled"), theme).unwrap_or(true),
        x: num("x").unwrap_or(0.0),
        y: num("y").unwrap_or(0.0),
        width,
        height,
        absolute: p.get("layoutPosition").and_then(Value::as_str) == Some("absolute"),
        layout,
        gap: num("gap").unwrap_or(0.0),
        padding,
        justify,
        align,
        fills,
        stroke,
        effects,
        opacity: num("opacity").unwrap_or(1.0).clamp(0.0, 1.0),
        rotation: num("rotation").unwrap_or(0.0),
        flip_x: v.bool(p.get("flipX"), theme).unwrap_or(false),
        flip_y: v.bool(p.get("flipY"), theme).unwrap_or(false),
        radius,
        reusable: p.get("reusable").and_then(Value::as_bool).unwrap_or(false),
        rect: Rect::default(),
    }
}

pub fn parse_fills(v: Option<&Value>, vars: &Vars, theme: &Theme) -> Vec<Paint> {
    let Some(v) = v else { return Vec::new() };
    let list: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    list.into_iter().filter_map(|f| parse_fill(f, vars, theme)).collect()
}

fn parse_fill(f: &Value, vars: &Vars, theme: &Theme) -> Option<Paint> {
    let f = vars.resolve(f, theme);
    match f {
        Value::String(s) => Some(Paint::Solid { color: Rgba::parse(s)?, blend: BlendMode::Normal }),
        Value::Object(o) => {
            if vars.bool(o.get("enabled"), theme) == Some(false) {
                return None;
            }
            let blend = BlendMode::parse(o.get("blendMode").and_then(Value::as_str));
            let opacity = vars.num(o.get("opacity"), theme).unwrap_or(1.0);
            let color = |x: Option<&Value>| x.and_then(|c| vars.str(Some(c), theme)).and_then(Rgba::parse);
            match o.get("type").and_then(Value::as_str).unwrap_or("color") {
                "color" => Some(Paint::Solid { color: color(o.get("color"))?, blend }),
                "gradient" => {
                    let kind = match o.get("gradientType").and_then(Value::as_str) {
                        Some("radial") => GradientKind::Radial,
                        Some("angular") => GradientKind::Angular,
                        _ => GradientKind::Linear,
                    };
                    let mut stops: Vec<(f32, Rgba)> = o
                        .get("colors")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| Some((vars.num(s.get("position"), theme).unwrap_or(0.0), color(s.get("color"))?)))
                                .collect()
                        })
                        .unwrap_or_default();
                    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
                    let c = o.get("center");
                    let sz = o.get("size");
                    Some(Paint::Gradient {
                        kind,
                        stops,
                        center: [
                            vars.num(c.and_then(|c| c.get("x")), theme).unwrap_or(0.5),
                            vars.num(c.and_then(|c| c.get("y")), theme).unwrap_or(0.5),
                        ],
                        size: [
                            vars.num(sz.and_then(|s| s.get("width")), theme).unwrap_or(1.0),
                            vars.num(sz.and_then(|s| s.get("height")), theme).unwrap_or(1.0),
                        ],
                        rotation: vars.num(o.get("rotation"), theme).unwrap_or(0.0),
                        opacity,
                        blend,
                    })
                }
                "image" => Some(Paint::Image {
                    url: o.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
                    mode: match o.get("mode").and_then(Value::as_str) {
                        Some("stretch") => ImageMode::Stretch,
                        Some("fit") => ImageMode::Fit,
                        _ => ImageMode::Fill,
                    },
                    opacity,
                    blend,
                }),
                "shader" => {
                    let uniforms = o
                        .get("uniforms")
                        .and_then(Value::as_object)
                        .map(|u| {
                            u.iter()
                                .filter_map(|(k, val)| {
                                    let val = vars.resolve(val, theme);
                                    let u = match val {
                                        Value::Number(n) => Uniform::Float(n.as_f64()? as f32),
                                        Value::Bool(b) => Uniform::Bool(*b),
                                        Value::String(s) => Rgba::parse(s).map(Uniform::Color).unwrap_or_else(|| Uniform::Text(s.clone())),
                                        Value::Array(a) => Uniform::Vec(
                                            a.iter().map(|x| vars.num(Some(x), theme).unwrap_or(0.0)).collect(),
                                        ),
                                        _ => return None,
                                    };
                                    Some((k.clone(), u))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(Paint::Shader {
                        url: o.get("url").and_then(Value::as_str).unwrap_or("").to_string(),
                        uniforms,
                        opacity,
                        blend,
                    })
                }
                "mesh_gradient" => {
                    let columns = o.get("columns").and_then(Value::as_u64).unwrap_or(2) as u32;
                    let rows = o.get("rows").and_then(Value::as_u64).unwrap_or(2) as u32;
                    let colors = o
                        .get("colors")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(|c| color(Some(c))).collect())
                        .unwrap_or_default();
                    let points = o
                        .get("points")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(|p| {
                                    let pair = |v: Option<&Value>| -> Option<[f32; 2]> {
                                        let a = v?.as_array()?;
                                        Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
                                    };
                                    if p.is_array() {
                                        return Some(MeshPoint { pos: pair(Some(p))?, ..Default::default() });
                                    }
                                    Some(MeshPoint {
                                        pos: pair(p.get("position"))?,
                                        left: pair(p.get("leftHandle")),
                                        right: pair(p.get("rightHandle")),
                                        top: pair(p.get("topHandle")),
                                        bottom: pair(p.get("bottomHandle")),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    Some(Paint::Mesh { columns, rows, colors, points, opacity, blend })
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn parse_stroke(p: &Props, vars: &Vars, theme: &Theme, kind: &Kind) -> Option<Box<Stroke>> {
    let mut stroke_v = p.get("stroke");
    let mut width_v = p.get("strokeWidth");
    let mut align_v = p.get("strokeAlignment").and_then(Value::as_str);
    // Legacy (≤2.11) form: { "align": "inside", "thickness": 2, "fill": ... }
    let legacy = stroke_v.and_then(Value::as_object).filter(|o| !o.contains_key("type"));
    if let Some(o) = legacy {
        stroke_v = o.get("fill");
        width_v = width_v.or(o.get("thickness"));
        align_v = align_v.or(match o.get("align").and_then(Value::as_str) {
            Some("inside") => Some("inner"),
            Some("outside") => Some("outer"),
            Some("center") => Some("center"),
            _ => None,
        });
    }
    let paints = parse_fills(stroke_v, vars, theme);
    if paints.is_empty() {
        return None;
    }
    let width = match width_v.map(|w| vars.resolve(w, theme)) {
        Some(Value::Object(o)) => {
            let side = |k: &str| vars.num(o.get(k), theme).unwrap_or(0.0);
            [side("top"), side("right"), side("bottom"), side("left")]
        }
        Some(x) => [vars.num(Some(x), theme).unwrap_or(1.0); 4],
        None => [1.0; 4],
    };
    if width.iter().all(|&w| w <= 0.0) {
        return None;
    }
    let default_align = match kind {
        Kind::Path { .. } | Kind::Text(_) => StrokeAlign::Center,
        _ => crate::DEFAULT_STROKE_ALIGN,
    };
    Some(Box::new(Stroke {
        paints,
        width,
        align: match align_v {
            Some("inner") => StrokeAlign::Inner,
            Some("outer") => StrokeAlign::Outer,
            Some("center") => StrokeAlign::Center,
            _ => default_align,
        },
        cap: match p.get("strokeLinecap").and_then(Value::as_str) {
            Some("round") => Cap::Round,
            Some("square") => Cap::Square,
            _ => Cap::Butt,
        },
        join: match p.get("strokeLinejoin").and_then(Value::as_str) {
            Some("round") => Join::Round,
            Some("bevel") => Join::Bevel,
            _ => Join::Miter,
        },
    }))
}

fn parse_effects(v: Option<&Value>, vars: &Vars, theme: &Theme) -> Vec<Effect> {
    let Some(v) = v else { return Vec::new() };
    let list: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    list.into_iter()
        .filter_map(|e| {
            let o = e.as_object()?;
            if vars.bool(o.get("enabled"), theme) == Some(false) {
                return None;
            }
            let num = |k: &str| vars.num(o.get(k), theme);
            Some(match o.get("type").and_then(Value::as_str)? {
                "blur" => Effect::Blur { radius: num("radius").unwrap_or(4.0) },
                "background_blur" => Effect::BackgroundBlur { radius: num("radius").unwrap_or(4.0) },
                "shadow" => Effect::Shadow {
                    inner: o.get("shadowType").and_then(Value::as_str) == Some("inner"),
                    offset: [
                        vars.num(o.get("offset").and_then(|x| x.get("x")), theme).unwrap_or(0.0),
                        vars.num(o.get("offset").and_then(|x| x.get("y")), theme).unwrap_or(0.0),
                    ],
                    spread: num("spread").unwrap_or(0.0),
                    blur: num("blur").unwrap_or(0.0),
                    color: o
                        .get("color")
                        .and_then(|c| vars.str(Some(c), theme))
                        .and_then(Rgba::parse)
                        .unwrap_or(Rgba([0.0, 0.0, 0.0, 0.25])),
                    blend: BlendMode::parse(o.get("blendMode").and_then(Value::as_str)),
                },
                _ => return None,
            })
        })
        .collect()
}
