//! Imports, including the imports of imported files.

use pen::imports::Imports;
use pen::scene::{compile_root_with, Kind, RootScene};
use pen::style::Paint;
use pen::vars::Vars;
use pen::Document;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// A scratch folder of `.pen` files, removed afterwards.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("pen-imports-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    fn write(&self, rel: &str, v: Value) -> PathBuf {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, v.to_string()).unwrap();
        p
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Compiles every top-level node of `path` with its imports.
fn compile(path: &Path) -> (Vec<RootScene>, Imports) {
    let doc = Document::load(path).unwrap();
    let imports = Imports::load(&doc);
    let mut vars = Vars::new(&doc);
    vars.add_imported(&imports.variables);
    let scenes = doc.roots.iter().map(|&r| compile_root_with(&doc, &vars, r, Some(&imports))).collect();
    (scenes, imports)
}

fn solid(s: &RootScene, i: usize) -> Option<[f32; 4]> {
    s.nodes[i].fills.iter().find_map(|p| match p {
        Paint::Solid { color, .. } => Some(color.0),
        _ => None,
    })
}

fn has_missing(s: &RootScene) -> bool {
    s.nodes.iter().any(|n| matches!(n.kind, Kind::Missing))
}

const BRAND: [f32; 4] = [0xE1 as f32 / 255.0, 0x1D as f32 / 255.0, 0x48 as f32 / 255.0, 1.0];

fn close(a: Option<[f32; 4]>, b: [f32; 4]) -> bool {
    a.is_some_and(|a| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-3))
}

fn tokens() -> Value {
    json!({"version": "2.6",
        "variables": {"brand": {"type": "color", "value": "#E11D48"}},
        "children": [{"type": "frame", "id": "swatch", "reusable": true, "width": 40, "height": 40, "fill": "#E11D48"}]})
}

fn kit(tokens_uri: &str) -> Value {
    json!({"version": "2.6", "imports": {"base": tokens_uri},
        "children": [
            {"type": "frame", "id": "viavar", "reusable": true, "width": 40, "height": 40, "fill": "$base:brand"},
            {"type": "frame", "id": "viaref", "reusable": true, "width": 60, "height": 60, "fill": "#FFFFFF",
             "children": [{"type": "ref", "id": "s", "ref": "base:swatch"}]},
            {"type": "frame", "id": "bare", "reusable": true, "width": 40, "height": 40, "fill": "$brand",
             "children": [{"type": "ref", "id": "s2", "ref": "swatch"}]}
        ]})
}

fn app(kit_uri: &str) -> Value {
    json!({"version": "2.6", "imports": {"ui": kit_uri},
        "children": [
            {"type": "ref", "id": "a", "ref": "ui:viavar"},
            {"type": "ref", "id": "b", "ref": "ui:viaref"},
            {"type": "ref", "id": "c", "ref": "ui:bare"}
        ]})
}

#[test]
fn a_librarys_own_imports_resolve_in_its_scope() {
    let d = Dir::new("nested");
    d.write("lib/tokens.lib.pen", tokens());
    d.write("lib/kit.lib.pen", kit("tokens.lib.pen"));
    let app = d.write("app.pen", app("lib/kit.lib.pen"));
    let (scenes, imports) = compile(&app);
    assert!(imports.errors.is_empty(), "{:?}", imports.errors);

    // A variable of the library's import.
    assert!(close(solid(&scenes[0], 0), BRAND), "viavar: {:?}", solid(&scenes[0], 0));
    // A component of the library's import, inside a component of the library.
    assert!(!has_missing(&scenes[1]));
    assert_eq!(scenes[1].nodes.len(), 2);
    assert!(close(solid(&scenes[1], 1), BRAND));
    // Bare names in the library that live in its import.
    assert!(!has_missing(&scenes[2]));
    assert!(close(solid(&scenes[2], 0), BRAND));
    assert!(close(solid(&scenes[2], 1), BRAND));
}

#[test]
fn shared_and_cyclic_imports_load_once() {
    let d = Dir::new("cycle");
    // tokens imports kit back, and the app imports tokens directly too.
    let mut t = tokens();
    t["imports"] = json!({"back": "kit.lib.pen", "home": "app.pen"});
    d.write("tokens.lib.pen", t);
    d.write("kit.lib.pen", kit("tokens.lib.pen"));
    let mut a = app("kit.lib.pen");
    a["imports"]["t"] = json!("./tokens.lib.pen");
    let app = d.write("app.pen", a);
    let (scenes, imports) = compile(&app);
    assert!(imports.errors.is_empty(), "{:?}", imports.errors);
    assert!(close(solid(&scenes[0], 0), BRAND));
    assert!(!has_missing(&scenes[1]));
    // tokens was first reached as the app's own `t`, so kit's `base` is that same scope.
    assert!(imports.node("t:swatch").is_some());
    assert!(imports.node("ui:base:swatch").is_none());
    assert!(imports.variables.contains_key("t:brand"));
}

#[test]
fn unresolved_imports_are_reported() {
    let d = Dir::new("missing");
    d.write("kit.lib.pen", kit("nowhere.lib.pen"));
    let mut a = app("kit.lib.pen");
    a["children"].as_array_mut().unwrap().push(json!({"type": "ref", "id": "x", "ref": "ui:stray"}));
    let app = d.write("app.pen", a);
    let mut k = kit("nowhere.lib.pen");
    k["children"].as_array_mut().unwrap().push(json!({"type": "frame", "id": "stray", "reusable": true, "fill": "$other:color"}));
    d.write("kit.lib.pen", k);
    let (scenes, imports) = compile(&app);
    assert!(has_missing(&scenes[1]));
    assert!(imports.errors.iter().any(|e| e == "import 'ui' → 'base' (nowhere.lib.pen): not found"), "{:?}", imports.errors);
    assert!(imports.errors.iter().any(|e| e == "import 'ui': uses $other:color, but has no import 'other'"), "{:?}", imports.errors);
}

#[test]
fn a_document_importing_one_file_twice() {
    let d = Dir::new("twice");
    d.write("tokens.lib.pen", tokens());
    let app = d.write(
        "app.pen",
        json!({"version": "2.6", "imports": {"a": "tokens.lib.pen", "b": "tokens.lib.pen"},
            "children": [{"type": "ref", "id": "x", "ref": "b:swatch"}, {"type": "frame", "id": "y", "fill": "$b:brand"}]}),
    );
    let (scenes, imports) = compile(&app);
    assert!(imports.errors.is_empty(), "{:?}", imports.errors);
    assert!(!has_missing(&scenes[0]));
    assert!(close(solid(&scenes[1], 0), BRAND));
}
