//! 套件安装管线：下载(断点续传/校验) → 解压 → 生成默认配置 → 注册服务。

use crate::download::Downloader;
use crate::error::{AppError, Result};
use crate::model::InstalledPackage;
use crate::paths::Paths;
use crate::store::Store;
use std::path::Path;
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

fn is_github_url(url: &str) -> bool {
    url.starts_with("https://github.com/")
        || url.starts_with("https://raw.githubusercontent.com/")
        || url.starts_with("https://objects.githubusercontent.com/")
}

impl Installer {
    pub fn bundled() -> Self {
        #[cfg(windows)]
        let raw = include_str!("../../../manifest/packages.win.json");
        #[cfg(not(windows))]
        let raw = include_str!("../../../manifest/packages.mac.json");
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
                    Ok(m) => inst.manifest = m,
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

    /// 把另一份清单合并进来：同 (id, version) 以新清单为准（用户覆盖内置），否则追加
    pub fn merge_manifest(&mut self, other: crate::model::Manifest) {
        for p in other.packages {
            match self
                .manifest
                .packages
                .iter_mut()
                .find(|e| e.id == p.id && e.version == p.version)
            {
                Some(slot) => *slot = p,
                None => self.manifest.packages.push(p),
            }
        }
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
            .filter(|p| p.id == id && version.map_or(true, |v| p.version == v))
            .cloned()
            .collect();
        // 按版本号语义取最新：字符串比较会把 5.26.30 排在 2025.09.0 前、21.0.9 排在 21.0.12 前
        candidates.sort_by(|a, b| crate::versions::cmp_version_desc(&a.version, &b.version));
        candidates.into_iter().next()
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
                .then_with(|| b.version_source.is_some().cmp(&a.version_source.is_some()))
                .then_with(|| crate::versions::cmp_version_desc(&a.version, &b.version))
        });
        entries.first().map(|p| (*p).clone())
    }

    /// 安装记录是已安装版本的依据。优先读取安装时保存的实际入口和服务描述，
    /// 旧版安装未保存描述时，再用清单的同版本条目或同套件模板恢复。
    pub fn installed_entry(
        &self,
        installed: &InstalledPackage,
    ) -> crate::model::PackageManifestEntry {
        let snapshot = Path::new(&installed.install_path).join(".niceenv-package.json");
        if let Some(entry) = std::fs::read_to_string(snapshot)
            .ok()
            .and_then(|raw| serde_json::from_str::<crate::model::PackageManifestEntry>(&raw).ok())
            .filter(|entry| entry.id == installed.id && entry.version == installed.version)
        {
            return entry;
        }
        if let Some(entry) = self.find(&format!("{}@{}", installed.id, installed.version)) {
            return entry;
        }
        if let Some(mut entry) = self.template_for(&installed.id) {
            let source = crate::versions::source_for(&entry);
            entry.entry = source
                .and_then(|s| s.entry_template)
                .filter(|tpl| !tpl.contains("{asset}"))
                .map(|tpl| tpl.replace("{version}", &installed.version))
                .unwrap_or_else(|| entry.entry.replace(&entry.version, &installed.version));
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
        let mut entries = self.manifest.packages.clone();
        for package in installed {
            let entry = self.installed_entry(package);
            if let Some(current) = entries
                .iter_mut()
                .find(|p| p.id == package.id && p.version == package.version)
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
                    .map(|p| p.version.clone())
                    .collect();
                available_versions.sort_by(|a, b| crate::versions::cmp_version_desc(a, b));
                available_versions.dedup();
                crate::model::PackageView {
                    manifest: entry.clone(),
                    install: installed
                        .iter()
                        .find(|p| p.id == entry.id && p.version == entry.version)
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
        e.version = remote.version.clone();
        e.display_name = e.display_name.replace(&template.version, &remote.version);
        e.url = remote.url.clone();
        e.sha256 = remote.sha256.clone();
        e.size_bytes = remote.size_bytes.unwrap_or(0);
        e.kind = remote.kind.clone();
        e.entry = remote.entry.clone();
        // 远程版本默认不带 mirrors（镜像策略仍在下载时按域名前缀应用）
        e.mirrors.clear();
        e
    }

    /// 解析安装 key，支持「清单里没有但版本源枚举得到」的版本：
    /// 先查清单，未命中则查版本目录缓存/远程。
    pub async fn resolve_entry(
        &self,
        key: &str,
        store: &Store,
    ) -> Result<Option<crate::model::PackageManifestEntry>> {
        if let Some(hit) = self.find(key) {
            return Ok(Some(hit));
        }
        let Some((id, version)) = key.split_once('@') else {
            return Ok(None);
        };
        let Some(template) = self.template_for(id) else {
            return Ok(None);
        };
        let cat = crate::versions::catalog(store, &template, false).await;
        let Some(remote) = cat.remote.iter().find(|r| r.version == version) else {
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

    pub async fn install(
        &self,
        key: &str,
        paths: &Paths,
        store: &Store,
        downloader: &Arc<Downloader>,
        emit: &dyn Fn(crate::Event),
    ) -> Result<InstalledPackage> {
        let task_id = self
            .find(key)
            .map(|entry| format!("{}@{}", entry.id, entry.version))
            .unwrap_or_else(|| key.to_string());
        let task = downloader.begin_task(&task_id)?;
        let result = self
            .install_task(key, paths, store, downloader, &task, emit)
            .await;
        if let Err(err) = &result {
            emit(crate::Event::DownloadProgress(
                crate::model::DownloadProgress {
                    task_id,
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
            task.begin_commit()?;
            emit(crate::Event::state(&task_id, "installed"));
            return Ok(p);
        }

        emit(crate::Event::state(&task_id, "downloading"));
        let urls = self.candidate_urls(&entry, store);
        let archive = downloader
            .download_with_task(
                task,
                &urls,
                entry.sha256.as_deref().unwrap_or("0"),
                entry.size_bytes,
                paths,
                emit,
            )
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
        // entry 声明的路径不存在时按「精确文件名 → 包内唯一文件」容错定位并搬移。
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
            store.upsert_installed(&installed)?;
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
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn uninstall(
        &self,
        key: &str,
        paths: &Paths,
        store: &Store,
        manager: &Arc<crate::services::ServiceManager>,
    ) -> Result<Option<String>> {
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
        // 下面会 remove_dir_all(runtimes/{id}/{version})，先挡住 `x@../..` 这类 key
        ensure_safe_key(id, version)?;
        if let Some(console) = crate::toolbox::adminer_status(manager)? {
            if (id == "php" && *version == console.php_version)
                || (id == "adminer" && *version == console.adminer_version) {
                return Err(AppError::new("PACKAGE_IN_USE", "该版本正在运行数据库管理台，请先在工具箱停止 Adminer"));
            }
        }
        self.check_uninstall_references(store, &installed)?;
        let entry = self.installed_entry(&installed);
        let service_id = if id == "php" || id == "mysql" {
            Some(format!("{id}@{version}"))
        } else if crate::generic::is_builtin(id) || entry.run.is_some() {
            Some(crate::generic::service_id_of(&entry))
        } else {
            None
        };
        // 单实例的另一个版本可能正在运行，只有版本吻合才能停止和移除注册。
        let stopped_service = service_id.filter(|sid| {
            manager
                .snapshot(sid)
                .is_some_and(|s| s.version.as_deref() == Some(version.as_str()))
        });
        if let Some(sid) = &stopped_service {
            crate::ops::stop_service(store, paths, manager, sid)?;
        }

        let runtime_dir = paths.runtime_dir(id, version);
        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir).map_err(|e| AppError::io("删除运行时目录", e))?;
        }
        store.remove_installed(id, version)?;
        if let Some(sid) = &stopped_service {
            manager.services.lock().remove(sid);
        }
        if store.get_setting(&format!("active{id}Version")).as_deref() == Some(version) {
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
    fn check_uninstall_references(&self, store: &Store, target: &InstalledPackage) -> Result<()> {
        let installed = store.list_installed()?;
        let has_alternative = installed
            .iter()
            .any(|p| p.id == target.id && p.version != target.version);
        let references_target = |key: &str| match key.split_once('@') {
            Some((id, version)) => id == target.id && version == target.version,
            None => key == target.id && !has_alternative,
        };
        let mut users = Vec::new();
        for site in store.list_sites()? {
            let php_ref = target.id == "php"
                && site.runtime.kind == crate::model::SiteKind::Php
                && site.runtime.php_version.as_deref() == Some(target.version.as_str());
            let web_ref = site.runtime.web_server == target.id && !has_alternative;
            let db_ref = target.id == "mysql"
                && site.db.as_ref().is_some_and(|db| {
                    db.enabled
                        && db.version.as_deref().map_or(!has_alternative, |version| {
                            version == target.version.as_str()
                        })
                });
            if php_ref || web_ref || db_ref {
                users.push(format!("站点「{}」", site.name));
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
                users.push(format!("服务栈「{}」", stack.name));
            }
        }
        for package in installed
            .iter()
            .filter(|p| p.id != target.id || p.version != target.version)
        {
            let entry = self.installed_entry(package);
            let required = entry
                .requires
                .iter()
                .chain(entry.depends.iter())
                .chain(entry.run.iter().flat_map(|run| run.requires.iter()));
            if required.into_iter().any(|dep| references_target(dep)) {
                users.push(format!("套件 {} {}", entry.display_name, package.version));
            }
        }
        if !users.is_empty() {
            return Err(AppError::new(
                "PACKAGE_IN_USE",
                format!(
                    "无法卸载 {} {}：仍被 {} 使用",
                    target.id,
                    target.version,
                    users.join("、")
                ),
            )
            .with_hint("先修改相关站点或服务栈的版本绑定，或卸载依赖它的套件后重试"));
        }
        Ok(())
    }
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
///  2. 解压树里按文件名精确匹配（不分大小写，取目录最浅的）→ 搬到声明位置；
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

/// tar.gz 解压：调用系统 tar（macOS bsdtar / Windows 10+ 内置）。
/// `.gz` 单文件（mihomo 等）按源 URL 识别，落到清单声明的 entry 路径。
/// 缓存名没有原始扩展名；tar 包失败不能退回 gunzip，否则会把整个 tar 误当主程序。
fn extract_targz(
    archive: &Path,
    dest: &Path,
    entry: &str,
    source_url: &str,
    task: &crate::download::DownloadTask,
) -> Result<()> {
    let single_gzip = reqwest::Url::parse(source_url).ok().is_some_and(|url| {
        let path = url.path().to_ascii_lowercase();
        path.ends_with(".gz") && !path.ends_with(".tar.gz")
    });
    if !single_gzip {
        let mut tar = platform::command("tar");
        tar.arg("-xzf").arg(archive).arg("-C").arg(dest);
        tar.stdout(std::process::Stdio::null());
        let (status, stderr) = run_extractor(&mut tar, task)?;
        if !status.success() {
            return Err(AppError::new("EXTRACT_FAILED", "tar.gz 解压失败").with_detail(stderr));
        }
        return Ok(());
    }
    // 单文件 .gz → gunzip 到 entry 指定的相对路径
    let target = dest.join(entry_relative_path(entry));
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppError::io("创建解压目录", e))?;
    }
    let mut gzip = platform::command("gzip");
    gzip.arg("-dc")
        .arg(archive)
        .stdout(std::fs::File::create(&target)?);
    let (status, stderr) = run_extractor(&mut gzip, task)?;
    if !status.success() {
        return Err(AppError::new("EXTRACT_FAILED", "gzip 解压失败").with_detail(stderr));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn copy_checked(
    input: &mut impl std::io::Read,
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

fn run_extractor(
    command: &mut std::process::Command,
    task: &crate::download::DownloadTask,
) -> Result<(std::process::ExitStatus, String)> {
    use std::io::{Read, Seek};
    task.check_cancelled()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdin(std::process::Stdio::null())
        .stderr(stderr.try_clone()?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(platform::spawn_pre_exec);
        }
    }
    let mut group = platform::ProcessGroup::new()?;
    let mut child = command
        .spawn()
        .map_err(|e| AppError::io("启动套件解压程序", e))?;
    if let Err(err) = group.attach(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err.into());
    }
    let result = loop {
        if let Err(err) = task.check_cancelled() {
            break Err(err);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
            Err(err) => break Err(AppError::io("等待套件解压程序", err)),
        }
    };
    if result.is_err() {
        let _ = group.terminate(true);
        let _ = child.kill();
    }
    let _ = child.wait();
    let status = result?;
    stderr.rewind()?;
    let mut detail = String::new();
    stderr.take(16 * 1024).read_to_string(&mut detail)?;
    Ok((status, detail))
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
            watchdog: Arc::new(crate::watchdog::Watchdog::new()),
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
        let output = platform::command("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&source)
            .arg("program.bin")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
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
        state.watchdog.note_started("nginx");
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
            .watchdog
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
        state.watchdog.note_started("caddy");
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
                .watchdog
                .status(&state.watchdog_config())
                .watched
                .len(),
            1
        );
        state.uninstall_package("caddy@2.11.0").unwrap();
        assert!(!platform::process_alive(pid));
        assert!(state.manager.snapshot("caddy").is_none());
        assert!(state
            .watchdog
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

        base.merge_manifest(manifest_with("bar", "2.0.0", "https://c/2"));
        assert_eq!(base.manifest.packages.len(), 2);
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
            ]
        }))
        .unwrap();
        let inst = Installer { manifest };
        assert_eq!(inst.find("neo4j").unwrap().version, "2025.09.0");
        assert_eq!(inst.find("jdk").unwrap().version, "21.0.12+8");
        // 显式指定版本不受影响
        assert_eq!(inst.find("neo4j@5.25.1").unwrap().version, "5.25.1");
    }
}

#[cfg(test)]
mod zip_slip_tests {
    use super::*;
    use std::io::Write;

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
