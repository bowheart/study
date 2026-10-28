//! `script` nodes: JavaScript files that generate a node's children (Pen's scripting).
//!
//! A script starts with `/** @schema 2.11 */`, declares inputs as `@input name: type [= default]`,
//! reads `pencil.width`, `pencil.height` and `pencil.input.<name>`, and returns an array of nodes.
//! `Math.random()` is a seeded generator so output is stable between renders (the seed differs
//! from Pen's, so random-based scripts won't match Pen's output exactly).

use rquickjs::{Context, Runtime};
use serde_json::{json, Map, Value};
use std::cell::Cell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

const TIME_LIMIT: Duration = Duration::from_secs(5);

pub struct Scripts {
    rt: Runtime,
    deadline: Rc<Cell<Option<Instant>>>,
    cache: HashMap<u64, Result<Vec<Value>, String>>,
}

impl Scripts {
    pub fn new() -> Option<Scripts> {
        let rt = Runtime::new().ok()?;
        rt.set_memory_limit(256 << 20);
        let deadline: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let d = deadline.clone();
        rt.set_interrupt_handler(Some(Box::new(move || d.get().is_some_and(|t| Instant::now() > t))));
        Some(Scripts { rt, deadline, cache: HashMap::new() })
    }

    /// Runs a script for a node of size `w`×`h` with the node's `inputs`.
    pub fn run(&mut self, file: &Path, w: f32, h: f32, inputs: &Map<String, Value>) -> Result<Vec<Value>, String> {
        let src = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        if !src.contains("@schema") {
            return Err(format!("{}: scripts must start with /** @schema 2.11 */", file.display()));
        }
        let mut input = declared_inputs(&src);
        for (k, v) in inputs {
            let coerced = match (input.get(k), v) {
                (Some(Value::Number(_)), Value::String(s)) => s.parse::<f64>().ok().map(pen::json::number).unwrap_or(v.clone()),
                _ => v.clone(),
            };
            input.insert(k.clone(), coerced);
        }
        let pencil = json!({"width": w, "height": h, "input": input});
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            src.hash(&mut hasher);
            pencil.to_string().hash(&mut hasher);
            hasher.finish()
        };
        if let Some(r) = self.cache.get(&key) {
            return r.clone();
        }
        let r = self.eval(&src, &pencil).map_err(|e| format!("{}: {e}", file.display()));
        if self.cache.len() > 512 {
            self.cache.clear();
        }
        self.cache.insert(key, r.clone());
        r
    }

    fn eval(&mut self, src: &str, pencil: &Value) -> Result<Vec<Value>, String> {
        let ctx = Context::full(&self.rt).map_err(|e| e.to_string())?;
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
        self.deadline.set(Some(Instant::now() + TIME_LIMIT));
        let r: Result<String, String> = ctx.with(|ctx| {
            ctx.eval::<String, _>(code).map_err(|e| match e {
                rquickjs::Error::Exception => {
                    let ex = ctx.catch();
                    ex.as_object().and_then(|o| o.get::<_, String>("message").ok()).unwrap_or_else(|| format!("{ex:?}"))
                }
                e => e.to_string(),
            })
        });
        self.deadline.set(None);
        let text = r?;
        match serde_json::from_str::<Value>(&text).map_err(|e| e.to_string())? {
            Value::Array(a) => Ok(a),
            Value::Object(o) => Ok(vec![Value::Object(o)]),
            other => Err(format!("a script must return an array of nodes, got {other}")),
        }
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
            ("number", Some(d)) => d.parse::<f64>().map(pen::json::number).unwrap_or(json!(0)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_generate_nodes_from_inputs() {
        let dir = std::env::temp_dir().join(format!("study-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("bars.js");
        std::fs::write(
            &file,
            "/**\n * @schema 2.11\n * @input rows: number = 3\n * @input color: color = #3B82F6\n */\n\
             const out = [];\n\
             for (let r = 0; r < pencil.input.rows; r++) out.push({type: 'rectangle', y: r * 10, width: pencil.width, height: 8, fill: pencil.input.color, opacity: Math.random()});\n\
             return out;",
        )
        .unwrap();
        let mut s = Scripts::new().unwrap();
        let mut inputs = Map::new();
        inputs.insert("rows".into(), json!(2));
        let a = s.run(&file, 100.0, 50.0, &inputs).unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[1]["y"], 10);
        assert_eq!(a[0]["fill"], "#3B82F6");
        assert_eq!(a[0]["width"], 100);
        // Deterministic random: a fresh runner gives the same output.
        let b = Scripts::new().unwrap().run(&file, 100.0, 50.0, &inputs).unwrap();
        assert_eq!(a, b);
        std::fs::remove_dir_all(&dir).ok();
    }
}
