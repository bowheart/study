//! Loads each .pen file and checks that saving reproduces it byte-for-byte.
fn main() {
    let mut bad = 0;
    for path in std::env::args().skip(1) {
        let text = std::fs::read_to_string(&path).unwrap();
        let t = std::time::Instant::now();
        let doc = pen::Document::parse(&text).unwrap();
        let parsed = t.elapsed();
        let out = doc.serialize();
        if out != text {
            bad += 1;
            let at = out.bytes().zip(text.bytes()).position(|(a, b)| a != b).unwrap_or(out.len().min(text.len()));
            let lo = at.saturating_sub(80);
            println!("MISMATCH {path} at {at} (len {} vs {})\n--- original\n{}\n--- ours\n{}", text.len(), out.len(),
                &text[lo..(at + 80).min(text.len())], &out[lo..(at + 80).min(out.len())]);
        } else {
            println!("ok {path} ({} nodes, parse {:?})", doc.node_count(), parsed);
        }
    }
    std::process::exit(bad);
}
