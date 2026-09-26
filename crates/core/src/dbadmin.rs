//! MySQL 管理：建库 / 建号授权 / 改 root 密码 / 库表列表（通过已装 mysql 客户端执行）。

use crate::error::{AppError, Result};
use crate::model::{DatabaseInfo, DbUserInfo};
use crate::paths::Paths;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub fn password_key(version: &str) -> String {
    format!("mysqlRootPassword@{version}")
}

pub fn port_key(version: &str) -> String {
    format!("mysqlLastPort@{version}")
}

pub fn saved_port(store: &crate::store::Store, version: &str) -> Option<u16> {
    store
        .get_setting(&port_key(version))
        .and_then(|value| value.parse().ok())
        .filter(|port| *port > 0)
}

pub fn saved_password(store: &crate::store::Store, version: &str) -> Option<String> {
    store
        .get_setting(&password_key(version))
        .or_else(|| store.get_setting("mysqlRootPassword"))
}

/// 只连接已启动的准确版本；端口使用本次启动记录，避免设置变化后连到另一个实例。
pub fn selected_client(
    state: &crate::CoreState,
    version: Option<&str>,
) -> Result<(String, MySqlClient)> {
    authenticated_client(state, version, None)
}

pub(crate) fn authenticated_client(
    state: &crate::CoreState,
    version: Option<&str>,
    password: Option<String>,
) -> Result<(String, MySqlClient)> {
    let package = match version {
        Some(version) => state.store.find_installed("mysql", Some(version)),
        None => crate::ops::installed_by_choice(&state.store, "mysql"),
    }
    .ok_or_else(|| AppError::not_installed("MySQL"))?;
    let id = format!("mysql@{}", package.version);
    let service = state
        .manager
        .snapshot(&id)
        .filter(|s| s.state == crate::model::ServiceState::Running)
        .ok_or_else(|| {
            AppError::new(
                "MYSQL_NOT_RUNNING",
                format!("MySQL {} 尚未启动", package.version),
            )
        })?;
    let port = service
        .port
        .ok_or_else(|| AppError::new("MYSQL_NOT_RUNNING", "无法确定 MySQL 实际端口"))?;
    let password = password
        .or_else(|| saved_password(&state.store, &package.version))
        .ok_or_else(|| AppError::new("MYSQL_AUTH_REQUIRED", "请先更新该实例的 root 连接密码"))?;
    let client = MySqlClient::from_install_dir(
        Path::new(&package.install_path),
        &package.version,
        port,
        password,
    );
    client.verify_data_dir(&state.paths.mysql_data_dir(&package.version))?;
    Ok((package.version, client))
}

/// 生命周期由调用方持有；defaults 和 login-path 均隔离到私有临时目录。
pub(crate) fn client_command(
    exe: &Path,
    host: &str,
    port: u16,
    user: &str,
    password: &str,
) -> Result<(tempfile::TempDir, Command)> {
    if port == 0
        || host.is_empty()
        || host.chars().any(char::is_control)
        || user.chars().any(char::is_control)
        || password.contains('\0')
    {
        return Err(AppError::new("BAD_CONNECTION", "数据库连接参数无效"));
    }
    let private = tempfile::Builder::new()
        .prefix("niceenv-mysql-")
        .tempdir()?;
    let defaults = private.path().join("client.cnf");
    let escape = |s: &str| {
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&defaults)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    write!(
        file,
        "[client]\nuser=\"{}\"\npassword=\"{}\"\ndefault-character-set=utf8mb4\n",
        escape(user),
        escape(password)
    )?;
    file.sync_all()?;
    let mut command = platform::command(exe);
    command
        .arg(format!("--defaults-file={}", defaults.display()))
        .args([
            "--protocol=TCP",
            "--host",
            host,
            "--port",
            &port.to_string(),
        ])
        .env(
            "MYSQL_TEST_LOGIN_FILE",
            private.path().join("unused.mylogin.cnf"),
        )
        .env_remove("MYSQL_PWD");
    Ok((private, command))
}

/// 文件重定向避免 stdout/stderr 管道互相等待；失败、超时均回收子进程。
pub(crate) fn wait_client(
    command: &mut Command,
    timeout: Duration,
    mut progress: impl FnMut(),
) -> Result<ExitStatus> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(platform::spawn_pre_exec);
        }
    }
    let mut group = platform::ProcessGroup::new()?;
    let mut child = command
        .spawn()
        .map_err(|e| AppError::io("启动数据库客户端", e))?;
    if let Err(error) = group.attach(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error.into());
    }
    let started = Instant::now();
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                progress();
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                break Err(AppError::new(
                    "MYSQL_TIMEOUT",
                    "数据库操作超时，已停止客户端",
                ))
            }
            Err(error) => break Err(AppError::io("等待数据库客户端", error)),
        }
    };
    if result.is_err() {
        let _ = group.terminate(true);
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}

pub(crate) fn read_output(file: &mut std::fs::File, limit: u64) -> Result<String> {
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn query_client(
    exe: &Path,
    host: &str,
    port: u16,
    user: &str,
    password: &str,
    sql: &str,
) -> Result<String> {
    let (_private, mut command) = client_command(exe, host, port, user, password)?;
    let mut input = tempfile::tempfile()?;
    input.write_all(sql.as_bytes())?;
    input.rewind()?;
    let mut output = tempfile::tempfile()?;
    let mut error = tempfile::tempfile()?;
    command
        .args([
            "--batch",
            "--raw",
            "--skip-column-names",
            "--binary-mode",
            "--connect-timeout=5",
        ])
        .stdin(Stdio::from(input))
        .stdout(output.try_clone()?)
        .stderr(error.try_clone()?);
    let status = wait_client(&mut command, Duration::from_secs(30), || {})?;
    if !status.success() {
        let mut detail = read_output(&mut error, 16 * 1024)?;
        if !password.is_empty() {
            detail = detail.replace(password, "***");
        }
        return Err(AppError::new("MYSQL_EXEC_FAILED", "MySQL 命令执行失败")
            .with_hint("确认所选实例已启动；密码不一致时请更新 root 连接密码")
            .with_detail(detail));
    }
    read_output(&mut output, 16 * 1024 * 1024)
}

pub struct MySqlClient {
    pub exe: PathBuf,
    pub port: u16,
    pub root_password: String,
}

impl MySqlClient {
    pub fn from_state(paths: &Paths, version: &str, port: u16, root_password: String) -> Self {
        Self::from_install_dir(
            &paths.runtime_dir("mysql", version),
            version,
            port,
            root_password,
        )
    }

    pub fn from_install_dir(
        install: &Path,
        version: &str,
        port: u16,
        root_password: String,
    ) -> Self {
        let exe = install
            .join(crate::ops::mysql_root_name(version))
            .join("bin")
            .join(crate::ops::exe_name("mysql"));
        Self {
            exe,
            port,
            root_password,
        }
    }

    pub(crate) fn run(&self, sql: &str) -> Result<String> {
        query_client(
            &self.exe,
            "127.0.0.1",
            self.port,
            "root",
            &self.root_password,
            sql,
        )
    }

    pub fn ping(&self) -> Result<()> {
        self.run("SELECT 1;").map(|_| ())
    }

    pub fn verify_data_dir(&self, expected: &Path) -> Result<()> {
        let path = self.run("SELECT @@datadir;")?;
        let normalize = |path: &Path| {
            let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            let value = path
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches("//?/")
                .trim_end_matches('/')
                .to_string();
            if cfg!(windows) {
                value.to_lowercase()
            } else {
                value
            }
        };
        if normalize(Path::new(path.trim())) != normalize(expected) {
            return Err(AppError::new(
                "MYSQL_INSTANCE_MISMATCH",
                "端口上的 MySQL 数据目录与所选实例不一致，操作已中止",
            ));
        }
        Ok(())
    }

    pub fn list_databases(&self) -> Result<Vec<DatabaseInfo>> {
        let out = self.run("SELECT HEX(schema_name), COUNT(table_name), COALESCE(SUM(data_length+index_length),0) FROM information_schema.schemata LEFT JOIN information_schema.tables ON table_schema=schema_name GROUP BY schema_name ORDER BY schema_name;")?;
        out.lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let mut parts = line.split('\t');
                let bad = || AppError::new("MYSQL_RESPONSE", "数据库列表响应不完整，请重试");
                let name = String::from_utf8(
                    hex::decode(parts.next().ok_or_else(bad)?).map_err(|_| bad())?,
                )
                .map_err(|_| bad())?;
                let tables = parts
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .ok_or_else(bad)?;
                let size = parts
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .ok_or_else(bad)?;
                Ok(DatabaseInfo {
                    name,
                    tables: Some(tables),
                    size_kb: Some(size / 1024),
                })
            })
            .collect()
    }

    pub fn create_database(&self, name: &str) -> Result<()> {
        let safe = sanitize_ident(name)?;
        self.run(&format!(
            "CREATE DATABASE IF NOT EXISTS `{safe}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"
        ))?;
        Ok(())
    }

    pub fn drop_database(&self, name: &str) -> Result<()> {
        if crate::dbbackup::is_system_db(name) {
            return Err(AppError::new(
                "SYSTEM_DATABASE",
                "不能删除 MySQL 系统数据库",
            ));
        }
        let safe = sanitize_ident(name)?;
        self.run(&format!("DROP DATABASE IF EXISTS `{safe}`;"))?;
        Ok(())
    }

    pub fn create_user_grant(&self, username: &str, password: &str, database: &str) -> Result<()> {
        validate_create_db(database, username, password)?;
        let u = sanitize_ident(username)?;
        let d = sanitize_ident(database)?;
        if username.eq_ignore_ascii_case("root") || crate::dbbackup::is_system_db(database) {
            return Err(AppError::new(
                "SYSTEM_ACCOUNT",
                "请为业务数据库创建独立账号",
            ));
        }
        if !self.list_databases()?.iter().any(|db| db.name == database) {
            return Err(AppError::new(
                "NO_DATABASE",
                "所选数据库不存在，请刷新后选择",
            ));
        }
        if self.list_users()?.iter().any(|user| {
            user.username == username && ["localhost", "127.0.0.1"].contains(&user.host.as_str())
        }) {
            return Err(AppError::new(
                "DB_USER_EXISTS",
                "同名本地账号已存在，未修改密码或权限，请使用其他用户名",
            ));
        }
        let escaped = password.replace('\'', "''");
        // SQL 经 stdin 传入；只改变当前短连接的转义规则。
        self.run(&format!(
            "SET SESSION sql_mode = 'NO_BACKSLASH_ESCAPES'; \
             CREATE USER '{u}'@'127.0.0.1' IDENTIFIED BY '{escaped}', '{u}'@'localhost' IDENTIFIED BY '{escaped}';"
        )).map_err(|mut error| {
            if let Some(detail) = error.detail.as_mut() { *detail = detail.replace(password, "***").replace(&escaped, "***"); }
            error
        })?;
        if let Err(error) = self.run(&format!(
            "GRANT ALL PRIVILEGES ON `{d}`.* TO '{u}'@'127.0.0.1'; \
             GRANT ALL PRIVILEGES ON `{d}`.* TO '{u}'@'localhost';"
        )) {
            let cleanup = self.run(&format!("DROP USER '{u}'@'127.0.0.1', '{u}'@'localhost';"));
            return Err(error.with_hint(if cleanup.is_ok() {
                "授权失败，已回收本次新建的两个账号，可修正后重试"
            } else {
                "授权失败且账号回收失败，请检查本次新建账号的权限后再操作"
            }));
        }
        Ok(())
    }

    pub fn list_users(&self) -> Result<Vec<DbUserInfo>> {
        let out = self.run("SELECT user, host FROM mysql.user ORDER BY user;")?;
        let mut list = Vec::new();
        for line in out.lines().filter(|l| !l.is_empty()) {
            let mut parts = line.split('\t');
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
        if new_password.is_empty() || new_password.chars().any(char::is_control) {
            return Err(AppError::new("BAD_PASSWORD", "密码不能为空或包含控制字符"));
        }
        let users = self.list_users()?;
        let password = new_password.replace('\'', "''");
        let accounts = users
            .iter()
            .filter(|u| {
                u.username == "root" && ["localhost", "127.0.0.1"].contains(&u.host.as_str())
            })
            .map(|u| format!("'root'@'{}' IDENTIFIED BY '{password}'", u.host))
            .collect::<Vec<_>>();
        if accounts.is_empty() {
            return Err(AppError::new("MYSQL_AUTH_REQUIRED", "找不到本地 root 账号"));
        }
        self.run(&format!(
            "SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; ALTER USER {};",
            accounts.join(", ")
        ))
        .map_err(|mut error| {
            if let Some(detail) = error.detail.as_mut() {
                *detail = detail
                    .replace(new_password, "***")
                    .replace(&password, "***");
            }
            error
        })?;
        Ok(())
    }
}

pub(crate) fn validate_create_db(database: &str, username: &str, password: &str) -> Result<()> {
    sanitize_ident(database)?;
    sanitize_ident(username)?;
    if database.len() > 64 || username.len() > 32 {
        return Err(AppError::new(
            "BAD_IDENTIFIER",
            "数据库名最多 64 个字符，用户名最多 32 个字符",
        ));
    }
    if password.is_empty() || password.chars().any(char::is_control) {
        return Err(AppError::new(
            "BAD_PASSWORD",
            "数据库密码不能为空或包含换行等控制字符",
        ));
    }
    Ok(())
}

fn sanitize_ident(s: &str) -> Result<String> {
    if s.is_empty() || s.len() > 64 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(AppError::new(
            "BAD_IDENTIFIER",
            format!("“{s}” 不是合法的标识符（只允许字母数字下划线）"),
        ));
    }
    Ok(s.to_string())
}

/// 生成 .env.example 内容
pub fn render_env_example(db: &str, user: &str, pass: &str, port: u16) -> String {
    let template = format!(
        r#"# NiceEnv 自动生成的本地数据库连接信息
DB_CONNECTION=mysql
DB_HOST=127.0.0.1
DB_PORT={port}
DB_DATABASE={db}
DB_USERNAME={user}
DB_PASSWORD=

# Redis（本应用托管，端口见套件页）
REDIS_HOST=127.0.0.1
REDIS_PORT=null
REDIS_PASSWORD=null
"#
    );
    crate::envfile::apply_env_changes(&template, &[("DB_PASSWORD".into(), pass.into())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_passwords_override_legacy_without_affecting_other_instances() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(temp.path().join("state.sqlite")).unwrap();
        store.set_setting("mysqlRootPassword", "legacy").unwrap();
        store
            .set_setting(&password_key("8.0.46"), "version-one")
            .unwrap();
        assert_eq!(
            saved_password(&store, "8.0.46").as_deref(),
            Some("version-one")
        );
        assert_eq!(saved_password(&store, "8.4.8").as_deref(), Some("legacy"));
        store
            .set_setting(&password_key("8.4.8"), "version-two")
            .unwrap();
        assert_eq!(
            saved_password(&store, "8.0.46").as_deref(),
            Some("version-one")
        );
    }

    #[test]
    fn private_client_config_cleans_up_and_credentials_stay_out_of_arguments() {
        let secret = "quote\" slash\\ # tab\tline\n";
        let example = render_env_example("app", "app_user", secret, 23306);
        let password = crate::envfile::parse_env(&example).into_iter().find(|entry| entry.key == "DB_PASSWORD").unwrap();
        assert_eq!(password.value, secret);
        let (private, command) =
            client_command(Path::new("mysql"), "127.0.0.1", 3306, "root", secret).unwrap();
        let dir = private.path().to_path_buf();
        let args = command.get_args().collect::<Vec<_>>();
        assert!(args[0].to_string_lossy().starts_with("--defaults-file="));
        assert!(args
            .iter()
            .all(|arg| !arg.to_string_lossy().contains(secret)));
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "MYSQL_PWD" && value.is_none()));
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "MYSQL_TEST_LOGIN_FILE" && value.is_some()));
        drop((command, private));
        assert!(!dir.exists());
        assert!(client_command(Path::new("mysql"), "127.0.0.1", 0, "root", "").is_err());
        assert!(
            client_command(Path::new("mysql"), "127.0.0.1", 3306, "root", "bad\0pass").is_err()
        );
    }
}
