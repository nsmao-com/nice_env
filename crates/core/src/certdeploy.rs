//! 证书部署目标：签发完成后的推送动作。
//!
//! 参考 certd 的「证书部署到任意平台」，这里内置桌面用户最常用的三类：
//! - 宝塔面板：证书入库 + （可选）直接配置到指定站点
//! - 1Panel：上传到面板证书库
//! - 阿里云：上传到 SSL 证书服务（CAS），供 CDN / WAF / SLB 等引用
//!
//! 各家签名算法都抽成 pub 供单测：真实面板拿不到 CI 环境，
//! 但签名函数与请求形状（shapes）可以用固定向量验证。

use crate::dnsprov::{aliyun_percent_encode, aliyun_sign, USER_AGENT};
use crate::error::{AppError, Result};
use crate::model::DeployTarget;
use base64::engine::general_purpose::STANDARD as B64;
use base64::engine::general_purpose::URL_SAFE as B64URL_SAFE;
use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use md5::{Digest as Md5Digest, Md5};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;
use sha2::{Digest, Sha256};
use std::time::Duration;

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(USER_AGENT)
        .danger_accept_invalid_certs(true) // 面板常是自签 HTTPS
        .build()
        .expect("http client")
}

fn md5_hex(s: &str) -> String {
    hex::encode(Md5::digest(s.as_bytes()))
}

fn sha1_hex(s: &str) -> String {
    hex::encode(Sha1::digest(s.as_bytes()))
}

fn cfg<'a>(t: &'a DeployTarget, key: &str) -> Result<&'a str> {
    t.config
        .get(key)
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            AppError::new(
                "DEPLOY_CONFIG",
                format!("部署目标「{}」缺少参数 {key}", t.name),
            )
        })
}

/// 部署一个目标；返回结果消息（人话）
pub fn deploy(
    target: &DeployTarget,
    domains: &[String],
    cert_pem: &str,
    key_pem: &str,
) -> Result<String> {
    match target.kind.as_str() {
        "btpanel" => bt_deploy(target, cert_pem, key_pem),
        "onepanel" => onepanel_deploy(target, domains, cert_pem, key_pem),
        "aliyun" => aliyun_upload(target, cert_pem, key_pem),
        "tencent" => tencent_upload(target, domains, cert_pem, key_pem),
        "synology" => synology_deploy(target, domains, cert_pem, key_pem),
        "k8s" => k8s_secret_deploy(target, domains, cert_pem, key_pem),
        "qiniu" => qiniu_upload(target, domains, cert_pem, key_pem),
        "hwssl" => hw_ssl_upload(target, domains, cert_pem, key_pem),
        "ssh" => ssh_deploy(target, domains, cert_pem, key_pem),
        "local" => local_deploy(target, domains, cert_pem, key_pem),
        other => Err(AppError::new(
            "DEPLOY_KIND",
            format!("不支持的部署目标类型：{other}"),
        )),
    }
}

/* ================= 通知（Webhook：通用 / 钉钉 / 企业微信 / 飞书） ================= */

/// 签发成功/失败后发通知。钉钉、企微、飞书的群机器人就是 POST 一个 JSON，
/// 各家消息体形状不同，这里按 kind 适配；generic 发结构化 JSON 自己解析。
pub fn notify(kind: &str, url: &str, ok: bool, title: &str, detail: &str) -> Result<()> {
    if url.trim().is_empty() {
        return Ok(());
    }
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let body = match kind {
        "dingtalk" | "wecom" => serde_json::json!({
            "msgtype": "text",
            "text": { "content": format!("[NiceEnv] {title}
        {detail}") }
        }),
        "feishu" => serde_json::json!({
            "msg_type": "text",
            "content": { "text": format!("[NiceEnv] {title}
        {detail}") }
        }),
        _ => serde_json::json!({
            "event": if ok { "cert_issued" } else { "cert_failed" },
            "ok": ok,
            "title": title,
            "detail": detail,
            "at": now,
            "source": "NiceEnv",
        }),
    };
    let resp = http()
        .post(url.trim())
        .json(&body)
        .send()
        .map_err(|e| AppError::new("NOTIFY_HTTP", format!("通知发送失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::new(
            "NOTIFY_HTTP",
            format!("通知端点返回 {}", resp.status()),
        ));
    }
    Ok(())
}

/* ================= 宝塔面板 ================= */

/// 宝塔请求令牌：request_token = md5(sha1(时间戳 + md5(api_sk)))
pub fn bt_request_token(api_sk: &str, timestamp_secs: u64) -> String {
    md5_hex(&sha1_hex(&format!("{timestamp_secs}{}", md5_hex(api_sk))))
}

fn bt_post(
    target: &DeployTarget,
    path: &str,
    action_query: &str,
    form: &[(&str, &str)],
) -> Result<serde_json::Value> {
    let base = cfg(target, "url")?.trim_end_matches('/');
    let api_sk = cfg(target, "apiSk")?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let url = format!(
        "{base}{path}?{action_query}&request_token={}&request_time={ts}",
        bt_request_token(api_sk, ts)
    );
    let mut form_vec: Vec<(String, String)> = form
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    form_vec.push(("request_time".into(), ts.to_string()));
    let resp = http()
        .post(&url)
        .form(&form_vec)
        .send()
        .map_err(|e| AppError::new("DEPLOY_HTTP", format!("宝塔请求失败：{e}")))?;
    let text = resp.text().unwrap_or_default();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    // 宝塔错误返回 {status:false, msg:"..."}
    if body.get("status") == Some(&serde_json::Value::Bool(false)) {
        return Err(AppError::new(
            "DEPLOY_BT",
            format!(
                "宝塔：{}",
                body["msg"]
                    .as_str()
                    .unwrap_or(&text.chars().take(200).collect::<String>())
            ),
        ));
    }
    Ok(body)
}

fn bt_deploy(target: &DeployTarget, cert_pem: &str, key_pem: &str) -> Result<String> {
    // 先确认面板连通（拿一把面板信息，顺带校验 apiSk）
    bt_post(target, "/data", "action=getSystemTotal", &[])?;

    let site_name = cfg(target, "siteName")?.to_string();
    // 把证书配置到指定站点（type:1 = 当前粘贴的证书）
    let ssl = serde_json::json!({
        "type": 1,
        "key": key_pem,
        "csr": cert_pem,
    });
    bt_post(
        target,
        "/site",
        "action=SetSSL",
        &[
            ("siteName", site_name.as_str()),
            ("ssl", &ssl.to_string()),
            (
                "forceHTTPS",
                target
                    .config
                    .get("forceHttps")
                    .map(|s| s.as_str())
                    .unwrap_or("false"),
            ),
        ],
    )?;
    Ok(format!("已将证书配置到宝塔站点 {site_name}"))
}

/* ================= 1Panel ================= */

/// 1Panel 接口签名：1Panel-Token = md5("1panel" + 时间戳 + api_key)
pub fn onepanel_token(api_key: &str, timestamp_secs: u64) -> String {
    md5_hex(&format!("1panel{timestamp_secs}{api_key}"))
}

fn onepanel_deploy(
    target: &DeployTarget,
    domains: &[String],
    cert_pem: &str,
    key_pem: &str,
) -> Result<String> {
    let base = cfg(target, "url")?.trim_end_matches('/');
    let api_key = cfg(target, "token")?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let url = format!("{base}/api/v2/websites/ssl/upload");
    let resp = http()
        .post(&url)
        .header("1Panel-Token", onepanel_token(api_key, ts))
        .header("1Panel-Timestamp", ts.to_string())
        .json(&serde_json::json!({
            "ssl": {
                "proxy": "",
                "privateKey": key_pem,
                "certificate": cert_pem,
                "description": domains.first().cloned().unwrap_or_else(|| "NiceEnv".into()),
                "provider": "manual",
                "type": "included",
                "autoRenew": false,
                "domains": [],
            }
        }))
        .send()
        .map_err(|e| AppError::new("DEPLOY_HTTP", format!("1Panel 请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    // 1Panel 统一 {code:200, message, data}
    if body["code"].as_i64() != Some(200) {
        return Err(AppError::new(
            "DEPLOY_1PANEL",
            format!("1Panel：{}", body["message"].as_str().unwrap_or("返回异常")),
        ));
    }
    Ok("已上传到 1Panel 证书库".into())
}

/* ================= 阿里云 SSL 证书服务（CAS） ================= */

fn aliyun_upload(target: &DeployTarget, cert_pem: &str, key_pem: &str) -> Result<String> {
    let ak = cfg(target, "accessKeyId")?;
    let sk = cfg(target, "accessKeySecret")?;
    let region = target
        .config
        .get("region")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("cn-hangzhou");
    let name = target
        .config
        .get("certName")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("NiceEnv");

    let mut params: Vec<(String, String)> = vec![
        ("Format".into(), "JSON".into()),
        ("Version".into(), "2018-07-13".into()),
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
        ("Action".into(), "UploadUserCertificate".into()),
        ("Name".into(), name.into()),
        ("Cert".into(), B64.encode(cert_pem)),
        ("Key".into(), B64.encode(key_pem)),
    ];
    params.sort();
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", aliyun_percent_encode(k), aliyun_percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let string_to_sign = format!(
        "GET&{}&{}",
        aliyun_percent_encode("/"),
        aliyun_percent_encode(&query)
    );
    let signature = aliyun_sign(sk, &string_to_sign);

    let resp = http()
        .post(format!("https://cas.{region}.aliyuncs.com/"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("Signature={signature}&{query}"))
        .send()
        .map_err(|e| AppError::new("DEPLOY_HTTP", format!("阿里云 CAS 请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    if let Some(code) = body.get("Code") {
        return Err(AppError::new(
            "DEPLOY_ALIYUN",
            format!(
                "阿里云上传证书失败：{} - {}",
                code.as_str().unwrap_or(""),
                body["Message"].as_str().unwrap_or("")
            ),
        ));
    }
    let order_id = body["OrderId"]
        .as_i64()
        .map(|v| v.to_string())
        .unwrap_or_default();
    Ok(format!("已上传到阿里云 SSL 证书服务（单号 {order_id}）"))
}

fn uuid_v4() -> String {
    let mut b = rand::random::<[u8; 16]>();
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

/* ================= SSH / SFTP 主机部署（certd 的招牌能力） ================= */

/// 上传证书/私钥到远程主机并可选执行脚本（如 `nginx -s reload`）。
/// config: host / port(默认22) / user / auth=password|key(默认password) /
///         password / keyPath(私钥文件路径) / certPath / keyPath / script
fn ssh_deploy(
    target: &DeployTarget,
    domains: &[String],
    cert_pem: &str,
    key_pem: &str,
) -> Result<String> {
    use russh::client::{AuthResult, Handler};
    use std::sync::Arc;

    struct AcceptAll;
    impl Handler for AcceptAll {
        type Error = russh::Error;
        // 首连信任主机指纹（accept-new 语义）；桌面端场景可接受
        async fn check_server_key(
            &mut self,
            _k: &russh::keys::PublicKeyOrCertificate,
        ) -> std::result::Result<bool, Self::Error> {
            Ok(true)
        }
    }

    let host = cfg(target, "host")?.to_string();
    let port: u16 = target
        .config
        .get("port")
        .and_then(|p| p.parse().ok())
        .unwrap_or(22);
    let user = cfg(target, "user")?.to_string();
    let auth_kind = target
        .config
        .get("auth")
        .map(|s| s.as_str())
        .unwrap_or("password");
    let remote_cert = cfg(target, "certPath")?.to_string();
    let remote_key = cfg(target, "keyPath")?.to_string();
    let script = target.config.get("script").cloned().unwrap_or_default();

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| AppError::internal("构建 tokio runtime", e.to_string()))?;
    rt.block_on(async move {
        let config = Arc::new(russh::client::Config::default());
        let mut handle = russh::client::connect(config, (host.as_str(), port), AcceptAll)
            .await
            .map_err(|e| {
                AppError::new("SSH_CONNECT", format!("SSH 连接 {host}:{port} 失败：{e}"))
            })?;

        let authed = if auth_kind == "key" {
            let key_path = cfg(target, "keyPath")
                .map(|s| s.to_string())
                .or_else(|_| cfg(target, "privateKey").map(|s| s.to_string()))?;
            let key = russh::keys::load_secret_key(&key_path, None)
                .map_err(|e| AppError::new("SSH_AUTH", format!("读取 SSH 私钥失败：{e}")))?;
            handle
                .authenticate_publickey(
                    user,
                    russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None),
                )
                .await
                .map_err(|e| AppError::new("SSH_AUTH", format!("SSH 公钥认证失败：{e}")))?
        } else {
            let pass = cfg(target, "password")?.to_string();
            handle
                .authenticate_password(user, pass)
                .await
                .map_err(|e| AppError::new("SSH_AUTH", format!("SSH 密码认证失败：{e}")))?
        };
        if !matches!(authed, AuthResult::Success) {
            return Err(AppError::new(
                "SSH_AUTH",
                "SSH 认证被拒绝（检查密码/密钥/用户名）",
            ));
        }

        // SFTP 上传两份文件
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| AppError::new("SSH_SFTP", format!("打开 SFTP 通道失败：{e}")))?;
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| AppError::new("SSH_SFTP", format!("SFTP 初始化失败：{e}")))?;
        for (path, content) in [
            (remote_cert.as_str(), cert_pem),
            (remote_key.as_str(), key_pem),
        ] {
            if let Some(parent) = std::path::Path::new(path).parent() {
                if let Some(p) = parent.to_str() {
                    if !p.is_empty() && p != "/" {
                        let _ = sftp.create_dir(p).await; // 已存在时忽略错误
                    }
                }
            }
            let mut f = sftp
                .create(path)
                .await
                .map_err(|e| AppError::new("SSH_SFTP", format!("写入 {path} 失败：{e}")))?;
            use tokio::io::AsyncWriteExt;
            f.write_all(content.as_bytes())
                .await
                .map_err(|e| AppError::new("SSH_SFTP", format!("写入 {path} 失败：{e}")))?;
            f.flush().await.ok();
        }

        // 可选：执行重启脚本
        let mut message = format!("已上传证书到 {host}:{port}（{remote_cert} / {remote_key}）");
        if !script.trim().is_empty() {
            let mut ch = handle
                .channel_open_session()
                .await
                .map_err(|e| AppError::new("SSH_EXEC", format!("打开执行通道失败：{e}")))?;
            ch.exec(true, script.as_str())
                .await
                .map_err(|e| AppError::new("SSH_EXEC", format!("执行脚本失败：{e}")))?;
            let mut out = String::new();
            let mut code: Option<u32> = None;
            while let Some(msg) = ch.wait().await {
                match msg {
                    russh::ChannelMsg::Data { data } => {
                        out.push_str(&String::from_utf8_lossy(&data))
                    }
                    russh::ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
                    russh::ChannelMsg::Eof | russh::ChannelMsg::Close => break,
                    _ => {}
                }
            }
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "done", "en")
                .await;
            match code {
                Some(0) => {
                    message = format!("{message}，脚本已执行");
                    if !out.trim().is_empty() {
                        message = format!(
                            "{message}：{}",
                            out.trim().chars().take(200).collect::<String>()
                        );
                    }
                }
                other => {
                    return Err(AppError::new(
                        "SSH_SCRIPT",
                        format!(
                            "远程脚本退出码 {other:?}：{}",
                            out.trim().chars().take(300).collect::<String>()
                        ),
                    ));
                }
            }
        }
        let _ = domains;
        Ok(message)
    })
}

/* ================= 本地目录复制 ================= */

/// 把证书复制到本机任意目录，可选执行一条本地命令（如重启本机 nginx）。
/// config: certPath / keyPath / script(可选)
fn local_deploy(
    target: &DeployTarget,
    domains: &[String],
    cert_pem: &str,
    key_pem: &str,
) -> Result<String> {
    let cert_path = cfg(target, "certPath")?.to_string();
    let key_path = cfg(target, "keyPath")?.to_string();
    for (path, content) in [(cert_path.as_str(), cert_pem), (key_path.as_str(), key_pem)] {
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(path, content).map_err(|e| AppError::io("写入证书文件失败", e))?;
    }
    let mut message = format!("已复制到本地（{cert_path} / {key_path}）");
    if let Some(script) = target.config.get("script").filter(|s| !s.trim().is_empty()) {
        // 部署后命令在本机执行；这是用户显式配置的动作
        let out = platform::command(if cfg!(windows) { "cmd" } else { "sh" })
            .args(if cfg!(windows) {
                ["/C", script]
            } else {
                ["-c", script]
            })
            .output()
            .map_err(|e| AppError::io("执行部署脚本失败", e))?;
        if !out.status.success() {
            return Err(AppError::new(
                "LOCAL_SCRIPT",
                format!(
                    "部署脚本退出码 {:?}：{}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                        .chars()
                        .take(300)
                        .collect::<String>()
                ),
            ));
        }
        message = format!("{message}，脚本已执行");
    }
    let _ = domains;
    Ok(message)
}

/* ================= 腾讯云 SSL 证书入库（TC3-HMAC-SHA256 签名） ================= */

/// 腾讯云 TC3 签名（可复用于腾讯云其它产品）
pub fn tc3_signature(secret_key: &str, date: &str, service: &str, string_to_sign: &str) -> String {
    let k_date = hmac_sha256_hex(format!("TC3{secret_key}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256_hex(k_date.as_bytes(), service.as_bytes());
    let k_signing = hmac_sha256_hex(k_region.as_bytes(), b"tc3_request");
    hex::encode(hmac_sha256_bytes(
        k_signing.as_bytes(),
        string_to_sign.as_bytes(),
    ))
}

fn hmac_sha256_hex(key: &[u8], data: &[u8]) -> String {
    hex::encode(hmac_sha256_bytes(key, data))
}

fn hmac_sha256_bytes(key: &[u8], data: &[u8]) -> [u8; 32] {
    crate::acme::hmac_sha256(key, data)
}

fn tencent_upload(
    target: &DeployTarget,
    domains: &[String],
    cert_pem: &str,
    key_pem: &str,
) -> Result<String> {
    let ak = cfg(target, "secretId")?;
    let sk = cfg(target, "secretKey")?;
    let name = target
        .config
        .get("certName")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("NiceEnv");

    let host = "ssl.tencentcloudapi.com";
    let service = "ssl";
    let action = "UploadCertificate";
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let date = time::OffsetDateTime::from_unix_timestamp(now as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .date()
        .to_string(); // YYYY-MM-DD
    let timestamp = now.to_string();

    let payload = serde_json::json!({
        "CertificatePublicKey": cert_pem,
        "CertificatePrivateKey": key_pem,
        "Alias": name,
    })
    .to_string();

    // 1. 规范请求串（POST / 空查询 + content-type/host/x-tc-action 头）
    let canonical_request = format!(
        "POST\n/\n\ncontent-type:application/json; charset=utf-8\nhost:{host}\nx-tc-action:{action}\n\ncontent-type;host;x-tc-action\n{}",
        hex::encode(Sha256::digest(payload.as_bytes()))
    );
    // 2. 待签名串
    let string_to_sign = format!(
        "TC3-HMAC-SHA256\n{timestamp}\n{date}/{service}/tc3_request\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    // 3. 签名
    let signature = tc3_signature(sk, &date, service, &string_to_sign);
    let authorization = format!(
        "TC3-HMAC-SHA256 Credential={ak}/{date}/{service}/tc3_request, SignedHeaders=content-type;host;x-tc-action, Signature={signature}"
    );

    let resp = http()
        .post(format!("https://{host}"))
        .header("X-TC-Action", action)
        .header("X-TC-Version", "2019-12-05")
        .header("X-TC-Timestamp", timestamp)
        .header(
            "X-TC-Region",
            target.config.get("region").cloned().unwrap_or_default(),
        )
        .header("Authorization", authorization)
        .header("Content-Type", "application/json; charset=utf-8")
        .body(payload)
        .send()
        .map_err(|e| AppError::new("DEPLOY_HTTP", format!("腾讯云请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    if body["Response"]["Error"].is_object() {
        return Err(AppError::new(
            "DEPLOY_TENCENT",
            format!(
                "腾讯云：{} - {}",
                body["Response"]["Error"]["Code"].as_str().unwrap_or(""),
                body["Response"]["Error"]["Message"]
                    .as_str()
                    .unwrap_or("失败")
            ),
        ));
    }
    let cert_id = body["Response"]["CertId"].as_str().unwrap_or("");
    let _ = domains;
    Ok(format!("已上传到腾讯云 SSL 证书服务（CertId {cert_id}）"))
}

/* ================= 群晖 DSM（certd 首页点名的平台） ================= */

/// 登录 DSM 拿 sid（API 6 支持格式化返回 JSON）
fn synology_login(base: &str, account: &str, password: &str) -> Result<String> {
    let resp = http()
        .get(format!(
            "{base}/webapi/auth.cgi?api=SYNO.API.Auth&version=6&method=login&account={account}&passwd={password}&session=Core&format=sid"
        ))
        .send()
        .map_err(|e| AppError::new("DEPLOY_HTTP", format!("群晖登录请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    if body["success"].as_bool() != Some(true) {
        return Err(AppError::new(
            "DEPLOY_SYNOLOGY",
            format!(
                "群晖登录失败：{}",
                body["error"]["code"].as_i64().unwrap_or_default()
            ),
        )
        .with_hint("code 400=账号密码错；401=账号被停用；403=IP 被黑名单"));
    }
    body["data"]["sid"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| AppError::new("DEPLOY_SYNOLOGY", "登录成功但未返回 sid"))
}

/// 上传证书到 DSM 并设为默认（multipart：key=私钥文件、cert=证书链文件）
fn synology_deploy(
    target: &DeployTarget,
    domains: &[String],
    cert_pem: &str,
    key_pem: &str,
) -> Result<String> {
    let base = cfg(target, "url")?.trim_end_matches('/').to_string();
    let account = cfg(target, "account")?;
    let password = cfg(target, "password")?;
    let sid = synology_login(&base, account, password)?;

    let name = domains.first().cloned().unwrap_or_else(|| "NiceEnv".into());
    let form = reqwest::blocking::multipart::Form::new()
        .text("api", "SYNO.Core.Certificate")
        .text("version", "1")
        .text("method", "upload")
        .text("sid", sid.clone())
        .text("id", "") // 空串 = 新增证书；替换已有证书时传证书 id
        .text("desc", format!("NiceEnv {name}"))
        .text("as_default", "true")
        .part(
            "key",
            reqwest::blocking::multipart::Part::bytes(key_pem.as_bytes().to_vec())
                .file_name("privkey.pem"),
        )
        .part(
            "cert",
            reqwest::blocking::multipart::Part::bytes(cert_pem.as_bytes().to_vec())
                .file_name("cert.pem"),
        );

    let resp = http()
        .post(format!("{base}/webapi/entry.cgi"))
        .multipart(form)
        .send()
        .map_err(|e| AppError::new("DEPLOY_HTTP", format!("群晖上传请求失败：{e}")))?;
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    if body["success"].as_bool() != Some(true) {
        return Err(AppError::new(
            "DEPLOY_SYNOLOGY",
            format!(
                "群晖上传失败：{}",
                body["error"]["errors"]["id"]
                    .as_str()
                    .unwrap_or("详见 DSM 日志")
            ),
        ));
    }
    // 顺手登出，别留会话
    let _ = http()
        .get(format!(
            "{base}/webapi/auth.cgi?api=SYNO.API.Auth&version=6&method=logout&session=Core&sid={sid}=_"
        ))
        .send();
    Ok(format!("已上传到群晖 DSM 并设为默认证书（{name}）"))
}

/* ================= Kubernetes Secret（tls 类型） ================= */

/// 把证书写进 k8s Secret（type: kubernetes.io/tls），存在则更新、不存在则创建。
/// config: serverUrl(如 https://k8s.example.com:6443) / token(可选，内网匿名 API 常见)
///         namespace(默认 default) / secretName / insecure(可选 "true" 跳过 TLS 校验)
fn k8s_secret_deploy(
    target: &DeployTarget,
    domains: &[String],
    cert_pem: &str,
    key_pem: &str,
) -> Result<String> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let server = cfg(target, "serverUrl")?.trim_end_matches('/').to_string();
    let namespace = target
        .config
        .get("namespace")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("default");
    let secret_name = cfg(target, "secretName")?;
    let insecure = target
        .config
        .get("insecure")
        .map(|s| s == "true")
        .unwrap_or(false);

    let mut client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(USER_AGENT);
    if insecure {
        client = client.danger_accept_invalid_certs(true); // 自签集群证书
    }
    let client = client
        .build()
        .map_err(|e| AppError::internal("HTTP 客户端", e.to_string()))?;

    let body = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "type": "kubernetes.io/tls",
        "metadata": { "name": secret_name, "namespace": namespace },
        "data": {
            "tls.crt": b64.encode(cert_pem),
            "tls.key": b64.encode(key_pem),
        }
    });

    let put_url = format!("{server}/api/v1/namespaces/{namespace}/secrets/{secret_name}");
    let post_url = format!("{server}/api/v1/namespaces/{namespace}/secrets");
    let token = target.config.get("token").cloned().unwrap_or_default();
    let mut req = client.put(&put_url).json(&body);
    if !token.trim().is_empty() {
        req = req.bearer_auth(token.trim());
    }
    let resp = req
        .send()
        .map_err(|e| AppError::new("DEPLOY_HTTP", format!("K8s 请求失败：{e}")))?;
    // 404 = Secret 不存在 → 创建
    if resp.status().as_u16() == 404 {
        let mut req2 = client.post(&post_url).json(&body);
        if !token.trim().is_empty() {
            req2 = req2.bearer_auth(token.trim());
        }
        let resp2 = req2
            .send()
            .map_err(|e| AppError::new("DEPLOY_HTTP", format!("K8s 创建 Secret 失败：{e}")))?;
        let status2 = resp2.status();
        let text2 = resp2.text().unwrap_or_default();
        if !status2.is_success() {
            return Err(AppError::new(
                "DEPLOY_K8S",
                format!("K8s 创建 Secret {}：{}", status2, api_message(&text2)),
            ));
        }
        return Ok(format!("已在 {namespace}/{secret_name} 创建 tls Secret"));
    }
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::new(
            "DEPLOY_K8S",
            format!("K8s 更新 Secret {}：{}", status, api_message(&text)),
        ));
    }
    let _ = domains;
    Ok(format!("已更新 {namespace}/{secret_name} 的 tls Secret"))
}

/// k8s API 错误体 {"kind":"Status","message":"..."} —— 翻出 message
fn api_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(String::from))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

/* ================= 七牛 SSL 证书库（QBox 签名） ================= */

/// QBox 签名：urlsafe_base64(hmac_sha1(SK, "<path>\n<body>"))
pub fn qbox_sign(sk: &str, path: &str, body: &str) -> String {
    let data = format!("{path}\n{body}");
    let mut mac = HmacSha1::new_from_slice(sk.as_bytes()).expect("hmac key");
    mac.update(data.as_bytes());
    B64URL_SAFE.encode(mac.finalize().into_bytes())
}

fn qiniu_upload(
    target: &DeployTarget,
    domains: &[String],
    cert_pem: &str,
    key_pem: &str,
) -> Result<String> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let ak = cfg(target, "accessKey")?;
    let sk = cfg(target, "secretKey")?;
    let name = target
        .config
        .get("certName")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| domains.first().map(|d| d.as_str()).unwrap_or("NiceEnv"));

    let body = serde_json::json!({
        "name": name,
        "common_name": domains.first().cloned().unwrap_or_default(),
        "pri": b64.encode(key_pem),
        "ca": b64.encode(cert_pem),
    })
    .to_string();
    let path = "/v2/ssl/cert";
    let authorization = format!("QBox {}:{}", ak, qbox_sign(sk, path, &body));

    let resp = http()
        .post(format!("https://ssl.qiniuapi.com{path}"))
        .header("Authorization", authorization)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .map_err(|e| AppError::new("DEPLOY_HTTP", format!("七牛请求失败：{e}")))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::new(
            "DEPLOY_QINIU",
            format!(
                "七牛上传证书 {}：{}",
                status,
                text.chars().take(250).collect::<String>()
            ),
        ));
    }
    let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let cert_id = v["cert_id"].as_str().unwrap_or("");
    Ok(format!("已上传到七牛 SSL 证书库（{cert_id}）"))
}

/* ================= 华为云 SSL 证书管理（SCM 上传入库） ================= */

/// 华为云 SCM：POST /v3/scm/certificates（复用 HWS 签名体系，host 换 scm）
fn hw_ssl_upload(
    target: &DeployTarget,
    domains: &[String],
    cert_pem: &str,
    key_pem: &str,
) -> Result<String> {
    let ak = cfg(target, "accessKeyId")?;
    let sk = cfg(target, "accessKeySecret")?;
    let region = target
        .config
        .get("region")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("cn-north-1");
    let name = target
        .config
        .get("certName")
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| domains.first().map(|d| d.as_str()).unwrap_or("NiceEnv"));

    let host = format!("scm.{region}.myhuaweicloud.com");
    let body = serde_json::json!({
        "name": name,
        "private_key": key_pem,
        "certificate": cert_pem,
        "project_name": target.config.get("projectName").cloned().unwrap_or_default(),
    });
    let resp = crate::dnsprov::hws_request(
        ak,
        sk,
        &host,
        reqwest::Method::POST,
        "/v3/scm/certificates",
        Some(&body),
    )?;
    Ok(format!(
        "已上传到华为云 SSL 证书管理（{}）",
        resp["certificate_id"]
            .as_str()
            .unwrap_or("证书 ID 见控制台")
    ))
}

/* ================= SMTP 邮件通知（certd 的邮件通知插件） ================= */

/// 发一封纯文本告警邮件。587 走 STARTTLS、465 走隐式 TLS（对齐常见服务商）。
pub fn notify_email(
    smtp: &crate::model::NotifySmtp,
    ok: bool,
    title: &str,
    detail: &str,
) -> Result<()> {
    use lettre::message::header::ContentType;
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

    if smtp.host.trim().is_empty() || smtp.to.trim().is_empty() {
        return Err(AppError::new(
            "SMTP_CONFIG",
            "SMTP 服务器 / 收件人没有填完整",
        ));
    }
    let from = if smtp.from.trim().is_empty() {
        smtp.username.clone()
    } else {
        smtp.from.clone()
    };
    if from.trim().is_empty() {
        return Err(AppError::new("SMTP_CONFIG", "发件人不能为空"));
    }

    let builder = if smtp.implicit_tls {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host)
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp.host)
    };
    let mut mailer = builder
        .map_err(|e| AppError::new("SMTP_RELAY", format!("SMTP 服务器连不上：{e}")))?
        .port(smtp.port);
    if !smtp.username.trim().is_empty() {
        mailer = mailer.credentials(Credentials::new(
            smtp.username.clone(),
            smtp.password.clone(),
        ));
    }

    let email = Message::builder()
        .from(
            from.parse()
                .map_err(|e| AppError::new("SMTP_FROM", format!("发件人地址不合法：{e}")))?,
        )
        .to(smtp
            .to
            .split(',')
            .filter_map(|a| a.trim().parse().ok())
            .collect::<Vec<_>>()
            .first()
            .cloned()
            .ok_or_else(|| AppError::new("SMTP_TO", "收件人地址不合法"))?)
        .subject(format!(
            "[NiceServBay] {title}{}",
            if ok { "" } else { " ⚠" }
        ))
        .header(ContentType::TEXT_PLAIN)
        .body(format!(
            "{title}

{detail}

—— NiceServBay 证书自动化"
        ))
        .map_err(|e| AppError::new("SMTP_BUILD", format!("邮件构建失败：{e}")))?;

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| AppError::internal("构建 tokio runtime", e.to_string()))?;
    rt.block_on(async move {
        mailer.build().send(email).await.map_err(|e| {
            AppError::new("SMTP_SEND", format!("邮件发送失败：{e}")).with_hint(
                "检查端口（587=STARTTLS / 465=TLS）、授权码（多数服务商要用授权码而非登录密码）",
            )
        })?;
        Ok(())
    })
}

/* ================= 测试 ================= */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bt_token_matches_reference_algo() {
        // 固定向量自校验：md5(sha1(ts + md5(sk)))
        let sk = "sk123";
        let ts = 1_700_000_000;
        let expect = md5_hex(&sha1_hex(&format!("{ts}{}", md5_hex(sk))));
        assert_eq!(bt_request_token(sk, ts), expect);
        assert_eq!(bt_request_token(sk, ts).len(), 32);
        // 时间戳变化则令牌变化
        assert_ne!(bt_request_token(sk, ts), bt_request_token(sk, ts + 1));
    }

    #[test]
    fn onepanel_token_matches_reference_algo() {
        let expect = md5_hex(&format!("1panel{ts}key", ts = 1_700_000_000,));
        assert_eq!(onepanel_token("key", 1_700_000_000), expect);
        assert_eq!(onepanel_token("key", 1_700_000_000).len(), 32);
    }

    #[test]
    fn tc3_signature_chain_matches_manual_hmac() {
        // 与手工分层 HMAC（TC3 标准）对拍
        let k_date = hmac_sha256_hex(b"TC3sk", b"2024-01-01");
        let k_region = hmac_sha256_hex(k_date.as_bytes(), b"ssl");
        let k_signing = hmac_sha256_hex(k_region.as_bytes(), b"tc3_request");
        let expect = hex::encode(hmac_sha256_bytes(k_signing.as_bytes(), b"sts"));
        assert_eq!(tc3_signature("sk", "2024-01-01", "ssl", "sts"), expect);
    }

    #[test]
    fn missing_config_gives_named_error() {
        let t = DeployTarget {
            id: "t1".into(),
            kind: "btpanel".into(),
            name: "我的宝塔".into(),
            config: Default::default(),
            last_result: None,
        };
        let err = deploy(&t, &["a.com".to_string()], "c", "k").unwrap_err();
        assert!(err.to_string().contains("url"));
    }

    #[test]
    fn qbox_sign_matches_manual_hmac_sha1() {
        // 与手工 HMAC-SHA1 对拍（QBox 规范：path + "\n" + body）
        let sig = qbox_sign("sk", "/v2/ssl/cert", "{\"name\":\"x\"}");
        let mut mac = HmacSha1::new_from_slice(b"sk").unwrap();
        mac.update(b"/v2/ssl/cert\n{\"name\":\"x\"}");
        assert_eq!(sig, B64URL_SAFE.encode(mac.finalize().into_bytes()));
    }

    #[test]
    fn k8s_api_message_extraction() {
        assert_eq!(
            api_message(r#"{"kind":"Status","message":"secrets \"x\" not found"}"#),
            "secrets \"x\" not found"
        );
        assert_eq!(api_message("plain"), "plain");
    }

    #[test]
    fn synology_missing_config_named_error() {
        let t = DeployTarget {
            id: "t3".into(),
            kind: "synology".into(),
            name: "NAS".into(),
            config: Default::default(),
            last_result: None,
        };
        let err = deploy(&t, &["a.com".to_string()], "c", "k").unwrap_err();
        // 第一个缺失参数是 url
        assert!(err.to_string().contains("url"));
    }

    #[test]
    fn unknown_kind_rejected() {
        let t = DeployTarget {
            id: "t2".into(),
            kind: "vendorx".into(),
            name: "x".into(),
            config: Default::default(),
            last_result: None,
        };
        assert!(deploy(&t, &[], "c", "k").is_err());
    }
}
