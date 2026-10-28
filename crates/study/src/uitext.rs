//! Text for the window's own labels (frame titles, badges, zoom) with per-character font
//! fallback, so symbols like ⌘ and → render even when the system UI font lacks them.

use skia_safe::{Canvas, Font, FontMgr, FontStyle, Paint, Typeface};
use std::cell::RefCell;
use std::collections::HashMap;

pub struct UiText {
    font: Font,
    mgr: FontMgr,
    /// Fallback fonts by character (None = nothing has it; keep the primary's missing glyph).
    fallback: RefCell<HashMap<char, Option<Font>>>,
}

impl UiText {
    pub fn new(size: f32) -> Option<UiText> {
        UiText::new_styled(size, FontStyle::normal())
    }

    pub fn new_styled(size: f32, style: FontStyle) -> Option<UiText> {
        let mgr = FontMgr::new();
        let typeface = mgr.legacy_make_typeface(None, style)?;
        Some(UiText { font: Font::new(typeface, size), mgr, fallback: RefCell::new(HashMap::new()) })
    }

    /// Splits `s` into runs that share a font.
    fn runs<'s>(&self, s: &'s str) -> Vec<(&'s str, Font)> {
        let mut out: Vec<(&str, Font)> = Vec::new();
        let mut start = 0;
        let mut current: Option<Font> = None;
        for (i, ch) in s.char_indices() {
            let f = self.font_for(ch);
            let same = current.as_ref().is_some_and(|c| c.typeface().unique_id() == f.typeface().unique_id());
            if !same {
                if let Some(c) = current.take() {
                    out.push((&s[start..i], c));
                }
                start = i;
                current = Some(f);
            }
        }
        if let Some(c) = current {
            out.push((&s[start..], c));
        }
        out
    }

    fn font_for(&self, ch: char) -> Font {
        if ch.is_whitespace() || self.font.unichar_to_glyph(ch as i32) != 0 {
            return self.font.clone();
        }
        let mut cache = self.fallback.borrow_mut();
        let found = cache.entry(ch).or_insert_with(|| {
            let family = self.font.typeface().family_name();
            let tf: Option<Typeface> = self.mgr.match_family_style_character(family, self.font.typeface().font_style(), &[], ch as i32);
            tf.map(|t| Font::new(t, self.font.size()))
        });
        found.clone().unwrap_or_else(|| self.font.clone())
    }

    pub fn measure(&self, s: &str) -> f32 {
        self.runs(s).iter().map(|(t, f)| f.measure_str(t, None).0).sum()
    }

    pub fn draw(&self, canvas: &Canvas, s: &str, (x, y): (f32, f32), paint: &Paint) {
        let mut x = x;
        for (t, f) in self.runs(s) {
            canvas.draw_str(t, (x, y), &f, paint);
            x += f.measure_str(t, None).0;
        }
    }
}
