//! JSON-level instance expansion (for `Get` with `resolveInstances`) and
//! instance-path helpers. Mirrors the merge rules in `scene.rs`.

use crate::{Document, Props};
use serde_json::{Map, Value};
use std::collections::HashMap;

type Overrides = HashMap<String, Vec<Value>>;

/// Expands every `ref` inside `v` into the subtree it renders as. Expanded nodes get
/// instance-path ids (`instanceId/childId`) that Update/Replace accept.
pub fn expand(doc: &Document, v: &Value) -> Value {
    expand_rec(doc, v, None, &mut Vec::new())
}

fn expand_rec(doc: &Document, v: &Value, inst: Option<(&str, &Overrides)>, stack: &mut Vec<String>) -> Value {
    let Some(src) = v.as_object() else { return v.clone() };
    let mut obj = src.clone();
    let own_id = obj.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    if let Some((_, ovs)) = inst {
        if let Some(list) = ovs.get(&own_id) {
            for ov in list {
                let Some(o) = ov.as_object() else { continue };
                if o.contains_key("type") {
                    obj = o.clone();
                } else {
                    for (k, val) in o {
                        if k != "id" {
                            obj.insert(k.clone(), val.clone());
                        }
                    }
                }
            }
        }
    }
    let path = match inst {
        Some((prefix, _)) => format!("{prefix}{own_id}"),
        None => own_id.clone(),
    };

    if obj.get("type").and_then(Value::as_str) == Some("ref") {
        let target = obj.get("ref").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(comp) = doc.find(&target).filter(|_| !stack.contains(&target)) else {
            obj.insert("id".into(), Value::String(path));
            return Value::Object(obj);
        };
        let comp_val = doc.to_value(comp);
        let mut merged = comp_val.as_object().cloned().unwrap_or_default();
        for (k, val) in &obj {
            if matches!(k.as_str(), "type" | "ref" | "descendants" | "reusable" | "children") {
                continue;
            }
            merged.insert(k.clone(), val.clone());
        }
        merged.shift_remove("reusable");
        let mut overrides: Overrides = HashMap::new();
        if let Some(Value::Object(d)) = obj.get("descendants") {
            for (k, val) in d {
                overrides.entry(k.clone()).or_default().push(val.clone());
            }
        }
        if let Some((_, outer)) = inst {
            let pre = format!("{own_id}/");
            for (k, list) in outer {
                if let Some(rest) = k.strip_prefix(&pre) {
                    overrides.entry(rest.to_string()).or_default().extend(list.iter().cloned());
                }
            }
        }
        let kids = obj.get("children").or_else(|| comp_val.get("children")).cloned();
        merged.insert("id".into(), Value::String(path.clone()));
        let prefix = format!("{path}/");
        stack.push(target);
        if let Some(Value::Array(list)) = kids {
            let out: Vec<Value> = list.iter().map(|c| expand_rec(doc, c, Some((&prefix, &overrides)), stack)).collect();
            merged.insert("children".into(), Value::Array(out));
        }
        stack.pop();
        return Value::Object(merged);
    }

    if inst.is_some() {
        obj.insert("id".into(), Value::String(path));
    }
    if let Some(Value::Array(list)) = obj.get("children").cloned() {
        let out: Vec<Value> = list.iter().map(|c| expand_rec(doc, c, inst, stack)).collect();
        obj.insert("children".into(), Value::Array(out));
    }
    Value::Object(obj)
}

/// Finds the data a node at `rest` (path inside an instance) currently renders with.
pub fn find_in_expanded<'a>(tree: &'a Value, full_path: &str) -> Option<&'a Value> {
    if tree.get("id").and_then(Value::as_str) == Some(full_path) {
        return Some(tree);
    }
    for c in tree.get("children").and_then(Value::as_array)? {
        if let Some(found) = find_in_expanded(c, full_path) {
            return Some(found);
        }
    }
    None
}

/// Applies `descendants`-style overrides to a plain (non-instance) subtree, matching
/// keys by node id or unique name. Used by Copy.
pub fn apply_descendants(tree: &mut Value, overrides: &Map<String, Value>) -> Vec<String> {
    let mut missing = Vec::new();
    for (key, ov) in overrides {
        let target = find_mut_by_id(tree, key).is_some();
        let by_name = if target { None } else { unique_name_id(tree, key) };
        let id = if target { Some(key.clone()) } else { by_name };
        let Some(id) = id else {
            missing.push(key.clone());
            continue;
        };
        let node = find_mut_by_id(tree, &id).unwrap();
        let Some(o) = ov.as_object() else { continue };
        if o.contains_key("type") {
            *node = Value::Object(o.clone());
        } else if let Some(n) = node.as_object_mut() {
            for (k, v) in o {
                if k != "id" {
                    n.insert(k.clone(), v.clone());
                }
            }
        }
    }
    missing
}

fn find_mut_by_id<'a>(v: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    if v.get("id").and_then(Value::as_str) == Some(id) {
        return Some(v);
    }
    for c in v.get_mut("children")?.as_array_mut()? {
        if let Some(f) = find_mut_by_id(c, id) {
            return Some(f);
        }
    }
    None
}

fn unique_name_id(v: &Value, name: &str) -> Option<String> {
    let mut found = Vec::new();
    collect_named(v, name, &mut found);
    (found.len() == 1).then(|| found.remove(0))
}

fn collect_named(v: &Value, name: &str, out: &mut Vec<String>) {
    if v.get("name").and_then(Value::as_str) == Some(name) {
        if let Some(id) = v.get("id").and_then(Value::as_str) {
            out.push(id.to_string());
        }
    }
    if let Some(list) = v.get("children").and_then(Value::as_array) {
        for c in list {
            collect_named(c, name, out);
        }
    }
}

/// Resolves descendant keys given by unique name inside a component into ids, so
/// `descendants` maps on refs may use names (as Copy allows).
pub fn names_to_ids(doc: &Document, component: &str, descendants: &Props) -> Props {
    let Some(comp) = doc.find(component) else { return descendants.clone() };
    let tree = expand(doc, &doc.to_value(comp));
    let mut out = Props::new();
    // Inside the expanded component, plain descendants keep their ids and nested
    // instance children are `nestedId/childId` — exactly the keys `descendants` uses.
    for (k, v) in descendants {
        if find_in_expanded(&tree, k).is_none() {
            if let Some(id) = unique_name_id(&tree, k) {
                out.insert(id, v.clone());
                continue;
            }
        }
        out.insert(k.clone(), v.clone());
    }
    out
}
