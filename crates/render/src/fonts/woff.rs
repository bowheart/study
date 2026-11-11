//! WOFF (version 1) to TrueType/OpenType. Google Fonts serves WOFF instead of WOFF2 to some
//! browsers (Safari and Firefox user agents among them), and FreeType reads neither. WOFF is the
//! font's tables, each zlib-compressed unless that didn't make it smaller, behind a header.
//! <https://www.w3.org/TR/WOFF/>

use std::io::Read;

pub fn is_woff(bytes: &[u8]) -> bool {
    bytes.starts_with(b"wOFF")
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The SFNT font inside a WOFF file, or None if it's malformed.
pub fn to_sfnt(woff: &[u8]) -> Option<Vec<u8>> {
    if !is_woff(woff) {
        return None;
    }
    let flavor = u32_at(woff, 4)?;
    let count = u16_at(woff, 12)? as usize;
    if count == 0 {
        return None;
    }
    // Tag, checksum and contents of each table, in the directory's (tag) order.
    let mut tables = Vec::with_capacity(count);
    for i in 0..count {
        let e = 44 + i * 20;
        let (tag, offset, comp, orig, sum) = (u32_at(woff, e)?, u32_at(woff, e + 4)? as usize, u32_at(woff, e + 8)? as usize, u32_at(woff, e + 12)? as usize, u32_at(woff, e + 16)?);
        let stored = woff.get(offset..offset.checked_add(comp)?)?;
        let data = if comp < orig {
            let mut out = Vec::with_capacity(orig);
            flate2::read::ZlibDecoder::new(stored).take(orig as u64).read_to_end(&mut out).ok()?;
            out
        } else {
            stored.to_vec()
        };
        if data.len() != orig {
            return None;
        }
        tables.push((tag, sum, data));
    }
    let pow2 = 1usize << (usize::BITS - 1 - count.leading_zeros());
    let mut out = Vec::new();
    out.extend(flavor.to_be_bytes());
    out.extend((count as u16).to_be_bytes());
    out.extend(((pow2 * 16) as u16).to_be_bytes());
    out.extend((pow2.trailing_zeros() as u16).to_be_bytes());
    out.extend(((count * 16 - pow2 * 16) as u16).to_be_bytes());
    let mut at = 12 + 16 * count;
    for (tag, sum, data) in &tables {
        out.extend(tag.to_be_bytes());
        out.extend(sum.to_be_bytes());
        out.extend((at as u32).to_be_bytes());
        out.extend((data.len() as u32).to_be_bytes());
        at += data.len().next_multiple_of(4);
    }
    for (_, _, data) in &tables {
        out.extend(data);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

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

    #[test]
    fn woff_round_trip() {
        let woff2 = include_bytes!("../../../web/fonts/inter-latin.woff2");
        let sfnt = woff2_patched::decode::convert_woff2_to_ttf(&mut &woff2[..]).unwrap();
        let woff = encode(&sfnt);
        assert!(super::is_woff(&woff) && woff.len() < sfnt.len());
        let back = super::to_sfnt(&woff).unwrap();
        // Same tables, checksums and contents (the table layout may differ).
        assert_eq!(encode(&back), woff);
        let fonts = crate::fonts::Fonts::new();
        let tf = fonts.typeface(&woff).expect("skia reads the decoded WOFF");
        assert!(tf.family_name().starts_with("Inter"));
        assert!(super::to_sfnt(&woff[..woff.len() / 2]).is_none());
    }

    #[test]
    fn built_in_ui_fonts_load() {
        let fonts = crate::fonts::Fonts::new();
        for (b, ch) in [(&include_bytes!("../../../web/fonts/inter-latin.woff2")[..], '%'), (&include_bytes!("../../../web/fonts/inter-latin-ext.woff2")[..], 'Ł')] {
            let tf = fonts.typeface(b).expect("woff2 decodes");
            assert_ne!(tf.unichar_to_glyph(ch as i32), 0, "{ch}");
        }
    }
}
