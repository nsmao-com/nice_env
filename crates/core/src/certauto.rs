//! 证书自动化：一条自动化 = 要签的域名 + DNS 凭据 + 部署目标。
//!
//! 流程（对齐 certd 的「申请 → 部署 → 定时续签」闭环）：
//! 1. ACME 下单，DNS-01 验证（TXT 写入由 dnsprov 完成，验证完即清理）；
//! 2. 拿到证书链后：本地部署（落 certs/sites/{主域名}.crt/.key，命中已开 HTTPS 的站点就重载 nginx）；
//! 3. 逐个推送外部部署目标（宝塔 / 1Panel / 阿里云），单项失败不阻断其它；
//! 4. nextRenewAt = 到期前 30 天；每分钟检查到期任务，共用 DNS/输出资源时延后。
//!
//! 手动「立即签发」与调度器共用同一条 run_once 路径，避免两套行为。

use crate::acme::AcmeClient;
use crate::certdeploy;
use crate::dnsprov;
use crate::error::{AppError, Result};
use crate::model;
use crate::model::{CertAutomation, CertRecord, CertRunRecord, DeployResult};
use crate::{CoreState, Event};
use std::sync::Arc;
use time::OffsetDateTime;

/// 提前续签窗口（对齐 certd 默认）：到期前 30 天
pub const RENEW_AHEAD_DAYS: i64 = 30;
/// 6 小时重试间隔常量；实际自动化按各任务的 retryIntervalMin 排期。
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
    let base = store.path.parent().ok_or_else(|| AppError::new("CERT_AUTO_PATH", "证书数据目录无效"))?;
    let path = crate::paths::checked_data_path(base, &format!("certauto-locks/{id}.lock"))?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => busy_error(),
        std::fs::TryLockError::Error(error) => AppError::io("锁定证书自动化", error),
    })?;
    Ok(file)
}

struct LocalSiteDeployment {
    // 第三项为该输出路径的主域名，用于持久化实际证书记录。
    outputs: Vec<(std::path::PathBuf, std::path::PathBuf, String)>,
    site_count: usize,
    skipped: Vec<String>,
}

/// 默认站点同步完整覆盖的证书；显式选择的站点只跟随自身证书输出，不改写绑定。
fn local_site_deployment(paths: &crate::paths::Paths, store: &crate::store::Store, a: &CertAutomation) -> Result<LocalSiteDeployment> {
    let primary = a.domains.first().ok_or_else(|| AppError::new("BAD_DOMAINS", "至少填写一个域名"))?;
    let mut outputs = vec![(
        crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{}.crt", primary.replace('*', "_wildcard").replace(':', "_")))?,
        crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{}.key", primary.replace('*', "_wildcard").replace(':', "_")))?,
        primary.clone(),
    )];
    let mut site_count = 0;
    let mut skipped = Vec::new();
    let mut protected = Vec::new();
    let sites = store.list_sites()?;
    for site in &sites {
        if !site.https || !site.runtime.uses_default_certificate() { continue; }
        let Some(site_primary) = site.domains.first() else { continue; };
        let stem = site_primary.replace('*', "_wildcard").replace(':', "_");
        let cert = crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{stem}.crt"))?;
        let key = crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{stem}.key"))?;
        if site_primary != primary && sites.iter().any(|other|
            other.runtime.acme_cert_id.as_deref() == Some(&format!("acme-{site_primary}"))) {
            skipped.push(format!("{}（证书文件已被选择为另一 ACME 主证书）", site.name));
            continue;
        }
        let missing: Vec<_> = site.domains.iter().filter(|domain| !crate::certs::covers_domain(&a.domains, domain)).collect();
        if !missing.is_empty() {
            if missing.len() < site.domains.len() { skipped.push(format!("{}（域名未完全覆盖）", site.name)); }
            protected.push((cert, site.name.clone(), missing.into_iter().cloned().collect::<Vec<_>>()));
            continue;
        }
        site_count += 1;
        if !outputs.iter().any(|(old_cert, old_key, _)| old_cert == &cert && old_key == &key) {
            outputs.push((cert, key, site_primary.clone()));
        }
    }
    for site in &sites {
        let Some(id) = &site.runtime.acme_cert_id else { continue; };
        let (cert, _) = crate::certs::acme_paths(paths, id)?;
        if !outputs.iter().any(|(output, _, _)| output == &cert) { continue; }
        let missing: Vec<_> = site.domains.iter().filter(|domain| !crate::certs::covers_domain(&a.domains, domain)).cloned().collect();
        if !missing.is_empty() { protected.push((cert, site.name.clone(), missing)); }
        else if site.https { site_count += 1; }
    }
    // 主输出以及历史重复主域名的共享文件都不能绕过完整覆盖检查。
    for (cert, name, missing) in protected {
        if outputs.iter().any(|(output, _, _)| output == &cert) {
            return Err(AppError::new("CERT_SITE_COVERAGE", format!("证书未覆盖站点「{name}」的全部域名：{}，未替换原证书", missing.join("、")))
                .with_hint("将缺少的域名加入证书自动化后重新签发，或关闭分配给本地站点。"));
        }
    }
    Ok(LocalSiteDeployment { outputs, site_count, skipped })
}

/// 不同自动化也可能共用验证记录或输出文件。锁文件名仅含摘要，不落盘凭据/目标内容。
fn work_resources(paths: &crate::paths::Paths, a: &CertAutomation, retry: bool) -> Result<std::collections::BTreeMap<String, String>> {
    let mut resources = std::collections::BTreeMap::new();
    if !retry {
        for domain in crate::tls::normalize_domains(&a.domains)? {
            let domain = domain.trim_start_matches("*.");
            let alias = alias_for(&a.cname_target, domain).trim_end_matches('.').to_ascii_lowercase();
            resources.insert(format!("dns:_acme-challenge.{alias}"), format!("DNS 验证记录 _acme-challenge.{alias}"));
        }
    }
    if a.deploy_local && (!retry || !a.local_deploy_result.as_ref().is_some_and(|r| r.ok)) {
        // work_resources 只接收 Paths，站点列表在部署入口再补充；主路径仍受锁保护。
        let primary = a.domains.first().ok_or_else(|| AppError::new("BAD_DOMAINS", "至少填写一个域名"))?;
        let stem = primary.replace('*', "_wildcard").replace(':', "_");
        for ext in ["crt", "key"] {
            let path = crate::paths::checked_data_path(&paths.base, &format!("certs/sites/{stem}.{ext}"))?;
            resources.insert(certdeploy::local_output_resource(&path.to_string_lossy())?, format!("本地站点证书 {primary}"));
        }
    }
    for target in &a.targets {
        if retry && target.last_result.as_ref().is_some_and(|r| r.ok) { continue; }
        resources.extend(certdeploy::output_resources(target)?);
    }
    Ok(resources)
}

fn lock_resources(paths: &crate::paths::Paths, resources: std::collections::BTreeMap<String, String>) -> Result<Vec<std::fs::File>> {
    use sha2::{Digest, Sha256};
    let mut locks = Vec::new();
    for (resource, description) in resources {
        let digest = hex::encode(Sha256::digest(resource.as_bytes()));
        let path = crate::paths::checked_data_path(&paths.base, &format!("certauto-locks/resources/{digest}.lock"))?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => AppError::new("CERT_AUTO_RESOURCE_BUSY", format!("{description} 正由另一条证书自动化使用，本次尚未签发或部署"))
                .with_hint("自动任务会稍后重试；手动任务请等待另一任务结束后再试。等待期间可关闭自动续签或修改配置"),
            std::fs::TryLockError::Error(error) => AppError::io("锁定共用证书资源", error),
        })?;
        locks.push(file);
    }
    // try_lock 不等待；失败会释放本次已取得的所有锁，不形成多资源死锁。
    Ok(locks)
}

#[cfg(test)]
fn resource_locks(paths: &crate::paths::Paths, a: &CertAutomation, retry: bool) -> Result<Vec<std::fs::File>> {
    lock_resources(paths, work_resources(paths, a, retry)?)
}

fn resource_locks_for_state(state: &CoreState, a: &CertAutomation, retry: bool) -> Result<Vec<std::fs::File>> {
    let mut resources = work_resources(&state.paths, a, retry)?;
    if a.deploy_local && (!retry || !a.local_deploy_result.as_ref().is_some_and(|r| r.ok)) {
        for (cert, key, label) in local_site_deployment(&state.paths, &state.store, a)?.outputs {
            for path in [&cert, &key] {
                resources.insert(certdeploy::local_output_resource(&path.to_string_lossy())?, format!("本地站点证书 {label}"));
            }
        }
    }
    lock_resources(&state.paths, resources)
}

fn pause_before_run(store: &crate::store::Store, a: &mut CertAutomation, error: &AppError) -> Result<()> {
    a.state = if a.deployment_id.is_empty() { "error" } else { "deploy_error" }.into();
    a.enabled = false; a.next_renew_at = i64::MAX / 2;
    a.last_error = format!("执行准备失败，自动执行已暂停：{error}");
    a.updated_at = next_revision(a.updated_at);
    a.runs.insert(0, CertRunRecord { at: a.updated_at, ok: false, message: a.last_error.clone(), log: Vec::new() });
    a.runs.truncate(MAX_RUNS); store.save_cert_automation(a)
}

fn reserve_work(state: &CoreState, a: &mut CertAutomation, scheduled: bool, retry: bool) -> Result<Vec<std::fs::File>> {
    match resource_locks_for_state(state, a, retry) {
        Ok(locks) => Ok(locks),
        Err(error) => {
            if scheduled {
                if error.code == "CERT_AUTO_RESOURCE_BUSY" {
                    a.state = if retry { "deploy_waiting" } else { "waiting" }.into();
                    a.last_error = error.message.clone(); a.next_renew_at = now_ms() + 60_000;
                    a.updated_at = next_revision(a.updated_at); state.store.save_cert_automation(a)?;
                    state.emit_event(Event::CertAuto { id: a.id.clone(), state: a.state.clone(), message: a.last_error.clone() });
                } else { pause_before_run(&state.store, a, &error)?; }
            }
            Err(error)
        }
    }
}

fn is_running(a: &CertAutomation) -> bool { matches!(a.state.as_str(), "issuing" | "manual_wait" | "deploying") }
fn next_revision(previous: i64) -> i64 { now_ms().max(previous.saturating_add(1)) }
fn load_automation(store: &crate::store::Store, id: &str) -> Result<CertAutomation> {
    store.get_cert_automation(id)?.ok_or_else(|| AppError::new("NOT_FOUND", "自动化不存在，可能已在其它窗口删除"))
}

/// 调用者必须持该任务执行锁；有状态却无锁主才算中断，不把另一窗口的工作标成失败。
fn recover_locked(store: &crate::store::Store, mut a: CertAutomation) -> Result<CertAutomation> {
    if !is_running(&a) { return Ok(a); }
    let deploying = a.state == "deploying";
    a.state = if deploying { "deploy_interrupted" } else { "error" }.into();
    a.enabled = false;
    a.next_renew_at = i64::MAX / 2;
    a.last_error = if deploying {
        "上次部署已中断，自动续签已暂停。请核对目标端结果后重试部署；已确认成功的目标会跳过，未确认的操作可能再次执行"
    } else {
        "上次签发已中断，未确认 DNS 清理和部署结果。请检查后手动重试；确认配置无误后可重新启用自动续签"
    }.into();
    a.fail_count = a.fail_count.saturating_add(1);
    a.updated_at = next_revision(a.updated_at);
    let mut log = vec![a.last_error.clone()];
    log.extend(a.manual_records.iter().map(|r| format!("中断时的 TXT 记录（请核对并清理）：{} → {}", r.name, r.value)));
    a.runs.insert(0, CertRunRecord { at: a.updated_at, ok: false, message: if deploying { "部署中断，等待人工检查" } else { "签发中断，等待人工检查" }.into(), log });
    a.runs.truncate(MAX_RUNS);
    store.save_cert_automation(&a)?;
    Ok(a)
}

fn recover_interrupted(store: &crate::store::Store) -> Result<()> {
    for a in store.list_cert_automations()? {
        if !is_running(&a) && legacy_deployment_problem(&a).is_none() { continue; }
        let _lock = match execution_lock(store, &a.id) {
            Ok(lock) => lock,
            Err(error) if error.code == "CERT_AUTO_BUSY" => continue,
            Err(error) => return Err(error),
        };
        if let Some(current) = store.get_cert_automation(&a.id)? {
            let mut current = recover_locked(store, current)?;
            if let Some(problem) = legacy_deployment_problem(&current) {
                current.state = "deploy_error".into(); current.enabled = false;
                current.last_error = format!("{problem}。旧版本未保存可重试的签发材料，请核对部署目标并重新签发；自动续签已暂停");
                current.next_renew_at = i64::MAX / 2;
                current.updated_at = next_revision(current.updated_at);
                current.runs.insert(0, CertRunRecord { at: current.updated_at, ok: false, message: "已纠正旧版本的部署状态".into(), log: vec![current.last_error.clone()] });
                current.runs.truncate(MAX_RUNS);
                store.save_cert_automation(&current)?;
            }
        }
    }
    Ok(())
}

fn legacy_deployment_problem(a: &CertAutomation) -> Option<&'static str> {
    if a.state != "ok" || !a.deployment_id.is_empty() { return None; }
    if a.targets.iter().any(|target| target.last_result.as_ref().is_some_and(|r| !r.ok)) {
        Some("旧版本记录为成功，但存在失败的部署目标")
    } else if !a.deploy_local && a.expires_at.is_none_or(|exp| exp <= 0) {
        Some("旧版本没有保存远程部署证书的有效期，无法可靠安排续签")
    } else { None }
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
    let mut target_ids = std::collections::HashSet::new();
    for t in &a.targets {
        if t.id.trim().is_empty() || !target_ids.insert(&t.id) {
            return Err(AppError::new("DEPLOY_TARGET_ID", "部署目标标识为空或重复，请删除重复目标后重新添加"));
        }
        if !TARGET_KINDS.contains(&t.kind.as_str()) {
            return Err(AppError::new(
                "DEPLOY_KIND",
                format!("部署目标「{}」类型未知：{}", t.name, t.kind),
            ));
        }
        certdeploy::validate_target(t)?;
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

/// 一个原子文件保留同批证书和私钥；配置导出只带批次标识，不含签发材料。
#[derive(serde::Serialize, serde::Deserialize)]
struct IssuedMaterial {
    version: u8,
    automation_id: String,
    deployment_id: String,
    domains: Vec<String>,
    ca: String,
    key_alg: String,
    chain: String,
    key_pem: String,
}

fn issued_path(paths: &crate::paths::Paths, id: &str) -> Result<std::path::PathBuf> {
    validate_id(id)?;
    Ok(crate::paths::checked_data_path(&paths.base, &format!("certs/acme/{id}.issued.json"))?)
}

fn read_issued_file(path: &std::path::Path) -> Result<Option<Vec<u8>>> {
    use std::io::Read;
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::io("读取签发材料", e)),
    };
    const LIMIT: u64 = 8 * 1024 * 1024;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > LIMIT {
        return Err(AppError::new("CERT_DEPLOY_FILE", "签发材料必须是 8 MiB 以内的普通文件"));
    }
    let mut content = Vec::new();
    std::fs::File::open(path)?.take(LIMIT + 1).read_to_end(&mut content)?;
    if content.len() as u64 > LIMIT { return Err(AppError::new("CERT_DEPLOY_FILE", "签发材料超过大小限制")); }
    Ok(Some(content))
}

fn read_issued(state: &CoreState, a: &CertAutomation) -> Result<IssuedMaterial> {
    let content = read_issued_file(&issued_path(&state.paths, &a.id)?)?
        .ok_or_else(|| AppError::new("CERT_DEPLOY_MISSING", "未找到已签发材料，请重新签发；配置备份不包含证书私钥"))?;
    let material: IssuedMaterial = serde_json::from_slice(&content)
        .map_err(|_| AppError::new("CERT_DEPLOY_FILE", "已签发材料损坏，未进行部署，请重新签发"))?;
    if material.version != 1 || material.automation_id != a.id || a.deployment_id.is_empty()
        || material.deployment_id != a.deployment_id || material.domains != a.domains
        || material.ca != a.ca || material.key_alg != a.key_alg {
        return Err(AppError::new("CERT_DEPLOY_IDENTITY", "签发材料与当前任务或证书批次不一致，请重新签发"));
    }
    crate::certs::deployment_validity(&material.chain, &material.key_pem, &a.domains)?;
    Ok(material)
}

fn save_progress(state: &CoreState, a: &mut CertAutomation) -> Result<()> {
    a.updated_at = next_revision(load_automation(&state.store, &a.id)?.updated_at);
    state.store.save_cert_automation(a)
}

fn retain_issued(state: &CoreState, a: &mut CertAutomation, material: &IssuedMaterial) -> Result<()> {
    // CA 已签发；后续材料保存失败也不能当成普通签发失败自动重新下单。
    a.state = "deploying".into();
    a.deployment_id.clear();
    let (_, not_after) = crate::certs::deployment_validity(&material.chain, &material.key_pem, &a.domains)?;
    let content = serde_json::to_vec(material).map_err(|e| AppError::internal("编码签发材料", e.to_string()))?;
    crate::paths::write_atomic(&issued_path(&state.paths, &a.id)?, &content)?;
    a.deployment_id = material.deployment_id.clone();
    a.issued_at = Some(now_ms()); a.expires_at = Some(not_after); a.cert_id = None;
    a.local_deploy_result = None;
    for target in &mut a.targets { target.last_result = None; }
    a.state = "deploying".into(); a.manual_records.clear();
    save_progress(state, a)
}

fn deployment_result(outcome: Result<String>, log: &mut Vec<String>, name: &str) -> DeployResult {
    let (ok, message) = match outcome { Ok(msg) => (true, msg), Err(error) => (false, error.to_string()) };
    log.push(format!("[{name}] {}：{message}", if ok { "完成" } else { "失败" }));
    DeployResult { ok, message, at: now_ms() }
}

fn deploy_pending(state: &CoreState, a: &mut CertAutomation, material: &IssuedMaterial, log: &mut Vec<String>) -> Result<()> {
    let (before, after) = crate::certs::deployment_validity(&material.chain, &material.key_pem, &a.domains)?;
    a.expires_at = Some(after);
    if a.deploy_local && !a.local_deploy_result.as_ref().is_some_and(|r| r.ok) {
        let result = deploy_local(state, a, &material.chain, &material.key_pem, before, after)
            .map(|(record, message)| {
                a.cert_id = Some(record.id);
                message
            });
        a.local_deploy_result = Some(deployment_result(result, log, "本地站点"));
        save_progress(state, a)?;
    }
    let mut uncertain = None;
    for index in 0..a.targets.len() {
        if a.targets[index].last_result.as_ref().is_some_and(|r| r.ok) {
            log.push(format!("[{}] 本批证书已部署成功，跳过", a.targets[index].name));
            continue;
        }
        let target = &a.targets[index];
        let outcome = certdeploy::deploy(target, &a.domains, &material.chain, &material.key_pem);
        if let Err(error) = &outcome {
            if error.code == "DEPLOY_UNCERTAIN" { uncertain = Some(error.clone()); }
        }
        let result = deployment_result(outcome, log, &target.name);
        a.targets[index].last_result = Some(result);
        save_progress(state, a)?;
    }
    if let Some(error) = uncertain { return Err(error); }
    let failed = usize::from(a.deploy_local && !a.local_deploy_result.as_ref().is_some_and(|r| r.ok))
        + a.targets.iter().filter(|t| !t.last_result.as_ref().is_some_and(|r| r.ok)).count();
    if failed > 0 {
        return Err(AppError::new("CERT_DEPLOY_FAILED", format!("证书已签发，{failed} 个部署目标未完成。请修正目标配置后重试部署，无需重新签发")));
    }
    Ok(())
}

/// 证书落到站点证书目录（与自签证书同一位置，nginx 配置无需变化）；
/// 命中已开 HTTPS 的站点时重载 web server 让新证书立刻生效。
fn deploy_local(
    state: &CoreState,
    a: &CertAutomation,
    chain: &str,
    key_pem: &str,
    not_before: i64,
    not_after: i64,
) -> Result<(CertRecord, String)> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let _operation = state.manager.lifecycle.lock();
    let _files = crate::tls::CERT_FILES.lock();
    let plan = local_site_deployment(&state.paths, &state.store, a)?;
    let primary = a.domains[0].clone();
    let outputs = &plan.outputs;
    let (crt_path, key_path, _) = outputs.first().cloned().ok_or_else(|| AppError::new("DEPLOY_PATH", "没有可用的本地证书输出位置"))?;
    std::fs::create_dir_all(state.paths.certs().join("sites"))?;

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
    // 所有匹配站点必须一起切换；任一站点写入失败则恢复本轮已经写入的文件。
    let mut previous = Vec::new();
    for (cert, key, _) in outputs {
        for path in [cert, key] {
            let content = match std::fs::read_to_string(path) {
                Ok(content) => Some(content),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(AppError::io("读取原站点证书文件", error)),
            };
            previous.push((path.clone(), content));
        }
    }
    let mut written = 0;
    let result: Result<()> = (|| {
        for (cert, key, _) in outputs {
            crate::tls::write_cert_pair(&state.paths, cert, key, chain, key_pem, || Ok(()))?;
            written += 2;
        }
        let records: Vec<_> = outputs.iter().map(|(cert, key, primary)| CertRecord {
            id: format!("acme-{primary}"), subject: primary.clone(),
            cert_path: cert.to_string_lossy().into_owned(), key_path: Some(key.to_string_lossy().into_owned()),
            ..record.clone()
        }).collect();
        state.store.replace_managed_certs(&records)
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        // write_cert_pair 已处理当前失败的一对文件，只恢复之前已提交的输出。
        for (path, content) in previous[..written].iter().rev() {
            let restored = match content {
                Some(value) => crate::paths::write_atomic(path, value.as_bytes()),
                None => match std::fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                },
            };
            if let Err(restore) = restored { failures.push(format!("{}: {restore}", path.display())); }
        }
        if failures.is_empty() { return Err(error); }
        return Err(AppError::new("CERT_ROLLBACK_FAILED", "本地站点证书写入失败，部分站点文件未能恢复")
            .with_detail(format!("{}；{}", error, failures.join("；"))));
    }

    if plan.site_count > 0 {
        // 证书文件已经落盘，但服务没有真正加载新证书时必须让调用方知道，
        // 不能继续显示“本地部署完成”或把自动化记成成功。
        crate::ops::rebuild_and_reload(&state.store, &state.paths, &state.manager).map_err(|e| {
            AppError::new("CERT_RELOAD_FAILED", "证书已写入，但 HTTPS 服务重载失败")
                .with_hint("检查服务日志和配置后手动重启 Web 服务；证书文件仍保留")
                .with_detail(e.to_string())
        })?;
    }
    let mut message = if plan.site_count == 0 {
        format!("证书已保存，未应用到本地 HTTPS 站点：{}", record.cert_path)
    } else {
        format!("本地部署完成：已同步到 {} 个本地 HTTPS 站点（{}）", plan.site_count, record.cert_path)
    };
    if !plan.skipped.is_empty() {
        message.push_str(&format!("；以下站点未自动分配，已保留原证书：{}", plan.skipped.join("、")));
    }
    Ok((record, message))
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
) -> Result<IssuedMaterial> {
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
    let (chain, key_pem, _, _) = client.issue(
        &a.domains,
        &a.key_alg,
        a.dns_wait_sec,
        &mut set_txt,
        &mut clear_txt,
    )?;

    log.push("ACME 签发成功，保存证书材料后开始部署".into());
    Ok(IssuedMaterial {
        version: 1, automation_id: a.id.clone(), deployment_id: format!("{:032x}", rand::random::<u128>()),
        domains: a.domains.clone(), ca: a.ca.clone(), key_alg: a.key_alg.clone(), chain, key_pem,
    })
}

/// 执行一次（手动「立即签发」与调度器共用）。同步阻塞，调用方负责放线程里。
/// 全程留痕：日志行进 runs 历史（certd 的执行日志），成功/失败发 webhook 通知。
pub fn run_once(state: &CoreState, id: &str) -> Result<CertAutomation> {
    let work = crate::BackgroundWork::begin(format!("证书签发（{id}）"))?;
    run_once_registered(state, id, work, false, false)
}

/// 持执行锁后再次读取排期，防止调度快照过期导致关闭后仍签发或刚续完又续。
#[cfg(test)]
fn claim_run(store: &crate::store::Store, id: &str, scheduled: bool) -> Result<CertAutomation> {
    claim_work(store, id, scheduled, false)
}

#[cfg(test)]
fn claim_work(store: &crate::store::Store, id: &str, scheduled: bool, retry: bool) -> Result<CertAutomation> {
    start_work(store, prepare_work(store, id, scheduled, retry)?, retry)
}

fn prepare_work(store: &crate::store::Store, id: &str, scheduled: bool, retry: bool) -> Result<CertAutomation> {
    let mut a = load_automation(store, id)?;
    if is_running(&a) {
        let a = recover_locked(store, a)?;
        return Err(AppError::new("CERT_AUTO_INTERRUPTED", a.last_error));
    }
    if scheduled && (!a.enabled || a.next_renew_at > now_ms()) {
        return Err(AppError::new("CERT_AUTO_NOT_DUE", "自动续签已关闭或尚未到执行时间"));
    }
    if scheduled && a.state == "deploy_interrupted" {
        return Err(AppError::new("CERT_AUTO_INTERRUPTED", "部署曾中断，请检查目标端后手动重试部署"));
    }
    if retry && a.deployment_id.is_empty() {
        return Err(AppError::new("CERT_DEPLOY_MISSING", "没有可重试的签发批次，请先签发证书"));
    }
    let checked = crate::tls::normalize_domains(&a.domains).and_then(|domains| {
        a.domains = domains;
        validate(&a)
    });
    if let Err(error) = checked {
        if scheduled { pause_before_run(store, &mut a, &error)?; }
        return Err(error);
    }
    Ok(a)
}

fn start_work(store: &crate::store::Store, mut a: CertAutomation, retry: bool) -> Result<CertAutomation> {
    a.state = if retry { "deploying" } else { "issuing" }.into();
    a.last_error = String::new();
    a.last_run_at = now_ms();
    a.manual_records.clear();
    a.updated_at = next_revision(a.updated_at);
    store.save_cert_automation(&a)?;
    Ok(a)
}

fn run_once_registered(state: &CoreState, id: &str, _work: crate::BackgroundWork, scheduled: bool, retry: bool) -> Result<CertAutomation> {
    let _activity = crate::paths::DataDirActivity::shared(&state.paths.base)?;
    let _lock = execution_lock(&state.store, id)?;
    let retry = retry || (scheduled && matches!(load_automation(&state.store, id)?.state.as_str(), "deploy_error" | "deploy_waiting"));
    let mut a = prepare_work(&state.store, id, scheduled, retry)?;
    let _resources = reserve_work(state, &mut a, scheduled, retry)?;
    let mut a = start_work(&state.store, a, retry)?;
    let mut log: Vec<String> = Vec::new();
    state.emit_event(Event::CertAuto {
        id: a.id.clone(),
        state: a.state.clone(),
        message: String::new(),
    });
    log.push(format!("开始处理：{}", a.domains.join(", ")));
    if a.key_alg != "ec256" {
        log.push(format!("证书私钥算法：{}", a.key_alg));
    }

    let outcome = (|| {
        let material = if retry {
            log.push("复用已签发证书，仅重试未确认成功的部署目标；不请求 CA 或 DNS".into());
            read_issued(state, &a)?
        } else {
            let material = run_inner(state, &a, &mut log)?;
            retain_issued(state, &mut a, &material)?;
            state.emit_event(Event::CertAuto { id: a.id.clone(), state: "deploying".into(), message: "证书已签发，正在部署".into() });
            material
        };
        deploy_pending(state, &mut a, &material, &mut log)
    })();
    a = finish_execution(state, a, &outcome, log)?;

    let title = format!("证书{}：{}", match a.state.as_str() { "ok" => "签发与部署完成", "deploy_error" | "deploy_interrupted" => "部署未完成", _ => "签发失败" }, a.domains.join(", "));
    let detail = if a.state == "ok" { format!("有效期至 {}", fmt_date(a.expires_at)) } else { a.last_error.clone() };
    // 通知以最终部署结果为准；通知失败不会改写证书状态。
    let notify_result = if a.notify_kind == "email" {
        a.notify_smtp.as_ref().map(|smtp| certdeploy::notify_email(smtp, a.state == "ok", &title, &detail))
    } else if !a.notify_kind.is_empty() && a.notify_kind != "none" && !a.notify_url.trim().is_empty() {
        Some(certdeploy::notify(&a.notify_kind, &a.notify_url, a.state == "ok", &title, &detail))
    } else { None };
    if let Some(Err(ne)) = notify_result {
        (state.emit)(Event::DownloadProgress(model::DownloadProgress {
            task_id: format!("certauto-notify-{}", a.id), received: 0, total: 0, speed_bps: 0, eta_sec: 0.0,
            state: "notify-failed".into(), error: Some(ne.to_string()),
        }));
    }
    Ok(a)
}

fn finish_execution(state: &CoreState, mut a: CertAutomation, outcome: &Result<()>, mut log: Vec<String>) -> Result<CertAutomation> {
    let run_at = now_ms();
    match outcome {
        Ok(()) => {
            a.state = "ok".into();
            a.fail_count = 0; a.last_error.clear();
            // 到期前 renew_days_ahead 天续签（certd 默认 30 天）；没拿到到期时间就 60 天后再看
            a.next_renew_at = match a.expires_at {
                Some(exp) if exp > 0 => {
                    let ahead = if a.renew_days_ahead > 0 {
                        a.renew_days_ahead
                    } else {
                        RENEW_AHEAD_DAYS
                    };
                    (exp - ahead * 86_400_000).max(run_at + 3_600_000)
                }
                _ => run_at + 60 * 86_400_000,
            };
            log.push(format!("完成，证书有效期至 {}", fmt_date(a.expires_at)));
        }
        Err(e) => {
            a.state = if a.state == "deploying" { "deploy_error" } else { "error" }.into();
            a.last_error = e.to_string();
            a.fail_count = a.fail_count.saturating_add(1);
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
            // 材料缺失、过期或状态持久化失败时暂停，避免静默重新申请或重放不确定的操作。
            if a.state == "deploy_error" && e.code != "CERT_DEPLOY_FAILED" {
                a.state = "deploy_interrupted".into(); a.enabled = false; a.next_renew_at = i64::MAX / 2;
                a.last_error = format!("{}。自动续签已暂停，请检查后重试部署或重新签发", a.last_error);
            }
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
                Ok(_) => "签发与部署完成".into(),
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
    save_progress(state, &mut a)?;

    let message = if a.state != "ok" {
        a.last_error.clone()
    } else {
        "ok".into()
    };
    state.emit_event(Event::CertAuto {
        id: a.id.clone(),
        state: a.state.clone(),
        message: message.clone(),
    });

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
        if !a.enabled || is_running(&a) || a.state == "deploy_interrupted" {
            continue;
        }
        // 首次保存 nextRenewAt=now → 立即进入调度；此后按「到期前 N 天」节奏
        if a.next_renew_at <= now {
            // 手动模式的续期需要人加记录：调度只负责唤醒提示（发事件），
            // 不在这里占着调度线程等 1 小时 —— 用户在界面上点「立即续签」
            if a.dns.kind == "manual" && a.issued_at.is_some() && !matches!(a.state.as_str(), "deploy_error" | "deploy_waiting") {
                let _ = a_emit_manual_due(state, &a.id);
                continue;
            }
            // 每个任务独立线程：一个任务卡住（网络慢 / 手动等待）不拖累其它
            let st = state.clone();
            let id = a.id.clone();
            // 先注册再派生线程，退出准备不会漏掉已接受、尚未开始的签发。
            let Ok(work) = crate::BackgroundWork::begin(format!("证书签发（{id}）")) else { break; };
            if std::thread::Builder::new().name("certificate-issue".into()).spawn(move || {
                let _ = run_once_registered(&st, &id, work, true, false);
            }).is_ok() { processed.push(a.id); }
        }
    }
    processed
}

/// 手动模式到期提醒：状态回 idle + 发事件，等用户来点
fn a_emit_manual_due(state: &CoreState, id: &str) -> Result<()> {
    let _lock = execution_lock(&state.store, id)?;
    if let Some(mut a) = state.store.get_cert_automation(id)? {
        if !a.enabled || is_running(&a) || a.next_renew_at > now_ms() || a.dns.kind != "manual" || a.issued_at.is_none() || a.state.starts_with("deploy_") { return Ok(()); }
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
/// 此后每分钟检查到期/等待任务；证书监控仍每小时执行。
pub fn spawn_scheduler(state: Arc<CoreState>) {
    spawn_scheduler_when_ready(state,None);
}

pub fn spawn_scheduler_when_ready(state: Arc<CoreState>, gate: Option<std::sync::Arc<crate::restart::StartupGate>>) {
    std::thread::spawn(move || {
        if gate.is_some_and(|gate| !gate.wait()) { return; }
        std::thread::sleep(std::time::Duration::from_secs(30));
        let _ = tick(&state);
        crate::certmonitor::tick_all(&state);
        let mut last_monitor = std::time::Instant::now();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            let _ = tick(&state);
            if last_monitor.elapsed() >= std::time::Duration::from_secs(3600) {
                crate::certmonitor::tick_all(&state); last_monitor = std::time::Instant::now();
            }
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
            a.deployment_id.clear(); a.local_deploy_result = None;
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
            let local_changed = a.deploy_local != existing.deploy_local;
            // 客户端只能修改配置，执行结果、排期、历史和自动续签开关由各自入口维护。
            a.state = existing.state; a.last_error = existing.last_error; a.cert_id = existing.cert_id;
            a.issued_at = existing.issued_at; a.expires_at = existing.expires_at;
            a.deployment_id = existing.deployment_id;
            a.local_deploy_result = if local_changed { None } else { existing.local_deploy_result };
            if local_changed { a.cert_id = None; }
            a.last_run_at = existing.last_run_at; a.fail_count = existing.fail_count;
            a.runs = existing.runs; a.manual_records = existing.manual_records;
            a.enabled = existing.enabled; a.created_at = existing.created_at;
            a.next_renew_at = if a.enabled && a.state == "ok" && a.renew_days_ahead != existing.renew_days_ahead {
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
                a.deployment_id.clear(); a.local_deploy_result = None;
                a.next_renew_at = if a.enabled { now_ms() } else { i64::MAX / 2 };
                for target in &mut a.targets { target.last_result = None; }
            } else if !a.deployment_id.is_empty() && a.state != "deploy_interrupted"
                && ((a.deploy_local && a.local_deploy_result.is_none()) || a.targets.iter().any(|t| t.last_result.is_none())) {
                a.state = "deploy_error".into();
                a.last_error = "部署配置已更新，请重试部署以应用已签发证书".into();
                a.next_renew_at = if a.enabled { now_ms() } else { i64::MAX / 2 };
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
        // 删除自动化专用的账号/重试材料；已部署的站点证书仍保留。
        let path = account_key_path(&self.paths, id);
        let previous = read_account_key(&path)?;
        let issued = issued_path(&self.paths, id)?;
        let issued_previous = read_issued_file(&issued)?;
        let files = [(path, previous.map(String::into_bytes)), (issued, issued_previous)];
        let mut removed = Vec::new();
        let result = (|| {
            for (path, content) in &files {
                if content.is_some() { std::fs::remove_file(path)?; removed.push((path, content.as_ref().unwrap())); }
            }
            self.store.delete_cert_automation(id)
        })();
        if let Err(error) = result {
            let mut failures = Vec::new();
            for (path, content) in removed.into_iter().rev() {
                if let Err(restore) = crate::paths::write_atomic(path, content) { failures.push(restore.to_string()); }
            }
            if !failures.is_empty() { return Err(AppError::new("CERT_AUTO_DELETE_FAILED", format!("删除失败，部分签发材料未能恢复：{error}；{}", failures.join("；")))); }
            return Err(error);
        }
        Ok(true)
    }

    pub fn certauto_set_enabled(&self, id: &str, enabled: bool) -> Result<CertAutomation> {
        let _work = crate::BackgroundWork::begin("切换证书自动续签")?;
        let _activity = crate::paths::DataDirActivity::shared(&self.paths.base)?;
        let _lock = execution_lock(&self.store, id)?;
        let mut a = recover_locked(&self.store, load_automation(&self.store, id)?)?;
        if enabled {
            validate(&a)?;
            if a.state == "deploy_interrupted" { return Err(AppError::new("CERT_AUTO_INTERRUPTED", "请先核对目标端并手动重试部署，完成后再启用自动续签")); }
            if matches!(a.state.as_str(), "deploy_error" | "deploy_waiting") && a.deployment_id.is_empty() { return Err(AppError::new("CERT_DEPLOY_MISSING", "旧记录缺少签发材料，请先重新签发，完成后再启用自动续签")); }
        }
        if a.enabled == enabled { return Ok(a); }
        a.enabled = enabled;
        if !enabled && matches!(a.state.as_str(), "waiting" | "deploy_waiting") {
            a.state = if a.state == "deploy_waiting" { "deploy_error" } else { "idle" }.into();
            a.last_error = if a.state == "deploy_error" { "已取消等待；已签发证书仍可手动重试部署".into() } else { String::new() };
        }
        a.updated_at = next_revision(a.updated_at);
        // 已部署成功的证书沿用真实有效期，重新开启不会立即重复申请。
        a.next_renew_at = if enabled {
            if a.state == "ok" { a.expires_at.filter(|exp| *exp > 0).map(|exp| (exp - a.renew_days_ahead * 86_400_000).max(now_ms())).unwrap_or_else(now_ms) }
            else { now_ms() }
        } else { i64::MAX / 2 };
        self.store.save_cert_automation(&a)?;
        Ok(a)
    }

    pub fn certauto_issue(&self, id: &str) -> Result<CertAutomation> {
        run_once(self, id)
    }

    pub fn certauto_retry_deploy(&self, id: &str) -> Result<CertAutomation> {
        let work = crate::BackgroundWork::begin(format!("证书部署（{id}）"))?;
        run_once_registered(self, id, work, false, true)
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

    fn material(a: &CertAutomation, days: i64) -> IssuedMaterial {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(a.domains.clone()).unwrap();
        params.not_before = OffsetDateTime::now_utc() - time::Duration::days(1);
        params.not_after = OffsetDateTime::now_utc() + time::Duration::days(days);
        let cert = params.self_signed(&key).unwrap();
        IssuedMaterial {
            version: 1, automation_id: a.id.clone(), deployment_id: "fixture-batch".into(),
            domains: a.domains.clone(), ca: a.ca.clone(), key_alg: a.key_alg.clone(), chain: cert.pem(), key_pem: key.serialize_pem(),
        }
    }

    fn local_target(root: &std::path::Path, name: &str) -> model::DeployTarget {
        model::DeployTarget { id: name.into(), name: name.into(), kind: "local".into(), last_result: None,
            config: [("certPath".into(), root.join(format!("{name}.crt")).to_string_lossy().into_owned()),
                ("keyPath".into(), root.join(format!("{name}.key")).to_string_lossy().into_owned())].into() }
    }

    fn https_site(
        id: &str,
        name: &str,
        domains: &[&str],
        imported_cert_id: Option<&str>,
        root: &std::path::Path,
    ) -> model::Site {
        model::Site {
            access_url: None,
            id: id.into(), name: name.into(), domains: domains.iter().map(|v| (*v).into()).collect(),
            root_dir: root.to_string_lossy().into(),
            runtime: model::SiteRuntime {
                acme_cert_id: None, imported_cert_id: imported_cert_id.map(str::to_owned), web_server: "nginx".into(),
                kind: model::SiteKind::Static, php_version: None, proxy_target: None,
                command: None, cwd: None,
            },
            https: true, rewrite: model::RewritePreset::None, db: None, status: "running".into(),
            php_overrides: None, created_at: 1, updated_at: 1,
        }
    }

    #[test]
    fn acme_selected_certificate_roundtrips_renders_and_renews_without_a_local_ca() {
        let (dir, state, mut a) = fixture(); a.targets.clear();
        a.domains = vec!["*.example.com".into(), "example.com".into()];
        let first = material(&a, 60);
        let (record, _) = deploy_local(&state, &a, &first.chain, &first.key_pem, 1, 2).unwrap();
        let mut site = https_site("selected", "ACME 站点", &["www.example.com", "api.example.com"], None, dir.path());
        site.runtime.acme_cert_id = Some(record.id.clone());
        state.store.save_site(&site).unwrap();
        let restored: model::Site = serde_json::from_str(&serde_json::to_string(&site).unwrap()).unwrap();
        assert_eq!(restored.runtime.acme_cert_id, site.runtime.acme_cert_id);
        assert_eq!(state.store.list_sites().unwrap()[0].runtime.acme_cert_id, site.runtime.acme_cert_id);
        crate::certs::validate_site_certificate(&state.paths, &state.store, &site).unwrap();
        for server in ["nginx", "apache"] {
            site.runtime.web_server = server.into();
            crate::sites::write_site_conf(&state.paths, &state.store, &site).unwrap();
            let config = std::fs::read_to_string(state.paths.etc().join(server).join("sites/selected.conf")).unwrap();
            assert!(config.contains("_wildcard.example.com.crt"));
            assert!(config.contains("_wildcard.example.com.key"));
            assert!(!config.contains("www.example.com.crt"));
        }
        assert!(!state.paths.certs().join("ca.crt").exists());
        let choices = crate::certs::site_certificate_choices(&state.paths, &state.store).unwrap();
        assert_eq!(choices.len(), 1); assert!(choices[0].usable);
        assert_eq!(choices[0].used_by_sites, [site.name.clone()]);
        assert!(state.repair_site_certificates().unwrap().is_empty());
        assert!(!state.paths.certs().join("ca.crt").exists());
        let second = material(&a, 90);
        let (_, message) = deploy_local(&state, &a, &second.chain, &second.key_pem, 3, 4).unwrap();
        assert!(message.contains("1 个本地 HTTPS 站点"));
        assert_eq!(std::fs::read_to_string(&record.cert_path).unwrap(), second.chain);
        assert!(!state.paths.certs().join("sites/www.example.com.crt").exists());
        let report = crate::certs::report(&state.paths, &state.store).unwrap();
        assert_eq!(report.certs.iter().find(|c| c.id == record.id).unwrap().used_by_sites, [site.name]);
        crate::sites::delete(&site.id, false, true, &state.paths, &state.store, &state.manager).unwrap();
        assert!(std::path::Path::new(&record.cert_path).is_file());
        assert!(state.store.list_certs().unwrap().iter().any(|c| c.id == record.id));
    }

    #[test]
    fn acme_selected_certificate_rejects_partial_wildcard_coverage_invalid_ids_and_double_binding() {
        let (dir, state, mut a) = fixture(); a.targets.clear();
        a.domains = vec!["*.example.com".into(), "example.com".into()];
        let signed = material(&a, 60);
        let (record, _) = deploy_local(&state, &a, &signed.chain, &signed.key_pem, 1, 2).unwrap();
        let mut site = https_site("selected", "Selected", &["www.example.com", "two.level.example.com"], None, dir.path());
        site.runtime.acme_cert_id = Some(record.id.clone());
        assert_eq!(crate::certs::validate_site_certificate(&state.paths, &state.store, &site).unwrap_err().code, "CERT_DOMAIN_MISMATCH");
        site.domains = vec!["example.com".into(), "www.example.com".into()];
        crate::certs::validate_site_certificate(&state.paths, &state.store, &site).unwrap();
        for id in ["acme-../escape", "acme-Example.com", "acme-example.com/../../evil", "cert-example.com", "acme-"] {
            site.runtime.acme_cert_id = Some(id.into());
            assert_eq!(crate::certs::validate_site_certificate(&state.paths, &state.store, &site).unwrap_err().code, "BAD_CERT_ID");
        }
        site.runtime.acme_cert_id = Some("acme-missing.example.com".into());
        assert_eq!(crate::certs::validate_site_certificate(&state.paths, &state.store, &site).unwrap_err().code, "ACME_CERT_MISSING");
        site.runtime.acme_cert_id = Some(record.id);
        site.runtime.imported_cert_id = Some("imported".into()); site.https = false;
        assert_eq!(crate::certs::validate_site_certificate(&state.paths, &state.store, &site).unwrap_err().code, "BAD_CERT_ID");
    }

    #[test]
    fn acme_selected_certificate_uses_real_files_and_keeps_unusable_choices_visible() {
        let (dir, state, mut a) = fixture(); a.targets.clear();
        a.domains = vec!["example.com".into(), "www.example.com".into()];
        let fresh = material(&a, 60);
        let (record, _) = deploy_local(&state, &a, &fresh.chain, &fresh.key_pem, 1, 2).unwrap();
        let mut site = https_site("selected", "Selected", &["www.example.com"], None, dir.path());
        site.runtime.acme_cert_id = Some(record.id.clone()); state.store.save_site(&site).unwrap();
        let key = record.key_path.as_ref().unwrap();
        let expired = material(&a, -1);
        for failure in ["expired", "wrong-key", "missing-key", "missing-cert", "bad-record-path"] {
            std::fs::write(&record.cert_path, &fresh.chain).unwrap();
            std::fs::write(key, &fresh.key_pem).unwrap();
            state.store.save_cert(&record).unwrap();
            match failure {
                "expired" => { std::fs::write(&record.cert_path, &expired.chain).unwrap(); std::fs::write(key, &expired.key_pem).unwrap(); }
                "wrong-key" => std::fs::write(key, &expired.key_pem).unwrap(),
                "missing-key" => std::fs::remove_file(key).unwrap(),
                "missing-cert" => std::fs::remove_file(&record.cert_path).unwrap(),
                _ => { let mut wrong = record.clone(); wrong.cert_path = dir.path().join("outside.crt").to_string_lossy().into_owned(); state.store.save_cert(&wrong).unwrap(); }
            }
            let before = std::fs::read(&record.cert_path).ok();
            let choices = crate::certs::site_certificate_choices(&state.paths, &state.store).unwrap();
            assert_eq!(choices.len(), 1, "{failure}"); assert!(!choices[0].usable, "{failure}");
            assert!(choices[0].problem.is_some(), "{failure}");
            assert_eq!(crate::certs::validate_site_certificate(&state.paths, &state.store, &site).unwrap_err().code, "CERT_UNUSABLE");
            assert_eq!(state.repair_site_certificates().unwrap_err().code, "ACME_REPAIR_REQUIRED");
            assert_eq!(std::fs::read(&record.cert_path).ok(), before);
            assert!(!state.paths.certs().join("ca.crt").exists());
        }
    }

    #[test]
    fn acme_explicit_selection_protects_output_from_other_automations_and_self_signing() {
        let (dir, state, mut a) = fixture(); a.targets.clear();
        a.domains = vec!["example.com".into(), "*.example.com".into()];
        let signed = material(&a, 60);
        let (record, _) = deploy_local(&state, &a, &signed.chain, &signed.key_pem, 1, 2).unwrap();
        let mut selected = https_site("selected", "Selected", &["www.example.com"], None, dir.path());
        selected.runtime.acme_cert_id = Some(record.id.clone()); state.store.save_site(&selected).unwrap();
        let default = https_site("default", "Default", &["example.com"], None, dir.path()); state.store.save_site(&default).unwrap();
        assert_eq!(state.issue_certificate("example.com", &[]).unwrap_err().code, "CERT_IN_USE");
        let mut other = a.clone(); other.domains = vec!["other.example.com".into(), "*.example.com".into(), "example.com".into()];
        let plan = local_site_deployment(&state.paths, &state.store, &other).unwrap();
        assert_eq!(plan.outputs.len(), 1); assert_eq!(plan.site_count, 0); assert_eq!(plan.skipped.len(), 1);
        let other_material = material(&other, 60);
        deploy_local(&state, &other, &other_material.chain, &other_material.key_pem, 1, 2).unwrap();
        assert_eq!(std::fs::read_to_string(&record.cert_path).unwrap(), signed.chain);
        // 移除已绑定站点所需 SAN 时，在签发准备和部署阶段都拒绝；关闭 HTTPS 也保留选择保护。
        selected.https = false; state.store.save_site(&selected).unwrap(); a.domains = vec!["example.com".into()];
        assert_eq!(resource_locks_for_state(&state, &a, false).unwrap_err().code, "CERT_SITE_COVERAGE");
        assert_eq!(local_site_deployment(&state.paths, &state.store, &a).err().unwrap().code, "CERT_SITE_COVERAGE");
        assert_eq!(state.issue_certificate("example.com", &[]).unwrap_err().code, "CERT_IN_USE");
    }

    #[test]
    fn acme_missing_binding_in_config_import_fails_before_any_import_writes() {
        let (dir, state, _a) = fixture();
        let mut site = https_site("selected", "Selected", &["www.example.com"], None, dir.path());
        site.runtime.acme_cert_id = Some("acme-example.com".into());
        let bundle = crate::transfer::ExportBundle { format: "niceservbay/1".into(), sites: vec![site],
            settings: vec![("certificate-import-sentinel".into(), "changed".into())], ..Default::default() };
        let file = dir.path().join("fixture-backup.json");
        std::fs::write(&file, serde_json::to_vec(&bundle).unwrap()).unwrap();
        assert_eq!(crate::transfer::import_from(&file, &state.paths, &state.store, &state.manager).unwrap_err().code, "ACME_CERT_MISSING");
        assert!(state.store.list_sites().unwrap().is_empty());
        assert!(state.store.get_setting("certificate-import-sentinel").is_none());
    }

    #[test]
    fn local_deploy_updates_each_san_site_and_keeps_imported_certificates() {
        let (dir, state, mut a) = fixture();
        a.domains = vec!["example.com".into(), "www.example.com".into(), "imported.example.com".into()];
        a.targets.clear();
        state.store.save_cert_automation(&a).unwrap();
        state.store.save_site(&https_site("www", "WWW 站点", &["www.example.com"], None, dir.path())).unwrap();
        state.store.save_site(&https_site("imported", "导入站点", &["imported.example.com"], Some("cert-imported"), dir.path())).unwrap();

        let outputs = local_site_deployment(&state.paths, &state.store, &a).unwrap().outputs;
        assert_eq!(outputs.len(), 2, "主域名和 SAN 主域名站点各有一组输出文件");
        assert!(outputs.iter().any(|(_, _, primary)| primary == "www.example.com"));
        assert!(!outputs.iter().any(|(_, _, primary)| primary == "imported.example.com"));

        let imported_cert = state.paths.certs().join("sites/imported.example.com.crt");
        let imported_key = state.paths.certs().join("sites/imported.example.com.key");
        std::fs::create_dir_all(imported_cert.parent().unwrap()).unwrap();
        std::fs::write(&imported_cert, "imported-certificate").unwrap();
        std::fs::write(&imported_key, "imported-key").unwrap();

        let material = material(&a, 30);
        deploy_local(&state, &a, &material.chain, &material.key_pem, 10, 20).unwrap();
        let primary_cert = state.paths.certs().join("sites/example.com.crt");
        let primary_key = state.paths.certs().join("sites/example.com.key");
        let www_cert = state.paths.certs().join("sites/www.example.com.crt");
        let www_key = state.paths.certs().join("sites/www.example.com.key");
        assert_eq!(std::fs::read_to_string(primary_cert).unwrap(), material.chain);
        assert_eq!(std::fs::read_to_string(primary_key).unwrap(), material.key_pem);
        assert_eq!(std::fs::read_to_string(www_cert).unwrap(), material.chain);
        assert_eq!(std::fs::read_to_string(www_key).unwrap(), material.key_pem);
        assert_eq!(std::fs::read_to_string(imported_cert).unwrap(), "imported-certificate");
        assert_eq!(std::fs::read_to_string(imported_key).unwrap(), "imported-key");
    }

    #[test]
    fn local_deploy_rolls_back_every_site_when_record_commit_fails() {
        let (dir, state, mut a) = fixture();
        a.domains = vec!["example.com".into(), "www.example.com".into()];
        a.targets.clear();
        state.store.save_cert_automation(&a).unwrap();
        state.store.save_site(&https_site("www", "WWW 站点", &["www.example.com"], None, dir.path())).unwrap();
        let paths = [
            state.paths.certs().join("sites/example.com.crt"),
            state.paths.certs().join("sites/example.com.key"),
            state.paths.certs().join("sites/www.example.com.crt"),
            state.paths.certs().join("sites/www.example.com.key"),
        ];
        std::fs::create_dir_all(paths[0].parent().unwrap()).unwrap();
        for (path, old) in paths.iter().zip(["old-primary-cert", "old-primary-key", "old-www-cert", "old-www-key"]) {
            std::fs::write(path, old).unwrap();
        }
        for (offset, domain) in [(0, "example.com"), (2, "www.example.com")] {
            state.store.save_cert(&CertRecord { id: format!("cert-{domain}"), kind: "site".into(),
                subject: domain.into(), sans: vec![domain.into()], not_before: 1, not_after: 2,
                cert_path: paths[offset].to_string_lossy().into_owned(),
                key_path: Some(paths[offset + 1].to_string_lossy().into_owned()), trusted: None }).unwrap();
        }
        let db = rusqlite::Connection::open(state.paths.db()).unwrap();
        db.execute_batch("CREATE TRIGGER reject_fixture_cert BEFORE INSERT ON certs WHEN NEW.kind='acme' AND NEW.subject='www.example.com' BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        let material = material(&a, 30);
        let error = deploy_local(&state, &a, &material.chain, &material.key_pem, 10, 20).unwrap_err();
        assert_eq!(error.code, "INTERNAL");
        for (path, old) in paths.iter().zip(["old-primary-cert", "old-primary-key", "old-www-cert", "old-www-key"]) {
            assert_eq!(std::fs::read_to_string(path).unwrap(), old);
        }
        let records = state.store.list_certs().unwrap();
        assert_eq!(records.len(), 2);
        assert!(records.iter().all(|c| c.kind == "site" && c.not_after == 2), "第二条记录失败必须回滚前一条以及删除的旧记录");
        db.execute_batch("DROP TRIGGER reject_fixture_cert;").unwrap();
    }

    #[test]
    fn local_deploy_preserves_partially_covered_sites_and_rejects_primary_conflicts() {
        let (dir, state, mut a) = fixture(); a.targets.clear();
        a.domains = vec!["example.com".into(), "www.example.com".into()];
        let mut site = https_site("multi", "多域名站点", &["www.example.com", "uncovered.example.net"], None, dir.path());
        state.store.save_site(&site).unwrap();
        let old = crate::tls::issue_site_cert(&state.paths, &state.store, &site.domains).unwrap();
        let original = std::fs::read(&old.cert_path).unwrap();
        let material = material(&a, 60);
        deploy_pending(&state, &mut a, &material, &mut Vec::new()).unwrap();
        assert_eq!(std::fs::read(&old.cert_path).unwrap(), original, "部分覆盖不能替换整个站点的证书");
        assert!(a.local_deploy_result.as_ref().unwrap().message.contains("未应用到本地 HTTPS 站点"));
        assert!(a.local_deploy_result.as_ref().unwrap().message.contains("多域名站点"));
        site.domains[0] = "example.com".into(); state.store.save_site(&site).unwrap();
        a.local_deploy_result = None;
        let path = state.paths.certs().join("sites/example.com.crt");
        let before = std::fs::read(&path).unwrap();
        assert_eq!(resource_locks_for_state(&state, &a, false).unwrap_err().code, "CERT_SITE_COVERAGE");
        assert!(deploy_local(&state, &a, &material.chain, &material.key_pem, 10, 20).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn acme_deployment_records_each_output_and_local_repair_preserves_it() {
        let (dir, state, mut a) = fixture(); a.targets.clear();
        a.domains = vec!["example.com".into(), "*.example.com".into()];
        let site = https_site("www", "WWW", &["www.example.com", "api.example.com"], None, dir.path());
        state.store.save_site(&site).unwrap();
        crate::tls::issue_site_cert(&state.paths, &state.store, &site.domains).unwrap();
        let material = material(&a, 60);
        deploy_pending(&state, &mut a, &material, &mut Vec::new()).unwrap();
        assert!(a.local_deploy_result.as_ref().unwrap().message.contains("1 个本地 HTTPS 站点"));
        let records = state.store.list_certs().unwrap();
        assert_eq!(records.iter().filter(|c| c.kind == "acme").count(), 2);
        assert!(!records.iter().any(|c| c.id == "cert-www.example.com"));
        let choices = crate::certs::site_certificate_choices(&state.paths, &state.store).unwrap();
        assert_eq!(choices.len(), 1, "只列主证书，不把站点同步副本作为可续签的选择");
        assert_eq!(choices[0].id, "acme-example.com");
        assert!(state.repair_site_certificates().unwrap().is_empty());
        let cert = state.paths.certs().join("sites/www.example.com.crt");
        assert_eq!(std::fs::read_to_string(&cert).unwrap(), material.chain);
        let key = state.paths.certs().join("sites/www.example.com.key");
        std::fs::remove_file(&key).unwrap();
        assert_eq!(state.repair_site_certificates().unwrap_err().code, "ACME_REPAIR_REQUIRED");
        assert!(!key.exists());
        assert_eq!(std::fs::read_to_string(cert).unwrap(), material.chain);
    }

    #[test]
    fn repair_recovers_legacy_san_metadata_and_preserves_expired_acme_files() {
        let (dir, state, mut a) = fixture(); a.targets.clear();
        a.domains = vec!["example.com".into(), "www.example.com".into()];
        let site = https_site("www", "WWW", &["www.example.com"], None, dir.path());
        state.store.save_site(&site).unwrap();
        let old = crate::tls::issue_site_cert(&state.paths, &state.store, &site.domains).unwrap();
        let fresh = material(&a, 60);
        deploy_pending(&state, &mut a, &fresh, &mut Vec::new()).unwrap();
        // 重现 v0.2.39：副本已写入，但记录仍为之前的本地自签元数据。
        state.store.delete_cert("acme-www.example.com").unwrap();
        state.store.save_cert(&old).unwrap();
        assert!(state.repair_site_certificates().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&old.cert_path).unwrap(), fresh.chain);
        let records = state.store.list_certs().unwrap();
        assert!(records.iter().any(|c| c.id == "acme-www.example.com"));
        assert!(!records.iter().any(|c| c.id == old.id));
        let expired = material(&a, -1);
        std::fs::write(&old.cert_path, &expired.chain).unwrap();
        std::fs::write(old.key_path.as_ref().unwrap(), &expired.key_pem).unwrap();
        assert_eq!(state.repair_site_certificates().unwrap_err().code, "ACME_REPAIR_REQUIRED");
        assert_eq!(std::fs::read_to_string(&old.cert_path).unwrap(), expired.chain);
        // 用户明确手动改回本地 CA 后，过时的 ACME 元数据应一起被替换。
        crate::tls::issue_site_cert(&state.paths, &state.store, &site.domains).unwrap();
        assert!(state.repair_site_certificates().unwrap().is_empty());
        assert!(!state.store.list_certs().unwrap().iter().any(|c| c.id == "acme-www.example.com"));
    }

    #[cfg(windows)]
    #[test]
    fn second_site_write_failure_restores_completed_outputs() {
        use std::os::windows::fs::OpenOptionsExt;
        let (dir, state, mut a) = fixture(); a.targets.clear();
        a.domains = vec!["example.com".into(), "www.example.com".into()];
        let site = https_site("www", "WWW", &["www.example.com"], None, dir.path());
        state.store.save_site(&site).unwrap();
        let first = crate::tls::issue_site_cert(&state.paths, &state.store, &["example.com".into()]).unwrap();
        let second = crate::tls::issue_site_cert(&state.paths, &state.store, &site.domains).unwrap();
        let outputs = [&first.cert_path, first.key_path.as_ref().unwrap(), &second.cert_path, second.key_path.as_ref().unwrap()];
        let before: Vec<_> = outputs.iter().map(|p| std::fs::read(p).unwrap()).collect();
        // 允许快照读取，但禁止第二个站点私钥被替换。
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(second.key_path.as_ref().unwrap()).unwrap();
        let fresh = material(&a, 60);
        assert!(deploy_local(&state, &a, &fresh.chain, &fresh.key_pem, 10, 20).is_err());
        for (path, bytes) in outputs.iter().zip(before) { assert_eq!(std::fs::read(path).unwrap(), bytes); }
        assert!(state.store.list_certs().unwrap().iter().all(|c| c.kind != "acme"));
        drop(held);
    }

    #[test]
    fn state_resource_locks_include_all_matching_site_outputs() {
        let (dir, state, mut a) = fixture();
        a.domains = vec!["example.com".into(), "www.example.com".into()];
        a.targets.clear();
        state.store.save_cert_automation(&a).unwrap();
        state.store.save_site(&https_site("www", "WWW 站点", &["www.example.com"], None, dir.path())).unwrap();
        let held = resource_locks_for_state(&state, &a, false).unwrap();
        let mut b = a.clone();
        b.id = "second".into();
        b.domains = vec!["other.example.com".into()]; b.deploy_local = false;
        b.targets = vec![local_target(&state.paths.certs().join("sites"), "www.example.com")];
        assert_eq!(resource_locks_for_state(&state, &b, true).unwrap_err().code, "CERT_AUTO_RESOURCE_BUSY");
        drop(held);
        assert!(resource_locks_for_state(&state, &b, true).is_ok());
    }

    #[test]
    fn shared_dns_resources_cover_wildcards_aliases_and_release_partial_claims() {
        let (_dir, state, mut a) = fixture(); a.deploy_local = false; a.targets.clear();
        a.domains = vec!["EXAMPLE.COM.".into(), "*.example.com".into()];
        assert_eq!(work_resources(&state.paths, &a, false).unwrap().len(), 1);
        let held = resource_locks(&state.paths, &a, false).unwrap();
        let mut b = a.clone(); b.id = "second".into(); b.domains = vec!["example.com".into()];
        assert_eq!(resource_locks(&state.paths, &b, false).unwrap_err().code, "CERT_AUTO_RESOURCE_BUSY");
        b.domains = vec!["other.example.com".into()]; assert!(resource_locks(&state.paths, &b, false).is_ok());
        drop(held);
        a.cname_target = "Shared.Validation.Example.".into(); b.cname_target = "shared.validation.example".into();
        let held = resource_locks(&state.paths, &a, false).unwrap();
        assert_eq!(resource_locks(&state.paths, &b, false).unwrap_err().code, "CERT_AUTO_RESOURCE_BUSY"); drop(held);
        a.cname_target.clear(); a.domains = vec!["z.example.com".into()];
        let held = resource_locks(&state.paths, &a, false).unwrap();
        b.cname_target.clear(); b.domains = vec!["a.example.com".into(), "z.example.com".into()];
        assert!(resource_locks(&state.paths, &b, false).is_err());
        b.domains.pop(); assert!(resource_locks(&state.paths, &b, false).is_ok(), "部分取得的锁必须释放");
        drop(held); assert!(resource_locks(&state.paths, &a, false).is_ok());
    }

    #[test]
    fn declared_output_conflicts_cover_site_files_and_ssh_aliases() {
        let (_dir, state, mut a) = fixture(); a.targets.clear();
        let held = resource_locks(&state.paths, &a, false).unwrap();
        let mut b = a.clone(); b.id = "second".into(); b.domains = vec!["other.example.com".into()]; b.deploy_local = false;
        b.targets = vec![local_target(&state.paths.certs().join("sites"), "a.com")];
        assert_eq!(resource_locks(&state.paths, &b, false).unwrap_err().code, "CERT_AUTO_RESOURCE_BUSY");
        if cfg!(windows) {
            for value in b.targets[0].config.values_mut() { *value = value.replace('\\', "/").to_uppercase(); }
            assert!(resource_locks(&state.paths, &b, false).is_err());
        }
        b.targets[0].last_result = Some(DeployResult { ok: true, message: "done".into(), at: 1 });
        assert!(resource_locks(&state.paths, &b, true).is_ok(), "部署重试跳过已成功目标和 DNS 验证资源"); drop(held);
        let target = model::DeployTarget { id: "ssh".into(), name: "ssh".into(), kind: "ssh".into(), last_result: None,
            config: [("host", "EXAMPLE.COM."), ("port", "22"), ("hostFingerprint", "SHA256:fixture"),
                ("certPath", "/ssl/cert.pem"), ("keyPath", "/ssl/key.pem")].map(|(k,v)| (k.into(),v.into())).into() };
        a.deploy_local = false; a.targets = vec![target.clone()];
        b.targets = vec![target]; b.targets[0].config.insert("host".into(), "alias.example.com".into());
        let held = resource_locks(&state.paths, &a, true).unwrap();
        assert!(resource_locks(&state.paths, &b, true).is_err(), "同一主机指纹的别名必须互斥");
        b.targets[0].config.insert("host".into(), "example.com".into());
        b.targets[0].config.insert("hostFingerprint".into(), "SHA256:changed".into());
        assert!(resource_locks(&state.paths, &b, true).is_err(), "同一地址更换指纹仍占用同一路径");
        b.targets[0].config.insert("certPath".into(), "/other/cert.pem".into());
        b.targets[0].config.insert("keyPath".into(), "/other/key.pem".into());
        assert!(resource_locks(&state.paths, &b, true).is_ok()); drop(held);
        a.targets = sample().targets; b.targets = a.targets.clone();
        b.targets[0].config.insert("url".into(), "http://X:80/".into());
        b.targets[0].config.insert("apiSk".into(), "different-credential".into());
        let held = resource_locks(&state.paths, &a, true).unwrap();
        assert!(resource_locks(&state.paths, &b, true).is_err(), "凭据变化不能绕过同一面板站点的资源锁"); drop(held);
    }

    #[test]
    fn waiting_work_preserves_history_and_can_be_edited_disabled_or_deleted() {
        let (_dir, state, mut a) = fixture(); a.deploy_local = false; a.targets.clear();
        state.store.save_cert_automation(&a).unwrap(); let held = resource_locks(&state.paths, &a, false).unwrap();
        let before = serde_json::to_value(&a).unwrap();
        assert_eq!(state.certauto_issue(&a.id).unwrap_err().code, "CERT_AUTO_RESOURCE_BUSY");
        assert_eq!(serde_json::to_value(state.store.get_cert_automation(&a.id).unwrap().unwrap()).unwrap(), before);
        assert!(!account_key_path(&state.paths, &a.id).exists());
        let work = crate::BackgroundWork::begin("fixture queued issue").unwrap();
        assert_eq!(run_once_registered(&state, &a.id, work, true, false).unwrap_err().code, "CERT_AUTO_RESOURCE_BUSY");
        let mut waiting = state.certauto_list().unwrap().remove(0);
        assert_eq!(waiting.state, "waiting"); assert!(waiting.enabled); assert_eq!(waiting.fail_count, a.fail_count);
        assert_eq!(waiting.runs.len(), a.runs.len()); assert_eq!(waiting.last_run_at, a.last_run_at);
        assert!(waiting.next_renew_at >= now_ms() + 55_000); assert!(tick(&state).is_empty());
        waiting.name = "可编辑的等待任务".into(); let edited = state.certauto_save(waiting).unwrap();
        assert_eq!(edited.name, "可编辑的等待任务");
        let stopped = state.certauto_set_enabled(&a.id, false).unwrap();
        assert_eq!(stopped.state, "idle"); assert!(!stopped.enabled); assert!(tick(&state).is_empty());
        state.certauto_delete(&a.id).unwrap(); assert!(state.store.get_cert_automation(&a.id).unwrap().is_none());
        drop(held);
    }

    #[test]
    fn queued_deployment_resumes_saved_material_without_reissuing_or_replaying_success() {
        let (dir, state, mut a) = fixture(); a.deploy_local = false; a.dns.kind = "manual".into();
        a.targets = vec![local_target(dir.path(), "completed"), local_target(dir.path(), "pending")];
        let material = material(&a, 50); retain_issued(&state, &mut a, &material).unwrap();
        a.state = "deploy_error".into(); a.next_renew_at = 0;
        a.targets[0].last_result = Some(DeployResult { ok: true, message: "already done".into(), at: 1 });
        std::fs::write(dir.path().join("completed.crt"), "retained-marker").unwrap();
        state.store.save_cert_automation(&a).unwrap();
        let held = resource_locks(&state.paths, &a, true).unwrap();
        let work = crate::BackgroundWork::begin("fixture queued deploy").unwrap();
        assert_eq!(run_once_registered(&state, &a.id, work, true, false).unwrap_err().code, "CERT_AUTO_RESOURCE_BUSY");
        let mut waiting = state.certauto_list().unwrap().remove(0); assert_eq!(waiting.state, "deploy_waiting");
        assert_eq!(waiting.deployment_id, material.deployment_id); assert!(waiting.targets[0].last_result.as_ref().unwrap().ok);
        let stopped = state.certauto_set_enabled(&a.id, false).unwrap(); assert_eq!(stopped.state, "deploy_error");
        assert_eq!(stopped.deployment_id, material.deployment_id); assert!(tick(&state).is_empty());
        // 恢复排期后资源释放，以保留的 deploy_waiting 状态走实际自动执行入口。
        waiting.updated_at = stopped.updated_at + 1; waiting.next_renew_at = 0;
        state.store.save_cert_automation(&waiting).unwrap(); drop(held);
        assert_eq!(tick(&state), vec![a.id.clone()]);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let done = loop {
            let current = state.store.get_cert_automation(&a.id).unwrap().unwrap();
            if current.state == "ok" { break current; }
            assert!(std::time::Instant::now() < deadline, "部署等待未恢复：{}", current.state);
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(done.state, "ok"); assert_eq!(done.deployment_id, material.deployment_id);
        assert_eq!(std::fs::read_to_string(dir.path().join("pending.crt")).unwrap(), material.chain);
        assert_eq!(std::fs::read_to_string(dir.path().join("completed.crt")).unwrap(), "retained-marker");
        assert!(!account_key_path(&state.paths, &a.id).exists());
        assert_eq!(done.runs.len(), 1); assert!(done.runs[0].log.iter().any(|v| v.contains("不请求 CA 或 DNS")));
    }

    #[test]
    fn actual_deployment_holds_shared_output_until_script_finishes() {
        let (dir, state, mut a) = fixture(); a.deploy_local = false; a.dns.kind = "manual".into();
        a.targets = vec![local_target(dir.path(), "shared")];
        let marker = dir.path().join("script-ready");
        let script = if cfg!(windows) {
            format!(r#"powershell.exe -NoProfile -NonInteractive -Command "[IO.File]::WriteAllText('{}','ready'); Start-Sleep -Milliseconds 1200""#, marker.to_string_lossy().replace('\'', "''"))
        } else { format!("printf ready > '{}' && sleep 1", marker.to_string_lossy().replace('\'', "'\\''")) };
        a.targets[0].config.insert("script".into(), script);
        let first = material(&a, 40); retain_issued(&state, &mut a, &first).unwrap(); a.state = "deploy_error".into();
        state.store.save_cert_automation(&a).unwrap();
        let mut b = a.clone(); b.id = "other-auto".into(); b.targets[0].config.remove("script");
        state.store.save_cert_automation(&b).unwrap();
        let second = material(&b, 50); retain_issued(&state, &mut b, &second).unwrap(); b.state = "deploy_error".into();
        state.store.save_cert_automation(&b).unwrap();
        let background = state.clone(); let id = a.id.clone();
        let worker = std::thread::spawn(move || background.certauto_retry_deploy(&id));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while !marker.exists() { assert!(std::time::Instant::now() < deadline); std::thread::sleep(std::time::Duration::from_millis(10)); }
        assert_eq!(state.certauto_retry_deploy(&b.id).unwrap_err().code, "CERT_AUTO_RESOURCE_BUSY");
        assert_eq!(std::fs::read_to_string(dir.path().join("shared.crt")).unwrap(), first.chain);
        assert_eq!(state.store.get_cert_automation(&b.id).unwrap().unwrap().state, "deploy_error");
        assert_eq!(worker.join().unwrap().unwrap().state, "ok");
        assert_eq!(state.certauto_retry_deploy(&b.id).unwrap().state, "ok");
        assert_eq!(std::fs::read_to_string(dir.path().join("shared.crt")).unwrap(), second.chain);
        assert!(!account_key_path(&state.paths, &a.id).exists()); assert!(!account_key_path(&state.paths, &b.id).exists());
    }

    #[test]
    fn certificate_resource_lock_probe() {
        let Some(root) = std::env::var_os("NSB_CERT_RESOURCE_PROBE") else { return; };
        let paths = crate::paths::Paths::new(root.into());
        let store = crate::store::Store::open(paths.db()).unwrap();
        let a = store.get_cert_automation("auto-1").unwrap().unwrap();
        let _held = resource_locks(&paths, &a, false).unwrap();
        std::fs::write(paths.base.join("resource-ready"), "ready").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(8));
    }

    #[test]
    fn resource_locks_block_other_processes_and_release_after_process_exit() {
        let (dir, state, mut a) = fixture(); a.deploy_local = false; a.targets.clear();
        state.store.save_cert_automation(&a).unwrap();
        let mut child = platform::command(std::env::current_exe().unwrap())
            .args(["--exact", "certauto::tests::certificate_resource_lock_probe", "--nocapture"])
            .env("NSB_CERT_RESOURCE_PROBE", dir.path()).spawn().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while !dir.path().join("resource-ready").is_file() {
            assert!(std::time::Instant::now() < deadline); assert!(child.try_wait().unwrap().is_none());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        a.id = "different-automation".into();
        assert_eq!(resource_locks(&state.paths, &a, false).unwrap_err().code, "CERT_AUTO_RESOURCE_BUSY");
        let mut independent = a.clone(); independent.domains = vec!["independent.example.com".into()];
        assert!(resource_locks(&state.paths, &independent, false).is_ok());
        child.kill().unwrap(); child.wait().unwrap();
        assert!(resource_locks(&state.paths, &a, false).is_ok());
        for entry in std::fs::read_dir(dir.path().join("certauto-locks/resources")).unwrap() {
            assert_eq!(entry.unwrap().metadata().unwrap().len(), 0, "锁文件不保存凭据");
        }
    }

    #[test]
    fn invalid_scheduled_deployment_is_paused_with_visible_history() {
        let (_dir, state, mut a) = fixture();
        a.targets[0].kind = "ssh".into(); a.next_renew_at = 0;
        state.store.save_cert_automation(&a).unwrap();
        assert_eq!(claim_run(&state.store, &a.id, true).unwrap_err().code, "DEPLOY_CONFIG");
        let saved = state.store.get_cert_automation(&a.id).unwrap().unwrap();
        assert!(!saved.enabled); assert_eq!(saved.state, "error"); assert_eq!(saved.runs.len(), 1);
        assert!(saved.last_error.contains("自动执行已暂停")); assert!(tick(&state).is_empty());
        assert_eq!(claim_run(&state.store, &a.id, true).unwrap_err().code, "CERT_AUTO_NOT_DUE");
        assert_eq!(state.store.get_cert_automation(&a.id).unwrap().unwrap().runs.len(), 1);
        // 已签发批次不能退回重新签发；修正配置后仍可复用材料重试。
        a.deployment_id = "retained-batch".into(); a.state = "deploy_error".into();
        state.store.save_cert_automation(&a).unwrap();
        assert!(claim_work(&state.store, &a.id, true, true).is_err());
        let saved = state.store.get_cert_automation(&a.id).unwrap().unwrap();
        assert_eq!(saved.state, "deploy_error"); assert_eq!(saved.deployment_id, "retained-batch"); assert!(!saved.enabled);
    }

    #[test]
    fn directory_move_rebases_only_local_ssh_identity_and_refuses_active_deployment() {
        let (dir, state, mut a) = fixture();
        a.targets[0].kind = "ssh".into();
        let identity = dir.path().join("identity").to_string_lossy().into_owned();
        a.targets[0].config = [("identityFile".into(), identity.clone()), ("privateKey".into(), identity.clone()),
            ("certPath".into(), "/etc/ssl/cert.pem".into()), ("keyPath".into(), "/etc/ssl/key.pem".into()),
            ("script".into(), format!("echo {identity}"))].into();
        state.store.save_cert_automation(&a).unwrap();
        let target = dir.path().join("copy"); std::fs::create_dir(&target).unwrap();
        let rebase = crate::paths::DataPathRebase::new(dir.path(), &target).unwrap();
        crate::store::Store::snapshot_for_data_dir(&state.paths.db(), &target.join("nsb.sqlite"), &rebase).unwrap();
        let store = crate::store::Store::open(target.join("nsb.sqlite")).unwrap();
        let copy = store.get_cert_automation(&a.id).unwrap().unwrap();
        assert_eq!(copy.targets[0].config["identityFile"], rebase.path(&identity));
        assert_eq!(copy.targets[0].config["privateKey"], rebase.path(&identity));
        for key in ["certPath", "keyPath", "script"] { assert_eq!(copy.targets[0].config[key], a.targets[0].config[key]); }
        assert_eq!(state.store.get_cert_automation(&a.id).unwrap().unwrap().targets[0].config["identityFile"], identity);
        a.state = "deploying".into(); state.store.save_cert_automation(&a).unwrap();
        assert_eq!(crate::store::Store::snapshot_for_data_dir(&state.paths.db(), &target.join("busy.sqlite"), &rebase).unwrap_err().code, "DATA_DIR_BUSY");
    }

    #[test]
    fn uncertain_script_pauses_retry_but_other_targets_finish() {
        let (dir, state, mut a) = fixture();
        a.deploy_local = false; a.dns.kind = "manual".into();
        a.targets = vec![local_target(dir.path(), "failed"), local_target(dir.path(), "saved")];
        a.targets[0].config.insert("script".into(), "exit 7".into());
        let material = material(&a, 45); retain_issued(&state, &mut a, &material).unwrap();
        let mut log = Vec::new(); let outcome = deploy_pending(&state, &mut a, &material, &mut log);
        assert_eq!(outcome.as_ref().unwrap_err().code, "DEPLOY_UNCERTAIN");
        let done = finish_execution(&state, a, &outcome, log).unwrap();
        assert_eq!(done.state, "deploy_interrupted"); assert!(!done.enabled); assert!(tick(&state).is_empty());
        assert!(!done.targets[0].last_result.as_ref().unwrap().ok); assert!(done.targets[1].last_result.as_ref().unwrap().ok);
        assert_eq!(std::fs::read_to_string(dir.path().join("failed.crt")).unwrap(), material.chain);
        assert_eq!(std::fs::read_to_string(dir.path().join("saved.key")).unwrap(), material.key_pem);
        assert!(read_issued(&state, &done).is_ok());
    }

    #[test]
    fn remote_only_partial_failure_retries_saved_certificate_and_skips_successful_targets() {
        let (dir, state, mut a) = fixture();
        a.deploy_local = false; a.dns.kind = "manual".into();
        a.targets = vec![local_target(dir.path(), "first"), local_target(dir.path(), "second")];
        a.targets[1].config.remove("keyPath");
        let material = material(&a, 67);
        retain_issued(&state, &mut a, &material).unwrap();
        let expiry = a.expires_at.unwrap(); let issued_at = a.issued_at;
        let exported = dir.path().join("config-export.json");
        crate::transfer::export_to(&state.store, &exported).unwrap();
        assert!(!std::fs::read_to_string(exported).unwrap().contains("PRIVATE KEY"));
        assert!(expiry > now_ms() + 66 * 86_400_000);
        let mut log = Vec::new();
        let result = deploy_pending(&state, &mut a, &material, &mut log);
        assert_eq!(result.as_ref().unwrap_err().code, "CERT_DEPLOY_FAILED");
        let mut a = finish_execution(&state, a, &result, log).unwrap();
        assert_eq!(a.state, "deploy_error"); assert!(a.cert_id.is_none()); assert!(!a.runs[0].ok);
        assert!(a.targets[0].last_result.as_ref().unwrap().ok); assert!(!a.targets[1].last_result.as_ref().unwrap().ok);
        assert_eq!(a.expires_at, Some(expiry));
        // 第一目标故意改成标记，成功目标重试时不得再次推送或运行脚本。
        std::fs::write(dir.path().join("first.crt"), "already-deployed-marker").unwrap();
        a.targets[1] = local_target(dir.path(), "second");
        a = state.certauto_save(a).unwrap();
        assert!(a.targets[0].last_result.as_ref().unwrap().ok);
        // 即使是 manual DNS，部署失败的自动排期也不重新联系 CA / DNS。
        let work = crate::BackgroundWork::begin("fixture deploy retry").unwrap();
        let done = run_once_registered(&state, &a.id, work, true, false).unwrap();
        assert_eq!(done.state, "ok"); assert!(done.runs[0].ok); assert_eq!(done.fail_count, 0);
        assert_eq!(done.expires_at, Some(expiry)); assert_eq!(done.issued_at, issued_at);
        assert_eq!(done.next_renew_at, expiry - 30 * 86_400_000);
        assert_eq!(std::fs::read_to_string(dir.path().join("first.crt")).unwrap(), "already-deployed-marker");
        assert_eq!(std::fs::read_to_string(dir.path().join("second.crt")).unwrap(), material.chain);
        assert!(done.runs[0].log.iter().any(|line| line.contains("跳过")));
    }

    #[test]
    fn failed_local_commit_restores_pair_and_does_not_block_other_targets() {
        let (dir, state, mut a) = fixture();
        a.targets = vec![local_target(dir.path(), "other")];
        let cert = state.paths.certs().join("sites/a.com.crt");
        let key = state.paths.certs().join("sites/a.com.key");
        std::fs::create_dir_all(cert.parent().unwrap()).unwrap();
        std::fs::write(&cert, "old-certificate").unwrap(); std::fs::write(&key, "old-key").unwrap();
        let db = rusqlite::Connection::open(state.paths.db()).unwrap();
        db.execute_batch("CREATE TRIGGER reject_fixture_cert BEFORE INSERT ON certs BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        let material = material(&a, 45);
        retain_issued(&state, &mut a, &material).unwrap();
        let mut log = Vec::new(); let outcome = deploy_pending(&state, &mut a, &material, &mut log);
        let a = finish_execution(&state, a, &outcome, log).unwrap();
        assert_eq!(a.state, "deploy_error"); assert!(!a.local_deploy_result.as_ref().unwrap().ok);
        assert!(a.targets[0].last_result.as_ref().unwrap().ok);
        assert_eq!(std::fs::read_to_string(cert).unwrap(), "old-certificate");
        assert_eq!(std::fs::read_to_string(key).unwrap(), "old-key");
        db.execute_batch("DROP TRIGGER reject_fixture_cert;").unwrap();
        let done = state.certauto_retry_deploy(&a.id).unwrap();
        assert_eq!(done.state, "ok"); assert!(done.local_deploy_result.unwrap().ok);
        assert_eq!(done.cert_id.as_deref(), Some("acme-a.com"));
    }

    #[test]
    fn retry_material_rejects_missing_corrupt_wrong_batch_domains_keys_and_expiry() {
        let (_dir, state, mut a) = fixture();
        a.targets.clear(); a.deploy_local = false;
        let mut good = material(&a, 40);
        retain_issued(&state, &mut a, &good).unwrap();
        let path = issued_path(&state.paths, &a.id).unwrap();
        assert!(read_issued(&state, &a).is_ok());
        std::fs::remove_file(&path).unwrap();
        assert_eq!(read_issued(&state, &a).err().unwrap().code, "CERT_DEPLOY_MISSING");
        std::fs::write(&path, "{").unwrap();
        assert_eq!(read_issued(&state, &a).err().unwrap().code, "CERT_DEPLOY_FILE");
        let write = |m: &IssuedMaterial| std::fs::write(&path, serde_json::to_vec(m).unwrap()).unwrap();
        let original_id = good.deployment_id.clone(); good.deployment_id = "other".into(); write(&good);
        assert_eq!(read_issued(&state, &a).err().unwrap().code, "CERT_DEPLOY_IDENTITY");
        good.deployment_id = original_id;
        let original_key = good.key_pem.clone(); good.key_pem = rcgen::KeyPair::generate().unwrap().serialize_pem(); write(&good);
        assert_eq!(read_issued(&state, &a).err().unwrap().code, "CERT_KEY_MISMATCH");
        good.key_pem = original_key;
        let mut other = a.clone(); other.domains = vec!["other.example.invalid".into()];
        let other_cert = material(&other, 40); good.chain = other_cert.chain; good.key_pem = other_cert.key_pem; write(&good);
        assert_eq!(read_issued(&state, &a).err().unwrap().code, "CERT_DEPLOY_DOMAINS");
        let expired = material(&a, 0); write(&expired);
        assert_eq!(read_issued(&state, &a).err().unwrap().code, "CERT_DEPLOY_EXPIRED");
        // 人工重试材料异常必须暂停；没有实际目标、CA、DNS 或通知请求。
        a.state = "deploy_error".into(); state.store.save_cert_automation(&a).unwrap();
        let rejected = state.certauto_retry_deploy(&a.id).unwrap();
        assert_eq!(rejected.state, "deploy_interrupted"); assert!(!rejected.enabled);
        assert!(!rejected.runs[0].ok); assert!(tick(&state).is_empty());
        assert_eq!(state.certauto_set_enabled(&a.id, true).unwrap_err().code, "CERT_AUTO_INTERRUPTED");
        std::fs::remove_file(&path).unwrap(); std::fs::create_dir(&path).unwrap();
        assert_eq!(read_issued(&state, &a).err().unwrap().code, "CERT_DEPLOY_FILE");
    }

    #[test]
    fn interrupted_deployment_preserves_progress_and_only_manual_retry_resumes() {
        let (dir, state, mut a) = fixture();
        a.deploy_local = false; a.targets = vec![local_target(dir.path(), "saved")];
        let material = material(&a, 50);
        retain_issued(&state, &mut a, &material).unwrap();
        deploy_pending(&state, &mut a, &material, &mut Vec::new()).unwrap();
        // 持锁中的 deploying 不算中断；释放后才恢复一次并暂停排期。
        let lock = execution_lock(&state.store, &a.id).unwrap();
        assert_eq!(state.certauto_list().unwrap()[0].state, "deploying"); drop(lock);
        let recovered = state.certauto_list().unwrap().remove(0);
        assert_eq!(recovered.state, "deploy_interrupted"); assert!(!recovered.enabled);
        assert!(recovered.targets[0].last_result.as_ref().unwrap().ok);
        assert!(tick(&state).is_empty()); assert_eq!(state.certauto_list().unwrap()[0].runs.len(), 1);
        let done = state.certauto_retry_deploy(&a.id).unwrap();
        assert_eq!(done.state, "ok"); assert!(!done.enabled); assert_eq!(done.deployment_id, material.deployment_id);
        let enabled = state.certauto_set_enabled(&a.id, true).unwrap();
        assert!(enabled.next_renew_at > now_ms() + 19 * 86_400_000);
    }

    #[test]
    fn legacy_false_success_and_unknown_remote_expiry_are_corrected_once() {
        let (_dir, state, mut a) = fixture();
        a.state = "ok".into();
        a.targets[0].last_result = Some(DeployResult { ok: false, message: "old failure".into(), at: 1 });
        state.store.save_cert_automation(&a).unwrap();
        let fixed = state.certauto_list().unwrap().remove(0);
        assert_eq!(fixed.state, "deploy_error"); assert!(!fixed.enabled);
        assert!(fixed.last_error.contains("旧版本")); assert_eq!(fixed.runs.len(), 1);
        assert_eq!(state.certauto_set_enabled(&a.id, true).unwrap_err().code, "CERT_DEPLOY_MISSING");
        assert_eq!(state.certauto_list().unwrap()[0].runs.len(), 1); assert!(tick(&state).is_empty());
        a.targets.clear(); a.deploy_local = false; a.expires_at = Some(0);
        state.store.save_cert_automation(&a).unwrap();
        let fixed = state.certauto_list().unwrap().remove(0);
        assert_eq!(fixed.state, "deploy_error"); assert!(!fixed.enabled); assert!(fixed.last_error.contains("有效期"));
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
        let issued = issued_path(&state.paths, &a.id).unwrap();
        std::fs::write(&issued, "fixture-issued-material").unwrap();
        let db = rusqlite::Connection::open(state.paths.db()).unwrap();
        // 普通 trigger 对独立连接生效；仅在临时数据库中制造删除失败。
        db.execute_batch("CREATE TRIGGER block_fixture_delete BEFORE DELETE ON cert_automations BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(state.certauto_delete(&a.id).is_err());
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "fixture-account-key");
        assert_eq!(std::fs::read_to_string(&issued).unwrap(), "fixture-issued-material");
        assert!(state.store.get_cert_automation(&a.id).unwrap().is_some());
        db.execute_batch("DROP TRIGGER block_fixture_delete;").unwrap();
        state.certauto_delete(&a.id).unwrap();
        assert!(!key.exists());
        assert!(!issued.exists());
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
