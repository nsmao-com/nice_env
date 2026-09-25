//! 端口诊断：谁占用了端口 → pid → 进程名/命令行。
//! 排查过程绝不自动 kill；结束进程只发生在用户明确点了按钮之后。

use crate::error::Result;
use crate::model::{ListenerInfo, PortDiagnosis, PortRangeScan, PortScanEntry};
use crate::services::{PortsProfile, ServiceManager};
use crate::store::Store;
use std::sync::Arc;

pub fn diagnose_port(port: u16) -> Result<PortDiagnosis> {
    let listeners = platform_listeners()?;
    Ok(listeners
        .into_iter()
        .find(|(p, _)| *p == port)
        .map(|(p, pid)| PortDiagnosis {
            port: p,
            in_use: true,
            pid: Some(pid),
            process_name: process_name(pid),
            cmdline: process_cmdline(pid),
        })
        .unwrap_or(PortDiagnosis {
            port,
            in_use: false,
            pid: None,
            process_name: None,
            cmdline: None,
        }))
}

/// 端口区间扫描：一次拿全表，返回范围内所有监听者（含归属标注）。
/// 工具箱「扫某个端口 → 结束占用它的进程」用这个；单端口也能用（from == to）。
pub fn scan_port_range(
    manager: &Arc<ServiceManager>,
    from: u16,
    to: u16,
) -> Result<PortRangeScan> {
    let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
    let listeners = platform_listeners()?;
    // pid → serviceId：用于标注「这个监听者是本应用哪个服务」
    let mut owned: Vec<(u32, String)> = Vec::new();
    for s in manager.list_status() {
        for pid in s.pids {
            owned.push((pid, s.id.clone()));
        }
    }

    let mut out: Vec<ListenerInfo> = listeners
        .into_iter()
        .filter(|(p, _)| *p >= lo && *p <= hi)
        .map(|(port, pid)| {
            let service_id = owned.iter().find(|(op, _)| *op == pid).map(|(_, sid)| sid.clone());
            ListenerInfo {
                port,
                pid,
                process_name: process_name(pid),
                cmdline: process_cmdline(pid),
                owned_by_self: service_id.is_some(),
                service_id,
            }
        })
        .collect();
    out.sort_by_key(|l| (l.port, l.pid));
    Ok(PortRangeScan {
        from: lo,
        to: hi,
        listeners: out,
        scanned_at: crate::services::now_ms(),
    })
}

/// 一次 netstat 拿到的全部监听者（诊断/体检内部使用）
pub fn listeners() -> Result<Vec<(u16, u32)>> {
    platform_listeners()
}

/// 结束占用某端口的进程。
/// `prefer_graceful` 为真且该端口是本应用某服务在跑时，走该服务的正常停止流程
/// （MySQL 会干净关库、nginx 会先 reload 停）；否则直接 kill 该 pid。
/// 返回 (是否走了服务停止, 被处理的 pid 列表, 服务 id)。
pub fn close_port(
    store: &Store,
    paths: &crate::paths::Paths,
    manager: &Arc<ServiceManager>,
    port: u16,
) -> Result<ClosePortOutcome> {
    let listeners = platform_listeners()?;
    let pids: Vec<u32> = listeners
        .iter()
        .filter(|(p, _)| *p == port)
        .map(|(_, pid)| *pid)
        .collect();
    if pids.is_empty() {
        return Ok(ClosePortOutcome {
            port,
            graceful: false,
            service_id: None,
            killed_pids: Vec::new(),
        });
    }

    // 本应用自己的服务占着这个端口 → 走正常停止（带优雅关闭）
    let mut service_id: Option<String> = None;
    for s in manager.list_status() {
        if s.pids.iter().any(|p| pids.contains(p)) {
            service_id = Some(s.id.clone());
            break;
        }
    }
    if let Some(sid) = service_id {
        crate::ops::stop_service(store, paths, manager, &sid)?;
        crate::ops::save_pidfile(paths, manager);
        return Ok(ClosePortOutcome {
            port,
            graceful: true,
            service_id: Some(sid),
            killed_pids: pids,
        });
    }

    // 外部进程：用户明确要求结束，逐个 kill
    let mut killed = Vec::new();
    for pid in &pids {
        if kill_pid(*pid)? {
            killed.push(*pid);
        }
    }
    if killed.is_empty() {
        return Err(crate::AppError::new(
            "KILL_FAILED",
            format!("无法结束占用端口 {port} 的进程"),
        )
        .with_hint("该进程可能需要管理员权限；试着以管理员身份重开应用后再操作")
        .with_pid(pids[0]));
    }
    Ok(ClosePortOutcome {
        port,
        graceful: false,
        service_id: None,
        killed_pids: killed,
    })
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ClosePortOutcome {
    pub port: u16,
    /// true = 走的是本应用服务停止流程（优雅），false = 直接 kill 外部进程
    pub graceful: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
    pub killed_pids: Vec<u32>,
}

/// 全量体检：把本应用「当前会绑定的所有端口」逐个比对实际占用者。
/// 一次 netstat/lsof 调用拿全表，避免 N 次进程创建。
/// 结论三态：free / self（是自己的服务在跑）/ conflict（被别人占了，起不来）。
pub fn scan_app_ports(store: &Store, manager: &Arc<ServiceManager>) -> Result<Vec<PortScanEntry>> {
    let listeners = platform_listeners()?;
    let ports = PortsProfile::from_settings(store);

    // (service_id, label, port) —— 与 ops::start_* 里 precheck 的端口一一对应
    let mut wanted: Vec<(String, String, u16)> = vec![
        ("nginx".into(), "Nginx".into(), ports.http),
        ("nginx".into(), "Nginx (HTTPS)".into(), ports.https),
        ("apache".into(), "Apache".into(), ports.apache_http),
        ("apache".into(), "Apache (HTTPS)".into(), ports.apache_https),
        ("mysql".into(), "MySQL".into(), ports.mysql),
        ("postgresql".into(), "PostgreSQL".into(), ports.postgres),
        ("mongodb".into(), "MongoDB".into(), ports.mongodb),
        ("redis".into(), "Redis".into(), ports.redis),
        (
            "mihomo".into(),
            "mihomo 混合端口".into(),
            crate::configgen::MIHOMO_MIXED_PORT,
        ),
        (
            "mihomo".into(),
            "mihomo 控制端口".into(),
            crate::configgen::MIHOMO_CONTROLLER_PORT,
        ),
    ];

    // 已安装的 MySQL 把 id 细化为带版本的 serviceId（mysql@8.0.46），与 manager 注册一致
    if let Some(v) = store.find_installed("mysql", None).map(|p| p.version) {
        for row in wanted.iter_mut().filter(|r| r.0 == "mysql") {
            row.0 = format!("mysql@{v}");
            row.1 = format!("MySQL {v}");
        }
    }

    // php 各版本端口池
    for (sid, base) in store.all_port_assigns() {
        if !sid.starts_with("php@") {
            continue;
        }
        let version = sid.trim_start_matches("php@").to_string();
        for i in 0..crate::configgen::PHP_POOL_WORKERS {
            wanted.push((
                format!("php@{version}"),
                format!("PHP {version} worker {}", i + 1),
                base + i,
            ));
        }
    }

    // 清单驱动的基础服务（Caddy / Meilisearch / MinIO / Mailpit …）：
    // 已安装且解析出端口的，纳入体检（端口来自分配表或清单 defaultPort）
    if let Ok(installed) = store.list_installed() {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in installed {
            if crate::generic::is_builtin(&p.id) {
                continue; // 内置服务的端口已在上面的固定表中列出
            }
            let sid = crate::generic::service_id_for(&p);
            if !seen.insert(sid.clone()) || wanted.iter().any(|w| w.0 == sid) {
                continue;
            }
            let Some(entry) = crate::generic::manifest_entry_for(store, &sid) else {
                continue;
            };
            if entry.run.is_none() {
                continue;
            }
            // 端口取值优先分配表；未分配时按当前端口方案推导（与启动逻辑一致）
            let port = store.get_port_assign(&sid).or_else(|| {
                crate::generic::planned_port(store, &sid).filter(|_| {
                    // 标准档直接用清单端口；安全档由 planned_port 给出的偏移值
                    true
                })
            });
            if let Some(port) = port {
                wanted.push((sid, entry.display_name, port));
            }
        }
    }

    let mut out = Vec::with_capacity(wanted.len());
    for (service_id, label, port) in wanted {
        let running = manager
            .snapshot(&service_id)
            .map(|s| s.state == crate::model::ServiceState::Running)
            .unwrap_or(false);
        let own_pids: Vec<u32> = manager
            .snapshot(&service_id)
            .map(|s| s.pids)
            .unwrap_or_default();
        let holder = listeners.iter().find(|(p, _)| *p == port).map(|(_, pid)| *pid);

        let (verdict, pid, pname, cmdline) = match holder {
            None => ("free", None, None, None),
            Some(pid) if own_pids.contains(&pid) => (
                "self",
                Some(pid),
                process_name(pid),
                process_cmdline(pid),
            ),
            Some(pid) => (
                "conflict",
                Some(pid),
                process_name(pid),
                process_cmdline(pid),
            ),
        };

        out.push(PortScanEntry {
            service_id,
            label,
            port,
            owned_by_self: verdict == "self",
            pid,
            process_name: pname,
            cmdline,
            running,
            verdict: verdict.to_string(),
        });
    }

    out.sort_by_key(|e| e.port);
    Ok(out)
}

/// 结束进程（用户确认后调用）
pub fn kill_pid(pid: u32) -> Result<bool> {
    #[cfg(windows)]
    {
        let out = platform::command("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
            .map_err(|e| crate::error::AppError::io("结束进程", e))?;
        Ok(out.status.success())
    }
    #[cfg(not(windows))]
    {
        let out = platform::command("kill")
            .arg("-9")
            .arg(pid.to_string())
            .output()
            .map_err(|e| crate::error::AppError::io("结束进程", e))?;
        Ok(out.status.success())
    }
}

/// (port, pid) 监听列表
fn platform_listeners() -> Result<Vec<(u16, u32)>> {
    let out = if cfg!(windows) {
        platform::command("netstat")
            .args(["-ano", "-p", "tcp"])
            .output()
            .map_err(|e| crate::error::AppError::io("执行 netstat", e))?
    } else {
        platform::command("lsof")
            .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
            .output()
            .map_err(|e| crate::error::AppError::io("执行 lsof", e))?
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut result = Vec::new();

    #[cfg(windows)]
    {
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Proto Local-Address Foreign-Address State PID
            if parts.len() >= 5 && parts[3].eq_ignore_ascii_case("LISTENING") {
                if let Some((port, pid)) = parse_addr_pid(parts[1], parts[4]) {
                    result.push((port, pid));
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME
            if parts.len() >= 9 {
                if let Some((port, pid)) = parse_lsof(parts[8], parts[1]) {
                    result.push((port, pid));
                }
            }
        }
    }
    Ok(result)
}

#[cfg(windows)]
fn parse_addr_pid(addr: &str, pid: &str) -> Option<(u16, u32)> {
    let port = addr.rsplit(':').next()?.parse().ok()?;
    let pid = pid.parse().ok()?;
    Some((port, pid))
}

#[cfg(not(windows))]
fn parse_lsof(name: &str, pid: &str) -> Option<(u16, u32)> {
    // *:8080 (LISTEN) 或 127.0.0.1:3306
    let port = name.split(':').last()?.parse().ok()?;
    let pid = pid.parse().ok()?;
    Some((port, pid))
}

pub fn process_name(pid: u32) -> Option<String> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true);
    sys.process(Pid::from_u32(pid))
        .map(|p| p.name().to_string_lossy().to_string())
}

pub fn process_cmdline(pid: u32) -> Option<String> {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true);
    sys.process(Pid::from_u32(pid))
        .and_then(|p| {
            let cmd = p.cmd();
            if cmd.is_empty() {
                None
            } else {
                Some(
                    cmd.iter()
                        .map(|s| s.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            }
        })
}
