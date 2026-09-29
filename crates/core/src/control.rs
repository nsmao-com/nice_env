//! 桌面与命令行共用一个服务控制者。文件锁决定归属，已发送的操作绝不自动重试。

use crate::{error::{AppError, Result}, paths::{DataDirActivity, Paths}, CoreState};
use hmac::{Hmac, KeyInit, Mac};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::{fs::File, io::{Read, Write}, net::{SocketAddr, TcpListener, TcpStream}, path::{Path, PathBuf},
    sync::{atomic::{AtomicBool, Ordering}, Arc}, time::{Duration, Instant}};

const LOCK_FILE: &str = ".niceenv-control.lock";
const ENDPOINT_FILE: &str = ".niceenv-control.json";
const PROTOCOL: u8 = 1;
const MAX_REQUEST: usize = 64 * 1024;
const MAX_RESPONSE: usize = 16 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status, Sites, Packages,
    Start { id: String }, Stop { id: String }, Restart { id: String }, Kill { id: String },
    StartAll, StopAll,
    SiteUrl { name: String },
    Logs { id: String, lines: usize },
    Mcp { name: String, arguments: Value },
}

impl Request {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Start { id } | Self::Stop { id } | Self::Restart { id } | Self::Kill { id } | Self::Logs { id, .. }
                if id.is_empty() || id.len() > 512 || id.chars().any(|c| c.is_control() || c.is_whitespace()) =>
                Err(AppError::new("BAD_CONTROL_REQUEST", "服务 id 无效")),
            Self::Logs { lines, .. } if *lines == 0 => Err(AppError::new("BAD_CONTROL_REQUEST", "日志行数必须为正整数")),
            Self::SiteUrl { name } if name.trim().is_empty() || name.len() > 1024 || name.chars().any(char::is_control) =>
                Err(AppError::new("BAD_CONTROL_REQUEST", "站点名无效")),
            Self::Mcp { name, arguments } => crate::mcp::validate_tool_arguments(name, arguments)
                .map_err(|message| AppError::new("BAD_CONTROL_REQUEST", message)),
            _ => Ok(()),
        }
    }

    fn execute(&self, state: &Arc<CoreState>) -> Result<Value> {
        self.validate()?;
        let _activity = DataDirActivity::shared(&state.paths.base)?;
        crate::ensure_application_accepts_work()?;
        let encode = |value| serde_json::to_value(value).map_err(|error| AppError::internal("编码控制结果", error.to_string()));
        match self {
            Self::Status => encode(state.service_status_list()),
            Self::Sites => serde_json::to_value(crate::sites::list_with_status(&state.paths, &state.store, &state.manager)?)
                .map_err(|error| AppError::internal("编码站点列表", error.to_string())),
            Self::Packages => serde_json::to_value(state.list_packages()?).map_err(|error| AppError::internal("编码套件列表", error.to_string())),
            Self::Start { id } => { state.start_service(id)?; Ok(Value::Null) },
            Self::Stop { id } => { state.stop_service(id)?; Ok(Value::Null) },
            Self::Restart { id } => { state.restart_service(id)?; Ok(Value::Null) },
            Self::Kill { id } => {
                let preview = state.service_stop_preview(id)?;
                state.force_stop_service(id, &preview.revision)?;
                Ok(Value::Null)
            },
            Self::StartAll | Self::StopAll => {
                let report = if matches!(self, Self::StartAll) {
                    let mut ids: Vec<_> = state.service_status_list().into_iter().map(|status| status.id).collect();
                    ids.sort();
                    state.bulk_start(&ids)?
                } else { state.stop_all_services()? };
                serde_json::to_value(report).map_err(|error| AppError::internal("编码批量结果", error.to_string()))
            },
            Self::SiteUrl { name } => {
                let sites = crate::sites::list(&state.store)?;
                let site = sites.iter().find(|site| &site.name == name)
                    .ok_or_else(|| AppError::new("SITE_NOT_FOUND", format!("找不到站点 {name}")))?;
                Ok(Value::String(crate::sites::access_url(&state.paths, &state.store, &state.manager, &site.id)?))
            },
            Self::Logs { id, lines } => serde_json::to_value(state.tail_logs_checked(id, *lines)?)
                .map_err(|error| AppError::internal("编码服务日志", error.to_string())),
            Self::Mcp { name, arguments } => Ok(crate::mcp::handle_tool_call(state, name, arguments)),
        }
    }
}

fn busy() -> AppError {
    AppError::new("CONTROLLER_BUSY", "另一个应用或工具正在初始化、执行操作或退出")
        .with_hint("请稍后重试；若桌面应用已打开，请确认使用同一安装包中的命令行工具。")
}

/// 独占归属覆盖 CoreState 初始化及整个会话，迁移活动锁仍按每个操作获取。
pub struct Lease { pub base: PathBuf, _file: File }
impl Lease {
    fn try_acquire(base: &Path) -> Result<Option<Self>> {
        std::fs::create_dir_all(base)?;
        let _activity = DataDirActivity::shared(base)?;
        let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(base.join(LOCK_FILE))?;
        match file.try_lock() {
            Ok(()) => {
                let base = std::fs::canonicalize(base)?;
                crate::ops::ensure_no_foreign_controller(&Paths::new(base.clone()))?;
                // 只有持锁者可以清理旧描述文件；过期端口不能被当作新控制通道。
                match std::fs::remove_file(base.join(ENDPOINT_FILE)) {
                    Ok(()) => {},
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                    Err(error) => return Err(AppError::io("清理旧控制通道", error)),
                }
                Ok(Some(Self { base, _file: file }))
            },
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(AppError::io("锁定服务控制权", error)),
        }
    }

    pub fn acquire(base: Option<PathBuf>, timeout: Duration) -> Result<Self> {
        let base = Paths::resolve(base)?;
        let deadline = Instant::now() + timeout;
        loop {
            match Self::try_acquire(&base) {
                Ok(Some(lease)) => return Ok(lease),
                Ok(None) => {},
                // 短会话可能刚释放文件锁、尚未完成进程退出；等待其真正交还进程记录。
                Err(error) if error.code == "CONTROLLER_UNAVAILABLE" && Instant::now() < deadline => {},
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline { return Err(busy()); }
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Endpoint { protocol: u8, address: SocketAddr, token: String, pid: u32, started: String }

enum Backend { Local { state: Arc<CoreState>, _lease: Lease }, Remote(Endpoint) }
pub struct Client { backend: Backend, base: PathBuf }
impl Client {
    pub fn connect(base: Option<PathBuf>) -> Result<Self> {
        let base = Paths::resolve(base)?;
        if let Some(lease) = Lease::try_acquire(&base)? {
            let state = CoreState::init(Some(lease.base.clone()), Arc::new(|_| {}))?;
            return Ok(Self { base, backend: Backend::Local { state, _lease: lease } });
        }
        // 控制者已存在时只连接该会话；连接错误不能退回本地执行同一个动作。
        let file = File::open(base.join(ENDPOINT_FILE)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound { busy() } else { AppError::io("读取控制通道", error) }
        })?;
        let mut bytes = Vec::new();
        file.take((MAX_REQUEST + 1) as u64).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_REQUEST { return Err(busy()); }
        let endpoint: Endpoint = serde_json::from_slice(&bytes).map_err(|_| busy())?;
        if endpoint.protocol != PROTOCOL || endpoint.address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            || endpoint.address.port() == 0 || !valid_nonce(&endpoint.token)
            || platform::process_start_marker(endpoint.pid).as_deref() != Some(endpoint.started.as_str()) {
            return Err(AppError::new("CONTROLLER_UNAVAILABLE", "服务控制通道已变化或版本不兼容，请重新打开应用后重试"));
        }
        Ok(Self { base, backend: Backend::Remote(endpoint) })
    }

    pub fn call<T: DeserializeOwned>(&self, request: Request) -> Result<T> {
        request.validate()?;
        let _activity = DataDirActivity::shared(&self.base)?;
        let value = match &self.backend {
            Backend::Local { state, .. } => request.execute(state)?,
            Backend::Remote(endpoint) => remote_call(endpoint, &request)?,
        };
        serde_json::from_value(value).map_err(|_| AppError::new("CONTROL_RESPONSE_INVALID", "控制者返回的结果格式不兼容，请使用同一版本的应用与工具"))
    }
}

fn valid_nonce(value: &str) -> bool { value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) }
fn nonce() -> String { hex::encode(rand::random::<[u8; 32]>()) }
fn mac(token: &str, role: &str, client_nonce: &str, server_nonce: &str) -> Result<Hmac<sha2_11::Sha256>> {
    let mut mac = Hmac::<sha2_11::Sha256>::new_from_slice(token.as_bytes()).map_err(|_| busy())?;
    for part in ["NiceEnv-control-v1", role, client_nonce, server_nonce] { mac.update(part.as_bytes()); }
    Ok(mac)
}
fn verify(token: &str, role: &str, client_nonce: &str, server_nonce: &str, proof: &str) -> Result<()> {
    let proof = hex::decode(proof).map_err(|_| busy())?;
    mac(token, role, client_nonce, server_nonce)?.verify_slice(&proof).map_err(|_| AppError::new("CONTROL_AUTH_FAILED", "无法核实服务控制通道身份，未执行操作"))
}

fn write_frame(stream: &mut TcpStream, frame: &impl Serialize, limit: usize) -> Result<()> {
    let bytes = serde_json::to_vec(frame).map_err(|error| AppError::internal("编码控制消息", error.to_string()))?;
    if bytes.len() > limit { return Err(AppError::new("CONTROL_MESSAGE_TOO_LARGE", "控制消息过大，请缩小查询范围")); }
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}
fn read_frame<T: DeserializeOwned>(stream: &mut TcpStream, limit: usize) -> Result<T> {
    let deadline = Instant::now() + stream.read_timeout()?.unwrap_or(Duration::from_secs(5));
    let read = |stream: &mut TcpStream, mut bytes: &mut [u8]| -> Result<()> {
        while !bytes.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { return Err(AppError::new("CONTROL_TIMEOUT", "等待控制消息超时")); }
            stream.set_read_timeout(Some(remaining))?;
            let length = stream.read(bytes)?;
            if length == 0 { return Err(AppError::new("CONTROL_DISCONNECTED", "服务控制连接已断开")); }
            bytes = &mut bytes[length..];
        }
        Ok(())
    };
    let mut size = [0; 4];
    read(stream, &mut size)?;
    let length = u32::from_be_bytes(size) as usize;
    if length > limit { return Err(AppError::new("CONTROL_MESSAGE_TOO_LARGE", "控制消息超过大小限制")); }
    let mut bytes = vec![0; length];
    read(stream, &mut bytes)?;
    serde_json::from_slice(&bytes).map_err(|_| AppError::new("BAD_CONTROL_REQUEST", "控制消息格式不正确"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello { protocol: u8, nonce: String }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Challenge { nonce: String, proof: String }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope { proof: String, request: Request }

fn remote_call(endpoint: &Endpoint, request: &Request) -> Result<Value> {
    let mut stream = TcpStream::connect_timeout(&endpoint.address, Duration::from_secs(3))
        .map_err(|error| AppError::io("连接当前应用的服务控制通道", error).with_hint("请确认应用仍在运行后重试。"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let client_nonce = nonce();
    write_frame(&mut stream, &Hello { protocol: PROTOCOL, nonce: client_nonce.clone() }, MAX_REQUEST)?;
    let challenge: Challenge = read_frame(&mut stream, MAX_REQUEST)?;
    if !valid_nonce(&challenge.nonce) { return Err(busy()); }
    verify(&endpoint.token, "server", &client_nonce, &challenge.nonce, &challenge.proof)?;
    let proof = hex::encode(mac(&endpoint.token, "client", &client_nonce, &challenge.nonce)?.finalize().into_bytes());
    // 长服务操作可能超过普通连接超时；一旦开始发送，不再重发或本地执行。
    stream.set_read_timeout(Some(Duration::from_secs(180)))?;
    let outcome = (|| -> Result<Result<Value>> {
        write_frame(&mut stream, &Envelope { proof, request: request.clone() }, MAX_REQUEST)?;
        read_frame(&mut stream, MAX_RESPONSE)
    })();
    outcome.map_err(|error| AppError::new("CONTROL_RESULT_UNKNOWN", "与服务控制者的连接中断，无法确认本次操作结果")
        .with_hint("请先查询服务状态再决定是否重试；本次没有自动重复执行。")
        .with_detail(error.message))?
}

pub struct Server { stopping: Arc<AtomicBool>, thread: Option<std::thread::JoinHandle<()>> }
impl Server {
    pub fn start(lease: Lease, state: Arc<CoreState>, ready: impl Fn() -> Result<()> + Send + 'static) -> Result<Self> {
        if std::fs::canonicalize(&state.paths.base)? != lease.base { return Err(busy()); }
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let endpoint = Endpoint { protocol: PROTOCOL, address: listener.local_addr()?, token: nonce(), pid: std::process::id(),
            started: platform::process_start_marker(std::process::id()).ok_or_else(|| AppError::new("CONTROL_IDENTITY_UNAVAILABLE", "无法确认当前应用进程身份"))? };
        let mut pending = tempfile::NamedTempFile::new_in(&lease.base)?;
        platform::restrict_file_to_owner(pending.path())?;
        pending.write_all(&serde_json::to_vec(&endpoint).map_err(|error| AppError::internal("保存控制通道", error.to_string()))?)?;
        pending.as_file().sync_all()?;
        pending.persist(lease.base.join(ENDPOINT_FILE)).map_err(|error| AppError::io("保存控制通道", error.error))?;
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = stopping.clone();
        let thread = std::thread::Builder::new().name("local-control".into()).spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut serve = || -> Result<()> {
                            // Windows accept 继承 listener 的非阻塞标志；握手与请求使用有界阻塞读取。
                            stream.set_nonblocking(false)?;
                            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                            let hello: Hello = read_frame(&mut stream, MAX_REQUEST)?;
                            if hello.protocol != PROTOCOL || !valid_nonce(&hello.nonce) { return Err(busy()); }
                            let server_nonce = nonce();
                            let proof = hex::encode(mac(&endpoint.token, "server", &hello.nonce, &server_nonce)?.finalize().into_bytes());
                            write_frame(&mut stream, &Challenge { nonce: server_nonce.clone(), proof }, MAX_REQUEST)?;
                            let envelope: Envelope = read_frame(&mut stream, MAX_REQUEST)?;
                            verify(&endpoint.token, "client", &hello.nonce, &server_nonce, &envelope.proof)?;
                            let result = ready().and_then(|_| envelope.request.execute(&state));
                            if let Err(error) = write_frame(&mut stream, &result, MAX_RESPONSE) {
                                if error.code == "CONTROL_MESSAGE_TOO_LARGE" { write_frame(&mut stream, &Err::<Value, _>(error), MAX_RESPONSE)?; }
                            }
                            Ok(())
                        };
                        let _ = serve(); // 未认证/断开连接不能执行操作或污染 MCP 的 stdout。
                    },
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => break,
                }
            }
            let _ = std::fs::remove_file(lease.base.join(ENDPOINT_FILE));
            drop(lease);
        }).map_err(|error| AppError::io("启动本机控制通道", error))?;
        Ok(Self { stopping, thread: Some(thread) })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ServiceState;

    fn owner(base: &Path) -> (Lease, Arc<CoreState>) {
        let lease = Lease::acquire(Some(base.into()), Duration::ZERO).unwrap();
        let state = CoreState::init(Some(base.into()), Arc::new(|_| {})).unwrap();
        (lease, state)
    }

    #[test]
    fn controller_service_probe() {
        if std::env::var("NICEENV_CONTROL_SERVICE_PROBE").as_deref() != Ok("1") { return; }
        std::thread::sleep(Duration::from_secs(30));
    }

    #[test]
    fn controller_native_owner_probe() {
        let Some(root) = std::env::var_os("NICEENV_CONTROL_NATIVE_PROBE") else { return; };
        let base = PathBuf::from(root);
        let (lease, state) = owner(&base);
        state.manager.register("control-native", "native probe", Some("7.8.9".into()), None, None, base.join("probe.log"));
        std::fs::write(base.join("probe.log"), "native-controller-log\n").unwrap();
        crate::services::spawn_tracked(&state.manager, "control-native", &crate::services::SpawnSpec {
            program: std::env::current_exe().unwrap(),
            args: vec!["--exact".into(), "control::tests::controller_service_probe".into(), "--nocapture".into()],
            cwd: Some(base.clone()), env: vec![("NICEENV_CONTROL_SERVICE_PROBE".into(), "1".into())], detached: Some(false),
        }).unwrap();
        struct Cleanup(Arc<CoreState>);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = self.0.stop_service("control-native"); } }
        let _cleanup = Cleanup(state.clone());
        state.manager.set_state("control-native", ServiceState::Running);
        let _server = Server::start(lease, state.clone(), || Ok(())).unwrap();
        std::fs::write(base.join("ready"), "ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(25);
        while !base.join("done").exists() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(20)); }
        assert!(base.join("done").exists(), "原生工具验证未结束");
        assert_eq!(state.manager.snapshot("control-native").unwrap().state, ServiceState::Stopped);
    }

    #[test]
    fn controller_client_probe() {
        let Some(base) = std::env::var_os("NICEENV_CONTROL_CLIENT_PROBE") else { return; };
        let client = Client::connect(Some(base.into())).unwrap();
        let list: Vec<crate::model::ServiceStatus> = client.call(Request::Status).unwrap();
        let status = list.iter().find(|status| status.id == "control-probe").expect("必须读取控制进程中的状态");
        assert_eq!(status.state, ServiceState::Running);
        assert_eq!(status.version.as_deref(), Some("1.2.3"));
        assert_eq!(status.pids.len(), 1);
        let logs: Vec<crate::model::LogLine> = client.call(Request::Logs { id: "control-probe".into(), lines: 50 }).unwrap();
        assert!(logs.iter().any(|line| line.line.contains("controller-log")));
        let logs: Value = client.call(Request::Mcp { name: "read_logs".into(), arguments: serde_json::json!({"id":"control-probe","lines":50}) }).unwrap();
        assert_eq!(logs["isError"], false);
        assert!(logs["content"][0]["text"].as_str().unwrap().contains("controller-log"));
        let result: Value = client.call(Request::Mcp { name: "list_services".into(), arguments: serde_json::json!({}) }).unwrap();
        assert_eq!(result["isError"], false);
        assert!(result["content"][0]["text"].as_str().unwrap().contains("control-probe"));
        client.call::<()>(Request::Start { id: "control-probe".into() }).unwrap();
        client.call::<()>(Request::Stop { id: "control-probe".into() }).unwrap();
    }

    #[test]
    fn controller_shares_live_process_state_with_another_process() {
        let base = tempfile::tempdir().unwrap();
        let (lease, state) = owner(base.path());
        state.manager.register("control-probe", "probe", Some("1.2.3".into()), None, None, base.path().join("probe.log"));
        std::fs::write(base.path().join("probe.log"), "controller-log\n").unwrap();
        let pid = crate::services::spawn_tracked(&state.manager, "control-probe", &crate::services::SpawnSpec {
            program: std::env::current_exe().unwrap(),
            args: vec!["--exact".into(), "control::tests::controller_service_probe".into(), "--nocapture".into()],
            cwd: Some(base.path().into()), env: vec![("NICEENV_CONTROL_SERVICE_PROBE".into(), "1".into())], detached: Some(false),
        }).unwrap();
        struct Cleanup(Arc<CoreState>);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = self.0.stop_service("control-probe"); } }
        let _cleanup = Cleanup(state.clone());
        state.manager.set_state("control-probe", ServiceState::Running);
        crate::ops::save_pidfile_checked(&state.paths, &state.manager).unwrap();
        let server = Server::start(lease, state.clone(), || Ok(())).unwrap();
        let output = platform::command(std::env::current_exe().unwrap())
            .args(["--exact", "control::tests::controller_client_probe", "--nocapture"])
            .env("NICEENV_CONTROL_CLIENT_PROBE", base.path()).env("NSB_SKIP_HOSTS", "1").output().unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(!platform::process_alive(pid));
        assert_eq!(state.manager.snapshot("control-probe").unwrap().state, ServiceState::Stopped);
        drop(server);
        assert!(!base.path().join(ENDPOINT_FILE).exists());
        assert!(Lease::acquire(Some(base.path().into()), Duration::ZERO).is_ok());
    }

    #[test]
    fn controller_authentication_busy_and_migration_gates_do_not_fall_back() {
        let base = tempfile::tempdir().unwrap();
        let (lease, state) = owner(base.path());
        let paused = Arc::new(AtomicBool::new(false));
        let gate = paused.clone();
        let _server = Server::start(lease, state, move || {
            if gate.load(Ordering::Acquire) { Err(AppError::new("APP_BUSY", "transition")) } else { Ok(()) }
        }).unwrap();
        let endpoint_file = base.path().join(ENDPOINT_FILE);
        let original = std::fs::read(&endpoint_file).unwrap();
        let mut endpoint: Endpoint = serde_json::from_slice(&original).unwrap();
        endpoint.token = nonce();
        std::fs::write(&endpoint_file, serde_json::to_vec(&endpoint).unwrap()).unwrap();
        let client = Client::connect(Some(base.path().into())).unwrap();
        assert_eq!(client.call::<Value>(Request::Stop { id: "missing".into() }).unwrap_err().code, "CONTROL_AUTH_FAILED");
        std::fs::write(&endpoint_file, original).unwrap();
        let client = Client::connect(Some(base.path().into())).unwrap();
        paused.store(true, Ordering::Release);
        assert_eq!(client.call::<Value>(Request::Status).unwrap_err().code, "APP_BUSY");
        paused.store(false, Ordering::Release);
        assert!(client.call::<Value>(Request::Status).is_ok());
        assert_eq!(client.call::<Value>(Request::Stop { id: "missing".into() }).unwrap_err().code, "UNKNOWN_SERVICE");
        let migration = DataDirActivity::exclusive(base.path()).unwrap();
        assert_eq!(client.call::<Value>(Request::Status).unwrap_err().code, "DATA_DIR_BUSY");
        drop(migration);
        assert!(client.call::<Value>(Request::Status).is_ok());
        assert!(serde_json::from_value::<Request>(serde_json::json!({"command":"stop","id":"nginx","extra":true})).is_err());
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(endpoint_file).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn controller_does_not_replay_when_result_connection_is_lost() {
        let base = tempfile::tempdir().unwrap();
        let _lease = Lease::acquire(Some(base.path().into()), Duration::ZERO).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = Endpoint { protocol: PROTOCOL, address: listener.local_addr().unwrap(), token: nonce(),
            pid: std::process::id(), started: platform::process_start_marker(std::process::id()).unwrap() };
        std::fs::write(base.path().join(ENDPOINT_FILE), serde_json::to_vec(&endpoint).unwrap()).unwrap();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let hello: Hello = read_frame(&mut stream, MAX_REQUEST).unwrap();
            let server_nonce = nonce();
            let proof = hex::encode(mac(&endpoint.token, "server", &hello.nonce, &server_nonce).unwrap().finalize().into_bytes());
            write_frame(&mut stream, &Challenge { nonce: server_nonce.clone(), proof }, MAX_REQUEST).unwrap();
            let request: Envelope = read_frame(&mut stream, MAX_REQUEST).unwrap();
            verify(&endpoint.token, "client", &hello.nonce, &server_nonce, &request.proof).unwrap();
            assert!(matches!(request.request, Request::Start { .. }));
            // 已收到操作但丢失回复；调用者必须报告结果未知，不能再本地执行。
        });
        let client = Client::connect(Some(base.path().into())).unwrap();
        assert_eq!(client.call::<Value>(Request::Start { id: "nginx".into() }).unwrap_err().code, "CONTROL_RESULT_UNKNOWN");
        thread.join().unwrap();
        assert!(!base.path().join("nsb.sqlite").exists());
    }

    #[test]
    fn controller_rejects_oversized_frames_and_keeps_serving() {
        let base = tempfile::tempdir().unwrap();
        let (lease, state) = owner(base.path());
        let _server = Server::start(lease, state, || Ok(())).unwrap();
        let endpoint: Endpoint = serde_json::from_slice(&std::fs::read(base.path().join(ENDPOINT_FILE)).unwrap()).unwrap();
        let mut stream = TcpStream::connect(endpoint.address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        stream.write_all(&((MAX_REQUEST + 1) as u32).to_be_bytes()).unwrap();
        let mut byte = [0];
        assert!(matches!(stream.read(&mut byte), Ok(0) | Err(_)));
        assert!(Client::connect(Some(base.path().into())).unwrap().call::<Value>(Request::Status).is_ok());
    }
}
