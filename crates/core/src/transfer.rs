//! 配置导入/导出：站点 + 已装套件清单 + 设置 + 代理订阅 + 服务栈 → 单个 JSON 备份文件。
//! 套件本体（几百 MB 运行时）不随文件走：导入后报告缺失清单，由用户在套件页补装。

use crate::error::{AppError, Result};
use crate::model::{Site, Stack};
use crate::paths::Paths;
use crate::services::ServiceManager;
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExportBundle {
    pub format: String,
    pub app_version: String,
    pub exported_at: i64,
    pub settings: Vec<(String, String)>,
    /// (id, version) —— 导入端据此提示补装
    pub packages: Vec<(String, String)>,
    pub sites: Vec<Site>,
    /// (name, url)
    pub proxy_profiles: Vec<(String, String)>,
    /// 用户定义的服务栈（内置预设不导出：导入端会自己生成）
    #[serde(default)]
    pub stacks: Vec<Stack>,
    /// 证书自动化（含 DNS 凭据与部署目标配置 —— 备份文件请妥善保管）
    #[serde(default)]
    pub cert_automations: Vec<crate::model::CertAutomation>,
    /// 网站证书监控
    #[serde(default)]
    pub cert_monitors: Vec<crate::model::CertMonitor>,
}

#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub sites: usize,
    pub skipped_sites: usize,
    pub settings: usize,
    pub proxy_profiles: usize,
    pub stacks: usize,
    pub cert_automations: usize,
    pub cert_monitors: usize,
    pub missing_packages: Vec<String>,
}

pub fn export_to(store: &Store, path: &std::path::Path) -> Result<usize> {
    let bundle = ExportBundle {
        format: "niceservbay/backup-v1".into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        exported_at: crate::services::now_ms(),
        settings: store.all_settings()?,
        packages: store
            .list_installed()?
            .into_iter()
            .map(|p| (p.id, p.version))
            .collect(),
        sites: store.list_sites()?,
        proxy_profiles: store
            .list_proxy_profiles()?
            .into_iter()
            .map(|(_, name, url, _, _)| (name, url))
            .collect(),
        stacks: store
            .list_stacks()?
            .into_iter()
            .filter(|s| !s.builtin)
            .collect(),
        cert_automations: store.list_cert_automations()?,
        cert_monitors: store.list_cert_monitors()?,
    };
    let total = bundle.sites.len()
        + bundle.packages.len()
        + bundle.settings.len()
        + bundle.stacks.len()
        + bundle.cert_automations.len()
        + bundle.cert_monitors.len();
    let json = serde_json::to_string_pretty(&bundle)
        .map_err(|e| AppError::internal("序列化备份", e.to_string()))?;
    std::fs::write(path, json).map_err(|e| AppError::io("写入备份文件", e))?;
    Ok(total)
}

pub fn import_from(
    path: &std::path::Path,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
) -> Result<ImportReport> {
    let raw = std::fs::read_to_string(path).map_err(|e| AppError::io("读取备份文件", e))?;
    let bundle: ExportBundle =
        serde_json::from_str(&raw).map_err(|e| AppError::new("BAD_BACKUP", format!("备份文件解析失败：{e}")))?;
    if !bundle.format.starts_with("niceservbay/") {
        return Err(AppError::new("BAD_BACKUP", "不是 NiceEnv 的备份文件"));
    }
    let mut report = ImportReport::default();

    // ---- 设置（逐项覆盖） ----
    for (k, v) in &bundle.settings {
        store.set_setting(k, v)?;
        report.settings += 1;
    }

    // ---- 代理订阅（按 url 去重） ----
    let existing: Vec<String> = store
        .list_proxy_profiles()?
        .into_iter()
        .map(|(_, _, url, _, _)| url)
        .collect();
    for (name, url) in &bundle.proxy_profiles {
        if existing.contains(url) {
            continue;
        }
        let id = format!("sub-{}", crate::services::now_ms());
        store.save_proxy_profile(&id, name, url, false)?;
        report.proxy_profiles += 1;
    }

    // ---- 服务栈（重开 id，避免覆盖本机同名栈） ----
    let now = crate::services::now_ms();
    for (i, s) in bundle.stacks.iter().enumerate() {
        let stack = Stack {
            id: format!("stack-{}-{i}", now),
            name: s.name.clone(),
            description: s.description.clone(),
            items: s.items.clone(),
            builtin: false,
            created_at: now + i as i64,
            updated_at: now + i as i64,
        };
        store.save_stack(&stack)?;
        report.stacks += 1;
    }

    // ---- 站点（域名冲突跳过；目录缺失则创建） ----
    let current_domains: Vec<String> =
        store.list_sites()?.into_iter().flat_map(|s| s.domains).collect();
    for site in &bundle.sites {
        if site.domains.iter().any(|d| current_domains.contains(d)) {
            report.skipped_sites += 1;
            continue;
        }
        let root = std::path::PathBuf::from(&site.root_dir);
        if !root.exists() {
            std::fs::create_dir_all(&root).map_err(|e| AppError::io("创建站点目录", e))?;
            std::fs::write(
                root.join("index.php"),
                "<?php\nheader('Content-Type: text/html; charset=utf-8');\necho '<h1>It works!</h1><p>Imported site.</p>';\n",
            )
            .ok();
        }
        store.save_site(site)?;
        crate::sites::write_site_conf(paths, store, site)?;
        report.sites += 1;
    }
    if report.sites > 0 {
        let _ = crate::ops::rebuild_and_reload(store, paths, manager);
        let _ = crate::hosts::apply(store, paths, None);
    }

    // ---- 证书自动化（按主域名去重；重开 id 防覆盖） ----
    let existing_primaries: Vec<String> = store
        .list_cert_automations()?
        .into_iter()
        .filter(|a| !a.domains.is_empty())
        .map(|a| a.domains[0].clone())
        .collect();
    for (i, a) in bundle.cert_automations.iter().enumerate() {
        if let Some(primary) = a.domains.first() {
            if existing_primaries.contains(primary) {
                continue;
            }
        }
        let mut imported = a.clone();
        imported.id = format!("auto-{}-{i}", now);
        imported.enabled = false; // 导入不等于开跑：核对凭据后再启用
        imported.state = "idle".into();
        imported.last_error = String::new();
        imported.fail_count = 0;
        imported.manual_records = Vec::new();
        store.save_cert_automation(&imported)?;
        report.cert_automations += 1;
    }

    // ---- 网站证书监控（按 host:port 去重） ----
    let existing_hosts: Vec<String> = store
        .list_cert_monitors()?
        .into_iter()
        .map(|m| format!("{}:{}", m.host, m.port))
        .collect();
    for m in &bundle.cert_monitors {
        let key = format!("{}:{}", m.host, m.port);
        if existing_hosts.contains(&key) {
            continue;
        }
        let mut imported = m.clone();
        imported.id = format!("mon-{}-{}", now, imported.id);
        store.save_cert_monitor(&imported)?;
        report.cert_monitors += 1;
    }

    // ---- 套件缺失报告 ----
    for (id, version) in &bundle.packages {
        if store.find_installed(id, Some(version)).is_none() {
            report
                .missing_packages
                .push(format!("{id}@{version}"));
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_backup_without_cert_fields_still_importable() {
        // v1 老备份没有 certAutomations/certMonitors —— serde(default) 兜底
        let old = r#"{"format":"niceservbay/backup-v1","appVersion":"0.1","exportedAt":1,
            "settings":[],"packages":[],"sites":[],"proxyProfiles":[],"stacks":[]}"#;
        let bundle: ExportBundle = serde_json::from_str(old).expect("老备份应可解析");
        assert!(bundle.cert_automations.is_empty());
        assert!(bundle.cert_monitors.is_empty());
    }

    #[test]
    fn cert_fields_round_trip() {
        let a = crate::model::CertAutomation {
            id: "auto-x".into(),
            name: "n".into(),
            domains: vec!["a.com".into()],
            email: "e@a.com".into(),
            ca: "letsencrypt".into(),
            dns: crate::model::DnsProvider {
                kind: "aliyun".into(),
                access_key: "AK".into(),
                secret: "SK".into(),
            },
            deploy_local: true,
            targets: vec![],
            enabled: false,
            state: "idle".into(),
            last_error: String::new(),
            cert_id: None,
            issued_at: None,
            expires_at: None,
            next_renew_at: 0,
            last_run_at: 0,
            key_alg: "ec256".into(),
            eab_kid: String::new(),
            eab_hmac_key: String::new(),
            dns_wait_sec: 0,
            cname_target: String::new(),
            renew_days_ahead: 30,
            retry_times: 3,
            retry_interval_min: 30,
            fail_count: 0,
            notify_kind: String::new(),
            notify_url: String::new(),
            notify_smtp: None,
            runs: vec![],
            manual_records: vec![],
            created_at: 1,
            updated_at: 1,
        };
        let bundle = ExportBundle {
            format: "niceservbay/backup-v1".into(),
            app_version: "t".into(),
            exported_at: 1,
            settings: vec![],
            packages: vec![],
            sites: vec![],
            proxy_profiles: vec![],
            stacks: vec![],
            cert_automations: vec![a],
            cert_monitors: vec![crate::model::CertMonitor {
                id: "m1".into(),
                host: "h.com".into(),
                port: 443,
                name: String::new(),
                state: "idle".into(),
                issuer: String::new(),
                expires_at: None,
                last_checked: None,
                last_error: String::new(),
                created_at: 1,
                updated_at: 1,
            }],
        };
        let json = serde_json::to_string(&bundle).unwrap();
        let back: ExportBundle = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cert_automations.len(), 1);
        assert_eq!(back.cert_automations[0].dns.access_key, "AK");
        assert_eq!(back.cert_monitors[0].host, "h.com");
    }
}
