//! MCP tool implementations that operate on an open document.

use crate::api::{self, png_base64, Edit};
use crate::engine::Engine;
use serde_json::{json, Value};

/// What the window knows that the engine doesn't (camera), for get_app_state.
#[derive(Clone, Copy, Default)]
pub struct ViewInfo {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub zoom: f32,
}

pub fn execute(engine: &mut Engine, args: &Value) -> Value {
    let input = args.get("input").and_then(Value::as_str).map(str::to_string);
    let edit_id = args.get("editId").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let edits: Vec<Edit> = args
        .get("edits")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    Some(Edit {
                        find: e.get("find")?.as_str()?.to_string(),
                        replace: e.get("replace").and_then(Value::as_str).unwrap_or("").to_string(),
                        all: e.get("all").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if edit_id.is_some() && input.is_some() {
        return crate::ipc::tool_error("Send either `input` or `editId` + `edits`, not both.");
    }
    let r = api::execute(engine, input, edit_id, edits);
    let mut content = vec![json!({"type": "text", "text": r.text})];
    for png in r.images {
        content.push(json!({"type": "image", "data": png_base64(&png), "mimeType": "image/png"}));
    }
    json!({"content": content, "isError": !r.ok})
}

/// The state block of get_app_state for one document.
pub fn app_state(engine: &mut Engine, view: Option<ViewInfo>) -> String {
    engine.refresh();
    let mut s = String::new();
    let path = engine.path().map(|p| p.display().to_string()).unwrap_or_else(|| "(unsaved)".into());
    s.push_str(&format!("Active document: {path}\n"));
    s.push_str(&format!("Nodes: {}, top-level nodes: {}\n", engine.doc.node_count(), engine.doc.roots.len()));
    for e in &engine.imports.errors {
        s.push_str(&format!("Import problem: {e}
"));
    }
    if let Some(v) = view {
        s.push_str(&format!(
            "Viewport (document coords): x={:.0} y={:.0} width={:.0} height={:.0}, zoom {:.0}%\n",
            v.x,
            v.y,
            v.w,
            v.h,
            v.zoom * 100.0
        ));
    }
    if engine.selection.is_empty() {
        s.push_str("Selection: none\n");
    } else {
        s.push_str("Selection:\n");
        for key in &engine.selection {
            let (ty, name) = describe(engine, key);
            let r = engine.abs_rect(key).map(|r| format!(" at x={:.0} y={:.0} {:.0}×{:.0}", r.x, r.y, r.w, r.h)).unwrap_or_default();
            s.push_str(&format!("- {key} ({ty} '{name}'){r}\n"));
        }
    }
    let vars = engine.doc.variables().map(|v| v.len()).unwrap_or(0);
    let themes = engine.doc.themes().map(|t| serde_json::to_string(t).unwrap_or_default()).unwrap_or_else(|| "none".into());
    s.push_str(&format!("Variables: {vars}, themes: {themes}\n"));
    let comps: Vec<String> = {
        let mut out = Vec::new();
        engine.doc.walk(|n, _| {
            let node = engine.doc.node(n);
            if node.is_reusable() {
                out.push(format!("{} '{}'", node.id(), node.name().unwrap_or("")));
            }
        });
        out
    };
    if !comps.is_empty() {
        s.push_str(&format!("Components ({}): {}\n", comps.len(), comps.iter().take(40).cloned().collect::<Vec<_>>().join(", ")));
        if comps.len() > 40 {
            s.push_str("  … use Get(n => n.reusable && Print(n.id, n.name)) for the full list\n");
        }
    }
    s.push_str("Top-level nodes (id 'name' x y width height):\n");
    for (k, &r) in engine.doc.roots.iter().enumerate() {
        if k >= 80 {
            s.push_str(&format!("  … {} more\n", engine.doc.roots.len() - 80));
            break;
        }
        let node = engine.doc.node(r);
        let rect = engine.abs_rect(node.id()).unwrap_or_default();
        s.push_str(&format!(
            "  {} '{}' {:.0} {:.0} {:.0} {:.0}{}\n",
            node.id(),
            node.name().unwrap_or(node.ty()),
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            if node.is_reusable() { " (component)" } else { "" }
        ));
    }
    s
}

fn describe(engine: &Engine, key: &str) -> (String, String) {
    match engine.locate(key) {
        Some((sc, i)) => {
            let n = &sc.nodes[i as usize];
            let ty = match &n.kind {
                pen::scene::Kind::Frame { .. } => "frame",
                pen::scene::Kind::Group => "group",
                pen::scene::Kind::Rect => "rectangle",
                pen::scene::Kind::Ellipse { .. } => "ellipse",
                pen::scene::Kind::Polygon { .. } => "polygon",
                pen::scene::Kind::Path { .. } => "path",
                pen::scene::Kind::Text(_) => "text",
                pen::scene::Kind::Icon { .. } => "icon",
                pen::scene::Kind::Note(..) => "note",
                pen::scene::Kind::Script { .. } => "script",
                pen::scene::Kind::Missing => "ref",
            };
            (ty.to_string(), n.name.as_deref().unwrap_or("").to_string())
        }
        None => ("?".into(), String::new()),
    }
}
