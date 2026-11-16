//! The `browser` tool: a real Chrome/Chromium over the DevTools Protocol.
//!
//! Pen drives its embedded browser; study drives a headless one (or a visible window with
//! `STUDY_BROWSER_HEADED=1`), which also works on a server. Pages can be read, screenshotted,
//! imported as editable layers, or driven through raw CDP.

pub mod cdp;
pub mod convert;
pub mod css;

use anyhow::{anyhow, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const DOM_JS: &str = include_str!("dom.js");

pub enum Reply {
    Text(String),
    Image(Vec<u8>),
    /// A node to insert on the canvas; the caller runs the snippet so undo and saving happen
    /// through the same path as `execute`.
    Canvas { node: Value, note: String },
}

pub struct Session {
    browser: Option<Browser>,
    cache: HashMap<String, String>,
}

impl Default for Session {
    fn default() -> Self {
        Session { browser: None, cache: HashMap::new() }
    }
}

impl Session {
    pub fn status(&self) -> Option<String> {
        let b = self.browser.as_ref()?;
        let more = if b.headless { "headless" } else { "window" };
        Some(format!("Browser: {} ({more}, profile {})", b.exe.display(), b.profile.display()))
    }

    pub fn close(&mut self) {
        self.browser = None;
    }

    /// Runs one tool call. `doc` is the document the result belongs to (assets live beside it).
    pub fn call(&mut self, action: &str, args: &Value, doc: &Path) -> Result<Reply> {
        if std::env::var_os("STUDY_NO_BROWSER").is_some() {
            return Err(anyhow!("the browser tool is disabled (STUDY_NO_BROWSER is set)"));
        }
        if action == "close" {
            let running = self.browser.is_some();
            self.close();
            return Ok(Reply::Text(if running { "Browser closed.".into() } else { "The browser was not running.".into() }));
        }
        let target = self.ensure()?;

        match action {
            "load-page" => {
                let url = args.get("url").and_then(Value::as_str).ok_or_else(|| anyhow!("`url` is required for load-page"))?;
                check_url(url)?;
                if let Some(vp) = args.get("viewport") {
                    let w = vp.get("width").and_then(Value::as_u64).unwrap_or(1440).max(200);
                    let h = vp.get("height").and_then(Value::as_u64).unwrap_or(900).max(200);
                    target.send(
                        "Emulation.setDeviceMetricsOverride",
                        json!({"width": w, "height": h, "deviceScaleFactor": 1, "mobile": false}),
                    )?;
                }
                target.navigate(url)?;
                target.wait_ready(timeout(args, 30))?;
                if let Some(sel) = args.get("waitFor").and_then(Value::as_str) {
                    target.wait_selector(sel, timeout(args, 30))?;
                }
                let info = target.evaluate(
                    "({title: document.title, url: location.href, w: innerWidth, h: innerHeight, \
                      sw: document.documentElement.scrollWidth, sh: document.documentElement.scrollHeight})",
                )?;
                Ok(Reply::Text(format!(
                    "Loaded {}\nTitle: {}\nViewport: {}×{} (page {}×{})",
                    info["url"].as_str().unwrap_or(url),
                    info["title"].as_str().unwrap_or(""),
                    info["w"].as_u64().unwrap_or(0),
                    info["h"].as_u64().unwrap_or(0),
                    info["sw"].as_u64().unwrap_or(0),
                    info["sh"].as_u64().unwrap_or(0),
                )))
            }

            "return-element" => {
                let expr = element_expr(args)?;
                let js = format!(
                    "(() => {{ const el = {expr}; if (!el) return null; \
                      const cs = getComputedStyle(el); const styles = {{}}; \
                      for (const k of ['display','position','flexDirection','flexWrap','justifyContent','alignItems','gap',\
                        'padding','margin','width','height','backgroundColor','backgroundImage','color','fontFamily','fontSize',\
                        'fontWeight','fontStyle','lineHeight','letterSpacing','textAlign','textDecorationLine','whiteSpace',\
                        'overflow','borderRadius','borderTopWidth','borderTopColor','boxShadow','opacity','zIndex','objectFit']) \
                        styles[k] = cs[k]; \
                      const r = el.getBoundingClientRect(); \
                      return {{tag: el.tagName.toLowerCase(), id: el.id || null, classes: String(el.className || '').split(/\\s+/).filter(Boolean), \
                        rect: {{x: r.left + scrollX, y: r.top + scrollY, w: r.width, h: r.height}}, \
                        text: (el.innerText || el.value || '').replace(/\\s+/g, ' ').trim().slice(0, 400), \
                        html: el.outerHTML.slice(0, 3000), styles}}; }})()"
                );
                let v = target.evaluate(&js)?;
                if v.is_null() {
                    return Err(anyhow!("no element matched {}", describe_target(args)));
                }
                let mut text = serde_json::to_string_pretty(&v)?;
                if text.len() > 24_000 {
                    text.truncate(24_000);
                    text.push_str("\n… (truncated)");
                }
                Ok(Reply::Text(text))
            }

            "return-screenshot" | "screenshot-to-canvas" => {
                let rect = clip_rect(target, args)?;
                let png = target.screenshot(rect)?;
                if action == "return-screenshot" {
                    return Ok(Reply::Image(png));
                }
                let (w, h) = rect.map(|r| (r.2, r.3)).unwrap_or(target.page_size()?);
                let name = screenshot_name(target)?;
                let url = self.save_bytes(&png, "png", doc)?;
                Ok(Reply::Canvas {
                    node: json!({
                        "type": "rectangle",
                        "name": name,
                        "width": round(w),
                        "height": round(h),
                        "fill": {"type": "image", "url": url, "mode": "fill"},
                    }),
                    note: format!("Screenshot placed ({:.0}×{:.0}).", w, h),
                })
            }

            "import-to-canvas" => {
                let expr = element_expr(args)?;
                target.evaluate(&format!("window.__studyTarget = {expr}; window.__studyOpts = {{maxNodes: 4000, maxDepth: 40}}; true"))?;
                let v = target.evaluate(DOM_JS)?;
                let _ = target.evaluate("delete window.__studyTarget; delete window.__studyOpts; true");
                if v.is_null() {
                    return Err(anyhow!("no element matched {}", describe_target(args)));
                }
                let page: convert::Page = serde_json::from_value(v).map_err(|e| anyhow!("could not read the page: {e}"))?;
                let mut warnings = Vec::new();
                let mut resolve = |url: &str| self.asset(url, doc);
                let mut imported = convert::convert(&page, None, &mut resolve);
                warnings.append(&mut imported.warnings);
                let mut note = format!("Imported {} nodes, {} images.", imported.nodes, imported.images);
                if !warnings.is_empty() {
                    note.push_str("\nWarnings:\n");
                    for w in warnings.iter().take(12) {
                        note.push_str(&format!("- {w}\n"));
                    }
                }
                Ok(Reply::Canvas { node: imported.node, note })
            }

            "evaluate" => {
                let expr = args
                    .get("params")
                    .and_then(|p| p.get("expression"))
                    .and_then(Value::as_str)
                    .or_else(|| args.get("expression").and_then(Value::as_str))
                    .ok_or_else(|| anyhow!("`params.expression` is required for evaluate"))?;
                let v = target.evaluate(expr)?;
                Ok(Reply::Text(json_text(&v)))
            }

            "cdp" => {
                let method = args.get("method").and_then(Value::as_str).ok_or_else(|| anyhow!("`method` is required for cdp"))?;
                let params = args.get("params").cloned().unwrap_or(json!({}));
                let v = target.passthrough(method, params)?;
                if method == "Page.captureScreenshot" {
                    if let Some(data) = v.get("data").and_then(Value::as_str) {
                        let png = base64::engine::general_purpose::STANDARD.decode(data)?;
                        return Ok(Reply::Image(png));
                    }
                }
                Ok(Reply::Text(json_text(&v)))
            }

            _ => Err(anyhow!(
                "unknown browser action '{action}'; use load-page, import-to-canvas, screenshot-to-canvas, return-element, return-screenshot, evaluate or cdp"
            )),
        }
    }

    fn ensure(&mut self) -> Result<&mut cdp::Target> {
        if self.browser.is_none() {
            self.browser = Some(Browser::launch()?);
        }
        Ok(&mut self.browser.as_mut().unwrap().target)
    }

    /// A raster image from the page, saved next to the document and named by content hash.
    fn asset(&mut self, url: &str, doc: &Path) -> Option<String> {
        if let Some(hit) = self.cache.get(url) {
            return Some(hit.clone());
        }
        let (bytes, ext) = self.fetch(url)?;
        let path = self.save_bytes(&bytes, &ext, doc).ok()?;
        self.cache.insert(url.to_string(), path.clone());
        Some(path)
    }

    fn fetch(&mut self, url: &str) -> Option<(Vec<u8>, String)> {
        if let Some(rest) = url.strip_prefix("data:") {
            let (meta, data) = rest.split_once(',')?;
            let ext = meta.strip_prefix("image/").unwrap_or("png").split(';').next().unwrap_or("png").to_string();
            let raw = if meta.ends_with(";base64") {
                base64::engine::general_purpose::STANDARD.decode(data).ok()?
            } else {
                percent_decode(data).into_bytes()
            };
            return Some((raw, mime_ext(&ext)));
        }
        if url.starts_with("blob:") {
            let js = format!(
                "(async () => {{ const b = await (await fetch({u})).blob(); \
                  const d = await new Promise(r => {{ const f = new FileReader(); f.onload = () => r(f.result); f.readAsDataURL(b); }}); \
                  return {{d}}; }})()",
                u = json!(url)
            );
            let v = self.browser.as_mut()?.target.evaluate(&js).ok()?;
            let data = v.get("d")?.as_str()?;
            return self.fetch(data);
        }
        if let Some(path) = url.strip_prefix("file://") {
            pen::access::check_read(Path::new(path)).ok()?;
            let bytes = std::fs::read(path).ok()?;
            let ext = magic_ext(&bytes).unwrap_or("png");
            return Some((bytes, ext.to_string()));
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return None;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        render::net::get(url.to_string(), 32 << 20, move |r| {
            let _ = tx.send(r);
        });
        let bytes = rx.recv_timeout(Duration::from_secs(30)).ok()?.ok()?;
        let ext = magic_ext(&bytes).map(str::to_string).unwrap_or_else(|| url_ext(url));
        Some((bytes, ext))
    }

    fn save_bytes(&self, bytes: &[u8], ext: &str, doc: &Path) -> Result<String> {
        let dir = doc.parent().unwrap_or(Path::new("."));
        let stem = doc.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "document".into());
        let assets = dir.join(format!("{stem}.assets"));
        pen::access::check_write(&assets.join("x")).map_err(|e| anyhow!(e))?;
        std::fs::create_dir_all(&assets)?;
        let mut h = Sha256::new();
        h.update(bytes);
        let name = format!("{:x}", h.finalize());
        let file = assets.join(format!("{}.{ext}", &name[..16]));
        if !file.exists() {
            std::fs::write(&file, bytes)?;
        }
        let rel = file
            .strip_prefix(dir)
            .map(|p| p.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join("/"))
            .unwrap_or_else(|_| file.display().to_string());
        Ok(rel)
    }
}

fn json_text(v: &Value) -> String {
    let text = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
    if text.len() > 24_000 {
        format!("{}\n… (truncated)", &text[..24_000])
    } else {
        text
    }
}

fn timeout(args: &Value, default: u64) -> Duration {
    Duration::from_millis(args.get("timeout").and_then(Value::as_u64).unwrap_or(default * 1000).clamp(1000, 300_000))
}

fn describe_target(args: &Value) -> String {
    match args.get("target").and_then(Value::as_str).unwrap_or("full-page") {
        "query" => args.get("querySelector").and_then(Value::as_str).map(|s| format!("`{s}`")).unwrap_or_else(|| "the query".into()),
        "selection" => "the picked element".into(),
        other => format!("`{other}`"),
    }
}

/// A JavaScript expression for the element the action works on.
fn element_expr(args: &Value) -> Result<String> {
    match args.get("target").and_then(Value::as_str).unwrap_or("full-page") {
        "query" => {
            let sel = args
                .get("querySelector")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("`querySelector` is required when target is \"query\""))?;
            Ok(format!("document.querySelector({})", json!(sel)))
        }
        "selection" => Err(anyhow!("element picking needs a visible browser window; use target \"query\" with a selector")),
        "full-page" => Ok("document.documentElement".into()),
        other => Err(anyhow!("target must be \"full-page\", \"selection\" or \"query\", not \"{other}\"")),
    }
}

fn clip_rect(target: &mut cdp::Target, args: &Value) -> Result<Option<(f64, f64, f64, f64)>> {
    match args.get("target").and_then(Value::as_str).unwrap_or("full-page") {
        "full-page" => Ok(None),
        "query" => {
            let sel = args
                .get("querySelector")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("`querySelector` is required when target is \"query\""))?;
            Ok(Some(target.element_rect(sel)?))
        }
        "selection" => Err(anyhow!("element picking needs a visible browser window; use target \"query\" with a selector")),
        other => Err(anyhow!("target must be \"full-page\", \"selection\" or \"query\", not \"{other}\"")),
    }
}

fn screenshot_name(target: &mut cdp::Target) -> Result<String> {
    let t = target.evaluate("document.title").unwrap_or(Value::Null);
    let t = t.as_str().unwrap_or("").trim();
    Ok(if t.is_empty() { "Screenshot".into() } else { format!("Screenshot — {t}") })
}

fn check_url(url: &str) -> Result<()> {
    if let Some(path) = url.strip_prefix("file://") {
        return pen::access::check_read(Path::new(path)).map_err(|e| anyhow!(e));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        if !render::net::url_allowed(url) {
            return Err(anyhow!("{url} is not in STUDY_FETCH_HOSTS"));
        }
        return Ok(());
    }
    if url.starts_with("data:") || url.starts_with("about:") {
        return Ok(());
    }
    Err(anyhow!("load-page needs an http(s), file or data URL"))
}

fn mime_ext(m: &str) -> String {
    match m {
        "jpeg" | "jpg" => "jpg",
        "svg+xml" => "svg",
        "x-icon" => "ico",
        other => other,
    }
    .to_string()
}

fn magic_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        Some("jpg")
    } else if bytes.starts_with(b"GIF8") {
        Some("gif")
    } else if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        Some("svg")
    } else {
        None
    }
}

fn url_ext(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "avif" | "ico" => path.rsplit('.').next().unwrap_or("png").to_ascii_lowercase(),
        _ => "png".into(),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn round(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

struct Browser {
    child: Child,
    target: cdp::Target,
    exe: PathBuf,
    profile: PathBuf,
    headless: bool,
    persistent: bool,
}

impl Drop for Browser {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // The whole group: Chrome's zygote and renderers go with it.
            let _ = std::process::Command::new("kill").arg("-KILL").arg(format!("-{}", self.child.id())).status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !self.persistent {
            let _ = std::fs::remove_dir_all(&self.profile);
        }
    }
}

/// Chrome records its owner as a `SingletonLock` symlink (`hostname-pid`). On Unix, if that
/// process is gone the lock is stale and the next launch would abort; remove it. Windows has no
/// portable liveness check here, so the private-profile fallback handles a held profile instead.
fn clear_stale_lock(profile: &Path) {
    #[cfg(not(unix))]
    {
        let _ = profile;
    }
    #[cfg(unix)]
    {
        let lock = profile.join("SingletonLock");
        let Ok(target) = std::fs::read_link(&lock) else { return };
        let pid = target.to_string_lossy().rsplit('-').next().and_then(|p| p.parse::<u32>().ok());
        // `kill -0` tests liveness without a signal on Linux and macOS alike.
        let alive = pid.is_some_and(|p| {
            std::process::Command::new("kill")
                .arg("-0")
                .arg(p.to_string())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        });
        if alive {
            return;
        }
        let _ = std::fs::remove_file(&lock);
        let _ = std::fs::remove_file(profile.join("SingletonSocket"));
        let _ = std::fs::remove_file(profile.join("SingletonCookie"));
    }
}

impl Browser {
    fn launch() -> Result<Browser> {
        let shared = render::data_dir().join("browser");
        clear_stale_lock(&shared);
        match Browser::launch_in(&shared, true) {
            Ok(b) => Ok(b),
            Err(first) => {
                // Another study may hold the profile (Chrome refuses to share it). A private
                // profile still works; only sign-ins are lost.
                let unique = render::data_dir().join("browser").join(format!("session-{}", std::process::id()));
                Browser::launch_in(&unique, false).map_err(|e| anyhow!("{first}\n(and with a private profile: {e})"))
            }
        }
    }

    fn launch_in(profile: &Path, persistent: bool) -> Result<Browser> {
        let exe = browser_exe()?;
        let port = cdp::free_port()?;
        let profile = profile.to_path_buf();
        std::fs::create_dir_all(&profile)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700));
        }
        let headless = std::env::var_os("STUDY_BROWSER_HEADED").is_none();
        let mut cmd = Command::new(&exe);
        cmd.arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", profile.display()))
            .args([
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-component-update",
                "--disable-sync",
                "--disable-default-apps",
                "--disable-extensions",
                "--metrics-recording-only",
                "--mute-audio",
                "--hide-crash-restore-bubble",
                "--window-size=1440,900",
            ])
            .arg(if headless { "--headless=new" } else { "--start-maximized" });
        if headless {
            cmd.arg("--disable-gpu");
        }
        if let Ok(extra) = std::env::var("STUDY_BROWSER_ARGS") {
            cmd.args(extra.split_whitespace());
        }
        cmd.arg("about:blank");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let log = std::fs::OpenOptions::new().create(true).append(true).open(profile.join("browser.log")).ok();
        cmd.stdin(Stdio::null());
        match &log {
            Some(f) => {
                cmd.stdout(Stdio::from(f.try_clone()?));
                cmd.stderr(Stdio::from(f.try_clone()?));
            }
            None => {
                cmd.stdout(Stdio::null());
                cmd.stderr(Stdio::null());
            }
        }
        let mut child = cmd.spawn().map_err(|e| anyhow!("could not start {}: {e}", exe.display()))?;

        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last;
        loop {
            if let Some(status) = child.try_wait()? {
                let tail = log.as_ref().and_then(|_| std::fs::read_to_string(profile.join("browser.log")).ok()).unwrap_or_default();
                let tail: String = tail.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
                return Err(anyhow!("the browser exited ({status}) before it was ready\n{tail}"));
            }
            match cdp::browser_ws(port) {
                Ok(ws) => {
                    let target = cdp::Target::connect(&ws)?;
                    return Ok(Browser { child, target, exe, profile, headless, persistent });
                }
                Err(e) => last = e.to_string(),
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                return Err(anyhow!("the browser did not start in 20s: {last}"));
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }
}

fn browser_exe() -> Result<PathBuf> {
    if let Some(v) = std::env::var_os("STUDY_BROWSER") {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Ok(p);
        }
        return Err(anyhow!("STUDY_BROWSER points at {}, which is not a file", p.display()));
    }
    let names = [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "brave-browser",
        "brave",
        "microsoft-edge",
        "msedge",
    ];
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        for n in names {
            let p = dir.join(n);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    let fixed = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/snap/bin/chromium",
        "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
        "C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe",
    ];
    for f in fixed {
        let p = PathBuf::from(f);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(anyhow!(
        "no Chrome or Chromium found; install one or set STUDY_BROWSER to its path (STUDY_NO_BROWSER=1 disables the tool)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_and_extensions() {
        assert_eq!(magic_ext(b"\x89PNG\r\n\x1a\n rest"), Some("png"));
        assert_eq!(url_ext("https://x.test/a/b.webp?w=2"), "webp");
        assert_eq!(url_ext("https://x.test/img"), "png");
    }

    /// Drives a real browser end to end. Run with:
    /// `cargo test -p study --no-default-features --bin study drives_a_real_browser -- --ignored`
    #[test]
    #[ignore = "needs Chrome or Chromium"]
    fn drives_a_real_browser() {
        let dir = std::env::temp_dir().join("study-browser-smoke");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let html = dir.join("page.html");
        std::fs::write(
            &html,
            r#"<!doctype html><html><head><title>Probe</title>
<style>body{margin:0;font-family:Inter,sans-serif;background:#f8fafc}
.row{display:flex;gap:20px;padding:24px;background:#ffffff;border-radius:12px;
box-shadow:rgba(0,0,0,0.12) 0px 6px 18px 0px;margin:24px}
.cell{width:100px;height:40px;background:#1f3374;color:#fff}</style></head>
<body><div class="row"><div class="cell" id="one">One</div><div class="cell">Two</div><div class="cell">Three</div></div>
<p id="body">Some words that wrap across more than one line in a narrow column.</p></body></html>"#,
        )
        .unwrap();
        let doc = dir.join("probe.pen");
        let url = format!("file://{}", html.display());
        let mut s = Session::default();

        let r = s
            .call("load-page", &json!({"url": url, "viewport": {"width": 400, "height": 300}}), &doc)
            .expect("load-page");
        match r {
            Reply::Text(t) => assert!(t.contains("Probe"), "{t}"),
            _ => panic!("expected text"),
        }

        let r = s.call("return-element", &json!({"target": "query", "querySelector": ".row"}), &doc).unwrap();
        let Reply::Text(t) = r else { panic!("expected text") };
        assert!(t.contains("\"display\": \"flex\""), "{t}");
        assert!(t.contains("Probe") || t.contains("flex"), "{t}");

        let r = s.call("return-screenshot", &json!({}), &doc).unwrap();
        let Reply::Image(png) = r else { panic!("expected image") };
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(png.len() > 2000);

        let r = s.call("evaluate", &json!({"params": {"expression": "document.title"}}), &doc).unwrap();
        let Reply::Text(t) = r else { panic!("expected text") };
        assert_eq!(t.trim(), "\"Probe\"");

        let r = s.call("import-to-canvas", &json!({}), &doc).unwrap();
        let Reply::Canvas { node, .. } = r else { panic!("expected canvas") };
        assert_eq!(node["type"], "frame");
        let kids = node["children"].as_array().expect("imported children");
        assert!(kids.len() >= 1);
        fn find_layout<'a>(v: &'a Value, want: &str) -> Option<&'a Value> {
            if v["layout"] == want {
                return Some(v);
            }
            for k in v["children"].as_array()? {
                if let Some(hit) = find_layout(k, want) {
                    return Some(hit);
                }
            }
            None
        }
        let row = find_layout(&node, "horizontal").expect("the flex row keeps its layout");
        assert_eq!(row["gap"], 20.0);

        let r = s.call("screenshot-to-canvas", &json!({"target": "query", "querySelector": ".row"}), &doc).unwrap();
        let Reply::Canvas { node, .. } = r else { panic!("expected canvas") };
        assert_eq!(node["type"], "rectangle");
        assert_eq!(node["width"], 352.0);
    }

    #[test]
    fn data_urls_decode() {
        let mut s = Session::default();
        let (bytes, ext) = s.fetch("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(ext, "png");
        assert_eq!(bytes, b"\x89PNG\r\n\x1a\n");
    }
}
