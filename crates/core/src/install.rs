//! 套件安装管线：下载(断点续传/校验) → 解压 → 生成默认配置 → 注册服务。

use crate::download::Downloader;
use crate::error::{AppError, Result};
use crate::model::InstalledPackage;
use crate::paths::Paths;
use crate::store::Store;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub struct Installer {
    pub manifest: crate::model::Manifest,
}

/// GitHub 直连的加速前缀，按近期可用性排序；仅对 github.com / *.githubusercontent.com 生效
const GH_ACCELERATORS: [&str; 3] = [
    "https://ghproxy.net/",
    "https://ghfast.top/",
    "https://gh-proxy.com/",
];

const QDRANT_WEB_ASSET: &str = "qdrant-web-ui-0.2.18";
const QDRANT_WEB_URL: &str = "https://github.com/qdrant/qdrant-web-ui/releases/download/v0.2.18/dist-qdrant.zip";
const QDRANT_WEB_SHA256: &str = "fdce24c04ec1627d2369cb8fe610ee06ad9236f82aad214aa7f294ac37372859";

/// 发行版 tag 可带一个 v 前缀，但安装记录、目录和前端筛选必须使用同一规范。
pub(crate) fn canonical_version(version: &str) -> &str {
    version.trim_start_matches(['v', 'V'])
}

pub(crate) fn same_version(left: &str, right: &str) -> bool {
    canonical_version(left) == canonical_version(right)
}

pub(crate) fn same_optional_version(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => same_version(left, right),
        (None, None) => true,
        _ => false,
    }
}

/// 检查清单中的依赖是否由已安装套件满足。依赖可以只写套件 ID，
/// 也可以写 `id@version`；历史安装记录的 `v` 前缀不应导致依赖被误报为缺失。
pub(crate) fn installed_package_satisfies(package: &InstalledPackage, dependency: &str) -> bool {
    match dependency.split_once('@') {
        Some((id, version)) => package.id == id && same_version(&package.version, version),
        None => package.id == dependency,
    }
}

pub(crate) fn official_qdrant(entry: &crate::model::PackageManifestEntry) -> bool {
    entry.id == "qdrant" && entry.url.starts_with("https://github.com/qdrant/qdrant/releases/download/")
        && entry.run.as_ref().is_some_and(|run| run.args == ["--config-path", "{etc}/config.yaml", "--disable-telemetry"]
            && run.config_file.as_deref() == Some("config.yaml") && run.cwd.is_none())
}

/// 只升级曾随应用发布的原始运行描述；下载信息、实际入口及用户修改保持不变。
fn upgrade_legacy_run(mut entry: crate::model::PackageManifestEntry) -> crate::model::PackageManifestEntry {
    if entry.id == "rustfs" && entry.url.starts_with("https://github.com/rustfs/rustfs/releases/download/") {
        let legacy: crate::model::ServiceRunSpec = serde_json::from_value(serde_json::json!({
            "args": ["server", "--address", ":{port}", "--console-enable", "--console-address", ":{port+1}", "{data}/rustfs-data"],
            "health": "tcp", "healthTimeoutSec": 20, "initDirs": ["rustfs-data"]
        })).expect("内置旧运行描述合法");
        if entry.run.as_ref().is_some_and(|run| serde_json::to_value(run).ok() == serde_json::to_value(&legacy).ok()) {
            // 上游会再次按空白切分 VOLUMES；相对路径避免拆开用户数据目录。
            let run = entry.run.as_mut().unwrap();
            *run.args.last_mut().unwrap() = ".".into();
            run.cwd = Some("{data}/rustfs-data".into());
        }
        return entry;
    }
    if entry.id == "mariadb" {
        let bundled: crate::model::Manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).expect("内置清单合法");
        if let Some(current) = bundled.packages.into_iter().find(|p| p.id == "mariadb").and_then(|p| p.run) {
            let mut legacy = current.clone();
            legacy.init_args = Some(vec!["--datadir={data}".into(), "--service=MariaDB".into()]);
            legacy.init_dirs = vec!["data".into()];
            if entry.run.as_ref().is_some_and(|run| serde_json::to_value(run).ok() == serde_json::to_value(&legacy).ok()) {
                entry.run = Some(current);
            }
        }
        return entry;
    }
    if entry.id == "consul" {
        let legacy: crate::model::ServiceRunSpec = serde_json::from_value(serde_json::json!({
            "args": ["agent", "-dev", "-client", "127.0.0.1", "-http-port", "{port}"],
            "health": "tcp", "healthTimeoutSec": 20
        })).expect("内置旧运行描述合法");
        if entry.run.as_ref().is_some_and(|run| serde_json::to_value(run).ok() == serde_json::to_value(&legacy).ok()) {
            let bundled: crate::model::Manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).expect("内置清单合法");
            entry.run = bundled.packages.into_iter().find(|p| p.id == "consul").and_then(|p| p.run);
            if entry.description == "服务发现与配置中心（开发模式，自带 UI）" {
                entry.description = "服务发现与配置中心（本机单节点，持久化数据，自带 UI）".into();
            }
        }
        return entry;
    }
    if entry.id == "sftpgo" {
        let legacy: crate::model::ServiceRunSpec = serde_json::from_value(serde_json::json!({
            "args": ["serve", "--config-dir", "{etc}", "--log-file-path", "{data}/sftpgo.log"],
            "health": "tcp", "healthTimeoutSec": 20
        })).expect("内置旧运行描述合法");
        if entry.run.as_ref().is_some_and(|run| serde_json::to_value(run).ok() == serde_json::to_value(&legacy).ok()) {
            let env = std::collections::HashMap::from([
                ("SFTPGO_SFTPD__BINDINGS__0__PORT".into(), "{port}".into()),
                ("SFTPGO_HTTPD__BINDINGS__0__PORT".into(), "{port+6058}".into()),
            ]);
            entry.run.as_mut().unwrap().env = Some(env);
        }
        return entry;
    }
    if entry.id == "rabbitmq" {
        let legacy: crate::model::ServiceRunSpec = serde_json::from_value(serde_json::json!({
            "args": [], "health": "tcp", "healthTimeoutSec": 40,
            "requires": ["erlang"],
            "env": { "RABBITMQ_BASE": "{data}", "RABBITMQ_NODE_PORT": "{port}" }
        })).expect("内置旧 RabbitMQ 运行描述合法");
        if entry.run.as_ref().is_some_and(|run| serde_json::to_value(run).ok() == serde_json::to_value(&legacy).ok()) {
            let bundled: crate::model::Manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json"))
                .expect("内置清单 JSON 必须合法");
            entry.run = bundled.packages.into_iter()
                .find(|candidate| candidate.id == "rabbitmq" && same_version(&candidate.version, &entry.version))
                .and_then(|candidate| candidate.run);
        }
        return entry;
    }
    if entry.id != "rnacos" { return entry; }
    let legacy: crate::model::ServiceRunSpec = serde_json::from_value(serde_json::json!({
        "args": [], "health": "tcp", "healthTimeoutSec": 20,
        "env": { "RNACOS_HTTP_PORT": "{port}", "RNACOS_DATA_DIR": "{data}/nacos_db" },
        "configFile": ".env",
        "configTemplate": "RNACOS_HTTP_PORT={port}\nRNACOS_GRPC_PORT={port+1000}\nRNACOS_HTTP_CONSOLE_PORT={port+2000}\nRNACOS_DATA_DIR={data}/nacos_db\nRNACOS_CONFIG_DB_FILE={data}/nacos_db/config.db\nRNACOS_NAMING_DB_FILE={data}/nacos_db/naming.db\n"
    })).expect("内置旧运行描述合法");
    if entry.run.as_ref().is_some_and(|run| serde_json::to_value(run).ok() == serde_json::to_value(&legacy).ok()) {
        // 显式使用 Windows 清单：历史安装快照可能来自另一台机器。
        let bundled: crate::model::Manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json"))
            .expect("内置清单 JSON 必须合法");
        entry.run = bundled.packages.into_iter().find(|p| p.id == "rnacos").and_then(|p| p.run);
    }
    entry
}

pub(crate) fn is_sftpgo_installer(entry: &crate::model::PackageManifestEntry) -> bool {
    entry.id == "sftpgo" && (entry.entry.ends_with("_windows_x86_64.exe")
        || entry.url.ends_with("_windows_x86_64.exe"))
}

fn official_apache(entry: &crate::model::PackageManifestEntry) -> bool {
    // 历史快照可能在非 Windows 主机上被读取；下载地址和固定入口已经足够
    // 识别这条官方构建，不能因为快照被标记为当前平台就跳过失效地址修复。
    // 没有显式 version_source 时，Windows 清单会通过内置源识别；跨平台读取
    // 旧快照时 source_for 可能因平台筛选返回 None，因此这里仅在显式声明时校验来源。
    entry.id == "apache"
        && entry.kind == "archive" && entry.entry == "Apache24/bin/httpd.exe" && entry.mirrors.is_empty()
        && entry.url.starts_with("https://www.apachelounge.com/download/VS")
        && entry.url.contains(&format!("/binaries/httpd-{}-", entry.version))
        && entry.url.ends_with(".zip")
        && entry.version_source.as_ref().is_none_or(|source| source.kind == "apache")
}

/// 修正历史可下载条目；已安装快照保留实际安装时的下载信息和入口。
fn upgrade_available_entry(entry: crate::model::PackageManifestEntry) -> crate::model::PackageManifestEntry {
    let mut entry = upgrade_legacy_run(entry);
    // 旧远端清单快照会覆盖新内置清单。只修正曾发布过的失效 URL/哈希组合，保留用户自定义来源。
    if official_apache(&entry) && entry.version == "2.4.68"
        && entry.url == "https://www.apachelounge.com/download/VS18/binaries/httpd-2.4.68-260827-Win64-VS18.zip"
        && entry.sha256.as_deref() == Some("a6b7de9fdccb28456f5b1f884920fe0b2425aadfca25c63ae1c0969d43bb355b") {
        let bundled: crate::model::Manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).expect("内置清单合法");
        if let Some(correct) = bundled.packages.into_iter().find(|p| p.id == entry.id && p.version == entry.version) {
            entry.url = correct.url; entry.sha256 = correct.sha256; entry.size_bytes = correct.size_bytes;
        }
    }
    if is_sftpgo_installer(&entry)
        && entry.url == format!("https://github.com/drakkan/sftpgo/releases/download/v{0}/sftpgo_v{0}_windows_x86_64.exe", entry.version) {
        let bundled: crate::model::Manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).expect("内置清单合法");
        if let Some(correct) = bundled.packages.into_iter().find(|p| p.id == entry.id && p.version == entry.version && p.kind == "archive") {
            entry.entry = correct.entry; entry.kind = correct.kind; entry.url = correct.url;
            entry.size_bytes = correct.size_bytes; entry.sha256 = correct.sha256; entry.mirrors.clear();
        }
    }
    entry
}

fn is_github_url(url: &str) -> bool {
    url.starts_with("https://github.com/")
        || url.starts_with("https://raw.githubusercontent.com/")
        || url.starts_with("https://objects.githubusercontent.com/")
}

impl Installer {
    pub fn bundled() -> Self {
        #[cfg(windows)]
        let raw = include_str!("../../../manifest/packages.win.json");
        #[cfg(target_os = "macos")]
        let raw = include_str!("../../../manifest/packages.mac.json");
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        // 当前发布清单只覆盖 Windows 和 macOS；Linux 不能误装 macOS 二进制。
        // Linux 构建仍可运行核心服务，但套件列表必须为空，等有对应清单后再开放。
        let raw = r#"{"revision":0,"packages":[]}"#;
        let manifest: crate::model::Manifest =
            serde_json::from_str(raw).expect("内置清单 JSON 必须合法");
        Self { manifest }
    }

    /// 构造「生效清单」：内置 → 叠加远端快照（etc/manifest.json，若存在且合法）
    /// → 叠加用户自定义模块（user-modules/*.json）。
    /// 任何一层损坏都跳过该层，绝不因外部文件让 App 起不来。
    pub fn effective(paths: &Paths) -> Self {
        let mut inst = Self::bundled();

        // 层 1：远端清单快照
        let snap = paths.etc().join("manifest.json");
        if snap.is_file() {
            if let Ok(raw) = std::fs::read_to_string(&snap) {
                match parse_manifest_str(&raw) {
                    Ok(m) => {
                        // 快照只覆盖它包含的条目，不能隐藏应用升级后新增的内置套件。
                        inst.manifest.revision = m.revision;
                        inst.merge_manifest(m);
                    }
                    Err(_) => { /* 损坏快照：忽略，继续用内置 */ }
                }
            }
        }

        // 层 2：用户自定义模块（同 id+version 覆盖内置，否则追加）
        let dir = paths.base.join("user-modules");
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut files: Vec<_> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
                .collect();
            files.sort();
            for f in files {
                let Ok(raw) = std::fs::read_to_string(&f) else {
                    continue;
                };
                let Ok(m) = parse_manifest_str(&raw) else {
                    continue;
                };
                inst.merge_manifest(m);
            }
        }
        inst
    }

    /// 同 id/version 按平台范围覆盖；不同架构共存，未限制平台的用户条目覆盖全部变体。
    pub fn merge_manifest(&mut self, other: crate::model::Manifest) {
        for p in other.packages {
            let covers = |new: &[String], old: &[String]| {
                new.is_empty() || !old.is_empty() && old.iter().all(|value| new.contains(value))
            };
            self.manifest.packages.retain(|e| {
                !(e.id == p.id && same_version(&e.version, &p.version)
                    && covers(&p.os, &e.os) && covers(&p.arch, &e.arch))
            });
            self.manifest.packages.push(p);
        }
    }

    /// 从数据目录中的运行时文件夹重新建立安装记录。
    ///
    /// 安装记录可能因旧版本升级、数据库恢复或用户迁移数据目录而缺失，
    /// 但运行时目录和安装快照仍然存在。扫描只接受安全的两级路径，并且
    /// 必须能从快照/当前清单确认入口文件真实存在，避免把失败安装的空目录
    /// 或用户随手创建的目录误判为已安装套件。
    pub fn reconcile_installed(
        &self,
        paths: &Paths,
        store: &Store,
    ) -> Result<crate::model::PackageReconcileResult> {
        let root = paths.runtimes();
        if !root.exists() {
            return Ok(crate::model::PackageReconcileResult::default());
        }
        let mut known = store.list_installed()?;
        let mut result = crate::model::PackageReconcileResult::default();
        let ids = std::fs::read_dir(&root).map_err(|error| AppError::io("扫描本地运行时", error))?;
        for id_entry in ids.flatten() {
            let id_path = id_entry.path();
            let id_type = std::fs::symlink_metadata(&id_path).ok();
            if !id_type.is_some_and(|metadata| metadata.file_type().is_dir()) {
                continue;
            }
            let id = id_entry.file_name().to_string_lossy().to_string();
            if !is_safe_path_component(&id) {
                continue;
            }
            let Ok(versions) = std::fs::read_dir(&id_path) else { continue; };
            for version_entry in versions.flatten() {
                let runtime_dir = version_entry.path();
                let version_type = std::fs::symlink_metadata(&runtime_dir).ok();
                if !version_type.is_some_and(|metadata| metadata.file_type().is_dir()) {
                    continue;
                }
                let folder_version = version_entry.file_name().to_string_lossy().to_string();
                if !is_safe_path_component(&folder_version) {
                    continue;
                }
                let snapshot = runtime_dir.join(".niceenv-package.json");
                let snapshot_entry = std::fs::read_to_string(&snapshot)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<crate::model::PackageManifestEntry>(&raw).ok());
                let mut entry = match snapshot_entry {
                    Some(entry) if entry.id == id && same_version(&entry.version, &folder_version) => entry,
                    Some(_) => continue,
                    None => match self.find(&format!("{id}@{folder_version}")) {
                        Some(entry) => entry,
                        None => {
                            // 旧版安装可能没有保存快照，清单更新后也可能不再列出该版本。
                            // 只要套件仍有模板，就按版本源的入口模板恢复；下面还会
                            // 再检查真实入口文件，避免把空目录或陌生目录当成已安装。
                            let Some(mut entry) = self.template_for(&id) else {
                                continue;
                            };
                            let version = canonical_version(&folder_version).to_string();
                            let source = crate::versions::source_for(&entry);
                            entry.entry = source
                                .and_then(|source| source.entry_template)
                                .filter(|template| !template.contains("{asset}"))
                                .map(|template| template.replace("{version}", &version))
                                .unwrap_or_else(|| entry.entry.replace(&entry.version, &version));
                            if entry.entry.trim().is_empty() || entry.entry.contains('{') {
                                continue;
                            }
                            entry.version = version;
                            entry.url.clear();
                            entry.mirrors.clear();
                            entry.sha256 = None;
                            entry.size_bytes = 0;
                            entry
                        }
                    },
                };
                let version = canonical_version(&folder_version).to_string();
                ensure_safe_key(&id, &version)?;
                if !runtime_entry_exists(&runtime_dir, &entry.entry) {
                    continue;
                }
                entry.version = version.clone();
                let current = known.iter().find(|installed| installed.id == id && same_version(&installed.version, &version));
                let installed = crate::model::InstalledPackage {
                    id: id.clone(),
                    version: current.map(|installed| installed.version.clone()).unwrap_or(version),
                    category: entry.category.clone(),
                    install_path: runtime_dir.to_string_lossy().to_string(),
                    config_path: current.map(|installed| installed.config_path.clone())
                        .filter(|path| !path.trim().is_empty())
                        .unwrap_or_else(|| paths.etc_dir(&id, &entry.version).to_string_lossy().to_string()),
                    installed_at: current.map(|installed| installed.installed_at)
                        .filter(|timestamp| *timestamp > 0)
                        .unwrap_or_else(|| modified_at_ms(&runtime_dir)),
                };
                let changed = current.is_none_or(|previous| previous.category != installed.category
                    || previous.install_path != installed.install_path
                    || previous.config_path != installed.config_path);
                store.upsert_installed(&installed)?;
                if let Some(previous) = current {
                    if changed {
                        result.refreshed.push(installed.clone());
                    } else {
                        // Keep the in-memory list in sync without reporting a no-op.
                        let _ = previous;
                    }
                } else {
                    result.imported.push(installed.clone());
                }
                if let Some(previous) = known.iter_mut().find(|item| item.id == installed.id && same_version(&item.version, &installed.version)) {
                    *previous = installed;
                } else {
                    known.push(installed);
                }
            }
        }
        Ok(result)
    }

    pub fn find(&self, key: &str) -> Option<crate::model::PackageManifestEntry> {
        // key: "nginx" | "php@8.3.33" | "mysql"（取最新版）
        let (id, version) = match key.split_once('@') {
            Some((i, v)) => (i, Some(v)),
            None => (key, None),
        };
        let mut candidates: Vec<_> = self
            .manifest
            .packages
            .iter()
            .filter(|p| p.id == id && version.map_or(true, |v| same_version(&p.version, v)))
            .cloned()
            .collect();
        // 按版本号语义取最新：字符串比较会把 5.26.30 排在 2025.09.0 前、21.0.9 排在 21.0.12 前
        candidates.sort_by(|a, b| {
            Self::is_platform_compatible(b).cmp(&Self::is_platform_compatible(a))
                .then_with(|| crate::versions::is_prerelease(&a.version).cmp(&crate::versions::is_prerelease(&b.version)))
                .then_with(|| crate::versions::cmp_version_desc(&a.version, &b.version))
                .then_with(|| a.arch.is_empty().cmp(&b.arch.is_empty()))
                .then_with(|| a.os.is_empty().cmp(&b.os.is_empty()))
        });
        candidates.into_iter().next().map(upgrade_available_entry)
    }

    /// 找合成远程版本的模板：优先当前平台、显式版本源与较新版本。
    pub fn template_for(&self, id: &str) -> Option<crate::model::PackageManifestEntry> {
        let mut entries: Vec<_> = self
            .manifest
            .packages
            .iter()
            .filter(|p| p.id == id)
            .collect();
        entries.sort_by(|a, b| {
            Self::is_platform_compatible(b)
                .cmp(&Self::is_platform_compatible(a))
                .then_with(|| a.arch.is_empty().cmp(&b.arch.is_empty()))
                .then_with(|| a.os.is_empty().cmp(&b.os.is_empty()))
                .then_with(|| b.version_source.is_some().cmp(&a.version_source.is_some()))
                .then_with(|| crate::versions::is_prerelease(&a.version).cmp(&crate::versions::is_prerelease(&b.version)))
                .then_with(|| crate::versions::cmp_version_desc(&a.version, &b.version))
        });
        entries.first().map(|p| upgrade_available_entry((*p).clone()))
    }

    /// 安装记录是已安装版本的依据。优先读取安装时保存的实际入口和服务描述，
    /// 旧版安装未保存描述时，再用清单的同版本条目或同套件模板恢复。
    pub fn installed_entry(
        &self,
        installed: &InstalledPackage,
    ) -> crate::model::PackageManifestEntry {
        let snapshot = Path::new(&installed.install_path).join(".niceenv-package.json");
        if let Some(mut entry) = std::fs::read_to_string(snapshot)
            .ok()
            .and_then(|raw| serde_json::from_str::<crate::model::PackageManifestEntry>(&raw).ok())
            .filter(|entry| entry.id == installed.id && same_version(&entry.version, &installed.version))
        {
            // 安装记录是版本身份的最终来源；快照只提供真实入口和运行描述。
            // 这样带 v 前缀的历史记录不会在 API 列表里变成另一套版本身份。
            entry.version = installed.version.clone();
            return upgrade_legacy_run(entry);
        }
        if let Some(mut entry) = self.find(&format!("{}@{}", installed.id, installed.version)) {
            entry.version = installed.version.clone();
            return entry;
        }
        if let Some(mut entry) = self.template_for(&installed.id) {
            let source = crate::versions::source_for(&entry);
            entry.entry = source
                .and_then(|s| s.entry_template)
                .filter(|tpl| !tpl.contains("{asset}"))
                .map(|tpl| tpl.replace("{version}", &installed.version))
                .unwrap_or_else(|| entry.entry.replace(&entry.version, &installed.version));
            // 旧 Windows Redis 安装可能使用 5.0.14 兼容标识或清单外 3.x/4.x。
            // 新版源的嵌套目录不能用于恢复这些历史安装的入口（例如加入 PATH）。
            if installed.id == "redis" && Path::new(&installed.install_path).join("redis-server.exe").is_file() {
                entry.entry = "redis-server.exe".into();
            }
            entry.display_name = entry
                .display_name
                .replace(&entry.version, &installed.version);
            entry.version = installed.version.clone();
            // 历史安装只能恢复运行描述，不能沿用另一个版本的下载信息。
            entry.url.clear();
            entry.mirrors.clear();
            entry.sha256 = None;
            entry.size_bytes = 0;
            return entry;
        }
        // 用户模块已被移除时仍保留安装记录的展示和卸载入口，不猜测启动命令。
        crate::model::PackageManifestEntry {
            id: installed.id.clone(),
            version: installed.version.clone(),
            category: installed.category.clone(),
            display_name: installed.id.clone(),
            description: String::new(),
            homepage: None,
            note: None,
            os: vec![],
            arch: vec![],
            kind: "binary".into(),
            url: String::new(),
            mirrors: vec![],
            sha256: None,
            size_bytes: 0,
            entry: String::new(),
            default_port: None,
            depends: vec![],
            run: None,
            requires: vec![],
            version_source: None,
        }
    }

    pub fn package_views(&self, installed: &[InstalledPackage]) -> Vec<crate::model::PackageView> {
        // UI/安装记录使用 id@version 作为标识，同版本的多架构包只展示本机适用的一项。
        // 上游 tag 有时带 v 前缀，必须按规范化版本去重，否则会出现同一版本两行。
        let mut seen = std::collections::HashSet::new();
        let mut entries: Vec<_> = self.manifest.packages.iter()
            // macOS 清单同时保存 arm64 与 x64；必须先按当前平台过滤，再按
            // id+版本去重，否则排在前面的另一架构条目会污染套件列表。
            .filter(|p| Self::is_platform_compatible(p))
            .filter(|p| seen.insert((p.id.clone(), canonical_version(&p.version).to_owned())))
            .filter_map(|p| self.find(&format!("{}@{}", p.id, p.version)))
            .collect();
        for package in installed {
            let entry = self.installed_entry(package);
            if let Some(current) = entries
                .iter_mut()
                .find(|p| p.id == package.id && same_version(&p.version, &package.version))
            {
                *current = entry;
            } else {
                entries.push(entry);
            }
        }
        entries
            .iter()
            .map(|entry| {
                let mut available_versions: Vec<_> = entries
                    .iter()
                    .filter(|p| p.id == entry.id)
                    .map(|p| canonical_version(&p.version).to_owned())
                    .collect();
                available_versions.sort_by(|a, b| crate::versions::cmp_version_desc(a, b));
                available_versions.dedup();
                crate::model::PackageView {
                    manifest: entry.clone(),
                    install: installed
                        .iter()
                        .find(|p| p.id == entry.id && same_version(&p.version, &entry.version))
                        .cloned(),
                    available_versions,
                    active: false,
                }
            })
            .collect()
    }

    /// 把远程枚举到的版本合成为可安装条目：继承模板的 category/run/entry 结构，
    /// 只替换 version / url / sha256 / sizeBytes / entry。
    /// 这样安装流程（下载→校验→解压→注册服务）与内置版本完全一致。
    pub fn entry_from_remote(
        template: &crate::model::PackageManifestEntry,
        remote: &crate::model::RemoteVersion,
    ) -> crate::model::PackageManifestEntry {
        let mut e = template.clone();
        // GitHub tags commonly use `v1.2.3`; keep the installed key and folder
        // consistent with manifest versions while retaining the real download URL.
        let version = canonical_version(&remote.version).to_owned();
        e.version = version.clone();
        e.display_name = e.display_name.replace(&template.version, &version);
        e.url = remote.url.clone();
        let same_file =
            same_version(&template.version, &remote.version) && template.url == remote.url;
        e.sha256 = remote
            .sha256
            .clone()
            .or_else(|| same_file.then(|| template.sha256.clone()).flatten());
        e.size_bytes = remote
            .size_bytes
            .unwrap_or(if same_file { template.size_bytes } else { 0 });
        e.kind = remote.kind.clone();
        e.entry = remote.entry.clone();
        // 远程版本默认不带 mirrors（镜像策略仍在下载时按域名前缀应用）
        e.mirrors.clear();
        e
    }

    fn download_refresh_candidate(
        entry: &crate::model::PackageManifestEntry,
        catalog: &crate::model::VersionCatalog,
        failure: &AppError,
    ) -> Result<Option<crate::model::PackageManifestEntry>> {
        // 过期缓存和网络失败不能证明旧版本失效，也不能用它们替换校验信息。
        if !catalog.online || catalog.error.is_some() {
            return Ok(None);
        }
        let Some(remote) = catalog
            .remote
            .iter()
            .find(|remote| same_version(&remote.version, &entry.version))
        else {
            if matches!(
                failure.code.as_str(),
                "DOWNLOAD_NOT_FOUND" | "DOWNLOAD_NOT_PACKAGE"
            ) {
                let suggestion = catalog
                    .remote
                    .iter()
                    .find(|remote| !remote.prerelease)
                    .map(|remote| format!("，例如 {}", remote.version))
                    .unwrap_or_default();
                let mut error = AppError::new("PACKAGE_DOWNLOAD_UNAVAILABLE", format!("{} {} 的下载地址当前不可用", entry.display_name, entry.version))
                    .with_hint(format!("已刷新上游列表，但未找到该版本。请选择列表中的其它版本{suggestion}；不会自动替换成不同版本"));
                error.detail = failure.detail.clone();
                return Err(error);
            }
            return Ok(None);
        };
        let refreshed = Self::entry_from_remote(entry, remote);
        if failure.code == "CHECKSUM_MISMATCH" && refreshed.sha256.is_none() {
            return Ok(None);
        }
        // 同一地址未提供新哈希时保留已知校验，不能通过取消校验来消除失败。
        Ok((refreshed.url != entry.url
            || (refreshed.sha256.is_some() && refreshed.sha256 != entry.sha256))
            .then_some(refreshed))
    }

    async fn download_entry(
        &self,
        mut entry: crate::model::PackageManifestEntry,
        paths: &Paths,
        store: &Store,
        downloader: &Arc<Downloader>,
        task: &crate::download::DownloadTask,
        emit: &dyn Fn(crate::Event),
    ) -> Result<(crate::model::PackageManifestEntry, PathBuf)> {
        let mut refreshed = false;
        loop {
            let urls = self.candidate_urls(&entry, store);
            let result = downloader
                .download_with_task(
                    task,
                    &urls,
                    entry.sha256.as_deref().unwrap_or("0"),
                    entry.size_bytes,
                    paths,
                    emit,
                )
                .await;
            let failure = match result {
                Ok(archive) => return Ok((entry, archive)),
                Err(failure) => failure,
            };
            if refreshed
                || !matches!(
                    failure.code.as_str(),
                    "DOWNLOAD_NOT_FOUND" | "DOWNLOAD_NOT_PACKAGE" | "CHECKSUM_MISMATCH"
                )
                || !crate::versions::source_for(&entry)
                    .is_some_and(|source| source.kind != "static")
            {
                return Err(failure);
            }
            let catalog = tokio::select! {
                biased;
                _ = task.cancelled() => return Err(AppError::new("CANCELLED", "安装已取消")),
                catalog = crate::versions::catalog(store, &entry, true) => catalog,
            };
            let Some(mut updated) = Self::download_refresh_candidate(&entry, &catalog, &failure)?
            else {
                return Err(failure);
            };
            if updated.id == "node" && updated.sha256.is_none() {
                updated.sha256 = Some(tokio::select! {
                    biased;
                    _ = task.cancelled() => return Err(AppError::new("CANCELLED", "安装已取消")),
                    checksum = crate::versions::node_sha256(&updated.version, &updated.url) => checksum?,
                });
            }
            downloader.discard_task_partial(task, paths)?;
            entry = updated;
            refreshed = true;
        }
    }

    /// 解析安装 key，支持「清单里没有但版本源枚举得到」的版本：
    /// 先查清单，未命中则查版本目录缓存/远程；Apache 同版本重构建优先采用当前官方链接。
    pub async fn resolve_entry(
        &self,
        key: &str,
        store: &Store,
    ) -> Result<Option<crate::model::PackageManifestEntry>> {
        if let Some(hit) = self.find(key) {
            if store
                .list_installed()
                .ok()
                .is_none_or(|installed| !installed.iter().any(|p| p.id == hit.id && same_version(&p.version, &hit.version)))
                && crate::versions::source_for(&hit).is_some()
            {
                // 版本源是安装信息的最终来源：清单中的旧 URL 不能覆盖上游当前构建。
                // 普通安装复用六小时缓存，Apache 仍强制刷新以规避官方替换同版本构建。
                let catalog = crate::versions::catalog(store, &hit, official_apache(&hit)).await;
                if let Some(remote) = catalog
                    .remote
                    .iter()
                    .find(|r| same_version(&r.version, &hit.version))
                {
                    return Ok(Some(Self::entry_from_remote(&hit, remote)));
                }
            }
            return Ok(Some(hit));
        }
        let Some((id, version)) = key.split_once('@') else {
            return Ok(None);
        };
        let Some(template) = self.template_for(id) else {
            return Ok(None);
        };
        let cat = crate::versions::catalog(store, &template, official_apache(&template)).await;
        let Some(remote) = cat.remote.iter().find(|r| same_version(&r.version, version)) else {
            return Ok(None);
        };
        let mut remote = remote.clone();
        if id == "node" && remote.sha256.is_none() {
            // 不因未列在清单中就跳过 Node 官方提供的完整性校验。
            remote.sha256 = Some(crate::versions::node_sha256(version, &remote.url).await?);
        }
        Ok(Some(Self::entry_from_remote(&template, &remote)))
    }

    /// 镜像策略：official → [url] + mirrors；ghproxy → github 前缀加速；custom → 自定义前缀。
    /// 无论镜像怎么设置，GitHub 直连失败后都会自动尝试加速前缀兜底 ——
    /// 默认设置下被墙不再需要用户手动到设置里切镜像再装一遍。
    fn candidate_urls(
        &self,
        entry: &crate::model::PackageManifestEntry,
        store: &Store,
    ) -> Vec<String> {
        let mirror = store
            .get_setting("mirror")
            .unwrap_or_else(|| "official".into());
        let gh = is_github_url(&entry.url);
        let mut urls = vec![entry.url.clone()];
        match mirror.as_str() {
            "ghproxy" => {
                if gh {
                    // 加速源优先，直连殿后
                    let mut acc: Vec<String> = GH_ACCELERATORS
                        .iter()
                        .map(|p| format!("{p}{}", entry.url))
                        .collect();
                    acc.extend(urls.drain(..));
                    urls = acc;
                }
            }
            "custom" => {
                if let Some(prefix) = store.get_setting("customMirror") {
                    if !prefix.is_empty() {
                        let name = entry.url.rsplit('/').next().unwrap_or("").to_string();
                        urls.insert(0, format!("{prefix}/{name}"));
                    }
                }
            }
            _ => {}
        }
        if gh {
            for p in GH_ACCELERATORS {
                let u = format!("{p}{}", entry.url);
                if !urls.contains(&u) {
                    urls.push(u);
                }
            }
        }
        urls.extend(entry.mirrors.iter().cloned());
        urls.dedup();
        urls
    }

    /// 返回实际安装时会尝试的下载地址，供版本验收和诊断工具复用同一套镜像回退逻辑。
    pub fn candidate_urls_for(
        &self,
        entry: &crate::model::PackageManifestEntry,
        store: &Store,
    ) -> Vec<String> {
        self.candidate_urls(entry, store)
    }

    pub async fn install(
        &self,
        key: &str,
        paths: &Paths,
        store: &Store,
        downloader: &Arc<Downloader>,
        emit: &dyn Fn(crate::Event),
    ) -> Result<InstalledPackage> {
        self.install_for_request(key, None, paths, store, downloader, emit).await
    }

    pub(crate) async fn install_for_request(
        &self, key: &str, request_id: Option<&str>, paths: &Paths, store: &Store,
        downloader: &Arc<Downloader>, emit: &dyn Fn(crate::Event),
    ) -> Result<InstalledPackage> {
        let task_id = self
            .find(key)
            .map(|entry| format!("{}@{}", entry.id, entry.version))
            .unwrap_or_else(|| key.to_string());
        let task = downloader.begin_task_for_request(&task_id, request_id)?;
        let emit_request = |mut event: crate::Event| {
            if let crate::Event::DownloadProgress(progress) = &mut event {
                progress.request_id = request_id.map(str::to_owned);
            }
            emit(event);
        };
        let result = self
            .install_task(key, paths, store, downloader, &task, &emit_request)
            .await;
        if let Err(err) = &result {
            emit_request(crate::Event::DownloadProgress(
                crate::model::DownloadProgress {
                    task_id,
                    request_id: None,
                    received: 0,
                    total: 0,
                    speed_bps: 0,
                    eta_sec: 0.0,
                    state: if err.code == "CANCELLED" {
                        "cancelled"
                    } else {
                        "error"
                    }
                    .into(),
                    error: Some(err.message.clone()),
                },
            ));
        }
        result
    }

    /// 官方原生发行包只包含主程序；补齐官方静态 UI，不修改程序、数据或配置。
    async fn ensure_qdrant_web_ui(
        &self, entry: &crate::model::PackageManifestEntry, root: &Path, paths: &Paths, store: &Store,
        downloader: &Arc<Downloader>, task: &crate::download::DownloadTask,
        emit: &dyn Fn(crate::Event), published: bool,
    ) -> Result<()> {
        if !official_qdrant(entry) {
            return Ok(());
        }
        let relative = root
            .strip_prefix(&paths.base)
            .map_err(|_| AppError::new("INVALID_PACKAGE_PATH", "Qdrant 程序目录不在托管目录内"))?;
        let root = crate::paths::checked_data_path(
            &paths.base,
            &crate::paths::portable_path_text(relative),
        )?;
        let target = crate::paths::checked_data_path(&root, "static")?;
        if target.exists() {
            if crate::paths::checked_data_path(&target, "index.html")?.is_file() { return Ok(()); }
            return Err(AppError::new("QDRANT_WEB_INCOMPLETE", "Qdrant 已有管理台目录缺少 index.html，原目录已保留")
                .with_hint("请先备份并修复或移走程序目录中的 static，再重试补齐管理台。"));
        }
        emit(crate::Event::state(task.id(), "downloading"));
        let mut asset = entry.clone(); asset.url = QDRANT_WEB_URL.into(); asset.mirrors.clear();
        let archive = downloader.download_asset_with_task(task, QDRANT_WEB_ASSET, &self.candidate_urls(&asset, store),
            QDRANT_WEB_SHA256, 7_189_135, paths, emit).await?;
        emit(crate::Event::state(task.id(), "extracting"));
        let staging = tempfile::Builder::new().prefix(".qdrant-web-").tempdir_in(&root)?;
        extract_zip_checked(&archive, staging.path(), &|| task.check_cancelled())?;
        let prepared = staging.path().join("dist");
        if !prepared.join("index.html").is_file() || !prepared.join("assets").is_dir() || !prepared.join("openapi.json").is_file() {
            return Err(AppError::new("QDRANT_WEB_INCOMPLETE", "下载的 Qdrant 管理台文件不完整"));
        }
        task.check_cancelled()?;
        crate::paths::checked_data_path(&root, "static")?;
        if target.exists() { return Err(AppError::new("QDRANT_WEB_CHANGED", "管理台目录在安装期间发生变化，请重试")); }
        if published { task.begin_commit()?; }
        std::fs::rename(&prepared, &target).map_err(|e| AppError::io("安装 Qdrant 管理台", e))?;
        Ok(())
    }

    pub(crate) async fn repair_qdrant_web_ui(
        &self, installed: &InstalledPackage, paths: &Paths, store: &Store,
        downloader: &Arc<Downloader>, emit: &dyn Fn(crate::Event),
    ) -> Result<()> {
        let entry = self.installed_entry(installed);
        if !official_qdrant(&entry) { return Err(AppError::new("QDRANT_WEB_CUSTOM", "自定义 Qdrant 安装请按其配置补齐管理台")); }
        let task = downloader.begin_task(&format!("{}@{}", installed.id, installed.version))?;
        let bin = Path::new(&installed.install_path).join(entry_relative_path(&entry.entry));
        if !bin.is_file() { return Err(AppError::new("BROKEN_INSTALL", "Qdrant 主程序已丢失，请重新安装此版本")); }
        let result = self.ensure_qdrant_web_ui(&entry, bin.parent().unwrap(), paths, store, downloader, &task, emit, true).await;
        if let Err(error) = result {
            emit(crate::Event::state(task.id(), if error.code == "CANCELLED" { "cancelled" } else { "error" }));
            return Err(error);
        }
        task.begin_commit()?;
        emit(crate::Event::state(task.id(), "installed"));
        Ok(())
    }

    async fn install_task(
        &self,
        key: &str,
        paths: &Paths,
        store: &Store,
        downloader: &Arc<Downloader>,
        task: &crate::download::DownloadTask,
        emit: &dyn Fn(crate::Event),
    ) -> Result<InstalledPackage> {
        // 先查清单内置版本；未命中但版本源能枚举到时，用远程版本合成条目
        let resolved = tokio::select! {
            biased;
            _ = task.cancelled() => return Err(AppError::new("CANCELLED", "安装已取消")),
            result = self.resolve_entry(key, store) => result?,
        };
        let entry = match resolved {
            Some(e) => e,
            None => {
                return Err(AppError::new(
                    "PACKAGE_NOT_FOUND",
                    format!("清单与版本源里都没有套件 {key}"),
                )
                .with_hint("在套件页点「刷新版本」获取最新版本列表后再试"))
            }
        };

        // 平台过滤：mac 清单里有 arm64-only 的包，x64 Mac 装上启动必崩；
        // 在下载前就拦下，给出明确原因
        if !Self::is_platform_compatible(&entry) {
            return Err(AppError::new(
                "PLATFORM_UNSUPPORTED",
                format!(
                    "{} {} 不支持当前平台（{}-{}）",
                    entry.display_name,
                    entry.version,
                    current_os(),
                    current_arch()
                ),
            )
            .with_hint(format!(
                "该条目声明的平台：os={:?} arch={:?}。请安装与本机架构匹配的版本",
                entry.os, entry.arch
            )));
        }

        let version = entry.version.clone();
        ensure_safe_key(&entry.id, &version)?;
        let task_id = format!("{}@{}", entry.id, version);

        // 已装则幂等返回
        if let Some(p) = store.find_installed(&entry.id, Some(&version)) {
            let metadata = self.installed_entry(&p);
            if !Path::new(&p.install_path)
                .join(entry_relative_path(&metadata.entry))
                .is_file()
            {
                return Err(AppError::new(
                    "BROKEN_INSTALL",
                    format!("{} {} 的主程序已丢失", entry.display_name, version),
                )
                .with_hint("停止该服务，卸载此版本后重新安装"));
            }
            let binary = Path::new(&p.install_path).join(entry_relative_path(&metadata.entry));
            self.ensure_qdrant_web_ui(&metadata, binary.parent().unwrap(), paths, store, downloader, task, emit, true).await?;
            task.begin_commit()?;
            emit(crate::Event::state(&task_id, "installed"));
            return Ok(p);
        }

        emit(crate::Event::state(&task_id, "downloading"));
        let (entry, archive) = self
            .download_entry(entry, paths, store, downloader, task, emit)
            .await?;

        task.check_cancelled()?;
        emit(crate::Event::state(&task_id, "extracting"));
        let runtime_dir = paths.runtime_dir(&entry.id, &version);
        let parent = runtime_dir
            .parent()
            .ok_or_else(|| AppError::new("INVALID_PACKAGE_KEY", "安装目录无效"))?;
        std::fs::create_dir_all(parent)?;
        let staging = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(parent)
            .map_err(|e| AppError::io("创建安装暂存目录", e))?;
        let prepared = staging.path().join("package");
        std::fs::create_dir(&prepared)?;
        match entry.kind.as_str() {
            "archive" => extract_zip_checked(&archive, &prepared, &|| task.check_cancelled())?,
            // tar.gz / gz：用系统 tar（macOS 自带 bsdtar；Windows 10+ 亦内置）
            "targz" => extract_targz(&archive, &prepared, &entry.entry, &entry.url, task)?,
            // 7z：纯 Rust 解压，避免依赖用户是否安装 7-Zip。
            "sevenzip" => extract_sevenzip_checked(&archive, &prepared, task)?,
            // 单文件（composer.phar 等）：直接落盘
            "binary" => {
                let dest = prepared.join(entry_relative_path(&entry.entry));
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| AppError::io("创建单文件包目录", e))?;
                }
                copy_checked(
                    &mut std::fs::File::open(&archive)?,
                    &mut std::fs::File::create(&dest)?,
                    &|| task.check_cancelled(),
                )?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
                }
            }
            other => {
                return Err(AppError::new(
                    "UNSUPPORTED_KIND",
                    format!("暂不支持的包格式 {other}"),
                ));
            }
        }

        // 入口归位。解包产物的内部命名五花八门：顶层目录、版本化文件名
        // （mihomo-windows-amd64-v1.19.31.exe）、单文件 gz 解出的内名不可控。
        // entry 声明的路径不存在时先处理根目录改名，再按文件名/单文件定位。
        let entry_rel = entry_relative_path(&entry.entry);
        let entry_path = match settle_entry_file(&prepared, &entry_rel) {
            Some(p) => p,
            None => {
                // 列出解压产物顶层内容，方便排障（清单与包内容不一致时一眼能看出差在哪）
                let listing = std::fs::read_dir(&prepared)
                    .map(|rd| {
                        rd.flatten()
                            .map(|e| e.file_name().to_string_lossy().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                return Err(AppError::new(
                    "ENTRY_MISSING",
                    format!("解压后找不到主程序 {}", entry.entry),
                )
                .with_hint("清单声明的入口与压缩包不一致；下载缓存已保留，请检查清单后重试")
                .with_detail(format!(
                    "期望路径：{}；解压目录内容：{}",
                    runtime_dir.join(&entry_rel).display(),
                    if listing.is_empty() {
                        "（空）"
                    } else {
                        &listing
                    }
                )));
            }
        };
        if !entry_path.is_file() || std::fs::metadata(&entry_path)?.len() == 0 {
            return Err(AppError::new("ENTRY_MISSING", "套件主程序不是有效文件"));
        }
        self.ensure_qdrant_web_ui(&entry, entry_path.parent().unwrap(), paths, store, downloader, task, emit, false).await?;
        task.check_cancelled()?;

        emit(crate::Event::state(&task_id, "configuring"));
        let installed = InstalledPackage {
            id: entry.id.clone(),
            version: version.clone(),
            category: entry.category.clone(),
            install_path: runtime_dir.to_string_lossy().to_string(),
            config_path: paths
                .etc_dir(&entry.id, &version)
                .to_string_lossy()
                .to_string(),
            installed_at: crate::services::now_ms(),
        };
        // 保存安装时的完整描述，使清单外版本在离线、重启和清单更新后仍可识别。
        // 与运行时目录一同卸载，不需要新增数据库表或修改用户模块清单。
        let snapshot = serde_json::to_vec_pretty(&entry)
            .map_err(|e| AppError::internal("保存套件安装信息", e.to_string()))?;
        std::fs::write(prepared.join(".niceenv-package.json"), snapshot)
            .map_err(|e| AppError::io("保存套件安装信息", e))?;
        // 所有下载/解压都已完成，再原子发布；取消与提交之间不能出现竞态。
        task.begin_commit()?;
        let previous = if runtime_dir.exists() {
            // 老版本失败残留或用户放入的文件也保留，不能直接递归覆盖。
            let backup = tempfile::Builder::new()
                .prefix(&format!("runtime-{}-{}-", entry.id, version))
                .tempdir_in(paths.backup())
                .map_err(|e| AppError::io("备份旧运行时", e))?;
            let location = backup.path().join("previous");
            std::fs::rename(&runtime_dir, &location)
                .map_err(|e| AppError::io("备份旧运行时", e))?;
            let _ = backup.keep();
            Some(location)
        } else {
            None
        };
        let config = default_config_path(&entry, paths);
        let had_config = config.as_ref().is_some_and(|path| path.exists());
        let mut published = false;
        let commit = (|| -> Result<()> {
            std::fs::rename(&prepared, &runtime_dir)
                .map_err(|e| AppError::io("发布套件运行时", e))?;
            published = true;
            self.ensure_default_configs(&entry, paths, store)?;
            store.complete_install(&installed)?;
            Ok(())
        })();
        if let Err(err) = commit {
            let rollback = (|| -> Result<()> {
                if published {
                    std::fs::rename(&runtime_dir, &prepared)
                        .map_err(|e| AppError::io("回收失败安装", e))?;
                }
                if let Some(previous) = &previous {
                    std::fs::rename(previous, &runtime_dir)
                        .map_err(|e| AppError::io("恢复旧运行时", e))?;
                    if let Some(parent) = previous.parent() {
                        let _ = std::fs::remove_dir(parent);
                    }
                }
                if !had_config {
                    if let Some(path) = &config {
                        if path.is_file() {
                            std::fs::remove_file(path)?;
                        }
                    }
                }
                Ok(())
            })();
            if let Err(rollback_err) = rollback {
                return Err(AppError::new(
                    "INSTALL_ROLLBACK_FAILED",
                    "安装失败，原有文件已保留但未能自动恢复",
                )
                .with_hint("关闭占用安装目录的程序后重试；不要删除备份目录")
                .with_detail(format!("{err}; {rollback_err}; backup={previous:?}")));
            }
            return Err(err);
        }
        emit(crate::Event::state(&task_id, "installed"));
        Ok(installed)
    }

    pub fn ensure_default_configs(
        &self,
        entry: &crate::model::PackageManifestEntry,
        paths: &Paths,
        store: &Store,
    ) -> Result<()> {
        // 安装新版本不能覆盖已保存的 PHP 设置或正在使用的 Apache 共用配置。
        if default_config_path(entry, paths).is_some_and(|path| path.is_file()) {
            return Ok(());
        }
        match entry.id.as_str() {
            "php" => crate::configgen::write_php_ini(paths, &entry.version)?,
            "redis" => crate::configgen::write_redis_conf(
                paths,
                &entry.version,
                crate::services::PortsProfile::from_settings(store).redis,
            )?,
            "mysql" => {
                let basedir = paths
                    .runtime_dir("mysql", &entry.version)
                    .join(crate::ops::mysql_root_name(&entry.version));
                crate::configgen::write_mysql_ini(
                    paths,
                    &entry.version,
                    &basedir,
                    crate::services::PortsProfile::from_settings(store).mysql,
                )?;
            }
            "mihomo" => {
                if !paths.mihomo_config().exists() {
                    crate::configgen::write_mihomo_config(
                        paths,
                        &crate::configgen::render_mihomo_builtin_config(),
                    )?;
                }
            }
            "apache" => {
                let root = std::path::PathBuf::from(&paths.runtime_dir("apache", &entry.version))
                    .join("Apache24");
                let pools: Vec<(String, u16)> = store
                    .all_port_assigns()
                    .into_iter()
                    .filter(|(sid, _)| sid.starts_with("php@"))
                    .map(|(sid, base)| (sid.trim_start_matches("php@").to_string(), base))
                    .collect();
                let ports = crate::services::PortsProfile::from_settings(store);
                crate::configgen::write_httpd_conf(
                    paths,
                    &root,
                    &pools,
                    ports.apache_http,
                    ports.apache_https,
                    crate::webnetwork::mode(store, "apache")?,
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    /// 预览与执行共用依赖和服务判定；只检查，不停止服务或删除文件。
    pub fn uninstall_preview(
        &self,
        key: &str,
        paths: &Paths,
        store: &Store,
        manager: &Arc<crate::services::ServiceManager>,
    ) -> Result<crate::model::PackageUninstallPreview> {
        let _operation = manager.lifecycle.lock();
        let installed = match key.split_once('@') {
            Some((id, version)) => {
                ensure_safe_key(id, version)?;
                store.find_installed(id, Some(version))
            }
            None => crate::ops::installed_by_choice(store, key),
        }
        .ok_or_else(|| AppError::not_installed(key))?;
        let id = &installed.id;
        let version = &installed.version;
        // 执行阶段会删除 runtimes/{id}/{version}，先挡住 `x@../..` 这类 key。
        ensure_safe_key(id, version)?;
        let mut blockers = self.uninstall_references(store, &installed)?;
        if let Some(console) = crate::toolbox::adminer_status(manager)? {
            if (id == "php" && same_version(version, &console.php_version))
                || (id == &console.package_id && same_version(version, &console.adminer_version)) {
                blockers.push(crate::model::PackageUninstallBlocker {
                    kind: "console".into(), name: console.package_id,
                });
            }
        }
        let entry = self.installed_entry(&installed);
        let service_id = if id == "php" || id == "mysql" {
            Some(format!("{id}@{version}"))
        } else if crate::generic::is_builtin(id) || entry.run.is_some() {
            Some(crate::generic::service_id_of(&entry))
        } else {
            None
        };
        let service = service_id.and_then(|sid| manager.snapshot(&sid))
            .filter(|service| service.version.as_deref().is_some_and(|current| same_version(current, version)));
        let runtime_path = paths.runtime_dir(id, version).to_string_lossy().into_owned();
        Ok(crate::model::PackageUninstallPreview { installed, runtime_path, service, blockers })
    }

    pub fn uninstall(
        &self, key: &str, paths: &Paths, store: &Store,
        manager: &Arc<crate::services::ServiceManager>,
    ) -> Result<Option<String>> {
        let _operation = manager.lifecycle.lock();
        // 不能信任之前显示的预览：在生命周期锁内重新检查真实安装与引用。
        let preview = self.uninstall_preview(key, paths, store, manager)?;
        if !preview.blockers.is_empty() {
            let users = preview.blockers.iter().map(|item| match item.kind.as_str() {
                "site" => format!("站点「{}」", item.name),
                "stack" => format!("服务栈「{}」", item.name),
                "console" => format!("数据库管理台 {}", item.name),
                _ => format!("套件 {}", item.name),
            }).collect::<Vec<_>>();
            return Err(AppError::new("PACKAGE_IN_USE", format!(
                "无法卸载 {} {}：仍被 {} 使用", preview.installed.id, preview.installed.version, users.join("、"),
            )).with_hint("先修改相关站点、项目版本或服务栈的版本绑定，卸载依赖套件，或在数据库页面停止管理台后重试"));
        }
        let installed = preview.installed;
        let id = &installed.id;
        let version = &installed.version;
        // 单实例的另一个版本可能正在运行，只有版本吻合才能停止和移除注册。
        let stopped_service = preview.service.map(|service| service.id);
        if let Some(sid) = &stopped_service {
            crate::ops::stop_service(store, paths, manager, sid)?;
        }
        if id == "qdrant" {
            crate::generic::preserve_qdrant_snapshots(store, paths, manager, Some(&installed))?;
        }

        let runtime_dir = paths.runtime_dir(id, version);
        if runtime_dir.exists() {
            // Windows 在进程退出后仍可能短暂保留映像/静态文件句柄；有限重试，不改权限。
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                crate::paths::checked_data_path(
                    &paths.base,
                    &crate::paths::portable_path_text(
                        runtime_dir.strip_prefix(&paths.base).map_err(|_| {
                            AppError::new("INVALID_PACKAGE_PATH", "运行时目录超出托管目录")
                        })?,
                    ),
                )?;
                match std::fs::remove_dir_all(&runtime_dir) {
                    Ok(()) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(error) if cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33 | 145))
                        && std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(50)),
                    Err(error) => return Err(AppError::io("删除运行时目录", error)),
                }
            }
        }
        store.remove_installed(id, version)?;
        if let Some(sid) = &stopped_service {
            manager.services.lock().remove(sid);
            manager.watchdog.forget(sid);
        }
        if store.get_setting(&format!("active{id}Version")).as_deref().is_some_and(|current| same_version(current, version)) {
            let fallback = crate::ops::installed_by_choice(store, id)
                .map(|p| p.version)
                .unwrap_or_default();
            store.set_setting(&format!("active{id}Version"), &fallback)?;
        }
        crate::ops::register_services(paths, store, manager);
        crate::generic::register_services(paths, store, manager);
        crate::ops::save_pidfile(paths, manager);
        Ok(stopped_service)
    }

    /// 固定版本引用始终保护；无版本引用仅在卸载最后一个可用版本时阻止。
    fn uninstall_references(&self, store: &Store, target: &InstalledPackage) -> Result<Vec<crate::model::PackageUninstallBlocker>> {
        let installed = store.list_installed()?;
        let has_alternative = installed
            .iter()
            .any(|p| p.id == target.id && !same_version(&p.version, &target.version));
        let references_target = |key: &str| match key.split_once('@') {
            Some((id, version)) => id == target.id && same_version(version, &target.version),
            None => key == target.id && !has_alternative,
        };
        let mut users = Vec::new();
        for site in store.list_sites()? {
            let project_ref = target.category == "runtime"
                && crate::pathenv::project_references_version(store, &site, &target.id, &target.version)?;
            let php_ref = target.id == "php"
                && site.runtime.kind == crate::model::SiteKind::Php
                && site.runtime.php_version.as_deref().is_some_and(|version| same_version(version, &target.version));
            let application_ref = crate::applications::runtime_id(&site.runtime.kind) == Some(target.id.as_str())
                && site.runtime.application.as_ref().is_some_and(|app| same_version(&app.version, &target.version));
            let web_ref = site.runtime.web_server == target.id && !has_alternative;
            let db_ref = target.id == "mysql"
                && site.db.as_ref().is_some_and(|db| {
                    db.enabled
                        && db.version.as_deref().map_or(!has_alternative, |version| {
                        same_version(version, &target.version)
                        })
                });
            if php_ref || application_ref || web_ref || db_ref || project_ref {
                users.push(crate::model::PackageUninstallBlocker { kind: "site".into(), name: site.name });
            }
        }
        for stack in store.list_stacks()? {
            // 内置栈是安装建议，不是用户配置的硬依赖。
            if !stack.builtin
                && stack
                    .items
                    .iter()
                    .any(|item| references_target(&item.service_id))
            {
                users.push(crate::model::PackageUninstallBlocker { kind: "stack".into(), name: stack.name });
            }
        }
        for package in installed
            .iter()
            .filter(|p| p.id != target.id || !same_version(&p.version, &target.version))
        {
            let entry = self.installed_entry(package);
            let required = entry
                .requires
                .iter()
                .chain(entry.depends.iter())
                .chain(entry.run.iter().flat_map(|run| run.requires.iter()));
            if required.into_iter().any(|dep| references_target(dep)) {
                users.push(crate::model::PackageUninstallBlocker {
                    kind: "package".into(), name: format!("{} {}", entry.display_name, package.version),
                });
            }
        }
        Ok(users)
    }
}

fn modified_at_ms(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .filter(|timestamp| *timestamp > 0)
        .unwrap_or_else(crate::services::now_ms)
}

fn runtime_entry_exists(root: &Path, entry: &str) -> bool {
    let relative = entry_relative_path(entry);
    if relative.as_os_str().is_empty() {
        return false;
    }
    let expected = root.join(&relative);
    if std::fs::symlink_metadata(&expected).is_ok_and(|metadata| metadata.file_type().is_file()) {
        return true;
    }
    let Some(wanted) = relative.file_name().map(|name| name.to_string_lossy().to_ascii_lowercase()) else {
        return false;
    };
    fn walk(dir: &Path, wanted: &str, depth: usize, files: &mut usize) -> bool {
        if depth > 8 || *files > 4096 {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return false; };
        for item in entries.flatten() {
            let path = item.path();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else { continue; };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.file_type().is_file() {
                *files += 1;
                if metadata.len() > 0 && item.file_name().to_string_lossy().to_ascii_lowercase() == wanted {
                    return true;
                }
            } else if metadata.file_type().is_dir() && walk(&path, wanted, depth + 1, files) {
                return true;
            }
        }
        false
    }
    let mut files = 0;
    walk(root, &wanted, 0, &mut files)
}

/// 默认配置只在首次安装时创建；保留已有配置用于重装和多版本共存。
fn default_config_path(
    entry: &crate::model::PackageManifestEntry,
    paths: &Paths,
) -> Option<std::path::PathBuf> {
    match entry.id.as_str() {
        "php" => Some(paths.php_ini(&entry.version)),
        "redis" => Some(paths.redis_conf(&entry.version)),
        "mysql" => Some(paths.mysql_ini(&entry.version)),
        "mihomo" => Some(paths.mihomo_config()),
        "apache" => Some(paths.apache_conf()),
        _ => None,
    }
}

/// 把清单里的相对入口路径拆成平台无关的多段 join。
/// 清单统一用 '/'；Windows 的 Path::join 能正确吃掉 '/'，macOS 也如此。
pub fn entry_relative_path(entry: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::new();
    // `..` 与带 `:` 的段（盘符）直接丢弃：entry 也可能来自远程版本源，不能借它写到解压目录外
    for seg in entry
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != ".." && !s.contains(':'))
    {
        p.push(seg);
    }
    p
}

/// 解包后把主程序归位到 entry 声明的路径，三级容错：
///  1. 声明路径已存在 → 直接用；
///  2. 解压树里按文件名精确匹配；只有包根目录命名不同时整目录归位，保留依赖和工具；
///  3. 整棵解压树只有一个文件时认定它是主程序（gz 内名/版本化单文件包）→ 搬移。
/// 都不命中返回 None（调用方报 ENTRY_MISSING）。
fn settle_entry_file(runtime_dir: &Path, entry_rel: &Path) -> Option<std::path::PathBuf> {
    let expected = runtime_dir.join(entry_rel);
    if expected.exists() {
        return Some(expected);
    }
    let want = entry_rel.file_name()?.to_string_lossy().to_lowercase();

    fn walk(
        dir: &Path,
        depth: usize,
        want: &str,
        hits: &mut Vec<(usize, std::path::PathBuf)>,
        files: &mut usize,
    ) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, depth + 1, want, hits, files);
            } else {
                *files += 1;
                let name = p
                    .file_name()
                    .map(|x| x.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if name == want {
                    hits.push((depth, p));
                }
            }
        }
    }

    let mut hits = Vec::new();
    let mut files = 0usize;
    walk(runtime_dir, 0, &want, &mut hits, &mut files);

    if hits.len() == 1 {
        let source = hits[0].1.strip_prefix(runtime_dir).ok()?;
        let source_parts: Vec<_> = source.components().collect();
        let target_parts: Vec<_> = entry_rel.components().collect();
        if source_parts.len() > 1
            && source_parts.len() == target_parts.len()
            && source_parts[1..] == target_parts[1..]
            && source_parts[0] != target_parts[0]
        {
            // 仅在本次解压暂存目录中移动一个根目录；绝不把主程序从旁边的 DLL/
            // mongos 等资源中拆走，也不合并/覆盖压缩包里另一个已经存在的目录。
            let source_root = runtime_dir.join(source_parts[0].as_os_str());
            let target_root = runtime_dir.join(target_parts[0].as_os_str());
            if target_root.exists() {
                return None;
            }
            std::fs::rename(source_root, target_root).ok()?;
            return Some(expected);
        }
    }

    let src = if let Some((_, p)) = hits.first() {
        p.clone()
    } else if files == 1 {
        let mut only = Vec::new();
        fn collect_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    collect_files(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
        collect_files(runtime_dir, &mut only);
        only.into_iter().next()?
    } else {
        return None;
    };

    if let Some(parent) = expected.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    // 同卷 rename 一般可用；万一失败（文件被占用等）退回复制
    if std::fs::rename(&src, &expected).is_err() {
        std::fs::copy(&src, &expected).ok()?;
    }
    Some(expected)
}

/// 压缩包条目名 → 安全的相对路径；会逃出解压目录的条目返回 None。
///
/// 拒绝：绝对路径（`/etc/x`、`\\server\x`）、`..` 段、带 `:` 的段（`C:` 盘符 /
/// NTFS 备用数据流）。`\\` 统一当分隔符（Windows 打的 zip 常见），`.` 与空段忽略。
/// 只看整段是否为 `..`，所以 `nginx..conf` 这类合法文件名不会被误杀。
fn safe_archive_path(name: &str) -> Option<std::path::PathBuf> {
    let name = name.replace('\\', "/");
    if name.starts_with('/') {
        return None;
    }
    let mut p = std::path::PathBuf::new();
    for seg in name.split('/') {
        match seg {
            "" | "." => continue,
            ".." => return None,
            s if s.contains(':') => return None,
            s => p.push(s),
        }
    }
    if p.as_os_str().is_empty() {
        None
    } else {
        Some(p)
    }
}

#[cfg(test)]
fn write_gzip_tar(path: &Path, entries: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for (name, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_path(name).unwrap();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, *body).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
}

/// 套件 id / 版本号会直接拼进目录路径（runtimes/{id}/{version}），
/// 必须是单个普通路径段，不能借 `..` 或分隔符指到别处。
fn is_safe_path_component(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains(['/', '\\', ':'])
}

fn ensure_safe_key(id: &str, version: &str) -> Result<()> {
    if is_safe_path_component(id) && is_safe_path_component(version) {
        Ok(())
    } else {
        Err(AppError::new(
            "INVALID_PACKAGE_KEY",
            format!("非法的套件标识 {id}@{version}"),
        ))
    }
}

pub(crate) fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    extract_zip_checked(archive, dest, &|| Ok(()))
}

fn extract_zip_checked(archive: &Path, dest: &Path, check: &dyn Fn() -> Result<()>) -> Result<()> {
    let file = std::fs::File::open(archive).map_err(|e| AppError::io("打开压缩包", e))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| AppError::internal("读取压缩包", e.to_string()))?;
    for i in 0..zip.len() {
        check()?;
        let mut entry = zip
            .by_index(i)
            .map_err(|e| AppError::internal("读取压缩条目", e.to_string()))?;
        // 防路径穿越（Zip Slip）：绝对路径 / 盘符 / `..` 的条目一律跳过
        let Some(rel) = safe_archive_path(entry.name()) else {
            continue;
        };
        let out_path = dest.join(rel);
        if entry.is_dir() {
            // 保留压缩包里的空目录（nginx 的 logs/temp 等）
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path).map_err(|e| AppError::io("创建文件", e))?;
        copy_checked(&mut entry, &mut out, check)?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&out_path, std::fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

/// tar.gz / 单文件 gzip 解压：使用 Rust 实现，避免依赖 Windows/macOS 的外部命令。
/// tar 条目逐个校验路径，只接受普通文件和目录，拒绝链接条目。
fn extract_targz(
    archive: &Path,
    dest: &Path,
    entry: &str,
    source_url: &str,
    task: &crate::download::DownloadTask,
) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let single_gzip = reqwest::Url::parse(source_url).ok().is_some_and(|url| {
        let path = url.path().to_ascii_lowercase();
        path.ends_with(".gz") && !path.ends_with(".tar.gz")
    });
    let file = std::fs::File::open(archive).map_err(|e| AppError::io("打开压缩包", e))?;
    let decoder = flate2::read::GzDecoder::new(file);
    if single_gzip {
        // 单文件 .gz → 解压到清单指定的相对路径。
        let relative = safe_archive_path(entry).ok_or_else(|| {
            AppError::new("EXTRACT_FAILED", "gzip 入口路径无效").with_detail(entry.to_string())
        })?;
        let target = dest.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AppError::io("创建解压目录", e))?;
        }
        let mut output =
            std::fs::File::create(&target).map_err(|e| AppError::io("创建解压文件", e))?;
        let mut decoder = decoder;
        copy_checked(&mut decoder, &mut output, &|| task.check_cancelled())?;
        #[cfg(unix)]
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
        return Ok(());
    }

    let mut archive = tar::Archive::new(decoder);
    let entries = archive.entries().map_err(|e| {
        AppError::new("EXTRACT_FAILED", "读取 tar.gz 条目失败").with_detail(e.to_string())
    })?;
    for item in entries {
        task.check_cancelled()?;
        let mut item = item.map_err(|e| {
            AppError::new("EXTRACT_FAILED", "读取 tar.gz 条目失败").with_detail(e.to_string())
        })?;
        let path = item.path().map_err(|e| {
            AppError::new("EXTRACT_FAILED", "读取 tar.gz 路径失败").with_detail(e.to_string())
        })?;
        let name = path
            .to_str()
            .ok_or_else(|| AppError::new("EXTRACT_FAILED", "tar.gz 含有无法识别的文件名"))?;
        let relative = safe_archive_path(name).ok_or_else(|| {
            AppError::new("EXTRACT_FAILED", "tar.gz 含有不安全的路径").with_detail(name.to_string())
        })?;
        let target = dest.join(relative);
        let entry_type = item.header().entry_type();
        if entry_type.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| AppError::io("创建解压目录", e))?;
            continue;
        }
        if !entry_type.is_file() {
            return Err(
                AppError::new("EXTRACT_FAILED", "tar.gz 含有不支持的链接或特殊文件")
                    .with_detail(name.to_string()),
            );
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AppError::io("创建解压目录", e))?;
        }
        let mut output =
            std::fs::File::create(&target).map_err(|e| AppError::io("创建解压文件", e))?;
        copy_checked(&mut item, &mut output, &|| task.check_cancelled())?;
        #[cfg(unix)]
        if let Ok(mode) = item.header().mode() {
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

/// 7z 解压：使用纯 Rust 实现，并复用归档路径校验，避免依赖系统 7-Zip 和路径穿越。
fn extract_sevenzip_checked(
    archive: &Path,
    dest: &Path,
    task: &crate::download::DownloadTask,
) -> Result<()> {
    task.check_cancelled()?;
    let mut cancelled = None;
    let result = sevenz_rust::decompress_file_with_extract_fn(
        archive,
        dest,
        |entry, reader, _| {
            if let Err(err) = task.check_cancelled() {
                cancelled = Some(err);
                return Err(sevenz_rust::Error::other("安装已取消"));
            }
            let Some(relative) = safe_archive_path(entry.name()) else {
                // 与 zip 提取保持一致：拒绝绝对路径、盘符、.. 和 NTFS ADS。
                return Ok(true);
            };
            let target = dest.join(relative);
            if entry.is_directory() {
                std::fs::create_dir_all(&target).map_err(sevenz_rust::Error::io)?;
                return Ok(true);
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(sevenz_rust::Error::io)?;
            }
            let mut output = std::fs::File::create(&target).map_err(sevenz_rust::Error::io)?;
            copy_checked(reader, &mut output, &|| task.check_cancelled())
                .map_err(|err| sevenz_rust::Error::other(err.to_string()))?;
            Ok(true)
        },
    );
    if let Some(err) = cancelled {
        return Err(err);
    }
    result.map_err(|err| AppError::new("EXTRACT_FAILED", "7z 解压失败").with_detail(err.to_string()))
}

fn copy_checked<R: std::io::Read + ?Sized>(
    input: &mut R,
    output: &mut impl std::io::Write,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check()?;
        let n = input
            .read(&mut buffer)
            .map_err(|e| AppError::io("读取套件内容", e))?;
        if n == 0 {
            return Ok(());
        }
        output
            .write_all(&buffer[..n])
            .map_err(|e| AppError::io("写入套件内容", e))?;
    }
}

/* ================= 平台兼容性 ================= */

/// 当前运行平台，取值与清单 os 字段一致（windows / macos / linux）
pub fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// 当前架构，取值与清单 arch 字段一致（x64 / arm64）
pub fn current_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    }
}

impl Installer {
    /// 清单条目是否支持当前平台。
    /// os/arch 为空数组视为「不限平台」（宽容处理老清单）；
    /// 非空则必须包含当前平台——下载前拦截，避免 arm64 包装上 x64 机器启动即崩。
    pub fn is_platform_compatible(entry: &crate::model::PackageManifestEntry) -> bool {
        let (os_ok, arch_ok) = (
            entry.os.is_empty() || entry.os.iter().any(|o| o == current_os()),
            entry.arch.is_empty() || entry.arch.iter().any(|a| a == current_arch()),
        );
        os_ok && arch_ok
    }
}

#[cfg(test)]
mod settle_entry_tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("nsb-settle-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &std::path::Path, body: &[u8]) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn keeps_expected_path_when_present() {
        let d = temp_dir("exact");
        write(&d.join("bin/mihomo.exe"), b"x");
        let got = settle_entry_file(&d, std::path::Path::new("bin/mihomo.exe")).unwrap();
        assert_eq!(got, d.join("bin/mihomo.exe"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn relocates_versioned_name_in_nested_dir() {
        let d = temp_dir("versioned");
        // 包内是「顶层目录 + 版本化文件名」，entry 声明的是根下不带版本号的名字
        write(
            &d.join("mihomo-v1.19.31/mihomo-windows-amd64-v1.19.31.exe"),
            b"x",
        );
        let got = settle_entry_file(&d, std::path::Path::new("mihomo-windows-amd64.exe")).unwrap();
        assert_eq!(got, d.join("mihomo-windows-amd64.exe"));
        assert!(got.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn single_file_in_tree_is_the_entry() {
        let d = temp_dir("single");
        // gz 单文件包解出的内名不可控：整棵树只有一个文件
        write(&d.join("mihomo-windows-amd64-v1.19.31"), b"x");
        let got = settle_entry_file(&d, std::path::Path::new("mihomo-windows-amd64.exe")).unwrap();
        assert_eq!(got, d.join("mihomo-windows-amd64.exe"));
        assert!(got.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn returns_none_when_nothing_matches() {
        let d = temp_dir("nomatch");
        write(&d.join("a.txt"), b"x");
        write(&d.join("b.txt"), b"y");
        assert!(settle_entry_file(&d, std::path::Path::new("mihomo.exe")).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn renamed_package_root_keeps_companion_programs_and_libraries_together() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let actual = "mongodb-macos-aarch64--9.0.2";
        let expected = "mongodb-macos-arm64-9.0.2";
        write(&root.join(actual).join("bin/mongod"), b"server");
        write(&root.join(actual).join("bin/mongos"), b"router");
        write(&root.join(actual).join("lib/runtime.dylib"), b"library");
        let entry = Path::new(expected).join("bin/mongod");
        assert_eq!(settle_entry_file(root, &entry).unwrap(), root.join(&entry));
        assert_eq!(
            std::fs::read(root.join(expected).join("bin/mongos")).unwrap(),
            b"router"
        );
        assert_eq!(
            std::fs::read(root.join(expected).join("lib/runtime.dylib")).unwrap(),
            b"library"
        );
        assert!(!root.join(actual).exists());
        assert_eq!(settle_entry_file(root, &entry).unwrap(), root.join(entry));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PackageManifestEntry;

    fn fixture() -> (tempfile::TempDir, crate::CoreState) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        // 不运行 CoreState::init 的系统修复/计划任务；所有记录与文件限定在临时目录。
        let state = crate::CoreState {
            paths,
            store,
            manager: Arc::new(crate::services::ServiceManager::new()),
            installer: Installer::bundled(),
            downloader: Arc::new(crate::download::Downloader::new()),
            emit: Arc::new(|_| {}),
        };
        (temp, state)
    }

    fn install_fixture(state: &crate::CoreState, id: &str, version: &str) -> InstalledPackage {
        let path = state.paths.runtime_dir(id, version);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("keep.txt"), "owned fixture").unwrap();
        let installed = InstalledPackage {
            id: id.into(),
            version: version.into(),
            category: "runtime".into(),
            install_path: path.to_string_lossy().into(),
            config_path: String::new(),
            installed_at: 0,
        };
        state.store.upsert_installed(&installed).unwrap();
        // 测试使用的服务不一定收录在当前主机清单；保存快照模拟真实安装元数据。
        if state.installer.installed_entry(&installed).entry.is_empty() {
            let source = Installer {
                manifest: serde_json::from_str(include_str!("../../../manifest/packages.win.json"))
                    .unwrap(),
            };
            let entry = source.installed_entry(&installed);
            std::fs::write(
                path.join(".niceenv-package.json"),
                serde_json::to_vec(&entry).unwrap(),
            )
            .unwrap();
        }
        installed
    }

    fn cached_package(state: &mut crate::CoreState, id: &str, kind: &str, bytes: &[u8]) -> String {
        let mut entry = entry_with(vec![], vec![]);
        entry.id = id.into();
        entry.kind = kind.into();
        entry.entry = "program.bin".into();
        let key = format!("{id}@{}", entry.version);
        let cache = state.paths.downloads().join(format!("{key}.pkg"));
        std::fs::write(&cache, bytes).unwrap();
        entry.sha256 = Some(crate::download::sha256_file(&cache).unwrap());
        entry.size_bytes = bytes.len() as u64;
        state.installer.manifest.packages.push(entry);
        key
    }

    #[test]
    fn legacy_rnacos_snapshots_upgrade_without_replacing_custom_runs_or_install_metadata() {
        let (_temp, mut state) = fixture();
        state.installer.manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap();
        let mut legacy = state.installer.find("rnacos").unwrap();
        let run = legacy.run.as_mut().unwrap();
        run.args.clear();
        run.env.as_mut().unwrap().remove("RNACOS_GRPC_PORT");
        run.env.as_mut().unwrap().remove("RNACOS_HTTP_CONSOLE_PORT");
        run.config_template = Some("RNACOS_HTTP_PORT={port}\nRNACOS_GRPC_PORT={port+1000}\nRNACOS_HTTP_CONSOLE_PORT={port+2000}\nRNACOS_DATA_DIR={data}/nacos_db\nRNACOS_CONFIG_DB_FILE={data}/nacos_db/config.db\nRNACOS_NAMING_DB_FILE={data}/nacos_db/naming.db\n".into());
        legacy.entry = "original/rnacos.exe".into();
        // 该回归夹具复用 Windows 清单，但 package_views 会按当前平台筛选。
        // 标记为当前平台，测试仍只关注快照升级行为。
        legacy.os = vec![current_os().into()];
        legacy.arch = vec![current_arch().into()];
        let installed = install_fixture(&state, "rnacos", &legacy.version);
        let snapshot = Path::new(&installed.install_path).join(".niceenv-package.json");
        let raw = serde_json::to_vec(&legacy).unwrap();
        std::fs::write(&snapshot, &raw).unwrap();
        let upgraded = state.installer.installed_entry(&installed);
        assert_eq!(upgraded.entry, legacy.entry);
        assert_eq!(upgraded.url, legacy.url);
        assert_eq!(upgraded.sha256, legacy.sha256);
        assert_eq!(upgraded.run.unwrap().args, ["-e", "{etc}/.env"]);
        assert_eq!(std::fs::read(&snapshot).unwrap(), raw);
        state.installer.manifest.packages = vec![legacy.clone()];
        assert_eq!(state.installer.find("rnacos").unwrap().run.unwrap().args.len(), 2);
        assert_eq!(state.installer.template_for("rnacos").unwrap().run.unwrap().args.len(), 2);
        assert_eq!(state.installer.package_views(&[])[0].manifest.run.as_ref().unwrap().args.len(), 2);
        for variation in 0..4 {
            let mut custom = legacy.clone();
            let run = custom.run.as_mut().unwrap();
            match variation {
                0 => run.args = vec!["-e".into(), "my.env".into()],
                1 => run.cwd = Some("{data}".into()),
                2 => { run.env.as_mut().unwrap().insert("RUST_LOG".into(), "warn".into()); },
                _ => run.config_template.as_mut().unwrap().push_str("RUST_LOG=warn\n"),
            }
            std::fs::write(&snapshot, serde_json::to_vec(&custom).unwrap()).unwrap();
            assert_eq!(serde_json::to_value(state.installer.installed_entry(&installed)).unwrap(), serde_json::to_value(custom).unwrap());
        }
    }

    #[test]
    fn legacy_consul_dev_runs_become_persistent_without_replacing_custom_settings() {
        let (_temp, mut state) = fixture();
        state.installer.manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap();
        let expected = state.installer.find("consul").unwrap().run.unwrap();
        let mut legacy = state.installer.find("consul").unwrap();
        // 该回归夹具复用 Windows 清单，但 package_views 会按当前平台筛选。
        legacy.os = vec![current_os().into()];
        legacy.arch = vec![current_arch().into()];
        legacy.run = Some(serde_json::from_value(serde_json::json!({
            "args":["agent","-dev","-client","127.0.0.1","-http-port","{port}"], "health":"tcp", "healthTimeoutSec":20
        })).unwrap());
        let installed = install_fixture(&state, "consul", &legacy.version);
        let snapshot = Path::new(&installed.install_path).join(".niceenv-package.json");
        let raw = serde_json::to_vec(&legacy).unwrap(); std::fs::write(&snapshot, &raw).unwrap();
        let upgraded = state.installer.installed_entry(&installed);
        assert_eq!(upgraded.entry, legacy.entry); assert_eq!(upgraded.url, legacy.url); assert_eq!(upgraded.sha256, legacy.sha256);
        assert_eq!(serde_json::to_value(upgraded.run.unwrap()).unwrap(), serde_json::to_value(&expected).unwrap());
        assert_eq!(std::fs::read(&snapshot).unwrap(), raw);
        state.installer.manifest.packages = vec![legacy.clone()];
        for run in [state.installer.find("consul").unwrap().run.unwrap(), state.installer.template_for("consul").unwrap().run.unwrap(),
            state.installer.package_views(&[])[0].manifest.run.clone().unwrap()] {
            assert_eq!(serde_json::to_value(run).unwrap(), serde_json::to_value(&expected).unwrap());
        }
        for variation in 0..5 {
            let mut custom = legacy.clone(); let run = custom.run.as_mut().unwrap();
            match variation {
                0 => run.args.extend(["-data-dir".into(), "custom".into()]),
                1 => run.cwd = Some("{data}".into()),
                2 => run.env = Some(std::collections::HashMap::from([("CONSUL_DATACENTER".into(), "custom".into())])),
                3 => run.config_file = Some("custom.hcl".into()),
                _ => run.health_timeout_sec = 60,
            }
            std::fs::write(&snapshot, serde_json::to_vec(&custom).unwrap()).unwrap();
            assert_eq!(serde_json::to_value(state.installer.installed_entry(&installed)).unwrap(), serde_json::to_value(custom).unwrap());
        }
    }

    #[test]
    fn legacy_rabbitmq_runs_gain_managed_console_without_replacing_custom_settings() {
        let (_temp, mut state) = fixture();
        state.installer.manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap();
        let expected = state.installer.find("rabbitmq").unwrap().run.unwrap();
        let mut legacy = state.installer.find("rabbitmq").unwrap();
        legacy.run = Some(serde_json::from_value(serde_json::json!({
            "args": [], "health": "tcp", "healthTimeoutSec": 40,
            "requires": ["erlang"],
            "env": { "RABBITMQ_BASE": "{data}", "RABBITMQ_NODE_PORT": "{port}" }
        })).unwrap());
        let installed = install_fixture(&state, "rabbitmq", &legacy.version);
        let snapshot = Path::new(&installed.install_path).join(".niceenv-package.json");
        let raw = serde_json::to_vec(&legacy).unwrap(); std::fs::write(&snapshot, &raw).unwrap();
        let upgraded = state.installer.installed_entry(&installed);
        assert_eq!(serde_json::to_value(upgraded.run.unwrap()).unwrap(), serde_json::to_value(&expected).unwrap());
        assert_eq!(std::fs::read(&snapshot).unwrap(), raw);
        for variation in 0..4 {
            let mut custom = legacy.clone(); let run = custom.run.as_mut().unwrap();
            match variation {
                0 => run.init_bin = Some("custom-plugins.bat".into()),
                1 => run.init_args = Some(vec!["disable".into(), "rabbitmq_management".into()]),
                2 => { run.env.as_mut().unwrap().insert("RABBITMQ_NODE_NAME".into(), "custom".into()); },
                _ => run.config_template = Some("listeners.tcp.default = 127.0.0.1:{port}\n".into()),
            }
            std::fs::write(&snapshot, serde_json::to_vec(&custom).unwrap()).unwrap();
            assert_eq!(serde_json::to_value(state.installer.installed_entry(&installed)).unwrap(), serde_json::to_value(custom).unwrap());
        }
    }

    #[tokio::test]
    async fn qdrant_console_preserves_existing_assets_and_cancellation_never_publishes_partial_files() {
        let (_temp, state) = fixture();
        let manifest: crate::model::Manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap();
        let entry = manifest.packages.into_iter().find(|p| p.id == "qdrant").unwrap();
        let root = state.paths.runtime_dir(&entry.id, &entry.version); std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("qdrant.exe"), "keep program").unwrap();
        let task = state.downloader.begin_task("qdrant-console-fixture").unwrap();
        let result = state.installer.ensure_qdrant_web_ui(&entry, &root, &state.paths, &state.store, &state.downloader, &task, &|_| {
            state.downloader.cancel(task.id());
        }, true).await;
        assert_eq!(result.unwrap_err().code, "CANCELLED");
        assert!(!root.join("static").exists());
        assert_eq!(std::fs::read_to_string(root.join("qdrant.exe")).unwrap(), "keep program");
        drop(task);
        let task = state.downloader.begin_task("qdrant-console-fixture").unwrap();
        std::fs::create_dir(root.join("static")).unwrap();
        std::fs::write(root.join("static/keep.txt"), "custom asset").unwrap();
        assert_eq!(state.installer.ensure_qdrant_web_ui(&entry, &root, &state.paths, &state.store, &state.downloader, &task, &|_| {}, true)
            .await.unwrap_err().code, "QDRANT_WEB_INCOMPLETE");
        std::fs::write(root.join("static/index.html"), "custom dashboard").unwrap();
        state.installer.ensure_qdrant_web_ui(&entry, &root, &state.paths, &state.store, &state.downloader, &task, &|_| panic!("must not download existing UI"), true).await.unwrap();
        assert_eq!(std::fs::read_to_string(root.join("static/index.html")).unwrap(), "custom dashboard");
        assert_eq!(std::fs::read_to_string(root.join("static/keep.txt")).unwrap(), "custom asset");
    }

    #[cfg(windows)]
    #[test]
    fn uninstall_waits_for_a_short_lived_windows_file_handle() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_temp, state) = fixture();
        let installed = install_fixture(&state, "fixture", "1.0.0");
        let locked = std::fs::OpenOptions::new().read(true).share_mode(0)
            .open(Path::new(&installed.install_path).join("keep.txt")).unwrap();
        let release = std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_millis(200)); drop(locked); });
        state.uninstall_package("fixture@1.0.0").unwrap();
        release.join().unwrap();
        assert!(!Path::new(&installed.install_path).exists());
        assert!(state.store.find_installed("fixture", Some("1.0.0")).is_none());
    }

    #[test]
    fn sftpgo_downloads_use_portable_but_installed_setup_executables_are_never_started() {
        let (_temp, mut state) = fixture();
        state.installer.manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap();
        let mut legacy = state.installer.find("sftpgo@2.7.5").unwrap();
        legacy.kind = "binary".into(); legacy.entry = "sftpgo_v2.7.5_windows_x86_64.exe".into();
        legacy.url = format!("https://github.com/drakkan/sftpgo/releases/download/v2.7.5/{}", legacy.entry);
        legacy.run.as_mut().unwrap().env = None;
        state.installer.manifest.packages = vec![legacy.clone()];
        let available = state.installer.find("sftpgo@2.7.5").unwrap();
        assert_eq!(available.kind, "archive"); assert_eq!(available.entry, "sftpgo.exe");
        assert!(available.url.ends_with("_windows_portable.zip"));
        assert!(available.run.as_ref().unwrap().env.as_ref().unwrap().contains_key("SFTPGO_HTTPD__BINDINGS__0__PORT"));
        let installed = install_fixture(&state, "sftpgo", "2.7.5");
        let snapshot = Path::new(&installed.install_path).join(".niceenv-package.json");
        std::fs::write(&snapshot, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(state.installer.installed_entry(&installed).entry, legacy.entry);
        let error = crate::generic::resolve(&state.store, &state.paths, "sftpgo").err().unwrap();
        assert_eq!(error.code, "SFTPGO_INSTALLER_PACKAGE");
        legacy.run.as_mut().unwrap().cwd = Some("{data}".into());
        std::fs::write(&snapshot, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(state.installer.installed_entry(&installed).run).unwrap(), serde_json::to_value(&legacy.run).unwrap());
    }

    #[test]
    fn apache_old_catalog_snapshots_update_only_the_retired_official_artifact() {
        let (_temp, mut state) = fixture();
        state.installer.manifest = serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap();
        let current = state.installer.find("apache@2.4.68").unwrap();
        let mut legacy = current.clone();
        // 该回归夹具复用 Windows 清单，但 package_views 会按当前平台筛选。
        legacy.os = vec![current_os().into()];
        legacy.arch = vec![current_arch().into()];
        legacy.url = "https://www.apachelounge.com/download/VS18/binaries/httpd-2.4.68-260827-Win64-VS18.zip".into();
        legacy.sha256 = Some("a6b7de9fdccb28456f5b1f884920fe0b2425aadfca25c63ae1c0969d43bb355b".into());
        legacy.size_bytes = 14584378;
        state.installer.manifest.packages = vec![legacy.clone()];
        for entry in [state.installer.find("apache").unwrap(), state.installer.template_for("apache").unwrap(),
            state.installer.package_views(&[])[0].manifest.clone()] {
            assert_eq!(entry.url, current.url); assert_eq!(entry.sha256, current.sha256); assert_eq!(entry.size_bytes, current.size_bytes);
        }
        let installed = install_fixture(&state, "apache", "2.4.68");
        let snapshot = Path::new(&installed.install_path).join(".niceenv-package.json");
        std::fs::write(&snapshot, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(state.installer.installed_entry(&installed).url, legacy.url);
        assert_eq!(state.installer.installed_entry(&installed).sha256, legacy.sha256);
        for variation in 0..5 {
            let mut custom = legacy.clone();
            match variation {
                0 => custom.url = "https://example.org/custom-apache.zip".into(),
                1 => custom.sha256 = Some("a".repeat(64)),
                2 => custom.entry = "custom/httpd.exe".into(),
                3 => custom.mirrors.push("https://example.org/apache.zip".into()),
                _ => { custom.version_source = Some(crate::model::VersionSource { kind: "static".into(), ..Default::default() }); },
            }
            assert_eq!(serde_json::to_value(upgrade_available_entry(custom.clone())).unwrap(), serde_json::to_value(custom).unwrap());
        }
    }

    #[tokio::test]
    async fn install_publishes_complete_runtime_and_preserves_previous_files() {
        let (_temp, mut state) = fixture();
        let key = cached_package(&mut state, "fixture", "binary", b"owned package bytes");
        let runtime = state.paths.runtime_dir("fixture", "1.0.0");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(runtime.join("previous.txt"), "preserve me").unwrap();
        let installed = state.install_package(&key).await.unwrap();
        assert_eq!(
            std::fs::read(runtime.join("program.bin")).unwrap(),
            b"owned package bytes"
        );
        assert!(runtime.join(".niceenv-package.json").is_file());
        assert_eq!(
            state
                .store
                .find_installed("fixture", Some("1.0.0"))
                .unwrap()
                .install_path,
            installed.install_path
        );
        let backups: Vec<_> = std::fs::read_dir(state.paths.backup())
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(backups[0].path().join("previous/previous.txt")).unwrap(),
            "preserve me"
        );
        // 幂等请求也须确认主程序仍存在。
        std::fs::remove_file(runtime.join("program.bin")).unwrap();
        assert_eq!(
            state.install_package(&key).await.unwrap_err().code,
            "BROKEN_INSTALL"
        );
    }

    #[tokio::test]
    async fn failed_or_cancelled_extraction_leaves_no_partial_install() {
        let (_temp, mut state) = fixture();
        let key = cached_package(&mut state, "broken", "archive", b"not a zip archive");
        let runtime = state.paths.runtime_dir("broken", "1.0.0");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(runtime.join("previous.txt"), "original").unwrap();
        assert!(state.install_package(&key).await.is_err());
        assert!(state
            .store
            .find_installed("broken", Some("1.0.0"))
            .is_none());
        assert_eq!(
            std::fs::read_to_string(runtime.join("previous.txt")).unwrap(),
            "original"
        );
        assert!(!std::fs::read_dir(runtime.parent().unwrap())
            .unwrap()
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().starts_with(".install-")));

        let key = cached_package(
            &mut state,
            "cancelled",
            "binary",
            b"cancel before publishing",
        );
        let result = state
            .installer
            .install(
                &key,
                &state.paths,
                &state.store,
                &state.downloader,
                &|event| {
                    if let crate::Event::DownloadProgress(progress) = event {
                        if progress.state == "extracting" {
                            assert!(state.downloader.cancel(&progress.task_id));
                        }
                    }
                },
            )
            .await;
        assert_eq!(result.unwrap_err().code, "CANCELLED");
        assert!(!state.paths.runtime_dir("cancelled", "1.0.0").exists());
        assert!(state
            .store
            .find_installed("cancelled", Some("1.0.0"))
            .is_none());
        assert!(
            state.install_package(&key).await.is_ok(),
            "取消后任务锁应释放，缓存可复用"
        );
    }

    #[tokio::test]
    async fn config_failure_restores_runtime_and_install_preserves_existing_configs() {
        let (_temp, mut state) = fixture();
        let key = cached_package(&mut state, "php", "binary", b"fixture php bytes");
        let runtime = state.paths.runtime_dir("php", "1.0.0");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(runtime.join("original.txt"), "original").unwrap();
        let config = state.paths.php_ini("1.0.0");
        std::fs::create_dir_all(&config).unwrap(); // 配置目标为目录，强制真实文件系统错误。
        assert!(state.install_package(&key).await.is_err());
        assert_eq!(
            std::fs::read_to_string(runtime.join("original.txt")).unwrap(),
            "original"
        );
        assert!(!runtime.join("program.bin").exists());
        assert!(state.store.find_installed("php", Some("1.0.0")).is_none());
        std::fs::remove_dir(&config).unwrap();
        std::fs::write(&config, "memory_limit=321M\n; user configuration").unwrap();
        state.install_package(&key).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(config).unwrap(),
            "memory_limit=321M\n; user configuration"
        );

        let key = cached_package(&mut state, "apache", "binary", b"fixture apache bytes");
        let config = state.paths.apache_conf();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "existing running Apache configuration").unwrap();
        state.install_package(&key).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(config).unwrap(),
            "existing running Apache configuration"
        );
    }

    #[test]
    fn uninstall_cannot_race_install_of_same_version() {
        let (_temp, state) = fixture();
        let installed = install_fixture(&state, "fixture", "1.0.0");
        let task = state.downloader.begin_task("fixture@1.0.0").unwrap();
        assert_eq!(
            state.uninstall_package("fixture@1.0.0").unwrap_err().code,
            "PACKAGE_BUSY"
        );
        assert!(Path::new(&installed.install_path).exists());
        drop(task);
        state.uninstall_package("fixture@1.0.0").unwrap();
    }

    #[tokio::test]
    async fn tar_install_publishes_files_and_rejects_broken_archive_without_gzip_fallback() {
        let (temp, mut state) = fixture();
        let source = temp.path().join("tar-source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("program.bin"), b"owned tar payload").unwrap();
        let archive = temp.path().join("fixture.tar.gz");
        write_gzip_tar(&archive, &[("program.bin", b"owned tar payload")]);
        let key = cached_package(
            &mut state,
            "tar-good",
            "targz",
            &std::fs::read(&archive).unwrap(),
        );
        state.installer.manifest.packages.last_mut().unwrap().url =
            "https://example.invalid/fixture.tar.gz".into();
        let installed = state.install_package(&key).await.unwrap();
        assert_eq!(
            std::fs::read(Path::new(&installed.install_path).join("program.bin")).unwrap(),
            b"owned tar payload"
        );

        let key = cached_package(&mut state, "tar-bad", "targz", b"damaged archive");
        state.installer.manifest.packages.last_mut().unwrap().url =
            "https://example.invalid/fixture.tar.gz".into();
        let err = state.install_package(&key).await.unwrap_err();
        assert_eq!(err.code, "EXTRACT_FAILED", "{err}");
        assert!(!state.paths.runtime_dir("tar-bad", "1.0.0").exists());
        assert!(state
            .store
            .find_installed("tar-bad", Some("1.0.0"))
            .is_none());
    }

    #[test]
    fn single_gzip_extracts_to_declared_entry_without_external_gzip() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("mihomo.gz");
        let output = temp.path().join("package");
        std::fs::create_dir_all(&output).unwrap();
        let file = std::fs::File::create(&archive).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, b"portable binary payload").unwrap();
        encoder.finish().unwrap();

        let downloader = crate::download::Downloader::new();
        let task = downloader.begin_task("mihomo-gzip").unwrap();
        extract_targz(
            &archive,
            &output,
            "bin/mihomo",
            "https://example.invalid/mihomo.gz?download=1",
            &task,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(output.join("bin/mihomo")).unwrap(),
            b"portable binary payload"
        );
    }

    #[test]
    #[ignore = "requires macOS, NSB_NATIVE_PACKAGE and NSB_SKIP_HOSTS=1; downloads official packages and uses temporary service data"]
    fn official_macos_package_lifecycle() {
        assert_eq!(current_os(), "macos");
        assert_eq!(std::env::var("NSB_SKIP_HOSTS").as_deref(), Ok("1"));
        let id = std::env::var("NSB_NATIVE_PACKAGE").expect("NSB_NATIVE_PACKAGE");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (_temp, mut state) = fixture();
        state.paths = Paths::new(_temp.path().join("NiceEnv native data"));
        state.paths.ensure_dirs().unwrap();
        state.store = Store::open(state.paths.db()).unwrap();
        state.store.set_setting("pathEnvEnabled", "0").unwrap();
        state.store.set_setting("portProfile", "safe").unwrap();
        let entry = state.installer.template_for(&id).expect("native package");
        assert!(Installer::is_platform_compatible(&entry));
        let key = format!("{id}@{}", entry.version);
        let installed = runtime.block_on(state.install_package(&key)).unwrap();
        let actual = state.installer.installed_entry(&installed);
        let program = Path::new(&installed.install_path).join(entry_relative_path(&actual.entry));
        assert!(program.is_file(), "{}", program.display());
        if id == "composer" {
            // macOS 清单尚无 PHP；这里只验收 PHAR 安装，不能声称已执行 Composer。
            println!("Composer PHAR installed; execution requires a separately installed PHP");
        } else {
            let argument = match id.as_str() {
                "go" => "version",
                "mihomo" => "-v",
                _ => "--version",
            };
            let output = platform::command(&program).arg(argument).output().unwrap();
            let banner = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.status.success(), "{key}: {banner}");
            assert!(banner.contains(&actual.version), "{key}: {banner}");
            println!(
                "native binary {key}: {}",
                banner.lines().next().unwrap_or("")
            );
        }
        let listed = state.list_packages().unwrap();
        assert_eq!(
            listed
                .iter()
                .filter(|p| p.manifest.id == id
                    && p.manifest.version == actual.version
                    && p.install.is_some())
                .count(),
            1
        );
        crate::versions::clear_cache(&state.store);
        assert_eq!(
            runtime
                .block_on(state.install_package(&key))
                .unwrap()
                .installed_at,
            installed.installed_at
        );
        let reopened = Store::open(state.paths.db()).unwrap();
        let record = reopened.find_installed(&id, Some(&actual.version)).unwrap();
        let offline = Installer {
            manifest: crate::model::Manifest {
                revision: 0,
                packages: vec![],
            },
        };
        assert_eq!(offline.installed_entry(&record).entry, actual.entry);
        assert!(offline.package_views(&[record])[0].install.is_some());

        if matches!(id.as_str(), "mysql" | "mongodb" | "mihomo" | "nats") {
            let service = if id == "mysql" {
                format!("mysql@{}", actual.version)
            } else if actual.run.is_some() {
                crate::generic::service_id_of(&actual)
            } else {
                id.clone()
            };
            struct Cleanup<'a>(&'a crate::CoreState, String);
            impl Drop for Cleanup<'_> {
                fn drop(&mut self) {
                    if self.0.stop_service(&self.1).is_err() {
                        if let Ok(preview) = self.0.service_stop_preview(&self.1) {
                            let _ = self.0.force_stop_service(&self.1, &preview.revision);
                        }
                    }
                }
            }
            let _cleanup = Cleanup(&state, service.clone());
            for _ in 0..2 {
                state.start_service(&service).unwrap_or_else(|error| {
                    panic!(
                        "{key}: {error:?}\n{}",
                        state.manager.tail(&service, 40).join("\n")
                    )
                });
                let running = state.manager.snapshot(&service).unwrap();
                assert_eq!(running.state, crate::model::ServiceState::Running);
                assert!(!running.pids.is_empty());
                assert!(running.pids.iter().all(|pid| platform::process_alive(*pid)));
                state.stop_service(&service).unwrap();
                assert!(running
                    .pids
                    .iter()
                    .all(|pid| !platform::process_alive(*pid)));
                assert!(state.manager.snapshot(&service).unwrap().pids.is_empty());
            }
            println!("{key}: two native start/health/stop cycles passed");
        }
        let preserved = state.paths.data().join(&id).join("audit-preserve.txt");
        std::fs::create_dir_all(preserved.parent().unwrap()).unwrap();
        std::fs::write(&preserved, "retain user data").unwrap();
        state.uninstall_package(&key).unwrap();
        assert!(!Path::new(&installed.install_path).exists());
        assert!(state
            .store
            .find_installed(&id, Some(&actual.version))
            .is_none());
        assert!(state
            .list_packages()
            .unwrap()
            .iter()
            .filter(|p| p.manifest.id == id && p.manifest.version == actual.version)
            .all(|p| p.install.is_none()));
        assert_eq!(
            std::fs::read_to_string(preserved).unwrap(),
            "retain user data"
        );
        println!("{key}: official install, installed list, offline reload, repeat install and uninstall passed");
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "downloads official Nginx into a temporary directory; validates -v without starting a service"]
    async fn official_nginx_install_checks_real_binary_and_uninstalls_cleanly() {
        let (_temp, state) = fixture();
        let entry = state.installer.template_for("nginx").unwrap();
        let key = format!("nginx@{}", entry.version);
        let installed = state.install_package(&key).await.unwrap();
        let program = Path::new(&installed.install_path).join(entry_relative_path(&entry.entry));
        let output = platform::command(&program).arg("-v").output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let banner = String::from_utf8_lossy(&output.stderr);
        assert!(
            banner.contains(&format!("nginx/{}", entry.version)),
            "{banner}"
        );
        assert_eq!(
            state.install_package(&key).await.unwrap().installed_at,
            installed.installed_at
        );
        state.uninstall_package(&key).unwrap();
        assert!(!Path::new(&installed.install_path).exists());
        assert!(state
            .store
            .find_installed("nginx", Some(&entry.version))
            .is_none());
        println!(
            "official download → staged install → {} → idempotent repeat → uninstall passed",
            banner.trim()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "downloads official Apache into a temporary directory; requires NSB_SKIP_HOSTS=1; starts only its own HTTP service"]
    async fn official_apache_rebuilt_release_installs_and_serves_without_touching_system_settings() {
        assert_eq!(std::env::var("NSB_SKIP_HOSTS").as_deref(), Ok("1"));
        let (_temp, mut state) = fixture();
        state.store.set_setting("pathEnvEnabled", "0").unwrap();
        let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let https = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = http.local_addr().unwrap().port();
        state.store.set_port_override("apacheHttp", Some(port)).unwrap();
        state.store.set_port_override("apacheHttps", Some(https.local_addr().unwrap().port())).unwrap();
        let template = state.installer.template_for("apache").unwrap();
        let catalog = crate::versions::catalog(&state.store, &template, true).await;
        assert!(catalog.online, "{:?}", catalog.error);
        let remote = catalog.remote.first().unwrap();
        assert!(remote.sha256.as_ref().is_some_and(|hash| hash.len() == 64));
        let key = format!("apache@{}", remote.version);
        state.installer.manifest.packages.retain(|entry| entry.id != "apache" || entry.version != remote.version);
        let unlisted = state.installer.resolve_entry(&key, &state.store).await.unwrap().unwrap();
        assert_eq!(unlisted.url, remote.url); assert_eq!(unlisted.sha256, remote.sha256);
        let mut stale = Installer::entry_from_remote(&template, remote);
        // 模拟同版本的另一旧构建，不能靠已知 260827 URL 的兼容修复侥幸通过。
        stale.url = format!("https://www.apachelounge.com/download/VS18/binaries/httpd-{}-000101-Win64-VS18.zip", remote.version);
        stale.sha256 = Some("1".repeat(64));
        state.installer.manifest.packages = vec![stale.clone()];
        // 直接用失效构建进入下载链路，确认下载失败后会刷新并只重试同一版本。
        let task = state.downloader.begin_task(&key).unwrap();
        let (refreshed, archive) = state
            .installer
            .download_entry(
                stale,
                &state.paths,
                &state.store,
                &state.downloader,
                &task,
                &|_| {},
            )
            .await
            .unwrap();
        assert_eq!(refreshed.version, remote.version);
        assert_eq!(refreshed.url, remote.url);
        assert_eq!(
            Some(crate::download::sha256_file(&archive).unwrap()),
            remote.sha256
        );
        drop(task);
        let installed = state.install_package(&key).await.unwrap();
        let actual = state.installer.installed_entry(&installed);
        assert_eq!(actual.url, remote.url); assert_eq!(actual.sha256, remote.sha256);
        assert_eq!(crate::download::sha256_file(&state.paths.downloads().join(format!("{key}.pkg"))).unwrap(), remote.sha256.as_ref().unwrap().as_str());
        let binary = Path::new(&installed.install_path).join(&actual.entry);
        let output = platform::command(&binary).arg("-v").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("Apache/{}", remote.version)));
        let views = state.list_packages().unwrap();
        assert_eq!(views.iter().filter(|p| p.manifest.id == "apache" && p.install.is_some()).count(), 1);
        crate::versions::clear_cache(&state.store);
        let before = std::fs::read(Path::new(&installed.install_path).join(".niceenv-package.json")).unwrap();
        assert_eq!(state.install_package(&key).await.unwrap().installed_at, installed.installed_at);
        assert_eq!(std::fs::read(Path::new(&installed.install_path).join(".niceenv-package.json")).unwrap(), before);
        let project = state.paths.base.join("project");
        std::fs::create_dir_all(project.join("assets/.git")).unwrap();
        std::fs::write(project.join("index.html"), "verified Apache rebuild").unwrap();
        std::fs::write(project.join("assets/.git/config"), "private").unwrap();
        let site: crate::model::Site = serde_json::from_value(serde_json::json!({
            "id":"apache-rebuild","name":"Apache rebuild","domains":["rebuild.test"],"rootDir":project,
            "runtime":{"kind":"static","webServer":"apache"},"https":false,"rewrite":"none","createdAt":1,"updatedAt":1
        })).unwrap();
        state.store.save_site(&site).unwrap();
        crate::sites::write_site_conf(&state.paths, &state.store, &site).unwrap();
        struct Cleanup<'a>(&'a crate::CoreState);
        impl Drop for Cleanup<'_> { fn drop(&mut self) { let _ = self.0.stop_service("apache"); } }
        let _cleanup = Cleanup(&state);
        drop((http, https));
        state.start_service("apache").unwrap();
        let client = reqwest::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(5)).build().unwrap();
        for (path, expected) in [("/", 200), ("/assets/.git/config", 403)] {
            let response = client.get(format!("http://127.0.0.1:{port}{path}")).header("Host", "rebuild.test").send().await.unwrap();
            assert_eq!(response.status().as_u16(), expected);
            let body = response.text().await.unwrap();
            if expected == 200 { assert_eq!(body, "verified Apache rebuild"); }
        }
        assert!(state.tail_logs_checked("site:apache-rebuild", 20).unwrap().iter().any(|line| line.line.contains(" 200 ")));
        assert!(state.tail_logs_checked("site-error:apache-rebuild", 20).unwrap().iter().any(|line| line.line.contains(".git")));
        let pids = state.manager.snapshot("apache").unwrap().pids;
        state.stop_service("apache").unwrap();
        assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        state.store.delete_site(&site.id).unwrap();
        state.uninstall_package(&key).unwrap();
        assert!(!Path::new(&installed.install_path).exists());
        assert!(state.store.find_installed("apache", Some(&remote.version)).is_none());
        println!("Apache {}: refreshed same-version build, official SHA256, real download/install/-v, installed list, repeat, HTTP 200/403, logs, stop and uninstall passed", remote.version);
    }

    #[test]
    fn switching_updates_service_metadata_and_runtime_selection() {
        let (_temp, state) = fixture();
        for (id, version) in [
            ("nginx", "1.9.0"),
            ("nginx", "1.31.0"),
            ("node", "9.99.0"),
            ("node", "24.21.0"),
            ("caddy", "2.10.0"),
            ("caddy", "2.11.0"),
        ] {
            install_fixture(&state, id, version);
        }
        crate::ops::register_services(&state.paths, &state.store, &state.manager);
        assert_eq!(
            state.manager.snapshot("nginx").unwrap().version.as_deref(),
            Some("1.31.0")
        );
        state.set_active_version("nginx", "1.9.0").unwrap();
        assert_eq!(
            state.manager.snapshot("nginx").unwrap().version.as_deref(),
            Some("1.9.0")
        );
        state
            .manager
            .set_state("nginx", crate::model::ServiceState::Starting);
        assert_eq!(
            state
                .set_active_version("nginx", "1.31.0")
                .unwrap_err()
                .code,
            "SERVICE_BUSY"
        );
        assert_eq!(
            crate::ops::installed_by_choice(&state.store, "nginx")
                .unwrap()
                .version,
            "1.9.0"
        );
        state.set_active_version("node", "9.99.0").unwrap();
        state.set_active_version("caddy", "2.10.0").unwrap();
        assert_eq!(
            state.manager.snapshot("caddy").unwrap().version.as_deref(),
            Some("2.10.0")
        );
        let packages = state.list_packages().unwrap();
        assert!(packages
            .iter()
            .any(|p| p.manifest.id == "node" && p.manifest.version == "9.99.0" && p.active));
        assert!(!packages
            .iter()
            .any(|p| p.manifest.id == "node" && p.manifest.version == "24.21.0" && p.active));
        assert!(state.manager.snapshot("node").is_none());
    }

    #[test]
    fn uninstall_checks_records_dependencies_and_refreshes_fallback() {
        let (_temp, state) = fixture();
        let old = install_fixture(&state, "nginx", "1.28.1");
        install_fixture(&state, "nginx", "1.31.0");
        state.set_active_version("nginx", "1.31.0").unwrap();
        let missing = state.paths.runtime_dir("nginx", "1.0.0");
        std::fs::create_dir_all(&missing).unwrap();
        assert_eq!(
            state.uninstall_package("nginx@1.0.0").unwrap_err().code,
            "NOT_INSTALLED"
        );
        assert!(missing.exists());
        state.manager.watchdog.note_started("nginx");
        state.uninstall_package("nginx@1.31.0").unwrap();
        assert_eq!(
            state.manager.snapshot("nginx").unwrap().version.as_deref(),
            Some("1.28.1")
        );
        assert_eq!(
            state.store.get_setting("activenginxVersion").as_deref(),
            Some("1.28.1")
        );
        assert!(Path::new(&old.install_path).exists());
        assert!(state
            .manager.watchdog
            .status(&state.watchdog_config())
            .watched
            .is_empty());
        state.uninstall_package("nginx@1.28.1").unwrap();
        assert!(state.manager.snapshot("nginx").is_none());

        let php = install_fixture(&state, "php", "8.4.0");
        install_fixture(&state, "composer", "2.8.0");
        assert_eq!(
            state.uninstall_package("php@8.4.0").unwrap_err().code,
            "PACKAGE_IN_USE"
        );
        assert!(Path::new(&php.install_path).exists());
        install_fixture(&state, "php", "8.3.0");
        state.uninstall_package("php@8.4.0").unwrap();
    }

    #[test]
    fn uninstall_preserves_site_and_fixed_stack_versions() {
        let (_temp, state) = fixture();
        let php = install_fixture(&state, "php", "8.4.0");
        install_fixture(&state, "php", "8.3.0");
        let site: crate::model::Site = serde_json::from_value(serde_json::json!({
            "id":"fixture", "name":"Fixture site", "domains":["fixture.test"],
            "rootDir":state.paths.base, "runtime":{"kind":"php","phpVersion":"8.4.0"},
            "https":false, "rewrite":"none", "status":"stopped", "createdAt":0,"updatedAt":0
        }))
        .unwrap();
        state.store.save_site(&site).unwrap();
        let err = state.uninstall_package("php@8.4.0").unwrap_err();
        assert_eq!(err.code, "PACKAGE_IN_USE");
        assert!(err.message.contains("Fixture site"));
        assert!(Path::new(&php.install_path).exists());
        state.store.delete_site(&site.id).unwrap();
        let stack: crate::model::Stack = serde_json::from_value(serde_json::json!({
            "id":"fixture", "name":"Fixed stack", "items":[{"serviceId":"php@8.4.0"}],
            "createdAt":0,"updatedAt":0
        }))
        .unwrap();
        state.store.save_stack(&stack).unwrap();
        assert_eq!(
            state.uninstall_package("php@8.4.0").unwrap_err().code,
            "PACKAGE_IN_USE"
        );
        state.store.delete_stack(&stack.id).unwrap();
        state.uninstall_package("php@8.4.0").unwrap();
        assert!(state.manager.snapshot("php@8.4.0").is_none());
    }

    #[test]
    fn uninstall_respects_site_mysql_version_bindings() {
        let (_temp, state) = fixture();
        let mysql = install_fixture(&state, "mysql", "8.4.0");
        install_fixture(&state, "mysql", "8.0.40");
        let mut site: crate::model::Site = serde_json::from_value(serde_json::json!({
            "id":"mysql-site", "name":"MySQL fixture", "domains":["mysql.test"],
            "rootDir":state.paths.base, "runtime":{"kind":"static"},
            "db":{"enabled":true,"database":"fixture","username":"fixture","version":"8.4.0"},
            "https":false, "rewrite":"none", "status":"stopped", "createdAt":0,"updatedAt":0
        }))
        .unwrap();
        state.store.save_site(&site).unwrap();
        assert_eq!(
            state.uninstall_package("mysql@8.4.0").unwrap_err().code,
            "PACKAGE_IN_USE"
        );
        assert!(Path::new(&mysql.install_path).exists());
        assert!(state
            .store
            .list_installed()
            .unwrap()
            .iter()
            .any(|p| p.id == "mysql" && p.version == "8.4.0"));
        state.uninstall_package("mysql@8.0.40").unwrap();

        // 跟随默认版本允许保留一个候选，但最后一个版本仍受保护。
        install_fixture(&state, "mysql", "8.0.40");
        site.db.as_mut().unwrap().version = None;
        state.store.save_site(&site).unwrap();
        state.uninstall_package("mysql@8.0.40").unwrap();
        assert_eq!(
            state.uninstall_package("mysql@8.4.0").unwrap_err().code,
            "PACKAGE_IN_USE"
        );

        // 关闭数据库绑定后不再阻止卸载。
        site.db.as_mut().unwrap().version = Some("8.4.0".into());
        site.db.as_mut().unwrap().enabled = false;
        state.store.save_site(&site).unwrap();
        state.uninstall_package("mysql@8.4.0").unwrap();
        assert!(!Path::new(&mysql.install_path).exists());
    }

    #[test]
    fn uninstall_inactive_version_keeps_running_process_and_stops_adopted_process() {
        let (_temp, state) = fixture();
        install_fixture(&state, "caddy", "2.10.0");
        install_fixture(&state, "caddy", "2.11.0");
        state.set_active_version("caddy", "2.11.0").unwrap();
        // 真正的短暂自有子进程；用 Drop 兜底清理，断言失败也不留残余进程。
        struct ChildGuard(std::process::Child);
        impl Drop for ChildGuard {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut command = if cfg!(windows) {
            let mut command = platform::command("powershell.exe");
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ]);
            command
        } else {
            let mut command = platform::command("sleep");
            command.arg("30");
            command
        };
        let child = ChildGuard(command.spawn().unwrap());
        let pid = child.0.id();
        state.manager.adopt("caddy", &[pid], Some(32123));
        state.manager.watchdog.note_started("caddy");
        assert_eq!(
            state
                .set_active_version("caddy", "2.10.0")
                .unwrap_err()
                .code,
            "SERVICE_BUSY"
        );
        state.uninstall_package("caddy@2.10.0").unwrap();
        let running = state.manager.snapshot("caddy").unwrap();
        assert_eq!(running.version.as_deref(), Some("2.11.0"));
        assert_eq!(running.port, Some(32123));
        assert!(platform::process_alive(pid));
        assert_eq!(
            state
                .manager.watchdog
                .status(&state.watchdog_config())
                .watched
                .len(),
            1
        );
        state.uninstall_package("caddy@2.11.0").unwrap();
        assert!(!platform::process_alive(pid));
        assert!(state.manager.snapshot("caddy").is_none());
        assert!(state
            .manager.watchdog
            .status(&state.watchdog_config())
            .watched
            .is_empty());
    }

    fn entry_with(os: Vec<&str>, arch: Vec<&str>) -> PackageManifestEntry {
        serde_json::from_value(serde_json::json!({
            "id": "test", "version": "1.0.0", "category": "tool",
            "displayName": "T", "description": "",
            "os": os, "arch": arch,
            "kind": "binary", "url": "https://example.com/x",
            "sha256": "0", "sizeBytes": 1, "entry": "x"
        }))
        .unwrap()
    }

    #[test]
    fn platform_filter_matches_current_machine() {
        // 本机（编译目标）必须兼容：win/mac 清单里对应本机的条目
        let e = entry_with(vec![current_os()], vec![current_arch()]);
        assert!(Installer::is_platform_compatible(&e));

        // 空数组 = 不限平台
        let e = entry_with(vec![], vec![]);
        assert!(Installer::is_platform_compatible(&e));

        // 仅其它 OS → 不兼容
        let other_os = if current_os() == "windows" {
            "macos"
        } else {
            "windows"
        };
        let e = entry_with(vec![other_os], vec![current_arch()]);
        assert!(!Installer::is_platform_compatible(&e));

        // 仅其它架构 → 不兼容（arm64-only 包装上 x64 必须被拦下）
        let other_arch = if current_arch() == "x64" {
            "arm64"
        } else {
            "x64"
        };
        let e = entry_with(vec![current_os()], vec![other_arch]);
        assert!(!Installer::is_platform_compatible(&e));
    }

    #[test]
    fn reconcile_recovers_native_runtime_when_foreign_arch_comes_first() {
        let (_temp, mut state) = fixture();
        let mut native = entry_with(vec![current_os()], vec![current_arch()]);
        native.entry = "native/bin/program-native".into();
        let mut foreign = native.clone();
        foreign.arch = vec![if current_arch() == "x64" {
            "arm64"
        } else {
            "x64"
        }
        .into()];
        foreign.entry = "foreign/bin/program-foreign".into();
        state.installer.manifest.packages = vec![foreign, native.clone()];
        let runtime = state.paths.runtime_dir(&native.id, &native.version);
        let executable = runtime.join(&native.entry);
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, "native runtime fixture").unwrap();
        let result = state
            .installer
            .reconcile_installed(&state.paths, &state.store)
            .unwrap();
        assert_eq!(result.imported.len(), 1);
        let installed = state
            .store
            .find_installed(&native.id, Some(&native.version))
            .unwrap();
        assert_eq!(Path::new(&installed.install_path), runtime);
        assert!(state
            .installer
            .package_views(&[installed])
            .iter()
            .any(|view| view.install.is_some()));
        assert!(state
            .installer
            .reconcile_installed(&state.paths, &state.store)
            .unwrap()
            .imported
            .is_empty());
        assert_eq!(
            std::fs::read_to_string(executable).unwrap(),
            "native runtime fixture"
        );
    }

    #[test]
    fn version_prefixes_are_normalized_across_manifest_remote_and_install_records() {
        let temp = tempfile::tempdir().unwrap();
        let mut entry = entry_with(vec![], vec![]);
        entry.id = "fixture".into();
        entry.version = "1.2.3".into();
        let installer = Installer {
            manifest: crate::model::Manifest { revision: 1, packages: vec![entry.clone()] },
        };
        assert_eq!(installer.find("fixture@v1.2.3").unwrap().version, "1.2.3");

        let installed = InstalledPackage {
            id: "fixture".into(),
            version: "v1.2.3".into(),
            category: "tool".into(),
            install_path: temp.path().to_string_lossy().into_owned(),
            config_path: String::new(),
            installed_at: 0,
        };
        let views = installer.package_views(std::slice::from_ref(&installed));
        assert_eq!(views.len(), 1);
        assert!(views[0].install.is_some());
        assert_eq!(views[0].available_versions, ["1.2.3"]);
        assert_eq!(views[0].manifest.version, installed.version);

        // 快照描述程序包内的真实路径，版本身份必须来自安装记录。
        entry.entry = "fixture-1.2.3/bin/app.exe".into();
        entry.run = Some(serde_json::from_value(serde_json::json!({
            "args": [], "singleInstance": false
        })).unwrap());
        let snapshot = temp.path().join(".niceenv-package.json");
        let original = serde_json::to_vec(&entry).unwrap();
        std::fs::write(&snapshot, &original).unwrap();
        let recovered = installer.installed_entry(&installed);
        assert_eq!(recovered.version, installed.version);
        assert_eq!(recovered.entry, entry.entry);
        assert_eq!(crate::generic::service_id_of(&recovered), "fixture@v1.2.3");
        assert_eq!(std::fs::read(&snapshot).unwrap(), original);

        let remote = crate::model::RemoteVersion {
            version: "v1.2.4".into(),
            url: "https://example.com/v1.2.4.zip".into(),
            sha256: None,
            size_bytes: None,
            entry: "bin/app.exe".into(),
            kind: "archive".into(),
            prerelease: false,
            note: None,
            released_at: None,
        };
        let remote_entry = Installer::entry_from_remote(&entry, &remote);
        assert_eq!(remote_entry.version, "1.2.4");
        assert_eq!(remote_entry.url, remote.url);
        let mut same = remote.clone();
        same.version = entry.version.clone();
        same.url = entry.url.clone();
        entry.sha256 = Some("a".repeat(64));
        entry.size_bytes = 123;
        let preserved = Installer::entry_from_remote(&entry, &same);
        assert_eq!(preserved.sha256, entry.sha256);
        assert_eq!(preserved.size_bytes, 123);
        let failure = AppError::new("CHECKSUM_MISMATCH", "fixture checksum failure");
        let mut catalog = crate::model::VersionCatalog {
            id: entry.id.clone(),
            remote: vec![same.clone()],
            online: true,
            cached_at: Some(1),
            error: None,
        };
        assert!(
            Installer::download_refresh_candidate(&entry, &catalog, &failure)
                .unwrap()
                .is_none()
        );
        same.sha256 = Some("b".repeat(64));
        catalog.remote = vec![same.clone()];
        assert_eq!(
            Installer::download_refresh_candidate(&entry, &catalog, &failure)
                .unwrap()
                .unwrap()
                .sha256,
            same.sha256
        );
        catalog.online = false;
        assert!(
            Installer::download_refresh_candidate(&entry, &catalog, &failure)
                .unwrap()
                .is_none()
        );
        catalog.online = true;
        catalog.error = Some("upstream offline".into());
        assert!(
            Installer::download_refresh_candidate(&entry, &catalog, &failure)
                .unwrap()
                .is_none()
        );
        catalog.error = None;
        same.version = "9.9.9".into();
        catalog.remote = vec![same];
        let missing = AppError::new("DOWNLOAD_NOT_FOUND", "fixture missing");
        let unavailable =
            Installer::download_refresh_candidate(&entry, &catalog, &missing).unwrap_err();
        assert_eq!(unavailable.code, "PACKAGE_DOWNLOAD_UNAVAILABLE");
        assert!(unavailable.hint.unwrap().contains("9.9.9"));
        assert!(
            Installer::download_refresh_candidate(&entry, &catalog, &failure)
                .unwrap()
                .is_none()
        );
        // 相同 URL 被用于另一个版本时，不能沿用旧版本哈希。
        assert!(Installer::entry_from_remote(&entry, &catalog.remote[0])
            .sha256
            .is_some());
        catalog.remote[0].sha256 = None;
        assert!(Installer::entry_from_remote(&entry, &catalog.remote[0])
            .sha256
            .is_none());
        catalog.remote[0].version = entry.version.clone();
        catalog.remote[0].url = "https://example.com/rebuilt.zip".into();
        assert!(
            Installer::download_refresh_candidate(&entry, &catalog, &failure)
                .unwrap()
                .is_none()
        );
        assert!(
            Installer::download_refresh_candidate(&entry, &catalog, &missing)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn uninstall_removes_install_record_when_version_prefix_differs() {
        let (_temp, state) = fixture();
        let installed = install_fixture(&state, "fixture", "v1.0.0");

        // 调用方通常使用清单里的标准版本；历史数据库记录可能仍带上游 tag 的 v 前缀。
        state.uninstall_package("fixture@1.0.0").unwrap();

        assert!(!Path::new(&installed.install_path).exists());
        assert!(state.store.find_installed("fixture", Some("1.0.0")).is_none());
    }
}

/// 清单解析 + 基本合法性校验（远端快照 / 用户模块共用）。
/// 返回 Err 时调用方必须放弃该来源，绝不能带病上线。
pub fn parse_manifest_str(raw: &str) -> Result<crate::model::Manifest> {
    let m: crate::model::Manifest = serde_json::from_str(raw)
        .map_err(|e| AppError::new("BAD_MANIFEST", format!("清单 JSON 不合法：{e}")))?;
    if m.packages.is_empty() {
        return Err(AppError::new("BAD_MANIFEST", "清单里没有任何套件"));
    }
    for p in &m.packages {
        if p.id.trim().is_empty() || p.version.trim().is_empty() || p.url.trim().is_empty() {
            return Err(AppError::new(
                "BAD_MANIFEST",
                format!("清单条目缺 id/version/url：{}", p.display_name),
            ));
        }
    }
    Ok(m)
}

#[cfg(test)]
mod manifest_layer_tests {
    use super::*;

    fn manifest_with(id: &str, ver: &str, url: &str) -> crate::model::Manifest {
        serde_json::from_value(serde_json::json!({
            "revision": 1, "updated": "t",
            "packages": [{
                "id": id, "version": ver, "category": "tool",
                "displayName": id, "description": "", "os": [], "arch": [],
                "kind": "binary", "url": url, "sha256": "0",
                "sizeBytes": 1, "entry": "x"
            }]
        }))
        .unwrap()
    }

    #[test]
    fn parse_rejects_garbage_and_empty() {
        assert!(parse_manifest_str("not json").is_err());
        assert!(parse_manifest_str("{\"revision\":1,\"packages\":[]}").is_err());
        // 缺 url 的条目
        let bad = r#"{"revision":1,"packages":[{"id":"x","version":"1"}]}"#;
        assert!(parse_manifest_str(bad).is_err());
    }

    #[test]
    fn merge_overrides_same_id_version_and_appends_new() {
        let mut base = Installer {
            manifest: manifest_with("foo", "1.0.0", "https://a/1"),
        };
        base.merge_manifest(manifest_with("foo", "1.0.0", "https://b/1"));
        assert_eq!(base.manifest.packages.len(), 1);
        assert_eq!(
            base.manifest.packages[0].url, "https://b/1",
            "同版本应被覆盖"
        );

        // 上游 tag 常带 v 前缀，不能把 v1.0.0 与 1.0.0 当成两套安装版本。
        let mut prefixed = Installer {
            manifest: manifest_with("prefix", "1.0.0", "https://plain/1"),
        };
        prefixed.merge_manifest(manifest_with("prefix", "v1.0.0", "https://prefixed/1"));
        assert_eq!(prefixed.manifest.packages.len(), 1);
        assert_eq!(prefixed.manifest.packages[0].version, "v1.0.0");
        assert_eq!(prefixed.manifest.packages[0].url, "https://prefixed/1");

        base.merge_manifest(manifest_with("bar", "2.0.0", "https://c/2"));
        assert_eq!(base.manifest.packages.len(), 2);

        let mut native = manifest_with("foo", "1.0.0", "https://native/1");
        native.packages[0].os = vec![current_os().into()];
        native.packages[0].arch = vec![current_arch().into()];
        let mut foreign = native.clone();
        foreign.packages[0].arch = vec![if current_arch() == "x64" { "arm64" } else { "x64" }.into()];
        foreign.packages[0].url = "https://foreign/1".into();
        base.merge_manifest(foreign.clone());
        base.merge_manifest(native.clone());
        assert_eq!(base.find("foo@1.0.0").unwrap().url, "https://native/1");
        assert_eq!(base.template_for("foo").unwrap().url, "https://native/1");
        assert_eq!(base.package_views(&[]).iter().filter(|p| p.manifest.id == "foo").count(), 1);
        assert_eq!(base.package_views(&[]).iter().find(|p| p.manifest.id == "foo").unwrap().manifest.url, "https://native/1");
        native.packages[0].url = "https://native/updated".into();
        base.merge_manifest(native);
        assert_eq!(base.find("foo").unwrap().url, "https://native/updated");
        assert!(base.manifest.packages.iter().any(|p| p.url == "https://foreign/1"));
        base.merge_manifest(manifest_with("foo", "1.0.0", "https://custom/1"));
        assert_eq!(base.manifest.packages.iter().filter(|p| p.id == "foo").count(), 1);
        assert_eq!(base.find("foo").unwrap().url, "https://custom/1");

        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().into());
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.etc().join("manifest.json"), serde_json::to_vec(&foreign).unwrap()).unwrap();
        let effective = Installer::effective(&paths);
        let bundled = Installer::bundled();
        let bundled_ids: std::collections::HashSet<_> = bundled
            .manifest
            .packages
            .iter()
            .map(|entry| entry.id.as_str())
            .collect();
        if bundled_ids.contains("mongosh") {
            assert!(
                effective.find("mongosh").is_some(),
                "旧快照不能隐藏新内置包"
            );
            assert!(effective.find("mongodb-database-tools").is_some());
        }
        assert_eq!(effective.find("foo").unwrap().url, "https://foreign/1");
        std::fs::create_dir_all(paths.base.join("user-modules")).unwrap();
        let custom = manifest_with("mongosh", "2.12.0", "https://custom/mongosh");
        std::fs::write(paths.base.join("user-modules/custom.json"), serde_json::to_vec(&custom).unwrap()).unwrap();
        assert_eq!(Installer::effective(&paths).find("mongosh@2.12.0").unwrap().url, "https://custom/mongosh");
    }
}

#[cfg(test)]
mod find_latest_tests {
    use super::*;

    #[test]
    fn bare_id_picks_semantically_latest_version() {
        let pkg = |id: &str, ver: &str| {
            serde_json::json!({
                "id": id, "version": ver, "category": "tool",
                "displayName": id, "description": "", "os": [], "arch": [],
                "kind": "binary", "url": "https://example.com/x", "sha256": "0",
                "sizeBytes": 1, "entry": "x"
            })
        };
        // 真实清单里的两组：字符串比较会选成 5.26.30 / 21.0.9+10
        let manifest: crate::model::Manifest = serde_json::from_value(serde_json::json!({
            "revision": 1, "updated": "t",
            "packages": [
                pkg("neo4j", "5.26.30"), pkg("neo4j", "2025.09.0"), pkg("neo4j", "5.25.1"),
                pkg("jdk", "21.0.9+10"), pkg("jdk", "21.0.8+9"), pkg("jdk", "21.0.12+8"),
                pkg("zincsearch", "1.0.0-beta3"), pkg("zincsearch", "0.4.10"),
                pkg("preview-only", "2.0.0-rc.10"), pkg("preview-only", "2.0.0-rc.9"),
            ]
        }))
        .unwrap();
        let inst = Installer { manifest };
        assert_eq!(inst.find("neo4j").unwrap().version, "2025.09.0");
        assert_eq!(inst.find("jdk").unwrap().version, "21.0.12+8");
        assert_eq!(inst.find("zincsearch").unwrap().version, "0.4.10");
        assert_eq!(inst.template_for("zincsearch").unwrap().version, "0.4.10");
        assert_eq!(inst.find("zincsearch@1.0.0-beta3").unwrap().version, "1.0.0-beta3");
        assert_eq!(inst.find("preview-only").unwrap().version, "2.0.0-rc.10");
        // 显式指定版本不受影响
        assert_eq!(inst.find("neo4j@5.25.1").unwrap().version, "5.25.1");
        let mut native = inst.find("jdk@21.0.9+10").unwrap();
        native.os = vec![current_os().into()];
        native.arch = vec![current_arch().into()];
        let mut foreign = native.clone();
        foreign.arch = vec![if current_arch() == "x64" { "arm64" } else { "x64" }.into()];
        foreign.version = "99.0.0".into();
        let mut inst = Installer { manifest: crate::model::Manifest { revision: 1, packages: vec![foreign, native.clone()] } };
        assert_eq!(inst.find("jdk").unwrap().version, native.version);
        let mut foreign_twin = native.clone();
        foreign_twin.arch = vec!["unsupported".into()];
        inst.manifest.packages.insert(0, foreign_twin);
        assert_eq!(inst.find("jdk@21.0.9+10").unwrap().arch, native.arch);
        let views = inst.package_views(&[]);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].manifest.version, native.version);
        assert_eq!(views[0].manifest.arch, native.arch);
    }

    #[test]
    fn bundled_nginx_prefers_the_latest_manifest_version() {
        // Nginx 官方二进制目前只收录在 Windows 清单；在任意 CI 主机审计同一份发布数据。
        let inst = Installer {
            manifest: serde_json::from_str(include_str!("../../../manifest/packages.win.json"))
                .unwrap(),
        };
        let nginx = inst.find("nginx").expect("清单应包含 Nginx");
        assert_eq!(nginx.version, "1.31.6");
        assert_eq!(nginx.size_bytes, 2_797_070);
        assert!(nginx.sha256.as_deref().is_some_and(|sha| sha.len() == 64));
    }
}

#[cfg(test)]
mod zip_slip_tests {
    use super::*;
    use std::io::Write;

    fn write_gzip_tar_link(path: &Path, entry_type: tar::EntryType, name: &str, link: &str) {
        let file = std::fs::File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_path(name).unwrap();
        header.set_entry_type(entry_type);
        header.set_link_name(link).unwrap();
        header.set_size(0);
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
    }

    fn write_gzip_tar_raw_path(path: &Path, name: &str, body: &[u8]) {
        let file = std::fs::File::create(path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_path("placeholder").unwrap();
        let raw = name.as_bytes();
        assert!(raw.len() < 100);
        let bytes = header.as_mut_bytes();
        bytes[..100].fill(0);
        bytes[..raw.len()].copy_from_slice(raw);
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, body).unwrap();
        builder.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn safe_archive_path_rejects_escapes() {
        for bad in [
            "/etc/cron.d/evil",
            "\\Windows\\evil.dll",
            "C:/Windows/evil.dll",
            "C:\\Windows\\evil.dll",
            "../evil",
            "a/../../evil",
            "a\\..\\..\\evil",
            "file.txt:stream",
            "",
            "./",
        ] {
            assert!(safe_archive_path(bad).is_none(), "应拒绝 {bad:?}");
        }
    }

    #[test]
    fn safe_archive_path_keeps_normal_entries() {
        let p = safe_archive_path("nginx-1.28.0\\conf\\nginx.conf").unwrap();
        assert_eq!(
            p,
            std::path::Path::new("nginx-1.28.0")
                .join("conf")
                .join("nginx.conf")
        );
        // 文件名里带 `..` 但不是 `..` 段：合法，不能误杀
        let p = safe_archive_path("./php/ext/php..ini-dev").unwrap();
        assert_eq!(
            p,
            std::path::Path::new("php").join("ext").join("php..ini-dev")
        );
    }

    #[test]
    fn extract_zip_never_writes_outside_dest() {
        let base = std::env::temp_dir().join(format!("nsb-zipslip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dest = base.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let outside = base.join("outside.txt");
        let archive = base.join("evil.zip");

        let mut w = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file("ok/readme.txt", opts).unwrap();
        w.write_all(b"ok").unwrap();
        w.start_file("../outside.txt", opts).unwrap();
        w.write_all(b"evil").unwrap();
        w.start_file(outside.to_string_lossy().to_string(), opts)
            .unwrap();
        w.write_all(b"evil").unwrap();
        w.finish().unwrap();
        // 确认恶意条目名原样进了压缩包（否则这个测试什么也没测到）
        let names: Vec<String> = zip::ZipArchive::new(std::fs::File::open(&archive).unwrap())
            .unwrap()
            .file_names()
            .map(String::from)
            .collect();
        assert!(names.iter().any(|n| n == "../outside.txt"));
        assert!(names.iter().any(|n| *n == outside.to_string_lossy()));

        extract_zip(&archive, &dest).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("ok/readme.txt")).unwrap(),
            "ok"
        );
        assert!(!outside.exists(), "绝对路径 / .. 条目不能写到解压目录之外");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn extract_targz_keeps_paths_inside_dest_and_supports_normal_files() {
        let base = tempfile::tempdir().unwrap();
        let dest = base.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let archive = base.path().join("normal.tar.gz");
        write_gzip_tar(&archive, &[("bin/program", b"ok")]);
        let downloader = crate::download::Downloader::new();
        let task = downloader.begin_task("tar-normal").unwrap();
        extract_targz(
            &archive,
            &dest,
            "bin/program",
            "https://example.invalid/normal.tar.gz",
            &task,
        )
        .unwrap();
        assert_eq!(std::fs::read(dest.join("bin/program")).unwrap(), b"ok");

        let outside = base.path().join("escape.txt");
        let archive = base.path().join("escape.tar.gz");
        write_gzip_tar_raw_path(&archive, "../escape.txt", b"must not escape");
        let task = downloader.begin_task("tar-escape").unwrap();
        let error = extract_targz(
            &archive,
            &dest,
            "bin/program",
            "https://example.invalid/escape.tar.gz",
            &task,
        )
        .unwrap_err();
        assert_eq!(error.code, "EXTRACT_FAILED");
        assert!(!outside.exists());
    }

    #[test]
    fn extract_targz_rejects_symlink_and_hardlink_entries() {
        let base = tempfile::tempdir().unwrap();
        let dest = base.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let downloader = crate::download::Downloader::new();
        for (suffix, entry_type) in [
            ("symlink", tar::EntryType::symlink()),
            ("hardlink", tar::EntryType::hard_link()),
        ] {
            let archive = base.path().join(format!("{suffix}.tar.gz"));
            write_gzip_tar_link(&archive, entry_type, "link", "outside");
            let task = downloader.begin_task(&format!("tar-{suffix}")).unwrap();
            let error = extract_targz(
                &archive,
                &dest,
                "bin/program",
                &format!("https://example.invalid/{suffix}.tar.gz"),
                &task,
            )
            .unwrap_err();
            assert_eq!(error.code, "EXTRACT_FAILED");
            assert!(!dest.join("link").exists());
        }
    }

    #[test]
    fn sevenzip_extracts_with_safe_relative_paths() {
        let base = std::env::temp_dir().join(format!("nsb-sevenzip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let source = base.join("source.bin");
        let archive = base.join("sample.7z");
        let dest = base.join("dest");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&source, b"sevenzip fixture").unwrap();

        let mut writer = sevenz_rust::SevenZWriter::create(&archive).unwrap();
        let entry = sevenz_rust::SevenZArchiveEntry::from_path(
            &source,
            "ruby/bin/ruby.exe".to_string(),
        );
        writer
            .push_archive_entry(entry, Some(std::fs::File::open(&source).unwrap()))
            .unwrap();
        writer.finish().unwrap();

        let downloader = crate::download::Downloader::new();
        let task = downloader.begin_task("sevenzip-fixture").unwrap();
        extract_sevenzip_checked(&archive, &dest, &task).unwrap();
        assert_eq!(
            std::fs::read(dest.join("ruby/bin/ruby.exe")).unwrap(),
            b"sevenzip fixture"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn package_key_must_be_plain_path_segments() {
        assert!(ensure_safe_key("php", "8.3.33").is_ok());
        assert!(ensure_safe_key("temurin-jdk21", "21.0.12+8").is_ok());
        assert!(ensure_safe_key("x", "../..").is_err());
        assert!(ensure_safe_key("..", "1.0").is_err());
        assert!(ensure_safe_key("a/b", "1.0").is_err());
        assert!(ensure_safe_key("a", "1\\..\\..").is_err());
        assert!(ensure_safe_key("a", "").is_err());
    }

    #[test]
    fn entry_relative_path_drops_traversal_segments() {
        assert_eq!(
            entry_relative_path("../../bin/../mysqld"),
            std::path::Path::new("bin").join("mysqld")
        );
        assert_eq!(
            entry_relative_path("C:/x/y.exe"),
            std::path::Path::new("x").join("y.exe")
        );
    }
}
