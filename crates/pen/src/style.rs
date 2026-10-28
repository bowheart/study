//! Typed, fully-resolved node styles (no variables left), parsed from JSON props.

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rgba(pub [f32; 4]);

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba([0.0; 4]);
    pub const BLACK: Rgba = Rgba([0.0, 0.0, 0.0, 1.0]);

    pub fn parse(s: &str) -> Option<Rgba> {
        let s = s.trim();
        let hex = s.strip_prefix('#')?;
        let digits: Vec<u8> = hex.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
        let (r, g, b, a) = match digits.len() {
            3 => (digits[0] * 17, digits[1] * 17, digits[2] * 17, 255),
            4 => (digits[0] * 17, digits[1] * 17, digits[2] * 17, digits[3] * 17),
            6 => (digits[0] << 4 | digits[1], digits[2] << 4 | digits[3], digits[4] << 4 | digits[5], 255),
            8 => (digits[0] << 4 | digits[1], digits[2] << 4 | digits[3], digits[4] << 4 | digits[5], digits[6] << 4 | digits[7]),
            _ => return None,
        };
        Some(Rgba([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0]))
    }

    pub fn with_alpha(self, a: f32) -> Rgba {
        let mut c = self;
        c.0[3] *= a;
        c
    }

    pub fn to_u32(self) -> u32 {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        c(self.0[3]) << 24 | c(self.0[0]) << 16 | c(self.0[1]) << 8 | c(self.0[2])
    }

    pub fn hex(self) -> String {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        if self.0[3] >= 1.0 {
            format!("#{:02X}{:02X}{:02X}", c(self.0[0]), c(self.0[1]), c(self.0[2]))
        } else {
            format!("#{:02X}{:02X}{:02X}{:02X}", c(self.0[0]), c(self.0[1]), c(self.0[2]), c(self.0[3]))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sizing {
    Fixed(f32),
    Fit(Option<f32>),
    Fill(Option<f32>),
}

impl Sizing {
    pub fn is_fill(self) -> bool {
        matches!(self, Sizing::Fill(_))
    }
    pub fn fallback(self) -> f32 {
        match self {
            Sizing::Fixed(v) => v,
            Sizing::Fit(f) | Sizing::Fill(f) => f.unwrap_or(0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Dir {
    None,
    #[default]
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlendMode {
    #[default]
    Normal,
    Darken,
    Multiply,
    LinearBurn,
    ColorBurn,
    Light,
    Screen,
    LinearDodge,
    ColorDodge,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub fn parse(s: Option<&str>) -> BlendMode {
        use BlendMode::*;
        match s.unwrap_or("normal") {
            "darken" => Darken,
            "multiply" => Multiply,
            "linearBurn" => LinearBurn,
            "colorBurn" => ColorBurn,
            "light" | "lighten" => Light,
            "screen" => Screen,
            "linearDodge" => LinearDodge,
            "colorDodge" => ColorDodge,
            "overlay" => Overlay,
            "softLight" => SoftLight,
            "hardLight" => HardLight,
            "difference" => Difference,
            "exclusion" => Exclusion,
            "hue" => Hue,
            "saturation" => Saturation,
            "color" => Color,
            "luminosity" => Luminosity,
            _ => Normal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GradientKind {
    Linear,
    Radial,
    Angular,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageMode {
    Stretch,
    Fill,
    Fit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Uniform {
    Float(f32),
    Bool(bool),
    Color(Rgba),
    Vec(Vec<f32>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Solid {
        color: Rgba,
        blend: BlendMode,
    },
    Gradient {
        kind: GradientKind,
        stops: Vec<(f32, Rgba)>,
        center: [f32; 2],
        size: [f32; 2],
        rotation: f32,
        opacity: f32,
        blend: BlendMode,
    },
    Image {
        url: String,
        mode: ImageMode,
        opacity: f32,
        blend: BlendMode,
    },
    Shader {
        url: String,
        uniforms: Vec<(String, Uniform)>,
        opacity: f32,
        blend: BlendMode,
    },
    Mesh {
        columns: u32,
        rows: u32,
        colors: Vec<Rgba>,
        points: Vec<[f32; 2]>,
        opacity: f32,
        blend: BlendMode,
    },
}

impl Paint {
    /// A single representative color, used for level-of-detail drawing and icon tinting.
    pub fn average(&self) -> Rgba {
        match self {
            Paint::Solid { color, .. } => *color,
            Paint::Gradient { stops, opacity, .. } => avg(stops.iter().map(|s| s.1)).with_alpha(*opacity),
            Paint::Mesh { colors, opacity, .. } => avg(colors.iter().copied()).with_alpha(*opacity),
            Paint::Image { opacity, .. } => Rgba([0.6, 0.6, 0.6, *opacity]),
            Paint::Shader { opacity, .. } => Rgba([0.5, 0.5, 0.5, *opacity]),
        }
    }
}

fn avg(it: impl Iterator<Item = Rgba>) -> Rgba {
    let mut sum = [0.0f32; 4];
    let mut n = 0.0;
    for c in it {
        for i in 0..4 {
            sum[i] += c.0[i];
        }
        n += 1.0;
    }
    if n == 0.0 {
        return Rgba::TRANSPARENT;
    }
    Rgba(sum.map(|v| v / n))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StrokeAlign {
    #[default]
    Inner,
    Center,
    Outer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Cap {
    #[default]
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Join {
    #[default]
    Miter,
    Bevel,
    Round,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub paints: Vec<Paint>,
    /// top, right, bottom, left
    pub width: [f32; 4],
    pub align: StrokeAlign,
    pub cap: Cap,
    pub join: Join,
}

impl Stroke {
    pub fn is_uniform(&self) -> bool {
        self.width.iter().all(|&w| w == self.width[0])
    }
    pub fn max_width(&self) -> f32 {
        self.width.iter().cloned().fold(0.0, f32::max)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    Blur { radius: f32 },
    BackgroundBlur { radius: f32 },
    Shadow { inner: bool, offset: [f32; 2], spread: f32, blur: f32, color: Rgba, blend: BlendMode },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VAlign {
    #[default]
    Top,
    Middle,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextGrowth {
    #[default]
    Auto,
    FixedWidth,
    FixedWidthHeight,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    pub family: String,
    pub size: f32,
    pub weight: u16,
    pub italic: bool,
    pub letter_spacing: f32,
    pub line_height: Option<f32>,
    pub align: TextAlign,
    pub valign: VAlign,
    pub underline: bool,
    pub strikethrough: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle {
            family: crate::DEFAULT_FONT.into(),
            size: 14.0,
            weight: 400,
            italic: false,
            letter_spacing: 0.0,
            line_height: None,
            align: TextAlign::Left,
            valign: VAlign::Top,
            underline: false,
            strikethrough: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextData {
    pub content: String,
    pub style: TextStyle,
    pub growth: TextGrowth,
}

pub fn parse_weight(v: &str) -> u16 {
    match v.trim().to_ascii_lowercase().as_str() {
        "thin" | "hairline" => 100,
        "extralight" | "extra-light" | "ultralight" => 200,
        "light" => 300,
        "normal" | "regular" | "" => 400,
        "medium" => 500,
        "semibold" | "semi-bold" | "demibold" => 600,
        "bold" => 700,
        "extrabold" | "extra-bold" | "ultrabold" => 800,
        "black" | "heavy" => 900,
        other => other.parse::<f32>().map(|w| w.round().clamp(1.0, 1000.0) as u16).unwrap_or(400),
    }
}

/// Parses `fit_content`, `fill_container(120)`, numbers, etc. Variables must already be resolved.
pub fn parse_sizing(v: Option<&Value>, default: Sizing) -> Sizing {
    match v {
        Some(Value::Number(n)) => Sizing::Fixed(n.as_f64().unwrap_or(0.0) as f32),
        Some(Value::String(s)) => {
            let s = s.trim();
            let (word, arg) = match s.find('(') {
                Some(i) => (&s[..i], s[i + 1..].trim_end_matches(')').trim().parse::<f32>().ok()),
                None => (s, None),
            };
            match word {
                "fit_content" => Sizing::Fit(arg),
                "fill_container" => Sizing::Fill(arg),
                _ => s.parse::<f32>().map(Sizing::Fixed).unwrap_or(default),
            }
        }
        _ => default,
    }
}
