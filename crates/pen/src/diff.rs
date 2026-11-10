//! Compares two laid-out versions of a document, node by node, the way they render.
//!
//! Nodes are matched by key: their id, or their instance path (`instance/child`) inside
//! component instances. Instances are compared as expanded, so a change made in a component shows
//! up on every instance that renders it. Besides each node's own properties, the comparison
//! sees layout: a node whose laid-out box changed while its own properties didn't (pushed by a
//! sibling, resized by flex, changed through its component) is reported too. Nodes a `script` node
//! generated have no stable identity and are left out.

use crate::instance::expand;
use crate::scene::{Rect, RootScene, NONE};
use crate::{Document, NodeId};
use serde_json::{Map, Value};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

/// What changed from one version of a document to another.
#[derive(Debug, Default, PartialEq)]
pub struct Diff {
    /// Nodes only in the newer version (only the outermost of a new subtree).
    pub added: Vec<String>,
    /// Nodes only in the older version (only the outermost of a removed subtree).
    pub removed: Vec<String>,
    /// Nodes in both that differ, in the newer version's order.
    pub changed: Vec<Change>,
    /// Top-level nodes containing any of the above: the newer version's ids, then removed
    /// top-level nodes of the older one.
    pub frames: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub struct Change {
    pub id: String,
    /// Properties whose values differ (as rendered, for nodes inside instances).
    pub props: Vec<String>,
    /// It has a different parent, or its order among siblings that are in both versions changed.
    pub moved: bool,
    /// Its laid-out box in document space, before and after, when it moved within its parent
    /// or resized (not when it was only carried along by a parent that moved).
    pub bounds: Option<(Rect, Rect)>,
}

/// One version: the document and its compiled, laid-out top-level nodes.
pub struct Version<'a> {
    pub doc: &'a Document,
    pub scenes: &'a HashMap<NodeId, RootScene>,
}

struct Entry<'a> {
    key: String,
    parent: Option<usize>,
    children: Vec<usize>,
    root: usize,
    props: Cow<'a, Map<String, Value>>,
    bounds: Rect,
}

struct Nodes<'a> {
    entries: Vec<Entry<'a>>,
    by_key: HashMap<String, usize>,
    /// Top-level entries in order.
    top: Vec<usize>,
}

/// Boxes closer than this (document units) count as the same.
const EPS: f32 = 0.01;

pub fn compare(before: &Version, after: &Version) -> Diff {
    let (a, b) = (collect(before), collect(after));
    let mut diff = Diff::default();
    let mut frames: Vec<String> = Vec::new();
    let mut frame = |key: &str| {
        if !frames.iter().any(|f| f == key) {
            frames.push(key.to_string());
        }
    };

    let moved = reordered(&a, &b);
    for e in &b.entries {
        let Some(&old) = a.by_key.get(&e.key) else {
            // Only the outermost new node of a subtree.
            if e.parent.is_none_or(|p| a.by_key.contains_key(&b.entries[p].key)) {
                diff.added.push(e.key.clone());
                frame(&b.entries[e.root].key);
            }
            continue;
        };
        let o = &a.entries[old];
        let props = changed_props(&o.props, &e.props);
        let parent_key = |n: &Nodes, x: &Entry| x.parent.map(|p| n.entries[p].key.clone());
        let moved = parent_key(&a, o) != parent_key(&b, e) || moved.contains(e.key.as_str());
        // A box change counts when the node moved within its parent or resized, not when it
        // was only carried along by a parent that moved.
        let rel = |n: &Nodes, x: &Entry| {
            let p = x.parent.map_or(Rect::new(0.0, 0.0, 0.0, 0.0), |p| n.entries[p].bounds);
            Rect::new(x.bounds.x - p.x, x.bounds.y - p.y, x.bounds.w, x.bounds.h)
        };
        let shifted = !same_rect(&rel(&a, o), &rel(&b, e)) || (moved && !same_rect(&o.bounds, &e.bounds));
        let bounds = shifted.then_some((o.bounds, e.bounds));
        if !props.is_empty() || moved || bounds.is_some() {
            diff.changed.push(Change { id: e.key.clone(), props, moved, bounds });
            frame(&b.entries[e.root].key);
        }
    }
    for e in &a.entries {
        if b.by_key.contains_key(&e.key) {
            continue;
        }
        if e.parent.is_none_or(|p| b.by_key.contains_key(&a.entries[p].key)) {
            diff.removed.push(e.key.clone());
            let root = &a.entries[e.root].key;
            frame(b.by_key.get(root).map_or(root, |&i| &b.entries[i].key));
        }
    }
    diff.frames = frames;
    diff
}

fn same_rect(a: &Rect, b: &Rect) -> bool {
    (a.x - b.x).abs() < EPS && (a.y - b.y).abs() < EPS && (a.w - b.w).abs() < EPS && (a.h - b.h).abs() < EPS
}

/// Property names whose values differ, the newer version's order first.
fn changed_props(a: &Map<String, Value>, b: &Map<String, Value>) -> Vec<String> {
    let skip = |k: &str| k == "children" || k == "id";
    let mut out: Vec<String> = b.iter().filter(|(k, v)| !skip(k) && a.get(*k) != Some(*v)).map(|(k, _)| k.clone()).collect();
    out.extend(a.keys().filter(|k| !skip(k) && !b.contains_key(*k)).cloned());
    out
}

/// Keys of nodes whose order among their siblings changed: for each parent in both versions,
/// the children in both, minus the longest run that kept its relative order.
fn reordered<'k>(a: &Nodes, b: &'k Nodes) -> HashSet<&'k str> {
    let mut out = HashSet::new();
    let mut check = |old: &[usize], new: &'k [usize]| {
        let pos: HashMap<&str, usize> = old.iter().enumerate().map(|(i, &e)| (a.entries[e].key.as_str(), i)).collect();
        let common: Vec<(&'k str, usize)> = new.iter().filter_map(|&e| pos.get(b.entries[e].key.as_str()).map(|&p| (b.entries[e].key.as_str(), p))).collect();
        let keep = longest_increasing(&common.iter().map(|c| c.1).collect::<Vec<_>>());
        for (i, (k, _)) in common.iter().enumerate() {
            if !keep.contains(&i) {
                out.insert(*k);
            }
        }
    };
    check(&a.top, &b.top);
    for e in &b.entries {
        if let Some(&o) = a.by_key.get(&e.key) {
            check(&a.entries[o].children, &e.children);
        }
    }
    out
}

/// Indices of one longest strictly increasing subsequence.
fn longest_increasing(v: &[usize]) -> HashSet<usize> {
    let mut tails: Vec<usize> = Vec::new(); // index into v of the smallest tail per length
    let mut prev: Vec<Option<usize>> = vec![None; v.len()];
    for i in 0..v.len() {
        let at = tails.partition_point(|&t| v[t] < v[i]);
        prev[i] = at.checked_sub(1).map(|p| tails[p]);
        if at == tails.len() {
            tails.push(i);
        } else {
            tails[at] = i;
        }
    }
    let mut out = HashSet::new();
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        out.insert(i);
        cur = prev[i];
    }
    out
}

fn collect<'a>(v: &Version<'a>) -> Nodes<'a> {
    let doc = v.doc;
    let mut nodes = Nodes { entries: Vec::new(), by_key: HashMap::new(), top: Vec::new() };
    for r in &doc.roots {
        let Some(s) = v.scenes.get(r) else { continue };
        // Instances as they render, by instance path.
        let mut expanded: HashMap<String, Map<String, Value>> = HashMap::new();
        for n in &s.nodes {
            let Some(src) = n.src else { continue };
            if n.path.is_none() && doc.node(src).ty() == "ref" {
                flatten(&expand(doc, &doc.to_value(src)), &mut expanded);
            }
        }
        let mut entry_of = vec![usize::MAX; s.nodes.len()];
        let mut root = usize::MAX;
        for (i, n) in s.nodes.iter().enumerate() {
            let key = n.key();
            if key.is_empty() || (n.src.is_none() && n.path.is_none()) || nodes.by_key.contains_key(key) {
                continue;
            }
            let mut p = n.parent;
            while p != NONE && entry_of[p as usize] == usize::MAX {
                p = s.nodes[p as usize].parent;
            }
            let parent = (p != NONE).then(|| entry_of[p as usize]);
            let props = match expanded.remove(key) {
                Some(m) => Cow::Owned(m),
                None => match n.src {
                    Some(src) => Cow::Borrowed(&doc.node(src).props),
                    None => Cow::Owned(Map::new()),
                },
            };
            let idx = nodes.entries.len();
            if parent.is_none() {
                root = idx;
                nodes.top.push(idx);
            }
            let (m, w, h) = s.transform(i as u32);
            let pts = [m.map(0.0, 0.0), m.map(w, 0.0), m.map(w, h), m.map(0.0, h)];
            let (l, t) = pts.iter().fold((f32::MAX, f32::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
            let (r, btm) = pts.iter().fold((f32::MIN, f32::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
            nodes.entries.push(Entry { key: key.to_string(), parent, children: Vec::new(), root, props, bounds: Rect::new(l, t, r - l, btm - t) });
            if let Some(p) = parent {
                nodes.entries[p].children.push(idx);
            }
            nodes.by_key.insert(key.to_string(), idx);
            entry_of[i] = idx;
        }
    }
    nodes
}

/// Every node of an expanded instance tree, by id, without its children.
fn flatten(v: &Value, out: &mut HashMap<String, Map<String, Value>>) {
    let Some(o) = v.as_object() else { return };
    if let Some(id) = o.get("id").and_then(Value::as_str) {
        let mut own = o.clone();
        own.remove("children");
        out.insert(id.to_string(), own);
    }
    for c in o.get("children").and_then(Value::as_array).into_iter().flatten() {
        flatten(c, out);
    }
}

#[cfg(test)]
mod tests {
    use super::longest_increasing;

    #[test]
    fn keeps_the_longest_ordered_run() {
        let keep = longest_increasing(&[0, 3, 1, 2]);
        assert_eq!(keep.len(), 3);
        assert!(!keep.contains(&1)); // the 3 moved
        assert_eq!(longest_increasing(&[]).len(), 0);
    }
}
