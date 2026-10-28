//! Local IPC between `study mcp` and running study windows.
//!
//! Each window listens on 127.0.0.1 (random port) and registers itself in
//! `~/.study/instances/<pid>.json` with the port, a random token, and its file.
//! Requests and replies are single JSON lines.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct Request {
    pub tool: String,
    pub args: Value,
    pub reply: Sender<Value>,
}

#[derive(Clone, Debug)]
pub struct Instance {
    #[allow(dead_code)]
    pub pid: u32,
    pub port: u16,
    pub token: String,
    pub file: Option<PathBuf>,
    pub focused_at: u64,
}

fn dir() -> PathBuf {
    render::data_dir().join("instances")
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Normalizes a path for comparisons (case-insensitive on Windows).
pub fn norm(p: &Path) -> String {
    let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = abs.to_string_lossy().trim_start_matches(r"\\?\").to_string();
    if cfg!(windows) {
        s.replace('/', "\\").to_lowercase()
    } else {
        s
    }
}

/// Starts the listener; `deliver` hands each request to the UI thread.
pub fn serve(deliver: impl Fn(Request) + Send + Sync + 'static) -> Result<(u16, String)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let token: String = (0..32).map(|_| fastrand::alphanumeric()).collect();
    let expected = token.clone();
    let deliver = std::sync::Arc::new(deliver);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let expected = expected.clone();
            let deliver = deliver.clone();
            std::thread::spawn(move || {
                let _ = handle(stream, &expected, &*deliver);
            });
        }
    });
    Ok((port, token))
}

fn handle(stream: TcpStream, token: &str, deliver: &dyn Fn(Request)) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let msg: Value = serde_json::from_str(&line)?;
    if msg.get("token").and_then(Value::as_str) != Some(token) {
        return Err(anyhow!("bad token"));
    }
    let (tx, rx) = channel();
    deliver(Request {
        tool: msg.get("tool").and_then(Value::as_str).unwrap_or("").to_string(),
        args: msg.get("args").cloned().unwrap_or(json!({})),
        reply: tx,
    });
    let reply = rx.recv_timeout(Duration::from_secs(600)).unwrap_or_else(|_| tool_error("study window did not answer"));
    let mut w = stream;
    w.write_all(serde_json::to_string(&reply)?.as_bytes())?;
    w.write_all(b"\n")?;
    Ok(())
}

pub fn tool_error(msg: &str) -> Value {
    json!({"content": [{"type": "text", "text": msg}], "isError": true})
}

pub fn register(port: u16, token: &str, file: Option<&Path>, focused: bool) {
    let _ = std::fs::create_dir_all(dir());
    let pid = std::process::id();
    let prev = read_instance(&dir().join(format!("{pid}.json")));
    let focused_at = if focused { now_ms() } else { prev.map(|p| p.focused_at).unwrap_or(0) };
    let v = json!({
        "pid": pid,
        "port": port,
        "token": token,
        "file": file.map(|f| f.to_string_lossy().to_string()),
        "focusedAt": focused_at,
    });
    let _ = std::fs::write(dir().join(format!("{pid}.json")), v.to_string());
}

pub fn unregister() {
    let _ = std::fs::remove_file(dir().join(format!("{}.json", std::process::id())));
}

fn read_instance(p: &Path) -> Option<Instance> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
    Some(Instance {
        pid: v.get("pid")?.as_u64()? as u32,
        port: v.get("port")?.as_u64()? as u16,
        token: v.get("token")?.as_str()?.to_string(),
        file: v.get("file").and_then(Value::as_str).map(PathBuf::from),
        focused_at: v.get("focusedAt").and_then(Value::as_u64).unwrap_or(0),
    })
}

/// Live instances, most recently focused first. Stale registrations are removed.
pub fn instances() -> Vec<Instance> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir()) else { return out };
    for e in entries.flatten() {
        let Some(inst) = read_instance(&e.path()) else { continue };
        if TcpStream::connect_timeout(&([127, 0, 0, 1], inst.port).into(), Duration::from_millis(300)).is_ok() {
            out.push(inst);
        } else {
            let _ = std::fs::remove_file(e.path());
        }
    }
    out.sort_by(|a, b| b.focused_at.cmp(&a.focused_at));
    out
}

pub fn send(inst: &Instance, tool: &str, args: &Value) -> Result<Value> {
    let stream = TcpStream::connect(("127.0.0.1", inst.port))?;
    stream.set_read_timeout(Some(Duration::from_secs(600)))?;
    let mut w = stream.try_clone()?;
    let msg = json!({"token": inst.token, "tool": tool, "args": args});
    w.write_all(msg.to_string().as_bytes())?;
    w.write_all(b"\n")?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}
