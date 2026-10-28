//! The study window: an OpenGL-backed Skia canvas with pan/zoom, selection, a native
//! menu bar, and live updates from MCP requests.

use crate::engine::Engine;
use crate::ipc;
use crate::tools::{self, ViewInfo};
use crate::updater;
use anyhow::{anyhow, Result};
use glutin::config::{ConfigTemplateBuilder, GlConfig};
use glutin::context::{ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface as GlWinSurface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use glutin_winit::DisplayBuilder;
use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use pen::scene::Rect;
use raw_window_handle::HasWindowHandle;
#[cfg(windows)]
use raw_window_handle::RawWindowHandle;
use render::View;
use serde_json::{json, Value};
use skia_safe::gpu::gl::FramebufferInfo;
use skia_safe::{Color, Color4f, Contains, FontStyle, Matrix, Paint, PaintStyle, Path as SkPath};
use std::collections::HashMap;
use std::ffi::CString;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
#[cfg(windows)]
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Icon, Theme, Window, WindowAttributes, WindowId};

pub enum UserEvent {
    Request(ipc::Request),
    Menu(String),
    /// A newer release has been downloaded and can be installed.
    UpdateReady(updater::Update),
    /// The result of a manual "Check for Updates", when there's nothing to install.
    UpdateStatus(String),
}

/// Fields drop in declaration order: Skia objects, then the GL surface and context, and the
/// window last. Destroying the window first leaves the GL driver tearing down a context whose
/// window is gone, which hangs on exit.
struct Gl {
    surface: skia_safe::Surface,
    gr: skia_safe::gpu::DirectContext,
    gl_surface: GlWinSurface<WindowSurface>,
    gl_context: PossiblyCurrentContext,
    window: Window,
    fb_info: FramebufferInfo,
    samples: usize,
    stencil: usize,
}

#[derive(Clone, Copy)]
struct Camera {
    /// Document coordinates at the window's top-left.
    x: f32,
    y: f32,
    /// Logical pixels per document unit.
    zoom: f32,
}

#[derive(Clone, Copy, PartialEq)]
enum ThemePref {
    Light,
    Dark,
    System,
}

impl ThemePref {
    fn name(self) -> &'static str {
        match self {
            ThemePref::Light => "light",
            ThemePref::Dark => "dark",
            ThemePref::System => "system",
        }
    }
}

struct Menus {
    bar: Menu,
    recent: Submenu,
    themes: [(ThemePref, CheckMenuItem); 3],
}

/// A path node being drawn in by the pen.
struct Anim {
    key: String,
    start: Instant,
    dur: f32,
}

/// A node being scanned because the agent read it.
struct Scan {
    key: String,
    start: Instant,
}

/// Sweep, then linger and fade (see `draw_scan`).
const SCAN_SECS: f32 = 2.2;
/// How long a frame keeps glowing after the agent last touched it, and its fade-out.
const WORK_HOLD: f32 = 20.0;
const WORK_FADE: f32 = 2.5;

struct App {
    engine: Option<Engine>,
    gl: Option<Gl>,
    menus: Option<Menus>,
    cam: Camera,
    pending_fit: bool,
    cursor: (f32, f32),
    pan_from: Option<(f32, f32)>,
    space_down: bool,
    mods: ModifiersState,
    hover: Option<String>,
    theme: ThemePref,
    dark: bool,
    recent: Vec<PathBuf>,
    anims: Vec<Anim>,
    scans: Vec<Scan>,
    /// Top-level frames the agent is working in: first and latest activity.
    working: HashMap<String, (Instant, Instant)>,
    proxy: EventLoopProxy<UserEvent>,
    /// A downloaded update waiting to be installed, and whether its banner was dismissed.
    update: Option<updater::Update>,
    banner_hidden: bool,
    /// Banner button under the pointer: 0 = install now, 1 = next launch.
    banner_hover: Option<usize>,
    ui_title: crate::uitext::UiText,
    ui_body: crate::uitext::UiText,
    port: u16,
    token: String,
    last_poll: Instant,
    ui: crate::uitext::UiText,
    initial: Option<PathBuf>,
    /// App start, the clock for pulsing animations.
    started: Instant,
}

const MAX_RECENT: usize = 12;

pub fn run(file: Option<PathBuf>) -> Result<()> {
    if updater::enabled() {
        updater::cleanup();
        // "Install on next launch": swap in the new version, then start it instead.
        if let Some(exe) = updater::install_pending() {
            if updater::relaunch(&exe, file.as_deref()).is_ok() {
                return Ok(());
            }
        }
    }
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy: EventLoopProxy<UserEvent> = event_loop.create_proxy();
    if updater::enabled() {
        let p = proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(10));
            loop {
                if let Ok(Some(u)) = updater::check() {
                    let _ = p.send_event(UserEvent::UpdateReady(u));
                }
                std::thread::sleep(updater::INTERVAL);
            }
        });
    }
    let app_proxy = proxy.clone();
    let menu_proxy = Mutex::new(proxy.clone());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = menu_proxy.lock().unwrap().send_event(UserEvent::Menu(e.id.0));
    }));
    let (port, token) = ipc::serve(move |req| {
        let _ = proxy.send_event(UserEvent::Request(req));
    })?;
    let state = load_state();
    let file = file.or_else(|| state.get("last").and_then(Value::as_str).map(PathBuf::from).filter(|p| p.exists()));
    let theme = match state.get("theme").and_then(Value::as_str) {
        Some("dark") => ThemePref::Dark,
        Some("light") => ThemePref::Light,
        Some(_) => ThemePref::System,
        None if state.get("dark").and_then(Value::as_bool) == Some(true) => ThemePref::Dark,
        None => ThemePref::System,
    };
    let recent = state
        .get("recent")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(PathBuf::from).collect())
        .unwrap_or_default();
    let mut app = App {
        engine: None,
        gl: None,
        menus: None,
        cam: Camera { x: 0.0, y: 0.0, zoom: 1.0 },
        pending_fit: false,
        cursor: (0.0, 0.0),
        pan_from: None,
        space_down: false,
        mods: ModifiersState::empty(),
        hover: None,
        theme,
        dark: theme == ThemePref::Dark,
        recent,
        anims: Vec::new(),
        scans: Vec::new(),
        working: HashMap::new(),
        proxy: app_proxy,
        update: None,
        banner_hidden: false,
        banner_hover: None,
        ui_title: crate::uitext::UiText::new_styled(15.0, FontStyle::bold()).ok_or_else(|| anyhow!("no system font"))?,
        ui_body: crate::uitext::UiText::new(13.0).ok_or_else(|| anyhow!("no system font"))?,
        port,
        token,
        last_poll: Instant::now(),
        ui: crate::uitext::UiText::new(11.0).ok_or_else(|| anyhow!("no system font"))?,
        initial: file,
        started: Instant::now(),
    };
    ipc::register(port, &app.token, None, true);
    event_loop.run_app(&mut app)?;
    ipc::unregister();
    Ok(())
}

impl App {
    fn scale_factor(&self) -> f32 {
        self.gl.as_ref().map(|g| g.window.scale_factor() as f32).unwrap_or(1.0)
    }

    fn size(&self) -> (f32, f32) {
        let Some(g) = &self.gl else { return (1.0, 1.0) };
        let s = g.window.inner_size();
        let sf = g.window.scale_factor() as f32;
        (s.width as f32 / sf, s.height as f32 / sf)
    }

    fn viewport(&self) -> Rect {
        let (w, h) = self.size();
        Rect::new(self.cam.x, self.cam.y, w / self.cam.zoom, h / self.cam.zoom)
    }

    fn to_doc(&self, sx: f32, sy: f32) -> (f32, f32) {
        (self.cam.x + sx / self.cam.zoom, self.cam.y + sy / self.cam.zoom)
    }

    fn redraw(&self) {
        if let Some(g) = &self.gl {
            g.window.request_redraw();
        }
    }

    #[cfg(windows)]
    fn hwnd(&self) -> Option<isize> {
        let g = self.gl.as_ref()?;
        match g.window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
            _ => None,
        }
    }

    fn open(&mut self, path: &Path) {
        self.save_camera();
        match Engine::open(path) {
            Ok(e) => {
                let key = ipc::norm(e.path().unwrap_or(path));
                let saved = load_state().get("cameras").and_then(|c| c.get(&key)).and_then(Value::as_array).cloned();
                self.engine = Some(e);
                self.anims.clear();
                match saved {
                    Some(c) if c.len() == 3 => {
                        self.cam = Camera {
                            x: c[0].as_f64().unwrap_or(0.0) as f32,
                            y: c[1].as_f64().unwrap_or(0.0) as f32,
                            zoom: c[2].as_f64().unwrap_or(1.0) as f32,
                        };
                        self.pending_fit = false;
                    }
                    _ => self.pending_fit = true,
                }
                self.document_moved();
            }
            Err(e) => {
                rfd::MessageDialog::new().set_title("study").set_description(format!("Could not open {}:\n{e:#}", path.display())).show();
            }
        }
    }

    /// After opening or renaming: remember the file, re-register for MCP routing, retitle.
    fn document_moved(&mut self) {
        let p = self.engine.as_ref().and_then(|e| e.path().map(Path::to_path_buf));
        update_state(|s| s["last"] = json!(p.as_ref().map(|p| p.to_string_lossy().to_string())));
        if let Some(p) = &p {
            self.add_recent(p);
        }
        ipc::register(self.port, &self.token, p.as_deref(), true);
        self.update_title();
        self.redraw();
    }

    fn add_recent(&mut self, p: &Path) {
        let key = ipc::norm(p);
        self.recent.retain(|r| ipc::norm(r) != key);
        self.recent.insert(0, p.to_path_buf());
        self.recent.truncate(MAX_RECENT);
        self.save_recent();
    }

    fn save_recent(&mut self) {
        let list: Vec<String> = self.recent.iter().map(|p| p.to_string_lossy().to_string()).collect();
        update_state(|s| s["recent"] = json!(list));
        self.rebuild_recent_menu();
    }

    fn update_title(&self) {
        let Some(g) = &self.gl else { return };
        let name = self
            .engine
            .as_ref()
            .and_then(|e| e.path())
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "no document".into());
        g.window.set_title(&format!("{name} — study"));
    }

    fn save_camera(&self) {
        let Some(path) = self.engine.as_ref().and_then(|e| e.path()) else { return };
        let key = ipc::norm(path);
        let cam = self.cam;
        update_state(|s| {
            if !s["cameras"].is_object() {
                s["cameras"] = json!({});
            }
            s["cameras"][key] = json!([cam.x, cam.y, cam.zoom]);
        });
    }

    fn fit(&mut self, r: Option<Rect>) {
        let Some(r) = r.or_else(|| self.engine.as_ref().and_then(|e| e.doc_bounds())) else { return };
        let (w, h) = self.size();
        let margin = 48.0;
        let zoom = ((w - 2.0 * margin) / r.w.max(1.0)).min((h - 2.0 * margin) / r.h.max(1.0)).clamp(0.002, 1.0);
        self.cam = Camera { x: r.x + r.w / 2.0 - w / zoom / 2.0, y: r.y + r.h / 2.0 - h / zoom / 2.0, zoom };
        self.redraw();
    }

    fn zoom_at(&mut self, factor: f32, sx: f32, sy: f32) {
        let (dx, dy) = self.to_doc(sx, sy);
        let zoom = (self.cam.zoom * factor).clamp(0.001, 256.0);
        self.cam.zoom = zoom;
        self.cam.x = dx - sx / zoom;
        self.cam.y = dy - sy / zoom;
        self.redraw();
    }

    // ------------------------------------------------------------------ theme

    fn set_theme(&mut self, pref: ThemePref) {
        self.theme = pref;
        update_state(|s| s["theme"] = json!(pref.name()));
        self.apply_theme();
    }

    /// Syncs the canvas, title bar and menu bar with the theme preference.
    fn apply_theme(&mut self) {
        if let Some(g) = &self.gl {
            g.window.set_theme(match self.theme {
                ThemePref::Light => Some(Theme::Light),
                ThemePref::Dark => Some(Theme::Dark),
                ThemePref::System => None,
            });
            self.dark = match self.theme {
                ThemePref::Light => false,
                ThemePref::Dark => true,
                ThemePref::System => g.window.theme() == Some(Theme::Dark),
            };
        }
        if let Some(m) = &self.menus {
            for (p, item) in &m.themes {
                item.set_checked(*p == self.theme);
            }
            #[cfg(windows)]
            if let Some(hwnd) = self.hwnd() {
                let t = if self.dark { muda::MenuTheme::Dark } else { muda::MenuTheme::Light };
                // Safety: hwnd is this window's live handle, on the UI thread.
                let _ = unsafe { m.bar.set_theme_for_hwnd(hwnd, t) };
            }
        }
        self.redraw();
    }

    /// The "update ready" banner and its two buttons (install now, next launch), when showing.
    fn banner(&self) -> Option<(skia_safe::Rect, [skia_safe::Rect; 2])> {
        if self.update.is_none() || self.banner_hidden {
            return None;
        }
        let (ww, wh) = self.size();
        let w = 460f32.min(ww - 32.0);
        let panel = skia_safe::Rect::from_xywh((ww - w) / 2.0, wh - 128.0 - 24.0, w, 128.0);
        let bw = (w - 40.0 - 12.0) / 2.0;
        let install = skia_safe::Rect::from_xywh(panel.left + 20.0, panel.top + 72.0, bw, 36.0);
        let later = skia_safe::Rect::from_xywh(install.right + 12.0, install.top, bw, 36.0);
        Some((panel, [install, later]))
    }

    /// The light/dark switch in the bottom-right corner (logical px).
    fn theme_switch_rect(&self) -> skia_safe::Rect {
        let (ww, wh) = self.size();
        let zoom_w = self.ui.measure("00000%");
        skia_safe::Rect::from_xywh(ww - zoom_w - 18.0 - 52.0, wh - 30.0, 52.0, 22.0)
    }

    // ------------------------------------------------------------------ menus

    fn build_menus(&mut self) {
        use muda::accelerator::{Accelerator, Code, Modifiers, CMD_OR_CTRL};
        let item = |id: &str, text: &str, mods: Option<Modifiers>, key: Option<Code>| {
            let accel = key.map(|k| Accelerator::new(mods.unwrap_or(Modifiers::empty()), k));
            MenuItem::with_id(id, text, true, accel)
        };
        let cmd = Some(CMD_OR_CTRL);
        let cmd_shift = Some(CMD_OR_CTRL | Modifiers::SHIFT);
        let sep = PredefinedMenuItem::separator;
        let recent = Submenu::new("Open &Recent", true);
        let file = Submenu::new("&File", true);
        let _ = file.append_items(&[
            &item("new", "&New…", cmd, Some(Code::KeyN)),
            &item("open", "&Open…", cmd, Some(Code::KeyO)),
            &recent,
            &sep(),
            &item("save", "&Save", cmd, Some(Code::KeyS)),
            &item("save_as", "Save &As…", cmd_shift, Some(Code::KeyS)),
            &item("reload", "Re&load from Disk", None, Some(Code::F5)),
        ]);
        #[cfg(not(target_os = "macos"))]
        let _ = file.append_items(&[&sep(), &item("check_updates", "Check for &Updates…", None, None), &sep(), &item("exit", "E&xit", None, None)]);
        let edit = Submenu::new("&Edit", true);
        let _ = edit.append_items(&[
            &item("undo", "&Undo", cmd, Some(Code::KeyZ)),
            &item("redo", "&Redo", cmd_shift, Some(Code::KeyZ)),
            &sep(),
            &item("deselect", "Select &None", None, Some(Code::Escape)),
        ]);
        let themes = [
            (ThemePref::Light, CheckMenuItem::with_id("theme_light", "&Light", true, false, None)),
            (ThemePref::Dark, CheckMenuItem::with_id("theme_dark", "&Dark", true, false, None)),
            (ThemePref::System, CheckMenuItem::with_id("theme_system", "Use &System Setting", true, false, None)),
        ];
        let theme = Submenu::new("&Theme", true);
        let _ = theme.append_items(&[&themes[0].1, &themes[1].1, &themes[2].1]);
        let view = Submenu::new("&View", true);
        let _ = view.append_items(&[
            &item("fit", "Zoom to &Fit", cmd, Some(Code::Digit0)),
            &item("actual", "&Actual Size", cmd, Some(Code::Digit1)),
            &item("zoom_sel", "Zoom to &Selection", cmd, Some(Code::Digit2)),
            &item("zoom_in", "Zoom &In", cmd, Some(Code::Equal)),
            &item("zoom_out", "Zoom &Out", cmd, Some(Code::Minus)),
            &sep(),
            &theme,
        ]);
        let bar = Menu::new();
        #[cfg(target_os = "macos")]
        {
            // The first submenu is the application menu on macOS.
            let app = Submenu::new("study", true);
            let _ = app.append_items(&[
                &PredefinedMenuItem::about(None, None),
                &item("check_updates", "Check for Updates…", None, None),
                &sep(),
                &PredefinedMenuItem::hide(None),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &sep(),
                &item("exit", "Quit study", cmd, Some(Code::KeyQ)),
            ]);
            let _ = bar.append(&app);
        }
        let _ = bar.append_items(&[&file, &edit, &view]);
        #[cfg(windows)]
        {
            let Some(hwnd) = self.hwnd() else { return };
            let t = if self.dark { muda::MenuTheme::Dark } else { muda::MenuTheme::Light };
            // Safety: hwnd is this window's live handle, on the UI thread.
            let _ = unsafe { bar.init_for_hwnd_with_theme(hwnd, t) };
        }
        #[cfg(target_os = "macos")]
        bar.init_for_nsapp();
        self.menus = Some(Menus { bar, recent, themes });
        self.rebuild_recent_menu();
    }

    fn rebuild_recent_menu(&mut self) {
        let Some(m) = &self.menus else { return };
        while m.recent.remove_at(0).is_some() {}
        for (i, p) in self.recent.iter().enumerate() {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
            let label = format!("{name}   {dir}").replace('&', "&&");
            let _ = m.recent.append(&MenuItem::with_id(format!("recent:{i}"), label, true, None));
        }
        if self.recent.is_empty() {
            let _ = m.recent.append(&MenuItem::new("(none)", false, None));
        } else {
            let _ = m.recent.append(&PredefinedMenuItem::separator());
            let _ = m.recent.append(&MenuItem::with_id("recent_clear", "&Clear Recent", true, None));
        }
    }

    fn dialog(&self) -> rfd::FileDialog {
        let mut d = rfd::FileDialog::new().add_filter("Pen design", &["pen"]);
        if let Some(dir) = self.engine.as_ref().and_then(|e| e.path()).and_then(Path::parent) {
            d = d.set_directory(dir);
        }
        d
    }

    fn error(&self, what: &str, e: impl std::fmt::Display) {
        rfd::MessageDialog::new().set_title("study").set_description(format!("{what}:\n{e}")).show();
    }

    /// Menu commands and their keyboard shortcuts.
    fn command(&mut self, el: &ActiveEventLoop, id: &str) {
        let (w, h) = self.size();
        match id {
            "new" => {
                if let Some(p) = self.dialog().set_file_name("Untitled.pen").set_title("New design").save_file() {
                    // The save dialog already confirmed replacing an existing file.
                    let _ = std::fs::remove_file(&p);
                    self.open(&p);
                }
            }
            "open" => {
                if let Some(p) = self.dialog().pick_file() {
                    self.open(&p);
                }
            }
            "save" => {
                if let Some(e) = self.engine.as_mut() {
                    if let Err(err) = e.save() {
                        self.error("Could not save", err);
                    }
                }
                self.save_camera();
            }
            "save_as" => {
                let Some(name) = self.engine.as_ref().and_then(|e| e.path()).and_then(Path::file_name).map(|n| n.to_string_lossy().to_string()) else { return };
                if let Some(p) = self.dialog().set_file_name(name).set_title("Save design as").save_file() {
                    let result = self.engine.as_mut().map(|e| e.save_as(&p));
                    match result {
                        Some(Err(err)) => self.error("Could not save", err),
                        Some(Ok(())) => {
                            self.save_camera();
                            self.document_moved();
                        }
                        None => {}
                    }
                }
            }
            "reload" => {
                if let Some(p) = self.engine.as_ref().and_then(|e| e.path().map(Path::to_path_buf)) {
                    let cam = self.cam;
                    self.open(&p);
                    self.cam = cam;
                    self.pending_fit = false;
                }
            }
            "exit" => {
                self.save_camera();
                el.exit();
            }
            "undo" => {
                if let Some(e) = self.engine.as_mut() {
                    if e.undo() {
                        e.prune_selection();
                        self.anims.clear();
                        self.redraw();
                    }
                }
            }
            "redo" => {
                if self.engine.as_mut().is_some_and(|e| e.redo()) {
                    self.redraw();
                }
            }
            "deselect" => {
                if let Some(e) = self.engine.as_mut() {
                    e.selection.clear();
                }
                self.redraw();
            }
            "fit" => self.fit(None),
            "actual" => self.zoom_at(1.0 / self.cam.zoom, w / 2.0, h / 2.0),
            "zoom_sel" => {
                let r = self.engine.as_ref().and_then(|e| e.selection.iter().filter_map(|k| e.abs_rect(k)).reduce(|a, b| a.union(&b)));
                if r.is_some() {
                    self.fit(r);
                }
            }
            "zoom_in" => self.zoom_at(1.25, w / 2.0, h / 2.0),
            "zoom_out" => self.zoom_at(0.8, w / 2.0, h / 2.0),
            "theme_light" => self.set_theme(ThemePref::Light),
            "theme_dark" => self.set_theme(ThemePref::Dark),
            "theme_system" => self.set_theme(ThemePref::System),
            "recent_clear" => {
                self.recent.clear();
                self.save_recent();
            }
            "check_updates" => {
                if !updater::enabled() {
                    self.error("Updates", "This copy of study was built from source; update it with git pull and cargo install.");
                } else if self.update.is_some() {
                    self.banner_hidden = false;
                    self.redraw();
                } else {
                    let p = self.proxy.clone();
                    std::thread::spawn(move || {
                        let ev = match updater::check() {
                            Ok(Some(u)) => UserEvent::UpdateReady(u),
                            Ok(None) => UserEvent::UpdateStatus(format!("study {} is the latest version.", updater::current())),
                            Err(e) => UserEvent::UpdateStatus(format!("Could not check for updates:\n{e}")),
                        };
                        let _ = p.send_event(ev);
                    });
                }
            }
            "update_install" => {
                let Some(u) = self.update.clone() else { return };
                let file = self.engine.as_ref().and_then(|e| e.path().map(Path::to_path_buf));
                self.save_camera();
                match updater::install(&u).and_then(|exe| updater::relaunch(&exe, file.as_deref())) {
                    Ok(()) => el.exit(),
                    Err(e) => self.error("Could not install the update", e),
                }
            }
            "update_later" => {
                if let Some(u) = &self.update {
                    updater::set_pending(u);
                }
                self.banner_hidden = true;
                self.redraw();
            }
            other => {
                if let Some(p) = other.strip_prefix("recent:").and_then(|i| i.parse::<usize>().ok()).and_then(|i| self.recent.get(i).cloned()) {
                    if p.exists() {
                        self.open(&p);
                    } else {
                        self.error("This file no longer exists", p.display());
                        self.recent.retain(|r| *r != p);
                        self.save_recent();
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------ requests

    fn handle_request(&mut self, req: ipc::Request) {
        let reply = match req.tool.as_str() {
            "open" => {
                let p = req.args.get("filePath").and_then(Value::as_str).map(PathBuf::from);
                match p {
                    Some(p) => {
                        self.open(&p);
                        json!({"content": [{"type": "text", "text": "opened"}], "isError": self.engine.is_none()})
                    }
                    None => ipc::tool_error("missing filePath"),
                }
            }
            "get_app_state" => {
                let view = self.viewport();
                let info = ViewInfo { x: view.x, y: view.y, w: view.w, h: view.h, zoom: self.cam.zoom };
                let text = match self.engine.as_mut() {
                    Some(e) => tools::app_state(e, Some(info)),
                    None => "A study window is open with no document. Pass a .pen file path as `filePath` to execute.\n".into(),
                };
                json!({"content": [{"type": "text", "text": text}], "isError": false})
            }
            "execute" => match self.engine.as_mut() {
                Some(e) => {
                    e.reload_if_changed();
                    let r = tools::execute(e, &req.args);
                    e.prune_selection();
                    let fresh = std::mem::take(&mut e.fresh_paths);
                    let reads = std::mem::take(&mut e.fresh_reads);
                    let touched = std::mem::take(&mut e.touched_roots);
                    let now = Instant::now();
                    for k in touched {
                        let w = self.working.entry(k).or_insert((now, now));
                        if now.duration_since(w.1).as_secs_f32() > WORK_HOLD + WORK_FADE {
                            w.0 = now;
                        }
                        w.1 = now;
                    }
                    self.draw_in(fresh);
                    self.scan(reads);
                    r
                }
                None => ipc::tool_error("No document is open in this window."),
            },
            // Development aid: the window's current frame as PNG, optionally after moving the camera.
            "debug_frame" => {
                if let Some(c) = req.args.get("camera").and_then(Value::as_array) {
                    self.cam = Camera {
                        x: c.first().and_then(Value::as_f64).unwrap_or(0.0) as f32,
                        y: c.get(1).and_then(Value::as_f64).unwrap_or(0.0) as f32,
                        zoom: c.get(2).and_then(Value::as_f64).unwrap_or(1.0) as f32,
                    };
                }
                if req.args.get("fit").and_then(Value::as_bool) == Some(true) {
                    self.fit(None);
                }
                if let Some(t) = req.args.get("theme").and_then(Value::as_str) {
                    self.set_theme(match t {
                        "dark" => ThemePref::Dark,
                        "light" => ThemePref::Light,
                        _ => ThemePref::System,
                    });
                }
                if let Some(ms) = req.args.get("waitMs").and_then(Value::as_u64) {
                    std::thread::sleep(Duration::from_millis(ms.min(10_000)));
                }
                let t = Instant::now();
                self.draw();
                let ms = t.elapsed().as_secs_f32() * 1000.0;
                let drawn = self.engine.as_ref().map(|e| e.renderer.drawn).unwrap_or(0);
                let png = self
                    .gl
                    .as_mut()
                    .and_then(|g| g.surface.image_snapshot().make_raster_image(&mut g.gr, None))
                    .and_then(|img| render::offscreen::encode(&img, "png", None));
                self.present();
                match png {
                    Some(p) => json!({"content": [
                        {"type": "text", "text": format!("frame {ms:.1} ms, {drawn} nodes drawn, camera {:.0},{:.0} zoom {:.3}, {} drawing", self.cam.x, self.cam.y, self.cam.zoom, self.anims.len())},
                        {"type": "image", "data": crate::api::png_base64(&p), "mimeType": "image/png"}
                    ], "isError": false}),
                    None => ipc::tool_error("could not read back the frame"),
                }
            }
            other => ipc::tool_error(&format!("unknown request {other}")),
        };
        let _ = req.reply.send(reply);
        self.redraw();
    }

    /// Queues newly created paths to be drawn in one after another, like a pen sketching them.
    fn draw_in(&mut self, keys: Vec<String>) {
        let Some(e) = &self.engine else { return };
        let view = self.viewport();
        let zoom = self.cam.zoom;
        let mut items: Vec<(String, f32)> = keys
            .into_iter()
            .filter_map(|k| {
                let r = e.abs_rect(&k)?;
                let px = (r.w + r.h) * zoom;
                // Off-screen or sub-pixel paths just appear.
                (r.intersects(&view) && px > 2.0).then(|| (k, (0.3 + px / 900.0).clamp(0.3, 1.1)))
            })
            .collect();
        if items.is_empty() {
            return;
        }
        // Keep a whole drawing to a few seconds.
        let total: f32 = items.iter().map(|(_, d)| d * 0.7).sum();
        let squeeze = if total > 5.0 { 5.0 / total } else { 1.0 };
        let mut at = Instant::now() + Duration::from_millis(80);
        if let Some(last) = self.anims.iter().map(|a| a.start + Duration::from_secs_f32(a.dur)).max() {
            at = at.max(last);
        }
        for (k, d) in items.drain(..) {
            let dur = (d * squeeze).max(0.08);
            self.anims.push(Anim { key: k, start: at, dur });
            at += Duration::from_secs_f32(dur * 0.7);
        }
        self.redraw();
    }

    /// Sweeps a scan line over nodes the agent read, staggered so several reads ripple across.
    fn scan(&mut self, mut keys: Vec<String>) {
        let Some(e) = &self.engine else { return };
        let view = self.viewport();
        let mut seen = std::collections::HashSet::new();
        keys.retain(|k| seen.insert(k.clone()) && e.abs_rect(k).is_some_and(|r| r.intersects(&view)));
        let now = Instant::now();
        for (i, key) in keys.into_iter().take(24).enumerate() {
            self.scans.push(Scan { key, start: now + Duration::from_millis(140 * i as u64) });
        }
        self.redraw();
    }

    fn select_at(&mut self, sx: f32, sy: f32, toggle: bool) {
        let (dx, dy) = self.to_doc(sx, sy);
        let Some(e) = self.engine.as_mut() else { return };
        let hit = e.hit_test(dx, dy);
        match (hit, toggle) {
            (Some(k), true) => {
                if let Some(i) = e.selection.iter().position(|s| *s == k) {
                    e.selection.remove(i);
                } else {
                    e.selection.push(k);
                }
            }
            (Some(k), false) => e.selection = vec![k],
            (None, false) => e.selection.clear(),
            (None, true) => {}
        }
        self.redraw();
    }

    /// Moves the selection to parents (`up`) or first children.
    fn select_relative(&mut self, up: bool) {
        let Some(e) = self.engine.as_mut() else { return };
        let mut next = Vec::new();
        for k in &e.selection {
            let Some((s, i)) = e.locate(k) else { continue };
            let n = &s.nodes[i as usize];
            let target = if up {
                (n.parent != pen::scene::NONE).then(|| s.nodes[n.parent as usize].key().to_string())
            } else {
                n.children.iter().find(|&&c| s.nodes[c as usize].enabled).map(|&c| s.nodes[c as usize].key().to_string())
            };
            next.push(target.unwrap_or_else(|| k.clone()));
        }
        next.dedup();
        e.selection = next;
        self.redraw();
    }

    // ------------------------------------------------------------------ drawing

    fn draw(&mut self) {
        let sf = self.scale_factor();
        let viewport = self.viewport();
        let (ww, wh) = self.size();
        let cam = self.cam;
        let dark = self.dark;
        let switch = self.theme_switch_rect();
        let banner = self.banner();

        // Progress of pen animations, keyed for the renderer.
        let now = Instant::now();
        self.anims.retain(|a| now < a.start + Duration::from_secs_f32(a.dur));
        let mut reveal: HashMap<u32, HashMap<u32, f32>> = HashMap::new();
        if let Some(e) = &self.engine {
            for a in &self.anims {
                let Some((s, i)) = e.locate(&a.key) else { continue };
                let Some(root) = s.doc_root else { continue };
                let t = if now < a.start { 0.0 } else { (now - a.start).as_secs_f32() / a.dur };
                reveal.entry(root.0).or_default().insert(i, t.min(1.0));
            }
        }
        // Screen rects of nodes being scanned, with progress.
        self.scans.retain(|s| now < s.start + Duration::from_secs_f32(SCAN_SECS));
        let mut scans = Vec::new();
        if let Some(e) = &self.engine {
            for s in self.scans.iter().filter(|s| now >= s.start) {
                let Some((m, w, h)) = e.node_transform(&s.key) else { continue };
                let pts = [m.map(0.0, 0.0), m.map(w, 0.0), m.map(w, h), m.map(0.0, h)].map(|(x, y)| ((x - cam.x) * cam.zoom, (y - cam.y) * cam.zoom));
                let (l, t) = pts.iter().fold((f32::MAX, f32::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
                let (r, b) = pts.iter().fold((f32::MIN, f32::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
                scans.push((skia_safe::Rect::new(l, t, r, b), (now - s.start).as_secs_f32() / SCAN_SECS));
            }
        }
        // Frames the agent is working in: screen rect, corner radius, glow strength.
        self.working.retain(|_, (_, last)| now.duration_since(*last).as_secs_f32() < WORK_HOLD + WORK_FADE);
        let mut glows = Vec::new();
        if let Some(e) = &self.engine {
            for (key, (first, last)) in &self.working {
                let Some((s, i)) = e.locate(key) else { continue };
                let n = &s.nodes[i as usize];
                let r = s.abs_rect(i);
                let rect = skia_safe::Rect::from_xywh((r.x - cam.x) * cam.zoom, (r.y - cam.y) * cam.zoom, r.w * cam.zoom, r.h * cam.zoom);
                let fade_in = (now.duration_since(*first).as_secs_f32() / 0.4).min(1.0);
                let idle = now.duration_since(*last).as_secs_f32();
                let fade_out = 1.0 - ((idle - WORK_HOLD) / WORK_FADE).clamp(0.0, 1.0);
                glows.push((rect, n.radius[0] * cam.zoom, fade_in * fade_out));
            }
        }
        let clock = now.duration_since(self.started).as_secs_f32();

        let Some(g) = self.gl.as_mut() else { return };
        let canvas = g.surface.canvas();
        canvas.clear(if dark { Color::from_rgb(0x1E, 0x1E, 0x20) } else { Color::from_rgb(0xEC, 0xEC, 0xEC) });
        canvas.save();
        canvas.scale((sf, sf));
        let label = Paint::new(if dark { Color4f::new(0.74, 0.74, 0.77, 1.0) } else { Color4f::new(0.42, 0.42, 0.45, 1.0) }, None);
        if let Some(e) = self.engine.as_mut() {
            canvas.save();
            canvas.scale((cam.zoom, cam.zoom));
            canvas.translate((-cam.x, -cam.y));
            let view = View { scale: cam.zoom * sf, viewport, lod: true };
            e.renderer.begin_frame();
            e.renderer.reveal = reveal;
            let roots: Vec<_> = e.doc.roots.clone();
            for r in roots {
                if let Some(s) = e.scenes.get(&r) {
                    e.renderer.draw_scene(canvas, s, &view);
                }
            }
            e.renderer.text.end_frame();
            canvas.restore();

            // Overlays in screen space.
            for &r in &e.doc.roots {
                let Some(s) = e.scenes.get(&r) else { continue };
                let Some(n) = s.root() else { continue };
                if !n.enabled || !matches!(n.kind, pen::scene::Kind::Frame { .. }) {
                    continue;
                }
                let sx = (n.rect.x - cam.x) * cam.zoom;
                let sy = (n.rect.y - cam.y) * cam.zoom;
                let sw = n.rect.w * cam.zoom;
                if sw < 40.0 || sy < -4.0 || sy > wh + 20.0 || sx > ww || sx + sw < 0.0 {
                    continue;
                }
                let name = n.name.as_deref().unwrap_or(&n.id);
                let text = ellipsize(&self.ui, name, sw);
                self.ui.draw(canvas, &text, (sx, sy - 6.0), &label);
            }
            let outline = |key: &str, color: Color4f, width: f32, with_label: bool| {
                let Some((m, w, h)) = e.node_transform(key) else { return };
                let pts = [m.map(0.0, 0.0), m.map(w, 0.0), m.map(w, h), m.map(0.0, h)];
                let mut b = skia_safe::PathBuilder::new();
                for (k, (x, y)) in pts.iter().enumerate() {
                    let p = ((x - cam.x) * cam.zoom, (y - cam.y) * cam.zoom);
                    if k == 0 {
                        b.move_to(p);
                    } else {
                        b.line_to(p);
                    }
                }
                b.close();
                let path: SkPath = b.detach();
                let mut p = Paint::new(color, None);
                p.set_style(PaintStyle::Stroke);
                p.set_stroke_width(width);
                p.set_anti_alias(true);
                canvas.draw_path(&path, &p);
                if with_label {
                    if let Some((s, i)) = e.locate(key) {
                        let n = &s.nodes[i as usize];
                        let txt = format!("{}  {}×{}", n.name.as_deref().unwrap_or(&n.id), n.rect.w.round(), n.rect.h.round());
                        let (x, y) = ((pts[3].0 - cam.x) * cam.zoom, (pts[3].1 - cam.y) * cam.zoom);
                        let tw = self.ui.measure(&txt);
                        let bg = Paint::new(color, None);
                        canvas.draw_round_rect(skia_safe::Rect::from_xywh(x, y + 4.0, tw + 10.0, 17.0), 3.0, 3.0, &bg);
                        self.ui.draw(canvas, &txt, (x + 5.0, y + 16.5), &Paint::new(Color4f::new(1.0, 1.0, 1.0, 1.0), None));
                    }
                }
            };
            if let Some(h) = &self.hover {
                if !e.selection.contains(h) {
                    outline(h, Color4f::new(0.10, 0.55, 1.0, 0.55), 1.0, false);
                }
            }
            for k in &e.selection {
                outline(k, Color4f::new(0.10, 0.55, 1.0, 1.0), 1.5, true);
            }
            for (r, radius, strength) in &glows {
                draw_working(canvas, *r, *radius, *strength, clock, &self.ui);
            }
            for (r, t) in &scans {
                draw_scan(canvas, *r, *t);
            }
            if let Some((p, _)) = e.renderer.tips.last() {
                draw_nib(canvas, p.x / sf, p.y / sf);
            }
            let zoom_txt = format!("{:.0}%", cam.zoom * 100.0);
            let tw = self.ui.measure(&zoom_txt);
            self.ui.draw(canvas, &zoom_txt, (ww - tw - 10.0, wh - 10.0), &label);
        } else {
            let msg = "Open a .pen file: File › Open, Ctrl+O, or drop it here";
            let tw = self.ui.measure(msg);
            self.ui.draw(canvas, msg, ((ww - tw) / 2.0, wh / 2.0), &label);
        }
        draw_theme_switch(canvas, switch, dark);
        if let (Some((panel, buttons)), Some(u)) = (banner, &self.update) {
            draw_banner(canvas, panel, buttons, &u.version, self.banner_hover, &self.ui_title, &self.ui_body);
        }
        canvas.restore();
    }

    fn present(&mut self) {
        let Some(g) = self.gl.as_mut() else { return };
        let _ = g.gr.flush_and_submit();
        let _ = g.gl_surface.swap_buffers(&g.gl_context);
    }
}

fn ellipsize(font: &crate::uitext::UiText, s: &str, max: f32) -> String {
    if font.measure(s) <= max {
        return s.to_string();
    }
    let mut out: String = s.to_string();
    while !out.is_empty() && font.measure(&format!("{out}…")) > max {
        out.pop();
    }
    format!("{out}…")
}

/// The calligraphy pen that draws new paths in: study's logo nib, tip at (x, y).
fn draw_nib(canvas: &skia_safe::Canvas, x: f32, y: f32) {
    const NIB: &str = "M50 88 C44 76 31 61 31 47 L31 31 C31 25 35 21 41 21 L59 21 C65 21 69 25 69 31 L69 47 C69 61 56 76 50 88 Z \
                       M50 41 A6 6 0 1 0 50.01 41 Z M48.8 53 L51.2 53 L50.5 82 L49.5 82 Z";
    let Some(path) = SkPath::from_svg(NIB) else { return };
    let mut m = Matrix::translate((x, y));
    m.pre_rotate(32.0, None);
    m.pre_scale((0.5, 0.5), None);
    m.pre_translate((-50.0, -88.0));
    let mut path = path.make_transform(&m);
    path.set_fill_type(skia_safe::PathFillType::EvenOdd);
    let mut shadow = Paint::new(Color4f::new(0.0, 0.0, 0.0, 0.25), None);
    shadow.set_anti_alias(true);
    shadow.set_mask_filter(skia_safe::MaskFilter::blur(skia_safe::BlurStyle::Normal, 2.0, None));
    canvas.save();
    canvas.translate((1.5, 2.0));
    canvas.draw_path(&path, &shadow);
    canvas.restore();
    let mut fill = Paint::new(Color4f::new(0.97, 0.78, 0.42, 1.0), None);
    fill.set_anti_alias(true);
    canvas.draw_path(&path, &fill);
    let mut edge = Paint::new(Color4f::new(0.36, 0.24, 0.08, 1.0), None);
    edge.set_anti_alias(true);
    edge.set_style(PaintStyle::Stroke);
    edge.set_stroke_width(1.0);
    canvas.draw_path(&path, &edge);
}

const ACCENT: (f32, f32, f32) = (0.23, 0.51, 0.96);

fn accent(a: f32) -> Color4f {
    Color4f::new(ACCENT.0, ACCENT.1, ACCENT.2, a)
}

/// The "agent is reading this" effect, `t` in 0..1: a scan line sweeps down the node's box
/// (first 55%), then the highlighted box lingers and fades.
fn draw_scan(canvas: &skia_safe::Canvas, r: skia_safe::Rect, t: f32) {
    let sweep = (t / 0.55).min(1.0);
    let ease = 1.0 - (1.0 - sweep).powi(3);
    let fade = if t < 0.55 { 1.0 } else { 1.0 - ((t - 0.55) / 0.45).powi(2) };
    let y = r.top + ease * r.height();
    canvas.save();
    canvas.clip_rect(r, None, true);
    let mut tint = Paint::new(accent(0.10 * fade), None);
    tint.set_anti_alias(true);
    canvas.draw_rect(skia_safe::Rect::new(r.left, r.top, r.right, y), &tint);
    if sweep < 1.0 {
        let trail = (r.height() * 0.4).clamp(16.0, 140.0);
        let colors = [accent(0.0).to_color(), accent(0.45).to_color()];
        #[allow(deprecated)]
        let shader = skia_safe::gradient_shader::linear(((r.left, y - trail), (r.left, y)), colors.as_slice(), None, skia_safe::TileMode::Clamp, None, None);
        let mut band = Paint::default();
        band.set_shader(shader);
        canvas.draw_rect(skia_safe::Rect::new(r.left, y - trail, r.right, y), &band);
        let mut glow = Paint::new(accent(0.8), None);
        glow.set_anti_alias(true);
        glow.set_mask_filter(skia_safe::MaskFilter::blur(skia_safe::BlurStyle::Normal, 3.0, None));
        canvas.draw_rect(skia_safe::Rect::new(r.left, y - 2.0, r.right, y + 2.0), &glow);
        let mut line = Paint::new(accent(1.0), None);
        line.set_anti_alias(true);
        line.set_stroke_width(2.0);
        canvas.draw_line((r.left, y), (r.right, y), &line);
    }
    canvas.restore();
    let mut edge = Paint::new(accent(0.9 * fade), None);
    edge.set_anti_alias(true);
    edge.set_style(PaintStyle::Stroke);
    edge.set_stroke_width(1.5);
    canvas.draw_rect(r, &edge);
}

/// "The agent is working here": a softly pulsing glow around a top-level frame, plus a small
/// badge above its top-right corner.
fn draw_working(canvas: &skia_safe::Canvas, r: skia_safe::Rect, radius: f32, strength: f32, clock: f32, font: &crate::uitext::UiText) {
    if strength <= 0.0 {
        return;
    }
    let pulse = 0.7 + 0.3 * (clock * std::f32::consts::TAU / 1.8).sin();
    let rr = skia_safe::RRect::new_rect_xy(r, radius, radius);
    for (width, sigma, alpha) in [(10.0, 14.0, 0.45), (4.0, 5.0, 0.6)] {
        let mut p = Paint::new(accent(alpha * pulse * strength), None);
        p.set_anti_alias(true);
        p.set_style(PaintStyle::Stroke);
        p.set_stroke_width(width);
        p.set_mask_filter(skia_safe::MaskFilter::blur(skia_safe::BlurStyle::Outer, sigma, None));
        canvas.draw_rrect(rr, &p);
    }
    let mut edge = Paint::new(accent(0.85 * strength), None);
    edge.set_anti_alias(true);
    edge.set_style(PaintStyle::Stroke);
    edge.set_stroke_width(1.5);
    canvas.draw_rrect(rr, &edge);

    let label = "Claude is working";
    let tw = font.measure(label);
    let badge = skia_safe::Rect::from_xywh(r.right - tw - 30.0, r.top - 26.0, tw + 30.0, 20.0);
    if badge.left < r.left {
        return;
    }
    let mut bg = Paint::new(accent(strength), None);
    bg.set_anti_alias(true);
    canvas.draw_round_rect(badge, 10.0, 10.0, &bg);
    let mut dot = Paint::new(Color4f::new(1.0, 1.0, 1.0, strength * (0.45 + 0.55 * pulse)), None);
    dot.set_anti_alias(true);
    canvas.draw_circle((badge.left + 11.0, badge.center_y()), 3.5, &dot);
    font.draw(canvas, label, (badge.left + 20.0, badge.center_y() + 4.0), &Paint::new(Color4f::new(1.0, 1.0, 1.0, strength), None));
}

/// The "update ready" banner: a dark card with a title, a line of text and two buttons.
fn draw_banner(canvas: &skia_safe::Canvas, panel: skia_safe::Rect, buttons: [skia_safe::Rect; 2], version: &str, hover: Option<usize>, title: &crate::uitext::UiText, body: &crate::uitext::UiText) {
    let rr = skia_safe::RRect::new_rect_xy(panel, 14.0, 14.0);
    let mut shadow = Paint::new(Color4f::new(0.0, 0.0, 0.0, 0.35), None);
    shadow.set_anti_alias(true);
    shadow.set_mask_filter(skia_safe::MaskFilter::blur(skia_safe::BlurStyle::Normal, 12.0, None));
    canvas.save();
    canvas.translate((0.0, 6.0));
    canvas.draw_rrect(rr, &shadow);
    canvas.restore();
    let mut bg = Paint::new(Color4f::new(0.10, 0.10, 0.11, 0.98), None);
    bg.set_anti_alias(true);
    canvas.draw_rrect(rr, &bg);
    let mut edge = Paint::new(Color4f::new(1.0, 1.0, 1.0, 0.08), None);
    edge.set_anti_alias(true);
    edge.set_style(PaintStyle::Stroke);
    canvas.draw_rrect(rr, &edge);

    let white = Paint::new(Color4f::new(1.0, 1.0, 1.0, 1.0), None);
    title.draw(canvas, "Update Ready", (panel.left + 20.0, panel.top + 32.0), &white);
    let msg = format!("study {version} has been downloaded and is ready to install.");
    body.draw(canvas, &msg, (panel.left + 20.0, panel.top + 55.0), &Paint::new(Color4f::new(0.66, 0.66, 0.70, 1.0), None));

    for (i, (b, label)) in buttons.iter().zip(["Install and Restart", "Install on next launch"]).enumerate() {
        let brr = skia_safe::RRect::new_rect_xy(*b, 8.0, 8.0);
        let hot = hover == Some(i);
        let mut p = Paint::default();
        p.set_anti_alias(true);
        let text = if i == 0 {
            p.set_color4f(if hot { Color4f::new(0.88, 0.88, 0.90, 1.0) } else { Color4f::new(1.0, 1.0, 1.0, 1.0) }, None);
            canvas.draw_rrect(brr, &p);
            Color4f::new(0.07, 0.07, 0.08, 1.0)
        } else {
            if hot {
                p.set_color4f(Color4f::new(1.0, 1.0, 1.0, 0.08), None);
                canvas.draw_rrect(brr, &p);
            }
            p.set_color4f(Color4f::new(1.0, 1.0, 1.0, 0.22), None);
            p.set_style(PaintStyle::Stroke);
            canvas.draw_rrect(brr, &p);
            Color4f::new(1.0, 1.0, 1.0, 1.0)
        };
        let tw = body.measure(label);
        body.draw(canvas, label, (b.center_x() - tw / 2.0, b.center_y() + 4.5), &Paint::new(text, None));
    }
}

/// A two-segment light/dark switch: sun on the left, moon on the right.
fn draw_theme_switch(canvas: &skia_safe::Canvas, r: skia_safe::Rect, dark: bool) {
    let (track, knob, fg, fg_dim) = if dark {
        (Color4f::new(1.0, 1.0, 1.0, 0.08), Color4f::new(1.0, 1.0, 1.0, 0.16), Color4f::new(0.92, 0.92, 0.95, 1.0), Color4f::new(0.6, 0.6, 0.64, 1.0))
    } else {
        (Color4f::new(0.0, 0.0, 0.0, 0.06), Color4f::new(1.0, 1.0, 1.0, 1.0), Color4f::new(0.15, 0.15, 0.17, 1.0), Color4f::new(0.5, 0.5, 0.53, 1.0))
    };
    let mut p = Paint::new(track, None);
    p.set_anti_alias(true);
    canvas.draw_round_rect(r, r.height() / 2.0, r.height() / 2.0, &p);
    let half = r.width() / 2.0;
    let active = skia_safe::Rect::from_xywh(r.left + if dark { half } else { 0.0 } + 2.0, r.top + 2.0, half - 4.0, r.height() - 4.0);
    p.set_color4f(knob, None);
    canvas.draw_round_rect(active, active.height() / 2.0, active.height() / 2.0, &p);

    let (sx, sy) = (r.left + half / 2.0, r.center_y());
    let mut sun = Paint::new(if dark { fg_dim } else { fg }, None);
    sun.set_anti_alias(true);
    canvas.draw_circle((sx, sy), 3.0, &sun);
    sun.set_style(PaintStyle::Stroke);
    sun.set_stroke_width(1.2);
    sun.set_stroke_cap(skia_safe::PaintCap::Round);
    for k in 0..8 {
        let a = k as f32 * std::f32::consts::FRAC_PI_4;
        let (c, s) = (a.cos(), a.sin());
        canvas.draw_line((sx + c * 4.8, sy + s * 4.8), (sx + c * 6.4, sy + s * 6.4), &sun);
    }
    let (mx, my) = (r.left + half * 1.5, r.center_y());
    let mut moon = SkPath::circle((mx, my), 5.0, None);
    if let Some(m) = moon.op(&SkPath::circle((mx + 3.2, my - 2.4), 4.2, None), skia_safe::PathOp::Difference) {
        moon = m;
    }
    let mut mp = Paint::new(if dark { fg } else { fg_dim }, None);
    mp.set_anti_alias(true);
    canvas.draw_path(&moon, &mp);
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gl.is_some() {
            return;
        }
        match create_gl(el) {
            Ok(g) => self.gl = Some(g),
            Err(e) => {
                eprintln!("study: could not create an OpenGL window: {e:#}");
                el.exit();
                return;
            }
        }
        self.apply_theme();
        self.build_menus();
        self.apply_theme();
        if let Some(f) = self.initial.take() {
            self.open(&f);
        }
        self.update_title();
        el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250)));
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Request(req) if req.tool == "command" => {
                // Development aid: run a menu command by id.
                let id = req.args.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let _ = req.reply.send(json!({"content": [{"type": "text", "text": format!("ran {id}")}], "isError": false}));
                self.command(el, &id);
            }
            UserEvent::Request(req) => self.handle_request(req),
            UserEvent::Menu(id) => self.command(el, &id),
            UserEvent::UpdateReady(u) => {
                self.update = Some(u);
                self.banner_hidden = false;
                self.redraw();
            }
            UserEvent::UpdateStatus(msg) => {
                rfd::MessageDialog::new().set_title("study").set_description(msg).show();
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let mut wake = Duration::from_millis(1000);
        let check_disk = self.last_poll.elapsed() > Duration::from_millis(900);
        if check_disk {
            self.last_poll = Instant::now();
        }
        let mut dirty = false;
        if let Some(e) = self.engine.as_mut() {
            dirty |= e.poll_assets();
            if e.renderer.has_pending() {
                wake = Duration::from_millis(100);
            }
            if check_disk && e.reload_if_changed() {
                e.prune_selection();
                self.anims.clear();
                dirty = true;
            }
        }
        if dirty {
            self.redraw();
        }
        el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + wake));
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.save_camera();
                el.exit();
            }
            WindowEvent::Focused(true) => {
                let p = self.engine.as_ref().and_then(|e| e.path().map(Path::to_path_buf));
                ipc::register(self.port, &self.token, p.as_deref(), true);
            }
            WindowEvent::ThemeChanged(_) if self.theme == ThemePref::System => self.apply_theme(),
            WindowEvent::Resized(size) => {
                if let Some(g) = self.gl.as_mut() {
                    if size.width > 0 && size.height > 0 {
                        g.gl_surface.resize(&g.gl_context, NonZeroU32::new(size.width).unwrap(), NonZeroU32::new(size.height).unwrap());
                        g.surface = create_surface(&g.window, g.fb_info, &mut g.gr, g.samples, g.stencil);
                    }
                }
                self.redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => self.redraw(),
            WindowEvent::RedrawRequested => {
                if self.pending_fit && self.engine.is_some() {
                    self.pending_fit = false;
                    self.fit(None);
                }
                self.draw();
                self.present();
                if !self.anims.is_empty() || !self.scans.is_empty() || !self.working.is_empty() || self.engine.as_ref().is_some_and(|e| e.renderer.needs_redraw) {
                    self.redraw();
                }
            }
            WindowEvent::DroppedFile(p) => self.open(&p),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let sf = self.scale_factor();
                let (x, y) = (position.x as f32 / sf, position.y as f32 / sf);
                if let Some((px, py)) = self.pan_from {
                    self.cam.x -= (x - px) / self.cam.zoom;
                    self.cam.y -= (y - py) / self.cam.zoom;
                    self.pan_from = Some((x, y));
                    self.redraw();
                } else if let Some(e) = &self.engine {
                    let (dx, dy) = self.to_doc(x, y);
                    let pt = skia_safe::Point::new(x, y);
                    let over_banner = self.banner().map(|(panel, buttons)| (panel.contains(pt), buttons.iter().position(|b| b.contains(pt))));
                    let banner_hover = over_banner.and_then(|(_, b)| b);
                    if banner_hover != self.banner_hover {
                        self.banner_hover = banner_hover;
                        self.redraw();
                    }
                    let blocked = over_banner.is_some_and(|(p, _)| p) || self.theme_switch_rect().contains(pt);
                    let h = if blocked { None } else { e.hit_test(dx, dy) };
                    if h != self.hover {
                        self.hover = h;
                        self.redraw();
                    }
                }
                self.cursor = (x, y);
            }
            WindowEvent::CursorLeft { .. } => {
                if self.hover.take().is_some() {
                    self.redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Middle, ElementState::Pressed) => self.pan_from = Some(self.cursor),
                (MouseButton::Left, ElementState::Pressed) if self.space_down => self.pan_from = Some(self.cursor),
                (MouseButton::Middle | MouseButton::Left, ElementState::Released) if self.pan_from.is_some() => self.pan_from = None,
                (MouseButton::Left, ElementState::Pressed) => {
                    let (x, y) = self.cursor;
                    let pt = skia_safe::Point::new(x, y);
                    let r = self.theme_switch_rect();
                    if let Some((panel, buttons)) = self.banner() {
                        if let Some(i) = buttons.iter().position(|b| b.contains(pt)) {
                            self.command(el, if i == 0 { "update_install" } else { "update_later" });
                            return;
                        }
                        if panel.contains(pt) {
                            return;
                        }
                    }
                    if r.contains(pt) {
                        self.set_theme(if x < r.center_x() { ThemePref::Light } else { ThemePref::Dark });
                    } else {
                        self.select_at(x, y, self.mods.shift_key());
                    }
                }
                _ => {}
            },
            // Trackpad pinch (macOS).
            WindowEvent::PinchGesture { delta, .. } => {
                let (x, y) = self.cursor;
                self.zoom_at(1.0 + delta as f32, x, y);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let sf = self.scale_factor();
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * 50.0, y * 50.0),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32 / sf, p.y as f32 / sf),
                };
                if self.mods.control_key() || self.mods.super_key() {
                    let factor = 1.0018f32.powf(dy);
                    let (x, y) = self.cursor;
                    self.zoom_at(factor, x, y);
                } else if self.mods.shift_key() {
                    self.cam.x -= (dy + dx) / self.cam.zoom;
                    self.redraw();
                } else {
                    self.cam.x -= dx / self.cam.zoom;
                    self.cam.y -= dy / self.cam.zoom;
                    self.redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;
                if let Key::Named(NamedKey::Space) = event.logical_key {
                    self.space_down = pressed;
                    return;
                }
                if !pressed {
                    return;
                }
                let ctrl = self.mods.control_key() || self.mods.super_key();
                let shift = self.mods.shift_key();
                let cmd = match &event.logical_key {
                    Key::Character(c) if ctrl => match c.to_lowercase().as_str() {
                        "n" => "new",
                        "o" => "open",
                        "s" if shift => "save_as",
                        "s" => "save",
                        "z" if shift => "redo",
                        "z" => "undo",
                        "y" => "redo",
                        "0" => "fit",
                        "1" => "actual",
                        "2" => "zoom_sel",
                        "=" | "+" => "zoom_in",
                        "-" => "zoom_out",
                        _ => return,
                    },
                    Key::Character(c) if c.as_str() == "b" || c.as_str() == "B" => {
                        if self.dark {
                            "theme_light"
                        } else {
                            "theme_dark"
                        }
                    }
                    Key::Named(NamedKey::Escape) => "deselect",
                    Key::Named(NamedKey::F5) => "reload",
                    Key::Named(NamedKey::Enter) => {
                        self.select_relative(shift);
                        return;
                    }
                    _ => return,
                };
                self.command(el, cmd);
            }
            _ => {}
        }
    }
}

fn app_icon() -> Option<Icon> {
    let img = skia_safe::Image::from_encoded(skia_safe::Data::new_copy(include_bytes!("../../../assets/icon-256.png")))?;
    let (w, h) = (img.width(), img.height());
    let info = skia_safe::ImageInfo::new((w, h), skia_safe::ColorType::RGBA8888, skia_safe::AlphaType::Unpremul, None);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    img.read_pixels(&info, &mut rgba, (w * 4) as usize, (0, 0), skia_safe::image::CachingHint::Allow).then_some(())?;
    Icon::from_rgba(rgba, w as u32, h as u32).ok()
}

fn create_gl(el: &ActiveEventLoop) -> Result<Gl> {
    let icon = app_icon();
    let attrs = WindowAttributes::default()
        .with_title("study")
        .with_inner_size(winit::dpi::LogicalSize::new(1440.0, 900.0))
        .with_window_icon(icon.clone());
    #[cfg(windows)]
    let attrs = attrs.with_taskbar_icon(icon);
    let template = ConfigTemplateBuilder::new().with_alpha_size(8).with_transparency(false);
    let (window, config) = DisplayBuilder::new()
        .with_window_attributes(Some(attrs))
        .build(el, template, |configs| configs.reduce(|a, c| if c.num_samples() < a.num_samples() { c } else { a }).unwrap())
        .map_err(|e| anyhow!("{e}"))?;
    let window = window.ok_or_else(|| anyhow!("no window"))?;
    let raw = window.window_handle()?.as_raw();
    // macOS only offers modern OpenGL as a 3.2+ core profile, and only when asked for it.
    #[cfg(target_os = "macos")]
    let ctx_attrs = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::OpenGl(Some(glutin::context::Version::new(4, 1))))
        .with_profile(glutin::context::GlProfile::Core)
        .build(Some(raw));
    #[cfg(not(target_os = "macos"))]
    let ctx_attrs = ContextAttributesBuilder::new().build(Some(raw));
    let gles_attrs = ContextAttributesBuilder::new().with_context_api(ContextApi::Gles(None)).build(Some(raw));
    let not_current = unsafe {
        config
            .display()
            .create_context(&config, &ctx_attrs)
            .or_else(|_| config.display().create_context(&config, &gles_attrs))?
    };
    let size = window.inner_size();
    let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
        raw,
        NonZeroU32::new(size.width.max(1)).unwrap(),
        NonZeroU32::new(size.height.max(1)).unwrap(),
    );
    let gl_surface = unsafe { config.display().create_window_surface(&config, &surface_attrs)? };
    let gl_context = not_current.make_current(&gl_surface)?;
    let _ = gl_surface.set_swap_interval(&gl_context, SwapInterval::Wait(NonZeroU32::new(1).unwrap()));
    gl::load_with(|s| config.display().get_proc_address(CString::new(s).unwrap().as_c_str()));
    let interface = skia_safe::gpu::gl::Interface::new_load_with(|name| {
        if name == "eglGetCurrentDisplay" {
            return std::ptr::null();
        }
        config.display().get_proc_address(CString::new(name).unwrap().as_c_str())
    })
    .ok_or_else(|| anyhow!("could not create the Skia GL interface"))?;
    let mut gr = skia_safe::gpu::direct_contexts::make_gl(interface, None).ok_or_else(|| anyhow!("could not create the Skia GL context"))?;
    let fb_info = {
        let mut fboid: gl::types::GLint = 0;
        unsafe { gl::GetIntegerv(gl::FRAMEBUFFER_BINDING, &mut fboid) };
        FramebufferInfo { fboid: fboid as u32, format: skia_safe::gpu::gl::Format::RGBA8.into(), ..Default::default() }
    };
    let samples = config.num_samples() as usize;
    let stencil = config.stencil_size() as usize;
    let surface = create_surface(&window, fb_info, &mut gr, samples, stencil);
    Ok(Gl { window, surface, gl_surface, gl_context, gr, fb_info, samples, stencil })
}

fn create_surface(window: &Window, fb: FramebufferInfo, gr: &mut skia_safe::gpu::DirectContext, samples: usize, stencil: usize) -> skia_safe::Surface {
    let size = window.inner_size();
    let size = (size.width.max(1) as i32, size.height.max(1) as i32);
    let target = skia_safe::gpu::backend_render_targets::make_gl(size, samples, stencil, fb);
    skia_safe::gpu::surfaces::wrap_backend_render_target(
        gr,
        &target,
        skia_safe::gpu::SurfaceOrigin::BottomLeft,
        skia_safe::ColorType::RGBA8888,
        None,
        None,
    )
    .expect("could not create the Skia surface")
}

fn state_file() -> PathBuf {
    render::data_dir().join("state.json")
}

fn load_state() -> Value {
    std::fs::read_to_string(state_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({}))
}

fn update_state(f: impl FnOnce(&mut Value)) {
    let mut s = load_state();
    if !s.is_object() {
        s = json!({});
    }
    f(&mut s);
    let _ = std::fs::create_dir_all(render::data_dir());
    let _ = std::fs::write(state_file(), serde_json::to_string_pretty(&s).unwrap_or_default());
}
