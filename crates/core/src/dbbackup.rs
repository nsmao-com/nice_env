//! 数据库备份 / 还原（mysqldump / mysql）。
//!
//! phpStudy、ServBay 都有这块，而且是被用得很频繁的功能——改数据前先导一份、
//! 换机器时搬过去。这里做成「导出到文件 / 从文件还原」，并把几件容易出事的事
//! 处理掉：
//!
//! - **备份前先确认服务在跑**，否则 mysqldump 连不上，报错信息还很难懂；
//! - **导出成 .sql 时带上建库语句**（`--databases`），还原时不必先手工建库；
//! - **还原前自动备份当前状态**，因为还原是破坏性的、且不可撤销；
//! - **进度可观测**：按实际写出的字节数回传；恢复期间不显示虚构的百分比。
//! - **密码不走命令行**：用临时 defaults-file 传，避免出现在进程列表里
//!   （Windows 上任何用户都能看到别人的命令行）。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::error::{AppError, Result};
use crate::model::{DbBackupFile, DbBackupProgress};
use crate::paths::Paths;
use crate::dbadmin::DatabaseEngine;

/// 备份文件的落盘目录：{base}/backup/db/
pub fn backup_dir(paths: &Paths) -> PathBuf {
    paths.backup().join("db")
}

pub fn dump_path(paths: &Paths, version: &str, name: &str) -> Result<PathBuf> {
    dump_path_for(paths, DatabaseEngine::Mysql, version, name)
}

pub fn dump_path_for(paths: &Paths, engine: DatabaseEngine, version: &str, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) || !name.to_ascii_lowercase().ends_with(".sql")
    {
        return Err(AppError::new(
            "BAD_BACKUP_NAME",
            "备份名称必须是单个 SQL 文件名",
        ));
    }
    crate::paths::checked_data_path(&paths.base, &format!("backup/db/{}-{version}-{name}", engine.id()))
        .map_err(Into::into)
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 时间戳：20260921-203045
fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

fn unique_stamp() -> String {
    format!(
        "{}-{}",
        stamp(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )
}

/// 原始库名的完整摘要隔离轮转，避免字符清洗或大小写折叠合并不同数据库。
pub(crate) fn automatic_database_id(database: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(database.as_bytes()))
}
pub(crate) fn automatic_marker(engine: DatabaseEngine, version: &str, database: &str) -> String {
    format!("\n-- NiceEnv automatic backup v1 {} {version} {}\n", engine.id(), automatic_database_id(database))
}
pub(crate) fn dump_database_auto(paths: &Paths, conn: &ConnInfo, database: &str) -> Result<PathBuf> {
    let label: String = sanitize(database).chars().take(16).collect();
    let name = format!("auto-{}-{label}-{}.sql", automatic_database_id(database), unique_stamp());
    let path = dump_path_for(paths, conn.engine, &conn.version, &name)?;
    dump_databases_kind(paths, conn, &[database.into()], &path, true, &|_| {})?;
    Ok(path)
}

fn tool_path(paths: &Paths, conn: &ConnInfo, tool: &str) -> Result<PathBuf> {
    let bin = conn.resolved_bin_dir(paths)?;
    Ok(crate::dbadmin::database_tool(&bin, conn.engine, tool))
}

/// 备份/还原共用的参数
#[derive(Debug, Clone)]
pub struct ConnInfo {
    pub engine: DatabaseEngine,
    pub version: String,
    pub port: u16,
    pub root_password: String,
    pub bin_dir: Option<PathBuf>,
}

impl ConnInfo {
    pub(crate) fn resolved_bin_dir(&self, paths: &Paths) -> Result<PathBuf> {
        if let Some(bin) = &self.bin_dir { return Ok(bin.clone()); }
        if self.engine == DatabaseEngine::Mariadb {
            return Err(AppError::new("DATABASE_TOOL_MISSING", "缺少所选 MariaDB 安装的客户端路径"));
        }
        Ok(paths.runtime_dir("mysql", &self.version).join(crate::ops::mysql_root_name(&self.version)).join("bin"))
    }
}

/// MySQL 8+ 默认采集的直方图信息不适用于 5.7/MariaDB 来源。
/// 5.7 客户端没有 column-statistics 选项，因此只为支持的客户端关闭它。
pub(crate) fn dump_options(command: &mut std::process::Command, engine: DatabaseEngine, version: &str) {
    command.args([
        "--single-transaction",
        "--routines",
        "--triggers",
        "--events",
        "--hex-blob",
        "--no-tablespaces",
    ]);
    if engine == DatabaseEngine::Mysql { command.arg("--set-gtid-purged=OFF"); }
    if engine == DatabaseEngine::Mysql && version
        .split('.')
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|v| v >= 8)
    {
        command.arg("--column-statistics=0");
    }
    command.args(["--databases", "--"]);
}

/// 导出单个或多个数据库到 .sql。
///
/// `progress` 会被周期性回调，前端据此显示「已写出 N MB」。
/// 之所以按字节而不是按表回调：mysqldump 输出是流式的，靠解析表名估算
/// 反而不准，直接看写出去多少字节最实在。
pub fn dump_databases(
    paths: &Paths,
    conn: &ConnInfo,
    databases: &[String],
    out_path: &Path,
    progress: &dyn Fn(DbBackupProgress),
) -> Result<u64> {
    dump_databases_kind(paths, conn, databases, out_path, false, progress)
}

fn dump_databases_kind(
    paths: &Paths,
    conn: &ConnInfo,
    databases: &[String],
    out_path: &Path,
    automatic: bool,
    progress: &dyn Fn(DbBackupProgress),
) -> Result<u64> {
    if databases.is_empty() {
        return Err(AppError::new("NO_DATABASE", "没有选择要备份的数据库"));
    }
    if databases.iter().any(|name| {
        name.is_empty()
            || name.starts_with('-')
            || name.chars().any(char::is_control)
            || is_system_db(name)
    }) {
        return Err(AppError::new("BAD_DATABASE", "只能备份有效的业务数据库"));
    }
    let dump = tool_path(paths, conn, "mysqldump")?;
    if !dump.is_file() {
        return Err(AppError::new(
            "MYSQL_TOOL_MISSING",
            "找不到 mysqldump，无法备份",
        ));
    }
    let client = crate::dbadmin::MySqlClient {
        exe: tool_path(paths, conn, "mysql")?,
        port: conn.port,
        root_password: conn.root_password.clone(),
    };
    let available = client.list_databases()?;
    if databases
        .iter()
        .any(|name| !available.iter().any(|db| &db.name == name))
    {
        return Err(AppError::new(
            "NO_DATABASE",
            "所选数据库已不存在，请刷新列表",
        ));
    }
    let parent = out_path
        .parent()
        .ok_or_else(|| AppError::new("BAD_PATH", "备份目录无效"))?;
    std::fs::create_dir_all(parent)?;
    if out_path.exists() {
        return Err(AppError::new(
            "BACKUP_EXISTS",
            "同名备份已存在，未覆盖原文件",
        ));
    }
    let mut pending = tempfile::Builder::new()
        .prefix(".dump-")
        .tempfile_in(parent)?;
    let mut error = tempfile::tempfile()?;
    let (_private, mut command) =
        crate::dbadmin::client_command(&dump, "127.0.0.1", conn.port, "root", &conn.root_password)?;
    dump_options(&mut command, conn.engine, &conn.version);
    command
        .args(databases)
        .stdin(Stdio::null())
        .stdout(pending.as_file().try_clone()?)
        .stderr(error.try_clone()?);
    let mut last = std::time::Instant::now();
    let status = crate::dbadmin::wait_client(&mut command, Duration::from_secs(1800), || {
        if last.elapsed() >= Duration::from_millis(200) {
            progress(DbBackupProgress {
                database: databases.join(", "),
                bytes: pending.as_file().metadata().map(|m| m.len()).unwrap_or(0),
                total: None,
                state: "running".into(),
                message: None,
            });
            last = std::time::Instant::now();
        }
    })?;
    if !status.success() {
        let detail = crate::dbadmin::read_output(&mut error, 64 * 1024)?;
        let detail = if conn.root_password.is_empty() {
            detail
        } else {
            detail.replace(&conn.root_password, "***")
        };
        return Err(
            AppError::new("DUMP_FAILED", "导出失败，未发布不完整的备份文件").with_detail(detail),
        );
    }
    let written = pending.as_file().metadata()?.len();
    if written == 0 {
        return Err(AppError::new("DUMP_FAILED", "导出内容为空，未发布备份文件"));
    }
    if automatic {
        use std::io::{Seek, SeekFrom, Write};
        pending.as_file_mut().seek(SeekFrom::End(0))?;
        pending.write_all(automatic_marker(conn.engine, &conn.version, &databases[0]).as_bytes())?;
    }
    pending.as_file().sync_all()?;
    let written = pending.as_file().metadata()?.len();
    pending
        .persist_noclobber(out_path)
        .map_err(|e| AppError::io("发布数据库备份", e.error))?;
    progress(DbBackupProgress {
        database: databases.join(", "),
        bytes: written,
        total: Some(written),
        state: "done".into(),
        message: None,
    });
    Ok(written)
}

/// 从 .sql 还原。
///
/// `safety_backup` 为 true 时，先把当前所有库整体导出一份放到备份目录，
/// 这样用户点错了还能回去——还原是不可撤销操作，这一步值得。
pub fn restore_from_file(
    paths: &Paths,
    conn: &ConnInfo,
    sql_path: &Path,
    safety_backup: bool,
    progress: &dyn Fn(DbBackupProgress),
) -> Result<Option<PathBuf>> {
    restore_from_file_into(paths, conn, sql_path, None, safety_backup, progress)
}

/// database 仅指定未写 USE 的语句所用的默认库；文件内的 USE/限定库名仍由 MySQL 执行。
pub fn restore_from_file_into(
    paths: &Paths,
    conn: &ConnInfo,
    sql_path: &Path,
    database: Option<&str>,
    safety_backup: bool,
    progress: &dyn Fn(DbBackupProgress),
) -> Result<Option<PathBuf>> {
    if !sql_path.is_file()
        || !sql_path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("sql"))
    {
        return Err(AppError::new("FILE_NOT_FOUND", "请选择有效的 SQL 备份文件"));
    }
    let file = std::fs::File::open(sql_path).map_err(|e| AppError::io("打开备份文件", e))?;
    let total = file.metadata()?.len();
    if total == 0 {
        return Err(AppError::new("EMPTY_BACKUP", "备份文件为空，未执行恢复"));
    }
    let mysql = tool_path(paths, conn, "mysql")?;
    if !mysql.is_file() {
        return Err(AppError::new(
            "MYSQL_TOOL_MISSING",
            "找不到 mysql 客户端，无法还原",
        ));
    }
    let client = crate::dbadmin::MySqlClient {
        exe: mysql.clone(), port: conn.port, root_password: conn.root_password.clone(),
    };
    let databases = if database.is_some() || safety_backup { client.list_databases()? } else { Vec::new() };
    if let Some(database) = database {
        if is_system_db(database) || !databases.iter().any(|db| db.name == database) {
            return Err(AppError::new("RESTORE_DATABASE_INVALID", "请选择当前实例中已存在的业务数据库，不能恢复到系统库"));
        }
    }
    let mut safety = None;
    if safety_backup {
        let dbs = databases.into_iter().filter(|db| !is_system_db(&db.name)).map(|db| db.name).collect::<Vec<_>>();
        if !dbs.is_empty() {
            let path = dump_path_for(
                paths,
                conn.engine,
                &conn.version,
                &format!("pre-restore-{}.sql", unique_stamp()),
            )?;
            dump_databases(paths, conn, &dbs, &path, &|mut state| {
                state.message = Some("正在创建恢复前备份".into());
                state.state = "running".into();
                progress(state);
            })
            .map_err(|error| {
                AppError::new(
                    "SAFETY_BACKUP_FAILED",
                    "恢复前备份失败，已中止恢复，原数据库未改动",
                )
                .with_detail(error.message)
            })?;
            safety = Some(path);
        }
    }
    let (_private, mut command) = crate::dbadmin::client_command(
        &mysql,
        "127.0.0.1",
        conn.port,
        "root",
        &conn.root_password,
    )?;
    if let Some(database) = database { command.arg(format!("--database={database}")); }
    let mut error = tempfile::tempfile()?;
    command
        .args(["--binary-mode", "--local-infile=0", "--connect-timeout=5"])
        .stdin(Stdio::from(file))
        .stdout(Stdio::null())
        .stderr(error.try_clone()?);
    let label = sql_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    progress(DbBackupProgress {
        database: label.clone(),
        bytes: 0,
        total: None,
        state: "running".into(),
        message: Some("正在执行 SQL，耗时取决于数据量".into()),
    });
    let result = crate::dbadmin::wait_client(&mut command, Duration::from_secs(1800), || {});
    if !result.as_ref().is_ok_and(|status| status.success()) {
        let detail = match result {
            Ok(_) => crate::dbadmin::read_output(&mut error, 64 * 1024)?,
            Err(error) => error.message,
        };
        let detail = if conn.root_password.is_empty() {
            detail
        } else {
            detail.replace(&conn.root_password, "***")
        };
        return Err(AppError::new(
            "RESTORE_FAILED",
            "还原未完成，部分 SQL 可能已执行，请先检查数据库",
        )
        .with_hint(
            safety
                .as_ref()
                .map(|p| format!("恢复前备份：{}", p.display()))
                .unwrap_or_else(|| "当前操作没有可用的恢复前备份".into()),
        )
        .with_detail(detail));
    }
    progress(DbBackupProgress {
        database: label,
        bytes: total,
        total: Some(total),
        state: "done".into(),
        message: safety
            .as_ref()
            .map(|p| format!("恢复前备份：{}", p.display())),
    });
    Ok(safety)
}

/// MySQL 自带的系统库，不该出现在备份范围里
pub fn is_system_db(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "information_schema" | "performance_schema" | "mysql" | "sys"
    )
}

/// 列出备份目录里的 .sql 文件（按修改时间倒序）
pub fn list_backups(paths: &Paths) -> Result<Vec<DbBackupFile>> {
    let dir = crate::paths::checked_data_path(&paths.base, "backup/db")?;
    let mut out: Vec<DbBackupFile> = Vec::new();
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(error) => return Err(error.into()),
    };
    for e in rd {
        let e = e?;
        if !e.file_type()?.is_file() {
            continue;
        }
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sql") {
            continue;
        }
        let meta = e.metadata()?;
        let created_at = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(DbBackupFile {
            name: path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            path: path.to_string_lossy().to_string(),
            size_bytes: meta.len(),
            created_at,
        });
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(out)
}

/// 删除一个备份文件（只允许删备份目录里的，避免被当成任意文件删除接口）
pub fn delete_backup(paths: &Paths, path: &str) -> Result<()> {
    let target = std::path::Path::new(path);
    let dir = backup_dir(paths);
    let relative = target
        .strip_prefix(&paths.base)
        .map_err(|_| AppError::new("FORBIDDEN", "只能删除备份目录内的文件"))?;
    crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
    if !target
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("sql"))
    {
        return Err(AppError::new("FORBIDDEN", "只能删除 SQL 备份文件"));
    }
    // 规范化后必须仍在备份目录内 —— 防止 ../../ 之类的路径穿越
    let canon_target = target
        .canonicalize()
        .map_err(|e| AppError::io("定位备份文件", e))?;
    let canon_dir = dir
        .canonicalize()
        .map_err(|e| AppError::io("定位备份目录", e))?;
    if !canon_target.starts_with(&canon_dir) {
        return Err(AppError::new("FORBIDDEN", "只能删除备份目录内的文件"));
    }
    std::fs::remove_file(&canon_target).map_err(|e| AppError::io("删除备份", e))
}

/// 给一个默认的备份文件名
pub fn default_dump_name(databases: &[String]) -> String {
    let label = if databases.len() == 1 {
        sanitize(&databases[0])
    } else {
        format!("{}dbs", databases.len())
    };
    format!("{label}-{}.sql", unique_stamp())
}

/// PostgreSQL 使用独立目录与 custom archive，不混入 MySQL/MariaDB 的 SQL 列表。
pub fn postgres_backup_dir(paths: &Paths) -> Result<PathBuf> {
    Ok(crate::paths::checked_data_path(&paths.base, "backup/postgresql")?)
}

fn postgres_backup_path(paths: &Paths, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) || !name.ends_with(".dump") {
        return Err(AppError::new("BAD_BACKUP_NAME", "请选择 PostgreSQL 备份目录内的 .dump 文件"));
    }
    Ok(crate::paths::checked_data_path(&paths.base, &format!("backup/postgresql/{name}"))?)
}

pub fn postgres_list_backups(paths: &Paths) -> Result<Vec<DbBackupFile>> {
    let dir = postgres_backup_dir(paths)?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() { continue; }
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".dump") { continue; }
        let path = postgres_backup_path(paths, &name)?;
        let metadata = entry.metadata()?;
        files.push(DbBackupFile { name, path: path.to_string_lossy().into(), size_bytes: metadata.len(),
            created_at: metadata.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|time| time.as_secs() as i64).unwrap_or(0) });
    }
    files.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.name.cmp(&b.name)));
    Ok(files)
}

pub fn postgres_delete_backup(paths: &Paths, name: &str) -> Result<()> {
    let path = postgres_backup_path(paths, name)?;
    if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
        return Err(AppError::new("BAD_BACKUP_FILE", "只能删除普通 PostgreSQL 备份文件"));
    }
    std::fs::remove_file(path).map_err(Into::into)
}

fn postgres_tool_result(
    command: &mut std::process::Command, error: &mut std::fs::File, client: &crate::dbadmin::PostgresClient,
    message: &str, progress: impl FnMut(),
) -> Result<()> {
    let result = crate::dbadmin::wait_client(command, Duration::from_secs(1800), progress);
    if result.as_ref().is_ok_and(|status| status.success()) { return Ok(()); }
    let detail = match result {
        Ok(_) => crate::dbadmin::read_output(error, 64 * 1024)?,
        Err(error) => error.message,
    };
    let detail = if client.password.is_empty() { detail } else { detail.replace(&client.password, "***") };
    Err(AppError::new("POSTGRES_BACKUP_FAILED", message).with_detail(detail))
}

pub fn postgres_dump(
    paths: &Paths, client: &crate::dbadmin::PostgresClient, version: &str, database: &str, oid: u32,
    progress: &dyn Fn(DbBackupProgress),
) -> Result<PathBuf> {
    postgres_dump_kind(paths, client, version, database, oid, false, progress)
}

pub(crate) fn postgres_dump_kind(
    paths: &Paths, client: &crate::dbadmin::PostgresClient, version: &str, database: &str, oid: u32,
    automatic: bool, progress: &dyn Fn(DbBackupProgress),
) -> Result<PathBuf> {
    let target = client.list_databases()?.into_iter().find(|db| db.name == database && db.oid == oid)
        .ok_or_else(|| AppError::new("POSTGRES_TARGET_CHANGED", "数据库已变化，请刷新后重新选择"))?;
    if target.protected || !target.allow_connections {
        return Err(AppError::new("POSTGRES_PROTECTED", "请选择可连接的业务数据库进行备份"));
    }
    let dir = postgres_backup_dir(paths)?;
    std::fs::create_dir_all(&dir)?;
    let prefix = if automatic { format!("auto-postgresql-{}-{oid}", sanitize(version)) } else { format!("postgresql-{}", sanitize(version)) };
    let path = postgres_backup_path(paths, &format!("{prefix}-{}-{}.dump", sanitize(database), unique_stamp()))?;
    let pending = tempfile::Builder::new().prefix(".dump-").tempfile_in(&dir)?;
    let mut error = tempfile::tempfile()?;
    let (_private, mut command) = client.tool_command("pg_dump", database, None)?;
    command.args(["--format=custom", "--lock-wait-timeout=5000"])
        .env("PGOPTIONS", "-c statement_timeout=1800000 -c lock_timeout=5000")
        .stdin(Stdio::null()).stdout(pending.as_file().try_clone()?).stderr(error.try_clone()?);
    let mut last = std::time::Instant::now();
    postgres_tool_result(&mut command, &mut error, client, "PostgreSQL 导出失败，未发布不完整的备份文件", || {
        if last.elapsed() >= Duration::from_millis(250) {
            progress(DbBackupProgress { database: database.into(), bytes: pending.as_file().metadata().map(|m| m.len()).unwrap_or(0), total: None, state: "running".into(), message: None });
            last = std::time::Instant::now();
        }
    })?;
    pending.as_file().sync_all()?;
    let bytes = pending.as_file().metadata()?.len();
    if bytes < 5 { return Err(AppError::new("POSTGRES_BACKUP_FAILED", "导出内容为空，未发布备份文件")); }
    pending.persist_noclobber(&path).map_err(|error| AppError::io("发布 PostgreSQL 备份", error.error))?;
    progress(DbBackupProgress { database: database.into(), bytes, total: Some(bytes), state: "done".into(), message: None });
    Ok(path)
}

/// 使用选中文件的同一份快照进行预检与恢复，避免选择后被替换；只创建新库。
pub fn postgres_restore(
    paths: &Paths, client: &crate::dbadmin::PostgresClient, source: &Path, database: &str, owner: &str, trusted: bool,
    progress: &dyn Fn(DbBackupProgress),
) -> Result<()> {
    postgres_restore_staged(paths, client, source, database, owner, trusted, progress).map(|_| ())
}

fn postgres_restore_staged(
    paths: &Paths, client: &crate::dbadmin::PostgresClient, source: &Path, database: &str, owner: &str, trusted: bool,
    progress: &dyn Fn(DbBackupProgress),
) -> Result<crate::dbadmin::PostgresDatabaseInfo> {
    use std::io::{Read, Seek};
    if !trusted { return Err(AppError::new("POSTGRES_BACKUP_UNTRUSTED", "请先确认备份来源可信；恢复会执行归档中的数据库代码")); }
    if !source.is_absolute() || !source.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("dump")) {
        return Err(AppError::new("BAD_BACKUP_FILE", "请选择 pg_dump custom 格式的 .dump 文件"));
    }
    let mut input = std::fs::File::open(source)?;
    if !input.metadata()?.is_file() { return Err(AppError::new("BAD_BACKUP_FILE", "备份必须是普通文件")); }
    let dir = postgres_backup_dir(paths)?;
    std::fs::create_dir_all(&dir)?;
    let mut snapshot = tempfile::Builder::new().prefix(".restore-").tempfile_in(&dir)?;
    progress(DbBackupProgress { database: database.into(), bytes: 0, total: None, state: "running".into(), message: Some("正在读取并校验备份".into()) });
    std::io::copy(&mut input, snapshot.as_file_mut())?;
    snapshot.as_file_mut().rewind()?;
    let mut magic = [0u8; 5];
    if snapshot.as_file_mut().read_exact(&mut magic).is_err() || magic != *b"PGDMP" {
        return Err(AppError::new("BAD_BACKUP_FILE", "文件不是 PostgreSQL custom 归档；未创建或修改数据库"));
    }
    snapshot.as_file_mut().rewind()?;
    let mut error = tempfile::tempfile()?;
    let mut preflight = platform::command(client.exe.with_file_name(crate::ops::exe_name("pg_restore")));
    preflight.args(["--list", "--format=custom"]).stdin(snapshot.as_file().try_clone()?).stdout(Stdio::null()).stderr(error.try_clone()?);
    postgres_tool_result(&mut preflight, &mut error, client, "备份无法由当前版本读取，未创建数据库；请检查文件及 PostgreSQL 版本", || {})?;
    // 将角色作为 UTF-8 连接选项传递，避免 Windows pg_restore 的 --role 参数经本地编码损坏。
    let role = owner.replace('\\', "\\\\").replace(' ', "\\ ");
    let options = format!("-c statement_timeout=1800000 -c lock_timeout=5000 -c role={role}");
    let (_private, mut command) = client.tool_command("pg_restore", database, Some(&options))?;
    // 不恢复源账号、ACL 或表空间；对象由选择的所有者拥有。归档仍必须来自可信来源。
    command.args(["--format=custom", "--single-transaction", "--exit-on-error", "--no-owner", "--no-privileges", "--no-tablespaces"]);
    snapshot.as_file_mut().rewind()?;
    let mut error = tempfile::tempfile()?;
    command.stdin(snapshot.as_file().try_clone()?).stdout(Stdio::null()).stderr(error.try_clone()?);
    let bytes = snapshot.as_file().metadata()?.len();
    client.create_database(database, owner)?;
    let staged = client.list_databases()?.into_iter().find(|db| db.name == database)
        .ok_or_else(|| AppError::new("POSTGRES_TARGET_CHANGED", "新建的数据库已变化，未开始恢复"))?;
    progress(DbBackupProgress { database: database.into(), bytes: 0, total: None, state: "running".into(), message: Some("正在恢复到新数据库，耗时取决于数据量".into()) });
    postgres_tool_result(&mut command, &mut error, client, "PostgreSQL 恢复未完成", || {})
        .map_err(|error| error.with_hint(format!("新建数据库“{database}”已保留，请检查后处理。恢复使用单个事务，未自动删除数据库；重试时请选择另一个新库名称。")))?;
    progress(DbBackupProgress { database: database.into(), bytes, total: Some(bytes), state: "done".into(), message: None });
    Ok(staged)
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostgresReplaceInput {
    pub path: String,
    pub name: String,
    pub oid: u32,
    pub owner: String,
    pub confirmed_name: String,
    pub trusted: bool,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostgresReplaceResult {
    pub database: String,
    pub previous_database: String,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub safety_backup: String,
}

/// 两次重命名在同一事务内完成。RENAME 持有数据库排他锁直到提交，随后核对 OID；
/// 名称被外部 SQL 替换、目标被占用或第二次重命名失败都会使事务回滚。
pub(crate) fn postgres_swap_restored(
    client: &crate::dbadmin::PostgresClient, target: &crate::dbadmin::PostgresDatabaseInfo,
    staged: &crate::dbadmin::PostgresDatabaseInfo, previous_name: &str,
) -> Result<()> {
    let ident = crate::dbadmin::postgres_ident;
    let literal = |name: &str| format!("E'{}'", name.replace('\\', "\\\\").replace('\'', "''"));
    let guard = |oid: u32, name: &str| format!("DO {};", literal(&format!("BEGIN IF NOT EXISTS (SELECT 1 FROM pg_database WHERE oid={oid} AND datname={} AND NOT datistemplate AND datallowconn) OR EXISTS (SELECT 1 FROM pg_subscription WHERE subdbid={oid}) OR EXISTS (SELECT 1 FROM pg_replication_slots WHERE datoid={oid}) THEN RAISE EXCEPTION 'NICEENV_TARGET_CHANGED_OR_REPLICATION'; END IF; END", literal(name))));
    if target.protected || staged.protected || !target.allow_connections || !staged.allow_connections || target.oid == staged.oid {
        return Err(AppError::new("POSTGRES_PROTECTED", "不能替换系统库、模板库或不可连接的数据库"));
    }
    let sql = format!(
        "BEGIN;\nALTER DATABASE {} RENAME TO {};\n{}\nALTER DATABASE {} RENAME TO {};\n{}\nCOMMIT;",
        ident(&target.name)?, ident(previous_name)?, guard(target.oid, previous_name),
        ident(&staged.name)?, ident(&target.name)?, guard(staged.oid, &target.name),
    );
    let switched = client.query(&sql);
    // 提交附近断连时不能凭客户端退出码断定回滚；重新读目录核实最终状态。
    let databases = client.list_databases();
    if databases.as_ref().is_ok_and(|dbs| dbs.iter().any(|db| db.name == target.name && db.oid == staged.oid)
        && dbs.iter().any(|db| db.name == previous_name && db.oid == target.oid)) {
        return Ok(());
    }
    let original_retained = databases.as_ref().is_ok_and(|dbs| dbs.iter().any(|db| db.name == target.name && db.oid == target.oid));
    let detail = switched.err().or_else(|| databases.err()).map(|error| error.detail.unwrap_or(error.message)).unwrap_or_else(|| "数据库名称或标识与预期不一致".into());
    Err(AppError::new(if original_retained { "POSTGRES_REPLACE_FAILED" } else { "POSTGRES_REPLACE_UNCONFIRMED" }, if original_retained { "数据库替换失败，已核对原数据库仍在原名称下" } else { "未能确认数据库替换结果，请刷新核对后再操作" })
        .with_hint(format!("请先关闭业务连接；不会强制断开连接或自动删除数据库。请核对原名称“{}”、暂存库“{}”和保留库“{previous_name}”。", target.name, staged.name))
        .with_detail(detail))
}

pub fn postgres_replace_from_file(
    paths: &Paths, client: &crate::dbadmin::PostgresClient, version: &str, input: &PostgresReplaceInput,
    progress: &dyn Fn(DbBackupProgress),
) -> Result<PostgresReplaceResult> {
    if !input.trusted { return Err(AppError::new("POSTGRES_BACKUP_UNTRUSTED", "请先确认备份来源可信")); }
    if input.name != input.confirmed_name { return Err(AppError::new("POSTGRES_CONFIRM_NAME", "请输入要替换的完整数据库名称")); }
    let target = client.list_databases()?.into_iter().find(|db| db.name == input.name && db.oid == input.oid)
        .ok_or_else(|| AppError::new("POSTGRES_TARGET_CHANGED", "目标数据库已变化，请刷新后重新确认"))?;
    if target.protected || !target.allow_connections { return Err(AppError::new("POSTGRES_PROTECTED", "只能替换可连接的业务数据库")); }
    if !client.list_roles()?.iter().any(|role| role.name == input.owner && role.can_login) {
        return Err(AppError::new("POSTGRES_OWNER_CHANGED", "所选所有者不存在或无法登录，请重新选择"));
    }
    let busy = client.query(&format!("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datid={}) OR EXISTS (SELECT 1 FROM pg_prepared_xacts WHERE database=(SELECT datname FROM pg_database WHERE oid={}));", target.oid, target.oid))?;
    if busy.trim() != "f" { return Err(AppError::new("POSTGRES_DATABASE_BUSY", "目标数据库仍有连接或预备事务，请先处理后重试；不会强制断开连接")); }
    let replication = client.query(&format!("SELECT EXISTS (SELECT 1 FROM pg_subscription WHERE subdbid={}) OR EXISTS (SELECT 1 FROM pg_replication_slots WHERE datoid={});", target.oid, target.oid))?;
    if replication.trim() != "f" {
        return Err(AppError::new("POSTGRES_REPLICATION_UNSUPPORTED", "此数据库含逻辑复制订阅或复制槽，不能自动替换")
            .with_hint("可恢复到新数据库进行核对；现有复制关系需要由管理员手动处理。"));
    }
    let suffix = crate::dbadmin::random_database_password()[..16].to_ascii_lowercase();
    let staged_name = format!("niceenv_restore_{suffix}");
    let previous_name = format!("niceenv_previous_{suffix}");
    let safety = postgres_dump(paths, client, version, &target.name, target.oid, &|mut state| {
        state.state = "running".into(); state.message = Some("正在创建恢复前备份".into()); progress(state);
    }).map_err(|error| AppError::new("SAFETY_BACKUP_FAILED", "恢复前备份失败，已中止替换").with_detail(error.detail.unwrap_or(error.message)))?;
    let recovery = format!("恢复前备份：{}。暂存库：{staged_name}。保留库名称：{previous_name}。", safety.display());
    let staged = postgres_restore_staged(paths, client, Path::new(&input.path), &staged_name, &input.owner, true, &|mut state| {
        state.state = "running".into(); progress(state);
    }).map_err(|error| error.with_hint(format!("尚未切换数据库名称，请保留现场检查。{recovery}")))?;
    progress(DbBackupProgress { database: target.name.clone(), bytes: 0, total: None, state: "running".into(), message: Some("正在核对并切换数据库名称".into()) });
    postgres_swap_restored(client, &target, &staged, &previous_name)
        .map_err(|error| { let hint = format!("{} {recovery}", error.hint.as_deref().unwrap_or("")); error.with_hint(hint) })?;
    progress(DbBackupProgress { database: target.name.clone(), bytes: 0, total: None, state: "done".into(), message: Some(recovery) });
    Ok(PostgresReplaceResult { database: target.name, previous_database: previous_name, safety_backup: safety.to_string_lossy().into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names_cannot_escape_or_target_windows_streams() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        for name in [
            "../outside.sql",
            "sub/file.sql",
            "bad:stream.sql",
            "a.sql/../b.sql",
        ] {
            assert!(dump_path(&paths, "8.0.46", name).is_err(), "{name}");
        }
        assert!(dump_path(&paths, "../outside", "backup.sql").is_err());
        for name in ["../outside.dump", "sub/file.dump", "bad:stream.dump", "backup.sql"] {
            assert!(postgres_backup_path(&paths, name).is_err(), "{name}");
            assert!(postgres_delete_backup(&paths, name).is_err(), "{name}");
        }
        assert!(postgres_list_backups(&paths).unwrap().is_empty());
        let archived = postgres_backup_path(&paths, "example.dump").unwrap();
        std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
        std::fs::write(&archived, b"PGDMPfixture").unwrap();
        assert_eq!(postgres_list_backups(&paths).unwrap().len(), 1);
        postgres_delete_backup(&paths, "example.dump").unwrap();
        assert!(!archived.exists());
        assert_ne!(
            default_dump_name(&["app".into()]),
            default_dump_name(&["app".into()])
        );
    }

    #[test]
    fn sanitize_removes_path_separators() {
        assert_eq!(sanitize("my/db"), "my_db");
        assert_eq!(sanitize("..\\evil"), ".._evil");
        assert_eq!(sanitize("ok-name_1.x"), "ok-name_1.x");
    }

    #[test]
    fn system_databases_are_detected() {
        for s in [
            "mysql",
            "MySQL",
            "information_schema",
            "performance_schema",
            "sys",
        ] {
            assert!(is_system_db(s), "{s} 应判为系统库");
        }
        for s in ["app", "wordpress", "mydb"] {
            assert!(!is_system_db(s), "{s} 不应判为系统库");
        }
    }

    #[test]
    fn default_dump_name_single_db_uses_its_name() {
        let n = default_dump_name(&["shop".to_string()]);
        assert!(n.starts_with("shop-"), "{n}");
        assert!(n.ends_with(".sql"));
    }

    #[test]
    fn default_dump_name_multi_lists_count() {
        let n = default_dump_name(&["a".into(), "b".into(), "c".into()]);
        assert!(n.starts_with("3dbs-"), "{n}");
    }

    #[test]
    fn default_dump_name_is_filesystem_safe() {
        let n = default_dump_name(&["a/b:c".to_string()]);
        assert!(
            !n.contains('/') && !n.contains(':'),
            "不能带路径分隔符：{n}"
        );
    }

    #[test]
    fn stamp_has_expected_shape() {
        let s = stamp();
        assert_eq!(s.len(), 15, "{s}");
        assert!(s.contains('-'));
    }

    #[test]
    fn list_backups_on_missing_dir_is_empty_not_error() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-nonexistent-dir-for-test"));
        assert!(list_backups(&paths).unwrap().is_empty());
    }

    #[test]
    fn delete_backup_rejects_path_outside_backup_dir() {
        let base = std::env::temp_dir().join("nsb-del-test");
        let paths = Paths::new(base.clone());
        std::fs::create_dir_all(backup_dir(&paths)).unwrap();
        // 备份目录外造一个文件，尝试用它做路径穿越
        let outside = base.join("secret.sql");
        std::fs::write(&outside, "x").unwrap();
        let r = delete_backup(&paths, &outside.to_string_lossy());
        assert!(r.is_err(), "不应允许删除备份目录外的文件");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn delete_backup_allows_file_inside() {
        let base = std::env::temp_dir().join("nsb-del-test2");
        let paths = Paths::new(base.clone());
        let dir = backup_dir(&paths);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.sql");
        std::fs::write(&f, "x").unwrap();
        assert!(delete_backup(&paths, &f.to_string_lossy()).is_ok());
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dump_without_databases_is_rejected_early() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-dump-test"));
        let conn = ConnInfo {
            engine: crate::dbadmin::DatabaseEngine::Mysql,
            version: "5.7.44".into(),
            port: 3306,
            root_password: String::new(),
            bin_dir: None,
        };
        let out = backup_dir(&paths).join("x.sql");
        let r = dump_databases(&paths, &conn, &[], &out, &|_| {});
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, "NO_DATABASE");
    }
}
