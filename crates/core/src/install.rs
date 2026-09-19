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

impl Installer {
    pub fn bundled() -> Self {
        #[cfg(windows)]
        let raw = include_str!("../../../manifest/packages.win.json");
        #[cfg(not(windows))]
        let raw = include_str!("../../../manifest/packages.mac.json");
        let manifest: crate::model::Manifest = serde_json::from_str(raw)
            .expect("内置清单 JSON 必须合法");
        Self { manifest }
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
        candidates.sort_by(|a, b| b.version.cmp(&a.version));
        candidates.into_iter().next()
    }

    /// 找「模板条目」：同 id 的任一条目（用于合成远程版本的可安装条目）。
    pub fn template_for(&self, id: &str) -> Option<crate::model::PackageManifestEntry> {
        self.manifest
            .packages
            .iter()
            .find(|p| p.id == id)
            .cloned()
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
        e.url = remote.url.clone();
        e.sha256 = remote.sha256.clone();
        if let Some(sz) = remote.size_bytes {
            e.size_bytes = sz;
        }
        e.kind = remote.kind.clone();
        e.entry = remote.entry.clone();
        // 远程版本默认不带 mirrors（镜像策略仍在下载时按域名前缀应用）
        e
    }

    /// 解析安装 key，支持「清单里没有但版本源枚举得到」的版本：
    /// 先查清单，未命中则查版本目录缓存/远程。
    pub async fn resolve_entry(
        &self,
        key: &str,
        store: &Store,
    ) -> Option<crate::model::PackageManifestEntry> {
        if let Some(hit) = self.find(key) {
            return Some(hit);
        }
        let (id, version) = key.split_once('@')?;
        let template = self.template_for(id)?;
        let cat = crate::versions::catalog(store, &template, false).await;
        let remote = cat.remote.iter().find(|r| r.version == version)?;
        Some(Self::entry_from_remote(&template, remote))
    }

    /// 镜像策略：official → [url] + mirrors；ghproxy → github 前缀加速；custom → 自定义前缀
    fn candidate_urls(&self, entry: &crate::model::PackageManifestEntry, store: &Store) -> Vec<String> {
        let mirror = store.get_setting("mirror").unwrap_or_else(|| "official".into());
        let mut urls = vec![entry.url.clone()];
        match mirror.as_str() {
            "ghproxy" => {
                let mut gh: Vec<String> = entry
                    .url
                    .starts_with("https://github.com")
                    .then(|| format!("https://ghproxy.net/{}", entry.url))
                    .into_iter()
                    .collect();
                gh.extend(urls.clone());
                urls = gh;
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
        // 先查清单内置版本；未命中但版本源能枚举到时，用远程版本合成条目
        let entry = match self.resolve_entry(key, store).await {
            Some(e) => e,
            None => {
                return Err(AppError::new(
                    "PACKAGE_NOT_FOUND",
                    format!("清单与版本源里都没有套件 {key}"),
                )
                .with_hint("在套件页点「刷新版本」获取最新版本列表后再试"))
            }
        };

        let version = entry.version.clone();
        let task_id = format!("{}@{}", entry.id, version);

        // 已装则幂等返回
        if let Some(p) = store.find_installed(&entry.id, Some(&version)) {
            return Ok(p);
        }

        emit(crate::Event::state(&task_id, "downloading"));
        let urls = self.candidate_urls(&entry, store);
        let archive = downloader
            .download(&task_id, &urls, entry.sha256.as_deref().unwrap_or("0"), entry.size_bytes, paths, emit)
            .await?;

        emit(crate::Event::state(&task_id, "extracting"));
        let runtime_dir = paths.runtime_dir(&entry.id, &version);
        std::fs::create_dir_all(&runtime_dir)?;
        match entry.kind.as_str() {
            "archive" => extract_zip(&archive, &runtime_dir)?,
            // tar.gz / gz：用系统 tar（macOS 自带 bsdtar；Windows 10+ 亦内置）
            "targz" => extract_targz(&archive, &runtime_dir, &entry.entry)?,
            // 单文件（composer.phar 等）：直接落盘
            "binary" => {
                let dest = runtime_dir.join(entry_relative_path(&entry.entry));
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| AppError::io("创建单文件包目录", e))?;
                }
                std::fs::copy(&archive, &dest).map_err(|e| AppError::io("写入单文件包", e))?;
            }
            other => {
                return Err(AppError::new(
                    "UNSUPPORTED_KIND",
                    format!("暂不支持的包格式 {other}"),
                ));
            }
        }

        // 校验入口存在。entry 清单里一律用 '/' 分隔；按段 join，Windows/macOS 通用
        // （不能整串 replace('/','\\')：macOS 上会变成单个含反斜杠的文件名而永远找不到）
        let entry_path = runtime_dir.join(entry_relative_path(&entry.entry));
        if !entry_path.exists() {
            return Err(AppError::new(
                "ENTRY_MISSING",
                format!("解压后找不到主程序 {}", entry.entry),
            )
            .with_detail(format!("期望路径：{}", entry_path.display())));
        }

        emit(crate::Event::state(&task_id, "configuring"));
        let installed = InstalledPackage {
            id: entry.id.clone(),
            version: version.clone(),
            category: entry.category.clone(),
            install_path: runtime_dir.to_string_lossy().to_string(),
            config_path: paths.etc_dir(&entry.id, &version).to_string_lossy().to_string(),
            installed_at: crate::services::now_ms(),
        };
        store.upsert_installed(&installed)?;

        // 默认配置
        self.ensure_default_configs(&entry, paths, store)?;
        emit(crate::Event::state(&task_id, "installed"));
        Ok(installed)
    }

    pub fn ensure_default_configs(
        &self,
        entry: &crate::model::PackageManifestEntry,
        paths: &Paths,
        store: &Store,
    ) -> Result<()> {
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
                crate::configgen::write_httpd_conf(paths, &root, &pools, ports.apache_http, ports.apache_https)?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn uninstall(&self, key: &str, paths: &Paths, store: &Store, manager: &Arc<crate::services::ServiceManager>) -> Result<()> {
        let (id, version) = match key.split_once('@') {
            Some((i, v)) => (i.to_string(), v.to_string()),
            None => {
                let inst = store
                    .find_installed(key, None)
                    .ok_or_else(|| AppError::not_installed(key))?;
                (inst.id, inst.version)
            }
        };
        // 停服务（忽略未运行错误）
        let service_id = if id == "php" || id == "mysql" {
            format!("{id}@{version}")
        } else {
            id.clone()
        };
        let _ = crate::ops::stop_service(store, paths, manager, &service_id);

        let runtime_dir = paths.runtime_dir(&id, &version);
        if runtime_dir.exists() {
            std::fs::remove_dir_all(&runtime_dir)
                .map_err(|e| AppError::io("删除运行时目录", e))?;
        }
        store.remove_installed(&id, &version)?;
        Ok(())
    }
}

/// 把清单里的相对入口路径拆成平台无关的多段 join。
/// 清单统一用 '/'；Windows 的 Path::join 能正确吃掉 '/'，macOS 也如此。
pub fn entry_relative_path(entry: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::new();
    for seg in entry.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".") {
        p.push(seg);
    }
    p
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {    let file = std::fs::File::open(archive).map_err(|e| AppError::io("打开压缩包", e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| AppError::internal("读取压缩包", e.to_string()))?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| AppError::internal("读取压缩条目", e.to_string()))?;
        // 防路径穿越
        let name = entry.name().replace('\\', "/");
        if name.contains("..") {
            continue;
        }
        let out_path = dest.join(&name);
        if entry.is_dir() {
            // 保留压缩包里的空目录（nginx 的 logs/temp 等）
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&out_path)
            .map_err(|e| AppError::io("创建文件", e))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| AppError::io("解压写入", e))?;
    }
    Ok(())
}

/// tar.gz 解压：调用系统 tar（macOS bsdtar / Windows 10+ 内置）。
/// `.gz` 单文件（mihomo 等）tar 会失败，退回 gunzip 并落到清单声明的 entry 路径
/// ——不能用下载缓存文件名推导（缓存名是 `{id}@{ver}.zip`，与真实文件名无关）。
fn extract_targz(archive: &Path, dest: &Path, entry: &str) -> Result<()> {
    let out = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|e| AppError::io("执行 tar 解压", e))?;
    if out.status.success() {
        return Ok(());
    }
    // 单文件 .gz → gunzip 到 entry 指定的相对路径
    let out = std::process::Command::new("gzip")
        .arg("-dc")
        .arg(archive)
        .output()
        .map_err(|e| AppError::io("执行 gzip 解压", e))?;
    if !out.status.success() {
        return Err(AppError::new("EXTRACT_FAILED", "tar.gz 解压失败")
            .with_detail(String::from_utf8_lossy(&out.stderr).to_string()));
    }
    let target = dest.join(entry_relative_path(entry));
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AppError::io("创建解压目录", e))?;
    }
    std::fs::write(&target, out.stdout).map_err(|e| AppError::io("写入解压文件", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
    }
    Ok(())
}
