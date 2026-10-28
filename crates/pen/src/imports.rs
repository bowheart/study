//! `imports`: other `.pen` files a document pulls components and variables from.
//!
//! The header maps short aliases to URIs relative to the file (`{"ui": "kit.lib.pen"}`), or to
//! Pen's built-in libraries (`pencil:shadcn.lib.pen`), which are read from a local Pen install.
//! Pen's exact reference syntax isn't documented; study accepts `alias:id` for refs and
//! `$alias:name` for variables, and falls back to a bare id/name that exists in an import.
//! Nested references inside an imported component are rewritten to `alias:...` so they keep
//! resolving against the right file.

use crate::doc::Document;
use crate::Props;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
pub struct Imports {
    /// `alias:id` → the node's JSON with nested refs/variables namespaced.
    nodes: HashMap<String, Value>,
    /// Bare id → `alias:id`, for refs that don't name their import.
    bare: HashMap<String, String>,
    /// `alias:name` → variable definition.
    pub variables: Props,
    /// Imports that could not be loaded, with the reason.
    pub errors: Vec<String>,
}

impl Imports {
    pub fn load(doc: &Document) -> Imports {
        let mut out = Imports::default();
        let Some(Value::Object(list)) = doc.header.get("imports") else { return out };
        let base = doc.path.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
        for (alias, uri) in list {
            let Some(uri) = uri.as_str() else { continue };
            match load_uri(&base, uri) {
                Ok(d) => out.add(alias, &d),
                Err(e) => out.errors.push(format!("import '{alias}' ({uri}): {e}")),
            }
        }
        out
    }

    fn add(&mut self, alias: &str, d: &Document) {
        let mut stack: Vec<_> = d.roots.clone();
        while let Some(n) = stack.pop() {
            let node = d.node(n);
            let id = node.id().to_string();
            let mut v = d.to_value(n);
            namespace(&mut v, alias);
            let key = format!("{alias}:{id}");
            self.bare.entry(id).or_insert_with(|| key.clone());
            self.nodes.insert(key, v);
            stack.extend(node.children.iter().copied());
        }
        if let Some(vars) = d.variables() {
            for (name, def) in vars {
                let mut def = def.clone();
                namespace_values(&mut def, alias);
                self.variables.insert(format!("{alias}:{name}"), def);
            }
        }
    }

    /// The imported node a ref target names, if any.
    pub fn node(&self, target: &str) -> Option<&Value> {
        self.nodes.get(target).or_else(|| self.bare.get(target).and_then(|k| self.nodes.get(k)))
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.variables.is_empty()
    }
}

/// Prefixes refs and variable references inside an imported subtree with its alias.
fn namespace(v: &mut Value, alias: &str) {
    if let Value::Object(o) = v {
        if o.get("type").and_then(Value::as_str) == Some("ref") {
            if let Some(Value::String(r)) = o.get_mut("ref") {
                if !r.contains(':') {
                    *r = format!("{alias}:{r}");
                }
            }
        }
    }
    namespace_values(v, alias);
}

fn namespace_values(v: &mut Value, alias: &str) {
    match v {
        Value::String(s) if s.starts_with('$') && !s.contains(':') => *s = format!("${alias}:{}", &s[1..]),
        Value::Array(a) => a.iter_mut().for_each(|x| namespace(x, alias)),
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                // Descendant override keys are ids inside the component, not references.
                if k != "ref" {
                    namespace(x, alias);
                }
            }
        }
        _ => {}
    }
}

fn load_uri(base: &Path, uri: &str) -> Result<Document, String> {
    if let Some(name) = uri.strip_prefix("pencil:") {
        let bytes = pen_library(name).ok_or("Pen's built-in libraries need a local Pen install")?;
        let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        return Document::parse(&text).map_err(|e| e.to_string());
    }
    let rel: String = uri.split('/').map(percent_decode).collect::<Vec<_>>().join("/");
    let path = base.join(rel);
    Document::load(&path).map_err(|e| format!("{e:#}"))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Finds one of Pen's bundled `.pen` libraries inside its `app.asar`.
fn pen_library(name: &str) -> Option<Vec<u8>> {
    let stem = name.trim_end_matches(".pen").trim_end_matches(".lib");
    let candidates = [name.to_string(), format!("pencil-{stem}.pen"), format!("{stem}.pen")];
    pen_asars().iter().find_map(|asar| candidates.iter().find_map(|c| asar_file(asar, c)))
}

fn pen_asars() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        v.push(PathBuf::from(local).join("Programs/Pen/resources/app.asar"));
    }
    v.push(PathBuf::from("/Applications/Pen.app/Contents/Resources/app.asar"));
    if let Some(home) = std::env::var_os("HOME") {
        v.push(PathBuf::from(home).join("Applications/Pen.app/Contents/Resources/app.asar"));
    }
    v.into_iter().filter(|p| p.exists()).collect()
}

/// Reads a file (by name, anywhere in the archive) from an Electron asar archive.
fn asar_file(asar: &Path, name: &str) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(asar).ok()?;
    let mut head = [0u8; 16];
    f.read_exact(&mut head).ok()?;
    let header_size = u32::from_le_bytes(head[4..8].try_into().ok()?) as u64;
    let json_len = u32::from_le_bytes(head[12..16].try_into().ok()?) as usize;
    let mut json = vec![0u8; json_len];
    f.read_exact(&mut json).ok()?;
    let tree: Value = serde_json::from_slice(&json).ok()?;
    let (rel, entry) = find_entry(&tree, name, String::new())?;
    if entry.get("unpacked").and_then(Value::as_bool) == Some(true) {
        return std::fs::read(asar.with_extension("asar.unpacked").join(rel)).ok();
    }
    let offset: u64 = entry.get("offset")?.as_str()?.parse().ok()?;
    let size = entry.get("size")?.as_u64()? as usize;
    f.seek(SeekFrom::Start(8 + header_size + offset)).ok()?;
    let mut buf = vec![0u8; size];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn find_entry<'a>(dir: &'a Value, name: &str, prefix: String) -> Option<(String, &'a Map<String, Value>)> {
    let files = dir.get("files")?.as_object()?;
    for (k, v) in files {
        let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}/{k}") };
        if v.get("files").is_some() {
            if let Some(found) = find_entry(v, name, path) {
                return Some(found);
            }
        } else if k == name {
            return Some((path, v.as_object()?));
        }
    }
    None
}
