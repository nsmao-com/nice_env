//! Clash (mihomo) 模块：内核启停（走 services）、系统代理开关、订阅导入、节点查询/切换/测速。

use crate::configgen;
use crate::error::{AppError, Result};
use crate::paths::Paths;
use crate::store::Store;
use base64::Engine;
use serde::Deserialize;

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
                    .with_hint("请确认内核已启动；端口被占计时可在套件页查看冲突")
            })?;
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

    /// 切换 Selector 节点
    pub fn select(&self, group: &str, node: &str) -> Result<()> {
        let resp = self
            .client
            .put(format!("{}/proxies/{}", self.base_url, urlencode(group)))
            .body(format!("{{\"name\":\"{}\"}}", node.replace('"', "\\\"")))
            .header("Content-Type", "application/json")
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

async fn fetch_subscription(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .user_agent("clash-verge/1.6 NiceEnv/0.1")
        .build()
        .map_err(|e| AppError::internal("创建客户端", e.to_string()))?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::download(url, e.to_string()))?;
    if !resp.status().is_success() {
        return Err(AppError::download(url, format!("HTTP {}", resp.status())));
    }
    // 订阅可能是 base64 编码的纯节点列表；yaml 以proxies:/port: 开头才直接用
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::download(url, e.to_string()))?;
    Ok(decode_subscription(&bytes))
}

fn ensure_clash_config(raw: &str) -> Result<()> {
    if !raw.contains("proxies") && !raw.contains("proxy-groups") {
        return Err(
            AppError::new("NOT_A_CLASH_CONFIG", "订阅内容不是 Clash YAML 配置")
                .with_hint("请确认链接是 Clash 订阅（YAML），而不是 v2ray base64 节点链接"),
        );
    }
    Ok(())
}

pub async fn import_profile(name: &str, url: &str, paths: &Paths, store: &Store) -> Result<String> {
    let raw = fetch_subscription(url).await?;
    ensure_clash_config(&raw)?;

    let id = format!("profile-{}", crate::services::now_ms());
    let adapted = configgen::adapt_mihomo_profile(&raw);
    let file = paths
        .mihomo_dir()
        .join("profiles")
        .join(format!("{id}.yaml"));
    std::fs::create_dir_all(paths.mihomo_dir().join("profiles"))?;
    std::fs::write(&file, &adapted).map_err(|e| AppError::io("保存订阅配置", e))?;
    store.save_proxy_profile(&id, name, url, false)?;
    Ok(id)
}

/// 重新拉取订阅并覆盖原文件（id 不变）；该订阅处于激活态时同步重写主配置。
pub async fn update_profile(paths: &Paths, store: &Store, id: &str) -> Result<()> {
    let target = store
        .list_proxy_profiles()?
        .into_iter()
        .find(|(pid, ..)| pid == id)
        .ok_or_else(|| AppError::new("PROFILE_NOT_FOUND", format!("订阅 {id} 不存在")))?;
    let (_, _, url, active, _) = target;

    let raw = fetch_subscription(&url).await?;
    ensure_clash_config(&raw)?;
    let adapted = configgen::adapt_mihomo_profile(&raw);
    let file = paths
        .mihomo_dir()
        .join("profiles")
        .join(format!("{id}.yaml"));
    std::fs::create_dir_all(paths.mihomo_dir().join("profiles"))?;
    std::fs::write(&file, &adapted).map_err(|e| AppError::io("保存订阅配置", e))?;
    if active {
        activate_profile(paths, store, id)?;
    }
    Ok(())
}

fn decode_subscription(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes).trim().to_string();
    if text.contains("proxies:") || text.contains("proxy-providers:") {
        return text;
    }
    // 尝试 base64 → 但裸节点列表我们并不解析，仅用于校验提示
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(&cleaned) {
        return String::from_utf8_lossy(&decoded).to_string();
    }
    text
}

/// 激活订阅：内容写入主配置
pub fn activate_profile(paths: &Paths, store: &Store, id: &str) -> Result<()> {
    let file = paths
        .mihomo_dir()
        .join("profiles")
        .join(format!("{id}.yaml"));
    let content = std::fs::read_to_string(&file).map_err(|e| AppError::io("读取订阅配置", e))?;
    configgen::write_mihomo_config(paths, &content)?;
    store.set_active_proxy_profile(id)?;
    Ok(())
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
