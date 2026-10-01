//! Redis 常用启动设置：只编辑已识别字段，复用配置历史与冲突检查。
use crate::{
    cfgeditor,
    error::{AppError, Result},
    paths::Paths,
    store::Store,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RedisSettings {
    pub max_memory_bytes: Option<u64>,
    pub eviction_policy: Option<String>,
    pub timeout_seconds: Option<u32>,
    pub max_clients: Option<u32>,
    /// None 沿用对应 Redis 版本默认值；Some([]) 显式关闭自动快照。
    pub save_rules: Option<Vec<SnapshotRule>>,
    pub append_fsync: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotRule {
    pub seconds: u32,
    pub changes: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisSettingsView {
    pub version: String,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub path: String,
    pub revision: String,
    pub settings: RedisSettings,
    /// AOF 开关不在普通配置表单中切换：已有数据需要在线迁移与完成状态核对。
    pub append_only: Option<bool>,
}

const FIELDS: [&str; 7] = [
    "maxmemory",
    "maxmemory-policy",
    "timeout",
    "maxclients",
    "save",
    "appendfsync",
    "appendonly",
];
const POLICIES: [&str; 8] = [
    "noeviction",
    "allkeys-lru",
    "allkeys-lfu",
    "allkeys-random",
    "volatile-lru",
    "volatile-lfu",
    "volatile-random",
    "volatile-ttl",
];
const MAX_MEMORY: u64 = 9_007_199_254_740_991;

fn unsupported(line: usize) -> AppError {
    AppError::new(
        "REDIS_SETTINGS_COMPLEX",
        format!("第 {line} 行配置无法由常用设置准确表示"),
    )
    .with_hint("请到工具箱的配置编辑器检查；当前文件未改动")
}

fn directive(line: &str, number: usize) -> Result<Option<(String, Vec<String>)>> {
    let text = line.trim();
    if text.is_empty() || text.starts_with('#') {
        return Ok(None);
    }
    if text.starts_with(['\'', '"']) {
        return Err(unsupported(number));
    }
    let key = text
        .split_ascii_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if key == "include" {
        return Err(AppError::new(
            "REDIS_SETTINGS_INCLUDE",
            "配置使用 include 引入其他文件，常用设置无法确定最终覆盖顺序",
        )
        .with_hint("请在配置编辑器中管理这些文件，避免保存后设置未按预期生效"));
    }
    if !FIELDS.contains(&key.as_str()) {
        return Ok(None);
    }
    let mut values = Vec::new();
    for part in text.split_ascii_whitespace().skip(1) {
        let value = if part.starts_with(['\'', '"']) {
            let quote = part.as_bytes()[0];
            if part.len() < 2 || part.as_bytes().last() != Some(&quote) {
                return Err(unsupported(number));
            }
            &part[1..part.len() - 1]
        } else {
            part
        };
        if value.contains(['\'', '"', '\\', '#']) {
            return Err(unsupported(number));
        }
        values.push(value.to_string());
    }
    Ok(Some((key, values)))
}

fn number(value: &str, line: usize) -> Result<u64> {
    if value.is_empty() || !value.bytes().all(|c| c.is_ascii_digit()) {
        return Err(unsupported(line));
    }
    value.parse().map_err(|_| unsupported(line))
}

fn memory(value: &str, line: usize) -> Result<u64> {
    let lower = value.to_ascii_lowercase();
    let split = lower
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(lower.len());
    let factor = match &lower[split..] {
        "" => 1,
        "k" => 1000,
        "kb" => 1024,
        "m" => 1_000_000,
        "mb" => 1024 * 1024,
        "g" => 1_000_000_000,
        "gb" => 1024 * 1024 * 1024,
        _ => return Err(unsupported(line)),
    };
    number(&lower[..split], line)?
        .checked_mul(factor)
        .filter(|n| *n <= MAX_MEMORY)
        .ok_or_else(|| unsupported(line))
}

pub(crate) fn parse(content: &str) -> Result<(RedisSettings, Option<bool>)> {
    let mut settings = RedisSettings::default();
    let mut append_only = None;
    for (index, line) in content.lines().enumerate() {
        let n = index + 1;
        let Some((key, values)) = directive(line, n)? else {
            continue;
        };
        if key == "save" {
            let rules = settings.save_rules.get_or_insert_with(Vec::new);
            if values == [""] {
                rules.clear();
            } else {
                if values.is_empty() || values.len() % 2 != 0 {
                    return Err(unsupported(n));
                }
                for pair in values.chunks_exact(2) {
                    let seconds =
                        u32::try_from(number(&pair[0], n)?).map_err(|_| unsupported(n))?;
                    let changes =
                        u32::try_from(number(&pair[1], n)?).map_err(|_| unsupported(n))?;
                    if seconds == 0 || seconds > i32::MAX as u32 || changes > i32::MAX as u32 {
                        return Err(unsupported(n));
                    }
                    rules.push(SnapshotRule { seconds, changes });
                }
            }
            if rules.len() > 16 {
                return Err(unsupported(n));
            }
            continue;
        }
        if values.len() != 1 {
            return Err(unsupported(n));
        }
        let value = &values[0];
        match key.as_str() {
            "maxmemory" => settings.max_memory_bytes = Some(memory(value, n)?),
            "maxmemory-policy" => settings.eviction_policy = Some(value.to_ascii_lowercase()),
            "timeout" => {
                settings.timeout_seconds =
                    Some(u32::try_from(number(value, n)?).map_err(|_| unsupported(n))?)
            }
            "maxclients" => {
                settings.max_clients =
                    Some(u32::try_from(number(value, n)?).map_err(|_| unsupported(n))?)
            }
            "appendfsync" => settings.append_fsync = Some(value.to_ascii_lowercase()),
            "appendonly" => {
                append_only = Some(match value.to_ascii_lowercase().as_str() {
                    "yes" => true,
                    "no" => false,
                    _ => return Err(unsupported(n)),
                })
            }
            _ => unreachable!(),
        }
    }
    Ok((settings, append_only))
}

fn validate(next: &RedisSettings, previous: &RedisSettings) -> Result<()> {
    let invalid = || {
        AppError::new(
            "REDIS_SETTINGS_INVALID",
            "请检查内存大小、连接限制、快照规则和持久化选项",
        )
    };
    if next.max_memory_bytes.is_some_and(|n| n > MAX_MEMORY)
        || next.timeout_seconds.is_some_and(|n| n > i32::MAX as u32)
        || next
            .max_clients
            .is_some_and(|n| n == 0 || n > i32::MAX as u32)
        || next.save_rules.as_ref().is_some_and(|r| {
            r.len() > 16
                || r.iter().any(|r| {
                    r.seconds == 0 || r.seconds > i32::MAX as u32 || r.changes > i32::MAX as u32
                })
        })
    {
        return Err(invalid());
    }
    // 新版 Redis 的额外策略可以按原样保留，不能提交任意指令或跨版本猜测新策略。
    if next.eviction_policy != previous.eviction_policy
        && next
            .eviction_policy
            .as_deref()
            .is_some_and(|p| !POLICIES.contains(&p))
    {
        return Err(invalid());
    }
    if next.append_fsync != previous.append_fsync
        && next
            .append_fsync
            .as_deref()
            .is_some_and(|v| !["always", "everysec", "no"].contains(&v))
    {
        return Err(invalid());
    }
    Ok(())
}

/// 仅替换有变化的指令；保留其他设置、注释、空行和原换行风格。
pub(crate) fn merge(
    content: &str,
    next: &RedisSettings,
    acknowledge_disable: bool,
) -> Result<String> {
    let (before, _) = parse(content)?;
    validate(next, &before)?;
    if next.save_rules.as_ref().is_some_and(Vec::is_empty)
        && !before.save_rules.as_ref().is_some_and(Vec::is_empty)
        && !acknowledge_disable
    {
        return Err(AppError::new(
            "REDIS_SNAPSHOT_CONFIRM",
            "关闭自动快照前，请确认已了解未持久化数据可能丢失",
        ));
    }
    let mut edits = std::collections::BTreeMap::<&str, Vec<String>>::new();
    for (key, a, b) in [
        (
            "maxmemory",
            before.max_memory_bytes.map(|v| v.to_string()),
            next.max_memory_bytes.map(|v| v.to_string()),
        ),
        (
            "maxmemory-policy",
            before.eviction_policy.clone(),
            next.eviction_policy.clone(),
        ),
        (
            "timeout",
            before.timeout_seconds.map(|v| v.to_string()),
            next.timeout_seconds.map(|v| v.to_string()),
        ),
        (
            "maxclients",
            before.max_clients.map(|v| v.to_string()),
            next.max_clients.map(|v| v.to_string()),
        ),
        (
            "appendfsync",
            before.append_fsync.clone(),
            next.append_fsync.clone(),
        ),
    ] {
        if a != b {
            edits.insert(
                key,
                b.map(|v| vec![format!("{key} {v}")]).unwrap_or_default(),
            );
        }
    }
    if next.save_rules != before.save_rules {
        edits.insert(
            "save",
            match &next.save_rules {
                None => vec![],
                Some(rules) if rules.is_empty() => vec!["save \"\"".into()],
                Some(rules) => rules
                    .iter()
                    .map(|r| format!("save {} {}", r.seconds, r.changes))
                    .collect(),
            },
        );
    }
    if edits.is_empty() {
        return Ok(content.into());
    }
    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut written = std::collections::BTreeSet::new();
    let mut output = String::new();
    for (index, line) in content.split_inclusive('\n').enumerate() {
        if let Some((key, _)) = directive(line, index + 1)? {
            if let Some(replacements) = edits.get(key.as_str()) {
                if written.insert(key.clone()) {
                    for replacement in replacements {
                        output.push_str(replacement);
                        output.push_str(newline);
                    }
                }
                continue;
            }
        }
        output.push_str(line);
    }
    for (key, replacements) in &edits {
        if !written.contains(*key) && !replacements.is_empty() {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push_str(newline);
            }
            for replacement in replacements {
                output.push_str(replacement);
                output.push_str(newline);
            }
        }
    }
    if parse(&output)?.0 != *next {
        return Err(AppError::new(
            "REDIS_SETTINGS_MERGE",
            "配置合并结果不一致，未写入文件",
        ));
    }
    Ok(output)
}

fn read(paths: &Paths, store: &Store, version: &str) -> Result<String> {
    store
        .find_installed("redis", Some(version))
        .ok_or_else(|| AppError::not_installed("Redis"))?;
    let file =
        crate::paths::checked_data_path(&paths.base, &format!("etc/redis/{version}/redis.conf"))?;
    let metadata = std::fs::symlink_metadata(&file).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::new("CONFIG_NOT_GENERATED", "该版本的 Redis 配置尚未生成")
                .with_hint("请先在套件页启动一次对应版本，再打开常用设置")
        } else {
            AppError::io("读取 Redis 配置", e)
        }
    })?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err(AppError::new(
            "REDIS_SETTINGS_FILE",
            "Redis 配置不是普通文件或超过 1 MiB",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(file)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(AppError::new("REDIS_SETTINGS_FILE", "Redis 配置超过 1 MiB"));
    }
    String::from_utf8(bytes).map_err(|_| {
        AppError::new(
            "REDIS_SETTINGS_ENCODING",
            "Redis 配置不是 UTF-8，请先在配置编辑器中处理",
        )
    })
}

fn view(paths: &Paths, version: &str, content: &str) -> Result<RedisSettingsView> {
    let (settings, append_only) = parse(content)?;
    Ok(RedisSettingsView {
        version: version.into(),
        path: crate::paths::portable_path_text(&paths.redis_conf(version)),
        revision: hex::encode(Sha256::digest(content.as_bytes())),
        settings,
        append_only,
    })
}

pub fn get(paths: &Paths, store: &Store, version: &str) -> Result<RedisSettingsView> {
    view(paths, version, &read(paths, store, version)?)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisPasswordView {
    pub version: String,
    pub revision: String,
    pub enabled: bool,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisPasswordSave {
    pub view: RedisPasswordView,
    /// 配置已原子保存；连接记录失败时单独反馈，允许使用新 revision 重试。
    pub connection_saved: bool,
}

/// 只判断现有密码是否为空，不向界面返回密码，也不把原文写入错误。
pub(crate) fn password_state(content: &str) -> Result<bool> {
    let mut enabled = false;
    for (index, line) in content.lines().enumerate() {
        let text = line.trim();
        if text.is_empty() || text.starts_with('#') { continue; }
        if text.starts_with(['\'', '"']) { return Err(unsupported(index + 1)); }
        let key = text.split_ascii_whitespace().next().unwrap_or_default();
        let value = text[key.len()..].trim();
        if key.eq_ignore_ascii_case("include") || key.eq_ignore_ascii_case("user")
            || (key.eq_ignore_ascii_case("aclfile") && !matches!(value, "\"\"" | "''")) {
            return Err(AppError::new("REDIS_PASSWORD_COMPLEX", "配置包含 include 或 ACL 用户规则，请在配置编辑器中管理认证，避免覆盖现有权限"));
        }
        if !key.eq_ignore_ascii_case("requirepass") { continue; }
        let bytes = value.as_bytes();
        if bytes.is_empty() { return Err(unsupported(index + 1)); }
        if matches!(bytes[0], b'\'' | b'"') {
            let quote = bytes[0];
            let mut i = 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && (quote == b'"' || bytes.get(i + 1) == Some(&b'\'')) {
                    i += 2; continue;
                }
                if bytes[i] == quote { break; }
                i += 1;
            }
            if i != bytes.len() - 1 || bytes[i] != quote { return Err(unsupported(index + 1)); }
            enabled = i > 1;
        } else {
            if value.chars().any(|c| c.is_whitespace() || matches!(c, '\'' | '"')) { return Err(unsupported(index + 1)); }
            enabled = true;
        }
    }
    Ok(enabled)
}

fn password_view(version: &str, content: &str) -> RedisPasswordView {
    let parsed = password_state(content);
    RedisPasswordView {
        version: version.into(),
        revision: hex::encode(Sha256::digest(content.as_bytes())),
        enabled: parsed.as_ref().copied().unwrap_or(false),
        blocked_reason: parsed.err().map(|error| error.message),
    }
}

pub fn password_get(paths: &Paths, store: &Store, version: &str) -> Result<RedisPasswordView> {
    Ok(password_view(version, &read(paths, store, version)?))
}

pub(crate) fn password_merge(content: &str, password: &str, acknowledge_disable: bool) -> Result<String> {
    password_state(content)?;
    if password.len() > 512 || password.chars().any(char::is_control)
        || (!password.is_empty() && password.trim().is_empty()) {
        return Err(AppError::new("REDIS_PASSWORD_INVALID", "密码须为 1–512 字节，不能仅为空白或包含控制字符"));
    }
    if password.is_empty() && !acknowledge_disable {
        return Err(AppError::new("REDIS_PASSWORD_CONFIRM", "关闭密码认证前，请确认了解所有可连接到此实例的客户端都将能够访问数据"));
    }
    // Redis 双引号支持反斜杠转义；不经 shell 传参，保留空格、# 和非 ASCII 字符。
    let quoted = password.replace('\\', "\\\\").replace('"', "\\\"");
    let newline = if content.contains("\r\n") { "\r\n" } else { "\n" };
    let replacement = format!("requirepass \"{quoted}\"{newline}");
    let mut output = String::new();
    let mut written = false;
    for line in content.split_inclusive('\n') {
        if line.split_ascii_whitespace().next().is_some_and(|key| key.eq_ignore_ascii_case("requirepass")) {
            if !written { output.push_str(&replacement); written = true; }
        } else { output.push_str(line); }
    }
    if !written {
        if !output.is_empty() && !output.ends_with('\n') { output.push_str(newline); }
        output.push_str(&replacement);
    }
    if password_state(&output)? != !password.is_empty() {
        return Err(AppError::new("REDIS_PASSWORD_INVALID", "密码配置合并失败，文件未改动"));
    }
    Ok(output)
}

/// 调用方必须持有生命周期锁并确认 Redis 停止，避免覆盖运行中实例的停机凭据。
pub(crate) fn password_save(paths: &Paths, store: &Store, version: &str, revision: &str, password: &str, acknowledge_disable: bool) -> Result<RedisPasswordSave> {
    let content = read(paths, store, version)?;
    if revision != hex::encode(Sha256::digest(content.as_bytes())) {
        return Err(AppError::new("CONFIG_CONFLICT", "Redis 配置已变化，未保存密码，请重新读取后确认"));
    }
    let merged = password_merge(&content, password, acknowledge_disable)?;
    cfgeditor::save_config_selected(paths, store, &format!("redis-conf@{version}"), &merged, false, Some(&content))?;
    let credentials = crate::stats::RedisCredentials { username: String::new(), password: password.into() };
    let connection_saved = store.set_setting_json(&crate::stats::RedisCredentials::key(version), &credentials).is_ok();
    Ok(RedisPasswordSave { view: password_view(version, &merged), connection_saved })
}

/// 调用方持有服务生命周期锁，与原始配置编辑器、重启生成和历史还原互斥。
pub fn save(
    paths: &Paths,
    store: &Store,
    version: &str,
    revision: &str,
    settings: &RedisSettings,
    acknowledge_disable: bool,
) -> Result<RedisSettingsView> {
    let content = read(paths, store, version)?;
    if revision != hex::encode(Sha256::digest(content.as_bytes())) {
        return Err(
            AppError::new("CONFIG_CONFLICT", "Redis 配置已变化，当前草稿未覆盖文件")
                .with_hint("请保留草稿后重新读取最新设置"),
        );
    }
    let merged = merge(&content, settings, acknowledge_disable)?;
    cfgeditor::save_config_selected(
        paths,
        store,
        &format!("redis-conf@{version}"),
        &merged,
        false,
        Some(&content),
    )?;
    view(paths, version, &merged)
}
