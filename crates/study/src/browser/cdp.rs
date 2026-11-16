//! A tiny Chrome DevTools Protocol client: the HTTP discovery endpoint plus one flattened
//! browser session over a WebSocket. Everything the `browser` tool does goes through here.

use anyhow::{anyhow, Result};
use base64::Engine as _;
use serde_json::{json, Value};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

pub type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

pub struct Target {
    ws: Socket,
    session: String,
    next: u64,
    events: Vec<Value>,
}

impl Target {
    /// Connects to the browser endpoint, opens a page and attaches to it (flattened).
    pub fn connect(ws_url: &str) -> Result<Target> {
        let mut ws = connect(ws_url)?;
        let target = round_trip(&mut ws, None, 1, "Target.createTarget", json!({"url": "about:blank"}))?;
        let target_id = target["targetId"].as_str().ok_or_else(|| anyhow!("the browser opened no page"))?.to_string();
        let attached = round_trip(
            &mut ws,
            None,
            2,
            "Target.attachToTarget",
            json!({"targetId": target_id, "flatten": true}),
        )?;
        let session = attached["sessionId"].as_str().ok_or_else(|| anyhow!("could not attach to the page"))?.to_string();
        let mut t = Target { ws, session, next: 3, events: Vec::new() };
        t.send("Page.enable", json!({}))?;
        t.send("Runtime.enable", json!({}))?;
        Ok(t)
    }

    pub fn send(&mut self, method: &str, params: Value) -> Result<Value> {
        let session = self.session.clone();
        self.request(Some(&session), method, params)
    }

    pub fn send_browser(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request(None, method, params)
    }

    fn request(&mut self, session: Option<&str>, method: &str, params: Value) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.ws.send(Message::Text(msg.to_string().into()))?;
        loop {
            let v = read(&mut self.ws)?;
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = v.get("error") {
                    let text = err.get("message").and_then(Value::as_str).unwrap_or("CDP error");
                    return Err(anyhow!("{method}: {text}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            if v.get("method").is_some() {
                if self.events.len() > 256 {
                    self.events.remove(0);
                }
                self.events.push(v);
            }
        }
    }

    /// Evaluates an expression and returns its JSON value (awaiting promises).
    pub fn evaluate(&mut self, expression: &str) -> Result<Value> {
        let r = self.send(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true, "awaitPromise": true, "userGesture": true}),
        )?;
        if let Some(details) = r.get("exceptionDetails") {
            let text = details
                .get("exception")
                .and_then(|e| e.get("description").or_else(|| e.get("value")))
                .and_then(Value::as_str)
                .or_else(|| details.get("text").and_then(Value::as_str))
                .unwrap_or("script error");
            return Err(anyhow!(text.to_string()));
        }
        Ok(r.get("result").and_then(|r| r.get("value")).cloned().unwrap_or(Value::Null))
    }

    pub fn navigate(&mut self, url: &str) -> Result<()> {
        // A fresh document gets a fresh loader id. Without waiting for it, the ready-state poll
        // below can see the previous document still "complete" and return before the new page
        // has committed.
        let before = self.loader_id()?;
        let r = self.send("Page.navigate", json!({"url": url}))?;
        if let Some(err) = r.get("errorText").and_then(Value::as_str) {
            return Err(anyhow!("{url}: {err}"));
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.loader_id()?.as_deref() != before.as_deref() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(anyhow!("{url}: the navigation did not start"))
    }

    fn loader_id(&mut self) -> Result<Option<String>> {
        let tree = self.send("Page.getFrameTree", json!({}))?;
        Ok(tree["frameTree"]["frame"]["loaderId"].as_str().map(str::to_string))
    }

    /// Waits until the document is fully loaded. Redirects are followed by the browser, so the
    /// ready-state poll below is what catches them; events are not required.
    pub fn wait_ready(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let state = self.evaluate("document.readyState").unwrap_or(Value::Null);
            if state == json!("complete") {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(anyhow!("the page did not finish loading in {}s", timeout.as_secs()))
    }

    pub fn wait_selector(&mut self, selector: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let js = format!("!!document.querySelector({})", json!(selector));
        while Instant::now() < deadline {
            if self.evaluate(&js)? == json!(true) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(anyhow!("no element matched {selector} in {}s", timeout.as_secs()))
    }

    /// The document size in CSS pixels.
    pub fn page_size(&mut self) -> Result<(f64, f64)> {
        let v = self.evaluate(
            "({w: Math.max(document.documentElement.scrollWidth, innerWidth), \
              h: Math.max(document.documentElement.scrollHeight, innerHeight)})",
        )?;
        Ok((v["w"].as_f64().unwrap_or(0.0), v["h"].as_f64().unwrap_or(0.0)))
    }

    /// A PNG of the viewport, the whole page, or a rectangle in page coordinates.
    ///
    /// `captureBeyondViewport` renders an empty surface on current headless Chromium, so the
    /// viewport is grown to the requested size instead (and restored afterwards). Very large
    /// pages are clamped rather than captured in slices.
    pub fn screenshot(&mut self, rect: Option<(f64, f64, f64, f64)>) -> Result<Vec<u8>> {
        const MAX_W: f64 = 4096.0;
        const MAX_H: f64 = 12_000.0;
        let (x, y, w, h) = match rect {
            Some(r) => r,
            None => {
                let (w, h) = self.page_size()?;
                (0.0, 0.0, w, h)
            }
        };
        let (w, h) = (w.min(MAX_W).max(1.0), h.min(MAX_H).max(1.0));
        let (vw, vh) = self.viewport()?;
        let (sx, sy) = self.scroll()?;
        let grow = w > vw + 0.5 || h > vh + 0.5;
        if grow {
            self.set_metrics(w, h)?;
        }
        let _ = self.evaluate(&format!("window.scrollTo({}, {})", x.max(0.0) as i64, y.max(0.0) as i64));
        let (nx, ny) = self.scroll()?;
        let clip = json!({"x": (x - nx).max(0.0), "y": (y - ny).max(0.0), "width": w, "height": h, "scale": 1});
        let r = self.send("Page.captureScreenshot", json!({"format": "png", "clip": clip}));
        if grow {
            self.set_metrics(vw, vh)?;
        }
        let _ = self.evaluate(&format!("window.scrollTo({}, {})", sx.max(0.0) as i64, sy.max(0.0) as i64));
        let r = r?;
        let data = r["data"].as_str().ok_or_else(|| anyhow!("the browser returned no image"))?;
        base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| anyhow!("bad image data: {e}"))
    }

    fn viewport(&mut self) -> Result<(f64, f64)> {
        let v = self.evaluate("({w: innerWidth, h: innerHeight})")?;
        Ok((v["w"].as_f64().unwrap_or(1440.0), v["h"].as_f64().unwrap_or(900.0)))
    }

    fn scroll(&mut self) -> Result<(f64, f64)> {
        let v = self.evaluate("({x: scrollX, y: scrollY})")?;
        Ok((v["x"].as_f64().unwrap_or(0.0), v["y"].as_f64().unwrap_or(0.0)))
    }

    fn set_metrics(&mut self, w: f64, h: f64) -> Result<()> {
        self.send(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": w.round().max(1.0) as u64, "height": h.round().max(1.0) as u64, "deviceScaleFactor": 1, "mobile": false}),
        )?;
        Ok(())
    }

    /// The box of an element in page coordinates, for clipping screenshots and imports.
    pub fn element_rect(&mut self, selector: &str) -> Result<(f64, f64, f64, f64)> {
        let js = format!(
            "(() => {{ const e = document.querySelector({sel}); if (!e) return null; \
              const r = e.getBoundingClientRect(); \
              return {{x: r.left + scrollX, y: r.top + scrollY, w: r.width, h: r.height}}; }})()",
            sel = json!(selector)
        );
        let v = self.evaluate(&js)?;
        if v.is_null() {
            return Err(anyhow!("no element matched {selector}"));
        }
        Ok((
            v["x"].as_f64().unwrap_or(0.0),
            v["y"].as_f64().unwrap_or(0.0),
            v["w"].as_f64().unwrap_or(0.0),
            v["h"].as_f64().unwrap_or(0.0),
        ))
    }

    /// Runs a CDP method, sending browser-level methods without a session id.
    pub fn passthrough(&mut self, method: &str, params: Value) -> Result<Value> {
        const PAGE: &[&str] = &["DOM.", "Input.", "Page.", "Runtime.", "Emulation.", "CSS.", "Accessibility.", "Network.", "Log.", "Overlay."];
        if PAGE.iter().any(|p| method.starts_with(p)) {
            self.send(method, params)
        } else if method.starts_with("Target.") || method.starts_with("Browser.") {
            self.send_browser(method, params)
        } else {
            Err(anyhow!("{method} is not allowed; use DOM, Input, Page, Runtime, Emulation, CSS, Accessibility, Network, Log or Overlay"))
        }
    }
}

fn connect(ws_url: &str) -> Result<Socket> {
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(256 << 20))
        .max_frame_size(Some(64 << 20));
    let (ws, _) = tungstenite::client::connect_with_config(ws_url, Some(config), 3)?;
    if let MaybeTlsStream::Plain(tcp) = ws.get_ref() {
        tcp.set_read_timeout(Some(Duration::from_secs(120)))?;
        tcp.set_write_timeout(Some(Duration::from_secs(30)))?;
    }
    Ok(ws)
}

fn round_trip(ws: &mut Socket, session: Option<&str>, id: u64, method: &str, params: Value) -> Result<Value> {
    let mut msg = json!({"id": id, "method": method, "params": params});
    if let Some(s) = session {
        msg["sessionId"] = json!(s);
    }
    ws.send(Message::Text(msg.to_string().into()))?;
    loop {
        let v = read(ws)?;
        if v.get("id").and_then(Value::as_u64) == Some(id) {
            if let Some(err) = v.get("error") {
                return Err(anyhow!("{method}: {}", err.get("message").and_then(Value::as_str).unwrap_or("error")));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

fn read(ws: &mut Socket) -> Result<Value> {
    loop {
        match ws.read()? {
            Message::Text(t) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    return Ok(v);
                }
            }
            Message::Binary(b) => {
                if let Ok(v) = serde_json::from_slice::<Value>(&b) {
                    return Ok(v);
                }
            }
            Message::Ping(p) => ws.send(Message::Pong(p))?,
            Message::Pong(_) | Message::Frame(_) => {}
            Message::Close(_) => return Err(anyhow!("the browser closed the connection")),
        }
    }
}

/// `http://127.0.0.1:port/json/version` → the browser's WebSocket URL, when it is up.
pub fn browser_ws(port: u16) -> Result<String> {
    let url = format!("http://127.0.0.1:{port}/json/version");
    let mut r = ureq::get(&url).call().map_err(|e| anyhow!("could not reach the browser on port {port}: {e}"))?;
    let body = r.body_mut().with_config().limit(1 << 20).read_to_string()?;
    let v: Value = serde_json::from_str(&body)?;
    v["webSocketDebuggerUrl"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("the browser reported no DevTools URL"))
}

/// A free loopback port for `--remote-debugging-port`.
pub fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}
