//! 本地 PKI：根 CA + 站点证书（rcgen 签发）+ 信任导入。

use crate::error::{AppError, Result};
use crate::model::CertRecord;
use crate::paths::{write_with_backup, Paths};
use crate::store::Store;
use time::OffsetDateTime;

pub const CA_DAYS: i64 = 3650;
pub const SITE_DAYS: i64 = 30;

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
    let ca_key = paths.certs().join("ca.key");
    let ca_crt = paths.certs().join("ca.crt");
    if ca_key.exists() && ca_crt.exists() {
        return Ok(());
    }
    let subject = "NiceServBay Local Root CA";
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
        .push(rcgen::DnType::OrganizationName, "NiceServBay");
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
    std::fs::write(&ca_key, key_pair.serialize_pem())
        .map_err(|e| AppError::io("写入 CA 私钥", e))?;
    std::fs::write(&ca_crt, certified.pem())
        .map_err(|e| AppError::io("写入 CA 证书", e))?;
    Ok(())
}

/// 站点证书签发（SAN 支持多域名），写入 certs/sites/{primary}.crt/.key
pub fn issue_site_cert(paths: &Paths, store: &Store, domains: &[String]) -> Result<CertRecord> {
    ensure_ca(paths)?;
    if domains.is_empty() {
        return Err(AppError::new("BAD_DOMAINS", "至少需要一个域名"));
    }
    let primary = &domains[0];
    let ca_key_pem = std::fs::read_to_string(paths.certs().join("ca.key"))
        .map_err(|e| AppError::io("读取 CA 私钥", e))?;
    let ca_crt_pem = std::fs::read_to_string(paths.certs().join("ca.crt"))
        .map_err(|e| AppError::io("读取 CA 证书", e))?;
    let ca_key = rcgen::KeyPair::from_pem(&ca_key_pem)
        .map_err(|e| AppError::internal("解析 CA 私钥", e.to_string()))?;
    // 从持久化的 CA 证书提取参数，并用 CA 密钥重建等价 issuer（同 key/同 subject，
    // signed_by 只用到 issuer 的 DN + 密钥，因此链式校验与磁盘上的 CA 一致）
    let ca_params = rcgen::CertificateParams::from_ca_cert_pem(&ca_crt_pem)
        .map_err(|e| AppError::internal("解析 CA 证书", e.to_string()))?;
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
    let not_after = now_plus(SITE_DAYS);
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

    let crt_path = paths.certs().join("sites").join(format!("{primary}.crt"));
    let key_path = paths.certs().join("sites").join(format!("{primary}.key"));
    std::fs::create_dir_all(paths.certs().join("sites"))?;
    write_with_backup(&crt_path, &certified.pem(), &paths.backup())?;
    write_with_backup(&key_path, &key_pair.serialize_pem(), &paths.backup())?;

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
    store.save_cert(&record)?;
    Ok(record)
}

/// 信任根 CA：Windows certutil（需管理员） / macOS security
pub fn trust_ca(paths: &Paths) -> Result<()> {
    ensure_ca(paths)?;
    let ca = paths.certs().join("ca.crt");
    #[cfg(windows)]
    {
        // 先直接尝试（若已提权则成功）；失败则走 UAC 提权
        if let Ok(o) = std::process::Command::new("certutil")
            .args(["-addstore", "-f", "Root"])
            .arg(&ca)
            .output()
        {
            if o.status.success() {
                return Ok(());
            }
        }
        platform::run_elevated("certutil", &["-addstore", "-f", "Root", &ca.to_string_lossy()])
            .map_err(AppError::from)?;
        Ok(())
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
        .map_err(AppError::from)
    }
}

/// CA 是否已信任（Windows: certutil 按 CN 查找 Root store）
pub fn ca_trusted(_paths: &Paths) -> bool {
    #[cfg(windows)]
    {
        let Ok(out) = std::process::Command::new("certutil")
            .args(["-verify", "-store", "Root", "NiceServBay Local Root CA"])
            .output()
        else {
            return false;
        };
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        text.contains("NiceServBay")
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 证书列表（CA 置顶并附带信任状态）
pub fn list_certs(paths: &Paths, store: &Store) -> Result<Vec<CertRecord>> {
    let mut list = store.list_certs()?;
    ensure_ca(paths)?;
    if !list.iter().any(|c| c.kind == "ca") {
        let rec = CertRecord {
            id: "ca".into(),
            kind: "ca".into(),
            subject: "NiceServBay Local Root CA".into(),
            sans: vec![],
            not_before: to_ms(now_minus(1)),
            not_after: to_ms(now_plus(CA_DAYS)),
            cert_path: paths.certs().join("ca.crt").to_string_lossy().to_string(),
            key_path: Some(paths.certs().join("ca.key").to_string_lossy().to_string()),
            trusted: Some(false),
        };
        store.save_cert(&rec)?;
        list.insert(0, rec);
    }
    let trusted = ca_trusted(paths);
    for c in list.iter_mut() {
        if c.kind == "ca" {
            c.trusted = Some(trusted);
        }
    }
    Ok(list)
}

/// 逐个站点补齐缺失/过期的 HTTPS 证书（「修复向导 → 重建证书」用）。
/// 只对 https 已开启、且证书缺失或 30 天内到期的站点重新签发；
/// 返回重新签发的域名列表。绝不签发与站点无关的占位证书。
pub fn reissue_missing_site_certs(paths: &Paths, store: &Store) -> Result<Vec<String>> {
    let sites = store.list_sites()?;
    let now = to_ms(OffsetDateTime::now_utc());
    let soon = now + 30 * 86_400_000;
    let mut issued = Vec::new();
    for site in sites {
        if !site.https || site.domains.is_empty() {
            continue;
        }
        let primary = &site.domains[0];
        let needs = match store
            .list_certs()?
            .into_iter()
            .find(|c| c.kind == "site" && c.subject == *primary)
        {
            Some(c) => c.not_after < soon,
            None => true,
        };
        if needs {
            issue_site_cert(paths, store, &site.domains)?;
            issued.push(primary.clone());
        }
    }
    Ok(issued)
}
