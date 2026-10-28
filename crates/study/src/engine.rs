//! The live document plus everything derived from it: compiled scenes, layout,
//! renderer caches, undo history, selection. Shared by the window and headless MCP.

use anyhow::Result;
use pen::doc::Undo;
use pen::layout::{compute_bounds, layout, Affine};
use pen::imports::Imports;
use pen::scene::{compile_root_with, graft, Kind, Rect, RootScene, NONE};
use pen::vars::Vars;
use pen::{Document, NodeId};
use render::Renderer;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub struct Engine {
    pub doc: Document,
    pub vars: Vars,
    pub scenes: HashMap<NodeId, RootScene>,
    pub renderer: Renderer,
    dirty: HashSet<NodeId>,
    dirty_all: bool,
    pub undo: Vec<Vec<Undo>>,
    pub redo: Vec<Vec<Undo>>,
    /// Selected node keys (ids or instance paths).
    pub selection: Vec<String>,
    pub js: Option<crate::api::Js>,
    /// Snippets of failed execute calls, by editId.
    pub failed: HashMap<String, String>,
    pub next_edit: u32,
    /// Modification time of the file as we last wrote or read it.
    pub disk_mtime: Option<SystemTime>,
    /// Path nodes created by the last execute call, for the window to draw in.
    pub fresh_paths: Vec<String>,
    /// Nodes the last execute call read, for the window to scan.
    pub fresh_reads: Vec<String>,
    /// Top-level nodes the last execute call changed or read.
    pub touched_roots: Vec<String>,
    /// Components and variables from files listed in the document's `imports`.
    pub imports: Imports,
    scripts: Option<crate::scripts::Scripts>,
    /// Script failures since the last execute call reported them.
    pub script_errors: Vec<String>,
}

impl Engine {
    pub fn new(doc: Document) -> Engine {
        let mut renderer = Renderer::new();
        if let Some(dir) = doc.path.as_ref().and_then(|p| p.parent()) {
            renderer.base_dir = dir.to_path_buf();
        }
        let disk_mtime = doc.path.as_ref().and_then(|p| mtime(p));
        let imports = Imports::load(&doc);
        let mut e = Engine {
            vars: vars_for(&doc, &imports),
            doc,
            scenes: HashMap::new(),
            renderer,
            dirty: HashSet::new(),
            dirty_all: true,
            undo: Vec::new(),
            redo: Vec::new(),
            selection: Vec::new(),
            js: None,
            failed: HashMap::new(),
            next_edit: 1,
            disk_mtime,
            fresh_paths: Vec::new(),
            fresh_reads: Vec::new(),
            touched_roots: Vec::new(),
            imports,
            scripts: None,
            script_errors: Vec::new(),
        };
        e.refresh();
        e
    }

    pub fn open(path: &Path) -> Result<Engine> {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let path = PathBuf::from(path.to_string_lossy().trim_start_matches(r"\\?\"));
        let doc = if path.exists() {
            Document::load(&path)?
        } else {
            let mut d = Document::default();
            d.path = Some(path.clone());
            d.format.trailing_newline = false;
            d.save()?;
            d
        };
        Ok(Engine::new(doc))
    }

    pub fn path(&self) -> Option<&Path> {
        self.doc.path.as_deref()
    }

    /// Recompiles and lays out whatever the document changes touched. Returns true if anything changed.
    pub fn refresh(&mut self) -> bool {
        let ch = self.doc.take_changes();
        if ch.variables {
            self.vars = vars_for(&self.doc, &self.imports);
            self.dirty_all = true;
        }
        for n in ch.nodes {
            let root = self.doc.root_of(n);
            self.dirty.insert(root);
            // Edits inside a component re-render every root that instances it.
            let mut cur = Some(n);
            while let Some(c) = cur {
                if self.doc.node(c).is_reusable() {
                    for (r, s) in &self.scenes {
                        if s.uses.contains(&c) {
                            self.dirty.insert(*r);
                        }
                    }
                }
                cur = self.doc.node(c).parent;
            }
        }
        let roots: HashSet<NodeId> = self.doc.roots.iter().copied().collect();
        let before = self.scenes.len();
        self.scenes.retain(|k, _| roots.contains(k));
        let mut changed = before != self.scenes.len();
        for r in &self.doc.roots {
            if !self.scenes.contains_key(r) {
                self.dirty.insert(*r);
            }
        }
        if self.dirty_all {
            self.dirty.extend(roots.iter().copied());
            self.dirty_all = false;
        }
        let dirty: Vec<NodeId> = self.dirty.drain().filter(|r| roots.contains(r)).collect();
        for r in dirty {
            let mut s = compile_root_with(&self.doc, &self.vars, r, Some(&self.imports));
            layout(&mut s, &mut self.renderer.text);
            self.run_scripts(&mut s);
            compute_bounds(&mut s);
            self.scenes.insert(r, s);
            changed = true;
        }
        changed
    }

    /// Re-runs layout everywhere (after fonts arrive).
    pub fn relayout_all(&mut self) {
        for s in self.scenes.values_mut() {
            layout(s, &mut self.renderer.text);
            compute_bounds(s);
        }
    }

    /// Collects background loads. Returns true if a redraw is needed.
    pub fn poll_assets(&mut self) -> bool {
        let (fonts, any) = self.renderer.poll();
        if fonts {
            self.relayout_all();
        }
        any
    }

    /// Blocks until fonts/icons/images are loaded and layout is final (headless use).
    pub fn settle(&mut self) {
        self.refresh();
        for _ in 0..4 {
            if !self.renderer.has_pending() {
                break;
            }
            if self.renderer.wait_for_assets(Duration::from_secs(30)) {
                self.relayout_all();
            }
        }
    }

    pub fn ordered_scenes(&self) -> Vec<&RootScene> {
        self.doc.roots.iter().filter_map(|r| self.scenes.get(r)).collect()
    }

    /// Scene and index for a node id or instance path.
    pub fn locate(&self, key: &str) -> Option<(&RootScene, u32)> {
        let first = key.split('/').next()?;
        let n = self.doc.find(first)?;
        if !self.doc.node(n).attached {
            return None;
        }
        let scene = self.scenes.get(&self.doc.root_of(n))?;
        let i = if key.contains('/') { scene.lookup(key, &self.doc)? } else { scene.lookup_node(n)? };
        Some((scene, i))
    }

    /// Absolute (document-space) box of a node, ignoring rotation.
    pub fn abs_rect(&self, key: &str) -> Option<Rect> {
        let (s, i) = self.locate(key)?;
        Some(s.abs_rect(i))
    }

    /// Union of all root bounds.
    pub fn doc_bounds(&self) -> Option<Rect> {
        self.ordered_scenes().iter().filter_map(|s| s.bounds.first().copied()).filter(|b| b.w >= 0.0).reduce(|a, b| a.union(&b))
    }

    /// Deepest enabled node under a document-space point.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<String> {
        for s in self.ordered_scenes().iter().rev() {
            if s.bounds.first().is_none_or(|b| !b.contains(x, y)) {
                continue;
            }
            if let Some(mut i) = hit(s, 0, &Affine::IDENTITY, x, y) {
                // Nodes generated by scripts have no key; select the script itself.
                while s.nodes[i as usize].key().is_empty() && s.nodes[i as usize].parent != NONE {
                    i = s.nodes[i as usize].parent;
                }
                return Some(s.nodes[i as usize].key().to_string());
            }
        }
        None
    }

    /// Absolute transform of a node's own coordinate space.
    pub fn node_transform(&self, key: &str) -> Option<(Affine, f32, f32)> {
        let (s, i) = self.locate(key)?;
        let mut chain = Vec::new();
        let mut cur = i;
        while cur != NONE {
            chain.push(cur);
            cur = s.nodes[cur as usize].parent;
        }
        let mut m = Affine::IDENTITY;
        for &c in chain.iter().rev() {
            m = m.then(&Affine::of_node(&s.nodes[c as usize]));
        }
        let n = &s.nodes[i as usize];
        Some((m, n.rect.w, n.rect.h))
    }

    pub fn save(&mut self) -> Result<()> {
        if self.doc.path.is_none() {
            return Ok(());
        }
        self.doc.save()?;
        self.disk_mtime = self.path().and_then(mtime);
        Ok(())
    }

    /// Writes the document to a new path and continues editing it there.
    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        self.doc.save_as(path)?;
        if let Some(dir) = path.parent() {
            self.renderer.base_dir = dir.to_path_buf();
        }
        self.disk_mtime = mtime(path);
        Ok(())
    }

    /// Re-reads the file if it changed on disk behind our back. Returns true if reloaded.
    pub fn reload_if_changed(&mut self) -> bool {
        let Some(path) = self.path().map(Path::to_path_buf) else { return false };
        let now = mtime(&path);
        if now.is_none() || now == self.disk_mtime {
            return false;
        }
        self.disk_mtime = now;
        match Document::load(&path) {
            Ok(doc) => {
                self.doc = doc;
                self.imports = Imports::load(&self.doc);
                self.vars = vars_for(&self.doc, &self.imports);
                self.scenes.clear();
                self.dirty_all = true;
                self.undo.clear();
                self.redo.clear();
                self.refresh();
                true
            }
            Err(_) => false,
        }
    }

    pub fn undo(&mut self) -> bool {
        let Some(log) = self.undo.pop() else { return false };
        let redo = self.doc.undo(log);
        self.redo.push(redo);
        self.after_history();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(log) = self.redo.pop() else { return false };
        let undo = self.doc.undo(log);
        self.undo.push(undo);
        self.after_history();
        true
    }

    fn after_history(&mut self) {
        self.vars = vars_for(&self.doc, &self.imports);
        self.refresh();
        let _ = self.save();
    }

    pub fn new_edit_id(&mut self) -> String {
        let id = format!("e{}", self.next_edit);
        self.next_edit += 1;
        id
    }
}

fn hit(s: &RootScene, i: u32, parent: &Affine, x: f32, y: f32) -> Option<u32> {
    let n = &s.nodes[i as usize];
    if !n.enabled {
        return None;
    }
    if !s.bounds[i as usize].contains(x, y) {
        return None;
    }
    let m = parent.then(&Affine::of_node(n));
    let (lx, ly) = m.invert()?.map(x, y);
    let inside = lx >= 0.0 && ly >= 0.0 && lx <= n.rect.w && ly <= n.rect.h;
    if n.clips() && !inside {
        return None;
    }
    for &c in n.children.iter().rev() {
        if let Some(h) = hit(s, c, &m, x, y) {
            return Some(h);
        }
    }
    let hittable = !matches!(n.kind, pen::scene::Kind::Group);
    (inside && hittable).then_some(i)
}

pub fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl Engine {
    /// Drops selected keys that no longer resolve to a node.
    pub fn prune_selection(&mut self) {
        let keep: Vec<String> = self.selection.iter().filter(|k| self.locate(k).is_some()).cloned().collect();
        self.selection = keep;
    }
}

fn vars_for(doc: &Document, imports: &Imports) -> Vars {
    let mut v = Vars::new(doc);
    v.add_imported(&imports.variables);
    v
}

impl Engine {
    /// Runs `script` nodes in a laid-out scene and grafts their output in as children.
    fn run_scripts(&mut self, s: &mut RootScene) {
        let jobs: Vec<(u32, std::sync::Arc<str>, std::sync::Arc<serde_json::Map<String, serde_json::Value>>, f32, f32)> = s
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| match &n.kind {
                Kind::Script { uri, inputs, .. } if n.enabled => Some((i as u32, uri.clone(), inputs.clone(), n.rect.w, n.rect.h)),
                _ => None,
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        if self.scripts.is_none() {
            self.scripts = crate::scripts::Scripts::new();
        }
        let Some(runner) = self.scripts.as_mut() else { return };
        let base = self.doc.path.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
        for (i, uri, inputs, w, h) in jobs {
            match runner.run(&base.join(&*uri), w, h, &inputs) {
                Ok(nodes) => graft(s, &self.doc, &self.vars, Some(&self.imports), i, &nodes),
                Err(e) => {
                    s.nodes[i as usize].kind = Kind::Missing;
                    if !self.script_errors.contains(&e) {
                        self.script_errors.push(e);
                    }
                }
            }
        }
        layout(s, &mut self.renderer.text);
    }
}
