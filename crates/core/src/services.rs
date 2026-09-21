//! 服务管理器：启动/停止/重启/状态 + Job Object 进程树 + 日志采集 + 健康检查。
//! 服务 ID 约定：nginx | php@8.3.33 | mysql@8.0.46 | redis | mihomo

use crate::configgen;
use crate::error::{AppError, Result};
use crate::model::{AppErrorInfo, ServiceState, ServiceStatus};
use crate::store::Store;
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct ServiceEntry {
    pub label: String,
    pub version: Option<String>,
    pub category: Option<String>,
    pub state: Mutex<ServiceState>,
    pub pids: Mutex<Vec<u32>>,
    pub started_at: Mutex<Option<SystemTime>>,
    pub last_error: Mutex<Option<AppError>>,
    pub group: Mutex<Option<platform::ProcessGroup>>,
    pub ring: Mutex<VecDeque<String>>,
    pub log_file: PathBuf,
    pub port: Option<u16>,
    /// 本进程实际启动时使用的端口。停机命令（mysqladmin/redis-cli）必须用它，
    /// 而不是当前设置里的端口——用户在运行期切换端口方案时会变。
    pub started_port: Mutex<Option<u16>>,
    /// 清单声明的前置依赖（服务 id）。注册时从清单带入，
    /// 这样列表就能显示「需要先装 X」而不必再查一次清单。
    pub requires: Vec<String>,
}

pub struct ServiceManager {
    pub services: Mutex<HashMap<String, Arc<ServiceEntry>>>,
    pub shutting_down: AtomicBool,
}

impl ServiceManager {
    pub fn new() -> Self {
        Self {
            services: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
        }
    }

    fn entry(&self, id: &str) -> Option<Arc<ServiceEntry>> {
        self.services.lock().get(id).cloned()
    }

    pub fn register(&self, id: &str, label: &str, version: Option<String>, category: Option<String>, port: Option<u16>, log_file: PathBuf) {
        let mut map = self.services.lock();
        if !map.contains_key(id) {
            map.insert(
                id.to_string(),
                Arc::new(ServiceEntry {
                    label: label.to_string(),
                    version,
                    category,
                    state: Mutex::new(ServiceState::Stopped),
                    pids: Mutex::new(Vec::new()),
                    started_at: Mutex::new(None),
                    last_error: Mutex::new(None),
                    group: Mutex::new(None),
                    ring: Mutex::new(VecDeque::new()),
                    log_file,
                    port,
                    started_port: Mutex::new(None),
                    requires: Vec::new(),
                }),
            );
        }
    }

    pub fn push_log(&self, id: &str, line: &str) {
        if let Some(e) = self.entry(id) {
            let mut ring = e.ring.lock();
            ring.push_back(line.to_string());
            while ring.len() > 2000 {
                ring.pop_front();
            }
        }
    }

    pub fn tail(&self, id: &str, lines: usize) -> Vec<String> {
        match self.entry(id) {
            Some(e) => {
                let ring = e.ring.lock();
                ring.iter().rev().take(lines).cloned().collect::<Vec<_>>().into_iter().rev().collect()
            }
            None => {
                // 未运行过：尝试读日志文件
                let path = crate::paths::Paths::new(std::path::PathBuf::new()).service_log(id);
                let _ = path;
                Vec::new()
            }
        }
    }

    pub fn set_state(&self, id: &str, state: ServiceState) {
        if let Some(e) = self.entry(id) {
            // 一旦进入运行态，上一次的失败记录就不再适用
            if matches!(state, ServiceState::Running | ServiceState::Starting) {
                *e.last_error.lock() = None;
            }
            *e.state.lock() = state;
        }
    }

    pub fn set_error(&self, id: &str, err: AppError) {
        if let Some(e) = self.entry(id) {
            *e.last_error.lock() = Some(err);
            *e.state.lock() = ServiceState::Error;
        }
    }

    /// 记录本次启动实际绑定的端口（停机命令据此寻址）
    pub fn set_started_port(&self, id: &str, port: u16) {
        if let Some(e) = self.entry(id) {
            *e.started_port.lock() = Some(port);
        }
    }

    /// 本次启动实际使用的端口；没有记录时回退到当前设置端口
    pub fn started_port_or(&self, id: &str, fallback: u16) -> u16 {
        self.entry(id)
            .and_then(|e| *e.started_port.lock())
            .unwrap_or(fallback)
    }

    /* ---------- 状态聚合 ---------- */

    pub fn snapshot(&self, id: &str) -> Option<ServiceStatus> {
        let e = self.entry(id)?;
        let state = e.state.lock().clone();
        let pids = e.pids.lock().clone();
        // 真实进程校验：running 状态但进程全没了 → 回写 Stopped，
        // 否则 e.state 会永远停在 Running，stop_service 的短路判断与 UI 显示长期不一致
        let effective = if state == ServiceState::Running && !pids.is_empty() {
            let alive = pids.iter().any(|p| platform::process_alive(*p));
            if alive {
                ServiceState::Running
            } else {
                *e.state.lock() = ServiceState::Stopped;
                ServiceState::Stopped
            }
        } else {
            state
        };
        let uptime = e
            .started_at
            .lock()
            .map(|t| SystemTime::now().duration_since(t).unwrap_or_default().as_secs());
        let memory_mb = if !pids.is_empty() {
            Some(crate::stats::processes_memory_mb(&pids))
        } else {
            None
        };
        let last_error = e
            .last_error
            .lock()
            .as_ref()
            .map(|err| AppErrorInfo::from(err.clone()));
        Some(ServiceStatus {
            id: id.to_string(),
            label: e.label.clone(),
            state: effective,
            pids,
            port: e.port,
            version: e.version.clone(),
            memory_mb,
            uptime_sec: uptime,
            last_error,
            log_file: Some(e.log_file.to_string_lossy().to_string()),
            category: e.category.clone(),
            requires: e.requires.clone(),
            // missing 需要 store 才能判断，由门面层补齐
            missing_requires: Vec::new(),
        })
    }

    pub fn list_status(&self) -> Vec<ServiceStatus> {
        let ids: Vec<String> = self.services.lock().keys().cloned().collect();
        let mut out: Vec<ServiceStatus> = ids.iter().filter_map(|id| self.snapshot(id)).collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }
}

/* ================= 启动辅助 ================= */

pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

/// 启动子进程：CREATE_NO_WINDOW + 管道输出 → 日志线程 + Job Object
pub fn spawn_tracked(
    manager: &Arc<ServiceManager>,
    service_id: &str,
    spec: &SpawnSpec,
) -> Result<u32> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 子进程自成进程组：停止时可用负 pid 一次性收走整棵子树
        unsafe {
            cmd.pre_exec(|| platform::spawn_pre_exec());
        }
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::io(&format!("启动 {}", spec.program.display()), e))?;
    let pid = child.id();

    // Job Object 归组（Windows 防孤儿）
    if let Some(entry) = manager.entry(service_id) {
        let mut group = entry.group.lock();
        if group.is_none() {
            *group = Some(platform::ProcessGroup::new().map_err(AppError::from)?);
        }
        group
            .as_mut()
            .unwrap()
            .attach(pid)
            .map_err(AppError::from)?;
    }
    if let Some(entry) = manager.entry(service_id) {
        entry.pids.lock().push(pid);
    }

    // 日志线程：stdout + stderr → ring + 文件
    if let Some(out) = child.stdout.take() {
        spawn_log_reader(Arc::clone(manager), service_id, out, "OUT");
    }
    if let Some(err) = child.stderr.take() {
        spawn_log_reader(Arc::clone(manager), service_id, err, "ERR");
    }

    // 回收线程（防止僵尸）
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    Ok(pid)
}

fn spawn_log_reader<R: std::io::Read + Send + 'static>(
    manager: Arc<ServiceManager>,
    service_id: &str,
    pipe: R,
    tag: &str,
) {
    let sid = service_id.to_string();
    let tag = tag.to_string();
    std::thread::spawn(move || {
        let reader = BufReader::new(pipe);
        for line in reader.lines() {
            match line {
                Ok(l) if !l.trim().is_empty() => {
                    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                    let full = format!("[{ts}] [{tag}] {l}");
                    manager.push_log(&sid, &full);
                    append_to_log_file(&manager, &sid, &full);
                }
                _ => break,
            }
        }
    });
}

fn append_to_log_file(manager: &ServiceManager, service_id: &str, line: &str) {
    if let Some(e) = manager.entry(service_id) {
        use std::io::Write;
        if let Some(parent) = e.log_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&e.log_file) {
            let _ = writeln!(f, "{line}");
        }
    }
}

/* ================= 健康检查 ================= */

pub fn tcp_port_open(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(600),
    )
    .is_ok()
}

pub fn wait_healthy(port: u16, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if tcp_port_open(port) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

pub fn wait_pids_alive(manager: &ServiceManager, id: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(e) = manager.entry(id) {
            let pids = e.pids.lock().clone();
            if !pids.is_empty() && pids.iter().all(|p| platform::process_alive(*p)) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/* ================= 各服务启停实现（由 core::ops 调用） ================= */

pub struct PortsProfile {
    pub http: u16,
    pub https: u16,
    pub mysql: u16,
    pub redis: u16,
    pub apache_http: u16,
    pub apache_https: u16,
    pub postgres: u16,
    pub mongodb: u16,
}

impl PortsProfile {
    pub fn safe() -> Self {
        Self {
            http: 8080, https: 8443, mysql: 23306, redis: 26379,
            apache_http: 8180, apache_https: 8444,
            postgres: 25432, mongodb: 28017,
        }
    }

    /// 标准档 = 各类服务约定俗成的默认端口（80/443/3306/6379…）。
    /// 这是新装应用的默认档：与项目里 `127.0.0.1:3306` 这类写死的连接串一致。
    pub fn standard() -> Self {
        Self {
            http: 80, https: 443, mysql: 3306, redis: 6379,
            apache_http: 8080, apache_https: 8443,
            postgres: 5432, mongodb: 27017,
        }
    }

    pub fn from_settings(store: &Store) -> Self {
        // 缺省走「标准档」（80/3306/6379…）：项目里写死的 127.0.0.1:3306 这类连接串
        // 开箱即用。与 FlyEnv/XAMPP 共存时不想抢常用端口，在设置里切「安全档」。
        // 注意：这里必须与 packages/schema 的 AppSettings.portProfile 默认值（standard）
        // 以及设置页展示保持一致，否则「没设置过」时会和用户看到的档位不符。
        let mut p = match store.get_setting("portProfile").as_deref() {
            Some("safe") => Self::safe(),
            _ => Self::standard(),
        };
        // 用户在「设置 → 端口」里逐个改过的端口优先于档位默认值
        for (key, value) in store.port_overrides() {
            if let Ok(n) = value.parse::<u16>() {
                p.apply(&key, n);
            }
        }
        p
    }

    /// 单独覆盖某一项（key 与 portOverrides 设置一致）
    fn apply(&mut self, key: &str, port: u16) {
        match key {
            "http" => self.http = port,
            "https" => self.https = port,
            "mysql" => self.mysql = port,
            "redis" => self.redis = port,
            "apacheHttp" => self.apache_http = port,
            "apacheHttps" => self.apache_https = port,
            "postgres" => self.postgres = port,
            "mongodb" => self.mongodb = port,
            _ => {}
        }
    }

    /// 端口项的稳定 key（设置页与 from_settings 共用同一套命名）
    pub fn keys() -> &'static [&'static str] {
        &[
            "http", "https", "mysql", "redis", "apacheHttp", "apacheHttps", "postgres", "mongodb",
        ]
    }
}

/// 端口预检：被占用即返回人话错误（含占用进程）
pub fn precheck_port(port: u16, what: &str) -> Result<()> {
    if !tcp_port_open(port) {
        return Ok(());
    }
    let diag = crate::ports::diagnose_port(port)?;
    let mut err = AppError::port_conflict(port, diag.process_name.as_deref())
        .with_detail(format!("启动 {what} 前端口预检失败"));
    if let Some(pid) = diag.pid {
        err = err.with_pid(pid);
    }
    Err(err)
}

/// 为 php 版本分配端口池（9100–9199，每池 4 个，避开已用）。
/// 已分配且**当前可用**则复用；否则重新分配并覆盖记录
/// （曾出现：用户装了别的环境占了 9100+，旧记录导致 PHP 永远起不来）。
pub fn allocate_php_pool(store: &Store, service_id: &str) -> Result<u16> {
    // 其它 php 版本已占用的池端口（不含自己）
    let used_by_others: Vec<u16> = store
        .all_port_assigns()
        .into_iter()
        .filter(|(sid, _)| sid != service_id)
        .flat_map(|(_, b)| (b..b + configgen::PHP_POOL_WORKERS).collect::<Vec<_>>())
        .collect();

    if let Some(p) = store.get_port_assign(service_id) {
        let range: Vec<u16> = (p..p + configgen::PHP_POOL_WORKERS).collect();
        let clash = range
            .iter()
            .any(|port| used_by_others.contains(port) || tcp_port_open(*port));
        if !clash {
            return Ok(p);
        }
        // 旧池已被外部占用 → 往下重找覆盖记录
    }

    let mut base = 9100u16;
    while base + configgen::PHP_POOL_WORKERS <= 9200 {
        let range: Vec<u16> = (base..base + configgen::PHP_POOL_WORKERS).collect();
        let clash = range
            .iter()
            .any(|p| used_by_others.contains(p) || tcp_port_open(*p));
        if !clash {
            store.set_port_assign(service_id, base)?;
            return Ok(base);
        }
        base += 4;
    }
    Err(AppError::new("NO_FREE_PORT", "php-cgi 端口池耗尽（9100-9200）")
        .with_hint("重开应用或检查是否有异常进程占用 9100+ 端段"))
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
