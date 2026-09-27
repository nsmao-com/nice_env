//! 网站证书监控（certd 的「站点证书监控」）：
//! 对任意 host:port 发起一次 TLS 握手，读取对端证书链，展示签发者与到期时间。
//!
//! 设计取舍：
//! - **不校验证书链**：监控的对象是「对面挂的证书长什么样、什么时候到期」，
//!   自签 / 过期 / 域名不匹配都要能读出来，这正是监控的价值；
//! - 握手完拿完证书立刻断开，不发任何业务数据；
//! - 结果随自动化调度每小时刷新一次（certauto::spawn_scheduler 顺带调用）；
//! - 状态跃迁为 expiring / expired / error 时发 `certmonitor://alert` 事件，
//!   稳定态重复检查不重复告警。

use crate::error::{AppError, Result};
use crate::model::CertMonitor;
use crate::{CoreState, Event};
use time::OffsetDateTime;

fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// 用户可粘贴 HTTPS URL、域名:端口或 IPv6。只保存 TLS 端点，不发送 URL 路径上的请求。
pub(crate) fn normalize_endpoint(input: &str, default_port: u16) -> Result<(String, u16)> {
    let raw = input.trim();
    let invalid = || AppError::new("BAD_HOST", "请填写域名、IP、host:port 或 HTTPS 地址；IPv6 带端口时使用 [::1]:8443");
    if raw.is_empty() || raw.len() > 2048 || default_port == 0 || raw.contains('\\') || raw.chars().any(char::is_whitespace) { return Err(invalid()); }
    if let Ok(ip) = raw.parse::<std::net::IpAddr>() { return Ok((ip.to_string(), default_port)); }
    let url = reqwest::Url::parse(&if raw.contains("://") { raw.to_string() } else { format!("https://{raw}") }).map_err(|_| invalid())?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() { return Err(invalid()); }
    let host = url.host_str().ok_or_else(invalid)?.trim_matches(['[', ']']).trim_end_matches('.').to_ascii_lowercase();
    rustls::pki_types::ServerName::try_from(host.clone()).map_err(|_| invalid())?;
    let authority = raw.split("://").last().unwrap_or(raw).split(['/', '?', '#']).next().unwrap_or(raw);
    if authority.ends_with(':') { return Err(invalid()); }
    let explicit_port = authority.rsplit_once(':').is_some_and(|(_, value)| !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()));
    let port = if explicit_port || raw.contains("://") { url.port_or_known_default().unwrap_or(default_port) } else { default_port };
    if port == 0 { return Err(invalid()); }
    Ok((host, port))
}

fn endpoint(host: &str, port: u16) -> String {
    if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") }
}

fn execution_lock(state: &CoreState, id: &str) -> Result<std::fs::File> {
    use sha2::{Digest, Sha256};
    let name = hex::encode(Sha256::digest(id.as_bytes()));
    let path = crate::paths::checked_data_path(&state.paths.base, &format!("certmonitor-locks/{name}.lock"))?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => AppError::new("MONITOR_BUSY", "该监控正在检查，请等待结果后重试"),
        std::fs::TryLockError::Error(error) => AppError::io("锁定证书监控", error),
    })?;
    Ok(file)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MonitorNotificationSettings { pub kind: String, pub url: String }

fn notification_settings(store: &crate::store::Store) -> Result<MonitorNotificationSettings> {
    store.read_snapshot(|snapshot| Ok(MonitorNotificationSettings {
        kind: snapshot.get_setting_checked("monitorNotifyKind")?.unwrap_or_else(|| "none".into()),
        url: snapshot.get_setting_checked("monitorNotifyUrl")?.unwrap_or_default(),
    }))
}

fn validate_notifications(settings: &MonitorNotificationSettings) -> Result<()> {
    if !["none", "generic", "dingtalk", "wecom", "feishu"].contains(&settings.kind.as_str()) {
        return Err(AppError::new("BAD_NOTIFY_KIND", "请选择支持的通知方式"));
    }
    if settings.kind == "none" { return Ok(()); }
    let invalid = || AppError::new("BAD_NOTIFY_URL", "请填写完整的 HTTP/HTTPS Webhook 地址，不要包含用户名或密码");
    let url = reqwest::Url::parse(settings.url.trim()).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(invalid());
    }
    Ok(())
}

/// 一条证书链的解读结果
pub struct ChainInfo {
    pub issuer: String,
    pub not_after: i64,
    pub not_before: i64,
    /// 链长度（leaf + 中间证书）
    pub len: usize,
}

/// 跳过信任链、域名和有效期验证，保留握手签名验证，以读取过期/自签证书。
#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &rustls::crypto::ring::default_provider().signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &rustls::crypto::ring::default_provider().signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

/// TLS 连一次 host:port，读对端证书链
pub fn fetch_chain(host: &str, port: u16) -> Result<ChainInfo> {
    fetch_chain_with_timeout(host, port, std::time::Duration::from_secs(20))
}

fn fetch_chain_with_timeout(host: &str, port: u16, timeout: std::time::Duration) -> Result<ChainInfo> {
    let (host, port) = normalize_endpoint(host, port)?;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()
        .map_err(|e| AppError::internal("构建 tokio runtime", e.to_string()))?;
    let result = rt.block_on(async move { tokio::time::timeout(timeout, async move {
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| AppError::internal("TLS 配置", e.to_string()))?
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        cfg.alpn_protocols = vec![]; // 不协商应用协议，只要证书
                                     // 关键：装上 NoVerify —— 不装的话空根存储会让自签/过期证书在握手期就失败，
                                     // 监控反而读不到「坏证书」（这正是要盯的东西）
        cfg.dangerous()
            .set_certificate_verifier(std::sync::Arc::new(NoVerify));
        let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(cfg));

        let addr = (host.as_str(), port);
        let tcp = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::net::TcpStream::connect(addr),
        )
        .await
        .map_err(|_| AppError::new("MONITOR_TIMEOUT", format!("连接 {host}:{port} 超时")))
        .and_then(|r| {
            r.map_err(|e| AppError::new("MONITOR_CONNECT", format!("连接 {host}:{port} 失败：{e}")))
        })?;

        let server_name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|e| AppError::new("MONITOR_SNI", format!("主机名非法：{e}")))?;
        let tls = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            connector.connect(server_name, tcp),
        )
        .await
        .map_err(|_| AppError::new("MONITOR_TIMEOUT", format!("TLS 握手 {host}:{port} 超时")))
        .and_then(|r| r.map_err(|e| AppError::new("MONITOR_TLS", format!("TLS 握手失败：{e}"))))?;

        let certs = tls
            .get_ref()
            .1
            .peer_certificates()
            .map(|c| c.to_vec())
            .unwrap_or_default();
        drop(tls);
        if certs.is_empty() {
            return Err(AppError::new("MONITOR_NO_CERT", "对端没有出示证书"));
        }
        parse_chain(&certs)
    }).await.map_err(|_| AppError::new("MONITOR_TIMEOUT", "证书检查超时，请确认地址、端口和 TLS 服务状态"))? });
    // 系统 DNS 查询可能在线程池中仍未返回；不能让 runtime Drop 把超时变成无限等待。
    rt.shutdown_timeout(std::time::Duration::from_millis(100));
    result
}

/// 解析证书链：leaf 的签发者与有效期
pub fn parse_chain(certs: &[rustls::pki_types::CertificateDer<'static>]) -> Result<ChainInfo> {
    let leaf = certs
        .first()
        .ok_or_else(|| AppError::new("MONITOR_NO_CERT", "空证书链"))?;
    let (_, cert) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|e| AppError::new("MONITOR_PARSE", format!("证书解析失败：{e}")))?;
    let issuer = cert.issuer().to_string();
    let not_before = cert.validity().not_before.timestamp() * 1000;
    let not_after = cert.validity().not_after.timestamp() * 1000;
    Ok(ChainInfo {
        issuer,
        not_after,
        not_before,
        len: certs.len(),
    })
}

fn state_of(expires_at: Option<i64>) -> &'static str {
    let Some(exp) = expires_at else { return "idle" };
    let now = now_ms();
    if exp <= now {
        "expired"
    } else if exp <= now + 30 * 86_400_000 {
        "expiring"
    } else {
        "ok"
    }
}

/// 刷新一条监控（同步阻塞；调度线程调用）
pub fn check(state: &CoreState, id: &str) -> Result<CertMonitor> {
    check_with_probe(state, id, fetch_chain)
}

fn check_with_probe(state: &CoreState, id: &str, probe: impl FnOnce(&str, u16) -> Result<ChainInfo>) -> Result<CertMonitor> {
    check_with_handlers(state, id, probe, push_alert)
}

fn check_with_handlers(state: &CoreState, id: &str, probe: impl FnOnce(&str, u16) -> Result<ChainInfo>,
    push: impl FnOnce(&CoreState, &str, &str, &str) -> Result<()>) -> Result<CertMonitor> {
    let _work = crate::BackgroundWork::begin(format!("证书监控（{id}）"))?;
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _execution = execution_lock(state, id)?;
    let mut m = state
        .store
        .get_cert_monitor(id)?
        .ok_or_else(|| AppError::new("NOT_FOUND", "监控不存在"))?;
    let prev = m.state.clone();
    let retry_notification = !m.notification_error.is_empty();
    let revision = m.updated_at;
    match normalize_endpoint(&m.host, m.port).and_then(|(host, port)| probe(&host, port)) {
        Ok(info) => {
            m.state = state_of(Some(info.not_after)).into();
            m.issuer = info.issuer;
            m.expires_at = Some(info.not_after);
            m.last_error = String::new();
            if info.not_before > now_ms() {
                m.state = "error".into();
                m.last_error = "证书尚未生效，请检查服务器证书与本机时间".into();
            }
        }
        Err(e) => {
            m.state = "error".into();
            m.last_error = e.to_string();
        }
    }
    m.last_checked = Some(now_ms());
    m.updated_at = now_ms().max(revision.saturating_add(1));
    if !state.store.complete_cert_monitor(&m, revision)? {
        return Err(AppError::new("MONITOR_CHANGED", "监控已删除或修改，本次检查结果已丢弃"));
    }

    // 状态跃迁告警：首次进入 expiring / expired / error 时推给前端。
    // 稳定态重复检查不刷屏 —— 每小时 tick 不该每小时弹同一条警告。
    let alerting = matches!(m.state.as_str(), "expiring" | "expired" | "error");
    if alerting && (prev != m.state || retry_notification) {
        let message = if m.state == "expired" {
            format!("证书已过期 {} 天", -fmt_days(m.expires_at))
        } else if m.state == "expiring" {
            format!("证书将到期（剩 {} 天）", fmt_days(m.expires_at))
        } else {
            m.last_error.clone()
        };
        if prev != m.state { state.emit_event(Event::CertMonitorAlert {
            host: endpoint(&m.host, m.port),
            state: m.state.clone(),
            message: message.clone(),
        }); }
        // 推送通知（certd 的证书监控告警）：设置里配了通知渠道就转发一份
        m.notification_error = push(state, &endpoint(&m.host, m.port), &m.state, &message)
            .err().map(|_| "告警推送失败，请检查通知配置；下次检查会重试".to_string()).unwrap_or_default();
    } else if !alerting { m.notification_error.clear(); }
    if retry_notification || !m.notification_error.is_empty() {
        let revision = m.updated_at;
        m.updated_at = now_ms().max(revision.saturating_add(1));
        if !state.store.complete_cert_monitor(&m, revision)? {
            return Err(AppError::new("MONITOR_CHANGED", "监控已删除或修改，未覆盖新记录"));
        }
    }
    Ok(m)
}

/// 监控告警推送：读全局设置 monitorNotifyKind / monitorNotifyUrl（钉钉/企微/飞书/通用）。
/// 没配就静默跳过；失败单独保存原因供下次重试，不改动证书检查状态。
fn push_alert(state: &CoreState, host: &str, mstate: &str, message: &str) -> Result<()> {
    let settings = notification_settings(&state.store)?;
    if settings.kind.is_empty() || settings.kind == "none" {
        return Ok(());
    }
    validate_notifications(&settings)?;
    let title = format!("证书监控告警：{host}（{mstate}）");
    crate::certdeploy::notify(&settings.kind, &settings.url, mstate == "ok", &title, message)?;
    Ok(())
}

/// 剩余天数（可为负，表示已过期天数）
fn fmt_days(expires_at: Option<i64>) -> i64 {
    (expires_at.unwrap_or(0) - now_ms()).div_euclid(86_400_000)
}

/// 全量刷新（调度器每小时顺带执行）
pub fn tick_all(state: &CoreState) {
    let Ok(_work) = crate::BackgroundWork::begin("证书监控调度") else { return; };
    let Ok(list) = state.store.list_cert_monitors() else {
        return;
    };
    for m in list {
        let _ = check(state, &m.id);
    }
}

impl CoreState {
    pub fn certmonitor_notification_get(&self) -> Result<MonitorNotificationSettings> {
        let _activity = crate::paths::DataDirActivity::shared(&self.paths.base)?;
        notification_settings(&self.store)
    }

    pub fn certmonitor_notification_save(&self, mut settings: MonitorNotificationSettings) -> Result<MonitorNotificationSettings> {
        let _work = crate::BackgroundWork::begin("保存证书监控通知")?;
        let _activity = crate::paths::DataDirActivity::shared(&self.paths.base)?;
        settings.url = settings.url.trim().into(); validate_notifications(&settings)?;
        self.store.save_monitor_notifications(&settings.kind, &settings.url)?;
        Ok(settings)
    }
    pub fn certmonitor_list(&self) -> Result<Vec<CertMonitor>> {
        let _activity = crate::paths::DataDirActivity::shared(&self.paths.base)?;
        self.store.list_cert_monitors()
    }

    pub fn certmonitor_add(&self, m: CertMonitor) -> Result<CertMonitor> {
        let _work = crate::BackgroundWork::begin("添加证书监控")?;
        let _activity = crate::paths::DataDirActivity::shared(&self.paths.base)?;
        if !m.id.is_empty() { return Err(AppError::new("BAD_MONITOR_ID", "新增监控不能覆盖已有记录")); }
        let m = prepare_monitor(m)?;
        self.store.create_cert_monitor(&m)?;
        Ok(m)
    }

    pub fn certmonitor_delete(&self, id: &str) -> Result<bool> {
        let _work = crate::BackgroundWork::begin("删除证书监控")?;
        let _activity = crate::paths::DataDirActivity::shared(&self.paths.base)?;
        self.store.delete_cert_monitor(id)?;
        Ok(true)
    }

    pub fn certmonitor_check(&self, id: &str) -> Result<CertMonitor> {
        check(self, id)
    }
}

/// 新增与导入只接受配置，运行状态必须由本机实际检查产生。
pub(crate) fn prepare_monitor(mut m: CertMonitor) -> Result<CertMonitor> {
    (m.host, m.port) = normalize_endpoint(&m.host, m.port)?;
    m.id = format!("mon-{:032x}", rand::random::<u128>());
    m.created_at = now_ms(); m.updated_at = m.created_at;
    m.state = "idle".into(); m.issuer.clear(); m.last_error.clear(); m.notification_error.clear(); m.expires_at = None; m.last_checked = None;
    if m.name.trim().is_empty() { m.name = endpoint(&m.host, m.port); }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, CoreState, std::sync::Arc<parking_lot::Mutex<Vec<(String, String)>>>) {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::new(dir.path().to_path_buf()); paths.ensure_dirs().unwrap();
        let events = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new())); let sink = events.clone();
        let state = CoreState {
            store: crate::store::Store::open(paths.db()).unwrap(), paths,
            manager: std::sync::Arc::new(crate::services::ServiceManager::new()),
            installer: crate::install::Installer::bundled(),
            downloader: std::sync::Arc::new(crate::download::Downloader::new()),
            emit: std::sync::Arc::new(move |event| { if let Event::CertMonitorAlert { host, state, .. } = event { sink.lock().push((host, state)); } }),
        };
        (dir, state, events)
    }

    fn input(host: &str) -> CertMonitor {
        CertMonitor { id: String::new(), name: String::new(), host: host.into(), port: 443, state: "ok".into(),
            issuer: "forged".into(), last_error: "forged".into(), notification_error: "forged".into(),
            expires_at: Some(1), last_checked: Some(1), created_at: 1, updated_at: 1 }
    }

    fn chain(days: i64) -> Result<ChainInfo> {
        Ok(ChainInfo { issuer: "fixture issuer".into(), not_after: now_ms() + days * 86_400_000, not_before: now_ms() - 86_400_000, len: 1 })
    }

    #[test]
    fn endpoints_normalize_urls_ports_ip_and_reject_ambiguous_inputs() {
        for (raw, default, host, port) in [
            ("Example.COM.", 443, "example.com", 443), (" example.com ", 8443, "example.com", 8443),
            ("example.com:443", 8443, "example.com", 443), ("https://EXAMPLE.com./path?q=1", 8443, "example.com", 443),
            ("https://example.com:9443/a", 443, "example.com", 9443), ("127.0.0.1", 8443, "127.0.0.1", 8443),
            ("127.0.0.1:8443", 443, "127.0.0.1", 8443), ("::1", 8443, "::1", 8443),
            ("[::1]", 8443, "::1", 8443), ("[::1]:443", 8443, "::1", 443),
            ("https://[2001:db8::1]:8443/test", 443, "2001:db8::1", 8443),
        ] { assert_eq!(normalize_endpoint(raw, default).unwrap(), (host.into(), port), "{raw}"); }
        for raw in ["", "bad host", "http://example.com", "ftp://example.com", "https://user:pass@example.com", "example.com:0",
            "[::1]:0", "example.com:65536", "example.com:", "https://", "[::1", "https://example.com\\evil", "example.com:-1"] {
            assert!(normalize_endpoint(raw, 443).is_err(), "{raw}");
        }
        assert!(normalize_endpoint("example.com", 0).is_err());
        assert_eq!(endpoint("::1", 8443), "[::1]:8443");
    }

    #[test]
    fn add_owns_identity_runtime_and_deduplicates_canonical_endpoints() {
        let (_dir, state, _) = fixture();
        let first = state.certmonitor_add(input("https://EXAMPLE.com.:443/path")).unwrap();
        assert_eq!(first.host, "example.com"); assert_eq!(first.state, "idle");
        assert!(first.issuer.is_empty() && first.last_error.is_empty() && first.notification_error.is_empty());
        assert!(first.expires_at.is_none() && first.last_checked.is_none());
        assert_eq!(state.certmonitor_add(input("example.com")).unwrap_err().code, "MONITOR_EXISTS");
        assert_eq!(state.certmonitor_add(first.clone()).unwrap_err().code, "BAD_MONITOR_ID");
        let second = state.certmonitor_add(input("example.com:8443")).unwrap(); assert_ne!(first.id, second.id);
        let mut imported = input("https://example.com:9443"); imported.id = "foreign-id".into();
        let prepared = prepare_monitor(imported).unwrap(); assert_ne!(prepared.id, "foreign-id"); assert_eq!(prepared.state, "idle");
    }

    #[test]
    fn corrupt_rows_are_visible_but_do_not_block_reading_another_id() {
        let (_dir, state, _) = fixture(); let m = state.certmonitor_add(input("example.com")).unwrap();
        let db = rusqlite::Connection::open(state.paths.db()).unwrap();
        db.execute("INSERT INTO cert_monitors(id,data,updated_at) VALUES('broken','{',0)", []).unwrap();
        assert_eq!(state.certmonitor_list().unwrap_err().code, "MONITOR_CORRUPT");
        assert_eq!(state.store.get_cert_monitor(&m.id).unwrap().unwrap().id, m.id);
        db.execute("UPDATE cert_monitors SET data=?1 WHERE id='broken'", [serde_json::to_string(&m).unwrap()]).unwrap();
        assert_eq!(state.store.get_cert_monitor("broken").unwrap_err().code, "MONITOR_CORRUPT");
    }

    #[test]
    fn deleted_or_changed_monitors_discard_inflight_results_without_alerts() {
        let (_dir, state, events) = fixture();
        let m = state.certmonitor_add(input("example.com")).unwrap();
        let result = check_with_probe(&state, &m.id, |_, _| { state.certmonitor_delete(&m.id).unwrap(); chain(-1) });
        assert_eq!(result.unwrap_err().code, "MONITOR_CHANGED"); assert!(state.certmonitor_list().unwrap().is_empty());
        let m = state.certmonitor_add(input("example.com")).unwrap();
        let result = check_with_probe(&state, &m.id, |_, _| {
            let mut edited = m.clone(); edited.updated_at += 1; edited.name = "new config".into();
            state.store.save_cert_monitor(&edited).unwrap(); chain(-1)
        });
        assert_eq!(result.unwrap_err().code, "MONITOR_CHANGED");
        let saved = state.store.get_cert_monitor(&m.id).unwrap().unwrap(); assert_eq!(saved.name, "new config"); assert_eq!(saved.state, "idle");
        assert!(events.lock().is_empty());
    }

    #[test]
    fn simultaneous_checks_are_locked_and_recovery_allows_a_new_alert() {
        let (_dir, state, events) = fixture(); let m = state.certmonitor_add(input("[::1]:8443")).unwrap();
        check_with_probe(&state, &m.id, |host, port| {
            assert_eq!((host, port), ("::1", 8443));
            assert_eq!(check_with_probe(&state, &m.id, |_, _| panic!("duplicate probe")).unwrap_err().code, "MONITOR_BUSY");
            chain(-1)
        }).unwrap();
        check_with_probe(&state, &m.id, |_, _| chain(-1)).unwrap(); assert_eq!(events.lock().len(), 1);
        check_with_probe(&state, &m.id, |_, _| chain(90)).unwrap();
        check_with_probe(&state, &m.id, |_, _| chain(-1)).unwrap(); assert_eq!(events.lock().len(), 2);
        assert_eq!(events.lock()[0].0, "[::1]:8443");
    }

    #[test]
    fn failed_probe_preserves_history_and_future_cert_is_not_healthy() {
        let (_dir, state, _) = fixture(); let m = state.certmonitor_add(input("example.com")).unwrap();
        let healthy = check_with_probe(&state, &m.id, |_, _| chain(90)).unwrap();
        let failed = check_with_probe(&state, &m.id, |_, _| Err(AppError::new("MONITOR_TLS", "fixture handshake failed"))).unwrap();
        assert_eq!(failed.state, "error"); assert_eq!(failed.expires_at, healthy.expires_at); assert_eq!(failed.issuer, healthy.issuer);
        assert!(failed.last_error.contains("handshake failed")); assert!(failed.updated_at > healthy.updated_at);
        let future = check_with_probe(&state, &m.id, |_, _| { let mut c = chain(90)?; c.not_before = now_ms() + 86_400_000; Ok(c) }).unwrap();
        assert_eq!(future.state, "error"); assert!(future.last_error.contains("尚未生效"));
        assert!(fmt_days(Some(now_ms() - 1)) < 0);
    }

    #[test]
    fn notification_settings_are_atomic_and_failed_pushes_retry_without_duplicate_toasts() {
        let (_dir, state, events) = fixture();
        assert_eq!(state.certmonitor_notification_get().unwrap().kind, "none");
        let settings = MonitorNotificationSettings { kind: "generic".into(), url: " http://127.0.0.1:9/?token=private-fixture ".into() };
        let saved = state.certmonitor_notification_save(settings).unwrap(); assert_eq!(state.certmonitor_notification_get().unwrap().url, saved.url);
        let db = rusqlite::Connection::open(state.paths.db()).unwrap();
        db.execute_batch("CREATE TRIGGER reject_notify_url BEFORE UPDATE ON settings WHEN NEW.key='monitorNotifyUrl' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(state.certmonitor_notification_save(MonitorNotificationSettings { kind: "wecom".into(), url: "https://example.com/hook".into() }).is_err());
        assert_eq!(state.certmonitor_notification_get().unwrap().kind, "generic"); assert_eq!(state.certmonitor_notification_get().unwrap().url, saved.url);
        db.execute_batch("DROP TRIGGER reject_notify_url;").unwrap();
        state.store.save_monitor_notifications("invalid", "https://example.com/?token=private-fixture").unwrap();
        let m = state.certmonitor_add(input("example.com")).unwrap();
        let failed = check_with_probe(&state, &m.id, |_, _| chain(-1)).unwrap();
        assert!(!failed.notification_error.is_empty()); assert!(!failed.notification_error.contains("private-fixture"));
        assert!(!state.store.get_cert_monitor(&m.id).unwrap().unwrap().notification_error.is_empty());
        state.certmonitor_notification_save(MonitorNotificationSettings { kind: "none".into(), url: String::new() }).unwrap();
        let retried = check_with_probe(&state, &m.id, |_, _| chain(-1)).unwrap(); assert!(retried.notification_error.is_empty());
        assert!(state.store.get_cert_monitor(&m.id).unwrap().unwrap().notification_error.is_empty()); assert_eq!(events.lock().len(), 1);
        for url in ["", "file:///tmp/hook", "https://user:pass@example.com", "https://example.com/#secret"] {
            assert!(state.certmonitor_notification_save(MonitorNotificationSettings { kind: "generic".into(), url: url.into() }).is_err());
        }
    }

    #[test]
    fn successful_notification_retry_clears_error_and_late_notification_cannot_restore_deleted_row() {
        let (_dir, state, events) = fixture(); let m = state.certmonitor_add(input("example.com")).unwrap();
        let first = check_with_handlers(&state, &m.id, |_, _| chain(-1), |_, host, status, _| {
            assert_eq!(host, "example.com:443"); assert_eq!(status, "expired"); Err(AppError::new("FIXTURE", "private-token"))
        }).unwrap();
        assert!(!first.notification_error.is_empty()); assert!(!first.notification_error.contains("private-token"));
        let mut pushed = false;
        let retry = check_with_handlers(&state, &m.id, |_, _| chain(-1), |_, _, _, _| { pushed = true; Ok(()) }).unwrap();
        assert!(pushed); assert!(retry.notification_error.is_empty());
        assert!(state.store.get_cert_monitor(&m.id).unwrap().unwrap().notification_error.is_empty()); assert_eq!(events.lock().len(), 1);
        // 先记录一次推送失败，随后在重试发送期间删除；第二次结果保存同样不能复活记录。
        check_with_handlers(&state, &m.id, |_, _| chain(10), |_, _, _, _| Err(AppError::new("FIXTURE", "retry"))).unwrap();
        let result = check_with_handlers(&state, &m.id, |_, _| chain(10), |state, _, _, _| { state.certmonitor_delete(&m.id)?; Ok(()) });
        assert_eq!(result.unwrap_err().code, "MONITOR_CHANGED"); assert!(state.certmonitor_list().unwrap().is_empty());
    }

    #[test]
    fn loopback_tls_reads_self_signed_expired_and_future_certs_without_business_data() {
        use std::{sync::Arc, time::Duration};
        for days in [-2, 90] { for future in [false, true] {
            let key = rcgen::KeyPair::generate().unwrap(); let mut params = rcgen::CertificateParams::new(vec!["fixture.invalid".into()]).unwrap();
            params.not_before = OffsetDateTime::now_utc() + time::Duration::days(if future { 1 } else { -30 });
            params.not_after = OffsetDateTime::now_utc() + time::Duration::days(days);
            let cert = params.self_signed(&key).unwrap(); let expected = parse_chain(&[cert.der().clone()]).unwrap();
            let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions().unwrap().with_no_client_auth()
                .with_single_cert(vec![cert.der().clone()], rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into()).unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let port = listener.local_addr().unwrap().port(); listener.set_nonblocking(true).unwrap();
            let worker = std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async move {
                    use tokio::io::AsyncReadExt;
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept()).await.unwrap().unwrap();
                    let mut tls = tokio::time::timeout(Duration::from_secs(3), tokio_rustls::TlsAcceptor::from(Arc::new(cfg)).accept(stream)).await.unwrap().unwrap();
                    let mut data = [0; 32]; let read = tokio::time::timeout(Duration::from_secs(3), tls.read(&mut data)).await.unwrap();
                    assert!(matches!(read, Ok(0)) || read.is_err(), "monitor sent application data");
                });
            });
            let actual = fetch_chain("127.0.0.1", port); worker.join().unwrap(); let actual = actual.unwrap();
            assert_eq!(actual.not_after, expected.not_after); assert_eq!(actual.not_before, expected.not_before); assert_eq!(actual.len, 1);
        } }
    }

    #[test]
    fn stalled_tls_handshake_obeys_total_deadline() {
        use std::time::{Duration, Instant};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); let port = listener.local_addr().unwrap().port(); listener.set_nonblocking(true).unwrap();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop { match listener.accept() {
                Ok((_stream, _)) => { std::thread::sleep(Duration::from_millis(200)); return; },
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => { assert!(Instant::now() < deadline); std::thread::sleep(Duration::from_millis(2)); },
                Err(e) => panic!("{e}"),
            } }
        });
        let started = Instant::now(); let result = fetch_chain_with_timeout("127.0.0.1", port, Duration::from_millis(75));
        assert_eq!(result.err().unwrap().code, "MONITOR_TIMEOUT"); assert!(started.elapsed() < Duration::from_secs(1)); worker.join().unwrap();
    }

    #[test]
    fn state_thresholds_follow_certd_style() {
        let now = now_ms();
        assert_eq!(state_of(Some(now - 1)), "expired");
        assert_eq!(state_of(Some(now + 7 * 86_400_000)), "expiring");
        assert_eq!(state_of(Some(now + 90 * 86_400_000)), "ok");
        assert_eq!(state_of(None), "idle");
    }
}
