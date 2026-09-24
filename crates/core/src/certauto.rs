//! 证书自动化：一条自动化 = 要签的域名 + DNS 凭据 + 部署目标。
//!
//! 流程（对齐 certd 的「申请 → 部署 → 定时续签」闭环）：
//! 1. ACME 下单，DNS-01 验证（TXT 写入由 dnsprov 完成，验证完即清理）；
//! 2. 拿到证书链后：本地部署（落 certs/sites/{主域名}.crt/.key，命中已开 HTTPS 的站点就重载 nginx）；
//! 3. 逐个推送外部部署目标（宝塔 / 1Panel / 阿里云），单项失败不阻断其它；
//! 4. nextRenewAt = 到期前 30 天；后台线程每小时 tick 一次，到点自动重签。
//!
//! 手动「立即签发」与调度器共用同一条 run_once 路径，避免两套行为。

use crate::acme::AcmeClient;
use crate::certdeploy;
use crate::dnsprov;
use crate::error::{AppError, Result};
use crate::model::{CertAutomation, CertRecord, CertRunRecord, DeployResult};
use crate::paths::write_with_backup;
use crate::model;
use crate::{CoreState, Event};
use std::sync::Arc;
use time::OffsetDateTime;

/// 提前续签窗口（对齐 certd 默认）：到期前 30 天
pub const RENEW_AHEAD_DAYS: i64 = 30;
/// 失败后的重试间隔：6 小时（调度器每小时 tick，实际最多滞后 1 小时）
pub const RETRY_AFTER_MS: i64 = 6 * 3600 * 1000;

fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

fn account_key_path(base: &crate::paths::Paths, id: &str) -> std::path::PathBuf {
    base.certs().join("acme").join(format!("{id}.account.key"))
}

/* ---------- 校验 ---------- */

const DNS_KINDS: &[&str] = &[
    "aliyun",
    "cloudflare",
    "dnspod",
    "huawei",
    "godaddy",
    "digitalocean",
    "porkbun",
    // 不接 API：用户自己加 TXT 记录，等公共解析可见后继续（certd 的手动模式）
    "manual",
];
const TARGET_KINDS: &[&str] = &["btpanel", "onepanel", "aliyun", "tencent", "ssh", "local"];
const NOTIFY_KINDS: &[&str] = &["", "none", "generic", "dingtalk", "wecom", "feishu", "email"];
const KEY_ALGS: &[&str] = &["ec256", "ec384", "rsa2048", "rsa3072", "rsa4096"];
/// 执行历史最多保留条数
const MAX_RUNS: usize = 20;

fn validate(a: &CertAutomation) -> Result<()> {
    if a.domains.is_empty() {
        return Err(AppError::new("BAD_DOMAINS", "至少填写一个要签发的域名"));
    }
    for d in &a.domains {
        let d = d.trim_end_matches('.');
        if d.contains('*') && !d.starts_with("*.") {
            return Err(AppError::new("BAD_DOMAINS", format!("通配符域名格式不对：{d}")));
        }
        if !d.contains('.') {
            return Err(AppError::new("BAD_DOMAINS", format!("域名不完整：{d}")));
        }
    }
    if !DNS_KINDS.contains(&a.dns.kind.as_str()) {
        return Err(AppError::new(
            "DNS_PROVIDER",
            format!("DNS 服务商仅支持：{}", DNS_KINDS.join(" / ")),
        ));
    }
    // manual：无需任何凭据
    if a.dns.kind != "manual"
        && (a.dns.access_key.trim().is_empty()
            || (a.dns.kind != "cloudflare" && a.dns.kind != "digitalocean" && a.dns.secret.trim().is_empty()))
    {
        return Err(AppError::new("DNS_PROVIDER", "DNS 凭据不完整"));
    }
    for t in &a.targets {
        if !TARGET_KINDS.contains(&t.kind.as_str()) {
            return Err(AppError::new(
                "DEPLOY_KIND",
                format!("部署目标「{}」类型未知：{}", t.name, t.kind),
            ));
        }
    }
    if a.notify_kind == "email" {
        let Some(smtp) = a.notify_smtp.as_ref() else {
            return Err(AppError::new("BAD_NOTIFY", "邮件通知需要填写 SMTP 配置"));
        };
        if smtp.host.trim().is_empty() || smtp.to.trim().is_empty() {
            return Err(AppError::new("BAD_NOTIFY", "SMTP 服务器与收件人不能为空"));
        }
    }
    if !NOTIFY_KINDS.contains(&a.notify_kind.as_str()) {
        return Err(AppError::new(
            "BAD_NOTIFY",
            format!("通知方式仅支持：{}", NOTIFY_KINDS.iter().filter(|k| !k.is_empty()).copied().collect::<Vec<_>>().join(" / ")),
        ));
    }
    if !KEY_ALGS.contains(&a.key_alg.as_str()) {
        return Err(AppError::new("BAD_KEY_ALG", format!("私钥算法仅支持：{}", KEY_ALGS.join(" / "))));
    }
    Ok(())
}

/* ---------- 本地部署 ---------- */

/// 证书落到站点证书目录（与自签证书同一位置，nginx 配置无需变化）；
/// 命中已开 HTTPS 的站点时重载 web server 让新证书立刻生效。
fn deploy_local(
    state: &CoreState,
    a: &CertAutomation,
    chain: &str,
    key_pem: &str,
    not_before: i64,
    not_after: i64,
) -> Result<CertRecord> {
    let primary = a.domains[0].clone();
    // 通配符域名带 `*`，Windows 文件名不允许 —— 与 tls::issue_site_cert 同一套净化规则
    let file_stem = primary.replace('*', "_wildcard");
    let crt_path = state.paths.certs().join("sites").join(format!("{file_stem}.crt"));
    let key_path = state.paths.certs().join("sites").join(format!("{file_stem}.key"));
    std::fs::create_dir_all(state.paths.certs().join("sites"))?;
    write_with_backup(&crt_path, chain, &state.paths.backup())?;
    write_with_backup(&key_path, key_pem, &state.paths.backup())?;

    let record = CertRecord {
        id: format!("acme-{primary}"),
        kind: "acme".into(),
        subject: primary.clone(),
        sans: a.domains.clone(),
        not_before,
        not_after,
        cert_path: crt_path.to_string_lossy().to_string(),
        key_path: Some(key_path.to_string_lossy().to_string()),
        trusted: Some(true),
    };
    state.store.save_cert(&record)?;

    let hit = state
        .store
        .list_sites()?
        .iter()
        .any(|s| s.https && s.domains.iter().any(|d| d == &primary));
    if hit {
        // 重载失败不回滚证书本身：文件已换新，下次重启/重载自然生效
        let _ = crate::ops::rebuild_and_reload(&state.store, &state.paths, &state.manager);
    }
    Ok(record)
}

/* ---------- 单次执行 ---------- */

/// CNAME 代理验证的域名映射：cname_target 支持 `{domain}` 占位符。
/// 空 → 原域名（不代理）；固定值 → 全部域名共用一个授权域；含占位符 → 按域名展开。
pub fn alias_for(cname_target: &str, domain: &str) -> String {
    let t = cname_target.trim().trim_end_matches('.');
    if t.is_empty() {
        domain.to_string()
    } else if t.contains("{domain}") {
        t.replace("{domain}", domain)
    } else {
        t.to_string()
    }
}

/// 手动 DNS 模式：进入「等待用户加记录」状态 —— 记录写进 automation（前端展示+复制），
/// 状态切到 manual_wait 并发事件；完成/失败后由 run_once 统一收尾清理。
fn mark_manual_wait(
    state: &CoreState,
    id: &str,
    name: &str,
    value: &str,
    log: &mut Vec<String>,
) -> Result<()> {
    let mut a = state
        .store
        .get_cert_automation(id)?
        .ok_or_else(|| AppError::new("NOT_FOUND", "自动化不存在"))?;
    if !a.manual_records.iter().any(|r| r.name == name) {
        a.manual_records.push(model::DnsTxtRecord {
            name: name.to_string(),
            value: value.to_string(),
        });
    }
    a.state = "manual_wait".into();
    a.updated_at = now_ms();
    state.store.save_cert_automation(&a)?;
    log.push(format!("请在 DNS 控制台添加 TXT 记录：{name} → {value}"));
    state.emit_event(Event::CertAuto {
        id: id.to_string(),
        state: "manual_wait".into(),
        message: format!("{name} → {value}"),
    });
    Ok(())
}

fn run_inner(
    state: &CoreState,
    a: &CertAutomation,
    log: &mut Vec<String>,
) -> Result<(Option<CertRecord>, Vec<crate::model::DeployTarget>)> {
    // ACME 账号密钥：每个自动化独立账号（凭据隔离，换邮箱互不影响）
    let key_path = account_key_path(&state.paths, &a.id);
    let existing = std::fs::read_to_string(&key_path).ok();
    let (mut client, fresh_pem) = AcmeClient::connect(
        &a.ca,
        existing.as_deref(),
        &a.email,
        // EAB：ZeroSSL / Google / BuyPass 需要外部账号绑定，其余留空
        if a.eab_kid.trim().is_empty() || a.eab_hmac_key.trim().is_empty() {
            None
        } else {
            Some((a.eab_kid.trim(), a.eab_hmac_key.trim()))
        },
    )?;
    if let Some(pem) = fresh_pem {
        std::fs::create_dir_all(key_path.parent().unwrap_or(&state.paths.certs()))?;
        std::fs::write(&key_path, pem)?;
    }

    let manual = a.dns.kind == "manual";
    let auto_id = a.id.clone();
    let cname_target = a.cname_target.clone();
    let mut set_txt = |domain: &str, prefix: &str, value: &str| -> Result<(String, String)> {
        // CNAME 代理：TXT 实际写到授权域（_acme-challenge.<域名> 已 CNAME 过去）
        let dns_domain = alias_for(&cname_target, domain);
        if manual {
            // 手动模式：把要加的记录摆给用户，然后盯公共解析直到记录可见
            let name = format!("{prefix}.{dns_domain}");
            mark_manual_wait(state, &auto_id, &name, value, log)?;
            let timeout = if a.dns_wait_sec > 0 { (a.dns_wait_sec * 60) as u64 } else { 3600 };
            log.push(format!("等待 TXT 生效（每 10s 检查一次，最长 {} 分钟）…", timeout / 60));
            dnsprov::wait_txt_visible(&name, value, timeout)?;
            log.push("TXT 已生效，继续验证".into());
            Ok(("manual".into(), name))
        } else {
            if dns_domain != domain {
                log.push(format!("CNAME 代理：TXT 写到 {dns_domain}（{domain} 的验证经 CNAME 命中）"));
            }
            let (zone, record_id) = dnsprov::set_txt(&a.dns, &dns_domain, prefix, value)?;
            // 传播预检：公共解析确认可见再触发验证，避免 CA 查不到而 invalid
            // （自动模式等 3 分钟上限；比固定 sleep 聪明，也比直接触发稳）
            let name = format!("{prefix}.{dns_domain}");
            match dnsprov::wait_txt_visible(&name, value, 180) {
                Ok(()) => log.push(format!("TXT 已生效：{name}")),
                Err(e) => {
                    log.push(format!("TXT 传播预检未通过（继续尝试验证）：{e}"));
                    // 不阻断：个别本地 DNS 出口禁 DoH 时，服务商写入本身多半已生效
                }
            }
            Ok((zone, record_id))
        }
    };
    let mut clear_txt = |_zone: &str, _name: &str, _record_id: &str| -> Result<()> {
        // manual：记录由用户自管，不清理；自动模式也不该到这（闭包按 kind 分流）
        if manual {
            Ok(())
        } else {
            dnsprov::clear_txt(&a.dns, _zone, _name, _record_id)
        }
    };
    let (chain, key_pem, not_before, not_after) =
        client.issue(&a.domains, &a.key_alg, a.dns_wait_sec, &mut set_txt, &mut clear_txt)?;

    log.push("ACME 签发成功，开始部署".into());
    let record = if a.deploy_local {
        let rec = deploy_local(state, a, &chain, &key_pem, not_before, not_after)?;
        log.push(format!("本地部署完成：{}", rec.cert_path));
        Some(rec)
    } else {
        None
    };

    // 外部目标逐个推：单项失败记结果，不打断其它目标
    let mut targets = a.targets.clone();
    for t in targets.iter_mut() {
        let r = match certdeploy::deploy(t, &a.domains, &chain, &key_pem) {
            Ok(msg) => {
                log.push(format!("[{}] {}", t.name, msg));
                DeployResult { ok: true, message: msg, at: now_ms() }
            }
            Err(e) => {
                log.push(format!("[{}] 失败：{}", t.name, e));
                DeployResult { ok: false, message: e.to_string(), at: now_ms() }
            }
        };
        t.last_result = Some(r);
    }
    Ok((record, targets))
}

/// 执行一次（手动「立即签发」与调度器共用）。同步阻塞，调用方负责放线程里。
/// 全程留痕：日志行进 runs 历史（certd 的执行日志），成功/失败发 webhook 通知。
pub fn run_once(state: &CoreState, id: &str) -> Result<CertAutomation> {
    let mut a = state
        .store
        .get_cert_automation(id)?
        .ok_or_else(|| AppError::new("NOT_FOUND", "自动化不存在"))?;
    let mut log: Vec<String> = Vec::new();
    a.state = "issuing".into();
    a.last_error = String::new();
    a.last_run_at = now_ms();
    a.updated_at = now_ms();
    state.store.save_cert_automation(&a)?;
    state.emit_event(Event::CertAuto { id: a.id.clone(), state: "issuing".into(), message: String::new() });
    log.push(format!("开始处理：{}", a.domains.join(", ")));
    if a.key_alg != "ec256" {
        log.push(format!("证书私钥算法：{}", a.key_alg));
    }

    let outcome = run_inner(state, &a, &mut log);
    let run_at = now_ms();
    match &outcome {
        Ok((record, targets)) => {
            a.state = "ok".into();
            a.targets = targets.clone();
            a.cert_id = record.as_ref().map(|r| r.id.clone());
            a.issued_at = Some(run_at);
            a.expires_at = Some(record.as_ref().map(|r| r.not_after).unwrap_or_default());
            a.fail_count = 0;
            // 到期前 renew_days_ahead 天续签（certd 默认 30 天）；没拿到到期时间就 60 天后再看
            a.next_renew_at = match a.expires_at {
                Some(exp) if exp > 0 => {
                    let ahead = if a.renew_days_ahead > 0 { a.renew_days_ahead } else { RENEW_AHEAD_DAYS };
                    exp - ahead * 86_400_000
                }
                _ => run_at + 60 * 86_400_000,
            };
            log.push(format!("完成，证书有效期至 {}", fmt_date(a.expires_at)));
        }
        Err(e) => {
            a.state = "error".into();
            a.last_error = e.to_string();
            a.fail_count += 1;
            // 重试节奏：按配置间隔重试；连续失败超过次数后改为每天兜底重试，等人工修配置
            let interval = if a.retry_interval_min > 0 { a.retry_interval_min } else { 30 };
            a.next_renew_at = run_at
                + if a.fail_count > a.retry_times.max(1) {
                    24 * 3600 * 1000
                } else {
                    interval * 60_000
                };
            log.push(format!("失败：{}", a.last_error));
        }
    }

    // 执行历史留痕（最近 MAX_RUNS 条，新的在前）
    a.runs.insert(
        0,
        CertRunRecord {
            at: run_at,
            ok: outcome.is_ok(),
            message: match &outcome {
                Ok(_) => "签发成功".into(),
                Err(e) => e.to_string(),
            },
            log,
        },
    );
    a.runs.truncate(MAX_RUNS);
    // 手动模式等待结束（成功/失败/超时）：清掉待加记录，UI 横幅消失
    if !a.manual_records.is_empty() {
        a.manual_records.clear();
    }
    a.updated_at = now_ms();
    state.store.save_cert_automation(&a)?;

    let message = if a.state == "error" { a.last_error.clone() } else { "ok".into() };
    state.emit_event(Event::CertAuto {
        id: a.id.clone(),
        state: a.state.clone(),
        message: message.clone(),
    });

    // 通知：钉钉 / 企微 / 飞书 / 通用 webhook，或 SMTP 邮件。失败只报进度事件，不影响签发结果。
    if a.notify_kind == "email" {
        if let Some(smtp) = a.notify_smtp.as_ref() {
            let title = format!("证书{}：{}", if a.state == "ok" { "签发成功" } else { "签发失败" }, a.domains.join(", "));
            let detail = match &outcome {
                Ok(_) => format!("有效期至 {}", fmt_date(a.expires_at)),
                Err(e) => e.to_string(),
            };
            if let Err(ne) = certdeploy::notify_email(smtp, a.state == "ok", &title, &detail) {
                (state.emit)(Event::DownloadProgress(model::DownloadProgress {
                    task_id: format!("certauto-notify-{}", a.id),
                    received: 0, total: 0, speed_bps: 0, eta_sec: 0.0,
                    state: "notify-failed".into(),
                    error: Some(ne.to_string()),
                }));
            }
        }
    } else if !a.notify_kind.is_empty() && a.notify_kind != "none" && !a.notify_url.trim().is_empty() {
        let title = if a.state == "ok" {
            format!("证书签发成功：{}", a.domains.join(", "))
        } else {
            format!("证书签发失败：{}", a.domains.join(", "))
        };
        let detail = match &outcome {
            Ok(_) => format!("有效期至 {}", fmt_date(a.expires_at)),
            Err(e) => e.to_string(),
        };
        if let Err(ne) = certdeploy::notify(&a.notify_kind, &a.notify_url, a.state == "ok", &title, &detail) {
            (state.emit)(Event::DownloadProgress(model::DownloadProgress {
                task_id: format!("certauto-notify-{}", a.id),
                received: 0,
                total: 0,
                speed_bps: 0,
                eta_sec: 0.0,
                state: "notify-failed".into(),
                error: Some(ne.to_string()),
            }));
        }
    }
    Ok(a)
}

fn fmt_date(exp: Option<i64>) -> String {
    exp.map(|ms| {
        time::OffsetDateTime::from_unix_timestamp(ms / 1000)
            .map(|d| d.date().to_string())
            .unwrap_or_default()
    })
    .unwrap_or_default()
}
/// 调度 tick：执行所有「到点」的自动化，返回处理过的 id
pub fn tick(state: &Arc<CoreState>) -> Vec<String> {
    let mut processed = Vec::new();
    let Ok(list) = state.store.list_cert_automations() else {
        return processed;
    };
    let now = now_ms();
    for a in list {
        if !a.enabled || a.state == "issuing" || a.state == "manual_wait" {
            continue;
        }
        // 首次保存 nextRenewAt=now → 立即进入调度；此后按「到期前 N 天」节奏
        if a.next_renew_at <= now {
            // 手动模式的续期需要人加记录：调度只负责唤醒提示（发事件），
            // 不在这里占着调度线程等 1 小时 —— 用户在界面上点「立即续签」
            if a.dns.kind == "manual" && a.issued_at.is_some() {
                a_emit_manual_due(state, &a.id);
                continue;
            }
            // 每个任务独立线程：一个任务卡住（网络慢 / 手动等待）不拖累其它
            let st = state.clone();
            let id = a.id.clone();
            std::thread::spawn(move || {
                let _ = run_once(&st, &id);
            });
            processed.push(a.id);
        }
    }
    processed
}

/// 手动模式到期提醒：状态回 idle + 发事件，等用户来点
fn a_emit_manual_due(state: &CoreState, id: &str) {
    if let Ok(Some(mut a)) = state.store.get_cert_automation(id) {
        a.state = "idle".into();
        a.last_error = "证书到期需要手动续签（点「立即续签」会给出 TXT 记录）".into();
        a.next_renew_at = now_ms() + 24 * 3600 * 1000; // 明天再提醒
        a.updated_at = now_ms();
        let _ = state.store.save_cert_automation(&a);
        state.emit_event(Event::CertAuto {
            id: a.id.clone(),
            state: "manual_due".into(),
            message: a.last_error.clone(),
        });
    }
}

/// 后台调度线程：启动 30 秒后先跑一轮（覆盖「开应用就能续上」），
/// 之后每小时 tick。桌面端在 CoreState 初始化后 spawn 一次。
pub fn spawn_scheduler(state: Arc<CoreState>) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        let _ = tick(&state);
        crate::certmonitor::tick_all(&state);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3600));
            let _ = tick(&state);
            crate::certmonitor::tick_all(&state);
        }
    });
}


/* ---------- 门面（desktop 命令直通） ---------- */

impl CoreState {
    pub fn certauto_list(&self) -> Result<Vec<CertAutomation>> {
        self.store.list_cert_automations()
    }

    pub fn certauto_save(&self, mut a: CertAutomation) -> Result<CertAutomation> {
        let existing = self.store.get_cert_automation(&a.id)?;
        if a.id.trim().is_empty() {
            a.id = format!("auto-{}", now_ms());
            a.created_at = now_ms();
        } else if existing.is_none() {
            a.created_at = now_ms();
        }
        a.updated_at = now_ms();
        if a.name.trim().is_empty() {
            a.name = a.domains.first().cloned().unwrap_or_else(|| a.id.clone());
        }
        validate(&a)?;
        // 新建（或未签发过）时排到「现在」：调度器 1 小时内自动跑首签，
        // 想立刻拿证书就点「立即签发」
        if a.next_renew_at == 0 {
            a.next_renew_at = now_ms();
        }
        a.domains = a.domains.iter().map(|d| d.trim().trim_end_matches('.').to_string()).filter(|d| !d.is_empty()).collect();
        self.store.save_cert_automation(&a)?;
        Ok(a)
    }

    pub fn certauto_delete(&self, id: &str) -> Result<bool> {
        // 账号密钥一并清掉；已签发的证书文件保留（站点可能还在用）
        let _ = std::fs::remove_file(account_key_path(&self.paths, id));
        self.store.delete_cert_automation(id)?;
        Ok(true)
    }

    pub fn certauto_set_enabled(&self, id: &str, enabled: bool) -> Result<CertAutomation> {
        let mut a = self
            .store
            .get_cert_automation(id)?
            .ok_or_else(|| AppError::new("NOT_FOUND", "自动化不存在"))?;
        a.enabled = enabled;
        a.updated_at = now_ms();
        // 关掉就不再排期；重新打开则从现在起算
        a.next_renew_at = if enabled { now_ms() } else { i64::MAX / 2 };
        self.store.save_cert_automation(&a)?;
        Ok(a)
    }

    pub fn certauto_issue(&self, id: &str) -> Result<CertAutomation> {
        run_once(self, id)
    }
}

/* ================= 测试 ================= */

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CertAutomation {
        serde_json::from_str(
            r#"{
            "id": "auto-1", "name": "主站", "domains": ["a.com"],
            "email": "me@a.com", "ca": "letsencrypt",
            "dns": {"kind": "aliyun", "accessKey": "ak", "secret": "sk"},
            "deployLocal": true,
            "targets": [{"id":"t","kind":"btpanel","name":"bt","config":{"url":"http://x","apiSk":"s","siteName":"a.com"}}],
            "enabled": true, "state": "idle", "lastError": "",
            "nextRenewAt": 0, "lastRunAt": 0,
            "createdAt": 1, "updatedAt": 1
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn validate_accepts_good_and_rejects_bad() {
        let mut a = sample();
        assert!(validate(&a).is_ok());
        a.domains.clear();
        assert!(validate(&a).is_err());
        a.domains = vec!["localhost".into()];
        assert!(validate(&a).is_err());
        a.domains = vec!["*.a.com".into()];
        assert!(validate(&a).is_ok());
        a.domains = vec!["*x.a.com".into()];
        assert!(validate(&a).is_err());
    }




    #[test]
    fn cname_alias_mapping() {
        // 空 = 不代理
        assert_eq!(alias_for("", "a.com"), "a.com");
        // 固定授权域 = 全部域名共用
        assert_eq!(alias_for("acme.example.net", "a.com"), "acme.example.net");
        // 占位符 = 按域名展开（DNSPod/七牛的免费代理域就长这样）
        assert_eq!(
            alias_for("{domain}.acme.dnspod.cn", "www.a.com"),
            "www.a.com.acme.dnspod.cn"
        );
        // 尾部点号容错
        assert_eq!(alias_for("acme.example.net.", "a.com"), "acme.example.net");
    }

    #[test]
    fn validate_manual_dns_needs_no_credentials() {
        let mut a = sample();
        a.dns = serde_json::from_str(r#"{"kind":"manual"}"#).unwrap();
        assert!(validate(&a).is_ok());
        // manual 不允许调服务商接口（内部防护）
        assert!(dnsprov::set_txt(&a.dns, "a.com", "_acme-challenge", "v").is_err());
    }

    #[test]
    fn validate_dns_credentials() {
        let mut a = sample();
        a.dns.access_key = "  ".into();
        assert!(validate(&a).is_err());
        a.dns = serde_json::from_str(r#"{"kind":"cloudflare","accessKey":"token"}"#).unwrap();
        assert!(validate(&a).is_ok());
        a.dns = serde_json::from_str(r#"{"kind":"aliyun","accessKey":"ak"}"#).unwrap();
        assert!(validate(&a).is_err());
    }

    #[test]
    fn validate_target_kinds() {
        let mut a = sample();
        a.targets[0].kind = "vendorx".into();
        assert!(validate(&a).is_err());
    }

    #[test]
    fn renew_window_math() {
        let now: i64 = 1_700_000_000_000;
        let exp = now + 90 * 86_400_000;
        // 到期前 30 天 = exp - 30d
        assert_eq!(exp - RENEW_AHEAD_DAYS * 86_400_000, now + 60 * 86_400_000);
        assert!(RENEW_AHEAD_DAYS < 90);
    }
}
