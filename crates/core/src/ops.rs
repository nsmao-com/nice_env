//! 服务启停编排：按服务类型组装 SpawnSpec、健康等待、优雅停止、站点重载。

use crate::configgen;
use crate::error::{AppError, Result};
use crate::model::ServiceState;
use crate::paths::{nginx_path, Paths};
use crate::services::*;
use crate::store::Store;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// 注册所有已安装服务（应用启动/安装后调用）。
/// 只有「守护进程型」套件注册为服务；node/python/go/composer 等纯运行时不注册。
/// 内置编排的服务（nginx/mysql 等）走这里；其余清单声明 `run` 的包走 `generic::register_services`。
pub fn register_services(paths: &Paths, store: &Store, manager: &Arc<ServiceManager>) {
    let _operation = manager.lifecycle.lock();
    let Ok(installed) = store.list_installed() else {
        return;
    };
    let ports = PortsProfile::from_settings(store);
    const SERVICE_IDS: &[&str] = &[
        "nginx",
        "apache",
        "php",
        "mysql",
        "postgresql",
        "mongodb",
        "redis",
        "mihomo",
    ];
    for p in installed {
        if !SERVICE_IDS.contains(&p.id.as_str()) {
            continue;
        }
        if p.id != "php"
            && p.id != "mysql"
            && installed_by_choice(store, &p.id).is_none_or(|active| active.version != p.version)
        {
            continue;
        }
        let service_id = if p.id == "php" || p.id == "mysql" {
            format!("{}@{}", p.id, p.version)
        } else {
            p.id.clone()
        };
        let label = match p.id.as_str() {
            "nginx" => "Nginx".to_string(),
            "apache" => "Apache".to_string(),
            "php" => format!("PHP {} (CGI 池)", p.version),
            "mysql" => format!("MySQL {}", p.version),
            "postgresql" => "PostgreSQL".to_string(),
            "mongodb" => "MongoDB".to_string(),
            "redis" => "Redis".to_string(),
            "mihomo" => "mihomo (Clash)".to_string(),
            other => other.to_string(),
        };
        let port = match p.id.as_str() {
            "nginx" => Some(ports.http),
            "apache" => Some(ports.apache_http),
            "mysql" => Some(ports.mysql),
            "postgresql" => Some(ports.postgres),
            "mongodb" => Some(ports.mongodb),
            "redis" => Some(ports.redis),
            "mihomo" => Some(configgen::MIHOMO_MIXED_PORT),
            "php" => store.get_port_assign(&service_id),
            _ => None,
        };
        manager.register(
            &service_id,
            &label,
            Some(p.version.clone()),
            Some(p.category.clone()),
            port,
            paths.service_log(&service_id),
        );
    }
}

fn php_exe(store: &Store, version: &str) -> Result<PathBuf> {
    let inst = store.find_installed("php", Some(version)).ok_or_else(|| {
        AppError::new("NOT_INSTALLED", format!("PHP {version} 尚未安装"))
            .with_hint("到「套件 / 服务」页安装该版本")
    })?;
    let exe = PathBuf::from(&inst.install_path).join(exe_name("php-cgi"));
    if !exe.exists() {
        return Err(AppError::new("BROKEN_INSTALL", "找不到 php-cgi"));
    }
    Ok(exe)
}

pub(crate) fn nginx_exe(store: &Store) -> Result<(PathBuf, PathBuf)> {
    let inst =
        installed_by_choice(store, "nginx").ok_or_else(|| AppError::not_installed("Nginx"))?;
    nginx_exe_for(&inst)
}

fn nginx_exe_for(inst: &crate::model::InstalledPackage) -> Result<(PathBuf, PathBuf)> {
    let root = PathBuf::from(&inst.install_path).join(format!("nginx-{}", inst.version));
    let exe_name = if cfg!(windows) { "nginx.exe" } else { "nginx" };
    let exe = root.join(exe_name);
    if !exe.exists() {
        return Err(
            AppError::new("BROKEN_INSTALL", format!("找不到 {exe_name}，套件可能损坏"))
                .with_hint("在套件页卸载后重新安装 Nginx"),
        );
    }
    Ok((root, exe))
}

/* ============ 单实例服务的「使用中版本」 ============ */

/// 安装路径锚定：优先用户在套件页选择的「使用中版本」，否则最新已装
pub fn installed_by_choice(store: &Store, id: &str) -> Option<crate::model::InstalledPackage> {
    if let Some(v) = store.get_setting(&format!("active{id}Version")) {
        if store.find_installed(id, Some(&v)).is_some() {
            return store.find_installed(id, Some(&v));
        }
    }
    let mut list = store
        .list_installed()
        .ok()?
        .into_iter()
        .filter(|p| p.id == id)
        .collect::<Vec<_>>();
    list.sort_by(|a, b| crate::versions::cmp_version_desc(&a.version, &b.version));
    list.into_iter().next()
}

/// 套件页「切换使用版本」：仅已装版本可切换
pub fn set_active_version(store: &Store, id: &str, version: &str) -> Result<()> {
    let inst = store.find_installed(id, Some(version)).ok_or_else(|| {
        AppError::not_installed(&format!("{id} {version}")).with_hint("先安装该版本再切换")
    })?;
    store.set_setting(&format!("active{id}Version"), &inst.version)?;
    Ok(())
}

/// 平台差异：可执行文件名（Windows 带 .exe）
pub fn exe_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// MySQL 发行包解压后的根目录名（winx64 / macos15-{arch}）
pub fn mysql_root_name(version: &str) -> String {
    if cfg!(windows) {
        format!("mysql-{version}-winx64")
    } else {
        let arch = match std::env::consts::ARCH {
            "aarch64" => "arm64",
            other => other,
        };
        format!("mysql-{version}-macos15-{arch}")
    }
}

pub fn mysql_paths(store: &Store, version: &str) -> Result<(PathBuf, PathBuf)> {
    let inst = store
        .find_installed("mysql", Some(version))
        .ok_or_else(|| AppError::not_installed("MySQL"))?;
    let basedir = PathBuf::from(&inst.install_path).join(mysql_root_name(version));
    let mysqld = basedir.join("bin").join(exe_name("mysqld"));
    if !mysqld.exists() {
        return Err(AppError::new("BROKEN_INSTALL", "找不到 mysqld"));
    }
    Ok((basedir, mysqld))
}

fn redis_paths(store: &Store) -> Result<(PathBuf, PathBuf)> {
    let inst =
        installed_by_choice(store, "redis").ok_or_else(|| AppError::not_installed("Redis"))?;
    let dir = PathBuf::from(&inst.install_path);
    let exe = dir.join(exe_name("redis-server"));
    Ok((dir, exe))
}

pub(crate) fn mihomo_paths(store: &Store) -> Result<(PathBuf, PathBuf)> {
    let inst = installed_by_choice(store, "mihomo")
        .ok_or_else(|| AppError::not_installed("mihomo 内核"))?;
    let dir = PathBuf::from(&inst.install_path);
    // 找任意 mihomo* 可执行（跨平台命名：mihomo-windows-amd64.exe / mihomo-darwin-arm64）
    if let Ok(rd) = std::fs::read_dir(&dir) {
        let mut first_any: Option<PathBuf> = None;
        for entry in rd.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("mihomo") {
                if !name.contains('.') {
                    // 无扩展名（unix 可执行）优先
                    return Ok((dir, entry.path()));
                }
                if first_any.is_none() {
                    first_any = Some(entry.path());
                }
            }
        }
        if let Some(p) = first_any {
            return Ok((dir, p));
        }
    }
    Err(AppError::new("BROKEN_INSTALL", "找不到 mihomo 可执行文件"))
}

pub(crate) fn apache_paths(store: &Store) -> Result<(PathBuf, PathBuf)> {
    let inst =
        installed_by_choice(store, "apache").ok_or_else(|| AppError::not_installed("Apache"))?;
    apache_paths_for(&inst)
}

fn apache_paths_for(inst: &crate::model::InstalledPackage) -> Result<(PathBuf, PathBuf)> {
    let root = PathBuf::from(&inst.install_path).join("Apache24");
    let exe = root.join("bin").join(exe_name("httpd"));
    if !exe.exists() {
        return Err(AppError::new("BROKEN_INSTALL", "找不到 httpd 可执行文件"));
    }
    Ok((root, exe))
}

fn postgres_paths(store: &Store) -> Result<(PathBuf, PathBuf)> {
    let inst = installed_by_choice(store, "postgresql")
        .ok_or_else(|| AppError::not_installed("PostgreSQL"))?;
    let root = PathBuf::from(&inst.install_path).join("pgsql");
    let exe = root.join("bin").join(exe_name("postgres"));
    if !exe.exists() {
        return Err(AppError::new(
            "BROKEN_INSTALL",
            "找不到 postgres 可执行文件",
        ));
    }
    Ok((root, exe))
}

fn mongodb_paths(store: &Store) -> Result<(PathBuf, PathBuf)> {
    let inst =
        installed_by_choice(store, "mongodb").ok_or_else(|| AppError::not_installed("MongoDB"))?;
    let dir = PathBuf::from(&inst.install_path);
    // 官方 zip 根目录带版本号，向下一层找 bin/mongod
    let direct = dir.join("bin").join(exe_name("mongod"));
    if direct.exists() {
        return Ok((dir, direct));
    }
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path().join("bin").join(exe_name("mongod"));
            if p.exists() {
                return Ok((entry.path(), p));
            }
        }
    }
    Err(AppError::new("BROKEN_INSTALL", "找不到 mongod 可执行文件"))
}

/* ================= 启动 ================= */

pub fn start_service(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    id: &str,
) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    register_services(paths, store, manager);
    crate::generic::register_services(paths, store, manager);
    let status = manager
        .snapshot(id)
        .ok_or_else(|| AppError::new("UNKNOWN_SERVICE", format!("服务 {id} 未注册或已卸载")))?;
    if matches!(status.state, ServiceState::Starting | ServiceState::Stopping)
        || (status.state == ServiceState::Error && manager.is_busy(id)) {
        return Err(AppError::new(
            "SERVICE_BUSY",
            format!("{id} 仍有进程运行，请先停止后重试"),
        ));
    }
    if let Some(e) = manager.snapshot(id) {
        if e.state == ServiceState::Running {
            return Ok(());
        }
    }
    if !id.contains('@') {
        if let Some(version) = status.version {
            set_active_version(store, id, &version)?;
        }
    }
    let ports = PortsProfile::from_settings(store);
    manager.set_state(id, ServiceState::Starting);

    let result = match id {
        "nginx" => start_nginx(store, paths, manager, &ports),
        "apache" => start_apache(store, paths, manager, &ports),
        "redis" => start_redis(store, paths, manager, &ports),
        "mihomo" => start_mihomo(store, paths, manager),
        "postgresql" => start_postgresql(store, paths, manager, &ports),
        "mongodb" => start_mongodb(store, paths, manager, &ports),
        s if s.starts_with("php@") => {
            start_php(store, paths, manager, s.trim_start_matches("php@"), &ports)
        }
        s if s.starts_with("mysql@") => start_mysql(
            store,
            paths,
            manager,
            s.trim_start_matches("mysql@"),
            &ports,
        ),
        // 清单声明 `run` 的包（Caddy/Meilisearch/MinIO/Mailpit …）走通用路径
        other => crate::generic::start(store, paths, manager, other, &ports),
    };

    match result {
        Ok(()) => {
            manager.set_state(id, ServiceState::Running);
            let effective_ports = PortsProfile::from_settings(store);
            // 记录本次实际绑定的端口，停机命令据此寻址（端口方案可能在运行期被改）
            let actual_port = match id {
                "nginx" => Some(ports.http),
                "apache" => Some(ports.apache_http),
                "redis" => Some(effective_ports.redis),
                "postgresql" => Some(ports.postgres),
                "mongodb" => Some(ports.mongodb),
                "mihomo" => Some(configgen::MIHOMO_MIXED_PORT),
                s if s.starts_with("mysql@") => Some(effective_ports.mysql),
                s if s.starts_with("php@") => store.get_port_assign(s),
                _ => None,
            };
            if let Some(p) = actual_port {
                manager.set_started_port(id, p);
            }
            if let Some(e) = manager.services.lock().get(id).cloned() {
                *e.started_at.lock() = Some(std::time::SystemTime::now());
            }
            Ok(())
        }
        Err(mut err) => {
            if let Err(cleanup) = terminate_group(manager, id) {
                err.detail = Some(format!("{}\n启动失败后的进程清理未完成：{}",
                    err.detail.as_deref().unwrap_or_default(), cleanup.message));
            }
            manager.set_error(id, err.clone());
            Err(err)
        }
    }
}

fn start_nginx(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    ports: &PortsProfile,
) -> Result<()> {
    let (root, exe) = nginx_exe(store)?;
    precheck_port(ports.http, "Nginx")?;
    // 默认 server 块无条件 listen https（见 configgen::render_nginx_conf），
    // 端口被占则 nginx 直接 bind 失败，必须一并预检
    if ports.https != ports.http {
        precheck_port(ports.https, "Nginx (HTTPS)")?;
    }

    // 主配置只包含运行中的 PHP 池；仅安装但未启动的版本不能生成指向空端口的 upstream。
    let pools = running_php_pools(store, manager);
    configgen::write_nginx_conf(paths, &root, &pools, ports.http, ports.https)?;
    configgen::validate_nginx(&exe, &paths.nginx_conf())?;

    let site_endpoints = crate::sites::snapshot_endpoints(paths, store, "nginx");
    let spec = SpawnSpec {
        program: exe.clone(),
        args: vec![
            "-p".into(),
            root.to_string_lossy().to_string(),
            "-c".into(),
            paths.nginx_conf().to_string_lossy().to_string(),
        ],
        cwd: Some(root.clone()),
        env: vec![],
        detached: None,
    };
    let pid = spawn_tracked(manager, "nginx", &spec)?;
    let _ = pid;
    if !wait_healthy(ports.http, Duration::from_secs(10)) {
        return Err(
            AppError::new("NGINX_START_TIMEOUT", "Nginx 启动超时（端口未就绪）")
                .with_hint("查看日志页 nginx 的最后输出；常见原因是配置错误或端口冲突"),
        );
    }
    crate::sites::record_endpoints(manager, "nginx", site_endpoints);
    Ok(())
}

fn start_php(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    version: &str,
    ports: &PortsProfile,
) -> Result<()> {
    let service_id = format!("php@{version}");
    let exe = php_exe(store, version)?;
    configgen::write_php_ini(paths, version)?;
    let base = allocate_php_pool(store, &service_id)?;

    // 清理旧 pid 记录
    if let Some(e) = manager.services.lock().get(&service_id).cloned() {
        e.pids.lock().clear();
        *e.group.lock() = Some(platform::ProcessGroup::new().map_err(AppError::from)?);
    }

    // 先一次性预检整个池，避免第 3 个 worker 才失败时留下 2 个孤儿进程
    for i in 0..configgen::PHP_POOL_WORKERS {
        precheck_port(base + i, &format!("PHP {version} worker"))?;
    }

    let ini_dir = paths.etc_dir("php", version);
    for i in 0..configgen::PHP_POOL_WORKERS {
        let port = base + i;
        let spec = SpawnSpec {
            program: exe.clone(),
            args: vec!["-b".into(), format!("127.0.0.1:{port}")],
            cwd: Some(exe.parent().map(PathBuf::from).unwrap_or_default()),
            env: vec![
                ("PHPRC".into(), ini_dir.to_string_lossy().to_string()),
                ("PHP_INI_SCAN_DIR".into(), "".into()),
                ("PHP_FCGI_MAX_REQUESTS".into(), "1000".into()),
            ],
            detached: None,
        };
        spawn_tracked(manager, &service_id, &spec)?;
    }
    if !wait_pids_alive(manager, &service_id, Duration::from_secs(8)) {
        return Err(AppError::new(
            "PHP_START_FAILED",
            format!("PHP {version} 进程启动失败").clone(),
        )
        .with_hint("查看日志；常见原因是 php.ini 扩展加载失败或缺少 VC 运行库"));
    }
    // PHP 池启动后重建并加载正在运行的 Web 服务配置，确保新版本立即可被站点使用。
    // 不能只 reload 旧配置：旧配置里还没有刚分配的 PHP upstream。
    let mut pools = running_php_pools(store, manager);
    if !pools.iter().any(|(ver, _)| ver == version) {
        pools.push((version.to_string(), base));
    }
    if manager
        .snapshot("nginx")
        .map(|s| s.state == ServiceState::Running)
        .unwrap_or(false)
    {
        let (root, exe) = nginx_exe(store)?;
        configgen::write_nginx_conf(paths, &root, &pools, ports.http, ports.https)?;
        configgen::validate_nginx(&exe, &paths.nginx_conf())?;
        reload_nginx(store, paths, manager).map_err(|error| {
            AppError::new(
                "PHP_NGINX_RELOAD_FAILED",
                format!("PHP {version} 已启动，但 Nginx 未能加载新的 PHP 池配置"),
            )
            .with_hint("本次 PHP 启动已回滚；修复 Nginx 配置后重试")
            .with_detail(error.to_string())
        })?;
    }
    if manager
        .snapshot("apache")
        .map(|s| s.state == ServiceState::Running)
        .unwrap_or(false)
    {
        let (root, exe) = apache_paths(store)?;
        configgen::write_httpd_conf(paths, &root, &pools, ports.apache_http, ports.apache_https)?;
        configgen::validate_httpd(&exe, &paths.apache_conf())?;
        reload_apache(&root, &exe, paths, store, manager, manager.started_port_or("apache", ports.apache_http))
            .map_err(|error| {
                AppError::new(
                    "PHP_APACHE_RELOAD_FAILED",
                    format!("PHP {version} 已启动，但 Apache 未能加载新的 PHP 池配置"),
                )
                .with_hint("本次 PHP 启动已回滚；修复 Apache 配置后重试")
                .with_detail(error.to_string())
            })?;
    }
    Ok(())
}

fn start_mysql(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    version: &str,
    ports: &PortsProfile,
) -> Result<()> {
    let service_id = format!("mysql@{version}");
    let (basedir, mysqld) = mysql_paths(store, version)?;
    // 自动回落只更新托管端口；用户的缓冲池、连接数、SQL 模式等设置保留。
    let mysql_port =
        crate::services::fallback_port_for(store, "mysql", ports.mysql, &[]).unwrap_or(ports.mysql);
    // 仅同步 basedir/datadir/port，配置 include 中的同名项由启动参数最终约束。
    configgen::write_mysql_ini(paths, version, &basedir, mysql_port)?;
    let datadir = crate::paths::checked_data_path(&paths.base, &format!("data/mysql/{version}"))?;
    let needs_init = match std::fs::read_dir(&datadir) {
        Ok(mut entries) => match entries.next() {
            None => true,
            Some(entry) => {
                entry?;
                false
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    if needs_init {
        let parent = datadir
            .parent()
            .ok_or_else(|| AppError::new("MYSQL_INIT_FAILED", "数据目录无效"))?;
        std::fs::create_dir_all(parent)?;
        let pending = tempfile::Builder::new()
            .prefix(".mysql-init-")
            .tempdir_in(parent)?;
        let mut init_args = mysql_launch_args(paths, version, &basedir, mysql_port);
        for arg in &mut init_args {
            if arg.starts_with("--datadir=") {
                *arg = format!("--datadir={}", pending.path().display());
            }
        }
        init_args.push("--initialize-insecure".to_string());
        if cfg!(windows) {
            init_args.push("--console".to_string());
        }
        let mut output = tempfile::tempfile()?;
        let mut command = platform::command(&mysqld);
        command
            .args(&init_args)
            .stdin(std::process::Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output.try_clone()?);
        let status = crate::dbadmin::wait_client(&mut command, Duration::from_secs(180), || {})?;
        if !status.success() {
            return Err(
                AppError::new("MYSQL_INIT_FAILED", "MySQL 数据目录初始化失败")
                    .with_hint("检查磁盘空间和 MySQL 配置；现有数据目录未覆盖")
                    .with_detail(crate::dbadmin::read_output(&mut output, 64 * 1024)?),
            );
        }
        // 只删除空目录；用户在初始化期间写入的文件不得被清理。
        if datadir.exists() {
            std::fs::remove_dir(&datadir)?;
        }
        std::fs::rename(pending.path(), &datadir)?;
    }

    precheck_port(mysql_port, "MySQL")?;
    let mut mysql_args = mysql_launch_args(paths, version, &basedir, mysql_port);
    if cfg!(windows) {
        mysql_args.push("--console".to_string());
    }
    let spec = SpawnSpec {
        program: mysqld.clone(),
        args: mysql_args,
        cwd: Some(basedir.clone()),
        env: vec![],
        detached: None,
    };
    spawn_tracked(manager, &service_id, &spec)?;
    let ready_timeout = if needs_init { 60 } else { 30 };
    if !wait_healthy(mysql_port, Duration::from_secs(ready_timeout)) {
        return Err(AppError::new(
            "MYSQL_START_TIMEOUT",
            format!("MySQL 启动超时（{ready_timeout}s 内端口未就绪）"),
        )
        .with_hint("请检查磁盘空间、配置与错误日志后重试")
        .with_detail(manager.tail(&service_id, 40).join("\n")));
    }

    manager.set_started_port(&service_id, mysql_port);
    store.set_setting(&crate::dbadmin::port_key(version), &mysql_port.to_string())?;
    // 每个数据目录独立认证。兼容旧全局密码和过去部分设密失败留下的 root/空密码。
    let mut candidates = Vec::new();
    if let Some(value) = crate::dbadmin::saved_password(store, version) {
        candidates.push(value);
    }
    candidates.push("root".to_string());
    candidates.push(String::new());
    candidates.dedup();
    let mut authenticated = false;
    let mut auth_error = None;
    let auth_started = std::time::Instant::now();
    loop {
        for password in &candidates {
            let client = crate::dbadmin::MySqlClient {
                exe: basedir.join("bin").join(exe_name("mysql")),
                port: mysql_port,
                root_password: password.clone(),
            };
            if let Err(error) = client.verify_data_dir(&datadir) {
                auth_error = Some(error);
                continue;
            }
            if password.is_empty() {
                use rand::Rng;
                let secure: String = rand::thread_rng()
                    .sample_iter(&rand::distributions::Alphanumeric)
                    .take(24)
                    .map(char::from)
                    .collect();
                // 先落地凭据；异常退出后再次启动可凭此恢复，不能生成后丢失。
                store.set_setting(&crate::dbadmin::password_key(version), &secure)?;
                client.reset_root_password(&secure)?;
                crate::dbadmin::MySqlClient {
                    root_password: secure,
                    ..client
                }
                .verify_data_dir(&datadir)?;
            } else {
                store.set_setting(&crate::dbadmin::password_key(version), &password)?;
            }
            authenticated = true;
            break;
        }
        if authenticated || !needs_init || auth_started.elapsed() >= Duration::from_secs(10) {
            break;
        }
        if auth_error
            .as_ref()
            .is_some_and(|error| error.code == "MYSQL_INSTANCE_MISMATCH")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    if !authenticated {
        if needs_init {
            return Err(auth_error.unwrap_or_else(|| {
                AppError::new("MYSQL_AUTH_FAILED", "新实例初始化后无法认证，已停止服务")
            }));
        }
        // 服务已经真实就绪；保留运行，以便用户在数据库页验证并更新本机连接凭据。
        manager.push_log(
            &service_id,
            "MySQL 已启动，但保存的 root 凭据无法认证；请在数据库页更新连接密码。",
        );
    }

    Ok(())
}

fn mysql_launch_args(paths: &Paths, version: &str, basedir: &Path, port: u16) -> Vec<String> {
    // --defaults-file 必须位于其他参数之前，后续参数覆盖 include 中过时的托管路径/端口。
    vec![
        format!(
            "--defaults-file={}",
            paths.mysql_ini(version).to_string_lossy()
        ),
        format!("--basedir={}", basedir.to_string_lossy()),
        format!(
            "--datadir={}",
            paths.mysql_data_dir(version).to_string_lossy()
        ),
        format!("--port={port}"),
    ]
}

fn start_redis(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    ports: &PortsProfile,
) -> Result<()> {
    let (_, exe) = redis_paths(store)?;
    let version = installed_by_choice(store, "redis")
        .map(|p| p.version)
        .unwrap_or_default();
    // 端口被占 + 自动回落开启 → 换附近空闲端口（写入覆盖项，重启稳定）
    let redis_port =
        crate::services::fallback_port_for(store, "redis", ports.redis, &[]).unwrap_or(ports.redis);
    configgen::write_redis_conf(paths, &version, redis_port)?;
    precheck_port(redis_port, "Redis")?;
    let spec = SpawnSpec {
        program: exe.clone(),
        args: redis_launch_args(paths, &version, redis_port),
        cwd: Some(exe.parent().map(PathBuf::from).unwrap_or_default()),
        env: vec![],
        detached: None,
    };
    spawn_tracked(manager, "redis", &spec)?;
    if !wait_healthy(redis_port, Duration::from_secs(10)) {
        return Err(AppError::new("REDIS_START_TIMEOUT", "Redis 启动超时")
            .with_hint("查看日志页 redis 输出，检查端口和配置")
            .with_detail(manager.tail("redis", 30).join("\n")));
    }
    Ok(())
}

fn start_mihomo(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>) -> Result<()> {
    let (_, exe) = mihomo_paths(store)?;
    let previous = match std::fs::read_to_string(paths.mihomo_config()) {
        Ok(value) => Some(value),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(AppError::io("读取代理配置", e)),
    };
    let raw = previous.clone().unwrap_or_else(configgen::render_mihomo_builtin_config);
    let config = configgen::adapt_mihomo_profile(&raw, &crate::proxy::configured_mode(store)?)?;
    crate::proxy::validate_profile(paths, store, &config)?;
    crate::cfgeditor::write_generated_config(paths, "mihomo-config", &paths.mihomo_config(), &config, previous.as_deref())?;
    precheck_port(configgen::MIHOMO_MIXED_PORT, "mihomo 混合端口")?;
    // 控制端口也由 mihomo 绑定；被占时内核起不来，且健康检查无法区分「自己起来了」
    // 和「别的进程恰好占着 19090」
    precheck_port(configgen::MIHOMO_CONTROLLER_PORT, "mihomo 控制端口")?;
    let spec = SpawnSpec {
        program: exe.clone(),
        args: vec![
            "-d".into(),
            paths.mihomo_dir().to_string_lossy().to_string(),
            "-f".into(),
            paths.mihomo_config().to_string_lossy().to_string(),
        ],
        cwd: Some(paths.mihomo_dir()),
        env: vec![],
        detached: None,
    };
    spawn_tracked(manager, "mihomo", &spec)?;
    if !wait_healthy(configgen::MIHOMO_CONTROLLER_PORT, Duration::from_secs(10))
        || !wait_healthy(configgen::MIHOMO_MIXED_PORT, Duration::from_secs(3))
        || !manager.snapshot("mihomo").is_some_and(|s| !s.pids.is_empty()) {
        return Err(AppError::new("MIHOMO_START_TIMEOUT", "mihomo 内核启动超时")
            .with_hint("查看日志页 mihomo 输出；配置损坏时请重新更新或切换订阅"));
    }
    crate::proxy::ProxyRuntime::new().version()?;
    Ok(())
}

fn redis_launch_args(paths: &Paths, version: &str, port: u16) -> Vec<String> {
    vec![
        paths.redis_conf(version).to_string_lossy().to_string(),
        "--port".into(),
        port.to_string(),
        "--dir".into(),
        paths.redis_data_dir().to_string_lossy().to_string(),
        "--daemonize".into(),
        "no".into(),
    ]
}

fn start_apache(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    ports: &PortsProfile,
) -> Result<()> {
    let (root, exe) = apache_paths(store)?;
    precheck_port(ports.apache_http, "Apache")?;
    // httpd.conf 里 Listen ... https 是无条件写出的，端口冲突会导致启动失败
    if ports.apache_https != ports.apache_http {
        precheck_port(ports.apache_https, "Apache (HTTPS)")?;
    }
    // 重建主配置（包含运行中的 php 池 balancer）
    let pools = running_php_pools(store, manager);
    configgen::write_httpd_conf(paths, &root, &pools, ports.apache_http, ports.apache_https)?;
    configgen::validate_httpd(&exe, &paths.apache_conf())?;

    let site_endpoints = crate::sites::snapshot_endpoints(paths, store, "apache");
    let spec = SpawnSpec {
        program: exe.clone(),
        args: vec![
            "-d".into(),
            root.to_string_lossy().to_string(),
            "-f".into(),
            paths.apache_conf().to_string_lossy().to_string(),
        ],
        cwd: Some(root.clone()),
        env: vec![],
        detached: None,
    };
    spawn_tracked(manager, "apache", &spec)?;
    if !wait_healthy(ports.apache_http, Duration::from_secs(12)) {
        return Err(
            AppError::new("APACHE_START_TIMEOUT", "Apache 启动超时（端口未就绪）")
                .with_hint("查看日志页 apache 输出；常见原因是端口冲突或缺少 VC 运行库"),
        );
    }
    crate::sites::record_endpoints(manager, "apache", site_endpoints);
    Ok(())
}

fn start_postgresql(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    ports: &PortsProfile,
) -> Result<()> {
    let (root, _exe) = postgres_paths(store)?;
    let version = installed_by_choice(store, "postgresql")
        .map(|p| p.version)
        .unwrap_or_default();
    let datadir = paths.postgres_data_dir(&version);
    let initdb = root.join("bin").join(exe_name("initdb"));
    let needs_init = !datadir.exists()
        || std::fs::read_dir(&datadir)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true);
    if needs_init {
        std::fs::create_dir_all(&datadir)?;
        // macOS 的 initdb 拒绝以 root 用户运行，且 -U 指定的是数据库超级用户；
        // 两端统一用 postgres（Windows 上 pg_ctl 也不认 root 以外的惯例名）
        let out = platform::command(&initdb)
            .args([
                "-D".to_string(),
                datadir.to_string_lossy().to_string(),
                "-U".into(),
                "postgres".into(),
                "-A".into(),
                "trust".into(),
                "-E".into(),
                "UTF8".into(),
                "--no-locale".into(),
            ])
            .output()
            .map_err(|e| AppError::io("初始化 PostgreSQL 数据目录", e))?;
        if !out.status.success() {
            let _ = std::fs::remove_dir_all(&datadir);
            return Err(
                AppError::new("PG_INIT_FAILED", "PostgreSQL 数据目录初始化失败")
                    .with_detail(String::from_utf8_lossy(&out.stderr).to_string()),
            );
        }
    }

    precheck_port(ports.postgres, "PostgreSQL")?;
    let postgres = root.join("bin").join(exe_name("postgres"));
    #[allow(unused_mut)]
    let mut pg_args: Vec<String> = vec![
        "-D".into(),
        datadir.to_string_lossy().to_string(),
        "-p".into(),
        ports.postgres.to_string(),
        "-c".into(),
        "listen_addresses=127.0.0.1".into(),
    ];
    // unix socket 目录只在类 Unix 有意义；Windows 仅 TCP
    #[cfg(not(windows))]
    pg_args.push(format!(
        "unix_socket_directories={}",
        paths.apache_run_dir().to_string_lossy()
    ));
    let spec = SpawnSpec {
        program: postgres.clone(),
        args: pg_args,
        cwd: Some(root.clone()),
        env: vec![],
        detached: None,
    };
    spawn_tracked(manager, "postgresql", &spec)?;
    if !wait_healthy(ports.postgres, Duration::from_secs(20)) {
        return Err(AppError::new(
            "PG_START_TIMEOUT",
            "PostgreSQL 启动超时（20s 内端口未就绪）",
        )
        .with_hint("查看日志页 postgresql 输出；首次初始化可能较慢"));
    }
    Ok(())
}

fn start_mongodb(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    ports: &PortsProfile,
) -> Result<()> {
    let (dir, exe) = mongodb_paths(store)?;
    let version = installed_by_choice(store, "mongodb")
        .map(|p| p.version)
        .unwrap_or_default();
    let dbpath = paths.mongo_data_dir(&version);
    std::fs::create_dir_all(&dbpath)?;
    let logfile = paths.service_log("mongodb");
    if let Some(parent) = logfile.parent() {
        std::fs::create_dir_all(parent)?;
    }
    precheck_port(ports.mongodb, "MongoDB")?;
    let spec = SpawnSpec {
        program: exe.clone(),
        args: vec![
            "--dbpath".into(),
            dbpath.to_string_lossy().to_string(),
            "--port".into(),
            ports.mongodb.to_string(),
            "--bind_ip".into(),
            "127.0.0.1".into(),
            "--logpath".into(),
            logfile.to_string_lossy().to_string(),
            "--logappend".into(),
        ],
        cwd: Some(dir.clone()),
        env: vec![],
        detached: None,
    };
    spawn_tracked(manager, "mongodb", &spec)?;
    if !wait_healthy(ports.mongodb, Duration::from_secs(15)) {
        return Err(AppError::new("MONGO_START_TIMEOUT", "MongoDB 启动超时")
            .with_hint("查看日志页 mongodb 输出；通常是端口冲突或数据目录损坏"));
    }
    Ok(())
}

/// 当前处于运行态的 php 池（供 apache balancer / nginx upstream 使用）
fn running_php_pools(store: &Store, manager: &Arc<ServiceManager>) -> Vec<(String, u16)> {
    let mut pools: Vec<(String, u16)> = store
        .all_port_assigns()
        .into_iter()
        .filter(|(sid, _)| sid.starts_with("php@"))
        .map(|(sid, base)| (sid.trim_start_matches("php@").to_string(), base))
        .collect();
    pools.retain(|(ver, _)| {
        manager
            .snapshot(&format!("php@{ver}"))
            .map(|s| s.state == ServiceState::Running)
            .unwrap_or(false)
    });
    pools
}

/* ================= 停止 ================= */

pub fn stop_service(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    id: &str,
) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    if id == "coredns" { crate::dns::restore_before_stop(store)?; }
    if manager.snapshot(id).is_none() {
        return Ok(());
    }
    if !manager.is_busy(id) {
        // Error/Unknown 但已无进程时也属于已停止，不再发送无目标的停机命令。
        manager.set_state(id, ServiceState::Stopped);
        return Ok(());
    }
    manager.set_state(id, ServiceState::Stopping);
    let ports = PortsProfile::from_settings(store);
    // 停机命令要打向「启动时用的端口」：用户可能在运行期切换了端口方案，
    // 用当前设置去 shutdown 会打到错误的端口，失败后只能强杀（MySQL 会脏关）
    let mysql_port = manager.started_port_or(id, ports.mysql);
    let redis_port = manager.started_port_or(id, ports.redis);
    let result = (|| -> Result<()> {
        match id {
            "nginx" => {
                if let Ok((root, exe)) = nginx_exe(store) {
                    let _ = platform::command(&exe)
                        .args([
                            "-p".into(),
                            root.to_string_lossy().to_string(),
                            "-c".into(),
                            paths.nginx_conf().to_string_lossy().to_string(),
                            "-s".into(),
                            "stop".into(),
                        ])
                        .output();
                    std::thread::sleep(Duration::from_millis(800));
                }
                terminate_group(manager, id)?;
                Ok(())
            }
            "apache" => {
                if let Ok((root, exe)) = apache_paths(store) {
                    let _ = platform::command(&exe)
                        .args([
                            "-d".into(),
                            root.to_string_lossy().to_string(),
                            "-f".into(),
                            paths.apache_conf().to_string_lossy().to_string(),
                            "-k".into(),
                            "stop".into(),
                        ])
                        .output();
                    std::thread::sleep(Duration::from_millis(800));
                }
                terminate_group(manager, id)?;
                Ok(())
            }
            "postgresql" => {
                if let Ok((root, _)) = postgres_paths(store) {
                    let pg_ctl = root.join("bin").join(exe_name("pg_ctl"));
                    let version = installed_by_choice(store, "postgresql")
                        .map(|p| p.version)
                        .unwrap_or_default();
                    let _ = platform::command(&pg_ctl)
                        .args([
                            "-D".into(),
                            paths
                                .postgres_data_dir(&version)
                                .to_string_lossy()
                                .to_string(),
                            "-m".into(),
                            "fast".into(),
                            "stop".into(),
                        ])
                        .output();
                    for _ in 0..16 {
                        if !tcp_port_open(ports.postgres) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
                terminate_group(manager, id)?;
                Ok(())
            }
            "mongodb" => {
                // mongod 对 SIGTERM/强杀均靠 journaling 恢复，直接终止组
                terminate_group(manager, id)?;
                Ok(())
            }
            "redis" => {
                if let Some(service) = manager.snapshot(id) {
                    let version = service.version.as_deref().ok_or_else(|| AppError::new("REDIS_VERSION_UNKNOWN", "无法确认 Redis 版本，未发送停机命令"))?;
                    let credentials = crate::stats::RedisCredentials::load(store, version)?;
                    crate::stats::redis_shutdown(redis_port, &credentials, &service.pids)?;
                    for _ in 0..100 {
                        if service.pids.iter().all(|pid| !platform::process_alive(*pid)) { break; }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    if service.pids.iter().any(|pid| platform::process_alive(*pid)) {
                        return Err(AppError::new("REDIS_SHUTDOWN_TIMEOUT", "Redis 仍在运行，未强制结束进程，请检查保存进度和日志"));
                    }
                }
                terminate_group(manager, id)?;
                Ok(())
            }
            s if s.starts_with("mysql@") => {
                let version = s.trim_start_matches("mysql@");
                if let Ok((basedir, _)) = mysql_paths(store, version) {
                    let admin = basedir.join("bin").join(exe_name("mysqladmin"));
                    let pass = crate::dbadmin::saved_password(store, version).unwrap_or_default();
                    if let Ok((_private, mut command)) = crate::dbadmin::client_command(&admin, "127.0.0.1", mysql_port, "root", &pass) {
                        command.args(["--connect-timeout=5", "shutdown"]).stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
                        let _ = crate::dbadmin::wait_client(&mut command, Duration::from_secs(10), || {});
                    }
                    // 优雅关闭最多等 10s
                    for _ in 0..20 {
                        if !tcp_port_open(mysql_port) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
                terminate_group(manager, id)?;
                Ok(())
            }
            _ => {
                // 清单驱动的通用服务：先试清单声明的优雅停止命令，再终止进程组
                crate::generic::graceful_stop(store, paths, id);
                terminate_group(manager, id)?;
                Ok(())
            }
        }
    })();

    // 清 pid/状态：先确认进程真的没了，否则保留 pid 让下次 stop 能重试
    let mut survivors: Vec<u32> = Vec::new();
    if let Some(e) = manager.services.lock().get(id).cloned() {
        let pids = e.pids.lock().clone();
        for _ in 0..10 {
            survivors = pids
                .iter()
                .copied()
                .filter(|p| platform::process_alive(*p))
                .collect();
            if survivors.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if survivors.is_empty() {
            e.pids.lock().clear();
            *e.started_at.lock() = None;
            *e.started_port.lock() = None;
            *e.group.lock() = None;
        }
    }
    if survivors.is_empty() {
        manager.set_state(id, ServiceState::Stopped);
    } else {
        // 进程仍在：不要谎报 Stopped，否则后续 stop 会被短路掉再也杀不掉
        if let Err(error) = &result {
            manager.set_error(id, error.clone());
            return Err(error.clone());
        }
        let err = AppError::new(
            "STOP_FAILED",
            format!("{id} 仍有 {} 个进程未退出", survivors.len()),
        )
        .with_hint("尝试以管理员身份重开应用后再停止；或用「工具箱 → 端口扫描」结束占用进程")
        .with_detail(format!("残留 pid: {survivors:?}"));
        manager.set_error(id, err.clone());
        return Err(err);
    }
    result
}

fn terminate_group(manager: &Arc<ServiceManager>, id: &str) -> Result<()> {
    if let Some(e) = manager.services.lock().get(id).cloned() {
        let mut group = e.group.lock();
        if let Some(g) = group.as_mut() {
            g.terminate(true)?;
        }
        *group = None;
        let alive: Vec<_> = e
            .pids
            .lock()
            .iter()
            .copied()
            .filter(|pid| platform::process_alive(*pid))
            .collect();
        if !alive.is_empty() {
            platform::ProcessGroup::from_pids(alive).terminate(true)?;
        }
    }
    Ok(())
}

/* ================= nginx 重载 ================= */

pub fn reload_nginx(store: &Store, paths: &Paths, manager: &ServiceManager) -> Result<()> {
    let (root, exe) = nginx_exe(store)?;
    configgen::validate_nginx(&exe, &paths.nginx_conf())?;
    let site_endpoints = crate::sites::snapshot_endpoints(paths, store, "nginx");
    let out = platform::command(&exe)
        .args([
            "-p".into(),
            root.to_string_lossy().to_string(),
            "-c".into(),
            paths.nginx_conf().to_string_lossy().to_string(),
            "-s".into(),
            "reload".into(),
        ])
        .output()
        .map_err(|e| AppError::io("重载 nginx", e))?;
    if !out.status.success() {
        return Err(AppError::new("NGINX_RELOAD_FAILED", "nginx 重载失败")
            .with_detail(String::from_utf8_lossy(&out.stderr).to_string()));
    }
    crate::sites::retain_reloaded_endpoints(manager, "nginx", site_endpoints);
    Ok(())
}

fn reload_apache(root: &Path, exe: &Path, paths: &Paths, store: &Store, manager: &ServiceManager, port: u16) -> Result<()> {
    let site_endpoints = crate::sites::snapshot_endpoints(paths, store, "apache");
    let out = platform::command(exe)
        .args([
            "-d".into(),
            root.to_string_lossy().to_string(),
            "-f".into(),
            paths.apache_conf().to_string_lossy().to_string(),
            "-k".into(),
            "restart".into(),
        ])
        .output()
        .map_err(|e| AppError::io("重载 Apache", e))?;
    if !out.status.success() {
        return Err(AppError::new("APACHE_RELOAD_FAILED", "Apache 重载失败")
            .with_detail(String::from_utf8_lossy(&out.stderr).to_string()));
    }
    if !wait_healthy(port, Duration::from_secs(12)) {
        return Err(AppError::new("APACHE_RELOAD_TIMEOUT", "Apache 重载后端口未恢复"));
    }
    crate::sites::retain_reloaded_endpoints(manager, "apache", site_endpoints);
    Ok(())
}

/// 重建主配置并重载（站点/池变化后）。
/// Windows 上 nginx -s reload 存在已知信号语义差异（新增 server 块可能不生效），
/// 因此 Windows 采用 stop→start；类 Unix 仅在站点入口不变时使用热 reload。
/// 入口新增或变化时同步重启，避免把异步信号发送成功当作新地址已经加载。
pub fn rebuild_and_reload(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
) -> Result<()> {
    rebuild_and_reload_selected(store, paths, manager, &["nginx", "apache"])
}

/// 证书更新只应用到实际引用它的 Web 服务，沿用原有配置验证和重载顺序。
pub(crate) fn rebuild_and_reload_selected(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    servers: &[&str],
) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    let ports = PortsProfile::from_settings(store);
    let pools = running_php_pools(store, manager);

    // ---- nginx ----
    if servers.contains(&"nginx") && nginx_exe(store).is_ok() {
        let (root, exe) = nginx_exe(store)?;
        configgen::write_nginx_conf(paths, &root, &pools, ports.http, ports.https)?;
        if manager
            .snapshot("nginx")
            .map(|s| s.state == ServiceState::Running)
            .unwrap_or(false)
        {
            configgen::validate_nginx(&exe, &paths.nginx_conf())?;
            #[cfg(windows)]
            {
                stop_service(store, paths, manager, "nginx")?;
                std::thread::sleep(Duration::from_millis(300));
                start_service(store, paths, manager, "nginx")?;
            }
            #[cfg(not(windows))]
            {
                let snapshot = crate::sites::snapshot_endpoints(paths, store, "nginx");
                if crate::sites::endpoints_changed(manager, "nginx", &snapshot) {
                    stop_service(store, paths, manager, "nginx")?;
                    start_service(store, paths, manager, "nginx")?;
                } else {
                    reload_nginx(store, paths, manager)?;
                }
            }
        }
    }

    // ---- apache ----
    if servers.contains(&"apache") && apache_paths(store).is_ok() {
        let (root, exe) = apache_paths(store)?;
        let running = manager
            .snapshot("apache")
            .map(|s| s.state == ServiceState::Running)
            .unwrap_or(false);
        let active_pools: Vec<(String, u16)> = if running {
            let mut p = pools.clone();
            p.retain(|(ver, _)| {
                manager
                    .snapshot(&format!("php@{ver}"))
                    .map(|s| s.state == ServiceState::Running)
                    .unwrap_or(false)
            });
            p
        } else {
            pools.clone()
        };
        configgen::write_httpd_conf(
            paths,
            &root,
            &active_pools,
            ports.apache_http,
            ports.apache_https,
        )?;
        if running {
            configgen::validate_httpd(&exe, &paths.apache_conf())?;
            // Windows 上 `-k restart` 是异步的：命令返回时新子进程可能尚未加载配置。
            // 连续 stop→start（如删站点后立即建站点）会与其竞态，导致服务存活但
            // 仍用旧配置（表现为新建站点 404）。这里统一走 stop_service → start_service
            // 的同步重启路径（含端口释放等待与健康检查），保证加载的是最新配置。
            #[cfg(windows)]
            {
                stop_service(store, paths, manager, "apache")?;
                std::thread::sleep(Duration::from_millis(300));
                start_service(store, paths, manager, "apache")?;
            }
            #[cfg(not(windows))]
            {
                let snapshot = crate::sites::snapshot_endpoints(paths, store, "apache");
                if crate::sites::endpoints_changed(manager, "apache", &snapshot) {
                    stop_service(store, paths, manager, "apache")?;
                    start_service(store, paths, manager, "apache")?;
                } else {
                    reload_apache(&root, &exe, paths, store, manager, manager.started_port_or("apache", ports.apache_http))?;
                }
            }
        }
    }
    Ok(())
}

/// 停全部（托盘退出时用）
pub fn stop_all(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>) {
    let _ = crate::toolbox::adminer_stop(manager);
    if let Ok(list) = store.list_installed() {
        for p in list {
            let sid = if p.id == "php" || p.id == "mysql" {
                format!("{}@{}", p.id, p.version)
            } else {
                p.id.clone()
            };
            let _ = stop_service(store, paths, manager, &sid);
        }
    }
    let _ = nginx_path; // 引用保持
}

/* ================= 孤儿进程清理 ================= */

/// 把当前托管的 pid 落盘（{data}/run/pids.json）。
/// 崩溃/被强杀时不会走到 stop_all，只能在下次启动时靠这份记录找回残留进程。
pub fn save_pidfile(paths: &Paths, manager: &Arc<ServiceManager>) {
    let _ = save_pidfile_checked(paths, manager);
}

/// 退出或迁移前必须确认恢复记录写入成功，不能把写入失败当作已完成收尾。
pub fn save_pidfile_checked(paths: &Paths, manager: &Arc<ServiceManager>) -> Result<()> {
    // 注意：必须先把 id 列表拷出来再逐个取，不能写成
    // `for id in manager.services.lock().keys()` —— 那样整个循环都持有该锁，
    // 循环体里再取同一个 Mutex 就是自死锁（parking_lot 不可重入）
    let ids: Vec<String> = manager.services.lock().keys().cloned().collect();
    let mut entries: Vec<(String, Vec<u32>)> = Vec::new();
    for id in ids {
        if let Some(e) = manager.services.lock().get(&id).cloned() {
            let pids = e.pids.lock().clone();
            if !pids.is_empty() {
                entries.push((id, pids));
            }
        }
    }
    // 管理台独立于套件服务列表；异常退出后仍由既有归属校验清理其残留进程。
    if let Some(pid) = crate::toolbox::adminer_pid(manager) {
        entries.push(("adminer-console".into(), vec![pid]));
    }
    let dir = paths.data().join("run");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::io("创建进程记录目录", e))?;
    let json = serde_json::json!({
        "appPid": std::process::id(),
        "savedAt": crate::services::now_ms(),
        "services": entries.iter().map(|(k, v)| {
            let port = manager.started_port_or(k, 0);
            serde_json::json!({"id": k, "pids": v, "port": port})
        }).collect::<Vec<_>>(),
    });
    let mut pending = tempfile::NamedTempFile::new_in(&dir)
        .map_err(|e| AppError::io("创建进程记录暂存文件", e))?;
    use std::io::Write;
    pending.write_all(json.to_string().as_bytes()).and_then(|_| pending.flush())
        .map_err(|e| AppError::io("写入进程记录", e))?;
    pending.persist(dir.join("pids.json"))
        .map_err(|e| AppError::io("保存进程记录", e.error))?;
    Ok(())
}

/// 启动时对上次会话残留的处置：**能收养就收养，收养不了才清杀**。
///
/// 背景：nsbctl CLI 分离启动的服务在 CLI 退出后仍存活（无 KILL_ON_JOB_CLOSE），
/// 桌面 App 启动时应该接管它们（按 pidfile 恢复 pid/端口/Running 态），
/// 而不是把它们当孤儿杀掉——用户在终端里起的服务被桌面端顺手杀掉会很意外。
///
/// 安全护栏：
/// 1. pidfile 写入者仍存活 → 是并发运行的另一个实例 → 整体跳过；
/// 2. 只处理 pidfile 记录过、且当前仍存活的 pid；
/// 3. pid 的可执行文件必须位于本应用 runtimes/ 内；
/// 4. 服务在注册表里 → 收养；不在（已卸载版本残留）→ 清杀；
/// 5. 本会话已 Running 的服务不动。
#[derive(Default, Debug)]
pub struct OrphanReport {
    pub adopted: Vec<(String, u32)>,
    pub killed: Vec<(String, u32)>,
}

pub fn sweep_orphans(paths: &Paths, manager: &Arc<ServiceManager>) -> OrphanReport {
    let path = paths.data().join("run").join("pids.json");
    let mut report = OrphanReport::default();
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return report;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
        let _ = std::fs::remove_file(&path);
        return report;
    };

    // 护栏 1：写入者仍存活 → 那是另一个正在运行的实例，它的服务不能被我们收走
    let writer_pid = v.get("appPid").and_then(|p| p.as_u64()).map(|p| p as u32);
    if let Some(w) = writer_pid {
        if w == std::process::id() || platform::process_alive(w) {
            return report;
        }
    }

    let runtimes = paths.runtimes();
    let runtimes_canon = std::fs::canonicalize(&runtimes).ok();
    let belongs_to_us = |pid: u32| -> bool {
        use sysinfo::{Pid, ProcessesToUpdate, System};
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true);
        let Some(p) = sys.process(Pid::from_u32(pid)) else {
            return false;
        };
        let Some(exe) = p.exe() else {
            return false;
        };
        let exe_canon = std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
        match &runtimes_canon {
            Some(rt) => exe_canon.starts_with(rt),
            None => exe_canon.starts_with(&runtimes),
        }
    };

    if let Some(arr) = v.get("services").and_then(|s| s.as_array()) {
        for item in arr {
            let Some(sid) = item.get("id").and_then(|i| i.as_str()) else {
                continue;
            };
            let Some(pids) = item.get("pids").and_then(|p| p.as_array()) else {
                continue;
            };
            // 护栏 4：本会话里已经起来的，不动
            if manager
                .snapshot(sid)
                .map(|s| s.state == ServiceState::Running)
                .unwrap_or(false)
            {
                continue;
            }
            let alive: Vec<u32> = pids
                .iter()
                .filter_map(|p| p.as_u64())
                .map(|p| p as u32)
                .filter(|pid| platform::process_alive(*pid) && belongs_to_us(*pid))
                .collect();
            if alive.is_empty() {
                continue;
            }
            let port = item.get("port").and_then(|p| p.as_u64()).map(|p| p as u16);
            // 护栏 4b：注册表里的服务 → 收养；否则清杀
            if manager.snapshot(sid).is_some() {
                manager.adopt(sid, &alive, port);
                for pid in &alive {
                    report.adopted.push((sid.to_string(), *pid));
                }
            } else {
                for pid in &alive {
                    let _ = crate::ports::kill_pid(*pid);
                    report.killed.push((sid.to_string(), *pid));
                }
            }
        }
    }
    let _ = std::fs::remove_file(&path);
    report
}

/* ================= 配置体检（不修改配置、不启动服务） ================= */

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ConfigCheck {
    pub kind: String,
    pub name: String,
    pub path: Option<String>,
    pub method: String, // native | readability | none
    pub ok: bool,
    pub status: String, // ok | warning | fail | skipped
    pub detail: String,
    pub checked_at: i64,
}

impl ConfigCheck {
    fn finish(&mut self, status: &str, detail: impl Into<String>) {
        self.status = status.into();
        self.ok = status == "ok" || status == "warning";
        self.detail = detail.into();
        self.checked_at = crate::services::now_ms();
    }
}

/// 全量体检或重试指定配置；失败逐项报告，读取安装记录失败不能伪装成未安装。
/// PHP/MySQL/Redis 按所有已安装版本检查；共享 Web 配置使用当前选中的运行版本。
pub fn validate_configs(
    store: &Store,
    paths: &Paths,
    only: Option<&[String]>,
) -> Result<Vec<ConfigCheck>> {
    validate_configs_selected(store, paths, only, None)
}

/// 单服务诊断必须使用当前服务快照的版本，不能转而验证另一个默认版本。
pub(crate) fn validate_service_config(
    store: &Store,
    paths: &Paths,
    service: &crate::model::ServiceStatus,
) -> Result<Option<ConfigCheck>> {
    Ok(validate_configs_selected(store, paths, None, Some(service))?.into_iter().next())
}

fn validate_configs_selected(
    store: &Store,
    paths: &Paths,
    only: Option<&[String]>,
    service: Option<&crate::model::ServiceStatus>,
) -> Result<Vec<ConfigCheck>> {
    use std::io::Read;
    let installed = store.list_installed()?;
    let mut checks = Vec::new();
    let mut runtimes = Vec::new();
    for (id, kind, label) in [
        ("nginx", "nginx-main", "Nginx"),
        ("apache", "apache-conf", "Apache"),
        ("php", "php-ini", "PHP"),
        ("mysql", "mysql-ini", "MySQL"),
        ("redis", "redis-conf", "Redis"),
    ] {
        if service.is_some_and(|service| service.id.split('@').next() != Some(id)) {
            continue;
        }
        let mut packages = installed.iter().filter(|p| p.id == id).collect::<Vec<_>>();
        packages.sort_by(|a, b| crate::versions::cmp_version_desc(&a.version, &b.version));
        if let Some(service) = service {
            let version = service.version.as_deref().or_else(|| service.id.split_once('@').map(|(_, version)| version))
                .filter(|version| !version.is_empty())
                .ok_or_else(|| AppError::new("SERVICE_VERSION_UNKNOWN", "无法确认当前服务版本，未执行配置校验"))?;
            packages.retain(|package| package.version == version);
            if packages.is_empty() { return Err(AppError::not_installed(&format!("{id} {version}"))); }
        } else if matches!(id, "nginx" | "apache") && !packages.is_empty() {
            let active = store.get_setting_checked(&format!("active{id}Version"))?;
            let selected = packages
                .iter()
                .find(|p| Some(&p.version) == active.as_ref())
                .copied()
                .unwrap_or(packages[0]);
            packages = vec![selected];
        }
        if packages.is_empty() {
            checks.push(ConfigCheck {
                kind: kind.into(),
                name: label.into(),
                path: None,
                method: "none".into(),
                ok: false,
                status: "skipped".into(),
                detail: "未安装，未执行检查".into(),
                checked_at: crate::services::now_ms(),
            });
            runtimes.push(None);
        }
        for package in packages {
            let key = if matches!(id, "nginx" | "apache") {
                kind.into()
            } else {
                format!("{kind}@{}", package.version)
            };
            let path = match id {
                "nginx" => paths.nginx_conf(),
                "apache" => paths.apache_conf(),
                "php" => paths.php_ini(&package.version),
                "mysql" => paths.mysql_ini(&package.version),
                _ => paths.redis_conf(&package.version),
            };
            checks.push(ConfigCheck {
                kind: key,
                name: format!("{label} {}", package.version),
                path: Some(path.to_string_lossy().into()),
                method: if matches!(id, "mysql" | "redis") {
                    "readability"
                } else {
                    "native"
                }
                .into(),
                ok: false,
                status: "fail".into(),
                detail: String::new(),
                checked_at: crate::services::now_ms(),
            });
            runtimes.push(Some(package));
        }
    }
    if let Some(keys) = only {
        if keys.is_empty()
            || keys.len() > 200
            || keys
                .iter()
                .any(|key| !checks.iter().any(|row| row.kind == *key))
        {
            return Err(AppError::new(
                "CONFIG_CHECK_TARGET_CHANGED",
                "检查目标已变更或不存在，请重新执行完整体检",
            ));
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut out = Vec::new();
    for (mut check, package) in checks.into_iter().zip(runtimes) {
        if only.is_some_and(|keys| !keys.contains(&check.kind)) {
            continue;
        }
        let Some(package) = package else {
            out.push(check);
            continue;
        };
        if std::time::Instant::now() >= deadline {
            check.finish(
                "fail",
                "本次体检已达到 60 秒时间上限，此项尚未检查，请单独重试",
            );
            out.push(check);
            continue;
        }
        let result = (|| -> Result<(String, String)> {
            let conf = PathBuf::from(check.path.as_ref().unwrap());
            if !conf.is_file() {
                return Err(AppError::new(
                    "CONFIG_FILE_UNAVAILABLE",
                    "配置文件不存在或不是普通文件；可在修复向导中生成默认配置",
                ));
            }
            // 验证路径位于应用数据目录内，不读取通过目录链接跳到外部的配置。
            let relative = conf
                .strip_prefix(&paths.base)
                .map_err(|_| AppError::new("BAD_CONFIG_PATH", "配置路径不在数据目录内"))?;
            crate::paths::checked_data_path(&paths.base, &nginx_path(relative))?;
            let mut file =
                std::fs::File::open(&conf).map_err(|e| AppError::io("读取配置文件", e))?;
            let mut first_byte = [0u8; 1];
            let size = file
                .read(&mut first_byte)
                .map_err(|e| AppError::io("读取配置文件", e))?;
            if check.method == "readability" {
                return Ok((
                    "ok".into(),
                    if size == 0 {
                        "配置文件为空且可读取；未执行服务原生语法校验，也未连接数据库".into()
                    } else {
                        "配置文件存在且可读取；未执行服务原生语法校验，也未连接数据库".into()
                    },
                ));
            }
            let timeout = deadline
                .saturating_duration_since(std::time::Instant::now())
                .min(Duration::from_secs(15));
            if package.id == "php" {
                return check_php_ini(
                    &PathBuf::from(&package.install_path).join(exe_name("php")),
                    &conf,
                    timeout,
                );
            }
            let (root, exe) = if package.id == "nginx" {
                nginx_exe_for(package)?
            } else {
                apache_paths_for(package)?
            };
            let mut command = platform::command(exe);
            command.current_dir(&root).arg("-t");
            if package.id == "nginx" {
                command.arg("-p").arg(&root).arg("-c").arg(&conf);
            } else {
                command.arg("-d").arg(&root).arg("-f").arg(&conf);
            }
            let (ok, output) = crate::cfgeditor::run_validator_with_timeout(&mut command, timeout)?;
            let status = if !ok {
                "fail"
            } else if native_check_has_warning(&output) {
                "warning"
            } else {
                "ok"
            };
            Ok((
                status.into(),
                if output.trim().is_empty() {
                    if ok {
                        "原生配置校验通过"
                    } else {
                        "原生配置校验退出失败，但没有返回诊断内容"
                    }
                    .into()
                } else {
                    output.trim().into()
                },
            ))
        })();
        match result {
            Ok((status, detail)) => check.finish(&status, detail),
            Err(error) => check.finish(
                "fail",
                [Some(error.message), error.hint, error.detail]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        }
        out.push(check);
    }
    Ok(out)
}

fn native_check_has_warning(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    ["[warn]", "warning", "deprecated", "notice", "ah00558"]
        .iter()
        .any(|word| lower.contains(word))
}

fn check_php_ini(exe: &Path, ini: &Path, timeout: Duration) -> Result<(String, String)> {
    use base64::Engine;
    // 显式加载目标 ini 并核对加载路径；不混用 -n。隔离额外扫描，不执行用户的 prepend / preload 脚本。
    let scan = tempfile::tempdir()?;
    let mut command = platform::command(exe);
    command.current_dir(exe.parent().unwrap_or_else(|| Path::new(".")))
        .env_remove("PHPRC").env("PHP_INI_SCAN_DIR", scan.path())
        .arg("-c").arg(ini)
        .args(["-d", "auto_prepend_file=", "-d", "auto_append_file=", "-d", "opcache.preload=", "-d", "opcache.enable_cli=0",
            "-d", "log_errors=0", "-d", "display_errors=stderr", "-d", "display_startup_errors=1",
            "-r", "echo \"\\nNSB_CONFIG_PROBE:\" . base64_encode((string)php_ini_loaded_file()) . \"\\n\";"]);
    let (ok, output) = crate::cfgeditor::run_validator_with_timeout(&mut command, timeout)?;
    let loaded = output
        .lines()
        .find_map(|line| line.strip_prefix("NSB_CONFIG_PROBE:"))
        .and_then(|encoded| {
            base64::engine::general_purpose::STANDARD
                .decode(encoded.trim())
                .ok()
        })
        .and_then(|bytes| String::from_utf8(bytes).ok());
    let same_file = loaded
        .as_deref()
        .and_then(|file| Path::new(file).canonicalize().ok())
        .zip(ini.canonicalize().ok())
        .is_some_and(|(loaded, expected)| loaded == expected);
    let diagnostic = output
        .lines()
        .filter(|line| !line.starts_with("NSB_CONFIG_PROBE:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    let lower = diagnostic.to_ascii_lowercase();
    if !ok
        || !same_file
        || [
            "syntax error",
            "parse error",
            "fatal error",
            "unable to load",
            "failed loading",
            "php startup:",
        ]
        .iter()
        .any(|word| lower.contains(word))
    {
        return Ok((
            "fail".into(),
            format!(
                "PHP 配置或扩展加载未通过{}{}",
                if same_file {
                    ""
                } else {
                    "：未能确认加载了所选 php.ini"
                },
                if diagnostic.is_empty() {
                    String::new()
                } else {
                    format!("\n{diagnostic}")
                }
            ),
        ));
    }
    Ok((
        if native_check_has_warning(&diagnostic) {
            "warning"
        } else {
            "ok"
        }
        .into(),
        format!(
            "已确认 PHP CLI 实际加载所选 php.ini；Web 请求与站点覆盖配置需另行检查{}",
            if diagnostic.is_empty() {
                String::new()
            } else {
                format!("\n{diagnostic}")
            }
        ),
    ))
}

#[cfg(test)]
mod validate_tests {
    use super::*;

    fn isolated_state(paths: Paths) -> crate::CoreState {
        paths.ensure_dirs().unwrap();
        crate::CoreState {
            store: Store::open(paths.db()).unwrap(),
            paths,
            manager: Arc::new(ServiceManager::new()),
            installer: crate::install::Installer::bundled(),
            downloader: Arc::new(crate::download::Downloader::new()),
            emit: Arc::new(|_| {}),
            watchdog: Arc::new(crate::watchdog::Watchdog::new()),
        }
    }

    fn register_fixture(state: &crate::CoreState, id: &str, version: &str, runtime: &Path) {
        state
            .store
            .upsert_installed(&crate::model::InstalledPackage {
                id: id.into(),
                version: version.into(),
                category: "runtime".into(),
                install_path: runtime.to_string_lossy().into(),
                config_path: String::new(),
                installed_at: 0,
            })
            .unwrap();
    }

    #[test]
    #[ignore = "requires NSB_PHP_ROOT and NSB_ADMINER_FILE; starts isolated PHP on ephemeral ports"]
    fn real_adminer_entry_readiness_state_and_cleanup() {
        use crate::toolbox;
        let php = PathBuf::from(std::env::var("NSB_PHP_ROOT").expect("NSB_PHP_ROOT"));
        let adminer = PathBuf::from(std::env::var("NSB_ADMINER_FILE").expect("NSB_ADMINER_FILE"));
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().join("adminer with spaces")));
        assert_eq!(toolbox::adminer_start_on_port(&state.store, &state.paths, &state.installer, &state.manager, 0).unwrap_err().code, "NOT_INSTALLED");
        register_fixture(&state, "php", "8.4.26", &php);
        let root = state.paths.runtime_dir("adminer", "6.1.0");
        let entry = root.join("console folder/adminer entry.php");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::copy(&adminer, &entry).unwrap();
        let mut manifest = state.installer.find("adminer@6.1.0").unwrap();
        manifest.entry = "console folder/adminer entry.php".into();
        std::fs::write(root.join(".niceenv-package.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        register_fixture(&state, "adminer", "6.1.0", &root);
        state.store.set_setting("activeadminerVersion", "6.1.0").unwrap();
        let ini = state.paths.php_ini("8.4.26");
        std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
        std::fs::write(&ini, format!("extension_dir=\"{}\"\nextension=mysqli\nsession.save_path=\"{}\"\n", php.join("ext").to_string_lossy().replace('\\', "/"), temp.path().to_string_lossy().replace('\\', "/"))).unwrap();
        let start = || toolbox::adminer_start_on_port(&state.store, &state.paths, &state.installer, &state.manager, 0).unwrap();
        let status = start();
        let pid = crate::ports::diagnose_port(status.port).unwrap().pid.unwrap();
        assert!(status.url.contains("adminer%20entry.php"), "{}", status.url);
        assert_eq!(status.php_version, "8.4.26");
        assert_eq!(state.adminer_status().unwrap().unwrap().url, status.url);
        assert_eq!(start().url, status.url);
        assert_eq!(crate::ports::diagnose_port(status.port).unwrap().pid, Some(pid));
        save_pidfile(&state.paths, &state.manager);
        let recorded: serde_json::Value = serde_json::from_slice(&std::fs::read(state.paths.data().join("run/pids.json")).unwrap()).unwrap();
        assert!(recorded["services"].as_array().unwrap().iter().any(|s| s["id"] == "adminer-console" && s["pids"][0] == pid));
        register_fixture(&state, "adminer", "99.0.0", &state.paths.runtime_dir("adminer", "99.0.0"));
        state.store.set_setting("activeadminerVersion", "99.0.0").unwrap();
        assert_eq!(start().adminer_version, "6.1.0");
        assert_eq!(state.uninstall_package("php@8.4.26").unwrap_err().code, "PACKAGE_IN_USE");
        assert_eq!(state.uninstall_package("adminer@6.1.0").unwrap_err().code, "PACKAGE_IN_USE");
        stop_all(&state.store, &state.paths, &state.manager);
        assert!(state.adminer_status().unwrap().is_none());
        assert!(!platform::process_alive(pid));
        assert!(!tcp_port_open(status.port));
        state.adminer_stop().unwrap();
        state.store.set_setting("activeadminerVersion", "6.1.0").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert_eq!(toolbox::adminer_start_on_port(&state.store, &state.paths, &state.installer, &state.manager, listener.local_addr().unwrap().port()).unwrap_err().code, "PORT_IN_USE");
        assert!(state.adminer_status().unwrap().is_none());
        drop(listener);
        std::fs::write(&entry, "<?php syntax error").unwrap();
        assert_eq!(toolbox::adminer_start_on_port(&state.store, &state.paths, &state.installer, &state.manager, 0).unwrap_err().code, "ADMINER_START_FAILED");
        assert!(state.adminer_status().unwrap().is_none());
        std::fs::remove_file(&entry).unwrap();
        assert_eq!(toolbox::adminer_start_on_port(&state.store, &state.paths, &state.installer, &state.manager, 0).unwrap_err().code, "ADMINER_ENTRY_MISSING");
    }

    #[test]
    #[ignore = "requires NSB_MYSQL_ROOT; uses two temporary isolated data directories and ephemeral ports"]
    fn real_mysql_auth_backup_restore_and_import() {
        use crate::{dbadmin, dbbackup, dbmigrate};
        let root = PathBuf::from(std::env::var("NSB_MYSQL_ROOT").expect("NSB_MYSQL_ROOT"));
        let temp = tempfile::tempdir().unwrap();
        let source = isolated_state(Paths::new(temp.path().join("source with spaces")));
        let target = isolated_state(Paths::new(temp.path().join("target with spaces")));
        struct StopOnDrop<'a>(&'a crate::CoreState);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                let _ = self.0.stop_service("mysql@8.0.46");
            }
        }
        let _source_cleanup = StopOnDrop(&source);
        let _target_cleanup = StopOnDrop(&target);
        for state in [&source, &target] {
            register_fixture(state, "mysql", "8.0.46", root.parent().unwrap());
            let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            state
                .store
                .set_port_override("mysql", Some(port.local_addr().unwrap().port()))
                .unwrap();
            drop(port);
            state.start_service("mysql@8.0.46").unwrap_or_else(|error| {
                panic!(
                    "{error:?}\n{}",
                    state.manager.tail("mysql@8.0.46", 40).join("\n")
                )
            });
        }
        let (_, mut client) = dbadmin::selected_client(&source, Some("8.0.46")).unwrap();
        let initial = dbadmin::saved_password(&source.store, "8.0.46").unwrap();
        assert_eq!(initial.len(), 24);
        assert_eq!(
            dbadmin::saved_port(&source.store, "8.0.46"),
            Some(client.port)
        );
        assert_ne!(
            initial,
            dbadmin::saved_password(&target.store, "8.0.46").unwrap()
        );
        // --initialize-insecure creates only root@localhost. Setting its password must still succeed.
        assert!(client
            .list_users()
            .unwrap()
            .iter()
            .any(|u| u.username == "root" && u.host == "localhost"));
        assert!(client
            .verify_data_dir(&target.paths.mysql_data_dir("8.0.46"))
            .is_err());
        source
            .store
            .set_port_override("mysql", Some(client.port.saturating_sub(1)))
            .unwrap();
        assert_eq!(
            dbadmin::selected_client(&source, Some("8.0.46"))
                .unwrap()
                .1
                .port,
            client.port
        );
        source
            .store
            .set_port_override("mysql", Some(client.port))
            .unwrap();
        let special = "quote' slash\\ dollar$ # semi; 中文";
        source
            .set_mysql_password(Some("8.0.46"), special, false)
            .unwrap();
        client.root_password = special.into();
        client.ping().unwrap();
        source
            .store
            .set_setting(&dbadmin::password_key("8.0.46"), "incorrect")
            .unwrap();
        assert!(dbadmin::selected_client(&source, Some("8.0.46")).is_err());
        source
            .set_mysql_password(Some("8.0.46"), special, true)
            .unwrap();
        source.stop_service("mysql@8.0.46").unwrap();
        source.start_service("mysql@8.0.46").unwrap();
        client = dbadmin::selected_client(&source, Some("8.0.46")).unwrap().1;
        assert_eq!(client.root_password, special);
        assert!(client.drop_database("mysql").is_err());
        client.create_database("niceenv_fixture").unwrap();
        client
            .create_user_grant("fixture_user", special, "niceenv_fixture")
            .unwrap();
        assert!(client
            .create_user_grant("fixture_user", "different", "niceenv_fixture")
            .is_err());
        dbadmin::query_client(
            &client.exe,
            "127.0.0.1",
            client.port,
            "fixture_user",
            special,
            "USE niceenv_fixture; SELECT 1;",
        )
        .unwrap();
        assert!(client
            .run("SHOW TABLES FROM niceenv_fixture;")
            .unwrap()
            .trim()
            .is_empty());
        client.run("CREATE TABLE niceenv_fixture.sample (id INT PRIMARY KEY, value VARCHAR(80)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;").unwrap();
        assert!(client
            .run("SHOW CREATE TABLE niceenv_fixture.sample;")
            .unwrap()
            .contains("utf8mb4"));
        client
            .run("INSERT INTO niceenv_fixture.sample VALUES (1, 'original');")
            .unwrap();
        let conn = dbbackup::ConnInfo {
            version: "8.0.46".into(),
            port: client.port,
            root_password: special.into(),
            bin_dir: Some(root.join("bin")),
        };
        let backup = dbbackup::dump_path(&source.paths, "8.0.46", "fixture.sql").unwrap();
        dbbackup::dump_databases(
            &source.paths,
            &conn,
            &["niceenv_fixture".into()],
            &backup,
            &|_| {},
        )
        .unwrap();
        let bytes = std::fs::read(&backup).unwrap();
        assert!(dbbackup::dump_databases(
            &source.paths,
            &conn,
            &["niceenv_fixture".into()],
            &backup,
            &|_| {}
        )
        .is_err());
        assert_eq!(std::fs::read(&backup).unwrap(), bytes);
        client
            .run("UPDATE niceenv_fixture.sample SET value='changed';")
            .unwrap();
        let safety = dbbackup::restore_from_file(&source.paths, &conn, &backup, true, &|_| {})
            .unwrap()
            .unwrap();
        assert!(safety.is_file());
        assert_eq!(
            client
                .run("SELECT value FROM niceenv_fixture.sample;")
                .unwrap()
                .trim(),
            "original"
        );
        // An external table-only dump can target an existing database without rewriting SQL.
        client.create_database("chosen_restore").unwrap();
        let external = temp.path().join("external plain SQL.sql");
        std::fs::write(&external, "CREATE TABLE restored (id INT PRIMARY KEY); INSERT INTO restored VALUES (42);").unwrap();
        for invalid in ["mysql", "missing_database"] {
            assert_eq!(dbbackup::restore_from_file_into(&source.paths, &conn, &external, Some(invalid), true, &|_| {}).unwrap_err().code, "RESTORE_DATABASE_INVALID");
        }
        assert!(client.run("SHOW TABLES FROM chosen_restore;").unwrap().trim().is_empty());
        let safety = dbbackup::restore_from_file_into(&source.paths, &conn, &external, Some("chosen_restore"), true, &|_| {}).unwrap().unwrap();
        assert!(safety.is_file());
        assert_eq!(client.run("SELECT id FROM chosen_restore.restored;").unwrap().trim(), "42");
        assert!(client.run("SHOW TABLES FROM niceenv_fixture LIKE 'restored';").unwrap().trim().is_empty());
        std::fs::write(&external, "USE niceenv_fixture; INSERT INTO sample VALUES (99, 'explicit database');").unwrap();
        dbbackup::restore_from_file_into(&source.paths, &conn, &external, Some("chosen_restore"), true, &|_| {}).unwrap();
        assert_eq!(client.run("SELECT value FROM niceenv_fixture.sample WHERE id=99;").unwrap().trim(), "explicit database");
        client.run("DELETE FROM niceenv_fixture.sample WHERE id=99;").unwrap();
        // A failing safety export must not execute even the first SQL statement of the requested restore.
        let blocked = isolated_state(Paths::new(temp.path().join("blocked backup")));
        std::fs::write(blocked.paths.backup().join("db"), "not a directory").unwrap();
        client
            .run("UPDATE niceenv_fixture.sample SET value='must remain';")
            .unwrap();
        assert!(
            dbbackup::restore_from_file(&blocked.paths, &conn, &backup, true, &|_| {}).is_err()
        );
        assert_eq!(
            client
                .run("SELECT value FROM niceenv_fixture.sample;")
                .unwrap()
                .trim(),
            "must remain"
        );
        let (_, target_client) = dbadmin::selected_client(&target, Some("8.0.46")).unwrap();
        let target_conn = dbbackup::ConnInfo {
            version: "8.0.46".into(),
            port: target_client.port,
            root_password: target_client.root_password.clone(),
            bin_dir: Some(root.join("bin")),
        };
        let src = dbmigrate::SourceConn {
            host: "127.0.0.1".into(),
            port: client.port,
            user: "root".into(),
            password: special.into(),
        };
        let report = dbmigrate::import_databases(
            &target.paths,
            &src,
            &["niceenv_fixture".into()],
            &target_conn,
            |_, _| {},
        )
        .unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(
            target_client
                .run("SELECT value FROM niceenv_fixture.sample;")
                .unwrap()
                .trim(),
            "must remain"
        );
        assert!(dbmigrate::import_databases(
            &source.paths,
            &src,
            &["niceenv_fixture".into()],
            &conn,
            |_, _| {}
        )
        .is_err());
        let (private, command) =
            dbadmin::client_command(&client.exe, "127.0.0.1", client.port, "root", special)
                .unwrap();
        assert!(!command
            .get_args()
            .any(|arg| arg.to_string_lossy().contains(special)));
        let path = private.path().to_path_buf();
        drop((command, private));
        assert!(!path.exists());
        for state in [&source, &target] {
            let pids = state.manager.snapshot("mysql@8.0.46").unwrap().pids;
            state.stop_service("mysql@8.0.46").unwrap();
            assert!(pids.into_iter().all(|pid| !platform::process_alive(pid)));
        }
    }

    fn http_response(port: u16) -> String {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    #[ignore = "requires NSB_APACHE_ROOT; starts an isolated Apache with temporary configuration"]
    fn real_apache_custom_config_survives_rebuild_and_port_changes() {
        let root = PathBuf::from(std::env::var("NSB_APACHE_ROOT").expect("NSB_APACHE_ROOT"));
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().join("apache with spaces")));
        register_fixture(&state, "apache", "2.4.66", root.parent().unwrap());
        let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let https = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = http.local_addr().unwrap().port();
        state
            .store
            .set_port_override("apacheHttp", Some(port))
            .unwrap();
        state
            .store
            .set_port_override("apacheHttps", Some(https.local_addr().unwrap().port()))
            .unwrap();
        drop((http, https));
        state.start_service("apache").unwrap();
        std::fs::write(
            state.paths.etc().join("apache/htdocs/index.html"),
            "isolated apache",
        )
        .unwrap();
        let original = std::fs::read_to_string(state.paths.apache_conf()).unwrap();
        let customized =
            format!("{original}\nTimeout 123\nHeader always set X-NiceEnv-Custom \"persisted\"\n");
        state
            .save_config("apache-conf", &customized, false, Some(&original))
            .unwrap();
        rebuild_and_reload(&state.store, &state.paths, &state.manager).unwrap();
        let response = http_response(port);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(
            response
                .to_ascii_lowercase()
                .contains("x-niceenv-custom: persisted"),
            "{response}"
        );
        state.stop_service("apache").unwrap();
        let new_http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let new_port = new_http.local_addr().unwrap().port();
        state
            .store
            .set_port_override("apacheHttp", Some(new_port))
            .unwrap();
        drop(new_http);
        state.start_service("apache").unwrap();
        let response = http_response(new_port);
        assert!(response.contains("isolated apache"), "{response}");
        assert!(response
            .to_ascii_lowercase()
            .contains("x-niceenv-custom: persisted"));
        assert!(std::fs::read_to_string(state.paths.apache_conf())
            .unwrap()
            .contains("Timeout 123"));
        let pids = state.manager.snapshot("apache").unwrap().pids;
        state.stop_service("apache").unwrap();
        assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        println!("Apache: native validation and HTTP confirmed custom header across rebuild and port change; owned processes stopped");
    }

    #[test]
    #[ignore = "requires NSB_REDIS_ROOT; starts only an isolated Redis on ephemeral ports"]
    fn real_redis_keeps_settings_and_records_stable_fallback_port() {
        let source = PathBuf::from(std::env::var("NSB_REDIS_ROOT").expect("NSB_REDIS_ROOT"));
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().join("redis with spaces")));
        let root = state.paths.runtime_dir("redis", "5.0.14");
        std::fs::create_dir_all(&root).unwrap();
        for file in ["redis-server.exe", "redis-cli.exe", "EventLog.dll"] {
            std::fs::copy(source.join(file), root.join(file)).unwrap();
        }
        register_fixture(&state, "redis", "5.0.14", &root);
        struct StopRedisOnDrop<'a>(&'a crate::CoreState);
        impl Drop for StopRedisOnDrop<'_> {
            fn drop(&mut self) {
                if self.0.stop_service("redis").is_err() {
                    if let Some(service) = self.0.manager.snapshot("redis") {
                        let _ = platform::ProcessGroup::from_pids(service.pids).terminate(true);
                    }
                }
            }
        }
        let _cleanup = StopRedisOnDrop(&state);
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let desired = occupied.local_addr().unwrap().port();
        state
            .store
            .set_port_override("redis", Some(desired))
            .unwrap();
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        configgen::write_redis_conf(&state.paths, "5.0.14", desired).unwrap();
        let config = state.paths.redis_conf("5.0.14");
        let customized = std::fs::read_to_string(&config)
            .unwrap()
            .replace("maxmemory 256mb", "maxmemory 64mb")
            .replace(
                "maxmemory-policy allkeys-lru",
                "maxmemory-policy noeviction",
            )
            .replace("databases 16", "databases 32")
            .replace("save \"\"", "save 3600 1");
        std::fs::write(&config, &customized).unwrap();
        state.start_service("redis").unwrap();
        let port = PortsProfile::from_settings(&state.store).redis;
        assert_ne!(port, desired);
        assert_eq!(state.manager.snapshot("redis").unwrap().port, Some(port));
        let query = |key: &str| {
            let output = platform::command(&root.join("redis-cli.exe"))
                .args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &port.to_string(),
                    "--raw",
                    "CONFIG",
                    "GET",
                    key,
                ])
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .replace("\r\n", "\n")
        };
        assert_eq!(query("maxmemory"), "maxmemory\n67108864");
        assert_eq!(query("maxmemory-policy"), "maxmemory-policy\nnoeviction");
        assert_eq!(query("databases"), "databases\n32");
        let initial = state.redis_stats().unwrap();
        assert_eq!(initial.port, port);
        assert_eq!(initial.keys, Some(0));
        state.store.set_port_override("redis", Some(desired)).unwrap();
        assert_eq!(state.redis_stats().unwrap().port, port);
        state.store.set_port_override("redis", Some(port)).unwrap();
        for database in [0, 2] {
            let out = platform::command(root.join("redis-cli.exe"))
                .args(["-p", &port.to_string(), "-n", &database.to_string(), "SET", "isolated-fixture", "1"])
                .output().unwrap();
            assert!(out.status.success());
        }
        assert_eq!(state.redis_stats().unwrap().keys, Some(2));
        let set_password = |password: &str| {
            use std::io::{Read, Write};
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let command = format!("*4\r\n$6\r\nCONFIG\r\n$3\r\nSET\r\n$11\r\nrequirepass\r\n${}\r\n{}\r\n", password.len(), password);
            stream.write_all(command.as_bytes()).unwrap();
            let mut response = [0; 5]; stream.read_exact(&mut response).unwrap();
            assert_eq!(&response, b"+OK\r\n");
            stream
        };
        drop(set_password("isolated-fixture-password"));
        assert_eq!(state.redis_stats().unwrap_err().code, "REDIS_AUTH_REQUIRED");
        assert_eq!(state.stop_service("redis").unwrap_err().code, "REDIS_AUTH_REQUIRED");
        assert!(state.manager.snapshot("redis").unwrap().pids.iter().any(|pid| platform::process_alive(*pid)));
        let running_pids = state.manager.snapshot("redis").unwrap().pids;
        let ids = vec!["redis".into(), "redis".into()];
        let stopped = crate::bulk::stop_many(&state.store, &state.paths, &state.manager, &ids).unwrap();
        assert_eq!(stopped.order, vec!["redis"]);
        assert_eq!(stopped.failed.len(), 1);
        assert_eq!(stopped.failed[0].error.code, "REDIS_AUTH_REQUIRED");
        assert!(stopped.succeeded.is_empty() && stopped.already.is_empty());
        let restarted = crate::bulk::restart_many(&state.store, &state.paths, &state.manager, &ids).unwrap();
        assert_eq!(restarted.failed.len(), 1);
        assert_eq!(restarted.failed[0].error.code, "REDIS_AUTH_REQUIRED");
        assert!(restarted.succeeded.is_empty() && restarted.already.is_empty());
        let stopped_stack = state.stop_stack("builtin-data").unwrap();
        assert_eq!(stopped_stack.failed.len(), 1);
        assert_eq!(stopped_stack.failed[0].service_id, "redis");
        assert!(stopped_stack.started.is_empty() && stopped_stack.already_running.is_empty());
        assert_eq!(state.manager.snapshot("redis").unwrap().pids, running_pids);
        let credentials = |password: &str| crate::stats::RedisCredentials { username: String::new(), password: password.into() };
        assert_eq!(state.save_redis_connection("5.0.14", credentials("wrong")).unwrap_err().code, "REDIS_AUTH_FAILED");
        assert!(state.store.get_setting(&crate::stats::RedisCredentials::key("5.0.14")).is_none());
        state.save_redis_connection("5.0.14", credentials("isolated-fixture-password")).unwrap();
        assert_eq!(state.redis_stats().unwrap().keys, Some(2));
        assert_eq!(state.save_redis_connection("5.0.14", credentials("wrong")).unwrap_err().code, "REDIS_AUTH_FAILED");
        assert_eq!(crate::stats::RedisCredentials::load(&state.store, "5.0.14").unwrap().password, "isolated-fixture-password");
        assert!(crate::stats::RedisCredentials::load(&state.store, "7.0.0").unwrap().password.is_empty());
        assert_eq!(state.save_redis_connection("7.0.0", credentials("isolated-fixture-password")).unwrap_err().code, "REDIS_INSTANCE_CHANGED");
        let info = state.redis_connection("5.0.14").unwrap();
        assert!(info.has_password);
        assert!(!serde_json::to_string(&info).unwrap().contains("isolated-fixture-password"));
        let exported = temp.path().join("config-backup.json");
        crate::transfer::export_to(&state.store, &exported).unwrap();
        assert!(!std::fs::read_to_string(&exported).unwrap().contains("redisConnection@"));
        let persisted = std::fs::read_to_string(&config).unwrap();
        state.stop_service("redis").unwrap();
        assert!(state.paths.redis_data_dir().join("dump.rdb").is_file());
        state.start_service("redis").unwrap();
        // requirepass was a runtime-only change: restarted server is unauthenticated again.
        assert_eq!(state.redis_stats().unwrap_err().code, "REDIS_AUTH_FAILED");
        state.save_redis_connection("5.0.14", crate::stats::RedisCredentials::default()).unwrap();
        assert_eq!(state.redis_stats().unwrap().keys, Some(2));
        assert!(!state.redis_connection("5.0.14").unwrap().has_password);
        assert_eq!(state.manager.snapshot("redis").unwrap().port, Some(port));
        assert_eq!(PortsProfile::from_settings(&state.store).redis, port);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), persisted);
        assert_eq!(query("maxmemory"), "maxmemory\n67108864");
        let pids = state.manager.snapshot("redis").unwrap().pids;
        state.stop_service("redis").unwrap();
        assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        assert_eq!(occupied.local_addr().unwrap().port(), desired);
        println!("Redis: CONFIG GET confirmed memory/policy/databases across restarts; fallback port stable and recorded; owned processes stopped");
    }

    #[test]
    #[ignore = "requires NSB_MYSQL_ROOT; runs mysqld --verbose --help only, without initializing a database"]
    fn real_mysql_option_parser_keeps_tuning_and_applies_owned_launch_options() {
        let root = PathBuf::from(std::env::var("NSB_MYSQL_ROOT").expect("NSB_MYSQL_ROOT"));
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("mysql with spaces"));
        paths.ensure_dirs().unwrap();
        configgen::write_mysql_ini(&paths, "8.0.46", &root, 23306).unwrap();
        let file = paths.mysql_ini("8.0.46");
        let extra = temp.path().join("extra.ini");
        std::fs::write(
            &extra,
            "[mysqld]\nport=1\ndatadir=C:/unused-config-fixture\n",
        )
        .unwrap();
        let custom = std::fs::read_to_string(&file)
            .unwrap()
            .replace("max_connections=200", "max_connections=321")
            .replace(
                "innodb_buffer_pool_size=256M",
                "innodb_buffer_pool_size=32M",
            );
        std::fs::write(
            &file,
            format!("{custom}\n!include {}\n", nginx_path(&extra)),
        )
        .unwrap();
        configgen::write_mysql_ini(&paths, "8.0.46", &root, 23307).unwrap();
        let mut args = mysql_launch_args(&paths, "8.0.46", &root, 23307);
        args.extend(["--verbose".into(), "--help".into()]);
        let output = platform::command(&root.join("bin/mysqld.exe"))
            .current_dir(&root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let help = String::from_utf8_lossy(&output.stdout);
        for (option, expected) in [
            ("max-connections", "321"),
            ("innodb-buffer-pool-size", "33554432"),
            ("port", "23307"),
        ] {
            assert!(
                help.lines()
                    .any(|line| line.split_whitespace().collect::<Vec<_>>() == [option, expected]),
                "missing {option}={expected}"
            );
        }
        let datadir = help
            .lines()
            .find(|line| line.split_whitespace().next() == Some("datadir"))
            .unwrap();
        assert!(datadir
            .replace('\\', "/")
            .contains(&nginx_path(&paths.mysql_data_dir("8.0.46"))));
        assert!(!paths.mysql_data_dir("8.0.46").exists());
        println!("MySQL: actual option parser confirmed tuning and command-line port/datadir precedence; no server or database initialization");
    }

    #[test]
    #[ignore = "requires NSB_NGINX_ROOT; starts only a copied Nginx on isolated ephemeral ports"]
    fn selected_nginx_version_survives_install_and_uninstall_of_other_versions() {
        use crate::model::InstalledPackage;
        use std::io::{Read, Write};
        let source = PathBuf::from(std::env::var("NSB_NGINX_ROOT").expect("NSB_NGINX_ROOT"));
        let version = source
            .file_name()
            .unwrap()
            .to_string_lossy()
            .strip_prefix("nginx-")
            .expect("nginx-version directory")
            .to_string();
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let manager = Arc::new(ServiceManager::new());
        let state = crate::CoreState {
            paths,
            store,
            manager,
            installer: crate::install::Installer::bundled(),
            downloader: Arc::new(crate::download::Downloader::new()),
            emit: Arc::new(|_| {}),
            watchdog: Arc::new(crate::watchdog::Watchdog::new()),
        };
        struct StopOnDrop<'a>(&'a crate::CoreState);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                let _ = self.0.stop_service("nginx");
            }
        }
        let _cleanup = StopOnDrop(&state);
        for ver in [&version, "1.0.0"] {
            let runtime = state.paths.runtime_dir("nginx", ver);
            std::fs::create_dir_all(&runtime).unwrap();
            state
                .store
                .upsert_installed(&InstalledPackage {
                    id: "nginx".into(),
                    version: ver.into(),
                    category: "web-server".into(),
                    install_path: runtime.to_string_lossy().into(),
                    config_path: String::new(),
                    installed_at: 0,
                })
                .unwrap();
        }
        let root = state
            .paths
            .runtime_dir("nginx", &version)
            .join(format!("nginx-{version}"));
        std::fs::create_dir_all(root.join("conf")).unwrap();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::copy(source.join(exe_name("nginx")), root.join(exe_name("nginx"))).unwrap();
        std::fs::copy(source.join("conf/mime.types"), root.join("conf/mime.types")).unwrap();
        let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let https = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = http.local_addr().unwrap().port();
        let https_port = https.local_addr().unwrap().port();
        state.store.set_port_override("http", Some(port)).unwrap();
        state
            .store
            .set_port_override("https", Some(https_port))
            .unwrap();
        let planned = PortsProfile::from_settings(&state.store);
        assert_eq!((planned.http, planned.https), (port, https_port));
        state.set_active_version("nginx", &version).unwrap();
        drop(http);
        drop(https);
        state.start_service("nginx").unwrap();
        let before = state.manager.snapshot("nginx").unwrap();
        assert_eq!(before.version.as_deref(), Some(version.as_str()));
        assert_eq!(before.port, Some(port));
        assert_eq!(
            state.set_active_version("nginx", "1.0.0").unwrap_err().code,
            "SERVICE_BUSY"
        );
        state.uninstall_package("nginx@1.0.0").unwrap();
        assert_eq!(state.manager.snapshot("nginx").unwrap().pids, before.pids);
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(before.pids.iter().all(|pid| platform::process_alive(*pid)));

        let newer = state.paths.runtime_dir("nginx", "99.0.0");
        std::fs::create_dir_all(&newer).unwrap();
        state
            .store
            .upsert_installed(&InstalledPackage {
                id: "nginx".into(),
                version: "99.0.0".into(),
                category: "web-server".into(),
                install_path: newer.to_string_lossy().into(),
                config_path: String::new(),
                installed_at: 0,
            })
            .unwrap();
        register_services(&state.paths, &state.store, &state.manager);
        assert_eq!(
            installed_by_choice(&state.store, "nginx").unwrap().version,
            version
        );
        assert_eq!(
            state.manager.snapshot("nginx").unwrap().version.as_deref(),
            Some(version.as_str())
        );
        let original = std::fs::read_to_string(state.paths.nginx_conf()).unwrap();
        let custom = original.replace("gzip on;", "gzip off;").replace(
            "http {",
            "http {\n    add_header X-NiceEnv-Custom persisted always;",
        );
        state
            .save_config("nginx-main", &custom, false, Some(&original))
            .unwrap();
        state.store.set_port_assign("php@8.4.26", 9110).unwrap();
        rebuild_and_reload(&state.store, &state.paths, &state.manager).unwrap();
        assert!(http_response(port)
            .to_ascii_lowercase()
            .contains("x-niceenv-custom: persisted"));
        assert!(std::fs::read_to_string(state.paths.nginx_conf())
            .unwrap()
            .contains("nsb_php_8_4_26"));
        state.stop_service("nginx").unwrap();
        let next = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let next_port = next.local_addr().unwrap().port();
        state
            .store
            .set_port_override("http", Some(next_port))
            .unwrap();
        drop(next);
        state.start_service("nginx").unwrap();
        assert!(http_response(next_port)
            .to_ascii_lowercase()
            .contains("x-niceenv-custom: persisted"));
        assert!(std::fs::read_to_string(state.paths.nginx_conf())
            .unwrap()
            .contains("gzip off;"));
        let last_pids = state.manager.snapshot("nginx").unwrap().pids;
        state.stop_service("nginx").unwrap();
        let stopped = state.manager.snapshot("nginx").unwrap();
        assert_eq!(stopped.state, ServiceState::Stopped);
        assert!(stopped.pids.is_empty());
        assert!(stopped.uptime_sec.is_none());
        assert!(before.pids.iter().all(|pid| !platform::process_alive(*pid)));
        assert!(last_pids.iter().all(|pid| !platform::process_alive(*pid)));
        println!("selected Nginx {version}: real HTTP confirmed custom header across rebuild, pool and port changes; stopped cleanly");
    }

    #[test]
    fn default_version_keeps_live_instances_and_site_bindings() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        assert!(!crate::pathenv::is_enabled(&state.store));
        let site: crate::model::Site = serde_json::from_value(serde_json::json!({
            "id": "pinned-site", "name": "Pinned PHP", "domains": ["pinned.test"],
            "rootDir": temp.path().to_string_lossy(),
            "runtime": { "kind": "php", "phpVersion": "8.3.17" },
            "https": false, "rewrite": "none", "db": null,
            "createdAt": 1, "updatedAt": 1
        })).unwrap();
        state.store.save_site(&site).unwrap();
        let original_site = serde_json::to_value(state.store.list_sites().unwrap()).unwrap();
        for (id, versions) in [("php", ["7.4.33", "8.3.17"]), ("mysql", ["5.7.44", "8.0.46"])] {
            for version in versions {
                let runtime = state.paths.runtime_dir(id, version);
                register_fixture(&state, id, version, &runtime);
                let installed = state.store.find_installed(id, Some(version)).unwrap();
                let entry = state.installer.installed_entry(&installed);
                let executable = runtime.join(&entry.entry);
                std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
            }
        }
        register_services(&state.paths, &state.store, &state.manager);
        // 仅借用当前测试 PID 验证存活；本用例不执行任何启停操作。
        for (id, old, latest) in [("php", "7.4.33", "8.3.17"), ("mysql", "5.7.44", "8.0.46")] {
            let sid = format!("{id}@{latest}");
            state.manager.adopt(&sid, &[std::process::id()], None);
            state.set_active_version(id, old).unwrap();
            assert_eq!(installed_by_choice(&state.store, id).unwrap().version, old);
            let active = state.list_packages().unwrap().into_iter()
                .filter(|p| p.manifest.id == id && p.active)
                .map(|p| p.manifest.version).collect::<Vec<_>>();
            assert_eq!(active, vec![old]);
            let old_install = state.store.find_installed(id, Some(old)).unwrap();
            let old_meta = state.installer.installed_entry(&old_install);
            let selected_dir = crate::pathenv::bin_dir_for(&old_install.install_path, &old_meta.entry).unwrap();
            assert!(crate::pathenv::desired_dirs(&state.store, &state.installer.manifest).contains(&selected_dir));
            let running = state.manager.snapshot(&sid).unwrap();
            assert_eq!(running.state, ServiceState::Running);
            assert_eq!(running.pids, vec![std::process::id()]);
            assert_eq!(state.manager.snapshot(&format!("{id}@{old}")).unwrap().state, ServiceState::Stopped);
            assert_eq!(state.set_active_version(id, "0.0.0").unwrap_err().code, "NOT_INSTALLED");
            assert_eq!(installed_by_choice(&state.store, id).unwrap().version, old);
        }
        assert_eq!(serde_json::to_value(state.store.list_sites().unwrap()).unwrap(), original_site);
        let reopened = Store::open(state.paths.db()).unwrap();
        assert_eq!(installed_by_choice(&reopened, "php").unwrap().version, "7.4.33");
        assert_eq!(installed_by_choice(&reopened, "mysql").unwrap().version, "5.7.44");
        assert!(!crate::pathenv::is_enabled(&reopened));
        assert!(reopened.get_setting("pathEnvDirs").is_none());
    }

    #[test]
    fn default_version_rejects_busy_single_instance_without_changing_selection() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        for version in ["1.26.3", "1.28.0"] {
            register_fixture(&state, "nginx", version, &state.paths.runtime_dir("nginx", version));
        }
        state.set_active_version("nginx", "1.26.3").unwrap();
        state.manager.adopt("nginx", &[std::process::id()], Some(18080));
        assert_eq!(state.set_active_version("nginx", "1.28.0").unwrap_err().code, "SERVICE_BUSY");
        assert_eq!(installed_by_choice(&state.store, "nginx").unwrap().version, "1.26.3");
        assert_eq!(state.manager.snapshot("nginx").unwrap().pids, vec![std::process::id()]);
        // 对已经选中的版本重试不会重启服务，也不会破坏现有 PID。
        state.set_active_version("nginx", "1.26.3").unwrap();
        assert_eq!(state.manager.snapshot("nginx").unwrap().version.as_deref(), Some("1.26.3"));
    }

    #[test]
    fn config_check_reports_broken_runtime_and_all_database_versions() {
        let base = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(base.path().to_path_buf()));
        register_fixture(&state, "nginx", "1.28.0", &base.path().join("missing-nginx"));
        std::fs::create_dir_all(state.paths.nginx_conf().parent().unwrap()).unwrap();
        std::fs::write(state.paths.nginx_conf(), "events {}\nhttp {}\n").unwrap();
        for (id, version) in [("mysql", "5.7.44"), ("mysql", "8.0.46"), ("redis", "5.0.14"), ("redis", "7.2.0")] {
            register_fixture(&state, id, version, &base.path().join(format!("{id}-{version}")));
            let config = if id == "mysql" { state.paths.mysql_ini(version) } else { state.paths.redis_conf(version) };
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            if version == "7.2.0" { std::fs::create_dir_all(config).unwrap(); }
            else { std::fs::write(config, "unsupported_option\n").unwrap(); }
        }
        let report = state.validate_configs(None).unwrap();
        let nginx = report.iter().find(|row| row.kind == "nginx-main").unwrap();
        assert_eq!(nginx.status, "fail");
        assert!(nginx.detail.contains("找不到"));
        assert_eq!(report.iter().filter(|row| row.kind.starts_with("mysql-ini@")).count(), 2);
        let mysql = report.iter().find(|row| row.kind == "mysql-ini@8.0.46").unwrap();
        assert_eq!(mysql.method, "readability");
        assert!(mysql.detail.contains("未执行服务原生语法校验"));
        assert_eq!(report.iter().find(|row| row.kind == "redis-conf@7.2.0").unwrap().status, "fail");
        let retry = state.validate_configs(Some(&["mysql-ini@5.7.44".into()])).unwrap();
        assert_eq!(retry.len(), 1);
        assert!(retry[0].path.as_deref().unwrap().contains("5.7.44"));
        assert!(state.validate_configs(Some(&["mysql-ini@missing".into()])).is_err());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires NSB_NGINX_ROOT; native nginx -t against temporary configuration, no service"]
    fn native_nginx_config_check_uses_active_runtime_and_returns_errors() {
        let source = PathBuf::from(std::env::var("NSB_NGINX_ROOT").expect("NSB_NGINX_ROOT"));
        let version = source.file_name().unwrap().to_string_lossy().strip_prefix("nginx-").unwrap().to_string();
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().join("checks with spaces")));
        let install = state.paths.runtime_dir("nginx", &version);
        let runtime = install.join(format!("nginx-{version}"));
        std::fs::create_dir_all(runtime.join("logs")).unwrap();
        std::fs::copy(source.join("nginx.exe"), runtime.join("nginx.exe")).unwrap();
        register_fixture(&state, "nginx", &version, &install);
        register_fixture(&state, "nginx", "99.0.0", &temp.path().join("broken-version"));
        state.store.set_setting("activenginxVersion", &version).unwrap();
        let conf = state.paths.nginx_conf();
        std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
        std::fs::write(&conf, "events {}\nhttp {}\n").unwrap();
        let report = state.validate_configs(Some(&["nginx-main".into()])).unwrap();
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].status, "ok", "{:?}", report[0]);
        assert!(report[0].name.contains(&version));
        state.manager.register("nginx", "Nginx", Some(version.clone()), None, None, state.paths.service_log("nginx"));
        // 默认选择已变化，但单服务诊断必须继续验证快照对应的原生程序。
        state.store.set_setting("activenginxVersion", "99.0.0").unwrap();
        let service_report = crate::diagnostics::diagnose_service(&state.paths, &state.store, &state.manager, "nginx").unwrap();
        let config = service_report.checks.iter().find(|check| check.id == "config").unwrap();
        assert_eq!(config.state, crate::diagnostics::ServiceCheckState::Ok, "{}", config.detail);
        assert_eq!(config.method, "native");
        assert!(config.detail.contains(&version));
        state.store.set_setting("activenginxVersion", &version).unwrap();
        let bad = "events {}\nunknown_directive yes;\n";
        std::fs::write(&conf, bad).unwrap();
        let failed = state.validate_configs(Some(&["nginx-main".into()])).unwrap();
        assert_eq!(failed[0].status, "fail");
        assert!(failed[0].detail.contains("unknown directive"));
        assert_eq!(std::fs::read_to_string(conf).unwrap(), bad);
    }

    #[test]
    fn config_check_reports_installed_record_read_errors() {
        let base = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(base.path().to_path_buf()));
        // 仅破坏临时夹具数据库；验证读取失败不能返回一组「未安装」。
        rusqlite::Connection::open(state.paths.db()).unwrap().execute_batch("DROP TABLE installed").unwrap();
        assert!(state.validate_configs(None).is_err());
    }

    #[test]
    #[ignore = "requires NSB_PHP_ROOT; runs finite PHP CLI configuration checks, no service"]
    fn native_php_config_check_detects_zero_exit_errors_and_missing_extensions() {
        let php = PathBuf::from(std::env::var("NSB_PHP_ROOT").expect("NSB_PHP_ROOT"));
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().join("config with spaces")));
        register_fixture(&state, "php", "8.4.26", &php);
        let ini = state.paths.php_ini("8.4.26");
        std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
        std::fs::write(&ini, "[PHP]\nmemory_limit=128M\n").unwrap();
        let check = || state.validate_configs(Some(&["php-ini@8.4.26".into()])).unwrap().remove(0);
        assert_eq!(check().status, "ok");
        let bad = "[PHP]\nmemory_limit = \"unclosed\n";
        std::fs::write(&ini, bad).unwrap();
        let failed = check();
        assert_eq!(failed.status, "fail");
        assert!(failed.detail.contains("syntax error"));
        assert_eq!(std::fs::read_to_string(&ini).unwrap(), bad);
        std::fs::write(&ini, "[PHP]\nextension=niceenv_missing_extension_for_verification\n").unwrap();
        let failed = check();
        assert_eq!(failed.status, "fail");
        assert!(failed.detail.to_lowercase().contains("unable to load"));
        state.manager.register("php@8.4.26", "PHP", Some("8.4.26".into()), None, None, state.paths.service_log("php"));
        let service_report = crate::diagnostics::diagnose_service(&state.paths, &state.store, &state.manager, "php@8.4.26").unwrap();
        let config = service_report.checks.iter().find(|check| check.id == "config").unwrap();
        assert_eq!(config.state, crate::diagnostics::ServiceCheckState::Error);
        assert!(config.detail.to_lowercase().contains("unable to load"));
        let user_script = temp.path().join("prepend.php");
        let sentinel = temp.path().join("must-not-exist");
        std::fs::write(&user_script, format!("<?php file_put_contents('{}', 'executed');", sentinel.to_string_lossy().replace('\\', "/"))).unwrap();
        std::fs::write(&ini, format!("[PHP]\nauto_prepend_file=\"{}\"\n", user_script.to_string_lossy().replace('\\', "/"))).unwrap();
        assert_eq!(check().status, "ok");
        assert!(!sentinel.exists());
        assert!(!state.paths.backup().exists() || std::fs::read_dir(state.paths.backup()).unwrap().next().is_none());
    }

    #[test]
    fn empty_install_reports_all_skipped() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::new(base.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let checks = validate_configs(&store, &paths, None).unwrap();
        assert!(!checks.is_empty());
        // 什么都没装时体检不该有失败项
        assert!(
            checks.iter().all(|c| c.status == "skipped" && !c.ok),
            "未安装不应报 fail：{:?}",
            checks
        );
        assert!(checks.iter().any(|c| c.status == "skipped"));
    }
    #[test]
    fn service_lifecycle_rejects_unknown_and_concurrent_operations() {
        let temp = tempfile::tempdir().unwrap();
        let state = Arc::new(isolated_state(Paths::new(temp.path().to_path_buf())));
        assert_eq!(state.restart_service("coredns").unwrap_err().code, "UNKNOWN_SERVICE");
        assert_eq!(state.stop_service("missing").unwrap_err().code, "UNKNOWN_SERVICE");
        let mut freed = Vec::new();
        assert_eq!(state.start_service_with_port_policy("missing", |p| freed.push(p)).unwrap_err().code, "UNKNOWN_SERVICE");
        assert!(freed.is_empty());
        let _lock = state.manager.lifecycle.lock();
        let other = state.clone();
        std::thread::spawn(move || {
            assert_eq!(other.start_service("missing").unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.stop_service("missing").unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.restart_service("missing").unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.start_service_with_port_policy("missing", |_| panic!("must not free ports")).unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.bulk_start(&[]).unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.bulk_stop(&[]).unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.bulk_restart(&[]).unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.start_stack("missing").unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.stop_stack("missing").unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.stop_all_services().unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.with_stopped_services(|| -> Result<()> { panic!("must not transition"); }).unwrap_err().code, "SERVICE_BUSY");
        }).join().unwrap();
    }

    #[test]
    fn service_lifecycle_failed_stop_never_starts_and_watchdog_ignores_survivors() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        state.manager.register("redis", "Redis", None, None, None, state.paths.service_log("redis"));
        // 缺少版本会在发送停机请求前失败；使用本测试 PID 证明它仍在，不执行结束命令。
        state.manager.adopt("redis", &[std::process::id()], None);
        state.watchdog.note_started("redis");
        state.store.set_setting("watchdogEnabled", "true").unwrap();
        let error = state.restart_service("redis").unwrap_err();
        assert_eq!(error.code, "REDIS_VERSION_UNKNOWN");
        assert!(error.message.contains("停止阶段失败"));
        let snapshot = state.manager.snapshot("redis").unwrap();
        assert_eq!(snapshot.state, ServiceState::Error);
        assert_eq!(snapshot.pids, [std::process::id()]);
        assert_eq!(snapshot.last_error.unwrap().code, "REDIS_VERSION_UNKNOWN");
        assert!(state.watchdog_tick().is_empty());
        assert_eq!(state.watchdog_status().watched[0].attempts, 0);
        let report = state.bulk_restart(&["redis".into(), "missing".into(), "redis".into()]).unwrap();
        assert_eq!(report.failed.len(), 2);
        assert!(report.succeeded.is_empty() && report.already.is_empty());
        assert!(report.failed.iter().any(|f| f.service_id == "redis" && f.error.message.contains("停止阶段失败")));
        let stopped = state.stop_all_services().unwrap();
        assert_eq!(stopped.failed[0].service_id, "redis");
        let pidfile: serde_json::Value = serde_json::from_slice(&std::fs::read(state.paths.data().join("run/pids.json")).unwrap()).unwrap();
        assert_eq!(pidfile["services"][0]["pids"][0], std::process::id());
        assert_eq!(state.with_stopped_services(|| -> Result<()> { panic!("must not exit or launch installer"); }).unwrap_err().code, "SERVICES_STOP_FAILED");
        let target = temp.path().join("must-not-copy");
        assert_eq!(state.migrate_data_dir(&target).unwrap_err().code, "SERVICES_STOP_FAILED");
        assert!(!target.exists());
        assert!(state.paths.data().join("run/pids.json").is_file());
    }

    #[test]
    fn service_lifecycle_reports_start_failure_after_successful_stop() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        register_fixture(&state, "nginx", "1", &temp.path().join("missing-runtime"));
        register_services(&state.paths, &state.store, &state.manager);
        state.watchdog.note_started("nginx");
        let error = state.restart_service("nginx").unwrap_err();
        assert!(error.message.contains("服务已停止，但重新启动失败"));
        assert_ne!(error.code, "UNKNOWN_SERVICE");
        let snapshot = state.manager.snapshot("nginx").unwrap();
        assert_eq!(snapshot.state, ServiceState::Error);
        assert!(snapshot.pids.is_empty());
        let cfg = crate::watchdog::WatchdogConfig { enabled: true, ..Default::default() };
        assert!(!state.watchdog.should_restart("nginx", &cfg));
        assert!(state.paths.data().join("run/pids.json").exists());
        // 已无进程的 Error 也应被主动停止抑制，批量重启失败不能重新启用看门狗。
        state.watchdog.note_started("nginx");
        let stopped = state.bulk_stop(&["nginx".into()]).unwrap();
        assert_eq!(stopped.already, ["nginx"]);
        assert!(!state.watchdog.should_restart("nginx", &cfg));
        let restarted = state.bulk_restart(&["nginx".into()]).unwrap();
        assert!(restarted.failed[0].error.message.contains("重新启动失败"));
        assert!(!state.watchdog.should_restart("nginx", &cfg));
    }

    #[test]
    fn service_lifecycle_running_start_is_idempotent_and_transitions_are_busy() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        state.manager.register("fixture", "Fixture", None, None, None, state.paths.service_log("fixture"));
        state.manager.adopt("fixture", &[std::process::id()], None);
        state.start_service_with_port_policy("fixture", |_| panic!("must not free ports")).unwrap();
        assert_eq!(state.manager.snapshot("fixture").unwrap().pids, [std::process::id()]);
        for status in [ServiceState::Starting, ServiceState::Stopping] {
            state.manager.set_state("fixture", status.clone());
            assert_eq!(state.start_service("fixture").unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(state.restart_service("fixture").unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(state.manager.snapshot("fixture").unwrap().state, status);
        }
    }

    #[test]
    fn service_lifecycle_port_policy_preserves_protected_and_disabled_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let root = temp.path().join("runtime");
        let nginx = root.join("nginx-1"); std::fs::create_dir_all(&nginx).unwrap();
        std::fs::write(nginx.join(exe_name("nginx")), b"not executed; precheck must fail first").unwrap();
        register_fixture(&state, "nginx", "1", &root);
        state.store.set_port_override("http", Some(port)).unwrap();
        state.store.set_setting("autoClosePortOnStart", "false").unwrap();
        let error = state.start_service_with_port_policy("nginx", |_| panic!("must not free ports")).unwrap_err();
        assert_eq!(error.code, "PORT_IN_USE");
        assert_eq!(error.port, Some(port));
        state.store.set_setting("autoClosePortOnStart", "true").unwrap();
        assert_eq!(state.start_service_with_port_policy("nginx", |_| panic!("must not free ports")).unwrap_err().code, "PORT_TARGET_PROTECTED");
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn service_lifecycle_real_restart_and_port_policy_update_watchdog_and_pids() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reservation.local_addr().unwrap().port(); drop(reservation);
        for id in ["fixture-a", "fixture-b"] {
            let runtime = state.paths.runtime_dir(id, "1"); std::fs::create_dir_all(&runtime).unwrap();
            let script = r#"@echo off
powershell.exe -NoProfile -NonInteractive -Command "$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, {PORT}); try { $listener.Start(); Start-Sleep -Seconds 30 } finally { $listener.Stop() }"
"#.replace("{PORT}", &port.to_string());
            std::fs::write(runtime.join("fixture.cmd"), script).unwrap();
            let manifest = serde_json::json!({"id":id,"version":"1","category":"tool","displayName":id,"description":"finite isolated fixture","os":["windows"],"arch":["x64"],"kind":"archive","url":"","sizeBytes":0,"entry":"fixture.cmd","defaultPort":port,"run":{"args":[],"health":"tcp","healthTimeoutSec":5}});
            std::fs::write(runtime.join(".niceenv-package.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
            register_fixture(&state, id, "1", &runtime);
        }
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { for id in ["fixture-a", "fixture-b"] { let _ = self.0.stop_service(id); } } }
        let _cleanup = Cleanup(&state);
        state.store.set_setting("autoClosePortOnStart", "true").unwrap();
        state.store.set_setting("autoFallbackPort", "false").unwrap();
        state.start_service("fixture-a").unwrap();
        let cfg = crate::watchdog::WatchdogConfig { enabled: true, ..Default::default() };
        assert!(state.watchdog.should_restart("fixture-a", &cfg));
        let mut freed = Vec::new();
        state.start_service_with_port_policy("fixture-b", |p| freed.push(p)).unwrap();
        assert_eq!(freed, [port]);
        assert_eq!(state.manager.snapshot("fixture-a").unwrap().state, ServiceState::Stopped);
        assert!(!state.watchdog.should_restart("fixture-a", &cfg));
        let before = state.manager.snapshot("fixture-b").unwrap().pids;
        state.restart_service("fixture-b").unwrap();
        let after = state.manager.snapshot("fixture-b").unwrap();
        assert_eq!(after.state, ServiceState::Running);
        assert_ne!(before, after.pids);
        assert!(before.iter().all(|pid| !platform::process_alive(*pid)));
        assert!(state.watchdog.should_restart("fixture-b", &cfg));
        let recorded: serde_json::Value = serde_json::from_slice(&std::fs::read(state.paths.data().join("run/pids.json")).unwrap()).unwrap();
        assert!(recorded["services"].as_array().unwrap().iter().any(|row| row["id"] == "fixture-b" && row["pids"][0] == after.pids[0]));
        let scan = state.scan_port_range(port, port).unwrap();
        let outcome = state.close_port_checked(port, &scan.listeners).unwrap();
        assert!(outcome.port_free);
        assert!(!state.watchdog.should_restart("fixture-b", &cfg));

        // 同一真实临时实例覆盖整栈、批量和托盘全部停止所用的核心入口。
        let stack = state.save_stack(crate::model::StackInput {
            id: None, name: "Lifecycle fixture".into(), description: String::new(),
            items: vec![crate::model::StackItem { service_id: "fixture-b".into(), label: None, order: 0 }],
        }).unwrap();
        assert_eq!(state.start_stack(&stack.id).unwrap().started, ["fixture-b"]);
        let stack_pid = state.manager.snapshot("fixture-b").unwrap().pids;
        assert_eq!(state.start_stack(&stack.id).unwrap().already_running, ["fixture-b"]);
        assert_eq!(state.manager.snapshot("fixture-b").unwrap().pids, stack_pid);
        assert!(state.watchdog.should_restart("fixture-b", &cfg));
        assert_eq!(state.stop_stack(&stack.id).unwrap().started, ["fixture-b"]);
        assert!(!state.watchdog.should_restart("fixture-b", &cfg));
        let report = state.bulk_start(&["fixture-b".into(), "missing".into(), "fixture-b".into()]).unwrap();
        assert_eq!(report.succeeded, ["fixture-b"], "{report:?}");
        assert_eq!(report.failed[0].service_id, "missing");
        assert!(state.watchdog.should_restart("fixture-b", &cfg));
        let before = state.manager.snapshot("fixture-b").unwrap().pids;
        let report = state.bulk_restart(&["fixture-b".into()]).unwrap();
        assert_eq!(report.succeeded, ["fixture-b"], "{report:?}");
        assert_ne!(state.manager.snapshot("fixture-b").unwrap().pids, before);
        assert!(before.iter().all(|pid| !platform::process_alive(*pid)));
        assert!(state.watchdog.should_restart("fixture-b", &cfg));
        let listeners = state.scan_port_range(port, port).unwrap().listeners;
        assert!(!listeners.is_empty());
        assert_eq!(state.stop_all_services().unwrap().succeeded, ["fixture-b"]);
        assert!(!state.watchdog.should_restart("fixture-b", &cfg));
        assert!(listeners.iter().all(|listener| !platform::process_alive(listener.pid)));
        assert!(state.scan_port_range(port, port).unwrap().listeners.is_empty());
        // 与真正启动的预检一致，验证端口可重新绑定，避免连接探针额外制造临时连接。
        assert!(std::net::TcpListener::bind(("127.0.0.1", port)).is_ok());
        state.start_service("fixture-b").unwrap();
        let process = state.manager.snapshot("fixture-b").unwrap().pids;
        let error = state.with_stopped_services(|| -> Result<()> {
            assert!(process.iter().all(|pid| !platform::process_alive(*pid)));
            assert!(state.manager.snapshot("fixture-b").unwrap().pids.is_empty());
            Err(AppError::new("LAUNCH_FAILED", "isolated launch failure"))
        }).unwrap_err();
        assert_eq!(error.code, "LAUNCH_FAILED");
        // 后续动作失败不会留下生命周期锁，可重新操作服务。
        state.start_service("fixture-b").unwrap();
        state.stop_service("fixture-b").unwrap();
    }

    #[test]
    fn service_lifecycle_transition_blocks_tasks_and_pidfile_write_failure() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        let task = state.downloader.begin_task("isolated-install").unwrap();
        assert_eq!(state.with_stopped_services(|| -> Result<()> { panic!("must not interrupt install"); }).unwrap_err().code, "PACKAGE_BUSY");
        drop(task);
        let run = state.paths.data().join("run");
        std::fs::create_dir_all(run.join("pids.json")).unwrap();
        let marker = run.join("pids.json/keep.txt");
        std::fs::write(&marker, "preserve").unwrap();
        assert_eq!(state.with_stopped_services(|| -> Result<()> { panic!("must not ignore persistence failure"); }).unwrap_err().code, "IO_ERROR");
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "preserve");
    }

    #[test]
    fn service_lifecycle_prepared_migration_blocks_work_until_guard_is_released() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().join("source")));
        let target = temp.path().join("target");
        let busy = crate::paths::DataDirActivity::shared(&state.paths.base).unwrap();
        assert!(matches!(state.prepare_data_dir_migration(&target),Err(error) if error.code == "DATA_DIR_BUSY"));
        assert!(!target.exists());
        drop(busy);
        let (result, pending) = state.prepare_data_dir_migration(&target).unwrap();
        assert!(std::path::Path::new(&result.path).join("nsb.sqlite").is_file());
        assert_eq!(state.start_service("unknown").unwrap_err().code,"DATA_DIR_BUSY");
        assert_eq!(state.stop_all_services().unwrap_err().code,"DATA_DIR_BUSY");
        assert_eq!(crate::backup_job::run_backup_now(&state.store,&state.paths).unwrap_err().code,"DATA_DIR_BUSY");
        assert_eq!(crate::cron::run_job(&state.store,"unknown",true).unwrap_err().code,"DATA_DIR_BUSY");
        drop(pending);
        assert_eq!(state.start_service("unknown").unwrap_err().code,"UNKNOWN_SERVICE");
        assert!(state.stop_all_services().is_ok());
        assert!(target.join("nsb.sqlite").is_file());
    }

    #[test]
    fn service_lifecycle_migration_fences_cached_instances_and_watchdog() {
        let temp = tempfile::tempdir().unwrap();
        let source = Paths::new(temp.path().join("source"));
        let state = isolated_state(source.clone());
        let cached = Arc::new(isolated_state(source));
        cached.store.set_setting("watchdogEnabled","true").unwrap();
        cached.manager.register("handoff-probe","Handoff probe",None,None,None,cached.paths.service_log("handoff-probe"));
        cached.watchdog.note_started("handoff-probe");
        assert!(cached.watchdog.should_restart("handoff-probe",&cached.watchdog_config()));
        let target = temp.path().join("target");
        let (_,guard) = state.prepare_data_dir_migration(&target).unwrap();
        let pidfile = std::fs::read(state.paths.data().join("run/pids.json")).unwrap();
        assert!(cached.watchdog_tick().is_empty());
        assert_eq!(cached.watchdog_status().watched[0].attempts,0);
        guard.select_with_file(&temp.path().join("selection.json"),&target,||Ok(())).unwrap();
        drop(guard);
        assert_eq!(cached.start_service("handoff-probe").unwrap_err().code,"DATA_DIR_RELOCATED");
        assert_eq!(cached.stop_all_services().unwrap_err().code,"DATA_DIR_RELOCATED");
        assert!(cached.watchdog_tick().is_empty());
        assert_eq!(cached.watchdog_status().watched[0].attempts,0);
        assert_eq!(crate::backup_job::run_backup_now(&cached.store,&cached.paths).unwrap_err().code,"DATA_DIR_RELOCATED");
        assert_eq!(crate::cron::run_job(&cached.store,"missing",true).unwrap_err().code,"DATA_DIR_RELOCATED");
        let result = crate::mcp::handle_tool_call(&cached,"list_services",&serde_json::json!({}));
        assert_eq!(result["isError"],true);
        assert!(result["content"][0]["text"].as_str().unwrap().contains("重新连接"));
        assert_eq!(std::fs::read(state.paths.data().join("run/pids.json")).unwrap(),pidfile);
        assert_eq!(cached.store.get_setting("watchdogEnabled").as_deref(),Some("true"));
    }

}
