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
    let etc = if managed_sftpgo(&entry, &spec) { sftpgo_config_dir(store, paths)? } else { paths.etc_dir(&id, &version) };
    let log = paths.service_log(&service_id.replace('@', "_"));
    std::fs::create_dir_all(&data)?;
    std::fs::create_dir_all(&etc)?;

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

fn managed_sftpgo(entry: &PackageManifestEntry, spec: &ServiceRunSpec) -> bool {
    entry.id == "sftpgo" && spec.single_instance && spec.health == "tcp"
        && spec.args == ["serve", "--config-dir", "{etc}", "--log-file-path", "{data}/sftpgo.log"]
        && spec.env.as_ref().is_some_and(|env| env.get("SFTPGO_SFTPD__BINDINGS__0__PORT").map(String::as_str) == Some("{port}")
            && env.get("SFTPGO_HTTPD__BINDINGS__0__PORT").map(String::as_str) == Some("{port+6058}"))
}

/// 绑定相对配置目录；不复制数据库或 SSH 主机密钥，切换程序版本继续使用原目录。
fn sftpgo_config_dir(store: &Store, paths: &Paths) -> Result<PathBuf> {
    if let Some(relative) = store.get_setting_checked(SFTPGO_CONFIG_BINDING)? {
        let parts: Vec<_> = relative.split('/').collect();
        if parts.len() != 3 || parts[0] != "etc" || parts[1] != "sftpgo" || parts[2].is_empty()
            || matches!(parts[2], "." | "..") || !parts[2].bytes().all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(&c)) {
            return Err(AppError::new("SFTPGO_CONFIG_PATH", "SFTPGo 的已绑定配置目录无效"));
        }
        let path = crate::paths::checked_data_path(&paths.base, &relative)?;
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
            .with_hint("请先备份各目录，再在 etc/sftpgo 下保留需要继续使用的一份，其余移至备份目录后重试。")),
    }
}

struct SftpgoConfig {
    file: PathBuf,
    env: Vec<(String, String)>,
    web_target: Result<String>,
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

/// 仅收集 env.d 声明的键名；实际值、引号和插值仍交给 SFTPGo 的 gotenv 处理。
fn sftpgo_env_keys(paths: &Paths, directory: &std::path::Path) -> Result<std::collections::HashSet<String>> {
    let mut keys = std::collections::HashSet::new();
    let relative = directory.join("env.d").strip_prefix(&paths.base).map_err(|_| AppError::new("SFTPGO_CONFIG_PATH", "环境配置路径无效"))?.to_path_buf();
    let directory = crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(&relative))?;
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(keys),
        Err(error) => return Err(error.into()),
    };
    let assignment = regex::Regex::new(r"(?m)^[\t ]*(?:export[\t ]+)?([A-Za-z_][A-Za-z0-9_]*)[\t ]*=").unwrap();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(&paths.base).map_err(|_| AppError::new("SFTPGO_CONFIG_PATH", "环境配置路径无效"))?;
        let path = crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
        let metadata = std::fs::metadata(&path)?;
        // 与上游的文件限制相同，不读取大文件或目录。
        if !metadata.is_file() || metadata.len() > 1024 * 1024 { continue; }
        let content = std::fs::read_to_string(path)?;
        for capture in assignment.captures_iter(&content) {
            let key = if cfg!(windows) { capture[1].to_ascii_uppercase() } else { capture[1].to_string() };
            keys.insert(key);
        }
    }
    Ok(keys)
}

fn prepare_sftpgo(store: &Store, paths: &Paths, r: &Resolved) -> Result<SftpgoConfig> {
    let existing = sftpgo_config_file(&r.etc)?;
    let previously_started = store.get_setting_checked(SFTPGO_CONFIG_BINDING)?.is_some();
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
    let file_env_keys = sftpgo_env_keys(paths, &r.etc)?;
    // env.d 的变量由上游加载，不能凭 JSON/YAML 猜测它最终选择的外部数据库或密钥。
    // 已确认使用文件配置的本地状态丢失时，拦住上游自动创建空库/更换主机身份。
    if previously_started {
        let value = |key: &str, pointer: &str, default: &str| {
            r.spec.env.as_ref().and_then(|env| env.get(key)).map(|value| expand(value, r))
                .or_else(|| std::env::var(key).ok())
                .unwrap_or_else(|| config.pointer(pointer).and_then(|value| value.as_str()).unwrap_or(default).to_string())
        };
        let require = |name: &str| -> Result<()> {
            let path = r.etc.join(name);
            let present = std::fs::metadata(&path).map(|m| m.is_file() && m.len() > 0).unwrap_or(false);
            if !present { return Err(AppError::new("SFTPGO_STATE_MISSING", "SFTPGo 原数据库或主机密钥缺失，未自动重新初始化")
                .with_hint(format!("请先恢复原文件：{}", path.display()))); }
            Ok(())
        };
        let driver = value("SFTPGO_DATA_PROVIDER__DRIVER", "/data_provider/driver", "sqlite");
        let connection = value("SFTPGO_DATA_PROVIDER__CONNECTION_STRING", "/data_provider/connection_string", "");
        if matches!(driver.as_str(), "bolt" | "sqlite") && connection.is_empty()
            && ["SFTPGO_DATA_PROVIDER__DRIVER", "SFTPGO_DATA_PROVIDER__NAME", "SFTPGO_DATA_PROVIDER__CONNECTION_STRING"].iter().all(|key| !file_env_keys.contains(*key)) {
            require(&value("SFTPGO_DATA_PROVIDER__NAME", "/data_provider/name", "sftpgo.db"))?;
        }
        if !file_env_keys.contains("SFTPGO_SFTPD__HOST_KEYS") && r.spec.env.as_ref().is_none_or(|env| !env.contains_key("SFTPGO_SFTPD__HOST_KEYS")) && std::env::var_os("SFTPGO_SFTPD__HOST_KEYS").is_none() {
            let keys: Vec<_> = config.pointer("/sftpd/host_keys").and_then(|v| v.as_array()).into_iter().flatten().filter_map(|v| v.as_str()).collect();
            if keys.is_empty() { for name in ["id_rsa", "id_ecdsa", "id_ed25519"] { require(name)?; } }
            else { for name in keys { require(name)?; } }
        }
    }
    let mut env = Vec::new();
    for (pointer, key, default) in [
        ("/httpd/templates_path", "SFTPGO_HTTPD__TEMPLATES_PATH", "templates"),
        ("/httpd/static_files_path", "SFTPGO_HTTPD__STATIC_FILES_PATH", "static"),
        ("/httpd/openapi_path", "SFTPGO_HTTPD__OPENAPI_PATH", "openapi"),
        ("/smtp/templates_path", "SFTPGO_SMTP__TEMPLATES_PATH", "templates"),
    ] {
        if !file_env_keys.contains(key) && std::env::var_os(key).is_none()
            && config.pointer(pointer).is_none_or(|value| value.as_str() == Some(default)) {
            env.push((key.into(), r.root.join(default).to_string_lossy().into_owned()));
        }
    }
    let file = existing.unwrap_or_else(|| r.etc.join(source.file_name().unwrap()));
    if file != source {
        crate::paths::write_with_backup_expected(&file, &content, &paths.backup(), Some(None))?;
    }
    let web_target = sftpgo_web_target(r, &config, &file_env_keys);
    Ok(SftpgoConfig { file, env, web_target })
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

fn sftpgo_web_target(r: &Resolved, config: &serde_json::Value, file_env_keys: &std::collections::HashSet<String>) -> Result<String> {
    let value = |key: &str, pointer: &str, default: &str| -> Result<String> {
        if let Some(value) = r.spec.env.as_ref().and_then(|env| env.get(key)) { return Ok(expand(value, r)); }
        if let Ok(value) = std::env::var(key) { return Ok(value); }
        if file_env_keys.contains(key) {
            return Err(web_unavailable("env.d 自定义了管理台地址；请按该文件访问，或把管理台地址设置放入 SFTPGo 的 JSON/YAML 配置后重启。"));
        }
        Ok(config.pointer(pointer).map(|value| value.as_str().map(str::to_string).unwrap_or_else(|| value.to_string())).unwrap_or_else(|| default.into()))
    };
    let boolean = |value: String| -> Result<bool> {
        match value.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" => Ok(true), "false" | "f" | "0" => Ok(false),
            _ => Err(web_unavailable("管理台开关配置无法识别，请检查配置后重启。")),
        }
    };
    if !boolean(value("SFTPGO_HTTPD__BINDINGS__0__ENABLE_WEB_ADMIN", "/httpd/bindings/0/enable_web_admin", "true")?)? {
        return Err(web_unavailable("SFTPGo Web Admin 已关闭；如需管理台，请启用后重启服务。"));
    }
    let address = value("SFTPGO_HTTPD__BINDINGS__0__ADDRESS", "/httpd/bindings/0/address", "")?;
    let https = boolean(value("SFTPGO_HTTPD__BINDINGS__0__ENABLE_HTTPS", "/httpd/bindings/0/enable_https", "false")?)?;
    let root = value("SFTPGO_HTTPD__WEB_ROOT", "/httpd/web_root", "")?;
    let root = if root.starts_with('/') { root.as_str() } else { "" };
    let port = r.port.and_then(|port| port.checked_add(6058)).ok_or_else(|| web_unavailable("管理台派生端口超出范围。"))?;
    local_web_url(&address, port, https, &format!("{root}/web/admin"))
}

/// 仅识别由本程序明确传入监听端口的运行描述；不猜测自定义命令的端口。
fn generic_web_target(r: &Resolved, sftpgo: Option<&SftpgoConfig>) -> Result<String> {
    if let Some(config) = sftpgo { return config.web_target.clone(); }
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
        tcp_port_bindable(port) && (r.entry.id != "coredns" || std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok())));
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
        if r.entry.id == "coredns" {
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
        ports.iter().all(|port| listeners.iter().any(|(p, pid)| p == port && pids.contains(pid))))
}

fn wait_sftpgo_healthy(manager: &ServiceManager, r: &Resolved, timeout: Duration) -> bool {
    let Some(ports) = r.port.and_then(|port| port.checked_add(6058).map(|web| [port, web])) else { return false; };
    let deadline = std::time::Instant::now() + timeout;
    let mut ready_since = None;
    while std::time::Instant::now() < deadline {
        if owned_ports_ready(manager, &r.service_id, &ports) {
            if ready_since.get_or_insert_with(std::time::Instant::now).elapsed() >= Duration::from_millis(300) { return true; }
        } else { ready_since = None; }
        if manager.snapshot(&r.service_id).is_none_or(|service| service.pids.iter().all(|pid| !platform::process_alive(*pid))) { return false; }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

const RNACOS_START_MARKER: &str = "r-nacos：开始本次启动检查";

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
    } else if managed_rnacos(&r) {
        wait_rnacos_healthy(manager, &r, timeout)
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
                if r.port.is_some() {
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
    fn console_targets_use_resolved_ports_and_preserve_sftpgo_web_configuration() {
        for (id, offset, path) in [("mailpit", 0, "/"), ("minio", 1, "/"), ("consul", 0, "/ui"), ("rnacos", 2000, "/rnacos"), ("qdrant", 0, "/dashboard")] {
            let (_temp, _state, mut r) = fixture(id); r.port = Some(31000);
            assert_eq!(generic_web_target(&r, None).unwrap(), format!("http://127.0.0.1:{}{path}", 31000 + offset));
            r.spec.args = vec!["custom".into()];
            assert_eq!(generic_web_target(&r, None).unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        }
        let (_temp, _state, mut r) = fixture("sftpgo"); r.port = Some(31000);
        let config = serde_json::json!({"httpd":{"web_root":"/custom/../中文 path","bindings":[{"address":"::","enable_https":true,"enable_web_admin":true}]}});
        let keys = std::collections::HashSet::new();
        assert_eq!(sftpgo_web_target(&r, &config, &keys).unwrap(), "https://[::1]:37058/%E4%B8%AD%E6%96%87%20path/web/admin");
        r.spec.env.as_mut().unwrap().insert("SFTPGO_HTTPD__WEB_ROOT".into(), "/environment".into());
        assert_eq!(sftpgo_web_target(&r, &config, &keys).unwrap(), "https://[::1]:37058/environment/web/admin");
        r.spec.env.as_mut().unwrap().insert("SFTPGO_HTTPD__BINDINGS__0__ENABLE_WEB_ADMIN".into(), "false".into());
        assert!(sftpgo_web_target(&r, &config, &keys).unwrap_err().hint.unwrap().contains("已关闭"));
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
    #[ignore = "requires NSB_VERIFY_SFTPGO_OLD and NSB_VERIFY_SFTPGO_NEW pointing to official portable 2.7.5/2.7.6 directories"]
    fn native_sftpgo_transfers_files_and_keeps_accounts_keys_and_config_across_versions() {
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
        assert_eq!(web_url, format!("http://127.0.0.1:{}/niceenv-console/web/admin", first + 6058));
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
        install("2.7.6", &new_source); state.set_active_version("sftpgo", "2.7.6").unwrap();
        let requested = (base + 20..42000).find(|port| [0, 6058].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        let occupied_sftp = std::net::TcpListener::bind(("127.0.0.1", requested)).unwrap();
        state.store.set_port_override("sftpgo", Some(requested)).unwrap();
        state.start_service("sftpgo").unwrap_or_else(|e| panic!("{e:?}\n{:?}", state.manager.tail("sftpgo", 20)));
        let second = state.manager.snapshot("sftpgo").unwrap().port.unwrap(); assert_ne!(second, requested);
        assert_eq!(state.service_web_url("sftpgo").unwrap(), format!("http://127.0.0.1:{}/niceenv-console/web/admin", second + 6058));
        assert_eq!(state.manager.snapshot("sftpgo").unwrap().version.as_deref(), Some("2.7.6"));
        assert_eq!(crate::certdeploy::probe_ssh("127.0.0.1", second).unwrap().fingerprint, fingerprint);
        let response = client.get(format!("http://127.0.0.1:{}/api/v2/users/native-user", second + 6058)).bearer_auth(token(second)).send().unwrap();
        assert!(response.status().is_success()); transfer(second, false);
        assert_eq!(std::fs::read(&config_path).unwrap(), original_config);
        assert_eq!(state.store.get_setting(SFTPGO_CONFIG_BINDING).as_deref(), Some("etc/sftpgo/2.7.5"));
        assert!(!state.paths.etc_dir("sftpgo", "2.7.6").join("sftpgo.db").exists());
        state.stop_service("sftpgo").unwrap();
        for name in ["sftpgo.db", "id_ed25519"] {
            let original = legacy_dir.join(name); let backup = legacy_dir.join(format!("{name}.preserved"));
            std::fs::rename(&original, &backup).unwrap();
            assert_eq!(state.start_service("sftpgo").unwrap_err().code, "SFTPGO_STATE_MISSING");
            assert!(!original.exists()); assert!(state.manager.snapshot("sftpgo").unwrap().pids.is_empty());
            std::fs::rename(backup, original).unwrap();
        }
        std::fs::write(&config_path, "{broken").unwrap();
        assert_eq!(state.start_service("sftpgo").unwrap_err().code, "SFTPGO_CONFIG_INVALID");
        assert!(state.manager.snapshot("sftpgo").unwrap().pids.is_empty());
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
