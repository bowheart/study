//! Shader fills: WebGL 1.0 fragment shaders translated to SkSL runtime effects.
//!
//! Supported annotations (doc comments on uniforms): `@resolution`, `@time`, `@mouse`, `@default`,
//! and on `sampler2D` uniforms `@sdf` (signed distance field of the node's shape) and `@backdrop`
//! (what is drawn behind the node). Other `sampler2D` uniforms take an image URL as their value.
//! Textures are sampled as if uploaded with UNPACK_FLIP_Y, so `uv` (0,0) is the bottom-left like
//! `gl_FragCoord`.

use pen::style::{Rgba, Uniform};
use skia_safe::runtime_effect::{ChildPtr, RuntimeShaderBuilder};
use skia_safe::{Color, Data, Image, Matrix, RuntimeEffect, SamplingOptions, Shader, TileMode};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug)]
struct UniformInfo {
    name: String,
    ty: String,
    role: Role,
    default: Vec<f32>,
    default_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Role {
    Resolution,
    Time,
    Mouse,
    Value,
    /// sampler2D roles
    Image,
    Sdf,
    Backdrop,
}

struct Compiled {
    effect: RuntimeEffect,
    uniforms: Vec<UniformInfo>,
    backdrop: Option<String>,
}

/// What a shader fill needs from the renderer while it is being built.
pub struct Ctx<'a> {
    pub w: f32,
    pub h: f32,
    /// Pointer position in the node's local space (y down), if over the window.
    pub mouse: Option<(f32, f32)>,
    pub image: &'a mut dyn FnMut(&str) -> Option<Image>,
    pub sdf: &'a mut dyn FnMut() -> Option<Image>,
}

pub enum Built {
    Shader(Shader),
    /// Needs the backdrop: run as a backdrop image filter. The caller sets the
    /// `study_to_local` / `study_from_local` matrices and names `child` as the input.
    Backdrop { builder: RuntimeShaderBuilder, child: String },
}

#[derive(Default)]
pub struct Shaders {
    cache: HashMap<PathBuf, Option<Compiled>>,
    pub errors: HashMap<PathBuf, String>,
    /// Seconds, fed to `@time` uniforms.
    pub time: f32,
    /// Set when a drawn shader read `@time` / `@mouse` (the caller should keep redrawing).
    pub uses_time: bool,
    pub uses_mouse: bool,
}

impl Shaders {
    pub fn shader(&mut self, path: PathBuf, user: &[(String, Uniform)], ctx: Ctx) -> Option<Built> {
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
        let (w, h) = (ctx.w, ctx.h);

        // Children (sampler2D) in declaration order, with their texel sizes.
        let mut children: Vec<ChildPtr> = Vec::new();
        let mut sizes: HashMap<String, [f32; 2]> = HashMap::new();
        for info in c.uniforms.iter().filter(|u| matches!(u.role, Role::Image | Role::Sdf | Role::Backdrop)) {
            let img = match info.role {
                Role::Image => {
                    let url = match user.iter().find(|(k, _)| *k == info.name).map(|(_, v)| v) {
                        Some(Uniform::Text(s)) => Some(s.clone()),
                        _ => info.default_url.clone(),
                    };
                    url.and_then(|u| (ctx.image)(&u))
                }
                Role::Sdf => (ctx.sdf)(),
                _ => None,
            };
            let size = match (&img, info.role) {
                (_, Role::Backdrop) => [w, h],
                (Some(i), _) => [i.width() as f32, i.height() as f32],
                (None, _) => [1.0, 1.0],
            };
            sizes.insert(info.name.clone(), size);
            if info.role != Role::Backdrop {
                let s = img
                    .and_then(|i| i.to_shader((TileMode::Clamp, TileMode::Clamp), SamplingOptions::default(), None))
                    .unwrap_or_else(|| skia_safe::shaders::color(Color::TRANSPARENT));
                children.push(s.into());
            }
        }

        // Uniform values, by name.
        let mut values: Vec<(String, Vec<f32>, bool)> = Vec::new();
        for info in &c.uniforms {
            let v: Vec<f32> = match info.role {
                Role::Resolution => vec![w, h],
                Role::Time => {
                    self.uses_time = true;
                    vec![self.time]
                }
                Role::Mouse => {
                    self.uses_mouse = true;
                    let (mx, my) = ctx.mouse.unwrap_or((-1.0, -1.0));
                    vec![mx, h - my]
                }
                Role::Value => match user.iter().find(|(k, _)| *k == info.name).map(|(_, v)| v) {
                    Some(Uniform::Float(f)) => vec![*f],
                    Some(Uniform::Bool(b)) => vec![if *b { 1.0 } else { 0.0 }],
                    Some(Uniform::Color(c)) => color_values(*c, &info.ty),
                    Some(Uniform::Vec(v)) => v.clone(),
                    Some(Uniform::Text(_)) | None => info.default.clone(),
                },
                Role::Image | Role::Sdf | Role::Backdrop => {
                    values.push((format!("study_size_{}", info.name), sizes[&info.name].to_vec(), false));
                    continue;
                }
            };
            let is_int = info.ty.starts_with("int") || info.ty.starts_with("ivec") || info.ty == "bool";
            values.push((info.name.clone(), v, is_int));
        }
        let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        values.push(("study_to_local".into(), identity.to_vec(), false));
        values.push(("study_from_local".into(), identity.to_vec(), false));

        if let Some(child) = &c.backdrop {
            let mut b = RuntimeShaderBuilder::new(c.effect.clone());
            for (name, v, is_int) in &values {
                let Some(u) = c.effect.find_uniform(name) else { continue };
                let n = u.size_in_bytes() / 4;
                let mut v = v.clone();
                v.resize(n, 0.0);
                if *is_int {
                    let ints: Vec<i32> = v.iter().map(|f| *f as i32).collect();
                    let _ = b.set_uniform_int(name, &ints);
                } else {
                    let _ = b.set_uniform_float(name, &v);
                }
            }
            return Some(Built::Backdrop { builder: b, child: child.clone() });
        }

        let mut data = vec![0u8; c.effect.uniform_size()];
        for (name, v, is_int) in &values {
            let Some(u) = c.effect.find_uniform(name) else { continue };
            let off = u.offset();
            for (k, x) in v.iter().enumerate().take(u.size_in_bytes() / 4) {
                let bytes = if *is_int { (*x as i32).to_le_bytes() } else { x.to_le_bytes() };
                data[off + k * 4..off + k * 4 + 4].copy_from_slice(&bytes);
            }
        }
        c.effect.make_shader(Data::new_copy(&data), &children, Some(&Matrix::default())).map(Built::Shader)
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
    let res_name = uniforms.iter().find(|u| u.role == Role::Resolution).map(|u| u.name.clone());
    let samplers: Vec<&UniformInfo> = uniforms.iter().filter(|u| matches!(u.role, Role::Image | Role::Sdf | Role::Backdrop)).collect();
    let res = res_name.clone().unwrap_or_else(|| "study_resolution".into());

    let mut body = String::new();
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with('#') || t.starts_with("precision ") {
            continue;
        }
        if let Some(s) = samplers.iter().find(|s| is_sampler_decl(t, &s.name)) {
            // A sampler becomes a child shader plus a size and a texture2D stand-in.
            let n = &s.name;
            body.push_str(&format!("uniform shader {n};\nuniform vec2 study_size_{n};\n"));
            let sample = if s.role == Role::Backdrop {
                format!("{n}.eval((study_from_local * vec3(vec2(uv.x, 1.0 - uv.y) * {res}, 1.0)).xy)")
            } else {
                format!("{n}.eval(vec2(uv.x, 1.0 - uv.y) * study_size_{n})")
            };
            body.push_str(&format!("vec4 study_tex_{n}(vec2 uv) {{ return unpremul({sample}); }}\n"));
            continue;
        }
        body.push_str(line);
        body.push('\n');
    }
    // SkSL doesn't allow shadowing builtin functions with locals.
    for builtin in ["dot", "mix", "step", "length", "distance", "clamp", "smoothstep", "fract", "floor", "sign", "min", "max"] {
        body = rename_shadowed(&body, builtin);
    }
    body = rewrite_calls(&body, "texture2D", |name, _| format!("study_tex_{name}("), false);
    body = rewrite_calls(&body, "textureSize", |name, _| format!("ivec2(study_size_{name})"), true);
    // SkSL reserves the gl_ names, so they become parameters of a renamed main.
    body = body.replace("gl_FragCoord", "study_FragCoord").replace("gl_FragColor", "study_FragColor");
    body = body.replace("void main()", "void study_main(vec4 study_FragCoord, inout vec4 study_FragColor)");
    body = body.replace("void main(void)", "void study_main(vec4 study_FragCoord, inout vec4 study_FragColor)");
    let mut prelude = String::from("uniform float3x3 study_to_local;\nuniform float3x3 study_from_local;\n");
    if res_name.is_none() {
        prelude.push_str("uniform vec2 study_resolution;\n");
    }
    let sksl = format!(
        "{prelude}{body}\nhalf4 main(float2 p) {{\n  float2 q = (study_to_local * float3(p, 1.0)).xy;\n  vec4 c = vec4(0.0);\n  study_main(vec4(q.x, {res}.y - q.y, 0.5, 1.0), c);\n  c = clamp(c, 0.0, 1.0);\n  return half4(c.rgb * c.a, c.a);\n}}\n"
    );
    let effect = RuntimeEffect::make_for_shader(&sksl, None).map_err(|e| e.to_string())?;
    let mut uniforms = uniforms;
    if res_name.is_none() {
        uniforms.push(UniformInfo { name: "study_resolution".into(), ty: "vec2".into(), role: Role::Resolution, default: vec![], default_url: None });
    }
    let backdrop = uniforms.iter().find(|u| u.role == Role::Backdrop).map(|u| u.name.clone());
    Ok(Compiled { effect, uniforms, backdrop })
}

fn is_sampler_decl(line: &str, name: &str) -> bool {
    let Some(rest) = line.strip_prefix("uniform ") else { return false };
    let words: Vec<&str> = rest.split(|c: char| c.is_whitespace() || c == ';').filter(|w| !w.is_empty()).collect();
    words.contains(&"sampler2D") && words.last() == Some(&name)
}

/// Replaces `func(name, ...rest)` calls. With `whole`, the entire call is replaced; otherwise just
/// `func(name,` (the remaining arguments stay).
fn rewrite_calls(src: &str, func: &str, f: impl Fn(&str, &str) -> String, whole: bool) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(i) = rest.find(func) {
        let before_ok = rest[..i].chars().last().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let after = &rest[i + func.len()..];
        let open = after.trim_start();
        if !before_ok || !open.starts_with('(') {
            out.push_str(&rest[..i + func.len()]);
            rest = after;
            continue;
        }
        let args = &open[1..];
        let name_len = args.trim_start().find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(0);
        let lead = args.len() - args.trim_start().len();
        let name = &args[lead..lead + name_len];
        let after_name = args[lead + name_len..].trim_start();
        if name.is_empty() || !(after_name.starts_with(',') || after_name.starts_with(')')) {
            out.push_str(&rest[..i + func.len()]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..i]);
        if whole {
            // Skip to the matching close paren.
            let mut depth = 0;
            let mut end = 0;
            for (k, ch) in open.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = k + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            out.push_str(&f(name, ""));
            rest = &open[end..];
        } else {
            out.push_str(&f(name, ""));
            rest = after_name.strip_prefix(',').unwrap_or(after_name);
        }
    }
    out.push_str(rest);
    out
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
        if in_doc || t.starts_with("/*") || t.starts_with("//") {
            pending_doc.push_str(t);
            pending_doc.push(' ');
            in_doc = t.starts_with("/*") && !t.contains("*/") || in_doc && !t.contains("*/");
            if !t.starts_with("uniform") {
                continue;
            }
        }
        if let Some(rest) = t.strip_prefix("uniform ") {
            let decl = rest.split(';').next().unwrap_or("");
            let mut words = decl.split_whitespace().filter(|w| !matches!(*w, "lowp" | "mediump" | "highp"));
            let (Some(ty), Some(name)) = (words.next(), words.next()) else { continue };
            let doc = std::mem::take(&mut pending_doc);
            let role = if ty == "sampler2D" {
                if doc.contains("@sdf") {
                    Role::Sdf
                } else if doc.contains("@backdrop") {
                    Role::Backdrop
                } else {
                    Role::Image
                }
            } else if doc.contains("@resolution") {
                Role::Resolution
            } else if doc.contains("@time") {
                Role::Time
            } else if doc.contains("@mouse") {
                Role::Mouse
            } else {
                Role::Value
            };
            let default_url = (role == Role::Image).then(|| default_token(&doc)).flatten().map(|s| s.trim_matches('"').to_string());
            let default = parse_default(&doc, ty);
            out.push(UniformInfo { name: name.to_string(), ty: ty.to_string(), role, default, default_url });
        } else if !t.is_empty() {
            pending_doc.clear();
        }
    }
    out
}

fn default_token(doc: &str) -> Option<String> {
    let i = doc.find("@default")?;
    let token: String = doc[i + 8..].trim_start().split(['@', '*']).next().unwrap_or("").trim().to_string();
    (!token.is_empty()).then_some(token)
}

fn parse_default(doc: &str, ty: &str) -> Vec<f32> {
    let Some(token) = default_token(doc) else { return vec![0.0; components(ty)] };
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

/// Signed distance field of a shape as an RGBA float image: r = distance in node units (positive
/// inside), g/b = unit gradient (direction of increasing distance) in texel space, y up.
pub fn sdf_image(path: &skia_safe::Path, w: f32, h: f32) -> Option<Image> {
    const MAX: f32 = 512.0;
    let scale = (MAX / w.max(h)).min(1.0);
    let (tw, th) = ((w * scale).ceil().max(1.0) as usize, (h * scale).ceil().max(1.0) as usize);
    let mut mask = skia_safe::surfaces::raster_n32_premul((tw as i32, th as i32))?;
    mask.canvas().clear(Color::TRANSPARENT);
    mask.canvas().scale((scale, scale));
    let mut p = skia_safe::Paint::default();
    p.set_anti_alias(false);
    mask.canvas().draw_path(path, &p);
    let info = skia_safe::ImageInfo::new((tw as i32, th as i32), skia_safe::ColorType::Alpha8, skia_safe::AlphaType::Premul, None);
    let mut a = vec![0u8; tw * th];
    mask.image_snapshot().read_pixels(&info, &mut a, tw, (0, 0), skia_safe::image::CachingHint::Allow).then_some(())?;
    let inside: Vec<bool> = a.iter().map(|v| *v > 127).collect();
    let d_out = edt(&inside, tw, th, false); // distance from outside pixels to the shape
    let d_in = edt(&inside, tw, th, true); // distance from inside pixels to the outside
    let sd: Vec<f32> = (0..tw * th).map(|i| if inside[i] { d_in[i] } else { -d_out[i] } / scale).collect();
    let mut px = vec![0f32; tw * th * 4];
    for y in 0..th {
        for x in 0..tw {
            let at = |x: usize, y: usize| sd[y * tw + x];
            let gx = at((x + 1).min(tw - 1), y) - at(x.saturating_sub(1), y);
            let gy = at(x, y.saturating_sub(1)) - at(x, (y + 1).min(th - 1)); // y up
            let len = (gx * gx + gy * gy).sqrt().max(1e-6);
            // Texture rows are stored bottom-up to match the flipped sampling.
            let o = ((th - 1 - y) * tw + x) * 4;
            px[o..o + 4].copy_from_slice(&[at(x, y), gx / len, gy / len, 1.0]);
        }
    }
    let info = skia_safe::ImageInfo::new((tw as i32, th as i32), skia_safe::ColorType::RGBAF16, skia_safe::AlphaType::Unpremul, None);
    // Half floats: GPU backends support them as textures where 32-bit floats may not be.
    let bytes: Vec<u8> = px.iter().flat_map(|f| f16_bits(*f).to_le_bytes()).collect();
    skia_safe::images::raster_from_data(&info, Data::new_copy(&bytes), tw * 8)
}

/// Euclidean distance transform: distance from each pixel to the nearest pixel where
/// `inside[i] != target`... i.e. for `target = false`, distances to the shape; for `true`, to the outside.
fn edt(inside: &[bool], w: usize, h: usize, from_inside: bool) -> Vec<f32> {
    const INF: f32 = 1e20;
    // Feature pixels (distance 0) are the ones on the other side.
    let mut f: Vec<f32> = inside.iter().map(|&i| if i != from_inside { 0.0 } else { INF }).collect();
    let mut buf = vec![0f32; w.max(h)];
    for x in 0..w {
        let col: Vec<f32> = (0..h).map(|y| f[y * w + x]).collect();
        edt_1d(&col, &mut buf[..h]);
        for y in 0..h {
            f[y * w + x] = buf[y];
        }
    }
    for y in 0..h {
        let row: Vec<f32> = f[y * w..(y + 1) * w].to_vec();
        edt_1d(&row, &mut buf[..w]);
        f[y * w..(y + 1) * w].copy_from_slice(&buf[..w]);
    }
    f.iter().map(|d| d.sqrt()).collect()
}

/// Felzenszwalb & Huttenlocher's 1D squared distance transform.
fn edt_1d(f: &[f32], d: &mut [f32]) {
    let n = f.len();
    let mut v = vec![0usize; n];
    let mut z = vec![0f32; n + 1];
    let mut k = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * q as f32 - 2.0 * p as f32);
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                // k == 0: replace the first parabola.
                v[0] = q;
                z[1] = f32::INFINITY;
                break;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
            break;
        }
    }
    k = 0;
    for q in 0..n {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        d[q] = (q as f32 - p as f32).powi(2) + f[p];
    }
}

/// IEEE 754 half-precision bits for an f32 (round to nearest, clamped to the finite range).
fn f16_bits(v: f32) -> u16 {
    let v = v.clamp(-65504.0, 65504.0);
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
    let man = b & 0x7f_ffff;
    if exp <= 0 {
        if exp < -10 {
            return sign;
        }
        // Subnormal.
        let m = (man | 0x80_0000) >> (1 - exp);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    let h = ((exp as u32) << 10) | (man >> 13);
    let round = (man >> 12) & 1;
    sign | (h + round).min(0x7bff) as u16
}
