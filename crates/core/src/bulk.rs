//! 批量服务操作：对任选的一组服务做启停，而不是只能整套栈。
//!
//! 「服务栈」解决的是「我有一套固定组合」；但日常更常见的是**临时**一批：
//! 调试时想只停掉数据库相关的三个服务、或者把上次崩掉的几个一起拉起来。
//! 为此专门存一个栈太重了。
//!
//! 顺序沿用栈的规则，因为服务之间确实有依赖：
//! - **启动**：按依赖拓扑排序（数据库/缓存先起，Web 服务器最后）——
//!   nginx 起来时上游还没就绪会直接 502。
//! - **停止**：反过来（先断流量再关库），避免请求打到已停的数据库。
//!
//! 逐项回报，单项失败不阻断其余项 —— 批量操作最忌讳「因为一个失败就全停」，
//! 那样用户还得自己猜停到哪一步了。

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::model::{AppErrorInfo, ServiceState};
use crate::paths::Paths;
use crate::services::ServiceManager;
use std::sync::Arc;

/// 批量操作结果（与 StackStartReport 同形，便于前端复用同一套展示）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkReport {
    pub action: String,
    /// 成功执行（启动成功 / 停止成功）
    pub succeeded: Vec<String>,
    /// 本来就在目标状态，跳过
    pub already: Vec<String>,
    /// 失败项
    pub failed: Vec<BulkFailure>,
    /// 实际执行的顺序（用户要求按依赖排序时会与传入顺序不同）
    pub order: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkFailure {
    pub service_id: String,
    pub error: AppErrorInfo,
}

/// 服务的依赖层级：数字越小越先启动。
///
/// 这不是精确的依赖图（那是 ops 里每个服务的启动逻辑各自处理的），
/// 而是「按经验排个大致对的顺序」，用于让批量启动不至于必然失败：
/// 0 = 数据与缓存（MySQL / Redis / Mongo / PG —— 上游）
/// 1 = 运行容器（PHP / Node / Java … —— 中间层）
/// 2 = Web 服务器与代理（nginx / apache / caddy / mihomo —— 最前端）
/// 3 = 其它（工具类，无所谓先后）
///
/// 判层级靠 id 前缀匹配，因为服务 id 形如 `php@8.3.33` / `mysql@8.0.46`。
pub fn tier_of(service_id: &str) -> u8 {
    let base = service_id.split('@').next().unwrap_or(service_id);
    match base {
        "mysql" | "mariadb" | "postgresql" | "mongodb" | "qdrant" | "neo4j" | "redis"
        | "memcached" | "rabbitmq" | "elasticsearch" | "meilisearch" | "zincsearch"
        | "minio" | "rustfs" | "consul" | "etcd" | "r-nacos" | "temporal" => 0,
        "php" | "node" | "python" | "java" | "go" | "dotnet" | "bun" | "deno" | "ruby"
        | "rust" | "zig" | "flutter" | "perl" | "erlang" | "ollama" => 1,
        "nginx" | "apache" | "caddy" | "frankenphp" | "tomcat" | "roadrunner" | "mihomo" => 2,
        _ => 3,
    }
}

/// 按「先上游后前端」排序（启动顺序）
pub fn order_for_start(ids: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ids.to_vec();
    // 稳定排序：同层级保持用户选择的相对顺序，避免界面上的勾选顺序被莫名打乱
    v.sort_by_key(|id| tier_of(id));
    v
}

/// 按「先前端后上游」排序（停止顺序）
pub fn order_for_stop(ids: &[String]) -> Vec<String> {
    let mut v: Vec<String> = ids.to_vec();
    v.sort_by_key(|id| std::cmp::Reverse(tier_of(id)));
    v
}

/// 批量启动
///
/// `skip_already_running` 为 true 时不重复启动已在跑的服务，
/// 只把它们列进 `already`（默认行为，避免 restinterface 引起的短暂中断）。
pub fn start_many(
    store: &crate::store::Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    ids: &[String],
) -> Result<BulkReport> {
    let order = order_for_start(ids);
    let mut report = BulkReport {
        action: "start".into(),
        succeeded: Vec::new(),
        already: Vec::new(),
        failed: Vec::new(),
        order: order.clone(),
    };
    for id in order {
        let running = manager
            .snapshot(&id)
            .map(|s| matches!(s.state, ServiceState::Running | ServiceState::Starting))
            .unwrap_or(false);
        if running {
            report.already.push(id);
            continue;
        }
        match crate::ops::start_service(store, paths, manager, &id) {
            Ok(()) => report.succeeded.push(id),
            Err(e) => report.failed.push(BulkFailure {
                service_id: id,
                error: AppErrorInfo::from(e),
            }),
        }
    }
    crate::ops::save_pidfile(paths, manager);
    Ok(report)
}

/// 批量停止
pub fn stop_many(
    store: &crate::store::Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    ids: &[String],
) -> Result<BulkReport> {
    let order = order_for_stop(ids);
    let mut report = BulkReport {
        action: "stop".into(),
        succeeded: Vec::new(),
        already: Vec::new(),
        failed: Vec::new(),
        order: order.clone(),
    };
    for id in order {
        let running = manager
            .snapshot(&id)
            .map(|s| matches!(s.state, ServiceState::Running | ServiceState::Starting))
            .unwrap_or(false);
        if !running {
            report.already.push(id);
            continue;
        }
        match crate::ops::stop_service(store, paths, manager, &id) {
            Ok(()) => report.succeeded.push(id),
            Err(e) => report.failed.push(BulkFailure {
                service_id: id,
                error: AppErrorInfo::from(e),
            }),
        }
    }
    crate::ops::save_pidfile(paths, manager);
    Ok(report)
}

/// 批量重启：先按停止顺序停，再按启动顺序起。
///
/// 不用逐个 restart——那样共享依赖（如 MySQL）会被反复中断，
/// 一起停再一起起对依赖它的服务更友好。
pub fn restart_many(
    store: &crate::store::Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    ids: &[String],
) -> Result<BulkReport> {
    let stop_report = stop_many(store, paths, manager, ids)?;
    let start_report = start_many(store, paths, manager, ids)?;
    // 合并成一个报告：动作叫 restart，失败项取两阶段的并集
    let mut report = BulkReport {
        action: "restart".into(),
        succeeded: start_report.succeeded,
        already: start_report.already,
        failed: start_report.failed,
        order: start_report.order,
    };
    report.failed.extend(stop_report.failed);
    Ok(report)
}

/// 服务的可批量操作开关状态（前端用来显示「3 个运行中 / 2 个已停止」）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkSelectionSummary {
    pub total: usize,
    pub running: usize,
    pub stopped: usize,
    /// 选中的服务里，是否有正在运行的（决定「停止」按钮是否可点）
    pub can_stop: bool,
    /// 是否有已停止的（决定「启动」是否可点）
    pub can_start: bool,
}

pub fn summarize(manager: &Arc<ServiceManager>, ids: &[String]) -> BulkSelectionSummary {
    let mut running = 0;
    for id in ids {
        if manager
            .snapshot(id)
            .map(|s| matches!(s.state, ServiceState::Running | ServiceState::Starting))
            .unwrap_or(false)
        {
            running += 1;
        }
    }
    BulkSelectionSummary {
        total: ids.len(),
        running,
        stopped: ids.len() - running,
        can_stop: running > 0,
        can_start: ids.len() > running,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_classify_by_base_id() {
        assert_eq!(tier_of("mysql@8.0.46"), 0);
        assert_eq!(tier_of("redis"), 0);
        assert_eq!(tier_of("postgresql@16"), 0);
        assert_eq!(tier_of("php@8.3.33"), 1);
        assert_eq!(tier_of("node@20"), 1);
        assert_eq!(tier_of("nginx"), 2);
        assert_eq!(tier_of("apache@2.4"), 2);
        assert_eq!(tier_of("composer"), 3);
        assert_eq!(tier_of("adminer"), 3);
    }

    #[test]
    fn start_order_puts_databases_before_webserver() {
        let ids = vec!["nginx".to_string(), "mysql@8.0".to_string(), "php@8.3".to_string()];
        let o = order_for_start(&ids);
        assert_eq!(o[0], "mysql@8.0", "数据库必须先起：{o:?}");
        assert_eq!(o[1], "php@8.3");
        assert_eq!(o[2], "nginx", "nginx 必须最后起：{o:?}");
    }

    #[test]
    fn stop_order_is_reverse_of_start() {
        let ids = vec!["nginx".to_string(), "mysql@8.0".to_string(), "php@8.3".to_string()];
        let o = order_for_stop(&ids);
        assert_eq!(o[0], "nginx", "先断流量：{o:?}");
        assert_eq!(o[2], "mysql@8.0", "数据库最后停：{o:?}");
    }

    #[test]
    fn order_preserves_relative_order_within_tier() {
        // 同层级内不该被打乱（用稳定排序），否则界面勾选顺序变了会让用户困惑
        let ids = vec!["redis".to_string(), "mysql".to_string(), "mongodb".to_string()];
        let o = order_for_start(&ids);
        assert_eq!(o, ids, "同层应保持原序：{o:?}");
    }

    #[test]
    fn full_layering_is_correct() {
        let ids = vec![
            "nginx".to_string(),
            "composer".to_string(),
            "php@8.3".to_string(),
            "mysql@8.0".to_string(),
            "redis".to_string(),
        ];
        let o = order_for_start(&ids);
        // 数据层 → 运行层 → Web 层 → 其它
        assert_eq!(o[0], "mysql@8.0");
        assert_eq!(o[1], "redis");
        assert_eq!(o[2], "php@8.3");
        assert_eq!(o[3], "nginx");
        assert_eq!(o[4], "composer");
    }

    #[test]
    fn unknown_service_goes_last_for_start() {
        let ids = vec!["weird-thing".to_string(), "mysql".to_string()];
        let o = order_for_start(&ids);
        assert_eq!(o[0], "mysql");
        assert_eq!(o[1], "weird-thing");
    }

    #[test]
    fn empty_selection_is_handled() {
        let o = order_for_start(&[]);
        assert!(o.is_empty());
        let o = order_for_stop(&[]);
        assert!(o.is_empty());
    }

    #[test]
    fn single_service_order_is_identity() {
        let ids = vec!["nginx".to_string()];
        assert_eq!(order_for_start(&ids), ids);
        assert_eq!(order_for_stop(&ids), ids);
    }

    #[test]
    fn summarize_on_empty_manager() {
        let m = Arc::new(ServiceManager::new());
        let s = summarize(&m, &[]);
        assert_eq!((s.total, s.running, s.stopped), (0, 0, 0));
        assert!(!s.can_start && !s.can_stop);
    }

    #[test]
    fn summarize_reports_all_stopped_for_unknown_ids() {
        let m = Arc::new(ServiceManager::new());
        let ids = vec!["nginx".to_string(), "mysql".to_string()];
        let s = summarize(&m, &ids);
        assert_eq!(s.total, 2);
        assert_eq!(s.running, 0);
        assert_eq!(s.stopped, 2);
        assert!(s.can_start, "都停着 → 可以启动");
        assert!(!s.can_stop, "都停着 → 没啥可停");
    }

    #[test]
    fn bulk_report_serializes_camel_case() {
        let r = BulkReport {
            action: "start".into(),
            succeeded: vec!["a".into()],
            already: vec![],
            failed: vec![],
            order: vec!["a".into()],
        };
        let j = serde_json::to_string(&r).unwrap();
        assert!(j.contains("\"succeeded\""), "{j}");
        assert!(j.contains("\"action\":\"start\""), "{j}");
    }

    #[test]
    fn tiers_cover_every_known_manifest_service() {
        // 清单里的服务都应能落到某个层级（未知会落 3，也算落位）
        for id in [
            "nginx", "apache", "caddy", "frankenphp", "tomcat", "roadrunner",
            "php@8.3.33", "node@20.11", "python@3.12", "go@1.22", "java@21",
            "dotnet@8", "bun@1.1", "deno@1.44", "ruby@3.3", "rust@1.79",
            "zig@0.13", "flutter@3.24", "perl@5.40", "erlang@27",
            "mysql@8.0.46", "mariadb@11", "postgresql@16", "mongodb@7",
            "qdrant@1.9", "neo4j@5", "redis@7.2", "memcached@1.6",
            "rabbitmq@3.13", "elasticsearch@8", "meilisearch@1.8",
            "zincsearch@0.4", "minio@2024", "rustfs@0.1", "consul@1.19",
            "etcd@3.5", "r-nacos@0.6", "temporal@1.24", "ollama@0.3",
            "mihomo@1.18", "composer@2.7", "adminer@4.8",
        ] {
            let t = tier_of(id);
            assert!(t <= 3, "{id} 层级越界：{t}");
        }
    }

    #[test]
    fn infra_services_are_tier_zero() {
        // 这些是「别的服务依赖它」的，必须能先起来
        for id in ["redis", "mysql", "postgresql", "mongodb", "etcd", "consul"] {
            assert_eq!(tier_of(id), 0, "{id} 应属数据/基础设施层");
        }
    }

    #[test]
    fn web_servers_are_tier_two() {
        // 它们依赖上游就绪，必须最后起
        for id in ["nginx", "apache", "caddy", "tomcat", "mihomo"] {
            assert_eq!(tier_of(id), 2, "{id} 应属最前端层");
        }
    }
}
