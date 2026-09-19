//! 系统资源统计：CPU / 内存 / 磁盘 + 5 分钟历史环。

use crate::model::{StatsPoint, SystemStats};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static HISTORY: Lazy<Mutex<Vec<StatsPoint>>> = Lazy::new(|| Mutex::new(Vec::new()));
static SYS: Lazy<Mutex<sysinfo::System>> = Lazy::new(|| Mutex::new(sysinfo::System::new()));

pub fn get_system_stats() -> SystemStats {
    {
        let mut sys = SYS.lock();
        sys.refresh_memory();
        sys.refresh_cpu_usage();
        // CPU 使用率需要两轮采样间隔
        std::thread::sleep(std::time::Duration::from_millis(120));
        sys.refresh_cpu_usage();
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
        mem: if mem_total > 0.0 { (mem_used / mem_total) * 100.0 } else { 0.0 },
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
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    let targets: Vec<Pid> = pids.iter().map(|p| Pid::from_u32(*p)).collect();
    sys.refresh_processes(ProcessesToUpdate::Some(&targets), true);
    sys.processes()
        .iter()
        .filter(|(pid, _)| targets.contains(pid))
        .map(|(_, p)| p.memory() as f64 / 1024.0 / 1024.0)
        .sum()
}

fn now_sec() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
