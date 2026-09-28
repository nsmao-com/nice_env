//! MongoDB 文档浏览：只连接已核对的本机托管实例，通过官方 mongosh 执行固定的只读操作。
use crate::{AppError, CoreState, Result};
use serde::{Deserialize, Serialize};
use std::{io::Write, path::Path, process::Stdio, time::Duration};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentFilter {
    pub field: String,
    pub value: String,
    pub value_type: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum BrowseRequest {
    Overview,
    Collections { database: String, search: String },
    Documents { database: String, collection: String, offset: u32, limit: u32, filter: Option<DocumentFilter> },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionInfo { pub name: String, pub kind: String }

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document { pub content: String, pub truncated: bool }

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum BrowseResponse {
    Overview { version: String, #[serde(rename = "serverVersion")] server_version: String, port: u16, uri: String,
        #[serde(rename = "shellVersion")] shell_version: String, databases: Vec<String>, limited: bool },
    Collections { database: String, entries: Vec<CollectionInfo>, limited: bool },
    Documents { database: String, collection: String, offset: u32, limit: u32, documents: Vec<Document>, #[serde(rename = "hasMore")] has_more: bool },
}

fn valid_name(name: &str, max: usize) -> bool {
    !name.is_empty() && name.len() <= max && !name.chars().any(char::is_control)
}

fn validate(request: &BrowseRequest) -> Result<()> {
    let bad = || AppError::new("MONGO_QUERY_INVALID", "数据库、集合、分页或筛选条件无效");
    match request {
        BrowseRequest::Overview => {}
        BrowseRequest::Collections { database, search } => {
            if !valid_name(database, 64) || search.len() > 200 || search.chars().any(char::is_control) { return Err(bad()); }
        }
        BrowseRequest::Documents { database, collection, offset, limit, filter } => {
            if !valid_name(database, 64) || !valid_name(collection, 255) || *offset > 10_000 || !(1..=25).contains(limit) { return Err(bad()); }
            if let Some(filter) = filter {
                if !valid_name(&filter.field, 255) || filter.field.contains('$') || filter.field.split('.').any(str::is_empty)
                    || filter.value.len() > 4096 || filter.value.contains('\0') { return Err(bad()); }
                let valid = match filter.value_type.as_str() {
                    "text" => true,
                    "number" => filter.value.parse::<f64>().is_ok_and(|value| value.is_finite()
                        && (value.fract() != 0.0 || value.abs() <= 9_007_199_254_740_991.0)),
                    "boolean" => matches!(filter.value.as_str(), "true" | "false"),
                    "null" => filter.value.is_empty(),
                    "objectId" => filter.value.len() == 24 && filter.value.bytes().all(|value| value.is_ascii_hexdigit()),
                    _ => false,
                };
                if !valid { return Err(bad().with_hint("数字需有限且整数不超过安全精度范围；ObjectId 需为 24 位十六进制字符")); }
            }
        }
    }
    Ok(())
}

// 用户选择只作为 JSON 数据传入。没有自由脚本、任意命令、远程地址或写入操作入口。
const CONNECT: &str = r###"
try {
  const connection = new Mongo(input.uri + '/?directConnection=true&serverSelectionTimeoutMS=4000&socketTimeoutMS=6000&appName=NiceEnv');
  const admin = connection.getDB('admin');
  const checked = result => { if (result.ok !== 1) throw Object.assign(new Error(result.errmsg || 'MongoDB query failed'), { code: result.code }); return result; };
  const options = checked(admin.runCommand({getCmdLineOpts:1}));
  const fs = require('fs');
  const normalize = path => { const resolved = fs.realpathSync(path); return process.platform === 'win32' ? resolved.toLowerCase() : resolved; };
  let sameDataDir = false;
  try { sameDataDir = !!options.parsed?.storage?.dbPath && normalize(options.parsed.storage.dbPath) === normalize(input.dataDir); } catch {}
  if (!sameDataDir) {
    throw Object.assign(new Error('MongoDB data directory changed'), { code: 'MONGO_INSTANCE_CHANGED' });
  }
"###;
const SCRIPT: &str = r###"
  const request = input.request;
  let result;
  if (request.action === 'overview') {
    const reply = checked(admin.runCommand({listDatabases:1,nameOnly:true,authorizedDatabases:true,maxTimeMS:5000}));
    const names = reply.databases.map(row => row.name).sort();
    result = {kind:'overview', version:input.version, serverVersion:admin.version(), port:input.port, uri:input.uri,
      shellVersion:input.shellVersion, databases:names.slice(0,1000), limited:names.length>1000};
  } else {
    const database = connection.getDB(request.database);
    const escaped = value => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    const match = request.action === 'collections' ? (request.search ? {name:{$regex:escaped(request.search),$options:'i'}} : {}) : {name:request.collection};
    let cursor = checked(database.runCommand({listCollections:1,filter:match,nameOnly:true,authorizedCollections:true,cursor:{batchSize:100},maxTimeMS:5000})).cursor;
    const entries = [];
    let limited = false;
    try {
      while (true) {
        for (const item of cursor.firstBatch || cursor.nextBatch || []) {
          if (entries.length >= 1000) { limited = true; break; }
          entries.push({name:item.name,kind:item.type});
        }
        if (limited || cursor.id.toString() === '0') break;
        if (entries.length >= 1000) { limited = true; break; }
        cursor = checked(database.runCommand({getMore:cursor.id,collection:cursor.ns.slice(request.database.length+1),batchSize:100})).cursor;
      }
    } finally { if (cursor.id.toString() !== '0') database.runCommand({killCursors:cursor.ns.slice(request.database.length+1),cursors:[cursor.id]}); }
    if (request.action === 'collections') {
      entries.sort((a,b) => a.name.localeCompare(b.name)); result = {kind:'collections',database:request.database,entries,limited};
    } else {
      if (!entries.some(entry => entry.name === request.collection)) throw Object.assign(new Error('Collection no longer exists'), {code:'MONGO_COLLECTION_MISSING'});
      let filter = {};
      if (request.filter) {
        const spec = request.filter;
        const value = spec.valueType === 'number' ? Number(spec.value) : spec.valueType === 'boolean' ? spec.value === 'true' : spec.valueType === 'null' ? null : spec.valueType === 'objectId' ? ObjectId(spec.value) : spec.value;
        filter = {[spec.field]: {$eq:value}};
      }
      const docs = database.getCollection(request.collection).find(filter).sort({_id:1}).skip(request.offset).limit(request.limit+1).maxTimeMS(5000);
      const documents = [];
      let hasMore = false;
      try {
        while (docs.hasNext()) {
          if (documents.length >= request.limit) { hasMore = true; break; }
          const text = EJSON.stringify(docs.next(), null, 2, {relaxed:false});
          documents.push({content:text.slice(0,65536),truncated:text.length>65536});
        }
      } finally { docs.close(); }
      result = {kind:'documents',database:request.database,collection:request.collection,offset:request.offset,limit:request.limit,documents,hasMore};
    }
  }
  print(JSON.stringify({result}));
"###;
const CATCH: &str = r###"
} catch (error) {
  print(JSON.stringify({error:{code:String(error.code || ''),message:String(error.message || 'MongoDB query failed').slice(0,2000)}}));
}
"###;

pub fn browse(state: &CoreState, version: &str, request: BrowseRequest) -> Result<BrowseResponse> {
    validate(&request)?;
    let value = execute(state, version, serde_json::to_value(request).map_err(|e| AppError::internal("准备 MongoDB 查询", e.to_string()))?, SCRIPT)?;
    serde_json::from_value(value).map_err(|e| AppError::internal("解析 MongoDB 浏览结果", e.to_string()))
}

// 只接受后端固定脚本；Tauri 不暴露此函数。备份操作复用实例与数据目录核对。
pub(crate) fn execute(state: &CoreState, version: &str, request: serde_json::Value, script_body: &str) -> Result<serde_json::Value> {
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _operation = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重新读取"))?;
    let service = state.manager.snapshot("mongodb").filter(|service| service.version.as_deref() == Some(version)
        && matches!(service.state, crate::model::ServiceState::Running | crate::model::ServiceState::Error)
        && service.pids.iter().any(|pid| platform::process_alive(*pid)))
        .ok_or_else(|| AppError::new("MONGO_NOT_RUNNING", "所选 MongoDB 实例未运行或运行版本已变化"))?;
    state.store.find_installed("mongodb", Some(version)).ok_or_else(|| AppError::not_installed("MongoDB"))?;
    let port = service.port.filter(|port| *port > 0).ok_or_else(|| AppError::new("MONGO_PORT_UNKNOWN", "无法确认 MongoDB 实际端口"))?;
    crate::ops::verify_database_listener(&state.manager, "mongodb", port)?;
    let shell = crate::ops::installed_by_choice(&state.store, "mongosh")
        .ok_or_else(|| AppError::new("MONGO_SHELL_MISSING", "请先安装 MongoDB Shell，以启用数据库浏览"))?;
    let entry = state.installer.installed_entry(&shell);
    crate::pathenv::terminal_package_directory(&shell, &entry)
        .map_err(|detail| AppError::new("MONGO_SHELL_MISSING", "MongoDB Shell 入口不可用，请重新安装").with_detail(detail))?;
    let executable = Path::new(&shell.install_path).join(crate::install::entry_relative_path(&entry.entry));
    let temp = tempfile::Builder::new().prefix(".mongo-browse-").tempdir_in(&state.paths.base)?;
    let input = serde_json::json!({"uri":format!("mongodb://127.0.0.1:{port}"),"port":port,"version":version,
        "shellVersion":shell.version,"dataDir":state.paths.mongo_data_dir(version),"request":request});
    let script = temp.path().join("query.js");
    let mut file = std::fs::File::create(&script)?;
    write!(file, "const input = {};\n{}\n{}\n{}", serde_json::to_string(&input).map_err(|e| AppError::internal("准备 MongoDB 查询", e.to_string()))?, CONNECT, script_body, CATCH)?;
    drop(file);
    let mut output = tempfile::tempfile()?;
    let mut error = tempfile::tempfile()?;
    let mut command = platform::command(executable);
    command.args(["--quiet", "--norc", "--nodb", "--file"]).arg(&script).current_dir(temp.path())
        .env("HOME", temp.path()).env("USERPROFILE", temp.path()).env("APPDATA", temp.path()).env("LOCALAPPDATA", temp.path())
        .env("MONGOSH_LOG_DIR", temp.path()).stdin(Stdio::null()).stdout(output.try_clone()?).stderr(error.try_clone()?);
    let status = crate::dbadmin::wait_client(&mut command, Duration::from_secs(20), || {})?;
    if !status.success() {
        return Err(AppError::new("MONGO_SHELL_FAILED", "MongoDB Shell 执行失败，请检查工具安装及实例状态")
            .with_detail(crate::dbadmin::read_output(&mut error, 16 * 1024).unwrap_or_default()));
    }
    let raw = crate::dbadmin::read_output(&mut output, 8 * 1024 * 1024)?;
    let value: serde_json::Value = serde_json::from_str(raw.trim())
        .map_err(|_| AppError::new("MONGO_RESULT_INVALID", "MongoDB Shell 未返回有效结果，请更新官方 MongoDB Shell 后重试"))?;
    if let Some(error) = value.get("error") {
        let (code, message) = match error["code"].as_str().unwrap_or_default() {
            "13" | "18" => ("MONGO_ACCESS_DENIED", "MongoDB 拒绝访问，请检查实例的认证与权限设置"),
            "50" => ("MONGO_QUERY_TIMEOUT", "MongoDB 查询超时，请缩小筛选范围后重试"),
            "MONGO_INSTANCE_CHANGED" => ("MONGO_INSTANCE_CHANGED", "MongoDB 数据目录与所选实例不一致，已停止读取"),
            "MONGO_COLLECTION_MISSING" => ("MONGO_COLLECTION_MISSING", "所选集合已不存在，请刷新集合列表"),
            _ => ("MONGO_QUERY_FAILED", "MongoDB 查询失败，请检查实例状态后重试"),
        };
        return Err(AppError::new(code, message).with_detail(error["message"].as_str().unwrap_or_default()));
    }
    crate::ops::verify_database_listener(&state.manager, "mongodb", port)?;
    value.get("result").cloned().ok_or_else(|| AppError::new("MONGO_RESULT_INVALID", "MongoDB Shell 未返回有效结果"))
}
