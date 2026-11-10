//! `script` nodes, evaluated with QuickJS. The wrapping and result format are shared with the web
//! viewer in `pen::script`.

use rquickjs::{Context, Runtime};
use serde_json::{Map, Value};
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
        pen::access::check_read(file)?;
        let src = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
        let job = pen::script::prepare(&src, w, h, inputs).map_err(|e| format!("{}: {e}", file.display()))?;
        let key = job.key;
        if let Some(r) = self.cache.get(&key) {
            return r.clone();
        }
        let r = self.eval(job.code).map_err(|e| format!("{}: {e}", file.display()));
        if self.cache.len() > 512 {
            self.cache.clear();
        }
        self.cache.insert(key, r.clone());
        r
    }

    fn eval(&mut self, code: String) -> Result<Vec<Value>, String> {
        let ctx = Context::full(&self.rt).map_err(|e| e.to_string())?;
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
        pen::script::parse_output(&r?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
