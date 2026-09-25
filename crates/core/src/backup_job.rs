//! 自动配置备份：把站点/设置/订阅导出为带时间戳的 JSON，放进 backup/auto/。
//! 由设置项 backupSchedule 控制（off / daily / weekly）；
//! CoreState::init 里起一条低频线程，按上次备份时间判断是否到期。

use crate::error::Result;
use crate::paths::Paths;
use crate::store::Store;
use std::path::PathBuf;

/// 生成一份自动备份，返回文件路径。文件名：auto-20260920-103000.json
pub fn run_backup_now(store: &Store, paths: &Paths) -> Result<PathBuf> {
    let dir = paths.backup().join("auto");
    std::fs::create_dir_all(&dir)?;
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let dest = dir.join(format!("auto-{ts}.json"));
    crate::transfer::export_to(store, &dest)?;
    rotate(&dir, 10);
    store.set_setting("lastAutoBackupAt", &crate::services::now_ms().to_string())?;
    Ok(dest)
}

/// 只保留最近 keep 份（按文件名倒序 = 时间倒序），多余的删除。
pub fn rotate(dir: &std::path::Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(String, std::path::PathBuf)> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.starts_with("auto-") && n.ends_with(".json")
        })
        .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(path);
    }
}

/// 距上次备份是否已到间隔（毫秒）。从未备份过 = 已到期。
pub fn due(store: &Store, interval_ms: i64) -> bool {
    match store
        .get_setting("lastAutoBackupAt")
        .and_then(|v| v.parse::<i64>().ok())
    {
        Some(last) => crate::services::now_ms() - last >= interval_ms,
        None => true,
    }
}

/// 调度常量：各档位的最小间隔
pub const DAILY_MS: i64 = 24 * 3600 * 1000;
pub const WEEKLY_MS: i64 = 7 * DAILY_MS;

/// 后台线程主体：每 6 小时醒来一次，按设置决定是否备份。
/// 线程内自开 SQLite 连接（Store 非 Clone；WAL 下多连接安全），独立于主状态。
pub fn spawn_scheduler(paths: Paths) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
        let Ok(store) = Store::open(paths.db()) else {
            continue;
        };
        let mode = store
            .get_setting("backupSchedule")
            .unwrap_or_else(|| "off".into());
        let interval = match mode.as_str() {
            "daily" => Some(DAILY_MS),
            "weekly" => Some(WEEKLY_MS),
            _ => None,
        };
        if let Some(iv) = interval {
            if due(&store, iv) {
                let _ = run_backup_now(&store, &paths);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_creates_timestamped_file_and_rotates() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::new(base.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();

        // 一份备份落盘 + lastAutoBackupAt 更新
        let p = run_backup_now(&store, &paths).unwrap();
        assert!(p.is_file());
        assert!(p
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("auto-"));
        assert!(store.get_setting("lastAutoBackupAt").is_some());

        // 内容是合法导出 JSON（含 format 标记）
        let raw = std::fs::read_to_string(&p).unwrap();
        assert!(raw.contains("niceservbay/backup-v1"));

        // 轮转：造假 12 份旧的，只留最新 10 份
        for i in 0..12 {
            std::fs::write(
                p.parent().unwrap().join(format!("auto-old-{i:02}.json")),
                "{}",
            )
            .unwrap();
        }
        rotate(p.parent().unwrap(), 10);
        let left = std::fs::read_dir(p.parent().unwrap()).unwrap().count();
        assert_eq!(left, 10, "应只剩 10 份");
    }

    #[test]
    fn due_logic() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::new(base.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        assert!(due(&store, DAILY_MS), "从未备份 = 到期");
        store
            .set_setting("lastAutoBackupAt", &crate::services::now_ms().to_string())
            .unwrap();
        assert!(!due(&store, DAILY_MS), "刚备份过 = 未到期");
    }
}
