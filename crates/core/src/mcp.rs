//! MCP (Model Context Protocol) server —— 让 AI 助手（Claude / Cursor 等）
//! 直接查询与操作本地开发环境：列服务、起停服务、列站点。
//!
//! 传输：stdio，每行一个 JSON-RPC 2.0 消息（与 MCP 2024-11-05 规范的 stdio
//! 传输一致；本实现刻意不依赖 SDK，保持 core 零新增依赖）。
//!
//! 客户端配置示例（Claude Desktop / Cursor）：
//!   { "mcpServers": { "niceservbay": { "command": "nsb-mcp" } } }

use crate::error::Result;
use serde_json::{json, Value};
use std::sync::Arc;

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "niceservbay";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 工具定义（tools/list 返回）
pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_services",
            "description": "列出本地开发环境的全部服务及运行状态（nginx/php/mysql/redis 等）",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "start_service",
            "description": "启动一个服务。id 形如 nginx、php@8.3.33、mysql@8.0.46",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        },
        {
            "name": "stop_service",
            "description": "停止一个运行中的服务",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string" } },
                "required": ["id"]
            }
        },
        {
            "name": "list_sites",
            "description": "列出本地站点（名称 / 域名 / 状态 / 访问 URL）",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "diagnose_port",
            "description": "查询某个端口被哪个进程占用",
            "inputSchema": {
                "type": "object",
                "properties": { "port": { "type": "integer" } },
                "required": ["port"]
            }
        }
    ])
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// 执行一次工具调用（独立函数，便于单测与复用）
pub fn handle_tool_call(state: &Arc<crate::CoreState>, name: &str, args: &Value) -> Value {
    let res: Result<Value> = match name {
        "list_services" => {
            let list = state.service_status_list();
            let rows: Vec<Value> = list
                .iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "state": format!("{:?}", s.state).to_lowercase(),
                        "port": s.port,
                        "pids": s.pids,
                        "version": s.version,
                    })
                })
                .collect();
            Ok(json!({ "services": rows }))
        }
        "start_service" | "stop_service" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if id.is_empty() {
                return text_result("缺少服务 id".into(), true);
            }
            let r = if name == "start_service" {
                state.start_service(id)
            } else {
                state.stop_service(id)
            };
            match r {
                Ok(()) => Ok(json!({ "ok": true, "id": id })),
                Err(e) => Ok(json!({ "ok": false, "id": id, "error": e.message })),
            }
        }
        "list_sites" => match crate::sites::list(&state.store) {
            Ok(sites) => {
                let rows: Vec<Value> = sites
                    .iter()
                    .map(|s| {
                        let domain = s.domains.first().map(|d| d.as_str()).unwrap_or("localhost");
                        json!({
                            "name": s.name,
                            "domain": domain,
                            "https": s.https,
                            "status": s.status,
                            "url": format!("{}://{}", if s.https { "https" } else { "http" }, domain),
                        })
                    })
                    .collect();
                Ok(json!({ "sites": rows }))
            }
            Err(e) => Ok(json!({ "error": e.message })),
        },
        "diagnose_port" => {
            let port = args.get("port").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
            match crate::ports::diagnose_port(port) {
                Ok(d) => Ok(json!({
                    "port": d.port,
                    "inUse": d.in_use,
                    "pid": d.pid,
                    "process": d.process_name,
                })),
                Err(e) => Ok(json!({ "error": e.message })),
            }
        }
        other => return text_result(format!("未知工具 {other}"), true),
    };
    match res {
        Ok(v) => text_result(serde_json::to_string_pretty(&v).unwrap_or_default(), false),
        Err(e) => text_result(e.message, true),
    }
}

/// 处理一条 JSON-RPC 请求。通知类消息（无 id）返回 None。
pub fn handle_request(state: &Arc<crate::CoreState>, req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(|m| m.as_str())?;
    let id = req.get("id").cloned();
    let respond = |result: Value| -> Option<Value> {
        Some(json!({ "jsonrpc": "2.0", "id": id.clone()?, "result": result }))
    };

    match method {
        "initialize" => respond(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        })),
        "notifications/initialized" => None,
        "ping" => respond(json!({})),
        "tools/list" => respond(json!({ "tools": tool_definitions() })),
        "tools/call" => {
            let name = req
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = req
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));
            let result = handle_tool_call(state, name, &args);
            Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
        }
        other => {
            let id = id?;
            Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {other}") }
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> Arc<crate::CoreState> {
        let base = tempfile::tempdir().unwrap();
        crate::CoreState::init(Some(base.path().to_path_buf()), Arc::new(|_| {})).unwrap()
    }

    #[test]
    fn initialize_returns_protocol_and_capabilities() {
        let st = state();
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
        let resp = handle_request(&st, &req).unwrap();
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn notification_returns_none() {
        let st = state();
        let req = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle_request(&st, &req).is_none());
    }

    #[test]
    fn tools_list_has_five_tools() {
        let st = state();
        let req = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let resp = handle_request(&st, &req).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 5);
        for t in tools {
            assert!(t["name"].is_string() && t["inputSchema"].is_object());
        }
    }

    #[test]
    fn tool_call_list_services_returns_content() {
        let st = state();
        let req = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "list_services", "arguments": {} }
        });
        let resp = handle_request(&st, &req).unwrap();
        let content = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(content.contains("services"), "{content}");
        assert_eq!(resp["result"]["isError"], false);
    }

    #[test]
    fn unknown_tool_is_reported_not_panicked() {
        let st = state();
        let req = json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "nope", "arguments": {} }
        });
        let resp = handle_request(&st, &req).unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn unknown_method_gives_json_rpc_error() {
        let st = state();
        let req = json!({ "jsonrpc": "2.0", "id": 5, "method": "x/y" });
        let resp = handle_request(&st, &req).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }
}
