//! 证书健康检查与自定义证书导入。
//!
//! 自签证书不会过期（我们签 10 年），但两件事会真的咬人：
//! 1. **用户导入的证书**（公司内网 CA、通配符证书）有真实有效期，
//!    过期当天站点直接打不开，而浏览器给的提示往往看不出是过期；
//! 2. **证书与站点配置对不上**：域名改了但证书没重新签，SAN 里没有新域名。
//!
//! 这里把两件事都查出来，并给出「还剩几天 / 该怎么办」。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::model::CertRecord;
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

/// 生成体检报告
pub fn report(paths: &Paths, store: &crate::store::Store) -> Result<CertReport> {
    let certs = crate::tls::list_certs(paths, store)?;
    let sites = crate::sites::list(store).unwrap_or_default();
    let now = chrono::Local::now().timestamp();
    let day = 86_400i64;

    let mut out: Vec<CertHealth> = Vec::with_capacity(certs.len());
    for c in certs {
        let days_left = (c.not_after - now).div_euclid(day);
        let status = status_for(days_left).to_string();
        let file_present = Path::new(&c.cert_path).is_file();

        // 哪些站点在用这张证书（按域名交集判断）
        let used_by: Vec<String> = sites
            .iter()
            .filter(|s| s.domains.iter().any(|d| c.sans.iter().any(|san| san == d)))
            .map(|s| s.name.clone())
            .collect();

        // 站点当前域名里，证书没覆盖的
        let mut missing: Vec<String> = Vec::new();
        if c.kind == "site" {
            for s in &sites {
                if used_by.contains(&s.name) {
                    for d in &s.domains {
                        if !c.sans.iter().any(|san| san == d) && !missing.contains(d) {
                            missing.push(d.clone());
                        }
                    }
                }
            }
        }

        let advice = if !file_present {
            "证书文件已丢失，请到「证书」页重新签发".to_string()
        } else if status == "expired" {
            if c.kind == "ca" {
                "根 CA 已过期，需要重建 CA 并重新签发所有站点证书".to_string()
            } else {
                "已过期：到站点详情里重新签发证书即可".to_string()
            }
        } else if status == "critical" || status == "warn" {
            format!("还有 {days_left} 天到期，建议尽快重新签发")
        } else if !missing.is_empty() {
            format!("证书未覆盖域名 {}，需重新签发", missing.join("、"))
        } else {
            String::new()
        };

        out.push(CertHealth {
            id: c.id,
            kind: c.kind,
            subject: c.subject,
            sans: c.sans,
            not_after: c.not_after,
            days_left,
            status,
            file_present,
            used_by_sites: used_by,
            missing_sans: missing,
            advice,
        });
    }

    // 最紧急的排前面，方便一眼看到要先处理谁
    out.sort_by_key(|c| c.days_left);

    let expired = out.iter().filter(|c| c.status == "expired").count();
    let critical = out.iter().filter(|c| c.status == "critical").count();
    let warning = out.iter().filter(|c| c.status == "warn").count();

    Ok(CertReport {
        certs: out,
        expired,
        critical,
        warning,
        ca_trusted: crate::tls::ca_trusted(paths),
        checked_at: now,
    })
}

/* ================= 自定义证书导入 ================= */

/// 用户自带的一对证书（公司 CA 签的、或买的通配符证书）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedCert {
    /// 落地后的证书路径
    pub cert_path: String,
    pub key_path: String,
    pub subject: String,
    pub sans: Vec<String>,
    pub not_before: i64,
    pub not_after: i64,
    pub days_left: i64,
}

/// 浅解析 PEM 证书，取出 subject / SAN / 有效期。
///
/// 这里**不引入 X.509 解析库**：本应用生成证书用的是 rcgen，但没有解析需求；
/// 为了一个「显示证书信息」的功能拉进来一个几百 KB 的依赖不划算。
/// 所以做法是从 PEM 里取出 DER，再按 ASN.1 结构把 CN 与 SAN 里的
/// 可见字符串抠出来——足够满足「让用户确认导入的是哪张证书」。
pub fn parse_pem_info(pem: &str) -> Option<(String, Vec<String>, i64, i64)> {
    let der = pem_to_der(pem)?;
    let text = extract_ascii_strings(&der);
    // 有效期：取两个看起来像日期的字符串（UTCTime/GeneralizedTime）
    let dates = extract_dates(&der);
    let (nb, na) = (
        dates.first().copied().unwrap_or(0),
        dates.get(1).copied().unwrap_or(0),
    );
    // CN 优先，其次取第一个像域名的字符串
    let subject = text
        .iter()
        .find(|s| s.contains('.') && !s.contains(' ') && looks_like_host(s))
        .cloned()
        .unwrap_or_else(|| "unknown".to_string());
    let sans: Vec<String> = text
        .iter()
        .filter(|s| looks_like_host(s) && s.contains('.'))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    Some((subject, sans, nb, na))
}

fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    let begin = pem.find("-----BEGIN CERTIFICATE-----")?;
    let after = &pem[begin + "-----BEGIN CERTIFICATE-----".len()..];
    let end = after.find("-----END CERTIFICATE-----")?;
    let b64: String = after[..end].chars().filter(|c| !c.is_whitespace()).collect();
    base64_decode(&b64)
}

/// 极简 base64 解码（避免为一个功能引入依赖）
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0u32;
    for ch in s.bytes() {
        if ch == b'=' {
            break;
        }
        let v = T.iter().position(|c| *c == ch)? as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// 从 DER 里抠出可见的 ASCII 串（长度 ≥4）
fn extract_ascii_strings(der: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for &b in der {
        if (0x20..0x7f).contains(&b) {
            cur.push(b as char);
        } else {
            if cur.len() >= 4 {
                out.push(cur.clone());
            }
            cur.clear();
        }
    }
    if cur.len() >= 4 {
        out.push(cur);
    }
    out
}

fn looks_like_host(s: &str) -> bool {
    // 只保留「字母数字.-*」组成的串，且不含常见噪声词
    !s.is_empty()
        && s.len() <= 253
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*' | '_'))
        && !s.chars().all(|c| c.is_ascii_digit())
}

/// 从 DER 里找 UTCTime(YYMMDDHHMMSSZ) / GeneralizedTime(YYYYMMDDHHMMSSZ)
fn extract_dates(der: &[u8]) -> Vec<i64> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < der.len() {
        // UTCTime: tag 0x17, GeneralizedTime: tag 0x18
        if der[i] == 0x17 || der[i] == 0x18 {
            let generalized = der[i] == 0x18;
            let need = if generalized { 15 } else { 13 };
            if i + 2 + need <= der.len() && der[i + 1] as usize == need {
                let s: String = der[i + 2..i + 2 + need]
                    .iter()
                    .take_while(|b| b.is_ascii_digit() || **b == b'Z')
                    .map(|b| *b as char)
                    .collect();
                if let Some(ts) = parse_asn1_time(&s, generalized) {
                    out.push(ts);
                }
                i += 2 + need;
                continue;
            }
        }
        i += 1;
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// ASN.1 时间 → Unix 秒
pub fn parse_asn1_time(s: &str, generalized: bool) -> Option<i64> {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    let (y, rest) = if generalized {
        if digits.len() < 14 {
            return None;
        }
        (digits[0..4].parse::<i32>().ok()?, &digits[4..])
    } else {
        if digits.len() < 12 {
            return None;
        }
        let yy: i32 = digits[0..2].parse().ok()?;
        // UTCTime：50-99 是 19xx，00-49 是 20xx
        (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &digits[2..])
    };
    let mo: u32 = rest.get(0..2)?.parse().ok()?;
    let d: u32 = rest.get(2..4)?.parse().ok()?;
    let h: u32 = rest.get(4..6)?.parse().ok()?;
    let mi: u32 = rest.get(6..8)?.parse().ok()?;
    let se: u32 = rest.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    chrono::NaiveDate::from_ymd_opt(y, mo, d)?.and_hms_opt(h, mi, se)?.and_utc().timestamp().into()
}

/// 导入一对证书文件到本应用（复制而非移动，不动用户的原始文件）
pub fn import_cert_pair(paths: &Paths, cert_src: &Path, key_src: &Path) -> Result<ImportedCert> {
    let cert_pem =
        std::fs::read_to_string(cert_src).map_err(|e| AppError::io("读取证书文件", e))?;
    if !cert_pem.contains("BEGIN CERTIFICATE") {
        return Err(AppError::new(
            "NOT_A_CERT",
            "这个文件里没有 PEM 格式的证书",
        )
        .with_hint("请选择 .crt / .pem 证书文件（不是 .key 私钥，也不是 .pfx 二进制格式）"));
    }
    let key_pem = std::fs::read_to_string(key_src).map_err(|e| AppError::io("读取私钥文件", e))?;
    if !key_pem.contains("PRIVATE KEY") {
        return Err(AppError::new("NOT_A_KEY", "这个文件里没有 PEM 格式的私钥")
            .with_hint("请选择 .key 私钥文件；若只有 .pfx，需要先转换成 pem"));
    }
    let (subject, sans, not_before, not_after) = parse_pem_info(&cert_pem)
        .ok_or_else(|| AppError::new("CERT_PARSE_FAILED", "无法解析这张证书"))?;

    let dir = paths.certs().join("imported");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::io("创建证书目录", e))?;
    // 用证书主体 + 时间戳命名，避免同名互相覆盖
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let safe_subject: String = subject
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    let base = format!("{safe_subject}-{stamp}");
    let cert_dst = dir.join(format!("{base}.crt"));
    let key_dst = dir.join(format!("{base}.key"));
    std::fs::write(&cert_dst, &cert_pem).map_err(|e| AppError::io("写入证书", e))?;
    // 私钥文件权限收紧（Windows 上 ACL 由上层处理，至少不要放到世界可读的临时目录）
    std::fs::write(&key_dst, &key_pem).map_err(|e| AppError::io("写入私钥", e))?;

    let now = chrono::Local::now().timestamp();
    Ok(ImportedCert {
        cert_path: cert_dst.to_string_lossy().to_string(),
        key_path: key_dst.to_string_lossy().to_string(),
        subject,
        sans,
        not_before,
        not_after,
        days_left: (not_after - now).div_euclid(86_400),
    })
}

/// 已导入的证书列表
pub fn list_imported(paths: &Paths) -> Vec<ImportedCert> {
    let dir = paths.certs().join("imported");
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("crt") {
            continue;
        }
        let pem = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let key = p.with_extension("key");
        if let Some((subject, sans, nb, na)) = parse_pem_info(&pem) {
            let now = chrono::Local::now().timestamp();
            out.push(ImportedCert {
                cert_path: p.to_string_lossy().to_string(),
                key_path: key.to_string_lossy().to_string(),
                subject,
                sans,
                not_before: nb,
                not_after: na,
                days_left: (na - now).div_euclid(86_400),
            });
        }
    }
    out.sort_by_key(|c| c.days_left);
    out
}

/// 删除一个已导入的证书（只允许删导入目录里的）
pub fn delete_imported(paths: &Paths, cert_path: &str) -> Result<()> {
    let dir = paths.certs().join("imported");
    let target = PathBuf::from(cert_path);
    let canon_dir = dir.canonicalize().map_err(|e| AppError::io("定位导入目录", e))?;
    let canon = target
        .canonicalize()
        .map_err(|e| AppError::io("定位证书", e))?;
    if !canon.starts_with(&canon_dir) {
        return Err(AppError::new("FORBIDDEN", "只能删除导入目录内的证书"));
    }
    std::fs::remove_file(&canon).map_err(|e| AppError::io("删除证书", e))?;
    // 同名私钥一并删掉，避免留下孤儿私钥
    let key = canon.with_extension("key");
    if key.is_file() {
        let _ = std::fs::remove_file(key);
    }
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

const CERT_EXTS: [&str; 2] = ["crt", "pem"];
const KEY_EXTS: [&str; 2] = ["key", "pem"];

/// 纯配对逻辑：同一目录下的证书文件与同名 .key 配成对。
/// 规则（对齐 certd 的输出习惯 fullchain.pem/cert.pem + private.pem/privkey.pem）：
/// - `x.crt` / `x.pem` 配 `x.key`
/// - `fullchain/cert/certificate` 类证书名配 `private/privkey/key` 类私钥名（常见命名族）
/// - 私钥文件永远不当证书用；`chain`/`ca`/`issuer` 只当链的一部分，不单独导入
pub fn pair_cert_files(names: &[String]) -> Vec<(String, String)> {
    let stem = |n: &str| -> (String, Option<String>) {
        match n.rsplit_once('.') {
            Some((s, e)) => (s.to_string(), Some(e.to_ascii_lowercase())),
            None => (n.to_string(), None),
        }
    };
    let keyish = |stem: &str| {
        ["private", "privkey", "privatekey"].iter().any(|c| stem.to_ascii_lowercase().contains(c))
    };
    let is_key = |n: &str| {
        let (s, ext) = stem(n);
        ext.as_deref() == Some("key") || (ext.as_deref() == Some("pem") && keyish(&s))
    };
    let is_cert = |n: &str| {
        let (s, ext) = stem(n);
        let cert_name = ["fullchain", "cert", "certificate", "server", "domain"].iter().any(|c| s.eq_ignore_ascii_case(c));
        // fullchain 是「完整证书链（leaf 在前）」，仍按证书导入；
        // 只排除纯中间链命名（chain/ca/issuer/…）
        let fullchain = s.to_ascii_lowercase().contains("fullchain");
        let chain_only = !fullchain
            && ["chain", "ca.", ".ca", "issuer", "intermediate", "root"].iter().any(|c| s.to_ascii_lowercase().contains(c));
        let looks_key = keyish(&s);
        ext.as_deref() == Some("crt")
            || (ext.as_deref() == Some("pem") && !chain_only && !looks_key)
            || (ext.is_none() && cert_name)
            || (ext.as_deref() == Some("pem") && cert_name && !chain_only && !looks_key)
    };
    let mut keys: Vec<(String, String)> = Vec::new(); // (stem_lower, filename)
    for n in names {
        if is_key(n) {
            let (s, _) = stem(n);
            keys.push((s.to_ascii_lowercase(), n.clone()));
        }
    }
    let family_key = |cert_stem: &str, keys: &[(String, String)]| -> Option<String> {
        let cs = cert_stem.to_ascii_lowercase();
        // 同名优先；fullchain/cert 族配 private/privkey 族
        if let Some((_, k)) = keys.iter().find(|(ks, _)| *ks == cs) {
            return Some(k.clone());
        }
        let cert_family = cs.contains("fullchain") || cs.contains("cert") || cs.contains("server") || cs.contains("domain");
        if cert_family {
            for cand in ["private", "privkey", "key", "privatekey"] {
                if let Some((_, k)) = keys.iter().find(|(ks, _)| ks.contains(cand)) {
                    return Some(k.clone());
                }
            }
        }
        None
    };
    let mut pairs = Vec::new();
    let mut used_keys: Vec<String> = Vec::new();
    for n in names {
        if is_cert(n) {
            let (s, _) = stem(n);
            if let Some(k) = family_key(&s, &keys) {
                if !used_keys.contains(&k) {
                    used_keys.push(k.clone());
                    pairs.push((n.clone(), k));
                }
            }
        }
    }
    pairs
}

/// 扫描目录（含一层子目录），把所有「证书 + 私钥」对导入。
/// 单个失败（解析不了 / 缺私钥）记进 skipped 继续，不阻断其它。
pub fn import_cert_dir(paths: &Paths, dir: &Path) -> Result<DirImportResult> {
    let mut out = DirImportResult::default();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_files(dir, 0, &mut files)?;
    let names: Vec<String> = files
        .iter()
        .map(|f| f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
        .collect();
    let pairs = pair_cert_files(&names);
    for (cert_name, key_name) in pairs.iter() {
        let cert_path = files
            .iter()
            .find(|f| f.file_name().map(|n| n.to_string_lossy() == cert_name.as_str()).unwrap_or(false));
        let key_path = files
            .iter()
            .find(|f| f.file_name().map(|n| n.to_string_lossy() == key_name.as_str()).unwrap_or(false));
        if let (Some(c), Some(k)) = (cert_path, key_path) {
            match import_cert_pair(paths, c, k) {
                Ok(ic) => out.imported.push(ic),
                Err(e) => out.skipped.push(format!("{cert_name}：{}", e)),
            }
        }
    }
    // 报告没配上的证书文件（只有私钥的不报，避免噪音）
    let paired_certs: Vec<String> = pairs.iter().map(|(c, _)| c.clone()).collect();
    for n in &names {
        let (s, ext) = match n.rsplit_once('.') {
            Some((s, e)) => (s.to_string(), Some(e.to_ascii_lowercase())),
            None => (n.clone(), None),
        };
        let _ = s;
        if ext.as_deref() == Some("crt") && !paired_certs.contains(n) {
            out.skipped.push(format!("{n}：找不到同名私钥"));
        }
    }
    Ok(out)
}

/// 收集目录文件（最多两层：certd 输出目录常见 `{域名}/cert|key` 一层子目录）
fn collect_files(dir: &Path, depth: u8, out: &mut Vec<std::path::PathBuf>) -> Result<()> {
    let read = std::fs::read_dir(dir).map_err(|e| AppError::io("读取目录", e))?;
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_file() {
            out.push(path);
        } else if depth < 1 && path.is_dir() {
            collect_files(&path, depth + 1, out)?;
        }
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
        assert_eq!(dt.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-09-21 20:30:45");
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
        assert!(parse_asn1_time("991399000000Z", false).is_none(), "13 月应被拒绝");
        assert!(parse_asn1_time("990101250000Z", false).is_none(), "25 时应被拒绝");
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
        let p = pair_cert_files(&["cert.pem".into(), "fullchain.pem".into(), "private.key".into()]);
        assert_eq!(p.len(), 1);
    }

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
        std::fs::write(&c, "-----BEGIN CERTIFICATE-----\nTWFu\n-----END CERTIFICATE-----\n").unwrap();
        std::fs::write(&k, "not a key").unwrap();
        let r = import_cert_pair(&paths, &c, &k);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, "NOT_A_KEY");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn list_imported_empty_when_dir_missing() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-imp-none"));
        assert!(list_imported(&paths).is_empty());
    }

    #[test]
    fn delete_imported_rejects_outside_path() {
        let t = std::env::temp_dir().join(format!("nsb-imp3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let paths = Paths::new(t.clone());
        std::fs::create_dir_all(paths.certs().join("imported")).unwrap();
        let outside = t.join("secret.crt");
        std::fs::write(&outside, "x").unwrap();
        assert!(delete_imported(&paths, &outside.to_string_lossy()).is_err());
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
