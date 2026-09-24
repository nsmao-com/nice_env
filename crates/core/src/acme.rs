//! ACME 客户端（RFC 8555）：申请 / 续签 TLS 证书，DNS-01 验证。
//!
//! 参考 certd 的玩法但做成桌面端内置：域名解析权在 DNS 服务商手里（阿里云 /
//! Cloudflare / DNSPod），所以走 DNS-01 —— 无需公网可达本机，天然支持多域名与通配符。
//!
//! 实现取舍：
//! - 只做 ES256（P-256）JWS：ACME 通用支持，签名最短、无证书链包袱；
//! - 账号密钥 PKCS8 PEM 持久化在 certs/acme/ 下，同一自动化复用同一账号；
//! - 全程 reqwest blocking：签发本来就串行，包一层线程即可，不引 tokio 复杂度。

use crate::error::{AppError, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use base64::Engine as _;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::point::AffineCoordinates;
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use sha2::{Digest, Sha256};
use std::time::Duration;

/// 支持的 CA → ACME directory
pub fn directory_url(ca: &str) -> &'static str {
    match ca {
        "letsencrypt-staging" => "https://acme-staging-v02.api.letsencrypt.org/directory",
        "zerossl" => "https://acme.zerossl.com/v2/DV90",
        "google" => "https://dv.acme-v02.api.pki.goog/directory",
        "buypass" => "https://api.buypass.com/acme/directory",
        // 缺省按 Let's Encrypt 生产
        _ => "https://acme-v02.api.letsencrypt.org/directory",
    }
}

/* ================= base64url / 编码工具 ================= */

pub fn b64url(data: &[u8]) -> String {
    B64URL.encode(data)
}

/// RFC 7638 JWK 指纹：P-256 公钥的规范 JSON 的 SHA-256
pub fn jwk_thumbprint(x: &[u8], y: &[u8]) -> String {
    let canonical = format!(
        r#"{{"crv":"P-256","kty":"EC","x":"{}","y":"{}"}}"#,
        b64url(x),
        b64url(y)
    );
    b64url(&Sha256::digest(canonical.as_bytes()))
}

/// DNS-01 记录值：base64url(sha256(keyAuthorization))
/// keyAuthorization = challenge.token + "." + jwk_thumbprint
pub fn dns01_txt_value(token: &str, thumbprint: &str) -> String {
    let key_authorization = format!("{token}.{thumbprint}");
    b64url(&Sha256::digest(key_authorization.as_bytes()))
}

/* ================= 账号密钥 ================= */

pub struct AccountKey {
    signing: SigningKey,
    /// 公钥 X / Y 坐标（各 32 字节）
    x: Vec<u8>,
    y: Vec<u8>,
    thumbprint: String,
}

impl AccountKey {
    /// 从 PKCS8 PEM 加载（已有的 ACME 账号）
    pub fn from_pem(pem: &str) -> Result<Self> {
        let signing = SigningKey::from_pkcs8_pem(pem)
            .map_err(|e| AppError::internal("解析 ACME 账号密钥", e.to_string()))?;
        Self::build(signing)
    }

    /// 生成新账号并导出 PEM（调用方负责持久化）。
    /// rand 0.8 的系统熵源出 32 字节标量（p256 0.14 未启用自带 OsRng 特性）；
    /// 非法标量概率约 2^-32，重试几次是纯理论保险。
    pub fn generate() -> Result<(Self, String)> {
        for _ in 0..8 {
            let bytes = rand::random::<[u8; 32]>();
            if let Ok(sk) = SigningKey::from_slice(&bytes) {
                let pem = sk
                    .to_pkcs8_pem(p256::pkcs8::LineEnding::LF)
                    .map_err(|e| AppError::internal("导出 ACME 账号密钥", e.to_string()))?
                    .to_string();
                return Ok((Self::build(sk)?, pem));
            }
        }
        Err(AppError::internal("生成 ACME 账号密钥", "随机标量反复非法"))
    }

    fn build(signing: SigningKey) -> Result<Self> {
        // 公钥坐标直接取自仿射点（JWK 的 x / y 各 32 字节）
        let affine = signing.verifying_key().as_affine();
        let (x, y) = (affine.x().to_vec(), affine.y().to_vec());
        let thumbprint = jwk_thumbprint(&x, &y);
        Ok(Self {
            signing,
            x,
            y,
            thumbprint,
        })
    }

    pub fn thumbprint(&self) -> &str {
        &self.thumbprint
    }

    fn jwk_json(&self) -> serde_json::Value {
        serde_json::json!({
            "kty": "EC", "crv": "P-256",
            "x": b64url(&self.x), "y": b64url(&self.y),
        })
    }

    fn sign(&self, data: &[u8]) -> String {
        let sig: Signature = self.signing.sign(data);
        // JOSE 要求 raw r||s（64 字节），不是 DER
        b64url(&sig.to_bytes())
    }
}

/* ================= ACME 客户端 ================= */

pub struct AcmeClient {
    http: reqwest::blocking::Client,
    dir: serde_json::Value,
    account: AccountKey,
    kid: String,
    nonce: Option<String>,
    /// newOrder 响应的 Location（order 资源 URL），finalize 后轮询用
    pending_order_url: Option<String>,
}

impl AcmeClient {
    /// 建客户端：拉 directory；有账号 PEM 就复用，否则新建（返回新 PEM 供调用方保存）。
    /// `eab = (kid, hmac_key)`：ZeroSSL / Google Trust Services / BuyPass 需要
    /// 外部账号绑定（RFC 8555 §7.3.4）。
    pub fn connect(
        ca: &str,
        account_pem: Option<&str>,
        email: &str,
        eab: Option<(&str, &str)>,
    ) -> Result<(Self, Option<String>)> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("NiceEnv/0.1 (+https://github.com)")
            .build()
            .map_err(|e| AppError::internal("构建 HTTP 客户端", e.to_string()))?;

        let dir_resp = http
            .get(directory_url(ca))
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|e| {
                AppError::new("ACME_DIRECTORY", format!("拉取 ACME directory 失败：{e}"))
                    .with_hint("检查网络能否访问证书颁发机构；国内可先在代理页开启系统代理")
            })?;
        let dir: serde_json::Value = dir_resp.json().map_err(|e| {
            AppError::new("ACME_DIRECTORY", format!("directory 不是合法 JSON：{e}"))
        })?;

        let (account, fresh_pem) = match account_pem {
            Some(p) if !p.trim().is_empty() => (AccountKey::from_pem(p)?, None),
            _ => {
                let (k, pem) = AccountKey::generate()?;
                (k, Some(pem))
            }
        };

        let mut client = Self {
            http,
            dir,
            account,
            kid: String::new(),
            nonce: None,
            pending_order_url: None,
        };

        // 注册账号（存在则幂等返回既有 kid）
        let account_url = client
            .dir
            .get("newAccount")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::new("ACME_DIRECTORY", "directory 缺少 newAccount"))?
            .to_string();
        let mut payload = serde_json::json!({
            "termsOfServiceAgreed": true,
            "onlyReturnExisting": false,
        });
        // contact 是 CA 发到期/吊销提醒的通道；空邮箱就不带（ACME 允许）
        if !email.trim().is_empty() {
            payload["contact"] = serde_json::json!([format!("mailto:{}", email.trim())]);
        }
        if let Some((kid, hmac_key)) = eab {
            payload["externalAccountBinding"] =
                eab_jws(kid, hmac_key, &account_url, &client.account)?;
        }
        let resp = client.jws_post_new(&account_url, &payload)?;
        let kid = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::new("ACME_ACCOUNT", "newAccount 未返回 Location (kid)"))?
            .to_string();
        client.kid = kid;
        Ok((client, fresh_pem))
    }

    fn fetch_nonce(&mut self) -> Result<String> {
        if let Some(n) = self.nonce.take() {
            return Ok(n);
        }
        let url = self
            .dir
            .get("newNonce")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::new("ACME_DIRECTORY", "directory 缺少 newNonce"))?
            .to_string();
        let resp = self
            .http
            .head(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|e| AppError::new("ACME_NONCE", format!("获取 nonce 失败：{e}")))?;
        resp.headers()
            .get("replay-nonce")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .ok_or_else(|| AppError::new("ACME_NONCE", "响应缺少 Replay-Nonce"))
    }

    /// 带 JWS 的 POST（用既有 kid）；坏 nonce 自动换一个重试（ACME 高频坑）
    fn jws_post(
        &mut self,
        url: &str,
        payload: &serde_json::Value,
    ) -> Result<reqwest::blocking::Response> {
        let mut last = None;
        for _ in 0..3 {
            let resp = self.jws_post_once(url, payload, false)?;
            let status = resp.status();
            if status.is_success() || status.as_u16() != 400 {
                return Ok(resp);
            }
            let body = resp.text().unwrap_or_default();
            if body.contains("badNonce") {
                self.nonce = None;
                last = Some(AppError::new("ACME_BAD_NONCE", body));
                continue;
            }
            return Err(acme_error(&body, url));
        }
        Err(last.unwrap_or_else(|| AppError::new("ACME_RETRY", "重试次数用尽")))
    }

    /// newAccount 专用：protected 里带 jwk 而不是 kid
    fn jws_post_new(
        &mut self,
        url: &str,
        payload: &serde_json::Value,
    ) -> Result<reqwest::blocking::Response> {
        self.jws_post_once(url, payload, true)
    }

    fn jws_post_once(
        &mut self,
        url: &str,
        payload: &serde_json::Value,
        new_account: bool,
    ) -> Result<reqwest::blocking::Response> {
        let nonce = self.fetch_nonce()?;
        let mut protected = serde_json::json!({
            "alg": "ES256",
            "nonce": nonce,
            "url": url,
        });
        if new_account {
            protected["jwk"] = self.account.jwk_json();
        } else {
            protected["kid"] = serde_json::Value::String(self.kid.clone());
        }
        // POST-as-GET：payload 为空字符串
        let payload_str = if payload.is_null() {
            String::new()
        } else {
            serde_json::to_string(payload)
                .map_err(|e| AppError::internal("序列化 payload", e.to_string()))?
        };
        let body = serde_json::json!({
            "protected": b64url(protected.to_string().as_bytes()),
            "payload": b64url(payload_str.as_bytes()),
            "signature": self.account.sign(
                format!("{}.{}",
                    b64url(protected.to_string().as_bytes()),
                    b64url(payload_str.as_bytes())
                ).as_bytes()
            ),
        });
        self.http
            .post(url)
            .json(&body)
            .send()
            .map_err(|e| AppError::new("ACME_HTTP", format!("请求 {url} 失败：{e}")))
    }

    /// POST-as-GET（幂等拉取），返回文本
    fn post_as_get(&mut self, url: &str) -> Result<String> {
        let resp = self.jws_post(url, &serde_json::Value::Null)?;
        ensure_ok(resp, url)
    }

    /// 带触点签发流程：下单 → DNS-01 验证全部域名 → finalize → 拿证书链
    ///
    /// `set_txt(domain, prefix, value) -> (zone, record_id)` 与
    /// `clear_txt(zone, name, record_id)` 由调用方注入（DNS 服务商差异隔离在 dnsprov）。
    /// 返回（证书链 PEM、证书私钥 PEM、leaf notBefore/notAfter 毫秒）。
    pub fn issue(
        &mut self,
        domains: &[String],
        key_alg: &str,
        dns_wait_sec: i64,
        set_txt: &mut dyn FnMut(&str, &str, &str) -> Result<(String, String)>,
        clear_txt: &mut dyn FnMut(&str, &str, &str) -> Result<()>,
    ) -> Result<(String, String, i64, i64)> {
        if domains.is_empty() {
            return Err(AppError::new("BAD_DOMAINS", "至少需要一个域名"));
        }
        let new_order_url = dir_str(&self.dir, "newOrder")?;
        let identifiers: Vec<serde_json::Value> = domains
            .iter()
            .map(|d| serde_json::json!({"type": "dns", "value": d}))
            .collect();
        let resp = self.jws_post(
            &new_order_url,
            &serde_json::json!({ "identifiers": identifiers }),
        )?;
        // order 资源 URL 在 Location 头里，finalize 后轮询要用
        self.pending_order_url = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let order: serde_json::Value = serde_json::from_str(&ensure_ok(resp, "newOrder")?)
            .map_err(|e| AppError::new("ACME_ORDER", format!("order 响应解析失败：{e}")))?;

        // 逐条 authorization 完成 DNS-01
        let authz_urls: Vec<String> = order["authorizations"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if authz_urls.is_empty() {
            return Err(AppError::new("ACME_ORDER", "order 未返回 authorizations"));
        }

        struct PendingTxt {
            zone: String,
            name: String,
            record_id: String,
        }
        let mut pendings: Vec<PendingTxt> = Vec::new();
        let result = (|| -> Result<()> {
            for url in &authz_urls {
                let authz: serde_json::Value = serde_json::from_str(&self.post_as_get(url)?)
                    .map_err(|e| {
                        AppError::new("ACME_AUTHZ", format!("authorization 解析失败：{e}"))
                    })?;
                let domain = authz["identifier"]["value"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let challenge = authz["challenges"]
                    .as_array()
                    .and_then(|cs| cs.iter().find(|c| c["type"] == "dns-01"))
                    .ok_or_else(|| {
                        AppError::new("ACME_CHALLENGE", format!("{domain} 未提供 dns-01 验证方式"))
                    })?;
                let token = challenge["token"].as_str().unwrap_or_default().to_string();
                let chall_url = challenge["url"].as_str().unwrap_or_default().to_string();
                let txt = dns01_txt_value(&token, self.account.thumbprint());

                // 记录名：_acme-challenge.{去掉 zone 后的主机部分}；zone 由 DNS 侧解析
                let (zone, record_id) = set_txt(&domain, "_acme-challenge", &txt)?;
                // DNS 同步慢的服务商：给用户可配的生效等待（certd 的「DNS 生效等待」）
                if dns_wait_sec > 0 {
                    std::thread::sleep(Duration::from_secs(dns_wait_sec.min(600) as u64));
                }
                pendings.push(PendingTxt {
                    zone: zone.clone(),
                    name: format!("_acme-challenge.{domain}"),
                    record_id,
                });

                // 触发验证
                let resp = self.jws_post(&chall_url, &serde_json::json!({}))?;
                ensure_ok(resp, "challenge")?;
                // 等 DNS 生效 + CA 核验
                self.wait_authorization(url, &domain)?;
            }
            Ok(())
        })();

        // 无论成败都清理 TXT（失败重试不留脏记录）
        for p in &pendings {
            let _ = clear_txt(&p.zone, &p.name, &p.record_id);
        }
        result?;

        // finalize：CSR（证书私钥算法可配，密钥随证书一起落盘）
        let (key_pair, _) = generate_cert_key(key_alg)?;
        let mut params = rcgen::CertificateParams::new(domains.to_vec())
            .map_err(|e| AppError::internal("证书参数", e.to_string()))?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, domains[0].as_str());
        let csr = params
            .serialize_request(&key_pair)
            .map_err(|e| AppError::internal("生成 CSR", e.to_string()))?;
        let finalize_url = order["finalize"]
            .as_str()
            .ok_or_else(|| AppError::new("ACME_ORDER", "order 缺少 finalize"))?
            .to_string();
        let resp = self.jws_post(
            &finalize_url,
            &serde_json::json!({ "csr": b64url(csr.der().as_ref()) }),
        )?;
        ensure_ok(resp, "finalize")?;

        // 轮询 order 直到 valid
        let order_url = self.kid_order_url()?;
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        let cert_url = loop {
            let body = self.post_as_get(&order_url)?;
            let ord: serde_json::Value = serde_json::from_str(&body)
                .map_err(|e| AppError::new("ACME_ORDER", format!("order 轮询解析失败：{e}")))?;
            match ord["status"].as_str() {
                Some("valid") => {
                    break ord["certificate"]
                        .as_str()
                        .ok_or_else(|| {
                            AppError::new("ACME_ORDER", "order valid 但缺少 certificate")
                        })?
                        .to_string()
                }
                Some("invalid") => {
                    return Err(AppError::new(
                        "ACME_INVALID",
                        "证书订单被 CA 判定失败（常见：DNS 未生效 / 域名被 CA 限流）",
                    ))
                }
                _ => {}
            }
            if std::time::Instant::now() > deadline {
                return Err(AppError::new("ACME_TIMEOUT", "等待 CA 签发超时（2 分钟）"));
            }
            std::thread::sleep(Duration::from_secs(3));
        };

        // 下载证书链（PEM）
        let chain = self.post_as_get(&cert_url)?;
        if !chain.contains("BEGIN CERTIFICATE") {
            return Err(AppError::new("ACME_CERT", "下载证书链内容异常"));
        }

        // 有效期从 leaf 解析（rcgen 参数解析即可，不需要完整 x509 库）
        let leaf = chain
            .split("-----END CERTIFICATE-----")
            .next()
            .map(|s| format!("{s}-----END CERTIFICATE-----"))
            .unwrap_or_else(|| chain.clone());
        let leaf_params = rcgen::CertificateParams::from_ca_cert_pem(&leaf)
            .map_err(|e| AppError::new("ACME_CERT", format!("解析 leaf 证书失败：{e}")))?;
        let not_before = to_ms(leaf_params.not_before);
        let not_after = to_ms(leaf_params.not_after);

        Ok((chain, key_pair.serialize_pem(), not_before, not_after))
    }

    /// order URL：newOrder 的 Location（保存在 client 里）；拿不到就明说，不瞎猜
    fn kid_order_url(&self) -> Result<String> {
        self.pending_order_url
            .clone()
            .ok_or_else(|| AppError::new("ACME_ORDER", "newOrder 未返回 order URL (Location)"))
    }

    fn wait_authorization(&mut self, authz_url: &str, domain: &str) -> Result<()> {
        let deadline = std::time::Instant::now() + Duration::from_secs(180);
        loop {
            let authz: serde_json::Value = serde_json::from_str(&self.post_as_get(authz_url)?)
                .map_err(|e| AppError::new("ACME_AUTHZ", format!("轮询解析失败：{e}")))?;
            match authz["status"].as_str() {
                Some("valid") => return Ok(()),
                Some("invalid") => {
                    // 把 challenge 的错误细节翻出来给用户
                    let detail = authz["challenges"]
                        .as_array()
                        .and_then(|cs| {
                            cs.iter().find_map(|c| {
                                c["error"].as_object().map(|e| {
                                    format!(
                                        "{}: {}",
                                        e.get("type").and_then(|v| v.as_str()).unwrap_or("error"),
                                        e.get("detail").and_then(|v| v.as_str()).unwrap_or("")
                                    )
                                })
                            })
                        })
                        .unwrap_or_else(|| "DNS 验证失败".into());
                    return Err(AppError::new("ACME_DNS_FAIL", format!("{domain} 验证失败：{detail}"))
                        .with_hint("确认 TXT 记录已生效（可用 dig/nslookup 查 _acme-challenge 子域），DNS 供应商凭据是否正确"));
                }
                _ => {}
            }
            if std::time::Instant::now() > deadline {
                return Err(AppError::new(
                    "ACME_TIMEOUT",
                    format!("{domain} DNS 验证超时（3 分钟）"),
                ));
            }
            std::thread::sleep(Duration::from_secs(4));
        }
    }
}

/// HMAC-SHA256（RFC 2104）：EAB 与华为云签名共用。
/// 仓库里 sha2 是 0.10 而 hmac 0.13 只认 digest 0.11 —— 为了对齐两个版本
/// 引两套同名依赖不值当，HMAC 本身十几行，配 RFC 固定向量测试更稳。
pub(crate) fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::digest([&ipad[..], data].concat());
    let outer = Sha256::digest([&opad[..], &inner].concat());
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer);
    out
}

/// EAB：用 CA 预共享的 HMAC 密钥给账号 JWK 再签一层 JWS（HS256）
fn eab_jws(
    kid: &str,
    hmac_key: &str,
    url: &str,
    account: &AccountKey,
) -> Result<serde_json::Value> {
    let key = B64URL
        .decode(hmac_key.trim())
        .map_err(|e| AppError::new("ACME_EAB", format!("EAB HMAC 密钥不是合法 base64url：{e}")))?;
    let payload = account.jwk_json().to_string();
    let protected = serde_json::json!({
        "alg": "HS256",
        "kid": kid,
        "url": url,
    });
    let signing_input = format!(
        "{}.{}",
        b64url(protected.to_string().as_bytes()),
        b64url(payload.as_bytes())
    );
    let sig = hmac_sha256(&key, signing_input.as_bytes());
    Ok(serde_json::json!({
        "protected": b64url(protected.to_string().as_bytes()),
        "payload": b64url(payload.as_bytes()),
        "signature": b64url(&sig),
    }))
}

/// 证书私钥生成：ECDSA P-256（默认）/ P-384，或 RSA 2048/3072/4096。
/// ring 不能生成 RSA 密钥，RSA 走 rsa crate 生成后以 PKCS8 交给 rcgen 签名。
pub fn generate_cert_key(alg: &str) -> Result<(rcgen::KeyPair, String)> {
    let kp = match alg {
        "ec384" => rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384)
            .map_err(|e| AppError::internal("生成 P-384 密钥", e.to_string()))?,
        "rsa2048" | "rsa3072" | "rsa4096" => {
            let bits = match alg {
                "rsa2048" => 2048,
                "rsa3072" => 3072,
                _ => 4096,
            };
            let mut rng = rand::thread_rng();
            let key = rsa::RsaPrivateKey::new(&mut rng, bits)
                .map_err(|e| AppError::internal("生成 RSA 密钥", e.to_string()))?;
            use rsa::pkcs8::EncodePrivateKey;
            let pem = key
                .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)
                .map_err(|e| AppError::internal("导出 RSA 密钥", e.to_string()))?
                .to_string();
            rcgen::KeyPair::from_pkcs8_pem_and_sign_algo(&pem, &rcgen::PKCS_RSA_SHA256)
                .map_err(|e| AppError::internal("导入 RSA 密钥", e.to_string()))?
        }
        _ => rcgen::KeyPair::generate()
            .map_err(|e| AppError::internal("生成 P-256 密钥", e.to_string()))?,
    };
    let pem = kp.serialize_pem();
    Ok((kp, pem))
}

fn dir_str(dir: &serde_json::Value, key: &str) -> Result<String> {
    dir.get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| AppError::new("ACME_DIRECTORY", format!("directory 缺少 {key}")))
}

fn ensure_ok(resp: reqwest::blocking::Response, what: &str) -> Result<String> {
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if status.is_success() {
        Ok(text)
    } else {
        Err(acme_error(&text, what))
    }
}

/// ACME 错误体是 JSON {type, detail}；尽量翻出 detail 给人话
fn acme_error(body: &str, what: &str) -> AppError {
    let parsed = serde_json::from_str::<serde_json::Value>(body);
    let (code, detail) = match parsed {
        Ok(v) => (
            v["type"]
                .as_str()
                .unwrap_or("ACME_ERROR")
                .rsplit('/')
                .next()
                .unwrap_or("ACME_ERROR")
                .to_string(),
            v["detail"].as_str().unwrap_or(body).to_string(),
        ),
        Err(_) => ("ACME_ERROR".to_string(), body.chars().take(300).collect()),
    };
    AppError::new(&code, format!("{what}：{detail}"))
}

fn to_ms(t: time::OffsetDateTime) -> i64 {
    (t.unix_timestamp_nanos() / 1_000_000) as i64
}

/* ================= 测试 ================= */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns01_value_is_sha256_of_key_authorization() {
        // RFC 8555 §8.4：TXT = base64url(sha256(token "." thumbprint))
        let v = dns01_txt_value("token-x", "thumb-y");
        let expect = b64url(&Sha256::digest(b"token-x.thumb-y"));
        assert_eq!(v, expect);
        // 86 字符 = sha256(32B) 的 base64url
        assert_eq!(v.len(), 43);
    }

    #[test]
    fn account_key_roundtrip_and_thumbprint() {
        let (k, pem) = AccountKey::generate().unwrap();
        let k2 = AccountKey::from_pem(&pem).unwrap();
        assert_eq!(k.thumbprint(), k2.thumbprint());
        assert_eq!(k.thumbprint().len(), 43);
        // JWK x/y 与 PEM 复原一致
        assert_eq!(k.x, k2.x);
        assert_eq!(k.y, k2.y);
    }

    #[test]
    fn signature_is_64_byte_raw() {
        let (k, _) = AccountKey::generate().unwrap();
        let sig = k.sign(b"payload");
        // base64url(64B) 长度
        assert_eq!(B64URL.decode(&sig).unwrap().len(), 64);
    }

    #[test]
    fn directory_urls_known() {
        assert!(directory_url("letsencrypt").contains("acme-v02"));
        assert!(directory_url("letsencrypt-staging").contains("staging"));
        assert!(directory_url("zerossl").contains("zerossl"));
        assert!(directory_url("google").contains("pki.goog"));
        assert!(directory_url("buypass").contains("buypass"));
        assert!(directory_url("其它").contains("acme-v02"));
    }

    #[test]
    fn hmac_sha256_matches_rfc_vector() {
        // RFC 4231 / 著名测试向量
        assert_eq!(
            hex::encode(hmac_sha256(
                b"key",
                b"The quick brown fox jumps over the lazy dog"
            )),
            "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"
        );
    }

    #[test]
    fn eab_signature_is_hs256_of_signing_input() {
        // 固定向量自校验：HS256(base64url 段拼接) 与手工 hmac 一致
        let (k, _) = AccountKey::generate().unwrap();
        let payload = k.jwk_json().to_string();
        let protected = serde_json::json!({"alg":"HS256","kid":"kid-1","url":"https://x/dir"});
        let signing_input = format!(
            "{}.{}",
            b64url(protected.to_string().as_bytes()),
            b64url(payload.as_bytes())
        );
        let v = eab_jws("kid-1", &b64url(b"0123456789abcdef"), "https://x/dir", &k).unwrap();
        assert_eq!(
            v["signature"],
            b64url(&hmac_sha256(b"0123456789abcdef", signing_input.as_bytes()))
        );
    }

    #[test]
    fn cert_key_algorithms_supported() {
        for alg in ["ec256", "ec384", "rsa2048"] {
            let (kp, pem) = generate_cert_key(alg).unwrap();
            assert!(!pem.is_empty());
            assert!(!kp.serialize_pem().is_empty());
        }
    }
}
