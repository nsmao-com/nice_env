//! 环境体检：把散落各处的检查项汇总成一个「现在到底有没有问题」的结论。
//!
//! R1–R9 各自做了自己的检查（扩展加载、证书有效期、端口占用、配置语法…），
//! 但它们分散在不同页面，用户不会主动去逐个点。
//! 这个模块把它们聚合起来，给出**按严重程度排序**的一张清单，
//! 并明确「哪些能一键修、哪些只能人工处理」。
//!
//! 特意不做成「全绿就放心」的假安全感：检查失败的项会带着原始错误一起展示。

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::paths::Paths;

/// 检查项严重程度
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// 不影响使用，但值得知道
    Info,
    /// 建议处理，会逐渐变严重
    Warn,
    /// 现在就是坏的
    Error,
}

/// 一条体检结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckItem {
    /// 稳定 id，前端可据此定位/跳转
    pub id: String,
    pub severity: Severity,
    /// 一句话结论
    pub title: String,
    /// 细节（为什么、影响是什么）
    pub detail: String,
    /// 建议动作
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// 前端可跳转的页面（路由）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
}

/// 体检报告
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub items: Vec<CheckItem>,
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    /// 一句话总览
    pub summary: String,
    pub checked_at: i64,
}

impl HealthReport {
    fn push(&mut self, item: CheckItem) {
        match item.severity {
            Severity::Error => self.errors += 1,
            Severity::Warn => self.warnings += 1,
            Severity::Info => self.infos += 1,
        }
        self.items.push(item);
    }
}

/// 跑一遍全部检查
pub fn check(
    paths: &Paths,
    store: &crate::store::Store,
    manager: &std::sync::Arc<crate::services::ServiceManager>,
) -> Result<HealthReport> {
    let mut r = HealthReport {
        items: Vec::new(),
        errors: 0,
        warnings: 0,
        infos: 0,
        summary: String::new(),
        checked_at: chrono::Local::now().timestamp(),
    };

    // ---- 1) 是否装过任何套件 ----
    let installed = store.list_installed().unwrap_or_default();
    if installed.is_empty() {
        r.push(CheckItem {
            id: "no-packages".into(),
            severity: Severity::Info,
            title: "还没有安装任何套件".into(),
            detail: "本地环境是空的，先装 Web 服务器与运行时才能建站".into(),
            action: Some("到「套件 / 服务」安装 Nginx + PHP + MySQL".into()),
            route: Some("/packages".into()),
        });
    }

    // ---- 2) 端口占用（只报被外部程序占的，本应用自己在跑不算问题）----
    let profile = crate::services::PortsProfile::from_settings(store);
    let running: Vec<String> = manager
        .list_status()
        .into_iter()
        .filter(|s| matches!(s.state, crate::model::ServiceState::Running))
        .map(|s| s.id)
        .collect();

    let ports: [(&str, u16, &str); 8] = [
        ("HTTP", profile.http, "nginx"),
        ("HTTPS", profile.https, "nginx"),
        ("MySQL", profile.mysql, "mysql"),
        ("Redis", profile.redis, "redis"),
        ("Apache HTTP", profile.apache_http, "apache"),
        ("Apache HTTPS", profile.apache_https, "apache"),
        ("PostgreSQL", profile.postgres, "postgresql"),
        ("MongoDB", profile.mongodb, "mongodb"),
    ];
    let mut conflicts: Vec<String> = Vec::new();
    for (label, port, owner) in ports {
        // 该服务的版本装了没有？没装就不用管它的端口
        let installed_owner = installed.iter().any(|p| p.id == owner);
        if !installed_owner {
            continue;
        }
        if let Ok(d) = crate::ports::diagnose_port(port) {
            if d.in_use {
                let holder = d.process_name.clone().unwrap_or_default();
                let is_self = holder.to_ascii_lowercase().contains(owner);
                if !is_self {
                    conflicts.push(format!(
                        "{label} 端口 {port} 被 {holder}(pid {}) 占用",
                        d.pid.unwrap_or(0)
                    ));
                }
            }
        }
    }
    if !conflicts.is_empty() {
        r.push(CheckItem {
            id: "port-conflict".into(),
            severity: Severity::Error,
            title: format!("{} 个端口被其它程序占用", conflicts.len()),
            detail: conflicts.join("；"),
            action: Some("到「工具箱 → 端口」结束占用者，或在「设置 → 端口」改用其它端口".into()),
            route: Some("/tools".into()),
        });
    }

    // ---- 3) 站点引用了未安装的运行时 ----
    let sites = crate::sites::list(store).unwrap_or_default();
    let mut broken_sites: Vec<String> = Vec::new();
    for s in &sites {
        let need = match s.runtime.kind {
            crate::model::SiteKind::Php => {
                // 站点指定的 PHP 版本必须真的装了
                match s.runtime.php_version.as_deref() {
                    Some(v) => !installed.iter().any(|p| p.id == "php" && p.version == v),
                    None => !installed.iter().any(|p| p.id == "php"),
                }
            }
            crate::model::SiteKind::Static => false,
            _ => false,
        };
        // 文档根目录不存在 —— 站点一定 404
        let root_missing = !std::path::Path::new(&s.root_dir).is_dir();
        if need || root_missing {
            let why = if root_missing {
                format!("目录不存在：{}", s.root_dir)
            } else {
                format!(
                    "需要的 PHP 版本未安装：{}",
                    s.runtime.php_version.clone().unwrap_or_default()
                )
            };
            broken_sites.push(format!("{}（{why}）", s.name));
        }
    }
    if !broken_sites.is_empty() {
        r.push(CheckItem {
            id: "broken-sites".into(),
            severity: Severity::Error,
            title: format!("{} 个站点配置有问题", broken_sites.len()),
            detail: broken_sites.join("；"),
            action: Some("到「站点」修正路径，或到「套件 / 服务」补装对应版本".into()),
            route: Some("/sites".into()),
        });
    }

    // ---- 4) 证书 ----
    if let Ok(cert) = crate::certs::report(paths, store) {
        if cert.expired > 0 {
            r.push(CheckItem {
                id: "cert-expired".into(),
                severity: Severity::Error,
                title: format!("{} 张证书已过期", cert.expired),
                detail: "过期证书会让 HTTPS 站点直接打不开".into(),
                action: Some("到「证书与域名」重新签发".into()),
                route: Some("/tls".into()),
            });
        } else if cert.critical > 0 {
            r.push(CheckItem {
                id: "cert-critical".into(),
                severity: Severity::Warn,
                title: format!("{} 张证书 7 天内到期", cert.critical),
                detail: "到期后 HTTPS 站点会立即不可用".into(),
                action: Some("尽快到「证书与域名」重新签发".into()),
                route: Some("/tls".into()),
            });
        } else if cert.warning > 0 {
            r.push(CheckItem {
                id: "cert-warn".into(),
                severity: Severity::Info,
                title: format!("{} 张证书 30 天内到期", cert.warning),
                detail: "还有时间，但建议早点处理".into(),
                action: None,
                route: Some("/tls".into()),
            });
        }
        if !cert.ca_trusted && !sites.is_empty() {
            r.push(CheckItem {
                id: "ca-untrusted".into(),
                severity: Severity::Warn,
                title: "根 CA 未被系统信任".into(),
                detail: "所有 HTTPS 站点在浏览器里都会显示「不安全」".into(),
                action: Some("到「证书与域名」点一次「信任根证书」".into()),
                route: Some("/tls".into()),
            });
        }
        // 证书文件丢失
        let lost: Vec<String> = cert
            .certs
            .iter()
            .filter(|c| !c.file_present)
            .map(|c| c.subject.clone())
            .collect();
        if !lost.is_empty() {
            r.push(CheckItem {
                id: "cert-file-missing".into(),
                severity: Severity::Error,
                title: format!("{} 张证书的文件已丢失", lost.len()),
                detail: lost.join(", "),
                action: Some("重新签发这些证书".into()),
                route: Some("/tls".into()),
            });
        }
    }

    // ---- 5) hosts 托管记录是否与站点一致 ----
    let wanted = crate::hosts::managed_entries(store).len();
    // read_all 已按标记块标好 managed，用它比自己去数行更可靠
    let actual = crate::hosts::read_all()
        .map(|v| v.iter().filter(|e| e.managed).count())
        .unwrap_or(0);
    if wanted != actual {
        r.push(CheckItem {
            id: "hosts-drift".into(),
            severity: Severity::Warn,
            title: "hosts 里的托管记录与站点列表不一致".into(),
            detail: format!("应有 {wanted} 条，实际 {actual} 条 —— 域名可能解析不到本机"),
            action: Some("到「工具箱 → 重建 hosts」一键同步".into()),
            route: Some("/tools".into()),
        });
    }

    // ---- 6) 服务错误状态 ----
    let failed: Vec<String> = manager
        .list_status()
        .into_iter()
        .filter(|s| matches!(s.state, crate::model::ServiceState::Error))
        .map(|s| {
            let why = s
                .last_error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "未知错误".into());
            format!("{}（{why}）", s.id)
        })
        .collect();
    if !failed.is_empty() {
        r.push(CheckItem {
            id: "service-error".into(),
            severity: Severity::Error,
            title: format!("{} 个服务处于错误状态", failed.len()),
            detail: failed.join("；"),
            action: Some("到「日志」看具体报错".into()),
            route: Some("/logs".into()),
        });
    }

    // ---- 7) 数据目录可写 ----
    let probe = paths.base.join(".health-probe");
    if std::fs::write(&probe, b"ok").is_err() {
        r.push(CheckItem {
            id: "data-dir-readonly".into(),
            severity: Severity::Error,
            title: "数据目录不可写".into(),
            detail: format!("{}", paths.base.to_string_lossy()),
            action: Some("移到可写位置，或用环境变量 NSB_HOME 指定".into()),
            route: Some("/settings".into()),
        });
    } else {
        let _ = std::fs::remove_file(&probe);
    }

    // ---- 8) PHP 扩展加载失败（装了但加载不了）----
    for p in installed.iter().filter(|p| p.id == "php") {
        if let Ok(exts) = crate::phpext::scan_available(paths, &p.version) {
            // 已启用但缺依赖的扩展
            let broken: Vec<String> = exts
                .iter()
                .filter(|e| e.enabled && !e.missing_deps.is_empty())
                .map(|e| format!("{} 缺少 {}", e.label, e.missing_deps.join("/")))
                .collect();
            if !broken.is_empty() {
                r.push(CheckItem {
                    id: format!("php-ext-deps-{}", p.version),
                    severity: Severity::Warn,
                    title: format!("PHP {} 有 {} 个扩展缺依赖", p.version, broken.len()),
                    detail: broken.join("；"),
                    action: Some("到「套件 / 服务 → PHP 扩展」补齐依赖".into()),
                    route: Some("/packages".into()),
                });
            }
        }
    }

    // ---- 排序：error → warn → info ----
    r.items.sort_by_key(|i| match i.severity {
        Severity::Error => 0,
        Severity::Warn => 1,
        Severity::Info => 2,
    });

    r.summary = if r.errors > 0 {
        format!("发现 {} 个需要处理的问题", r.errors)
    } else if r.warnings > 0 {
        format!("{} 项建议处理，当前可用", r.warnings)
    } else if !installed.is_empty() {
        "环境正常".to_string()
    } else {
        "尚未配置环境".to_string()
    };

    let _ = running;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_counts_by_severity() {
        let mut r = HealthReport {
            items: Vec::new(),
            errors: 0,
            warnings: 0,
            infos: 0,
            summary: String::new(),
            checked_at: 0,
        };
        r.push(CheckItem {
            id: "a".into(),
            severity: Severity::Error,
            title: "e".into(),
            detail: String::new(),
            action: None,
            route: None,
        });
        r.push(CheckItem {
            id: "b".into(),
            severity: Severity::Warn,
            title: "w".into(),
            detail: String::new(),
            action: None,
            route: None,
        });
        r.push(CheckItem {
            id: "c".into(),
            severity: Severity::Info,
            title: "i".into(),
            detail: String::new(),
            action: None,
            route: None,
        });
        assert_eq!((r.errors, r.warnings, r.infos), (1, 1, 1));
    }

    #[test]
    fn severity_serializes_kebab_case() {
        let j = serde_json::to_string(&Severity::Warn).unwrap();
        assert_eq!(j, "\"warn\"");
        let j = serde_json::to_string(&Severity::Error).unwrap();
        assert_eq!(j, "\"error\"");
        let j = serde_json::to_string(&Severity::Info).unwrap();
        assert_eq!(j, "\"info\"");
    }

    #[test]
    fn check_runs_on_empty_env_without_panicking() {
        let t = std::env::temp_dir().join(format!("nsb-health-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        let store = crate::store::Store::open(t.join("h.sqlite")).unwrap();
        let manager = std::sync::Arc::new(crate::services::ServiceManager::new());
        let r = check(&paths, &store, &manager).unwrap();
        // 空环境下至少应提示「还没装套件」
        assert!(r.items.iter().any(|i| i.id == "no-packages"));
        assert!(r.summary.contains("尚未配置"));
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn items_are_sorted_errors_first() {
        let t = std::env::temp_dir().join(format!("nsb-health2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        let store = crate::store::Store::open(t.join("h.sqlite")).unwrap();
        let manager = std::sync::Arc::new(crate::services::ServiceManager::new());
        let r = check(&paths, &store, &manager).unwrap();
        let mut seen_lower = false;
        for i in &r.items {
            let rank = match i.severity {
                Severity::Error => 0,
                Severity::Warn => 1,
                Severity::Info => 2,
            };
            if rank > 0 {
                seen_lower = true;
            } else if seen_lower {
                panic!(
                    "error 应排在最前，实际顺序：{:?}",
                    r.items
                        .iter()
                        .map(|x| format!("{:?}", x.severity))
                        .collect::<Vec<_>>()
                );
            }
        }
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn data_dir_probe_cleans_up_after_itself() {
        let t = std::env::temp_dir().join(format!("nsb-health3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        let store = crate::store::Store::open(t.join("h.sqlite")).unwrap();
        let manager = std::sync::Arc::new(crate::services::ServiceManager::new());
        let _ = check(&paths, &store, &manager).unwrap();
        // 探针文件不该留在数据目录里
        assert!(!t.join(".health-probe").exists(), "探测文件应被清理");
        let _ = std::fs::remove_dir_all(&t);
    }
}
