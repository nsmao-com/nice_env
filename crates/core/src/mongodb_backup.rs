//! 官方 BSON archive 备份与整库恢复。恢复前保存目标库，失败保留现场及保护备份。
use crate::{AppError, CoreState, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}, process::Stdio, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MongoBackup {
    pub id: String, pub database: String, pub version: String, pub tools_version: String,
    pub created_at: i64, pub size_bytes: u64, pub sha256: String, pub kind: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupList { pub items: Vec<MongoBackup>, pub issues: Vec<BackupIssue>, pub unreadable: usize, #[serde(serialize_with = "crate::model::serialize_path")] pub directory: String }
#[derive(Debug, Serialize)]
pub struct BackupIssue { pub id: String, pub problem: String }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub source: String, pub info: crate::mongodb_archive::ArchiveInfo, pub size_bytes: u64, pub sha256: String, pub revision: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRemoval { pub id: String, pub database: Option<String>, pub kind: Option<String>, pub size_bytes: Option<u64>, pub revision: String }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePreview { pub backup: MongoBackup, #[serde(serialize_with = "crate::model::serialize_path")] pub target: String, pub exists: bool, pub revision: String }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult { #[serde(serialize_with = "crate::model::serialize_path")] pub target: String, pub safety_backup: Option<MongoBackup> }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseDeletePreview { pub version: String, pub database: String, pub collections: usize, pub revision: String }

fn invalid(message: &str) -> AppError { AppError::new("MONGO_BACKUP_INVALID", message) }
fn database_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 63 || name.chars().any(|c| c.is_control() || c.is_whitespace() || "/\\.\"$*<>:|?".contains(c))
        || matches!(name.to_ascii_lowercase().as_str(), "admin" | "local" | "config") {
        return Err(invalid("请选择业务数据库；名称须少于 64 字节，不能包含空白或路径、命名空间特殊字符"));
    }
    Ok(())
}
fn database_delete_revision(info: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(info).map_err(|e| AppError::internal("准备 MongoDB 删除确认", e.to_string()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
pub fn database_delete_preview(state: &CoreState, version: &str, database: &str) -> Result<DatabaseDeletePreview> {
    let _work = crate::BackgroundWork::begin("检查 MongoDB 数据库删除范围")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    database_name(database)?;
    let info = inspect(state, version, database, false)?;
    if info["exists"] != true { return Err(AppError::new("MONGO_DATABASE_MISSING", "所选数据库已不存在，请刷新数据库列表")); }
    let collections = info["collections"].as_array().map_or(0, Vec::len);
    Ok(DatabaseDeletePreview { version: version.into(), database: database.into(), collections, revision: database_delete_revision(&info)? })
}
pub fn database_delete(state: &CoreState, version: &str, database: &str, revision: &str, confirmation: &str) -> Result<()> {
    let _work = crate::BackgroundWork::begin("删除 MongoDB 数据库")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    database_name(database)?;
    if confirmation != database { return Err(AppError::new("MONGO_DATABASE_CONFIRM", "请输入完整数据库名称以确认删除")); }
    let before = inspect(state, version, database, false)?;
    if before["exists"] != true { return Err(AppError::new("MONGO_DATABASE_MISSING", "所选数据库已不存在，请刷新数据库列表")); }
    if database_delete_revision(&before)? != revision { return Err(AppError::new("MONGO_DATABASE_CHANGED", "数据库结构或实例状态已变化，请重新检查后确认")); }
    inspect(state, version, database, true)?;
    let after = inspect(state, version, database, false)?;
    if after["exists"] == true { return Err(AppError::new("MONGO_DATABASE_DELETE_INCOMPLETE", "MongoDB 未确认删除数据库，请刷新后检查")); }
    Ok(())
}
pub fn directory(state: &CoreState) -> Result<PathBuf> { Ok(crate::paths::checked_data_path(&state.paths.base, "backup/mongodb")?) }
fn item_path(state: &CoreState, id: &str, file: &str) -> Result<PathBuf> {
    if id.is_empty() || id.len() > 80 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') { return Err(invalid("备份标识无效")); }
    Ok(crate::paths::checked_data_path(&state.paths.base, &format!("backup/mongodb/{id}/{file}"))?)
}
fn metadata(state: &CoreState, id: &str) -> Result<MongoBackup> {
    let path = item_path(state, id, "metadata.json")?;
    if !path.is_file() || fs::metadata(&path)?.len() > 16384 { return Err(invalid("备份记录不可读取")); }
    let record: MongoBackup = serde_json::from_slice(&fs::read(path)?).map_err(|_| invalid("备份记录损坏"))?;
    database_name(&record.database)?;
    if record.id != id || record.created_at <= 0 || record.version.len() > 128 || record.version.is_empty()
        || record.tools_version.len() > 128 || record.tools_version.is_empty() || record.sha256.len() != 64
        || !record.sha256.bytes().all(|b| b.is_ascii_hexdigit()) || !matches!(record.kind.as_str(), "manual" | "before-restore" | "imported" | "automatic") {
        return Err(invalid("备份记录字段无效"));
    }
    Ok(record)
}
fn copy_hash(path: &Path, mut output: impl Write) -> Result<(u64, String)> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || linked(&before) { return Err(invalid("备份必须是普通文件")); }
    let mut options = fs::OpenOptions::new(); options.read(true);
    #[cfg(windows)] { use std::os::windows::fs::OpenOptionsExt; options.share_mode(5).custom_flags(0x00200000); }
    let mut file = options.open(path)?;
    let actual = file.metadata()?;
    if linked(&actual) || !actual.is_file() || before.len() != actual.len() || before.modified()? != actual.modified()? { return Err(invalid("备份在打开时发生变化")); }
    #[cfg(unix)] { use std::os::unix::fs::MetadataExt; if before.dev()!=actual.dev() || before.ino()!=actual.ino() { return Err(invalid("备份在打开时被替换")); } }
    let mut hash = Sha256::new(); let mut size = 0; let mut buffer = [0u8; 65536];
    loop { let n = file.read(&mut buffer)?; if n == 0 { break; } hash.update(&buffer[..n]); output.write_all(&buffer[..n])?; size += n as u64; }
    if before.len() != size || before.modified()? != file.metadata()?.modified()? { return Err(invalid("备份在读取时发生变化，请重试")); }
    Ok((size, hex::encode(hash.finalize())))
}
pub fn list(state: &CoreState) -> Result<BackupList> {
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let dir = directory(state)?; fs::create_dir_all(&dir)?;
    let mut result = BackupList { items: vec![], issues: vec![], unreadable: 0, directory: crate::paths::portable_path_text(&dir) };
    for item in fs::read_dir(dir)? {
        let name = item?.file_name().to_string_lossy().into_owned(); if name.starts_with('.') { continue; }
        let record = metadata(state, &name).and_then(|record| {
            let path = item_path(state, &name, "archive.gz")?;
            if !path.is_file() || fs::metadata(path)?.len() != record.size_bytes { return Err(invalid("备份文件缺失或大小变化")); }
            Ok(record)
        });
        match record { Ok(record) => result.items.push(record), Err(error) => { result.unreadable += 1; result.issues.push(BackupIssue { id: name, problem: error.message }); } }
    }
    result.items.sort_by(|a,b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id))); Ok(result)
}

// 复用 mongosh 的进程、实际端口和真实 dataDir 检查；只执行这两种固定操作。
const INSPECT: &str = r#"
  const name = input.request.database;
  const names = checked(admin.runCommand({listDatabases:1,nameOnly:true,maxTimeMS:5000})).databases.map(d=>d.name);
  if (names.some(n => n.toLowerCase() === name.toLowerCase() && n !== name)) throw new Error('Database name differs only by case');
  const exists = names.includes(name);
  const db = connection.getDB(name);
  if (input.request.drop) checked(db.runCommand({dropDatabase:1}));
  const collections = exists && !input.request.drop ? db.getCollectionInfos().map(c=>({name:c.name,type:c.type,uuid:EJSON.stringify(c.info?.uuid ?? null)})).sort((a,b)=>a.name.localeCompare(b.name)) : [];
  print(JSON.stringify({result:{exists,collections,version:admin.version(),port:input.port,uri:input.uri}}));
"#;
fn inspect(state: &CoreState, version: &str, database: &str, drop: bool) -> Result<Value> {
    database_name(database)?;
    crate::mongodb::execute(state, version, json!({"database":database,"drop":drop}), INSPECT)
}
fn tool(state: &CoreState, name: &str, version: Option<&str>) -> Result<(PathBuf, String)> {
    let installed = match version { Some(version) => state.store.find_installed("mongodb-database-tools", Some(version)), None => crate::ops::installed_by_choice(&state.store, "mongodb-database-tools") }
        .ok_or_else(|| AppError::new("MONGO_TOOLS_MISSING", "请安装 MongoDB Database Tools；恢复需使用创建备份时的工具版本"))?;
    let entry = state.installer.installed_entry(&installed);
    let bin = crate::pathenv::terminal_package_directory(&installed, &entry)
        .map_err(|detail| AppError::new("MONGO_TOOLS_MISSING", "MongoDB Database Tools 入口不可用").with_detail(detail))?;
    let executable = PathBuf::from(bin).join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if !executable.is_file() { return Err(AppError::new("MONGO_TOOLS_MISSING", "所选工具包缺少备份或恢复命令，请重新安装")); }
    Ok((executable, installed.version))
}
fn run_tool(state: &CoreState, version: &str, executable: &Path, uri: &str, args: &[String], home: &Path) -> Result<()> {
    let credentials = crate::mongodb_auth::Credentials::load(state, version)?;
    let mut output = tempfile::tempfile()?; let mut command = platform::command(executable);
    // 官方工具的私有配置文件仅在本次调用期间存在，密码不进入进程参数或备份目录。
    let mut config = tempfile::NamedTempFile::new()?;
    #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; config.as_file().set_permissions(std::fs::Permissions::from_mode(0o600))?; }
    if !credentials.username.is_empty() {
        writeln!(config,"password: {}",serde_json::to_string(&credentials.password).map_err(|_|invalid("无法准备备份认证"))?)?;
        config.flush()?;
        command.arg(format!("--config={}",config.path().display())).arg(format!("--username={}",credentials.username))
            .arg(format!("--authenticationDatabase={}",credentials.auth_database));
    }
    command.arg(format!("--uri={uri}/?directConnection=true&serverSelectionTimeoutMS=5000&connectTimeoutMS=5000"))
        .args(args).current_dir(home).env("HOME", home).env("USERPROFILE", home)
        .stdin(Stdio::null()).stdout(output.try_clone()?).stderr(output.try_clone()?);
    let status = crate::dbadmin::wait_client(&mut command, Duration::from_secs(1800), || {})?;
    if !status.success() { return Err(AppError::new("MONGO_BACKUP_TOOL_FAILED", "MongoDB 备份工具执行失败")
        .with_detail(credentials.redact(&crate::dbadmin::read_output(&mut output, 65536).unwrap_or_default()))); }
    Ok(())
}
fn create_inner(state: &CoreState, version: &str, database: &str, kind: &str) -> Result<MongoBackup> {
    let info = inspect(state, version, database, false)?;
    if info["exists"] != true { return Err(invalid("所选数据库已不存在，请刷新列表")); }
    let (exe, tools_version) = tool(state, "mongodump", None)?;
    let dir = directory(state)?; fs::create_dir_all(&dir)?;
    let pending = tempfile::Builder::new().prefix(".pending-").tempdir_in(&dir)?;
    let archive = pending.path().join("archive.gz");
    run_tool(state, version, &exe, info["uri"].as_str().ok_or_else(|| invalid("实例地址无效"))?, &[format!("--db={database}"), format!("--archive={}", archive.display()), "--gzip".into()], pending.path())?;
    crate::ops::verify_database_listener(&state.manager, "mongodb", info["port"].as_u64().unwrap_or(0) as u16)?;
    let (size_bytes, sha256) = copy_hash(&archive, std::io::sink())?;
    if size_bytes == 0 { return Err(invalid("导出的备份为空")); }
    fs::OpenOptions::new().read(true).write(true).open(&archive)?.sync_all()?;
    let id = format!("{}-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(), pending.path().file_name().unwrap().to_string_lossy().trim_start_matches(".pending-"));
    let record = MongoBackup { id: id.clone(), database: database.into(), version: info["version"].as_str().unwrap_or(version).into(), tools_version,
        created_at: chrono::Utc::now().timestamp(), size_bytes, sha256, kind: kind.into() };
    validate_content(&record, &archive)?;
    let mut meta = fs::File::create(pending.path().join("metadata.json"))?;
    meta.write_all(&serde_json::to_vec(&record).map_err(|e| AppError::internal("保存 MongoDB 备份记录", e.to_string()))?)?; meta.sync_all()?; drop(meta);
    let dest = item_path(state, &id, "archive.gz")?.parent().unwrap().to_path_buf();
    if dest.exists() { return Err(invalid("备份标识冲突，请重试")); }
    fs::rename(pending.path(), dest)?;
    Ok(record)
}
pub fn create(state: &CoreState, version: &str, database: &str) -> Result<MongoBackup> {
    let _work = crate::BackgroundWork::begin("创建 MongoDB 备份")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    create_inner(state, version, database, "manual")
}

/// 调度器持有整个计划的生命周期锁；固定脚本取得完整库列表，不使用浏览器的分页上限。
pub(crate) fn automatic_databases(state: &CoreState, version: &str) -> Result<Vec<String>> {
    let value = crate::mongodb::execute(state, version, json!({}), r#"
      const names = checked(admin.runCommand({listDatabases:1,nameOnly:true,maxTimeMS:5000})).databases.map(d=>d.name);
      print(JSON.stringify({result:names.filter(n=>!['admin','local','config'].includes(n.toLowerCase())).sort()}));
    "#)?;
    serde_json::from_value(value).map_err(|error| AppError::internal("读取 MongoDB 计划备份范围", error.to_string()))
}
pub(crate) fn create_automatic(state: &CoreState, version: &str, database: &str) -> Result<MongoBackup> {
    create_inner(state, version, database, "automatic")
}

/// 新副本完整校验后才轮转。同版本、同库的有效自动副本计入保留数；异常副本保留并报告。
pub(crate) fn rotate_automatic(state: &CoreState, newest: &MongoBackup, keep: usize) -> Result<()> {
    if keep == 0 { return Ok(()); }
    if newest.kind != "automatic" || verified(state, &newest.id, std::io::sink())? != *newest {
        return Err(invalid("新自动备份已变化，未清理旧备份"));
    }
    let mut candidates = Vec::new(); let mut problems = Vec::new();
    for entry in fs::read_dir(directory(state)?)? {
        let id = entry?.file_name().to_string_lossy().into_owned(); if id.starts_with('.') { continue; }
        let record = match metadata(state, &id) {
            Ok(record) => record,
            Err(error) => { problems.push(format!("{id}：{}，已保留", error.message)); continue; }
        };
        if record.kind != "automatic" || record.database != newest.database || !crate::install::same_version(&record.version, &newest.version) { continue; }
        let checked = (|| -> Result<String> {
            let preview = removal_preview(state, &id)?;
            if verified(state, &id, std::io::sink())? != record { return Err(invalid("备份记录在检查期间变化")); }
            Ok(preview.revision)
        })();
        match checked { Ok(revision) => candidates.push((record, revision)), Err(error) => problems.push(format!("{id}：{}，已保留", error.message)) }
    }
    if !candidates.iter().any(|(record,_)| record.id == newest.id) { return Err(invalid("新自动备份不可用，未清理旧备份")); }
    candidates.sort_by(|(a,_),(b,_)| (b.id == newest.id).cmp(&(a.id == newest.id)).then_with(|| b.created_at.cmp(&a.created_at)).then_with(|| b.id.cmp(&a.id)));
    for (record, revision) in candidates.into_iter().skip(keep) {
        if let Err(error) = remove(state, &record.id, &revision) { problems.push(format!("{}：{}", record.id, error.message)); }
    }
    if !problems.is_empty() { return Err(AppError::new("MONGO_ROTATION_INCOMPLETE", "部分旧副本未清理，请检查备份列表").with_detail(problems.join("；"))); }
    Ok(())
}
fn verified(state: &CoreState, id: &str, output: impl Write) -> Result<MongoBackup> {
    let record = metadata(state, id)?;
    let (size, hash) = copy_hash(&item_path(state, id, "archive.gz")?, output)?;
    if size != record.size_bytes || hash != record.sha256 { return Err(AppError::new("MONGO_BACKUP_CHECKSUM", "备份校验失败，未执行恢复")); }
    validate_content(&record, &item_path(state, id, "archive.gz")?)?;
    Ok(record)
}
fn preview_inner(state: &CoreState, version: &str, backup: MongoBackup, target: &str) -> Result<RestorePreview> {
    let info = inspect(state, version, target, false)?;
    let major = |text: &str| text.split('.').next().and_then(|n| n.parse::<u32>().ok());
    if major(&backup.version).is_none() || major(&backup.version) != major(info["version"].as_str().unwrap_or_default()) {
        return Err(AppError::new("MONGO_BACKUP_VERSION", "备份与目标 MongoDB 主版本不同，请切换到相同主版本后恢复"));
    }
    tool(state, "mongorestore", Some(&backup.tools_version))?;
    let service = state.manager.snapshot("mongodb").ok_or_else(|| invalid("MongoDB 实例已变化"))?;
    let revision = hex::encode(Sha256::digest(serde_json::to_vec(&json!({"backup":backup,"target":target,"info":info,"pids":service.pids})).map_err(|e| AppError::internal("准备恢复预览", e.to_string()))?));
    Ok(RestorePreview { backup, target: target.into(), exists: info["exists"] == true, revision })
}
pub fn preview(state: &CoreState, version: &str, id: &str, target: &str) -> Result<RestorePreview> {
    let _work = crate::BackgroundWork::begin("检查 MongoDB 恢复范围")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    preview_inner(state, version, verified(state, id, std::io::sink())?, target)
}
pub fn restore(state: &CoreState, version: &str, id: &str, target: &str, revision: &str, confirmation: &str) -> Result<RestoreResult> {
    let _work = crate::BackgroundWork::begin("恢复 MongoDB 数据库")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    database_name(target)?;
    if confirmation != target { return Err(invalid("请准确输入目标数据库名称以确认恢复")); }
    let pending = tempfile::Builder::new().prefix(".mongo-restore-").tempdir_in(&state.paths.base)?;
    let archive = pending.path().join("archive.gz"); let mut file = fs::File::create(&archive)?;
    let backup = verified(state, id, &mut file)?; file.sync_all()?; drop(file);
    validate_content(&backup, &archive)?;
    let current = preview_inner(state, version, backup.clone(), target)?;
    if current.revision != revision { return Err(AppError::new("MONGO_RESTORE_CHANGED", "实例、目标集合或备份已变化，请重新检查后确认恢复")); }
    let (exe, _) = tool(state, "mongorestore", Some(&backup.tools_version))?;
    let info = inspect(state, version, target, false)?;
    let uri = info["uri"].as_str().ok_or_else(|| invalid("实例地址无效"))?;
    let args = vec![format!("--archive={}", archive.display()), "--gzip".into(), "--stopOnError".into(),
        format!("--nsInclude={}.*", backup.database), format!("--nsFrom={}.*", backup.database), format!("--nsTo={target}.*")];
    let mut dry = args.clone(); dry.push("--dryRun".into()); run_tool(state, version, &exe, uri, &dry, pending.path())?;
    let safety_backup = if current.exists { Some(create_inner(state, version, target, "before-restore")?) } else { None };
    // 保护备份期间目标结构或服务变化时不开始写入；外部应用须暂停写入（不宣称跨客户端事务）。
    if preview_inner(state, version, backup, target)?.revision != revision {
        return Err(AppError::new("MONGO_RESTORE_CHANGED", "目标在准备期间发生变化，已取消恢复；已有保护备份仍保留"));
    }
    let write_result: Result<()> = (|| {
        if current.exists { inspect(state, version, target, true)?; }
        run_tool(state, version, &exe, uri, &args, pending.path())?;
        let after = inspect(state, version, target, false)?;
        if after["exists"] != true { return Err(invalid("恢复命令未创建目标数据库，请检查备份内容")); }
        Ok(())
    })();
    if let Err(error) = write_result {
        let hint = match &safety_backup { Some(backup) => format!("目标 {target} 可能已部分恢复。请保持应用停止写入；保护备份 {} 可从备份列表再次恢复。", backup.id), None => format!("目标 {target} 可能包含部分恢复数据。请保持应用停止写入，检查原因后重新恢复。") };
        return Err(AppError::new("MONGO_RESTORE_INCOMPLETE", "MongoDB 恢复未完成").with_hint(hint).with_detail(format!("{}\n{}", error.message, error.detail.unwrap_or_default())));
    }
    Ok(RestoreResult { target: target.into(), safety_backup })
}

fn validate_content(record: &MongoBackup, path: &Path) -> Result<()> {
    let info = crate::mongodb_archive::inspect(fs::File::open(path)?, None, std::io::sink())?;
    if !crate::install::same_version(&info.version, &record.version) || info.tools_version != record.tools_version || info.compression != "gzip"
        || info.databases.len() != 1 || info.databases[0].name != record.database {
        return Err(AppError::new("MONGO_BACKUP_METADATA", "备份记录与归档中的真实数据库或版本不一致，未执行操作"));
    }
    Ok(())
}
fn linked(meta: &fs::Metadata) -> bool {
    #[cfg(windows)] { use std::os::windows::fs::MetadataExt; if meta.file_attributes() & 0x400 != 0 { return true; } }
    meta.file_type().is_symlink()
}
fn external_path(source: &str, exporting: bool) -> Result<PathBuf> {
    let path = Path::new(source);
    if !path.is_absolute() || path.components().any(|part| matches!(part, std::path::Component::ParentDir)) { return Err(invalid("请选择完整文件路径")); }
    let parent = path.parent().ok_or_else(|| invalid("文件目录无效"))?;
    for ancestor in if exporting { parent } else { path }.ancestors() {
        if linked(&fs::symlink_metadata(ancestor)?) { return Err(invalid("文件路径不能经过链接或目录联接，请选择原始目录")); }
    }
    Ok(crate::paths::checked_data_path(&parent.canonicalize()?, path.file_name().and_then(|name|name.to_str()).ok_or_else(||invalid("文件名无效"))?)?)
}
fn inspect_source(path: &Path) -> Result<ImportPreview> {
    let (size_bytes, sha256) = copy_hash(path, std::io::sink())?;
    let info = crate::mongodb_archive::inspect(fs::File::open(path)?, None, std::io::sink())?;
    if !info.databases.iter().any(|db| database_name(&db.name).is_ok()) { return Err(invalid("归档中没有可导入的业务数据库")); }
    let source = crate::paths::portable_path_text(path);
    let revision = hex::encode(Sha256::digest(serde_json::to_vec(&json!({"source":source,"size":size_bytes,"sha256":sha256,"info":info})).map_err(|e|AppError::internal("检查 MongoDB 导入",e.to_string()))?));
    Ok(ImportPreview { source, info, size_bytes, sha256, revision })
}
pub fn inspect_import(source: &str) -> Result<ImportPreview> {
    let _work = crate::BackgroundWork::begin("检查外部 MongoDB 归档")?;
    let path = external_path(source, false)?;
    let preview = inspect_source(&path)?;
    let (size, hash) = copy_hash(&path, std::io::sink())?;
    if size != preview.size_bytes || hash != preview.sha256 { return Err(AppError::new("MONGO_IMPORT_CHANGED", "归档在检查期间发生变化，请重新选择")); }
    Ok(preview)
}
pub fn import_archive(state: &CoreState, source: &str, database: &str, revision: &str) -> Result<MongoBackup> {
    let _work = crate::BackgroundWork::begin("导入 MongoDB 归档副本")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    database_name(database)?;
    let source = external_path(source, false)?;
    let preview = inspect_source(&source)?;
    if preview.revision != revision { return Err(AppError::new("MONGO_IMPORT_CHANGED", "源归档与预览不一致，请重新检查并确认")); }
    let dir = directory(state)?; fs::create_dir_all(&dir)?;
    let pending = tempfile::Builder::new().prefix(".pending-").tempdir_in(&dir)?;
    let staged = pending.path().join("source.archive"); let mut copied = fs::File::create(&staged)?;
    let (size, hash) = copy_hash(&source, &mut copied)?; copied.sync_all()?; drop(copied);
    if size != preview.size_bytes || hash != preview.sha256 { return Err(AppError::new("MONGO_IMPORT_CHANGED", "源归档在复制期间变化，未保存导入结果")); }
    let archive = pending.path().join("archive.gz");
    let mut encoder = flate2::write::GzEncoder::new(fs::File::create(&archive)?, flate2::Compression::default());
    let info = crate::mongodb_archive::inspect(fs::File::open(&staged)?, Some(database), &mut encoder)?;
    encoder.finish()?.sync_all()?; fs::remove_file(staged)?;
    let (size_bytes, sha256) = copy_hash(&archive, std::io::sink())?;
    let id = format!("{}-{:016x}", chrono::Utc::now().timestamp_millis(), rand::random::<u64>());
    let record = MongoBackup { id:id.clone(), database:database.into(), version:info.version, tools_version:info.tools_version,
        created_at:chrono::Utc::now().timestamp(), size_bytes, sha256, kind:"imported".into() };
    validate_content(&record, &archive)?;
    let mut file = fs::File::create(pending.path().join("metadata.json"))?;
    file.write_all(&serde_json::to_vec(&record).map_err(|e|AppError::internal("保存导入记录",e.to_string()))?)?; file.sync_all()?; drop(file);
    let dest = item_path(state, &id, "archive.gz")?.parent().unwrap().to_path_buf();
    if dest.exists() { return Err(invalid("备份标识冲突，请重试")); }
    fs::rename(pending.path(), dest)?; Ok(record)
}
pub fn export(state: &CoreState, id: &str, destination: &str) -> Result<String> {
    let _work = crate::BackgroundWork::begin("导出 MongoDB 备份")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    let dest = external_path(destination, true)?; let parent = dest.parent().unwrap();
    if !dest.extension().and_then(|s|s.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("gz")) { return Err(invalid("导出文件名须以 .gz 结尾")); }
    let base = state.paths.base.canonicalize()?;
    #[cfg(windows)] let inside = Path::new(&parent.to_string_lossy().to_lowercase()).starts_with(base.to_string_lossy().to_lowercase());
    #[cfg(not(windows))] let inside = parent.starts_with(base);
    if inside { return Err(invalid("请选择 NiceEnv 数据目录以外的位置")); }
    if dest.exists() { return Err(AppError::new("MONGO_EXPORT_EXISTS", "目标文件已存在，请另选名称；已有文件未覆盖")); }
    let mut pending = tempfile::Builder::new().prefix(".mongo-export-").tempfile_in(parent)?;
    let record = verified(state, id, &mut pending)?;
    validate_content(&record, pending.path())?;
    if metadata(state, id)? != record { return Err(invalid("备份记录在导出期间发生变化")); }
    pending.as_file().sync_all()?;
    pending.persist_noclobber(&dest).map_err(|error| AppError::io("保存 MongoDB 导出文件", error.error))?;
    Ok(crate::paths::portable_path_text(&dest))
}
fn removal_revision(dir: &Path, id: &str) -> Result<(String,Option<u64>)> {
    let meta = fs::symlink_metadata(dir)?;
    if linked(&meta) || !meta.is_dir() { return Err(invalid("备份目录不是普通目录")); }
    for item in fs::read_dir(dir)? {
        let name = item?.file_name();
        if name != "archive.gz" && name != "metadata.json" { return Err(AppError::new("MONGO_BACKUP_EXTRA_FILES", "备份目录包含其他文件，请打开目录检查；未删除任何内容")); }
    }
    let mut files = Vec::new();
    for name in ["archive.gz", "metadata.json"] {
        let path = crate::paths::checked_data_path(dir, name)?;
        match fs::symlink_metadata(&path) {
            Ok(_)=>files.push(Some(copy_hash(&path, std::io::sink())?)),
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>files.push(None),
            Err(error)=>return Err(error.into()),
        }
    }
    let size = files[0].as_ref().map(|(size,_)|*size);
    Ok((hex::encode(Sha256::digest(serde_json::to_vec(&(id,files)).map_err(|_|invalid("备份删除范围无效"))?)),size))
}
pub fn removal_preview(state: &CoreState, id: &str) -> Result<BackupRemoval> {
    let _work = crate::BackgroundWork::begin("检查 MongoDB 备份删除范围")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    let dir = item_path(state, id, "archive.gz")?.parent().unwrap().to_path_buf();
    let (revision,size_bytes) = removal_revision(&dir,id)?; let record = metadata(state,id).ok();
    if removal_revision(&dir,id)?.0 != revision { return Err(invalid("备份在检查期间发生变化，请重试")); }
    Ok(BackupRemoval { id:id.into(), database:record.as_ref().map(|r|r.database.clone()), kind:record.map(|r|r.kind), size_bytes, revision })
}
pub fn remove(state: &CoreState, id: &str, revision: &str) -> Result<()> {
    let _work = crate::BackgroundWork::begin("删除 MongoDB 备份副本")?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    let source = item_path(state,id,"archive.gz")?.parent().unwrap().to_path_buf();
    if removal_revision(&source,id)?.0 != revision { return Err(AppError::new("MONGO_BACKUP_CHANGED", "备份已变化，请重新检查删除范围")); }
    let staged = directory(state)?.join(format!(".delete-{id}-{:016x}",rand::random::<u64>()));
    if staged.exists() { return Err(invalid("删除暂存目录冲突，请重试")); }
    fs::rename(&source,&staged)?;
    let cleanup = (|| -> Result<()> {
        if removal_revision(&staged,id)?.0 != revision { return Err(invalid("备份在删除前变化，已中止")); }
        // 不递归清理；只处理两个受管文件，未知文件与用户额外内容必须保留。
        for name in ["archive.gz","metadata.json"] {
            match fs::remove_file(staged.join(name)) { Ok(())=>{},Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},Err(e)=>return Err(e.into()) }
        }
        fs::remove_dir(&staged)?; Ok(())
    })();
    cleanup.map_err(|error| {
        let retained = if !source.exists() && fs::rename(&staged,&source).is_ok() { source } else { staged };
        error.with_hint(format!("未清理完的内容保留在 {}；当前 MongoDB 数据没有改动",crate::paths::portable_path_text(&retained)))
    })
}
