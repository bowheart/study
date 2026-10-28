//! Builds an MCP request stream: `mkreq <filePath> <snippet.js>...` → JSON lines on stdout.
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    println!("{}", serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}));
    for (i, f) in a[1..].iter().enumerate() {
        let code = std::fs::read_to_string(f).unwrap();
        println!("{}", serde_json::json!({"jsonrpc": "2.0", "id": i + 2, "method": "tools/call",
            "params": {"name": "execute", "arguments": {"filePath": a[0], "input": code}}}));
    }
}
