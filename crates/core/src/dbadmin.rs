//! MySQL 管理：建库 / 建号授权 / 改 root 密码 / 库表列表（通过已装 mysql 客户端执行）。

use crate::error::{AppError, Result};
use crate::model::{DatabaseInfo, DbUserInfo};
use crate::paths::Paths;
use std::path::PathBuf;

pub struct MySqlClient {
    pub exe: PathBuf,
    pub port: u16,
    pub root_password: String,
}

impl MySqlClient {
    pub fn from_state(paths: &Paths, version: &str, port: u16, root_password: String) -> Self {
        let exe = paths
            .runtime_dir("mysql", version)
            .join(crate::ops::mysql_root_name(version))
            .join("bin")
            .join(crate::ops::exe_name("mysql"));
        Self { exe, port, root_password }
    }

    fn run(&self, sql: &str) -> Result<String> {
        let out = std::process::Command::new(&self.exe)
            .args([
                "-h",
                "127.0.0.1",
                "-P",
                &self.port.to_string(),
                "-u",
                "root",
                &format!("--password={}", self.root_password),
                "--default-character-set=utf8mb4",
                "-e",
                sql,
            ])
            .output()
            .map_err(|e| AppError::io("执行 mysql 客户端", e))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(AppError::new("MYSQL_EXEC_FAILED", format!("MySQL 命令执行失败"))
                .with_hint("确认 MySQL 服务已启动；root 密码可在「数据库」页重置")
                .with_detail(err));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    pub fn ping(&self) -> Result<()> {
        self.run("SELECT 1;").map(|_| ())
    }

    pub fn list_databases(&self) -> Result<Vec<DatabaseInfo>> {
        let out = self.run(
            "SELECT schema_name FROM information_schema.schemata ORDER BY schema_name;",
        )?;
        let mut list = Vec::new();
        for name in out.lines().skip(1).map(str::trim).filter(|l| !l.is_empty()) {
            let tables = self
                .run(&format!(
                    "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='{name}';"
                ))
                .ok()
                .and_then(|o| o.lines().nth(1).and_then(|l| l.trim().parse::<u32>().ok()));
            list.push(DatabaseInfo {
                name: name.to_string(),
                tables,
                size_kb: None,
            });
        }
        Ok(list)
    }

    pub fn create_database(&self, name: &str) -> Result<()> {
        let safe = sanitize_ident(name)?;
        self.run(&format!(
            "CREATE DATABASE IF NOT EXISTS `{safe}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"
        ))?;
        Ok(())
    }

    pub fn drop_database(&self, name: &str) -> Result<()> {
        let safe = sanitize_ident(name)?;
        self.run(&format!("DROP DATABASE IF EXISTS `{safe}`;"))?;
        Ok(())
    }

    pub fn create_user_grant(&self, username: &str, password: &str, database: &str) -> Result<()> {
        let u = sanitize_ident(username)?;
        let d = sanitize_ident(database)?;
        self.run(&format!(
            "CREATE USER IF NOT EXISTS '{u}'@'127.0.0.1' IDENTIFIED BY '{password}';\
             CREATE USER IF NOT EXISTS '{u}'@'localhost' IDENTIFIED BY '{password}';\
             GRANT ALL PRIVILEGES ON `{d}`.* TO '{u}'@'127.0.0.1';\
             GRANT ALL PRIVILEGES ON `{d}`.* TO '{u}'@'localhost';\
             FLUSH PRIVILEGES;"
        ))?;
        Ok(())
    }

    pub fn list_users(&self) -> Result<Vec<DbUserInfo>> {
        let out = self.run("SELECT user, host FROM mysql.user ORDER BY user;")?;
        let mut list = Vec::new();
        for line in out.lines().skip(1).filter(|l| !l.trim().is_empty()) {
            let mut parts = line.split_whitespace();
            if let (Some(user), Some(host)) = (parts.next(), parts.next()) {
                list.push(DbUserInfo {
                    username: user.to_string(),
                    host: host.to_string(),
                    grants: None,
                });
            }
        }
        Ok(list)
    }

    pub fn reset_root_password(&self, new_password: &str) -> Result<()> {
        if new_password.is_empty() || new_password.contains('\'') {
            return Err(AppError::new("BAD_PASSWORD", "密码不能为空且不能包含单引号"));
        }
        self.run(&format!(
            "ALTER USER 'root'@'localhost' IDENTIFIED BY '{new_password}';\
             ALTER USER 'root'@'127.0.0.1' IDENTIFIED BY '{new_password}';\
             FLUSH PRIVILEGES;"
        ))?;
        Ok(())
    }
}

fn sanitize_ident(s: &str) -> Result<String> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(AppError::new(
            "BAD_IDENTIFIER",
            format!("“{s}” 不是合法的标识符（只允许字母数字下划线）"),
        ));
    }
    Ok(s.to_string())
}

/// 生成 .env.example 内容
pub fn render_env_example(db: &str, user: &str, pass: &str, port: u16) -> String {
    format!(
        r#"# NiceServBay 自动生成的本地数据库连接信息
DB_CONNECTION=mysql
DB_HOST=127.0.0.1
DB_PORT={port}
DB_DATABASE={db}
DB_USERNAME={user}
DB_PASSWORD={pass}

# Redis（本应用托管，端口见套件页）
REDIS_HOST=127.0.0.1
REDIS_PORT=null
REDIS_PASSWORD=null
"#
    )
}
