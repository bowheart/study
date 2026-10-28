//! Text shaping and measurement with SkParagraph (the same shaper Pen uses).

use crate::fonts::Fonts;
use pen::layout::TextMeasure;
use pen::style::{TextAlign, TextData};
use skia_safe::font_style::{Slant, Weight, Width};
use skia_safe::textlayout::{
    Decoration, ParagraphBuilder, ParagraphStyle, TextAlign as SkAlign, TextDecoration, TextDecorationMode, TextShadow,
    TextStyle as SkTextStyle,
};
use skia_safe::textlayout::Paragraph;
use skia_safe::{Color, FontStyle, Paint};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

pub struct TextEngine {
    pub fonts: Fonts,
    measured: HashMap<u64, [f32; 2]>,
    paragraphs: HashMap<u64, (Paragraph, u64)>,
    frame: u64,
}

/// Wrapping width used for "no wrap" so alignment math stays finite.
const NO_WRAP: f32 = 1.0e7;

impl TextEngine {
    pub fn new() -> TextEngine {
        TextEngine { fonts: Fonts::new(), measured: HashMap::new(), paragraphs: HashMap::new(), frame: 0 }
    }

    /// Forget cached shaping results (after new fonts arrive).
    pub fn invalidate(&mut self) {
        self.measured.clear();
        self.paragraphs.clear();
    }

    /// Drops paragraphs that haven't been drawn for a while.
    pub fn end_frame(&mut self) {
        self.frame += 1;
        if self.paragraphs.len() > 8000 {
            let cutoff = self.frame.saturating_sub(30);
            self.paragraphs.retain(|_, (_, used)| *used >= cutoff);
        }
    }

    fn build(&mut self, t: &TextData, paint: Option<&Paint>, shadows: &[TextShadow]) -> Paragraph {
        let s = &t.style;
        self.fonts.request(&s.family, s.weight, s.italic);
        let mut ts = SkTextStyle::new();
        ts.set_font_families(&[s.family.as_str()]);
        ts.set_font_size(s.size);
        ts.set_font_style(FontStyle::new(
            Weight::from(s.weight as i32),
            Width::NORMAL,
            if s.italic { Slant::Italic } else { Slant::Upright },
        ));
        ts.set_letter_spacing(s.letter_spacing);
        if let Some(lh) = s.line_height {
            ts.set_height(lh);
            ts.set_height_override(true);
            ts.set_half_leading(true);
        }
        if s.underline || s.strikethrough {
            let mut d = Decoration::default();
            let mut ty = TextDecoration::NO_DECORATION;
            if s.underline {
                ty |= TextDecoration::UNDERLINE;
            }
            if s.strikethrough {
                ty |= TextDecoration::LINE_THROUGH;
            }
            d.ty = ty;
            d.mode = TextDecorationMode::Through;
            d.color = paint.map(|p| p.color()).unwrap_or(Color::BLACK);
            ts.set_decoration(&d);
        }
        match paint {
            Some(p) => {
                ts.set_foreground_paint(p);
            }
            None => {
                ts.set_color(Color::BLACK);
            }
        }
        for sh in shadows {
            ts.add_shadow(*sh);
        }
        let mut ps = ParagraphStyle::new();
        ps.set_text_style(&ts);
        ps.set_text_align(match s.align {
            TextAlign::Left => SkAlign::Left,
            TextAlign::Center => SkAlign::Center,
            TextAlign::Right => SkAlign::Right,
            TextAlign::Justify => SkAlign::Justify,
        });
        ps.set_apply_rounding_hack(false);
        let mut b = ParagraphBuilder::new(&ps, self.fonts.collection.clone());
        b.push_style(&ts);
        // Pen treats a lone trailing newline as an extra (empty) line, like SkParagraph.
        b.add_text(&t.content);
        b.build()
    }

    /// A laid-out paragraph for drawing. Cached across frames for solid fills.
    pub fn paragraph(&mut self, t: &TextData, width: Option<f32>, paint: &Paint, cacheable: bool, shadows: &[TextShadow]) -> &mut Paragraph {
        let key = key(t, width, Some({ let c = paint.color(); (c.a() as u32) << 24 | (c.r() as u32) << 16 | (c.g() as u32) << 8 | c.b() as u32 } ^ paint.shader().is_some() as u32), shadows.len());
        let frame = self.frame;
        if !cacheable || shadows.len() > 0 || !self.paragraphs.contains_key(&key) {
            let mut p = self.build(t, Some(paint), shadows);
            p.layout(width.unwrap_or(NO_WRAP));
            if width.is_none() {
                // Re-layout at the intrinsic width so alignment has a sensible box.
                let w = p.max_intrinsic_width();
                p.layout(w + 0.5);
            }
            self.paragraphs.insert(key, (p, frame));
        }
        let entry = self.paragraphs.get_mut(&key).unwrap();
        entry.1 = frame;
        &mut entry.0
    }
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TextMeasure for TextEngine {
    fn measure(&mut self, t: &TextData, width: Option<f32>) -> [f32; 2] {
        let k = key(t, width, None, 0);
        if let Some(m) = self.measured.get(&k) {
            return *m;
        }
        let mut p = self.build(t, None, &[]);
        let m = match width {
            Some(w) => {
                p.layout(w.max(0.0));
                [w, p.height()]
            }
            None => {
                p.layout(NO_WRAP);
                [p.max_intrinsic_width(), p.height()]
            }
        };
        self.measured.insert(k, m);
        m
    }
}

fn key(t: &TextData, width: Option<f32>, color: Option<u32>, extra: usize) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.content.hash(&mut h);
    t.style.family.hash(&mut h);
    t.style.size.to_bits().hash(&mut h);
    t.style.weight.hash(&mut h);
    t.style.italic.hash(&mut h);
    t.style.letter_spacing.to_bits().hash(&mut h);
    t.style.line_height.map(f32::to_bits).hash(&mut h);
    (t.style.align as u8).hash(&mut h);
    t.style.underline.hash(&mut h);
    t.style.strikethrough.hash(&mut h);
    width.map(f32::to_bits).hash(&mut h);
    color.hash(&mut h);
    extra.hash(&mut h);
    h.finish()
}
