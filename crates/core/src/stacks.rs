//! 服务栈：用户把「自己的一整套服务」存成一个组合，之后一键启动。
//!
//! 栈只是「有序的服务 id 列表」——不复制服务的启动逻辑，启动时仍然走
//! `ops::start_service`，因此 nginx 的 reload、MySQL 的初始化、php 端口池
//! 这些关键处理一个都不会绕过。
//!
//! 内置预设（LNMP / 纯前端 / 数据栈）随应用提供，可复制成自定义栈再改。

use crate::error::{AppError, Result};
use crate::model::{
    AppErrorInfo, ServiceState, Stack, StackInput, StackItem, StackItemFailure, StackStartReport,
};
use crate::paths::Paths;
use crate::services::ServiceManager;
use crate::store::Store;
use std::sync::Arc;

/// 内置预设：id 固定、builtin=true（不可删）
fn presets(now: i64) -> Vec<Stack> {
    let item = |service_id: &str, order: i32| StackItem {
        service_id: service_id.to_string(),
        label: None,
        order,
    };
    vec![
        Stack {
            id: "builtin-lnmp".into(),
            name: "LNMP 经典".into(),
            description: "Nginx + MySQL + PHP，最通用的 PHP 本地开发环境".into(),
            // 顺序：先库后 web —— php-cgi 起来后 nginx 才有 upstream 可用
            items: vec![item("mysql", 10), item("php", 20), item("nginx", 30)],
            builtin: true,
            created_at: now,
            updated_at: now,
        },
        Stack {
            id: "builtin-web".into(),
            name: "前端 / 静态站点".into(),
            description: "只要一个 Web 服务器，托管静态产物或反代 dev server".into(),
            items: vec![item("nginx", 10)],
            builtin: true,
            created_at: now,
            updated_at: now,
        },
        Stack {
            id: "builtin-data".into(),
            name: "数据栈".into(),
            description: "MySQL + Redis，跑后端服务或调试数据时常用".into(),
            items: vec![item("mysql", 10), item("redis", 20)],
            builtin: true,
            created_at: now,
            updated_at: now,
        },
    ]
}

/// 首次调用时写入内置预设（已存在则不动，用户改过的名字不会被覆盖）
pub fn ensure_presets(store: &Store) -> Result<()> {
    if !store.list_stacks()?.is_empty() {
        return Ok(());
    }
    let now = crate::services::now_ms();
    for s in presets(now) {
        store.save_stack(&s)?;
    }
    Ok(())
}

pub fn list(store: &Store) -> Result<Vec<Stack>> {
    ensure_presets(store)?;
    let mut all = store.list_stacks()?;
    // 内置预设排前面，其余按更新时间
    all.sort_by_key(|s| (!s.builtin, -s.updated_at));
    Ok(all)
}

/// 新建或更新；返回落库后的栈
pub fn save(store: &Store, input: StackInput) -> Result<Stack> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::new("BAD_STACK", "栈名称不能为空"));
    }
    if input.items.is_empty() {
        return Err(AppError::new("BAD_STACK", "栈里至少要有一个服务")
            .with_hint("从「已安装的服务」里勾选需要一起启动的项"));
    }
    let now = crate::services::now_ms();
    let existing = match &input.id {
        Some(id) => Some(store.get_stack(id)?.ok_or_else(|| {
            AppError::new("STACK_NOT_FOUND", "此服务栈已被删除，请关闭编辑器后刷新列表")
        })?),
        None => None,
    };
    let stack = match existing {
        Some(mut s) => {
            if s.builtin {
                return Err(AppError::new("STACK_BUILTIN", "内置预设不能直接修改")
                    .with_hint("先「另存为」一份再改，预设本身留着当模板"));
            }
            s.name = name.to_string();
            s.description = input.description;
            s.items = normalized_items(input.items);
            s.updated_at = now;
            s
        }
        None => Stack {
            id: format!("stack-{now}-{:016x}", rand::random::<u64>()),
            name: name.to_string(),
            description: input.description,
            items: normalized_items(input.items),
            builtin: false,
            created_at: now,
            updated_at: now,
        },
    };
    store.save_stack(&stack)?;
    Ok(stack)
}

/// 复制一份（内置预设 → 可编辑副本；普通栈 → 换个名字的副本）
pub fn duplicate(store: &Store, id: &str, name: Option<String>) -> Result<Stack> {
    let src = store
        .get_stack(id)?
        .ok_or_else(|| AppError::new("STACK_NOT_FOUND", format!("找不到服务栈 {id}")))?;
    let now = crate::services::now_ms();
    let stack = Stack {
        id: format!("stack-{now}-{:016x}", rand::random::<u64>()),
        name: name.unwrap_or_else(|| format!("{} 副本", src.name)),
        description: src.description,
        items: src.items,
        builtin: false,
        created_at: now,
        updated_at: now,
    };
    store.save_stack(&stack)?;
    Ok(stack)
}

pub fn delete(store: &Store, id: &str) -> Result<()> {
    match store.get_stack(id)? {
        None => Err(AppError::new(
            "STACK_NOT_FOUND",
            format!("找不到服务栈 {id}"),
        )),
        Some(s) if s.builtin => Err(AppError::new("STACK_BUILTIN", "内置预设不能删除")
            .with_hint("它是模板；可以复制成自己的栈再删副本")),
        Some(_) => {
            store.delete_stack(id)?;
            Ok(())
        }
    }
}

/// 排序 + 去重（同一服务只留一条）
fn normalized_items(mut items: Vec<StackItem>) -> Vec<StackItem> {
    items.sort_by_key(|i| i.order);
    let mut seen = std::collections::HashSet::new();
    items.retain(|i| seen.insert(i.service_id.clone()));
    items
}

/// 把栈里的 id 展开成「实际存在、能启动的服务 id」。
/// - 无版本的通用 id（mysql/php/nginx）→ 当前「使用中版本」对应的 service id
/// - 未安装 / 非服务的项直接跳过，并在报告里说明
fn resolve_items(
    store: &Store,
    manager: &Arc<ServiceManager>,
    stack: &Stack,
) -> (Vec<StackItem>, Vec<String>) {
    let known: Vec<String> = manager.list_status().into_iter().map(|s| s.id).collect();
    let mut runnable = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in normalized_items(stack.items.clone()) {
        let sid = resolve_service_id(store, &known, &item.service_id);
        match sid {
            Some(sid) if seen.insert(sid.clone()) => runnable.push(StackItem {
                service_id: sid,
                label: item.label,
                order: item.order,
            }),
            Some(_) => {},
            None => skipped.push(item.service_id),
        }
    }
    (runnable, skipped)
}

/// 把栈里写的 id 映射到 manager 里注册过的真实 service id
fn resolve_service_id(store: &Store, known: &[String], wanted: &str) -> Option<String> {
    if known.iter().any(|k| k == wanted) {
        return Some(wanted.to_string());
    }
    // 显式固定的版本不存在时必须报告缺失，不能悄悄换成另一个版本。
    if wanted.contains('@') {
        return None;
    }
    // 所有按版本注册的服务都跟随已选择版本，不能取 HashMap 中的第一项。
    if let Some(inst) = crate::ops::installed_by_choice(store, wanted) {
        let sid = format!("{}@{}", wanted, inst.version);
        if known.iter().any(|k| k == &sid) {
            return Some(sid);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fixed_version_does_not_start_or_count_another_version() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("fixture.sqlite")).unwrap();
        store
            .upsert_installed(&crate::model::InstalledPackage {
                id: "php".into(),
                version: "8.4.0".into(),
                category: "runtime".into(),
                install_path: String::new(),
                config_path: String::new(),
                installed_at: 0,
            })
            .unwrap();
        let known = vec!["php@8.4.0".to_string()];
        assert_eq!(
            resolve_service_id(&store, &known, "php"),
            Some("php@8.4.0".into())
        );
        assert_eq!(resolve_service_id(&store, &known, "php@8.3.0"), None);
        let manager = Arc::new(ServiceManager::new());
        manager.register(
            "php@8.4.0",
            "PHP",
            Some("8.4.0".into()),
            None,
            None,
            Default::default(),
        );
        let stack: Stack = serde_json::from_value(serde_json::json!({
            "id":"fixture","name":"Fixed PHP","items":[{"serviceId":"php@8.3.0"}],
            "createdAt":0,"updatedAt":0
        }))
        .unwrap();
        let (items, skipped) = resolve_items(&store, &manager, &stack);
        assert!(items.is_empty());
        assert_eq!(skipped, ["php@8.3.0"]);
        assert_eq!(status_of(&store, &manager, &stack), (0, 1));
        let mut aliases = stack;
        aliases.items = serde_json::from_value(serde_json::json!([
            {"serviceId":"php"}, {"serviceId":"php@8.4.0"}, {"serviceId":"php"}
        ])).unwrap();
        let (items, skipped) = resolve_items(&store, &manager, &aliases);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].service_id, "php@8.4.0");
        assert!(skipped.is_empty());
    }

    #[test]
    fn selected_version_and_missing_items_control_summary() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("fixture.sqlite")).unwrap();
        let manager = Arc::new(ServiceManager::new());
        for version in ["8.3.17", "8.4.26"] {
            store.upsert_installed(&crate::model::InstalledPackage {
                id: "php".into(), version: version.into(), category: "runtime".into(),
                install_path: String::new(), config_path: String::new(), installed_at: 0,
            }).unwrap();
            manager.register(&format!("php@{version}"), "PHP", Some(version.into()), None, None, Default::default());
        }
        // 仅状态解析：借用当前测试 PID 判断存活，不启动或结束任何进程。
        manager.adopt("php@8.3.17", &[std::process::id()], None);
        let mut stack: Stack = serde_json::from_value(serde_json::json!({
            "id":"versions","name":"Versions","items":[{"serviceId":"php"}],"createdAt":0,"updatedAt":0
        })).unwrap();
        assert_eq!(resolve_items(&store, &manager, &stack).0[0].service_id, "php@8.4.26");
        assert_eq!(status_of(&store, &manager, &stack), (0, 1));
        store.set_setting("activephpVersion", "8.3.17").unwrap();
        assert_eq!(resolve_items(&store, &manager, &stack).0[0].service_id, "php@8.3.17");
        assert_eq!(status_of(&store, &manager, &stack), (1, 1));
        stack.items.extend([
            StackItem { service_id: "php@8.3.17".into(), label: None, order: 10 },
            StackItem { service_id: "php@missing".into(), label: None, order: 20 },
        ]);
        assert_eq!(status_of(&store, &manager, &stack), (1, 2));
    }

    #[test]
    fn saving_keeps_version_rules_and_does_not_recreate_deleted_stacks() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("fixture.sqlite")).unwrap();
        ensure_presets(&store).unwrap();
        let input = || StackInput {
            id: None, name: "Versions".into(), description: String::new(),
            items: vec![StackItem { service_id: "php".into(), label: None, order: 10 },
                StackItem { service_id: "php@8.3.17".into(), label: None, order: 20 }],
        };
        let original = save(&store, input()).unwrap();
        for _ in 0..32 { duplicate(&store, &original.id, None).unwrap(); }
        assert_eq!(store.list_stacks().unwrap().len(), 36);
        assert_eq!(store.get_stack(&original.id).unwrap().unwrap().items[1].service_id, "php@8.3.17");
        delete(&store, &original.id).unwrap();
        let mut stale = input(); stale.id = Some(original.id.clone());
        assert_eq!(save(&store, stale).unwrap_err().code, "STACK_NOT_FOUND");
        assert!(store.get_stack(&original.id).unwrap().is_none());
    }
}

/// 一键启动：逐项串行（服务之间有依赖顺序），单项失败不阻断后续。
pub fn start(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    id: &str,
) -> Result<StackStartReport> {
    let _operation = manager.lifecycle.lock();
    // 预设只在「首次列出」时写入；启动路径也要保证它存在，
    // 否则全新环境下点托盘的预设栈会报「找不到服务栈」
    ensure_presets(store)?;
    let stack = store
        .get_stack(id)?
        .ok_or_else(|| AppError::new("STACK_NOT_FOUND", format!("找不到服务栈 {id}")))?;
    let (items, skipped) = resolve_items(store, manager, &stack);
    if items.is_empty() {
        return Err(AppError::new(
            "STACK_EMPTY",
            format!("「{}」里没有可启动的服务", stack.name),
        )
        .with_hint(format!(
            "栈里的服务都还没安装（{}）；先到「套件 / 服务」页装好再启动",
            skipped.join(", ")
        )));
    }

    let mut report = StackStartReport {
        stack_id: stack.id.clone(),
        started: Vec::new(),
        already_running: Vec::new(),
        skipped,
        failed: Vec::new(),
    };

    for item in items {
        if manager.snapshot(&item.service_id).is_some_and(|s| matches!(s.state, ServiceState::Starting | ServiceState::Stopping)) {
            report.failed.push(StackItemFailure {
                service_id: item.service_id.clone(),
                error: AppErrorInfo::from(AppError::new("SERVICE_BUSY", format!("服务 {} 正在切换状态，请稍后重试", item.service_id))),
            });
            continue;
        }
        let running = manager
            .snapshot(&item.service_id)
            .map(|s| s.state == ServiceState::Running)
            .unwrap_or(false);
        if running {
            report.already_running.push(item.service_id);
            continue;
        }
        match crate::ops::start_service(store, paths, manager, &item.service_id) {
            Ok(()) => report.started.push(item.service_id),
            Err(e) => report.failed.push(StackItemFailure {
                service_id: item.service_id,
                error: AppErrorInfo::from(e),
            }),
        }
    }
    crate::ops::save_pidfile(paths, manager);
    Ok(report)
}

/// 停止栈里所有正在运行的服务（逆序停止：先 web 后库）
pub fn stop(
    store: &Store,
    paths: &Paths,
    manager: &Arc<ServiceManager>,
    id: &str,
) -> Result<StackStartReport> {
    let _operation = manager.lifecycle.lock();
    ensure_presets(store)?;
    let stack = store
        .get_stack(id)?
        .ok_or_else(|| AppError::new("STACK_NOT_FOUND", format!("找不到服务栈 {id}")))?;
    let (mut items, skipped) = resolve_items(store, manager, &stack);
    // 逆序：nginx 先停，数据库最后停
    items.reverse();

    let mut report = StackStartReport {
        stack_id: stack.id.clone(),
        started: Vec::new(),
        already_running: Vec::new(),
        skipped,
        failed: Vec::new(),
    };
    for item in items {
        // 认证停机失败后可以处于 Error 且仍持有进程，继续尝试真实停机。
        if !manager.is_busy(&item.service_id) {
            report.already_running.push(item.service_id);
            continue;
        }
        match crate::ops::stop_service(store, paths, manager, &item.service_id) {
            Ok(()) => report.started.push(item.service_id),
            Err(e) => report.failed.push(StackItemFailure {
                service_id: item.service_id,
                error: AppErrorInfo::from(e),
            }),
        }
    }
    crate::ops::save_pidfile(paths, manager);
    Ok(report)
}

/// 栈的运行态摘要：运行中 / 共几项（前端列表与托盘菜单显示用）
pub fn status_of(store: &Store, manager: &Arc<ServiceManager>, stack: &Stack) -> (usize, usize) {
    let (items, skipped) = resolve_items(store, manager, stack);
    let total = items.len() + skipped.len();
    let running = items.iter().filter(|item| {
        manager.snapshot(&item.service_id).is_some_and(|s| s.state == ServiceState::Running)
    }).count();
    (running, total)
}
