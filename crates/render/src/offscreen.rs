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
    let fmt = match format {
        "jpeg" | "jpg" => EncodedImageFormat::JPEG,
        "webp" => EncodedImageFormat::WEBP,
        _ => EncodedImageFormat::PNG,
    };
    let q = quality.unwrap_or(90);
    #[allow(deprecated)]
    img.encode(None, fmt, q).map(|d| d.as_bytes().to_vec())
}
