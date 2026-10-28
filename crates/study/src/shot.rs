//! Screenshots for TakeScreenshot: renders a node's region as it appears on the canvas.

use crate::engine::Engine;
use anyhow::{anyhow, Result};
use pen::scene::Rect;
use render::offscreen::{encode, render_region};
use std::time::Duration;

/// Largest screenshot edge in pixels; bigger nodes are scaled down.
const MAX_EDGE: f32 = 1568.0;

pub fn region_of(engine: &Engine, id: &str) -> Result<Rect> {
    if id == "document" {
        return engine.doc_bounds().ok_or_else(|| anyhow!("the document is empty"));
    }
    let (s, i) = engine.locate(id).ok_or_else(|| anyhow!("no rendered node '{id}' (is it disabled?)"))?;
    let rotated = {
        let mut cur = i;
        let mut any = false;
        while cur != pen::scene::NONE {
            any |= s.nodes[cur as usize].rotation != 0.0;
            cur = s.nodes[cur as usize].parent;
        }
        any
    };
    Ok(if rotated { s.bounds[i as usize] } else { s.abs_rect(i) })
}

/// Renders a region at `scale`, waiting for fonts/icons/images first.
pub fn render(engine: &mut Engine, region: Rect, scale: f32, format: &str, quality: Option<u32>) -> Result<Vec<u8>> {
    if region.w <= 0.0 || region.h <= 0.0 {
        return Err(anyhow!("node has zero size"));
    }
    // A first pass requests any icons/images in view; then wait for them.
    {
        let scenes: Vec<&pen::scene::RootScene> = engine.doc.roots.iter().filter_map(|r| engine.scenes.get(r)).collect();
        let _ = render_region(&mut engine.renderer, &scenes, region, scale.min(0.25), None);
    }
    if engine.renderer.wait_for_assets(Duration::from_secs(30)) {
        engine.relayout_all();
    }
    let scenes: Vec<&pen::scene::RootScene> = engine.doc.roots.iter().filter_map(|r| engine.scenes.get(r)).collect();
    let bg = if format == "jpeg" { Some(skia_safe::Color::WHITE) } else { None };
    let img = render_region(&mut engine.renderer, &scenes, region, scale, bg).ok_or_else(|| anyhow!("region too large to render"))?;
    encode(&img, format, quality).ok_or_else(|| anyhow!("could not encode {format}"))
}

pub fn screenshot(engine: &mut Engine, id: &str) -> Result<Vec<u8>> {
    let region = region_of(engine, id)?;
    let scale = (MAX_EDGE / region.w.max(region.h)).min(2.0);
    render(engine, region, scale, "png", None)
}
