//! 站点目录浏览与小型文本编辑器。
//!
//! 这里故意只暴露站点根目录下的普通目录和文本文件：不跟随软链接、拒绝
//! `..` 穿越、限制单次读取大小，并使用 revision 做乐观并发校验。这样站点
//! 页面可以提供 ServBay/FlyEnv 常见的快速文件入口，同时不会把任意本机路径
//! 变成桌面端文件读写 API。

use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    error::{AppError, Result},
    model::Site,
    store::Store,
};

const MAX_ENTRIES: usize = 500;
const MAX_TEXT_BYTES: u64 = 1024 * 1024;
const MAX_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RELATIVE_PATH: usize = 2048;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteFileEntry {
    pub name: String,
    pub path: String,
    pub directory: bool,
    pub size_bytes: u64,
    pub modified_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteDirectory {
    pub site_id: String,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub root: String,
    pub current: String,
    pub parent: Option<String>,
    pub revision: String,
    pub entries: Vec<SiteFileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteTextFile {
    pub site_id: String,
    pub path: String,
    pub size_bytes: u64,
    pub revision: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteFileDeleteReceipt {
    pub site_id: String,
    pub path: String,
    pub directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteFileCreateReceipt {
    pub site_id: String,
    pub path: String,
    pub directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteFileRenameReceipt {
    pub site_id: String,
    pub from: String,
    pub path: String,
    pub directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteFileUploadReceipt {
    pub site_id: String,
    pub path: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteFileDownloadReceipt {
    pub site_id: String,
    pub path: String,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub destination: String,
    pub size_bytes: u64,
}

fn site(store: &Store, id: &str) -> Result<Site> {
    store
        .list_sites()?
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| AppError::new("SITE_NOT_FOUND", "站点已不存在，请刷新列表"))
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

fn root(store: &Store, id: &str) -> Result<PathBuf> {
    let site = site(store, id)?;
    if site.runtime.kind == crate::model::SiteKind::Redirect || site.root_dir.trim().is_empty() {
        return Err(AppError::new(
            "SITE_FILES_UNAVAILABLE",
            "跳转站点没有可浏览的项目目录",
        ));
    }
    let raw = Path::new(&site.root_dir);
    let meta = fs::symlink_metadata(raw).map_err(|e| AppError::io("读取站点目录", e))?;
    if linked(&meta) || !meta.is_dir() {
        return Err(AppError::new(
            "SITE_FILES_UNAVAILABLE",
            "站点根目录不是可安全浏览的普通目录",
        ));
    }
    let actual = raw
        .canonicalize()
        .map_err(|e| AppError::io("解析站点目录", e))?;
    if !actual.is_dir() {
        return Err(AppError::new(
            "SITE_FILES_UNAVAILABLE",
            "站点根目录不存在或不可读取",
        ));
    }
    Ok(actual)
}

fn relative(input: Option<&str>) -> Result<PathBuf> {
    let value = input.unwrap_or_default();
    if value.len() > MAX_RELATIVE_PATH || value.chars().any(char::is_control) {
        return Err(AppError::new(
            "SITE_FILE_PATH_INVALID",
            "文件路径过长或包含控制字符",
        ));
    }
    let mut output = PathBuf::new();
    for component in Path::new(value).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                let part = part.to_string_lossy();
                if part.is_empty()
                    || part == "."
                    || part == ".."
                    || part.contains([':', '\\', '/', '\0'])
                {
                    return Err(AppError::new(
                        "SITE_FILE_PATH_INVALID",
                        "文件路径包含不安全的目录名",
                    ));
                }
                output.push(part.as_ref());
            }
            Component::RootDir | Component::Prefix(_) | Component::ParentDir => {
                return Err(AppError::new(
                    "SITE_FILE_PATH_INVALID",
                    "文件路径必须位于站点根目录内",
                ));
            }
        }
    }
    Ok(output)
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn checked_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative.as_os_str().is_empty() {
        return Ok(root.to_path_buf());
    }
    // 必须先检查原始路径各层，再 canonicalize，否则内部软链接会被解析掉。
    let mut candidate = root.to_path_buf();
    for component in relative.components() {
        candidate.push(component);
        let meta = fs::symlink_metadata(&candidate).map_err(|e| AppError::io("读取站点文件", e))?;
        if linked(&meta) {
            return Err(AppError::new(
                "SITE_FILE_PATH_INVALID",
                "站点文件路径不能经过符号链接或目录联接",
            ));
        }
    }
    let actual = candidate
        .canonicalize()
        .map_err(|e| AppError::io("解析站点文件", e))?;
    if !actual.starts_with(root) {
        return Err(AppError::new(
            "SITE_FILE_PATH_INVALID",
            "文件路径超出站点根目录",
        ));
    }
    Ok(candidate)
}

fn checked_new_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    let Some(name) = relative.file_name() else {
        return Err(AppError::new("SITE_FILE_PATH_INVALID", "请选择要创建的文件或目录"));
    };
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let parent_path = checked_path(root, parent)?;
    let parent_meta = fs::symlink_metadata(&parent_path).map_err(|e| AppError::io("读取目标目录", e))?;
    if linked(&parent_meta) || !parent_meta.is_dir() {
        return Err(AppError::new("SITE_FILE_NOT_DIRECTORY", "目标父目录不存在或不可写"));
    }
    let target = parent_path.join(name);
    if fs::symlink_metadata(&target).is_ok() {
        return Err(AppError::new("SITE_FILE_EXISTS", "同名文件或目录已存在"));
    }
    Ok(target)
}

fn modified_at(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn directory_revision(entries: &[SiteFileEntry]) -> String {
    let mut digest = Sha256::new();
    for entry in entries {
        digest.update(entry.path.as_bytes());
        digest.update([0]);
        digest.update([u8::from(entry.directory)]);
        digest.update(entry.size_bytes.to_le_bytes());
        digest.update(entry.modified_at.to_le_bytes());
    }
    hex::encode(digest.finalize())
}

fn file_revision(root: &Path, path: &Path, bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    for value in [
        root.to_string_lossy().as_bytes(),
        path.to_string_lossy().as_bytes(),
        bytes,
    ] {
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value);
    }
    hex::encode(digest.finalize())
}

fn sensitive_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || matches!(
            name.as_str(),
            "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519"
        )
        || name.ends_with(".key")
        || name.ends_with(".pem")
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
        || name.ends_with(".crt")
        || name.ends_with(".cer")
}

fn read_text(path: &Path) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path).map_err(|e| AppError::io("读取站点文件", e))?;
    if linked(&meta) || !meta.is_file() {
        return Err(AppError::new("SITE_FILE_NOT_TEXT", "请选择普通文本文件"));
    }
    if sensitive_name(path) {
        return Err(AppError::new(
            "SITE_FILE_SENSITIVE",
            "环境变量和密钥文件请使用专用编辑器或系统文件管理器打开",
        ));
    }
    if meta.len() > MAX_TEXT_BYTES {
        return Err(AppError::new(
            "SITE_FILE_TOO_LARGE",
            "文件超过 1 MiB，暂不支持在应用内编辑",
        ));
    }
    let file = fs::File::open(path).map_err(|e| AppError::io("读取站点文件", e))?;
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| AppError::io("读取站点文件", e))?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(AppError::new(
            "SITE_FILE_TOO_LARGE",
            "文件超过 1 MiB，暂不支持在应用内编辑",
        ));
    }
    if bytes.contains(&0) {
        return Err(AppError::new(
            "SITE_FILE_NOT_TEXT",
            "该文件包含二进制内容，不能在文本编辑器中打开",
        ));
    }
    Ok(bytes)
}

pub fn list(store: &Store, id: &str, current: Option<&str>) -> Result<SiteDirectory> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let root = root(store, id)?;
    let relative = relative(current)?;
    let directory = checked_path(&root, &relative)?;
    let meta = fs::symlink_metadata(&directory).map_err(|e| AppError::io("读取站点目录", e))?;
    if !meta.is_dir() || linked(&meta) {
        return Err(AppError::new(
            "SITE_FILE_NOT_DIRECTORY",
            "请选择一个普通目录",
        ));
    }
    let mut entries = Vec::new();
    for (index, item) in fs::read_dir(&directory)
        .map_err(|e| AppError::io("读取站点目录", e))?
        .enumerate()
    {
        if index >= MAX_ENTRIES {
            return Err(AppError::new(
                "SITE_DIRECTORY_TOO_LARGE",
                "此目录超过 500 项，请使用系统文件管理器打开",
            ));
        }
        let item = item.map_err(|e| AppError::io("读取站点目录", e))?;
        let name = item.file_name().to_string_lossy().to_string();
        let meta =
            fs::symlink_metadata(item.path()).map_err(|e| AppError::io("读取站点文件信息", e))?;
        if linked(&meta) || (!meta.is_file() && !meta.is_dir()) {
            continue;
        }
        let child = relative.join(&name);
        entries.push(SiteFileEntry {
            name,
            path: display_path(&child),
            directory: meta.is_dir(),
            size_bytes: if meta.is_file() { meta.len() } else { 0 },
            modified_at: modified_at(&meta),
        });
    }
    entries.sort_by(|a, b| {
        (!a.directory, a.name.to_ascii_lowercase())
            .cmp(&(!b.directory, b.name.to_ascii_lowercase()))
    });
    let parent = relative.parent().map(display_path);
    Ok(SiteDirectory {
        site_id: id.to_string(),
        root: display_path(&root),
        current: display_path(&relative),
        parent,
        revision: directory_revision(&entries),
        entries,
    })
}

pub fn read(store: &Store, id: &str, path: &str) -> Result<SiteTextFile> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let root = root(store, id)?;
    let relative = relative(Some(path))?;
    if relative.as_os_str().is_empty() {
        return Err(AppError::new("SITE_FILE_NOT_TEXT", "请选择一个文件"));
    }
    let target = checked_path(&root, &relative)?;
    let bytes = read_text(&target)?;
    let content = String::from_utf8(bytes.clone())
        .map_err(|_| AppError::new("SITE_FILE_NOT_TEXT", "文件不是 UTF-8 文本"))?;
    Ok(SiteTextFile {
        site_id: id.to_string(),
        path: display_path(&relative),
        size_bytes: bytes.len() as u64,
        revision: file_revision(&root, &relative, &bytes),
        content,
    })
}

pub fn write(
    store: &Store,
    id: &str,
    path: &str,
    content: &str,
    expected_revision: &str,
) -> Result<SiteTextFile> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    if content.len() as u64 > MAX_TEXT_BYTES {
        return Err(AppError::new(
            "SITE_FILE_TOO_LARGE",
            "文件超过 1 MiB，不能保存",
        ));
    }
    if content.contains('\0') {
        return Err(AppError::new(
            "SITE_FILE_NOT_TEXT",
            "文件内容不能包含二进制字符",
        ));
    }
    let root = root(store, id)?;
    let relative = relative(Some(path))?;
    if relative.as_os_str().is_empty() || sensitive_name(&relative) {
        return Err(AppError::new(
            "SITE_FILE_SENSITIVE",
            "环境变量和密钥文件请使用专用编辑器编辑",
        ));
    }
    let target = checked_path(&root, &relative)?;
    let previous = read_text(&target)?;
    if file_revision(&root, &relative, &previous) != expected_revision {
        return Err(AppError::new(
            "SITE_FILE_CHANGED",
            "文件已在其他位置发生变化，请重新打开后再保存",
        ));
    }
    std::str::from_utf8(&previous)
        .map_err(|_| AppError::new("SITE_FILE_NOT_TEXT", "文件不是 UTF-8 文本"))?;
    let metadata = fs::metadata(&target).map_err(|e| AppError::io("读取站点文件权限", e))?;
    if metadata.permissions().readonly() {
        return Err(AppError::new(
            "SITE_FILE_READ_ONLY",
            "文件为只读，请调整权限后重试",
        ));
    }
    // 沿用环境编辑器的临时文件原子替换方式，并保留源文件权限。
    let mut pending = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
    pending.write_all(content.as_bytes())?;
    pending.as_file().set_permissions(metadata.permissions())?;
    pending.as_file().sync_all()?;
    checked_path(&root, &relative)?;
    if read_text(&target)? != previous {
        return Err(AppError::new(
            "SITE_FILE_CHANGED",
            "文件已在其他位置发生变化，请重新打开后再保存",
        ));
    }
    pending
        .persist(&target)
        .map_err(|e| AppError::io("保存站点文件", e.error))?;
    Ok(SiteTextFile {
        site_id: id.to_string(),
        path: display_path(&relative),
        size_bytes: content.len() as u64,
        revision: file_revision(&root, &relative, content.as_bytes()),
        content: content.to_string(),
    })
}

pub fn delete(
    store: &Store,
    id: &str,
    path: &str,
    confirmation: &str,
) -> Result<SiteFileDeleteReceipt> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let root = root(store, id)?;
    let relative = relative(Some(path))?;
    if relative.as_os_str().is_empty() {
        return Err(AppError::new("SITE_FILE_DELETE_INVALID", "不能删除站点根目录"));
    }
    if sensitive_name(&relative) {
        return Err(AppError::new(
            "SITE_FILE_SENSITIVE",
            "环境变量和密钥文件请使用专用编辑器或系统文件管理器管理",
        ));
    }
    let display = display_path(&relative);
    if confirmation != display {
        return Err(AppError::new(
            "SITE_FILE_DELETE_CONFIRM_REQUIRED",
            "请输入完整相对路径以确认删除",
        ));
    }
    let target = checked_path(&root, &relative)?;
    let metadata = fs::symlink_metadata(&target).map_err(|e| AppError::io("读取站点文件", e))?;
    if linked(&metadata) || (!metadata.is_file() && !metadata.is_dir()) {
        return Err(AppError::new(
            "SITE_FILE_DELETE_INVALID",
            "只能删除站点目录中的普通文件或空目录",
        ));
    }
    let directory = metadata.is_dir();
    if directory {
        let mut entries = fs::read_dir(&target).map_err(|e| AppError::io("读取站点目录", e))?;
        if entries.next().transpose().map_err(|e| AppError::io("检查目录内容", e))?.is_some() {
            return Err(AppError::new(
                "SITE_FILE_DIRECTORY_NOT_EMPTY",
                "目录不为空，请先删除或移动其中的文件",
            ));
        }
        fs::remove_dir(&target).map_err(|e| AppError::io("删除站点目录", e))?;
    } else {
        fs::remove_file(&target).map_err(|e| AppError::io("删除站点文件", e))?;
    }
    Ok(SiteFileDeleteReceipt { site_id: id.to_string(), path: display, directory })
}

pub fn create(store: &Store, id: &str, path: &str, directory: bool) -> Result<SiteFileCreateReceipt> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let root = root(store, id)?;
    let relative = relative(Some(path))?;
    if relative.as_os_str().is_empty() || sensitive_name(&relative) {
        return Err(AppError::new("SITE_FILE_SENSITIVE", "环境变量和密钥文件请使用专用编辑器或系统文件管理器管理"));
    }
    let target = checked_new_path(&root, &relative)?;
    if directory {
        fs::create_dir(&target).map_err(|e| AppError::io("创建站点目录", e))?;
    } else {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| AppError::io("创建站点文件", e))?;
    }
    Ok(SiteFileCreateReceipt { site_id: id.to_string(), path: display_path(&relative), directory })
}

pub fn rename(store: &Store, id: &str, path: &str, new_path: &str) -> Result<SiteFileRenameReceipt> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let root = root(store, id)?;
    let source_relative = relative(Some(path))?;
    let target_relative = relative(Some(new_path))?;
    if source_relative.as_os_str().is_empty() || target_relative.as_os_str().is_empty() || sensitive_name(&source_relative) || sensitive_name(&target_relative) {
        return Err(AppError::new("SITE_FILE_RENAME_INVALID", "不能重命名站点根目录、敏感文件或空路径"));
    }
    if source_relative == target_relative {
        return Err(AppError::new("SITE_FILE_RENAME_INVALID", "新旧名称没有变化"));
    }
    let source = checked_path(&root, &source_relative)?;
    let source_meta = fs::symlink_metadata(&source).map_err(|e| AppError::io("读取原文件", e))?;
    if linked(&source_meta) || (!source_meta.is_file() && !source_meta.is_dir()) {
        return Err(AppError::new("SITE_FILE_RENAME_INVALID", "只能重命名普通文件或目录"));
    }
    let target = checked_new_path(&root, &target_relative)?;
    if source_meta.is_dir() {
        let source_actual = source.canonicalize().map_err(|e| AppError::io("解析原目录", e))?;
        let target_parent = target.parent().unwrap_or(&root).canonicalize().map_err(|e| AppError::io("解析目标目录", e))?;
        if target_parent.starts_with(&source_actual) {
            return Err(AppError::new("SITE_FILE_RENAME_INVALID", "目录不能移动到自身或子目录中"));
        }
    }
    fs::rename(&source, &target).map_err(|e| AppError::io("重命名站点文件", e))?;
    Ok(SiteFileRenameReceipt { site_id: id.to_string(), from: display_path(&source_relative), path: display_path(&target_relative), directory: source_meta.is_dir() })
}

pub fn upload(store: &Store, id: &str, source: &str, path: &str) -> Result<SiteFileUploadReceipt> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let root = root(store, id)?;
    let relative = relative(Some(path))?;
    if relative.as_os_str().is_empty() || sensitive_name(&relative) {
        return Err(AppError::new("SITE_FILE_SENSITIVE", "环境变量和密钥文件请使用专用编辑器或系统文件管理器管理"));
    }
    let source_path = Path::new(source);
    if source.trim().is_empty() {
        return Err(AppError::new("SITE_FILE_UPLOAD_INVALID", "请选择要上传的文件"));
    }
    let source_meta = fs::symlink_metadata(source_path).map_err(|e| AppError::io("读取待上传文件", e))?;
    if linked(&source_meta) || !source_meta.is_file() {
        return Err(AppError::new("SITE_FILE_UPLOAD_INVALID", "只能上传普通文件，不能上传目录或符号链接"));
    }
    if source_meta.len() > MAX_UPLOAD_BYTES {
        return Err(AppError::new("SITE_FILE_UPLOAD_TOO_LARGE", "上传文件不能超过 64 MiB"));
    }
    let target = checked_new_path(&root, &relative)?;
    let input = fs::File::open(source_path).map_err(|e| AppError::io("打开待上传文件", e))?;
    let mut pending = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
    let copied = std::io::copy(&mut input.take(MAX_UPLOAD_BYTES + 1), pending.as_file_mut())?;
    if copied > MAX_UPLOAD_BYTES {
        return Err(AppError::new("SITE_FILE_UPLOAD_TOO_LARGE", "上传文件不能超过 64 MiB"));
    }
    pending.as_file().sync_all()?;
    pending
        .persist(&target)
        .map_err(|e| AppError::io("保存上传文件", e.error))?;
    Ok(SiteFileUploadReceipt { site_id: id.to_string(), path: display_path(&relative), size_bytes: copied })
}

/// 将站点目录中的普通文件导出到用户选择的目标位置。
///
/// 导出不覆盖已有文件，源文件和目标父目录都拒绝符号链接，避免文件管理器
/// 操作被重定向到站点目录之外；复制期间持有站点变更锁，保证源文件不会被
/// 本应用的其它文件操作同时替换。
pub fn download(store: &Store, id: &str, path: &str, destination: &str) -> Result<SiteFileDownloadReceipt> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let root = root(store, id)?;
    let relative = relative(Some(path))?;
    if relative.as_os_str().is_empty() || sensitive_name(&relative) {
        return Err(AppError::new(
            "SITE_FILE_SENSITIVE",
            "环境变量和密钥文件请使用专用编辑器或系统文件管理器导出",
        ));
    }
    let source = checked_path(&root, &relative)?;
    let source_meta = fs::symlink_metadata(&source).map_err(|e| AppError::io("读取站点文件", e))?;
    if linked(&source_meta) || !source_meta.is_file() {
        return Err(AppError::new("SITE_FILE_DOWNLOAD_INVALID", "只能导出站点目录中的普通文件"));
    }
    if source_meta.len() > MAX_UPLOAD_BYTES {
        return Err(AppError::new("SITE_FILE_DOWNLOAD_TOO_LARGE", "文件超过 64 MiB，暂不支持在应用内导出"));
    }
    if destination.trim().is_empty() || destination.chars().any(char::is_control) {
        return Err(AppError::new("SITE_FILE_DEST_INVALID", "请选择有效的导出位置"));
    }
    let target = PathBuf::from(destination);
    if !target.is_absolute() {
        return Err(AppError::new("SITE_FILE_DEST_INVALID", "导出位置必须是绝对路径"));
    }
    let parent = target.parent().filter(|value| !value.as_os_str().is_empty()).ok_or_else(|| {
        AppError::new("SITE_FILE_DEST_INVALID", "导出位置缺少目标目录")
    })?;
    let parent_meta = fs::symlink_metadata(parent).map_err(|e| AppError::io("读取导出目录", e))?;
    let mut ancestor = Some(parent);
    while let Some(path) = ancestor {
        let meta = fs::symlink_metadata(path).map_err(|e| AppError::io("读取导出目录", e))?;
        if linked(&meta) {
            return Err(AppError::new("SITE_FILE_DEST_INVALID", "导出目录不能经过符号链接或目录联接"));
        }
        ancestor = path.parent();
    }
    if !parent_meta.is_dir() {
        return Err(AppError::new("SITE_FILE_DEST_INVALID", "导出目录不是可用的普通目录"));
    }
    let source_actual = source.canonicalize().map_err(|e| AppError::io("解析站点文件", e))?;
    if target.canonicalize().is_ok_and(|actual| actual == source_actual) {
        return Err(AppError::new("SITE_FILE_DEST_SAME", "导出位置不能是站点原文件，请另选文件名"));
    }
    if fs::symlink_metadata(&target).is_ok() {
        return Err(AppError::new("SITE_FILE_DEST_EXISTS", "目标文件已存在，请另选文件名"));
    }
    let mut input = fs::File::open(&source).map_err(|e| AppError::io("读取站点文件", e))?;
    let mut pending = tempfile::NamedTempFile::new_in(parent).map_err(|e| AppError::io("创建导出文件", e))?;
    let copied = std::io::copy(&mut input, pending.as_file_mut()).map_err(|e| AppError::io("复制站点文件", e))?;
    if copied != source_meta.len() {
        return Err(AppError::new("SITE_FILE_CHANGED", "导出期间源文件发生变化，请重试"));
    }
    pending.as_file().sync_all().map_err(|e| AppError::io("保存导出文件", e))?;
    pending.persist_noclobber(&target).map_err(|e| {
        if e.error.kind() == std::io::ErrorKind::AlreadyExists {
            AppError::new("SITE_FILE_DEST_EXISTS", "目标文件已存在，请另选文件名")
        } else {
            AppError::io("保存导出文件", e.error)
        }
    })?;
    Ok(SiteFileDownloadReceipt {
        site_id: id.to_string(),
        path: display_path(&relative),
        destination: target.to_string_lossy().into_owned(),
        size_bytes: copied,
    })
}
