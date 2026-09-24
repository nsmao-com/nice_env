//! nsb-mcp — NiceEnv 的 MCP 服务器入口。
//! stdio 逐行 JSON-RPC；给 Claude Desktop / Cursor 等 AI 客户端当工具用：
//!   { "mcpServers": { "niceservbay": { "command": "nsb-mcp" } } }

use std::io::{BufRead, Write};

fn main() {
    let emit: nsb_core::EventSink = std::sync::Arc::new(|_| {});
    let state = match nsb_core::CoreState::init(None, emit) {
        Ok(s) => s,
        Err(e) => {
            // MCP 初始化失败只能写 stderr（stdout 是协议通道）
            eprintln!("nsb-mcp: init failed: {}", e.message);
            std::process::exit(1);
        }
    };

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(req) => nsb_core::mcp::handle_request(&state, &req),
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
            let _ = stdout.flush();
        }
    }
}
