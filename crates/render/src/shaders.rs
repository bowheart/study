//! Shader fills: WebGL 1.0 fragment shaders translated to SkSL runtime effects.

use pen::style::{Rgba, Uniform};
use skia_safe::{Data, Matrix, RuntimeEffect, Shader};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug)]
struct UniformInfo {
    name: String,
    ty: String,
    role: Role,
    default: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Role {
    Resolution,
    Time,
    Value,
}

struct Compiled {
    effect: RuntimeEffect,
    uniforms: Vec<UniformInfo>,
}

#[derive(Default)]
pub struct Shaders {
    cache: HashMap<PathBuf, Option<Compiled>>,
    pub errors: HashMap<PathBuf, String>,
    /// Seconds, fed to `@time` uniforms.
    pub time: f32,
    pub uses_time: bool,
}

impl Shaders {
    pub fn shader(&mut self, path: PathBuf, user: &[(String, Uniform)], w: f32, h: f32) -> Option<Shader> {
        if !self.cache.contains_key(&path) {
            let compiled = match std::fs::read_to_string(&path) {
                Ok(src) => match compile(&src) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        self.errors.insert(path.clone(), e);
                        None
                    }
                },
                Err(e) => {
                    self.errors.insert(path.clone(), e.to_string());
                    None
                }
            };
            self.cache.insert(path.clone(), compiled);
        }
        let c = self.cache.get(&path)?.as_ref()?;
        let mut data = vec![0u8; c.effect.uniform_size()];
        for u in c.effect.uniforms() {
            let Some(info) = c.uniforms.iter().find(|i| i.name == u.name()) else { continue };
            let values: Vec<f32> = match info.role {
                Role::Resolution => vec![w, h],
                Role::Time => {
                    self.uses_time = true;
                    vec![self.time]
                }
                Role::Value => match user.iter().find(|(k, _)| *k == info.name).map(|(_, v)| v) {
                    Some(Uniform::Float(f)) => vec![*f],
                    Some(Uniform::Bool(b)) => vec![if *b { 1.0 } else { 0.0 }],
                    Some(Uniform::Color(c)) => color_values(*c, &info.ty),
                    Some(Uniform::Vec(v)) => v.clone(),
                    None => info.default.clone(),
                },
            };
            let off = u.offset();
            let is_int = info.ty.starts_with("int") || info.ty.starts_with("ivec") || info.ty == "bool";
            for (k, v) in values.iter().enumerate().take(u.size_in_bytes() / 4) {
                let bytes = if is_int { (*v as i32).to_le_bytes() } else { v.to_le_bytes() };
                data[off + k * 4..off + k * 4 + 4].copy_from_slice(&bytes);
            }
        }
        c.effect.make_shader(Data::new_copy(&data), &[], Some(&Matrix::default()))
    }
}

fn color_values(c: Rgba, ty: &str) -> Vec<f32> {
    if ty == "vec4" {
        c.0.to_vec()
    } else {
        c.0[..3].to_vec()
    }
}

/// Rewrites a GLSL ES fragment shader into an SkSL runtime effect.
fn compile(src: &str) -> Result<Compiled, String> {
    let uniforms = parse_uniforms(src);
    let has_res = uniforms.iter().any(|u| u.role == Role::Resolution);
    let res_name = uniforms.iter().find(|u| u.role == Role::Resolution).map(|u| u.name.clone());

    let mut body = String::new();
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with('#') || t.starts_with("precision ") {
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    // SkSL doesn't allow shadowing builtin functions with locals.
    for builtin in ["dot", "mix", "step", "length", "distance", "clamp", "smoothstep", "fract", "floor", "sign", "min", "max"] {
        body = rename_shadowed(&body, builtin);
    }
    // SkSL reserves the gl_ names, so they become parameters of a renamed main.
    body = body.replace("gl_FragCoord", "study_FragCoord").replace("gl_FragColor", "study_FragColor");
    body = body.replace("void main()", "void study_main(vec4 study_FragCoord, inout vec4 study_FragColor)");
    body = body.replace("void main(void)", "void study_main(vec4 study_FragCoord, inout vec4 study_FragColor)");
    let mut prelude = String::new();
    if !has_res {
        prelude.push_str("uniform vec2 study_resolution;\n");
    }
    let res = res_name.unwrap_or_else(|| "study_resolution".into());
    let sksl = format!(
        "{prelude}{body}\nhalf4 main(float2 p) {{\n  vec4 c = vec4(0.0);\n  study_main(vec4(p.x, {res}.y - p.y, 0.5, 1.0), c);\n  c = clamp(c, 0.0, 1.0);\n  return half4(c.rgb * c.a, c.a);\n}}\n"
    );
    let effect = RuntimeEffect::make_for_shader(&sksl, None).map_err(|e| e.to_string())?;
    let mut uniforms = uniforms;
    if !has_res {
        uniforms.push(UniformInfo { name: "study_resolution".into(), ty: "vec2".into(), role: Role::Resolution, default: vec![] });
    }
    Ok(Compiled { effect, uniforms })
}

/// `float dot = ...` declares a local named like a builtin; rename the declaration and its uses
/// (uses are recognized as the identifier not followed by `(`).
fn rename_shadowed(src: &str, name: &str) -> String {
    let decl = ["float", "vec2", "vec3", "vec4", "int"].iter().any(|ty| contains_word_seq(src, ty, name));
    if !decl {
        return src.to_string();
    }
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < src.len() {
        if src[i..].starts_with(name) {
            let before = if i == 0 { b' ' } else { bytes[i - 1] };
            let after_idx = i + name.len();
            let after = src[after_idx..].trim_start().bytes().next().unwrap_or(b' ');
            let word_before = before.is_ascii_alphanumeric() || before == b'_';
            let word_after = bytes.get(after_idx).is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
            if !word_before && !word_after && after != b'(' {
                out.push_str("study_");
                out.push_str(name);
                i = after_idx;
                continue;
            }
        }
        let ch = src[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn contains_word_seq(src: &str, ty: &str, name: &str) -> bool {
    let mut parts = src.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|s| !s.is_empty()).peekable();
    while let Some(p) = parts.next() {
        if p == ty && parts.peek() == Some(&name) {
            return true;
        }
    }
    false
}

/// Reads `uniform <type> <name>;` declarations and their `/** @... */` annotations.
fn parse_uniforms(src: &str) -> Vec<UniformInfo> {
    let mut out = Vec::new();
    let mut pending_doc = String::new();
    let mut in_doc = false;
    for line in src.lines() {
        let t = line.trim();
        if in_doc || t.starts_with("/*") {
            pending_doc.push_str(t);
            pending_doc.push(' ');
            in_doc = !t.contains("*/");
            if !t.starts_with("uniform") {
                continue;
            }
        }
        if let Some(rest) = t.strip_prefix("uniform ") {
            let decl = rest.split(';').next().unwrap_or("");
            let mut words = decl.split_whitespace().filter(|w| !matches!(*w, "lowp" | "mediump" | "highp"));
            let (Some(ty), Some(name)) = (words.next(), words.next()) else { continue };
            let doc = std::mem::take(&mut pending_doc);
            let role = if doc.contains("@resolution") {
                Role::Resolution
            } else if doc.contains("@time") {
                Role::Time
            } else {
                Role::Value
            };
            let default = parse_default(&doc, ty);
            out.push(UniformInfo { name: name.to_string(), ty: ty.to_string(), role, default });
        } else if !t.is_empty() {
            pending_doc.clear();
        }
    }
    out
}

fn parse_default(doc: &str, ty: &str) -> Vec<f32> {
    let Some(i) = doc.find("@default") else { return vec![0.0; components(ty)] };
    let rest = doc[i + 8..].trim_start();
    let token: String = rest.split(['@', '*']).next().unwrap_or("").trim().to_string();
    if token.starts_with('#') {
        if let Some(c) = Rgba::parse(token.split_whitespace().next().unwrap_or("")) {
            return color_values(c, ty);
        }
    }
    let nums: Vec<f32> = token.split([',', ' ']).filter_map(|s| s.trim().parse().ok()).collect();
    match token.as_str() {
        "true" => vec![1.0],
        "false" => vec![0.0],
        _ if nums.is_empty() => vec![0.0; components(ty)],
        _ => nums,
    }
}

fn components(ty: &str) -> usize {
    match ty {
        "vec2" | "ivec2" => 2,
        "vec3" | "ivec3" => 3,
        "vec4" | "ivec4" => 4,
        _ => 1,
    }
}
