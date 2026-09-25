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
        let manifest: crate::model::Manifest = serde_json::from_str(raw)
            .expect("内置清单 JSON 必须合法");
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
                let Ok(raw) = std::fs::read_to_string(&f) else { continue };
                let Ok(m) = parse_manifest_str(&raw) else { continue };
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

    /// 镜像策略：official → [url] + mirrors；ghproxy → github 前缀加速；custom → 自定义前缀。
    /// 无论镜像怎么设置，GitHub 直连失败后都会自动尝试加速前缀兜底 ——
    /// 默认设置下被墙不再需要用户手动到设置里切镜像再装一遍。
    fn candidate_urls(&self, entry: &crate::model::PackageManifestEntry, store: &Store) -> Vec<String> {
        let mirror = store.get_setting("mirror").unwrap_or_else(|| "official".into());
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
                )),
            );
        }

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

        // 入口归位。解包产物的内部命名五花八门：顶层目录、版本化文件名
        // （mihomo-windows-amd64-v1.19.31.exe）、单文件 gz 解出的内名不可控。
        // entry 声明的路径不存在时按「精确文件名 → 包内唯一文件」容错定位并搬移。
        let entry_rel = entry_relative_path(&entry.entry);
        let entry_path = match settle_entry_file(&runtime_dir, &entry_rel) {
            Some(p) => p,
            None => {
                // 列出解压产物顶层内容，方便排障（清单与包内容不一致时一眼能看出差在哪）
                let listing = std::fs::read_dir(&runtime_dir)
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
                .with_hint("清单声明的入口与压缩包实际内容不一致；已保留解压产物，可反馈补清单")
                .with_detail(format!(
                    "期望路径：{}；解压目录内容：{}",
                    runtime_dir.join(&entry_rel).display(),
                    if listing.is_empty() { "（空）" } else { &listing }
                )));
            }
        };
        let _ = entry_path;

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

/// 解包后把主程序归位到 entry 声明的路径，三级容错：
///  1. 声明路径已存在 → 直接用；
///  2. 解压树里按文件名精确匹配（不分大小写，取目录最浅的）→ 搬到声明位置；
///  3. 整棵解压树只有一个文件时认定它是主程序（gz 内名/版本化单文件包）→ 搬移。
/// 都不命中返回 None（调用方报 ENTRY_MISSING）。
fn settle_entry_file(
    runtime_dir: &Path,
    entry_rel: &Path,
) -> Option<std::path::PathBuf> {
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
        let Ok(rd) = std::fs::read_dir(dir) else { return };
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
            let Ok(rd) = std::fs::read_dir(dir) else { return };
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
    let out = platform::command("tar")
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
    let out = platform::command("gzip")
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
        write(&d.join("mihomo-v1.19.31/mihomo-windows-amd64-v1.19.31.exe"), b"x");
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
        let other_os = if current_os() == "windows" { "macos" } else { "windows" };
        let e = entry_with(vec![other_os], vec![current_arch()]);
        assert!(!Installer::is_platform_compatible(&e));

        // 仅其它架构 → 不兼容（arm64-only 包装上 x64 必须被拦下）
        let other_arch = if current_arch() == "x64" { "arm64" } else { "x64" };
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
        let mut base = Installer { manifest: manifest_with("foo", "1.0.0", "https://a/1") };
        base.merge_manifest(manifest_with("foo", "1.0.0", "https://b/1"));
        assert_eq!(base.manifest.packages.len(), 1);
        assert_eq!(base.manifest.packages[0].url, "https://b/1", "同版本应被覆盖");

        base.merge_manifest(manifest_with("bar", "2.0.0", "https://c/2"));
        assert_eq!(base.manifest.packages.len(), 2);
    }
}
