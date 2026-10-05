//! SFTPGo Bolt 副本的只读检查。格式依据 bbolt v1.5 的 common/{meta,page,bucket}.go；
//! 不直接修改页、空闲列表或事务元数据，写入交给 SFTPGo 自身。

use crate::error::{AppError, Result};
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bucket {
    pub sequence: u64,
    pub values: BTreeMap<Vec<u8>, Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub sequence: u64,
    pub buckets: BTreeMap<Vec<u8>, Bucket>,
}

fn invalid() -> AppError {
    AppError::new(
        "DATA_DIR_SFTPGO_BOLT",
        "SFTPGo Bolt 账号库结构无法完整验证，未切换数据目录",
    )
    .with_hint("原账号库和文件已保留；请先使用原 SFTPGo 版本确认账号库可以正常打开。")
}

fn number(bytes: &[u8], start: usize, size: usize) -> Result<u64> {
    let bytes = bytes
        .get(start..start.checked_add(size).ok_or_else(invalid)?)
        .ok_or_else(invalid)?;
    Ok(bytes
        .iter()
        .enumerate()
        .fold(0, |n, (i, byte)| n | ((*byte as u64) << (8 * i))))
}

#[derive(Clone, Copy)]
struct Meta {
    page_size: u64,
    root: u64,
    sequence: u64,
    hwm: u64,
    txid: u64,
}

fn meta(bytes: &[u8], offset: u64, length: u64) -> Result<Meta> {
    let page_size = number(bytes, 24, 4)?;
    if !(512..=65536).contains(&page_size)
        || !page_size.is_power_of_two()
        || (offset != 0 && offset != page_size)
        || number(bytes, 0, 8)? != offset / page_size
        || number(bytes, 8, 2)? != 4
        || number(bytes, 12, 4)? != 0
        || number(bytes, 16, 4)? != 0xed0cdaed
        || number(bytes, 20, 4)? != 2
    {
        return Err(invalid());
    }
    let checksum = bytes
        .get(16..72)
        .ok_or_else(invalid)?
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ *byte as u64).wrapping_mul(0x100000001b3)
        });
    if checksum != number(bytes, 72, 8)? {
        return Err(invalid());
    }
    let value = Meta {
        page_size,
        root: number(bytes, 32, 8)?,
        sequence: number(bytes, 40, 8)?,
        hwm: number(bytes, 56, 8)?,
        txid: number(bytes, 64, 8)?,
    };
    if value.txid % 2 != offset / page_size
        || value.hwm > length / page_size
        || value.root < 2
        || value.root >= value.hwm
    {
        return Err(invalid());
    }
    Ok(value)
}

struct Reader {
    file: File,
    meta: Meta,
    visited: HashSet<u64>,
    remaining: usize,
}

struct Entry {
    key: Vec<u8>,
    value: Vec<u8>,
    bucket: bool,
}

impl Reader {
    fn page(&mut self, id: u64, depth: usize) -> Result<Vec<Entry>> {
        if depth > 64 || id < 2 || id >= self.meta.hwm {
            return Err(invalid());
        }
        let position = id.checked_mul(self.meta.page_size).ok_or_else(invalid)?;
        self.file.seek(SeekFrom::Start(position))?;
        let mut header = [0; 16];
        self.file.read_exact(&mut header)?;
        let span = number(&header, 12, 4)?.checked_add(1).ok_or_else(invalid)?;
        let end = id.checked_add(span).ok_or_else(invalid)?;
        let size = usize::try_from(span.checked_mul(self.meta.page_size).ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        if number(&header, 0, 8)? != id || end > self.meta.hwm || size > self.remaining {
            return Err(invalid());
        }
        for page in id..end {
            if !self.visited.insert(page) {
                return Err(invalid());
            }
        }
        self.remaining -= size;
        let mut bytes = vec![0; size];
        bytes[..16].copy_from_slice(&header);
        self.file.read_exact(&mut bytes[16..])?;
        self.entries(&bytes, depth)
    }

    fn entries(&mut self, page: &[u8], depth: usize) -> Result<Vec<Entry>> {
        let flags = number(page, 8, 2)?;
        let count = number(page, 10, 2)? as usize;
        if !matches!(flags, 1 | 2) || 16 + count * 16 > page.len() {
            return Err(invalid());
        }
        let mut entries = Vec::new();
        let mut end = 16 + count * 16;
        for i in 0..count {
            let base = 16 + i * 16;
            let leaf = flags == 2;
            let offset = number(page, base + if leaf { 4 } else { 0 }, 4)? as usize;
            let key_size = number(page, base + if leaf { 8 } else { 4 }, 4)? as usize;
            let value_size = if leaf {
                number(page, base + 12, 4)? as usize
            } else {
                0
            };
            let start = base.checked_add(offset).ok_or_else(invalid)?;
            let middle = start.checked_add(key_size).ok_or_else(invalid)?;
            let finish = middle.checked_add(value_size).ok_or_else(invalid)?;
            if start < end || key_size == 0 || finish > page.len() {
                return Err(invalid());
            }
            end = finish;
            let key = page[start..middle].to_vec();
            if leaf {
                let entry_flags = number(page, base, 4)?;
                if entry_flags > 1 {
                    return Err(invalid());
                }
                entries.push(Entry {
                    key,
                    value: page[middle..finish].to_vec(),
                    bucket: entry_flags == 1,
                });
            } else {
                let children = self.page(number(page, base + 8, 8)?, depth + 1)?;
                if children.first().is_none_or(|child| child.key != key) {
                    return Err(invalid());
                }
                entries.extend(children);
            }
        }
        if entries.windows(2).any(|pair| pair[0].key >= pair[1].key) {
            return Err(invalid());
        }
        Ok(entries)
    }
}

pub(crate) fn read(path: &Path) -> Result<Snapshot> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut candidates = Vec::new();
    for offset in std::iter::once(0).chain((9..=16).map(|shift| 1_u64 << shift)) {
        let mut bytes = [0; 80];
        file.seek(SeekFrom::Start(offset))?;
        if file.read_exact(&mut bytes).is_ok() {
            if let Ok(value) = meta(&bytes, offset, length) {
                candidates.push(value);
            }
        }
    }
    let meta = candidates
        .into_iter()
        .max_by_key(|value| value.txid)
        .ok_or_else(invalid)?;
    let mut reader = Reader {
        file,
        meta,
        visited: HashSet::new(),
        remaining: 256 * 1024 * 1024,
    };
    let mut buckets = BTreeMap::new();
    for entry in reader.page(meta.root, 0)? {
        if !entry.bucket || entry.value.len() < 16 {
            return Err(invalid());
        }
        let root = number(&entry.value, 0, 8)?;
        let sequence = number(&entry.value, 8, 8)?;
        let values = if root == 0 {
            let page = &entry.value[16..];
            if number(page, 0, 8)? != 0 || number(page, 8, 2)? != 2 || number(page, 12, 4)? != 0 {
                return Err(invalid());
            }
            reader.entries(page, 0)?
        } else {
            if entry.value.len() != 16 {
                return Err(invalid());
            }
            reader.page(root, 0)?
        };
        let mut map = BTreeMap::new();
        for value in values {
            if value.bucket || map.insert(value.key, value.value).is_some() {
                return Err(invalid());
            }
        }
        if buckets
            .insert(
                entry.key,
                Bucket {
                    sequence,
                    values: map,
                },
            )
            .is_some()
        {
            return Err(invalid());
        }
    }
    Ok(Snapshot {
        sequence: meta.sequence,
        buckets,
    })
}

pub(crate) struct Plan {
    before: Snapshot,
    dump: serde_json::Value,
    pub directories: Vec<std::path::PathBuf>,
    changed: bool,
}

fn field(bucket: &str) -> &'static str {
    match bucket {
        "users" => "/home_dir",
        "groups" => "/user_settings/home_dir",
        _ => "/mapped_path",
    }
}

fn account(bytes: &[u8]) -> Result<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if !value.is_object() {
        return Err(invalid());
    }
    Ok(value)
}

pub(crate) fn plan(path: &Path, rebase: &crate::paths::DataPathRebase) -> Result<Plan> {
    let before = read(path)?;
    let schema = before
        .buckets
        .get(b"db_version".as_slice())
        .and_then(|bucket| bucket.values.get(b"version".as_slice()))
        .ok_or_else(invalid)?;
    // 上游 schemaVersion 没有 json tag，字段名为 Version（备份格式则是 version）。
    if account(schema)?["Version"].as_u64() != Some(33) {
        return Err(AppError::new(
            "DATA_DIR_SFTPGO_BOLT_VERSION",
            "此 Bolt 账号库版本需要先由 SFTPGo 完成升级，未切换数据目录",
        )
        .with_hint("请先用已安装的 SFTPGo 确认旧账号库可以正常使用，再重试迁移。"));
    }
    let mut dump = serde_json::json!({"version":16});
    let mut directories = Vec::new();
    let mut changed = false;
    for name in ["folders", "groups", "users"] {
        let bucket = before.buckets.get(name.as_bytes()).ok_or_else(invalid)?;
        let mut records = Vec::new();
        for (key, bytes) in &bucket.values {
            let mut value = account(bytes)?;
            let identity = if name == "users" { "username" } else { "name" };
            if value[identity]
                .as_str()
                .is_none_or(|value| value.as_bytes() != key)
            {
                return Err(invalid());
            }
            if let Some(home) = value.pointer_mut(field(name)) {
                if !home.is_null() {
                    let original = home.as_str().ok_or_else(invalid)?;
                    let mut directory = std::path::PathBuf::from(original);
                    if name == "groups" {
                        while directory.to_string_lossy().contains("%username%")
                            || directory.to_string_lossy().contains("%role%")
                        {
                            if !directory.pop() {
                                break;
                            }
                        }
                    }
                    if directory.is_absolute() {
                        directories.push(directory);
                    }
                    let migrated = rebase.path(original);
                    if migrated != original {
                        *home = migrated.into();
                        changed = true;
                        records.push(value);
                    }
                }
            }
        }
        dump[name] = records.into();
    }
    Ok(Plan {
        before,
        dump,
        directories,
        changed,
    })
}

fn normalize_relations(bucket: &str, value: &mut serde_json::Value) {
    let fields: &[&str] = match bucket {
        "folders" => &["users", "groups"],
        "groups" => &["users", "admins"],
        "roles" => &["users", "admins"],
        _ => &[],
    };
    for field in fields {
        if let Some(values) = value
            .get_mut(*field)
            .and_then(serde_json::Value::as_array_mut)
        {
            values.sort_by_key(serde_json::Value::to_string);
        }
    }
}

fn verify(
    before: &Snapshot,
    after: &Snapshot,
    rebase: &crate::paths::DataPathRebase,
) -> Result<()> {
    if before.sequence != after.sequence || before.buckets.keys().ne(after.buckets.keys()) {
        return Err(invalid());
    }
    for (key, bucket) in &before.buckets {
        let updated = &after.buckets[key];
        if bucket.sequence != updated.sequence || bucket.values.keys().ne(updated.values.keys()) {
            return Err(invalid());
        }
        let name = std::str::from_utf8(key).map_err(|_| invalid())?;
        for (id, original) in &bucket.values {
            let current = &updated.values[id];
            if !matches!(name, "users" | "folders" | "groups" | "roles") {
                if current != original {
                    return Err(invalid());
                }
                continue;
            }
            let mut expected = account(original)?;
            let mut actual = account(current)?;
            if name != "roles" {
                if let Some(home) = expected.pointer_mut(field(name)) {
                    if let Some(value) = home.as_str() {
                        *home = rebase.path(value).into();
                    }
                }
            }
            // 官方导入会更新用户/组时间，并刷新引用被改文件夹的用户时间。
            // 不忽略 LastLogin、配额更新时间、计数、密码、权限、角色或其他字段。
            if matches!(name, "users" | "groups") {
                if let Some(time) = expected
                    .get("updated_at")
                    .and_then(serde_json::Value::as_i64)
                {
                    if actual
                        .get("updated_at")
                        .and_then(serde_json::Value::as_i64)
                        .is_none_or(|actual| actual < time)
                    {
                        return Err(invalid());
                    }
                    actual["updated_at"] = time.into();
                }
            }
            normalize_relations(name, &mut expected);
            normalize_relations(name, &mut actual);
            if actual != expected {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

pub(crate) fn migrate(
    path: &Path,
    executable: Option<&Path>,
    rebase: &crate::paths::DataPathRebase,
) -> Result<bool> {
    let planned = plan(path, rebase)?;
    if !planned.changed {
        return Ok(false);
    }
    let executable = executable.ok_or_else(|| {
        AppError::new(
            "DATA_DIR_SFTPGO_RUN",
            "找不到迁移 Bolt 账号库所需的 SFTPGo 程序，未切换数据目录",
        )
        .with_hint("请先恢复 SFTPGo 安装；原账号库和文件已保留。")
    })?;
    let content = serde_json::to_vec(&planned.dump).map_err(|_| invalid())?;
    if content.len() > 20 * 1024 * 1024 {
        return Err(AppError::new(
            "DATA_DIR_SFTPGO_BOLT_SIZE",
            "账号数据超过 SFTPGo 单次导入上限，未切换数据目录",
        ));
    }
    let temp = tempfile::Builder::new()
        .prefix("niceenv-sftpgo-import-")
        .tempdir()?;
    let config = temp.path().join("sftpgo.json");
    let dump = temp.path().join("accounts.json");
    std::fs::write(
        &config,
        serde_json::json!({
            "data_provider":{"driver":"bolt","name":crate::paths::portable_path_text(path),
                "actions":{"execute_on":[],"execute_for":[],"hook":""}},
            "plugins":[], "kms":{"secrets":{"url":"","master_key_path":"","master_key":""}}
        })
        .to_string(),
    )?;
    std::fs::write(&dump, content)?;
    let mut command = platform::command(executable);
    command
        .current_dir(temp.path())
        .args(["initprovider", "--config-dir"])
        .arg(temp.path())
        .arg("--config-file")
        .arg(&config)
        .arg("--loaddata-from")
        .arg(&dump)
        .args(["--loaddata-mode", "0"]);
    // env.d、运行快照和系统 SFTPGO_* 均不能把离线操作重定向到源库/远程库或插件。
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("SFTPGO_")
        {
            command.env_remove(key);
        }
    }
    let success = crate::cfgeditor::run_quiet_command_with_timeout(
        &mut command,
        std::time::Duration::from_secs(30),
    )
    .map_err(|error| {
        AppError::new(
            "DATA_DIR_SFTPGO_IMPORT",
            "SFTPGo 未能完成账号目录迁移，未切换数据目录",
        )
        .with_detail(error.code)
        .with_hint("原账号库和文件已保留；请检查 SFTPGo 安装及账号库是否完整。")
    })?;
    if !success {
        return Err(AppError::new(
            "DATA_DIR_SFTPGO_IMPORT",
            "SFTPGo 拒绝导入迁移后的账号目录，原数据已保留",
        ));
    }
    verify(&planned.before, &read(path)?, rebase)?;
    Ok(true)
}
