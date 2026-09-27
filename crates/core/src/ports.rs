//! 端口诊断：谁占用了端口 → pid → 进程名/命令行。
//! 排查过程绝不自动 kill；结束进程只发生在用户明确点了按钮之后。

use crate::error::{AppError, Result};
use crate::health::{ownership, Ownership};
use crate::model::{ListenerInfo, PortDiagnosis, PortRangeScan, PortScanEntry};
use crate::services::{PortsProfile, ServiceManager};
use crate::store::Store;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub fn diagnose_port(port: u16) -> Result<PortDiagnosis> {
    validate_range(port, port)?;
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

/// 同一次系统进程快照供监听者归属、名称和身份核对使用。
struct ProcessSnapshot {
    system: sysinfo::System,
    parents: HashMap<u32, (u32, String)>,
}
impl ProcessSnapshot {
    fn read() -> Self {
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let parents = system
            .processes()
            .iter()
            .map(|(pid, p)| {
                (
                    pid.as_u32(),
                    (
                        p.parent().map(|p| p.as_u32()).unwrap_or(0),
                        p.name().to_string_lossy().into_owned(),
                    ),
                )
            })
            .collect();
        Self { system, parents }
    }

    fn listener(
        &self,
        port: u16,
        pid: u32,
        services: &[crate::model::ServiceStatus],
    ) -> ListenerInfo {
        let service = services
            .iter()
            .find(|s| ownership(pid, &s.pids, &self.parents) == Ownership::Own);
        let known = service.is_some()
            || services.iter().all(|s| {
                s.pids.is_empty() || ownership(pid, &s.pids, &self.parents) == Ownership::Other
            });
        let process = self.system.process(sysinfo::Pid::from_u32(pid));
        let process_started_at = process.map(|p| p.start_time()).filter(|t| *t > 0);
        let protected = pid <= 4
            || pid == std::process::id()
            || ownership(std::process::id(), &[pid], &self.parents) == Ownership::Own;
        let close_reason = if protected {
            Some("系统进程或 NiceEnv 所在进程链不能从此处结束".into())
        } else if !known {
            Some("无法确认进程归属，请重新扫描或在系统工具中检查".into())
        } else if process_started_at.is_none() {
            Some("无法读取进程身份，不能安全结束，请重新扫描".into())
        } else {
            None
        };
        ListenerInfo {
            port,
            pid,
            process_name: process.map(|p| p.name().to_string_lossy().into_owned()),
            cmdline: process.filter(|p| !p.cmd().is_empty()).map(|p| {
                p.cmd()
                    .iter()
                    .map(|s| s.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
            owned_by_self: service.is_some(),
            service_id: service.map(|s| s.id.clone()),
            process_started_at,
            ownership: if service.is_some() {
                "self"
            } else if known && process.is_some() {
                "external"
            } else {
                "unknown"
            }
            .into(),
            can_close: close_reason.is_none(),
            close_reason,
        }
    }
}

fn validate_range(from: u16, to: u16) -> Result<(u16, u16)> {
    if from == 0 || to == 0 {
        return Err(AppError::new("BAD_PORT", "端口必须为 1 到 65535 的整数"));
    }
    Ok((from.min(to), from.max(to)))
}

/// 返回每个不同的 (端口, PID)，合并同一进程的 IPv4 / IPv6 重复记录。
pub fn scan_port_range(manager: &Arc<ServiceManager>, from: u16, to: u16) -> Result<PortRangeScan> {
    let (lo, hi) = validate_range(from, to)?;
    let _operation = manager
        .lifecycle
        .try_lock()
        .ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重新扫描"))?;
    let listeners = platform_listeners()?;
    let snapshot = ProcessSnapshot::read();
    let services = manager.list_status();
    let mut out = listeners
        .into_iter()
        .filter(|(port, _)| *port >= lo && *port <= hi)
        .map(|(port, pid)| snapshot.listener(port, pid, &services))
        .collect::<Vec<_>>();
    out.sort_by_key(|l| (l.port, l.pid));
    out.dedup_by_key(|l| (l.port, l.pid));
    Ok(PortRangeScan {
        from: lo,
        to: hi,
        listeners: out,
        scanned_at: crate::services::now_ms(),
    })
}

pub fn listeners() -> Result<Vec<(u16, u32)>> {
    platform_listeners()
}

/// 仅供已启用自动释放端口的内部启动流程；UI 必须传扫描时选中的监听者。
pub fn close_port(
    store: &Store,
    paths: &crate::paths::Paths,
    manager: &Arc<ServiceManager>,
    port: u16,
) -> Result<ClosePortOutcome> {
    let expected = scan_port_range(manager, port, port)?.listeners;
    close_port_checked(store, paths, manager, port, &expected)
}

fn validate_close_targets(
    port: u16,
    expected: &[ListenerInfo],
    current: &[ListenerInfo],
) -> Result<Vec<ListenerInfo>> {
    validate_range(port, port)?;
    if expected.len() > 128 {
        return Err(AppError::new(
            "BAD_PORT_TARGETS",
            "一次处理的监听进程过多，请分别操作",
        ));
    }
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for target in expected {
        if target.port != port || !seen.insert(target.pid) {
            return Err(AppError::new(
                "BAD_PORT_TARGETS",
                "监听者选择无效，请重新扫描",
            ));
        }
        // 已退出的旧 PID 不再处理，也不替换成新占用者。
        let Some(now) = current.iter().find(|l| l.pid == target.pid) else {
            continue;
        };
        if target.process_started_at.is_none()
            || target.process_started_at != now.process_started_at
            || target.service_id != now.service_id
            || target.ownership != now.ownership
            || target.process_name != now.process_name
            || target.cmdline != now.cmdline
        {
            return Err(AppError::new(
                "PORT_TARGET_CHANGED",
                "占用进程或服务归属已变化，请重新扫描并确认",
            ));
        }
        if !now.can_close {
            return Err(AppError::new(
                "PORT_TARGET_PROTECTED",
                now.close_reason
                    .clone()
                    .unwrap_or_else(|| "当前进程不能安全结束".into()),
            ));
        }
        selected.push(now.clone());
    }
    Ok(selected)
}

/// 只处理已选对象；任何新监听者均保留，结果以操作后的实际扫描为准。
pub fn close_port_checked(
    store: &Store,
    paths: &crate::paths::Paths,
    manager: &Arc<ServiceManager>,
    port: u16,
    expected: &[ListenerInfo],
) -> Result<ClosePortOutcome> {
    let _operation = manager
        .lifecycle
        .try_lock()
        .ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
    let current = scan_port_range(manager, port, port)?.listeners;
    let targets = validate_close_targets(port, expected, &current)?;
    let mut stopped = HashSet::new();
    let mut killed = Vec::new();
    let mut errors = Vec::new();
    for target in targets {
        if target
            .service_id
            .as_ref()
            .is_some_and(|id| stopped.contains(id))
        {
            continue;
        }
        // 每个动作前再次采集，避免前一个动作期间 PID 或归属发生变化。
        let check = scan_port_range(manager, port, port).and_then(|scan| {
            validate_close_targets(port, std::slice::from_ref(&target), &scan.listeners)
        });
        let live = match check {
            Ok(live) => live,
            Err(e) => {
                errors.push(e.message);
                continue;
            }
        };
        if live.is_empty() {
            continue;
        }
        let result = if let Some(id) = &target.service_id {
            crate::ops::stop_service(store, paths, manager, id).map(|()| {
                stopped.insert(id.clone());
            })
        } else {
            kill_pid(target.pid).map(|_| ())
        };
        match result {
            Ok(()) => killed.push(target.pid),
            Err(e) => errors.push(e.message),
        }
    }
    if !stopped.is_empty() {
        crate::ops::save_pidfile(paths, manager);
    }
    // 外部结束命令成功也可能尚未退出；最多等待 1 秒，绝不把未退出当作成功释放。
    let mut remaining = scan_port_range(manager, port, port)?.listeners;
    for _ in 0..4 {
        if !remaining.iter().any(|row| killed.contains(&row.pid)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
        remaining = scan_port_range(manager, port, port)?.listeners;
    }
    let port_free = remaining.is_empty();
    let service_id = if stopped.len() == 1 {
        stopped.iter().next().cloned()
    } else {
        None
    };
    Ok(ClosePortOutcome {
        port,
        graceful: !stopped.is_empty(),
        service_id,
        killed_pids: killed,
        port_free,
        remaining,
        errors,
    })
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ClosePortOutcome {
    pub port: u16,
    /// 经过服务正常停止流程；并非对应用自身数据完整性的额外保证。
    pub graceful: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
    pub killed_pids: Vec<u32>,
    pub port_free: bool,
    pub remaining: Vec<ListenerInfo>,
    pub errors: Vec<String>,
}

/// 检查已注册实例的实际主端口与停止实例计划端口，同时列出附加端口计划。
pub fn scan_app_ports(store: &Store, manager: &Arc<ServiceManager>) -> Result<Vec<PortScanEntry>> {
    let _operation = manager
        .lifecycle
        .try_lock()
        .ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重新检查"))?;
    let services = manager.list_status();
    let mut targets = crate::health::port_targets(store, &services)?;
    let profile = PortsProfile::from_settings_checked(store)?;
    let mut planned = HashSet::new();
    for service in &services {
        let extra = match service.id.as_str() {
            "nginx" => Some((profile.https, "HTTPS 计划端口")),
            "apache" => Some((profile.apache_https, "HTTPS 计划端口")),
            "mihomo" => Some((crate::configgen::MIHOMO_CONTROLLER_PORT, "控制端口")),
            _ => None,
        };
        if let Some((port, label)) = extra {
            planned.insert((service.id.clone(), port));
            targets.push(crate::health::PortTarget {
                service_id: service.id.clone(),
                label: format!("{} · {label}", service.label),
                port,
                running: false,
                pids: service.pids.clone(),
            });
        }
    }
    let listeners = platform_listeners()?;
    let snapshot = ProcessSnapshot::read();
    let mut out = Vec::new();
    for target in targets {
        let pids = listeners
            .iter()
            .filter(|(port, _)| *port == target.port)
            .map(|(_, pid)| *pid)
            .collect::<std::collections::BTreeSet<_>>();
        let owners = pids
            .iter()
            .map(|pid| (*pid, ownership(*pid, &target.pids, &snapshot.parents)))
            .collect::<Vec<_>>();
        let (verdict, holder) = port_verdict(target.running, &owners);
        let listener = holder.map(|pid| snapshot.listener(target.port, pid, &services));
        let detail = if planned.contains(&(target.service_id.clone(), target.port)) {
            "按当前设置检查附加端口计划；未确认服务当前是否启用此监听"
        } else if services
            .iter()
            .any(|service| service.id == target.service_id && !service.pids.is_empty())
        {
            "按当前实例实际端口检查 TCP 监听，不验证业务响应"
        } else if services.iter().any(|service| {
            service.id == target.service_id && service.state == crate::model::ServiceState::Stopped
        }) {
            "按停止实例的计划端口检查占用，不代表服务正在运行"
        } else {
            "服务状态尚未确定；仅检查已记录端口的 TCP 监听"
        };
        out.push(PortScanEntry {
            service_id: target.service_id,
            label: target.label,
            port: target.port,
            owned_by_self: verdict == "self",
            pid: holder,
            process_name: listener.as_ref().and_then(|l| l.process_name.clone()),
            cmdline: listener.and_then(|l| l.cmdline),
            running: target.running,
            verdict: verdict.into(),
            detail: detail.into(),
            listener_count: pids.len(),
        });
    }
    out.sort_by_key(|row| row.port);
    Ok(out)
}

fn port_verdict(running: bool, owners: &[(u32, Ownership)]) -> (&'static str, Option<u32>) {
    if let Some((pid, _)) = owners.iter().find(|(_, owner)| *owner == Ownership::Other) {
        return ("conflict", Some(*pid));
    }
    if let Some((pid, _)) = owners
        .iter()
        .find(|(_, owner)| *owner == Ownership::Unknown)
    {
        return ("unknown", Some(*pid));
    }
    match owners.first() {
        Some((pid, _)) => ("self", Some(*pid)),
        None if running => ("missing", None),
        _ => ("free", None),
    }
}

/// 结束进程（用户确认后调用）
pub fn kill_pid(pid: u32) -> Result<bool> {
    if pid <= 4 || pid == std::process::id() {
        return Err(AppError::new(
            "PROTECTED_PROCESS",
            "不能结束系统进程或 NiceEnv 自身",
        ));
    }
    #[cfg(windows)]
    {
        let out = platform::command("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
            .map_err(|e| crate::error::AppError::io("结束进程", e))?;
        if !out.status.success() {
            return Err(AppError::new(
                "KILL_FAILED",
                format!("未能结束 PID {pid}，请确认进程状态和权限"),
            )
            .with_detail(platform::decode_command_output(&out.stderr)));
        }
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        let out = platform::command("kill")
            .arg("-9")
            .arg(pid.to_string())
            .output()
            .map_err(|e| crate::error::AppError::io("结束进程", e))?;
        if !out.status.success() {
            return Err(AppError::new(
                "KILL_FAILED",
                format!("未能结束 PID {pid}，请确认进程状态和权限"),
            )
            .with_detail(platform::decode_command_output(&out.stderr)));
        }
        Ok(true)
    }
}

/// 保留监听地址以核实管理台确实属于本机进程，不能把同端口的远端 IP 当作本机。
#[derive(Clone, Debug)]
pub(crate) struct ListenerEndpoint {
    pub port: u16,
    pub pid: u32,
    pub address: Option<std::net::SocketAddr>,
}

impl ListenerEndpoint {
    pub fn probe_address(&self) -> Option<std::net::SocketAddr> {
        let mut address = self.address?;
        if address.ip().is_unspecified() {
            address.set_ip(if address.is_ipv4() { std::net::Ipv4Addr::LOCALHOST.into() } else { std::net::Ipv6Addr::LOCALHOST.into() });
        }
        Some(address)
    }

    pub fn accepts(&self, target: std::net::SocketAddr) -> bool {
        self.address.is_some_and(|address| address.port() == target.port()
            && (address == target || (address.ip().is_unspecified() && target.ip().is_loopback()
                && address.is_ipv4() == target.is_ipv4())))
    }
}

/// 地址无法识别时仍保留端口/PID，避免扫描和结束端口流程将其错误报告为空闲。
fn parse_listener_endpoint(name: &str, pid: &str, ipv6: bool) -> Option<ListenerEndpoint> {
    let (host, port) = name.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let pid = pid.parse().ok()?;
    let address = if host == "*" {
        Some(std::net::SocketAddr::new(if ipv6 { std::net::Ipv6Addr::UNSPECIFIED.into() } else { std::net::Ipv4Addr::UNSPECIFIED.into() }, port))
    } else { name.parse().ok() };
    Some(ListenerEndpoint { port, pid, address })
}

fn platform_listeners() -> Result<Vec<(u16, u32)>> {
    Ok(listener_endpoints()?.into_iter().map(|entry| (entry.port, entry.pid)).collect())
}

pub(crate) fn listener_endpoints() -> Result<Vec<ListenerEndpoint>> {
    let out = if cfg!(windows) {
        platform::command("netstat")
            // -p tcp 在 Windows 会排除 TCPv6；统一读取，再仅解析 TCP LISTENING。
            .args(["-ano"])
            .output()
            .map_err(|e| crate::error::AppError::io("执行 netstat", e))?
    } else {
        platform::command("lsof")
            .args(["-nP", "-iTCP", "-sTCP:LISTEN"])
            .output()
            .map_err(|e| crate::error::AppError::io("执行 lsof", e))?
    };
    check_listener_exit(out.status.code(), &out.stdout, &out.stderr, !cfg!(windows))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut result = Vec::new();

    #[cfg(windows)]
    {
        for line in text.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            // Proto Local-Address Foreign-Address State PID
            if parts.len() >= 5 && parts[0].eq_ignore_ascii_case("TCP") && parts[3].eq_ignore_ascii_case("LISTENING") {
                if let Some(entry) = parse_listener_endpoint(parts[1], parts[4], false) {
                    result.push(entry);
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
                if let Some(entry) = parse_listener_endpoint(parts[8], parts[1], parts[4] == "IPv6") {
                    result.push(entry);
                }
            }
        }
    }
    Ok(result)
}

/// lsof 无匹配时允许 exit 1 且两路输出为空；其它失败不能报告端口空闲。
pub(crate) fn check_listener_exit(
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
    lsof: bool,
) -> Result<()> {
    if (code == Some(0) && stderr.is_empty())
        || (lsof && code == Some(1) && stdout.is_empty() && stderr.is_empty())
    {
        return Ok(());
    }
    Err(crate::AppError::new(
        "PORT_SCAN_FAILED",
        "无法完整读取系统 TCP 监听端口，不能判断端口是否空闲",
    )
    .with_detail(platform::decode_command_output(stderr)))
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
    sys.process(Pid::from_u32(pid)).and_then(|p| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_endpoint_retains_family_scope_and_unrecognized_owners() {
        for (name, ipv6, probe) in [
            ("127.0.0.2:9000", false, "127.0.0.2:9000"),
            ("192.0.2.10:9000", false, "192.0.2.10:9000"),
            ("0.0.0.0:9000", false, "127.0.0.1:9000"),
            ("[::]:9000", false, "[::1]:9000"),
            ("[::1]:9000", false, "[::1]:9000"),
            ("[fe80::1%12]:9000", true, "[fe80::1%12]:9000"),
            ("*:9000", false, "127.0.0.1:9000"),
            ("*:9000", true, "[::1]:9000"),
        ] {
            let entry = parse_listener_endpoint(name, "77", ipv6).unwrap();
            assert_eq!((entry.port, entry.pid), (9000, 77));
            assert_eq!(entry.probe_address().unwrap().to_string(), probe);
            assert!(entry.accepts(probe.parse().unwrap()));
            assert!(!entry.accepts("192.0.2.20:9000".parse().unwrap()));
            assert!(!entry.accepts("127.0.0.1:9001".parse().unwrap()));
            let other_family = if entry.address.unwrap().is_ipv4() { "[::1]:9000" } else { "127.0.0.1:9000" };
            assert!(!entry.accepts(other_family.parse().unwrap()));
        }
        let unknown = parse_listener_endpoint("unknown-host:9000", "88", false).unwrap();
        assert_eq!((unknown.port, unknown.pid), (9000, 88));
        assert!(unknown.probe_address().is_none());
        assert!(!unknown.accepts("127.0.0.1:9000".parse().unwrap()));
        assert!(!parse_listener_endpoint("[fe80::1%12]:9000", "77", true).unwrap()
            .accepts("[fe80::1%13]:9000".parse().unwrap()));
    }

    fn row(pid: u32) -> ListenerInfo {
        ListenerInfo {
            port: 8080,
            pid,
            process_name: Some("fixture".into()),
            cmdline: None,
            owned_by_self: false,
            service_id: None,
            process_started_at: Some(10),
            ownership: "external".into(),
            can_close: true,
            close_reason: None,
        }
    }

    #[test]
    fn selected_listener_does_not_expand_to_other_or_replacement_processes() {
        let selected = row(500);
        let current = vec![selected.clone(), row(600)];
        assert_eq!(
            validate_close_targets(8080, &[selected.clone()], &current)
                .unwrap()
                .iter()
                .map(|r| r.pid)
                .collect::<Vec<_>>(),
            [500]
        );
        assert!(validate_close_targets(8080, &[row(400)], &current)
            .unwrap()
            .is_empty());
        assert!(validate_close_targets(8080, &[], &current)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn changed_identity_ownership_and_protected_processes_cannot_be_closed() {
        let expected = row(500);
        let mut current = expected.clone();
        current.process_started_at = Some(20);
        assert_eq!(
            validate_close_targets(8080, &[expected.clone()], &[current])
                .unwrap_err()
                .code,
            "PORT_TARGET_CHANGED"
        );
        let mut current = expected.clone();
        current.service_id = Some("mysql@8".into());
        assert!(validate_close_targets(8080, &[expected.clone()], &[current]).is_err());
        let mut current = expected.clone();
        current.can_close = false;
        assert_eq!(
            validate_close_targets(8080, &[expected.clone()], &[current])
                .unwrap_err()
                .code,
            "PORT_TARGET_PROTECTED"
        );
        let mut current = expected.clone();
        current.process_name = Some("reused-pid".into());
        assert!(validate_close_targets(8080, &[expected.clone()], &[current]).is_err());
        let mut unknown = expected.clone();
        unknown.process_started_at = None;
        assert!(validate_close_targets(8080, &[unknown.clone()], &[unknown]).is_err());
    }

    #[test]
    fn invalid_ranges_and_duplicated_targets_are_rejected() {
        assert!(validate_range(0, 80).is_err());
        assert_eq!(validate_range(9000, 8000).unwrap(), (8000, 9000));
        assert!(validate_close_targets(8080, &[row(500), row(500)], &[row(500)]).is_err());
        assert!(validate_close_targets(80, &[row(500)], &[row(500)]).is_err());
        assert!(kill_pid(0).is_err());
        assert!(kill_pid(std::process::id()).is_err());
    }

    #[test]
    fn all_listener_owners_determine_verdict() {
        assert_eq!(port_verdict(true, &[]), ("missing", None));
        assert_eq!(port_verdict(false, &[]), ("free", None));
        assert_eq!(
            port_verdict(true, &[(1, Ownership::Own), (2, Ownership::Other)]),
            ("conflict", Some(2))
        );
        assert_eq!(
            port_verdict(true, &[(1, Ownership::Own), (3, Ownership::Unknown)]),
            ("unknown", Some(3))
        );
        assert_eq!(
            port_verdict(true, &[(1, Ownership::Own), (2, Ownership::Own)]),
            ("self", Some(1))
        );
    }

    #[test]
    fn scan_recognizes_managed_children_and_marks_missing_ancestry() {
        let manager = Arc::new(ServiceManager::new());
        manager.register(
            "fixture",
            "Fixture",
            None,
            None,
            None,
            std::path::PathBuf::new(),
        );
        let mut service = manager.snapshot("fixture").unwrap();
        service.pids = vec![900];
        let snapshot = ProcessSnapshot {
            system: sysinfo::System::new(),
            parents: HashMap::from([(901, (900, "child".into())), (800, (0, "other".into()))]),
        };
        let child = snapshot.listener(8080, 901, std::slice::from_ref(&service));
        assert_eq!(child.service_id.as_deref(), Some("fixture"));
        assert_eq!(child.ownership, "self");
        assert!(!child.can_close); // 身份读取失败不能操作。
        assert_eq!(
            snapshot
                .listener(8080, 999, std::slice::from_ref(&service))
                .ownership,
            "unknown"
        );
        assert!(
            !snapshot
                .listener(8080, 999, std::slice::from_ref(&service))
                .can_close
        );
    }

    #[test]
    fn real_listener_is_reported_without_killing_or_claiming_it_was_freed() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let manager = Arc::new(ServiceManager::new());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let v6 = std::net::TcpListener::bind("[::1]:0").unwrap();
        let v6_addr = v6.local_addr().unwrap();
        let endpoints = listener_endpoints().unwrap();
        assert!(endpoints.iter().any(|entry| entry.pid == std::process::id() && entry.address == Some(v6_addr)));
        assert!(endpoints.iter().any(|entry| entry.pid == std::process::id() && entry.address == Some(listener.local_addr().unwrap())));
        assert!(scan_port_range(&manager, v6_addr.port(), v6_addr.port()).unwrap().listeners.iter().any(|entry| entry.pid == std::process::id()));
        manager.register(
            "fixture",
            "Fixture",
            Some("1".into()),
            None,
            Some(port),
            paths.service_log("fixture"),
        );
        manager.adopt("fixture", &[std::process::id()], Some(port));
        let scan = scan_port_range(&manager, port, port).unwrap();
        let ours = scan
            .listeners
            .iter()
            .find(|r| r.pid == std::process::id())
            .unwrap();
        assert_eq!(ours.service_id.as_deref(), Some("fixture"));
        assert!(!ours.can_close);
        assert_eq!(
            scan.listeners
                .iter()
                .filter(|r| r.pid == std::process::id())
                .count(),
            1
        );
        assert!(
            close_port_checked(&store, &paths, &manager, port, std::slice::from_ref(ours)).is_err()
        );
        let outcome = close_port_checked(&store, &paths, &manager, port, &[]).unwrap();
        assert!(!outcome.port_free);
        assert!(outcome.killed_pids.is_empty());
        assert!(outcome
            .remaining
            .iter()
            .any(|r| r.pid == std::process::id()));
        let rows = scan_app_ports(&store, &manager).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].port, port);
        assert_eq!(rows[0].verdict, "self");
        drop(listener);
        assert!(scan_port_range(&manager, port, port)
            .unwrap()
            .listeners
            .iter()
            .all(|r| r.pid != std::process::id()));
    }

    #[test]
    fn broken_port_settings_and_overflow_are_not_hidden() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("state.sqlite")).unwrap();
        let manager = Arc::new(ServiceManager::new());
        store
            .set_setting("portOverride.http", "invalid-port")
            .unwrap();
        assert!(scan_app_ports(&store, &manager).is_err());
        store.set_setting("portOverride.http", "80").unwrap();
        manager.register(
            "php@8",
            "PHP 8",
            Some("8".into()),
            None,
            Some(65535),
            temp.path().join("php.log"),
        );
        manager.adopt("php@8", &[std::process::id()], Some(65535));
        assert!(scan_app_ports(&store, &manager).is_err());
    }

    #[test]
    fn busy_lifecycle_rejects_scan_without_waiting() {
        let manager = Arc::new(ServiceManager::new());
        let _lock = manager.lifecycle.lock();
        let other = manager.clone();
        assert_eq!(
            std::thread::spawn(move || scan_port_range(&other, 80, 80).unwrap_err().code)
                .join()
                .unwrap(),
            "SERVICE_BUSY"
        );
    }
    #[cfg(windows)]
    #[test]
    fn closes_only_a_selected_temporary_listener_process() {
        use std::io::BufRead;
        struct Fixture(std::process::Child, u16);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        fn fixture() -> Fixture {
            // 有限的独立验证进程，仅监听临时 TCP 端口，30 秒后自行退出；不写系统配置。
            let mut child = platform::command("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command",
                "$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0); try { $listener.Start(); [Console]::WriteLine($listener.LocalEndpoint.Port); Start-Sleep -Seconds 30 } finally { $listener.Stop() }"])
                .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().unwrap();
            let stdout = child.stdout.take().unwrap();
            let mut fixture = Fixture(child, 0);
            let mut line = String::new();
            std::io::BufReader::new(stdout)
                .read_line(&mut line)
                .unwrap();
            fixture.1 = line.trim().parse().unwrap();
            fixture
        }
        let first = fixture();
        let mut other = fixture();
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let manager = Arc::new(ServiceManager::new());
        let scan = scan_port_range(&manager, first.1, first.1).unwrap();
        let target = scan
            .listeners
            .iter()
            .find(|row| row.pid == first.0.id())
            .unwrap();
        assert!(target.can_close, "{:?}", target);
        let result = close_port_checked(
            &store,
            &paths,
            &manager,
            first.1,
            std::slice::from_ref(target),
        )
        .unwrap();
        assert!(result.port_free, "{:?}", result);
        assert_eq!(result.killed_pids, [first.0.id()]);
        assert!(result.errors.is_empty());
        assert!(other.0.try_wait().unwrap().is_none());
        assert!(scan_port_range(&manager, other.1, other.1)
            .unwrap()
            .listeners
            .iter()
            .any(|row| row.pid == other.0.id()));
    }
}
