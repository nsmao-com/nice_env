//! 系统资源统计：CPU / 内存 / 磁盘 + 5 分钟历史环。

use crate::model::{StatsPoint, SystemStats};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static HISTORY: Lazy<Mutex<Vec<StatsPoint>>> = Lazy::new(|| Mutex::new(Vec::new()));
static SYS: Lazy<Mutex<sysinfo::System>> = Lazy::new(|| Mutex::new(sysinfo::System::new()));
/// 上一次刷新 CPU 采样的时刻
static LAST_CPU_REFRESH: Lazy<Mutex<Option<std::time::Instant>>> = Lazy::new(|| Mutex::new(None));
/// 磁盘容量缓存：(采样时刻, 可用 GB, 总 GB)。枚举卷在 Windows 上可能很慢
/// （网络盘 / 休眠的机械盘），而剩余空间几秒内几乎不变，没必要每次轮询都查。
static DISK_CACHE: Lazy<Mutex<Option<(std::time::Instant, f64, f64)>>> =
    Lazy::new(|| Mutex::new(None));
const DISK_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

pub fn get_system_stats() -> SystemStats {
    {
        let mut sys = SYS.lock();
        sys.refresh_memory();
        // CPU 使用率是两次采样之间的差值。SYS 常驻、前端定时轮询，
        // 上一次轮询就是天然的第一轮采样，只有首次调用才需要现场补一轮
        let mut last = LAST_CPU_REFRESH.lock();
        if sys.cpus().is_empty() {
            sys.refresh_cpu_usage();
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
            sys.refresh_cpu_usage();
            *last = Some(std::time::Instant::now());
        } else if last.is_none_or(|at| at.elapsed() >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL) {
            // 多个页面各自轮询，两次调用可能挨得很近；间隔太短差值会失真，沿用上次结果
            sys.refresh_cpu_usage();
            *last = Some(std::time::Instant::now());
        }
    }
    let sys = SYS.lock();
    let cpu = sys.global_cpu_usage();
    let mem_total = sys.total_memory() as f64 / 1024.0 / 1024.0;
    let mem_used = sys.used_memory() as f64 / 1024.0 / 1024.0;

    // 磁盘：数据目录所在盘
    let (disk_free, disk_total) = disk_of_base();
    let t = now_sec();

    let point = StatsPoint {
        t,
        cpu,
        mem: if mem_total > 0.0 {
            (mem_used / mem_total) * 100.0
        } else {
            0.0
        },
    };
    {
        let mut h = HISTORY.lock();
        h.push(point);
        // 5 分钟（2s 采样 → 150 点）
        while h.len() > 150 {
            h.remove(0);
        }
    }

    SystemStats {
        cpu_percent: cpu,
        mem_used_mb: mem_used,
        mem_total_mb: mem_total,
        disk_free_gb: disk_free,
        disk_total_gb: disk_total,
        history: HISTORY.lock().clone(),
    }
}

fn disk_of_base() -> (f64, f64) {
    let mut cache = DISK_CACHE.lock();
    if let Some((at, free, total)) = *cache {
        if at.elapsed() < DISK_CACHE_TTL {
            return (free, total);
        }
    }
    let (free, total) = query_disk_of_base();
    *cache = Some((std::time::Instant::now(), free, total));
    (free, total)
}

fn query_disk_of_base() -> (f64, f64) {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let Ok(base) = crate::paths::Paths::resolve(None) else { return (0.0, 0.0); };
    let mut best: Option<(usize, f64, f64)> = None;
    for d in disks.list() {
        if let Some(mount) = d.mount_point().to_str() {
            let norm = mount.trim_end_matches('\\').to_lowercase();
            if base.to_string_lossy().to_lowercase().starts_with(&norm) {
                let len = norm.len();
                if best.as_ref().map(|(l, _, _)| len > *l).unwrap_or(true) {
                    best = Some((
                        len,
                        d.available_space() as f64 / 1024.0 / 1024.0 / 1024.0,
                        d.total_space() as f64 / 1024.0 / 1024.0 / 1024.0,
                    ));
                }
            }
        }
    }
    match best {
        Some((_, free, total)) => (free, total),
        None => (0.0, 0.0),
    }
}

/// 指定 pids 的内存占用（MB）
pub fn processes_memory_mb(pids: &[u32]) -> f64 {
    processes_memory_map(pids).values().sum()
}

/// 一次刷新批量取多个 pid 各自的内存占用（MB），查不到的 pid 不出现在结果里。
///
/// Windows 上 sysinfo 哪怕只刷新一个 pid，也要用 NtQuerySystemInformation 把全部
/// 系统进程拉一遍；服务列表逐个服务查，同样的全量枚举会重复 N 次，所以批量查。
pub fn processes_memory_map(pids: &[u32]) -> std::collections::HashMap<u32, f64> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut out = std::collections::HashMap::new();
    if pids.is_empty() {
        return out;
    }
    let mut sys = System::new();
    let targets: Vec<Pid> = pids.iter().map(|p| Pid::from_u32(*p)).collect();
    sys.refresh_processes(ProcessesToUpdate::Some(&targets), true);
    for pid in &targets {
        if let Some(p) = sys.process(*pid) {
            out.insert(pid.as_u32(), p.memory() as f64 / 1024.0 / 1024.0);
        }
    }
    out
}

fn now_sec() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/* ================= Redis 运行统计（原生 RESP，零依赖） ================= */

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisStats {
    pub reachable: bool,
    pub port: u16,
    #[serde(skip)]
    pub process_id: u32,
    pub used_memory_human: Option<String>,
    pub keys: Option<u64>,
    pub uptime_days: Option<u64>,
    pub connected_clients: Option<u64>,
}

/// 内联 RESP 命令编码：*N\r\n$len\r\narg…（与冒烟测试同款，免依赖）
fn resp_command(args: &[&str]) -> String {
    let mut out = format!("*{}\r\n", args.len());
    for a in args {
        out.push_str(&format!("${}\r\n{}\r\n", a.len(), a));
    }
    out
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct RedisCredentials {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

impl RedisCredentials {
    pub fn key(version: &str) -> String {
        format!("redisConnection@{version}")
    }

    pub fn load(store: &crate::store::Store, version: &str) -> crate::error::Result<Self> {
        store
            .get_setting(&Self::key(version))
            .map(|value| {
                serde_json::from_str(&value).map_err(|_| {
                    crate::error::AppError::new(
                        "REDIS_CREDENTIALS_INVALID",
                        "Redis 本机连接记录损坏，请重新设置连接认证",
                    )
                })
            })
            .transpose()
            .map(Option::unwrap_or_default)
    }

    pub fn validate(&self) -> crate::error::Result<()> {
        if self.username.len() > 512
            || self.password.len() > 16384
            || self.username.contains('\0')
            || self.password.contains('\0')
            || (!self.username.is_empty() && self.password.is_empty())
        {
            return Err(crate::error::AppError::new(
                "REDIS_CREDENTIALS_INVALID",
                "请填写有效的 Redis 用户名和密码；无认证时两项均留空",
            ));
        }
        Ok(())
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisConnectionInfo {
    pub version: String,
    pub username: String,
    pub has_password: bool,
}

/// Redis 键空间浏览请求。浏览只使用 SCAN 和只读元数据命令。
#[derive(serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyRequest {
    pub version: String,
    #[serde(default)]
    pub database: u8,
    #[serde(default)]
    pub cursor: String,
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub count: u16,
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyInfo {
    pub key: String,
    pub key_type: String,
    /// -1 表示没有过期时间，-2 表示读取期间已经过期或不存在。
    pub ttl_ms: i64,
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyPage {
    pub version: String,
    pub database: u8,
    pub cursor: String,
    pub next_cursor: String,
    pub pattern: String,
    pub items: Vec<RedisKeyInfo>,
}

#[derive(serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyPreviewRequest {
    pub version: String,
    #[serde(default)]
    pub database: u8,
    pub key: String,
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyPreview {
    pub version: String,
    pub database: u8,
    pub key: String,
    pub key_type: String,
    pub ttl_ms: i64,
    pub memory_bytes: Option<u64>,
    pub elements: Option<u64>,
    pub value: Option<String>,
    pub value_truncated: bool,
    /// 用于编辑时检测键值是否在读取后被其他客户端修改。
    pub revision: String,
}

#[derive(serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyUpdateRequest {
    pub version: String,
    #[serde(default)]
    pub database: u8,
    pub key: String,
    /// 字符串键传入新值；集合类型传 null 表示只调整 TTL。
    pub value: Option<String>,
    /// preserve 保留当前 TTL，persist 清除 TTL，duration 设置 ttl_ms。
    pub ttl_mode: String,
    #[serde(default)]
    pub ttl_ms: i64,
    /// 由详情接口返回的内容修订号。
    pub revision: String,
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyUpdateReceipt {
    pub version: String,
    pub database: u8,
    pub key: String,
    pub updated: bool,
}

#[derive(serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyDeleteRequest {
    pub version: String,
    #[serde(default)]
    pub database: u8,
    pub key: String,
    pub confirmation: String,
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisKeyDeleteReceipt {
    pub version: String,
    pub database: u8,
    pub key: String,
    pub deleted: u64,
}

#[derive(serde::Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisFlushRequest {
    pub version: String,
    #[serde(default)]
    pub database: u8,
    pub confirmation: String,
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisFlushReceipt {
    pub version: String,
    pub database: u8,
    pub mode: String,
}

struct RedisClient(std::io::BufReader<std::net::TcpStream>, Option<u32>);

enum RedisReply {
    Simple(String),
    Integer(i64),
    Bulk(String),
    Nil,
    Array(Vec<RedisReply>),
}

impl RedisClient {
    fn connect(
        port: u16,
        credentials: &RedisCredentials,
        expected_pids: Option<&[u32]>,
    ) -> crate::error::Result<Self> {
        use crate::error::AppError;
        use std::time::Duration;
        credentials.validate()?;
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let stream = std::net::TcpStream::connect_timeout(&address, Duration::from_millis(500))
            .map_err(|_| {
                AppError::new(
                    "REDIS_UNREACHABLE",
                    "Redis 连接失败，请检查服务日志与实际端口",
                )
            })?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        // 先建立连接，再核对当前监听归属；任何凭据都只能发给本应用的实例。
        let mut verified_pid = None;
        if let Some(pids) = expected_pids {
            let listeners = crate::ports::listeners()?;
            let owners: Vec<_> = listeners.iter().filter(|(p, _)| *p == port).collect();
            if owners.is_empty() || owners.iter().any(|(_, pid)| !pids.contains(pid)) {
                return Err(AppError::new(
                    "REDIS_INSTANCE_MISMATCH",
                    "Redis 端口归属与当前实例不符，未发送连接密码",
                ));
            }
            let first = owners[0].1;
            if owners.iter().all(|(_, pid)| *pid == first) {
                verified_pid = Some(first);
            }
        }
        let mut client = Self(std::io::BufReader::new(stream), verified_pid);
        if !credentials.password.is_empty() {
            let args = if credentials.username.is_empty() || credentials.username == "default" {
                vec!["AUTH", credentials.password.as_str()]
            } else {
                vec![
                    "AUTH",
                    credentials.username.as_str(),
                    credentials.password.as_str(),
                ]
            };
            if !matches!(client.command(&args)?, RedisReply::Simple(value) if value == "OK") {
                return Err(AppError::new("REDIS_AUTH_FAILED", "Redis 身份验证未成功"));
            }
        }
        Ok(client)
    }

    fn native_process_info(&self, info: String) -> String {
        // MSYS2/Cygwin 的 INFO process_id 是 POSIX PID，不能与 Windows PID 比较。
        // 仅对已通过系统监听表核验的托管连接使用原生 PID；未验证的连接绝不猜测归属。
        let posix = cfg!(windows) && info.lines().any(|line| {
            line.starts_with("os:MSYS_") || line.starts_with("os:CYGWIN_")
        });
        if let Some(pid) = self.1.filter(|_| posix) {
            return info.lines().map(|line| {
                if line.strip_prefix("process_id:").is_some_and(|v| v.trim().parse::<u32>().is_ok_and(|p| p > 0)) {
                    format!("process_id:{pid}")
                } else { line.to_string() }
            }).collect::<Vec<_>>().join("\r\n");
        }
        info
    }

    fn command(&mut self, args: &[&str]) -> crate::error::Result<RedisReply> {
        use std::io::Write;
        self.0.get_mut().write_all(resp_command(args).as_bytes())?;
        self.reply(
            args.first().copied().unwrap_or_default(),
            0,
            &mut (1024 * 1024),
        )
    }

    fn reply(
        &mut self,
        command: &str,
        depth: usize,
        remaining: &mut usize,
    ) -> crate::error::Result<RedisReply> {
        use crate::error::AppError;
        use std::io::{BufRead, Read};
        let mut header = Vec::new();
        (&mut self.0).take(1025).read_until(b'\n', &mut header)?;
        let invalid = || AppError::new("REDIS_PROTOCOL_ERROR", "Redis 响应格式无效，操作未完成");
        if header.len() > 1024 || !header.ends_with(b"\r\n") {
            return Err(invalid());
        }
        *remaining = remaining.checked_sub(header.len()).ok_or_else(invalid)?;
        let header = std::str::from_utf8(&header[..header.len() - 2]).map_err(|_| invalid())?;
        if let Some(error) = header.strip_prefix('-') {
            // 不回显服务端错误正文，避免 AUTH 错误意外包含用户输入的凭据。
            return Err(if command == "AUTH" {
                AppError::new(
                    "REDIS_AUTH_FAILED",
                    "Redis 认证失败，请检查用户名、密码或选择无认证连接",
                )
            } else {
                match error.split_whitespace().next().unwrap_or_default() {
                    "NOAUTH" | "WRONGPASS" => AppError::new(
                        "REDIS_AUTH_REQUIRED",
                        "Redis 需要认证，请设置连接认证后重试",
                    ),
                    "NOPERM" if command == "INFO" => AppError::new(
                        "REDIS_INFO_DENIED",
                        "当前 Redis 账号没有 INFO 权限，无法读取统计数据",
                    ),
                    "NOPERM" if command == "FLUSHDB" => AppError::new("REDIS_FLUSH_DENIED", "当前 Redis 账号没有清空逻辑数据库的权限"),
                    "NOPERM" if matches!(command, "SCAN" | "TYPE" | "PTTL" | "GET" | "MEMORY" | "SELECT") =>
                        AppError::new("REDIS_KEY_BROWSE_DENIED", "当前 Redis 账号没有读取键空间所需的权限"),
                    "NOPERM" if matches!(command, "SET" | "PEXPIRE" | "PERSIST") =>
                        AppError::new("REDIS_KEY_EDIT_DENIED", "当前 Redis 账号没有修改键值或过期时间的权限"),
                    "NOPERM" => AppError::new("REDIS_COMMAND_DENIED", "当前 Redis 账号没有执行快照操作所需的权限")
                        .with_hint("请检查 INFO、TIME、BGSAVE 权限；独立备份还需要 CONFIG GET 权限，或在连接认证中选择合适账号"),
                    _ if command == "FLUSHDB" => AppError::new("REDIS_FLUSH_FAILED", "Redis 未接受清空逻辑数据库的请求")
                        .with_hint("请检查当前账号权限、实例状态和服务日志；未改用同步清空。"),
                    _ if matches!(command, "SCAN" | "TYPE" | "PTTL" | "GET" | "MEMORY" | "SELECT") =>
                        AppError::new("REDIS_KEY_BROWSE_FAILED", "Redis 拒绝读取键空间，请检查账号权限和服务日志"),
                    _ if matches!(command, "SET" | "PEXPIRE" | "PERSIST") =>
                        AppError::new("REDIS_KEY_EDIT_FAILED", "Redis 未接受键值或过期时间修改，请检查账号权限和服务日志"),
                    _ if command == "INFO" => AppError::new(
                        "REDIS_INFO_FAILED",
                        "Redis 拒绝请求，请检查服务日志与账号权限",
                    ),
                    _ => AppError::new("REDIS_COMMAND_FAILED", "Redis 拒绝快照请求")
                        .with_hint("请检查持久化状态、磁盘空间、目录权限和服务日志；没有改用同步保存或自动重试"),
                }
            });
        }
        if let Some(value) = header.strip_prefix('+') {
            return Ok(RedisReply::Simple(value.to_string()));
        }
        if let Some(value) = header.strip_prefix(':') {
            return value
                .parse::<i64>()
                .map(RedisReply::Integer)
                .map_err(|_| invalid());
        }
        if let Some(length) = header.strip_prefix('*') {
            let length: usize = length
                .parse()
                .ok()
                .filter(|n| *n <= 128 && depth <= 4)
                .ok_or_else(invalid)?;
            let mut values = Vec::with_capacity(length);
            for _ in 0..length {
                values.push(self.reply(command, depth + 1, remaining)?);
            }
            return Ok(RedisReply::Array(values));
        }
        let length = header
            .strip_prefix('$')
            .and_then(|v| v.parse::<isize>().ok())
            .ok_or_else(invalid)?;
        if length == -1 {
            return Ok(RedisReply::Nil);
        }
        let length: usize = length
            .try_into()
            .ok()
            .filter(|n: &usize| *n <= 1024 * 1024)
            .ok_or_else(invalid)?;
        *remaining = remaining.checked_sub(length + 2).ok_or_else(invalid)?;
        let mut payload = vec![0; length + 2];
        self.0.read_exact(&mut payload).map_err(|_| invalid())?;
        if &payload[length..] != b"\r\n" {
            return Err(invalid());
        }
        Ok(RedisReply::Bulk(
            std::str::from_utf8(&payload[..length])
                .map_err(|_| invalid())?
                .to_string(),
        ))
    }

    fn select_database(&mut self, database: u8) -> crate::error::Result<()> {
        if database == 0 {
            return Ok(());
        }
        let value = database.to_string();
        match self.command(&["SELECT", value.as_str()])? {
            RedisReply::Simple(message) if message == "OK" => Ok(()),
            _ => Err(crate::error::AppError::new(
                "REDIS_DATABASE_FAILED",
                "无法选择 Redis 逻辑数据库，请检查数据库编号和账号权限",
            )),
        }
    }

    fn key_type(&mut self, key: &str) -> crate::error::Result<String> {
        match self.command(&["TYPE", key])? {
            RedisReply::Simple(value) | RedisReply::Bulk(value) => Ok(value),
            _ => Err(crate::error::AppError::new(
                "REDIS_PROTOCOL_ERROR",
                "Redis 未返回有效的键类型",
            )),
        }
    }

    fn key_ttl(&mut self, key: &str) -> crate::error::Result<i64> {
        match self.command(&["PTTL", key])? {
            RedisReply::Integer(value) => Ok(value),
            RedisReply::Bulk(value) => value.parse().map_err(|_| {
                crate::error::AppError::new("REDIS_PROTOCOL_ERROR", "Redis 未返回有效的键过期时间")
            }),
            _ => Err(crate::error::AppError::new(
                "REDIS_PROTOCOL_ERROR",
                "Redis 未返回有效的键过期时间",
            )),
        }
    }
}

fn validate_key_pattern(pattern: &str) -> crate::error::Result<String> {
    use crate::error::AppError;
    if pattern.as_bytes().len() > 256 || pattern.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
        return Err(AppError::new(
            "REDIS_KEY_QUERY_INVALID",
            "键名筛选不能超过 256 字节，也不能包含控制字符",
        ));
    }
    Ok(if pattern.is_empty() { "*".to_string() } else { pattern.to_string() })
}

fn validate_key_name(key: &str) -> crate::error::Result<()> {
    use crate::error::AppError;
    if key.is_empty()
        || key.as_bytes().len() > 16 * 1024
        || key.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return Err(AppError::new(
            "REDIS_KEY_INVALID",
            "键名为空、过长或包含不支持的控制字符",
        ));
    }
    Ok(())
}

fn truncate_preview(value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &value[..end]), true)
}

fn redis_key_revision(key: &str, key_type: &str, value: Option<&str>, elements: Option<u64>) -> String {
    use sha2::{Digest, Sha256};
    let payload = format!("{key}\n{key_type}\n{}\n{}", value.unwrap_or_default(), elements.map(|n| n.to_string()).unwrap_or_default());
    hex::encode(Sha256::digest(payload.as_bytes()))
}

fn redis_cardinality(client: &mut RedisClient, key_type: &str, key: &str) -> Option<u64> {
    let command = match key_type {
        "list" => "LLEN",
        "set" => "SCARD",
        "hash" => "HLEN",
        "zset" => "ZCARD",
        "stream" => "XLEN",
        _ => return None,
    };
    match client.command(&[command, key]).ok()? {
        RedisReply::Integer(value) if value >= 0 => u64::try_from(value).ok(),
        RedisReply::Bulk(value) => value.parse().ok(),
        _ => None,
    }
}

pub(crate) fn redis_keys(
    port: u16,
    credentials: &RedisCredentials,
    pids: &[u32],
    request: &RedisKeyRequest,
) -> crate::error::Result<RedisKeyPage> {
    use crate::error::AppError;
    let pattern = validate_key_pattern(&request.pattern)?;
    let cursor = if request.cursor.is_empty() { "0" } else { request.cursor.as_str() };
    if cursor.parse::<u64>().is_err() {
        return Err(AppError::new("REDIS_KEY_QUERY_INVALID", "键空间游标无效，请重新刷新列表"));
    }
    let count = match request.count {
        0 => 40,
        1..=100 => request.count,
        _ => return Err(AppError::new("REDIS_KEY_QUERY_INVALID", "单页最多读取 100 个键")),
    };
    let mut client = RedisClient::connect(port, credentials, Some(pids))?;
    client.select_database(request.database)?;
    let count_text = count.to_string();
    let response = client.command(&["SCAN", cursor, "MATCH", pattern.as_str(), "COUNT", count_text.as_str()])?;
    let RedisReply::Array(mut envelope) = response else {
        return Err(AppError::new("REDIS_PROTOCOL_ERROR", "Redis 未返回有效的键空间列表"));
    };
    if envelope.len() != 2 {
        return Err(AppError::new("REDIS_PROTOCOL_ERROR", "Redis 键空间列表格式无效"));
    }
    let next_cursor = match envelope.remove(0) {
        RedisReply::Bulk(value) | RedisReply::Simple(value) => value,
        _ => return Err(AppError::new("REDIS_PROTOCOL_ERROR", "Redis 键空间游标格式无效")),
    };
    if next_cursor.parse::<u64>().is_err() {
        return Err(AppError::new("REDIS_PROTOCOL_ERROR", "Redis 键空间游标格式无效"));
    }
    let RedisReply::Array(keys) = envelope.remove(0) else {
        return Err(AppError::new("REDIS_PROTOCOL_ERROR", "Redis 键空间条目格式无效"));
    };
    let mut items = Vec::with_capacity(keys.len());
    for key_reply in keys {
        let key = match key_reply {
            RedisReply::Bulk(value) | RedisReply::Simple(value) => value,
            _ => return Err(AppError::new("REDIS_PROTOCOL_ERROR", "Redis 键名格式无效")),
        };
        let key_type = client.key_type(&key)?;
        if key_type == "none" {
            continue;
        }
        let ttl_ms = client.key_ttl(&key)?;
        items.push(RedisKeyInfo { key, key_type, ttl_ms });
    }
    Ok(RedisKeyPage {
        version: request.version.clone(),
        database: request.database,
        cursor: cursor.to_string(),
        next_cursor,
        pattern,
        items,
    })
}

pub(crate) fn redis_key_preview(
    port: u16,
    credentials: &RedisCredentials,
    pids: &[u32],
    request: &RedisKeyPreviewRequest,
) -> crate::error::Result<RedisKeyPreview> {
    use crate::error::AppError;
    validate_key_name(&request.key)?;
    let mut client = RedisClient::connect(port, credentials, Some(pids))?;
    client.select_database(request.database)?;
    let key_type = client.key_type(&request.key)?;
    let ttl_ms = client.key_ttl(&request.key)?;
    if key_type == "none" {
        return Err(AppError::new("REDIS_KEY_GONE", "这个键已不存在，请刷新键空间列表"));
    }
    let memory_bytes = match client.command(&["MEMORY", "USAGE", request.key.as_str()]) {
        Ok(RedisReply::Integer(value)) if value >= 0 => u64::try_from(value).ok(),
        Ok(RedisReply::Bulk(value)) => value.parse().ok(),
        _ => None,
    };
    let elements = redis_cardinality(&mut client, &key_type, &request.key);
    let (value, value_truncated) = if key_type == "string" {
        match client.command(&["GET", request.key.as_str()])? {
            RedisReply::Bulk(value) => {
                let (value, truncated) = truncate_preview(value, 64 * 1024);
                (Some(value), truncated)
            }
            RedisReply::Nil => (None, false),
            _ => return Err(AppError::new("REDIS_PROTOCOL_ERROR", "Redis 字符串值格式无效")),
        }
    } else {
        (None, false)
    };
    let revision = redis_key_revision(&request.key, &key_type, value.as_deref(), elements);
    Ok(RedisKeyPreview {
        version: request.version.clone(),
        database: request.database,
        key: request.key.clone(),
        key_type,
        ttl_ms,
        memory_bytes,
        elements,
        value,
        value_truncated,
        revision,
    })
}

pub(crate) fn redis_key_update(
    port: u16,
    credentials: &RedisCredentials,
    pids: &[u32],
    request: &RedisKeyUpdateRequest,
) -> crate::error::Result<RedisKeyUpdateReceipt> {
    use crate::error::AppError;
    validate_key_name(&request.key)?;
    if request.revision.trim().is_empty() {
        return Err(AppError::new("REDIS_KEY_REVISION_REQUIRED", "键详情已过期，请重新读取后再保存"));
    }
    let value = request.value.as_ref();
    if value.is_some_and(|value| value.len() > 1024 * 1024) {
        return Err(AppError::new("REDIS_VALUE_TOO_LARGE", "字符串值不能超过 1 MiB"));
    }
    let ttl_mode = request.ttl_mode.as_str();
    if !matches!(ttl_mode, "preserve" | "persist" | "duration") {
        return Err(AppError::new("REDIS_TTL_INVALID", "过期时间设置无效，请重新选择"));
    }
    if ttl_mode == "duration" && !(1..=31_536_000_000_000_i64).contains(&request.ttl_ms) {
        return Err(AppError::new("REDIS_TTL_INVALID", "过期时间必须在 1 毫秒到 1000 年之间"));
    }
    let mut client = RedisClient::connect(port, credentials, Some(pids))?;
    client.select_database(request.database)?;
    let key_type = client.key_type(&request.key)?;
    if key_type == "none" {
        return Err(AppError::new("REDIS_KEY_GONE", "这个键已不存在，请刷新键详情"));
    }
    let current_value = if key_type == "string" {
        match client.command(&["GET", request.key.as_str()])? {
            RedisReply::Bulk(value) => Some(value),
            RedisReply::Nil => return Err(AppError::new("REDIS_KEY_GONE", "这个键已不存在，请刷新键详情")),
            _ => return Err(AppError::new("REDIS_PROTOCOL_ERROR", "Redis 未返回有效的字符串值")),
        }
    } else {
        None
    };
    if value.is_some() && key_type != "string" {
        return Err(AppError::new("REDIS_VALUE_TYPE_UNSUPPORTED", "只有字符串键支持直接编辑值；集合键可以调整过期时间"));
    }
    let elements = redis_cardinality(&mut client, &key_type, &request.key);
    let current_revision = redis_key_revision(&request.key, &key_type, current_value.as_deref(), elements);
    if current_revision != request.revision {
        return Err(AppError::new("REDIS_KEY_CHANGED", "这个键在读取后已被其他客户端修改，请刷新详情后重试"));
    }
    let current_ttl = client.key_ttl(&request.key)?;
    if current_ttl == -2 {
        return Err(AppError::new("REDIS_KEY_GONE", "这个键已不存在，请刷新键详情"));
    }
    let mut command: Vec<String> = Vec::new();
    if let Some(value) = value {
        // XX 防止键在读取校验后恰好过期时被意外重新创建。
        command.extend(["SET".into(), request.key.clone(), value.clone(), "XX".into()]);
        match ttl_mode {
            "duration" => command.extend(["PX".into(), request.ttl_ms.to_string()]),
            "preserve" if current_ttl > 0 => command.extend(["PX".into(), current_ttl.to_string()]),
            _ => {}
        }
        let args = command.iter().map(String::as_str).collect::<Vec<_>>();
        match client.command(&args)? {
            RedisReply::Simple(message) | RedisReply::Bulk(message) if message.eq_ignore_ascii_case("OK") => {}
            RedisReply::Nil => return Err(AppError::new("REDIS_KEY_GONE", "这个键已不存在，请刷新键详情")),
            _ => return Err(AppError::new("REDIS_KEY_EDIT_FAILED", "Redis 未返回保存确认，键值未能确认已更新")),
        }
    } else {
        let response = match ttl_mode {
            "persist" => client.command(&["PERSIST", request.key.as_str()])?,
            "duration" => {
                let ttl = request.ttl_ms.to_string();
                client.command(&["PEXPIRE", request.key.as_str(), ttl.as_str()])?
            }
            "preserve" => return Err(AppError::new("REDIS_TTL_NOOP", "没有需要保存的键值或过期时间变化")),
            _ => unreachable!(),
        };
        match response {
            RedisReply::Integer(1) => {}
            RedisReply::Bulk(value) if value == "1" => {}
            RedisReply::Integer(0) if ttl_mode == "persist" => {}
            _ => return Err(AppError::new("REDIS_KEY_GONE", "键已不存在或过期时间未能更新，请刷新详情")),
        }
    }
    Ok(RedisKeyUpdateReceipt { version: request.version.clone(), database: request.database, key: request.key.clone(), updated: true })
}

pub(crate) fn redis_key_delete(
    port: u16,
    credentials: &RedisCredentials,
    pids: &[u32],
    request: &RedisKeyDeleteRequest,
) -> crate::error::Result<RedisKeyDeleteReceipt> {
    use crate::error::AppError;
    validate_key_name(&request.key)?;
    if request.confirmation != request.key {
        return Err(AppError::new(
            "REDIS_KEY_DELETE_CONFIRM_REQUIRED",
            "请输入完整键名以确认删除",
        ));
    }
    let mut client = RedisClient::connect(port, credentials, Some(pids))?;
    client.select_database(request.database)?;
    match client.command(&["DEL", request.key.as_str()])? {
        RedisReply::Integer(1) => Ok(RedisKeyDeleteReceipt {
            version: request.version.clone(),
            database: request.database,
            key: request.key.clone(),
            deleted: 1,
        }),
        RedisReply::Integer(0) => Err(AppError::new(
            "REDIS_KEY_GONE",
            "这个键已不存在，请刷新键空间列表",
        )),
        RedisReply::Bulk(value) if value == "1" => Ok(RedisKeyDeleteReceipt {
            version: request.version.clone(),
            database: request.database,
            key: request.key.clone(),
            deleted: 1,
        }),
        RedisReply::Bulk(value) if value == "0" => Err(AppError::new(
            "REDIS_KEY_GONE",
            "这个键已不存在，请刷新键空间列表",
        )),
        _ => Err(AppError::new(
            "REDIS_DELETE_FAILED",
            "Redis 未返回删除确认，未能确认这个键已删除",
        )),
    }
}

pub(crate) fn redis_flush(
    port: u16,
    credentials: &RedisCredentials,
    pids: &[u32],
    request: &RedisFlushRequest,
) -> crate::error::Result<RedisFlushReceipt> {
    use crate::error::AppError;
    if request.confirmation.trim() != "FLUSHDB" {
        return Err(AppError::new("REDIS_FLUSH_CONFIRM_REQUIRED", "请输入 FLUSHDB 以确认清空当前逻辑数据库"));
    }
    let mut client = RedisClient::connect(port, credentials, Some(pids))?;
    client.select_database(request.database)?;
    match client.command(&["FLUSHDB", "ASYNC"])? {
        RedisReply::Simple(value) if value.eq_ignore_ascii_case("OK") => Ok(RedisFlushReceipt {
            version: request.version.clone(),
            database: request.database,
            mode: "async".into(),
        }),
        _ => Err(AppError::new("REDIS_FLUSH_FAILED", "Redis 未返回清空确认，未能确认数据已清除")),
    }
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisPersistence {
    pub version: String,
    pub run_id: String,
    pub process_id: u32,
    pub loading: bool,
    pub saving: bool,
    pub changes_since_save: u64,
    pub last_save_time: u64,
    pub last_save_status: String,
    pub last_save_duration: Option<u64>,
    pub aof_enabled: bool,
    pub aof_rewriting: bool,
    pub aof_rewrite_scheduled: Option<bool>,
    pub aof_last_rewrite_status: Option<String>,
    pub aof_last_write_status: Option<String>,
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisSnapshotReceipt {
    pub version: String,
    pub run_id: String,
    pub process_id: u32,
    pub minimum_save_time: u64,
}

fn persistence_info(
    info: &str,
    version: &str,
    expected_pids: &[u32],
) -> crate::error::Result<RedisPersistence> {
    use crate::error::AppError;
    let invalid = || {
        AppError::new(
            "REDIS_PERSISTENCE_INVALID",
            "Redis 持久化响应不完整，无法确认保存状态",
        )
    };
    let get = |key: &str| {
        info.lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.trim())
    };
    let number = |key: &str| {
        get(key)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(invalid)
    };
    let flag = |key: &str| match get(key) {
        Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => Err(invalid()),
    };
    let status = |key: &str| match get(key) {
        Some("ok") => Ok(Some("ok".into())),
        Some("err") => Ok(Some("err".into())),
        None => Ok(None),
        _ => Err(invalid()),
    };
    let process_id = get("process_id")
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(invalid)?;
    if !expected_pids.contains(&process_id) {
        return Err(AppError::new(
            "REDIS_INSTANCE_MISMATCH",
            "Redis 响应与托管进程不符，未执行快照操作",
        ));
    }
    let run_id = get("run_id")
        .filter(|v| v.len() == 40 && v.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(invalid)?;
    Ok(RedisPersistence {
        version: version.into(),
        run_id: run_id.into(),
        process_id,
        loading: flag("loading")?,
        saving: flag("rdb_bgsave_in_progress")?,
        changes_since_save: number("rdb_changes_since_last_save")?,
        last_save_time: number("rdb_last_save_time")?,
        last_save_status: status("rdb_last_bgsave_status")?.ok_or_else(invalid)?,
        last_save_duration: match get("rdb_last_bgsave_time_sec") {
            Some("-1") | None => None,
            Some(value) => Some(value.parse().map_err(|_| invalid())?),
        },
        aof_enabled: flag("aof_enabled")?,
        aof_rewriting: flag("aof_rewrite_in_progress")?,
        aof_rewrite_scheduled: get("aof_rewrite_scheduled")
            .map(|_| flag("aof_rewrite_scheduled"))
            .transpose()?,
        aof_last_rewrite_status: status("aof_last_bgrewrite_status")?,
        aof_last_write_status: status("aof_last_write_status")?,
    })
}

impl RedisClient {
    fn persistence(
        &mut self,
        version: &str,
        pids: &[u32],
    ) -> crate::error::Result<RedisPersistence> {
        if let RedisReply::Bulk(info) = self.command(&["INFO"])? {
            return persistence_info(&self.native_process_info(info), version, pids);
        }
        Err(crate::error::AppError::new(
            "REDIS_PROTOCOL_ERROR",
            "Redis 未返回持久化状态",
        ))
    }

    fn server_time(&mut self) -> crate::error::Result<u64> {
        if let RedisReply::Array(values) = self.command(&["TIME"])? {
            if let [RedisReply::Bulk(seconds), RedisReply::Bulk(micros)] = values.as_slice() {
                if let (Ok(seconds), Ok(micros)) = (seconds.parse::<u64>(), micros.parse::<u32>()) {
                    if micros < 1_000_000 {
                        return Ok(seconds);
                    }
                }
            }
        }
        Err(crate::error::AppError::new(
            "REDIS_PROTOCOL_ERROR",
            "Redis 未返回有效的服务器时间，未发送快照请求",
        ))
    }
}

pub(crate) fn redis_persistence(
    port: u16,
    credentials: &RedisCredentials,
    pids: &[u32],
    version: &str,
) -> crate::error::Result<RedisPersistence> {
    RedisClient::connect(port, credentials, Some(pids))?.persistence(version, pids)
}

pub(crate) fn redis_rdb_file(
    paths: &crate::paths::Paths, port: u16, credentials: &RedisCredentials,
    pids: &[u32], version: &str, run_id: &str,
) -> crate::error::Result<std::path::PathBuf> {
    use crate::error::AppError;
    let mut client = RedisClient::connect(port, credentials, Some(pids))?;
    let current = client.persistence(version, pids)?;
    if current.run_id != run_id || current.loading || current.saving {
        return Err(AppError::new("REDIS_BACKUP_CHANGED", "Redis 实例或快照状态已变化，请重新生成备份"));
    }
    let mut config = |name: &str| -> crate::error::Result<String> {
        if let RedisReply::Array(values) = client.command(&["CONFIG", "GET", name])? {
            if let [RedisReply::Bulk(key), RedisReply::Bulk(value)] = values.as_slice() {
                if key == name { return Ok(value.clone()); }
            }
        }
        Err(AppError::new("REDIS_STORAGE_UNKNOWN", "无法确认 Redis 实际 RDB 保存位置"))
    };
    let directory = config("dir")?;
    // 新版 Windows Redis 的 CONFIG GET dir 返回 /cygdrive/c/...。
    // 转回原生路径后仍执行相同的 canonicalize 与托管目录归属检查。
    let directory = if cfg!(windows) {
        directory.strip_prefix("/cygdrive/")
            .filter(|p| p.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) && p.as_bytes().get(1) == Some(&b'/'))
            .map(|p| format!("{}:{}", &p[..1], &p[1..]))
            .unwrap_or(directory)
    } else { directory };
    let directory = std::path::PathBuf::from(directory);
    let expected = crate::paths::checked_data_path(&paths.base, "data/redis")?;
    if !directory.is_absolute() || directory.canonicalize()? != expected.canonicalize()? {
        return Err(AppError::new("REDIS_STORAGE_UNMANAGED", "Redis 使用了非托管数据目录，请在外部管理该目录的备份"));
    }
    let name = config("dbfilename")?;
    if name.contains(['/', '\\']) { return Err(AppError::new("REDIS_STORAGE_UNKNOWN", "RDB 文件名不是单个文件名")); }
    Ok(crate::paths::checked_data_path(&paths.base, &format!("data/redis/{name}"))?)
}

/// 只提交异步保存；完成状态由同一 run_id + PID 的 INFO 回读确认，不把 BGSAVE 接受当成已落盘。
pub(crate) fn redis_snapshot(
    port: u16,
    credentials: &RedisCredentials,
    pids: &[u32],
    version: &str,
) -> crate::error::Result<RedisSnapshotReceipt> {
    use crate::error::AppError;
    let mut client = RedisClient::connect(port, credentials, Some(pids))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let current = client.persistence(version, pids)?;
        if current.loading
            || current.saving
            || current.aof_rewriting
            || current.aof_rewrite_scheduled == Some(true)
        {
            return Err(AppError::new(
                "REDIS_PERSISTENCE_BUSY",
                "Redis 正在载入数据、生成快照或重写 AOF，请等待完成后再试",
            ));
        }
        // LASTSAVE 只有秒级精度且启动时也会初始化；跨过服务器时间边界再请求，避免立即完成的小快照被误判。
        let now = client.server_time()?;
        if now > current.last_save_time {
            match client.command(&["BGSAVE"]) {
                Ok(RedisReply::Simple(message))
                    if message == "Background saving started" || message == "OK" =>
                {
                    return Ok(RedisSnapshotReceipt {
                        version: version.into(),
                        run_id: current.run_id,
                        process_id: current.process_id,
                        minimum_save_time: now,
                    });
                }
                Err(error)
                    if matches!(
                        error.code.as_str(),
                        "REDIS_AUTH_REQUIRED" | "REDIS_COMMAND_DENIED" | "REDIS_COMMAND_FAILED"
                    ) =>
                {
                    return Err(error)
                }
                _ => {
                    return Err(AppError::new(
                        "REDIS_SNAPSHOT_UNCONFIRMED",
                        "快照请求的响应未确认，Redis 可能已开始保存",
                    )
                    .with_hint("请重新读取持久化状态并检查服务日志，不要连续重复提交"))
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(AppError::new(
                "REDIS_SNAPSHOT_CLOCK",
                "Redis 保存时间尚未越过确认边界，未发送快照请求",
            )
            .with_hint("请稍后再试；若持续出现，请检查服务器系统时间或其他客户端的频繁保存操作"));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// 按 RESP2 帧长度读取；Redis 保持连接时无需等待超时或 EOF。
pub fn redis_stats(port: u16) -> crate::error::Result<RedisStats> {
    redis_stats_authenticated(port, &RedisCredentials::default(), None)
}

pub(crate) fn redis_stats_authenticated(
    port: u16,
    credentials: &RedisCredentials,
    expected_pids: Option<&[u32]>,
) -> crate::error::Result<RedisStats> {
    use crate::error::AppError;
    let mut client = RedisClient::connect(port, credentials, expected_pids)?;
    let invalid = || {
        AppError::new(
            "REDIS_PROTOCOL_ERROR",
            "Redis 统计响应格式无效，未显示统计数据",
        )
    };
    let RedisReply::Bulk(info) = client.command(&["INFO"])? else {
        return Err(invalid());
    };
    let info = client.native_process_info(info);
    let get = |key: &str| {
        info.lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.trim())
    };
    let process_id = get("process_id")
        .and_then(|v| v.parse().ok())
        .ok_or_else(invalid)?;
    let mut keys = 0u64;
    for line in info.lines() {
        if let Some((db, values)) = line.split_once(':') {
            if db
                .strip_prefix("db")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            {
                let count = values
                    .split(',')
                    .find_map(|v| v.strip_prefix("keys="))
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or_else(invalid)?;
                keys = keys.checked_add(count).ok_or_else(invalid)?;
            }
        }
    }
    Ok(RedisStats {
        reachable: true,
        port,
        process_id,
        used_memory_human: Some(get("used_memory_human").ok_or_else(invalid)?.to_string()),
        keys: info
            .lines()
            .any(|line| line == "# Keyspace")
            .then_some(keys),
        uptime_days: Some(
            get("uptime_in_days")
                .and_then(|v| v.parse().ok())
                .ok_or_else(invalid)?,
        ),
        connected_clients: Some(
            get("connected_clients")
                .and_then(|v| v.parse().ok())
                .ok_or_else(invalid)?,
        ),
    })
}

pub(crate) fn redis_shutdown(
    port: u16,
    credentials: &RedisCredentials,
    expected_pids: &[u32],
) -> crate::error::Result<()> {
    use std::io::{BufRead, Read, Write};
    let mut client = RedisClient::connect(port, credentials, Some(expected_pids))?;
    client
        .0
        .get_mut()
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    client
        .0
        .get_mut()
        .write_all(resp_command(&["SHUTDOWN"]).as_bytes())?;
    let mut reply = Vec::new();
    match (&mut client.0).take(1025).read_until(b'\n', &mut reply) {
        Ok(0) => Ok(()), // 成功关闭连接；ops 继续确认进程已退出。
        // Windows Redis 退出时可能复位连接；仍由 ops 确认托管 PID 全部退出。
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
            ) =>
        {
            Ok(())
        }
        Ok(_) if reply.starts_with(b"-NOAUTH ") || reply.starts_with(b"-WRONGPASS ") => {
            Err(crate::error::AppError::new(
                "REDIS_AUTH_REQUIRED",
                "Redis 需要认证，请设置连接认证后重试",
            ))
        }
        Ok(_) => Err(crate::error::AppError::new(
            "REDIS_SHUTDOWN_DENIED",
            "Redis 拒绝停止，实例仍在运行",
        )
        .with_hint("请确认连接账号具备 SHUTDOWN 权限，并检查持久化目录是否可写；未强制结束进程")),
        Err(_) => Err(crate::error::AppError::new(
            "REDIS_SHUTDOWN_TIMEOUT",
            "Redis 尚未确认退出，请检查持久化进度和服务日志",
        )
        .with_hint("未强制结束进程，请等待保存完成后再检查状态")),
    }
}

#[cfg(test)]
mod redis_stats_tests {
    use super::*;

    #[test]
    fn resp_encoding_matches_protocol() {
        assert_eq!(crate::redis_backup::rdb_checksum(0, b"123456789"), 0xe9c6d914c4b8d9ca);
        assert_eq!(resp_command(&["INFO"]), "*1\r\n$4\r\nINFO\r\n");
        assert_eq!(
            resp_command(&["SET", "k", "v"]),
            "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n"
        );
        for (reply, valid) in [
            ("*2\r\n$10\r\n1790607430\r\n$6\r\n123456\r\n", true),
            ("*2\r\n$10\r\n1790607430\r\n$7\r\n1000000\r\n", false),
            ("*17\r\n", false), ("*1\r\n*1\r\n", false), ("*2\r\n$-1\r\n", false),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                use std::io::{Read, Write};
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
                let mut bytes = vec![0; resp_command(&["TIME"]).len()];
                stream.read_exact(&mut bytes).unwrap();
                assert_eq!(bytes, resp_command(&["TIME"]).as_bytes());
                stream.write_all(reply.as_bytes()).unwrap();
            });
            let result = RedisClient::connect(port, &RedisCredentials::default(), None).unwrap().server_time();
            assert_eq!(result.is_ok(), valid);
            if valid { assert_eq!(result.unwrap(), 1790607430); }
            server.join().unwrap();
        }
    }

    #[test]
    fn unreachable_redis_reports_not_reachable() {
        // 找一个肯定没监听的端口
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l); // 释放后再连
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(redis_stats(port).unwrap_err().code, "REDIS_UNREACHABLE");
    }

    #[test]
    fn info_reply_parses_into_stats() {
        // 起一个一次性 TCP 服务假装 Redis，回一段典型 INFO
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut s, _)) = l.accept() {
                let mut b = [0u8; 256];
                let _ = s.read(&mut b);
                let body = "process_id:123\r\nused_memory_human:1.5M\r\nuptime_in_days:12\r\nconnected_clients:3\r\n# Keyspace\r\ndb0:keys=40,expires=0\r\ndb2:keys=2,expires=0\r\n";
                s.write_all(format!("${}\r\n{}\r\n", body.len(), body).as_bytes())
                    .unwrap();
                // 客户端应在服务端仍保留连接时完成读取。
                s.set_read_timeout(Some(std::time::Duration::from_secs(4)))
                    .unwrap();
                let mut eof = [0; 1];
                assert_eq!(s.read(&mut eof).unwrap(), 0);
            }
        });
        let st = redis_stats(port).unwrap();
        assert!(st.reachable);
        assert_eq!(st.port, port);
        assert_eq!(st.process_id, 123);
        assert_eq!(st.used_memory_human.as_deref(), Some("1.5M"));
        assert_eq!(st.keys, Some(42));
        assert_eq!(st.uptime_days, Some(12));
        assert_eq!(st.connected_clients, Some(3));
        let info = "process_id:123\r\nrun_id:0123456789abcdef0123456789abcdef01234567\r\nloading:0\r\nrdb_bgsave_in_progress:0\r\nrdb_changes_since_last_save:12\r\nrdb_last_save_time:1790607430\r\nrdb_last_bgsave_status:ok\r\nrdb_last_bgsave_time_sec:-1\r\naof_enabled:0\r\naof_rewrite_in_progress:0\r\n";
        let persistence = persistence_info(info, "5.0.14", &[123]).unwrap();
        assert_eq!(persistence.last_save_duration, None);
        assert_eq!(persistence.aof_last_write_status, None);
        assert_eq!(persistence.changes_since_save, 12);
        assert_eq!(persistence_info(info, "5.0.14", &[124]).unwrap_err().code, "REDIS_INSTANCE_MISMATCH");
        for broken in [info.replace("loading:0", "loading:2"), info.replace("rdb_last_bgsave_status:ok", "rdb_last_bgsave_status:unknown"), info.replace("rdb_changes_since_last_save:12", ""), info.replace("rdb_last_bgsave_time_sec:-1", "rdb_last_bgsave_time_sec:-2")] {
            assert_eq!(persistence_info(&broken, "5.0.14", &[123]).unwrap_err().code, "REDIS_PERSISTENCE_INVALID");
        }
        let failed = persistence_info(&(info.replace("rdb_last_bgsave_status:ok", "rdb_last_bgsave_status:err") + "aof_last_write_status:err\r\naof_rewrite_scheduled:1\r\n"), "5.0.14", &[123]).unwrap();
        assert_eq!(failed.last_save_status, "err");
        assert_eq!(failed.aof_last_write_status.as_deref(), Some("err"));
        assert_eq!(failed.aof_rewrite_scheduled, Some(true));
    }

    #[test]
    fn redis_errors_and_incomplete_frames_are_not_success() {
        for (reply, code) in [
            (
                "-NOAUTH Authentication required.\r\n",
                "REDIS_AUTH_REQUIRED",
            ),
            ("-NOPERM no permissions\r\n", "REDIS_INFO_DENIED"),
            ("-ERR unknown command\r\n", "REDIS_INFO_FAILED"),
            ("$9999999\r\n", "REDIS_PROTOCOL_ERROR"),
            ("$12\r\ntruncated", "REDIS_PROTOCOL_ERROR"),
            ("$-1\r\n", "REDIS_PROTOCOL_ERROR"),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = std::thread::spawn(move || {
                use std::io::{Read, Write};
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut command = [0; 256];
                stream.read(&mut command).unwrap();
                stream.write_all(reply.as_bytes()).unwrap();
            });
            assert_eq!(redis_stats(port).unwrap_err().code, code);
            server.join().unwrap();
        }
    }

    #[test]
    fn redis_auth_frames_support_legacy_acl_and_redacted_failures() {
        for username in ["", "default", "worker"] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let expected = if username == "worker" {
                resp_command(&["AUTH", username, "space 中文 #"])
            } else {
                resp_command(&["AUTH", "space 中文 #"])
            };
            let server = std::thread::spawn(move || {
                use std::io::{Read, Write};
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut command = vec![0; expected.len()];
                stream.read_exact(&mut command).unwrap();
                assert_eq!(command, expected.as_bytes());
                stream.write_all(b"+OK\r\n").unwrap();
                let mut info = vec![0; resp_command(&["INFO"]).len()];
                stream.read_exact(&mut info).unwrap();
                assert_eq!(info, resp_command(&["INFO"]).as_bytes());
                let body = "process_id:123\r\nused_memory_human:1M\r\nuptime_in_days:0\r\nconnected_clients:1\r\n# Keyspace\r\n";
                stream
                    .write_all(format!("${}\r\n{}\r\n", body.len(), body).as_bytes())
                    .unwrap();
            });
            let credentials = RedisCredentials {
                username: username.into(),
                password: "space 中文 #".into(),
            };
            assert_eq!(
                redis_stats_authenticated(port, &credentials, None)
                    .unwrap()
                    .keys,
                Some(0)
            );
            server.join().unwrap();
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut command = [0; 256];
            stream.read(&mut command).unwrap();
            stream
                .write_all(b"-WRONGPASS sensitive-fixture\r\n")
                .unwrap();
        });
        let error = redis_stats_authenticated(
            port,
            &RedisCredentials {
                username: String::new(),
                password: "sensitive-fixture".into(),
            },
            None,
        )
        .unwrap_err();
        assert_eq!(error.code, "REDIS_AUTH_FAILED");
        assert!(!serde_json::to_string(&error)
            .unwrap()
            .contains("sensitive-fixture"));
        server.join().unwrap();
    }

    #[test]
    fn credentials_are_not_sent_to_unowned_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            use std::io::Read;
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut bytes = [0; 1];
            assert_eq!(stream.read(&mut bytes).unwrap(), 0);
        });
        let credentials = RedisCredentials {
            username: String::new(),
            password: "must-not-send".into(),
        };
        assert_eq!(
            RedisClient::connect(port, &credentials, Some(&[u32::MAX]))
                .err()
                .unwrap()
                .code,
            "REDIS_INSTANCE_MISMATCH"
        );
        server.join().unwrap();
    }
}
