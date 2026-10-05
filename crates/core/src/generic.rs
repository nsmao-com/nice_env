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
    expand_paths(
        &substitute_ports(template, r.port).replace("{httpPort}", &r.http_port.to_string()),
        r,
        str::to_owned,
    )
}

static PATH_TOKEN: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(r"\{(root|data|etc|log|bin)\}").expect("constant path token")
});

fn expand_paths(template: &str, r: &Resolved, encode: impl Fn(&str) -> String) -> String {
    // 只扫描模板一次，目录名里恰好包含 {etc} 等字面文本时不能二次展开。
    PATH_TOKEN
        .replace_all(template, |captures: &regex::Captures<'_>| {
            let value = match &captures[1] {
                "root" => crate::paths::portable_path_text(&r.root),
                "data" => crate::paths::portable_path_text(&r.data),
                "etc" => crate::paths::portable_path_text(&r.etc),
                "log" => crate::paths::portable_path_text(&r.log),
                "bin" => crate::paths::portable_path_text(&r.bin),
                _ => unreachable!(),
            };
            encode(&value)
        })
        .into_owned()
}

/// 只在新建配置时展开路径；保留模板布局和注释，按各格式的字符串规则引用。
pub fn expand_config(template: &str, r: &Resolved) -> Result<String> {
    let template =
        substitute_ports(template, r.port).replace("{httpPort}", &r.http_port.to_string());
    let file = r
        .spec
        .config_file
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    if file == "caddyfile" {
        return crate::caddy::expand_template_paths(&template, |value| {
            PATH_TOKEN
                .is_match(value)
                .then(|| expand_paths(value, r, str::to_owned))
        });
    }
    let yaml = file.ends_with(".yaml") || file.ends_with(".yml");
    let env = file == ".env" || file.ends_with(".env");
    let ini = file.ends_with(".ini") || file.ends_with(".cnf");
    let invalid = || {
        AppError::new("CONFIG_TEMPLATE", "服务配置模板中的路径格式无法安全展开")
            .with_hint("请检查路径所在的配置值和引号；原有配置不会被覆盖。")
    };
    template
        .split_inclusive('\n')
        .map(|line| {
            if !PATH_TOKEN.is_match(line) || line.trim_start().starts_with(['#', ';']) {
                return Ok(line.to_string());
            }
            if !yaml && !env && !ini {
                return Err(invalid());
            }
            let separator = line
                .find(if yaml { ':' } else { '=' })
                .ok_or_else(invalid)?;
            let rest = &line[separator + 1..];
            let start = separator + 1 + rest.len() - rest.trim_start_matches([' ', '\t']).len();
            let mut quote = None;
            let mut escaped = false;
            let mut end = line.trim_end_matches(['\r', '\n']).len();
            for (offset, character) in line[start..end].char_indices() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if character == '\\' && quote != Some('\'') {
                    escaped = true;
                    continue;
                }
                if let Some(current) = quote {
                    if character == current {
                        quote = None;
                    }
                } else if matches!(character, '\'' | '"') {
                    quote = Some(character);
                } else if character == '#'
                    && (ini || offset == 0 || line[..start + offset].ends_with(char::is_whitespace))
                {
                    end = start + offset;
                    break;
                }
            }
            if quote.is_some() {
                return Err(invalid());
            }
            end = start + line[start..end].trim_end().len();
            let scalar = &line[start..end];
            let encode_double = |value: &str| {
                let json = serde_json::Value::String(value.to_string()).to_string();
                let inner = json[1..json.len() - 1].to_string();
                if env {
                    inner.replace('$', "\\$")
                } else {
                    inner
                }
            };
            let replacement = if scalar.starts_with('"') && scalar.ends_with('"') {
                expand_paths(scalar, r, encode_double)
            } else if scalar.starts_with('\'') && scalar.ends_with('\'') {
                expand_paths(scalar, r, |value| {
                    if yaml {
                        value.replace('\'', "''")
                    } else if env {
                        value.replace('\'', "'\\''")
                    } else {
                        value.replace('\\', "\\\\").replace('\'', "\\'")
                    }
                })
            } else if env {
                // 模板自身的环境变量仍交给 dotenv 展开，注入路径中的 $ 必须保留为字面值。
                let literal = scalar.replace('\\', "\\\\").replace('"', "\\\"");
                format!("\"{}\"", expand_paths(&literal, r, encode_double))
            } else {
                serde_json::Value::String(expand_paths(scalar, r, str::to_owned)).to_string()
            };
            Ok(format!("{}{replacement}{}", &line[..start], &line[end..]))
        })
        .collect()
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

/// 返回通用服务由清单推导出的受管数据目录。
///
/// 目录名只能来自已安装版本对应的 `run.dataDir` 或服务 id，不能由前端
/// 传入任意路径；同时复用应用数据目录的路径检查，避免目录穿越、设备名和
/// 软链接把“打开数据目录”变成任意文件系统访问入口。
pub fn service_data_dir(store: &Store, paths: &Paths, service_id: &str) -> Result<PathBuf> {
    let entry = manifest_entry_for(store, service_id).ok_or_else(|| {
        AppError::new("UNKNOWN_SERVICE", format!("清单里没有服务 {service_id}"))
            .with_hint("该服务可能是内置编排，或清单需要更新")
    })?;
    if is_builtin(&entry.id) {
        return Err(AppError::new(
            "SERVICE_DATA_DIR_UNSUPPORTED",
            format!("{} 的数据目录由专用管理页提供", entry.display_name),
        ));
    }
    let spec = entry
        .run
        .as_ref()
        .ok_or_else(|| AppError::new("NOT_A_SERVICE", "该套件不是可运行的服务"))?;
    let (id, version) = match service_id.split_once('@') {
        Some((id, version)) if !id.is_empty() && !version.is_empty() => {
            (id.to_string(), version.to_string())
        }
        Some(_) => return Err(AppError::new("BAD_SERVICE_ID", "服务版本无效")),
        None => {
            let installed = crate::ops::installed_by_choice(store, &entry.id)
                .ok_or_else(|| AppError::not_installed(&entry.display_name))?;
            (installed.id.clone(), installed.version.clone())
        }
    };
    store
        .find_installed(&id, Some(&version))
        .ok_or_else(|| AppError::not_installed(&format!("{} {}", entry.display_name, version)))?;
    let name = spec
        .data_dir
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&id);
    let relative = format!("data/{}", name.replace('\\', "/"));
    crate::paths::checked_data_path(&paths.base, &relative)
        .map_err(|error| AppError::io("解析服务数据目录", error))
}

/// 检查服务启动前必须具备的套件依赖。
///
/// 依赖校验放在通用服务入口之外复用，确保内置编排（Nginx、PHP、MySQL 等）
/// 和清单驱动服务使用同一条前置规则。重启流程也会在停止前调用它，避免
/// 因为依赖缺失把一个原本正在运行的服务先停掉。
pub fn ensure_dependencies(store: &Store, service_id: &str) -> Result<()> {
    let Some(entry) = manifest_entry_for(store, service_id) else {
        return Ok(());
    };
    let mut dependencies = entry
        .run
        .as_ref()
        .map(|run| run.requires.clone())
        .unwrap_or_default();
    dependencies.extend(entry.requires.iter().cloned());
    dependencies.sort();
    dependencies.dedup();
    let installed = store.list_installed().unwrap_or_default();
    let missing: Vec<String> = dependencies
        .iter()
        .filter(|dependency| {
            !installed
                .iter()
                .any(|package| crate::install::installed_package_satisfies(package, dependency))
        })
        .cloned()
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(AppError::new(
        "DEPENDENCY_MISSING",
        format!("{} 需要先安装：{}", entry.display_name, missing.join("、")),
    )
    .with_hint(format!(
        "到套件页安装 {} 后再启动或重启",
        missing.join("、")
    )))
}

/// 解析服务上下文：安装信息 + 清单条目 + run 描述 + 各占位符取值
pub fn resolve(store: &Store, paths: &Paths, service_id: &str) -> Result<Resolved> {
    resolve_with_sftpgo_directory(store, paths, service_id, None)
}

pub(crate) fn sftpgo_migration_cwd(
    store: &Store,
    paths: &Paths,
    directory: PathBuf,
) -> Result<PathBuf> {
    // 与启动使用同一份有效安装快照、活动版本和占位符；显式目录启用只读预览。
    let r = resolve_with_sftpgo_directory(store, paths, "sftpgo", Some(directory))?;
    let cwd = r
        .spec
        .cwd
        .as_ref()
        .map(|value| PathBuf::from(expand(value, &r)))
        .unwrap_or(r.root);
    std::path::absolute(cwd).map_err(|error| AppError::io("解析 SFTPGo 工作目录", error))
}

pub(crate) struct SftpgoMigration {
    pub cwd: Option<PathBuf>,
    pub resources: Vec<crate::configpaths::ResourcePath>,
    pub files: Vec<(PathBuf, Vec<u8>)>,
    pub provider: Option<crate::sftpgo_data::Provider>,
}

pub(crate) fn sftpgo_migrate_environment(
    store: &Store,
    paths: &Paths,
    directory: PathBuf,
    content: &str,
    json: bool,
    rebase: &crate::paths::DataPathRebase,
) -> Result<SftpgoMigration> {
    // 尚无安装记录的历史配置仍需迁移账号库；相对 connection_string 不能猜工作目录。
    if crate::ops::installed_by_choice(store, "sftpgo").is_none()
        && !directory.join("env.d").is_dir()
    {
        let environment = sftpgo_inherited_environment();
        let config = crate::configpaths::sftpgo_config(content, json)?;
        let (updated, resources) =
            crate::configpaths::sftpgo_environment_paths(&config, &environment, rebase)?;
        if updated.iter().any(|(key, value)| environment.get(key) != Some(value)) {
            return Err(AppError::new("DATA_DIR_ENV_OVERRIDE",
                "系统环境变量仍引用旧数据目录，未切换数据目录")
                .with_hint("请先检查 SFTPGo 系统环境变量；原账号库和文件已保留。"));
        }
        return Ok(SftpgoMigration {
            cwd: None,
            resources,
            files: Vec::new(),
            provider: crate::sftpgo_data::provider(content, json, &environment, &directory, None)?,
        });
    }
    let mut r = resolve_with_sftpgo_directory(store, paths, "sftpgo", Some(directory.clone()))?;
    if !managed_sftpgo(&r.entry, &r.spec) {
        return Err(AppError::new(
            "DATA_DIR_SFTPGO_RUN",
            "自定义 SFTPGo 启动参数无法确认环境配置目录，未切换数据目录",
        )
        .with_hint("请检查自定义启动参数中的配置目录；原数据和配置保持不变。"));
    }
    let config = crate::configpaths::sftpgo_config(content, json)?;
    let config_json = serde_json::to_value(&config)
        .map_err(|_| AppError::new("SFTPGO_CONFIG_INVALID", "SFTPGo 配置类型无效"))?;
    let files = sftpgo_env_files(paths, &directory)?;
    let (_, before, effective) = sftpgo_environment(&r, &config_json, &files)?;
    let cwd = r
        .spec
        .cwd
        .as_ref()
        .map(|value| PathBuf::from(expand(value, &r)))
        .unwrap_or_else(|| r.root.clone());
    let cwd =
        std::path::absolute(cwd).map_err(|error| AppError::io("解析 SFTPGo 工作目录", error))?;
    let mut provider = crate::sftpgo_data::provider(content, json, &effective, &directory, Some(&cwd))?;
    if let Some(provider) = &mut provider {
        if provider.driver == "bolt" { provider.executable = Some(r.bin.clone()); }
    }
    let snapshot_path = PathBuf::from(&r.inst.install_path).join(".niceenv-package.json");
    let original_snapshot = match std::fs::read(&snapshot_path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut snapshot = if let Some(bytes) = &original_snapshot {
        serde_json::from_slice::<serde_json::Value>(bytes)
            .map_err(|_| AppError::new("DATA_DIR_SFTPGO_RUN", "SFTPGo 安装快照无法解析"))?
    } else {
        serde_json::to_value(&r.entry)
            .map_err(|_| AppError::new("DATA_DIR_SFTPGO_RUN", "SFTPGo 运行配置无法保存"))?
    };
    let original_value = snapshot.clone();
    let mut raw_environment = effective.clone();
    for (key, value) in r.spec.env.iter().flatten() {
        raw_environment.insert(sftpgo_env_key(key), value.clone());
    }
    let (raw_paths, _) =
        crate::configpaths::sftpgo_environment_paths(&config, &raw_environment, rebase)?;
    let source_expansions = r
        .spec
        .env
        .iter()
        .flatten()
        .map(|(key, value)| (key.clone(), expand(value, &r)))
        .collect::<std::collections::HashMap<_, _>>();
    for path in [&mut r.bin, &mut r.root, &mut r.data, &mut r.etc, &mut r.log] {
        *path = PathBuf::from(rebase.path(&crate::paths::portable_path_text(path)));
    }
    let mut environment = r.spec.env.clone().unwrap_or_default();
    for (key, value) in &mut environment {
        let folded = sftpgo_env_key(key);
        if let Some(path) = raw_paths.get(&folded) {
            *value = path.clone();
        } else if expand(value, &r) != source_expansions[key] {
            *value = source_expansions[key].clone();
        }
    }
    if r.spec.env.is_some() {
        r.spec.env = Some(environment.clone());
    }
    if let Some(cwd) = &mut r.spec.cwd {
        *cwd = rebase.path(cwd);
    }
    if let Some(data) = &mut r.spec.data_dir {
        *data = rebase.path(data);
    }
    if r.spec.env.is_some() {
        snapshot["run"]["env"] = serde_json::to_value(environment)
            .map_err(|_| AppError::new("DATA_DIR_SFTPGO_RUN", "SFTPGo 环境配置无法保存"))?;
    }
    if let Some(cwd) = &r.spec.cwd {
        snapshot["run"]["cwd"] = cwd.clone().into();
    }
    if let Some(data) = &r.spec.data_dir {
        snapshot["run"]["dataDir"] = data.clone().into();
    }
    let migrated_config = crate::configpaths::rebase(content, "sftpgo", json, rebase)?;
    let migrated_config =
        serde_json::to_value(crate::configpaths::sftpgo_config(&migrated_config, json)?)
            .map_err(|_| AppError::new("SFTPGO_CONFIG_INVALID", "SFTPGo 配置类型无效"))?;
    let (_, after, _) = sftpgo_environment(&r, &migrated_config, &files)?;
    let (updated, resources) = rebase_sftpgo_env(&files, before, after, &config, rebase)?;
    let mut output = Vec::new();
    for (path, content) in updated {
        let original = std::fs::read(&path)?;
        let encoded = encode_sftpgo_env(&original, &content);
        if encoded.len() > 1024 * 1024 {
            return Err(sftpgo_env_error(&path, 1));
        }
        output.push((path, encoded));
    }
    if snapshot != original_value {
        output.push((
            snapshot_path,
            serde_json::to_vec_pretty(&snapshot)
                .map_err(|_| AppError::new("DATA_DIR_SFTPGO_RUN", "SFTPGo 安装快照无法保存"))?,
        ));
    } else if let Some(original) = original_snapshot {
        output.push((snapshot_path, original));
    }
    Ok(SftpgoMigration {
        cwd: Some(cwd),
        resources,
        files: output,
        provider,
    })
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
    let data = if id == "mariadb" { mariadb_data_dir(paths, &version)? } else {
        paths.data().join(spec.data_dir.clone().unwrap_or_else(|| id.clone()))
    };
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
pub(crate) fn resolve_port(
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

/// 仅识别内置的 RabbitMQ 单节点编排：AMQP 使用主端口，管理插件使用主端口
/// +10000，并由首次启动命令在当前 RABBITMQ_BASE 中启用。自定义运行描述不猜测
/// 管理台地址，也不强制等待额外端口。
fn managed_rabbitmq(entry: &PackageManifestEntry, spec: &ServiceRunSpec) -> bool {
    entry.id == "rabbitmq"
        && spec.args.is_empty()
        && spec.config_file.as_deref() == Some("rabbitmq.conf")
        && spec.init_bin.as_deref() == Some("rabbitmq-plugins.bat")
        && spec.init_args.as_deref() == Some(["enable".into(), "rabbitmq_management".into()].as_slice())
        && spec.env.as_ref().is_some_and(|env| {
            env.get("RABBITMQ_BASE").is_some_and(|value| value == "{data}")
                && env.get("RABBITMQ_NODE_PORT").is_some_and(|value| value == "{port}")
                && env.get("RABBITMQ_CONFIG_FILE").is_some_and(|value| value == "{etc}/rabbitmq.conf")
        })
        && spec.config_template.as_deref().is_some_and(|template| {
            template.contains("listeners.tcp.default = 127.0.0.1:{port}")
                && template.contains("management.tcp.port = {port+10000}")
                && template.contains("management.tcp.ip = 127.0.0.1")
        })
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
    if !crate::install::same_version(&latest.version, version) || (latest.current.as_deref() != expected_current && latest.current.as_deref() != Some(directory)) {
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
    let relative = directory
        .join("env.d")
        .strip_prefix(&paths.base)
        .map_err(|_| AppError::new("SFTPGO_CONFIG_PATH", "环境配置路径无效"))?
        .to_path_buf();
    let directory =
        crate::paths::checked_data_path(&paths.base, &crate::paths::portable_path_text(&relative))?;
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().to_str().is_none() {
            return Err(sftpgo_env_error(&path, 1));
        }
        let relative = path
            .strip_prefix(&paths.base)
            .map_err(|_| AppError::new("SFTPGO_CONFIG_PATH", "环境配置路径无效"))?;
        let path = crate::paths::checked_data_path(
            &paths.base,
            &crate::paths::portable_path_text(relative),
        )?;
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
fn sftpgo_parse_env(
    files: &[(PathBuf, String)],
    environment: std::collections::HashMap<String, String>,
) -> Result<std::collections::HashMap<String, String>> {
    sftpgo_parse_env_with(files, environment, &mut |_, _, _, value| {
        Ok(value.to_string())
    })
}

fn sftpgo_parse_env_with(
    files: &[(PathBuf, String)],
    mut environment: std::collections::HashMap<String, String>,
    visit: &mut impl FnMut(&std::path::Path, std::ops::Range<usize>, &str, &str) -> Result<String>,
) -> Result<std::collections::HashMap<String, String>> {
    static ASSIGNMENT: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(||
        regex::Regex::new(r#"\A[ \t\r\n\f]*(?:export[ \t\r\n\f]+)?([A-Za-z0-9_.]+)(?:[ \t\r\n\f]*=[ \t\r\n\f]*|:[ \t\r\n\f]+?)('(?:\'|[^'])*'|"(?:\"|[^"])*"|[^#\n]+)?[ \t\r\n\f]*(?:[ \t\r\n\f]*\#.*)?\z"#).unwrap());
    static VARIABLE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(||
        regex::Regex::new(r"(\\)?(\$)(\{?([A-Z0-9_]+)?\}?)").unwrap());
    static UNESCAPE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(||
        regex::Regex::new(r"\\([^$])").unwrap());
    for (path, content) in files {
        let mut normalized = Vec::with_capacity(content.len());
        let mut positions = vec![0];
        let mut position = 0;
        while position < content.len() {
            let byte = content.as_bytes()[position];
            position += 1;
            if byte == b'\r' {
                if content.as_bytes().get(position) == Some(&b'\n') {
                    position += 1;
                }
                normalized.push(b'\n');
            } else {
                normalized.push(byte);
            }
            positions.push(position);
        }
        let normalized = String::from_utf8(normalized).map_err(|_| sftpgo_env_error(path, 1))?;
        let mut offset = 0;
        let mut lines = normalized
            .split_inclusive('\n')
            .enumerate()
            .map(|(number, line)| {
                let start = offset;
                offset += line.len();
                (number, start, line.strip_suffix('\n').unwrap_or(line))
            });
        let mut parsed = std::collections::HashMap::<String, String>::new();
        let mut names = std::collections::HashMap::<String, String>::new();
        while let Some((number, start, raw)) = lines.next() {
            let invalid = || sftpgo_env_error(path, number + 1);
            if raw.len() >= 65535 || raw.contains('\0') { return Err(invalid()); }
            let mut line = raw.trim().to_string();
            if line.is_empty() || line.starts_with('#') { continue; }
            let leading = raw.len() - raw.trim_start().len();
            let mut end = start + raw.trim_end().len();
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
                let Some((_, next_start, next)) = lines.next() else {
                    return Err(invalid());
                };
                if next.len() >= 65535 || next.contains('\0') {
                    return Err(invalid());
                }
                line.push('\n');
                line.push_str(next);
                end = next_start + next.len();
                if next.rfind(ending).is_some_and(|i| i == 0 || next.as_bytes()[i - 1] != b'\\') { quote = None; }
            }
            let captures = ASSIGNMENT.captures(&line).ok_or_else(invalid)?;
            let key = captures[1].to_string();
            let folded = sftpgo_env_key(&key);
            if names.insert(folded, key.clone()).is_some_and(|previous| previous != key) {
                return Err(AppError::new("SFTPGO_ENV_AMBIGUOUS", "SFTPGo 环境配置包含重复的大小写变量名，请合并后重试")
                    .with_hint(path.display().to_string()));
            }
            let captured = captures.get(2);
            let raw_value = captured.map(|m| m.as_str()).unwrap_or("");
            let value_start = start
                + leading
                + captured.map(|m| m.start()).unwrap_or(line.len())
                + raw_value.len()
                - raw_value.trim_start().len();
            let value_end = end
                - (line.len() - captured.map(|m| m.end()).unwrap_or(line.len()))
                - (raw_value.len() - raw_value.trim_end().len());
            let range = positions[value_start.min(value_end)]..positions[value_end];
            let mut value = raw_value.trim().to_string();
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
            value = visit(path, range, &key, &value)?;
            if value.contains('\0') {
                return Err(invalid());
            }
            parsed.insert(key, value);
        }
        // 同一文件的同名赋值最后一项生效；跨文件和继承环境则保留先已有的值（含空值）。
        for (key, value) in parsed { environment.entry(sftpgo_env_key(&key)).or_insert(value); }
    }
    Ok(environment)
}

fn sftpgo_inherited_environment() -> std::collections::HashMap<String, String> {
    std::env::vars_os().filter_map(|(key, value)|
        Some((sftpgo_env_key(&key.into_string().ok()?), value.into_string().ok()?))).collect()
}

fn sftpgo_process_env(r: &Resolved) -> Result<std::collections::HashMap<String, String>> {
    let mut environment = sftpgo_inherited_environment();
    let mut names = std::collections::HashSet::new();
    for (key, value) in r.spec.env.iter().flatten() {
        let key = sftpgo_env_key(key);
        if !names.insert(key.clone()) { return Err(AppError::new("SFTPGO_ENV_AMBIGUOUS", "运行配置包含重复的大小写环境变量名，请合并后重试")); }
        environment.insert(key, expand(value, r));
    }
    Ok(environment)
}

fn sftpgo_env_literal(value: &str) -> Result<String> {
    let double = value
        .replace('\\', "\\\\")
        .replace('$', "\\$")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    for candidate in [
        format!("'{value}'"),
        format!("\"{double}\""),
        value.replace('$', "\\$"),
    ] {
        let probe = vec![(
            PathBuf::from("env.d"),
            format!("NICEENV_VALUE={candidate}\n"),
        )];
        if sftpgo_parse_env(&probe, Default::default())
            .is_ok_and(|parsed| parsed.get("NICEENV_VALUE").map(String::as_str) == Some(value))
        {
            return Ok(candidate);
        }
    }
    Err(AppError::new(
        "DATA_DIR_ENV_ENCODING",
        "环境变量无法按服务原有语法安全保存，未切换数据目录",
    )
    .with_hint(
        "原文件和数据已保留；请检查环境配置中的引号、换行与末尾反斜杠。错误不会展示变量值。",
    ))
}

fn rebase_sftpgo_env(
    files: &[(PathBuf, String)],
    before: std::collections::HashMap<String, String>,
    after: std::collections::HashMap<String, String>,
    config: &yaml_serde::Value,
    rebase: &crate::paths::DataPathRebase,
) -> Result<(
    Vec<(PathBuf, String)>,
    Vec<crate::configpaths::ResourcePath>,
)> {
    let mut assignments = Vec::new();
    let original = sftpgo_parse_env_with(files, before, &mut |_, _, key, value| {
        assignments.push((sftpgo_env_key(key), value.to_string()));
        Ok(value.to_string())
    })?;
    let (paths, mut resources) =
        crate::configpaths::sftpgo_environment_paths(config, &original, rebase)?;
    let mut expected = original.clone();
    expected.extend(paths);
    let mut desired = Vec::new();
    for (key, value) in assignments {
        let mut context = std::collections::HashMap::new();
        for guard in [
            "SFTPGO_DATA_PROVIDER__DRIVER",
            "SFTPGO_DATA_PROVIDER__CONNECTION_STRING",
        ] {
            if let Some(value) = original.get(guard) {
                context.insert(guard.into(), value.clone());
            }
        }
        context.insert(key.clone(), value.clone());
        let (paths, refs) = crate::configpaths::sftpgo_environment_paths(config, &context, rebase)?;
        desired.push(paths.get(&key).cloned().unwrap_or(value));
        resources.extend(refs);
    }
    let mut changes =
        std::collections::HashMap::<PathBuf, Vec<(std::ops::Range<usize>, String)>>::new();
    let mut index = 0;
    let migrated = sftpgo_parse_env_with(files, after.clone(), &mut |path, range, _, value| {
        let expected = &desired[index];
        index += 1;
        if value != expected {
            changes
                .entry(path.into())
                .or_default()
                .push((range, sftpgo_env_literal(expected)?));
        }
        Ok(expected.clone())
    })?;
    if migrated != expected {
        return Err(AppError::new(
            "DATA_DIR_ENV_OVERRIDE",
            "已有环境变量覆盖了服务配置，无法安全切换数据目录",
        )
        .with_hint(
            "请检查系统环境变量和服务运行配置中指向旧数据目录的路径；原文件与数据均已保留。",
        ));
    }
    let mut updated = files.to_vec();
    for (path, text) in &mut updated {
        if let Some(edits) = changes.remove(path) {
            for (range, value) in edits.into_iter().rev() {
                if text.get(range.clone()).is_none() {
                    return Err(sftpgo_env_error(path, 1));
                }
                text.replace_range(range, &value);
            }
        }
    }
    if sftpgo_parse_env(&updated, after)? != expected {
        return Err(AppError::new(
            "DATA_DIR_ENV_ENCODING",
            "环境配置转换后的值不一致，未切换数据目录",
        ));
    }
    Ok((updated, resources))
}

fn encode_sftpgo_env(original: &[u8], text: &str) -> Vec<u8> {
    if original.starts_with(&[0xff, 0xfe]) || original.starts_with(&[0xfe, 0xff]) {
        let mut output = original[..2].to_vec();
        for word in text.encode_utf16() {
            output.extend(if original[0] == 0xff {
                word.to_le_bytes()
            } else {
                word.to_be_bytes()
            });
        }
        output
    } else {
        let mut output = if original.starts_with(&[0xef, 0xbb, 0xbf]) {
            vec![0xef, 0xbb, 0xbf]
        } else {
            Vec::new()
        };
        output.extend_from_slice(text.as_bytes());
        output
    }
}

fn sftpgo_environment(
    r: &Resolved,
    config: &serde_json::Value,
    files: &[(PathBuf, String)],
) -> Result<(
    Vec<(String, String)>,
    std::collections::HashMap<String, String>,
    std::collections::HashMap<String, String>,
)> {
    let mut process_env = sftpgo_process_env(r)?;
    let mut effective_env = sftpgo_parse_env(files, process_env.clone())?;
    let mut env = Vec::new();
    for (pointer, key, default) in [
        (
            "/httpd/templates_path",
            "SFTPGO_HTTPD__TEMPLATES_PATH",
            "templates",
        ),
        (
            "/httpd/static_files_path",
            "SFTPGO_HTTPD__STATIC_FILES_PATH",
            "static",
        ),
        (
            "/httpd/openapi_path",
            "SFTPGO_HTTPD__OPENAPI_PATH",
            "openapi",
        ),
        (
            "/smtp/templates_path",
            "SFTPGO_SMTP__TEMPLATES_PATH",
            "templates",
        ),
    ] {
        if !effective_env.contains_key(key)
            && config
                .pointer(pointer)
                .is_none_or(|value| value.as_str() == Some(default))
        {
            let value = crate::paths::portable_path_text(&r.root.join(default));
            env.push((key.into(), value.clone()));
            process_env.insert(key.into(), value);
        }
    }
    // 默认资源路径也属于子进程环境，env.d 的插值必须能读取到相同的值。
    if !env.is_empty() {
        effective_env = sftpgo_parse_env(files, process_env.clone())?;
    }
    Ok((env, process_env, effective_env))
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
    let relative = source
        .strip_prefix(&paths.base)
        .map_err(|_| AppError::new("SFTPGO_CONFIG_PATH", "配置源必须位于托管数据目录"))?;
    let source =
        crate::paths::checked_data_path(&paths.base, &crate::paths::portable_path_text(relative))?;
    let metadata = std::fs::metadata(&source)?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 { return Err(AppError::new("SFTPGO_CONFIG_INVALID", "SFTPGo 配置必须是小于 1 MiB 的文本文件")); }
    let content = std::fs::read_to_string(&source)?;
    let json = match source.extension().and_then(|v| v.to_str()) {
        Some("json") => true,
        Some("yaml" | "yml") => false,
        _ => return Err(AppError::new("SFTPGO_CONFIG_FORMAT", "托管 SFTPGo 配置目前支持 JSON 或 YAML，请使用自定义模块运行其他格式")),
    };
    let config =
        serde_json::to_value(crate::configpaths::sftpgo_config(&content, json)?).map_err(|_| {
            AppError::new(
                "SFTPGO_CONFIG_INVALID",
                "SFTPGo 配置字段类型无法识别，原文件已保留",
            )
        })?;
    let files = sftpgo_env_files(paths, &r.etc)?;
    let (mut env, _, effective_env) = sftpgo_environment(r, &config, &files)?;
    let value = |key: &str, pointer: &str, default: &str| {
        effective_env.get(key).cloned().unwrap_or_else(|| {
            config
                .pointer(pointer)
                .and_then(|value| value.as_str())
                .unwrap_or(default)
                .to_string()
        })
    };
    let driver = value(
        "SFTPGO_DATA_PROVIDER__DRIVER",
        "/data_provider/driver",
        "sqlite",
    );
    let connection = value(
        "SFTPGO_DATA_PROVIDER__CONNECTION_STRING",
        "/data_provider/connection_string",
        "",
    );
    let database = if driver == "sqlite" {
        let connection = if connection.is_empty() {
            let name = value(
                "SFTPGO_DATA_PROVIDER__NAME",
                "/data_provider/name",
                "sftpgo.db",
            );
            if name.is_empty() {
                return Err(AppError::new(
                    "SFTPGO_CONFIG_INVALID",
                    "SQLite 数据库文件名不能为空",
                ));
            }
            // 上游直接拼接 file:{name}，#/?/% 会改变实际数据库位置；只覆盖子进程参数，不重写用户配置。
            let connection = crate::configpaths::sftpgo_sqlite_dsn(&r.etc, &name);
            env.push((
                "SFTPGO_DATA_PROVIDER__CONNECTION_STRING".into(),
                connection.clone(),
            ));
            connection
        } else {
            connection.clone()
        };
        let cwd = r
            .spec
            .cwd
            .as_ref()
            .map(|path| PathBuf::from(expand(path, r)))
            .unwrap_or_else(|| r.root.clone());
        crate::configpaths::sqlite_connection_path(&connection)?.map(|path| cwd.join(path))
    } else if driver == "bolt" && connection.is_empty() {
        Some(r.etc.join(value(
            "SFTPGO_DATA_PROVIDER__NAME",
            "/data_provider/name",
            "sftpgo.db",
        )))
    } else {
        None
    };
    // 使用实际生效的环境配置检查本地状态，避免 env.d 使丢失检查被跳过。
    let mut state_files = Vec::new();
    if previously_started {
        let mut require = |path: PathBuf| -> Result<()> {
            let present = std::fs::metadata(&path).map(|m| m.is_file() && m.len() > 0).unwrap_or(false);
            if !present { return Err(AppError::new("SFTPGO_STATE_MISSING", "SFTPGo 原数据库或主机密钥缺失，未自动重新初始化")
                .with_hint(format!("请先恢复原文件：{}", path.display()))); }
            state_files.push(path);
            Ok(())
        };
        if let Some(database) = database {
            require(database)?;
        }
        let keys: Vec<_> = if let Some(value) = effective_env.get("SFTPGO_SFTPD__HOST_KEYS") {
            if value.is_empty() {
                vec![]
            } else {
                value.split(',').collect()
            }
        } else {
            config
                .pointer("/sftpd/host_keys")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str())
                .collect()
        };
        if keys.is_empty() {
            for name in ["id_rsa", "id_ecdsa", "id_ed25519"] {
                require(r.etc.join(name))?;
            }
        } else {
            for name in keys {
                require(r.etc.join(name))?;
            }
        }
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

struct MinioSettings {
    browser: bool,
    subpath: String,
    destination: Option<String>,
}

/// 内置本地单盘运行描述；允许指定证书目录和日志选项，不猜测自定义地址或分布式布局。
fn managed_minio(r: &Resolved) -> bool {
    if r.entry.id != "minio" || !r.spec.single_instance || r.spec.health != "tcp" || r.spec.init_args.is_some()
        || !r.spec.args.starts_with(&["server".into(), "{data}/data".into(), "--address".into(), "127.0.0.1:{port}".into(),
            "--console-address".into(), "127.0.0.1:{port+1}".into()]) { return false; }
    let mut args = r.spec.args[6..].iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--certs-dir" | "-S" | "--config-dir" | "-C" => if args.next().is_none() { return false; },
            "--quiet" | "--anonymous" | "--json" => {},
            value if value.starts_with("--certs-dir=") || value.starts_with("--config-dir=") => {},
            _ => return false,
        }
    }
    true
}

fn minio_settings(r: &Resolved) -> Result<MinioSettings> {
    let key = |value: &str| if cfg!(windows) { value.to_ascii_uppercase() } else { value.to_owned() };
    let mut env: std::collections::HashMap<_, _> = std::env::vars_os().filter_map(|(name, value)|
        Some((key(&name.into_string().ok()?), value.into_string().ok()?))).collect();
    let mut names = std::collections::HashSet::new();
    for (name, value) in r.spec.env.iter().flatten() {
        let name = key(name);
        if !names.insert(name.clone()) { return Err(AppError::new("MINIO_ENV_INVALID", "MinIO 运行配置包含重复的大小写环境变量名，请合并后重试")); }
        env.insert(name, expand(value, r));
    }
    let cwd = r.spec.cwd.as_ref().map(|path| PathBuf::from(expand(path, r))).unwrap_or_else(|| r.root.clone());
    // CLI 的 YAML 配置环境默认值早于 MINIO_CONFIG_ENV_FILE 读取。
    if let Some(config) = env.get("MINIO_CONFIG").filter(|value| !value.is_empty()) {
        let file = cwd.join(config); let metadata = std::fs::metadata(&file)?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 { return Err(AppError::new("MINIO_CONFIG_INVALID", "MinIO YAML 配置必须是小于 1 MiB 的文本文件")); }
        let config: serde_json::Value = yaml_serde::from_str(&std::fs::read_to_string(&file)?)
            .map_err(|_| AppError::new("MINIO_CONFIG_INVALID", "MinIO YAML 配置无法解析，请检查后重试"))?;
        for (name, expected) in [("address", expand("127.0.0.1:{port}", r)), ("console-address", expand("127.0.0.1:{port+1}", r))] {
            if config.get(name).is_some_and(|value| value.as_str().is_none_or(|value| !value.is_empty() && value != expected)) {
                return Err(AppError::new("MINIO_PORT_CONFIG", "MinIO YAML 配置覆盖了托管监听地址或端口")
                    .with_hint("请使 YAML 的 address 和 console-address 与服务端口一致，或移除这两项以使用托管参数；原配置未修改。"));
            }
        }
    }
    // 环境文件按逐行 KEY=value 解析，最后一次赋值覆盖；值为字面量，不执行 shell 插值。
    if let Some(file) = env.get("MINIO_CONFIG_ENV_FILE").filter(|value| !value.is_empty()) {
        let file = cwd.join(file.trim());
        match std::fs::metadata(&file) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(error.into()),
            Ok(metadata) => {
                let invalid = |line| AppError::new("MINIO_ENV_INVALID", "MinIO 环境文件无法完整解析，未启动服务")
                    .with_hint(format!("请检查 {} 第 {} 行的赋值或编码；原文件已保留。", file.display(), line));
                if !metadata.is_file() || metadata.len() > 1024 * 1024 { return Err(invalid(1)); }
                let content = std::fs::read_to_string(&file).map_err(|_| invalid(1))?;
                for (index, line) in content.split('\n').enumerate() {
                    if line.len() >= 65536 { return Err(invalid(index + 1)); }
                    let line = line.trim(); if line.is_empty() || line.starts_with('#') { continue; }
                    let line = line.strip_prefix("export").unwrap_or(line).trim();
                    let (name, value) = line.split_once('=').ok_or_else(|| invalid(index + 1))?;
                    let value = if value.len() >= 2 && ((value.starts_with('"') && value.ends_with('"')) || (value.starts_with('\'') && value.ends_with('\''))) {
                        &value[1..value.len()-1]
                    } else { value };
                    if name.is_empty() || name.contains('\0') || value.contains('\0') { return Err(invalid(index + 1)); }
                    env.insert(key(name), value.to_owned());
                }
            },
        }
    }
    let value = |name: &str, fallback: &str| env.get(name).filter(|value| !value.is_empty()).map(String::as_str).unwrap_or(fallback).trim().to_owned();
    let browser = match value("MINIO_BROWSER", "on").as_str() {
        "1" | "t" | "T" | "true" | "TRUE" | "True" | "on" | "ON" | "On" => true,
        "0" | "f" | "F" | "false" | "FALSE" | "False" | "off" | "OFF" | "Off" => false,
        other if other.eq_ignore_ascii_case("enabled") => true,
        other if other.eq_ignore_ascii_case("disabled") => false,
        _ => return Err(AppError::new("MINIO_ENV_INVALID", "MINIO_BROWSER 开关值无效，请使用 on 或 off")),
    };
    let subpath = value("CONSOLE_SUBPATH", "/");
    let mut destination = None;
    let redirect = value("MINIO_BROWSER_REDIRECT_URL", "");
    if browser && !redirect.is_empty() {
        let invalid = || AppError::new("MINIO_REDIRECT_INVALID", "MinIO 管理台重定向地址无效，请检查 MINIO_BROWSER_REDIRECT_URL");
        let url = reqwest::Url::parse(&redirect).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some()
            || url.query().is_some() || url.fragment().is_some() { return Err(invalid()); }
        // 子路径由代理剥离；直接访问本机相同路径只会返回 SPA HTML，脚本和登录 API 无法工作。
        destination = Some(url.to_string());
    }
    Ok(MinioSettings { browser, subpath, destination })
}

fn minio_web_target(r: &Resolved, settings: &MinioSettings) -> Result<String> {
    if !settings.browser { return Err(web_unavailable("MinIO 管理台已关闭；S3 服务可继续使用，如需管理台请启用 MINIO_BROWSER 后重启。")); }
    if let Some(destination) = &settings.destination { return Ok(destination.clone()); }
    if !settings.subpath.trim_matches('/').is_empty() { return Err(web_unavailable("MinIO 控制台子路径需要反向代理；请配置 MINIO_BROWSER_REDIRECT_URL 为完整访问地址后重启。")); }
    let port = r.port.and_then(|port| port.checked_add(1)).ok_or_else(|| web_unavailable("MinIO 管理台端口超出范围。"))?;
    local_web_url("127.0.0.1", port, false, "/")
}

/// 将服务配置里的监听地址转换成可打开的 URL 主机部分。
/// 此处只校验格式，不在服务启动的生命周期锁内等待 DNS；打开时再核实监听归属。
fn local_web_host(raw: &str) -> Result<String> {
    if raw.is_empty() || raw == "0.0.0.0" {
        return Ok("127.0.0.1".into());
    }
    if raw == "::" || raw == "[::]" {
        return Ok("[::1]".into());
    }
    if raw.chars().any(|c| {
        c.is_control() || c.is_whitespace() || matches!(c, '/' | '\\' | '@' | '?' | '#' | '%')
    }) {
        return Err(web_unavailable(
            "管理台监听地址包含无效字符，请检查服务配置。",
        ));
    }
    let unbracketed = raw
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(raw);
    if let Ok(ip) = unbracketed.parse::<std::net::IpAddr>() {
        if ip.is_multicast() {
            return Err(web_unavailable("管理台不能使用组播地址。"));
        }
        return Ok(match ip {
            std::net::IpAddr::V4(ip) if ip.is_unspecified() => "127.0.0.1".into(),
            std::net::IpAddr::V4(ip) => ip.to_string(),
            std::net::IpAddr::V6(ip) if ip.is_unspecified() => "[::1]".into(),
            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        });
    }
    if raw.contains(['[', ']', ':']) {
        return Err(web_unavailable("管理台监听地址不是有效的本地主机名或 IP。"));
    }
    // 与浏览器使用同一套 URL/IDNA 规范化；保留主机名供 Host/SNI 和证书校验使用。
    let url = reqwest::Url::parse(&format!("http://{raw}/"))
        .map_err(|_| web_unavailable("管理台监听主机名无效。"))?;
    let host = url
        .host_str()
        .ok_or_else(|| web_unavailable("管理台监听主机名无效。"))?;
    if host.parse::<std::net::IpAddr>().is_ok()
        || host.len() > 254
        || host.trim_end_matches('.').split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        })
    {
        return Err(web_unavailable(
            "管理台监听主机名无效，请使用完整 IP 或本地主机名。",
        ));
    }
    Ok(host.to_string())
}

/// 限时解析主机名；解析本身不向目标发送 HTTP，也不把任意解析结果视为本机。
fn resolve_web_host(host: &str, port: u16) -> Result<Vec<std::net::SocketAddr>> {
    if port == 0 {
        return Err(web_unavailable("管理台端口未启用。"));
    }
    let raw = host.trim_matches(['[', ']']);
    let mut addresses = if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
        vec![std::net::SocketAddr::new(ip, port)]
    } else {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .map_err(|_| web_unavailable("无法创建管理台主机名解析器。"))?;
        let result = runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_millis(1500),
                tokio::net::lookup_host((raw, port)),
            )
            .await
            .map_err(|_| web_unavailable("管理台主机名解析超时，请检查本机 DNS 或 hosts 后重试。"))?
            .map(|addresses| addresses.collect::<Vec<_>>())
            .map_err(|_| web_unavailable("管理台主机名无法解析，请检查服务配置或本机 hosts。"))
        });
        // 系统 DNS 可能仍在阻塞线程执行，不能让 runtime 的析构无限等待。
        runtime.shutdown_background();
        result?
    };
    addresses.retain(|address| !address.ip().is_unspecified() && !address.ip().is_multicast());
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(web_unavailable("管理台没有可核实的本机监听地址。"));
    }
    Ok(addresses)
}

fn local_web_url(address: &str, port: u16, https: bool, path: &str) -> Result<String> {
    if port == 0 {
        return Err(web_unavailable("管理台端口未启用。"));
    }
    let host = local_web_host(address)?;
    let mut url = reqwest::Url::parse(&format!(
        "{}://{host}:{port}/",
        if https { "https" } else { "http" }
    ))
    .map_err(|_| web_unavailable("管理台地址无效，请检查服务配置。"))?;
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    url.path_segments_mut()
        .map_err(|_| web_unavailable("管理台路径无效。"))?
        .clear()
        .extend(segments);
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
    if managed_minio(r) { return minio_web_target(r, &minio_settings(r)?); }
    if crate::install::official_qdrant(&r.entry) && r.entry.run.as_ref().is_some_and(|run| run.args == r.spec.args) { return qdrant_web_target(r); }
    let args_pair = |flag: &str, value: &str| r.spec.args.windows(2).any(|pair| pair[0] == flag && pair[1] == value);
    let env_pair = |name: &str, value: &str| r.spec.env.as_ref().and_then(|env| env.get(name)).is_some_and(|current| current == value);
    let (offset, path) = match r.entry.id.as_str() {
        "mailpit" if args_pair("--listen", "127.0.0.1:{port}") => (0, "/"),
        // RustFS 的官方 server 描述明确托管了对象端口和控制台端口；只有两组
        // 参数都仍由清单提供时才生成入口，避免把用户自定义监听地址猜成控制台。
        "rustfs"
            if args_pair("--address", ":{port}")
                && args_pair("--console-address", ":{port+1}")
                && r.spec.args.iter().any(|arg| arg == "--console-enable") =>
        {
            (1, "/")
        }
        // ZincSearch 的清单运行描述直接托管 Web UI 与数据目录；不满足完整描述时
        // 不猜测端口，避免把用户自定义 API 端口误当成管理台。
        "zincsearch"
            if r.spec.args.is_empty()
                && env_pair("ZINC_SERVER_PORT", "{port}")
                && env_pair("ZINC_DATA_PATH", "{data}") =>
        {
            (0, "/")
        }
        // Temporal CLI 的开发服务把 Web UI 固定放在 gRPC 端口 + 1000；
        // 只有同时确认官方启动参数仍由清单托管时才提供快捷入口。
        "temporal-cli"
            if args_pair("--port", "{port}") && args_pair("--ui-port", "{port+1000}") =>
        {
            (1000, "/")
        }
        // Neo4j Community 的 console 模式在默认 HTTP 端口提供内置 Browser。
        "neo4j" if r.spec.args == ["console"] && r.port == Some(7474) => (0, "/browser"),
        // RabbitMQ 管理插件由首次启动初始化命令启用；配置文件把 HTTP 管理台
        // 固定在 AMQP 端口 + 10000，避免 safe 端口档位与其它实例冲突。
        "rabbitmq" if managed_rabbitmq(&r.entry, &r.spec) => (10000, "/"),
        "consul" if args_pair("-http-port", "{port}") && args_pair("-client", "127.0.0.1") => (0, "/ui/"),
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
        let values: Vec<_> = entry
            .run
            .as_ref()
            .and_then(|run| run.env.as_ref())
            .into_iter()
            .flatten()
            .filter(|(name, _)| {
                if cfg!(windows) {
                    name.eq_ignore_ascii_case(key)
                } else {
                    name.as_str() == key
                }
            })
            .map(|(_, value)| value)
            .collect();
        if values.len() > 1 {
            return Err(AppError::new(
                "QDRANT_CONFIG_ENV",
                "Qdrant 环境变量存在重复的大小写名称，请合并后重试",
            ));
        }
        Ok(values
            .first()
            .map(|value| {
                value
                    .replace("{root}", &crate::paths::portable_path_text(root))
                    .replace("{data}", &crate::paths::portable_path_text(&data))
                    .replace("{etc}", &crate::paths::portable_path_text(&etc))
            })
            .or_else(|| std::env::var(key).ok()))
    };
    if let Some(value) = env_value(key)? { return Ok(value); }
    let read = |path: &std::path::Path| -> Result<Option<String>> {
        let relative = path
            .strip_prefix(&paths.base)
            .map_err(|_| AppError::new("QDRANT_CONFIG_PATH", "Qdrant 配置不在托管目录内"))?;
        let path = crate::paths::checked_data_path(
            &paths.base,
            &crate::paths::portable_path_text(relative),
        )?;
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
    let text = crate::paths::portable_path_text(&path);
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
        return Ok(vec![("QDRANT__STORAGE__SNAPSHOTS_PATH".into(), crate::paths::portable_path_text(&target))]);
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
        let relative = source
            .strip_prefix(&paths.base)
            .map_err(|_| AppError::new("QDRANT_SNAPSHOT_PATH", "快照路径超出托管目录"))?;
        let source = crate::paths::checked_data_path(
            &paths.base,
            &crate::paths::portable_path_text(relative),
        )?;
        if source.exists() && std::fs::read_dir(&source)?.next().transpose()?.is_some() {
            sources.push((installed.version, source));
        }
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
            let key = crate::paths::portable_path_text(path);
            let folded = if cfg!(windows) {
                key.to_lowercase()
            } else {
                key.clone()
            };
            if plan.contains_key(&folded) {
                return Err(conflict(&key));
            }
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
        crate::paths::checked_data_path(
            &paths.base,
            &crate::paths::portable_path_text(
                source
                    .strip_prefix(&paths.base)
                    .map_err(|_| AppError::new("QDRANT_SNAPSHOT_PATH", "快照路径超出托管目录"))?,
            ),
        )?;
        let backup = tempfile::Builder::new()
            .prefix(&format!("qdrant-snapshots-{version}-"))
            .tempdir_in(&backups)?;
        std::fs::rename(&source, backup.path().join("snapshots"))
            .map_err(|e| AppError::io("备份旧版 Qdrant 快照目录", e))?;
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
    let destination = reqwest::Url::parse(&original.url).map_err(|_| web_unavailable("本次启动的管理台地址无效，请重启服务。"))?;
    if !matches!(destination.scheme(), "http" | "https") || destination.host_str().is_none() || !destination.username().is_empty() || destination.password().is_some() {
        return Err(web_unavailable("管理台地址必须是无内嵌凭据的 HTTP/HTTPS 地址。"));
    }
    let mut url = reqwest::Url::parse(original.probe.as_deref().unwrap_or(&original.url))
        .map_err(|_| web_unavailable("本次启动的管理台探测地址无效，请重启服务。"))?;
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
        return Err(web_unavailable("管理台快捷入口仅支持本机 HTTP/HTTPS 监听地址。"));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| web_unavailable("管理台端口无效。"))?;
    let host = url
        .host_str()
        .ok_or_else(|| web_unavailable("管理台主机无效。"))?
        .to_string();
    let resolved = resolve_web_host(&host, port)?;
    let listeners = crate::ports::listener_endpoints()?;
    let target = resolved
        .iter()
        .copied()
        .find(|target| {
            listeners
                .iter()
                .any(|entry| entry.accepts(*target) && before.pids.contains(&entry.pid))
        })
        .ok_or_else(|| {
            web_unavailable("管理台主机名或 IP 未指向当前服务的本机监听地址，请检查配置。")
        })?;
    let owned = || -> Result<bool> {
        let listeners = crate::ports::listener_endpoints()?;
        Ok(listeners
            .iter()
            .any(|entry| entry.accepts(target) && before.pids.contains(&entry.pid))
            && listeners
                .iter()
                .filter(|entry| entry.port == port)
                .all(|entry| before.pids.contains(&entry.pid)))
    };
    if !owned()? { return Err(web_unavailable("管理台端口尚未就绪或已由其他进程占用，请查看服务日志。")); }
    // 仅探测本机受管进程，不携带凭据、不跟随跳转；自签证书仍由浏览器正常提示。
    let mut client_builder = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_millis(1500));
    if url.domain().is_some() {
        // 固定到刚核实的监听地址，保留请求的 Host/SNI，不让第二次 DNS 查询换到远端。
        client_builder = client_builder.resolve(&host, target);
    }
    let client = client_builder
        .build()
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
    reachable.map(|reachable| if original.probe.is_some() { original.url } else { reachable })
        .ok_or_else(|| web_unavailable("管理台未返回可访问的网页；请检查是否启用了 Web 界面、自定义路径或访问限制。"))
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
    if managed_minio(r) && !minio_settings(r)?.browser { offsets.remove(&1); }
    let ports = |base: u16| -> Option<Vec<u16>> {
        offsets.iter().map(|offset| i32::from(base).checked_add(*offset)
            .and_then(|value| u16::try_from(value).ok()).filter(|p| *p > 0)).collect()
    };
    let original = ports(desired).ok_or_else(|| AppError::new("BAD_PORT", "主端口及派生端口必须位于 1–65535，请调整服务端口"))?;
    // Caddy 的本机模式只占用回环地址；局域网 IP 的同号端口不能阻止失败后的本机配置恢复。
    // 未识别的地址仍视为冲突，实际配置与监听归属继续由启动流程验证。
    let listeners = if r.entry.id == "caddy" && crate::webnetwork::mode(store, "caddy")? != Some(true) {
        crate::ports::listener_endpoints()?.into_iter().filter(|entry| entry.address.is_none()
            || entry.accepts(([127, 0, 0, 1], entry.port).into()))
            .map(|entry| (entry.port, entry.pid)).collect()
    } else { crate::ports::listeners()? };
    let caddy_tls = if r.entry.id == "caddy" { Some(crate::caddy::https_port(store)?) } else { None };
    let available = |base| ports(base).is_some_and(|ports| ports.into_iter().all(|port|
        Some(port) != caddy_tls && !listeners.iter().any(|(bound, _)| *bound == port) && tcp_port_bindable(port)
            && (!needs_udp(r, base, port) || std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok())));
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
        if let Some((_, pid)) = listeners.iter().find(|(bound, _)| *bound == port) {
            return Err(AppError::port_conflict(port, crate::ports::process_name(*pid).as_deref()).with_pid(*pid));
        }
        precheck_port(port, &r.entry.display_name)?;
        if needs_udp(r, desired, port) {
            std::net::UdpSocket::bind(("127.0.0.1", port))
                .map_err(|e| AppError::port_conflict(port, Some("UDP 端口不可用")).with_detail(e.to_string()))?;
        }
    }
    Ok(Some(desired))
}

fn config_line_scopes(content: &str, r: &Resolved) -> Vec<String> {
    let file = r
        .spec
        .config_file
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    let ini = file.ends_with(".ini") || file.ends_with(".cnf");
    let yaml = file.ends_with(".yaml") || file.ends_with(".yml");
    let mut section = String::new();
    let mut parents: Vec<(usize, String)> = Vec::new();
    content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if ini {
                if let Some((name, _)) = trimmed.strip_prefix('[').and_then(|s| s.split_once(']')) {
                    section = name.trim().to_ascii_lowercase();
                }
            }
            if yaml && !trimmed.is_empty() && !trimmed.starts_with('#') {
                let indent = line.len() - line.trim_start().len();
                while parents.last().is_some_and(|(depth, _)| *depth >= indent) {
                    parents.pop();
                }
                section = parents
                    .iter()
                    .map(|(_, key)| key.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                if let Some((key, value)) = trimmed.split_once(':') {
                    if (value.trim().is_empty() || value.trim().starts_with('#'))
                        && key
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    {
                        parents.push((indent, key.to_string()));
                    }
                }
            }
            section.clone()
        })
        .collect()
}

/// 只替换模板声明的端口数字，保留其余自定义行、缩进、注释和换行符。
/// INI 按节、YAML 按父级路径匹配；缺失或多义时停止，不能覆盖其他配置段的同名项。
fn sync_config_ports(current: &str, template: &str, r: &Resolved) -> Result<String> {
    let mut lines: Vec<String> = current.split_inclusive('\n').map(str::to_owned).collect();
    let current_scopes = config_line_scopes(current, r);
    let template_scopes = config_line_scopes(template, r);
    for (line_index, line) in template.lines().enumerate() {
        let line = line.trim();
        let tokens: Vec<_> = PORT_TOKEN.captures_iter(line).collect();
        if tokens.is_empty() {
            continue;
        }
        let literal = |text: &str| {
            regex::escape(&expand(text, r))
                .replace(' ', "[ \\t]*")
                .replace('\t', "[ \\t]*")
                .replace('=', "[ \\t]*=[ \\t]*")
        };
        let mut pattern = String::from(r"^[ \t]*");
        let mut end = 0;
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
    content
        .split_inclusive('\n')
        .map(|line| {
            let body = line.trim_end_matches(['\r', '\n']);
            for (key, suffix) in [
                ("RNACOS_DATA_DIR", "nacos_db"),
                ("RNACOS_CONFIG_DB_FILE", "nacos_db/config.db"),
                ("RNACOS_NAMING_DB_FILE", "nacos_db/naming.db"),
            ] {
                let value = expand(&format!("{{data}}/{suffix}"), r);
                if body == format!("{key}={value}") || body == format!("{key}=\"{value}\"") {
                    let escaped = value
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('$', "\\$");
                    return format!("{key}=\"{escaped}\"{}", &line[body.len()..]);
                }
            }
            line.to_string()
        })
        .collect()
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

/// 只修正与旧模板完整匹配的路径行；自定义路径、额外参数和注释保持原样。
fn sync_template_paths(current: &str, template: &str, r: &Resolved) -> Result<String> {
    let mut corrections = Vec::new();
    let current_scopes = config_line_scopes(current, r);
    let template_scopes = config_line_scopes(template, r);
    for (index, line) in template
        .lines()
        .enumerate()
        .filter(|(_, line)| PATH_TOKEN.is_match(line) && !line.trim_start().starts_with(['#', ';']))
    {
        let old = expand(line, r);
        let scope = &template_scopes[index];
        if current
            .lines()
            .enumerate()
            .any(|(index, line)| &current_scopes[index] == scope && line.trim() == old.trim())
        {
            let corrected = expand_config(line, r)?;
            if corrected.trim() != old.trim() {
                corrections.push((scope, old.trim().to_string(), corrected.trim().to_string()));
            }
        }
    }
    Ok(current
        .split_inclusive('\n')
        .enumerate()
        .map(|(index, line)| {
            let Some((_, _, corrected)) = corrections
                .iter()
                .find(|(scope, old, _)| *scope == &current_scopes[index] && line.trim() == old)
            else {
                return line.to_string();
            };
            let start = line.len() - line.trim_start().len();
            let end = line.trim_end().len();
            let corrected = if line.ends_with("\r\n") {
                corrected.replace('\n', "\r\n")
            } else {
                corrected.clone()
            };
            format!("{}{corrected}{}", &line[..start], &line[end..])
        })
        .collect())
}

fn prepare_config(paths: &Paths, r: &Resolved) -> Result<Vec<(String, String)>> {
    let mut env = Vec::new();
    if let (Some(cf), Some(tpl)) = (&r.spec.config_file, &r.spec.config_template) {
        let path = r.etc.join(cf);
        let relative = path
            .strip_prefix(&paths.base)
            .map_err(|_| AppError::new("CONFIG_PATH", "配置路径必须位于数据目录内"))?;
        let path = crate::paths::checked_data_path(
            &paths.base,
            &crate::paths::portable_path_text(relative),
        )?;
        let previous = match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() && meta.len() <= 1024 * 1024 => Some(std::fs::read_to_string(&path)?),
            Ok(_) => return Err(AppError::new("CONFIG_READ", "配置必须是小于 1 MiB 的文本文件")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let mut content = match &previous {
            Some(current) => sync_template_paths(&sync_config_ports(current, tpl, r)?, tpl, r)?,
            None => expand_config(tpl, r)?,
        };
        if r.entry.id == "mariadb" {
            // resolve 已确认仅为托管路径；同步旧空目录配置，避免下次启动仍指向共享目录。
            let mut section = String::new();
            content = content
                .split_inclusive('\n')
                .map(|line| {
                    let trimmed = line.trim();
                    if trimmed.starts_with('[') && trimmed.ends_with(']') {
                        section = trimmed[1..trimmed.len() - 1].to_ascii_lowercase();
                    }
                    if ["mysqld", "server", "mariadb", "mariadbd"].contains(&section.as_str())
                        && trimmed
                            .split_once('=')
                            .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case("datadir"))
                    {
                        format!(
                            "datadir=\"{}\"{}",
                            crate::paths::quoted_config_path(&r.data),
                            if line.ends_with("\r\n") {
                                "\r\n"
                            } else if line.ends_with('\n') {
                                "\n"
                            } else {
                                ""
                            }
                        )
                    } else {
                        line.to_string()
                    }
                })
                .collect();
        }
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

/// 从本次进程实际建立的监听生成入口，遵循上游 .env/继承环境的最终结果。
/// SDK 和控制台可以使用不同地址；不以端口号猜测 127.0.0.1，也不解析远端主机名。
fn rnacos_http_target(manager: &ServiceManager, r: &Resolved, offset: u16, path: &str) -> Result<String> {
    if !managed_rnacos(r) { return Err(web_unavailable("当前 r-nacos 运行配置没有受管的 HTTP 入口。")); }
    let port = r.port.and_then(|port| port.checked_add(offset)).ok_or_else(|| web_unavailable("r-nacos 端口超出范围。"))?;
    let pids = manager.snapshot(&r.service_id).map(|service| service.pids).unwrap_or_default();
    if pids.is_empty() || !pids.iter().any(|pid| platform::process_alive(*pid)) {
        return Err(web_unavailable("r-nacos 进程未运行，请启动后重试。"));
    }
    let listeners = crate::ports::listener_endpoints()?;
    if listeners.iter().any(|entry| entry.port == port && !pids.contains(&entry.pid)) {
        return Err(web_unavailable("r-nacos 端口由其他进程占用，请检查服务日志。"));
    }
    let address = listeners.iter().filter(|entry| entry.port == port && pids.contains(&entry.pid))
        .filter_map(|entry| entry.probe_address())
        .filter(|address| !address.ip().is_multicast() && !matches!(address, std::net::SocketAddr::V6(ip) if ip.scope_id() != 0))
        .min_by_key(|address| (!address.ip().is_loopback(), address.is_ipv6(), *address))
        .ok_or_else(|| web_unavailable("r-nacos 尚无可访问的本机 HTTP 监听地址；带区域标识的 IPv6 地址暂不支持。"))?;
    local_web_url(&address.ip().to_string(), port, false, path)
}

fn owned_ports_ready(manager: &ServiceManager, service_id: &str, ports: &[u16]) -> bool {
    let pids = manager.snapshot(service_id).map(|s| s.pids).unwrap_or_default();
    if pids.is_empty() || !pids.iter().any(|pid| platform::process_alive(*pid)) { return false; }
    if service_id == "caddy" {
        return ports.iter().all(|port| {
            let target = ([127, 0, 0, 1], *port).into();
            crate::ports::owns_listener(target, &pids).unwrap_or(false)
                && std::net::TcpStream::connect_timeout(&target, Duration::from_millis(200)).is_ok()
        });
    }
    crate::ports::listener_endpoints().is_ok_and(|listeners| ports.iter().all(|port|
        listeners.iter().filter(|entry| entry.port == *port).all(|entry| pids.contains(&entry.pid))
            && listeners.iter().filter(|entry| entry.port == *port && pids.contains(&entry.pid))
                .filter_map(|entry| entry.probe_address()).any(|address|
                    std::net::TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok())))
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

fn wait_minio_healthy(manager: &ServiceManager, r: &Resolved, settings: &MinioSettings, timeout: Duration) -> bool {
    let Some(port) = r.port else { return false; };
    let mut ports = vec![port];
    if settings.browser { let Some(console) = port.checked_add(1) else { return false; }; ports.push(console); }
    // 仅探测已确认归属本次进程的回环端口，不携带凭据；兼容用户已有的本地自签证书。
    let Ok(client) = reqwest::blocking::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .danger_accept_invalid_certs(true).timeout(Duration::from_millis(700)).build() else { return false; };
    let deadline = std::time::Instant::now() + timeout;
    let mut ready_since = None; let mut last_scheme = "http";
    while std::time::Instant::now() < deadline {
        if manager.snapshot(&r.service_id).is_none_or(|service| service.pids.iter().all(|pid| !platform::process_alive(*pid))) { return false; }
        let mut ready = false;
        if owned_ports_ready(manager, &r.service_id, &ports) {
            for scheme in [last_scheme, if last_scheme == "http" { "https" } else { "http" }] {
                if client.get(format!("{scheme}://127.0.0.1:{port}/minio/health/ready")).send().is_ok_and(|response| response.status().is_success()) {
                    last_scheme = scheme; ready = true; break;
                }
            }
        }
        if ready {
            if ready_since.get_or_insert_with(std::time::Instant::now).elapsed() >= Duration::from_millis(300) { return true; }
        } else { ready_since = None; }
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

fn rnacos_panic_error(manager: &ServiceManager, r: &Resolved) -> AppError {
    let known_init_failure = cfg!(windows) && matches!(r.entry.version.as_str(), "0.8.6" | "0.8.7")
        && manager.services.lock().get(&r.service_id).is_some_and(|entry| entry.ring.lock().iter().rev()
            .take_while(|line| line.as_str() != RNACOS_START_MARKER)
            .find(|line| line.contains("panicked at "))
            .is_some_and(|line| {
                let normalized = line.replace('\\', "/");
                (421..=424).any(|line| normalized.contains(&format!("src/raft/filestore/raftapply.rs:{line}:")))
            }));
    if known_init_failure {
        // 这两个官方版本在依赖注入前启动 Raft，实例元数据初始化的异步 I/O 可触发竞态。
        // 关闭持久化会改变注册中心语义，只提供经验证的选项，不自动改配置或清空数据。
        AppError::new("SERVICE_RUNTIME_PANIC", "r-nacos 的 Raft 初始化失败，已停止服务并保留配置和数据")
            .with_hint(format!("若不需要注册实例元数据持久化，可在 {} 中显式设置 RNACOS_NAMING_INSTANCE_METADATA_PERSISTENCE_ENABLE=false 后重试；这会停止加载和保存注册实例元数据，配置中心的配置仍可持久化。此选项不能修复已异常的 Raft 数据。需要实例元数据持久化时，请保留原配置并等待上游修复。", r.etc.join(".env").display()))
    } else {
        AppError::new("SERVICE_RUNTIME_PANIC", "r-nacos 内部线程启动失败，已停止服务")
            .with_hint("请查看服务日志中的 panic 原因，或切换其他已安装版本；仅 HTTP 端口可连接不能证明配置中心可用。")
    }
}

fn rnacos_raft_metrics_ready(metrics: &serde_json::Value) -> bool {
    let Some(id) = metrics["id"].as_u64().filter(|id| *id > 0) else { return false; };
    let Some(leader) = metrics["current_leader"].as_u64().filter(|id| *id > 0) else { return false; };
    metrics["last_applied"].as_u64().is_some_and(|index| index > 0)
        && match metrics["state"].as_str() {
            Some("Leader") => leader == id,
            Some("Follower") => leader != id,
            _ => false,
        }
}

fn wait_rnacos_healthy(manager: &ServiceManager, r: &Resolved, timeout: Duration) -> bool {
    let Ok(client) = reqwest::blocking::Client::builder().no_proxy()
        .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_millis(500)).build() else { return false; };
    let deadline = std::time::Instant::now() + timeout;
    let mut ready_since = None;
    let mut first_health = None;
    while std::time::Instant::now() < deadline {
        if rnacos_startup_panicked(manager, &r.service_id) { return false; }
        if manager.snapshot(&r.service_id).is_none_or(|service| service.pids.iter().all(|pid| !platform::process_alive(*pid))) { return false; }
        let origin = if rnacos_ports_ready(manager, r) { rnacos_http_target(manager, r, 0, "/").ok() } else { None };
        let healthy = origin.as_ref().is_some_and(|origin| client.get(format!("{origin}health"))
            .send().is_ok_and(|response| response.status().is_success()
                && response.text().is_ok_and(|text| text.trim() == "success")));
        let ready = healthy && {
            let health_since = first_health.get_or_insert_with(std::time::Instant::now);
            client.get(format!("{}nacos/v1/raft/metrics", origin.as_deref().unwrap())).send().is_ok_and(|response| {
                match response.status().as_u16() {
                    200 => response.json::<serde_json::Value>().is_ok_and(|metrics| rnacos_raft_metrics_ready(&metrics)),
                    // 开启鉴权时不尝试登录或关闭鉴权。上游 /health 初始有 12.5 秒宽限，
                    // 须在宽限结束后再次确认，不能把尚无 leader 的节点当作可用服务。
                    401 | 403 | 404 => health_since.elapsed() >= Duration::from_secs(13),
                    _ => false,
                }
            })
        };
        if ready {
            // 留出启动日志汇入的时间；上游工作线程 panic 后 HTTP 线程仍可能正常应答。
            if ready_since.get_or_insert_with(std::time::Instant::now).elapsed() >= Duration::from_millis(500) {
                return !rnacos_startup_panicked(manager, &r.service_id) && rnacos_ports_ready(manager, r)
                    && rnacos_http_target(manager, r, 0, "/").ok() == origin;
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

    // 先确定最终端口再生成配置、初始化和展开命令，所有阶段使用同一组端口。
    let planned = r.port;
    r.port = select_port(store, &r)?;
    let minio = if managed_minio(&r) { Some(minio_settings(&r)?) } else { None };
    if minio.is_some() {
        let relative = r.data.join("data").strip_prefix(&paths.base).map_err(|_| AppError::new("MINIO_DATA_PATH", "MinIO 数据目录不在托管路径内"))?.to_path_buf();
        crate::paths::checked_data_path(&paths.base, &crate::paths::portable_path_text(&relative))?;
    }
    prepare_config(paths, &r)?;
    let caddy_snapshot = if r.entry.id == "caddy" {
        crate::caddy::prepare(paths, store, &r)?;
        Some(crate::sites::snapshot_endpoints(paths, store, "caddy"))
    } else { None };
    let qdrant_env = qdrant_snapshot_env(store, paths, manager, &r)?;
    let sftpgo = if managed_sftpgo(&r.entry, &r.spec) { Some(prepare_sftpgo(store, paths, &r)?) } else { None };
    let mut web_target = generic_web_target(&r, sftpgo.as_ref());
    // CoreDNS 特例：Corefile 每次启动都重写——TLD 设置或转发策略变化要自动跟上，
    // 且通配解析模板含 {{ .Name }} 占位符，不能走通用模板渲染
    if r.entry.id == "coredns" {
        let tld = store
            .get_setting_checked("defaultTld")?
            .unwrap_or_else(|| "test".into());
        crate::dns::write_corefile(paths, &r.etc.join("Corefile"), &tld, &[])?;
    }

    // 启动前自建的数据子目录（如 Temurin/Qdrant 的 storage、RabbitMQ 的 mnesia）
    for d in r.spec.init_dirs.iter().filter(|_| r.entry.id != "mariadb") {
        std::fs::create_dir_all(r.data.join(d))?;
    }

    // 一次性初始化（MariaDB 的 install-db、Neo4j 的 set-initial-password 等）
    if r.entry.id == "mariadb" { initialize_mariadb(&r)?; } else { run_init_if_needed(&r)?; }

    if let Some(port) = r.port { store.save_generic_port(&r.service_id, port, planned != r.port)?; }

    let mut args: Vec<String> = r.spec.args.iter().map(|a| expand(a, &r)).collect();
    if r.entry.id == "memcached" {
        args = crate::memcached_settings::apply_args(store, &r.entry.version, args)?;
    }
    if r.entry.id == "mariadb" {
        args.extend([
            format!("--basedir={}", crate::paths::portable_path_text(r.root.parent().ok_or_else(|| AppError::new("MARIADB_PATH", "MariaDB 安装路径无效"))?)),
            format!("--datadir={}", crate::paths::portable_path_text(&r.data)),
            format!("--port={}", r.port.ok_or_else(|| AppError::new("DATABASE_PORT_UNKNOWN", "MariaDB 端口未配置"))?),
            "--bind-address=127.0.0.1".into(),
        ]);
        if cfg!(windows) { args.push("--console".into()); }
    }
    if let Some(config) = &sftpgo { args.extend(["--config-file".into(), crate::paths::portable_path_text(&config.file)]); }
    let cwd = r
        .spec
        .cwd
        .as_ref()
        .map(|c| PathBuf::from(expand(c, &r)))
        .unwrap_or_else(|| r.root.clone());
    let mut env: Vec<(String, String)> = r
        .spec
        .env
        .iter()
        .flatten()
        .map(|(k, v)| (k.clone(), expand(v, &r)))
        .collect();
    // 已解析的默认值最后应用，避免显式空连接地址覆盖必要的 SQLite URI 编码。
    if let Some(config) = &sftpgo {
        env.extend(config.env.clone());
    }
    env.extend(qdrant_env);

    // .bat/.cmd 不是可执行文件：Windows 上须经 cmd.exe 转发（Tomcat/Neo4j/MariaDB 等）
    let (program, args) = if cfg!(windows) && is_script(&r.bin) {
        let mut cmd = crate::paths::portable_path_text(&r.bin);
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
    let healthy = if r.entry.id == "caddy" {
        let mut ports = vec![r.port.unwrap_or(0)];
        if store.list_sites()?.iter().any(|site| site.runtime.web_server == "caddy" && site.https && crate::sites::derive_status(paths, site) == "running") {
            ports.push(crate::caddy::https_port(store)?);
        }
        wait_owned_ports(manager, &r.service_id, &ports, timeout)
    } else if r.entry.id == "mariadb" {
        r.port.is_some_and(|port| wait_owned_ports(manager, &r.service_id, &[port], timeout))
    } else if sftpgo.is_some() {
        wait_sftpgo_healthy(manager, &r, timeout)
    } else if crate::install::official_qdrant(&r.entry) {
        r.port.and_then(|port| port.checked_add(1).map(|grpc| [port, grpc]))
            .is_some_and(|ports| wait_owned_ports(manager, &r.service_id, &ports, timeout))
    } else if managed_rabbitmq(&r.entry, &r.spec) {
        r.port.and_then(|port| port.checked_add(10000).map(|management| [port, management]))
            .is_some_and(|ports| wait_owned_ports(manager, &r.service_id, &ports, timeout))
    } else if r.entry.id == "rustfs"
        && r.spec.args.iter().any(|arg| arg == "--console-enable")
        && r.spec.args.windows(2).any(|pair| pair[0] == "--console-address" && pair[1] == ":{port+1}") {
        r.port.and_then(|port| port.checked_add(1).map(|console| [port, console]))
            .is_some_and(|ports| wait_owned_ports(manager, &r.service_id, &ports, timeout))
    } else if managed_rnacos(&r) {
        wait_rnacos_healthy(manager, &r, timeout)
    } else if managed_consul(&r.entry, &r.spec) {
        wait_consul_healthy(manager, &r, timeout)
    } else if let Some(settings) = &minio {
        wait_minio_healthy(manager, &r, settings, timeout)
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
        let panic_error = (managed_rnacos(&r) && rnacos_startup_panicked(manager, &r.service_id))
            .then(|| rnacos_panic_error(manager, &r));
        if r.entry.id == "coredns" || managed_rnacos(&r) || sftpgo.is_some() { crate::ops::stop_service(store, paths, manager, &r.service_id)?; }
        if let Some(error) = panic_error { return Err(error); }
        let error = AppError::new(
            "SERVICE_START_TIMEOUT",
            format!(
                "{} 启动超时（{}s 内{}）",
                r.entry.display_name,
                r.spec.health_timeout_sec,
                if managed_consul(&r.entry, &r.spec) {
                    "监听端口或单节点 leader 未就绪"
                } else if managed_rnacos(&r) {
                    "监听端口或 Raft 集群未就绪"
                } else if minio.is_some() {
                    "S3 服务或已启用的管理台未就绪"
                } else if r.entry.id == "rustfs" {
                    "S3 服务或管理台未就绪"
                } else if managed_rabbitmq(&r.entry, &r.spec) {
                    "AMQP 服务或管理台未就绪"
                } else if r.port.is_some() {
                    "端口未就绪"
                } else {
                    "进程未存活"
                }
            ),
        )
        .with_hint(if managed_rnacos(&r) {
            "已停止服务并保留配置和数据。请查看 r-nacos 日志，核对 Raft 集群配置；已有异常数据应从可用备份恢复。开启鉴权时健康检查至少需要约 14 秒，请预留足够启动时间。".into()
        } else { format!(
            "查看日志页 {} 的最后输出；常见原因是端口冲突、缺少依赖运行库或配置不合法",
            r.service_id
        ) });
        return Err(if r.entry.id == "caddy" { error.with_detail(manager.tail(&r.service_id, 40).join("\n")) } else { error });
    }
    if let Some(snapshot) = caddy_snapshot { crate::sites::record_endpoints(manager, "caddy", snapshot); }
    if managed_rnacos(&r) { web_target = rnacos_http_target(manager, &r, 2000, "/rnacos/"); }
    if sftpgo.is_some() {
        let relative = r
            .etc
            .strip_prefix(&paths.base)
            .map_err(|_| AppError::new("SFTPGO_CONFIG_PATH", "配置目录超出托管目录"))?;
        if let Err(error) = store.set_setting(
            SFTPGO_CONFIG_BINDING,
            &crate::paths::portable_path_text(relative),
        ) {
            crate::ops::stop_service(store, paths, manager, &r.service_id)?;
            return Err(error);
        }
    }
    if let Some(port) = r.port {
        manager.set_started_port(&r.service_id, port);
    }
    if r.entry.id == "mariadb" { connect_mariadb(store, manager, &r)?; }
    if minio.as_ref().is_some_and(|settings| settings.destination.is_some()) {
        let probe = local_web_url("127.0.0.1", r.port.and_then(|port| port.checked_add(1)).ok_or_else(|| web_unavailable("MinIO 管理台端口无效。"))?, false, "/")?;
        manager.set_web_target_with_probe(&r.service_id, web_target, probe);
    } else { manager.set_web_target(&r.service_id, web_target); }
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

/// MySQL 在引号外仍把 \# 视为注释；引号内才用反斜杠跳过转义字符。
pub(crate) fn mysql_option_value_end(value: &str) -> usize {
    let mut quote = None;
    let mut escaped = false;
    let mut end = value.len();
    for (offset, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(current) = quote {
            if character == current {
                quote = None;
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '#' {
            end = offset;
            break;
        }
    }
    end
}

/// MySQL/MariaDB option-file 字符串：注释在引号外，已知反斜杠转义按上游解码。
fn mysql_option_text(value: &str) -> String {
    let value = value[..mysql_option_value_end(value)].trim();
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            value
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
        })
        .unwrap_or(value);
    let mut output = String::new();
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('b') => output.push('\u{8}'),
            Some('s') => output.push(' '),
            Some(next @ ('\\' | '\'' | '"')) => output.push(next),
            Some(next) => {
                output.push('\\');
                output.push(next);
            }
            None => output.push('\\'),
        }
    }
    output
}

/// 新实例按版本隔离；保留旧 my.ini 所指向的共享目录，不移动用户数据库。
pub(crate) fn mariadb_data_dir(paths: &Paths, version: &str) -> Result<PathBuf> {
    let isolated = crate::paths::checked_data_path(&paths.base, &format!("data/mariadb-versions/{version}"))?;
    let legacy = crate::paths::checked_data_path(&paths.base, "data/mariadb")?;
    let config = crate::paths::checked_data_path(&paths.base, &format!("etc/mariadb/{version}/my.ini"))?;
    let content = match std::fs::read_to_string(&config) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(isolated),
        Err(error) => return Err(error.into()),
    };
    let normalize = |value: &str| {
        let value = crate::paths::portable_path_text(std::path::Path::new(value))
            .trim_end_matches('/')
            .to_string();
        if cfg!(windows) {
            value.to_lowercase()
        } else {
            value
        }
    };
    let mut section = String::new();
    let mut data = None;
    for line in content.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') { section = line[1..line.len()-1].to_ascii_lowercase(); }
        if ["mysqld", "server", "mariadb", "mariadbd"].contains(&section.as_str()) {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim().eq_ignore_ascii_case("datadir") {
                    data = Some(normalize(&mysql_option_text(value)));
                }
            }
        }
    }
    match data {
        None => Ok(isolated),
        Some(data) if data == normalize(&isolated.to_string_lossy()) => Ok(isolated),
        Some(data) if data == normalize(&legacy.to_string_lossy()) => {
            if legacy.join("mysql").is_dir() { Ok(legacy) } else {
                // 旧配置可能在初始化失败前生成；不复用非空的未知目录。
                let entries = match std::fs::read_dir(&legacy) {
                    Ok(entries) => entries.collect::<std::io::Result<Vec<_>>>()?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                    Err(error) => return Err(error.into()),
                };
                if entries.iter().any(|e| e.file_name() != "data" || !e.path().is_dir() || std::fs::read_dir(e.path()).is_ok_and(|mut entries| entries.next().is_some())) {
                    return Err(AppError::new("MARIADB_DATA_UNVERIFIED", "旧 MariaDB 数据目录不完整，未重新初始化或覆盖")
                        .with_hint("请检查数据/mariadb 目录和日志，保留原文件后恢复有效备份。"));
                }
                Ok(isolated)
            }
        }
        Some(_) => Err(AppError::new("MARIADB_DATA_UNMANAGED", "MariaDB 配置指向自定义数据目录，未自动初始化或启动")
            .with_hint("请保留现有目录，通过原环境导出 SQL 后导入托管实例；不能将未知数据目录自动升级。")),
    }
}

fn verify_mariadb_data_version(data: &std::path::Path, version: &str) -> Result<()> {
    let marker = data.join(".niceenv-mariadb-version");
    if let Ok(recorded) = std::fs::read_to_string(&marker) {
        if crate::install::same_version(recorded.trim(), version) { return Ok(()); }
    } else {
        // 旧版没有版本标记；只接受日志中唯一、明确的原版本，不猜测升级关系。
        use std::io::{Read, Seek, SeekFrom};
        let pattern = regex::Regex::new(r"Starting MariaDB ([0-9]+\.[0-9]+\.[0-9]+)-MariaDB").unwrap();
        let mut versions = std::collections::BTreeSet::new();
        if let Ok(recorded) = std::fs::read_to_string(data.join("mysql_upgrade_info")) {
            if let Some(value) = recorded.trim().split('-').next().filter(|s| s.split('.').count() == 3) { versions.insert(value.to_string()); }
        }
        for entry in std::fs::read_dir(data)? {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "err") { continue; }
            let mut file = std::fs::File::open(path)?;
            let length = file.metadata()?.len();
            file.seek(SeekFrom::Start(length.saturating_sub(2 * 1024 * 1024)))?;
            let mut bytes = Vec::new(); file.take(2 * 1024 * 1024).read_to_end(&mut bytes)?;
            for captures in pattern.captures_iter(&String::from_utf8_lossy(&bytes)) { versions.insert(captures[1].to_string()); }
        }
        if versions.len() == 1 && versions.iter().next().is_some_and(|recorded| crate::install::same_version(recorded, version)) { return Ok(()); }
    }
    Err(AppError::new("MARIADB_DATA_VERSION", "MariaDB 数据目录的原版本与所选版本不一致或无法确认，未启动")
        .with_hint("请使用原版本导出 SQL，再切换版本并导入；原数据目录已保留，未自动升级。"))
}

fn initialize_mariadb(r: &Resolved) -> Result<()> {
    if r.data.join("mysql").is_dir() { return verify_mariadb_data_version(&r.data, &r.entry.version); }
    if r.spec.init_bin.as_deref() != Some("mariadb-install-db.exe")
        || r.spec.init_args.as_ref().is_none_or(|args| args != &["--datadir={data}", "--port={port}"]) {
        return Err(AppError::new("MARIADB_INIT_CUSTOM", "MariaDB 使用自定义初始化配置，未自动执行")
            .with_hint("请检查该套件的初始化程序和参数；托管实例不注册系统服务。"));
    }
    if std::fs::read_dir(&r.data)?.next().transpose()?.is_some() {
        return Err(AppError::new("MARIADB_INIT_FAILED", "MariaDB 数据目录非空，未覆盖或重新初始化").with_hint("请检查已有文件和初始化日志，并从有效备份恢复。"));
    }
    if !cfg!(windows) {
        return Err(AppError::new("MARIADB_INIT_UNSUPPORTED", "该平台尚未配置 MariaDB 初始化程序"));
    }
    let pending = tempfile::Builder::new().prefix(".mariadb-init-").tempdir_in(r.data.parent().ok_or_else(|| AppError::new("MARIADB_PATH", "数据目录无效"))?)?;
    let mut output = tempfile::tempfile()?;
    let mut command = platform::command(r.root.join("mariadb-install-db.exe"));
    // 不传 --service 或命令行密码；隔离初始化成功后才发布数据目录。
    command.arg(format!("--datadir={}", crate::paths::portable_path_text(pending.path())))
        .arg(format!("--port={}", r.port.unwrap_or(3306)))
        .current_dir(&r.root).stdin(std::process::Stdio::null())
        .stdout(output.try_clone()?).stderr(output.try_clone()?);
    let status = crate::dbadmin::wait_client(&mut command, Duration::from_secs(180), || {})?;
    if !status.success() || !pending.path().join("mysql").is_dir() {
        return Err(AppError::new("MARIADB_INIT_FAILED", "MariaDB 初始化失败，原数据未覆盖")
            .with_detail(crate::dbadmin::read_output(&mut output, 64 * 1024)?));
    }
    std::fs::write(pending.path().join(".niceenv-mariadb-version"), &r.entry.version)?;
    std::fs::remove_dir(&r.data)?; // 只允许替换空目录。
    std::fs::rename(pending.path(), &r.data)?;
    Ok(())
}

fn connect_mariadb(store: &Store, manager: &Arc<ServiceManager>, r: &Resolved) -> Result<()> {
    use crate::dbadmin::{DatabaseEngine, MySqlClient};
    let engine = DatabaseEngine::Mariadb;
    let port = r.port.ok_or_else(|| AppError::new("DATABASE_PORT_UNKNOWN", "MariaDB 端口未知"))?;
    crate::ops::verify_database_listener(manager, &r.service_id, port)?;
    let mut candidates = engine.saved_password(store, &r.entry.version).into_iter().collect::<Vec<_>>();
    candidates.push(String::new()); candidates.dedup();
    for password in candidates {
        let client = MySqlClient { exe: crate::dbadmin::database_tool(&r.root, engine, "mysql"), port, root_password: password.clone() };
        if client.verify_data_dir(&r.data).is_err() { continue; }
        if password.is_empty() {
            use rand::Rng;
            let password: String = rand::thread_rng().sample_iter(&rand::distributions::Alphanumeric).take(24).map(char::from).collect();
            store.set_setting(&engine.password_key(&r.entry.version), &password)?;
            client.reset_root_password(&password)?;
            MySqlClient { root_password: password, ..client }.verify_data_dir(&r.data)?;
        } else { store.set_setting(&engine.password_key(&r.entry.version), &password)?; }
        std::fs::write(r.data.join(".niceenv-mariadb-version"), &r.entry.version)?;
        return Ok(());
    }
    // 旧实例凭据可能由用户修改，保留运行以便通过数据库页验证并更新。
    manager.push_log(&r.service_id, "MariaDB 已启动；保存的 root 凭据无法认证，请在数据库页更新连接密码。");
    Ok(())
}

/// 首次启动前执行一次性初始化（Neo4j set-initial-password 等）。
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
    let env = r.spec.env.iter().flatten().map(|(key, value)| (key, expand(value, r)));
    // Windows 的 RabbitMQ 插件脚本是 .bat；与服务启动路径保持一致，经 cmd.exe
    // 转发，并把受管环境传给初始化命令，确保 RABBITMQ_BASE 指向当前实例。
    let (program, args) = if cfg!(windows) && is_script(&init_exe) {
        let mut command = crate::paths::portable_path_text(&init_exe);
        if command.contains(' ') { command = format!("\"{command}\""); }
        let mut forwarded = vec!["/C".to_string(), command];
        forwarded.extend(args);
        (PathBuf::from("cmd.exe"), forwarded)
    } else {
        (init_exe, args)
    };
    let out = platform::command(&program)
        .args(&args)
        .current_dir(&r.root)
        .envs(env)
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
            if active.map(|a| !crate::install::same_version(&a.version, &p.version)).unwrap_or(true) {
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

    #[test]
    fn mariadb_legacy_data_is_preserved_and_unknown_versions_are_not_upgraded() {
        assert_eq!(mysql_option_text(r"C:/old-data/a\#b"), r"C:/old-data/a\");
        assert_eq!(
            mysql_option_text(r##"C:/old-data/a\"#b""##),
            "C:/old-data/a\"#b\""
        );
        let (_temp, state, r) = fixture_version("mariadb", Some("11.4.8"));
        let legacy = state.paths.data().join("mariadb");
        let config = r.etc.join("my.ini");
        std::fs::create_dir_all(legacy.join("mysql")).unwrap();
        std::fs::write(legacy.join("sentinel"), b"existing database").unwrap();
        std::fs::write(&config, format!("[mysqld]\ndatadir={}\nport=3306\n", crate::paths::nginx_path(&legacy))).unwrap();
        assert_eq!(mariadb_data_dir(&state.paths, "11.4.8").unwrap(), legacy);
        assert_eq!(verify_mariadb_data_version(&legacy, "11.4.8").unwrap_err().code, "MARIADB_DATA_VERSION");
        std::fs::write(legacy.join("old.err"), "[Note] Starting MariaDB 11.4.8-MariaDB source revision verified\n").unwrap();
        verify_mariadb_data_version(&legacy, "11.4.8").unwrap();
        assert!(verify_mariadb_data_version(&legacy, "10.11.13").is_err());
        assert_eq!(
            mariadb_data_dir(&state.paths, "12.3.3").unwrap(),
            state.paths.data().join("mariadb-versions/12.3.3")
        );
        assert_eq!(
            std::fs::read(legacy.join("sentinel")).unwrap(),
            b"existing database"
        );
        std::fs::write(
            &config,
            format!(
                "[mysqld]\ndatadir=\"{}\" # managed directory\n",
                crate::paths::quoted_config_path(&legacy)
            ),
        )
        .unwrap();
        assert_eq!(mariadb_data_dir(&state.paths, "11.4.8").unwrap(), legacy);
        #[cfg(unix)]
        {
            let paths = Paths::new(_temp.path().join(r"data\new\tail"));
            let legacy = paths.data().join("mariadb");
            std::fs::create_dir_all(legacy.join("mysql")).unwrap();
            let file = paths.etc().join("mariadb/11.4.8/my.ini");
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(
                &file,
                format!(
                    "[mysqld]\ndatadir=\"{}\"\n",
                    crate::paths::quoted_config_path(&legacy)
                ),
            )
            .unwrap();
            assert_eq!(mariadb_data_dir(&paths, "11.4.8").unwrap(), legacy);
            let different = crate::paths::portable_path_text(&legacy).replace('\\', "/");
            std::fs::create_dir_all(std::path::Path::new(&different).join("mysql")).unwrap();
            std::fs::write(&file, format!("[mysqld]\ndatadir=\"{different}\"\n")).unwrap();
            assert_eq!(
                mariadb_data_dir(&paths, "11.4.8").unwrap_err().code,
                "MARIADB_DATA_UNMANAGED"
            );
        }
    }

    #[test]
    fn mariadb_old_snapshot_removes_service_registration_and_preserves_custom_configuration() {
        let (_temp, state, r) = fixture_version("mariadb", Some("11.4.8"));
        let mut old = r.entry.clone();
        let run = old.run.as_mut().unwrap();
        run.init_args = Some(vec!["--datadir={data}".into(), "--service=MariaDB".into()]);
        run.init_dirs = vec!["data".into()];
        let snapshot = std::path::Path::new(&r.inst.install_path).join(".niceenv-package.json");
        std::fs::write(&snapshot, serde_json::to_vec(&old).unwrap()).unwrap();
        let upgraded = state.installer.installed_entry(&r.inst).run.unwrap();
        assert_eq!(upgraded.init_args.unwrap(), ["--datadir={data}", "--port={port}"]);
        assert!(upgraded.init_dirs.is_empty());
        old.run.as_mut().unwrap().init_args.as_mut().unwrap().push("--custom-option".into());
        std::fs::write(&snapshot, serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(state.installer.installed_entry(&r.inst).run).unwrap(), serde_json::to_value(old.run).unwrap());
    }

    fn fixture(id: &str) -> (tempfile::TempDir, crate::CoreState, Resolved) {
        fixture_version(id, None)
    }

    fn fixture_version(
        id: &str,
        version: Option<&str>,
    ) -> (tempfile::TempDir, crate::CoreState, Resolved) {
        let prefix = if id == "sftpgo" {
            "niceenv SFTP # %23 中文 "
        } else {
            "niceenv fixture "
        };
        let temp = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let state = crate::CoreState {
            store: Store::open(paths.db()).unwrap(), paths,
            installer: crate::install::Installer { manifest: serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap() },
            manager: Arc::new(ServiceManager::new()),
            downloader: Arc::new(crate::download::Downloader::new()), emit: Arc::new(|_| {}),
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
    fn config_paths_roundtrip_through_yaml_and_dotenv_without_reexpanding_names() {
        let (temp, _state, mut r) = fixture("qdrant");
        r.data = temp.path().join("data {etc} # O'Brien $NSB_PATH_TEST [1]");
        #[cfg(unix)]
        {
            r.data = r.data.join(r"literal\new\tail");
        }
        r.root = temp.path().join("runtime {data}");
        let raw = crate::paths::portable_path_text(&r.data);
        assert_eq!(expand("{data}/storage", &r), format!("{raw}/storage"));
        assert_eq!(
            expand("{root}", &r),
            crate::paths::portable_path_text(&r.root)
        );
        r.spec.config_file = Some("config.yaml".into());
        let template = "# preserve comment\r\nstorage:\r\n  plain: {data}/plain#part # keep plain\r\n  double: \"{data}/double\" # keep double\r\n  single: '{data}/single' # keep single\r\n";
        let rendered = expand_config(template, &r).unwrap();
        let decoded: yaml_serde::Value = yaml_serde::from_str(&rendered).unwrap();
        for (key, suffix) in [
            ("plain", "plain#part"),
            ("double", "double"),
            ("single", "single"),
        ] {
            assert_eq!(
                decoded["storage"][key].as_str(),
                Some(format!("{raw}/{suffix}").as_str())
            );
            assert!(rendered.contains(&format!(" # keep {key}\r\n")));
        }
        assert!(rendered.starts_with("# preserve comment\r\n"));
        r.spec.config_file = Some(".env".into());
        let template = "NSB_PATH_TEST=must-not-replace-literal-path\nPLAIN={data}/plain # keep\nDOUBLE=\"{data}/double\"\nSINGLE='{data}/single'\n";
        let rendered = expand_config(template, &r).unwrap();
        let decoded = rnacos_config_env(&rendered, temp.path()).unwrap();
        for (key, suffix) in [
            ("PLAIN", "plain"),
            ("DOUBLE", "double"),
            ("SINGLE", "single"),
        ] {
            assert_eq!(
                decoded.iter().find(|(name, _)| name == key).unwrap().1,
                format!("{raw}/{suffix}")
            );
        }
        assert!(rendered.contains(" # keep\n"));
        r.spec.config_file = Some("config.yaml".into());
        let template =
            "storage:\n  storage_path: {data}/storage\n  snapshots_path: \"{data}/snapshots\"\n";
        let legacy = format!(
            "# user comment\n{}custom: preserved\n",
            expand(template, &r)
        );
        let repaired = sync_template_paths(&legacy, template, &r).unwrap();
        let decoded: yaml_serde::Value = yaml_serde::from_str(&repaired).unwrap();
        assert_eq!(
            decoded["storage"]["snapshots_path"].as_str(),
            Some(format!("{raw}/snapshots").as_str())
        );
        assert_eq!(decoded["custom"].as_str(), Some("preserved"));
        assert!(repaired.starts_with("# user comment\n"));
        assert_eq!(
            sync_template_paths(&repaired, template, &r).unwrap(),
            repaired
        );
        let other_section = format!("custom:\n  snapshots_path: \"{raw}/snapshots\"\n");
        let with_custom = format!("{repaired}{other_section}");
        assert_eq!(
            sync_template_paths(&with_custom, template, &r).unwrap(),
            with_custom
        );
        let custom = "storage:\n  storage_path: custom/location # leave this value\n  snapshots_path: other/location\n";
        assert_eq!(sync_template_paths(custom, template, &r).unwrap(), custom);
    }

    #[test]
    fn template_ports_update_without_losing_custom_settings_or_comments() {
        for id in ["caddy", "mariadb", "qdrant", "rnacos"] {
            let (_temp, state, mut r) = fixture(id);
            r.port = Some(31000);
            let template = r.spec.config_template.as_ref().unwrap();
            let generated = expand_config(template, &r)
                .unwrap()
                .replace('\n', "\r\n")
                .replace("port=31000", "port = 31000 ; inline comment");
            let extra = if id == "mariadb" {
                "\r\n[custom]\r\nport=31000\r\nsetting=keep\r\n"
            } else {
                "\r\n# custom setting 31000 remains\r\n"
            };
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
        let generated = expand_config(r.spec.config_template.as_deref().unwrap(), &r).unwrap();
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
        let generated = expand_config(tpl, &r).unwrap();
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
        // OS 分配的临时端口的前一位可能被其他进程占用或由 Windows 保留。
        // 先确认整组可用，再只占用副端口，才能验证副端口冲突这一前提。
        let secondary = (40000..60000).find_map(|base| {
            if tcp_port_bindable(base) && tcp_port_bindable(base + 1) {
                std::net::TcpListener::bind(("::1", base + 1)).ok()
            } else { None }
        }).expect("an available primary and secondary port pair");
        let port = secondary.local_addr().unwrap().port(); r.port = Some(port - 1);
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        let selected = select_port(&state.store, &r).unwrap().unwrap();
        assert_ne!(selected, port - 1); assert_ne!(selected, port);
        assert!(tcp_port_bindable(selected)); assert!(tcp_port_bindable(selected + 1));
        assert!(state.store.get_port_assign("qdrant").is_none());
        assert!(state.store.get_setting("portOverride.qdrant").is_none());
        state.store.set_setting("autoFallbackPort", "false").unwrap();
        state.store.set_port_override("qdrant", r.port).unwrap();
        let error = select_port(&state.store, &r).unwrap_err();
        assert_eq!(error.code, "PORT_IN_USE"); assert_eq!(error.port, Some(port));
        assert_eq!(error.pid, Some(std::process::id()));
        state.store.set_port_override("qdrant", None).unwrap();
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
        let base = expand_config(r.spec.config_template.as_deref().unwrap(), &r).unwrap();
        let legacy = base.replace("RNACOS_DATA_DIR=\"", "RNACOS_DATA_DIR=").replace("nacos_db\"", "nacos_db");
        let marker = format!("NICEENV_CONFIG_FIXTURE_{}", rand::random::<u64>());
        let content = format!("# keep user comment\r\n{}\r\n{marker}='literal $value'\r\n", legacy.replace('\n', "\r\n"));
        std::fs::write(&config, content).unwrap();
        let env = prepare_config(&state.paths, &r).unwrap();
        assert!(env.iter().any(|(k, v)| k == &marker && v == "literal $value"));
        assert!(std::env::var_os(&marker).is_none());
        assert!(std::fs::read_to_string(&config)
            .unwrap()
            .starts_with("# keep user comment\r\n"));
        assert!(env
            .iter()
            .any(|(k, v)| k == "RNACOS_DATA_DIR" && v == &expand("{data}/nacos_db", &r)));
        for invalid in [
            "BROKEN='secret-unclosed",
            "CUSTOM=one\nCUSTOM=two",
            "CUSTOM=hidden\0value",
        ] {
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
        let mut custom = r.entry.clone();
        custom.run.as_mut().unwrap().cwd = Some("{etc}/resources".into());
        std::fs::write(
            PathBuf::from(&r.inst.install_path).join(".niceenv-package.json"),
            serde_json::to_vec(&custom).unwrap(),
        )
        .unwrap();
        let readonly = Store::open_read_only(state.paths.db()).unwrap();
        let preview = state.paths.etc_dir("sftpgo", "migration-preview");
        assert_eq!(
            sftpgo_migration_cwd(&readonly, &state.paths, preview.clone()).unwrap(),
            preview.join("resources")
        );
        assert!(!preview.exists(), "只读解析不能创建目录");
        let secret = format!(
            "{}/literal-secret",
            crate::paths::portable_path_text(&state.paths.base)
        );
        for version in ["2.7.4", "2.7.5"] {
            let directory = state.paths.etc_dir("sftpgo", version);
            std::fs::create_dir(directory.join("resources")).unwrap();
            std::fs::write(directory.join("resources/master.key"), &secret).unwrap();
            // 目录选择使用的伪 Bolt 字节不能充当可迁移账号库，迁移阶段使用有效 SQLite。
            let conn = rusqlite::Connection::open(directory.join("migration.sqlite")).unwrap();
            conn.execute_batch("CREATE TABLE users(id INTEGER PRIMARY KEY,home_dir TEXT);
                CREATE TABLE folders(id INTEGER PRIMARY KEY,path TEXT);").unwrap();
            drop(conn);
            std::fs::write(directory.join("sftpgo.json"), r#"{"data_provider":{"driver":"sqlite","name":"migration.sqlite"},"kms":{"secrets":{"master_key_path":"master.key"}}}"#).unwrap();
        }
        let migration = tempfile::tempdir().unwrap();
        let target = migration.path().join("migration destination");
        crate::paths::copy_data_dir(&state.paths.base, &target).unwrap();
        for version in ["2.7.4", "2.7.5"] {
            assert_eq!(
                std::fs::read_to_string(
                    target.join(format!("etc/sftpgo/{version}/resources/master.key"))
                )
                .unwrap(),
                secret
            );
        }
    }

    #[test]
    fn console_targets_use_resolved_ports_and_preserve_sftpgo_web_configuration() {
        for (id, offset, path) in [("mailpit", 0, "/"), ("minio", 1, "/"), ("rustfs", 1, "/"), ("zincsearch", 0, "/"), ("consul", 0, "/ui"), ("qdrant", 0, "/dashboard"), ("temporal-cli", 1000, "/"), ("neo4j", 0, "/browser"), ("rabbitmq", 10000, "/")] {
            let (_temp, state, mut r) = fixture(id);
            let base_port = if id == "neo4j" { 7474 } else { 31000 };
            r.port = Some(base_port);
            if id == "qdrant" {
                prepare_config(&state.paths, &r).unwrap();
                std::fs::create_dir(r.root.join("static")).unwrap();
                std::fs::write(r.root.join("static/index.html"), "<html></html>").unwrap();
            }
            assert_eq!(generic_web_target(&r, None).unwrap(), format!("http://127.0.0.1:{}{path}", base_port + offset));
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
        assert_eq!(
            local_web_url("dev-box.local", 8080, false, "/").unwrap(),
            "http://dev-box.local:8080/"
        );
        assert!(local_web_url("remote.example/path", 8080, false, "/").is_err());
        assert!(local_web_url("127.0.0.1", 0, false, "/").is_err());
    }

    #[test]
    fn minio_console_configuration_matches_environment_file_precedence_and_disabled_ports() {
        let (_temp, state, mut r) = fixture("minio"); r.port = Some(33000);
        let env = r.spec.env.as_mut().unwrap();
        for (key, value) in [("MINIO_CONFIG", ""), ("MINIO_CONFIG_ENV_FILE", ""), ("MINIO_BROWSER", "on"),
            ("MINIO_BROWSER_REDIRECT_URL", ""), ("CONSOLE_SUBPATH", "")] { env.insert(key.into(), value.into()); }
        r.spec.env.as_mut().unwrap().insert("MINIO_BROWSER_REDIRECT_URL".into(), "https://console.example.invalid/one//two/../%E4%B8%AD%E6%96%87%20path+plus/".into());
        assert_eq!(generic_web_target(&r, None).unwrap(), "https://console.example.invalid/one//%E4%B8%AD%E6%96%87%20path+plus/");
        let file = r.root.join("minio.env");
        let marker = format!("NICEENV_MINIO_FIXTURE_{}", rand::random::<u64>());
        let content = format!("# fixture\r\nMINIO_BROWSER=on\r\nexport MINIO_BROWSER='Off'\r\n{marker}='$literal'\r\n");
        std::fs::write(&file, &content).unwrap(); r.spec.env.as_mut().unwrap().insert("MINIO_CONFIG_ENV_FILE".into(), "minio.env".into());
        assert!(!minio_settings(&r).unwrap().browser); assert!(generic_web_target(&r, None).unwrap_err().hint.unwrap().contains("已关闭"));
        assert!(std::env::var_os(&marker).is_none()); assert_eq!(std::fs::read_to_string(&file).unwrap(), content);
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let console = occupied.local_addr().unwrap().port();
        r.port = Some(console - 1);
        if tcp_port_bindable(console - 1) { assert_eq!(select_port(&state.store, &r).unwrap(), r.port); }
        std::fs::write(&file, "MINIO_BROWSER=ENABLED\nMINIO_BROWSER_REDIRECT_URL='https://external.invalid/from-file'\n").unwrap();
        assert!(select_port(&state.store, &r).is_err());
        assert!(generic_web_target(&r, None).unwrap().ends_with("/from-file")); drop(occupied);
        for invalid in ["bad secret-line", "MINIO_BROWSER=on # not a supported inline comment", "MINIO_BROWSER=hidden-secret\0"] {
            std::fs::write(&file, invalid).unwrap();
            let error = minio_settings(&r).err().unwrap(); assert_eq!(error.code, "MINIO_ENV_INVALID");
            assert!(!format!("{error:?}").contains("hidden-secret")); assert_eq!(std::fs::read_to_string(&file).unwrap(), invalid);
        }
        std::fs::write(&file, "MINIO_BROWSER=on\nMINIO_BROWSER_REDIRECT_URL=https://secret:password@example.invalid/path\n").unwrap();
        let error = minio_settings(&r).err().unwrap(); assert_eq!(error.code, "MINIO_REDIRECT_INVALID"); assert!(!format!("{error:?}").contains("password"));
        r.spec.env.as_mut().unwrap().insert("MINIO_CONFIG_ENV_FILE".into(), "absent.env".into());
        assert!(minio_settings(&r).unwrap().browser);
        let config = r.root.join("config.yaml"); std::fs::write(&config, "version: v2\naddress: 127.0.0.1:1234\n").unwrap();
        r.spec.env.as_mut().unwrap().insert("MINIO_CONFIG".into(), "config.yaml".into());
        assert_eq!(minio_settings(&r).err().unwrap().code, "MINIO_PORT_CONFIG");
        assert_eq!(std::fs::read_to_string(config).unwrap(), "version: v2\naddress: 127.0.0.1:1234\n");
    }

    #[test]
    fn configured_console_proxy_is_returned_after_only_a_local_owned_probe() {
        use std::io::{Read, Write};
        let (_temp, state, _r) = fixture("minio"); register_services(&state.paths, &state.store, &state.manager);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let port = listener.local_addr().unwrap().port();
        let local = format!("http://127.0.0.1:{port}/"); let public = "https://console.example.invalid/storage/";
        state.manager.adopt("minio", &[std::process::id()], Some(port));
        state.manager.set_web_target_with_probe("minio", Ok(public.into()), local.clone());
        let worker = listener.try_clone().unwrap();
        let serve = std::thread::spawn(move || {
            let (mut stream, _) = worker.accept().unwrap(); stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut input = [0; 4096]; let length = stream.read(&mut input).unwrap();
            let request = String::from_utf8_lossy(&input[..length]); assert!(request.starts_with("GET / HTTP/1.1"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 13\r\nConnection: close\r\n\r\n<html></html>").unwrap();
        });
        assert_eq!(state.service_web_url("minio").unwrap(), public); serve.join().unwrap();
        state.manager.set_web_target_with_probe("minio", Ok("https://secret:password@example.invalid/".into()), local.clone());
        let error = state.service_web_url("minio").unwrap_err(); assert_eq!(error.code, "SERVICE_WEB_UNAVAILABLE"); assert!(!format!("{error:?}").contains("password"));
        state.manager.set_web_target_with_probe("minio", Ok(public.into()), "http://example.invalid/".into());
        assert_eq!(state.service_web_url("minio").unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        state.manager.services.lock().get("minio").unwrap().pids.lock().clear(); state.manager.set_state("minio", crate::model::ServiceState::Stopped);
        assert_eq!(state.manager.web_target("minio").unwrap_err().code, "SERVICE_WEB_UNKNOWN");
        state.manager.services.lock().remove("minio");
    }

    #[test]
    fn console_probe_checks_live_owner_http_response_and_does_not_follow_external_redirects() {
        use std::io::{Read, Write};
        let (_temp, state, _r) = fixture("mailpit");
        register_services(&state.paths, &state.store, &state.manager);
        let listener = std::net::TcpListener::bind(("::1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://[::1]:{port}/console");
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
        state.manager.set_web_target("mailpit", Ok(format!("http://192.0.2.10:{port}/")));
        assert_eq!(state.service_web_url("mailpit").unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        state.manager.set_web_target("mailpit", Ok(url));
        drop(listener);
        assert_eq!(state.service_web_url("mailpit").unwrap_err().code, "SERVICE_WEB_UNAVAILABLE");
        state.manager.set_state("mailpit", crate::model::ServiceState::Stopped);
        assert_eq!(state.manager.web_target("mailpit").unwrap_err().code, "SERVICE_WEB_UNKNOWN");
        state.manager.services.lock().remove("mailpit");
    }

    #[test]
    fn console_probe_pins_a_local_hostname_to_the_owned_listener() {
        use std::io::{Read, Write};
        let (_temp, state, _r) = fixture("mailpit");
        register_services(&state.paths, &state.store, &state.manager);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        state
            .manager
            .adopt("mailpit", &[std::process::id()], Some(port));
        state
            .manager
            .set_web_target("mailpit", Ok(format!("http://localhost:{port}/console")));
        let worker = listener.try_clone().unwrap();
        let serve = std::thread::spawn(move || {
            let (mut stream, _) = worker.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut input = [0; 4096];
            let n = stream.read(&mut input).unwrap();
            let request = String::from_utf8_lossy(&input[..n]);
            assert!(request.starts_with("GET /console HTTP/1.1"));
            assert!(request.to_ascii_lowercase().contains("host: localhost:"));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 13\r\nConnection: close\r\n\r\n<html></html>").unwrap();
        });
        assert_eq!(
            state.service_web_url("mailpit").unwrap(),
            format!("http://localhost:{port}/console")
        );
        serve.join().unwrap();
        state
            .manager
            .set_state("mailpit", crate::model::ServiceState::Stopped);
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
        #[cfg(unix)]
        {
            let literal_paths = Paths::new(state.paths.base.join(r"literal\data"));
            assert_eq!(
                qdrant_snapshot_directory(&r.entry, &r.root, &literal_paths).unwrap(),
                literal_paths.data().join("qdrant/private-snapshots")
            );
        }
        if cfg!(windows) {
            r.entry.run.as_mut().unwrap().env = Some(std::collections::HashMap::from([("qdrant__storage__snapshots_path".into(), "{data}/lowercase-snapshots".into())]));
            assert_eq!(qdrant_snapshot_directory(&r.entry, &r.root, &state.paths).unwrap(), state.paths.data().join("qdrant/lowercase-snapshots"));
            r.entry.run.as_mut().unwrap().env.as_mut().unwrap().insert("QDRANT__STORAGE__SNAPSHOTS_PATH".into(), "other".into());
            assert_eq!(qdrant_snapshot_directory(&r.entry, &r.root, &state.paths).unwrap_err().code, "QDRANT_CONFIG_ENV");
        }
    }

    #[test]
    fn qdrant_path_key_uses_platform_path_rules() {
        #[cfg(windows)]
        {
            assert_eq!(
                qdrant_path_key(std::path::Path::new(r"C:\NiceEnv\Data\Qdrant")),
                "c:/niceenv/data/qdrant"
            );
            assert_eq!(
                qdrant_path_key(std::path::Path::new(r"//?/C:/NiceEnv/Data/Qdrant/")),
                "c:/niceenv/data/qdrant"
            );
        }
        #[cfg(unix)]
        {
            let with_backslash = std::path::Path::new("/tmp/niceenv\\qdrant");
            let with_slash = std::path::Path::new("/tmp/niceenv/qdrant");
            assert_eq!(qdrant_path_key(with_backslash), "/tmp/niceenv\\qdrant");
            assert_eq!(qdrant_path_key(with_slash), "/tmp/niceenv/qdrant");
            assert_ne!(qdrant_path_key(with_backslash), qdrant_path_key(with_slash));
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
        prepare_config(&state.paths, &r).unwrap();
        let config_path = r.etc.join("config.yaml");
        let content = std::fs::read_to_string(&config_path).unwrap().replace("host: 127.0.0.1", "host: \"::1\"");
        std::fs::write(&config_path, content).unwrap();
        let base = (30000..42000).find(|port| [0, 1, 2].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        let occupied_grpc = std::net::TcpListener::bind(("::1", base + 1)).unwrap();
        state.store.set_port_override("qdrant", Some(base)).unwrap(); state.store.set_setting("autoFallbackPort", "true").unwrap();
        struct Cleanup(Arc<crate::CoreState>);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = self.0.stop_service("qdrant"); } }
        let _cleanup = Cleanup(state.clone());
        state.start_service("qdrant").unwrap_or_else(|e| panic!("{e:?}\n{:?}", state.manager.tail("qdrant", 20)));
        let first = state.manager.snapshot("qdrant").unwrap(); let port = first.port.unwrap(); assert_ne!(port, base);
        assert!(owned_ports_ready(&state.manager, "qdrant", &[port, port + 1]));
        assert_eq!(state.service_web_url("qdrant").unwrap_err().code, "QDRANT_WEB_MISSING");
        let client = reqwest::blocking::Client::builder().no_proxy().timeout(Duration::from_secs(10)).build().unwrap();
        let endpoint = |port, path: &str| format!("http://[::1]:{port}{path}");
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
        std::fs::create_dir_all(r.root.join("static")).unwrap();
        std::fs::write(r.root.join("static/probe.txt"), "resource contents").unwrap();
        let content = "{\"Data_Provider\":{\"DRIVER\":\"bolt\",\"Name\":\"kept.db\"},\"HTTPD\":{\"Templates_Path\":\"custom-templates\"}}\n";
        std::fs::write(r.root.join("sftpgo.json"), content).unwrap();
        let config = prepare_sftpgo(&state.store, &state.paths, &r).unwrap();
        assert_eq!(std::fs::read_to_string(&config.file).unwrap(), content);
        assert!(config
            .env
            .iter()
            .all(|(key, _)| key != "SFTPGO_HTTPD__TEMPLATES_PATH"));
        assert!(config
            .env
            .iter()
            .all(|(key, _)| !key.starts_with("SFTPGO_DATA_PROVIDER")));
        let static_path = &config
            .env
            .iter()
            .find(|(key, _)| key == "SFTPGO_HTTPD__STATIC_FILES_PATH")
            .unwrap()
            .1;
        assert!(!static_path.contains("//?/"));
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(static_path).join("probe.txt")).unwrap(),
            "resource contents"
        );
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
        let database = r.etc.join("accounts # %23.db");
        let content =
            serde_json::json!({"data_provider":{"driver":"sqlite","name":"accounts # %23.db"}})
                .to_string();
        std::fs::write(&config.file, &content).unwrap();
        let prepared = inspect_sftpgo(&state.store, &state.paths, &r, false, false).unwrap();
        let connection = &prepared
            .env
            .iter()
            .find(|(key, _)| key == "SFTPGO_DATA_PROVIDER__CONNECTION_STRING")
            .unwrap()
            .1;
        assert_eq!(
            crate::configpaths::sqlite_connection_path(connection).unwrap(),
            Some(PathBuf::from(crate::paths::portable_path_text(&database)))
        );
        assert_eq!(
            prepare_sftpgo(&state.store, &state.paths, &r)
                .err()
                .unwrap()
                .code,
            "SFTPGO_STATE_MISSING"
        );
        std::fs::write(&database, b"persisted database fixture").unwrap();
        assert!(prepare_sftpgo(&state.store, &state.paths, &r)
            .unwrap()
            .state_files
            .contains(&database));
        let explicit = serde_json::json!({"data_provider":{"driver":"sqlite","connection_string":format!("{}?mode=rw", crate::configpaths::sqlite_file_uri(&database)),"password":"retained"}}).to_string();
        std::fs::write(&config.file, &explicit).unwrap();
        let prepared = prepare_sftpgo(&state.store, &state.paths, &r).unwrap();
        assert!(prepared
            .env
            .iter()
            .all(|(key, _)| key != "SFTPGO_DATA_PROVIDER__CONNECTION_STRING"));
        assert!(prepared.state_files.contains(&database));
        std::fs::remove_file(&database).unwrap();
        assert_eq!(
            prepare_sftpgo(&state.store, &state.paths, &r)
                .err()
                .unwrap()
                .code,
            "SFTPGO_STATE_MISSING"
        );
        assert_eq!(std::fs::read_to_string(&config.file).unwrap(), explicit);
        std::fs::write(&config.file, r#"{"data_provider":{"driver":"sqlite","connection_string":"file:temporary?mode=memory"}}"#).unwrap();
        assert!(prepare_sftpgo(&state.store, &state.paths, &r).is_ok());
        let mut r = r;
        r.spec.cwd = Some("{root}".into());
        std::fs::write(r.root.join("relative.db"), b"database at actual child cwd").unwrap();
        std::fs::write(&config.file, r#"{"data_provider":{"driver":"sqlite","connection_string":"file:relative.db?mode=rw"}}"#).unwrap();
        assert!(prepare_sftpgo(&state.store, &state.paths, &r)
            .unwrap()
            .state_files
            .contains(&r.root.join("relative.db")));
        for duplicate in [
            r#"{"Data_Provider":{"DRIVER":"bolt"},"data_provider":{"driver":"sqlite"}}"#,
            r#"{"httpd":{"bindings":[{"ENABLE_HTTPS":true,"enable_https":false}]}}"#,
        ] {
            std::fs::write(&config.file, duplicate).unwrap();
            assert_eq!(
                prepare_sftpgo(&state.store, &state.paths, &r)
                    .err()
                    .unwrap()
                    .code,
                "SFTPGO_CONFIG_AMBIGUOUS"
            );
            assert_eq!(std::fs::read_to_string(&config.file).unwrap(), duplicate);
        }
        std::fs::remove_file(&config.file).unwrap();
        let yaml = r.etc.join("sftpgo.yaml");
        let content = "defaults: &provider\n  Driver: bolt\n  Name: kept.db\nData_Provider:\n  <<: *provider\nSFTPD:\n  HOST_KEYS: [custom_key]\nHTTPD:\n  Templates_Path: custom-templates\n  Web_Root: /case-path\n  BINDINGS:\n    - ADDRESS: '::1'\n      ENABLE_HTTPS: true\n      ENABLE_WEB_ADMIN: true\n";
        std::fs::write(&yaml, content).unwrap();
        std::fs::write(
            r.etc.join("custom_key"),
            b"existing case-sensitive key file",
        )
        .unwrap();
        r.port = Some(31000);
        let prepared = prepare_sftpgo(&state.store, &state.paths, &r).unwrap();
        assert_eq!(
            prepared.state_files,
            vec![r.etc.join("kept.db"), r.etc.join("custom_key")]
        );
        assert!(prepared
            .env
            .iter()
            .all(|(key, _)| key != "SFTPGO_HTTPD__TEMPLATES_PATH"
                && !key.starts_with("SFTPGO_DATA_PROVIDER")));
        assert_eq!(
            prepared.web_target.unwrap(),
            "https://[::1]:37058/case-path/web/admin"
        );
        assert_eq!(std::fs::read_to_string(&yaml).unwrap(), content);
        std::fs::remove_file(r.etc.join("custom_key")).unwrap();
        assert_eq!(
            prepare_sftpgo(&state.store, &state.paths, &r)
                .err()
                .unwrap()
                .code,
            "SFTPGO_STATE_MISSING"
        );
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
    fn sftpgo_env_migration_preserves_interpolated_credentials_and_assignment_ranges() {
        use std::collections::HashMap;
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("old data");
        let target = temp.path().join("新 data # [1]");
        let old = crate::paths::portable_path_text(&source);
        let new = crate::paths::portable_path_text(&target);
        let rebase = crate::paths::DataPathRebase::new(&source, &target).unwrap();
        let first = format!("# retained comment\r\nBASE='{old}'\r\n  export SFTPGO_KMS__SECRETS__MASTER_KEY_PATH = ${{BASE}}/first.key  # key\r\nPASSWORD=${{SFTPGO_KMS__SECRETS__MASTER_KEY_PATH}}\r\nSFTPGO_KMS__SECRETS__MASTER_KEY_PATH='${{BASE}}/literal'\r\nSFTPGO_KMS__SECRETS__MASTER_KEY_PATH=\"${{BASE}}/last.key\"\r\nMULTILINE=\"中文\r\n${{SFTPGO_KMS__SECRETS__MASTER_KEY_PATH}}\r\nend\" # multiline\r\nEMPTY=  # empty\r\nQUOTED='${{BASE}} # literal'\r\n");
        let files = vec![(PathBuf::from("10-first.env"), first), (PathBuf::from("20-last.env"),
            "SFTPGO_KMS__SECRETS__MASTER_KEY_PATH=/ignored\nAFTER=${SFTPGO_KMS__SECRETS__MASTER_KEY_PATH}\nEMPTY=ignored\n".into())];
        let original = sftpgo_parse_env(&files, HashMap::new()).unwrap();
        let config = crate::configpaths::sftpgo_config("{}", true).unwrap();
        let (updated, resources) =
            rebase_sftpgo_env(&files, HashMap::new(), HashMap::new(), &config, &rebase).unwrap();
        let mut expected = original.clone();
        expected.insert(
            "SFTPGO_KMS__SECRETS__MASTER_KEY_PATH".into(),
            format!("{new}/last.key"),
        );
        assert_eq!(
            sftpgo_parse_env(&updated, HashMap::new()).unwrap(),
            expected
        );
        assert!(updated[0].1.contains("  # key\r\n"));
        assert!(updated[0].1.contains("EMPTY=  # empty\r\n"));
        assert!(updated[0].1.contains("QUOTED='${BASE} # literal'\r\n"));
        assert_eq!(expected["PASSWORD"], format!("{old}/first.key"));
        assert_eq!(expected["AFTER"], format!("{old}/last.key"));
        assert!(resources.iter().any(|r| r.path == source.join("last.key")));
        for value in [
            "",
            "plain",
            "中文 # $VALUE",
            "a'b\"c",
            "first\nsecond",
            r"C:\path\tail",
            "old/path'suffix\\",
        ] {
            let literal = sftpgo_env_literal(value).unwrap();
            let parsed = sftpgo_parse_env(
                &[(PathBuf::from("probe.env"), format!("VALUE={literal}\n"))],
                HashMap::new(),
            )
            .unwrap();
            assert_eq!(parsed["VALUE"], value);
        }
        let inherited = HashMap::from([(
            "SFTPGO_KMS__SECRETS__MASTER_KEY_PATH".into(),
            format!("{old}/override"),
        )]);
        assert_eq!(
            rebase_sftpgo_env(&files, inherited.clone(), inherited, &config, &rebase)
                .err()
                .unwrap()
                .code,
            "DATA_DIR_ENV_OVERRIDE"
        );
        let unknown = HashMap::from([
            (
                "SFTPGO_httpd__TEMPLATES_PATH".into(),
                format!("{old}/opaque"),
            ),
            (
                "SFTPGO_HTTPD__BINDINGS__10__CERTIFICATE_FILE".into(),
                format!("{old}/ignored"),
            ),
            (
                "SFTPGO_SFTPD__HOST_KEYS__0".into(),
                format!("{old}/ignored"),
            ),
        ]);
        assert!(
            crate::configpaths::sftpgo_environment_paths(&config, &unknown, &rebase)
                .unwrap()
                .0
                .is_empty()
        );
    }

    #[test]
    fn sftpgo_account_migration_uses_run_environment_even_without_env_directory() {
        let (_temp, state, mut r) = fixture("sftpgo");
        let destination = tempfile::tempdir().unwrap();
        let target = destination.path().join("new data");
        std::fs::create_dir_all(&r.data).unwrap();
        let database = r.data.join("accounts.db");
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute_batch("CREATE TABLE run_users(id INTEGER PRIMARY KEY,home_dir TEXT);
            CREATE TABLE run_folders(id INTEGER PRIMARY KEY,path TEXT);
            CREATE TABLE run_groups(id INTEGER PRIMARY KEY,user_settings TEXT);").unwrap();
        let old_home = crate::paths::portable_path_text(&r.data.join("home"));
        conn.execute("INSERT INTO run_users VALUES(1,?1)", [&old_home]).unwrap();
        drop(conn);
        let env = r.entry.run.as_mut().unwrap().env.as_mut().unwrap();
        env.insert("SFTPGO_DATA_PROVIDER__DRIVER".into(), "sqlite".into());
        env.insert("SFTPGO_DATA_PROVIDER__NAME".into(), "{data}/accounts.db".into());
        env.insert("SFTPGO_DATA_PROVIDER__SQL_TABLES_PREFIX".into(), "run_".into());
        std::fs::write(PathBuf::from(&r.inst.install_path).join(".niceenv-package.json"),
            serde_json::to_vec(&r.entry).unwrap()).unwrap();
        std::fs::write(r.etc.join("sftpgo.json"),
            r#"{"data_provider":{"driver":"mysql","name":"unused","sql_tables_prefix":"wrong_"}}"#).unwrap();
        assert!(!r.etc.join("env.d").exists());
        let before = std::fs::read(&database).unwrap();
        let migrated = crate::paths::copy_data_dir(&state.paths.base, &target).unwrap();
        let target = PathBuf::from(migrated.path);
        assert_eq!(std::fs::read(&database).unwrap(), before);
        let rebase = crate::paths::DataPathRebase::new(&state.paths.base, &target).unwrap();
        let migrated = rebase.path(&crate::paths::portable_path_text(&database));
        let conn = rusqlite::Connection::open(migrated).unwrap();
        assert_eq!(conn.query_row("SELECT home_dir FROM run_users", [], |r|r.get::<_,String>(0)).unwrap(),
            rebase.path(&old_home));
        drop(conn);
        // 配置文件声明本地库时，实际运行环境指定远程库仍必须拦截迁移。
        std::fs::write(r.etc.join("sftpgo.json"), r#"{"data_provider":{"driver":"sqlite"}}"#).unwrap();
        for from_file in [false, true] {
            let driver = if from_file { "postgresql" } else { "mysql" };
            let env = r.entry.run.as_mut().unwrap().env.as_mut().unwrap();
            if from_file {
                env.remove("SFTPGO_DATA_PROVIDER__DRIVER");
                std::fs::create_dir_all(r.etc.join("env.d")).unwrap();
                std::fs::write(r.etc.join("env.d/provider.env"), format!("SFTPGO_DATA_PROVIDER__DRIVER={driver}\n")).unwrap();
            } else {
                env.insert("SFTPGO_DATA_PROVIDER__DRIVER".into(), driver.into());
            }
            let snapshot_path = PathBuf::from(&r.inst.install_path).join(".niceenv-package.json");
            let snapshot = serde_json::to_vec(&r.entry).unwrap();
            std::fs::write(&snapshot_path, &snapshot).unwrap();
            let rejected = destination.path().join(format!("reject-{driver}"));
            assert_eq!(crate::paths::copy_data_dir(&state.paths.base, &rejected).unwrap_err().code,
                "DATA_DIR_SFTPGO_PROVIDER");
            assert!(!rejected.exists());
            assert_eq!(std::fs::read(&database).unwrap(), before);
            assert_eq!(std::fs::read(&snapshot_path).unwrap(), snapshot);
        }
    }

    #[test]
    fn sftpgo_env_migration_keeps_encoding_snapshot_secrets_and_resource_bytes() {
        fn normalized_path(path: &std::path::Path) -> std::path::PathBuf {
            let mut missing = Vec::new();
            let mut existing = path;
            while !existing.exists() {
                missing.push(
                    existing
                        .file_name()
                        .expect("missing path component")
                        .to_owned(),
                );
                existing = existing.parent().expect("missing path parent");
            }
            let mut normalized = existing.canonicalize().expect("canonicalize existing path");
            for component in missing.iter().rev() {
                normalized.push(component);
            }
            normalized
        }

        fn assert_same_path(actual: &str, expected: &std::path::Path) {
            assert_eq!(
                normalized_path(std::path::Path::new(actual)),
                normalized_path(expected)
            );
        }

        for bom in [
            vec![],
            vec![0xef, 0xbb, 0xbf],
            vec![0xff, 0xfe],
            vec![0xfe, 0xff],
        ] {
            let (_temp, state, mut r) = fixture("sftpgo");
            let destination = tempfile::tempdir().unwrap();
            let target = destination.path().join("新 data # [1]");
            let old = crate::paths::portable_path_text(&state.paths.base);
            let secret = format!("{old}/literal-secret");
            let resource = r.etc.join("resources/sftpgo.json");
            std::fs::create_dir_all(resource.parent().unwrap()).unwrap();
            std::fs::write(&resource, &secret).unwrap();
            let env_dir = r.etc.join("env.d");
            std::fs::create_dir(&env_dir).unwrap();
            let resource_text = crate::paths::portable_path_text(&resource);
            let text = format!("# retain comment\r\nBASE='{old}'\r\nSFTPGO_KMS__SECRETS__MASTER_KEY_PATH='{resource_text}'\r\nSFTPGO_DATA_PROVIDER__DRIVER=sqlite\r\nSFTPGO_DATA_PROVIDER__CONNECTION_STRING='{}'\r\nSFTPGO_SFTPD__HOST_KEYS='{resource_text}'\r\nSFTPGO_HTTPD__PASSWORD='${{BASE}} literal'\r\nSECRET=${{SFTPGO_KMS__SECRETS__MASTER_KEY_PATH}}\r\n", crate::configpaths::sqlite_file_uri(&state.paths.data().join("sftpgo/accounts.db")));
            let bytes = encode_sftpgo_env(&bom, &text);
            std::fs::write(env_dir.join("first.env"), &bytes).unwrap();
            std::fs::write(
                r.etc.join("sftpgo.json"),
                r#"{"data_provider":{"driver":"mysql"}}"#,
            )
            .unwrap();
            let env = r.entry.run.as_mut().unwrap().env.as_mut().unwrap();
            env.insert("NICEENV_SECRET".into(), "{etc}/literal-secret".into());
            env.insert("SFTPGO_FTPD__BANNER_FILE".into(), resource_text.clone());
            let snapshot_path = PathBuf::from(&r.inst.install_path).join(".niceenv-package.json");
            let snapshot = serde_json::to_vec(&r.entry).unwrap();
            std::fs::write(&snapshot_path, &snapshot).unwrap();
            let rebase = crate::paths::DataPathRebase::new(&state.paths.base, &target).unwrap();
            crate::paths::copy_data_dir(&state.paths.base, &target).unwrap();
            assert_eq!(std::fs::read(env_dir.join("first.env")).unwrap(), bytes);
            assert_eq!(std::fs::read(&snapshot_path).unwrap(), snapshot);
            let target_paths = Paths::new(target.clone());
            let target_dir = PathBuf::from(rebase.path(&crate::paths::portable_path_text(&r.etc)));
            let updated = sftpgo_env_files(&target_paths, &target_dir).unwrap();
            let environment = sftpgo_parse_env(&updated, Default::default()).unwrap();
            assert_eq!(environment["BASE"], old);
            assert_eq!(environment["SECRET"], resource_text);
            assert_same_path(
                &environment["SFTPGO_KMS__SECRETS__MASTER_KEY_PATH"],
                std::path::Path::new(&rebase.path(&resource_text)),
            );
            let sqlite_path = crate::configpaths::sqlite_connection_path(
                &environment["SFTPGO_DATA_PROVIDER__CONNECTION_STRING"],
            )
            .unwrap()
            .unwrap();
            assert_same_path(
                &sqlite_path.to_string_lossy(),
                &target.join("data/sftpgo/accounts.db"),
            );
            assert_eq!(
                std::fs::read(target_dir.join("resources/sftpgo.json")).unwrap(),
                secret.as_bytes()
            );
            let target_bytes = std::fs::read(target_dir.join("env.d/first.env")).unwrap();
            assert_eq!(target_bytes, encode_sftpgo_env(&bom, &updated[0].1));
            assert!(updated[0].1.starts_with("# retain comment\r\n"));
            let snapshot: serde_json::Value = serde_json::from_slice(
                &std::fs::read(rebase.path(&crate::paths::portable_path_text(&snapshot_path)))
                    .unwrap(),
            )
            .unwrap();
            let expected_secret = format!(
                "{}/literal-secret",
                crate::paths::portable_path_text(&r.etc)
            );
            assert_same_path(
                snapshot["run"]["env"]["NICEENV_SECRET"]
                    .as_str()
                    .expect("snapshot secret path"),
                std::path::Path::new(&expected_secret),
            );
            assert_same_path(
                snapshot["run"]["env"]["SFTPGO_FTPD__BANNER_FILE"]
                    .as_str()
                    .expect("snapshot banner path"),
                std::path::Path::new(&rebase.path(&resource_text)),
            );
        }
    }

    #[test]
    fn sftpgo_env_migration_rejects_conflicting_snapshots_without_publishing_target() {
        let (_temp, state, mut r) = fixture("sftpgo");
        let destination = tempfile::tempdir().unwrap();
        let target = destination.path().join("target");
        for directory in [r.etc.clone(), state.paths.etc_dir("sftpgo", "other")] {
            std::fs::create_dir_all(directory.join("env.d")).unwrap();
            std::fs::write(directory.join("sftpgo.json"), "{}").unwrap();
        }
        r.entry
            .run
            .as_mut()
            .unwrap()
            .env
            .as_mut()
            .unwrap()
            .insert("NICEENV_SECRET".into(), "{etc}/literal-secret".into());
        let snapshot_path = PathBuf::from(&r.inst.install_path).join(".niceenv-package.json");
        let snapshot = serde_json::to_vec(&r.entry).unwrap();
        std::fs::write(&snapshot_path, &snapshot).unwrap();
        assert_eq!(
            crate::paths::copy_data_dir(&state.paths.base, &target)
                .unwrap_err()
                .code,
            "DATA_DIR_ENV_CONFLICT"
        );
        assert!(!target.exists() || std::fs::read_dir(&target).unwrap().next().is_none());
        assert_eq!(std::fs::read(&snapshot_path).unwrap(), snapshot);
        assert_eq!(
            std::fs::read_to_string(r.etc.join("sftpgo.json")).unwrap(),
            "{}"
        );
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
        #[cfg(unix)]
        {
            std::fs::write(&env_file, "VALID=1\n").unwrap();
            std::fs::create_dir(env_dir.join("nested")).unwrap();
            std::fs::write(env_dir.join("nested/redirected.env"), "WRONG_FILE=1\n").unwrap();
            std::fs::write(env_dir.join(r"nested\redirected.env"), "ACTUAL_FILE=1\n").unwrap();
            // 托管相对路径不支持反斜杠时应拒绝，不能悄悄读取另一个目录的文件。
            assert!(sftpgo_env_files(&state.paths, &r.etc).is_err());
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
        let address: std::net::IpAddr = std::env::var("NSB_VERIFY_SFTPGO_BIND")
            .unwrap_or_else(|_| if with_env_directory { "::1" } else { "127.0.0.1" }.into()).parse().unwrap();
        let host = match address { std::net::IpAddr::V4(ip) => ip.to_string(), std::net::IpAddr::V6(ip) => format!("[{ip}]") };
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
            if with_env_directory && version == "2.7.5" {
                env.insert(
                    "SFTPGO_DATA_PROVIDER__CONNECTION_STRING".into(),
                    String::new(),
                );
            }
            std::fs::write(
                root.join(".niceenv-package.json"),
                serde_json::to_vec(&entry).unwrap(),
            )
            .unwrap();
            state
                .store
                .upsert_installed(&InstalledPackage {
                    id: "sftpgo".into(),
                    version: version.into(),
                    category: "ftp".into(),
                    install_path: root.to_string_lossy().into_owned(),
                    config_path: String::new(),
                    installed_at: 0,
                })
                .unwrap();
        };
        install("2.7.5", &old_source);
        let legacy_dir = state.paths.etc_dir("sftpgo", "2.7.5"); std::fs::create_dir_all(&legacy_dir).unwrap();
        let mut config: serde_json::Value = serde_json::from_slice(&std::fs::read(old_source.join("sftpgo.json")).unwrap()).unwrap();
        config["sftpd"]["bindings"][0]["address"] = host.clone().into();
        config["httpd"]["bindings"][0]["address"] = host.clone().into();
        config["common"]["idle_timeout"] = 17.into();
        if with_env_directory {
            config["data_provider"]["driver"] = "sqlite".into();
        }
        config["httpd"]["web_root"] = "/niceenv-console".into();
        let encode_config = |config: &serde_json::Value| {
            if with_env_directory {
                return serde_json::to_vec_pretty(config).unwrap();
            }
            fn uppercase(value: &mut serde_json::Value) {
                match value {
                    serde_json::Value::Object(values) => {
                        *values = std::mem::take(values)
                            .into_iter()
                            .map(|(key, mut value)| {
                                uppercase(&mut value);
                                (key.to_uppercase(), value)
                            })
                            .collect();
                    }
                    serde_json::Value::Array(values) => values.iter_mut().for_each(uppercase),
                    _ => {}
                }
            }
            let mut upper = config.clone();
            uppercase(&mut upper);
            let provider = upper
                .as_object_mut()
                .unwrap()
                .remove("DATA_PROVIDER")
                .unwrap();
            format!(
                "Provider_Defaults: &provider {}\nDATA_PROVIDER:\n  <<: *provider\n{}",
                serde_json::to_string(&provider).unwrap(),
                yaml_serde::to_string(&upper).unwrap()
            )
            .into_bytes()
        };
        let original_config = encode_config(&config);
        let config_path = legacy_dir.join(if with_env_directory {
            "sftpgo.json"
        } else {
            "sftpgo.yaml"
        });
        std::fs::write(&config_path, &original_config).unwrap();
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
            std::fs::write(directory.join("10-first.env"), first.replace("ADDRESS: 127.0.0.1", &format!("ADDRESS: {host}"))).unwrap();
        }
        let base = (22000..42000).find(|port| [0, 1, 6058, 6059].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        let occupied_web = std::net::TcpListener::bind((address, base + 6058)).unwrap();
        state.store.set_port_override("sftpgo", Some(base)).unwrap();
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("sftpgo"); } }
        let _cleanup = Cleanup(&state);
        state.start_service("sftpgo").unwrap_or_else(|e| panic!("{e:?}\n{:?}", state.manager.tail("sftpgo", 20)));
        let first = state.manager.snapshot("sftpgo").unwrap().port.unwrap(); assert_ne!(first, base);
        let client = reqwest::blocking::Client::builder().no_proxy().pool_max_idle_per_host(0).timeout(Duration::from_secs(5)).build().unwrap();
        let token = |port: u16| {
            let response = client.get(format!("http://{host}:{}/api/v2/token", port + 6058)).basic_auth("fixture", Some(&password)).send().unwrap();
            assert!(response.status().is_success(), "token status: {}", response.status());
            response.json::<serde_json::Value>().unwrap()["access_token"].as_str().unwrap().to_string()
        };
        let access = token(first);
        let user_home = state.paths.data().join("sftpgo/user-files");
        let response = client.post(format!("http://{host}:{}/api/v2/users", first + 6058)).bearer_auth(&access)
            .json(&serde_json::json!({"username":"native-user","password":password,"status":1,"home_dir":user_home.to_string_lossy(),"permissions":{"/":["*"]}})).send().unwrap();
        assert!(response.status().is_success(), "create user status: {}", response.status());
        let web_url = state.service_web_url("sftpgo").unwrap();
        assert_eq!(web_url, format!("http://{host}:{}{console_path}/web/admin", first + 6058));
        let admin = client.get(&web_url).send().unwrap();
        assert!(admin.status().is_success()); assert!(admin.text().unwrap().to_ascii_lowercase().contains("<html"));
        // 修改文件中的待生效路径和计划端口，不得改变当前进程的入口。
        config["httpd"]["web_root"] = "/next-start".into();
        std::fs::write(&config_path, encode_config(&config)).unwrap();
        state.store.set_port_override("sftpgo", Some(first + 10)).unwrap();
        assert_eq!(state.service_web_url("sftpgo").unwrap(), web_url);
        std::fs::write(&config_path, &original_config).unwrap();
        let fingerprint = crate::certdeploy::probe_ssh(&address.to_string(), first).unwrap().fingerprint;
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
                    let mut handle = russh::client::connect(Arc::new(russh::client::Config::default()), (address, port), VerifyHost(fingerprint.clone())).await.unwrap();
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
            let response = client.post(format!("http://{host}:{}/api/v2/users", alternate_port + 6058)).bearer_auth(token(alternate_port))
                .json(&serde_json::json!({"username":"alternate-only","password":password,"status":1,
                    "home_dir":state.paths.data().join("sftpgo/alternate-files").to_string_lossy(),"permissions":{"/":["*"]}})).send().unwrap();
            assert!(response.status().is_success());
            assert_eq!(state.select_sftpgo_config("etc/sftpgo/2.7.5", "2.7.5", Some("etc/sftpgo/archive")).unwrap_err().code, "SERVICE_BUSY");
            state.stop_service("sftpgo").unwrap();
            state.select_sftpgo_config("etc/sftpgo/2.7.5", "2.7.5", Some("etc/sftpgo/archive")).unwrap();
            assert!(alternate.join(database_name).is_file());
        }
        if with_env_directory {
            let connection = format!(
                "{}?mode=rw&cache=shared&_foreign_keys=1",
                crate::configpaths::sqlite_file_uri(&legacy_dir.join(database_name))
            );
            std::fs::write(
                legacy_dir.join("env.d/40-connection.env"),
                format!("SFTPGO_DATA_PROVIDER__CONNECTION_STRING='{connection}'\n"),
            )
            .unwrap();
        }
        install("2.7.6", &new_source); state.set_active_version("sftpgo", "2.7.6").unwrap();
        let requested = (base + 20..42000).find(|port| [0, 6058].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        let occupied_sftp = std::net::TcpListener::bind((address, requested)).unwrap();
        state.store.set_port_override("sftpgo", Some(requested)).unwrap();
        state.start_service("sftpgo").unwrap_or_else(|e| panic!("{e:?}\n{:?}", state.manager.tail("sftpgo", 20)));
        let second = state.manager.snapshot("sftpgo").unwrap().port.unwrap(); assert_ne!(second, requested);
        assert_eq!(state.service_web_url("sftpgo").unwrap(), format!("http://{host}:{}{console_path}/web/admin", second + 6058));
        assert_eq!(state.manager.snapshot("sftpgo").unwrap().version.as_deref(), Some("2.7.6"));
        assert_eq!(crate::certdeploy::probe_ssh(&address.to_string(), second).unwrap().fingerprint, fingerprint);
        let response = client.get(format!("http://{host}:{}/api/v2/users/native-user", second + 6058)).bearer_auth(token(second)).send().unwrap();
        assert!(response.status().is_success()); transfer(second, false);
        if with_env_directory {
            let response = client.get(format!("http://{host}:{}/api/v2/users/alternate-only", second + 6058)).bearer_auth(token(second)).send().unwrap();
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
    #[ignore = "requires NSB_VERIFY_RUSTFS pointing to the verified RustFS 1.0.0 executable; isolated ports and data only"]
    fn native_rustfs_legacy_snapshot_starts_with_spaces_in_data_path() {
        let executable = std::env::var_os("NSB_VERIFY_RUSTFS").expect("set NSB_VERIFY_RUSTFS");
        let (_temp, state, mut r) = fixture("rustfs");
        assert!(r.data.to_string_lossy().contains(' '), "fixture must exercise paths with spaces");
        std::fs::copy(executable, &r.bin).unwrap();
        // 模拟升级前保存的默认快照，确认无需重装即可修复；定制描述保持不变。
        let run = r.entry.run.as_mut().unwrap();
        run.cwd = None;
        *run.args.last_mut().unwrap() = "{data}/rustfs-data".into();
        let snapshot = PathBuf::from(&r.inst.install_path).join(".niceenv-package.json");
        let mut custom = r.entry.clone();
        custom.run.as_mut().unwrap().cwd = Some("{data}/custom".into());
        std::fs::write(&snapshot, serde_json::to_vec(&custom).unwrap()).unwrap();
        assert_eq!(state.installer.installed_entry(&r.inst).run.unwrap().cwd, Some("{data}/custom".into()));
        std::fs::write(&snapshot, serde_json::to_vec(&r.entry).unwrap()).unwrap();
        let base = (31000..42000).find(|p| tcp_port_bindable(*p) && tcp_port_bindable(*p + 1)).unwrap();
        state.store.set_port_override("rustfs", Some(base)).unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("rustfs"); } }
        let _cleanup = Cleanup(&state);
        let client = reqwest::blocking::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build().unwrap();
        for _ in 0..2 {
            state.start_service("rustfs").unwrap();
            let port = state.manager.snapshot("rustfs").unwrap().port.unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !client.get(format!("http://127.0.0.1:{port}/health/live")).send().is_ok_and(|r| r.status().is_success()) {
                assert!(std::time::Instant::now() < deadline, "RustFS HTTP health must be ready");
                std::thread::sleep(Duration::from_millis(100));
            }
            assert!(r.data.join("rustfs-data/.rustfs.sys").is_dir());
            let pids = state.manager.snapshot("rustfs").unwrap().pids;
            state.stop_service("rustfs").unwrap();
            assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        }
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
    #[ignore = "requires verified NSB_VERIFY_MINIO_OLD, NSB_VERIFY_MINIO_NEW and NSB_VERIFY_CADDY executables"]
    fn native_minio_objects_and_console_survive_restart_upgrade_and_reinstall() { verify_native_minio(false); }

    #[test]
    #[ignore = "requires verified NSB_VERIFY_MINIO_OLD, NSB_VERIFY_MINIO_NEW and NSB_VERIFY_CADDY executables"]
    fn native_minio_tls_and_disabled_console_keep_s3_available() { verify_native_minio(true); }

    fn verify_native_minio(tls: bool) {
        use hmac::{KeyInit, Mac};
        use sha2_11::{Digest, Sha256};
        let old_program = std::env::var_os("NSB_VERIFY_MINIO_OLD").expect("set NSB_VERIFY_MINIO_OLD");
        let new_program = std::env::var_os("NSB_VERIFY_MINIO_NEW").expect("set NSB_VERIFY_MINIO_NEW");
        let caddy_program = std::env::var_os("NSB_VERIFY_CADDY").expect("set NSB_VERIFY_CADDY");
        let proxy_socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let proxy_port = proxy_socket.local_addr().unwrap().port(); drop(proxy_socket);
        let scheme = if tls { "https" } else { "http" };
        let public_url = format!("{scheme}://127.0.0.1:{proxy_port}/niceenv/");
        let (_temp, state, mut r) = fixture_version("minio", Some("RELEASE.2025-07-23T15-54-02Z"));
        std::fs::copy(old_program, &r.bin).unwrap();
        let password = format!("fixture-{}", rand::random::<u64>()); let user = "niceenv-fixture";
        r.spec.args.extend(["--certs-dir".into(), "{data}/certs".into(), "--config-dir".into(), "{etc}".into()]);
        let env = r.spec.env.as_mut().unwrap();
        for name in ["MINIO_CONFIG", "MINIO_VOLUMES", "MINIO_ENDPOINTS", "MINIO_SERVER_URL", "MINIO_BROWSER_REDIRECT_URL", "CONSOLE_SUBPATH",
            "MINIO_ROOT_USER_FILE", "MINIO_ROOT_PASSWORD_FILE", "MINIO_ACCESS_KEY_FILE", "MINIO_SECRET_KEY_FILE"] { env.insert(name.into(), "".into()); }
        env.insert("MINIO_ROOT_USER".into(), user.into()); env.insert("MINIO_ROOT_PASSWORD".into(), password.clone());
        env.insert("MINIO_BROWSER".into(), "off".into());
        env.insert("MINIO_CONFIG_ENV_FILE".into(), "{data}/minio.env".into());
        let environment_file = r.data.join("minio.env");
        std::fs::write(&environment_file, format!("# preserved native configuration\r\nMINIO_BROWSER=off\r\nexport MINIO_BROWSER='on'\r\nMINIO_BROWSER_REDIRECT_URL='{public_url}'\r\n")).unwrap();
        let certs = r.data.join("certs"); std::fs::create_dir_all(&certs).unwrap();
        if tls {
            let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into(), "localhost".into()]).unwrap();
            std::fs::write(certs.join("public.crt"), certificate.cert.pem()).unwrap();
            std::fs::write(certs.join("private.key"), certificate.key_pair.serialize_pem()).unwrap();
        }
        let save_run = |entry: &PackageManifestEntry, root: &std::path::Path| {
            let mut entry = entry.clone(); entry.run = Some(r.spec.clone());
            std::fs::write(root.join(".niceenv-package.json"), serde_json::to_vec(&entry).unwrap()).unwrap();
        };
        save_run(&r.entry, &r.root);
        let base = (34000..42000).find(|port| [0, 1, 2].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        let occupied_console = std::net::TcpListener::bind(("127.0.0.1", base + 1)).unwrap();
        state.store.set_port_override("minio", Some(base)).unwrap(); state.store.set_setting("autoFallbackPort", "true").unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("minio"); } }
        let _cleanup = Cleanup(&state);
        let start = || { let result = state.start_service("minio"); assert!(result.is_ok(), "{result:?}\n{:?}", state.manager.tail("minio", 30)); };
        let client = reqwest::blocking::Client::builder().no_proxy().danger_accept_invalid_certs(true).timeout(Duration::from_secs(8)).build().unwrap();
        let sign = |key: &[u8], bytes: &[u8]| {
            let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).unwrap(); mac.update(bytes); mac.finalize().into_bytes().to_vec()
        };
        let s3 = |method: reqwest::Method, path: &str, bytes: &[u8]| {
            let port = state.manager.snapshot("minio").unwrap().port.unwrap(); let host = format!("127.0.0.1:{port}");
            let now = chrono::Utc::now(); let date = now.format("%Y%m%d").to_string(); let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
            let payload = hex::encode(Sha256::digest(bytes)); let signed = "host;x-amz-content-sha256;x-amz-date";
            let canonical = format!("{method}\n{path}\n\nhost:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{timestamp}\n\n{signed}\n{payload}");
            let scope = format!("{date}/us-east-1/s3/aws4_request");
            let message = format!("AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}", hex::encode(Sha256::digest(canonical.as_bytes())));
            let key = sign(format!("AWS4{password}").as_bytes(), date.as_bytes()); let key = sign(&key, b"us-east-1");
            let key = sign(&key, b"s3"); let key = sign(&key, b"aws4_request"); let signature = hex::encode(sign(&key, message.as_bytes()));
            client.request(method, format!("{scheme}://{host}{path}")).header("x-amz-date", timestamp).header("x-amz-content-sha256", payload)
                .header("Authorization", format!("AWS4-HMAC-SHA256 Credential={user}/{scope}, SignedHeaders={signed}, Signature={signature}"))
                .body(bytes.to_vec()).send().unwrap()
        };
        let contents = "原生 S3 对象：重启、升级及重装后保留\n".as_bytes();
        let read_object = || { assert_eq!(s3(reqwest::Method::GET, "/niceenv-fixture/preserved.txt", &[]).error_for_status().unwrap().bytes().unwrap().as_ref(), contents); };
        let verify_console = || {
            let port = state.manager.snapshot("minio").unwrap().port.unwrap();
            // 真实 Caddy 剥离子路径并转发静态资源和登录 API；代理端口不属于 MinIO。
            let proxy_config = r.data.join("Caddyfile");
            let tls_config = if tls { format!("tls \"{}\" \"{}\"", crate::paths::nginx_path(&certs.join("public.crt")), crate::paths::nginx_path(&certs.join("private.key"))) } else { String::new() };
            let transport = if tls { "transport http {\n tls_insecure_skip_verify\n }" } else { "" };
            std::fs::write(&proxy_config, format!("{{\n admin off\n auto_https off\n persist_config off\n}}\n{scheme}://127.0.0.1:{proxy_port} {{\n {tls_config}\n handle_path /niceenv/* {{\n reverse_proxy {scheme}://127.0.0.1:{} {{\n {transport}\n }}\n }}\n}}\n", port + 1)).unwrap();
            struct Proxy(std::process::Child);
            impl Drop for Proxy { fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); } }
            let mut proxy = Proxy(platform::command(&caddy_program).args(["run", "--adapter", "caddyfile", "--config"]).arg(&proxy_config)
                .env("XDG_CONFIG_HOME", r.data.join("proxy-config")).env("XDG_DATA_HOME", r.data.join("proxy-data"))
                .stdout(std::process::Stdio::null()).stderr(std::fs::File::create(r.data.join("proxy.log")).unwrap()).spawn().unwrap());
            assert!(wait_healthy(proxy_port, Duration::from_secs(5)), "{}", std::fs::read_to_string(r.data.join("proxy.log")).unwrap());
            assert!(proxy.0.try_wait().unwrap().is_none());
            let console = state.service_web_url("minio").unwrap(); assert_eq!(console, public_url);
            let html = client.get(&console).send().unwrap().error_for_status().unwrap().text().unwrap(); assert!(html.contains("MinIO"));
            let script = regex::Regex::new(r#"src="([^"]+\.js(?:\?[^"]*)?)""#).unwrap();
            let asset = script.captures_iter(&html).last().expect("real MinIO console script");
            let asset_url = reqwest::Url::parse(&console).unwrap().join(&asset[1]).unwrap();
            let javascript = client.get(asset_url.clone()).send().unwrap().error_for_status().unwrap();
            assert!(!javascript.headers().get("content-type").unwrap().to_str().unwrap().contains("text/html"),
                "MinIO asset {asset_url} returned HTML");
            assert!(javascript.bytes().unwrap().len() > 1000);
            let login = client.post(format!("{console}api/v1/login")).json(&serde_json::json!({"accessKey":user,"secretKey":password})).send().unwrap();
            assert!(login.status().is_success(), "{}", login.text().unwrap());
        };
        start(); assert_ne!(state.manager.snapshot("minio").unwrap().port, Some(base));
        s3(reqwest::Method::PUT, "/niceenv-fixture", &[]).error_for_status().unwrap();
        s3(reqwest::Method::PUT, "/niceenv-fixture/preserved.txt", contents).error_for_status().unwrap(); read_object(); verify_console();
        let original_env = std::fs::read(&environment_file).unwrap(); std::fs::write(&environment_file, "MINIO_BROWSER=off\n").unwrap();
        assert_eq!(state.service_web_url("minio").unwrap(), public_url); std::fs::write(&environment_file, original_env).unwrap();
        state.restart_service("minio").unwrap(); read_object(); verify_console(); state.stop_service("minio").unwrap();
        let new_version = "RELEASE.2025-09-07T16-13-09Z"; let key = format!("minio@{new_version}");
        std::fs::copy(&new_program, state.paths.downloads().join(format!("{key}.pkg"))).unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap(); runtime.block_on(state.install_package(&key)).unwrap();
        let installed = state.store.find_installed("minio", Some(new_version)).unwrap(); let new_entry = state.installer.installed_entry(&installed);
        save_run(&new_entry, std::path::Path::new(&installed.install_path)); state.set_active_version("minio", new_version).unwrap();
        start(); read_object(); verify_console();
        let pids = state.manager.snapshot("minio").unwrap().pids;
        state.uninstall_package("minio@RELEASE.2025-07-23T15-54-02Z").unwrap(); assert_eq!(state.manager.snapshot("minio").unwrap().pids, pids);
        state.uninstall_package(&key).unwrap(); assert!(pids.iter().all(|pid| !platform::process_alive(*pid))); assert!(r.data.join("data/niceenv-fixture").is_dir());
        std::fs::copy(&new_program, state.paths.downloads().join(format!("{key}.pkg"))).unwrap(); runtime.block_on(state.install_package(&key)).unwrap();
        save_run(&new_entry, std::path::Path::new(&installed.install_path)); start(); read_object(); verify_console(); state.stop_service("minio").unwrap();
        std::fs::write(&environment_file, "MINIO_BROWSER=off\n").unwrap();
        state.store.set_port_override("minio", Some(base)).unwrap(); start();
        assert_eq!(state.manager.snapshot("minio").unwrap().port, Some(base)); read_object();
        assert!(state.service_web_url("minio").unwrap_err().hint.unwrap().contains("已关闭"));
        state.stop_service("minio").unwrap();
        // 未配置代理时直接访问根入口，HTTPS 由实际本机控制台确认。
        std::fs::write(&environment_file, "MINIO_BROWSER=on\n").unwrap(); start(); read_object();
        let port = state.manager.snapshot("minio").unwrap().port.unwrap();
        let direct = state.service_web_url("minio").unwrap(); assert_eq!(direct, format!("{scheme}://127.0.0.1:{}/", port + 1));
        assert!(client.get(&direct).send().unwrap().error_for_status().unwrap().text().unwrap().contains("MinIO"));
        assert!(client.post(format!("{direct}api/v1/login")).json(&serde_json::json!({"accessKey":user,"secretKey":password})).send().unwrap().status().is_success());
        state.stop_service("minio").unwrap();
        std::fs::write(&environment_file, "MINIO_BROWSER=invalid-value\n").unwrap();
        assert_eq!(state.start_service("minio").unwrap_err().code, "MINIO_ENV_INVALID");
        assert!(state.manager.snapshot("minio").unwrap().pids.is_empty()); drop(occupied_console);
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
        assert!(rnacos_http_target(&state.manager, &r, 0, "/").is_err());
        // 只登记当前检查进程为监听所有者，验证归属判断本身。
        state.manager.adopt("rnacos", &[std::process::id()], Some(base));
        assert!(rnacos_ports_ready(&state.manager, &r));
        assert_eq!(rnacos_http_target(&state.manager, &r, 0, "/").unwrap(), format!("http://127.0.0.1:{base}/"));
        assert_eq!(rnacos_http_target(&state.manager, &r, 2000, "/rnacos/").unwrap(), format!("http://127.0.0.1:{}/rnacos", base + 2000));
        drop(listeners.pop());
        assert!(!rnacos_ports_ready(&state.manager, &r));
        assert!(rnacos_http_target(&state.manager, &r, 2000, "/rnacos/").is_err());
        let ipv6_console = std::net::TcpListener::bind(("::1", base + 2000)).unwrap();
        assert!(rnacos_ports_ready(&state.manager, &r));
        assert_eq!(rnacos_http_target(&state.manager, &r, 2000, "/rnacos/").unwrap(), format!("http://[::1]:{}/rnacos", base + 2000));
        assert!(generic_web_target(&r, None).is_err()); // 未启动时不能猜测控制台地址。
        drop(ipv6_console);
        state.manager.push_log("rnacos", "thread panicked at old run");
        state.manager.push_log("rnacos", RNACOS_START_MARKER);
        assert!(!rnacos_startup_panicked(&state.manager, "rnacos"));
        state.manager.push_log("rnacos", "thread panicked at this run");
        assert!(rnacos_startup_panicked(&state.manager, "rnacos"));
        assert!(!rnacos_panic_error(&state.manager, &r).hint.unwrap().contains("PERSISTENCE_ENABLE"));
        for line in 421..=424 {
            state.manager.push_log("rnacos", &format!("thread '<unnamed>' panicked at src\\raft\\filestore\\raftapply.rs:{line}:52:"));
            let error = rnacos_panic_error(&state.manager, &r);
            assert_eq!(error.code, "SERVICE_RUNTIME_PANIC");
            assert_eq!(error.hint.unwrap().contains("PERSISTENCE_ENABLE=false"), cfg!(windows));
        }
        r.entry.version = "0.8.8".into();
        assert!(!rnacos_panic_error(&state.manager, &r).hint.unwrap().contains("PERSISTENCE_ENABLE"));
        r.entry.version = "0.8.7".into();
        state.manager.push_log("rnacos", RNACOS_START_MARKER);
        assert!(!rnacos_startup_panicked(&state.manager, "rnacos"));
        assert!(!rnacos_panic_error(&state.manager, &r).hint.unwrap().contains("PERSISTENCE_ENABLE"));
        state.manager.push_log("rnacos", "thread panicked at src/raft/filestore/raftapply.rs:200:20:");
        assert!(!rnacos_panic_error(&state.manager, &r).hint.unwrap().contains("PERSISTENCE_ENABLE"));
        state.manager.services.lock().remove("rnacos");
        drop(listeners);
        for (state, leader, applied, ready) in [
            ("Leader", Some(1), 2, true), ("Follower", Some(2), 2, true),
            ("Learner", None, 0, false), ("Candidate", None, 2, false),
            ("Leader", None, 2, false), ("Leader", Some(2), 2, false),
            ("Follower", Some(1), 2, false), ("Leader", Some(1), 0, false),
        ] {
            assert_eq!(rnacos_raft_metrics_ready(&serde_json::json!({"id":1,"state":state,"current_leader":leader,"last_applied":applied})), ready);
        }
        assert!(!rnacos_raft_metrics_ready(&serde_json::json!({})));
    }

    #[test]
    #[ignore = "requires NSB_VERIFY_RNACOS (0.8.7) and NSB_VERIFY_RNACOS_PREVIOUS (0.8.6) official executables"]
    fn native_rnacos_loads_config_keeps_data_and_moves_all_ports_together() {
        for (version, variable) in [("0.8.6", "NSB_VERIFY_RNACOS_PREVIOUS"), ("0.8.7", "NSB_VERIFY_RNACOS")] {
            verify_native_rnacos_config(std::env::var_os(variable).expect(variable), version);
        }
    }

    fn verify_native_rnacos_config(executable: std::ffi::OsString, version: &str) {
        let (_temp, state, mut r) = fixture_version("rnacos", Some(version));
        std::fs::copy(executable, &r.bin).unwrap();
        let sdk_host = std::env::var("NSB_VERIFY_RNACOS_BIND").unwrap_or_else(|_| "[::1]".into());
        let console_host = std::env::var("NSB_VERIFY_RNACOS_CONSOLE_HOST").unwrap_or_else(|_| sdk_host.clone());
        let sdk_ip: std::net::IpAddr = sdk_host.trim_matches(['[', ']']).parse().unwrap();
        let console_ip = if console_host == "localhost" { "127.0.0.1" } else { console_host.trim_matches(['[', ']']) }.parse::<std::net::IpAddr>().unwrap();
        let bindable = |port| tcp_port_bindable(port) && std::net::TcpListener::bind((sdk_ip, port)).is_ok()
            && std::net::TcpListener::bind((console_ip, port)).is_ok();
        let base = (22000..42000).find(|port| [0, 1, 1000, 1001, 2000, 2001].iter().all(|offset| bindable(port + offset))).unwrap();
        let occupied_grpc = std::net::TcpListener::bind((sdk_ip, base + 1000)).unwrap();
        state.store.set_port_assign("rnacos", base).unwrap();
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        r.port = Some(base);
        let config = r.etc.join(".env");
        let password = format!("fixture-{}", rand::random::<u64>());
        let content = format!("{}\n# native fixture\nNSB_FIXTURE_RNACOS_HOST='{sdk_host}'\nRNACOS_SDK_HOST=${{NSB_FIXTURE_RNACOS_HOST}}\nRNACOS_CONSOLE_HOST={console_host}\nRNACOS_HTTP_WORKERS=1\nRNACOS_NAMING_INSTANCE_METADATA_PERSISTENCE_ENABLE=false\nRNACOS_ENABLE_OPEN_API_AUTH=false\nRNACOS_INIT_ADMIN_USERNAME=fixture\nRNACOS_INIT_ADMIN_PASSWORD={password}\n", expand_config(r.spec.config_template.as_deref().unwrap(), &r).unwrap());
        std::fs::write(&config, content).unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("rnacos"); } }
        let _cleanup = Cleanup(&state);
        state.start_service("rnacos").unwrap();
        let port = state.manager.snapshot("rnacos").unwrap().port.unwrap();
        assert_ne!(port, base);
        let client = reqwest::blocking::Client::builder().no_proxy().pool_max_idle_per_host(0).timeout(Duration::from_secs(3)).build().unwrap();
        let url = local_web_url(&sdk_host, port, false, "/nacos/v1/cs/configs").unwrap();
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
        let console_url = state.service_web_url("rnacos").unwrap();
        assert_eq!(console_url, local_web_url(&console_host, port + 2000, false, "/rnacos").unwrap());
        let console = client.get(&console_url).send().unwrap();
        assert!(console.status().is_success());
        let html = console.text().unwrap();
        assert!(html.to_ascii_lowercase().contains("<html"));
        let script = regex::Regex::new(r#"src="([^"]+\.js)""#).unwrap().captures(&html).expect("console must load real JavaScript")[1].to_string();
        let script = client.get(reqwest::Url::parse(&console_url).unwrap().join(&script).unwrap()).send().unwrap().error_for_status().unwrap();
        assert!(script.headers()[reqwest::header::CONTENT_TYPE].to_str().unwrap().contains("javascript"));
        assert!(script.text().unwrap().len() > 100);
        let running_config = std::fs::read_to_string(&config).unwrap();
        std::fs::write(&config, running_config.replace(&format!("RNACOS_CONSOLE_HOST={console_host}"), "RNACOS_CONSOLE_HOST=192.0.2.1")).unwrap();
        assert_eq!(state.service_web_url("rnacos").unwrap(), console_url);
        std::fs::write(&config, &running_config).unwrap();
        assert!(r.data.join("nacos_db").is_dir());
        assert!(!r.root.join("nacos_db").exists());
        let pids = state.manager.snapshot("rnacos").unwrap().pids;
        state.stop_service("rnacos").unwrap();
        assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        assert_eq!(state.service_web_url("rnacos").unwrap_err().code, "SERVICE_NOT_RUNNING");
        // 换一组未使用过的端口制造第二次冲突，不把 Windows TCP 释放延迟当作产品错误。
        let requested = (base + 10..42000).find(|port| [0, 1000, 2000].iter().all(|offset| bindable(port + offset))).unwrap();
        state.store.set_port_override("rnacos", Some(requested)).unwrap();
        let occupied_console = std::net::TcpListener::bind((console_ip, requested + 2000)).unwrap();
        state.start_service("rnacos").unwrap();
        let second = state.manager.snapshot("rnacos").unwrap().port.unwrap();
        assert_ne!(second, requested);
        let url = local_web_url(&sdk_host, second, false, "/nacos/v1/cs/configs").unwrap();
        assert_eq!(state.service_web_url("rnacos").unwrap(), local_web_url(&console_host, second + 2000, false, "/rnacos").unwrap());
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
        std::fs::write(&config, content.replace("RNACOS_ENABLE_OPEN_API_AUTH=false", "RNACOS_ENABLE_OPEN_API_AUTH=true")
            .replace(&format!("RNACOS_CONSOLE_HOST={console_host}"), "RNACOS_CONSOLE_HOST=192.0.2.1")).unwrap();
        // 运行描述环境覆盖 .env，控制台切换地址后入口必须来自本次真实监听。
        let mut entry = r.entry.clone();
        entry.run.as_mut().unwrap().env.as_mut().unwrap().insert("RNACOS_CONSOLE_HOST".into(), "127.0.0.1".into());
        std::fs::write(r.root.join(".niceenv-package.json"), serde_json::to_vec(&entry).unwrap()).unwrap();
        state.start_service("rnacos").unwrap();
        let third = state.manager.snapshot("rnacos").unwrap().port.unwrap();
        assert_eq!(state.store.get_port_assign("rnacos"), Some(third));
        assert_eq!(state.service_web_url("rnacos").unwrap(), format!("http://127.0.0.1:{}/rnacos", third + 2000));
        assert!(std::fs::read_to_string(&config).unwrap().contains("RNACOS_CONSOLE_HOST=192.0.2.1"));
        let url = local_web_url(&sdk_host, third, false, "/nacos/v1/cs/configs").unwrap();
        let response = client.get(&url).query(&[("dataId", "niceenv-fixture"), ("group", "DEFAULT_GROUP")]).send().unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        let login = client.post(local_web_url(&sdk_host, third, false, "/nacos/v1/auth/login").unwrap())
            .form(&[("username", "fixture"), ("password", password.as_str())]).send().unwrap().error_for_status().unwrap()
            .json::<serde_json::Value>().unwrap();
        let token = login["accessToken"].as_str().expect("authenticated login must issue a token");
        let response = client.get(&url)
            .query(&[("dataId", "niceenv-fixture"), ("group", "DEFAULT_GROUP"), ("accessToken", token)])
            .send().unwrap().error_for_status().unwrap();
        assert_eq!(response.text().unwrap(), "persisted-fixture");
        assert!(std::fs::read_to_string(&config).unwrap().contains("RNACOS_NAMING_INSTANCE_METADATA_PERSISTENCE_ENABLE=false"));
        state.stop_service("rnacos").unwrap();
        assert_native_rnacos_stopped(&state, third);
        // 控制台线程失败时 SDK 仍可能启动，必须报错并清理整个临时服务。
        let mut entry = r.entry.clone(); entry.run.as_mut().unwrap().health_timeout_sec = 3;
        std::fs::write(r.root.join(".niceenv-package.json"), serde_json::to_vec(&entry).unwrap()).unwrap();
        let error = state.start_service("rnacos").unwrap_err();
        assert!(matches!(error.code.as_str(), "SERVICE_START_TIMEOUT" | "SERVICE_RUNTIME_PANIC"), "{error:?}");
        let failed_port = state.store.get_port_assign("rnacos").unwrap();
        assert_native_rnacos_stopped(&state, failed_port);
        drop(occupied_console); drop(occupied_grpc);
    }

    #[test]
    #[ignore = "requires NSB_VERIFY_RNACOS_BROKEN pointing to the official r-nacos 0.8.7 Windows executable"]
    fn native_rnacos_upstream_panic_cannot_report_a_healthy_service() {
        let executable = std::env::var_os("NSB_VERIFY_RNACOS_BROKEN").expect("set NSB_VERIFY_RNACOS_BROKEN");
        verify_native_rnacos_without_leader(&executable);
        // 初始化竞态不保证每次触发；成功必须能读写，失败必须完整清理且不修改配置。
        for _ in 0..3 { verify_native_rnacos_startup_recovery(&executable); }
    }

    fn verify_native_rnacos_without_leader(executable: &std::ffi::OsStr) {
        let (_temp, state, mut r) = fixture_version("rnacos", Some("0.8.7"));
        std::fs::copy(executable, &r.bin).unwrap();
        let port = (42000..44000).find(|port| [0, 1000, 2000].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        state.store.set_port_override("rnacos", Some(port)).unwrap(); r.port = Some(port);
        let content = format!("{}\nRNACOS_SDK_HOST=127.0.0.1\nRNACOS_CONSOLE_HOST=127.0.0.1\nRNACOS_HTTP_WORKERS=1\nRNACOS_ENABLE_OPEN_API_AUTH=true\nRNACOS_RAFT_AUTO_INIT=false\nRNACOS_NAMING_INSTANCE_METADATA_PERSISTENCE_ENABLE=false\n", expand_config(r.spec.config_template.as_deref().unwrap(), &r).unwrap());
        std::fs::write(r.etc.join(".env"), &content).unwrap();
        let mut entry = r.entry.clone(); entry.run.as_mut().unwrap().health_timeout_sec = 15;
        std::fs::write(r.root.join(".niceenv-package.json"), serde_json::to_vec(&entry).unwrap()).unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("rnacos"); } }
        let _cleanup = Cleanup(&state);
        // /health 在初始宽限期返回 success，受保护的 metrics 返回 403；不能提前放行。
        let error = state.start_service("rnacos").unwrap_err();
        assert_eq!(error.code, "SERVICE_START_TIMEOUT", "{error:?}");
        assert!(error.message.contains("Raft"));
        assert_eq!(std::fs::read_to_string(r.etc.join(".env")).unwrap(), content);
        assert_native_rnacos_stopped(&state, port);
    }

    fn verify_native_rnacos_startup_recovery(executable: &std::ffi::OsStr) {
        let (_temp, state, mut r) = fixture_version("rnacos", Some("0.8.7"));
        std::fs::copy(executable, &r.bin).unwrap();
        let port = (30000..42000).find(|port| [0, 1000, 2000].iter().all(|offset| tcp_port_bindable(port + offset))).unwrap();
        state.store.set_port_override("rnacos", Some(port)).unwrap(); r.port = Some(port);
        let content = format!("{}\nRNACOS_SDK_HOST=127.0.0.1\nRNACOS_CONSOLE_HOST=127.0.0.1\nRNACOS_HTTP_WORKERS=1\nRNACOS_ENABLE_OPEN_API_AUTH=false\nRNACOS_NAMING_INSTANCE_METADATA_PERSISTENCE_ENABLE=true\n", expand_config(r.spec.config_template.as_deref().unwrap(), &r).unwrap());
        std::fs::write(r.etc.join(".env"), &content).unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("rnacos"); } }
        let _cleanup = Cleanup(&state);
        let start = state.start_service("rnacos");
        let initial_succeeded = start.is_ok();
        if let Err(error) = start {
            assert_eq!(error.code, "SERVICE_RUNTIME_PANIC", "{error:?}");
            assert!(error.hint.as_deref().unwrap().contains("PERSISTENCE_ENABLE=false"), "{error:?}; {:?}", state.manager.tail("rnacos", 80));
        } else {
            verify_native_rnacos_recovery_value(&state, true);
            state.stop_service("rnacos").unwrap();
        }
        assert_eq!(std::fs::read_to_string(r.etc.join(".env")).unwrap(), content);
        assert_native_rnacos_stopped(&state, port);
        // 模拟用户明确选择规避方式；沿用失败时的数据目录，不删除 Raft 文件。
        let marker = r.data.join("keep-existing-data");
        std::fs::write(&marker, "preserved").unwrap();
        std::fs::write(r.etc.join(".env"), content.replace("PERSISTENCE_ENABLE=true", "PERSISTENCE_ENABLE=false")).unwrap();
        if let Err(error) = state.start_service("rnacos") {
            // 失败的初始 Raft 写入可能留下未就绪数据，关闭元数据持久化并不能修复它。
            assert!(!initial_succeeded, "healthy data failed to restart: {error:?}");
            assert_eq!(error.code, "SERVICE_START_TIMEOUT", "{error:?}");
            assert!(error.message.contains("Raft"));
            assert_native_rnacos_stopped(&state, port);
            assert_eq!(std::fs::read_to_string(marker).unwrap(), "preserved");
            return;
        }
        verify_native_rnacos_recovery_value(&state, !initial_succeeded);
        state.restart_service("rnacos").unwrap();
        verify_native_rnacos_recovery_value(&state, false);
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "preserved");
        state.stop_service("rnacos").unwrap();
        assert_native_rnacos_stopped(&state, port);
    }

    fn assert_native_rnacos_stopped(state: &crate::CoreState, port: u16) {
        assert!(state.manager.snapshot("rnacos").unwrap().pids.is_empty());
        // Windows 终止进程后释放监听句柄可能稍有延迟，但最终必须全部关闭。
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while [0, 1000, 2000].iter().any(|offset| tcp_port_open(port + offset)) {
            assert!(std::time::Instant::now() < deadline, "stopped r-nacos kept a listener open");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn verify_native_rnacos_recovery_value(state: &crate::CoreState, write: bool) {
        let port = state.manager.snapshot("rnacos").unwrap().port.unwrap();
        let url = format!("http://127.0.0.1:{port}/nacos/v1/cs/configs");
        let client = reqwest::blocking::Client::builder().no_proxy().pool_max_idle_per_host(0).timeout(Duration::from_secs(3)).build().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let write_status = if write {
                Some(client.post(&url).form(&[("dataId", "recovery-fixture"), ("group", "DEFAULT_GROUP"), ("content", "recovered-value")])
                    .send().unwrap().status())
            } else { None };
            let response = client.get(&url).query(&[("dataId", "recovery-fixture"), ("group", "DEFAULT_GROUP")]).send().unwrap();
            if response.status().is_success() && response.text().unwrap() == "recovered-value" { break; }
            assert!(std::time::Instant::now() < deadline, "r-nacos did not preserve configuration; write status {write_status:?}: {:?}", state.manager.tail("rnacos", 40));
            std::thread::sleep(Duration::from_millis(200));
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
