//! 证书健康检查与自定义证书导入。
//!
//! 检查真实证书的有效期、私钥匹配和站点 SAN 覆盖，安全导入及管理自定义证书。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::paths::Paths;

/// 剩余天数到多少就该提醒
pub const WARN_DAYS: i64 = 30;
/// 剩余天数到多少算紧急
pub const CRIT_DAYS: i64 = 7;

/// 一张证书的健康状况
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CertHealth {
    pub id: String,
    /// ca / site
    pub kind: String,
    pub subject: String,
    pub sans: Vec<String>,
    pub not_after: i64,
    /// 剩余天数；负数表示已过期
    pub days_left: i64,
    /// ok / warn / critical / expired
    pub status: String,
    /// 证书文件是否还在（用户手删过就会丢）
    pub file_present: bool,
    /// 是否有站点在用这张证书（用于判断「能不能直接删」）
    pub used_by_sites: Vec<String>,
    /// 该站点当前需要的域名，但与证书 SAN 不匹配的
    #[serde(default)]
    pub missing_sans: Vec<String>,
    /// 给用户的下一步建议
    pub advice: String,
}

/// 汇总
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CertReport {
    pub certs: Vec<CertHealth>,
    pub expired: usize,
    pub critical: usize,
    pub warning: usize,
    /// 根 CA 是否已被系统信任
    pub ca_trusted: bool,
    /// 检查时间
    pub checked_at: i64,
}

/// 依据剩余天数判定状态
pub fn status_for(days_left: i64) -> &'static str {
    if days_left < 0 {
        "expired"
    } else if days_left <= CRIT_DAYS {
        "critical"
    } else if days_left <= WARN_DAYS {
        "warn"
    } else {
        "ok"
    }
}

/// 按磁盘文件体检，不使用数据库缓存的有效期，也不在体检时生成/覆盖证书。
pub fn report(paths: &Paths, store: &crate::store::Store) -> Result<CertReport> {
    let _files = crate::tls::CERT_FILES.lock();
    let sites = store.list_sites()?;
    let mut certs = store.list_certs()?;
    certs.retain(|c| c.kind != "ca");
    certs.insert(0, crate::model::CertRecord {
        id: "ca".into(), kind: "ca".into(), subject: "NiceEnv Local Root CA".into(), sans: vec![],
        not_before: 0, not_after: 0, cert_path: paths.certs().join("ca.crt").to_string_lossy().into(),
        key_path: Some(paths.certs().join("ca.key").to_string_lossy().into()), trusted: None,
    });
    let now = chrono::Utc::now().timestamp();
    let mut out = Vec::new();
    for c in certs {
        let users: Vec<_> = sites.iter().filter(|s| s.https && s.runtime.imported_cert_id.is_none()
            && s.domains.first().is_some_and(|domain| domain.eq_ignore_ascii_case(&c.subject))).collect();
        let mut subject = c.subject;
        let mut sans = c.sans;
        let mut not_after = c.not_after / 1000;
        let mut not_before = c.not_before / 1000;
        let checked: Result<()> = (|| {
            let pem = read_pem(Path::new(&c.cert_path))?;
            let info = parse_pem_info(&pem).ok_or_else(|| AppError::new("CERT_PARSE_FAILED", "证书内容损坏，无法解析"))?;
            subject = info.0; sans = info.1; not_before = info.2; not_after = info.3;
            let key = c.key_path.as_deref().ok_or_else(|| AppError::new("NOT_A_KEY", "没有匹配的私钥文件"))?;
            check_pair(&pem, &read_pem(Path::new(key))?)
        })();
        let file_present = Path::new(&c.cert_path).is_file()
            && c.key_path.as_deref().is_some_and(|key| Path::new(key).is_file());
        let missing: Vec<_> = users.iter().flat_map(|s| &s.domains)
            .filter(|domain| !covers_domain(&sans, domain)).cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        let days_left = (not_after - now).div_euclid(86_400);
        let repair = if c.kind == "ca" { "请恢复匹配的根 CA 证书与私钥；过期根 CA 需要重新配置。" }
            else if c.kind == "acme" { "请在自动签发页续签并重新部署。" }
            else { "请重新签发本地证书。" };
        let (status, advice) = if let Err(error) = checked {
            ("invalid".into(), format!("{}。{repair}", error.message))
        } else if not_before > now {
            ("invalid".into(), format!("证书尚未生效。{repair}"))
        } else if not_after <= now {
            ("expired".into(), format!("证书已过期。{repair}"))
        } else if !missing.is_empty() {
            ("invalid".into(), format!("证书未覆盖域名 {}。{repair}", missing.join("、")))
        } else {
            let status = if c.kind == "site" && days_left > CRIT_DAYS { "ok" } else { status_for(days_left) };
            let advice = if status == "ok" { String::new() } else { format!("还有 {days_left} 天到期。{repair}") };
            (status.into(), advice)
        };
        out.push(CertHealth {
            id: c.id, kind: c.kind, subject, sans, not_after, days_left, status, file_present,
            used_by_sites: users.iter().map(|s| s.name.clone()).collect(), missing_sans: missing, advice,
        });
    }
    for cert in list_imported(paths, store)? {
        let missing: Vec<_> = sites.iter().filter(|s| s.https && s.runtime.imported_cert_id.as_deref() == Some(&cert.id))
            .flat_map(|s| &s.domains).filter(|d| !covers_domain(&cert.sans, d)).cloned()
            .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        let status = if cert.not_after > 0 && cert.not_after <= now { "expired" }
            else if !cert.usable || !missing.is_empty() { "invalid" } else { status_for(cert.days_left) };
        let advice = cert.problem.clone().unwrap_or_else(|| {
            if !missing.is_empty() { format!("所选证书未覆盖域名 {}", missing.join("、")) }
            else if status != "ok" { format!("还有 {} 天到期，请导入续签证书并更新站点的证书选择。", cert.days_left) }
            else { String::new() }
        });
        out.push(CertHealth {
            id: cert.id, kind: "imported".into(), subject: cert.subject, sans: cert.sans,
            not_after: cert.not_after, days_left: cert.days_left, status: status.into(),
            file_present: Path::new(&cert.cert_path).is_file() && Path::new(&cert.key_path).is_file(),
            used_by_sites: cert.used_by_sites, missing_sans: missing, advice,
        });
    }
    out.sort_by_key(|c| (c.advice.is_empty(), c.days_left));
    Ok(CertReport {
        expired: out.iter().filter(|c| c.status == "expired").count(),
        critical: out.iter().filter(|c| c.status == "critical" || c.status == "invalid").count(),
        warning: out.iter().filter(|c| c.status == "warn").count(),
        ca_trusted: crate::tls::ca_trusted(paths), checked_at: now, certs: out,
    })
}

/* ================= 自定义证书导入 ================= */

/// 用户自带的一对证书（公司 CA 签的、或买的通配符证书）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedCert {
    pub id: String,
    pub usable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    #[serde(default)]
    pub used_by_sites: Vec<String>,
    /// 落地后的证书路径
    pub cert_path: String,
    pub key_path: String,
    pub subject: String,
    pub sans: Vec<String>,
    pub not_before: i64,
    pub not_after: i64,
    pub days_left: i64,
}

/// 使用已有 X.509 解析器读取第一张证书；有效期统一为 Unix 秒。
pub fn parse_pem_info(pem: &str) -> Option<(String, Vec<String>, i64, i64)> {
    let chain = parse_chain(pem).ok()?;
    cert_info(chain.first()?.as_ref()).ok()
}

// 保留旧的纯函数 API，现有源码测试仍覆盖它们；实际导入路径全部使用上面的 X.509 解析。
#[cfg(test)]
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}
#[cfg(test)]
fn parse_asn1_time(s: &str, generalized: bool) -> Option<i64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    let (year, rest) = if generalized {
        if digits.len() < 14 { return None; }
        (digits[0..4].parse::<i32>().ok()?, &digits[4..])
    } else {
        if digits.len() < 12 { return None; }
        let yy = digits[0..2].parse::<i32>().ok()?;
        (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &digits[2..])
    };
    let month = rest.get(0..2)?.parse::<u32>().ok()?;
    let day = rest.get(2..4)?.parse::<u32>().ok()?;
    let hour = rest.get(4..6)?.parse::<u32>().ok()?;
    let minute = rest.get(6..8)?.parse::<u32>().ok()?;
    let second = rest.get(8..10)?.parse::<u32>().ok()?;
    chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)?.and_utc().timestamp().into()
}
#[cfg(test)]
fn extract_ascii_strings(der: &[u8]) -> Vec<String> {
    let mut out = Vec::new(); let mut current = String::new();
    for byte in der.iter().copied().chain(std::iter::once(0)) {
        if (0x20..0x7f).contains(&byte) { current.push(byte as char); }
        else { if current.len() >= 4 { out.push(std::mem::take(&mut current)); } else { current.clear(); } }
    }
    out
}
#[cfg(test)]
fn looks_like_host(value: &str) -> bool {
    !value.is_empty() && value.len() <= 253 && value.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*' | '_')) && !value.chars().all(|c| c.is_ascii_digit())
}
#[cfg(test)]
fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    let body = pem.split("-----BEGIN CERTIFICATE-----").nth(1)?.split("-----END CERTIFICATE-----").next()?;
    base64_decode(&body.chars().filter(|c| !c.is_whitespace()).collect::<String>())
}

fn parse_chain(pem: &str) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject;
    let chain = rustls::pki_types::CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| AppError::new("CERT_PARSE_FAILED", format!("证书 PEM 无法解析：{e}")))?;
    if chain.is_empty() { return Err(AppError::new("NOT_A_CERT", "这个文件里没有 PEM 格式的证书")); }
    for cert in &chain {
        x509_parser::parse_x509_certificate(cert.as_ref())
            .map_err(|e| AppError::new("CERT_PARSE_FAILED", format!("证书内容无法解析：{e}")))?;
    }
    Ok(chain)
}

fn cert_info(der: &[u8]) -> Result<(String, Vec<String>, i64, i64)> {
    use x509_parser::extensions::GeneralName;
    let (_, cert) = x509_parser::parse_x509_certificate(der)
        .map_err(|e| AppError::new("CERT_PARSE_FAILED", format!("证书内容无法解析：{e}")))?;
    let mut sans = Vec::new();
    let extension = cert.subject_alternative_name()
        .map_err(|e| AppError::new("CERT_PARSE_FAILED", format!("SAN 无法解析：{e}")))?;
    if let Some(extension) = extension {
        for name in &extension.value.general_names {
            let name = match name {
                GeneralName::DNSName(name) => Some(name.to_ascii_lowercase()),
                GeneralName::IPAddress(bytes) if bytes.len() == 4 =>
                    Some(std::net::Ipv4Addr::from(<[u8; 4]>::try_from(*bytes).unwrap()).to_string()),
                GeneralName::IPAddress(bytes) if bytes.len() == 16 =>
                    Some(std::net::Ipv6Addr::from(<[u8; 16]>::try_from(*bytes).unwrap()).to_string()),
                _ => None,
            };
            if let Some(name) = name { if !sans.contains(&name) { sans.push(name); } }
        }
    }
    let subject = cert.subject().iter_common_name().next().and_then(|cn| cn.as_str().ok())
        .map(str::to_string).or_else(|| sans.first().cloned()).unwrap_or_else(|| cert.subject().to_string());
    Ok((subject, sans, cert.validity().not_before.timestamp(), cert.validity().not_after.timestamp()))
}

fn check_pair(cert_pem: &str, key_pem: &str) -> Result<()> {
    use rustls::pki_types::pem::PemObject;
    let chain = parse_chain(cert_pem)?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
        .map_err(|_| AppError::new("NOT_A_KEY", "无法解析私钥，请选择未加密的 PEM 私钥（PKCS#1、PKCS#8 或 SEC1）"))?;
    let certified = rustls::sign::CertifiedKey::from_der(chain, key, &rustls::crypto::ring::default_provider())
        .map_err(|_| AppError::new("CERT_KEY_MISMATCH", "证书与私钥不匹配，或私钥算法不受支持"))?;
    certified.keys_match().map_err(|_| AppError::new("CERT_KEY_MISMATCH", "证书与私钥不匹配"))
}

fn read_pem(path: &Path) -> Result<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| AppError::io("读取证书或私钥文件", e))?;
    if !file.metadata()?.is_file() { return Err(AppError::new("CERT_FILE", "请选择证书文件，不能选择目录")); }
    let mut text = String::new();
    file.take(4 * 1024 * 1024 + 1).read_to_string(&mut text)
        .map_err(|e| AppError::io("读取 PEM 文件", e))?;
    if text.len() > 4 * 1024 * 1024 { return Err(AppError::new("CERT_FILE_SIZE", "证书或私钥文件超过 4 MiB")); }
    Ok(text)
}

fn read_managed_pem(path: &Path) -> Result<String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| AppError::io("读取托管证书文件", e))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppError::new("CERT_FILE", "托管证书文件必须是普通文件，不能是软链接或目录"));
    }
    read_pem(path)
}

fn check_server_leaf(pem: &str) -> Result<()> {
    let chain = parse_chain(pem)?;
    let (_, leaf) = x509_parser::parse_x509_certificate(chain[0].as_ref())
        .map_err(|e| AppError::new("CERT_PARSE_FAILED", e.to_string()))?;
    if leaf.is_ca() { return Err(AppError::new("CERT_IS_CA", "请选择站点证书；根证书和中间 CA 不能作为站点证书导入")); }
    let eku = leaf.extended_key_usage().map_err(|e| AppError::new("CERT_PARSE_FAILED", e.to_string()))?;
    if eku.is_some_and(|eku| !eku.value.server_auth && !eku.value.any) {
        return Err(AppError::new("CERT_USAGE", "这张证书不允许用于 HTTPS 服务器"));
    }
    Ok(())
}

pub fn valid_imported_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 200 && id != "." && id != ".."
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
}

pub fn imported_paths(paths: &Paths, id: &str) -> Result<(PathBuf, PathBuf)> {
    if !valid_imported_id(id) { return Err(AppError::new("BAD_CERT_ID", "导入证书标识无效，请重新选择证书")); }
    Ok((
        crate::paths::checked_data_path(&paths.base, &format!("certs/imported/{id}.crt"))?,
        crate::paths::checked_data_path(&paths.base, &format!("certs/imported/{id}.key"))?,
    ))
}

/// DNS 通配符只覆盖一个标签；IP 与显式通配符站点要求精确 SAN。
pub fn covers_domain(sans: &[String], domain: &str) -> bool {
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    sans.iter().any(|san| {
        if san.eq_ignore_ascii_case(&domain) { return true; }
        if domain.parse::<std::net::IpAddr>().is_ok() || domain.starts_with("*.") { return false; }
        san.strip_prefix("*.").is_some_and(|suffix|
            domain.split_once('.').is_some_and(|(label, rest)| !label.is_empty() && rest.eq_ignore_ascii_case(suffix))
        )
    })
}

fn imported_entry(paths: &Paths, id: &str) -> ImportedCert {
    let mut entry = ImportedCert {
        id: id.into(), cert_path: paths.certs().join("imported").join(format!("{id}.crt")).to_string_lossy().into(),
        key_path: paths.certs().join("imported").join(format!("{id}.key")).to_string_lossy().into(),
        subject: id.into(), sans: vec![], not_before: 0, not_after: 0, days_left: 0,
        usable: false, problem: None, used_by_sites: vec![],
    };
    let checked: Result<()> = (|| {
        let (cert, key) = imported_paths(paths, id)?;
        let cert_pem = read_managed_pem(&cert)?;
        let (subject, sans, nb, na) = parse_pem_info(&cert_pem)
            .ok_or_else(|| AppError::new("CERT_PARSE_FAILED", "无法解析证书内容"))?;
        entry.subject = subject;
        entry.sans = sans;
        entry.not_before = nb;
        entry.not_after = na;
        let now = chrono::Utc::now().timestamp();
        entry.days_left = (na - now).div_euclid(86_400);
        check_server_leaf(&cert_pem)?;
        check_pair(&cert_pem, &read_managed_pem(&key)?)?;
        if nb > now { return Err(AppError::new("CERT_NOT_YET_VALID", "证书尚未生效")); }
        if na <= now { return Err(AppError::new("CERT_EXPIRED", "证书已过期，请导入续签后的证书")); }
        if entry.sans.is_empty() { return Err(AppError::new("CERT_NO_SAN", "证书没有 DNS 或 IP SAN，不能用于站点 HTTPS")); }
        Ok(())
    })();
    match checked {
        Ok(()) => entry.usable = true,
        Err(error) => entry.problem = Some(error.message),
    }
    entry
}

/// 复制而非移动源文件。先验证密钥，两个临时文件写好后才发布；同名导入不覆盖旧证书。
pub fn import_cert_pair(paths: &Paths, cert_src: &Path, key_src: &Path) -> Result<ImportedCert> {
    use std::io::Write;
    let _files = crate::tls::CERT_FILES.lock();
    let cert_pem = read_pem(cert_src)?;
    if !cert_pem.contains("BEGIN CERTIFICATE") {
        return Err(AppError::new("NOT_A_CERT", "这个文件里没有 PEM 格式的证书")
            .with_hint("请选择 .crt / .pem 证书文件，而不是私钥或二进制 PFX。"));
    }
    let key_pem = read_pem(key_src)?;
    if !key_pem.contains("PRIVATE KEY") {
        return Err(AppError::new("NOT_A_KEY", "这个文件里没有 PEM 格式的私钥")
            .with_hint("请选择 .key 私钥文件；若只有 .pfx，需要先转换成 PEM。"));
    }
    check_pair(&cert_pem, &key_pem)?;
    check_server_leaf(&cert_pem)?;
    let dir = crate::paths::checked_data_path(&paths.base, "certs/imported")?;
    std::fs::create_dir_all(&dir)?;
    let id = format!("cert-{:016x}{:016x}", rand::random::<u64>(), rand::random::<u64>());
    let (cert_dst, key_dst) = imported_paths(paths, &id)?;
    let mut cert = tempfile::NamedTempFile::new_in(&dir)?;
    let mut key = tempfile::NamedTempFile::new_in(&dir)?;
    cert.write_all(cert_pem.as_bytes())?;
    key.write_all(key_pem.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        key.as_file().set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    cert.as_file().sync_all()?;
    key.as_file().sync_all()?;
    cert.persist_noclobber(&cert_dst).map_err(|e| AppError::io("保存导入证书", e.error))?;
    if let Err(error) = key.persist_noclobber(&key_dst) {
        std::fs::remove_file(&cert_dst).map_err(|e| AppError::io("私钥保存失败，清理未完成的证书失败", e))?;
        return Err(AppError::io("保存导入私钥", error.error));
    }
    Ok(imported_entry(paths, &id))
}

/// 目录读取错误如实返回；损坏证书与孤立私钥保留在列表并标明原因，便于清理。
pub fn list_imported(paths: &Paths, store: &crate::store::Store) -> Result<Vec<ImportedCert>> {
    let _files = crate::tls::CERT_FILES.lock();
    let dir = crate::paths::checked_data_path(&paths.base, "certs/imported")?;
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => read,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(AppError::io("读取导入证书目录", error)),
    };
    let mut ids = std::collections::BTreeSet::new();
    for entry in read {
        let entry = entry?;
        if entry.file_type()?.is_symlink() { continue; }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e == "crt" || e == "key") {
            if let Some(id) = path.file_stem().and_then(|s| s.to_str()) { ids.insert(id.to_string()); }
        }
    }
    let sites = store.list_sites()?;
    let mut out: Vec<_> = ids.into_iter().map(|id| {
        let mut entry = imported_entry(paths, &id);
        entry.used_by_sites = sites.iter().filter(|s| s.runtime.imported_cert_id.as_deref() == Some(&id))
            .map(|s| s.name.clone()).collect();
        entry
    }).collect();
    out.sort_by_key(|c| (c.usable, c.days_left));
    Ok(out)
}

pub fn validate_site_certificate(paths: &Paths, site: &crate::model::Site) -> Result<()> {
    if !site.https { return Ok(()); }
    let Some(id) = &site.runtime.imported_cert_id else { return Ok(()) };
    validate_imported_domains(paths, id, &site.domains)
}

pub fn validate_imported_domains(paths: &Paths, id: &str, domains: &[String]) -> Result<()> {
    let _files = crate::tls::CERT_FILES.lock();
    let cert = imported_entry(paths, id);
    if !cert.usable {
        return Err(AppError::new("CERT_UNUSABLE", cert.problem.unwrap_or_else(|| "证书不可用".into()))
            .with_hint("请重新导入有效证书，或改为使用本地根 CA 签发。"));
    }
    let missing: Vec<_> = domains.iter().filter(|d| !covers_domain(&cert.sans, d)).cloned().collect();
    if !missing.is_empty() {
        return Err(AppError::new("CERT_DOMAIN_MISMATCH", format!("所选证书未覆盖域名：{}", missing.join("、")))
            .with_hint("请选择覆盖所有站点域名的证书，或修改站点域名。"));
    }
    Ok(())
}

/// 保留目录边界与站点引用保护；两份文件先暂存，移动失败时恢复。
pub fn delete_imported(paths: &Paths, store: &crate::store::Store, cert_path: &str) -> Result<()> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let _files = crate::tls::CERT_FILES.lock();
    let target = std::path::absolute(cert_path)?;
    let id = target.file_stem().and_then(|s| s.to_str())
        .ok_or_else(|| AppError::new("BAD_CERT_ID", "证书路径无效"))?;
    let (cert, key) = imported_paths(paths, id)?;
    if target.extension().and_then(|e| e.to_str()) != Some("crt")
        || target.parent().map(std::fs::canonicalize).transpose()? != cert.parent().map(std::fs::canonicalize).transpose()?
    { return Err(AppError::new("FORBIDDEN", "只能删除导入目录内的证书")); }
    let used_by: Vec<_> = store.list_sites()?.into_iter()
        .filter(|site| site.runtime.imported_cert_id.as_deref() == Some(id)).map(|site| site.name).collect();
    if !used_by.is_empty() {
        return Err(AppError::new("CERT_IN_USE", format!("证书仍被站点 {} 选择使用", used_by.join("、")))
            .with_hint("请先到站点设置更换证书，再删除。关闭 HTTPS 不会解除已保存的证书选择。"));
    }
    let staging = tempfile::tempdir_in(paths.certs())?;
    let mut moved = Vec::new();
    let result: Result<()> = (|| {
        for (index, source) in [cert, key].iter().enumerate() {
            match std::fs::symlink_metadata(source) {
                Ok(meta) if meta.is_file() => {
                    let dest = staging.path().join(index.to_string());
                    std::fs::rename(source, &dest)?;
                    moved.push((source.clone(), dest));
                }
                Ok(_) => return Err(AppError::new("CERT_FILE", "证书路径不是普通文件，未删除")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        for (source, dest) in moved.iter().rev() {
            if let Err(restore) = std::fs::rename(dest, source) {
                let recovery = staging.keep();
                return Err(AppError::new("CERT_ROLLBACK_FAILED", "删除失败，部分证书文件需要恢复")
                    .with_hint(format!("恢复目录：{}", recovery.display()))
                    .with_detail(format!("{}；{restore}", error.message)));
            }
        }
        return Err(error);
    }
    staging.close().map_err(|e| AppError::io("证书已移出列表，但暂存文件清理失败", e))?;
    Ok(())
}

/// 给 CertHealth 用：从 CertRecord 生成一句「该不该管」的结论
pub fn is_actionable(h: &CertHealth) -> bool {
    !h.file_present || !h.advice.is_empty()
}

/* ================= 从文件夹批量导入（certd 迁移的兜底路：没有 db.json、只有证书文件） ================= */

/// 目录批量导入结果
#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DirImportResult {
    pub imported: Vec<ImportedCert>,
    /// 没配成对的文件（只有证书没私钥 / 只剩私钥 / 不认识的格式）
    pub skipped: Vec<String>,
}

/// 同一目录内配对；完整链优先，同一私钥不重复导入 leaf 与 fullchain。
pub fn pair_cert_files(names: &[String]) -> Vec<(String, String)> {
    fn stem(name: &str) -> String {
        Path::new(name).file_stem().unwrap_or_default().to_string_lossy().to_ascii_lowercase()
    }
    fn extension(name: &str) -> String {
        Path::new(name).extension().unwrap_or_default().to_string_lossy().to_ascii_lowercase()
    }
    let key_name = |name: &str| matches!(stem(name).as_str(), "key" | "private" | "privkey" | "privatekey");
    let is_key = |name: &str| extension(name) == "key" || (key_name(name) && matches!(extension(name).as_str(), "" | "pem"));
    let is_cert = |name: &str| !is_key(name)
        && !matches!(stem(name).as_str(), "ca" | "chain" | "issuer" | "intermediate" | "root")
        && (matches!(extension(name).as_str(), "crt" | "cer" | "pem")
            || matches!(name.to_ascii_lowercase().as_str(), "fullchain" | "cert" | "certificate"));
    let mut certificates: Vec<_> = names.iter().filter(|name| is_cert(name)).collect();
    certificates.sort_by_key(|name| (!stem(name).contains("fullchain"), name.to_ascii_lowercase()));
    let mut keys: Vec<_> = names.iter().filter(|name| is_key(name)).collect();
    keys.sort();
    let mut used = std::collections::BTreeSet::new();
    let mut pairs = Vec::new();
    for cert in certificates {
        let exact = keys.iter().find(|key| stem(key) == stem(cert));
        let family = matches!(stem(cert).as_str(), "fullchain" | "cert" | "certificate" | "server" | "domain");
        let key = exact.or_else(|| family.then(|| keys.iter().find(|key| key_name(key))).flatten());
        if let Some(key) = key {
            if used.insert((*key).clone()) { pairs.push((cert.clone(), (*key).clone())); }
        }
    }
    pairs
}

/// 扫描目录和一层子目录，在各自目录内配对，保留完整链和来源相对路径。
pub fn import_cert_dir(paths: &Paths, dir: &Path) -> Result<DirImportResult> {
    let mut out = DirImportResult::default();
    let mut files = Vec::new();
    collect_files(dir, 0, &mut files)?;
    let mut groups = std::collections::BTreeMap::<PathBuf, Vec<String>>::new();
    for file in files {
        if let (Some(parent), Some(name)) = (file.parent(), file.file_name().and_then(|n| n.to_str())) {
            groups.entry(parent.to_path_buf()).or_default().push(name.to_string());
        }
    }
    for (parent, names) in groups {
        let pairs = pair_cert_files(&names);
        for (cert, key) in &pairs {
            let source = parent.join(cert);
            let label = source.strip_prefix(dir).unwrap_or(&source).display().to_string();
            match import_cert_pair(paths, &source, &parent.join(key)) {
                Ok(imported) => out.imported.push(imported),
                Err(error) => out.skipped.push(format!("{label}：{}", error.message)),
            }
        }
        for name in &names {
            if pairs.iter().any(|(cert, key)| name == cert || name == key) { continue; }
            let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if matches!(ext.as_str(), "crt" | "cer" | "pem" | "key") || matches!(name.as_str(), "cert" | "key") {
                let source = parent.join(name);
                out.skipped.push(format!("{}：未配对或属于独立链文件", source.strip_prefix(dir).unwrap_or(&source).display()));
            }
        }
    }
    Ok(out)
}

fn collect_files(dir: &Path, depth: u8, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| AppError::io("读取证书目录", e))? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() { continue; }
        if kind.is_file() { out.push(entry.path()); }
        else if depth < 1 && kind.is_dir() { collect_files(&entry.path(), depth + 1, out)?; }
        if out.len() > 2000 { return Err(AppError::new("CERT_SCAN_LIMIT", "目录内文件超过 2000 个，请选择更具体的证书目录")); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_thresholds() {
        assert_eq!(status_for(-1), "expired");
        assert_eq!(status_for(0), "critical");
        assert_eq!(status_for(7), "critical");
        assert_eq!(status_for(8), "warn");
        assert_eq!(status_for(30), "warn");
        assert_eq!(status_for(31), "ok");
        assert_eq!(status_for(3650), "ok");
    }

    #[test]
    fn base64_roundtrip_known_value() {
        // "MZ" 的 base64 是 "TVo="
        assert_eq!(base64_decode("TVo=").unwrap(), vec![b'M', b'Z']);
        assert_eq!(base64_decode("TWFu").unwrap(), b"Man".to_vec());
        assert_eq!(base64_decode("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn base64_rejects_invalid_char() {
        assert!(base64_decode("!!!!").is_none());
    }

    #[test]
    fn parse_utc_time_two_digit_year() {
        // 2026-09-21 20:30:45Z
        let ts = parse_asn1_time("260921203045Z", false).unwrap();
        let dt = chrono::DateTime::from_timestamp(ts, 0).unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-09-21 20:30:45"
        );
    }

    #[test]
    fn parse_utc_time_handles_19xx() {
        // UTCTime 里 50-99 表示 19xx
        let ts = parse_asn1_time("990101000000Z", false).unwrap();
        let dt = chrono::DateTime::from_timestamp(ts, 0).unwrap();
        assert_eq!(dt.format("%Y").to_string(), "1999");
    }

    #[test]
    fn parse_generalized_time() {
        let ts = parse_asn1_time("20260921203045Z", true).unwrap();
        let dt = chrono::DateTime::from_timestamp(ts, 0).unwrap();
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2026-09-21");
    }

    #[test]
    fn parse_time_rejects_garbage() {
        assert!(parse_asn1_time("", false).is_none());
        assert!(parse_asn1_time("abc", false).is_none());
        assert!(
            parse_asn1_time("991399000000Z", false).is_none(),
            "13 月应被拒绝"
        );
        assert!(
            parse_asn1_time("990101250000Z", false).is_none(),
            "25 时应被拒绝"
        );
    }

    #[test]
    fn extract_ascii_strings_finds_text() {
        // 阈值是 4 字符：更短的串基本都是 ASN.1 噪声，留着反而干扰 CN 识别
        let v = extract_ascii_strings(b"\x02\x03abcd\x00hello.world\xff");
        assert!(v.contains(&"abcd".to_string()), "{v:?}");
        assert!(v.contains(&"hello.world".to_string()), "{v:?}");
        assert!(!v.iter().any(|s| s == "abc"), "3 字符应被丢弃：{v:?}");
        assert!(!v.iter().any(|s| s == "ab"));
    }

    #[test]
    fn looks_like_host_filters_noise() {
        assert!(looks_like_host("example.com"));
        assert!(looks_like_host("*.example.com"));
        assert!(!looks_like_host(""));
        assert!(!looks_like_host("12345"), "纯数字不算域名");
        assert!(!looks_like_host("has space"));
        assert!(!looks_like_host("中文域名"));
    }

    #[test]
    fn pem_to_der_rejects_non_pem() {
        assert!(pem_to_der("not a pem").is_none());
        assert!(pem_to_der("").is_none());
    }

    #[test]
    fn pem_to_der_decodes_valid_block() {
        // 构造一个最小合法 PEM：内容随便，只要 base64 能解出来
        let pem = "-----BEGIN CERTIFICATE-----\nTWFu\n-----END CERTIFICATE-----\n";
        assert_eq!(pem_to_der(pem).unwrap(), b"Man".to_vec());
    }

    #[test]
    fn pem_handles_whitespace_in_base64() {
        let pem = "-----BEGIN CERTIFICATE-----\nTW\nFu\n-----END CERTIFICATE-----\n";
        assert_eq!(pem_to_der(pem).unwrap(), b"Man".to_vec());
    }

    #[test]
    fn pair_files_same_stem_and_families() {
        use super::pair_cert_files;
        // 同名成对
        let p = pair_cert_files(&["a.com.crt".into(), "a.com.key".into()]);
        assert_eq!(p, vec![("a.com.crt".to_string(), "a.com.key".to_string())]);
        // certd 常见命名族：fullchain/cert 配 private/privkey
        let p = pair_cert_files(&["fullchain.pem".into(), "private.pem".into()]);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].1, "private.pem");
        // 链文件不当证书导；纯私钥不报
        let p = pair_cert_files(&["chain.pem".into(), "ca.pem".into(), "privkey.pem".into()]);
        assert!(p.is_empty());
        // 只剩证书没私钥 → 无对
        let p = pair_cert_files(&["b.com.crt".into()]);
        assert!(p.is_empty());
        // 一个私钥不重复配两张证书
        let p = pair_cert_files(&[
            "cert.pem".into(),
            "fullchain.pem".into(),
            "private.key".into(),
        ]);
        assert_eq!(p.len(), 1);
    }

    #[test]
    fn import_rejects_non_certificate_file() {
        let t = std::env::temp_dir().join(format!("nsb-imp-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&t);
        let paths = Paths::new(t.clone());
        let c = t.join("a.txt");
        let k = t.join("b.txt");
        std::fs::write(&c, "hello").unwrap();
        std::fs::write(&k, "hello").unwrap();
        let r = import_cert_pair(&paths, &c, &k);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, "NOT_A_CERT");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn import_rejects_missing_key_pem() {
        let t = std::env::temp_dir().join(format!("nsb-imp2-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&t);
        let paths = Paths::new(t.clone());
        let c = t.join("a.crt");
        let k = t.join("b.key");
        std::fs::write(
            &c,
            "-----BEGIN CERTIFICATE-----\nTWFu\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        std::fs::write(&k, "not a key").unwrap();
        let r = import_cert_pair(&paths, &c, &k);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, "NOT_A_KEY");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn list_imported_empty_when_dir_missing() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-imp-none"));
        let store = crate::store::Store::open(paths.base.join("certs-test.sqlite")).unwrap();
        assert!(list_imported(&paths, &store).unwrap().is_empty());
    }

    #[test]
    fn delete_imported_rejects_outside_path() {
        let t = std::env::temp_dir().join(format!("nsb-imp3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let paths = Paths::new(t.clone());
        let store = crate::store::Store::open(t.join("certs-test.sqlite")).unwrap();
        std::fs::create_dir_all(paths.certs().join("imported")).unwrap();
        let outside = t.join("secret.crt");
        std::fs::write(&outside, "x").unwrap();
        assert!(delete_imported(&paths, &store, &outside.to_string_lossy()).is_err());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn is_actionable_flags_missing_file_and_advice() {
        let mk = |present: bool, advice: &str| CertHealth {
            id: "c".into(),
            kind: "site".into(),
            subject: "a.test".into(),
            sans: vec![],
            not_after: 0,
            days_left: 100,
            status: "ok".into(),
            file_present: present,
            used_by_sites: vec![],
            missing_sans: vec![],
            advice: advice.into(),
        };
        assert!(is_actionable(&mk(false, "")));
        assert!(is_actionable(&mk(true, "还有 5 天到期")));
        assert!(!is_actionable(&mk(true, "")));
    }
}
