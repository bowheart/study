//! The web build's font formats: Google Fonts sends WOFF2, or WOFF to some user agents
//! (`Fonts::typeface` unpacks both).

use super::Fonts;
use std::io::Write;

const INTER_LATIN: &[u8] = include_bytes!("../../../web/fonts/inter-latin.woff2");
const INTER_LATIN_EXT: &[u8] = include_bytes!("../../../web/fonts/inter-latin-ext.woff2");

/// A WOFF of `sfnt`, compressing the tables that get smaller.
fn encode(sfnt: &[u8]) -> Vec<u8> {
    let u16_at = |at: usize| u16::from_be_bytes([sfnt[at], sfnt[at + 1]]);
    let u32_at = |at: usize| u32::from_be_bytes(sfnt[at..at + 4].try_into().unwrap());
    let count = u16_at(4) as usize;
    let mut dir = Vec::new();
    let mut body = Vec::new();
    let mut at = 44 + 20 * count;
    for i in 0..count {
        let r = 12 + 16 * i;
        let (tag, sum, offset, len) = (u32_at(r), u32_at(r + 4), u32_at(r + 8) as usize, u32_at(r + 12) as usize);
        let data = &sfnt[offset..offset + len];
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(data).unwrap();
        let z = z.finish().unwrap();
        let stored = if z.len() < data.len() { z } else { data.to_vec() };
        for v in [tag, at as u32, stored.len() as u32, len as u32, sum] {
            dir.extend(v.to_be_bytes());
        }
        at += stored.len().next_multiple_of(4);
        body.extend(&stored);
        body.resize(body.len().next_multiple_of(4), 0);
    }
    let mut out = b"wOFF".to_vec();
    out.extend(u32_at(0).to_be_bytes());
    out.extend(((44 + dir.len() + body.len()) as u32).to_be_bytes());
    out.extend((count as u16).to_be_bytes());
    out.extend([0u8; 2]);
    out.extend((sfnt.len() as u32).to_be_bytes());
    out.extend([0u8; 24]);
    out.extend(dir);
    out.extend(body);
    out
}

/// Current Google Fonts WOFF2 (here what Chrome 142 gets) has the overlap bitmap in its glyf
/// table, which the previous decoder rejected for every font.
#[test]
fn google_woff2_with_overlap_bitmap() {
    let woff2 = include_bytes!("testdata/dm-sans-latin.woff2");
    let tf = Fonts::new().typeface(woff2).expect("decodes");
    assert!(tf.family_name().starts_with("DM Sans"), "{}", tf.family_name());
    assert_ne!(tf.unichar_to_glyph('A' as i32), 0);
}

#[test]
fn woff_round_trip() {
    let sfnt = wuff::decompress_woff2(INTER_LATIN).unwrap();
    let woff = encode(&sfnt);
    assert!(woff.starts_with(b"wOFF") && woff.len() < sfnt.len());
    // Same tables, checksums and contents (the table layout may differ).
    assert_eq!(encode(&wuff::decompress_woff1(&woff).unwrap()), woff);
    let tf = Fonts::new().typeface(&woff).expect("skia reads the decoded WOFF");
    assert!(tf.family_name().starts_with("Inter"));
    assert!(Fonts::new().typeface(&woff[..woff.len() / 2]).is_none());
}

#[test]
fn built_in_ui_fonts_load() {
    let fonts = Fonts::new();
    for (b, ch) in [(INTER_LATIN, '%'), (INTER_LATIN_EXT, 'Ł')] {
        let tf = fonts.typeface(b).expect("woff2 decodes");
        assert_ne!(tf.unichar_to_glyph(ch as i32), 0, "{ch}");
    }
}
