//! AI 客户端用户配置：保留 JSONC 原文，只修改 NiceEnv 条目。
use crate::error::{AppError, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs::File, io::{Read, Write}, path::{Path, PathBuf}};

const MAX_CONFIG_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatus {
    pub id: String,
    pub name: String,
    pub path: String,
    pub status: String,
    pub revision: Option<String>,
    pub error: Option<AppError>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientUpdate {
    pub client: ClientStatus,
    pub changed: bool,
    pub backup_path: Option<String>,
}

struct ClientSpec { id: &'static str, name: &'static str, path: PathBuf, group: &'static str }

fn specs(home: &Path, config: &Path) -> Vec<ClientSpec> {
    vec![
        ClientSpec { id: "cursor", name: "Cursor", path: home.join(".cursor/mcp.json"), group: "mcpServers" },
        ClientSpec { id: "claude-desktop", name: "Claude Desktop", path: config.join("Claude/claude_desktop_config.json"), group: "mcpServers" },
        ClientSpec { id: "vscode", name: "VS Code", path: config.join("Code/User/mcp.json"), group: "servers" },
    ]
}

fn user_specs() -> Result<Vec<ClientSpec>> {
    let home = dirs::home_dir().ok_or_else(|| AppError::new("MCP_CLIENT_HOME", "无法确定当前用户目录"))?;
    let config = dirs::config_dir().ok_or_else(|| AppError::new("MCP_CLIENT_HOME", "无法确定当前用户配置目录"))?;
    Ok(specs(&home, &config))
}

fn bad_config() -> AppError {
    AppError::new("MCP_CLIENT_CONFIG_INVALID", "客户端配置格式无效或存在重复字段，未修改文件")
        .with_hint("请在客户端中修复配置后重新检查；现有配置需为 JSON 或带注释的 JSON 对象。")
}

/// 注释与尾逗号替换为空格，保持字节偏移不变；字符串中的 //、逗号和转义不会改写。
fn document(text: &str) -> Result<(Vec<u8>, Value, Vec<usize>)> {
    let mut bytes = text.as_bytes().to_vec();
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) { bytes[..3].fill(b' '); }
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                match bytes[i] { b'\\' => i += 2, b'"' => { i += 1; break; }, _ => i += 1 }
            }
        } else if bytes.get(i..i + 2) == Some(b"//") {
            while i < bytes.len() && !matches!(bytes[i], b'\r' | b'\n') { bytes[i] = b' '; i += 1; }
        } else if bytes.get(i..i + 2) == Some(b"/*") {
            bytes[i..i + 2].fill(b' '); i += 2;
            loop {
                if i + 1 >= bytes.len() { return Err(bad_config()); }
                if bytes.get(i..i + 2) == Some(b"*/") { bytes[i..i + 2].fill(b' '); i += 2; break; }
                if !matches!(bytes[i], b'\r' | b'\n') { bytes[i] = b' '; }
                i += 1;
            }
        } else { i += 1; }
    }
    i = 0;
    let mut trailing = Vec::new();
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                match bytes[i] { b'\\' => i += 2, b'"' => { i += 1; break; }, _ => i += 1 }
            }
        } else {
            if bytes[i] == b',' {
                let next = skip_space(&bytes, i + 1);
                if bytes.get(next).is_some_and(|c| matches!(c, b'}' | b']')) {
                    let previous = bytes[..i].iter().rev().find(|c| !c.is_ascii_whitespace());
                    if previous.is_none_or(|c| matches!(c, b'{' | b'[' | b',' | b':')) { return Err(bad_config()); }
                    trailing.push(i);
                    bytes[i] = b' ';
                }
            }
            i += 1;
        }
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| bad_config())?;
    if !value.is_object() { return Err(bad_config()); }
    Ok((bytes, value, trailing))
}

fn skip_space(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) { at += 1; }
    at
}

struct Member { key: String, key_start: usize, start: usize, end: usize, comma: Option<usize> }

fn members(bytes: &[u8], at: usize) -> Result<Vec<Member>> {
    let open = skip_space(bytes, at);
    if bytes.get(open) != Some(&b'{') { return Err(bad_config()); }
    let mut at = open + 1;
    let mut result = Vec::new();
    let mut names = HashSet::new();
    loop {
        at = skip_space(bytes, at);
        if bytes.get(at) == Some(&b'}') { return Ok(result); }
        let key_start = at;
        if bytes.get(at) != Some(&b'"') { return Err(bad_config()); }
        let mut end = at + 1;
        while end < bytes.len() && bytes[end] != b'"' { end += if bytes[end] == b'\\' { 2 } else { 1 }; }
        if end >= bytes.len() { return Err(bad_config()); }
        let key: String = serde_json::from_slice(&bytes[at..=end]).map_err(|_| bad_config())?;
        if !names.insert(key.clone()) { return Err(bad_config()); }
        at = skip_space(bytes, end + 1);
        if bytes.get(at) != Some(&b':') { return Err(bad_config()); }
        let start = skip_space(bytes, at + 1);
        let mut reader = serde_json::Deserializer::from_slice(&bytes[start..]).into_iter::<Value>();
        reader.next().and_then(std::result::Result::ok).ok_or_else(bad_config)?;
        let end = start + reader.byte_offset();
        at = skip_space(bytes, end);
        let comma = (bytes.get(at) == Some(&b',')).then_some(at);
        result.push(Member { key, key_start, start, end, comma });
        if comma.is_some() { at += 1; } else if bytes.get(at) != Some(&b'}') { return Err(bad_config()); }
    }
}

fn expected(spec: &ClientSpec, executable: &Path, data: &Path) -> Result<Value> {
    let executable = executable.to_str().ok_or_else(bad_config)?;
    let data = data.to_str().ok_or_else(bad_config)?;
    let mut value = json!({"command":executable,"args":[],"env":{"NSB_HOME":data}});
    if spec.group == "servers" { value["type"] = json!("stdio"); }
    Ok(value)
}

fn entry(text: &str, group: &str) -> Result<Option<Value>> {
    let (bytes, root, _) = document(text)?;
    let top = members(&bytes, 0)?;
    let Some(parent) = top.iter().find(|m| m.key == group) else { return Ok(None); };
    let children = members(&bytes, parent.start)?;
    if let Some(child) = children.iter().find(|m| m.key == "niceenv") {
        let value = root[group]["niceenv"].clone();
        if value.is_object() {
            let fields = members(&bytes, child.start)?;
            if let Some(env) = fields.iter().find(|m| m.key == "env") {
                if value["env"].is_object() { members(&bytes, env.start)?; }
            }
        }
        Ok(Some(value))
    } else { Ok(None) }
}

fn matches_current(value: &Value, expected: &Value) -> bool {
    value["command"] == expected["command"] && value["env"]["NSB_HOME"] == expected["env"]["NSB_HOME"]
        && value.get("args").is_none_or(|args| args == &json!([]))
        && value.get("type").is_none_or(|kind| kind == "stdio") && value.get("url").is_none()
        && value.get("disabled").is_none_or(|disabled| disabled == false)
}

fn change(text: &str, group: &str, value: Option<Value>) -> Result<String> {
    let (bytes, _, trailing) = document(text)?;
    let root = members(&bytes, 0)?;
    let parent = root.iter().find(|m| m.key == group);
    let (open, siblings) = match parent {
        Some(parent) => (parent.start, members(&bytes, parent.start)?),
        None => {
            if let Some(value) = value { return insert_member(text, &root, skip_space(&bytes, 0), group, json!({"niceenv":value})); }
            return Ok(text.to_owned());
        }
    };
    if let Some(index) = siblings.iter().position(|m| m.key == "niceenv") {
        let member = &siblings[index];
        let mut output = text.to_owned();
        if let Some(value) = value {
            let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
            output.replace_range(member.start..member.end, &formatted(&value, newline, &indent_at(text, member.key_start))?);
        } else if let Some(comma) = member.comma {
            output.replace_range(member.key_start..comma + 1, "");
        } else if index > 0 {
            output.replace_range(siblings[index - 1].comma.ok_or_else(bad_config)?..member.end, "");
        } else {
            let end = trailing.iter().find(|at| **at >= member.end && **at < skip_space(&bytes, member.end))
                .map(|comma| comma + 1).unwrap_or(member.end);
            output.replace_range(member.key_start..end, "");
        }
        // 原文件尾逗号由 JSONC 解析器保留；修改后仍需完整验证。
        entry(&output, group)?;
        Ok(output)
    } else if let Some(value) = value { insert_member(text, &siblings, open, "niceenv", value) }
    else { Ok(text.to_owned()) }
}

fn insert_member(text: &str, siblings: &[Member], open: usize, key: &str, value: Value) -> Result<String> {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let indent = siblings.first().map(|first| indent_at(text, first.key_start)).filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}  ", indent_at(text, open)));
    let content = formatted(&value, newline, &indent)?;
    let (at, prefix) = siblings.last().map(|last| (last.end, ",")).unwrap_or((open + 1, ""));
    let mut output = text.to_owned();
    output.insert_str(at, &format!("{prefix}{newline}{indent}{}: {content}{newline}", json!(key)));
    document(&output)?;
    Ok(output)
}

fn indent_at(text: &str, at: usize) -> String {
    let before = &text[..at];
    before.rsplit('\n').next().unwrap_or("").chars().take_while(|c| matches!(c, ' ' | '\t')).collect()
}

fn formatted(value: &Value, newline: &str, indent: &str) -> Result<String> {
    Ok(serde_json::to_string_pretty(value).map_err(|error| AppError::internal("生成客户端配置失败", error.to_string()))?
        .replace('\n', &format!("{newline}{indent}")))
}

fn read_config(path: &Path) -> Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::io("读取客户端配置失败", error)),
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => return Err(AppError::new("MCP_CLIENT_CONFIG_FILE", "客户端配置不是普通文件，请检查配置路径")),
        Ok(_) => {},
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES { return Err(AppError::new("MCP_CLIENT_CONFIG_LARGE", "客户端配置超过 2 MiB，请先在客户端检查文件")); }
    String::from_utf8(bytes).map(Some).map_err(|_| bad_config())
}

fn revision(content: Option<&str>, expected: &Value) -> String {
    let mut hash = Sha256::new();
    hash.update(if content.is_some() { b"present".as_slice() } else { b"absent".as_slice() });
    hash.update(content.unwrap_or_default());
    hash.update(expected.to_string());
    hex::encode(hash.finalize())
}

fn inspect(spec: &ClientSpec, executable: &Path, data: &Path) -> ClientStatus {
    let mut result = ClientStatus { id: spec.id.into(), name: spec.name.into(), path: spec.path.to_string_lossy().into(),
        status: "error".into(), revision: None, error: None };
    let checked = (|| -> Result<()> {
        let parent = spec.path.parent().ok_or_else(bad_config)?;
        match std::fs::metadata(parent) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => { result.status = "unavailable".into(); return Ok(()); },
            Err(error) => return Err(AppError::io("读取客户端配置目录失败", error)),
            Ok(meta) if !meta.is_dir() => return Err(bad_config()),
            Ok(_) => {},
        }
        let expected = expected(spec, executable, data)?;
        let content = read_config(&spec.path)?;
        let current = content.as_deref().map(|text| entry(text, spec.group)).transpose()?.flatten();
        result.status = match current { None => "missing", Some(value) if matches_current(&value, &expected) => "connected", _ => "different" }.into();
        result.revision = Some(revision(content.as_deref(), &expected));
        Ok(())
    })();
    if let Err(error) = checked { result.error = Some(error); }
    result
}

pub fn list(executable: &Path, data: &Path) -> Result<Vec<ClientStatus>> {
    Ok(user_specs()?.iter().map(|spec| inspect(spec, executable, data)).collect())
}

pub fn configure(id: &str, action: &str, expected_revision: &str, replace: bool, executable: &Path, data: &Path) -> Result<ClientUpdate> {
    let spec = user_specs()?.into_iter().find(|spec| spec.id == id)
        .ok_or_else(|| AppError::new("MCP_CLIENT_UNKNOWN", "请选择支持的 AI 客户端"))?;
    configure_at(&spec, action, expected_revision, replace, executable, data)
}

fn configure_at(spec: &ClientSpec, action: &str, expected_revision: &str, replace: bool, executable: &Path, data: &Path) -> Result<ClientUpdate> {
    if !matches!(action, "connect" | "remove") { return Err(AppError::new("MCP_CLIENT_ACTION", "接入操作无效")); }
    let parent = spec.path.parent().ok_or_else(bad_config)?;
    if !parent.is_dir() { return Err(AppError::new("MCP_CLIENT_NOT_DETECTED", "未检测到客户端配置目录，请先安装并启动一次客户端")); }
    let lock = File::options().read(true).write(true).create(true).truncate(false).open(parent.join(".niceenv-mcp.lock"))?;
    match lock.try_lock() {
        Ok(()) => {},
        Err(std::fs::TryLockError::WouldBlock) => return Err(AppError::new("MCP_CLIENT_BUSY", "客户端配置正在更新，请稍后重试")),
        Err(std::fs::TryLockError::Error(error)) => return Err(AppError::io("锁定客户端配置失败", error)),
    }
    let desired = expected(spec, executable, data)?;
    let original = read_config(&spec.path)?;
    if expected_revision != revision(original.as_deref(), &desired) {
        return Err(AppError::new("MCP_CLIENT_CHANGED", "客户端配置已变化，请重新检查后再操作"));
    }
    let current = original.as_deref().map(|text| entry(text, spec.group)).transpose()?.flatten();
    let ours = current.as_ref().is_some_and(|value| matches_current(value, &desired));
    if current.is_some() && !ours && (action == "remove" || !replace) {
        return Err(AppError::new("MCP_CLIENT_OTHER_INSTANCE", "已有 NiceEnv 配置指向其他环境，请确认更换后再接入"));
    }
    if (action == "connect" && ours) || (action == "remove" && current.is_none()) {
        return Ok(ClientUpdate { client: inspect(spec, executable, data), changed: false, backup_path: None });
    }
    let output = change(original.as_deref().unwrap_or("{}\n"), spec.group, (action == "connect").then_some(desired))?;
    if output.len() as u64 > MAX_CONFIG_BYTES { return Err(AppError::new("MCP_CLIENT_CONFIG_LARGE", "接入后的配置超过 2 MiB，未保存文件")); }
    let backup_path = if let Some(original) = &original {
        let mut backup = tempfile::Builder::new().prefix(".niceenv-mcp-").suffix(".bak").tempfile_in(parent)?;
        platform::restrict_file_to_owner(backup.path())?;
        backup.write_all(original.as_bytes())?;
        backup.as_file().sync_all()?;
        let (file, path) = backup.keep().map_err(|error| AppError::io("保留客户端配置备份失败", error.error))?;
        drop(file);
        Some(path.to_string_lossy().into_owned())
    } else { None };
    let mut pending = tempfile::NamedTempFile::new_in(parent)?;
    platform::restrict_file_to_owner(pending.path())?;
    pending.write_all(output.as_bytes())?;
    pending.as_file().sync_all()?;
    if read_config(&spec.path)? != original { return Err(AppError::new("MCP_CLIENT_CHANGED", "客户端配置在保存期间发生变化，请重新检查")); }
    pending.persist(&spec.path).map_err(|error| AppError::io("保存客户端配置失败", error.error))?;
    Ok(ClientUpdate { client: inspect(spec, executable, data), changed: true, backup_path })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_edits_keep_other_settings_comments_and_strings() {
        let original = "\u{feff}{\r\n  // 保留用户说明\r\n  \"mcpServers\": {\r\n    \"other\": {\"command\":\"https://host/a,}/*literal*/\",\"args\":[\"中文\\\"\",],}, // 保留原服务\r\n  },\r\n  \"otherSettings\": [true, 9,],\r\n}\r\n";
        let wanted = json!({"command":"C:\\Nice Env\\nsb-mcp.exe","args":[],"env":{"NSB_HOME":"D:\\数据"}});
        let updated = change(original, "mcpServers", Some(wanted.clone())).unwrap();
        assert_eq!(entry(&updated, "mcpServers").unwrap(), Some(wanted));
        assert!(updated.starts_with('\u{feff}'));
        for snippet in ["// 保留用户说明", "// 保留原服务", "\"other\": {\"command\":\"https://host/a,}/*literal*/\",\"args\":[\"中文\\\"\",],}", "\"otherSettings\": [true, 9,]"] {
            assert!(updated.contains(snippet), "{updated}");
        }
        let removed = change(&updated, "mcpServers", None).unwrap();
        assert!(entry(&removed, "mcpServers").unwrap().is_none());
        assert_eq!(document(original).unwrap().1, document(&removed).unwrap().1);
    }

    #[test]
    fn remove_first_middle_last_and_only_entry_with_trailing_commas() {
        for original in [
            r#"{"servers":{"niceenv":{},"other":{"command":"x"}}}"#,
            r#"{"servers":{"first":{},"niceenv":{},"last":{}}}"#,
            r#"{"servers":{"first":{},"niceenv":{},}}"#,
            r#"{"servers":{"niceenv":{},/* keep */}}"#,
            r#"{"servers":{"niceenv":{}}}"#,
        ] {
            let mut expected = document(original).unwrap().1;
            expected["servers"].as_object_mut().unwrap().remove("niceenv");
            let result = change(original, "servers", None).unwrap();
            assert_eq!(document(&result).unwrap().1, expected, "{result}");
        }
        for original in ["{}", "{ /*inside*/ }", "{\"inputs\":[],}", "{\"servers\":{ /*inside*/ }}"] {
            let result = change(original, "servers", Some(json!({"type":"stdio","command":"x"}))).unwrap();
            assert_eq!(entry(&result, "servers").unwrap().unwrap()["type"], "stdio");
        }
    }

    #[test]
    fn malformed_or_ambiguous_config_is_never_accepted() {
        for text in ["", "[]", "null", "{,}", "{\"a\":[,]}", "{\"a\":1,,}", "{/*unclosed", "{\"mcpServers\":[]}",
            "{\"mcpServers\":{},\"mcpServers\":{}}", "{\"mcpServers\":{\"niceenv\":{},\"niceenv\":{}}}",
            "{\"mcpServers\":{\"niceenv\":{\"command\":\"x\",\"command\":\"y\"}}}",
            "{\"mcpServers\":{\"niceenv\":{\"env\":{\"NSB_HOME\":\"x\",\"NSB_HOME\":\"y\"}}}}"] {
            assert!(entry(text, "mcpServers").is_err(), "accepted {text}");
        }
    }

    #[test]
    fn client_files_connect_remove_backup_and_reject_stale_or_other_instance() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("Nice Env/nsb-mcp.exe");
        let data = temp.path().join("数据目录");
        for spec in specs(temp.path(), &temp.path().join("config")) {
            assert_eq!(inspect(&spec, &executable, &data).status, "unavailable");
            std::fs::create_dir_all(spec.path.parent().unwrap()).unwrap();
            let initial = inspect(&spec, &executable, &data);
            assert_eq!(initial.status, "missing");
            let added = configure_at(&spec, "connect", initial.revision.as_deref().unwrap(), false, &executable, &data).unwrap();
            assert_eq!(added.client.status, "connected");
            assert!(added.changed && added.backup_path.is_none());
            let saved = std::fs::read_to_string(&spec.path).unwrap();
            if spec.id == "vscode" { assert_eq!(document(&saved).unwrap().1["servers"]["niceenv"]["type"], "stdio"); }
            let current_revision = added.client.revision.as_deref().unwrap();
            let unchanged = configure_at(&spec, "connect", current_revision, false, &executable, &data).unwrap();
            assert!(!unchanged.changed && unchanged.backup_path.is_none());
            std::fs::write(&spec.path, format!("{saved}\n// edited externally")).unwrap();
            assert_eq!(configure_at(&spec, "remove", current_revision, false, &executable, &data).err().unwrap().code, "MCP_CLIENT_CHANGED");
            let other_data = temp.path().join("other-environment");
            let different = inspect(&spec, &executable, &other_data);
            assert_eq!(different.status, "different");
            assert_eq!(configure_at(&spec, "connect", different.revision.as_deref().unwrap(), false, &executable, &other_data).err().unwrap().code, "MCP_CLIENT_OTHER_INSTANCE");
            assert_eq!(configure_at(&spec, "remove", different.revision.as_deref().unwrap(), true, &executable, &other_data).err().unwrap().code, "MCP_CLIENT_OTHER_INSTANCE");
            let prior = std::fs::read_to_string(&spec.path).unwrap();
            let replaced = configure_at(&spec, "connect", different.revision.as_deref().unwrap(), true, &executable, &other_data).unwrap();
            assert_eq!(std::fs::read_to_string(replaced.backup_path.unwrap()).unwrap(), prior);
            let removed = configure_at(&spec, "remove", replaced.client.revision.as_deref().unwrap(), false, &executable, &other_data).unwrap();
            assert_eq!(removed.client.status, "missing");
            assert!(removed.backup_path.is_some());
            std::fs::write(&spec.path, "broken { contents").unwrap();
            assert_eq!(inspect(&spec, &executable, &data).status, "error");
            assert!(configure_at(&spec, "connect", "old-revision", true, &executable, &data).is_err());
            let broken_revision = revision(Some("broken { contents"), &expected(&spec, &executable, &data).unwrap());
            assert_eq!(configure_at(&spec, "connect", &broken_revision, true, &executable, &data).err().unwrap().code, "MCP_CLIENT_CONFIG_INVALID");
            assert_eq!(std::fs::read_to_string(&spec.path).unwrap(), "broken { contents");
        }
    }

    #[test]
    fn file_bounds_lock_and_non_regular_paths_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let spec = specs(temp.path(), temp.path()).remove(0);
        std::fs::create_dir_all(spec.path.parent().unwrap()).unwrap();
        let executable = temp.path().join("mcp");
        let status = inspect(&spec, &executable, temp.path());
        let lock = File::options().create(true).truncate(false).write(true).read(true).open(spec.path.parent().unwrap().join(".niceenv-mcp.lock")).unwrap();
        lock.lock().unwrap();
        assert_eq!(configure_at(&spec, "connect", status.revision.as_deref().unwrap(), false, &executable, temp.path()).err().unwrap().code, "MCP_CLIENT_BUSY");
        drop(lock);
        std::fs::create_dir(&spec.path).unwrap();
        assert_eq!(read_config(&spec.path).err().unwrap().code, "MCP_CLIENT_CONFIG_FILE");
        std::fs::remove_dir(&spec.path).unwrap();
        std::fs::write(&spec.path, " ".repeat(MAX_CONFIG_BYTES as usize + 1)).unwrap();
        assert_eq!(read_config(&spec.path).err().unwrap().code, "MCP_CLIENT_CONFIG_LARGE");
    }

    #[test]
    fn native_client_config_probe() {
        let Some(root) = std::env::var_os("NICEENV_MCP_CLIENT_NATIVE_ROOT") else { return; };
        let root = std::fs::canonicalize(root).unwrap();
        assert_eq!(root.parent().unwrap(), std::fs::canonicalize(std::env::temp_dir()).unwrap());
        assert!(root.file_name().unwrap().to_string_lossy().starts_with("niceenv-mcp-client-native-"));
        let executable = PathBuf::from(std::env::var_os("NICEENV_MCP_CLIENT_NATIVE_EXE").unwrap());
        let data = root.join("isolated-data");
        let mut output = Vec::new();
        for spec in specs(&root, &root.join("config")) {
            std::fs::create_dir_all(spec.path.parent().unwrap()).unwrap();
            std::fs::write(&spec.path, "{\n// existing client preference\n\"keepPreference\":true,\n}\n").unwrap();
            let initial = inspect(&spec, &executable, &data);
            let updated = configure_at(&spec, "connect", initial.revision.as_deref().unwrap(), false, &executable, &data).unwrap();
            assert_eq!(updated.client.status, "connected");
            output.push(json!({"id":spec.id,"path":spec.path,"group":spec.group,"backup":updated.backup_path,"data":data}));
        }
        println!("NATIVE_CLIENT_CONFIGS={}", json!(output));
    }
}
