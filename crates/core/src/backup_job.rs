//! 自动配置备份：把站点/设置/订阅导出为带时间戳的 JSON，放进 backup/auto/。
//! 由设置项 backupSchedule 控制（off / daily / weekly）；
//! 桌面端启动交接完成后放行低频线程，按上次备份时间判断是否到期。

use crate::error::{AppError, Result};
use crate::paths::Paths;
use crate::store::Store;
use std::path::PathBuf;
use std::io::Write;

/// 生成完整备份后原子发布；纳秒和随机后缀避免同秒覆盖，保留旧时间戳文件兼容性。
pub fn run_backup_now(store: &Store, paths: &Paths) -> Result<PathBuf> {
    let _work = crate::BackgroundWork::begin("自动配置备份")?;
    run_backup_registered(store, paths)
}

fn run_backup_registered(store: &Store, paths: &Paths) -> Result<PathBuf> {
    let _activity = crate::paths::DataDirActivity::shared(&paths.base)?;
    let dir = paths.backup().join("auto");
    std::fs::create_dir_all(&dir)?;
    // 不同窗口也不能同时发布和轮转，以免删除另一份刚刚生成的备份。
    let _lock = backup_lock(&dir)?;
    let (json, _) = crate::transfer::encode_export(store)?;
    let now = chrono::Local::now();
    let dest = dir.join(format!("auto-{}-{:09}-{:016x}.json",
        now.format("%Y%m%d-%H%M%S"), now.timestamp_subsec_nanos(), rand::random::<u64>()));
    let mut pending = tempfile::Builder::new().prefix(".pending-").tempfile_in(&dir)?;
    pending.write_all(&json)?;
    pending.as_file().sync_all()?;
    pending.persist_noclobber(&dest).map_err(|e| AppError::io("发布自动备份", e.error))?;
    rotate_locked(&dir, 10, Some(&dest)).map_err(|e| e.with_hint(format!("新备份已保留在 {}；清理旧备份失败，请检查目录权限后重试", dest.display())))?;
    store.set_setting("lastAutoBackupAt", &crate::services::now_ms().to_string())?;
    Ok(dest)
}

fn backup_lock(dir: &std::path::Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(dir.join(".backup.lock"))?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => AppError::new("BACKUP_BUSY", "已有配置备份正在生成或清理，请稍后重试"),
        std::fs::TryLockError::Error(error) => AppError::io("锁定自动备份目录", error),
    })?;
    Ok(file)
}

/// 只清理本程序命名且可解析的普通备份文件，链接、目录、损坏和其它文件保留。
pub fn rotate(dir: &std::path::Path, keep: usize) -> Result<()> {
    let _work = crate::BackgroundWork::begin("清理自动配置备份")?;
    let _lock = backup_lock(dir)?;
    rotate_locked(dir, keep, None)
}

fn rotate_locked(dir: &std::path::Path, keep: usize, newest: Option<&std::path::Path>) -> Result<()> {
    static NAME: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(||
        regex::Regex::new(r"^auto-(\d{8}-\d{6})(?:-(\d{9})-([0-9a-f]{16}))?\.json$").unwrap());
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() { continue; }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(parts) = NAME.captures(&name) else { continue; };
        if chrono::NaiveDateTime::parse_from_str(&parts[1], "%Y%m%d-%H%M%S").is_err() { continue; }
        let bytes = std::fs::read(entry.path())?;
        let Ok(bundle) = serde_json::from_slice::<crate::transfer::ExportBundle>(&bytes) else { continue; };
        if bundle.format != "niceservbay/backup-v1" { continue; }
        files.push((name, entry.path()));
    }
    // 即使用户回调系统时间，也保留本次刚生成的备份，返回路径必须仍然有效。
    files.sort_by(|a, b| (Some(b.1.as_path()) == newest).cmp(&(Some(a.1.as_path()) == newest)).then_with(|| b.0.cmp(&a.0)));
    for (_, path) in files.into_iter().skip(keep) {
        std::fs::remove_file(&path).map_err(|e| AppError::io(&format!("清理旧备份 {}", path.display()), e))?;
    }
    Ok(())
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
    spawn_scheduler_when_ready(paths,None);
}

pub fn spawn_scheduler_when_ready(paths: Paths, gate: Option<std::sync::Arc<crate::restart::StartupGate>>) {
    std::thread::spawn(move || {
        if gate.is_some_and(|gate| !gate.wait()) { return; }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
            // 在打开数据库前注册；准备退出期间不初始化或写入存储。
            let Ok(_work) = crate::BackgroundWork::begin("自动配置备份调度") else { continue; };
            let Ok(_activity) = crate::paths::DataDirActivity::shared(&paths.base) else { continue; };
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
                    let _ = run_backup_registered(&store, &paths);
                }
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

        // 轮转兼容旧版命名；只清理有效备份，保留名称相似的其它资料。
        for i in 0..12 {
            std::fs::write(
                p.parent().unwrap().join(format!("auto-20200101-0000{i:02}.json")),
                &raw,
            )
            .unwrap();
        }
        let dir = p.parent().unwrap();
        for name in ["auto-notes.json", "auto-20190101-000000.json", "auto-20201399-000000.json"] {
            std::fs::write(dir.join(name), "{}").unwrap();
        }
        let folder = dir.join("auto-20180101-000000.json");
        std::fs::create_dir(&folder).unwrap();
        rotate(dir, 10).unwrap();
        // 10 份有效备份 + 3 份非备份 + 1 目录 + 1 锁文件。
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 15);
        assert!(p.is_file());
        assert!(folder.is_dir());
        assert!(!dir.join("auto-20200101-000000.json").exists());
        assert!(dir.join("auto-notes.json").exists());
        assert!(rotate(&base.path().join("missing"), 10).is_err());
    }

    #[test]
    fn repeated_backups_preserve_contents_and_busy_backup_does_not_report_success() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::new(base.path().to_path_buf()); paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let mut copies = Vec::new();
        for n in 0..5 {
            store.set_setting("fixture", &n.to_string()).unwrap();
            let path = run_backup_now(&store, &paths).unwrap();
            assert!(!copies.contains(&path));
            copies.push(path);
        }
        for (n, path) in copies.iter().enumerate() {
            let data: crate::transfer::ExportBundle = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            assert!(data.settings.contains(&("fixture".into(), n.to_string())));
        }
        let before = store.get_setting("lastAutoBackupAt");
        let held = backup_lock(copies[0].parent().unwrap()).unwrap();
        assert_eq!(run_backup_now(&store, &paths).unwrap_err().code, "BACKUP_BUSY");
        assert!(rotate(copies[0].parent().unwrap(), 1).is_err());
        assert_eq!(store.get_setting("lastAutoBackupAt"), before);
        assert!(copies.iter().all(|path| path.is_file()));
        drop(held);
        assert!(run_backup_now(&store, &paths).is_ok());
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

    #[cfg(windows)]
    #[test]
    fn rotation_failure_is_reported_and_future_names_do_not_remove_new_backup() {
        use std::os::windows::fs::OpenOptionsExt;
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::new(base.path().to_path_buf()); paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let original = run_backup_now(&store, &paths).unwrap();
        let before = store.get_setting("lastAutoBackupAt");
        let bytes = std::fs::read(&original).unwrap();
        let dir = original.parent().unwrap();
        for i in 0..12 {
            std::fs::write(dir.join(format!("auto-20990101-0000{i:02}.json")), &bytes).unwrap();
        }
        let held = std::fs::OpenOptions::new().read(true).share_mode(3)
            .open(dir.join("auto-20990101-000000.json")).unwrap();
        let error = run_backup_now(&store, &paths).unwrap_err();
        assert!(error.message.contains("清理旧备份"));
        assert_eq!(store.get_setting("lastAutoBackupAt"), before);
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
        drop(held);
        let newest = run_backup_now(&store, &paths).unwrap();
        assert!(newest.is_file(), "时钟回调后新备份不能被未来时间戳的旧文件挤掉");
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 11); // 10 份备份 + 锁
    }
}
