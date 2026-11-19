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
    laid_doc(json!({"version": "2.17", "children": children}))
}

fn laid_doc(doc: Value) -> (Document, HashMap<NodeId, RootScene>) {
    let doc = Document::parse(&doc.to_string()).unwrap();
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
    let d = compare(&Version { doc: &a_doc, vars: &Vars::new(&a_doc), scenes: &a_scenes }, &Version { doc: &b_doc, vars: &Vars::new(&b_doc), scenes: &b_scenes });

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
    let d = compare(&Version { doc: &a_doc, vars: &Vars::new(&a_doc), scenes: &a_scenes }, &Version { doc: &b_doc, vars: &Vars::new(&b_doc), scenes: &b_scenes });
    let moved: Vec<&str> = d.changed.iter().filter(|c| c.moved).map(|c| c.id.as_str()).collect();
    assert_eq!(moved, ["a"]);
    // Everything shifted, so every box changed.
    assert_eq!(d.changed.len(), 4);

    let same = compare(&Version { doc: &a_doc, vars: &Vars::new(&a_doc), scenes: &a_scenes }, &Version { doc: &a_doc, vars: &Vars::new(&a_doc), scenes: &a_scenes });
    assert!(same.added.is_empty() && same.removed.is_empty() && same.changed.is_empty() && same.frames.is_empty());
}

/// Compares two whole documents.
fn diff(a: Value, b: Value) -> pen::diff::Diff {
    let (a_doc, a_scenes) = laid_doc(a);
    let (b_doc, b_scenes) = laid_doc(b);
    compare(&Version { doc: &a_doc, vars: &Vars::new(&a_doc), scenes: &a_scenes }, &Version { doc: &b_doc, vars: &Vars::new(&b_doc), scenes: &b_scenes })
}

fn props_of<'d>(d: &'d pen::diff::Diff, id: &str) -> Option<&'d [String]> {
    d.changed.iter().find(|c| c.id == id).map(|c| c.props.as_slice())
}

/// A Button component filled with `$accent`, and a card with a Cancel instance (its own fill)
/// and a Save instance (no overrides).
fn buttons(accent: &str, cancel: &str, spare: &str) -> Value {
    json!({"version": "2.17",
        "variables": {"accent": {"type": "color", "value": accent}, "spare": {"type": "color", "value": spare}},
        "children": [
            {"type": "frame", "id": "button", "reusable": true, "width": 80, "height": 30, "fill": "$accent"},
            {"type": "frame", "id": "card", "x": 200, "layout": "vertical", "children": [
                {"type": "ref", "id": "cancel", "ref": "button", "fill": cancel},
                {"type": "ref", "id": "save", "ref": "button"}
            ]}
        ]})
}

#[test]
fn a_changed_variable_reports_the_nodes_that_use_it() {
    let d = diff(buttons("#4F46E5", "#9CA3AF", "#000000"), buttons("#0D9488", "#64748B", "#000000"));
    assert_eq!(props_of(&d, "button"), Some(&["fill".to_string()][..]), "the component");
    assert_eq!(props_of(&d, "save"), Some(&["fill".to_string()][..]), "an instance that inherits the fill");
    assert_eq!(props_of(&d, "cancel"), Some(&["fill".to_string()][..]));
    assert_eq!(d.changed.len(), 3, "{:?}", d.changed);
    assert_eq!(d.frames, ["button", "card"]);

    // Only the variable changed: still reported.
    let d = diff(buttons("#4F46E5", "#9CA3AF", "#000000"), buttons("#0D9488", "#9CA3AF", "#000000"));
    assert_eq!(d.changed.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["button", "save"]);
}

#[test]
fn a_variable_nothing_uses_reports_nothing() {
    let d = diff(buttons("#4F46E5", "#9CA3AF", "#000000"), buttons("#4F46E5", "#9CA3AF", "#FFFFFF"));
    assert_eq!(d, pen::diff::Diff::default());
}

#[test]
fn the_same_value_spelled_differently_is_no_change() {
    let doc = |var: &str, fill: &str, stroke: &str| {
        json!({"version": "2.17", "variables": {var: {"type": "color", "value": "#4F46E5"}},
            "children": [{"type": "rectangle", "id": "r", "width": 10, "height": 10, "fill": fill,
                "stroke": {"thickness": 1, "fill": stroke}}]})
    };
    // A renamed variable with the same value, and a colour written shorter.
    let d = diff(doc("accent", "$accent", "#ffffff"), doc("brand", "$brand", "#FFF"));
    assert_eq!(d, pen::diff::Diff::default());
}

#[test]
fn a_change_to_one_theme_reports_the_nodes_shown_in_it() {
    let doc = |dark: &str| {
        json!({"version": "2.17", "themes": {"mode": ["light", "dark"]},
            "variables": {"surface": {"type": "color", "value": [{"value": "#FFFFFF"}, {"value": dark, "theme": {"mode": "dark"}}]}},
            "children": [
                {"type": "frame", "id": "light", "width": 50, "height": 50, "fill": "$surface"},
                {"type": "frame", "id": "dark", "x": 100, "width": 50, "height": 50, "theme": {"mode": "dark"},
                 "children": [{"type": "rectangle", "id": "inner", "width": 10, "height": 10, "fill": "$surface"}]}
            ]})
    };
    let d = diff(doc("#111111"), doc("#222222"));
    assert_eq!(d.changed.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["inner"]);
    assert_eq!(d.frames, ["dark"]);
}

#[test]
fn a_renamed_layer_is_listed_but_not_visible() {
    let doc = |name: &str| json!({"version": "2.17", "children": [{"type": "rectangle", "id": "r", "name": name, "width": 10, "height": 10}]});
    let d = diff(doc("Box"), doc("Card"));
    assert_eq!(props_of(&d, "r"), Some(&["name".to_string()][..]));
    assert!(!d.changed[0].visible());
}
