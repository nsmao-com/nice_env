//! 运行时 bin 目录注入系统 PATH —— 让 `php`、`mysql`、`node` 这些命令在终端里直接用。
//!
//! 设计要点：
//!
//! 1. **只碰我们自己写进去的目录**。改动前先把上次写入的目录列表读出来
//!    （`pathEnvDirs` 设置项），合并时精确移除这些条目，其余 PATH 原样保留。
//!    绝不按「看起来像我们的路径」去猜着删——用户手写的路径必须毫发无伤。
//! 2. **多版本只放一个**。PATH 版本独立保存，不改变默认版本、服务进程或站点绑定。
//!    旧设置尚未指定 PATH 版本时，沿用原有默认版本。
//! 3. **纯函数负责合并**（`merge_win_path` / 平台层的 `merge_profile_content`），
//!    写盘只在 platform 层发生，便于测试与审查。
//! 4. Windows 写 HKCU（用户级，无需管理员），写完广播变更让新终端立刻生效；
//!    macOS 写 `~/.zshrc` 托管块，与 hosts 同一套「标记块可整块回滚」的思路。

use crate::error::{AppError, Result};
use crate::model::{Manifest, PathEnvEntry, PathEnvStatus};
use crate::paths::Paths;
use crate::store::Store;

/// 总开关（"1" 为开）
const ENABLED_KEY: &str = "pathEnvEnabled";
/// 我们写入过（因而有责任清理）的目录列表，JSON 数组
const DIRS_KEY: &str = "pathEnvDirs";
/// 用户勾选要注入的包 id 列表，JSON 数组；缺省表示「全部」
const SELECTED_KEY: &str = "pathEnvSelected";
/// 每个套件单独选择的 PATH 版本；与 activeVersion 设置独立。
const VERSIONS_KEY: &str = "pathEnvVersions";

#[cfg(test)]
fn chosen_version(store: &Store, id: &str) -> Option<crate::model::InstalledPackage> {
    let installed = store.list_installed().ok()?;
    let versions = read_selection(store, VERSIONS_KEY).ok()?;
    chosen_from(store, &installed, &versions, id)
        .ok()
        .flatten()
        .cloned()
}

fn read_selection<T: serde::de::DeserializeOwned + Default>(store: &Store, key: &str) -> Result<T> {
    match store.get_setting_checked(key)? {
        Some(value) => serde_json::from_str(&value)
            .map_err(|error| AppError::internal("读取 PATH 版本选择", error.to_string())),
        None => Ok(T::default()),
    }
}

fn chosen_from<'a>(
    store: &Store,
    installed: &'a [crate::model::InstalledPackage],
    versions: &std::collections::BTreeMap<String, String>,
    id: &str,
) -> Result<Option<&'a crate::model::InstalledPackage>> {
    match versions.get(id) {
        // 已选版本卸载后不擅自换成其它版本，sync 会清除原有托管路径。
        Some(version) => Ok(installed
            .iter()
            .find(|p| p.id == id && p.version == *version)),
        None => {
            let active = store.get_setting_checked(&format!("active{id}Version"))?;
            Ok(installed
                .iter()
                .find(|p| p.id == id && Some(&p.version) == active.as_ref())
                .or_else(|| {
                    installed
                        .iter()
                        .filter(|p| p.id == id)
                        .min_by(|a, b| crate::versions::cmp_version_desc(&a.version, &b.version))
                }))
        }
    }
}

/// 当前终端使用的只读快照；总开关关闭也可生成脚本，不修改持久 PATH 或版本设置。
pub fn terminal_environment(
    store: &Store,
    paths: &Paths,
    manifest: &Manifest,
) -> Result<crate::model::TerminalEnvironment> {
    terminal_environment_selected(store, paths, manifest, &Default::default())
}

/// 项目版本优先；未固定的 PHP 跟随站点，其余命令沿用 PATH 选择。
/// 调用方按 SITE_CHANGES -> lifecycle 的顺序锁定站点与安装版本。
pub fn site_terminal_environment(store: &Store, paths: &Paths, manifest: &Manifest, site_id: &str) -> Result<crate::model::TerminalEnvironment> {
    let site = crate::sites::get(store, site_id)?;
    let root = project_directory(&site)?;
    let content = read_project_file(&root.join(PROJECT_FILE))?;
    let mut required = project_versions(&project_document(content.as_deref())?)?;
    let files = runtime_version_files(&root);
    let detected = detect_project_versions(store, &files)?;
    let sources = apply_detected_versions(&mut required, &detected)?;
    validate_project_versions(store, manifest, &required)?;
    if site.runtime.kind == crate::model::SiteKind::Php && !required.contains_key("php") {
        let version = site.runtime.php_version.as_ref().filter(|v| !v.is_empty()).ok_or_else(||
            AppError::new("TERMINAL_RUNTIME_UNAVAILABLE", "该 PHP 站点尚未指定 PHP 版本，请先保存站点设置"))?;
        required.insert("php".into(), version.clone());
    }
    let mut environment = terminal_environment_selected(store, paths, manifest, &required)?;
    for entry in &mut environment.entries { entry.source = sources.get(&entry.id).cloned(); }
    environment.cwd = root.to_string_lossy().into_owned();
    // 文件本身也是快照的一部分，外部改写后必须重新预览。
    environment.revision = terminal_revision(&environment, Some(&project_revision(&site, &root, content.as_deref(), &files, &detected)));
    Ok(environment)
}

const PROJECT_FILE: &str = ".niceenv.json";

// 仅读取确定的项目目录，不跨越项目边界查找用户级配置，也不执行文件中的内容。
type RuntimeVersionFiles = std::collections::BTreeMap<&'static str, Result<Option<String>>>;

fn runtime_version_files(root: &std::path::Path) -> RuntimeVersionFiles {
    [".nvmrc", ".node-version", ".python-version"].into_iter()
        .map(|name| (name, read_project_file(&root.join(name)))).collect()
}

fn numeric_runtime_version(value: &str, node: bool) -> Option<Vec<u64>> {
    let value = if node { value.strip_prefix('v').unwrap_or(value) } else { value };
    let parts: Vec<_> = value.split('.').collect();
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|part| part.is_empty()
        || !part.bytes().all(|c| c.is_ascii_digit()) || (part.len() > 1 && part.starts_with('0'))) { return None; }
    parts.into_iter().map(|part| part.parse().ok()).collect()
}

enum RuntimeRequirement { Numeric(Vec<u64>), Any, NodeLts(String) }

fn runtime_file_requirement(name: &str, content: &str) -> std::result::Result<(String, RuntimeRequirement), String> {
    let lines: Vec<_> = content.trim_start_matches('\u{feff}').lines().map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty() && !(name == ".nvmrc" && line.contains('='))).collect();
    if lines.len() != 1 || lines[0].split_whitespace().count() != 1 || lines[0].contains(':') {
        return Err(format!("{name} 需要一个版本号；空文件或多个解释器请在项目版本页明确选择版本"));
    }
    let value = lines[0];
    if value.len() > 128 || value.chars().any(char::is_control) { return Err(format!("{name} 中的版本号格式无效")); }
    if name == ".nvmrc" {
        if matches!(value, "node" | "stable") { return Ok((value.into(), RuntimeRequirement::Any)); }
        if let Some(alias) = value.strip_prefix("lts/").filter(|alias| *alias == "*" || (!alias.is_empty()
            && alias.len() <= 64 && alias.bytes().all(|c| c.is_ascii_alphabetic() || c == b'-'))) {
            return Ok((value.into(), RuntimeRequirement::NodeLts(alias.into())));
        }
    }
    let numeric = numeric_runtime_version(value, name != ".python-version").ok_or_else(||
        format!("{name} 使用了无法自动解析的版本写法；请在项目版本页选择已安装版本"))?;
    Ok((value.into(), RuntimeRequirement::Numeric(numeric)))
}

fn detect_project_versions(store: &Store, files: &RuntimeVersionFiles) -> Result<Vec<crate::model::ProjectRuntimeDetection>> {
    let installed = store.list_installed()?;
    let mut detections = Vec::new();
    for (id, names) in [("node", &[".nvmrc", ".node-version"][..]), ("python", &[".python-version"][..])] {
        let relevant: Vec<_> = names.iter().filter_map(|name| files.get(*name)
            .filter(|content| !matches!(content, Ok(None))).map(|content| (*name, content))).collect();
        if relevant.is_empty() { continue; }
        let mut detection = crate::model::ProjectRuntimeDetection { id: id.into(), files: relevant.iter().map(|(name, _)| (*name).into()).collect(),
            requirements: Vec::new(), resolved_version: None, issue: None };
        let mut constraints: Vec<Vec<u64>> = Vec::new();
        for (name, content) in relevant {
            let result = match content {
                Ok(Some(content)) => runtime_file_requirement(name, content),
                Err(error) => Err(format!("{name}：{}", error.message)),
                Ok(None) => continue,
            };
            match result {
                Ok((request, constraint)) => {
                    detection.requirements.push(format!("{name}: {request}"));
                    match constraint {
                        RuntimeRequirement::Numeric(parts) => constraints.push(parts),
                        RuntimeRequirement::Any => {},
                        RuntimeRequirement::NodeLts(alias) => match crate::versions::node_lts_version(store, &alias) {
                            Ok(version) => {
                                *detection.requirements.last_mut().unwrap() = format!("{name}: {request} → {version}");
                                if let Some(parts) = numeric_runtime_version(&version, true) { constraints.push(parts); }
                            }
                            Err(error) => if detection.issue.is_none() { detection.issue = Some(error.message); },
                        },
                    }
                }
                Err(issue) => if detection.issue.is_none() { detection.issue = Some(issue); },
            }
        }
        if detection.issue.is_none() && constraints.windows(2).any(|pair| {
            let len = pair[0].len().min(pair[1].len()); pair[0][..len] != pair[1][..len]
        }) {
            detection.issue = Some(".nvmrc 与 .node-version 的版本要求冲突，请统一文件或在项目版本页明确选择 Node.js 版本".into());
        }
        if detection.issue.is_none() {
            detection.resolved_version = installed.iter().filter(|package| package.id == id && package.category == "runtime")
                .filter(|package| numeric_runtime_version(&package.version, id == "node")
                    .is_some_and(|version| version.len() == 3 && constraints.iter().all(|constraint| version.starts_with(constraint))))
                .min_by(|a, b| crate::versions::cmp_version_desc(&a.version, &b.version)).map(|package| package.version.clone());
            if detection.resolved_version.is_none() {
                detection.issue = Some(format!("{}：没有符合文件要求的已安装版本，请先安装或在项目版本页明确选择版本", detection.requirements.join("；")));
            }
        }
        detections.push(detection);
    }
    Ok(detections)
}

fn apply_detected_versions(required: &mut std::collections::BTreeMap<String, String>, detections: &[crate::model::ProjectRuntimeDetection]) -> Result<std::collections::BTreeMap<String, String>> {
    let mut sources: std::collections::BTreeMap<String, String> = required.keys().map(|id| (id.clone(), PROJECT_FILE.into())).collect();
    for detected in detections {
        if required.contains_key(&detected.id) { continue; }
        let version = detected.resolved_version.as_ref().ok_or_else(|| AppError::new("PROJECT_RUNTIME_DETECTION", detected.issue.clone().unwrap_or_else(|| "无法确定项目运行时版本".into()))
            .with_hint("请在项目版本页选择已安装版本覆盖此项，或修正版本文件后刷新。"))?;
        sources.insert(detected.id.clone(), detected.files.join(" + "));
        required.insert(detected.id.clone(), version.clone());
    }
    Ok(sources)
}

fn terminal_runtime_label(id: &str, display_name: &str) -> String {
    match id { "php" => "PHP", "node" => "Node.js", "python" => "Python", "go" => "Go", _ => display_name }.into()
}

fn project_directory(site: &crate::model::Site) -> Result<std::path::PathBuf> {
    if site.runtime.kind == crate::model::SiteKind::Redirect {
        return Err(AppError::new("SITE_NO_PROJECT", "跳转站点没有项目目录或项目终端"));
    }
    terminal_directory(&site.root_dir)?;
    crate::envfile::project_root(std::path::Path::new(&site.root_dir)).canonicalize()
        .map_err(|e| AppError::io("读取项目目录", e))
}

fn project_file_error(mut error: AppError) -> AppError {
    error.code = error.code.replace("ENV_", "PROJECT_RUNTIME_");
    error.message = error.message.replace("环境文件", "项目版本文件");
    error
}

fn read_project_file(path: &std::path::Path) -> Result<Option<String>> {
    crate::envfile::read_env_file(path).map_err(project_file_error)
}

fn project_document(content: Option<&str>) -> Result<serde_json::Value> {
    let Some(content) = content else { return Ok(serde_json::json!({"schemaVersion": 1, "runtimes": {}})); };
    let value: serde_json::Value = serde_json::from_str(content.trim_start_matches('\u{feff}'))
        .map_err(|_| AppError::new("PROJECT_RUNTIME_FORMAT", ".niceenv.json 格式无效，未覆盖原文件")
            .with_hint("请修正项目文件中的 JSON 格式后重新读取。"))?;
    if !value.is_object() || value["schemaVersion"].as_u64() != Some(1) {
        return Err(AppError::new("PROJECT_RUNTIME_FORMAT", ".niceenv.json 的配置版本不受支持，未覆盖原文件")
            .with_hint("当前支持 schemaVersion 为 1 的项目配置。"));
    }
    project_versions(&value)?;
    Ok(value)
}

fn project_versions(value: &serde_json::Value) -> Result<std::collections::BTreeMap<String, String>> {
    let versions = value.get("runtimes").cloned().unwrap_or_else(|| serde_json::json!({}));
    let versions: std::collections::BTreeMap<String, String> = serde_json::from_value(versions)
        .map_err(|_| AppError::new("PROJECT_RUNTIME_FORMAT", "项目版本必须由运行时名称和版本号组成"))?;
    if versions.len() > 128 || versions.iter().any(|(id, version)| id.is_empty() || id.len() > 128 || version.is_empty() || version.len() > 128
        || id.chars().chain(version.chars()).any(char::is_control)) {
        return Err(AppError::new("PROJECT_RUNTIME_FORMAT", "项目版本名称或版本号无效，请检查 .niceenv.json"));
    }
    Ok(versions)
}

fn project_revision(site: &crate::model::Site, root: &std::path::Path, content: Option<&str>, files: &RuntimeVersionFiles, detected: &[crate::model::ProjectRuntimeDetection]) -> String {
    use sha2::{Digest, Sha256};
    let payload = (&site.id, &site.root_dir, root.to_string_lossy(), content, files, detected);
    format!("{:x}", Sha256::digest(serde_json::to_vec(&payload).expect("project snapshot contains only strings")))
}

fn project_view(store: &Store, manifest: &Manifest, site: &crate::model::Site, root: &std::path::Path, content: Option<&str>, files: &RuntimeVersionFiles) -> Result<crate::model::ProjectRuntimeVersions> {
    let versions = project_versions(&project_document(content)?)?;
    let installed = store.list_installed()?;
    let detected = detect_project_versions(store, files)?;
    let installer = crate::install::Installer { manifest: manifest.clone() };
    let mut options: std::collections::BTreeMap<String, crate::model::ProjectRuntimeOption> = std::collections::BTreeMap::new();
    for package in &installed {
        let entry = installer.installed_entry(package);
        if package.category != "runtime" || entry.entry.is_empty() || is_non_executable_entry(&entry.entry) { continue; }
        let option = options.entry(package.id.clone()).or_insert_with(|| crate::model::ProjectRuntimeOption {
            id: package.id.clone(), label: terminal_runtime_label(&package.id, &entry.display_name), versions: Vec::new(),
        });
        option.versions.push(package.version.clone());
    }
    // 缺失或未知的固定项仍可见、可解除，读取不能依赖终端预览成功。
    for id in versions.keys().chain(detected.iter().map(|entry| &entry.id)) {
        options.entry(id.clone()).or_insert_with(|| crate::model::ProjectRuntimeOption {
            id: id.clone(), label: terminal_runtime_label(id, manifest.packages.iter().find(|p| p.id == *id).map(|p| p.display_name.as_str()).unwrap_or(id)), versions: Vec::new(),
        });
    }
    for option in options.values_mut() { option.versions.sort_by(|a, b| crate::versions::cmp_version_desc(a, b)); option.versions.dedup(); }
    let shared_sites = store.list_sites()?.into_iter().filter(|other| other.id != site.id)
        .filter(|other| project_directory(other).ok().as_deref() == Some(root)).map(|other| other.name).collect();
    Ok(crate::model::ProjectRuntimeVersions {
        path: root.join(PROJECT_FILE).to_string_lossy().into_owned(), exists: content.is_some(),
        revision: project_revision(site, root, content, files, &detected), versions, options: options.into_values().collect(), shared_sites, detected,
        php_version: (site.runtime.kind == crate::model::SiteKind::Php).then(|| site.runtime.php_version.clone()).flatten(),
    })
}

pub fn project_runtime_versions(store: &Store, manifest: &Manifest, site_id: &str) -> Result<crate::model::ProjectRuntimeVersions> {
    let site = crate::sites::get(store, site_id)?;
    let root = project_directory(&site)?;
    let content = read_project_file(&root.join(PROJECT_FILE))?;
    project_view(store, manifest, &site, &root, content.as_deref(), &runtime_version_files(&root))
}

fn validate_project_versions(store: &Store, manifest: &Manifest, versions: &std::collections::BTreeMap<String, String>) -> Result<()> {
    let installed = store.list_installed()?;
    let installer = crate::install::Installer { manifest: manifest.clone() };
    for (id, version) in versions {
        let package = installed.iter().find(|p| p.id == *id && p.version == *version && p.category == "runtime")
            .ok_or_else(|| AppError::new("TERMINAL_RUNTIME_UNAVAILABLE", format!("项目指定的 {id} {version} 尚未安装或不是可用运行时"))
                .with_hint("请在项目版本中改选已安装版本或取消固定，也可以先到套件页安装所需版本。"))?;
        let entry = installer.installed_entry(package);
        terminal_package_directory(package, &entry).map_err(|reason| AppError::new("TERMINAL_RUNTIME_UNAVAILABLE", format!("{id} {version}：{reason}"))
            .with_hint("请修复此运行时，或在项目版本中改选其他版本。"))?;
    }
    Ok(())
}

pub fn save_project_runtime_versions(store: &Store, manifest: &Manifest, site_id: &str, versions: &std::collections::BTreeMap<String, String>, expected_revision: &str) -> Result<crate::model::ProjectRuntimeVersions> {
    let site = crate::sites::get(store, site_id)?;
    let root = project_directory(&site)?;
    let path = root.join(PROJECT_FILE);
    let original = read_project_file(&path)?;
    let files = runtime_version_files(&root);
    let detected = detect_project_versions(store, &files)?;
    if project_revision(&site, &root, original.as_deref(), &files, &detected) != expected_revision {
        return Err(AppError::new("PROJECT_RUNTIME_CHANGED", "项目文件、目录或自动匹配版本已变化，未覆盖当前文件")
            .with_hint("草稿已保留。请重新读取并核对最新版本后再保存。"));
    }
    let mut document = project_document(original.as_deref())?;
    let mut effective = versions.clone();
    apply_detected_versions(&mut effective, &detected)?;
    validate_project_versions(store, manifest, &effective)?;
    if project_versions(&document)? == *versions { return project_view(store, manifest, &site, &root, original.as_deref(), &files); }
    document["runtimes"] = serde_json::json!(versions);
    let next = format!("{}\n", serde_json::to_string_pretty(&document).map_err(|e| AppError::internal("保存项目版本", e.to_string()))?);
    if next.len() > 1024 * 1024 { return Err(AppError::new("PROJECT_RUNTIME_TOO_LARGE", "项目版本文件不能超过 1 MiB")); }
    let view = project_view(store, manifest, &site, &root, Some(&next), &files)?;
    let current_files = runtime_version_files(&root);
    if project_revision(&site, &root, original.as_deref(), &current_files, &detect_project_versions(store, &current_files)?) != expected_revision {
        return Err(AppError::new("PROJECT_RUNTIME_CHANGED", "项目版本文件已变化，未覆盖当前文件").with_hint("请重新读取并核对最新内容后保存。"));
    }
    if std::fs::metadata(&path).ok().is_some_and(|m| m.permissions().readonly()) {
        return Err(AppError::new("PROJECT_RUNTIME_READ_ONLY", "项目版本文件是只读文件，未保存设置"));
    }
    if let Some(original) = &original {
        let backup = root.join(format!("{PROJECT_FILE}.nsb-backup"));
        let previous = read_project_file(&backup)?;
        crate::envfile::replace_env_file(&backup, previous.as_deref(), original).map_err(project_file_error)?;
    }
    crate::envfile::replace_env_file(&path, original.as_deref(), &next).map_err(project_file_error)?;
    Ok(view)
}

/// 只保护仍在站点列表中、可读取的项目；不扫描任意磁盘目录。
pub(crate) fn project_references_version(store: &Store, site: &crate::model::Site, id: &str, version: &str) -> Result<bool> {
    if site.runtime.kind == crate::model::SiteKind::Redirect { return Ok(false); }
    let root = crate::envfile::project_root(std::path::Path::new(&site.root_dir));
    let result = (|| {
        let content = read_project_file(&root.join(PROJECT_FILE))?;
        if let Some(fixed) = project_versions(&project_document(content.as_deref())?)?.get(id) { return Ok(fixed == version); }
        if matches!(id, "node" | "python") {
            if let Some(detected) = detect_project_versions(store, &runtime_version_files(&root))?.into_iter().find(|entry| entry.id == id) {
                if let Some(issue) = detected.issue { return Err(AppError::new("PROJECT_RUNTIME_DETECTION", issue)); }
                return Ok(detected.resolved_version.as_deref() == Some(version));
            }
        }
        Ok(false)
    })();
    result.map_err(|error: AppError| AppError::new("PROJECT_RUNTIME_UNREADABLE", format!("无法检查站点「{}」的项目版本，未卸载运行时", site.name))
        .with_hint("请在项目版本页明确选择版本，或修复版本文件及目录访问权限后重试。").with_detail(error.message))
}

pub fn terminal_directory(cwd: &str) -> Result<std::path::PathBuf> {
    if cwd.trim().is_empty() || cwd.chars().any(char::is_control) {
        return Err(AppError::new("TERMINAL_DIRECTORY_INVALID", "终端目录不能为空或包含控制字符"));
    }
    let directory = std::path::Path::new(cwd).canonicalize().map_err(|e| AppError::io("读取终端工作目录", e))?;
    if !directory.is_dir() { return Err(AppError::new("TERMINAL_DIRECTORY_INVALID", "终端工作目录不是文件夹")); }
    Ok(directory)
}

fn terminal_revision(environment: &crate::model::TerminalEnvironment, site_id: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let payload = (&environment.shell, &environment.cwd, &environment.script, &environment.entries, &environment.warnings, site_id);
    format!("{:x}", Sha256::digest(serde_json::to_vec(&payload).expect("terminal snapshot contains only strings")))
}

fn terminal_environment_selected(store: &Store, paths: &Paths, manifest: &Manifest, required: &std::collections::BTreeMap<String, String>) -> Result<crate::model::TerminalEnvironment> {
    let installed = store.list_installed()?;
    let mut versions =
        read_selection::<std::collections::BTreeMap<String, String>>(store, VERSIONS_KEY)?;
    versions.extend(required.iter().map(|(id, version)| (id.clone(), version.clone())));
    let selected = read_selection::<Option<Vec<String>>>(store, SELECTED_KEY)?;
    let installer = crate::install::Installer {
        manifest: manifest.clone(),
    };
    let ids: std::collections::BTreeSet<_> = installed
        .iter()
        .map(|p| p.id.as_str())
        .chain(versions.keys().map(String::as_str))
        .collect();
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    for id in ids {
        if !wants(&selected, id) && !required.contains_key(id) {
            continue;
        }
        let Some(package) = chosen_from(store, &installed, &versions, id)? else {
            if let Some(version) = required.get(id) {
                return Err(AppError::new("TERMINAL_RUNTIME_UNAVAILABLE", format!("站点指定的 {id} {version} 尚未安装，未打开终端"))
                    .with_hint("请先安装此版本，或在站点设置中选择已安装版本。"));
            }
            warnings.push(format!("{id}：所选 PATH 版本已卸载，请重新选择版本"));
            continue;
        };
        let meta = installer.installed_entry(package);
        if is_non_executable_entry(&meta.entry) {
            if required.contains_key(id) { return Err(AppError::new("TERMINAL_RUNTIME_UNAVAILABLE", "站点指定版本没有可用的命令行入口")); }
            continue;
        }
        let check = terminal_package_directory(package, &meta);
        match check {
            Ok(bin_dir) => entries.push(crate::model::TerminalEnvironmentEntry {
                id: id.into(),
                label: terminal_runtime_label(id, &meta.display_name),
                version: package.version.clone(),
                bin_dir,
                source: None,
            }),
            Err(reason) => {
                let message = format!("{} {}：{reason}", meta.display_name, package.version);
                if required.contains_key(id) { return Err(AppError::new("TERMINAL_RUNTIME_UNAVAILABLE", message).with_hint("站点指定版本不可用，请修复或重新安装后再打开终端。")); }
                warnings.push(message);
            }
        }
    }
    entries.sort_by_key(|entry| (!required.contains_key(&entry.id), entry.id.clone()));
    let dirs = entries
        .iter()
        .map(|entry| entry.bin_dir.clone())
        .collect::<Vec<_>>();
    let mut environment = crate::model::TerminalEnvironment {
        shell: if cfg!(windows) { "powershell" } else { "posix" }.into(),
        cwd: paths.base.to_string_lossy().into_owned(),
        revision: String::new(),
        script: render_terminal_script(&dirs, cfg!(windows))?,
        entries,
        warnings,
    };
    environment.revision = terminal_revision(&environment, None);
    Ok(environment)
}

pub(crate) fn terminal_package_directory(package: &crate::model::InstalledPackage, meta: &crate::model::PackageManifestEntry) -> std::result::Result<String, String> {
    if meta.entry.trim().is_empty() || is_non_executable_entry(&meta.entry) { return Err("该运行时没有可用的命令行入口".into()); }
    let entry = terminal_cli_entry(&package.id, &meta.entry, cfg!(windows));
    let dir = bin_dir_for(&package.install_path, &entry)
        .ok_or_else(|| "无法确定安装入口，请重新安装该版本".to_string())?;
    let relative = entry.replace('\\', "/");
    if relative
        .split('/')
        .any(|part| part == ".." || part.contains(':'))
        || std::path::Path::new(&relative).is_absolute()
    {
        return Err("安装入口必须位于安装目录内".into());
    }
    if !std::path::Path::new(&package.install_path).is_absolute() {
        return Err("安装目录不是绝对路径，请重新安装该版本".into());
    }
    let root = std::path::Path::new(&package.install_path)
        .canonicalize()
        .map_err(|error| format!("无法读取安装目录：{error}"))?;
    let executable = root
        .join(relative)
        .canonicalize()
        .map_err(|error| format!("无法读取入口文件：{error}"))?;
    if !executable.starts_with(&root) || !executable.is_file() {
        return Err("入口文件不可用或指向安装目录外".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if executable
            .metadata()
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Err("入口文件没有执行权限".into());
        }
    }
    validate_terminal_dir(&dir, cfg!(windows)).map_err(|e| e.message)?;
    Ok(dir)
}

fn terminal_cli_entry(id: &str, entry: &str, windows: bool) -> String {
    if id != "php" { return entry.into(); }
    let entry = entry.replace('\\', "/");
    let parent = entry.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("");
    let parent = if !windows && (parent == "sbin" || parent.ends_with("/sbin")) {
        format!("{}bin", &parent[..parent.len() - 4])
    } else { parent.into() };
    let executable = if windows { "php.exe" } else { "php" };
    if parent.is_empty() { executable.into() } else { format!("{parent}/{executable}") }
}

/// 参数由后端快照生成，前端不能提交待执行脚本。
pub fn powershell_terminal_args(environment: &crate::model::TerminalEnvironment) -> Result<Vec<String>> {
    use base64::Engine;
    let script = format!("$ErrorActionPreference = 'Stop'\n{}", environment.script);
    let encoded = base64::engine::general_purpose::STANDARD.encode(script.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
    if encoded.len() > 24_000 { return Err(AppError::new("TERMINAL_ENV_TOO_LARGE", "终端环境过长，请减少 PATH 中选择的套件后重试")); }
    Ok(vec!["-NoLogo".into(), "-NoProfile".into(), "-NoExit".into(), "-EncodedCommand".into(), encoded])
}

pub fn posix_terminal_command(environment: &crate::model::TerminalEnvironment) -> Result<String> {
    if environment.cwd.chars().any(char::is_control) { return Err(AppError::new("TERMINAL_DIRECTORY_INVALID", "终端目录包含控制字符")); }
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\"'\"'"));
    let script = format!("cd -- {} || exit\n{}\nexec /bin/zsh -f", quote(&environment.cwd), environment.script);
    Ok(format!("/bin/zsh -f -c {}", quote(&script)))
}

fn validate_terminal_dir(dir: &str, windows: bool) -> Result<()> {
    if dir.is_empty()
        || dir.chars().any(char::is_control)
        || dir.contains(if windows { ';' } else { ':' })
    {
        return Err(AppError::new(
            "TERMINAL_PATH_INVALID",
            "目录含 PATH 分隔符或控制字符，无法安全加入 PATH",
        ));
    }
    Ok(())
}

/// 只改变运行脚本的终端进程环境；重复粘贴不会累加所选目录，其余 PATH 项保持原样。
fn render_terminal_script(dirs: &[String], windows: bool) -> Result<String> {
    for dir in dirs {
        validate_terminal_dir(dir, windows)?;
    }
    if dirs.is_empty() {
        return Ok(String::new());
    }
    if windows {
        let quoted = dirs
            .iter()
            .map(|dir| {
                let mut literal = String::from("'");
                for ch in dir.chars() {
                    literal.push(ch);
                    // PowerShell 也将弯引号识别为单引号。
                    if matches!(ch, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
                        literal.push(ch);
                    }
                }
                literal.push('\'');
                literal
            })
            .collect::<Vec<_>>()
            .join(",\n    ");
        Ok(format!(
            r#"& {{
  $nsbDirs = @(
    {quoted}
  )
  $nsbKeys = @($nsbDirs | ForEach-Object {{ $_.Replace('/', '\').TrimEnd('\') }})
  $nsbRest = @()
  if ($env:PATH) {{
    $nsbRest = @($env:PATH.Split(';') | Where-Object {{
      $nsbKeys -notcontains $_.Replace('/', '\').TrimEnd('\')
    }})
  }}
  $env:PATH = (@($nsbDirs) + $nsbRest) -join ';'
}}"#
        ))
    } else {
        let quote = |text: &str| format!("'{}'", text.replace('\'', "'\"'\"'"));
        let joined = quote(&dirs.join(":"));
        let choices = dirs.iter().map(|d| quote(d)).collect::<Vec<_>>().join("|");
        Ok(format!(
            r#"PATH="$(
  nsb_result={joined}
  nsb_rest=${{PATH-}}
  if [ -n "$nsb_rest" ]; then
    while :; do
      nsb_item=${{nsb_rest%%:*}}
      case "$nsb_item" in
        {choices}) ;;
        *) nsb_result=$nsb_result:$nsb_item ;;
      esac
      case "$nsb_rest" in
        *:*) nsb_rest=${{nsb_rest#*:}} ;;
        *) break ;;
      esac
    done
  fi
  printf '%s.' "$nsb_result"
)"
PATH=${{PATH%.}}
export PATH"#
        ))
    }
}

/* ================= bin 目录推导 ================= */

/// 入口程序不该注入 PATH 的扩展名：这些不是可直接执行的命令，
/// 混进 PATH 只会让 `composer` 之类指向一个无法执行的文件。
fn is_non_executable_entry(entry: &str) -> bool {
    let lower = entry.to_ascii_lowercase();
    matches!(
        lower.rsplit_once('.').map(|(_, ext)| ext),
        Some("phar" | "php" | "jar" | "txt" | "json" | "toml" | "yaml" | "yml" | "md" | "ini")
    )
}

/// 由「安装目录 + 清单 entry」推出应注入 PATH 的目录 = 入口程序所在目录。
/// 这是通用规则，无需为每个包写特例：`go/bin/go.exe` → `{root}/go/bin`，
/// `php-cgi.exe` → `{root}`，`nginx-1.26.3/nginx.exe` → `{root}/nginx-1.26.3`。
pub fn bin_dir_for(install_path: &str, entry: &str) -> Option<String> {
    if entry.trim().is_empty() || is_non_executable_entry(entry) {
        return None;
    }
    let rel = entry.replace('\\', "/");
    let parent = match rel.rsplit_once('/') {
        Some((p, _)) => p,
        None => "",
    };
    let base = install_path.trim_end_matches(['/', '\\']);
    let joined = if parent.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{parent}")
    };
    // Windows 扩展路径不接受混合分隔符，否则真实目录会被误判为不存在。
    Some(if cfg!(windows) && base.starts_with(r"\\?\") {
        joined.replace('/', "\\")
    } else {
        joined
    })
}

/// 扫描 bin 目录里可用的命令名（Windows 去掉 .exe/.bat/.cmd 扩展名）。
/// 以入口程序优先排序，最多返回 6 个，避免 `mysql/bin` 这种把 UI 塞爆。
fn commands_in_dir(bin_dir: &std::path::Path, prefer: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let Ok(read) = std::fs::read_dir(bin_dir) else {
        return names;
    };
    for e in read.flatten() {
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        let fname = e.file_name().to_string_lossy().to_string();
        let lower = fname.to_ascii_lowercase();
        let stem = if cfg!(windows) {
            match lower.rsplit_once('.') {
                Some((s, "exe" | "bat" | "cmd" | "ps1")) => s.to_string(),
                _ => continue,
            }
        } else {
            fname.clone()
        };
        if !names.contains(&stem) {
            names.push(stem);
        }
    }
    let prefer_stem = prefer
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(prefer)
        .to_string();
    let prefer_stem = if cfg!(windows) {
        prefer_stem
            .rsplit_once('.')
            .map(|(s, _)| s.to_string())
            .unwrap_or(prefer_stem)
    } else {
        prefer_stem
    };
    // 入口程序同名命令排最前，其余按字母序，保证展示稳定
    names.sort_by(|a, b| {
        let pa = a.eq_ignore_ascii_case(&prefer_stem);
        let pb = b.eq_ignore_ascii_case(&prefer_stem);
        pb.cmp(&pa).then_with(|| a.cmp(b))
    });
    names.truncate(6);
    names
}

/* ================= 选择集与状态 ================= */

#[cfg(test)]
fn selected_ids(store: &Store) -> Option<Vec<String>> {
    store.get_setting_or::<Option<Vec<String>>>(SELECTED_KEY)
}

fn set_selected_ids(store: &Store, ids: &[String]) -> Result<()> {
    store.set_setting_json(SELECTED_KEY, &ids.to_vec())
}

#[cfg(test)]
fn managed_dirs(store: &Store) -> Vec<String> {
    store.get_setting_or::<Vec<String>>(DIRS_KEY)
}

pub fn is_enabled(store: &Store) -> bool {
    store.get_setting(ENABLED_KEY).as_deref() == Some("1")
}

/// 某个包是否处于「用户想要注入」的状态（未设置过则默认想要）
fn wants(sel: &Option<Vec<String>>, id: &str) -> bool {
    match sel {
        Some(list) => list.iter().any(|s| s == id),
        None => true,
    }
}

/// 计算当前「应该」注入的目录集合（不受总开关影响，供 UI 预览与实际写入共用）。
///
/// 多版本包只取独立选择的 PATH 版本：同一 id 装了两个版本时，如果不做选择，
/// `php` 最终指向哪个版本取决于 PATH 顺序，行为不可预测。
pub fn desired_dirs(store: &Store, manifest: &Manifest) -> Vec<String> {
    desired_dirs_checked(store, manifest).unwrap_or_default()
}

/// 写入 PATH 时不能把数据库或选择集读取失败当成空列表，否则会误删原托管路径。
fn desired_dirs_checked(store: &Store, manifest: &Manifest) -> Result<Vec<String>> {
    let installer = crate::install::Installer {
        manifest: manifest.clone(),
    };
    let installed = store.list_installed()?;
    let sel = read_selection::<Option<Vec<String>>>(store, SELECTED_KEY)?;
    let versions = read_selection(store, VERSIONS_KEY)?;

    // 按 id 归并，每个套件只保留一个 PATH 版本。
    let mut by_id: Vec<String> = Vec::new();
    for p in &installed {
        if !by_id.contains(&p.id) {
            by_id.push(p.id.clone());
        }
    }
    let mut dirs: Vec<(String, String)> = Vec::new();
    for id in by_id {
        if !wants(&sel, &id) {
            continue;
        }
        let Some(chosen) = chosen_from(store, &installed, &versions, &id)? else {
            continue;
        };
        let entry = installer.installed_entry(chosen);
        if let Some(dir) = bin_dir_for(&chosen.install_path, &entry.entry) {
            if std::path::Path::new(&dir).is_dir() {
                dirs.push((id.clone(), dir));
            }
        }
    }
    // 按 id 排序，保证多次调用结果稳定
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(dirs.into_iter().map(|(_, d)| d).collect())
}

/// 读取已保存的系统配置；错误必须传给前端，不能伪装成关闭或空列表。
pub fn status(store: &Store, manifest: &Manifest) -> Result<PathEnvStatus> {
    status_with_paths(store, manifest, &read_current_path_entries()?, cfg!(windows))
}

fn status_with_paths(store: &Store, manifest: &Manifest, current_paths: &[Vec<String>], windows: bool) -> Result<PathEnvStatus> {
    if current_paths.is_empty() {
        return Err(AppError::new("PATH_STATUS_UNAVAILABLE", "没有可读取的系统 PATH 配置"));
    }
    let installer = crate::install::Installer {
        manifest: manifest.clone(),
    };
    let enabled = match store.get_setting_checked(ENABLED_KEY)?.as_deref() {
        None | Some("0") => false,
        Some("1") => true,
        _ => return Err(AppError::new("PATH_SETTINGS_INVALID", "保存的 PATH 开关状态无效，未将其视为关闭状态")),
    };
    let sel = read_selection::<Option<Vec<String>>>(store, SELECTED_KEY)?;
    let versions = read_selection(store, VERSIONS_KEY)?;
    let applied = read_selection::<Vec<String>>(store, DIRS_KEY)?;
    let installed = store.list_installed()?;
    let mut entries = Vec::new();
    for package in &installed {
        let meta = installer.installed_entry(package);
        let bin_dir = bin_dir_for(&package.install_path, &meta.entry);
        let exists = bin_dir
            .as_deref()
            .map(|d| std::path::Path::new(d).is_dir())
            .unwrap_or(false);
        let commands = bin_dir
            .as_deref()
            .map(|d| commands_in_dir(std::path::Path::new(d), &meta.entry))
            .unwrap_or_default();
        let in_path = bin_dir
            .as_deref()
            .map(|dir| current_paths.iter().all(|paths| paths.iter().any(|path| same_system_path(path, dir, windows))))
            .unwrap_or(false);
        let usable = bin_dir.is_some() && exists && !commands.is_empty();
        if !usable && bin_dir.is_some() {
            // 目录在但扫不到可执行文件（如纯数据包）：不进列表，避免噪音
            continue;
        }
        if bin_dir.is_none() {
            continue;
        }
        entries.push(PathEnvEntry {
            id: package.id.clone(),
            label: meta.display_name.clone(),
            version: package.version.clone(),
            bin_dir: bin_dir.unwrap_or_default(),
            exists,
            selected: wants(&sel, &package.id)
                && chosen_from(store, &installed, &versions, &package.id)?
                    .is_some_and(|chosen| chosen.version == package.version),
            in_path,
            commands,
        });
    }

    // 每个受管 shell 都必须一致；macOS 的标记块本身还可识别记录之外的残留路径。
    let desired = if enabled {
        desired_dirs_checked(store, manifest)?
    } else {
        Vec::new()
    };
    let drift = current_paths.iter().any(|paths| path_has_drift(&desired, if windows { &applied } else { paths }, paths, windows));

    Ok(PathEnvStatus {
        enabled,
        managed_dirs: applied,
        entries,
        note: platform_note(),
        drift,
    })
}

fn same_system_path(a: &str, b: &str, windows: bool) -> bool {
    if windows { same_path(a, b) } else { a.trim_end_matches('/') == b.trim_end_matches('/') }
}

fn path_has_drift(desired: &[String], applied: &[String], current: &[String], windows: bool) -> bool {
    desired.iter().any(|dir| !current.iter().any(|path| same_system_path(path, dir, windows)))
        || applied.iter().any(|dir| !desired.iter().any(|expected| same_system_path(expected, dir, windows))
            && current.iter().any(|path| same_system_path(path, dir, windows)))
}

fn platform_note() -> String {
    if cfg!(windows) {
        "写入的是当前用户的环境变量（无需管理员）。已打开的终端不会自动更新，请新开一个终端窗口。"
            .to_string()
    } else {
        "状态来自 ~/.zshrc 及已有 ~/.bash_profile 的托管块，不代表当前终端已更新。请新开对应 shell 的终端窗口后使用。".to_string()
    }
}

/* ================= PATH 读取与合并（Windows） ================= */

/// Windows 读取用户注册表；macOS 分别读取受管 profile，不能用 App 启动时继承的 PATH。
fn read_current_path_entries() -> Result<Vec<Vec<String>>> {
    #[cfg(windows)]
    {
        platform::pathenv::read_user_path()
            .map(|p| vec![split_win_path(&p.value)])
            .map_err(AppError::from)
    }
    #[cfg(not(windows))]
    {
        read_profile_paths(&platform::pathenv::shell_profiles())
    }
}

#[cfg(any(not(windows), test))]
fn read_profile_paths(profiles: &[std::path::PathBuf]) -> Result<Vec<Vec<String>>> {
    if profiles.is_empty() {
        return Err(AppError::new("NO_SHELL_PROFILE", "找不到用户 HOME 目录，无法读取 shell PATH 配置"));
    }
    profiles.iter().map(|path| platform::pathenv::read_profile(path)
        .map(|content| platform::pathenv::parse_profile_managed_dirs(&content)).map_err(AppError::from)).collect()
}

/// 按 `;` 切分 Windows PATH，去掉空段与首尾空白
pub fn split_win_path(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `merge_win_path` / `strip_win_path` 内部用的路径比较：大小写不敏感，忽略结尾分隔符。
///
/// 大小写不敏感性是 **Windows PATH 格式本身**的属性（这些函数处理的都是注册表里的
/// PATH 文本），不是运行平台的属性 —— 因此不能按 `cfg!(windows)` 分支：那样在
/// Linux/macOS 上跑测试或做交叉验证时，`d:\RT\PHP` 与 `D:\rt\php` 会被当成两条
/// 不同路径，托管条目清不掉。这里始终按 Windows 语义比较。
fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim()
            .trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// 纯函数：把托管目录合并进 Windows PATH 文本。
///
/// - 精确移除 `previously_managed` 里的条目（那是我们写的，有责任清理）
/// - 移除即将重新加入的 `new_dirs`（避免重复，随后统一放到最前）
/// - 其余条目**原样保留**（包括 `%SystemRoot%` 这类未展开的变量引用）
///
/// 前置而非追加：用户输入 `php` 时应该命中我们管理的版本，
/// 而不是系统里可能存在的另一个 PHP。
pub fn merge_win_path(
    existing: &str,
    previously_managed: &[String],
    new_dirs: &[String],
) -> String {
    let mut kept: Vec<String> = Vec::new();
    for item in split_win_path(existing) {
        let owned = previously_managed.iter().any(|m| same_path(m, &item));
        let replaced = new_dirs.iter().any(|n| same_path(n, &item));
        if owned || replaced {
            continue;
        }
        if kept.iter().any(|k| same_path(k, &item)) {
            continue; // 顺手去重，避免 PATH 越用越长
        }
        kept.push(item);
    }
    let mut out: Vec<String> = Vec::new();
    for d in new_dirs {
        if !out.iter().any(|k| same_path(k, d)) {
            out.push(d.clone());
        }
    }
    out.extend(kept);
    out.join(";")
}

/// 从 Windows PATH 文本里移除我们托管过的条目（关闭开关 / 卸载时用）
pub fn strip_win_path(existing: &str, previously_managed: &[String]) -> String {
    merge_win_path(existing, previously_managed, &[])
}

/* ================= 应用（写盘） ================= */

/// 按当前状态把托管目录写入系统 PATH。
/// 幂等：内容不变就不写盘，也不广播（避免无意义地惊动系统）。
pub fn apply(store: &Store, paths: &Paths, manifest: &Manifest) -> Result<PathEnvStatus> {
    let _ = paths;
    let enabled = store.get_setting_checked(ENABLED_KEY)?.as_deref() == Some("1");
    let desired = if enabled {
        desired_dirs_checked(store, manifest)?
    } else {
        Vec::new()
    };
    for dir in &desired {
        validate_terminal_dir(dir, cfg!(windows))?;
    }

    #[cfg(windows)]
    {
        // 上次写入的托管目录：先从 PATH 里摘掉再放新的
        let prev = read_selection::<Vec<String>>(store, DIRS_KEY)?;
        let raw = platform::pathenv::read_user_path().map_err(AppError::from)?;
        let merged = merge_win_path(&raw.value, &prev, &desired);
        if merged != raw.value {
            platform::pathenv::write_user_path(&merged, raw.reg_type).map_err(AppError::from)?;
        }
    }
    #[cfg(not(windows))]
    {
        // macOS：托管块写入 shell 启动文件；块本身就是记录，无需额外比对
        let dirs = desired.clone();
        let profiles = platform::pathenv::shell_profiles();
        if profiles.is_empty() {
            return Err(AppError::new(
                "NO_SHELL_PROFILE",
                "找不到用户 HOME 目录，无法写入 shell 配置",
            )
            .with_hint("请确认 HOME 环境变量已设置"));
        }
        for p in &profiles {
            platform::pathenv::write_profile_managed_block(p, &dirs).map_err(AppError::from)?;
        }
    }

    store.set_setting_json(DIRS_KEY, &desired)?;
    status(store, manifest)
}

/// 总开关、勾选与选版共用恢复流程；失败后保留旧选择，并记住需要清理的托管目录。
fn update_selection(
    store: &Store,
    manifest: &Manifest,
    update: impl FnOnce() -> Result<()>,
    mut apply_update: impl FnMut() -> Result<PathEnvStatus>,
) -> Result<PathEnvStatus> {
    let previous = [
        (ENABLED_KEY, store.get_setting_checked(ENABLED_KEY)?.unwrap_or_else(|| "0".into())),
        (SELECTED_KEY, store.get_setting_checked(SELECTED_KEY)?.unwrap_or_else(|| "null".into())),
        (VERSIONS_KEY, store.get_setting_checked(VERSIONS_KEY)?.unwrap_or_else(|| "{}".into())),
    ];
    let previous_dirs = read_selection::<Vec<String>>(store, DIRS_KEY)?;
    // 读取失败时不能用默认版本覆盖旧设置，也不能开始写系统环境。
    desired_dirs_checked(store, manifest)?;
    let current = status(store, manifest)?;
    let mut applied = false;
    let result = update().and_then(|()| { applied = true; apply_update() });
    let error = match result { Ok(status) => return Ok(status), Err(error) => error };
    let mut failures = Vec::new();
    if applied {
        let mut cleanup = previous_dirs;
        if let Ok(desired) = desired_dirs_checked(store, manifest) {
            cleanup.extend(desired.into_iter().filter(|dir| !current.entries.iter()
                .any(|entry| entry.in_path && same_path(&entry.bin_dir, dir))));
        }
        if let Err(error) = store.set_setting_json(DIRS_KEY, &cleanup) { failures.push(error.message); }
    }
    // 每项独立恢复；一次写入失败不能跳过其它已变更的设置。
    for (key, value) in previous {
        if let Err(error) = store.set_setting(key, &value) { failures.push(error.message); }
    }
    if failures.is_empty() && applied {
        if let Err(error) = apply_update() { failures.push(error.message); }
    }
    if failures.is_empty() { return Err(error); }
    Err(AppError::new("PATH_UPDATE_FAILED", "环境变量更新失败，尚未确认原 PATH 已恢复")
        .with_hint("请检查目录或系统环境变量的写入权限，再到环境变量卡片重新应用 PATH。")
        .with_detail(format!("原错误：{}；恢复时：{}", error.message, failures.join("；"))))
}

/// 开/关总开关
pub fn set_enabled(
    store: &Store,
    paths: &Paths,
    manifest: &Manifest,
    enabled: bool,
) -> Result<PathEnvStatus> {
    update_selection(store, manifest,
        || store.set_setting(ENABLED_KEY, if enabled { "1" } else { "0" }),
        || apply(store, paths, manifest))
}

/// 设置要注入的包集合；空集合表示不选择任何套件。
pub fn set_selected(
    store: &Store,
    paths: &Paths,
    manifest: &Manifest,
    ids: &[String],
) -> Result<PathEnvStatus> {
    update_selection(store, manifest, || set_selected_ids(store, ids), || apply(store, paths, manifest))
}

/// 从任一已安装版本直接加入/移出 PATH，整个操作由 CoreState 的生命周期锁串行执行。
pub fn set_version(
    store: &Store,
    paths: &Paths,
    manifest: &Manifest,
    id: &str,
    version: &str,
    selected: bool,
) -> Result<PathEnvStatus> {
    let current = status(store, manifest)?;
    let entry = current
        .entries
        .iter()
        .find(|entry| entry.id == id && entry.version == version)
        .ok_or_else(|| {
            AppError::new(
                "PATH_VERSION_UNAVAILABLE",
                "该版本尚未安装，或没有可加入环境变量的命令",
            )
        })?;
    if !selected && !entry.selected {
        return Ok(current);
    }
    let mut versions = read_selection::<std::collections::BTreeMap<String, String>>(store, VERSIONS_KEY)?;
    let mut ids: Vec<String> = if current.enabled {
        current
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.id.clone())
            .collect()
    } else {
        Vec::new()
    };
    ids.retain(|value| value != id);
    if selected {
        versions.insert(id.to_string(), version.to_string());
        ids.push(id.to_string());
    }
    update_selection(store, manifest, || {
        store.set_setting_json(VERSIONS_KEY, &versions)?;
        set_selected_ids(store, &ids)?;
        if selected {
            store.set_setting(ENABLED_KEY, "1")?;
        }
        Ok(())
    }, || apply(store, paths, manifest))
}

/// 安装/卸载/切换版本后自动同步（开关没开就只更新一次状态，不写盘）。
/// 这样用户装完 PHP 不用再手动回来点一次。
pub fn sync(store: &Store, paths: &Paths, manifest: &Manifest) -> Result<()> {
    if !is_enabled(store) {
        return Ok(());
    }
    apply(store, paths, manifest).map(|_| ())
}

enum MigrationSystemPath {
    Unchanged,
    #[cfg(windows)]
    Windows {
        previous: platform::pathenv::RawPath,
        expected: String,
    },
    #[cfg(not(windows))]
    Profiles(Vec<(std::path::PathBuf, Option<String>, String)>),
}

/// 只用于尚未确认接管的迁移子进程；失败后恢复其激活状态，供原窗口继续重试。
pub struct MigrationActivationRollback {
    paths: Paths,
    managed: Vec<String>,
    marker: Option<Vec<u8>>,
    system: MigrationSystemPath,
}

fn rollback_content_needed(
    current: Option<&str>,
    previous: Option<&str>,
    expected: &str,
) -> Result<bool> {
    if current == previous {
        return Ok(false);
    }
    if current == Some(expected) {
        return Ok(true);
    }
    Err(AppError::new(
        "PATH_ROLLBACK_CONFLICT",
        "启动过程中环境变量被其它操作修改，未覆盖这些改动",
    )
    .with_hint("请检查工具箱中的环境变量状态后重新应用 PATH"))
}

impl MigrationActivationRollback {
    pub fn capture(target: &std::path::Path) -> Result<Self> {
        let paths = Paths::new(target.to_path_buf());
        let store = Store::open(paths.db())?;
        let managed = read_selection::<Vec<String>>(&store, DIRS_KEY)?;
        let marker = match std::fs::read(paths.base.join(".data-dir-activation.json")) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(AppError::io("读取迁移激活状态", error)),
        };
        let enabled = store.get_setting_checked(ENABLED_KEY)?.as_deref() == Some("1");
        let system = if enabled && marker.is_some() {
            let manifest = crate::install::Installer::effective(&paths).manifest;
            let desired = desired_dirs_checked(&store, &manifest)?;
            for dir in &desired {
                validate_terminal_dir(dir, cfg!(windows))?;
            }
            #[cfg(windows)]
            {
                let previous = platform::pathenv::read_user_path().map_err(AppError::from)?;
                let expected = merge_win_path(&previous.value, &managed, &desired);
                MigrationSystemPath::Windows { previous, expected }
            }
            #[cfg(not(windows))]
            {
                let mut snapshots = Vec::new();
                for path in platform::pathenv::shell_profiles() {
                    let previous = match std::fs::read_to_string(&path) {
                        Ok(value) => Some(value),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                        Err(error) => return Err(AppError::io("保存 shell PATH 快照", error)),
                    };
                    let expected = platform::pathenv::merge_profile_content(
                        previous.as_deref().unwrap_or(""),
                        &desired,
                    );
                    snapshots.push((path, previous, expected));
                }
                MigrationSystemPath::Profiles(snapshots)
            }
        } else {
            MigrationSystemPath::Unchanged
        };
        Ok(Self {
            paths,
            managed,
            marker,
            system,
        })
    }

    pub fn restore(self) -> Result<()> {
        let mut failures = Vec::new();
        let restored = (|| -> Result<()> {
            match self.system {
                MigrationSystemPath::Unchanged => {}
                #[cfg(windows)]
                MigrationSystemPath::Windows { previous, expected } => {
                    let current = platform::pathenv::read_user_path().map_err(AppError::from)?;
                    if current.reg_type != previous.reg_type {
                        return Err(AppError::new(
                            "PATH_ROLLBACK_CONFLICT",
                            "用户 PATH 类型已改变，未覆盖其它操作",
                        ));
                    }
                    if rollback_content_needed(
                        Some(&current.value),
                        Some(&previous.value),
                        &expected,
                    )? {
                        platform::pathenv::write_user_path(&previous.value, previous.reg_type)
                            .map_err(AppError::from)?;
                    }
                }
                #[cfg(not(windows))]
                MigrationSystemPath::Profiles(snapshots) => {
                    for (path, previous, expected) in snapshots {
                        let attempt = (|| -> Result<()> {
                            let current = match std::fs::read_to_string(&path) {
                                Ok(value) => Some(value),
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                                Err(error) => return Err(AppError::io("读取 shell PATH", error)),
                            };
                            if rollback_content_needed(
                                current.as_deref(),
                                previous.as_deref(),
                                &expected,
                            )? {
                                match previous {
                                    Some(content) => std::fs::write(&path, content)?,
                                    None => std::fs::remove_file(&path)?,
                                }
                            }
                            Ok(())
                        })();
                        if let Err(error) = attempt {
                            failures.push(format!("{}：{}", path.display(), error.message));
                        }
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = restored {
            failures.push(error.message);
        }
        let local = (|| -> Result<()> {
            Store::open(self.paths.db())?.set_setting_json(DIRS_KEY, &self.managed)?;
            if let Some(marker) = self.marker {
                crate::paths::write_atomic(
                    &self.paths.base.join(".data-dir-activation.json"),
                    &marker,
                )?;
            }
            Ok(())
        })();
        if let Err(error) = local {
            failures.push(error.message);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AppError::new(
                "DATA_DIR_ACTIVATION_ROLLBACK_FAILED",
                "新进程未完成启动，恢复迁移激活状态时也遇到问题",
            )
            .with_hint("副本已保留。请检查环境变量状态与目录权限后再重试")
            .with_detail(failures.join("；")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_status_reads_each_profile_and_observes_saved_changes_immediately() {
        let (_temp, store, paths, manifest) = terminal_fixture();
        store.set_setting(ENABLED_KEY, "1").unwrap();
        let desired = desired_dirs_checked(&store, &manifest).unwrap();
        let profiles = vec![paths.base.join(".zshrc"), paths.base.join(".bash_profile")];
        let inherited = std::env::var_os("PATH");
        let content = platform::pathenv::merge_profile_content("# unrelated shell settings\n", &desired);
        std::fs::write(&profiles[0], &content).unwrap();
        std::fs::write(&profiles[1], "# no managed PATH yet\n").unwrap();
        let partial = status_with_paths(&store, &manifest, &read_profile_paths(&profiles).unwrap(), false).unwrap();
        assert!(partial.drift);
        assert!(!partial.entries.iter().find(|entry| entry.selected).unwrap().in_path);
        std::fs::write(&profiles[1], &content).unwrap();
        let ready = status_with_paths(&store, &manifest, &read_profile_paths(&profiles).unwrap(), false).unwrap();
        assert!(!ready.drift);
        assert!(ready.entries.iter().find(|entry| entry.selected).unwrap().in_path);
        std::fs::write(&profiles[0], "# managed block removed externally\n").unwrap();
        assert!(status_with_paths(&store, &manifest, &read_profile_paths(&profiles).unwrap(), false).unwrap().drift);
        assert_eq!(std::env::var_os("PATH"), inherited);
        assert_eq!(std::fs::read_to_string(&profiles[1]).unwrap(), content);
    }

    #[test]
    fn path_status_profiles_detect_owned_leftovers_without_a_database_record() {
        let (_temp, store, _paths, manifest) = terminal_fixture();
        let snapshot = vec![v(&["/old/niceenv/bin"]), Vec::new()];
        let disabled = status_with_paths(&store, &manifest, &snapshot, false).unwrap();
        assert!(!disabled.enabled);
        assert!(disabled.drift);
        assert!(disabled.managed_dirs.is_empty());
        // Windows 只清理记录中确实由我们写入的路径，不把其它目录认作残留。
        assert!(!status_with_paths(&store, &manifest, &snapshot, true).unwrap().drift);
        assert!(!status_with_paths(&store, &manifest, &[Vec::new()], false).unwrap().drift);
    }

    #[test]
    fn path_status_corrupt_settings_return_errors_instead_of_empty_success() {
        for (key, value) in [(ENABLED_KEY, "maybe"), (SELECTED_KEY, "broken"), (VERSIONS_KEY, "[]"), (DIRS_KEY, "null")] {
            let (_temp, store, _paths, manifest) = terminal_fixture();
            store.set_setting(key, value).unwrap();
            assert!(status_with_paths(&store, &manifest, &[Vec::new()], true).is_err());
            assert_eq!(store.get_setting(key).as_deref(), Some(value));
        }
    }

    #[test]
    fn path_status_profile_read_errors_are_not_treated_as_missing_configuration() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(read_profile_paths(&[]).unwrap_err().code, "NO_SHELL_PROFILE");
        assert_eq!(read_profile_paths(&[temp.path().join("missing")]).unwrap(), vec![Vec::<String>::new()]);
        assert!(read_profile_paths(&[temp.path().to_path_buf()]).is_err());
        let invalid = temp.path().join("invalid-profile");
        std::fs::write(&invalid, [0xff, 0xfe, 0xff]).unwrap();
        assert!(read_profile_paths(&[temp.path().join("missing"), invalid.clone()]).is_err());
        assert_eq!(std::fs::read(invalid).unwrap(), [0xff, 0xfe, 0xff]);
    }

    #[test]
    fn path_status_matches_posix_paths_without_windows_case_or_separator_folding() {
        assert!(same_system_path("/Users/me/PHP/", "/Users/me/PHP", false));
        assert!(!same_system_path("/Users/me/PHP", "/Users/me/php", false));
        assert!(!same_system_path("/Users/me/a\\b", "/Users/me/a/b", false));
        assert!(!same_system_path("/Users/me/php ", "/Users/me/php", false));
        assert!(same_system_path("C:\\Tools\\PHP\\", "c:/tools/php", true));
        assert!(path_has_drift(&v(&["/PHP"]), &v(&["/php"]), &v(&["/php"]), false));
    }

    #[test]
    fn path_selection_failed_apply_restores_enabled_selected_and_version() {
        for enabled in ["0", "1"] {
            let (_temp, store, _paths, manifest) = terminal_fixture();
            store.set_setting(ENABLED_KEY, enabled).unwrap();
            store.set_setting(SELECTED_KEY, r#"["terminal-fixture"]"#).unwrap();
            store.set_setting(VERSIONS_KEY, r#"{"terminal-fixture":"1.0.0"}"#).unwrap();
            let mut calls = 0;
            let error = update_selection(&store, &manifest, || {
                store.set_setting(ENABLED_KEY, if enabled == "0" { "1" } else { "0" })?;
                store.set_setting(SELECTED_KEY, "[]")?;
                store.set_setting(VERSIONS_KEY, r#"{"terminal-fixture":"2.0.0"}"#)
            }, || {
                calls += 1;
                if calls == 1 { return Err(AppError::new("WRITE_DENIED", "write denied")); }
                assert_eq!(store.get_setting(ENABLED_KEY).as_deref(), Some(enabled));
                assert_eq!(selected_ids(&store), Some(v(&["terminal-fixture"])));
                assert_eq!(chosen_version(&store, "terminal-fixture").unwrap().version, "1.0.0");
                status(&store, &manifest)
            }).unwrap_err();
            assert_eq!(error.code, "WRITE_DENIED");
            assert_eq!(calls, 2);
        }
    }

    #[test]
    fn path_selection_partial_settings_failure_never_applies_system_path() {
        let (_temp, store, _paths, manifest) = terminal_fixture();
        let error = update_selection(&store, &manifest, || {
            store.set_setting(ENABLED_KEY, "1")?;
            store.set_setting(SELECTED_KEY, "[]")?;
            Err(AppError::new("SETTINGS_DENIED", "settings denied"))
        }, || panic!("system PATH must not be written after a settings failure")).unwrap_err();
        assert_eq!(error.code, "SETTINGS_DENIED");
        assert!(!is_enabled(&store));
        assert_eq!(selected_ids(&store), None);
        assert!(read_selection::<std::collections::BTreeMap<String, String>>(&store, VERSIONS_KEY).unwrap().is_empty());
    }

    #[test]
    fn path_selection_failed_recovery_keeps_original_choices_and_cleanup_paths() {
        let (_temp, store, _paths, manifest) = terminal_fixture();
        store.set_setting(ENABLED_KEY, "0").unwrap();
        store.set_setting(VERSIONS_KEY, r#"{"terminal-fixture":"1.0.0"}"#).unwrap();
        let mut calls = 0;
        let error = update_selection(&store, &manifest, || {
            store.set_setting(ENABLED_KEY, "1")?;
            store.set_setting(VERSIONS_KEY, r#"{"terminal-fixture":"2.0.0"}"#)
        }, || {
            calls += 1;
            Err(AppError::new("WRITE_DENIED", if calls == 1 { "original failure" } else { "recovery failure" }))
        }).unwrap_err();
        assert_eq!(error.code, "PATH_UPDATE_FAILED");
        assert!(error.detail.as_deref().unwrap().contains("original failure"));
        assert!(error.detail.as_deref().unwrap().contains("recovery failure"));
        assert!(!is_enabled(&store));
        assert_eq!(chosen_version(&store, "terminal-fixture").unwrap().version, "1.0.0");
        assert!(managed_dirs(&store).iter().any(|dir| dir.contains("custom-2.0.0")));
    }

    #[test]
    fn path_selection_corrupt_snapshot_cannot_be_replaced_by_defaults() {
        for key in [SELECTED_KEY, VERSIONS_KEY, DIRS_KEY] {
            let (_temp, store, _paths, manifest) = terminal_fixture();
            store.set_setting(key, "invalid json").unwrap();
            assert!(update_selection(&store, &manifest,
                || panic!("corrupt selection must not be changed"),
                || panic!("system PATH must not be written")).is_err());
            assert_eq!(store.get_setting(key).as_deref(), Some("invalid json"));
        }
    }

    #[test]
    fn path_selection_success_applies_once_and_preserves_new_choices() {
        let (_temp, store, _paths, manifest) = terminal_fixture();
        let mut calls = 0;
        let result = update_selection(&store, &manifest, || {
            store.set_setting(ENABLED_KEY, "1")?;
            store.set_setting(SELECTED_KEY, "[]")
        }, || { calls += 1; status(&store, &manifest) }).unwrap();
        assert_eq!(calls, 1);
        assert!(result.enabled);
        assert_eq!(selected_ids(&store), Some(vec![]));
        assert!(desired_dirs_checked(&store, &manifest).unwrap().is_empty());
    }

    #[test]
    fn path_selection_drift_detects_leftovers_even_when_disabled() {
        assert!(path_has_drift(&[], &v(&["C:/old"]), &v(&["C:/old", "C:/foreign"]), true));
        assert!(path_has_drift(&v(&["C:/new"]), &v(&["C:/old", "C:/new"]), &v(&["C:/old", "C:/new"]), true));
        assert!(path_has_drift(&v(&["C:/new"]), &[], &v(&["C:/foreign"]), true));
        assert!(!path_has_drift(&[], &v(&["C:/old"]), &v(&["C:/foreign"]), true));
        assert!(!path_has_drift(&v(&["C:/new"]), &v(&["C:/new"]), &v(&["c:/NEW/", "C:/foreign"]), true));
    }

    #[test]
    fn migration_activation_rollback_restores_copy_for_retry_without_system_writes() {
        let temp=tempfile::tempdir().unwrap();
        let paths=Paths::new(temp.path().to_path_buf());paths.ensure_dirs().unwrap();
        let store=Store::open(paths.db()).unwrap();
        store.set_setting_json(DIRS_KEY,&vec!["old-managed"]).unwrap();
        let marker=serde_json::to_vec(&paths.base).unwrap();
        let marker_file=paths.base.join(".data-dir-activation.json");
        std::fs::write(&marker_file,&marker).unwrap();
        // PATH 未启用：capture/restore 不读取或修改实际注册表/profile。
        let rollback=MigrationActivationRollback::capture(&paths.base).unwrap();
        store.set_setting_json(DIRS_KEY,&vec!["new-managed"]).unwrap();
        std::fs::remove_file(&marker_file).unwrap();
        rollback.restore().unwrap();
        assert_eq!(managed_dirs(&store),vec!["old-managed"]);
        assert_eq!(std::fs::read(marker_file).unwrap(),marker);
    }

    #[test]
    fn migration_path_rollback_preserves_concurrent_edits() {
        assert!(!rollback_content_needed(Some("original"),Some("original"),"migrated").unwrap());
        assert!(rollback_content_needed(Some("migrated"),Some("original"),"migrated").unwrap());
        assert!(rollback_content_needed(Some("new managed block"),None,"new managed block").unwrap());
        assert_eq!(rollback_content_needed(Some("external edit"),Some("original"),"migrated").unwrap_err().code,"PATH_ROLLBACK_CONFLICT");
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn terminal_fixture() -> (tempfile::TempDir, Store, Paths, Manifest) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        let store = Store::open(paths.db()).unwrap();
        let mut manifest = crate::install::Installer::bundled().manifest;
        let mut template = manifest.packages[0].clone();
        template.id = "terminal-fixture".into();
        template.entry = if cfg!(windows) {
            "nested/bin/tool.exe"
        } else {
            "nested/bin/tool"
        }
        .into();
        manifest.packages.clear();
        for version in ["1.0.0", "2.0.0"] {
            let mut entry = template.clone();
            entry.version = version.into();
            let root = paths.base.join(format!("custom-{version}"));
            let executable = root.join(&entry.entry);
            std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
            std::fs::write(&executable, "fixture").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            store
                .upsert_installed(&crate::model::InstalledPackage {
                    id: entry.id.clone(),
                    version: version.into(),
                    category: "runtime".into(),
                    install_path: root.to_string_lossy().into_owned(),
                    config_path: String::new(),
                    installed_at: 0,
                })
                .unwrap();
            manifest.packages.push(entry);
        }
        (temp, store, paths, manifest)
    }

    #[test]
    fn terminal_uses_selected_version_and_real_nested_entry_without_writes() {
        let (_temp, store, paths, manifest) = terminal_fixture();
        store.set_setting(ENABLED_KEY, "0").unwrap();
        store
            .set_setting("activeterminal-fixtureVersion", "2.0.0")
            .unwrap();
        store
            .set_setting(VERSIONS_KEY, r#"{"terminal-fixture":"1.0.0"}"#)
            .unwrap();
        let result = terminal_environment(&store, &paths, &manifest).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].version, "1.0.0");
        assert!(result.entries[0]
            .bin_dir
            .replace('\\', "/")
            .ends_with("custom-1.0.0/nested/bin"));
        assert!(result.warnings.is_empty());
        assert!(!is_enabled(&store));
        assert_eq!(
            store
                .get_setting("activeterminal-fixtureVersion")
                .as_deref(),
            Some("2.0.0")
        );
        assert!(store.get_setting(DIRS_KEY).is_none());
    }

    #[test]
    fn terminal_does_not_substitute_uninstalled_or_broken_selected_version() {
        let (_temp, store, paths, mut manifest) = terminal_fixture();
        store
            .set_setting(VERSIONS_KEY, r#"{"terminal-fixture":"3.0.0"}"#)
            .unwrap();
        let missing = terminal_environment(&store, &paths, &manifest).unwrap();
        assert!(missing.entries.is_empty());
        assert!(missing.script.is_empty());
        assert_eq!(missing.warnings.len(), 1);
        store
            .set_setting(VERSIONS_KEY, r#"{"terminal-fixture":"2.0.0"}"#)
            .unwrap();
        manifest.packages[1].entry = "../outside/tool".into();
        let unsafe_entry = terminal_environment(&store, &paths, &manifest).unwrap();
        assert!(unsafe_entry.entries.is_empty());
        assert_eq!(unsafe_entry.warnings.len(), 1);
        manifest.packages[1].entry = "missing/tool".into();
        assert_eq!(
            terminal_environment(&store, &paths, &manifest)
                .unwrap()
                .warnings
                .len(),
            1
        );
    }

    #[test]
    fn terminal_respects_empty_selection_and_reports_corrupt_settings() {
        let (_temp, store, paths, manifest) = terminal_fixture();
        assert_eq!(
            terminal_environment(&store, &paths, &manifest)
                .unwrap()
                .entries[0]
                .version,
            "2.0.0"
        );
        store.set_setting(SELECTED_KEY, "[]").unwrap();
        assert!(terminal_environment(&store, &paths, &manifest)
            .unwrap()
            .entries
            .is_empty());
        store.set_setting(SELECTED_KEY, "broken").unwrap();
        assert!(terminal_environment(&store, &paths, &manifest).is_err());
    }

    #[test]
    fn terminal_rejects_path_separators_and_control_characters() {
        for (dir, windows) in [
            ("C:/bad;path", true),
            ("/bad:path", false),
            ("/bad\npath", false),
        ] {
            assert!(render_terminal_script(&v(&[dir]), windows).is_err());
        }
        assert!(render_terminal_script(&[], true).unwrap().is_empty());
    }

    fn site_terminal_fixture() -> (tempfile::TempDir, crate::CoreState, crate::model::Site) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("data"));
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let mut manifest = crate::install::Installer::bundled().manifest;
        let mut template = manifest.packages.iter().find(|p| p.id == "php").cloned().unwrap_or_else(|| manifest.packages[0].clone());
        template.id = "php".into();
        template.entry = if cfg!(windows) { "nested/bin/php-cgi.exe" } else { "nested/sbin/php-fpm" }.into();
        manifest.packages.clear();
        for version in ["1.0.0", "2.0.0"] {
            let mut entry = template.clone(); entry.version = version.into();
            let root = paths.base.join(format!("PHP O'Brien $var`&()/{version}"));
            for relative in [entry.entry.clone(), terminal_cli_entry("php", &entry.entry, cfg!(windows))] {
                let file = root.join(relative); std::fs::create_dir_all(file.parent().unwrap()).unwrap(); std::fs::write(&file, "fixture").unwrap();
                #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap(); }
            }
            store.upsert_installed(&crate::model::InstalledPackage { id: "php".into(), version: version.into(), category: "runtime".into(), install_path: root.to_string_lossy().into_owned(), config_path: String::new(), installed_at: 0 }).unwrap();
            manifest.packages.push(entry);
        }
        let project = temp.path().join("Project 中文 O'Brien $var`&()");
        std::fs::create_dir_all(project.join("public")).unwrap(); std::fs::write(project.join("composer.json"), "{}").unwrap();
        let site: crate::model::Site = serde_json::from_value(serde_json::json!({"id":"terminal-site","name":"Terminal","domains":["terminal.test"],"rootDir":project.join("public"),"runtime":{"kind":"php","phpVersion":"1.0.0"},"https":false,"rewrite":"none","createdAt":1,"updatedAt":1})).unwrap();
        store.save_site(&site).unwrap();
        let state = crate::CoreState { paths, store, manager: std::sync::Arc::new(crate::services::ServiceManager::new()),
            downloader: std::sync::Arc::new(crate::download::Downloader::new()), installer: crate::install::Installer { manifest },
            emit: std::sync::Arc::new(|_| {}) };
        (temp, state, site)
    }

    #[test]
    fn site_terminal_uses_php_cli_and_project_root_without_changing_global_selection() {
        let (_temp, state, site) = site_terminal_fixture();
        state.store.set_setting(VERSIONS_KEY, r#"{"php":"2.0.0"}"#).unwrap();
        state.store.set_setting(SELECTED_KEY, "[]").unwrap();
        state.store.set_setting("activephpVersion", "2.0.0").unwrap();
        let result = state.site_terminal_environment(&site.id).unwrap();
        assert_eq!(result.entries.len(), 1); assert_eq!(result.entries[0].version, "1.0.0");
        assert!(result.entries[0].bin_dir.replace('\\', "/").ends_with("1.0.0/nested/bin"));
        assert!(std::path::Path::new(&result.cwd).join("composer.json").is_file());
        assert!(state.terminal_environment().unwrap().entries.is_empty());
        assert_eq!(state.store.get_setting(VERSIONS_KEY).as_deref(), Some(r#"{"php":"2.0.0"}"#));
        assert_eq!(state.store.get_setting("activephpVersion").as_deref(), Some("2.0.0"));
        assert!(!is_enabled(&state.store)); assert!(state.store.get_setting(DIRS_KEY).is_none());
        let cli = std::path::Path::new(&result.entries[0].bin_dir).join(if cfg!(windows) { "php.exe" } else { "php" });
        std::fs::remove_file(cli).unwrap();
        assert_eq!(state.site_terminal_environment(&site.id).unwrap_err().code, "TERMINAL_RUNTIME_UNAVAILABLE");
    }

    fn project_runtime_fixture() -> (tempfile::TempDir, crate::CoreState, crate::model::Site) {
        let (temp, mut state, site) = site_terminal_fixture();
        for id in ["node", "python"] {
            for version in ["1.0.0", "2.0.0"] {
                let mut entry = state.installer.manifest.packages[0].clone();
                entry.id = id.into(); entry.category = "runtime".into(); entry.version = version.into();
                entry.entry = format!("bin/{id}{}", if cfg!(windows) { ".exe" } else { "" });
                let root = state.paths.runtime_dir(id, version);
                let file = root.join(&entry.entry);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap(); std::fs::write(&file, "fixture").unwrap();
                #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap(); }
                state.store.upsert_installed(&crate::model::InstalledPackage { id: id.into(), version: version.into(), category: "runtime".into(), install_path: root.to_string_lossy().into_owned(), config_path: String::new(), installed_at: 0 }).unwrap();
                state.installer.manifest.packages.push(entry);
            }
        }
        (temp, state, site)
    }

    #[test]
    fn project_versions_save_real_file_share_directory_and_override_only_terminal() {
        let (_temp, state, site) = project_runtime_fixture();
        state.store.set_setting(VERSIONS_KEY, r#"{"node":"2.0.0","python":"2.0.0"}"#).unwrap();
        state.store.set_setting(SELECTED_KEY, "[]").unwrap();
        let view = state.project_runtime_versions(&site.id).unwrap();
        assert!(!view.exists); assert!(!std::path::Path::new(&view.path).exists());
        let mut shared = site.clone(); shared.id = "shared-project".into(); shared.name = "Shared".into(); state.store.save_site(&shared).unwrap();
        let versions = std::collections::BTreeMap::from([("node".into(), "1.0.0".into()), ("python".into(), "1.0.0".into()), ("php".into(), "2.0.0".into())]);
        let saved = state.save_project_runtime_versions(&site.id, &versions, &view.revision).unwrap();
        assert!(saved.exists); assert_eq!(saved.shared_sites, vec!["Shared"]);
        let document: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&saved.path).unwrap()).unwrap();
        assert_eq!(document["schemaVersion"], 1); assert_eq!(document["runtimes"]["node"], "1.0.0");
        assert_eq!(state.project_runtime_versions(&shared.id).unwrap().versions, versions);
        let environment = state.site_terminal_environment(&site.id).unwrap();
        assert_eq!(environment.entries.len(), 3);
        for (id, version) in &versions { assert_eq!(&environment.entries.iter().find(|e| e.id == *id).unwrap().version, version); }
        assert_eq!(crate::sites::get(&state.store, &site.id).unwrap().runtime.php_version.as_deref(), Some("1.0.0"));
        assert!(state.terminal_environment().unwrap().entries.is_empty());
        assert_eq!(state.store.get_setting(VERSIONS_KEY).as_deref(), Some(r#"{"node":"2.0.0","python":"2.0.0"}"#));
        let old = std::fs::read_to_string(&saved.path).unwrap();
        let cleared = state.save_project_runtime_versions(&site.id, &Default::default(), &saved.revision).unwrap();
        assert!(cleared.versions.is_empty());
        assert_eq!(std::fs::read_to_string(format!("{}.nsb-backup", saved.path)).unwrap(), old);
        let environment = state.site_terminal_environment(&site.id).unwrap();
        assert_eq!(environment.entries.len(), 1); assert_eq!(environment.entries[0].version, "1.0.0");
    }

    #[test]
    fn project_versions_detect_conflicts_preserve_unknown_fields_and_repair_missing_versions() {
        let (temp, state, mut site) = project_runtime_fixture();
        let view = state.project_runtime_versions(&site.id).unwrap();
        let original = r#"{"schemaVersion":1,"runtimes":{"node":"missing","unknown-runtime":"9"},"custom":{"keep":true}}"#;
        std::fs::write(&view.path, original).unwrap();
        assert_eq!(state.save_project_runtime_versions(&site.id, &Default::default(), &view.revision).unwrap_err().code, "PROJECT_RUNTIME_CHANGED");
        assert_eq!(state.site_terminal_environment(&site.id).unwrap_err().code, "TERMINAL_RUNTIME_UNAVAILABLE");
        let view = state.project_runtime_versions(&site.id).unwrap();
        assert!(view.options.iter().any(|o| o.id == "unknown-runtime" && o.versions.is_empty()));
        let valid = std::collections::BTreeMap::from([("node".into(), "1.0.0".into())]);
        let saved = state.save_project_runtime_versions(&site.id, &valid, &view.revision).unwrap();
        let content = std::fs::read_to_string(&saved.path).unwrap();
        let document: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(document["custom"]["keep"], true);
        assert_eq!(std::fs::read_to_string(format!("{}.nsb-backup", saved.path)).unwrap(), original);
        let preview = state.site_terminal_environment(&site.id).unwrap();
        std::fs::write(&saved.path, format!("{content}\n")).unwrap();
        assert_eq!(state.with_terminal_environment::<()>(Some(&site.id), &preview.revision, |_| panic!("stale project launched")).unwrap_err().code, "TERMINAL_ENV_CHANGED");
        let view = state.project_runtime_versions(&site.id).unwrap();
        let other = temp.path().join("other-project"); std::fs::create_dir(&other).unwrap(); site.root_dir = other.to_string_lossy().into_owned(); state.store.save_site(&site).unwrap();
        assert_eq!(state.save_project_runtime_versions(&site.id, &valid, &view.revision).unwrap_err().code, "PROJECT_RUNTIME_CHANGED");
        assert!(!other.join(PROJECT_FILE).exists());
        for invalid in ["{broken", r#"{"schemaVersion":2,"runtimes":{}}"#, r#"{"schemaVersion":1,"runtimes":{"node":1}}"#] {
            std::fs::write(other.join(PROJECT_FILE), invalid).unwrap();
            assert_eq!(state.project_runtime_versions(&site.id).unwrap_err().code, "PROJECT_RUNTIME_FORMAT");
            assert_eq!(std::fs::read_to_string(other.join(PROJECT_FILE)).unwrap(), invalid);
        }
        for marker in ["pyproject.toml", "requirements.txt", "go.mod", PROJECT_FILE] {
            let root = temp.path().join(marker); let output = root.join("dist");
            std::fs::create_dir_all(&output).unwrap(); std::fs::write(root.join(marker), "{}").unwrap();
            assert_eq!(crate::envfile::project_root(&output), root);
        }
    }

    #[test]
    fn project_versions_validate_cli_backup_and_protect_uninstall() {
        let (_temp, state, site) = project_runtime_fixture();
        let view = state.project_runtime_versions(&site.id).unwrap();
        let valid = std::collections::BTreeMap::from([("node".into(), "1.0.0".into())]);
        let invalid = std::collections::BTreeMap::from([("node".into(), "3.0.0".into())]);
        assert_eq!(state.save_project_runtime_versions(&site.id, &invalid, &view.revision).unwrap_err().code, "TERMINAL_RUNTIME_UNAVAILABLE");
        assert!(!std::path::Path::new(&view.path).exists());
        let saved = state.save_project_runtime_versions(&site.id, &valid, &view.revision).unwrap();
        assert_eq!(state.uninstall_package("node@1.0.0").unwrap_err().code, "PACKAGE_IN_USE");
        assert!(state.paths.runtime_dir("node", "1.0.0").is_dir());
        let backup = format!("{}.nsb-backup", saved.path);
        std::fs::create_dir(&backup).unwrap();
        assert_eq!(state.save_project_runtime_versions(&site.id, &Default::default(), &saved.revision).unwrap_err().code, "PROJECT_RUNTIME_INVALID_FILE");
        assert_eq!(state.project_runtime_versions(&site.id).unwrap().versions, valid);
        std::fs::remove_dir(&backup).unwrap();
        let mut perms = std::fs::metadata(&saved.path).unwrap().permissions(); let original_perms = perms.clone(); perms.set_readonly(true);
        std::fs::set_permissions(&saved.path, perms).unwrap();
        assert_eq!(state.save_project_runtime_versions(&site.id, &Default::default(), &saved.revision).unwrap_err().code, "PROJECT_RUNTIME_READ_ONLY");
        std::fs::set_permissions(&saved.path, original_perms).unwrap();
        state.save_project_runtime_versions(&site.id, &Default::default(), &saved.revision).unwrap();
        let entry = state.installer.manifest.packages.iter().find(|e| e.id == "node" && e.version == "1.0.0").unwrap();
        std::fs::remove_file(state.paths.runtime_dir("node", "1.0.0").join(&entry.entry)).unwrap();
        let view = state.project_runtime_versions(&site.id).unwrap();
        assert_eq!(state.save_project_runtime_versions(&site.id, &valid, &view.revision).unwrap_err().code, "TERMINAL_RUNTIME_UNAVAILABLE");
        assert!(state.project_runtime_versions(&site.id).unwrap().versions.is_empty());
        std::fs::write(&view.path, "broken").unwrap();
        assert_eq!(state.uninstall_package("node@1.0.0").unwrap_err().code, "PROJECT_RUNTIME_UNREADABLE");
    }

    #[test]
    fn project_detects_standard_files_without_writing_and_rechecks_launch_and_save() {
        let (_temp, state, site) = project_runtime_fixture();
        let root = project_directory(&site).unwrap();
        state.store.set_setting(SELECTED_KEY, "[]").unwrap();
        let nvm = "\u{feff}# Project Node\r\nfuture=value\r\n v1.0 # managed elsewhere\r\n";
        std::fs::write(root.join(".nvmrc"), nvm).unwrap();
        std::fs::write(root.join(".node-version"), "1\n").unwrap();
        std::fs::write(root.join(".python-version"), "# CPython\n2.0\n").unwrap();
        let view = state.project_runtime_versions(&site.id).unwrap();
        assert!(!view.exists); assert!(view.versions.is_empty()); assert_eq!(view.detected.len(), 2);
        assert_eq!(view.detected[0].resolved_version.as_deref(), Some("1.0.0"));
        assert_eq!(view.detected[1].resolved_version.as_deref(), Some("2.0.0"));
        assert!(!root.join(PROJECT_FILE).exists()); assert_eq!(std::fs::read_to_string(root.join(".nvmrc")).unwrap(), nvm);
        let environment = state.site_terminal_environment(&site.id).unwrap();
        let node = environment.entries.iter().find(|e| e.id == "node").unwrap();
        assert_eq!(node.version, "1.0.0"); assert_eq!(node.source.as_deref(), Some(".nvmrc + .node-version"));
        assert_eq!(environment.entries.iter().find(|e| e.id == "python").unwrap().source.as_deref(), Some(".python-version"));
        assert!(state.terminal_environment().unwrap().entries.is_empty());
        let pins = std::collections::BTreeMap::from([("node".into(), "2.0.0".into())]);
        std::fs::write(root.join(".python-version"), "1.0\n").unwrap();
        assert_eq!(state.with_terminal_environment::<()>(Some(&site.id), &environment.revision, |_| panic!("stale detection launched")).unwrap_err().code, "TERMINAL_ENV_CHANGED");
        assert_eq!(state.save_project_runtime_versions(&site.id, &pins, &view.revision).unwrap_err().code, "PROJECT_RUNTIME_CHANGED");
        assert!(!root.join(PROJECT_FILE).exists());
        let view = state.project_runtime_versions(&site.id).unwrap();
        let saved = state.save_project_runtime_versions(&site.id, &pins, &view.revision).unwrap();
        let environment = state.site_terminal_environment(&site.id).unwrap();
        let node = environment.entries.iter().find(|e| e.id == "node").unwrap();
        assert_eq!(node.version, "2.0.0"); assert_eq!(node.source.as_deref(), Some(PROJECT_FILE));
        assert_eq!(std::fs::read_to_string(root.join(".nvmrc")).unwrap(), nvm);
        state.save_project_runtime_versions(&site.id, &Default::default(), &saved.revision).unwrap();
        assert_eq!(state.site_terminal_environment(&site.id).unwrap().entries.iter().find(|e| e.id == "node").unwrap().version, "1.0.0");
        for marker in [".nvmrc", ".node-version", ".python-version"] {
            let project = root.join(marker.trim_start_matches('.')); std::fs::create_dir_all(project.join("dist")).unwrap(); std::fs::write(project.join(marker), "1").unwrap();
            assert_eq!(crate::envfile::project_root(&project.join("dist")), project);
        }
    }

    #[test]
    fn project_detection_conflicts_and_unsupported_syntax_are_repairable_with_explicit_pins() {
        let (_temp, state, site) = project_runtime_fixture();
        let root = project_directory(&site).unwrap();
        std::fs::write(root.join(".nvmrc"), "1\n").unwrap(); std::fs::write(root.join(".node-version"), "2\n").unwrap();
        let view = state.project_runtime_versions(&site.id).unwrap();
        assert!(view.detected[0].issue.as_deref().unwrap().contains("冲突"));
        assert_eq!(state.site_terminal_environment(&site.id).unwrap_err().code, "PROJECT_RUNTIME_DETECTION");
        assert_eq!(state.uninstall_package("node@1.0.0").unwrap_err().code, "PROJECT_RUNTIME_UNREADABLE");
        let pins = std::collections::BTreeMap::from([("node".into(), "2.0.0".into())]);
        let saved = state.save_project_runtime_versions(&site.id, &pins, &view.revision).unwrap();
        assert!(saved.detected[0].issue.is_some());
        assert_eq!(state.site_terminal_environment(&site.id).unwrap().entries.iter().find(|e| e.id == "node").unwrap().version, "2.0.0");
        assert_eq!(state.save_project_runtime_versions(&site.id, &Default::default(), &saved.revision).unwrap_err().code, "PROJECT_RUNTIME_DETECTION");
        assert!(state.project_runtime_versions(&site.id).unwrap().versions.contains_key("node"));
        std::fs::remove_file(root.join(PROJECT_FILE)).unwrap(); std::fs::remove_file(root.join(".node-version")).unwrap();
        for unsupported in ["", "default", "$(echo 2.0.0)", "1\n2", "1.0.0-rc.1", "01.0.0"] {
            std::fs::write(root.join(".nvmrc"), unsupported).unwrap();
            let view = state.project_runtime_versions(&site.id).unwrap();
            assert!(view.detected[0].issue.is_some(), "{unsupported}");
            assert_eq!(state.site_terminal_environment(&site.id).unwrap_err().code, "PROJECT_RUNTIME_DETECTION");
            assert_eq!(std::fs::read_to_string(root.join(".nvmrc")).unwrap(), unsupported);
        }
        std::fs::remove_file(root.join(".nvmrc")).unwrap(); std::fs::create_dir(root.join(".nvmrc")).unwrap();
        let view = state.project_runtime_versions(&site.id).unwrap(); assert!(view.detected[0].issue.is_some());
        state.save_project_runtime_versions(&site.id, &pins, &view.revision).unwrap();
        assert!(state.site_terminal_environment(&site.id).is_ok());
        std::fs::remove_dir(root.join(".nvmrc")).unwrap();
        for multiple in ["3.13.3\n3.12.9\n", "3.13.3:3.12.9", "system", "pypy3.10-7.3.17"] {
            std::fs::write(root.join(".python-version"), multiple).unwrap();
            assert!(state.project_runtime_versions(&site.id).unwrap().detected.iter().find(|entry| entry.id == "python").unwrap().issue.is_some());
            assert_eq!(state.site_terminal_environment(&site.id).unwrap_err().code, "PROJECT_RUNTIME_DETECTION");
        }
    }

    #[test]
    fn project_lts_aliases_refresh_snapshots_and_require_the_exact_official_release() {
        let (_temp, state, site) = project_runtime_fixture();
        let root = project_directory(&site).unwrap();
        let file = root.join(".nvmrc");
        std::fs::write(&file, "lts/*\n").unwrap();
        let missing = state.project_runtime_versions(&site.id).unwrap();
        assert!(missing.detected[0].issue.as_deref().unwrap().contains("刷新"));
        assert_eq!(missing.detected[0].requirements, vec![".nvmrc: lts/*"]);
        let index = serde_json::json!([{"version":"v1.0.0","lts":"First"},{"version":"v2.0.0","lts":false}]);
        crate::versions::cache_node_lts_aliases(&state.store, index.as_array().unwrap()).unwrap();
        let view = state.project_runtime_versions(&site.id).unwrap();
        assert_eq!(view.detected[0].resolved_version.as_deref(), Some("1.0.0"));
        assert!(view.detected[0].requirements[0].contains("→ 1.0.0"));
        let preview = state.site_terminal_environment(&site.id).unwrap();
        assert_eq!(state.uninstall_package("node@1.0.0").unwrap_err().code, "PACKAGE_IN_USE");
        let pins = std::collections::BTreeMap::from([("node".into(), "2.0.0".into())]);
        assert_eq!(state.save_project_runtime_versions(&site.id, &pins, &missing.revision).unwrap_err().code, "PROJECT_RUNTIME_CHANGED");
        let updated = serde_json::json!([{"version":"v1.0.0","lts":"First"},{"version":"v2.0.0","lts":"Second"}]);
        crate::versions::cache_node_lts_aliases(&state.store, updated.as_array().unwrap()).unwrap();
        assert_eq!(state.with_terminal_environment::<()>(Some(&site.id), &preview.revision, |_| panic!("stale LTS launched")).unwrap_err().code, "TERMINAL_ENV_CHANGED");
        assert_eq!(state.save_project_runtime_versions(&site.id, &pins, &view.revision).unwrap_err().code, "PROJECT_RUNTIME_CHANGED");
        for (request, expected) in [("lts/*", "2.0.0"), ("lts/FiRsT", "1.0.0")] {
            std::fs::write(&file, request).unwrap();
            assert_eq!(state.site_terminal_environment(&site.id).unwrap().entries.iter().find(|e| e.id == "node").unwrap().version, expected);
        }
        std::fs::write(root.join(".node-version"), "2").unwrap();
        assert!(state.project_runtime_versions(&site.id).unwrap().detected[0].issue.as_deref().unwrap().contains("冲突"));
        std::fs::remove_file(root.join(".node-version")).unwrap();
        let newer = serde_json::json!([{"version":"v1.1.0","lts":"First"}]);
        crate::versions::cache_node_lts_aliases(&state.store, newer.as_array().unwrap()).unwrap();
        let unavailable = state.project_runtime_versions(&site.id).unwrap();
        assert!(unavailable.detected[0].issue.as_deref().unwrap().contains("1.1.0"));
        assert!(unavailable.detected[0].resolved_version.is_none());
        state.save_project_runtime_versions(&site.id, &pins, &unavailable.revision).unwrap();
        assert_eq!(state.site_terminal_environment(&site.id).unwrap().entries.iter().find(|e| e.id == "node").unwrap().version, "2.0.0");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "lts/FiRsT");
    }

    #[test]
    fn project_detection_uses_highest_numeric_match_and_protects_resolved_uninstall() {
        let (_temp, mut state, site) = project_runtime_fixture();
        let root = project_directory(&site).unwrap();
        std::fs::write(root.join(".nvmrc"), "node\n").unwrap();
        let view = state.project_runtime_versions(&site.id).unwrap(); assert_eq!(view.detected[0].resolved_version.as_deref(), Some("2.0.0"));
        assert_eq!(state.uninstall_package("node@2.0.0").unwrap_err().code, "PACKAGE_IN_USE");
        assert!(project_references_version(&state.store, &site, "node", "1.0.0").is_ok_and(|used| !used));
        std::fs::write(root.join(".nvmrc"), "1\n").unwrap();
        let previous = state.site_terminal_environment(&site.id).unwrap();
        let mut additional = state.store.find_installed("node", Some("1.0.0")).unwrap();
        additional.version = "1.9.0".into(); state.store.upsert_installed(&additional).unwrap();
        let mut metadata = state.installer.manifest.packages.iter().find(|p| p.id == "node" && p.version == "1.0.0").unwrap().clone();
        metadata.version = additional.version.clone(); state.installer.manifest.packages.push(metadata);
        assert_eq!(state.project_runtime_versions(&site.id).unwrap().detected[0].resolved_version.as_deref(), Some("1.9.0"));
        assert_eq!(state.with_terminal_environment::<()>(Some(&site.id), &previous.revision, |_| panic!("changed installed selection launched")).unwrap_err().code, "TERMINAL_ENV_CHANGED");
        std::fs::write(root.join(".nvmrc"), "1.99\n").unwrap();
        assert!(state.project_runtime_versions(&site.id).unwrap().detected[0].issue.as_deref().unwrap().contains("没有符合"));
        assert_eq!(state.site_terminal_environment(&site.id).unwrap_err().code, "PROJECT_RUNTIME_DETECTION");
    }

    #[test]
    fn terminal_launch_rechecks_site_versions_directory_and_holds_lifecycle_locks() {
        let (temp, state, mut site) = site_terminal_fixture();
        let view = state.site_terminal_environment(&site.id).unwrap();
        let called = std::cell::Cell::new(false);
        state.with_terminal_environment(Some(&site.id), &view.revision, |actual| {
            assert_eq!(actual.revision, view.revision);
            std::thread::scope(|scope| scope.spawn(|| {
                assert!(state.manager.lifecycle.try_lock().is_none());
                assert!(crate::sites::SITE_CHANGES.try_lock().is_none());
            }).join().unwrap());
            called.set(true); Ok(())
        }).unwrap();
        assert!(called.get());
        site.runtime.php_version = Some("2.0.0".into()); state.store.save_site(&site).unwrap();
        assert_eq!(state.with_terminal_environment::<()>(Some(&site.id), &view.revision, |_| panic!("stale PHP launched")).unwrap_err().code, "TERMINAL_ENV_CHANGED");
        let view = state.site_terminal_environment(&site.id).unwrap();
        let other = temp.path().join("other"); std::fs::create_dir(&other).unwrap(); site.root_dir = other.to_string_lossy().into_owned(); state.store.save_site(&site).unwrap();
        assert_eq!(state.with_terminal_environment::<()>(Some(&site.id), &view.revision, |_| panic!("stale directory launched")).unwrap_err().code, "TERMINAL_ENV_CHANGED");
        site.runtime.php_version = Some("missing".into()); state.store.save_site(&site).unwrap();
        assert_eq!(state.site_terminal_environment(&site.id).unwrap_err().code, "TERMINAL_RUNTIME_UNAVAILABLE");
        let global = state.terminal_environment().unwrap();
        state.store.set_setting(SELECTED_KEY, "[]").unwrap();
        assert_eq!(state.with_terminal_environment::<()>(None, &global.revision, |_| panic!("stale selection launched")).unwrap_err().code, "TERMINAL_ENV_CHANGED");
        for invalid in ["", "\n", "a\0b"] { assert_eq!(terminal_directory(invalid).unwrap_err().code, "TERMINAL_DIRECTORY_INVALID"); }
        assert!(terminal_directory(temp.path().join("absent").to_str().unwrap()).is_err());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires NSB_ENV_NODE and NSB_ENV_PYTHON; runs short hidden commands, no interactive terminal"]
    fn native_project_terminal_resolves_pinned_node_and_python() {
        let (_temp, mut state, mut site) = site_terminal_fixture();
        site.runtime.kind = crate::model::SiteKind::Static; state.store.save_site(&site).unwrap();
        state.store.set_setting(SELECTED_KEY, "[]").unwrap();
        let mut versions = std::collections::BTreeMap::new();
        let mut executables = std::collections::BTreeMap::new();
        for (id, key) in [("node", "NSB_ENV_NODE"), ("python", "NSB_ENV_PYTHON")] {
            let exe = std::path::PathBuf::from(std::env::var_os(key).expect(key)).canonicalize().unwrap();
            let output = platform::command(&exe).arg("--version").output().unwrap();
            assert!(output.status.success());
            let version = String::from_utf8(output.stdout).unwrap().trim().trim_start_matches("Python ").trim_start_matches('v').to_string();
            assert!(!version.is_empty());
            let mut entry = state.installer.manifest.packages[0].clone(); entry.id = id.into(); entry.category = "runtime".into(); entry.version = version.clone(); entry.entry = format!("{id}.exe");
            state.installer.manifest.packages.push(entry);
            state.store.upsert_installed(&crate::model::InstalledPackage { id: id.into(), version: version.clone(), category: "runtime".into(), install_path: exe.parent().unwrap().to_string_lossy().into_owned(), config_path: String::new(), installed_at: 0 }).unwrap();
            versions.insert(id.into(), version); executables.insert(id, exe);
        }
        let root = project_directory(&site).unwrap();
        std::fs::write(root.join(".node-version"), format!("v{}\n", versions["node"])).unwrap();
        std::fs::write(root.join(".python-version"), format!("{}\n", versions["python"].rsplit_once('.').unwrap().0)).unwrap();
        assert!(!root.join(PROJECT_FILE).exists());
        for mode in ["numeric", "lts", "pin"] {
            let pin = mode == "pin";
            if mode == "lts" {
                let lts = platform::command(&executables["node"]).args(["-p", "process.release.lts"]).output().unwrap();
                assert!(lts.status.success());
                let name = String::from_utf8(lts.stdout).unwrap().trim().to_string();
                assert_ne!(name, "undefined", "native LTS verification requires an LTS Node binary");
                let index = serde_json::json!([{"version":format!("v{}", versions["node"]),"lts":name}]);
                crate::versions::cache_node_lts_aliases(&state.store, index.as_array().unwrap()).unwrap();
                std::fs::remove_file(root.join(".node-version")).unwrap();
                std::fs::write(root.join(".nvmrc"), "lts/*\n").unwrap();
            }
            if pin {
                let view = state.project_runtime_versions(&site.id).unwrap();
                state.save_project_runtime_versions(&site.id, &versions, &view.revision).unwrap();
            }
            let mut environment = state.site_terminal_environment(&site.id).unwrap();
            assert!(environment.entries.iter().all(|entry| entry.source.as_deref() == Some(if pin { PROJECT_FILE } else if entry.id == "node" { if mode == "lts" { ".nvmrc" } else { ".node-version" } } else { ".python-version" })));
            environment.script.push_str("\n[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n$nodePath=(Get-Command node -CommandType Application).Source\n$pythonPath=(Get-Command python -CommandType Application).Source\n$nodeVersion= & node --version\nif ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n$pythonVersion= & python --version\nif ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n[pscustomobject]@{nodePath=$nodePath;pythonPath=$pythonPath;nodeVersion=$nodeVersion;pythonVersion=$pythonVersion;cwd=(Get-Location).ProviderPath} | ConvertTo-Json -Compress");
            let args = powershell_terminal_args(&environment).unwrap().into_iter().filter(|arg| arg != "-NoExit").collect::<Vec<_>>();
            let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/WindowsPowerShell/v1.0/powershell.exe");
            let before = std::env::var_os("PATH");
            let output = platform::command(powershell).args(args).current_dir(terminal_directory(&environment.cwd).unwrap()).env("PATH", "C:/no-node-or-python").output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let actual: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            for id in ["node", "python"] { assert_eq!(std::path::Path::new(actual[format!("{id}Path")].as_str().unwrap()).canonicalize().unwrap(), executables[id]); }
            assert_eq!(actual["nodeVersion"], format!("v{}", versions["node"]));
            assert_eq!(actual["pythonVersion"], format!("Python {}", versions["python"]));
            assert_eq!(terminal_directory(actual["cwd"].as_str().unwrap()).unwrap(), terminal_directory(&environment.cwd).unwrap());
            assert_eq!(std::env::var_os("PATH"), before);
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires NSB_ENV_PHP; runs a short hidden PowerShell with real PHP, no interactive terminal"]
    fn native_site_terminal_resolves_real_php_and_literal_working_directory() {
        let php = std::path::PathBuf::from(std::env::var_os("NSB_ENV_PHP").expect("NSB_ENV_PHP")).canonicalize().unwrap();
        let (_temp, mut state, mut site) = site_terminal_fixture();
        let root = php.parent().unwrap();
        let mut entry = state.installer.manifest.packages[0].clone(); entry.version = "8.4.26".into(); entry.entry = "php-cgi.exe".into();
        state.installer.manifest.packages.push(entry);
        state.store.upsert_installed(&crate::model::InstalledPackage { id: "php".into(), version: "8.4.26".into(), category: "runtime".into(), install_path: root.to_string_lossy().into_owned(), config_path: String::new(), installed_at: 0 }).unwrap();
        site.runtime.php_version = Some("8.4.26".into()); state.store.save_site(&site).unwrap();
        let mut environment = state.site_terminal_environment(&site.id).unwrap();
        environment.script.push_str("\n[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n$resolved = (Get-Command php -CommandType Application).Source\n$version = & php -n -r 'echo PHP_VERSION;'\nif ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n[pscustomobject]@{ resolved=$resolved; version=$version; cwd=(Get-Location).ProviderPath } | ConvertTo-Json -Compress");
        let args = powershell_terminal_args(&environment).unwrap().into_iter().filter(|arg| arg != "-NoExit").collect::<Vec<_>>();
        let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let global_path = std::env::var_os("PATH");
        let output = platform::command(powershell).args(args).current_dir(terminal_directory(&environment.cwd).unwrap()).env("PATH", "C:/not-the-selected-php").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(std::path::Path::new(result["resolved"].as_str().unwrap()).canonicalize().unwrap(), php);
        assert_eq!(result["version"], "8.4.26");
        assert_eq!(std::path::Path::new(result["cwd"].as_str().unwrap()).canonicalize().unwrap(), terminal_directory(&environment.cwd).unwrap());
        assert_eq!(std::env::var_os("PATH"), global_path);
    }

    #[cfg(windows)]
    #[test]
    fn terminal_powershell_is_literal_idempotent_and_preserves_other_entries() {
        use base64::Engine;
        let dirs = v(&[
            "C:/Nice Env/O'Brien/$nsbInjection`&()‘’‚‛/bin",
            "D:/second/bin",
        ]);
        let script = render_terminal_script(&dirs, true).unwrap();
        let program = format!("$nsbInjection = 'EXPANDED'\n{script}\n{script}\nif (Get-Variable nsbDirs -ErrorAction SilentlyContinue) {{ throw 'scope leak' }}\n[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($env:PATH))");
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            program
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let output = platform::command(powershell)
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
            .env("PATH", "C:/Other;;d:/SECOND/bin/;C:/Other;")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = base64::engine::general_purpose::STANDARD
            .decode(String::from_utf8_lossy(&output.stdout).trim())
            .unwrap();
        assert_eq!(
            String::from_utf8(actual).unwrap(),
            format!("{};{};C:/Other;;C:/Other;", dirs[0], dirs[1])
        );
    }

    #[test]
    fn terminal_posix_is_literal_idempotent_and_preserves_other_entries() {
        // Windows 上用已有 Git Bash；无该工具的平台仍执行纯函数与 PowerShell 回归。
        #[cfg(windows)]
        let shell = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .filter(|dir| dir.join("git.exe").is_file())
            .flat_map(|dir| {
                dir.ancestors()
                    .take(3)
                    .map(|parent| parent.join("bin/bash.exe"))
                    .collect::<Vec<_>>()
            })
            .find(|path| path.is_file())
            .unwrap_or_default();
        #[cfg(not(windows))]
        let shell = std::path::PathBuf::from("/bin/sh");
        if !shell.is_file() {
            return;
        }
        let dirs = v(&["/Nice Env/O'Brien/$nsbInjection`&()\\/bin", "/second/bin"]);
        let script = render_terminal_script(&dirs, false).unwrap();
        let program = format!("PATH='/other::/second/bin:/other:'\nnsbInjection=EXPANDED\n{script}\n{script}\n[ -z \"${{nsb_result+x}}\" ] || exit 2\nprintf '%s' \"$PATH\"");
        // /bin/sh 不认识 Bash 启动参数。
        #[cfg(not(windows))]
        let output = platform::command("/bin/sh")
            .args(["-c", &program])
            .output()
            .unwrap();
        #[cfg(windows)]
        let output = platform::command(shell)
            .args(["--noprofile", "--norc", "-c", &program])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}:{}:/other::/other:", dirs[0], dirs[1])
        );
    }

    #[test]
    fn bin_dir_is_entrys_parent() {
        assert_eq!(
            bin_dir_for("D:/rt/php/8.3.33", "php-cgi.exe").unwrap(),
            "D:/rt/php/8.3.33"
        );
        assert_eq!(
            bin_dir_for("D:/rt/go/1.24.1", "go/bin/go.exe").unwrap(),
            "D:/rt/go/1.24.1/go/bin"
        );
        assert_eq!(
            bin_dir_for("D:/rt/mysql/8.0.46", "mysql-8.0.46-winx64/bin/mysqld.exe").unwrap(),
            "D:/rt/mysql/8.0.46/mysql-8.0.46-winx64/bin"
        );
        // 结尾分隔符不该导致双斜杠
        assert_eq!(
            bin_dir_for("D:/rt/nginx/1.26.3/", "nginx-1.26.3/nginx.exe").unwrap(),
            "D:/rt/nginx/1.26.3/nginx-1.26.3"
        );
    }

    #[test]
    fn non_executable_entries_are_skipped() {
        assert!(bin_dir_for("D:/rt/composer/2.8.5", "composer.phar").is_none());
        assert!(bin_dir_for("D:/rt/adminer/4.8.1", "adminer-4.8.1.php").is_none());
        assert!(bin_dir_for("D:/rt/geoip", "GeoLite2-City.mmdb").is_some());
    }

    #[test]
    fn merge_prepends_and_preserves_foreign_entries() {
        let existing = "%SystemRoot%\\system32;C:\\Other\\bin";
        let out = merge_win_path(existing, &[], &v(&["D:\\rt\\php\\8.3.33"]));
        assert_eq!(
            out,
            "D:\\rt\\php\\8.3.33;%SystemRoot%\\system32;C:\\Other\\bin"
        );
    }

    #[test]
    fn merge_removes_previously_managed_dirs() {
        let existing = "D:\\rt\\php\\8.3.33;C:\\Other\\bin";
        let out = merge_win_path(
            existing,
            &v(&["D:\\rt\\php\\8.3.33"]),
            &v(&["D:\\rt\\php\\8.4.25"]),
        );
        assert_eq!(out, "D:\\rt\\php\\8.4.25;C:\\Other\\bin");
    }

    #[test]
    fn merge_is_idempotent() {
        let dirs = v(&["D:\\rt\\php\\8.3.33", "D:\\rt\\go\\1.24.1"]);
        let once = merge_win_path("%SystemRoot%\\system32", &[], &dirs);
        let twice = merge_win_path(&once, &dirs, &dirs);
        assert_eq!(once, twice, "重复应用不应产生重复条目");
        assert_eq!(twice.matches("D:\\rt\\php").count(), 1);
    }

    #[test]
    fn merge_dedupes_within_existing_path() {
        let existing = "C:\\a;C:\\a;C:\\b";
        let out = merge_win_path(existing, &[], &[]);
        assert_eq!(out, "C:\\a;C:\\b");
    }

    #[test]
    fn strip_removes_only_managed() {
        let existing = "D:\\rt\\php\\8.3.33;C:\\Other\\bin;D:\\rt\\go\\1.24.1";
        let out = strip_win_path(existing, &v(&["D:\\rt\\php\\8.3.33", "D:\\rt\\go\\1.24.1"]));
        assert_eq!(out, "C:\\Other\\bin");
    }

    #[test]
    fn disabled_leaves_no_trace_in_utility() {
        // 关掉时 merge 传空 new_dirs：应等价于「清掉我们的，保留别人的」
        let existing = "D:\\rt\\php\\8.3.33;%SystemRoot%\\system32";
        let out = merge_win_path(existing, &v(&["D:\\rt\\php\\8.3.33"]), &[]);
        assert_eq!(out, "%SystemRoot%\\system32");
    }

    #[test]
    fn case_insensitive_matching_on_windows_semantics() {
        // 大小写不同视为同一条目（Windows 路径不区分大小写）
        let existing = "d:\\RT\\PHP\\8.3.33;C:\\keep";
        let out = merge_win_path(existing, &v(&["D:\\rt\\php\\8.3.33"]), &[]);
        assert_eq!(out, "C:\\keep");
    }

    #[test]
    fn trailing_slash_treated_as_same_path() {
        let out = merge_win_path(
            "D:\\rt\\php\\8.3.33\\;C:\\keep",
            &[],
            &v(&["D:\\rt\\php\\8.3.33"]),
        );
        assert_eq!(out, "D:\\rt\\php\\8.3.33;C:\\keep");
    }
}
