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
    let ca_expected = crate::tls::local_ca_expected(paths, &certs, &sites)?;
    certs.retain(|c| c.kind != "ca");
    if ca_expected { certs.insert(0, crate::model::CertRecord {
        id: "ca".into(), kind: "ca".into(), subject: "NiceEnv Local Root CA".into(), sans: vec![],
        not_before: 0, not_after: 0, cert_path: paths.certs().join("ca.crt").to_string_lossy().into(),
        key_path: Some(paths.certs().join("ca.key").to_string_lossy().into()), trusted: None,
    }); }
    let now = chrono::Utc::now().timestamp();
    let mut out = Vec::new();
    for c in certs {
        let users: Vec<_> = sites.iter().filter(|s| s.https && uses_managed_certificate(s, &c)).collect();
        let mut subject = c.subject;
        let mut sans = c.sans;
        let mut not_after = c.not_after / 1000;
        let mut not_before = c.not_before / 1000;
        let checked: Result<()> = (|| {
            let cert_path = if c.kind == "ca" { crate::paths::checked_data_path(&paths.base, "certs/ca.crt")? } else { PathBuf::from(&c.cert_path) };
            let pem = read_managed_pem(&cert_path)?;
            let info = parse_pem_info(&pem).ok_or_else(|| AppError::new("CERT_PARSE_FAILED", "证书内容损坏，无法解析"))?;
            subject = info.0; sans = info.1; not_before = info.2; not_after = info.3;
            let key = c.key_path.as_deref().ok_or_else(|| AppError::new("NOT_A_KEY", "没有匹配的私钥文件"))?;
            let key_path = if c.kind == "ca" { crate::paths::checked_data_path(&paths.base, "certs/ca.key")? } else { PathBuf::from(key) };
            check_pair(&pem, &read_managed_pem(&key_path)?)?;
            if c.kind == "ca" {
                let chain = parse_chain(&pem)?;
                let (_, cert) = x509_parser::parse_x509_certificate(chain[0].as_ref())
                    .map_err(|e| AppError::new("CERT_PARSE_FAILED", e.to_string()))?;
                if !cert.is_ca() { return Err(AppError::new("CERT_NOT_CA", "此文件不是根 CA 证书，请恢复原根证书")); }
            }
            Ok(())
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
    let ca_usable = out.iter().any(|c| c.kind == "ca" && c.file_present && !matches!(c.status.as_str(), "invalid" | "expired"));
    Ok(CertReport {
        expired: out.iter().filter(|c| c.status == "expired").count(),
        critical: out.iter().filter(|c| c.status == "critical" || c.status == "invalid").count(),
        warning: out.iter().filter(|c| c.status == "warn").count(),
        ca_trusted: ca_usable && crate::tls::ca_trusted(paths), checked_at: now, certs: out,
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

/// 部署前校验证书、私钥和域名，并直接从叶证书读取有效期（毫秒）。
pub(crate) fn deployment_validity(cert_pem: &str, key_pem: &str, domains: &[String]) -> Result<(i64, i64)> {
    check_pair(cert_pem, key_pem)?;
    check_server_leaf(cert_pem)?;
    let chain = parse_chain(cert_pem)?;
    let (_, sans, before, after) = cert_info(chain[0].as_ref())?;
    let mut expected = crate::tls::normalize_domains(domains)?;
    let mut actual = crate::tls::normalize_domains(&sans)?;
    expected.sort(); actual.sort();
    if expected != actual {
        return Err(AppError::new("CERT_DEPLOY_DOMAINS", "已签发证书的域名与当前自动化不一致，请重新签发"));
    }
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if before > now || after <= now {
        return Err(AppError::new("CERT_DEPLOY_EXPIRED", "已签发证书已过期或尚未生效，请检查时间或重新签发"));
    }
    Ok((before * 1000, after * 1000))
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

pub(crate) fn read_managed_pem(path: &Path) -> Result<String> {
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

/// 调用方持有站点与服务生命周期锁。原位置更新并保留绑定，发布失败成对恢复。
fn replace_imported_with_reload(
    paths: &Paths, store: &crate::store::Store, id: &str, cert_src: &Path, key_src: &Path,
    reload: impl FnOnce(&[crate::model::Site]) -> Result<()>,
) -> Result<ImportedCert> {
    let _files = crate::tls::CERT_FILES.lock();
    let (cert, key) = imported_paths(paths, id)?;
    let users: Vec<_> = store.list_sites()?.into_iter()
        .filter(|site| site.runtime.imported_cert_id.as_deref() == Some(id)).collect();
    if !cert.try_exists()? && !key.try_exists()? && users.is_empty() {
        return Err(AppError::new("NOT_FOUND", "导入证书已不存在，请刷新列表或重新导入"));
    }
    let cert_pem = read_pem(cert_src)?;
    let key_pem = read_pem(key_src)?;
    let (_, sans, _, _) = parse_pem_info(&cert_pem)
        .ok_or_else(|| AppError::new("CERT_PARSE_FAILED", "无法解析新证书，请选择 PEM 证书或完整证书链"))?;
    deployment_validity(&cert_pem, &key_pem, &sans)?;
    // 关闭 HTTPS 或停用站点仍保留选择，不能在续期时移除它们需要的域名。
    for site in &users {
        validate_coverage(&sans, &site.domains).map_err(|error|
            error.with_hint(format!("站点「{}」仍选择此证书。请提供覆盖全部引用域名的新证书，或先更换该站点的证书。", site.name)))?;
    }
    crate::certdeploy::local_pair(&cert.to_string_lossy(), &key.to_string_lossy(), &cert_pem, &key_pem)?;
    // 重载失败时保留已验证的新材料，避免部分服务已经加载新证书却又退回旧文件。
    reload(&users).map_err(|error| AppError::new("CERT_RELOAD_FAILED", "新证书已保存，但 Web 服务未能完成加载")
        .with_hint("站点绑定保持不变。请检查服务状态和配置，修复后启动或重启对应 Web 服务；服务仍运行时也可再次应用这组文件。")
        .with_detail(error.to_string()))?;
    let mut updated = imported_entry(paths, id);
    updated.used_by_sites = users.into_iter().map(|site| site.name).collect();
    Ok(updated)
}

impl crate::CoreState {
    pub fn replace_imported_certificate(&self, id: &str, cert_src: &Path, key_src: &Path) -> Result<ImportedCert> {
        let _work = crate::BackgroundWork::begin("更新导入证书")?;
        let _sites = crate::sites::SITE_CHANGES.lock();
        let _operation = self.manager.lifecycle.lock();
        replace_imported_with_reload(&self.paths, &self.store, id, cert_src, key_src, |sites| {
            let servers: std::collections::BTreeSet<_> = sites.iter().filter(|site|
                site.https && crate::sites::derive_status(&self.paths, site) == "running"
                    && self.manager.snapshot(&site.runtime.web_server).is_some_and(|s| s.state == crate::model::ServiceState::Running)
            ).map(|site| site.runtime.web_server.as_str()).collect();
            // 已运行服务的安装入口丢失不能被通用重建入口跳过并误报成功。
            for server in &servers {
                match *server {
                    "nginx" => { crate::ops::nginx_exe(&self.store)?; }
                    "apache" => { crate::ops::apache_paths(&self.store)?; }
                    _ => return Err(AppError::new("BAD_WEB_SERVER", "此站点的 Web 服务不支持自动加载证书")),
                }
            }
            if servers.is_empty() { return Ok(()); }
            crate::ops::rebuild_and_reload_selected(&self.store, &self.paths, &self.manager, &servers.into_iter().collect::<Vec<_>>())
        })
    }
}

/// 目录读取错误如实返回；损坏证书与孤立私钥保留在列表并标明原因，便于清理。
pub fn list_imported(paths: &Paths, store: &crate::store::Store) -> Result<Vec<ImportedCert>> {
    let _files = crate::tls::CERT_FILES.lock();
    let dir = crate::paths::checked_data_path(&paths.base, "certs/imported")?;
    let read = match std::fs::read_dir(&dir) {
        Ok(read) => Some(read),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(AppError::io("读取导入证书目录", error)),
    };
    let mut ids = std::collections::BTreeSet::new();
    for entry in read.into_iter().flatten() {
        let entry = entry?;
        if entry.file_type()?.is_symlink() { continue; }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e == "crt" || e == "key") {
            if let Some(id) = path.file_stem().and_then(|s| s.to_str()) { ids.insert(id.to_string()); }
        }
    }
    let sites = store.list_sites()?;
    // 文件全部丢失时仍保留已绑定的条目，使用户可以原位恢复续期材料。
    ids.extend(sites.iter().filter_map(|site| site.runtime.imported_cert_id.clone()));
    let mut out: Vec<_> = ids.into_iter().map(|id| {
        let mut entry = imported_entry(paths, &id);
        entry.used_by_sites = sites.iter().filter(|s| s.runtime.imported_cert_id.as_deref() == Some(&id))
            .map(|s| s.name.clone()).collect();
        entry
    }).collect();
    out.sort_by_key(|c| (c.usable, c.days_left));
    Ok(out)
}

/// 同一输出路径可能被主域名默认配置或显式 ACME 选择引用。
pub(crate) fn uses_managed_certificate(site: &crate::model::Site, cert: &crate::model::CertRecord) -> bool {
    if site.runtime.imported_cert_id.is_some() || !matches!(cert.kind.as_str(), "site" | "acme") { return false; }
    match site.runtime.acme_cert_id.as_deref() {
        Some(id) => cert.kind == "acme" && cert.id == id,
        None => site.domains.first().is_some_and(|domain| domain.eq_ignore_ascii_case(&cert.subject)),
    }
}

pub(crate) fn acme_primary(id: &str) -> Result<String> {
    let primary = id.strip_prefix("acme-").ok_or_else(|| AppError::new("BAD_CERT_ID", "请选择有效的 ACME 证书"))?;
    let domains = crate::tls::normalize_domains(&[primary.into()])
        .map_err(|_| AppError::new("BAD_CERT_ID", "ACME 证书标识无效，请重新选择"))?;
    if domains[0] != primary { return Err(AppError::new("BAD_CERT_ID", "ACME 证书标识无效，请重新选择")); }
    Ok(primary.into())
}

pub(crate) fn acme_paths(paths: &Paths, id: &str) -> Result<(PathBuf, PathBuf)> {
    let stem = acme_primary(id)?.replace('*', "_wildcard").replace(':', "_");
    Ok((crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{stem}.crt"))?,
        crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{stem}.key"))?))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteCertificateChoice {
    pub id: String,
    pub kind: String,
    pub subject: String,
    pub sans: Vec<String>,
    pub not_before: i64,
    pub not_after: i64,
    pub days_left: i64,
    pub usable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    pub used_by_sites: Vec<String>,
}

fn acme_entry(paths: &Paths, cert: &crate::model::CertRecord) -> SiteCertificateChoice {
    let mut entry = SiteCertificateChoice { id: cert.id.clone(), kind: "acme".into(), subject: cert.subject.clone(),
        sans: vec![], not_before: 0, not_after: 0, days_left: 0, usable: false, problem: None, used_by_sites: vec![] };
    let checked: Result<()> = (|| {
        let (crt, key) = acme_paths(paths, &cert.id)?;
        if cert.kind != "acme" || Path::new(&cert.cert_path) != crt
            || cert.key_path.as_deref().map(Path::new) != Some(key.as_path())
        { return Err(AppError::new("CERT_PATH", "ACME 证书记录与受管文件位置不一致，请重新部署")); }
        let pem = read_managed_pem(&crt)?;
        let (subject, sans, before, after) = parse_pem_info(&pem)
            .ok_or_else(|| AppError::new("CERT_PARSE_FAILED", "无法解析 ACME 证书"))?;
        entry.subject = subject; entry.sans = sans; entry.not_before = before; entry.not_after = after;
        entry.days_left = (after - chrono::Utc::now().timestamp()).div_euclid(86_400);
        deployment_validity(&pem, &read_managed_pem(&key)?, &entry.sans)?;
        Ok(())
    })();
    match checked { Ok(()) => entry.usable = true, Err(error) => entry.problem = Some(error.message) }
    entry
}

/// 只读取已保存记录与真实文件，不生成或信任本地根 CA。
pub fn site_certificate_choices(paths: &Paths, store: &crate::store::Store) -> Result<Vec<SiteCertificateChoice>> {
    let _files = crate::tls::CERT_FILES.lock();
    let sites = store.list_sites()?;
    // SAN 站点的同步副本不是独立签发输出；只提供自动化主证书，确保绑定路径在续签后仍被更新。
    let mut out: Vec<_> = store.list_certs()?.iter().filter(|c| c.kind == "acme" && c.sans.first() == Some(&c.subject)).map(|cert| {
        let mut entry = acme_entry(paths, cert);
        entry.used_by_sites = sites.iter().filter(|site| uses_managed_certificate(site, cert)).map(|s| s.name.clone()).collect();
        entry
    }).collect();
    out.extend(list_imported(paths, store)?.into_iter().map(|c| SiteCertificateChoice {
        id: c.id, kind: "imported".into(), subject: c.subject, sans: c.sans, not_before: c.not_before,
        not_after: c.not_after, days_left: c.days_left, usable: c.usable, problem: c.problem, used_by_sites: c.used_by_sites,
    }));
    out.sort_by_key(|c| (c.kind.clone(), !c.usable, c.subject.clone(), c.id.clone()));
    Ok(out)
}

pub fn validate_acme_domains(paths: &Paths, store: &crate::store::Store, id: &str, domains: &[String]) -> Result<()> {
    let _files = crate::tls::CERT_FILES.lock();
    acme_primary(id)?;
    let cert = store.list_certs()?.into_iter().find(|c| c.kind == "acme" && c.id == id)
        .ok_or_else(|| AppError::new("ACME_CERT_MISSING", "所选 ACME 证书不存在，请重新部署或更换证书"))?;
    if cert.sans.first() != Some(&cert.subject) {
        return Err(AppError::new("BAD_CERT_ID", "请选择自动化的 ACME 主证书；站点同步副本不单独绑定"));
    }
    let entry = acme_entry(paths, &cert);
    if !entry.usable {
        return Err(AppError::new("CERT_UNUSABLE", entry.problem.unwrap_or_else(|| "ACME 证书不可用".into()))
            .with_hint("请在证书自动化中续签并重新部署，或更换站点证书。"));
    }
    validate_coverage(&entry.sans, domains)
}

pub fn validate_site_certificate(paths: &Paths, store: &crate::store::Store, site: &crate::model::Site) -> Result<()> {
    if site.runtime.imported_cert_id.is_some() && site.runtime.acme_cert_id.is_some() {
        return Err(AppError::new("BAD_CERT_ID", "每个站点只能选择一种证书来源"));
    }
    if !site.https { return Ok(()); }
    if let Some(id) = &site.runtime.acme_cert_id { return validate_acme_domains(paths, store, id, &site.domains); }
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
    validate_coverage(&cert.sans, domains)
}

fn validate_coverage(sans: &[String], domains: &[String]) -> Result<()> {
    let missing: Vec<_> = domains.iter().filter(|d| !covers_domain(sans, d)).cloned().collect();
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

    fn renewal_fixture() -> (tempfile::TempDir, crate::CoreState) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf()); paths.ensure_dirs().unwrap();
        let state = crate::CoreState {
            store: crate::store::Store::open(paths.db()).unwrap(), paths,
            manager: std::sync::Arc::new(crate::services::ServiceManager::new()),
            installer: crate::install::Installer::bundled(),
            downloader: std::sync::Arc::new(crate::download::Downloader::new()),
            emit: std::sync::Arc::new(|_| {}), watchdog: std::sync::Arc::new(crate::watchdog::Watchdog::new()),
        };
        (temp, state)
    }

    fn renewal_pair(directory: &Path, name: &str, domains: &[&str], days: i64) -> (PathBuf, PathBuf) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(domains.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
        params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(10);
        params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(days);
        let cert = params.self_signed(&key).unwrap();
        let crt = directory.join(format!("{name}.crt")); let private = directory.join(format!("{name}.key"));
        std::fs::write(&crt, cert.pem()).unwrap(); std::fs::write(&private, key.serialize_pem()).unwrap();
        (crt, private)
    }

    fn renewal_site(state: &crate::CoreState, id: &str, certificate: &str, domain: &str, https: bool) -> crate::model::Site {
        let site: crate::model::Site = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "domains": [domain], "rootDir": state.paths.base,
            "runtime": {"kind": "static", "webServer": "nginx", "importedCertId": certificate},
            "https": https, "rewrite": "none", "db": null, "status": "stopped", "createdAt": 1, "updatedAt": 1
        })).unwrap();
        state.store.save_site(&site).unwrap(); site
    }

    #[test]
    fn imported_certificate_renewal_preserves_identity_bindings_and_source_files() {
        let (_temp, state) = renewal_fixture();
        let (old, old_key) = renewal_pair(&state.paths.base, "old", &["*.example.com", "example.com"], 2);
        let original = import_cert_pair(&state.paths, &old, &old_key).unwrap();
        renewal_site(&state, "shop", &original.id, "shop.example.com", true);
        renewal_site(&state, "off", &original.id, "example.com", false);
        let (new, new_key) = renewal_pair(&state.paths.base, "new", &["*.example.com", "example.com"], 90);
        let content = std::fs::read(&new).unwrap(); let private = std::fs::read(&new_key).unwrap();
        let updated = state.replace_imported_certificate(&original.id, &new, &new_key).unwrap();
        assert_eq!(updated.id, original.id); assert_eq!(updated.cert_path, original.cert_path);
        assert!(updated.usable && updated.days_left > 80); assert_eq!(updated.used_by_sites.len(), 2);
        assert_eq!(std::fs::read(&updated.cert_path).unwrap(), content);
        assert_eq!(std::fs::read(&updated.key_path).unwrap(), private);
        assert_eq!(std::fs::read(new).unwrap(), content); assert_eq!(std::fs::read(new_key).unwrap(), private);
        assert!(state.store.list_sites().unwrap().iter().all(|s| s.runtime.imported_cert_id.as_deref() == Some(original.id.as_str())));
        assert_eq!(list_imported(&state.paths, &state.store).unwrap().len(), 1);
        assert!(!state.paths.certs().join("ca.crt").exists());
        assert!(!state.paths.nginx_conf().exists()); // 停止中的服务不创建配置、不启动。
        assert_eq!(report(&state.paths, &state.store).unwrap().certs[0].status, "ok");
    }

    #[test]
    fn imported_certificate_renewal_rejects_invalid_material_and_partial_coverage() {
        let (_temp, state) = renewal_fixture();
        let (old, old_key) = renewal_pair(&state.paths.base, "old", &["shop.example.com", "example.com"], 2);
        let original = import_cert_pair(&state.paths, &old, &old_key).unwrap();
        renewal_site(&state, "off", &original.id, "example.com", false);
        let before = std::fs::read(&original.cert_path).unwrap(); let before_key = std::fs::read(&original.key_path).unwrap();
        let (valid, valid_key) = renewal_pair(&state.paths.base, "valid", &["shop.example.com", "example.com"], 90);
        let (partial, partial_key) = renewal_pair(&state.paths.base, "partial", &["*.example.com"], 90);
        let (expired, expired_key) = renewal_pair(&state.paths.base, "expired", &["example.com"], -1);
        let corrupt = state.paths.base.join("corrupt.crt"); std::fs::write(&corrupt, "invalid").unwrap();
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["example.com".into()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = state.paths.base.join("root.crt"); let key = state.paths.base.join("root.key");
        std::fs::write(&ca, params.self_signed(&ca_key).unwrap().pem()).unwrap(); std::fs::write(&key, ca_key.serialize_pem()).unwrap();
        for (cert, key, code) in [(&partial, &partial_key, "CERT_DOMAIN_MISMATCH"), (&valid, &partial_key, "CERT_KEY_MISMATCH"),
            (&expired, &expired_key, "CERT_DEPLOY_EXPIRED"), (&corrupt, &valid_key, "CERT_PARSE_FAILED"), (&ca, &key, "CERT_IS_CA")] {
            let error = replace_imported_with_reload(&state.paths, &state.store, &original.id, cert, key, |_| panic!("invalid material must not reload")).unwrap_err();
            assert_eq!(error.code, code);
            assert_eq!(std::fs::read(&original.cert_path).unwrap(), before); assert_eq!(std::fs::read(&original.key_path).unwrap(), before_key);
        }
        assert_eq!(state.replace_imported_certificate("../old", &valid, &valid_key).unwrap_err().code, "BAD_CERT_ID");
        assert_eq!(state.replace_imported_certificate("gone", &valid, &valid_key).unwrap_err().code, "NOT_FOUND");
    }

    #[test]
    fn imported_certificate_missing_pair_can_be_restored_and_reload_failure_is_explicit() {
        let (_temp, state) = renewal_fixture();
        renewal_site(&state, "bound", "missing-cert", "repair.example.com", true);
        let missing = list_imported(&state.paths, &state.store).unwrap();
        assert_eq!(missing.len(), 1); assert!(!missing[0].usable); assert_eq!(missing[0].used_by_sites, ["bound"]);
        let (new, new_key) = renewal_pair(&state.paths.base, "repair", &["repair.example.com"], 90);
        let error = replace_imported_with_reload(&state.paths, &state.store, "missing-cert", &new, &new_key, |sites| {
            assert_eq!(sites.len(), 1);
            let (cert, key) = imported_paths(&state.paths, "missing-cert").unwrap();
            assert_eq!(std::fs::read(cert).unwrap(), std::fs::read(&new).unwrap());
            assert_eq!(std::fs::read(key).unwrap(), std::fs::read(&new_key).unwrap());
            Err(AppError::new("RELOAD_DENIED", "fixture reload rejected"))
        }).unwrap_err();
        assert_eq!(error.code, "CERT_RELOAD_FAILED");
        assert!(list_imported(&state.paths, &state.store).unwrap()[0].usable);
        assert!(state.replace_imported_certificate("missing-cert", &new, &new_key).unwrap().usable);
    }

    #[cfg(windows)]
    #[test]
    fn imported_certificate_second_file_failure_restores_original_pair() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_temp, state) = renewal_fixture();
        let (old, old_key) = renewal_pair(&state.paths.base, "old", &["example.com"], 2);
        let original = import_cert_pair(&state.paths, &old, &old_key).unwrap();
        let before = std::fs::read(&original.cert_path).unwrap(); let before_key = std::fs::read(&original.key_path).unwrap();
        let (new, new_key) = renewal_pair(&state.paths.base, "new", &["example.com"], 90);
        let _locked = std::fs::OpenOptions::new().read(true).share_mode(1).open(&original.key_path).unwrap();
        assert!(replace_imported_with_reload(&state.paths, &state.store, &original.id, &new, &new_key, |_| panic!("failed publish must not reload")).is_err());
        assert_eq!(std::fs::read(&original.cert_path).unwrap(), before); assert_eq!(std::fs::read(&original.key_path).unwrap(), before_key);
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires NSB_NGINX_ROOT; runs an isolated Nginx on ephemeral ports and stops it"]
    fn imported_certificate_native_nginx_serves_renewed_leaf() {
        let source = PathBuf::from(std::env::var("NSB_NGINX_ROOT").expect("NSB_NGINX_ROOT"));
        let (_temp, state) = renewal_fixture();
        let install = state.paths.base.join("runtime"); let root = install.join("nginx-fixture");
        std::fs::create_dir_all(root.join("conf")).unwrap(); std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::copy(source.join("nginx.exe"), root.join("nginx.exe")).unwrap();
        std::fs::copy(source.join("conf/mime.types"), root.join("conf/mime.types")).unwrap();
        state.store.upsert_installed(&crate::model::InstalledPackage {
            id: "nginx".into(), version: "fixture".into(), category: "web-server".into(),
            install_path: install.to_string_lossy().into(), config_path: String::new(), installed_at: 0,
        }).unwrap();
        let http = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let https = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let http_port = http.local_addr().unwrap().port(); let https_port = https.local_addr().unwrap().port();
        state.store.set_setting("portOverride.http", &http_port.to_string()).unwrap();
        state.store.set_setting("portOverride.https", &https_port.to_string()).unwrap();
        let (old, old_key) = renewal_pair(&state.paths.base, "old", &["localhost"], 2);
        let cert = import_cert_pair(&state.paths, &old, &old_key).unwrap();
        let mut site = renewal_site(&state, "native-renewal", &cert.id, "localhost", true);
        let www = state.paths.base.join("www"); std::fs::create_dir_all(&www).unwrap();
        site.root_dir = www.to_string_lossy().into(); state.store.save_site(&site).unwrap();
        std::fs::write(www.join("index.html"), "renewal fixture").unwrap();
        let conf = crate::configgen::render_site_conf(&site, http_port, https_port,
            &state.paths.etc().join("nginx/fastcgi_params"), &state.paths.certs().join("sites"), &state.paths.logs());
        std::fs::write(state.paths.nginx_sites_dir().join("native-renewal.conf"), conf).unwrap();
        crate::ops::register_services(&state.paths, &state.store, &state.manager);
        struct Stop<'a>(&'a crate::CoreState);
        impl Drop for Stop<'_> { fn drop(&mut self) { let _ = crate::ops::stop_service(&self.0.store, &self.0.paths, &self.0.manager, "nginx"); } }
        let guard = Stop(&state); drop(http); drop(https);
        crate::ops::start_service(&state.store, &state.paths, &state.manager, "nginx").unwrap();
        let leaf = || {
            let response = reqwest::blocking::Client::builder().no_proxy().danger_accept_invalid_certs(true).tls_info(true)
                .timeout(std::time::Duration::from_secs(5)).build().unwrap()
                .get(format!("https://localhost:{https_port}/")).send().unwrap();
            assert!(response.status().is_success());
            let der = response.extensions().get::<reqwest::tls::TlsInfo>().unwrap().peer_certificate().unwrap().to_vec();
            assert_eq!(response.text().unwrap(), "renewal fixture"); der
        };
        assert_eq!(leaf(), parse_chain(&std::fs::read_to_string(old).unwrap()).unwrap()[0].as_ref());
        let (new, new_key) = renewal_pair(&state.paths.base, "renewed", &["localhost"], 90);
        state.replace_imported_certificate(&cert.id, &new, &new_key).unwrap();
        assert_eq!(leaf(), parse_chain(&std::fs::read_to_string(new).unwrap()).unwrap()[0].as_ref());
        assert!(!state.paths.certs().join("ca.crt").exists());
        assert!(!state.paths.apache_conf().exists());
        let pids = state.manager.snapshot("nginx").unwrap().pids;
        drop(guard);
        assert!(!crate::services::tcp_port_open(http_port)); assert!(!crate::services::tcp_port_open(https_port));
        assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
    }

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
