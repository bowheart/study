//! Prints a compact outline of a node subtree: `tree <file.pen> <root name or id> [depth]`
use serde_json::Value;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let doc = pen::Document::load(std::path::Path::new(&a[1])).unwrap();
    let depth: usize = a.get(3).and_then(|d| d.parse().ok()).unwrap_or(3);
    let mut target = doc.find(&a[2]);
    if target.is_none() { doc.walk(|n, _| if target.is_none() && doc.node(n).name() == Some(&a[2]) { target = Some(n) }); }
    let t = target.expect("not found");
    fn show(doc: &pen::Document, n: pen::NodeId, d: usize, max: usize) {
        let node = doc.node(n);
        let props: Vec<String> = node.props.iter().filter(|(k, _)| !matches!(k.as_str(), "children" | "type" | "id" | "name" | "content" | "geometry" | "descendants"))
            .map(|(k, v)| format!("{k}={}", match v { Value::String(s) => s.clone(), o => { let s = o.to_string(); if s.len() > 40 { format!("{}…", &s[..40]) } else { s } } })).collect();
        println!("{}{} {} '{}' {}", "  ".repeat(d), node.ty(), node.id(), node.name().unwrap_or(""), props.join(" "));
        if d < max { for &c in &node.children { show(doc, c, d + 1, max); } } else if !node.children.is_empty() { println!("{}  … {} children", "  ".repeat(d), node.children.len()); }
    }
    show(&doc, t, 0, depth);
}
