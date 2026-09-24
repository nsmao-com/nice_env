//! 诊断包：把排查问题需要的信息打包成一个文件。
//!
//! 用户报「站点起不来」时，开发者最需要的是：环境版本、服务状态、端口占用、
//! 最近的日志、关键配置。让用户自己一项项截图复制既慢又常漏。
//!
//! 这个模块把上述内容汇总成一个 Markdown 文本（可选写盘为 .md + 关键配置附录），
//! 用户可以一键复制或另存，直接贴进 issue。
//!
//! **红线：绝不泄漏隐私**。打包前对以下内容做脱敏：
//! - 密码 / 密钥 / token 类环境变量与 ini 项 → 只留前 2 位并打码
//! - 数据库 root 密码
//! - 代理订阅里的节点信息（server / uuid / password）
//! - 用户主目录路径 → 替换成 `<home>`（避免暴露用户名）
//! - 公网 IP（如果出现）不做特殊处理，但也不会主动去查

use std::path::Path;

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
    /// 打包时被脱敏的条目数 —— 让用户确信密码没被打进去
    pub redacted: usize,
    pub generated_at: i64,
}

/// 脱敏：把敏感值换成只露前两位的形式
pub fn redact_value(v: &str) -> String {
    let n = v.chars().count();
    if n == 0 {
        return String::new();
    }
    if n <= 2 {
        return "**".to_string();
    }
    let head: String = v.chars().take(2).collect();
    format!("{head}{}", "*".repeat((n - 2).min(12)))
}

/// 把用户主目录替换成占位符，避免泄漏用户名
pub fn scrub_home(text: &str, home: Option<&Path>) -> String {
    let Some(h) = home else {
        return text.to_string();
    };
    let hs = h.to_string_lossy();
    if hs.is_empty() {
        return text.to_string();
    }
    let mut out = text.replace(hs.as_ref(), "<home>");
    // 反斜杠形式也要处理（Windows）
    let bs = hs.replace('/', "\\");
    if bs != hs {
        out = out.replace(&bs, "<home>");
    }
    out
}

/// 脱敏一行 ini / env 风格的内容（key=value）
pub fn redact_assignment_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let (body, commented) = match trimmed.strip_prefix(';') {
        Some(r) => (r.trim_start(), true),
        None => match trimmed.strip_prefix('#') {
            Some(r) => (r.trim_start(), true),
            None => (trimmed, false),
        },
    };
    let body = body.strip_prefix("export ").unwrap_or(body);
    let Some((k, v)) = body.split_once('=') else {
        return line.to_string();
    };
    if !crate::envfile::is_secret_key(k.trim()) {
        return line.to_string();
    }
    let val = v.trim().trim_matches('"').trim_matches('\'');
    let red = redact_value(val);
    let prefix = if commented { "#" } else { "" };
    format!("{indent}{prefix}{}={}", k.trim(), red)
}

/// 脱敏文本里的所有敏感赋值行
pub fn redact_text(text: &str) -> (String, usize) {
    let mut count = 0;
    let out: Vec<String> = text
        .lines()
        .map(|l| {
            let r = redact_assignment_line(l);
            if r != l {
                count += 1;
            }
            r
        })
        .collect();
    (out.join("\n"), count)
}

/// 生成诊断包
pub fn build(
    paths: &Paths,
    store: &crate::store::Store,
    manager: &std::sync::Arc<crate::services::ServiceManager>,
    app_version: &str,
) -> Result<DiagnosticsBundle> {
    let now = chrono::Local::now();
    let mut redacted = 0usize;
    let mut md = String::new();

    md.push_str("# NiceEnv 诊断报告\n\n");
    md.push_str(&format!(
        "- 应用版本：{}\n- 生成时间：{}\n- 操作系统：{} {}\n- 数据目录：{}\n\n",
        app_version,
        now.format("%Y-%m-%d %H:%M:%S"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        paths.base.to_string_lossy()
    ));

    // ---------- 服务状态 ----------
    let statuses = manager.list_status();
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
            // 记下最后一个错误（这是排查的关键线索）
            if let Some(err) = &s.last_error {
                md.push_str(&format!(
                    "\n> **{} 错误**：{}（{}）\n\n",
                    s.id, err.message, err.code
                ));
            }
        }
        md.push('\n');
    }

    // ---------- 已安装套件 ----------
    let installed = store.list_installed().unwrap_or_default();
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
    let sites = crate::sites::list(store).unwrap_or_default();
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
    for (label, port) in ports {
        // 逐个探测占用者：诊断包的价值就在于把「端口被谁占了」直接写清楚，
        // 而不是让用户自己再去查一遍
        let holder = match crate::ports::diagnose_port(port) {
            Ok(d) if d.in_use => d
                .process_name
                .clone()
                .map(|n| format!("{n}(pid {})", d.pid.unwrap_or(0)))
                .unwrap_or_else(|| format!("pid {}", d.pid.unwrap_or(0))),
            _ => "空闲".to_string(),
        };
        md.push_str(&format!("| {label} | {port} | {holder} |\n"));
    }
    md.push('\n');

    // ---------- 证书 ----------
    match crate::certs::report(paths, store) {
        Ok(r) => {
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
        Err(_) => {}
    }

    // ---------- 配置摘要（脱敏） ----------
    //
    // 注意：某个配置读不到时**必须明确写出来**，不能静默跳过。
    // 否则用户报「PHP 起不来」，拿到的诊断包里却没有 php.ini，
    // 看的人无从判断是「配置正常」还是「压根没采到」。
    md.push_str("## 配置摘要（已脱敏）\n\n");
    let mut skipped: Vec<String> = Vec::new();
    for kind in [
        crate::cfgeditor::ConfigKind::NginxMain,
        crate::cfgeditor::ConfigKind::PhpIni,
        crate::cfgeditor::ConfigKind::MySqlIni,
        crate::cfgeditor::ConfigKind::RedisConf,
    ] {
        let path = match crate::cfgeditor::resolve_path(paths, store, kind) {
            Ok(p) => p,
            Err(e) => {
                skipped.push(format!("{}：{}", kind_label(kind).0, e.message));
                continue;
            }
        };
        if !path.is_file() {
            skipped.push(format!(
                "{}：尚未生成（{}）",
                kind_label(kind).0,
                path.display()
            ));
            continue;
        }
        let (label, _, _) = kind_label(kind);
        md.push_str(&format!("### {label}\n\n```\n"));
        if let Ok(text) = std::fs::read_to_string(&path) {
            // 配置文件可能很长，只取前 60 行，够看出关键项
            let (red, n) = redact_text(&text);
            redacted += n;
            for l in red.lines().take(60) {
                md.push_str(l);
                md.push('\n');
            }
            if text.lines().count() > 60 {
                md.push_str("…（已截断）\n");
            }
        }
        md.push_str("```\n\n");
    }

    if !skipped.is_empty() {
        md.push_str("### 未能采集的配置\n\n");
        for s in &skipped {
            md.push_str(&format!("- {s}\n"));
        }
        md.push('\n');
    }

    // ---------- 日志尾部 ----------
    md.push_str("## 最近日志\n\n");
    let mut total_log_lines = 0usize;
    for s in statuses.iter().take(8) {
        let lines = manager.tail(&s.id, 40);
        if lines.is_empty() {
            continue;
        }
        md.push_str(&format!("### {}\n\n```\n", s.id));
        for l in &lines {
            let (red, n) = redact_text(l);
            redacted += n;
            md.push_str(&red);
            md.push('\n');
            total_log_lines += 1;
        }
        md.push_str("```\n\n");
    }
    if total_log_lines == 0 {
        md.push_str("（没有运行中的服务，无日志）\n\n");
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
        if let Some(v) = store.get_setting(k) {
            md.push_str(&format!("- {k}: {v}\n"));
        }
    }

    // 最后统一做 home 目录脱敏
    let home = dirs::home_dir();
    let final_md = scrub_home(&md, home.as_deref());

    Ok(DiagnosticsBundle {
        markdown: final_md,
        service_count: statuses.len(),
        site_count: sites.len(),
        log_lines: total_log_lines,
        redacted,
        generated_at: now.timestamp(),
    })
}

fn kind_label(k: crate::cfgeditor::ConfigKind) -> (&'static str, &'static str, Option<&'static str>) {
    match k {
        crate::cfgeditor::ConfigKind::NginxMain => ("Nginx 主配置", "", Some("nginx")),
        crate::cfgeditor::ConfigKind::PhpIni => ("php.ini", "", Some("php")),
        crate::cfgeditor::ConfigKind::MySqlIni => ("my.ini", "", Some("mysql")),
        crate::cfgeditor::ConfigKind::RedisConf => ("redis.conf", "", Some("redis")),
        crate::cfgeditor::ConfigKind::ApacheConf => ("httpd.conf", "", Some("apache")),
        crate::cfgeditor::ConfigKind::MihomoConfig => ("config.yaml", "", Some("mihomo")),
    }
}

/// 把诊断包写盘
pub fn save_to_file(paths: &Paths, bundle: &DiagnosticsBundle) -> Result<String> {
    let dir = paths.base.join("diagnostics");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::io("创建诊断目录", e))?;
    let name = format!(
        "niceenv-diagnostics-{}.md",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let path = dir.join(&name);
    std::fs::write(&path, &bundle.markdown).map_err(|e| AppError::io("写入诊断包", e))?;
    Ok(path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path as StdPath;

    #[test]
    fn redact_short_values_fully_masked() {
        assert_eq!(redact_value("a"), "**");
        assert_eq!(redact_value("ab"), "**");
        assert_eq!(redact_value(""), "");
    }

    #[test]
    fn redact_keeps_first_two_chars_only() {
        let r = redact_value("supersecret");
        assert!(r.starts_with("su"), "{r}");
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
    fn kind_label_covers_all_config_kinds() {
        for k in [
            crate::cfgeditor::ConfigKind::NginxMain,
            crate::cfgeditor::ConfigKind::PhpIni,
            crate::cfgeditor::ConfigKind::MySqlIni,
            crate::cfgeditor::ConfigKind::RedisConf,
            crate::cfgeditor::ConfigKind::ApacheConf,
            crate::cfgeditor::ConfigKind::MihomoConfig,
        ] {
            let (l, _, _) = kind_label(k);
            assert!(!l.is_empty());
        }
    }

    #[test]
    fn save_writes_markdown_file() {
        let t = std::env::temp_dir().join(format!("nsb-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        let b = DiagnosticsBundle {
            markdown: "# test\n".into(),
            service_count: 0,
            site_count: 0,
            log_lines: 0,
            redacted: 0,
            generated_at: 0,
        };
        let p = save_to_file(&paths, &b).unwrap();
        assert!(StdPath::new(&p).is_file());
        assert!(p.ends_with(".md"));
        let _ = std::fs::remove_dir_all(&t);
    }
}
