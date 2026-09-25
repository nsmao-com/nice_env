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

/// 采集 Redis 运行统计。连不上时 reachable=false（前端显示「未运行」而不是报错弹窗）。
pub fn redis_stats(port: u16) -> RedisStats {
    use std::io::{Read, Write};
    let mut empty = RedisStats {
        reachable: false,
        used_memory_human: None,
        keys: None,
        uptime_days: None,
        connected_clients: None,
    };
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return empty;
    };
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .ok();
    let cmd = resp_command(&["INFO"]);
    if stream.write_all(cmd.as_bytes()).is_err() {
        return empty;
    }
    let mut buf = String::new();
    if stream.read_to_string(&mut buf).is_err() && buf.is_empty() {
        return empty;
    }
    empty.reachable = true;

    let get = |key: &str| -> Option<String> {
        buf.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_string())
    };
    empty.used_memory_human = get("used_memory_human");
    empty.uptime_days = get("uptime_in_days").and_then(|v| v.parse().ok());
    empty.connected_clients = get("connected_clients").and_then(|v| v.parse().ok());
    // 键数：INFO 里没有现成字段，用 keyspace 行 db0:keys=N,expires=… 解析
    empty.keys = buf
        .lines()
        .find(|l| l.starts_with("db0:"))
        .and_then(|l| l.split_once(':'))
        .and_then(|(_, v)| v.split(',').next())
        .and_then(|kv| kv.strip_prefix("keys="))
        .and_then(|n| n.parse().ok());
    empty
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
        let st = redis_stats(port);
        assert!(!st.reachable);
        assert!(st.used_memory_human.is_none());
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
                let reply = "$92\r\nused_memory_human:1.5M\r\nuptime_in_days:12\r\nconnected_clients:3\r\ndb0:keys=42,expires=0\r\n".as_bytes();
                let _ = s.write_all(reply);
            }
        });
        let st = redis_stats(port);
        assert!(st.reachable);
        assert_eq!(st.used_memory_human.as_deref(), Some("1.5M"));
        assert_eq!(st.keys, Some(42));
        assert_eq!(st.uptime_days, Some(12));
        assert_eq!(st.connected_clients, Some(3));
    }
}
