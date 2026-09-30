//! 环境体检：报告已发现的问题与实际完成的检查范围。
//! 不修改系统配置，不把采集失败、未检查或用户隐藏的提示视为正常。

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use crate::error::{AppError, Result};
use crate::model::{InstalledPackage, ServiceState, ServiceStatus, Site, SiteKind};
use crate::paths::Paths;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckItem {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
}

/// checked 仅表示完成检查；是否有问题由 items 给出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckState {
    Checked,
    Unavailable,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckCoverage {
    pub id: String,
    pub label: String,
    pub state: CheckState,
    pub detail: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub items: Vec<CheckItem>,
    pub checks: Vec<CheckCoverage>,
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    pub summary: String,
    /// Unix 秒；报告是此时的快照，不是持续监控。
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

    fn issue(
        &mut self,
        id: &str,
        severity: Severity,
        title: impl Into<String>,
        detail: impl Into<String>,
        action: &str,
        route: &str,
    ) {
        self.push(CheckItem {
            id: id.into(),
            severity,
            title: title.into(),
            detail: detail.into(),
            action: Some(action.into()),
            route: Some(route.into()),
        });
    }

    fn coverage(&mut self, id: &str, label: &str, state: CheckState, detail: impl Into<String>) {
        self.checks.push(CheckCoverage {
            id: id.into(),
            label: label.into(),
            state,
            detail: detail.into(),
        });
    }

    fn unavailable(&mut self, id: &str, label: &str, error: AppError, route: &str) {
        self.issue(
            &format!("{id}-read-failed"),
            Severity::Warn,
            format!("未完成{label}检查"),
            &error.message,
            error
                .hint
                .as_deref()
                .unwrap_or("检查相关设置与读取权限后重新体检"),
            route,
        );
        self.coverage(id, label, CheckState::Unavailable, error.message);
    }

    fn finish(&mut self, empty: bool) {
        self.items.sort_by_key(|item| match item.severity {
            Severity::Error => 0,
            Severity::Warn => 1,
            Severity::Info => 2,
        });
        let incomplete = self
            .checks
            .iter()
            .filter(|check| check.state == CheckState::Unavailable)
            .count();
        self.summary = if self.errors > 0 {
            format!(
                "发现 {} 个需要处理的问题{}",
                self.errors,
                if incomplete > 0 {
                    format!("，{incomplete} 项检查未完成")
                } else {
                    String::new()
                }
            )
        } else if incomplete > 0 {
            format!("{incomplete} 项检查未完成，请查看原因")
        } else if self.warnings > 0 {
            format!("{} 项建议处理", self.warnings)
        } else if empty {
            "尚未配置环境".into()
        } else {
            "已检查范围内未发现问题".into()
        };
        self.checked_at = chrono::Utc::now().timestamp();
    }
}

pub fn check(
    paths: &Paths,
    store: &crate::store::Store,
    manager: &std::sync::Arc<crate::services::ServiceManager>,
) -> Result<HealthReport> {
    // 启停或切换期间不采集混合状态；重复体检也不排队阻塞服务操作。
    let _operation = manager
        .lifecycle
        .try_lock()
        .ok_or_else(|| AppError::new("HEALTH_BUSY", "正在体检或调整服务，请稍后重新检查"))?;
    let installed = store.list_installed()?;
    let sites = store.list_sites()?;
    let services = manager.list_status();
    let mut r = HealthReport::default();
    if installed.is_empty() {
        r.issue(
            "no-packages",
            Severity::Info,
            "还没有安装任何套件",
            "先安装所需的 Web 服务器与运行时，再创建站点",
            "到「套件 / 服务」选择套件",
            "/packages",
        );
    }
    for package in &installed {
        if !Path::new(&package.install_path).is_dir() {
            r.issue(
                &format!("install-missing-{}-{}", package.id, package.version),
                Severity::Error,
                format!("{} {} 安装目录缺失或无法访问", package.id, package.version),
                &package.install_path,
                "检查目录权限或重新安装该版本",
                "/packages",
            );
        }
    }
    r.coverage(
        "packages",
        "安装记录与目录",
        CheckState::Checked,
        format!(
            "{} 个安装记录；检查目录存在性，不验证全部文件完整性",
            installed.len()
        ),
    );

    check_ports(&mut r, store, &services);
    check_sites(&mut r, &sites, &installed);
    match crate::certs::report(paths, store) {
        Ok(certs) => check_certs(&mut r, paths, &sites, &certs),
        Err(error) => r.unavailable("certificates", "证书", error, "/tls"),
    }
    let hosts = crate::hosts::managed_entries(store).and_then(|wanted| {
        let actual: HashSet<_> = crate::hosts::read_all()?
            .into_iter()
            .filter(|entry| entry.managed)
            .map(|entry| (entry.ip, entry.domain))
            .collect();
        let wanted: HashSet<_> = wanted.into_iter().collect();
        Ok((
            wanted.difference(&actual).count(),
            actual.difference(&wanted).count(),
        ))
    });
    match hosts {
        Ok((missing, unexpected)) => {
            if missing > 0 || unexpected > 0 {
                r.issue(
                    "hosts-drift",
                    Severity::Warn,
                    "hosts 托管记录与预期不一致",
                    format!(
                        "缺少 {missing} 条映射，多出 {unexpected} 条映射，请核对域名与 IP 地址"
                    ),
                    "到「网络与域名」核对映射，或在「诊断与修复」重建 hosts",
                    "/network",
                );
            }
            r.coverage(
                "hosts",
                "hosts 托管映射",
                CheckState::Checked,
                "只比对托管记录；未测试系统 DNS 解析或浏览器访问",
            );
        }
        Err(error) => r.unavailable("hosts", "hosts 托管映射", error, "/network"),
    }
    check_services(&mut r, &services, &installed);
    match probe_data_dir(&paths.base) {
        Ok(()) => r.coverage(
            "data-directory",
            "数据目录可写性",
            CheckState::Checked,
            "已创建、写入并清理唯一临时文件；未检查所有子目录",
        ),
        Err(error) => {
            r.issue(
                "data-dir-readonly",
                Severity::Error,
                "数据目录写入或清理失败",
                error.to_string(),
                "检查磁盘空间与目录权限",
                "/settings",
            );
            r.coverage(
                "data-directory",
                "数据目录可写性",
                CheckState::Checked,
                "写入探测发现问题，详见上方提示",
            );
        }
    }
    let php: Vec<_> = installed
        .iter()
        .filter(|package| package.id == "php")
        .collect();
    if php.is_empty() {
        r.coverage(
            "php-extensions",
            "PHP 扩展",
            CheckState::Skipped,
            "未安装 PHP，无需检查扩展",
        );
    }
    for package in php {
        let id = format!("php-extensions-{}", package.version);
        let label = format!("PHP {} 扩展", package.version);
        let root = paths.runtime_dir("php", &package.version);
        let scan = if !root.join(crate::ops::exe_name("php")).is_file()
            || !paths.php_ini(&package.version).is_file()
        {
            Err(AppError::new(
                "PHP_SCAN_UNAVAILABLE",
                "PHP 可执行文件或 php.ini 缺失，无法完整检查扩展",
            )
            .with_hint("修复当前 PHP 安装后重试"))
        } else {
            crate::phpext::scan_available(paths, &package.version)
        };
        match scan {
            Ok(extensions) => {
                check_extensions(&mut r, &package.version, &root.join("ext"), &extensions);
                r.coverage(&id, &label, CheckState::Checked, "已检查内置模块、启用配置、扩展文件和已知依赖；未验证扩展在业务进程中的实际加载");
            }
            Err(error) => r.unavailable(&id, &label, error, "/packages"),
        }
    }
    r.coverage("application-probes", "配置语法与业务连通性", CheckState::Skipped,
        "本次未运行原生配置校验，也未测试 HTTP、数据库登录或 UDP；配置问题可到「工具箱 → 修复向导」检查");
    r.finish(installed.is_empty());
    Ok(r)
}

fn probe_data_dir(base: &Path) -> std::io::Result<()> {
    let mut file = tempfile::Builder::new()
        .prefix(".health-probe-")
        .tempfile_in(base)?;
    let write = file
        .write_all(b"NiceEnv health probe")
        .and_then(|()| file.flush());
    // 即使写入失败也显式清理；不能覆盖用户已有的 .health-probe 文件。
    let close = file.close();
    match (write, close) {
        (Err(write), Err(close)) => Err(std::io::Error::new(
            write.kind(),
            format!("{write}；清理探针失败：{close}"),
        )),
        (Err(error), _) | (_, Err(error)) => Err(error),
        _ => Ok(()),
    }
}

fn check_sites(r: &mut HealthReport, sites: &[Site], installed: &[InstalledPackage]) {
    for site in sites {
        let mut reasons = Vec::new();
        if !Path::new(&site.root_dir).is_absolute() || !Path::new(&site.root_dir).is_dir() {
            reasons.push(format!(
                "根目录不存在、无法访问或不是绝对路径：{}",
                site.root_dir
            ));
        }
        let web = site.runtime.web_server.as_str();
        if !matches!(web, "nginx" | "apache" | "caddy") {
            reasons.push(format!("不支持的 Web 服务器：{web}"));
        } else if !installed.iter().any(|package| package.id == web) {
            reasons.push(format!("未安装 {web}"));
        }
        if site.domains.is_empty() {
            reasons.push("未配置域名".into());
        }
        if site.runtime.kind == SiteKind::Php {
            match site
                .runtime
                .php_version
                .as_deref()
                .filter(|version| !version.is_empty())
            {
                Some(version)
                    if installed
                        .iter()
                        .any(|package| package.id == "php" && crate::install::same_version(&package.version, version)) => {}
                Some(version) => reasons.push(format!("未安装指定 PHP {version}")),
                None => reasons.push("未指定 PHP 版本".into()),
            }
        } else if site.runtime.kind != SiteKind::Static {
            if let Err(error) =
                crate::sites::proxy_url(site.runtime.proxy_target.as_deref().unwrap_or_default())
            {
                reasons.push(error.message);
            }
        }
        if !reasons.is_empty() {
            r.issue(
                &format!("broken-site-{}", site.id),
                Severity::Error,
                format!("站点「{}」配置有问题", site.name),
                reasons.join("；"),
                "到「站点」修正配置，或安装所需套件",
                "/sites",
            );
        }
    }
    r.coverage(
        "sites",
        "站点依赖与路径",
        if sites.is_empty() {
            CheckState::Skipped
        } else {
            CheckState::Checked
        },
        format!(
            "{} 个站点；检查 Web 服务器、PHP 版本、根目录与代理地址格式，不请求站点",
            sites.len()
        ),
    );
}

#[derive(Debug)]
pub(crate) struct PortTarget {
    pub(crate) service_id: String,
    pub(crate) label: String,
    pub(crate) port: u16,
    pub(crate) running: bool,
    pub(crate) pids: Vec<u32>,
}

pub(crate) fn port_targets(
    store: &crate::store::Store,
    services: &[ServiceStatus],
) -> Result<Vec<PortTarget>> {
    let profile = crate::services::PortsProfile::from_settings_checked(store)?;
    let mut targets = Vec::new();
    for service in services {
        let running = service.state == ServiceState::Running;
        let active = !service.pids.is_empty();
        let package = service.id.split('@').next().unwrap_or(&service.id);
        // 运行服务优先取实际启动端口；停止服务才使用当前方案或历史分配。
        let port = if active {
            service.port
        } else {
            match package {
                "nginx" => Some(profile.http),
                "apache" => Some(profile.apache_http),
                "mysql" => Some(profile.mysql),
                "redis" => Some(profile.redis),
                "postgresql" => Some(profile.postgres),
                "mongodb" => Some(profile.mongodb),
                "php" => store.get_port_assign_checked(&service.id)?,
                _ => store.get_port_assign_checked(&service.id)?.or(service.port),
            }
        };
        if let Some(base) = port {
            let count = if package == "php" {
                crate::configgen::PHP_POOL_WORKERS
            } else {
                1
            };
            for offset in 0..count {
                let port = base
                    .checked_add(offset)
                    .filter(|port| *port > 0)
                    .ok_or_else(|| {
                        AppError::new("BAD_PORT", format!("服务 {} 的端口范围无效", service.id))
                    })?;
                targets.push(PortTarget {
                    service_id: service.id.clone(),
                    label: service.label.clone(),
                    port,
                    running,
                    pids: service.pids.clone(),
                });
            }
        }
    }
    Ok(targets)
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ownership {
    Own,
    Other,
    Unknown,
}

pub(crate) fn ownership(mut pid: u32, roots: &[u32], parents: &HashMap<u32, (u32, String)>) -> Ownership {
    let mut visited = HashSet::new();
    for _ in 0..128 {
        if pid != 0 && roots.contains(&pid) {
            return Ownership::Own;
        }
        if pid == 0 {
            return Ownership::Other;
        }
        if !visited.insert(pid) {
            return Ownership::Unknown;
        }
        match parents.get(&pid) {
            Some((parent, _)) => pid = *parent,
            None => return Ownership::Unknown,
        }
    }
    Ownership::Unknown
}

fn analyze_ports(
    r: &mut HealthReport,
    targets: &[PortTarget],
    listeners: &[(u16, u32)],
    parents: &HashMap<u32, (u32, String)>,
) -> bool {
    let mut complete = true;
    for target in targets {
        let owners: HashSet<_> = listeners
            .iter()
            .filter(|(port, _)| *port == target.port)
            .map(|(_, pid)| *pid)
            .collect();
        if target.running && owners.is_empty() {
            r.issue(
                &format!("port-not-listening-{}-{}", target.service_id, target.port),
                Severity::Error,
                format!("{} 的端口 {} 未监听", target.label, target.port),
                "服务显示运行中，但未发现该 TCP 监听；进程可能刚退出或配置已改变",
                "到「套件 / 服务」检查状态和日志后重试",
                "/packages",
            );
        }
        let mut external = Vec::new();
        let mut unknown = Vec::new();
        for pid in owners {
            match ownership(pid, &target.pids, parents) {
                Ownership::Own => {}
                Ownership::Other => external.push(format!(
                    "{}（PID {pid}）",
                    parents
                        .get(&pid)
                        .map(|(_, name)| name.as_str())
                        .filter(|name| !name.is_empty())
                        .unwrap_or("其他进程")
                )),
                Ownership::Unknown => unknown.push(pid.to_string()),
            }
        }
        external.sort();
        unknown.sort();
        if !external.is_empty() {
            r.issue(
                &format!("port-conflict-{}-{}", target.service_id, target.port),
                if target.running {
                    Severity::Error
                } else {
                    Severity::Warn
                },
                format!("{} 的端口 {} 被其他进程占用", target.label, target.port),
                format!(
                    "{}。{}",
                    external.join("、"),
                    if target.running {
                        "监听者不属于该服务的受管进程树"
                    } else {
                        "这是停止服务的计划端口；启动时可能需要切换端口"
                    }
                ),
                "到「工具箱 → 端口」确认占用者，或修改端口设置",
                "/tools",
            );
        }
        if !unknown.is_empty() {
            complete = false;
            r.issue(
                &format!("port-owner-unknown-{}-{}", target.service_id, target.port),
                Severity::Warn,
                format!("无法确认 {} 端口 {} 的归属", target.label, target.port),
                format!(
                    "PID {} 的进程信息不完整，可能受权限限制或进程已退出",
                    unknown.join("、")
                ),
                "稍后重新检查；必要时核对进程权限",
                "/tools",
            );
        }
    }
    complete
}

pub(crate) fn check_ports(r: &mut HealthReport, store: &crate::store::Store, services: &[ServiceStatus]) {
    let scan = (|| {
        let targets = port_targets(store, services)?;
        if targets.is_empty() {
            return Ok((targets, Vec::new(), HashMap::new()));
        }
        let listeners = crate::ports::listeners()?;
        let mut system = sysinfo::System::new();
        system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
        let parents = system
            .processes()
            .iter()
            .map(|(pid, process)| {
                (
                    pid.as_u32(),
                    (
                        process.parent().map(|pid| pid.as_u32()).unwrap_or(0),
                        process.name().to_string_lossy().into_owned(),
                    ),
                )
            })
            .collect();
        Ok((targets, listeners, parents))
    })();
    match scan {
        Ok((targets, listeners, parents)) => {
            let complete = analyze_ports(r, &targets, &listeners, &parents);
            r.coverage("ports", "TCP 端口与进程归属", if targets.is_empty() { CheckState::Skipped } else if complete { CheckState::Checked } else { CheckState::Unavailable },
                format!("检查 {} 个已注册服务主端口及 PHP 池端口；使用实际运行端口或停止服务计划端口。未覆盖 HTTPS 等附加监听和 UDP。\n检查目标：{}", targets.len(),
                    targets.iter().map(|target| format!("{} :{}", target.service_id, target.port)).collect::<Vec<_>>().join("、")));
        }
        Err(error) => r.unavailable("ports", "TCP 端口与进程归属", error, "/tools"),
    }
}

fn check_services(
    r: &mut HealthReport,
    services: &[ServiceStatus],
    installed: &[InstalledPackage],
) {
    let mut incomplete = false;
    for service in services {
        if service.state == ServiceState::Error {
            r.issue(
                &format!("service-error-{}", service.id),
                Severity::Error,
                format!("{} 处于错误状态", service.label),
                service
                    .last_error
                    .as_ref()
                    .map(|error| error.message.as_str())
                    .unwrap_or("没有可用的错误详情，请查看日志"),
                "到「日志」查看具体报错",
                "/logs",
            );
        } else if matches!(
            service.state,
            ServiceState::Unknown | ServiceState::Starting | ServiceState::Stopping
        ) {
            incomplete = true;
            r.issue(
                &format!("service-pending-{}", service.id),
                Severity::Warn,
                format!("{} 状态尚未确定", service.label),
                "服务状态未知或正在切换，本次无法判断其稳定状态",
                "等待服务操作结束后重新检查",
                "/packages",
            );
        }
        let missing: Vec<_> = service
            .requires
            .iter()
            .filter(|dependency| !installed.iter().any(|package| crate::install::installed_package_satisfies(package, dependency)))
            .cloned()
            .collect();
        if !missing.is_empty() {
            r.issue(
                &format!("service-deps-{}", service.id),
                Severity::Error,
                format!("{} 缺少依赖", service.label),
                missing.join("、"),
                "安装所需套件后重试",
                "/packages",
            );
        }
    }
    for package in installed.iter().filter(|package| {
        matches!(
            package.id.as_str(),
            "nginx" | "apache" | "php" | "mysql" | "redis" | "postgresql" | "mongodb" | "mihomo"
        )
    }) {
        let id = if matches!(package.id.as_str(), "php" | "mysql") {
            format!("{}@{}", package.id, package.version)
        } else {
            package.id.clone()
        };
        if !services.iter().any(|service| service.id == id) {
            incomplete = true;
            if !r
                .items
                .iter()
                .any(|item| item.id == format!("service-unregistered-{id}"))
            {
                r.issue(
                    &format!("service-unregistered-{id}"),
                    Severity::Warn,
                    format!("服务 {id} 尚未注册"),
                    "安装记录存在，但没有服务状态和端口快照",
                    "检查安装状态并重新打开应用",
                    "/packages",
                );
            }
        }
    }
    r.coverage(
        "services",
        "受管服务状态",
        if incomplete {
            CheckState::Unavailable
        } else {
            CheckState::Checked
        },
        format!(
            "{} 个已注册服务；已停止不视为故障，运行状态不代表业务请求成功",
            services.len()
        ),
    );
}

fn check_certs(
    r: &mut HealthReport,
    _paths: &Paths,
    sites: &[Site],
    report: &crate::certs::CertReport,
) {
    let local_https = sites
        .iter()
        .any(|site| site.https && site.runtime.uses_default_certificate()
            && !report.certs.iter().any(|cert| cert.kind == "acme"
                && site.domains.first().is_some_and(|d| d.eq_ignore_ascii_case(&cert.subject))));
    let mut count = 0;
    for cert in &report.certs {
        count += 1;
        let issue = if !cert.file_present {
            Some((Severity::Error, "证书或私钥文件缺失"))
        } else {
            match cert.status.as_str() {
                "invalid" => Some((Severity::Error, "证书无效")),
                "expired" => Some((Severity::Error, "证书已过期")),
                "critical" => Some((Severity::Warn, "证书 7 天内到期")),
                "warn" => Some((Severity::Warn, "证书 30 天内到期")),
                _ => None,
            }
        };
        if let Some((severity, title)) = issue {
            r.issue(
                &format!("certificate-{}", cert.id),
                severity,
                format!("{title}：{}", cert.subject),
                &cert.advice,
                "到「证书与域名」检查并修复这张证书",
                "/tls",
            );
        }
    }
    for site in sites.iter().filter(|site| site.https) {
        let found =
            report
                .certs
                .iter()
                .any(|cert| {
                    if let Some(id) = &site.runtime.acme_cert_id {
                        return cert.kind == "acme" && cert.id == *id;
                    }
                    match site.runtime.imported_cert_id.as_deref() {
                        Some(id) => cert.kind == "imported" && cert.id == id,
                        None => {
                            cert.kind != "ca"
                                && cert.kind != "imported"
                                && site.domains.first().is_some_and(|domain| {
                                    cert.id.eq_ignore_ascii_case(&format!("cert-{domain}"))
                                        || cert.subject.eq_ignore_ascii_case(domain)
                                })
                        }
                    }
                });
        if !found {
            r.issue(
                &format!("site-cert-missing-{}", site.id),
                Severity::Error,
                format!("站点「{}」缺少关联证书", site.name),
                "HTTPS 已开启，但没有找到所选证书或本地签发记录",
                "检查站点证书选择，或重新签发证书",
                "/sites",
            );
        }
    }
    if local_https && !report.ca_trusted {
        r.issue(
            "ca-untrusted",
            Severity::Warn,
            "尚未确认本地根 CA 受系统信任",
            "使用本地签发证书的 HTTPS 站点可能出现浏览器信任提示；导入证书不使用此结论",
            "到「证书与域名」检查根证书信任状态",
            "/tls",
        );
    }
    r.coverage(
        "certificates",
        "证书文件与有效性",
        if count == 0 && !local_https && !sites.iter().any(|site| site.https) {
            CheckState::Skipped
        } else {
            CheckState::Checked
        },
        format!("检查 {count} 张证书的文件、有效期、密钥匹配及站点域名覆盖；未执行浏览器 TLS 握手"),
    );
}

fn check_extensions(
    r: &mut HealthReport,
    version: &str,
    ext_dir: &Path,
    extensions: &[crate::model::PhpExtension],
) {
    for extension in extensions
        .iter()
        .filter(|extension| extension.enabled && !extension.builtin)
    {
        let mut reasons = Vec::new();
        if !ext_dir.join(&extension.dll).is_file() {
            reasons.push(format!("扩展文件缺失：{}", extension.dll));
        }
        if !extension.missing_deps.is_empty() {
            reasons.push(format!("缺少依赖：{}", extension.missing_deps.join("、")));
        }
        if !extension.loaded {
            reasons.push("PHP 实测未加载此模块".into());
        }
        if !reasons.is_empty() {
            r.issue(
                &format!("php-ext-{version}-{}", extension.name),
                Severity::Warn,
                format!("PHP {version} 的 {} 无法完整加载", extension.label),
                reasons.join("；"),
                "到 PHP 扩展面板补齐依赖或关闭失效项",
                "/packages",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fixture() -> (
        tempfile::TempDir,
        Paths,
        crate::store::Store,
        Arc<crate::services::ServiceManager>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        let store = crate::store::Store::open(paths.db()).unwrap();
        (
            temp,
            paths,
            store,
            Arc::new(crate::services::ServiceManager::new()),
        )
    }

    fn service(id: &str, port: u16, pids: &[u32], state: &str) -> ServiceStatus {
        serde_json::from_value(serde_json::json!({
            "id": id, "label": id, "state": state, "pids": pids, "port": port,
            "version": "1", "requires": [], "missingRequires": []
        }))
        .unwrap()
    }

    fn target(running: bool) -> PortTarget {
        PortTarget {
            service_id: "nginx".into(),
            label: "Nginx".into(),
            port: 8080,
            running,
            pids: vec![10],
        }
    }

    fn site(root: &Path) -> Site {
        serde_json::from_value(serde_json::json!({
            "id": "site-a", "name": "site-a", "domains": ["app.test"], "rootDir": root,
            "runtime": { "kind": "php", "webServer": "apache", "phpVersion": "8.3.1" },
            "https": false, "rewrite": "none", "db": null, "createdAt": 1, "updatedAt": 1
        }))
        .unwrap()
    }

    #[test]
    fn report_counts_by_severity_and_prioritizes_errors_over_empty() {
        let mut r = HealthReport::default();
        r.issue("i", Severity::Info, "info", "", "", "/packages");
        r.issue("w", Severity::Warn, "warning", "", "", "/packages");
        r.issue("e", Severity::Error, "error", "", "", "/packages");
        r.coverage("a", "unavailable", CheckState::Unavailable, "failed");
        r.finish(true);
        assert_eq!((r.errors, r.warnings, r.infos), (1, 1, 1));
        assert_eq!(r.items[0].severity, Severity::Error);
        assert!(r.summary.contains("需要处理") && r.summary.contains("未完成"));
    }

    #[test]
    fn summary_distinguishes_unavailable_empty_and_checked_scope() {
        let mut r = HealthReport::default();
        r.finish(true);
        assert_eq!(r.summary, "尚未配置环境");
        r.finish(false);
        assert_eq!(r.summary, "已检查范围内未发现问题");
        r.unavailable("ports", "端口", AppError::new("READ", "读取失败"), "/tools");
        r.finish(true);
        assert!(r.summary.contains("未完成"));
    }

    #[test]
    fn report_serializes_coverage_and_timestamp() {
        let mut r = HealthReport::default();
        r.coverage("ports", "端口", CheckState::Unavailable, "读取失败");
        r.finish(false);
        let value = serde_json::to_value(&r).unwrap();
        assert_eq!(value["checks"][0]["state"], "unavailable");
        assert!(value["checkedAt"].as_i64().unwrap() > 0);
        assert_eq!(serde_json::to_string(&Severity::Warn).unwrap(), "\"warn\"");
    }

    #[test]
    fn check_runs_on_empty_env_without_reporting_uninitialized_ca_as_broken() {
        let (_temp, paths, store, manager) = fixture();
        let r = check(&paths, &store, &manager).unwrap();
        assert!(r.items.iter().any(|item| item.id == "no-packages"));
        assert!(!r.items.iter().any(|item| item.id == "certificate-ca"));
        assert!(
            r.checks
                .iter()
                .any(|check| check.id == "certificates" && check.state == CheckState::Skipped)
        );
    }

    #[test]
    fn data_dir_probe_preserves_existing_file_and_cleans_unique_file() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".health-probe"), "user data").unwrap();
        probe_data_dir(temp.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(temp.path().join(".health-probe")).unwrap(),
            "user data"
        );
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
        assert!(probe_data_dir(&temp.path().join("missing")).is_err());
    }

    #[test]
    fn same_named_external_process_is_not_owned_and_children_are_owned() {
        let parents = HashMap::from([
            (10, (0, "nginx".into())),
            (11, (10, "nginx".into())),
            (20, (0, "nginx".into())),
        ]);
        assert_eq!(ownership(11, &[10], &parents), Ownership::Own);
        assert_eq!(ownership(20, &[10], &parents), Ownership::Other);
        let mut r = HealthReport::default();
        assert!(analyze_ports(
            &mut r,
            &[target(true)],
            &[(8080, 11), (8080, 20), (8080, 20)],
            &parents
        ));
        assert_eq!(r.errors, 1);
        assert!(r.items[0].detail.contains("PID 20"));
        assert!(!r.items[0].detail.contains("PID 11"));
    }

    #[test]
    fn another_managed_service_does_not_own_this_service_port() {
        let parents = HashMap::from([(30, (0, "mysql".into()))]);
        let mut r = HealthReport::default();
        analyze_ports(&mut r, &[target(false)], &[(8080, 30)], &parents);
        assert_eq!((r.errors, r.warnings), (0, 1));
        assert!(r.items[0].detail.contains("停止服务"));
    }

    #[test]
    fn incomplete_process_tree_cannot_be_called_healthy() {
        let parents = HashMap::from([(40, (41, "nginx".into())), (41, (40, "nginx".into()))]);
        assert_eq!(ownership(40, &[10], &parents), Ownership::Unknown);
        let mut r = HealthReport::default();
        assert!(!analyze_ports(
            &mut r,
            &[target(true)],
            &[(8080, 99)],
            &parents
        ));
        assert_eq!(r.warnings, 1);
        assert!(r.items[0].id.starts_with("port-owner-unknown"));
    }

    #[test]
    fn running_without_listener_is_error_but_stopped_without_listener_is_expected() {
        let mut r = HealthReport::default();
        analyze_ports(&mut r, &[target(true)], &[], &HashMap::new());
        assert_eq!(r.errors, 1);
        let mut stopped = HealthReport::default();
        analyze_ports(&mut stopped, &[target(false)], &[], &HashMap::new());
        assert!(stopped.items.is_empty());
    }

    #[test]
    fn port_targets_use_actual_running_version_and_configured_stopped_ports() {
        let (_temp, _paths, store, _manager) = fixture();
        store.set_port_override("mysql", Some(3309)).unwrap();
        let targets = port_targets(
            &store,
            &[
                service("mysql@8.4", 3407, &[10], "running"),
                service("mysql@8.0", 3306, &[], "stopped"),
            ],
        )
        .unwrap();
        assert_eq!(
            (targets[0].service_id.as_str(), targets[0].port),
            ("mysql@8.4", 3407)
        );
        assert_eq!(
            (targets[1].service_id.as_str(), targets[1].port),
            ("mysql@8.0", 3309)
        );
    }

    #[test]
    fn invalid_port_settings_are_reported_as_unavailable() {
        let (_temp, paths, store, manager) = fixture();
        store.set_setting("portOverride.http", "0").unwrap();
        let r = check(&paths, &store, &manager).unwrap();
        assert!(r.items.iter().any(|item| item.id == "ports-read-failed"));
        assert!(r.summary.contains("未完成"));
        store.set_setting("portOverride.http", "70000").unwrap();
        assert!(port_targets(&store, &[]).is_err());
    }

    #[test]
    fn php_pool_range_overflow_is_rejected() {
        let (_temp, _paths, store, _manager) = fixture();
        store.set_port_assign("php@8.3", 65535).unwrap();
        assert!(port_targets(&store, &[service("php@8.3", 9100, &[], "stopped")]).is_err());
    }

    #[test]
    fn database_read_failure_does_not_become_empty_environment() {
        let (_temp, paths, store, manager) = fixture();
        rusqlite::Connection::open(paths.db())
            .unwrap()
            .execute("DROP TABLE installed", [])
            .unwrap();
        assert!(check(&paths, &store, &manager).is_err());
    }

    #[test]
    fn damaged_site_runtime_is_not_replaced_with_static_site() {
        let (_temp, paths, store, manager) = fixture();
        store.save_site(&site(&paths.base)).unwrap();
        rusqlite::Connection::open(paths.db())
            .unwrap()
            .execute("UPDATE sites SET runtime='not-json'", [])
            .unwrap();
        assert!(check(&paths, &store, &manager).is_err());
    }

    #[test]
    fn site_reports_missing_root_web_server_and_exact_php_version_together() {
        let temp = tempfile::tempdir().unwrap();
        let mut r = HealthReport::default();
        check_sites(&mut r, &[site(&temp.path().join("missing"))], &[]);
        let detail = &r.items[0].detail;
        assert!(
            detail.contains("根目录") && detail.contains("apache") && detail.contains("PHP 8.3.1")
        );
    }

    #[test]
    fn invalid_expired_and_expiring_certificates_are_all_reported_separately() {
        let (_temp, paths, _store, _manager) = fixture();
        let cert = |id: &str, status: &str| crate::certs::CertHealth {
            id: id.into(),
            kind: "imported".into(),
            subject: id.into(),
            sans: vec![],
            not_after: 1,
            days_left: 1,
            status: status.into(),
            file_present: true,
            used_by_sites: vec![],
            missing_sans: vec![],
            advice: status.into(),
        };
        let report = crate::certs::CertReport {
            certs: vec![
                cert("invalid", "invalid"),
                cert("expired", "expired"),
                cert("soon", "critical"),
            ],
            expired: 1,
            critical: 2,
            warning: 0,
            ca_trusted: false,
            checked_at: 0,
        };
        let mut r = HealthReport::default();
        check_certs(&mut r, &paths, &[], &report);
        assert_eq!((r.errors, r.warnings), (2, 1));
        assert!(r.items.iter().any(|item| item.title.contains("证书无效")));
        assert_eq!(
            r.items
                .iter()
                .filter(|item| item.title.contains("7 天内到期"))
                .count(),
            1
        );
        let mut imported = site(&paths.base);
        imported.https = true;
        imported.runtime.imported_cert_id = Some("soon".into());
        let mut r = HealthReport::default();
        check_certs(&mut r, &paths, &[imported], &report);
        assert!(!r.items.iter().any(|item| item.id == "ca-untrusted"));
        let mut selected = site(&paths.base);
        selected.https = true; selected.runtime.acme_cert_id = Some("acme-example.com".into());
        let mut report = report;
        report.certs = vec![crate::certs::CertHealth { id: "acme-example.com".into(), kind: "acme".into(),
            subject: "example.com".into(), ..cert("acme-example.com", "ok") }];
        let mut r = HealthReport::default();
        check_certs(&mut r, &paths, std::slice::from_ref(&selected), &report);
        assert!(!r.items.iter().any(|item| item.id == "ca-untrusted" || item.id.starts_with("site-cert-missing-")));
        report.certs.clear(); let mut r = HealthReport::default();
        check_certs(&mut r, &paths, &[selected], &report);
        assert!(r.items.iter().any(|item| item.id.starts_with("site-cert-missing-")));
    }

    #[test]
    fn certificate_read_failure_keeps_other_checks_and_marks_incomplete() {
        let (_temp, paths, store, manager) = fixture();
        rusqlite::Connection::open(paths.db())
            .unwrap()
            .execute("DROP TABLE certs", [])
            .unwrap();
        let r = check(&paths, &store, &manager).unwrap();
        assert!(
            r.items
                .iter()
                .any(|item| item.id == "certificates-read-failed")
        );
        assert!(r.checks.iter().any(|check| check.id == "data-directory"));
    }

    #[test]
    fn missing_local_certificate_is_not_masked_by_duplicate_site_name() {
        let (_temp, paths, _store, _manager) = fixture();
        let mut site = site(&paths.base);
        site.https = true;
        let report = crate::certs::CertReport {
            certs: vec![crate::certs::CertHealth {
                id: "cert-other.test".into(),
                kind: "site".into(),
                subject: "other.test".into(),
                sans: vec!["other.test".into()],
                not_after: 1,
                days_left: 90,
                status: "ok".into(),
                file_present: true,
                used_by_sites: vec![site.name.clone()],
                missing_sans: vec![],
                advice: String::new(),
            }],
            expired: 0,
            critical: 0,
            warning: 0,
            ca_trusted: true,
            checked_at: 0,
        };
        let mut r = HealthReport::default();
        check_certs(&mut r, &paths, &[site], &report);
        assert!(
            r.items
                .iter()
                .any(|item| item.id == "site-cert-missing-site-a")
        );
    }

    #[test]
    fn php_missing_enabled_library_and_dependencies_are_visible() {
        let temp = tempfile::tempdir().unwrap();
        let ext = crate::model::PhpExtension {
            name: "pdo_mysql".into(),
            label: "PDO MySQL".into(),
            group: "db".into(),
            hint: String::new(),
            enabled: true,
            loaded: false,
            zend: false,
            builtin: false,
            dll: "missing.dll".into(),
            missing_deps: vec!["pdo".into()],
        };
        let mut r = HealthReport::default();
        check_extensions(&mut r, "8.3", temp.path(), &[ext]);
        assert_eq!(r.warnings, 1);
        assert!(r.items[0].detail.contains("文件缺失")
            && r.items[0].detail.contains("pdo")
            && r.items[0].detail.contains("未加载"));
    }

    #[test]
    fn missing_php_binary_is_an_incomplete_scan() {
        let (_temp, paths, store, manager) = fixture();
        store
            .upsert_installed(&InstalledPackage {
                id: "php".into(),
                version: "8.3".into(),
                category: "runtime".into(),
                install_path: paths.base.to_string_lossy().into_owned(),
                config_path: String::new(),
                installed_at: 1,
            })
            .unwrap();
        let r = check(&paths, &store, &manager).unwrap();
        assert!(
            r.items
                .iter()
                .any(|item| item.id == "php-extensions-8.3-read-failed")
        );
    }

    #[test]
    fn php_probe_execution_failure_is_not_an_empty_extension_list() {
        let (_temp, paths, store, manager) = fixture();
        let root = paths.runtime_dir("php", "8.3");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(paths.etc_dir("php", "8.3")).unwrap();
        // 非可执行文件：验证执行错误路径，不启动实际 PHP 或长期服务。
        std::fs::write(root.join(crate::ops::exe_name("php")), "invalid executable").unwrap();
        std::fs::write(paths.php_ini("8.3"), "extension=pdo_mysql").unwrap();
        store
            .upsert_installed(&InstalledPackage {
                id: "php".into(),
                version: "8.3".into(),
                category: "runtime".into(),
                install_path: root.to_string_lossy().into_owned(),
                config_path: String::new(),
                installed_at: 1,
            })
            .unwrap();
        let r = check(&paths, &store, &manager).unwrap();
        assert!(r.checks.iter().any(
            |check| check.id == "php-extensions-8.3" && check.state == CheckState::Unavailable
        ));
        assert!(r.summary.contains("未完成"));
    }

    #[test]
    fn lifecycle_busy_returns_retryable_error_without_waiting() {
        let (_temp, paths, store, manager) = fixture();
        let lock = manager.lifecycle.lock();
        let other = Arc::clone(&manager);
        let error = std::thread::spawn(move || check(&paths, &store, &other).unwrap_err())
            .join()
            .unwrap();
        assert_eq!(error.code, "HEALTH_BUSY");
        drop(lock);
    }
}
