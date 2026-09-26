//! 应用数据目录布局：
//! {base}/runtimes/{id}/{version}/   运行时（解压产物）
//! {base}/etc/{id}/{version}/        每版本配置
//! {base}/data/{id}/                 服务数据（MySQL datadir、Redis RDB）
//! {base}/logs/{service}/out.log     服务日志
//! {base}/certs/                     CA 与站点证书
//! {base}/downloads/                 下载缓存（断点续传）
//! {base}/backup/                    配置修改前的自动备份
//! {base}/nsb.sqlite                 站点/套件/证书/设置

use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct Paths {
    pub base: PathBuf,
}

impl Paths {
    /// 数据目录解析优先级：
    /// 1. 显式 base（冒烟测试等）
    /// 2. NSB_HOME 环境变量（便携化/调试覆盖）
    /// 3. 安装版：{exe 所在目录}/nsb-data —— 数据跟随安装位置，可整目录迁移
    /// 4. 开发环境（cargo target 下运行）：LocalAppData，避免 cargo clean 清掉数据
    /// 相对路径统一锚定到当前目录（子进程 cwd 各异，绝不能把相对路径写进配置/参数）
    pub fn resolve(base: Option<PathBuf>) -> PathBuf {
        let absolutize = |p: PathBuf| {
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().unwrap_or_default().join(p)
            }
        };
        if let Some(p) = base {
            return absolutize(p);
        }
        if let Ok(env) = std::env::var("NSB_HOME") {
            if !env.trim().is_empty() {
                return absolutize(PathBuf::from(env));
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let s = dir.to_string_lossy().to_lowercase().replace('\\', "/");
                let is_dev = s.contains("/target/debug") || s.contains("/target/release");
                if !is_dev {
                    let candidate = absolutize(dir.join("nsb-data"));
                    // 写权限探测（用户可能装到 Program Files 等只读位置）
                    if std::fs::create_dir_all(&candidate).is_ok() {
                        let probe = candidate.join(".write-probe");
                        if std::fs::write(&probe, b"ok").is_ok() {
                            let _ = std::fs::remove_file(&probe);
                            return candidate;
                        }
                    }
                }
            }
        }
        // 产品改名（NiceServBay → NiceEnv）：把旧数据目录整体迁过来，
        // 设置/已装套件/站点注册全都无缝带走；迁移失败（如被占用）就沿用旧目录
        let data_local = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
        let base = data_local.join("NiceEnv");
        if !base.exists() {
            // "niceEnv" 是改名中途短暂的拼写，一并兼容
            for legacy_name in ["NiceServBay", "niceEnv"] {
                let legacy = data_local.join(legacy_name);
                if legacy.exists() {
                    if std::fs::rename(&legacy, &base).is_ok() {
                        break;
                    }
                    return legacy; // 迁移失败（如被占用）：沿用旧目录，保证还能读到数据
                }
            }
        }
        base
    }

    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for d in [
            self.base.clone(),
            self.runtimes(),
            self.etc(),
            self.data(),
            self.logs(),
            self.certs(),
            self.downloads(),
            self.backup(),
            self.etc().join("nginx").join("sites"),
            self.etc().join("nginx").join("rewrites"),
            self.etc().join("nginx").join("temp"),
            self.etc().join("php"),
            self.etc().join("mysql"),
            self.etc().join("redis"),
            self.etc().join("mihomo"),
            self.certs().join("sites"),
            self.etc().join("apache").join("sites"),
            self.etc().join("apache").join("run"),
            self.etc().join("apache").join("logs"),
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    pub fn runtimes(&self) -> PathBuf {
        self.base.join("runtimes")
    }
    pub fn etc(&self) -> PathBuf {
        self.base.join("etc")
    }
    pub fn data(&self) -> PathBuf {
        self.base.join("data")
    }
    pub fn logs(&self) -> PathBuf {
        self.base.join("logs")
    }
    pub fn certs(&self) -> PathBuf {
        self.base.join("certs")
    }
    pub fn downloads(&self) -> PathBuf {
        self.base.join("downloads")
    }
    pub fn backup(&self) -> PathBuf {
        self.base.join("backup")
    }
    pub fn db(&self) -> PathBuf {
        self.base.join("nsb.sqlite")
    }

    pub fn runtime_dir(&self, id: &str, version: &str) -> PathBuf {
        self.runtimes().join(id).join(version)
    }
    pub fn etc_dir(&self, id: &str, version: &str) -> PathBuf {
        self.etc().join(id).join(version)
    }

    pub fn nginx_conf(&self) -> PathBuf {
        self.etc().join("nginx").join("nginx.conf")
    }
    pub fn nginx_sites_dir(&self) -> PathBuf {
        self.etc().join("nginx").join("sites")
    }
    pub fn php_ini(&self, version: &str) -> PathBuf {
        self.etc().join("php").join(version).join("php.ini")
    }
    pub fn mysql_ini(&self, version: &str) -> PathBuf {
        self.etc().join("mysql").join(version).join("my.ini")
    }
    pub fn mysql_data_dir(&self, version: &str) -> PathBuf {
        self.data().join("mysql").join(version)
    }
    pub fn redis_conf(&self, version: &str) -> PathBuf {
        self.etc().join("redis").join(version).join("redis.conf")
    }
    pub fn redis_data_dir(&self) -> PathBuf {
        self.data().join("redis")
    }
    pub fn mihomo_dir(&self) -> PathBuf {
        self.etc().join("mihomo")
    }
    pub fn mihomo_config(&self) -> PathBuf {
        self.mihomo_dir().join("config.yaml")
    }
    /* ---------- Apache ---------- */
    pub fn apache_conf(&self) -> PathBuf {
        self.etc().join("apache").join("httpd.conf")
    }
    pub fn apache_sites_dir(&self) -> PathBuf {
        self.etc().join("apache").join("sites")
    }
    pub fn apache_run_dir(&self) -> PathBuf {
        self.etc().join("apache").join("run")
    }
    /* ---------- PostgreSQL / MongoDB（按版本隔离：跨大版本数据文件不兼容） ---------- */
    pub fn postgres_data_dir(&self, version: &str) -> PathBuf {
        self.data().join("postgresql").join(version)
    }
    pub fn mongo_data_dir(&self, version: &str) -> PathBuf {
        self.data().join("mongodb").join(version)
    }
    pub fn service_log(&self, service_id: &str) -> PathBuf {
        self.logs()
            .join(service_id.replace(['@', ':'], "_"))
            .join("out.log")
    }
}

/// 数据目录迁移结果。迁移成功后桌面端会重启应用，让新进程从目标目录打开数据库和运行时。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataDirMigration {
    pub path: String,
    pub files: u64,
    pub bytes: u64,
}

/// 将整个 NiceEnv 数据目录复制到一个新的空目录。
///
/// 复制使用同父目录的临时目录，完成后再一次性改名，避免目标目录只复制了一半就被下次启动
/// 选中。拒绝把目标放进源目录，也拒绝跟随软链接/目录联接，避免迁移时越出用户明确选择的范围。
pub fn copy_data_dir(
    source: &Path,
    requested_target: &Path,
) -> crate::error::Result<DataDirMigration> {
    if let Ok(metadata) = std::fs::symlink_metadata(source) {
        if linked(&metadata) {
            return Err(crate::error::AppError::new(
                "DATA_DIR_INVALID",
                "当前数据目录是软链接或目录联接，无法安全迁移",
            ));
        }
    }
    let source = std::fs::canonicalize(source)
        .map_err(|e| crate::error::AppError::io("读取当前数据目录", e))?;
    if !source.is_dir() {
        return Err(crate::error::AppError::new(
            "DATA_DIR_INVALID",
            "当前数据目录不是文件夹",
        ));
    }

    let requested_target = if requested_target.is_absolute() {
        requested_target.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| crate::error::AppError::io("解析目标数据目录", e))?
            .join(requested_target)
    };
    let name = requested_target
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| crate::error::AppError::new("DATA_DIR_INVALID", "请选择一个具体的数据目录"))?;
    let parent = requested_target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| crate::error::AppError::new("DATA_DIR_INVALID", "目标数据目录路径无效"))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| crate::error::AppError::io("创建目标数据目录的父目录", e))?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|e| crate::error::AppError::io("解析目标数据目录的父目录", e))?;
    let target = parent.join(name);

    if let Ok(metadata) = std::fs::symlink_metadata(&target) {
        if linked(&metadata) {
            return Err(crate::error::AppError::new(
                "DATA_DIR_INVALID",
                "目标目录不能是软链接或目录联接",
            ));
        }
    }

    let source_text = source.to_string_lossy();
    let target_text = target.to_string_lossy();
    let same_path = if cfg!(windows) {
        source_text.eq_ignore_ascii_case(&target_text)
    } else {
        source_text == target_text
    };
    if same_path || target.starts_with(&source) {
        return Err(crate::error::AppError::new(
            "DATA_DIR_INVALID",
            "目标目录不能与当前目录相同，也不能放在当前目录里面",
        ));
    }
    if target.exists() {
        if !target.is_dir() {
            return Err(crate::error::AppError::new(
                "DATA_DIR_NOT_EMPTY",
                "目标路径已经是一个文件",
            ));
        }
        let mut entries = std::fs::read_dir(&target)
            .map_err(|e| crate::error::AppError::io("检查目标数据目录", e))?;
        let has_entry = entries
            .next()
            .transpose()
            .map_err(|e| crate::error::AppError::io("检查目标数据目录", e))?
            .is_some();
        if has_entry {
            return Err(crate::error::AppError::new(
                "DATA_DIR_NOT_EMPTY",
                "目标数据目录必须为空",
            ));
        }
    }

    let staging = parent.join(format!(
        ".{}-migrating-{}",
        name.to_string_lossy(),
        crate::services::now_ms()
    ));
    if staging.exists() {
        return Err(crate::error::AppError::new(
            "DATA_DIR_BUSY",
            "目标目录正在进行另一次迁移，请稍后重试",
        ));
    }
    std::fs::create_dir(&staging)
        .map_err(|e| crate::error::AppError::io("创建迁移暂存目录", e))?;

    let mut stats = (0_u64, 0_u64);
    let copy_result = copy_tree(&source, &staging, &mut stats);
    if let Err(error) = copy_result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(crate::error::AppError::io("复制数据目录", error));
    }
    if let Err(error) = validate_migrated_root(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    if target.exists() {
        if let Err(error) = std::fs::remove_dir(&target) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(crate::error::AppError::io("替换空目标数据目录", error));
        }
    }
    if let Err(error) = std::fs::rename(&staging, &target) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(crate::error::AppError::io("提交新的数据目录", error));
    }
    Ok(DataDirMigration {
        path: target.to_string_lossy().to_string(),
        files: stats.0,
        bytes: stats.1,
    })
}

fn copy_tree(source: &Path, target: &Path, stats: &mut (u64, u64)) -> io::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&from)?;
        if linked(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("数据目录包含不支持迁移的链接：{}", from.display()),
            ));
        }
        if metadata.is_dir() {
            std::fs::create_dir(&to)?;
            copy_tree(&from, &to, stats)?;
        } else if metadata.is_file() {
            std::fs::copy(&from, &to)?;
            stats.0 = stats.0.saturating_add(1);
            stats.1 = stats.1.saturating_add(metadata.len());
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("数据目录包含不支持迁移的文件：{}", from.display()),
            ));
        }
    }
    Ok(())
}

fn validate_migrated_root(root: &Path) -> crate::error::Result<()> {
    if !root.join("nsb.sqlite").is_file() {
        return Err(crate::error::AppError::new(
            "DATA_DIR_INVALID",
            "迁移源缺少 NiceEnv 数据库，未切换目录",
        ));
    }
    Ok(())
}

/// 写文件前把旧内容备份到 {base}/backup/
pub fn write_with_backup(path: &Path, content: &str, backup_dir: &Path) -> std::io::Result<()> {
    write_with_backup_expected(path, content, backup_dir, None)
}

fn write_with_backup_expected(
    path: &Path,
    content: &str,
    backup_dir: &Path,
    expected: Option<Option<&[u8]>>,
) -> io::Result<()> {
    let base = backup_dir
        .parent()
        .ok_or_else(|| backup_error("备份目录无效"))?;
    let relative = path
        .strip_prefix(base)
        .map_err(|_| backup_error("配置不在应用数据目录内"))?;
    let relative = nginx_path(relative);
    let target = checked_data_path(base, &relative)?;
    let previous = read_optional(&target)?;
    if expected.is_some_and(|bytes| previous.as_deref() != bytes) {
        return Err(backup_error("配置已变化，请重新预览后恢复"));
    }
    if previous.as_deref() == Some(content.as_bytes()) {
        return Ok(());
    }
    let parent = target
        .parent()
        .ok_or_else(|| backup_error("配置目录无效"))?;
    std::fs::create_dir_all(parent)?;
    let mut pending = tempfile::NamedTempFile::new_in(parent)?;
    pending.write_all(content.as_bytes())?;
    pending.as_file().sync_all()?;
    if let Ok(metadata) = std::fs::metadata(&target) {
        pending.as_file().set_permissions(metadata.permissions())?;
    }
    if let Some(previous) = &previous {
        let directory = checked_data_path(base, "backup/files")?;
        std::fs::create_dir_all(&directory)?;
        let pending_backup = tempfile::Builder::new()
            .prefix(".pending-")
            .tempdir_in(&directory)?;
        let payload = pending_backup.path().join("content.bak");
        let mut file = std::fs::File::create(&payload)?;
        file.write_all(&previous)?;
        file.sync_all()?;
        drop(file);
        let metadata = BackupMetadata {
            version: 1,
            target: relative.clone(),
            sha256: hex::encode(Sha256::digest(previous)),
        };
        let mut file = std::fs::File::create(pending_backup.path().join("metadata.json"))?;
        file.write_all(&serde_json::to_vec(&metadata).map_err(io::Error::other)?)?;
        file.sync_all()?;
        drop(file);
        let suffix = pending_backup
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".pending-", "");
        let name = format!(
            "cfg-{}-{suffix}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        std::fs::rename(pending_backup.path(), directory.join(name))?;
    }
    checked_data_path(base, &relative)?;
    if read_optional(&target)? != previous {
        return Err(backup_error(
            "配置在写入前已变化，未覆盖当前文件，请重新预览",
        ));
    }
    pending.persist(&target).map_err(|e| e.error)?;
    Ok(())
}

/// Windows 路径转 nginx 正斜杠形式
pub fn nginx_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// 列出备份目录里的备份文件（新→旧）。返回 (文件名, 完整路径, 字节数, 修改时间 ms)
pub fn list_backups(base: &Path) -> Vec<(String, String, u64, i64)> {
    list_backup_files(base)
        .unwrap_or_default()
        .into_iter()
        .map(|file| (file.name, file.path, file.size_bytes, file.modified_at))
        .collect()
}

/// 从已记录准确目标的备份恢复；兼容目标唯一且无版本歧义的旧备份。
pub fn restore_backup(base: &Path, backup_name: &str) -> std::io::Result<PathBuf> {
    restore_backup_checked(base, backup_name, None)
}

fn backup_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

/// 相对路径必须由普通组件构成；拒绝盘符、ADS、尾点、软链接和目录联接。
pub(crate) fn checked_data_path(base: &Path, relative: &str) -> io::Result<PathBuf> {
    if relative.is_empty() || relative.contains(['\\', ':', '\0', '<', '>', '"', '|', '?', '*']) {
        return Err(backup_error("配置路径无效"));
    }
    let mut result = base.to_path_buf();
    for part in relative.split('/') {
        let device = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let numbered_device = device
            .strip_prefix("COM")
            .or_else(|| device.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(
                    suffix,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            });
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part.chars().any(|c| c.is_control())
            || ["CON", "PRN", "AUX", "NUL"].contains(&device.as_str())
            || numbered_device
            || Path::new(part)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(backup_error("配置路径无效"));
        }
        result.push(part);
        match std::fs::symlink_metadata(&result) {
            Ok(metadata) if linked(&metadata) => {
                return Err(backup_error("配置路径包含软链接或目录联接，无法自动恢复"))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Serialize, Deserialize)]
struct BackupMetadata {
    version: u8,
    target: String,
    sha256: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFile {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub modified_at: i64,
    pub target_path: Option<String>,
    pub restorable: bool,
    pub reason: Option<String>,
    #[serde(skip)]
    sort_time: u128,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPreview {
    pub name: String,
    pub target_path: String,
    pub target_relative: String,
    pub current_exists: bool,
    pub revision: String,
}

fn legacy_target(base: &Path, name: &str) -> io::Result<String> {
    let original = name
        .strip_suffix(".bak")
        .and_then(|s| s.rsplit_once('.').map(|(name, _)| name))
        .ok_or_else(|| backup_error("旧备份文件名无法解析"))?;
    if ["php.ini", "my.ini", "redis.conf"].contains(&original) {
        return Err(backup_error(
            "旧备份没有记录所属版本，请打开备份目录确认，不能自动恢复",
        ));
    }
    let etc = checked_data_path(base, "etc")?;
    let mut stack = vec![etc];
    let mut found = Vec::new();
    while let Some(directory) = stack.pop() {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if linked(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() && entry.file_name().to_string_lossy() == original {
                found.push(nginx_path(
                    entry
                        .path()
                        .strip_prefix(base)
                        .map_err(|_| backup_error("配置路径无效"))?,
                ));
            }
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(backup_error(
            "找不到旧备份对应的配置文件，请打开备份目录确认",
        )),
        _ => Err(backup_error(
            "存在多个同名配置，旧备份无法确定目标，不能自动恢复",
        )),
    }
}

fn backup_source(base: &Path, name: &str) -> io::Result<(PathBuf, String, Option<String>)> {
    let parts: Vec<_> = name.split('/').collect();
    if parts.len() == 2 && parts[0] == "files" && parts[1].starts_with("cfg-") {
        let directory = checked_data_path(base, &format!("backup/{name}"))?;
        let meta = checked_data_path(base, &format!("backup/{name}/metadata.json"))?;
        if std::fs::metadata(&meta)?.len() > 65536 {
            return Err(backup_error("备份元信息过大"));
        }
        let metadata: BackupMetadata =
            serde_json::from_slice(&std::fs::read(meta)?).map_err(io::Error::other)?;
        if metadata.version != 1 {
            return Err(backup_error("暂不支持此备份格式"));
        }
        checked_data_path(base, &metadata.target)?;
        let file = directory.join("content.bak");
        checked_data_path(base, &format!("backup/{name}/content.bak"))?;
        return Ok((file, metadata.target, Some(metadata.sha256)));
    }
    if parts.len() == 2 && parts[0] == "config" && parts[1].ends_with(".bak") {
        let source = checked_data_path(base, &format!("backup/{name}"))?;
        let target = crate::cfgeditor::backup_relative_target(parts[1])
            .ok_or_else(|| backup_error("旧备份没有准确的版本信息，不能自动恢复"))?;
        return Ok((source, target, None));
    }
    if parts.len() == 1 && name.ends_with(".bak") {
        let source = checked_data_path(base, &format!("backup/{name}"))?;
        return Ok((source, legacy_target(base, name)?, None));
    }
    Err(backup_error("备份标识无效"))
}

pub fn list_backup_files(base: &Path) -> io::Result<Vec<BackupFile>> {
    let mut output = Vec::new();
    for area in ["", "config", "files"] {
        let directory = checked_data_path(
            base,
            &if area.is_empty() {
                "backup".into()
            } else {
                format!("backup/{area}")
            },
        )?;
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if linked(&metadata) {
                continue;
            }
            let filename = entry.file_name().to_string_lossy().to_string();
            if area == "files" {
                if !metadata.is_dir() || !filename.starts_with("cfg-") {
                    continue;
                }
            } else if !metadata.is_file() || !filename.ends_with(".bak") {
                continue;
            }
            let name = if area.is_empty() {
                filename
            } else {
                format!("{area}/{filename}")
            };
            let path = if area == "files" {
                entry.path().join("content.bak")
            } else {
                entry.path()
            };
            if std::fs::symlink_metadata(&path).is_ok_and(|meta| linked(&meta)) {
                continue;
            }
            let (metadata, payload_error) = match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => (meta, None),
                Ok(_) => (metadata, Some("备份内容不是普通文件".to_string())),
                Err(error) => (metadata, Some(format!("无法读取备份内容：{error}"))),
            };
            let (target_path, reason) = match backup_source(base, &name) {
                Ok((_, relative, _)) if relative.starts_with("etc/") => {
                    match checked_data_path(base, &relative) {
                        Ok(_) => (Some(relative), None),
                        Err(error) => (Some(relative), Some(error.to_string())),
                    }
                }
                Ok((_, relative, _)) => (
                    Some(relative),
                    Some("此备份不是服务配置，请在对应功能页处理".into()),
                ),
                Err(error) => (None, Some(error.to_string())),
            };
            let size_bytes = if payload_error.is_some() {
                0
            } else {
                metadata.len()
            };
            let reason = payload_error.or(reason);
            output.push(BackupFile {
                name,
                path: path.to_string_lossy().into(),
                size_bytes,
                modified_at: metadata
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64,
                target_path,
                restorable: reason.is_none(),
                reason,
                sort_time: metadata
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
            });
        }
    }
    output.sort_by(|a, b| {
        b.sort_time
            .cmp(&a.sort_time)
            .then_with(|| b.name.cmp(&a.name))
    });
    Ok(output)
}

fn read_backup_snapshot(
    base: &Path,
    name: &str,
) -> io::Result<(BackupPreview, Vec<u8>, Option<Vec<u8>>)> {
    let (source, relative, expected_hash) = backup_source(base, name)?;
    if !relative.starts_with("etc/") {
        return Err(backup_error("只能从此入口恢复服务配置"));
    }
    let target = checked_data_path(base, &relative)?;
    let content = std::fs::read(source)?;
    let digest = hex::encode(Sha256::digest(&content));
    if expected_hash.is_some_and(|hash| hash != digest) {
        return Err(backup_error("备份内容校验失败，原配置未改动"));
    }
    let current = read_optional(&target)?;
    let revision = hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            name,
            &relative,
            &digest,
            current.as_ref().map(|v| hex::encode(Sha256::digest(v))),
        ))
        .map_err(io::Error::other)?,
    ));
    Ok((
        BackupPreview {
            name: name.into(),
            target_path: target.to_string_lossy().into(),
            target_relative: relative,
            current_exists: current.is_some(),
            revision,
        },
        content,
        current,
    ))
}

pub fn preview_backup(base: &Path, name: &str) -> io::Result<BackupPreview> {
    read_backup_snapshot(base, name).map(|(preview, _, _)| preview)
}

pub fn restore_backup_checked(
    base: &Path,
    name: &str,
    expected_revision: Option<&str>,
) -> io::Result<PathBuf> {
    let (preview, content, current) = read_backup_snapshot(base, name)?;
    if expected_revision.is_some_and(|revision| preview.revision != revision) {
        return Err(backup_error("配置或备份已变化，请重新预览后恢复"));
    }
    let content = String::from_utf8(content).map_err(io::Error::other)?;
    let target = checked_data_path(base, &preview.target_relative)?;
    write_with_backup_expected(
        &target,
        &content,
        &base.join("backup"),
        Some(current.as_deref()),
    )?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Paths) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("app data"));
        paths.ensure_dirs().unwrap();
        (temp, paths)
    }

    #[test]
    fn data_dir_copy_is_atomic_and_requires_an_empty_target() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let paths = Paths::new(source.clone());
        paths.ensure_dirs().unwrap();
        std::fs::write(paths.db(), b"sqlite-fixture").unwrap();
        std::fs::write(paths.etc().join("marker.ini"), b"preserve me").unwrap();
        let target = temp.path().join("target");

        let result = copy_data_dir(&source, &target).unwrap();
        assert_eq!(result.files, 2);
        assert_eq!(std::fs::read(target.join("nsb.sqlite")).unwrap(), b"sqlite-fixture");
        assert_eq!(std::fs::read(target.join("etc/marker.ini")).unwrap(), b"preserve me");

        std::fs::write(target.join("keep.txt"), b"do not overwrite").unwrap();
        let error = copy_data_dir(&source, &target).unwrap_err();
        assert_eq!(error.code, "DATA_DIR_NOT_EMPTY");
        assert_eq!(std::fs::read(target.join("keep.txt")).unwrap(), b"do not overwrite");
    }

    #[test]
    fn exact_targets_survive_missing_files_and_rapid_writes_keep_every_backup() {
        let (_temp, paths) = fixture();
        let target = paths.php_ini("8.2.0");
        write_with_backup(&target, "original", &paths.backup()).unwrap();
        write_with_backup(&paths.php_ini("8.4.0"), "other version", &paths.backup()).unwrap();
        for value in 0..8 {
            write_with_backup(&target, &value.to_string(), &paths.backup()).unwrap();
        }
        write_with_backup(&target, "7", &paths.backup()).unwrap();
        let history = list_backup_files(&paths.base).unwrap();
        assert_eq!(history.len(), 8);
        let names: std::collections::HashSet<_> = history.iter().map(|b| &b.name).collect();
        assert_eq!(names.len(), 8);
        assert!(history
            .iter()
            .all(|b| b.target_path.as_deref() == Some("etc/php/8.2.0/php.ini") && b.restorable));
        let original = history
            .iter()
            .find(|b| std::fs::read_to_string(&b.path).unwrap() == "original")
            .unwrap();
        std::fs::remove_file(&target).unwrap();
        let preview = preview_backup(&paths.base, &original.name).unwrap();
        assert!(!preview.current_exists);
        assert_eq!(
            restore_backup_checked(&paths.base, &original.name, Some(&preview.revision)).unwrap(),
            target
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
        assert_eq!(
            std::fs::read_to_string(paths.php_ini("8.4.0")).unwrap(),
            "other version"
        );
    }

    #[test]
    fn restore_conflicts_tampering_and_backup_failures_preserve_current_content() {
        let (_temp, paths) = fixture();
        let target = paths.nginx_conf();
        write_with_backup(&target, "v1", &paths.backup()).unwrap();
        write_with_backup(&target, "v2", &paths.backup()).unwrap();
        let backup = list_backup_files(&paths.base).unwrap().remove(0);
        let preview = preview_backup(&paths.base, &backup.name).unwrap();
        std::fs::write(&target, "external").unwrap();
        assert!(
            restore_backup_checked(&paths.base, &backup.name, Some(&preview.revision)).is_err()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "external");
        std::fs::write(&backup.path, "tampered").unwrap();
        assert!(preview_backup(&paths.base, &backup.name).is_err());
        assert!(restore_backup(&paths.base, &backup.name).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "external");

        let (_temp2, other) = fixture();
        write_with_backup(&other.nginx_conf(), "keep", &other.backup()).unwrap();
        std::fs::write(other.backup().join("files"), "blocked directory").unwrap();
        assert!(write_with_backup(&other.nginx_conf(), "lost", &other.backup()).is_err());
        assert_eq!(std::fs::read_to_string(other.nginx_conf()).unwrap(), "keep");
        assert!(list_backup_files(&other.base).is_err());
    }

    #[test]
    fn malformed_bundles_are_disabled_without_hiding_valid_history() {
        let (_temp, paths) = fixture();
        write_with_backup(&paths.nginx_conf(), "v1", &paths.backup()).unwrap();
        write_with_backup(&paths.nginx_conf(), "v2", &paths.backup()).unwrap();
        std::fs::create_dir_all(paths.backup().join("files/cfg-incomplete")).unwrap();
        std::fs::create_dir_all(paths.backup().join("files/.pending-unpublished")).unwrap();
        let history = list_backup_files(&paths.base).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history
            .iter()
            .any(|b| b.name == "files/cfg-incomplete" && !b.restorable && b.reason.is_some()));
        assert_eq!(history.iter().filter(|b| b.restorable).count(), 1);
    }

    #[test]
    fn legacy_ambiguity_non_config_backups_and_unsafe_paths_cannot_be_restored() {
        let (_temp, paths) = fixture();
        write_with_backup(&paths.php_ini("8.4.0"), "keep", &paths.backup()).unwrap();
        std::fs::write(paths.backup().join("php.ini.20260101.bak"), "old").unwrap();
        write_with_backup(&paths.nginx_conf(), "nginx", &paths.backup()).unwrap();
        std::fs::write(paths.etc().join("nginx.conf"), "duplicate").unwrap();
        std::fs::write(paths.backup().join("nginx.conf.20260101.bak"), "old").unwrap();
        write_with_backup(&paths.certs().join("secret.key"), "key1", &paths.backup()).unwrap();
        write_with_backup(&paths.certs().join("secret.key"), "key2", &paths.backup()).unwrap();
        for backup in list_backup_files(&paths.base).unwrap() {
            assert!(!backup.restorable, "{}", backup.name);
            assert!(restore_backup(&paths.base, &backup.name).is_err());
        }
        for path in [
            "../escape",
            "etc/../escape",
            "etc//file",
            "etc/./file",
            "/etc/file",
            "C:/file",
            "etc/a:stream",
            "etc/a.",
            "etc/a ",
            "etc/NUL.ini",
            "etc/COM1/x",
            "etc/LPT²/x",
            "etc/a?b",
            "etc\\file",
        ] {
            assert!(checked_data_path(&paths.base, path).is_err(), "{path}");
        }
        for name in [
            "../secret.bak",
            "config/../../secret.bak",
            "files/cfg-a/../b",
            "C:/file.bak",
        ] {
            assert!(restore_backup(&paths.base, name).is_err(), "{name}");
        }
    }

    #[test]
    fn altered_metadata_cannot_write_outside_the_config_tree() {
        let (_temp, paths) = fixture();
        write_with_backup(&paths.nginx_conf(), "v1", &paths.backup()).unwrap();
        write_with_backup(&paths.nginx_conf(), "v2", &paths.backup()).unwrap();
        let backup = list_backup_files(&paths.base).unwrap().remove(0);
        let metadata = paths.backup().join(&backup.name).join("metadata.json");
        for target in [
            "../escape",
            "etc/../secret",
            "etc/file:stream",
            "certs/root.key",
        ] {
            std::fs::write(
                &metadata,
                serde_json::to_vec(&BackupMetadata {
                    version: 1,
                    target: target.into(),
                    sha256: hex::encode(Sha256::digest(b"v1")),
                })
                .unwrap(),
            )
            .unwrap();
            assert!(
                restore_backup(&paths.base, &backup.name).is_err(),
                "{target}"
            );
        }
        assert_eq!(std::fs::read_to_string(paths.nginx_conf()).unwrap(), "v2");
        assert!(!paths.base.parent().unwrap().join("escape").exists());
    }

    #[test]
    fn directory_links_cannot_redirect_restores_or_backup_writes() {
        let (temp, paths) = fixture();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("file"), "keep outside").unwrap();
        let link = paths.etc().join("linked");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let result = std::process::Command::new("cmd.exe")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&outside)
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(checked_data_path(&paths.base, "etc/linked/file").is_err());
        assert!(write_with_backup(&link.join("file"), "lost", &paths.backup()).is_err());
        std::fs::write(paths.backup().join("file.20260101.bak"), "lost").unwrap();
        assert!(restore_backup(&paths.base, "file.20260101.bak").is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("file")).unwrap(),
            "keep outside"
        );
        #[cfg(windows)]
        std::fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&link).unwrap();
    }
}
