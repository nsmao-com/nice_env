//! Memcached 启动参数设置。
//!
//! Memcached 没有常驻配置文件，内存、连接数和线程数都通过启动参数生效。
//! 将设置按版本保存到本地配置库，启动时重新展开参数，避免用户手工编辑清单。

use crate::{
    error::{AppError, Result},
    store::Store,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MEMORY_MIN_MB: u32 = 64;
const MEMORY_MAX_MB: u32 = 1_048_576;
const CONNECTIONS_MIN: u32 = 16;
const CONNECTIONS_MAX: u32 = 1_000_000;
const THREADS_MIN: u32 = 1;
const THREADS_MAX: u32 = 64;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemcachedSettings {
    pub memory_mb: u32,
    pub max_connections: u32,
    pub threads: u32,
}

impl Default for MemcachedSettings {
    fn default() -> Self {
        Self {
            memory_mb: 512,
            max_connections: 1024,
            threads: 4,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemcachedSettingsView {
    pub version: String,
    pub revision: String,
    pub settings: MemcachedSettings,
    /// 参数在下次启动或重启 Memcached 时生效。
    pub restart_required: bool,
}

fn key(version: &str) -> String {
    format!("memcached-settings@{version}")
}

fn revision(settings: &MemcachedSettings) -> Result<String> {
    let bytes = serde_json::to_vec(settings)
        .map_err(|e| AppError::internal("序列化 Memcached 设置", e.to_string()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn validate(settings: &MemcachedSettings) -> Result<()> {
    if !(MEMORY_MIN_MB..=MEMORY_MAX_MB).contains(&settings.memory_mb)
        || !(CONNECTIONS_MIN..=CONNECTIONS_MAX).contains(&settings.max_connections)
        || !(THREADS_MIN..=THREADS_MAX).contains(&settings.threads)
    {
        return Err(AppError::new("MEMCACHED_SETTINGS_INVALID", "请检查内存、最大连接数和工作线程设置")
            .with_hint(format!("内存 {MEMORY_MIN_MB}–{MEMORY_MAX_MB} MB，连接数 {CONNECTIONS_MIN}–{CONNECTIONS_MAX}，线程 {THREADS_MIN}–{THREADS_MAX}")));
    }
    Ok(())
}

fn read(store: &Store, version: &str) -> Result<MemcachedSettings> {
    store
        .find_installed("memcached", Some(version))
        .ok_or_else(|| AppError::not_installed("Memcached"))?;
    let value = store.get_setting_checked(&key(version))?;
    let settings = match value {
        Some(raw) => serde_json::from_str(&raw).map_err(|_| {
            AppError::new(
                "MEMCACHED_SETTINGS_INVALID",
                "Memcached 设置记录已损坏，请恢复默认设置后重试",
            )
        })?,
        None => MemcachedSettings::default(),
    };
    validate(&settings)?;
    Ok(settings)
}

fn view(version: &str, settings: MemcachedSettings) -> Result<MemcachedSettingsView> {
    Ok(MemcachedSettingsView {
        version: version.into(),
        revision: revision(&settings)?,
        settings,
        restart_required: true,
    })
}

pub fn get(store: &Store, version: &str) -> Result<MemcachedSettingsView> {
    view(version, read(store, version)?)
}

pub fn save(
    store: &Store,
    version: &str,
    expected_revision: &str,
    settings: &MemcachedSettings,
) -> Result<MemcachedSettingsView> {
    let current = read(store, version)?;
    if revision(&current)? != expected_revision {
        return Err(
            AppError::new("CONFIG_CONFLICT", "Memcached 设置已变化，请重新读取后保存")
                .with_hint("当前草稿没有覆盖最新设置"),
        );
    }
    validate(settings)?;
    store.set_setting_json(&key(version), settings)?;
    view(version, settings.clone())
}

/// 将受控设置覆盖到清单启动参数，保留其它参数和参数顺序。
pub fn apply_args(store: &Store, version: &str, mut args: Vec<String>) -> Result<Vec<String>> {
    let settings = read(store, version)?;
    set_option(&mut args, "-m", settings.memory_mb.to_string());
    set_option(&mut args, "-c", settings.max_connections.to_string());
    set_option(&mut args, "-t", settings.threads.to_string());
    Ok(args)
}

fn set_option(args: &mut Vec<String>, flag: &str, value: String) {
    if let Some(index) = args.iter().position(|arg| arg == flag) {
        if let Some(current) = args.get_mut(index + 1) {
            *current = value;
            return;
        }
    }
    args.extend([flag.to_string(), value]);
}
