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
    io::{Read, Write},
    path::{Path, PathBuf},
};

const FORMAT: &str = "niceenv/site-files-v1";
const MANIFEST: &str = "niceenv-site-backup.json";
const MAX_ENTRIES: usize = 100_000;
const MAX_BYTES: u64 = 50 * 1024 * 1024 * 1024;
const MAX_MANIFEST: u64 = 32 * 1024 * 1024;
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
    pub root: String,
    pub revision: String,
    pub excluded: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub created_at: i64,
    pub files: u64,
    pub original_bytes: u64,
    pub root: String,
    pub excluded: Vec<String>,
    pub restorable: bool,
    pub error: Option<String>,
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
        "site-{}-{:016x}.zip",
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

fn read_manifest(zip: &mut zip::ZipArchive<File>, id: &str) -> Result<Manifest> {
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
        || manifest.site_id != id
        || manifest.entries.len() > MAX_ENTRIES
        || zip.len() != manifest.entries.len() + 1
    {
        return Err(invalid("归档不属于此站点或格式不受支持"));
    }
    Ok(manifest)
}
fn info(path: &Path, manifest: Manifest) -> BackupInfo {
    BackupInfo {
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
            .and_then(|file| zip::ZipArchive::new(file).map_err(archive_error))
            .and_then(|mut zip| read_manifest(&mut zip, id));
        result.push(match opened {
            Ok(manifest) => info(&path, manifest),
            Err(error) => BackupInfo {
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
    result.sort_by(|a, b| b.name.cmp(&a.name));
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
pub fn restore(
    state: &crate::CoreState,
    id: &str,
    name: &str,
    parent: Option<&str>,
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
    let path = archive_path(&state.paths, id, name)?;
    let mut zip = zip::ZipArchive::new(open_plain(&path)?).map_err(archive_error)?;
    let manifest = read_manifest(&mut zip, id)?;
    if zip.len() != manifest.entries.len() + 1 {
        return Err(invalid("归档条目与清单不一致"));
    }
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
    // 私有新目录由 tempfile 原子创建；不使用用户输入作为目标子目录名，不覆盖既有目录。
    let pending = tempfile::Builder::new()
        .prefix("restored-site-")
        .tempdir_in(&parent)?;
    let mut names = BTreeSet::new();
    let mut aliases = std::collections::BTreeMap::new();
    let mut total = 0u64;
    for entry in &manifest.entries {
        crate::paths::checked_data_path(pending.path(), &entry.path)?;
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
        let mut digest = Sha256::new();
        let mut written = 0;
        loop {
            let n = member.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            written += n as u64;
            if written > entry.size {
                return Err(invalid("归档解压大小与清单不一致"));
            }
            output.write_all(&buffer[..n])?;
            digest.update(&buffer[..n]);
            bytes += n as u64;
            progress("restore", files, bytes);
        }
        if written != entry.size || hex::encode(digest.finalize()) != entry.sha256.to_lowercase() {
            return Err(invalid("归档校验失败，未保留恢复副本"));
        }
        output.sync_all()?;
        drop(output);
        files += 1;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(target, fs::Permissions::from_mode(entry.mode & 0o777))?;
        }
    }
    progress("complete", files, bytes);
    Ok(pending.keep().to_string_lossy().into())
}
