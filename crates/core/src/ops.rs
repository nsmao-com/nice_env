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
    crate::applications::register_services(paths, store, manager);
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
    mongodb_paths_for(&inst)
}

fn mongodb_paths_for(inst: &crate::model::InstalledPackage) -> Result<(PathBuf, PathBuf)> {
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
    start_service_inner(store, paths, manager, id, true, true)
}

/// 自动恢复不能经过用户启动的计数重置路径。
pub(crate) fn start_service_for_watchdog(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    id: &str,
) -> Result<()> {
    start_service_inner(store, paths, manager, id, true, false)
}

/// 站点事务自行应用并回滚受影响的 Web 配置，PHP 启动不能提前重载其它服务。
pub(crate) fn start_php_for_site(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    version: &str,
) -> Result<()> {
    start_service_inner(store, paths, manager, &format!("php@{version}"), false, true)
}

fn start_service_inner(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    id: &str,
    reload_php_web: bool,
    user_start: bool,
) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    if manager.recovery.lock().blocked_services.iter().any(|blocked| blocked == id) {
        return Err(AppError::new("PROCESS_RECOVERY_UNVERIFIED", "历史进程尚未确认，暂不能启动此服务，请先重新检查服务接管状态"));
    }
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
            if user_start { manager.watchdog.note_started(id); }
            return Ok(());
        }
    }
    crate::generic::ensure_dependencies(store, id)?;
    if !id.contains('@') && crate::applications::site_id(id).is_none() {
        if let Some(version) = status.version {
            set_active_version(store, id, &version)?;
        }
    }
    let ports = PortsProfile::from_settings(store);
    manager.set_state(id, ServiceState::Starting);

    let result = match id {
        s if crate::applications::site_id(s).is_some() => crate::applications::spawn(store, paths, manager, s),
        "nginx" => start_nginx(store, paths, manager, &ports),
        "apache" => start_apache(store, paths, manager, &ports),
        "redis" => start_redis(store, paths, manager, &ports),
        "mihomo" => start_mihomo(store, paths, manager),
        "postgresql" => start_postgresql(store, paths, manager, &ports),
        "mongodb" => start_mongodb(store, paths, manager, &ports),
        s if s.starts_with("php@") => {
            start_php(store, paths, manager, s.trim_start_matches("php@"), &ports, reload_php_web)
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
                "postgresql" => Some(effective_ports.postgres),
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
            if !manager.snapshot(id).is_some_and(|status| status.state == ServiceState::Running && !status.pids.is_empty()) {
                let error = AppError::new("SERVICE_START_EXITED", format!("{id} 启动后进程已退出，请检查日志"));
                manager.set_error(id, error.clone());
                return Err(error);
            }
            if user_start { manager.watchdog.note_started(id); }
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
    reload_web: bool,
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
    if !reload_web { return Ok(()); }
    // 单独启动 PHP 池时重建并加载正在运行的 Web 服务配置，确保新版本立即可被站点使用。
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
    let datadir = crate::paths::checked_data_path(&paths.base, &format!("data/postgresql/{version}"))?;
    let needs_init = match std::fs::read_dir(&datadir) {
        Ok(mut entries) => entries.next().transpose()?.is_none(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(AppError::io("读取 PostgreSQL 数据目录", error)),
    };
    if !needs_init { verify_postgres_data_version(&datadir, &version)?; }
    let port = crate::services::fallback_port_for(store, "postgres", ports.postgres, &[]).unwrap_or(ports.postgres);
    precheck_port(port, "PostgreSQL")?;
    if needs_init {
        use std::io::Write;
        let parent = datadir.parent().ok_or_else(|| AppError::new("PG_INIT_FAILED", "数据目录无效"))?;
        std::fs::create_dir_all(parent)?;
        let pending = tempfile::Builder::new().prefix(".postgres-init-").tempdir_in(parent)?;
        let private = tempfile::Builder::new().prefix("niceenv-initdb-").tempdir()?;
        let password = crate::dbadmin::random_database_password();
        let passfile = private.path().join("password");
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&passfile)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        writeln!(file, "{password}")?; file.sync_all()?; drop(file);
        // 初始化前保存凭据，异常退出后仍可连接；密码不进入命令行或日志。
        store.set_setting(&crate::dbadmin::postgres_password_key(&version), &password)?;
        let mut output = tempfile::tempfile()?;
        let mut command = platform::command(root.join("bin").join(exe_name("initdb")));
        command.arg("-D").arg(pending.path()).args(["-U", "postgres", "-A", "scram-sha-256", "-E", "UTF8", "--no-locale"])
            .arg("--pwfile").arg(&passfile).current_dir(&root)
            .stdin(std::process::Stdio::null()).stdout(output.try_clone()?).stderr(output.try_clone()?);
        let status = crate::dbadmin::wait_client(&mut command, Duration::from_secs(180), || {})?;
        if !status.success() {
            return Err(AppError::new("PG_INIT_FAILED", "PostgreSQL 初始化失败，原数据目录未覆盖")
                .with_detail(crate::dbadmin::read_output(&mut output, 64 * 1024)?.replace(&password, "***")));
        }
        verify_postgres_data_version(pending.path(), &version)?;
        crate::paths::checked_data_path(&paths.base, &format!("data/postgresql/{version}"))?;
        if datadir.try_exists()? { std::fs::remove_dir(&datadir)?; } // 只允许替换空目录。
        std::fs::rename(pending.path(), &datadir)?;
    }

    precheck_port(port, "PostgreSQL")?;
    let postgres = root.join("bin").join(exe_name("postgres"));
    #[allow(unused_mut)]
    let mut pg_args: Vec<String> = vec![
        "-D".into(),
        datadir.to_string_lossy().to_string(),
        "-p".into(),
        port.to_string(),
        "-c".into(),
        "listen_addresses=127.0.0.1".into(),
    ];
    // unix socket 目录只在类 Unix 有意义；Windows 仅 TCP
    #[cfg(not(windows))]
    {
        std::fs::create_dir_all(paths.apache_run_dir())?;
        pg_args.extend(["-c".into(), format!("unix_socket_directories={}", paths.apache_run_dir().to_string_lossy())]);
    }
    let spec = SpawnSpec {
        program: postgres.clone(),
        args: pg_args,
        cwd: Some(root.clone()),
        env: vec![],
        detached: None,
    };
    spawn_tracked(manager, "postgresql", &spec)?;
    manager.set_started_port("postgresql", port);
    // 回落端口可能位于系统临时端口范围；先等真实监听者，避免主动探测占用待绑定端口。
    let waiting = std::time::Instant::now();
    loop {
        match verify_database_listener(manager, "postgresql", port) {
            Ok(()) => break,
            Err(error) => {
                if waiting.elapsed() >= Duration::from_secs(20)
                    || manager.snapshot("postgresql").is_none_or(|service| service.pids.is_empty()) {
                    return Err(error.with_hint("PostgreSQL 未在预期端口建立可核实的监听，请检查实例日志。")
                        .with_detail(manager.tail("postgresql", 40).join("\n")));
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    let client = crate::dbadmin::PostgresClient {
        exe: root.join("bin").join(exe_name("psql")), port,
        password: store.get_setting_checked(&crate::dbadmin::postgres_password_key(&version))?.unwrap_or_default(),
    };
    if let Err(error) = client.verify_data_dir(&datadir) {
        if needs_init || error.code != "POSTGRES_CONNECTION_FAILED" { return Err(error); }
        // 旧实例保留运行，允许在数据库页验证并恢复连接凭据。
        manager.push_log("postgresql", "PostgreSQL 已启动；保存的凭据无法连接，请在数据库页更新 postgres 密码。");
    }
    Ok(())
}

fn verify_postgres_data_version(data: &Path, version: &str) -> Result<()> {
    let expected = if version.starts_with("9.") { version.split('.').take(2).collect::<Vec<_>>().join(".") }
        else { version.split('.').next().unwrap_or_default().to_string() };
    let actual = std::fs::read_to_string(data.join("PG_VERSION")).map_err(|_| AppError::new("POSTGRES_DATA_INVALID", "PostgreSQL 数据目录非空但缺少可读的 PG_VERSION，未重新初始化"))?;
    if expected.is_empty() || actual.trim() != expected {
        return Err(AppError::new("POSTGRES_DATA_VERSION", "数据目录与所选 PostgreSQL 主版本不一致，未自动升级")
            .with_hint("请用原版本导出数据，再通过新版本导入；原目录保持不变。"));
    }
    if !data.join("global/pg_control").is_file() || !data.join("base").is_dir() {
        return Err(AppError::new("POSTGRES_DATA_INVALID", "PostgreSQL 数据目录不完整，请恢复有效备份；未重新初始化"));
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
    crate::configgen::write_mongodb_conf(paths, &version)?;
    std::fs::create_dir_all(&dbpath)?;
    let logfile = paths.service_log("mongodb");
    if let Some(parent) = logfile.parent() {
        std::fs::create_dir_all(parent)?;
    }
    precheck_port(ports.mongodb, "MongoDB")?;
    let require_auth = crate::mongodb_auth::enabled(store, &version)?;
    let spec = SpawnSpec {
        program: exe.clone(),
        args: {
            let mut args = vec![
            "--config".into(),
            paths.mongo_conf(&version).to_string_lossy().to_string(),
            "--dbpath".into(),
            dbpath.to_string_lossy().to_string(),
            "--port".into(),
            ports.mongodb.to_string(),
            "--bind_ip".into(),
            "127.0.0.1".into(),
            "--logpath".into(),
            logfile.to_string_lossy().to_string(),
            "--logappend".into(),
            ];
            if require_auth {
                args.push("--auth".into());
            } else {
                // 配置编辑器允许用户保留 security.authorization；显式传 --noauth
                // 让应用托管的认证开关始终优先，避免 UI 状态与实际实例不一致。
                args.push("--noauth".into());
            }
            args
        },
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

pub(crate) fn service_stop_preview(
    manager: &Arc<ServiceManager>,
    id: &str,
) -> Result<crate::model::ServiceStopPreview> {
    use sha2::{Digest, Sha256};
    let service = manager
        .snapshot(id)
        .ok_or_else(|| AppError::new("UNKNOWN_SERVICE", "服务未注册或已卸载"))?;
    if matches!(
        service.state,
        ServiceState::Starting | ServiceState::Stopping
    ) {
        return Err(AppError::new(
            "SERVICE_BUSY",
            "服务正在切换状态，请稍后重新读取",
        ));
    }
    if manager
        .recovery
        .lock()
        .blocked_services
        .iter()
        .any(|blocked| blocked == id)
    {
        return Err(AppError::new(
            "PROCESS_RECOVERY_UNVERIFIED",
            "历史进程尚未确认，请先重新检查服务接管状态",
        ));
    }
    let entry = manager
        .services
        .lock()
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::new("UNKNOWN_SERVICE", "服务已移除"))?;
    let identities = entry.identities.lock();
    let mut processes = Vec::new();
    for pid in &service.pids {
        let identity = identities
            .get(pid)
            .filter(|identity| identity.current() == Some(true))
            .ok_or_else(|| {
                AppError::new(
                    "PROCESS_IDENTITY_UNAVAILABLE",
                    "无法确认当前服务进程，未准备强制停止",
                )
            })?;
        processes.push(identity.clone());
    }
    processes.sort_by_key(|identity| identity.pid);
    let encoded =
        serde_json::to_vec(&(id, &service.version, service.port, &processes)).map_err(|error| {
            AppError::new("PROCESS_IDENTITY_UNAVAILABLE", "无法生成进程确认信息")
                .with_detail(error.to_string())
        })?;
    let revision = hex::encode(Sha256::digest(encoded));
    Ok(crate::model::ServiceStopPreview { service, revision })
}

pub(crate) fn force_stop_service(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    id: &str,
    revision: &str,
) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    let current = service_stop_preview(manager, id)?;
    if revision.is_empty() || current.revision != revision {
        return Err(AppError::new(
            "SERVICE_TARGET_CHANGED",
            "服务进程或版本已变化，请重新读取状态并确认",
        ));
    }
    stop_service_with_mode(store, paths, manager, id, true)
}

pub fn stop_service(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    id: &str,
) -> Result<()> {
    stop_service_with_mode(store, paths, manager, id, false)
}

fn stop_service_with_mode(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>, id: &str, force: bool) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    if manager.recovery.lock().blocked_services.iter().any(|blocked| blocked == id) && !manager.is_process_busy(id) {
        return Err(AppError::new("PROCESS_RECOVERY_UNVERIFIED", "历史进程尚未确认，无法安全停止，请先检查端口占用并重新检查服务接管状态"));
    }
    if id == "coredns" { crate::dns::restore_before_stop(store)?; }
    if manager.snapshot(id).is_none() {
        manager.watchdog.forget(id);
        return Ok(());
    }
    if !manager.is_busy(id) {
        // Error/Unknown 但已无进程时也属于已停止，不再发送无目标的停机命令。
        manager.set_state(id, ServiceState::Stopped);
        manager.watchdog.note_user_stopped(id);
        return Ok(());
    }
    if let Some(entry) = manager.services.lock().get(id).cloned() {
        let identities = entry.identities.lock().clone();
        if entry.pids.lock().iter().any(|pid| identities.get(pid).is_none_or(|identity| identity.current().is_none())) {
            return Err(AppError::new("PROCESS_IDENTITY_UNAVAILABLE", "服务进程身份暂时无法确认，请检查权限后重试停止"));
        }
    }
    manager.set_state(id, ServiceState::Stopping);
    let ports = PortsProfile::from_settings(store);
    // 停机命令要打向「启动时用的端口」：用户可能在运行期切换了端口方案，
    // 不向修改后的端口发送密码或 shutdown，避免误操作其他实例。
    let mysql_port = manager.started_port_or(id, ports.mysql);
    let redis_port = manager.started_port_or(id, ports.redis);
    let result = (|| -> Result<()> {
        if force { return terminate_group(manager, id); }
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
                let service = manager.snapshot(id).ok_or_else(|| AppError::new("UNKNOWN_SERVICE", "PostgreSQL 服务已移除"))?;
                let version = service.version.as_deref().ok_or_else(|| AppError::new("POSTGRES_VERSION_UNKNOWN", "无法确认正在运行的 PostgreSQL 版本，未发送停机命令"))?;
                let installed = store.find_installed("postgresql", Some(version)).ok_or_else(|| AppError::not_installed("PostgreSQL"))?;
                let data = paths.postgres_data_dir(version);
                let pid = std::fs::read_to_string(data.join("postmaster.pid"))?.lines().next().and_then(|line| line.trim().parse::<u32>().ok())
                    .ok_or_else(|| AppError::new("POSTGRES_PID_UNVERIFIED", "PostgreSQL 进程记录无法识别，未发送停机命令"))?;
                let entry = manager.services.lock().get(id).cloned().ok_or_else(|| AppError::new("UNKNOWN_SERVICE", "PostgreSQL 服务已移除"))?;
                if !entry.identities.lock().get(&pid).is_some_and(|identity| identity.current() == Some(true)) {
                    return Err(AppError::new("POSTGRES_PID_UNVERIFIED", "数据目录中的 PostgreSQL 进程与当前服务不一致，未发送停机命令"));
                }
                let mut command = platform::command(PathBuf::from(installed.install_path).join("pgsql/bin").join(exe_name("pg_ctl")));
                command.arg("-D").arg(data).args(["-m", "fast", "-w", "-t", "10", "stop"]);
                run_database_stop(&mut command, "PostgreSQL", None)?;
                wait_database_stopped(manager, id, "PostgreSQL")
            }
            "mongodb" => {
                stop_mongodb(store, manager)?;
                wait_database_stopped(manager, id, "MongoDB")
            }
            "redis" => {
                if let Some(service) = manager.snapshot(id) {
                    let version = service.version.as_deref().ok_or_else(|| AppError::new("REDIS_VERSION_UNKNOWN", "无法确认 Redis 版本，未发送停机命令"))?;
                    let credentials = crate::stats::RedisCredentials::load(store, version)?;
                    crate::stats::redis_shutdown(redis_port, &credentials, &service.pids)?;
                    wait_database_stopped(manager, id, "Redis")?;
                }
                Ok(())
            }
            "mariadb" => {
                use crate::dbadmin::DatabaseEngine;
                let service = manager.snapshot(id).ok_or_else(|| AppError::new("UNKNOWN_SERVICE", "MariaDB 服务已移除"))?;
                let version = service.version.as_deref().ok_or_else(|| AppError::new("DATABASE_VERSION_UNKNOWN", "无法确认运行中的 MariaDB 版本"))?;
                let package = store.find_installed("mariadb", Some(version)).ok_or_else(|| AppError::not_installed("MariaDB"))?;
                let port = service.port.ok_or_else(|| AppError::new("DATABASE_PORT_UNKNOWN", "无法确认 MariaDB 实际端口"))?;
                verify_database_listener(manager, id, port)?;
                let pass = DatabaseEngine::Mariadb.saved_password(store, version).ok_or_else(|| AppError::new("MYSQL_AUTH_REQUIRED", "请先在数据库页更新 MariaDB 的 root 连接密码"))?;
                let admin = crate::dbadmin::database_tool(&DatabaseEngine::Mariadb.bin_dir(&package)?, DatabaseEngine::Mariadb, "mysqladmin");
                let (_private, mut command) = crate::dbadmin::client_command(&admin, "127.0.0.1", port, "root", &pass)?;
                command.args(["--connect-timeout=5", "shutdown"]);
                run_database_stop(&mut command, "MariaDB", Some(&pass))?;
                wait_database_stopped(manager, id, "MariaDB")
            }
            s if s.starts_with("mysql@") => {
                let version = s.trim_start_matches("mysql@");
                let (basedir, _) = mysql_paths(store, version)?;
                verify_database_listener(manager, id, mysql_port)?;
                let admin = basedir.join("bin").join(exe_name("mysqladmin"));
                let pass = crate::dbadmin::saved_password(store, version).unwrap_or_default();
                let (_private, mut command) = crate::dbadmin::client_command(&admin, "127.0.0.1", mysql_port, "root", &pass)?;
                command.args(["--connect-timeout=5", "shutdown"]);
                run_database_stop(&mut command, "MySQL", Some(&pass))?;
                wait_database_stopped(manager, id, "MySQL")
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
        let identities = e.identities.lock().clone();
        for _ in 0..10 {
            survivors = pids
                .iter()
                .copied()
                .filter(|pid| identities.get(pid).is_none_or(|identity| identity.current() != Some(false)))
                .collect();
            if survivors.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if survivors.is_empty() {
            e.pids.lock().clear();
            e.identities.lock().clear();
            *e.started_at.lock() = None;
            *e.started_port.lock() = None;
            *e.group.lock() = None;
        }
    }
    if survivors.is_empty() {
        manager.set_state(id, ServiceState::Stopped);
        manager.watchdog.note_user_stopped(id);
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

fn stop_mongodb(store: &Store, manager: &Arc<ServiceManager>) -> Result<()> {
    let service = manager
        .snapshot("mongodb")
        .ok_or_else(|| AppError::new("UNKNOWN_SERVICE", "MongoDB 服务已移除"))?;
    let version = service.version.as_deref().ok_or_else(|| {
        AppError::new(
            "MONGO_VERSION_UNKNOWN",
            "无法确认正在运行的 MongoDB 版本，未发送停机请求",
        )
    })?;
    let installed = store
        .find_installed("mongodb", Some(version))
        .ok_or_else(|| AppError::not_installed("MongoDB"))?;
    let (_, executable) = mongodb_paths_for(&installed)?;
    let expected = executable.canonicalize()?;
    let entry = manager
        .services
        .lock()
        .get("mongodb")
        .cloned()
        .ok_or_else(|| AppError::new("UNKNOWN_SERVICE", "MongoDB 服务已移除"))?;
    let identities = entry.identities.lock().clone();
    let mut targets = Vec::new();
    for pid in &service.pids {
        let identity = identities
            .get(pid)
            .filter(|identity| {
                identity.current() == Some(true)
                    && identity
                        .executable
                        .canonicalize()
                        .is_ok_and(|path| path == expected)
            })
            .ok_or_else(|| {
                AppError::new(
                    "MONGO_PROCESS_UNVERIFIED",
                    "MongoDB 进程与正在运行的套件不一致，未发送停机请求",
                )
            })?;
        if let Some(process) =
            platform::VerifiedProcess::open(*pid, &identity.started).map_err(|error| {
                AppError::new(
                    "PROCESS_IDENTITY_UNAVAILABLE",
                    "无法核实 MongoDB 进程或取得操作权限",
                )
                .with_detail(error.to_string())
            })?
        {
            targets.push(process);
        }
    }
    for process in targets {
        process.request_mongodb_shutdown().map_err(|error| {
            AppError::new(
                "DATABASE_SHUTDOWN_FAILED",
                "MongoDB 正常停机请求失败，未强制结束进程",
            )
            .with_hint("请检查服务日志和进程权限后重试；必要时在服务诊断中确认强制停止")
            .with_detail(error.to_string())
        })?;
    }
    Ok(())
}

pub(crate) fn verify_database_listener(manager: &Arc<ServiceManager>, id: &str, port: u16) -> Result<()> {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let endpoints = crate::ports::listener_endpoints()?;
    let listeners: Vec<_> = endpoints
        .iter()
        .filter(|endpoint| endpoint.accepts(address))
        .collect();
    let scan = crate::ports::scan_port_range(manager, port, port)?;
    if listeners.is_empty()
        || listeners.iter().any(|listener| {
            !scan.listeners.iter().any(|row| {
                row.pid == listener.pid
                    && row.service_id.as_deref() == Some(id)
                    && row.process_start_marker.as_deref().is_some_and(|marker| {
                        platform::process_start_marker(row.pid).as_deref() == Some(marker)
                    })
            })
        })
    {
        return Err(AppError::new(
            "DATABASE_OWNER_UNVERIFIED",
            "数据库端口未确认属于当前服务，未发送密码或停机命令",
        )
        .with_hint("请检查端口占用及服务诊断，确认实际运行实例后重试")
        .with_detail(format!("目标 {id}:{port}，服务 PID {:?}，监听 PID {:?}，扫描归属 {:?}",
            manager.snapshot(id).map(|service| service.pids), listeners.iter().map(|entry| entry.pid).collect::<Vec<_>>(),
            scan.listeners.iter().map(|row| (row.pid, &row.service_id, &row.process_start_marker)).collect::<Vec<_>>())));
    }
    Ok(())
}

fn run_database_stop(
    command: &mut std::process::Command,
    name: &str,
    password: Option<&str>,
) -> Result<()> {
    let mut error = tempfile::tempfile()?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(error.try_clone()?);
    let result = crate::dbadmin::wait_client(command, Duration::from_secs(15), || {});
    if result.as_ref().is_ok_and(|status| status.success()) {
        return Ok(());
    }
    let mut detail = crate::dbadmin::read_output(&mut error, 16 * 1024).unwrap_or_default();
    if let Err(error) = result {
        detail.push_str(&error.message);
    }
    if let Some(password) = password.filter(|password| !password.is_empty()) {
        detail = detail.replace(password, "***");
    }
    Err(AppError::new(
        "DATABASE_SHUTDOWN_FAILED",
        format!("{name} 未确认正常停止，未强制结束进程"),
    )
    .with_hint("请检查连接密码、权限和日志后重试；必要时在服务诊断中确认强制停止")
    .with_detail(detail))
}

fn wait_database_stopped(manager: &Arc<ServiceManager>, id: &str, name: &str) -> Result<()> {
    for _ in 0..100 {
        if manager
            .snapshot(id)
            .is_none_or(|service| service.pids.is_empty())
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(AppError::new(
        "DATABASE_SHUTDOWN_TIMEOUT",
        format!("{name} 仍在退出过程中，未强制结束进程"),
    )
    .with_hint("请等待数据保存并查看日志；必要时在服务诊断中确认强制停止"))
}

fn terminate_group(manager: &Arc<ServiceManager>, id: &str) -> Result<()> {
    if let Some(e) = manager.services.lock().get(id).cloned() {
        let identities = e.identities.lock().clone();
        let tracked: Vec<_> = e
            .pids
            .lock()
            .iter()
            .filter_map(|pid| identities.get(pid).cloned())
            .collect();
        let tree = crate::services::ProcessTree::capture(&tracked)?;
        // 终止过程中任何一个后代失败，也必须保留其身份供状态显示和下次重试。
        for identity in tree.identities() {
            e.identities.lock().insert(identity.pid, identity.clone());
            let mut pids = e.pids.lock();
            if !pids.contains(&identity.pid) {
                pids.push(identity.pid);
            }
        }
        let mut group = e.group.lock();
        if let Some(g) = group.as_mut() {
            g.retain_roots(|pid| {
                identities.get(&pid).is_some_and(|identity| {
                    match platform::process_start_marker(pid) {
                        Some(started) => started == identity.started,
                        // 已创建的 Unix 组可在组长退出后继续存在；仍存活但身份不明则不发送信号。
                        None => !platform::process_alive(pid),
                    }
                })
            });
            #[cfg(windows)]
            if g.has_job() {
                g.terminate(true)?;
            }
            #[cfg(not(windows))]
            g.terminate(true)?;
        }
        *group = None;
        tree.terminate()?;
        // Linux pidfd 可确认已退出但尚未由父进程回收的僵尸；不要再用 kill(pid, 0) 把它报为运行中。
        for identity in tree.identities() {
            e.pids.lock().retain(|pid| *pid != identity.pid);
            e.identities.lock().remove(&identity.pid);
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
    let applications: Vec<_> = manager.services.lock().keys().filter(|id| crate::applications::site_id(id).is_some()).cloned().collect();
    for id in applications { let _ = stop_service(store, paths, manager, &id); }
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcessRecord {
    id: String,
    pids: Vec<u32>,
    #[serde(default)]
    processes: Option<Vec<ProcessIdentity>>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    owner_pid: Option<u32>,
    #[serde(default)]
    owner_started: Option<String>,
    #[serde(default)]
    recorded_at: Option<u64>,
    #[serde(default)]
    started_at: Option<u64>,
    #[serde(default)]
    site_endpoints: std::collections::HashMap<String, crate::sites::SiteEndpoint>,
    #[serde(default)]
    web_target: Option<Result<ServiceWebTarget>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProcessFile {
    #[serde(default)]
    format_version: u32,
    app_pid: u32,
    #[serde(default)]
    app_started: Option<String>,
    saved_at: u64,
    services: Vec<ProcessRecord>,
}

/// 同一数据目录的 CLI 与桌面端共用文件锁，避免并发保存互相覆盖。
fn lock_pidfile(paths: &Paths) -> Result<std::fs::File> {
    let dir = paths.data().join("run");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::io("创建进程记录目录", e))?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("pids.lock"))
        .map_err(|e| AppError::io("打开进程记录锁", e))?;
    lock.lock().map_err(|e| AppError::io("锁定进程记录", e))?;
    Ok(lock)
}

fn read_pidfile(paths: &Paths) -> Result<Option<ProcessFile>> {
    let raw = match std::fs::read(paths.data().join("run/pids.json")) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::io("读取进程恢复记录", e)),
    };
    let mut file: ProcessFile = serde_json::from_slice(&raw).map_err(|e| {
        AppError::new(
            "PROCESS_RECORD_INVALID",
            "进程恢复记录格式损坏，已保留原文件，未接管或清理进程",
        )
        .with_hint("请检查数据目录 data/run/pids.json；确认残留进程后再备份并移走损坏文件。")
        .with_detail(e.to_string())
    })?;
    if file.format_version > 2 {
        return Err(AppError::new(
            "PROCESS_RECORD_NEWER",
            "进程恢复记录由较新版本生成，请使用原版本管理这些服务",
        ));
    }
    for record in &mut file.services {
        if record.recorded_at.is_none() {
            record.recorded_at = Some(file.saved_at);
        }
        if record.owner_pid.is_none() {
            record.owner_pid = Some(file.app_pid);
            record.owner_started = file.app_started.clone();
        }
    }
    Ok(Some(file))
}

fn write_pidfile(paths: &Paths, services: Vec<ProcessRecord>) -> Result<()> {
    let file = ProcessFile {
        format_version: 2,
        app_pid: std::process::id(),
        app_started: platform::process_start_marker(std::process::id()),
        saved_at: crate::services::now_ms() as u64,
        services,
    };
    let bytes = serde_json::to_vec(&file)
        .map_err(|e| AppError::internal("序列化进程记录", e.to_string()))?;
    let dir = paths.data().join("run");
    let mut pending = tempfile::NamedTempFile::new_in(&dir)
        .map_err(|e| AppError::io("创建进程记录暂存文件", e))?;
    use std::io::Write;
    pending
        .write_all(&bytes)
        .and_then(|_| pending.as_file().sync_all())
        .map_err(|e| AppError::io("写入进程记录", e))?;
    pending
        .persist(dir.join("pids.json"))
        .map_err(|e| AppError::io("保存进程记录", e.error))?;
    Ok(())
}

/// 崩溃/被强杀时不会经过 stop_all，下次会话通过创建标识确认进程归属。
pub fn save_pidfile(paths: &Paths, manager: &Arc<ServiceManager>) {
    let _ = save_pidfile_checked(paths, manager);
}

pub fn save_pidfile_checked(paths: &Paths, manager: &Arc<ServiceManager>) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    let _file_lock = lock_pidfile(paths)?;
    let previous = read_pidfile(paths)?;
    let ids: Vec<_> = manager.services.lock().keys().cloned().collect();
    let mut records = Vec::new();
    for id in ids {
        let Some(status) = manager.snapshot(&id) else {
            continue;
        };
        let Some(entry) = manager.services.lock().get(&id).cloned() else {
            continue;
        };
        let identities = entry.identities.lock().clone();
        let processes: Vec<_> = status
            .pids
            .iter()
            .filter_map(|pid| identities.get(pid).cloned())
            .collect();
        if processes.is_empty() {
            continue;
        }
        records.push(ProcessRecord {
            id,
            pids: processes.iter().map(|process| process.pid).collect(),
            processes: Some(processes),
            version: status.version,
            port: *entry.started_port.lock(),
            owner_pid: Some(std::process::id()),
            owner_started: platform::process_start_marker(std::process::id()),
            recorded_at: Some(crate::services::now_ms() as u64),
            started_at: entry.started_at.lock().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|duration| duration.as_millis() as u64),
            site_endpoints: entry.site_endpoints.lock().clone(),
            web_target: entry.web_target.lock().clone(),
        });
    }
    if let Some(pid) = crate::toolbox::adminer_pid(manager) {
        let process = ProcessIdentity::capture(pid).ok_or_else(|| {
            AppError::new(
                "PROCESS_IDENTITY_UNAVAILABLE",
                "无法核实管理台进程，未覆盖恢复记录",
            )
        })?;
        records.push(ProcessRecord {
            id: "adminer-console".into(),
            pids: vec![pid],
            processes: Some(vec![process]),
            version: None,
            port: None,
            owner_pid: Some(std::process::id()),
            owner_started: platform::process_start_marker(std::process::id()),
            recorded_at: Some(crate::services::now_ms() as u64),
            started_at: None,
            site_endpoints: Default::default(),
            web_target: None,
        });
    }
    // 保留其他实例或尚未接管的记录。已确认死亡/复用的 PID 才可淘汰。
    if let Some(previous) = previous {
        for mut record in previous.services {
            if let Some(processes) = record.processes.as_mut() {
                let unidentified: Vec<_> = record
                    .pids
                    .iter()
                    .copied()
                    .filter(|pid| {
                        platform::process_alive(*pid)
                            && !processes.iter().any(|process| process.pid == *pid)
                            && !records.iter().any(|known| known.pids.contains(pid))
                    })
                    .collect();
                processes.retain(|process| {
                    process.current() != Some(false)
                        && !records.iter().any(|known| {
                            known.processes.as_ref().is_some_and(|list| {
                                list.iter().any(|item| {
                                    item.pid == process.pid && item.started == process.started
                                })
                            })
                        })
                });
                record.pids = processes.iter().map(|process| process.pid).collect();
                record.pids.extend(unidentified);
            } else {
                record.pids.retain(|pid| {
                    platform::process_alive(*pid)
                        && !records.iter().any(|known| known.pids.contains(pid))
                });
            }
            if !record.pids.is_empty() {
                records.push(record);
            }
        }
    }
    write_pidfile(paths, records)
}

#[derive(Default, Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanReport {
    pub adopted: Vec<(String, u32)>,
    pub killed: Vec<(String, u32)>,
    pub unresolved: Vec<String>,
    pub blocked_services: Vec<String>,
}

fn record_owner_alive(record: &ProcessRecord) -> bool {
    let Some(pid) = record.owner_pid else {
        return false;
    };
    if !platform::process_alive(pid) {
        return false;
    }
    match &record.owner_started {
        Some(expected) => {
            platform::process_start_marker(pid).is_none_or(|actual| actual == *expected)
        }
        None => true, // 旧记录无法排除并发实例，保留原有保护。
    }
}

/// 旧版本没有创建标识，只接受 runtimes 内、创建时间不晚于记录的进程。
fn legacy_process(pid: u32, saved_at: u64, paths: &Paths) -> Option<ProcessIdentity> {
    let identity = ProcessIdentity::capture(pid)?;
    let root = std::fs::canonicalize(paths.runtimes()).ok()?;
    let executable = std::fs::canonicalize(&identity.executable).ok()?;
    if !executable.starts_with(root) {
        return None;
    }
    let mut system = sysinfo::System::new();
    system.refresh_processes(
        sysinfo::ProcessesToUpdate::Some(&[sysinfo::Pid::from_u32(pid)]),
        true,
    );
    let started = system.process(sysinfo::Pid::from_u32(pid))?.start_time();
    (started > 0 && started <= saved_at / 1000 && identity.current() == Some(true))
        .then_some(identity)
}

/// 只接管已确认的原进程；外部 Go 临时可执行文件也由出生标识确认。
/// 成功后保留并转交恢复记录，CLI 只查一次 status 再退出也不会丢掉服务。
pub fn sweep_orphans(paths: &Paths, store: &Store, manager: &Arc<ServiceManager>) -> OrphanReport {
    let _operation = manager.lifecycle.lock();
    let mut report = OrphanReport::default();
    let mut restore = || -> Result<()> {
        let _file_lock = lock_pidfile(paths)?;
        let Some(mut file) = read_pidfile(paths)? else {
            return Ok(());
        };
        let installed = store.list_installed()?;
        for record in &mut file.services {
            if record_owner_alive(record) {
                continue;
            }
            if manager.is_process_busy(&record.id) {
                let tracked = manager
                    .services
                    .lock()
                    .get(&record.id)
                    .map(|entry| entry.identities.lock().clone())
                    .unwrap_or_default();
                if record
                    .pids
                    .iter()
                    .any(|pid| platform::process_alive(*pid) && !tracked.contains_key(pid))
                {
                    report.unresolved.push(format!(
                        "{} 还有未接管的历史进程，当前实例保持运行，请先处理历史进程",
                        record.id
                    ));
                }
                continue;
            }
            let mut verified = Vec::new();
            let mut unknown = false;
            for pid in &record.pids {
                if !platform::process_alive(*pid) {
                    continue;
                }
                let identity = match &record.processes {
                    Some(processes) => processes
                        .iter()
                        .find(|process| process.pid == *pid)
                        .cloned(),
                    None => {
                        legacy_process(*pid, record.recorded_at.unwrap_or(file.saved_at), paths)
                    }
                };
                match identity {
                    Some(identity) if identity.current() == Some(true) => verified.push(identity),
                    Some(identity) if identity.current() == Some(false) => {} // PID 已复用，不能认领。
                    _ => unknown = true,
                }
            }
            if unknown {
                let error = AppError::new(
                    "PROCESS_RECOVERY_UNVERIFIED",
                    format!("{} 的部分历史进程身份无法确认，已保留记录", record.id),
                )
                .with_hint("请检查服务日志和端口占用，确认这些进程后再操作，未自动结束未知进程。");
                manager.set_error(&record.id, error.clone());
                report.unresolved.push(error.message);
                report.blocked_services.push(record.id.clone());
                continue;
            }
            manager
                .recovery
                .lock()
                .blocked_services
                .retain(|id| id != &record.id);
            if verified.is_empty() {
                if let Some(entry) = manager.services.lock().get(&record.id).cloned() {
                    let mut error = entry.last_error.lock();
                    if error.as_ref().is_some_and(|error| {
                        matches!(
                            error.code.as_str(),
                            "PROCESS_RECOVERY_UNVERIFIED" | "PROCESS_VERSION_UNKNOWN"
                        )
                    }) {
                        *error = None;
                        *entry.state.lock() = ServiceState::Stopped;
                    }
                }
                record.pids.clear();
                record.processes = Some(Vec::new());
                continue;
            }
            if let Some(status) = manager.snapshot(&record.id) {
                // 按记录显示实际运行版本，不能将旧实例冒充当前默认版本。
                let version = record.version.clone().or_else(|| {
                    installed
                        .iter()
                        .filter(|package| {
                            let base = record.id.split('@').next().unwrap_or(&record.id);
                            package.id == base
                                && verified.iter().any(|process| {
                                    std::fs::canonicalize(&process.executable)
                                        .ok()
                                        .zip(
                                            std::fs::canonicalize(if package.id == "nginx" {
                                                PathBuf::from(&package.install_path)
                                                    .join(format!("nginx-{}", package.version))
                                            } else {
                                                PathBuf::from(&package.install_path)
                                            })
                                            .ok(),
                                        )
                                        .is_some_and(|(exe, root)| exe.starts_with(root))
                                })
                        })
                        .max_by_key(|package| package.install_path.len())
                        .map(|package| package.version.clone())
                });
                if version.is_none() && record.processes.is_none() {
                    let error = AppError::new(
                        "PROCESS_VERSION_UNKNOWN",
                        format!("{} 的旧记录无法确定运行版本，已保留进程", record.id),
                    );
                    manager.set_error(&record.id, error.clone());
                    report.unresolved.push(error.message);
                    report.blocked_services.push(record.id.clone());
                    continue;
                }
                if version != status.version {
                    let entry = manager.services.lock().get(&record.id).cloned().unwrap();
                    manager.register(
                        &record.id,
                        &status.label,
                        version.clone(),
                        status.category,
                        record.port,
                        entry.log_file.clone(),
                    );
                }
                manager.adopt_identified(
                    &record.id,
                    &verified,
                    record.port.filter(|port| *port > 0),
                );
                if let Some(entry) = manager.services.lock().get(&record.id).cloned() {
                    *entry.site_endpoints.lock() = record.site_endpoints.clone();
                    *entry.web_target.lock() = record.web_target.clone();
                    if let Some(started) = record.started_at.and_then(|millis| std::time::UNIX_EPOCH.checked_add(Duration::from_millis(millis))) {
                        *entry.started_at.lock() = Some(started);
                    }
                }
                let adopted = manager
                    .snapshot(&record.id)
                    .map(|status| status.pids)
                    .unwrap_or_default();
                report
                    .adopted
                    .extend(adopted.iter().map(|pid| (record.id.clone(), *pid)));
                record.pids = adopted;
                record.processes = Some(verified);
                record.version = version;
                record.owner_pid = Some(std::process::id());
                record.owner_started = platform::process_start_marker(std::process::id());
            } else {
                // 已删除的服务只清理确认过的进程，并核对结果后才报告成功。
                for process in &verified {
                    if process.current() != Some(true) {
                        continue;
                    }
                    if let Err(error) = crate::services::terminate_processes(std::slice::from_ref(process)) {
                        report
                            .unresolved
                            .push(format!("{}：{}", record.id, error.message));
                    } else {
                        let deadline = std::time::Instant::now() + Duration::from_secs(2);
                        while process.current() == Some(true)
                            && std::time::Instant::now() < deadline
                        {
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        if process.current() == Some(false) {
                            report.killed.push((record.id.clone(), process.pid));
                        } else {
                            report
                                .unresolved
                                .push(format!("{} 的进程 {} 尚未结束", record.id, process.pid));
                        }
                    }
                }
                verified.retain(|process| process.current() != Some(false));
                record.pids = verified.iter().map(|process| process.pid).collect();
                record.processes = Some(verified);
            }
        }
        file.services.retain(|record| !record.pids.is_empty());
        write_pidfile(paths, file.services)
    };
    if let Err(error) = restore() {
        report.unresolved.push(error.message);
        // 记录整体不可读时无法排除仍在运行的旧实例，不能让新启动绕过接管检查。
        report
            .blocked_services
            .extend(manager.services.lock().keys().cloned());
        report.blocked_services.sort();
        report.blocked_services.dedup();
    }
    *manager.recovery.lock() = report.clone();
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
        }
    }

    #[test]
    fn process_recovery_payload() {
        let Some(ready) = std::env::var_os("NSB_RECOVERY_PAYLOAD") else {
            return;
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std::fs::write(ready, listener.local_addr().unwrap().port().to_string()).unwrap();
        std::thread::sleep(Duration::from_secs(90)); // 有限寿命；父验收的守卫负责提前清理。
    }

    #[test]
    fn process_recovery_handoff_worker() {
        let Some(base) = std::env::var_os("NSB_RECOVERY_HANDOFF") else {
            return;
        };
        let state = isolated_state(Paths::new(base.into()));
        let manager = &state.manager;
        manager.register(
            "handoff",
            "Handoff",
            Some("2".into()),
            Some("tool".into()),
            None,
            state.paths.service_log("handoff"),
        );
        if std::env::var_os("NSB_RECOVERY_READ_ONLY").is_some() {
            let report = sweep_orphans(&state.paths, &state.store, manager);
            assert_eq!(report.adopted.len(), 1, "{report:?}");
            assert_eq!(
                manager.snapshot("handoff").unwrap().version.as_deref(),
                Some("1")
            );
            return; // 只读取状态即退出，恢复记录仍必须保留。
        }
        manager.register(
            "handoff",
            "Handoff",
            Some("1".into()),
            Some("tool".into()),
            None,
            state.paths.service_log("handoff"),
        );
        let ready = state.paths.base.join("ready-port");
        let program = if std::env::var_os("NSB_RECOVERY_LEGACY").is_some() {
            let root = state.paths.runtime_dir("handoff", "1");
            std::fs::create_dir_all(&root).unwrap();
            let program = root.join(exe_name("handoff"));
            std::fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
            register_fixture(&state, "handoff", "1", &root);
            program
        } else {
            std::env::current_exe().unwrap()
        };
        let pid = spawn_tracked(
            manager,
            "handoff",
            &SpawnSpec {
                program,
                args: vec![
                    "--exact".into(),
                    "ops::validate_tests::process_recovery_payload".into(),
                    "--nocapture".into(),
                ],
                cwd: None,
                env: vec![(
                    "NSB_RECOVERY_PAYLOAD".into(),
                    ready.to_string_lossy().into(),
                )],
                detached: Some(true),
            },
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !ready.is_file() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        let port: u16 = std::fs::read_to_string(&ready).unwrap().parse().unwrap();
        manager.set_state("handoff", ServiceState::Running);
        manager.set_started_port("handoff", port);
        manager.set_web_target("handoff", Ok(format!("http://127.0.0.1:{port}/")));
        let endpoint: crate::sites::SiteEndpoint = serde_json::from_value(serde_json::json!({"url":"http://handoff.test:8080/", "port":8080,"domains":["handoff.test"],"https":false})).unwrap();
        manager
            .services
            .lock()
            .get("handoff")
            .unwrap()
            .site_endpoints
            .lock()
            .insert("site".into(), endpoint);
        save_pidfile_checked(&state.paths, manager).unwrap();
        if std::env::var_os("NSB_RECOVERY_LEGACY").is_some() {
            let file = read_pidfile(&state.paths).unwrap().unwrap();
            let legacy = serde_json::json!({"appPid":std::process::id(), "savedAt":file.saved_at,
                    "services":[{"id":"handoff", "pids":[pid], "port":port}]});
            std::fs::write(
                state.paths.data().join("run/pids.json"),
                serde_json::to_vec(&legacy).unwrap(),
            )
            .unwrap();
        }
        assert!(platform::process_alive(pid));
    }

    #[test]
    fn process_recovery_survives_two_real_session_exits_and_preserves_version_and_endpoints() {
        for legacy in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let paths = Paths::new(temp.path().to_path_buf());
            let launch = |read_only: bool| {
                let mut command = platform::command(std::env::current_exe().unwrap());
                command
                    .args([
                        "--exact",
                        "ops::validate_tests::process_recovery_handoff_worker",
                        "--nocapture",
                    ])
                    .env("NSB_RECOVERY_HANDOFF", &paths.base);
                if read_only {
                    command.env("NSB_RECOVERY_READ_ONLY", "1");
                }
                if legacy {
                    command.env("NSB_RECOVERY_LEGACY", "1");
                }
                // Windows 的分离子进程可能继承管道句柄；等待会话 PID 退出，不等待后代关闭输出管道。
                let output = tempfile::NamedTempFile::new_in(&paths.base).unwrap();
                command
                    .stdout(output.as_file().try_clone().unwrap())
                    .stderr(output.as_file().try_clone().unwrap());
                let status = command.status().unwrap();
                let text = std::fs::read_to_string(output.path()).unwrap();
                assert!(status.success(), "{text}");
                text
            };
            let launch_output = launch(false);
            let original = read_pidfile(&paths).unwrap().unwrap();
            let pid = original.services[0].pids[0];
            struct Cleanup(u32);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = crate::ports::kill_pid(self.0);
                }
            }
            let _cleanup = Cleanup(pid);
            assert!(
                platform::process_alive(pid),
                "launcher: {launch_output}; child log: {:?}",
                std::fs::read_to_string(paths.service_log("handoff"))
            );
            if !legacy {
                assert!(!original.services[0].processes.as_ref().unwrap()[0]
                    .executable
                    .starts_with(paths.runtimes()));
            }
            launch(true);
            let state = isolated_state(paths);
            state.manager.register(
                "handoff",
                "Handoff",
                Some("2".into()),
                Some("tool".into()),
                None,
                state.paths.service_log("handoff"),
            );
            let report = state.recover_processes().unwrap();
            assert_eq!(state.process_recovery_status().adopted, report.adopted);
            assert_eq!(report.adopted, [("handoff".into(), pid)], "{report:?}");
            assert!(report.unresolved.is_empty());
            let status = state.manager.snapshot("handoff").unwrap();
            assert_eq!(status.version.as_deref(), Some("1"));
            let port = status.port.unwrap();
            assert!(tcp_port_open(port));
            if !legacy {
                assert_eq!(
                    state.manager.web_target("handoff").unwrap().url,
                    format!("http://127.0.0.1:{port}/")
                );
                assert!(state
                    .manager
                    .services
                    .lock()
                    .get("handoff")
                    .unwrap()
                    .site_endpoints
                    .lock()
                    .contains_key("site"));
            }
            assert!(state
                .watchdog_status()
                .watched
                .iter()
                .any(|entry| entry.id == "handoff" && entry.enabled));
            state.stop_service("handoff").unwrap();
            assert!(!platform::process_alive(pid));
            assert!(read_pidfile(&state.paths)
                .unwrap()
                .unwrap()
                .services
                .is_empty());
        }
    }

    #[test]
    fn process_recovery_rejects_reused_pid_and_preserves_live_owner_and_corrupt_records() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        state.manager.register(
            "probe",
            "Probe",
            Some("1".into()),
            None,
            None,
            state.paths.service_log("probe"),
        );
        state.manager.adopt("probe", &[std::process::id()], None);
        save_pidfile_checked(&state.paths, &state.manager).unwrap();
        let original = read_pidfile(&state.paths).unwrap().unwrap();
        let other = Arc::new(ServiceManager::new());
        other.register(
            "probe",
            "Probe",
            Some("2".into()),
            None,
            None,
            state.paths.service_log("probe"),
        );
        assert!(sweep_orphans(&state.paths, &state.store, &other)
            .adopted
            .is_empty());
        save_pidfile_checked(&state.paths, &other).unwrap();
        assert_eq!(
            read_pidfile(&state.paths).unwrap().unwrap().services.len(),
            1,
            "另一个实例不能覆盖存活写入者的记录"
        );
        let mut wrong = original.services.clone();
        wrong[0].owner_started = Some("previous-writer".into());
        wrong[0].processes.as_mut().unwrap()[0].started = "reused-pid".into();
        write_pidfile(&state.paths, wrong).unwrap();
        let report = sweep_orphans(&state.paths, &state.store, &other);
        assert!(report.adopted.is_empty() && report.killed.is_empty());
        assert!(platform::process_alive(std::process::id()));
        assert!(read_pidfile(&state.paths)
            .unwrap()
            .unwrap()
            .services
            .is_empty());
        let mut unknown = original.services;
        unknown[0].owner_started = Some("previous-writer".into());
        unknown[0].processes = Some(Vec::new());
        write_pidfile(&state.paths, unknown).unwrap();
        let report = sweep_orphans(&state.paths, &state.store, &other);
        assert_eq!(report.unresolved.len(), 1);
        assert_eq!(
            start_service(&state.store, &state.paths, &other, "probe")
                .unwrap_err()
                .code,
            "PROCESS_RECOVERY_UNVERIFIED"
        );
        assert_eq!(
            stop_service(&state.store, &state.paths, &other, "probe")
                .unwrap_err()
                .code,
            "PROCESS_RECOVERY_UNVERIFIED"
        );
        assert!(other.is_busy("probe"));
        save_pidfile_checked(&state.paths, &other).unwrap();
        assert_eq!(
            read_pidfile(&state.paths).unwrap().unwrap().services[0].pids,
            [std::process::id()]
        );
        let path = state.paths.data().join("run/pids.json");
        std::fs::write(&path, b"damaged record").unwrap();
        assert!(!sweep_orphans(&state.paths, &state.store, &other)
            .unresolved
            .is_empty());
        assert_eq!(
            save_pidfile_checked(&state.paths, &other).unwrap_err().code,
            "PROCESS_RECORD_INVALID"
        );
        assert_eq!(std::fs::read(path).unwrap(), b"damaged record");
    }

    #[test]
    fn process_recovery_snapshot_does_not_recapture_a_reused_pid() {
        let manager = ServiceManager::new();
        let temp = tempfile::tempdir().unwrap();
        manager.register(
            "fixture",
            "Fixture",
            None,
            None,
            None,
            temp.path().join("service.log"),
        );
        manager.adopt("fixture", &[std::process::id()], None);
        manager
            .services
            .lock()
            .get("fixture")
            .unwrap()
            .identities
            .lock()
            .get_mut(&std::process::id())
            .unwrap()
            .started = "old-instance".into();
        assert!(!manager.is_busy("fixture"));
        let status = manager.snapshot("fixture").unwrap();
        assert_eq!(status.state, ServiceState::Stopped);
        assert!(status.pids.is_empty());
        assert!(platform::process_alive(std::process::id()));
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

    fn validate_native_database_user_deletion(state: &crate::CoreState, engine: crate::dbadmin::DatabaseEngine, version: &str, client: &crate::dbadmin::MySqlClient) {
        use crate::dbadmin::{self, DatabaseUserDropInput};
        let username = "retire'quote;中文";
        let account = "'retire''quote;中文'@'localhost'";
        let alternate = "'retire''quote;中文'@'127.0.0.2'";
        let read = || client.user_drop_info(username, "localhost").unwrap();
        let drop_account = |info: &dbadmin::DatabaseUserDropInfo, confirmation: &str| dbadmin::drop_database_user(state, engine, version,
            &DatabaseUserDropInput { username: info.username.clone(), host: info.host.clone(), confirmation: confirmation.into(), revision: info.revision.clone() });
        let confirmation = format!("{username}@localhost");
        client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; CREATE DATABASE delete_fixture; CREATE TABLE delete_fixture.proof (id INT); INSERT INTO delete_fixture.proof VALUES (93); CREATE USER {account} IDENTIFIED BY 'delete-fixture-password', {alternate} IDENTIFIED BY 'other-host-kept'; GRANT SELECT ON delete_fixture.* TO {account};")).unwrap();
        let before = read(); assert!(!before.protected && before.dependencies.is_empty());
        assert_eq!(drop_account(&before, username).unwrap_err().code, "DB_USER_CONFIRM");
        let root = client.user_drop_info("root", "localhost").unwrap(); assert!(root.protected);
        assert_eq!(drop_account(&root, "root@localhost").unwrap_err().code, "SYSTEM_ACCOUNT");
        client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; GRANT CREATE USER ON *.* TO {account};")).unwrap();
        assert!(read().protected);
        assert_eq!(drop_account(&read(), &confirmation).unwrap_err().code, "SYSTEM_ACCOUNT");
        client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; REVOKE CREATE USER ON *.* FROM {account}; GRANT INSERT ON delete_fixture.proof TO {account};")).unwrap();
        assert_eq!(drop_account(&before, &confirmation).unwrap_err().code, "DB_USER_CHANGED");
        let without_dependencies = read();
        client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; CREATE DEFINER={account} VIEW delete_fixture.dependent_view AS SELECT id FROM delete_fixture.proof; CREATE DEFINER={account} PROCEDURE delete_fixture.dependent_procedure() SELECT 93; CREATE DEFINER={account} TRIGGER delete_fixture.dependent_trigger BEFORE INSERT ON delete_fixture.proof FOR EACH ROW SET NEW.id=NEW.id; CREATE DEFINER={account} EVENT delete_fixture.dependent_event ON SCHEDULE EVERY 1 DAY DISABLE DO SELECT 93;")).unwrap();
        let dependency = read(); assert_eq!(dependency.dependencies.len(), 4);
        for kind in ["view", "routine", "trigger", "event"] { assert!(dependency.dependencies.iter().any(|item| item.kind == kind && item.database == "delete_fixture")); }
        assert_eq!(drop_account(&without_dependencies, &confirmation).unwrap_err().code, "DB_USER_DEPENDENCIES");
        assert!(client.list_users().unwrap().iter().any(|user| user.username == username && user.host == "localhost"));
        // 仅在隔离实例中暂撤元数据权限，确认不能把不可见的依赖误判为不存在。
        let administrator = client.run("SELECT CURRENT_USER();").unwrap();
        let (admin_user, admin_host) = administrator.trim().rsplit_once('@').unwrap();
        let administrator = format!("'{}'@'{}'", admin_user.replace('\'', "''"), admin_host.replace('\'', "''"));
        client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; CREATE USER 'delete_metadata_admin'@'localhost' IDENTIFIED BY 'metadata-fixture-only'; GRANT ALL PRIVILEGES ON *.* TO 'delete_metadata_admin'@'localhost' WITH GRANT OPTION; REVOKE SHOW VIEW ON *.* FROM {administrator};")).unwrap();
        assert_eq!(client.user_drop_info(username, "localhost").unwrap_err().code, "DB_USER_METADATA_ACCESS");
        assert_eq!(drop_account(&without_dependencies, &confirmation).unwrap_err().code, "DB_USER_METADATA_ACCESS");
        dbadmin::query_client(&client.exe, "127.0.0.1", client.port, "delete_metadata_admin", "metadata-fixture-only", &format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; GRANT SHOW VIEW ON *.* TO {administrator};")).unwrap();
        client.run("DROP USER 'delete_metadata_admin'@'localhost';").unwrap();
        assert_eq!(read().dependencies.len(), 4);
        client.run("DROP VIEW delete_fixture.dependent_view; DROP PROCEDURE delete_fixture.dependent_procedure; DROP TRIGGER delete_fixture.dependent_trigger; DROP EVENT delete_fixture.dependent_event;").unwrap();
        client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; CREATE USER 'delete_proxy'@'localhost'; GRANT PROXY ON {account} TO 'delete_proxy'@'localhost';")).unwrap();
        assert_eq!(read().proxy_dependents, 1);
        assert_eq!(drop_account(&read(), &confirmation).unwrap_err().code, "DB_USER_DEPENDENCIES");
        client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; REVOKE PROXY ON {account} FROM 'delete_proxy'@'localhost';")).unwrap();
        if engine == dbadmin::DatabaseEngine::Mysql {
            client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; GRANT {account} TO 'delete_proxy'@'localhost';")).unwrap();
            assert_eq!(read().role_dependents, 1);
            assert_eq!(drop_account(&read(), &confirmation).unwrap_err().code, "DB_USER_DEPENDENCIES");
            client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; REVOKE {account} FROM 'delete_proxy'@'localhost';")).unwrap();
        }
        let alternate_revision = client.user_drop_info(username, "127.0.0.2").unwrap().revision;
        let ready = read();
        struct LiveSession(Option<std::process::Child>);
        impl Drop for LiveSession { fn drop(&mut self) { if let Some(child) = self.0.as_mut() { let _ = child.kill(); let _ = child.wait(); } } }
        let (_private, mut command) = dbadmin::client_command(&client.exe, "127.0.0.1", client.port, username, "delete-fixture-password").unwrap();
        let mut live = LiveSession(Some(command.args(["--batch", "--unbuffered", "--skip-column-names"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()));
        let mut stdin = live.0.as_mut().unwrap().stdin.take().unwrap();
        std::io::Write::write_all(&mut stdin, b"USE delete_fixture; SELECT id FROM proof;\n").unwrap(); std::io::Write::flush(&mut stdin).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop { let active = read(); if active.username_connections > 0 { assert_eq!(active.revision, ready.revision); break; } assert!(std::time::Instant::now() < deadline, "fixture connection did not open"); }
        drop_account(&ready, &confirmation).unwrap();
        assert_eq!(client.user_drop_info(username, "localhost").unwrap_err().code, "DB_USER_MISSING");
        assert!(dbadmin::query_client(&client.exe, "127.0.0.1", client.port, username, "delete-fixture-password", "SELECT 1;").is_err());
        std::io::Write::write_all(&mut stdin, b"SELECT id FROM proof;\n").unwrap(); drop(stdin);
        let existing = live.0.take().unwrap().wait_with_output().unwrap();
        assert!(existing.status.success(), "{}", String::from_utf8_lossy(&existing.stderr));
        assert_eq!(String::from_utf8_lossy(&existing.stdout).lines().filter(|line| *line == "93").count(), 2);
        assert_eq!(client.user_drop_info(username, "127.0.0.2").unwrap().revision, alternate_revision);
        assert_eq!(client.run("SELECT id FROM delete_fixture.proof;").unwrap().trim(), "93");
        assert_eq!(client.run(&format!("SELECT COUNT(*) FROM mysql.tables_priv WHERE HEX(User)='{}' AND Host='localhost';", hex::encode_upper(username))).unwrap().trim(), "0");
        client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; DROP USER {alternate}, 'delete_proxy'@'localhost'; DROP DATABASE delete_fixture;")).unwrap();
    }

    fn validate_native_database_passwords(state: &crate::CoreState, engine: crate::dbadmin::DatabaseEngine, version: &str, client: &crate::dbadmin::MySqlClient, username: &str, original_password: &str) {
        use crate::dbadmin::{self, DatabaseUserPasswordInput};
        let read = |name: &str, host: &str| client.user_password_info(name, host).unwrap();
        let save = |info: &dbadmin::DatabaseUserPasswordInfo, password: &str| dbadmin::update_database_user_password(state, engine, version,
            &DatabaseUserPasswordInput { username: info.username.clone(), host: info.host.clone(), password: password.into(), revision: info.revision.clone() });
        let query = |name: &str, password: &str| dbadmin::query_client(&client.exe, "127.0.0.1", client.port, name, password, "SELECT CURRENT_USER();");
        let before = read(username, "localhost");
        assert!(before.supported && !before.protected && !before.other_authentication);
        let original_grants = serde_json::to_string(&client.grants(username, "localhost").unwrap()).unwrap();
        let alternate = format!("'{}'@'127.0.0.2'", username.replace('\'', "''"));
        client.run(&format!("CREATE USER {alternate};")).unwrap();
        let untouched = read(username, "127.0.0.2").revision;
        assert_eq!(save(&before, "").unwrap_err().code, "BAD_PASSWORD");
        assert_eq!(save(&before, &"界".repeat(1366)).unwrap_err().code, "BAD_PASSWORD");
        assert_eq!(save(&read("root", "localhost"), "never-change-root").unwrap_err().code, "SYSTEM_ACCOUNT");
        let secret = "new'quote \\semi; 中文密码 92";
        client.run("SET GLOBAL log_output='TABLE'; SET GLOBAL general_log=ON;").unwrap();
        let saved = save(&before, secret).unwrap();
        client.run("SET GLOBAL general_log=OFF;").unwrap();
        assert_eq!(client.run(&format!("SELECT COUNT(*) FROM mysql.general_log WHERE HEX(argument) LIKE '%{}%';", hex::encode_upper(secret))).unwrap().trim(), "0");
        assert_eq!(saved.plugins, before.plugins);
        assert_ne!(saved.revision, before.revision);
        assert!(query(username, secret).unwrap().contains(username));
        assert!(query(username, original_password).is_err());
        assert_eq!(read(username, "127.0.0.2").revision, untouched);
        assert_eq!(serde_json::to_string(&client.grants(username, "localhost").unwrap()).unwrap(), original_grants);
        assert_eq!(save(&before, "outdated").unwrap_err().code, "DB_PASSWORD_CHANGED");
        assert!(!serde_json::to_string(&saved).unwrap().contains(secret));
        let account = format!("'{}'@'localhost'", username.replace('\'', "''"));
        if engine == dbadmin::DatabaseEngine::Mysql {
            // MySQL 保留的备用密码不会被 SET PASSWORD 撤销，界面必须如实提示。
            client.run(&format!("SET PASSWORD FOR {account} = 'new-primary-before-ui' RETAIN CURRENT PASSWORD;")).unwrap();
            let dual = read(username, "localhost");
            assert!(dual.other_authentication);
            save(&dual, "ui-primary-after-retain").unwrap();
            assert!(query(username, "ui-primary-after-retain").is_ok());
            assert!(query(username, secret).is_ok());
            assert!(query(username, "new-primary-before-ui").is_err());
            client.run(&format!("ALTER USER {account} DISCARD OLD PASSWORD;")).unwrap();
            assert!(query(username, secret).is_err());
        } else {
            client.run("INSTALL SONAME 'auth_ed25519'; INSTALL SONAME 'auth_named_pipe';").unwrap();
            client.run("CREATE USER 'password_ed'@'localhost' IDENTIFIED VIA ed25519 USING PASSWORD('ed-initial');").unwrap();
            let ed = read("password_ed", "localhost");
            assert_eq!(ed.target_plugin, "ed25519"); assert!(ed.supported);
            save(&ed, "ed-updated-password").unwrap();
            assert!(query("password_ed", "ed-updated-password").is_ok());
            assert!(query("password_ed", "ed-initial").is_err());
            // auth_or 的 {} 必须还原成主插件；只改变第一个支持密码的认证方式。
            client.run("ALTER USER 'password_ed'@'localhost' IDENTIFIED VIA ed25519 USING PASSWORD('ed-original') OR mysql_native_password USING PASSWORD('native-alternative');").unwrap();
            let multi = read("password_ed", "localhost");
            assert_eq!(multi.plugins, ["ed25519", "mysql_native_password"]); assert!(multi.other_authentication);
            let saved = save(&multi, "ed-new-primary").unwrap();
            assert_eq!(saved.plugins, multi.plugins);
            assert!(query("password_ed", "ed-new-primary").is_ok());
            assert!(query("password_ed", "native-alternative").is_ok());
            assert!(query("password_ed", "ed-original").is_err());
            client.run("CREATE USER 'password_pipe'@'localhost' IDENTIFIED VIA named_pipe;").unwrap();
            let pipe = read("password_pipe", "localhost"); assert!(!pipe.supported && !pipe.protected);
            assert_eq!(save(&pipe, "do-not-switch-auth").unwrap_err().code, "DB_PASSWORD_UNSUPPORTED");
            client.run("ALTER USER 'password_pipe'@'localhost' IDENTIFIED VIA named_pipe OR mysql_native_password USING PASSWORD('pipe-alternative');").unwrap();
            let mixed = read("password_pipe", "localhost");
            assert_eq!(mixed.plugins, ["named_pipe", "mysql_native_password"]); assert!(!mixed.supported && mixed.other_authentication);
            assert_eq!(save(&mixed, "do-not-change-later-password").unwrap_err().code, "DB_PASSWORD_UNSUPPORTED");
            assert_eq!(read("password_pipe", "localhost").revision, mixed.revision);
            assert!(query("password_pipe", "pipe-alternative").is_ok());
            assert!(query("password_pipe", "do-not-change-later-password").is_err());
            // 密码插件在前的组合可以改密，后面的外部认证必须完整保留。
            client.run("ALTER USER 'password_pipe'@'localhost' IDENTIFIED VIA mysql_native_password USING PASSWORD('native-first') OR named_pipe;").unwrap();
            let native_first = read("password_pipe", "localhost");
            assert_eq!(native_first.plugins, ["mysql_native_password", "named_pipe"]); assert!(native_first.supported);
            let saved = save(&native_first, "native-first-updated").unwrap();
            assert_eq!(saved.plugins, native_first.plugins);
            assert!(query("password_pipe", "native-first-updated").is_ok());
            assert!(query("password_pipe", "native-first").is_err());
            client.run("DROP USER 'password_ed'@'localhost', 'password_pipe'@'localhost'; UNINSTALL SONAME 'auth_ed25519'; UNINSTALL SONAME 'auth_named_pipe';").unwrap();
        }
        client.run(&format!("DROP USER {alternate};")).unwrap();
        assert_eq!(client.user_password_info(username, "127.0.0.2").unwrap_err().code, "DB_USER_MISSING");
    }

    fn validate_native_database_grants(state: &crate::CoreState, engine: crate::dbadmin::DatabaseEngine, version: &str, client: &crate::dbadmin::MySqlClient, username: &str, password: &str, database: &str) {
        use crate::dbadmin::{self, DatabaseGrantInput};
        let collision = database.replace('_', "X");
        client.create_database(&collision).unwrap();
        client.create_database("grant_other").unwrap();
        client.run("SHOW TABLES FROM grant_other;").unwrap();
        client.run("CREATE TABLE grant_other.sample (id INT) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;").unwrap();
        client.run("SHOW CREATE TABLE grant_other.sample;").unwrap();
        client.run("INSERT INTO grant_other.sample VALUES (7);").unwrap();
        let query = |sql: &str| dbadmin::query_client(&client.exe, "127.0.0.1", client.port, username, password, sql);
        assert!(query(&format!("USE `{collision}`; SELECT 1;")).is_err(), "新建账号不能通过下划线通配访问相似库名");
        let mut originals = Vec::new();
        for host in ["localhost", "127.0.0.1"] {
            let before = client.grants(username, host).unwrap();
            let target = before.scopes.iter().find(|scope| scope.label == database).unwrap();
            assert!(!target.pattern); assert!(!target.protected);
            originals.push((host, target.clone()));
            let add_other = DatabaseGrantInput { username: username.into(), host: host.into(), target: "grant_other".into(), new_database: true, privileges: vec!["SELECT".into()], grant_option: false, revision: before.revision.clone() };
            let added = dbadmin::update_database_grants(state, engine, version, &add_other).unwrap();
            assert_eq!(dbadmin::update_database_grants(state, engine, version, &add_other).unwrap_err().code, "DATABASE_GRANTS_CHANGED");
            let mut input = DatabaseGrantInput { target: target.scope.clone(), new_database: false, privileges: vec!["SELECT".into()], grant_option: true, revision: added.revision, ..add_other };
            let selected = dbadmin::update_database_grants(state, engine, version, &input).unwrap();
            assert!(selected.scopes.iter().find(|scope| scope.scope == input.target).unwrap().grant_option);
            assert_eq!(selected.scopes.iter().find(|scope| scope.label == "grant_other").unwrap().privileges, ["SELECT"]);
            input.revision = selected.revision;
            input.privileges = vec!["SELECT; DROP DATABASE mysql".into()];
            assert_eq!(dbadmin::update_database_grants(state, engine, version, &input).unwrap_err().code, "BAD_PRIVILEGE");
            input.privileges = vec!["SELECT".into()]; input.grant_option = false;
            dbadmin::update_database_grants(state, engine, version, &input).unwrap();
        }
        assert_eq!(query(&format!("SELECT value FROM `{database}`.sample;")).unwrap().trim(), "original");
        assert!(query(&format!("INSERT INTO `{database}`.sample VALUES (99, 'forbidden');")).is_err());
        assert_eq!(query("SELECT id FROM grant_other.sample;").unwrap().trim(), "7");
        for (host, original) in &originals {
            let before = client.grants(username, host).unwrap();
            let input = DatabaseGrantInput { username: username.into(), host: (*host).into(), target: original.scope.clone(), new_database: false, privileges: Vec::new(), grant_option: false, revision: before.revision };
            let empty = dbadmin::update_database_grants(state, engine, version, &input).unwrap();
            assert!(empty.scopes.iter().filter(|scope| scope.scope == original.scope).all(|scope| scope.privileges.is_empty() && !scope.grant_option));
        }
        assert!(query(&format!("SELECT value FROM `{database}`.sample;")).is_err());
        assert_eq!(query("SELECT id FROM grant_other.sample;").unwrap().trim(), "7");
        for (host, original) in originals {
            let before = client.grants(username, host).unwrap();
            let input = DatabaseGrantInput { username: username.into(), host: host.into(), target: database.into(), new_database: true, privileges: original.privileges, grant_option: original.grant_option, revision: before.revision };
            dbadmin::update_database_grants(state, engine, version, &input).unwrap();
        }
        let root = client.grants("root", "localhost").unwrap();
        let protected = DatabaseGrantInput { username: "root".into(), host: "localhost".into(), target: database.into(), new_database: true, privileges: vec!["SELECT".into()], grant_option: false, revision: root.revision };
        assert_eq!(dbadmin::update_database_grants(state, engine, version, &protected).unwrap_err().code, "DATABASE_GRANTS_PROTECTED");
        // 历史通配范围必须原样读取、显式编辑，不能误当作具体数据库。
        client.run(&format!("GRANT SELECT ON `{database}`.* TO '{username}'@'localhost';")).unwrap();
        let legacy = client.grants(username, "localhost").unwrap();
        assert!(legacy.scopes.iter().any(|scope| scope.scope == database && scope.pattern));
        let edit = DatabaseGrantInput { username: username.into(), host: "localhost".into(), target: database.into(), new_database: false, privileges: vec!["SELECT".into(), "INSERT".into()], grant_option: false, revision: legacy.revision };
        let saved = dbadmin::update_database_grants(state, engine, version, &edit).unwrap();
        assert_eq!(saved.scopes.iter().find(|scope| scope.scope == database).unwrap().privileges.len(), 2);
        client.run(&format!("REVOKE SELECT, INSERT ON `{database}`.* FROM '{username}'@'localhost'; GRANT SELECT ON `%`.* TO '{username}'@'localhost';")).unwrap();
        let protected_scope = client.grants(username, "localhost").unwrap();
        assert!(protected_scope.scopes.iter().find(|scope| scope.scope == "%").unwrap().protected);
        let edit = DatabaseGrantInput { target: "%".into(), revision: protected_scope.revision, ..edit };
        assert_eq!(dbadmin::update_database_grants(state, engine, version, &edit).unwrap_err().code, "SYSTEM_DATABASE");
        client.run(&format!("REVOKE SELECT ON `%`.* FROM '{username}'@'localhost';")).unwrap();
        if engine == dbadmin::DatabaseEngine::Mysql {
            let before_mode_change = client.grants(username, "localhost").unwrap();
            client.run("SET GLOBAL partial_revokes=ON;").unwrap();
            // MySQL 将既有转义范围也按字面处理：不能误报旧授权仍覆盖原库。
            assert!(query(&format!("SELECT value FROM `{database}`.sample;")).is_err());
            let changed_mode = client.grants(username, "localhost").unwrap();
            assert!(changed_mode.scopes.iter().all(|scope| !scope.pattern && scope.label == scope.scope));
            let stale = DatabaseGrantInput { username: username.into(), host: "localhost".into(), target: database.into(), new_database: true, privileges: vec!["SELECT".into()], grant_option: false, revision: before_mode_change.revision };
            assert_eq!(dbadmin::update_database_grants(state, engine, version, &stale).unwrap_err().code, "DATABASE_GRANTS_CHANGED");
            client.create_user_grant("partial_user", password, database).unwrap();
            let literal = client.grants("partial_user", "localhost").unwrap();
            assert!(literal.partial_revokes); assert!(!literal.scopes[0].pattern); assert_eq!(literal.scopes[0].scope, database);
            let edit = DatabaseGrantInput { username: "partial_user".into(), host: "localhost".into(), target: database.into(), new_database: false, privileges: vec!["SELECT".into()], grant_option: false, revision: literal.revision };
            dbadmin::update_database_grants(state, engine, version, &edit).unwrap();
            client.run("GRANT SELECT ON *.* TO 'partial_user'@'localhost';").unwrap();
            let global = client.grants("partial_user", "localhost").unwrap();
            assert!(global.global_privileges);
            assert_eq!(dbadmin::update_database_grants(state, engine, version, &DatabaseGrantInput { revision: global.revision, ..edit }).unwrap_err().code, "DATABASE_GRANTS_PROTECTED");
            client.run("DROP USER 'partial_user'@'localhost', 'partial_user'@'127.0.0.1'; SET GLOBAL partial_revokes=OFF;").unwrap();
        }
        // 带引号的既有账号、带反引号的库名须精确寻址，不能改变 SQL 结构。
        let special_user = "grant'quote";
        let special_db = "grant`quote";
        let password_sql = password.replace('\'', "''");
        client.run(&format!("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; CREATE USER 'grant''quote'@'localhost' IDENTIFIED BY '{password_sql}'; CREATE DATABASE `grant``quote` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;")).unwrap();
        client.run("SHOW TABLES FROM `grant``quote`;").unwrap();
        client.run("CREATE TABLE `grant``quote`.sample (id INT) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;").unwrap();
        client.run("SHOW CREATE TABLE `grant``quote`.sample;").unwrap();
        client.run("INSERT INTO `grant``quote`.sample VALUES (9);").unwrap();
        let current = client.grants(special_user, "localhost").unwrap();
        let input = DatabaseGrantInput { username: special_user.into(), host: "localhost".into(), target: special_db.into(), new_database: true, privileges: vec!["SELECT".into()], grant_option: false, revision: current.revision };
        dbadmin::update_database_grants(state, engine, version, &input).unwrap();
        assert_eq!(dbadmin::query_client(&client.exe, "127.0.0.1", client.port, special_user, password, "SELECT id FROM `grant``quote`.sample;").unwrap().trim(), "9");
        validate_native_database_passwords(state, engine, version, client, special_user, password);
        validate_native_database_user_deletion(state, engine, version, client);
        client.run("SET SESSION sql_mode='NO_BACKSLASH_ESCAPES'; DROP USER 'grant''quote'@'localhost'; DROP DATABASE `grant``quote`;").unwrap();
        client.drop_database(&collision).unwrap(); client.drop_database("grant_other").unwrap();
    }

    #[test]
    #[ignore = "requires NSB_MARIADB_ROOT and NSB_MYSQL_ROOT; isolated temporary databases and ephemeral ports"]
    fn real_mariadb_management_isolation_and_graceful_shutdown() {
        use crate::{dbadmin::{self, DatabaseEngine}, dbbackup, dbmigrate};
        let engine = DatabaseEngine::Mariadb;
        let root = PathBuf::from(std::env::var("NSB_MARIADB_ROOT").expect("NSB_MARIADB_ROOT"));
        let mysql = PathBuf::from(std::env::var("NSB_MYSQL_ROOT").expect("NSB_MYSQL_ROOT"));
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().join("MariaDB with spaces")));
        let before_service = platform::command("sc.exe").args(["query", "MariaDB"]).output().unwrap();
        register_fixture(&state, "mariadb", "11.4.8", root.parent().unwrap());
        register_fixture(&state, "mysql", "8.0.46", mysql.parent().unwrap());
        crate::generic::register_services(&state.paths, &state.store, &state.manager);
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                for id in ["mariadb", "mysql@8.0.46"] {
                    if self.0.stop_service(id).is_err() {
                        if let Ok(preview) = self.0.service_stop_preview(id) { let _ = self.0.force_stop_service(id, &preview.revision); }
                    }
                }
            }
        }
        let _cleanup = Cleanup(&state);
        for id in ["mariadb", "mysql"] {
            let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            state.store.set_port_override(id, Some(free.local_addr().unwrap().port())).unwrap(); drop(free);
            state.start_service(if id == "mysql" { "mysql@8.0.46" } else { id }).unwrap_or_else(|error| panic!("{error:?}\n{:?}", state.manager.tail(id, 40)));
        }
        let after_service = platform::command("sc.exe").args(["query", "MariaDB"]).output().unwrap();
        assert_eq!(before_service.status.code(), after_service.status.code());
        assert_eq!(before_service.stdout, after_service.stdout);
        let (_, mut client) = dbadmin::authenticated_client(&state, engine, Some("11.4.8"), None).unwrap();
        let (_, mysql_client) = dbadmin::selected_client(&state, Some("8.0.46")).unwrap();
        assert_ne!(client.port, mysql_client.port);
        assert_eq!(client.root_password.len(), 24);
        assert_ne!(client.root_password, mysql_client.root_password);
        assert!(state.paths.data().join("mariadb-versions/11.4.8/mysql").is_dir());
        assert!(!state.paths.data().join("mariadb/mysql").exists());
        client.create_database("mariadb_fixture").unwrap();
        assert!(client.drop_database("mysql").is_err());
        assert!(!mysql_client.list_databases().unwrap().iter().any(|db| db.name == "mariadb_fixture"));
        assert!(client.run("SHOW TABLES FROM mariadb_fixture;").unwrap().trim().is_empty());
        client.run("CREATE TABLE mariadb_fixture.sample (id INT PRIMARY KEY, value VARCHAR(80)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;").unwrap();
        assert!(client.run("SHOW CREATE TABLE mariadb_fixture.sample;").unwrap().contains("utf8mb4"));
        client.run("INSERT INTO mariadb_fixture.sample VALUES (1, 'original');").unwrap();
        let secret = "quote' slash\\ dollar$ # semi; 中文";
        state.set_database_password(engine, Some("11.4.8"), secret, false).unwrap();
        client.root_password = secret.into();
        client.create_user_grant("maria_user", secret, "mariadb_fixture").unwrap();
        dbadmin::query_client(&client.exe, "127.0.0.1", client.port, "maria_user", secret, "SELECT value FROM mariadb_fixture.sample;").unwrap();
        validate_native_database_grants(&state, engine, "11.4.8", &client, "maria_user", secret, "mariadb_fixture");
        state.store.set_setting(&engine.password_key("11.4.8"), "incorrect").unwrap();
        let pid = state.manager.snapshot("mariadb").unwrap().pids;
        assert_eq!(state.stop_service("mariadb").unwrap_err().code, "DATABASE_SHUTDOWN_FAILED");
        assert!(pid.iter().all(|pid| platform::process_alive(*pid)));
        assert!(state.restart_service("mariadb").is_err());
        state.set_database_password(engine, Some("11.4.8"), secret, true).unwrap();
        assert_eq!(state.manager.snapshot("mariadb").unwrap().state, ServiceState::Running);
        assert_eq!(mysql_client.root_password, dbadmin::saved_password(&state.store, "8.0.46").unwrap());
        let conn = dbbackup::ConnInfo { engine, version: "11.4.8".into(), port: client.port, root_password: secret.into(), bin_dir: Some(root.join("bin")) };
        let backup = dbbackup::dump_path_for(&state.paths, engine, "11.4.8", "fixture.sql").unwrap();
        dbbackup::dump_databases(&state.paths, &conn, &["mariadb_fixture".into()], &backup, &|_| {}).unwrap();
        assert!(backup.file_name().unwrap().to_string_lossy().starts_with("mariadb-"));
        client.run("UPDATE mariadb_fixture.sample SET value='changed';").unwrap();
        let safety = dbbackup::restore_from_file(&state.paths, &conn, &backup, true, &|_| {}).unwrap().unwrap();
        assert!(safety.is_file());
        assert_eq!(client.run("SELECT value FROM mariadb_fixture.sample;").unwrap().trim(), "original");
        mysql_client.create_database("mysql_source").unwrap();
        assert!(mysql_client.run("SHOW TABLES FROM mysql_source;").unwrap().trim().is_empty());
        mysql_client.run("CREATE TABLE mysql_source.sample (id INT PRIMARY KEY) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;").unwrap();
        mysql_client.run("SHOW CREATE TABLE mysql_source.sample;").unwrap();
        mysql_client.run("INSERT INTO mysql_source.sample VALUES (42);").unwrap();
        let source = dbmigrate::SourceConn { host: "127.0.0.1".into(), port: mysql_client.port, user: "root".into(), password: mysql_client.root_password.clone() };
        let report = dbmigrate::import_databases(&state.paths, &source, &["mysql_source".into()], &conn, |_, _| {}).unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert_eq!(client.run("SELECT id FROM mysql_source.sample;").unwrap().trim(), "42");
        // 原生计划走实际客户端、独立实例和现有 SQL 恢复链路。
        for (plan_engine, version, database, credential) in [
            (DatabaseEngine::Mysql, "8.0.46", "mysql_source", mysql_client.root_password.as_str()),
            (DatabaseEngine::Mariadb, "11.4.8", "mariadb_fixture", secret),
        ] {
            use crate::backup_job::{self, BackupPlanConfig};
            let config = BackupPlanConfig { enabled: true, keep: 1, ..Default::default() };
            backup_job::save_database_plan(&state, plan_engine, version, config.clone()).unwrap();
            assert_eq!(backup_job::run_database_plan(&state, plan_engine, version, false).unwrap().last_run_at, None);
            let first = backup_job::run_database_plan(&state, plan_engine, version, true).unwrap();
            assert_eq!(first.state, "success", "{}", first.message);
            assert_eq!(first.files.len(), if plan_engine == DatabaseEngine::Mysql { 1 } else { 2 });
            let dir = dbbackup::backup_dir(&state.paths);
            assert!(first.files.iter().all(|file| dir.join(file).is_file()));
            let key = format!("{}BackupPlan@{version}", plan_engine.id());
            state.store.set_setting(&plan_engine.password_key(version), "incorrect").unwrap();
            assert_eq!(backup_job::run_database_plan(&state, plan_engine, version, true).unwrap().state, "failed");
            assert!(first.files.iter().all(|file| dir.join(file).is_file()));
            state.store.set_setting(&plan_engine.password_key(version), credential).unwrap();
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                let held = std::fs::OpenOptions::new().read(true).share_mode(3).open(dir.join(&first.files[0])).unwrap();
                let partial = backup_job::run_database_plan(&state, plan_engine, version, true).unwrap();
                assert_eq!(partial.state, "partial", "{}", partial.message);
                assert!(dir.join(&first.files[0]).is_file());
                assert!(partial.files.iter().all(|file| dir.join(file).is_file()));
                drop(held);
            }
            let mut due = backup_job::database_plan(&state, plan_engine, version).unwrap();
            due.next_at = Some(1); state.store.set_setting_json(&key, &due).unwrap();
            backup_job::tick_databases(&state);
            let latest = backup_job::database_plan(&state, plan_engine, version).unwrap();
            assert_eq!(latest.state, "success", "{}", latest.message);
            assert!(latest.next_at.unwrap() > crate::services::now_ms());
            assert!(first.files.iter().all(|file| !dir.join(file).exists()));
            assert!(latest.files.iter().all(|file| dir.join(file).is_file()));
            assert!(backup.is_file() && safety.is_file(), "手动和恢复前备份不参与轮转");
            backup_job::tick_databases(&state);
            assert_eq!(backup_job::database_plan(&state, plan_engine, version).unwrap().last_run_at, latest.last_run_at);
            let hash = dbbackup::automatic_database_id(database);
            let file = dir.join(latest.files.iter().find(|file| file.contains(&hash)).unwrap());
            let text = std::fs::read_to_string(&file).unwrap();
            assert!(text.ends_with(&dbbackup::automatic_marker(plan_engine, version, database)));
            let native_client = if plan_engine == DatabaseEngine::Mysql { &mysql_client } else { &client };
            if plan_engine == DatabaseEngine::Mysql { native_client.run("UPDATE mysql_source.sample SET id=99;").unwrap(); }
            else { native_client.run("UPDATE mariadb_fixture.sample SET value='after schedule';").unwrap(); }
            let restore_conn = dbbackup::ConnInfo { engine: plan_engine, version: version.into(), port: native_client.port, root_password: credential.into(), bin_dir: native_client.exe.parent().map(Path::to_path_buf) };
            dbbackup::restore_from_file(&state.paths, &restore_conn, &file, false, &|_| {}).unwrap();
            if plan_engine == DatabaseEngine::Mysql { assert_eq!(native_client.run("SELECT id FROM mysql_source.sample;").unwrap().trim(), "42"); }
            else { assert_eq!(native_client.run("SELECT value FROM mariadb_fixture.sample;").unwrap().trim(), "original"); }
            let disabled = backup_job::save_database_plan(&state, plan_engine, version, BackupPlanConfig { enabled: false, ..config }).unwrap();
            assert_eq!(disabled.next_at, None);
        }
        state.stop_service("mariadb").unwrap();
        assert!(pid.iter().all(|pid| !platform::process_alive(*pid)));
        state.start_service("mariadb").unwrap();
        let (_, client) = dbadmin::authenticated_client(&state, engine, Some("11.4.8"), None).unwrap();
        assert_eq!(client.run("SELECT value FROM mariadb_fixture.sample;").unwrap().trim(), "original");
        state.stop_service("mariadb").unwrap();
        assert!(state.manager.tail("mariadb", 400).join("\n").contains("Shutdown complete"));
        // 旧共享目录只允许原版本重新打开；未知、跨版本不自动升级。
        let isolated = state.paths.data().join("mariadb-versions/11.4.8");
        std::fs::write(isolated.join(".niceenv-mariadb-version"), "10.11.13").unwrap();
        assert_eq!(state.start_service("mariadb").unwrap_err().code, "MARIADB_DATA_VERSION");
        assert!(state.manager.snapshot("mariadb").unwrap().pids.is_empty());
        assert!(isolated.join("mariadb_fixture").is_dir());
        std::fs::write(isolated.join(".niceenv-mariadb-version"), "11.4.8").unwrap();
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
                if self.0.stop_service("mysql@8.0.46").is_err() {
                    if let Ok(preview) = self.0.service_stop_preview("mysql@8.0.46") { let _ = self.0.force_stop_service("mysql@8.0.46", &preview.revision); }
                }
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
        validate_native_database_grants(&source, crate::dbadmin::DatabaseEngine::Mysql, "8.0.46", &client, "fixture_user", special, "niceenv_fixture");
        let conn = dbbackup::ConnInfo {
            engine: crate::dbadmin::DatabaseEngine::Mysql,
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
            engine: crate::dbadmin::DatabaseEngine::Mysql,
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
        let before = source.service_stop_preview("mysql@8.0.46").unwrap();
        source.store.set_setting(&dbadmin::password_key("8.0.46"), "deliberately-incorrect-fixture-password").unwrap();
        let denied = source.stop_service("mysql@8.0.46").unwrap_err();
        assert_eq!(denied.code, "DATABASE_SHUTDOWN_FAILED");
        assert!(!denied.detail.unwrap_or_default().contains("deliberately-incorrect-fixture-password"));
        assert_eq!(source.manager.snapshot("mysql@8.0.46").unwrap().pids, before.service.pids);
        assert_eq!(client.run("SELECT 1;").unwrap().trim(), "1");
        assert_eq!(source.restart_service("mysql@8.0.46").unwrap_err().code, "DATABASE_SHUTDOWN_FAILED");
        source.store.set_setting(&dbadmin::password_key("8.0.46"), special).unwrap();
        for state in [&source, &target] {
            let pids = state.manager.snapshot("mysql@8.0.46").unwrap().pids;
            state.stop_service("mysql@8.0.46").unwrap();
            assert!(pids.into_iter().all(|pid| !platform::process_alive(pid)));
        }
        source.start_service("mysql@8.0.46").unwrap();
        assert_eq!(source.force_stop_service("mysql@8.0.46", &before.revision).unwrap_err().code, "SERVICE_TARGET_CHANGED");
        let current = source.service_stop_preview("mysql@8.0.46").unwrap();
        source.force_stop_service("mysql@8.0.46", &current.revision).unwrap();
        assert!(current.service.pids.iter().all(|pid| !platform::process_alive(*pid)));
        source.start_service("mysql@8.0.46").unwrap();
        assert_eq!(dbadmin::selected_client(&source, Some("8.0.46")).unwrap().1.run("SELECT 1;").unwrap().trim(), "1");
        source.stop_service("mysql@8.0.46").unwrap();
    }

    #[test]
    #[ignore = "requires NSB_POSTGRES_ROOT; initializes only a temporary PostgreSQL cluster on a private port"]
    fn postgres_native_stop_preserves_wrong_pid_and_uses_running_version() {
        let source = PathBuf::from(std::env::var("NSB_POSTGRES_ROOT").expect("NSB_POSTGRES_ROOT"));
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("runtime");
        let root = runtime.join("pgsql");
        fn copy_tree(source: &Path, destination: &Path) {
            std::fs::create_dir_all(destination).unwrap();
            for entry in std::fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                let target = destination.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    copy_tree(&entry.path(), &target);
                } else {
                    std::fs::copy(entry.path(), target).unwrap();
                }
            }
        }
        for folder in ["bin", "lib", "share"] {
            copy_tree(&source.join(folder), &root.join(folder));
        }
        let state = isolated_state(Paths::new(temp.path().join("isolated data")));
        register_fixture(&state, "postgresql", "16.6", &runtime);
        state
            .store
            .set_setting("activepostgresqlVersion", "16.6")
            .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        state
            .store
            .set_port_override("postgres", Some(port))
            .unwrap();
        drop(listener);
        struct Stop<'a>(&'a crate::CoreState);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                if let Ok(preview) = self.0.service_stop_preview("postgresql") {
                    let _ = self.0.force_stop_service("postgresql", &preview.revision);
                }
            }
        }
        let _stop = Stop(&state);
        let data = state.paths.postgres_data_dir("16.6");
        std::fs::create_dir_all(&data).unwrap();
        let sentinel = data.join("keep.txt");
        std::fs::write(&sentinel, "existing data").unwrap();
        assert_eq!(state.start_service("postgresql").unwrap_err().code, "POSTGRES_DATA_INVALID");
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "existing data");
        std::fs::write(data.join("PG_VERSION"), "15\n").unwrap();
        assert_eq!(state.start_service("postgresql").unwrap_err().code, "POSTGRES_DATA_VERSION");
        std::fs::remove_file(data.join("PG_VERSION")).unwrap();
        std::fs::remove_file(sentinel).unwrap();
        let initializer = root.join("bin").join(exe_name("initdb"));
        let unavailable = initializer.with_extension("fixture-disabled");
        std::fs::rename(&initializer, &unavailable).unwrap();
        assert!(state.start_service("postgresql").is_err());
        assert!(data.is_dir() && std::fs::read_dir(&data).unwrap().next().is_none());
        std::fs::rename(unavailable, initializer).unwrap();
        state.start_service("postgresql").unwrap();
        let info = state.postgres_connection("16.6").unwrap();
        assert_eq!(info.port, port); assert_eq!(info.database_count, 1); assert!(info.size_bytes > 0 && info.password_required);
        let initial_password = state.postgres_password("16.6").unwrap();
        assert_eq!(initial_password.len(), 32);
        assert!(state.set_postgres_password("16.6", "wrong", true, false).is_err());
        let password = "safe:slash\\quote' 密码";
        state.set_postgres_password("16.6", password, false, true).unwrap();
        assert_eq!(state.postgres_password("16.6").unwrap(), password);
        let client = crate::dbadmin::selected_postgres(&state, "16.6", None).unwrap();
        assert!(crate::dbadmin::PostgresClient { exe: client.exe.clone(), port, password: initial_password }.query("SELECT 1").is_err());
        let management = || state.with_postgres("16.6", |client| client.list_databases()).unwrap();
        let system = management().into_iter().find(|db| db.name == "postgres").unwrap();
        assert!(system.protected);
        assert_eq!(client.drop_database("postgres", system.oid).unwrap_err().code, "POSTGRES_PROTECTED");
        assert_eq!(client.create_database("bad; DROP DATABASE postgres", "postgres").unwrap_err().code, "POSTGRES_BAD_NAME");
        assert_eq!(client.create_role("pg_reserved", "unused").unwrap_err().code, "POSTGRES_PROTECTED");
        let account_password = "project:slash\\quote' 密码";
        state.with_postgres("16.6", |client| client.create_role("project_user", account_password)).unwrap();
        assert_eq!(client.create_role("project_user", "different").unwrap_err().code, "POSTGRES_ROLE_EXISTS");
        let role = client.list_roles().unwrap().into_iter().find(|role| role.name == "project_user").unwrap();
        assert!(role.can_login && !role.superuser && !role.create_db && !role.create_role && !role.replication && !role.bypass_rls && !role.protected);
        state.with_postgres("16.6", |client| client.create_database("project_data", "project_user")).unwrap();
        assert_eq!(client.create_database("project_data", "postgres").unwrap_err().code, "POSTGRES_DATABASE_EXISTS");
        let project = management().into_iter().find(|db| db.name == "project_data").unwrap();
        assert_eq!(project.owner, "project_user"); assert_eq!(project.encoding, "UTF8"); assert!(!project.protected);
        let account_command = |password: &str| {
            let (private, mut command) = client.command().unwrap();
            std::fs::write(private.path().join("pgpass"), format!("127.0.0.1:{port}:project_data:project_user:{}\n", password.replace('\\', "\\\\").replace(':', "\\:"))).unwrap();
            command.args(["--username=project_user", "--dbname=project_data"]);
            (private, command)
        };
        let account_query = |password: &str, sql: &str| {
            let (_private, mut command) = account_command(password);
            let mut input = tempfile::tempfile().unwrap();
            std::io::Write::write_all(&mut input, sql.as_bytes()).unwrap();
            std::io::Seek::rewind(&mut input).unwrap();
            command.stdin(input).output().unwrap()
        };
        let account = account_query(account_password, "SELECT current_user; CREATE TABLE project_proof (value int); INSERT INTO project_proof VALUES (84); SELECT value FROM project_proof;");
        assert!(account.status.success(), "{}", String::from_utf8_lossy(&account.stderr));
        assert!(String::from_utf8_lossy(&account.stdout).contains("project_user"));
        assert!(!account_query(account_password, "CREATE ROLE cannot_escalate SUPERUSER;").status.success());
        assert!(client.drop_role(&role.name, role.oid).is_err());
        assert_eq!(client.drop_database(&project.name, project.oid + 1).unwrap_err().code, "POSTGRES_TARGET_CHANGED");
        let new_password = "changed:quote'\\密码";
        assert_eq!(client.set_role_password(&role.name, role.oid + 1, new_password).unwrap_err().code, "POSTGRES_TARGET_CHANGED");
        client.set_role_password(&role.name, role.oid, new_password).unwrap();
        assert!(!account_query(account_password, "SELECT 1;").status.success());
        assert!(account_query(new_password, "SELECT value FROM project_proof;").status.success());
        // 普通账号连接控制：以真实登录、并存会话和连接池行为验收，不只核对目录值。
        let access = client.role_access(&role.name, role.oid).unwrap();
        assert!(access.can_login && access.connection_limit == -1 && access.active_connections == 0);
        let mut access_input = crate::dbadmin::PostgresRoleAccessInput { name: role.name.clone(), oid: role.oid, can_login: true, connection_limit: 1, revision: access.revision.clone(), confirm_restriction: false };
        let save_access = |input: &crate::dbadmin::PostgresRoleAccessInput| crate::dbadmin::update_postgres_role_access(&state, "16.6", input);
        assert_eq!(save_access(&access_input).unwrap_err().code, "POSTGRES_CONFIRM_RESTRICTION");
        assert_eq!(client.role_access(&role.name, role.oid).unwrap().connection_limit, -1);
        access_input.confirm_restriction = true;
        let limited = save_access(&access_input).unwrap();
        assert_eq!(limited.connection_limit, 1);
        assert_eq!(save_access(&access_input).unwrap_err().code, "POSTGRES_ACCESS_CHANGED");
        access_input.revision = limited.revision.clone();
        assert_eq!(save_access(&crate::dbadmin::PostgresRoleAccessInput { connection_limit: -2, ..access_input.clone() }).unwrap_err().code, "POSTGRES_BAD_LIMIT");
        assert_eq!(save_access(&crate::dbadmin::PostgresRoleAccessInput { oid: role.oid + 1, ..access_input.clone() }).unwrap_err().code, "POSTGRES_TARGET_CHANGED");
        let administrator = client.list_roles().unwrap().into_iter().find(|role| role.name == "postgres").unwrap();
        let admin_access = client.role_access(&administrator.name, administrator.oid).unwrap();
        assert!(admin_access.protected);
        assert_eq!(save_access(&crate::dbadmin::PostgresRoleAccessInput { name: administrator.name, oid: administrator.oid, revision: admin_access.revision, ..access_input.clone() }).unwrap_err().code, "POSTGRES_PROTECTED");
        struct AccessSession(Option<std::process::Child>);
        impl Drop for AccessSession { fn drop(&mut self) { if let Some(child) = self.0.as_mut() { let _ = child.kill(); let _ = child.wait(); } } }
        let (_account_private, mut live_command) = account_command(new_password);
        let mut live = AccessSession(Some(live_command.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()));
        let mut live_input = live.0.as_mut().unwrap().stdin.take().unwrap();
        std::io::Write::write_all(&mut live_input, b"SELECT 1;\n").unwrap();
        std::io::Write::flush(&mut live_input).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while client.role_access(&role.name, role.oid).unwrap().active_connections == 0 {
            assert!(std::time::Instant::now() < deadline, "account session did not connect");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(client.role_access(&role.name, role.oid).unwrap().revision, limited.revision);
        let rejected = account_query(new_password, "SELECT 1;");
        assert!(!rejected.status.success() && String::from_utf8_lossy(&rejected.stderr).contains("too many connections"));
        access_input.can_login = false;
        let paused = save_access(&access_input).unwrap();
        assert!(!paused.can_login && paused.active_connections == 1);
        let rejected = account_query(new_password, "SELECT 1;");
        assert!(!rejected.status.success() && String::from_utf8_lossy(&rejected.stderr).contains("not permitted to log in"));
        access_input.revision = paused.revision; access_input.can_login = true; access_input.connection_limit = 0;
        let zero = save_access(&access_input).unwrap();
        assert!(zero.can_login && zero.active_connections == 1 && zero.connection_limit == 0);
        assert!(!account_query(new_password, "SELECT 1;").status.success());
        std::io::Write::write_all(&mut live_input, b"SELECT value FROM project_proof;\n").unwrap();
        drop(live_input);
        let existing = live.0.take().unwrap().wait_with_output().unwrap();
        assert!(existing.status.success() && String::from_utf8_lossy(&existing.stdout).contains("84"), "{}", String::from_utf8_lossy(&existing.stderr));
        // 外部管理员改变属性会使旧快照失效，失败不得覆盖外部改动。
        client.query("ALTER ROLE project_user CREATEDB;").unwrap();
        access_input.revision = zero.revision; access_input.connection_limit = -1;
        assert_eq!(save_access(&access_input).unwrap_err().code, "POSTGRES_ACCESS_CHANGED");
        assert_eq!(client.role_access(&role.name, role.oid).unwrap().connection_limit, 0);
        client.query("ALTER ROLE project_user NOCREATEDB;").unwrap();
        access_input.revision = client.role_access(&role.name, role.oid).unwrap().revision;
        let restored_access = save_access(&access_input).unwrap();
        assert!(restored_access.can_login && restored_access.connection_limit == -1);
        assert!(account_query(new_password, "SELECT value FROM project_proof;").status.success());
        let after_access = client.list_roles().unwrap().into_iter().find(|entry| entry.oid == role.oid).unwrap();
        assert!(!after_access.superuser && !after_access.create_db && !after_access.create_role && !after_access.replication && !after_access.bypass_rls);
        assert_eq!(after_access.databases, vec!["project_data".to_string()]);
        let unusual = "access'\"$role$中文";
        let unusual_ident = crate::dbadmin::postgres_ident(unusual).unwrap();
        client.query(&format!("CREATE ROLE {unusual_ident} LOGIN;")).unwrap();
        let unusual_role = client.list_roles().unwrap().into_iter().find(|entry| entry.name == unusual).unwrap();
        let unusual_access = client.role_access(unusual, unusual_role.oid).unwrap();
        let unusual_input = crate::dbadmin::PostgresRoleAccessInput { name: unusual.into(), oid: unusual_role.oid, can_login: false, connection_limit: 2, revision: unusual_access.revision, confirm_restriction: true };
        assert!(!save_access(&unusual_input).unwrap().can_login);
        client.drop_role(unusual, unusual_role.oid).unwrap();
        client.query(&format!("CREATE ROLE {unusual_ident} LOGIN;")).unwrap();
        assert_eq!(save_access(&unusual_input).unwrap_err().code, "POSTGRES_TARGET_CHANGED");
        let replacement = client.list_roles().unwrap().into_iter().find(|entry| entry.name == unusual).unwrap();
        assert!(replacement.can_login && replacement.connection_limit == -1);
        client.drop_role(unusual, replacement.oid).unwrap();
        // 归档必须能恢复真实结构、Unicode/二进制数据和序列；不依赖浏览器演示结果。
        let pg_query = |database: &str, sql: &str| {
            let (_private, mut command) = client.tool_command("psql", database, None).unwrap();
            let mut input = tempfile::tempfile().unwrap();
            std::io::Write::write_all(&mut input, sql.as_bytes()).unwrap();
            std::io::Seek::rewind(&mut input).unwrap();
            command.args(["--no-psqlrc", "--no-align", "--tuples-only", "--set=ON_ERROR_STOP=1"]).stdin(input).output().unwrap()
        };
        let created = account_query(new_password, "CREATE TABLE archive_payload (id serial PRIMARY KEY, label text NOT NULL, bytes bytea); INSERT INTO archive_payload (label, bytes) VALUES ('备份内容', decode('00ff1020','hex')); CREATE INDEX archive_label_idx ON archive_payload(label);");
        assert!(created.status.success(), "{}", String::from_utf8_lossy(&created.stderr));
        let large_object = account_query(new_password, "SELECT lo_from_bytea(0, decode('0123456789abcdef','hex'));");
        assert!(large_object.status.success());
        let large_object: u32 = String::from_utf8_lossy(&large_object.stdout).trim().parse().unwrap();
        assert_eq!(crate::dbbackup::postgres_dump(&state.paths, &client, "16.6", &project.name, project.oid + 1, &|_| {}).unwrap_err().code, "POSTGRES_TARGET_CHANGED");
        assert!(crate::dbbackup::postgres_dump(&state.paths, &client, "16.6", "postgres", system.oid, &|_| {}).is_err());
        let archive = state.with_postgres("16.6", |client| crate::dbbackup::postgres_dump(&state.paths, client, "16.6", &project.name, project.oid, &|_| {})).unwrap();
        assert!(std::fs::read(&archive).unwrap().starts_with(b"PGDMP"));
        assert!(crate::dbbackup::postgres_list_backups(&state.paths).unwrap().iter().any(|file| Path::new(&file.path) == archive));
        assert!(crate::dbbackup::list_backups(&state.paths).unwrap().is_empty());
        client.create_role("restore_owner", "restore-account-password").unwrap();
        let restore = |path: &Path, name: &str, trusted: bool| state.with_postgres("16.6", |client| crate::dbbackup::postgres_restore(&state.paths, client, path, name, "restore_owner", trusted, &|_| {}));
        assert_eq!(restore(&archive, "archive_restored", false).unwrap_err().code, "POSTGRES_BACKUP_UNTRUSTED");
        let invalid = temp.path().join("invalid.dump");
        std::fs::write(&invalid, "not a PostgreSQL archive").unwrap();
        assert_eq!(restore(&invalid, "archive_restored", true).unwrap_err().code, "BAD_BACKUP_FILE");
        std::fs::write(&invalid, "PGDMPbroken").unwrap();
        assert!(restore(&invalid, "archive_restored", true).is_err());
        assert!(!management().iter().any(|db| db.name == "archive_restored"));
        let external_archive = temp.path().join("external 中文 archive.dump");
        std::fs::copy(&archive, &external_archive).unwrap();
        restore(&external_archive, "archive_restored", true).unwrap();
        let restored = management().into_iter().find(|db| db.name == "archive_restored").unwrap();
        assert_eq!(restored.owner, "restore_owner");
        let restored_query = pg_query("archive_restored", "SELECT value FROM project_proof; SELECT label, encode(bytes,'hex') FROM archive_payload; SELECT pg_get_userbyid(relowner) FROM pg_class WHERE relname='archive_payload'; INSERT INTO archive_payload(label) VALUES ('second') RETURNING id;");
        assert!(restored_query.status.success(), "{}", String::from_utf8_lossy(&restored_query.stderr));
        let restored_text = String::from_utf8_lossy(&restored_query.stdout);
        assert!(restored_text.contains("84") && restored_text.contains("备份内容|00ff1020") && restored_text.contains("restore_owner") && restored_text.contains('2'));
        assert_eq!(String::from_utf8_lossy(&pg_query("archive_restored", &format!("SELECT encode(lo_get({large_object}), 'hex');")).stdout).trim(), "0123456789abcdef");
        assert_eq!(restore(&archive, "archive_restored", true).unwrap_err().code, "POSTGRES_DATABASE_EXISTS");
        assert!(String::from_utf8_lossy(&pg_query("archive_restored", "SELECT count(*) FROM archive_payload;").stdout).trim() == "2");
        client.query("CREATE ROLE \"恢复 所有者\" LOGIN;").unwrap();
        crate::dbbackup::postgres_restore(&state.paths, &client, &archive, "archive_unicode", "恢复 所有者", true, &|_| {}).unwrap();
        assert_eq!(String::from_utf8_lossy(&pg_query("archive_unicode", "SELECT pg_get_userbyid(relowner) FROM pg_class WHERE relname='archive_payload';").stdout).trim(), "恢复 所有者");
        let unicode_db = management().into_iter().find(|db| db.name == "archive_unicode").unwrap();
        client.drop_database(&unicode_db.name, unicode_db.oid).unwrap();
        let unicode_owner = client.list_roles().unwrap().into_iter().find(|role| role.name == "恢复 所有者").unwrap();
        client.drop_role(&unicode_owner.name, unicode_owner.oid).unwrap();
        // 非超级用户不能恢复事件触发器；失败必须回滚同一事务中的表和数据。
        let trigger = pg_query("project_data", "CREATE FUNCTION archive_event_probe() RETURNS event_trigger LANGUAGE plpgsql AS $$ BEGIN RETURN; END; $$; CREATE EVENT TRIGGER archive_event_probe ON ddl_command_end EXECUTE FUNCTION archive_event_probe();");
        assert!(trigger.status.success(), "{}", String::from_utf8_lossy(&trigger.stderr));
        let privileged = crate::dbbackup::postgres_dump(&state.paths, &client, "16.6", &project.name, project.oid, &|_| {}).unwrap();
        let denied = restore(&privileged, "archive_denied", true).unwrap_err();
        assert_eq!(denied.code, "POSTGRES_BACKUP_FAILED");
        assert!(denied.hint.unwrap_or_default().contains("archive_denied"));
        assert_eq!(String::from_utf8_lossy(&pg_query("archive_denied", "SELECT count(*) FROM pg_tables WHERE schemaname='public';").stdout).trim(), "0");
        assert!(pg_query("project_data", "DROP EVENT TRIGGER archive_event_probe; DROP FUNCTION archive_event_probe();").status.success());
        // 替换必须保留原库和恢复前归档；多余旧表不能混入恢复后的数据库。
        client.create_database("replace_target", "restore_owner").unwrap();
        assert!(pg_query("replace_target", "CREATE TABLE only_before (value int); INSERT INTO only_before VALUES (87);").status.success());
        let target = management().into_iter().find(|db| db.name == "replace_target").unwrap();
        let replace_input = crate::dbbackup::PostgresReplaceInput { path: archive.to_string_lossy().into(), name: target.name.clone(), oid: target.oid,
            owner: "restore_owner".into(), confirmed_name: target.name.clone(), trusted: true };
        let replace = |input: &crate::dbbackup::PostgresReplaceInput| state.with_postgres("16.6", |client| crate::dbbackup::postgres_replace_from_file(&state.paths, client, "16.6", input, &|_| {}));
        assert_eq!(replace(&crate::dbbackup::PostgresReplaceInput { confirmed_name: "wrong".into(), ..replace_input.clone() }).unwrap_err().code, "POSTGRES_CONFIRM_NAME");
        assert_eq!(replace(&crate::dbbackup::PostgresReplaceInput { oid: target.oid + 1, ..replace_input.clone() }).unwrap_err().code, "POSTGRES_TARGET_CHANGED");
        assert_eq!(replace(&crate::dbbackup::PostgresReplaceInput { trusted: false, ..replace_input.clone() }).unwrap_err().code, "POSTGRES_BACKUP_UNTRUSTED");
        assert_eq!(replace(&crate::dbbackup::PostgresReplaceInput { name: "postgres".into(), confirmed_name: "postgres".into(), oid: system.oid, ..replace_input.clone() }).unwrap_err().code, "POSTGRES_PROTECTED");
        assert!(pg_query("replace_target", "CREATE SUBSCRIPTION niceenv_replace_probe CONNECTION 'host=127.0.0.1 port=1 dbname=unused' PUBLICATION unused WITH (connect=false, slot_name=NONE);").status.success());
        assert_eq!(replace(&replace_input).unwrap_err().code, "POSTGRES_REPLICATION_UNSUPPORTED");
        assert!(pg_query("replace_target", "DROP SUBSCRIPTION niceenv_replace_probe;").status.success());
        let denied = replace(&crate::dbbackup::PostgresReplaceInput { path: privileged.to_string_lossy().into(), ..replace_input.clone() }).unwrap_err();
        assert!(denied.hint.as_deref().is_some_and(|hint| hint.contains(".dump")));
        assert!(management().iter().any(|db| db.name == target.name && db.oid == target.oid));
        assert_eq!(String::from_utf8_lossy(&pg_query("replace_target", "SELECT value FROM only_before;").stdout).trim(), "87");
        let result = replace(&replace_input).unwrap();
        assert!(Path::new(&result.safety_backup).is_file());
        assert!(management().iter().any(|db| db.name == result.previous_database && db.oid == target.oid));
        assert_eq!(String::from_utf8_lossy(&pg_query("replace_target", "SELECT value FROM project_proof;").stdout).trim(), "84");
        assert!(!pg_query("replace_target", "SELECT * FROM only_before;").status.success());
        assert_eq!(String::from_utf8_lossy(&pg_query(&result.previous_database, "SELECT value FROM only_before;").stdout).trim(), "87");
        restore(Path::new(&result.safety_backup), "safety_verified", true).unwrap();
        assert_eq!(String::from_utf8_lossy(&pg_query("safety_verified", "SELECT value FROM only_before;").stdout).trim(), "87");
        // 第二次重命名后的 OID 核对失败时，第一次重命名也必须回滚。
        client.create_database("swap_target", "restore_owner").unwrap();
        client.create_database("swap_stage", "restore_owner").unwrap();
        let swap_target = management().into_iter().find(|db| db.name == "swap_target").unwrap();
        let mut swap_stage = management().into_iter().find(|db| db.name == "swap_stage").unwrap();
        let stage_oid = swap_stage.oid;
        swap_stage.oid += 1;
        assert!(crate::dbbackup::postgres_swap_restored(&client, &swap_target, &swap_stage, "swap_previous").is_err());
        assert!(management().iter().any(|db| db.name == swap_target.name && db.oid == swap_target.oid));
        assert!(management().iter().any(|db| db.name == swap_stage.name && db.oid == stage_oid));
        assert!(!management().iter().any(|db| db.name == "swap_previous"));
        swap_stage.oid = stage_oid;
        assert!(crate::dbbackup::postgres_swap_restored(&client, &swap_target, &swap_stage, "postgres").is_err());
        let (_private_busy, mut busy_command) = client.tool_command("psql", "swap_target", None).unwrap();
        busy_command.args(["-c", "SELECT pg_sleep(12);"]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let mut busy_connection = Connection(busy_command.spawn().unwrap());
        let deadline = std::time::Instant::now();
        while client.query(&format!("SELECT count(*) FROM pg_stat_activity WHERE datid={};", swap_target.oid)).unwrap().trim() == "0" {
            assert!(deadline.elapsed() < Duration::from_secs(5)); std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(replace(&crate::dbbackup::PostgresReplaceInput { name: swap_target.name.clone(), confirmed_name: swap_target.name.clone(), oid: swap_target.oid, ..replace_input.clone() }).unwrap_err().code, "POSTGRES_DATABASE_BUSY");
        assert!(crate::dbbackup::postgres_swap_restored(&client, &swap_target, &swap_stage, "swap_previous").is_err());
        assert!(busy_connection.0.try_wait().unwrap().is_none());
        assert!(busy_connection.0.wait().unwrap().success());
        drop(busy_connection);
        assert!(management().iter().any(|db| db.name == swap_target.name && db.oid == swap_target.oid));
        crate::dbbackup::postgres_swap_restored(&client, &swap_target, &swap_stage, "swap_previous").unwrap();
        client.query("CREATE DATABASE \"swap$niceenv$'中文\";").unwrap();
        client.create_database("quoted_stage", "restore_owner").unwrap();
        let quoted_target = management().into_iter().find(|db| db.name == "swap$niceenv$'中文").unwrap();
        let quoted_stage = management().into_iter().find(|db| db.name == "quoted_stage").unwrap();
        crate::dbbackup::postgres_swap_restored(&client, &quoted_target, &quoted_stage, "quoted_previous").unwrap();
        assert!(management().iter().any(|db| db.name == quoted_target.name && db.oid == quoted_stage.oid));
        for db in management().into_iter().filter(|db| db.name.starts_with("niceenv_restore_") || db.name == result.previous_database
            || ["replace_target", "safety_verified", "swap_target", "swap_previous", "swap$niceenv$'中文", "quoted_previous"].contains(&db.name.as_str())) {
            client.drop_database(&db.name, db.oid).unwrap();
        }
        for file in crate::dbbackup::postgres_list_backups(&state.paths).unwrap().into_iter().filter(|file| file.name.contains("replace_target")) {
            crate::dbbackup::postgres_delete_backup(&state.paths, &file.name).unwrap();
        }
        for name in ["archive_restored", "archive_denied"] {
            let db = management().into_iter().find(|db| db.name == name).unwrap(); client.drop_database(name, db.oid).unwrap();
        }
        let restore_owner = client.list_roles().unwrap().into_iter().find(|role| role.name == "restore_owner").unwrap();
        client.drop_role(&restore_owner.name, restore_owner.oid).unwrap();
        // 自动备份经同一原生入口执行；到期判断、保留策略、部分失败与恢复可用性一起验证。
        let schedule = crate::backup_job::PostgresPlanConfig { enabled: true, keep: 1, ..Default::default() };
        let saved = crate::backup_job::save_postgres_plan(&state, "16.6", schedule.clone()).unwrap();
        assert!(saved.next_at.unwrap() > now_ms());
        assert_eq!(crate::backup_job::run_postgres_plan(&state, "16.6", false).unwrap().last_run_at, None);
        let first = crate::backup_job::run_postgres_plan(&state, "16.6", true).unwrap();
        assert_eq!(first.state, "success", "{}", first.message);
        let auto_dir = crate::dbbackup::postgres_backup_dir(&state.paths).unwrap();
        let auto_name = first.files.iter().find(|name| name.contains(&format!("-{}-project_data-", project.oid))).unwrap();
        let first_path = auto_dir.join(auto_name);
        let saved_key = crate::dbadmin::postgres_password_key("16.6");
        let saved_auth = state.store.get_setting_checked(&saved_key).unwrap().unwrap();
        state.store.set_setting(&saved_key, "invalid-scheduled-backup-password").unwrap();
        let failed = crate::backup_job::run_postgres_plan(&state, "16.6", true).unwrap();
        assert_eq!(failed.state, "failed"); assert!(failed.files.is_empty());
        assert!(first_path.exists() && archive.exists(), "认证失败不能清理既有备份");
        state.store.set_setting(&saved_key, &saved_auth).unwrap();
        #[cfg(windows)]
        {
            // 阻止删除旧文件：新备份仍保留，结果必须为部分完成，不能假装轮转成功。
            use std::os::windows::fs::OpenOptionsExt;
            let held_archive = std::fs::OpenOptions::new().read(true).share_mode(3).open(&first_path).unwrap();
            let partial = crate::backup_job::run_postgres_plan(&state, "16.6", true).unwrap();
            assert_eq!(partial.state, "partial", "{}", partial.message);
            assert!(first_path.exists()); drop(held_archive);
        }
        let mut due = crate::backup_job::postgres_plan(&state, "16.6").unwrap();
        due.next_at = Some(now_ms() - 1);
        state.store.set_setting_json("postgresBackupPlan@16.6", &due).unwrap();
        crate::backup_job::tick_postgres(&state);
        let done = crate::backup_job::postgres_plan(&state, "16.6").unwrap();
        assert_eq!(done.state, "success", "{}", done.message);
        assert!(!first_path.exists()); assert!(archive.exists(), "手动归档必须保留");
        let completed_at = done.last_run_at;
        assert_eq!(crate::backup_job::run_postgres_plan(&state, "16.6", false).unwrap().last_run_at, completed_at);
        let latest = done.files.iter().find(|name| name.contains(&format!("-{}-project_data-", project.oid))).unwrap();
        crate::dbbackup::postgres_restore(&state.paths, &client, &auto_dir.join(latest), "scheduled_verified", "postgres", true, &|_| {}).unwrap();
        assert_eq!(String::from_utf8_lossy(&pg_query("scheduled_verified", "SELECT value FROM project_proof;").stdout).trim(), "84");
        let verified = management().into_iter().find(|db| db.name == "scheduled_verified").unwrap(); client.drop_database(&verified.name, verified.oid).unwrap();
        let disabled = crate::backup_job::save_postgres_plan(&state, "16.6", crate::backup_job::PostgresPlanConfig { enabled: false, ..schedule }).unwrap();
        assert!(disabled.next_at.is_none());
        for file in crate::dbbackup::postgres_list_backups(&state.paths).unwrap().into_iter().filter(|file| file.name.starts_with("auto-postgresql-")) {
            crate::dbbackup::postgres_delete_backup(&state.paths, &file.name).unwrap();
        }
        // 库名里的等号、引号不得被 libpq 解释成另一个连接目标。
        client.query("CREATE DATABASE \"name=host=invalid ' 中文\";").unwrap();
        let unusual = management().into_iter().find(|db| db.name == "name=host=invalid ' 中文").unwrap();
        assert!(pg_query(&unusual.name, "CREATE TABLE retained (value int); INSERT INTO retained VALUES (86);").status.success());
        let unusual_archive = crate::dbbackup::postgres_dump(&state.paths, &client, "16.6", &unusual.name, unusual.oid, &|_| {}).unwrap();
        assert!(std::fs::metadata(&unusual_archive).unwrap().len() > 5);
        client.drop_database(&unusual.name, unusual.oid).unwrap();
        for archive in [archive, privileged, unusual_archive] {
            let name = archive.file_name().unwrap().to_str().unwrap();
            crate::dbbackup::postgres_delete_backup(&state.paths, name).unwrap(); assert!(!archive.exists());
        }
        assert!(crate::dbbackup::postgres_list_backups(&state.paths).unwrap().is_empty());
        // 有真实业务连接时，删除不得强制断开它。
        let (_private_connection, mut connection) = account_command(new_password);
        connection.args(["-c", "SELECT pg_sleep(15);"]).env("PGOPTIONS", "-c statement_timeout=30000")
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        struct Connection(std::process::Child);
        impl Drop for Connection { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
        let mut connection = Connection(connection.spawn().unwrap());
        let deadline = std::time::Instant::now();
        while client.query("SELECT COUNT(*) FROM pg_stat_activity WHERE datname='project_data';").unwrap().trim() == "0" {
            assert!(deadline.elapsed() < Duration::from_secs(5)); std::thread::sleep(Duration::from_millis(100));
        }
        assert!(client.drop_database(&project.name, project.oid).is_err());
        assert!(connection.0.try_wait().unwrap().is_none());
        assert!(connection.0.wait().unwrap().success());
        drop(connection);
        let deadline = std::time::Instant::now();
        while client.query("SELECT COUNT(*) FROM pg_stat_activity WHERE datname='project_data';").unwrap().trim() != "0" {
            assert!(deadline.elapsed() < Duration::from_secs(5)); std::thread::sleep(Duration::from_millis(100));
        }
        client.drop_database(&project.name, project.oid).unwrap();
        client.create_database(&project.name, &role.name).unwrap();
        assert_eq!(client.drop_database(&project.name, project.oid).unwrap_err().code, "POSTGRES_TARGET_CHANGED");
        let recreated = management().into_iter().find(|db| db.name == project.name).unwrap();
        client.drop_database(&recreated.name, recreated.oid).unwrap();
        client.drop_role(&role.name, role.oid).unwrap();
        assert!(!client.list_roles().unwrap().iter().any(|row| row.name == role.name));
        client.create_role(&role.name, new_password).unwrap();
        assert_eq!(client.drop_role(&role.name, role.oid).unwrap_err().code, "POSTGRES_TARGET_CHANGED");
        let recreated_role = client.list_roles().unwrap().into_iter().find(|row| row.name == role.name).unwrap();
        client.drop_role(&recreated_role.name, recreated_role.oid).unwrap();
        let admin = client.list_roles().unwrap().into_iter().find(|row| row.name == "postgres").unwrap();
        assert_eq!(client.drop_role(&admin.name, admin.oid).unwrap_err().code, "POSTGRES_PROTECTED");
        assert_eq!(client.set_role_password(&admin.name, admin.oid, "blocked").unwrap_err().code, "POSTGRES_PROTECTED");
        client.query("CREATE DATABASE \"quoted ' db\";").unwrap();
        let quoted = management().into_iter().find(|db| db.name == "quoted ' db").unwrap();
        client.drop_database(&quoted.name, quoted.oid).unwrap();
        client.query("CREATE TABLE niceenv_probe AS SELECT 42 AS value;").unwrap();
        let exported = temp.path().join("config-export.json");
        crate::transfer::export_to(&state.store, &exported).unwrap();
        assert!(!std::fs::read_to_string(exported).unwrap().contains("postgresPassword@"));
        // 旧默认 trust 只能通过明确的改密操作转换；自定义规则在改密前拒绝。
        let hba = data.join("pg_hba.conf");
        let original_hba = std::fs::read_to_string(&hba).unwrap();
        let trust_hba = original_hba.replace("scram-sha-256", "trust");
        std::fs::write(&hba, &trust_hba).unwrap();
        client.query("SELECT pg_reload_conf();").unwrap();
        for _ in 0..30 { if !client.password_required().unwrap() { break; } std::thread::sleep(Duration::from_millis(100)); }
        assert!(!state.postgres_connection("16.6").unwrap().password_required);
        assert_eq!(state.postgres_password("16.6").unwrap_err().code, "POSTGRES_AUTH_DISABLED");
        assert_eq!(state.set_postgres_password("16.6", "arbitrary", true, false).unwrap_err().code, "POSTGRES_AUTH_DISABLED");
        let custom = format!("{trust_hba}\nhost all all 192.0.2.0/24 scram-sha-256\n");
        std::fs::write(&hba, &custom).unwrap();
        assert_eq!(state.set_postgres_password("16.6", "changed", false, true).unwrap_err().code, "POSTGRES_AUTH_CUSTOM");
        assert_eq!(std::fs::read_to_string(&hba).unwrap(), custom);
        assert_eq!(state.store.get_setting(&crate::dbadmin::postgres_password_key("16.6")).as_deref(), Some(password));
        std::fs::write(&hba, &trust_hba).unwrap();
        state.set_postgres_password("16.6", password, false, true).unwrap();
        assert!(state.postgres_connection("16.6").unwrap().password_required);
        assert!(state.paths.backup().join("files").is_dir());
        state.store.set_setting(&crate::dbadmin::postgres_password_key("16.6"), "outdated").unwrap();
        state.manager.set_error("postgresql", AppError::new("CONNECTION", "fixture"));
        state.set_postgres_password("16.6", password, true, false).unwrap();
        assert_eq!(state.manager.snapshot("postgresql").unwrap().state, ServiceState::Running);
        let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        state.store.set_port_override("postgres", Some(other.local_addr().unwrap().port())).unwrap();
        assert_eq!(state.postgres_connection("16.6").unwrap().port, port);
        state.store.set_port_override("postgres", Some(port)).unwrap();
        let preview = state.service_stop_preview("postgresql").unwrap();
        let pidfile = state.paths.postgres_data_dir("16.6").join("postmaster.pid");
        let original = std::fs::read_to_string(&pidfile).unwrap();
        let changed = format!(
            "{}\n{}",
            std::process::id(),
            original.split_once('\n').unwrap().1
        );
        std::fs::write(&pidfile, changed).unwrap();
        let error = state.stop_service("postgresql").unwrap_err();
        assert_eq!(error.code, "POSTGRES_PID_UNVERIFIED");
        assert!(preview
            .service
            .pids
            .iter()
            .all(|pid| platform::process_alive(*pid)));
        std::fs::write(&pidfile, original).unwrap();
        let ctl = root.join("bin").join(exe_name("pg_ctl"));
        let renamed = ctl.with_extension("fixture-disabled");
        std::fs::rename(&ctl, &renamed).unwrap();
        assert_eq!(
            state.stop_service("postgresql").unwrap_err().code,
            "DATABASE_SHUTDOWN_FAILED"
        );
        assert!(preview
            .service
            .pids
            .iter()
            .all(|pid| platform::process_alive(*pid)));
        std::fs::rename(renamed, ctl).unwrap();
        register_fixture(
            &state,
            "postgresql",
            "99",
            &temp.path().join("missing alternative"),
        );
        state
            .store
            .set_setting("activepostgresqlVersion", "99")
            .unwrap();
        state.stop_service("postgresql").unwrap();
        assert!(preview
            .service
            .pids
            .iter()
            .all(|pid| !platform::process_alive(*pid)));
        assert!(!pidfile.exists());
        state
            .store
            .set_setting("activepostgresqlVersion", "16.6")
            .unwrap();
        state.start_service("postgresql").unwrap();
        assert_eq!(crate::dbadmin::selected_postgres(&state, "16.6", None).unwrap().query("SELECT value FROM niceenv_probe;").unwrap().trim(), "42");
        assert_eq!(
            state
                .force_stop_service("postgresql", &preview.revision)
                .unwrap_err()
                .code,
            "SERVICE_TARGET_CHANGED"
        );
        let current = state.service_stop_preview("postgresql").unwrap();
        state
            .force_stop_service("postgresql", &current.revision)
            .unwrap();
        state.start_service("postgresql").unwrap();
        state.stop_service("postgresql").unwrap();
        state.store.set_setting(&crate::dbadmin::postgres_password_key("16.6"), "outdated").unwrap();
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let requested = occupied.local_addr().unwrap().port();
        state.store.set_port_override("postgres", Some(requested)).unwrap();
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        state.start_service("postgresql").unwrap();
        let actual = state.manager.snapshot("postgresql").unwrap().port.unwrap();
        assert_ne!(actual, requested);
        assert_eq!(PortsProfile::from_settings(&state.store).postgres, actual);
        assert!(state.postgres_connection("16.6").is_err());
        state.set_postgres_password("16.6", password, true, false).unwrap();
        assert_eq!(state.postgres_connection("16.6").unwrap().port, actual);
        assert_eq!(crate::dbadmin::selected_postgres(&state, "16.6", None).unwrap().query("SELECT value FROM niceenv_probe;").unwrap().trim(), "42");
        state.stop_service("postgresql").unwrap();
    }

    #[test]
    #[ignore = "requires NSB_MONGO_ROOT, NSB_ENV_NODE and NSB_MONGO_DRIVER; writes only to a temporary MongoDB instance"]
    fn mongodb_native_stop_is_clean_and_preserves_documents_after_restart() {
        let runtime = PathBuf::from(std::env::var("NSB_MONGO_ROOT").expect("NSB_MONGO_ROOT"));
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().join("isolated mongo data")));
        register_fixture(&state, "mongodb", "8.0.4", &runtime);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        state
            .store
            .set_port_override("mongodb", Some(port))
            .unwrap();
        drop(listener);
        struct Stop<'a>(&'a crate::CoreState);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                if self.0.stop_service("mongodb").is_err() {
                    if let Ok(preview) = self.0.service_stop_preview("mongodb") {
                        let _ = self.0.force_stop_service("mongodb", &preview.revision);
                    }
                }
            }
        }
        let _stop = Stop(&state);
        let client = |phase: &str| {
            let code = r#"
const { MongoClient } = require(process.argv[1]);
const client = new MongoClient(`mongodb://127.0.0.1:${process.argv[2]}`, { directConnection: true, serverSelectionTimeoutMS: 5000, socketTimeoutMS: 5000 });
(async () => {
  try {
    await client.connect();
    const collection = client.db(process.argv[3] === 'restored' ? 'niceenv_restored' : 'niceenv_fixture').collection('documents');
    if (process.argv[3] === 'write') await collection.insertOne({ _id: 'canary', value: '持久化检查', count: 81 });
    const doc = await collection.findOne({ _id: 'canary' });
    if (!doc || doc.value !== '持久化检查' || doc.count !== 81) throw new Error('Document did not survive shutdown');
    if (process.argv[3] === 'restored') {
      const shell = await collection.findOne({ _id: 'shell' });
      if (!shell || shell.value !== '工具链检查' || await collection.countDocuments() !== 2) throw new Error('Shell data did not survive archive restore');
    }
  } finally { await client.close(); }
})().catch(error => { process.stderr.write(String(error)); process.exitCode = 1; });
"#;
            let mut command = platform::command(std::env::var("NSB_ENV_NODE").expect("NSB_ENV_NODE"));
            command.args([
                "-e",
                code,
                &std::env::var("NSB_MONGO_DRIVER").expect("NSB_MONGO_DRIVER"),
                &port.to_string(),
                phase,
            ]);
            assert!(
                crate::dbadmin::wait_client(&mut command, Duration::from_secs(15), || {})
                    .unwrap()
                    .success()
            );
        };
        state.start_service("mongodb").unwrap();
        client("write");
        // 可选真实官方工具包验收：安装管线、安装快照、管理终端及跨数据库备份恢复。
        if let Some(archives) = std::env::var_os("NSB_MONGO_TOOLS_ARCHIVES") {
            assert_eq!(crate::mongodb::browse(&state, "8.0.4", crate::mongodb::BrowseRequest::Overview).unwrap_err().code, "MONGO_SHELL_MISSING");
            let archives = PathBuf::from(archives);
            let async_runtime = tokio::runtime::Runtime::new().unwrap();
            let mut bins = std::collections::HashMap::new();
            state.store.set_setting("pathEnvEnabled", "0").unwrap();
            state.store.set_setting("pathEnvSelected", r#"["mongosh","mongodb-database-tools"]"#).unwrap();
            for id in ["mongosh", "mongodb-database-tools"] {
                let template = state.installer.template_for(id).unwrap();
                let key = format!("{id}@{}", template.version);
                let archive = archives.join(template.url.rsplit('/').next().unwrap());
                std::fs::copy(archive, state.paths.downloads().join(format!("{key}.pkg"))).unwrap();
                let installed = async_runtime.block_on(state.installer.install(&key, &state.paths, &state.store, &state.downloader, &|_| {})).unwrap();
                let binary = PathBuf::from(&installed.install_path).join(&template.entry);
                assert!(binary.is_file());
                assert_eq!(state.installer.installed_entry(&installed).entry, template.entry);
                let again = async_runtime.block_on(state.installer.install(&key, &state.paths, &state.store, &state.downloader, &|_| {})).unwrap();
                assert_eq!(again.install_path, installed.install_path);
                assert_eq!(state.installer.package_views(&state.store.list_installed().unwrap()).iter().filter(|p| p.manifest.id == id && p.install.is_some()).count(), 1);
                bins.insert(id, binary.parent().unwrap().to_path_buf());
            }
            let environment = crate::pathenv::terminal_environment(&state.store, &state.paths, &state.installer.manifest).unwrap();
            assert!(environment.warnings.is_empty(), "{:?}", environment.warnings);
            for id in ["mongosh", "mongodb-database-tools"] {
                let entry = environment.entries.iter().find(|entry| entry.id == id).unwrap();
                assert_eq!(PathBuf::from(&entry.bin_dir), bins[id]);
            }
            assert!(!crate::pathenv::is_enabled(&state.store));
            let tool_home = temp.path().join("tool home");
            std::fs::create_dir_all(&tool_home).unwrap();
            let run = |id: &str, name: &str, args: &[&str]| {
                let binary = bins[id].join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
                assert!(binary.is_file(), "{}", binary.display());
                let output = tempfile::NamedTempFile::new_in(temp.path()).unwrap();
                let mut command = platform::command(binary);
                command.args(args).current_dir(temp.path())
                    .env("HOME", &tool_home).env("USERPROFILE", &tool_home)
                    .env("APPDATA", &tool_home).env("LOCALAPPDATA", &tool_home).env("MONGOSH_LOG_DIR", &tool_home)
                    .stdout(output.reopen().unwrap()).stderr(output.reopen().unwrap());
                assert!(crate::dbadmin::wait_client(&mut command, Duration::from_secs(30), || {}).unwrap().success(), "{name} failed: {}", std::fs::read_to_string(output.path()).unwrap());
            };
            run("mongosh", "mongosh", &["--version"]);
            #[cfg(windows)]
            assert!(bins["mongosh"].join("mongosh_crypt_v1.dll").is_file());
            for name in ["bsondump", "mongodump", "mongoexport", "mongofiles", "mongoimport", "mongorestore", "mongostat", "mongotop"] {
                run("mongodb-database-tools", name, &["--version"]);
            }
            let uri = format!("mongodb://127.0.0.1:{port}/niceenv_fixture?directConnection=true&serverSelectionTimeoutMS=5000");
            run("mongosh", "mongosh", &[&uri, "--quiet", "--norc", "--eval", "if (db.documents.findOne({_id:'canary'}).count !== 81) throw new Error('wrong source'); db.documents.insertOne({_id:'shell',value:'工具链检查'});"]);
            let archive = temp.path().join("mongo archive with spaces.gz");
            let archive_arg = format!("--archive={}", archive.display());
            run("mongodb-database-tools", "mongodump", &["--uri", &uri, &archive_arg, "--gzip"]);
            assert!(std::fs::metadata(&archive).unwrap().len() > 0);
            let restore_uri = format!("mongodb://127.0.0.1:{port}/?directConnection=true&serverSelectionTimeoutMS=5000");
            run("mongodb-database-tools", "mongorestore", &["--uri", &restore_uri, &archive_arg, "--gzip", "--nsInclude=niceenv_fixture.*", "--nsFrom=niceenv_fixture.*", "--nsTo=niceenv_restored.*"]);
            client("restored");
            use crate::mongodb::{BrowseRequest, BrowseResponse, DocumentFilter};
            let overview = crate::mongodb::browse(&state, "8.0.4", BrowseRequest::Overview).unwrap();
            match overview {
                BrowseResponse::Overview { port: actual, databases, .. } => { assert_eq!(actual, port); assert!(databases.contains(&"niceenv_fixture".into())); assert!(databases.contains(&"niceenv_restored".into())); }
                _ => panic!("wrong overview response"),
            }
            let collections = crate::mongodb::browse(&state, "8.0.4", BrowseRequest::Collections { database: "niceenv_fixture".into(), search: "doc".into() }).unwrap();
            match collections { BrowseResponse::Collections { entries, .. } => assert_eq!(entries[0].name, "documents"), _ => panic!("wrong collections response") }
            let page = |offset, filter| BrowseRequest::Documents { database: "niceenv_fixture".into(), collection: "documents".into(), offset, limit: 1, filter };
            match crate::mongodb::browse(&state, "8.0.4", page(0, None)).unwrap() {
                BrowseResponse::Documents { documents, has_more, .. } => { assert!(has_more); assert!(documents[0].content.contains("canary")); }
                _ => panic!("wrong documents response"),
            }
            match crate::mongodb::browse(&state, "8.0.4", page(1, None)).unwrap() {
                BrowseResponse::Documents { documents, has_more, .. } => { assert!(!has_more); assert!(documents[0].content.contains("工具链检查")); }
                _ => panic!("wrong documents response"),
            }
            let equals = |field: &str, value: &str, kind: &str| Some(DocumentFilter { field: field.into(), value: value.into(), value_type: kind.into() });
            for filter in [equals("count", "81", "number"), equals("value", "工具链检查", "text")] {
                match crate::mongodb::browse(&state, "8.0.4", page(0, filter)).unwrap() {
                    BrowseResponse::Documents { documents, has_more, .. } => { assert!(!has_more); assert_eq!(documents.len(), 1); assert!(!documents[0].truncated); }
                    _ => panic!("wrong filtered response"),
                }
            }
            assert_eq!(crate::mongodb::browse(&state, "99", BrowseRequest::Overview).unwrap_err().code, "MONGO_NOT_RUNNING");
            assert_eq!(crate::mongodb::browse(&state, "8.0.4", page(0, equals("$where", "throw new Error('injection')", "text"))).unwrap_err().code, "MONGO_QUERY_INVALID");
            assert_eq!(crate::mongodb::browse(&state, "8.0.4", page(0, equals("count", "NaN", "number"))).unwrap_err().code, "MONGO_QUERY_INVALID");
            assert_eq!(crate::mongodb::browse(&state, "8.0.4", page(10_001, None)).unwrap_err().code, "MONGO_QUERY_INVALID");
            let missing = BrowseRequest::Documents { database: "niceenv_fixture".into(), collection: "absent".into(), offset: 0, limit: 10, filter: None };
            assert_eq!(crate::mongodb::browse(&state, "8.0.4", missing).unwrap_err().code, "MONGO_COLLECTION_MISSING");
            run("mongosh", "mongosh", &[&uri, "--quiet", "--norc", "--eval", r#"
const browse = db.getSiblingDB('niceenv_browse');
for (let i=0;i<105;i++) browse.createCollection('collection_'+i);
browse.getCollection("quotes'[];collection").insertOne({_id:ObjectId('0123456789abcdef01234567'), value:"'); db.dropDatabase();//", active:true, nothing:null, largeInteger:Long('9007199254740993'), precise:Decimal128('12.50'), date:new Date('2026-09-29T00:00:00Z')});
browse.createView('view_documents', "quotes'[];collection", []);
browse.large.insertOne({_id:'large', value:'中'.repeat(70000)});
browse.createCollection('measurements', {timeseries:{timeField:'time',metaField:'sensor'}});
browse.measurements.insertOne({_id:'measurement',time:new Date('2026-09-29T00:00:00Z'),sensor:'local',value:23});
"#]);
            match crate::mongodb::browse(&state, "8.0.4", BrowseRequest::Collections { database: "niceenv_browse".into(), search: String::new() }).unwrap() {
                BrowseResponse::Collections { entries, limited, .. } => { assert_eq!(entries.iter().filter(|entry| entry.name.starts_with("collection_")).count(), 105); assert!(!limited); assert!(entries.iter().any(|entry| entry.kind == "view")); }
                _ => panic!("wrong collections response"),
            }
            match crate::mongodb::browse(&state, "8.0.4", BrowseRequest::Collections { database: "niceenv_browse".into(), search: "[];".into() }).unwrap() {
                BrowseResponse::Collections { entries, .. } => assert_eq!(entries.len(), 1), _ => panic!("wrong literal name search"),
            }
            let special = |collection: &str, filter| BrowseRequest::Documents { database: "niceenv_browse".into(), collection: collection.into(), offset: 0, limit: 10, filter };
            for filter in [equals("value", "'); db.dropDatabase();//", "text"), equals("active", "true", "boolean"), equals("nothing", "", "null"), equals("_id", "0123456789abcdef01234567", "objectId")] {
                match crate::mongodb::browse(&state, "8.0.4", special("quotes'[];collection", filter)).unwrap() {
                    BrowseResponse::Documents { documents, .. } => {
                        assert_eq!(documents.len(), 1);
                        let data: serde_json::Value = serde_json::from_str(&documents[0].content).unwrap();
                        assert_eq!(data["largeInteger"]["$numberLong"], "9007199254740993");
                        assert_eq!(data["precise"]["$numberDecimal"], "12.50");
                        assert!(data["date"]["$date"]["$numberLong"].is_string());
                    }
                    _ => panic!("wrong special document result"),
                }
            }
            match crate::mongodb::browse(&state, "8.0.4", special("large", None)).unwrap() {
                BrowseResponse::Documents { documents, .. } => { assert!(documents[0].truncated); assert_eq!(documents[0].content.chars().count(), 65_536); }
                _ => panic!("wrong large document result"),
            }
            match crate::mongodb::browse(&state, "8.0.4", special("view_documents", None)).unwrap() {
                BrowseResponse::Documents { documents, .. } => assert_eq!(documents.len(), 1), _ => panic!("wrong view result"),
            }
            // 图形备份/恢复入口的原生验收；以独立 Node 驱动核对实际数据与索引。
            use crate::mongodb_backup as mb;
            run("mongosh", "mongosh", &[&uri, "--quiet", "--norc", "--eval", "const source=db.getSiblingDB('niceenv_browse'); source.documents.insertMany(db.documents.find().toArray()); source.documents.createIndex({value:1},{name:'value_lookup'}); db.getSiblingDB('niceenv_target').keep.insertOne({_id:1,value:'保护原数据'});"]);
            assert!(mb::list(&state).unwrap().items.is_empty());
            for name in ["admin", "local", "config", "a.*", "a$b", "../x", "", "名字".repeat(22).as_str()] {
                assert_eq!(mb::create(&state, "8.0.4", name).unwrap_err().code, "MONGO_BACKUP_INVALID");
            }
            assert!(mb::create(&state, "8.0.4", "absent_database").is_err());
            let backup = mb::create(&state, "8.0.4", "niceenv_browse").unwrap();
            assert_eq!(backup.tools_version, state.installer.template_for("mongodb-database-tools").unwrap().version);
            assert!(backup.size_bytes > 0); assert_eq!(mb::list(&state).unwrap().items.len(), 1);
            let fresh = mb::preview(&state, "8.0.4", &backup.id, "niceenv_backup_new").unwrap();
            assert!(!fresh.exists);
            assert!(mb::restore(&state, "8.0.4", &backup.id, &fresh.target, &fresh.revision, "wrong").is_err());
            let restored = mb::restore(&state, "8.0.4", &backup.id, &fresh.target, &fresh.revision, &fresh.target).unwrap();
            assert!(restored.safety_backup.is_none());
            let stale = mb::preview(&state, "8.0.4", &backup.id, "niceenv_target").unwrap();
            run("mongosh", "mongosh", &[&uri, "--quiet", "--norc", "--eval", "db.getSiblingDB('niceenv_target').extra.insertOne({_id:2});"]);
            assert_eq!(mb::restore(&state, "8.0.4", &backup.id, &stale.target, &stale.revision, &stale.target).unwrap_err().code, "MONGO_RESTORE_CHANGED");
            let existing = mb::preview(&state, "8.0.4", &backup.id, "niceenv_target").unwrap();
            assert!(existing.exists);
            let dump_exe = bins["mongodb-database-tools"].join(format!("mongodump{}", std::env::consts::EXE_SUFFIX));
            let disabled_dump = dump_exe.with_extension("fixture-disabled");
            std::fs::rename(&dump_exe, &disabled_dump).unwrap();
            let safety_failure = mb::restore(&state, "8.0.4", &backup.id, &existing.target, &existing.revision, &existing.target);
            let missing_tools_plan = crate::backup_job::run_mongodb_plan(&state, "8.0.4", true).unwrap();
            std::fs::rename(disabled_dump, dump_exe).unwrap();
            assert_eq!(safety_failure.unwrap_err().code, "MONGO_TOOLS_MISSING");
            assert_eq!(missing_tools_plan.state, "failed"); assert!(missing_tools_plan.files.is_empty());
            run("mongosh", "mongosh", &[&uri, "--quiet", "--norc", "--eval", "if(db.getSiblingDB('niceenv_target').keep.findOne({_id:1}).value!=='保护原数据')throw Error('Safety backup failure modified target');"]);
            let replaced = mb::restore(&state, "8.0.4", &backup.id, &existing.target, &existing.revision, &existing.target).unwrap();
            let safety = replaced.safety_backup.unwrap(); assert_eq!(safety.kind, "before-restore");
            let safety_preview = mb::preview(&state, "8.0.4", &safety.id, "niceenv_safety_check").unwrap();
            mb::restore(&state, "8.0.4", &safety.id, &safety_preview.target, &safety_preview.revision, &safety_preview.target).unwrap();
            let export_path = temp.path().join("external Mongo backup.gz");
            mb::export(&state, &backup.id, export_path.to_str().unwrap()).unwrap();
            assert_eq!(mb::export(&state, &backup.id, export_path.to_str().unwrap()).unwrap_err().code, "MONGO_EXPORT_EXISTS");
            assert!(mb::export(&state, &backup.id, state.paths.base.join("bad.gz").to_str().unwrap()).is_err());
            let exported = mb::inspect_import(export_path.to_str().unwrap()).unwrap();
            assert_eq!(exported.info.version, "8.0.4"); assert_eq!(exported.info.compression, "gzip");
            assert_eq!(exported.info.databases.len(), 1); assert_eq!(exported.info.databases[0].name, "niceenv_browse");
            let multi_path = temp.path().join("multiple databases.archive");
            run("mongodb-database-tools", "mongodump", &["--uri", &restore_uri, &format!("--archive={}",multi_path.display())]);
            let multi = mb::inspect_import(multi_path.to_str().unwrap()).unwrap();
            assert_eq!(multi.info.compression, "none"); assert!(multi.info.databases.len() > 1);
            let raw_archive = std::fs::read(&multi_path).unwrap();
            let inspect_bytes = |bytes: &[u8]| crate::mongodb_archive::inspect(bytes, None, std::io::sink());
            assert!(inspect_bytes(&raw_archive[..raw_archive.len()-1]).is_err());
            let mut trailing = raw_archive.clone(); trailing.extend_from_slice(&[0;4]); assert!(inspect_bytes(&trailing).is_err());
            let mut bad_length = raw_archive.clone(); bad_length[4..8].copy_from_slice(&u32::MAX.to_le_bytes()); assert!(inspect_bytes(&bad_length).is_err());
            let mut crc_error = raw_archive.clone();
            let canary = crc_error.windows(7).position(|bytes|bytes==b"canary\0").unwrap(); crc_error[canary] ^= 1;
            assert_eq!(inspect_bytes(&crc_error).unwrap_err().message, "MongoDB 归档集合 CRC 校验失败");
            std::fs::write(&multi_path, &crc_error).unwrap(); assert!(mb::import_archive(&state, &multi.source, "niceenv_browse", &multi.revision).is_err());
            std::fs::write(&multi_path, &raw_archive).unwrap();
            assert!(mb::import_archive(&state, &multi.source, "missing", &multi.revision).is_err());
            assert_eq!(mb::import_archive(&state, &multi.source, "niceenv_browse", "stale").unwrap_err().code, "MONGO_IMPORT_CHANGED");
            let imported = mb::import_archive(&state, &multi.source, "niceenv_browse", &multi.revision).unwrap();
            assert_eq!(imported.kind, "imported");
            let imported_path = mb::directory(&state).unwrap().join(&imported.id).join("archive.gz");
            let filtered = mb::inspect_import(imported_path.to_str().unwrap()).unwrap();
            assert_eq!(filtered.info.databases.len(),1); assert_eq!(filtered.info.databases[0].name,"niceenv_browse");
            let imported_preview = mb::preview(&state, "8.0.4", &imported.id, "niceenv_imported").unwrap();
            mb::restore(&state, "8.0.4", &imported.id, &imported_preview.target, &imported_preview.revision, &imported_preview.target).unwrap();
            let oracle = r#"
const {MongoClient,EJSON}=require(process.argv[1]); const c=new MongoClient(`mongodb://127.0.0.1:${process.argv[2]}`);
(async()=>{try{await c.connect(); for(const name of ['niceenv_backup_new','niceenv_target','niceenv_imported']) {
 const db=c.db(name),source=c.db('niceenv_browse');
 const collections=await source.listCollections().toArray();
 for(const item of collections.filter(v=>v.name!=='system.views')) {
  const a=await source.collection(item.name).find().sort({_id:1}).toArray(),b=await db.collection(item.name).find().sort({_id:1}).toArray();
  if(JSON.stringify(a)!==JSON.stringify(b))throw Error('Data mismatch '+name+'.'+item.name);
 }
 if(!(await db.collection('documents').indexes()).some(i=>i.name==='value_lookup'))throw Error('Index missing');
 if((await db.listCollections({name:'view_documents'}).toArray())[0]?.type!=='view')throw Error('View missing');
 if(await db.collection('keep').countDocuments() || await db.collection('extra').countDocuments())throw Error('Old target collections remain');
}
if((await c.db('niceenv_safety_check').collection('keep').findOne({_id:1}))?.value!=='保护原数据')throw Error('Safety copy missing');
if(await c.db('niceenv_safety_check').collection('extra').countDocuments()!==1)throw Error('Safety extra missing');
}finally{await c.close()}})().catch(e=>{process.stderr.write(String(e));process.exitCode=1});
"#;
            let mut verify = platform::command(std::env::var("NSB_ENV_NODE").unwrap());
            verify.args(["-e", oracle, &std::env::var("NSB_MONGO_DRIVER").unwrap(), &port.to_string()]);
            assert!(crate::dbadmin::wait_client(&mut verify, Duration::from_secs(30), || {}).unwrap().success());
            let archive_path = mb::directory(&state).unwrap().join(&backup.id).join("archive.gz");
            let original_archive = std::fs::read(&archive_path).unwrap();
            let mut corrupt = original_archive.clone(); corrupt[0] ^= 1; std::fs::write(&archive_path, corrupt).unwrap();
            assert_eq!(mb::preview(&state, "8.0.4", &backup.id, "niceenv_target").unwrap_err().code, "MONGO_BACKUP_CHECKSUM");
            std::fs::write(&archive_path, &original_archive).unwrap();
            let metadata_path = archive_path.parent().unwrap().join("metadata.json");
            let original_metadata = std::fs::read(&metadata_path).unwrap();
            let mut changed: serde_json::Value = serde_json::from_slice(&original_metadata).unwrap();
            changed["version"] = serde_json::json!("7.0.0"); std::fs::write(&metadata_path, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert_eq!(mb::preview(&state, "8.0.4", &backup.id, "niceenv_target").unwrap_err().code, "MONGO_BACKUP_METADATA");
            changed["version"] = serde_json::json!(backup.version); changed["toolsVersion"] = serde_json::json!("99.0.0");
            std::fs::write(&metadata_path, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert_eq!(mb::preview(&state, "8.0.4", &backup.id, "niceenv_target").unwrap_err().code, "MONGO_BACKUP_METADATA");
            // 即使记录摘要对应损坏文件，归档结构检查也必须在预览阶段拒绝它。
            use sha2::{Digest, Sha256};
            let malformed = b"not a MongoDB archive";
            changed["toolsVersion"] = serde_json::json!(backup.tools_version); changed["sizeBytes"] = serde_json::json!(malformed.len());
            changed["sha256"] = serde_json::json!(hex::encode(Sha256::digest(malformed)));
            std::fs::write(&archive_path, malformed).unwrap(); std::fs::write(&metadata_path, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert_eq!(mb::preview(&state, "8.0.4", &backup.id, "niceenv_target").unwrap_err().code, "MONGO_ARCHIVE_INVALID");
            run("mongosh", "mongosh", &[&uri, "--quiet", "--norc", "--eval", "if(db.getSiblingDB('niceenv_target').documents.findOne({_id:'canary'}).count!==81)throw Error('Preflight failure modified target');"]);
            std::fs::write(&archive_path, original_archive).unwrap(); std::fs::write(&metadata_path, original_metadata).unwrap();
            assert!(mb::preview(&state, "8.0.4", "../escape", "niceenv_target").is_err());
            std::fs::create_dir_all(mb::directory(&state).unwrap().join("broken-record")).unwrap();
            assert_eq!(mb::list(&state).unwrap().unreadable, 1);
            assert_eq!(mb::list(&state).unwrap().issues[0].id, "broken-record");
            let broken = mb::removal_preview(&state,"broken-record").unwrap();
            mb::remove(&state,"broken-record",&broken.revision).unwrap();
            let removal = mb::removal_preview(&state,&imported.id).unwrap();
            std::fs::write(imported_path.parent().unwrap().join("user-note.txt"), "keep").unwrap();
            assert_eq!(mb::remove(&state,&imported.id,&removal.revision).unwrap_err().code, "MONGO_BACKUP_EXTRA_FILES");
            std::fs::remove_file(imported_path.parent().unwrap().join("user-note.txt")).unwrap();
            let saved = std::fs::read(&imported_path).unwrap(); std::fs::write(&imported_path,b"changed").unwrap();
            assert_eq!(mb::remove(&state,&imported.id,&removal.revision).unwrap_err().code, "MONGO_BACKUP_CHANGED");
            std::fs::write(&imported_path,saved).unwrap(); mb::remove(&state,&imported.id,&removal.revision).unwrap();
            assert!(!imported_path.exists()); assert!(export_path.exists());
            // 独立 CoreState 验证纯文件功能无需安装数据库或工具，不改运行实例。
            let file_only = isolated_state(Paths::new(temp.path().join("file only mongo workspace")));
            let external = mb::inspect_import(export_path.to_str().unwrap()).unwrap();
            let offline = mb::import_archive(&file_only, &external.source, "niceenv_browse", &external.revision).unwrap();
            let offline_dest = temp.path().join("offline export.gz"); mb::export(&file_only,&offline.id,offline_dest.to_str().unwrap()).unwrap();
            let offline_removal = mb::removal_preview(&file_only,&offline.id).unwrap(); mb::remove(&file_only,&offline.id,&offline_removal.revision).unwrap();
            assert!(mb::list(&file_only).unwrap().items.is_empty());
            // 复用真实工具链验收 MongoDB 计划：到期仅执行一次，关闭后仍可立即执行。
            use crate::backup_job as schedule;
            let config = schedule::BackupPlanConfig { enabled: true, keep: 1, ..Default::default() };
            let mut plan = schedule::save_mongodb_plan(&state, "8.0.4", config).unwrap();
            assert!(plan.next_at.unwrap() > crate::services::now_ms());
            let before = mb::list(&state).unwrap().items;
            schedule::tick_mongodb(&state);
            assert_eq!(mb::list(&state).unwrap().items, before);
            plan.next_at = Some(1);
            state.store.set_setting_json("mongodbBackupPlan@8.0.4", &plan).unwrap();
            schedule::tick_mongodb(&state);
            let first = schedule::mongodb_plan(&state, "8.0.4").unwrap();
            assert_eq!(first.state, "success", "{}", first.message);
            let names = mb::automatic_databases(&state,"8.0.4").unwrap();
            assert_eq!(first.files.len(), names.len()); assert!(names.len() >= 3);
            assert!(names.iter().all(|name| !["admin","local","config"].contains(&name.as_str())));
            schedule::tick_mongodb(&state);
            assert_eq!(schedule::mongodb_plan(&state,"8.0.4").unwrap().files, first.files);
            let first_copies = mb::list(&state).unwrap().items;
            let automatic = first_copies.iter().find(|r|r.kind=="automatic" && r.database=="niceenv_fixture").unwrap();
            // 用真实归档复制受管样本，验证坏文件、额外内容及不同版本不会被自动删除。
            let dir = mb::directory(&state).unwrap();
            let copy = |id: &str, version: &str, kind: &str| {
                let mut record = automatic.clone(); record.id=id.into(); record.version=version.into(); record.kind=kind.into(); record.created_at=4_000_000_000;
                let dest=dir.join(id); std::fs::create_dir(&dest).unwrap();
                std::fs::copy(dir.join(&automatic.id).join("archive.gz"),dest.join("archive.gz")).unwrap();
                std::fs::write(dest.join("metadata.json"),serde_json::to_vec(&record).unwrap()).unwrap();
                record
            };
            let old = copy("schedule-future", "8.0.4", "automatic");
            let corrupt = copy("schedule-corrupt", "8.0.4", "automatic");
            std::fs::write(dir.join(&corrupt.id).join("archive.gz"), b"bad archive").unwrap();
            let extra = copy("schedule-extra", "8.0.4", "automatic");
            std::fs::write(dir.join(&extra.id).join("note.txt"), "user content").unwrap();
            let other = copy("schedule-other-version", "8.0.5", "automatic");
            let imported_copy = copy("schedule-imported", "8.0.4", "imported");
            mb::rotate_automatic(&state,automatic,0).unwrap(); assert!(dir.join(&old.id).exists());
            assert_eq!(mb::rotate_automatic(&state,automatic,1).unwrap_err().code, "MONGO_ROTATION_INCOMPLETE");
            assert!(dir.join(&automatic.id).exists()); assert!(!dir.join(&old.id).exists());
            for record in [&corrupt,&extra,&other,&imported_copy] { assert!(dir.join(&record.id).exists()); }
            let mut disabled = first.config.clone(); disabled.enabled=false;
            schedule::save_mongodb_plan(&state,"8.0.4",disabled).unwrap();
            let second = schedule::run_mongodb_plan(&state,"8.0.4",true).unwrap();
            assert_eq!(second.state,"partial","{}",second.message); assert!(second.next_at.is_none());
            assert_eq!(second.files.len(),names.len()); assert!(second.message.contains("清理"));
            for id in first.files { assert!(!dir.join(id).exists()); }
            let second_copies = mb::list(&state).unwrap();
            for record in before { assert!(second_copies.items.contains(&record)); }
            assert!(second_copies.items.contains(&imported_copy)); assert!(dir.join(&other.id).exists());
            assert_eq!(std::fs::read_to_string(dir.join(&extra.id).join("note.txt")).unwrap(),"user content");
            let scheduled = second_copies.items.iter().find(|r|r.kind=="automatic" && r.database=="niceenv_fixture" && second.files.contains(&r.id)).unwrap();
            let preview = mb::preview(&state,"8.0.4",&scheduled.id,"niceenv_scheduled").unwrap();
            mb::restore(&state,"8.0.4",&scheduled.id,"niceenv_scheduled",&preview.revision,"niceenv_scheduled").unwrap();
            run("mongosh","mongosh",&[&uri,"--quiet","--norc","--eval","if(db.getSiblingDB('niceenv_scheduled').documents.countDocuments({})!==2)throw Error('Scheduled archive restore lost data');"]);
            let unchanged = schedule::run_mongodb_plan(&state,"8.0.4",false).unwrap();
            assert_eq!(unchanged.files,second.files);
            let mut wrong_directory = isolated_state(Paths::new(temp.path().join("wrong mongo directory")));
            wrong_directory.manager = state.manager.clone();
            for package in state.store.list_installed().unwrap() { wrong_directory.store.upsert_installed(&package).unwrap(); }
            assert_eq!(crate::mongodb::browse(&wrong_directory, "8.0.4", BrowseRequest::Overview).unwrap_err().code, "MONGO_INSTANCE_CHANGED");
            assert_eq!(mb::create(&wrong_directory, "8.0.4", "niceenv_fixture").unwrap_err().code, "MONGO_INSTANCE_CHANGED");
            assert_eq!(schedule::run_mongodb_plan(&wrong_directory,"8.0.4",true).unwrap().state,"failed");
            let guard = state.manager.lifecycle.lock();
            std::thread::scope(|scope| {
                assert_eq!(scope.spawn(|| crate::mongodb::browse(&state, "8.0.4", BrowseRequest::Overview)).join().unwrap().unwrap_err().code, "SERVICE_BUSY");
                assert_eq!(scope.spawn(|| mb::create(&state, "8.0.4", "niceenv_fixture")).join().unwrap().unwrap_err().code, "SERVICE_BUSY");
                let busy = scope.spawn(|| schedule::run_mongodb_plan(&state,"8.0.4",true)).join().unwrap().unwrap();
                assert_eq!(busy.state,"failed"); assert!(busy.message.contains("服务正在操作"));
            });
            drop(guard);
            // 完整认证流程使用真实 mongod/mongosh/tools；结束后关闭认证以继续既有停机验收。
            use crate::mongodb_auth as auth;
            let initial=auth::status(&state,"8.0.4").unwrap();
            assert_eq!(initial.authorization,Some(false)); assert_eq!(initial.has_users,Some(false));
            let credentials=auth::Credentials { username:"manager'用户名".into(),password:"fixture\\quote'\"秘密:12345".into(),auth_database:"admin".into() };
            assert!(auth::save_connection(&state,"8.0.4",&initial.revision,credentials.clone()).is_err());
            assert!(state.store.get_setting("mongodbCredentials@8.0.4").is_none());
            let request=|revision: String, enabled, acknowledge_restart, acknowledge_disable, administrator| auth::ApplyAuth {revision,enabled,acknowledge_restart,acknowledge_disable,administrator};
            assert_eq!(auth::apply(&state,"8.0.4",request(initial.revision.clone(),true,false,false,Some(credentials.clone()))).unwrap_err().code,"MONGO_AUTH_CONFIRM");
            assert_eq!(auth::apply(&state,"8.0.4",request("stale".into(),true,true,false,Some(credentials.clone()))).unwrap_err().code,"MONGO_AUTH_CHANGED");
            let secured=auth::apply(&state,"8.0.4",request(initial.revision.clone(),true,true,false,Some(credentials.clone()))).unwrap();
            assert!(secured.configured && secured.administrator && secured.has_password); assert_eq!(secured.authorization,Some(true));
            assert!(!serde_json::to_string(&secured).unwrap().contains(&credentials.password));
            let anonymous=auth::Credentials::default();
            assert_eq!(crate::mongodb::execute_as(&state,"8.0.4",serde_json::json!({}),"print(JSON.stringify({result:true}));",&anonymous).unwrap_err().code,"MONGO_ACCESS_DENIED");
            assert!(crate::mongodb::browse(&state,"8.0.4",BrowseRequest::Overview).is_ok());
            let auth_backup=mb::create(&state,"8.0.4","niceenv_fixture").unwrap();
            let auth_preview=mb::preview(&state,"8.0.4",&auth_backup.id,"niceenv_authenticated").unwrap();
            mb::restore(&state,"8.0.4",&auth_backup.id,"niceenv_authenticated",&auth_preview.revision,"niceenv_authenticated").unwrap();
            let authenticated_plan=schedule::run_mongodb_plan(&state,"8.0.4",true).unwrap();
            assert_eq!(authenticated_plan.state,"partial","{}",authenticated_plan.message); assert!(!authenticated_plan.files.is_empty());
            let stored=state.store.get_setting("mongodbCredentials@8.0.4").unwrap();
            let mut wrong=credentials.clone(); wrong.password="wrong-secret-123".into();
            assert!(auth::save_connection(&state,"8.0.4",&secured.revision,wrong).is_err());
            assert_eq!(state.store.get_setting("mongodbCredentials@8.0.4").unwrap(),stored);
            assert_eq!(auth::apply(&state,"8.0.4",request(secured.revision.clone(),false,true,false,None)).unwrap_err().code,"MONGO_AUTH_CONFIRM");
            let new_password="next:密码'\"\\secret-67890".to_string();
            let changed=auth::change_password(&state,"8.0.4",&secured.revision,new_password.clone()).unwrap();
            assert!(changed.administrator && changed.has_password);
            assert_eq!(crate::mongodb::execute_as(&state,"8.0.4",serde_json::json!({}),"print(JSON.stringify({result:true}));",&credentials).unwrap_err().code,"MONGO_ACCESS_DENIED");
            let fresh=auth::Credentials {password:new_password.clone(),..credentials.clone()};
            let connected=auth::save_connection(&state,"8.0.4",&changed.revision,fresh.clone()).unwrap();
            assert!(connected.administrator);
            // 外部客户端改密后，本机旧凭据失效；恢复入口应在受管无认证短窗口内更新账号并重新开启认证。
            let externally_changed=auth::Credentials {password:"external-expired-123".into(),..fresh.clone()};
            crate::mongodb::execute_as(&state,"8.0.4",serde_json::json!({"password":externally_changed.password}),r#"
      checked(connection.getDB(input.credentials.authDatabase).runCommand({updateUser:input.credentials.username,pwd:input.request.password}));
      print(JSON.stringify({result:true}));
    "#,&fresh).unwrap();
            let expired=auth::status(&state,"8.0.4").unwrap(); assert!(expired.problem.is_some()); assert_eq!(expired.username,fresh.username);
            let recovered_password="recovered:password-123".to_string();
            let recovered=auth::reset_password(&state,"8.0.4",&expired.revision,recovered_password.clone()).unwrap();
            assert!(recovered.administrator && recovered.authorization==Some(true));
            let recovered_credentials=auth::Credentials {password:recovered_password.clone(),..fresh.clone()};
            // 本机连接记录损坏仍可从图形入口验证并修复，不回退为匿名访问。
            state.store.set_setting("mongodbCredentials@8.0.4","broken").unwrap();
            let damaged=auth::status(&state,"8.0.4").unwrap(); assert!(damaged.problem.is_some());
            assert!(auth::save_connection(&state,"8.0.4",&damaged.revision,recovered_credentials.clone()).unwrap().administrator);
            let (exported,_)=crate::transfer::encode_export(&state.store).unwrap();
            let exported=String::from_utf8(exported).unwrap();
            for needle in ["mongodbCredentials@","mongodbAuthEnabled@","mongodbBackupPlan@",&credentials.password,&new_password] { assert!(!exported.contains(needle)); }
            assert!(crate::envfile::is_secret_key("mongodbCredentials@8.0.4"));
            assert!(!recovered_credentials.redact(&format!("failure {recovered_password}")).contains(&recovered_password));
            assert!(!std::fs::read_dir(&state.paths.base).unwrap().any(|item|item.unwrap().file_name().to_string_lossy().starts_with(".mongo-browse-")));
            let unprotected=auth::apply(&state,"8.0.4",request(auth::status(&state,"8.0.4").unwrap().revision,true,true,false,Some(credentials.clone()))).unwrap_err();
            assert_eq!(unprotected.code,"MONGO_ADMIN_EXISTS");
            let disabled=auth::apply(&state,"8.0.4",request(auth::status(&state,"8.0.4").unwrap().revision,false,true,true,None)).unwrap();
            assert_eq!(disabled.authorization,Some(false)); assert!(!disabled.configured);
        }
        let preview = state.service_stop_preview("mongodb").unwrap();
        let entry = state
            .manager
            .services
            .lock()
            .get("mongodb")
            .cloned()
            .unwrap();
        let pid = preview.service.pids[0];
        let original_executable = entry
            .identities
            .lock()
            .get(&pid)
            .unwrap()
            .executable
            .clone();
        entry.identities.lock().get_mut(&pid).unwrap().executable = std::env::current_exe().unwrap();
        assert_eq!(
            state.stop_service("mongodb").unwrap_err().code,
            "MONGO_PROCESS_UNVERIFIED"
        );
        assert!(platform::process_alive(pid));
        client("read");
        entry.identities.lock().get_mut(&pid).unwrap().executable = original_executable;
        register_fixture(
            &state,
            "mongodb",
            "99",
            &temp.path().join("missing other version"),
        );
        state
            .store
            .set_setting("activemongodbVersion", "99")
            .unwrap();
        // 默认版本和端口配置已经变化，停机仍必须只针对当前实例。
        let unrelated = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        state
            .store
            .set_port_override("mongodb", Some(unrelated.local_addr().unwrap().port()))
            .unwrap();
        if std::env::var_os("NSB_MONGO_TOOLS_ARCHIVES").is_some() {
            match crate::mongodb::browse(&state, "8.0.4", crate::mongodb::BrowseRequest::Overview).unwrap() {
                crate::mongodb::BrowseResponse::Overview { port: actual, .. } => assert_eq!(actual, port),
                _ => panic!("wrong instance response"),
            }
        }
        state.stop_service("mongodb").unwrap();
        assert!(!platform::process_alive(pid));
        let log = std::fs::read_to_string(state.paths.service_log("mongodb")).unwrap();
        assert!(log.contains("mongod shutdown complete"), "{log}");
        assert!(log.contains("\"exitCode\":0"), "{log}");
        #[cfg(windows)]
        assert!(log.contains("shutdown event signaled"), "{log}");
        state
            .store
            .set_setting("activemongodbVersion", "8.0.4")
            .unwrap();
        state
            .store
            .set_port_override("mongodb", Some(port))
            .unwrap();
        state.start_service("mongodb").unwrap();
        assert_eq!(
            state
                .force_stop_service("mongodb", &preview.revision)
                .unwrap_err()
                .code,
            "SERVICE_TARGET_CHANGED"
        );
        client("read");
        state.restart_service("mongodb").unwrap();
        client("read");
        if std::env::var_os("NSB_MONGO_TOOLS_ARCHIVES").is_some() { client("restored"); }
        state.stop_service("mongodb").unwrap();
        let log = std::fs::read_to_string(state.paths.service_log("mongodb")).unwrap();
        assert!(log.matches("mongod shutdown complete").count() >= 3);
        assert!(!log.to_ascii_lowercase().contains("unclean shutdown"));
        assert!(!state.manager.watchdog.should_restart(
            "mongodb",
            &crate::watchdog::WatchdogConfig {
                enabled: true,
                ..Default::default()
            }
        ));
        state.store.set_setting("watchdogEnabled", "true").unwrap();
        assert!(state.watchdog_tick().is_empty());
        assert_eq!(
            state.manager.snapshot("mongodb").unwrap().state,
            ServiceState::Stopped
        );
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
        let root = state.paths.runtime_dir("redis", "5.0.14.1");
        std::fs::create_dir_all(&root).unwrap();
        for file in ["redis-server.exe", "redis-cli.exe", "EventLog.dll"] {
            std::fs::copy(source.join(file), root.join(file)).unwrap();
        }
        register_fixture(&state, "redis", "5.0.14.1", &root);
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
        configgen::write_redis_conf(&state.paths, "5.0.14.1", desired).unwrap();
        let config = state.paths.redis_conf("5.0.14.1");
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
        assert_eq!(state.redis_snapshot("5.0.14.1").unwrap_err().code, "REDIS_AUTH_REQUIRED");
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
        assert_eq!(state.save_redis_connection("5.0.14.1", credentials("wrong")).unwrap_err().code, "REDIS_AUTH_FAILED");
        assert!(state.store.get_setting(&crate::stats::RedisCredentials::key("5.0.14.1")).is_none());
        state.save_redis_connection("5.0.14.1", credentials("isolated-fixture-password")).unwrap();
        assert_eq!(state.redis_stats().unwrap().keys, Some(2));
        let authenticated_snapshot = state.redis_snapshot("5.0.14.1").unwrap();
        let wait_for_snapshot = |receipt: &crate::stats::RedisSnapshotReceipt, expected: &str| {
            let started = std::time::Instant::now();
            loop {
                let persistence = state.redis_persistence("5.0.14.1").unwrap();
                assert_eq!(persistence.run_id, receipt.run_id);
                assert_eq!(persistence.process_id, receipt.process_id);
                if !persistence.saving && persistence.last_save_status == expected
                    && (expected == "err" || persistence.last_save_time >= receipt.minimum_save_time) {
                    return persistence;
                }
                assert!(started.elapsed() < Duration::from_secs(20), "snapshot not confirmed: {persistence:?}");
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        assert!(!wait_for_snapshot(&authenticated_snapshot, "ok").aof_enabled);
        let authenticated_backup = state.redis_backup_create("5.0.14.1").unwrap();
        assert_eq!(authenticated_backup.kind, "snapshot");
        assert!(crate::redis_backup::list(&state.paths).unwrap().items.iter().any(|item| item.id == authenticated_backup.id));
        assert_eq!(state.redis_snapshot("7.0.0").unwrap_err().code, "REDIS_INSTANCE_CHANGED");
        assert_eq!(state.save_redis_connection("5.0.14.1", credentials("wrong")).unwrap_err().code, "REDIS_AUTH_FAILED");
        assert_eq!(crate::stats::RedisCredentials::load(&state.store, "5.0.14.1").unwrap().password, "isolated-fixture-password");
        assert!(crate::stats::RedisCredentials::load(&state.store, "7.0.0").unwrap().password.is_empty());
        assert_eq!(state.save_redis_connection("7.0.0", credentials("isolated-fixture-password")).unwrap_err().code, "REDIS_INSTANCE_CHANGED");
        let info = state.redis_connection("5.0.14.1").unwrap();
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
        state.save_redis_connection("5.0.14.1", crate::stats::RedisCredentials::default()).unwrap();
        assert_eq!(state.redis_stats().unwrap().keys, Some(2));
        assert!(!state.redis_connection("5.0.14.1").unwrap().has_password);
        assert_eq!(state.manager.snapshot("redis").unwrap().port, Some(port));
        assert_eq!(PortsProfile::from_settings(&state.store).redis, port);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), persisted);
        assert_eq!(query("maxmemory"), "maxmemory\n67108864");

        // 可视化配置保存通过原生 CONFIG GET 与实际 RDB 文件与键数回读验证。
        let before_view=state.redis_settings("5.0.14.1").unwrap();
        assert_eq!(before_view.settings.max_memory_bytes,Some(64*1024*1024));
        let mut settings=before_view.settings.clone();
        settings.max_memory_bytes=Some(96*1024*1024);settings.eviction_policy=Some("allkeys-random".into());
        settings.timeout_seconds=Some(42);settings.max_clients=Some(333);
        settings.save_rules=Some(vec![crate::redis_settings::SnapshotRule{seconds:60,changes:1},crate::redis_settings::SnapshotRule{seconds:300,changes:10}]);
        let before_file=std::fs::read_to_string(&config).unwrap();
        let saved=state.save_redis_settings("5.0.14.1",&before_view.revision,&settings,false).unwrap();
        assert_eq!(saved.settings,settings);assert_ne!(saved.revision,before_view.revision);
        assert_eq!(query("maxmemory"),"maxmemory\n67108864","save must not change the running server");
        assert_eq!(query("databases"),"databases\n32","unrelated directives preserved");
        let history=crate::cfgeditor::list_config_backups_selected(&state.paths,&state.store,Some("redis-conf@5.0.14.1")).unwrap();
        assert!(history.iter().any(|b|std::fs::read_to_string(&b.path).unwrap()==before_file));
        assert_eq!(state.save_redis_settings("5.0.14.1",&before_view.revision,&settings,false).unwrap_err().code,"CONFIG_CONFLICT");
        let mut disabled=settings.clone();disabled.save_rules=Some(vec![]);
        assert_eq!(state.save_redis_settings("5.0.14.1",&saved.revision,&disabled,false).unwrap_err().code,"REDIS_SNAPSHOT_CONFIRM");
        let customized=std::fs::read_to_string(&config).unwrap();assert!(customized.contains("databases 32"));assert!(customized.contains("appendonly no"));
        state.stop_service("redis").unwrap();state.start_service("redis").unwrap();
        assert_eq!(query("maxmemory"),"maxmemory\n100663296");assert_eq!(query("maxmemory-policy"),"maxmemory-policy\nallkeys-random");
        assert_eq!(query("timeout"),"timeout\n42");assert_eq!(query("maxclients"),"maxclients\n333");assert_eq!(query("save"),"save\n60 1 300 10");
        assert_eq!(state.redis_stats().unwrap().keys,Some(2));
        let current=state.redis_settings("5.0.14.1").unwrap();let backup=history.iter().find(|b|std::fs::read_to_string(&b.path).unwrap()==before_file).unwrap();
        state.rollback_config(&backup.name,Some("redis-conf@5.0.14.1"),Some(&std::fs::read_to_string(&config).unwrap())).unwrap();
        assert_eq!(state.save_redis_settings("5.0.14.1",&current.revision,&settings,false).unwrap_err().code,"CONFIG_CONFLICT");
        state.stop_service("redis").unwrap();state.start_service("redis").unwrap();
        assert_eq!(query("maxmemory"),"maxmemory\n67108864");assert_eq!(state.redis_stats().unwrap().keys,Some(2));
        // 手动 RDB 快照需要在停止主实例之前回读独立文件，不能由 SHUTDOWN 的保存掩盖失败。
        let cli = |args: &[&str]| {
            let output = platform::command(root.join("redis-cli.exe")).args(["-p", &port.to_string(), "--raw"]).args(args).output().unwrap();
            assert!(output.status.success());
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        let occupied_rdb = state.paths.redis_data_dir().join("occupied.rdb");
        std::fs::create_dir(&occupied_rdb).unwrap();
        assert_eq!(cli(&["CONFIG", "SET", "dbfilename", "occupied.rdb"]), "OK");
        let failed_snapshot = state.redis_snapshot("5.0.14.1").unwrap();
        assert_eq!(wait_for_snapshot(&failed_snapshot, "err").last_save_status, "err");
        assert_eq!(state.redis_stats().unwrap().keys, Some(2));
        assert_eq!(cli(&["CONFIG", "SET", "dbfilename", "dump.rdb"]), "OK");
        let before_snapshot = state.redis_persistence("5.0.14.1").unwrap();
        let manual_snapshot = state.redis_snapshot("5.0.14.1").unwrap();
        assert!(manual_snapshot.minimum_save_time > before_snapshot.last_save_time);
        let confirmed = wait_for_snapshot(&manual_snapshot, "ok");
        assert_eq!(confirmed.changes_since_save, 0);
        assert_ne!((&manual_snapshot.run_id, manual_snapshot.process_id), (&authenticated_snapshot.run_id, authenticated_snapshot.process_id));
        let restore_dir = temp.path().join("snapshot-reader");
        std::fs::create_dir(&restore_dir).unwrap();
        std::fs::copy(state.paths.redis_data_dir().join("dump.rdb"), restore_dir.join("dump.rdb")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let reader_port = listener.local_addr().unwrap().port(); drop(listener);
        struct SnapshotReader(std::process::Child);
        impl Drop for SnapshotReader { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
        let reader = platform::command(root.join("redis-server.exe"))
            .args(["--bind", "127.0.0.1", "--port", &reader_port.to_string(), "--dir", restore_dir.to_str().unwrap(), "--save", "", "--appendonly", "no", "--databases", "32"])
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
        let reader = SnapshotReader(reader);
        let started = std::time::Instant::now();
        while std::net::TcpStream::connect(("127.0.0.1", reader_port)).is_err() {
            assert!(started.elapsed() < Duration::from_secs(10)); std::thread::sleep(Duration::from_millis(100));
        }
        for database in [0, 2] {
            let output = platform::command(root.join("redis-cli.exe")).args(["-p", &reader_port.to_string(), "-n", &database.to_string(), "--raw", "GET", "isolated-fixture"]).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "1");
        }
        drop(reader);
        assert!(std::net::TcpStream::connect(("127.0.0.1", reader_port)).is_err());
        // 独立备份 -> 修改数据 -> 恢复 -> 回退，均用真实 Redis 读取逻辑数据库值。
        let backup = state.redis_backup_create("5.0.14.1").unwrap();
        assert_eq!(state.redis_restore_preview("5.0.14.1", &backup.id).unwrap_err().code, "REDIS_RESTORE_RUNNING");
        assert_eq!(cli(&["SET", "isolated-fixture", "2"]), "OK");
        state.stop_service("redis").unwrap();
        let preview = state.redis_restore_preview("5.0.14.1", &backup.id).unwrap();
        assert!(state.redis_backup_restore("5.0.14.1", &backup.id, &preview.revision, "Redis wrong").is_err());
        let original_config = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config, format!("{original_config}\n# external edit\n")).unwrap();
        assert_eq!(state.redis_backup_restore("5.0.14.1", &backup.id, &preview.revision, "Redis 5.0.14.1").unwrap_err().code, "REDIS_RESTORE_CHANGED");
        std::fs::write(&config, original_config.replace("appendonly no", "appendonly yes")).unwrap();
        assert_eq!(state.redis_restore_preview("5.0.14.1", &backup.id).unwrap_err().code, "REDIS_RESTORE_AOF");
        std::fs::write(&config, &original_config).unwrap();
        let content_path = state.paths.backup().join("redis").join(&backup.id).join("content.rdb");
        let original_backup = std::fs::read(&content_path).unwrap();
        let mut corrupt = original_backup.clone(); corrupt[10] ^= 1;
        std::fs::write(&content_path, &corrupt).unwrap();
        assert_eq!(state.redis_restore_preview("5.0.14.1", &backup.id).unwrap_err().code, "REDIS_BACKUP_CHECKSUM");
        std::fs::write(&content_path, &original_backup).unwrap();
        assert!(state.redis_restore_preview("5.0.14.1", "../escape").is_err());
        let preview = state.redis_restore_preview("5.0.14.1", &backup.id).unwrap();
        let restored = state.redis_backup_restore("5.0.14.1", &backup.id, &preview.revision, "Redis 5.0.14.1").unwrap();
        let safety = restored.safety_backup.unwrap();
        assert_eq!(std::fs::read(state.paths.redis_data_dir().join("dump.rdb")).unwrap(), original_backup);
        assert!(!state.manager.is_busy("redis"));
        state.start_service("redis").unwrap();
        assert_eq!(cli(&["GET", "isolated-fixture"]), "1");
        assert_eq!(state.redis_stats().unwrap().keys, Some(2));
        state.stop_service("redis").unwrap();
        let preview = state.redis_restore_preview("5.0.14.1", &safety.id).unwrap();
        state.redis_backup_restore("5.0.14.1", &safety.id, &preview.revision, "Redis 5.0.14.1").unwrap();
        state.start_service("redis").unwrap();
        assert_eq!(cli(&["GET", "isolated-fixture"]), "2");
        state.stop_service("redis").unwrap();
        std::fs::write(state.paths.redis_data_dir().join("dump.rdb"), b"damaged original").unwrap();
        let preview = state.redis_restore_preview("5.0.14.1", &backup.id).unwrap();
        let restored = state.redis_backup_restore("5.0.14.1", &backup.id, &preview.revision, "Redis 5.0.14.1").unwrap();
        let preserved = state.paths.backup().join("redis").join(restored.safety_backup.unwrap().id).join("content.rdb");
        assert_eq!(std::fs::read(preserved).unwrap(), b"damaged original");
        state.start_service("redis").unwrap();
        assert_eq!(cli(&["GET", "isolated-fixture"]), "1");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original_config);
        // 外部 RDB 导入只保存副本，原文件与当前实例不变；恢复后用真实 Redis 读回。
        let external = temp.path().join("external snapshot.RDB");
        std::fs::write(&external, &original_backup).unwrap();
        let before_import = crate::redis_backup::list(&state.paths).unwrap().items.len();
        let import = crate::redis_backup::inspect_import(external.to_str().unwrap()).unwrap();
        assert_eq!(import.version, "5.0.14.1"); assert_eq!(import.rdb_version, 9);
        assert_eq!(import.size_bytes, original_backup.len() as u64);
        assert_eq!(crate::redis_backup::list(&state.paths).unwrap().items.len(), before_import);
        assert!(crate::redis_backup::import_rdb(&state.paths, &import.source, "stale").is_err());
        assert_eq!(cli(&["SET", "isolated-fixture", "3"]), "OK");
        let imported = crate::redis_backup::import_rdb(&state.paths, &import.source, &import.revision).unwrap();
        assert_eq!(imported.kind, "imported"); assert_eq!(imported.sha256, import.sha256);
        assert_eq!(std::fs::read(&external).unwrap(), original_backup);
        assert_eq!(cli(&["GET", "isolated-fixture"]), "3");
        assert_eq!(crate::redis_backup::list(&state.paths).unwrap().items.len(), before_import + 1);
        let mut changed = original_backup.clone();
        let source_version_at = changed.windows(8).position(|bytes| bytes == b"5.0.14.1").unwrap();
        changed[source_version_at + 5] = b'5';
        let checksum_at = changed.len() - 8;
        let checksum = crate::redis_backup::rdb_checksum(0, &changed[..checksum_at]);
        changed[checksum_at..].copy_from_slice(&checksum.to_le_bytes());
        std::fs::write(&external, &changed).unwrap();
        assert_eq!(crate::redis_backup::inspect_import(external.to_str().unwrap()).unwrap().version, "5.0.15.1");
        assert_eq!(crate::redis_backup::import_rdb(&state.paths, &import.source, &import.revision).unwrap_err().code, "REDIS_IMPORT_CHANGED");
        std::fs::write(&external, &corrupt).unwrap();
        assert_eq!(crate::redis_backup::inspect_import(external.to_str().unwrap()).unwrap_err().code, "REDIS_BACKUP_CHECKSUM");
        assert!(crate::redis_backup::import_rdb(&state.paths, &import.source, &import.revision).is_err());
        // 无版本标记、超大声明长度、非 RDB 与目录联接均不能绕过导入检查。
        for payload in [b"REDIS0009\xff".to_vec(), b"REDIS0009\xfa\x80\xff\xff\xff\xff".to_vec()] {
            let mut bytes = payload; bytes.extend_from_slice(&[0; 8]);
            std::fs::write(&external, bytes).unwrap();
            assert!(crate::redis_backup::inspect_import(external.to_str().unwrap()).is_err());
        }
        assert!(crate::redis_backup::inspect_import(config.to_str().unwrap()).is_err());
        std::fs::write(&external, &original_backup).unwrap();
        #[cfg(windows)] {
            let linked_dir = temp.path().join("rdb junction");
            let outside = temp.path().join("outside import"); std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("dump.rdb"), &original_backup).unwrap();
            assert!(platform::command("cmd").args(["/c", "mklink", "/J"]).arg(&linked_dir).arg(&outside).output().unwrap().status.success());
            assert!(crate::redis_backup::inspect_import(linked_dir.join("dump.rdb").to_str().unwrap()).is_err());
            std::fs::remove_dir(linked_dir).unwrap();
        }
        state.stop_service("redis").unwrap();
        for directive in ["preload-file rdb:/tmp/other.rdb", "replicaof 127.0.0.1 6379", "slaveof 127.0.0.1 6379"] {
            std::fs::write(&config, format!("{original_config}\n{directive}\n")).unwrap();
            assert_eq!(state.redis_restore_preview("5.0.14.1", &imported.id).unwrap_err().code, "REDIS_RESTORE_SOURCE");
        }
        std::fs::write(&config, format!("{original_config}\npreload-file \"\"\n")).unwrap();
        assert!(state.redis_restore_preview("5.0.14.1", &imported.id).is_ok());
        std::fs::write(&config, &original_config).unwrap();
        let preview = state.redis_restore_preview("5.0.14.1", &imported.id).unwrap();
        state.redis_backup_restore("5.0.14.1", &imported.id, &preview.revision, "Redis 5.0.14.1").unwrap();
        assert!(!state.manager.is_busy("redis"));
        state.start_service("redis").unwrap();
        assert_eq!(cli(&["GET", "isolated-fixture"]), "1");
        assert_eq!(state.redis_stats().unwrap().keys, Some(2));
        // 文件管理只影响选中的备份；导出可再次导入，不覆盖已有目标或当前实例数据。
        let exported_rdb = temp.path().join("exported snapshot.rdb");
        state.redis_backup_export(&imported.id, exported_rdb.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(&exported_rdb).unwrap(), original_backup);
        assert_eq!(state.redis_backup_export(&imported.id, exported_rdb.to_str().unwrap()).unwrap_err().code, "REDIS_EXPORT_EXISTS");
        assert!(state.redis_backup_export(&imported.id, state.paths.base.join("not-an-export.rdb").to_str().unwrap()).is_err());
        let exported_scope = crate::redis_backup::inspect_import(exported_rdb.to_str().unwrap()).unwrap();
        assert_eq!(exported_scope.version, "5.0.14.1");
        let make_copy = || crate::redis_backup::import_rdb(&state.paths, &exported_scope.source, &exported_scope.revision).unwrap();
        let damaged = make_copy();
        let damaged_dir = state.paths.backup().join("redis").join(&damaged.id);
        std::fs::write(damaged_dir.join("metadata.json"), "broken record").unwrap();
        let listed = crate::redis_backup::list(&state.paths).unwrap();
        let listed = listed.items.iter().find(|entry| entry.id == damaged.id).unwrap();
        assert!(listed.version.is_none() && listed.problem.is_some());
        let scope = state.redis_backup_removal_preview(&damaged.id).unwrap();
        std::fs::write(damaged_dir.join("content.rdb"), "changed after preview").unwrap();
        assert_eq!(state.redis_backup_delete(&damaged.id, &scope.revision).unwrap_err().code, "REDIS_BACKUP_CHANGED");
        assert!(damaged_dir.is_dir());
        let scope = state.redis_backup_removal_preview(&damaged.id).unwrap();
        state.redis_backup_delete(&damaged.id, &scope.revision).unwrap();
        assert!(!damaged_dir.exists());
        assert_eq!(cli(&["GET", "isolated-fixture"]), "1");
        assert!(content_path.is_file() && exported_rdb.is_file());
        let missing = make_copy();
        let missing_dir = state.paths.backup().join("redis").join(&missing.id);
        std::fs::remove_file(missing_dir.join("content.rdb")).unwrap();
        let listed = crate::redis_backup::list(&state.paths).unwrap();
        let listed = listed.items.iter().find(|entry| entry.id == missing.id).unwrap();
        assert!(listed.size_bytes.is_none() && listed.problem.is_some());
        let scope = state.redis_backup_removal_preview(&missing.id).unwrap();
        state.redis_backup_delete(&missing.id, &scope.revision).unwrap();
        assert!(!missing_dir.exists());
        let extra = make_copy();
        let extra_dir = state.paths.backup().join("redis").join(&extra.id);
        std::fs::write(extra_dir.join("keep.txt"), "unrelated").unwrap();
        assert_eq!(state.redis_backup_removal_preview(&extra.id).unwrap_err().code, "REDIS_BACKUP_EXTRA_FILES");
        assert_eq!(std::fs::read_to_string(extra_dir.join("keep.txt")).unwrap(), "unrelated");
        std::fs::remove_file(extra_dir.join("keep.txt")).unwrap();
        std::fs::write(extra_dir.join("content.rdb"), &corrupt).unwrap();
        let rejected_export = temp.path().join("not-published.rdb");
        assert!(state.redis_backup_export(&extra.id, rejected_export.to_str().unwrap()).is_err());
        assert!(!rejected_export.exists());
        let scope = state.redis_backup_removal_preview(&extra.id).unwrap();
        {
            let _locked = state.manager.lifecycle.lock();
            std::thread::scope(|threads| {
                threads.spawn(|| {
                    assert_eq!(state.redis_backup_delete(&extra.id, &scope.revision).unwrap_err().code, "SERVICE_BUSY");
                    assert_eq!(state.redis_backup_export(&imported.id, rejected_export.to_str().unwrap()).unwrap_err().code, "SERVICE_BUSY");
                }).join().unwrap();
            });
        }
        state.redis_backup_delete(&extra.id, &scope.revision).unwrap();
        assert!(state.redis_backup_removal_preview("../outside").is_err());
        assert_eq!(std::fs::read(&exported_rdb).unwrap(), original_backup);
        assert_eq!(cli(&["GET", "isolated-fixture"]), "1");
        let password_scope = state.redis_password("5.0.14.1").unwrap();
        assert!(!password_scope.enabled && password_scope.blocked_reason.is_none());
        assert_eq!(state.save_redis_password("5.0.14.1", &password_scope.revision, "new-password", false).unwrap_err().code, "REDIS_PASSWORD_RUNNING");
        assert_eq!(state.stop_redis_for_password("7.4.0").unwrap_err().code, "REDIS_INSTANCE_CHANGED");
        let pids = state.manager.snapshot("redis").unwrap().pids;
        state.stop_redis_for_password("5.0.14.1").unwrap();
        assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        let original_config = std::fs::read_to_string(&config).unwrap();
        let password = "fixture # quote\" and slash\\ value";
        let saved = state.save_redis_password("5.0.14.1", &password_scope.revision, password, false).unwrap();
        assert!(saved.connection_saved && saved.view.enabled);
        assert_eq!(crate::stats::RedisCredentials::load(&state.store,"5.0.14.1").unwrap().password, password);
        assert!(!serde_json::to_string(&saved).unwrap().contains(password));
        assert_eq!(state.save_redis_password("5.0.14.1", &password_scope.revision, password, false).unwrap_err().code, "CONFIG_CONFLICT");
        let history = crate::cfgeditor::list_config_backups_selected(&state.paths, &state.store, Some("redis-conf@5.0.14.1")).unwrap();
        assert!(history.iter().any(|entry| std::fs::read_to_string(&entry.path).ok().as_deref() == Some(original_config.as_str())));
        state.start_service("redis").unwrap();
        assert_eq!(state.redis_stats().unwrap().keys,Some(2));
        assert_eq!(crate::stats::redis_stats_authenticated(port,&credentials(""),None).unwrap_err().code,"REDIS_AUTH_REQUIRED");
        assert_eq!(crate::stats::redis_stats_authenticated(port,&credentials("wrong"),None).unwrap_err().code,"REDIS_AUTH_FAILED");
        let read_back = platform::command(root.join("redis-cli.exe")).env("REDISCLI_AUTH",password)
            .args(["-p",&port.to_string(),"--raw","GET","isolated-fixture"]).output().unwrap();
        assert!(read_back.status.success());
        assert_eq!(String::from_utf8_lossy(&read_back.stdout).trim(),"1");
        state.stop_service("redis").unwrap();
        let scope = state.redis_password("5.0.14.1").unwrap();
        let before = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config,format!("{before}# external edit\n")).unwrap();
        assert_eq!(state.save_redis_password("5.0.14.1",&scope.revision,"",true).unwrap_err().code,"CONFIG_CONFLICT");
        let scope = state.redis_password("5.0.14.1").unwrap();
        assert_eq!(state.save_redis_password("5.0.14.1",&scope.revision,"",false).unwrap_err().code,"REDIS_PASSWORD_CONFIRM");
        let disabled = state.save_redis_password("5.0.14.1",&scope.revision,"",true).unwrap();
        assert!(!disabled.view.enabled && disabled.connection_saved);
        state.start_service("redis").unwrap();
        assert_eq!(state.redis_stats().unwrap().keys,Some(2));
        assert_eq!(cli(&["GET","isolated-fixture"]),"1");
        state.stop_service("redis").unwrap();
        let disabled_config = std::fs::read_to_string(&config).unwrap();
        for extra in ["include extra.conf\n","aclfile users.acl\n","user default on nopass ~* +@all\n"] {
            std::fs::write(&config,format!("{disabled_config}{extra}")).unwrap();
            let view = state.redis_password("5.0.14.1").unwrap();
            assert!(view.blocked_reason.is_some());
            assert_eq!(state.save_redis_password("5.0.14.1",&view.revision,password,false).unwrap_err().code,"REDIS_PASSWORD_COMPLEX");
            assert!(crate::stats::RedisCredentials::load(&state.store,"5.0.14.1").unwrap().password.is_empty());
        }
        std::fs::write(&config, &disabled_config).unwrap();
        let view = state.redis_password("5.0.14.1").unwrap();
        // 仅隔离 SQLite 夹具注入记录写入失败，确认不把部分保存报告为全部成功。
        let db = rusqlite::Connection::open(state.paths.db()).unwrap();
        db.execute_batch("CREATE TRIGGER reject_redis_credentials BEFORE INSERT ON settings WHEN NEW.key = 'redisConnection@5.0.14.1' BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END;").unwrap();
        let partial = state.save_redis_password("5.0.14.1",&view.revision,password,false).unwrap();
        assert!(partial.view.enabled && !partial.connection_saved);
        db.execute_batch("DROP TRIGGER reject_redis_credentials;").unwrap();
        assert!(state.save_redis_password("5.0.14.1",&partial.view.revision,password,false).unwrap().connection_saved);
        {
            let _locked = state.manager.lifecycle.lock();
            std::thread::scope(|threads| threads.spawn(|| {
                assert_eq!(state.save_redis_password("5.0.14.1",&partial.view.revision,password,false).unwrap_err().code,"SERVICE_BUSY");
            }).join().unwrap());
        }
        state.start_service("redis").unwrap();
        assert_eq!(state.redis_stats().unwrap().keys,Some(2));
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
            assert_eq!(other.service_stop_preview("missing").unwrap_err().code, "SERVICE_BUSY");
            assert_eq!(other.force_stop_service("missing", "stale").unwrap_err().code, "SERVICE_BUSY");
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
        state.manager.watchdog.note_started("redis");
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
    fn database_shutdown_failures_preserve_processes_and_block_restart() {
        for (id, package) in [("mysql@8.0.46", "mysql"), ("postgresql", "postgresql")] {
            let temp = tempfile::tempdir().unwrap();
            let state = isolated_state(Paths::new(temp.path().to_path_buf()));
            let runtime = state.paths.runtime_dir(package, "8.0.46");
            let server = runtime
                .join(if package == "mysql" {
                    mysql_root_name("8.0.46")
                } else {
                    "pgsql".into()
                })
                .join("bin")
                .join(exe_name(if package == "mysql" {
                    "mysqld"
                } else {
                    "postgres"
                }));
            std::fs::create_dir_all(server.parent().unwrap()).unwrap();
            std::fs::write(server, "unused server fixture").unwrap();
            register_fixture(&state, package, "8.0.46", &runtime);
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            state.manager.register(
                id,
                package,
                Some("8.0.46".into()),
                Some("database".into()),
                Some(port),
                state.paths.service_log(id),
            );
            state.manager.adopt(id, &[std::process::id()], Some(port));
            if package == "postgresql" {
                let data = state.paths.postgres_data_dir("8.0.46");
                std::fs::create_dir_all(&data).unwrap();
                std::fs::write(
                    data.join("postmaster.pid"),
                    format!("{}\n", std::process::id()),
                )
                .unwrap();
            }
            assert_eq!(
                state.stop_service(id).unwrap_err().code,
                "DATABASE_SHUTDOWN_FAILED"
            );
            assert_eq!(
                state.manager.snapshot(id).unwrap().pids,
                [std::process::id()]
            );
            assert!(platform::process_alive(std::process::id()));
            assert_eq!(
                state.restart_service(id).unwrap_err().code,
                "DATABASE_SHUTDOWN_FAILED"
            );
            assert_eq!(
                state.manager.snapshot(id).unwrap().state,
                ServiceState::Error
            );
            assert!(state.watchdog_tick().is_empty());
            if package == "postgresql" {
                std::fs::write(
                    state
                        .paths
                        .postgres_data_dir("8.0.46")
                        .join("postmaster.pid"),
                    "1\n",
                )
                .unwrap();
                assert_eq!(
                    state.stop_service(id).unwrap_err().code,
                    "POSTGRES_PID_UNVERIFIED"
                );
            } else {
                state
                    .manager
                    .set_started_port(id, port.checked_sub(1).unwrap());
                assert_eq!(
                    state.stop_service(id).unwrap_err().code,
                    "DATABASE_OWNER_UNVERIFIED"
                );
            }
            assert!(platform::process_alive(std::process::id()));
        }
    }

    #[test]
    fn mongodb_normal_stop_rejects_other_executables_without_killing() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        let runtime = temp.path().join("runtime");
        std::fs::create_dir_all(runtime.join("bin")).unwrap();
        std::fs::write(runtime.join("bin").join(exe_name("mongod")), "not executed").unwrap();
        register_fixture(&state, "mongodb", "8.0.4", &runtime);
        register_services(&state.paths, &state.store, &state.manager);
        // 使用当前验证进程证明失败后仍存活，路径核对必须在发送任何信号之前拒绝。
        state.manager.adopt("mongodb", &[std::process::id()], None);
        assert_eq!(
            state.stop_service("mongodb").unwrap_err().code,
            "MONGO_PROCESS_UNVERIFIED"
        );
        assert_eq!(
            state.restart_service("mongodb").unwrap_err().code,
            "MONGO_PROCESS_UNVERIFIED"
        );
        let snapshot = state.manager.snapshot("mongodb").unwrap();
        assert_eq!(snapshot.pids, [std::process::id()]);
        assert_eq!(snapshot.state, ServiceState::Error);
        assert!(platform::process_alive(std::process::id()));
    }

    #[test]
    fn force_stop_requires_current_preview_and_suppresses_watchdog() {
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let sleeper = || {
            let mut command = if cfg!(windows) {
                platform::command("powershell.exe")
            } else {
                platform::command("sh")
            };
            if cfg!(windows) {
                command.args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 30",
                ]);
            } else {
                command.args(["-c", "sleep 30"]);
            }
            ChildGuard(
                command
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        };
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        state.manager.register(
            "fixture",
            "Fixture",
            Some("1".into()),
            None,
            None,
            state.paths.service_log("fixture"),
        );
        let mut first = sleeper();
        state.manager.adopt("fixture", &[first.0.id()], None);
        let old = state.service_stop_preview("fixture").unwrap();
        assert_eq!(old.service.pids, [first.0.id()]);
        first.0.kill().unwrap();
        first.0.wait().unwrap();
        let mut next = sleeper();
        state.manager.adopt("fixture", &[next.0.id()], None);
        assert_eq!(
            state
                .force_stop_service("fixture", &old.revision)
                .unwrap_err()
                .code,
            "SERVICE_TARGET_CHANGED"
        );
        assert!(next.0.try_wait().unwrap().is_none());
        let preview = state.service_stop_preview("fixture").unwrap();
        assert_ne!(preview.revision, old.revision);
        assert_eq!(
            state.force_stop_service("fixture", "").unwrap_err().code,
            "SERVICE_TARGET_CHANGED"
        );
        state
            .force_stop_service("fixture", &preview.revision)
            .unwrap();
        assert!(next.0.try_wait().unwrap().is_some());
        assert_eq!(
            state.manager.snapshot("fixture").unwrap().state,
            ServiceState::Stopped
        );
        assert!(!state.manager.watchdog.should_restart(
            "fixture",
            &crate::watchdog::WatchdogConfig {
                enabled: true,
                ..Default::default()
            }
        ));
        state
            .manager
            .recovery
            .lock()
            .blocked_services
            .push("fixture".into());
        assert_eq!(
            state.service_stop_preview("fixture").unwrap_err().code,
            "PROCESS_RECOVERY_UNVERIFIED"
        );
    }

    #[test]
    fn service_lifecycle_reports_start_failure_after_successful_stop() {
        let temp = tempfile::tempdir().unwrap();
        let state = isolated_state(Paths::new(temp.path().to_path_buf()));
        register_fixture(&state, "nginx", "1", &temp.path().join("missing-runtime"));
        register_services(&state.paths, &state.store, &state.manager);
        state.manager.watchdog.note_started("nginx");
        let error = state.restart_service("nginx").unwrap_err();
        assert!(error.message.contains("服务已停止，但重新启动失败"));
        assert_ne!(error.code, "UNKNOWN_SERVICE");
        let snapshot = state.manager.snapshot("nginx").unwrap();
        assert_eq!(snapshot.state, ServiceState::Error);
        assert!(snapshot.pids.is_empty());
        let cfg = crate::watchdog::WatchdogConfig { enabled: true, ..Default::default() };
        assert!(!state.manager.watchdog.should_restart("nginx", &cfg));
        assert!(state.paths.data().join("run/pids.json").exists());
        // 已无进程的 Error 也应被主动停止抑制，批量重启失败不能重新启用看门狗。
        state.manager.watchdog.note_started("nginx");
        let stopped = state.bulk_stop(&["nginx".into()]).unwrap();
        assert_eq!(stopped.already, ["nginx"]);
        assert!(!state.manager.watchdog.should_restart("nginx", &cfg));
        let restarted = state.bulk_restart(&["nginx".into()]).unwrap();
        assert!(restarted.failed[0].error.message.contains("重新启动失败"));
        assert!(!state.manager.watchdog.should_restart("nginx", &cfg));
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
        assert!(state.manager.watchdog.should_restart("fixture-a", &cfg));
        let mut freed = Vec::new();
        state.start_service_with_port_policy("fixture-b", |p| freed.push(p)).unwrap();
        assert_eq!(freed, [port]);
        assert_eq!(state.manager.snapshot("fixture-a").unwrap().state, ServiceState::Stopped);
        assert!(!state.manager.watchdog.should_restart("fixture-a", &cfg));
        let before = state.manager.snapshot("fixture-b").unwrap().pids;
        state.restart_service("fixture-b").unwrap();
        let after = state.manager.snapshot("fixture-b").unwrap();
        assert_eq!(after.state, ServiceState::Running);
        assert_ne!(before, after.pids);
        assert!(before.iter().all(|pid| !platform::process_alive(*pid)));
        assert!(state.manager.watchdog.should_restart("fixture-b", &cfg));
        let recorded: serde_json::Value = serde_json::from_slice(&std::fs::read(state.paths.data().join("run/pids.json")).unwrap()).unwrap();
        assert!(recorded["services"].as_array().unwrap().iter().any(|row| row["id"] == "fixture-b" && row["pids"][0] == after.pids[0]));
        let scan = state.scan_port_range(port, port).unwrap();
        let outcome = state.close_port_checked(port, &scan.listeners).unwrap();
        assert!(outcome.port_free);
        assert!(!state.manager.watchdog.should_restart("fixture-b", &cfg));

        // 同一真实临时实例覆盖整栈、批量和托盘全部停止所用的核心入口。
        let stack = state.save_stack(crate::model::StackInput {
            id: None, name: "Lifecycle fixture".into(), description: String::new(),
            items: vec![crate::model::StackItem { service_id: "fixture-b".into(), label: None, order: 0 }],
        }).unwrap();
        assert_eq!(state.start_stack(&stack.id).unwrap().started, ["fixture-b"]);
        let stack_pid = state.manager.snapshot("fixture-b").unwrap().pids;
        assert_eq!(state.start_stack(&stack.id).unwrap().already_running, ["fixture-b"]);
        assert_eq!(state.manager.snapshot("fixture-b").unwrap().pids, stack_pid);
        assert!(state.manager.watchdog.should_restart("fixture-b", &cfg));
        assert_eq!(state.stop_stack(&stack.id).unwrap().started, ["fixture-b"]);
        assert!(!state.manager.watchdog.should_restart("fixture-b", &cfg));
        let report = state.bulk_start(&["fixture-b".into(), "missing".into(), "fixture-b".into()]).unwrap();
        assert_eq!(report.succeeded, ["fixture-b"], "{report:?}");
        assert_eq!(report.failed[0].service_id, "missing");
        assert!(state.manager.watchdog.should_restart("fixture-b", &cfg));
        let before = state.manager.snapshot("fixture-b").unwrap().pids;
        let report = state.bulk_restart(&["fixture-b".into()]).unwrap();
        assert_eq!(report.succeeded, ["fixture-b"], "{report:?}");
        assert_ne!(state.manager.snapshot("fixture-b").unwrap().pids, before);
        assert!(before.iter().all(|pid| !platform::process_alive(*pid)));
        assert!(state.manager.watchdog.should_restart("fixture-b", &cfg));
        let listeners = state.scan_port_range(port, port).unwrap().listeners;
        assert!(!listeners.is_empty());
        assert_eq!(state.stop_all_services().unwrap().succeeded, ["fixture-b"]);
        assert!(!state.manager.watchdog.should_restart("fixture-b", &cfg));
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
        cached.manager.watchdog.note_started("handoff-probe");
        assert!(cached.manager.watchdog.should_restart("handoff-probe",&cached.watchdog_config()));
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
