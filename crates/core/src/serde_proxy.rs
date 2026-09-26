//! 代理（mihomo/Clash）前端数据模型与 mihomo REST API 响应解析。

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStatusInfo {
    pub running: bool,
    pub mixed_port: u16,
    pub controller_port: u16,
    pub mode: String,
    pub system_proxy_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProxyProfile {
    pub id: String,
    pub name: String,
    pub url: String,
    pub active: bool,
    pub added_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProxyNode {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<u32>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProxyGroupView {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub now: String,
    pub nodes: Vec<ProxyNode>,
}

/* ---------- mihomo /proxies 解析 ---------- */

#[derive(Deserialize, Clone, Debug)]
struct MihomoDelay {
    delay: Option<u32>,
}

#[derive(Deserialize, Clone, Debug)]
struct MihomoProxy {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    now: Option<String>,
    all: Option<Vec<String>>,
    alive: Option<bool>,
    history: Option<Vec<MihomoDelay>>,
}

#[derive(Deserialize, Clone, Debug)]
struct MihomoProxies {
    proxies: std::collections::HashMap<String, MihomoProxy>,
}

const GROUP_TYPES: &[&str] = &["Selector", "URLTest", "Fallback", "LoadBalance", "Relay"];

pub fn parse_groups(value: &serde_json::Value) -> crate::error::Result<Vec<ProxyGroupView>> {
    let parsed: MihomoProxies = serde_json::from_value(value.clone())
        .map_err(|_| crate::error::AppError::new("MIHOMO_API", "mihomo 节点响应格式无效，请重试"))?;
    let mut groups = Vec::new();
    for p in parsed.proxies.values() {
        if !GROUP_TYPES.contains(&p.kind.as_str()) {
            continue;
        }
        let members = p.all.clone().unwrap_or_default();
        let nodes = members
            .iter()
            .filter_map(|name| {
                let m = parsed.proxies.get(name)?;
                Some(ProxyNode {
                    name: m.name.clone(),
                    kind: m.kind.clone(),
                    alive: m.alive,
                    history: Some(
                        m.history
                            .as_ref()
                            .map(|h| h.iter().filter_map(|d| d.delay).collect())
                            .unwrap_or_default(),
                    ),
                })
            })
            .collect();
        groups.push(ProxyGroupView {
            name: p.name.clone(),
            kind: p.kind.clone(),
            now: p.now.clone().unwrap_or_default(),
            nodes,
        });
    }
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(groups)
}
