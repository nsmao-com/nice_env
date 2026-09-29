//! nsb-mcp — NiceEnv 的 MCP 服务器入口。
//! stdio 逐行 JSON-RPC；给 Claude Desktop / Cursor 等 AI 客户端当工具用：
//!   { "mcpServers": { "niceservbay": { "command": "nsb-mcp" } } }

use std::io::{BufRead, Write};

fn main() {
    // 与 CLI 一样，客户端断开后保留已启动服务，供后续会话接管。
    std::env::set_var("NSB_CLI", "1");
    let mut state = None;

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(req) => nsb_core::mcp::handle_request_with(&req, |name, args| {
                let active = if let Some(active) = &state {
                    active
                } else {
                    let emit: nsb_core::EventSink = std::sync::Arc::new(|_| {});
                    match nsb_core::CoreState::init(None, emit) {
                        Ok(initialized) => state.insert(initialized),
                        Err(error) => return nsb_core::mcp::tool_error(error),
                    }
                };
                nsb_core::mcp::handle_tool_call(active, name, args)
            }),
            Err(e) => Some(serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("parse error: {e}") }
            })),
        };
        if let Some(v) = resp {
            let Ok(mut out) = serde_json::to_string(&v) else {
                continue;
            };
            out.push('\n');
            if stdout.write_all(out.as_bytes()).is_err() {
                break; // 客户端断开
            }
            if stdout.flush().is_err() { break; }
        }
    }
}
