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
    let base = crate::paths::Paths::resolve(None);
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

struct RedisClient(std::io::BufReader<std::net::TcpStream>);

enum RedisReply {
    Simple(String),
    Bulk(String),
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
        if let Some(pids) = expected_pids {
            let listeners = crate::ports::listeners()?;
            let owners: Vec<_> = listeners.iter().filter(|(p, _)| *p == port).collect();
            if owners.is_empty() || owners.iter().any(|(_, pid)| !pids.contains(pid)) {
                return Err(AppError::new(
                    "REDIS_INSTANCE_MISMATCH",
                    "Redis 端口归属与当前实例不符，未发送连接密码",
                ));
            }
        }
        let mut client = Self(std::io::BufReader::new(stream));
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

    fn command(&mut self, args: &[&str]) -> crate::error::Result<RedisReply> {
        use crate::error::AppError;
        use std::io::{BufRead, Read, Write};
        self.0.get_mut().write_all(resp_command(args).as_bytes())?;
        let mut header = Vec::new();
        (&mut self.0).take(1025).read_until(b'\n', &mut header)?;
        let invalid = || AppError::new("REDIS_PROTOCOL_ERROR", "Redis 响应格式无效，操作未完成");
        if header.len() > 1024 || !header.ends_with(b"\r\n") {
            return Err(invalid());
        }
        let header = std::str::from_utf8(&header[..header.len() - 2]).map_err(|_| invalid())?;
        if let Some(error) = header.strip_prefix('-') {
            // 不回显服务端错误正文，避免 AUTH 错误意外包含用户输入的凭据。
            return Err(if args.first() == Some(&"AUTH") {
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
                    "NOPERM" => AppError::new(
                        "REDIS_INFO_DENIED",
                        "当前 Redis 账号没有 INFO 权限，无法读取统计数据",
                    ),
                    _ => AppError::new(
                        "REDIS_INFO_FAILED",
                        "Redis 拒绝请求，请检查服务日志与账号权限",
                    ),
                }
            });
        }
        if let Some(value) = header.strip_prefix('+') {
            return Ok(RedisReply::Simple(value.to_string()));
        }
        let length: usize = header
            .strip_prefix('$')
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0 && *n <= 1024 * 1024)
            .ok_or_else(invalid)?;
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
        assert_eq!(resp_command(&["INFO"]), "*1\r\n$4\r\nINFO\r\n");
        assert_eq!(
            resp_command(&["SET", "k", "v"]),
            "*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n"
        );
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
