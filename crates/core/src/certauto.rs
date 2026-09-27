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
use crate::model;
use crate::model::{CertAutomation, CertRecord, CertRunRecord, DeployResult};
use crate::paths::write_with_backup;
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

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > 128 || !id.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c)) {
        return Err(AppError::new("CERT_AUTO_ID", "证书自动化标识无效，未访问账号密钥或执行任务"));
    }
    Ok(())
}

fn busy_error() -> AppError {
    AppError::new("CERT_AUTO_BUSY", "该证书自动化正在执行或修改，请等待当前操作结束")
        .with_hint("可查看执行状态和历史；运行期间不能重复签发、编辑、切换自动续签或删除")
}

/// 签发、修改、删除和恢复共用操作系统锁，跨窗口/进程互斥；进程退出自动释放。
fn execution_lock(store: &crate::store::Store, id: &str) -> Result<std::fs::File> {
    validate_id(id)?;
    let dir = store.path.parent().ok_or_else(|| AppError::new("CERT_AUTO_PATH", "证书数据目录无效"))?.join("certauto-locks");
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(dir.join(format!("{id}.lock")))?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => busy_error(),
        std::fs::TryLockError::Error(error) => AppError::io("锁定证书自动化", error),
    })?;
    Ok(file)
}

fn is_running(a: &CertAutomation) -> bool { matches!(a.state.as_str(), "issuing" | "manual_wait") }
fn next_revision(previous: i64) -> i64 { now_ms().max(previous.saturating_add(1)) }
fn load_automation(store: &crate::store::Store, id: &str) -> Result<CertAutomation> {
    store.get_cert_automation(id)?.ok_or_else(|| AppError::new("NOT_FOUND", "自动化不存在，可能已在其它窗口删除"))
}

/// 调用者必须持该任务执行锁；有状态却无锁主才算中断，不把另一窗口的工作标成失败。
fn recover_locked(store: &crate::store::Store, mut a: CertAutomation) -> Result<CertAutomation> {
    if !is_running(&a) { return Ok(a); }
    a.state = "error".into();
    a.enabled = false;
    a.next_renew_at = i64::MAX / 2;
    a.last_error = "上次签发已中断，未确认 DNS 清理和部署结果。请检查后手动重试；确认配置无误后可重新启用自动续签".into();
    a.fail_count = a.fail_count.saturating_add(1);
    a.updated_at = next_revision(a.updated_at);
    let mut log = vec![a.last_error.clone()];
    log.extend(a.manual_records.iter().map(|r| format!("中断时的 TXT 记录（请核对并清理）：{} → {}", r.name, r.value)));
    a.runs.insert(0, CertRunRecord { at: a.updated_at, ok: false, message: "签发中断，等待人工检查".into(), log });
    a.runs.truncate(MAX_RUNS);
    store.save_cert_automation(&a)?;
    Ok(a)
}

fn recover_interrupted(store: &crate::store::Store) -> Result<()> {
    for a in store.list_cert_automations()? {
        if !is_running(&a) { continue; }
        let _lock = match execution_lock(store, &a.id) {
            Ok(lock) => lock,
            Err(error) if error.code == "CERT_AUTO_BUSY" => continue,
            Err(error) => return Err(error),
        };
        if let Some(current) = store.get_cert_automation(&a.id)? { recover_locked(store, current)?; }
    }
    Ok(())
}

fn read_account_key(path: &std::path::Path) -> Result<Option<String>> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::io("读取 ACME 账号密钥", e)),
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(AppError::new("CERT_ACCOUNT_KEY", "ACME 账号密钥不是普通文件，请检查证书目录"));
    }
    std::fs::read_to_string(path).map(Some).map_err(|e| AppError::io("读取 ACME 账号密钥", e))
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
const NOTIFY_KINDS: &[&str] = &[
    "", "none", "generic", "dingtalk", "wecom", "feishu", "email",
];
const KEY_ALGS: &[&str] = &["ec256", "ec384", "rsa2048", "rsa3072", "rsa4096"];
/// 执行历史最多保留条数
const MAX_RUNS: usize = 20;

fn validate(a: &CertAutomation) -> Result<()> {
    validate_id(&a.id)?;
    let domains = crate::tls::normalize_domains(&a.domains)?;
    if domains.iter().any(|d| d == "localhost" || d.parse::<std::net::IpAddr>().is_ok()) {
        return Err(AppError::new("BAD_DOMAINS", "DNS 验证需要完整域名，不能使用 localhost 或 IP 地址"));
    }
    if !(1..=90).contains(&a.renew_days_ahead) || !(0..=100).contains(&a.retry_times)
        || !(1..=1440).contains(&a.retry_interval_min) || !(0..=3600).contains(&a.dns_wait_sec) {
        return Err(AppError::new("CERT_AUTO_SCHEDULE", "续签提前天数应为 1–90，重试次数为 0–100，重试间隔为 1–1440 分钟，DNS 等待为 0–3600 秒"));
    }
    if a.domains.is_empty() {
        return Err(AppError::new("BAD_DOMAINS", "至少填写一个要签发的域名"));
    }
    for d in &a.domains {
        let d = d.trim_end_matches('.');
        if d.contains('*') && !d.starts_with("*.") {
            return Err(AppError::new(
                "BAD_DOMAINS",
                format!("通配符域名格式不对：{d}"),
            ));
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
            || (a.dns.kind != "cloudflare"
                && a.dns.kind != "digitalocean"
                && a.dns.secret.trim().is_empty()))
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
            format!(
                "通知方式仅支持：{}",
                NOTIFY_KINDS
                    .iter()
                    .filter(|k| !k.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        ));
    }
    if !KEY_ALGS.contains(&a.key_alg.as_str()) {
        return Err(AppError::new(
            "BAD_KEY_ALG",
            format!("私钥算法仅支持：{}", KEY_ALGS.join(" / ")),
        ));
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
    let crt_path = state
        .paths
        .certs()
        .join("sites")
        .join(format!("{file_stem}.crt"));
    let key_path = state
        .paths
        .certs()
        .join("sites")
        .join(format!("{file_stem}.key"));
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
        // 证书文件已经落盘，但服务没有真正加载新证书时必须让调用方知道，
        // 不能继续显示“本地部署完成”或把自动化记成成功。
        crate::ops::rebuild_and_reload(&state.store, &state.paths, &state.manager).map_err(|e| {
            AppError::new("CERT_RELOAD_FAILED", "证书已写入，但 HTTPS 服务重载失败")
                .with_hint("检查服务日志和配置后手动重启 Web 服务；证书文件仍保留")
                .with_detail(e.to_string())
        })?;
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
    if !a.manual_records.iter().any(|r| r.name == name && r.value == value) {
        a.manual_records.push(model::DnsTxtRecord {
            name: name.to_string(),
            value: value.to_string(),
        });
    }
    a.state = "manual_wait".into();
    a.updated_at = next_revision(a.updated_at);
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
    let existing = read_account_key(&key_path)?;
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
        crate::paths::write_atomic(&key_path, pem.as_bytes()).map_err(|e| AppError::io("保存 ACME 账号密钥", e))?;
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
            let timeout = if a.dns_wait_sec > 0 {
                a.dns_wait_sec as u64
            } else {
                3600
            };
            log.push(format!(
                "等待 TXT 生效（每 10s 检查一次，最长 {} 秒）…",
                timeout
            ));
            dnsprov::wait_txt_visible(&name, value, timeout)?;
            log.push("TXT 已生效，继续验证".into());
            Ok(("manual".into(), name))
        } else {
            if dns_domain != domain {
                log.push(format!(
                    "CNAME 代理：TXT 写到 {dns_domain}（{domain} 的验证经 CNAME 命中）"
                ));
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
    let (chain, key_pem, not_before, not_after) = client.issue(
        &a.domains,
        &a.key_alg,
        a.dns_wait_sec,
        &mut set_txt,
        &mut clear_txt,
    )?;

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
                DeployResult {
                    ok: true,
                    message: msg,
                    at: now_ms(),
                }
            }
            Err(e) => {
                log.push(format!("[{}] 失败：{}", t.name, e));
                DeployResult {
                    ok: false,
                    message: e.to_string(),
                    at: now_ms(),
                }
            }
        };
        t.last_result = Some(r);
    }
    Ok((record, targets))
}

/// 执行一次（手动「立即签发」与调度器共用）。同步阻塞，调用方负责放线程里。
/// 全程留痕：日志行进 runs 历史（certd 的执行日志），成功/失败发 webhook 通知。
pub fn run_once(state: &CoreState, id: &str) -> Result<CertAutomation> {
    let work = crate::BackgroundWork::begin(format!("证书签发（{id}）"))?;
    run_once_registered(state, id, work, false)
}

/// 持执行锁后再次读取排期，防止调度快照过期导致关闭后仍签发或刚续完又续。
fn claim_run(store: &crate::store::Store, id: &str, scheduled: bool) -> Result<CertAutomation> {
    let mut a = load_automation(store, id)?;
    if is_running(&a) {
        let a = recover_locked(store, a)?;
        return Err(AppError::new("CERT_AUTO_INTERRUPTED", a.last_error));
    }
    if scheduled && (!a.enabled || a.next_renew_at > now_ms()) {
        return Err(AppError::new("CERT_AUTO_NOT_DUE", "自动续签已关闭或尚未到执行时间"));
    }
    a.domains = crate::tls::normalize_domains(&a.domains)?;
    validate(&a)?;
    a.state = "issuing".into();
    a.last_error = String::new();
    a.last_run_at = now_ms();
    a.manual_records.clear();
    a.updated_at = next_revision(a.updated_at);
    store.save_cert_automation(&a)?;
    Ok(a)
}

fn run_once_registered(state: &CoreState, id: &str, _work: crate::BackgroundWork, scheduled: bool) -> Result<CertAutomation> {
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = execution_lock(&state.store, id)?;
    let mut a = claim_run(&state.store, id, scheduled)?;
    let mut log: Vec<String> = Vec::new();
    state.emit_event(Event::CertAuto {
        id: a.id.clone(),
        state: "issuing".into(),
        message: String::new(),
    });
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
                    let ahead = if a.renew_days_ahead > 0 {
                        a.renew_days_ahead
                    } else {
                        RENEW_AHEAD_DAYS
                    };
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
            let interval = if a.retry_interval_min > 0 {
                a.retry_interval_min
            } else {
                30
            };
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
    a.updated_at = next_revision(state.store.get_cert_automation(id)?.map(|current| current.updated_at).unwrap_or(a.updated_at));
    state.store.save_cert_automation(&a)?;

    let message = if a.state == "error" {
        a.last_error.clone()
    } else {
        "ok".into()
    };
    state.emit_event(Event::CertAuto {
        id: a.id.clone(),
        state: a.state.clone(),
        message: message.clone(),
    });

    // 通知：钉钉 / 企微 / 飞书 / 通用 webhook，或 SMTP 邮件。失败只报进度事件，不影响签发结果。
    if a.notify_kind == "email" {
        if let Some(smtp) = a.notify_smtp.as_ref() {
            let title = format!(
                "证书{}：{}",
                if a.state == "ok" {
                    "签发成功"
                } else {
                    "签发失败"
                },
                a.domains.join(", ")
            );
            let detail = match &outcome {
                Ok(_) => format!("有效期至 {}", fmt_date(a.expires_at)),
                Err(e) => e.to_string(),
            };
            if let Err(ne) = certdeploy::notify_email(smtp, a.state == "ok", &title, &detail) {
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
    } else if !a.notify_kind.is_empty()
        && a.notify_kind != "none"
        && !a.notify_url.trim().is_empty()
    {
        let title = if a.state == "ok" {
            format!("证书签发成功：{}", a.domains.join(", "))
        } else {
            format!("证书签发失败：{}", a.domains.join(", "))
        };
        let detail = match &outcome {
            Ok(_) => format!("有效期至 {}", fmt_date(a.expires_at)),
            Err(e) => e.to_string(),
        };
        if let Err(ne) = certdeploy::notify(
            &a.notify_kind,
            &a.notify_url,
            a.state == "ok",
            &title,
            &detail,
        ) {
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
    let Ok(_work) = crate::BackgroundWork::begin("证书自动化调度") else { return Vec::new(); };
    let Ok(_activity) = crate::paths::DataDirActivity::shared(&state.paths.base) else { return Vec::new(); };
    if recover_interrupted(&state.store).is_err() { return Vec::new(); }
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
                let _ = a_emit_manual_due(state, &a.id);
                continue;
            }
            // 每个任务独立线程：一个任务卡住（网络慢 / 手动等待）不拖累其它
            let st = state.clone();
            let id = a.id.clone();
            // 先注册再派生线程，退出准备不会漏掉已接受、尚未开始的签发。
            let Ok(work) = crate::BackgroundWork::begin(format!("证书签发（{id}）")) else { break; };
            if std::thread::Builder::new().name("certificate-issue".into()).spawn(move || {
                let _ = run_once_registered(&st, &id, work, true);
            }).is_ok() { processed.push(a.id); }
        }
    }
    processed
}

/// 手动模式到期提醒：状态回 idle + 发事件，等用户来点
fn a_emit_manual_due(state: &CoreState, id: &str) -> Result<()> {
    let _lock = execution_lock(&state.store, id)?;
    if let Some(mut a) = state.store.get_cert_automation(id)? {
        if !a.enabled || is_running(&a) || a.next_renew_at > now_ms() || a.dns.kind != "manual" || a.issued_at.is_none() { return Ok(()); }
        a.state = "idle".into();
        a.last_error = "证书到期需要手动续签（点「立即续签」会给出 TXT 记录）".into();
        a.next_renew_at = now_ms() + 24 * 3600 * 1000; // 明天再提醒
        a.updated_at = next_revision(a.updated_at);
        state.store.save_cert_automation(&a)?;
        state.emit_event(Event::CertAuto {
            id: a.id.clone(),
            state: "manual_due".into(),
            message: a.last_error.clone(),
        });
    }
    Ok(())
}

/// 后台调度线程：启动 30 秒后先跑一轮（覆盖「开应用就能续上」），
/// 之后每小时 tick。桌面端在 CoreState 初始化后 spawn 一次。
pub fn spawn_scheduler(state: Arc<CoreState>) {
    spawn_scheduler_when_ready(state,None);
}

pub fn spawn_scheduler_when_ready(state: Arc<CoreState>, gate: Option<std::sync::Arc<crate::restart::StartupGate>>) {
    std::thread::spawn(move || {
        if gate.is_some_and(|gate| !gate.wait()) { return; }
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
        let _work = crate::BackgroundWork::begin("检查证书任务状态")?;
        let _activity = crate::paths::DataDirActivity::shared(&self.paths.base)?;
        recover_interrupted(&self.store)?;
        self.store.list_cert_automations()
    }

    pub fn certauto_save(&self, mut a: CertAutomation) -> Result<CertAutomation> {
        let _work = crate::BackgroundWork::begin("保存证书自动化")?;
        let _activity = crate::paths::DataDirActivity::shared(&self.paths.base)?;
        let creating = a.id.trim().is_empty();
        if a.id.trim().is_empty() {
            a.id = format!("auto-{}-{:016x}", now_ms(), rand::random::<u64>());
        }
        let _lock = execution_lock(&self.store, &a.id)?;
        a.domains = crate::tls::normalize_domains(&a.domains)?;
        if a.name.trim().is_empty() {
            a.name = a.domains.first().cloned().unwrap_or_else(|| a.id.clone());
        }
        validate(&a)?;
        if creating {
            if self.store.get_cert_automation(&a.id)?.is_some() { return Err(busy_error()); }
            a.state = "idle".into(); a.last_error.clear(); a.cert_id = None;
            a.issued_at = None; a.expires_at = None; a.last_run_at = 0; a.fail_count = 0;
            a.runs.clear(); a.manual_records.clear();
            a.created_at = now_ms(); a.updated_at = a.created_at;
            a.next_renew_at = if a.enabled { now_ms() } else { i64::MAX / 2 };
            for target in &mut a.targets { target.last_result = None; }
        } else {
            let existing = recover_locked(&self.store, load_automation(&self.store, &a.id)?)?;
            if a.updated_at != existing.updated_at {
                return Err(AppError::new("CERT_AUTO_CONFLICT", "自动化状态或配置已在其它操作中更新，未覆盖最新记录")
                    .with_hint("当前表单内容仍保留。请关闭后重新打开编辑，核对最新结果再修改"));
            }
            let identity_changed = a.domains != existing.domains || a.key_alg != existing.key_alg || a.ca != existing.ca;
            // 客户端只能修改配置，执行结果、排期、历史和自动续签开关由各自入口维护。
            a.state = existing.state; a.last_error = existing.last_error; a.cert_id = existing.cert_id;
            a.issued_at = existing.issued_at; a.expires_at = existing.expires_at;
            a.last_run_at = existing.last_run_at; a.fail_count = existing.fail_count;
            a.runs = existing.runs; a.manual_records = existing.manual_records;
            a.enabled = existing.enabled; a.created_at = existing.created_at;
            a.next_renew_at = if a.enabled && a.renew_days_ahead != existing.renew_days_ahead {
                a.expires_at.filter(|exp| *exp > 0).map(|exp| exp.saturating_sub(a.renew_days_ahead * 86_400_000)).unwrap_or(existing.next_renew_at)
            } else { existing.next_renew_at };
            a.updated_at = next_revision(existing.updated_at);
            for target in &mut a.targets {
                target.last_result = existing.targets.iter().find(|old| old.id == target.id && old.kind == target.kind && old.config == target.config)
                    .and_then(|old| old.last_result.clone());
            }
            if identity_changed {
                // 旧证书文件/历史保留给已有站点；不能把旧域名或旧算法的结果显示成新配置已签发。
                a.state = "idle".into(); a.last_error.clear(); a.cert_id = None;
                a.issued_at = None; a.expires_at = None; a.fail_count = 0;
                a.next_renew_at = if a.enabled { now_ms() } else { i64::MAX / 2 };
                for target in &mut a.targets { target.last_result = None; }
            }
        }
        self.store.save_cert_automation(&a)?;
        Ok(a)
    }

    pub fn certauto_delete(&self, id: &str) -> Result<bool> {
        let _work = crate::BackgroundWork::begin("删除证书自动化")?;
        let _activity = crate::paths::DataDirActivity::shared(&self.paths.base)?;
        let _lock = execution_lock(&self.store, id)?;
        load_automation(&self.store, id)?;
        // 账号密钥一并清掉；已签发的证书文件保留（站点可能还在用）
        let path = account_key_path(&self.paths, id);
        let previous = read_account_key(&path)?;
        if previous.is_some() { std::fs::remove_file(&path).map_err(|e| AppError::io("删除 ACME 账号密钥", e))?; }
        if let Err(error) = self.store.delete_cert_automation(id) {
            if let Some(previous) = previous {
                crate::paths::write_atomic(&path, previous.as_bytes()).map_err(|restore| AppError::new(
                    "CERT_AUTO_DELETE_FAILED", format!("删除记录失败，账号密钥恢复也失败：{error}；{restore}")))?;
            }
            return Err(error);
        }
        Ok(true)
    }

    pub fn certauto_set_enabled(&self, id: &str, enabled: bool) -> Result<CertAutomation> {
        let _work = crate::BackgroundWork::begin("切换证书自动续签")?;
        let _activity = crate::paths::DataDirActivity::shared(&self.paths.base)?;
        let _lock = execution_lock(&self.store, id)?;
        let mut a = recover_locked(&self.store, load_automation(&self.store, id)?)?;
        if enabled { validate(&a)?; }
        if a.enabled == enabled { return Ok(a); }
        a.enabled = enabled;
        a.updated_at = next_revision(a.updated_at);
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

    fn fixture() -> (tempfile::TempDir, Arc<CoreState>, CertAutomation) {
        let dir = tempfile::tempdir().unwrap();
        let state = CoreState::init(Some(dir.path().to_path_buf()), Arc::new(|_| {})).unwrap();
        let a = sample(); state.store.save_cert_automation(&a).unwrap();
        (dir, state, a)
    }

    #[test]
    fn certificate_lock_probe() {
        let Some(root) = std::env::var_os("NSB_CERTIFICATE_LOCK_PROBE") else { return; };
        let paths = crate::paths::Paths::new(root.into());
        let store = crate::store::Store::open(paths.db()).unwrap();
        let _lock = execution_lock(&store, "auto-1").unwrap();
        let mut a = claim_run(&store, "auto-1", false).unwrap();
        a.state = "manual_wait".into();
        a.manual_records.push(model::DnsTxtRecord { name: "_acme-challenge.example.invalid".into(), value: "fixture-value".into() });
        store.save_cert_automation(&a).unwrap();
        std::fs::write(paths.base.join("lock-ready"), "ready").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(5)); // 父进程会提前结束此有限夹具。
    }

    #[test]
    fn live_process_blocks_conflicts_and_crash_is_recovered_once() {
        let (dir, state, original) = fixture();
        let mut child = platform::command(std::env::current_exe().unwrap())
            .args(["--exact", "certauto::tests::certificate_lock_probe", "--nocapture"])
            .env("NSB_CERTIFICATE_LOCK_PROBE", dir.path()).spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !dir.path().join("lock-ready").is_file() {
            assert!(std::time::Instant::now() < deadline);
            assert!(child.try_wait().unwrap().is_none());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(state.certauto_list().unwrap()[0].state, "manual_wait");
        assert_eq!(run_once(&state, &original.id).unwrap_err().code, "CERT_AUTO_BUSY");
        assert_eq!(state.certauto_save(original.clone()).unwrap_err().code, "CERT_AUTO_BUSY");
        assert_eq!(state.certauto_set_enabled(&original.id, false).unwrap_err().code, "CERT_AUTO_BUSY");
        assert_eq!(state.certauto_delete(&original.id).unwrap_err().code, "CERT_AUTO_BUSY");
        child.kill().unwrap(); child.wait().unwrap();
        let recovered = state.certauto_list().unwrap().remove(0);
        assert_eq!(recovered.state, "error"); assert!(!recovered.enabled);
        assert!(recovered.last_error.contains("中断"));
        assert_eq!(recovered.manual_records.len(), 1);
        assert_eq!(recovered.runs.len(), 1);
        assert!(recovered.runs[0].log.iter().any(|line| line.contains("fixture-value")));
        assert_eq!(state.certauto_list().unwrap()[0].runs.len(), 1);
        assert!(tick(&state).is_empty());
        let enabled = state.certauto_set_enabled(&original.id, true).unwrap();
        assert!(enabled.enabled);
        assert_eq!(enabled.runs.len(), 1);
    }

    #[test]
    fn edits_preserve_runtime_reject_stale_forms_and_never_resurrect_deleted_tasks() {
        let (_dir, state, mut a) = fixture();
        a.state = "ok".into(); a.cert_id = Some("certificate-fixture".into());
        a.issued_at = Some(100); a.expires_at = Some(now_ms() + 60 * 86_400_000);
        a.runs.push(CertRunRecord { at: 100, ok: true, message: "saved result".into(), log: vec![] });
        a.targets[0].last_result = Some(DeployResult { ok: true, message: "saved deployment".into(), at: 100 });
        state.store.save_cert_automation(&a).unwrap();
        let mut edited = a.clone(); edited.name = "new name".into();
        edited.state = "error".into(); edited.cert_id = None; edited.runs.clear(); edited.enabled = false;
        edited.targets[0].last_result = None;
        edited.domains = vec![" A.COM. ".into(), "a.com".into()];
        let saved = state.certauto_save(edited).unwrap();
        assert_eq!(saved.domains, vec!["a.com"]); assert!(saved.enabled);
        assert_eq!(saved.state, "ok"); assert_eq!(saved.cert_id, a.cert_id);
        assert_eq!(saved.runs.len(), 1); assert!(saved.targets[0].last_result.as_ref().unwrap().ok);
        assert!(saved.updated_at > a.updated_at);
        assert_eq!(state.certauto_save(a).unwrap_err().code, "CERT_AUTO_CONFLICT");
        let mut identity = saved.clone(); identity.domains = vec!["changed.example.invalid".into()];
        let changed = state.certauto_save(identity).unwrap();
        assert_eq!(changed.state, "idle"); assert!(changed.cert_id.is_none()); assert!(changed.expires_at.is_none());
        assert_eq!(changed.runs.len(), 1); assert!(changed.targets[0].last_result.is_none());
        state.certauto_set_enabled(&saved.id, false).unwrap();
        assert_eq!(state.certauto_save(saved.clone()).unwrap_err().code, "CERT_AUTO_CONFLICT");
        state.certauto_delete(&saved.id).unwrap();
        assert_eq!(state.certauto_save(saved.clone()).unwrap_err().code, "NOT_FOUND");
        assert_eq!(state.certauto_delete(&saved.id).unwrap_err().code, "NOT_FOUND");
        let mut new = saved; new.id.clear();
        let created = state.certauto_save(new.clone()).unwrap();
        assert_eq!(created.state, "idle"); assert!(created.runs.is_empty());
        assert!(created.cert_id.is_none()); assert!(created.targets[0].last_result.is_none());
        assert_ne!(created.id, state.certauto_save(new).unwrap().id);
    }

    #[test]
    fn delayed_scheduler_rechecks_enable_and_due_without_starting_acme() {
        let (_dir, state, mut a) = fixture();
        let _lock = execution_lock(&state.store, &a.id).unwrap();
        a.enabled = false; state.store.save_cert_automation(&a).unwrap();
        assert_eq!(claim_run(&state.store, &a.id, true).unwrap_err().code, "CERT_AUTO_NOT_DUE");
        a.enabled = true; a.next_renew_at = now_ms() + 86_400_000;
        state.store.save_cert_automation(&a).unwrap();
        assert_eq!(claim_run(&state.store, &a.id, true).unwrap_err().code, "CERT_AUTO_NOT_DUE");
        a.enabled = false; state.store.save_cert_automation(&a).unwrap();
        let manual = claim_run(&state.store, &a.id, false).unwrap();
        assert_eq!(manual.state, "issuing"); assert!(!manual.enabled);
        let mut log = Vec::new();
        mark_manual_wait(&state, &a.id, "_acme-challenge.example.invalid", "first", &mut log).unwrap();
        mark_manual_wait(&state, &a.id, "_acme-challenge.example.invalid", "second", &mut log).unwrap();
        mark_manual_wait(&state, &a.id, "_acme-challenge.example.invalid", "second", &mut log).unwrap();
        let records = state.store.get_cert_automation(&a.id).unwrap().unwrap().manual_records;
        assert_eq!(records.len(), 2, "同一 TXT 名称可有不同验证值");
    }

    #[test]
    fn invalid_identifiers_domains_and_corrupt_rows_fail_without_hiding_data() {
        let (_dir, state, a) = fixture();
        for id in ["../outside", "C:\\outside", "a/b", "", "a.b"] {
            assert!(execution_lock(&state.store, id).is_err());
            assert!(state.certauto_delete(id).is_err());
        }
        for domain in ["../bad.example", "example.com/escape", "127.0.0.1", "localhost", "*.127.0.0.1"] {
            let mut bad = a.clone(); bad.domains = vec![domain.into()];
            assert!(validate(&bad).is_err());
        }
        let db = rusqlite::Connection::open(state.paths.db()).unwrap();
        db.execute("UPDATE cert_automations SET data=?1 WHERE id=?2", rusqlite::params!["{", a.id]).unwrap();
        assert_eq!(state.certauto_list().unwrap_err().code, "CERT_AUTO_CORRUPT");
        assert_eq!(state.store.get_cert_automation(&a.id).unwrap_err().code, "CERT_AUTO_CORRUPT");
        let count: i64 = db.query_row("SELECT count(*) FROM cert_automations", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn account_key_errors_are_visible_and_failed_delete_restores_key() {
        let (_dir, state, a) = fixture();
        let key = account_key_path(&state.paths, &a.id);
        std::fs::create_dir_all(&key).unwrap();
        assert_eq!(state.certauto_delete(&a.id).unwrap_err().code, "CERT_ACCOUNT_KEY");
        // run_inner 在读取账号文件时即失败，不会访问任何 CA/DNS/部署/通知服务。
        let result = run_once(&state, &a.id).unwrap();
        assert_eq!(result.state, "error"); assert!(result.last_error.contains("普通文件"));
        assert_eq!(result.runs.len(), 1);
        std::fs::remove_dir(&key).unwrap();
        std::fs::write(&key, "fixture-account-key").unwrap();
        let db = rusqlite::Connection::open(state.paths.db()).unwrap();
        // 普通 trigger 对独立连接生效；仅在临时数据库中制造删除失败。
        db.execute_batch("CREATE TRIGGER block_fixture_delete BEFORE DELETE ON cert_automations BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(state.certauto_delete(&a.id).is_err());
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "fixture-account-key");
        assert!(state.store.get_cert_automation(&a.id).unwrap().is_some());
        db.execute_batch("DROP TRIGGER block_fixture_delete;").unwrap();
        state.certauto_delete(&a.id).unwrap();
        assert!(!key.exists());
    }

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
