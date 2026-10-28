pub mod draw;
pub mod fonts;
pub mod icons;
pub mod images;
pub mod offscreen;
pub mod shaders;
pub mod text;

pub use draw::{Renderer, View};
pub use icons::Icons;

/// Where study keeps downloaded fonts, icons, and settings: `~/.study`.
pub fn data_dir() -> std::path::PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_else(|| ".".into());
    std::path::PathBuf::from(home).join(".study")
}
