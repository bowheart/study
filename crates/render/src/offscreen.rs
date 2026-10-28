//! Rendering regions of the document to raster images (screenshots, exports).

use crate::draw::{Renderer, View};
use pen::scene::{Rect, RootScene};
use skia_safe::{surfaces, Color, EncodedImageFormat, Image};

/// Renders everything visible inside `region` (document coords) at `scale`.
pub fn render_region(r: &mut Renderer, scenes: &[&RootScene], region: Rect, scale: f32, background: Option<Color>) -> Option<Image> {
    let w = (region.w * scale).ceil().max(1.0) as i32;
    let h = (region.h * scale).ceil().max(1.0) as i32;
    if w as i64 * h as i64 > 16384 * 16384 {
        return None;
    }
    let mut surface = surfaces::raster_n32_premul((w, h))?;
    let canvas = surface.canvas();
    canvas.clear(background.unwrap_or(Color::TRANSPARENT));
    canvas.scale((scale, scale));
    canvas.translate((-region.x, -region.y));
    let view = View { scale, viewport: region, lod: false };
    for s in scenes {
        r.draw_scene(canvas, s, &view);
    }
    Some(surface.image_snapshot())
}

pub fn encode(img: &Image, format: &str, quality: Option<u32>) -> Option<Vec<u8>> {
    if format == "webp" {
        return encode_webp(img);
    }
    let fmt = match format {
        "jpeg" | "jpg" => EncodedImageFormat::JPEG,
        _ => EncodedImageFormat::PNG,
    };
    let q = quality.unwrap_or(90);
    #[allow(deprecated)]
    img.encode(None, fmt, q).map(|d| d.as_bytes().to_vec())
}

/// Lossless WebP (the prebuilt Skia has no WebP encoder).
fn encode_webp(img: &Image) -> Option<Vec<u8>> {
    let (w, h) = (img.width(), img.height());
    let info = skia_safe::ImageInfo::new((w, h), skia_safe::ColorType::RGBA8888, skia_safe::AlphaType::Unpremul, None);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    if !img.read_pixels(&info, &mut rgba, (w * 4) as usize, (0, 0), skia_safe::image::CachingHint::Allow) {
        return None;
    }
    let mut out = Vec::new();
    image_webp::WebPEncoder::new(&mut out).encode(&rgba, w as u32, h as u32, image_webp::ColorType::Rgba8).ok()?;
    Some(out)
}
