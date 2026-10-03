//! JSON/YAML 配置迁移：使用解析器定位值，只修改明确的文件路径，保留其它源码。
use crate::error::{AppError, Result};
use crate::paths::DataPathRebase;
use libyaml_rs as yaml;
use std::{collections::HashMap, mem::MaybeUninit, ops::Range, path::Path};
use yaml_serde::Value;

fn invalid() -> AppError {
    AppError::new(
        "DATA_DIR_CONFIG_FORMAT",
        "服务配置无法安全解析或迁移，原文件已保留",
    )
    .with_hint("请检查配置语法和路径字段；错误信息不会展示密码或订阅内容。")
}

/// 只识别服务配置文件，不将同目录的数据库、下载缓存等按配置改写。
pub(crate) fn service(relative: &Path) -> Option<(&'static str, bool)> {
    let relative = crate::paths::config_path_key(relative);
    let parts = Path::new(&relative)
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    let [root, service, rest @ ..] = parts.as_slice() else {
        return None;
    };
    if !matches!(*root, "etc" | "runtimes") || rest.is_empty() {
        return None;
    }
    let name = *rest.last()?;
    let name = name.strip_suffix(".disabled").unwrap_or(name);
    if name == ".niceenv-package.json" {
        return None;
    }
    let json = name.ends_with(".json");
    let yaml = name.ends_with(".yaml") || name.ends_with(".yml");
    match *service {
        "mongodb" if yaml || json || name == "mongod.conf" => Some(("mongodb", json)),
        "qdrant" if yaml || json => Some(("qdrant", json)),
        "mihomo" if yaml || json => Some(("mihomo", json)),
        "sftpgo" if matches!(name, "sftpgo.json" | "sftpgo.yaml" | "sftpgo.yml") => {
            Some(("sftpgo", json))
        }
        _ => None,
    }
}

fn sqlite_invalid() -> AppError {
    AppError::new(
        "SFTPGO_SQLITE_DSN",
        "无法安全识别 SQLite 数据库文件，未启动或更改数据库",
    )
    .with_hint("请检查本地 SQLite 文件路径或 file: 连接地址；连接参数和密码不会显示在错误信息中。")
}

fn uri_decode(value: &str) -> Result<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.as_bytes().iter().copied();
    while let Some(byte) = input.next() {
        bytes.push(if byte == b'%' {
            let hi = (input.next().ok_or_else(sqlite_invalid)? as char)
                .to_digit(16)
                .ok_or_else(sqlite_invalid)?;
            let lo = (input.next().ok_or_else(sqlite_invalid)? as char)
                .to_digit(16)
                .ok_or_else(sqlite_invalid)?;
            (hi * 16 + lo) as u8
        } else {
            byte
        });
    }
    if bytes.contains(&0) {
        return Err(sqlite_invalid());
    }
    String::from_utf8(bytes).map_err(|_| sqlite_invalid())
}

/// SQLite URI 路径单独编码；不能把 #、?、% 或 Unix 反斜杠解释成 URI 结构。
pub(crate) fn sqlite_file_uri(path: &Path) -> String {
    let path = crate::paths::portable_path_text(path);
    let mut uri = String::from("file:");
    if path.starts_with("//") {
        uri.push_str("//");
    } else if cfg!(windows) && path.as_bytes().get(1) == Some(&b':') {
        uri.push('/');
    }
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            uri.push(byte as char);
        } else {
            uri.push('%');
            uri.push(HEX[(byte >> 4) as usize] as char);
            uri.push(HEX[(byte & 15) as usize] as char);
        }
    }
    uri
}

pub(crate) fn sftpgo_sqlite_dsn(directory: &Path, name: &str) -> String {
    let uri = if name == ":memory:" {
        "file::memory:".into()
    } else {
        sqlite_file_uri(&directory.join(name))
    };
    format!("{uri}?cache=shared&_foreign_keys=1")
}

struct SqliteConnection<'a> {
    path: Option<std::path::PathBuf>,
    suffix: &'a str,
    uri: bool,
}

fn sqlite_connection(value: &str) -> Result<SqliteConnection<'_>> {
    let uri = value.starts_with("file:");
    let value_path = value.strip_prefix("file:").unwrap_or(value);
    let end = value_path
        .find(|c| c == '?' || (uri && c == '#'))
        .unwrap_or(value_path.len());
    let suffix = &value_path[end..];
    let mut path = &value_path[..end];
    if uri {
        if let Some(authority) = path.strip_prefix("//") {
            let host = authority.split('/').next().unwrap_or("");
            if !host.is_empty() && host != "localhost" {
                return Err(sqlite_invalid());
            }
            path = &path[2 + host.len()..];
        }
    }
    let path = if uri {
        uri_decode(path)?
    } else {
        path.to_string()
    };
    let memory = if uri {
        let parameters = suffix
            .strip_prefix('?')
            .unwrap_or("")
            .split('#')
            .next()
            .unwrap_or("")
            .split('&')
            .filter_map(|part| part.split_once('='))
            .map(|(key, value)| Ok((uri_decode(&key.replace('+', " "))?, value)))
            .collect::<Result<Vec<_>>>()?;
        let mut modes = parameters
            .iter()
            .filter(|(key, _)| key == "mode")
            .map(|(_, value)| uri_decode(&value.replace('+', " ")));
        let mode = modes.next().transpose()?;
        for next in modes {
            if Some(next?) != mode {
                return Err(sqlite_invalid());
            }
        }
        mode.as_deref() == Some("memory")
    } else {
        false
    };
    let path = if path.is_empty() || path == ":memory:" || memory {
        None
    } else {
        let path = if cfg!(windows)
            && path.starts_with('/')
            && path.as_bytes().get(1).is_some_and(u8::is_ascii_alphabetic)
            && path.as_bytes().get(2) == Some(&b':')
            && path.as_bytes().get(3) == Some(&b'/')
        {
            &path[1..]
        } else {
            &path
        };
        Some(std::path::PathBuf::from(path))
    };
    Ok(SqliteConnection { path, suffix, uri })
}

pub(crate) fn sqlite_connection_path(value: &str) -> Result<Option<std::path::PathBuf>> {
    Ok(sqlite_connection(value)?.path)
}

fn rebase_sqlite_connection(value: &str, rebase: &DataPathRebase) -> Result<String> {
    let connection = sqlite_connection(value)?;
    let Some(path) = connection.path else {
        return Ok(value.into());
    };
    let original = path.to_str().ok_or_else(sqlite_invalid)?;
    let updated = rebase.path(original);
    if updated == original {
        return Ok(value.into());
    }
    // 普通 go-sqlite3 DSN 的查询参数可能含字面 #；转成 URI 时保留其参数值。
    let suffix = if connection.uri {
        connection.suffix.to_string()
    } else {
        connection.suffix.replace('#', "%23")
    };
    Ok(format!("{}{suffix}", sqlite_file_uri(Path::new(&updated))))
}

fn path_field(service: &str, path: &[String], root: &Value) -> bool {
    let path = path.iter().map(String::as_str).collect::<Vec<_>>();
    match (service, path.as_slice()) {
        (
            "mongodb",
            ["storage", "dbPath"]
            | ["systemLog", "path"]
            | ["processManagement", "pidFilePath"]
            | ["net", "unixDomainSocket", "pathPrefix"]
            | ["security", "keyFile"]
            | ["security", "encryptionKeyFile"]
            | ["auditLog", "path"],
        ) => true,
        (
            "mongodb",
            ["net", "tls", "certificateKeyFile" | "clusterFile" | "CAFile" | "clusterCAFile" | "CRLFile"]
            | ["net", "ssl", "PEMKeyFile" | "clusterFile" | "CAFile" | "CRLFile"],
        ) => true,
        (
            "qdrant",
            ["storage", "storage_path" | "snapshots_path" | "temp_path"]
            | ["tls", "cert" | "key" | "ca_cert"],
        ) => true,
        (
            "mihomo",
            ["external-ui" | "external-controller-unix"]
            | ["proxy-providers" | "rule-providers", _, "path"]
            | ["tls", "certificate" | "private-key"]
            | ["listeners", "*", "certificate" | "private-key"],
        ) => true,
        ("sftpgo", ["data_provider", "name"]) => {
            matches!(
                root["data_provider"]["driver"].as_str().unwrap_or("sqlite"),
                "sqlite" | "bolt"
            ) && root["data_provider"]["connection_string"]
                .as_str()
                .unwrap_or("")
                .is_empty()
        }
        ("sftpgo", ["data_provider", "connection_string"]) => {
            root["data_provider"]["driver"].as_str().unwrap_or("sqlite") == "sqlite"
        }
        (
            "sftpgo",
            ["common", "temp_path"]
            | ["data_provider", "credentials_path" | "backups_path" | "users_base_dir" | "root_cert" | "client_cert"
            | "client_key"]
            | ["httpd", "templates_path"
            | "static_files_path"
            | "openapi_path"
            | "certificate_file"
            | "certificate_key_file"
            | "signing_passphrase_file"]
            | ["httpd" | "ftpd" | "webdavd", "bindings", "*", "certificate_file" | "certificate_key_file"]
            | ["httpd", "bindings", "*", "oidc", "client_secret_file"]
            | ["sftpd", "host_keys" | "host_certificates" | "trusted_user_ca_keys", "*"]
            | ["sftpd", "revoked_user_certs_file" | "opkssh_path" | "login_banner_file"]
            | ["ftpd", "banner_file"]
            | ["ftpd" | "webdavd" | "telemetry", "certificate_file" | "certificate_key_file"]
            | ["httpd" | "ftpd" | "webdavd", "ca_certificates" | "ca_revocation_lists", "*"]
            | ["telemetry", "auth_user_file"]
            | ["http", "ca_certificates", "*"]
            | ["http", "certificates", "*", "cert" | "key"]
            | ["kms", "secrets", "master_key_path"]
            | ["smtp", "templates_path"]
            | ["acme", "certs_path"]
            | ["acme", "http01_challenge", "webroot"],
        ) => true,
        _ => false,
    }
}

fn rebase_field(
    service: &str,
    path: &[String],
    value: &str,
    rebase: &DataPathRebase,
) -> Result<String> {
    if service == "sftpgo" && path == ["data_provider", "connection_string"] {
        rebase_sqlite_connection(value, rebase)
    } else {
        rebase.config_value(value, str::to_owned)
    }
}

fn parse(source: &str, json: bool) -> Result<Value> {
    if json {
        serde_json::from_str::<serde_json::Value>(source).map_err(|_| invalid())?;
    }
    // 保留输入映射顺序，不能先经过默认会排序键的 serde_json::Value。
    yaml_serde::from_str(source).map_err(|_| invalid())
}

/// libyaml 的解析器持有输入指针，输入借用及堆上地址在整个解析期间保持不变。
struct Parser<'a> {
    inner: Box<MaybeUninit<yaml::yaml_parser_t>>,
    _source: &'a str,
}
impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Result<Self> {
        let mut inner = Box::new(MaybeUninit::uninit());
        // SAFETY: 先初始化，再提供在 Parser 生命周期内有效的 UTF-8 输入；不移动底层对象。
        unsafe {
            if yaml::yaml_parser_initialize(inner.as_mut_ptr()).fail {
                return Err(invalid());
            }
            yaml::yaml_parser_set_encoding(inner.as_mut_ptr(), yaml::YAML_UTF8_ENCODING);
            yaml::yaml_parser_set_input_string(
                inner.as_mut_ptr(),
                source.as_ptr(),
                source.len() as u64,
            );
        }
        Ok(Self {
            inner,
            _source: source,
        })
    }
}
impl Drop for Parser<'_> {
    fn drop(&mut self) {
        // SAFETY: 只释放成功初始化且尚未释放的解析器。
        unsafe {
            yaml::yaml_parser_delete(self.inner.as_mut_ptr());
        }
    }
}

#[derive(Debug)]
enum Node {
    Scalar(Range<usize>),
    Alias(Range<usize>),
    Sequence(Vec<Node>),
    Mapping(Vec<(Node, Node)>),
}
enum Event {
    Leaf(Node),
    Sequence,
    Mapping,
    EndSequence,
    EndMapping,
}

fn tree(source: &str) -> Result<Node> {
    let mut spans = HashMap::new();
    let mut scanner = Parser::new(source)?;
    loop {
        let mut token = MaybeUninit::<yaml::yaml_token_t>::uninit();
        // SAFETY: token 仅在扫描成功后读取，所有返回路径都在读取字段后释放其所有权。
        let (kind, start, end) = unsafe {
            if yaml::yaml_parser_scan(scanner.inner.as_mut_ptr(), token.as_mut_ptr()).fail {
                return Err(invalid());
            }
            let mut token = token.assume_init();
            let data = (
                token.type_,
                token.start_mark.index as usize,
                token.end_mark.index as usize,
            );
            yaml::yaml_token_delete(&mut token);
            data
        };
        if kind == yaml::YAML_SCALAR_TOKEN {
            spans.insert(end, start..end);
        }
        if kind == yaml::YAML_STREAM_END_TOKEN {
            break;
        }
    }
    let mut parser = Parser::new(source)?;
    let mut events = Vec::new();
    loop {
        let mut event = MaybeUninit::<yaml::yaml_event_t>::uninit();
        // SAFETY: 不借用事件持有的 C 字符串，只拷贝类型和位置，然后立即释放事件。
        let (kind, start, end) = unsafe {
            if yaml::yaml_parser_parse(parser.inner.as_mut_ptr(), event.as_mut_ptr()).fail {
                return Err(invalid());
            }
            let mut event = event.assume_init();
            let data = (
                event.type_,
                event.start_mark.index as usize,
                event.end_mark.index as usize,
            );
            yaml::yaml_event_delete(&mut event);
            data
        };
        if source.get(start..end).is_none() {
            return Err(invalid());
        }
        match kind {
            yaml::YAML_SCALAR_EVENT => events.push(Event::Leaf(Node::Scalar(
                spans.remove(&end).unwrap_or(start..end),
            ))),
            yaml::YAML_ALIAS_EVENT => events.push(Event::Leaf(Node::Alias(start..end))),
            yaml::YAML_SEQUENCE_START_EVENT => events.push(Event::Sequence),
            yaml::YAML_MAPPING_START_EVENT => events.push(Event::Mapping),
            yaml::YAML_SEQUENCE_END_EVENT => events.push(Event::EndSequence),
            yaml::YAML_MAPPING_END_EVENT => events.push(Event::EndMapping),
            yaml::YAML_STREAM_END_EVENT => break,
            _ => {}
        }
        if events.len() > 200_000 {
            return Err(invalid());
        }
    }
    fn node(
        events: &mut std::iter::Peekable<std::vec::IntoIter<Event>>,
        depth: usize,
    ) -> Result<Node> {
        if depth > 128 {
            return Err(invalid());
        }
        match events.next().ok_or_else(invalid)? {
            Event::Leaf(node) => Ok(node),
            Event::Sequence => {
                let mut items = Vec::new();
                while !matches!(events.peek(), Some(Event::EndSequence)) {
                    items.push(node(events, depth + 1)?);
                }
                events.next();
                Ok(Node::Sequence(items))
            }
            Event::Mapping => {
                let mut entries = Vec::new();
                while !matches!(events.peek(), Some(Event::EndMapping)) {
                    entries.push((node(events, depth + 1)?, node(events, depth + 1)?));
                }
                events.next();
                Ok(Node::Mapping(entries))
            }
            _ => Err(invalid()),
        }
    }
    let mut events = events.into_iter().peekable();
    let root = node(&mut events, 0)?;
    if events.next().is_some() {
        return Err(invalid());
    }
    Ok(root)
}

fn walk(
    node: &Node,
    value: &Value,
    path: &mut Vec<String>,
    merge_keys: bool,
    key: bool,
    visit: &mut impl FnMut(&Node, &Value, &[String], bool) -> Result<()>,
) -> Result<()> {
    visit(node, value, path, key)?;
    let value = if let Value::Tagged(tag) = value {
        &tag.value
    } else {
        value
    };
    match (node, value) {
        (Node::Mapping(nodes), Value::Mapping(values)) if nodes.len() == values.len() => {
            for ((key_node, value_node), (name, value)) in nodes.iter().zip(values) {
                walk(key_node, name, path, merge_keys, true, visit)?;
                let merge = merge_keys && name.as_str() == Some("<<");
                let merge_list = merge && value.is_sequence();
                if merge_list {
                    path.push("\u{1}".into());
                } else if !merge {
                    path.push(name.as_str().unwrap_or("\0").into());
                }
                walk(value_node, value, path, merge_keys, key, visit)?;
                if !merge || merge_list {
                    path.pop();
                }
            }
        }
        (Node::Sequence(nodes), Value::Sequence(values)) if nodes.len() == values.len() => {
            let merge = path.last().is_some_and(|value| value == "\u{1}");
            if merge {
                path.pop();
            }
            for (node, value) in nodes.iter().zip(values) {
                if !merge {
                    path.push("*".into());
                }
                walk(node, value, path, merge_keys, key, visit)?;
                if !merge {
                    path.pop();
                }
            }
            if merge {
                path.push("\u{1}".into());
            }
        }
        (Node::Scalar(_) | Node::Alias(_), _) => {}
        _ => return Err(invalid()),
    }
    Ok(())
}

fn flow(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).map_err(|_| invalid())?,
        Value::Sequence(values) => format!(
            "[{}]",
            values
                .iter()
                .map(flow)
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        Value::Mapping(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| Ok(format!("{}: {}", flow(key)?, flow(value)?)))
                .collect::<Result<Vec<_>>>()?
                .join(", ")
        ),
        Value::Tagged(value) => format!("{} {}", value.tag, flow(&value.value)?),
    })
}

fn contains_old(value: &Value, rebase: &DataPathRebase) -> Result<bool> {
    match value {
        Value::String(value) => Ok(rebase.config_value(value, str::to_owned)? != *value
            || rebase_sqlite_connection(value, rebase).is_ok_and(|updated| updated != *value)),
        Value::Sequence(values) => {
            for value in values {
                if contains_old(value, rebase)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Value::Mapping(values) => {
            for (key, value) in values {
                if contains_old(key, rebase)? || contains_old(value, rebase)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Value::Tagged(value) => contains_old(&value.value, rebase),
        _ => Ok(false),
    }
}

fn edits(source: &str, mut edits: Vec<(Range<usize>, String)>) -> Result<String> {
    edits.sort_by_key(|(range, _)| range.start);
    if edits.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
        return Err(invalid());
    }
    let mut output = source.to_string();
    for (range, value) in edits.into_iter().rev() {
        if output.get(range.clone()).is_none() {
            return Err(invalid());
        }
        output.replace_range(range, &value);
        if output.len() > 16 * 1024 * 1024 {
            return Err(invalid());
        }
    }
    Ok(output)
}

pub(crate) fn rebase(
    source: &str,
    service: &str,
    json: bool,
    rebase: &DataPathRebase,
) -> Result<String> {
    let original = parse(source, json)?;
    let mut root = original.clone();
    if !json {
        root.apply_merge().map_err(|_| invalid())?;
    }
    // 先判断实际配置是否需要变更；不改动纯凭据、注释或只有外部目录的文件。
    fn desired(
        value: &mut Value,
        path: &mut Vec<String>,
        service: &str,
        root: &Value,
        rebase: &DataPathRebase,
        merge_keys: bool,
    ) -> Result<()> {
        match value {
            Value::String(value) if path_field(service, path, root) => {
                *value = rebase_field(service, path, value, rebase)?
            }
            Value::Mapping(values) => {
                for (name, value) in values {
                    let merge = merge_keys && name.as_str() == Some("<<");
                    let merge_list = merge && value.is_sequence();
                    if merge_list {
                        path.push("\u{1}".into());
                    } else if !merge {
                        path.push(name.as_str().unwrap_or("\0").into());
                    }
                    desired(value, path, service, root, rebase, merge_keys)?;
                    if !merge || merge_list {
                        path.pop();
                    }
                }
            }
            Value::Sequence(values) => {
                let merge = path.last().is_some_and(|value| value == "\u{1}");
                if merge {
                    path.pop();
                }
                for value in values {
                    if !merge {
                        path.push("*".into());
                    }
                    desired(value, path, service, root, rebase, merge_keys)?;
                    if !merge {
                        path.pop();
                    }
                }
                if merge {
                    path.push("\u{1}".into());
                }
            }
            Value::Tagged(value) if value.value.is_mapping() || value.value.is_sequence() => {
                desired(&mut value.value, path, service, root, rebase, merge_keys)?
            }
            _ => {}
        }
        Ok(())
    }
    let mut expected = original.clone();
    desired(
        &mut expected,
        &mut Vec::new(),
        service,
        &root,
        rebase,
        !json,
    )?;
    if expected == original {
        return Ok(source.to_string());
    }
    // 先展开引用旧目录的别名，避免路径锚点的变更连带修改密码等其它使用者。
    let mut replacements = Vec::new();
    walk(
        &tree(source)?,
        &original,
        &mut Vec::new(),
        !json,
        false,
        &mut |node, value, _, _| {
            if let Node::Alias(range) = node {
                if contains_old(value, rebase)? {
                    replacements.push((range.clone(), flow(value)?));
                }
            }
            Ok(())
        },
    )?;
    let expanded = edits(source, replacements)?;
    if parse(&expanded, json)? != original {
        return Err(invalid());
    }
    let mut replacements = Vec::new();
    walk(
        &tree(&expanded)?,
        &original,
        &mut Vec::new(),
        !json,
        false,
        &mut |node, value, path, key| {
            if !key && path_field(service, path, &root) {
                if let (Node::Scalar(range), Value::String(value)) = (node, value) {
                    let updated = rebase_field(service, path, value, rebase)?;
                    if updated != *value {
                        let raw = &expanded[range.clone()];
                        let newline = if raw.ends_with("\r\n") {
                            "\r\n"
                        } else if raw.ends_with('\n') {
                            "\n"
                        } else {
                            ""
                        };
                        let comment = if raw.starts_with(['|', '>']) {
                            raw.lines()
                                .next()
                                .and_then(|line| {
                                    line.find('#').map(|at| format!(" {}", &line[at..]))
                                })
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        replacements.push((
                            range.clone(),
                            format!(
                                "{}{comment}{newline}",
                                serde_json::to_string(&updated).map_err(|_| invalid())?
                            ),
                        ));
                    }
                }
            }
            Ok(())
        },
    )?;
    let output = edits(&expanded, replacements)?;
    // 验证完整语义，任何非路径字段、别名或类型发生意外变化，都不落盘。
    if parse(&output, json)? != expected {
        return Err(invalid());
    }
    Ok(output)
}
