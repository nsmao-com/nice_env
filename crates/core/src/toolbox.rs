//! 工具箱扩展：Ollama 模型管理 + Adminer 数据库管理台（php 内置服务器托管）。

use crate::error::{AppError, Result};
use crate::paths::Paths;
use crate::store::Store;
use serde::Serialize;

/// 解析已安装包的主程序完整路径
pub fn resolve_exe(
    store: &Store,
    paths: &Paths,
    installer: &crate::install::Installer,
    id: &str,
) -> Result<std::path::PathBuf> {
    let inst = store.find_installed(id, None).ok_or_else(|| {
        AppError::not_installed(id).with_hint("先到「套件 / 服务」安装后再使用本功能")
    })?;
    let entry = installer
        .template_for(id)
        .ok_or_else(|| AppError::new("PACKAGE_NOT_FOUND", format!("清单里没有 {id}")))?
        .entry;
    Ok(paths
        .runtime_dir(id, &inst.version)
        .join(crate::install::entry_relative_path(&entry)))
}

/* ================= Ollama 模型管理 ================= */

mod ollama {
    use super::*;
    use parking_lot::Mutex;
    use serde::Deserialize;
    use std::io::Read;
    use std::sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, OnceLock,
    };
    use std::time::Duration;

    const MAX_JSON: usize = 4 * 1024 * 1024;
    const MAX_LINE: usize = 64 * 1024;
    static NEXT_PULL: AtomicU64 = AtomicU64::new(1);
    static DOWNLOAD: OnceLock<Controller> = OnceLock::new();
    fn controller() -> &'static Controller {
        DOWNLOAD.get_or_init(Controller::default)
    }

    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OllamaModelRow {
        pub name: String,
        pub digest: String,
        pub size: u64,
        pub modified: String,
        pub parameters: String,
        pub quantization: String,
    }
    #[derive(Deserialize)]
    struct ModelList {
        models: Vec<WireModel>,
    }
    #[derive(Deserialize)]
    struct WireModel {
        name: String,
        digest: String,
        size: u64,
        modified_at: String,
        #[serde(default)]
        details: ModelDetails,
    }
    #[derive(Default, Deserialize)]
    struct ModelDetails {
        #[serde(default)]
        parameter_size: String,
        #[serde(default)]
        quantization_level: String,
    }
    fn parse_models(bytes: &[u8]) -> Result<Vec<OllamaModelRow>> {
        let list: ModelList = serde_json::from_slice(bytes)
            .map_err(|_| AppError::new("OLLAMA_BAD_RESPONSE", "Ollama 返回的模型列表格式无效"))?;
        Ok(list
            .models
            .into_iter()
            .map(|m| OllamaModelRow {
                name: m.name,
                digest: m.digest,
                size: m.size,
                modified: m.modified_at,
                parameters: m.details.parameter_size,
                quantization: m.details.quantization_level,
            })
            .collect())
    }

    /// 只连接本应用当前托管进程的实际端口，不使用系统 OLLAMA_HOST 或 HTTP 代理。
    #[derive(Clone)]
    struct Endpoint {
        url: String,
        port: u16,
        pids: Vec<u32>,
        manager: Arc<crate::services::ServiceManager>,
    }
    impl Endpoint {
        fn managed(manager: Arc<crate::services::ServiceManager>) -> Result<Self> {
            let snapshot = manager
                .snapshot("ollama")
                .ok_or_else(|| AppError::not_installed("Ollama"))?;
            let port = snapshot.port.filter(|p| *p > 0).ok_or_else(|| {
                AppError::new("OLLAMA_NOT_RUNNING", "请先在套件 / 服务中启动 Ollama")
            })?;
            let endpoint = Self {
                url: format!("http://127.0.0.1:{port}"),
                port,
                pids: snapshot.pids,
                manager,
            };
            endpoint.check()?;
            Ok(endpoint)
        }
        fn check(&self) -> Result<()> {
            let owned = self.manager.snapshot("ollama").is_some_and(|s| {
                s.state == crate::model::ServiceState::Running
                    && s.port == Some(self.port)
                    && !self.pids.is_empty()
                    && s.pids == self.pids
            });
            let owners: Vec<_> = crate::ports::listeners()?
                .into_iter()
                .filter_map(|(port, pid)| (port == self.port).then_some(pid))
                .collect();
            if !owned || owners.is_empty() || owners.iter().any(|pid| !self.pids.contains(pid)) {
                return Err(AppError::new(
                    "OLLAMA_NOT_RUNNING",
                    "Ollama 未运行，或监听端口不属于本应用当前实例",
                )
                .with_hint("请到套件 / 服务启动 Ollama 后重试；不会操作其它应用的模型"));
            }
            Ok(())
        }
        fn models(&self) -> Result<Vec<OllamaModelRow>> {
            self.check()?;
            let response = blocking_client()?
                .get(format!("{}/api/tags", self.url))
                .send()
                .map_err(request_error)?;
            parse_models(&read_response(response)?)
        }
    }
    fn request_error(e: reqwest::Error) -> AppError {
        AppError::new(
            "OLLAMA_REQUEST_FAILED",
            format!("无法完成 Ollama 请求：{}", e.without_url()),
        )
        .with_hint("请确认本应用的 Ollama 服务仍在运行，并检查服务日志")
    }
    fn blocking_client() -> Result<reqwest::blocking::Client> {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(request_error)
    }
    fn http_error(status: reqwest::StatusCode, bytes: &[u8]) -> AppError {
        let detail = serde_json::from_slice::<serde_json::Value>(bytes)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.as_str())
                    .map(|s| s.chars().take(1024).collect::<String>())
            });
        AppError::new(
            "OLLAMA_API_ERROR",
            format!(
                "Ollama 请求失败（HTTP {}）{}",
                status.as_u16(),
                detail.map(|s| format!("：{s}")).unwrap_or_default()
            ),
        )
    }
    fn read_response(response: reqwest::blocking::Response) -> Result<Vec<u8>> {
        let status = response.status();
        let mut bytes = Vec::new();
        response
            .take((MAX_JSON + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_JSON {
            return Err(AppError::new(
                "OLLAMA_RESPONSE_LIMIT",
                "Ollama 响应过大，请检查服务日志",
            ));
        }
        if !status.is_success() {
            return Err(http_error(status, &bytes));
        }
        Ok(bytes)
    }
    fn model_name(raw: &str) -> Result<String> {
        let name = raw.trim();
        let parts: Vec<_> = name.split('/').collect();
        let word = |s: &str| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.as_bytes()[0].is_ascii_alphanumeric()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        };
        let valid = !name.is_empty()
            && name.len() <= 255
            && parts.iter().enumerate().all(|(i, p)| {
                if let Some((left, right)) = p.split_once(':') {
                    word(left)
                        && if i == parts.len() - 1 {
                            word(right)
                        } else {
                            i == 0 && right.parse::<u16>().is_ok_and(|port| port > 0)
                        }
                } else {
                    word(p)
                }
            });
        if !valid {
            return Err(AppError::new(
                "OLLAMA_MODEL_INVALID",
                "模型名格式无效，例如 qwen3:0.6b 或 namespace/model:tag",
            ));
        }
        Ok(name.to_owned())
    }
    fn canonical(name: &str) -> String {
        let name = name.strip_prefix("registry.ollama.ai/").unwrap_or(name);
        let name = name.strip_prefix("library/").unwrap_or(name);
        if name
            .rsplit('/')
            .next()
            .is_some_and(|part| part.contains(':'))
        {
            name.into()
        } else {
            format!("{name}:latest")
        }
    }

    #[derive(Debug, Clone, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct OllamaPullStatus {
        pub id: String,
        pub name: String,
        pub state: String,
        pub phase: String,
        pub digest: Option<String>,
        pub completed: Option<u64>,
        pub total: Option<u64>,
        pub error: Option<String>,
        pub started_at: i64,
        pub ended_at: Option<i64>,
    }
    impl OllamaPullStatus {
        fn active(&self) -> bool {
            matches!(self.state.as_str(), "pulling" | "cancelling")
        }
    }
    struct PullJob {
        info: Mutex<OllamaPullStatus>,
        cancel: tokio::sync::watch::Sender<bool>,
    }
    #[derive(Default)]
    struct Controller {
        operation: Mutex<()>,
        job: Mutex<Option<Arc<PullJob>>>,
        closing: AtomicBool,
    }
    impl Controller {
        fn status(&self) -> Option<OllamaPullStatus> {
            self.job.lock().as_ref().map(|job| job.info.lock().clone())
        }
        fn ensure_idle(&self) -> Result<()> {
            if self.closing.load(Ordering::Acquire) {
                return Err(AppError::new("OLLAMA_SHUTDOWN", "应用正在退出"));
            }
            if self.status().is_some_and(|s| s.active()) {
                return Err(AppError::new(
                    "OLLAMA_BUSY",
                    "请等待当前拉取完成或取消后再操作",
                ));
            }
            Ok(())
        }
        fn start(
            &self,
            endpoint: Endpoint,
            raw: &str,
            timing: PullTiming,
        ) -> Result<OllamaPullStatus> {
            let _operation = self.operation.lock();
            self.ensure_idle()?;
            let name = model_name(raw)?;
            endpoint.check()?;
            let (cancel, mut rx) = tokio::sync::watch::channel(false);
            let job = Arc::new(PullJob {
                info: Mutex::new(OllamaPullStatus {
                    id: format!("ollama-{}", NEXT_PULL.fetch_add(1, Ordering::Relaxed)),
                    name,
                    state: "pulling".into(),
                    phase: "pulling manifest".into(),
                    digest: None,
                    completed: None,
                    total: None,
                    error: None,
                    started_at: crate::services::now_ms(),
                    ended_at: None,
                }),
                cancel,
            });
            *self.job.lock() = Some(job.clone());
            let running = job.clone();
            let result = std::thread::Builder::new().name("ollama-pull".into()).spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread().enable_all().build()
                    .map_err(|e| AppError::io("启动模型拉取任务", e)).and_then(|rt| rt.block_on(async {
                        // watch 保留取消信号，线程尚未开始时取消也不会丢失。
                        if *rx.borrow() { return Err(AppError::new("OLLAMA_CANCELLED", "已取消本次拉取请求")); }
                        tokio::select! {
                            result = pull_stream(&endpoint, &running, timing) => result,
                            _ = rx.changed() => Err(AppError::new("OLLAMA_CANCELLED", "已取消本次拉取请求")),
                            _ = tokio::time::sleep(timing.total) => Err(AppError::new("OLLAMA_TIMEOUT", "拉取超过 2 小时，请检查网络后重试")),
                        }
                    }));
                let mut info = running.info.lock();
                if info.state == "cancelling" || result.as_ref().is_err_and(|e| e.code == "OLLAMA_CANCELLED") {
                    info.state = "cancelled".into(); info.error = None;
                } else { match result {
                    Ok(()) => { info.state = "succeeded".into(); info.phase = "success".into(); }
                    Err(e) => { info.state = "failed".into(); info.error = Some(e.message); }
                } }
                info.ended_at = Some(crate::services::now_ms());
            });
            if let Err(e) = result {
                let mut info = job.info.lock();
                info.state = "failed".into();
                info.error = Some(format!("无法启动拉取任务：{e}"));
                info.ended_at = Some(crate::services::now_ms());
                return Err(AppError::io("启动模型拉取任务", e));
            }
            let info = job.info.lock().clone();
            Ok(info)
        }
        fn cancel(&self, id: &str) -> Result<()> {
            let job =
                self.job.lock().clone().ok_or_else(|| {
                    AppError::new("OLLAMA_PULL_NOT_FOUND", "拉取任务不存在，请刷新")
                })?;
            let mut info = job.info.lock();
            if info.id != id {
                return Err(AppError::new(
                    "OLLAMA_PULL_NOT_FOUND",
                    "拉取任务已变更，请刷新",
                ));
            }
            if !info.active() {
                return Ok(());
            }
            info.state = "cancelling".into();
            job.cancel.send_replace(true);
            Ok(())
        }
        fn shutdown(&self) {
            let _operation = self.operation.lock();
            self.closing.store(true, Ordering::Release);
            if let Some(info) = self.status() {
                let _ = self.cancel(&info.id);
            }
        }
        fn delete(&self, endpoint: &Endpoint, raw: &str) -> Result<()> {
            let _operation = self.operation.lock();
            self.ensure_idle()?;
            let name = model_name(raw)?;
            let _lifecycle = endpoint.manager.lifecycle.lock();
            let before = endpoint.models()?;
            if !before
                .iter()
                .any(|row| canonical(&row.name) == canonical(&name))
            {
                return Err(AppError::new(
                    "OLLAMA_MODEL_NOT_FOUND",
                    "模型已不存在，请刷新列表",
                ));
            }
            let response = blocking_client()?
                .delete(format!("{}/api/delete", endpoint.url))
                .json(&serde_json::json!({"model": name}))
                .send()
                .map_err(request_error)?;
            read_response(response)?;
            if endpoint
                .models()?
                .iter()
                .any(|row| canonical(&row.name) == canonical(&name))
            {
                return Err(AppError::new(
                    "OLLAMA_DELETE_UNCONFIRMED",
                    "Ollama 仍返回此模型，尚未确认删除，请刷新后重试",
                ));
            }
            Ok(())
        }
    }
    #[derive(Clone, Copy)]
    struct PullTiming {
        idle: Duration,
        total: Duration,
    }
    impl Default for PullTiming {
        fn default() -> Self {
            Self {
                idle: Duration::from_secs(120),
                total: Duration::from_secs(7200),
            }
        }
    }
    #[derive(Deserialize)]
    struct ProgressLine {
        status: Option<String>,
        error: Option<String>,
        digest: Option<String>,
        total: Option<u64>,
        completed: Option<u64>,
    }
    fn apply_line(bytes: &[u8], job: &PullJob) -> Result<bool> {
        let line: ProgressLine = serde_json::from_slice(bytes)
            .map_err(|_| AppError::new("OLLAMA_BAD_STREAM", "Ollama 拉取进度格式无效"))?;
        if let Some(error) = line.error {
            return Err(AppError::new(
                "OLLAMA_PULL_FAILED",
                format!(
                    "模型拉取失败：{}",
                    error.chars().take(2048).collect::<String>()
                ),
            ));
        }
        let phase = line
            .status
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::new("OLLAMA_BAD_STREAM", "Ollama 未返回拉取状态"))?;
        let success = phase == "success";
        let mut info = job.info.lock();
        info.phase = phase.chars().take(256).collect();
        info.digest = line.digest.map(|d| d.chars().take(256).collect());
        info.total = line.total.filter(|n| *n > 0);
        info.completed = line
            .completed
            .map(|n| info.total.map_or(n, |total| n.min(total)));
        Ok(success)
    }
    async fn pull_stream(endpoint: &Endpoint, job: &PullJob, timing: PullTiming) -> Result<()> {
        endpoint.check()?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(timing.idle)
            .build()
            .map_err(request_error)?;
        let name = job.info.lock().name.clone();
        let mut response = client
            .post(format!("{}/api/pull", endpoint.url))
            .json(&serde_json::json!({"model": name, "stream": true}))
            .send()
            .await
            .map_err(request_error)?;
        let status = response.status();
        let mut pending = Vec::new();
        let mut success = false;
        while let Some(chunk) = response.chunk().await.map_err(request_error)? {
            for byte in chunk {
                if byte == b'\n' {
                    if !pending.is_empty() {
                        if !status.is_success() {
                            return Err(http_error(status, &pending));
                        }
                        success = apply_line(&pending, job)?;
                        pending.clear();
                    }
                } else {
                    if pending.len() >= MAX_LINE {
                        return Err(AppError::new(
                            "OLLAMA_STREAM_LIMIT",
                            "Ollama 单条进度响应过大",
                        ));
                    }
                    pending.push(byte);
                }
            }
        }
        if !status.is_success() {
            return Err(http_error(status, &pending));
        }
        if !pending.is_empty() {
            success = apply_line(&pending, job)?;
        }
        if !success {
            return Err(AppError::new(
                "OLLAMA_PULL_INCOMPLETE",
                "拉取连接已结束，但 Ollama 尚未确认成功，请重试",
            ));
        }
        job.info.lock().phase = "checking local model".into();
        // 再核对实例归属；不允许服务重启后把其它实例的模型当作本次结果。
        endpoint.check()?;
        let mut response = client
            .get(format!("{}/api/tags", endpoint.url))
            .send()
            .await
            .map_err(request_error)?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(request_error)? {
            if bytes.len() + chunk.len() > MAX_JSON {
                return Err(AppError::new(
                    "OLLAMA_RESPONSE_LIMIT",
                    "Ollama 模型列表响应过大",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(http_error(status, &bytes));
        }
        if !parse_models(&bytes)?
            .iter()
            .any(|row| canonical(&row.name) == canonical(&name))
        {
            return Err(AppError::new(
                "OLLAMA_PULL_UNCONFIRMED",
                "拉取已返回成功，但本地模型尚未找到，请刷新后重试",
            ));
        }
        endpoint.check()?;
        Ok(())
    }

    pub fn ollama_models(
        manager: Arc<crate::services::ServiceManager>,
    ) -> Result<Vec<OllamaModelRow>> {
        let _lifecycle = manager.lifecycle.lock();
        Endpoint::managed(manager.clone())?.models()
    }
    pub fn ollama_delete(manager: Arc<crate::services::ServiceManager>, name: &str) -> Result<()> {
        controller().delete(&Endpoint::managed(manager)?, name)
    }
    pub fn ollama_pull(
        manager: Arc<crate::services::ServiceManager>,
        name: &str,
    ) -> Result<OllamaPullStatus> {
        controller().start(Endpoint::managed(manager)?, name, PullTiming::default())
    }
    pub fn ollama_pull_status() -> Option<OllamaPullStatus> {
        controller().status()
    }
    pub fn ollama_cancel_pull(id: &str) -> Result<()> {
        controller().cancel(id)
    }
    pub fn ollama_shutdown() {
        controller().shutdown();
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;
        use std::net::{TcpListener, TcpStream};
        use std::time::Instant;

        #[derive(Clone)]
        enum Reply {
            Stream(Vec<Vec<u8>>),
            Hang,
            Error(u16, String),
        }
        struct Fixture {
            endpoint: Endpoint,
            plan: Arc<Mutex<Reply>>,
            tags: Arc<Mutex<(u16, String)>>,
            delete_status: Arc<AtomicU64>,
            delete_effect: Arc<AtomicBool>,
            closed: Arc<AtomicBool>,
            requests: Arc<Mutex<Vec<(String, String)>>>,
            stop: Arc<AtomicBool>,
            thread: Option<std::thread::JoinHandle<()>>,
        }
        fn model_json() -> String {
            serde_json::json!({"models":[{"name":"tiny:latest","digest":"sha256:fixture","size":512,
                "modified_at":"2026-09-27T00:00:00Z","details":{"parameter_size":"0.1B","quantization_level":"Q4_K_M"}}]}).to_string()
        }
        fn send(socket: &mut TcpStream, code: u16, body: &str) {
            let _ = write!(
                socket,
                "HTTP/1.1 {code} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
        impl Fixture {
            fn new() -> Self {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                listener.set_nonblocking(true).unwrap();
                let manager = Arc::new(crate::services::ServiceManager::new());
                manager.register(
                    "ollama",
                    "Fixture",
                    None,
                    None,
                    Some(port),
                    std::path::PathBuf::new(),
                );
                // 仅登记本次回环 HTTP fixture 所在进程，不启动或停止任何真实服务。
                manager.adopt("ollama", &[std::process::id()], Some(port));
                let endpoint = Endpoint {
                    url: format!("http://127.0.0.1:{port}"),
                    port,
                    pids: vec![std::process::id()],
                    manager,
                };
                let plan = Arc::new(Mutex::new(Reply::Stream(vec![
                    b"{\"status\":\"success\"}\n".to_vec(),
                ])));
                let tags = Arc::new(Mutex::new((200, model_json())));
                let delete_status = Arc::new(AtomicU64::new(200));
                let delete_effect = Arc::new(AtomicBool::new(true));
                let closed = Arc::new(AtomicBool::new(false));
                let requests = Arc::new(Mutex::new(Vec::new()));
                let stop = Arc::new(AtomicBool::new(false));
                let (p, t, d, e, c, r, s) = (
                    plan.clone(),
                    tags.clone(),
                    delete_status.clone(),
                    delete_effect.clone(),
                    closed.clone(),
                    requests.clone(),
                    stop.clone(),
                );
                let thread = std::thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(30);
                    let mut workers = Vec::new();
                    while !s.load(Ordering::Acquire) && Instant::now() < deadline {
                        if let Ok((mut socket, _)) = listener.accept() {
                            let (p, t, d, e, c, r, s) = (
                                p.clone(),
                                t.clone(),
                                d.clone(),
                                e.clone(),
                                c.clone(),
                                r.clone(),
                                s.clone(),
                            );
                            workers.push(std::thread::spawn(move || {
                                socket.set_nonblocking(false).unwrap();
                                socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
                                socket.set_write_timeout(Some(Duration::from_millis(100))).unwrap();
                                let mut data = Vec::new(); let mut buf = [0u8;1024];
                                let mut header_end = None; let mut length = 0;
                                while data.len() < 8192 {
                                    match socket.read(&mut buf) { Ok(0)|Err(_) => return, Ok(n) => data.extend_from_slice(&buf[..n]) }
                                    if header_end.is_none() {
                                        if let Some(index) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                                            header_end = Some(index+4);
                                            length = String::from_utf8_lossy(&data[..index]).lines().find_map(|l|
                                                l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|n| n.trim().parse::<usize>().ok())).unwrap_or(0);
                                        }
                                    }
                                    if header_end.is_some_and(|end| data.len() >= end + length) { break; }
                                }
                                let Some(end) = header_end else { return; };
                                let head = String::from_utf8_lossy(&data[..end]);
                                let first = head.lines().next().unwrap_or("").to_string();
                                let body = String::from_utf8_lossy(&data[end..]).into_owned();
                                r.lock().push((first.clone(), body));
                                if first.starts_with("GET /api/tags ") {
                                    let (code,body) = t.lock().clone(); send(&mut socket, code, &body);
                                } else if first.starts_with("DELETE /api/delete ") {
                                    let status = d.load(Ordering::Acquire) as u16;
                                    if status == 200 && e.load(Ordering::Acquire) { *t.lock() = (200, "{\"models\":[]}".into()); }
                                    send(&mut socket, status, if status == 200 { "" } else { "{\"error\":\"delete rejected\"}" });
                                } else if first.starts_with("POST /api/pull ") {
                                    let plan = p.lock().clone();
                                    if let Reply::Error(code, body) = plan { send(&mut socket, code, &body); return; }
                                    if socket.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n").is_err() { return; }
                                    match plan {
                                        Reply::Stream(chunks) => {
                                            for chunk in chunks {
                                                if write!(socket,"{:x}\r\n",chunk.len()).is_err() || socket.write_all(&chunk).is_err() || socket.write_all(b"\r\n").is_err() { return; }
                                                std::thread::sleep(Duration::from_millis(100));
                                            }
                                            let _ = socket.write_all(b"0\r\n\r\n");
                                        }
                                        Reply::Hang => {
                                            let deadline = Instant::now() + Duration::from_secs(4);
                                            while !s.load(Ordering::Acquire) && Instant::now() < deadline {
                                                match socket.read(&mut buf) {
                                                    Ok(0) => { c.store(true, Ordering::Release); return; }
                                                    Err(e) if !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => { c.store(true, Ordering::Release); return; }
                                                    _ => {}
                                                }
                                            }
                                        }
                                        _ => unreachable!(),
                                    }
                                } else { send(&mut socket,404,"{}"); }
                            }));
                        } else {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    for worker in workers {
                        worker.join().unwrap();
                    }
                });
                Self {
                    endpoint,
                    plan,
                    tags,
                    delete_status,
                    delete_effect,
                    closed,
                    requests,
                    stop,
                    thread: Some(thread),
                }
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                self.stop.store(true, Ordering::Release);
                if let Some(thread) = self.thread.take() {
                    thread.join().unwrap();
                }
            }
        }
        fn timing() -> PullTiming {
            PullTiming {
                idle: Duration::from_secs(2),
                total: Duration::from_secs(8),
            }
        }
        fn wait_for(mut predicate: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !predicate() {
                assert!(Instant::now() < deadline, "condition timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        fn ended(controller: &Controller) -> OllamaPullStatus {
            wait_for(|| controller.status().is_some_and(|s| !s.active()));
            controller.status().unwrap()
        }

        #[test]
        fn model_names_and_metadata_preserve_api_contract() {
            for name in [
                "qwen3:0.6b",
                "library/tiny",
                "hf.co/author/model:Q4_K_M",
                "localhost:5000/group/model:latest",
            ] {
                assert!(model_name(name).is_ok(), "{name}");
            }
            for name in [
                "",
                "--help",
                "tiny\nother",
                "../tiny",
                "https://host/model",
                "tiny:",
                "tiny:bad:tag",
                "host//model",
            ] {
                assert!(model_name(name).is_err(), "{name}");
            }
            assert_eq!(canonical("registry.ollama.ai/library/tiny"), "tiny:latest");
            let rows = parse_models(model_json().as_bytes()).unwrap();
            assert_eq!(rows[0].size, 512);
            assert_eq!(rows[0].parameters, "0.1B");
            assert_eq!(rows[0].quantization, "Q4_K_M");
            assert!(parse_models(b"{}").is_err());
            assert!(parse_models(b"<html>error</html>").is_err());
        }
        #[test]
        fn owned_instance_is_required_and_list_errors_are_not_empty_results() {
            let fixture = Fixture::new();
            assert_eq!(fixture.endpoint.models().unwrap().len(), 1);
            *fixture.tags.lock() = (503, "{\"error\":\"service unavailable\"}".into());
            assert!(fixture
                .endpoint
                .models()
                .unwrap_err()
                .message
                .contains("service unavailable"));
            *fixture.tags.lock() = (200, "{}".into());
            assert!(fixture.endpoint.models().is_err());
            fixture
                .endpoint
                .manager
                .set_state("ollama", crate::model::ServiceState::Stopped);
            assert!(fixture.endpoint.models().is_err());
            fixture
                .endpoint
                .manager
                .adopt("ollama", &[std::process::id()], Some(1));
            assert!(fixture.endpoint.check().is_err());
            let absent = Arc::new(crate::services::ServiceManager::new());
            assert!(Endpoint::managed(absent).is_err());
        }
        #[test]
        fn streaming_pull_tracks_files_deduplicates_and_verifies_local_model() {
            let fixture = Fixture::new();
            let controller = Controller::default();
            *fixture.plan.lock()=Reply::Stream(vec![b"{\"status\":\"pulling sha256:one\",\"digest\":\"sha256:one\",\"total\":400,\"completed\":100}\n".to_vec(),
                b"{\"sta".to_vec(),b"tus\":\"verifying sha256 digest\"}\n{\"status\":\"success\"}".to_vec()]);
            let job = controller
                .start(fixture.endpoint.clone(), "tiny", timing())
                .unwrap();
            assert!(controller
                .start(fixture.endpoint.clone(), "other", timing())
                .is_err());
            assert!(controller.delete(&fixture.endpoint, "tiny").is_err());
            assert!(controller.cancel("wrong-id").is_err());
            wait_for(|| controller.status().unwrap().completed == Some(100));
            assert_eq!(controller.status().unwrap().total, Some(400));
            let result = ended(&controller);
            assert_eq!(result.state, "succeeded", "{:?}", result.error);
            assert!(result.total.is_none());
            assert!(result.ended_at.is_some());
            assert_eq!(result.id, job.id);
            let requests = fixture.requests.lock();
            let body = &requests
                .iter()
                .find(|(line, _)| line.starts_with("POST"))
                .unwrap()
                .1;
            let json: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(json["model"], "tiny");
            assert_eq!(json["stream"], true);
            assert!(requests
                .iter()
                .any(|(line, _)| line.starts_with("GET /api/tags")));
        }
        #[test]
        fn pull_rejects_stream_errors_truncation_and_unverified_success() {
            let fixture = Fixture::new();
            let controller = Controller::default();
            for (reply, expected) in [
                (
                    Reply::Error(404, "{\"error\":\"model missing\"}".into()),
                    "model missing",
                ),
                (
                    Reply::Stream(vec![b"{\"error\":\"disk full\"}\n".to_vec()]),
                    "disk full",
                ),
                (
                    Reply::Stream(vec![b"{\"status\":\"pulling manifest\"}\n".to_vec()]),
                    "尚未确认成功",
                ),
                (Reply::Stream(vec![b"invalid\n".to_vec()]), "格式无效"),
                (Reply::Stream(vec![vec![b'x'; MAX_LINE + 1]]), "响应过大"),
            ] {
                *fixture.plan.lock() = reply;
                controller
                    .start(fixture.endpoint.clone(), "tiny", timing())
                    .unwrap();
                let status = ended(&controller);
                assert_eq!(status.state, "failed");
                assert!(status.error.unwrap().contains(expected), "{expected}");
            }
            *fixture.plan.lock() = Reply::Stream(vec![b"{\"status\":\"success\"}\n".to_vec()]);
            *fixture.tags.lock() = (200, "{\"models\":[]}".into());
            controller
                .start(fixture.endpoint.clone(), "tiny", timing())
                .unwrap();
            assert!(ended(&controller).error.unwrap().contains("尚未找到"));
        }
        #[test]
        fn cancel_drops_idle_connection_and_shutdown_rejects_new_work() {
            let fixture = Fixture::new();
            let controller = Controller::default();
            *fixture.plan.lock() = Reply::Hang;
            let first = controller
                .start(fixture.endpoint.clone(), "tiny", timing())
                .unwrap();
            wait_for(|| {
                fixture
                    .requests
                    .lock()
                    .iter()
                    .any(|(line, _)| line.starts_with("POST"))
            });
            controller.cancel(&first.id).unwrap();
            assert_eq!(ended(&controller).state, "cancelled");
            wait_for(|| fixture.closed.load(Ordering::Acquire));
            controller
                .start(fixture.endpoint.clone(), "tiny", timing())
                .unwrap();
            controller.shutdown();
            assert_eq!(ended(&controller).state, "cancelled");
            assert!(controller
                .start(fixture.endpoint.clone(), "tiny", timing())
                .is_err());
        }
        #[test]
        fn idle_and_total_timeouts_end_the_request() {
            for limits in [
                PullTiming {
                    idle: Duration::from_millis(100),
                    ..timing()
                },
                PullTiming {
                    total: Duration::from_millis(150),
                    ..timing()
                },
            ] {
                let fixture = Fixture::new();
                let controller = Controller::default();
                *fixture.plan.lock() = Reply::Hang;
                controller
                    .start(fixture.endpoint.clone(), "tiny", limits)
                    .unwrap();
                let status = ended(&controller);
                assert_eq!(status.state, "failed");
                assert!(status.error.is_some());
            }
        }
        #[test]
        fn deletion_requires_server_success_and_confirmed_absence() {
            let fixture = Fixture::new();
            let controller = Controller::default();
            fixture.delete_status.store(500, Ordering::Release);
            assert!(controller
                .delete(&fixture.endpoint, "tiny")
                .unwrap_err()
                .message
                .contains("delete rejected"));
            fixture.delete_status.store(200, Ordering::Release);
            fixture.delete_effect.store(false, Ordering::Release);
            assert_eq!(
                controller
                    .delete(&fixture.endpoint, "tiny")
                    .unwrap_err()
                    .code,
                "OLLAMA_DELETE_UNCONFIRMED"
            );
            fixture.delete_effect.store(true, Ordering::Release);
            controller.delete(&fixture.endpoint, "tiny").unwrap();
            assert!(fixture.endpoint.models().unwrap().is_empty());
            assert_eq!(
                controller
                    .delete(&fixture.endpoint, "tiny")
                    .unwrap_err()
                    .code,
                "OLLAMA_MODEL_NOT_FOUND"
            );
            let requests = fixture.requests.lock();
            let body = &requests
                .iter()
                .find(|(line, _)| line.starts_with("DELETE"))
                .unwrap()
                .1;
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(body).unwrap()["model"],
                "tiny"
            );
        }
    }
}
pub use ollama::{
    ollama_cancel_pull, ollama_delete, ollama_models, ollama_pull, ollama_pull_status,
    ollama_shutdown, OllamaModelRow, OllamaPullStatus,
};

/* ================= Adminer 数据库管理台 ================= */

pub const ADMINER_PORT: u16 = 8991;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminerStatus {
    pub port: u16,
    pub file: String,
    pub url: String,
    pub php_version: String,
    pub adminer_version: String,
}

pub(crate) struct AdminerRuntime {
    child: std::process::Child,
    group: platform::ProcessGroup,
    status: AdminerStatus,
}

impl Drop for AdminerRuntime {
    fn drop(&mut self) {
        let _ = self.group.terminate(true);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn adminer_status(manager: &crate::services::ServiceManager) -> Result<Option<AdminerStatus>> {
    let _operation = manager.lifecycle.lock();
    let mut guard = manager.adminer.lock();
    if let Some(runtime) = guard.as_mut() {
        match runtime
            .child
            .try_wait()
            .map_err(|e| AppError::io("读取管理台状态", e))?
        {
            None => return Ok(Some(runtime.status.clone())),
            Some(_) => {
                guard.take();
            }
        }
    }
    Ok(None)
}

pub(crate) fn adminer_pid(manager: &crate::services::ServiceManager) -> Option<u32> {
    manager.adminer.lock().as_mut().and_then(|runtime| {
        matches!(runtime.child.try_wait(), Ok(None)).then(|| runtime.child.id())
    })
}

/// 与服务启停、卸载共用生命周期锁；实际入口随本次进程保存。
pub fn adminer_start(
    store: &Store,
    paths: &Paths,
    installer: &crate::install::Installer,
    manager: &crate::services::ServiceManager,
) -> Result<AdminerStatus> {
    adminer_start_on_port(store, paths, installer, manager, ADMINER_PORT)
}

pub(crate) fn adminer_start_on_port(
    store: &Store,
    paths: &Paths,
    installer: &crate::install::Installer,
    manager: &crate::services::ServiceManager,
    port: u16,
) -> Result<AdminerStatus> {
    use std::io::Read;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let _operation = manager.lifecycle.lock();
    if manager
        .shutting_down
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err(AppError::new(
            "SHUTTING_DOWN",
            "应用正在退出，无法启动管理台",
        ));
    }
    if let Some(status) = adminer_status(manager)? {
        return Ok(status);
    }
    let php = crate::ops::installed_by_choice(store, "php")
        .ok_or_else(|| AppError::not_installed("PHP").with_hint("先到套件页安装 PHP"))?;
    let adm = crate::ops::installed_by_choice(store, "adminer")
        .ok_or_else(|| AppError::not_installed("Adminer").with_hint("先到套件页安装 Adminer"))?;
    let php_entry = PathBuf::from(&php.install_path).join(crate::install::entry_relative_path(
        &installer.installed_entry(&php).entry,
    ));
    let bin = php_entry
        .parent()
        .ok_or_else(|| AppError::new("BROKEN_INSTALL", "PHP 入口无效"))?;
    let php_exe = [
        bin.join(crate::ops::exe_name("php")),
        bin.join("../bin/php"),
        PathBuf::from(&php.install_path).join(crate::ops::exe_name("php")),
        PathBuf::from(&php.install_path).join("bin/php"),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .ok_or_else(|| {
        AppError::new(
            "PHP_EXE_MISSING",
            "所选 PHP 缺少 CLI 程序，请在套件页修复安装",
        )
    })?
    .canonicalize()?;
    let ini = paths.php_ini(&php.version);
    if !ini.is_file() {
        return Err(AppError::new(
            "PHP_CONFIG_MISSING",
            "所选 PHP 缺少配置文件，请在工具箱重置该版本的 PHP 配置后重试",
        ));
    }
    let root = PathBuf::from(&adm.install_path).canonicalize()?;
    let entry = root.join(crate::install::entry_relative_path(
        &installer.installed_entry(&adm).entry,
    ));
    if !entry.is_file() {
        return Err(AppError::new(
            "ADMINER_ENTRY_MISSING",
            "所选 Adminer 的入口文件不存在，请修复该版本安装",
        ));
    }
    let entry = entry.canonicalize()?;
    if !entry.starts_with(&root) || entry.extension().is_none_or(|e| e != "php") {
        return Err(AppError::new(
            "ADMINER_ENTRY_INVALID",
            "Adminer 入口必须是安装目录内的 PHP 文件",
        ));
    }
    let dir = entry
        .parent()
        .ok_or_else(|| AppError::new("ADMINER_ENTRY_INVALID", "Adminer 入口无效"))?;
    // PHP 内置服务器不能加载 Windows canonicalize 产生的 verbatim 文档根目录。
    #[cfg(windows)]
    let dir = {
        let text = dir.to_string_lossy();
        PathBuf::from(if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
        })
    };
    let file = entry
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            AppError::port_conflict(port, None)
        } else {
            AppError::io("预检管理台端口", e)
        }
    })?;
    let port = listener.local_addr()?.port();
    let mut url = reqwest::Url::parse(&format!("http://127.0.0.1:{port}/"))
        .map_err(|e| AppError::internal("管理台地址无效", e.to_string()))?;
    url.set_path(&format!("/{file}"));
    let status = AdminerStatus {
        port,
        file,
        url: url.to_string(),
        php_version: php.version,
        adminer_version: adm.version,
    };
    let log_path = paths.service_log("adminer");
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 每次启动独立日志，读取错误时不会混入上一次启动的内容。
    let log = std::fs::File::create(&log_path)?;
    let mut command = platform::command(&php_exe);
    command
        .arg("-c")
        .arg(&ini)
        .args(["-S", &format!("127.0.0.1:{port}"), "-t"])
        .arg(&dir)
        .current_dir(&dir)
        .env("PHPRC", &ini)
        .env("PHP_INI_SCAN_DIR", "")
        .env_remove("PHP_CLI_SERVER_WORKERS")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| platform::spawn_pre_exec());
        }
    }
    let mut group = platform::ProcessGroup::new_detached(false)?;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(800))
        .build()
        .map_err(|e| AppError::internal("准备管理台检查", e.to_string()))?;
    drop(listener);
    let mut child = command
        .spawn()
        .map_err(|e| AppError::io("启动 Adminer", e))?;
    if let Err(error) = group.attach(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error.into());
    }
    let mut runtime = AdminerRuntime {
        child,
        group,
        status: status.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if runtime.child.try_wait()?.is_some() {
            break;
        }
        if let Ok(response) = client.get(&status.url).send() {
            if response.status().is_success() {
                let mut body = String::new();
                if response.take(1024 * 1024).read_to_string(&mut body).is_ok()
                    && body.to_ascii_lowercase().contains("adminer")
                    && body.contains("<form")
                    && runtime.child.try_wait()?.is_none()
                    && crate::ports::listeners()?
                        .iter()
                        .any(|(p, pid)| *p == port && *pid == runtime.child.id())
                {
                    *manager.adminer.lock() = Some(runtime);
                    return Ok(status);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(runtime);
    let detail = std::fs::File::open(&log_path)
        .ok()
        .map(|f| {
            let mut s = String::new();
            let _ = f.take(16 * 1024).read_to_string(&mut s);
            s
        })
        .unwrap_or_default();
    Err(
        AppError::new("ADMINER_START_FAILED", "Adminer 页面未就绪，启动已取消")
            .with_hint(format!(
                "检查 PHP 配置、数据库扩展与日志：{}",
                log_path.display()
            ))
            .with_detail(detail),
    )
}

pub fn adminer_stop(manager: &crate::services::ServiceManager) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    let mut guard = manager.adminer.lock();
    if let Some(runtime) = guard.as_mut() {
        if runtime.child.try_wait()?.is_none() {
            runtime.group.terminate(true)?;
            if runtime.child.try_wait()?.is_none() {
                runtime.child.kill()?;
            }
        }
        runtime
            .child
            .wait()
            .map_err(|e| AppError::io("回收 Adminer 进程", e))?;
        guard.take();
    }
    Ok(())
}
