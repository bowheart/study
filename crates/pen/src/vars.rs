//! Variable and theme resolution.

use crate::{Document, Props};
use serde_json::Value;

/// Active theme: axis -> value. Tiny, so a Vec beats a map.
pub type Theme = Vec<(String, String)>;

pub fn theme_get<'a>(t: &'a Theme, axis: &str) -> Option<&'a str> {
    t.iter().find(|(k, _)| k == axis).map(|(_, v)| v.as_str())
}

/// Returns `base` with the axes of a node's `theme` property applied on top.
pub fn apply_theme(base: &Theme, theme_prop: Option<&Value>) -> Option<Theme> {
    let obj = theme_prop?.as_object()?;
    if obj.is_empty() {
        return None;
    }
    let mut t = base.clone();
    for (k, v) in obj {
        let Some(v) = v.as_str() else { continue };
        match t.iter_mut().find(|(a, _)| a == k) {
            Some(slot) => slot.1 = v.to_string(),
            None => t.push((k.clone(), v.to_string())),
        }
    }
    Some(t)
}

#[derive(Clone, Debug, Default)]
pub struct Vars {
    defs: Props,
    pub default_theme: Theme,
}

impl Vars {
    pub fn new(doc: &Document) -> Vars {
        let mut default_theme = Theme::new();
        if let Some(themes) = doc.themes() {
            for (axis, values) in themes {
                if let Some(first) = values.as_array().and_then(|a| a.first()).and_then(Value::as_str) {
                    default_theme.push((axis.clone(), first.to_string()));
                }
            }
        }
        Vars { defs: doc.variables().cloned().unwrap_or_default(), default_theme }
    }

    /// Adds variables from `imports` (already keyed `alias:name`).
    pub fn add_imported(&mut self, defs: &Props) {
        for (k, v) in defs {
            self.defs.insert(k.clone(), v.clone());
        }
    }

    pub fn defs(&self) -> &Props {
        &self.defs
    }

    /// Resolves a `$name` reference (following chains). Non-references are returned as-is.
    pub fn resolve<'a>(&'a self, v: &'a Value, theme: &Theme) -> &'a Value {
        let mut cur = v;
        for _ in 0..16 {
            let Some(name) = cur.as_str().and_then(|s| s.strip_prefix('$')) else { return cur };
            match self.lookup(name, theme) {
                Some(next) => cur = next,
                None => return &Value::Null,
            }
        }
        cur
    }

    fn lookup<'a>(&'a self, name: &str, theme: &Theme) -> Option<&'a Value> {
        // Imported variables are `alias:name`; a bare name may also live in one import.
        let def = self.defs.get(name).or_else(|| self.defs.iter().find(|(k, _)| k.split_once(':').is_some_and(|(_, n)| n == name)).map(|(_, v)| v))?;
        let value = def.get("value")?;
        match value {
            Value::Array(entries) => pick_themed(entries, theme),
            v => Some(v),
        }
    }

    /// Deeply resolves every variable reference inside a value (fills, uniforms, ...).
    pub fn resolve_deep(&self, v: &Value, theme: &Theme) -> Value {
        match v {
            Value::String(s) if s.starts_with('$') => self.resolve(v, theme).clone(),
            Value::Array(a) => Value::Array(a.iter().map(|x| self.resolve_deep(x, theme)).collect()),
            Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), self.resolve_deep(x, theme))).collect()),
            other => other.clone(),
        }
    }

    pub fn num(&self, v: Option<&Value>, theme: &Theme) -> Option<f32> {
        match self.resolve(v?, theme) {
            Value::Number(n) => n.as_f64().map(|f| f as f32),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    pub fn str<'a>(&'a self, v: Option<&'a Value>, theme: &Theme) -> Option<&'a str> {
        self.resolve(v?, theme).as_str()
    }

    pub fn bool(&self, v: Option<&Value>, theme: &Theme) -> Option<bool> {
        match self.resolve(v?, theme) {
            Value::Bool(b) => Some(*b),
            Value::String(s) => match s.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
            Value::Number(n) => n.as_f64().map(|f| f != 0.0),
            _ => None,
        }
    }
}

/// Picks the themed entry that best matches the active theme.
fn pick_themed<'a>(entries: &'a [Value], theme: &Theme) -> Option<&'a Value> {
    let mut best: Option<(usize, &Value)> = None;
    for e in entries {
        let Some(obj) = e.as_object() else { continue };
        let Some(value) = obj.get("value") else { continue };
        let score = match obj.get("theme").and_then(Value::as_object) {
            None => 0,
            Some(t) if t.is_empty() => 0,
            Some(t) => {
                if t.iter().all(|(k, v)| v.as_str().is_some_and(|v| theme_get(theme, k) == Some(v))) {
                    t.len()
                } else {
                    continue;
                }
            }
        };
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, value));
        }
    }
    best.map(|b| b.1).or_else(|| entries.first().and_then(|e| e.get("value")))
}
