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
pub(crate) fn write_cert_pair(
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
    let key_pem = crate::certs::read_managed_pem(&crate::paths::checked_data_path(&paths.base, "certs/ca.key")?)?;
    let cert_pem = crate::certs::read_managed_pem(&crate::paths::checked_data_path(&paths.base, "certs/ca.crt")?)?;
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

/// 未知来源的站点文件与历史本地记录要求恢复原 CA；已知 ACME 文件不依赖本地 CA。
fn has_local_ca_history(paths: &Paths, records: &[CertRecord]) -> Result<bool> {
    if records.iter().any(|c| matches!(c.kind.as_str(), "ca" | "site")) { return Ok(true); }
    let acme_paths: Vec<_> = records.iter().filter(|c| c.kind == "acme").filter_map(|c| {
        let (cert, key) = crate::certs::acme_paths(paths, &c.id).ok()?;
        (std::path::Path::new(&c.cert_path) == cert && c.key_path.as_deref().map(std::path::Path::new) == Some(key.as_path()))
            .then_some([cert, key])
    }).flatten().collect();
    let directory = crate::paths::checked_data_path(&paths.base, "certs/sites")?;
    match std::fs::read_dir(directory) {
        Ok(entries) => for entry in entries {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "crt" || ext == "key") && !acme_paths.contains(&path) { return Ok(true); }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(AppError::io("检查现有站点证书", error)),
    }
    Ok(false)
}

pub(crate) fn local_ca_expected(paths: &Paths, records: &[CertRecord], sites: &[crate::model::Site]) -> Result<bool> {
    for file in ["ca.crt", "ca.key"] {
        match std::fs::symlink_metadata(paths.certs().join(file)) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(AppError::io("检查根 CA 文件", error)),
        }
    }
    Ok(has_local_ca_history(paths, records)? || sites.iter().any(|site| site.https && site.runtime.uses_default_certificate()
        && !records.iter().any(|cert| cert.kind == "acme" && crate::certs::uses_managed_certificate(site, cert))))
}

/// 显式签发入口使用；读取列表和服务默认 TLS 不创建根 CA。
pub fn ensure_ca(paths: &Paths) -> Result<()> {
    ensure_ca_with_records(paths, &[])
}

fn ensure_ca_with_records(paths: &Paths, records: &[CertRecord]) -> Result<()> {
    let _files = CERT_FILES.lock();
    let ca_key = crate::paths::checked_data_path(&paths.base, "certs/ca.key")?;
    let ca_crt = crate::paths::checked_data_path(&paths.base, "certs/ca.crt")?;
    if ca_key.exists() && ca_crt.exists() {
        load_ca(paths)?;
        return Ok(());
    }
    if ca_key.exists() || ca_crt.exists() {
        return Err(AppError::new("CA_INCOMPLETE", "根 CA 文件不完整，已保留现有文件")
            .with_hint("请从备份恢复匹配的 ca.crt 与 ca.key。自动创建新 CA 会使原站点证书失去信任。"));
    }
    if has_local_ca_history(paths, records)? {
        return Err(AppError::new("CA_MISSING", "根 CA 文件丢失，但仍有本地站点证书或历史记录")
            .with_hint("请从备份恢复原来的 ca.crt 与 ca.key，避免自动更换根 CA 导致原证书失去信任。"));
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
    issue_site_cert_for_update(paths, store, domains, None)
}

/// 站点改回默认证书时允许解除自身选择，但不能覆盖其它站点仍绑定的 ACME 文件。
pub(crate) fn issue_site_cert_for_update(paths: &Paths, store: &Store, domains: &[String], site_id: Option<&str>) -> Result<CertRecord> {
    let domains = normalize_domains(domains)?;
    let _files = CERT_FILES.lock();
    let acme_id = format!("acme-{}", domains[0]);
    let users: Vec<_> = store.list_sites()?.into_iter().filter(|site|
        Some(site.id.as_str()) != site_id && site.runtime.acme_cert_id.as_deref() == Some(&acme_id)
    ).map(|site| site.name).collect();
    if !users.is_empty() {
        return Err(AppError::new("CERT_IN_USE", format!("此证书文件仍被站点 {} 选择为 ACME 证书，未替换", users.join("、")))
            .with_hint("请先更换这些站点的证书选择，或在当前站点直接选择该 ACME 证书。"));
    }
    ensure_ca_with_records(paths, &store.list_certs()?)?;
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
    write_cert_pair(paths, &crt_path, &key_path, &certified.pem(), &key_pair.serialize_pem(), || store.replace_managed_certs(std::slice::from_ref(&record)))?;
    Ok(record)
}

/// 信任根 CA：Windows certutil（需管理员） / macOS security
pub fn trust_ca(paths: &Paths) -> Result<()> {
    let _files = CERT_FILES.lock();
    load_ca(paths)?;
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

fn read_ca_record(paths: &Paths) -> Result<CertRecord> {
    let pem = crate::certs::read_managed_pem(&crate::paths::checked_data_path(&paths.base, "certs/ca.crt")?)?;
    let (subject, _, before, after) = crate::certs::parse_pem_info(&pem)
        .ok_or_else(|| AppError::new("CERT_PARSE_FAILED", "无法解析本地根 CA"))?;
    Ok(CertRecord {
        id: "ca".into(), kind: "ca".into(), subject, sans: vec![],
        not_before: before * 1000, not_after: after * 1000,
        cert_path: paths.certs().join("ca.crt").to_string_lossy().into(),
        key_path: Some(paths.certs().join("ca.key").to_string_lossy().into()),
        trusted: Some(load_ca(paths).is_ok() && ca_trusted(paths)),
    })
}

/// 默认欢迎页使用独立的非 CA 证书，不向 Web 服务提供本地根 CA 私钥。
pub(crate) fn ensure_server_fallback(paths: &Paths) -> Result<()> {
    let _files = CERT_FILES.lock();
    let cert = crate::paths::checked_data_path(&paths.base, "certs/fallback/localhost.crt")?;
    let key = crate::paths::checked_data_path(&paths.base, "certs/fallback/localhost.key")?;
    let domains = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    let current = crate::certs::read_managed_pem(&cert).and_then(|pem|
        crate::certs::read_managed_pem(&key).and_then(|key| crate::certs::deployment_validity(&pem, &key, &domains)));
    if current.is_ok_and(|(_, after)| after > to_ms(now_plus(RENEW_BEFORE_DAYS))) { return Ok(()); }
    let pair = rcgen::KeyPair::generate().map_err(|e| AppError::internal("生成默认 TLS 密钥", e.to_string()))?;
    let mut params = rcgen::CertificateParams::new(domains).map_err(|e| AppError::internal("默认 TLS 参数", e.to_string()))?;
    params.distinguished_name.push(rcgen::DnType::CommonName, "NiceEnv localhost");
    params.not_before = now_minus(1); params.not_after = now_plus(365);
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = params.self_signed(&pair).map_err(|e| AppError::internal("生成默认 TLS 证书", e.to_string()))?;
    std::fs::create_dir_all(cert.parent().unwrap())?;
    write_cert_pair(paths, &cert, &key, &leaf.pem(), &pair.serialize_pem(), || Ok(()))
}

/// 只读列表。根 CA 的缺失/损坏由健康接口逐项报告，不阻断其它证书。
pub fn list_certs(paths: &Paths, store: &Store) -> Result<Vec<CertRecord>> {
    let _files = CERT_FILES.lock();
    let mut list = store.list_certs()?;
    list.retain(|c| c.kind != "ca");
    if let Ok(ca) = read_ca_record(paths) { list.insert(0, ca); }
    Ok(list)
}

/// 只更新缺失、损坏、域名覆盖不完整或 7 天内到期的 HTTPS 站点证书。
/// 本地证书有效期为 30 天，不能把新签的证书也判成“30 天内到期”反复替换。
pub fn reissue_missing_site_certs(paths: &Paths, store: &Store) -> Result<Vec<String>> {
    let _files = CERT_FILES.lock();
    let records = store.list_certs()?;
    // 兼容旧版只为自动化主域名存记录、其它 SAN 站点只有文件副本的情况。
    let acme_files: Vec<_> = records.iter().filter(|c| c.kind == "acme").filter_map(|c| {
        let path = crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{}.crt", cert_stem(&c.subject))).ok()?;
        if std::path::Path::new(&c.cert_path) != path { return None; }
        crate::certs::read_managed_pem(&path).ok().map(|pem| (c, pem))
    }).collect();
    let mut local_sites = Vec::new();
    let mut recovered = Vec::new();
    // 先检查全部 ACME 文件，失败时不修改任何站点，防止悄悄降级到本地 CA。
    for site in store.list_sites()? {
        if !site.https || site.runtime.imported_cert_id.is_some() || site.domains.is_empty() { continue; }
        if site.runtime.acme_cert_id.is_some() {
            crate::certs::validate_site_certificate(paths, store, &site).map_err(|error|
                AppError::new("ACME_REPAIR_REQUIRED", format!("站点「{}」的 ACME 证书需要处理：{}", site.name, error.message))
                    .with_hint("已保留证书选择与文件，请在证书自动化中续签并重新部署。"))?;
            continue;
        }
        let domains = normalize_domains(&site.domains)?;
        let primary = &domains[0];
        let cert = crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{}.crt", cert_stem(primary)))?;
        let key = crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{}.key", cert_stem(primary)))?;
        let direct = records.iter().find(|c| c.kind == "acme" && std::path::Path::new(&c.cert_path) == cert);
        let pem = crate::certs::read_managed_pem(&cert);
        let legacy = pem.as_ref().ok().and_then(|pem| acme_files.iter().find(|(_, content)| content == pem).map(|(c, _)| *c));
        if direct.or(legacy).is_none() {
            local_sites.push((site, domains));
            continue;
        }
        let checked = (|| -> Result<CertRecord> {
            let pem = pem?;
            let key_pem = crate::certs::read_managed_pem(&key)?;
            let (_, sans, _, _) = crate::certs::parse_pem_info(&pem)
                .ok_or_else(|| AppError::new("CERT_PARSE_FAILED", "无法解析 ACME 证书"))?;
            let (not_before, not_after) = crate::certs::deployment_validity(&pem, &key_pem, &sans)?;
            if !domains.iter().all(|d| crate::certs::covers_domain(&sans, d)) {
                return Err(AppError::new("CERT_SITE_COVERAGE", "ACME 证书未覆盖站点全部域名"));
            }
            Ok(CertRecord { id: format!("acme-{primary}"), kind: "acme".into(), subject: primary.clone(),
                sans, not_before, not_after, cert_path: cert.to_string_lossy().into_owned(),
                key_path: Some(key.to_string_lossy().into_owned()), trusted: Some(true) })
        })().map_err(|error| AppError::new("ACME_REPAIR_REQUIRED", format!("站点「{}」的 ACME 证书需要处理：{}", site.name, error.message))
            .with_hint("原证书已保留。请在证书自动化中重试部署；材料过期或域名变更时重新签发。"))?;
        if direct.is_none() || records.iter().any(|c| c.kind == "site" && std::path::Path::new(&c.cert_path) == cert) {
            recovered.push(checked);
        }
    }
    if !recovered.is_empty() { store.replace_managed_certs(&recovered)?; }
    if local_sites.is_empty() { return Ok(Vec::new()); }
    ensure_ca_with_records(paths, &store.list_certs()?)?;
    let ca_pem = std::fs::read(paths.certs().join("ca.crt"))?;
    let (_, ca) = x509_parser::pem::parse_x509_pem(&ca_pem)
        .map_err(|e| AppError::internal("解析根 CA", e.to_string()))?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls::pki_types::CertificateDer::from(ca.contents))
        .map_err(|e| AppError::internal("读取根 CA", e.to_string()))?;
    let soon = to_ms(now_plus(RENEW_BEFORE_DAYS));
    let mut issued = Vec::new();
    for (_site, domains) in local_sites {
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
    fn certificate_inventory_is_read_only_and_acme_does_not_require_a_local_ca() {
        let (_temp, state) = fixture();
        assert!(list_certs(&state.paths, &state.store).unwrap().is_empty());
        assert!(crate::certs::report(&state.paths, &state.store).unwrap().certs.is_empty());
        assert!(!state.paths.certs().join("ca.crt").exists());
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["acme.example.com".into()]).unwrap().self_signed(&key).unwrap();
        let (crt, private) = crate::certs::acme_paths(&state.paths, "acme-acme.example.com").unwrap();
        std::fs::create_dir_all(crt.parent().unwrap()).unwrap();
        std::fs::write(&crt, cert.pem()).unwrap(); std::fs::write(&private, key.serialize_pem()).unwrap();
        let record = CertRecord { id: "acme-acme.example.com".into(), kind: "acme".into(), subject: "acme.example.com".into(),
            sans: vec!["acme.example.com".into()], not_before: 1, not_after: 2,
            cert_path: crt.to_string_lossy().into(), key_path: Some(private.to_string_lossy().into()), trusted: None };
        state.store.save_cert(&record).unwrap();
        let database = rusqlite::Connection::open(state.paths.db()).unwrap();
        database.execute_batch("CREATE TRIGGER no_certificate_read_write BEFORE INSERT ON certs BEGIN SELECT RAISE(ABORT,'read-only'); END;").unwrap();
        assert_eq!(list_certs(&state.paths, &state.store).unwrap().len(), 1);
        let report = crate::certs::report(&state.paths, &state.store).unwrap();
        assert_eq!(report.certs.len(), 1); assert_eq!(report.certs[0].kind, "acme");
        assert_eq!(report.critical, 0); assert!(!report.ca_trusted);
        assert!(!state.paths.certs().join("ca.key").exists());
        assert_eq!(std::fs::read_to_string(&crt).unwrap(), cert.pem());
        database.execute_batch("DROP TRIGGER no_certificate_read_write;").unwrap();
        state.issue_certificate("new-local.test", &[]).unwrap();
        assert!(state.paths.certs().join("ca.key").is_file());
        assert_eq!(std::fs::read_to_string(crt).unwrap(), cert.pem());
    }

    #[test]
    fn invalid_ca_does_not_hide_certificates_or_mutate_root_files() {
        let (_temp, state) = fixture();
        let local = state.issue_certificate("local.test", &[]).unwrap();
        let root = state.paths.certs().join("ca.crt"); let key = state.paths.certs().join("ca.key");
        let original = std::fs::read(&root).unwrap(); let original_key = std::fs::read(&key).unwrap();
        for failure in ["corrupt", "missing-key", "wrong-key", "not-ca", "missing-both"] {
            std::fs::write(&root, &original).unwrap(); std::fs::write(&key, &original_key).unwrap();
            match failure {
                "corrupt" => std::fs::write(&root, "not a certificate").unwrap(),
                "missing-key" => std::fs::remove_file(&key).unwrap(),
                "wrong-key" => std::fs::write(&key, rcgen::KeyPair::generate().unwrap().serialize_pem()).unwrap(),
                "not-ca" => { std::fs::copy(&local.cert_path, &root).unwrap(); std::fs::copy(local.key_path.as_ref().unwrap(), &key).unwrap(); },
                _ => { std::fs::remove_file(&root).unwrap(); std::fs::remove_file(&key).unwrap(); },
            }
            let before = std::fs::read(&root).ok(); let before_key = std::fs::read(&key).ok();
            let records = list_certs(&state.paths, &state.store).unwrap();
            assert!(records.iter().any(|c| c.id == local.id), "{failure}");
            let report = crate::certs::report(&state.paths, &state.store).unwrap();
            assert!(!report.ca_trusted, "{failure}");
            assert_eq!(report.certs.iter().find(|c| c.kind == "ca").unwrap().status, "invalid", "{failure}");
            assert!(state.issue_certificate("another.test", &[]).is_err(), "{failure}");
            assert_eq!(std::fs::read(&root).ok(), before); assert_eq!(std::fs::read(&key).ok(), before_key);
        }
    }

    #[test]
    fn ca_certificate_listing_and_export_do_not_persist_records() {
        let (temp, state) = fixture(); ensure_ca(&state.paths).unwrap();
        let database = rusqlite::Connection::open(state.paths.db()).unwrap();
        database.execute_batch("CREATE TRIGGER no_certificate_read_write BEFORE INSERT ON certs BEGIN SELECT RAISE(ABORT,'read-only'); END;").unwrap();
        assert_eq!(list_certs(&state.paths, &state.store).unwrap()[0].kind, "ca");
        assert!(state.store.list_certs().unwrap().is_empty());
        let output = temp.path().join("root.der");
        export_der(&state.paths, &state.store, "ca", &output).unwrap();
        let bytes = std::fs::read(output).unwrap();
        assert!(x509_parser::parse_x509_certificate(&bytes).unwrap().1.is_ca());
        assert!(state.store.list_certs().unwrap().is_empty());
    }

    #[test]
    fn default_server_certificate_is_independent_idempotent_and_preserves_root() {
        let (_temp, state) = fixture();
        // 模拟只有 ACME 文件，尚未创建本地 CA。配置生成不读取或改写站点证书。
        std::fs::create_dir_all(state.paths.certs().join("sites")).unwrap();
        let existing = state.paths.certs().join("sites/existing.crt"); std::fs::write(&existing, "keep-existing").unwrap();
        crate::configgen::write_nginx_conf(&state.paths, &state.paths.base.join("nginx-runtime"), &[], 8080, 8443).unwrap();
        crate::configgen::write_httpd_conf(&state.paths, &state.paths.base.join("apache-runtime"), &[], 8180, 8444).unwrap();
        assert!(!state.paths.certs().join("ca.crt").exists());
        let crt = state.paths.certs().join("fallback/localhost.crt"); let key = state.paths.certs().join("fallback/localhost.key");
        let pem = std::fs::read_to_string(&crt).unwrap(); let key_pem = std::fs::read_to_string(&key).unwrap();
        crate::certs::deployment_validity(&pem, &key_pem, &["localhost".into(), "127.0.0.1".into(), "::1".into()]).unwrap();
        let (_, parsed) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).unwrap();
        assert!(!parsed.parse_x509().unwrap().is_ca());
        for path in [state.paths.nginx_conf(), state.paths.apache_conf()] {
            let conf = std::fs::read_to_string(path).unwrap();
            assert!(conf.contains("/fallback/localhost.crt")); assert!(!conf.contains("/ca.key")); assert!(!conf.contains("ssl-dummy"));
        }
        std::fs::write(state.paths.certs().join("ca.crt"), "damaged-root-keep").unwrap();
        crate::configgen::write_nginx_conf(&state.paths, &state.paths.base.join("nginx-runtime"), &[], 8080, 8443).unwrap();
        crate::configgen::write_httpd_conf(&state.paths, &state.paths.base.join("apache-runtime"), &[], 8180, 8444).unwrap();
        assert_eq!(std::fs::read_to_string(&crt).unwrap(), pem); assert_eq!(std::fs::read_to_string(&key).unwrap(), key_pem);
        assert_eq!(std::fs::read_to_string(existing).unwrap(), "keep-existing");
        assert_eq!(std::fs::read_to_string(state.paths.certs().join("ca.crt")).unwrap(), "damaged-root-keep");
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
    fn imported_exports_preserve_chain_and_key_without_creating_local_ca() {
        let (_temp, state) = fixture();
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(vec!["fixture-ca.example.com".into()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["export.example.com".into()]).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, "export.example.com");
        let leaf = params.signed_by(&key, &ca, &ca_key).unwrap();
        let cert_pem = format!("{}{}", leaf.pem(), ca.pem());
        let (cert_path, key_path) = crate::certs::imported_paths(&state.paths, "fixture-export").unwrap();
        std::fs::create_dir_all(cert_path.parent().unwrap()).unwrap();
        std::fs::write(&cert_path, &cert_pem).unwrap(); std::fs::write(&key_path, key.serialize_pem()).unwrap();
        let id = "imported:fixture-export"; let out = state.paths.base.join("download");
        let password = "导出密码-123";
        export_pfx(&state.paths, &state.store, id, password, &out.with_extension("pfx")).unwrap();
        let pfx = std::fs::read(out.with_extension("pfx")).unwrap();
        let p12 = p12_keystore::KeyStore::from_pkcs12(&pfx, password, p12_keystore::Pkcs12ImportPolicy::Strict).unwrap();
        let (alias, entry) = p12.private_key_chain().unwrap(); assert_eq!(alias, "export.example.com"); assert_eq!(entry.certs().len(), 2);
        assert_eq!(entry.key().as_der(), key.serialize_der());
        assert!(p12_keystore::KeyStore::from_pkcs12(&pfx, "wrong", p12_keystore::Pkcs12ImportPolicy::Strict).is_err());
        assert_eq!(export_pfx(&state.paths, &state.store, id, "emoji-🔑", &out.with_extension("pfx")).unwrap_err().code, "PFX_PASSWORD");
        assert_eq!(std::fs::read(out.with_extension("pfx")).unwrap(), pfx);
        let password = "导出密码-🔑123";
        export_jks(&state.paths, &state.store, id, password, &out.with_extension("jks")).unwrap();
        // 独立按 OpenJDK JavaKeyStore 的 char[] 大端编码核对完整性摘要，避免库自身往返掩盖乱码密码。
        use sha1::Digest;
        let bytes = std::fs::read(out.with_extension("jks")).unwrap();
        let java_password = [0x5b,0xfc,0x51,0xfa,0x5b,0xc6,0x78,0x01,0x00,0x2d,0xd8,0x3d,0xdd,0x11,0x00,0x31,0x00,0x32,0x00,0x33];
        let mut digest = sha1::Sha1::new(); digest.update(java_password); digest.update(b"Mighty Aphrodite"); digest.update(&bytes[..bytes.len()-20]);
        assert_eq!(digest.finalize().as_slice(), &bytes[bytes.len()-20..]);
        let mut reader = bytes.as_slice(); let mut decoder = jks::decoder::Decoder::new(&mut reader);
        decoder.update_digest(&java_password); decoder.update_digest(b"Mighty Aphrodite");
        assert_eq!(decoder.read_u32().unwrap(), jks::common::MAGIC); let version = decoder.read_u32().unwrap();
        assert_eq!(decoder.read_u32().unwrap(), 1); assert_eq!(decoder.read_u32().unwrap(), jks::common::PRIVATE_KEY_TAG);
        assert_eq!(decoder.read_string().unwrap(), "export.example.com");
        let mut entry = decoder.read_private_key_entry(version).unwrap(); decoder.verify_digest().unwrap();
        entry.private_key = jks::keyprotector::decrypt(&entry.private_key, password.as_bytes(), jks_password_bytes).unwrap();
        assert_eq!(entry.certificate_chain.len(), 2); assert_eq!(entry.certificate_chain[0].content, leaf.der().as_ref());
        assert_eq!(entry.certificate_chain[1].content, ca.der().as_ref()); assert_eq!(entry.private_key, key.serialize_der());
        export_pem_bundle(&state.paths, &state.store, id, &out.with_extension("pem")).unwrap();
        assert_eq!(std::fs::read_to_string(out.with_extension("pem")).unwrap(), format!("{cert_pem}{}", key.serialize_pem()));
        export_der(&state.paths, &state.store, id, &out.with_extension("der")).unwrap();
        assert_eq!(std::fs::read(out.with_extension("der")).unwrap(), leaf.der().as_ref());
        assert!(!state.paths.certs().join("ca.crt").exists()); assert!(state.store.list_certs().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(cert_path).unwrap(), cert_pem); assert_eq!(std::fs::read_to_string(key_path).unwrap(), key.serialize_pem());
    }

    #[test]
    fn traditional_rsa_and_ec_keys_export_as_valid_pkcs8() {
        use rsa::{pkcs1::EncodeRsaPrivateKey, pkcs8::EncodePrivateKey};
        use p256::pkcs8::DecodePrivateKey;
        let (_temp, state) = fixture();
        let rsa = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let rsa_pkcs8 = rsa.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        let rsa_pem = rsa.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
        let ec_key = rcgen::KeyPair::generate().unwrap();
        let ec = p256::SecretKey::from_pkcs8_pem(&ec_key.serialize_pem()).unwrap();
        let ec_pem = ec.to_sec1_pem(p256::pkcs8::LineEnding::LF).unwrap();
        for (id, pkcs8, traditional) in [("rsa", rsa_pkcs8.to_string(), rsa_pem.to_string()), ("ec", ec_key.serialize_pem(), ec_pem.to_string())] {
            let key = rcgen::KeyPair::from_pem(&pkcs8).unwrap();
            let cert = rcgen::CertificateParams::new(vec!["traditional.example.com".into()]).unwrap().self_signed(&key).unwrap();
            let (crt, private) = crate::certs::imported_paths(&state.paths, id).unwrap(); std::fs::create_dir_all(crt.parent().unwrap()).unwrap();
            std::fs::write(crt, cert.pem()).unwrap(); std::fs::write(private, &traditional).unwrap();
            let material = export_material(&state.paths, &state.store, &format!("imported:{id}"), true).unwrap();
            let normalized = rustls::pki_types::PrivatePkcs8KeyDer::from(material.key_der.unwrap());
            let certified = rustls::sign::CertifiedKey::from_der(material.chain, normalized.into(), &rustls::crypto::ring::default_provider()).unwrap();
            certified.keys_match().unwrap();
            let output = state.paths.base.join(format!("{id}.pfx"));
            export_pfx(&state.paths, &state.store, &format!("imported:{id}"), "", &output).unwrap();
            let p12 = p12_keystore::KeyStore::from_pkcs12(&std::fs::read(output).unwrap(), "", p12_keystore::Pkcs12ImportPolicy::Strict).unwrap();
            let converted = rcgen::KeyPair::try_from(p12.private_key_chain().unwrap().1.key().as_der()).unwrap();
            assert_eq!(converted.public_key_der(), key.public_key_der());
            let output = state.paths.base.join(format!("{id}.jks"));
            export_jks(&state.paths, &state.store, &format!("imported:{id}"), "fixture", &output).unwrap();
            let mut jks = jks::KeyStore::new(); jks.load(std::fs::File::open(output).unwrap(), b"fixture").unwrap();
            let converted = rcgen::KeyPair::try_from(jks.get_private_key_entry(&material.subject, b"fixture").unwrap().private_key.as_slice()).unwrap();
            assert_eq!(converted.public_key_der(), key.public_key_der());
        }
    }

    #[test]
    fn invalid_export_material_never_replaces_existing_download_and_der_needs_no_key() {
        let (_temp, state) = fixture(); let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["expired.example.com".into()]).unwrap();
        params.not_before = OffsetDateTime::now_utc() - time::Duration::days(30);
        params.not_after = OffsetDateTime::now_utc() - time::Duration::days(1);
        let cert = params.self_signed(&key).unwrap();
        let (crt, private) = crate::certs::imported_paths(&state.paths, "archive").unwrap(); std::fs::create_dir_all(crt.parent().unwrap()).unwrap();
        std::fs::write(&crt, cert.pem()).unwrap(); std::fs::write(&private, key.serialize_pem()).unwrap();
        let out = state.paths.base.join("download");
        export_pfx(&state.paths, &state.store, "imported:archive", "", &out).unwrap(); // 过期证书可归档。
        std::fs::write(&private, rcgen::KeyPair::generate().unwrap().serialize_pem()).unwrap();
        std::fs::write(&out, b"previous-download").unwrap();
        assert_eq!(export_pfx(&state.paths, &state.store, "imported:archive", "", &out).unwrap_err().code, "CERT_KEY_MISMATCH");
        assert_eq!(export_jks(&state.paths, &state.store, "imported:archive", "fixture", &out).unwrap_err().code, "CERT_KEY_MISMATCH");
        assert_eq!(export_pem_bundle(&state.paths, &state.store, "imported:archive", &out).unwrap_err().code, "CERT_KEY_MISMATCH");
        assert_eq!(std::fs::read(&out).unwrap(), b"previous-download");
        std::fs::remove_file(&private).unwrap(); export_der(&state.paths, &state.store, "imported:archive", &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), cert.der().as_ref());
        for invalid in ["", "-----BEGIN CERTIFICATE-----\nbm90LXg1MDk=\n-----END CERTIFICATE-----\n"] {
            std::fs::write(&crt, invalid).unwrap();
            assert!(export_der(&state.paths, &state.store, "imported:archive", &out).is_err());
            assert_eq!(std::fs::read(&out).unwrap(), cert.der().as_ref());
        }
        assert_eq!(export_der(&state.paths, &state.store, "imported:../escape", &out).unwrap_err().code, "BAD_CERT_ID");
        std::fs::write(&crt, cert.pem()).unwrap();
        assert_eq!(export_der(&state.paths, &state.store, "imported:archive", &crt).unwrap_err().code, "CERT_EXPORT_TARGET");
        let folder = state.paths.base.join("target-folder"); std::fs::create_dir(&folder).unwrap();
        let before = std::fs::read_dir(&state.paths.base).unwrap().count();
        assert!(export_der(&state.paths, &state.store, "imported:archive", &folder).is_err());
        assert_eq!(std::fs::read_dir(&state.paths.base).unwrap().count(), before);
        let mut params = rcgen::CertificateParams::new(vec!["unicode.example.com".into()]).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, "导出🔑证书");
        let unicode = params.self_signed(&key).unwrap();
        std::fs::write(&crt, unicode.pem()).unwrap(); std::fs::write(&private, key.serialize_pem()).unwrap();
        assert_eq!(export_pfx(&state.paths, &state.store, "imported:archive", "fixture", &out).unwrap_err().code, "PFX_CERT_NAME");
        assert_eq!(std::fs::read(&out).unwrap(), cert.der().as_ref());
        export_jks(&state.paths, &state.store, "imported:archive", "fixture", &out).unwrap();
        let mut jks = jks::KeyStore::new(); jks.load(std::fs::File::open(&out).unwrap(), b"fixture").unwrap();
        assert_eq!(jks.get_private_key_entry("imported:archive", b"fixture").unwrap().certificate_chain[0].content, unicode.der().as_ref());
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
        let _sites = crate::sites::SITE_CHANGES.lock();
        let _operation = self.manager.lifecycle.lock();
        let mut requested = vec![domain.to_string()];
        requested.extend_from_slice(sans);
        let mut domains = normalize_domains(&requested)?;
        let sites = self.store.list_sites()?;
        for site in &sites {
            if site.https && site.runtime.uses_default_certificate() && site.domains.first().is_some_and(|d| d.eq_ignore_ascii_case(&domains[0])) {
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
        let _sites = crate::sites::SITE_CHANGES.lock();
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
        let _sites = crate::sites::SITE_CHANGES.lock();
        let _operation = self.manager.lifecycle.lock();
        let _files = CERT_FILES.lock();
        if id == "ca" { return Err(AppError::new("CERT_DELETE_UNSUPPORTED", "这里只能删除本地签发的站点证书")); }
        let cert = self.store.list_certs()?.into_iter().find(|c| c.id == id)
            .ok_or_else(|| AppError::new("NOT_FOUND", "证书不存在"))?;
        if cert.kind != "site" {
            return Err(AppError::new("CERT_DELETE_UNSUPPORTED", "这里只能删除本地签发的站点证书"));
        }
        let primary = normalize_domains(&[cert.subject.clone()])?.remove(0);
        let users: Vec<_> = self.store.list_sites()?.into_iter().filter(|site|
            site.runtime.acme_cert_id.as_deref() == Some(&format!("acme-{primary}"))
                || (site.https && site.runtime.uses_default_certificate() && site.domains.first().is_some_and(|d| d.eq_ignore_ascii_case(&primary)))
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

struct ExportMaterial {
    subject: String,
    cert_pem: String,
    chain: Vec<rustls::pki_types::CertificateDer<'static>>,
    key_pem: Option<String>,
    key_der: Option<Vec<u8>>,
}

// PKCS#12 别名使用 BMPString，JKS 的库字符串编码不支持补充平面字符。
// 证书主体不修改；仅在格式元数据无法表示名称时使用稳定的 ASCII 标识作别名。
fn export_alias(subject: &str, cert_id: &str) -> String {
    if subject.chars().any(|c| c == '\0' || c as u32 > 0xffff) { cert_id.to_ascii_lowercase() }
    else { subject.to_lowercase() }
}

/// imported: 前缀区分无数据库记录的导入证书；本地/ACME 的 cert-/acme- 标识保持兼容。
/// 导出用于迁移和归档，因此允许过期证书；含私钥的格式必须先核对真实材料匹配。
fn export_material(paths: &Paths, store: &Store, cert_id: &str, include_key: bool) -> Result<ExportMaterial> {
    use rustls::pki_types::{pem::PemObject, PrivateKeyDer};
    use rsa::pkcs8::{der::Encode, spki::SubjectPublicKeyInfoRef, PrivateKeyInfo};
    let (cert_path, key_path) = if let Some(id) = cert_id.strip_prefix("imported:") {
        let (cert, key) = crate::certs::imported_paths(paths, id)?;
        (cert, Some(key))
    } else {
        let record = if cert_id == "ca" { read_ca_record(paths)? } else {
            store.list_certs()?.into_iter().find(|c| c.id == cert_id)
                .ok_or_else(|| AppError::new("NOT_FOUND", "证书不存在，请刷新列表"))?
        };
        (std::path::PathBuf::from(record.cert_path), record.key_path.map(std::path::PathBuf::from))
    };
    let cert_pem = crate::certs::read_managed_pem(&cert_path)?;
    let chain = crate::certs::parse_chain(&cert_pem)?;
    let (_, leaf) = x509_parser::parse_x509_certificate(chain[0].as_ref())
        .map_err(|_| AppError::new("CERT_PARSE_FAILED", "无法解析导出证书"))?;
    let subject = crate::certs::parse_pem_info(&cert_pem).map(|info| info.0).unwrap_or_else(|| cert_id.into());
    let (key_pem, key_der) = if include_key {
        let path = key_path.ok_or_else(|| AppError::new("CERT_EXPORT_NO_KEY", "证书没有私钥，请选择仅导出证书的 DER 格式"))?;
        let pem = crate::certs::read_managed_pem(&path)?;
        crate::certs::check_pair(&cert_pem, &pem)?;
        let key = PrivateKeyDer::from_pem_slice(pem.as_bytes())
            .map_err(|_| AppError::new("NOT_A_KEY", "无法解析导出私钥"))?;
        // JKS/PKCS#12 需要 PKCS#8。用已核对的叶证书算法参数包装 PKCS#1/SEC1，
        // 保留 RSA 参数及 EC 曲线，不把传统 PEM 的裸 DER 冒充 PKCS#8。
        let der = match &key {
            PrivateKeyDer::Pkcs8(key) => key.secret_pkcs8_der().to_vec(),
            PrivateKeyDer::Pkcs1(_) | PrivateKeyDer::Sec1(_) => {
                let spki = SubjectPublicKeyInfoRef::try_from(leaf.public_key().raw)
                    .map_err(|_| AppError::new("CERT_EXPORT_KEY", "无法读取证书的密钥算法"))?;
                PrivateKeyInfo::new(spki.algorithm, key.secret_der()).to_der()
                    .map_err(|_| AppError::new("CERT_EXPORT_KEY", "无法将私钥转换为 PKCS#8"))?
            }
            _ => return Err(AppError::new("CERT_EXPORT_KEY", "此私钥格式不支持导出")),
        };
        (Some(pem), Some(der))
    } else { (None, None) };
    Ok(ExportMaterial { subject, cert_pem, chain, key_pem, key_der })
}

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

/// 把某张本机证书（含私钥与证书链）导出为 PKCS#12 (.pfx)。
/// Windows IIS / 部分设备导入只认这个格式。password 可为空（空密码保护）。
pub fn export_pfx(
    paths: &Paths,
    store: &Store,
    cert_id: &str,
    password: &str,
    out_path: &std::path::Path,
) -> Result<String> {
    let _work = crate::BackgroundWork::begin("导出 PFX 证书")?;
    let _activity = crate::paths::DataDirActivity::shared(&paths.base)?;
    let _files = CERT_FILES.lock();
    // PKCS#12 的 BMPString 密码不支持补充平面字符；给出可操作提示，不等到 ASN.1 写出失败。
    if password.chars().any(|c| c == '\0' || c as u32 > 0xffff) {
        return Err(AppError::new("PFX_PASSWORD", "PFX 密码不支持部分扩展字符（如 emoji），请改用常用中文、字母或数字"));
    }
    let material = export_material(paths, store, cert_id, true)?;
    let key = p12_keystore::PrivateKey::from_der(material.key_der.as_deref().unwrap())
        .map_err(|e| AppError::new("PFX_DECODE", format!("私钥解析失败：{e}")))?;
    let certs = material.chain.iter().map(|der| p12_keystore::Certificate::from_der(der.as_ref())
        .map_err(|e| AppError::new("PFX_DECODE", format!("证书解析失败：{e}")))).collect::<Result<Vec<_>>>()?;
    // 当前库还会把链中每张证书的完整主体写为 BMPString friendlyName，不能只替换条目别名。
    if certs.iter().any(|cert| cert.subject().chars().any(|c| c == '\0' || c as u32 > 0xffff)) {
        return Err(AppError::new("PFX_CERT_NAME", "PFX 暂不支持此证书名称中的扩展字符，请选择 PEM、JKS 或 DER 格式")
            .with_hint("原证书和已有导出文件保持不变。"));
    }

    let alias = export_alias(&material.subject, cert_id);
    let chain = p12_keystore::PrivateKeyChain::new(alias.clone(), key, certs);
    let mut store12 = p12_keystore::KeyStore::new();
    store12.add_entry(
        &alias,
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
    let _work = crate::BackgroundWork::begin("导出 DER 证书")?;
    let _activity = crate::paths::DataDirActivity::shared(&paths.base)?;
    let _files = CERT_FILES.lock();
    let material = export_material(paths, store, cert_id, false)?;
    write_certificate_export(paths, store, out_path, material.chain[0].as_ref())
}

// OpenJDK 对 Java char[]（UTF-16 code units）逐个写大端字节。jks crate 默认按
// UTF-8 的每个字节补零，只对 ASCII 密码兼容；输入来自 Rust str，需显式转换。
fn jks_password_bytes(password: &[u8]) -> Vec<u8> {
    String::from_utf8_lossy(password).encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// 导出 JKS（Java Keystore，Tomcat 等 Java 系中间件用）。
/// 沿用 keytool 的至少 6 个字符要求，避免生成后续工具拒绝使用的密码。
pub fn export_jks(
    paths: &Paths,
    store: &Store,
    cert_id: &str,
    password: &str,
    out_path: &std::path::Path,
) -> Result<String> {
    let _work = crate::BackgroundWork::begin("导出 JKS 证书")?;
    let _activity = crate::paths::DataDirActivity::shared(&paths.base)?;
    let _files = CERT_FILES.lock();
    if password.chars().count() < 6 {
        return Err(AppError::new(
            "JKS_PASSWORD",
            "JKS 密码至少 6 个字符",
        ));
    }

    let material = export_material(paths, store, cert_id, true)?;
    let chain = material.chain.iter().map(|der| jks::Certificate { cert_type: "X509".into(), content: der.to_vec() }).collect();
    let alias = export_alias(&material.subject, cert_id);

    let mut ks = jks::KeyStore::with_options(jks::KeyStoreOptions { password_bytes: jks_password_bytes, ..Default::default() });
    ks.set_private_key_entry(
        &alias,
        jks::PrivateKeyEntry {
            creation_time: std::time::SystemTime::now(),
            private_key: material.key_der.unwrap(),
            certificate_chain: chain,
        },
        password.as_bytes(),
    )
    .map_err(|e| AppError::new("JKS_BUILD", format!("构建 JKS 失败：{e}")))?;
    let mut buf = Vec::new();
    // store() 忽略 Options.password_bytes，仍按 UTF-8 每字节补零；使用库的 Encoder
    // 显式传入 Java 密码摘要，并复用已加密的条目与标准文件结构。
    let serialize = (|| -> jks::Result<()> {
        let mut encoder = jks::encoder::Encoder::new(&mut buf);
        encoder.update_digest(&jks_password_bytes(password.as_bytes()));
        encoder.update_digest(jks::common::WHITENER_MESSAGE);
        encoder.write_u32(jks::common::MAGIC)?;
        encoder.write_u32(jks::common::VERSION_02)?;
        encoder.write_u32(1)?;
        encoder.write_private_key_entry(&alias, &ks.get_raw_private_key_entry(&alias)?)?;
        encoder.write_digest()
    })();
    serialize.map_err(|e| AppError::new("JKS_BUILD", format!("序列化 JKS 失败：{e}")))?;

    write_certificate_export(paths, store, out_path, &buf)
}

/// 导出 PEM 打包（证书链 + 私钥 拼一个 .pem，nginx / 迁移别家最顺手）
pub fn export_pem_bundle(
    paths: &Paths,
    store: &Store,
    cert_id: &str,
    out_path: &std::path::Path,
) -> Result<String> {
    let _work = crate::BackgroundWork::begin("导出 PEM 证书")?;
    let _activity = crate::paths::DataDirActivity::shared(&paths.base)?;
    let _files = CERT_FILES.lock();
    let material = export_material(paths, store, cert_id, true)?;
    let mut bundle = material.cert_pem;
    if !bundle.ends_with('\n') {
        bundle.push('\n');
    }
    bundle.push_str(material.key_pem.as_deref().unwrap());
    write_certificate_export(paths, store, out_path, bundle.as_bytes())
}
