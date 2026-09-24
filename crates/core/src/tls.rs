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

    // 通配符域名带 `*`，Windows 文件名不允许 —— 落盘名做净化（SAN 里保留原样）
    let file_stem = primary.replace('*', "_wildcard");
    let crt_path = paths.certs().join("sites").join(format!("{file_stem}.crt"));
    let key_path = paths.certs().join("sites").join(format!("{file_stem}.key"));
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

/// CA 是否已信任（Windows: certutil 按 CN 查找 Root store）。
/// 历史安装的 CA 叫 "NiceServBay Local Root CA"，改名为 NiceEnv 后
/// 两者都视为已信任——不然老用户会永远显示「未信任」。
pub fn ca_trusted(_paths: &Paths) -> bool {
    /// CN → certutil 查找时无法用通配，逐个名字查
    const CA_CN_NEW: &str = "NiceEnv Local Root CA";
    const CA_CN_LEGACY: &str = "NiceServBay Local Root CA";
    #[cfg(windows)]
    {
        for cn in [CA_CN_NEW, CA_CN_LEGACY] {
            let Ok(out) = std::process::Command::new("certutil")
                .args(["-verify", "-store", "Root", cn])
                .output()
            else {
                return false;
            };
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if text.contains(cn) {
                return true;
            }
        }
        false
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
            subject: "NiceEnv Local Root CA".into(),
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

/* ================= PFX (PKCS#12) 导出 ================= */

/// 把一段 PEM（可能是链）拆成 DER 列表
fn pem_chain_to_certs(pem: &str) -> Result<Vec<p12_keystore::Certificate>> {
    let mut out = Vec::new();
    for block in pem.split("-----END CERTIFICATE-----") {
        let b = block.trim();
        if b.is_empty() {
            continue;
        }
        let b64: String = b
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
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
    store: &Store,
    cert_id: &str,
    password: &str,
    out_path: &std::path::Path,
) -> Result<String> {
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
    let cert_pem = std::fs::read_to_string(&rec.cert_path)
        .map_err(|e| AppError::io("读取证书失败", e))?;

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
    store12.add_entry(&rec.subject, p12_keystore::KeyStoreEntry::PrivateKeyChain(chain));
    let pfx = store12
        .writer(password)
        .write()
        .map_err(|e| AppError::new("PFX_BUILD", format!("生成 PFX 失败：{e}")))?;
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(out_path, pfx).map_err(|e| AppError::io("写入 PFX 失败", e))?;
    Ok(out_path.to_string_lossy().to_string())
}

/// 导出 DER（二进制 X.509，部分设备/中间件要这个格式）：取链里第一张（leaf）
pub fn export_der(store: &Store, cert_id: &str, out_path: &std::path::Path) -> Result<String> {
    let rec = store
        .list_certs()?
        .into_iter()
        .find(|c| c.id == cert_id)
        .ok_or_else(|| AppError::new("NOT_FOUND", "证书不存在"))?;
    let pem = std::fs::read_to_string(&rec.cert_path).map_err(|e| AppError::io("读取证书失败", e))?;
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
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(out_path, der).map_err(|e| AppError::io("写入 DER 失败", e))?;
    Ok(out_path.to_string_lossy().to_string())
}

/// 导出 JKS（Java Keystore，Tomcat 等 Java 系中间件用）。
/// JKS 规范要求密码 ≥ 6 字符，这里如实转述错误而不是悄悄放行。
pub fn export_jks(
    store: &Store,
    cert_id: &str,
    password: &str,
    out_path: &std::path::Path,
) -> Result<String> {
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
    let cert_pem = std::fs::read_to_string(&rec.cert_path)
        .map_err(|e| AppError::io("读取证书失败", e))?;
    if password.chars().count() < 6 {
        return Err(AppError::new(
            "JKS_PASSWORD",
            "JKS 密码至少 6 个字符（Java Keystore 规范）",
        ));
    }

    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let key_b64: String = key_pem.lines().filter(|l| !l.starts_with("-----")).collect();
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

    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(out_path, buf).map_err(|e| AppError::io("写入 JKS 失败", e))?;
    Ok(out_path.to_string_lossy().to_string())
}

/// 导出 PEM 打包（证书链 + 私钥 拼一个 .pem，nginx / 迁移别家最顺手）
pub fn export_pem_bundle(store: &Store, cert_id: &str, out_path: &std::path::Path) -> Result<String> {
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
    let cert_pem = std::fs::read_to_string(&rec.cert_path)
        .map_err(|e| AppError::io("读取证书失败", e))?;
    let mut bundle = String::new();
    if !cert_pem.ends_with('\n') {
        bundle.push('\n');
    }
    bundle.push_str(&cert_pem);
    bundle.push('\n');
    bundle.push_str(&key_pem);
    if let Some(parent) = out_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(out_path, bundle).map_err(|e| AppError::io("写入 PEM 失败", e))?;
    Ok(out_path.to_string_lossy().to_string())
}
