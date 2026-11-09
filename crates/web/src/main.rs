//! study's canvas in the browser: the same `pen` layout and `render` Skia renderer as the desktop
//! app, compiled to WebAssembly with Emscripten and drawn through WebGL2. `js/study-viewer.js`
//! wraps the `study_*` exports in `viewer.rs`. Everything runs on the page's main thread; network
//! requests go through the page (`render::net`).

#[cfg(target_os = "emscripten")]
mod viewer;

fn main() {
    #[cfg(target_os = "emscripten")]
    viewer::init_gl();
}
