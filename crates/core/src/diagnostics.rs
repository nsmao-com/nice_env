//! 诊断包：把排查问题需要的信息打包成一个文件。
//!
//! 用户报「站点起不来」时，开发者最需要的是：环境版本、服务状态、端口占用、
//! 最近的日志、关键配置。让用户自己一项项截图复制既慢又常漏。
//!
//! 这个模块把上述内容汇总成一个 Markdown 文本（可选写盘为 .md + 关键配置附录），
//! 用户可以一键复制或另存，直接贴进 issue。
//!
//! 打包前处理常见敏感内容；任意业务日志无法保证自动识别，分享前仍需检查：
//! - 常见密码、密钥、token、授权头和 URL 凭据完全隐藏，不保留前缀
//! - 数据库 root 密码
//! - 代理订阅里的常见认证字段（uuid / password）
//! - 用户主目录路径 → 替换成 `<home>`（避免暴露用户名）
//! - 公网 IP（如果出现）不做特殊处理，但也不会主动去查

use std::io::{Read, Write};
use std::path::Path;

const MAX_REPORT_BYTES: usize = 2 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 64 * 1024;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::paths::Paths;

/// 诊断包内容
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsBundle {
    /// 完整 Markdown 文本（用户直接复制这个）
    pub markdown: String,
    /// 各项统计，供 UI 显示「包含 N 个服务 / M 行日志」
    pub service_count: usize,
    pub site_count: usize,
    pub log_lines: usize,
    /// 识别并处理的敏感内容行数；不是完整隐私检查结论。
    pub redacted: usize,
    pub generated_at: i64,
    /// 采集失败或截断说明；不能将缺失内容解释为环境正常。
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// 完整隐藏值，不保留任何密码前缀。
pub fn redact_value(v: &str) -> String {
    if v.is_empty() {
        String::new()
    } else {
        "***".into()
    }
}

/// 同时处理 Windows 正反斜杠、JSON 转义路径及大小写，保留相邻用户名的边界。
pub fn scrub_home(text: &str, home: Option<&Path>) -> String {
    let Some(home) = home else {
        return text.to_string();
    };
    let path = home
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_string();
    if path.is_empty() {
        return text.to_string();
    }
    let windows = path.as_bytes().get(1) == Some(&b':') || path.starts_with("\\\\");
    let forward = path.replace('\\', "/");
    let back = forward.replace('/', "\\");
    let mut variants = vec![forward, back.clone(), back.replace('\\', "\\\\")];
    variants.sort_by_key(|v| std::cmp::Reverse(v.len()));
    variants.dedup();
    let alternatives = variants
        .iter()
        .map(|v| regex::escape(v))
        .collect::<Vec<_>>()
        .join("|");
    let pattern = format!(
        r#"{}(?:{})($|[/\\\s"'`<>:,;)\]}}])"#,
        if windows { "(?i)" } else { "" },
        alternatives
    );
    regex::Regex::new(&pattern)
        .expect("escaped home path")
        .replace_all(text, "<home>$1")
        .into_owned()
}

fn secret_key(key: &str) -> bool {
    crate::envfile::is_secret_key(key)
        || matches!(
            key.to_ascii_lowercase().as_str(),
            "pass"
                | "pwd"
                | "requirepass"
                | "masterauth"
                | "auth"
                | "authorization"
                | "proxy-authorization"
                | "cookie"
                | "set-cookie"
                | "uuid"
                | "session"
                | "sessionid"
        )
}

pub fn redact_assignment_line(line: &str) -> String {
    use once_cell::sync::Lazy;
    static ASSIGNMENT: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(r#"^\s*[;#]?\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_.-]*)\s*=(.*)$"#)
            .unwrap()
    });
    static FIELD: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(r#"(?i)\b([a-z_][a-z0-9_.-]{0,96})\\?["']?\s*(?:[=:]\s*|[ \t]+)"#)
            .unwrap()
    });
    static AUTH: Lazy<regex::Regex> = Lazy::new(|| {
        regex::Regex::new(r#"(?i)\b(?:bearer|basic)\s+\S+|[a-z][a-z0-9+.-]*://[^\s/]+@|(?:^|\s)--?(?:password|passwd|token|secret)(?:\s|=)|\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\."#).unwrap()
    });
    if let Some(caps) = ASSIGNMENT.captures(line) {
        if secret_key(&caps[1]) {
            let value = caps.get(2).unwrap();
            return format!(
                "{}{}",
                &line[..value.start()],
                redact_value(value.as_str().trim())
            );
        }
    }
    if AUTH.is_match(line) || FIELD.captures_iter(line).any(|caps| secret_key(&caps[1])) {
        return "[敏感内容已隐藏]".into();
    }
    line.to_string()
}

/// 逐行处理常见格式，PEM 和带引号/缩进的多行凭据不会留下后续行。
pub fn redact_text(text: &str) -> (String, usize) {
    let mut count = 0;
    let mut pem = false;
    let mut quoted: Option<char> = None;
    let mut block_indent: Option<usize> = None;
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        let indent = line.len() - line.trim_start().len();
        if trimmed.contains("-----BEGIN ") && trimmed.contains("PRIVATE KEY-----") {
            pem = true;
        }
        let continued = quoted.is_some()
            || block_indent.is_some_and(|base| trimmed.is_empty() || indent > base);
        let output = if pem || continued {
            if pem && trimmed.contains("-----END ") && trimmed.contains("PRIVATE KEY-----") {
                pem = false;
            }
            if quoted.is_some_and(|quote| line.contains(quote)) {
                quoted = None;
            }
            "***".to_string()
        } else {
            block_indent = None;
            let redacted = redact_assignment_line(line);
            if redacted != line {
                if let Some((_, value)) = line.split_once('=').or_else(|| line.split_once(':')) {
                    let value = value.trim();
                    if value.is_empty() || value.starts_with('|') || value.starts_with('>') {
                        block_indent = Some(indent);
                    } else if let Some(quote @ ('\'' | '"')) = value.chars().next() {
                        if value.chars().filter(|c| *c == quote).count() == 1 {
                            quoted = Some(quote);
                        }
                    }
                }
            }
            redacted
        };
        if output != line {
            count += 1;
        }
        lines.push(output);
    }
    (lines.join("\n"), count)
}

fn bounded_config(paths: &Paths, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(&paths.base)
        .map_err(|_| AppError::new("BAD_CONFIG_PATH", "配置不在应用数据目录内"))?;
    crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
    if !path.is_file() {
        return Err(AppError::new(
            "CONFIG_UNAVAILABLE",
            "配置尚未生成或不是普通文件",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_SOURCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(AppError::new(
            "DIAGNOSTICS_SOURCE_LIMIT",
            "文件超过 64 KiB，未收录内容；请在配置编辑器中查看",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| AppError::new("CONFIG_ENCODING", "配置不是有效 UTF-8，未收录内容"))
}

/// 生成诊断包
pub fn build(
    paths: &Paths,
    store: &crate::store::Store,
    manager: &std::sync::Arc<crate::services::ServiceManager>,
    app_version: &str,
) -> Result<DiagnosticsBundle> {
    build_with_listeners(paths, store, manager, app_version, crate::ports::listeners)
}

fn build_with_listeners(
    paths: &Paths,
    store: &crate::store::Store,
    manager: &std::sync::Arc<crate::services::ServiceManager>,
    app_version: &str,
    read_listeners: impl FnOnce() -> Result<Vec<(u16, u32)>>,
) -> Result<DiagnosticsBundle> {
    let now = chrono::Local::now();
    let installed = store.list_installed()?;
    let sites = store.list_sites()?;
    let settings = store.all_settings()?;
    let mut warnings = Vec::new();
    let mut redacted = 0usize;
    let mut md = String::new();

    md.push_str("# NiceEnv 诊断报告\n\n> 本报告是采集时的快照，不等同于配置或业务健康检查。常见敏感内容已处理，分享前请检查自定义日志、域名和路径。\n\n");
    md.push_str(&format!(
        "- 应用版本：{}\n- 生成时间：{}\n- 操作系统：{} {}\n- 数据目录：{}\n\n",
        app_version,
        now.format("%Y-%m-%d %H:%M:%S"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        paths.base.to_string_lossy()
    ));

    // ---------- 服务状态 ----------
    let mut statuses = manager.list_status();
    statuses.sort_by(|a, b| a.id.cmp(&b.id));
    md.push_str("## 服务状态\n\n");
    if statuses.is_empty() {
        md.push_str("（未注册任何服务）\n\n");
    } else {
        md.push_str("| 服务 | 状态 | 端口 | 版本 | 内存 | 运行时长 |\n");
        md.push_str("|------|------|------|------|------|----------|\n");
        for s in &statuses {
            md.push_str(&format!(
                "| {} | {:?} | {} | {} | {} | {} |\n",
                s.id,
                s.state,
                s.port.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                s.version.clone().unwrap_or_else(|| "-".into()),
                s.memory_mb
                    .map(|m| format!("{m:.0} MB"))
                    .unwrap_or_else(|| "-".into()),
                s.uptime_sec
                    .map(|u| format!("{u}s"))
                    .unwrap_or_else(|| "-".into()),
            ));
        }
        md.push('\n');
        // 错误放在表格之后，保留多行诊断和操作建议，不打断后续服务行。
        for s in &statuses {
            if let Some(err) = &s.last_error {
                md.push_str(&format!("### {} 最近错误（{}）\n\n", s.id, err.code));
                for text in [Some(&err.message), err.hint.as_ref(), err.detail.as_ref()]
                    .into_iter()
                    .flatten()
                {
                    if text.len() > MAX_SOURCE_BYTES {
                        warnings.push(format!("{} 的一段错误详情超过 64 KiB，未收录", s.id));
                    } else {
                        md.push_str(text);
                        md.push_str("\n\n");
                    }
                }
            }
        }
    }

    // ---------- 已安装套件 ----------
    md.push_str("## 已安装套件\n\n");
    if installed.is_empty() {
        md.push_str("（无）\n\n");
    } else {
        for p in &installed {
            md.push_str(&format!("- {} {}（{}）\n", p.id, p.version, p.category));
        }
        md.push('\n');
    }

    // ---------- 站点 ----------
    md.push_str("## 站点\n\n");
    if sites.is_empty() {
        md.push_str("（无）\n\n");
    } else {
        for s in &sites {
            md.push_str(&format!(
                "- **{}** — {} → `{}`（{:?}，rewrite={}，https={}）\n",
                s.name,
                s.domains.join(", "),
                s.root_dir,
                s.runtime.kind,
                format!("{:?}", s.rewrite).to_lowercase(),
                s.https
            ));
        }
        md.push('\n');
    }

    // ---------- 端口占用（本应用会绑定的端口） ----------
    md.push_str("## 端口\n\n");
    let profile = crate::services::PortsProfile::from_settings(store);
    md.push_str("| 用途 | 端口 | 占用者 |\n|------|------|--------|\n");
    let ports: [(&str, u16); 8] = [
        ("HTTP (nginx)", profile.http),
        ("HTTPS (nginx)", profile.https),
        ("MySQL", profile.mysql),
        ("Redis", profile.redis),
        ("Apache HTTP", profile.apache_http),
        ("Apache HTTPS", profile.apache_https),
        ("PostgreSQL", profile.postgres),
        ("MongoDB", profile.mongodb),
    ];
    let listeners = read_listeners();
    if let Err(error) = &listeners {
        warnings.push(format!("端口占用未能采集：{}", error.message));
    }
    let mut ports = ports
        .into_iter()
        .map(|(label, port)| (label.to_string(), port))
        .collect::<Vec<_>>();
    for status in &statuses {
        if let Some(port) = status.port {
            if !ports
                .iter()
                .any(|(label, p)| label == &status.id && *p == port)
            {
                ports.push((status.id.clone(), port));
            }
        }
    }
    for (label, port) in ports {
        let holder = match &listeners {
            Ok(all) => {
                let pids = all
                    .iter()
                    .filter(|(p, _)| *p == port)
                    .map(|(_, pid)| *pid)
                    .collect::<std::collections::BTreeSet<_>>();
                if pids.is_empty() {
                    "未发现 TCP 监听".into()
                } else {
                    pids.into_iter()
                        .map(|pid| {
                            format!(
                                "{} (pid {pid})",
                                crate::ports::process_name(pid)
                                    .unwrap_or_else(|| "未知进程".into())
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("、")
                }
            }
            Err(_) => "读取失败，状态未知".into(),
        };
        md.push_str(&format!("| {label} | {port} | {holder} |\n"));
    }
    md.push('\n');

    // ---------- 证书 ----------
    match crate::certs::report(paths, store) {
        Ok(r) => {
            if r.certs.len() > 10 {
                warnings.push(
                    "证书超过 10 张，仅列出前 10 张中的异常详情；完整结果请查看证书页".into(),
                );
            }
            md.push_str("## 证书\n\n");
            md.push_str(&format!(
                "- 根 CA 已信任：{}\n- 已过期 {} 张，7 天内到期 {} 张，30 天内 {} 张\n\n",
                if r.ca_trusted { "是" } else { "否" },
                r.expired,
                r.critical,
                r.warning
            ));
            for c in r.certs.iter().take(10) {
                if c.advice.is_empty() && c.file_present {
                    continue;
                }
                md.push_str(&format!("- {}（{}）：{}\n", c.subject, c.kind, c.advice));
            }
            md.push('\n');
        }
        Err(error) => warnings.push(format!("证书状态未能采集：{}", error.message)),
    }

    // ---------- 配置摘要（脱敏） ----------
    //
    // 注意：某个配置读不到时**必须明确写出来**，不能静默跳过。
    // 否则用户报「PHP 起不来」，拿到的诊断包里却没有 php.ini，
    // 看的人无从判断是「配置正常」还是「压根没采到」。
    md.push_str("## 配置摘要（已脱敏）\n\n");
    let mut skipped: Vec<String> = Vec::new();
    let configs = crate::cfgeditor::list_configs(paths, store);
    for package in &installed {
        let kind = match package.id.as_str() {
            "php" => Some("php-ini"),
            "mysql" => Some("mysql-ini"),
            "redis" => Some("redis-conf"),
            _ => None,
        };
        if let Some(kind) = kind {
            if !configs
                .iter()
                .any(|file| file.kind == format!("{kind}@{}", package.version))
            {
                skipped.push(format!(
                    "{} {}：无法解析配置目标，版本记录可能已变更或无效",
                    package.id, package.version
                ));
            }
        }
    }
    if configs.len() > 24 {
        warnings.push("配置文件超过 24 份，仅收录前 24 份".into());
    }
    for file in configs.iter().take(24) {
        let path = Path::new(&file.path);
        match bounded_config(paths, path) {
            Ok(text) => {
                let (red, n) = redact_text(&text);
                redacted += n;
                md.push_str(&format!(
                    "### {}\n\n路径：{}\n\n```\n",
                    file.label, file.path
                ));
                for line in red.lines().take(60) {
                    md.push_str(line);
                    md.push('\n');
                }
                if red.lines().count() > 60 {
                    md.push_str("…（仅收录前 60 行）\n");
                    warnings.push(format!("{}：仅收录前 60 行", file.label));
                }
                md.push_str("```\n\n");
            }
            Err(error) => skipped.push(format!(
                "{}（{}）：{}",
                file.label, file.path, error.message
            )),
        }
    }
    for (id, label) in [
        ("php", "php.ini"),
        ("mysql", "my.ini"),
        ("redis", "redis.conf"),
    ] {
        if !installed.iter().any(|package| package.id == id) {
            skipped.push(format!("{label}：未安装对应套件，未采集"));
        }
    }

    if !skipped.is_empty() {
        md.push_str("### 未能采集的配置\n\n");
        warnings.extend(skipped.iter().cloned());
        for s in &skipped {
            md.push_str(&format!("- {s}\n"));
        }
        md.push('\n');
    }

    // ---------- 日志尾部 ----------
    md.push_str("## 最近日志\n\n");
    let mut total_log_lines = 0usize;
    if statuses.len() > 20 {
        warnings.push("服务超过 20 个，仅收录前 20 个服务的日志".into());
    }
    for s in statuses.iter().take(20) {
        let lines = match manager.tail_checked(&s.id, 40) {
            Ok(lines) => lines,
            Err(error) => {
                warnings.push(format!("{} 日志未能采集：{}", s.id, error.message));
                continue;
            }
        };
        if lines.iter().map(String::len).sum::<usize>() > MAX_SOURCE_BYTES {
            warnings.push(format!(
                "{} 最近日志超过 64 KiB，未收录内容；请在日志页查看",
                s.id
            ));
            continue;
        }
        if lines.is_empty() {
            continue;
        }
        md.push_str(&format!("### {}\n\n```\n", s.id));
        let (red, n) = redact_text(&lines.join("\n"));
        redacted += n;
        md.push_str(&red);
        md.push('\n');
        total_log_lines += red.lines().count();
        md.push_str("```\n\n");
    }
    if total_log_lines == 0 {
        md.push_str("（未采集到日志；停止的服务也可能有历史日志，请结合采集说明查看）\n\n");
    }

    // ---------- 设置摘要（脱敏） ----------
    md.push_str("## 设置\n\n");
    let keys = [
        "language",
        "appearance",
        "portProfile",
        "mirror",
        "autostart",
        "minimizeToTray",
        "autoClosePortOnStart",
        "watchdogEnabled",
        "checkUpdateOnLaunch",
        "hideScrollbars",
    ];
    for k in keys {
        if let Some((_, v)) = settings.iter().find(|(key, _)| key == k) {
            md.push_str(&format!("- {k}: {v}\n"));
        }
    }

    if !warnings.is_empty() {
        md.push_str("\n## 采集说明（失败或截断不代表正常）\n\n");
        for warning in &warnings {
            md.push_str(&format!("- {warning}\n"));
        }
    }
    // 服务错误、站点描述和设置也经过同一处理，不只处理配置与日志。
    let home = dirs::home_dir();
    let (sanitized, additional) = redact_text(&md);
    redacted += additional;
    let final_md = scrub_home(&sanitized, home.as_deref());
    let warnings = warnings
        .iter()
        .map(|warning| scrub_home(&redact_text(warning).0, home.as_deref()))
        .collect();
    if final_md.len() > MAX_REPORT_BYTES {
        return Err(AppError::new(
            "DIAGNOSTICS_TOO_LARGE",
            "诊断报告超过 2 MiB，请通过日志页或配置编辑器分别导出所需内容",
        ));
    }

    Ok(DiagnosticsBundle {
        markdown: final_md,
        service_count: statuses.len(),
        site_count: sites.len(),
        log_lines: total_log_lines,
        redacted,
        generated_at: now.timestamp(),
        warnings,
    })
}

/// 把诊断包写盘
pub fn save_to_file(paths: &Paths, bundle: &DiagnosticsBundle) -> Result<String> {
    if bundle.markdown.trim().is_empty() || bundle.markdown.len() > MAX_REPORT_BYTES {
        return Err(AppError::new(
            "DIAGNOSTICS_INVALID",
            "报告为空或超过 2 MiB，请重新生成",
        ));
    }
    let dir = crate::paths::checked_data_path(&paths.base, "diagnostics")?;
    std::fs::create_dir_all(&dir).map_err(|e| AppError::io("创建诊断目录", e))?;
    crate::paths::checked_data_path(&paths.base, "diagnostics")?;
    let prefix = format!(
        "niceenv-diagnostics-{}-",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let mut file = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".md")
        .tempfile_in(&dir)
        .map_err(|e| AppError::io("创建诊断文件", e))?;
    file.write_all(bundle.markdown.as_bytes())
        .map_err(|e| AppError::io("写入诊断包", e))?;
    file.as_file()
        .sync_all()
        .map_err(|e| AppError::io("保存诊断包", e))?;
    let (_, path) = file
        .keep()
        .map_err(|e| AppError::io("保存诊断包", e.error))?;
    Ok(path.to_string_lossy().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path as StdPath;

    #[test]
    fn redact_short_values_fully_masked() {
        assert_eq!(redact_value("a"), "***");
        assert_eq!(redact_value("ab"), "***");
        assert_eq!(redact_value(""), "");
    }

    #[test]
    fn redact_does_not_keep_secret_prefix() {
        let r = redact_value("supersecret");
        assert!(!r.contains("su"), "{r}");
        assert!(r.contains('*'), "{r}");
        assert!(!r.contains("per"), "原文不该残留：{r}");
    }

    #[test]
    fn redact_caps_length() {
        // 超长值不该产生超长星号串
        let r = redact_value(&"x".repeat(200));
        assert!(r.len() <= 14, "长度 {} 应被截断：{r}", r.len());
    }

    #[test]
    fn redacts_password_assignment() {
        let out = redact_assignment_line("DB_PASSWORD=hunter2");
        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.starts_with("DB_PASSWORD="), "{out}");
    }

    #[test]
    fn redacts_secret_assignments_in_ini_style() {
        for line in [
            "mysql_root_password=abc123",
            "xdebug.remote_key=zzz",
            "api_token=xyz",
            "app_secret=qwerty",
        ] {
            let out = redact_assignment_line(line);
            assert!(out.contains('*'), "{line} 应被打码：{out}");
        }
    }

    #[test]
    fn keeps_non_secret_assignments_intact() {
        for line in [
            "memory_limit=256M",
            "port=3306",
            "display_errors=On",
            "DB_HOST=127.0.0.1",
            "max_connections=200",
        ] {
            assert_eq!(redact_assignment_line(line), line, "{line} 不该被改");
        }
    }

    #[test]
    fn redacts_commented_secret_lines_too() {
        // 注释掉的密码同样是密码
        let out = redact_assignment_line(";password=secret123");
        assert!(!out.contains("secret123"), "{out}");
    }

    #[test]
    fn redacts_export_prefixed_vars() {
        let out = redact_assignment_line("export AWS_SECRET_ACCESS_KEY=abcdefg");
        assert!(!out.contains("abcdefg"), "{out}");
    }

    #[test]
    fn redact_text_counts_hits() {
        let src = "DB_PASSWORD=aaa\nport=3306\napi_token=bbb\n";
        let (out, n) = redact_text(src);
        assert_eq!(n, 2, "应统计 2 处：{out}");
        assert!(!out.contains("aaa"));
        assert!(!out.contains("bbb"));
        assert!(out.contains("port=3306"), "非敏感项保留");
    }

    #[test]
    fn redact_text_preserves_line_count() {
        let src = "a=1\n\nb=2\n";
        let (out, _) = redact_text(src);
        assert_eq!(out.lines().count(), src.lines().count());
    }

    #[test]
    fn scrub_home_replaces_username_path() {
        let home = StdPath::new("C:/Users/alice");
        let text = "path is C:/Users/alice/projects/x";
        let out = scrub_home(text, Some(home));
        assert!(!out.contains("alice"), "不该泄漏用户名：{out}");
        assert!(out.contains("<home>"), "{out}");
    }

    #[test]
    fn scrub_home_handles_backslash_form() {
        let home = StdPath::new("C:/Users/alice");
        let text = r"root: C:\Users\alice\code";
        let out = scrub_home(text, Some(home));
        assert!(!out.contains("alice"), "{out}");
    }

    #[test]
    fn scrub_home_without_home_is_noop() {
        let text = "C:/Users/alice/x";
        assert_eq!(scrub_home(text, None), text);
    }

    #[test]
    fn scrub_home_leaves_unrelated_text_alone() {
        let home = StdPath::new("/home/bob");
        let text = "nothing to replace here";
        assert_eq!(scrub_home(text, Some(home)), text);
    }

    #[test]
    fn save_writes_markdown_file() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        let b = DiagnosticsBundle {
            markdown: "# test\n".into(),
            service_count: 0,
            site_count: 0,
            log_lines: 0,
            redacted: 0,
            generated_at: 0,
            warnings: vec![],
        };
        let p = save_to_file(&paths, &b).unwrap();
        assert!(StdPath::new(&p).is_file());
        assert!(p.ends_with(".md"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), b.markdown);
        let second = save_to_file(&paths, &b).unwrap();
        assert_ne!(p, second);
        assert_eq!(std::fs::read_to_string(&p).unwrap(), b.markdown);
        let mut invalid = b.clone();
        invalid.markdown.clear();
        assert!(save_to_file(&paths, &invalid).is_err());
        invalid.markdown = "x".repeat(MAX_REPORT_BYTES + 1);
        assert!(save_to_file(&paths, &invalid).is_err());
        assert_eq!(
            std::fs::read_dir(paths.base.join("diagnostics"))
                .unwrap()
                .count(),
            2
        );
    }

    #[test]
    fn redacts_structured_headers_urls_and_multiline_credentials() {
        for line in [
            r#"{"password":"fixture-secret-123"}"#,
            r#"{\"password\":\"fixture-secret-123\"}"#,
            "[ERROR] connecting with token=fixture-secret-123 failed",
            "Authorization: Bearer fixture-secret-123",
            "Cookie: session=fixture-secret-123",
            "requirepass fixture-secret-123",
            "masterauth fixture-secret-123",
            "url=https://name:fixture-secret-123@example.test/sub",
            "GET /?access_token=fixture-secret-123 HTTP/1.1",
            "  uuid: fixture-secret-123",
            "cmd --password=fixture-secret-123",
        ] {
            let (red, n) = redact_text(line);
            assert!(!red.contains("fixture-secret-123"), "{line} -> {red}");
            assert!(n > 0);
        }
        let input = "SECRET=\"first-secret\nsecond-secret\"\npassword: |\n  third-secret\n  fourth-secret\nport=3306\n-----BEGIN RSA PRIVATE KEY-----\nfifth-secret\n-----END RSA PRIVATE KEY-----\nmemory_limit=256M";
        let (red, _) = redact_text(input);
        for secret in [
            "first-secret",
            "second-secret",
            "third-secret",
            "fourth-secret",
            "fifth-secret",
        ] {
            assert!(!red.contains(secret), "{red}");
        }
        assert!(red.contains("port=3306"));
        assert!(red.contains("memory_limit=256M"));
        assert_eq!(redact_text(&red).0, red);
    }

    #[test]
    fn home_redaction_handles_native_windows_paths_and_user_boundaries() {
        let home = Path::new(r"C:\Users\Alice");
        let input = r#"C:/Users/Alice/project c:\users\alice\project C:\\Users\\Alice\\project C:/Users/Alice-other/project"#;
        let red = scrub_home(input, Some(home));
        assert_eq!(red.matches("<home>").count(), 3, "{red}");
        assert!(red.contains("Alice-other"));
        assert!(!red.contains(r"Users\Alice\project"));
    }

    #[test]
    fn report_contains_all_versions_stopped_logs_and_explicit_collection_errors() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = crate::store::Store::open(paths.db()).unwrap();
        for version in ["8.3.17", "8.4.26"] {
            store
                .upsert_installed(&crate::model::InstalledPackage {
                    id: "php".into(),
                    version: version.into(),
                    category: "runtime".into(),
                    install_path: paths.runtime_dir("php", version).to_string_lossy().into(),
                    config_path: "".into(),
                    installed_at: 0,
                })
                .unwrap();
            let ini = paths.php_ini(version);
            std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
            std::fs::write(
                &ini,
                format!("memory_limit=256M\npassword=fixture-{version}-secret\n"),
            )
            .unwrap();
        }
        std::fs::write(paths.nginx_conf(), vec![0xff]).unwrap();
        let manager = std::sync::Arc::new(crate::services::ServiceManager::new());
        let history = paths.base.join("stopped.log");
        std::fs::write(
            &history,
            "historical warning\nAuthorization: Bearer log-fixture-secret\n",
        )
        .unwrap();
        manager.register(
            "php@8.3.17",
            "PHP",
            Some("8.3.17".into()),
            None,
            None,
            history,
        );
        manager.register(
            "nginx",
            "Nginx",
            None,
            None,
            Some(18080),
            paths.base.clone(),
        );
        manager.set_error(
            "nginx",
            AppError::new("FIXTURE", "upstream failed: token=error-fixture-secret")
                .with_hint("请检查上游服务")
                .with_detail("upstream details\nCookie: detail-fixture-secret"),
        );
        // 仅破坏临时夹具的证书表；报告需保留其它内容并列出采集错误。
        rusqlite::Connection::open(paths.db())
            .unwrap()
            .execute_batch("DROP TABLE certs")
            .unwrap();
        let bundle = build_with_listeners(&paths, &store, &manager, "fixture", || {
            Err(AppError::new("PORT_FIXTURE", "端口读取失败"))
        })
        .unwrap();
        for version in ["8.3.17", "8.4.26"] {
            assert!(bundle.markdown.contains(&format!("php.ini · {version}")));
        }
        for secret in [
            "fixture-8.3.17-secret",
            "fixture-8.4.26-secret",
            "log-fixture-secret",
            "error-fixture-secret",
            "detail-fixture-secret",
        ] {
            assert!(!bundle.markdown.contains(secret), "{secret}");
        }
        assert!(bundle.markdown.contains("historical warning"));
        assert!(bundle.markdown.contains("请检查上游服务"));
        assert!(bundle.markdown.contains("upstream details"));
        assert_eq!(bundle.log_lines, 2);
        assert!(bundle.markdown.contains("读取失败，状态未知"));
        assert!(!bundle.markdown.contains("| 空闲 |"));
        for expected in ["端口", "证书", "UTF-8", "nginx 日志"] {
            assert!(
                bundle.warnings.iter().any(|w| w.contains(expected)),
                "{expected}: {:?}",
                bundle.warnings
            );
        }
        let saved = save_to_file(&paths, &bundle).unwrap();
        store
            .set_setting("language", "changed-after-preview")
            .unwrap();
        assert_eq!(std::fs::read_to_string(saved).unwrap(), bundle.markdown);
        rusqlite::Connection::open(paths.db())
            .unwrap()
            .execute_batch("DROP TABLE installed")
            .unwrap();
        assert!(build_with_listeners(&paths, &store, &manager, "fixture", || Ok(vec![])).is_err());
    }

    #[test]
    fn report_bounds_files_and_rejects_invalid_export_directory() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        let config = paths.base.join("large.ini");
        std::fs::write(&config, vec![b'a'; MAX_SOURCE_BYTES + 1]).unwrap();
        assert_eq!(
            bounded_config(&paths, &config).unwrap_err().code,
            "DIAGNOSTICS_SOURCE_LIMIT"
        );
        assert!(bounded_config(&paths, temp.path().parent().unwrap()).is_err());
        std::fs::write(paths.base.join("diagnostics"), "keep existing file").unwrap();
        let bundle = DiagnosticsBundle {
            markdown: "# snapshot\n".into(),
            service_count: 0,
            site_count: 0,
            log_lines: 0,
            redacted: 0,
            generated_at: 0,
            warnings: vec![],
        };
        assert!(save_to_file(&paths, &bundle).is_err());
        assert_eq!(
            std::fs::read_to_string(paths.base.join("diagnostics")).unwrap(),
            "keep existing file"
        );
    }

    #[test]
    fn failed_port_commands_cannot_report_no_listeners() {
        assert!(crate::ports::check_listener_exit(Some(0), b"", b"", false).is_ok());
        assert!(crate::ports::check_listener_exit(Some(1), b"", b"", true).is_ok());
        for lsof in [false, true] {
            assert!(
                crate::ports::check_listener_exit(Some(2), b"", b"permission denied", lsof)
                    .is_err()
            );
            assert!(crate::ports::check_listener_exit(None, b"", b"", lsof).is_err());
        }
        assert!(crate::ports::check_listener_exit(Some(1), b"", b"failed", true).is_err());
        assert!(crate::ports::check_listener_exit(
            Some(0),
            b"partial result",
            b"permission warning",
            true
        )
        .is_err());
    }
}
