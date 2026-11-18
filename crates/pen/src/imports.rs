//! `imports`: other `.pen` files a document pulls components and variables from.
//!
//! The header maps short aliases to URIs relative to the file (`{"ui": "kit.lib.pen"}`), or to
//! Pen's built-in libraries (`pencil:shadcn.lib.pen`), which are read from a local Pen install.
//! Pen's exact reference syntax isn't documented; study accepts `alias:id` for refs and
//! `$alias:name` for variables, and falls back to a shorter name that exists in an import.
//!
//! Imported files' own imports are followed too, each relative to the file that lists it. Every
//! file has a scope prefix: none for the document itself, `ui:` for its import `ui`, `ui:base:` for
//! that file's import `base`. References inside an imported file are rewritten into the scopes of
//! its own imports, so `$base:brand` in `kit.lib.pen` becomes `$ui:base:brand`, and its bare
//! `swatch` becomes `ui:swatch`. A file reached twice (shared by two imports, or in a cycle) is
//! read once and keeps the scope it was first reached with.

use crate::doc::Document;
use crate::Props;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

#[derive(Default)]
pub struct Imports {
    /// `scope:id` → the node's JSON with nested refs/variables namespaced.
    nodes: HashMap<String, Value>,
    /// Shorter names (see [`short_names`]) → full key. The import reached first wins a shared name.
    bare: HashMap<String, String>,
    /// `scope:name` → variable definition.
    pub variables: Props,
    /// Imports that could not be loaded, and references to aliases a file doesn't import.
    pub errors: Vec<String>,
    /// Each imported file with its modified time when it was read (`None` if it wasn't there),
    /// so changes on disk can be noticed. Pen's built-in libraries aren't listed.
    files: Vec<(PathBuf, Option<SystemTime>)>,
}

/// An imported file waiting to be added.
struct Pending {
    /// Scope prefix of its nodes and variables, like `ui:base:`.
    prefix: String,
    /// How it was reached, for messages: `'ui' → 'base'`.
    chain: String,
    doc: Document,
}

/// A reference to an alias its file doesn't import. Reported unless it resolves anyway.
struct Stray {
    chain: String,
    alias: String,
    target: String,
    variable: bool,
}

/// What rewriting one file's references needs.
struct Scope<'a> {
    prefix: &'a str,
    /// The file's import aliases → their scope prefixes.
    aliases: &'a HashMap<String, String>,
    chain: &'a str,
    strays: &'a mut Vec<Stray>,
}

impl Imports {
    pub fn load(doc: &Document) -> Imports {
        let mut out = Imports::default();
        // Files read (or being read) → their scope prefix. The document itself is the empty scope,
        // so an import that comes back to it resolves to its own nodes.
        let mut scopes: HashMap<String, String> = HashMap::new();
        if let Some(p) = &doc.path {
            scopes.insert(file_key(p), String::new());
        }
        let (aliases, mut queue) = out.follow(doc, "", "", &mut scopes);
        let mut strays = Vec::new();
        // Breadth first, so the document's direct imports win shared short names.
        while let Some(p) = queue.pop_front() {
            let (aliases, next) = out.follow(&p.doc, &p.prefix, &p.chain, &mut scopes);
            queue.extend(next);
            out.add(&p, &aliases, &mut strays);
        }
        // The document's nodes aren't rewritten, so an alias of its that names a file already read
        // under another alias gets copies of that file's keys.
        for (alias, prefix) in &aliases {
            let own = format!("{alias}:");
            if *prefix != own && !prefix.is_empty() {
                out.alias_scope(prefix, &own);
            }
        }
        let mut seen = HashSet::new();
        for s in strays {
            let found = if s.variable { out.variables.keys().any(|k| k == &s.target || short_name_of(k, &s.target)) } else { out.node(&s.target).is_some() };
            let what = if s.variable { format!("${}", s.target) } else { s.target.clone() };
            if !found && seen.insert((s.chain.clone(), what.clone())) {
                out.errors.push(format!("import {}: uses {what}, but has no import '{}'", s.chain, s.alias));
            }
        }
        out
    }

    /// Resolves a file's imports: maps each alias to a scope, and reads the files not seen yet.
    fn follow(&mut self, doc: &Document, prefix: &str, chain: &str, scopes: &mut HashMap<String, String>) -> (HashMap<String, String>, VecDeque<Pending>) {
        let mut aliases = HashMap::new();
        let mut next = VecDeque::new();
        let Some(Value::Object(list)) = doc.header.get("imports") else { return (aliases, next) };
        let base = doc.path.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
        for (alias, uri) in list {
            let Some(uri) = uri.as_str() else { continue };
            let chain = if chain.is_empty() { format!("'{alias}'") } else { format!("{chain} → '{alias}'") };
            let path = (!uri.starts_with("pencil:")).then(|| resolve(&base, uri));
            let key = path.as_deref().map(file_key).unwrap_or_else(|| uri.to_string());
            if let Some(scope) = scopes.get(&key) {
                aliases.insert(alias.clone(), scope.clone());
                continue;
            }
            let scope = format!("{prefix}{alias}:");
            scopes.insert(key, scope.clone());
            aliases.insert(alias.clone(), scope.clone());
            match load_uri(path.as_deref(), uri, &mut self.files) {
                Ok(doc) => next.push_back(Pending { prefix: scope, chain, doc }),
                Err(e) => self.errors.push(format!("import {chain} ({uri}): {e}")),
            }
        }
        (aliases, next)
    }

    fn add(&mut self, p: &Pending, aliases: &HashMap<String, String>, strays: &mut Vec<Stray>) {
        let d = &p.doc;
        let mut scope = Scope { prefix: &p.prefix, aliases, chain: &p.chain, strays };
        let mut stack: Vec<_> = d.roots.clone();
        while let Some(n) = stack.pop() {
            let node = d.node(n);
            let mut v = d.to_value(n);
            namespace(&mut v, &mut scope);
            let key = format!("{}{}", p.prefix, node.id());
            for name in short_names(&key) {
                self.bare.entry(name).or_insert_with(|| key.clone());
            }
            self.nodes.insert(key, v);
            stack.extend(node.children.iter().copied());
        }
        if let Some(vars) = d.variables() {
            for (name, def) in vars {
                let mut def = def.clone();
                namespace_values(&mut def, &mut scope);
                self.variables.insert(format!("{}{name}", p.prefix), def);
            }
        }
    }

    /// Makes everything under scope `from` also reachable under scope `to`.
    fn alias_scope(&mut self, from: &str, to: &str) {
        let nodes: Vec<_> = self.nodes.iter().filter_map(|(k, v)| Some((format!("{to}{}", k.strip_prefix(from)?), v.clone()))).collect();
        self.nodes.extend(nodes);
        let vars: Vec<_> = self.variables.iter().filter_map(|(k, v)| Some((format!("{to}{}", k.strip_prefix(from)?), v.clone()))).collect();
        self.variables.extend(vars);
    }

    /// True if an imported file was modified, created or deleted since the imports were loaded.
    pub fn changed_on_disk(&self) -> bool {
        self.files.iter().any(|(path, at)| mtime(path) != *at)
    }

    /// The imported node a ref target names, if any.
    pub fn node(&self, target: &str) -> Option<&Value> {
        self.nodes.get(target).or_else(|| self.bare.get(target).and_then(|k| self.nodes.get(k)))
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.variables.is_empty()
    }
}

/// The shorter names a key `a:b:c:id` also answers to: its endings (`b:c:id`, `c:id`, `id`) and
/// its outer scopes with the id (`a:b:id`, `a:id`). A file that names a component or variable of
/// one of its imports without the alias still finds it.
fn short_names(key: &str) -> Vec<String> {
    let parts: Vec<&str> = key.split(':').collect();
    let (id, scope) = parts.split_last().expect("split yields one part");
    let mut out: Vec<String> = (1..parts.len()).map(|i| parts[i..].join(":")).collect();
    out.extend((1..scope.len()).map(|i| format!("{}:{id}", scope[..i].join(":"))));
    out
}

/// True if `name` is one of the [`short_names`] of `key`.
pub fn short_name_of(key: &str, name: &str) -> bool {
    if key.strip_suffix(name).is_some_and(|rest| rest.ends_with(':')) {
        return true;
    }
    let Some((scope, id)) = name.rsplit_once(':') else { return false };
    key.strip_prefix(scope).and_then(|r| r.strip_prefix(':')).and_then(|r| r.strip_suffix(id)).is_some_and(|mid| mid.ends_with(':'))
}

/// Rewrites references inside an imported subtree into the scopes of its file.
fn namespace(v: &mut Value, scope: &mut Scope) {
    if let Value::Object(o) = v {
        if o.get("type").and_then(Value::as_str) == Some("ref") {
            if let Some(Value::String(r)) = o.get_mut("ref") {
                *r = rescope(r, scope, false);
            }
        }
    }
    namespace_values(v, scope);
}

fn namespace_values(v: &mut Value, scope: &mut Scope) {
    match v {
        Value::String(s) if s.starts_with('$') && s.len() > 1 && !s.contains(char::is_whitespace) => *s = format!("${}", rescope(&s[1..], scope, true)),
        Value::Array(a) => a.iter_mut().for_each(|x| namespace(x, scope)),
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                // Descendant override keys are ids inside the component, not references.
                if k != "ref" {
                    namespace(x, scope);
                }
            }
        }
        _ => {}
    }
}

/// `alias:name` → the alias's scope + `name`; a bare `name` → the file's own scope + `name`.
fn rescope(target: &str, scope: &mut Scope, variable: bool) -> String {
    match target.split_once(':') {
        Some((alias, rest)) => match scope.aliases.get(alias) {
            Some(prefix) => format!("{prefix}{rest}"),
            None => {
                // Not an import of this file: left as written (it may still resolve from the
                // document's own imports, as before nested imports were followed).
                scope.strays.push(Stray { chain: scope.chain.to_string(), alias: alias.to_string(), target: target.to_string(), variable });
                target.to_string()
            }
        },
        None => format!("{}{target}", scope.prefix),
    }
}

/// An import's path: `uri` (percent-encoded, `/`-separated) relative to the importing file's folder.
fn resolve(base: &Path, uri: &str) -> PathBuf {
    let rel: String = uri.split('/').map(percent_decode).collect::<Vec<_>>().join("/");
    let mut out = PathBuf::new();
    for c in base.join(rel).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.components().next_back(), Some(Component::Normal(_))) => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    out
}

/// Identifies a file across the different ways imports spell its path.
fn file_key(path: &Path) -> String {
    let p = resolve(path.parent().unwrap_or(Path::new("")), &path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
    let s = p.to_string_lossy().replace('\\', "/");
    if cfg!(any(windows, target_os = "macos")) { s.to_lowercase() } else { s }
}

/// Loads one import, adding a file it reads to `files` with its modified time.
fn load_uri(path: Option<&Path>, uri: &str, files: &mut Vec<(PathBuf, Option<SystemTime>)>) -> Result<Document, String> {
    let Some(path) = path else {
        let name = uri.strip_prefix("pencil:").unwrap_or(uri);
        let bytes = pen_library(name).ok_or("Pen's built-in libraries need a local Pen install")?;
        let text = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        return Document::parse(&text).map_err(|e| e.to_string());
    };
    // Taken before reading, so a write that races the read shows up as a change next time.
    files.push((path.to_path_buf(), mtime(path)));
    // Access first, so a file outside STUDY_ROOT isn't reported as there or not.
    crate::access::check_read(path)?;
    if !path.exists() {
        return Err("not found".into());
    }
    Document::load(path).map_err(|e| format!("{e:#}"))
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
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
