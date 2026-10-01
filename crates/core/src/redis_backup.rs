//! Redis 的独立 RDB 副本与停止实例恢复；不替换 AOF，也不把替换文件等同于已加载。
use crate::{
    error::{AppError, Result},
    paths::{checked_data_path, Paths},
    store::Store,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RedisBackup {
    pub id: String,
    pub version: String,
    pub created_at: i64,
    pub size_bytes: u64,
    pub sha256: String,
    pub kind: String,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisBackupList {
    pub items: Vec<RedisBackupEntry>,
    pub unreadable: usize,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub directory: String,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisBackupEntry {
    pub id: String,
    pub version: Option<String>,
    pub created_at: Option<i64>,
    pub size_bytes: Option<u64>,
    pub kind: Option<String>,
    pub problem: Option<String>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisBackupRemoval {
    pub entry: RedisBackupEntry,
    pub revision: String,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisRestorePreview {
    pub backup: RedisBackup,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub target: String,
    pub existing_size: Option<u64>,
    pub revision: String,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisRestoreResult {
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub target: String,
    pub safety_backup: Option<RedisBackup>,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RedisImportPreview {
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub source: String,
    pub version: String,
    pub rdb_version: u16,
    pub size_bytes: u64,
    pub sha256: String,
    pub revision: String,
}

fn invalid(message: &str) -> AppError {
    AppError::new("REDIS_BACKUP_INVALID", message)
}

pub fn directory(paths: &Paths) -> Result<PathBuf> {
    Ok(checked_data_path(&paths.base, "backup/redis")?)
}

fn item_path(paths: &Paths, id: &str, file: &str) -> Result<PathBuf> {
    if id.is_empty()
        || id.len() > 80
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
    {
        return Err(invalid("备份标识无效"));
    }
    Ok(checked_data_path(
        &paths.base,
        &format!("backup/redis/{id}/{file}"),
    )?)
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

fn open_plain(path: &Path) -> Result<File> {
    let before = fs::symlink_metadata(path)?;
    if linked(&before) || !before.is_file() {
        return Err(invalid("仅支持无链接的普通 RDB 文件"));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(5).custom_flags(0x00200000);
    }
    let file = options.open(path)?;
    let actual = file.metadata()?;
    if linked(&actual)
        || !actual.is_file()
        || before.len() != actual.len()
        || before.modified()? != actual.modified()?
    {
        return Err(invalid("文件在打开时发生变化，请重新检查"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != actual.dev() || before.ino() != actual.ino() {
            return Err(invalid("文件在打开时被替换"));
        }
    }
    Ok(file)
}

/// 流式复制并计算摘要；Redis 完成的 RDB 不会原地重写，打开的文件不随后续 rename 切换。
fn copy_rdb(path: &Path, mut output: impl Write) -> Result<(u64, String)> {
    copy_file(path, &mut output, true)
}

// Redis RDB 的 CRC64/Jones：反射多项式、初始值 0；按规范生成表，不依赖外部校验程序。
// 规范及检查向量：https://github.com/redis/redis/blob/5.0/src/crc64.c
pub(crate) fn rdb_checksum(mut value: u64, bytes: &[u8]) -> u64 {
    static TABLE: once_cell::sync::Lazy<[u64; 256]> = once_cell::sync::Lazy::new(|| {
        let mut table = [0; 256];
        for (index, entry) in table.iter_mut().enumerate() {
            let mut n = index as u64;
            for _ in 0..8 {
                n = if n & 1 != 0 {
                    (n >> 1) ^ 0x95ac9329ac4bc9b5
                } else {
                    n >> 1
                };
            }
            *entry = n;
        }
        table
    });
    for byte in bytes {
        value = TABLE[((value as u8) ^ byte) as usize] ^ (value >> 8);
    }
    value
}

fn copy_file(path: &Path, mut output: impl Write, require_rdb: bool) -> Result<(u64, String)> {
    copy_file_with_prefix(path, &mut output, require_rdb, None)
}

fn copy_file_with_prefix(
    path: &Path,
    mut output: impl Write,
    require_rdb: bool,
    mut prefix: Option<&mut Vec<u8>>,
) -> Result<(u64, String)> {
    let mut file = open_plain(path)?;
    let before = file.metadata()?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut crc = 0;
    let mut trailer = Vec::new();
    let mut last_payload = 0;
    if require_rdb {
        if before.len() < 18 {
            return Err(invalid("RDB 文件为空或过短"));
        }
        let mut header = [0; 9];
        file.read_exact(&mut header)?;
        if &header[..5] != b"REDIS" || !header[5..].iter().all(u8::is_ascii_digit) {
            return Err(invalid("文件不是有效的 RDB 快照头"));
        }
        hash.update(header);
        if let Some(bytes) = prefix.as_deref_mut() {
            bytes.extend_from_slice(&header);
        }
        crc = rdb_checksum(crc, &header);
        output.write_all(&header)?;
        size = 9;
    }
    let mut buffer = [0; 128 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if require_rdb {
            let payload = (before.len() - 8).saturating_sub(size).min(n as u64) as usize;
            crc = rdb_checksum(crc, &buffer[..payload]);
            if payload > 0 {
                last_payload = buffer[payload - 1];
            }
            trailer.extend_from_slice(&buffer[payload..n]);
            if trailer.len() > 8 {
                return Err(invalid("RDB 文件在复制时增长，未发布副本"));
            }
        }
        size += n as u64;
        if let Some(bytes) = prefix.as_deref_mut() {
            let count = n.min((64 * 1024usize).saturating_sub(bytes.len()));
            bytes.extend_from_slice(&buffer[..count]);
        }
        hash.update(&buffer[..n]);
        output.write_all(&buffer[..n])?;
    }
    let after = file.metadata()?;
    if before.len() != size || after.len() != size || before.modified()? != after.modified()? {
        return Err(invalid("RDB 文件在复制时发生变化，未发布副本"));
    }
    if require_rdb {
        let bytes: [u8; 8] = trailer
            .try_into()
            .map_err(|_| invalid("RDB 校验尾部不完整"))?;
        let expected = u64::from_le_bytes(bytes);
        // rdbchecksum=no 时 Redis 写入 0；这种文件仍计算并记录独立的 SHA-256。
        if last_payload != 0xff || (expected != 0 && expected != crc) {
            return Err(AppError::new(
                "REDIS_BACKUP_CHECKSUM",
                "RDB 结束标记或 CRC64 校验不符，未发布或恢复文件",
            ));
        }
    }
    Ok((size, hex::encode(hash.finalize())))
}

fn metadata(paths: &Paths, id: &str) -> Result<RedisBackup> {
    let path = item_path(paths, id, "metadata.json")?;
    let file = open_plain(&path)?;
    if file.metadata()?.len() > 8192 {
        return Err(invalid("备份记录过大"));
    }
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(invalid("备份记录过大"));
    }
    let entry: RedisBackup = serde_json::from_slice(&bytes).map_err(|_| invalid("备份记录损坏"))?;
    if entry.id != id
        || entry.created_at <= 0
        || entry.version.is_empty()
        || entry.version.len() > 128
        || !matches!(
            entry.kind.as_str(),
            "snapshot" | "before-restore" | "imported"
        )
        || entry.sha256.len() != 64
        || !entry.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(invalid("备份记录字段无效"));
    }
    Ok(entry)
}

fn verified(paths: &Paths, id: &str) -> Result<RedisBackup> {
    let entry = metadata(paths, id)?;
    let (size, hash) = copy_rdb(&item_path(paths, id, "content.rdb")?, std::io::sink())?;
    if entry.size_bytes != size || entry.sha256 != hash {
        return Err(AppError::new(
            "REDIS_BACKUP_CHECKSUM",
            "备份文件校验不符，未执行恢复",
        ));
    }
    Ok(entry)
}

fn summary(paths: &Paths, id: &str) -> Result<RedisBackupEntry> {
    let path = item_path(paths, id, "content.rdb")?;
    let dir = path.parent().unwrap();
    if !fs::symlink_metadata(dir)?.is_dir() {
        return Err(invalid("备份路径不是目录"));
    }
    let record = metadata(paths, id);
    let mut entry = match record {
        Ok(record) => RedisBackupEntry {
            id: id.into(),
            version: Some(record.version),
            created_at: Some(record.created_at),
            size_bytes: Some(record.size_bytes),
            kind: Some(record.kind),
            problem: None,
        },
        Err(error) => RedisBackupEntry {
            id: id.into(),
            version: None,
            created_at: None,
            size_bytes: None,
            kind: None,
            problem: Some(format!("备份记录无法读取：{}", error.message)),
        },
    };
    match fs::symlink_metadata(&path) {
        Ok(meta) if !linked(&meta) && meta.is_file() => {
            if entry.size_bytes.is_some_and(|size| size != meta.len()) {
                entry.problem = Some("RDB 文件大小与备份记录不符，请保留文件并检查其他副本".into());
            }
            entry.size_bytes = Some(meta.len());
        }
        Ok(_) => entry.problem = Some("RDB 路径不是普通文件，无法恢复或导出".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            entry.size_bytes = None;
            entry.problem = Some("RDB 文件已缺失，仅剩备份记录".into());
        }
        Err(_) => entry.problem = Some("RDB 文件无法读取，请检查权限或文件占用".into()),
    }
    Ok(entry)
}

pub fn list(paths: &Paths) -> Result<RedisBackupList> {
    let dir = directory(paths)?;
    fs::create_dir_all(&dir)?;
    let mut result = RedisBackupList {
        items: Vec::new(),
        unreadable: 0,
        directory: crate::paths::portable_path_text(&dir),
    };
    for item in fs::read_dir(&dir)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        match summary(paths, &name) {
            Ok(entry) => result.items.push(entry),
            Err(_) => result.unreadable += 1,
        }
    }
    result.items.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(result)
}

/// 只核对两个受管文件；目录中有其他内容时不允许自动清理。
fn removal_revision(dir: &Path, id: &str) -> Result<String> {
    let metadata = fs::symlink_metadata(dir)?;
    if linked(&metadata) || !metadata.is_dir() {
        return Err(invalid("备份目录不能是链接或普通文件"));
    }
    for item in fs::read_dir(dir)? {
        let name = item?.file_name();
        if name != "content.rdb" && name != "metadata.json" {
            return Err(AppError::new(
                "REDIS_BACKUP_EXTRA_FILES",
                "备份目录中包含其他文件，未执行删除",
            )
            .with_hint("请打开备份目录检查额外文件；此操作只管理 content.rdb 与 metadata.json"));
        }
    }
    let content = existing(&dir.join("content.rdb"))?;
    let record = existing(&dir.join("metadata.json"))?;
    Ok(hex::encode(Sha256::digest(
        serde_json::to_vec(&(id, content, record)).map_err(|_| invalid("备份范围序列化失败"))?,
    )))
}

pub fn removal_preview(paths: &Paths, id: &str) -> Result<RedisBackupRemoval> {
    let dir = item_path(paths, id, "content.rdb")?
        .parent()
        .unwrap()
        .to_path_buf();
    let revision = removal_revision(&dir, id)?;
    let entry = summary(paths, id)?;
    // 读取摘要期间文件可能变化，确认弹窗必须对应同一份内容。
    if removal_revision(&dir, id)? != revision {
        return Err(AppError::new(
            "REDIS_BACKUP_CHANGED",
            "备份已变化，请重新检查删除范围",
        ));
    }
    Ok(RedisBackupRemoval { entry, revision })
}

/// 调用方持有服务生命周期锁，防止删除与应用内恢复/导出交错。
pub(crate) fn remove(paths: &Paths, id: &str, revision: &str) -> Result<()> {
    let _work = crate::BackgroundWork::begin("删除 Redis 备份副本")?;
    let source = item_path(paths, id, "content.rdb")?
        .parent()
        .unwrap()
        .to_path_buf();
    if removal_revision(&source, id)? != revision {
        return Err(AppError::new(
            "REDIS_BACKUP_CHANGED",
            "备份已变化，未删除，请重新检查并确认",
        ));
    }
    let staged = directory(paths)?.join(format!(".delete-{id}-{:016x}", rand::random::<u64>()));
    if staged.exists() {
        return Err(invalid("删除暂存目录冲突，请重试"));
    }
    fs::rename(&source, &staged)?;
    let validation = removal_revision(&staged, id).and_then(|current| {
        if current == revision {
            Ok(())
        } else {
            Err(AppError::new(
                "REDIS_BACKUP_CHANGED",
                "备份在删除前发生变化，已中止",
            ))
        }
    });
    if let Err(error) = validation {
        if source.exists() || fs::rename(&staged, &source).is_err() {
            return Err(error.with_hint(format!(
                "文件保留在 {}，请打开备份目录检查",
                crate::paths::portable_path_text(&staged)
            )));
        }
        return Err(error);
    }
    // 不递归删除；额外文件、链接或外部进程造成的变化不应扩展删除范围。
    let cleanup = || -> std::io::Result<()> {
        for name in ["content.rdb", "metadata.json"] {
            match fs::remove_file(staged.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        fs::remove_dir(&staged)
    };
    cleanup().map_err(|error| {
        let retained = if !source.exists() && fs::rename(&staged, &source).is_ok() {
            &source
        } else {
            &staged
        };
        AppError::io("清理备份副本", error).with_hint(format!(
            "清理未全部完成，剩余内容保留在 {}；当前 Redis 数据未改动",
            crate::paths::portable_path_text(retained)
        ))
    })
}

pub(crate) fn export(paths: &Paths, id: &str, destination: &str) -> Result<String> {
    let _work = crate::BackgroundWork::begin("导出 Redis RDB 副本")?;
    let dest = Path::new(destination);
    if !dest.is_absolute()
        || dest
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(invalid("请选择完整的导出文件路径"));
    }
    if !dest
        .extension()
        .and_then(|part| part.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("rdb"))
    {
        return Err(invalid("导出文件名必须以 .rdb 结尾"));
    }
    let parent = dest.parent().ok_or_else(|| invalid("导出目录无效"))?;
    for part in parent.ancestors() {
        if linked(&fs::symlink_metadata(part)?) {
            return Err(invalid("导出目录不能经过链接或目录联接，请选择原始目录"));
        }
    }
    let parent = parent.canonicalize()?;
    let base = paths.base.canonicalize()?;
    #[cfg(windows)]
    let inside = Path::new(&parent.to_string_lossy().to_lowercase())
        .starts_with(base.to_string_lossy().to_lowercase());
    #[cfg(not(windows))]
    let inside = parent.starts_with(&base);
    if inside {
        return Err(invalid("请选择 NiceEnv 数据目录以外的位置保存导出文件"));
    }
    let dest = checked_data_path(
        &parent,
        dest.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid("导出文件名无效"))?,
    )?;
    if dest.exists() {
        return Err(AppError::new(
            "REDIS_EXPORT_EXISTS",
            "目标文件已存在，请另选文件名；已有文件未覆盖",
        ));
    }
    let record = metadata(paths, id)?;
    let mut pending = tempfile::Builder::new()
        .prefix(".redis-export-")
        .tempfile_in(&parent)?;
    let (size, hash) = copy_rdb(&item_path(paths, id, "content.rdb")?, &mut pending)?;
    if record.size_bytes != size || record.sha256 != hash || record != metadata(paths, id)? {
        return Err(AppError::new(
            "REDIS_BACKUP_CHECKSUM",
            "备份与记录不符，未导出文件，请检查备份副本",
        ));
    }
    pending.as_file().sync_all()?;
    pending
        .persist_noclobber(&dest)
        .map_err(|error| AppError::io("保存导出 RDB（不覆盖已有文件）", error.error))?;
    Ok(crate::paths::portable_path_text(&dest))
}

pub fn archive(paths: &Paths, version: &str, source: &Path, kind: &str) -> Result<RedisBackup> {
    archive_checked(paths, version, source, kind, None)
}

fn archive_checked(
    paths: &Paths,
    version: &str,
    source: &Path,
    kind: &str,
    expected: Option<&str>,
) -> Result<RedisBackup> {
    let dir = directory(paths)?;
    fs::create_dir_all(&dir)?;
    let id = format!(
        "{}-{:016x}",
        chrono::Utc::now().timestamp_millis(),
        rand::random::<u64>()
    );
    let pending = tempfile::Builder::new()
        .prefix(".pending-")
        .tempdir_in(&dir)?;
    let mut content = File::create(pending.path().join("content.rdb"))?;
    let (size_bytes, sha256) = copy_file(source, &mut content, kind != "before-restore")?;
    if expected.is_some_and(|hash| hash != sha256) {
        return Err(AppError::new(
            "REDIS_IMPORT_CHANGED",
            "源 RDB 在导入期间发生变化，未保存备份，请重新选择",
        ));
    }
    content.sync_all()?;
    drop(content);
    let entry = RedisBackup {
        id: id.clone(),
        version: version.into(),
        created_at: chrono::Utc::now().timestamp_millis(),
        size_bytes,
        sha256,
        kind: kind.into(),
    };
    let mut record = File::create(pending.path().join("metadata.json"))?;
    record.write_all(&serde_json::to_vec(&entry).map_err(|_| invalid("备份记录序列化失败"))?)?;
    record.sync_all()?;
    drop(record);
    let destination = item_path(paths, &id, "content.rdb")?
        .parent()
        .unwrap()
        .to_path_buf();
    if destination.exists() {
        return Err(invalid("备份名称冲突，未覆盖已有副本"));
    }
    fs::rename(pending.path(), &destination)?;
    Ok(entry)
}

fn import_source(source: &str) -> Result<PathBuf> {
    let path = Path::new(source);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(invalid("请选择本机 RDB 文件的完整路径"));
    }
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("rdb"))
    {
        return Err(invalid("请选择 .rdb 快照文件"));
    }
    for part in path.ancestors() {
        if linked(&fs::symlink_metadata(part)?) {
            return Err(invalid(
                "导入路径不能经过符号链接或目录联接，请选择原始文件",
            ));
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("文件路径无效"))?
        .canonicalize()?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("文件名无效"))?;
    Ok(checked_data_path(&parent, name)?)
}

/// 仅读取开头 AUX 元信息，不解释键值对象或执行任何 Redis 指令。
/// 编码依据 Redis RDB 长度/整数规范；总读取范围限制在已校验文件的前 64 KiB。
fn source_version(prefix: &[u8]) -> Result<(u16, String)> {
    let unknown = || {
        AppError::new("REDIS_IMPORT_VERSION", "无法从此 RDB 自动识别来源 Redis 版本")
        .with_hint("请选择由 Redis 生成、带 redis-ver 元信息的完整 RDB；旧格式、压缩版本字段或不明确的版本不支持自动导入")
    };
    fn take<'a>(data: &mut &'a [u8], count: usize) -> Option<&'a [u8]> {
        if count > data.len() {
            return None;
        }
        let (head, tail) = data.split_at(count);
        *data = tail;
        Some(head)
    }
    fn string(data: &mut &[u8]) -> Option<Vec<u8>> {
        let first = *take(data, 1)?.first()?;
        let length = match first >> 6 {
            0 => u64::from(first),
            1 => (u64::from(first & 63) << 8) | u64::from(take(data, 1)?[0]),
            2 if first == 0x80 => u64::from(u32::from_be_bytes(take(data, 4)?.try_into().ok()?)),
            2 if first == 0x81 => u64::from_be_bytes(take(data, 8)?.try_into().ok()?),
            3 => {
                let value = match first & 63 {
                    0 => i64::from(take(data, 1)?[0] as i8),
                    1 => i64::from(i16::from_le_bytes(take(data, 2)?.try_into().ok()?)),
                    2 => i64::from(i32::from_le_bytes(take(data, 4)?.try_into().ok()?)),
                    _ => return None,
                };
                return Some(value.to_string().into_bytes());
            }
            _ => return None,
        };
        if length > 4096 {
            return None;
        }
        Some(take(data, length as usize)?.to_vec())
    }
    let header = prefix.get(..9).ok_or_else(unknown)?;
    let format: u16 = std::str::from_utf8(&header[5..])
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(unknown)?;
    let mut data = &prefix[9..];
    for _ in 0..64 {
        if data.first() != Some(&0xfa) {
            break;
        }
        data = &data[1..];
        let key = string(&mut data).ok_or_else(unknown)?;
        let value = string(&mut data).ok_or_else(unknown)?;
        if key == b"redis-ver" {
            let value = String::from_utf8(value).map_err(|_| unknown())?;
            let parts: Vec<_> = value.split('.').collect();
            if !(3..=4).contains(&parts.len())
                || parts.iter().any(|part| {
                    part.is_empty() || part.len() > 5 || !part.bytes().all(|c| c.is_ascii_digit())
                })
            {
                return Err(unknown());
            }
            return Ok((format, value));
        }
    }
    Err(unknown())
}

pub fn inspect_import(source: &str) -> Result<RedisImportPreview> {
    let _work = crate::BackgroundWork::begin("检查外部 Redis RDB")?;
    let path = import_source(source)?;
    let mut prefix = Vec::new();
    let (size_bytes, sha256) =
        copy_file_with_prefix(&path, std::io::sink(), true, Some(&mut prefix))?;
    let (rdb_version, version) = source_version(&prefix)?;
    let source = crate::paths::portable_path_text(&path);
    let revision = hex::encode(Sha256::digest(
        serde_json::to_vec(&(&source, &version, rdb_version, size_bytes, &sha256))
            .map_err(|_| invalid("导入范围序列化失败"))?,
    ));
    Ok(RedisImportPreview {
        source,
        version,
        rdb_version,
        size_bytes,
        sha256,
        revision,
    })
}

pub fn import_rdb(paths: &Paths, source: &str, revision: &str) -> Result<RedisBackup> {
    let _work = crate::BackgroundWork::begin("导入外部 Redis RDB")?;
    let preview = inspect_import(source)?;
    if preview.revision != revision {
        return Err(AppError::new(
            "REDIS_IMPORT_CHANGED",
            "源文件与预览不一致，请重新选择并确认",
        ));
    }
    let path = import_source(&preview.source)?;
    archive_checked(
        paths,
        &preview.version,
        &path,
        "imported",
        Some(&preview.sha256),
    )
}

fn target(paths: &Paths, store: &Store, version: &str) -> Result<(PathBuf, String)> {
    store
        .find_installed("redis", Some(version))
        .ok_or_else(|| AppError::not_installed("Redis"))?;
    let path = checked_data_path(&paths.base, &format!("etc/redis/{version}/redis.conf"))?;
    let mut content = String::new();
    open_plain(&path)?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut content)?;
    if content.len() > 1024 * 1024 {
        return Err(invalid("Redis 配置超过 1 MiB"));
    }
    let (_, aof) = crate::redis_settings::parse(&content)?;
    if aof == Some(true) {
        return Err(AppError::new(
            "REDIS_RESTORE_AOF",
            "此实例启用 AOF，单独替换 RDB 不会恢复预期数据",
        )
        .with_hint("AOF 实例需要整体恢复持久化文件；本面板仅支持 RDB 模式恢复，不会自动关闭 AOF"));
    }
    let mut filename = "dump.rdb".to_string();
    for line in content
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
    {
        let key = line.split_ascii_whitespace().next().unwrap_or_default();
        if (key.eq_ignore_ascii_case("preload-file")
            && !matches!(line[key.len()..].trim(), "\"\"" | "''"))
            || key.eq_ignore_ascii_case("replicaof")
            || key.eq_ignore_ascii_case("slaveof")
        {
            return Err(AppError::new("REDIS_RESTORE_SOURCE", "启动配置指定了预载文件或上游复制，无法保证启动时使用恢复的 RDB")
                .with_hint("请在配置编辑器核对 preload-file / replicaof / slaveof；此面板不会自动修改复制关系或启动数据来源"));
        }
        if !key.eq_ignore_ascii_case("dbfilename") {
            continue;
        }
        let value = line[key.len()..].trim();
        let value = if value.starts_with(['\'', '"'])
            && value.len() >= 2
            && value.as_bytes()[0] == *value.as_bytes().last().unwrap()
        {
            &value[1..value.len() - 1]
        } else {
            if value.chars().any(char::is_whitespace) {
                return Err(invalid("dbfilename 参数无法准确识别，请检查配置原文"));
            }
            value
        };
        if value.contains(['\'', '"', '/', '\\']) {
            return Err(invalid("dbfilename 必须是单个普通文件名"));
        }
        filename = value.into();
    }
    // 启动配置生成器会固定 dir 为托管 data/redis，与运行时备份的目录核对保持一致。
    Ok((
        checked_data_path(&paths.base, &format!("data/redis/{filename}"))?,
        content,
    ))
}

fn existing(path: &Path) -> Result<Option<(u64, String)>> {
    match fs::symlink_metadata(path) {
        Ok(_) => copy_file(path, std::io::sink(), false).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn preview(
    paths: &Paths,
    store: &Store,
    version: &str,
    id: &str,
) -> Result<RedisRestorePreview> {
    let backup = verified(paths, id)?;
    if !crate::install::same_version(&backup.version, version) {
        return Err(AppError::new(
            "REDIS_RESTORE_VERSION",
            "请切换到生成此备份的 Redis 版本后再恢复，避免 RDB 格式不兼容",
        ));
    }
    let (target, config) = target(paths, store, version)?;
    let old = existing(&target)?;
    let scope =
        serde_json::to_vec(&(&backup, &config, &old)).map_err(|_| invalid("恢复范围序列化失败"))?;
    Ok(RedisRestorePreview {
        backup,
        target: crate::paths::portable_path_text(&target),
        existing_size: old.map(|v| v.0),
        revision: hex::encode(Sha256::digest(scope)),
    })
}

pub fn restore(
    paths: &Paths,
    store: &Store,
    version: &str,
    id: &str,
    revision: &str,
    confirmation: &str,
) -> Result<RedisRestoreResult> {
    if confirmation != format!("Redis {version}") {
        return Err(AppError::new(
            "REDIS_RESTORE_CONFIRM",
            "请输入目标 Redis 名称与版本，确认替换全部逻辑数据库",
        ));
    }
    let before = preview(paths, store, version, id)?;
    if before.revision != revision {
        return Err(AppError::new(
            "REDIS_RESTORE_CHANGED",
            "备份、配置或当前 RDB 已变化，请重新检查恢复范围",
        ));
    }
    let (target, _) = target(paths, store, version)?;
    fs::create_dir_all(target.parent().unwrap())?;
    let mut pending = tempfile::Builder::new()
        .prefix(".restore-")
        .tempfile_in(target.parent().unwrap())?;
    let (size, hash) = copy_rdb(&item_path(paths, id, "content.rdb")?, &mut pending)?;
    if size != before.backup.size_bytes || hash != before.backup.sha256 {
        return Err(invalid("备份在恢复期间发生变化"));
    }
    pending.as_file().sync_all()?;
    let safety_backup = if before.existing_size.is_some() {
        Some(archive(paths, version, &target, "before-restore")?)
    } else {
        None
    };
    if preview(paths, store, version, id)?.revision != revision {
        return Err(AppError::new(
            "REDIS_RESTORE_CHANGED",
            "恢复准备期间文件发生变化，未替换当前 RDB",
        ));
    }
    pending
        .persist(&target)
        .map_err(|e| AppError::io("原子替换 Redis RDB", e.error))?;
    Ok(RedisRestoreResult {
        target: crate::paths::portable_path_text(&target),
        safety_backup,
    })
}
