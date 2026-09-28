//! MySQL 管理：建库 / 建号授权 / 改 root 密码 / 库表列表（通过已装 mysql 客户端执行）。

use crate::error::{AppError, Result};
use crate::model::{DatabaseInfo, DbUserInfo};
use crate::paths::Paths;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseEngine {
    #[default]
    Mysql,
    Mariadb,
}

impl DatabaseEngine {
    pub fn id(self) -> &'static str {
        match self { Self::Mysql => "mysql", Self::Mariadb => "mariadb" }
    }

    pub fn label(self) -> &'static str {
        match self { Self::Mysql => "MySQL", Self::Mariadb => "MariaDB" }
    }

    pub fn password_key(self, version: &str) -> String {
        format!("{}RootPassword@{version}", self.id())
    }

    pub fn saved_password(self, store: &crate::store::Store, version: &str) -> Option<String> {
        store.get_setting(&self.password_key(version)).or_else(|| {
            (self == Self::Mysql).then(|| store.get_setting("mysqlRootPassword")).flatten()
        })
    }

    pub fn data_dir(self, paths: &Paths, version: &str) -> Result<PathBuf> {
        match self {
            Self::Mysql => Ok(paths.mysql_data_dir(version)),
            Self::Mariadb => crate::generic::mariadb_data_dir(paths, version),
        }
    }

    pub fn bin_dir(self, package: &crate::model::InstalledPackage) -> Result<PathBuf> {
        if self == Self::Mysql {
            return Ok(Path::new(&package.install_path).join(crate::ops::mysql_root_name(&package.version)).join("bin"));
        }
        let entry = crate::install::Installer::bundled().installed_entry(package);
        let exe = Path::new(&package.install_path).join(crate::install::entry_relative_path(&entry.entry));
        exe.parent().map(Path::to_path_buf).ok_or_else(|| AppError::new("DATABASE_TOOL_MISSING", "数据库安装入口无效"))
    }
}

/// 优先使用 MariaDB 的当前工具名，兼容仍保留 MySQL 别名的旧发行包。
pub(crate) fn database_tool(bin_dir: &Path, engine: DatabaseEngine, name: &str) -> PathBuf {
    if engine == DatabaseEngine::Mariadb {
        let modern = match name { "mysql" => "mariadb", "mysqldump" => "mariadb-dump", "mysqladmin" => "mariadb-admin", other => other };
        let path = bin_dir.join(crate::ops::exe_name(modern));
        if path.is_file() { return path; }
    }
    bin_dir.join(crate::ops::exe_name(name))
}

pub fn password_key(version: &str) -> String {
    DatabaseEngine::Mysql.password_key(version)
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
    DatabaseEngine::Mysql.saved_password(store, version)
}

/// 只连接已启动的准确版本；端口使用本次启动记录，避免设置变化后连到另一个实例。
pub fn selected_client(
    state: &crate::CoreState,
    version: Option<&str>,
) -> Result<(String, MySqlClient)> {
    authenticated_client(state, DatabaseEngine::Mysql, version, None)
}

pub(crate) fn authenticated_client(
    state: &crate::CoreState,
    engine: DatabaseEngine,
    version: Option<&str>,
    password: Option<String>,
) -> Result<(String, MySqlClient)> {
    let package = match version {
        Some(version) => state.store.find_installed(engine.id(), Some(version)),
        None => crate::ops::installed_by_choice(&state.store, engine.id()),
    }
    .ok_or_else(|| AppError::not_installed(engine.label()))?;
    let id = if engine == DatabaseEngine::Mysql { format!("mysql@{}", package.version) } else { "mariadb".into() };
    let service = state
        .manager
        .snapshot(&id)
        .filter(|s| s.version.as_deref() == Some(&package.version)
            && matches!(s.state, crate::model::ServiceState::Running | crate::model::ServiceState::Error)
            && s.pids.iter().any(|pid| platform::process_alive(*pid)))
        .ok_or_else(|| {
            AppError::new(
                "MYSQL_NOT_RUNNING",
                format!("{} {} 尚未启动", engine.label(), package.version),
            )
        })?;
    let port = service
        .port
        .ok_or_else(|| AppError::new("MYSQL_NOT_RUNNING", "无法确定数据库实际端口"))?;
    crate::ops::verify_database_listener(&state.manager, &id, port)?;
    let password = password
        .or_else(|| engine.saved_password(&state.store, &package.version))
        .ok_or_else(|| AppError::new("MYSQL_AUTH_REQUIRED", "请先更新该实例的 root 连接密码"))?;
    let client = MySqlClient { exe: database_tool(&engine.bin_dir(&package)?, engine, "mysql"), port, root_password: password };
    client.verify_data_dir(&engine.data_dir(&state.paths, &package.version)?)?;
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
    // portable 客户端的默认插件目录可能指向构建机器；私有配置同时隔离了包内 my.ini。
    // 指定本安装的插件目录，MariaDB 才能加载连接 MySQL 8 所需的 caching_sha2_password。
    if let Some(plugins) = exe.parent().and_then(Path::parent).map(|root| root.join("lib/plugin")).filter(|path| path.is_dir()) {
        command.arg(format!("--plugin-dir={}", plugins.display()));
    }
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

pub fn postgres_password_key(version: &str) -> String {
    format!("postgresPassword@{version}")
}

pub(crate) fn random_database_password() -> String {
    use rand::Rng;
    rand::thread_rng().sample_iter(&rand::distributions::Alphanumeric).take(32).map(char::from).collect()
}

#[derive(serde::Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PostgresConnectionInfo {
    pub version: String,
    pub port: u16,
    pub server_version: String,
    pub database_count: u32,
    pub size_bytes: u64,
    pub password_required: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PostgresDatabaseInfo {
    pub oid: u32,
    pub name: String,
    pub owner: String,
    pub encoding: String,
    pub size_bytes: u64,
    pub protected: bool,
    pub allow_connections: bool,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PostgresRoleInfo {
    pub oid: u32,
    pub name: String,
    pub can_login: bool,
    pub superuser: bool,
    pub create_db: bool,
    pub create_role: bool,
    pub replication: bool,
    pub bypass_rls: bool,
    pub protected: bool,
    pub databases: Vec<String>,
}

fn postgres_ident(name: &str) -> Result<String> {
    if name.is_empty() || name.len() > 63 || name.chars().any(char::is_control) {
        return Err(AppError::new("POSTGRES_BAD_NAME", "名称须为 1–63 字节且不能包含控制字符"));
    }
    Ok(format!("\"{}\"", name.replace('"', "\"\"")))
}

fn postgres_new_name(name: &str) -> Result<String> {
    if !name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return Err(AppError::new("POSTGRES_BAD_NAME", "新名称仅支持字母、数字和下划线，最多 63 字符"));
    }
    let reserved = name.to_ascii_lowercase();
    if reserved.starts_with("pg_") || ["postgres", "template0", "template1"].contains(&reserved.as_str()) {
        return Err(AppError::new("POSTGRES_PROTECTED", "此名称保留给系统使用，请换一个名称"));
    }
    postgres_ident(name)
}

pub struct PostgresClient {
    pub exe: PathBuf,
    pub port: u16,
    pub password: String,
}

impl PostgresClient {
    /// 密码仅写入私有 pgpass；不继承 PGHOST/PGSERVICE/PGOPTIONS 等外部连接配置。
    pub(crate) fn command(&self) -> Result<(tempfile::TempDir, Command)> {
        if self.port == 0 || self.password.chars().any(char::is_control) {
            return Err(AppError::new("BAD_CONNECTION", "PostgreSQL 连接参数无效"));
        }
        let private = tempfile::Builder::new().prefix("niceenv-postgres-").tempdir()?;
        let passfile = private.path().join("pgpass");
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&passfile)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let escaped = self.password.replace('\\', "\\\\").replace(':', "\\:");
        writeln!(file, "127.0.0.1:{}:postgres:postgres:{escaped}", self.port)?;
        file.sync_all()?;
        let mut command = platform::command(&self.exe);
        for (name, _) in std::env::vars_os().filter(|(name, _)| name.to_string_lossy().to_ascii_uppercase().starts_with("PG")) { command.env_remove(name); }
        command.args(["--no-psqlrc", "--no-password", "--no-align", "--tuples-only", "--set=ON_ERROR_STOP=1", "--host=127.0.0.1", "--username=postgres", "--dbname=postgres"])
            .arg(format!("--port={}", self.port))
            .env("PGPASSFILE", passfile).env("PGCONNECT_TIMEOUT", "5").env("PGCLIENTENCODING", "UTF8")
            .env("PGAPPNAME", "NiceEnv").env("LC_ALL", "C").env("PGOPTIONS", "-c statement_timeout=10000 -c lock_timeout=5000");
        Ok((private, command))
    }

    pub(crate) fn query(&self, sql: &str) -> Result<String> {
        let (_private, mut command) = self.command()?;
        let mut input = tempfile::tempfile()?;
        input.write_all(sql.as_bytes())?; input.rewind()?;
        let mut output = tempfile::tempfile()?; let mut error = tempfile::tempfile()?;
        command.stdin(Stdio::from(input)).stdout(output.try_clone()?).stderr(error.try_clone()?);
        let status = wait_client(&mut command, Duration::from_secs(20), || {})?;
        if !status.success() {
            let mut detail = read_output(&mut error, 16 * 1024)?;
            if !self.password.is_empty() { detail = detail.replace(&self.password, "***"); }
            return Err(AppError::new("POSTGRES_CONNECTION_FAILED", "无法连接或操作 PostgreSQL 实例")
                .with_hint("请确认实例正在运行，并在连接设置中更新 postgres 账号密码。")
                .with_detail(detail));
        }
        read_output(&mut output, 1024 * 1024)
    }

    pub(crate) fn verify_data_dir(&self, expected: &Path) -> Result<()> {
        let path = self.query("SHOW data_directory;")?;
        let canonical = |path: &Path| path.canonicalize().map_err(|error| AppError::io("核对 PostgreSQL 数据目录", error));
        if canonical(Path::new(path.trim()))? != canonical(expected)? {
            return Err(AppError::new("POSTGRES_INSTANCE_MISMATCH", "端口上的 PostgreSQL 与所选数据目录不一致，操作已中止"));
        }
        Ok(())
    }

    /// trust 等免密规则下，不能把任意输入密码声称为已验证。
    pub(crate) fn password_required(&self) -> Result<bool> {
        self.query("SELECT 1;")?;
        let probe = Self { exe: self.exe.clone(), port: self.port, password: random_database_password() };
        match probe.query("SELECT 1;") {
            Ok(_) => Ok(false),
            Err(error) if error.code == "POSTGRES_CONNECTION_FAILED"
                && error.detail.as_deref().is_some_and(|detail| detail.contains("password authentication failed for user")) => {
                self.query("SELECT 1;")?; Ok(true)
            }
            Err(error) => Err(AppError::new("POSTGRES_AUTH_UNCONFIRMED", "未能确认 PostgreSQL 是否强制要求密码")
                .with_hint("请检查连接稳定性和实例认证日志；不会把网络故障或无法识别的认证错误当作密码验证成功。")
                .with_detail(error.detail.unwrap_or(error.message))),
        }
    }

    pub(crate) fn info(&self, version: &str) -> Result<PostgresConnectionInfo> {
        #[derive(serde::Deserialize)]
        struct Summary { server_version: String, database_count: u32, size_bytes: u64 }
        let out = self.query("SELECT json_build_object('server_version', current_setting('server_version'), 'database_count', COUNT(*), 'size_bytes', COALESCE(SUM(pg_database_size(oid)), 0)) FROM pg_database WHERE NOT datistemplate;")?;
        let summary: Summary = serde_json::from_str(out.trim()).map_err(|error| AppError::new("POSTGRES_RESPONSE", "PostgreSQL 统计响应无法识别").with_detail(error.to_string()))?;
        Ok(PostgresConnectionInfo { version: version.into(), port: self.port, server_version: summary.server_version,
            database_count: summary.database_count, size_bytes: summary.size_bytes, password_required: self.password_required()? })
    }

    pub(crate) fn change_password(&self, password: &str) -> Result<()> {
        if password.is_empty() || password.chars().any(char::is_control) {
            return Err(AppError::new("BAD_PASSWORD", "密码不能为空或包含控制字符"));
        }
        self.password_statement("ALTER ROLE postgres", password)
    }

    fn password_statement(&self, prefix: &str, password: &str) -> Result<()> {
        if password.is_empty() || password.len() > 4096 || password.chars().any(char::is_control) {
            return Err(AppError::new("BAD_PASSWORD", "密码须为 1–4096 字节且不能包含控制字符"));
        }
        let literal = password.replace('\\', "\\\\").replace('\'', "''");
        // psql 逐条提交语句，先关闭本会话的语句记录，再发送改密语句。
        let sql = format!("SET log_statement='none';\nSET log_min_error_statement='panic';\nSET log_min_duration_statement=-1;\nSET log_duration=off;\nSELECT set_config('log_min_duration_sample','-1',false) WHERE current_setting('log_min_duration_sample',true) IS NOT NULL;\nSELECT set_config('log_transaction_sample_rate','0',false) WHERE current_setting('log_transaction_sample_rate',true) IS NOT NULL;\nSET password_encryption='scram-sha-256';\n{prefix} PASSWORD E'{literal}';\n");
        self.query(&sql).map(|_| ()).map_err(|mut error| {
            if let Some(detail) = error.detail.as_mut() { *detail = detail.replace(password, "***").replace(&literal, "***"); }
            error
        })
    }

    pub fn list_databases(&self) -> Result<Vec<PostgresDatabaseInfo>> {
        let out = self.query("SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.name), '[]'::json) FROM (SELECT oid::bigint AS oid, datname AS name, pg_get_userbyid(datdba) AS owner, pg_encoding_to_char(encoding) AS encoding, pg_database_size(oid) AS \"sizeBytes\", (datistemplate OR datname IN ('postgres', 'template0', 'template1') OR datname = current_database()) AS protected, datallowconn AS \"allowConnections\" FROM pg_database) d;")?;
        serde_json::from_str(out.trim()).map_err(|error| AppError::new("POSTGRES_RESPONSE", "无法读取 PostgreSQL 数据库列表").with_detail(error.to_string()))
    }

    pub fn list_roles(&self) -> Result<Vec<PostgresRoleInfo>> {
        let out = self.query("SELECT COALESCE(json_agg(row_to_json(r) ORDER BY r.name), '[]'::json) FROM (SELECT oid::bigint AS oid, rolname AS name, rolcanlogin AS \"canLogin\", rolsuper AS superuser, rolcreatedb AS \"createDb\", rolcreaterole AS \"createRole\", rolreplication AS replication, rolbypassrls AS \"bypassRls\", (rolsuper OR rolname IN ('postgres', current_user, session_user) OR starts_with(rolname, 'pg_')) AS protected, ARRAY(SELECT datname FROM pg_database WHERE datdba = pg_roles.oid ORDER BY datname) AS databases FROM pg_roles WHERE NOT starts_with(rolname, 'pg_')) r;")?;
        serde_json::from_str(out.trim()).map_err(|error| AppError::new("POSTGRES_RESPONSE", "无法读取 PostgreSQL 账号列表").with_detail(error.to_string()))
    }

    pub fn create_database(&self, name: &str, owner: &str) -> Result<()> {
        let identifier = postgres_new_name(name)?;
        let owner_identifier = postgres_ident(owner)?;
        if self.list_databases()?.iter().any(|db| db.name == name) {
            return Err(AppError::new("POSTGRES_DATABASE_EXISTS", "同名数据库已存在，请刷新列表或更换名称"));
        }
        if !self.list_roles()?.iter().any(|role| role.name == owner && role.can_login) {
            return Err(AppError::new("POSTGRES_OWNER_CHANGED", "所选数据库所有者不存在或无法登录，请重新选择"));
        }
        // CREATE DATABASE 不能置于事务中；不复制 template1 的用户对象，也不悄悄复用同名库。
        self.query(&format!("CREATE DATABASE {identifier} OWNER {owner_identifier} TEMPLATE template0 ENCODING 'UTF8';"))?;
        Ok(())
    }

    pub fn drop_database(&self, name: &str, oid: u32) -> Result<()> {
        let identifier = postgres_ident(name)?;
        let target = self.list_databases()?.into_iter().find(|db| db.name == name && db.oid == oid)
            .ok_or_else(|| AppError::new("POSTGRES_TARGET_CHANGED", "数据库已删除或发生变化，请刷新列表后重新确认"))?;
        if target.protected { return Err(AppError::new("POSTGRES_PROTECTED", "不能删除 PostgreSQL 系统库或模板库")); }
        // 不使用 FORCE，也不终止连接；数据库仍被使用时由服务器拒绝删除。
        self.query(&format!("DROP DATABASE {identifier};")).map_err(|error| error.with_hint("请刷新确认数据库状态；仍有连接时需先关闭应用连接，不会自动强制断开连接。"))?;
        Ok(())
    }

    pub fn create_role(&self, name: &str, password: &str) -> Result<()> {
        let identifier = postgres_new_name(name)?;
        if self.list_roles()?.iter().any(|role| role.name == name) {
            return Err(AppError::new("POSTGRES_ROLE_EXISTS", "同名账号已存在，未修改其密码或权限"));
        }
        self.password_statement(&format!("CREATE ROLE {identifier} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"), password)
    }

    fn ordinary_role(&self, name: &str, oid: u32) -> Result<PostgresRoleInfo> {
        let role = self.list_roles()?.into_iter().find(|role| role.name == name && role.oid == oid)
            .ok_or_else(|| AppError::new("POSTGRES_TARGET_CHANGED", "账号已删除或发生变化，请刷新列表后重新确认"))?;
        if role.protected { return Err(AppError::new("POSTGRES_PROTECTED", "此账号属于系统或超级用户，不能在普通账号管理中修改")); }
        Ok(role)
    }

    pub fn set_role_password(&self, name: &str, oid: u32, password: &str) -> Result<()> {
        let identifier = postgres_ident(name)?;
        let role = self.ordinary_role(name, oid)?;
        if !role.can_login { return Err(AppError::new("POSTGRES_ROLE_NOLOGIN", "该角色未启用登录，不能设置登录密码")); }
        self.password_statement(&format!("ALTER ROLE {identifier}"), password)
    }

    pub fn drop_role(&self, name: &str, oid: u32) -> Result<()> {
        let identifier = postgres_ident(name)?;
        self.ordinary_role(name, oid)?;
        self.query(&format!("DROP ROLE {identifier};")).map_err(|error| error.with_hint("账号如仍拥有数据库、对象或权限依赖，需先手动转移或解除依赖；不会删除它拥有的数据。"))?;
        Ok(())
    }

    pub(crate) fn local_auth_update(&self, paths: &Paths, version: &str) -> Result<(PathBuf, String, String)> {
        let file = crate::paths::checked_data_path(&paths.base, &format!("data/postgresql/{version}/pg_hba.conf"))?;
        let actual = self.query("SHOW hba_file;")?;
        if Path::new(actual.trim()).canonicalize()? != file.canonicalize()? {
            return Err(AppError::new("POSTGRES_AUTH_CUSTOM", "实例使用自定义认证配置文件，未自动修改"));
        }
        let previous = std::fs::read_to_string(&file)?;
        let updated = postgres_local_password_rules(&previous)?;
        Ok((file, previous, updated))
    }
}

pub(crate) fn selected_postgres(state: &crate::CoreState, version: &str, password: Option<String>) -> Result<PostgresClient> {
    let package = state.store.find_installed("postgresql", Some(version)).ok_or_else(|| AppError::not_installed("PostgreSQL"))?;
    let service = state.manager.snapshot("postgresql").filter(|service| service.version.as_deref() == Some(version)
        && matches!(service.state, crate::model::ServiceState::Running | crate::model::ServiceState::Error)
        && service.pids.iter().any(|pid| platform::process_alive(*pid)))
        .ok_or_else(|| AppError::new("POSTGRES_NOT_RUNNING", "所选 PostgreSQL 实例未运行或运行版本已变化"))?;
    let port = service.port.ok_or_else(|| AppError::new("POSTGRES_PORT_UNKNOWN", "无法确认 PostgreSQL 实际端口"))?;
    crate::ops::verify_database_listener(&state.manager, "postgresql", port)?;
    let client = PostgresClient {
        exe: Path::new(&package.install_path).join("pgsql/bin").join(crate::ops::exe_name("psql")), port,
        password: match password { Some(value) => value, None => state.store.get_setting_checked(&postgres_password_key(version))?.unwrap_or_default() },
    };
    client.verify_data_dir(&state.paths.postgres_data_dir(version))?;
    Ok(client)
}

/// 只转换 initdb 生成的本机默认 trust 规则，保留注释；复杂/自定义规则交由用户维护。
pub(crate) fn postgres_local_password_rules(content: &str) -> Result<String> {
    let mut seen = std::collections::BTreeSet::new();
    for line in content.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let fields: Vec<_> = line.split_whitespace().collect();
        let key = match fields.as_slice() {
            ["local", database @ ("all" | "replication"), "all", "trust"] => format!("local:{database}"),
            ["host", database @ ("all" | "replication"), "all", address @ ("127.0.0.1/32" | "::1/128"), "trust"] => format!("host:{database}:{address}"),
            _ => return Err(AppError::new("POSTGRES_AUTH_CUSTOM", "pg_hba.conf 包含自定义认证规则，未自动修改")
                .with_hint("请在原配置中检查本机连接的认证方式，再用现有密码更新本机连接记录。")),
        };
        if !seen.insert(key) { return Err(AppError::new("POSTGRES_AUTH_CUSTOM", "pg_hba.conf 存在重复规则，未自动修改")); }
    }
    if !seen.contains("host:all:127.0.0.1/32") || !seen.contains("host:all:::1/128") {
        return Err(AppError::new("POSTGRES_AUTH_CUSTOM", "未找到完整的默认本机认证规则，未自动修改"));
    }
    Ok(content.split_inclusive('\n').map(|line| {
        if line.trim_start().starts_with('#') { line.into() } else {
            line.rfind("trust").map_or_else(|| line.to_string(), |at| format!("{}scram-sha-256{}", &line[..at], &line[at + 5..]))
        }
    }).collect())
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
    fn postgres_private_credentials_and_default_auth_rules() {
        assert!(postgres_new_name(&"a".repeat(64)).is_err());
        assert!(postgres_new_name("pg_monitor").is_err());
        assert!(postgres_new_name("bad\nname").is_err());
        assert_eq!(postgres_ident("quote\";sql").unwrap(), "\"quote\"\";sql\"");
        let client = PostgresClient { exe: PathBuf::from("psql"), port: 25432, password: "colon:slash\\quote'密码".into() };
        let (private, command) = client.command().unwrap();
        assert!(command.get_args().all(|value| !value.to_string_lossy().contains(&client.password)));
        assert_eq!(std::fs::read_to_string(private.path().join("pgpass")).unwrap(), "127.0.0.1:25432:postgres:postgres:colon\\:slash\\\\quote'密码\n");
        assert!(command.get_args().any(|value| value == "--no-psqlrc"));
        assert!(command.get_args().any(|value| value == "--no-password"));
        assert!(command.get_envs().any(|(key, value)| key == "PGPASSFILE" && value.is_some()));
        let directory = private.path().to_path_buf(); drop(private);
        assert!(!directory.exists());
        let default = "# trust comments stay\nlocal all all trust\nhost all all 127.0.0.1/32 trust\nhost all all ::1/128 trust\nlocal replication all trust\nhost replication all 127.0.0.1/32 trust\nhost replication all ::1/128 trust\n";
        let updated = postgres_local_password_rules(default).unwrap();
        assert_eq!(updated.matches("scram-sha-256").count(), 6);
        assert!(updated.starts_with("# trust comments stay"));
        for custom in [format!("{default}host all all 0.0.0.0/0 trust\n"), format!("{default}include other.conf\n"), format!("{default}local all all trust\n"), "host all all 127.0.0.1/32 trust".into()] {
            assert_eq!(postgres_local_password_rules(&custom).unwrap_err().code, "POSTGRES_AUTH_CUSTOM");
        }
        assert_eq!(PostgresClient { password: "bad\npass".into(), ..client }.command().unwrap_err().code, "BAD_CONNECTION");
    }

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
        assert!(DatabaseEngine::Mariadb.saved_password(&store, "8.0.46").is_none());
        store.set_setting(&DatabaseEngine::Mariadb.password_key("8.0.46"), "mariadb-only").unwrap();
        assert_eq!(DatabaseEngine::Mariadb.saved_password(&store, "8.0.46").as_deref(), Some("mariadb-only"));
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
