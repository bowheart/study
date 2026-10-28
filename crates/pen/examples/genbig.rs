//! Writes a large synthetic .pen file for performance testing: `genbig <out.pen> <roots>`
use serde_json::json;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let roots: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(400);
    let mut id = 0u64;
    let mut next = || { id += 1; format!("n{id}") };
    let words = ["Revenue", "Orders", "Customers", "Churn", "Latency", "Uptime", "Sessions", "Bounce"];
    let mut children = Vec::new();
    for r in 0..roots {
        let mut rows = Vec::new();
        for row in 0..50 {
            let mut cells = Vec::new();
            for c in 0..20 {
                let mut kids = vec![json!({"type": "rectangle", "id": next(), "name": "Swatch", "width": 12, "height": 12, "cornerRadius": 3,
                    "fill": format!("#{:02X}{:02X}{:02X}", (r * 7 + c * 11) % 256, (row * 5) % 256, 180)})];
                if c % 4 == 0 {
                    kids.push(json!({"type": "text", "id": next(), "name": "Label", "content": format!("{} {}", words[(row + c) % 8], c), "fontFamily": "Inter", "fontSize": 11, "fill": "#333333"}));
                }
                cells.push(json!({"type": "frame", "id": next(), "name": "Cell", "width": "fill_container", "gap": 6, "padding": [6, 8], "alignItems": "center", "fill": "#FFFFFF", "cornerRadius": 4, "children": kids}));
            }
            rows.push(json!({"type": "frame", "id": next(), "name": format!("Row {row}"), "width": "fill_container", "gap": 4, "children": cells}));
        }
        children.push(json!({"type": "frame", "id": next(), "name": format!("Board {r}"), "x": (r % 20) * 1700, "y": (r / 20) * 1500,
            "width": 1600, "layout": "vertical", "gap": 4, "padding": 16, "fill": "#F3F4F6", "clip": true, "children": rows}));
    }
    let doc = json!({"version": "2.17", "children": children});
    std::fs::write(&a[1], serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    println!("{} nodes", id);
}
