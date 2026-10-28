//! The `execute` tool: runs a JavaScript snippet against the document.
//!
//! Each document keeps one QuickJS context so plain globals (`id = Insert(...)`)
//! persist between calls. A failed call rolls back every document change and
//! removes globals it created; the snippet is kept under an `editId` so it can
//! be patched with `edits` and re-run.

use crate::engine::Engine;
use base64::Engine as _;
use pen::instance::{apply_descendants, expand, find_in_expanded, names_to_ids};
use pen::scene::Rect;
use pen::validate;
use pen::vars::{apply_theme, Theme};
use pen::{NodeId, Props};
use rquickjs::{Context, Function, Runtime};
use serde_json::{json, Map, Value};
use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

const PRELUDE: &str = include_str!("prelude.js");
const TIME_LIMIT: Duration = Duration::from_secs(120);

pub struct Js {
    _rt: Runtime,
    ctx: Context,
}

thread_local! {
    static HOST: Cell<*mut Engine> = const { Cell::new(std::ptr::null_mut()) };
    static STATE: RefCell<ExecState> = RefCell::new(ExecState::default());
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

#[derive(Default)]
struct ExecState {
    prints: Vec<String>,
    warnings: Vec<String>,
    created: Vec<String>,
    /// Nodes read with Get, for the window's scan animation.
    reads: Vec<String>,
    screenshots: Vec<String>,
    exports: Vec<(Vec<String>, String, String, Value)>,
}

pub struct Edit {
    pub find: String,
    pub replace: String,
    pub all: bool,
}

pub struct ExecResult {
    pub text: String,
    pub images: Vec<Vec<u8>>,
    pub ok: bool,
}

impl Js {
    fn new() -> anyhow::Result<Js> {
        let rt = Runtime::new()?;
        rt.set_memory_limit(1 << 30);
        rt.set_interrupt_handler(Some(Box::new(|| DEADLINE.with(|d| d.get().is_some_and(|t| Instant::now() > t)))));
        let ctx = Context::full(&rt)?;
        ctx.with(|ctx| -> rquickjs::Result<()> {
            let host = Function::new(ctx.clone(), |name: String, args: String| -> String { host_call(&name, &args) })?;
            ctx.globals().set("__host", host)?;
            ctx.eval::<(), _>(PRELUDE)?;
            Ok(())
        })?;
        Ok(Js { _rt: rt, ctx })
    }
}

pub fn execute(engine: &mut Engine, input: Option<String>, edit_id: Option<String>, edits: Vec<Edit>) -> ExecResult {
    let (snippet, edit_id) = match (&edit_id, input) {
        (Some(id), _) => {
            let Some(mut code) = engine.failed.get(id).cloned() else {
                return fail_plain(format!("Unknown editId '{id}'. Only ids from failed execute calls can be patched; send the snippet in `input` instead."));
            };
            for (k, e) in edits.iter().enumerate() {
                let count = code.matches(&e.find).count();
                if count == 0 {
                    return fail_plain(format!("Edit {} failed: `find` text was not found in snippet {id}. Current snippet:\n{code}", k + 1));
                }
                if count > 1 && !e.all {
                    return fail_plain(format!("Edit {} failed: `find` matches {count} times in snippet {id}; make it unique or set `all: true`.", k + 1));
                }
                code = if e.all { code.replace(&e.find, &e.replace) } else { code.replacen(&e.find, &e.replace, 1) };
            }
            (code, Some(id.clone()))
        }
        (None, Some(code)) => (code, None),
        (None, None) => return fail_plain("Provide the JavaScript snippet in `input` (or `editId` + `edits` to patch a failed call).".into()),
    };

    engine.refresh();
    if engine.js.is_none() {
        match Js::new() {
            Ok(js) => engine.js = Some(js),
            Err(e) => return fail_plain(format!("Could not start the JavaScript engine: {e}")),
        }
    }
    STATE.with(|s| *s.borrow_mut() = ExecState::default());
    HOST.with(|h| h.set(engine as *mut Engine));
    DEADLINE.with(|d| d.set(Some(Instant::now() + TIME_LIMIT)));
    engine.doc.begin();

    let wrapped = format!("(function(){{\n{snippet}\n}})();");
    let ctx = engine.js.as_ref().unwrap().ctx.clone();
    let outcome: Result<(), String> = ctx.with(|ctx| {
        let before: Vec<String> = ctx.eval("Object.getOwnPropertyNames(globalThis)").unwrap_or_default();
        // Sloppy mode, so `id = Insert(...)` creates a global that persists across calls.
        let mut opts = rquickjs::context::EvalOptions::default();
        opts.strict = false;
        opts.global = true;
        opts.filename = Some("snippet".into());
        let r = ctx.eval_with_options::<(), _>(wrapped.as_str(), opts);
        match r {
            Ok(()) => Ok(()),
            Err(e) => {
                let msg = describe_error(&ctx, e);
                // Drop globals the failed snippet created.
                let after: Vec<String> = ctx.eval("Object.getOwnPropertyNames(globalThis)").unwrap_or_default();
                let g = ctx.globals();
                for name in after.iter().filter(|n| !before.contains(n)) {
                    let _ = g.remove(name.as_str());
                }
                Err(msg)
            }
        }
    });

    DEADLINE.with(|d| d.set(None));
    HOST.with(|h| h.set(std::ptr::null_mut()));
    let st = STATE.with(|s| std::mem::take(&mut *s.borrow_mut()));

    match outcome {
        Err(msg) => {
            engine.doc.rollback();
            engine.refresh();
            let id = edit_id.unwrap_or_else(|| engine.new_edit_id());
            engine.failed.insert(id.clone(), snippet);
            let mut text = format!("Error: {msg}\n\nAll changes from this call were reverted. editId: {id}\nFix the snippet by calling execute with editId \"{id}\" and `edits` (find/replace) instead of resending it.");
            if !st.warnings.is_empty() {
                text.push_str("\n\nWarnings:\n");
                text.push_str(&st.warnings.join("\n"));
            }
            ExecResult { text, images: vec![], ok: false }
        }
        Ok(()) => {
            if let Some(id) = &edit_id {
                engine.failed.remove(id);
            }
            let log = engine.doc.commit();
            let changed = !log.is_empty();
            if changed {
                engine.undo.push(log);
                engine.redo.clear();
            }
            engine.refresh();
            let mut out = String::new();
            if changed {
                if let Err(e) = engine.save() {
                    out.push_str(&format!("Warning: could not save the file: {e}\n"));
                }
            }
            if !st.prints.is_empty() {
                out.push_str(&st.prints.join("\n"));
                out.push('\n');
            }
            engine.fresh_paths = fresh_paths(engine, &st.created);
            engine.fresh_reads = st.reads.clone();
            let names = created_names(engine, &st.created);
            if !names.is_empty() {
                out.push_str("\nNodes (name = id):\n");
                out.push_str(&names);
            }
            let mut images = Vec::new();
            if !st.screenshots.is_empty() || !st.exports.is_empty() {
                engine.settle();
            }
            for id in &st.screenshots {
                match crate::shot::screenshot(engine, id) {
                    Ok(png) => images.push(png),
                    Err(e) => out.push_str(&format!("\nScreenshot of {id} failed: {e}")),
                }
            }
            for (ids, format, path, opts) in &st.exports {
                match crate::export::export(engine, ids, format, path, opts) {
                    Ok(files) => {
                        out.push_str("\nExported:\n");
                        for f in files {
                            out.push_str(&format!("{f}\n"));
                        }
                    }
                    Err(e) => out.push_str(&format!("\nExport failed: {e}\n")),
                }
            }
            if !st.warnings.is_empty() {
                out.push_str("\nWarnings (fix these in the next execute call):\n");
                let mut seen = std::collections::HashSet::new();
                for w in st.warnings.iter().filter(|w| seen.insert(w.as_str())).take(60) {
                    out.push_str(&format!("- {w}\n"));
                }
            }
            if out.trim().is_empty() {
                out = if changed { "Done.".into() } else { "Done (no changes).".into() };
            }
            ExecResult { text: out.trim_end().to_string(), images, ok: true }
        }
    }
}

fn fail_plain(text: String) -> ExecResult {
    ExecResult { text, images: vec![], ok: false }
}

fn describe_error(ctx: &rquickjs::Ctx, e: rquickjs::Error) -> String {
    if let rquickjs::Error::Exception = e {
        let ex = ctx.catch();
        if let Some(obj) = ex.as_object() {
            let msg: String = obj.get("message").unwrap_or_default();
            let stack: String = obj.get("stack").unwrap_or_default();
            // The snippet is wrapped in one extra line; report the user's line number.
            let line = stack
                .lines()
                .find_map(|l| l.split("snippet:").nth(1))
                .and_then(|s| s.split(':').next())
                .and_then(|n| n.trim_end_matches(')').parse::<i64>().ok())
                .map(|n| n - 1);
            return match line {
                Some(l) if l > 0 => format!("{msg} (line {l})"),
                _ => msg,
            };
        }
        if let Some(s) = ex.as_string() {
            return s.to_string().unwrap_or_default();
        }
        return format!("{ex:?}");
    }
    if DEADLINE.with(|d| d.get().is_some_and(|t| Instant::now() > t)) {
        return format!("Timed out after {}s", TIME_LIMIT.as_secs());
    }
    e.to_string()
}

/// Path nodes inside everything this call created, in document order (for the draw-in animation).
fn fresh_paths(engine: &Engine, created: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for id in created {
        let Some(n) = engine.doc.find(id) else { continue };
        let mut stack = vec![n];
        while let Some(n) = stack.pop() {
            if !seen.insert(n) || out.len() >= 4000 {
                continue;
            }
            let node = engine.doc.node(n);
            if node.ty() == "path" {
                out.push(node.id().to_string());
            }
            stack.extend(node.children.iter().rev());
        }
    }
    out
}

fn created_names(engine: &Engine, created: &[String]) -> String {
    let mut out = String::new();
    let mut lines = 0;
    let ids: std::collections::HashSet<NodeId> = created.iter().filter_map(|id| engine.doc.find(id)).collect();
    for id in created {
        let Some(n) = engine.doc.find(id) else { continue };
        // Children inserted in the same call are listed under their parent.
        let mut up = engine.doc.node(n).parent;
        let mut nested = false;
        while let Some(p) = up {
            nested |= ids.contains(&p);
            up = engine.doc.node(p).parent;
        }
        if nested {
            continue;
        }
        let mut stack = vec![(n, 0usize)];
        while let Some((n, d)) = stack.pop() {
            if lines >= 300 {
                out.push_str("…\n");
                return out;
            }
            let node = engine.doc.node(n);
            out.push_str(&format!("{}{}={}\n", "  ".repeat(d), node.name().unwrap_or(node.ty()), node.id()));
            lines += 1;
            for &c in node.children.iter().rev() {
                stack.push((c, d + 1));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------- host bridge

fn host_call(name: &str, args: &str) -> String {
    let ptr = HOST.with(|h| h.get());
    if ptr.is_null() {
        return json!({"__error": "no document"}).to_string();
    }
    // Safety: set only for the duration of `execute`, on this thread, while the engine is borrowed mutably.
    let engine = unsafe { &mut *ptr };
    let args: Vec<Value> = match serde_json::from_str::<Value>(args) {
        Ok(Value::Array(a)) => a,
        _ => vec![],
    };
    let r = STATE.with(|s| {
        let mut st = s.borrow_mut();
        let mut h = Host { e: engine, st: &mut st };
        h.call(name, &args)
    });
    match r {
        Ok(v) => json!({"__value": v}).to_string(),
        Err(e) => json!({"__error": e}).to_string(),
    }
}

enum Target {
    Document,
    Doc(NodeId),
    Inst { inst: NodeId, rest: String },
}

struct Host<'a> {
    e: &'a mut Engine,
    st: &'a mut ExecState,
}

type R = Result<Value, String>;

fn arg<'a>(args: &'a [Value], i: usize) -> &'a Value {
    args.get(i).unwrap_or(&Value::Null)
}

fn str_arg(args: &[Value], i: usize, what: &str) -> Result<String, String> {
    match arg(args, i) {
        Value::String(s) => Ok(s.clone()),
        Value::Object(o) if o.contains_key("id") => Err(format!("{what} must be an id string, not a node object; pass node.id")),
        v => Err(format!("{what} must be an id string, got {v}")),
    }
}

impl Host<'_> {
    fn call(&mut self, name: &str, a: &[Value]) -> R {
        match name {
            "Insert" => self.insert(a),
            "Copy" => self.copy(a),
            "Update" => self.update(a),
            "Replace" => self.replace(a),
            "Move" => self.move_(a),
            "Delete" => self.delete(a),
            "Generate" => self.generate(a),
            "SetVariables" => self.set_variables(a),
            "GetVariables" => Ok(json!({
                "variables": self.e.doc.variables().cloned().unwrap_or_default(),
                "themes": self.e.doc.themes().cloned().unwrap_or_default(),
            })),
            "GetNode" => self.get_node(a),
            "GetTree" => self.get_tree(a),
            "FindEmptySpace" => self.find_empty_space(a),
            "Print" => {
                self.st.prints.push(arg(a, 0).as_str().unwrap_or("").to_string());
                Ok(Value::Null)
            }
            "TakeScreenshot" => {
                let ids = id_list(arg(a, 0))?;
                for id in &ids {
                    if id != "document" {
                        self.target(id)?;
                    }
                }
                self.st.screenshots.extend(ids);
                Ok(Value::Null)
            }
            "Export" => {
                let ids = id_list(arg(a, 0))?;
                let format = str_arg(a, 1, "format")?;
                if !["png", "jpeg", "webp", "pdf", "html-tailwind", "html-css"].contains(&format.as_str()) {
                    return Err(format!("unknown export format '{format}' (png, jpeg, webp, pdf, html-tailwind, html-css)"));
                }
                let path = str_arg(a, 2, "outputPath")?;
                self.st.exports.push((ids, format, path, arg(a, 3).clone()));
                Ok(Value::Null)
            }
            _ => Err(format!("unknown function {name}")),
        }
    }

    fn target(&self, path: &str) -> Result<Target, String> {
        if path == "document" {
            return Ok(Target::Document);
        }
        if let Some((first, rest)) = path.split_once('/') {
            let inst = self.e.doc.find(first).ok_or_else(|| format!("no node with id '{first}'"))?;
            if self.e.doc.node(inst).ty() != "ref" {
                return Err(format!("'{path}': slash paths only address children of component instances, and '{first}' is not an instance"));
            }
            let tree = expand(&self.e.doc, &self.e.doc.to_value(inst));
            if find_in_expanded(&tree, path).is_none() {
                return Err(format!("no node at instance path '{path}'"));
            }
            return Ok(Target::Inst { inst, rest: rest.to_string() });
        }
        let n = self.e.doc.find(path).ok_or_else(|| format!("no node with id '{path}'"))?;
        Ok(Target::Doc(n))
    }

    fn parent_target(&self, v: &Value) -> Result<Option<NodeId>, String> {
        let p = match v {
            Value::Null => return Ok(None),
            Value::String(s) => s.clone(),
            Value::Object(o) if o.contains_key("id") => return Err("parent must be an id string, not a node object; pass node.id".into()),
            other => return Err(format!("parent must be an id string, got {other}")),
        };
        match self.target(&p)? {
            Target::Document => Ok(None),
            Target::Doc(n) => {
                let node = self.e.doc.node(n);
                if !node.can_have_children() {
                    return Err(format!("'{p}' is a {} and cannot have children (only frames and groups can)", node.ty()));
                }
                Ok(Some(n))
            }
            Target::Inst { .. } => {
                Err(format!("cannot insert into '{p}' inside a component instance; use Replace(\"{p}\", ...) or `descendants` overrides"))
            }
        }
    }

    fn parent_layout(&self, parent: Option<NodeId>) -> Option<String> {
        let p = parent?;
        let node = self.e.doc.node(p);
        match node.ty() {
            "frame" => Some(node.get("layout").and_then(Value::as_str).unwrap_or("horizontal").to_string()),
            _ => Some("none".into()),
        }
    }

    fn check(&mut self, data: &Value, parent: Option<NodeId>, partial: bool) -> Result<(), String> {
        let layout = self.parent_layout(parent);
        let r = validate::check(data, layout.as_deref(), partial);
        self.st.warnings.extend(r.warnings);
        if r.errors.is_empty() {
            Ok(())
        } else {
            Err(r.errors.join("; "))
        }
    }

    /// Converts name-keyed `descendants` on refs into id keys.
    fn normalize_ref(&self, data: &mut Value) {
        let Some(obj) = data.as_object_mut() else { return };
        if obj.get("type").and_then(Value::as_str) == Some("ref") {
            let comp = obj.get("ref").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some(Value::Object(d)) = obj.get("descendants") {
                let fixed = names_to_ids(&self.e.doc, &comp, d);
                obj.insert("descendants".into(), Value::Object(fixed));
            }
        }
        if let Some(Value::Array(kids)) = obj.get_mut("children") {
            for k in kids {
                self.normalize_ref(k);
            }
        }
    }

    fn insert(&mut self, a: &[Value]) -> R {
        let parent = self.parent_target(arg(a, 0))?;
        let mut data = arg(a, 1).clone();
        if !data.is_object() {
            return Err(format!("Insert: nodeData must be an object, got {data}"));
        }
        if let Some(r) = data.get("ref").and_then(Value::as_str) {
            if self.e.doc.find(r).is_none() {
                return Err(format!("Insert: ref '{r}' does not exist"));
            }
        }
        self.check(&data, parent, false)?;
        self.normalize_ref(&mut data);
        let id = self.e.doc.insert(parent, None, data).map_err(|e| e.to_string())?;
        let s = self.e.doc.node(id).id().to_string();
        self.st.created.push(s.clone());
        Ok(Value::String(s))
    }

    /// Only `svg` works locally: the caller writes the SVG markup itself and passes it as the prompt.
    fn generate(&mut self, a: &[Value]) -> R {
        let id = str_arg(a, 0, "Generate: nodeId")?;
        let kind = str_arg(a, 1, "Generate: type")?;
        let prompt = arg(a, 2).as_str().unwrap_or("").trim().to_string();
        if kind != "svg" {
            self.st.warnings.push(format!(
                "Generate \"{kind}\" is not available in study (no image or stock photo service); the node was left as is. Use existing image files in fills, gradients, or shapes instead."
            ));
            return Ok(Value::Null);
        }
        let Target::Doc(n) = self.target(&id)? else { return Err("Generate: nodeId must be a frame in the document".into()) };
        if self.e.doc.node(n).ty() != "frame" {
            return Err(format!("Generate: '{id}' is a {}; insert a frame with a width and height first", self.e.doc.node(n).ty()));
        }
        if !prompt.contains("<svg") {
            return Err("Generate \"svg\" in study has no generation service: you are the generator. Pass the complete SVG markup you drew as the prompt, e.g. Generate(frameId, \"svg\", `<svg viewBox=\"0 0 100 100\" xmlns=\"http://www.w3.org/2000/svg\">…</svg>`).".into());
        }
        self.e.refresh();
        let r = self.e.abs_rect(&id).ok_or("Generate: the frame has no layout yet")?;
        if r.w < 1.0 || r.h < 1.0 {
            return Err(format!("Generate: frame '{id}' is {}×{}; give it a fixed width and height", r.w, r.h));
        }
        let nodes = crate::svg::to_nodes(&prompt, r.w, r.h)?;
        if self.e.doc.node(n).get("layout").and_then(Value::as_str) != Some("none") {
            let mut patch = Props::new();
            patch.insert("layout".into(), json!("none"));
            self.e.doc.update(n, &patch);
        }
        for v in nodes {
            let c = self.e.doc.insert(Some(n), None, v).map_err(|e| e.to_string())?;
            self.st.created.push(self.e.doc.node(c).id().to_string());
        }
        Ok(Value::Null)
    }

    fn copy(&mut self, a: &[Value]) -> R {
        let src = str_arg(a, 0, "Copy: path")?;
        let parent = self.parent_target(arg(a, 1))?;
        let over = arg(a, 2).as_object().cloned().unwrap_or_default();
        let descendants = over.get("descendants").and_then(Value::as_object).cloned();
        let mut root_over: Props = over.clone();
        root_over.shift_remove("descendants");
        root_over.shift_remove("id");
        root_over.shift_remove("children");

        let data = match self.target(&src)? {
            Target::Document => return Err("Copy: cannot copy the document".into()),
            Target::Doc(n) if self.e.doc.node(n).is_reusable() => {
                // Copying a component creates a connected instance.
                let mut o = Map::new();
                o.insert("type".into(), json!("ref"));
                o.insert("ref".into(), json!(self.e.doc.node(n).id()));
                for k in ["x", "y"] {
                    if let Some(v) = self.e.doc.node(n).get(k) {
                        o.insert(k.into(), v.clone());
                    }
                }
                for (k, v) in root_over {
                    o.insert(k, v);
                }
                if let Some(d) = descendants {
                    o.insert("descendants".into(), Value::Object(names_to_ids(&self.e.doc, self.e.doc.node(n).id(), &d)));
                }
                Value::Object(o)
            }
            Target::Doc(n) if self.e.doc.node(n).ty() == "ref" => {
                let mut v = self.e.doc.to_value(n);
                let o = v.as_object_mut().unwrap();
                for (k, val) in root_over {
                    o.insert(k, val);
                }
                if let Some(d) = descendants {
                    let comp = o.get("ref").and_then(Value::as_str).unwrap_or("").to_string();
                    let d = names_to_ids(&self.e.doc, &comp, &d);
                    let mut existing = o.get("descendants").and_then(Value::as_object).cloned().unwrap_or_default();
                    for (k, val) in d {
                        merge_override(&mut existing, &k, &val);
                    }
                    o.insert("descendants".into(), Value::Object(existing));
                }
                v
            }
            Target::Doc(n) => {
                let mut v = self.e.doc.to_value(n);
                if let Some(d) = &descendants {
                    let missing = apply_descendants(&mut v, d);
                    if !missing.is_empty() {
                        return Err(format!("Copy: no descendant with id or unique name {missing:?} under '{src}'"));
                    }
                }
                let o = v.as_object_mut().unwrap();
                o.shift_remove("reusable");
                for (k, val) in root_over {
                    o.insert(k, val);
                }
                v
            }
            Target::Inst { inst, .. } => {
                let tree = expand(&self.e.doc, &self.e.doc.to_value(inst));
                let mut v = find_in_expanded(&tree, &src).cloned().ok_or_else(|| format!("no node at '{src}'"))?;
                strip_paths(&mut v);
                let o = v.as_object_mut().unwrap();
                for (k, val) in root_over {
                    o.insert(k, val);
                }
                v
            }
        };
        self.check(&data, parent, false)?;
        let id = self.e.doc.insert(parent, None, data).map_err(|e| e.to_string())?;
        let s = self.e.doc.node(id).id().to_string();
        self.st.created.push(s.clone());
        Ok(Value::String(s))
    }

    fn update(&mut self, a: &[Value]) -> R {
        let path = str_arg(a, 0, "Update: path")?;
        let data = arg(a, 1).as_object().cloned().ok_or("Update: updateData must be an object")?;
        if data.contains_key("children") {
            return Err("Update cannot change children; use Replace (or Insert/Move/Delete)".into());
        }
        match self.target(&path)? {
            Target::Document => Err("Update: cannot update the document; use SetVariables for variables".into()),
            Target::Doc(n) => {
                let node = self.e.doc.node(n).clone();
                for k in ["id", "type", "ref"] {
                    if let Some(v) = data.get(k) {
                        if node.get(k) != Some(v) {
                            return Err(format!("Update cannot change `{k}`; use Replace to swap a node"));
                        }
                    }
                }
                let parent = node.parent;
                let mut merged = Value::Object(data.clone());
                if let Some(t) = node.get("type") {
                    merged.as_object_mut().unwrap().insert("type".into(), t.clone());
                }
                self.check(&merged, parent, true)?;
                let mut patch = data;
                if node.ty() == "ref" {
                    if let Some(Value::Object(d)) = patch.get("descendants").cloned() {
                        let comp = node.get("ref").and_then(Value::as_str).unwrap_or("").to_string();
                        let d = names_to_ids(&self.e.doc, &comp, &d);
                        let mut existing = node.get("descendants").and_then(Value::as_object).cloned().unwrap_or_default();
                        for (k, v) in d {
                            merge_override(&mut existing, &k, &v);
                        }
                        patch.insert("descendants".into(), Value::Object(existing));
                    }
                }
                self.e.doc.update(n, &patch);
                Ok(Value::Null)
            }
            Target::Inst { inst, rest } => {
                for k in ["id", "type", "ref"] {
                    if data.contains_key(k) {
                        return Err(format!("Update cannot change `{k}`; use Replace(\"{path}\", ...) to swap the node"));
                    }
                }
                self.check(&Value::Object(data.clone()), None, true)?;
                let node = self.e.doc.node(inst);
                let mut desc = node.get("descendants").and_then(Value::as_object).cloned().unwrap_or_default();
                merge_override(&mut desc, &rest, &Value::Object(data));
                let mut patch = Props::new();
                patch.insert("descendants".into(), Value::Object(desc));
                self.e.doc.update(inst, &patch);
                Ok(Value::Null)
            }
        }
    }

    fn replace(&mut self, a: &[Value]) -> R {
        let path = str_arg(a, 0, "Replace: path")?;
        let mut data = arg(a, 1).clone();
        if !data.is_object() {
            return Err("Replace: nodeData must be an object".into());
        }
        match self.target(&path)? {
            Target::Document => Err("Replace: cannot replace the document".into()),
            Target::Doc(n) => {
                let parent = self.e.doc.node(n).parent;
                let index = self.e.doc.index_in_parent(n);
                self.check(&data, parent, false)?;
                self.normalize_ref(&mut data);
                let id = self.e.doc.insert(parent, Some(index), data).map_err(|e| e.to_string())?;
                self.e.doc.delete(n);
                let s = self.e.doc.node(id).id().to_string();
                self.st.created.push(s.clone());
                Ok(Value::String(s))
            }
            Target::Inst { inst, rest } => {
                self.check(&data, None, false)?;
                self.normalize_ref(&mut data);
                // The replacement keeps the replaced node's id so its path stays stable.
                let old_id = rest.rsplit('/').next().unwrap_or(&rest).to_string();
                assign_ids(&mut data, &self.e.doc, Some(&old_id));
                let node = self.e.doc.node(inst);
                let mut desc = node.get("descendants").and_then(Value::as_object).cloned().unwrap_or_default();
                desc.insert(rest, data);
                let mut patch = Props::new();
                patch.insert("descendants".into(), Value::Object(desc));
                self.e.doc.update(inst, &patch);
                Ok(Value::String(path))
            }
        }
    }

    fn move_(&mut self, a: &[Value]) -> R {
        let path = str_arg(a, 0, "Move: path")?;
        let Target::Doc(n) = self.target(&path)? else {
            return Err("Move: only regular nodes can be moved (not the document or nodes inside instances)".into());
        };
        let parent = match arg(a, 1) {
            Value::Null => self.e.doc.node(n).parent,
            v => self.parent_target(v)?,
        };
        let index = arg(a, 2).as_f64().map(|i| i.max(0.0) as usize);
        self.e.doc.move_node(n, parent, index).map_err(|e| e.to_string())?;
        Ok(Value::Null)
    }

    fn delete(&mut self, a: &[Value]) -> R {
        let path = str_arg(a, 0, "Delete: path")?;
        match self.target(&path)? {
            Target::Doc(n) => {
                self.e.doc.delete(n);
                Ok(Value::Null)
            }
            Target::Inst { .. } => Err(format!(
                "cannot delete '{path}' inside a component instance; hide it with Update(\"{path}\", {{enabled: false}})"
            )),
            Target::Document => Err("Delete: cannot delete the document".into()),
        }
    }

    fn set_variables(&mut self, a: &[Value]) -> R {
        let vars = arg(a, 0).as_object().cloned().ok_or("SetVariables: expected an object of variable definitions")?;
        let replace = arg(a, 1).as_bool().unwrap_or(false);
        let mut header = self.e.doc.header.clone();
        let mut defs = if replace { Map::new() } else { header.get("variables").and_then(Value::as_object).cloned().unwrap_or_default() };
        let mut themes = header.get("themes").and_then(Value::as_object).cloned().unwrap_or_default();
        for (name, def) in vars {
            if name.starts_with('$') {
                return Err(format!("SetVariables: variable names must not start with '$' ('{name}'); use the bare name"));
            }
            let Some(o) = def.as_object() else {
                return Err(format!("SetVariables: '{name}' must be an object like {{type: \"color\", value: \"#000000\"}}"));
            };
            let ty = o.get("type").and_then(Value::as_str).unwrap_or("");
            if !["color", "number", "string", "boolean"].contains(&ty) {
                return Err(format!("SetVariables: '{name}' needs type color, number, string or boolean"));
            }
            let Some(value) = o.get("value") else { return Err(format!("SetVariables: '{name}' needs a value")) };
            if let Value::Array(entries) = value {
                for e in entries {
                    let Some(t) = e.get("theme").and_then(Value::as_object) else { continue };
                    for (axis, val) in t {
                        let Some(val) = val.as_str() else { continue };
                        let list = themes.entry(axis.clone()).or_insert_with(|| json!([]));
                        if let Some(arr) = list.as_array_mut() {
                            if !arr.iter().any(|x| x.as_str() == Some(val)) {
                                arr.push(json!(val));
                            }
                        }
                    }
                }
            }
            defs.insert(name, def);
        }
        insert_ordered(&mut header, "themes", Value::Object(themes));
        insert_ordered(&mut header, "variables", Value::Object(defs));
        if header.get("themes").and_then(Value::as_object).is_some_and(|t| t.is_empty()) {
            header.shift_remove("themes");
        }
        self.e.doc.set_header(header);
        Ok(Value::Null)
    }

    // ------------------------------------------------------------------ reading

    fn read_root(&self, path: &str, resolve_instances: bool) -> Result<(Value, Theme), String> {
        let (v, theme_from) = match self.target(path)? {
            Target::Document => return Err("internal: document read".into()),
            Target::Doc(n) => (self.e.doc.to_value(n), Some(n)),
            Target::Inst { inst, .. } => {
                let tree = expand(&self.e.doc, &self.e.doc.to_value(inst));
                (find_in_expanded(&tree, path).cloned().ok_or("no such node")?, Some(inst))
            }
        };
        let v = if resolve_instances { expand(&self.e.doc, &v) } else { v };
        // Theme in effect at this node: defaults plus every ancestor's `theme`.
        let mut theme = self.e.vars.default_theme.clone();
        if let Some(n) = theme_from {
            let mut chain = Vec::new();
            let mut cur = self.e.doc.node(n).parent;
            while let Some(c) = cur {
                chain.push(c);
                cur = self.e.doc.node(c).parent;
            }
            for c in chain.iter().rev() {
                if let Some(t) = apply_theme(&theme, self.e.doc.node(*c).get("theme")) {
                    theme = t;
                }
            }
        }
        Ok((v, theme))
    }

    fn get_node(&mut self, a: &[Value]) -> R {
        let path = str_arg(a, 0, "Get: path")?;
        self.st.reads.push(path.clone());
        let o = Opts::parse(arg(a, 1));
        let (v, theme) = self.read_root(&path, o.instances)?;
        Ok(self.shape_data(v, &theme, &o, 0))
    }

    /// Pure node data honoring depth / resolveVariables / includePathGeometry.
    fn shape_data(&self, v: Value, theme: &Theme, o: &Opts, depth: u32) -> Value {
        let Value::Object(mut obj) = v else { return v };
        let theme = apply_theme(theme, obj.get("theme")).unwrap_or_else(|| theme.clone());
        if !o.geometry && obj.get("type").and_then(Value::as_str) == Some("path") && obj.contains_key("geometry") {
            obj.insert("geometry".into(), json!("..."));
        }
        if o.variables {
            for (k, val) in obj.iter_mut() {
                if k != "children" && k != "descendants" {
                    *val = self.e.vars.resolve_deep(val, &theme);
                }
            }
        }
        if let Some(Value::Array(kids)) = obj.shift_remove("children") {
            if o.depth.is_some_and(|d| depth >= d) {
                obj.insert("children".into(), json!("..."));
            } else {
                let kids: Vec<Value> = kids.into_iter().map(|c| self.shape_data(c, &theme, o, depth + 1)).collect();
                obj.insert("children".into(), Value::Array(kids));
            }
        }
        Value::Object(obj)
    }

    fn get_tree(&mut self, a: &[Value]) -> R {
        self.e.refresh();
        let path = str_arg(a, 0, "Get: path")?;
        let o = Opts::parse(arg(a, 1));
        let roots: Vec<(Value, Theme)> = if path == "document" {
            let ids: Vec<String> = self.e.doc.roots.iter().map(|&r| self.e.doc.node(r).id().to_string()).collect();
            self.st.reads.extend(ids.iter().cloned());
            ids.iter().map(|id| self.read_root(id, o.instances)).collect::<Result<_, _>>()?
        } else {
            self.st.reads.push(path.clone());
            vec![self.read_root(&path, o.instances)?]
        };
        let out: Vec<Value> = roots.into_iter().map(|(v, t)| self.visit_tree(v, &t, &o, 0)).collect();
        Ok(Value::Array(out))
    }

    /// `{d: data, b: bounds, p: problems, c: children, e: elided}` for the JS visitor.
    fn visit_tree(&self, v: Value, theme: &Theme, o: &Opts, depth: u32) -> Value {
        let Value::Object(mut obj) = v else { return json!({"d": v}) };
        let kids = obj.shift_remove("children");
        let id = obj.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let theme2 = apply_theme(theme, obj.get("theme")).unwrap_or_else(|| theme.clone());
        let data = self.shape_data(Value::Object(obj), theme, &Opts { depth: Some(0), ..o.clone() }, 0);
        let mut out = Map::new();
        out.insert("d".into(), data);
        if let Some((s, i)) = self.e.locate(&id) {
            let r = s.nodes[i as usize].rect;
            out.insert("b".into(), json!([r.x, r.y, r.w, r.h]));
            if let Some(p) = pen::layout::clip_problem(s, i) {
                out.insert("p".into(), json!(p));
            }
        }
        if let Some(Value::Array(list)) = kids {
            if o.depth.is_some_and(|d| depth >= d) {
                out.insert("e".into(), json!(true));
            } else {
                let c: Vec<Value> = list.into_iter().map(|k| self.visit_tree(k, &theme2, o, depth + 1)).collect();
                out.insert("c".into(), Value::Array(c));
            }
        }
        Value::Object(out)
    }

    fn find_empty_space(&mut self, a: &[Value]) -> R {
        let i = arg(a, 0);
        let w = i.get("width").and_then(Value::as_f64).ok_or("FindEmptySpace: width is required")? as f32;
        let h = i.get("height").and_then(Value::as_f64).ok_or("FindEmptySpace: height is required")? as f32;
        let dir = i.get("direction").and_then(Value::as_str).unwrap_or("right").to_string();
        let pad = i.get("padding").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        let anchor = i.get("nodeId").and_then(Value::as_str).map(str::to_string);
        self.e.refresh();

        // Obstacles: siblings of the anchor (or all roots), in that parent's coordinates.
        let (parent_id, obstacles, anchor_rect): (Option<String>, Vec<Rect>, Option<Rect>) = match &anchor {
            Some(id) => {
                let Target::Doc(n) = self.target(id)? else { return Err("FindEmptySpace: nodeId must be a regular node".into()) };
                let parent = self.e.doc.node(n).parent;
                let rect_of = |k: NodeId| -> Option<Rect> {
                    let key = self.e.doc.node(k).id().to_string();
                    let (s, idx) = self.e.locate(&key)?;
                    Some(if parent.is_none() { s.bounds[idx as usize] } else { s.nodes[idx as usize].rect })
                };
                let obs = self.e.doc.children_of(parent).iter().filter_map(|&k| rect_of(k)).collect();
                (parent.map(|p| self.e.doc.node(p).id().to_string()), obs, rect_of(n))
            }
            None => (None, self.e.ordered_scenes().iter().filter_map(|s| s.bounds.first().copied()).filter(|b| b.w >= 0.0).collect(), None),
        };
        let all = obstacles.iter().copied().reduce(|a, b| a.union(&b));
        let Some(all) = all else { return Ok(json!({"x": 0, "y": 0})) };
        let base = anchor_rect.unwrap_or(all);
        let (mut x, mut y) = match dir.as_str() {
            "left" => (base.x - pad - w, base.y),
            "top" => (base.x, base.y - pad - h),
            "bottom" => (base.x, base.bottom() + pad),
            _ => (base.right() + pad, base.y),
        };
        // Slide further in the same direction until nothing overlaps.
        for _ in 0..10000 {
            let cand = Rect::new(x - pad, y - pad, w + 2.0 * pad, h + 2.0 * pad);
            let Some(hit) = obstacles.iter().find(|o| o.intersects(&cand)) else { break };
            match dir.as_str() {
                "left" => x = hit.x - pad - w,
                "top" => y = hit.y - pad - h,
                "bottom" => y = hit.bottom() + pad,
                _ => x = hit.right() + pad,
            }
        }
        x = x.round();
        y = y.round();
        let mut out = json!({"x": x, "y": y});
        if let Some(p) = parent_id {
            out["parentId"] = json!(p);
        }
        Ok(out)
    }
}

#[derive(Clone, Default)]
struct Opts {
    depth: Option<u32>,
    variables: bool,
    instances: bool,
    geometry: bool,
}

impl Opts {
    fn parse(v: &Value) -> Opts {
        Opts {
            depth: v.get("depth").and_then(Value::as_f64).map(|d| d.max(0.0) as u32),
            variables: v.get("resolveVariables").and_then(Value::as_bool).unwrap_or(false),
            instances: v.get("resolveInstances").and_then(Value::as_bool).unwrap_or(false),
            geometry: v.get("includePathGeometry").and_then(Value::as_bool).unwrap_or(false),
        }
    }
}

fn id_list(v: &Value) -> Result<Vec<String>, String> {
    match v {
        Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string).ok_or_else(|| format!("expected id strings, got {x}"))).collect(),
        Value::String(s) => Ok(vec![s.clone()]),
        _ => Err("expected an array of node ids".into()),
    }
}

/// Merges one `descendants` override into a map: property patches merge, replacements replace.
fn merge_override(desc: &mut Map<String, Value>, key: &str, patch: &Value) {
    let Some(p) = patch.as_object() else { return };
    if p.contains_key("type") {
        desc.insert(key.to_string(), patch.clone());
        return;
    }
    let entry = desc.entry(key.to_string()).or_insert_with(|| json!({}));
    if let Some(e) = entry.as_object_mut() {
        for (k, v) in p {
            if v.is_null() {
                e.shift_remove(k);
            } else {
                e.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Removes instance-path ids (with '/') so the subtree can be inserted as fresh nodes.
fn strip_paths(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        if o.get("id").and_then(Value::as_str).is_some_and(|s| s.contains('/')) {
            o.shift_remove("id");
        }
        if let Some(Value::Array(k)) = o.get_mut("children") {
            for c in k {
                strip_paths(c);
            }
        }
    }
}

/// Gives every node in an override subtree an id (the root may keep `root_id`).
fn assign_ids(v: &mut Value, doc: &pen::Document, root_id: Option<&str>) {
    if let Some(o) = v.as_object_mut() {
        let id = root_id.map(str::to_string).unwrap_or_else(|| doc.new_id());
        let mut out = Map::new();
        for (k, val) in std::mem::take(o) {
            let is_type = k == "type";
            if k == "id" {
                continue;
            }
            out.insert(k, val);
            if is_type {
                out.insert("id".into(), json!(id));
            }
        }
        *o = out;
        if let Some(Value::Array(k)) = o.get_mut("children") {
            for c in k {
                assign_ids(c, doc, None);
            }
        }
    }
}

/// Inserts a top-level key, keeping `children` last-ish like Pen's files (before `fileToken`).
fn insert_ordered(header: &mut Props, key: &str, v: Value) {
    if header.contains_key(key) {
        header.insert(key.into(), v);
        return;
    }
    let pos = header.keys().position(|k| k == "children").map(|i| i + 1).unwrap_or(header.len());
    let mut out = Props::new();
    for (i, (k, val)) in std::mem::take(header).into_iter().enumerate() {
        if i == pos {
            out.insert(key.into(), v.clone());
        }
        out.insert(k, val);
    }
    if !out.contains_key(key) {
        out.insert(key.into(), v);
    }
    *header = out;
}

pub fn png_base64(png: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(png)
}
