use pen::layout::{layout, TextMeasure};
use pen::scene::{compile_root, Kind, RootScene};
use pen::style::TextData;
use pen::vars::Vars;
use pen::Document;
use serde_json::json;

/// Every character is 10×20.
struct Mono;
impl TextMeasure for Mono {
    fn measure(&mut self, t: &TextData, width: Option<f32>) -> [f32; 2] {
        let w = t.content.chars().count() as f32 * 10.0;
        match width {
            Some(max) if max > 0.0 => [max, (w / max).ceil().max(1.0) * 20.0],
            _ => [w, 20.0],
        }
    }
}

fn scene(root: serde_json::Value) -> RootScene {
    let doc = Document::parse(&json!({"version": "2.17", "children": [root]}).to_string()).unwrap();
    let vars = Vars::new(&doc);
    let mut s = compile_root(&doc, &vars, doc.roots[0]);
    layout(&mut s, &mut Mono);
    s
}

fn rect(s: &RootScene, name: &str) -> [f32; 4] {
    let n = s.nodes.iter().find(|n| n.name.as_deref() == Some(name)).unwrap();
    [n.rect.x, n.rect.y, n.rect.w, n.rect.h]
}

#[test]
fn fit_content_sums_children_gaps_and_padding() {
    let s = scene(json!({"type": "frame", "name": "P", "layout": "vertical", "gap": 10, "padding": [5, 7],
        "children": [{"type": "rectangle", "name": "A", "width": 100, "height": 30}, {"type": "rectangle", "name": "B", "width": 60, "height": 20}]}));
    assert_eq!(rect(&s, "P"), [0.0, 0.0, 114.0, 70.0]);
    assert_eq!(rect(&s, "A"), [7.0, 5.0, 100.0, 30.0]);
    assert_eq!(rect(&s, "B"), [7.0, 45.0, 60.0, 20.0]);
}

#[test]
fn fill_container_splits_leftover_space() {
    let s = scene(json!({"type": "frame", "name": "P", "width": 300, "height": 50, "gap": 10,
        "children": [{"type": "rectangle", "name": "A", "width": 80, "height": 10},
                     {"type": "rectangle", "name": "B", "width": "fill_container", "height": "fill_container"},
                     {"type": "rectangle", "name": "C", "width": "fill_container", "height": 10}]}));
    assert_eq!(rect(&s, "B"), [90.0, 0.0, 100.0, 50.0]);
    assert_eq!(rect(&s, "C"), [200.0, 0.0, 100.0, 10.0]);
}

#[test]
fn fill_child_in_fit_parent_collapses() {
    let s = scene(json!({"type": "frame", "name": "P", "layout": "vertical",
        "children": [{"type": "text", "name": "T", "textGrowth": "fixed-width", "width": "fill_container", "content": "hello"}]}));
    assert_eq!(rect(&s, "P")[2], 0.0);
    let s = scene(json!({"type": "frame", "name": "P", "layout": "vertical", "width": "fit_content(200)",
        "children": [{"type": "text", "name": "T", "textGrowth": "fixed-width", "width": "fill_container", "content": "hello world, wrapping"}]}));
    assert_eq!(rect(&s, "T"), [0.0, 0.0, 200.0, 40.0]);
}

#[test]
fn justify_and_align() {
    let s = scene(json!({"type": "frame", "name": "P", "width": 200, "height": 100, "justifyContent": "space_between", "alignItems": "center",
        "children": [{"type": "rectangle", "name": "A", "width": 20, "height": 20}, {"type": "rectangle", "name": "B", "width": 20, "height": 40}]}));
    assert_eq!(rect(&s, "A"), [0.0, 40.0, 20.0, 20.0]);
    assert_eq!(rect(&s, "B"), [180.0, 30.0, 20.0, 40.0]);
    let s = scene(json!({"type": "frame", "name": "P", "width": 200, "height": 100, "justifyContent": "center", "alignItems": "end",
        "children": [{"type": "rectangle", "name": "A", "width": 20, "height": 20}]}));
    assert_eq!(rect(&s, "A"), [90.0, 80.0, 20.0, 20.0]);
}

#[test]
fn absolute_and_disabled_children_leave_the_flow() {
    let s = scene(json!({"type": "frame", "name": "P", "gap": 10,
        "children": [{"type": "rectangle", "name": "A", "width": 20, "height": 20},
                     {"type": "rectangle", "name": "Abs", "layoutPosition": "absolute", "x": 5, "y": 6, "width": 99, "height": 99},
                     {"type": "rectangle", "name": "Off", "enabled": false, "width": 50, "height": 50},
                     {"type": "rectangle", "name": "B", "width": 20, "height": 20}]}));
    assert_eq!(rect(&s, "P"), [0.0, 0.0, 50.0, 20.0]);
    assert_eq!(rect(&s, "Abs"), [5.0, 6.0, 99.0, 99.0]);
    assert_eq!(rect(&s, "B"), [30.0, 0.0, 20.0, 20.0]);
}

#[test]
fn instances_apply_root_and_descendant_overrides() {
    let doc = Document::parse(
        &json!({"version": "2.17", "children": [
            {"type": "frame", "id": "comp", "name": "Button", "reusable": true, "padding": 8, "fill": "#111111",
             "children": [{"type": "text", "id": "label", "name": "Label", "content": "OK", "fill": "#ffffff"}]},
            {"type": "ref", "id": "inst", "ref": "comp", "fill": "#ff0000", "descendants": {"label": {"content": "Cancel"}}}
        ]})
        .to_string(),
    )
    .unwrap();
    let vars = Vars::new(&doc);
    let mut s = compile_root(&doc, &vars, doc.roots[1]);
    layout(&mut s, &mut Mono);
    let root = &s.nodes[0];
    assert_eq!(root.rect.w, 6.0 * 10.0 + 16.0);
    assert_eq!(root.fills.len(), 1);
    let label = &s.nodes[1];
    assert_eq!(label.path.as_deref(), Some("inst/label"));
    match &label.kind {
        Kind::Text(t) => assert_eq!(t.content, "Cancel"),
        _ => panic!("expected text"),
    }
}

#[test]
fn undo_restores_the_exact_file() {
    let text = json!({"version": "2.17", "children": [{"type": "frame", "id": "a", "name": "A", "children": []}]}).to_string();
    let mut doc = Document::parse(&text).unwrap();
    let before = doc.serialize();
    doc.begin();
    let a = doc.find("a").unwrap();
    let b = doc.insert(Some(a), None, json!({"type": "rectangle", "name": "B"})).unwrap();
    let mut patch = pen::Props::new();
    patch.insert("name".into(), json!("Renamed"));
    doc.update(a, &patch);
    doc.delete(b);
    let log = doc.commit();
    assert_ne!(doc.serialize(), before);
    doc.undo(log);
    assert_eq!(doc.serialize(), before);
}
