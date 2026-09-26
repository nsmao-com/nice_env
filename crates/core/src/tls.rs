//! 本地 PKI：根 CA + 站点证书（rcgen 签发）+ 信任导入。

use crate::error::{AppError, Result};
use crate::model::CertRecord;
use crate::paths::{write_with_backup, Paths};
use crate::store::Store;
use time::OffsetDateTime;

pub const CA_DAYS: i64 = 3650;
pub const SITE_DAYS: i64 = 30;
const RENEW_BEFORE_DAYS: i64 = 7;
pub(crate) static CERT_FILES: parking_lot::ReentrantMutex<()> = parking_lot::ReentrantMutex::new(());

/// 签发入口同时用于站点和证书面板，不能依赖前端或站点表单替它校验文件名。
pub(crate) fn normalize_domains(domains: &[String]) -> Result<Vec<String>> {
    if domains.is_empty() || domains.len() > 100 {
        return Err(AppError::new("BAD_DOMAINS", "请填写 1 至 100 个域名或 IP 地址"));
    }
    let mut normalized = Vec::new();
    for input in domains {
        let domain = input.trim().trim_end_matches('.').to_ascii_lowercase();
        let domain = if let Ok(ip) = domain.parse::<std::net::IpAddr>() {
            ip.to_string()
        } else {
            let host = domain.strip_prefix("*.").unwrap_or(&domain);
            if host.len() > 253 || (host != "localhost" && !host.contains('.'))
                || host.split('.').any(|label| label.is_empty() || label.len() > 63
                    || label.starts_with('-') || label.ends_with('-')
                    || !label.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
                || (domain.starts_with("*.") && (host == "localhost" || host.parse::<std::net::IpAddr>().is_ok()))
                || host.bytes().all(|c| c.is_ascii_digit() || c == b'.')
            {
                return Err(AppError::new("BAD_DOMAINS", format!("域名或 IP 地址格式不正确：{input}"))
                    .with_hint("填写域名、*.example.test 或 IP 地址，不要包含协议、端口或路径。"));
            }
            domain
        };
        if !normalized.contains(&domain) { normalized.push(domain); }
    }
    Ok(normalized)
}

fn cert_stem(primary: &str) -> String {
    primary.replace('*', "_wildcard").replace(':', "_")
}

/// 两份文件与记录一起提交；写入/入库失败时恢复旧文件，避免留下不匹配的证书和私钥。
fn write_cert_pair(
    paths: &Paths, cert_path: &std::path::Path, key_path: &std::path::Path,
    cert_pem: &str, key_pem: &str, commit: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let mut previous = Vec::new();
    for path in [cert_path, key_path] {
        let content = match std::fs::read_to_string(path) {
            Ok(content) => Some(content),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(AppError::io("读取原证书文件", error)),
        };
        previous.push((path, content));
    }
    let mut written = 0;
    let result = (|| {
        for (path, content) in [(cert_path, cert_pem), (key_path, key_pem)] {
            write_with_backup(path, content, &paths.backup())?;
            written += 1;
        }
        commit()
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        for (path, content) in previous[..written].iter().rev() {
            let restored = match content {
                Some(content) => write_with_backup(path, content, &paths.backup()),
                None => std::fs::remove_file(path),
            };
            if let Err(restore_error) = restored { failures.push(restore_error.to_string()); }
        }
        if !failures.is_empty() {
            return Err(AppError::new("CERT_ROLLBACK_FAILED", "证书保存失败，部分文件未能恢复")
                .with_hint("请先从备份恢复匹配的证书和私钥，再重新签发。")
                .with_detail(format!("{}；{}", error.message, failures.join("；"))));
        }
        return Err(error);
    }
    Ok(())
}

fn load_ca(paths: &Paths) -> Result<(rcgen::KeyPair, rcgen::CertificateParams)> {
    let key_pem = std::fs::read_to_string(paths.certs().join("ca.key"))?;
    let cert_pem = std::fs::read_to_string(paths.certs().join("ca.crt"))?;
    let key = rcgen::KeyPair::from_pem(&key_pem)
        .map_err(|e| AppError::internal("解析 CA 私钥", e.to_string()))?;
    let (_, pem) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| AppError::internal("解析 CA 证书", e.to_string()))?;
    let cert = pem.parse_x509().map_err(|e| AppError::internal("解析 CA 证书", e.to_string()))?;
    if !cert.is_ca() || cert.public_key().raw != key.public_key_der() {
        return Err(AppError::new("CA_KEY_MISMATCH", "根 CA 证书与私钥不匹配，或该证书不是 CA")
            .with_hint("请恢复原来匹配的 ca.crt 与 ca.key；不会自动覆盖现有 CA。"));
    }
    if !cert.validity().is_valid() {
        return Err(AppError::new("CA_EXPIRED", "根 CA 已过期或尚未生效，无法签发新证书"));
    }
    let params = rcgen::CertificateParams::from_ca_cert_pem(&cert_pem)
        .map_err(|e| AppError::internal("解析 CA 参数", e.to_string()))?;
    Ok((key, params))
}

fn now_minus(days: i64) -> OffsetDateTime {
    OffsetDateTime::now_utc() - time::Duration::days(days)
}
fn now_plus(days: i64) -> OffsetDateTime {
    OffsetDateTime::now_utc() + time::Duration::days(days)
}
fn to_ms(t: OffsetDateTime) -> i64 {
    (t.unix_timestamp_nanos() / 1_000_000) as i64
}

/// 确保 CA 存在（不存在则生成并入库）
pub fn ensure_ca(paths: &Paths) -> Result<()> {
    let _files = CERT_FILES.lock();
    let ca_key = paths.certs().join("ca.key");
    let ca_crt = paths.certs().join("ca.crt");
    if ca_key.exists() && ca_crt.exists() {
        load_ca(paths)?;
        return Ok(());
    }
    if ca_key.exists() || ca_crt.exists() {
        return Err(AppError::new("CA_INCOMPLETE", "根 CA 文件不完整，已保留现有文件")
            .with_hint("请从备份恢复匹配的 ca.crt 与 ca.key。自动创建新 CA 会使原站点证书失去信任。"));
    }
    let sites_dir = paths.certs().join("sites");
    if sites_dir.exists() {
        for entry in std::fs::read_dir(&sites_dir)? {
            if entry?.path().extension().is_some_and(|ext| ext == "crt" || ext == "key") {
                return Err(AppError::new("CA_MISSING", "根 CA 文件丢失，但仍有本地站点证书")
                    .with_hint("请从备份恢复原来的 ca.crt 与 ca.key，避免自动更换根 CA 导致原证书失去信任。"));
            }
        }
    }
    let subject = "NiceEnv Local Root CA";
    let key_pair = rcgen::KeyPair::generate()
        .map_err(|e| AppError::internal("生成 CA 密钥", e.to_string()))?;
    let mut params = rcgen::CertificateParams::new(vec![])
        .map_err(|e| AppError::internal("CA 参数", e.to_string()))?;
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, subject);
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "NiceEnv");
    params.not_before = now_minus(1);
    params.not_after = now_plus(CA_DAYS);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let certified = params
        .self_signed(&key_pair)
        .map_err(|e| AppError::internal("签发 CA", e.to_string()))?;

    std::fs::create_dir_all(paths.certs())?;
    write_cert_pair(paths, &ca_crt, &ca_key, &certified.pem(), &key_pair.serialize_pem(), || Ok(()))
}

/// 站点证书签发（SAN 支持多域名），写入 certs/sites/{primary}.crt/.key
pub fn issue_site_cert(paths: &Paths, store: &Store, domains: &[String]) -> Result<CertRecord> {
    let domains = normalize_domains(domains)?;
    let _files = CERT_FILES.lock();
    ensure_ca(paths)?;
    let primary = &domains[0];
    let (ca_key, ca_params) = load_ca(paths)?;
    // 从持久化的 CA 证书提取参数，并用 CA 密钥重建等价 issuer（同 key/同 subject，
    // signed_by 只用到 issuer 的 DN + 密钥，因此链式校验与磁盘上的 CA 一致）
    let ca_not_after = ca_params.not_after;
    let ca_cert = ca_params
        .self_signed(&ca_key)
        .map_err(|e| AppError::internal("重建 CA issuer", e.to_string()))?;

    let key_pair = rcgen::KeyPair::generate()
        .map_err(|e| AppError::internal("生成站点密钥", e.to_string()))?;
    let mut params = rcgen::CertificateParams::new(domains.to_vec())
        .map_err(|e| AppError::internal("站点证书参数", e.to_string()))?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, primary.as_str());
    let not_before = now_minus(1);
    let not_after = now_plus(SITE_DAYS).min(ca_not_after);
    params.not_before = not_before;
    params.not_after = not_after;
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let certified = params
        .signed_by(&key_pair, &ca_cert, &ca_key)
        .map_err(|e| AppError::internal("签发站点证书", e.to_string()))?;

    // 通配符域名带 `*`，Windows 文件名不允许 —— 落盘名做净化（SAN 里保留原样）
    let file_stem = cert_stem(primary);
    let crt_path = crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{file_stem}.crt"))?;
    let key_path = crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{file_stem}.key"))?;
    std::fs::create_dir_all(paths.certs().join("sites"))?;

    let record = CertRecord {
        id: format!("cert-{primary}"),
        kind: "site".into(),
        subject: primary.clone(),
        sans: domains.to_vec(),
        not_before: to_ms(not_before),
        not_after: to_ms(not_after),
        cert_path: crt_path.to_string_lossy().to_string(),
        key_path: Some(key_path.to_string_lossy().to_string()),
        trusted: None,
    };
    write_cert_pair(paths, &crt_path, &key_path, &certified.pem(), &key_pair.serialize_pem(), || store.save_cert(&record))?;
    Ok(record)
}

/// 信任根 CA：Windows certutil（需管理员） / macOS security
pub fn trust_ca(paths: &Paths) -> Result<()> {
    let _files = CERT_FILES.lock();
    ensure_ca(paths)?;
    let ca = paths.certs().join("ca.crt");
    #[cfg(windows)]
    {
        // 先直接尝试（若已提权则成功）；失败则走 UAC 提权
        if let Ok(o) = platform::command("certutil")
            .args(["-addstore", "-f", "Root"])
            .arg(&ca)
            .output()
        {
            if o.status.success() && ca_trusted(paths) {
                return Ok(());
            }
        }
        platform::run_elevated(
            "certutil",
            &["-addstore", "-f", "Root", &ca.to_string_lossy()],
        )
        .map_err(AppError::from)?;
        if ca_trusted(paths) { Ok(()) }
        else { Err(AppError::new("CA_NOT_TRUSTED", "导入命令已结束，但尚未确认当前根 CA 受信任，请重试")) }
    }
    #[cfg(not(windows))]
    {
        platform::run_elevated(
            "security",
            &[
                "add-trusted-cert",
                "-d",
                "-r",
                "trustRoot",
                "-k",
                "/Library/Keychains/System.keychain",
                &ca.to_string_lossy(),
            ],
        )
        .map_err(AppError::from)?;
        if ca_trusted(paths) { Ok(()) }
        else { Err(AppError::new("CA_NOT_TRUSTED", "尚未确认当前根 CA 受信任，请检查钥匙串中的信任设置")) }
    }
}

/// 按磁盘上这张 CA 的指纹查询，重装后的同名旧 CA 不能冒充当前证书已受信任。
fn ca_fingerprint(paths: &Paths) -> Result<String> {
    use sha1::{Digest, Sha1};
    let cert_pem = std::fs::read(paths.certs().join("ca.crt"))?;
    let (_, pem) = x509_parser::pem::parse_x509_pem(&cert_pem)
        .map_err(|e| AppError::internal("解析 CA 指纹", e.to_string()))?;
    pem.parse_x509().map_err(|e| AppError::internal("解析 CA 证书", e.to_string()))?;
    Ok(hex::encode(Sha1::digest(&pem.contents)))
}

pub fn ca_trusted(paths: &Paths) -> bool {
    let _files = CERT_FILES.lock();
    let Ok(fingerprint) = ca_fingerprint(paths) else { return false };
    #[cfg(windows)]
    {
        for user_store in [true, false] {
            let mut command = platform::command("certutil");
            if user_store { command.arg("-user"); }
            if command.args(["-store", "Root", &fingerprint]).output()
                .is_ok_and(|out| out.status.success()) { return true; }
        }
        false
    }
    #[cfg(not(windows))]
    {
        let _ = fingerprint;
        // 校验指定文件的完整信任链；只看钥匙串里的 CN 会误认同名旧 CA。
        platform::command("security").args(["verify-cert", "-p", "ssl", "-L", "-R", "offline", "-c"])
            .arg(paths.certs().join("ca.crt")).output().is_ok_and(|out| out.status.success())
    }
}

/// 证书列表（CA 置顶并附带信任状态）
pub fn list_certs(paths: &Paths, store: &Store) -> Result<Vec<CertRecord>> {
    let _files = CERT_FILES.lock();
    ensure_ca(paths)?;
    let mut list = store.list_certs()?;
    let pem_bytes = std::fs::read(paths.certs().join("ca.crt"))?;
    let (_, pem) = x509_parser::pem::parse_x509_pem(&pem_bytes)
        .map_err(|e| AppError::internal("读取 CA 有效期", e.to_string()))?;
    let cert = pem.parse_x509().map_err(|e| AppError::internal("读取 CA 有效期", e.to_string()))?;
    let subject = cert.subject().iter_common_name().next().and_then(|cn| cn.as_str().ok())
        .unwrap_or("NiceEnv Local Root CA").to_string();
    let rec = CertRecord {
        id: "ca".into(), kind: "ca".into(), subject, sans: vec![],
        not_before: cert.validity().not_before.timestamp() * 1000,
        not_after: cert.validity().not_after.timestamp() * 1000,
        cert_path: paths.certs().join("ca.crt").to_string_lossy().to_string(),
        key_path: Some(paths.certs().join("ca.key").to_string_lossy().to_string()),
        trusted: Some(ca_trusted(paths)),
    };
    store.save_cert(&rec)?;
    list.retain(|c| c.kind != "ca");
    list.insert(0, rec);
    Ok(list)
}

/// 只更新缺失、损坏、域名覆盖不完整或 7 天内到期的 HTTPS 站点证书。
/// 本地证书有效期为 30 天，不能把新签的证书也判成“30 天内到期”反复替换。
pub fn reissue_missing_site_certs(paths: &Paths, store: &Store) -> Result<Vec<String>> {
    let _files = CERT_FILES.lock();
    ensure_ca(paths)?;
    let ca_pem = std::fs::read(paths.certs().join("ca.crt"))?;
    let (_, ca) = x509_parser::pem::parse_x509_pem(&ca_pem)
        .map_err(|e| AppError::internal("解析根 CA", e.to_string()))?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls::pki_types::CertificateDer::from(ca.contents))
        .map_err(|e| AppError::internal("读取根 CA", e.to_string()))?;
    let soon = to_ms(now_plus(RENEW_BEFORE_DAYS));
    let mut issued = Vec::new();
    for site in store.list_sites()? {
        if !site.https || site.runtime.imported_cert_id.is_some() || site.domains.is_empty() { continue; }
        let domains = normalize_domains(&site.domains)?;
        let primary = &domains[0];
        let existing = store.list_certs()?.into_iter().find(|c| c.kind == "site" && c.subject == *primary);
        let intact = existing.as_ref().is_some_and(|c| {
            let Ok(cert_pem) = std::fs::read(&c.cert_path) else { return false };
            let Ok(key_pem) = std::fs::read_to_string(c.key_path.as_deref().unwrap_or("")) else { return false };
            let Ok((_, pem)) = x509_parser::pem::parse_x509_pem(&cert_pem) else { return false };
            let Ok(cert) = pem.parse_x509() else { return false };
            let Ok(key) = rcgen::KeyPair::from_pem(&key_pem) else { return false };
            cert.validity().is_valid() && cert.validity().not_after.timestamp() * 1000 > soon
                && cert.public_key().raw == key.public_key_der()
                && signed_by_current_ca(&pem.contents, &roots)
                && cert.subject_alternative_name().ok().flatten().is_some_and(|san| {
                    domains.iter().all(|domain| san.value.general_names.iter().any(|name| match name {
                        x509_parser::extensions::GeneralName::DNSName(name) => name.eq_ignore_ascii_case(domain),
                        x509_parser::extensions::GeneralName::IPAddress(bytes) => match domain.parse::<std::net::IpAddr>() {
                            Ok(std::net::IpAddr::V4(ip)) => *bytes == ip.octets(),
                            Ok(std::net::IpAddr::V6(ip)) => *bytes == ip.octets(),
                            Err(_) => false,
                        },
                        _ => false,
                    }))
                })
        });
        if !intact {
            issue_site_cert(paths, store, &domains)?;
            issued.push(primary.clone());
        }
    }
    Ok(issued)
}

fn signed_by_current_ca(der: &[u8], roots: &rustls::RootCertStore) -> bool {
    let der = rustls::pki_types::CertificateDer::from(der);
    let Ok(cert) = rustls::server::ParsedCertificate::try_from(&der) else { return false };
    rustls::client::verify_server_cert_signed_by_trust_anchor(
        &cert, roots, &[], rustls::pki_types::UnixTime::now(),
        rustls::crypto::ring::default_provider().signature_verification_algorithms.all,
    ).is_ok()
}

#[cfg(test)]
mod local_certificate_tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, crate::CoreState) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let state = crate::CoreState {
            store: Store::open(paths.db()).unwrap(), paths,
            manager: std::sync::Arc::new(crate::services::ServiceManager::new()),
            installer: crate::install::Installer::bundled(),
            downloader: std::sync::Arc::new(crate::download::Downloader::new()),
            emit: std::sync::Arc::new(|_| {}),
            watchdog: std::sync::Arc::new(crate::watchdog::Watchdog::new()),
        };
        (temp, state)
    }

    #[test]
    fn invalid_domains_cannot_write_files_and_sans_are_unique() {
        let (_temp, state) = fixture();
        for value in ["", "../../outside", "a.test/../../escape", "https://a.test", "a.test:443", "a..test", "x\\a.test", "*.127.0.0.1", "*.localhost", "bad_thing.test", "999.0.0.1"] {
            assert_eq!(issue_site_cert(&state.paths, &state.store, &[value.into()]).unwrap_err().code, "BAD_DOMAINS");
        }
        assert!(!state.paths.certs().join("ca.key").exists());
        let cert = state.issue_certificate(" App.Test. ", &["app.test".into(), "WWW.APP.TEST".into(), "127.0.0.1".into(), "::1".into()]).unwrap();
        assert_eq!(cert.sans, ["app.test", "www.app.test", "127.0.0.1", "::1"]);
        let pem = std::fs::read(&cert.cert_path).unwrap();
        let (_, pem) = x509_parser::pem::parse_x509_pem(&pem).unwrap();
        let parsed = pem.parse_x509().unwrap();
        assert_eq!(parsed.subject_alternative_name().unwrap().unwrap().value.general_names.len(), 4);
    }

    #[test]
    fn ca_missing_half_or_mismatched_key_is_preserved() {
        let (_temp, state) = fixture();
        ensure_ca(&state.paths).unwrap();
        let ca = state.paths.certs().join("ca.crt");
        let key = state.paths.certs().join("ca.key");
        let original = std::fs::read(&ca).unwrap();
        std::fs::remove_file(&key).unwrap();
        assert_eq!(ensure_ca(&state.paths).unwrap_err().code, "CA_INCOMPLETE");
        assert_eq!(std::fs::read(&ca).unwrap(), original);
        let wrong = rcgen::KeyPair::generate().unwrap().serialize_pem();
        std::fs::write(&key, &wrong).unwrap();
        assert_eq!(ensure_ca(&state.paths).unwrap_err().code, "CA_KEY_MISMATCH");
        assert_eq!(std::fs::read_to_string(&key).unwrap(), wrong);
    }

    #[test]
    fn same_name_cas_have_different_fingerprints_and_dates_come_from_disk() {
        let (_one, state) = fixture();
        let (_two, other) = fixture();
        ensure_ca(&state.paths).unwrap();
        ensure_ca(&other.paths).unwrap();
        assert_ne!(ca_fingerprint(&state.paths).unwrap(), ca_fingerprint(&other.paths).unwrap());
        let (_, params) = load_ca(&state.paths).unwrap();
        let records = list_certs(&state.paths, &state.store).unwrap();
        assert_eq!(records[0].not_after, params.not_after.unix_timestamp() * 1000);
        assert_eq!(records[0].not_before, params.not_before.unix_timestamp() * 1000);
        assert_eq!(records[0].trusted, Some(false));
    }

    #[test]
    fn failed_certificate_commit_restores_both_files() {
        let (_temp, state) = fixture();
        let cert = state.issue_certificate("restore.test", &[]).unwrap();
        let crt = std::path::Path::new(&cert.cert_path);
        let key = std::path::Path::new(cert.key_path.as_ref().unwrap());
        let old_cert = std::fs::read(crt).unwrap();
        let old_key = std::fs::read(key).unwrap();
        assert!(write_cert_pair(&state.paths, crt, key, "new cert", "new key", || Err(AppError::new("SAVE_FAILED", "fixture"))).is_err());
        assert_eq!(std::fs::read(crt).unwrap(), old_cert);
        assert_eq!(std::fs::read(key).unwrap(), old_key);
    }

    #[test]
    fn repair_is_idempotent_and_delete_respects_https_bindings() {
        let (_temp, state) = fixture();
        let mut site: crate::model::Site = serde_json::from_value(serde_json::json!({
            "id":"tls-fixture", "name":"TLS fixture", "domains":["primary.test","alias.test"],
            "rootDir":state.paths.base, "runtime":{"kind":"static"},
            "https":true, "rewrite":"none", "status":"stopped", "createdAt":0,"updatedAt":0
        })).unwrap();
        state.store.save_site(&site).unwrap();
        let cert = state.issue_certificate("primary.test", &[]).unwrap();
        assert!(cert.sans.contains(&"alias.test".to_string()));
        assert!(state.repair_site_certificates().unwrap().is_empty());
        std::fs::remove_file(cert.key_path.as_ref().unwrap()).unwrap();
        assert_eq!(state.repair_site_certificates().unwrap(), ["primary.test"]);
        assert!(state.repair_site_certificates().unwrap().is_empty());
        assert_eq!(state.delete_local_certificate(&cert.id).unwrap_err().code, "CERT_IN_USE");
        assert!(std::path::Path::new(&cert.cert_path).exists());
        site.https = false;
        state.store.save_site(&site).unwrap();
        state.delete_local_certificate(&cert.id).unwrap();
        assert!(!std::path::Path::new(&cert.cert_path).exists());
        assert!(!std::path::Path::new(cert.key_path.as_ref().unwrap()).exists());
        assert!(!state.store.list_certs().unwrap().iter().any(|c| c.id == cert.id));
    }

    #[test]
    fn repair_checks_actual_ca_signature_and_missing_ca_preserves_leaf() {
        let (_temp, state) = fixture();
        let (_other_temp, other) = fixture();
        let site = serde_json::from_value(serde_json::json!({
            "id":"chain-fixture", "name":"Chain fixture", "domains":["chain.test"],
            "rootDir":state.paths.base, "runtime":{"kind":"static"},
            "https":true, "rewrite":"none", "status":"stopped", "createdAt":0,"updatedAt":0
        })).unwrap();
        state.store.save_site(&site).unwrap();
        let cert = state.issue_certificate("chain.test", &[]).unwrap();
        let original = std::fs::read(&cert.cert_path).unwrap();
        let (_, leaf) = x509_parser::pem::parse_x509_pem(&original).unwrap();
        let roots = |paths: &Paths| {
            let bytes = std::fs::read(paths.certs().join("ca.crt")).unwrap();
            let (_, pem) = x509_parser::pem::parse_x509_pem(&bytes).unwrap();
            let mut roots = rustls::RootCertStore::empty();
            roots.add(rustls::pki_types::CertificateDer::from(pem.contents)).unwrap();
            roots
        };
        ensure_ca(&other.paths).unwrap();
        assert!(signed_by_current_ca(&leaf.contents, &roots(&state.paths)));
        assert!(!signed_by_current_ca(&leaf.contents, &roots(&other.paths)));
        for name in ["ca.crt", "ca.key"] {
            std::fs::copy(other.paths.certs().join(name), state.paths.certs().join(name)).unwrap();
        }
        assert_eq!(state.repair_site_certificates().unwrap(), ["chain.test"]);
        let updated = std::fs::read(&cert.cert_path).unwrap();
        let (_, leaf) = x509_parser::pem::parse_x509_pem(&updated).unwrap();
        assert!(signed_by_current_ca(&leaf.contents, &roots(&state.paths)));
        assert_ne!(original, updated);
        assert!(state.repair_site_certificates().unwrap().is_empty());
        for name in ["ca.crt", "ca.key"] { std::fs::remove_file(state.paths.certs().join(name)).unwrap(); }
        assert_eq!(ensure_ca(&state.paths).unwrap_err().code, "CA_MISSING");
        assert_eq!(std::fs::read(&cert.cert_path).unwrap(), updated);
        assert!(!state.paths.certs().join("ca.key").exists());
    }

    #[test]
    fn exports_are_readable_and_cannot_overwrite_managed_keys() {
        let (_temp, state) = fixture();
        let cert = state.issue_certificate("export.test", &[]).unwrap();
        let key_path = std::path::Path::new(cert.key_path.as_ref().unwrap());
        let original = std::fs::read(&cert.cert_path).unwrap();
        let original_key = std::fs::read(key_path).unwrap();
        let (_, leaf) = x509_parser::pem::parse_x509_pem(&original).unwrap();
        let output = state.paths.base.join("exports");
        let der = output.join("certificate.der");
        export_der(&state.paths, &state.store, &cert.id, &der).unwrap();
        assert_eq!(std::fs::read(&der).unwrap(), leaf.contents);
        let pem = output.join("certificate.pem");
        export_pem_bundle(&state.paths, &state.store, &cert.id, &pem).unwrap();
        let bundle = std::fs::read_to_string(pem).unwrap();
        assert_eq!(bundle.matches("BEGIN CERTIFICATE").count(), 1);
        assert_eq!(bundle.matches("BEGIN PRIVATE KEY").count(), 1);
        let pfx = output.join("certificate.pfx");
        export_pfx(&state.paths, &state.store, &cert.id, "export-password", &pfx).unwrap();
        let pfx = std::fs::read(pfx).unwrap();
        let imported = p12_keystore::KeyStore::from_pkcs12(&pfx, "export-password", p12_keystore::Pkcs12ImportPolicy::Strict).unwrap();
        assert!(imported.private_key_chain().is_some());
        assert!(p12_keystore::KeyStore::from_pkcs12(&pfx, "wrong", p12_keystore::Pkcs12ImportPolicy::Strict).is_err());
        let jks = output.join("certificate.jks");
        assert_eq!(export_jks(&state.paths, &state.store, &cert.id, "short", &jks).unwrap_err().code, "JKS_PASSWORD");
        assert!(!jks.exists());
        export_jks(&state.paths, &state.store, &cert.id, "export-password", &jks).unwrap();
        let mut imported = jks::KeyStore::new();
        imported.load(std::fs::File::open(jks).unwrap(), b"export-password").unwrap();
        assert_eq!(imported.get_private_key_entry(&cert.subject, b"export-password").unwrap().certificate_chain[0].content, leaf.contents);
        for target in [std::path::Path::new(&cert.cert_path), key_path, &state.paths.certs().join("ca.crt"), &state.paths.certs().join("export.der")] {
            assert_eq!(export_der(&state.paths, &state.store, &cert.id, target).unwrap_err().code, "CERT_EXPORT_TARGET");
        }
        assert_eq!(export_pfx(&state.paths, &state.store, &cert.id, "", key_path).unwrap_err().code, "CERT_EXPORT_TARGET");
        assert_eq!(export_jks(&state.paths, &state.store, &cert.id, "export-password", key_path).unwrap_err().code, "CERT_EXPORT_TARGET");
        assert_eq!(export_pem_bundle(&state.paths, &state.store, &cert.id, key_path).unwrap_err().code, "CERT_EXPORT_TARGET");
        let linked = output.join("hardlink.der");
        std::fs::hard_link(&cert.cert_path, &linked).unwrap();
        export_der(&state.paths, &state.store, &cert.id, &linked).unwrap();
        assert_eq!(std::fs::read(&cert.cert_path).unwrap(), original);
        assert_eq!(std::fs::read(key_path).unwrap(), original_key);
        list_certs(&state.paths, &state.store).unwrap();
        assert_eq!(state.delete_local_certificate("ca").unwrap_err().code, "CERT_DELETE_UNSUPPORTED");
        let mut acme = cert;
        acme.id = "acme-fixture".into();
        acme.kind = "acme".into();
        state.store.save_cert(&acme).unwrap();
        assert_eq!(state.delete_local_certificate(&acme.id).unwrap_err().code, "CERT_DELETE_UNSUPPORTED");
    }
}

/* ================= 证书生命周期 ================= */

impl crate::CoreState {
    /// 与站点写配置共用生命周期锁，更新证书后让运行中的 Web 服务加载新文件。
    pub fn issue_certificate(&self, domain: &str, sans: &[String]) -> Result<CertRecord> {
        let _operation = self.manager.lifecycle.lock();
        let mut requested = vec![domain.to_string()];
        requested.extend_from_slice(sans);
        let mut domains = normalize_domains(&requested)?;
        let sites = self.store.list_sites()?;
        for site in &sites {
            if site.https && site.domains.first().is_some_and(|d| d.eq_ignore_ascii_case(&domains[0])) {
                // 同一主域名对应同一证书文件，手动签发不能移除已绑定站点需要的 SAN。
                for name in normalize_domains(&site.domains)? {
                    if !domains.contains(&name) { domains.push(name); }
                }
            }
        }
        let cert = issue_site_cert(&self.paths, &self.store, &domains)?;
        self.reload_certificate_sites(&[cert.subject.clone()])?;
        Ok(cert)
    }

    pub fn repair_site_certificates(&self) -> Result<Vec<String>> {
        let _operation = self.manager.lifecycle.lock();
        let issued = reissue_missing_site_certs(&self.paths, &self.store)?;
        self.reload_certificate_sites(&issued)?;
        Ok(issued)
    }

    fn reload_certificate_sites(&self, domains: &[String]) -> Result<()> {
        let running = self.store.list_sites()?.iter().any(|site| {
            site.https && site.domains.first().is_some_and(|primary| domains.contains(primary))
                && crate::sites::derive_status(&self.paths, site) == "running"
                && self.manager.snapshot(&site.runtime.web_server)
                    .is_some_and(|s| s.state == crate::model::ServiceState::Running)
        });
        if running {
            crate::ops::rebuild_and_reload(&self.store, &self.paths, &self.manager).map_err(|error| {
                AppError::new("CERT_RELOAD_FAILED", "证书文件已更新，但 Web 服务未能加载新证书")
                    .with_hint("请检查服务日志和配置，修复后重新签发或重启对应 Web 服务。")
                    .with_detail(format!("{}: {}", error.code, error.message))
            })?;
        }
        Ok(())
    }

    /// 删除不再被 HTTPS 站点使用的本地证书；这不是公有 CA 的吊销操作。
    pub fn delete_local_certificate(&self, id: &str) -> Result<()> {
        let _operation = self.manager.lifecycle.lock();
        let _files = CERT_FILES.lock();
        let cert = self.store.list_certs()?.into_iter().find(|c| c.id == id)
            .ok_or_else(|| AppError::new("NOT_FOUND", "证书不存在"))?;
        if cert.kind != "site" {
            return Err(AppError::new("CERT_DELETE_UNSUPPORTED", "这里只能删除本地签发的站点证书"));
        }
        let primary = normalize_domains(&[cert.subject.clone()])?.remove(0);
        let users: Vec<_> = self.store.list_sites()?.into_iter().filter(|site|
            site.https && site.domains.first().is_some_and(|d| d.eq_ignore_ascii_case(&primary))
        ).map(|site| site.name).collect();
        if !users.is_empty() {
            return Err(AppError::new("CERT_IN_USE", format!("证书仍被站点 {} 使用", users.join("、")))
                .with_hint("先在这些站点中关闭 HTTPS 或更换主域名，再删除本地证书。"));
        }
        let stem = cert_stem(&primary);
        let paths: Vec<_> = ["crt", "key"].into_iter().map(|ext|
            crate::paths::checked_data_path(&self.paths.base, &format!("certs/sites/{stem}.{ext}"))
        ).collect::<std::io::Result<_>>()?;
        let staging = tempfile::tempdir_in(self.paths.certs())?;
        let mut moved = Vec::new();
        let result: Result<()> = (|| {
            for (index, path) in paths.iter().enumerate() {
                if path.exists() {
                    let target = staging.path().join(index.to_string());
                    std::fs::rename(path, &target)?;
                    moved.push((path, target));
                }
            }
            self.store.delete_cert(id)
        })();
        if let Err(error) = result {
            for (path, temporary) in moved.iter().rev() {
                if let Err(restore_error) = std::fs::rename(temporary, path) {
                    // 保留未恢复文件，不能让 TempDir::drop 删除它。
                    let recovery = staging.keep();
                    return Err(AppError::new("CERT_ROLLBACK_FAILED", "删除失败，证书文件需要手动恢复")
                        .with_hint(format!("恢复目录：{}", recovery.display()))
                        .with_detail(format!("{}；{restore_error}", error.message)));
                }
            }
            return Err(error);
        }
        Ok(())
    }
}

/* ================= 证书导出 ================= */

/// 导出不能覆盖托管证书目录或已有证书/私钥。临时文件替换避免写到一半破坏旧导出。
fn write_certificate_export(paths: &Paths, store: &Store, out_path: &std::path::Path, bytes: &[u8]) -> Result<String> {
    use std::io::Write;
    let absolute = std::path::absolute(out_path)?;
    let parent = absolute.parent().ok_or_else(|| AppError::new("CERT_EXPORT_TARGET", "请选择导出文件路径"))?;
    let filename = absolute.file_name().ok_or_else(|| AppError::new("CERT_EXPORT_TARGET", "请选择导出文件名"))?;
    std::fs::create_dir_all(parent).map_err(|e| AppError::io("创建导出目录失败", e))?;
    let target = parent.canonicalize()?.join(filename);
    let resolved = target.canonicalize().unwrap_or_else(|_| target.clone());
    let managed = paths.certs().canonicalize()?;
    let mut protected = target.starts_with(&managed) || resolved.starts_with(&managed);
    for record in store.list_certs()? {
        for source in std::iter::once(record.cert_path).chain(record.key_path) {
            if let Ok(source) = std::fs::canonicalize(source) {
                protected |= source == resolved;
            }
        }
    }
    if protected {
        return Err(AppError::new("CERT_EXPORT_TARGET", "不能将导出文件写入托管证书目录或覆盖正在使用的证书、私钥")
            .with_hint("请选择下载目录或其他独立文件夹。"));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(target.parent().unwrap())?;
    temporary.write_all(bytes).map_err(|e| AppError::io("写入导出文件失败", e))?;
    temporary.as_file().sync_all()?;
    temporary.persist(&target).map_err(|e| AppError::io("保存导出文件失败", e.error))?;
    Ok(out_path.to_string_lossy().to_string())
}

/// 把一段 PEM（可能是链）拆成 DER 列表
fn pem_chain_to_certs(pem: &str) -> Result<Vec<p12_keystore::Certificate>> {
    let mut out = Vec::new();
    for block in pem.split("-----END CERTIFICATE-----") {
        let b = block.trim();
        if b.is_empty() {
            continue;
        }
        let b64: String = b.lines().filter(|l| !l.starts_with("-----")).collect();
        use base64::Engine as _;
        let der = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| AppError::new("PFX_DECODE", format!("证书 PEM 解码失败：{e}")))?;
        out.push(
            p12_keystore::Certificate::from_der(&der)
                .map_err(|e| AppError::new("PFX_DECODE", format!("证书 DER 解析失败：{e}")))?,
        );
    }
    Ok(out)
}

/// 把某张本机证书（含私钥与证书链）导出为 PKCS#12 (.pfx)。
/// Windows IIS / 部分设备导入只认这个格式。password 可为空（不加密）。
pub fn export_pfx(
    paths: &Paths,
    store: &Store,
    cert_id: &str,
    password: &str,
    out_path: &std::path::Path,
) -> Result<String> {
    let _files = CERT_FILES.lock();
    let rec = store
        .list_certs()?
        .into_iter()
        .find(|c| c.id == cert_id)
        .ok_or_else(|| AppError::new("NOT_FOUND", "证书不存在"))?;
    let key_pem = std::fs::read_to_string(
        rec.key_path
            .as_deref()
            .ok_or_else(|| AppError::new("PFX_NO_KEY", "该证书没有私钥，无法导出 PFX"))?,
    )
    .map_err(|e| AppError::io("读取私钥失败", e))?;
    let cert_pem =
        std::fs::read_to_string(&rec.cert_path).map_err(|e| AppError::io("读取证书失败", e))?;

    // 私钥 PEM → PKCS#8 DER
    let key_b64: String = key_pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    use base64::Engine as _;
    let key_der = base64::engine::general_purpose::STANDARD
        .decode(key_b64.trim())
        .map_err(|e| AppError::new("PFX_DECODE", format!("私钥 PEM 解码失败：{e}")))?;
    let key = p12_keystore::PrivateKey::from_der(&key_der)
        .map_err(|e| AppError::new("PFX_DECODE", format!("私钥解析失败：{e}")))?;
    let certs = pem_chain_to_certs(&cert_pem)?;
    if certs.is_empty() {
        return Err(AppError::new("PFX_DECODE", "证书文件里没有证书"));
    }

    let chain = p12_keystore::PrivateKeyChain::new(rec.subject.clone(), key, certs);
    let mut store12 = p12_keystore::KeyStore::new();
    store12.add_entry(
        &rec.subject,
        p12_keystore::KeyStoreEntry::PrivateKeyChain(chain),
    );
    let pfx = store12
        .writer(password)
        .write()
        .map_err(|e| AppError::new("PFX_BUILD", format!("生成 PFX 失败：{e}")))?;
    write_certificate_export(paths, store, out_path, &pfx)
}

/// 导出 DER（二进制 X.509，部分设备/中间件要这个格式）：取链里第一张（leaf）
pub fn export_der(paths: &Paths, store: &Store, cert_id: &str, out_path: &std::path::Path) -> Result<String> {
    let _files = CERT_FILES.lock();
    let rec = store
        .list_certs()?
        .into_iter()
        .find(|c| c.id == cert_id)
        .ok_or_else(|| AppError::new("NOT_FOUND", "证书不存在"))?;
    let pem =
        std::fs::read_to_string(&rec.cert_path).map_err(|e| AppError::io("读取证书失败", e))?;
    let leaf = pem
        .split("-----END CERTIFICATE-----")
        .next()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::new("PFX_DECODE", "证书文件为空"))?;
    let b64: String = leaf.lines().filter(|l| !l.starts_with("-----")).collect();
    use base64::Engine as _;
    let der = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| AppError::new("PFX_DECODE", format!("证书解码失败：{e}")))?;
    write_certificate_export(paths, store, out_path, &der)
}

/// 导出 JKS（Java Keystore，Tomcat 等 Java 系中间件用）。
/// JKS 规范要求密码 ≥ 6 字符，这里如实转述错误而不是悄悄放行。
pub fn export_jks(
    paths: &Paths,
    store: &Store,
    cert_id: &str,
    password: &str,
    out_path: &std::path::Path,
) -> Result<String> {
    let _files = CERT_FILES.lock();
    let rec = store
        .list_certs()?
        .into_iter()
        .find(|c| c.id == cert_id)
        .ok_or_else(|| AppError::new("NOT_FOUND", "证书不存在"))?;
    let key_pem = std::fs::read_to_string(
        rec.key_path
            .as_deref()
            .ok_or_else(|| AppError::new("JKS_NO_KEY", "该证书没有私钥，无法导出 JKS"))?,
    )
    .map_err(|e| AppError::io("读取私钥失败", e))?;
    let cert_pem =
        std::fs::read_to_string(&rec.cert_path).map_err(|e| AppError::io("读取证书失败", e))?;
    if password.chars().count() < 6 {
        return Err(AppError::new(
            "JKS_PASSWORD",
            "JKS 密码至少 6 个字符（Java Keystore 规范）",
        ));
    }

    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let key_b64: String = key_pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    let key_der = b64
        .decode(key_b64.trim())
        .map_err(|e| AppError::new("JKS_DECODE", format!("私钥解码失败：{e}")))?;
    let mut chain = Vec::new();
    for block in cert_pem.split("-----END CERTIFICATE-----") {
        let b = block.trim();
        if b.is_empty() {
            continue;
        }
        let c_b64: String = b.lines().filter(|l| !l.starts_with("-----")).collect();
        let der = b64
            .decode(c_b64.trim())
            .map_err(|e| AppError::new("JKS_DECODE", format!("证书解码失败：{e}")))?;
        chain.push(jks::Certificate {
            cert_type: "X509".into(),
            content: der,
        });
    }
    if chain.is_empty() {
        return Err(AppError::new("JKS_DECODE", "证书文件里没有证书"));
    }

    let mut ks = jks::KeyStore::new();
    ks.set_private_key_entry(
        &rec.subject,
        jks::PrivateKeyEntry {
            creation_time: std::time::SystemTime::now(),
            private_key: key_der,
            certificate_chain: chain,
        },
        password.as_bytes(),
    )
    .map_err(|e| AppError::new("JKS_BUILD", format!("构建 JKS 失败：{e}")))?;
    let mut buf = Vec::new();
    ks.store(&mut buf, password.as_bytes())
        .map_err(|e| AppError::new("JKS_BUILD", format!("序列化 JKS 失败：{e}")))?;

    write_certificate_export(paths, store, out_path, &buf)
}

/// 导出 PEM 打包（证书链 + 私钥 拼一个 .pem，nginx / 迁移别家最顺手）
pub fn export_pem_bundle(
    paths: &Paths,
    store: &Store,
    cert_id: &str,
    out_path: &std::path::Path,
) -> Result<String> {
    let _files = CERT_FILES.lock();
    let rec = store
        .list_certs()?
        .into_iter()
        .find(|c| c.id == cert_id)
        .ok_or_else(|| AppError::new("NOT_FOUND", "证书不存在"))?;
    let key_pem = std::fs::read_to_string(
        rec.key_path
            .as_deref()
            .ok_or_else(|| AppError::new("PEM_NO_KEY", "该证书没有私钥，无法打包"))?,
    )
    .map_err(|e| AppError::io("读取私钥失败", e))?;
    let cert_pem =
        std::fs::read_to_string(&rec.cert_path).map_err(|e| AppError::io("读取证书失败", e))?;
    let mut bundle = cert_pem;
    if !bundle.ends_with('\n') {
        bundle.push('\n');
    }
    bundle.push_str(&key_pem);
    write_certificate_export(paths, store, out_path, bundle.as_bytes())
}
