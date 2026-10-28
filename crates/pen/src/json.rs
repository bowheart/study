//! A pretty JSON writer that matches `JSON.stringify(value, null, 2)` output.

use serde_json::Value;

pub struct Writer {
    out: String,
    indent: usize,
    nl: &'static str,
    /// Whether the current container has had an entry yet.
    first: Vec<bool>,
}

impl Writer {
    pub fn new(crlf: bool) -> Self {
        Writer { out: String::with_capacity(1 << 20), indent: 0, nl: if crlf { "\r\n" } else { "\n" }, first: Vec::new() }
    }

    pub fn finish(self) -> String {
        self.out
    }

    pub fn newline(&mut self) {
        self.out.push_str(self.nl);
    }

    fn break_line(&mut self) {
        self.out.push_str(self.nl);
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
    }

    fn entry(&mut self) {
        if let Some(first) = self.first.last_mut() {
            if !*first {
                self.out.push(',');
            }
            *first = false;
        }
        self.break_line();
    }

    pub fn begin_object(&mut self) {
        self.out.push('{');
        self.indent += 1;
        self.first.push(true);
    }

    pub fn end_object(&mut self, empty: bool) {
        self.close('}', empty);
    }

    pub fn begin_array(&mut self) {
        self.out.push('[');
        self.indent += 1;
        self.first.push(true);
    }

    pub fn end_array(&mut self, empty: bool) {
        self.close(']', empty);
    }

    fn close(&mut self, c: char, empty: bool) {
        self.indent -= 1;
        let had_entries = !self.first.pop().unwrap_or(true);
        if had_entries && !empty {
            self.break_line();
        }
        self.out.push(c);
    }

    pub fn key(&mut self, k: &str) {
        self.entry();
        write_str(&mut self.out, k);
        self.out.push_str(": ");
    }

    pub fn item(&mut self) {
        self.entry();
    }

    pub fn value(&mut self, v: &Value) {
        match v {
            Value::Object(o) if !o.is_empty() => {
                self.begin_object();
                for (k, v) in o {
                    self.key(k);
                    self.value(v);
                }
                self.end_object(false);
            }
            Value::Array(a) if !a.is_empty() => {
                self.begin_array();
                for v in a {
                    self.item();
                    self.value(v);
                }
                self.end_array(false);
            }
            Value::Object(_) => self.out.push_str("{}"),
            Value::Array(_) => self.out.push_str("[]"),
            Value::String(s) => write_str(&mut self.out, s),
            Value::Number(n) => write_number(&mut self.out, n),
            Value::Bool(b) => self.out.push_str(if *b { "true" } else { "false" }),
            Value::Null => self.out.push_str("null"),
        }
    }
}

fn write_str(out: &mut String, s: &str) {
    // serde_json's escaping matches JSON.stringify for valid UTF-8 strings.
    out.push_str(&serde_json::to_string(s).unwrap());
}

/// Formats numbers the way JavaScript does (integral floats without `.0`, JS exponent style).
pub fn write_number(out: &mut String, n: &serde_json::Number) {
    if n.is_f64() {
        out.push_str(&js_number(n.as_f64().unwrap()));
    } else {
        out.push_str(&n.to_string());
    }
}

pub fn js_number(f: f64) -> String {
    if !f.is_finite() {
        return "null".into();
    }
    if f == f.trunc() && f.abs() < 1e21 {
        return format!("{}", f as i128);
    }
    let abs = f.abs();
    if (1e-6..1e21).contains(&abs) {
        // Shortest round-trip digits, positional notation.
        return even_tie(format!("{}", f), f);
    }
    // Exponent notation: JS writes 1e-7 / 1.5e+21.
    let s = format!("{:e}", f);
    match s.split_once('e') {
        Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
        _ => s,
    }
}

/// Rust and JavaScript both print the shortest digits closest to the exact value, but on an
/// exact tie Rust rounds the last digit up where JavaScript picks the even one.
fn even_tie(s: String, f: f64) -> String {
    let last = s.as_bytes()[s.len() - 1];
    if !last.is_ascii_digit() || (last - b'0') % 2 == 0 || !s.contains('.') {
        return s;
    }
    let mut lower = s.clone();
    lower.pop();
    lower.push((last - 1) as char);
    if lower.parse::<f64>().ok() != Some(f) {
        return s;
    }
    let decimals = s.len() - s.find('.').unwrap() - 1;
    let exact = format!("{:.*}", decimals + 30, f.abs());
    let exact = exact.trim_end_matches('0');
    let midpoint = format!("{}5", lower.trim_start_matches('-'));
    if exact == midpoint {
        lower
    } else {
        s
    }
}

/// Converts a float to a JSON number, using an integer representation when integral.
pub fn number(f: f64) -> Value {
    if f == f.trunc() && f.abs() < 9e15 {
        Value::Number((f as i64).into())
    } else {
        serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
    }
}
