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


/// 原生数据库计划按引擎和安装版本隔离；状态与设置一次写入现有 settings，凭据不进入计划。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPlanConfig {
    pub enabled: bool,
    pub frequency: String,
    pub time: String,
    pub weekday: u32,
    pub month_day: u32,
    pub keep: usize,
}
impl Default for BackupPlanConfig {
    fn default() -> Self { Self { enabled: false, frequency: "daily".into(), time: "03:00".into(), weekday: 0, month_day: 1, keep: 10 } }
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPlan {
    pub config: BackupPlanConfig,
    pub next_at: Option<i64>,
    pub last_run_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub state: String,
    pub message: String,
    pub files: Vec<String>,
}
// 保持既有 PostgreSQL 命令和调用方类型兼容。
pub type PostgresPlanConfig = BackupPlanConfig;
pub type PostgresPlan = BackupPlan;

fn plan_key(engine: &str, version: &str) -> Result<String> {
    if version.is_empty() || version.len() > 64 || !version.bytes().all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c)) {
        return Err(AppError::new("BAD_VERSION", "数据库版本无效"));
    }
    let prefix = if engine == "postgresql" { "postgres" } else { engine };
    Ok(format!("{prefix}BackupPlan@{version}"))
}
fn pg_key(version: &str) -> Result<String> { plan_key("postgresql", version) }
fn plan_lock(paths: &Paths, engine: &str, version: &str) -> Result<std::fs::File> {
    plan_key(engine, version)?;
    let folder = if engine == "postgresql" { "postgresql" } else { "db" };
    let dir = crate::paths::checked_data_path(&paths.base, &format!("backup/{folder}"))?;
    std::fs::create_dir_all(&dir)?;
    // PostgreSQL 沿用原锁名，保证升级期间与已有实例互斥。
    let suffix = if engine == "postgresql" { version.to_owned() } else { format!("{engine}-{version}") };
    let path = crate::paths::checked_data_path(&paths.base, &format!("backup/{folder}/.schedule-{suffix}.lock"))?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => AppError::new("BACKUP_BUSY", "该版本的自动备份正在执行，请等待完成后再修改计划"),
        std::fs::TryLockError::Error(error) => AppError::io("锁定自动备份计划", error),
    })?;
    Ok(file)
}
fn pg_plan_lock(paths: &Paths, version: &str) -> Result<std::fs::File> { plan_lock(paths, "postgresql", version) }
fn read_plan(store: &Store, engine: &str, version: &str) -> Result<BackupPlan> {
    match store.get_setting_checked(&plan_key(engine, version)?)? {
        None => Ok(BackupPlan { state: "idle".into(), ..Default::default() }),
        Some(json) => serde_json::from_str(&json).map_err(|error| AppError::internal("读取数据库备份计划", error.to_string())),
    }
}
fn read_pg_plan(store: &Store, version: &str) -> Result<BackupPlan> { read_plan(store, "postgresql", version) }
fn validate_plan(config: &BackupPlanConfig) -> Result<chrono::NaiveTime> {
    let time = chrono::NaiveTime::parse_from_str(&config.time, "%H:%M").ok();
    if !["daily", "weekly", "monthly"].contains(&config.frequency.as_str()) || config.time.len() != 5 || time.is_none()
        || config.weekday > 6 || !(1..=31).contains(&config.month_day) || config.keep > 100 {
        return Err(AppError::new("BAD_BACKUP_PLAN", "请选择每天、每周或每月计划、有效时间及 0–100 份保留数量"));
    }
    time.ok_or_else(|| AppError::new("BAD_BACKUP_PLAN", "备份时间无效"))
}
/// 本地日历调度：短月份落在月末；夏令时跳时向后找有效分钟，重复时刻只取第一次。
fn next_run<T: chrono::TimeZone>(config: &BackupPlanConfig, after: chrono::DateTime<T>) -> Result<i64> {
    use chrono::Datelike;
    let time = validate_plan(config)?;
    let timezone = after.timezone();
    for day in 0..=370 {
        let date = after.date_naive().checked_add_days(chrono::Days::new(day))
            .ok_or_else(|| AppError::new("BAD_BACKUP_TIME", "无法计算下一次备份时间"))?;
        if config.frequency == "weekly" && date.weekday().num_days_from_monday() != config.weekday { continue; }
        if config.frequency == "monthly" && date.day() != config.month_day {
            let tomorrow = date.succ_opt().ok_or_else(|| AppError::new("BAD_BACKUP_TIME", "备份日期超出范围"))?;
            if date.day() >= config.month_day || tomorrow.month() == date.month() { continue; }
        }
        for offset in 0..180 {
            let local = date.and_time(time) + chrono::Duration::minutes(offset);
            if local.date() != date { break; }
            if let Some(at) = timezone.from_local_datetime(&local).earliest() {
                if at.timestamp_millis() > after.timestamp_millis() { return Ok(at.timestamp_millis()); }
                break;
            }
        }
    }
    Err(AppError::new("BAD_BACKUP_TIME", "无法计算下一次备份时间，请检查系统时区"))
}

fn inspect_plan(state: &crate::CoreState, engine: &str, version: &str) -> Result<BackupPlan> {
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let mut plan = read_plan(&state.store, engine, version)?;
    if plan.state == "running" {
        match plan_lock(&state.paths, engine, version) {
            Ok(_lock) => {
                plan = read_plan(&state.store, engine, version)?;
                if plan.state == "running" { plan.state = "interrupted".into(); plan.message = "上次备份未完成，已生成的归档仍保留；请检查后立即执行一次".into(); }
            },
            Err(error) if error.code == "BACKUP_BUSY" => {},
            Err(error) => return Err(error),
        }
    }
    Ok(plan)
}
fn save_plan(state: &crate::CoreState, engine: &str, version: &str, config: BackupPlanConfig) -> Result<BackupPlan> {
    let _work = crate::BackgroundWork::begin("保存数据库备份计划")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    validate_plan(&config)?;
    if state.store.find_installed(engine, Some(version)).is_none() { return Err(AppError::not_installed("所选数据库版本")); }
    let _lock = plan_lock(&state.paths, engine, version)?;
    let mut plan = read_plan(&state.store, engine, version)?;
    if plan.state == "running" { plan.state = "interrupted".into(); }
    plan.next_at = if config.enabled { Some(next_run(&config, chrono::Local::now())?) } else { None };
    plan.config = config;
    state.store.set_setting_json(&plan_key(engine, version)?, &plan)?;
    Ok(plan)
}
pub fn postgres_plan(state: &crate::CoreState, version: &str) -> Result<BackupPlan> { inspect_plan(state, "postgresql", version) }
pub fn save_postgres_plan(state: &crate::CoreState, version: &str, config: BackupPlanConfig) -> Result<BackupPlan> { save_plan(state, "postgresql", version, config) }
pub fn database_plan(state: &crate::CoreState, engine: crate::dbadmin::DatabaseEngine, version: &str) -> Result<BackupPlan> { inspect_plan(state, engine.id(), version) }
pub fn save_database_plan(state: &crate::CoreState, engine: crate::dbadmin::DatabaseEngine, version: &str, config: BackupPlanConfig) -> Result<BackupPlan> { save_plan(state, engine.id(), version, config) }
fn rotate_pg_backups(paths: &Paths, version: &str, oid: u32, keep: usize, newest: &std::path::Path) -> Result<()> {
    use std::io::Read;
    if keep == 0 { return Ok(()); }
    static SUFFIX: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| regex::Regex::new(r"^.+-\d{8}-\d{6}-\d{1,20}\.dump$").unwrap());
    let prefix = format!("auto-postgresql-{version}-{oid}-");
    let mut files = Vec::new();
    for file in crate::dbbackup::postgres_list_backups(paths)? {
        if !file.name.strip_prefix(&prefix).is_some_and(|suffix| SUFFIX.is_match(suffix)) { continue; }
        let mut magic = [0; 5];
        if std::fs::File::open(&file.path)?.read_exact(&mut magic).is_ok() && &magic == b"PGDMP" { files.push(file); }
    }
    files.sort_by(|a,b| (std::path::Path::new(&b.path) == newest).cmp(&(std::path::Path::new(&a.path) == newest))
        .then_with(|| b.created_at.cmp(&a.created_at)).then_with(|| b.name.cmp(&a.name)));
    for file in files.into_iter().skip(keep) { crate::dbbackup::postgres_delete_backup(paths, &file.name)?; }
    Ok(())
}

/// 手动验证与到期调度共用路径。先持有操作系统锁，再读取计划，防止多窗口重复运行。
pub fn run_postgres_plan(state: &crate::CoreState, version: &str, manual: bool) -> Result<BackupPlan> {
    let _work = crate::BackgroundWork::begin("PostgreSQL 自动备份")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = pg_plan_lock(&state.paths, version)?;
    let mut plan = read_pg_plan(&state.store, version)?;
    let now = chrono::Local::now();
    if !manual && (!plan.config.enabled || !plan.next_at.is_some_and(|at| at <= now.timestamp_millis())) { return Ok(plan); }
    validate_plan(&plan.config)?;
    plan.next_at = if plan.config.enabled { Some(next_run(&plan.config, now)?) } else { None };
    plan.last_run_at = Some(now.timestamp_millis()); plan.finished_at = None;
    plan.state = "running".into(); plan.message.clear(); plan.files.clear();
    state.store.set_setting_json(&pg_key(version)?, &plan)?;
    let result = state.with_postgres(version, |client| {
        let databases = client.list_databases()?.into_iter().filter(|db| !db.protected && db.allow_connections).collect::<Vec<_>>();
        if databases.is_empty() { plan.state = "skipped".into(); plan.message = "没有可备份的业务数据库".into(); return Ok(()); }
        let mut errors = Vec::new();
        for db in &databases {
            match crate::dbbackup::postgres_dump_kind(&state.paths, client, version, &db.name, db.oid, true, &|_| {}) {
                Ok(path) => {
                    let name = path.file_name().ok_or_else(|| AppError::new("BAD_BACKUP_NAME", "备份文件名无效"))?.to_string_lossy().into_owned();
                    plan.files.push(name);
                    // 每生成一份即记录；进程中断后仍可查看已完成的归档。
                    state.store.set_setting_json(&pg_key(version)?, &plan)?;
                    if let Err(error) = rotate_pg_backups(&state.paths, version, db.oid, plan.config.keep, &path) {
                        errors.push(format!("{}：新备份已保留，清理旧自动备份失败：{}", db.name, error.message));
                    }
                },
                Err(error) => errors.push(format!("{}：{} {}", db.name, error.message, error.detail.unwrap_or_default())),
            }
        }
        plan.state = if errors.is_empty() { "success" } else if plan.files.is_empty() { "failed" } else { "partial" }.into();
        plan.message = format!("已备份 {} / {} 个业务数据库{}", plan.files.len(), databases.len(), if errors.is_empty() { String::new() } else { format!("。{}", errors.join("；")) });
        Ok(())
    });
    if let Err(error) = result { plan.state = "failed".into(); plan.message = format!("{} {}", error.message, error.hint.unwrap_or_default()); }
    plan.finished_at = Some(crate::services::now_ms());
    state.store.set_setting_json(&pg_key(version)?, &plan)?;
    Ok(plan)
}

fn rotate_database_backups(paths: &Paths, engine: crate::dbadmin::DatabaseEngine, version: &str, database: &str, keep: usize, newest: &std::path::Path) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    if keep == 0 { return Ok(()); }
    static SUFFIX: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| regex::Regex::new(r"^.+-\d{8}-\d{6}-\d{1,20}\.sql$").unwrap());
    let prefix = format!("{}-{version}-auto-{}-", engine.id(), crate::dbbackup::automatic_database_id(database));
    let marker = crate::dbbackup::automatic_marker(engine, version, database);
    let mut files = Vec::new();
    for file in crate::dbbackup::list_backups(paths)? {
        if !file.name.strip_prefix(&prefix).is_some_and(|suffix| SUFFIX.is_match(suffix)) || file.size_bytes <= marker.len() as u64 { continue; }
        let mut handle = std::fs::File::open(&file.path)?;
        handle.seek(SeekFrom::End(-(marker.len() as i64)))?;
        let mut tail = vec![0; marker.len()]; handle.read_exact(&mut tail)?;
        if tail == marker.as_bytes() { files.push(file); }
    }
    files.sort_by(|a,b| (std::path::Path::new(&b.path) == newest).cmp(&(std::path::Path::new(&a.path) == newest))
        .then_with(|| b.created_at.cmp(&a.created_at)).then_with(|| b.name.cmp(&a.name)));
    for file in files.into_iter().skip(keep) { crate::dbbackup::delete_backup(paths, &file.path)?; }
    Ok(())
}

pub fn run_database_plan(state: &crate::CoreState, engine: crate::dbadmin::DatabaseEngine, version: &str, manual: bool) -> Result<BackupPlan> {
    let _work = crate::BackgroundWork::begin("数据库自动备份")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = plan_lock(&state.paths, engine.id(), version)?;
    let mut plan = read_plan(&state.store, engine.id(), version)?;
    let now = chrono::Local::now();
    if !manual && (!plan.config.enabled || !plan.next_at.is_some_and(|at| at <= now.timestamp_millis())) { return Ok(plan); }
    validate_plan(&plan.config)?;
    plan.next_at = if plan.config.enabled { Some(next_run(&plan.config, now)?) } else { None };
    plan.last_run_at = Some(now.timestamp_millis()); plan.finished_at = None;
    plan.state = "running".into(); plan.message.clear(); plan.files.clear();
    state.store.set_setting_json(&plan_key(engine.id(), version)?, &plan)?;
    let result = state.with_database(engine, Some(version), |version, client| {
        let conn = crate::dbbackup::ConnInfo { engine, version: version.into(), port: client.port, root_password: client.root_password.clone(), bin_dir: client.exe.parent().map(std::path::Path::to_path_buf) };
        let databases = client.list_databases()?.into_iter().filter(|db| !crate::dbbackup::is_system_db(&db.name)).collect::<Vec<_>>();
        if databases.is_empty() { plan.state = "skipped".into(); plan.message = "没有可备份的业务数据库".into(); return Ok(()); }
        let mut errors = Vec::new();
        for db in &databases {
            match crate::dbbackup::dump_database_auto(&state.paths, &conn, &db.name) {
                Ok(path) => {
                    let name = path.file_name().ok_or_else(|| AppError::new("BAD_BACKUP_NAME", "备份文件名无效"))?.to_string_lossy().into_owned();
                    plan.files.push(name);
                    // 每生成一份即记录；进程中断后仍可查看已完成的归档。
                    state.store.set_setting_json(&plan_key(engine.id(), version)?, &plan)?;
                    if let Err(error) = rotate_database_backups(&state.paths, engine, version, &db.name, plan.config.keep, &path) {
                        errors.push(format!("{}：新备份已保留，清理旧自动备份失败：{}", db.name, error.message));
                    }
                },
                Err(error) => errors.push(format!("{}：{} {}", db.name, error.message, error.detail.unwrap_or_default())),
            }
        }
        plan.state = if errors.is_empty() { "success" } else if plan.files.is_empty() { "failed" } else { "partial" }.into();
        plan.message = format!("已备份 {} / {} 个业务数据库{}", plan.files.len(), databases.len(), if errors.is_empty() { String::new() } else { format!("。{}", errors.join("；")) });
        Ok(())
    });
    if let Err(error) = result { plan.state = "failed".into(); plan.message = format!("{} {}", error.message, error.hint.unwrap_or_default()); }
    plan.finished_at = Some(crate::services::now_ms());
    state.store.set_setting_json(&plan_key(engine.id(), version)?, &plan)?;
    Ok(plan)
}

pub(crate) fn tick_postgres(state: &crate::CoreState) {
    let Ok(_activity) = crate::paths::DataDirActivity::shared(&state.paths.base) else { return; };
    let Ok(installed) = state.store.list_installed() else { return; };
    for package in installed.into_iter().filter(|package| package.id == "postgresql") {
        if read_pg_plan(&state.store, &package.version).is_ok_and(|plan| plan.config.enabled && plan.next_at.is_some_and(|at| at <= crate::services::now_ms())) {
            let _ = run_postgres_plan(state, &package.version, false);
        }
    }
}

pub(crate) fn tick_databases(state: &crate::CoreState) {
    let Ok(_activity) = crate::paths::DataDirActivity::shared(&state.paths.base) else { return; };
    let Ok(installed) = state.store.list_installed() else { return; };
    for package in installed {
        let engine = match package.id.as_str() { "mysql" => crate::dbadmin::DatabaseEngine::Mysql, "mariadb" => crate::dbadmin::DatabaseEngine::Mariadb, _ => continue };
        if read_plan(&state.store, engine.id(), &package.version).is_ok_and(|plan| plan.config.enabled && plan.next_at.is_some_and(|at| at <= crate::services::now_ms())) {
            let _ = run_database_plan(state, engine, &package.version, false);
        }
    }
}

pub fn spawn_database_scheduler_when_ready(state: std::sync::Arc<crate::CoreState>, gate: std::sync::Arc<crate::restart::StartupGate>) -> Result<()> {
    std::thread::Builder::new().name("database-backup-scheduler".into()).spawn(move || {
        if !gate.wait() { return; }
        loop {
            std::thread::sleep(std::time::Duration::from_secs(30));
            tick_postgres(&state);
            tick_databases(&state);
        }
    }).map_err(|error| AppError::io("启动数据库备份调度", error))?;
    Ok(())
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
    fn postgres_calendar_retention_and_execution_locks() {
        use chrono::TimeZone;
        let mut config = BackupPlanConfig { enabled: true, ..Default::default() };
        let at = chrono::Utc.with_ymd_and_hms(2026, 1, 31, 3, 0, 0).unwrap();
        assert_eq!(next_run(&config, at).unwrap(), chrono::Utc.with_ymd_and_hms(2026, 2, 1, 3, 0, 0).unwrap().timestamp_millis());
        config.frequency = "monthly".into(); config.month_day = 31;
        assert_eq!(next_run(&config, at).unwrap(), chrono::Utc.with_ymd_and_hms(2026, 2, 28, 3, 0, 0).unwrap().timestamp_millis());
        let leap = chrono::Utc.with_ymd_and_hms(2028, 1, 31, 4, 0, 0).unwrap();
        assert_eq!(next_run(&config, leap).unwrap(), chrono::Utc.with_ymd_and_hms(2028, 2, 29, 3, 0, 0).unwrap().timestamp_millis());
        config.frequency = "weekly".into(); config.weekday = 0;
        assert_eq!(next_run(&config, at).unwrap(), chrono::Utc.with_ymd_and_hms(2026, 2, 2, 3, 0, 0).unwrap().timestamp_millis());
        config.time = "24:00".into(); assert!(validate_plan(&config).is_err());
        config.time = "03:00".into(); config.keep = 101; assert!(validate_plan(&config).is_err());
        assert!(pg_key("../../outside").is_err());
        let temp = tempfile::tempdir().unwrap();
        let state = crate::CoreState::init(Some(temp.path().to_path_buf()), std::sync::Arc::new(|_| {})).unwrap();
        let dir = crate::dbbackup::postgres_backup_dir(&state.paths).unwrap(); std::fs::create_dir_all(&dir).unwrap();
        for name in ["auto-postgresql-16.6-100-project-20260928-030000-0.dump", "auto-postgresql-16.6-100-project-20260928-030000-1.dump", "auto-postgresql-16.6-100-project-20260928-030000-2.dump", "auto-postgresql-16.6-100-notes.dump", "postgresql-16.6-project-manual.dump", "auto-postgresql-16.6-101-other.dump", "auto-postgresql-16.7-100-project.dump"] {
            std::fs::write(dir.join(name), "PGDMPfixture").unwrap();
        }
        let corrupt = dir.join("auto-postgresql-16.6-100-corrupt.dump"); std::fs::write(&corrupt, "broken").unwrap();
        let current = dir.join("auto-postgresql-16.6-100-project-20260928-030000-0.dump");
        rotate_pg_backups(&state.paths, "16.6", 100, 1, &current).unwrap();
        assert!(current.exists() && corrupt.exists());
        assert!(dir.join("auto-postgresql-16.6-100-notes.dump").exists());
        assert!(!dir.join("auto-postgresql-16.6-100-project-20260928-030000-1.dump").exists());
        assert!(!dir.join("auto-postgresql-16.6-100-project-20260928-030000-2.dump").exists());
        assert!(dir.join("postgresql-16.6-project-manual.dump").exists());
        assert!(dir.join("auto-postgresql-16.6-101-other.dump").exists());
        assert!(dir.join("auto-postgresql-16.7-100-project.dump").exists());
        assert_eq!(run_postgres_plan(&state, "16.6", false).unwrap().last_run_at, None);
        let plan = BackupPlan { state: "running".into(), ..Default::default() };
        state.store.set_setting_json(&pg_key("16.6").unwrap(), &plan).unwrap();
        let held = pg_plan_lock(&state.paths, "16.6").unwrap();
        assert_eq!(postgres_plan(&state, "16.6").unwrap().state, "running");
        assert_eq!(run_postgres_plan(&state, "16.6", true).unwrap_err().code, "BACKUP_BUSY");
        drop(held);
        assert_eq!(postgres_plan(&state, "16.6").unwrap().state, "interrupted");
        assert_eq!(run_postgres_plan(&state, "16.6", true).unwrap().state, "failed");
        for engine in [crate::dbadmin::DatabaseEngine::Mysql, crate::dbadmin::DatabaseEngine::Mariadb] {
            let version = "8.0.46";
            let key = plan_key(engine.id(), version).unwrap();
            assert_ne!(key, pg_key(version).unwrap());
            let running = BackupPlan { state: "running".into(), ..Default::default() };
            state.store.set_setting_json(&key, &running).unwrap();
            let held = plan_lock(&state.paths, engine.id(), version).unwrap();
            assert_eq!(database_plan(&state, engine, version).unwrap().state, "running");
            assert_eq!(run_database_plan(&state, engine, version, true).unwrap_err().code, "BACKUP_BUSY");
            assert!(plan_lock(&state.paths, "postgresql", version).is_ok());
            drop(held);
            assert_eq!(database_plan(&state, engine, version).unwrap().state, "interrupted");
            assert_eq!(run_database_plan(&state, engine, version, true).unwrap().state, "failed");
            state.store.set_setting(&key, "broken").unwrap();
            assert!(database_plan(&state, engine, version).is_err());
            let dir = crate::dbbackup::backup_dir(&state.paths);
            let db = "a/b";
            let hash = crate::dbbackup::automatic_database_id(db);
            assert_ne!(hash, crate::dbbackup::automatic_database_id("a_b"));
            assert_ne!(hash, crate::dbbackup::automatic_database_id("A/b"));
            let prefix = format!("{}-{version}-auto-{hash}-", engine.id());
            let current = dir.join(format!("{prefix}a_b-20260928-030000-0.sql"));
            let old = dir.join(format!("{prefix}a_b-20990101-030000-1.sql"));
            let unrelated = dir.join(format!("{prefix}a_b-20260928-030000-2.sql"));
            let content = format!("SELECT 1;{}", crate::dbbackup::automatic_marker(engine, version, db));
            std::fs::write(&current, &content).unwrap(); std::fs::write(&old, &content).unwrap();
            std::fs::write(&unrelated, "SELECT 'manual';").unwrap();
            rotate_database_backups(&state.paths, engine, version, db, 0, &current).unwrap();
            assert!(old.is_file());
            rotate_database_backups(&state.paths, engine, version, db, 1, &current).unwrap();
            assert!(current.is_file() && unrelated.is_file()); assert!(!old.exists());
        }
        state.store.set_setting(&pg_key("16.6").unwrap(), "broken").unwrap();
        assert!(postgres_plan(&state, "16.6").is_err());
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
