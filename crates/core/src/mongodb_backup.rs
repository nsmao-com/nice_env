//! 官方 BSON archive 备份与整库恢复。恢复前保存目标库，失败保留现场及保护备份。
use crate::{AppError, CoreState, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs, io::{Read, Write}, path::{Path, PathBuf}, process::Stdio, time::Duration};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MongoBackup {
    pub id: String, pub database: String, pub version: String, pub tools_version: String,
    pub created_at: i64, pub size_bytes: u64, pub sha256: String, pub kind: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupList { pub items: Vec<MongoBackup>, pub unreadable: usize, pub directory: String }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePreview { pub backup: MongoBackup, pub target: String, pub exists: bool, pub revision: String }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreResult { pub target: String, pub safety_backup: Option<MongoBackup> }

fn invalid(message: &str) -> AppError { AppError::new("MONGO_BACKUP_INVALID", message) }
fn database_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 63 || name.chars().any(|c| c.is_control() || c.is_whitespace() || "/\\.\"$*<>:|?".contains(c))
        || matches!(name.to_ascii_lowercase().as_str(), "admin" | "local" | "config") {
        return Err(invalid("请选择业务数据库；名称须少于 64 字节，不能包含空白或路径、命名空间特殊字符"));
    }
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
        || !record.sha256.bytes().all(|b| b.is_ascii_hexdigit()) || !matches!(record.kind.as_str(), "manual" | "before-restore") {
        return Err(invalid("备份记录字段无效"));
    }
    Ok(record)
}
fn copy_hash(path: &Path, mut output: impl Write) -> Result<(u64, String)> {
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() { return Err(invalid("备份必须是普通文件")); }
    let mut options = fs::OpenOptions::new(); options.read(true);
    #[cfg(windows)] { use std::os::windows::fs::OpenOptionsExt; options.share_mode(5).custom_flags(0x00200000); }
    let mut file = options.open(path)?;
    let mut hash = Sha256::new(); let mut size = 0; let mut buffer = [0u8; 65536];
    loop { let n = file.read(&mut buffer)?; if n == 0 { break; } hash.update(&buffer[..n]); output.write_all(&buffer[..n])?; size += n as u64; }
    if before.len() != size || before.modified()? != file.metadata()?.modified()? { return Err(invalid("备份在读取时发生变化，请重试")); }
    Ok((size, hex::encode(hash.finalize())))
}
pub fn list(state: &CoreState) -> Result<BackupList> {
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let dir = directory(state)?; fs::create_dir_all(&dir)?;
    let mut result = BackupList { items: vec![], unreadable: 0, directory: crate::paths::portable_path_text(&dir) };
    for item in fs::read_dir(dir)? {
        let name = item?.file_name().to_string_lossy().into_owned(); if name.starts_with('.') { continue; }
        let record = metadata(state, &name).and_then(|record| {
            let path = item_path(state, &name, "archive.gz")?;
            if !path.is_file() || fs::metadata(path)?.len() != record.size_bytes { return Err(invalid("备份文件缺失或大小变化")); }
            Ok(record)
        });
        match record { Ok(record) => result.items.push(record), Err(_) => result.unreadable += 1 }
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
fn run_tool(executable: &Path, uri: &str, args: &[String], home: &Path) -> Result<()> {
    let mut output = tempfile::tempfile()?; let mut command = platform::command(executable);
    command.arg(format!("--uri={uri}/?directConnection=true&serverSelectionTimeoutMS=5000&connectTimeoutMS=5000"))
        .args(args).current_dir(home).env("HOME", home).env("USERPROFILE", home)
        .stdin(Stdio::null()).stdout(output.try_clone()?).stderr(output.try_clone()?);
    let status = crate::dbadmin::wait_client(&mut command, Duration::from_secs(1800), || {})?;
    if !status.success() { return Err(AppError::new("MONGO_BACKUP_TOOL_FAILED", "MongoDB 备份工具执行失败")
        .with_detail(crate::dbadmin::read_output(&mut output, 65536).unwrap_or_default())); }
    Ok(())
}
fn create_inner(state: &CoreState, version: &str, database: &str, kind: &str) -> Result<MongoBackup> {
    let info = inspect(state, version, database, false)?;
    if info["exists"] != true { return Err(invalid("所选数据库已不存在，请刷新列表")); }
    let (exe, tools_version) = tool(state, "mongodump", None)?;
    let dir = directory(state)?; fs::create_dir_all(&dir)?;
    let pending = tempfile::Builder::new().prefix(".pending-").tempdir_in(&dir)?;
    let archive = pending.path().join("archive.gz");
    run_tool(&exe, info["uri"].as_str().ok_or_else(|| invalid("实例地址无效"))?, &[format!("--db={database}"), format!("--archive={}", archive.display()), "--gzip".into()], pending.path())?;
    crate::ops::verify_database_listener(&state.manager, "mongodb", info["port"].as_u64().unwrap_or(0) as u16)?;
    let (size_bytes, sha256) = copy_hash(&archive, std::io::sink())?;
    if size_bytes == 0 { return Err(invalid("导出的备份为空")); }
    fs::OpenOptions::new().read(true).write(true).open(&archive)?.sync_all()?;
    let id = format!("{}-{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(), pending.path().file_name().unwrap().to_string_lossy().trim_start_matches(".pending-"));
    let record = MongoBackup { id: id.clone(), database: database.into(), version: info["version"].as_str().unwrap_or(version).into(), tools_version,
        created_at: chrono::Utc::now().timestamp(), size_bytes, sha256, kind: kind.into() };
    let mut meta = fs::File::create(pending.path().join("metadata.json"))?;
    meta.write_all(&serde_json::to_vec(&record).map_err(|e| AppError::internal("保存 MongoDB 备份记录", e.to_string()))?)?; meta.sync_all()?; drop(meta);
    let dest = item_path(state, &id, "archive.gz")?.parent().unwrap().to_path_buf();
    if dest.exists() { return Err(invalid("备份标识冲突，请重试")); }
    fs::rename(pending.path(), dest)?;
    Ok(record)
}
pub fn create(state: &CoreState, version: &str, database: &str) -> Result<MongoBackup> {
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    create_inner(state, version, database, "manual")
}
fn verified(state: &CoreState, id: &str, output: impl Write) -> Result<MongoBackup> {
    let record = metadata(state, id)?;
    let (size, hash) = copy_hash(&item_path(state, id, "archive.gz")?, output)?;
    if size != record.size_bytes || hash != record.sha256 { return Err(AppError::new("MONGO_BACKUP_CHECKSUM", "备份校验失败，未执行恢复")); }
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
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    preview_inner(state, version, verified(state, id, std::io::sink())?, target)
}
pub fn restore(state: &CoreState, version: &str, id: &str, target: &str, revision: &str, confirmation: &str) -> Result<RestoreResult> {
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    database_name(target)?;
    if confirmation != target { return Err(invalid("请准确输入目标数据库名称以确认恢复")); }
    let pending = tempfile::Builder::new().prefix(".mongo-restore-").tempdir_in(&state.paths.base)?;
    let archive = pending.path().join("archive.gz"); let mut file = fs::File::create(&archive)?;
    let backup = verified(state, id, &mut file)?; file.sync_all()?; drop(file);
    let current = preview_inner(state, version, backup.clone(), target)?;
    if current.revision != revision { return Err(AppError::new("MONGO_RESTORE_CHANGED", "实例、目标集合或备份已变化，请重新检查后确认恢复")); }
    let (exe, _) = tool(state, "mongorestore", Some(&backup.tools_version))?;
    let info = inspect(state, version, target, false)?;
    let uri = info["uri"].as_str().ok_or_else(|| invalid("实例地址无效"))?;
    let args = vec![format!("--archive={}", archive.display()), "--gzip".into(), "--stopOnError".into(),
        format!("--nsInclude={}.*", backup.database), format!("--nsFrom={}.*", backup.database), format!("--nsTo={target}.*")];
    let mut dry = args.clone(); dry.push("--dryRun".into()); run_tool(&exe, uri, &dry, pending.path())?;
    let safety_backup = if current.exists { Some(create_inner(state, version, target, "before-restore")?) } else { None };
    // 保护备份期间目标结构或服务变化时不开始写入；外部应用须暂停写入（不宣称跨客户端事务）。
    if preview_inner(state, version, backup, target)?.revision != revision {
        return Err(AppError::new("MONGO_RESTORE_CHANGED", "目标在准备期间发生变化，已取消恢复；已有保护备份仍保留"));
    }
    let write_result: Result<()> = (|| {
        if current.exists { inspect(state, version, target, true)?; }
        run_tool(&exe, uri, &args, pending.path())?;
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
