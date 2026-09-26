//! 本地域名解析（CoreDNS 编排）：把 `*.{tld}` 通配解析到 127.0.0.1，
//! 其余查询转发公共 DNS。对标 ServBay 的 dnsmasq / FlyEnv 的内置 DNS——
//! hosts 文件不支持通配符，DNS 才是 `.test` 域名的根治方案。
//!
//! 用户接入方式：把系统/网卡的 DNS 指到 127.0.0.1（标准档端口 53）。

use crate::error::{AppError, Result};
use crate::paths::{write_with_backup, Paths};
use crate::store::Store;
use crate::services::ServiceManager;
use crate::model::ServiceState;
use std::sync::Arc;

static DNS_CHANGES: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
const BACKUP_PREFIX: &str = "dnsBackup.";

pub fn normalize_tld(value: &str) -> Result<String> {
    let value = value.trim().trim_start_matches('.').trim_end_matches('.').to_ascii_lowercase();
    if value.is_empty() || value.len() > 253 || value.parse::<std::net::IpAddr>().is_ok()
        || value.split('.').any(|part| part.is_empty() || part.len() > 63 || part.starts_with('-') || part.ends_with('-')
            || !part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
    {
        return Err(AppError::new("DNS_BAD_ZONE", "默认域名后缀无效")
            .with_hint("填写 test 或 dev.example 等域名后缀，不要包含空格、协议或配置内容。"));
    }
    Ok(value)
}

/// 生成 Corefile：`{tld}` 区用 template 插件通配应答，其余转发公共 DNS。
/// 区名不带端口——coredns 以 `-dns.port` 统一指定监听端口。
pub fn render_corefile(tld: &str, upstreams: &[&str]) -> Result<String> {
    let tld = normalize_tld(tld)?;
    for server in upstreams {
        let ip = server.parse::<std::net::IpAddr>().map_err(|_| AppError::new("DNS_BAD_UPSTREAM", "上游 DNS 必须填写 IP 地址"))?;
        if ip.is_loopback() || ip.is_unspecified() { return Err(AppError::new("DNS_BAD_UPSTREAM", "上游 DNS 不能指向本机解析器")); }
    }
    let upstream = if upstreams.is_empty() {
        "8.8.8.8 1.1.1.1"
    } else {
        &upstreams.join(" ")
    };
    Ok(format!(
        r#"# NiceEnv managed Corefile — *.{tld} 通配解析到 127.0.0.1，其余转发公共 DNS
{tld} {{
    bind 127.0.0.1
    template IN A {tld} {{
        answer "{{{{ .Name }}}} 60 IN A 127.0.0.1"
    }}
    template IN ANY {tld} {{
        rcode NOERROR
    }}
    errors
}}
. {{
    bind 127.0.0.1
    forward . {upstream}
    errors
}}
"#,
        tld = tld,
        upstream = upstream,
    ))
}

/// 写 Corefile（每次启动前重写：TLD/站点变化自动跟上）
pub fn write_corefile(paths: &Paths, target: &std::path::Path, tld: &str, upstreams: &[&str]) -> Result<()> {
    let conf = render_corefile(tld, upstreams)?;
    write_with_backup(target, &conf, &paths.backup())
        .map_err(|e| crate::error::AppError::io("写入 Corefile", e))
}

/// 发起真实 UDP 查询，核对请求 ID、问题与 A 记录，避免把其它 TCP 服务误认成 DNS。
pub fn probe(port: u16, tld: &str) -> Result<()> {
    let domain = format!("niceenv-check.{}", normalize_tld(tld)?);
    let id = rand::random::<u16>().to_be_bytes();
    let mut query = vec![id[0], id[1], 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for part in domain.split('.') { query.push(part.len() as u8); query.extend_from_slice(part.as_bytes()); }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    let socket = std::net::UdpSocket::bind(("127.0.0.1", 0))?;
    socket.set_read_timeout(Some(std::time::Duration::from_millis(350)))?;
    socket.connect(("127.0.0.1", port))?;
    socket.send(&query)?;
    let mut reply = [0u8; 4096];
    let count = socket.recv(&mut reply).map_err(|e| AppError::io("本地 DNS 未能正常应答", e))?;
    if !valid_reply(&query, &reply[..count]) {
        return Err(AppError::new("DNS_BAD_RESPONSE", "本地 DNS 未返回预期的 127.0.0.1 映射"));
    }
    Ok(())
}

fn valid_reply(query: &[u8], reply: &[u8]) -> bool {
    if reply.len() < query.len() || reply[..2] != query[..2] || reply[2] & 0x82 != 0x80 || reply[3] & 15 != 0
        || reply[4..6] != [0, 1] || reply[12..query.len()] != query[12..] { return false; }
    let mut offset = query.len();
    for _ in 0..u16::from_be_bytes([reply[6], reply[7]]) {
        loop {
            let Some(&size) = reply.get(offset) else { return false; };
            if size & 0xc0 == 0xc0 { offset += 2; break; }
            if size > 63 { return false; }
            offset += size as usize + 1;
            if size == 0 { break; }
        }
        if offset + 10 > reply.len() { return false; }
        let length = u16::from_be_bytes([reply[offset + 8], reply[offset + 9]]) as usize;
        if offset + 10 + length > reply.len() { return false; }
        if reply[offset..offset + 4] == [0, 1, 0, 1] && length == 4 && reply[offset + 10..offset + 14] == [127, 0, 0, 1] { return true; }
        offset += 10 + length;
    }
    false
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterfaceStatus {
    pub current: platform::DnsConfiguration,
    pub backup: Option<platform::DnsConfiguration>,
    pub local: bool,
}

fn backup(store: &Store, name: &str) -> Result<Option<platform::DnsConfiguration>> {
    store.get_setting_checked(&format!("{BACKUP_PREFIX}{name}"))?
        .map(|raw| serde_json::from_str(&raw).map_err(|e| AppError::internal("读取 DNS 恢复记录", e.to_string())))
        .transpose().map(Option::flatten)
}

pub fn interface_status(store: &Store, name: &str) -> Result<InterfaceStatus> {
    let _change = DNS_CHANGES.lock();
    let current = platform::dns_configuration(name)?;
    Ok(InterfaceStatus { local: current.is_local(), current, backup: backup(store, name)? })
}

pub fn takeover(store: &Store, manager: &Arc<ServiceManager>, name: &str) -> Result<()> {
    let _lifecycle = manager.lifecycle.lock();
    let _change = DNS_CHANGES.lock();
    let status = manager.snapshot("coredns").ok_or_else(|| AppError::not_installed("CoreDNS"))?;
    if status.state != ServiceState::Running || manager.started_port_or("coredns", 0) != 53 {
        return Err(AppError::new("DNS_NOT_READY", "接管前请先让 CoreDNS 在 53 端口正常运行"));
    }
    probe(53, &store.get_setting_checked("defaultTld")?.unwrap_or_else(|| "test".into()))?;
    let current = platform::dns_configuration(name)?;
    takeover_with(store, name, current, |local| platform::set_dns_configuration_elevated(name, local).map_err(AppError::from))
}

fn takeover_with(store: &Store, name: &str, current: platform::DnsConfiguration,
    apply: impl FnOnce(&platform::DnsConfiguration) -> Result<()>) -> Result<()> {
    if let Some(original) = backup(store, name)? {
        if current.interface_id != original.interface_id { return Err(AppError::new("DNS_INTERFACE_CHANGED", "接口已变化，请先核对旧 DNS 恢复记录")); }
        if current.is_local() { return Ok(()); }
        return Err(AppError::new("DNS_BACKUP_EXISTS", "已有 DNS 恢复记录，请先恢复并核对后再接管"));
    }
    if current.is_local() { return Err(AppError::new("DNS_NO_ORIGINAL", "当前 DNS 已指向本机，但没有原配置记录")
        .with_hint("请先使用恢复自动获取，或在系统设置中配置正确 DNS，再重新接管。")); }
    store.set_setting_json(&format!("{BACKUP_PREFIX}{name}"), &Some(&current))?;
    let local = platform::DnsConfiguration { interface_id: current.interface_id, automatic: false, servers: vec!["127.0.0.1".into()] };
    apply(&local).map_err(|e| AppError::new("DNS_TAKEOVER_FAILED", e.to_string())
        .with_hint("原 DNS 配置已保存；请刷新核对当前状态，必要时点击恢复原配置。"))
}

pub fn restore(store: &Store, name: &str, automatic: bool) -> Result<()> {
    let _change = DNS_CHANGES.lock();
    restore_locked(store, name, automatic, true)
}

fn restore_locked(store: &Store, name: &str, automatic: bool, explicit: bool) -> Result<()> {
    let current = platform::dns_configuration(name)?;
    restore_with(store, name, automatic, explicit, current, |config| platform::set_dns_configuration_elevated(name, config).map_err(AppError::from))
}

fn restore_with(store: &Store, name: &str, automatic: bool, explicit: bool, current: platform::DnsConfiguration,
    apply: impl FnOnce(&platform::DnsConfiguration) -> Result<()>) -> Result<()> {
    let original = match backup(store, name)? {
        Some(saved) => saved,
        None if automatic => platform::DnsConfiguration { interface_id: current.interface_id.clone(), automatic: true, servers: Vec::new() },
        None => return Err(AppError::new("DNS_NO_BACKUP", "没有接管前的 DNS 配置记录")),
    };
    if original.interface_id != current.interface_id { return Err(AppError::new("DNS_INTERFACE_CHANGED", "网络接口已变化，不能写入旧接口配置")); }
    if !current.matches(&original) {
        if !current.is_local() && !explicit { return Err(AppError::new("DNS_CHANGED", "DNS 已被其它程序修改，原配置备份已保留")
            .with_hint("请核对当前与原 DNS 地址后，在系统网络设置中恢复；应用不会覆盖新的配置。")); }
        apply(&original)?;
    }
    store.set_setting_json(&format!("{BACKUP_PREFIX}{name}"), &Option::<platform::DnsConfiguration>::None)
        .map_err(|e| AppError::new("DNS_RESTORE_RECORD_FAILED", "DNS 已恢复，但恢复记录未能清理").with_detail(e.to_string()))
}

/// 停止本地解析器前先还原本机接管过的接口；失败则保留解析服务。
pub fn restore_before_stop(store: &Store) -> Result<()> {
    let _change = DNS_CHANGES.lock();
    for (key, raw) in store.all_settings()? {
        if let Some(name) = key.strip_prefix(BACKUP_PREFIX) {
            let saved: Option<platform::DnsConfiguration> = serde_json::from_str(&raw)
                .map_err(|e| AppError::internal("读取 DNS 恢复记录", e.to_string()))?;
            if saved.is_some() { restore_locked(store, name, false, false)?; }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corefile_answers_wildcard_tld_and_forwards_rest() {
        let c = render_corefile("test", &[]).unwrap();
        assert!(c.contains("test {"), "应有 test 区：{c}");
        assert!(c.contains("template IN A test"));
        assert!(c.contains("template IN ANY test {\n        rcode NOERROR"));
        assert_eq!(c.matches("bind 127.0.0.1").count(), 2);
        assert!(c.contains("127.0.0.1"));
        assert!(
            c.contains("forward . 8.8.8.8 1.1.1.1"),
            "其余应转发公共 DNS"
        );
        // {{ .Name }} 是 CoreDNS 模板占位符，不能被 Rust 格式化吃掉
        assert!(c.contains("{{ .Name }}"), "模板占位符必须保留：{c}");
    }

    #[test]
    fn custom_upstreams_and_tld_dot_stripped() {
        let c = render_corefile(".dev", &["9.9.9.9"]).unwrap();
        assert!(c.contains("dev {"));
        assert!(c.contains("forward . 9.9.9.9"));
        assert!(!c.contains(".dev {"), "前导点应被清理");
    }

    #[test]
    fn write_creates_file() {
        let base = tempfile::tempdir().unwrap();
        let paths = Paths::new(base.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let p = paths.etc_dir("coredns", "1.14.7").join("Corefile");
        write_corefile(&paths, &p, "test", &[]).unwrap();
        assert!(p.is_file());
        let raw = std::fs::read_to_string(p).unwrap();
        assert!(raw.contains("template IN ANY test"));
    }

    #[test]
    fn invalid_zone_and_upstream_cannot_inject_configuration() {
        for zone in ["", "test {\n forward . 127.0.0.1\n}", "bad..test", "https://test", "127.0.0.1"] {
            assert!(render_corefile(zone, &[]).is_err());
        }
        for upstream in ["127.0.0.1", "::1", "0.0.0.0", "8.8.8.8\nlog"] {
            assert!(render_corefile("test", &[upstream]).is_err());
        }
    }

    #[test]
    fn dns_response_requires_matching_question_and_local_a_answer() {
        let query = vec![1, 2, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 1, b'a', 0, 0, 1, 0, 1];
        let mut reply = query.clone();
        reply[2] = 0x81;
        reply[7] = 1;
        reply.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 127, 0, 0, 1]);
        assert!(valid_reply(&query, &reply));
        reply[0] = 3;
        assert!(!valid_reply(&query, &reply));
        reply[0] = 1;
        *reply.last_mut().unwrap() = 2;
        assert!(!valid_reply(&query, &reply));
        for end in 0..reply.len() { assert!(!valid_reply(&query, &reply[..end])); }
    }

    #[test]
    fn takeover_failure_keeps_original_dns_for_retry_after_reopening_store() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dns.sqlite");
        let store = Store::open(path.clone()).unwrap();
        let original = platform::DnsConfiguration { interface_id: "network-1".into(), automatic: false, servers: vec!["9.9.9.9".into(), "1.1.1.1".into()] };
        let error = takeover_with(&store, "Wi-Fi 2", original.clone(), |_| Err(AppError::new("DENIED", "cancelled"))).unwrap_err();
        assert_eq!(error.code, "DNS_TAKEOVER_FAILED");
        drop(store);
        let store = Store::open(path).unwrap();
        assert_eq!(backup(&store, "Wi-Fi 2").unwrap(), Some(original.clone()));
        let local = platform::DnsConfiguration { interface_id: "network-1".into(), automatic: false, servers: vec!["127.0.0.1".into()] };
        assert!(restore_with(&store, "Wi-Fi 2", false, true, local.clone(), |_| Err(AppError::new("DENIED", "cancelled"))).is_err());
        assert!(backup(&store, "Wi-Fi 2").unwrap().is_some());
        restore_with(&store, "Wi-Fi 2", false, true, local, |restored| { assert_eq!(restored, &original); Ok(()) }).unwrap();
        assert!(backup(&store, "Wi-Fi 2").unwrap().is_none());
    }

    #[test]
    fn automatic_stop_cannot_overwrite_external_changes_or_a_replaced_interface() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("dns.sqlite")).unwrap();
        let original = platform::DnsConfiguration { interface_id: "network-1".into(), automatic: true, servers: vec!["192.168.1.1".into()] };
        takeover_with(&store, "Wi-Fi", original, |_| Ok(())).unwrap();
        let mut current = platform::DnsConfiguration { interface_id: "network-1".into(), automatic: false, servers: vec!["8.8.8.8".into()] };
        let error = restore_with(&store, "Wi-Fi", false, false, current.clone(), |_| panic!("must not overwrite external DNS")).unwrap_err();
        assert_eq!(error.code, "DNS_CHANGED");
        current.interface_id = "replacement".into();
        let error = restore_with(&store, "Wi-Fi", false, true, current, |_| panic!("must not overwrite another interface")).unwrap_err();
        assert_eq!(error.code, "DNS_INTERFACE_CHANGED");
        assert!(backup(&store, "Wi-Fi").unwrap().is_some());
    }
}
