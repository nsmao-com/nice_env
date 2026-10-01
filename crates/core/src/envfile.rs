//! 站点 `.env` 管理。
//!
//! Laravel / Symfony / WordPress（用 wp-config 也好不到哪去）都要改 `.env`。
//! 用户改 `.env` 最容易踩的三个坑，这里都处理掉：
//! 1. **值里有空格或 # 但没加引号** → 被解析成注释或被截断；
//! 2. **改完忘了同步数据库连接串** → 站点连不上库，却是密码写错；
//! 3. **手改坏了没有退路** → 所以每次保存前备份。
//!
//! 另外做一件很实用的事：从站点绑定的数据库自动补全 DB_* 变量，
//! 不用用户自己去翻「数据库」页抄密码。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::paths::Paths;

/// 一条环境变量
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
    /// 是否为注释行（保留在文件里但不生效）
    pub commented: bool,
    /// 值看起来像敏感信息（密码 / 密钥 / token）—— 前端默认打码
    pub secret: bool,
    /// 该行在文件里的行号（1-based），用于精确定位
    pub line: usize,
    /// 值需要加引号但没加（保存时会修，或提示用户）
    pub needs_quote: bool,
}

/// 一个站点的 .env 文件
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvFileView {
    pub site_id: String,
    pub site_name: String,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub path: String,
    pub file_name: String,
    pub backup_exists: bool,
    pub has_compiled_env: bool,
    pub exists: bool,
    /// 绑定站点、项目目录、文件名、内容和语法，保存时必须仍与读取时一致。
    pub revision: String,
    pub entries: Vec<EnvEntry>,
    /// 站点绑定的数据库信息，可用于一键补全 DB_*
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_hint: Option<DbHint>,
    /// 探测到的 .env 变体文件（.env.example / .env.local …）
    #[serde(default)]
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvRestorePreview {
    pub file_name: String,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub backup_path: String,
    pub revision: String,
    pub current_exists: bool,
    pub content_changed: bool,
    pub changed_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbHint {
    pub database: String,
    pub username: String,
    pub password: String,
    pub port: u16,
}

/// 键名看起来是否敏感
pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    // 明确列出常见后缀/前缀，而不是模糊包含匹配，避免把 APP_KEY_ALGO 之类误判
    k.contains("PASSWORD")
        || k.contains("PASSWD")
        || k.ends_with("_PASS")
        || k == "DATABASE_URL"
        || k.contains("SECRET")
        || k.contains("_KEY")
        || k.ends_with("KEY")
        || k.contains("TOKEN")
        || k.contains("PRIVATE")
        || k == "APP_KEY"
        || k.contains("CREDENTIAL")
}

/// 值是否需要引号包裹。
///
/// dotenv 的规则：值里出现空格或 `#` 就必须加引号，否则
/// `NAME=my app#1` 会被解析成 `NAME=my app` 并把 `#1` 当注释。
pub fn needs_quoting(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    value.chars().any(|c| c.is_whitespace() || matches!(c, '#' | '"' | '\'' | '\\' | '$'))
        // 前后有空白也会被吃掉
        || value.trim() != value
}

/// 去掉值两端成对的引号
fn unquote(v: &str) -> String {
    let t = v.trim();
    if t.len() >= 2 {
        let b = t.as_bytes();
        if b[0] == b'\'' && b[t.len() - 1] == b'\'' {
            return t[1..t.len() - 1].to_string();
        }
        if b[0] == b'"' && b[t.len() - 1] == b'"' {
            let mut value = String::new();
            let mut chars = t[1..t.len() - 1].chars();
            while let Some(c) = chars.next() {
                if c != '\\' {
                    value.push(c);
                    continue;
                }
                match chars.next() {
                    Some('n') => value.push('\n'),
                    Some('r') => value.push('\r'),
                    Some('t') => value.push('\t'),
                    Some(c @ ('\\' | '"' | '$')) => value.push(c),
                    Some(c) => {
                        value.push('\\');
                        value.push(c);
                    }
                    None => value.push('\\'),
                }
            }
            return value;
        }
    }
    t.to_string()
}

fn quote_env_value(value: &str) -> String {
    if !needs_quoting(value) {
        return value.to_string();
    }
    // 密码中的 $ 必须保持字面值，不能被 PHP dotenv 当作变量展开。
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}

#[derive(Clone, Copy, PartialEq)]
enum EnvSyntax {
    Dotenv,
    ThinkPhp,
    CodeIgniter,
}

fn project_syntax(root: &Path) -> EnvSyntax {
    if root.join("think").is_file() {
        EnvSyntax::ThinkPhp
    } else if root.join("spark").is_file() {
        EnvSyntax::CodeIgniter
    } else {
        EnvSyntax::Dotenv
    }
}

fn quote_project_value(value: &str, syntax: EnvSyntax) -> String {
    if !needs_quoting(value) && !(syntax == EnvSyntax::ThinkPhp && value.contains(';')) {
        return value.to_string();
    }
    match syntax {
        // ThinkPHP 通过 INI_SCANNER_RAW 读取，双引号内的反斜杠和 $ 都是原值。
        EnvSyntax::ThinkPhp => format!("\"{value}\""),
        EnvSyntax::CodeIgniter => {
            format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
        }
        EnvSyntax::Dotenv => quote_env_value(value),
    }
}

pub(crate) fn apply_project_env_changes(
    root: &Path,
    original: &str,
    changes: &[(String, String)],
) -> Result<String> {
    let syntax = project_syntax(root);
    if let Some(record) = env_records(original, syntax).into_iter().find(|record| !record.entry.commented && !record.valid) {
        return Err(AppError::new("ENV_SYNTAX", format!("环境文件第 {} 行的 {} 引号或行尾格式不完整", record.entry.line, record.entry.key))
            .with_hint("请先在项目文件中修正这一行后重新读取；未覆盖原文件。"));
    }
    for (key, value) in changes {
        if !key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            return Err(AppError::new(
                "BAD_ENV_KEY",
                "环境变量名须以字母或下划线开头，且只允许字母、数字、下划线和点",
            ));
        }
        if value.contains('\0') {
            return Err(AppError::new("BAD_ENV_VALUE", "环境变量值不能包含空字符"));
        }
        if syntax != EnvSyntax::Dotenv && value.contains(['\n', '\r']) {
            return Err(AppError::new(
                "BAD_ENV_VALUE",
                "此框架的环境变量不能包含换行",
            ));
        }
        if is_secret_key(key)
            && matches!(
                value.to_ascii_lowercase().as_str(),
                "true"
                    | "false"
                    | "on"
                    | "off"
                    | "null"
                    | "empty"
                    | "(true)"
                    | "(false)"
                    | "(null)"
                    | "(empty)"
            )
        {
            return Err(
                AppError::new("BAD_ENV_VALUE", "框架会把此密码或密钥识别为布尔值或空值")
                    .with_hint("请使用包含更多字符的值，数据库密码可点击重新生成"),
            );
        }
        if syntax == EnvSyntax::CodeIgniter && is_secret_key(key) && value.contains("${") {
            return Err(AppError::new(
                "BAD_ENV_VALUE",
                "CodeIgniter 会展开密码中的 ${…} 环境变量表达式",
            )
            .with_hint("请使用不包含这种表达式的密码，或点击重新生成"));
        }
    }
    Ok(apply_env_changes_using(original, changes, syntax, &|value| {
        quote_project_value(value, syntax)
    }))
}

/// 解析 .env 内容。
///
/// 只处理 `KEY=VALUE` 与 `# 注释`；`export KEY=VALUE` 也认。
/// 保留注释行的原样（通过 line 与 commented 标记），保存时不丢用户注释。
pub fn parse_env(content: &str) -> Vec<EnvEntry> {
    parse_env_using(content, EnvSyntax::Dotenv)
}

struct EnvRecord {
    entry: EnvEntry,
    start: usize,
    end: usize,
    prefix: String,
    suffix: String,
    valid: bool,
}

fn env_value_end(value: &str, syntax: EnvSyntax) -> Option<usize> {
    let Some(quote) = value.chars().next() else { return Some(0); };
    if !matches!(quote, '\'' | '"') { return Some(value.find(if syntax == EnvSyntax::ThinkPhp { ';' } else { '#' }).unwrap_or(value.len())); }
    let mut escaped = false;
    for (offset, c) in value.char_indices().skip(1) {
        if escaped { escaped = false; continue; }
        if c == '\\' && syntax != EnvSyntax::ThinkPhp && (quote == '"' || syntax == EnvSyntax::CodeIgniter) {
            escaped = true; continue;
        }
        if c == quote { return Some(offset + 1); }
    }
    None
}

fn env_section(line: &str) -> Option<&str> {
    let (name, rest) = line.trim().strip_prefix('[')?.split_once(']')?;
    (!name.is_empty() && (rest.trim().is_empty() || rest.trim_start().starts_with([';', '#']))).then_some(name)
}

fn env_records(content: &str, syntax: EnvSyntax) -> Vec<EnvRecord> {
    let lines: Vec<_> = content.lines().collect();
    let mut records = Vec::new();
    let mut index = 0;
    let mut section = None;
    while index < lines.len() {
        let start = index;
        let raw = lines[index]; index += 1;
        let t = raw.trim_start_matches('\u{feff}').trim();
        if syntax == EnvSyntax::ThinkPhp {
            if let Some(name) = env_section(t) { section = Some(name); continue; }
        }
        let (body, commented) = t.strip_prefix('#').map(|rest| (rest.trim_start(), true)).unwrap_or((t, false));
        let body = body.strip_prefix("export ").unwrap_or(body);
        let Some((key, first_value)) = body.split_once('=') else { continue; };
        let key = key.trim();
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.')) { continue; }
        let mut text = first_value.trim_start().to_string();
        while !commented && env_value_end(&text, syntax).is_none() && index < lines.len() {
            text.push('\n'); text.push_str(lines[index]); index += 1;
        }
        let end = env_value_end(&text, syntax).unwrap_or(text.len());
        let raw_value = text[..end].trim_end();
        let suffix = text[raw_value.len()..].to_string();
        let quoted = raw_value.len() >= 2 && ((raw_value.starts_with('"') && raw_value.ends_with('"'))
            || (raw_value.starts_with('\'') && raw_value.ends_with('\'')));
        let value = match syntax {
            EnvSyntax::ThinkPhp if quoted && raw_value.starts_with('"') => raw_value[1..raw_value.len()-1].to_string(),
            EnvSyntax::ThinkPhp => raw_value.to_string(),
            EnvSyntax::CodeIgniter if quoted => raw_value[1..raw_value.len()-1]
                .replace(&format!("\\{}", &raw_value[..1]), &raw_value[..1]).replace("\\\\", "\\"),
            _ => unquote(raw_value),
        };
        let equals = raw.find('=').unwrap();
        let key = section.map(|section| format!("{section}.{key}")).unwrap_or_else(|| key.into());
        let comment = if syntax == EnvSyntax::ThinkPhp { ';' } else { '#' };
        records.push(EnvRecord {
            entry: EnvEntry { secret: is_secret_key(&key), key, value: value.clone(), commented,
                line: start+1, needs_quote: !commented && !quoted && (needs_quoting(&value) || (end == raw_value.len() && text[end..].starts_with(comment))) },
            start, end: index, prefix: raw[..=equals].into(),
            valid: env_value_end(&text, syntax).is_some() && (suffix.trim().is_empty() || suffix.trim_start().starts_with(comment)),
            suffix,
        });
    }
    records
}

fn parse_env_using(content: &str, syntax: EnvSyntax) -> Vec<EnvEntry> {
    env_records(content, syntax).into_iter().map(|record| record.entry).collect()
}

/// 更新所有同名的有效赋值；注释示例保持原样，新变量追加在文件末尾。
pub fn apply_env_changes(original: &str, changes: &[(String, String)]) -> String {
    apply_env_changes_using(original, changes, EnvSyntax::Dotenv, &quote_env_value)
}

fn apply_env_changes_using(
    original: &str,
    changes: &[(String, String)],
    syntax: EnvSyntax,
    quote: &dyn Fn(&str) -> String,
) -> String {
    if changes.is_empty() { return original.into(); }
    let lines: Vec<_> = original.lines().collect();
    let records = env_records(original, syntax);
    let mut applied = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0;
    for record in records.iter().filter(|record| !record.entry.commented) {
        let Some((key, value)) = changes.iter().find(|(key, _)| key == &record.entry.key) else { continue; };
        out.extend(lines[cursor..record.start].iter().map(|line| line.to_string()));
        out.push(format!("{}{}{}", record.prefix, quote(value), record.suffix));
        applied.insert(key.as_str()); cursor = record.end;
    }
    out.extend(lines[cursor..].iter().map(|line| line.to_string()));
    let remaining: Vec<_> = changes.iter().filter(|(key, _)| !applied.contains(key.as_str())).collect();
    if !remaining.is_empty() && syntax == EnvSyntax::ThinkPhp {
        // INI 的分节会持续到下一个分节；全局变量必须放在首个分节之前。
        // 已有分节以 section.key 显示；新增同分节变量放入对应分节。
        for (key, value) in &remaining {
            if let Some(index) = out.iter().rposition(|line| env_section(line).is_some_and(|section| key.strip_prefix(section).is_some_and(|rest| rest.starts_with('.')))) {
                let section = env_section(&out[index]).unwrap();
                let member = &key[section.len() + 1..];
                out.insert(index + 1, format!("{member}={}", quote(value))); continue;
            }
            let index = out.iter().position(|line| env_section(line).is_some()).unwrap_or(out.len());
            out.insert(index, format!("{key}={}", quote(value)));
        }
    } else if !remaining.is_empty() {
        if out.last().is_some_and(|line| !line.trim().is_empty()) { out.push(String::new()); }
        for (key, value) in &remaining { out.push(format!("{key}={}", quote(value))); }
    }
    let newline = if original.contains("\r\n") { "\r\n" } else { "\n" };
    let mut content = out.join(newline);
    if original.ends_with('\n') || !remaining.is_empty() { content.push_str(newline); }
    content
}

/// 生成一组与站点绑定数据库对齐的 DB_* 变量（Laravel 命名约定）
pub fn db_env_vars(hint: &DbHint) -> Vec<(String, String)> {
    vec![
        ("DB_CONNECTION".to_string(), "mysql".to_string()),
        ("DB_HOST".to_string(), "127.0.0.1".to_string()),
        ("DB_PORT".to_string(), hint.port.to_string()),
        ("DB_DATABASE".to_string(), hint.database.clone()),
        ("DB_USERNAME".to_string(), hint.username.clone()),
        ("DB_PASSWORD".to_string(), hint.password.clone()),
    ]
}

pub(crate) fn project_db_env_vars(root: &Path, hint: &DbHint) -> Result<Vec<(String, String)>> {
    let content = std::fs::read_to_string(root.join(".env")).ok();
    project_db_env_vars_from(root, hint, content.as_deref())
}

fn project_db_env_vars_from(root: &Path, hint: &DbHint, content: Option<&str>) -> Result<Vec<(String, String)>> {
    match project_syntax(root) {
        EnvSyntax::ThinkPhp => Ok(vec![
            ("DB_TYPE".into(), "mysql".into()),
            ("DB_HOST".into(), "127.0.0.1".into()),
            ("DB_PORT".into(), hint.port.to_string()),
            ("DB_NAME".into(), hint.database.clone()),
            ("DB_USER".into(), hint.username.clone()),
            ("DB_PASS".into(), hint.password.clone()),
            ("DB_CHARSET".into(), "utf8mb4".into()),
        ]),
        EnvSyntax::CodeIgniter => Ok(vec![
            ("database.default.hostname".into(), "127.0.0.1".into()),
            ("database.default.port".into(), hint.port.to_string()),
            ("database.default.database".into(), hint.database.clone()),
            ("database.default.username".into(), hint.username.clone()),
            ("database.default.password".into(), hint.password.clone()),
            ("database.default.DBDriver".into(), "MySQLi".into()),
        ]),
        EnvSyntax::Dotenv
            if root.join("bin/console").is_file() && root.join("config/bundles.php").is_file() =>
        {
            let existing = content.and_then(|s| {
                    parse_env(s)
                        .into_iter()
                        .find(|e| !e.commented && e.key == "DATABASE_URL")
                })
                .and_then(|e| reqwest::Url::parse(&e.value).ok())
                .filter(|url| url.scheme() == "mysql");
            if hint.password.is_empty()
                && existing.as_ref().and_then(|url| url.password()).is_none()
            {
                return Err(AppError::new(
                    "NO_DB_PASSWORD",
                    "当前未保存数据库密码，无法生成完整连接地址",
                )
                .with_hint("请保留现有连接配置，或先补充数据库凭据"));
            }
            let mut url = existing.unwrap_or(
                reqwest::Url::parse("mysql://127.0.0.1")
                    .map_err(|e| AppError::internal("生成数据库连接地址", e.to_string()))?,
            );
            url.set_host(Some("127.0.0.1"))
                .map_err(|e| AppError::internal("设置数据库地址", e.to_string()))?;
            url.set_port(Some(hint.port))
                .map_err(|_| AppError::new("BAD_PORT", "数据库端口无效"))?;
            url.set_username(&hint.username)
                .map_err(|_| AppError::new("BAD_USER", "数据库用户名无效"))?;
            if !hint.password.is_empty() {
                url.set_password(Some(&hint.password))
                    .map_err(|_| AppError::new("BAD_PASSWORD", "数据库密码无效"))?;
            }
            url.set_path(&hint.database);
            if url.query().is_none() {
                url.set_query(Some("charset=utf8mb4"));
            }
            Ok(vec![("DATABASE_URL".into(), url.to_string())])
        }
        _ => Ok(db_env_vars(hint)),
    }
}

/// .env 文件路径（站点根目录下）
pub fn env_path(root: &Path) -> PathBuf {
    root.join(".env")
}

/// public/out 等是对外目录，环境文件和版本配置位于包含项目清单的上一级目录。
pub(crate) fn project_root(web_root: &Path) -> PathBuf {
    if matches!(
        web_root.file_name().and_then(|name| name.to_str()),
        Some("public" | "out" | "dist" | "build")
    ) {
        if let Some(parent) = web_root.parent() {
            if [
                "composer.json",
                "package.json",
                "pyproject.toml",
                "requirements.txt",
                "go.mod",
                ".niceenv.json",
                ".nvmrc",
                ".node-version",
                ".python-version",
                "artisan",
                ".env",
                ".env.example",
                ".env.local",
            ]
            .iter()
            .any(|name| parent.join(name).is_file())
            {
                return parent.to_path_buf();
            }
        }
    }
    web_root.to_path_buf()
}

/// 探测站点目录下的 .env 变体
pub fn env_variants(root: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(root).into_iter().flatten().filter_map(|entry| {
        let entry = entry.ok()?;
        let name = entry.file_name().to_str()?.to_string();
        valid_env_name(&name).then_some(name)
    }).collect();
    names.sort(); names
}

fn valid_env_name(name: &str) -> bool {
    if name == ".env" { return true; }
    let Some(suffix) = name.strip_prefix(".env.") else { return false; };
    let reserved = name.to_ascii_lowercase();
    !suffix.is_empty() && name.len() <= 128 && !suffix.ends_with('.')
        && suffix.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !reserved.ends_with(".nsb-backup") && !reserved.ends_with(".nsb-before-restore") && reserved != ".env.local.php"
}

fn named_env_path(root: &Path, name: &str) -> Result<PathBuf> {
    if !valid_env_name(name) {
        return Err(AppError::new("BAD_ENV_FILE", "请选择项目目录中的 .env 或 .env.* 环境文件，不能选择路径、备份或编译缓存"));
    }
    Ok(root.join(name))
}

const MAX_ENV_BYTES: usize = 1024 * 1024;

pub(crate) fn read_env_file(path: &Path) -> Result<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::io("读取环境文件", e)),
    };
    let mut linked = metadata.file_type().is_symlink();
    #[cfg(windows)] {
        use std::os::windows::fs::MetadataExt;
        linked |= metadata.file_attributes() & 0x400 != 0;
    }
    if linked || !metadata.is_file() || metadata.len() > MAX_ENV_BYTES as u64 {
        return Err(AppError::new("ENV_INVALID_FILE", "环境文件及其备份必须是小于 1 MiB 的普通文件，不能是链接或目录")
            .with_hint(path.display().to_string()));
    }
    std::fs::read_to_string(path).map(Some).map_err(|e| AppError::io("读取 UTF-8 环境文件", e))
}

fn env_root(site: &crate::model::Site) -> Result<PathBuf> {
    if site.runtime.kind == crate::model::SiteKind::Redirect {
        return Err(AppError::new("SITE_NO_PROJECT", "跳转站点没有项目环境文件"));
    }
    if !Path::new(&site.root_dir).is_dir() {
        return Err(AppError::new("ROOT_MISSING", "站点根目录不存在")
            .with_hint("站点目录可能被移动或删除，请到站点详情里修正路径"));
    }
    std::fs::canonicalize(project_root(Path::new(&site.root_dir))).map_err(|e| AppError::io("读取项目目录", e))
}

fn env_revision(site: &crate::model::Site, root: &Path, name: &str, content: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    for value in [site.id.as_bytes(), site.root_dir.as_bytes(), root.to_string_lossy().as_bytes(), name.as_bytes()] {
        digest.update((value.len() as u64).to_le_bytes()); digest.update(value);
    }
    digest.update([project_syntax(root) as u8, u8::from(content.is_some())]);
    digest.update(content.unwrap_or_default().as_bytes());
    format!("{:x}", digest.finalize())
}

fn env_view(store: &crate::store::Store, site: &crate::model::Site, root: &Path, name: &str, content: Option<&str>) -> EnvFileView {
    let db_hint = site.db.as_ref().filter(|d| d.enabled).map(|d| DbHint {
        database: d.database.clone(),
        username: d.username.clone(),
        password: d.password.clone(),
        port: d.version.as_deref().and_then(|version| crate::dbadmin::saved_port(store, version))
            .or(d.port).unwrap_or_else(|| crate::services::PortsProfile::from_settings(store).mysql),
    });

    EnvFileView {
        site_id: site.id.clone(),
        site_name: site.name.clone(),
        path: root.join(name).to_string_lossy().to_string(),
        file_name: name.into(),
        backup_exists: std::fs::symlink_metadata(root.join(format!("{name}.nsb-backup"))).is_ok(),
        has_compiled_env: root.join(".env.local.php").is_file(),
        exists: content.is_some(),
        revision: env_revision(site, root, name, content),
        entries: parse_env_using(content.unwrap_or_default(), project_syntax(root)),
        db_hint,
        variants: env_variants(root),
    }
}

/// 读取和保存与站点目录变更串行；编辑器的 revision 还会检查外部文件变更。
pub fn read_env(_paths: &Paths, store: &crate::store::Store, site_id: &str) -> Result<EnvFileView> {
    read_env_named(_paths, store, site_id, ".env")
}

pub fn read_env_named(_paths: &Paths, store: &crate::store::Store, site_id: &str, name: &str) -> Result<EnvFileView> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let site = crate::sites::get(store, site_id)?;
    let root = env_root(&site)?;
    let content = read_env_file(&named_env_path(&root, name)?)?;
    Ok(env_view(store, &site, &root, name, content.as_deref()))
}

fn env_changed() -> AppError {
    AppError::new("ENV_CHANGED", "环境文件或站点目录已变化，未覆盖当前文件")
        .with_hint("请重新读取文件并检查最新内容，草稿不会自动覆盖外部修改。")
}

pub(crate) fn replace_env_file(path: &Path, expected: Option<&str>, content: &str) -> Result<()> {
    use std::io::Write;
    if read_env_file(path)?.as_deref() != expected { return Err(env_changed()); }
    let metadata = std::fs::metadata(path).ok();
    if metadata.as_ref().is_some_and(|m| m.permissions().readonly()) {
        return Err(AppError::new("ENV_READ_ONLY", "环境文件或备份是只读文件，未保存设置").with_hint(path.display().to_string()));
    }
    let mut pending = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    pending.write_all(content.as_bytes())?;
    if let Some(metadata) = metadata { pending.as_file().set_permissions(metadata.permissions())?; }
    pending.as_file().sync_all()?;
    if read_env_file(path)?.as_deref() != expected { return Err(env_changed()); }
    if expected.is_none() { pending.persist_noclobber(path).map_err(|e| AppError::io("创建环境文件", e.error))?; }
    else { pending.persist(path).map_err(|e| AppError::io("保存环境文件", e.error))?; }
    Ok(())
}

/// 保存 .env：校验读取版本，只修改指定键，原子替换并保留原文件备份。
pub fn save_env(
    _paths: &Paths,
    store: &crate::store::Store,
    site_id: &str,
    changes: &[(String, String)],
    expected_revision: &str,
) -> Result<EnvFileView> {
    save_env_named(_paths, store, site_id, ".env", changes, expected_revision)
}

pub fn save_env_named(_paths: &Paths, store: &crate::store::Store, site_id: &str, name: &str, changes: &[(String, String)], expected_revision: &str) -> Result<EnvFileView> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let site = crate::sites::get(store, site_id)?;
    let root = env_root(&site)?;
    let path = named_env_path(&root, name)?;
    let original = read_env_file(&path)?;
    if env_revision(&site, &root, name, original.as_deref()) != expected_revision { return Err(env_changed()); }
    let mut keys = std::collections::HashSet::new();
    if changes.iter().any(|(key, _)| !keys.insert(key)) {
        return Err(AppError::new("BAD_ENV_KEY", "同次保存不能提交重复变量名"));
    }
    if changes.is_empty() { return Ok(env_view(store, &site, &root, name, original.as_deref())); }
    let next = apply_project_env_changes(&root, original.as_deref().unwrap_or_default(), changes)?;
    if next.len() > MAX_ENV_BYTES { return Err(AppError::new("ENV_TOO_LARGE", "保存后的环境文件不能超过 1 MiB")); }
    let mut view = env_view(store, &site, &root, name, Some(&next));
    if !view.variants.iter().any(|file| file == name) { view.variants.push(name.into()); view.variants.sort(); }
    if original.as_deref() == Some(&next) { return Ok(view); }
    if std::fs::metadata(&path).ok().is_some_and(|m| m.permissions().readonly()) {
        return Err(AppError::new("ENV_READ_ONLY", format!("{name} 是只读文件，未保存设置")));
    }
    // 就地备份：.env 不进全局备份目录，放在项目旁边更直观
    if let Some(original) = &original {
        let bak = root.join(format!("{name}.nsb-backup"));
        let previous = read_env_file(&bak)?;
        replace_env_file(&bak, previous.as_deref(), original)?;
        view.backup_exists = true;
    }
    replace_env_file(&path, original.as_deref(), &next)?;
    Ok(view)
}

/// 返回待填入草稿的数据库变量，不写文件；仍须与编辑器读取的文件版本一致。
pub fn preview_db_vars(
    _paths: &Paths,
    store: &crate::store::Store,
    site_id: &str,
    expected_revision: &str,
) -> Result<Vec<(String, String)>> {
    preview_db_vars_named(_paths, store, site_id, ".env", expected_revision)
}

pub fn preview_db_vars_named(_paths: &Paths, store: &crate::store::Store, site_id: &str, name: &str, expected_revision: &str) -> Result<Vec<(String, String)>> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let site = crate::sites::get(store, site_id)?;
    let root = env_root(&site)?;
    let path = named_env_path(&root, name)?;
    let content = read_env_file(&path)?;
    let view = env_view(store, &site, &root, name, content.as_deref());
    if view.revision != expected_revision { return Err(env_changed()); }
    let hint = view.db_hint.ok_or_else(|| {
        AppError::new("NO_DB_BINDING", "该站点没有绑定数据库")
            .with_hint("先到站点详情里为它创建/绑定一个数据库")
    })?;
    let mut vars = project_db_env_vars_from(&root, &hint, content.as_deref())?;
    // 老版本未持久化密码：补全其它字段，不能用未知的空密码覆盖项目现有凭据。
    if hint.password.is_empty() {
        vars.retain(|(key, _)| {
            !matches!(
                key.as_str(),
                "DB_PASSWORD" | "DB_PASS" | "database.default.password"
            )
        });
    }
    if read_env_file(&path)? != content { return Err(env_changed()); }
    Ok(vars)
}

pub fn preview_env_restore(_paths: &Paths, store: &crate::store::Store, site_id: &str, name: &str, expected_revision: &str) -> Result<EnvRestorePreview> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let site = crate::sites::get(store, site_id)?;
    let root = env_root(&site)?;
    let content = read_env_file(&named_env_path(&root, name)?)?;
    if env_revision(&site, &root, name, content.as_deref()) != expected_revision { return Err(env_changed()); }
    let backup_name = format!("{name}.nsb-backup");
    let backup_path = root.join(&backup_name);
    let backup = read_env_file(&backup_path)?.ok_or_else(|| AppError::new("ENV_BACKUP_MISSING", "此环境文件没有上次保存的备份"))?;
    let values = |text: &str| -> std::collections::BTreeMap<String, Vec<String>> {
        let mut values: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for entry in parse_env_using(text, project_syntax(&root)).into_iter().filter(|entry| !entry.commented) {
            values.entry(entry.key).or_default().push(entry.value);
        }
        values
    };
    let current = values(content.as_deref().unwrap_or_default());
    let previous = values(&backup);
    let keys: std::collections::BTreeSet<_> = current.keys().chain(previous.keys()).cloned().collect();
    Ok(EnvRestorePreview {
        file_name: name.into(), backup_path: backup_path.to_string_lossy().into(),
        revision: env_revision(&site, &root, &backup_name, Some(&backup)),
        current_exists: content.is_some(), content_changed: content.as_deref() != Some(&backup),
        changed_keys: keys.into_iter().filter(|key| current.get(key) != previous.get(key)).collect(),
    })
}

/// 还原整份文件，保留用于还原的备份；当前内容另外保存为 nsb-before-restore。
pub fn restore_env(_paths: &Paths, store: &crate::store::Store, site_id: &str, name: &str, expected_revision: &str, expected_backup_revision: &str) -> Result<EnvFileView> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let site = crate::sites::get(store, site_id)?;
    let root = env_root(&site)?;
    let path = named_env_path(&root, name)?;
    let content = read_env_file(&path)?;
    if env_revision(&site, &root, name, content.as_deref()) != expected_revision { return Err(env_changed()); }
    let backup_name = format!("{name}.nsb-backup");
    let backup = read_env_file(&root.join(&backup_name))?.ok_or_else(|| AppError::new("ENV_BACKUP_MISSING", "备份已不存在，未还原文件"))?;
    if env_revision(&site, &root, &backup_name, Some(&backup)) != expected_backup_revision {
        return Err(AppError::new("ENV_BACKUP_CHANGED", "备份在预览后已变化，未还原文件").with_hint("请重新预览并确认备份。"));
    }
    let mut view = env_view(store, &site, &root, name, Some(&backup));
    if !view.variants.iter().any(|file| file == name) { view.variants.push(name.into()); view.variants.sort(); }
    if content.as_deref() == Some(&backup) { return Ok(view); }
    if let Some(content) = &content {
        let recovery = root.join(format!("{name}.nsb-before-restore"));
        let previous = read_env_file(&recovery)?;
        replace_env_file(&recovery, previous.as_deref(), content)?;
    }
    // 保留备份源原样；外部改动不以旧预览静默覆盖。
    if read_env_file(&root.join(&backup_name))?.as_deref() != Some(&backup) {
        return Err(AppError::new("ENV_BACKUP_CHANGED", "备份在还原前已变化，未还原文件"));
    }
    replace_env_file(&path, content.as_deref(), &backup)?;
    Ok(view)
}

/// 保留内部补全入口，按读取版本保存，文件改变时明确报错。
pub fn apply_db_vars(paths: &Paths, store: &crate::store::Store, site_id: &str) -> Result<Vec<String>> {
    let view = read_env(paths, store, site_id)?;
    let vars = preview_db_vars(paths, store, site_id, &view.revision)?;
    save_env(paths, store, site_id, &vars, &view.revision)?;
    Ok(vars.into_iter().map(|(k, _)| k).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_env() {
        let e = parse_env("APP_NAME=Demo\nAPP_ENV=local\n");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].key, "APP_NAME");
        assert_eq!(e[0].value, "Demo");
        assert_eq!(e[1].line, 2);
    }

    #[test]
    fn parses_quoted_and_comment_lines() {
        let e = parse_env("NAME=\"my app\"\n#DEBUG=true\n");
        assert_eq!(e[0].value, "my app");
        assert!(!e[0].needs_quote, "已加引号就不需要再加");
        assert_eq!(e[1].key, "DEBUG");
        assert!(e[1].commented);
    }

    #[test]
    fn detects_value_needing_quotes() {
        // 有空格但不加引号：dotenv 会截断
        let e = parse_env("NAME=my app\n");
        assert_eq!(e[0].value, "my app");
        assert!(e[0].needs_quote);
    }

    #[test]
    fn detects_hash_needing_quotes() {
        let e = parse_env("PASS=abc#1\n");
        assert!(e[0].needs_quote, "# 后的内容会被当注释");
    }

    #[test]
    fn no_quote_needed_for_plain_values() {
        for v in ["abc", "a-b_c.d", "1234", "a/b:c", ""] {
            let e = parse_env(&format!("K={v}\n"));
            if e.is_empty() {
                continue;
            }
            assert!(!e[0].needs_quote, "{v} 不需要引号");
        }
    }

    #[test]
    fn strips_export_prefix() {
        let e = parse_env("export PATH_EXTRA=/opt/bin\n");
        assert_eq!(e[0].key, "PATH_EXTRA");
        assert_eq!(e[0].value, "/opt/bin");
    }

    #[test]
    fn ignores_non_assignment_lines() {
        let e = parse_env("# just a title\n\nnot an assignment\nK=v\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].key, "K");
    }

    #[test]
    fn secret_keys_are_flagged() {
        for k in [
            "DB_PASSWORD",
            "APP_KEY",
            "JWT_SECRET",
            "AWS_SECRET_ACCESS_KEY",
            "API_TOKEN",
            "REDIS_PASSWORD",
        ] {
            assert!(is_secret_key(k), "{k} 应判为敏感");
        }
        for k in ["APP_NAME", "APP_ENV", "DB_HOST", "DB_PORT", "LOG_LEVEL"] {
            assert!(!is_secret_key(k), "{k} 不该判为敏感");
        }
    }

    #[test]
    fn apply_updates_existing_key_in_place() {
        let src = "APP_NAME=Old\nAPP_ENV=local\n";
        let out = apply_env_changes(src, &[("APP_NAME".into(), "New".into())]);
        assert!(out.contains("APP_NAME=New"));
        assert!(!out.contains("APP_NAME=Old"));
        assert!(out.contains("APP_ENV=local"), "其它键应保留");
        // 顺序不变
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("APP_NAME"));
    }

    #[test]
    fn apply_appends_new_keys_at_end() {
        let src = "APP_NAME=Old\n";
        let out = apply_env_changes(src, &[("DB_HOST".into(), "127.0.0.1".into())]);
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines[0], "APP_NAME=Old");
        assert_eq!(lines[1], "DB_HOST=127.0.0.1");
    }

    #[test]
    fn apply_preserves_comments_and_blank_lines() {
        let src = "# App\nAPP_NAME=Old\n\n# DB\nDB_HOST=x\n";
        let out = apply_env_changes(src, &[("APP_NAME".into(), "New".into())]);
        assert!(out.contains("# App"));
        assert!(out.contains("# DB"));
        // 空行结构保留（除了结尾）
        assert!(out.contains("New\n\n# DB"));
    }

    #[test]
    fn apply_quotes_values_that_need_it() {
        let out = apply_env_changes("K=1\n", &[("APP_NAME".into(), "my app".into())]);
        assert!(out.contains("APP_NAME=\"my app\""), "{out}");
    }

    #[test]
    fn apply_does_not_quote_plain_values() {
        let out = apply_env_changes("K=1\n", &[("DB_HOST".into(), "127.0.0.1".into())]);
        assert!(out.contains("DB_HOST=127.0.0.1"), "{out}");
        assert!(!out.contains("DB_HOST=\""));
    }

    #[test]
    fn apply_escapes_inner_quotes() {
        let out = apply_env_changes("", &[("K".into(), "say \"hi\" now".into())]);
        assert!(out.contains(r#"K="say \"hi\" now""#), "{out}");
        for value in [
            "literal${HOME}",
            "one\\two",
            "quote'and\"double",
            "line\nnext\rtab\tend",
            "密码 $secret # suffix",
        ] {
            let rendered = apply_env_changes("", &[("PASSWORD".into(), value.into())]);
            assert_eq!(parse_env(&rendered)[0].value, value);
            assert_eq!(
                apply_env_changes(&rendered, &[("PASSWORD".into(), value.into())]),
                rendered
            );
        }
    }

    #[test]
    fn apply_is_idempotent_for_same_change() {
        let src = "A=1\nB=2\n";
        let once = apply_env_changes(src, &[("A".into(), "9".into())]);
        let twice = apply_env_changes(&once, &[("A".into(), "9".into())]);
        assert_eq!(once, twice);
    }

    #[test]
    fn apply_multiple_changes_at_once() {
        let src = "DB_HOST=localhost\nDB_PORT=3306\n";
        let out = apply_env_changes(
            src,
            &[
                ("DB_HOST".into(), "127.0.0.1".into()),
                ("DB_PORT".into(), "23306".into()),
            ],
        );
        assert!(out.contains("DB_HOST=127.0.0.1"));
        assert!(out.contains("DB_PORT=23306"));
    }

    #[test]
    fn apply_handles_empty_original() {
        let out = apply_env_changes("", &[("A".into(), "1".into())]);
        assert_eq!(out.trim(), "A=1");
    }

    #[test]
    fn db_vars_use_laravel_conventions() {
        let hint = DbHint {
            database: "shop".into(),
            username: "shop_user".into(),
            password: "p@ss".into(),
            port: 3306,
        };
        let vars = db_env_vars(&hint);
        let map: std::collections::HashMap<_, _> = vars.into_iter().collect();
        assert_eq!(map.get("DB_CONNECTION").map(String::as_str), Some("mysql"));
        assert_eq!(map.get("DB_HOST").map(String::as_str), Some("127.0.0.1"));
        assert_eq!(map.get("DB_DATABASE").map(String::as_str), Some("shop"));
        assert_eq!(
            map.get("DB_USERNAME").map(String::as_str),
            Some("shop_user")
        );
        assert_eq!(map.get("DB_PORT").map(String::as_str), Some("3306"));
    }

    #[test]
    fn env_variants_detects_existing_only() {
        let t = std::env::temp_dir().join(format!("nsb-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join(".env"), "A=1").unwrap();
        std::fs::write(t.join(".env.example"), "A=").unwrap();
        let v = env_variants(&t);
        assert!(v.contains(&".env".to_string()));
        assert!(v.contains(&".env.example".to_string()));
        assert!(
            !v.contains(&".env.production".to_string()),
            "不存在的变体不该列出"
        );
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn save_env_rejects_missing_site_root() {
        let t = std::env::temp_dir().join(format!("nsb-env2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        let store = crate::store::Store::open(t.join("s.sqlite")).unwrap();
        // 站点不存在
        assert!(save_env(&paths, &store, "nope", &[], "missing").is_err());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn project_env_is_outside_public_and_unknown_password_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let public = temp.path().join("public");
        std::fs::create_dir_all(&public).unwrap();
        std::fs::write(temp.path().join("composer.json"), "{}").unwrap();
        assert_eq!(project_root(&public), temp.path());
        let paths = Paths::new(temp.path().join("app-data"));
        let store = crate::store::Store::open(paths.db()).unwrap();
        store.set_setting("portProfile", "safe").unwrap();
        let site: crate::model::Site = serde_json::from_value(serde_json::json!({
            "id":"env-site", "name":"Environment", "domains":["env.test"],
            "rootDir":public, "runtime":{"webServer":"nginx","kind":"php","phpVersion":"8.3.33"},
            "https":false, "rewrite":"laravel",
            "db":{"enabled":true,"database":"app","username":"app_user"},
            "createdAt":1, "updatedAt":1
        }))
        .unwrap();
        store.save_site(&site).unwrap();
        std::fs::write(
            temp.path().join(".env"),
            "DB_PASSWORD=keep-existing\nDB_PORT=3306\n",
        )
        .unwrap();
        apply_db_vars(&paths, &store, &site.id).unwrap();
        let content = std::fs::read_to_string(temp.path().join(".env")).unwrap();
        assert!(content.contains("DB_PASSWORD=keep-existing"));
        assert!(content.contains("DB_PORT=23306"));
        assert!(!public.join(".env").exists());
        assert!(temp.path().join(".env.nsb-backup").is_file());
    }

    #[test]
    fn unquote_handles_unescaped_pairs_only() {
        assert_eq!(unquote("\"abc\""), "abc");
        assert_eq!(unquote("'abc'"), "abc");
        assert_eq!(unquote("abc"), "abc");
        // 只有单边引号时不剥
        assert_eq!(unquote("\"abc"), "\"abc");
        assert_eq!(unquote("\""), "\"");
    }

    #[test]
    fn editor_preserves_comments_multiline_exports_and_line_endings() {
        let root = tempfile::tempdir().unwrap();
        let original = "# APP_NAME=example\r\nexport APP_NAME=old # project\r\nEMPTY=\r\nMULTI=\"first\r\nsecond\" # note\r\nAPP_NAME=last\r\nOTHER=keep\r\n";
        let parsed = parse_env(original);
        assert_eq!(parsed.iter().find(|entry| entry.key == "MULTI").unwrap().value, "first\nsecond");
        assert_eq!(parsed.iter().find(|entry| entry.key == "EMPTY").unwrap().value, "");
        let next = apply_project_env_changes(root.path(), original, &[("APP_NAME".into(), "new project".into()), ("MULTI".into(), "new\nlines".into())]).unwrap();
        assert!(next.starts_with("# APP_NAME=example\r\nexport APP_NAME=\"new project\" # project\r\nEMPTY=\r\n"));
        assert!(next.contains("MULTI=\"new\\nlines\" # note\r\n"));
        assert!(next.ends_with("APP_NAME=\"new project\"\r\nOTHER=keep\r\n"));
        let next = apply_env_changes("# API_TOKEN=example\r\n", &[("API_TOKEN".into(), "active".into())]);
        assert_eq!(next, "# API_TOKEN=example\r\n\r\nAPI_TOKEN=active\r\n");
        for invalid in ["MULTI=\"unfinished\nOTHER=keep\n", "NAME=\"value\"oops\n"] {
            assert_eq!(apply_project_env_changes(root.path(), invalid, &[("OTHER".into(), "new".into())]).unwrap_err().code, "ENV_SYNTAX");
        }
        assert_eq!(apply_env_changes("NAME=old", &[("NAME".into(), "new".into())]), "NAME=new");
        std::fs::write(root.path().join("think"), "").unwrap();
        let ini = "# project\r\n[DATABASE]\r\nHOST=db-host\r\n[REDIS]\r\nHOST=redis-host\r\n";
        let rows = parse_env_using(ini, EnvSyntax::ThinkPhp);
        assert_eq!(rows[0].key, "DATABASE.HOST"); assert_eq!(rows[1].key, "REDIS.HOST");
        let next = apply_project_env_changes(root.path(), ini, &[("DATABASE.HOST".into(), "database-new".into()), ("REDIS.PORT".into(), "6379".into()), ("DB_TYPE".into(), "mysql".into())]).unwrap();
        assert!(next.starts_with("# project\r\nDB_TYPE=mysql\r\n[DATABASE]\r\nHOST=database-new\r\n"));
        assert!(next.ends_with("[REDIS]\r\nPORT=6379\r\nHOST=redis-host\r\n"));
    }

    fn editor_site(root: &Path, store: &crate::store::Store) -> crate::model::Site {
        let site = serde_json::from_value(serde_json::json!({
            "id":"env-editor", "name":"Editor", "domains":["editor.test"], "rootDir":root,
            "runtime":{"webServer":"nginx","kind":"php","phpVersion":"8.4.26"}, "https":false,
            "rewrite":"none", "db":{"enabled":true,"database":"app","username":"app_user","password":"safe-password"},
            "createdAt":1, "updatedAt":1
        })).unwrap();
        store.save_site(&site).unwrap(); site
    }

    #[test]
    fn editor_preview_save_and_stale_directory_checks_are_real_file_operations() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("data"));
        let store = crate::store::Store::open(paths.db()).unwrap();
        let mut site = editor_site(temp.path(), &store);
        let path = temp.path().join(".env");
        let original = "# config\r\nAPP_NAME=before\r\nDB_PASSWORD=existing\r\n";
        std::fs::write(&path, original).unwrap();
        let view = read_env(&paths, &store, &site.id).unwrap();
        let values = preview_db_vars(&paths, &store, &site.id, &view.revision).unwrap();
        assert!(values.iter().any(|(key, value)| key == "DB_PASSWORD" && value == "safe-password"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!temp.path().join(".env.nsb-backup").exists());
        let changes = [("APP_NAME".into(), "after".into())];
        let saved = save_env(&paths, &store, &site.id, &changes, &view.revision).unwrap();
        assert_ne!(saved.revision, view.revision);
        assert_eq!(std::fs::read_to_string(temp.path().join(".env.nsb-backup")).unwrap(), original);
        assert_eq!(save_env(&paths, &store, &site.id, &changes, &view.revision).unwrap_err().code, "ENV_CHANGED");
        std::fs::write(&path, "APP_NAME=external\n").unwrap();
        assert_eq!(save_env(&paths, &store, &site.id, &changes, &saved.revision).unwrap_err().code, "ENV_CHANGED");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "APP_NAME=external\n");
        let before_move = read_env(&paths, &store, &site.id).unwrap();
        let next_root = temp.path().join("other"); std::fs::create_dir(&next_root).unwrap();
        std::fs::write(next_root.join(".env"), "APP_NAME=external\n").unwrap();
        site.root_dir = next_root.to_string_lossy().into(); store.save_site(&site).unwrap();
        assert_eq!(save_env(&paths, &store, &site.id, &changes, &before_move.revision).unwrap_err().code, "ENV_CHANGED");
        assert_eq!(preview_db_vars(&paths, &store, &site.id, &before_move.revision).unwrap_err().code, "ENV_CHANGED");
        assert_eq!(std::fs::read_to_string(next_root.join(".env")).unwrap(), "APP_NAME=external\n");
    }

    #[test]
    fn editor_rejects_unwritable_files_and_does_not_overwrite_new_files() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("data"));
        let store = crate::store::Store::open(paths.db()).unwrap();
        let site = editor_site(temp.path(), &store);
        let changes = [("NAME".into(), "new".into())];
        let missing = read_env(&paths, &store, &site.id).unwrap(); assert!(!missing.exists);
        let path = temp.path().join(".env");
        std::fs::write(&path, "NAME=external").unwrap();
        assert_eq!(save_env(&paths, &store, &site.id, &changes, &missing.revision).unwrap_err().code, "ENV_CHANGED");
        let view = read_env(&paths, &store, &site.id).unwrap();
        let permissions = std::fs::metadata(&path).unwrap().permissions();
        let mut readonly = permissions.clone(); readonly.set_readonly(true); std::fs::set_permissions(&path, readonly).unwrap();
        let result = save_env(&paths, &store, &site.id, &changes, &view.revision);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert_eq!(result.unwrap_err().code, "ENV_READ_ONLY");
        let backup = temp.path().join(".env.nsb-backup"); std::fs::create_dir(&backup).unwrap();
        assert_eq!(save_env(&paths, &store, &site.id, &changes, &view.revision).unwrap_err().code, "ENV_INVALID_FILE");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "NAME=external");
        std::fs::remove_dir(&backup).unwrap();
        for key in ["1NAME", "NAME\nOTHER", "NAME=OTHER"] {
            assert_eq!(save_env(&paths, &store, &site.id, &[(key.into(), "x".into())], &view.revision).unwrap_err().code, "BAD_ENV_KEY");
        }
        std::fs::remove_file(&path).unwrap();
        let missing = read_env(&paths, &store, &site.id).unwrap();
        let saved = save_env(&paths, &store, &site.id, &changes, &missing.revision).unwrap();
        assert!(saved.exists); assert!(saved.variants.contains(&".env".to_string()));
        assert!(!backup.exists());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "NAME=new\n");
    }

    #[test]
    fn named_editor_isolates_files_revisions_and_backups() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("data"));
        let store = crate::store::Store::open(paths.db()).unwrap();
        let site = editor_site(temp.path(), &store);
        let original = "# shared defaults\r\nAPP_NAME=old\r\n";
        for name in [".env", ".env.local"] { std::fs::write(temp.path().join(name), original).unwrap(); }
        let base = read_env(&paths, &store, &site.id).unwrap();
        let local = read_env_named(&paths, &store, &site.id, ".env.local").unwrap();
        assert_ne!(base.revision, local.revision);
        let changes = [("APP_NAME".into(), "local".into())];
        assert_eq!(save_env_named(&paths, &store, &site.id, ".env.local", &changes, &base.revision).unwrap_err().code, "ENV_CHANGED");
        let saved = save_env_named(&paths, &store, &site.id, ".env.local", &changes, &local.revision).unwrap();
        assert!(saved.backup_exists);
        assert_eq!(std::fs::read_to_string(temp.path().join(".env")).unwrap(), original);
        assert!(!temp.path().join(".env.nsb-backup").exists());
        assert_eq!(std::fs::read_to_string(temp.path().join(".env.local.nsb-backup")).unwrap(), original);
        let name = ".env.team-qa_2";
        let missing = read_env_named(&paths, &store, &site.id, name).unwrap();
        assert!(!missing.exists); assert!(!missing.variants.contains(&name.to_string()));
        preview_db_vars_named(&paths, &store, &site.id, name, &missing.revision).unwrap();
        save_env_named(&paths, &store, &site.id, name, &[], &missing.revision).unwrap();
        assert!(!temp.path().join(name).exists());
        let created = save_env_named(&paths, &store, &site.id, name, &changes, &missing.revision).unwrap();
        assert!(created.exists); assert!(!created.backup_exists); assert!(created.variants.contains(&name.to_string()));
        std::fs::write(temp.path().join(".env.local.php"), "<?php return []; ").unwrap();
        std::fs::write(temp.path().join(".env.local.nsb-before-restore"), "APP_NAME=old").unwrap();
        let view = read_env(&paths, &store, &site.id).unwrap();
        assert!(view.has_compiled_env);
        assert_eq!(view.variants, vec![".env", ".env.local", name]);
        for invalid in ["../.env", ".env/other", ".env.\\other", ".env.local:secret", ".env.", ".env.local.", ".env.local.php", ".env.LOCAL.PHP", ".env.local.NSB-BACKUP", ".env.NSB-BEFORE-RESTORE"] {
            assert_eq!(read_env_named(&paths, &store, &site.id, invalid).unwrap_err().code, "BAD_ENV_FILE", "{invalid}");
        }
        assert!(!valid_env_name(&format!(".env.{}", "a".repeat(124))));
    }

    #[test]
    fn selected_symfony_file_keeps_its_own_database_password() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("bin")).unwrap();
        std::fs::create_dir_all(temp.path().join("config")).unwrap();
        std::fs::write(temp.path().join("bin/console"), "").unwrap();
        std::fs::write(temp.path().join("config/bundles.php"), "<?php return []; ").unwrap();
        let paths = Paths::new(temp.path().join("data"));
        let store = crate::store::Store::open(paths.db()).unwrap();
        let mut site = editor_site(temp.path(), &store);
        site.db.as_mut().unwrap().password.clear(); store.save_site(&site).unwrap();
        std::fs::write(temp.path().join(".env"), "DATABASE_URL=mysql://old:base-secret@host/base\n").unwrap();
        std::fs::write(temp.path().join(".env.local"), "DATABASE_URL=mysql://old:local-secret@host/local\n").unwrap();
        let view = read_env_named(&paths, &store, &site.id, ".env.local").unwrap();
        let values = preview_db_vars_named(&paths, &store, &site.id, ".env.local", &view.revision).unwrap();
        assert_eq!(values.len(), 1); assert_eq!(values[0].0, "DATABASE_URL");
        let url = reqwest::Url::parse(&values[0].1).unwrap();
        assert_eq!(url.password(), Some("local-secret")); assert_eq!(url.username(), "app_user");
        assert!(!temp.path().join(".env.local.nsb-backup").exists());
        let missing = read_env_named(&paths, &store, &site.id, ".env.test").unwrap();
        assert_eq!(preview_db_vars_named(&paths, &store, &site.id, ".env.test", &missing.revision).unwrap_err().code, "NO_DB_PASSWORD");
    }

    #[test]
    fn restore_preserves_whole_backup_and_current_recovery_copy() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("data"));
        let store = crate::store::Store::open(paths.db()).unwrap();
        let site = editor_site(temp.path(), &store);
        let name = ".env.local";
        let path = temp.path().join(name);
        let backup = temp.path().join(format!("{name}.nsb-backup"));
        let recovery = temp.path().join(format!("{name}.nsb-before-restore"));
        let original = "# private config\r\nDB_PASSWORD=previous-secret\r\nDUP=1\r\nDUP=2\r\n";
        std::fs::write(&path, original).unwrap();
        let view = read_env_named(&paths, &store, &site.id, name).unwrap();
        let view = save_env_named(&paths, &store, &site.id, name, &[("DB_PASSWORD".into(), "current-secret".into()), ("DUP".into(), "2".into())], &view.revision).unwrap();
        let current = std::fs::read_to_string(&path).unwrap();
        let preview = preview_env_restore(&paths, &store, &site.id, name, &view.revision).unwrap();
        assert_eq!(preview.changed_keys, vec!["DB_PASSWORD", "DUP"]);
        assert!(preview.content_changed); assert!(preview.current_exists);
        let json = serde_json::to_string(&preview).unwrap();
        assert!(!json.contains("previous-secret")); assert!(!json.contains("current-secret"));
        assert!(!recovery.exists()); assert_eq!(std::fs::read_to_string(&path).unwrap(), current);
        let restored = restore_env(&paths, &store, &site.id, name, &view.revision, &preview.revision).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
        assert_eq!(std::fs::read_to_string(&recovery).unwrap(), current);
        let same = preview_env_restore(&paths, &store, &site.id, name, &restored.revision).unwrap();
        assert!(!same.content_changed); assert!(same.changed_keys.is_empty());
        restore_env(&paths, &store, &site.id, name, &restored.revision, &same.revision).unwrap();
        assert_eq!(std::fs::read_to_string(&recovery).unwrap(), current);
        std::fs::write(&path, original.replace("private config", "changed comment")).unwrap();
        let view = read_env_named(&paths, &store, &site.id, name).unwrap();
        let preview = preview_env_restore(&paths, &store, &site.id, name, &view.revision).unwrap();
        assert!(preview.content_changed); assert!(preview.changed_keys.is_empty());
        std::fs::remove_file(&path).unwrap();
        let missing = read_env_named(&paths, &store, &site.id, name).unwrap();
        let preview = preview_env_restore(&paths, &store, &site.id, name, &missing.revision).unwrap();
        assert!(!preview.current_exists);
        let restored = restore_env(&paths, &store, &site.id, name, &missing.revision, &preview.revision).unwrap();
        assert!(restored.exists); assert!(restored.variants.contains(&name.into()));
        assert_eq!(std::fs::read_to_string(&recovery).unwrap(), current);
    }

    #[test]
    fn restore_rejects_stale_previews_and_unwritable_targets() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("data"));
        let store = crate::store::Store::open(paths.db()).unwrap();
        let site = editor_site(temp.path(), &store);
        let path = temp.path().join(".env");
        let backup = temp.path().join(".env.nsb-backup");
        let recovery = temp.path().join(".env.nsb-before-restore");
        std::fs::write(&path, "NAME=current\n").unwrap();
        std::fs::write(&backup, "NAME=previous\n").unwrap();
        let view = read_env(&paths, &store, &site.id).unwrap();
        let preview = preview_env_restore(&paths, &store, &site.id, ".env", &view.revision).unwrap();
        std::fs::write(&backup, "NAME=external\n").unwrap();
        assert_eq!(restore_env(&paths, &store, &site.id, ".env", &view.revision, &preview.revision).unwrap_err().code, "ENV_BACKUP_CHANGED");
        assert!(!recovery.exists());
        std::fs::write(&backup, "NAME=previous\n").unwrap();
        std::fs::write(&path, "NAME=external\n").unwrap();
        assert_eq!(restore_env(&paths, &store, &site.id, ".env", &view.revision, &preview.revision).unwrap_err().code, "ENV_CHANGED");
        std::fs::write(&path, "NAME=current\n").unwrap();
        std::fs::create_dir(&recovery).unwrap();
        assert_eq!(restore_env(&paths, &store, &site.id, ".env", &view.revision, &preview.revision).unwrap_err().code, "ENV_INVALID_FILE");
        std::fs::remove_dir(&recovery).unwrap();
        let permissions = std::fs::metadata(&path).unwrap().permissions();
        let mut readonly = permissions.clone(); readonly.set_readonly(true); std::fs::set_permissions(&path, readonly).unwrap();
        let result = restore_env(&paths, &store, &site.id, ".env", &view.revision, &preview.revision);
        std::fs::set_permissions(&path, permissions).unwrap();
        assert_eq!(result.unwrap_err().code, "ENV_READ_ONLY");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "NAME=current\n");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "NAME=previous\n");
        std::fs::remove_file(&backup).unwrap(); std::fs::create_dir(&backup).unwrap();
        assert_eq!(preview_env_restore(&paths, &store, &site.id, ".env", &view.revision).unwrap_err().code, "ENV_INVALID_FILE");
        assert_eq!(restore_env(&paths, &store, &site.id, ".env", &view.revision, &preview.revision).unwrap_err().code, "ENV_INVALID_FILE");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "NAME=current\n");
    }

    #[test]
    #[ignore = "requires NSB_ENV_PHP and NSB_ENV_LARAVEL with installed phpdotenv"]
    fn native_env_editor_output_is_readable_by_phpdotenv() {
        let php = std::env::var_os("NSB_ENV_PHP").expect("NSB_ENV_PHP");
        let project = PathBuf::from(std::env::var_os("NSB_ENV_LARAVEL").expect("NSB_ENV_LARAVEL"));
        assert!(project.join("vendor/autoload.php").is_file());
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("data"));
        let store = crate::store::Store::open(paths.db()).unwrap();
        let site = editor_site(temp.path(), &store);
        let path = temp.path().join(".env");
        std::fs::write(&path, "# NAME=example\r\nexport NAME=old # app\r\nEMPTY=\r\nMULTI=\"original\r\nlines\" # retain comment\r\nUNCHANGED=kept\r\n").unwrap();
        let view = read_env(&paths, &store, &site.id).unwrap();
        let password = r#"secret ${HOME} # 中文 \ "quote""#;
        save_env(&paths, &store, &site.id, &[("NAME".into(), "My app".into()), ("DB_PASSWORD".into(), password.into()), ("MULTI".into(), "one\ntwo".into())], &view.revision).unwrap();
        let output = std::process::Command::new(&php).arg("-n").arg("-r")
            .arg(r#"require $argv[1]; echo json_encode(Dotenv\Dotenv::parse(file_get_contents($argv[2])), JSON_THROW_ON_ERROR);"#)
            .arg(project.join("vendor/autoload.php")).arg(&path).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let values: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(values["NAME"], "My app"); assert_eq!(values["DB_PASSWORD"], password);
        assert_eq!(values["MULTI"], "one\ntwo"); assert_eq!(values["EMPTY"], ""); assert_eq!(values["UNCHANGED"], "kept");
        let selected_path = temp.path().join(".env.local");
        std::fs::copy(&path, &selected_path).unwrap();
        let selected = read_env_named(&paths, &store, &site.id, ".env.local").unwrap();
        let saved = save_env_named(&paths, &store, &site.id, ".env.local", &[("NAME".into(), "Local app".into())], &selected.revision).unwrap();
        let preview = preview_env_restore(&paths, &store, &site.id, ".env.local", &saved.revision).unwrap();
        restore_env(&paths, &store, &site.id, ".env.local", &saved.revision, &preview.revision).unwrap();
        for (file, expected_name) in [(&selected_path, "My app"), (&temp.path().join(".env.local.nsb-before-restore"), "Local app")] {
            let output = std::process::Command::new(&php).arg("-n").arg("-r")
                .arg(r#"require $argv[1]; echo json_encode(Dotenv\Dotenv::parse(file_get_contents($argv[2])), JSON_THROW_ON_ERROR);"#)
                .arg(project.join("vendor/autoload.php")).arg(file).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            let values: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(values["NAME"], expected_name); assert_eq!(values["DB_PASSWORD"], password);
            assert_eq!(values["MULTI"], "one\ntwo");
        }
        std::fs::write(temp.path().join("think"), "").unwrap();
        std::fs::write(&path, "[DATABASE]\nHOST=db-host ; keep\n[REDIS]\nHOST=redis#host\n").unwrap();
        let view = read_env(&paths, &store, &site.id).unwrap();
        assert_eq!(view.entries.iter().find(|entry| entry.key == "REDIS.HOST").unwrap().value, "redis#host");
        save_env(&paths, &store, &site.id, &[("DATABASE.HOST".into(), "database;new".into()), ("REDIS.PORT".into(), "6379".into()), ("DB_TYPE".into(), "mysql".into())], &view.revision).unwrap();
        let output = std::process::Command::new(&php).arg("-n").arg("-r")
            .arg("echo json_encode(parse_ini_file($argv[1], true, INI_SCANNER_RAW), JSON_THROW_ON_ERROR);")
            .arg(&path).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let values: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(values["DATABASE"]["HOST"], "database;new"); assert_eq!(values["REDIS"]["HOST"], "redis#host");
        assert_eq!(values["REDIS"]["PORT"], "6379"); assert_eq!(values["DB_TYPE"], "mysql");
    }
}
