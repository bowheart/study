//! `study mcp`: the stdio MCP server. Tool calls go to the study window that has
//! the document open (launching one if needed), or run headless in-process.

use crate::engine::Engine;
use crate::ipc::{self, Instance};
use crate::{docs, tools};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const INSTRUCTIONS: &str = "This MCP server drives study, a local infinite-canvas viewer for .pen design files (the pen.dev format). \
Use it to read, generate, and validate designs. Call get_app_state first: it reports the open document, the user's selection, \
and includes the execute API reference. Make all document changes through the execute tool so the canvas updates live and \
the user can undo them; avoid editing .pen files on disk directly while study has them open.";

pub fn tool_list() -> Value {
    json!([
        {
            "name": "get_app_state",
            "description": "Get info about the current state of the study app: open documents, the user's current selection, top-level frames, and the documentation needed to use the execute tool.",
            "inputSchema": {"type": "object", "properties": {}, "$schema": "http://json-schema.org/draft-07/schema#"}
        },
        {
            "name": "execute",
            "description": "Use get_app_state if you don't have execute documentation",
            "inputSchema": {
                "type": "object",
                "$schema": "http://json-schema.org/draft-07/schema#",
                "properties": {
                    "filePath": {"type": "string", "description": "An optional file path to access a .pen file."},
                    "input": {"type": "string", "description": "The JavaScript snippet to execute. Required unless `edits` is provided."},
                    "editId": {"type": "string", "description": "The id of the failed snippet to patch, as printed in that call's failure message. Send it only together with `edits`, and always use a valid editId that is referencing a failed execute call. Never send it alongside `input`."},
                    "edits": {
                        "type": "array",
                        "description": "When an execute call fails, ALWAYS retry with this instead of resending the snippet in `input`. Each edit replaces `find` with `replace` in the failed snippet, then the patched snippet re-runs from scratch. Edits apply in order: each `find` must match the snippet as already modified by the preceding edits. Requires `editId`; omit `input` when using it. If the patched snippet fails again, keep fixing it with further `edits` under the same editId - `find` must then match the already-patched snippet.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "find": {"type": "string", "description": "Exact text in the failed snippet to replace. Must match exactly once unless `all` is true."},
                                "replace": {"type": "string", "description": "The replacement text."},
                                "all": {"type": "boolean", "description": "Replace every occurrence instead of requiring a unique match."}
                            },
                            "required": ["find", "replace"]
                        }
                    }
                },
                "required": ["filePath"]
            }
        },
        {
            "name": "read_skill",
            "description": "Read the study skill that teaches how to design on the canvas.\n\nUsage:\n1. read_skill(): returns the skill's SKILL.md\n2. read_skill({ path }): read a file referenced from SKILL.md (e.g. \"execute.md\", \"pen-schema.md\")",
            "inputSchema": {
                "type": "object",
                "$schema": "http://json-schema.org/draft-07/schema#",
                "properties": {"path": {"type": "string", "description": "Relative path of a file referenced from SKILL.md. Omit to read SKILL.md itself."}}
            }
        },
        {
            "name": "get_style",
            "description": "Load visual style archetypes for working with .pen files. Styles provide configurable fonts, colors, and imagery. Styles do not save variables, they only provide reference values.\n\nUsage:\n1. get_style(): list available styles\n2. get_style({ name }): load style with no params or get required params when required\n3. get_style({ name, params }): load style",
            "inputSchema": {
                "type": "object",
                "$schema": "http://json-schema.org/draft-07/schema#",
                "properties": {
                    "name": {"type": "string", "description": "Style name from the listing"},
                    "params": {"type": "object", "additionalProperties": {}, "description": "Key-value pairs for required params returned in step 2"}
                }
            }
        }
    ])
}

struct Server {
    headless: HashMap<String, Engine>,
}

pub fn run() {
    let mut server = Server { headless: HashMap::new() };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg = match serde_json::from_str::<Value>(&line) {
            Ok(m) => m,
            Err(e) => {
                let reply = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}});
                let _ = writeln!(stdout, "{reply}");
                let _ = stdout.flush();
                continue;
            }
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result = match method {
            "initialize" => Some(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "study", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            })),
            "ping" => Some(json!({})),
            "tools/list" => Some(json!({"tools": tool_list()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Some(server.call(&name, &args))
            }
            "resources/list" => Some(json!({"resources": []})),
            "prompts/list" => Some(json!({"prompts": []})),
            _ => None,
        };
        let Some(id) = id else { continue };
        let reply = match result {
            Some(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            None => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {method}")}}),
        };
        let _ = writeln!(stdout, "{reply}");
        let _ = stdout.flush();
    }
}

impl Server {
    fn call(&mut self, name: &str, args: &Value) -> Value {
        match name {
            "read_skill" => text(docs::read(args.get("path").and_then(Value::as_str))),
            "get_style" => text(docs::style(args.get("name").and_then(Value::as_str), args.get("params"))),
            "get_app_state" => self.app_state(),
            "execute" => {
                let file = args.get("filePath").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(PathBuf::from);
                match self.route(file.as_deref()) {
                    Route::Window(inst) => ipc::send(&inst, "execute", args).unwrap_or_else(|e| ipc::tool_error(&format!("study window error: {e}"))),
                    Route::Headless(key) => {
                        let engine = self.headless.get_mut(&key).unwrap();
                        engine.reload_if_changed();
                        tools::execute(engine, args)
                    }
                    Route::Error(e) => ipc::tool_error(&e),
                }
            }
            "browser" => ipc::tool_error("The browser tool is not available in study."),
            _ => ipc::tool_error(&format!("unknown tool {name}")),
        }
    }

    fn app_state(&mut self) -> Value {
        let windows = ipc::instances();
        let mut out = String::new();
        if let Some(first) = windows.first() {
            match ipc::send(first, "get_app_state", &json!({})) {
                Ok(v) => out.push_str(v["content"][0]["text"].as_str().unwrap_or("")),
                Err(e) => out.push_str(&format!("(could not reach the study window: {e})\n")),
            }
            if windows.len() > 1 {
                out.push_str("\nOther open study windows:\n");
                for w in &windows[1..] {
                    out.push_str(&format!("- {}\n", w.file.as_ref().map(|f| f.display().to_string()).unwrap_or_else(|| "(no document)".into())));
                }
            }
        } else if !self.headless.is_empty() {
            out.push_str("No study window is open. Documents open headlessly in this session:\n");
            for e in self.headless.values() {
                out.push_str(&format!("- {}\n", e.path().map(|p| p.display().to_string()).unwrap_or_default()));
            }
        } else {
            out.push_str("No study window is open and no document is loaded. Pass a .pen file path as `filePath` to execute; study opens it in a window.\n");
        }
        out.push_str("\n");
        out.push_str(&docs::app_state_docs());
        text(out)
    }

    fn route(&mut self, file: Option<&Path>) -> Route {
        let windows = ipc::instances();
        let Some(file) = file else {
            return match windows.into_iter().find(|w| w.file.is_some()) {
                Some(w) => Route::Window(w),
                None => match self.headless.keys().next() {
                    Some(k) => Route::Headless(k.clone()),
                    None => Route::Error("No document is open. Pass the .pen file path as `filePath`.".into()),
                },
            };
        };
        let file = absolute(file);
        let key = ipc::norm(&file);
        if let Some(w) = windows.iter().find(|w| w.file.as_ref().is_some_and(|f| ipc::norm(f) == key)) {
            return Route::Window(w.clone());
        }
        // Open it in a new study window, unless windows are disabled.
        if std::env::var_os("STUDY_HEADLESS").is_none() {
            if let Some(w) = launch_window(&file, &key) {
                self.headless.remove(&key);
                return Route::Window(w);
            }
        }
        if !self.headless.contains_key(&key) {
            match Engine::open(&file) {
                Ok(e) => {
                    self.headless.insert(key.clone(), e);
                }
                Err(e) => return Route::Error(format!("Could not open {}: {e:#}", file.display())),
            }
        }
        Route::Headless(key)
    }
}

enum Route {
    Window(Instance),
    Headless(String),
    Error(String),
}

fn text(s: String) -> Value {
    json!({"content": [{"type": "text", "text": s}], "isError": false})
}

fn absolute(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Starts a study window for `file` and waits for it to register.
fn launch_window(file: &Path, key: &str) -> Option<Instance> {
    let exe = std::env::current_exe().ok()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg(file);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(DETACHED_PROCESS);
    }
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.spawn().ok()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(w) = ipc::instances().into_iter().find(|w| w.file.as_ref().is_some_and(|f| ipc::norm(f) == key)) {
            return Some(w);
        }
    }
    None
}
