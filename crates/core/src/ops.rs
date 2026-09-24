//! 服务启停编排：按服务类型组装 SpawnSpec、健康等待、优雅停止、站点重载。

use crate::configgen;
use crate::error::{AppError, Result};
use crate::model::ServiceState;
use crate::paths::{nginx_path, Paths};
use crate::services::*;
use crate::store::Store;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// 注册所有已安装服务（应用启动/安装后调用）。
/// 只有「守护进程型」套件注册为服务；node/python/go/composer 等纯运行时不注册。
/// 内置编排的服务（nginx/mysql 等）走这里；其余清单声明 `run` 的包走 `generic::register_services`。
pub fn register_services(paths: &Paths, store: &Store, manager: &Arc<ServiceManager>) {
    let Ok(installed) = store.list_installed() else {
        return;
    };
    let ports = PortsProfile::from_settings(store);
    const SERVICE_IDS: &[&str] = &[
        "nginx", "apache", "php", "mysql", "postgresql", "mongodb", "redis", "mihomo",
    ];
    for p in installed {
        if !SERVICE_IDS.contains(&p.id.as_str()) {
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
    let inst = store
        .find_installed("php", Some(version))
        .ok_or_else(|| AppError::new("NOT_INSTALLED", format!("PHP {version} 尚未安装"))
            .with_hint("到「套件 / 服务」页安装该版本"))?;
    let exe = PathBuf::from(&inst.install_path).join(exe_name("php-cgi"));
    if !exe.exists() {
        return Err(AppError::new("BROKEN_INSTALL", "找不到 php-cgi"));
    }
    Ok(exe)
}

fn nginx_exe(store: &Store) -> Result<(PathBuf, PathBuf)> {
    let inst = store
        .find_installed("nginx", None)
        .ok_or_else(|| AppError::not_installed("Nginx"))?;
    let root = PathBuf::from(&inst.install_path).join(format!("nginx-{}", inst.version));
    let exe_name = if cfg!(windows) { "nginx.exe" } else { "nginx" };
    let exe = root.join(exe_name);
    if !exe.exists() {
        return Err(AppError::new("BROKEN_INSTALL", format!("找不到 {exe_name}，套件可能损坏"))
            .with_hint("在套件页卸载后重新安装 Nginx"));
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
    let mut list = store.list_installed().ok()?
        .into_iter()
        .filter(|p| p.id == id)
        .collect::<Vec<_>>();
    list.sort_by(|a, b| b.version.cmp(&a.version));
    list.into_iter().next()
}

/// 套件页「切换使用版本」：仅已装版本可切换
pub fn set_active_version(store: &Store, id: &str, version: &str) -> Result<()> {
    let inst = store
        .find_installed(id, Some(version))
        .ok_or_else(|| AppError::not_installed(&format!("{id} {version}"))
            .with_hint("先安装该版本再切换"))?;
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
        .or_else(|| installed_by_choice(store, "mysql"))
        .ok_or_else(|| AppError::not_installed("MySQL"))?;
    let basedir = PathBuf::from(&inst.install_path).join(mysql_root_name(version));
    let mysqld = basedir.join("bin").join(exe_name("mysqld"));
    if !mysqld.exists() {
        return Err(AppError::new("BROKEN_INSTALL", "找不到 mysqld"));
    }
    Ok((basedir, mysqld))
}

fn redis_paths(store: &Store) -> Result<(PathBuf, PathBuf)> {
    let inst = installed_by_choice(store, "redis")
        .ok_or_else(|| AppError::not_installed("Redis"))?;
    let dir = PathBuf::from(&inst.install_path);
    let exe = dir.join(exe_name("redis-server"));
    Ok((dir, exe))
}

fn mihomo_paths(store: &Store) -> Result<(PathBuf, PathBuf)> {
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

fn apache_paths(store: &Store) -> Result<(PathBuf, PathBuf)> {
    let inst = installed_by_choice(store, "apache")
        .ok_or_else(|| AppError::not_installed("Apache"))?;
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
        return Err(AppError::new("BROKEN_INSTALL", "找不到 postgres 可执行文件"));
    }
    Ok((root, exe))
}

fn mongodb_paths(store: &Store) -> Result<(PathBuf, PathBuf)> {
    let inst = installed_by_choice(store, "mongodb")
        .ok_or_else(|| AppError::not_installed("MongoDB"))?;
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

pub fn start_service(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>, id: &str) -> Result<()> {
    if let Some(e) = manager.snapshot(id) {
        if e.state == ServiceState::Running || e.state == ServiceState::Starting {
            return Ok(());
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
        s if s.starts_with("php@") => start_php(store, paths, manager, s.trim_start_matches("php@"), &ports),
        s if s.starts_with("mysql@") => start_mysql(store, paths, manager, s.trim_start_matches("mysql@"), &ports),
        // 清单声明 `run` 的包（Caddy/Meilisearch/MinIO/Mailpit …）走通用路径
        other => crate::generic::start(store, paths, manager, other, &ports),
    };

    match result {
        Ok(()) => {
            manager.set_state(id, ServiceState::Running);
            // 记录本次实际绑定的端口，停机命令据此寻址（端口方案可能在运行期被改）
            let actual_port = match id {
                "nginx" => Some(ports.http),
                "apache" => Some(ports.apache_http),
                "redis" => Some(ports.redis),
                "postgresql" => Some(ports.postgres),
                "mongodb" => Some(ports.mongodb),
                "mihomo" => Some(configgen::MIHOMO_MIXED_PORT),
                s if s.starts_with("mysql@") => Some(ports.mysql),
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
        Err(err) => {
            manager.set_error(id, err.clone());
            Err(err)
        }
    }
}

fn start_nginx(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>, ports: &PortsProfile) -> Result<()> {
    let (root, exe) = nginx_exe(store)?;
    precheck_port(ports.http, "Nginx")?;
    // 默认 server 块无条件 listen https（见 configgen::render_nginx_conf），
    // 端口被占则 nginx 直接 bind 失败，必须一并预检
    if ports.https != ports.http {
        precheck_port(ports.https, "Nginx (HTTPS)")?;
    }

    // 主配置：包含所有 php 池（运行中的才写 upstream）
    let pools: Vec<(String, u16)> = store
        .all_port_assigns()
        .into_iter()
        .filter(|(sid, _)| sid.starts_with("php@"))
        .map(|(sid, base)| (sid.trim_start_matches("php@").to_string(), base))
        .collect();
    configgen::write_nginx_conf(paths, &root, &pools, ports.http, ports.https)?;
    configgen::validate_nginx(&exe, &paths.nginx_conf())?;

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
        return Err(AppError::new("NGINX_START_TIMEOUT", "Nginx 启动超时（端口未就绪）")
            .with_hint("查看日志页 nginx 的最后输出；常见原因是配置错误或端口冲突"));
    }
    Ok(())
}

fn start_php(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>, version: &str, _ports: &PortsProfile) -> Result<()> {
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
        return Err(AppError::new("PHP_START_FAILED", format!("PHP {version} 进程启动失败")
            .clone())
            .with_hint("查看日志；常见原因是 php.ini 扩展加载失败或缺少 VC 运行库"));
    }
    // nginx 运行中则热加载新 upstream
    if manager.snapshot("nginx").map(|s| s.state == ServiceState::Running).unwrap_or(false) {
        let _ = reload_nginx(store, paths);
    }
    Ok(())
}

fn start_mysql(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>, version: &str, ports: &PortsProfile) -> Result<()> {
    let service_id = format!("mysql@{version}");
    let (basedir, mysqld) = mysql_paths(store, version)?;
    // 端口被占 + 自动回落开启 → 换附近空闲端口（写入覆盖项；ini 每次启动重写，自动跟上）
    let mysql_port = crate::services::fallback_port_for(store, "mysql", ports.mysql, &[])
        .unwrap_or(ports.mysql);
    // 每次启动前重写 ini（镜像策略/端口方案可能变化；写前自动备份）
    configgen::write_mysql_ini(paths, version, &basedir, mysql_port)?;
    let datadir = paths.mysql_data_dir(version);
    if !datadir.exists() || std::fs::read_dir(&datadir).map(|mut d| d.next().is_none()).unwrap_or(true) {
        // 首次初始化（insecure → 启动后设密）
        std::fs::create_dir_all(&datadir)?;
        let ini = paths.mysql_ini(version);
        let mut init_args: Vec<String> = vec![
            format!("--defaults-file={}", ini.to_string_lossy()),
            "--initialize-insecure".to_string(),
        ];
        // --console 是 Windows 专属选项，macOS 上传入会以 unknown option 直接失败
        if cfg!(windows) {
            init_args.push("--console".to_string());
        }
        let out = std::process::Command::new(&mysqld)
            .args(&init_args)
            .output()
            .map_err(|e| AppError::io("初始化 MySQL 数据目录", e))?;
        if !out.status.success() {
            // 清理半初始化目录，避免下次误判为已初始化
            let _ = std::fs::remove_dir_all(&datadir);
            return Err(AppError::new("MYSQL_INIT_FAILED", "MySQL 数据目录初始化失败")
                .with_hint("检查磁盘空间；数据目录路径不要包含中文或空格")
                .with_detail(format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))));
        }
    }

    precheck_port(mysql_port, "MySQL")?;
    let ini = paths.mysql_ini(version);
    let mut mysql_args: Vec<String> = vec![format!("--defaults-file={}", ini.to_string_lossy())];
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
    if !wait_healthy(ports.mysql, Duration::from_secs(30)) {
        return Err(AppError::new("MYSQL_START_TIMEOUT", "MySQL 启动超时（30s 内端口未就绪）")
            .with_hint("首次启动需要初始化，可能较慢；持续失败请看日志页错误输出"));
    }

    // 首次设置 root 密码（若未设置过）
    if store.get_setting("mysqlRootPassword").is_none() {
        let default_pass = "root";
        let client = crate::dbadmin::MySqlClient::from_state(paths, version, ports.mysql, String::new());
        if client.ping().is_ok() {
            // insecure 模式空密码可连，直接设置默认密码
            if set_root_password_via(paths, version, ports.mysql, "", default_pass).is_ok() {
                store.set_setting("mysqlRootPassword", default_pass)?;
            }
        }
    }
    Ok(())
}

fn set_root_password_via(paths: &Paths, version: &str, port: u16, old: &str, new: &str) -> Result<()> {
    let inst = paths.runtime_dir("mysql", version).join(mysql_root_name(version));
    let exe = inst.join("bin").join(exe_name("mysql"));
    let out = std::process::Command::new(&exe)
        .args([
            "-h", "127.0.0.1", "-P", &port.to_string(),
            "-u", "root", &format!("--password={old}"),
            "-e", &format!(
                "ALTER USER 'root'@'localhost' IDENTIFIED BY '{new}'; ALTER USER 'root'@'127.0.0.1' IDENTIFIED BY '{new}'; FLUSH PRIVILEGES;"
            ),
        ])
        .output()
        .map_err(|e| AppError::io("设置 root 密码", e))?;
    if !out.status.success() {
        return Err(AppError::new("MYSQL_PASSWORD_FAILED", "MySQL root 密码设置失败")
            .with_detail(String::from_utf8_lossy(&out.stderr).to_string()));
    }
    Ok(())
}

fn start_redis(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>, ports: &PortsProfile) -> Result<()> {
    let (_, exe) = redis_paths(store)?;
    let version = installed_by_choice(store, "redis").map(|p| p.version).unwrap_or_default();
    // 端口被占 + 自动回落开启 → 换附近空闲端口（写入覆盖项，重启稳定）
    let redis_port = crate::services::fallback_port_for(store, "redis", ports.redis, &[])
        .unwrap_or(ports.redis);
    configgen::write_redis_conf(paths, &version, redis_port)?;
    precheck_port(redis_port, "Redis")?;
    let conf = paths.redis_conf(&version);
    let spec = SpawnSpec {
        program: exe.clone(),
        args: vec![conf.to_string_lossy().to_string()],
        cwd: Some(exe.parent().map(PathBuf::from).unwrap_or_default()),
        env: vec![],
        detached: None,
    };
    spawn_tracked(manager, "redis", &spec)?;
    if !wait_healthy(redis_port, Duration::from_secs(10)) {
        return Err(AppError::new("REDIS_START_TIMEOUT", "Redis 启动超时")
            .with_hint("查看日志页 redis 输出；通常是端口冲突"));
    }
    Ok(())
}

fn start_mihomo(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>) -> Result<()> {
    let (_, exe) = mihomo_paths(store)?;
    if !paths.mihomo_config().exists() {
        configgen::write_mihomo_config(paths, &configgen::render_mihomo_builtin_config())?;
    }
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
    if !wait_healthy(configgen::MIHOMO_CONTROLLER_PORT, Duration::from_secs(10)) {
        return Err(AppError::new("MIHOMO_START_TIMEOUT", "mihomo 内核启动超时")
            .with_hint("查看日志页 mihomo 输出；配置损坏时可在代理页删除订阅恢复内置配置"));
    }
    Ok(())
}

fn start_apache(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>, ports: &PortsProfile) -> Result<()> {
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
        return Err(AppError::new("APACHE_START_TIMEOUT", "Apache 启动超时（端口未就绪）")
            .with_hint("查看日志页 apache 输出；常见原因是端口冲突或缺少 VC 运行库"));
    }
    Ok(())
}

fn start_postgresql(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>, ports: &PortsProfile) -> Result<()> {
    let (root, _exe) = postgres_paths(store)?;
    let version = installed_by_choice(store, "postgresql").map(|p| p.version).unwrap_or_default();
    let datadir = paths.postgres_data_dir(&version);
    let initdb = root.join("bin").join(exe_name("initdb"));
    let needs_init = !datadir.exists()
        || std::fs::read_dir(&datadir).map(|mut d| d.next().is_none()).unwrap_or(true);
    if needs_init {
        std::fs::create_dir_all(&datadir)?;
        // macOS 的 initdb 拒绝以 root 用户运行，且 -U 指定的是数据库超级用户；
        // 两端统一用 postgres（Windows 上 pg_ctl 也不认 root 以外的惯例名）
        let out = std::process::Command::new(&initdb)
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
            return Err(AppError::new("PG_INIT_FAILED", "PostgreSQL 数据目录初始化失败")
                .with_detail(String::from_utf8_lossy(&out.stderr).to_string()));
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
        return Err(AppError::new("PG_START_TIMEOUT", "PostgreSQL 启动超时（20s 内端口未就绪）")
            .with_hint("查看日志页 postgresql 输出；首次初始化可能较慢"));
    }
    Ok(())
}

fn start_mongodb(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>, ports: &PortsProfile) -> Result<()> {
    let (dir, exe) = mongodb_paths(store)?;
    let version = installed_by_choice(store, "mongodb").map(|p| p.version).unwrap_or_default();
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

pub fn stop_service(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>, id: &str) -> Result<()> {
    if let Some(e) = manager.snapshot(id) {
        if e.state == ServiceState::Stopped {
            return Ok(());
        }
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
                    let _ = std::process::Command::new(&exe)
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
                terminate_group(manager, id);
                Ok(())
            }
            "apache" => {
                if let Ok((root, exe)) = apache_paths(store) {
                    let _ = std::process::Command::new(&exe)
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
                terminate_group(manager, id);
                Ok(())
            }
            "postgresql" => {
                if let Ok((root, _)) = postgres_paths(store) {
                    let pg_ctl = root.join("bin").join(exe_name("pg_ctl"));
                    let version = installed_by_choice(store, "postgresql").map(|p| p.version).unwrap_or_default();
                    let _ = std::process::Command::new(&pg_ctl)
                        .args([
                            "-D".into(),
                            paths.postgres_data_dir(&version).to_string_lossy().to_string(),
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
                terminate_group(manager, id);
                Ok(())
            }
            "mongodb" => {
                // mongod 对 SIGTERM/强杀均靠 journaling 恢复，直接终止组
                terminate_group(manager, id);
                Ok(())
            }
            "redis" => {
                let out = std::process::Command::new("redis-cli")
                    .args(["-p", &redis_port.to_string(), "shutdown", "nosave"])
                    .output();
                if out.map(|o| o.status.success()).unwrap_or(false) {
                    std::thread::sleep(Duration::from_millis(600));
                }
                terminate_group(manager, id);
                Ok(())
            }
            s if s.starts_with("mysql@") => {
                let version = s.trim_start_matches("mysql@");
                if let Ok((basedir, _)) = mysql_paths(store, version) {
                    let admin = basedir.join("bin").join(exe_name("mysqladmin"));
                    let pass = store.get_setting("mysqlRootPassword").unwrap_or_else(|| "root".into());
                    let _ = std::process::Command::new(&admin)
                        .args([
                            "-h", "127.0.0.1", "-P", &mysql_port.to_string(),
                            "-u", "root", &format!("--password={pass}"), "shutdown",
                        ])
                        .output();
                    // 优雅关闭最多等 10s
                    for _ in 0..20 {
                        if !tcp_port_open(mysql_port) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
                terminate_group(manager, id);
                Ok(())
            }
            _ => {
                // 清单驱动的通用服务：先试清单声明的优雅停止命令，再终止进程组
                crate::generic::graceful_stop(store, paths, id);
                terminate_group(manager, id);
                Ok(())
            }
        }
    })();

    // 清 pid/状态：先确认进程真的没了，否则保留 pid 让下次 stop 能重试
    let mut survivors: Vec<u32> = Vec::new();
    if let Some(e) = manager.services.lock().get(id).cloned() {
        let pids = e.pids.lock().clone();
        for _ in 0..10 {
            survivors = pids.iter().copied().filter(|p| platform::process_alive(*p)).collect();
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
        let err = AppError::new("STOP_FAILED", format!("{id} 仍有 {} 个进程未退出", survivors.len()))
            .with_hint("尝试以管理员身份重开应用后再停止；或用「工具箱 → 端口扫描」结束占用进程")
            .with_detail(format!("残留 pid: {survivors:?}"));
        manager.set_error(id, err.clone());
        return Err(err);
    }
    result
}

fn terminate_group(manager: &Arc<ServiceManager>, id: &str) {
    if let Some(e) = manager.services.lock().get(id).cloned() {
        let mut group = e.group.lock();
        if let Some(g) = group.as_mut() {
            let _ = g.terminate(true);
        }
        *group = None;
    }
}

/* ================= nginx 重载 ================= */

pub fn reload_nginx(store: &Store, paths: &Paths) -> Result<()> {
    let (root, exe) = nginx_exe(store)?;
    configgen::validate_nginx(&exe, &paths.nginx_conf())?;
    let out = std::process::Command::new(&exe)
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
    Ok(())
}

/// 重建主配置并重载（站点/池变化后）。
/// Windows 上 nginx -s reload 存在已知信号语义差异（新增 server 块可能不生效），
/// 因此 Windows 采用「快速重启」（stop→start，亚秒级）；类 Unix 用热 reload。
/// Apache 同样重建：运行中则 httpd -k restart（信号经 pidfile，跨平台可靠）。
pub fn rebuild_and_reload(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>) -> Result<()> {
    let ports = PortsProfile::from_settings(store);
    let pools: Vec<(String, u16)> = store
        .all_port_assigns()
        .into_iter()
        .filter(|(sid, _)| sid.starts_with("php@"))
        .map(|(sid, base)| (sid.trim_start_matches("php@").to_string(), base))
        .collect();

    // ---- nginx ----
    if nginx_exe(store).is_ok() {
        let (root, exe) = nginx_exe(store)?;
        configgen::write_nginx_conf(paths, &root, &pools, ports.http, ports.https)?;
        if manager.snapshot("nginx").map(|s| s.state == ServiceState::Running).unwrap_or(false) {
            configgen::validate_nginx(&exe, &paths.nginx_conf())?;
            #[cfg(windows)]
            {
                stop_service(store, paths, manager, "nginx").ok();
                std::thread::sleep(Duration::from_millis(300));
                start_service(store, paths, manager, "nginx")?;
            }
            #[cfg(not(windows))]
            {
                reload_nginx(store, paths)?;
            }
        }
    }

    // ---- apache ----
    if apache_paths(store).is_ok() {
        let (root, exe) = apache_paths(store)?;
        let running = manager.snapshot("apache").map(|s| s.state == ServiceState::Running).unwrap_or(false);
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
        configgen::write_httpd_conf(paths, &root, &active_pools, ports.apache_http, ports.apache_https)?;
        if running {
            configgen::validate_httpd(&exe, &paths.apache_conf())?;
            // Windows 上 `-k restart` 是异步的：命令返回时新子进程可能尚未加载配置。
            // 连续 stop→start（如删站点后立即建站点）会与其竞态，导致服务存活但
            // 仍用旧配置（表现为新建站点 404）。这里统一走 stop_service → start_service
            // 的同步重启路径（含端口释放等待与健康检查），保证加载的是最新配置。
            #[cfg(windows)]
            {
                stop_service(store, paths, manager, "apache").ok();
                std::thread::sleep(Duration::from_millis(300));
                start_service(store, paths, manager, "apache")?;
            }
            #[cfg(not(windows))]
            {
                let out = std::process::Command::new(&exe)
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
            }
        }
    }
    Ok(())
}

/// 停全部（托盘退出时用）
pub fn stop_all(store: &Store, paths: &Paths, manager: &Arc<ServiceManager>) {
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
    let dir = paths.data().join("run");
    let _ = std::fs::create_dir_all(&dir);
    let json = serde_json::json!({
        "appPid": std::process::id(),
        "savedAt": crate::services::now_ms(),
        "services": entries.iter().map(|(k, v)| {
            let port = manager.started_port_or(k, 0);
            serde_json::json!({"id": k, "pids": v, "port": port})
        }).collect::<Vec<_>>(),
    });
    let _ = std::fs::write(dir.join("pids.json"), json.to_string());
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

/* ================= 配置体检（只读，不改任何文件） ================= */

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ConfigCheck {
    pub name: String,
    pub ok: bool,
    /// 未安装时为 skipped（前端显示灰）；失败时带 stderr 摘要
    pub status: String, // ok | fail | skipped
    pub detail: String,
}

/// 对已安装服务的配置做一次只读体检：
/// nginx `-t` / httpd `-t` / php `-n -c ini -v` / 配置文件存在性。
/// 修复向导的「重写配置」会改动文件；这里只看不动。
pub fn validate_configs(store: &Store, paths: &Paths) -> Vec<ConfigCheck> {
    let mut out = Vec::new();

    // nginx
    if let Ok((root, exe)) = nginx_exe(store) {
        let conf = paths.nginx_conf();
        if !conf.exists() {
            out.push(ConfigCheck { name: "Nginx".into(), ok: false, status: "fail".into(), detail: "nginx.conf 不存在（先启动一次生成）".into() });
        } else {
            let o = std::process::Command::new(&exe)
                .args(["-p".into(), root.to_string_lossy().to_string(), "-t".into(), "-c".into(), conf.to_string_lossy().to_string()])
                .output();
            match o {
                Ok(o) if o.status.success() => out.push(ConfigCheck { name: "Nginx".into(), ok: true, status: "ok".into(), detail: "syntax ok".into() }),
                Ok(o) => out.push(ConfigCheck { name: "Nginx".into(), ok: false, status: "fail".into(), detail: String::from_utf8_lossy(&o.stderr).lines().take(3).collect::<Vec<_>>().join(" / ") }),
                Err(e) => out.push(ConfigCheck { name: "Nginx".into(), ok: false, status: "fail".into(), detail: e.to_string() }),
            }
        }
    } else {
        out.push(ConfigCheck { name: "Nginx".into(), ok: true, status: "skipped".into(), detail: "未安装".into() });
    }

    // apache
    if let Ok((root, exe)) = apache_paths(store) {
        let conf = paths.apache_conf();
        if !conf.exists() {
            out.push(ConfigCheck { name: "Apache".into(), ok: false, status: "fail".into(), detail: "httpd.conf 不存在".into() });
        } else {
            let o = std::process::Command::new(&exe)
                .args(["-d".into(), root.to_string_lossy().to_string(), "-t".into(), "-f".into(), conf.to_string_lossy().to_string()])
                .output();
            match o {
                Ok(o) if o.status.success() => out.push(ConfigCheck { name: "Apache".into(), ok: true, status: "ok".into(), detail: "syntax ok".into() }),
                Ok(o) => out.push(ConfigCheck { name: "Apache".into(), ok: false, status: "fail".into(), detail: String::from_utf8_lossy(&o.stderr).lines().take(3).collect::<Vec<_>>().join(" / ") }),
                Err(e) => out.push(ConfigCheck { name: "Apache".into(), ok: false, status: "fail".into(), detail: e.to_string() }),
            }
        }
    } else {
        out.push(ConfigCheck { name: "Apache".into(), ok: true, status: "skipped".into(), detail: "未安装".into() });
    }

    // php：每个已装版本 -n -c ini -v（能跑起来 = ini 没写坏）
    if let Ok(list) = store.list_installed() {
        let phps: Vec<_> = list.iter().filter(|p| p.id == "php").collect();
        if phps.is_empty() {
            out.push(ConfigCheck { name: "PHP".into(), ok: true, status: "skipped".into(), detail: "未安装".into() });
        }
        for p in phps {
            let exe = PathBuf::from(&p.install_path).join(exe_name("php"));
            let ini = paths.php_ini(&p.version);
            if !ini.exists() {
                out.push(ConfigCheck { name: format!("PHP {}", p.version), ok: false, status: "fail".into(), detail: "php.ini 不存在".into() });
                continue;
            }
            let ok = std::process::Command::new(&exe)
                .args(["-n", "-c"].iter().copied().chain(std::iter::once(ini.to_string_lossy().as_ref())))
                .arg("-v")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            out.push(ConfigCheck {
                name: format!("PHP {}", p.version),
                ok,
                status: if ok { "ok".into() } else { "fail".into() },
                detail: if ok { "ini loads".into() } else { "php.ini 加载失败（看日志页 PHP 输出）".into() },
            });
        }
    }

    // redis / mysql 配置存在性
    if let Some(p) = store.find_installed("redis", None) {
        let conf = paths.redis_conf(&p.version);
        let ok = conf.exists();
        out.push(ConfigCheck {
            name: "Redis".into(), ok,
            status: if ok { "ok".into() } else { "fail".into() },
            detail: if ok { "redis.conf 存在".into() } else { "redis.conf 不存在（先启动一次生成）".into() },
        });
    }
    if let Some(p) = store.find_installed("mysql", None) {
        let ini = paths.mysql_ini(&p.version);
        let ok = ini.exists();
        out.push(ConfigCheck {
            name: format!("MySQL {}", p.version), ok,
            status: if ok { "ok".into() } else { "fail".into() },
            detail: if ok { "my.ini 存在".into() } else { "my.ini 不存在（先启动一次生成）".into() },
        });
    }
    out
}

#[cfg(test)]
mod validate_tests {
    use super::*;

    #[test]
    fn empty_install_reports_all_skipped() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::new(base.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let checks = validate_configs(&store, &paths);
        assert!(!checks.is_empty());
        // 什么都没装时体检不该有失败项
        assert!(checks.iter().all(|c| c.ok), "未安装不应报 fail：{:?}", checks);
        assert!(checks.iter().any(|c| c.status == "skipped"));
    }
}
