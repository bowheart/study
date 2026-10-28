//! Renders root frames of a .pen file to PNGs: `snap <file.pen> <out_dir> [name-filter] [scale]`
use pen::scene::compile_root;
use pen::vars::Vars;
use std::time::{Duration, Instant};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = std::path::PathBuf::from(&args[1]);
    let out = std::path::PathBuf::from(&args[2]);
    let filter = args.get(3).cloned().unwrap_or_default();
    let scale: f32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1.0);
    std::fs::create_dir_all(&out).unwrap();
    let t = Instant::now();
    let doc = pen::Document::load(&path).unwrap();
    println!("load {:?} ({} nodes)", t.elapsed(), doc.node_count());
    let vars = Vars::new(&doc);
    let mut r = render::Renderer::new();
    r.base_dir = path.parent().unwrap().to_path_buf();
    let t = Instant::now();
    let mut scenes: Vec<_> = doc.roots.iter().map(|&root| compile_root(&doc, &vars, root)).collect();
    println!("compile {:?} ({} scene nodes)", t.elapsed(), scenes.iter().map(|s| s.nodes.len()).sum::<usize>());
    for pass in 0..3 {
        let t = Instant::now();
        for s in scenes.iter_mut() {
            pen::layout::layout(s, &mut r.text);
            pen::layout::compute_bounds(s);
        }
        println!("layout pass {pass} {:?}", t.elapsed());
        // Fonts are fetched during the first layout; wait and redo with real metrics.
        if !r.has_pending() && pass > 0 { break; }
        let t = Instant::now();
        r.wait_for_assets(Duration::from_secs(60));
        println!("assets {:?}", t.elapsed());
    }
    let refs: Vec<&pen::scene::RootScene> = scenes.iter().collect();
    for s in &scenes {
        let root = s.root().unwrap();
        let name = root.name.as_deref().unwrap_or(&root.id).to_string();
        if !filter.is_empty() && !name.to_lowercase().contains(&filter.to_lowercase()) { continue; }
        let region = s.abs_rect(0);
        // First draw requests icons/images; wait, then draw for real.
        render::offscreen::render_region(&mut r, &refs, region, scale, None);
        r.wait_for_assets(Duration::from_secs(60));
        let t = Instant::now();
        let img = render::offscreen::render_region(&mut r, &refs, region, scale, Some(skia_safe::Color::WHITE)).unwrap();
        let bytes = render::offscreen::encode(&img, "png", None).unwrap();
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
        let file = out.join(format!("{safe}.png"));
        std::fs::write(&file, bytes).unwrap();
        println!("{} {}x{} in {:?} -> {}", name, region.w, region.h, t.elapsed(), file.display());
    }
    for (p, e) in &r.shaders.errors { println!("SHADER ERROR {}: {e}", p.display()); }
}
