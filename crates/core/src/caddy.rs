//! Caddy 站点配置。复用已安装的 Caddy 与 PHP 池，只使用 NiceEnv 已选定的证书。
use crate::{
    error::{AppError, Result},
    model::{RewritePreset, Site, SiteKind},
    paths::{nginx_path, Paths},
    store::Store,
};
use std::path::{Path, PathBuf};

// Caddyfile 保留正则里的反斜杠；JSON 的双反斜杠转义会把匹配语义改变。
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

pub(crate) fn config_path(paths: &Paths, store: &Store) -> Result<PathBuf> {
    let package = crate::ops::installed_by_choice(store, "caddy")
        .ok_or_else(|| AppError::not_installed("Caddy"))?;
    Ok(crate::paths::checked_data_path(
        &paths.base,
        &format!("etc/caddy/{}/Caddyfile", package.version),
    )?)
}

pub(crate) fn https_port(store: &Store) -> Result<u16> {
    match store.get_setting_checked("portOverride.caddyHttps")? {
        Some(value) => value
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| AppError::new("BAD_PORT", "Caddy HTTPS 端口必须在 1–65535 范围内")),
        None => Ok(
            if store.get_setting_checked("portProfile")?.as_deref() == Some("safe") {
                28443
            } else {
                8445
            },
        ),
    }
}

pub(crate) fn ports(paths: &Paths, store: &Store) -> Result<(u16, u16)> {
    let entry = crate::generic::manifest_entry_for(store, "caddy")
        .or_else(|| {
            crate::install::Installer::effective(paths)
                .manifest
                .packages
                .into_iter()
                .find(|entry| entry.id == "caddy")
        })
        .ok_or_else(|| AppError::new("CADDY_PACKAGE", "清单中未找到 Caddy"))?;
    let spec = entry
        .run
        .as_ref()
        .ok_or_else(|| AppError::new("CADDY_PACKAGE", "Caddy 运行配置缺失"))?;
    let http = crate::generic::resolve_port(store, "caddy", &entry, spec)?
        .ok_or_else(|| AppError::new("BAD_PORT", "Caddy HTTP 端口未配置"))?;
    let https = https_port(store)?;
    if http == https {
        return Err(AppError::new(
            "BAD_PORT",
            "Caddy HTTP 与 HTTPS 端口不能相同",
        ));
    }
    Ok((http, https))
}

/// Caddyfile 中只有独立的花括号是块边界；引号、注释和占位符中的括号不是。
#[derive(Debug)]
struct Token {
    value: String,
    start: usize,
    end: usize,
    depth: usize,
}
fn tokens(source: &str) -> Result<Vec<Token>> {
    let bytes = source.as_bytes();
    let mut at = 0;
    let mut depth = 0usize;
    let mut out = Vec::new();
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if bytes[at] == b'#' {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        let start = at;
        let quote = (bytes[at] == b'"' || bytes[at] == b'`').then_some(bytes[at]);
        if let Some(quote) = quote {
            at += 1;
            let mut closed = false;
            while at < bytes.len() {
                if bytes[at] == b'\\' && quote == b'"' {
                    at += 2;
                    continue;
                }
                if bytes[at] == quote {
                    at += 1;
                    closed = true;
                    break;
                }
                at += 1;
            }
            if !closed {
                return Err(AppError::new("CADDY_CONFIG", "Caddyfile 有未闭合的引号"));
            }
        } else {
            while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
                at += 1;
            }
        }
        let value = &source[start..at];
        if quote.is_none() && value == "}" {
            depth = depth
                .checked_sub(1)
                .ok_or_else(|| AppError::new("CADDY_CONFIG", "Caddyfile 块边界无效"))?;
        }
        out.push(Token {
            value: value.into(),
            start,
            end: at,
            depth,
        });
        if quote.is_none() && value == "{" {
            depth += 1;
        }
    }
    if depth != 0 {
        return Err(AppError::new("CADDY_CONFIG", "Caddyfile 块未闭合"));
    }
    Ok(out)
}

fn global_options(source: &str, http: u16, https: u16, errors: &str) -> Result<String> {
    let mut clean = source.to_string();
    if let Some(start) = clean.find("\n\t# BEGIN NiceEnv site options\n") {
        let marker = "\t# END NiceEnv site options\n";
        let end = clean[start..]
            .find(marker)
            .ok_or_else(|| AppError::new("CADDY_CONFIG", "Caddy 托管选项标记不完整"))?
            + start
            + marker.len();
        clean.replace_range(start..end, "");
    }
    let source = clean.as_str();
    let parsed = tokens(source)?;
    // Caddy 2.11 的 skip_install_trust 只作用于显式声明的 CA；运行期生成的默认 CA 会忽略它。
    let pki = if parsed
        .iter()
        .any(|token| token.depth == 1 && token.value == "pki")
    {
        ""
    } else {
        "\tpki {\n\t\tca local {\n\t\t}\n\t}\n"
    };
    let options = format!("\n\t# BEGIN NiceEnv site options\n\thttp_port {http}\n\thttps_port {https}\n\tauto_https off\n\tskip_install_trust\n{pki}{errors}\t# END NiceEnv site options\n");
    if !parsed.first().is_some_and(|t| t.value == "{") {
        return Ok(format!("{{{options}}}\n{source}"));
    }
    let closing = parsed
        .iter()
        .find(|t| t.depth == 0 && t.value == "}")
        .ok_or_else(|| AppError::new("CADDY_CONFIG", "Caddyfile 全局配置未闭合"))?;
    let mut edits = Vec::new();
    for (index, token) in parsed
        .iter()
        .enumerate()
        .filter(|(_, t)| t.depth == 1 && t.start < closing.start)
    {
        if matches!(
            token.value.as_str(),
            "http_port" | "https_port" | "auto_https" | "skip_install_trust"
        ) {
            let end = source[token.end..]
                .find('\n')
                .map_or(token.end, |n| token.end + n);
            // 只管理顶层监听选项，不能跨过同行的块结束符。
            if source[token.end..end].contains(['{', '}']) {
                return Err(AppError::new(
                    "CADDY_CONFIG",
                    "请将 Caddy 全局端口与 auto_https 选项各写一行",
                ));
            }
            edits.push((token.start, end, String::new()));
        }
        if token.value == "log"
            && parsed
                .get(index + 1)
                .is_some_and(|t| t.value.starts_with("niceenv_site_error_"))
        {
            let end = parsed[index + 1..]
                .iter()
                .find(|t| t.value == "}" && t.depth == 1)
                .ok_or_else(|| AppError::new("CADDY_CONFIG", "Caddy 日志块未闭合"))?
                .end;
            edits.push((token.start, end, String::new()));
        }
        if token.value == "pki" {
            let remaining = &parsed[index + 1..];
            let end = remaining
                .iter()
                .find(|t| t.value == "}" && t.depth == 1)
                .ok_or_else(|| AppError::new("CADDY_CONFIG", "Caddy PKI 块未闭合"))?;
            if !remaining
                .windows(2)
                .take_while(|pair| pair[0].start < end.start)
                .any(|pair| pair[0].depth == 2 && pair[0].value == "ca" && pair[1].value == "local")
            {
                edits.push((end.start, end.start, "\n\t\tca local {\n\t\t}\n\t".into()));
            }
        }
    }
    let mut output = source.to_string();
    edits.push((closing.start, closing.start, options));
    edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for (start, end, value) in edits {
        output.replace_range(start..end, &value);
    }
    Ok(output)
}

pub(crate) fn import_line(paths: &Paths) -> String {
    format!(
        "import {}",
        quoted(&format!("{}/*.conf", nginx_path(&paths.caddy_sites_dir())))
    )
}

pub(crate) fn includes_sites(source: &str, paths: &Paths) -> bool {
    let expected = quoted(&format!("{}/*.conf", nginx_path(&paths.caddy_sites_dir())));
    tokens(source).is_ok_and(|tokens| {
        tokens.windows(2).any(|pair| {
            pair[0].depth == 0 && pair[0].value == "import" && pair[1].value == expected
        })
    })
}

fn bind_default_site(source: &str, http: u16) -> Result<String> {
    let parsed = tokens(source)?;
    let mut additions = Vec::new();
    for (index, token) in parsed.iter().enumerate() {
        if token.depth != 0
            || token.value != "{"
            || index == 0
            || parsed[index - 1].value != format!("http://:{http}")
        {
            continue;
        }
        let block = parsed[index + 1..]
            .iter()
            .take_while(|token| token.depth > 0);
        // 主配置的兜底站点与托管域名必须复用同一监听；保留用户显式写过的 bind。
        if !block
            .into_iter()
            .any(|token| token.depth == 1 && token.value == "bind")
        {
            additions.push(token.end);
        }
    }
    let mut output = source.to_string();
    for at in additions.into_iter().rev() {
        output.insert_str(at, "\n\tbind 127.0.0.1");
    }
    Ok(output)
}

/// 由 generic 在选定最终 HTTP 端口后调用；现有自定义配置保留，仅管理站点导入与监听选项。
pub(crate) fn prepare(
    paths: &Paths,
    store: &Store,
    resolved: &crate::generic::Resolved,
) -> Result<serde_json::Value> {
    let config = resolved.etc.join("Caddyfile");
    let previous = std::fs::read_to_string(&config)?;
    // 兼容旧清单直接展开的路径；只引用完全匹配的托管路径，不重写用户自定义目录。
    let initial: String = previous
        .split_inclusive('\n')
        .map(|line| {
            for (directive, path) in [
                ("storage file_system", resolved.data.join("storage")),
                ("root *", resolved.data.join("www")),
            ] {
                let path = nginx_path(&path);
                if line.trim() == format!("{directive} {path}") {
                    let indent = &line[..line.len() - line.trim_start().len()];
                    return format!(
                        "{indent}{directive} {}{}",
                        quoted(&path),
                        if line.ends_with('\n') { "\n" } else { "" }
                    );
                }
            }
            line.into()
        })
        .collect();
    let sites = store
        .list_sites()?
        .into_iter()
        .filter(|site| site.runtime.web_server == "caddy")
        .collect::<Vec<_>>();
    let http = resolved
        .port
        .ok_or_else(|| AppError::new("BAD_PORT", "Caddy HTTP 端口未配置"))?;
    let https = https_port(store)?;
    if http == https {
        return Err(AppError::new(
            "BAD_PORT",
            "Caddy HTTP 与 HTTPS 端口不能相同",
        ));
    }
    let mut changed: Vec<(PathBuf, String, String)> = Vec::new();
    let result = (|| {
        let mut errors = String::new();
        for site in &sites {
            if site.id.is_empty()
                || !site
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
            {
                return Err(AppError::new(
                    "BAD_SITE_ID",
                    "站点标识无效，无法生成 Caddy 配置",
                ));
            }
            let path = crate::paths::checked_data_path(
                &paths.base,
                &format!("etc/caddy/sites/{}.conf", site.id),
            )?;
            if !path.is_file() {
                continue;
            }
            let original = std::fs::read_to_string(&path)?;
            let mut updated = sync_site_ports(&original, site, http, https)?;
            if site.runtime.kind == SiteKind::Php {
                let php = site
                    .runtime
                    .php_version
                    .as_deref()
                    .and_then(|version| store.get_port_assign(&format!("php@{version}")))
                    .unwrap_or(1);
                let pattern = regex::Regex::new(r"(?m)^(\s*php_fastcgi 127\.0\.0\.1:)\d+( \{)$")
                    .expect("constant pattern");
                updated = pattern
                    .replace_all(&updated, format!("${{1}}{php}${{2}}"))
                    .into_owned();
            }
            crate::paths::write_with_backup_expected(
                &path,
                &updated,
                &paths.backup(),
                Some(Some(original.as_bytes())),
            )?;
            if original != updated {
                changed.push((path, original, updated));
            }
            errors.push_str(&format!("\tlog niceenv_site_error_{id} {{\n\t\tlevel ERROR\n\t\tinclude http.log.error.niceenv_{id}_http http.log.error.niceenv_{id}_https\n\t\toutput file {file}\n\t}}\n", id = site.id,
            file = quoted(&nginx_path(&paths.logs().join("caddy").join(format!("{}.error.log", site.id))))));
        }
        let mut content = if sites.is_empty() && !includes_sites(&initial, paths) {
            initial.clone()
        } else {
            global_options(&bind_default_site(&initial, http)?, http, https, &errors)?
        };
        if !sites.is_empty() && !includes_sites(&content, paths) {
            content.push_str(&format!(
                "\n# NiceEnv managed sites\n{}\n",
                import_line(paths)
            ));
        }
        crate::paths::write_with_backup_expected(
            &config,
            &content,
            &paths.backup(),
            Some(Some(previous.as_bytes())),
        )?;
        if previous != content {
            changed.push((config.clone(), previous, content));
        }
        adapt(resolved, &config)
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        for (path, before, after) in changed.into_iter().rev() {
            if let Err(failure) = crate::paths::write_with_backup_expected(
                &path,
                &before,
                &paths.backup(),
                Some(Some(after.as_bytes())),
            ) {
                failures.push(format!("{}: {failure}", path.display()));
            }
        }
        if !failures.is_empty() {
            return Err(AppError::new(
                "CADDY_CONFIG_RESTORE_FAILED",
                "Caddy 配置校验失败，部分文件未能恢复",
            )
            .with_detail(format!(
                "{}；{}",
                error.detail.unwrap_or(error.message),
                failures.join("；")
            )));
        }
        return Err(error);
    }
    result
}

pub(crate) fn adapt(
    resolved: &crate::generic::Resolved,
    config: &Path,
) -> Result<serde_json::Value> {
    let mut output = tempfile::tempfile()?;
    let mut errors = tempfile::tempfile()?;
    let mut command = platform::command(&resolved.bin);
    command
        .args(["adapt", "--adapter", "caddyfile", "--validate", "--config"])
        .arg(config)
        .current_dir(&resolved.root)
        .env("XDG_DATA_HOME", resolved.data.join("xdg"))
        .env("XDG_CONFIG_HOME", resolved.data.join("xdg"))
        .stdin(std::process::Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(errors.try_clone()?);
    let status =
        crate::dbadmin::wait_client(&mut command, std::time::Duration::from_secs(20), || {})?;
    if !status.success() {
        return Err(AppError::new("CADDY_CONFIG", "Caddy 配置校验失败")
            .with_detail(crate::dbadmin::read_output(&mut errors, 65536)?));
    }
    serde_json::from_str(&crate::dbadmin::read_output(&mut output, 16 * 1024 * 1024)?).map_err(
        |e| AppError::new("CADDY_CONFIG", "无法读取 Caddy 解析后的配置").with_detail(e.to_string()),
    )
}

/// 从原生适配后的配置确认域名、TLS 与回环监听；不使用待应用的设置拼访问地址。
pub(crate) fn endpoint(site: &Site, config: &serde_json::Value) -> Option<(String, u16)> {
    let domain = site
        .domains
        .iter()
        .find(|domain| !domain.starts_with("*."))
        .or_else(|| site.domains.first())?
        .replacen("*.", "www.", 1);
    fn has_host(value: &serde_json::Value, domain: &str) -> bool {
        match value {
            serde_json::Value::Object(object) => {
                object
                    .get("host")
                    .and_then(|v| v.as_array())
                    .is_some_and(|hosts| {
                        hosts.iter().filter_map(|h| h.as_str()).any(|host| {
                            host.eq_ignore_ascii_case(domain)
                                || host.strip_prefix("*.").is_some_and(|suffix| {
                                    domain.strip_suffix(suffix).is_some_and(|prefix| {
                                        prefix.ends_with('.')
                                            && !prefix[..prefix.len() - 1].contains('.')
                                    })
                                })
                        })
                    })
                    || object.values().any(|v| has_host(v, domain))
            }
            serde_json::Value::Array(array) => array.iter().any(|v| has_host(v, domain)),
            _ => false,
        }
    }
    for server in config.pointer("/apps/http/servers")?.as_object()?.values() {
        let tls = server
            .get("tls_connection_policies")
            .and_then(|v| v.as_array())
            .is_some_and(|v| !v.is_empty());
        if tls != site.https || !has_host(&server["routes"], &domain) {
            continue;
        }
        for listener in server["listen"]
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str())
        {
            let address = if listener.starts_with(':') {
                format!("127.0.0.1{listener}")
            } else {
                listener.into()
            };
            let Ok(address) = address.parse::<std::net::SocketAddr>() else {
                continue;
            };
            if !address.is_ipv4()
                || !(address.ip().is_unspecified() || address.ip().is_loopback())
                || address.port() == 0
            {
                continue;
            }
            let port = address.port();
            let scheme = if site.https { "https" } else { "http" };
            let suffix = if port == if site.https { 443 } else { 80 } {
                String::new()
            } else {
                format!(":{port}")
            };
            return Some((format!("{scheme}://{domain}{suffix}"), port));
        }
    }
    None
}

fn sync_site_ports(content: &str, site: &Site, http: u16, https: u16) -> Result<String> {
    let tokens = tokens(content)?;
    if !content.starts_with("# NiceEnv site:") {
        return Err(AppError::new(
            "CADDY_CONFIG",
            "Caddy 站点配置已改为自定义结构，无法自动同步监听端口",
        )
        .with_hint("请在站点编辑页保存配置后重试。"));
    }
    let mut edits = Vec::new();
    let mut start = 0;
    for (index, token) in tokens.iter().enumerate() {
        if token.depth == 0 && token.value == "{" {
            let first = tokens
                .get(start)
                .ok_or_else(|| AppError::new("CADDY_CONFIG", "站点配置为空"))?;
            let secure = first.value.starts_with("https://");
            if !secure && !first.value.starts_with("http://") {
                return Err(AppError::new(
                    "CADDY_CONFIG",
                    "站点地址必须显式指定 HTTP 或 HTTPS",
                ));
            }
            let scheme = if secure { "https" } else { "http" };
            let port = if secure { https } else { http };
            let addresses = site
                .domains
                .iter()
                .map(|domain| format!("{scheme}://{domain}:{port}"))
                .collect::<Vec<_>>()
                .join(", ");
            edits.push((first.start, token.start, format!("{addresses} ")));
        }
        if token.depth == 0 && token.value == "}" {
            start = index + 1;
        }
    }
    if edits.len() != if site.https { 2 } else { 1 } {
        return Err(AppError::new(
            "CADDY_CONFIG",
            "Caddy 站点块数量已变更，请在站点编辑页重新保存",
        ));
    }
    let mut updated = content.to_string();
    for (start, end, replacement) in edits.into_iter().rev() {
        updated.replace_range(start..end, &replacement);
    }
    Ok(updated)
}

fn cors(cors: &crate::model::SiteCors) -> Result<String> {
    let cors = crate::sitecors::normalize(cors)?;
    let origins = if cors.origins == ["*"] {
        ".+".into()
    } else {
        cors.origins
            .iter()
            .map(|origin| regex::escape(origin))
            .collect::<Vec<_>>()
            .join("|")
    };
    let origin_pattern = quoted(&format!("^(?:{origins})$"));
    let methods = quoted(&format!("^(?:{})$", cors.methods.join("|")));
    let mut out = format!("\t@cors_origin header_regexp Origin {origin_pattern}\n\t@cors_other not header_regexp Origin {origin_pattern}\n\t@cors_preflight {{\n\t\tmethod OPTIONS\n\t\theader_regexp Origin {origin_pattern}\n\t\theader_regexp Access-Control-Request-Method {methods}\n\t}}\n\t@cors_denied {{\n\t\tmethod OPTIONS\n\t\theader Access-Control-Request-Method *\n\t}}\n\troute {{\n");
    out.push_str("\t\theader +Vary Origin\n\t\theader @cors_other {\n\t\t\tdefer\n");
    for header in crate::sitecors::HEADERS {
        out.push_str(&format!("\t\t\t-{header}\n"));
    }
    out.push_str("\t\t}\n\t\theader @cors_origin {\n\t\t\tdefer\n");
    for (header, value) in crate::sitecors::HEADERS.iter().zip([
        if cors.origins == ["*"] {
            "*".into()
        } else {
            "{http.request.header.Origin}".into()
        },
        if cors.credentials {
            "true".into()
        } else {
            String::new()
        },
        cors.methods.join(", "),
        cors.allowed_headers.join(", "),
        cors.exposed_headers.join(", "),
        cors.max_age.to_string(),
    ]) {
        if value.is_empty() {
            out.push_str(&format!("\t\t\t-{header}\n"));
        } else {
            out.push_str(&format!("\t\t\t{header} {}\n", quoted(&value)));
        }
    }
    out.push_str("\t\t}\n\t\trespond @cors_preflight 204\n\t\trespond @cors_denied 403\n\t}\n");
    Ok(out)
}

fn proxy(
    paths: &Paths,
    target: &str,
    rule: Option<&crate::model::SiteProxyRule>,
) -> Result<String> {
    let target = crate::sites::proxy_url(target)?;
    let url =
        reqwest::Url::parse(&target).map_err(|_| AppError::new("BAD_PROXY", "代理目标无效"))?;
    let origin = url.origin().ascii_serialization();
    let base = url.path().trim_end_matches('/');
    let mut out = "\t\t\troute {\n".to_string();
    if let Some(rule) = rule.filter(|rule| rule.strip_prefix) {
        out.push_str(&format!("\t\t\turi strip_prefix {}\n", quoted(&rule.path)));
    }
    if !base.is_empty() {
        out.push_str(&format!(
            "\t\t\trewrite * {}\n",
            quoted(&format!("{base}{{uri}}"))
        ));
    }
    out.push_str(&format!(
        "\t\t\treverse_proxy {} {{\n\t\t\t\theader_up Host {{upstream_hostport}}\n",
        quoted(&origin)
    ));
    if url.scheme() == "https"
        && url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
        && paths.certs().join("ca.crt").is_file()
    {
        out.push_str(&format!("\t\t\t\ttransport http {{\n\t\t\t\t\ttls_trust_pool file {{\n\t\t\t\t\t\tpem_file {}\n\t\t\t\t\t}}\n\t\t\t\t}}\n", quoted(&nginx_path(&paths.certs().join("ca.crt")))));
    }
    if let Some(rule) = rule {
        let upstream_path = if rule.strip_prefix {
            base.to_string()
        } else {
            format!("{base}{}", rule.path)
        };
        let location = format!(
            "^{}{}(/.*)?$",
            regex::escape(&origin),
            regex::escape(&upstream_path)
        );
        out.push_str(&format!(
            "\t\t\t\theader_down Location {} {}\n",
            quoted(&location),
            quoted(&format!("{}${{1}}", rule.path))
        ));
        // RE2 不支持 lookahead：捕获分隔符并放回，Cookie 子路径不会被提升到站点根路径。
        let cookie = if upstream_path.is_empty() {
            "(?i)(;[ \\t]*path=)(/[^;]*)(;|$)".into()
        } else {
            format!(
                "(?i)(;[ \\t]*path=){}(/[^;]*)?(;|$)",
                regex::escape(&upstream_path)
            )
        };
        out.push_str(&format!(
            "\t\t\t\theader_down Set-Cookie {} {}\n",
            quoted(&cookie),
            quoted(&format!("${{1}}{}${{2}}${{3}}", rule.path))
        ));
    }
    out.push_str("\t\t\t}\n\t\t\t}\n");
    Ok(out)
}

pub(crate) fn render(
    site: &Site,
    paths: &Paths,
    http: u16,
    https: u16,
    php_port: Option<u16>,
) -> Result<String> {
    let mut output = render_block(site, paths, http, false, php_port)?;
    if site.https {
        output.push_str(&render_block(site, paths, https, true, php_port)?);
    }
    Ok(output)
}

fn render_block(
    site: &Site,
    paths: &Paths,
    port: u16,
    secure: bool,
    php_port: Option<u16>,
) -> Result<String> {
    let scheme = if secure { "https" } else { "http" };
    let addresses = site
        .domains
        .iter()
        .map(|domain| format!("{scheme}://{domain}:{port}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out = format!(
        "# NiceEnv site: {}\n{addresses} {{\n\tbind 127.0.0.1\n",
        site.id
    );
    if secure {
        let (cert, key) =
            crate::configgen::site_certificate_files(site, &paths.certs().join("sites"));
        out.push_str(&format!(
            "\ttls {} {}\n",
            quoted(&nginx_path(&cert)),
            quoted(&nginx_path(&key))
        ));
    }
    out.push_str(&format!(
        "\tlog niceenv_{}_{scheme} {{\n\t\toutput file {}\n\t}}\n",
        site.id,
        quoted(&nginx_path(
            &paths
                .logs()
                .join("caddy")
                .join(format!("{}.access.log", site.id))
        ))
    ));
    out.push_str(&format!(
        "\tlog niceenv_{}_{scheme}_errors {{\n\t\tno_hostname\n\t\toutput file {}\n\t}}\n",
        site.id,
        quoted(&nginx_path(
            &paths
                .logs()
                .join("caddy")
                .join(format!("{}.error.log", site.id))
        ))
    ));
    if site.runtime.kind != SiteKind::Redirect {
        out.push_str(&format!(
            "\troot * {}\n",
            quoted(&nginx_path(Path::new(&site.root_dir)))
        ));
    }
    // 显式 route 顺序：跨域 → 私有文件保护 → 路径代理 → 站点主体。
    out.push_str("\troute {\n");
    if let Some(policy) = &site.runtime.cors {
        out.push_str(&cors(policy)?);
    }
    out.push_str("\t\t@private {\n\t\t\tpath_regexp private (^|/)\\.[^/]+\n\t\t\tnot path /.well-known/*\n\t\t}\n\t\t@nested_private path_regexp nested_private ^/\\.well-known/(.*/)?\\.[^/]+\n\t\trespond @private 403\n\t\trespond @nested_private 403\n");
    let mut rules = crate::siteproxy::normalize(&site.runtime)?;
    rules.sort_by(|a, b| {
        b.path
            .len()
            .cmp(&a.path.len())
            .then_with(|| a.path.cmp(&b.path))
    });
    for (index, rule) in rules.iter().enumerate() {
        out.push_str(&format!(
            "\t\t@proxy_{index} path {} {}\n\t\thandle @proxy_{index} {{\n",
            quoted(&rule.path),
            quoted(&format!("{}/*", rule.path))
        ));
        out.push_str(&proxy(paths, &rule.target, Some(rule))?);
        out.push_str("\t\t}\n");
    }
    out.push_str("\t\thandle {\n");
    match site.runtime.kind {
        SiteKind::Redirect => {
            let redirect = site
                .runtime
                .redirect
                .as_ref()
                .ok_or_else(|| AppError::new("BAD_REDIRECT", "请配置跳转目标"))?;
            let target = crate::sites::redirect_url(redirect, &site.domains)?;
            let target = if redirect.preserve_path {
                format!("{}{{uri}}", target.trim_end_matches('/'))
            } else {
                target
            };
            out.push_str(&format!(
                "\t\t\tredir {} {}\n",
                quoted(&target),
                redirect.status
            ));
        }
        SiteKind::Php | SiteKind::Static => {
            if let Some(custom) = &site.runtime.custom_rewrite {
                if custom.server != "caddy" {
                    return Err(AppError::new(
                        "BAD_REWRITE",
                        "伪静态模板与 Caddy 不兼容，请重新选择",
                    ));
                }
                validate_rewrite(&custom.content)?;
                out.push_str(&custom.content);
                out.push('\n');
            } else {
                match site.rewrite {
                    RewritePreset::SpaFallback => out.push_str("\t\t\ttry_files {path} {path}/ /index.html\n"),
                    RewritePreset::NextExport => out.push_str("\t\t\ttry_files {path} {path}.html {path}/\n"),
                    RewritePreset::Thinkphp | RewritePreset::Cakephp => {
                        let key = if matches!(site.rewrite, RewritePreset::Thinkphp) { "s" } else { "url" };
                        out.push_str(&format!("\t\t\t@missing not file\n\t\t\trewrite @missing /index.php?{key}={{path}}&{{query}}\n"));
                    }
                    RewritePreset::Wordpress => out.push_str("\t\t\tredir /wp-admin /wp-admin/ 301\n"),
                    RewritePreset::Codeigniter => out.push_str("\t\t\t@framework_private path /app/* /system/* /writable/*\n\t\t\trespond @framework_private 403\n"),
                    RewritePreset::Drupal => out.push_str("\t\t\t@framework_private path_regexp framework (?i)\\.(engine|inc|info|install|module|profile|po|sh|.*sql|theme|tpl(\\.php)?|xtmpl)$\n\t\t\trespond @framework_private 403\n"),
                    _ => {}
                }
            }
            if site.runtime.kind == SiteKind::Php {
                let port = php_port.unwrap_or(1);
                out.push_str(&format!(
                    "\t\t\tphp_fastcgi 127.0.0.1:{port} {{\n\t\t\t\tcapture_stderr\n\t\t\t}}\n"
                ));
            } else {
                out.push_str("\t\t\t@php_source path_regexp php_source (?i)\\.php(/|$)\n\t\t\trespond @php_source 403\n");
            }
            out.push_str("\t\t\tfile_server\n");
        }
        _ => out.push_str(&proxy(
            paths,
            site.runtime
                .proxy_target
                .as_deref()
                .unwrap_or("127.0.0.1:1"),
            None,
        )?),
    }
    out.push_str("\t\t}\n\t}\n");
    out.push_str(&format!("\thandle_errors {{\n\t\troute {{\n\t\tlog_name niceenv_{id}_{scheme} niceenv_{id}_{scheme}_errors\n\t\tlog_append error_message {{err.message}}\n\t\tlog_append error_trace {{err.trace}}\n", id = site.id));
    if let Some(policy) = &site.runtime.cors {
        out.push_str(&cors(policy)?.replace("@cors_", "@error_cors_"));
    }
    if site.runtime.kind == SiteKind::Static
        && site.runtime.custom_rewrite.is_none()
        && matches!(site.rewrite, RewritePreset::NextExport)
    {
        // Next 导出的自定义错误页保留 404 状态；缺少文件时使用通用错误响应。
        out.push_str("\t\t@next404 {\n\t\t\texpression {err.status_code} == 404\n\t\t\tfile /404.html\n\t\t}\n\t\thandle @next404 {\n\t\t\trewrite * /404.html\n\t\t\tfile_server {\n\t\t\t\tstatus 404\n\t\t\t}\n\t\t}\n\t\thandle {\n\t\t\trespond \"{err.status_code} {err.status_text}\" {err.status_code}\n\t\t}\n");
    } else {
        out.push_str("\t\trespond \"{err.status_code} {err.status_text}\" {err.status_code}\n");
    }
    out.push_str("\t\t}\n\t}\n");
    out.push_str("}\n");
    Ok(out)
}

pub(crate) fn validate_rewrite(source: &str) -> Result<()> {
    let parsed = tokens(source)?;
    for token in parsed.iter().filter(|t| t.depth == 0) {
        let beginning = source[..token.start].rfind('\n').map_or(0, |i| i + 1);
        if !source[beginning..token.start].trim().is_empty() || token.value == "}" {
            continue;
        }
        if !matches!(
            token.value.as_str(),
            "try_files"
                | "rewrite"
                | "redir"
                | "respond"
                | "uri"
                | "handle"
                | "handle_path"
                | "route"
        ) && !token.value.starts_with('@')
        {
            return Err(AppError::new(
                "BAD_REWRITE",
                "Caddy 伪静态仅允许匹配器、路径重写、跳转和响应规则",
            ));
        }
    }
    if parsed.iter().any(|t| {
        matches!(
            t.value.as_str(),
            "import" | "tls" | "bind" | "root" | "reverse_proxy"
        ) || t.value.contains("{$")
    }) {
        return Err(AppError::new(
            "BAD_REWRITE",
            "伪静态规则不能导入配置或修改站点监听、根目录、代理与证书",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn caddy_preserves_custom_main_and_validates_block_boundaries() {
        let source = "# user comment\n{\n admin off\n storage file_system C:/data/storage\n}\nhttp://:8080 {\n header X-Preserved yes\n file_server\n}\n";
        let first = global_options(source, 31000, 31001, "").unwrap();
        assert!(first.contains("header X-Preserved yes"));
        assert!(first.contains("storage file_system C:/data/storage"));
        assert_eq!(global_options(&first, 31000, 31001, "").unwrap(), first);
        let custom_pki = "{\n pki {\n  ca team {\n   name TeamCA\n  }\n }\n}\n";
        let managed = global_options(custom_pki, 31000, 31001, "").unwrap();
        assert!(managed.contains("name TeamCA"));
        assert!(managed.contains("ca local {"));
        assert_eq!(global_options(&managed, 31000, 31001, "").unwrap(), managed);
        for invalid in [
            "}\nhttp://evil.test {\n",
            "import external.conf",
            "route {\n import external.conf\n}",
            "rewrite * {$SECRET}",
        ] {
            assert!(validate_rewrite(invalid).is_err(), "{invalid}");
        }
        validate_rewrite("@missing not file\nrewrite @missing /index.php?{query}\n").unwrap();
    }

    #[test]
    #[ignore = "requires NSB_VERIFY_CADDY and NSB_SKIP_HOSTS=1; isolated native Caddy lifecycle"]
    fn native_caddy_sites_lifecycle() {
        assert_eq!(std::env::var("NSB_SKIP_HOSTS").as_deref(), Ok("1"));
        let executable =
            PathBuf::from(std::env::var_os("NSB_VERIFY_CADDY").expect("NSB_VERIFY_CADDY"));
        let temp = tempfile::tempdir().unwrap();
        let state = crate::CoreState::init(
            Some(temp.path().join("caddy sites with spaces")),
            Arc::new(|_| {}),
        )
        .unwrap();
        state
            .store
            .upsert_installed(&crate::model::InstalledPackage {
                id: "caddy".into(),
                version: "2.11.4".into(),
                category: "web-server".into(),
                install_path: executable.parent().unwrap().to_string_lossy().into(),
                config_path: String::new(),
                installed_at: 1,
            })
            .unwrap();
        let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let https = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = http.local_addr().unwrap().port();
        let tls_port = https.local_addr().unwrap().port();
        state.store.set_port_override("caddy", Some(port)).unwrap();
        state
            .store
            .set_port_override("caddyHttps", Some(tls_port))
            .unwrap();
        let project = temp.path().join("project with spaces");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("index.html"), "caddy-site-front").unwrap();
        std::fs::write(project.join(".env"), "PRIVATE=secret").unwrap();
        std::fs::write(project.join("secret.php"), "<?php echo 'secret';").unwrap();
        let input: crate::model::CreateSiteInput = serde_json::from_value(serde_json::json!({
            "name":"Caddy native", "domains":["127.0.0.1","caddy.test"], "rootDir":project, "https":true, "rewrite":"none", "template":"none", "writeEnvExample":false,
            "runtime":{"kind":"static","webServer":"caddy","cors":{"origins":["https://frontend.test"],"methods":["GET","POST","OPTIONS"],"allowedHeaders":["Content-Type"],"exposedHeaders":[],"credentials":true,"maxAge":600}}
        })).unwrap();
        struct Cleanup(Arc<crate::CoreState>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = self.0.stop_service("caddy");
                let _ = self.0.stop_service("nginx");
            }
        }
        let _cleanup = Cleanup(state.clone());
        drop((http, https));
        let mut site =
            crate::sites::create(&input, &state.paths, &state.store, &state.manager).unwrap();
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .danger_accept_invalid_certs(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let mut address = format!("http://127.0.0.1:{port}");
        for origin in [address.clone(), format!("https://127.0.0.1:{tls_port}")] {
            let response = client
                .get(&origin)
                .header("Origin", "https://frontend.test")
                .send()
                .unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(
                response
                    .headers()
                    .get("access-control-allow-origin")
                    .and_then(|v| v.to_str().ok()),
                Some("https://frontend.test")
            );
            assert_eq!(response.text().unwrap(), "caddy-site-front");
        }
        for path in ["/.env", "/secret.php"] {
            assert_eq!(
                client
                    .get(format!("{address}{path}"))
                    .send()
                    .unwrap()
                    .status(),
                403
            );
        }
        let preflight = client
            .request(reqwest::Method::OPTIONS, &address)
            .header("Origin", "https://frontend.test")
            .header("Access-Control-Request-Method", "POST")
            .send()
            .unwrap();
        assert_eq!(preflight.status(), 204);
        assert_eq!(
            client
                .request(reqwest::Method::OPTIONS, &address)
                .header("Origin", "https://denied.test")
                .header("Access-Control-Request-Method", "POST")
                .send()
                .unwrap()
                .status(),
            403
        );
        site.status = "running".into();
        assert_eq!(
            crate::sites::running_url(&state.paths, site.clone(), &state.manager).unwrap(),
            format!("https://127.0.0.1:{tls_port}")
        );
        let main = config_path(&state.paths, &state.store).unwrap();
        let resolved = crate::generic::resolve(&state.store, &state.paths, "caddy").unwrap();
        let adapted = adapt(&resolved, &main).unwrap();
        assert_eq!(
            adapted.pointer("/apps/pki/certificate_authorities/local/install_trust"),
            Some(&serde_json::Value::Bool(false))
        );
        let preset_file = temp.path().join("presets.Caddyfile");
        let custom_pki = global_options(
            "{\n admin off\n pki {\n  ca team {\n   name TeamCA\n  }\n }\n}\n",
            port,
            tls_port,
            "",
        )
        .unwrap();
        for preset in [
            RewritePreset::None,
            RewritePreset::Laravel,
            RewritePreset::Thinkphp,
            RewritePreset::Wordpress,
            RewritePreset::SpaFallback,
            RewritePreset::NextExport,
            RewritePreset::Symfony,
            RewritePreset::Yii2,
            RewritePreset::Codeigniter,
            RewritePreset::Cakephp,
            RewritePreset::Drupal,
            RewritePreset::Joomla,
        ] {
            let mut candidate = site.clone();
            candidate.rewrite = preset;
            std::fs::write(
                &preset_file,
                format!(
                    "{custom_pki}{}",
                    render(&candidate, &state.paths, port, tls_port, Some(1)).unwrap()
                ),
            )
            .unwrap();
            let config = adapt(&resolved, &preset_file).unwrap();
            for ca in ["local", "team"] {
                assert_eq!(
                    config.pointer(&format!(
                        "/apps/pki/certificate_authorities/{ca}/install_trust"
                    )),
                    Some(&serde_json::Value::Bool(false))
                );
            }
        }
        std::fs::write(project.join("about.html"), "next-about").unwrap();
        std::fs::write(project.join("404.html"), "next-custom-404").unwrap();
        site.rewrite = RewritePreset::NextExport;
        crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
        assert_eq!(
            client
                .get(format!("{address}/about"))
                .send()
                .unwrap()
                .text()
                .unwrap(),
            "next-about"
        );
        let missing = client
            .get(format!("{address}/missing-page"))
            .header("Origin", "https://frontend.test")
            .send()
            .unwrap();
        assert_eq!(missing.status(), 404);
        assert_eq!(
            missing.headers()["access-control-allow-origin"],
            "https://frontend.test"
        );
        assert_eq!(missing.text().unwrap(), "next-custom-404");
        site.rewrite = RewritePreset::SpaFallback;
        crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
        assert_eq!(
            client
                .get(format!("{address}/client-route"))
                .send()
                .unwrap()
                .text()
                .unwrap(),
            "caddy-site-front"
        );
        site.rewrite = RewritePreset::None;
        crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
        let before = std::fs::read_to_string(&main).unwrap();
        let validation = crate::cfgeditor::validate_selected(
            &state.paths,
            &state.store,
            "caddy-conf@2.11.4",
            "{\n unknown_directive on\n}\n",
        )
        .unwrap();
        assert!(!validation.ok);
        assert_eq!(std::fs::read_to_string(&main).unwrap(), before);
        let vhost = state
            .paths
            .caddy_sites_dir()
            .join(format!("{}.conf", site.id));
        let original_vhost = std::fs::read(&vhost).unwrap();
        site.runtime.custom_rewrite = Some(crate::model::CustomRewrite {
            name: "invalid fixture".into(),
            server: "caddy".into(),
            content: "rewrite\n".into(),
        });
        assert!(crate::sites::update(&site, &state.paths, &state.store, &state.manager).is_err());
        assert_eq!(std::fs::read(&vhost).unwrap(), original_vhost);
        assert_eq!(
            client.get(&address).send().unwrap().text().unwrap(),
            "caddy-site-front"
        );
        site.runtime.custom_rewrite = None;
        let custom = std::fs::read_to_string(&main)
            .unwrap()
            .replace("file_server", "header X-Custom preserved\n\tfile_server");
        std::fs::write(&main, format!("# keep my Caddy config\n{custom}")).unwrap();
        state.stop_service("caddy").unwrap();
        let occupied = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        state.store.set_setting("autoFallbackPort", "true").unwrap();
        state.start_service("caddy").unwrap();
        let actual_port = state.manager.snapshot("caddy").unwrap().port.unwrap();
        assert_ne!(actual_port, port);
        assert_ne!(actual_port, tls_port);
        address = format!("http://127.0.0.1:{actual_port}");
        assert_eq!(
            client.get(&address).send().unwrap().text().unwrap(),
            "caddy-site-front"
        );
        drop(occupied);
        if let Some(root) = std::env::var_os("NSB_NGINX_ROOT") {
            let root = PathBuf::from(root);
            let version = root
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .strip_prefix("nginx-")
                .unwrap();
            state
                .store
                .upsert_installed(&crate::model::InstalledPackage {
                    id: "nginx".into(),
                    version: version.into(),
                    category: "web-server".into(),
                    install_path: root.parent().unwrap().to_string_lossy().into(),
                    config_path: String::new(),
                    installed_at: 1,
                })
                .unwrap();
            let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let https = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let nginx_port = http.local_addr().unwrap().port();
            state
                .store
                .set_port_override("http", Some(nginx_port))
                .unwrap();
            state
                .store
                .set_port_override("https", Some(https.local_addr().unwrap().port()))
                .unwrap();
            drop((http, https));
            site.runtime.web_server = "nginx".into();
            crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
            assert_eq!(
                client
                    .get(format!("http://127.0.0.1:{nginx_port}"))
                    .send()
                    .unwrap()
                    .text()
                    .unwrap(),
                "caddy-site-front"
            );
            assert!(!vhost.exists());
            site.runtime.web_server = "caddy".into();
            crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
            assert_eq!(
                client.get(&address).send().unwrap().text().unwrap(),
                "caddy-site-front"
            );
            assert!(!state
                .paths
                .nginx_sites_dir()
                .join(format!("{}.conf", site.id))
                .exists());
        }
        site.runtime.kind = SiteKind::ReverseProxy;
        site.runtime.proxy_target = Some("http://127.0.0.1:1".into());
        crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
        assert_eq!(client.get(&address).send().unwrap().status(), 502);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let access = state
            .tail_logs_checked(&format!("site:{}", site.id), 30)
            .unwrap();
        assert!(!access.is_empty());
        let errors = state
            .tail_logs_checked(&format!("site-error:{}", site.id), 30)
            .unwrap();
        assert!(
            !errors.is_empty(),
            "missing per-site runtime errors: {:?}",
            state.tail_logs_checked("caddy", 50).unwrap()
        );
        assert!(std::fs::read_to_string(main)
            .unwrap()
            .contains("X-Custom preserved"));
        assert!(state
            .tail_logs_checked("caddy", 500)
            .unwrap()
            .iter()
            .all(|line| !line.line.contains("installing root certificate")));
        crate::sites::stop_site(&site.id, &state.paths, &state.store, &state.manager).unwrap();
        assert_eq!(crate::sites::derive_status(&state.paths, &site), "stopped");
        crate::sites::start_site(&site.id, &state.paths, &state.store, &state.manager).unwrap();
        crate::sites::delete(
            &site.id,
            false,
            false,
            &state.paths,
            &state.store,
            &state.manager,
        )
        .unwrap();
        assert!(!state
            .paths
            .caddy_sites_dir()
            .join(format!("{}.conf", site.id))
            .exists());
        println!("Caddy static/HTTPS/CORS/private files/real endpoint/custom config/access and error logs/stop/start/delete passed");
    }
}
