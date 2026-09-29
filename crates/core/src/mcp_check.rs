//! 检查随包 MCP 的真实 stdio 会话与当前桌面控制通道；不改客户端配置或服务状态。

use crate::error::{AppError, Result};
use crate::mcp::{PROTOCOL_VERSION, SERVER_NAME, SERVER_VERSION};
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path, process::Stdio, time::{Duration, Instant}};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const CHECK_TIMEOUT: Duration = Duration::from_secs(12);
const MAX_OUTPUT: usize = 1024 * 1024;
static CHECK_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionReport {
    version: String,
    protocol: String,
    tool_count: usize,
    service_count: usize,
    elapsed_ms: u64,
}

fn invalid_response() -> AppError {
    AppError::new("MCP_CHECK_RESPONSE", "MCP 工具返回的内容不完整或格式不兼容")
        .with_hint("请重新安装当前版本的完整 NiceEnv 安装包后重试")
}

fn check_io(error: std::io::Error) -> AppError {
    AppError::io("与 MCP 工具通信失败", error)
        .with_hint("请确认工具可正常运行，重新安装当前版本的完整 NiceEnv 安装包后重试")
}

/// 进程组先于第一条请求就位，nsb-mcp 此时仍在等待输入，尚未启动工具子会话。
/// 完成、超时和 future 取消均回收自己创建的整组，不接触桌面或已安装的服务。
struct CheckProcess {
    child: Child,
    group: Option<platform::ProcessGroup>,
}

impl Drop for CheckProcess {
    fn drop(&mut self) {
        if let Some(mut group) = self.group.take() { let _ = group.terminate(true); }
        let _ = self.child.start_kill();
    }
}

impl CheckProcess {
    async fn finish(&mut self) -> Result<()> {
        if let Some(mut group) = self.group.take() {
            if let Err(error) = group.terminate(true) {
                self.group = Some(group);
                return Err(AppError::new("MCP_CHECK_CLEANUP", "无法结束 MCP 检查进程，请重新打开 NiceEnv 后重试")
                    .with_detail(error.to_string()));
            }
        }
        self.child.start_kill().map_err(check_io)?;
        tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await
            .map_err(|_| AppError::new("MCP_CHECK_CLEANUP", "等待 MCP 检查进程退出超时，请重新打开 NiceEnv 后重试"))?
            .map_err(check_io)?;
        Ok(())
    }
}

struct Session {
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    remaining: usize,
}

impl Session {
    async fn send(&mut self, request: Value) -> Result<()> {
        let mut line = request.to_string();
        line.push('\n');
        self.input.write_all(line.as_bytes()).await.map_err(check_io)?;
        self.input.flush().await.map_err(check_io)
    }

    async fn request(&mut self, id: u8, method: &str, params: Value) -> Result<Value> {
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params})).await?;
        let mut line = Vec::new();
        // take 限制累计输出，即使子进程永不写换行也不能无限分配内存。
        (&mut self.output).take((self.remaining + 1) as u64).read_until(b'\n', &mut line).await.map_err(check_io)?;
        if line.len() > self.remaining {
            return Err(AppError::new("MCP_CHECK_OUTPUT_LIMIT", "MCP 检查结果超过大小限制，已停止检查"));
        }
        self.remaining -= line.len();
        if line.last() != Some(&b'\n') { return Err(invalid_response()); }
        let reply: Value = serde_json::from_slice(&line).map_err(|_| invalid_response())?;
        if reply.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || reply.get("id") != Some(&json!(id))
            || reply.get("error").is_some() {
            return Err(invalid_response());
        }
        reply.get("result").filter(|value| value.is_object()).cloned().ok_or_else(invalid_response)
    }
}

fn validate_initialize(result: &Value) -> Result<()> {
    if result["protocolVersion"] != PROTOCOL_VERSION || result["serverInfo"]["name"] != SERVER_NAME
        || !result["capabilities"]["tools"].is_object() { return Err(invalid_response()); }
    let version = result["serverInfo"]["version"].as_str().ok_or_else(invalid_response)?;
    if version != SERVER_VERSION {
        return Err(AppError::new("MCP_CHECK_VERSION", format!("MCP 工具版本 {version} 与当前应用 {SERVER_VERSION} 不一致"))
            .with_hint("请使用同一安装包中的应用与工具，重新安装后再检查"));
    }
    Ok(())
}

fn validate_tools(result: &Value) -> Result<usize> {
    let tools = result["tools"].as_array().ok_or_else(invalid_response)?;
    let expected = crate::mcp::tool_definitions();
    let expected = expected.as_array().ok_or_else(invalid_response)?;
    let mut names = BTreeSet::new();
    if result.get("nextCursor").is_some() || tools.len() != expected.len() { return Err(invalid_response()); }
    for tool in tools {
        let name = tool["name"].as_str().ok_or_else(invalid_response)?;
        if !names.insert(name) || !expected.iter().any(|item| item["name"] == name && item["inputSchema"] == tool["inputSchema"]) {
            return Err(invalid_response());
        }
    }
    Ok(tools.len())
}

fn service_count(result: &Value) -> Result<usize> {
    let content = result["content"].as_array().ok_or_else(invalid_response)?;
    if content.len() != 1 || content[0]["type"] != "text" { return Err(invalid_response()); }
    let body: Value = serde_json::from_str(content[0]["text"].as_str().ok_or_else(invalid_response)?)
        .map_err(|_| invalid_response())?;
    if result["isError"] == true {
        let error: AppError = serde_json::from_value(body["error"].clone()).map_err(|_| invalid_response())?;
        return Err(AppError::new("MCP_CHECK_CONTROL", format!("无法通过 MCP 读取当前服务状态：{}", error.message))
            .with_hint(error.hint.unwrap_or_else(|| "请等待 NiceEnv 启动完成；若仍失败，请重新打开应用后重试".into())));
    }
    if result["isError"] != false { return Err(invalid_response()); }
    let services = body["services"].as_array().ok_or_else(invalid_response)?;
    if services.iter().any(|row| !row["id"].is_string() || !row["state"].is_string() || !row["pids"].is_array()) {
        return Err(invalid_response());
    }
    Ok(services.len())
}

async fn exchange(child: &mut Child) -> Result<ConnectionReport> {
    let mut session = Session {
        input: child.stdin.take().ok_or_else(invalid_response)?,
        output: BufReader::new(child.stdout.take().ok_or_else(invalid_response)?),
        remaining: MAX_OUTPUT,
    };
    let result = session.request(1, "initialize", json!({"protocolVersion":PROTOCOL_VERSION,
        "capabilities":{}, "clientInfo":{"name":"NiceEnv connection check", "version":SERVER_VERSION}})).await?;
    // 老版本不具备共享控制通道；版本通过后才允许任何工具查询。
    validate_initialize(&result)?;
    session.send(json!({"jsonrpc":"2.0", "method":"notifications/initialized"})).await?;
    let tool_count = validate_tools(&session.request(2, "tools/list", json!({})).await?)?;
    let service_count = service_count(&session.request(3, "tools/call", json!({"name":"list_services", "arguments":{}})).await?)?;
    Ok(ConnectionReport { version: SERVER_VERSION.into(), protocol: PROTOCOL_VERSION.into(), tool_count, service_count, elapsed_ms: 0 })
}

async fn check_command(mut command: Command, timeout: Duration) -> Result<ConnectionReport> {
    let started = Instant::now();
    let group = platform::ProcessGroup::new().map_err(|error| AppError::new("MCP_CHECK_START", "无法准备 MCP 检查进程").with_detail(error.to_string()))?;
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(unix)]
    unsafe { command.pre_exec(platform::spawn_pre_exec); }
    let child = command.spawn().map_err(|error| AppError::new("MCP_CHECK_START", "无法启动 MCP 工具")
        .with_hint("请检查系统是否阻止工具运行，或重新安装当前版本的完整 NiceEnv 安装包")
        .with_detail(error.to_string()))?;
    let mut process = CheckProcess { child, group: Some(group) };
    let pid = process.child.id().ok_or_else(invalid_response)?;
    if let Err(error) = process.group.as_mut().ok_or_else(invalid_response)?.attach(pid) {
        let _ = process.child.kill().await;
        return Err(AppError::new("MCP_CHECK_START", "无法管理 MCP 检查进程，已停止检查").with_detail(error.to_string()));
    }
    let result = tokio::time::timeout(timeout, exchange(&mut process.child)).await
        .unwrap_or_else(|_| Err(AppError::new("MCP_CHECK_TIMEOUT", "MCP 连接检查超时，已停止检查")
            .with_hint("请等待 NiceEnv 启动完成后重试；若持续超时，请重新打开应用")));
    process.finish().await?;
    let mut report = result?;
    report.elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    Ok(report)
}

/// executable 必须由桌面安装位置推导，不接受客户端配置中的任意 command。
pub async fn check_connection(executable: &Path, base: &Path) -> Result<ConnectionReport> {
    let _permit = CHECK_GATE.try_acquire().map_err(|_| AppError::new("MCP_CHECK_BUSY", "MCP 连接检查正在进行，请稍后重试"))?;
    let mut command: Command = platform::command(executable).into();
    command.env("NSB_HOME", base).env("NSB_MCP_CHECK", "1").current_dir(base);
    check_command(command, CHECK_TIMEOUT).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn initialized() -> Value {
        json!({"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{}},
            "serverInfo":{"name":SERVER_NAME,"version":SERVER_VERSION}})
    }

    #[test]
    fn validates_versions_tools_and_read_only_results() {
        let mut value = initialized();
        assert!(validate_initialize(&value).is_ok());
        value["serverInfo"]["version"] = json!("0.2.153");
        assert_eq!(validate_initialize(&value).unwrap_err().code, "MCP_CHECK_VERSION");
        value = initialized(); value["protocolVersion"] = json!("unsupported");
        assert!(validate_initialize(&value).is_err());
        value = json!({"tools":crate::mcp::tool_definitions()});
        assert_eq!(validate_tools(&value).unwrap(), 8);
        value["tools"][1] = value["tools"][0].clone();
        assert!(validate_tools(&value).is_err());
        value = json!({"tools":crate::mcp::tool_definitions(), "nextCursor":"more"});
        assert!(validate_tools(&value).is_err());
        assert_eq!(service_count(&json!({"isError":false,"content":[{"type":"text","text":"{\"services\":[]}"}]})).unwrap(), 0);
        assert_eq!(service_count(&crate::mcp::tool_error(AppError::new("APP_BUSY", "正在迁移"))).unwrap_err().code, "MCP_CHECK_CONTROL");
        assert!(service_count(&json!({"isError":false,"content":[{"type":"text","text":"{}"}]})).is_err());
    }

    #[test]
    fn existing_controller_connection_never_initializes_an_environment() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("not-created");
        assert!(crate::control::Client::connect_existing(Some(base.clone())).is_err());
        assert!(!base.exists());
    }

    #[cfg(windows)]
    fn powershell(script: &str) -> Command {
        let mut command = platform::command("powershell.exe");
        command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
        command.into()
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn rejects_wrong_versions_and_excess_output_before_tool_calls() {
        let root = tempfile::tempdir().unwrap();
        let marker = root.path().join("unexpected-call");
        let mut result = initialized(); result["serverInfo"]["version"] = json!("0.2.153");
        let mut command = powershell("$null=[Console]::ReadLine(); [Console]::WriteLine($env:NICEENV_CHECK_REPLY); while ($null -ne ($line=[Console]::ReadLine())) { if ($line) { [IO.File]::WriteAllText($env:NICEENV_CHECK_MARKER,$line) } }");
        command.env("NICEENV_CHECK_REPLY", json!({"jsonrpc":"2.0","id":1,"result":result}).to_string()).env("NICEENV_CHECK_MARKER", &marker);
        assert_eq!(check_command(command, Duration::from_secs(6)).await.unwrap_err().code, "MCP_CHECK_VERSION");
        assert!(!marker.exists(), "旧版本不得收到工具查询");
        let command = powershell("$null=[Console]::ReadLine(); [Console]::Write(('x' * 1048577)); Start-Sleep -Seconds 30");
        assert_eq!(check_command(command, Duration::from_secs(6)).await.unwrap_err().code, "MCP_CHECK_OUTPUT_LIMIT");
        let command = powershell("$null=[Console]::ReadLine(); [Console]::WriteLine('{}'); Start-Sleep -Seconds 30");
        assert_eq!(check_command(command, Duration::from_secs(6)).await.unwrap_err().code, "MCP_CHECK_RESPONSE");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn timeout_and_cancellation_reap_the_entire_check_process_tree() {
        let root = tempfile::tempdir().unwrap();
        for cancel in [false, true] {
            let marker = root.path().join(if cancel { "cancel" } else { "timeout" });
            let mut command = powershell("$null=[Console]::ReadLine(); $child=Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30' -PassThru; [IO.File]::WriteAllText($env:NICEENV_CHECK_MARKER, \"$PID,$($child.Id)\"); Start-Sleep -Seconds 30");
            command.env("NICEENV_CHECK_MARKER", &marker);
            let task = tokio::spawn(check_command(command, if cancel { Duration::from_secs(10) } else { Duration::from_secs(4) }));
            let deadline = Instant::now() + Duration::from_secs(3);
            while !marker.exists() && Instant::now() < deadline { tokio::time::sleep(Duration::from_millis(20)).await; }
            let pids: Vec<u32> = std::fs::read_to_string(&marker).unwrap().split(',').map(|pid| pid.parse().unwrap()).collect();
            assert_eq!(pids.len(), 2);
            if cancel { task.abort(); assert!(task.await.unwrap_err().is_cancelled()); }
            else { assert_eq!(task.await.unwrap().unwrap_err().code, "MCP_CHECK_TIMEOUT"); }
            let deadline = Instant::now() + Duration::from_secs(2);
            while pids.iter().any(|pid| platform::process_alive(*pid)) && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(pids.iter().all(|pid| !platform::process_alive(*pid)), "检查的父进程和孙进程均应回收");
        }
    }

    /// 原生验收显式传入本次编译的工具；正常 cargo test 不依赖 sidecar 已构建。
    #[tokio::test]
    async fn native_connection_probe() {
        let Some(executable) = std::env::var_os("NICEENV_MCP_CHECK_TOOL") else { return; };
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("data");
        std::fs::create_dir(&base).unwrap();
        let executable = std::path::PathBuf::from(executable);
        assert_eq!(check_connection(&executable, &base).await.unwrap_err().code, "MCP_CHECK_CONTROL");
        assert!(!base.join("nsb.sqlite").exists());
        let lease = crate::control::Lease::acquire(Some(base.clone()), Duration::ZERO).unwrap();
        let state = crate::CoreState::init(Some(base.clone()), std::sync::Arc::new(|_| {})).unwrap();
        state.manager.register("mcp-check-fixture", "fixture", Some("1.2.3".into()), None, None, base.join("fixture.log"));
        let pid = crate::services::spawn_tracked(&state.manager, "mcp-check-fixture", &crate::services::SpawnSpec {
            program: std::env::current_exe().unwrap(),
            args: vec!["--exact".into(), "control::tests::controller_service_probe".into(), "--nocapture".into()],
            cwd: Some(base.clone()), env: vec![("NICEENV_CONTROL_SERVICE_PROBE".into(), "1".into())], detached: Some(false),
        }).unwrap();
        struct Cleanup(std::sync::Arc<crate::CoreState>);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = self.0.stop_service("mcp-check-fixture"); } }
        let _cleanup = Cleanup(state.clone());
        state.manager.set_state("mcp-check-fixture", crate::model::ServiceState::Running);
        let server = crate::control::Server::start(lease, state.clone(), || Ok(())).unwrap();
        let report = check_connection(&executable, &base).await.unwrap();
        assert_eq!(report.tool_count, 8);
        assert_eq!(report.version, SERVER_VERSION);
        assert_eq!(report.service_count, state.service_status_list().len());
        assert!(platform::process_alive(pid), "连接检查不能结束当前控制者管理的服务");
        drop(server);
        assert_eq!(check_connection(&executable, &base).await.unwrap_err().code, "MCP_CHECK_CONTROL");
        assert!(platform::process_alive(pid));
        println!("MCP_CHECK_REPORT={}", serde_json::to_string(&report).unwrap());
    }
}
