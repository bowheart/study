use pen::diff::{compare, Version};
use pen::layout::{layout, TextMeasure};
use pen::scene::{compile_root, RootScene};
use pen::style::TextData;
use pen::vars::Vars;
use pen::{Document, NodeId};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Every character is 10×20.
struct Mono;
impl TextMeasure for Mono {
    fn measure(&mut self, t: &TextData, _width: Option<f32>) -> [f32; 2] {
        [t.content.chars().count() as f32 * 10.0, 20.0]
    }
}

fn laid(children: Value) -> (Document, HashMap<NodeId, RootScene>) {
    let doc = Document::parse(&json!({"version": "2.17", "children": children}).to_string()).unwrap();
    let vars = Vars::new(&doc);
    let scenes = doc
        .roots
        .iter()
        .map(|&r| {
            let mut s = compile_root(&doc, &vars, r);
            layout(&mut s, &mut Mono);
            (r, s)
        })
        .collect();
    (doc, scenes)
}

fn page(cards: Value, label: &str) -> Value {
    json!([
        {"type": "frame", "id": "page", "layout": "vertical", "children": cards},
        {"type": "frame", "id": "comp", "reusable": true, "x": 500, "children": [{"type": "text", "id": "label", "content": label}]},
        {"type": "ref", "id": "inst", "ref": "comp", "x": 800}
    ])
}

#[test]
fn reports_props_reflow_components_and_structure() {
    let (a_doc, a_scenes) = laid(page(
        json!([
            {"type": "rectangle", "id": "A", "width": 100, "height": 30},
            {"type": "frame", "id": "B", "width": 100, "height": 30, "children": [{"type": "rectangle", "id": "kid", "width": 10, "height": 10}]},
            {"type": "frame", "id": "gone", "width": 10, "height": 10, "children": [{"type": "rectangle", "id": "inner", "width": 5, "height": 5}]}
        ]),
        "OK",
    ));
    let (b_doc, b_scenes) = laid(page(
        json!([
            {"type": "rectangle", "id": "A", "width": 100, "height": 50},
            {"type": "frame", "id": "B", "width": 100, "height": 30, "children": [{"type": "rectangle", "id": "kid", "width": 10, "height": 10}]},
            {"type": "rectangle", "id": "C", "width": 100, "height": 30}
        ]),
        "Go!",
    ));
    let d = compare(&Version { doc: &a_doc, scenes: &a_scenes }, &Version { doc: &b_doc, scenes: &b_scenes });

    assert_eq!(d.added, ["C"]);
    assert_eq!(d.removed, ["gone"], "only the outermost removed node");
    let change = |id: &str| d.changed.iter().find(|c| c.id == id).unwrap_or_else(|| panic!("{id} not reported"));
    assert_eq!(change("A").props, ["height"]);
    // B didn't change, but A pushed it down.
    assert!(change("B").props.is_empty() && !change("B").moved);
    let (before, after) = change("B").bounds.unwrap();
    assert_eq!((before.y, after.y), (30.0, 50.0));
    // Its child moved with it, not within it: not reported.
    assert!(d.changed.iter().all(|c| c.id != "kid"));
    // The component's text changed, and so did the instance that renders it.
    assert_eq!(change("label").props, ["content"]);
    assert_eq!(change("inst/label").props, ["content"]);
    assert!(change("inst/label").bounds.is_some());
    assert_eq!(d.frames, ["page", "comp", "inst"]);
}

#[test]
fn reordering_marks_only_what_moved() {
    let row = |ids: &[&str]| json!([{"type": "frame", "id": "row", "children": ids.iter().map(|id| json!({"type": "rectangle", "id": id, "width": 10, "height": 10})).collect::<Vec<_>>()}]);
    let (a_doc, a_scenes) = laid(row(&["a", "b", "c", "d"]));
    let (b_doc, b_scenes) = laid(row(&["b", "c", "d", "a"]));
    let d = compare(&Version { doc: &a_doc, scenes: &a_scenes }, &Version { doc: &b_doc, scenes: &b_scenes });
    let moved: Vec<&str> = d.changed.iter().filter(|c| c.moved).map(|c| c.id.as_str()).collect();
    assert_eq!(moved, ["a"]);
    // Everything shifted, so every box changed.
    assert_eq!(d.changed.len(), 4);

    let same = compare(&Version { doc: &a_doc, scenes: &a_scenes }, &Version { doc: &a_doc, scenes: &a_scenes });
    assert!(same.added.is_empty() && same.removed.is_empty() && same.changed.is_empty() && same.frames.is_empty());
}
