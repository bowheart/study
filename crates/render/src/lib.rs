pub mod draw;
pub mod fonts;
pub mod icons;
pub mod images;
pub mod net;
pub mod controls;
pub mod offscreen;
pub mod overlay;
pub mod shaders;
pub mod text;

pub use draw::{Renderer, View};
pub use icons::Icons;

/// Where study keeps downloaded fonts, icons, and settings: `$STUDY_DATA_DIR`, or `~/.study`.
pub fn data_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("STUDY_DATA_DIR").filter(|d| !d.is_empty()) {
        return dir.into();
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_else(|| ".".into());
    std::path::PathBuf::from(home).join(".study")
}
