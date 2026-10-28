//! Survey which node types / properties / value shapes a set of .pen files use.
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Default)]
struct Stats {
    types: BTreeMap<String, usize>,
    props: BTreeMap<String, BTreeMap<String, usize>>,
    shapes: BTreeMap<String, usize>,
    key_orders: BTreeMap<String, BTreeMap<String, usize>>,
    nodes: usize,
    max_depth: usize,
}

fn shape(v: &Value) -> String {
    match v {
        Value::String(s) if s.starts_with('$') => "var".into(),
        Value::String(s) => {
            if s.starts_with('#') { "color".into() }
            else if s.starts_with("fit_content") || s.starts_with("fill_container") { s.split('(').next().unwrap().into() }
            else { "str".into() }
        }
        Value::Number(_) => "num".into(),
        Value::Bool(_) => "bool".into(),
        Value::Array(a) => format!("[{}]", a.iter().map(shape).collect::<Vec<_>>().join(",")),
        Value::Object(o) => match o.get("type") {
            Some(Value::String(t)) => format!("{{{t}}}"),
            _ => "{}".into(),
        },
        Value::Null => "null".into(),
    }
}

fn walk(v: &Value, s: &mut Stats, depth: usize) {
    let Some(o) = v.as_object() else { return };
    let t = o.get("type").and_then(|t| t.as_str()).unwrap_or("?").to_string();
    s.nodes += 1;
    s.max_depth = s.max_depth.max(depth);
    *s.types.entry(t.clone()).or_default() += 1;
    let order: Vec<&str> = o.keys().map(|k| k.as_str()).filter(|k| *k != "children").collect();
    *s.key_orders.entry(t.clone()).or_default().entry(order.join(",")).or_default() += 1;
    for (k, val) in o {
        *s.props.entry(t.clone()).or_default().entry(k.clone()).or_default() += 1;
        if k != "children" && k != "id" && k != "name" && k != "content" && k != "geometry" && k != "descendants" {
            let sh = shape(val);
            let sh = if sh.len() > 80 { sh[..80].to_string() } else { sh };
            *s.shapes.entry(format!("{t}.{k}: {sh}")).or_default() += 1;
        }
        if k == "descendants" {
            if let Some(d) = val.as_object() {
                for (_, ov) in d {
                    if let Some(ovo) = ov.as_object() {
                        for (ok, ovv) in ovo {
                            *s.shapes.entry(format!("descendants.{ok}: {}", shape(ovv))).or_default() += 1;
                        }
                        if ovo.contains_key("type") { walk(ov, s, depth + 1); }
                    }
                }
            }
        }
    }
    if let Some(Value::Array(c)) = o.get("children") {
        for ch in c { walk(ch, s, depth + 1); }
    }
}

fn main() {
    let mut s = Stats::default();
    let mut top: BTreeMap<String, usize> = BTreeMap::new();
    for path in std::env::args().skip(1) {
        let text = std::fs::read_to_string(&path).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        let o = doc.as_object().unwrap();
        for k in o.keys() { *top.entry(k.clone()).or_default() += 1; }
        if let Some(Value::Array(c)) = o.get("children") { for ch in c { walk(ch, &mut s, 1); } }
        if let Some(t) = o.get("themes") { println!("themes {path}: {t}"); }
        if let Some(i) = o.get("imports") { println!("imports {path}: {i}"); }
    }
    println!("nodes {} max_depth {}\ntop-level keys {:?}\ntypes {:?}", s.nodes, s.max_depth, top, s.types);
    for (t, p) in &s.props { println!("\n== {t}: {:?}", p); }
    println!("\n== shapes");
    for (k, n) in &s.shapes { println!("{n:7} {k}"); }
    println!("\n== top key orders");
    for (t, m) in &s.key_orders {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        for (o, n) in v.iter().take(3) { println!("{t} {n}: {o}"); }
    }
}
