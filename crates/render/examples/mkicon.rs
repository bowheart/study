//! Renders `assets/logo.svg` into app icons: `mkicon <logo.svg> <out-dir>` →
//! `study.ico` (Windows, 16–256 px PNG entries), `icon-256.png` (window icon) and
//! `study.iconset/` (macOS; turn it into an .icns with `iconutil -c icns`).
use skia_safe::{svg, surfaces, Color, FontMgr};

fn render(svg_text: &[u8], px: i32) -> Vec<u8> {
    let mut dom = svg::Dom::from_bytes(svg_text, FontMgr::new()).expect("parse svg");
    let mut s = surfaces::raster_n32_premul((px, px)).unwrap();
    s.canvas().clear(Color::TRANSPARENT);
    s.canvas().scale((px as f32 / 100.0, px as f32 / 100.0));
    dom.set_container_size((100.0, 100.0));
    dom.render(s.canvas());
    render::offscreen::encode(&s.image_snapshot(), "png", None).unwrap()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let text = std::fs::read(&a[0]).expect("read svg");
    let out = std::path::Path::new(&a[1]);
    std::fs::create_dir_all(out).unwrap();

    let pngs: Vec<(i32, Vec<u8>)> = [16, 20, 24, 32, 40, 48, 64, 128, 256].iter().map(|&px| (px, render(&text, px))).collect();
    std::fs::write(out.join("icon-256.png"), &pngs.last().unwrap().1).unwrap();
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

    let set = out.join("study.iconset");
    std::fs::create_dir_all(&set).unwrap();
    for base in [16, 32, 128, 256, 512] {
        std::fs::write(set.join(format!("icon_{base}x{base}.png")), render(&text, base)).unwrap();
        std::fs::write(set.join(format!("icon_{base}x{base}@2x.png")), render(&text, base * 2)).unwrap();
    }
}
