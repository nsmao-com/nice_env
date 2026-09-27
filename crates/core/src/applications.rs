//! 站点应用进程：明确选择已安装运行时，以独立参数执行，复用托管进程组和日志。

use crate::error::{AppError, Result};
use crate::model::{Site, SiteKind, SiteRuntime};
use crate::paths::Paths;
use crate::services::{ServiceManager, SpawnSpec};
use crate::store::Store;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

const PREFIX: &str = "site-app:";

pub(crate) fn runtime_id(kind: &SiteKind) -> Option<&'static str> {
    match kind {
        SiteKind::Node => Some("node"),
        SiteKind::Python => Some("python"),
        SiteKind::Java => Some("temurin-jdk21"),
        SiteKind::Go => Some("go"),
        _ => None,
    }
}

pub(crate) fn service_id(site: &Site) -> String {
    format!("{PREFIX}{}", site.id)
}

pub(crate) fn site_id(service: &str) -> Option<&str> {
    service.strip_prefix(PREFIX)
}

pub(crate) fn target(runtime: &SiteRuntime) -> Result<SocketAddr> {
    let raw = runtime.proxy_target.as_deref().unwrap_or_default().trim();
    let url = reqwest::Url::parse(&if raw.contains("://") {
        raw.into()
    } else {
        format!("http://{raw}")
    })
    .map_err(|_| AppError::new("APP_BAD_TARGET", "请填写应用实际监听的本机 HTTP 地址和端口"))?;
    let ip = url
        .host_str()
        .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .filter(IpAddr::is_loopback);
    if url.scheme() != "http"
        || ip.is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(AppError::new(
            "APP_BAD_TARGET",
            "托管应用需要本机 HTTP 监听地址，例如 http://127.0.0.1:8080，不含路径或凭据",
        )
        .with_hint("HTTPS 由站点 Web 服务提供；已在外部运行的应用可关闭进程托管，使用反向代理。"));
    }
    let port = url
        .port_or_known_default()
        .filter(|port| *port != 0)
        .ok_or_else(|| AppError::new("APP_BAD_TARGET", "应用监听端口必须为 1–65535"))?;
    Ok(SocketAddr::new(ip.unwrap(), port))
}

pub(crate) fn validate(runtime: &SiteRuntime) -> Result<()> {
    let Some(app) = &runtime.application else {
        return Ok(());
    };
    if runtime_id(&runtime.kind).is_none() {
        return Err(AppError::new(
            "APP_BAD_RUNTIME",
            "只有 Node.js、Python、Java 和 Go 站点可以开启应用进程托管",
        ));
    }
    if app.version.is_empty()
        || app.version.len() > 128
        || app.version.chars().any(char::is_control)
    {
        return Err(AppError::new(
            "APP_VERSION_REQUIRED",
            "请选择已安装的应用运行时版本",
        ));
    }
    if app.args.is_empty()
        || app.args.len() > 64
        || app.args[0].trim().is_empty()
        || app
            .args
            .iter()
            .any(|arg| arg.len() > 8192 || arg.contains('\0'))
        || app.args.iter().map(String::len).sum::<usize>() > 32 * 1024
    {
        return Err(AppError::new(
            "APP_BAD_ARGUMENTS",
            "请填写应用入口和参数；最多 64 项、共 32 KiB，不能包含空字符",
        )
        .with_hint(
            "每个输入框对应一个参数，路径中的空格不需要加引号；不执行 shell 管道或命令拼接。",
        ));
    }
    if app
        .cwd
        .as_ref()
        .is_some_and(|cwd| cwd.len() > 4096 || cwd.chars().any(char::is_control))
    {
        return Err(AppError::new(
            "APP_BAD_DIRECTORY",
            "应用工作目录不能包含控制字符或超过 4096 字符",
        ));
    }
    target(runtime)?;
    Ok(())
}

pub(crate) fn installed_version(
    store: &Store,
    runtime: &SiteRuntime,
) -> Result<crate::model::InstalledPackage> {
    validate(runtime)?;
    let app = runtime
        .application
        .as_ref()
        .ok_or_else(|| AppError::new("APP_NOT_MANAGED", "此站点未开启应用进程托管"))?;
    let id = runtime_id(&runtime.kind)
        .ok_or_else(|| AppError::new("APP_BAD_RUNTIME", "此站点不支持应用进程托管"))?;
    store
        .find_installed(id, Some(&app.version))
        .filter(|package| package.category == "runtime")
        .ok_or_else(|| {
            AppError::new(
                "APP_RUNTIME_UNAVAILABLE",
                format!("{id} {} 尚未安装，请先安装或改选已安装版本", app.version),
            )
        })
}

pub(crate) fn register_services(paths: &Paths, store: &Store, manager: &ServiceManager) {
    let Ok(sites) = store.list_sites() else {
        return;
    };
    let wanted: std::collections::HashSet<_> = sites
        .iter()
        .filter(|site| {
            site.runtime.application.is_some()
                && runtime_id(&site.runtime.kind).is_some()
                && valid_site_id(&site.id)
        })
        .map(service_id)
        .collect();
    let stale: Vec<_> = manager
        .services
        .lock()
        .keys()
        .filter(|id| site_id(id).is_some() && !wanted.contains(*id))
        .cloned()
        .collect();
    for id in stale {
        if !manager.is_busy(&id) {
            manager.services.lock().remove(&id);
        }
    }
    for site in sites {
        let Some(app) = &site.runtime.application else {
            continue;
        };
        if runtime_id(&site.runtime.kind).is_none() || !valid_site_id(&site.id) {
            continue;
        }
        let id = service_id(&site);
        manager.register(
            &id,
            &format!("{} · 应用", site.name),
            Some(app.version.clone()),
            Some("runtime".into()),
            target(&site.runtime).ok().map(|target| target.port()),
            paths.service_log(&id),
        );
    }
}

fn valid_site_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
}

fn launch_spec(paths: &Paths, store: &Store, site: &Site) -> Result<(SpawnSpec, SocketAddr)> {
    let package = installed_version(store, &site.runtime)?;
    let app = site.runtime.application.as_ref().unwrap();
    let target = target(&site.runtime)?;
    let ports = crate::services::PortsProfile::from_settings(store);
    let web_ports = if site.runtime.web_server == "apache" {
        [ports.apache_http, ports.apache_https]
    } else {
        [ports.http, ports.https]
    };
    if web_ports.contains(&target.port()) {
        return Err(AppError::new(
            "APP_WEB_PORT_CONFLICT",
            "应用监听端口与站点 Web 服务相同，请为应用选择另一个端口",
        ));
    }
    let entry = crate::install::Installer::effective(paths).installed_entry(&package);
    let bin = crate::pathenv::terminal_package_directory(&package, &entry).map_err(|reason| {
        AppError::new(
            "APP_RUNTIME_UNAVAILABLE",
            format!("{} {}：{reason}", package.id, package.version),
        )
    })?;
    let program = PathBuf::from(&package.install_path)
        .join(entry.entry.replace('\\', "/"))
        .canonicalize()?;
    let cwd = app
        .cwd
        .as_deref()
        .filter(|cwd| !cwd.trim().is_empty())
        .unwrap_or(&site.root_dir);
    let cwd = crate::pathenv::terminal_directory(cwd).map_err(|error| {
        AppError::new("APP_BAD_DIRECTORY", "应用工作目录不可用")
            .with_detail(error.message)
            .with_hint("请选择存在的项目目录后重试。")
    })?;
    let mut directories = vec![PathBuf::from(&bin)];
    if let Some(existing) = std::env::var_os("PATH") {
        directories.extend(std::env::split_paths(&existing));
    }
    let path = std::env::join_paths(directories)
        .map_err(|error| AppError::internal("准备应用 PATH", error.to_string()))?;
    let mut env = vec![
        ("PATH".into(), path.to_string_lossy().into_owned()),
        ("PORT".into(), target.port().to_string()),
        ("HOST".into(), target.ip().to_string()),
    ];
    if site.runtime.kind == SiteKind::Python {
        env.push(("PYTHONUNBUFFERED".into(), "1".into()));
    }
    if site.runtime.kind == SiteKind::Java {
        if let Some(home) = PathBuf::from(bin).parent() {
            env.push(("JAVA_HOME".into(), home.to_string_lossy().into_owned()));
        }
    }
    Ok((
        SpawnSpec {
            program,
            args: app.args.clone(),
            cwd: Some(cwd),
            env,
            detached: Some(false),
        },
        target,
    ))
}

/// 运行时入口由已安装套件确定，不把参数拼接成 shell 命令。
pub(crate) fn spawn(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    id: &str,
) -> Result<()> {
    let site_id = site_id(id)
        .filter(|id| valid_site_id(id))
        .ok_or_else(|| AppError::new("BAD_SITE_ID", "站点应用标识无效"))?;
    let site = crate::sites::get(store, site_id)?;
    let (spec, target) = launch_spec(paths, store, &site)?;
    crate::services::precheck_port(target.port(), &format!("{} 应用", site.name))?;
    let pid = crate::services::spawn_tracked(manager, id, &spec)?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        if !platform::process_alive(pid) {
            return Err(
                AppError::new("APP_EXITED", "应用在监听端口前退出，请查看应用日志")
                    .with_hint("请检查入口文件、参数和项目依赖；NiceEnv 不会自动安装项目依赖。"),
            );
        }
        let listeners = crate::ports::listener_endpoints()?;
        if listeners.iter().any(|listener| listener.accepts(target)) {
            let system = sysinfo::System::new_all();
            let belongs = |candidate: u32| {
                let mut current = Some(sysinfo::Pid::from_u32(candidate));
                for _ in 0..128 {
                    let Some(value) = current else {
                        return false;
                    };
                    if value.as_u32() == pid {
                        return true;
                    }
                    current = system.process(value).and_then(|process| process.parent());
                }
                false
            };
            let owned: Vec<_> = listeners
                .iter()
                .filter(|listener| listener.accepts(target))
                .collect();
            if owned.iter().any(|listener| !belongs(listener.pid)) {
                return Err(AppError::new(
                    "APP_PORT_CHANGED",
                    "应用启动期间端口被其他进程占用，未接管该进程",
                ));
            }
            if std::net::TcpStream::connect_timeout(&target, Duration::from_millis(300)).is_ok() {
                // Go / Node worker 可由启动器派生；记录已确认归属的监听进程，便于停机核验。
                if let Some(entry) = manager.services.lock().get(id).cloned() {
                    let mut pids = entry.pids.lock();
                    for listener in owned {
                        if !pids.contains(&listener.pid) {
                            pids.push(listener.pid);
                        }
                    }
                }
                manager.set_started_port(id, target.port());
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(AppError::new(
        "APP_START_TIMEOUT",
        format!("应用在 60 秒内未监听 {target}"),
    )
    .with_hint(
        "请查看应用日志，确认项目依赖已安装，应用读取 PORT/HOST 环境变量或通过参数监听相同地址。",
    ))
}

pub(crate) fn running_settings_changed(
    original: &Site,
    next: &Site,
    manager: &ServiceManager,
) -> bool {
    manager.is_busy(&service_id(original))
        && (original.runtime.application != next.runtime.application
            || original.runtime.kind != next.runtime.kind
            || original.root_dir != next.root_dir
            || original.runtime.proxy_target != next.runtime.proxy_target)
}

pub(crate) fn stop(
    site: &Site,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
) -> Result<()> {
    crate::ops::stop_service(store, paths, manager, &service_id(site))?;
    crate::ops::save_pidfile_checked(paths, manager)
}

#[derive(Default)]
pub(crate) struct ApplicationStart {
    attempted: Option<String>,
}

impl ApplicationStart {
    pub(crate) fn ensure_running(
        &mut self,
        site: &Site,
        paths: &Paths,
        store: &Store,
        manager: &Arc<ServiceManager>,
    ) -> Result<()> {
        if site.runtime.application.is_none() {
            return Ok(());
        }
        let id = service_id(site);
        if !manager.is_busy(&id) {
            self.attempted = Some(id.clone());
        }
        crate::ops::start_service(store, paths, manager, &id)?;
        crate::ops::save_pidfile_checked(paths, manager)
    }

    pub(crate) fn restore(
        &self,
        paths: &Paths,
        store: &Store,
        manager: &Arc<ServiceManager>,
    ) -> Vec<String> {
        let mut failures = Vec::new();
        if let Some(id) = &self.attempted {
            if let Err(error) = crate::ops::stop_service(store, paths, manager, id) {
                failures.push(format!(
                    "停止本次启动的应用：{error}；{}",
                    error.detail.as_deref().unwrap_or_default()
                ));
            }
            if let Err(error) = crate::ops::save_pidfile_checked(paths, manager) {
                failures.push(format!("保存应用进程恢复记录：{error}"));
            }
        }
        failures
    }
}
