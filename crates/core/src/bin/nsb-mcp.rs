//! nsb-mcp — NiceEnv 的 MCP 服务器入口。
//! stdio 逐行 JSON-RPC；给 Claude Desktop / Cursor 等 AI 客户端当工具用：
//!   { "mcpServers": { "niceservbay": { "command": "nsb-mcp" } } }

use std::io::{BufRead, Write};
use nsb_core::{control::{Client, Request}, error::AppError};

fn execute(name: &str, arguments: &serde_json::Value) -> serde_json::Value {
    match Client::connect(None).and_then(|client| client.call(Request::Mcp { name: name.into(), arguments: arguments.clone() })) {
        Ok(result) => result,
        Err(error) => nsb_core::mcp::tool_error(error),
    }
}

/// 没有桌面进程时，单次工具调用由短会话执行。子进程退出后其服务可被桌面接管，
/// 长期保持的 AI stdio 连接不会持有一份过期的服务状态或阻止接管。
fn execute_in_child(name: &str, arguments: &serde_json::Value) -> serde_json::Value {
    let result = (|| -> nsb_core::error::Result<serde_json::Value> {
        let executable = std::env::current_exe()?;
        let mut command = platform::command(executable);
        command.arg("--execute-tool").stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
        let mut child = command.spawn().map_err(|error| AppError::io("创建 MCP 工具会话", error))?;
        let request = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}});
        let sent = match child.stdin.take() {
            Some(mut stdin) => writeln!(stdin, "{request}"),
            None => Err(std::io::Error::other("工具会话没有输入通道")),
        };
        // 即使发送失败也回收子进程；不重新执行结果不明的请求。
        let output = child.wait_with_output().map_err(|error| AppError::io("等待 MCP 工具结果", error))?;
        sent.map_err(|error| AppError::io("发送 MCP 工具请求", error))?;
        if !output.status.success() || output.stdout.len() > 16 * 1024 * 1024 {
            return Err(AppError::new("MCP_TOOL_PROCESS_FAILED", "工具会话未正常返回，请查询服务状态后重试"));
        }
        let response: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| AppError::new("MCP_TOOL_RESPONSE_INVALID", "工具会话返回格式不正确，请查询服务状态后重试"))?;
        response.get("result").cloned().ok_or_else(|| AppError::new("MCP_TOOL_RESPONSE_INVALID", "工具会话没有返回结果"))
    })();
    result.unwrap_or_else(nsb_core::mcp::tool_error)
}

fn main() {
    // 与 CLI 一样，客户端断开后保留已启动服务，供后续会话接管。
    std::env::set_var("NSB_CLI", "1");
    let args: Vec<_> = std::env::args().skip(1).collect();
    let single_call = args.len() == 1 && args[0] == "--execute-tool";
    if !args.is_empty() && !single_call {
        eprintln!("nsb-mcp: 用法 nsb-mcp（通过标准输入输出连接 MCP 客户端）");
        std::process::exit(2);
    }

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(req) => nsb_core::mcp::handle_request_with(&req, if single_call { execute } else { execute_in_child }),
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
        if single_call { break; }
    }
}
