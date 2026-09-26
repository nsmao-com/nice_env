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

/// 备份文件的落盘目录：{base}/backup/db/
pub fn backup_dir(paths: &Paths) -> PathBuf {
    paths.backup().join("db")
}

pub fn dump_path(paths: &Paths, version: &str, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) || !name.to_ascii_lowercase().ends_with(".sql")
    {
        return Err(AppError::new(
            "BAD_BACKUP_NAME",
            "备份名称必须是单个 SQL 文件名",
        ));
    }
    crate::paths::checked_data_path(&paths.base, &format!("backup/db/mysql-{version}-{name}"))
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

fn tool_path(paths: &Paths, conn: &ConnInfo, tool: &str) -> PathBuf {
    conn.bin_dir
        .clone()
        .unwrap_or_else(|| {
            paths
                .runtime_dir("mysql", &conn.version)
                .join(crate::ops::mysql_root_name(&conn.version))
                .join("bin")
        })
        .join(crate::ops::exe_name(tool))
}

/// 备份/还原共用的参数
#[derive(Debug, Clone)]
pub struct ConnInfo {
    pub version: String,
    pub port: u16,
    pub root_password: String,
    pub bin_dir: Option<PathBuf>,
}

/// MySQL 8+ 默认采集的直方图信息不适用于 5.7/MariaDB 来源。
/// 5.7 客户端没有 column-statistics 选项，因此只为支持的客户端关闭它。
pub(crate) fn dump_options(command: &mut std::process::Command, version: &str) {
    command.args([
        "--single-transaction",
        "--routines",
        "--triggers",
        "--events",
        "--hex-blob",
        "--set-gtid-purged=OFF",
        "--no-tablespaces",
    ]);
    if version
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
    let dump = tool_path(paths, conn, "mysqldump");
    if !dump.is_file() {
        return Err(AppError::new(
            "MYSQL_TOOL_MISSING",
            "找不到 mysqldump，无法备份",
        ));
    }
    let client = crate::dbadmin::MySqlClient {
        exe: tool_path(paths, conn, "mysql"),
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
    let pending = tempfile::Builder::new()
        .prefix(".dump-")
        .tempfile_in(parent)?;
    let mut error = tempfile::tempfile()?;
    let (_private, mut command) =
        crate::dbadmin::client_command(&dump, "127.0.0.1", conn.port, "root", &conn.root_password)?;
    dump_options(&mut command, &conn.version);
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
    pending.as_file().sync_all()?;
    let written = pending.as_file().metadata()?.len();
    if written == 0 {
        return Err(AppError::new("DUMP_FAILED", "导出内容为空，未发布备份文件"));
    }
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
    let mysql = tool_path(paths, conn, "mysql");
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
            let path = dump_path(
                paths,
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
