//! Export: raster images, PDF, and HTML (plain CSS or Tailwind).

use crate::engine::Engine;
use crate::shot::{region_of, render};
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn export(engine: &mut Engine, ids: &[String], format: &str, output: &str, opts: &Value) -> Result<Vec<String>> {
    let base = engine.path().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let out = if Path::new(output).is_absolute() { PathBuf::from(output) } else { base.join(output.trim_start_matches("./").trim_start_matches(".\\")) };
    let out: PathBuf = out.components().collect();
    let scale = opts.get("scale").and_then(Value::as_f64).unwrap_or(2.0) as f32;
    let quality = opts.get("quality").and_then(Value::as_u64).map(|q| q.clamp(1, 100) as u32);
    match format {
        "png" | "jpeg" => {
            std::fs::create_dir_all(&out)?;
            let mut files = Vec::new();
            for id in ids {
                let region = region_of(engine, id)?;
                let bytes = render(engine, region, scale, format, quality)?;
                let file = out.join(format!("{}.{}", id.replace('/', "_"), if format == "jpeg" { "jpg" } else { "png" }));
                std::fs::write(&file, bytes)?;
                files.push(file.display().to_string());
            }
            Ok(files)
        }
        "webp" => bail!("webp export isn't available in this build of study; use png or jpeg"),
        "pdf" => {
            std::fs::create_dir_all(&out)?;
            let file = out.join("export.pdf");
            pdf(engine, ids, &file)?;
            Ok(vec![file.display().to_string()])
        }
        "html-css" | "html-tailwind" => {
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let html = crate::html::export(engine, ids, format == "html-tailwind", opts, out.parent().unwrap_or(&base))?;
            std::fs::write(&out, html)?;
            Ok(vec![out.display().to_string()])
        }
        _ => Err(anyhow!("unknown format {format}")),
    }
}

fn pdf(engine: &mut Engine, ids: &[String], file: &Path) -> Result<()> {
    // Make sure assets are in before drawing vector pages.
    for id in ids {
        let region = region_of(engine, id)?;
        let _ = render(engine, region, 0.1, "png", None);
    }
    let mut bytes: Vec<u8> = Vec::new();
    {
        let mut doc = skia_safe::pdf::new_document(&mut bytes, None);
        for id in ids {
            let region = region_of(engine, id)?;
            let mut page = doc.begin_page((region.w, region.h), None);
            {
                let canvas = page.canvas();
                canvas.translate((-region.x, -region.y));
                let view = render::View { scale: 1.0, viewport: region, lod: false };
                let scenes: Vec<&pen::scene::RootScene> = engine.doc.roots.iter().filter_map(|r| engine.scenes.get(r)).collect();
                for s in scenes {
                    engine.renderer.draw_scene(canvas, s, &view);
                }
            }
            doc = page.end_page();
        }
        doc.close();
    }
    std::fs::write(file, bytes)?;
    Ok(())
}
