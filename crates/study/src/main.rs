//! study — a local canvas for .pen design files, driven by Claude over MCP.
//!
//! `study [file.pen]`  open the canvas window
//! `study mcp`         run the MCP server on stdio (configure this in Claude Code)

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod api;
mod docs;
mod engine;
mod export;
mod gui;
mod html;
mod ipc;
mod mcp;
mod shot;
mod scripts;
mod svg;
mod tools;
mod uitext;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mcp") => {
            attach_console();
            mcp::run();
        }
        Some("ipc") => {
            // Development aid: send one request to the most recently focused window.
            let tool = args.get(1).cloned().unwrap_or_default();
            let a: serde_json::Value = args.get(2).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(serde_json::json!({}));
            let Some(inst) = ipc::instances().into_iter().next() else { eprintln!("no study window"); std::process::exit(1) };
            let r = ipc::send(&inst, &tool, &a).unwrap();
            for (i, c) in r["content"].as_array().into_iter().flatten().enumerate() {
                match c["type"].as_str() {
                    Some("image") => {
                        use base64::Engine as _;
                        let bytes = base64::engine::general_purpose::STANDARD.decode(c["data"].as_str().unwrap_or("")).unwrap_or_default();
                        let f = std::env::temp_dir().join(format!("study-ipc-{i}.png"));
                        std::fs::write(&f, bytes).unwrap();
                        println!("[image] {}", f.display());
                    }
                    _ => println!("{}", c["text"].as_str().unwrap_or("")),
                }
            }
        }
        Some("--help" | "-h") => {
            attach_console();
            println!("study [file.pen]   open the canvas\nstudy mcp          run the MCP server on stdio");
        }
        _ => {
            let file = args.iter().find(|a| !a.starts_with('-')).map(std::path::PathBuf::from);
            if let Err(e) = gui::run(file) {
                eprintln!("study: {e:#}");
                std::process::exit(1);
            }
        }
    }
}

/// Release builds use the GUI subsystem (no console window); the MCP server talks
/// over inherited stdio handles, which work without a console.
fn attach_console() {}
