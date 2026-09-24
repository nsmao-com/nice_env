//! DNS 服务商适配：DNS-01 验证需要往 `_acme-challenge.<域名>` 写 TXT 记录。
//!
//! 每家 API 风格不同，这里统一成三个操作：
//! - `zones()`：列出账号下托管的域名（用于把证书域名映射到正确的主域名区）
//! - `set_txt()`：写一条 TXT，返回记录 id
//! - `clear_txt()`：按 id 删除（ACME 验证完即清，不留脏记录）
//!
//! 签名实现都带单测（用固定向量自校验），真实请求失败时把服务商原文翻出来。

use crate::error::{AppError, Result};
use crate::model::DnsProvider;
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256};

type HmacSha1 = Hmac<Sha1>;

pub const USER_AGENT: &str = "NiceEnv/0.1 (+https://github.com)";

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(USER_AGENT)
        .build()
        .expect("http client")
}

/* ================= Aliyun（RPC 签名，DNS 2015-01-09） ================= */

/// 阿里云 RPC percentEncode：RFC3986 未保留字符之外的都编码
pub fn aliyun_percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 阿里云 RPC 请求签名：StringToSign = GET&%2F&encode(query)，HMAC-SHA1(secret + "&")
pub fn aliyun_sign(secret: &str, string_to_sign: &str) -> String {
    let mut mac = HmacSha1::new_from_slice(format!("{secret}&").as_bytes())
        .expect("hmac key");
    mac.update(string_to_sign.as_bytes());
    B64_STANDARD.encode(mac.finalize().into_bytes())
}

/// 发一个阿里云 RPC 请求（GET + 签名），返回 JSON
fn aliyun_rpc(
    endpoint: &str,
    version: &str,
    action: &str,
    ak: &str,
    secret: &str,
    extra: &[(&str, &str)],
) -> Result<serde_json::Value> {
    let mut params: Vec<(String, String)> = vec![
        ("Format".into(), "JSON".into()),
        ("Version".into(), version.into()),
        ("AccessKeyId".into(), ak.into()),
        ("SignatureMethod".into(), "HMAC-SHA1".into()),
        ("SignatureVersion".into(), "1.0".into()),
        ("SignatureNonce".into(), uuid_v4()),
        (
            "Timestamp".into(),
            time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
                .replace("+00:00", "Z"),
        ),
        ("Action".into(), action.into()),
    ];
    for (k, v) in extra {
        params.push(((*k).into(), (*v).into()));
    }
    params.sort();

    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", aliyun_percent_encode(k), aliyun_percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let string_to_sign = format!("GET&{}&{}", aliyun_percent_encode("/"), aliyun_percent_encode(&query));
    let signature = aliyun_sign(secret, &string_to_sign);
    let url = format!("{endpoint}/?Signature={signature}&{query}");

    let resp = http()
        .get(&url)
        .send()
        .map_err(|e| AppError::new("DNS_HTTP", format!("阿里云 DNS 请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::json!({}));
    if let Some(err) = body.get("Code") {
        return Err(AppError::new(
            "DNS_ALIYUN",
            format!(
                "阿里云 DNS {} 失败：{} - {}",
                action,
                err.as_str().unwrap_or(""),
                body["Message"].as_str().unwrap_or("")
            ),
        ));
    }
    Ok(body)
}

fn aliyun_zones(ak: &str, secret: &str) -> Result<Vec<String>> {
    let body = aliyun_rpc(
        "https://alidns.aliyuncs.com",
        "2015-01-09",
        "DescribeDomains",
        ak,
        secret,
        &[("PageSize", "100")],
    )?;
    Ok(body["Domains"]["Domain"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| d["DomainName"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

fn aliyun_add_txt(ak: &str, secret: &str, zone: &str, rr: &str, value: &str) -> Result<String> {
    let body = aliyun_rpc(
        "https://alidns.aliyuncs.com",
        "2015-01-09",
        "AddDomainRecord",
        ak,
        secret,
        &[("DomainName", zone), ("RR", rr), ("Type", "TXT"), ("Value", value)],
    )?;
    Ok(body["RecordId"]
        .as_i64()
        .map(|id| id.to_string())
        .unwrap_or_default())
}

fn aliyun_del_txt(ak: &str, secret: &str, record_id: &str) -> Result<()> {
    aliyun_rpc(
        "https://alidns.aliyuncs.com",
        "2015-01-09",
        "DeleteDomainRecord",
        ak,
        secret,
        &[("RecordId", record_id)],
    )?;
    Ok(())
}

/* ================= Cloudflare（API Token + Bearer） ================= */

const CF_BASE: &str = "https://api.cloudflare.com/client/v4";

fn cf_request(
    token: &str,
    method: reqwest::Method,
    path: &str,
    json: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let mut req = http().request(method.clone(), format!("{CF_BASE}{path}")).bearer_auth(token);
    if let Some(j) = &json {
        req = req.json(j);
    }
    let resp = req
        .send()
        .map_err(|e| AppError::new("DNS_HTTP", format!("Cloudflare 请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::json!({}));
    // Cloudflare 统一 {success, errors:[{code,message}], result}
    if body["success"].as_bool() != Some(true) {
        let msg = body["errors"]
            .as_array()
            .map(|es| {
                es.iter()
                    .map(|e| e["message"].as_str().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_else(|| "Cloudflare 返回失败".into());
        return Err(AppError::new("DNS_CLOUDFLARE", format!("Cloudflare：{msg}")));
    }
    Ok(body["result"].clone())
}

fn cf_zones(token: &str) -> Result<Vec<String>> {
    let mut zones = Vec::new();
    let mut page = 1;
    loop {
        let result = cf_request(token, reqwest::Method::GET, &format!("/zones?per_page=50&page={page}"), None)?;
        if let Some(list) = result.as_array() {
            for z in list {
                if let Some(name) = z["name"].as_str() {
                    zones.push(name.to_string());
                }
            }
            if list.len() < 50 {
                break;
            }
        } else {
            break;
        }
        page += 1;
        if page > 20 {
            break; // 防御：最多翻 1000 个域
        }
    }
    Ok(zones)
}

fn cf_add_txt(token: &str, zone_id: &str, name: &str, value: &str) -> Result<String> {
    let result = cf_request(
        token,
        reqwest::Method::POST,
        &format!("/zones/{zone_id}/dns_records"),
        Some(serde_json::json!({
            "type": "TXT",
            "name": name,
            "content": format!("\"{value}\""),
            "ttl": 120,
        })),
    )?;
    Ok(result["id"].as_str().unwrap_or_default().to_string())
}

fn cf_del_txt(token: &str, zone_id: &str, record_id: &str) -> Result<()> {
    cf_request(token, reqwest::Method::DELETE, &format!("/zones/{zone_id}/dns_records/{record_id}"), None)?;
    Ok(())
}

/* ================= DNSPod（login_token） ================= */

fn dnspod_post(token: &str, params: &[(&str, &str)]) -> Result<serde_json::Value> {
    let mut form: Vec<(String, String)> = vec![
        ("login_token".into(), token.into()),
        ("format".into(), "json".into()),
        ("lang".into(), "zh".into()),
    ];
    for (k, v) in params {
        form.push(((*k).into(), (*v).into()));
    }
    let resp = http()
        .post("https://dnsapi.cn/")
        .form(&form)
        .send()
        .map_err(|e| AppError::new("DNS_HTTP", format!("DNSPod 请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::json!({}));
    let code = body["status"]["code"].as_str().unwrap_or("");
    if code != "1" {
        return Err(AppError::new(
            "DNS_DNSPOD",
            format!(
                "DNSPod：{} ({})",
                body["status"]["message"].as_str().unwrap_or("失败"),
                code
            ),
        ));
    }
    Ok(body)
}

/// dnspod login_token 格式为 "id,token"（原样透传）
fn dnspod_zones(token: &str) -> Result<Vec<String>> {
    let body = dnspod_post(token, &[("action", "Domain.List")])?;
    Ok(body["data"]["domains"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| d["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

fn dnspod_add_txt(token: &str, zone: &str, sub: &str, value: &str) -> Result<String> {
    let body = dnspod_post(
        token,
        &[
            ("action", "Record.Create"),
            ("domain", zone),
            ("sub_domain", sub),
            ("record_type", "TXT"),
            ("record_line_id", "0"),
            ("value", value),
        ],
    )?;
    Ok(body["data"]["record"]["id"]
        .as_i64()
        .map(|id| id.to_string())
        .unwrap_or_default())
}

fn dnspod_del_txt(token: &str, zone: &str, record_id: &str) -> Result<()> {
    dnspod_post(token, &[("action", "Record.Delete"), ("domain", zone), ("record_id", record_id)])?;
    Ok(())
}


/* ================= GoDaddy（sso-key 头 + REST） ================= */

fn godaddy_zones(ak: &str, secret: &str) -> Result<Vec<String>> {
    let resp = http()
        .get("https://api.godaddy.com/v1/domains?statuses=ACTIVE&limit=100")
        .header("Authorization", format!("sso-key {ak}:{secret}"))
        .send()
        .map_err(|e| AppError::new("DNS_HTTP", format!("GoDaddy 请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    Ok(body
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| d["domain"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

/// GoDaddy 的 TXT 是「按名字整体替换」：PUT 该名字下的全部记录
fn godaddy_put(ak: &str, secret: &str, zone: &str, rr: &str, records: serde_json::Value) -> Result<()> {
    let resp = http()
        .put(format!(
            "https://api.godaddy.com/v1/domains/{zone}/records/TXT/{rr}"
        ))
        .header("Authorization", format!("sso-key {ak}:{secret}"))
        .json(&records)
        .send()
        .map_err(|e| AppError::new("DNS_HTTP", format!("GoDaddy 请求失败：{e}")))?;
    if !resp.status().is_success() {
        let t = resp.text().unwrap_or_default();
        return Err(AppError::new(
            "DNS_GODADDY",
            format!("GoDaddy：{}", t.chars().take(200).collect::<String>()),
        ));
    }
    Ok(())
}

fn godaddy_add_txt(ak: &str, secret: &str, zone: &str, name: &str, value: &str) -> Result<String> {
    let rr = name
        .strip_suffix(&format!(".{zone}"))
        .unwrap_or(name)
        .to_string();
    godaddy_put(
        ak,
        secret,
        zone,
        &rr,
        serde_json::json!([{ "data": value, "ttl": 600 }]),
    )?;
    // 无记录 id 概念：以记录名为伪 id，删除时按名字清空
    Ok(format!("name:{rr}"))
}

fn godaddy_clear_txt(ak: &str, secret: &str, zone: &str, name: &str) -> Result<()> {
    let rr = name
        .strip_suffix(&format!(".{zone}"))
        .unwrap_or(name)
        .to_string();
    godaddy_put(ak, secret, zone, &rr, serde_json::json!([]))
}

/* ================= DigitalOcean（Bearer Token） ================= */

fn do_zones(token: &str) -> Result<Vec<String>> {
    let resp = http()
        .get("https://api.digitalocean.com/v2/domains?per_page=200")
        .bearer_auth(token)
        .send()
        .map_err(|e| AppError::new("DNS_HTTP", format!("DigitalOcean 请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    Ok(body["domains"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| d["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

fn do_add_txt(token: &str, zone: &str, name: &str, value: &str) -> Result<String> {
    let resp = http()
        .post(format!("https://api.digitalocean.com/v2/domains/{zone}/records"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "type": "TXT", "name": name, "data": value, "ttl": 120 }))
        .send()
        .map_err(|e| AppError::new("DNS_HTTP", format!("DigitalOcean 请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    Ok(body["record"]["id"]
        .as_i64()
        .map(|v| v.to_string())
        .unwrap_or_default())
}

fn do_del_txt(token: &str, zone: &str, record_id: &str) -> Result<()> {
    http()
        .delete(format!(
            "https://api.digitalocean.com/v2/domains/{zone}/records/{record_id}"
        ))
        .bearer_auth(token)
        .send()
        .map_err(|e| AppError::new("DNS_HTTP", format!("DigitalOcean 删除失败：{e}")))?;
    Ok(())
}

/* ================= Porkbun（apikey + secret，JSON POST） ================= */

fn porkbun_post(path: &str, ak: &str, secret: &str, extra: serde_json::Value) -> Result<serde_json::Value> {
    let mut body = serde_json::json!({ "apikey": ak, "secretapikey": secret });
    if let (Some(obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    let resp = http()
        .post(format!("https://porkbun.com/api/json/v3{path}"))
        .json(&body)
        .send()
        .map_err(|e| AppError::new("DNS_HTTP", format!("Porkbun 请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    if body["status"].as_str() == Some("ERROR") {
        return Err(AppError::new(
            "DNS_PORKBUN",
            format!("Porkbun：{}", body["message"].as_str().unwrap_or("失败")),
        ));
    }
    Ok(body)
}

fn porkbun_zones(ak: &str, secret: &str) -> Result<Vec<String>> {
    let body = porkbun_post("/domain/list", ak, secret, serde_json::json!({}))?;
    // 兼容两种返回形状：["a.com"] 与 [{"domain":"a.com"}]
    Ok(body["domains"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|d| {
                    d.as_str()
                        .map(String::from)
                        .or_else(|| d["domain"].as_str().map(String::from))
                })
                .collect()
        })
        .unwrap_or_default())
}

fn porkbun_add_txt(ak: &str, secret: &str, zone: &str, name: &str, value: &str) -> Result<String> {
    let rr = name
        .strip_suffix(&format!(".{zone}"))
        .unwrap_or(name)
        .to_string();
    let body = porkbun_post(
        &format!("/dns/create/{zone}"),
        ak,
        secret,
        serde_json::json!({ "name": rr, "type": "TXT", "content": value, "ttl": 600 }),
    )?;
    let id = body["id"]
        .as_str()
        .map(String::from)
        .or_else(|| body["id"].as_i64().map(|v| v.to_string()))
        .unwrap_or_default();
    Ok(id)
}

fn porkbun_del_txt(ak: &str, secret: &str, zone: &str, record_id: &str) -> Result<()> {
    porkbun_post(&format!("/dns/delete/{zone}/{record_id}"), ak, secret, serde_json::json!({}))?;
    Ok(())
}

/* ================= 华为云 DNS（SDK-HMAC-SHA256 签名） ================= */

/// 华为云签名串：`SDK-HMAC-SHA256\n{时间戳}\n{规范请求哈希}`，HMAC-SHA256(AK/SK)
pub fn hws_signature(sk: &str, sdk_date: &str, canonical_request_hash: &str) -> String {
    let string_to_sign = format!("SDK-HMAC-SHA256\n{sdk_date}\n{canonical_request_hash}");
    hex::encode(crate::acme::hmac_sha256(sk.as_bytes(), string_to_sign.as_bytes()))
}

/// RFC3986 URI 编码（华为云规范；`/` 可选不编码）
fn hws_uri_encode(s: &str, slash_safe: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if slash_safe => out.push('/'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn hws_sdk_date() -> String {
    let d = time::OffsetDateTime::now_utc();
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        d.year(),
        u8::from(d.month()),
        d.day(),
        d.hour(),
        d.minute(),
        d.second()
    )
}

/// 带签名的华为云请求；返回 JSON。host 形如 dns.myhuaweicloud.com / scm.cn-north-1.myhuaweicloud.com
pub fn hws_request(
    ak: &str,
    sk: &str,
    host: &str,
    method: reqwest::Method,
    path_with_query: &str,
    payload: Option<&serde_json::Value>,
) -> Result<serde_json::Value> {
    let (path, query) = match path_with_query.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path_with_query, ""),
    };
    let sdk_date = hws_sdk_date();
    let payload_str = payload.map(|p| p.to_string()).unwrap_or_default();

    // 规范查询串：按 key 排序 k=v（URI 编码）
    let mut pairs: Vec<(String, String)> = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (hws_uri_encode(k, false), hws_uri_encode(v, false)),
            None => (hws_uri_encode(kv, false), String::new()),
        })
        .collect();
    pairs.sort();
    let canonical_query = pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");

    let canonical_headers = format!("host:dns.myhuaweicloud.com\nx-sdk-date:{sdk_date}\n");
    let signed_headers = "host;x-sdk-date";
    let hashed_payload = hex::encode(Sha256::digest(payload_str.as_bytes()));
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.as_str(),
        hws_uri_encode(path, true),
        canonical_query,
        canonical_headers,
        signed_headers,
        hashed_payload
    );
    let hashed_canonical = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    let signature = hws_signature(sk, &sdk_date, &hashed_canonical);
    let authorization = format!(
        "SDK-HMAC-SHA256 Credential={ak}, SignedHeaders={signed_headers}, Signature={signature}"
    );

    let mut req = http()
        .request(method, format!("https://{host}{path_with_query}"))
        .header("X-Sdk-Date", &sdk_date)
        .header("Host", host)
        .header("Authorization", authorization);
    if payload.is_some() {
        req = req.header("Content-Type", "application/json").body(payload_str);
    }
    let resp = req
        .send()
        .map_err(|e| AppError::new("DNS_HTTP", format!("华为云请求失败：{e}")))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        return Err(AppError::new(
            "DNS_HUAWEI",
            format!(
                "华为云 DNS：{} {}",
                body["error_code"].as_str().unwrap_or(""),
                body["error_msg"].as_str().unwrap_or("请求失败")
            ),
        ));
    }
    Ok(body)
}

fn hw_zones(ak: &str, sk: &str) -> Result<Vec<String>> {
    let body = hws_request(ak, sk, "dns.myhuaweicloud.com", reqwest::Method::GET, "/v2/zones?type=public&limit=500", None)?;
    Ok(body["zones"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|z| z["name"].as_str().map(|n| n.trim_end_matches('.').to_string()))
                .collect()
        })
        .unwrap_or_default())
}

fn hw_zone_id(ak: &str, sk: &str, zone: &str) -> Result<String> {
    let body = hws_request(
        ak,
        sk,
        "dns.myhuaweicloud.com",
        reqwest::Method::GET,
        &format!("/v2/zones?name={zone}.&limit=1"),
        None,
    )?;
    body["zones"][0]["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| AppError::new("DNS_HUAWEI", format!("找不到 zone {zone}")))
}

fn hw_add_txt(ak: &str, sk: &str, zone: &str, name: &str, value: &str) -> Result<String> {
    let zone_id = hw_zone_id(ak, sk, zone)?;
    let body = hws_request(
        ak,
        sk,
        "dns.myhuaweicloud.com",
        reqwest::Method::POST,
        &format!("/v2/zones/{zone_id}/recordsets"),
        Some(&serde_json::json!({
            "name": format!("{name}."),
            "type": "TXT",
            "ttl": 300,
            "records": [format!("\"{value}\"")],
        })),
    )?;
    let rs_id = body["id"].as_str().unwrap_or_default().to_string();
    // 删除接口需要完整资源路径：zones/{zone_id}/recordsets/{id}
    Ok(format!("zones/{zone_id}/recordsets/{rs_id}"))
}

fn hw_del_txt(ak: &str, sk: &str, record_id: &str) -> Result<()> {
    // record_id 即完整资源路径（add 时拼好）
    hws_request(ak, sk, "dns.myhuaweicloud.com", reqwest::Method::DELETE, &format!("/v2/{record_id}"), None)?;
    Ok(())
}



/* ================= TXT 传播检测（DoH 公共解析，手动模式的核心） ================= */

/// 解析 Google DoH `resolve` 响应，抽出全部 TXT 值（去掉包裹引号）
pub fn parse_doh_txt(body: &str) -> Vec<String> {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    v["Answer"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|a| a["type"].as_i64() == Some(16)) // TXT
                .filter_map(|a| a["data"].as_str())
                .map(|d| d.trim_matches('"').to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// 查一次 TXT（dns.google 优先，Cloudflare 兜底；都挂了返回空）
pub fn doh_query_txt(name: &str) -> Vec<String> {
    for url in [
        format!("https://dns.google/resolve?name={name}&type=TXT"),
        format!("https://cloudflare-dns.com/dns-query?name={name}&type=TXT"),
    ] {
        let req = http()
            .get(&url)
            .header("accept", "application/dns-json");
        if let Ok(resp) = req.send() {
            if let Ok(body) = resp.text() {
                let got = parse_doh_txt(&body);
                if !got.is_empty() {
                    return got;
                }
            }
        }
    }
    Vec::new()
}

/// 等 TXT 记录在公共解析里可见（手动模式：用户加完记录才能过验证）。
/// 返回 Ok(()) = 已可见；Err = 超时（把记录内容带回给用户看）。
pub fn wait_txt_visible(name: &str, value: &str, timeout_sec: u64) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_sec);
    loop {
        let got = doh_query_txt(name);
        if got.iter().any(|v| v == value) {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err(AppError::new(
                "TXT_NOT_VISIBLE",
                format!("等待 TXT 记录 {name} 生效超时（{timeout_sec}s）"),
            )
            .with_hint("确认记录已添加且值完全一致；部分 DNS 服务商同步较慢，可稍后重试"));
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}

/* ================= 对上暴露的统一操作 ================= */

/// 证书域名 → (zone, 子域名前缀 RR)
/// 例：`_acme-challenge.a.b.example.com` 且 zone=`example.com` → ("example.com", "_acme-challenge.a.b")
pub fn split_zone<'a>(zones: &'a [String], domain: &str) -> Result<(&'a str, String)> {
    let d = domain.trim_end_matches('.').to_ascii_lowercase();
    let zone = zones
        .iter()
        .filter(|z| d == *z.to_ascii_lowercase() || d.ends_with(&format!(".{}", z.to_ascii_lowercase())))
        .max_by_key(|z| z.len())
        .ok_or_else(|| {
            AppError::new("DNS_ZONE", format!("域名 {domain} 不在你的 DNS 账号下（找不到托管区）"))
                .with_hint("确认该域名的解析确实托管在所选 DNS 服务商，且凭据有读取域名列表的权限")
        })?;
    let rr = d
        .strip_suffix(&format!(".{}", zone.to_ascii_lowercase()))
        .map(|s| s.to_string())
        .unwrap_or_default();
    Ok((zone.as_str(), rr))
}

/// 写 TXT：自动找 zone，返回 (zone, record_id)。
/// manual 模式不走这里 —— 由 certauto 编排（提示用户 + 等传播），见 wait_txt_visible。
pub fn set_txt(dns: &DnsProvider, domain: &str, prefix: &str, value: &str) -> Result<(String, String)> {
    if dns.kind == "manual" {
        return Err(AppError::new(
            "DNS_MANUAL",
            "手动模式不应调用服务商接口（内部错误）",
        ));
    }
    let zones = match dns.kind.as_str() {
        "aliyun" => aliyun_zones(&dns.access_key, &dns.secret)?,
        "cloudflare" => cf_zones(&dns.access_key)?,
        "dnspod" => dnspod_zones(&dns.access_key_with_secret())?,
        "godaddy" => godaddy_zones(&dns.access_key, &dns.secret)?,
        "digitalocean" => do_zones(&dns.access_key)?,
        "porkbun" => porkbun_zones(&dns.access_key, &dns.secret)?,
        "huawei" => hw_zones(&dns.access_key, &dns.secret)?,
        other => {
            return Err(AppError::new("DNS_PROVIDER", format!("不支持的 DNS 服务商：{other}")));
        }
    };
    let (zone, sub) = split_zone(&zones, domain)?;
    // 记录名：prefix[.sub]（域名即 zone 时只有 prefix）
    let name = if sub.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}.{sub}")
    };
    let record_id = match dns.kind.as_str() {
        "aliyun" => aliyun_add_txt(&dns.access_key, &dns.secret, zone, &name, value)?,
        "cloudflare" => {
            let zone_id = cf_zone_id(&dns.access_key, zone)?;
            cf_add_txt(&dns.access_key, &zone_id, &name, value)?
        }
        "godaddy" => godaddy_add_txt(&dns.access_key, &dns.secret, zone, &name, value)?,
        "digitalocean" => do_add_txt(&dns.access_key, zone, &name, value)?,
        "porkbun" => porkbun_add_txt(&dns.access_key, &dns.secret, zone, &name, value)?,
        "huawei" => hw_add_txt(&dns.access_key, &dns.secret, zone, &name, value)?,
        _ => dnspod_add_txt(&dns.access_key_with_secret(), zone, &name, value)?,
    };
    if record_id.is_empty() {
        return Err(AppError::new("DNS_PROVIDER", "服务商未返回记录 id，无法后续清理"));
    }
    Ok((zone.to_string(), record_id))
}

/// 清理 TXT。`name` 为完整记录名（GoDaddy 按名字覆盖删除，其余按 id）
pub fn clear_txt(dns: &DnsProvider, zone: &str, name: &str, record_id: &str) -> Result<()> {
    match dns.kind.as_str() {
        "aliyun" => aliyun_del_txt(&dns.access_key, &dns.secret, record_id),
        "cloudflare" => {
            let zone_id = cf_zone_id(&dns.access_key, zone)?;
            cf_del_txt(&dns.access_key, &zone_id, record_id)
        }
        "godaddy" => godaddy_clear_txt(&dns.access_key, &dns.secret, zone, name),
        "digitalocean" => do_del_txt(&dns.access_key, zone, record_id),
        "porkbun" => porkbun_del_txt(&dns.access_key, &dns.secret, zone, record_id),
        "huawei" => hw_del_txt(&dns.access_key, &dns.secret, record_id),
        _ => dnspod_del_txt(&dns.access_key_with_secret(), zone, record_id),
    }
}

fn cf_zone_id(token: &str, zone: &str) -> Result<String> {
    let result = cf_request(
        token,
        reqwest::Method::GET,
        &format!("/zones?name={zone}"),
        None,
    )?;
    result[0]["id"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| AppError::new("DNS_CLOUDFLARE", format!("找不到 zone {zone}")))
}

impl DnsProvider {
    /// dnspod 的 login_token = "id,token"：access_key 存 id，secret 存 token
    fn access_key_with_secret(&self) -> String {
        format!("{},{}", self.access_key, self.secret)
    }
}

/* ================= 小工具 ================= */

fn uuid_v4() -> String {
    let b = rand::random::<[u8; 16]>();
    let mut b = b;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

use base64::engine::general_purpose::STANDARD as B64_STANDARD;
use base64::Engine as _;

/* ================= 测试 ================= */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliyun_encoding_rfc3986() {
        assert_eq!(aliyun_percent_encode("aB3-_.~"), "aB3-_.~");
        assert_eq!(aliyun_percent_encode("a b"), "a%20b");
        assert_eq!(aliyun_percent_encode("a+b"), "a%2Bb");
        assert_eq!(aliyun_percent_encode("*"), "%2A");
    }

    #[test]
    fn aliyun_signature_matches_known_pattern() {
        // 用固定输入自校验：HMAC-SHA1(key="secret&", msg=StringToSign) → base64
        let sig = aliyun_sign("test-secret", "GET&%2F&AccessKeyId%3Dak");
        let mut mac = HmacSha1::new_from_slice(b"test-secret&").unwrap();
        mac.update(b"GET&%2F&AccessKeyId%3Dak");
        let expect = B64_STANDARD.encode(mac.finalize().into_bytes());
        assert_eq!(sig, expect);
    }

    #[test]
    fn split_zone_picks_longest_match() {
        let zones = vec!["example.com".to_string(), "b.example.com".to_string()];
        let (z, rr) = split_zone(&zones, "_acme-challenge.a.b.example.com").unwrap();
        assert_eq!(z, "b.example.com");
        assert_eq!(rr, "_acme-challenge.a");
        let (z, rr) = split_zone(&zones, "example.com").unwrap();
        assert_eq!(z, "example.com");
        assert_eq!(rr, "");
        assert!(split_zone(&zones, "other.org").is_err());
    }

    #[test]
    fn uuid_shape() {
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(u.as_bytes()[14], b'4'); // version 4
    }

    #[test]
    fn hws_signature_self_consistent() {
        let sig = hws_signature("sk", "20240101T000000Z", "abc");
        // 与统一 HMAC-SHA256 实现对拍
        assert_eq!(
            sig,
            hex::encode(crate::acme::hmac_sha256(
                b"sk",
                b"SDK-HMAC-SHA256\n20240101T000000Z\nabc"
            ))
        );
        assert_eq!(sig.len(), 64);
    }

    #[test]
    fn hws_uri_encode_matches_spec() {
        assert_eq!(hws_uri_encode("a/b", true), "a/b");
        assert_eq!(hws_uri_encode("a/b", false), "a%2Fb");
        assert_eq!(hws_uri_encode("a b", false), "a%20b");
    }

    #[test]
    fn parse_doh_txt_extracts_txt_values() {
        let body = r#"{"Status":0,"Answer":[
            {"name":"_acme-challenge.a.com.","type":16,"TTL":120,"data":"\"abc123\""},
            {"name":"_acme-challenge.a.com.","type":16,"TTL":120,"data":"\"other\""},
            {"name":"a.com.","type":1,"TTL":300,"data":"1.2.3.4"}
        ]}"#;
        let got = parse_doh_txt(body);
        assert_eq!(got, vec!["abc123".to_string(), "other".to_string()]);
        assert!(parse_doh_txt("not json").is_empty());
        assert!(parse_doh_txt("{}").is_empty());
    }

    #[test]
    fn sha256_helper_consistent() {
        // 给 certauto/dnsprov 共用的指纹逻辑一个固定锚点
        assert_eq!(
            hex::encode(Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
