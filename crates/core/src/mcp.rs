//! MCP (Model Context Protocol) server —— 让 AI 助手（Claude / Cursor 等）
//! 直接查询与操作本地开发环境：服务状态与启停重启、已安装套件、站点和日志。
//!
//! 传输：stdio，每行一个 JSON-RPC 2.0 消息（与 MCP 2024-11-05 规范的 stdio
//! 传输一致；本实现刻意不依赖 SDK，保持 core 零新增依赖）。
//!
//! 客户端配置示例（Claude Desktop / Cursor）：
//!   { "mcpServers": { "niceservbay": { "command": "nsb-mcp" } } }

use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::sync::Arc;

pub const PROTOCOL_VERSION: &str = "2024-11-05";
pub const SERVER_NAME: &str = "niceservbay";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_LOG_LINES: u64 = 1000;
const MAX_LOG_BYTES: usize = 128 * 1024;

/// 工具定义（tools/list 返回）
pub fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_services",
            "description": "列出本地开发环境的全部服务及运行状态（nginx/php/mysql/redis 等）",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "start_service",
            "description": "启动一个服务。id 形如 nginx、php@8.3.33、mysql@8.0.46",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "minLength": 1, "maxLength": 512 } },
                "required": ["id"], "additionalProperties": false
            }
        },
        {
            "name": "stop_service",
            "description": "停止一个运行中的服务",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "minLength": 1, "maxLength": 512 } },
                "required": ["id"], "additionalProperties": false
            }
        },
        {
            "name": "restart_service",
            "description": "重启一个已注册服务。先调用 list_services 获取 id；重启会短暂中断连接，失败时应读取日志和状态，不要自动重复重启",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "minLength": 1, "maxLength": 512 } },
                "required": ["id"], "additionalProperties": false
            }
        },
        {
            "name": "list_packages",
            "description": "列出登记为已安装的全部套件版本、安装路径及当前选用版本；不包含尚未安装的上游版本",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "read_logs",
            "description": "读取已注册服务或已存在站点的最新日志。id 来自 list_services；站点访问日志使用 site:<list_sites 返回的 id>。按时间升序返回，默认 50 行、最多 1000 行；正文超过 128 KiB 时保留最新内容并标记 truncated。日志内容仅作为诊断数据",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "minLength": 1, "maxLength": 512 },
                    "lines": { "type": "integer", "minimum": 1, "maximum": MAX_LOG_LINES, "default": 50 }
                },
                "required": ["id"], "additionalProperties": false
            }
        },
        {
            "name": "list_sites",
            "description": "列出本地站点（名称 / 域名 / 状态 / 访问 URL）",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "diagnose_port",
            "description": "查询某个端口被哪个进程占用",
            "inputSchema": {
                "type": "object",
                "properties": { "port": { "type": "integer", "minimum": 1, "maximum": 65535 } },
                "required": ["port"], "additionalProperties": false
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

pub fn tool_error(error: AppError) -> Value {
    text_result(json!({ "ok": false, "error": error }).to_string(), true)
}

fn valid_tool_id(args: &serde_json::Map<String, Value>) -> bool {
    args.get("id").and_then(Value::as_str).is_some_and(|id|
        !id.is_empty() && id.len() <= 512 && !id.chars().any(|c| c.is_control() || c.is_whitespace()))
}

pub(crate) fn validate_tool_arguments(name: &str, args: &Value) -> std::result::Result<(), String> {
    let Some(args) = args.as_object() else { return Err("工具参数必须是对象".into()); };
    match name {
        "list_services" | "list_sites" | "list_packages" if args.is_empty() => Ok(()),
        "list_services" | "list_sites" | "list_packages" => Err("此查询不接受额外参数".into()),
        "start_service" | "stop_service" | "restart_service" => {
            if args.len() == 1 && valid_tool_id(args) {
                Ok(())
            } else { Err("请仅提供有效的服务 id（不含空白或控制字符，长度不超过 512）".into()) }
        }
        "read_logs" => {
            if !valid_tool_id(args) || args.keys().any(|key| key != "id" && key != "lines") {
                return Err("请提供有效的日志来源 id，仅支持 id 和 lines 参数".into());
            }
            if args.get("lines").is_some_and(|lines| !lines.as_u64().is_some_and(|n| (1..=MAX_LOG_LINES).contains(&n))) {
                return Err(format!("lines 必须是 1–{MAX_LOG_LINES} 的整数"));
            }
            Ok(())
        }
        "diagnose_port" => {
            if args.len() == 1 && args.get("port").and_then(Value::as_u64).is_some_and(|port| (1..=65535).contains(&port)) {
                Ok(())
            } else { Err("port 必须是 1–65535 的整数，且不能包含额外参数".into()) }
        }
        _ => Err(format!("未知工具 {name}")),
    }
}

/// 保留最新日志和 UTF-8 字符边界；对截断明确标记，不把被省略的内容当成完整日志。
fn bounded_logs(id: &str, requested_lines: usize, logs: Vec<crate::model::LogLine>) -> Value {
    let mut remaining = MAX_LOG_BYTES;
    let mut tail = Vec::new();
    let mut truncated = false;
    for mut entry in logs.into_iter().rev() {
        if remaining == 0 { truncated = true; break; }
        if entry.line.len() > remaining {
            let mut start = entry.line.len() - remaining;
            while !entry.line.is_char_boundary(start) { start += 1; }
            entry.line = entry.line[start..].to_owned();
            truncated = true;
        }
        remaining = remaining.saturating_sub(entry.line.len().saturating_add(1));
        tail.push(entry);
        if truncated { break; }
    }
    tail.reverse();
    json!({ "id": id, "requestedLines": requested_lines, "returnedLines": tail.len(),
        "truncated": truncated, "byteLimit": MAX_LOG_BYTES, "lines": tail })
}

/// 执行一次工具调用（独立函数，便于单测与复用）
pub fn handle_tool_call(state: &Arc<crate::CoreState>, name: &str, args: &Value) -> Value {
    if let Err(message) = validate_tool_arguments(name, args) {
        return tool_error(AppError::new("BAD_TOOL_ARGUMENTS", message));
    }
    let _activity = match crate::paths::DataDirActivity::shared(&state.paths.base) {
        Ok(guard) => guard,
        Err(error) => return tool_error(error),
    };
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
        "start_service" | "stop_service" | "restart_service" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let r = match name {
                "start_service" => state.start_service(id),
                "restart_service" => state.restart_service(id),
                _ => state.stop_service(id),
            };
            match r {
                Ok(()) => Ok(json!({ "ok": true, "id": id })),
                Err(e) => Err(e),
            }
        }
        "list_packages" => state.list_packages().map(|packages| {
            let rows: Vec<Value> = packages.into_iter().filter_map(|package| {
                package.install.map(|installed| json!({
                    "id": installed.id, "version": installed.version,
                    "name": package.manifest.display_name, "category": installed.category,
                    "active": package.active, "installPath": installed.install_path,
                    "installedAt": installed.installed_at,
                }))
            }).collect();
            json!({ "packages": rows })
        }),
        "read_logs" => {
            let id = args["id"].as_str().unwrap_or("");
            let lines = args.get("lines").and_then(Value::as_u64).unwrap_or(50) as usize;
            state.tail_logs_checked(id, lines).map(|logs| bounded_logs(id, lines, logs))
        },
        "list_sites" => match crate::sites::list_with_status(&state.paths, &state.store, &state.manager) {
            Ok(sites) => {
                let rows: Vec<Value> = sites
                    .iter()
                    .map(|s| {
                        json!({
                            "id": s.id,
                            "name": s.name,
                            "domain": s.domains.first(),
                            "https": s.https,
                            "status": s.status,
                            "url": s.access_url,
                        })
                    })
                    .collect();
                Ok(json!({ "sites": rows }))
            }
            Err(e) => Err(e),
        },
        "diagnose_port" => {
            let Some(port) = args.get("port").and_then(Value::as_u64).and_then(|port| u16::try_from(port).ok()).filter(|port| *port > 0) else {
                return tool_error(AppError::new("BAD_TOOL_ARGUMENTS", "port 必须是 1–65535 的整数"));
            };
            match crate::ports::diagnose_port(port) {
                Ok(d) => Ok(json!({
                    "port": d.port,
                    "inUse": d.in_use,
                    "pid": d.pid,
                    "process": d.process_name,
                })),
                Err(e) => Err(e),
            }
        }
        other => return text_result(format!("未知工具 {other}"), true),
    };
    match res {
        Ok(v) => text_result(v.to_string(), false),
        Err(e) => tool_error(e),
    }
}

/// 处理一条 JSON-RPC 请求。通知类消息（无 id）返回 None。
pub fn handle_request(state: &Arc<crate::CoreState>, req: &Value) -> Option<Value> {
    handle_request_with(req, |name, args| handle_tool_call(state, name, args))
}

/// 先验证协议与参数再访问环境；通知、握手和错误请求不会触发初始化或服务操作。
pub fn handle_request_with(req: &Value, mut execute: impl FnMut(&str, &Value) -> Value) -> Option<Value> {
    let valid_id = req.get("id").filter(|id| id.is_string() || id.is_i64() || id.is_u64());
    let invalid = || rpc_error(Value::Null, -32600, "Invalid Request");
    if !req.is_object() || req.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(invalid());
    }
    let Some(method) = req.get("method").and_then(Value::as_str).filter(|method| !method.is_empty()) else {
        // 本服务未发出反向请求；忽略响应，避免双方错误响应循环。
        if valid_id.is_some() && (req.get("result").is_some() || req.get("error").is_some()) { return None; }
        return Some(invalid());
    };
    if req.get("id").is_some() && valid_id.is_none() { return Some(invalid()); }
    let id = valid_id?.clone();
    let respond = |result: Value| Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    let bad_params = |message: &str| Some(rpc_error(id.clone(), -32602, message));
    if req.get("params").is_some_and(|params| !params.is_object()) { return bad_params("params 必须是对象"); }
    match method {
        "initialize" => {
            if !req.pointer("/params/protocolVersion").and_then(Value::as_str).is_some_and(|v| !v.is_empty())
                || !req.pointer("/params/capabilities").is_some_and(Value::is_object)
                || !["name", "version"].iter().all(|key| req.pointer(&format!("/params/clientInfo/{key}")).and_then(Value::as_str).is_some_and(|v| !v.is_empty())) {
                return bad_params("initialize 需要 protocolVersion、capabilities 和 clientInfo 的 name/version");
            }
            respond(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
            }))
        },
        "ping" => respond(json!({})),
        "tools/list" => {
            if req.pointer("/params/cursor").is_some() { return bad_params("工具列表没有分页游标，请不带 cursor 重新读取"); }
            respond(json!({ "tools": tool_definitions() }))
        },
        "tools/call" => {
            let name = req
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = req
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));
            if let Err(message) = validate_tool_arguments(name, &args) { return bad_params(&message); }
            respond(execute(name, &args))
        }
        other => {
            Some(rpc_error(id, -32601, &format!("method not found: {other}")))
        }
    }
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (tempfile::TempDir, Arc<crate::CoreState>) {
        let base = tempfile::tempdir().unwrap();
        let state = crate::CoreState::init(Some(base.path().to_path_buf()), Arc::new(|_| {})).unwrap();
        (base, state)
    }

    #[test]
    fn initialize_returns_protocol_and_capabilities() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": { "name": "client", "version": "1.0" }
        } });
        let resp = handle_request_with(&req, |_, _| panic!("握手不得初始化环境")).unwrap();
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], SERVER_NAME);
    }

    #[test]
    fn notification_returns_none() {
        let req = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle_request_with(&req, |_, _| panic!("通知不得执行工具")).is_none());
    }

    #[test]
    fn tools_list_includes_diagnostic_and_control_tools() {
        let req = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let resp = handle_request_with(&req, |_, _| panic!("工具列表不得初始化环境")).unwrap();
        let tools = resp["result"]["tools"].as_array().unwrap();
        let mut names: Vec<_> = tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["diagnose_port", "list_packages", "list_services", "list_sites", "read_logs", "restart_service", "start_service", "stop_service"]);
        for t in tools {
            assert!(t["name"].is_string() && t["inputSchema"].is_object());
        }
    }

    #[test]
    fn tool_call_list_services_returns_content() {
        let (_base, st) = state();
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
        let req = json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "nope", "arguments": {} }
        });
        let resp = handle_request_with(&req, |_, _| panic!("未知工具不得执行")).unwrap();
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[test]
    fn unknown_method_gives_json_rpc_error() {
        let req = json!({ "jsonrpc": "2.0", "id": 5, "method": "x/y" });
        let resp = handle_request_with(&req, |_, _| panic!("未知方法不得执行工具")).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn tool_notifications_never_execute_operations() {
        for name in ["start_service", "stop_service", "restart_service", "read_logs", "list_packages", "list_services"] {
            let req = json!({ "jsonrpc": "2.0", "method": "tools/call", "params": {
                "name": name, "arguments": { "id": "nginx" }
            } });
            assert!(handle_request_with(&req, |_, _| panic!("无 id 的调用不得执行")).is_none());
        }
    }

    #[test]
    fn invalid_envelopes_never_execute_operations() {
        let mut requests = vec![Value::Null, json!([]), json!([{"jsonrpc":"2.0","id":1,"method":"ping"}]), json!({}),
            json!({"jsonrpc":"1.0","id":1,"method":"ping"}), json!({"jsonrpc":"2.0","id":1,"method":8})];
        for id in [Value::Null, json!(false), json!({}), json!([]), json!(1.5)] {
            requests.push(json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"start_service","arguments":{"id":"nginx"}}}));
        }
        for req in requests {
            let resp = handle_request_with(&req, |_, _| panic!("非法请求不得执行")).unwrap();
            assert_eq!(resp["error"]["code"], -32600, "{req}");
            assert!(resp["id"].is_null());
        }
    }

    #[test]
    fn invalid_tool_arguments_are_protocol_errors() {
        let mut cases = Vec::new();
        for port in [json!(0), json!(-1), json!(65536), json!(u64::MAX), json!(80.5), json!("80"), Value::Null] {
            cases.push(("diagnose_port", json!({"port":port})));
        }
        for id in [json!(""), json!("nginx extra"), json!("nginx\n"), json!("x".repeat(513)), Value::Null, json!(5)] {
            for name in ["start_service", "stop_service", "restart_service", "read_logs"] { cases.push((name, json!({"id":id}))); }
        }
        for lines in [json!(0), json!(-1), json!(1001), json!(u64::MAX), json!(3.5), json!("50"), Value::Null] {
            cases.push(("read_logs", json!({"id":"nginx","lines":lines})));
        }
        cases.extend([
            ("read_logs", json!({"id":"nginx","path":"/tmp/secret"})), ("list_packages", json!({"extra":true})),
            ("diagnose_port", json!({"port":80,"extra":true})), ("start_service", json!({"id":"nginx","extra":true})),
            ("list_services", json!({"extra":true})), ("list_sites", Value::Null), ("list_sites", json!([])), ("start_service", json!({})),
        ]);
        for (name, args) in cases {
            let req = json!({"jsonrpc":"2.0","id":"bad-args","method":"tools/call","params":{"name":name,"arguments":args}});
            let resp = handle_request_with(&req, |_, _| panic!("非法参数不得执行")).unwrap();
            assert_eq!(resp["error"]["code"], -32602, "{req}");
            assert_eq!(resp["id"], "bad-args");
        }
    }

    #[test]
    fn valid_calls_preserve_ids_and_execute_once() {
        for id in [json!("request-7"), json!(0), json!(-3), json!(u64::MAX)] {
            for port in [1, 65535] {
                let req = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"diagnose_port","arguments":{"port":port}}});
                let mut calls = 0;
                let resp = handle_request_with(&req, |name, args| {
                    calls += 1;
                    assert_eq!(name, "diagnose_port");
                    assert_eq!(args["port"], port);
                    text_result("done".into(), false)
                }).unwrap();
                assert_eq!(calls, 1);
                assert_eq!(resp["id"], id);
                assert_eq!(resp["result"]["isError"], false);
            }
        }
    }

    #[test]
    fn control_messages_do_not_initialize_environment() {
        let ping = json!({"jsonrpc":"2.0","id":1,"method":"ping"});
        assert_eq!(handle_request_with(&ping, |_, _| panic!("ping 不得初始化")).unwrap()["result"], json!({}));
        for req in [json!({"jsonrpc":"2.0","id":1,"result":{}}), json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no"}})] {
            assert!(handle_request_with(&req, |_, _| panic!("响应不得初始化")).is_none());
        }
        for req in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"cursor":"bad"}}),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":[]}),
        ] {
            assert_eq!(handle_request_with(&req, |_, _| panic!("非法参数不得初始化")).unwrap()["error"]["code"], -32602);
        }
    }

    #[test]
    fn operation_errors_set_is_error_and_keep_details() {
        let (_base, st) = state();
        for name in ["start_service", "stop_service", "restart_service", "read_logs"] {
            let result = handle_tool_call(&st, name, &json!({"id":"missing-service"}));
            assert_eq!(result["isError"], true);
            let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(body["ok"], false);
            assert_eq!(body["error"]["code"], "UNKNOWN_SERVICE");
            assert!(body["error"]["message"].as_str().unwrap().contains("missing-service"));
        }
        let result = tool_error(AppError::not_installed("nginx"));
        let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(body["error"]["hint"].as_str().unwrap().contains("nginx"));
    }

    #[test]
    fn installed_packages_keep_all_recorded_versions_and_active_choice() {
        let (base, st) = state();
        for version in ["1.2.3", "2.0.0"] {
            st.store.upsert_installed(&crate::model::InstalledPackage {
                id: "mcp-legacy-package".into(), version: version.into(), category: "runtime".into(),
                install_path: base.path().join(version).to_string_lossy().into(),
                config_path: String::new(), installed_at: 123,
            }).unwrap();
        }
        st.store.set_setting("activemcp-legacy-packageVersion", "1.2.3").unwrap();
        let result = handle_tool_call(&st, "list_packages", &json!({}));
        assert_eq!(result["isError"], false);
        let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        let rows = body["packages"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "未安装的内置版本不能混入已安装结果");
        for row in rows {
            assert_eq!(row["id"], "mcp-legacy-package");
            assert_eq!(row["active"], row["version"] == "1.2.3");
            assert!(row["installPath"].as_str().unwrap().contains(row["version"].as_str().unwrap()));
            assert_eq!(row["installedAt"], 123);
        }
    }

    #[test]
    fn logs_read_real_tail_and_propagate_missing_source_and_io_errors() {
        let (base, st) = state();
        let logfile = base.path().join("service.log");
        std::fs::write(&logfile, (0..75).map(|n| format!("entry {n}\n")).collect::<String>()).unwrap();
        st.manager.register("mcp-logs", "Logs", None, None, None, logfile.clone());
        for (args, expected, first) in [(json!({"id":"mcp-logs"}), 50, "entry 25"), (json!({"id":"mcp-logs","lines":1}), 1, "entry 74")] {
            let result = handle_tool_call(&st, "read_logs", &args);
            assert_eq!(result["isError"], false);
            let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(body["requestedLines"], expected);
            assert_eq!(body["returnedLines"], expected);
            assert_eq!(body["truncated"], false);
            assert_eq!(body["lines"][0]["line"], first);
            assert_eq!(body["lines"].as_array().unwrap().last().unwrap()["line"], "entry 74");
        }
        for (id, code) in [("../../private", "UNKNOWN_SERVICE"), ("site:../private", "BAD_SITE_ID")] {
            let result = handle_tool_call(&st, "read_logs", &json!({"id":id}));
            assert_eq!(result["isError"], true);
            let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(body["error"]["code"], code);
        }
        std::fs::remove_file(&logfile).unwrap();
        let empty = handle_tool_call(&st, "read_logs", &json!({"id":"mcp-logs"}));
        assert_eq!(empty["isError"], false);
        let body: Value = serde_json::from_str(empty["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["lines"], json!([]));
        std::fs::create_dir(&logfile).unwrap();
        assert_eq!(handle_tool_call(&st, "read_logs", &json!({"id":"mcp-logs"}))["isError"], true);
    }

    #[test]
    fn log_byte_limit_keeps_latest_unicode_content_and_reports_truncation() {
        let line = |text: String| crate::model::LogLine { ts: None, line: text };
        let long = "中文🦀".repeat(MAX_LOG_BYTES);
        let result = bounded_logs("id", 3, vec![line("old".into()), line(long.clone()), line("latest".into())]);
        assert_eq!(result["truncated"], true);
        assert_eq!(result["returnedLines"], 2);
        let partial = result["lines"][0]["line"].as_str().unwrap();
        assert!(!partial.contains('\u{fffd}'));
        assert!(long.ends_with(partial));
        assert!(partial.len() + "latest".len() < MAX_LOG_BYTES);
        assert_eq!(result["lines"][1]["line"], "latest");
        assert!(serde_json::to_vec(&result).unwrap().len() < 1024 * 1024);
        let exact = bounded_logs("id", 1, vec![line("x".repeat(MAX_LOG_BYTES))]);
        assert_eq!(exact["truncated"], false);
        assert_eq!(exact["lines"][0]["line"].as_str().unwrap().len(), MAX_LOG_BYTES);
        let empty = bounded_logs("id", 50, vec![]);
        assert_eq!(empty["truncated"], false);
        assert_eq!(empty["returnedLines"], 0);
    }

    #[test]
    fn inactive_sites_do_not_advertise_an_unconfirmed_url() {
        let (base, st) = state();
        let site: crate::model::Site = serde_json::from_value(json!({
            "id":"demo", "name":"demo", "domains":["demo.test"], "rootDir":base.path(),
            "runtime":{"kind":"static","webServer":"nginx"}, "https":false,
            "rewrite":"none", "db":null, "status":"running", "createdAt":0, "updatedAt":0
        })).unwrap();
        st.store.save_site(&site).unwrap();
        let result = handle_tool_call(&st, "list_sites", &json!({}));
        assert_eq!(result["isError"], false);
        let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["sites"][0]["id"], "demo");
        assert_ne!(body["sites"][0]["status"], "running");
        assert!(body["sites"][0]["url"].is_null());
        let log_dir = st.paths.logs().join("nginx");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(log_dir.join("demo.access.log"), "GET /local-site 200\n").unwrap();
        let result = handle_tool_call(&st, "read_logs", &json!({"id":"site:demo","lines":1}));
        assert_eq!(result["isError"], false);
        let body: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["lines"][0]["line"], "GET /local-site 200");
    }
}
