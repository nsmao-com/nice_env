//! 跨环境 MySQL 迁移：把 FlyEnv / phpStudy / ServBay / XAMPP 等本机已有
//! MySQL/MariaDB 实例里的库，导入到 NiceEnv 托管的实例。
//!
//! 实现：用本应用安装的 `mysql` / `mysqldump` 客户端二进制
//! 先完整导出、再保护性备份、最后恢复。密码经私有 defaults-file 传递。

use crate::error::{AppError, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;

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

/// 过滤系统库，只留用户库
pub fn filter_user_databases(all: &[String]) -> Vec<String> {
    all.iter()
        .filter(|n| !crate::dbbackup::is_system_db(n))
        .filter(|n| !n.is_empty())
        .cloned()
        .collect()
}

fn mysql_bin(bin_dir: &Path, name: &str) -> PathBuf {
    bin_dir.join(crate::ops::exe_name(name))
}

/// 列出源实例上的用户数据库，库名用 HEX 避免制表符等字符破坏响应格式。
pub fn list_source_databases(bin_dir: &Path, src: &SourceConn) -> Result<Vec<SourceDb>> {
    let out = source_query(bin_dir, src, "SELECT HEX(schema_name), COALESCE(SUM(data_length+index_length),0) FROM information_schema.schemata LEFT JOIN information_schema.tables ON table_schema=schema_name GROUP BY schema_name ORDER BY schema_name;")?;
    out.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let bad = || AppError::new("MYSQL_RESPONSE", "源数据库列表响应不完整，请重试");
            let mut parts = line.split('\t');
            let name =
                String::from_utf8(hex::decode(parts.next().ok_or_else(bad)?).map_err(|_| bad())?)
                    .map_err(|_| bad())?;
            let bytes = parts
                .next()
                .ok_or_else(bad)?
                .parse::<u64>()
                .map_err(|_| bad())?;
            Ok(SourceDb {
                name,
                size_kb: Some(bytes / 1024),
            })
        })
        .collect::<Result<Vec<_>>>()
        .map(|dbs| {
            dbs.into_iter()
                .filter(|db| !crate::dbbackup::is_system_db(&db.name))
                .collect()
        })
}

fn source_query(bin_dir: &Path, src: &SourceConn, sql: &str) -> Result<String> {
    crate::dbadmin::query_client(
        &mysql_bin(bin_dir, "mysql"),
        &src.host,
        src.port,
        &src.user,
        &src.password,
        sql,
    )
    .map_err(|error| error.with_hint("确认源环境正在运行，地址、端口、账号及密码正确"))
}

/// 先完整导出到私有临时文件，再保护性备份并还原到明确的目标实例。
/// 导出失败绝不启动导入；还原失败明确保留部分执行的可能性和保护备份位置。
pub fn import_databases(
    paths: &crate::paths::Paths,
    src: &SourceConn,
    databases: &[String],
    target: &crate::dbbackup::ConnInfo,
    mut emit_state: impl FnMut(String, &str),
) -> Result<ImportReport> {
    let mut report = ImportReport::default();
    if databases.is_empty() {
        return Ok(report);
    }
    let bin_dir = target.bin_dir.clone().unwrap_or_else(|| {
        paths
            .runtime_dir("mysql", &target.version)
            .join(crate::ops::mysql_root_name(&target.version))
            .join("bin")
    });
    let dump = mysql_bin(&bin_dir, "mysqldump");
    if !dump.is_file() {
        return Err(AppError::new(
            "MYSQL_TOOL_MISSING",
            "找不到 mysqldump，请检查所选 MySQL 安装",
        ));
    }
    let source_dbs = list_source_databases(&bin_dir, src)?;
    if databases.iter().any(|db| {
        db.is_empty()
            || db.starts_with('-')
            || db.chars().any(char::is_control)
            || crate::dbbackup::is_system_db(db)
            || !source_dbs.iter().any(|item| &item.name == db)
    }) {
        return Err(AppError::new(
            "BAD_DATABASE",
            "所选源数据库无效或已不存在，请重新检测",
        ));
    }
    let identity = "SELECT HEX(CONCAT(@@hostname, CHAR(0), @@datadir, CHAR(0), @@port));";
    let source_id = source_query(&bin_dir, src, identity)?;
    let client = crate::dbadmin::MySqlClient {
        exe: mysql_bin(&bin_dir, "mysql"),
        port: target.port,
        root_password: target.root_password.clone(),
    };
    if source_id.trim() == client.run(identity)?.trim() {
        return Err(AppError::new(
            "SAME_MYSQL_INSTANCE",
            "源和目标是同一个 MySQL 实例，请选择其他来源",
        ));
    }
    let pending = tempfile::Builder::new()
        .prefix("niceenv-import-")
        .suffix(".sql")
        .tempfile()?;
    let mut error = tempfile::tempfile()?;
    let (_private, mut command) =
        crate::dbadmin::client_command(&dump, &src.host, src.port, &src.user, &src.password)?;
    crate::dbbackup::dump_options(&mut command, &target.version);
    command
        .args(databases)
        .stdin(Stdio::null())
        .stdout(pending.as_file().try_clone()?)
        .stderr(error.try_clone()?);
    for db in databases {
        emit_state(db.clone(), "exporting");
    }
    let status =
        crate::dbadmin::wait_client(&mut command, std::time::Duration::from_secs(1800), || {})?;
    if !status.success() || pending.as_file().metadata()?.len() == 0 {
        let detail = crate::dbadmin::read_output(&mut error, 64 * 1024)?;
        let detail = if src.password.is_empty() {
            detail
        } else {
            detail.replace(&src.password, "***")
        };
        return Err(
            AppError::new("SOURCE_DUMP_FAILED", "源数据库导出失败，目标数据库未改动")
                .with_detail(detail),
        );
    }
    pending.as_file().sync_all()?;
    for db in databases {
        emit_state(db.clone(), "importing");
    }
    match crate::dbbackup::restore_from_file(paths, target, pending.path(), true, &|_| {}) {
        Ok(_) => {
            report.imported = databases.to_vec();
            for db in databases {
                emit_state(db.clone(), "imported");
            }
        }
        Err(error) => {
            let message = format!(
                "{}{}",
                error.message,
                error
                    .hint
                    .map(|hint| format!("；{hint}"))
                    .unwrap_or_default()
            );
            for db in databases {
                report.failed.push((db.clone(), message.clone()));
                emit_state(db.clone(), "failed");
            }
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
        let (_private, command) = crate::dbadmin::client_command(
            Path::new("mysql"),
            &src.host,
            src.port,
            &src.user,
            &src.password,
        )
        .unwrap();
        let args = command
            .get_args()
            .map(|s| s.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!args.contains("s3cret"), "密码不得进命令行参数");
        assert!(args.contains("127.0.0.1"));
    }

    #[test]
    fn empty_database_list_is_noop_report() {
        // 不需要真实服务器：空列表直接返回空报告
        let paths = crate::paths::Paths::new(std::env::temp_dir().join("niceenv-empty-import"));
        let target = crate::dbbackup::ConnInfo {
            version: "8.0.46".into(),
            port: 0,
            root_password: String::new(),
            bin_dir: None,
        };
        let src = SourceConn {
            host: "".into(),
            port: 0,
            user: "".into(),
            password: "".into(),
        };
        let mut states = Vec::new();
        let r = import_databases(&paths, &src, &[], &target, |db, st| {
            states.push((db, st.to_string()))
        })
        .unwrap();
        assert!(r.imported.is_empty() && r.failed.is_empty() && states.is_empty());
    }
}
