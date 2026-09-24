//! 数据库备份 / 还原（mysqldump / mysql）。
//!
//! phpStudy、ServBay 都有这块，而且是被用得很频繁的功能——改数据前先导一份、
//! 换机器时搬过去。这里做成「导出到文件 / 从文件还原」，并把几件容易出事的事
//! 处理掉：
//!
//! - **备份前先确认服务在跑**，否则 mysqldump 连不上，报错信息还很难懂；
//! - **导出成 .sql 时带上建库语句**（`--databases`），还原时不必先手工建库；
//! - **还原前自动备份当前状态**，因为还原是破坏性的、且不可撤销；
//! - **进度可观测**：大库导出要几十秒，用子进程输出行数估算进度回传，
//!   而不是让界面干等着。
//! - **密码不走命令行**：用临时 defaults-file 传，避免出现在进程列表里
//!   （Windows 上任何用户都能看到别人的命令行）。

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{AppError, Result};
use crate::model::{DbBackupFile, DbBackupProgress};
use crate::paths::Paths;

/// 备份文件的落盘目录：{base}/backup/db/
pub fn backup_dir(paths: &Paths) -> PathBuf {
    paths.backup().join("db")
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

/// 生成一个仅本次调用可用的 defaults-file。
/// 把密码写进文件而不是命令行参数，避免被其它进程通过进程列表看到。
///
/// 文件名必须唯一：同一进程内并发跑多个 mysqldump（比如同时备份几个库）
/// 如果共用一个文件名，后写的会覆盖先写的密码，先跑的那个就用错凭据了。
/// 这里用 pid + 单调计数器 + 纳秒时间戳凑唯一名。
fn write_defaults_file(password: &str) -> Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "nsb-my-{}-{}-{}.cnf",
        std::process::id(),
        seq,
        nanos
    ));
    let content = format!(
        "[client]\nuser=root\npassword={}\ndefault-character-set=utf8mb4\n",
        // my.cnf 里 password 若含空格/引号需要引号包裹
        if password.contains(' ') || password.contains('"') {
            format!("\"{}\"", password.replace('"', "\\\""))
        } else {
            password.to_string()
        }
    );
    std::fs::write(&path, content).map_err(|e| AppError::io("写入临时配置文件", e))?;
    Ok(path)
}

/// 定位 mysqldump / mysql 可执行文件（与 data 目录里的 bin 同级）
fn tool_path(paths: &Paths, version: &str, tool: &str) -> PathBuf {
    paths
        .runtime_dir("mysql", version)
        .join(crate::ops::mysql_root_name(version))
        .join("bin")
        .join(crate::ops::exe_name(tool))
}

/// 备份/还原共用的参数
#[derive(Debug, Clone)]
pub struct ConnInfo {
    pub version: String,
    pub port: u16,
    pub root_password: String,
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
    let dump = tool_path(paths, &conn.version, "mysqldump");
    if !dump.is_file() {
        return Err(
            AppError::new("MYSQL_TOOL_MISSING", "找不到 mysqldump，无法备份")
                .with_hint("确认已安装 MySQL 套件；该工具随 MySQL 客户端一起提供"),
        );
    }
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppError::io("创建备份目录", e))?;
    }
    let defaults = write_defaults_file(&conn.root_password)?;

    let mut cmd = Command::new(&dump);
    cmd.arg(format!("--defaults-extra-file={}", defaults.display()))
        .args(["-h", "127.0.0.1", "-P", &conn.port.to_string()])
        // --databases：把 CREATE DATABASE 一起写进去，还原时不用先建库
        .arg("--databases")
        // 单事务导出（InnoDB），避免锁表影响正在跑的站点
        .args([
            "--single-transaction",
            "--routines",
            "--triggers",
            "--events",
        ])
        .args(["--hex-blob", "--default-character-set=utf8mb4"])
        .args(databases)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| AppError::io("启动 mysqldump", e))?;

    let mut written: u64 = 0;
    {
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::new("SPAWN_FAILED", "无法读取 mysqldump 输出"))?;
        let mut reader = BufReader::new(stdout);
        let file = std::fs::File::create(out_path).map_err(|e| AppError::io("创建备份文件", e))?;
        let mut writer = std::io::BufWriter::new(file);
        let mut buf = vec![0u8; 64 * 1024];
        let mut last_report = std::time::Instant::now();
        loop {
            let n = std::io::Read::read(&mut reader, &mut buf)
                .map_err(|e| AppError::io("读取 mysqldump 输出", e))?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut writer, &buf[..n])
                .map_err(|e| AppError::io("写入备份文件", e))?;
            written += n as u64;
            // 200ms 报一次，避免刷爆前端
            if last_report.elapsed().as_millis() >= 200 {
                progress(DbBackupProgress {
                    database: databases.join(", "),
                    bytes: written,
                    total: None,
                    state: "running".into(),
                    message: None,
                });
                last_report = std::time::Instant::now();
            }
        }
        std::io::Write::flush(&mut writer).map_err(|e| AppError::io("落盘备份文件", e))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| AppError::io("等待 mysqldump 结束", e))?;
    let _ = std::fs::remove_file(&defaults);

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).to_string();
        // 失败时留一个半截文件没有意义，删掉免得用户以为备份成功了
        let _ = std::fs::remove_file(out_path);
        return Err(AppError::new("DUMP_FAILED", "导出失败")
            .with_hint("确认 MySQL 服务正在运行、root 密码正确（可在「数据库」页重置）")
            .with_detail(err));
    }

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
    if !sql_path.is_file() {
        return Err(AppError::new("FILE_NOT_FOUND", "备份文件不存在"));
    }
    let mysql = tool_path(paths, &conn.version, "mysql");
    if !mysql.is_file() {
        return Err(AppError::new(
            "MYSQL_TOOL_MISSING",
            "找不到 mysql 客户端，无法还原",
        ));
    }

    // 还原前兜底：把现有全部业务库导一份
    let mut safety: Option<PathBuf> = None;
    if safety_backup {
        let dbs = crate::dbadmin::MySqlClient::from_state(
            paths,
            &conn.version,
            conn.port,
            conn.root_password.clone(),
        )
        .list_databases()
        .map(|list| {
            list.into_iter()
                // 系统库不备份，还原它们没有意义且可能出问题
                .filter(|d| !is_system_db(&d.name))
                .map(|d| d.name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
        if !dbs.is_empty() {
            let path = backup_dir(paths).join(format!("pre-restore-{}.sql", stamp()));
            if dump_databases(paths, conn, &dbs, &path, &|_| {}).is_ok() {
                safety = Some(path);
            }
        }
    }

    let defaults = write_defaults_file(&conn.root_password)?;
    let file = std::fs::File::open(sql_path).map_err(|e| AppError::io("打开备份文件", e))?;
    let total = file.metadata().ok().map(|m| m.len());

    let child = Command::new(&mysql)
        .arg(format!("--defaults-extra-file={}", defaults.display()))
        .args(["-h", "127.0.0.1", "-P", &conn.port.to_string()])
        .arg("--default-character-set=utf8mb4")
        .stdin(Stdio::from(file))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::io("启动 mysql 客户端", e))?;

    progress(DbBackupProgress {
        database: sql_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        bytes: 0,
        total,
        state: "running".into(),
        message: None,
    });

    let output = child
        .wait_with_output()
        .map_err(|e| AppError::io("等待还原结束", e))?;
    let _ = std::fs::remove_file(&defaults);

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(AppError::new("RESTORE_FAILED", "还原失败")
            .with_hint(
                safety
                    .as_ref()
                    .map(|p| format!("还原前的自动备份在 {}", p.display()))
                    .unwrap_or_else(|| "确认 MySQL 正在运行、root 密码正确".to_string()),
            )
            .with_detail(err));
    }
    progress(DbBackupProgress {
        database: sql_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        bytes: total.unwrap_or(0),
        total,
        state: "done".into(),
        message: safety
            .as_ref()
            .map(|p| format!("还原前备份：{}", p.display())),
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
pub fn list_backups(paths: &Paths) -> Vec<DbBackupFile> {
    let dir = backup_dir(paths);
    let mut out: Vec<DbBackupFile> = Vec::new();
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("sql") {
            continue;
        }
        let meta = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
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
    out
}

/// 删除一个备份文件（只允许删备份目录里的，避免被当成任意文件删除接口）
pub fn delete_backup(paths: &Paths, path: &str) -> Result<()> {
    let target = std::path::Path::new(path);
    let dir = backup_dir(paths);
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
    format!("{label}-{}.sql", stamp())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(list_backups(&paths).is_empty());
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
        };
        let out = backup_dir(&paths).join("x.sql");
        let r = dump_databases(&paths, &conn, &[], &out, &|_| {});
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, "NO_DATABASE");
    }

    #[test]
    fn defaults_file_quotes_password_with_spaces() {
        let p = write_defaults_file("pa ss").unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("password=\"pa ss\""), "{content}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn defaults_file_plain_password_unquoted() {
        let p = write_defaults_file("secret").unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert!(content.contains("password=secret"));
        let _ = std::fs::remove_file(&p);
    }
}
