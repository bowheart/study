//! `script` nodes (Pen's scripting): the code to evaluate for a node and how to read its result.
//! The desktop app evaluates it with QuickJS (`crates/study/src/scripts.rs`) and the web viewer in
//! a sandboxed worker (`crates/web`). Both run this same wrapped code, so the output, including
//! the seeded `Math.random`, is identical.
//!
//! A script starts with `/** @schema 2.11 */`, declares inputs as `@input name: type [= default]`,
//! reads `pencil.width`, `pencil.height` and `pencil.input.<name>`, and returns an array of nodes.
//! The seed differs from Pen's, so random-based scripts won't match Pen's output exactly.

use serde_json::{json, Map, Value};
use std::hash::{Hash, Hasher};

/// A script ready to evaluate. `code` is an expression that evaluates to the JSON text of the
/// generated nodes; `key` identifies the run (source plus `pencil` values) for caching.
pub struct Job {
    pub code: String,
    pub key: u64,
}

/// Wraps script `src` for a node of size `w`×`h` with the node's `inputs`.
pub fn prepare(src: &str, w: f32, h: f32, inputs: &Map<String, Value>) -> Result<Job, String> {
    if !src.contains("@schema") {
        return Err("scripts must start with /** @schema 2.11 */".into());
    }
    let mut input = declared_inputs(src);
    for (k, v) in inputs {
        let coerced = match (input.get(k), v) {
            (Some(Value::Number(_)), Value::String(s)) => s.parse::<f64>().ok().map(crate::json::number).unwrap_or(v.clone()),
            _ => v.clone(),
        };
        input.insert(k.clone(), coerced);
    }
    let pencil = json!({"width": w, "height": h, "input": input});
    let code = format!(
        "(function() {{
  const pencil = {pencil};
  let seed = 0x2F6B3A1D >>> 0;
  Math.random = function() {{
    seed = (seed + 0x6D2B79F5) >>> 0;
    let t = seed;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }};
  const out = (function(pencil) {{
{src}
  }})(pencil);
  return JSON.stringify(out === undefined ? [] : out);
}})()"
    );
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    code.hash(&mut hasher);
    Ok(Job { code, key: hasher.finish() })
}

/// The nodes a script returned, from the JSON text its code evaluated to.
pub fn parse_output(text: &str) -> Result<Vec<Value>, String> {
    match serde_json::from_str::<Value>(text).map_err(|e| e.to_string())? {
        Value::Array(a) => Ok(a),
        Value::Object(o) => Ok(vec![Value::Object(o)]),
        other => Err(format!("a script must return an array of nodes, got {other}")),
    }
}

/// `@input name: type [= default]` declarations, as default values.
fn declared_inputs(src: &str) -> Map<String, Value> {
    let mut out = Map::new();
    for line in src.lines() {
        let Some(rest) = line.split("@input").nth(1) else { continue };
        let Some((name, rest)) = rest.split_once(':') else { continue };
        let (ty, default) = match rest.split_once('=') {
            Some((t, d)) => (t.trim(), Some(d.trim().trim_end_matches("*/").trim())),
            None => (rest.trim(), None),
        };
        let ty = ty.split('(').next().unwrap_or(ty).trim();
        let unquote = |s: &str| s.trim_matches('"').trim_matches('\'').to_string();
        let v = match (ty, default) {
            ("number", Some(d)) => d.parse::<f64>().map(crate::json::number).unwrap_or(json!(0)),
            ("number", None) => json!(0),
            ("boolean", Some(d)) => json!(d == "true"),
            ("boolean", None) => json!(false),
            (_, Some(d)) => json!(unquote(d)),
            (_, None) => json!(""),
        };
        out.insert(name.trim().to_string(), v);
    }
    out
}
