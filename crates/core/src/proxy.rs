//! Clash (mihomo) 模块：内核启停（走 services）、系统代理开关、订阅导入、节点查询/切换/测速。

use crate::configgen;
use crate::error::{AppError, Result};
use crate::paths::Paths;
use crate::store::Store;
use base64::Engine;
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use crate::services::ServiceManager;

pub struct ProxyRuntime {
    pub base_url: String,
    client: reqwest::blocking::Client,
}

impl ProxyRuntime {
    pub fn new() -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{}", configgen::MIHOMO_CONTROLLER_PORT),
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(8))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new()),
        }
    }

    fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let resp = self
            .client
            .get(format!("{}{path}", self.base_url))
            .send()
            .map_err(|e| {
                AppError::new("MIHOMO_API", format!("无法连接 mihomo 控制接口：{e}"))
                    .with_hint("请确认内核已启动；端口被占用时可在套件页查看冲突")
            })?;
        let resp = resp.error_for_status().map_err(|e| AppError::new("MIHOMO_API", format!("mihomo 控制接口返回错误：{e}")))?;
        resp.json::<T>()
            .map_err(|e| AppError::internal("解析 mihomo 响应", e.to_string()))
    }

    pub fn version(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct V {
            version: String,
        }
        Ok(self.get_json::<V>("/version")?.version)
    }

    pub fn proxies(&self) -> Result<serde_json::Value> {
        self.get_json("/proxies")
    }

    /// 实时连接（含累计上下行流量与每条连接明细）
    pub fn connections(&self) -> Result<serde_json::Value> {
        self.get_json("/connections")
    }

    pub fn mode(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct Config { mode: String }
        let mode = self.get_json::<Config>("/configs")?.mode;
        if !matches!(mode.as_str(), "rule" | "global" | "direct") {
            return Err(AppError::new("MIHOMO_API", "内核返回了未知的代理模式"));
        }
        Ok(mode)
    }

    /// 原生 API 热重载，失败时调用方恢复旧配置，避免停掉系统代理所依赖的进程。
    pub fn reload(&self, content: &str) -> Result<()> {
        let response = self.client.put(format!("{}/configs?force=true", self.base_url))
            .json(&serde_json::json!({ "payload": content }))
            .send().map_err(|e| AppError::new("PROXY_RELOAD_FAILED", format!("代理配置生效失败：{e}")))?;
        if !response.status().is_success() {
            return Err(AppError::new("PROXY_RELOAD_FAILED", format!("代理配置未被内核接受（HTTP {}）", response.status())));
        }
        Ok(())
    }

    /// 切换 Selector 节点
    pub fn select(&self, group: &str, node: &str) -> Result<()> {
        let resp = self
            .client
            .put(format!("{}/proxies/{}", self.base_url, urlencode(group)))
            .json(&serde_json::json!({ "name": node }))
            .send()
            .map_err(|e| AppError::internal("切换节点请求", e.to_string()))?;
        if !resp.status().is_success() {
            return Err(AppError::new(
                "SELECT_FAILED",
                format!("切换节点失败（HTTP {}）", resp.status()),
            ));
        }
        Ok(())
    }

    /// 修改运行中的 mihomo 模式。调用方只有在真正收到成功响应后才应持久化设置，
    /// 否则页面会显示已经切换，但内核仍在使用旧模式。
    pub fn set_mode(&self, mode: &str) -> Result<()> {
        if !matches!(mode, "rule" | "global" | "direct") {
            return Err(AppError::new("BAD_PROXY_MODE", "代理模式无效"));
        }
        let resp = self
            .client
            .patch(format!("{}/configs", self.base_url))
            .json(&serde_json::json!({ "mode": mode }))
            .send()
            .map_err(|e| AppError::new("MODE_FAILED", format!("切换代理模式失败：{e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp.text().unwrap_or_default();
            return Err(AppError::new(
                "MODE_FAILED",
                format!("切换代理模式失败（HTTP {status}）"),
            )
            .with_detail(detail));
        }
        Ok(())
    }

    /// 节点延迟测试，返回毫秒；超时/失败返回 Err
    pub fn delay(&self, node: &str) -> Result<u32> {
        let url = format!(
            "{}/proxies/{}/delay?timeout=5000&url=https%3A%2F%2Fwww.gstatic.com%2Fgenerate_204",
            self.base_url,
            urlencode(node)
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .map_err(|e| AppError::internal("测速请求", e.to_string()))?;
        #[derive(Deserialize)]
        struct D {
            delay: Option<u32>,
            message: Option<String>,
        }
        let d: D = resp
            .json()
            .map_err(|e| AppError::internal("解析测速结果", e.to_string()))?;
        match d.delay {
            Some(ms) => Ok(ms),
            None => Err(AppError::new("DELAY_TIMEOUT", "测速超时或节点不可用")
                .with_detail(d.message.unwrap_or_default())),
        }
    }
}

fn urlencode(s: &str) -> String {
    // 简易百分号编码（节点名含 emoji/中文）
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/* ============ 订阅导入 / 更新（异步下载） ============ */

const MAX_SUBSCRIPTION_BYTES: usize = 8 * 1024 * 1024;

fn subscription_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| AppError::new("BAD_SUBSCRIPTION_URL", "请输入完整的 HTTP 或 HTTPS 订阅地址"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some() || raw.len() > 8192 {
        return Err(AppError::new("BAD_SUBSCRIPTION_URL", "订阅地址必须使用 HTTP 或 HTTPS，且不能包含用户名密码"));
    }
    Ok(url)
}

async fn fetch_subscription(raw_url: &str) -> Result<String> {
    let url = subscription_url(raw_url)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent(concat!("clash-verge NiceEnv/", env!("CARGO_PKG_VERSION")))
        .build().map_err(|e| AppError::internal("创建订阅客户端", e.to_string()))?;
    let mut resp = client.get(url).send().await.map_err(|e| {
        AppError::new("SUBSCRIPTION_DOWNLOAD_FAILED", format!("下载订阅失败：{}", e.without_url()))
    })?;
    if !resp.status().is_success() {
        return Err(AppError::new("SUBSCRIPTION_DOWNLOAD_FAILED", format!("订阅服务器返回 HTTP {}", resp.status())));
    }
    if resp.content_length().is_some_and(|n| n > MAX_SUBSCRIPTION_BYTES as u64) {
        return Err(AppError::new("SUBSCRIPTION_TOO_LARGE", "订阅文件超过 8 MB，请检查链接"));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| {
        AppError::new("SUBSCRIPTION_DOWNLOAD_FAILED", format!("读取订阅失败：{}", e.without_url()))
    })? {
        if bytes.len() + chunk.len() > MAX_SUBSCRIPTION_BYTES {
            return Err(AppError::new("SUBSCRIPTION_TOO_LARGE", "订阅文件超过 8 MB，请检查链接"));
        }
        bytes.extend_from_slice(&chunk);
    }
    decode_subscription(&bytes)
}

fn decode_subscription(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).map_err(|_| AppError::new("NOT_A_CLASH_CONFIG", "订阅不是 UTF-8 文本"))?
        .trim_start_matches('\u{feff}').trim().to_string();
    if text.is_empty() {
        return Err(AppError::new("NOT_A_CLASH_CONFIG", "订阅内容为空"));
    }
    if text.contains(':') || text.starts_with('{') { return Ok(text); }
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    for engine in [base64::engine::general_purpose::STANDARD, base64::engine::general_purpose::URL_SAFE] {
        if let Ok(decoded) = engine.decode(&cleaned) {
            return String::from_utf8(decoded).map_err(|_| AppError::new("NOT_A_CLASH_CONFIG", "订阅解码后不是 UTF-8 文本"));
        }
    }
    Ok(text)
}

fn profile_path(paths: &Paths, id: &str) -> Result<PathBuf> {
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)) {
        return Err(AppError::new("BAD_PROFILE_ID", "订阅标识无效"));
    }
    let path = paths.mihomo_dir().join("profiles").join(format!("{id}.yaml"));
    let relative = path.strip_prefix(&paths.base).map_err(|_| AppError::new("BAD_PROFILE_PATH", "订阅目录无效"))?;
    Ok(crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?)
}

fn profile_record(store: &Store, id: &str) -> Result<(String, String, String, bool, i64)> {
    store.list_proxy_profiles()?.into_iter().find(|(pid, ..)| pid == id)
        .ok_or_else(|| AppError::new("PROFILE_NOT_FOUND", "代理订阅不存在"))
}

fn optional_text(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AppError::io("读取代理配置", e)),
    }
}

pub fn configured_mode(store: &Store) -> Result<String> {
    Ok(store.get_setting_checked("proxyMode")?.unwrap_or_else(|| "rule".into()))
}

/// 已安装内核时使用其 -t 校验；没有内核时仍可保存经 YAML 结构校验的订阅，启动时再次验证。
pub(crate) fn validate_profile(paths: &Paths, store: &Store, content: &str) -> Result<()> {
    let (_, exe) = match crate::ops::mihomo_paths(store) {
        Ok(runtime) => runtime,
        Err(e) if e.code == "NOT_INSTALLED" => return Ok(()),
        Err(e) => return Err(e),
    };
    std::fs::create_dir_all(paths.mihomo_dir())?;
    let mut file = tempfile::Builder::new().prefix(".nsb-validate-").suffix(".yaml").tempfile_in(paths.mihomo_dir())?;
    file.write_all(content.as_bytes())?;
    file.flush()?;
    let (ok, _) = crate::cfgeditor::run_validator(platform::command(&exe)
        .current_dir(paths.mihomo_dir()).arg("-t").arg("-d").arg(paths.mihomo_dir()).arg("-f").arg(file.path()))?;
    if !ok {
        return Err(AppError::new("PROXY_CONFIG_INVALID", "订阅未通过 mihomo 原生校验，原配置未改变")
            .with_hint("请确认订阅适用于当前 mihomo 版本，检查节点、规则与 provider 配置"));
    }
    Ok(())
}

pub async fn import_profile(name: &str, url: &str, paths: &Paths, store: &Store, manager: &ServiceManager) -> Result<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
        return Err(AppError::new("BAD_PROFILE_NAME", "订阅名称需为 1–128 个字符，且不能包含换行符"));
    }
    let url = subscription_url(url)?.to_string();
    let raw = fetch_subscription(&url).await?;
    let _operation = manager.lifecycle.lock();
    let adapted = configgen::adapt_mihomo_profile(&raw, &configured_mode(store)?)?;
    validate_profile(paths, store, &adapted)?;
    let id = format!("profile-{}-{:016x}", crate::services::now_ms(), rand::random::<u64>());
    let file = profile_path(paths, &id)?;
    std::fs::create_dir_all(file.parent().unwrap())?;
    let mut temp = tempfile::NamedTempFile::new_in(file.parent().unwrap())?;
    temp.write_all(adapted.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(&file).map_err(|e| AppError::io("保存订阅配置", e.error))?;
    if let Err(error) = store.save_proxy_profile(&id, name, &url, false) {
        std::fs::remove_file(&file).map_err(|e| AppError::io("保存记录失败，清理订阅文件也失败", e))?;
        return Err(error);
    }
    Ok(id)
}

/// 主配置、运行内核与选中记录全部成功才返回；失败尝试恢复磁盘和运行配置。
fn apply_config(
    paths: &Paths, store: &Store, manager: &ServiceManager, content: &str,
    commit: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let running = manager.snapshot("mihomo").is_some_and(|s| s.state == crate::model::ServiceState::Running);
    apply_config_with(paths, store, content, running, |text| ProxyRuntime::new().reload(text), commit)
}

fn apply_config_with(
    paths: &Paths, store: &Store, content: &str, running: bool,
    reload: impl Fn(&str) -> Result<()>, commit: impl FnOnce() -> Result<()>,
) -> Result<()> {
    validate_profile(paths, store, content)?;
    let main = paths.mihomo_config();
    let previous = optional_text(&main)?;
    if running && previous.is_none() {
        return Err(AppError::new("CONFIG_NOT_GENERATED", "运行中的代理缺少配置文件，请先修复配置"));
    }
    crate::cfgeditor::write_generated_config(paths, "mihomo-config", &main, content, previous.as_deref())?;
    let applied = (|| {
        if running { reload(content)?; }
        commit()
    })();
    if let Err(error) = applied {
        let mut failures = Vec::new();
        let disk = match previous.as_deref() {
            Some(old) => crate::cfgeditor::write_generated_config(paths, "mihomo-config", &main, old, Some(content)),
            None => std::fs::remove_file(&main).map_err(|e| AppError::io("恢复代理配置", e)),
        };
        if let Err(e) = disk { failures.push(e.message); }
        if running {
            if let Some(old) = previous.as_deref() {
                if let Err(e) = reload(old) { failures.push(e.message); }
            }
        }
        if !failures.is_empty() {
            return Err(AppError::new("PROXY_RECOVERY_FAILED", format!("操作失败，旧配置恢复未完成：{}", failures.join("；")))
                .with_hint("请检查代理日志与配置备份；当前运行配置可能与所选订阅不同")
                .with_detail(error.message));
        }
        return Err(error.with_hint("已恢复原配置，请检查订阅后重试"));
    }
    Ok(())
}

pub fn activate_profile(paths: &Paths, store: &Store, manager: &ServiceManager, id: &str) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    let file = profile_path(paths, id)?;
    profile_record(store, id)?;
    let raw = std::fs::read_to_string(file).map_err(|e| AppError::io("读取订阅配置", e))?;
    let adapted = configgen::adapt_mihomo_profile(&raw, &configured_mode(store)?)?;
    apply_config(paths, store, manager, &adapted, || store.set_active_proxy_profile(id))
}

pub async fn update_profile(paths: &Paths, store: &Store, manager: &ServiceManager, id: &str) -> Result<bool> {
    let file = profile_path(paths, id)?;
    let target = profile_record(store, id)?;
    let previous = optional_text(&file)?;
    let raw = fetch_subscription(&target.2).await?;
    let _operation = manager.lifecycle.lock();
    let current = profile_record(store, id)?;
    if target.2 != current.2 || optional_text(&file)? != previous {
        return Err(AppError::new("PROFILE_CHANGED", "下载期间订阅已变化，请重新更新"));
    }
    let adapted = configgen::adapt_mihomo_profile(&raw, &configured_mode(store)?)?;
    validate_profile(paths, store, &adapted)?;
    crate::paths::write_with_backup(&file, &adapted, &paths.backup())?;
    if current.3 {
        if let Err(error) = apply_config(paths, store, manager, &adapted, || Ok(())) {
            let restored = match previous.as_deref() {
                Some(old) => crate::paths::write_with_backup(&file, old, &paths.backup()),
                None => std::fs::remove_file(&file),
            };
            if let Err(e) = restored {
                return Err(AppError::io("更新失败，恢复订阅文件也失败", e).with_detail(error.message));
            }
            return Err(error);
        }
    }
    Ok(current.3)
}

pub fn delete_profile(paths: &Paths, store: &Store, manager: &ServiceManager, id: &str) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    let file = profile_path(paths, id)?;
    if profile_record(store, id)?.3 {
        return Err(AppError::new("PROFILE_ACTIVE", "当前订阅正在使用，请先切换到其它订阅"));
    }
    let content = optional_text(&file)?;
    // 删除文件之前保留已有内容，数据库失败时可恢复；配置目录外的链接已被拒绝。
    if content.is_some() {
        std::fs::remove_file(&file).map_err(|e| AppError::io("删除订阅文件", e))?;
    }
    if let Err(error) = store.delete_proxy_profile(id) {
        if let Some(text) = content {
            crate::paths::write_with_backup(&file, &text, &paths.backup())
                .map_err(|e| AppError::io("删除失败，恢复订阅文件也失败", e))?;
        }
        return Err(error);
    }
    Ok(())
}

pub fn set_mode(paths: &Paths, store: &Store, manager: &ServiceManager, mode: &str) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    let raw = optional_text(&paths.mihomo_config())?.unwrap_or_else(configgen::render_mihomo_builtin_config);
    let adapted = configgen::adapt_mihomo_profile(&raw, mode)?;
    apply_config(paths, store, manager, &adapted, || store.set_setting("proxyMode", mode))
}

/* ============ 系统代理（带备份恢复） ============ */

pub fn system_proxy_on() -> Result<()> {
    platform::set_system_proxy(true, &format!("127.0.0.1:{}", configgen::MIHOMO_MIXED_PORT))
        .map_err(AppError::from)?;
    Ok(())
}

pub fn system_proxy_off() -> Result<()> {
    platform::set_system_proxy(false, "").map_err(AppError::from)?;
    Ok(())
}

pub fn system_proxy_state() -> platform::SystemProxyState {
    platform::get_system_proxy().unwrap_or(platform::SystemProxyState {
        enabled: false,
        server: String::new(),
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn fixture() -> (tempfile::TempDir, Paths, Store) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_owned());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        (temp, paths, store)
    }

    #[test]
    fn proxy_failed_activation_restores_disk_runtime_and_selection() {
        let (_temp, paths, store) = fixture();
        let original = configgen::render_mihomo_builtin_config();
        configgen::write_mihomo_config(&paths, &original).unwrap();
        store.save_proxy_profile("old", "Old", "https://example.test/old", true).unwrap();
        store.save_proxy_profile("new", "New", "https://example.test/new", false).unwrap();
        let candidate = configgen::adapt_mihomo_profile("proxies: []\nrules: [MATCH,DIRECT]", "direct").unwrap();
        let calls = RefCell::new(Vec::new());
        let error = apply_config_with(&paths, &store, &candidate, true, |text| {
            calls.borrow_mut().push(text.to_owned());
            if text == candidate { Err(AppError::new("RELOAD_FAILED", "fixture rejection")) } else { Ok(()) }
        }, || store.set_active_proxy_profile("new")).unwrap_err();
        assert_eq!(error.code, "RELOAD_FAILED");
        assert_eq!(std::fs::read_to_string(paths.mihomo_config()).unwrap(), original);
        assert_eq!(*calls.borrow(), vec![candidate, original]);
        assert!(profile_record(&store, "old").unwrap().3);
        assert!(!profile_record(&store, "new").unwrap().3);
    }

    #[test]
    fn proxy_record_failure_restores_runtime_and_keeps_active_record() {
        let (_temp, paths, store) = fixture();
        let original = configgen::render_mihomo_builtin_config();
        configgen::write_mihomo_config(&paths, &original).unwrap();
        store.save_proxy_profile("old", "Old", "https://example.test/old", true).unwrap();
        let calls = RefCell::new(Vec::new());
        let candidate = configgen::adapt_mihomo_profile(&original, "global").unwrap();
        let error = apply_config_with(&paths, &store, &candidate, true, |text| {
            calls.borrow_mut().push(text.to_owned()); Ok(())
        }, || store.set_active_proxy_profile("missing")).unwrap_err();
        assert_eq!(error.code, "PROFILE_NOT_FOUND");
        assert!(profile_record(&store, "old").unwrap().3);
        assert_eq!(calls.borrow().last(), Some(&original));
        assert_eq!(std::fs::read_to_string(paths.mihomo_config()).unwrap(), original);
    }

    #[test]
    fn proxy_failed_recovery_is_explicit_and_does_not_claim_success() {
        let (_temp, paths, store) = fixture();
        let original = configgen::render_mihomo_builtin_config();
        configgen::write_mihomo_config(&paths, &original).unwrap();
        let error = apply_config_with(&paths, &store, "rules: [MATCH,DIRECT]", true,
            |_| Err(AppError::new("OFFLINE", "fixture offline")), || panic!("must not commit")) .unwrap_err();
        assert_eq!(error.code, "PROXY_RECOVERY_FAILED");
        assert_eq!(std::fs::read_to_string(paths.mihomo_config()).unwrap(), original);
    }

    #[test]
    fn proxy_invalid_ids_and_missing_records_cannot_change_active_config() {
        let (_temp, paths, store) = fixture();
        let manager = ServiceManager::new();
        let original = configgen::render_mihomo_builtin_config();
        configgen::write_mihomo_config(&paths, &original).unwrap();
        store.save_proxy_profile("old", "Old", "https://example.test/old", true).unwrap();
        for id in ["../outside", "..\\outside", "", "name:stream", "a/b"] {
            assert!(activate_profile(&paths, &store, &manager, id).is_err());
            assert!(delete_profile(&paths, &store, &manager, id).is_err());
        }
        std::fs::create_dir_all(paths.mihomo_dir().join("profiles")).unwrap();
        std::fs::write(profile_path(&paths, "missing").unwrap(), "proxies: []").unwrap();
        assert_eq!(activate_profile(&paths, &store, &manager, "missing").unwrap_err().code, "PROFILE_NOT_FOUND");
        assert_eq!(delete_profile(&paths, &store, &manager, "old").unwrap_err().code, "PROFILE_ACTIVE");
        assert_eq!(std::fs::read_to_string(paths.mihomo_config()).unwrap(), original);
        assert!(profile_record(&store, "old").unwrap().3);
    }

    #[test]
    fn proxy_mode_is_persisted_and_subscription_deletion_removes_its_file() {
        let (_temp, paths, store) = fixture();
        let manager = ServiceManager::new();
        set_mode(&paths, &store, &manager, "direct").unwrap();
        assert_eq!(configured_mode(&store).unwrap(), "direct");
        let config: yaml_serde::Value = yaml_serde::from_str(&std::fs::read_to_string(paths.mihomo_config()).unwrap()).unwrap();
        assert_eq!(config["mode"].as_str(), Some("direct"));
        store.save_proxy_profile("remove", "Remove", "https://example.test/remove", false).unwrap();
        let file = profile_path(&paths, "remove").unwrap();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "proxies: []").unwrap();
        delete_profile(&paths, &store, &manager, "remove").unwrap();
        assert!(!file.exists());
        assert!(store.list_proxy_profiles().unwrap().is_empty());
    }

    #[test]
    fn proxy_subscription_decoding_and_url_validation_are_strict() {
        let yaml = "proxies: []\nrules: [MATCH,DIRECT]";
        assert_eq!(decode_subscription(yaml.as_bytes()).unwrap(), yaml);
        assert_eq!(decode_subscription(base64::engine::general_purpose::STANDARD.encode(yaml).as_bytes()).unwrap(), yaml);
        assert!(decode_subscription(b"\xff").is_err());
        assert!(decode_subscription(b"  ").is_err());
        for bad in ["file:///config.yaml", "ftp://example.test/a", "https://user:pass@example.test/sub", "bad"] {
            assert!(subscription_url(bad).is_err());
        }
        assert!(subscription_url("https://example.test/sub?token=fixture").is_ok());
        assert!(crate::serde_proxy::parse_groups(&serde_json::json!({"message":"unauthorized"})).is_err());
        let groups = crate::serde_proxy::parse_groups(&serde_json::json!({"proxies": {
            "Pick": {"name": "Pick", "type": "Selector", "now": "DIRECT", "all": ["DIRECT"]},
            "DIRECT": {"name": "DIRECT", "type": "Direct"}
        }})).unwrap();
        let payload = serde_json::to_value(groups).unwrap();
        assert_eq!(payload[0]["type"], "Selector");
        assert_eq!(payload[0]["nodes"][0]["type"], "Direct");
        assert!(payload[0].get("kind").is_none());
    }

    fn subscription_server(bodies: Vec<String>) -> (String, std::thread::JoinHandle<()>) {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/subscription?token=fixture-secret", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            for body in bodies {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut b = [0]; stream.read_exact(&mut b).unwrap(); bytes.push(b[0]);
                }
                let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (url, handle)
    }

    #[tokio::test]
    async fn proxy_import_and_update_download_real_bytes_and_reject_bad_replacement() {
        let (_temp, paths, store) = fixture();
        let manager = ServiceManager::new();
        let raw = "proxies: [{name: local, type: socks5, server: 127.0.0.1, port: 9091}]\nrules: [MATCH,DIRECT]";
        let (url, server) = subscription_server(vec![raw.into(), "<html>proxies broken</html>".into()]);
        let id = import_profile(" Fixture ", &url, &paths, &store, &manager).await.unwrap();
        let file = profile_path(&paths, &id).unwrap();
        let original = std::fs::read_to_string(&file).unwrap();
        assert_eq!(profile_record(&store, &id).unwrap().1, "Fixture");
        let value: yaml_serde::Value = yaml_serde::from_str(&original).unwrap();
        assert_eq!(value["proxies"][0]["port"].as_u64(), Some(9091));
        activate_profile(&paths, &store, &manager, &id).unwrap();
        assert!(profile_record(&store, &id).unwrap().3);
        let before = std::fs::read(paths.mihomo_config()).unwrap();
        assert_eq!(update_profile(&paths, &store, &manager, &id).await.unwrap_err().code, "NOT_A_CLASH_CONFIG");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        assert_eq!(std::fs::read(paths.mihomo_config()).unwrap(), before);
        assert!(profile_record(&store, &id).unwrap().3);
        server.join().unwrap();
    }

    #[tokio::test]
    async fn proxy_download_failure_does_not_expose_subscription_token() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/private?token=fixture-secret", listener.local_addr().unwrap());
        drop(listener);
        let error = fetch_subscription(&url).await.unwrap_err();
        let text = serde_json::to_string(&error).unwrap();
        assert!(!text.contains("fixture-secret"));
        assert!(!text.contains("/private"));
    }

    #[test]
    #[ignore = "requires NSB_MIHOME_TEST_EXE; runs only native -t validation, no service"]
    fn proxy_native_mihomo_validates_adapted_nodes_and_rejects_invalid_config() {
        let exe = PathBuf::from(std::env::var_os("NSB_MIHOME_TEST_EXE").expect("set native fixture path"));
        let (_temp, paths, store) = fixture();
        let runtime = paths.runtime_dir("mihomo", "fixture");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::copy(exe, runtime.join(crate::ops::exe_name("mihomo"))).unwrap();
        store.upsert_installed(&crate::model::InstalledPackage {
            id: "mihomo".into(), version: "fixture".into(), category: "tool".into(),
            install_path: runtime.to_string_lossy().into(), config_path: String::new(), installed_at: 0,
        }).unwrap();
        let raw = "proxies: [{name: fixture, type: socks5, server: 127.0.0.1, port: 9091}]\nproxy-groups: [{name: Pick, type: select, proxies: [fixture, DIRECT]}]\nrules: [\"MATCH,DIRECT\"]";
        let adapted = configgen::adapt_mihomo_profile(raw, "rule").unwrap();
        validate_profile(&paths, &store, &adapted).unwrap();
        assert_eq!(validate_profile(&paths, &store, "mixed-port: invalid-port").unwrap_err().code, "PROXY_CONFIG_INVALID");
        assert!(!paths.mihomo_config().exists(), "validation must not publish a main config");
    }

    #[test]
    fn proxy_api_serializes_node_names_and_rejects_http_errors() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut received = Vec::new();
            let header_end = loop {
                let mut byte = [0]; stream.read_exact(&mut byte).unwrap(); received.push(byte[0]);
                if received.ends_with(b"\r\n\r\n") { break received.len(); }
            };
            let headers = String::from_utf8_lossy(&received).to_lowercase();
            let length: usize = headers.lines().find_map(|s| s.strip_prefix("content-length:")).unwrap().trim().parse().unwrap();
            received.resize(header_end + length, 0);
            stream.read_exact(&mut received[header_end..]).unwrap();
            stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            serde_json::from_slice::<serde_json::Value>(&received[header_end..]).unwrap()
        });
        let mut runtime = ProxyRuntime::new();
        runtime.base_url = format!("http://{address}");
        let name = "node\\path\n\"quoted\"";
        assert_eq!(runtime.select("group /中文", name).unwrap_err().code, "SELECT_FAILED");
        assert_eq!(server.join().unwrap()["name"], name);
    }
}
