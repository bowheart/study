//! The document: a lossless, mutable tree of .pen nodes.
//!
//! Every node keeps its original JSON property map (key order included), so a
//! load/save round-trip reproduces the file byte-for-byte. The `children` key
//! stays in the map as a `null` placeholder so its position is preserved too.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub type Props = Map<String, Value>;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    /// All properties, including `type` and `id`. `children` is a `null` placeholder.
    pub props: Props,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// False once the node has been deleted (kept around for undo).
    pub attached: bool,
}

impl Node {
    pub fn id(&self) -> &str {
        self.props.get("id").and_then(Value::as_str).unwrap_or("")
    }
    pub fn ty(&self) -> &str {
        self.props.get("type").and_then(Value::as_str).unwrap_or("")
    }
    pub fn name(&self) -> Option<&str> {
        self.props.get("name").and_then(Value::as_str)
    }
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.props.get(k)
    }
    pub fn can_have_children(&self) -> bool {
        matches!(self.ty(), "frame" | "group")
    }
    pub fn is_reusable(&self) -> bool {
        self.props.get("reusable").and_then(Value::as_bool).unwrap_or(false)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Format {
    pub crlf: bool,
    pub trailing_newline: bool,
}

/// One reversible step, recorded while a transaction is open.
#[derive(Clone, Debug)]
pub enum Undo {
    Props { node: NodeId, old: Props },
    Inserted { node: NodeId },
    Removed { node: NodeId, parent: Option<NodeId>, index: usize },
    Moved { node: NodeId, parent: Option<NodeId>, index: usize },
    Header { old: Props },
}

/// What changed since the last time the consumer drained it.
#[derive(Clone, Debug, Default)]
pub struct Changes {
    pub nodes: Vec<NodeId>,
    pub structure: bool,
    pub variables: bool,
}

#[derive(Clone, Debug)]
pub struct Document {
    slots: Vec<Node>,
    pub roots: Vec<NodeId>,
    ids: HashMap<String, NodeId>,
    /// Top-level keys (`version`, `themes`, `variables`, ...). `children` is a placeholder.
    pub header: Props,
    pub format: Format,
    pub path: Option<PathBuf>,
    pub revision: u64,
    journal: Option<Vec<Undo>>,
    pub changes: Changes,
}

impl Default for Document {
    fn default() -> Self {
        let mut header = Props::new();
        header.insert("version".into(), Value::String(crate::VERSION.into()));
        header.insert("children".into(), Value::Null);
        Document {
            slots: Vec::new(),
            roots: Vec::new(),
            ids: HashMap::new(),
            header,
            format: Format { crlf: false, trailing_newline: false },
            path: None,
            revision: 0,
            journal: None,
            changes: Changes::default(),
        }
    }
}

impl Document {
    pub fn load(path: &Path) -> Result<Document> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut doc = Document::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
        doc.path = Some(path.to_path_buf());
        Ok(doc)
    }

    pub fn parse(text: &str) -> Result<Document> {
        let root: Value = serde_json::from_str(text)?;
        let Value::Object(mut top) = root else { bail!("document is not a JSON object") };
        let mut doc = Document {
            format: Format { crlf: text.contains("\r\n"), trailing_newline: text.ends_with('\n') },
            ..Document::default()
        };
        let children = match top.get_mut("children") {
            Some(v) => std::mem::replace(v, Value::Null),
            None => Value::Array(vec![]),
        };
        doc.header = top;
        if !doc.header.contains_key("children") {
            doc.header.insert("children".into(), Value::Null);
        }
        if let Value::Array(list) = children {
            for child in list {
                let id = doc.build(child, None, false)?;
                doc.roots.push(id);
            }
        }
        Ok(doc)
    }

    /// Builds a subtree from JSON. With `fresh_ids`, every node gets a new unique id.
    fn build(&mut self, v: Value, parent: Option<NodeId>, fresh_ids: bool) -> Result<NodeId> {
        let Value::Object(mut props) = v else { bail!("node is not a JSON object") };
        let children = match props.get_mut("children") {
            Some(c) => std::mem::replace(c, Value::Null),
            None => Value::Null,
        };
        if fresh_ids || !matches!(props.get("id"), Some(Value::String(s)) if !s.is_empty() && !self.ids.contains_key(s.as_str()))
        {
            let id = self.new_id();
            set_id(&mut props, id);
        }
        let id_str = props["id"].as_str().unwrap().to_string();
        let nid = NodeId(self.slots.len() as u32);
        self.slots.push(Node { props, parent, children: Vec::new(), attached: true });
        self.ids.insert(id_str, nid);
        if let Value::Array(list) = children {
            let mut kids = Vec::with_capacity(list.len());
            for c in list {
                kids.push(self.build(c, Some(nid), fresh_ids)?);
            }
            self.slots[nid.index()].children = kids;
        }
        Ok(nid)
    }

    pub fn new_id(&self) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        loop {
            let mut s = String::with_capacity(5);
            for _ in 0..5 {
                s.push(ALPHABET[fastrand::usize(..ALPHABET.len())] as char);
            }
            if !self.ids.contains_key(&s) {
                return s;
            }
        }
    }

    // ---------------------------------------------------------------- reading

    pub fn node(&self, id: NodeId) -> &Node {
        &self.slots[id.index()]
    }
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.slots[id.index()]
    }
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
    pub fn find(&self, id: &str) -> Option<NodeId> {
        self.ids.get(id).copied()
    }
    pub fn children_of(&self, parent: Option<NodeId>) -> &[NodeId] {
        match parent {
            Some(p) => &self.slots[p.index()].children,
            None => &self.roots,
        }
    }
    pub fn root_of(&self, mut id: NodeId) -> NodeId {
        while let Some(p) = self.node(id).parent {
            id = p;
        }
        id
    }
    pub fn index_in_parent(&self, id: NodeId) -> usize {
        let parent = self.node(id).parent;
        self.children_of(parent).iter().position(|&c| c == id).unwrap_or(0)
    }
    pub fn variables(&self) -> Option<&Props> {
        self.header.get("variables").and_then(Value::as_object)
    }
    pub fn themes(&self) -> Option<&Props> {
        self.header.get("themes").and_then(Value::as_object)
    }
    pub fn node_count(&self) -> usize {
        self.ids.len()
    }

    /// Depth-first visit of every attached node.
    pub fn walk(&self, mut f: impl FnMut(NodeId, usize)) {
        let mut stack: Vec<(NodeId, usize)> = self.roots.iter().rev().map(|&r| (r, 0)).collect();
        while let Some((n, d)) = stack.pop() {
            f(n, d);
            for &c in self.node(n).children.iter().rev() {
                stack.push((c, d + 1));
            }
        }
    }

    /// Serializes a subtree back to JSON (children nested).
    pub fn to_value(&self, id: NodeId) -> Value {
        let node = self.node(id);
        let mut props = node.props.clone();
        if props.contains_key("children") || !node.children.is_empty() {
            let kids = node.children.iter().map(|&c| self.to_value(c)).collect();
            props.insert("children".into(), Value::Array(kids));
        }
        Value::Object(props)
    }

    // ---------------------------------------------------------------- saving

    pub fn serialize(&self) -> String {
        let mut w = crate::json::Writer::new(self.format.crlf);
        w.begin_object();
        for (k, v) in &self.header {
            w.key(k);
            if k == "children" {
                w.begin_array();
                for &r in &self.roots {
                    w.item();
                    self.write_node(&mut w, r);
                }
                w.end_array(self.roots.is_empty());
            } else {
                w.value(v);
            }
        }
        w.end_object(self.header.is_empty());
        if self.format.trailing_newline {
            w.newline();
        }
        w.finish()
    }

    fn write_node(&self, w: &mut crate::json::Writer, id: NodeId) {
        let node = self.node(id);
        w.begin_object();
        let mut wrote_children = false;
        for (k, v) in &node.props {
            if k == "children" {
                w.key(k);
                self.write_children(w, node);
                wrote_children = true;
            } else {
                w.key(k);
                w.value(v);
            }
        }
        if !wrote_children && !node.children.is_empty() {
            w.key("children");
            self.write_children(w, node);
        }
        w.end_object(node.props.is_empty());
    }

    fn write_children(&self, w: &mut crate::json::Writer, node: &Node) {
        w.begin_array();
        for &c in &node.children {
            w.item();
            self.write_node(w, c);
        }
        w.end_array(node.children.is_empty());
    }

    pub fn save(&mut self) -> Result<()> {
        let path = self.path.clone().ok_or_else(|| anyhow!("document has no path"))?;
        self.save_as(&path)
    }

    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        let text = self.serialize();
        let tmp = path.with_extension("pen.tmp");
        std::fs::write(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, path)?;
        self.path = Some(path.to_path_buf());
        Ok(())
    }

    // ---------------------------------------------------------------- transactions

    pub fn begin(&mut self) {
        self.journal = Some(Vec::new());
    }

    /// Closes the transaction, returning its undo log.
    pub fn commit(&mut self) -> Vec<Undo> {
        self.journal.take().unwrap_or_default()
    }

    /// Reverts everything recorded since `begin`.
    pub fn rollback(&mut self) {
        if let Some(log) = self.journal.take() {
            self.undo(log);
        }
    }

    /// Applies an undo log (in reverse order), returning the redo log.
    pub fn undo(&mut self, log: Vec<Undo>) -> Vec<Undo> {
        let outer = self.journal.replace(Vec::new());
        for step in log.into_iter().rev() {
            match step {
                Undo::Props { node, old } => self.set_props(node, old),
                Undo::Inserted { node } => self.detach(node),
                Undo::Removed { node, parent, index } => self.attach(node, parent, index),
                Undo::Moved { node, parent, index } => {
                    let _ = self.move_node(node, parent, Some(index));
                }
                Undo::Header { old } => self.set_header(old),
            }
        }
        let redo = std::mem::replace(&mut self.journal, outer).unwrap_or_default();
        redo
    }

    fn record(&mut self, u: Undo) {
        if let Some(j) = &mut self.journal {
            j.push(u);
        }
    }

    fn touched(&mut self, id: NodeId) {
        self.revision += 1;
        self.changes.nodes.push(id);
    }

    // ---------------------------------------------------------------- mutations

    /// Replaces a node's whole property map (children are untouched).
    pub fn set_props(&mut self, id: NodeId, mut props: Props) {
        let node = &mut self.slots[id.index()];
        let old_id = node.id().to_string();
        if node.props.contains_key("children") && !props.contains_key("children") {
            props.insert("children".into(), Value::Null);
        }
        let old = std::mem::replace(&mut node.props, props);
        let new_id = self.slots[id.index()].id().to_string();
        if new_id != old_id && self.slots[id.index()].attached {
            self.ids.remove(&old_id);
            self.ids.insert(new_id, id);
        }
        self.record(Undo::Props { node: id, old });
        self.touched(id);
    }

    /// Merges properties into a node. `null` values remove the property.
    pub fn update(&mut self, id: NodeId, patch: &Props) {
        let mut props = self.node(id).props.clone();
        for (k, v) in patch {
            if k == "id" || k == "type" || k == "children" {
                continue;
            }
            if v.is_null() {
                props.shift_remove(k);
            } else {
                props.insert(k.clone(), v.clone());
            }
        }
        self.set_props(id, props);
    }

    pub fn set_header(&mut self, header: Props) {
        let old = std::mem::replace(&mut self.header, header);
        self.record(Undo::Header { old });
        self.revision += 1;
        self.changes.variables = true;
    }

    /// Inserts a JSON subtree under `parent` (None = document root). Ids are always regenerated.
    pub fn insert(&mut self, parent: Option<NodeId>, index: Option<usize>, data: Value) -> Result<NodeId> {
        if let Some(p) = parent {
            if !self.node(p).can_have_children() {
                bail!("node '{}' of type '{}' cannot have children", self.node(p).id(), self.node(p).ty());
            }
        }
        let nid = self.build(data, parent, true)?;
        let list = self.children_mut(parent);
        let at = index.unwrap_or(list.len()).min(list.len());
        list.insert(at, nid);
        self.record(Undo::Inserted { node: nid });
        self.changes.structure = true;
        self.touched(nid);
        Ok(nid)
    }

    pub fn delete(&mut self, id: NodeId) {
        let parent = self.node(id).parent;
        let index = self.index_in_parent(id);
        self.detach(id);
        self.record(Undo::Removed { node: id, parent, index });
    }

    pub fn move_node(&mut self, id: NodeId, parent: Option<NodeId>, index: Option<usize>) -> Result<()> {
        if let Some(p) = parent {
            if !self.node(p).can_have_children() {
                bail!("node '{}' cannot have children", self.node(p).id());
            }
            let mut a = Some(p);
            while let Some(x) = a {
                if x == id {
                    bail!("cannot move a node into itself");
                }
                a = self.node(x).parent;
            }
        }
        let old_parent = self.node(id).parent;
        let old_index = self.index_in_parent(id);
        self.children_mut(old_parent).retain(|&c| c != id);
        let list = self.children_mut(parent);
        let at = index.unwrap_or(list.len()).min(list.len());
        list.insert(at, id);
        self.slots[id.index()].parent = parent;
        self.record(Undo::Moved { node: id, parent: old_parent, index: old_index });
        self.changes.structure = true;
        self.touched(id);
        Ok(())
    }

    fn children_mut(&mut self, parent: Option<NodeId>) -> &mut Vec<NodeId> {
        match parent {
            Some(p) => &mut self.slots[p.index()].children,
            None => &mut self.roots,
        }
    }

    fn detach(&mut self, id: NodeId) {
        let parent = self.node(id).parent;
        self.children_mut(parent).retain(|&c| c != id);
        self.set_attached(id, false);
        self.changes.structure = true;
        self.touched(id);
    }

    fn attach(&mut self, id: NodeId, parent: Option<NodeId>, index: usize) {
        let list = self.children_mut(parent);
        let at = index.min(list.len());
        list.insert(at, id);
        self.slots[id.index()].parent = parent;
        self.set_attached(id, true);
        self.record(Undo::Inserted { node: id });
        self.changes.structure = true;
        self.touched(id);
    }

    fn set_attached(&mut self, id: NodeId, on: bool) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            let node = &mut self.slots[n.index()];
            node.attached = on;
            let key = node.id().to_string();
            stack.extend(node.children.iter().copied());
            if on {
                self.ids.insert(key, n);
            } else if self.ids.get(&key) == Some(&n) {
                self.ids.remove(&key);
            }
        }
    }

    pub fn take_changes(&mut self) -> Changes {
        std::mem::take(&mut self.changes)
    }
}

/// Sets `id` on a props map, placing it right after `type` (as Pen does for new nodes).
fn set_id(props: &mut Props, id: String) {
    if props.contains_key("id") {
        props.insert("id".into(), Value::String(id));
        return;
    }
    let mut out = Props::new();
    let mut placed = false;
    for (k, v) in std::mem::take(props) {
        let is_type = k == "type";
        out.insert(k, v);
        if is_type && !placed {
            out.insert("id".into(), Value::String(id.clone()));
            placed = true;
        }
    }
    if !placed {
        out.shift_insert(0, "id".into(), Value::String(id));
    }
    *props = out;
}
