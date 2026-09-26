//! 临时 HTTP 隧道：本地目标检查、真实 readiness、限量诊断输出与受管进程生命周期。
use crate::error::{AppError, Result};
use crate::model::TunnelInfo;
use parking_lot::Mutex;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, OnceLock,
};
use std::time::{Duration, Instant};

const MAX_LOG_LINES: usize = 80;
const MAX_RECORDS: usize = 20;
static SEQ: AtomicU64 = AtomicU64::new(1);
static REGISTRY: OnceLock<TunnelRegistry> = OnceLock::new();
fn registry() -> &'static TunnelRegistry {
    REGISTRY.get_or_init(TunnelRegistry::default)
}

#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub port: u16,
    pub host: Option<String>,
    pub site_id: Option<String>,
}
impl Target {
    pub fn local(port: u16) -> Result<Self> {
        if port == 0 {
            return Err(AppError::new(
                "TUNNEL_BAD_PORT",
                "本地 HTTP 端口必须为 1–65535",
            ));
        }
        Ok(Self {
            port,
            host: None,
            site_id: None,
        })
    }
    pub fn site(site: &crate::model::Site, port: u16) -> Result<Self> {
        let host = site
            .domains
            .first()
            .filter(|s| {
                !s.is_empty()
                    && s.len() <= 253
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
            })
            .ok_or_else(|| AppError::new("TUNNEL_BAD_HOST", "站点没有有效的本地域名"))?;
        let mut target = Self::local(port)?;
        target.host = Some(host.clone());
        target.site_id = Some(site.id.clone());
        Ok(target)
    }
    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn label(&self) -> String {
        format!(
            "http://{}:{}",
            self.host.as_deref().unwrap_or("127.0.0.1"),
            self.port
        )
    }
    fn same_origin(&self, other: &Self) -> bool {
        self.port == other.port && self.host == other.host
    }
}

fn local_client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| AppError::internal("创建隧道检查客户端", e.to_string()))
}

/// 验证端口提供 HTTP；不跟随跳转，站点以真实 Host 请求同一个回环地址。
fn check_origin(target: &Target) -> Result<()> {
    let client = local_client()?;
    let mut request = client.head(target.origin());
    if let Some(host) = &target.host {
        request = request.header(reqwest::header::HOST, host);
    }
    request.send().map_err(|e| {
        AppError::new(
            "TUNNEL_ORIGIN_UNAVAILABLE",
            format!("本地 HTTP 服务无法访问：{}", e.without_url()),
        )
        .with_hint("请先启动对应站点或 HTTP 服务；此入口不支持数据库端口或仅提供 HTTPS 的端口")
    })?;
    Ok(())
}

struct ManagedProcess {
    child: std::process::Child,
    group: platform::ProcessGroup,
}
impl Drop for ManagedProcess {
    fn drop(&mut self) {
        let _ = self.group.terminate(true);
        let _ = self.child.kill();
        // 最后的清理也必须有界，不能因系统拒绝终止而卡住应用退出。
        let deadline = Instant::now() + Duration::from_secs(3);
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(30));
        }
    }
}
struct Entry {
    target: Target,
    info: Mutex<TunnelInfo>,
    metrics: Mutex<Option<String>>,
    process: Mutex<ManagedProcess>,
    // 保留本次独立配置，避免使用用户已有 cloudflared 的账号或 ingress 配置。
    _config: Option<tempfile::NamedTempFile>,
}
impl Entry {
    fn snapshot(&self) -> TunnelInfo {
        self.refresh_process();
        self.info.lock().clone()
    }
    fn refresh_process(&self) {
        let mut process = self.process.lock();
        let exit = process.child.try_wait();
        let mut info = self.info.lock();
        if !info.alive {
            return;
        }
        match exit {
            Ok(Some(status)) => {
                let _ = process.group.terminate(true);
                info.alive = false;
                info.state = "failed".into();
                info.error = Some(format!("cloudflared 已退出（{status}），请查看输出后重试"));
            }
            Ok(None) => {}
            Err(e) => {
                info.state = "failed".into();
                info.error = Some(format!("无法确认隧道进程状态：{e}"));
            }
        }
    }
    fn stop(&self, failure: Option<&str>) -> Result<()> {
        let mut process = self.process.lock();
        if !self.info.lock().alive {
            return Ok(());
        }
        let result = (|| -> Result<()> {
            process.group.terminate(true).map_err(AppError::from)?;
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                match process
                    .child
                    .try_wait()
                    .map_err(|e| AppError::io("确认隧道停止", e))?
                {
                    Some(_) => return Ok(()),
                    None if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(30))
                    }
                    None => {
                        return Err(AppError::new(
                            "TUNNEL_STOP_TIMEOUT",
                            "隧道尚未退出，请再次停止或查看输出",
                        ))
                    }
                }
            }
        })();
        let mut info = self.info.lock();
        match result {
            Ok(()) => {
                info.alive = false;
                info.state = if failure.is_some() {
                    "failed"
                } else {
                    "stopped"
                }
                .into();
                info.error = failure.map(str::to_owned);
                Ok(())
            }
            Err(error) => {
                info.state = "failed".into();
                info.error = Some(error.message.clone());
                Err(error)
            }
        }
    }
}

#[derive(Default)]
struct TunnelRegistry {
    entries: Mutex<Vec<Arc<Entry>>>,
    start_gate: Mutex<()>,
    closing: AtomicBool,
}
impl TunnelRegistry {
    fn list(&self) -> Vec<TunnelInfo> {
        self.entries
            .lock()
            .clone()
            .iter()
            .map(|e| e.snapshot())
            .collect()
    }
    fn find(&self, id: &str) -> Result<Arc<Entry>> {
        self.entries
            .lock()
            .iter()
            .find(|e| e.info.lock().id == id)
            .cloned()
            .ok_or_else(|| AppError::new("TUNNEL_NOT_FOUND", "隧道记录不存在，请刷新列表"))
    }
    fn remove(&self, id: &str) -> Result<()> {
        let mut entries = self.entries.lock();
        let index = entries
            .iter()
            .position(|e| e.info.lock().id == id)
            .ok_or_else(|| AppError::new("TUNNEL_NOT_FOUND", "隧道记录不存在，请刷新列表"))?;
        if entries[index].snapshot().alive {
            return Err(AppError::new("TUNNEL_RUNNING", "请先停止隧道，再移除记录"));
        }
        entries.remove(index);
        Ok(())
    }
    fn start(
        &self,
        target: Target,
        command: impl FnOnce() -> Result<(std::process::Command, Option<tempfile::NamedTempFile>)>,
        timing: Timing,
    ) -> Result<TunnelInfo> {
        let _operation = self.start_gate.lock();
        if self.closing.load(Ordering::Acquire) {
            return Err(AppError::new(
                "TUNNEL_SHUTDOWN",
                "应用正在退出，无法创建隧道",
            ));
        }
        for entry in self.entries.lock().iter() {
            if entry.target.same_origin(&target) {
                let info = entry.snapshot();
                if info.alive {
                    return Ok(info);
                }
            }
        }
        {
            let mut entries = self.entries.lock();
            if entries.len() >= MAX_RECORDS {
                if let Some(index) = entries.iter().position(|e| !e.snapshot().alive) {
                    entries.remove(index);
                } else {
                    return Err(AppError::new(
                        "TUNNEL_LIMIT",
                        "最多同时运行 20 条隧道，请先停止不需要的隧道",
                    ));
                }
            }
        }
        check_origin(&target)?;
        let (mut command, config) = command()?;
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                command.pre_exec(platform::spawn_pre_exec);
            }
        }
        let mut group = platform::ProcessGroup::new()?;
        let mut child = command
            .spawn()
            .map_err(|e| AppError::io("启动 cloudflared", e))?;
        if let Err(e) = group.attach(child.id()) {
            drop(ManagedProcess { child, group });
            return Err(e.into());
        }
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let info = TunnelInfo {
            id: format!("tunnel-{}", SEQ.fetch_add(1, Ordering::Relaxed)),
            port: target.port,
            url: None,
            started_at: crate::services::now_ms(),
            alive: true,
            state: "starting".into(),
            target: target.label(),
            site_id: target.site_id.clone(),
            error: None,
            logs: Vec::new(),
            local_reachable: Some(true),
        };
        let entry = Arc::new(Entry {
            target,
            info: Mutex::new(info),
            metrics: Mutex::new(None),
            process: Mutex::new(ManagedProcess { child, group }),
            _config: config,
        });
        // 输出或监测线程创建失败时也保留记录，停止失败仍可再次操作。
        self.entries.lock().push(entry.clone());
        if let Some(pipe) = stdout {
            spawn_reader(pipe, entry.clone())?;
        }
        if let Some(pipe) = stderr {
            spawn_reader(pipe, entry.clone())?;
        }
        let watched = entry.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("tunnel-health".into())
            .spawn(move || monitor(watched, timing))
        {
            let _ = entry.stop(Some("无法启动隧道状态监测"));
            return Err(AppError::io("监测隧道", e));
        }
        Ok(entry.snapshot())
    }
    fn shutdown(&self) {
        let _operation = self.start_gate.lock();
        self.closing.store(true, Ordering::Release);
        for entry in self.entries.lock().iter() {
            let _ = entry.stop(None);
        }
    }
}
#[derive(Clone, Copy)]
struct Timing {
    interval: Duration,
    unavailable: Duration,
}
impl Default for Timing {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(2),
            unavailable: Duration::from_secs(90),
        }
    }
}

fn spawn_reader(pipe: impl Read + Send + 'static, entry: Arc<Entry>) -> Result<()> {
    let watched = entry.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("tunnel-output".into())
        .spawn(move || read_output(pipe, &watched))
    {
        let _ = entry.stop(Some("无法读取隧道输出"));
        return Err(AppError::io("读取隧道输出", e));
    }
    Ok(())
}

fn read_output(mut pipe: impl Read, entry: &Entry) {
    let mut buffer = [0u8; 4096];
    let mut line = Vec::new();
    let mut clipped = false;
    loop {
        let count = match pipe.read(&mut buffer) {
            Ok(0) => {
                if !line.is_empty() {
                    record_line(entry, &line, clipped);
                }
                return;
            }
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                record_line(entry, format!("读取输出失败：{e}").as_bytes(), false);
                return;
            }
        };
        for byte in &buffer[..count] {
            if *byte == b'\n' {
                record_line(entry, &line, clipped);
                line.clear();
                clipped = false;
            } else if line.len() < 8192 {
                line.push(*byte);
            } else {
                clipped = true;
            }
        }
    }
}
fn record_line(entry: &Entry, bytes: &[u8], clipped: bool) {
    let raw = platform::decode_command_output(bytes);
    let message = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_else(|| raw.trim().to_string());
    if let Some(address) = metrics_address(&message) {
        *entry.metrics.lock() = Some(address);
    }
    let mut info = entry.info.lock();
    if info.url.is_none() {
        info.url = extract_tunnel_url(&message);
    }
    if raw.trim().is_empty() {
        return;
    }
    let mut displayed: String = raw.trim().chars().take(2048).collect();
    if clipped || raw.chars().count() > 2048 {
        displayed.push_str(" …（已截断）");
    }
    info.logs.push(displayed);
    if info.logs.len() > MAX_LOG_LINES {
        info.logs.remove(0);
    }
}
fn metrics_address(message: &str) -> Option<String> {
    let address = message
        .split("Starting metrics server on ")
        .nth(1)?
        .split("/metrics")
        .next()?;
    let port = address.strip_prefix("127.0.0.1:")?.parse::<u16>().ok()?;
    (port > 0).then(|| format!("http://127.0.0.1:{port}"))
}
fn extract_tunnel_url(line: &str) -> Option<String> {
    for after in line.split("https://").skip(1) {
        let host: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.')
            .collect();
        let host = host.to_ascii_lowercase();
        if let Some(sub) = host.strip_suffix(".trycloudflare.com") {
            if !sub.is_empty()
                && sub.len() <= 63
                && sub != "api"
                && !sub.contains('.')
                && !sub.starts_with('-')
                && !sub.ends_with('-')
            {
                return Some(format!("https://{host}"));
            }
        }
    }
    None
}
fn readiness(client: &reqwest::blocking::Client, address: &str) -> bool {
    let Ok(response) = client.get(format!("{address}/ready")).send() else {
        return false;
    };
    if response.status() != reqwest::StatusCode::OK {
        return false;
    }
    let mut bytes = Vec::new();
    if response.take(64 * 1024).read_to_end(&mut bytes).is_err() {
        return false;
    }
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| v.get("readyConnections").and_then(|n| n.as_u64()))
        .is_some_and(|n| n > 0)
}
fn monitor(entry: Arc<Entry>, timing: Timing) {
    let client = match local_client() {
        Ok(c) => c,
        Err(e) => {
            let _ = entry.stop(Some(&e.message));
            return;
        }
    };
    let mut last_available = Instant::now();
    let mut was_connected = false;
    loop {
        entry.refresh_process();
        if {
            let info = entry.info.lock();
            !info.alive || info.state == "failed"
        } {
            return;
        }
        let metrics = entry.metrics.lock().clone();
        let ready = metrics
            .as_deref()
            .is_some_and(|url| readiness(&client, url));
        let local = std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], entry.target.port)),
            Duration::from_millis(500),
        )
        .is_ok();
        {
            let mut info = entry.info.lock();
            if !info.alive || info.state == "failed" {
                return;
            }
            info.local_reachable = Some(local);
            if ready && info.url.is_some() {
                info.state = "connected".into();
                info.error = None;
                was_connected = true;
                last_available = Instant::now();
            } else {
                info.state = if was_connected {
                    "reconnecting"
                } else {
                    "starting"
                }
                .into();
            }
        }
        if last_available.elapsed() >= timing.unavailable {
            let message = if was_connected {
                "隧道断线后 90 秒内未恢复，请检查网络和输出后重试"
            } else {
                "90 秒内未建立可用隧道，请检查 cloudflared 输出与网络后重试"
            };
            let _ = entry.stop(Some(message));
            return;
        }
        std::thread::sleep(timing.interval);
    }
}

fn cloudflared_command(
    exe: &Path,
    target: &Target,
) -> Result<(std::process::Command, Option<tempfile::NamedTempFile>)> {
    let mut config = tempfile::Builder::new()
        .prefix("niceenv-tunnel-")
        .suffix(".yml")
        .tempfile()?;
    config.write_all(b"{}\n")?;
    config.flush()?;
    let mut command = platform::command(exe);
    command
        .args(["tunnel", "--no-autoupdate", "--config"])
        .arg(config.path())
        .args([
            "--url",
            &target.origin(),
            "--metrics",
            "127.0.0.1:0",
            "--output",
            "json",
        ]);
    if let Some(host) = &target.host {
        command.args(["--http-host-header", host]);
    }
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("TUNNEL_")
        {
            command.env_remove(name);
        }
    }
    Ok((command, Some(config)))
}
pub fn start(exe: &Path, port: u16) -> Result<TunnelInfo> {
    start_target(exe, Target::local(port)?)
}
pub(crate) fn start_target(exe: &Path, target: Target) -> Result<TunnelInfo> {
    let args = target.clone();
    registry().start(
        target,
        || cloudflared_command(exe, &args),
        Timing::default(),
    )
}
pub fn list() -> Vec<TunnelInfo> {
    registry().list()
}
pub fn stop(id: &str) -> Result<()> {
    registry().find(id)?.stop(None)
}
pub fn remove(id: &str) -> Result<()> {
    registry().remove(id)
}
pub fn shutdown() {
    registry().shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// 有界回环 HTTP 服务，仅用于这个已有模块的回归验证。
    struct HttpFixture {
        port: u16,
        reply: Arc<Mutex<String>>,
        requests: Arc<Mutex<Vec<String>>>,
        delay: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl HttpFixture {
        fn new(status: u16, body: &str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let reply = Arc::new(Mutex::new(Self::response(status, body)));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let delay = Arc::new(AtomicU64::new(0));
            let (r, q, s, d) = (reply.clone(), requests.clone(), stop.clone(), delay.clone());
            let thread = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(15);
                while !s.load(Ordering::Acquire) && Instant::now() < deadline {
                    if let Ok((mut socket, _)) = listener.accept() {
                        // Windows 的 accept socket 继承监听器的 nonblocking 属性。
                        socket.set_nonblocking(false).unwrap();
                        socket
                            .set_read_timeout(Some(Duration::from_millis(100)))
                            .unwrap();
                        socket
                            .set_write_timeout(Some(Duration::from_millis(100)))
                            .unwrap();
                        let mut bytes = Vec::new();
                        let mut buf = [0; 1024];
                        while bytes.len() < 8192 && !bytes.ends_with(b"\r\n\r\n") {
                            match socket.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => bytes.extend_from_slice(&buf[..n]),
                            }
                        }
                        if bytes.is_empty() {
                            continue;
                        }
                        q.lock().push(String::from_utf8_lossy(&bytes).into_owned());
                        let response = r.lock().clone();
                        std::thread::sleep(Duration::from_millis(d.load(Ordering::Acquire)));
                        let _ = socket.write_all(response.as_bytes());
                    } else {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            });
            Self {
                port,
                reply,
                requests,
                stop,
                delay,
                thread: Some(thread),
            }
        }
        fn response(status: u16, body: &str) -> String {
            format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
        }
        fn set(&self, status: u16, body: &str) {
            *self.reply.lock() = Self::response(status, body);
        }
        fn address(&self) -> String {
            format!("http://127.0.0.1:{}", self.port)
        }
    }
    impl Drop for HttpFixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    #[test]
    #[ignore = "bounded child process, launched by tunnel lifecycle checks"]
    fn native_fixture() {
        let Ok(mode) = std::env::var("NSB_TUNNEL_FIXTURE_MODE") else {
            return;
        };
        println!(r#"{{"message":"https://fixture-tunnel.trycloudflare.com"}}"#);
        if let Ok(port) = std::env::var("NSB_TUNNEL_FIXTURE_METRICS") {
            println!(r#"{{"message":"Starting metrics server on 127.0.0.1:{port}/metrics"}}"#);
        }
        std::io::stdout().flush().unwrap();
        if mode == "exit" {
            std::thread::sleep(Duration::from_millis(150));
            eprintln!("fixture process ended with a diagnostic");
        } else {
            std::thread::sleep(Duration::from_secs(12));
        }
    }

    fn command(
        mode: &str,
        metrics: Option<u16>,
    ) -> Result<(std::process::Command, Option<tempfile::NamedTempFile>)> {
        let mut command = platform::command(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "tunnel::tests::native_fixture",
                "--nocapture",
            ])
            .env("NSB_TUNNEL_FIXTURE_MODE", mode);
        if let Some(port) = metrics {
            command.env("NSB_TUNNEL_FIXTURE_METRICS", port.to_string());
        }
        Ok((command, None))
    }
    fn timing() -> Timing {
        Timing {
            interval: Duration::from_millis(20),
            unavailable: Duration::from_secs(3),
        }
    }
    fn wait_for(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate() {
            assert!(Instant::now() < deadline, "condition timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn ignores_request_line_before_url() {
        assert_eq!(
            extract_tunnel_url(
                "2026-09-24T10:00:00Z INF Requesting new quick Tunnel on trycloudflare.com..."
            ),
            None
        );
    }
    #[test]
    fn ignores_api_endpoint_in_errors() {
        assert_eq!(
            extract_tunnel_url("failed to request https://api.trycloudflare.com/tunnel"),
            None
        );
        for host in ["-bad", "bad-", "nested.name", ""] {
            assert_eq!(
                extract_tunnel_url(&format!("https://{host}.trycloudflare.com")),
                None
            );
        }
        assert_eq!(
            extract_tunnel_url("https://valid.trycloudflare.com.evil.invalid"),
            None
        );
    }
    #[test]
    fn extracts_url_from_banner_box() {
        assert_eq!(
            extract_tunnel_url("INF | https://Brave-Otter-Sample-42.trycloudflare.com |")
                .as_deref(),
            Some("https://brave-otter-sample-42.trycloudflare.com")
        );
    }
    #[test]
    fn metrics_address_accepts_only_loopback() {
        assert_eq!(
            metrics_address("Starting metrics server on 127.0.0.1:23456/metrics").as_deref(),
            Some("http://127.0.0.1:23456")
        );
        for addr in [
            "0.0.0.0:1234",
            "evil.invalid:1234",
            "127.0.0.1:0",
            "127.0.0.1:65536",
            "127.0.0.1:2345@evil.invalid",
        ] {
            assert!(
                metrics_address(&format!("Starting metrics server on {addr}/metrics")).is_none()
            );
        }
    }
    #[test]
    fn origin_checks_http_host_and_does_not_follow_redirects() {
        assert!(Target::local(0).is_err());
        let server = HttpFixture::new(302, "");
        *server.reply.lock() = "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/unreachable\r\nContent-Length: 0\r\n\r\n".into();
        let mut target = Target::local(server.port).unwrap();
        target.host = Some("local-app.test".into());
        check_origin(&target).unwrap();
        let request = server.requests.lock()[0].to_ascii_lowercase();
        assert!(request.starts_with("head / http/1.1\r\n"));
        assert!(request.contains("\r\nhost: local-app.test\r\n"));
        server.set(405, "");
        check_origin(&target).unwrap();
        *server.reply.lock() = "not an HTTP service\r\n".into();
        assert!(check_origin(&target).is_err());
        let port = server.port;
        drop(server);
        assert!(check_origin(&Target::local(port).unwrap()).is_err());
    }
    #[test]
    fn readiness_requires_success_and_active_connections() {
        let server = HttpFixture::new(200, r#"{"readyConnections":1}"#);
        let client = local_client().unwrap();
        assert!(readiness(&client, &server.address()));
        for (status, body) in [
            (503, r#"{"readyConnections":1}"#),
            (200, r#"{"readyConnections":0}"#),
            (200, "{}"),
            (200, "invalid"),
            (200, r#"{"readyConnections":"1"}"#),
        ] {
            server.set(status, body);
            assert!(!readiness(&client, &server.address()));
        }
        server.set(200, &"x".repeat(70 * 1024));
        assert!(!readiness(&client, &server.address()));
    }
    #[test]
    fn read_output_takes_real_url_and_drains_the_rest() {
        let origin = HttpFixture::new(200, "");
        let registry = TunnelRegistry::default();
        let info = registry
            .start(
                Target::local(origin.port).unwrap(),
                || command("hold", None),
                timing(),
            )
            .unwrap();
        let entry = registry.find(&info.id).unwrap();
        wait_for(|| entry.info.lock().url.is_some());
        entry.stop(None).unwrap();
        entry.info.lock().url = None;
        let mut bytes = b"Requesting new quick Tunnel on trycloudflare.com\nhttps://first-url.trycloudflare.com\nhttps://second-url.trycloudflare.com\n\xff\xfe garbage\n".to_vec();
        bytes.extend(vec![b'x'; 100_000]);
        bytes.extend_from_slice(b"\nRegistered tunnel connection\n");
        let mut cursor = std::io::Cursor::new(bytes);
        read_output(&mut cursor, &entry);
        assert_eq!(cursor.position(), cursor.get_ref().len() as u64);
        let info = entry.info.lock();
        assert_eq!(
            info.url.as_deref(),
            Some("https://first-url.trycloudflare.com")
        );
        assert!(info.logs.iter().any(|line| line.contains("已截断")));
        assert_eq!(info.logs.last().unwrap(), "Registered tunnel connection");
        drop(info);
        read_output(std::io::Cursor::new("line\n".repeat(200)), &entry);
        assert_eq!(entry.info.lock().logs.len(), MAX_LOG_LINES);
    }
    #[test]
    fn native_connection_recovery_stop_and_remove() {
        let origin = HttpFixture::new(200, "");
        let ready = HttpFixture::new(200, r#"{"readyConnections":1}"#);
        let registry = TunnelRegistry::default();
        let info = registry
            .start(
                Target::local(origin.port).unwrap(),
                || command("hold", Some(ready.port)),
                timing(),
            )
            .unwrap();
        let entry = registry.find(&info.id).unwrap();
        let pid = entry.process.lock().child.id();
        wait_for(|| entry.snapshot().state == "connected");
        assert!(registry.remove(&info.id).is_err());
        ready.set(503, "{}");
        wait_for(|| entry.snapshot().state == "reconnecting");
        ready.set(200, r#"{"readyConnections":1}"#);
        wait_for(|| entry.snapshot().state == "connected");
        drop(origin);
        wait_for(|| entry.snapshot().local_reachable == Some(false));
        assert_eq!(entry.snapshot().state, "connected");
        // 延迟返回的 readiness 不得将已停止状态改回 connected。
        ready.delay.store(200, Ordering::Release);
        let count = ready.requests.lock().len();
        wait_for(|| ready.requests.lock().len() > count);
        entry.stop(None).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(entry.snapshot().state, "stopped");
        assert!(!platform::process_alive(pid));
        entry.stop(Some("late timeout")).unwrap();
        assert_eq!(entry.snapshot().state, "stopped");
        registry.remove(&info.id).unwrap();
        assert!(registry.list().is_empty());
        assert!(registry.remove(&info.id).is_err());
    }
    #[test]
    fn native_exit_and_url_only_timeout_keep_diagnostics() {
        let origin = HttpFixture::new(200, "");
        let registry = TunnelRegistry::default();
        let info = registry
            .start(
                Target::local(origin.port).unwrap(),
                || command("exit", None),
                timing(),
            )
            .unwrap();
        let entry = registry.find(&info.id).unwrap();
        wait_for(|| !entry.snapshot().alive);
        wait_for(|| {
            entry
                .snapshot()
                .logs
                .iter()
                .any(|line| line.contains("fixture process ended"))
        });
        assert_eq!(registry.list()[0].state, "failed");
        assert!(registry.list()[0].error.is_some());
        let info = registry
            .start(
                Target::local(origin.port).unwrap(),
                || command("hold", None),
                Timing {
                    unavailable: Duration::from_millis(500),
                    ..timing()
                },
            )
            .unwrap();
        let entry = registry.find(&info.id).unwrap();
        let pid = entry.process.lock().child.id();
        wait_for(|| !entry.snapshot().alive);
        assert!(entry.snapshot().url.is_some());
        assert_eq!(entry.snapshot().state, "failed");
        assert!(entry.snapshot().error.unwrap().contains("未建立"));
        assert!(!platform::process_alive(pid));
        assert_eq!(registry.list().len(), 2);
    }
    #[test]
    fn concurrent_start_deduplicates_and_shutdown_rejects_new_work() {
        let origin = HttpFixture::new(200, "");
        let registry = Arc::new(TunnelRegistry::default());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let registry = registry.clone();
                let barrier = barrier.clone();
                let port = origin.port;
                std::thread::spawn(move || {
                    barrier.wait();
                    registry
                        .start(
                            Target::local(port).unwrap(),
                            || command("hold", None),
                            timing(),
                        )
                        .unwrap()
                })
            })
            .collect();
        let infos: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(infos[0].id, infos[1].id);
        assert_eq!(registry.list().len(), 1);
        let mut other = Target::local(origin.port).unwrap();
        other.host = Some("other.test".into());
        registry
            .start(other, || command("hold", None), timing())
            .unwrap();
        assert_eq!(registry.list().len(), 2);
        let pids: Vec<_> = registry
            .entries
            .lock()
            .iter()
            .map(|e| e.process.lock().child.id())
            .collect();
        registry.shutdown();
        assert!(pids.into_iter().all(|pid| !platform::process_alive(pid)));
        assert!(registry.list().iter().all(|entry| entry.state == "stopped"));
        assert!(registry
            .start(
                Target::local(origin.port).unwrap(),
                || command("hold", None),
                timing()
            )
            .is_err());
    }
    #[test]
    fn command_isolates_config_and_forwards_site_host() {
        let target = Target {
            port: 8180,
            host: Some("site.test".into()),
            site_id: Some("site-1".into()),
        };
        let (command, file) = cloudflared_command(Path::new("cloudflared"), &target).unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args
            .windows(2)
            .any(|w| w == ["--url", "http://127.0.0.1:8180"]));
        assert!(args
            .windows(2)
            .any(|w| w == ["--http-host-header", "site.test"]));
        assert!(args.windows(2).any(|w| w == ["--metrics", "127.0.0.1:0"]));
        assert!(args.windows(2).any(|w| w == ["--output", "json"]));
        assert_eq!(
            std::fs::read_to_string(file.unwrap().path()).unwrap(),
            "{}\n"
        );
    }

    #[test]
    #[ignore = "requires the SHA256-verified official cloudflared executable"]
    fn official_cloudflared_accepts_arguments() {
        let exe = std::env::var_os("NSB_CLOUDFLARED_VERIFY").expect("official executable required");
        let target = Target {
            port: 8180,
            host: Some("site.test".into()),
            site_id: None,
        };
        let (mut command, _config) = cloudflared_command(Path::new(&exe), &target).unwrap();
        // --help 只验证 CLI 参数，不建立公网连接。此版本未知 flag 也可能退出 0。
        let output = command.arg("--help").output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{text}");
        assert!(
            !text.contains("Incorrect Usage") && !text.contains("flag provided but not defined"),
            "{text}"
        );
        assert!(
            text.contains("--http-host-header") && text.contains("--output"),
            "{text}"
        );
    }
}
