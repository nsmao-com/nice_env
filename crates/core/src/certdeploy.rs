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

fn cloud_http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none()).build().expect("cloud http client")
}

/// HTTP 成功与有效对象都必须成立；不把网关错误、登录页或空响应当成部署成功。
fn deployment_json(response: reqwest::blocking::Response, provider: &str) -> Result<serde_json::Value> {
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::new("DEPLOY_HTTP", format!("{provider} 返回 HTTP {status}，未确认部署成功")));
    }
    let body: serde_json::Value = response.json()
        .map_err(|_| AppError::new("DEPLOY_RESPONSE", format!("{provider} 未返回有效 JSON，请检查面板地址与 API 设置")))?;
    if !body.is_object() {
        return Err(AppError::new("DEPLOY_RESPONSE", format!("{provider} 响应格式异常，未确认部署成功")));
    }
    Ok(body)
}

fn upload_id(value: &serde_json::Value, provider: &str) -> Result<String> {
    let id = value.as_str().filter(|v| !v.trim().is_empty()).map(str::to_owned)
        .or_else(|| value.as_u64().filter(|v| *v > 0).map(|v| v.to_string()));
    id.ok_or_else(|| AppError::new("DEPLOY_RESPONSE", format!("{provider} 未返回证书标识，无法确认上传成功")))
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
    let resp = reqwest::blocking::Client::builder().timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none()).user_agent(USER_AGENT).build()
        .map_err(|_| AppError::new("NOTIFY_HTTP", "无法创建通知连接"))?
        .post(url.trim())
        .json(&body)
        .send()
        .map_err(|_| AppError::new("NOTIFY_HTTP", "通知发送失败，请检查 Webhook 地址与网络连接"))?;
    if !resp.status().is_success() {
        return Err(AppError::new(
            "NOTIFY_HTTP",
            format!("通知端点返回 {}", resp.status()),
        ));
    }
    if matches!(kind, "dingtalk" | "wecom" | "feishu") {
        let body: serde_json::Value = resp.json().map_err(|_| AppError::new("NOTIFY_RESPONSE", "通知端点未返回有效确认"))?;
        let code = if kind == "feishu" { body.get("code").or_else(|| body.get("StatusCode")) } else { body.get("errcode") };
        if code.and_then(|v| v.as_i64()) != Some(0) {
            return Err(AppError::new("NOTIFY_REJECTED", "通知被服务端拒绝，请检查机器人地址、关键词或签名配置"));
        }
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
    let body = deployment_json(resp, "宝塔")?;
    // 宝塔错误返回 {status:false, msg:"..."}
    if body.get("status") == Some(&serde_json::Value::Bool(false)) {
        return Err(AppError::new(
            "DEPLOY_BT",
            format!(
                "宝塔：{}",
                body["msg"]
                    .as_str()
                    .unwrap_or("接口拒绝了请求")
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
    let result = bt_post(
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
    if result["status"].as_bool() != Some(true) {
        return Err(AppError::new("DEPLOY_BT", "宝塔未明确确认站点证书配置成功，请检查站点与 API 响应"));
    }
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
    let body = deployment_json(resp, "1Panel")?;
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
    // 同一账号的证书名称不能重复；续签新证书用新名称，重试同一证书保持幂等凭据。
    let fingerprint = hex::encode(Sha256::digest(cert_pem.as_bytes()));
    let name = format!("{}-{}", name.chars().take(50).collect::<String>(), &fingerprint[..12]);
    let client_token = hex::encode(Sha256::digest(format!("{name}\n{cert_pem}").as_bytes()));

    let mut params: Vec<(String, String)> = vec![
        ("Format".into(), "JSON".into()),
        ("Version".into(), "2020-04-07".into()),
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
        ("Name".into(), name),
        ("ClientToken".into(), client_token),
        ("Cert".into(), cert_pem.into()),
        ("Key".into(), key_pem.into()),
    ];
    params.sort();
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", aliyun_percent_encode(k), aliyun_percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let string_to_sign = format!(
        "POST&{}&{}",
        aliyun_percent_encode("/"),
        aliyun_percent_encode(&query)
    );
    let signature = aliyun_percent_encode(&aliyun_sign(sk, &string_to_sign));

    // 与官方 CAS 2020-04-07 SDK 的区域映射一致；其余区域沿用 regional endpoint 规则。
    let shared_endpoint = region.starts_with("cn-") || matches!(region,
        "ap-northeast-2-pop" | "ap-southeast-3" | "ap-southeast-5" | "eu-west-1" | "eu-west-1-oxs"
        | "rus-west-1-pop" | "us-east-1" | "us-west-1");
    let endpoint = if shared_endpoint { "cas.aliyuncs.com".to_string() } else { format!("cas.{region}.aliyuncs.com") };
    let resp = cloud_http()
        .post(format!("https://{endpoint}/"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("Signature={signature}&{query}"))
        .send()
        .map_err(|e| AppError::new("DEPLOY_HTTP", format!("阿里云 CAS 请求失败：{e}")))?;
    let body = deployment_json(resp, "阿里云 CAS")?;
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
    let cert_id = upload_id(&body["CertId"], "阿里云 CAS")?;
    Ok(format!("已上传到阿里云 SSL 证书服务（证书 {cert_id}）"))
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

/// SSH 登录密钥与证书输出路径分开配置；主机指纹必须由用户先确认。
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SshHostKey {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
}

fn ssh_endpoint(host: &str, port: u16) -> Result<(String, u16)> {
    let host = host.trim().trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || port == 0 || host.contains(['/', '\\', '@', '\0']) || host.chars().any(char::is_whitespace) {
        return Err(AppError::new("SSH_CONFIG", "请填写有效的 SSH 主机地址和 1–65535 端口，不要包含协议或登录用户名"));
    }
    Ok((host.into(), port))
}

fn ssh_port(target: &DeployTarget) -> Result<u16> {
    match target.config.get("port").map(|v| v.trim()).filter(|v| !v.is_empty()) {
        None => Ok(22),
        Some(value) => value.parse::<u16>().ok().filter(|v| *v > 0)
            .ok_or_else(|| AppError::new("SSH_CONFIG", "SSH 端口必须是 1–65535 的整数")),
    }
}

fn script_timeout(target: &DeployTarget) -> Result<Duration> {
    let seconds = target.config.get("timeoutSec").filter(|v| !v.trim().is_empty())
        .map(|v| v.parse::<u64>()).transpose()
        .map_err(|_| AppError::new("DEPLOY_CONFIG", "脚本超时必须是 1–600 秒的整数"))?.unwrap_or(60);
    if !(1..=600).contains(&seconds) { return Err(AppError::new("DEPLOY_CONFIG", "脚本超时必须是 1–600 秒的整数")); }
    Ok(Duration::from_secs(seconds))
}

fn remote_path(path: &str) -> Result<()> {
    if !path.starts_with('/') || path.ends_with('/') || path.contains(['\0', '\\'])
        || path.chars().any(char::is_control) || path[1..].split('/').any(|p| p.is_empty() || p == "." || p == "..") {
        return Err(AppError::new("SSH_PATH", "远程证书和私钥请填写完整的 Unix 文件路径，如 /etc/nginx/ssl/fullchain.pem"));
    }
    Ok(())
}

/// 保存及签发前校验，避免签完才发现本地/SSH 目标根本无法执行。
pub(crate) fn validate_target(target: &DeployTarget) -> Result<()> {
    if !matches!(target.kind.as_str(), "local" | "ssh") { return Ok(()); }
    script_timeout(target)?;
    let cert = cfg(target, "certPath")?;
    let key = cfg(target, "keyPath")?;
    if cert == key { return Err(AppError::new("DEPLOY_PATH", "证书和私钥必须保存到两个不同文件")); }
    if target.kind == "local" {
        for path in [cert, key] {
            local_path(path)?;
        }
        if cfg!(windows) && cert.eq_ignore_ascii_case(key) { return Err(AppError::new("DEPLOY_PATH", "证书和私钥不能指向同一文件")); }
    } else {
        ssh_endpoint(cfg(target, "host")?, ssh_port(target)?)?;
        cfg(target, "user")?; remote_path(cert)?; remote_path(key)?;
        match target.config.get("auth").map(String::as_str).unwrap_or("password") {
            "password" => { cfg(target, "password")?; }
            "key" => {
                let path = target.config.get("identityFile").or_else(|| target.config.get("privateKey"))
                    .filter(|v| !v.trim().is_empty()).ok_or_else(|| AppError::new("SSH_CONFIG", "请选择用于 SSH 登录的私钥文件；远程证书私钥路径不能用来登录"))?;
                if !std::path::Path::new(path).is_absolute() { return Err(AppError::new("SSH_CONFIG", "SSH 登录私钥必须是本机的绝对文件路径")); }
            }
            _ => return Err(AppError::new("SSH_CONFIG", "请选择密码登录或密钥登录")),
        }
        let fingerprint = target.config.get("hostFingerprint").map(String::as_str).unwrap_or("");
        if !fingerprint.starts_with("SHA256:") || fingerprint.len() != 50 {
            return Err(AppError::new("SSH_HOST_KEY", "请先读取并确认 SSH 主机指纹，再保存部署目标"));
        }
    }
    Ok(())
}

struct VerifyHost {
    expected: Option<String>,
    seen: std::sync::Arc<parking_lot::Mutex<Option<String>>>,
}
impl russh::client::Handler for VerifyHost {
    type Error = russh::Error;
    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> std::result::Result<bool, Self::Error> {
        let fingerprint = key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
        *self.seen.lock() = Some(fingerprint.clone());
        // 探测只读取密钥，拒绝继续握手；不会发送用户名、密码或私钥认证请求。
        Ok(self.expected.as_ref() == Some(&fingerprint))
    }
}

pub fn probe_ssh(host: &str, port: u16) -> Result<SshHostKey> {
    let _work = crate::BackgroundWork::begin("读取 SSH 主机指纹")?;
    let (host, port) = ssh_endpoint(host, port)?;
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(None));
    let handler = VerifyHost { expected: None, seen: seen.clone() };
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(async { tokio::time::timeout(Duration::from_secs(15), async {
        russh::client::connect(std::sync::Arc::new(russh::client::Config::default()), (host.as_str(), port), handler).await
    }).await });
    let fingerprint = seen.lock().clone();
    if let Some(fingerprint) = fingerprint { return Ok(SshHostKey { host, port, fingerprint }); }
    Err(AppError::new("SSH_CONNECT", match result {
        Err(_) => "读取主机指纹超时，请检查地址、端口和网络".into(),
        Ok(Err(error)) => format!("无法读取 SSH 主机指纹：{error}"),
        Ok(Ok(_)) => "服务端未提供可验证的 SSH 主机密钥".into(),
    }))
}

async fn ssh_step<T, E: std::fmt::Display>(operation: impl std::future::Future<Output = std::result::Result<T, E>>, label: &str) -> Result<T> {
    tokio::time::timeout(Duration::from_secs(15), operation).await
        .map_err(|_| AppError::new("SSH_TIMEOUT", format!("{label}超时")))?
        .map_err(|error| AppError::new("SSH_OPERATION", format!("{label}失败：{error}")))
}

async fn remote_metadata(sftp: &russh_sftp::client::SftpSession, path: &str) -> Result<Option<russh_sftp::protocol::FileAttributes>> {
    match sftp.symlink_metadata(path).await {
        Ok(meta) => Ok(Some(meta)),
        Err(russh_sftp::client::error::Error::Status(status)) if status.status_code == russh_sftp::protocol::StatusCode::NoSuchFile => Ok(None),
        Err(error) => Err(AppError::new("SSH_SFTP", format!("检查远程路径 {path} 失败：{error}"))),
    }
}

async fn remote_pair(sftp: &russh_sftp::client::SftpSession, cert: &str, key: &str, cert_pem: &str, key_pem: &str) -> Result<String> {
    use russh_sftp::protocol::{FileAttributes, OpenFlags};
    use tokio::io::AsyncWriteExt;
    struct Entry { path: String, staged: String, backup: String, old: Option<FileAttributes>, moved: bool, installed: bool }
    let nonce = format!("{:032x}", rand::random::<u128>());
    let mut entries = Vec::new();
    for path in [cert, key] {
        // 不跟随父目录软链接；逐层确认或创建目录，不能忽略权限错误。
        let mut parent = String::new();
        let parts = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
        for part in &parts[..parts.len() - 1] {
            parent.push('/'); parent.push_str(part);
            match remote_metadata(sftp, &parent).await? {
                Some(meta) if meta.is_dir() => {}
                Some(_) => return Err(AppError::new("SSH_PATH", format!("远程父路径不是普通目录：{parent}"))),
                None => sftp.create_dir(parent.clone()).await.map_err(|e| AppError::new("SSH_SFTP", format!("创建远程目录失败：{e}")))?,
            }
        }
        let old = remote_metadata(sftp, path).await?;
        if old.as_ref().is_some_and(|meta| !meta.is_regular()) { return Err(AppError::new("SSH_PATH", format!("远程目标不是普通文件：{path}"))); }
        entries.push(Entry { path: path.into(), staged: format!("{path}.niceenv-{nonce}.new"), backup: format!("{path}.niceenv-{nonce}.old"), old, moved: false, installed: false });
    }
    let mut commit_started = false;
    let result: Result<()> = async {
        for (index, content) in [cert_pem, key_pem].into_iter().enumerate() {
            let entry = &entries[index];
            let old = entry.old.clone().unwrap_or_default();
            let mode = if index == 1 { old.permissions.unwrap_or(0o600) & 0o660 } else { old.permissions.unwrap_or(0o644) & 0o777 };
            let attributes = FileAttributes { permissions: Some(mode), uid: old.uid, gid: old.gid, ..Default::default() };
            let mut file = sftp.open_with_flags_and_attributes(entry.staged.clone(), OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE, attributes)
                .await.map_err(|e| AppError::new("SSH_SFTP", format!("创建远程暂存文件失败：{e}")))?;
            file.write_all(content.as_bytes()).await?;
            file.flush().await?;
            file.sync_all().await.map_err(|e| AppError::new("SSH_SFTP", format!("同步远程文件失败：{e}")))?;
            file.close().await?;
        }
        for entry in &mut entries {
            commit_started = true;
            if entry.old.is_some() {
                sftp.rename(entry.path.clone(), entry.backup.clone()).await.map_err(|e| AppError::new("SSH_SFTP", format!("保留远程原文件失败：{e}")))?;
                entry.moved = true;
            }
            sftp.rename(entry.staged.clone(), entry.path.clone()).await.map_err(|e| AppError::new("SSH_SFTP", format!("发布远程证书文件失败：{e}")))?;
            entry.installed = true;
        }
        Ok(())
    }.await;
    if let Err(error) = result {
        let mut failures = Vec::new();
        for entry in entries.iter().rev() {
            if entry.installed {
                if let Err(e) = sftp.remove_file(entry.path.clone()).await { failures.push(e.to_string()); }
            }
            if entry.moved {
                if let Err(e) = sftp.rename(entry.backup.clone(), entry.path.clone()).await { failures.push(e.to_string()); }
            }
            if let Err(e) = sftp.remove_file(entry.staged.clone()).await {
                if !matches!(e, russh_sftp::client::error::Error::Status(ref status) if status.status_code == russh_sftp::protocol::StatusCode::NoSuchFile) { failures.push(e.to_string()); }
            }
        }
        if commit_started || !failures.is_empty() {
            return Err(AppError::new("DEPLOY_UNCERTAIN", format!("远程证书写入未完成，已尝试恢复原文件。请核对证书、私钥和 .niceenv-{nonce} 暂存/备份后重试：{error}"))
                .with_detail(failures.join("；")));
        }
        return Err(error);
    }
    let mut retained = Vec::new();
    for entry in entries {
        if entry.moved && sftp.remove_file(entry.backup.clone()).await.is_err() { retained.push(entry.backup); }
    }
    Ok(if retained.is_empty() { String::new() } else { format!("；旧备份未能清理，请检查：{}", retained.join("、")) })
}

fn ssh_deploy(target: &DeployTarget, _domains: &[String], cert_pem: &str, key_pem: &str) -> Result<String> {
    validate_target(target)?;
    let (host, port) = ssh_endpoint(cfg(target, "host")?, ssh_port(target)?)?;
    let user = cfg(target, "user")?;
    let seen = std::sync::Arc::new(parking_lot::Mutex::new(None));
    let expected = cfg(target, "hostFingerprint")?.to_string();
    let handler = VerifyHost { expected: Some(expected.clone()), seen: seen.clone() };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let connected = ssh_step(russh::client::connect(std::sync::Arc::new(russh::client::Config::default()), (host.as_str(), port), handler), "SSH 连接").await;
        if seen.lock().as_ref().is_some_and(|actual| actual != &expected) {
            return Err(AppError::new("SSH_HOST_KEY_CHANGED", "SSH 主机指纹与已确认值不同，未发送登录凭据。请核对服务器后重新读取并确认指纹"));
        }
        let mut handle = connected?;
        let outcome = async {
            let authed = if target.config.get("auth").is_some_and(|v| v == "key") {
                let identity = target.config.get("identityFile").or_else(|| target.config.get("privateKey")).unwrap();
                let passphrase = target.config.get("keyPassphrase").filter(|v| !v.is_empty()).map(String::as_str);
                let key = russh::keys::load_secret_key(identity, passphrase).map_err(|e| AppError::new("SSH_AUTH", format!("读取 SSH 登录私钥失败：{e}")))?;
                let hash = if key.algorithm().is_rsa() { ssh_step(handle.best_supported_rsa_hash(), "协商 RSA 签名算法").await?.flatten() } else { None };
                ssh_step(handle.authenticate_publickey(user, russh::keys::PrivateKeyWithHashAlg::new(std::sync::Arc::new(key), hash)), "SSH 密钥认证").await?
            } else { ssh_step(handle.authenticate_password(user, cfg(target, "password")?), "SSH 密码认证").await? };
            if !authed.success() { return Err(AppError::new("SSH_AUTH", "SSH 登录被拒绝，请检查用户名和所选认证方式")); }
            let channel = ssh_step(handle.channel_open_session(), "打开 SFTP 通道").await?;
            ssh_step(channel.request_subsystem(true, "sftp"), "请求 SFTP 子系统").await?;
            let sftp = ssh_step(russh_sftp::client::SftpSession::new(channel.into_stream()), "初始化 SFTP").await?;
            sftp.set_timeout(10);
            let cert = cfg(target, "certPath")?; let key = cfg(target, "keyPath")?;
            let note = remote_pair(&sftp, cert, key, cert_pem, key_pem).await?;
            let mut message = format!("已上传证书到 {host}:{port}（{cert} / {key}）{note}");
            if let Some(script) = target.config.get("script").filter(|v| !v.trim().is_empty()) {
                let mut channel = ssh_step(handle.channel_open_session(), "打开脚本通道").await?;
                ssh_step(channel.exec(true, script.as_str()), "发送部署脚本").await
                    .map_err(|e| AppError::new("DEPLOY_UNCERTAIN", format!("未能确认远程脚本是否开始执行，请核对服务器：{e}")))?;
                let mut output = Vec::new(); let mut exit = None;
                let execution = tokio::time::timeout(script_timeout(target)?, async {
                    while let Some(message) = channel.wait().await {
                        match message {
                            russh::ChannelMsg::Data { data } | russh::ChannelMsg::ExtendedData { data, .. } => {
                                if output.len() + data.len() > 64 * 1024 { return Err("远程脚本输出超过 64 KiB"); }
                                output.extend_from_slice(&data);
                            }
                            russh::ChannelMsg::ExitStatus { exit_status } => exit = Some(exit_status),
                            russh::ChannelMsg::ExitSignal { .. } => return Err("远程脚本被信号终止"),
                            russh::ChannelMsg::Close => break,
                            // EOF 只表示输出结束，退出状态可能在其后到达。
                            _ => {}
                        }
                    }
                    Ok(())
                }).await;
                if !matches!(execution, Ok(Ok(()))) || exit != Some(0) {
                    let _ = tokio::time::timeout(Duration::from_secs(2), channel.signal(russh::Sig::TERM)).await;
                    let _ = tokio::time::timeout(Duration::from_secs(2), channel.close()).await;
                    let reason = match execution { Err(_) => "远程脚本超时".into(), Ok(Err(e)) => e.into(), Ok(Ok(())) => format!("远程脚本退出状态 {exit:?}") };
                    return Err(AppError::new("DEPLOY_UNCERTAIN", format!("{reason}，证书文件已上传。远程进程是否停止尚未确认，请核对后手动重试"))
                        .with_detail(String::from_utf8_lossy(&output).chars().take(1000).collect::<String>()));
                }
                message.push_str("，脚本已执行");
            }
            Ok(message)
        }.await;
        let _ = tokio::time::timeout(Duration::from_secs(2), handle.disconnect(russh::Disconnect::ByApplication, "done", "en")).await;
        outcome
    })
}

/* ================= 本地目录复制 ================= */

fn local_path(path: &str) -> Result<std::path::PathBuf> {
    let path = std::path::PathBuf::from(path);
    if !path.is_absolute() || path.components().any(|p| matches!(p, std::path::Component::ParentDir | std::path::Component::CurDir)) {
        return Err(AppError::new("DEPLOY_PATH", "请填写不含 . 或 .. 的本地绝对文件路径"));
    }
    let root = path.ancestors().last().ok_or_else(|| AppError::new("DEPLOY_PATH", "文件路径缺少根目录"))?;
    let relative = path.strip_prefix(root).map_err(|_| AppError::new("DEPLOY_PATH", "文件路径无效"))?.to_string_lossy();
    let relative = if cfg!(windows) { relative.replace('\\', "/") } else { relative.into_owned() };
    // 复用已有路径防护，拒绝链接/目录联接、Windows ADS、设备名与尾点别名。
    crate::paths::checked_data_path(root, &relative).map_err(|error| AppError::new("DEPLOY_PATH", format!("部署路径无效：{error}")))
}

pub(crate) fn local_pair(cert: &str, key: &str, cert_pem: &str, key_pem: &str) -> Result<()> {
    local_pair_with_publish(cert, key, cert_pem, key_pem, |file, path| file.persist(path).map(|_| ()).map_err(|e| e.error))
}

/// 用实际文件位置识别共用输出；规范化已有父目录，未创建的文件也能参与互斥。
pub(crate) fn local_output_resource(path: &str) -> Result<String> {
    let path = local_path(path)?;
    let mut parent = path.as_path();
    let mut suffix = Vec::new();
    let mut resolved = loop {
        match std::fs::canonicalize(parent) {
            Ok(path) => break path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(parent.file_name().ok_or_else(|| AppError::new("DEPLOY_PATH", "无法确认部署文件位置"))?.to_owned());
                parent = parent.parent().ok_or_else(|| AppError::new("DEPLOY_PATH", "部署路径缺少父目录"))?;
            }
            Err(error) => return Err(AppError::io("确认部署文件位置", error)),
        }
    };
    for part in suffix.into_iter().rev() { resolved.push(part); }
    let mut path = resolved.to_string_lossy().replace('\\', "/");
    if let Some(unc) = path.strip_prefix("//?/UNC/") { path = format!("//{unc}"); }
    else if let Some(plain) = path.strip_prefix("//?/") { path = plain.into(); }
    if cfg!(any(windows, target_os = "macos")) { path = path.to_lowercase(); }
    Ok(format!("file:{path}"))
}

/// 只包含会覆盖已有内容的声明目标，不从用户脚本猜测其任意副作用。
pub(crate) fn output_resources(target: &DeployTarget) -> Result<Vec<(String, String)>> {
    let mut resources = Vec::new();
    match target.kind.as_str() {
        "local" => for key in ["certPath", "keyPath"] {
            let path = cfg(target, key)?;
            resources.push((local_output_resource(path)?, format!("本地文件 {path}")));
        },
        "ssh" => {
            let (host, port) = ssh_endpoint(cfg(target, "host")?, ssh_port(target)?)?;
            let host = host.parse::<std::net::IpAddr>().map(|ip| ip.to_string()).unwrap_or_else(|_| host.trim_end_matches('.').to_ascii_lowercase());
            let fingerprint = cfg(target, "hostFingerprint")?;
            for key in ["certPath", "keyPath"] {
                let path = cfg(target, key)?; remote_path(path)?;
                let label = format!("SSH {host}:{port} 的 {path}");
                // 指纹覆盖同一主机的不同 DNS 别名；地址覆盖原地址更换主机密钥的情况。
                resources.push((format!("ssh-key:{fingerprint}:{path}"), label.clone()));
                resources.push((format!("ssh-host:{host}:{port}:{path}"), label));
            }
        }
        "btpanel" => {
            let url = reqwest::Url::parse(cfg(target, "url")?).map_err(|_| AppError::new("DEPLOY_CONFIG", "宝塔面板地址无效，请填写完整 HTTP/HTTPS 地址"))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() { return Err(AppError::new("DEPLOY_CONFIG", "宝塔面板地址必须是 HTTP/HTTPS 地址")); }
            let site = cfg(target, "siteName")?.trim().trim_end_matches('.').to_ascii_lowercase();
            resources.push((format!("bt:{}:{}:{}:{site}", url.host_str().unwrap(), url.port_or_known_default().unwrap(), url.path().trim_end_matches('/')), format!("宝塔站点 {site}")));
        }
        _ => {} // 证书库上传创建独立条目，不覆写本地/远程输出文件。
    }
    Ok(resources)
}

fn local_pair_with_publish(cert: &str, key: &str, cert_pem: &str, key_pem: &str,
    mut publish: impl FnMut(tempfile::NamedTempFile, &std::path::Path) -> std::io::Result<()>) -> Result<()> {
    use std::io::{Read, Write};
    let _files = crate::tls::CERT_FILES.lock();
    let paths = [local_path(cert)?, local_path(key)?];
    let comparable = |p: &std::path::Path| if cfg!(windows) { p.to_string_lossy().to_lowercase() } else { p.to_string_lossy().into_owned() };
    if comparable(&paths[0]) == comparable(&paths[1]) { return Err(AppError::new("DEPLOY_PATH", "证书和私钥不能指向同一文件")); }
    let mut previous = Vec::new(); let mut pending = Vec::new();
    for (path, content) in paths.iter().zip([cert_pem, key_pem]) {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() && meta.len() <= 8 * 1024 * 1024 => Some(meta),
            Ok(_) => return Err(AppError::new("DEPLOY_PATH", "已有证书/私钥必须是 8 MiB 以内的普通文件")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let old = if meta.is_some() {
            let mut bytes = Vec::new(); std::fs::File::open(path)?.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 8 * 1024 * 1024 { return Err(AppError::new("DEPLOY_PATH", "已有文件超过大小限制")); }
            Some(bytes)
        } else { None };
        let parent = path.parent().ok_or_else(|| AppError::new("DEPLOY_PATH", "缺少证书父目录"))?;
        std::fs::create_dir_all(parent)?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(content.as_bytes())?; staged.as_file().sync_all()?;
        if let Some(meta) = &meta { staged.as_file().set_permissions(meta.permissions())?; }
        previous.push((old, meta.map(|m| m.permissions()))); pending.push(staged);
    }
    for (index, staged) in pending.into_iter().enumerate() {
        if let Err(error) = publish(staged, &paths[index]) {
            let mut failures = Vec::new();
            for done in (0..index).rev() {
                let restored = match &previous[done].0 {
                    Some(bytes) => crate::paths::write_atomic(&paths[done], bytes).and_then(|_| {
                        if let Some(mode) = &previous[done].1 { std::fs::set_permissions(&paths[done], mode.clone()) } else { Ok(()) }
                    }),
                    None => std::fs::remove_file(&paths[done]),
                };
                if let Err(e) = restored { failures.push(e.to_string()); }
            }
            if !failures.is_empty() { return Err(AppError::new("DEPLOY_UNCERTAIN", format!("本地部署失败且原文件未能完整恢复，请核对证书和私钥：{}", failures.join("；")))); }
            return Err(AppError::io("发布证书文件失败，已恢复原文件", error));
        }
    }
    Ok(())
}

fn local_deploy(target: &DeployTarget, _domains: &[String], cert_pem: &str, key_pem: &str) -> Result<String> {
    validate_target(target)?;
    let cert = cfg(target, "certPath")?; let key = cfg(target, "keyPath")?;
    local_pair(cert, key, cert_pem, key_pem)?;
    let mut message = format!("已复制到本地（{cert} / {key}）");
    if let Some(script) = target.config.get("script").filter(|s| !s.trim().is_empty()) {
        let mut command = platform::command(if cfg!(windows) { "cmd" } else { "sh" });
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // 与计划任务一致：完整 shell 命令不能使用 C argv 的引号转义。
            command.args(["/D", "/S", "/C"]).raw_arg(format!("\"{script}\""));
        }
        #[cfg(not(windows))]
        command.args(["-c", script.as_str()]);
        let (ok, output) = crate::cfgeditor::run_validator_with_timeout(&mut command, script_timeout(target)?)
            .map_err(|e| AppError::new("DEPLOY_UNCERTAIN", "证书文件已写入，但部署脚本未正常完成；请核对服务状态后手动重试").with_detail(e.to_string()))?;
        if !ok { return Err(AppError::new("DEPLOY_UNCERTAIN", "证书文件已写入，但部署脚本返回失败；请核对服务状态后手动重试").with_detail(output.chars().take(1000).collect::<String>())); }
        message.push_str("，脚本已执行");
    }
    Ok(message)
}


/* ================= 腾讯云 SSL 证书入库（TC3-HMAC-SHA256 签名） ================= */

/// 腾讯云 TC3 签名（可复用于腾讯云其它产品）
pub fn tc3_signature(secret_key: &str, date: &str, service: &str, string_to_sign: &str) -> String {
    let k_date = hmac_sha256_bytes(format!("TC3{secret_key}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256_bytes(&k_date, service.as_bytes());
    let k_signing = hmac_sha256_bytes(&k_region, b"tc3_request");
    hex::encode(hmac_sha256_bytes(
        &k_signing,
        string_to_sign.as_bytes(),
    ))
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
        "CertificateType": "SVR",
        "Repeatable": false,
        "Alias": name,
    })
    .to_string();

    // 1. 规范请求串（POST / 空查询 + content-type/host/x-tc-action 头）
    let canonical_request = format!(
        "POST\n/\n\ncontent-type:application/json; charset=utf-8\nhost:{host}\nx-tc-action:{}\n\ncontent-type;host;x-tc-action\n{}",
        action.to_ascii_lowercase(),
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

    let resp = cloud_http()
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
    let body = deployment_json(resp, "腾讯云")?;
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
    let cert_id = upload_id(&body["Response"]["CertificateId"], "腾讯云")?;
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

    use russh_sftp::protocol::{Attrs, FileAttributes, Handle, OpenFlags, Status, StatusCode, Version};
    use std::collections::HashMap;
    use std::sync::Arc;

    #[derive(Default)]
    struct RemoteFixture {
        files: HashMap<String, (Vec<u8>, FileAttributes)>,
        auth: Vec<String>,
        subsystem: bool,
        fail_key_publish: bool,
        deny_metadata: bool,
    }

    fn fixture_key(seed: u8) -> russh::keys::PrivateKey {
        russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[seed; 32]).into()
    }

    fn sftp_ok(id: u32) -> Status {
        Status { id, status_code: StatusCode::Ok, error_message: String::new(), language_tag: String::new() }
    }

    struct MemorySftp(Arc<parking_lot::Mutex<RemoteFixture>>);
    impl russh_sftp::server::Handler for MemorySftp {
        type Error = StatusCode;
        fn unimplemented(&self) -> StatusCode { StatusCode::OpUnsupported }
        async fn init(&mut self, _: u32, _: HashMap<String, String>) -> std::result::Result<Version, StatusCode> { Ok(Version::new()) }
        async fn lstat(&mut self, id: u32, path: String) -> std::result::Result<Attrs, StatusCode> {
            let state = self.0.lock();
            if state.deny_metadata { return Err(StatusCode::PermissionDenied); }
            Ok(Attrs { id, attrs: state.files.get(&path).ok_or(StatusCode::NoSuchFile)?.1.clone() })
        }
        async fn mkdir(&mut self, id: u32, path: String, mut attrs: FileAttributes) -> std::result::Result<Status, StatusCode> {
            attrs.set_dir(true); self.0.lock().files.insert(path, (Vec::new(), attrs)); Ok(sftp_ok(id))
        }
        async fn open(&mut self, id: u32, path: String, flags: OpenFlags, mut attrs: FileAttributes) -> std::result::Result<Handle, StatusCode> {
            let mut state = self.0.lock();
            if state.files.contains_key(&path) && flags.contains(OpenFlags::EXCLUDE) { return Err(StatusCode::Failure); }
            attrs.set_regular(true); state.files.insert(path.clone(), (Vec::new(), attrs)); Ok(Handle { id, handle: path })
        }
        async fn write(&mut self, id: u32, handle: String, offset: u64, data: Vec<u8>) -> std::result::Result<Status, StatusCode> {
            let mut state = self.0.lock(); let file = &mut state.files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?.0;
            assert!(offset + data.len() as u64 <= 1024 * 1024);
            file.resize(file.len().max(offset as usize + data.len()), 0);
            file[offset as usize..offset as usize + data.len()].copy_from_slice(&data); Ok(sftp_ok(id))
        }
        async fn close(&mut self, id: u32, _: String) -> std::result::Result<Status, StatusCode> { Ok(sftp_ok(id)) }
        async fn rename(&mut self, id: u32, old: String, new: String) -> std::result::Result<Status, StatusCode> {
            let mut state = self.0.lock();
            if state.fail_key_publish && old.ends_with(".new") && new.ends_with("key.pem") { return Err(StatusCode::PermissionDenied); }
            if state.files.contains_key(&new) { return Err(StatusCode::Failure); }
            let file = state.files.remove(&old).ok_or(StatusCode::NoSuchFile)?; state.files.insert(new, file); Ok(sftp_ok(id))
        }
        async fn remove(&mut self, id: u32, path: String) -> std::result::Result<Status, StatusCode> {
            self.0.lock().files.remove(&path).ok_or(StatusCode::NoSuchFile)?; Ok(sftp_ok(id))
        }
    }

    struct FixtureSsh {
        state: Arc<parking_lot::Mutex<RemoteFixture>>,
        channels: HashMap<russh::ChannelId, russh::Channel<russh::server::Msg>>,
    }
    impl russh::server::Handler for FixtureSsh {
        type Error = russh::Error;
        async fn auth_password(&mut self, user: &str, password: &str) -> std::result::Result<russh::server::Auth, Self::Error> {
            self.state.lock().auth.push("password".into());
            Ok(if user == "fixture" && password == "fixture-password" { russh::server::Auth::Accept } else { russh::server::Auth::reject() })
        }
        async fn auth_publickey(&mut self, user: &str, public: &russh::keys::PublicKey) -> std::result::Result<russh::server::Auth, Self::Error> {
            self.state.lock().auth.push("key".into());
            Ok(if user == "fixture" && public == fixture_key(2).public_key() { russh::server::Auth::Accept } else { russh::server::Auth::reject() })
        }
        async fn channel_open_session(&mut self, channel: russh::Channel<russh::server::Msg>, reply: russh::server::ChannelOpenHandle,
            _: &mut russh::server::Session) -> std::result::Result<(), Self::Error> {
            self.channels.insert(channel.id(), channel); reply.accept().await; Ok(())
        }
        async fn subsystem_request(&mut self, id: russh::ChannelId, name: &str, session: &mut russh::server::Session) -> std::result::Result<(), Self::Error> {
            assert_eq!(name, "sftp"); self.state.lock().subsystem = true; session.channel_success(id)?;
            let channel = self.channels.remove(&id).unwrap(); let state = self.state.clone();
            tokio::spawn(async move { russh_sftp::server::run(channel.into_stream(), MemorySftp(state)).await; }); Ok(())
        }
        async fn exec_request(&mut self, id: russh::ChannelId, data: &[u8], session: &mut russh::server::Session) -> std::result::Result<(), Self::Error> {
            session.channel_success(id)?;
            match data {
                b"timeout" => return Ok(()),
                b"overflow" => session.extended_data(id, 1, vec![b'x'; 65537])?,
                b"fail" => session.extended_data(id, 1, b"fixture error".to_vec())?,
                _ => session.data(id, b"fixture output".to_vec())?,
            }
            session.eof(id)?;
            if data != b"missing-exit" { session.exit_status_request(id, if data == b"fail" { 7 } else { 0 })?; }
            session.close(id)?; Ok(())
        }
    }

    // 每个夹具只接收一次回环连接，有限时运行，不操作远程机器或本地服务文件。
    fn ssh_fixture() -> (DeployTarget, Arc<parking_lot::Mutex<RemoteFixture>>, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port(); listener.set_nonblocking(true).unwrap();
        let state = Arc::new(parking_lot::Mutex::new(RemoteFixture::default())); let shared = state.clone();
        let key = fixture_key(1); let fingerprint = key.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string();
        let worker = std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let (stream, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept()).await.unwrap().unwrap();
                let config = russh::server::Config { keys: vec![key], auth_rejection_time: Duration::ZERO,
                    auth_rejection_time_initial: Some(Duration::ZERO), ..Default::default() };
                let result = tokio::time::timeout(Duration::from_secs(15), async {
                    if let Ok(session) = russh::server::run_stream(Arc::new(config), stream,
                        FixtureSsh { state: shared, channels: HashMap::new() }).await { let _ = session.await; }
                }).await;
                assert!(result.is_ok(), "SSH fixture session did not close");
            });
        });
        let target = DeployTarget { id: "fixture".into(), name: "fixture".into(), kind: "ssh".into(), last_result: None,
            config: [("host", "127.0.0.1".into()), ("port", port.to_string()), ("user", "fixture".into()),
                ("password", "fixture-password".into()), ("hostFingerprint", fingerprint), ("certPath", "/ssl/chain.pem".into()),
                ("keyPath", "/ssl/key.pem".into()), ("script", "success".into()), ("timeoutSec", "1".into())]
                .map(|(k, v)| (k.into(), v)).into() };
        (target, state, worker)
    }

    #[test]
    fn ssh_probe_and_changed_fingerprint_never_send_credentials() {
        let (target, state, worker) = ssh_fixture();
        let found = probe_ssh("127.0.0.1", ssh_port(&target).unwrap()).unwrap(); worker.join().unwrap();
        assert_eq!(found.fingerprint, target.config["hostFingerprint"]); assert!(state.lock().auth.is_empty());
        let (mut target, state, worker) = ssh_fixture();
        target.config.insert("hostFingerprint".into(), fixture_key(3).public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string());
        assert_eq!(ssh_deploy(&target, &[], "cert", "key").unwrap_err().code, "SSH_HOST_KEY_CHANGED");
        worker.join().unwrap(); assert!(state.lock().auth.is_empty()); assert!(state.lock().files.is_empty());
    }

    #[test]
    fn ssh_password_and_key_auth_publish_pair_and_wait_for_exit_after_eof() {
        let dir = tempfile::tempdir().unwrap(); let identity = dir.path().join("identity");
        std::fs::write(&identity, fixture_key(2).to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap().as_bytes()).unwrap();
        for auth in ["password", "key"] {
            let (mut target, state, worker) = ssh_fixture();
            target.config.insert("auth".into(), auth.into()); target.config.insert("identityFile".into(), identity.to_string_lossy().into_owned());
            let result = ssh_deploy(&target, &[], "new-cert", "new-key"); worker.join().unwrap(); assert!(result.is_ok(), "{result:?}");
            let state = state.lock(); assert!(state.subsystem); assert_eq!(state.auth, vec![auth]);
            assert_eq!(state.files["/ssl/chain.pem"].0, b"new-cert"); assert_eq!(state.files["/ssl/key.pem"].0, b"new-key");
            assert_eq!(state.files["/ssl/key.pem"].1.permissions.unwrap() & 0o777, 0o600);
            assert_eq!(state.files.len(), 3); assert!(std::fs::read_to_string(&identity).unwrap().contains("OPENSSH PRIVATE KEY"));
        }
    }

    #[test]
    fn ssh_script_failure_missing_status_timeout_and_overflow_are_uncertain() {
        for script in ["fail", "missing-exit", "timeout", "overflow"] {
            let (mut target, state, worker) = ssh_fixture(); target.config.insert("script".into(), script.into());
            let result = ssh_deploy(&target, &[], "cert", "key"); worker.join().unwrap();
            assert_eq!(result.unwrap_err().code, "DEPLOY_UNCERTAIN", "{script}");
            assert_eq!(state.lock().files["/ssl/key.pem"].0, b"key");
        }
    }

    #[test]
    fn ssh_failed_second_publish_restores_pair_and_metadata_errors_stop_writes() {
        let (target, state, worker) = ssh_fixture();
        let mut attrs = FileAttributes { permissions: Some(0o640), ..Default::default() }; attrs.set_regular(true);
        { let mut state = state.lock(); state.fail_key_publish = true;
            state.files.insert("/ssl/chain.pem".into(), (b"old-cert".to_vec(), attrs.clone()));
            state.files.insert("/ssl/key.pem".into(), (b"old-key".to_vec(), attrs)); }
        let error = ssh_deploy(&target, &[], "new-cert", "new-key").unwrap_err(); worker.join().unwrap();
        assert_eq!(error.code, "DEPLOY_UNCERTAIN"); let saved = state.lock();
        assert_eq!(saved.files["/ssl/chain.pem"].0, b"old-cert"); assert_eq!(saved.files["/ssl/key.pem"].0, b"old-key");
        assert_eq!(saved.files.len(), 3); drop(saved);
        for denied in [true, false] {
            let (target, state, worker) = ssh_fixture();
            if denied { state.lock().deny_metadata = true; } else {
                // 软链接目录不能当作普通目录跟随。
                state.lock().files.insert("/ssl".into(), (Vec::new(), FileAttributes { permissions: Some(0o120777), ..Default::default() }));
            }
            assert!(ssh_deploy(&target, &[], "cert", "key").is_err()); worker.join().unwrap();
            assert!(!state.lock().files.contains_key("/ssl/chain.pem"));
        }
    }

    #[test]
    fn local_pair_restores_original_files_when_second_publish_fails() {
        let dir = tempfile::tempdir().unwrap(); let cert = dir.path().join("chain.pem"); let key = dir.path().join("key.pem");
        for previous in [true, false] {
            if previous { std::fs::write(&cert, "old-cert").unwrap(); std::fs::write(&key, "old-key").unwrap(); }
            let mut index = 0;
            let result = local_pair_with_publish(cert.to_str().unwrap(), key.to_str().unwrap(), "new-cert", "new-key", |file, path| {
                index += 1;
                if index == 2 { return Err(std::io::Error::other("fixture publish failure")); }
                file.persist(path).map(|_| ()).map_err(|e| e.error)
            });
            assert!(result.is_err());
            if previous {
                assert_eq!(std::fs::read_to_string(&cert).unwrap(), "old-cert"); assert_eq!(std::fs::read_to_string(&key).unwrap(), "old-key");
                std::fs::remove_file(&cert).unwrap(); std::fs::remove_file(&key).unwrap();
            }
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn local_paths_and_script_exit_are_checked() {
        let dir = tempfile::tempdir().unwrap(); let cert = dir.path().join("chain.pem"); let key = dir.path().join("key.pem");
        let mut target = DeployTarget { id: "local".into(), name: "local".into(), kind: "local".into(), last_result: None,
            config: [("certPath".into(), cert.to_string_lossy().into_owned()), ("keyPath".into(), key.to_string_lossy().into_owned())].into() };
        for (script, success) in [("exit 0", true), ("exit 7", false)] {
            target.config.insert("script".into(), script.into()); let result = local_deploy(&target, &[], "new-cert", "new-key");
            if success { assert!(result.is_ok(), "{result:?}"); } else { assert_eq!(result.unwrap_err().code, "DEPLOY_UNCERTAIN"); }
        }
        let scripts = if cfg!(windows) {
            [r#"powershell.exe -NoProfile -NonInteractive -Command "Start-Sleep -Seconds 10""#,
             r#"powershell.exe -NoProfile -NonInteractive -Command "[Console]::Out.Write(('x' * 70000))""#]
        } else { ["sleep 10", "head -c 70000 /dev/zero"] };
        for (index, script) in scripts.into_iter().enumerate() {
            target.config.insert("timeoutSec".into(), if index == 0 { "1" } else { "5" }.into());
            target.config.insert("script".into(), script.into()); let started = std::time::Instant::now();
            let error = local_deploy(&target, &[], "new-cert", "new-key").unwrap_err();
            assert_eq!(error.code, "DEPLOY_UNCERTAIN", "{script}");
            assert!(error.detail.as_deref().is_some_and(|d| d.contains(if index == 0 { "超时" } else { "64 KiB" }) || (index == 1 && d.contains("输出超过上限"))), "{error:?}");
            assert!(started.elapsed() < Duration::from_secs(5));
        }
        let marker = dir.path().join("quoted file.txt");
        target.config.insert("script".into(), if cfg!(windows) { format!("echo quoted-value > \"{}\"", marker.display()) }
            else { format!("printf quoted-value > '{}'", marker.display()) });
        local_deploy(&target, &[], "new-cert", "new-key").unwrap();
        assert_eq!(std::fs::read_to_string(marker).unwrap().trim(), "quoted-value");
        assert_eq!(std::fs::read_to_string(key).unwrap(), "new-key");
        assert!(local_pair(cert.to_str().unwrap(), cert.to_str().unwrap(), "c", "k").is_err());
        assert!(local_path("relative.pem").is_err()); assert!(local_path(dir.path().join("../escape.pem").to_str().unwrap()).is_err());
        if cfg!(windows) { assert!(local_path(dir.path().join("key.pem:secret").to_str().unwrap()).is_err()); }
    }

    fn panel_fixture(replies: Vec<(u16, &'static str)>) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for (status, body) in replies {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline, "fixture request timed out");
                            std::thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                // Windows accepted sockets can inherit nonblocking mode from the listener.
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut request = Vec::new();
                loop {
                    let mut chunk = [0u8; 4096]; let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0); request.extend_from_slice(&chunk[..n]); assert!(request.len() < 128 * 1024);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let len = headers.lines().find_map(|line| line.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap_or(0);
                        if request.len() >= end + 4 + len { break; }
                    }
                }
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        (url, worker)
    }

    #[test]
    fn notification_http_and_business_errors_are_not_success_or_secret_leaks() {
        for (kind, status, body, ok) in [
            ("generic", 204, "", true), ("generic", 500, "private-fixture", false), ("generic", 302, "", false),
            ("dingtalk", 200, r#"{"errcode":0}"#, true), ("wecom", 200, r#"{"errcode":0}"#, true),
            ("feishu", 200, r#"{"code":0}"#, true), ("feishu", 200, r#"{"StatusCode":0}"#, true),
            ("dingtalk", 200, r#"{"errcode":310000,"errmsg":"private-fixture"}"#, false),
            ("wecom", 200, "{}", false), ("feishu", 200, "<html>login</html>", false),
        ] {
            let (url, worker) = panel_fixture(vec![(status, body)]);
            let result = notify(kind, &format!("{url}/?token=private-fixture"), false, "fixture", "certificate expired");
            worker.join().unwrap(); assert_eq!(result.is_ok(), ok, "{kind} {body}");
            if let Err(error) = result { assert!(!error.to_string().contains("private-fixture")); }
        }
    }

    #[test]
    fn panel_http_and_business_failures_never_report_success() {
        for (status, body, ok) in [(500, r#"{"status":true}"#, false), (200, "<html>login</html>", false),
            (200, "null", false), (200, "{}", false), (200, r#"{"status":false,"msg":"denied"}"#, false),
            (200, r#"{"status":true}"#, true)] {
            let (url, worker) = panel_fixture(vec![(200, "{}"), (status, body)]);
            let target = DeployTarget { id: "fixture".into(), name: "fixture".into(), kind: "btpanel".into(), last_result: None,
                config: [("url".into(), url), ("apiSk".into(), "fixture".into()), ("siteName".into(), "example.invalid".into())].into() };
            assert_eq!(bt_deploy(&target, "fixture-cert", "fixture-key").is_ok(), ok, "{status}: {body}");
            worker.join().unwrap();
        }
        for (status, body, ok) in [(503, r#"{"code":200}"#, false), (200, "<html>login</html>", false),
            (200, "{}", false), (200, r#"{"code":401,"message":"denied"}"#, false), (200, r#"{"code":200}"#, true)] {
            let (url, worker) = panel_fixture(vec![(status, body)]);
            let target = DeployTarget { id: "fixture".into(), name: "fixture".into(), kind: "onepanel".into(), last_result: None,
                config: [("url".into(), url), ("token".into(), "fixture".into())].into() };
            assert_eq!(onepanel_deploy(&target, &["example.invalid".into()], "fixture-cert", "fixture-key").is_ok(), ok);
            worker.join().unwrap();
        }
    }

    #[test]
    fn upload_requires_a_positive_certificate_identifier() {
        for value in [serde_json::Value::Null, serde_json::json!(""), serde_json::json!("  "), serde_json::json!(0), serde_json::json!(-1), serde_json::json!({})] {
            assert!(upload_id(&value, "fixture").is_err());
        }
        assert_eq!(upload_id(&serde_json::json!(123), "fixture").unwrap(), "123");
        assert_eq!(upload_id(&serde_json::json!("cert-id"), "fixture").unwrap(), "cert-id");
    }

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
    fn tc3_signature_matches_independent_binary_hmac_vector() {
        // 按官方 TC3 二进制派生规则，用 Node crypto 独立计算，避免测试重复实现同一错误。
        assert_eq!(tc3_signature("sk", "2024-01-01", "ssl", "sts"),
            "2cb6b1dc0394e022fc7feec630ca4be3b81119d14bcf234f1ad286d5d1aa0f69");
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
