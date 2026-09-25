//! 清单驱动的通用服务启停：清单里声明了 `run` 的包无需在 ops.rs 写分支。
//! 覆盖全景目录里的长尾服务（Caddy / Meilisearch / MinIO / Mailpit / Consul …）。
//!
//! 端口分配：标准档用清单声明的 defaultPort（冲突时明确报错）；安全档从
//! defaultPort+20000 起找第一个空闲端口并持久化，避免与系统及其它环境抢端口。

use crate::error::{AppError, Result};
use crate::install::entry_relative_path;
use crate::model::{InstalledPackage, PackageManifestEntry, ServiceRunSpec};
use crate::paths::{write_with_backup, Paths};
use crate::services::*;
use crate::store::Store;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// 安全档端口偏移：defaultPort + 20000 起找空闲
pub const SAFE_PORT_OFFSET: u16 = 20000;

/// 有专属编排逻辑的内置服务（ops.rs 的 start_*/stop_* 分支）。
/// 它们同样在清单里声明 run（供前端展示启停语义），但启停必须走内置路径，
/// 否则会绕过 nginx reload / mysql 初始化 / php-cgi 端口池等关键处理。
pub const BUILTIN_SERVICE_IDS: &[&str] = &[
    "nginx", "apache", "php", "mysql", "postgresql", "mongodb", "redis", "mihomo",
];

pub fn is_builtin(id: &str) -> bool {
    BUILTIN_SERVICE_IDS.contains(&id)
}

/// 已解析的服务上下文：所有占位符的取值来源
pub struct Resolved {
    pub service_id: String,
    pub inst: InstalledPackage,
    pub entry: PackageManifestEntry,
    pub spec: ServiceRunSpec,
    /// 入口可执行文件绝对路径（{bin}）
    pub bin: PathBuf,
    /// 入口所在目录（{root}）
    pub root: PathBuf,
    /// 数据目录（{data}）
    pub data: PathBuf,
    /// 配置目录（{etc}）
    pub etc: PathBuf,
    /// 分配到的端口（{port}）；无端口服务为 None
    pub port: Option<u16>,
    /// 服务日志文件（{log}）
    pub log: PathBuf,
    /// 当前端口方案下的 Web 端口（{httpPort}）——隧道类服务转发站点时引用
    pub http_port: u16,
}

/// 占位符展开：{root} {data} {etc} {port} {log} {bin}
/// 派生端口：{port+1} / {port+1000} / {port-7000}（多端口服务如 MinIO 控制台、Temporal UI）
pub fn expand(template: &str, r: &Resolved) -> String {
    expand_inner(template, r, false)
}

/// 配置文件类模板展开：路径用正斜杠。
/// 配置格式（Caddyfile / YAML / TOML / ini）里反斜杠是转义字符，
/// Windows 路径直接写进去会被吞掉；正斜杠各平台通吃（与 configgen 的 nginx_path 同策略）。
pub fn expand_config(template: &str, r: &Resolved) -> String {
    expand_inner(template, r, true)
}

fn expand_inner(template: &str, r: &Resolved, slash_paths: bool) -> String {
    let path_str = |p: &std::path::Path| {
        if slash_paths {
            p.to_string_lossy().replace('\\', "/")
        } else {
            p.to_string_lossy().to_string()
        }
    };
    // 端口类占位符（含 {port+N}）先展开，避免 {port} 抢先替换掉 {port+N} 的前缀
    let with_ports = substitute_ports(template, r.port);
    with_ports
        .replace("{root}", &path_str(&r.root))
        .replace("{data}", &path_str(&r.data))
        .replace("{etc}", &path_str(&r.etc))
        .replace("{httpPort}", &r.http_port.to_string())
        .replace("{log}", &path_str(&r.log))
        .replace("{bin}", &path_str(&r.bin))
}

/// 替换所有 `{port}` / `{port+N}` / `{port-N}`；port 为 None 时清空
fn substitute_ports(text: &str, port: Option<u16>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{port") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 5..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let expr = &after[..close];
        if expr.is_empty() || expr.starts_with(['+', '-']) {
            let value = match port {
                None => String::new(),
                Some(base) => {
                    let delta: i32 = expr.trim_start_matches(['+', '-']).trim().parse().unwrap_or(0);
                    let signed = if expr.starts_with('-') { -delta } else { delta };
                    (base as i32 + signed).clamp(1, 65535).to_string()
                }
            };
            out.push_str(&value);
        } else {
            // 非端口占位符（如 {portable}）原样保留
            out.push_str(&rest[start..start + 5 + close + 1]);
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// 服务 ID 约定：单实例 → 包 id；多实例 → id@version
pub fn service_id_of(entry: &PackageManifestEntry) -> String {
    match &entry.run {
        Some(r) if !r.single_instance => format!("{}@{}", entry.id, entry.version),
        _ => entry.id.clone(),
    }
}

/// 已安装包 → 服务 ID（无需清单：php/mysql 这类内置编排按版本细分）
pub fn service_id_for(p: &InstalledPackage) -> String {
    if p.id == "php" || p.id == "mysql" {
        format!("{}@{}", p.id, p.version)
    } else {
        p.id.clone()
    }
}

/// 从清单里按「服务 ID」找到对应条目（去 @version 后匹配；单实例取使用中版本）
pub fn manifest_entry_for(store: &Store, service_id: &str) -> Option<PackageManifestEntry> {
    let (id, version) = match service_id.split_once('@') {
        Some((i, v)) => (i, Some(v)),
        None => (service_id, None),
    };
    let installer = crate::install::Installer::bundled();
    if let Some(v) = version {
        return installer.find(&format!("{id}@{v}"));
    }
    // 无版本：跟随「使用中版本」
    let inst = crate::ops::installed_by_choice(store, id)?;
    installer.find(&format!("{id}@{}", inst.version))
}

/// 解析服务上下文：安装信息 + 清单条目 + run 描述 + 各占位符取值
pub fn resolve(store: &Store, paths: &Paths, service_id: &str) -> Result<Resolved> {
    let entry = manifest_entry_for(store, service_id).ok_or_else(|| {
        AppError::new("UNKNOWN_SERVICE", format!("清单里没有服务 {service_id}"))
            .with_hint("该服务可能是内置编排（nginx/mysql 等），或清单需要更新")
    })?;
    // 内置服务有专属编排（nginx reload / mysql 初始化 / php 端口池），不能走通用路径
    if is_builtin(&entry.id) {
        return Err(AppError::new(
            "BUILTIN_SERVICE",
            format!("{} 由内置编排管理", entry.display_name),
        )
        .with_hint("该服务请通过常规启停调用；若启动失败请查看日志页"));
    }
    let spec = entry.run.clone().ok_or_else(|| {
        AppError::new(
            "NOT_A_SERVICE",
            format!("{} 是运行时/工具，不作为服务启动", entry.display_name),
        )
        .with_hint("纯运行时（node/python/go/composer 等）在站点里引用即可，无需启停")
    })?;

    let (id, version) = match service_id.split_once('@') {
        Some((i, v)) => (i.to_string(), v.to_string()),
        None => {
            let inst = crate::ops::installed_by_choice(store, &entry.id)
                .ok_or_else(|| AppError::not_installed(&entry.display_name))?;
            (inst.id.clone(), inst.version.clone())
        }
    };
    let inst = store
        .find_installed(&id, Some(&version))
        .ok_or_else(|| AppError::not_installed(&format!("{} {}", entry.display_name, version)))?;

    let root_dir = PathBuf::from(&inst.install_path);
    let bin = root_dir.join(entry_relative_path(&entry.entry));
    if !bin.exists() {
        return Err(
            AppError::new("BROKEN_INSTALL", format!("找不到 {}", bin.display()))
                .with_hint("套件可能损坏；卸载后在套件页重新安装"),
        );
    }
    let root = bin.parent().map(PathBuf::from).unwrap_or_else(|| root_dir.clone());
    let data = paths
        .data()
        .join(spec.data_dir.clone().unwrap_or_else(|| id.clone()));
    let etc = paths.etc_dir(&id, &version);
    let log = paths.service_log(&service_id.replace('@', "_"));
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&etc)?;

    let http_port = crate::services::PortsProfile::from_settings(store).http;
    let port = resolve_port(store, service_id, &entry, &spec);
    Ok(Resolved {
        service_id: service_id.to_string(),
        inst,
        entry,
        spec,
        bin,
        root,
        data,
        etc,
        port,
        log,
        http_port,
    })
}

/// 端口解析：已分配 > 标准档 defaultPort > 安全档偏移找空闲
fn resolve_port(
    store: &Store,
    service_id: &str,
    entry: &PackageManifestEntry,
    spec: &ServiceRunSpec,
) -> Option<u16> {
    if !uses_port(entry, spec) {
        return None;
    }
    if let Some(p) = store.get_port_assign(service_id) {
        return Some(p);
    }
    let def = entry.default_port.unwrap_or(0);
    if is_standard(store) && def > 0 {
        return Some(def);
    }
    // 安全档：从 defaultPort + 20000 起找第一个空闲端口（每服务独立，持久化）
    let start = def.saturating_add(SAFE_PORT_OFFSET).max(SAFE_PORT_OFFSET);
    let mut p = start;
    while p < start.saturating_add(200) {
        if !tcp_port_open(p) {
            return Some(p);
        }
        p += 1;
    }
    Some(start)
}

/// 是否是「标准档」。缺省视为标准档 —— 必须与 `PortsProfile::from_settings`
/// 和设置项默认值保持一致，否则没设置过的用户会看到「档位显示标准、实际按安全档分配端口」。
fn is_standard(store: &Store) -> bool {
    store.get_setting("portProfile").as_deref() != Some("safe")
}

/// 该服务是否需要端口（清单声明 defaultPort，或参数/配置模板里引用 {port}）
fn uses_port(entry: &PackageManifestEntry, spec: &ServiceRunSpec) -> bool {
    entry.default_port.is_some()
        || spec.args.iter().any(|a| a.contains("{port"))
        || spec
            .config_template
            .as_ref()
            .is_some_and(|t| t.contains("{port"))
}

/// 只读预览端口（不写分配表）：端口体检 / UI 展示用，需先确认服务已安装
pub fn planned_port(store: &Store, service_id: &str) -> Option<u16> {
    let entry = manifest_entry_for(store, service_id)?;
    let spec = entry.run.clone()?;
    if !uses_port(&entry, &spec) {
        return None;
    }
    if let Some(p) = store.get_port_assign(service_id) {
        return Some(p);
    }
    let def = entry.default_port.unwrap_or(0);
    if is_standard(store) && def > 0 {
        return Some(def);
    }
    Some(def.saturating_add(SAFE_PORT_OFFSET).max(SAFE_PORT_OFFSET))
}

/* ================= 通用启动 ================= */

pub fn start(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    service_id: &str,
    ports: &PortsProfile,
) -> Result<()> {
    let _ = ports;
    let r = resolve(store, paths, service_id)?;

    // 前置依赖提示（不阻断：用户可能用系统里已有的运行时）
    for dep in &r.spec.requires {
        if crate::ops::installed_by_choice(store, dep).is_none() {
            return Err(AppError::new(
                "DEPENDENCY_MISSING",
                format!("{} 需要先安装 {}", r.entry.display_name, dep),
            )
            .with_hint(format!("到套件页安装 {dep} 后再启动")));
        }
    }

    // 默认配置文件（首次生成；已存在则保留用户改动）
    if let (Some(cf), Some(tpl)) = (&r.spec.config_file, &r.spec.config_template) {
        let dest = r.etc.join(cf);
        if !dest.exists() {
            let content = expand_config(tpl, &r);
            write_with_backup(&dest, &content, &paths.backup())?;
        }
    }
    // CoreDNS 特例：Corefile 每次启动都重写——TLD 设置或转发策略变化要自动跟上，
    // 且通配解析模板含 {{ .Name }} 占位符，不能走通用模板渲染
    if r.entry.id == "coredns" {
        let tld = store
            .get_setting("defaultTld")
            .unwrap_or_else(|| "test".into());
        crate::dns::write_corefile(paths, &tld, &[]).map_err(AppError::from)?;
    }

    // 启动前自建的数据子目录（如 Temurin/Qdrant 的 storage、RabbitMQ 的 mnesia）
    for d in &r.spec.init_dirs {
        std::fs::create_dir_all(r.data.join(d))?;
    }

    // 一次性初始化（MariaDB 的 install-db、Neo4j 的 set-initial-password 等）
    run_init_if_needed(&r)?;

    if let Some(port) = r.port {
        // 被占 + 开了自动回落 → 换到附近空闲端口并固化为覆盖项
        let port = match crate::services::fallback_port_for(store, &r.service_id, port, &[]) {
            Some(p) => p,
            None => port,
        };
        precheck_port(port, &r.entry.display_name)?;
        store.set_port_assign(&r.service_id, port)?;
    }

    let args: Vec<String> = r.spec.args.iter().map(|a| expand(a, &r)).collect();
    let cwd = r
        .spec
        .cwd
        .as_ref()
        .map(|c| PathBuf::from(expand(c, &r)))
        .unwrap_or_else(|| r.root.clone());
    let env: Vec<(String, String)> = r
        .spec
        .env
        .iter()
        .flatten()
        .map(|(k, v)| (k.clone(), expand(v, &r)))
        .collect();

    // .bat/.cmd 不是可执行文件：Windows 上须经 cmd.exe 转发（Tomcat/Neo4j/MariaDB 等）
    let (program, args) = if cfg!(windows) && is_script(&r.bin) {
        let mut cmd = r.bin.to_string_lossy().to_string();
        // cmd 会按空格重切命令串，包一层引号保证带空格路径也正确
        if cmd.contains(' ') {
            cmd = format!("\"{cmd}\"");
        }
        let mut forwarded = vec!["/C".to_string(), cmd];
        forwarded.extend(args);
        (PathBuf::from("cmd.exe"), forwarded)
    } else {
        (r.bin.clone(), args)
    };

    let spec = SpawnSpec {
        program,
        args,
        cwd: Some(cwd),
        env,
        detached: None,
    };
    spawn_tracked(manager, &r.service_id, &spec)?;

    let timeout = Duration::from_secs(r.spec.health_timeout_sec.max(3));
    let healthy = match r.spec.health.as_str() {
        "none" => true,
        "process" => wait_pids_alive(manager, &r.service_id, timeout),
        _ => match r.port {
            Some(port) => wait_healthy(port, timeout),
            None => wait_pids_alive(manager, &r.service_id, timeout),
        },
    };
    if !healthy {
        return Err(AppError::new(
            "SERVICE_START_TIMEOUT",
            format!(
                "{} 启动超时（{}s 内{}）",
                r.entry.display_name,
                r.spec.health_timeout_sec,
                if r.port.is_some() { "端口未就绪" } else { "进程未存活" }
            ),
        )
        .with_hint(format!(
            "查看日志页 {} 的最后输出；常见原因是端口冲突、缺少依赖运行库或配置不合法",
            r.service_id
        )));
    }
    if let Some(port) = r.port {
        manager.set_started_port(&r.service_id, port);
    }
    Ok(())
}

/// 宽限停止：先跑清单声明的 stopArgs（如 `stop --config …`），失败由调用方兜底强杀
pub fn graceful_stop(store: &Store, paths: &Paths, service_id: &str) {
    if is_builtin(service_id) {
        return;
    }
    let Ok(r) = resolve(store, paths, service_id) else {
        return;
    };
    let Some(stop_args) = &r.spec.stop_args else {
        return;
    };
    let args: Vec<String> = stop_args.iter().map(|a| expand(a, &r)).collect();
    let _ = platform::command(&r.bin).args(&args).output();
    std::thread::sleep(Duration::from_millis(600));
}

/// .bat/.cmd/.ps1 等脚本入口：Windows 不能直接 CreateProcess，须经解释器
pub fn is_script(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "bat" | "cmd" | "ps1"))
}

/// 首次启动前执行一次性初始化（MariaDB install-db / Neo4j set-initial-password 等）。
/// 用 {data}/.nsb-initialized 标记防止重复执行；初始化失败即报错，不进入启动。
fn run_init_if_needed(r: &Resolved) -> Result<()> {
    let Some(init_args) = &r.spec.init_args else {
        return Ok(());
    };
    let marker = r
        .data
        .join(r.spec.init_marker.as_deref().unwrap_or(".nsb-initialized"));
    if marker.exists() {
        return Ok(());
    }

    // 初始化程序默认与主程序同目录、同名（如 bin/mariadb-install-db.exe）
    let init_exe = match &r.spec.init_bin {
        Some(name) => r
            .bin
            .parent()
            .map(|p| p.join(name))
            .unwrap_or_else(|| PathBuf::from(name)),
        None => r.bin.clone(),
    };
    let init_exe = if cfg!(windows) && !init_exe.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) && !init_exe.exists() {
        PathBuf::from(format!("{}.exe", init_exe.to_string_lossy()))
    } else {
        init_exe
    };

    let args: Vec<String> = init_args.iter().map(|a| expand(a, r)).collect();
    let out = platform::command(&init_exe)
        .args(&args)
        .current_dir(&r.root)
        .output()
        .map_err(|e| {
            AppError::io(&format!("执行 {} 初始化", r.entry.display_name), e)
                .with_hint("套件可能不完整；可在套件页卸载后重新安装")
        })?;
    if !out.status.success() {
        return Err(AppError::new(
            "SERVICE_INIT_FAILED",
            format!("{} 初始化失败", r.entry.display_name),
        )
        .with_hint("检查数据目录是否为空、磁盘空间是否充足")
        .with_detail(format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    if let Some(parent) = marker.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&marker, b"ok")?;
    Ok(())
}

/// 注册所有「清单声明 run」的已装服务（应用启动/安装后调用）
pub fn register_services(paths: &Paths, store: &Store, manager: &Arc<ServiceManager>) {
    let Ok(installed) = store.list_installed() else {
        return;
    };
    let installer = crate::install::Installer::bundled();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for p in installed {
        // 内置编排服务（nginx/mysql/php…）由 ops::register_services 注册，此处跳过
        if is_builtin(&p.id) {
            continue;
        }
        // 单实例服务只注册「使用中版本」；多实例逐版本注册
        let entry = match installer.find(&format!("{}@{}", p.id, p.version)) {
            Some(e) => e,
            None => continue,
        };
        let Some(run) = &entry.run else { continue };
        if run.single_instance {
            let active = crate::ops::installed_by_choice(store, &p.id);
            if active.map(|a| a.version != p.version).unwrap_or(true) {
                continue;
            }
        }
        let service_id = service_id_of(&entry);
        if !seen.insert(service_id.clone()) {
            continue;
        }
        let port = resolve_port(store, &service_id, &entry, run);
        manager.register(
            &service_id,
            &entry.display_name,
            Some(p.version.clone()),
            Some(entry.category.clone()),
            port,
            paths.service_log(&service_id.replace('@', "_")),
        );
    }
}
