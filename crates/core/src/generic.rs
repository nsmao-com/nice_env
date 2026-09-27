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
    let etc = paths.etc_dir(&id, &version);
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

fn prepare_config(paths: &Paths, r: &Resolved) -> Result<()> {
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
        let content = match &previous { Some(current) => sync_config_ports(current, tpl, r)?, None => expand_config(tpl, r) };
        crate::paths::write_with_backup_expected(&path, &content, &paths.backup(), Some(previous.as_deref().map(str::as_bytes)))?;
    }
    Ok(())
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
    let healthy = if r.entry.id == "coredns" {
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
        if r.entry.id == "coredns" { crate::ops::stop_service(store, paths, manager, &r.service_id)?; }
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
        let temp = tempfile::tempdir().unwrap(); let paths = Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let state = crate::CoreState {
            store: Store::open(paths.db()).unwrap(), paths,
            installer: crate::install::Installer { manifest: serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap() },
            manager: Arc::new(ServiceManager::new()),
            downloader: Arc::new(crate::download::Downloader::new()), emit: Arc::new(|_| {}),
            watchdog: Arc::new(crate::watchdog::Watchdog::new()),
        };
        let mut entry = state.installer.template_for(id).unwrap();
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
