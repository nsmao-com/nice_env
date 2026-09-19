//! hosts 读写：托管标记块 + 人话错误。真实写盘在 platform crate。

use crate::error::Result;
use crate::model::HostsEntry;
use crate::paths::Paths;
use crate::store::Store;

/// 用户在工具箱里手动添加的条目设置键（JSON 数组，持久化在 SQLite）
const EXTRA_HOSTS_KEY: &str = "extraHosts";

/// 读取用户手动添加的 hosts 条目
pub fn extra_entries(store: &Store) -> Vec<(String, String)> {
    store.get_setting_or::<Vec<(String, String)>>(EXTRA_HOSTS_KEY)
}

/// 覆盖写入用户手动条目（来自工具箱的「添加」/「删除」）
pub fn set_extra_entries(store: &Store, entries: &[(String, String)]) -> Result<()> {
    store.set_setting_json(EXTRA_HOSTS_KEY, &entries.to_vec())
}

/// 当前应写入 hosts 的全部托管条目：站点域名（恒为 127.0.0.1）+ 用户手动条目
pub fn managed_entries(store: &Store) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Ok(sites) = store.list_sites() {
        for site in sites {
            for d in &site.domains {
                out.push(("127.0.0.1".to_string(), d.clone()));
            }
        }
    }
    // 与站点同域时以站点为准（127.0.0.1），避免同一域名两条记录
    for (ip, domain) in extra_entries(store) {
        if !out.iter().any(|(_, d)| d == &domain) {
            out.push((ip, domain));
        }
    }
    out
}

/// 读取整个 hosts 解析为条目列表（含托管标记）
pub fn read_all() -> Result<Vec<HostsEntry>> {
    let content = platform::read_hosts_file().map_err(crate::error::AppError::from)?;
    let mut in_managed = false;
    let mut out = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if t == platform::HOSTS_BEGIN {
            in_managed = true;
            continue;
        }
        if t == platform::HOSTS_END {
            in_managed = false;
            continue;
        }
        if t.starts_with('#') || t.is_empty() {
            continue;
        }
        let mut parts = t.split_whitespace();
        if let (Some(ip), Some(domain)) = (parts.next(), parts.next()) {
            out.push(HostsEntry {
                ip: ip.to_string(),
                domain: domain.to_string(),
                managed: in_managed,
            });
        }
    }
    Ok(out)
}

/// 应用托管条目（失败给人话）。
/// 传入的 `entries` 里凡不属于站点域名的，作为「用户手动条目」持久化，
/// 这样工具箱里填的自定义 IP 才真正生效、且重启后仍在。
pub fn apply(store: &Store, paths: &Paths, entries: Option<Vec<HostsEntry>>) -> Result<()> {
    let _ = paths;
    if let Some(list) = entries {
        let site_domains: Vec<String> = store
            .list_sites()
            .map(|sites| sites.into_iter().flat_map(|s| s.domains).collect())
            .unwrap_or_default();
        let extras: Vec<(String, String)> = list
            .into_iter()
            .filter(|e| !e.domain.trim().is_empty())
            .filter(|e| !site_domains.contains(&e.domain))
            .map(|e| {
                let ip = if e.ip.trim().is_empty() { "127.0.0.1".to_string() } else { e.ip.trim().to_string() };
                (ip, e.domain.trim().to_string())
            })
            .collect();
        set_extra_entries(store, &extras)?;
    }

    // 冒烟/无头测试跳过（避免无管理员权限时改系统 hosts）
    if std::env::var("NSB_SKIP_HOSTS").map(|v| v == "1").unwrap_or(false) {
        return Ok(());
    }
    let merged = managed_entries(store);
    platform::apply_managed_hosts(&merged).map_err(crate::error::AppError::from)
}

/// 按当前站点 + 用户自定义条目重建托管块（不改动手动条目集合）
pub fn rebuild(store: &Store, paths: &Paths) -> Result<()> {
    apply(store, paths, None)
}
