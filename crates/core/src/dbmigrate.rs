//! 跨环境 MySQL 迁移：把 FlyEnv / phpStudy / ServBay / XAMPP 等本机已有
//! MySQL/MariaDB 实例里的库，导入到 NiceEnv 托管的实例。
//!
//! 实现：用本应用安装的 `mysql` / `mysqldump` 客户端二进制
//! （`mysqldump --single-transaction` 流式管道到 `mysql`，不落中间文件）。
//! 密码经 `MYSQL_PWD` 环境变量传递，不进命令行（防泄漏到进程列表）。

use crate::error::{AppError, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const SYSTEM_DATABASES: &[&str] = &["information_schema", "mysql", "performance_schema", "sys"];

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SourceDb {
    pub name: String,
    /// 源库里该库的大致大小（KB；拿不到为 None）
    pub size_kb: Option<u64>,
}

#[derive(serde::Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: Vec<String>,
    pub failed: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct SourceConn {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
}

impl SourceConn {
    fn client_args(&self) -> Vec<String> {
        vec![
            "-h".into(),
            self.host.clone(),
            "-P".into(),
            self.port.to_string(),
            "-u".into(),
            self.user.clone(),
        ]
    }
    fn envs(&self) -> Vec<(String, String)> {
        vec![("MYSQL_PWD".into(), self.password.clone())]
    }
}

/// 过滤系统库，只留用户库
pub fn filter_user_databases(all: &[String]) -> Vec<String> {
    all.iter()
        .filter(|n| !SYSTEM_DATABASES.contains(&n.as_str()))
        .filter(|n| !n.is_empty())
        .cloned()
        .collect()
}

fn mysql_bin(bin_dir: &Path, name: &str) -> PathBuf {
    bin_dir.join(crate::ops::exe_name(name))
}

/// 列出源实例上的用户数据库（含大致大小）
pub fn list_source_databases(bin_dir: &Path, src: &SourceConn) -> Result<Vec<SourceDb>> {
    let out = Command::new(mysql_bin(bin_dir, "mysql"))
        .args(src.client_args())
        .args(["-N", "-B", "-e", "SELECT schema_name, COALESCE(SUM(data_length+index_length),0) FROM information_schema.schemata LEFT JOIN information_schema.tables ON table_schema=schema_name GROUP BY schema_name ORDER BY schema_name;"])
        .envs(src.envs())
        .output()
        .map_err(|e| AppError::io("启动 mysql 客户端", e))?;
    if !out.status.success() {
        return Err(AppError::new(
            "SOURCE_CONNECT_FAILED",
            format!(
                "连不上源数据库：{}",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .next()
                    .unwrap_or("")
            ),
        )
        .with_hint("确认源环境（FlyEnv/phpStudy/ServBay）正在运行，host/port/账号密码正确"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut list = Vec::new();
    for line in text.lines() {
        let mut it = line.split('\t');
        let name = it.next().unwrap_or("").trim().to_string();
        if name.is_empty() {
            continue;
        }
        let size_kb = it
            .next()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|b| b / 1024);
        list.push(SourceDb { name, size_kb });
    }
    Ok(list
        .into_iter()
        .filter(|db| !SYSTEM_DATABASES.contains(&db.name.as_str()))
        .collect())
}

/// 导入指定库：逐库 mysqldump | mysql 流式管道。
/// `emit_state` 会在每个库开始/结束时回调（前端进度用）。
pub fn import_databases(
    bin_dir: &Path,
    src: &SourceConn,
    databases: &[String],
    target: &SourceConn,
    mut emit_state: impl FnMut(String, &str),
) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    if databases.is_empty() {
        return Ok(report); // 没选库就不用碰客户端，也不该要求已安装
    }

    let dump = mysql_bin(bin_dir, "mysqldump");
    let client = mysql_bin(bin_dir, "mysql");
    if !dump.is_file() || !client.is_file() {
        return Err(AppError::not_installed("MySQL")
            .with_hint("导入用的是本应用安装的 mysqldump/mysql 客户端，先在套件页装 MySQL"));
    }

    for db in databases {
        let safe = db.trim();
        // 库名进命令行参数，防注入：只允许常见库名字符
        if safe.is_empty()
            || !safe
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '$'))
        {
            report.failed.push((db.clone(), "库名不合法".into()));
            continue;
        }
        emit_state(safe.to_string(), "importing");

        let mut dumper = Command::new(&dump)
            .args(src.client_args())
            .args([
                "--single-transaction",
                "--routines",
                "--events",
                "--no-tablespaces",
                "--default-character-set=utf8mb4",
                safe,
            ])
            .envs(src.envs())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AppError::io("启动 mysqldump", e))?;

        let dump_stdout = dumper.stdout.take();
        let mut importer = Command::new(&client)
            .args(target.client_args())
            .envs(target.envs())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AppError::io("启动 mysql 导入端", e))?;
        let importer_stdin = importer.stdin.take();

        // 泵：dump stdout → importer stdin（独立线程，避免双方管道死锁）
        let pump = std::thread::spawn(move || {
            let Some(mut reader) = dump_stdout else {
                return;
            };
            let Some(mut writer) = importer_stdin else {
                return;
            };
            let mut buf = [0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if writer.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = writer.flush();
        });
        let _ = pump.join();

        // stdout 已被泵线程接管，只收 stderr；wait 拿退出码
        let mut dumper_err = String::new();
        if let Some(mut e) = dumper.stderr.take() {
            use std::io::Read as _;
            let _ = e.read_to_string(&mut dumper_err);
        }
        let mut importer_err = String::new();
        if let Some(mut e) = importer.stderr.take() {
            use std::io::Read as _;
            let _ = e.read_to_string(&mut importer_err);
        }
        let dump_ok = dumper.wait().map(|s| s.success()).unwrap_or(false);
        let import_ok = importer.wait().map(|s| s.success()).unwrap_or(false);

        if dump_ok && import_ok {
            report.imported.push(safe.to_string());
            emit_state(safe.to_string(), "imported");
        } else {
            let msg = if !dumper_err.is_empty() {
                dumper_err
            } else {
                importer_err
            };
            report.failed.push((
                safe.to_string(),
                msg.lines().next().unwrap_or("").to_string(),
            ));
            emit_state(safe.to_string(), "failed");
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_databases_are_filtered() {
        let all = vec![
            "information_schema".into(),
            "mysql".into(),
            "performance_schema".into(),
            "sys".into(),
            "laravel_shop".into(),
            "wordpress".into(),
        ];
        let users = filter_user_databases(&all);
        assert_eq!(users, vec!["laravel_shop", "wordpress"]);
    }

    #[test]
    fn client_args_hide_password() {
        let src = SourceConn {
            host: "127.0.0.1".into(),
            port: 3306,
            user: "root".into(),
            password: "s3cret".into(),
        };
        let args = src.client_args().join(" ");
        assert!(!args.contains("s3cret"), "密码不得进命令行参数");
        assert!(args.contains("127.0.0.1"));
    }

    #[test]
    fn empty_database_list_is_noop_report() {
        // 不需要真实服务器：空列表直接返回空报告
        let bin = Path::new(".");
        let src = SourceConn {
            host: "".into(),
            port: 0,
            user: "".into(),
            password: "".into(),
        };
        let mut states = Vec::new();
        let r = import_databases(bin, &src, &[], &src, |db, st| {
            states.push((db, st.to_string()))
        })
        .unwrap();
        assert!(r.imported.is_empty() && r.failed.is_empty() && states.is_empty());
    }
}
