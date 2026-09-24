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

/// 一条证书链的解读结果
pub struct ChainInfo {
    pub issuer: String,
    pub not_after: i64,
    pub not_before: i64,
    /// 链长度（leaf + 中间证书）
    pub len: usize,
}

/// 忽略一切校验的 verifier：监控就要能看到过期/自签证书
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
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}

/// TLS 连一次 host:port，读对端证书链
pub fn fetch_chain(host: &str, port: u16) -> Result<ChainInfo> {
    let host = host.to_string();
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| AppError::internal("构建 tokio runtime", e.to_string()))?;
    rt.block_on(async move {
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| AppError::internal("TLS 配置", e.to_string()))?
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        cfg.alpn_protocols = vec![]; // 不协商应用协议，只要证书
        // 关键：装上 NoVerify —— 不装的话空根存储会让自签/过期证书在握手期就失败，
        // 监控反而读不到「坏证书」（这正是要盯的东西）
        cfg.dangerous().set_certificate_verifier(std::sync::Arc::new(NoVerify));
        let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(cfg));

        let addr = (host.as_str(), port);
        let tcp = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::net::TcpStream::connect(addr),
        )
        .await
        .map_err(|_| AppError::new("MONITOR_TIMEOUT", format!("连接 {host}:{port} 超时")))
        .and_then(|r| r.map_err(|e| AppError::new("MONITOR_CONNECT", format!("连接 {host}:{port} 失败：{e}"))))?;

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
    })
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
    let mut m = state
        .store
        .get_cert_monitor(id)?
        .ok_or_else(|| AppError::new("NOT_FOUND", "监控不存在"))?;
    let prev = m.state.clone();
    match fetch_chain(&m.host, m.port) {
        Ok(info) => {
            m.state = state_of(Some(info.not_after)).into();
            m.issuer = info.issuer;
            m.expires_at = Some(info.not_after);
            m.last_error = String::new();
        }
        Err(e) => {
            m.state = "error".into();
            m.last_error = e.to_string();
        }
    }
    m.last_checked = Some(now_ms());
    m.updated_at = now_ms();
    state.store.save_cert_monitor(&m)?;

    // 状态跃迁告警：首次进入 expiring / expired / error 时推给前端。
    // 稳定态重复检查不刷屏 —— 每小时 tick 不该每小时弹同一条警告。
    let alerting = matches!(m.state.as_str(), "expiring" | "expired" | "error");
    if alerting && prev != m.state {
        let message = if m.state == "expired" {
            format!("证书已过期（剩 {} 天）", fmt_days(m.expires_at))
        } else if m.state == "expiring" {
            format!("证书将到期（剩 {} 天）", fmt_days(m.expires_at))
        } else {
            m.last_error.clone()
        };
        state.emit_event(Event::CertMonitorAlert {
            host: m.host.clone(),
            state: m.state.clone(),
            message: message.clone(),
        });
        // 推送通知（certd 的证书监控告警）：设置里配了通知渠道就转发一份
        let _ = push_alert(state, &m.host, &m.state, &message);
    }
    Ok(m)
}

/// 监控告警推送：读全局设置 monitorNotifyKind / monitorNotifyUrl（钉钉/企微/飞书/通用）。
/// 没配就静默跳过；发送失败只记进度事件，不影响监控状态。
fn push_alert(state: &CoreState, host: &str, mstate: &str, message: &str) -> Result<()> {
    let kind = state
        .store
        .get_setting("monitorNotifyKind")
        .unwrap_or_default();
    let url = state
        .store
        .get_setting("monitorNotifyUrl")
        .unwrap_or_default();
    if kind.is_empty() || kind == "none" || url.trim().is_empty() {
        return Ok(());
    }
    let title = format!("证书监控告警：{host}（{mstate}）");
    crate::certdeploy::notify(&kind, &url, mstate == "ok", &title, message)?;
    Ok(())
}

/// 剩余天数（可为负，表示已过期天数）
fn fmt_days(expires_at: Option<i64>) -> i64 {
    (expires_at.unwrap_or(0) - now_ms()) / 86_400_000
}

/// 全量刷新（调度器每小时顺带执行）
pub fn tick_all(state: &CoreState) {
    let Ok(list) = state.store.list_cert_monitors() else {
        return;
    };
    for m in list {
        let _ = check(state, &m.id);
    }
}

impl CoreState {
    pub fn certmonitor_list(&self) -> Result<Vec<CertMonitor>> {
        self.store.list_cert_monitors()
    }

    pub fn certmonitor_add(&self, mut m: CertMonitor) -> Result<CertMonitor> {
        if m.host.trim().is_empty() {
            return Err(AppError::new("BAD_HOST", "主机不能为空"));
        }
        if m.id.trim().is_empty() {
            m.id = format!("mon-{}", now_ms());
            m.created_at = now_ms();
        }
        m.updated_at = now_ms();
        if m.name.trim().is_empty() {
            m.name = format!("{}:{}", m.host, m.port);
        }
        self.store.save_cert_monitor(&m)?;
        Ok(m)
    }

    pub fn certmonitor_delete(&self, id: &str) -> Result<bool> {
        self.store.delete_cert_monitor(id)?;
        Ok(true)
    }

    pub fn certmonitor_check(&self, id: &str) -> Result<CertMonitor> {
        check(self, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_thresholds_follow_certd_style() {
        let now = now_ms();
        assert_eq!(state_of(Some(now - 1)), "expired");
        assert_eq!(state_of(Some(now + 7 * 86_400_000)), "expiring");
        assert_eq!(state_of(Some(now + 90 * 86_400_000)), "ok");
        assert_eq!(state_of(None), "idle");
    }
}
