//! Renders `assets/logo.svg` into the app icon: `mkicon <logo.svg> <out-dir>` →
//! `study.ico` (16–256 px, PNG entries) and `icon-256.png`.
use skia_safe::{svg, surfaces, Color, FontMgr};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let text = std::fs::read(&a[0]).expect("read svg");
    let out = std::path::Path::new(&a[1]);
    let sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256];
    let mut pngs = Vec::new();
    for &px in &sizes {
        let mut dom = svg::Dom::from_bytes(&text, FontMgr::new()).expect("parse svg");
        dom.set_container_size((px as f32, px as f32));
        let mut s = surfaces::raster_n32_premul((px, px)).unwrap();
        s.canvas().clear(Color::TRANSPARENT);
        s.canvas().scale((px as f32 / 100.0, px as f32 / 100.0));
        dom.set_container_size((100.0, 100.0));
        dom.render(s.canvas());
        let png = render::offscreen::encode(&s.image_snapshot(), "png", None).unwrap();
        if px == 256 {
            std::fs::write(out.join("icon-256.png"), &png).unwrap();
        }
        pngs.push((px, png));
    }
    let mut ico = vec![0u8, 0, 1, 0];
    ico.extend((pngs.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * pngs.len() as u32;
    for (px, png) in &pngs {
        let d = if *px >= 256 { 0 } else { *px as u8 };
        ico.extend([d, d, 0, 0]);
        ico.extend(1u16.to_le_bytes());
        ico.extend(32u16.to_le_bytes());
        ico.extend((png.len() as u32).to_le_bytes());
        ico.extend(offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &pngs {
        ico.extend(png);
    }
    std::fs::write(out.join("study.ico"), ico).unwrap();
}
