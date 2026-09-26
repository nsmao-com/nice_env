//! hosts 读写：托管标记块 + 人话错误。真实写盘在 platform crate。

use crate::error::{AppError, Result};
use crate::model::HostsEntry;
use crate::paths::Paths;
use crate::store::Store;

/// 用户在工具箱里手动添加的条目设置键（JSON 数组，持久化在 SQLite）
const EXTRA_HOSTS_KEY: &str = "extraHosts";
pub(crate) static HOSTS_CHANGES: parking_lot::ReentrantMutex<()> = parking_lot::ReentrantMutex::new(());

/// 读取用户手动添加的 hosts 条目
pub fn extra_entries(store: &Store) -> Result<Vec<(String, String)>> {
    let _change = HOSTS_CHANGES.lock();
    match store.get_setting_checked(EXTRA_HOSTS_KEY)? {
        Some(raw) => serde_json::from_str(&raw).map_err(|e| AppError::new("HOSTS_SETTINGS_INVALID", "已保存的 hosts 条目无法读取，未覆盖现有记录")
            .with_hint("请先检查配置备份并恢复 hosts 设置。")
            .with_detail(e.to_string())),
        None => Ok(Vec::new()),
    }
}

/// 覆盖写入用户手动条目（来自工具箱的「添加」/「删除」）
pub fn set_extra_entries(store: &Store, entries: &[(String, String)]) -> Result<()> {
    let _change = HOSTS_CHANGES.lock();
    store.set_setting_json(EXTRA_HOSTS_KEY, &normalize_entries(entries)?)
}

fn normalize_entry(ip: &str, domain: &str) -> Result<(String, String)> {
    let ip = ip.trim().parse::<std::net::IpAddr>()
        .map_err(|_| AppError::new("HOSTS_BAD_IP", format!("IP 地址格式不正确：{ip}"))
            .with_hint("请填写完整 IPv4 或 IPv6 地址，例如 127.0.0.1 或 ::1。"))?;
    let domain = domain.trim().to_ascii_lowercase();
    let domain = domain.strip_suffix('.').unwrap_or(&domain);
    if domain.is_empty() || domain.len() > 253 || domain.parse::<std::net::IpAddr>().is_ok()
        || domain.split('.').any(|label| label.is_empty() || label.len() > 63
            || label.starts_with('-') || label.ends_with('-')
            || !label.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
    {
        return Err(AppError::new("HOSTS_BAD_DOMAIN", format!("主机名格式不正确：{domain}"))
            .with_hint("填写域名或主机名，不要包含协议、端口、路径、空格或通配符。"));
    }
    Ok((ip.to_string(), domain.to_string()))
}

fn normalize_entries(entries: &[(String, String)]) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (ip, domain) in entries {
        let entry = normalize_entry(ip, domain)?;
        if seen.insert(entry.clone()) { out.push(entry); }
    }
    Ok(out)
}

fn site_entries(store: &Store) -> Result<Vec<(String, String)>> {
    let entries = store.list_sites()?.into_iter().flat_map(|site| site.domains)
        .filter(|d| !d.starts_with("*.") && d.parse::<std::net::IpAddr>().is_err())
        .map(|d| ("127.0.0.1".into(), d)).collect::<Vec<_>>();
    normalize_entries(&entries)
}

fn merge_entries(mut sites: Vec<(String, String)>, extras: Vec<(String, String)>) -> Vec<(String, String)> {
    let domains: std::collections::HashSet<_> = sites.iter().map(|(_, d)| d.clone()).collect();
    sites.extend(extras.into_iter().filter(|(_, d)| !domains.contains(d)));
    sites
}

/// 当前应写入 hosts 的全部托管条目：站点域名（恒为 127.0.0.1）+ 用户手动条目
pub fn managed_entries(store: &Store) -> Result<Vec<(String, String)>> {
    let _change = HOSTS_CHANGES.lock();
    Ok(merge_entries(site_entries(store)?, normalize_entries(&extra_entries(store)?)?))
}

/// 读取整个 hosts 解析为条目列表（含托管标记）
pub fn read_all() -> Result<Vec<HostsEntry>> {
    let _change = HOSTS_CHANGES.lock();
    let content = platform::read_hosts_file().map_err(crate::error::AppError::from)?;
    parse_content(&content)
}

fn parse_content(content: &str) -> Result<Vec<HostsEntry>> {
    let lines: Vec<&str> = content.lines().collect();
    // 与 platform::merge_hosts_content 同一口径：结束标记缺失时，孤立的开始标记不算托管块
    let starts = platform::hosts_block_starts(&lines);
    let mut in_managed = false;
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t == platform::HOSTS_BEGIN || t == platform::HOSTS_BEGIN_LEGACY {
            in_managed = starts[i];
            continue;
        }
        if t == platform::HOSTS_END || t == platform::HOSTS_END_LEGACY {
            in_managed = false;
            continue;
        }
        if t.starts_with('#') || t.is_empty() {
            continue;
        }
        let mut parts = t.split('#').next().unwrap_or_default().split_whitespace();
        let ip = parts.next().unwrap_or_default();
        let domains: Vec<_> = parts.collect();
        if in_managed && domains.is_empty() {
            return Err(AppError::new("HOSTS_INVALID_LINE", format!("hosts 第 {} 行缺少主机名", i + 1)));
        }
        for domain in domains {
            match normalize_entry(ip, domain) {
                Ok((ip, domain)) => out.push(HostsEntry { ip, domain, managed: in_managed }),
                Err(error) if in_managed => return Err(error.with_hint(format!("请检查 hosts 托管段第 {} 行；现有内容未改动。", i + 1))),
                Err(_) => {} // 块外原文不参与写入，无法解析的系统行保留在文件中。
            }
        }
    }
    Ok(out)
}

/// 应用托管条目（失败给人话）。
/// 传入的托管 `entries` 里凡不属于站点域名的，作为「用户手动条目」持久化，
/// 这样工具箱里填的自定义 IP 才真正生效、且重启后仍在。
pub fn apply(store: &Store, paths: &Paths, entries: Option<Vec<HostsEntry>>) -> Result<()> {
    apply_checked(store, paths, entries, None)
}

/// 编辑器携带读取时的快照，避免覆盖其它窗口或程序刚写入的记录。
pub fn apply_checked(store: &Store, paths: &Paths, entries: Option<Vec<HostsEntry>>, expected: Option<&[HostsEntry]>) -> Result<()> {
    let _change = HOSTS_CHANGES.lock();
    let _ = paths;
    if let Some(expected) = expected { ensure_unchanged(expected, &read_all()?)?; }
    apply_with_writer(store, entries, |merged| {
        // 冒烟/无头测试跳过（避免无管理员权限时改系统 hosts）
        if std::env::var("NSB_SKIP_HOSTS").is_ok_and(|v| v == "1") { return Ok(()); }
        platform::apply_managed_hosts(merged).map_err(AppError::from)
    })
}

fn ensure_unchanged(expected: &[HostsEntry], current: &[HostsEntry]) -> Result<()> {
    let sorted = |entries: &[HostsEntry]| {
        let mut keys = entries.iter().map(|e| (e.ip.clone(), e.domain.clone(), e.managed)).collect::<Vec<_>>();
        keys.sort();
        keys
    };
    if sorted(expected) != sorted(current) {
        return Err(AppError::new("HOSTS_CHANGED", "hosts 内容已变化，本次修改未写入")
            .with_hint("请刷新并核对最新条目，再重新操作。"));
    }
    Ok(())
}

fn apply_with_writer(store: &Store, entries: Option<Vec<HostsEntry>>, write: impl FnOnce(&[(String, String)]) -> Result<()>) -> Result<()> {
    let _change = HOSTS_CHANGES.lock();
    let previous = extra_entries(store)?;
    let sites = site_entries(store)?;
    let updating = entries.is_some();
    let extras = if let Some(list) = entries {
        let requested = normalize_entries(&list.into_iter().filter(|e| e.managed)
            .map(|e| (e.ip, e.domain)).collect::<Vec<_>>())?;
        let mut extras = Vec::new();
        for (ip, domain) in requested {
            if let Some((site_ip, _)) = sites.iter().find(|(_, d)| d == &domain) {
                if &ip != site_ip {
                    return Err(AppError::new("HOSTS_SITE_MANAGED", format!("{domain} 由站点自动维护，不能在 hosts 编辑器中更改地址"))
                        .with_hint("请到站点设置管理该域名。"));
                }
            } else { extras.push((ip, domain)); }
        }
        extras
    } else { normalize_entries(&previous)? };
    if updating { set_extra_entries(store, &extras)?; }
    if let Err(error) = write(&merge_entries(sites, extras)) {
        if updating {
            if let Err(rollback) = store.set_setting_json(EXTRA_HOSTS_KEY, &previous) {
                return Err(AppError::new("HOSTS_ROLLBACK_FAILED", "hosts 写入失败，应用内记录也未能恢复")
                    .with_hint("请检查目录权限和磁盘空间，修复后重新读取 hosts 并核对记录。")
                    .with_detail(format!("写入：{error}；恢复：{rollback}")));
            }
        }
        return Err(error);
    }
    Ok(())
}

/// 按当前站点 + 用户自定义条目重建托管块（不改动手动条目集合）
pub fn rebuild(store: &Store, paths: &Paths) -> Result<()> {
    apply(store, paths, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ip: &str, domain: &str) -> HostsEntry {
        HostsEntry { ip: ip.into(), domain: domain.into(), managed: true }
    }

    #[test]
    fn hosts_parser_reads_aliases_ipv6_comments_and_preserves_block_boundaries() {
        let text = format!("127.0.0.1 localhost local-alias # system\n{}\n::1 FIRST.test second.test # manual\n{}\n{}\n10.0.0.3 external.test\n", platform::HOSTS_BEGIN, platform::HOSTS_END, platform::HOSTS_BEGIN);
        let entries = parse_content(&text).unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[2].domain, "first.test");
        assert_eq!(entries[3].domain, "second.test");
        assert_eq!(entries[3].ip, "::1");
        assert!(entries[2].managed && entries[3].managed);
        assert!(!entries[0].managed && !entries[4].managed);
        assert!(parse_content(&format!("{}\n999.1.1.1 broken.test\n{}", platform::HOSTS_BEGIN, platform::HOSTS_END)).is_err());
    }

    #[test]
    fn hosts_validation_rejects_entire_invalid_update_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("store.sqlite")).unwrap();
        set_extra_entries(&store, &[("10.0.0.1".into(), "keep.test".into())]).unwrap();
        let previous = extra_entries(&store).unwrap();
        for invalid in [entry("999.0.0.1", "bad.test"), entry("1.2.3.4", "ok.test\n10.0.0.1 injected.test"), entry("::1", "*.test"), entry("::1", "https://bad.test"), entry("::1", "bad..test")] {
            let error = apply_with_writer(&store, Some(vec![entry("::1", "valid.test"), invalid]), |_| panic!("invalid input must not reach disk"));
            assert!(error.is_err());
            assert_eq!(extra_entries(&store).unwrap(), previous);
        }
    }

    #[test]
    fn hosts_failed_write_restores_saved_entries_and_bad_settings_block_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("store.sqlite")).unwrap();
        let previous = vec![("10.0.0.1".into(), "keep.test".into())];
        set_extra_entries(&store, &previous).unwrap();
        let error = apply_with_writer(&store, Some(vec![entry("::1", "new.test")]), |_| Err(AppError::new("DENIED", "permission denied"))).unwrap_err();
        assert_eq!(error.code, "DENIED");
        assert_eq!(extra_entries(&store).unwrap(), previous);
        store.set_setting(EXTRA_HOSTS_KEY, "broken json").unwrap();
        let error = apply_with_writer(&store, None, |_| panic!("unreadable settings must not clear hosts")).unwrap_err();
        assert_eq!(error.code, "HOSTS_SETTINGS_INVALID");
        assert_eq!(store.get_setting_checked(EXTRA_HOSTS_KEY).unwrap().as_deref(), Some("broken json"));
    }

    #[test]
    fn hosts_keeps_dual_stack_and_ignores_system_entries() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("store.sqlite")).unwrap();
        let mut system = entry("127.0.0.1", "localhost");
        system.managed = false;
        apply_with_writer(&store, Some(vec![system, entry("127.0.0.1", "Dual.TEST."), entry("::1", "dual.test"), entry("0:0:0:0:0:0:0:1", "dual.test")]), |written| {
            assert_eq!(written.len(), 2);
            assert!(written.contains(&("::1".into(), "dual.test".into())));
            Ok(())
        }).unwrap();
        assert_eq!(extra_entries(&store).unwrap().len(), 2);
        apply_with_writer(&store, None, |written| { assert_eq!(written.len(), 2); Ok(()) }).unwrap();
    }

    #[test]
    fn hosts_stale_snapshot_rejects_changed_mapping_with_the_same_count() {
        let previous = vec![entry("127.0.0.1", "local.test"), entry("::1", "local.test")];
        assert!(ensure_unchanged(&previous, &[previous[1].clone(), previous[0].clone()]).is_ok());
        let current = vec![entry("10.0.0.1", "local.test"), entry("::1", "local.test")];
        assert_eq!(ensure_unchanged(&previous, &current).unwrap_err().code, "HOSTS_CHANGED");
    }
}
