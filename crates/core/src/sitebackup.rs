//! 站点文件归档：配置与数据库备份独立；恢复始终创建新目录。
use crate::{
    error::{AppError, Result},
    model::Site,
    paths::Paths,
    store::Store,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const FORMAT: &str = "niceenv/site-files-v1";
const MANIFEST: &str = "niceenv-site-backup.json";
const MAX_ENTRIES: usize = 100_000;
const MAX_BYTES: u64 = 50 * 1024 * 1024 * 1024;
const MAX_MANIFEST: u64 = 32 * 1024 * 1024;
const MAX_ARCHIVE: u64 = MAX_BYTES + 1024 * 1024 * 1024;
const EXCLUDED: &[&str] = &[
    ".git",
    "node_modules",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    "__pycache__",
    "target",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub root: String,
    pub revision: String,
    pub excluded: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub name: String,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub path: String,
    pub size_bytes: u64,
    pub created_at: i64,
    pub files: u64,
    pub original_bytes: u64,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub root: String,
    pub excluded: Vec<String>,
    pub restorable: bool,
    pub error: Option<String>,
    #[serde(default)]
    pub automatic: bool,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub operation_id: String,
    pub site_id: String,
    pub phase: String,
    pub files: u64,
    pub bytes: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub source_path: String,
    pub source_site_id: String,
    pub target_name: String,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub target_root: String,
    pub archive: BackupInfo,
    pub revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreEntry {
    pub path: String,
    pub directory: bool,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePreview {
    pub archive: BackupInfo,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub parent: String,
    pub revision: String,
    pub sha256: String,
    pub verified_at: i64,
    pub entries: Vec<RestoreEntry>,
    pub total_entries: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    path: String,
    directory: bool,
    size: u64,
    sha256: String,
    mode: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    format: String,
    site_id: String,
    root: String,
    created_at: i64,
    excluded: Vec<String>,
    entries: Vec<Entry>,
    #[serde(default)]
    automatic: bool,
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new("SITE_BACKUP_INVALID", message)
}
fn archive_error(error: zip::result::ZipError) -> AppError {
    invalid(format!("无法读取或写入站点归档：{error}"))
}
fn within(path: &Path, root: &Path) -> bool {
    #[cfg(windows)]
    {
        return Path::new(&path.to_string_lossy().to_lowercase())
            .starts_with(root.to_string_lossy().to_lowercase());
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}
fn linked(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}
fn plain_directory(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(invalid("目录必须是绝对路径"));
    }
    for part in path.ancestors() {
        if linked(&fs::symlink_metadata(part)?) {
            return Err(invalid("目录不能经过符号链接或目录联接"));
        }
    }
    let actual = path.canonicalize()?;
    if !actual.is_dir() {
        return Err(invalid("所选路径不是目录"));
    }
    Ok(actual)
}
fn open_plain(path: &Path) -> Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if linked(&metadata) || !metadata.is_file() {
        return Err(invalid(format!("仅支持普通文件：{}", path.display())));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x00200000);
    }
    let file = options.open(path)?;
    let actual = file.metadata()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let current = fs::symlink_metadata(path)?;
        if linked(&current)
            || actual.dev() != metadata.dev()
            || actual.ino() != metadata.ino()
            || current.dev() != actual.dev()
            || current.ino() != actual.ino()
        {
            return Err(invalid("文件在打开时被替换，请停止修改后重试"));
        }
    }
    if linked(&actual)
        || !actual.is_file()
        || actual.len() != metadata.len()
        || actual.modified()? != metadata.modified()?
    {
        return Err(invalid("文件在打开时发生变化，请停止修改后重试"));
    }
    Ok(file)
}
fn site(store: &Store, id: &str) -> Result<Site> {
    store
        .list_sites()?
        .into_iter()
        .find(|site| site.id == id)
        .ok_or_else(|| AppError::new("SITE_NOT_FOUND", "站点已不存在，请刷新列表"))
}
fn directory(paths: &Paths, id: &str) -> Result<PathBuf> {
    Ok(crate::paths::checked_data_path(
        &paths.base,
        &format!(
            "backup/sites/{}",
            hex::encode(Sha256::digest(id.as_bytes()))
        ),
    )?)
}
fn lock(paths: &Paths, id: &str) -> Result<File> {
    let dir = directory(paths, id)?;
    fs::create_dir_all(&dir)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(crate::paths::checked_data_path(&dir, ".files.lock")?)?;
    file.try_lock().map_err(|_| {
        AppError::new(
            "SITE_BACKUP_BUSY",
            "此站点正在备份、恢复或删除归档，请稍后再试",
        )
    })?;
    Ok(file)
}
fn archive_path(paths: &Paths, id: &str, name: &str) -> Result<PathBuf> {
    if name.len() > 120
        || !name.starts_with("site-")
        || !name.ends_with(".zip")
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c))
    {
        return Err(invalid("备份文件名无效"));
    }
    Ok(crate::paths::checked_data_path(
        &directory(paths, id)?,
        name,
    )?)
}
pub fn scope(store: &Store, id: &str, project: bool, exclude_generated: bool) -> Result<Scope> {
    let site = site(store, id)?;
    if site.runtime.kind == crate::model::SiteKind::Redirect {
        return Err(AppError::new("SITE_NO_PROJECT", "跳转站点没有需要备份的项目目录，请使用配置备份"));
    }
    let web = Path::new(&site.root_dir);
    let root = if project {
        site.runtime
            .application
            .as_ref()
            .and_then(|app| app.cwd.as_deref())
            .filter(|cwd| !cwd.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::envfile::project_root(web))
    } else {
        web.to_path_buf()
    };
    let root = plain_directory(&root)?;
    let excluded = if exclude_generated {
        EXCLUDED.iter().map(|name| name.to_string()).collect()
    } else {
        Vec::new()
    };
    let revision = hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            id,
            &site.root_dir,
            site.updated_at,
            &root,
            project,
            &excluded,
        ))
        .map_err(|e| invalid(e.to_string()))?,
    ));
    Ok(Scope {
        root: root.to_string_lossy().into(),
        revision,
        excluded,
    })
}

#[derive(PartialEq)]
struct SourceEntry {
    path: String,
    directory: bool,
    size: u64,
    modified: std::time::SystemTime,
    mode: u32,
}
fn collect(root: &Path, excluded: &[String]) -> Result<Vec<SourceEntry>> {
    fn walk(
        root: &Path,
        relative: &str,
        excluded: &[String],
        entries: &mut Vec<SourceEntry>,
        bytes: &mut u64,
    ) -> Result<()> {
        if relative.split('/').count() > 128 {
            return Err(invalid("目录层级过深，无法备份"));
        }
        let dir = if relative.is_empty() {
            root.to_path_buf()
        } else {
            crate::paths::checked_data_path(root, relative)?
        };
        for item in fs::read_dir(dir)? {
            let item = item?;
            let name = item
                .file_name()
                .into_string()
                .map_err(|_| invalid("文件名不是 UTF-8，无法创建可移植归档"))?;
            let metadata = fs::symlink_metadata(item.path())?;
            if excluded.contains(&name) && (metadata.is_dir() || linked(&metadata)) {
                continue;
            }
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            if path.len() > 2048 || linked(&metadata) || (!metadata.is_file() && !metadata.is_dir())
            {
                return Err(invalid(format!("不支持的链接、特殊文件或过长路径：{path}")));
            }
            crate::paths::checked_data_path(root, &path)?;
            let size = if metadata.is_file() {
                metadata.len()
            } else {
                0
            };
            *bytes = bytes
                .checked_add(size)
                .ok_or_else(|| invalid("备份文件大小超限"))?;
            if *bytes > MAX_BYTES || entries.len() >= MAX_ENTRIES {
                return Err(invalid("单份备份最多支持 100,000 个条目及 50 GiB 原始文件"));
            }
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o777
            };
            #[cfg(not(unix))]
            let mode = if metadata.is_dir() {
                0o755
            } else if metadata.permissions().readonly() {
                0o444
            } else {
                0o644
            };
            entries.push(SourceEntry {
                path: path.clone(),
                directory: metadata.is_dir(),
                size,
                modified: metadata.modified()?,
                mode,
            });
            if metadata.is_dir() {
                walk(root, &path, excluded, entries, bytes)?;
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    walk(root, "", excluded, &mut entries, &mut 0)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut names = BTreeSet::new();
    if entries
        .iter()
        .any(|entry| !names.insert(entry.path.to_lowercase()))
    {
        return Err(invalid("源目录包含仅大小写不同的路径，无法生成可移植归档"));
    }
    Ok(entries)
}

pub fn create(
    state: &crate::CoreState,
    id: &str,
    project: bool,
    exclude_generated: bool,
    revision: &str,
    confirmed: bool,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<BackupInfo> {
    if !confirmed {
        return Err(invalid(
            "请确认已暂停文件修改，并了解归档可能包含环境变量和密钥",
        ));
    }
    let _work = crate::BackgroundWork::begin("备份站点文件")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = lock(&state.paths, id)?;
    create_locked(
        state,
        id,
        project,
        exclude_generated,
        revision,
        false,
        progress,
    )
}

fn create_locked(
    state: &crate::CoreState,
    id: &str,
    project: bool,
    exclude_generated: bool,
    revision: &str,
    automatic: bool,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<BackupInfo> {
    let scope = scope(&state.store, id, project, exclude_generated)?;
    if scope.revision != revision {
        return Err(AppError::new(
            "SITE_BACKUP_CHANGED",
            "站点目录或备份范围已变化，请重新检查",
        ));
    }
    let root = Path::new(&scope.root);
    let dir = plain_directory(&directory(&state.paths, id)?)?;
    if within(&dir, root) || within(root, &dir) {
        return Err(invalid("备份目录与源目录不能相互包含"));
    }
    progress("scan", 0, 0);
    let sources = collect(root, &scope.excluded)?;
    let mut pending = tempfile::Builder::new()
        .prefix(".pending-")
        .tempfile_in(&dir)?;
    let mut zip = zip::ZipWriter::new(pending.as_file_mut());
    let mut manifest = Manifest {
        format: FORMAT.into(),
        site_id: id.into(),
        root: scope.root.clone(),
        created_at: crate::services::now_ms(),
        excluded: scope.excluded.clone(),
        entries: Vec::new(),
        automatic,
    };
    let (mut files, mut bytes) = (0, 0);
    let mut buffer = vec![0; 256 * 1024];
    for entry in &sources {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true)
            .unix_permissions(entry.mode);
        let mut digest = Sha256::new();
        if entry.directory {
            zip.add_directory(format!("files/{}/", entry.path), options)
                .map_err(archive_error)?;
        } else {
            let path = crate::paths::checked_data_path(root, &entry.path)?;
            let mut input = open_plain(&path)?;
            zip.start_file(format!("files/{}", entry.path), options)
                .map_err(archive_error)?;
            let mut written = 0;
            loop {
                let n = input.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                written += n as u64;
                if written > entry.size {
                    return Err(invalid("源文件在备份期间增长，请停止修改后重试"));
                }
                zip.write_all(&buffer[..n])?;
                digest.update(&buffer[..n]);
                bytes += n as u64;
                progress("backup", files, bytes);
            }
            if written != entry.size || input.metadata()?.modified()? != entry.modified {
                return Err(invalid("源文件在备份期间变化，请停止修改后重试"));
            }
            files += 1;
        }
        manifest.entries.push(Entry {
            path: entry.path.clone(),
            directory: entry.directory,
            size: entry.size,
            sha256: if entry.directory {
                String::new()
            } else {
                hex::encode(digest.finalize())
            },
            mode: entry.mode,
        });
    }
    if sources != collect(root, &scope.excluded)?
        || self::scope(&state.store, id, project, exclude_generated)?.revision != revision
    {
        return Err(AppError::new(
            "SITE_BACKUP_CHANGED",
            "文件列表、内容时间或站点配置在备份期间变化，未发布归档",
        ));
    }
    let metadata = serde_json::to_vec(&manifest).map_err(|e| invalid(e.to_string()))?;
    if metadata.len() as u64 > MAX_MANIFEST {
        return Err(invalid("归档文件清单过大"));
    }
    zip.start_file(MANIFEST, zip::write::SimpleFileOptions::default())
        .map_err(archive_error)?;
    zip.write_all(&metadata)?;
    zip.finish().map_err(archive_error)?;
    pending.as_file().sync_all()?;
    let name = format!(
        "site-{}{}-{:016x}.zip",
        if automatic { "auto-" } else { "" },
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        rand::random::<u64>()
    );
    let dest = dir.join(&name);
    pending
        .persist_noclobber(&dest)
        .map_err(|e| AppError::io("发布站点备份", e.error))?;
    progress("complete", files, bytes);
    Ok(info(&dest, manifest))
}

fn read_manifest(zip: &mut zip::ZipArchive<File>, id: Option<&str>) -> Result<Manifest> {
    let mut entry = zip.by_name(MANIFEST).map_err(archive_error)?;
    if entry.size() > MAX_MANIFEST {
        return Err(invalid("归档清单过大"));
    }
    let mut bytes = Vec::new();
    (&mut entry)
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err(invalid("归档清单过大"));
    }
    drop(entry);
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| invalid("归档清单损坏或不受支持"))?;
    if manifest.format != FORMAT
        || id.is_some_and(|id| manifest.site_id != id)
        || manifest.site_id.is_empty()
        || manifest.site_id.len() > 256
        || manifest.site_id.chars().any(char::is_control)
        || manifest.entries.len() > MAX_ENTRIES
        || zip.len() != manifest.entries.len() + 1
    {
        return Err(invalid("归档不属于此站点或格式不受支持"));
    }
    Ok(manifest)
}
fn info(path: &Path, manifest: Manifest) -> BackupInfo {
    BackupInfo {
        automatic: manifest.automatic,
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
        path: path.to_string_lossy().into(),
        size_bytes: fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        created_at: manifest.created_at,
        files: manifest
            .entries
            .iter()
            .filter(|entry| !entry.directory)
            .count() as u64,
        original_bytes: manifest
            .entries
            .iter()
            .map(|entry| entry.size)
            .fold(0, u64::saturating_add),
        root: manifest.root,
        excluded: manifest.excluded,
        restorable: true,
        error: None,
    }
}
pub fn list(paths: &Paths, id: &str) -> Result<Vec<BackupInfo>> {
    let dir = directory(paths, id)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("site-")
            || !name.ends_with(".zip")
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-._".contains(&c))
        {
            continue;
        }
        let path = entry.path();
        let opened = archive_path(paths, id, &name)
            .and_then(|path| open_plain(&path))
            .and_then(open_snapshot)
            .and_then(|mut zip| read_manifest(&mut zip, Some(id)));
        result.push(match opened {
            Ok(manifest) => info(&path, manifest),
            Err(error) => BackupInfo {
                automatic: false,
                name,
                path: path.to_string_lossy().into(),
                size_bytes: fs::symlink_metadata(&path)?.len(),
                created_at: 0,
                files: 0,
                original_bytes: 0,
                root: String::new(),
                excluded: Vec::new(),
                restorable: false,
                error: Some(error.message),
            },
        });
    }
    result.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.name.cmp(&a.name))
    });
    Ok(result)
}
pub fn delete(state: &crate::CoreState, id: &str, name: &str) -> Result<()> {
    let _work = crate::BackgroundWork::begin("删除站点归档")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = lock(&state.paths, id)?;
    let path = archive_path(&state.paths, id, name)?;
    let file = open_plain(&path)?;
    drop(file);
    fs::remove_file(path)?;
    Ok(())
}
fn validate_manifest(
    zip: &mut zip::ZipArchive<File>,
    manifest: &Manifest,
    root: &Path,
) -> Result<()> {
    let mut names = BTreeSet::new();
    let mut aliases = std::collections::BTreeMap::new();
    let mut total = 0u64;
    for entry in &manifest.entries {
        crate::paths::checked_data_path(root, &entry.path)?;
        if !names.insert(entry.path.to_lowercase())
            || entry.path.len() > 2048
            || entry.path.split('/').count() > 128
            || (entry.directory && entry.size != 0)
        {
            return Err(invalid("归档路径重复、冲突或长度无效"));
        }
        let parts = entry.path.split('/').collect::<Vec<_>>();
        for end in 1..=parts.len() {
            let prefix = parts[..end].join("/");
            let directory = end < parts.len() || entry.directory;
            if let Some(previous) =
                aliases.insert(prefix.to_lowercase(), (prefix.clone(), directory))
            {
                if previous != (prefix, directory) {
                    return Err(invalid("归档包含大小写别名或文件与目录冲突"));
                }
            }
        }
        total = total
            .checked_add(entry.size)
            .ok_or_else(|| invalid("解压大小超限"))?;
        if total > MAX_BYTES {
            return Err(invalid("归档原始数据超过 50 GiB"));
        }
        let key = format!(
            "files/{}{}",
            entry.path,
            if entry.directory { "/" } else { "" }
        );
        let member = zip.by_name(&key).map_err(archive_error)?;
        let kind = member.unix_mode().unwrap_or(0) & 0o170000;
        if member.size() != entry.size
            || member.is_dir() != entry.directory
            || ![0, 0o040000, 0o100000].contains(&kind)
            || (entry.directory && kind == 0o100000)
            || (!entry.directory && kind == 0o040000)
        {
            return Err(invalid("归档文件类型或大小与清单不一致"));
        }
        if !entry.directory
            && (entry.sha256.len() != 64 || !entry.sha256.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(invalid("归档文件缺少有效校验值"));
        }
    }
    Ok(())
}

// NiceEnv 生成无注释、无前缀、单磁盘 ZIP。先限制中央目录，避免第三方输入在 ZipArchive::new 内大量分配。
fn check_archive_directory(file: &mut File) -> Result<usize> {
    let length = file.metadata()?.len();
    if !(22..=MAX_ARCHIVE).contains(&length) {
        return Err(invalid("归档为空或超过 51 GiB"));
    }
    file.seek(SeekFrom::End(-22))?;
    let mut end = [0u8; 22];
    file.read_exact(&mut end)?;
    let u16_at = |bytes: &[u8], offset| {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as u64
    };
    let u32_at = |bytes: &[u8], offset| {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as u64
    };
    let u64_at =
        |bytes: &[u8], offset| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
    if &end[..4] != b"PK\x05\x06"
        || u16_at(&end, 20) != 0
        || u16_at(&end, 4) != 0
        || u16_at(&end, 6) != 0
        || u16_at(&end, 8) != u16_at(&end, 10)
    {
        return Err(invalid(
            "请选择 NiceEnv 创建的完整站点 ZIP；不支持分卷、附加注释或自解压归档",
        ));
    }
    let (mut count, mut size, mut offset, mut footer) = (
        u16_at(&end, 10),
        u32_at(&end, 12),
        u32_at(&end, 16),
        length - 22,
    );
    if count == u16::MAX as u64 || size == u32::MAX as u64 || offset == u32::MAX as u64 {
        if length < 98 {
            return Err(invalid("ZIP64 归档不完整"));
        }
        file.seek(SeekFrom::End(-42))?;
        let mut locator = [0; 20];
        file.read_exact(&mut locator)?;
        if &locator[..4] != b"PK\x06\x07" || u32_at(&locator, 4) != 0 || u32_at(&locator, 16) != 1 {
            return Err(invalid("ZIP64 定位记录无效"));
        }
        footer = u64_at(&locator, 8);
        if footer.checked_add(56) != Some(length - 42) {
            return Err(invalid("ZIP64 结束记录无效"));
        }
        file.seek(SeekFrom::Start(footer))?;
        let mut record = [0; 56];
        file.read_exact(&mut record)?;
        if &record[..4] != b"PK\x06\x06"
            || u64_at(&record, 4) != 44
            || u32_at(&record, 16) != 0
            || u32_at(&record, 20) != 0
            || u64_at(&record, 24) != u64_at(&record, 32)
        {
            return Err(invalid("ZIP64 结束记录无效"));
        }
        count = u64_at(&record, 32);
        size = u64_at(&record, 40);
        offset = u64_at(&record, 48);
    }
    if count == 0
        || count > MAX_ENTRIES as u64 + 1
        || size > 256 * 1024 * 1024
        || count.saturating_mul(46) > size
        || offset.checked_add(size) != Some(footer)
    {
        return Err(invalid("归档中央目录不完整或超出站点备份限制"));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut signature = [0; 4];
    file.read_exact(&mut signature)?;
    if &signature != b"PK\x03\x04" {
        return Err(invalid("不支持带前缀的归档，请选择 NiceEnv 原始 ZIP"));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(count as usize)
}

fn open_snapshot(mut file: File) -> Result<zip::ZipArchive<File>> {
    let count = check_archive_directory(&mut file)?;
    let zip = zip::ZipArchive::new(file).map_err(archive_error)?;
    if zip.len() != count {
        return Err(invalid("归档存在重复或缺失的 ZIP 条目"));
    }
    Ok(zip)
}

fn snapshot_source(
    paths: &Paths,
    source: &str,
    phase: &str,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<(tempfile::NamedTempFile, PathBuf, String)> {
    let source = Path::new(source);
    let parent = source
        .parent()
        .ok_or_else(|| invalid("请选择本机的 ZIP 文件"))?;
    let path =
        plain_directory(parent)?.join(source.file_name().ok_or_else(|| invalid("归档路径无效"))?);
    let mut input = open_plain(&path)?;
    check_archive_directory(&mut input)?;
    let before = input.metadata()?;
    let mut snapshot = tempfile::Builder::new()
        .prefix(".site-import-")
        .tempfile_in(&paths.base)?;
    let mut digest = Sha256::new();
    let mut bytes = 0;
    let mut buffer = vec![0; 256 * 1024];
    progress(phase, 0, 0);
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > MAX_ARCHIVE {
            return Err(invalid("归档超过 51 GiB"));
        }
        snapshot.write_all(&buffer[..count])?;
        digest.update(&buffer[..count]);
        progress(phase, 0, bytes);
    }
    let after = input.metadata()?;
    if bytes != before.len()
        || after.len() != before.len()
        || after.modified()? != before.modified()?
    {
        return Err(AppError::new(
            "SITE_IMPORT_CHANGED",
            "源 ZIP 在读取期间发生变化，请重新选择",
        ));
    }
    snapshot.flush()?;
    check_archive_directory(snapshot.as_file_mut())?;
    Ok((snapshot, path, hex::encode(digest.finalize())))
}
fn import_revision(site: &Site, digest: &str) -> Result<String> {
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            digest,
            &site.id,
            &site.name,
            &site.root_dir,
            site.updated_at,
        ))
        .map_err(|error| invalid(error.to_string()))?,
    )))
}

pub fn inspect_import(
    state: &crate::CoreState,
    id: &str,
    source: &str,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<ImportPreview> {
    let _work = crate::BackgroundWork::begin("检查外部站点归档")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let target = site(&state.store, id)?;
    let (snapshot, path, digest) = snapshot_source(&state.paths, source, "inspect", progress)?;
    let mut zip = open_snapshot(snapshot.reopen()?)?;
    let manifest = read_manifest(&mut zip, None)?;
    let scratch = tempfile::Builder::new()
        .prefix(".site-import-check-")
        .tempdir_in(&state.paths.base)?;
    validate_manifest(&mut zip, &manifest, scratch.path())?;
    let revision = import_revision(&target, &digest)?;
    if import_revision(&site(&state.store, id)?, &digest)? != revision {
        return Err(AppError::new(
            "SITE_IMPORT_CHANGED",
            "目标站点在检查期间变化，请重新读取",
        ));
    }
    let source_site_id = manifest.site_id.clone();
    let mut archive = info(&path, manifest);
    archive.size_bytes = snapshot.as_file().metadata()?.len();
    progress("complete", archive.files, archive.original_bytes);
    Ok(ImportPreview {
        source_path: path.to_string_lossy().into(),
        source_site_id,
        target_name: target.name,
        target_root: target.root_dir,
        archive,
        revision,
    })
}

pub fn import_archive(
    state: &crate::CoreState,
    id: &str,
    source: &str,
    revision: &str,
    confirmed: bool,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<BackupInfo> {
    if !confirmed {
        return Err(invalid(
            "请确认归档来源可信，并同意将其作为此站点的备份副本导入",
        ));
    }
    let _work = crate::BackgroundWork::begin("导入站点文件归档")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = lock(&state.paths, id)?;
    let target = site(&state.store, id)?;
    let (snapshot, _, digest) = snapshot_source(&state.paths, source, "importRead", progress)?;
    if import_revision(&target, &digest)? != revision {
        return Err(AppError::new(
            "SITE_IMPORT_CHANGED",
            "源 ZIP 或目标站点已变化，请重新读取并确认",
        ));
    }
    let mut zip = open_snapshot(snapshot.reopen()?)?;
    let mut manifest = read_manifest(&mut zip, None)?;
    let dir = plain_directory(&directory(&state.paths, id)?)?;
    let scratch = tempfile::Builder::new()
        .prefix(".import-check-")
        .tempdir_in(&dir)?;
    validate_manifest(&mut zip, &manifest, scratch.path())?;
    let mut pending = tempfile::Builder::new()
        .prefix(".pending-")
        .tempfile_in(&dir)?;
    let mut output = zip::ZipWriter::new(pending.as_file_mut());
    let mut buffer = vec![0; 256 * 1024];
    let (mut files, mut bytes) = (0, 0);
    for entry in &manifest.entries {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true)
            .unix_permissions(entry.mode & 0o777);
        let key = format!(
            "files/{}{}",
            entry.path,
            if entry.directory { "/" } else { "" }
        );
        if entry.directory {
            output.add_directory(key, options).map_err(archive_error)?;
            continue;
        }
        let mut input = zip.by_name(&key).map_err(archive_error)?;
        output.start_file(key, options).map_err(archive_error)?;
        copy_verified(
            &mut input,
            &mut output,
            entry,
            &mut buffer,
            files,
            &mut bytes,
            "import",
            progress,
        )?;
        files += 1;
    }
    manifest.site_id = id.into();
    // 外部归档导入属于用户手动保存的副本，不纳入本机自动保留策略。
    manifest.automatic = false;
    let metadata = serde_json::to_vec(&manifest).map_err(|error| invalid(error.to_string()))?;
    if metadata.len() as u64 > MAX_MANIFEST {
        return Err(invalid("导入后的归档清单过大"));
    }
    output
        .start_file(MANIFEST, zip::write::SimpleFileOptions::default())
        .map_err(archive_error)?;
    output.write_all(&metadata)?;
    output.finish().map_err(archive_error)?;
    pending.as_file().sync_all()?;
    if import_revision(&site(&state.store, id)?, &digest)? != revision {
        return Err(AppError::new(
            "SITE_IMPORT_CHANGED",
            "目标站点在导入期间变化，未发布归档，请重新读取",
        ));
    }
    let name = format!(
        "site-{}-{:016x}.zip",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        rand::random::<u64>()
    );
    let destination = dir.join(name);
    pending
        .persist_noclobber(&destination)
        .map_err(|error| AppError::io("发布导入的站点归档", error.error))?;
    progress("complete", files, bytes);
    Ok(info(&destination, manifest))
}

fn restore_parent(state: &crate::CoreState, id: &str, parent: Option<&str>) -> Result<PathBuf> {
    let parent = match parent {
        Some(path) if !path.is_empty() => plain_directory(Path::new(path))?,
        _ => {
            let path = crate::paths::checked_data_path(&state.paths.base, "restored-sites")?;
            fs::create_dir_all(&path)?;
            plain_directory(&path)?
        }
    };
    for site in state.store.list_sites()? {
        for project in [true, false] {
            if let Ok(scope) = scope(&state.store, &site.id, project, false) {
                if within(&parent, Path::new(&scope.root)) {
                    return Err(invalid(
                        "恢复父目录不能位于已登记站点的项目目录内，请选择独立目录",
                    ));
                }
            }
        }
    }
    let archive_dir = plain_directory(&directory(&state.paths, id)?)?;
    if within(&parent, archive_dir.parent().unwrap_or(&state.paths.base)) {
        return Err(invalid("不能把项目恢复到归档目录内"));
    }
    Ok(parent)
}
fn restore_revision(site: &Site, name: &str, digest: &str, parent: &Path) -> Result<String> {
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(&(import_revision(site, digest)?, name, parent))
            .map_err(|error| invalid(error.to_string()))?,
    )))
}
fn restore_changed() -> AppError {
    AppError::new(
        "SITE_RESTORE_CHANGED",
        "归档、站点或恢复目录已变化，请重新校验并确认",
    )
}

// 同一流式内容校验用于预检、导入和恢复；读取到 EOF 才能核对 ZIP CRC。
fn copy_verified(
    input: &mut dyn Read,
    output: &mut dyn Write,
    entry: &Entry,
    buffer: &mut [u8],
    files: u64,
    bytes: &mut u64,
    phase: &str,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<()> {
    let mut digest = Sha256::new();
    let mut written = 0;
    loop {
        let count = input.read(buffer).map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
            ) {
                invalid(format!("归档文件损坏或校验失败：{}", entry.path))
                    .with_hint("请使用其它归档，或重新拷贝完整 ZIP 后再校验")
                    .with_detail(error.to_string())
            } else {
                AppError::io("读取归档文件", error)
            }
        })?;
        if count == 0 {
            break;
        }
        written += count as u64;
        if written > entry.size {
            return Err(invalid("归档解压大小与清单不一致"));
        }
        output.write_all(&buffer[..count])?;
        digest.update(&buffer[..count]);
        *bytes += count as u64;
        progress(phase, files, *bytes);
    }
    if written != entry.size || hex::encode(digest.finalize()) != entry.sha256.to_lowercase() {
        return Err(invalid(format!("归档文件校验失败：{}", entry.path)));
    }
    progress(phase, files + 1, *bytes);
    Ok(())
}

pub fn inspect_restore(
    state: &crate::CoreState,
    id: &str,
    name: &str,
    parent: Option<&str>,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<RestorePreview> {
    let _work = crate::BackgroundWork::begin("校验站点归档")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = lock(&state.paths, id)?;
    let target = site(&state.store, id)?;
    let parent = restore_parent(state, id, parent)?;
    let path = archive_path(&state.paths, id, name)?;
    let (snapshot, path, digest) = snapshot_source(
        &state.paths,
        &path.to_string_lossy(),
        "verifyRead",
        progress,
    )?;
    let mut zip = open_snapshot(snapshot.reopen()?)?;
    let manifest = read_manifest(&mut zip, Some(id))?;
    // 临时空目录同时核对目标可写和本机路径规则；预检不释放任何项目文件。
    let scratch = tempfile::Builder::new()
        .prefix(".site-restore-check-")
        .tempdir_in(&parent)?;
    validate_manifest(&mut zip, &manifest, scratch.path())?;
    let (mut files, mut bytes) = (0, 0);
    let mut buffer = vec![0; 256 * 1024];
    progress("verify", 0, 0);
    for entry in &manifest.entries {
        if entry.directory {
            continue;
        }
        let mut input = zip
            .by_name(&format!("files/{}", entry.path))
            .map_err(archive_error)?;
        copy_verified(
            &mut input,
            &mut std::io::sink(),
            entry,
            &mut buffer,
            files,
            &mut bytes,
            "verify",
            progress,
        )?;
        files += 1;
    }
    let revision = restore_revision(&target, name, &digest, &parent)?;
    let current_parent = restore_parent(state, id, Some(&parent.to_string_lossy()))?;
    if restore_revision(&site(&state.store, id)?, name, &digest, &current_parent)? != revision {
        return Err(restore_changed());
    }
    let total_entries = manifest.entries.len();
    let entries = manifest
        .entries
        .iter()
        .take(100)
        .map(|entry| RestoreEntry {
            path: entry.path.clone(),
            directory: entry.directory,
            size: entry.size,
        })
        .collect();
    let mut archive = info(&path, manifest);
    archive.size_bytes = snapshot.as_file().metadata()?.len();
    progress("complete", files, bytes);
    Ok(RestorePreview {
        archive,
        parent: parent.to_string_lossy().into(),
        revision,
        sha256: digest,
        verified_at: crate::services::now_ms(),
        entries,
        total_entries,
    })
}

pub fn restore(
    state: &crate::CoreState,
    id: &str,
    name: &str,
    parent: Option<&str>,
    revision: &str,
    trusted: bool,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<String> {
    if !trusted {
        return Err(invalid(
            "请先确认归档来源可信；恢复的项目文件可能包含可执行代码和密钥",
        ));
    }
    let _work = crate::BackgroundWork::begin("恢复站点文件")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = lock(&state.paths, id)?;
    let parent = restore_parent(state, id, parent)?;
    let path = archive_path(&state.paths, id, name)?;
    let (snapshot, _, digest) = snapshot_source(
        &state.paths,
        &path.to_string_lossy(),
        "verifyRead",
        progress,
    )?;
    if restore_revision(&site(&state.store, id)?, name, &digest, &parent)? != revision {
        return Err(restore_changed());
    }
    let mut zip = open_snapshot(snapshot.reopen()?)?;
    let manifest = read_manifest(&mut zip, Some(id))?;
    // 私有新目录由 tempfile 原子创建；不使用用户输入作为目标子目录名，不覆盖既有目录。
    let pending = tempfile::Builder::new()
        .prefix("restored-site-")
        .tempdir_in(&parent)?;
    validate_manifest(&mut zip, &manifest, pending.path())?;
    let (mut files, mut bytes) = (0, 0);
    let mut buffer = vec![0; 256 * 1024];
    for entry in &manifest.entries {
        let target = crate::paths::checked_data_path(pending.path(), &entry.path)?;
        if entry.directory {
            fs::create_dir_all(target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut member = zip
            .by_name(&format!("files/{}", entry.path))
            .map_err(archive_error)?;
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)?;
        copy_verified(
            &mut member,
            &mut output,
            entry,
            &mut buffer,
            files,
            &mut bytes,
            "restore",
            progress,
        )?;
        output.sync_all()?;
        drop(output);
        files += 1;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(target, fs::Permissions::from_mode(entry.mode & 0o777))?;
        }
    }
    let current_parent = restore_parent(state, id, Some(&parent.to_string_lossy()))?;
    if restore_revision(&site(&state.store, id)?, name, &digest, &current_parent)? != revision {
        return Err(restore_changed());
    }
    progress("complete", files, bytes);
    Ok(pending.keep().to_string_lossy().into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePlan {
    pub status: crate::backup_job::BackupPlan,
    pub project: bool,
    pub exclude_generated: bool,
    pub scope: Option<Scope>,
    #[serde(default)]
    pub revision: String,
}
impl Default for FilePlan {
    fn default() -> Self {
        Self {
            status: crate::backup_job::BackupPlan {
                state: "idle".into(),
                ..Default::default()
            },
            project: true,
            exclude_generated: true,
            scope: None,
            revision: String::new(),
        }
    }
}
fn plan_key(id: &str) -> String {
    format!(
        "siteFileBackupPlan@{}",
        hex::encode(Sha256::digest(id.as_bytes()))
    )
}
fn plan_revision(plan: &FilePlan) -> Result<String> {
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            &plan.status.config,
            plan.project,
            plan.exclude_generated,
            &plan.scope,
        ))
        .map_err(|error| invalid(error.to_string()))?,
    )))
}
fn read_plan(store: &Store, id: &str) -> Result<FilePlan> {
    let mut plan: FilePlan = match store.get_setting_checked(&plan_key(id))? {
        None => FilePlan::default(),
        Some(json) => serde_json::from_str(&json)
            .map_err(|error| AppError::internal("读取站点自动备份计划", error.to_string()))?,
    };
    plan.revision = plan_revision(&plan)?;
    Ok(plan)
}
fn write_plan(store: &Store, id: &str, plan: &mut FilePlan) -> Result<()> {
    plan.revision = plan_revision(plan)?;
    store.set_setting_json(&plan_key(id), plan)
}
pub fn file_plan(state: &crate::CoreState, id: &str) -> Result<FilePlan> {
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    site(&state.store, id)?;
    let mut plan = read_plan(&state.store, id)?;
    if plan.status.state == "running" {
        match lock(&state.paths, id) {
            Ok(_lock) => {
                plan = read_plan(&state.store, id)?;
                if plan.status.state == "running" {
                    plan.status.state = "interrupted".into();
                    plan.status.message =
                        "上次执行中断，已生成的归档仍保留；请检查后立即执行计划".into();
                }
            }
            Err(error) if error.code == "SITE_BACKUP_BUSY" => {}
            Err(error) => return Err(error),
        }
    }
    Ok(plan)
}
pub fn save_file_plan(
    state: &crate::CoreState,
    id: &str,
    config: crate::backup_job::BackupPlanConfig,
    project: bool,
    exclude_generated: bool,
    expected_revision: &str,
    scope_revision: Option<&str>,
    confirmed: bool,
) -> Result<FilePlan> {
    let _work = crate::BackgroundWork::begin("保存站点自动备份计划")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = lock(&state.paths, id)?;
    site(&state.store, id)?;
    crate::backup_job::validate_plan(&config)?;
    let mut plan = read_plan(&state.store, id)?;
    if plan.revision != expected_revision {
        return Err(AppError::new(
            "SITE_PLAN_CHANGED",
            "备份计划已在其它窗口变化，请重新读取",
        ));
    }
    if config.enabled {
        if !confirmed {
            return Err(invalid("请确认自动备份范围、敏感文件和历史保留策略"));
        }
        let current = scope(&state.store, id, project, exclude_generated)?;
        if Some(current.revision.as_str()) != scope_revision {
            return Err(AppError::new(
                "SITE_BACKUP_CHANGED",
                "站点范围已变化，请重新读取并确认",
            ));
        }
        plan.scope = Some(current);
        plan.project = project;
        plan.exclude_generated = exclude_generated;
    }
    plan.status.next_at = if config.enabled {
        Some(crate::backup_job::next_run(&config, chrono::Local::now())?)
    } else {
        None
    };
    plan.status.config = config;
    if ["running", "needs-review"].contains(&plan.status.state.as_str()) {
        plan.status.state = "idle".into();
        plan.status.message.clear();
    }
    write_plan(&state.store, id, &mut plan)?;
    Ok(plan)
}
fn rotate_automatic(paths: &Paths, id: &str, keep: usize, newest: &str) -> Result<()> {
    if keep == 0 {
        return Ok(());
    }
    let mut archives = list(paths, id)?
        .into_iter()
        .filter(|entry| entry.restorable && entry.automatic && entry.name.starts_with("site-auto-"))
        .collect::<Vec<_>>();
    archives.sort_by(|a, b| {
        (b.name == newest)
            .cmp(&(a.name == newest))
            .then_with(|| b.created_at.cmp(&a.created_at))
            .then_with(|| b.name.cmp(&a.name))
    });
    for archive in archives.into_iter().skip(keep) {
        let path = archive_path(paths, id, &archive.name)?;
        let file = open_plain(&path)?;
        drop(file);
        fs::remove_file(path)?;
    }
    Ok(())
}
pub fn run_file_plan(
    state: &crate::CoreState,
    id: &str,
    manual: bool,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<FilePlan> {
    let _work = crate::BackgroundWork::begin("执行站点自动备份")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = lock(&state.paths, id)?;
    site(&state.store, id)?;
    let mut plan = read_plan(&state.store, id)?;
    let now = chrono::Local::now();
    if !manual
        && (!plan.status.config.enabled
            || !plan
                .status
                .next_at
                .is_some_and(|at| at <= now.timestamp_millis()))
    {
        return Ok(plan);
    }
    crate::backup_job::validate_plan(&plan.status.config)?;
    plan.status.next_at = if plan.status.config.enabled {
        Some(crate::backup_job::next_run(&plan.status.config, now)?)
    } else {
        None
    };
    plan.status.last_run_at = Some(now.timestamp_millis());
    plan.status.finished_at = None;
    plan.status.state = "running".into();
    plan.status.message.clear();
    plan.status.files.clear();
    write_plan(&state.store, id, &mut plan)?;
    let result = (|| -> Result<()> {
        let approved = plan.scope.as_ref().ok_or_else(|| {
            AppError::new("SITE_PLAN_SCOPE", "请先设置并确认此站点的自动备份范围")
        })?;
        let current = scope(&state.store, id, plan.project, plan.exclude_generated)?;
        if current.revision != approved.revision {
            return Err(AppError::new(
                "SITE_PLAN_SCOPE",
                "站点配置或源目录已变化，自动备份已暂停；请重新确认范围",
            ));
        }
        let archive = create_locked(
            state,
            id,
            plan.project,
            plan.exclude_generated,
            &approved.revision,
            true,
            progress,
        )?;
        plan.status.files.push(archive.name.clone());
        write_plan(&state.store, id, &mut plan)?;
        if let Err(error) =
            rotate_automatic(&state.paths, id, plan.status.config.keep, &archive.name)
        {
            plan.status.state = "partial".into();
            plan.status.message = format!("新归档已保留，清理旧自动备份失败：{}", error.message);
        } else {
            plan.status.state = "success".into();
            plan.status.message = format!(
                "已备份 {} 个文件，共 {} 字节",
                archive.files, archive.original_bytes
            );
        }
        Ok(())
    })();
    if let Err(error) = result {
        // 普通文件写入造成快照失败仍在下一周期重试；仅批准的站点范围变化才暂停。
        let scope_changed = error.code == "SITE_BACKUP_CHANGED"
            && scope(&state.store, id, plan.project, plan.exclude_generated)
                .ok()
                .zip(plan.scope.as_ref())
                .is_some_and(|(current, approved)| current.revision != approved.revision);
        if error.code == "SITE_PLAN_SCOPE" || scope_changed {
            plan.status.state = "needs-review".into();
            plan.status.config.enabled = false;
            plan.status.next_at = None;
        } else {
            plan.status.state = if plan.status.files.is_empty() {
                "failed"
            } else {
                "partial"
            }
            .into();
        }
        plan.status.message = error.message;
    }
    plan.status.finished_at = Some(crate::services::now_ms());
    // 长任务跨过多个时间点时不连续补跑；重新打开应用也只补执行一次。
    if plan.status.config.enabled {
        plan.status.next_at = Some(crate::backup_job::next_run(
            &plan.status.config,
            chrono::Local::now(),
        )?);
    }
    write_plan(&state.store, id, &mut plan)?;
    state.emit_event(crate::Event::SiteBackupStatus {
        site_id: id.into(),
        state: plan.status.state.clone(),
        message: plan.status.message.clone(),
    });
    Ok(plan)
}
pub(crate) fn tick_file_plans(state: &crate::CoreState) -> Result<()> {
    let _work = crate::BackgroundWork::begin("站点备份调度")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let mut failures = Vec::new();
    for site in state.store.list_sites()? {
        let result = read_plan(&state.store, &site.id).and_then(|plan| {
            if plan.status.config.enabled
                && plan
                    .status
                    .next_at
                    .is_some_and(|at| at <= crate::services::now_ms())
            {
                run_file_plan(state, &site.id, false, &|_, _, _| {}).map(|_| ())
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            if error.code != "SITE_BACKUP_BUSY" {
                failures.push(format!("{}：{}", site.name, error.message));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(AppError::new("SITE_PLAN_SCHEDULER", failures.join("；")))
    }
}
pub fn spawn_scheduler_when_ready(
    state: std::sync::Arc<crate::CoreState>,
    gate: std::sync::Arc<crate::restart::StartupGate>,
) -> Result<()> {
    std::thread::Builder::new()
        .name("site-file-backup-scheduler".into())
        .spawn(move || {
            if !gate.wait() {
                return;
            }
            let mut previous_error = String::new();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
                match tick_file_plans(&state) {
                    Ok(()) => previous_error.clear(),
                    Err(error) => {
                        if error.message != previous_error {
                            state.emit_event(crate::Event::SiteBackupStatus {
                                site_id: String::new(),
                                state: "failed".into(),
                                message: error.message.clone(),
                            });
                            previous_error = error.message;
                        }
                    }
                }
            }
        })
        .map_err(|error| AppError::io("启动站点自动备份调度", error))?;
    Ok(())
}
