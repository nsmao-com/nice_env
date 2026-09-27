//! 清单驱动的通用服务启停：清单里声明了 `run` 的包无需在 ops.rs 写分支。
//! 覆盖全景目录里的长尾服务（Caddy / Meilisearch / MinIO / Mailpit / Consul …）。
//!
//! 端口分配：标准档用清单声明的 defaultPort（冲突时明确报错）；安全档从
//! defaultPort+20000 起找第一个空闲端口并持久化，避免与系统及其它环境抢端口。

use crate::error::{AppError, Result};
use crate::install::entry_relative_path;
use crate::model::{InstalledPackage, PackageManifestEntry, ServiceRunSpec};
use crate::paths::Paths;
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
    "nginx",
    "apache",
    "php",
    "mysql",
    "postgresql",
    "mongodb",
    "redis",
    "mihomo",
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
                    let delta: i32 = expr
                        .trim_start_matches(['+', '-'])
                        .trim()
                        .parse()
                        .unwrap_or(0);
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
        let installed = store.find_installed(id, Some(v))?;
        return Some(installer.installed_entry(&installed));
    }
    // 无版本：跟随「使用中版本」
    let inst = crate::ops::installed_by_choice(store, id)?;
    Some(installer.installed_entry(&inst))
}

/// 解析服务上下文：安装信息 + 清单条目 + run 描述 + 各占位符取值
pub fn resolve(store: &Store, paths: &Paths, service_id: &str) -> Result<Resolved> {
    resolve_with_sftpgo_directory(store, paths, service_id, None)
}

/// 显式目录只供选择前的只读检查，不能创建目录或绕过路径检查。
fn resolve_with_sftpgo_directory(store: &Store, paths: &Paths, service_id: &str, directory: Option<PathBuf>) -> Result<Resolved> {
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
    if crate::install::is_sftpgo_installer(&entry) {
        return Err(AppError::new("SFTPGO_INSTALLER_PACKAGE", "该 SFTPGo 安装记录指向 Windows 安装器，不能作为服务启动")
            .with_hint("请在套件页卸载并重新安装该版本，获取 portable 包；卸载程序版本会保留配置和数据目录。"));
    }
    let bin = root_dir.join(entry_relative_path(&entry.entry));
    if !bin.exists() {
        return Err(
            AppError::new("BROKEN_INSTALL", format!("找不到 {}", bin.display()))
                .with_hint("套件可能损坏；卸载后在套件页重新安装"),
        );
    }
    let root = bin
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| root_dir.clone());
    let data = paths
        .data()
        .join(spec.data_dir.clone().unwrap_or_else(|| id.clone()));
    if managed_consul(&entry, &spec) { crate::paths::checked_data_path(&paths.base, "data/consul")?; }
    let preview = directory.is_some();
    let etc = if managed_sftpgo(&entry, &spec) {
        match directory { Some(directory) => directory, None => sftpgo_config_dir(store, paths)? }
    } else { paths.etc_dir(&id, &version) };
    let log = paths.service_log(&service_id.replace('@', "_"));
    if !preview { std::fs::create_dir_all(&data)?; std::fs::create_dir_all(&etc)?; }

    let http_port = crate::services::PortsProfile::from_settings(store).http;
    let port = resolve_port(store, service_id, &entry, &spec)?;
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

/// 端口解析不探测或占用端口；实际启动时一次选择主端口及派生端口。
fn resolve_port(
    store: &Store,
    service_id: &str,
    entry: &PackageManifestEntry,
    spec: &ServiceRunSpec,
) -> Result<Option<u16>> {
    if !uses_port(entry, spec) {
        return Ok(None);
    }
    let adjustable = port_templates(spec).iter().any(|text| PORT_TOKEN.is_match(text));
    let check = |port| {
        if !adjustable && entry.default_port != Some(port) {
            Err(AppError::new("SERVICE_PORT_UNSUPPORTED", format!("{} 的运行配置未声明可调整端口，不能应用端口 {port}", entry.display_name))
                .with_hint("请先为该模块配置实际的监听端口参数；不能仅修改健康检查使用的端口。"))
        } else { Ok(Some(port)) }
    };
    if let Some(value) = store.get_setting_checked(&format!("portOverride.{service_id}"))? {
        let port = value.parse::<u16>().ok().filter(|p| *p > 0)
            .ok_or_else(|| AppError::new("BAD_PORT", format!("服务 {service_id} 的端口覆盖无效")))?;
        return check(port);
    }
    if let Some(p) = store.get_port_assign_checked(service_id)? {
        return check(p);
    }
    let def = entry.default_port.unwrap_or(0);
    // 没有端口占位符的服务不能仅改变健康检查端口；继续使用其原生配置默认端口。
    if (is_standard(store) || !adjustable) && def > 0 {
        return Ok(Some(def));
    }
    let start = def.saturating_add(SAFE_PORT_OFFSET).max(SAFE_PORT_OFFSET);
    Ok(Some(start))
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
        || spec.env.iter().flat_map(|env| env.values()).any(|s| s.contains("{port"))
}

/// 只读预览端口（不写分配表）：端口体检 / UI 展示用，需先确认服务已安装
pub fn planned_port(store: &Store, service_id: &str) -> Option<u16> {
    let entry = manifest_entry_for(store, service_id)?;
    let spec = entry.run.clone()?;
    resolve_port(store, service_id, &entry, &spec).ok().flatten()
}

/* ================= 通用启动 ================= */

fn port_templates(spec: &ServiceRunSpec) -> Vec<&str> {
    spec.args.iter().chain(spec.init_args.iter().flatten()).chain(spec.stop_args.iter().flatten())
        .chain(spec.env.iter().flat_map(|env| env.values())).chain(spec.config_template.iter())
        .map(String::as_str).collect()
}

static PORT_TOKEN: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(||
    regex::Regex::new(r"\{port([+-]\d+)?\}").unwrap());

const SFTPGO_CONFIG_BINDING: &str = "sftpgoConfigDir";

const CONSUL_TCP_OFFSETS: [i32; 7] = [-200, -199, -198, 0, 2, 3, 100];
const CONSUL_UDP_OFFSETS: [i32; 3] = [-199, -198, 100];

/// 仅对内置持久化单节点编排应用端口组和 leader 检查，不猜测自定义集群设置。
/// Windows 的上游 WAL 在同步目录时失败；内置清单显式使用受支持的 BoltDB 后端。
fn managed_consul(entry: &PackageManifestEntry, spec: &ServiceRunSpec) -> bool {
    entry.id == "consul" && spec.single_instance && spec.data_dir.is_none() && spec.config_file.is_none()
        && spec.config_template.is_none() && spec.health == "tcp"
        && spec.args == ["agent", "-server", "-bootstrap-expect", "1", "-node", "niceenv-consul",
            "-bind", "127.0.0.1", "-client", "127.0.0.1", "-data-dir", "{data}", "-ui",
            "-http-port", "{port}", "-dns-port", "{port+100}", "-server-port", "{port-200}",
            "-serf-lan-port", "{port-199}", "-serf-wan-port", "{port-198}", "-grpc-port", "{port+2}",
            "-grpc-tls-port", "{port+3}", "-hcl", "connect { enabled = true }",
            "-hcl", "raft_logstore { backend = \"boltdb\" }"]
}

fn needs_udp(r: &Resolved, base: u16, port: u16) -> bool {
    r.entry.id == "coredns" || (managed_consul(&r.entry, &r.spec)
        && CONSUL_UDP_OFFSETS.contains(&(i32::from(port) - i32::from(base))))
}

fn managed_sftpgo(entry: &PackageManifestEntry, spec: &ServiceRunSpec) -> bool {
    entry.id == "sftpgo" && spec.single_instance && spec.health == "tcp"
        && spec.args == ["serve", "--config-dir", "{etc}", "--log-file-path", "{data}/sftpgo.log"]
        && spec.env.as_ref().is_some_and(|env| env.get("SFTPGO_SFTPD__BINDINGS__0__PORT").map(String::as_str) == Some("{port}")
            && env.get("SFTPGO_HTTPD__BINDINGS__0__PORT").map(String::as_str) == Some("{port+6058}"))
}

/// 绑定相对配置目录；不复制数据库或 SSH 主机密钥，切换程序版本继续使用原目录。
fn sftpgo_config_dir(store: &Store, paths: &Paths) -> Result<PathBuf> {
    if let Some(relative) = store.get_setting_checked(SFTPGO_CONFIG_BINDING)? {
        let path = checked_sftpgo_directory(paths, &relative)?;
        if !path.is_dir() {
            return Err(AppError::new("SFTPGO_CONFIG_MISSING", "SFTPGo 原配置和数据目录不存在，未创建空库")
                .with_hint(format!("请恢复原目录后重试：{}", path.display())));
        }
        return Ok(path);
    }
    let parent = crate::paths::checked_data_path(&paths.base, "etc/sftpgo")?;
    let mut candidates = Vec::new();
    match std::fs::read_dir(&parent) {
        Ok(entries) => for entry in entries {
            let entry = entry?;
            let relative = format!("etc/sftpgo/{}", entry.file_name().to_string_lossy());
            let path = crate::paths::checked_data_path(&paths.base, &relative)?;
            if path.is_dir() && std::fs::read_dir(&path)?.next().transpose()?.is_some() { candidates.push(path); }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error.into()),
    }
    match candidates.len() {
        0 => Ok(crate::paths::checked_data_path(&paths.base, "etc/sftpgo/shared")?),
        1 => Ok(candidates.remove(0)),
        _ => Err(AppError::new("SFTPGO_CONFIG_AMBIGUOUS", "发现多份旧 SFTPGo 配置和数据，未自动选择或合并")
            .with_hint("请在服务的“配置目录”中选择要继续使用的一份，再启动服务；各目录的原文件会保留。")),
    }
}

fn checked_sftpgo_directory(paths: &Paths, relative: &str) -> Result<PathBuf> {
    let parts: Vec<_> = relative.split('/').collect();
    if parts.len() != 3 || parts[0] != "etc" || parts[1] != "sftpgo" || parts[2].is_empty()
        || matches!(parts[2], "." | "..") || !parts[2].bytes().all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(&c)) {
        return Err(AppError::new("SFTPGO_CONFIG_PATH", "SFTPGo 配置目录无效，请从已发现的目录中选择"));
    }
    Ok(crate::paths::checked_data_path(&paths.base, relative)?)
}

pub(crate) fn sftpgo_config_directories(store: &Store, paths: &Paths) -> Result<crate::model::SftpgoConfigDirectories> {
    let entry = manifest_entry_for(store, "sftpgo").ok_or_else(|| AppError::not_installed("SFTPGo"))?;
    if entry.run.as_ref().is_none_or(|spec| !managed_sftpgo(&entry, spec)) {
        return Err(AppError::new("SFTPGO_CONFIG_UNSUPPORTED", "此 SFTPGo 使用自定义运行配置，请按其启动参数管理配置目录"));
    }
    let current = store.get_setting_checked(SFTPGO_CONFIG_BINDING)?;
    let parent = crate::paths::checked_data_path(&paths.base, "etc/sftpgo")?;
    let mut directories = Vec::new();
    let entries = match std::fs::read_dir(&parent) {
        Ok(entries) => Some(entries), Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    for entry in entries.into_iter().flatten() {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let label = entry.file_name().to_string_lossy().into_owned();
        let relative = format!("etc/sftpgo/{label}");
        let mut row = crate::model::SftpgoConfigDirectory { directory: relative.clone(), label, config_file: None,
            state_files: vec![], modified_at: None, issue: None };
        let inspected = (|| -> Result<()> {
            let directory = checked_sftpgo_directory(paths, &relative)?;
            if std::fs::read_dir(&directory)?.next().transpose()?.is_none() && current.as_deref() != Some(&relative) { return Ok(()); }
            row.modified_at = std::fs::metadata(&directory)?.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
            row.config_file = sftpgo_config_file(&directory)?.and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()));
            let r = resolve_with_sftpgo_directory(store, paths, "sftpgo", Some(directory))?;
            let config = inspect_sftpgo(store, paths, &r, true, false)?;
            row.state_files = config.state_files.iter().map(|path| path.strip_prefix(&r.etc).unwrap_or(path).to_string_lossy().into_owned()).collect();
            for file in std::iter::once(&config.file).chain(config.state_files.iter()) {
                let modified = std::fs::metadata(file)?.modified().ok().and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
                row.modified_at = row.modified_at.max(modified);
            }
            Ok(())
        })();
        if let Err(error) = inspected { row.issue = Some(match error.hint { Some(hint) => format!("{}：{hint}", error.message), None => error.message }); }
        if row.modified_at.is_some() || row.issue.is_some() { directories.push(row); }
    }
    directories.sort_by(|a, b| a.directory.cmp(&b.directory));
    Ok(crate::model::SftpgoConfigDirectories { version: entry.version, current, directories })
}

pub(crate) fn select_sftpgo_config(store: &Store, paths: &Paths, manager: &ServiceManager,
    directory: &str, version: &str, expected_current: Option<&str>) -> Result<()> {
    checked_sftpgo_directory(paths, directory)?;
    if manager.snapshot("sftpgo").is_some_and(|s| matches!(s.state, crate::model::ServiceState::Running | crate::model::ServiceState::Starting | crate::model::ServiceState::Stopping)
        || s.pids.iter().any(|pid| platform::process_alive(*pid))) {
        return Err(AppError::new("SERVICE_BUSY", "请先停止 SFTPGo，再选择配置目录"));
    }
    let latest = sftpgo_config_directories(store, paths)?;
    if latest.version != version || (latest.current.as_deref() != expected_current && latest.current.as_deref() != Some(directory)) {
        return Err(AppError::new("SFTPGO_CONFIG_CHANGED", "SFTPGo 版本或目录选择已经变化，请刷新后重新选择"));
    }
    let candidate = latest.directories.iter().find(|entry| entry.directory == directory)
        .ok_or_else(|| AppError::new("SFTPGO_CONFIG_MISSING", "所选配置目录已不存在，请刷新后重新选择"))?;
    if let Some(issue) = &candidate.issue { return Err(AppError::new("SFTPGO_CONFIG_INVALID", "所选目录未通过启动前检查，原选择保持不变").with_hint(issue)); }
    store.set_setting(SFTPGO_CONFIG_BINDING, directory)?;
    if manager.snapshot("sftpgo").is_some_and(|s| s.last_error.is_some_and(|e| e.code.starts_with("SFTPGO_CONFIG_") || e.code == "SFTPGO_STATE_MISSING")) {
        manager.set_state("sftpgo", crate::model::ServiceState::Stopped);
    }
    Ok(())
}

struct SftpgoConfig {
    file: PathBuf,
    env: Vec<(String, String)>,
    web_target: Result<String>,
    state_files: Vec<PathBuf>,
}

fn sftpgo_config_file(directory: &std::path::Path) -> Result<Option<PathBuf>> {
    let mut files = Vec::new();
    for extension in ["json", "yaml", "yml", "toml", "hcl", "ini", "properties"] {
        let file = directory.join(format!("sftpgo.{extension}"));
        match std::fs::symlink_metadata(&file) {
            Ok(_) => files.push(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
        }
    }
    if files.len() > 1 {
        return Err(AppError::new("SFTPGO_CONFIG_AMBIGUOUS", "同一目录有多个 SFTPGo 配置文件，无法确定使用哪一个"));
    }
    Ok(files.pop())
}

fn sftpgo_env_key(key: &str) -> String {
    if cfg!(windows) { key.to_ascii_uppercase() } else { key.into() }
}

fn sftpgo_env_error(path: &std::path::Path, line: usize) -> AppError {
    AppError::new("SFTPGO_ENV_INVALID", "SFTPGo 环境配置无法完整解析，未启动服务")
        .with_hint(format!("请检查 {} 第 {} 行的赋值、引号或编码；原文件已保留。", path.display(), line))
}

/// 一次读取，按上游 os.ReadDir 的文件名顺序解析；不把配置中的值写入父进程环境或错误信息。
fn sftpgo_env_files(paths: &Paths, directory: &std::path::Path) -> Result<Vec<(PathBuf, String)>> {
    let mut files = Vec::new();
    let relative = directory.join("env.d").strip_prefix(&paths.base).map_err(|_| AppError::new("SFTPGO_CONFIG_PATH", "环境配置路径无效"))?.to_path_buf();
    let directory = crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(&relative))?;
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().to_str().is_none() { return Err(sftpgo_env_error(&path, 1)); }
        let relative = path.strip_prefix(&paths.base).map_err(|_| AppError::new("SFTPGO_CONFIG_PATH", "环境配置路径无效"))?;
        let path = crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
        let metadata = std::fs::metadata(&path)?;
        // 与上游的文件限制相同，不读取大文件或目录。
        if !metadata.is_file() || metadata.len() > 1024 * 1024 { continue; }
        let bytes = std::fs::read(&path)?;
        let content = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
            if bytes.len() % 2 != 0 { return Err(sftpgo_env_error(&path, 1)); }
            let words: Vec<_> = bytes[2..].chunks_exact(2).map(|pair| if bytes[0] == 0xff {
                u16::from_le_bytes([pair[0], pair[1]])
            } else { u16::from_be_bytes([pair[0], pair[1]]) }).collect();
            String::from_utf16(&words).map_err(|_| sftpgo_env_error(&path, 1))?
        } else { String::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes).to_vec())
            .map_err(|_| sftpgo_env_error(&path, 1))? };
        files.push((path, content));
    }
    files.sort_by(|a, b| a.0.file_name().unwrap().to_str().unwrap().cmp(b.0.file_name().unwrap().to_str().unwrap()));
    Ok(files)
}

// Parsing expressions and quoting/interpolation semantics adapted from gotenv v1.6.0:
// https://github.com/subosito/gotenv/blob/v1.6.0/gotenv.go
// The MIT License (MIT), Copyright (c) 2013 Alif Rachmawadi
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.
fn sftpgo_parse_env(files: &[(PathBuf, String)], mut environment: std::collections::HashMap<String, String>)
    -> Result<std::collections::HashMap<String, String>> {
    static ASSIGNMENT: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(||
        regex::Regex::new(r#"\A[ \t\r\n\f]*(?:export[ \t\r\n\f]+)?([A-Za-z0-9_.]+)(?:[ \t\r\n\f]*=[ \t\r\n\f]*|:[ \t\r\n\f]+?)('(?:\'|[^'])*'|"(?:\"|[^"])*"|[^#\n]+)?[ \t\r\n\f]*(?:[ \t\r\n\f]*\#.*)?\z"#).unwrap());
    static VARIABLE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(||
        regex::Regex::new(r"(\\)?(\$)(\{?([A-Z0-9_]+)?\}?)").unwrap());
    static UNESCAPE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(||
        regex::Regex::new(r"\\([^$])").unwrap());
    for (path, content) in files {
        let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
        let mut lines = normalized.split('\n').enumerate();
        let mut parsed = std::collections::HashMap::<String, String>::new();
        let mut names = std::collections::HashMap::<String, String>::new();
        while let Some((number, raw)) = lines.next() {
            let invalid = || sftpgo_env_error(path, number + 1);
            if raw.len() >= 65535 || raw.contains('\0') { return Err(invalid()); }
            let mut line = raw.trim().to_string();
            if line.is_empty() || line.starts_with('#') { continue; }
            let mut quote = None;
            if let Some(index) = line.find('=').or_else(|| line.find(':')).filter(|i| *i > 0 && *i + 1 < line.len()) {
                let value = line[index + 1..].trim();
                if let Some(first @ (b'\'' | b'"')) = value.as_bytes().first().copied() {
                    // 与 gotenv 的跨行判定一致，包括行尾转义引号。
                    quote = Some(first as char);
                    if value[1..].trim().rfind(first as char).is_some_and(|i| value.as_bytes()[i] != b'\\') { quote = None; }
                }
            }
            while let Some(ending) = quote {
                let Some((_, next)) = lines.next() else { return Err(invalid()); };
                if next.len() >= 65535 || next.contains('\0') { return Err(invalid()); }
                line.push('\n'); line.push_str(next);
                if next.rfind(ending).is_some_and(|i| i == 0 || next.as_bytes()[i - 1] != b'\\') { quote = None; }
            }
            let captures = ASSIGNMENT.captures(&line).ok_or_else(invalid)?;
            let key = captures[1].to_string();
            let folded = sftpgo_env_key(&key);
            if names.insert(folded, key.clone()).is_some_and(|previous| previous != key) {
                return Err(AppError::new("SFTPGO_ENV_AMBIGUOUS", "SFTPGo 环境配置包含重复的大小写变量名，请合并后重试")
                    .with_hint(path.display().to_string()));
            }
            let mut value = captures.get(2).map(|m| m.as_str()).unwrap_or("").trim().to_string();
            let single = value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'');
            let double = value.len() >= 2 && value.starts_with('"') && value.ends_with('"');
            if single || double { value = value[1..value.len() - 1].to_string(); }
            if double {
                value = value.replace(r"\n", "\n").replace(r"\r", "\r");
                value = UNESCAPE.replace_all(&value, "$1").into_owned();
            }
            if !single {
                value = VARIABLE.replace_all(&value, |capture: &regex::Captures<'_>| {
                    if capture.get(1).is_some() { return capture[0][1..].to_string(); }
                    let Some(name) = capture.get(4) else { return capture[0].to_string(); };
                    environment.get(&sftpgo_env_key(name.as_str())).or_else(|| parsed.get(name.as_str())).cloned().unwrap_or_default()
                }).into_owned();
            }
            if value.contains('\0') { return Err(invalid()); }
            parsed.insert(key, value);
        }
        // 同一文件的同名赋值最后一项生效；跨文件和继承环境则保留先已有的值（含空值）。
        for (key, value) in parsed { environment.entry(sftpgo_env_key(&key)).or_insert(value); }
    }
    Ok(environment)
}

fn sftpgo_process_env(r: &Resolved) -> Result<std::collections::HashMap<String, String>> {
    let mut environment: std::collections::HashMap<_, _> = std::env::vars_os().filter_map(|(key, value)|
        Some((sftpgo_env_key(&key.into_string().ok()?), value.into_string().ok()?))).collect();
    let mut names = std::collections::HashSet::new();
    for (key, value) in r.spec.env.iter().flatten() {
        let key = sftpgo_env_key(key);
        if !names.insert(key.clone()) { return Err(AppError::new("SFTPGO_ENV_AMBIGUOUS", "运行配置包含重复的大小写环境变量名，请合并后重试")); }
        environment.insert(key, expand(value, r));
    }
    Ok(environment)
}

fn prepare_sftpgo(store: &Store, paths: &Paths, r: &Resolved) -> Result<SftpgoConfig> {
    inspect_sftpgo(store, paths, r, store.get_setting_checked(SFTPGO_CONFIG_BINDING)?.is_some(), true)
}

fn inspect_sftpgo(store: &Store, paths: &Paths, r: &Resolved, previously_started: bool, write_config: bool) -> Result<SftpgoConfig> {
    let existing = sftpgo_config_file(&r.etc)?;
    if previously_started && existing.is_none() {
        return Err(AppError::new("SFTPGO_CONFIG_MISSING", "原 SFTPGo 配置文件已丢失，未使用默认配置创建空库")
            .with_hint(format!("请恢复 {} 下的原配置文件后重试。", r.etc.display())));
    }
    let source = if let Some(file) = &existing { file.clone() } else {
        // 旧服务可能从其程序目录读取 portable 默认配置（例如 Bolt 数据库），必须先沿用它。
        let old_version = r.etc.file_name().and_then(|name| name.to_str()).unwrap_or("");
        let original_root = store.find_installed("sftpgo", Some(old_version)).map(|installed| {
            let entry = crate::install::Installer::effective(paths).installed_entry(&installed);
            PathBuf::from(installed.install_path).join(entry_relative_path(&entry.entry)).parent().map(PathBuf::from).unwrap()
        }).unwrap_or_else(|| r.root.clone());
        sftpgo_config_file(&original_root)?.ok_or_else(|| AppError::new("SFTPGO_CONFIG_MISSING", "SFTPGo portable 包缺少默认配置")
            .with_hint("请重新安装完整 portable 包；现有数据库和主机密钥保持不变。"))?
    };
    let relative = source.strip_prefix(&paths.base).map_err(|_| AppError::new("SFTPGO_CONFIG_PATH", "配置源必须位于托管数据目录"))?;
    let source = crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
    let metadata = std::fs::metadata(&source)?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 { return Err(AppError::new("SFTPGO_CONFIG_INVALID", "SFTPGo 配置必须是小于 1 MiB 的文本文件")); }
    let content = std::fs::read_to_string(&source)?;
    let config: serde_json::Value = match source.extension().and_then(|v| v.to_str()) {
        Some("json") => serde_json::from_str(&content).map_err(|_| AppError::new("SFTPGO_CONFIG_INVALID", "SFTPGo JSON 配置语法错误，原文件已保留"))?,
        Some("yaml" | "yml") => yaml_serde::from_str(&content).map_err(|_| AppError::new("SFTPGO_CONFIG_INVALID", "SFTPGo YAML 配置语法错误，原文件已保留"))?,
        _ => return Err(AppError::new("SFTPGO_CONFIG_FORMAT", "托管 SFTPGo 配置目前支持 JSON 或 YAML，请使用自定义模块运行其他格式")),
    };
    if !config.is_object() { return Err(AppError::new("SFTPGO_CONFIG_INVALID", "SFTPGo 配置根节点必须是对象")); }
    let files = sftpgo_env_files(paths, &r.etc)?;
    let mut process_env = sftpgo_process_env(r)?;
    let mut effective_env = sftpgo_parse_env(&files, process_env.clone())?;
    let mut env = Vec::new();
    for (pointer, key, default) in [
        ("/httpd/templates_path", "SFTPGO_HTTPD__TEMPLATES_PATH", "templates"),
        ("/httpd/static_files_path", "SFTPGO_HTTPD__STATIC_FILES_PATH", "static"),
        ("/httpd/openapi_path", "SFTPGO_HTTPD__OPENAPI_PATH", "openapi"),
        ("/smtp/templates_path", "SFTPGO_SMTP__TEMPLATES_PATH", "templates"),
    ] {
        if !effective_env.contains_key(key) && config.pointer(pointer).is_none_or(|value| value.as_str() == Some(default)) {
            let value = r.root.join(default).to_string_lossy().into_owned();
            env.push((key.into(), value.clone())); process_env.insert(key.into(), value);
        }
    }
    // 默认资源路径也属于子进程环境，env.d 的插值必须能读取到相同的值。
    if !env.is_empty() { effective_env = sftpgo_parse_env(&files, process_env)?; }
    // 使用实际生效的环境配置检查本地状态，避免 env.d 使丢失检查被跳过。
    let mut state_files = Vec::new();
    if previously_started {
        let value = |key: &str, pointer: &str, default: &str| {
            effective_env.get(key).cloned()
                .unwrap_or_else(|| config.pointer(pointer).and_then(|value| value.as_str()).unwrap_or(default).to_string())
        };
        let mut require = |name: &str| -> Result<()> {
            let path = r.etc.join(name);
            let present = std::fs::metadata(&path).map(|m| m.is_file() && m.len() > 0).unwrap_or(false);
            if !present { return Err(AppError::new("SFTPGO_STATE_MISSING", "SFTPGo 原数据库或主机密钥缺失，未自动重新初始化")
                .with_hint(format!("请先恢复原文件：{}", path.display()))); }
            state_files.push(path);
            Ok(())
        };
        let driver = value("SFTPGO_DATA_PROVIDER__DRIVER", "/data_provider/driver", "sqlite");
        let connection = value("SFTPGO_DATA_PROVIDER__CONNECTION_STRING", "/data_provider/connection_string", "");
        if matches!(driver.as_str(), "bolt" | "sqlite") && connection.is_empty() {
            require(&value("SFTPGO_DATA_PROVIDER__NAME", "/data_provider/name", "sftpgo.db"))?;
        }
        let keys: Vec<_> = if let Some(value) = effective_env.get("SFTPGO_SFTPD__HOST_KEYS") {
            if value.is_empty() { vec![] } else { value.split(',').collect() }
        } else { config.pointer("/sftpd/host_keys").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()).collect() };
        if keys.is_empty() { for name in ["id_rsa", "id_ecdsa", "id_ed25519"] { require(name)?; } }
        else { for name in keys { require(name)?; } }
    }
    let file = existing.unwrap_or_else(|| r.etc.join(source.file_name().unwrap()));
    if write_config && file != source {
        crate::paths::write_with_backup_expected(&file, &content, &paths.backup(), Some(None))?;
    }
    let web_target = sftpgo_web_target(r, &config, &effective_env);
    Ok(SftpgoConfig { file, env, web_target, state_files })
}

fn web_unavailable(hint: &str) -> AppError {
    AppError::new("SERVICE_WEB_UNAVAILABLE", "无法确定此服务的管理台入口").with_hint(hint)
}

/// 只生成本机回环链接；不把远端地址、凭据或原始配置拼进 URL。
fn local_web_url(address: &str, port: u16, https: bool, path: &str) -> Result<String> {
    let host = match address {
        "" | "0.0.0.0" | "localhost" => "127.0.0.1".to_string(),
        "::" | "[::]" => "[::1]".to_string(),
        other => {
            let ip: std::net::IpAddr = other.trim_matches(['[', ']']).parse()
                .map_err(|_| web_unavailable("管理台未使用本机回环监听，请按服务配置访问。"))?;
            if !ip.is_loopback() { return Err(web_unavailable("管理台绑定了指定网卡地址，请按服务配置访问。")); }
            match ip { std::net::IpAddr::V4(ip) => ip.to_string(), std::net::IpAddr::V6(ip) => format!("[{ip}]") }
        }
    };
    if port == 0 { return Err(web_unavailable("管理台端口未启用。")); }
    let mut url = reqwest::Url::parse(&format!("{}://{host}:{port}/", if https { "https" } else { "http" }))
        .map_err(|_| web_unavailable("管理台地址无效，请检查服务配置。"))?;
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment { "" | "." => {}, ".." => { segments.pop(); }, other => segments.push(other) }
    }
    url.path_segments_mut().map_err(|_| web_unavailable("管理台路径无效。"))?.clear().extend(segments);
    Ok(url.to_string())
}

fn sftpgo_web_target(r: &Resolved, config: &serde_json::Value, environment: &std::collections::HashMap<String, String>) -> Result<String> {
    let value = |key: &str, pointer: &str, default: &str| -> Result<String> {
        if let Some(value) = environment.get(key) { return Ok(value.clone()); }
        Ok(config.pointer(pointer).map(|value| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string())).unwrap_or_else(|| default.into()))
    };
    let boolean = |key: &str, pointer: &str, default: bool| -> Result<bool> {
        // 上游 strconv.ParseBool 不接受任意混合大小写；非法环境覆盖会被忽略。
        let parse = |value: &str| match value {
            "true" | "True" | "TRUE" | "t" | "T" | "1" => Some(true),
            "false" | "False" | "FALSE" | "f" | "F" | "0" => Some(false), _ => None,
        };
        let parsed = environment.get(key).and_then(|value| parse(value));
        if let Some(value) = parsed { return Ok(value); }
        config.pointer(pointer).map(|value| value.as_bool().or_else(|| value.as_str().and_then(parse))
            .ok_or_else(|| web_unavailable("管理台开关配置无法识别，请检查配置后重启。")))
            .unwrap_or(Ok(default))
    };
    if !boolean("SFTPGO_HTTPD__BINDINGS__0__ENABLE_WEB_ADMIN", "/httpd/bindings/0/enable_web_admin", true)? {
        return Err(web_unavailable("SFTPGo Web Admin 已关闭；如需管理台，请启用后重启服务。"));
    }
    let address = value("SFTPGO_HTTPD__BINDINGS__0__ADDRESS", "/httpd/bindings/0/address", "")?;
    let https = boolean("SFTPGO_HTTPD__BINDINGS__0__ENABLE_HTTPS", "/httpd/bindings/0/enable_https", false)?;
    let root = value("SFTPGO_HTTPD__WEB_ROOT", "/httpd/web_root", "")?;
    let root = if root.starts_with('/') { root.as_str() } else { "" };
    let port = r.port.and_then(|port| port.checked_add(6058)).ok_or_else(|| web_unavailable("管理台派生端口超出范围。"))?;
    local_web_url(&address, port, https, &format!("{root}/web/admin"))
}

/// 仅识别由本程序明确传入监听端口的运行描述；不猜测自定义命令的端口。
fn generic_web_target(r: &Resolved, sftpgo: Option<&SftpgoConfig>) -> Result<String> {
    if let Some(config) = sftpgo { return config.web_target.clone(); }
    if crate::install::official_qdrant(&r.entry) && r.entry.run.as_ref().is_some_and(|run| run.args == r.spec.args) { return qdrant_web_target(r); }
    let args_pair = |flag: &str, value: &str| r.spec.args.windows(2).any(|pair| pair[0] == flag && pair[1] == value);
    let (offset, path) = match r.entry.id.as_str() {
        "mailpit" if args_pair("--listen", "127.0.0.1:{port}") => (0, "/"),
        "minio" if args_pair("--console-address", "127.0.0.1:{port+1}") => (1, "/"),
        "consul" if args_pair("-http-port", "{port}") && args_pair("-client", "127.0.0.1") => (0, "/ui/"),
        "rnacos" if managed_rnacos(r) => (2000, "/rnacos/"),
        "qdrant" if args_pair("--config-path", "{etc}/config.yaml") => (0, "/dashboard/"),
        _ => return Err(web_unavailable("当前运行配置没有已知的管理台入口，请按服务配置访问。")),
    };
    let port = r.port.and_then(|port| port.checked_add(offset)).ok_or_else(|| web_unavailable("管理台派生端口超出范围。"))?;
    if r.entry.id == "mailpit" {
        let root = r.spec.args.windows(2).find(|pair| pair[0] == "--webroot").map(|pair| expand(&pair[1], r))
            .or_else(|| r.spec.args.iter().find_map(|arg| arg.strip_prefix("--webroot=").map(|value| expand(value, r))))
            .or_else(|| r.spec.env.as_ref().and_then(|env| env.get("MP_WEBROOT")).map(|value| expand(value, r)))
            .or_else(|| std::env::var("MP_WEBROOT").ok()).unwrap_or_else(|| "/".into());
        return local_web_url("127.0.0.1", port, false, &root);
    }
    local_web_url("127.0.0.1", port, false, path)
}

fn qdrant_web_target(r: &Resolved) -> Result<String> {
    let config: serde_json::Value = yaml_serde::from_str(&std::fs::read_to_string(r.etc.join("config.yaml"))?)
        .map_err(|_| web_unavailable("Qdrant 配置无法解析，请检查后重启。"))?;
    let value = |key: &str, pointer: &str, fallback: &str| {
        r.spec.env.as_ref().and_then(|env| env.get(key)).map(|value| expand(value, r))
            .or_else(|| std::env::var(key).ok())
            .or_else(|| config.pointer(pointer).filter(|value| !value.is_null()).map(|value| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string())))
            .unwrap_or_else(|| fallback.into())
    };
    let enabled = value("QDRANT__SERVICE__ENABLE_STATIC_CONTENT", "/service/enable_static_content", "true");
    if enabled != "true" { return Err(web_unavailable("Qdrant 静态管理台未启用，请检查 enable_static_content 后重启。")); }
    let content = value("QDRANT__SERVICE__STATIC_CONTENT_DIR", "/service/static_content_dir", "./static");
    let directory = r.root.join(&content);
    if !directory.join("index.html").is_file() {
        if matches!(content.as_str(), "static" | "./static") && !directory.exists() {
            return Err(AppError::new("QDRANT_WEB_MISSING", "此 Qdrant 安装尚未包含管理台文件")
                .with_hint("可下载官方管理台并重启服务；数据库和配置会保留。"));
        }
        return Err(web_unavailable("Qdrant 静态目录缺少 index.html，请检查 static_content_dir；已有文件不会自动覆盖。"));
    }
    let host = value("QDRANT__SERVICE__HOST", "/service/host", "127.0.0.1");
    let tls = value("QDRANT__SERVICE__ENABLE_TLS", "/service/enable_tls", "false");
    if !matches!(tls.as_str(), "true" | "false") { return Err(web_unavailable("Qdrant TLS 设置无效，请检查配置。")); }
    local_web_url(&host, r.port.ok_or_else(|| web_unavailable("Qdrant 端口未配置。"))?, tls == "true", "/dashboard")
}

fn qdrant_managed_snapshots(entry: &PackageManifestEntry) -> bool {
    crate::install::official_qdrant(entry) && entry.run.as_ref().is_some_and(|run| run.single_instance && run.data_dir.is_none())
}

/// 对快照设置按 Qdrant 的环境、显式配置、local、RUN_MODE、基础配置顺序读取。
/// 只读与数据保留有关的值；不能识别的配置格式在未被高优先级设置覆盖时明确报错。
fn qdrant_snapshot_setting(entry: &PackageManifestEntry, root: &std::path::Path, paths: &Paths,
    key: &str, pointer: &str, default: &str) -> Result<String> {
    let data = paths.data().join("qdrant"); let etc = paths.etc_dir("qdrant", &entry.version);
    let env_value = |key: &str| -> Result<Option<String>> {
        let values: Vec<_> = entry.run.as_ref().and_then(|run| run.env.as_ref()).into_iter().flatten()
            .filter(|(name, _)| if cfg!(windows) { name.eq_ignore_ascii_case(key) } else { name.as_str() == key }).map(|(_, value)| value).collect();
        if values.len() > 1 { return Err(AppError::new("QDRANT_CONFIG_ENV", "Qdrant 环境变量存在重复的大小写名称，请合并后重试")); }
        Ok(values.first().map(|value| value.replace("{root}", &crate::paths::nginx_path(root)).replace("{data}", &crate::paths::nginx_path(&data))
            .replace("{etc}", &crate::paths::nginx_path(&etc))).or_else(|| std::env::var(key).ok()))
    };
    if let Some(value) = env_value(key)? { return Ok(value); }
    let read = |path: &std::path::Path| -> Result<Option<String>> {
        let relative = path.strip_prefix(&paths.base).map_err(|_| AppError::new("QDRANT_CONFIG_PATH", "Qdrant 配置不在托管目录内"))?;
        let path = crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta, Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None), Err(e) => return Err(e.into()),
        };
        if !meta.is_file() || meta.len() > 1024 * 1024 { return Err(AppError::new("QDRANT_CONFIG_READ", "Qdrant 配置必须是小于 1 MiB 的文本文件")); }
        let config: serde_json::Value = yaml_serde::from_str(&std::fs::read_to_string(&path)?)
            .map_err(|_| AppError::new("QDRANT_CONFIG_INVALID", "无法解析 Qdrant 配置，未改动快照").with_hint(path.display().to_string()))?;
        config.pointer(pointer).map(|value| value.as_str().map(str::to_string)
            .ok_or_else(|| AppError::new("QDRANT_CONFIG_INVALID", "Qdrant 快照设置必须是有效的文本值"))).transpose()
    };
    if let Some(value) = read(&etc.join("config.yaml"))? { return Ok(value); }
    let mode = env_value("RUN_MODE")?.unwrap_or_else(|| "development".into());
    if mode.is_empty() || !mode.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)) {
        return Err(AppError::new("QDRANT_CONFIG_MODE", "无法确认自定义 RUN_MODE 的快照设置，请在主配置中明确设置快照路径"));
    }
    for name in ["local", mode.as_str(), "config"] {
        let candidates: Vec<_> = ["yaml", "yml", "json", "toml", "ini", "ron", "json5"]
            .iter().map(|ext| root.join("config").join(format!("{name}.{ext}"))).filter(|path| path.exists()).collect();
        if candidates.len() > 1 || candidates.first().is_some_and(|path| !matches!(path.extension().and_then(|s| s.to_str()), Some("yaml" | "yml" | "json"))) {
            return Err(AppError::new("QDRANT_CONFIG_AMBIGUOUS", "无法确认 Qdrant 叠加配置的快照设置，未改动快照")
                .with_hint("请在 etc/qdrant 对应版本的 config.yaml 中明确设置 storage.snapshots_path 和 storage.snapshots_config.snapshots_storage。"));
        }
        if let Some(path) = candidates.first() { if let Some(value) = read(path)? { return Ok(value); } }
    }
    Ok(default.into())
}

fn qdrant_snapshot_directory(entry: &PackageManifestEntry, root: &std::path::Path, paths: &Paths) -> Result<PathBuf> {
    let value = qdrant_snapshot_setting(entry, root, paths, "QDRANT__STORAGE__SNAPSHOTS_PATH", "/storage/snapshots_path", "./snapshots")?;
    if value.is_empty() { return Err(AppError::new("QDRANT_SNAPSHOT_PATH", "Qdrant 快照目录不能为空")); }
    let mut normalized = PathBuf::new();
    for part in root.join(value).components() {
        match part {
            std::path::Component::CurDir => {},
            std::path::Component::ParentDir => { if !normalized.pop() { return Err(AppError::new("QDRANT_SNAPSHOT_PATH", "Qdrant 快照路径无效")); } },
            part => normalized.push(part.as_os_str()),
        }
    }
    Ok(normalized)
}

fn qdrant_path_key(path: &std::path::Path) -> String {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text).trim_end_matches('/');
    if cfg!(windows) { text.to_lowercase() } else { text.into() }
}

fn qdrant_snapshot_env(store: &Store, paths: &Paths, manager: &ServiceManager, r: &Resolved) -> Result<Vec<(String, String)>> {
    if !qdrant_managed_snapshots(&r.entry) { return Ok(vec![]); }
    let location = qdrant_snapshot_directory(&r.entry, &r.root, paths)?;
    let storage = qdrant_snapshot_setting(&r.entry, &r.root, paths, "QDRANT__STORAGE__SNAPSHOTS_CONFIG__SNAPSHOTS_STORAGE",
        "/storage/snapshots_config/snapshots_storage", "local")?;
    if !matches!(storage.as_str(), "local" | "s3") { return Err(AppError::new("QDRANT_SNAPSHOT_CONFIG", "Qdrant 快照存储类型必须是 local 或 s3")); }
    preserve_qdrant_snapshots(store, paths, manager, None)?;
    let target = crate::paths::checked_data_path(&paths.base, "data/qdrant/snapshots")?;
    if storage == "local" && (qdrant_path_key(&location) == qdrant_path_key(&r.root.join("snapshots"))
        || qdrant_path_key(&location) == qdrant_path_key(&target)) {
        std::fs::create_dir_all(&target)?;
        // 原文件的注释和自定义设置不重写；只修正上游默认的易丢失路径。
        return Ok(vec![("QDRANT__STORAGE__SNAPSHOTS_PATH".into(), target.to_string_lossy().into_owned())]);
    }
    Ok(vec![])
}

/// 将旧程序目录的默认快照汇入独立数据目录，再把原目录完整留作备份。
/// 先检查所有重名文件；新文件独占发布，失败可以重试，绝不覆盖已有快照。
pub(crate) fn preserve_qdrant_snapshots(store: &Store, paths: &Paths, manager: &ServiceManager,
    uninstall: Option<&InstalledPackage>) -> Result<()> {
    use std::collections::BTreeMap;
    let destination = crate::paths::checked_data_path(&paths.base, "data/qdrant/snapshots")?;
    let installer = crate::install::Installer::bundled();
    let packages = match uninstall { Some(installed) => vec![installed.clone()], None => store.list_installed()? };
    let mut sources = Vec::new();
    for installed in packages.into_iter().filter(|p| p.id == "qdrant") {
        let entry = installer.installed_entry(&installed);
        if !qdrant_managed_snapshots(&entry) { continue; }
        let binary = PathBuf::from(&installed.install_path).join(entry_relative_path(&entry.entry));
        let root = binary.parent().ok_or_else(|| AppError::new("QDRANT_SNAPSHOT_PATH", "Qdrant 程序路径无效"))?;
        if uninstall.is_some() {
            let configured = qdrant_snapshot_directory(&entry, root, paths)?;
            let runtime = paths.runtime_dir("qdrant", &installed.version);
            let configured_key = qdrant_path_key(&configured); let runtime_key = qdrant_path_key(&runtime);
            if (configured_key == runtime_key || configured_key.starts_with(&format!("{runtime_key}/")))
                && configured_key != qdrant_path_key(&root.join("snapshots")) && configured.exists() {
                return Err(AppError::new("QDRANT_SNAPSHOT_IN_RUNTIME", "自定义快照仍保存在待卸载的程序目录中，已中止卸载")
                    .with_hint(format!("请先把 {} 移到程序目录之外，并更新 storage.snapshots_path 后再卸载。", configured.display())));
            }
        }
        let source = root.join("snapshots");
        let relative = source.strip_prefix(&paths.base).map_err(|_| AppError::new("QDRANT_SNAPSHOT_PATH", "快照路径超出托管目录"))?;
        let source = crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
        if source.exists() && std::fs::read_dir(&source)?.next().transpose()?.is_some() { sources.push((installed.version, source)); }
    }
    if sources.is_empty() { return Ok(()); }
    if manager.snapshot("qdrant").is_some_and(|s| s.pids.iter().any(|pid| platform::process_alive(*pid))) {
        return Err(AppError::new("SERVICE_BUSY", "请先停止 Qdrant，再保留旧版快照或卸载此版本"));
    }
    fn scan(root: &std::path::Path, relative: &str, depth: usize, files: &mut Vec<(String, PathBuf)>) -> Result<()> {
        if depth > 32 || files.len() > 100_000 { return Err(AppError::new("QDRANT_SNAPSHOT_LIMIT", "快照目录层级或文件数量过多，请手动整理后重试")); }
        let directory = if relative.is_empty() { root.to_path_buf() } else { crate::paths::checked_data_path(root, relative)? };
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|_| AppError::new("QDRANT_SNAPSHOT_PATH", "快照文件名不是有效的 UTF-8"))?;
            let key = if relative.is_empty() { name } else { format!("{relative}/{name}") };
            let path = crate::paths::checked_data_path(root, &key)?;
            let meta = std::fs::metadata(&path)?;
            if meta.is_dir() { scan(root, &key, depth + 1, files)?; }
            else if meta.is_file() { files.push((key, path)); }
            else { return Err(AppError::new("QDRANT_SNAPSHOT_PATH", "快照目录包含特殊文件，未移动原目录")); }
        }
        Ok(())
    }
    let mut plan: BTreeMap<String, (String, PathBuf, String)> = BTreeMap::new();
    let conflict = |key: &str| AppError::new("QDRANT_SNAPSHOT_CONFLICT", format!("同名快照内容不同，未覆盖：{key}"))
        .with_hint("请备份并重命名冲突文件后重试；原程序目录和已有快照均保留。");
    let mut originals = Vec::new();
    for (_, source) in &sources {
        let mut files = Vec::new(); scan(source, "", 0, &mut files)?;
        for (key, path) in files {
            let digest = crate::download::sha256_file(&path)?;
            let folded = if cfg!(windows) { key.to_lowercase() } else { key.clone() };
            if let Some((_, _, previous)) = plan.get(&folded) { if previous != &digest { return Err(conflict(&key)); } }
            else { plan.insert(folded, (key, path.clone(), digest.clone())); }
            originals.push((path, digest));
        }
    }
    for (key, _, digest) in plan.values() {
        let mut parent = std::path::Path::new(key).parent();
        while let Some(path) = parent.filter(|path| !path.as_os_str().is_empty()) {
            let key = crate::paths::nginx_path(path); let folded = if cfg!(windows) { key.to_lowercase() } else { key.clone() };
            if plan.contains_key(&folded) { return Err(conflict(&key)); }
            parent = path.parent();
        }
        let target = crate::paths::checked_data_path(&destination, key)?;
        if target.exists() && (!target.is_file() || crate::download::sha256_file(&target)? != *digest) { return Err(conflict(key)); }
    }
    std::fs::create_dir_all(&destination)?;
    let staging = tempfile::Builder::new().prefix(".qdrant-snapshots-").tempdir_in(destination.parent().unwrap())?;
    let mut ready = Vec::new();
    for (key, source, digest) in plan.values() {
        let target = crate::paths::checked_data_path(&destination, key)?;
        if target.is_file() { continue; }
        let mut temporary = tempfile::NamedTempFile::new_in(staging.path())?;
        std::io::copy(&mut std::fs::File::open(source)?, temporary.as_file_mut())?;
        temporary.as_file().sync_all()?;
        if crate::download::sha256_file(temporary.path())? != *digest { return Err(conflict(key)); }
        ready.push((key.clone(), temporary));
    }
    // 防止复制期间有外部进程改写原快照；验证失败时不挪走来源目录。
    for (source, digest) in &originals { if crate::download::sha256_file(source)? != *digest { return Err(conflict(&source.display().to_string())); } }
    for (key, temporary) in ready {
        let target = crate::paths::checked_data_path(&destination, &key)?;
        std::fs::create_dir_all(target.parent().unwrap())?;
        temporary.persist_noclobber(target).map_err(|e| AppError::io("保留 Qdrant 快照", e.error))?;
    }
    let backups = crate::paths::checked_data_path(&paths.base, "backup")?;
    std::fs::create_dir_all(&backups)?;
    for (version, source) in sources {
        crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(source.strip_prefix(&paths.base)
            .map_err(|_| AppError::new("QDRANT_SNAPSHOT_PATH", "快照路径超出托管目录"))?))?;
        let backup = tempfile::Builder::new().prefix(&format!("qdrant-snapshots-{version}-")).tempdir_in(&backups)?;
        std::fs::rename(&source, backup.path().join("snapshots")).map_err(|e| AppError::io("备份旧版 Qdrant 快照目录", e))?;
        let _ = backup.keep();
    }
    Ok(())
}

pub fn service_web_url(manager: &ServiceManager, id: &str) -> Result<String> {
    let before = manager.snapshot(id).ok_or_else(|| AppError::new("UNKNOWN_SERVICE", "该服务未安装或已卸载"))?;
    if before.state != crate::model::ServiceState::Running || before.pids.is_empty() {
        return Err(AppError::new("SERVICE_NOT_RUNNING", "请先启动服务，再打开管理台"));
    }
    let original = manager.web_target(id)?;
    let mut url = reqwest::Url::parse(&original).map_err(|_| web_unavailable("本次启动的管理台地址无效，请重启服务。"))?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some()
        || !url.host_str().and_then(|host| host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok()).is_some_and(|ip| ip.is_loopback()) {
        return Err(web_unavailable("管理台快捷入口仅支持本机 HTTP/HTTPS 监听地址。"));
    }
    let port = url.port_or_known_default().ok_or_else(|| web_unavailable("管理台端口无效。"))?;
    let owned = || -> Result<bool> {
        let listeners = crate::ports::listeners()?;
        Ok(listeners.iter().any(|(p, pid)| *p == port && before.pids.contains(pid))
            && listeners.iter().filter(|(p, _)| *p == port).all(|(_, pid)| before.pids.contains(pid)))
    };
    if !owned()? { return Err(web_unavailable("管理台端口尚未就绪或已由其他进程占用，请查看服务日志。")); }
    // 仅探测本机受管进程，不携带凭据、不跟随跳转；自签证书仍由浏览器正常提示。
    let client = reqwest::blocking::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .danger_accept_invalid_certs(true).timeout(Duration::from_millis(1500)).build()
        .map_err(|_| web_unavailable("无法创建本机管理台连接。"))?;
    let mut reachable = None;
    for scheme in [url.scheme().to_string(), if url.scheme() == "http" { "https".into() } else { "http".into() }] {
        url.set_scheme(&scheme).map_err(|_| web_unavailable("管理台协议无效。"))?;
        url.set_port(Some(port)).map_err(|_| web_unavailable("管理台端口无效。"))?;
        if let Ok(response) = client.get(url.clone()).send() {
            let status = response.status();
            let html = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/html"));
            if (status.is_success() && html) || (status.is_redirection() && response.headers().contains_key(reqwest::header::LOCATION))
                || status == reqwest::StatusCode::UNAUTHORIZED {
                reachable = Some(url.to_string()); break;
            }
        }
    }
    let current = manager.snapshot(id).ok_or_else(|| AppError::new("SERVICE_NOT_RUNNING", "服务已停止，请重新启动后重试"))?;
    if current.state != crate::model::ServiceState::Running || current.pids != before.pids || !owned()? {
        return Err(AppError::new("SERVICE_NOT_RUNNING", "管理台检查期间服务状态发生变化，请重试"));
    }
    reachable.ok_or_else(|| web_unavailable("管理台未返回可访问的网页；请检查是否启用了 Web 界面、自定义路径或访问限制。"))
}

/// 验证整个端口组，不能把溢出的派生端口钳到 1/65535，也不能只检查主端口。
fn select_port(store: &Store, r: &Resolved) -> Result<Option<u16>> {
    let Some(desired) = r.port else { return Ok(None) };
    let mut offsets = std::collections::BTreeSet::from([0i32]);
    let mut adjustable = false;
    for text in port_templates(&r.spec) {
        for (index, _) in text.match_indices("{port") {
            if text[index + 5..].starts_with(['}', '+', '-'])
                && PORT_TOKEN.find(&text[index..]).is_none_or(|m| m.start() != 0) {
                return Err(AppError::new("BAD_PORT", "服务端口占位符无效，请检查模块配置"));
            }
        }
        for token in PORT_TOKEN.captures_iter(text) {
            adjustable = true;
            let offset = token.get(1).map(|m| m.as_str().parse::<i32>()).transpose()
                .map_err(|_| AppError::new("BAD_PORT", "服务端口偏移无效"))?.unwrap_or(0);
            offsets.insert(offset);
        }
    }
    let ports = |base: u16| -> Option<Vec<u16>> {
        offsets.iter().map(|offset| i32::from(base).checked_add(*offset)
            .and_then(|value| u16::try_from(value).ok()).filter(|p| *p > 0)).collect()
    };
    let available = |base| ports(base).is_some_and(|ports| ports.into_iter().all(|port|
        tcp_port_bindable(port) && (!needs_udp(r, base, port) || std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok())));
    let original = ports(desired).ok_or_else(|| AppError::new("BAD_PORT", "主端口及派生端口必须位于 1–65535，请调整服务端口"))?;
    if available(desired) { return Ok(Some(desired)); }
    let enabled = match store.get_setting_checked("autoFallbackPort")?.as_deref() {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err(AppError::new("BAD_SETTING", "自动回落端口设置无效，请重新选择")),
    };
    let initial_safe = !is_standard(store) && store.get_port_assign_checked(&r.service_id)?.is_none()
        && store.get_setting_checked(&format!("portOverride.{}", r.service_id))?.is_none();
    if (enabled || initial_safe) && adjustable {
        for offset in 1..=200 {
            if let Some(port) = desired.checked_add(offset).filter(|p| available(*p)) { return Ok(Some(port)); }
        }
        for _ in 0..32 {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
            let port = listener.local_addr()?.port(); drop(listener);
            if available(port) { return Ok(Some(port)); }
        }
    }
    for port in original {
        precheck_port(port, &r.entry.display_name)?;
        if needs_udp(r, desired, port) {
            std::net::UdpSocket::bind(("127.0.0.1", port))
                .map_err(|e| AppError::port_conflict(port, Some("UDP 端口不可用")).with_detail(e.to_string()))?;
        }
    }
    Ok(Some(desired))
}

/// 只替换模板声明的端口数字，保留其余自定义行、缩进、注释和换行符。
/// INI 按节、YAML 按父级路径匹配；缺失或多义时停止，不能覆盖其他配置段的同名项。
fn sync_config_ports(current: &str, template: &str, r: &Resolved) -> Result<String> {
    let mut lines: Vec<String> = current.split_inclusive('\n').map(str::to_owned).collect();
    let scopes = |content: &str| -> Vec<String> {
        let file = r.spec.config_file.as_deref().unwrap_or("").to_ascii_lowercase();
        let ini = file.ends_with(".ini") || file.ends_with(".cnf");
        let yaml = file.ends_with(".yaml") || file.ends_with(".yml");
        let mut section = String::new(); let mut parents: Vec<(usize, String)> = Vec::new();
        content.lines().map(|line| {
            let trimmed = line.trim();
            if ini {
                if let Some((name, _)) = trimmed.strip_prefix('[').and_then(|s| s.split_once(']')) { section = name.trim().to_ascii_lowercase(); }
            }
            if yaml && !trimmed.is_empty() && !trimmed.starts_with('#') {
                let indent = line.len() - line.trim_start().len();
                while parents.last().is_some_and(|(depth, _)| *depth >= indent) { parents.pop(); }
                section = parents.iter().map(|(_, key)| key.as_str()).collect::<Vec<_>>().join("/");
                if let Some((key, value)) = trimmed.split_once(':') {
                    if (value.trim().is_empty() || value.trim().starts_with('#')) && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                        parents.push((indent, key.to_string()));
                    }
                }
            }
            section.clone()
        }).collect()
    };
    let current_scopes = scopes(current); let template_scopes = scopes(template);
    for (line_index, line) in template.lines().enumerate() {
        let line = line.trim();
        let tokens: Vec<_> = PORT_TOKEN.captures_iter(line).collect();
        if tokens.is_empty() { continue; }
        let literal = |text: &str| regex::escape(&expand_config(text, r))
            .replace(' ', "[ \\t]*").replace('\t', "[ \\t]*").replace('=', "[ \\t]*=[ \\t]*");
        let mut pattern = String::from(r"^[ \t]*"); let mut end = 0;
        for token in &tokens {
            let matched = token.get(0).unwrap();
            pattern.push_str(&literal(&line[end..matched.start()]));
            pattern.push_str(r"([0-9]{1,5})"); end = matched.end();
        }
        pattern.push_str(&literal(&line[end..])); pattern.push_str(r"[ \t]*(?:[#;].*)?$" );
        let regex = regex::Regex::new(&pattern).map_err(|_| AppError::new("CONFIG_PORT_SYNC", "无法解析服务端口模板"))?;
        let mut matches = Vec::new();
        for (index, current) in lines.iter().enumerate() {
            if template_scopes[line_index] == current_scopes[index] && regex.is_match(current.trim_end_matches(['\r', '\n'])) { matches.push(index); }
        }
        if matches.len() != 1 {
            return Err(AppError::new("CONFIG_PORT_SYNC", format!("{} 的配置中无法唯一识别托管端口项，原文件已保留", r.entry.display_name))
                .with_hint(format!("请检查 {} 中的 {}，保留模板的端口项结构后重试；其他自定义配置无需删除。", r.etc.join(r.spec.config_file.as_deref().unwrap_or("")).display(), line)));
        }
        let target = &mut lines[matches[0]];
        let captures = regex.captures(target.trim_end_matches(['\r', '\n'])).unwrap();
        let mut replacements = Vec::new();
        for (index, token) in tokens.iter().enumerate() {
            let value = expand(token.get(0).unwrap().as_str(), r);
            let matched = captures.get(index + 1).unwrap();
            replacements.push((matched.range(), value));
        }
        for (range, value) in replacements.into_iter().rev() { target.replace_range(range, &value); }
    }
    Ok(lines.concat())
}

fn managed_rnacos(r: &Resolved) -> bool {
    r.entry.id == "rnacos" && r.spec.args == ["-e", "{etc}/.env"]
        && r.spec.config_file.as_deref() == Some(".env") && r.spec.health == "tcp"
        && r.spec.env.as_ref().is_some_and(|env| [
            ("RNACOS_HTTP_PORT", "{port}"),
            ("RNACOS_GRPC_PORT", "{port+1000}"),
            ("RNACOS_HTTP_CONSOLE_PORT", "{port+2000}"),
        ].iter().all(|(key, value)| env.get(*key).map(String::as_str) == Some(*value)))
}

/// 修正旧模板生成的未引用路径；只匹配受管路径的原始整行，保留自定义值和注释。
fn quote_legacy_rnacos_paths(content: &str, r: &Resolved) -> String {
    content.split_inclusive('\n').map(|line| {
        let body = line.trim_end_matches(['\r', '\n']);
        for (key, suffix) in [
            ("RNACOS_DATA_DIR", "nacos_db"),
            ("RNACOS_CONFIG_DB_FILE", "nacos_db/config.db"),
            ("RNACOS_NAMING_DB_FILE", "nacos_db/naming.db"),
        ] {
            let value = expand_config(&format!("{{data}}/{suffix}"), r);
            if body == format!("{key}={value}") || body == format!("{key}=\"{value}\"") {
                let escaped = value.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$");
                return format!("{key}=\"{escaped}\"{}", &line[body.len()..]);
            }
        }
        line.to_string()
    }).collect()
}

#[allow(deprecated)] // from_path 会修改整个应用的环境；这里只使用上游提供的只读迭代器。
fn rnacos_config_env(content: &str, directory: &std::path::Path) -> Result<Vec<(String, String)>> {
    use std::io::Write;
    let invalid = || AppError::new("CONFIG_ENV_INVALID", "r-nacos 的 .env 配置无效，未启动服务")
        .with_hint("请检查赋值格式、引号、重复配置项或不可见字符；原文件保持不变。配置值不会写入错误信息。");
    let mut keys = std::collections::HashSet::new();
    let mut env = Vec::new();
    // 上游 dotenv 0.15 仅提供文件迭代器；暂存后完整解析，失败或完成都自动删除。
    // 只验证语法；实际变量插值留给 r-nacos，才能使用子进程最终的托管端口环境。
    let mut staged = tempfile::NamedTempFile::new_in(directory)?;
    staged.write_all(content.as_bytes())?;
    for item in dotenv::from_path_iter(staged.path()).map_err(|_| invalid())? {
        let (key, value) = item.map_err(|_| invalid())?;
        let unique = if cfg!(windows) { key.to_ascii_uppercase() } else { key.clone() };
        if key.contains('\0') || value.contains('\0') || !keys.insert(unique) { return Err(invalid()); }
        env.push((key, value));
    }
    Ok(env)
}

fn prepare_config(paths: &Paths, r: &Resolved) -> Result<Vec<(String, String)>> {
    let mut env = Vec::new();
    if let (Some(cf), Some(tpl)) = (&r.spec.config_file, &r.spec.config_template) {
        let path = r.etc.join(cf);
        let relative = path.strip_prefix(&paths.base).map_err(|_| AppError::new("CONFIG_PATH", "配置路径必须位于数据目录内"))?;
        let path = crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
        let previous = match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() && meta.len() <= 1024 * 1024 => Some(std::fs::read_to_string(&path)?),
            Ok(_) => return Err(AppError::new("CONFIG_READ", "配置必须是小于 1 MiB 的文本文件")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let mut content = match &previous { Some(current) => sync_config_ports(current, tpl, r)?, None => expand_config(tpl, r) };
        if managed_rnacos(r) {
            content = quote_legacy_rnacos_paths(&content, r);
            env = rnacos_config_env(&content, &r.etc)?;
        }
        crate::paths::write_with_backup_expected(&path, &content, &paths.backup(), Some(previous.as_deref().map(str::as_bytes)))?;
    }
    Ok(env)
}

/// r-nacos 的 SDK、gRPC 和控制台均应由刚启动的进程监听，不能借用其他进程的端口。
fn rnacos_ports_ready(manager: &ServiceManager, r: &Resolved) -> bool {
    let Some(port) = r.port else { return false; };
    let Some(ports) = [0, 1000, 2000].iter().map(|offset| port.checked_add(*offset)).collect::<Option<Vec<_>>>() else { return false; };
    owned_ports_ready(manager, &r.service_id, &ports)
}

fn owned_ports_ready(manager: &ServiceManager, service_id: &str, ports: &[u16]) -> bool {
    let pids = manager.snapshot(service_id).map(|s| s.pids).unwrap_or_default();
    if pids.is_empty() || !pids.iter().any(|pid| platform::process_alive(*pid)) { return false; }
    ports.iter().all(|port| tcp_port_open(*port)) && crate::ports::listeners().is_ok_and(|listeners|
        ports.iter().all(|port| listeners.iter().any(|(p, pid)| p == port && pids.contains(pid))
            && listeners.iter().filter(|(p, _)| p == port).all(|(_, pid)| pids.contains(pid))))
}

fn wait_sftpgo_healthy(manager: &ServiceManager, r: &Resolved, timeout: Duration) -> bool {
    let Some(ports) = r.port.and_then(|port| port.checked_add(6058).map(|web| [port, web])) else { return false; };
    wait_owned_ports(manager, &r.service_id, &ports, timeout)
}

fn wait_owned_ports(manager: &ServiceManager, id: &str, ports: &[u16], timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    let mut ready_since = None;
    while std::time::Instant::now() < deadline {
        if owned_ports_ready(manager, id, ports) {
            if ready_since.get_or_insert_with(std::time::Instant::now).elapsed() >= Duration::from_millis(300) { return true; }
        } else { ready_since = None; }
        if manager.snapshot(id).is_none_or(|service| service.pids.iter().all(|pid| !platform::process_alive(*pid))) { return false; }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

const RNACOS_START_MARKER: &str = "r-nacos：开始本次启动检查";

fn wait_consul_healthy(manager: &ServiceManager, r: &Resolved, timeout: Duration) -> bool {
    let Some(port) = r.port else { return false; };
    let Some(ports) = CONSUL_TCP_OFFSETS.iter().map(|offset| u16::try_from(i32::from(port) + offset).ok())
        .collect::<Option<Vec<_>>>() else { return false; };
    let Ok(client) = reqwest::blocking::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(700)).build() else { return false; };
    let deadline = std::time::Instant::now() + timeout;
    let mut ready_since = None;
    while std::time::Instant::now() < deadline {
        if manager.snapshot(&r.service_id).is_none_or(|service| service.pids.iter().all(|pid| !platform::process_alive(*pid))) { return false; }
        let ready = owned_ports_ready(manager, &r.service_id, &ports)
            && client.get(format!("http://127.0.0.1:{port}/v1/status/leader")).send().is_ok_and(|response|
                response.status().is_success() && response.json::<String>().is_ok_and(|leader| leader == format!("127.0.0.1:{}", ports[0])));
        if ready {
            if ready_since.get_or_insert_with(std::time::Instant::now).elapsed() >= Duration::from_millis(300) { return true; }
        } else { ready_since = None; }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn rnacos_startup_panicked(manager: &ServiceManager, id: &str) -> bool {
    manager.services.lock().get(id).is_some_and(|entry| entry.ring.lock().iter().rev()
        .take_while(|line| line.as_str() != RNACOS_START_MARKER)
        .any(|line| line.contains("panicked at ")))
}

fn wait_rnacos_healthy(manager: &ServiceManager, r: &Resolved, timeout: Duration) -> bool {
    let Some(port) = r.port else { return false; };
    let Ok(client) = reqwest::blocking::Client::builder().no_proxy()
        .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_millis(500)).build() else { return false; };
    let deadline = std::time::Instant::now() + timeout;
    let mut ready_since = None;
    while std::time::Instant::now() < deadline {
        if rnacos_startup_panicked(manager, &r.service_id) { return false; }
        let ready = rnacos_ports_ready(manager, r) && client.get(format!("http://127.0.0.1:{port}/health"))
            .send().is_ok_and(|response| response.status().is_success()
                && response.text().is_ok_and(|text| text.trim() == "success"));
        if ready {
            // 留出启动日志汇入的时间；上游工作线程 panic 后 HTTP 线程仍可能正常应答。
            if ready_since.get_or_insert_with(std::time::Instant::now).elapsed() >= Duration::from_millis(500) {
                return !rnacos_startup_panicked(manager, &r.service_id);
            }
        } else { ready_since = None; }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

pub fn start(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    service_id: &str,
    ports: &PortsProfile,
) -> Result<()> {
    let _ = ports;
    let mut r = resolve(store, paths, service_id)?;

    // 清单声明的运行时依赖必须先安装。
    for dep in &r.spec.requires {
        if crate::ops::installed_by_choice(store, dep).is_none() {
            return Err(AppError::new(
                "DEPENDENCY_MISSING",
                format!("{} 需要先安装 {}", r.entry.display_name, dep),
            )
            .with_hint(format!("到套件页安装 {dep} 后再启动")));
        }
    }

    // 先确定最终端口再生成配置、初始化和展开命令，所有阶段使用同一组端口。
    let planned = r.port;
    r.port = select_port(store, &r)?;
    prepare_config(paths, &r)?;
    let qdrant_env = qdrant_snapshot_env(store, paths, manager, &r)?;
    let sftpgo = if managed_sftpgo(&r.entry, &r.spec) { Some(prepare_sftpgo(store, paths, &r)?) } else { None };
    let web_target = generic_web_target(&r, sftpgo.as_ref());
    // CoreDNS 特例：Corefile 每次启动都重写——TLD 设置或转发策略变化要自动跟上，
    // 且通配解析模板含 {{ .Name }} 占位符，不能走通用模板渲染
    if r.entry.id == "coredns" {
        let tld = store
            .get_setting_checked("defaultTld")?
            .unwrap_or_else(|| "test".into());
        crate::dns::write_corefile(paths, &r.etc.join("Corefile"), &tld, &[])?;
    }

    // 启动前自建的数据子目录（如 Temurin/Qdrant 的 storage、RabbitMQ 的 mnesia）
    for d in &r.spec.init_dirs {
        std::fs::create_dir_all(r.data.join(d))?;
    }

    // 一次性初始化（MariaDB 的 install-db、Neo4j 的 set-initial-password 等）
    run_init_if_needed(&r)?;

    if let Some(port) = r.port { store.save_generic_port(&r.service_id, port, planned != r.port)?; }

    let mut args: Vec<String> = r.spec.args.iter().map(|a| expand(a, &r)).collect();
    if let Some(config) = &sftpgo { args.extend(["--config-file".into(), config.file.to_string_lossy().into_owned()]); }
    let cwd = r
        .spec
        .cwd
        .as_ref()
        .map(|c| PathBuf::from(expand(c, &r)))
        .unwrap_or_else(|| r.root.clone());
    let mut env: Vec<(String, String)> = sftpgo.as_ref().map(|config| config.env.clone()).unwrap_or_default();
    env.extend(r
        .spec
        .env
        .iter()
        .flatten()
        .map(|(k, v)| (k.clone(), expand(v, &r)))
        .collect::<Vec<_>>());
    env.extend(qdrant_env);

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
    if managed_rnacos(&r) { manager.push_log(&r.service_id, RNACOS_START_MARKER); }
    spawn_tracked(manager, &r.service_id, &spec)?;

    let timeout = Duration::from_secs(r.spec.health_timeout_sec.max(3));
    let healthy = if sftpgo.is_some() {
        wait_sftpgo_healthy(manager, &r, timeout)
    } else if crate::install::official_qdrant(&r.entry) {
        r.port.and_then(|port| port.checked_add(1).map(|grpc| [port, grpc]))
            .is_some_and(|ports| wait_owned_ports(manager, &r.service_id, &ports, timeout))
    } else if managed_rnacos(&r) {
        wait_rnacos_healthy(manager, &r, timeout)
    } else if managed_consul(&r.entry, &r.spec) {
        wait_consul_healthy(manager, &r, timeout)
    } else if r.entry.id == "coredns" {
        let tld = store.get_setting_checked("defaultTld")?.unwrap_or_else(|| "test".into());
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if crate::dns::probe(r.port.unwrap_or(53), &tld).is_ok()
                && manager.snapshot(&r.service_id).is_some_and(|s| s.pids.iter().any(|&pid| platform::process_alive(pid))) { break true; }
            if std::time::Instant::now() >= deadline { break false; }
            std::thread::sleep(Duration::from_millis(100));
        }
    } else { match r.spec.health.as_str() {
        "none" => true,
        "process" => wait_pids_alive(manager, &r.service_id, timeout),
        _ => match r.port {
            Some(port) => wait_healthy(port, timeout),
            None => wait_pids_alive(manager, &r.service_id, timeout),
        },
    }};
    if !healthy {
        let panicked = managed_rnacos(&r) && rnacos_startup_panicked(manager, &r.service_id);
        if r.entry.id == "coredns" || managed_rnacos(&r) || sftpgo.is_some() { crate::ops::stop_service(store, paths, manager, &r.service_id)?; }
        if panicked {
            return Err(AppError::new("SERVICE_RUNTIME_PANIC", "r-nacos 内部线程启动失败，已停止服务")
                .with_hint("请查看服务日志中的 panic 原因，或切换其他已安装版本；仅 HTTP 端口可连接不能证明配置中心可用。"));
        }
        return Err(AppError::new(
            "SERVICE_START_TIMEOUT",
            format!(
                "{} 启动超时（{}s 内{}）",
                r.entry.display_name,
                r.spec.health_timeout_sec,
                if managed_consul(&r.entry, &r.spec) {
                    "监听端口或单节点 leader 未就绪"
                } else if r.port.is_some() {
                    "端口未就绪"
                } else {
                    "进程未存活"
                }
            ),
        )
        .with_hint(format!(
            "查看日志页 {} 的最后输出；常见原因是端口冲突、缺少依赖运行库或配置不合法",
            r.service_id
        )));
    }
    if sftpgo.is_some() {
        let relative = r.etc.strip_prefix(&paths.base).map_err(|_| AppError::new("SFTPGO_CONFIG_PATH", "配置目录超出托管目录"))?;
        if let Err(error) = store.set_setting(SFTPGO_CONFIG_BINDING, &crate::paths::nginx_path(relative)) {
            crate::ops::stop_service(store, paths, manager, &r.service_id)?;
            return Err(error);
        }
    }
    if let Some(port) = r.port {
        manager.set_started_port(&r.service_id, port);
    }
    manager.set_web_target(&r.service_id, web_target);
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
    let init_exe = if cfg!(windows)
        && !init_exe
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
        && !init_exe.exists()
    {
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
    let _operation = manager.lifecycle.lock();
    let Ok(installed) = store.list_installed() else {
        return;
    };
    let installer = crate::install::Installer::effective(paths);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for p in installed {
        // 内置编排服务（nginx/mysql/php…）由 ops::register_services 注册，此处跳过
        if is_builtin(&p.id) {
            continue;
        }
        // 单实例服务只注册「使用中版本」；多实例逐版本注册
        let entry = installer.installed_entry(&p);
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
            port.as_ref().ok().copied().flatten(),
            paths.service_log(&service_id.replace('@', "_")),
        );
        if let Err(error) = port { manager.set_error(&service_id, error); }
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    fn fixture(id: &str) -> (tempfile::TempDir, crate::CoreState, Resolved) {
        fixture_version(id, None)
    }

    fn fixture_version(id: &str, version: Option<&str>) -> (tempfile::TempDir, crate::CoreState, Resolved) {
        let temp = tempfile::Builder::new().prefix("niceenv fixture ").tempdir().unwrap(); let paths = Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let state = crate::CoreState {
            store: Store::open(paths.db()).unwrap(), paths,
            installer: crate::install::Installer { manifest: serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap() },
            manager: Arc::new(ServiceManager::new()),
            downloader: Arc::new(crate::download::Downloader::new()), emit: Arc::new(|_| {}),
            watchdog: Arc::new(crate::watchdog::Watchdog::new()),
        };
        let mut entry = match version { Some(v) => state.installer.find(&format!("{id}@{v}")), None => state.installer.template_for(id) }.unwrap();
        entry.entry = crate::ops::exe_name("fixture");
        let runtime = state.paths.runtime_dir(id, &entry.version);
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(runtime.join(&entry.entry), b"fixture").unwrap();
        std::fs::write(runtime.join(".niceenv-package.json"), serde_json::to_vec(&entry).unwrap()).unwrap();
        state.store.upsert_installed(&InstalledPackage {
            id: id.into(), version: entry.version.clone(), category: entry.category.clone(),
            install_path: runtime.to_string_lossy().into(), config_path: String::new(), installed_at: 0,
        }).unwrap();
        let resolved = resolve(&state.store, &state.paths, id).unwrap();
        (temp, state, resolved)
    }

    #[test]
    fn template_ports_update_without_losing_custom_settings_or_comments() {
        for id in ["caddy", "mariadb", "qdrant", "rnacos"] {
            let (_temp, state, mut r) = fixture(id);
            r.port = Some(31000);
            let template = r.spec.config_template.as_ref().unwrap();
            let generated = expand_config(template, &r).replace('\n', "\r\n").replace("port=31000", "port = 31000 ; inline comment");
            let extra = if id == "mariadb" { "\r\n[custom]\r\nport=31000\r\nsetting=keep\r\n" } else { "\r\n# custom setting 31000 remains\r\n" };
            let current = format!("# custom heading\r\n{generated}{extra}");
            let output = r.etc.join(r.spec.config_file.as_ref().unwrap());
            std::fs::write(&output, &current).unwrap();
            r.port = Some(32000); prepare_config(&state.paths, &r).unwrap();
            let updated = std::fs::read_to_string(&output).unwrap();
            assert!(updated.contains("32000"), "{id}"); assert!(updated.starts_with("# custom heading\r\n"));
            assert!(updated.ends_with(extra));
            if id == "qdrant" { assert!(updated.contains("grpc_port: 32001")); }
            if id == "rnacos" { assert!(updated.contains("RNACOS_GRPC_PORT=33000")); }
            let count = crate::paths::list_backup_files(&state.paths.base).unwrap().len();
            prepare_config(&state.paths, &r).unwrap();
            assert_eq!(crate::paths::list_backup_files(&state.paths.base).unwrap().len(), count);
            assert_eq!(std::fs::read_to_string(output).unwrap(), updated);
        }
    }

    #[test]
    fn ambiguous_or_invalid_configs_never_replace_user_files_or_assign_ports() {
        let (_temp, state, mut r) = fixture("caddy"); r.port = Some(31000);
        let path = r.etc.join("Caddyfile");
        let generated = expand_config(r.spec.config_template.as_deref().unwrap(), &r);
        for current in ["# a completely custom server".to_string(), format!("{generated}\nhttp://:31000 {{\n respond \"other\"\n}}\n")] {
            std::fs::write(&path, &current).unwrap(); r.port = Some(32000);
            assert_eq!(prepare_config(&state.paths, &r).unwrap_err().code, "CONFIG_PORT_SYNC");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), current);
            assert!(state.store.get_port_assign("caddy").is_none());
        }
        std::fs::write(&path, [0xffu8, 0xfe]).unwrap(); assert!(prepare_config(&state.paths, &r).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [0xffu8, 0xfe]);
        r.spec.config_file = Some("../outside".into()); assert!(prepare_config(&state.paths, &r).is_err());
    }

    #[test]
    fn yaml_port_updates_only_touch_the_declared_service_section() {
        let (_temp, state, mut r) = fixture("qdrant"); r.port = Some(31000);
        let tpl = r.spec.config_template.as_ref().unwrap();
        let generated = expand_config(tpl, &r);
        let extra = "\ncustom:\n  http_port: 31000\n  grpc_port: 31001\n";
        let output = r.etc.join("config.yaml");
        std::fs::write(&output, format!("{generated}{extra}")).unwrap();
        r.port = Some(32000); prepare_config(&state.paths, &r).unwrap();
        let updated = std::fs::read_to_string(&output).unwrap();
        assert!(updated.contains("  http_port: 32000")); assert!(updated.ends_with(extra));
        let missing = generated.replace("  http_port: 31000\n", "") + extra;
        std::fs::write(&output, &missing).unwrap();
        assert_eq!(prepare_config(&state.paths, &r).unwrap_err().code, "CONFIG_PORT_SYNC");
        assert_eq!(std::fs::read_to_string(output).unwrap(), missing);
    }

    #[test]
    fn final_port_selection_covers_secondary_ports_and_never_commits_early() {
        let (_temp, state, mut r) = fixture("qdrant");
        let secondary = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = secondary.local_addr().unwrap().port(); r.port = Some(port - 1);
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        let selected = select_port(&state.store, &r).unwrap().unwrap();
        assert_ne!(selected, port - 1); assert_ne!(selected, port);
        assert!(tcp_port_bindable(selected)); assert!(tcp_port_bindable(selected + 1));
        assert!(state.store.get_port_assign("qdrant").is_none());
        assert!(state.store.get_setting("portOverride.qdrant").is_none());
        r.port = Some(selected); prepare_config(&state.paths, &r).unwrap();
        let config = std::fs::read_to_string(r.etc.join("config.yaml")).unwrap();
        assert!(config.contains(&format!("http_port: {selected}")));
        assert!(config.contains(&format!("grpc_port: {}", selected + 1)));
        r.port = Some(u16::MAX); assert_eq!(select_port(&state.store, &r).unwrap_err().code, "BAD_PORT");
        r.port = Some(31000); r.spec.args.push("--invalid={port+no}".into());
        assert_eq!(select_port(&state.store, &r).unwrap_err().code, "BAD_PORT");
        r.spec.args.pop(); r.spec.config_template = None; r.spec.args.clear();
        r.spec.env = Some(std::collections::HashMap::from([("PORT".into(), "{port-32000}".into())]));
        assert_eq!(select_port(&state.store, &r).unwrap_err().code, "BAD_PORT");
    }

    #[test]
    fn configured_ports_are_checked_and_fallback_records_are_atomic() {
        let (_temp, state, r) = fixture("caddy");
        state.store.set_port_assign("caddy", 31000).unwrap();
        state.store.set_port_override("caddy", Some(32000)).unwrap();
        assert_eq!(resolve_port(&state.store, "caddy", &r.entry, &r.spec).unwrap(), Some(32000));
        state.store.set_setting("portOverride.caddy", "invalid").unwrap();
        assert_eq!(resolve_port(&state.store, "caddy", &r.entry, &r.spec).unwrap_err().code, "BAD_PORT");
        state.store.set_port_override("caddy", None).unwrap();
        let database = rusqlite::Connection::open(state.paths.db()).unwrap();
        database.execute_batch("CREATE TRIGGER reject_fallback BEFORE INSERT ON settings WHEN NEW.key='portOverride.caddy' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(state.store.save_generic_port("caddy", 33000, true).is_err());
        assert_eq!(state.store.get_port_assign("caddy"), Some(31000));
        assert!(state.store.get_setting("portOverride.caddy").is_none());
        database.execute_batch("DROP TRIGGER reject_fallback;").unwrap();
        state.store.save_generic_port("caddy", 33000, true).unwrap();
        assert_eq!(state.store.get_port_assign("caddy"), Some(33000));
        assert_eq!(state.store.get_setting("portOverride.caddy").as_deref(), Some("33000"));
    }

    #[test]
    fn safe_profile_handles_bound_sockets_and_fixed_ports_are_not_faked() {
        let (_temp, state, mut r) = fixture("caddy");
        let socket = tokio::net::TcpSocket::new_v4().unwrap(); socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let occupied = socket.local_addr().unwrap().port(); r.port = Some(occupied);
        assert_eq!(select_port(&state.store, &r).unwrap_err().code, "PORT_UNAVAILABLE");
        state.store.set_setting("portProfile", "safe").unwrap();
        assert_ne!(select_port(&state.store, &r).unwrap(), r.port);
        r.spec.args.clear(); r.spec.config_template = None; r.spec.env = None;
        r.entry.default_port = Some(occupied);
        assert_eq!(resolve_port(&state.store, "caddy", &r.entry, &r.spec).unwrap(), Some(occupied));
        assert_eq!(select_port(&state.store, &r).unwrap_err().code, "PORT_UNAVAILABLE");
        state.store.set_port_override("caddy", Some(30000)).unwrap();
        assert_eq!(resolve_port(&state.store, "caddy", &r.entry, &r.spec).unwrap_err().code, "SERVICE_PORT_UNSUPPORTED");
    }

    #[test]
    fn rnacos_env_validation_preserves_files_and_never_changes_parent_environment() {
        let (_temp, state, mut r) = fixture("rnacos"); r.port = Some(32000);
        let config = r.etc.join(".env");
        let base = expand_config(r.spec.config_template.as_deref().unwrap(), &r);
        let legacy = base.replace("RNACOS_DATA_DIR=\"", "RNACOS_DATA_DIR=").replace("nacos_db\"", "nacos_db");
        let marker = format!("NICEENV_CONFIG_FIXTURE_{}", rand::random::<u64>());
        let content = format!("# keep user comment\r\n{}\r\n{marker}='literal $value'\r\n", legacy.replace('\n', "\r\n"));
        std::fs::write(&config, content).unwrap();
        let env = prepare_config(&state.paths, &r).unwrap();
        assert!(env.iter().any(|(k, v)| k == &marker && v == "literal $value"));
        assert!(std::env::var_os(&marker).is_none());
        assert!(std::fs::read_to_string(&config).unwrap().starts_with("# keep user comment\r\n"));
        assert!(env.iter().any(|(k, v)| k == "RNACOS_DATA_DIR" && v == &expand_config("{data}/nacos_db", &r)));
        for invalid in ["BROKEN='secret-unclosed", "CUSTOM=one\nCUSTOM=two", "CUSTOM=hidden\0value"] {
            let content = format!("{base}{invalid}\n");
            std::fs::write(&config, &content).unwrap();
            let error = prepare_config(&state.paths, &r).unwrap_err();
            assert_eq!(error.code, "CONFIG_ENV_INVALID");
            assert!(!format!("{error:?}").contains("secret-unclosed"));
            assert_eq!(std::fs::read_to_string(&config).unwrap(), content);
            assert!(state.store.get_port_assign("rnacos").is_none());
        }
    }

    #[test]
    fn consul_allocates_tcp_and_udp_as_one_port_group() {
        let (_temp, state, mut r) = fixture("consul");
        assert!(managed_consul(&r.entry, &r.spec));
        let manifest: crate::model::Manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap();
        for entry in manifest.packages.iter().filter(|entry| entry.id == "consul") { assert!(managed_consul(entry, entry.run.as_ref().unwrap())); }
        let base = (31000..41000).find(|base| CONSUL_TCP_OFFSETS.iter().all(|offset| {
            let port = (i32::from(*base) + offset) as u16;
            tcp_port_bindable(port) && std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok()
        })).unwrap();
        r.port = Some(base);
        for offset in CONSUL_UDP_OFFSETS {
            let port = (i32::from(base) + offset) as u16;
            let occupied = std::net::UdpSocket::bind(("127.0.0.1", port)).unwrap();
            assert!(select_port(&state.store, &r).is_err());
            state.store.set_setting("autoFallbackPort", "true").unwrap();
            assert_ne!(select_port(&state.store, &r).unwrap(), Some(base));
            assert!(state.store.get_port_assign("consul").is_none());
            state.store.set_setting("autoFallbackPort", "false").unwrap(); drop(occupied);
        }
        let occupied = std::net::TcpListener::bind(("127.0.0.1", base + 3)).unwrap();
        assert!(select_port(&state.store, &r).is_err()); drop(occupied);
        for port in [1, 200, 65500] { r.port = Some(port); assert_eq!(select_port(&state.store, &r).unwrap_err().code, "BAD_PORT"); }
        r.spec.args.push("-dev".into()); assert!(!managed_consul(&r.entry, &r.spec));
    }

    #[test]
    fn sftpgo_keeps_existing_state_directory_and_refuses_ambiguous_or_missing_data() {
        let (_temp, state, _r) = fixture("sftpgo");
        let old = state.paths.etc_dir("sftpgo", "2.7.5"); std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("sftpgo.db"), b"existing database").unwrap();
        std::fs::write(old.join("id_ed25519"), b"existing host key").unwrap();
        assert_eq!(sftpgo_config_dir(&state.store, &state.paths).unwrap(), old);
        let other = state.paths.etc_dir("sftpgo", "2.7.4"); std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("sftpgo.db"), b"another database").unwrap();
        assert_eq!(sftpgo_config_dir(&state.store, &state.paths).unwrap_err().code, "SFTPGO_CONFIG_AMBIGUOUS");
        state.store.set_setting(SFTPGO_CONFIG_BINDING, "etc/sftpgo/2.7.5").unwrap();
        assert_eq!(sftpgo_config_dir(&state.store, &state.paths).unwrap(), old);
        assert_eq!(std::fs::read(old.join("sftpgo.db")).unwrap(), b"existing database");
        assert_eq!(std::fs::read(old.join("id_ed25519")).unwrap(), b"existing host key");
        state.store.set_setting(SFTPGO_CONFIG_BINDING, "etc/sftpgo/missing").unwrap();
        assert_eq!(sftpgo_config_dir(&state.store, &state.paths).unwrap_err().code, "SFTPGO_CONFIG_MISSING");
        assert!(!state.paths.etc_dir("sftpgo", "missing").exists());
        for invalid in ["etc/sftpgo/../outside", "data/sftpgo/user", "etc/sftpgo/", "etc/sftpgo/.."] {
            state.store.set_setting(SFTPGO_CONFIG_BINDING, invalid).unwrap();
            assert!(sftpgo_config_dir(&state.store, &state.paths).is_err());
        }
    }

    #[test]
    fn sftpgo_directory_selection_is_read_only_until_confirmed_and_rechecks_state() {
        let (_temp, state, r) = fixture("sftpgo");
        for version in ["2.7.4", "2.7.5"] {
            let directory = state.paths.etc_dir("sftpgo", version); std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("sftpgo.json"), r#"{"data_provider":{"driver":"bolt","name":"accounts.db"},"sftpd":{"host_keys":["identity"]}}"#).unwrap();
            std::fs::write(directory.join("accounts.db"), version).unwrap();
            std::fs::write(directory.join("identity"), format!("key-{version}")).unwrap();
        }
        let report = state.sftpgo_config_directories().unwrap();
        assert!(report.current.is_none()); assert_eq!(report.directories.len(), 2);
        assert!(report.directories.iter().all(|item| item.issue.is_none() && item.state_files.len() == 2));
        assert!(!r.etc.join("sftpgo.json").exists()); assert!(!r.etc.join("accounts.db").exists());
        assert!(state.store.get_setting(SFTPGO_CONFIG_BINDING).is_none());
        assert_eq!(sftpgo_config_dir(&state.store, &state.paths).unwrap_err().code, "SFTPGO_CONFIG_AMBIGUOUS");
        for invalid in ["../outside", "etc/sftpgo/../2.7.5", "etc/sftpgo/2.7.5/extra", "etc/sftpgo/.."] {
            assert_eq!(state.select_sftpgo_config(invalid, &report.version, None).unwrap_err().code, "SFTPGO_CONFIG_PATH");
        }
        assert_eq!(state.select_sftpgo_config("etc/sftpgo/2.7.5", "stale-version", None).unwrap_err().code, "SFTPGO_CONFIG_CHANGED");
        let removed = state.paths.etc_dir("sftpgo", "2.7.5").join("identity"); std::fs::remove_file(&removed).unwrap();
        assert_eq!(state.select_sftpgo_config("etc/sftpgo/2.7.5", &report.version, None).unwrap_err().code, "SFTPGO_CONFIG_INVALID");
        assert!(state.store.get_setting(SFTPGO_CONFIG_BINDING).is_none());
        std::fs::write(removed, "key-2.7.5").unwrap();
        state.select_sftpgo_config("etc/sftpgo/2.7.5", &report.version, None).unwrap();
        assert_eq!(sftpgo_config_dir(&state.store, &state.paths).unwrap(), state.paths.etc_dir("sftpgo", "2.7.5"));
        assert_eq!(state.select_sftpgo_config("etc/sftpgo/2.7.4", &report.version, None).unwrap_err().code, "SFTPGO_CONFIG_CHANGED");
        register_services(&state.paths, &state.store, &state.manager);
        state.manager.adopt("sftpgo", &[std::process::id()], Some(2022));
        assert_eq!(state.select_sftpgo_config("etc/sftpgo/2.7.4", &report.version, Some("etc/sftpgo/2.7.5")).unwrap_err().code, "SERVICE_BUSY");
        state.manager.services.lock().remove("sftpgo");
        state.select_sftpgo_config("etc/sftpgo/2.7.4", &report.version, Some("etc/sftpgo/2.7.5")).unwrap();
        for version in ["2.7.4", "2.7.5"] {
            let directory = state.paths.etc_dir("sftpgo", version);
            assert_eq!(std::fs::read_to_string(directory.join("accounts.db")).unwrap(), version);
            assert_eq!(std::fs::read_to_string(directory.join("identity")).unwrap(), format!("key-{version}"));
        }
    }

    #[test]
    fn console_targets_use_resolved_ports_and_preserve_sftpgo_web_configuration() {
        for (id, offset, path) in [("mailpit", 0, "/"), ("minio", 1, "/"), ("consul", 0, "/ui"), ("rnacos", 2000, "/rnacos"), ("qdrant", 0, "/dashboard")] {
            let (_temp, state, mut r) = fixture(id); r.port = Some(31000);
            if id == "qdrant" {
                prepare_config(&state.paths, &r).unwrap();
                std::fs::create_dir(r.root.join("static")).unwrap();
                std::fs::write(r.root.join("static/index.html"), "<html></html>").unwrap();
            }
            assert_eq!(generic_web_target(&r, None).unwrap(), format!("http://127.0.0.1:{}{path}", 31000 + offset));
            r.spec.args = vec!["custom".into()];
            assert_eq!(generic_web_target(&r, None).unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        }
        let (_temp, _state, mut r) = fixture("sftpgo"); r.port = Some(31000);
        let config = serde_json::json!({"httpd":{"web_root":"/custom/../中文 path","bindings":[{"address":"::","enable_https":true,"enable_web_admin":true}]}});
        assert_eq!(sftpgo_web_target(&r, &config, &sftpgo_process_env(&r).unwrap()).unwrap(), "https://[::1]:37058/%E4%B8%AD%E6%96%87%20path/web/admin");
        r.spec.env.as_mut().unwrap().insert("SFTPGO_HTTPD__WEB_ROOT".into(), "/environment".into());
        assert_eq!(sftpgo_web_target(&r, &config, &sftpgo_process_env(&r).unwrap()).unwrap(), "https://[::1]:37058/environment/web/admin");
        r.spec.env.as_mut().unwrap().insert("SFTPGO_HTTPD__BINDINGS__0__ENABLE_WEB_ADMIN".into(), "false".into());
        assert!(sftpgo_web_target(&r, &config, &sftpgo_process_env(&r).unwrap()).unwrap_err().hint.unwrap().contains("已关闭"));
        let config = serde_json::json!({"httpd":{"bindings":[{"enable_web_admin":"true","enable_https":"false"}]}});
        r.spec.env.as_mut().unwrap().insert("SFTPGO_HTTPD__BINDINGS__0__ENABLE_WEB_ADMIN".into(), "fAlSe".into());
        assert!(sftpgo_web_target(&r, &config, &sftpgo_process_env(&r).unwrap()).unwrap().starts_with("http://"));
        assert!(local_web_url("remote.example", 8080, false, "/").is_err());
        assert!(local_web_url("127.0.0.1", 0, false, "/").is_err());
    }

    #[test]
    fn console_probe_checks_live_owner_http_response_and_does_not_follow_external_redirects() {
        use std::io::{Read, Write};
        let (_temp, state, _r) = fixture("mailpit");
        register_services(&state.paths, &state.store, &state.manager);
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/console");
        state.manager.set_web_target("mailpit", Ok(url.clone()));
        assert_eq!(state.service_web_url("mailpit").unwrap_err().code, "SERVICE_NOT_RUNNING");
        state.manager.adopt("mailpit", &[std::process::id()], Some(port));
        let worker = listener.try_clone().unwrap();
        let serve = std::thread::spawn(move || {
            let (mut stream, _) = worker.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut input = [0; 4096]; let n = stream.read(&mut input).unwrap();
            assert!(String::from_utf8_lossy(&input[..n]).starts_with("GET /console HTTP/1.1"));
            assert!(!String::from_utf8_lossy(&input[..n]).to_ascii_lowercase().contains("authorization:"));
            stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: https://example.invalid/login\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        assert_eq!(state.service_web_url("mailpit").unwrap(), url);
        serve.join().unwrap();
        state.manager.set_web_target("mailpit", Ok("https://example.invalid/".into()));
        assert_eq!(state.service_web_url("mailpit").unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        state.manager.set_web_target("mailpit", Ok(url));
        drop(listener);
        assert_eq!(state.service_web_url("mailpit").unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        state.manager.set_state("mailpit", crate::model::ServiceState::Stopped);
        assert_eq!(state.manager.web_target("mailpit").unwrap_err().code, "SERVICE_WEB_UNKNOWN");
        state.manager.services.lock().remove("mailpit");
    }

    #[test]
    fn qdrant_console_respects_disabled_custom_tls_and_missing_assets() {
        let (_temp, state, mut r) = fixture("qdrant"); r.port = Some(31000);
        prepare_config(&state.paths, &r).unwrap();
        assert_eq!(qdrant_web_target(&r).unwrap_err().code, "QDRANT_WEB_MISSING");
        let config = r.etc.join("config.yaml");
        let content = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config, content.replace("service:", "service:\n  enable_static_content: false")).unwrap();
        assert_eq!(qdrant_web_target(&r).unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        std::fs::write(&config, content.replace("service:", "service:\n  static_content_dir: custom-ui\n  enable_tls: true")).unwrap();
        assert_eq!(qdrant_web_target(&r).unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        std::fs::create_dir(r.root.join("custom-ui")).unwrap();
        std::fs::write(r.root.join("custom-ui/index.html"), "user dashboard").unwrap();
        assert_eq!(qdrant_web_target(&r).unwrap(), "https://127.0.0.1:31000/dashboard");
        r.spec.env = Some(std::collections::HashMap::from([("QDRANT__SERVICE__ENABLE_STATIC_CONTENT".into(), "false".into())]));
        assert_eq!(qdrant_web_target(&r).unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        assert_eq!(std::fs::read_to_string(r.root.join("custom-ui/index.html")).unwrap(), "user dashboard");
    }

    #[test]
    fn qdrant_snapshots_are_preserved_without_overwriting_and_do_not_reappear_after_deletion() {
        let (_temp, state, r) = fixture("qdrant");
        let legacy = r.root.join("snapshots/collection"); std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("one.snapshot"), "first snapshot").unwrap();
        std::fs::write(r.root.join("snapshots/full.snapshot"), "full snapshot").unwrap();
        let durable = state.paths.data().join("qdrant/snapshots");
        std::fs::create_dir_all(durable.join("collection")).unwrap();
        std::fs::write(durable.join("collection/one.snapshot"), "first snapshot").unwrap();
        std::fs::write(durable.join("keep.snapshot"), "existing snapshot").unwrap();
        preserve_qdrant_snapshots(&state.store, &state.paths, &state.manager, None).unwrap();
        assert!(!r.root.join("snapshots").exists());
        assert_eq!(std::fs::read_to_string(durable.join("full.snapshot")).unwrap(), "full snapshot");
        assert_eq!(std::fs::read_to_string(durable.join("keep.snapshot")).unwrap(), "existing snapshot");
        let backup = std::fs::read_dir(state.paths.backup()).unwrap().flatten().find(|e| e.file_name().to_string_lossy().starts_with("qdrant-snapshots-")).unwrap();
        assert_eq!(std::fs::read_to_string(backup.path().join("snapshots/collection/one.snapshot")).unwrap(), "first snapshot");
        std::fs::remove_file(durable.join("collection/one.snapshot")).unwrap();
        preserve_qdrant_snapshots(&state.store, &state.paths, &state.manager, None).unwrap();
        assert!(!durable.join("collection/one.snapshot").exists());
        std::fs::create_dir_all(&legacy).unwrap(); std::fs::write(legacy.join("conflict.snapshot"), "legacy").unwrap();
        std::fs::write(durable.join("collection/conflict.snapshot"), "current").unwrap();
        assert_eq!(preserve_qdrant_snapshots(&state.store, &state.paths, &state.manager, None).unwrap_err().code, "QDRANT_SNAPSHOT_CONFLICT");
        assert_eq!(std::fs::read_to_string(legacy.join("conflict.snapshot")).unwrap(), "legacy");
        assert_eq!(std::fs::read_to_string(durable.join("collection/conflict.snapshot")).unwrap(), "current");
    }

    #[test]
    fn qdrant_snapshot_paths_follow_configuration_precedence_and_preserve_user_text() {
        let (_temp, state, mut r) = fixture("qdrant");
        prepare_config(&state.paths, &r).unwrap();
        let config = r.etc.join("config.yaml");
        let original = std::fs::read_to_string(&config).unwrap().lines().filter(|line| !line.contains("snapshots_path:"))
            .collect::<Vec<_>>().join("\r\n") + "\r\n# keep this comment\r\n";
        std::fs::write(&config, &original).unwrap();
        let env = qdrant_snapshot_env(&state.store, &state.paths, &state.manager, &r).unwrap();
        assert_eq!(qdrant_path_key(std::path::Path::new(&env[0].1)), qdrant_path_key(&state.paths.data().join("qdrant/snapshots")));
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);
        std::fs::create_dir_all(r.root.join("config")).unwrap();
        std::fs::write(r.root.join("config/config.yaml"), "storage:\n  snapshots_path: base-snapshots\n").unwrap();
        std::fs::write(r.root.join("config/local.json"), r#"{"storage":{"snapshots_path":"custom-snapshots"}}"#).unwrap();
        assert_eq!(qdrant_snapshot_directory(&r.entry, &r.root, &state.paths).unwrap(), r.root.join("custom-snapshots"));
        assert!(qdrant_snapshot_env(&state.store, &state.paths, &state.manager, &r).unwrap().is_empty());
        std::fs::write(&config, original.replace("storage:", "storage:\r\n  snapshots_path: explicit-snapshots")).unwrap();
        assert_eq!(qdrant_snapshot_directory(&r.entry, &r.root, &state.paths).unwrap(), r.root.join("explicit-snapshots"));
        r.entry.run.as_mut().unwrap().env = Some(std::collections::HashMap::from([("QDRANT__STORAGE__SNAPSHOTS_PATH".into(), "{data}/private-snapshots".into())]));
        assert_eq!(qdrant_snapshot_directory(&r.entry, &r.root, &state.paths).unwrap(), state.paths.data().join("qdrant/private-snapshots"));
        if cfg!(windows) {
            r.entry.run.as_mut().unwrap().env = Some(std::collections::HashMap::from([("qdrant__storage__snapshots_path".into(), "{data}/lowercase-snapshots".into())]));
            assert_eq!(qdrant_snapshot_directory(&r.entry, &r.root, &state.paths).unwrap(), state.paths.data().join("qdrant/lowercase-snapshots"));
            r.entry.run.as_mut().unwrap().env.as_mut().unwrap().insert("QDRANT__STORAGE__SNAPSHOTS_PATH".into(), "other".into());
            assert_eq!(qdrant_snapshot_directory(&r.entry, &r.root, &state.paths).unwrap_err().code, "QDRANT_CONFIG_ENV");
        }
    }

    #[test]
    fn qdrant_uninstall_refuses_custom_snapshot_data_in_runtime_and_busy_migration() {
        let (_temp, state, r) = fixture("qdrant");
        prepare_config(&state.paths, &r).unwrap();
        let config = r.etc.join("config.yaml"); let original = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config, original.lines().map(|line| if line.contains("snapshots_path:") { "  snapshots_path: custom-snapshots" } else { line }).collect::<Vec<_>>().join("\n")).unwrap();
        std::fs::create_dir(r.root.join("custom-snapshots")).unwrap();
        std::fs::write(r.root.join("custom-snapshots/keep.snapshot"), "keep").unwrap();
        let key = format!("qdrant@{}", r.entry.version);
        assert_eq!(state.uninstall_package(&key).unwrap_err().code, "QDRANT_SNAPSHOT_IN_RUNTIME");
        assert!(r.bin.exists()); assert!(state.store.find_installed("qdrant", Some(&r.entry.version)).is_some());
        std::fs::write(&config, &original).unwrap();
        std::fs::create_dir(r.root.join("snapshots")).unwrap(); std::fs::write(r.root.join("snapshots/old.snapshot"), "old").unwrap();
        register_services(&state.paths, &state.store, &state.manager);
        state.manager.adopt("qdrant", &[std::process::id()], Some(31000));
        assert_eq!(preserve_qdrant_snapshots(&state.store, &state.paths, &state.manager, None).unwrap_err().code, "SERVICE_BUSY");
        assert!(r.root.join("snapshots/old.snapshot").is_file());
        state.manager.services.lock().remove("qdrant");
    }

    #[test]
    #[ignore = "requires NSB_VERIFY_QDRANT and NSB_VERIFY_QDRANT_ZIP and NSB_VERIFY_QDRANT_UI pointing to official verified assets"]
    fn native_qdrant_repairs_console_and_preserves_real_vectors_across_restart_and_reinstall() {
        let executable = std::env::var_os("NSB_VERIFY_QDRANT").expect("set NSB_VERIFY_QDRANT");
        let archive = std::env::var_os("NSB_VERIFY_QDRANT_ZIP").expect("set NSB_VERIFY_QDRANT_ZIP");
        let ui_archive = std::env::var_os("NSB_VERIFY_QDRANT_UI").expect("set NSB_VERIFY_QDRANT_UI");
        let (_temp, state, r) = fixture("qdrant");
        let stop_during_download = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop_during_download.clone();
        let state = Arc::new_cyclic(|weak: &std::sync::Weak<crate::CoreState>| {
            let weak = weak.clone();
            crate::CoreState { emit: Arc::new(move |event| {
                if matches!(event, crate::Event::DownloadProgress(ref p) if p.state == "downloading")
                    && flag.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    weak.upgrade().unwrap().stop_service("qdrant").unwrap();
                }
            }), ..state }
        });
        std::fs::copy(executable, &r.bin).unwrap();
        let key = format!("qdrant@{}", r.entry.version);
        let package = state.installer.find(&key).unwrap();
        assert_eq!(crate::download::sha256_file(std::path::Path::new(&archive)).unwrap(), package.sha256.unwrap());
        std::fs::copy(archive, state.paths.downloads().join(format!("{key}.pkg"))).unwrap();
        let cache = state.paths.downloads().join(format!("{key}--qdrant-web-ui-0.2.18.pkg"));
        std::fs::copy(ui_archive, &cache).unwrap();
        assert_eq!(crate::download::sha256_file(&cache).unwrap(), "fdce24c04ec1627d2369cb8fe610ee06ad9236f82aad214aa7f294ac37372859");
        let base = (30000..42000).find(|port| [0, 1, 2].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        let occupied_grpc = std::net::TcpListener::bind(("127.0.0.1", base + 1)).unwrap();
        state.store.set_port_override("qdrant", Some(base)).unwrap(); state.store.set_setting("autoFallbackPort", "true").unwrap();
        struct Cleanup(Arc<crate::CoreState>);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = self.0.stop_service("qdrant"); } }
        let _cleanup = Cleanup(state.clone());
        state.start_service("qdrant").unwrap_or_else(|e| panic!("{e:?}\n{:?}", state.manager.tail("qdrant", 20)));
        let first = state.manager.snapshot("qdrant").unwrap(); let port = first.port.unwrap(); assert_ne!(port, base);
        assert!(owned_ports_ready(&state.manager, "qdrant", &[port, port + 1]));
        assert_eq!(state.service_web_url("qdrant").unwrap_err().code, "QDRANT_WEB_MISSING");
        let client = reqwest::blocking::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build().unwrap();
        let endpoint = |port, path: &str| format!("http://127.0.0.1:{port}{path}");
        let response = client.put(endpoint(port, "/collections/niceenv_verify"))
            .json(&serde_json::json!({"vectors":{"size":3,"distance":"Cosine"}})).send().unwrap();
        assert!(response.status().is_success(), "{}", response.text().unwrap());
        let response = client.put(endpoint(port, "/collections/niceenv_verify/points?wait=true"))
            .json(&serde_json::json!({"points":[{"id":42,"vector":[0.2,0.4,0.8],"payload":{"name":"保留向量"}}]})).send().unwrap();
        assert!(response.status().is_success(), "{}", response.text().unwrap());
        let config_path = r.etc.join("config.yaml");
        let original = std::fs::read_to_string(&config_path).unwrap() + "\n# user comment preserved\n";
        std::fs::write(&config_path, &original).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(runtime.block_on(state.repair_service_web_ui("qdrant", "wrong-version")).unwrap_err().code, "SERVICE_CHANGED");
        stop_during_download.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(runtime.block_on(state.repair_service_web_ui("qdrant", &r.entry.version)).unwrap_err().code, "SERVICE_CHANGED");
        let stopped = state.manager.snapshot("qdrant").unwrap();
        assert_eq!(stopped.state, crate::model::ServiceState::Stopped); assert!(stopped.pids.is_empty());
        assert!(r.root.join("static/index.html").is_file());
        std::fs::rename(r.root.join("static"), _temp.path().join("verified-dashboard")).unwrap();
        state.start_service("qdrant").unwrap();
        let url = runtime.block_on(state.repair_service_web_ui("qdrant", &r.entry.version)).unwrap();
        let after = state.manager.snapshot("qdrant").unwrap(); assert_ne!(after.pids, first.pids);
        assert_eq!(url, endpoint(after.port.unwrap(), "/dashboard"));
        let html = client.get(&url).send().unwrap().error_for_status().unwrap().text().unwrap();
        assert!(html.contains("Qdrant Web UI"));
        let script = regex::Regex::new(r#"src="(/dashboard/assets/[^" ]+\.js)""#).unwrap();
        let script = script.captures(&html).unwrap().get(1).unwrap().as_str();
        assert!(client.get(endpoint(after.port.unwrap(), script)).send().unwrap().error_for_status().unwrap().bytes().unwrap().len() > 1000);
        let read_vector = |port| {
            let value: serde_json::Value = client.get(endpoint(port, "/collections/niceenv_verify/points/42"))
                .send().unwrap().error_for_status().unwrap().json().unwrap();
            assert_eq!(value["result"]["payload"]["name"], "保留向量");
        };
        read_vector(after.port.unwrap());
        assert!(std::fs::read_to_string(&config_path).unwrap().contains("# user comment preserved"));
        state.stop_service("qdrant").unwrap();
        state.uninstall_package(&key).unwrap();
        runtime.block_on(state.install_package(&key)).unwrap();
        let new_binary = state.paths.runtime_dir("qdrant", &r.entry.version);
        assert!(new_binary.join("static/index.html").is_file());
        assert!(new_binary.join("static/openapi.json").is_file());
        state.start_service("qdrant").unwrap();
        let last = state.manager.snapshot("qdrant").unwrap();
        read_vector(last.port.unwrap());
        assert!(state.service_web_url("qdrant").unwrap().ends_with("/dashboard"));
        state.stop_service("qdrant").unwrap();
        assert!(last.pids.iter().all(|pid| !platform::process_alive(*pid)));
        assert_eq!(state.service_web_url("qdrant").unwrap_err().code, "SERVICE_NOT_RUNNING");
        drop(occupied_grpc);
    }

    #[test]
    #[ignore = "requires verified NSB_VERIFY_QDRANT_OLD (1.19.0), NSB_VERIFY_QDRANT_ZIP (1.19.1), NSB_VERIFY_QDRANT_UI"]
    fn native_qdrant_snapshots_survive_old_version_uninstall_and_restore_on_new_version() {
        verify_native_qdrant_snapshot_upgrade(true);
    }

    #[test]
    #[ignore = "requires verified NSB_VERIFY_QDRANT_OLD (1.19.0), NSB_VERIFY_QDRANT_ZIP (1.19.1), NSB_VERIFY_QDRANT_UI"]
    fn native_qdrant_snapshots_survive_version_switch_and_inactive_uninstall() {
        verify_native_qdrant_snapshot_upgrade(false);
    }

    fn verify_native_qdrant_snapshot_upgrade(uninstall_first: bool) {
        use sha2::Digest;
        let old_program = std::env::var_os("NSB_VERIFY_QDRANT_OLD").expect("set NSB_VERIFY_QDRANT_OLD");
        let new_zip = std::env::var_os("NSB_VERIFY_QDRANT_ZIP").expect("set NSB_VERIFY_QDRANT_ZIP");
        let ui_zip = std::env::var_os("NSB_VERIFY_QDRANT_UI").expect("set NSB_VERIFY_QDRANT_UI");
        let (_temp, state, mut r) = fixture_version("qdrant", Some("v1.19.0"));
        std::fs::copy(old_program, &r.bin).unwrap();
        r.port = Some((30000..42000).find(|port| [0, 1].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap());
        let port = r.port.unwrap();
        prepare_config(&state.paths, &r).unwrap();
        // 复现旧发布的实际行为：主程序从 runtime 启动，配置未声明快照路径。
        let config_file = r.etc.join("config.yaml");
        let legacy_config = std::fs::read_to_string(&config_file).unwrap().lines().filter(|line| !line.contains("snapshots_path:"))
            .collect::<Vec<_>>().join("\r\n") + "\r\n# legacy user configuration\r\n";
        std::fs::write(&config_file, &legacy_config).unwrap();
        register_services(&state.paths, &state.store, &state.manager);
        state.manager.set_state("qdrant", crate::model::ServiceState::Starting);
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("qdrant"); } }
        let _cleanup = Cleanup(&state);
        spawn_tracked(&state.manager, "qdrant", &SpawnSpec { program: r.bin.clone(),
            args: vec!["--config-path".into(), config_file.to_string_lossy().into_owned(), "--disable-telemetry".into()],
            cwd: Some(r.root.clone()), env: vec![], detached: None }).unwrap();
        assert!(wait_owned_ports(&state.manager, "qdrant", &[port, port + 1], Duration::from_secs(20)), "{:?}", state.manager.tail("qdrant", 20));
        state.manager.set_started_port("qdrant", port); state.manager.set_state("qdrant", crate::model::ServiceState::Running);
        let client = reqwest::blocking::Client::builder().no_proxy().timeout(Duration::from_secs(15)).build().unwrap();
        let url = |port, path: &str| format!("http://127.0.0.1:{port}{path}");
        client.put(url(port, "/collections/snapshot_source")).json(&serde_json::json!({"vectors":{"size":3,"distance":"Cosine"},
            "optimizers_config":{"default_segment_number":1},"wal_config":{"wal_capacity_mb":1}}))
            .send().unwrap().error_for_status().unwrap();
        client.put(url(port, "/collections/snapshot_source/points?wait=true")).json(&serde_json::json!({"points":[{"id":7,"vector":[0.3,0.5,0.9],"payload":{"value":"跨版本快照"}}]}))
            .send().unwrap().error_for_status().unwrap();
        let snapshot: serde_json::Value = client.post(url(port, "/collections/snapshot_source/snapshots")).send().unwrap().error_for_status().unwrap().json().unwrap();
        let name = snapshot["result"]["name"].as_str().unwrap();
        let bytes = client.get(url(port, &format!("/collections/snapshot_source/snapshots/{name}"))).send().unwrap().error_for_status().unwrap().bytes().unwrap();
        let digest = hex::encode(sha2::Sha256::digest(&bytes));
        assert_eq!(crate::download::sha256_file(&r.root.join("snapshots/snapshot_source").join(name)).unwrap(), digest);
        let full: serde_json::Value = client.post(url(port, "/snapshots")).send().unwrap().error_for_status().unwrap().json().unwrap();
        let full_name = full["result"]["name"].as_str().unwrap();
        let full_digest = crate::download::sha256_file(&r.root.join("snapshots").join(full_name)).unwrap();
        let old_pids = state.manager.snapshot("qdrant").unwrap().pids;
        if uninstall_first { state.uninstall_package("qdrant@v1.19.0").unwrap(); }
        else { state.stop_service("qdrant").unwrap(); }
        assert!(old_pids.iter().all(|pid| !platform::process_alive(*pid)));
        assert_eq!(r.root.exists(), !uninstall_first);
        assert_eq!(std::fs::read_to_string(&config_file).unwrap(), legacy_config);
        let durable = state.paths.data().join("qdrant/snapshots");
        let key = "qdrant@v1.19.1";
        std::fs::copy(new_zip, state.paths.downloads().join(format!("{key}.pkg"))).unwrap();
        std::fs::copy(ui_zip, state.paths.downloads().join(format!("{key}--qdrant-web-ui-0.2.18.pkg"))).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap(); runtime.block_on(state.install_package(key)).unwrap();
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        state.set_active_version("qdrant", "v1.19.1").unwrap();
        state.start_service("qdrant").unwrap(); let port = state.manager.snapshot("qdrant").unwrap().port.unwrap();
        assert_eq!(crate::download::sha256_file(&durable.join("snapshot_source").join(name)).unwrap(), digest);
        assert_eq!(crate::download::sha256_file(&durable.join(full_name)).unwrap(), full_digest);
        if !uninstall_first {
            assert!(!r.root.join("snapshots").exists());
            let current_pids = state.manager.snapshot("qdrant").unwrap().pids;
            state.uninstall_package("qdrant@v1.19.0").unwrap();
            assert_eq!(state.manager.snapshot("qdrant").unwrap().pids, current_pids);
        }
        let list: serde_json::Value = client.get(url(port, "/collections/snapshot_source/snapshots")).send().unwrap().error_for_status().unwrap().json().unwrap();
        assert!(list["result"].as_array().unwrap().iter().any(|item| item["name"] == name));
        let bytes = client.get(url(port, &format!("/collections/snapshot_source/snapshots/{name}"))).send().unwrap().error_for_status().unwrap().bytes().unwrap();
        assert_eq!(hex::encode(sha2::Sha256::digest(&bytes)), digest);
        let full_bytes = client.get(url(port, &format!("/snapshots/{full_name}"))).send().unwrap().error_for_status().unwrap().bytes().unwrap();
        assert_eq!(hex::encode(sha2::Sha256::digest(&full_bytes)), full_digest);
        let location = reqwest::Url::from_file_path(durable.join("snapshot_source").join(name)).unwrap();
        let restored = client.put(url(port, "/collections/restored_from_snapshot/snapshots/recover"))
            .json(&serde_json::json!({"location":location.as_str(),"priority":"snapshot"})).send().unwrap();
        assert!(restored.status().is_success(), "{}", restored.text().unwrap());
        let point: serde_json::Value = client.get(url(port, "/collections/restored_from_snapshot/points/7"))
            .send().unwrap().error_for_status().unwrap().json().unwrap();
        assert_eq!(point["result"]["payload"]["value"], "跨版本快照");
        let fresh: serde_json::Value = client.post(url(port, "/collections/restored_from_snapshot/snapshots"))
            .send().unwrap().error_for_status().unwrap().json().unwrap();
        let fresh_name = fresh["result"]["name"].as_str().unwrap();
        assert!(durable.join("restored_from_snapshot").join(fresh_name).is_file());
        client.delete(url(port, &format!("/collections/snapshot_source/snapshots/{name}"))).send().unwrap().error_for_status().unwrap();
        state.restart_service("qdrant").unwrap();
        assert!(!durable.join("snapshot_source").join(name).exists());
        state.uninstall_package(key).unwrap();
        assert!(durable.join("restored_from_snapshot").join(fresh_name).is_file());
        assert!(durable.join(full_name).is_file());
    }

    #[test]
    fn console_probe_rejects_an_api_response_and_keeps_the_service_running() {
        use std::io::{Read, Write};
        let (_temp, state, _r) = fixture("qdrant");
        register_services(&state.paths, &state.store, &state.manager);
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        state.manager.adopt("qdrant", &[std::process::id()], Some(port));
        state.manager.set_web_target("qdrant", Ok(format!("http://127.0.0.1:{port}/dashboard")));
        let worker = listener.try_clone().unwrap();
        let serve = std::thread::spawn(move || {
            let (mut stream, _) = worker.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut input = [0; 4096]; let _ = stream.read(&mut input).unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
            let (stream, _) = worker.accept().unwrap(); drop(stream); // HTTPS 探测也不能把 API 误报成网页。
        });
        assert_eq!(state.service_web_url("qdrant").unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        serve.join().unwrap();
        assert_eq!(state.manager.snapshot("qdrant").unwrap().state, crate::model::ServiceState::Running);
        state.manager.services.lock().remove("qdrant");
    }

    #[test]
    fn sftpgo_preserves_provider_custom_resources_and_rejects_bad_configuration() {
        let (_temp, state, r) = fixture("sftpgo");
        let content = "{\"data_provider\":{\"driver\":\"bolt\",\"name\":\"kept.db\"},\"httpd\":{\"templates_path\":\"custom-templates\"}}\n";
        std::fs::write(r.root.join("sftpgo.json"), content).unwrap();
        let config = prepare_sftpgo(&state.store, &state.paths, &r).unwrap();
        assert_eq!(std::fs::read_to_string(&config.file).unwrap(), content);
        assert!(config.env.iter().all(|(key, _)| key != "SFTPGO_HTTPD__TEMPLATES_PATH"));
        assert!(config.env.iter().all(|(key, _)| !key.starts_with("SFTPGO_DATA_PROVIDER")));
        assert!(config.env.iter().any(|(key, value)| key == "SFTPGO_HTTPD__STATIC_FILES_PATH" && value == &r.root.join("static").to_string_lossy()));
        std::fs::write(r.root.join("sftpgo.json"), "{}").unwrap();
        assert_eq!(std::fs::read_to_string(prepare_sftpgo(&state.store, &state.paths, &r).unwrap().file).unwrap(), content);
        std::fs::write(&config.file, "{broken").unwrap();
        assert_eq!(prepare_sftpgo(&state.store, &state.paths, &r).err().unwrap().code, "SFTPGO_CONFIG_INVALID");
        assert_eq!(std::fs::read_to_string(&config.file).unwrap(), "{broken");
        assert!(state.store.get_setting(SFTPGO_CONFIG_BINDING).is_none());
        assert!(state.store.get_port_assign("sftpgo").is_none());
        std::fs::write(&config.file, content).unwrap();
        let env_dir = r.etc.join("env.d"); std::fs::create_dir(&env_dir).unwrap();
        std::fs::write(env_dir.join("resources.env"), "export SFTPGO_HTTPD__STATIC_FILES_PATH='custom-assets'\n").unwrap();
        assert!(prepare_sftpgo(&state.store, &state.paths, &r).unwrap().env.iter().all(|(key, _)| key != "SFTPGO_HTTPD__STATIC_FILES_PATH"));
        state.store.set_setting(SFTPGO_CONFIG_BINDING, "etc/sftpgo/shared").unwrap();
        assert_eq!(prepare_sftpgo(&state.store, &state.paths, &r).err().unwrap().code, "SFTPGO_STATE_MISSING");
        assert!(!r.etc.join("kept.db").exists());
        std::fs::write(r.etc.join("kept.db"), b"old database").unwrap();
        assert_eq!(prepare_sftpgo(&state.store, &state.paths, &r).err().unwrap().code, "SFTPGO_STATE_MISSING");
        for key in ["id_rsa", "id_ecdsa", "id_ed25519"] { std::fs::write(r.etc.join(key), b"kept host key").unwrap(); }
        assert!(prepare_sftpgo(&state.store, &state.paths, &r).is_ok());
    }

    #[test]
    fn sftpgo_env_matches_gotenv_precedence_quotes_interpolation_and_encoding() {
        use std::collections::HashMap;
        let (_temp, state, mut r) = fixture("sftpgo"); r.port = Some(31000);
        let env_dir = r.etc.join("env.d"); std::fs::create_dir(&env_dir).unwrap();
        let first = concat!("\u{feff}# keep comment\r", "ROOT=/first\rROOT='/console'\r",
            "export SFTPGO_HTTPD__WEB_ROOT=\"${ROOT}/$UPSTREAM\" # comment\r",
            "SFTPGO_HTTPD__BINDINGS__0__ADDRESS: 127.0.0.1\r",
            "EMPTY=from-file\rFROM_EMPTY=${EMPTY}/suffix\r",
            "LITERAL='$ROOT # literal'\rESCAPED=\"\\$ROOT\"\r",
            "MULTILINE=\"first\rsecond\"\rDUPLICATE=one\rDUPLICATE=two\r");
        // 故意逆序创建文件，不能依赖文件系统的枚举顺序。
        let second = "ROOT=/wrong\nSFTPGO_HTTPD__WEB_ROOT=/wrong\nAFTER=${ROOT}/after\nSFTPGO_HTTPD__BINDINGS__0__ENABLE_HTTPS=false\n";
        let mut encoded = vec![0xff, 0xfe]; for word in second.encode_utf16() { encoded.extend(word.to_le_bytes()); }
        std::fs::write(env_dir.join("20-last.env"), encoded).unwrap();
        std::fs::write(env_dir.join("10-first.env"), first).unwrap();
        let files = sftpgo_env_files(&state.paths, &r.etc).unwrap();
        let inherited = HashMap::from([("UPSTREAM".into(), "inherited".into()), ("EMPTY".into(), String::new())]);
        let env = sftpgo_parse_env(&files, inherited.clone()).unwrap();
        assert_eq!(env["SFTPGO_HTTPD__WEB_ROOT"], "/console/inherited");
        assert_eq!(env["AFTER"], "/console/after"); assert_eq!(env["EMPTY"], "");
        assert_eq!(env["FROM_EMPTY"], "/suffix"); assert_eq!(env["LITERAL"], "$ROOT # literal");
        assert_eq!(env["ESCAPED"], "$ROOT"); assert_eq!(env["MULTILINE"], "first\nsecond");
        assert_eq!(env["DUPLICATE"], "two");
        assert_eq!(sftpgo_web_target(&r, &serde_json::json!({}), &env).unwrap(), "http://127.0.0.1:37058/console/inherited/web/admin");
        let mut big_endian = vec![0xfe, 0xff]; for word in "UNICODE='中文'\n".encode_utf16() { big_endian.extend(word.to_be_bytes()); }
        std::fs::write(env_dir.join("30-unicode.env"), big_endian).unwrap();
        assert_eq!(sftpgo_parse_env(&sftpgo_env_files(&state.paths, &r.etc).unwrap(), inherited).unwrap()["UNICODE"], "中文");
        let marker = format!("NICEENV_ENV_FIXTURE_{}", rand::random::<u64>());
        sftpgo_parse_env(&[(PathBuf::from("fixture.env"), format!("{marker}=private"))], HashMap::new()).unwrap();
        assert!(std::env::var_os(marker).is_none());
        if cfg!(windows) {
            r.spec.env.as_mut().unwrap().insert("sftpgo_httpd__web_root".into(), "/lowercase".into());
            assert_eq!(sftpgo_parse_env(&files, sftpgo_process_env(&r).unwrap()).unwrap()["SFTPGO_HTTPD__WEB_ROOT"], "/lowercase");
            r.spec.env.as_mut().unwrap().insert("SFTPGO_HTTPD__WEB_ROOT".into(), "/duplicate".into());
            assert_eq!(sftpgo_process_env(&r).unwrap_err().code, "SFTPGO_ENV_AMBIGUOUS");
            assert_eq!(sftpgo_parse_env(&[(PathBuf::from("fixture.env"), "KEY=one\nkey=two".into())], HashMap::new()).unwrap_err().code, "SFTPGO_ENV_AMBIGUOUS");
        }
    }

    #[test]
    fn sftpgo_env_state_guards_and_invalid_files_preserve_configuration() {
        let (_temp, state, r) = fixture("sftpgo");
        std::fs::write(r.root.join("sftpgo.json"), "{}").unwrap();
        let env_dir = r.etc.join("env.d"); std::fs::create_dir(&env_dir).unwrap();
        let env_file = env_dir.join("state.env");
        let content = "DB_NAME=custom.db\nSFTPGO_DATA_PROVIDER__NAME=${DB_NAME}\nSFTPGO_SFTPD__HOST_KEYS=custom-key\nSFTPGO_HTTPD__WEB_ROOT=/from-env\nRESOURCE=${SFTPGO_HTTPD__STATIC_FILES_PATH}\n";
        std::fs::write(&env_file, content).unwrap();
        let prepared = prepare_sftpgo(&state.store, &state.paths, &r).unwrap();
        assert!(prepared.web_target.unwrap().ends_with("/from-env/web/admin"));
        state.store.set_setting(SFTPGO_CONFIG_BINDING, "etc/sftpgo/shared").unwrap();
        assert_eq!(prepare_sftpgo(&state.store, &state.paths, &r).err().unwrap().code, "SFTPGO_STATE_MISSING");
        std::fs::write(r.etc.join("custom.db"), "existing database").unwrap();
        assert_eq!(prepare_sftpgo(&state.store, &state.paths, &r).err().unwrap().code, "SFTPGO_STATE_MISSING");
        std::fs::write(r.etc.join("custom-key"), "existing key").unwrap();
        assert!(prepare_sftpgo(&state.store, &state.paths, &r).is_ok());
        for bad in ["SECRET='private-unclosed", "SECRET=private\nnot an assignment", "SECRET=private\0value"] {
            std::fs::write(&env_file, bad).unwrap();
            let error = prepare_sftpgo(&state.store, &state.paths, &r).err().unwrap();
            assert_eq!(error.code, "SFTPGO_ENV_INVALID"); assert!(!format!("{error:?}").contains("private"));
            assert_eq!(std::fs::read_to_string(&env_file).unwrap(), bad);
            assert_eq!(std::fs::read_to_string(&prepared.file).unwrap(), "{}");
            assert!(state.store.get_port_assign("sftpgo").is_none());
        }
    }

    #[test]
    #[ignore = "requires NSB_VERIFY_SFTPGO_OLD and NSB_VERIFY_SFTPGO_NEW pointing to official portable 2.7.5/2.7.6 directories"]
    fn native_sftpgo_transfers_files_and_keeps_accounts_keys_and_config_across_versions() {
        verify_native_sftpgo_upgrade(false);
    }

    #[test]
    #[ignore = "requires NSB_VERIFY_SFTPGO_OLD and NSB_VERIFY_SFTPGO_NEW pointing to official portable 2.7.5/2.7.6 directories"]
    fn native_sftpgo_env_directory_keeps_console_state_and_identity_across_versions() {
        verify_native_sftpgo_upgrade(true);
    }

    fn verify_native_sftpgo_upgrade(with_env_directory: bool) {
        let old_source = PathBuf::from(std::env::var_os("NSB_VERIFY_SFTPGO_OLD").expect("set NSB_VERIFY_SFTPGO_OLD"));
        let new_source = PathBuf::from(std::env::var_os("NSB_VERIFY_SFTPGO_NEW").expect("set NSB_VERIFY_SFTPGO_NEW"));
        let (_temp, state, _initial) = fixture_version("sftpgo", Some("2.7.5"));
        let password = format!("Native-fixture-{}!", rand::random::<u64>());
        fn copy_tree(source: &std::path::Path, destination: &std::path::Path) {
            std::fs::create_dir_all(destination).unwrap();
            for entry in std::fs::read_dir(source).unwrap() {
                let entry = entry.unwrap(); let target = destination.join(entry.file_name());
                assert!(!entry.file_type().unwrap().is_symlink());
                if entry.file_type().unwrap().is_dir() { copy_tree(&entry.path(), &target); }
                else { std::fs::copy(entry.path(), target).unwrap(); }
            }
        }
        let install = |version: &str, source: &std::path::Path| {
            let mut entry = state.installer.find(&format!("sftpgo@{version}")).unwrap();
            let root = state.paths.runtime_dir("sftpgo", version); std::fs::create_dir_all(&root).unwrap();
            for file in ["sftpgo.exe", "sftpgo.json"] { std::fs::copy(source.join(file), root.join(file)).unwrap(); }
            for dir in ["templates", "static", "openapi"] { copy_tree(&source.join(dir), &root.join(dir)); }
            let env = entry.run.as_mut().unwrap().env.as_mut().unwrap();
            env.insert("SFTPGO_DATA_PROVIDER__CREATE_DEFAULT_ADMIN".into(), "true".into());
            env.insert("SFTPGO_DEFAULT_ADMIN_USERNAME".into(), "fixture".into());
            env.insert("SFTPGO_DEFAULT_ADMIN_PASSWORD".into(), password.clone());
            std::fs::write(root.join(".niceenv-package.json"), serde_json::to_vec(&entry).unwrap()).unwrap();
            state.store.upsert_installed(&InstalledPackage { id: "sftpgo".into(), version: version.into(), category: "ftp".into(),
                install_path: root.to_string_lossy().into_owned(), config_path: String::new(), installed_at: 0 }).unwrap();
        };
        install("2.7.5", &old_source);
        let legacy_dir = state.paths.etc_dir("sftpgo", "2.7.5"); std::fs::create_dir_all(&legacy_dir).unwrap();
        let mut config: serde_json::Value = serde_json::from_slice(&std::fs::read(old_source.join("sftpgo.json")).unwrap()).unwrap();
        config["sftpd"]["bindings"][0]["address"] = "127.0.0.1".into();
        config["httpd"]["bindings"][0]["address"] = "127.0.0.1".into();
        config["common"]["idle_timeout"] = 17.into();
        config["httpd"]["web_root"] = "/niceenv-console".into();
        let original_config = serde_json::to_vec_pretty(&config).unwrap();
        let config_path = legacy_dir.join("sftpgo.json"); std::fs::write(&config_path, &original_config).unwrap();
        let console_path = if with_env_directory { "/env-console/last" } else { "/niceenv-console" };
        let database_name = if with_env_directory { "env-provider.db" } else { "sftpgo.db" };
        if with_env_directory {
            let directory = legacy_dir.join("env.d"); std::fs::create_dir(&directory).unwrap();
            let first = concat!("\u{feff}ROOT='/env-console'\r\nSFTPGO_HTTPD__WEB_ROOT=\"${ROOT}/first\"\r\n",
                "SFTPGO_HTTPD__WEB_ROOT=\"${ROOT}/last\"\r\nSFTPGO_HTTPD__BINDINGS__0__ADDRESS: 127.0.0.1\r\n",
                "SFTPGO_HTTPD__BINDINGS__0__PORT=1\r\nDB_NAME='env-provider.db'\r\nSFTPGO_DATA_PROVIDER__NAME=${DB_NAME}\r\n",
                "SFTPGO_DEFAULT_ADMIN_USERNAME=wrong-user\r\nUNUSED='first\r\nsecond'\r\n");
            let second = "SFTPGO_HTTPD__WEB_ROOT=/ignored-later\nSFTPGO_HTTPD__BINDINGS__0__ENABLE_HTTPS=false\n";
            let mut encoded = vec![0xff, 0xfe]; for word in second.encode_utf16() { encoded.extend(word.to_le_bytes()); }
            std::fs::write(directory.join("20-last.env"), encoded).unwrap();
            std::fs::write(directory.join("10-first.env"), first).unwrap();
        }
        let base = (22000..42000).find(|port| [0, 1, 6058, 6059].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        let occupied_web = std::net::TcpListener::bind(("127.0.0.1", base + 6058)).unwrap();
        state.store.set_port_override("sftpgo", Some(base)).unwrap();
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("sftpgo"); } }
        let _cleanup = Cleanup(&state);
        state.start_service("sftpgo").unwrap_or_else(|e| panic!("{e:?}\n{:?}", state.manager.tail("sftpgo", 20)));
        let first = state.manager.snapshot("sftpgo").unwrap().port.unwrap(); assert_ne!(first, base);
        let client = reqwest::blocking::Client::builder().no_proxy().pool_max_idle_per_host(0).timeout(Duration::from_secs(5)).build().unwrap();
        let token = |port: u16| {
            let response = client.get(format!("http://127.0.0.1:{}/api/v2/token", port + 6058)).basic_auth("fixture", Some(&password)).send().unwrap();
            assert!(response.status().is_success(), "token status: {}", response.status());
            response.json::<serde_json::Value>().unwrap()["access_token"].as_str().unwrap().to_string()
        };
        let access = token(first);
        let user_home = state.paths.data().join("sftpgo/user-files");
        let response = client.post(format!("http://127.0.0.1:{}/api/v2/users", first + 6058)).bearer_auth(&access)
            .json(&serde_json::json!({"username":"native-user","password":password,"status":1,"home_dir":user_home.to_string_lossy(),"permissions":{"/":["*"]}})).send().unwrap();
        assert!(response.status().is_success(), "create user status: {}", response.status());
        let web_url = state.service_web_url("sftpgo").unwrap();
        assert_eq!(web_url, format!("http://127.0.0.1:{}{console_path}/web/admin", first + 6058));
        let admin = client.get(&web_url).send().unwrap();
        assert!(admin.status().is_success()); assert!(admin.text().unwrap().to_ascii_lowercase().contains("<html"));
        // 修改文件中的待生效路径和计划端口，不得改变当前进程的入口。
        config["httpd"]["web_root"] = "/next-start".into();
        std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        state.store.set_port_override("sftpgo", Some(first + 10)).unwrap();
        assert_eq!(state.service_web_url("sftpgo").unwrap(), web_url);
        std::fs::write(&config_path, &original_config).unwrap();
        let fingerprint = crate::certdeploy::probe_ssh("127.0.0.1", first).unwrap().fingerprint;
        struct VerifyHost(String);
        impl russh::client::Handler for VerifyHost {
            type Error = russh::Error;
            async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> std::result::Result<bool, Self::Error> {
                Ok(key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string() == self.0)
            }
        }
        let transfer = |port: u16, write: bool| {
            tokio::runtime::Runtime::new().unwrap().block_on(async {
                tokio::time::timeout(Duration::from_secs(10), async {
                    let mut handle = russh::client::connect(Arc::new(russh::client::Config::default()), ("127.0.0.1", port), VerifyHost(fingerprint.clone())).await.unwrap();
                    assert!(handle.authenticate_password("native-user", &password).await.unwrap().success());
                    let channel = handle.channel_open_session().await.unwrap(); channel.request_subsystem(true, "sftp").await.unwrap();
                    let sftp = russh_sftp::client::SftpSession::new(channel.into_stream()).await.unwrap(); sftp.set_timeout(5);
                    if write {
                        use tokio::io::AsyncWriteExt;
                        let mut file = sftp.create("/native.txt").await.unwrap();
                        file.write_all(b"persistent SFTP content").await.unwrap(); file.close().await.unwrap();
                    }
                    assert_eq!(sftp.read("/native.txt").await.unwrap(), b"persistent SFTP content");
                    sftp.close().await.unwrap(); handle.disconnect(russh::Disconnect::ByApplication, "done", "en").await.unwrap();
                }).await.unwrap();
            });
        };
        transfer(first, true);
        assert_eq!(std::fs::read(user_home.join("native.txt")).unwrap(), b"persistent SFTP content");
        state.stop_service("sftpgo").unwrap();
        assert_eq!(state.service_web_url("sftpgo").unwrap_err().code, "SERVICE_NOT_RUNNING");
        if with_env_directory {
            let key_directory = legacy_dir.join("ssh-keys"); std::fs::create_dir(&key_directory).unwrap();
            for key in ["id_rsa", "id_ecdsa", "id_ed25519"] {
                std::fs::rename(legacy_dir.join(key), key_directory.join(key)).unwrap();
            }
            std::fs::write(legacy_dir.join("env.d/30-keys.env"),
                "SFTPGO_SFTPD__HOST_KEYS='ssh-keys/id_rsa,ssh-keys/id_ecdsa,ssh-keys/id_ed25519'\n").unwrap();
            let alternate = state.paths.etc_dir("sftpgo", "archive"); copy_tree(&legacy_dir, &alternate);
            let choices = state.sftpgo_config_directories().unwrap();
            assert_eq!(choices.directories.len(), 2); assert!(choices.directories.iter().all(|row| row.issue.is_none()));
            state.select_sftpgo_config("etc/sftpgo/archive", "2.7.5", Some("etc/sftpgo/2.7.5")).unwrap();
            state.start_service("sftpgo").unwrap();
            let alternate_port = state.manager.snapshot("sftpgo").unwrap().port.unwrap();
            let response = client.post(format!("http://127.0.0.1:{}/api/v2/users", alternate_port + 6058)).bearer_auth(token(alternate_port))
                .json(&serde_json::json!({"username":"alternate-only","password":password,"status":1,
                    "home_dir":state.paths.data().join("sftpgo/alternate-files").to_string_lossy(),"permissions":{"/":["*"]}})).send().unwrap();
            assert!(response.status().is_success());
            assert_eq!(state.select_sftpgo_config("etc/sftpgo/2.7.5", "2.7.5", Some("etc/sftpgo/archive")).unwrap_err().code, "SERVICE_BUSY");
            state.stop_service("sftpgo").unwrap();
            state.select_sftpgo_config("etc/sftpgo/2.7.5", "2.7.5", Some("etc/sftpgo/archive")).unwrap();
            assert!(alternate.join(database_name).is_file());
        }
        install("2.7.6", &new_source); state.set_active_version("sftpgo", "2.7.6").unwrap();
        let requested = (base + 20..42000).find(|port| [0, 6058].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        let occupied_sftp = std::net::TcpListener::bind(("127.0.0.1", requested)).unwrap();
        state.store.set_port_override("sftpgo", Some(requested)).unwrap();
        state.start_service("sftpgo").unwrap_or_else(|e| panic!("{e:?}\n{:?}", state.manager.tail("sftpgo", 20)));
        let second = state.manager.snapshot("sftpgo").unwrap().port.unwrap(); assert_ne!(second, requested);
        assert_eq!(state.service_web_url("sftpgo").unwrap(), format!("http://127.0.0.1:{}{console_path}/web/admin", second + 6058));
        assert_eq!(state.manager.snapshot("sftpgo").unwrap().version.as_deref(), Some("2.7.6"));
        assert_eq!(crate::certdeploy::probe_ssh("127.0.0.1", second).unwrap().fingerprint, fingerprint);
        let response = client.get(format!("http://127.0.0.1:{}/api/v2/users/native-user", second + 6058)).bearer_auth(token(second)).send().unwrap();
        assert!(response.status().is_success()); transfer(second, false);
        if with_env_directory {
            let response = client.get(format!("http://127.0.0.1:{}/api/v2/users/alternate-only", second + 6058)).bearer_auth(token(second)).send().unwrap();
            assert_eq!(response.status().as_u16(), 404);
        }
        assert_eq!(std::fs::read(&config_path).unwrap(), original_config);
        assert_eq!(state.store.get_setting(SFTPGO_CONFIG_BINDING).as_deref(), Some("etc/sftpgo/2.7.5"));
        assert!(!state.paths.etc_dir("sftpgo", "2.7.6").join("sftpgo.db").exists());
        state.stop_service("sftpgo").unwrap();
        let identity_name = if with_env_directory { "ssh-keys/id_ed25519" } else { "id_ed25519" };
        for name in [database_name, identity_name] {
            let original = legacy_dir.join(name); let backup = legacy_dir.join(format!("{name}.preserved"));
            std::fs::rename(&original, &backup).unwrap();
            assert_eq!(state.start_service("sftpgo").unwrap_err().code, "SFTPGO_STATE_MISSING");
            assert!(!original.exists()); assert!(state.manager.snapshot("sftpgo").unwrap().pids.is_empty());
            std::fs::rename(backup, original).unwrap();
        }
        std::fs::write(&config_path, "{broken").unwrap();
        assert_eq!(state.start_service("sftpgo").unwrap_err().code, "SFTPGO_CONFIG_INVALID");
        assert!(state.manager.snapshot("sftpgo").unwrap().pids.is_empty());
        if with_env_directory {
            std::fs::write(&config_path, &original_config).unwrap();
            let env_file = legacy_dir.join("env.d/10-first.env");
            let original_env = std::fs::read(&env_file).unwrap();
            std::fs::write(&env_file, "SFTPGO_HTTPD__WEB_ROOT='private-unclosed").unwrap();
            assert_eq!(state.start_service("sftpgo").unwrap_err().code, "SFTPGO_ENV_INVALID");
            assert!(state.manager.snapshot("sftpgo").unwrap().pids.is_empty());
            std::fs::write(env_file, original_env).unwrap();
        }
        drop(occupied_web); drop(occupied_sftp);
    }

    #[test]
    #[ignore = "requires NSB_VERIFY_MAILPIT pointing to the official Windows Mailpit executable"]
    fn native_mailpit_console_uses_fallback_port_and_opens_real_mail_interface() {
        let executable = std::env::var_os("NSB_VERIFY_MAILPIT").expect("set NSB_VERIFY_MAILPIT");
        let (_temp, state, mut r) = fixture("mailpit");
        std::fs::copy(executable, &r.bin).unwrap();
        r.entry.run.as_mut().unwrap().args.extend(["--webroot".into(), "/niceenv-mail/".into()]);
        r.spec = r.entry.run.clone().unwrap();
        std::fs::write(PathBuf::from(&r.inst.install_path).join(".niceenv-package.json"), serde_json::to_vec(&r.entry).unwrap()).unwrap();
        let base = (29000..42000).find(|port| [0, 1, -7000, -6999].iter().all(|offset| tcp_port_bindable((*port as i32 + offset) as u16))).unwrap();
        let occupied = std::net::TcpListener::bind(("127.0.0.1", base)).unwrap();
        state.store.set_port_override("mailpit", Some(base)).unwrap();
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("mailpit"); } }
        let _cleanup = Cleanup(&state);
        state.start_service("mailpit").unwrap();
        let port = state.manager.snapshot("mailpit").unwrap().port.unwrap();
        assert_ne!(port, base);
        let url = state.service_web_url("mailpit").unwrap();
        assert_eq!(url, format!("http://127.0.0.1:{port}/niceenv-mail"));
        let client = reqwest::blocking::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().unwrap();
        let response = client.get(&url).send().unwrap();
        assert!(response.status().is_success()); assert!(response.text().unwrap().contains("Mailpit"));
        let list = client.get(format!("{url}/api/v1/messages")).send().unwrap();
        assert!(list.status().is_success()); assert_eq!(list.json::<serde_json::Value>().unwrap()["total"], 0);
        r.port = Some(port);
        assert_eq!(generic_web_target(&r, None).unwrap(), url);
        state.store.set_port_override("mailpit", Some(port + 10)).unwrap();
        assert_eq!(state.service_web_url("mailpit").unwrap(), url);
        let pids = state.manager.snapshot("mailpit").unwrap().pids;
        state.stop_service("mailpit").unwrap();
        assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        assert_eq!(state.service_web_url("mailpit").unwrap_err().code, "SERVICE_NOT_RUNNING");
        drop(occupied);
    }

    #[test]
    #[ignore = "requires verified NSB_VERIFY_CONSUL_OLD executable (2.0.3) and NSB_VERIFY_CONSUL_ZIP (2.0.4)"]
    fn native_consul_keeps_kv_and_services_across_restart_port_change_and_upgrade() {
        let old_program = std::env::var_os("NSB_VERIFY_CONSUL_OLD").expect("set NSB_VERIFY_CONSUL_OLD");
        let new_zip = std::env::var_os("NSB_VERIFY_CONSUL_ZIP").expect("set NSB_VERIFY_CONSUL_ZIP");
        let (_temp, state, r) = fixture_version("consul", Some("2.0.3"));
        std::fs::copy(old_program, &r.bin).unwrap();
        // 使用旧安装快照，验证升级应用后无需重新安装就能启用持久化运行描述。
        let mut legacy = r.entry.clone();
        legacy.run = Some(serde_json::from_value(serde_json::json!({
            "args":["agent","-dev","-client","127.0.0.1","-http-port","{port}"], "health":"tcp", "healthTimeoutSec":20
        })).unwrap());
        let raw = serde_json::to_vec(&legacy).unwrap();
        let snapshot = r.root.join(".niceenv-package.json"); std::fs::write(&snapshot, &raw).unwrap();
        let base = (31000..40000).find(|base| CONSUL_TCP_OFFSETS.iter().all(|offset| {
            let port = (i32::from(*base) + offset) as u16;
            tcp_port_bindable(port) && std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok()
        })).unwrap();
        let occupied_dns = std::net::UdpSocket::bind(("127.0.0.1", base + 100)).unwrap();
        state.store.set_port_override("consul", Some(base)).unwrap();
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("consul"); } }
        let _cleanup = Cleanup(&state);
        let start = || { let result = state.start_service("consul"); assert!(result.is_ok(), "{result:?}\n{:?}", state.manager.tail("consul", 35)); };
        start();
        let port = state.manager.snapshot("consul").unwrap().port.unwrap(); assert_ne!(port, base);
        assert_eq!(std::fs::read(&snapshot).unwrap(), raw);
        let client = reqwest::blocking::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        let url = |port, path: &str| format!("http://127.0.0.1:{port}{path}");
        assert_eq!(client.put(url(port, "/v1/kv/niceenv/persisted")).body("重启和升级后保留").send().unwrap().error_for_status().unwrap().text().unwrap(), "true");
        client.put(url(port, "/v1/agent/service/register")).json(&serde_json::json!({"ID":"niceenv-fixture","Name":"niceenv-fixture",
            "Address":"127.0.0.1","Port":12345,"Tags":["preserved"]})).send().unwrap().error_for_status().unwrap();
        let self_info: serde_json::Value = client.get(url(port, "/v1/agent/self")).send().unwrap().error_for_status().unwrap().json().unwrap();
        let node_id = self_info["Config"]["NodeID"].clone(); assert!(!node_id.is_null());
        let verify = || {
            let current = state.manager.snapshot("consul").unwrap(); let port = current.port.unwrap();
            assert_eq!(client.get(url(port, "/v1/kv/niceenv/persisted?raw&consistent")).send().unwrap().error_for_status().unwrap().text().unwrap(), "重启和升级后保留");
            let services: serde_json::Value = client.get(url(port, "/v1/agent/services")).send().unwrap().error_for_status().unwrap().json().unwrap();
            assert_eq!(services["niceenv-fixture"]["Tags"][0], "preserved");
            let info: serde_json::Value = client.get(url(port, "/v1/agent/self")).send().unwrap().error_for_status().unwrap().json().unwrap();
            assert_eq!(info["Config"]["NodeID"], node_id);
            assert_eq!(info["DebugConfig"]["DevMode"], false);
            let console = state.service_web_url("consul").unwrap(); assert_eq!(console, url(port, "/ui"));
            let html = client.get(&console).send().unwrap().error_for_status().unwrap().text().unwrap();
            assert!(html.to_lowercase().contains("consul"));
            let script = regex::Regex::new(r#"src="([^"]+\.js(?:\?[^"]*)?)""#).unwrap();
            let asset = script.captures_iter(&html).last().expect("real Consul UI script");
            let asset_url = reqwest::Url::parse(&console).unwrap().join(&asset[1]).unwrap();
            let javascript = client.get(asset_url).send().unwrap().error_for_status().unwrap();
            assert!(!javascript.headers().get("content-type").unwrap().to_str().unwrap().contains("text/html"));
            assert!(javascript.bytes().unwrap().len() > 1000);
            // DNS 使用同组 UDP 端口，实际解析内置 consul 服务。
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap(); socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let query = b"\x46\x91\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x06consul\x07service\x06consul\x00\x00\x01\x00\x01";
            socket.send_to(query, ("127.0.0.1", port + 100)).unwrap(); let mut answer = [0u8; 4096];
            let (length, peer) = socket.recv_from(&mut answer).unwrap(); assert_eq!(peer.port(), port + 100);
            assert_eq!(&answer[..2], &query[..2]); assert_eq!(answer[3] & 0x0f, 0);
            assert!(u16::from_be_bytes([answer[6], answer[7]]) > 0);
            assert!(answer[..length].windows(6).any(|bytes| bytes == [0, 4, 127, 0, 0, 1]));
        };
        verify();
        state.restart_service("consul").unwrap(); verify();
        state.stop_service("consul").unwrap();
        // 重启时移动整组端口，Raft 必须仍能选出当前节点并读回原数据。
        state.store.set_port_override("consul", Some(base + 3000)).unwrap(); start(); verify();
        state.stop_service("consul").unwrap();
        let key = "consul@2.0.4"; std::fs::copy(&new_zip, state.paths.downloads().join(format!("{key}.pkg"))).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap(); runtime.block_on(state.install_package(key)).unwrap();
        state.set_active_version("consul", "2.0.4").unwrap(); start(); verify();
        let pids = state.manager.snapshot("consul").unwrap().pids;
        state.uninstall_package("consul@2.0.3").unwrap(); assert_eq!(state.manager.snapshot("consul").unwrap().pids, pids); verify();
        state.uninstall_package(key).unwrap(); assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        assert!(r.data.join("node-id").is_file()); assert!(r.data.join("raft").is_dir());
        std::fs::copy(&new_zip, state.paths.downloads().join(format!("{key}.pkg"))).unwrap();
        runtime.block_on(state.install_package(key)).unwrap(); start(); verify();
        state.stop_service("consul").unwrap();
        assert_eq!(state.service_web_url("consul").unwrap_err().code, "SERVICE_NOT_RUNNING");
        drop(occupied_dns);
    }

    #[test]
    fn rnacos_health_requires_live_owner_and_all_three_listeners() {
        let (_temp, state, mut r) = fixture("rnacos");
        let base = (22000..42000).find(|port| [0, 1000, 2000].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        let mut listeners: Vec<_> = [0, 1000, 2000].iter().map(|offset| std::net::TcpListener::bind(("127.0.0.1", base + offset)).unwrap()).collect();
        r.port = Some(base);
        register_services(&state.paths, &state.store, &state.manager);
        state.manager.adopt("rnacos", &[u32::MAX], Some(base));
        assert!(!rnacos_ports_ready(&state.manager, &r));
        // 只登记当前检查进程为监听所有者，验证归属判断本身。
        state.manager.adopt("rnacos", &[std::process::id()], Some(base));
        assert!(rnacos_ports_ready(&state.manager, &r));
        drop(listeners.pop());
        assert!(!rnacos_ports_ready(&state.manager, &r));
        state.manager.push_log("rnacos", "thread panicked at old run");
        state.manager.push_log("rnacos", RNACOS_START_MARKER);
        assert!(!rnacos_startup_panicked(&state.manager, "rnacos"));
        state.manager.push_log("rnacos", "thread panicked at this run");
        assert!(rnacos_startup_panicked(&state.manager, "rnacos"));
        state.manager.services.lock().remove("rnacos");
        drop(listeners);
    }

    #[test]
    #[ignore = "requires NSB_VERIFY_RNACOS pointing to the official r-nacos 0.8.6 executable"]
    fn native_rnacos_loads_config_keeps_data_and_moves_all_ports_together() {
        let executable = std::env::var_os("NSB_VERIFY_RNACOS").expect("set NSB_VERIFY_RNACOS");
        let (_temp, state, mut r) = fixture_version("rnacos", Some("0.8.6"));
        std::fs::copy(executable, &r.bin).unwrap();
        let base = (22000..42000).find(|port| [0, 1, 1000, 1001, 2000, 2001].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        let occupied_grpc = std::net::TcpListener::bind(("127.0.0.1", base + 1000)).unwrap();
        state.store.set_port_assign("rnacos", base).unwrap();
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        r.port = Some(base);
        let config = r.etc.join(".env");
        let content = format!("{}\n# native fixture\nRNACOS_SDK_HOST=127.0.0.1\nRNACOS_CONSOLE_HOST=127.0.0.1\nRNACOS_HTTP_WORKERS=1\nRNACOS_ENABLE_OPEN_API_AUTH=false\nRNACOS_INIT_ADMIN_USERNAME=fixture\nRNACOS_INIT_ADMIN_PASSWORD=fixture-{}\n", expand_config(r.spec.config_template.as_deref().unwrap(), &r), rand::random::<u64>());
        std::fs::write(&config, content).unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("rnacos"); } }
        let _cleanup = Cleanup(&state);
        state.start_service("rnacos").unwrap();
        let port = state.manager.snapshot("rnacos").unwrap().port.unwrap();
        assert_ne!(port, base);
        let client = reqwest::blocking::Client::builder().no_proxy().pool_max_idle_per_host(0).timeout(Duration::from_secs(3)).build().unwrap();
        let url = format!("http://127.0.0.1:{port}/nacos/v1/cs/configs");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let response = client.post(&url).form(&[("dataId", "niceenv-fixture"), ("group", "DEFAULT_GROUP"), ("content", "persisted-fixture")]).send().unwrap();
            if response.status().is_success() && response.text().unwrap().trim() == "true" {
                // 上游在 Raft 尚未就绪时也可能返回 true，必须读回确认写入确实生效。
                let readback = client.get(&url).query(&[("dataId", "niceenv-fixture"), ("group", "DEFAULT_GROUP")]).send().unwrap();
                if readback.status().is_success() && readback.text().unwrap() == "persisted-fixture" { break; }
            }
            assert!(std::time::Instant::now() < deadline, "r-nacos config writer did not become ready: {:?}", state.manager.tail("rnacos", 60));
            std::thread::sleep(Duration::from_millis(200));
        }
        let console = client.get(format!("http://127.0.0.1:{}/rnacos/", port + 2000)).send().unwrap();
        assert!(console.status().is_success());
        assert!(console.text().unwrap().to_ascii_lowercase().contains("<html"));
        assert!(r.data.join("nacos_db").is_dir());
        assert!(!r.root.join("nacos_db").exists());
        let pids = state.manager.snapshot("rnacos").unwrap().pids;
        state.stop_service("rnacos").unwrap();
        assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        // 换一组未使用过的端口制造第二次冲突，不把 Windows TCP 释放延迟当作产品错误。
        let requested = (base + 10..42000).find(|port| [0, 1000, 2000].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        state.store.set_port_override("rnacos", Some(requested)).unwrap();
        let occupied_console = std::net::TcpListener::bind(("127.0.0.1", requested + 2000)).unwrap();
        state.start_service("rnacos").unwrap();
        let second = state.manager.snapshot("rnacos").unwrap().port.unwrap();
        assert_ne!(second, requested);
        let url = format!("http://127.0.0.1:{second}/nacos/v1/cs/configs");
        // HTTP 监听建立后，Raft 的本地数据重放仍可能尚未完成。
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let response = client.get(&url).query(&[("dataId", "niceenv-fixture"), ("group", "DEFAULT_GROUP")]).send().unwrap();
            let status = response.status(); let body = response.text().unwrap();
            if status.is_success() && body == "persisted-fixture" { break; }
            assert!(std::time::Instant::now() < deadline, "persisted config missing: {status} {body}");
            std::thread::sleep(Duration::from_millis(200));
        }
        state.stop_service("rnacos").unwrap();
        let content = std::fs::read_to_string(&config).unwrap();
        assert!(content.contains("# native fixture"));
        std::fs::write(&config, content.replace("RNACOS_ENABLE_OPEN_API_AUTH=false", "RNACOS_ENABLE_OPEN_API_AUTH=true")).unwrap();
        state.start_service("rnacos").unwrap();
        let third = state.manager.snapshot("rnacos").unwrap().port.unwrap();
        assert_eq!(state.store.get_port_assign("rnacos"), Some(third));
        let response = client.get(format!("http://127.0.0.1:{third}/nacos/v1/cs/configs")).query(&[("dataId", "niceenv-fixture"), ("group", "DEFAULT_GROUP")]).send().unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        state.stop_service("rnacos").unwrap();
        assert!([0, 1000, 2000].iter().all(|offset| !tcp_port_open(third + offset)));
        // 控制台线程失败时 SDK 仍可能启动，必须报错并清理整个临时服务。
        let content = std::fs::read_to_string(&config).unwrap().replace("RNACOS_CONSOLE_HOST=127.0.0.1", "RNACOS_CONSOLE_HOST=192.0.2.1");
        std::fs::write(&config, content).unwrap();
        let mut entry = r.entry.clone(); entry.run.as_mut().unwrap().health_timeout_sec = 3;
        std::fs::write(r.root.join(".niceenv-package.json"), serde_json::to_vec(&entry).unwrap()).unwrap();
        let error = state.start_service("rnacos").unwrap_err();
        assert!(matches!(error.code.as_str(), "SERVICE_START_TIMEOUT" | "SERVICE_RUNTIME_PANIC"), "{error:?}");
        let failed_port = state.store.get_port_assign("rnacos").unwrap();
        assert!([0, 1000, 2000].iter().all(|offset| !tcp_port_open(failed_port + offset)));
        drop(occupied_console); drop(occupied_grpc);
    }

    #[test]
    #[ignore = "requires NSB_VERIFY_RNACOS_BROKEN pointing to the official r-nacos 0.8.7 Windows executable"]
    fn native_rnacos_upstream_panic_cannot_report_a_healthy_service() {
        let executable = std::env::var_os("NSB_VERIFY_RNACOS_BROKEN").expect("set NSB_VERIFY_RNACOS_BROKEN");
        let (_temp, state, mut r) = fixture_version("rnacos", Some("0.8.7"));
        std::fs::copy(executable, &r.bin).unwrap();
        let port = (30000..42000).find(|port| [0, 1000, 2000].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        state.store.set_port_override("rnacos", Some(port)).unwrap(); r.port = Some(port);
        let content = format!("{}\nRNACOS_SDK_HOST=127.0.0.1\nRNACOS_CONSOLE_HOST=127.0.0.1\nRNACOS_HTTP_WORKERS=1\n", expand_config(r.spec.config_template.as_deref().unwrap(), &r));
        std::fs::write(r.etc.join(".env"), content).unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("rnacos"); } }
        let _cleanup = Cleanup(&state);
        let error = state.start_service("rnacos").unwrap_err();
        assert_eq!(error.code, "SERVICE_RUNTIME_PANIC", "{error:?}");
        assert!(state.manager.snapshot("rnacos").unwrap().pids.is_empty());
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while [0, 1000, 2000].iter().any(|offset| tcp_port_open(port + offset)) {
            assert!(std::time::Instant::now() < deadline, "failed r-nacos kept a listener open");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    #[ignore = "requires NSB_VERIFY_CADDY pointing to a native Caddy executable"]
    fn native_caddy_keeps_config_and_actual_port_in_sync_across_restarts() {
        use std::io::{Read, Write};
        let executable = std::env::var_os("NSB_VERIFY_CADDY").expect("set NSB_VERIFY_CADDY");
        let (_temp, state, mut r) = fixture("caddy");
        std::fs::copy(executable, &r.bin).unwrap();
        // 临时监听只对回环开放；不自动签发证书，不接触真实站点。
        let mut entry = r.entry.clone();
        entry.run.as_mut().unwrap().config_template = Some(r.spec.config_template.as_ref().unwrap().replace("http://:{port}", "http://127.0.0.1:{port}"));
        std::fs::write(std::path::Path::new(&r.inst.install_path).join(".niceenv-package.json"), serde_json::to_vec(&entry).unwrap()).unwrap();
        r = resolve(&state.store, &state.paths, "caddy").unwrap();
        let occupied = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let requested = occupied.local_addr().unwrap().port();
        state.store.set_port_assign("caddy", requested).unwrap(); state.store.set_setting("autoFallbackPort", "true").unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("caddy"); } }
        let _cleanup = Cleanup(&state);
        let request = |port: u16| {
            let mut stream = std::net::TcpStream::connect_timeout(&([127,0,0,1], port).into(), Duration::from_secs(3)).unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            write!(stream, "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").unwrap();
            let mut response = String::new(); stream.read_to_string(&mut response).unwrap(); response
        };
        std::fs::create_dir_all(r.data.join("www")).unwrap(); std::fs::write(r.data.join("www/index.html"), "native-port-fixture").unwrap();
        state.start_service("caddy").unwrap();
        let first = state.manager.snapshot("caddy").unwrap().port.unwrap(); assert_ne!(first, requested);
        assert!(request(first).contains("native-port-fixture"));
        state.stop_service("caddy").unwrap();
        let config = r.etc.join("Caddyfile");
        let custom = std::fs::read_to_string(&config).unwrap().replace("file_server", "header X-NiceEnv preserved\n\tfile_server");
        std::fs::write(&config, format!("# custom comment\n{custom}")).unwrap();
        let occupied_again = std::net::TcpListener::bind(("127.0.0.1", first)).unwrap();
        state.start_service("caddy").unwrap();
        let second = state.manager.snapshot("caddy").unwrap().port.unwrap(); assert_ne!(second, first);
        let response = request(second); assert!(response.contains("native-port-fixture")); assert!(response.to_ascii_lowercase().contains("x-niceenv: preserved"));
        assert_eq!(state.store.get_port_assign("caddy"), Some(second));
        assert_eq!(state.store.get_setting("portOverride.caddy").unwrap(), second.to_string());
        state.stop_service("caddy").unwrap();
        state.start_service("caddy").unwrap(); assert_eq!(state.manager.snapshot("caddy").unwrap().port, Some(second));
        assert!(request(second).contains("native-port-fixture")); state.stop_service("caddy").unwrap();
        assert!(std::fs::read_to_string(config).unwrap().starts_with("# custom comment\n"));
        drop(occupied_again); drop(occupied);
    }
}
