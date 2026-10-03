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
        .filter(|s| s.version.as_deref().is_some_and(|version| crate::install::same_version(version, &package.version))
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
        .arg(format!("--defaults-file={}", crate::paths::portable_path_text(&defaults)))
        .args([
            "--protocol=TCP",
            "--host",
            host,
            "--port",
            &port.to_string(),
        ])
        .env(
            "MYSQL_TEST_LOGIN_FILE",
            crate::paths::portable_path_text(&private.path().join("unused.mylogin.cnf")),
        )
        .env_remove("MYSQL_PWD");
    // portable 客户端的默认插件目录可能指向构建机器；私有配置同时隔离了包内 my.ini。
    // 指定本安装的插件目录，MariaDB 才能加载连接 MySQL 8 所需的 caching_sha2_password。
    if let Some(plugins) = exe.parent().and_then(Path::parent).map(|root| root.join("lib/plugin")).filter(|path| path.is_dir()) {
        command.arg(format!("--plugin-dir={}", crate::paths::portable_path_text(&plugins)));
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

/// Database-level privileges supported by the installed server are discovered from mysql.db.
const DATABASE_PRIVILEGES: &[(&str, &str)] = &[
    ("Select_priv", "SELECT"), ("Insert_priv", "INSERT"), ("Update_priv", "UPDATE"), ("Delete_priv", "DELETE"),
    ("Create_priv", "CREATE"), ("Drop_priv", "DROP"), ("References_priv", "REFERENCES"), ("Index_priv", "INDEX"),
    ("Alter_priv", "ALTER"), ("Create_tmp_table_priv", "CREATE TEMPORARY TABLES"), ("Lock_tables_priv", "LOCK TABLES"),
    ("Create_view_priv", "CREATE VIEW"), ("Show_view_priv", "SHOW VIEW"), ("Create_routine_priv", "CREATE ROUTINE"),
    ("Alter_routine_priv", "ALTER ROUTINE"), ("Execute_priv", "EXECUTE"), ("Event_priv", "EVENT"),
    ("Trigger_priv", "TRIGGER"), ("Delete_history_priv", "DELETE HISTORY"),
];
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseGrantScope {
    pub scope: String,
    pub label: String,
    pub pattern: bool,
    pub protected: bool,
    pub privileges: Vec<String>,
    pub grant_option: bool,
    pub extra_privileges: Vec<String>,
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseGrants {
    pub username: String,
    pub host: String,
    pub scopes: Vec<DatabaseGrantScope>,
    pub databases: Vec<String>,
    pub available: Vec<String>,
    pub partial_revokes: bool,
    pub mariadb: bool,
    pub global_privileges: bool,
    pub protected: bool,
    pub revision: String,
}
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseGrantInput {
    pub username: String,
    pub host: String,
    pub target: String,
    pub new_database: bool,
    pub privileges: Vec<String>,
    pub grant_option: bool,
    pub revision: String,
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseUserPasswordInfo {
    pub username: String,
    pub host: String,
    pub plugins: Vec<String>,
    pub target_plugin: String,
    pub supported: bool,
    pub protected: bool,
    pub other_authentication: bool,
    pub revision: String,
}

// 不派生 Debug/Serialize，避免密码被当作普通请求内容输出。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseUserPasswordInput {
    pub username: String,
    pub host: String,
    pub password: String,
    pub revision: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseUserDependency {
    pub kind: String,
    pub database: String,
    pub name: String,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseUserDropInfo {
    pub username: String,
    pub host: String,
    pub protected: bool,
    pub dependencies: Vec<DatabaseUserDependency>,
    pub more_dependencies: bool,
    pub role_dependents: u64,
    pub proxy_dependents: u64,
    pub username_connections: u64,
    pub revision: String,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseUserDropInput {
    pub username: String,
    pub host: String,
    pub confirmation: String,
    pub revision: String,
}

pub fn drop_database_user(state: &crate::CoreState, engine: DatabaseEngine, version: &str, input: &DatabaseUserDropInput) -> Result<()> {
    let _work = crate::BackgroundWork::begin("删除数据库账号")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    if version.is_empty() || version.len() > 64 || !version.bytes().all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c)) {
        return Err(AppError::new("BAD_VERSION", "数据库版本无效"));
    }
    let path = crate::paths::checked_data_path(&state.paths.base, &format!("etc/.db-user-drop-{}-{version}.lock", engine.id()))?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => AppError::new("DB_USER_DROP_BUSY", "此实例正在删除账号，请稍后再试"),
        std::fs::TryLockError::Error(error) => AppError::io("锁定账号删除操作", error),
    })?;
    state.with_database(engine, Some(version), |_, client| client.drop_user(input))
}

pub fn update_database_user_password(state: &crate::CoreState, engine: DatabaseEngine, version: &str, input: &DatabaseUserPasswordInput) -> Result<DatabaseUserPasswordInfo> {
    let _work = crate::BackgroundWork::begin("修改数据库账号密码")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    if version.is_empty() || version.len() > 64 || !version.bytes().all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c)) {
        return Err(AppError::new("BAD_VERSION", "数据库版本无效"));
    }
    let path = crate::paths::checked_data_path(&state.paths.base, &format!("etc/.db-user-password-{}-{version}.lock", engine.id()))?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => AppError::new("DB_PASSWORD_BUSY", "此实例正在修改账号密码，请稍后再试"),
        std::fs::TryLockError::Error(error) => AppError::io("锁定账号密码操作", error),
    })?;
    state.with_database(engine, Some(version), |_, client| client.set_user_password(input))
}

fn account_filter(username: &str, host: &str) -> Result<String> {
    if username.len() > 384 || host.len() > 1020 || username.chars().chain(host.chars()).any(char::is_control) {
        return Err(AppError::new("BAD_ACCOUNT", "账号名称或来源主机无效"));
    }
    Ok(format!("HEX(User)='{}' AND HEX(Host)='{}'", hex::encode_upper(username), hex::encode_upper(host)))
}
fn protected_account(username: &str, host: &str) -> bool {
    username.is_empty() || host.is_empty() || username.eq_ignore_ascii_case("root")
        || username.to_ascii_lowercase().starts_with("mysql.") || username.eq_ignore_ascii_case("mariadb.sys")
}
fn grant_literal(database: &str, partial_revokes: bool) -> String {
    if partial_revokes { database.into() } else { database.replace('\\', "\\\\").replace('_', "\\_").replace('%', "\\%") }
}
fn grant_scope_details(scope: &str, partial_revokes: bool) -> Result<(String, bool, bool)> {
    let mut pattern = false;
    let mut label = String::new();
    let mut expression = String::from("(?i)^");
    let mut chars = scope.chars();
    while let Some(c) = chars.next() {
        if !partial_revokes && c == '\\' {
            let c = chars.next().unwrap_or('\\'); label.push(c); expression.push_str(&regex::escape(&c.to_string()));
        } else if !partial_revokes && (c == '%' || c == '_') {
            pattern = true; label.push(c); expression.push_str(if c == '%' { ".*" } else { "." });
        } else { label.push(c); expression.push_str(&regex::escape(&c.to_string())); }
    }
    expression.push('$');
    let matcher = regex::Regex::new(&expression).map_err(|e| AppError::internal("读取授权范围", e.to_string()))?;
    let protected = ["mysql", "sys", "information_schema", "performance_schema"].iter().any(|name| matcher.is_match(name));
    Ok((label, pattern, protected))
}
fn mysql_identifier(name: &str) -> String { format!("`{}`", name.replace('`', "``")) }
fn decode_hex_field(value: &str) -> Result<String> {
    String::from_utf8(hex::decode(value).map_err(|_| AppError::new("MYSQL_RESPONSE", "账号授权响应格式错误"))?)
        .map_err(|_| AppError::new("MYSQL_RESPONSE", "账号授权响应编码无效"))
}

/// 同一实例的多窗口/多进程保存串行化；外部 SQL 修改通过 revision 检测，不宣称 GRANT 可事务回滚。
pub fn update_database_grants(state: &crate::CoreState, engine: DatabaseEngine, version: &str, input: &DatabaseGrantInput) -> Result<DatabaseGrants> {
    let _work = crate::BackgroundWork::begin("修改数据库授权")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    if version.is_empty() || version.len() > 64 || !version.bytes().all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c)) {
        return Err(AppError::new("BAD_VERSION", "数据库版本无效"));
    }
    let path = crate::paths::checked_data_path(&state.paths.base, &format!("etc/.db-grants-{}-{version}.lock", engine.id()))?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
    lock.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => AppError::new("DATABASE_GRANTS_BUSY", "此实例正在保存账号授权，请稍后重试"),
        std::fs::TryLockError::Error(error) => AppError::io("锁定数据库授权", error),
    })?;
    state.with_database(engine, Some(version), |_, client| client.apply_grants(input))
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
        let d = mysql_identifier(&grant_literal(database, self.partial_revokes()?));
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
        let server = self.run("SELECT @@version;")?;
        let grant_mode = if server.to_ascii_lowercase().contains("mariadb") || server.starts_with("5.") { "NO_BACKSLASH_ESCAPES,NO_AUTO_CREATE_USER" } else { "NO_BACKSLASH_ESCAPES" };
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
            "SET SESSION sql_mode='{grant_mode}'; GRANT ALL PRIVILEGES ON {d}.* TO '{u}'@'127.0.0.1'; \
             GRANT ALL PRIVILEGES ON {d}.* TO '{u}'@'localhost';"
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
        let out = self.run("SELECT HEX(user), HEX(host) FROM mysql.user ORDER BY user, host;")?;
        out.lines().filter(|line| !line.is_empty()).map(|line| {
            let (user, host) = line.split_once('\t').ok_or_else(|| AppError::new("MYSQL_RESPONSE", "账号列表响应不完整"))?;
            Ok(DbUserInfo { username: decode_hex_field(user)?, host: decode_hex_field(host)?, grants: None })
        }).collect()
    }

    pub fn user_drop_info(&self, username: &str, host: &str) -> Result<DatabaseUserDropInfo> {
        use sha2::{Digest, Sha256};
        let filter = account_filter(username, host)?;
        let user_hex = hex::encode_upper(username);
        let host_hex = hex::encode_upper(host);
        let tables = self.run("SHOW TABLES FROM mysql;")?.lines().map(str::to_string).collect::<Vec<_>>();
        let count = |sql: &str| -> Result<u64> { self.run(sql)?.trim().parse().map_err(|_| AppError::new("MYSQL_RESPONSE", "账号依赖计数无法识别")) };
        let mut snapshots = Vec::new();
        let mut protected = protected_account(username, host);
        // 账号与授权表逐表确认结构；认证原文只参与服务器内哈希，不返回本机。
        for (table, columns, predicate) in [
            ("user", vec!["User", "Host"], filter.clone()),
            ("global_priv", vec!["User", "Host"], filter.clone()),
            ("db", vec!["User", "Host"], filter.clone()),
            ("tables_priv", vec!["User", "Host"], filter.clone()),
            ("columns_priv", vec!["User", "Host"], filter.clone()),
            ("procs_priv", vec!["User", "Host"], filter.clone()),
            ("global_grants", vec!["User", "Host", "Priv"], filter.clone()),
            ("default_roles", vec!["User", "Host"], filter.clone()),
            ("role_edges", vec!["FROM_USER", "FROM_HOST", "TO_USER", "TO_HOST"], format!("(HEX(FROM_USER)='{user_hex}' AND HEX(FROM_HOST)='{host_hex}') OR (HEX(TO_USER)='{user_hex}' AND HEX(TO_HOST)='{host_hex}')")),
            ("roles_mapping", vec!["User", "Host", "Role"], format!("({filter}) OR HEX(Role)='{user_hex}'")),
            ("proxies_priv", vec!["User", "Host", "Proxied_user", "Proxied_host"], format!("({filter}) OR (HEX(Proxied_user)='{user_hex}' AND HEX(Proxied_host)='{host_hex}')")),
        ] {
            if !tables.iter().any(|name| name == table) {
                if ["user", "db", "tables_priv", "columns_priv", "procs_priv", "proxies_priv"].contains(&table) { return Err(AppError::new("DB_USER_DROP_UNSUPPORTED", "授权表不完整，不能安全检查账号删除影响")); }
                continue;
            }
            let fields = self.run(&format!("SHOW COLUMNS FROM mysql.{};", mysql_identifier(table)))?.lines().filter_map(|line| line.split('\t').next()).map(str::to_string).collect::<Vec<_>>();
            if fields.is_empty() || columns.iter().any(|expected| !fields.iter().any(|field| field.eq_ignore_ascii_case(expected))) {
                return Err(AppError::new("DB_USER_DROP_UNSUPPORTED", "当前授权表结构不支持账号删除检查"));
            }
            // HEX 同时保留 NULL，并避免 BLOB/二进制字符集被 JSON_ARRAY 拒绝。
            let values = fields.iter().map(|field| format!("HEX({})", mysql_identifier(field))).collect::<Vec<_>>().join(",");
            let digest = self.run(&format!("SELECT SHA2(JSON_ARRAY({values}),256) FROM mysql.{} WHERE {predicate} ORDER BY 1;", mysql_identifier(table)))?;
            if table == "user" {
                if digest.trim().is_empty() { return Err(AppError::new("DB_USER_MISSING", "所选账号已不存在，请刷新列表")); }
                let metadata_privileges = ["Select_priv", "Show_view_priv", "Trigger_priv", "Event_priv"];
                let restrictions = if fields.iter().any(|field| field.eq_ignore_ascii_case("User_attributes")) { " AND COALESCE(JSON_LENGTH(User_attributes,'$.Restrictions'),0)=0" } else { "" };
                if metadata_privileges.iter().any(|required| !fields.iter().any(|field| field.eq_ignore_ascii_case(required)))
                    || count(&format!("SELECT COUNT(*) FROM mysql.user WHERE HEX(CONCAT(User,'@',Host))=HEX(CURRENT_USER()) AND {}{restrictions};", metadata_privileges.iter().map(|name| format!("{name}='Y'")).collect::<Vec<_>>().join(" AND ")))? != 1 {
                    return Err(AppError::new("DB_USER_METADATA_ACCESS", "当前管理连接缺少完整的对象查看权限，不能确认删除影响").with_hint("请用具备全局 SELECT、SHOW VIEW、TRIGGER、EVENT 权限的管理连接重新检查。"));
                }
                let admin = fields.iter().filter(|name| ["super_priv", "create_user_priv"].contains(&name.to_ascii_lowercase().as_str())).map(|name| format!("{}='Y'", mysql_identifier(name))).collect::<Vec<_>>();
                if !admin.is_empty() { protected |= count(&format!("SELECT COUNT(*) FROM mysql.user WHERE {filter} AND ({});", admin.join(" OR ")))? > 0; }
            } else if table == "global_grants" {
                protected |= count(&format!("SELECT COUNT(*) FROM mysql.global_grants WHERE {filter} AND Priv IN ('SYSTEM_USER','ROLE_ADMIN');"))? > 0;
            }
            // 防止客户端输出上限导致不完整指纹被当成有效快照。
            if digest.lines().any(|line| line.len() != 64 || !line.bytes().all(|c| c.is_ascii_hexdigit())) || digest.len() >= 16 * 1024 * 1024 {
                return Err(AppError::new("MYSQL_RESPONSE", "账号授权快照不完整，请使用数据库客户端管理"));
            }
            snapshots.push((table, digest));
        }
        let definer = hex::encode_upper(format!("{username}@{host}"));
        let mut queries = Vec::new();
        for (kind, table, database, name) in [("view", "VIEWS", "TABLE_SCHEMA", "TABLE_NAME"), ("routine", "ROUTINES", "ROUTINE_SCHEMA", "ROUTINE_NAME"), ("trigger", "TRIGGERS", "TRIGGER_SCHEMA", "TRIGGER_NAME"), ("event", "EVENTS", "EVENT_SCHEMA", "EVENT_NAME")] {
            let fields = self.run(&format!("SHOW COLUMNS FROM information_schema.{table};"))?;
            let fields = fields.lines().filter_map(|line| line.split('\t').next()).collect::<Vec<_>>();
            if [database, name, "DEFINER"].iter().any(|required| !fields.contains(required)) { return Err(AppError::new("DB_USER_DROP_UNSUPPORTED", "无法完整检查账号关联的数据库对象")); }
            queries.push(format!("SELECT '{kind}' AS kind, HEX({database}) AS db_name, HEX({name}) AS object_name FROM information_schema.{table} WHERE HEX(DEFINER)='{definer}'"));
        }
        let rows = self.run(&format!("{} ORDER BY kind, db_name, object_name LIMIT 51;", queries.join(" UNION ALL ")))?;
        let mut dependencies = Vec::new();
        for line in rows.lines().filter(|line| !line.is_empty()) {
            let fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() != 3 { return Err(AppError::new("MYSQL_RESPONSE", "账号关联对象响应不完整")); }
            dependencies.push(DatabaseUserDependency { kind: fields[0].into(), database: decode_hex_field(fields[1])?, name: decode_hex_field(fields[2])? });
        }
        let role_dependents = if tables.iter().any(|name| name == "role_edges") { count(&format!("SELECT COUNT(*) FROM mysql.role_edges WHERE HEX(FROM_USER)='{user_hex}' AND HEX(FROM_HOST)='{host_hex}';"))? }
            else if tables.iter().any(|name| name == "roles_mapping") && host.is_empty() { count(&format!("SELECT COUNT(*) FROM mysql.roles_mapping WHERE HEX(Role)='{user_hex}';"))? } else { 0 };
        let proxy_dependents = count(&format!("SELECT COUNT(*) FROM mysql.proxies_priv WHERE HEX(Proxied_user)='{user_hex}' AND HEX(Proxied_host)='{host_hex}' AND NOT ({filter});"))?;
        let mut info = DatabaseUserDropInfo { username: username.into(), host: host.into(), protected, more_dependencies: dependencies.len() > 50, dependencies,
            role_dependents, proxy_dependents, username_connections: 0, revision: String::new() };
        info.dependencies.truncate(50);
        static SNAPSHOT_KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
        let mut digest = Sha256::new(); digest.update(SNAPSHOT_KEY.get_or_init(rand::random));
        digest.update(self.port.to_be_bytes()); digest.update(self.exe.to_string_lossy().as_bytes());
        digest.update(self.run("SELECT @@version;")?.as_bytes());
        digest.update(serde_json::to_vec(&(snapshots, &info)).map_err(|error| AppError::internal("记录账号删除版本", error.to_string()))?);
        info.revision = hex::encode(digest.finalize());
        // PROCESSLIST 的 Host 是客户端来源，不能等同授权账号的 Host；此处仅报告同用户名总数。
        info.username_connections = count(&format!("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE HEX(USER)='{user_hex}';"))?;
        Ok(info)
    }

    fn drop_user(&self, input: &DatabaseUserDropInput) -> Result<()> {
        if input.confirmation != format!("{}@{}", input.username, input.host) { return Err(AppError::new("DB_USER_CONFIRM", "请完整输入要删除的账号及来源主机")); }
        let before = self.user_drop_info(&input.username, &input.host)?;
        if before.protected { return Err(AppError::new("SYSTEM_ACCOUNT", "系统账号或具有账号管理权限的账号受保护")); }
        if !before.dependencies.is_empty() || before.role_dependents > 0 || before.proxy_dependents > 0 {
            return Err(AppError::new("DB_USER_DEPENDENCIES", "账号仍被数据库对象、角色或代理授权使用，请先转移或解除依赖"));
        }
        if before.revision != input.revision { return Err(AppError::new("DB_USER_CHANGED", "账号、授权或依赖已变化，请重新读取并确认删除范围")); }
        let account = format!("'{}'@'{}'", input.username.replace('\'', "''"), input.host.replace('\'', "''"));
        // 单个精确账号，不使用 IF EXISTS、FORCE 或 KILL；不删除业务对象。
        // 外部管理工具不受应用锁约束；服务器仍执行自己的权限和依赖校验。
        let result = self.run(&format!("SET SESSION sql_mode=CONCAT_WS(',',NULLIF(@@SESSION.sql_mode,''),'NO_BACKSLASH_ESCAPES'); DROP USER {account};"));
        let filter = account_filter(&input.username, &input.host)?;
        let remains = self.run(&format!("SELECT COUNT(*) FROM mysql.user WHERE {filter};"))
            .map_err(|error| error.with_hint("删除语句已发送，但无法确认当前状态；请刷新账号列表后再操作。"))?;
        if remains.trim() == "0" { return Ok(()); }
        result.map_err(|error| error.with_hint("未能确认账号已删除，请重新读取账号与依赖信息。已有连接不会被自动断开。"))?;
        Err(AppError::new("DB_USER_DROP_VERIFY", "删除后仍读到同名账号，请重新读取确认，勿反复提交"))
    }

    pub fn user_password_info(&self, username: &str, host: &str) -> Result<DatabaseUserPasswordInfo> {
        use sha2::{Digest, Sha256};
        let filter = account_filter(username, host)?;
        let server = self.run("SELECT @@version;")?;
        let mariadb = server.to_ascii_lowercase().contains("mariadb");
        let columns = self.run("SHOW COLUMNS FROM mysql.user;")?.lines().filter_map(|line| line.split('\t').next()).map(str::to_string).collect::<Vec<_>>();
        if !columns.iter().any(|column| column == "plugin") || !columns.iter().any(|column| column == "authentication_string") {
            return Err(AppError::new("DB_PASSWORD_UNSUPPORTED", "此服务器认证结构不支持账号密码编辑"));
        }
        let attributes = if columns.iter().any(|column| column == "User_attributes") { "COALESCE(User_attributes,'{}')" } else { "'{}'" };
        // 原始认证串不离开数据库；后端仅接收服务器内计算的指纹，不将此指纹返回界面。
        let rows = self.run(&format!("SELECT HEX(plugin), SHA2(CONCAT(COALESCE(authentication_string,''), {attributes}),256), JSON_CONTAINS_PATH({attributes}, 'one', '$.additional_password', '$.multi_factor_authentication') FROM mysql.user WHERE {filter};"))?;
        let fields = rows.trim_end().split('\t').collect::<Vec<_>>();
        if rows.trim().is_empty() { return Err(AppError::new("DB_USER_MISSING", "所选账号已不存在，请刷新列表")); }
        if fields.len() != 3 { return Err(AppError::new("MYSQL_RESPONSE", "账号认证响应不完整")); }
        let primary = decode_hex_field(fields[0])?;
        let mut fingerprint = fields[1].to_string();
        let mut plugins = vec![primary.clone()];
        let mut other_authentication = fields[2] == "1";
        if mariadb && self.run("SHOW TABLES FROM mysql LIKE 'global_priv';")?.trim() == "global_priv" {
            let row = self.run(&format!("SELECT COALESCE(JSON_LENGTH(Priv,'$.auth_or'),0), SHA2(Priv,256) FROM mysql.global_priv WHERE {filter};"))?;
            let (count, digest) = row.trim_end().split_once('\t').ok_or_else(|| AppError::new("DB_USER_MISSING", "账号已变化，请重新读取"))?;
            let count = count.parse::<usize>().ok().filter(|count| *count <= 256).ok_or_else(|| AppError::new("MYSQL_RESPONSE", "账号认证方式数量无法识别"))?;
            if count > 0 {
                // 通配 JSON 路径会跳过 {} 占位项；逐项读取 plugin，保留主插件占位与认证顺序。
                let values = (0..count).map(|index| format!("JSON_EXTRACT(Priv,'$.auth_or[{index}].plugin')")).collect::<Vec<_>>().join(",");
                let encoded = self.run(&format!("SELECT HEX(JSON_ARRAY({values})) FROM mysql.global_priv WHERE {filter};"))?;
                let alternatives: Vec<Option<String>> = serde_json::from_str(&decode_hex_field(encoded.trim())?)
                    .map_err(|error| AppError::new("MYSQL_RESPONSE", "无法读取账号认证方式").with_detail(error.to_string()))?;
                plugins = alternatives.into_iter().map(|plugin| plugin.unwrap_or_else(|| primary.clone())).collect();
            }
            fingerprint = digest.to_string();
            other_authentication = plugins.len() > 1;
        }
        // MariaDB 11.4 的外部插件在前时，SET PASSWORD 可能返回错误但仍改动后续密码。
        // 仅支持认证链首项就是已知密码插件的组合，不跳过外部或未知插件。
        let target_plugin = if mariadb { plugins.first().cloned().unwrap_or_default() } else { primary };
        let known = if mariadb { ["mysql_native_password", "mysql_old_password", "ed25519"].contains(&target_plugin.as_str()) }
            else { ["mysql_native_password", "caching_sha2_password", "sha256_password"].contains(&target_plugin.as_str()) };
        let protected = protected_account(username, host);
        let mut info = DatabaseUserPasswordInfo { username: username.into(), host: host.into(), plugins, target_plugin, supported: known && !protected,
            protected, other_authentication, revision: String::new() };
        // 进程随机密钥使返回 token 不能用于离线猜测密码；应用重启后旧表单自然失效。
        static SNAPSHOT_KEY: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
        let key = SNAPSHOT_KEY.get_or_init(rand::random);
        let mut digest = Sha256::new(); digest.update(key); digest.update(fingerprint.as_bytes()); digest.update(server.as_bytes());
        digest.update(serde_json::to_vec(&info).map_err(|error| AppError::internal("记录认证版本", error.to_string()))?);
        info.revision = hex::encode(digest.finalize());
        Ok(info)
    }

    fn set_user_password(&self, input: &DatabaseUserPasswordInput) -> Result<DatabaseUserPasswordInfo> {
        if input.password.is_empty() || input.password.len() > 4096 || input.password.chars().any(char::is_control) {
            return Err(AppError::new("BAD_PASSWORD", "密码须为 1–4096 字节，不能包含换行或控制字符"));
        }
        let before = self.user_password_info(&input.username, &input.host)?;
        if before.protected { return Err(AppError::new("SYSTEM_ACCOUNT", "系统账号受保护；root 密码请使用专用管理入口")); }
        if !before.supported { return Err(AppError::new("DB_PASSWORD_UNSUPPORTED", "此认证方式或组合暂不支持在此改密，请在对应认证系统或数据库客户端中管理")); }
        if before.revision != input.revision { return Err(AppError::new("DB_PASSWORD_CHANGED", "账号认证信息已变化，请重新读取后再保存")); }
        let escaped = input.password.replace('\'', "''");
        let account = format!("'{}'@'{}'", input.username.replace('\'', "''"), input.host.replace('\'', "''"));
        let mariadb = self.run("SELECT @@version;")?.to_ascii_lowercase().contains("mariadb");
        let mode = if mariadb { format!("SET SESSION old_passwords={};", if before.target_plugin == "mysql_old_password" { 1 } else { 0 }) } else { String::new() };
        let value = if mariadb { format!("PASSWORD('{escaped}')") } else { format!("'{escaped}'") };
        // SET PASSWORD 保留认证插件、其它认证方式及授权；不能用 IDENTIFIED BY 切到服务器默认插件。
        // 禁止当前管理会话记录通用日志，密码经临时 stdin 传递，不进入 argv、环境或本机设置。
        self.run(&format!("SET SESSION sql_log_off=ON; SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; {mode} SET PASSWORD FOR {account} = {value};"))
            .map_err(|mut error| {
                error.message = "未能确认账号密码已修改".into();
                error.hint = Some("密码策略可能拒绝此次修改；连接中断时修改也可能已经生效。请重新读取后确认，勿反复盲目重试。".into());
                if let Some(detail) = error.detail.as_mut() { *detail = detail.replace(&input.password, "***").replace(&escaped, "***"); }
                error
            })?;
        let after = self.user_password_info(&input.username, &input.host)
            .map_err(|error| error.with_hint("密码语句已执行，但未能读取最新认证状态；请确认当前密码和实例状态后再操作。"))?;
        if after.plugins != before.plugins || after.protected != before.protected {
            return Err(AppError::new("DB_PASSWORD_CHANGED", "密码操作后账号认证配置发生变化，请重新读取并确认当前登录方式"));
        }
        Ok(after)
    }

    fn partial_revokes(&self) -> Result<bool> {
        Ok(self.run("SHOW VARIABLES LIKE 'partial_revokes';")?.lines().any(|line| line.split('\t').nth(1).is_some_and(|value| value.eq_ignore_ascii_case("ON"))))
    }

    pub fn grants(&self, username: &str, host: &str) -> Result<DatabaseGrants> {
        use sha2::{Digest, Sha256};
        let filter = account_filter(username, host)?;
        if !self.list_users()?.iter().any(|account| account.username == username && account.host == host) {
            return Err(AppError::new("DB_USER_MISSING", "所选账号已不存在，请刷新账号列表"));
        }
        let columns = self.run("SHOW COLUMNS FROM mysql.db;")?.lines().filter_map(|line| line.split('\t').next())
            .filter(|column| column.ends_with("_priv") && column.bytes().all(|c| c.is_ascii_alphabetic() || c == b'_')).map(str::to_owned).collect::<Vec<_>>();
        if columns.is_empty() || !columns.iter().any(|name| name == "Grant_priv") { return Err(AppError::new("DATABASE_GRANTS_UNSUPPORTED", "此版本的授权表结构尚不支持编辑")); }
        let available = DATABASE_PRIVILEGES.iter().filter(|(column, _)| columns.iter().any(|name| name == column)).map(|(_, name)| name.to_string()).collect::<Vec<_>>();
        let partial_revokes = self.partial_revokes()?;
        let mariadb = self.run("SELECT @@version;")?.to_ascii_lowercase().contains("mariadb");
        let rows = self.run(&format!("SELECT HEX(Db),{} FROM mysql.db WHERE {filter} ORDER BY HEX(Db);", columns.join(",")))?;
        let mut scopes = Vec::new();
        for row in rows.lines().filter(|line| !line.is_empty()) {
            let fields = row.split('\t').collect::<Vec<_>>();
            if fields.len() != columns.len() + 1 || fields[1..].iter().any(|value| !["Y", "N"].contains(value)) { return Err(AppError::new("MYSQL_RESPONSE", "数据库授权响应不完整")); }
            let scope = decode_hex_field(fields[0])?;
            let (label, pattern, protected) = grant_scope_details(&scope, partial_revokes)?;
            let mut item = DatabaseGrantScope { scope, label, pattern, protected, privileges: Vec::new(), grant_option: false, extra_privileges: Vec::new() };
            for (column, value) in columns.iter().zip(&fields[1..]) {
                if *value != "Y" { continue; }
                if column == "Grant_priv" { item.grant_option = true; }
                else if let Some((_, privilege)) = DATABASE_PRIVILEGES.iter().find(|(name, _)| *name == column) { item.privileges.push(privilege.to_string()); }
                else { item.extra_privileges.push(column.clone()); }
            }
            scopes.push(item);
        }
        // 只读权限标志，不读取密码哈希、认证插件配置或 SHOW GRANTS 的 IDENTIFIED 内容。
        let global_columns = self.run("SHOW COLUMNS FROM mysql.user;")?.lines().filter_map(|line| line.split('\t').next())
            .filter(|column| column.ends_with("_priv") && column.bytes().all(|c| c.is_ascii_alphabetic() || c == b'_')).map(|column| format!("{column}='Y'")).collect::<Vec<_>>();
        let global_privileges = !global_columns.is_empty() && self.run(&format!("SELECT COUNT(*) FROM mysql.user WHERE {filter} AND ({});", global_columns.join(" OR ")))?.trim() != "0";
        let databases = self.list_databases()?.into_iter().filter(|db| !crate::dbbackup::is_system_db(&db.name)).map(|db| db.name).collect();
        let mut result = DatabaseGrants { username: username.into(), host: host.into(), scopes, databases, available, partial_revokes, mariadb, global_privileges, protected: protected_account(username, host), revision: String::new() };
        result.revision = hex::encode(Sha256::digest(serde_json::to_vec(&result).map_err(|error| AppError::internal("记录授权版本", error.to_string()))?));
        Ok(result)
    }

    fn apply_grants(&self, input: &DatabaseGrantInput) -> Result<DatabaseGrants> {
        let before = self.grants(&input.username, &input.host)?;
        if before.protected || (before.partial_revokes && before.global_privileges) {
            return Err(AppError::new("DATABASE_GRANTS_PROTECTED", "系统账号，或同时使用全局授权与部分撤销的账号，不允许在此修改"));
        }
        if before.revision != input.revision { return Err(AppError::new("DATABASE_GRANTS_CHANGED", "授权或数据库列表已变化，请重新读取后再保存")); }
        if input.privileges.len() > before.available.len() || input.privileges.iter().any(|name| !before.available.contains(name)) {
            return Err(AppError::new("BAD_PRIVILEGE", "请选择当前实例支持的数据库权限"));
        }
        let target = if input.new_database {
            if !before.databases.contains(&input.target) { return Err(AppError::new("NO_DATABASE", "所选业务数据库不存在")); }
            grant_literal(&input.target, before.partial_revokes)
        } else {
            if !before.scopes.iter().any(|scope| scope.scope == input.target) { return Err(AppError::new("DATABASE_GRANTS_CHANGED", "所选授权范围已不存在，请重新读取")); }
            input.target.clone()
        };
        let (_, _, protected) = grant_scope_details(&target, before.partial_revokes)?;
        if protected { return Err(AppError::new("SYSTEM_DATABASE", "不能在此修改覆盖系统数据库的授权")); }
        let current = before.scopes.iter().find(|scope| scope.scope == target);
        let previous = current.map(|scope| scope.privileges.as_slice()).unwrap_or_default();
        let mut wanted = input.privileges.clone(); wanted.sort(); wanted.dedup();
        let remove = previous.iter().filter(|name| !wanted.contains(name)).cloned().collect::<Vec<_>>();
        let add = wanted.iter().filter(|name| !previous.contains(name)).cloned().collect::<Vec<_>>();
        let account = format!("'{}'@'{}'", input.username.replace('\'', "''"), input.host.replace('\'', "''"));
        let target_sql = mysql_identifier(&target);
        // MariaDB 禁止 GRANT 隐式重建被外部删除的账号；MySQL 8 已移除此模式和隐式创建。
        let mode = if before.mariadb || self.run("SELECT @@version;")?.starts_with("5.") { "NO_BACKSLASH_ESCAPES,NO_AUTO_CREATE_USER" } else { "NO_BACKSLASH_ESCAPES" };
        let mut sql = format!("SET SESSION sql_mode='{mode}';");
        if current.is_some_and(|scope| scope.grant_option) && !input.grant_option { sql.push_str(&format!(" REVOKE GRANT OPTION ON {target_sql}.* FROM {account};")); }
        if !remove.is_empty() { sql.push_str(&format!(" REVOKE {} ON {target_sql}.* FROM {account};", remove.join(", "))); }
        if !add.is_empty() { sql.push_str(&format!(" GRANT {} ON {target_sql}.* TO {account};", add.join(", "))); }
        if input.grant_option && !current.is_some_and(|scope| scope.grant_option) { sql.push_str(&format!(" GRANT USAGE ON {target_sql}.* TO {account} WITH GRANT OPTION;")); }
        self.run(&sql).map_err(|error| error.with_hint("授权语句不能整体回滚，部分修改可能已生效。请重新读取当前授权后再保存；其它授权范围未主动修改"))?;
        let after = self.grants(&input.username, &input.host)?;
        let actual = after.scopes.iter().find(|scope| scope.scope == target);
        let mut privileges = actual.map(|scope| scope.privileges.clone()).unwrap_or_default(); privileges.sort();
        if after.partial_revokes != before.partial_revokes || privileges != wanted || actual.is_some_and(|scope| scope.grant_option) != input.grant_option {
            return Err(AppError::new("DATABASE_GRANTS_VERIFY", "保存后读取的权限与选择不一致，请重新读取当前授权"));
        }
        Ok(after)
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

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PostgresQueryRequest {
    pub database: String,
    pub sql: String,
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PostgresQueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
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
    pub connection_limit: i32,
    pub superuser: bool,
    pub create_db: bool,
    pub create_role: bool,
    pub replication: bool,
    pub bypass_rls: bool,
    pub protected: bool,
    pub databases: Vec<String>,
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PostgresRoleAccess {
    pub oid: u32,
    pub name: String,
    pub can_login: bool,
    pub connection_limit: i32,
    pub active_connections: u32,
    pub protected: bool,
    pub revision: String,
}

#[derive(serde::Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PostgresRoleAccessInput {
    pub oid: u32,
    pub name: String,
    pub can_login: bool,
    pub connection_limit: i32,
    pub revision: String,
    pub confirm_restriction: bool,
}

fn postgres_role_access_select(oid: u32) -> String {
    // 不读取 rolpassword；活动连接数是即时观察值，不参与编辑版本比较。
    format!("SELECT json_build_object('oid', r.oid::bigint, 'name', r.rolname, 'canLogin', r.rolcanlogin, 'connectionLimit', r.rolconnlimit,
        'activeConnections', (SELECT count(*) FROM pg_stat_activity a WHERE a.usesysid = r.oid AND a.backend_type = 'client backend'),
        'protected', (r.rolsuper OR r.rolname IN ('postgres', current_user, session_user) OR starts_with(r.rolname, 'pg_')),
        'revision', md5(json_build_array(r.oid, r.rolname, r.rolcanlogin, r.rolconnlimit, r.rolsuper, r.rolcreatedb, r.rolcreaterole, r.rolreplication, r.rolbypassrls, r.rolinherit, r.rolvaliduntil, r.rolconfig)::text))
        FROM pg_roles r WHERE r.oid = {oid}")
}

pub fn update_postgres_role_access(state: &crate::CoreState, version: &str, input: &PostgresRoleAccessInput) -> Result<PostgresRoleAccess> {
    let _work = crate::BackgroundWork::begin("保存 PostgreSQL 账号连接设置")?;
    state.with_postgres(version, |client| client.set_role_access(input))
}

pub(crate) fn postgres_ident(name: &str) -> Result<String> {
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
        let (private, mut command) = self.tool_command("psql", "postgres", None)?;
        command.args(["--no-psqlrc", "--no-align", "--tuples-only", "--set=ON_ERROR_STOP=1"])
            .env("PGOPTIONS", "-c statement_timeout=10000 -c lock_timeout=5000");
        Ok((private, command))
    }

    pub(crate) fn tool_command(&self, tool: &str, database: &str, options: Option<&str>) -> Result<(tempfile::TempDir, Command)> {
        postgres_ident(database)?;
        if !["psql", "pg_dump", "pg_restore"].contains(&tool) {
            return Err(AppError::new("POSTGRES_TOOL", "不支持的 PostgreSQL 客户端"));
        }
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
        let pass_database = database.replace('\\', "\\\\").replace(':', "\\:");
        writeln!(file, "127.0.0.1:{}:{pass_database}:postgres:{escaped}", self.port)?;
        file.sync_all()?;
        let exe = self.exe.with_file_name(crate::ops::exe_name(tool));
        let mut command = platform::command(&exe);
        for (name, _) in std::env::vars_os().filter(|(name, _)| name.to_string_lossy().to_ascii_uppercase().starts_with("PG")) { command.env_remove(name); }
        // 用 ASCII URI 传递 UTF-8 库名：既阻止 conninfo 注入，也避开 Windows 客户端 argv 的本地编码。
        let encode = |value: &str| value.as_bytes().iter().map(|byte| format!("%{byte:02X}")).collect::<String>();
        let database = encode(database);
        let options = options.map(|value| format!("?options={}", encode(value))).unwrap_or_default();
        command.args(["--no-password", "--host=127.0.0.1", "--username=postgres"])
            .arg(format!("--dbname=postgresql://postgres@127.0.0.1:{}/{database}{options}", self.port))
            .arg(format!("--port={}", self.port))
            .env("PGPASSFILE", crate::paths::portable_path_text(&passfile)).env("PGCONNECT_TIMEOUT", "5").env("PGCLIENTENCODING", "UTF8")
            .env("PGAPPNAME", "NiceEnv").env("LC_ALL", "C");
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

    pub(crate) fn query_database(&self, database: &str, sql: &str) -> Result<String> {
        postgres_ident(database)?;
        if sql.is_empty() || sql.len() > 1024 * 1024 || sql.contains('\0') {
            return Err(AppError::new("BAD_SQL", "SQL 为空、过长或包含无效字符"));
        }
        let (_private, mut command) = self.tool_command("psql", database, None)?;
        command.args([
            "--no-psqlrc",
            "--csv",
            "--set=ON_ERROR_STOP=1",
            "--pset=footer=off",
        ]).env("PGOPTIONS", "-c statement_timeout=20000 -c lock_timeout=5000");
        let mut input = tempfile::tempfile()?;
        input.write_all(sql.as_bytes())?;
        input.rewind()?;
        let mut output = tempfile::tempfile()?;
        let mut error = tempfile::tempfile()?;
        command.stdin(Stdio::from(input)).stdout(output.try_clone()?).stderr(error.try_clone()?);
        let status = wait_client(&mut command, Duration::from_secs(20), || {})?;
        if !status.success() {
            let mut detail = read_output(&mut error, 16 * 1024)?;
            if !self.password.is_empty() {
                detail = detail.replace(&self.password, "***");
            }
            return Err(AppError::new("POSTGRES_QUERY_FAILED", "PostgreSQL 查询失败，数据修改结果请以实例实际状态为准")
                .with_hint("请检查 SQL、当前数据库和账号权限；查询设置了 20 秒执行上限")
                .with_detail(detail));
        }
        if output.metadata()?.len() > 16 * 1024 * 1024 {
            return Err(AppError::new("POSTGRES_RESULT_TOO_LARGE", "查询结果超过 16 MB，请使用 LIMIT；已执行的写入不会因此撤销"));
        }
        read_output(&mut output, 16 * 1024 * 1024)
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
        let out = self.query("SELECT COALESCE(json_agg(row_to_json(r) ORDER BY r.name), '[]'::json) FROM (SELECT oid::bigint AS oid, rolname AS name, rolcanlogin AS \"canLogin\", rolconnlimit AS \"connectionLimit\", rolsuper AS superuser, rolcreatedb AS \"createDb\", rolcreaterole AS \"createRole\", rolreplication AS replication, rolbypassrls AS \"bypassRls\", (rolsuper OR rolname IN ('postgres', current_user, session_user) OR starts_with(rolname, 'pg_')) AS protected, ARRAY(SELECT datname FROM pg_database WHERE datdba = pg_roles.oid ORDER BY datname) AS databases FROM pg_roles WHERE NOT starts_with(rolname, 'pg_')) r;")?;
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

    pub fn role_access(&self, name: &str, oid: u32) -> Result<PostgresRoleAccess> {
        postgres_ident(name)?;
        let out = self.query(&format!("{};", postgres_role_access_select(oid)))?;
        if out.trim().is_empty() { return Err(AppError::new("POSTGRES_TARGET_CHANGED", "账号已删除或变化，请刷新列表")); }
        let access: PostgresRoleAccess = serde_json::from_str(out.trim())
            .map_err(|error| AppError::new("POSTGRES_RESPONSE", "无法读取账号连接设置").with_detail(error.to_string()))?;
        if access.name != name { return Err(AppError::new("POSTGRES_TARGET_CHANGED", "账号已更名，请刷新列表")); }
        Ok(access)
    }

    pub(crate) fn set_role_access(&self, input: &PostgresRoleAccessInput) -> Result<PostgresRoleAccess> {
        let identifier = postgres_ident(&input.name)?;
        if input.connection_limit < -1 {
            return Err(AppError::new("POSTGRES_BAD_LIMIT", "连接数上限须为 0–2147483647，或选择不限"));
        }
        if input.revision.len() != 32 || !input.revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(AppError::new("POSTGRES_ACCESS_CHANGED", "账号设置版本无效，请重新读取"));
        }
        let literal = |value: &str| format!("E'{}'", value.replace('\\', "\\\\").replace('\'', "''"));
        let query = postgres_role_access_select(input.oid);
        let name = literal(&input.name);
        let revision = literal(&input.revision);
        let can_login = input.can_login;
        let limit = input.connection_limit;
        let confirmed = input.confirm_restriction;
        let alter = literal(&format!("ALTER ROLE {identifier} {} CONNECTION LIMIT {limit}", if can_login { "LOGIN" } else { "NOLOGIN" }));
        let block = format!("DECLARE snapshot json; BEGIN
            snapshot := ({query});
            IF snapshot IS NULL OR snapshot->>'name' <> {name} THEN RAISE EXCEPTION 'NSB_ROLE_TARGET_CHANGED'; END IF;
            IF (snapshot->>'protected')::boolean THEN RAISE EXCEPTION 'NSB_ROLE_PROTECTED'; END IF;
            IF snapshot->>'revision' <> {revision} THEN RAISE EXCEPTION 'NSB_ROLE_ACCESS_CHANGED'; END IF;
            IF NOT {confirmed} AND (((snapshot->>'canLogin')::boolean AND NOT {can_login}) OR ({limit} >= 0 AND ((snapshot->>'connectionLimit')::int = -1 OR {limit} < (snapshot->>'connectionLimit')::int))) THEN RAISE EXCEPTION 'NSB_ROLE_CONFIRM_RESTRICTION'; END IF;
            EXECUTE {alter};
            snapshot := ({query});
            IF snapshot IS NULL OR snapshot->>'name' <> {name} OR (snapshot->>'canLogin')::boolean <> {can_login} OR (snapshot->>'connectionLimit')::int <> {limit} THEN RAISE EXCEPTION 'NSB_ROLE_VERIFY_FAILED'; END IF;
        END");
        // ALTER ROLE 使用 pg_authid 的 RowExclusiveLock。短事务先取得冲突表锁，使外部
        // ALTER/DROP/CREATE ROLE 也不能插入版本检查与写入之间；超时沿用客户端设置。
        let sql = format!("BEGIN; LOCK TABLE pg_catalog.pg_authid IN SHARE ROW EXCLUSIVE MODE; DO {}; {query}; COMMIT;", literal(&block));
        let out = self.query(&sql).map_err(|error| {
            let detail = error.detail.as_deref().unwrap_or("");
            for (marker, code, message) in [
                ("NSB_ROLE_TARGET_CHANGED", "POSTGRES_TARGET_CHANGED", "账号已删除或更名，请刷新列表"),
                ("NSB_ROLE_PROTECTED", "POSTGRES_PROTECTED", "系统账号或超级用户的连接设置受保护"),
                ("NSB_ROLE_ACCESS_CHANGED", "POSTGRES_ACCESS_CHANGED", "账号已被其他操作修改，请重新读取后再保存"),
                ("NSB_ROLE_CONFIRM_RESTRICTION", "POSTGRES_CONFIRM_RESTRICTION", "请先确认暂停登录或降低连接数上限的影响"),
            ] { if detail.contains(marker) { return AppError::new(code, message); } }
            AppError::new("POSTGRES_ACCESS_SAVE_FAILED", "未能确认账号连接设置已保存")
                .with_hint("请重新读取确认实际状态后再试；连接中断时不能仅凭报错判断事务是否已提交。")
                .with_detail(detail.to_string())
        })?;
        serde_json::from_str(out.lines().find(|line| line.starts_with('{')).unwrap_or(""))
            .map_err(|error| AppError::new("POSTGRES_RESPONSE", "设置已提交，但返回结果无法识别，请重新读取确认").with_detail(error.to_string()))
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

pub fn postgres_query(client: &PostgresClient, request: &PostgresQueryRequest) -> Result<PostgresQueryResult> {
    if !request.confirmed {
        return Err(AppError::new("SQL_CONFIRM_REQUIRED", "请确认目标数据库和完整 SQL 后执行"));
    }
    if !client.list_databases()?.iter().any(|database| database.name == request.database && database.allow_connections) {
        return Err(AppError::new("DATABASE_NOT_FOUND", "所选 PostgreSQL 数据库不存在或不允许连接"));
    }
    let output = client.query_database(&request.database, &request.sql)?;
    parse_postgres_csv(&output)
}

pub fn parse_postgres_csv(input: &str) -> Result<PostgresQueryResult> {
    let bytes = input.as_bytes();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row = Vec::new();
    let mut field: Vec<u8> = Vec::new();
    let mut quoted = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            match byte {
                b'"' if bytes.get(index + 1) == Some(&b'"') => {
                    field.push(b'"');
                    index += 2;
                }
                b'"' => {
                    quoted = false;
                    index += 1;
                }
                _ => {
                    field.push(byte);
                    index += 1;
                }
            }
            continue;
        }
        match byte {
            b'"' if field.is_empty() => {
                quoted = true;
                index += 1;
            }
            b',' => {
                row.push(String::from_utf8_lossy(&std::mem::take(&mut field)).into_owned());
                index += 1;
            }
            b'\n' => {
                row.push(String::from_utf8_lossy(&std::mem::take(&mut field)).into_owned());
                rows.push(std::mem::take(&mut row));
                index += 1;
            }
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => index += 1,
            _ => {
                field.push(byte);
                index += 1;
            }
        }
    }
    if quoted {
        return Err(AppError::new("POSTGRES_RESULT_INVALID", "PostgreSQL 返回的 CSV 结果不完整"));
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(String::from_utf8_lossy(&field).into_owned());
        rows.push(row);
    }
    if rows.is_empty() {
        return Ok(PostgresQueryResult { columns: Vec::new(), rows: Vec::new() });
    }
    let columns = rows.remove(0);
    if columns.len() > 256 {
        return Err(AppError::new("POSTGRES_RESULT_INVALID", "查询返回的字段数量过多"));
    }
    if rows.iter().any(|row| row.len() != columns.len()) {
        return Err(AppError::new("POSTGRES_RESULT_INVALID", "PostgreSQL 返回的列数不一致"));
    }
    if rows.len() > 20_000 {
        return Err(AppError::new("POSTGRES_RESULT_TOO_LARGE", "单个查询结果超过 20,000 行，请使用 LIMIT"));
    }
    Ok(PostgresQueryResult { columns, rows })
}

pub(crate) fn selected_postgres(state: &crate::CoreState, version: &str, password: Option<String>) -> Result<PostgresClient> {
    let package = state.store.find_installed("postgresql", Some(version)).ok_or_else(|| AppError::not_installed("PostgreSQL"))?;
    let service = state.manager.snapshot("postgresql").filter(|service| service.version.as_deref().is_some_and(|current| crate::install::same_version(current, version))
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
        assert!(grant_scope_details("information\\_schema", false).unwrap().2);
        assert!(grant_scope_details("information_schema", true).unwrap().2);
        assert_eq!(grant_scope_details("project\\_db", false).unwrap(), ("project_db".into(), false, false));
        assert_eq!(grant_scope_details("project\\_db", true).unwrap(), ("project\\_db".into(), false, false));
        assert!(grant_scope_details("%", false).unwrap().2);
        assert!(!grant_scope_details("project_db", true).unwrap().1);
        assert!(grant_scope_details("project_db", false).unwrap().1);
        assert_eq!(mysql_identifier("quote`name"), "`quote``name`");
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
