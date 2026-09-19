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
}

#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub sites: usize,
    pub skipped_sites: usize,
    pub settings: usize,
    pub proxy_profiles: usize,
    pub stacks: usize,
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
    };
    let total = bundle.sites.len() + bundle.packages.len() + bundle.settings.len() + bundle.stacks.len();
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
        return Err(AppError::new("BAD_BACKUP", "不是 NiceServBay 的备份文件"));
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
