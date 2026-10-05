//! 服务配置生成：首次写入默认值，后续仅同步运行所需的托管项，保留用户配置。

use crate::error::{AppError, Result};
use crate::model::{RewritePreset, Site};
use crate::paths::{portable_path_text, quoted_config_path, write_with_backup, Paths};
use std::path::Path;

fn previous_config(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(AppError::io("读取已有配置", e)),
    }
}

fn publish_config(
    paths: &Paths,
    key: &str,
    path: &Path,
    content: &str,
    previous: Option<&str>,
) -> Result<()> {
    crate::cfgeditor::write_generated_config(paths, key, path, content, previous)
}

/* ================= nginx ================= */

/// php-cgi 池端口：base..base+3（4 worker）
pub const PHP_POOL_WORKERS: u16 = 4;

pub fn nginx_upstream_name(php_version: &str) -> String {
    format!("nsb_php_{}", php_version.replace('.', "_"))
}

pub(crate) fn nginx_include_pattern(path: &Path, sites_glob: bool) -> String {
    let text = portable_path_text(path);
    // Nginx 遇到 *?[ 会再交给系统 glob。Unix 的目录字符要先经过这一层转义，
    // 再经过配置字符串转义；否则合法的反斜杠或方括号目录会静默漏掉站点。
    let mut pattern = if cfg!(unix) && (sites_glob || text.contains(['*', '?', '['])) {
        crate::paths::escaped_glob_path(path)
    } else {
        text
    };
    if sites_glob {
        pattern.push_str("/*.conf");
    }
    pattern
}

fn quoted_nginx_include(path: &Path, sites_glob: bool) -> String {
    nginx_include_pattern(path, sites_glob)
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[derive(Debug)]
pub(crate) struct NginxDirective {
    start: usize,
    end: usize,
    pub(crate) words: Vec<String>,
    pub(crate) children: Vec<NginxDirective>,
}

struct NginxToken {
    start: usize,
    end: usize,
    word: String,
    delimiter: Option<u8>,
}

fn nginx_structure_error() -> AppError {
    AppError::new(
        "CONFIG_STRUCTURE",
        "无法安全更新 Nginx 的托管配置，原文件已保留",
    )
    .with_hint("请先在配置编辑器校验语法，并保留一个顶层 http 配置块")
}

/// 只解析指令边界，不重新序列化用户配置。引号、注释、转义、${变量} 均不能当作块边界。
fn nginx_tokens(content: &str) -> Result<Vec<NginxToken>> {
    let bytes = content.as_bytes();
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes[pos].is_ascii_whitespace() {
            pos += 1;
            continue;
        }
        if bytes[pos] == b'#' {
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        let start = pos;
        if b"{};".contains(&bytes[pos]) {
            tokens.push(NginxToken {
                start,
                end: pos + 1,
                word: String::new(),
                delimiter: Some(bytes[pos]),
            });
            pos += 1;
            continue;
        }
        let mut word = Vec::new();
        let mut quote = None;
        while pos < bytes.len() {
            let byte = bytes[pos];
            if byte == b'\\' {
                pos += 1;
                if pos == bytes.len() {
                    return Err(nginx_structure_error());
                }
                // 与 ngx_conf_read_token 一致：未知转义保留反斜杠，不能吞掉路径字符。
                match bytes[pos] {
                    b't' => word.push(b'\t'),
                    b'r' => word.push(b'\r'),
                    b'n' => word.push(b'\n'),
                    b'\\' | b'\'' | b'"' => word.push(bytes[pos]),
                    other => word.extend_from_slice(&[b'\\', other]),
                }
                pos += 1;
                continue;
            }
            if let Some(q) = quote {
                if byte == q {
                    quote = None;
                } else {
                    word.push(byte);
                }
                pos += 1;
                continue;
            }
            if byte == b'\'' || byte == b'"' {
                quote = Some(byte);
                pos += 1;
                continue;
            }
            if byte == b'$' && bytes.get(pos + 1) == Some(&b'{') {
                while pos < bytes.len() && bytes[pos] != b'}' {
                    word.push(bytes[pos]);
                    pos += 1;
                }
                if pos == bytes.len() {
                    return Err(nginx_structure_error());
                }
                word.push(b'}');
                pos += 1;
                continue;
            }
            if byte.is_ascii_whitespace() || b"{};#".contains(&byte) {
                break;
            }
            word.push(byte);
            pos += 1;
        }
        if quote.is_some() {
            return Err(nginx_structure_error());
        }
        tokens.push(NginxToken {
            start,
            end: pos,
            word: String::from_utf8(word).map_err(|_| nginx_structure_error())?,
            delimiter: None,
        });
    }
    Ok(tokens)
}

pub(crate) fn nginx_directives(content: &str) -> Result<Vec<NginxDirective>> {
    let tokens = nginx_tokens(content)?;
    fn parse(tokens: &[NginxToken], pos: &mut usize, depth: usize) -> Result<Vec<NginxDirective>> {
        if depth > 128 {
            return Err(nginx_structure_error());
        }
        let mut nodes = Vec::new();
        while *pos < tokens.len() {
            if tokens[*pos].delimiter == Some(b'}') {
                if depth == 0 {
                    return Err(nginx_structure_error());
                }
                *pos += 1;
                return Ok(nodes);
            }
            let start = tokens[*pos].start;
            let mut words = Vec::new();
            while *pos < tokens.len() && tokens[*pos].delimiter.is_none() {
                words.push(tokens[*pos].word.clone());
                *pos += 1;
            }
            if words.is_empty() || *pos == tokens.len() {
                return Err(nginx_structure_error());
            }
            let delimiter = tokens[*pos].delimiter;
            *pos += 1;
            let children = match delimiter {
                Some(b';') => Vec::new(),
                Some(b'{') => parse(tokens, pos, depth + 1)?,
                _ => return Err(nginx_structure_error()),
            };
            nodes.push(NginxDirective {
                start,
                end: tokens[*pos - 1].end,
                words,
                children,
            });
        }
        if depth > 0 {
            return Err(nginx_structure_error());
        }
        Ok(nodes)
    }
    parse(&tokens, &mut 0, 0)
}

pub(crate) fn rebase_posix_glob_pattern(
    value: &str,
    rebase: &crate::paths::DataPathRebase,
) -> Result<String> {
    if value.contains(['*', '?', '[']) {
        let rebased = rebase.config_value(value, crate::paths::escaped_posix_glob_text)?;
        if rebased != value && !rebased.contains(['*', '?', '[']) {
            // 新路径不再进入系统 glob 时，撤去仅供 glob 使用的一层转义。
            let mut literal = String::new();
            let mut chars = rebased.chars();
            while let Some(character) = chars.next() {
                literal.push(if character == '\\' {
                    chars.next().unwrap_or(character)
                } else {
                    character
                });
            }
            Ok(literal)
        } else {
            Ok(rebased)
        }
    } else {
        let rebased = rebase.config_value(value, str::to_owned)?;
        // 新目录第一次引入 glob 字符时，整条路径（含原后缀）都要引用。
        if rebased != value && rebased.contains(['*', '?', '[']) {
            Ok(crate::paths::escaped_posix_glob_text(&rebased))
        } else {
            Ok(rebased)
        }
    }
}

pub(crate) fn rebase_nginx_config(
    content: &str,
    rebase: &crate::paths::DataPathRebase,
) -> Result<String> {
    let mut directive = None;
    let mut argument = 0;
    let mut first_argument = String::new();
    let mut changes = Vec::new();
    let mut variables = Vec::new();
    let mut path_values = Vec::new();
    let mut has_mapping = false;
    let mut data_blocks = Vec::new();
    for token in nginx_tokens(content)? {
        if let Some(delimiter) = token.delimiter {
            if delimiter == b'{' {
                data_blocks.push(
                    data_blocks.last().copied().unwrap_or(false)
                        || matches!(
                            directive.as_deref(),
                            Some("map" | "geo" | "split_clients" | "types")
                        ),
                );
            } else if delimiter == b'}' {
                data_blocks.pop();
            }
            directive = None;
            continue;
        }
        let Some(name) = directive.as_deref() else {
            has_mapping |= matches!(token.word.as_str(), "map" | "geo" | "split_clients");
            directive = Some(token.word);
            argument = 0;
            first_argument.clear();
            continue;
        };
        argument += 1;
        if argument == 1 {
            first_argument.clone_from(&token.word);
        }
        if data_blocks.last() == Some(&true) && name != "include" {
            continue;
        }
        if name == "set" && argument == 2 {
            variables.push((
                first_argument.trim_start_matches('$').to_string(),
                token.word.clone(),
            ));
        }
        // URL、响应正文、请求头和应用参数都可能恰好像旧目录，不能按文本猜路径。
        let path_argument = match name {
            "include"
            | "root"
            | "alias"
            | "pid"
            | "lock_file"
            | "working_directory"
            | "load_module"
            | "error_log"
            | "access_log"
            | "auth_basic_user_file"
            | "client_body_temp_path"
            | "proxy_temp_path"
            | "fastcgi_temp_path"
            | "uwsgi_temp_path"
            | "scgi_temp_path"
            | "proxy_cache_path"
            | "fastcgi_cache_path"
            | "uwsgi_cache_path"
            | "scgi_cache_path"
            | "proxy_store"
            | "fastcgi_store"
            | "uwsgi_store"
            | "scgi_store"
            | "ssl_certificate"
            | "ssl_certificate_key"
            | "ssl_client_certificate"
            | "ssl_trusted_certificate"
            | "ssl_crl"
            | "ssl_dhparam"
            | "ssl_password_file"
            | "ssl_session_ticket_key"
            | "proxy_ssl_certificate"
            | "proxy_ssl_certificate_key"
            | "proxy_ssl_trusted_certificate"
            | "proxy_ssl_crl"
            | "proxy_ssl_password_file"
            | "grpc_ssl_certificate"
            | "grpc_ssl_certificate_key"
            | "grpc_ssl_trusted_certificate"
            | "grpc_ssl_crl"
            | "grpc_ssl_password_file"
            | "uwsgi_ssl_certificate"
            | "uwsgi_ssl_certificate_key"
            | "uwsgi_ssl_trusted_certificate"
            | "uwsgi_ssl_crl"
            | "uwsgi_ssl_password_file" => argument == 1,
            "index" => true,
            "fastcgi_param" | "uwsgi_param" | "scgi_param" => {
                argument == 2
                    && matches!(
                        first_argument.as_str(),
                        "SCRIPT_FILENAME" | "DOCUMENT_ROOT" | "PATH_TRANSLATED"
                    )
            }
            _ => false,
        };
        if !path_argument {
            continue;
        }
        path_values.push(token.word.clone());
        let value = if name == "include" && cfg!(unix) {
            rebase_posix_glob_pattern(&token.word, rebase)?
        } else {
            rebase.config_value(&token.word, str::to_owned)?
        };
        if value != token.word {
            let quoted = value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            changes.push((token.start, token.end, format!("\"{quoted}\"")));
        }
    }
    fn collect_maps(nodes: &[NginxDirective], variables: &mut Vec<(String, String)>) {
        for node in nodes {
            if matches!(node.words[0].as_str(), "map" | "geo" | "split_clients") {
                if let Some(name) = node.words.last().and_then(|word| word.strip_prefix('$')) {
                    for entry in &node.children {
                        if let Some(value) = entry.words.last() {
                            variables.push((name.to_string(), value.clone()));
                        }
                    }
                }
            }
            collect_maps(&node.children, variables);
        }
    }
    if has_mapping {
        collect_maps(&nginx_directives(content)?, &mut variables);
    }
    validate_rebase_variables(&variables, &path_values, rebase, "Nginx", |_| true)?;
    let mut output = content.to_string();
    for (start, end, value) in changes.into_iter().rev() {
        output.replace_range(start..end, &value);
    }
    Ok(output)
}

// 定义可同时用于文件路径和业务数据，不能全局改写；跨文件定义由迁移入口合并检查。
fn validate_rebase_variables(
    definitions: &[(String, String)],
    paths: &[String],
    rebase: &crate::paths::DataPathRebase,
    service: &str,
    seed: impl Fn(&str) -> bool,
) -> Result<()> {
    let references = regex::Regex::new(if service == "Apache" {
        r"\$\{([^}]+)\}"
    } else {
        r"\$\{([A-Za-z0-9_]+)\}|\$([A-Za-z0-9_]+)"
    })
    .unwrap();
    let mut affected = std::collections::HashSet::new();
    for (name, value) in definitions {
        if seed(name) && rebase.config_value(value, str::to_owned)? != *value {
            affected.insert(name.as_str());
        }
    }
    let uses_affected = |value: &str, affected: &std::collections::HashSet<&str>| {
        references.captures_iter(value).any(|capture| {
            affected.contains(capture.get(1).or_else(|| capture.get(2)).unwrap().as_str())
        })
    };
    loop {
        let before = affected.len();
        for (name, value) in definitions {
            if uses_affected(value, &affected) {
                affected.insert(name.as_str());
            }
        }
        if before == affected.len() {
            break;
        }
    }
    if paths.iter().any(|value| uses_affected(value, &affected)) {
        return Err(AppError::new(
            "DATA_DIR_CONFIG_VARIABLE",
            format!("{service} 配置通过自定义变量引用旧数据目录，无法安全自动迁移"),
        )
        .with_hint("请先在文件路径指令中填入完整路径，并将请求头、密码等业务变量与托管目录变量分开后重试。当前数据目录和原配置未修改。"));
    }
    Ok(())
}

fn directive_span(content: &str, node: &NginxDirective) -> std::ops::Range<usize> {
    let start = content[..node.start].rfind('\n').map_or(0, |p| p + 1);
    let end = content[node.end..]
        .find('\n')
        .map_or(content.len(), |p| node.end + p + 1);
    if content[start..node.start].trim().is_empty() && content[node.end..end].trim().is_empty() {
        start..end
    } else {
        node.start..node.end
    }
}

fn sync_nginx_config(current: &str, generated: &str, paths: &Paths) -> Result<String> {
    let old = nginx_directives(current)?;
    let new = nginx_directives(generated)?;
    let http = |nodes: &Vec<NginxDirective>| nodes.iter().position(|node| node.words[0] == "http");
    if old.iter().filter(|node| node.words[0] == "http").count() != 1 {
        return Err(nginx_structure_error());
    }
    let old_http = &old[http(&old).ok_or_else(nginx_structure_error)?];
    let new_http = &new[http(&new).ok_or_else(nginx_structure_error)?];
    if current.as_bytes()[old_http.end - 1] != b'}' {
        return Err(nginx_structure_error());
    }
    let sites = nginx_include_pattern(&paths.nginx_sites_dir(), true);
    let normalize_path = |value: &str| portable_path_text(Path::new(value));
    let managed_sites = normalize_path(&sites);
    let same_path = |left: &str, right: &str| {
        if cfg!(windows) {
            left.eq_ignore_ascii_case(right)
        } else {
            left == right
        }
    };
    let class = |node: &NginxDirective| -> Option<usize> {
        match node.words[0].as_str() {
            "include"
                if node
                    .words
                    .get(1)
                    .is_some_and(|p| p.ends_with("/conf/mime.types")) =>
            {
                Some(1)
            }
            "server"
                if node.children.iter().any(|n| {
                    n.words[0] == "server_name" && n.words.get(1).is_some_and(|s| s == "_")
                }) =>
            {
                Some(2)
            }
            "upstream" if node.words.get(1).is_some_and(|s| s.starts_with("nsb_php_")) => Some(3),
            "include"
                if node
                    .words
                    .get(1)
                    .is_some_and(|path| same_path(&normalize_path(path), &managed_sites)) =>
            {
                Some(4)
            }
            _ => None,
        }
    };
    let newline = if current.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut edits = Vec::new();
    // 兼容旧版默认配置；长域名和别名不能受 Nginx Windows 的 32 字节默认桶限制。
    // 用户已有数值或额外 include 时保留其配置，避免与 include 中的同名指令重复。
    if !old_http.children.iter().any(|node| node.words[0] == "server_names_hash_bucket_size")
        && !old_http.children.iter().any(|node| node.words[0] == "include" && class(node).is_none())
    {
        edits.push((old_http.end - 1..old_http.end - 1, format!("{newline}    server_names_hash_bucket_size 512;{newline}")));
    }
    for group in 0..5 {
        let (before, after, insert_at) = if group == 0 {
            (&old, &new, 0)
        } else {
            (&old_http.children, &new_http.children, old_http.end - 1)
        };
        let matches = |node: &&NginxDirective| {
            if group == 0 {
                node.words[0] == "pid"
            } else {
                class(node) == Some(group)
            }
        };
        let before: Vec<_> = before.iter().filter(matches).collect();
        let after: Vec<_> = after.iter().filter(matches).collect();
        let text = after
            .iter()
            .map(|node| generated[directive_span(generated, node)].trim_end_matches(['\r', '\n']))
            .collect::<Vec<_>>()
            .join("\n")
            .replace('\n', newline);
        if before.is_empty() {
            if !text.is_empty() {
                edits.push((insert_at..insert_at, format!("{newline}{text}{newline}")));
            }
        } else {
            for (index, node) in before.into_iter().enumerate() {
                let span = directive_span(current, node);
                let replacement = if index == 0 && !text.is_empty() {
                    if span.start == node.start && span.end == node.end {
                        text.trim_start().to_string()
                    } else {
                        format!("{text}{newline}")
                    }
                } else {
                    String::new()
                };
                edits.push((span, replacement));
            }
        }
    }
    edits.sort_by(|a, b| a.0.start.cmp(&b.0.start).then(a.0.end.cmp(&b.0.end)));
    if edits.windows(2).any(|pair| pair[0].0.end > pair[1].0.start) {
        return Err(nginx_structure_error());
    }
    let mut output = current.to_string();
    for (span, replacement) in edits.into_iter().rev() {
        output.replace_range(span, &replacement);
    }
    Ok(output)
}

/// 旧版托管站点使用 `listen 端口`，它会监听全部网卡并触发防火墙询问。
/// 只补全这种默认写法，显式 IP、额外参数、注释和用户自建配置保持原样。
fn localize_legacy_site_listeners(current: &str) -> Result<String> {
    let header = current
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches('\u{feff}');
    if !header.starts_with("# site: ") || !header.contains("NiceEnv 托管") {
        return Ok(current.to_string());
    }
    let source = current.trim_start_matches('\u{feff}');
    let prefix = current.len() - source.len();
    let nodes = nginx_directives(source)?;
    let mut inserts = Vec::new();
    for server in nodes.iter().filter(|node| node.words[0] == "server") {
        for node in server
            .children
            .iter()
            .filter(|node| node.words[0] == "listen")
        {
            let Some(port) = node
                .words
                .get(1)
                .filter(|word| word.parse::<u16>().is_ok_and(|port| port != 0))
            else {
                continue;
            };
            let Some(rest) = source[node.start..node.end].strip_prefix("listen") else {
                continue;
            };
            let argument = rest.trim_start();
            if argument.starts_with(port) {
                inserts.push(prefix + node.start + "listen".len() + rest.len() - argument.len());
            }
        }
    }
    let mut output = current.to_string();
    for at in inserts.into_iter().rev() {
        output.insert_str(at, "127.0.0.1:");
    }
    Ok(output)
}

fn localize_legacy_nginx_sites(paths: &Paths, network: Option<bool>) -> Result<()> {
    let directory = crate::paths::checked_data_path(&paths.base, "etc/nginx/sites")?;
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(AppError::io("读取 Nginx 站点配置", error)),
    };
    let mut changes = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".conf") && !name.ends_with(".conf.disabled") {
            continue;
        }
        let path =
            crate::paths::checked_data_path(&paths.base, &format!("etc/nginx/sites/{name}"))?;
        if !path.is_file() {
            continue;
        }
        let previous = std::fs::read_to_string(&path)?;
        let mut content = localize_legacy_site_listeners(&previous)?;
        if let Some(enabled) = network {
            if previous.lines().next().is_some_and(|line| line.starts_with("# site: ") && line.contains("NiceEnv 托管")) {
                content = network_listeners(&content, enabled, false)?;
            }
        }
        if content != previous {
            changes.push((path, previous, content));
        }
    }
    for (path, previous, content) in changes {
        crate::paths::write_with_backup_expected(
            &path,
            &content,
            &paths.backup(),
            Some(Some(previous.as_bytes())),
        )?;
    }
    Ok(())
}

/// 只调整托管 server 的监听参数，保留端口、SSL 标记、正文与用户注释。
pub(crate) fn network_listeners(source: &str, enabled: bool, main: bool) -> Result<String> {
    let nodes = nginx_directives(source.trim_start_matches('\u{feff}'))?;
    let offset = source.len() - source.trim_start_matches('\u{feff}').len();
    let servers: Vec<_> = if main {
        nodes.iter().filter(|node| node.words[0] == "http").flat_map(|node| &node.children)
            .filter(|node| node.words[0] == "server" && node.children.iter().any(|child| child.words == ["server_name", "_"])).collect()
    } else { nodes.iter().filter(|node| node.words[0] == "server").collect() };
    let host = if enabled { "0.0.0.0" } else { "127.0.0.1" };
    let argument = regex::Regex::new(r"^listen\s+(\S+?)(?:\s|;)").expect("constant pattern");
    let mut edits = Vec::new();
    for server in servers {
        for node in server.children.iter().filter(|node| node.words[0] == "listen") {
            let value = node.words.get(1).ok_or_else(nginx_structure_error)?;
            let port = value.strip_prefix("127.0.0.1:").or_else(|| value.strip_prefix("0.0.0.0:"))
                .or_else(|| value.strip_prefix("*:"))
                .unwrap_or(value).parse::<u16>().ok().filter(|port| *port > 0)
                .ok_or_else(|| AppError::new("WEB_NETWORK_CUSTOM", "站点包含自定义监听地址，无法自动切换局域网访问")
                    .with_hint("请先在配置编辑器中将托管站点的监听改回 127.0.0.1，再重试。"))?;
            let span = argument.captures(&source[offset + node.start..offset + node.end]).and_then(|m| m.get(1))
                .ok_or_else(nginx_structure_error)?;
            edits.push((offset + node.start + span.start(), offset + node.start + span.end(), format!("{host}:{port}")));
        }
    }
    let mut result = source.to_string();
    for (start, end, value) in edits.into_iter().rev() { result.replace_range(start..end, &value); }
    Ok(result)
}

pub fn render_nginx_conf(
    paths: &Paths,
    nginx_root: &std::path::Path,
    http_port: u16,
    https_port: u16,
    php_pools: &[(String, u16)],
    adminer_path: Option<&std::path::Path>,
) -> String {
    // 装了 Adminer 就给默认 server 挂一个 /_adminer 入口，交给第一个可用 PHP 池执行
    // （否则数据库页那个按钮是死链）
    let adminer_loc = match (adminer_path.filter(|p| p.exists()), php_pools.first()) {
        (Some(p), Some((ver, _))) => {
            let upstream = nginx_upstream_name(ver);
            let file = quoted_config_path(p);
            // 公共参数含 SCRIPT_FILENAME / DOCUMENT_ROOT，Adminer 的固定入口必须在其后覆盖。
            format!(
                "        location = /_adminer {{ return 302 /_adminer/; }}\n        location /_adminer/ {{\n            allow 127.0.0.1;\n            deny all;\n            fastcgi_pass {upstream};\n            fastcgi_index index.php;\n            include \"{params}\";\n            fastcgi_param SCRIPT_FILENAME \"{file}\";\n            fastcgi_param DOCUMENT_ROOT \"{dir}\";\n        }}\n",
                dir = quoted_config_path(p.parent().unwrap_or(std::path::Path::new("."))),
                params = quoted_nginx_include(&paths.etc().join("nginx").join("fastcgi_params"), false),
            )
        }
        _ => String::new(),
    };
    let mut upstreams = String::new();
    for (ver, base) in php_pools {
        let name = nginx_upstream_name(ver);
        upstreams.push_str(&format!("    upstream {name} {{\n        least_conn;\n"));
        for i in 0..PHP_POOL_WORKERS {
            upstreams.push_str(&format!("        server 127.0.0.1:{};\n", base + i));
        }
        upstreams.push_str("    }\n\n");
    }

    format!(
        r#"# NiceEnv nginx.conf — 自定义全局配置在重启和更新站点后保留
# pid、mime.types、默认 server_name _、nsb_php_* 和站点 include 由应用管理
worker_processes  2;
pid        "{pid}";
error_log  "{error_log}" warn;

events {{
    worker_connections  1024;
}}

http {{
    include       "{mime}";
    default_type  application/octet-stream;

    sendfile        on;
    tcp_nopush      on;
    keepalive_timeout  65;
    server_names_hash_bucket_size 512;
    server_tokens   off;
    client_max_body_size 128m;

    access_log  "{access_log}";

    client_body_temp_path   "{temp}/client_body";
    proxy_temp_path         "{temp}/proxy";
    fastcgi_temp_path       "{temp}/fastcgi";
    uwsgi_temp_path         "{temp}/uwsgi";
    scgi_temp_path          "{temp}/scgi";

    gzip on;
    gzip_min_length 1k;
    gzip_comp_level 4;
    gzip_types text/plain text/css application/javascript application/json image/svg+xml;

    fastcgi_buffers 16 16k;
    fastcgi_buffer_size 32k;
    fastcgi_read_timeout 300s;

    # 默认兜底：直接访问时给一个引导页
    server {{
        listen 127.0.0.1:{http_port};
        listen 127.0.0.1:{https_port} ssl;
        server_name _;
        ssl_certificate     "{certs}/fallback/localhost.crt";
        ssl_certificate_key "{certs}/fallback/localhost.key";
        location / {{
            default_type text/html;
            return 200 "<h1>NiceEnv is running</h1><p>创建站点后用你的本地域名访问，例如 http://demo.test:{http_port}</p>";
        }}
{adminer_loc}
    }}

{upstreams}
    include "{sites}";
}}
"#,
        pid = quoted_config_path(&paths.etc().join("nginx").join("run").join("nginx.pid")),
        error_log = quoted_config_path(&paths.logs().join("nginx").join("error.log")),
        access_log = quoted_config_path(&paths.logs().join("nginx").join("access.log")),
        mime = quoted_nginx_include(&nginx_root.join("conf").join("mime.types"), false),
        temp = quoted_config_path(&paths.etc().join("nginx").join("temp")),
        certs = quoted_config_path(&paths.certs()),
        http_port = http_port,
        https_port = https_port,
        adminer_loc = adminer_loc,
        upstreams = upstreams,
        sites = quoted_nginx_include(&paths.nginx_sites_dir(), true),
    )
}

pub const FASTCGI_PARAMS: &str = r#"fastcgi_param  SCRIPT_FILENAME    $document_root$fastcgi_script_name;
fastcgi_param  QUERY_STRING       $query_string;
fastcgi_param  REQUEST_METHOD     $request_method;
fastcgi_param  CONTENT_TYPE       $content_type;
fastcgi_param  CONTENT_LENGTH     $content_length;
fastcgi_param  SCRIPT_NAME        $fastcgi_script_name;
fastcgi_param  REQUEST_URI        $request_uri;
fastcgi_param  DOCUMENT_URI       $document_uri;
fastcgi_param  DOCUMENT_ROOT      $document_root;
fastcgi_param  SERVER_PROTOCOL    $server_protocol;
fastcgi_param  REQUEST_SCHEME     $scheme;
fastcgi_param  HTTPS              $https if_not_empty;
fastcgi_param  GATEWAY_INTERFACE  CGI/1.1;
fastcgi_param  SERVER_SOFTWARE    nginx;
fastcgi_param  REMOTE_ADDR        $remote_addr;
fastcgi_param  REMOTE_PORT        $remote_port;
fastcgi_param  SERVER_ADDR        $server_addr;
fastcgi_param  SERVER_PORT        $server_port;
fastcgi_param  SERVER_NAME        $server_name;
fastcgi_param  REDIRECT_STATUS    200;
"#;

pub fn rewrite_snippet(preset: &RewritePreset) -> &'static str {
    match preset {
        RewritePreset::None => "",
        RewritePreset::Laravel => {
            "    location / {\n        try_files $uri $uri/ /index.php?$query_string;\n    }\n"
        }
        RewritePreset::Thinkphp => {
            "    location / {\n        if (!-e $request_filename) {\n            rewrite ^(.*)$ /index.php?s=$1 last;\n        }\n    }\n"
        }
        RewritePreset::Wordpress => {
            "    location / {\n        try_files $uri $uri/ /index.php?$args;\n    }\n    rewrite /wp-admin$ $scheme://$host$uri/ permanent;\n"
        }
        RewritePreset::SpaFallback => {
            "    location / {\n        try_files $uri $uri/ /index.html;\n    }\n"
        }
        RewritePreset::NextExport => {
            // 同时兼容 /about.html 与 /about/index.html；不存在的页面保留真实 404。
            "    location / {\n        try_files $uri $uri.html $uri/ =404;\n    }\n    error_page 404 /404.html;\n    location = /404.html { internal; }\n"
        }
        RewritePreset::Symfony => {
            // 入口在 public/，站点根目录应指向 public；这里兜 front controller
            "    location / {\n        try_files $uri $uri/ /index.php$is_args$args;\n    }\n"
        }
        RewritePreset::Yii2 => {
            "    location / {\n        try_files $uri $uri/ /index.php?$args;\n    }\n"
        }
        RewritePreset::Codeigniter => {
            // CI4：隐藏 index.php；保护 app/system/writable 等非公开目录
            "    location / {\n        try_files $uri $uri/ /index.php$is_args$args;\n    }\n    location ~* ^/(app|system|writable)/ {\n        deny all;\n    }\n"
        }
        RewritePreset::Cakephp => {
            // 经典 cake 食谱：webroot 剥离
            "    location / {\n        try_files $uri $uri/ /index.php?url=$uri&$args;\n    }\n"
        }
        RewritePreset::Drupal => {
            "    location / {\n        try_files $uri $uri/ /index.php?$query_string;\n    }\n    location ~* \\.(engine|inc|info|install|module|profile|po|sh|.*sql|theme|tpl(\\.php)?|xtmpl)$ {\n        deny all;\n    }\n"
        }
        RewritePreset::Joomla => {
            "    location / {\n        try_files $uri $uri/ /index.php?$args;\n    }\n"
        }
    }
}

/// 显式 ACME 选择引用签发时的主域名文件；导入和默认来源保留原有目录。
pub(crate) fn site_certificate_files(site: &Site, cert_dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let (directory, stem) = match &site.runtime.imported_cert_id {
        Some(id) => (
            cert_dir.parent().unwrap_or(cert_dir).join("imported"),
            if crate::certs::valid_imported_id(id) { id.clone() } else { "invalid-certificate".into() },
        ),
        None => (cert_dir.to_path_buf(), site.runtime.acme_cert_id.as_deref()
            .map(|id| crate::certs::acme_primary(id).unwrap_or_else(|_| "invalid-certificate".into()))
            .unwrap_or_else(|| site.domains.first().cloned().unwrap_or_else(|| "localhost".into()))
            .replace('*', "_wildcard").replace(':', "_")),
    };
    (directory.join(format!("{stem}.crt")), directory.join(format!("{stem}.key")))
}

/// 单站点 server block
pub fn render_site_conf(
    site: &Site,
    http_port: u16,
    https_port: u16,
    fastcgi_params_path: &std::path::Path,
    cert_dir: &std::path::Path,
    log_dir: &std::path::Path,
) -> String {
    render_site_conf_with_auth(site, http_port, https_port, fastcgi_params_path, cert_dir, log_dir, None)
}

pub fn render_site_conf_with_auth(
    site: &Site,
    http_port: u16,
    https_port: u16,
    fastcgi_params_path: &std::path::Path,
    cert_dir: &std::path::Path,
    log_dir: &std::path::Path,
    auth_file: Option<&std::path::Path>,
) -> String {
    let server_names = site.domains.join(" ");
    let https_redirect = site.https.then_some(site.runtime.https_redirect).flatten();
    let listen = if https_redirect.is_some() {
        format!("listen 127.0.0.1:{https_port} ssl")
    } else if site.https {
        format!("listen 127.0.0.1:{http_port};\n    listen 127.0.0.1:{https_port} ssl")
    } else {
        format!("listen 127.0.0.1:{http_port}")
    };
    let ssl_lines = if site.https {
        let (certificate, key) = site_certificate_files(site, cert_dir);
        format!(
            "    ssl_certificate     \"{}\";\n    ssl_certificate_key \"{}\";",
            quoted_config_path(&certificate),
            quoted_config_path(&key),
        )
    } else {
        String::new()
    };

    let (access_maps, access_gate) = crate::siteaccess::nginx(&site.id, site.runtime.access.as_ref());
    let cors = site.runtime.cors.as_ref().map(|cors| crate::sitecors::nginx(&site.id, cors));
    let error_pages = nginx_error_pages(site.runtime.error_pages.as_ref());
    let basic_auth = auth_file
        .map(|path| {
            format!(
                "    auth_basic \"Restricted\";\n    auth_basic_user_file \"{}\";\n",
                quoted_config_path(path)
            )
        })
        .unwrap_or_default();
    let body = match &site.runtime.kind {
        crate::model::SiteKind::Redirect => redirect_directives(site, false),
        crate::model::SiteKind::Php => {
            let upstream =
                nginx_upstream_name(site.runtime.php_version.as_deref().unwrap_or("8.3"));
            let mut s = site.runtime.custom_rewrite.as_ref().filter(|r| r.server == "nginx").map(|r| r.content.clone()).unwrap_or_else(|| rewrite_snippet(&site.rewrite).to_string());
            if site.runtime.custom_rewrite.is_none() && matches!(site.rewrite, RewritePreset::None) {
                s.push_str("    location / {\n        try_files $uri $uri/ /index.php?$query_string;\n    }\n");
            }
            s.push_str(&format!(
                "    location ~ \\.php$ {{\n        try_files $uri =404;\n        fastcgi_pass {upstream};\n        include \"{}\";\n    }}\n",
                quoted_nginx_include(fastcgi_params_path, false)
            ));
            s
        }
        crate::model::SiteKind::Static => {
            let mut s = site.runtime.custom_rewrite.as_ref().filter(|r| r.server == "nginx").map(|r| r.content.clone()).unwrap_or_else(|| rewrite_snippet(&site.rewrite).to_string());
            s.push_str("    location ~* \\.(?:php[0-9]*|phtml|pht|phar)(?:[./]|$) { deny all; }\n");
            if site.runtime.custom_rewrite.is_none() && matches!(site.rewrite, RewritePreset::None) {
                s.push_str("    location / {\n        try_files $uri $uri/ =404;\n    }\n");
            }
            s
        }
        crate::model::SiteKind::ReverseProxy
        | crate::model::SiteKind::Node
        | crate::model::SiteKind::Python
        | crate::model::SiteKind::Java
        | crate::model::SiteKind::Go => {
            let target = site
                .runtime
                .proxy_target
                .clone()
                .unwrap_or_else(|| "127.0.0.1:8080".into());
            let target =
                crate::sites::proxy_url(&target).unwrap_or_else(|_| "http://127.0.0.1:1/".into());
            format!(
                "    location / {{\n        proxy_pass {target};\n        proxy_intercept_errors on;\n        proxy_ssl_server_name on;\n        proxy_http_version 1.1;\n        proxy_set_header Host $proxy_host;\n        proxy_set_header X-Real-IP $remote_addr;\n        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n        proxy_set_header X-Forwarded-Proto $scheme;\n        proxy_set_header Upgrade $http_upgrade;\n        proxy_set_header Connection \"upgrade\";\n        proxy_read_timeout 300s;\n    }}\n"
            )
        }
    };

    format!(
        r#"# site: {name} ({id}) — NiceEnv 托管
{access_maps}
{cors_maps}
{http_redirect}
server {{
    {listen};
    server_name {server_names};
    {ssl_lines}
    {document_root}
    charset utf-8;

    # 站点级日志（日志页按站点查看就靠它）
    access_log "{access_log}";
    error_log "{error_log}" warn;
{error_pages}
{basic_auth}

    location ~ /\.(?!well-known(?:/|$)) {{ deny all; }}
{cors_headers}
{access_gate}
{cors_preflight}

{proxy_rules}
{body}}}
"#,
        name = site.name,
        id = site.id,
        access_log = quoted_config_path(&log_dir.join(format!("{}.access.log", site.id))),
        error_log = quoted_config_path(&log_dir.join(format!("{}.error.log", site.id))),
        error_pages = error_pages,
        basic_auth = basic_auth,
        listen = listen,
        server_names = server_names,
        ssl_lines = ssl_lines,
        document_root = if site.runtime.kind == crate::model::SiteKind::Redirect { String::new() } else {
            let index = if site.runtime.kind == crate::model::SiteKind::Php { "index.php index.html index.htm" } else { "index.html index.htm" };
            format!("root \"{}\";\n    index {index};", quoted_config_path(std::path::Path::new(&site.root_dir)))
        },
        body = cors.as_ref().map_or_else(|| body.clone(), |cors| nginx_cors_headers_in_locations(&body, &cors.headers)),
        cors_maps = cors.as_ref().map_or("", |cors| cors.maps.as_str()),
        http_redirect = https_redirect.map(|status| {
            let suffix = https_port_suffix(https_port);
            let hosts = https_redirect_hosts(site);
            format!("server {{\n    listen 127.0.0.1:{http_port};\n    server_name {server_names};\n    access_log \"{}\";\n    error_log \"{}\" warn;\n{access_gate}    if ($host !~* \"^(?:{hosts})$\") {{ return 421; }}\n    return {status} \"https://$host{suffix}$request_uri\";\n}}\n", quoted_config_path(&log_dir.join(format!("{}.access.log", site.id))), quoted_config_path(&log_dir.join(format!("{}.error.log", site.id))))
        }).unwrap_or_default(),
        cors_headers = cors.as_ref().map_or("", |cors| cors.headers.as_str()),
        cors_preflight = cors.as_ref().map_or("", |cors| cors.before_content.as_str()),
        proxy_rules = crate::siteproxy::nginx(&site.runtime),
    )
}

fn nginx_error_pages(pages: Option<&std::collections::BTreeMap<u16, String>>) -> String {
    pages.into_iter().flatten()
        .map(|(status, path)| format!("    error_page {status} {path};\n"))
        .collect()
}

/// Nginx 子块一旦定义 add_header 就不再继承父级；保留自定义头并补回托管 CORS 头。
fn nginx_cors_headers_in_locations(content: &str, headers: &str) -> String {
    let Ok(nodes) = nginx_directives(content) else { return content.into(); };
    fn edits(nodes: &[NginxDirective], headers: &str, out: &mut Vec<(usize, usize, String)>) {
        for node in nodes {
            if matches!(node.words.first().map(String::as_str), Some("add_header" | "proxy_pass_header" | "fastcgi_pass_header"))
                && node.words.get(1).is_some_and(|name| crate::sitecors::HEADERS.iter().any(|header| name.eq_ignore_ascii_case(header))) {
                out.push((node.start, node.end, String::new()));
            }
            if !node.children.is_empty() && node.children.iter().any(|child| child.words.first().is_some_and(|word| word == "add_header")) {
                out.push((node.end - 1, node.end - 1, format!("\n{headers}")));
            }
            // 自定义隐藏头同样会中断父级继承，子 location 仍须过滤应用返回的 CORS 头。
            for directive in ["proxy_hide_header", "fastcgi_hide_header"] {
                if node.children.iter().any(|child| child.words.first().is_some_and(|word| word == directive)) {
                    let hidden = crate::sitecors::HEADERS.iter().map(|header| format!("    {directive} {header};\n")).collect::<String>();
                    out.push((node.end - 1, node.end - 1, format!("\n{hidden}")));
                }
            }
            edits(&node.children, headers, out);
        }
    }
    let mut changes = Vec::new(); edits(&nodes, headers, &mut changes);
    changes.sort_by_key(|change| std::cmp::Reverse(change.0));
    let mut output = content.to_string();
    for (start, end, replacement) in changes { output.replace_range(start..end, &replacement); }
    output
}

/* ================= php.ini ================= */

pub(crate) fn https_port_suffix(port: u16) -> String {
    if port == 443 { String::new() } else { format!(":{port}") }
}

fn https_redirect_hosts(site: &Site) -> String {
    site.domains.iter().map(|domain| domain.strip_prefix("*.")
        .map(|suffix| format!("(?:[^.:]+\\.)+{}", regex::escape(suffix)))
        .unwrap_or_else(|| regex::escape(domain))).collect::<Vec<_>>().join("|")
}

fn redirect_directives(site: &Site, apache: bool) -> String {
    let redirect = site.runtime.redirect.as_ref();
    let target = redirect.and_then(|value| crate::sites::redirect_url(value, &site.domains).ok());
    let (Some(redirect), Some(target)) = (redirect, target) else {
        return if apache { "    RewriteEngine On\n    RewriteRule ^ - [F,L]\n" } else { "    return 400;\n" }.into();
    };
    let code = redirect.status;
    if apache {
        let target = target.replace('%', "\\%");
        if redirect.preserve_path {
            format!("    AllowEncodedSlashes NoDecode\n    RewriteEngine On\n    RewriteCond %{{THE_REQUEST}} \"\\s(/[^\\s?]*)(?:\\?[^\\s]*)?\\s\"\n    RewriteRule ^ \"{}%1\" [R={code},L,NE]\n", target.trim_end_matches('/'))
        } else {
            format!("    AllowEncodedSlashes NoDecode\n    RewriteEngine On\n    RewriteRule ^ \"{target}\" [R={code},L,NE,QSD]\n")
        }
    } else {
        let target = if redirect.preserve_path { format!("{}$request_uri", target.trim_end_matches('/')) } else { target };
        format!("    return {code} \"{target}\";\n")
    }
}

pub fn render_php_ini(paths: &Paths, version: &str, runtime_dir: &std::path::Path) -> String {
    format!(
        r#"; NiceEnv managed php.ini ({version})
[PHP]
engine=On
expose_php=Off
memory_limit=256M
error_reporting=E_ALL
display_errors=On
display_startup_errors=On
log_errors=On
error_log="{error_log}"
max_execution_time=300
max_input_time=300
post_max_size=128M
upload_max_filesize=128M
default_charset="UTF-8"
extension_dir="{ext}"
cgi.force_redirect=0
cgi.fix_pathinfo=1
fastcgi.impersonate=1
fastcgi.logging=0
variables_order="EGPCS"
request_order="GP"
date.timezone=Asia/Shanghai
opcache.enable=1
opcache.enable_cli=0

[Extensions]
extension=curl
extension=fileinfo
extension={gd}
extension=mbstring
extension=mysqli
extension=openssl
extension=pdo_mysql
extension=sockets

[Session]
session.save_handler=files
session.save_path="{sess}"

[syslog]
define_syslog_variables=Off
"#,
        version = version,
        error_log = quoted_config_path(
            &paths
                .logs()
                .join("php")
                .join(version)
                .join("php_errors.log")
        ),
        ext = quoted_config_path(&runtime_dir.join("ext")),
        gd = crate::phpext::gd_extension_name(runtime_dir, version),
        sess = quoted_config_path(&paths.data().join("php").join(version).join("sess")),
    )
}

/* ================= mysql my.ini ================= */

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum IniDialect {
    Php,
    Mysql,
}

#[derive(Clone, Copy, PartialEq)]
enum PathWordDialect {
    Php,
    Mysql,
    Redis,
}

fn decode_path_word(
    raw: &str,
    quote: Option<char>,
    dialect: PathWordDialect,
) -> Result<(String, Vec<usize>)> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::new();
    let mut ends = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let escapes = dialect == PathWordDialect::Mysql
            || quote == Some('"')
            || (dialect == PathWordDialect::Redis
                && quote == Some('\'')
                && bytes.get(at + 1) == Some(&b'\''));
        if bytes[at] == b'\\' && escapes && at + 1 < bytes.len() {
            let next = bytes[at + 1];
            if dialect == PathWordDialect::Redis
                && quote == Some('"')
                && next == b'x'
                && bytes
                    .get(at + 2..at + 4)
                    .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))
            {
                decoded.push(u8::from_str_radix(&raw[at + 2..at + 4], 16).unwrap());
                at += 4;
                ends.push(at);
                continue;
            }
            let special = match next {
                b'\\' if dialect != PathWordDialect::Redis || quote == Some('"') => Some(b'\\'),
                b'"' if dialect == PathWordDialect::Mysql || quote == Some('"') => Some(b'"'),
                b'\'' if dialect == PathWordDialect::Mysql || dialect == PathWordDialect::Redis => {
                    Some(b'\'')
                }
                b'n' if dialect != PathWordDialect::Php => Some(b'\n'),
                b'r' if dialect != PathWordDialect::Php => Some(b'\r'),
                b't' if dialect != PathWordDialect::Php => Some(b'\t'),
                b'b' if dialect != PathWordDialect::Php => Some(8),
                b's' if dialect == PathWordDialect::Mysql => Some(b' '),
                b'a' if dialect == PathWordDialect::Redis => Some(7),
                next if dialect == PathWordDialect::Redis && quote == Some('"') => Some(next),
                _ => None,
            };
            if let Some(special) = special {
                decoded.push(special);
                at += 2;
                ends.push(at);
                continue;
            }
        }
        decoded.push(bytes[at]);
        at += 1;
        ends.push(at);
    }
    let decoded = String::from_utf8(decoded).map_err(|_| {
        AppError::new(
            "DATA_DIR_CONFIG_ENCODING",
            "配置中的路径转义不是有效 UTF-8，无法自动迁移",
        )
    })?;
    Ok((decoded, ends))
}

/// 解码用于路径比较，但保留原后缀的字面写法（例如 PHP 的环境变量表达式）。
fn rebase_path_word(
    raw: &str,
    quote: Option<char>,
    dialect: PathWordDialect,
    rebase: &crate::paths::DataPathRebase,
) -> Result<String> {
    let (decoded, ends) = decode_path_word(raw, quote, dialect)?;
    let rebased = rebase.config_value(&decoded, str::to_owned)?;
    if decoded == rebased {
        return Ok(raw.to_string());
    }
    let Some(quote) = quote else {
        return Ok(rebased);
    };
    let shared = decoded
        .chars()
        .rev()
        .zip(rebased.chars().rev())
        .take_while(|(left, right)| left == right)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let raw_end = decoded
        .len()
        .checked_sub(shared + 1)
        .map_or(0, |index| ends[index]);
    let prefix = &rebased[..rebased.len() - shared];
    let prefix = if dialect == PathWordDialect::Php && quote == '\'' {
        prefix.to_string()
    } else if dialect == PathWordDialect::Redis && quote == '\'' {
        prefix.replace('\'', "\\'")
    } else {
        prefix
            .replace('\\', "\\\\")
            .replace(quote, &format!("\\{quote}"))
    };
    Ok(format!("{prefix}{}", &raw[raw_end..]))
}

/// 只迁移文件/目录选项，不能改写密码、SQL、注释等恰好包含旧目录的内容。
pub(crate) fn rebase_ini_config(
    content: &str,
    rebase: &crate::paths::DataPathRebase,
    dialect: IniDialect,
) -> Result<String> {
    let mut output = String::new();
    for (number, line) in content.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        if dialect == IniDialect::Mysql {
            if let Some((directive, tail)) = trimmed.split_once(char::is_whitespace) {
                if matches!(directive, "!include" | "!includedir") {
                    // MySQL 把剩余整行作为文件名；含空格也不能添加引号。
                    let start = line.len() - tail.trim_start().len();
                    let path = line[start..].trim_end();
                    output.push_str(&line[..start]);
                    output.push_str(&rebase.config_value(path, str::to_owned)?);
                    output.push_str(&line[start + path.len()..]);
                    continue;
                }
            }
        }
        if trimmed.starts_with([';', '#', '[']) {
            output.push_str(line);
            continue;
        }
        let Some((key, tail)) = line.split_once('=') else {
            output.push_str(line);
            continue;
        };
        let key = key.trim().to_ascii_lowercase().replace('-', "_");
        let key = key
            .strip_prefix("loose_")
            .unwrap_or(&key)
            .trim_end_matches("[]");
        let path_option = match dialect {
            IniDialect::Php => matches!(
                key,
                "extension"
                    | "zend_extension"
                    | "extension_dir"
                    | "error_log"
                    | "doc_root"
                    | "user_dir"
                    | "upload_tmp_dir"
                    | "sys_temp_dir"
                    | "browscap"
                    | "auto_prepend_file"
                    | "auto_append_file"
                    | "include_path"
                    | "open_basedir"
                    | "session.save_path"
                    | "opcache.file_cache"
                    | "opcache.preload"
                    | "opcache.blacklist_filename"
                    | "openssl.cafile"
                    | "openssl.capath"
                    | "curl.cainfo"
                    | "xdebug.log"
                    | "xdebug.output_dir"
                    | "xdebug.profiler_output_dir"
                    | "xdebug.trace_output_dir"
            ),
            IniDialect::Mysql => matches!(
                key,
                "basedir"
                    | "datadir"
                    | "tmpdir"
                    | "plugin_dir"
                    | "lc_messages_dir"
                    | "character_sets_dir"
                    | "log_error"
                    | "general_log_file"
                    | "slow_query_log_file"
                    | "log_bin"
                    | "log_bin_index"
                    | "relay_log"
                    | "relay_log_index"
                    | "relay_log_info_file"
                    | "pid_file"
                    | "socket"
                    | "secure_file_priv"
                    | "ssl_ca"
                    | "ssl_capath"
                    | "ssl_cert"
                    | "ssl_key"
                    | "ssl_crl"
                    | "ssl_crlpath"
                    | "admin_ssl_ca"
                    | "admin_ssl_capath"
                    | "admin_ssl_cert"
                    | "admin_ssl_key"
                    | "admin_ssl_crl"
                    | "admin_ssl_crlpath"
                    | "innodb_data_home_dir"
                    | "innodb_log_group_home_dir"
                    | "innodb_undo_directory"
                    | "innodb_tmpdir"
                    | "innodb_temp_tablespaces_dir"
                    | "innodb_doublewrite_dir"
                    | "innodb_directories"
                    | "innodb_data_file_path"
                    | "innodb_temp_data_file_path"
                    | "aria_log_dir_path"
                    | "slave_load_tmpdir"
                    | "replica_load_tmpdir"
                    | "rocksdb_datadir"
                    | "rocksdb_wal_dir"
            ),
        };
        if !path_option {
            output.push_str(line);
            continue;
        }
        let fail = || {
            AppError::new(
                "DATA_DIR_CONFIG_SYNTAX",
                format!(
                    "第 {} 行路径配置无法安全转换，请检查引号与拼接表达式",
                    number + 1
                ),
            )
        };
        let start = line.len() - tail.trim_start().len();
        let value = &line[start..];
        let quote = value.chars().next().filter(|c| matches!(c, '\'' | '"'));
        let comment = if dialect == IniDialect::Php { ';' } else { '#' };
        let (body, end) = if let Some(quote) = quote {
            let mut chars = value[1..].char_indices();
            let mut end = None;
            while let Some((offset, character)) = chars.next() {
                if character == '\\' && (dialect == IniDialect::Mysql || quote == '"') {
                    chars.next();
                } else if character == quote {
                    end = Some(offset + 2);
                    break;
                }
            }
            let end = end.ok_or_else(fail)?;
            let after = value[end..].trim();
            if !after.is_empty() && !after.starts_with(comment) {
                return Err(fail());
            }
            (&value[1..end - 1], end)
        } else {
            let end = if dialect == IniDialect::Mysql {
                crate::generic::mysql_option_value_end(value)
            } else {
                value.find(comment).unwrap_or(value.len())
            };
            let body = value[..end].trim_end();
            (body, body.len())
        };
        let encode = |path: &str| {
            if dialect == IniDialect::Php && quote == Some('\'') {
                path.to_string()
            } else {
                path.replace('\\', "\\\\")
                    .replace(quote.unwrap_or('"'), &format!("\\{}", quote.unwrap_or('"')))
            }
        };
        let word_dialect = if dialect == IniDialect::Php {
            PathWordDialect::Php
        } else {
            PathWordDialect::Mysql
        };
        let decoded;
        let body = if quote.is_none() {
            decoded = decode_path_word(body, None, word_dialect)?.0;
            decoded.as_str()
        } else {
            body
        };
        let rewrite = |value: &str| -> Result<String> {
            if quote.is_none() {
                rebase.config_value(value, str::to_owned)
            } else {
                rebase_path_word(value, quote, word_dialect, rebase)
            }
        };
        let list_separator = match (dialect, key) {
            (IniDialect::Php, "include_path" | "open_basedir") | (IniDialect::Mysql, "tmpdir") => {
                Some(if cfg!(windows) { ';' } else { ':' })
            }
            (
                IniDialect::Mysql,
                "innodb_directories" | "innodb_data_file_path" | "innodb_temp_data_file_path",
            ) => Some(';'),
            _ => None,
        };
        let rebased = if let Some(separator) = list_separator {
            body.split(separator)
                .map(rewrite)
                .collect::<Result<Vec<_>>>()?
                .join(&separator.to_string())
        } else if dialect == IniDialect::Php && key == "session.save_path" {
            // files session handler 可使用 N;MODE;/path，不能把这些前缀当作目录。
            let mut path = body;
            for _ in 0..2 {
                if let Some((prefix, tail)) = path.split_once(';') {
                    if !prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_digit()) {
                        path = tail;
                        continue;
                    }
                }
                break;
            }
            format!("{}{}", &body[..body.len() - path.len()], rewrite(path)?)
        } else {
            rewrite(body)?
        };
        if rebased == body {
            output.push_str(line);
            continue;
        }
        output.push_str(&line[..start]);
        if let Some(quote) = quote {
            output.push(quote);
            output.push_str(&rebased);
            output.push(quote);
        } else {
            output.push('"');
            output.push_str(&encode(&rebased));
            output.push('"');
        }
        output.push_str(&value[end..]);
    }
    Ok(output)
}

/// 只替换调用方明确拥有的逻辑行。其他行、注释、空白和多行指令原样保留。
/// 每组分别给出原位置替换内容与缺失时追加内容（INI 追加时需携带节名）。
fn sync_managed_lines(
    current: &str,
    groups: &[(String, String)],
    prepend_missing: &[usize],
    mut classify: impl FnMut(&str) -> Option<usize>,
) -> String {
    let newline = if current.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut seen = vec![false; groups.len()];
    let mut output = String::new();
    let mut pending = String::new();
    let mut emit = |raw: &str| {
        let logical = raw.replace("\\\r\n", "").replace("\\\n", "");
        if let Some(index) = classify(logical.trim()) {
            if !seen[index] {
                output.push_str(&groups[index].0.replace('\n', newline));
                output.push_str(newline);
                seen[index] = true;
            }
        } else {
            output.push_str(raw);
        }
    };
    for line in current.split_inclusive('\n') {
        pending.push_str(line);
        if !line.trim_end_matches(['\r', '\n']).ends_with('\\') {
            emit(&pending);
            pending.clear();
        }
    }
    if !pending.is_empty() {
        emit(&pending);
    }
    let mut prefix = String::new();
    for (index, (_, missing)) in groups.iter().enumerate() {
        if missing.is_empty() { continue; }
        if !seen[index] {
            if prepend_missing.contains(&index) {
                prefix.push_str(&missing.replace('\n', newline));
                prefix.push_str(newline);
                continue;
            }
            if !output.is_empty() && !output.ends_with('\n') {
                output.push_str(newline);
            }
            output.push_str(&missing.replace('\n', newline));
            output.push_str(newline);
        }
    }
    prefix + &output
}

/// 未显式配置监听地址的旧 my.ini 使用本机默认值；包含外部配置时不猜测其内容。
fn mysql_local_defaults(current: &str, version: &str) -> String {
    let mut section = String::new();
    let mut bind = false;
    let mut mysqlx_bind = false;
    for line in current.lines().map(str::trim) {
        if line.starts_with(['#', ';']) {
            continue;
        }
        if line.starts_with('!') {
            return current.to_string();
        }
        if let Some(rest) = line.strip_prefix('[') {
            if let Some((name, _)) = rest.split_once(']') {
                section = name.trim().to_ascii_lowercase();
            }
            continue;
        }
        if section != "mysqld" && section != "server" && !section.starts_with("mysqld-") {
            continue;
        }
        let key = line
            .split_once('=')
            .map(|(key, _)| key.trim())
            .unwrap_or(line)
            .to_ascii_lowercase()
            .replace('_', "-");
        let key = key.strip_prefix("loose-").unwrap_or(&key);
        bind |= key == "bind-address";
        mysqlx_bind |= key == "mysqlx-bind-address";
    }
    let mut defaults = Vec::new();
    if !bind {
        defaults.push("bind-address=127.0.0.1");
    }
    if !mysqlx_bind
        && version
            .split('.')
            .next()
            .and_then(|v| v.parse::<u32>().ok())
            .is_some_and(|v| v >= 8)
    {
        // X Plugin 可以被用户重新启用，也需避免再次监听全部网卡。
        defaults.push("loose-mysqlx-bind-address=127.0.0.1");
    }
    if defaults.is_empty() {
        return current.to_string();
    }
    let newline = if current.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    format!("{current}{newline}# NiceEnv: 默认仅供本机访问，可显式修改监听地址{newline}[mysqld]{newline}{}{newline}", defaults.join(newline))
}

fn sync_mysql_config(
    current: &str,
    paths: &Paths,
    version: &str,
    basedir: &Path,
    port: u16,
) -> String {
    let options = [
        (
            "mysqld",
            "basedir",
            format!("\"{}\"", quoted_config_path(basedir)),
        ),
        (
            "mysqld",
            "datadir",
            format!("\"{}\"", quoted_config_path(&paths.mysql_data_dir(version))),
        ),
        ("mysqld", "port", port.to_string()),
        ("client", "port", port.to_string()),
    ];
    let groups: Vec<_> = options
        .iter()
        .map(|(section, key, value)| {
            (
                format!("{key}={value}"),
                format!("[{section}]\n{key}={value}"),
            )
        })
        .collect();
    let mut section = String::new();
    let output = sync_managed_lines(current, &groups, &[], |line| {
        if line.starts_with(['#', ';', '!']) {
            return None;
        }
        if let Some(rest) = line.strip_prefix('[') {
            if let Some((name, _)) = rest.split_once(']') {
                section = name.trim().to_ascii_lowercase();
            }
            return None;
        }
        let key = line
            .split_once('=')
            .map(|(key, _)| key.trim())
            .unwrap_or(line);
        options
            .iter()
            .position(|(group, option, _)| section == *group && key.eq_ignore_ascii_case(option))
    });
    mysql_local_defaults(&output, version)
}

fn sync_redis_config(current: &str, paths: &Paths, port: u16) -> String {
    let options = [
        ("port", port.to_string()),
        (
            "dir",
            format!("\"{}\"", quoted_config_path(&paths.redis_data_dir())),
        ),
        ("daemonize", "no".into()),
    ];
    let groups: Vec<_> = options
        .iter()
        .map(|(key, value)| {
            let line = format!("{key} {value}");
            (line.clone(), line)
        })
        .collect();
    sync_managed_lines(current, &groups, &[], |line| {
        let key = line.split_whitespace().next()?;
        options
            .iter()
            .position(|(option, _)| key.eq_ignore_ascii_case(option))
    })
}

pub fn render_mysql_ini(
    paths: &Paths,
    version: &str,
    basedir: &std::path::Path,
    port: u16,
) -> String {
    // X Protocol 从 MySQL 8 起默认启用；5.7 不能传入这个选项。
    let mysqlx = if version
        .split('.')
        .next()
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|v| v >= 8)
    {
        "mysqlx=OFF\nloose-mysqlx-bind-address=127.0.0.1\n"
    } else {
        ""
    };
    format!(
        r#"# NiceEnv my.ini ({version}) — 自定义设置在重启后保留
# basedir/datadir/port 由应用管理，端口请在设置页修改
[mysqld]
basedir="{basedir}"
datadir="{datadir}"
port={port}
bind-address=127.0.0.1
{mysqlx}character-set-server=utf8mb4
collation-server=utf8mb4_unicode_ci
default-storage-engine=INNODB
sql-mode="STRICT_TRANS_TABLES,NO_ENGINE_SUBSTITUTION"
max_connections=200
max_allowed_packet=64M
innodb_buffer_pool_size=256M
log-error="{log_error}"
slow_query_log=0

[client]
port={port}
default-character-set=utf8mb4
"#,
        version = version,
        basedir = quoted_config_path(basedir),
        datadir = quoted_config_path(&paths.mysql_data_dir(version)),
        port = port,
        mysqlx = mysqlx,
        log_error = quoted_config_path(&paths.logs().join("mysql").join("error.log")),
    )
}

/* ================= redis.conf ================= */

pub(crate) fn rebase_redis_config(
    content: &str,
    rebase: &crate::paths::DataPathRebase,
    glob_include: bool,
) -> Result<String> {
    let mut output = String::new();
    for (number, line) in content.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        let Some((key, tail)) = trimmed.split_once(char::is_whitespace) else {
            output.push_str(line);
            continue;
        };
        if !matches!(
            key.to_ascii_lowercase().as_str(),
            "dir"
                | "logfile"
                | "pidfile"
                | "unixsocket"
                | "aclfile"
                | "cluster-config-file"
                | "tls-cert-file"
                | "tls-key-file"
                | "tls-ca-cert-file"
                | "tls-dh-params-file"
                | "tls-client-cert-file"
                | "tls-client-key-file"
                | "loadmodule"
                | "include"
        ) {
            output.push_str(line);
            continue;
        }
        let start = line.len() - tail.trim_start().len();
        let value = &line[start..];
        let quote = value.chars().next().filter(|c| matches!(c, '\'' | '"'));
        let fail = || {
            AppError::new(
                "DATA_DIR_CONFIG_SYNTAX",
                format!("Redis 第 {} 行路径引号或参数边界无效", number + 1),
            )
        };
        let (body, end) = if let Some(quote) = quote {
            let mut chars = value[1..].char_indices().peekable();
            let mut end = None;
            while let Some((offset, character)) = chars.next() {
                if character == '\\'
                    && (quote == '"' || chars.peek().is_some_and(|(_, next)| *next == '\''))
                {
                    chars.next();
                } else if character == quote {
                    end = Some(offset + 2);
                    break;
                }
            }
            let end = end.ok_or_else(fail)?;
            if value[end..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_ascii_whitespace())
            {
                return Err(fail());
            }
            (&value[1..end - 1], end)
        } else {
            let end = value
                .find(|c: char| c.is_ascii_whitespace())
                .unwrap_or(value.len());
            (&value[..end], end)
        };
        let rebased = if key.eq_ignore_ascii_case("include") && glob_include {
            let (decoded, _) = decode_path_word(body, quote, PathWordDialect::Redis)?;
            let rebased = rebase_posix_glob_pattern(&decoded, rebase)?;
            if decoded == rebased {
                body.to_string()
            } else if quote == Some('"') {
                rebased.replace('\\', "\\\\").replace('"', "\\\"")
            } else if quote == Some('\'') {
                rebased.replace('\'', "\\'")
            } else {
                rebased
            }
        } else {
            rebase_path_word(body, quote, PathWordDialect::Redis, rebase)?
        };
        if rebased == body {
            output.push_str(line);
            continue;
        }
        output.push_str(&line[..start]);
        if let Some(quote) = quote {
            output.push(quote);
            output.push_str(&rebased);
            output.push(quote);
        } else {
            output.push('"');
            output.push_str(&rebased.replace('\\', "\\\\").replace('"', "\\\""));
            output.push('"');
        }
        output.push_str(&value[end..]);
    }
    Ok(output)
}

pub fn render_redis_conf(paths: &Paths, version: &str, port: u16) -> String {
    format!(
        r#"# NiceEnv redis.conf ({version}) — 自定义设置在重启后保留
# port/dir/daemonize 由应用管理，端口请在设置页修改
bind 127.0.0.1
protected-mode yes
port {port}
tcp-backlog 511
timeout 0
tcp-keepalive 60
daemonize no
databases 16
save ""
stop-writes-on-bgsave-error yes
appendonly no
maxmemory 256mb
maxmemory-policy allkeys-lru
dir "{dir}"
dbfilename dump.rdb
logfile ""
"#,
        version = version,
        port = port,
        dir = quoted_config_path(&paths.redis_data_dir()),
    )
}

/* ================= mihomo (Clash) ================= */

pub const MIHOMO_MIXED_PORT: u16 = 17890;
pub const MIHOMO_CONTROLLER_PORT: u16 = 19090;

pub fn render_mihomo_builtin_config() -> String {
    format!(
        r#"# NiceEnv 内置直连配置（导入订阅后自动替换）
mixed-port: {mixed}
external-controller: 127.0.0.1:{controller}
secret: ""
allow-lan: false
mode: rule
log-level: info
ipv6: false
profile:
  store-selected: true
proxies: []
proxy-groups: []
rules:
  - MATCH,DIRECT
"#,
        mixed = MIHOMO_MIXED_PORT,
        controller = MIHOMO_CONTROLLER_PORT,
    )
}

/// 只修改 YAML 根映射的托管字段，保留节点、provider 和锚点中的远端参数。
pub fn adapt_mihomo_profile(raw: &str, mode: &str) -> Result<String> {
    use yaml_serde::Value;
    if !matches!(mode, "rule" | "global" | "direct") {
        return Err(AppError::new("BAD_PROXY_MODE", "代理模式无效"));
    }
    let yaml_error = |e: yaml_serde::Error| {
        // YAML 报错可能带订阅密码或节点内容，只给出位置。
        let position = e.location().map(|p| format!("（第 {} 行，第 {} 列）", p.line(), p.column())).unwrap_or_default();
        AppError::new("NOT_A_CLASH_CONFIG", format!("订阅 YAML 格式无效{position}"))
            .with_hint("请使用 Clash / mihomo YAML 订阅，并检查格式或联系订阅提供方")
    };
    let mut value: Value = yaml_serde::from_str(raw).map_err(yaml_error)?;
    value.apply_merge().map_err(yaml_error)?;
    let root = value.as_mapping_mut().ok_or_else(|| AppError::new("NOT_A_CLASH_CONFIG", "订阅必须是 Clash YAML 配置"))?;
    for key in ["proxies", "proxy-groups", "rules"] {
        if root.get(Value::from(key)).is_some_and(|v| !v.is_sequence()) {
            return Err(AppError::new("NOT_A_CLASH_CONFIG", format!("订阅字段 {key} 必须是列表")));
        }
    }
    if root.get(Value::from("proxy-providers")).is_some_and(|v| !v.is_mapping()) {
        return Err(AppError::new("NOT_A_CLASH_CONFIG", "订阅字段 proxy-providers 必须是映射"));
    }
    let has_nodes = root.get(Value::from("proxies")).is_some_and(Value::is_sequence);
    let has_providers = root.get(Value::from("proxy-providers")).is_some_and(Value::is_mapping);
    let has_rules = root.get(Value::from("rules")).is_some_and(Value::is_sequence);
    if !has_nodes && !has_providers && !has_rules {
        return Err(AppError::new("NOT_A_CLASH_CONFIG", "订阅未包含有效节点、节点提供者或规则")
            .with_hint("裸节点链接、网页和 base64 节点列表不能直接作为 Clash 配置"));
    }
    for (key, val) in [
        ("mixed-port", Value::from(MIHOMO_MIXED_PORT)),
        ("port", Value::from(0)),
        ("socks-port", Value::from(0)),
        ("external-controller", Value::from(format!("127.0.0.1:{MIHOMO_CONTROLLER_PORT}"))),
        ("secret", Value::from("")),
        ("allow-lan", Value::from(false)),
        ("mode", Value::from(mode)),
    ] {
        root.insert(Value::from(key), val);
    }
    root.remove(Value::from("external-ui"));
    yaml_serde::to_string(&value).map_err(yaml_error)
}

/* ================= 统一生成入口（修复向导也用） ================= */

pub fn ensure_all_configs(
    paths: &Paths,
    _pools: &[(String, u16)],
    _http_port: u16,
    _https_port: u16,
    _mysql_port: u16,
    _redis_port: u16,
) -> Result<()> {
    // nginx 主配置需要 nginx 运行时目录（mime.types）；由调用方传入已安装 nginx
    // 这里只生成 fastcgi_params / rewrites / 各服务配置
    let fp = paths.etc().join("nginx").join("fastcgi_params");
    write_with_backup(&fp, FASTCGI_PARAMS, &paths.backup())?;
    Ok(())
}

pub fn write_nginx_conf(
    paths: &Paths,
    nginx_root: &std::path::Path,
    pools: &[(String, u16)],
    http_port: u16,
    https_port: u16,
    network: Option<bool>,
) -> Result<()> {
    std::fs::create_dir_all(paths.logs().join("nginx"))?;
    std::fs::create_dir_all(paths.etc().join("nginx").join("temp"))?;
    std::fs::create_dir_all(paths.etc().join("nginx").join("run"))?;
    let adminer = adminer_path(paths);
    let conf = render_nginx_conf(
        paths,
        nginx_root,
        http_port,
        https_port,
        pools,
        adminer.as_deref(),
    );
    let path = paths.nginx_conf();
    let previous = previous_config(&path)?;
    let conf = match previous.as_deref() {
        Some(current) => sync_nginx_config(current, &conf, paths)?,
        None => conf,
    };
    let conf = match network { Some(enabled) => network_listeners(&conf, enabled, true)?, None => conf };
    crate::tls::ensure_server_fallback(paths)?;
    localize_legacy_nginx_sites(paths, network)?;
    publish_config(paths, "nginx-main", &path, &conf, previous.as_deref())?;
    let fp = paths.etc().join("nginx").join("fastcgi_params");
    if !fp.exists() {
        std::fs::write(&fp, FASTCGI_PARAMS)?;
    }
    Ok(())
}

pub fn write_php_ini(paths: &Paths, version: &str) -> Result<()> {
    let runtime_dir = paths.runtime_dir("php", version);
    std::fs::create_dir_all(paths.logs().join("php").join(version))?;
    std::fs::create_dir_all(paths.data().join("php").join(version).join("sess"))?;
    // php.ini 是扩展开关与配置编辑器共同保存的用户配置，启动时不能重置。
    if paths.php_ini(version).is_file() {
        let path = paths.php_ini(version);
        let content = std::fs::read_to_string(&path)?;
        let repaired = crate::phpext::repair_gd_directive(&content, &runtime_dir, version);
        if repaired != content { write_with_backup(&path, &repaired, &paths.backup())?; }
        return Ok(());
    }
    let ini = render_php_ini(paths, version, &runtime_dir);
    write_with_backup(&paths.php_ini(version), &ini, &paths.backup())?;
    Ok(())
}

pub fn write_mysql_ini(
    paths: &Paths,
    version: &str,
    basedir: &std::path::Path,
    port: u16,
) -> Result<()> {
    std::fs::create_dir_all(paths.logs().join("mysql"))?;
    let path = paths.mysql_ini(version);
    let previous = previous_config(&path)?;
    let ini = previous.as_deref().map_or_else(
        || render_mysql_ini(paths, version, basedir, port),
        |current| sync_mysql_config(current, paths, version, basedir, port),
    );
    publish_config(
        paths,
        &format!("mysql-ini@{version}"),
        &path,
        &ini,
        previous.as_deref(),
    )?;
    Ok(())
}

pub fn write_redis_conf(paths: &Paths, version: &str, port: u16) -> Result<()> {
    std::fs::create_dir_all(paths.redis_data_dir())?;
    let path = paths.redis_conf(version);
    let previous = previous_config(&path)?;
    let conf = previous.as_deref().map_or_else(
        || render_redis_conf(paths, version, port),
        |current| sync_redis_config(current, paths, port),
    );
    publish_config(
        paths,
        &format!("redis-conf@{version}"),
        &path,
        &conf,
        previous.as_deref(),
    )?;
    Ok(())
}

/* ================= MongoDB mongod.conf ================= */

/// MongoDB 的托管路径和认证开关由启动参数控制；此文件专门保存用户的其它 YAML 设置。
/// 这样配置编辑器可以安全提供版本级入口，同时不会让端口或数据目录被用户误改后
/// 影响应用对实例生命周期的核验。
pub fn write_mongodb_conf(paths: &Paths, version: &str) -> Result<()> {
    let path = paths.mongo_conf(version);
    let previous = previous_config(&path)?;
    let content = previous.clone().unwrap_or_else(|| {
        format!(
            "# NiceEnv MongoDB {version} config\n# storage.dbPath, systemLog.path, net.bindIp, net.port and security.authorization are supplied by NiceEnv at launch.\n# Add other mongod YAML options here; restart the service after saving.\n"
        )
    });
    publish_config(
        paths,
        &format!("mongo-conf@{version}"),
        &path,
        &content,
        previous.as_deref(),
    )?;
    Ok(())
}

pub fn write_mihomo_config(paths: &Paths, content: &str) -> Result<()> {
    std::fs::create_dir_all(paths.mihomo_dir().join("profiles"))?;
    write_with_backup(&paths.mihomo_config(), content, &paths.backup())?;
    Ok(())
}

/// Nginx 的 -p/-c 参数；调用命令必须将工作目录设为 root。
pub(crate) fn nginx_config_args(root: &Path, conf: &Path) -> Result<Vec<String>> {
    let prefix = portable_path_text(root);
    let config = portable_path_text(conf);
    #[cfg(windows)]
    if !prefix.is_ascii() || !config.is_ascii() {
        // nginx 的配置文件按 UTF-8 读取，但 Windows 的 main(argv) 仍使用系统代码页。
        // 工作目录由 CreateProcessW 传递；共同的非 ASCII 目录不再经过 argv。
        let base = prefix.trim_end_matches('/').split('/').collect::<Vec<_>>();
        let target = config.split('/').collect::<Vec<_>>();
        let shared = base
            .iter()
            .zip(&target)
            .take_while(|(left, right)| left.eq_ignore_ascii_case(right))
            .count();
        let same_volume = if prefix.starts_with("//") {
            shared >= 4
        } else {
            shared >= 1
        };
        let relative = if same_volume {
            std::iter::repeat_n("..", base.len() - shared)
                .chain(target[shared..].iter().copied())
                .collect::<Vec<_>>()
                .join("/")
        } else {
            config.clone()
        };
        if !relative.is_ascii() {
            return Err(AppError::new("NGINX_COMMAND_PATH", "Nginx 无法读取此配置路径中的特殊字符")
                .with_hint("请将 Nginx 程序和配置放在同一个数据目录下，并让其内部的子目录和配置文件名使用英文。共同的数据目录名称可以包含中文。"));
        }
        return Ok(vec!["-p".into(), "./".into(), "-c".into(), relative]);
    }
    Ok(vec!["-p".into(), prefix, "-c".into(), config])
}

/// 校验 nginx 配置语法：nginx -t
pub fn validate_nginx(nginx_exe: &std::path::Path, conf: &std::path::Path) -> Result<()> {
    let root = nginx_exe.parent().ok_or_else(nginx_structure_error)?;
    let mut command = platform::command(nginx_exe);
    command
        .current_dir(root)
        .args(nginx_config_args(root, conf)?)
        .arg("-t");
    let (ok, output) = crate::cfgeditor::run_validator(&mut command)?;
    if !ok {
        return Err(
            AppError::new("NGINX_CONF_INVALID", "nginx 配置语法校验失败")
                .with_hint("请检查高级设置里改过的内容；系统会在修改前自动备份")
                .with_detail(output),
        );
    }
    Ok(())
}

/* ================= Apache httpd ================= */

const HTTPD_PROXY_HTTP_MODULE: &str = "# NiceEnv HTTP reverse proxy support\n<IfModule !proxy_http_module>\n    LoadModule proxy_http_module modules/mod_proxy_http.so\n</IfModule>\n";

fn httpd_config_text(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn httpd_argument(value: &str) -> String {
    let value = value.trim();
    let quote = value.chars().next().filter(|c| matches!(c, '\'' | '"'));
    let value = quote
        .and_then(|quote| value.strip_prefix(quote)?.strip_suffix(quote))
        .unwrap_or(value);
    let mut result = String::new();
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\'
            && chars
                .peek()
                .is_some_and(|next| *next == '\\' || Some(*next) == quote)
        {
            result.push(chars.next().unwrap());
        } else {
            result.push(character);
        }
    }
    result
}

fn httpd_glob_text(value: &str) -> String {
    let mut pattern = String::new();
    for character in value.chars() {
        // Windows APR 会先把反斜杠当作分隔符；用字符类引用通配符，不能用 \\[。
        match character {
            '[' => pattern.push_str("[[]"),
            ']' => pattern.push_str("[]]"),
            '*' => pattern.push_str("[*]"),
            '?' => pattern.push_str("[?]"),
            '\\' => pattern.push_str("\\\\"),
            _ => pattern.push(character),
        }
    }
    pattern
}

pub(crate) fn httpd_sites_pattern(paths: &Paths) -> String {
    format!(
        "{}/*.conf",
        httpd_glob_text(&crate::paths::portable_path_text(&paths.apache_sites_dir()))
    )
}

pub(crate) fn rebase_httpd_config(
    content: &str,
    rebase: &crate::paths::DataPathRebase,
) -> Result<String> {
    let bytes = content.as_bytes();
    let mut at = 0;
    let mut directive = String::new();
    let mut argument = 0;
    let mut first_argument = String::new();
    let mut block = false;
    let mut changes = Vec::new();
    let mut variables = Vec::new();
    let mut path_values = Vec::new();
    let mut business_values = Vec::new();
    while at < bytes.len() {
        if bytes[at..].starts_with(b"\\\r\n") {
            at += 3;
            continue;
        }
        if bytes[at..].starts_with(b"\\\n") {
            at += 2;
            continue;
        }
        if bytes[at].is_ascii_whitespace() {
            if bytes[at] == b'\n' {
                directive.clear();
            }
            at += 1;
            continue;
        }
        if (directive.is_empty() && bytes[at] == b'#') || (block && bytes[at] == b'>') {
            while at < bytes.len() && bytes[at] != b'\n' {
                if bytes[at..].starts_with(b"\\\r\n") {
                    at += 3;
                } else if bytes[at..].starts_with(b"\\\n") {
                    at += 2;
                } else {
                    at += 1;
                }
            }
            continue;
        }
        let start = at;
        let quote = matches!(bytes[at], b'\'' | b'"').then_some(bytes[at]);
        if let Some(quote) = quote {
            at += 1;
            while at < bytes.len() && bytes[at] != quote {
                if bytes[at] == b'\\'
                    && bytes
                        .get(at + 1)
                        .is_some_and(|next| *next == quote || *next == b'\\')
                {
                    at += 1;
                }
                at += 1;
            }
            if at == bytes.len() {
                return Err(AppError::new(
                    "DATA_DIR_CONFIG_SYNTAX",
                    "Apache 配置引号未闭合，无法转换迁移路径",
                ));
            }
            at += 1;
        } else {
            while at < bytes.len()
                && !bytes[at].is_ascii_whitespace()
                && !(block && bytes[at] == b'>')
            {
                if bytes[at..].starts_with(b"\\\r\n") {
                    at += 3;
                } else if bytes[at..].starts_with(b"\\\n") {
                    at += 2;
                } else {
                    at += 1;
                }
            }
        }
        let raw = content[start..at].replace("\\\r\n", "").replace("\\\n", "");
        if directive.is_empty() {
            directive = raw
                .trim_start_matches(['<', '/'])
                .trim_end_matches('>')
                .to_ascii_lowercase();
            block = bytes[start] == b'<';
            argument = 0;
            first_argument.clear();
            continue;
        }
        argument += 1;
        let decoded = httpd_argument(&raw);
        if argument == 1 {
            first_argument.clone_from(&decoded);
        }
        if directive == "define" && argument == 2 {
            variables.push((first_argument.clone(), decoded.clone()));
        }
        // Alias 在 Location 块中允许省略 URL，只保留一个磁盘路径参数。
        let mut next = at;
        while next < bytes.len() {
            if bytes[next..].starts_with(b"\\\r\n") {
                next += 3;
            } else if bytes[next..].starts_with(b"\\\n") {
                next += 2;
            } else if matches!(bytes[next], b' ' | b'\t' | b'\r') {
                next += 1;
            } else {
                break;
            }
        }
        let last_argument = next == bytes.len() || bytes[next] == b'\n';
        let value = match directive.as_str() {
            "directory" | "include" | "includeoptional" if argument == 1 => {
                path_values.push(decoded.clone());
                rebase.config_value(&decoded, httpd_glob_text)?
            }
            "directorymatch" if argument == 1 => {
                path_values.push(decoded.clone());
                rebase.config_pattern(&decoded)?
            }
            "rewriterule" if argument == 2 && decoded.starts_with("fcgi://") => {
                path_values.push(decoded.clone());
                if let Some(slash) = decoded[7..].find('/').map(|index| index + 8) {
                    format!(
                        "{}{}",
                        &decoded[..slash],
                        rebase.config_value(&decoded[slash..], str::to_owned)?
                    )
                } else {
                    decoded.clone()
                }
            }
            "serverroot"
            | "documentroot"
            | "defaultruntimedir"
            | "pidfile"
            | "errorlog"
            | "customlog"
            | "globallog"
            | "transferlog"
            | "typesconfig"
            | "authuserfile"
            | "authgroupfile"
            | "sslcertificatefile"
            | "sslcertificatekeyfile"
            | "sslcertificatechainfile"
            | "sslcacertificatefile"
            | "sslcacertificatepath"
            | "sslcarevocationfile"
            | "sslcarevocationpath"
            | "sslsessionticketkeyfile"
            | "sslproxycacertificatefile"
            | "sslproxycacertificatepath"
            | "sslproxycarevocationfile"
            | "sslproxycarevocationpath"
            | "sslproxymachinecertificatefile"
            | "sslproxymachinecertificatepath"
            | "sslproxymachinecertificatechainfile"
            | "cacheroot"
            | "davlockdb"
            | "scoreboardfile"
            | "chrootdir"
                if argument == 1 =>
            {
                path_values.push(decoded.clone());
                rebase.config_value(&decoded, str::to_owned)?
            }
            "alias" | "scriptalias" if argument == 2 || (argument == 1 && last_argument) => {
                path_values.push(decoded.clone());
                rebase.config_value(&decoded, str::to_owned)?
            }
            "aliasmatch" | "scriptaliasmatch" | "loadmodule" if argument == 2 => {
                path_values.push(decoded.clone());
                rebase.config_value(&decoded, str::to_owned)?
            }
            "loadfile" => {
                path_values.push(decoded.clone());
                rebase.config_value(&decoded, str::to_owned)?
            }
            "sslsessioncache" | "sslstaplingcache" if argument == 1 => {
                path_values.push(decoded.clone());
                if let Some((backend, path)) = decoded
                    .split_once(':')
                    .filter(|(backend, _)| matches!(*backend, "shmcb" | "dbm"))
                {
                    format!("{backend}:{}", rebase.config_value(path, str::to_owned)?)
                } else {
                    rebase.config_value(&decoded, str::to_owned)?
                }
            }
            // NiceEnv 生成的目录宏；任意 Define/SetEnv 的值属于用户业务数据。
            "define" if argument == 2 && first_argument == "NSB_ETC" => {
                rebase.config_value(&decoded, str::to_owned)?
            }
            _ => {
                if directive != "define" {
                    business_values.push(decoded.clone());
                }
                decoded.clone()
            }
        };
        if value != decoded {
            changes.push((start, at, format!("\"{}\"", httpd_config_text(&value))));
        }
    }
    validate_rebase_variables(&variables, &path_values, rebase, "Apache", |name| {
        name != "NSB_ETC"
    })?;
    validate_rebase_variables(&variables, &business_values, rebase, "Apache", |name| {
        name == "NSB_ETC"
    })?;
    let mut output = content.to_string();
    for (start, end, value) in changes.into_iter().rev() {
        output.replace_range(start..end, &value);
    }
    Ok(output)
}

fn sync_httpd_config(current: &str, paths: &Paths, root: &Path, http: u16, https: u16) -> String {
    let sites_pattern = httpd_sites_pattern(paths);
    let sites = format!("\"{}\"", httpd_config_text(&sites_pattern));
    let managed_certificate = |line: &str, ext: &str| {
        line.split_once(char::is_whitespace)
            .is_some_and(|(_, value)| {
                let value = crate::paths::portable_path_text(Path::new(&httpd_argument(value)));
                value == format!("${{NSB_ETC}}/ssl-dummy.{ext}")
                    || value
                        == format!(
                            "{}/ssl-dummy.{ext}",
                            crate::paths::portable_path_text(&paths.etc().join("apache"))
                        )
                    || value
                        == format!(
                            "{}/fallback/localhost.{ext}",
                            crate::paths::portable_path_text(&paths.certs())
                        )
            })
    };
    let replacements = [
        format!("ServerRoot \"{}\"", quoted_config_path(root)),
        format!(
            "Define NSB_ETC \"{}\"",
            quoted_config_path(&paths.etc().join("apache"))
        ),
        format!("Listen 127.0.0.1:{http}\nListen 127.0.0.1:{https} https"),
        format!(
            "TypesConfig \"{}/conf/mime.types\"",
            quoted_config_path(root)
        ),
        format!("IncludeOptional {sites}"),
        format!(
            "SSLCertificateFile \"{}/fallback/localhost.crt\"",
            quoted_config_path(&paths.certs())
        ),
        format!(
            "SSLCertificateKeyFile \"{}/fallback/localhost.key\"",
            quoted_config_path(&paths.certs())
        ),
    ];
    let groups: Vec<_> = replacements
        .into_iter()
        .enumerate()
        // TLS 默认证书只替换应用原有行，不向用户自定义全局 TLS 配置追加覆盖项。
        .map(|(index, line)| (line.clone(), if index >= 5 { String::new() } else { line }))
        .collect();
    let mut depth = 0usize;
    let mut output = sync_managed_lines(current, &groups, &[0, 1], |line| {
        if line.starts_with('#') {
            return None;
        }
        if line.starts_with("</") {
            depth = depth.saturating_sub(1);
            return None;
        }
        if line.starts_with('<') {
            depth += 1;
            return None;
        }
        if depth != 0 {
            return None;
        }
        let mut words = line.split_whitespace();
        match words.next()?.to_ascii_lowercase().as_str() {
            "serverroot" => Some(0),
            "define" if words.next() == Some("NSB_ETC") => Some(1),
            "listen" => Some(2),
            "typesconfig" => Some(3),
            "sslcertificatefile" if managed_certificate(line, "crt") => Some(5),
            "sslcertificatekeyfile" if managed_certificate(line, "key") => Some(6),
            "includeoptional" | "include"
                if line
                    .split_once(char::is_whitespace)
                    .is_some_and(|(_, path)| {
                        let path =
                            crate::paths::portable_path_text(Path::new(&httpd_argument(path)));
                        path == sites_pattern
                            || path == "${NSB_ETC}/sites/*.conf"
                            || path
                                == format!(
                                    "{}/*.conf",
                                    crate::paths::portable_path_text(&paths.apache_sites_dir())
                                )
                    }) =>
            {
                Some(4)
            }
            _ => None,
        }
    });
    if !output
        .replace("\r\n", "\n")
        .contains(HTTPD_PROXY_HTTP_MODULE.trim_end())
    {
        let newline = if current.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        if !output.is_empty() && !output.ends_with('\n') {
            output.push_str(newline);
        }
        output.push_str(&HTTPD_PROXY_HTTP_MODULE.replace('\n', newline));
    }
    output
}

/// 渲染 httpd.conf（ApacheLounge Apache24；pools 为运行中的 php 池）
pub fn render_httpd_conf(
    paths: &Paths,
    apache_root: &std::path::Path,
    _pools: &[(String, u16)],
    http_port: u16,
    https_port: u16,
) -> String {
    let root = quoted_config_path(apache_root);
    let etc = quoted_config_path(&paths.etc().join("apache"));
    // php balancer 定义在各站点 vhost 内（BalancerMember 需携带 docroot 路径）
    // MPM 在 ApacheLounge 发行中为静态编译（无 mod_mpm_*.so），不 LoadModule
    format!(
        r#"# NiceEnv httpd.conf — 自定义模块与全局配置在重启和更新站点后保留
# ServerRoot、NSB_ETC、Listen、TypesConfig、站点 include 由应用管理
# 端口请在设置页修改
ServerRoot "{root}"
Define NSB_ETC "{etc}"

Listen 127.0.0.1:{http_port}
Listen 127.0.0.1:{https_port} https

LoadModule authz_core_module modules/mod_authz_core.so
LoadModule authz_host_module modules/mod_authz_host.so
LoadModule log_config_module modules/mod_log_config.so
LoadModule mime_module modules/mod_mime.so
LoadModule dir_module modules/mod_dir.so
LoadModule env_module modules/mod_env.so
LoadModule headers_module modules/mod_headers.so
LoadModule setenvif_module modules/mod_setenvif.so
LoadModule rewrite_module modules/mod_rewrite.so
LoadModule proxy_module modules/mod_proxy.so
LoadModule proxy_fcgi_module modules/mod_proxy_fcgi.so
{proxy_http_module}
LoadModule ssl_module modules/mod_ssl.so
LoadModule socache_shmcb_module modules/mod_socache_shmcb.so

ServerName 127.0.0.1:{http_port}
PidFile "${{NSB_ETC}}/run/httpd.pid"
ErrorLog "${{NSB_ETC}}/logs/error.log"
LogLevel warn
CustomLog "${{NSB_ETC}}/logs/access.log" common

SSLCertificateFile "{certs}/fallback/localhost.crt"
SSLCertificateKeyFile "{certs}/fallback/localhost.key"
SSLSessionCache "shmcb:${{NSB_ETC}}/logs/ssl_scache(512000)"

DocumentRoot "${{NSB_ETC}}/htdocs"
<Directory "/">
    AllowOverride All
    Require all granted
</Directory>

TypesConfig "{root}/conf/mime.types"
DirectoryIndex index.php index.html index.htm

IncludeOptional "{sites}"
"#,
        root = root,
        etc = etc,
        certs = quoted_config_path(&paths.certs()),
        sites = httpd_config_text(&httpd_sites_pattern(paths)),
        proxy_http_module = HTTPD_PROXY_HTTP_MODULE,
        http_port = http_port,
        https_port = https_port,
    )
}

/// 单站点 Apache vhost；php_pool 为该版本 php-cgi 池的起始端口（BalancerMember × workers）
pub fn render_httpd_vhost(
    site: &Site,
    http_port: u16,
    https_port: u16,
    cert_dir: &std::path::Path,
    php_pool: Option<u16>,
) -> String {
    render_httpd_vhost_with_auth(site, http_port, https_port, cert_dir, php_pool, None)
}

pub fn render_httpd_vhost_with_auth(
    site: &Site,
    http_port: u16,
    https_port: u16,
    cert_dir: &std::path::Path,
    php_pool: Option<u16>,
    auth_file: Option<&std::path::Path>,
) -> String {
    let server_names = site.domains.join(" ");
    let access = crate::siteaccess::apache(site.runtime.access.as_ref());
    let error_pages = apache_error_pages(site.runtime.error_pages.as_ref());
    let basic_auth = auth_file.map(|path| format!("    AuthType Basic\n    AuthName \"Restricted\"\n    AuthBasicProvider file\n    AuthUserFile \"{}\"\n    Require valid-user\n", quoted_config_path(path))).unwrap_or_default();
    let primary = site
        .domains
        .first()
        .cloned()
        .unwrap_or_else(|| "localhost".into());
    // 文档根与保护规则使用同一个真实路径，避免 Apache 规范化 .. 或目录联接后失配。
    let root_path = std::path::Path::new(&site.root_dir);
    let root = std::fs::canonicalize(root_path)
        .map(|path| crate::paths::portable_path_text(&path))
        .unwrap_or_else(|_| crate::paths::portable_path_text(root_path));
    // DirectoryMatch 匹配完整磁盘路径：只保护文档根内部的隐藏目录，
    // 不能因为项目位于 .work / .tmp 等父目录中就拒绝整个站点。
    let root_pattern = regex::escape(root.trim_end_matches('/'));
    let root_pattern = if cfg!(windows) {
        format!("(?i:{root_pattern})")
    } else {
        root_pattern
    };
    let root_pattern = httpd_config_text(&root_pattern);
    let directory = httpd_config_text(&httpd_glob_text(&root));
    let root = httpd_config_text(&root);

    let (listen, ssl_lines) = if site.https {
        let (certificate, key) = site_certificate_files(site, cert_dir);
        (
            format!("<VirtualHost *:{https_port}>"),
            format!(
                "    SSLEngine on\n    SSLCertificateFile \"{}\"\n    SSLCertificateKeyFile \"{}\"",
                quoted_config_path(&certificate),
                quoted_config_path(&key),
            ),
        )
    } else {
        (format!("<VirtualHost *:{http_port}>"), String::new())
    };

    let body = match &site.runtime.kind {
        crate::model::SiteKind::Redirect => redirect_directives(site, true),
        crate::model::SiteKind::Php => {
            let mut s = String::new();
            if let Some(base) = php_pool {
                // mod_proxy_balancer 依赖 slotmem-shm，Windows 重启存在已知缺陷；
                // 改为每站点直连池内一个 worker（按站点 id 轮转，4 worker 跨站点分摊）。
                // Handler 在 Apache 完成磁盘路径解析后转交 PHP，保留 DirectoryIndex 和 PATH_INFO。
                // 不再把磁盘路径拼入 RewriteRule URL 或 ap_expr 字符串。
                let worker = base
                    + (site.id.chars().map(|c| c as usize).sum::<usize>()
                        % PHP_POOL_WORKERS as usize) as u16;
                s.push_str(&format!(
                    "    <FilesMatch \"\\.ph(p[3457]?|t|tml)$\">\n        SetHandler \"proxy:fcgi://127.0.0.1:{worker}/\"\n        ProxyFCGIBackendType GENERIC\n"
                ));
                if cfg!(windows) {
                    // mod_proxy 给盘符前添加的 URL 斜杠不能传给 Windows php-cgi。
                    s.push_str("        ProxyFCGISetEnvIf \"reqenv('SCRIPT_FILENAME') =~ m|^/([A-Za-z]:/.*)$|\" SCRIPT_FILENAME \"$1\"\n");
                }
                s.push_str("    </FilesMatch>\n");
            } else {
                s.push_str(
                    "    <FilesMatch \"\\.php$\">\n        Require all denied\n    </FilesMatch>\n",
                );
            }
            s.push_str(&format!("    <Directory \"{directory}\">\n        AllowOverride All\n        Require all granted\n"));
            if site.runtime.custom_rewrite.is_none() {
                // 目录映射完成后才能判断实际脚本是否存在，保留 /index.php/path 的 PATH_INFO。
                s.push_str("        RewriteEngine On\n        RewriteCond %{REQUEST_FILENAME} !-d\n        RewriteCond %{REQUEST_FILENAME} !-f\n        RewriteRule ^ /index.php [QSA,L]\n");
            }
            s.push_str("    </Directory>\n");
            if let Some(custom) = &site.runtime.custom_rewrite {
                s.push_str("    RewriteEngine On\n");
                s.push_str(&custom.content);
                s.push('\n');
            }
            s
        }
        crate::model::SiteKind::Static => {
            let rewrite = match site.rewrite {
                RewritePreset::NextExport => "        RewriteEngine On\n        RewriteCond %{REQUEST_FILENAME} !-f\n        RewriteCond %{REQUEST_FILENAME} !-d\n        RewriteCond %{REQUEST_FILENAME}.html -f\n        RewriteRule ^(.+?)/?$ $1.html [END]\n",
                RewritePreset::SpaFallback => "        RewriteEngine On\n        RewriteCond %{REQUEST_FILENAME} !-f\n        RewriteCond %{REQUEST_FILENAME} !-d\n        RewriteRule ^ index.html [END]\n",
                _ => "",
            };
            let rewrite = site
                .runtime
                .custom_rewrite
                .as_ref()
                .map(|r| format!("{}\n", r.content))
                .unwrap_or_else(|| rewrite.to_string());
            let error_page = if matches!(site.rewrite, RewritePreset::NextExport) {
                "    ErrorDocument 404 /404.html\n"
            } else {
                ""
            };
            format!("    <FilesMatch \"(?i)\\.(?:php[0-9]*|phtml|pht|phar)(?:\\.|$)\">\n        Require all denied\n    </FilesMatch>\n    <Directory \"{directory}\">\n        AllowOverride All\n        Options -Indexes\n        DirectoryIndex index.html index.htm\n        Require all granted\n{rewrite}    </Directory>\n{error_page}")
        }
        _ => {
            let target = site
                .runtime
                .proxy_target
                .clone()
                .unwrap_or_else(|| "127.0.0.1:8080".into());
            let mut target =
                crate::sites::proxy_url(&target).unwrap_or_else(|_| "http://127.0.0.1:1/".into());
            if !target.ends_with('/') {
                target.push('/');
            }
            format!(
                "    SSLProxyEngine On\n    ProxyErrorOverride On\n    ProxyPreserveHost Off\n    ProxyPass / \"{target}\"\n    ProxyPassReverse / \"{target}\"\n"
            )
        }
    };

    let vhost = format!(
        r#"# site: {name} ({id}) — NiceEnv 托管
{listen}
    ServerName {primary}
    ServerAlias {server_names}
    {document_root}
    CustomLog "${{NSB_ETC}}/logs/{id}.access.log" "%h %l %u %t \"%r\" %>s %b"
    ErrorLog "${{NSB_ETC}}/logs/{id}.error.log"
    {ssl_lines}

    <FilesMatch "^\.">
        Require all denied
    </FilesMatch>
    <DirectoryMatch "^{root_pattern}/(?:[^/]+/)*\.(?!well-known(?:/|$))">
        Require all denied
    </DirectoryMatch>

{access}
{basic_auth}
{error_pages}
{cors}
{proxy_rules}
{body}
</VirtualHost>
"#,
        name = site.name,
        id = site.id,
        listen = listen,
        primary = primary,
        server_names = server_names,
        document_root = if site.runtime.kind == crate::model::SiteKind::Redirect {
            String::new()
        } else {
            format!("DocumentRoot \"{root}\"")
        },
        cors = site
            .runtime
            .cors
            .as_ref()
            .map(crate::sitecors::apache)
            .unwrap_or_default(),
        error_pages = error_pages,
        basic_auth = basic_auth,
        proxy_rules = crate::siteproxy::apache(&site.runtime),
        ssl_lines = ssl_lines,
        body = body,
    );
    if site.https {
        if let Some(status) = site.runtime.https_redirect {
            let suffix = https_port_suffix(https_port);
            let hosts = https_redirect_hosts(site);
            let http = format!("# NiceEnv HTTP to HTTPS\n<VirtualHost *:{http_port}>\n    ServerName {primary}\n    ServerAlias {server_names}\n    CustomLog \"${{NSB_ETC}}/logs/{}.access.log\" \"%h %l %u %t \\\"%r\\\" %>s %b\"\n    ErrorLog \"${{NSB_ETC}}/logs/{}.error.log\"\n    AllowEncodedSlashes NoDecode\n    RewriteEngine On\n{access}    RewriteCond %{{THE_REQUEST}} \"\\s(/[^\\s?]*)(?:\\?[^\\s]*)?\\s\"\n    RewriteRule ^ - [E=NSB_HTTPS_URI:%1]\n    RewriteCond %{{ENV:NSB_HTTPS_URI}} !^/\n    RewriteRule ^ - [R=400,L]\n    RewriteCond %{{HTTP_HOST}} \"^({hosts})(?::[0-9]+)?$\" [NC]\n    RewriteRule ^ \"https://%1{suffix}%{{ENV:NSB_HTTPS_URI}}\" [R={status},L,NE]\n    RewriteRule ^ - [R=421,L]\n</VirtualHost>\n", site.id, site.id);
            return format!("{http}\n{vhost}");
        }
        let http = vhost
            .replace(&listen, &format!("<VirtualHost *:{http_port}>"))
            .replace(&ssl_lines, "");
        format!("{http}\n{vhost}")
    } else {
        vhost
    }
}

fn apache_error_pages(pages: Option<&std::collections::BTreeMap<u16, String>>) -> String {
    pages.into_iter().flatten()
        .map(|(status, path)| format!("    ErrorDocument {status} {path}\n"))
        .collect()
}

pub fn write_httpd_conf(
    paths: &Paths,
    apache_root: &std::path::Path,
    pools: &[(String, u16)],
    http_port: u16,
    https_port: u16,
    network: Option<bool>,
) -> Result<()> {
    std::fs::create_dir_all(paths.apache_sites_dir())?;
    std::fs::create_dir_all(paths.apache_run_dir())?;
    std::fs::create_dir_all(paths.etc().join("apache").join("logs"))?;
    std::fs::create_dir_all(paths.etc().join("apache").join("htdocs"))?;
    let path = paths.apache_conf();
    let previous = previous_config(&path)?;
    let conf = previous.as_deref().map_or_else(
        || render_httpd_conf(paths, apache_root, pools, http_port, https_port),
        |current| sync_httpd_config(current, paths, apache_root, http_port, https_port),
    );
    let conf = if network == Some(true) { conf.replace("Listen 127.0.0.1:", "Listen 0.0.0.0:") } else { conf };
    crate::tls::ensure_server_fallback(paths)?;
    publish_config(paths, "apache-conf", &path, &conf, previous.as_deref())?;
    Ok(())
}

/// 校验 httpd 配置语法：httpd -t
pub fn validate_httpd(httpd_exe: &std::path::Path, conf: &std::path::Path) -> Result<()> {
    let root = httpd_exe
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| AppError::new("BAD_CONFIG_PATH", "Apache 安装目录无效"))?;
    let mut command = platform::command(httpd_exe);
    command
        .current_dir(root)
        .arg("-d")
        .arg(crate::paths::portable_path_text(root))
        .arg("-t")
        .arg("-f")
        .arg(crate::paths::portable_path_text(conf));
    let (ok, output) = crate::cfgeditor::run_validator(&mut command)?;
    if !ok {
        return Err(
            AppError::new("HTTPD_CONF_INVALID", "Apache 配置校验未通过").with_detail(output)
        );
    }
    Ok(())
}

/// Adminer 单文件位置（若已安装）
pub fn adminer_path(paths: &Paths) -> Option<std::path::PathBuf> {
    let mut versions: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(paths.runtimes().join("adminer")) {
        for e in rd.filter_map(|e| e.ok()) {
            if e.path().is_dir() {
                if let Some(n) = e.file_name().to_string_lossy().split('/').last() {
                    versions.push(n.to_string());
                }
            }
        }
    }
    versions.sort();
    versions.reverse();
    for v in versions {
        let p = paths
            .runtimes()
            .join("adminer")
            .join(&v)
            .join("adminer.php");
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod managed_config_tests {
    use super::*;
    use crate::paths::nginx_path;

    #[test]
    fn mihomo_adaptation_preserves_nested_ports_and_yaml_merges() {
        let raw = r#"
defaults: &defaults
  type: ss
  port: 443
  cipher: aes-128-gcm
  password: fixture-only
proxies:
  - <<: *defaults
    name: quoted-node
    server: example.test
  - {name: inline-node, type: socks5, server: 127.0.0.1, port: 9091}
proxy-providers:
  local: {type: file, path: local.yaml, health-check: {enable: false, port: 1234}}
proxy-groups: []
mixed-port: 8888
'port': 8889
mode: global
external-controller: 0.0.0.0:9090
secret: fixture-secret
"#;
        let adapted = adapt_mihomo_profile(raw, "direct").unwrap();
        let value: yaml_serde::Value = yaml_serde::from_str(&adapted).unwrap();
        assert_eq!(value["mixed-port"].as_u64(), Some(MIHOMO_MIXED_PORT as u64));
        assert_eq!(value["port"].as_u64(), Some(0));
        assert_eq!(value["proxies"][0]["port"].as_u64(), Some(443));
        assert_eq!(value["proxies"][1]["port"].as_u64(), Some(9091));
        assert_eq!(value["proxy-providers"]["local"]["health-check"]["port"].as_u64(), Some(1234));
        assert_eq!(value["mode"].as_str(), Some("direct"));
        assert_eq!(value["external-controller"].as_str(), Some("127.0.0.1:19090"));
        assert_eq!(value["secret"].as_str(), Some(""));
        assert_eq!(value["proxies"][0]["password"].as_str(), Some("fixture-only"));
    }

    #[test]
    fn mihomo_adaptation_accepts_provider_and_json_configs_and_rejects_invalid_yaml() {
        for raw in [r#"{"proxies":[],"rules":["MATCH,DIRECT"]}"#, "proxy-providers: {}\nproxy-groups: []", "rules: [MATCH,DIRECT]"] {
            assert!(adapt_mihomo_profile(raw, "rule").is_ok(), "{raw}");
        }
        for raw in ["<html>proxies</html>", "proxies: [", "proxies: {}\nrules: []", "proxies: []\nproxies: []", "---\nproxies: []\n---\nrules: []"] {
            assert!(adapt_mihomo_profile(raw, "rule").is_err(), "{raw}");
        }
        let error = adapt_mihomo_profile("password: fixture-secret\nproxies: [", "rule").unwrap_err();
        assert!(!error.message.contains("fixture-secret"));
        assert_eq!(adapt_mihomo_profile("proxies: []", "bad").unwrap_err().code, "BAD_PROXY_MODE");
    }

    fn fixture() -> (tempfile::TempDir, Paths) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("config with spaces"));
        paths.ensure_dirs().unwrap();
        (temp, paths)
    }

    #[test]
    fn nginx_keeps_user_blocks_and_comments_when_ports_pools_and_runtime_change() {
        let (_temp, paths) = fixture();
        let custom = r#"    # user: braces { } and ; do not define blocks
    map $host $custom_value { default "quoted # { ; }"; }
    upstream custom_api { server 127.0.0.1:9001; }
    server { listen 127.0.0.1:8999; server_name custom.test; return 200 "${custom_value}"; }
"#;
        let initial = render_nginx_conf(
            &paths,
            Path::new("C:/old/conf-root"),
            8080,
            8443,
            &[("8.2.0".into(), 9100)],
            None,
        )
        .replace(
            "worker_processes  2;",
            "worker_processes  4; # user workers",
        )
        .replace("gzip on;", "gzip off; # custom compression")
        .replace("http {\n", &format!("http {{\n{custom}"));
        let generated = render_nginx_conf(
            &paths,
            Path::new("C:/new/conf-root"),
            8081,
            8444,
            &[("8.4.0".into(), 9110)],
            None,
        );
        let output = sync_nginx_config(&initial, &generated, &paths).unwrap();
        assert!(output.contains(custom));
        assert!(output.contains("worker_processes  4; # user workers"));
        assert!(output.contains("gzip off; # custom compression"));
        assert!(output.contains("listen 127.0.0.1:8081;"));
        assert!(output.contains("listen 127.0.0.1:8444 ssl;"));
        assert!(output.contains("nsb_php_8_4_0"));
        assert!(!output.contains("nsb_php_8_2_0"));
        assert!(!output.contains("C:/old/conf-root"));
        assert_eq!(
            sync_nginx_config(&output, &generated, &paths).unwrap(),
            output
        );
        let crlf = output.replace('\n', "\r\n");
        assert_eq!(sync_nginx_config(&crlf, &generated, &paths).unwrap(), crlf);
        let missing_bucket = output.replace("    server_names_hash_bucket_size 512;\n", "");
        assert!(sync_nginx_config(&missing_bucket, &generated, &paths).unwrap().contains("server_names_hash_bucket_size 512;"));
        let custom_bucket = output.replace("server_names_hash_bucket_size 512;", "server_names_hash_bucket_size 128;");
        assert_eq!(sync_nginx_config(&custom_bucket, &generated, &paths).unwrap(), custom_bucket);
        let included_bucket = missing_bucket.replace("http {", "http {\n    include user-options.conf;");
        assert!(!sync_nginx_config(&included_bucket, &generated, &paths).unwrap().contains("server_names_hash_bucket_size"));

        // Windows 旧版本可能同时留下普通路径和 `//?/` 长路径形式；只能保留一条。
        #[cfg(windows)]
        {
            let managed_sites = portable_path_text(&paths.nginx_sites_dir());
            let output_body = output.trim_end().strip_suffix('}').unwrap().trim_end();
            let legacy_sites =
                format!("{output_body}\n    include \"//?/{managed_sites}/*.conf\";\n}}\n");
            let repaired_sites = sync_nginx_config(&legacy_sites, &generated, &paths).unwrap();
            let managed_include = format!("include \"{managed_sites}/*.conf\";");
            assert_eq!(repaired_sites.matches(&managed_include).count(), 1);
            assert!(!repaired_sites.contains("//?/"));
        }

        let legacy = "# site: demo (fixture) — NiceEnv 托管\r\nserver {\r\n    listen 8080; # keep comment\r\n    listen 8443 ssl;\r\n    listen 0.0.0.0:9000;\r\n    listen [::1]:9001;\r\n    location / { return 200 'listen 9999;'; }\r\n}\r\n";
        let localized = localize_legacy_site_listeners(legacy).unwrap();
        assert_eq!(localized, legacy.replace("listen 8080;", "listen 127.0.0.1:8080;").replace("listen 8443 ssl;", "listen 127.0.0.1:8443 ssl;"));
        assert_eq!(localize_legacy_site_listeners(&localized).unwrap(), localized);
        let custom = legacy.replace("# site: demo (fixture) — NiceEnv 托管", "# custom site");
        assert_eq!(localize_legacy_site_listeners(&custom).unwrap(), custom);
        assert_eq!(localize_legacy_site_listeners(&format!("\u{feff}{legacy}")).unwrap(), format!("\u{feff}{localized}"));
        std::fs::create_dir_all(paths.nginx_sites_dir()).unwrap();
        let site_path = paths.nginx_sites_dir().join("fixture.conf");
        std::fs::write(&site_path, legacy).unwrap();
        localize_legacy_nginx_sites(&paths, None).unwrap();
        assert_eq!(std::fs::read_to_string(&site_path).unwrap(), localized);
        localize_legacy_nginx_sites(&paths, None).unwrap();
        assert_eq!(std::fs::read_to_string(&site_path).unwrap(), localized);
    }

    #[test]
    fn nginx_tokens_keep_unknown_escapes_and_decode_supported_escapes() {
        let parsed =
            nginx_directives(r#"include "/tmp/a\dir"; value "a\tb\rc\nd\"e\\f\'g";"#).unwrap();
        assert_eq!(parsed[0].words[1], r"/tmp/a\dir");
        assert_eq!(parsed[1].words[1], "a\tb\rc\nd\"e\\f'g");
    }

    #[cfg(unix)]
    #[test]
    fn quoted_service_paths_preserve_unix_backslashes_and_nginx_include_identity() {
        let paths = Paths::new(Path::new(r#"/tmp/literal\new\tail with spaces"#).to_path_buf());
        let runtime = Path::new(r#"/tmp/runtime\version"#);
        let generated = render_nginx_conf(&paths, runtime, 8080, 8443, &[], None);
        let nodes = nginx_directives(&generated).unwrap();
        let pid = nodes.iter().find(|node| node.words[0] == "pid").unwrap();
        assert_eq!(
            pid.words[1],
            portable_path_text(&paths.etc().join("nginx/run/nginx.pid"))
        );
        // 这两个 Unix 目录不同，不能把用户自定义的正斜杠 include 当作托管项删除。
        let other = portable_path_text(&paths.nginx_sites_dir()).replace('\\', "/");
        let custom = format!("    include \"{other}/*.conf\"; # user include\n");
        let current = generated.replace("http {\n", &format!("http {{\n{custom}"));
        let updated = sync_nginx_config(&current, &generated, &paths).unwrap();
        assert!(updated.contains(&custom));
        let managed = format!(
            "include \"{}\";",
            quoted_nginx_include(&paths.nginx_sites_dir(), true)
        );
        assert_eq!(updated.matches(&managed).count(), 1);
        assert_eq!(
            sync_nginx_config(&updated, &generated, &paths).unwrap(),
            updated
        );

        let escaped = r"/tmp/literal\\new\\tail with spaces";
        assert!(render_php_ini(&paths, "8.4", runtime).contains(&format!(
            "error_log=\"{escaped}/logs/php/8.4/php_errors.log\""
        )));
        let mysql = render_mysql_ini(&paths, "8.4", runtime, 3306);
        assert!(mysql.contains(&format!("datadir=\"{escaped}/data/mysql/8.4\"")));
        assert_eq!(
            sync_mysql_config(&mysql, &paths, "8.4", runtime, 3306),
            mysql
        );
        let redis = render_redis_conf(&paths, "8", 6379);
        assert!(redis.contains(&format!("dir \"{escaped}/data/redis\"")));
        assert_eq!(sync_redis_config(&redis, &paths, 6379), redis);
        assert_eq!(
            quoted_config_path(Path::new("/tmp/quote\" and end\\")),
            "/tmp/quote\\\" and end\\\\"
        );
        assert_eq!(
            nginx_include_pattern(Path::new(r"/tmp/literal\[v1]"), true),
            r"/tmp/literal\\\[v1\]/*.conf"
        );
    }

    #[test]
    fn nginx_handles_compact_syntax_and_rejects_ambiguous_or_invalid_structure() {
        let (_temp, paths) = fixture();
        let generated = render_nginx_conf(&paths, Path::new("C:/nginx"), 8080, 8443, &[], None);
        let initial = "events {worker_connections 64;} http {gzip off;map $host $value {default 'a \\\' # {}';}}";
        let output = sync_nginx_config(initial, &generated, &paths).unwrap();
        assert!(output.contains("gzip off;map $host $value {default 'a \\\' # {}';}"));
        assert!(nginx_directives(&output).is_ok());
        assert_eq!(
            sync_nginx_config(&output, &generated, &paths).unwrap(),
            output
        );
        for invalid in [
            "http {",
            "http {} http {}",
            "events {}",
            "http { key \"unterminated; }",
            "http { set $x ${missing; } }",
        ] {
            assert!(
                sync_nginx_config(invalid, &generated, &paths).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn mysql_preserves_tuning_custom_sections_and_includes_and_only_updates_owned_options() {
        let (_temp, paths) = fixture();
        let user = "# user tuning\r\n[mysqld]\r\nmax_connections=321\r\ninnodb_buffer_pool_size=384M\r\nsql-mode=ANSI\r\nport=1234\r\nport=2345\r\n!include extra.ini\r\n[client]\r\nport=1234\r\npassword=fixture-only\r\n[custom]\r\nport=7890\r\n";
        let output =
            sync_mysql_config(user, &paths, "8.0.46", Path::new("C:/runtime/mysql"), 23307);
        assert!(output
            .contains("max_connections=321\r\ninnodb_buffer_pool_size=384M\r\nsql-mode=ANSI\r\n"));
        assert!(output.contains("!include extra.ini\r\n"));
        assert!(output.contains("[custom]\r\nport=7890\r\n"));
        assert_eq!(output.matches("port=23307").count(), 2);
        assert!(!output.contains("port=1234"));
        assert!(!output.contains("port=2345"));
        assert_eq!(
            sync_mysql_config(
                &output,
                &paths,
                "8.0.46",
                Path::new("C:/runtime/mysql"),
                23307
            ),
            output
        );
        assert!(
            render_mysql_ini(&paths, "9.4.0", Path::new("C:/mysql"), 3306).contains("mysqlx=OFF")
        );
        assert!(
            !render_mysql_ini(&paths, "5.7.44", Path::new("C:/mysql"), 3306).contains("mysqlx=")
        );
        assert!(!output.contains("bind-address="), "不能覆盖 include 中可能声明的监听地址");
        let legacy = "[mysqld]\r\nport=3306\r\nmax_connections=321\r\n[client]\r\nport=3306\r\n";
        let local = mysql_local_defaults(legacy, "8.0.46");
        assert!(local.starts_with(legacy));
        assert!(local.contains("\r\nbind-address=127.0.0.1\r\n"));
        assert!(local.contains("\r\nloose-mysqlx-bind-address=127.0.0.1\r\n"));
        assert_eq!(mysql_local_defaults(&local, "8.0.46"), local);
        assert!(!mysql_local_defaults(legacy, "5.7.44").contains("mysqlx"));
        let explicit = "[mysqld]\nbind_address=0.0.0.0\nmysqlx-bind-address=192.168.1.10\n";
        assert_eq!(mysql_local_defaults(explicit, "8.0.46"), explicit);
        for version in ["5.7.44", "8.0.46", "26.7.0"] {
            let generated = render_mysql_ini(&paths, version, Path::new("C:/mysql"), 3306);
            assert!(generated.contains("\nbind-address=127.0.0.1\n"));
            assert_eq!(mysql_local_defaults(&generated, version), generated);
        }
    }

    #[test]
    fn redis_preserves_repeated_save_rules_auth_and_memory_settings() {
        let (_temp, paths) = fixture();
        let user = "# custom\nport 1234\ndir \"C:/old\"\ndaemonize yes\nmaxmemory 384mb\nmaxmemory-policy noeviction\nsave 60 1\nsave 300 10\nappendonly yes\nrequirepass \"fixture # only\"\ninclude extra.conf\n";
        let output = sync_redis_config(user, &paths, 26380);
        assert!(output.contains("maxmemory 384mb\nmaxmemory-policy noeviction\nsave 60 1\nsave 300 10\nappendonly yes\nrequirepass \"fixture # only\"\ninclude extra.conf\n"));
        assert!(output.contains("port 26380\n"));
        assert!(output.contains("daemonize no\n"));
        assert!(!output.contains("C:/old"));
        assert_eq!(sync_redis_config(&output, &paths, 26380), output);
    }

    #[test]
    fn default_certificate_upgrade_preserves_custom_tls_paths_and_sections() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        let root = temp.path().join("apache-runtime");
        let generated = render_httpd_conf(&paths, &root, &[], 8180, 8444);
        let legacy = generated.replace(&format!("{}/fallback/localhost.crt", nginx_path(&paths.certs())), "${NSB_ETC}/ssl-dummy.crt")
            .replace(&format!("{}/fallback/localhost.key", nginx_path(&paths.certs())), "${NSB_ETC}/ssl-dummy.key");
        let upgraded = sync_httpd_config(&legacy, &paths, &root, 8180, 8444);
        assert!(upgraded.contains("/fallback/localhost.crt")); assert!(!upgraded.contains("ssl-dummy"));
        let custom = legacy.replace("${NSB_ETC}/ssl-dummy.crt", "D:/custom/ssl-dummy.crt")
            .replace("${NSB_ETC}/ssl-dummy.key", "D:/custom/ssl-dummy.key")
            + "\n<VirtualHost *:9443>\nSSLCertificateFile \"${NSB_ETC}/ssl-dummy.crt\"\n</VirtualHost>\n";
        let preserved = sync_httpd_config(&custom, &paths, &root, 8180, 8444);
        assert!(preserved.contains("D:/custom/ssl-dummy.crt")); assert!(preserved.contains("D:/custom/ssl-dummy.key"));
        assert!(preserved.contains("SSLCertificateFile \"${NSB_ETC}/ssl-dummy.crt\""));
        assert!(!preserved.contains("/fallback/localhost.crt"));
        let nginx = render_nginx_conf(&paths, &root, 8080, 8443, &[], None);
        let legacy_nginx = nginx.replace("/fallback/localhost.crt", "/ca.crt").replace("/fallback/localhost.key", "/ca.key");
        let upgraded_nginx = sync_nginx_config(&legacy_nginx, &nginx, &paths).unwrap();
        assert!(upgraded_nginx.contains("/fallback/localhost.crt")); assert!(!upgraded_nginx.contains("/ca.key"));
    }

    #[test]
    fn apache_preserves_modules_custom_directives_and_nested_sections() {
        let (_temp, paths) = fixture();
        let user = "# existing config\nServerRoot \\\n \"C:/old\"\nDefine NSB_ETC \"C:/old/etc\"\nListen 127.0.0.1:8180\nListen 127.0.0.1:8444 https\nLogLevel debug\nTimeout 123\nLoadModule custom_module modules/mod_custom.so\n<IfModule mod_headers.c>\n Header always set X-Custom \"keep me\"\n</IfModule>\n<VirtualHost *:9000>\n TypesConfig \"C:/custom/mime.types\"\n</VirtualHost>\n";
        let output = sync_httpd_config(user, &paths, Path::new("C:/new/apache"), 8181, 8445);
        assert!(output.contains("LogLevel debug\nTimeout 123\nLoadModule custom_module modules/mod_custom.so\n<IfModule mod_headers.c>\n Header always set X-Custom \"keep me\"\n</IfModule>\n<VirtualHost *:9000>\n TypesConfig \"C:/custom/mime.types\"\n</VirtualHost>\n"));
        assert!(!output.contains("C:/old"));
        assert!(output.contains("Listen 127.0.0.1:8181\nListen 127.0.0.1:8445 https\n"));
        assert_eq!(
            sync_httpd_config(&output, &paths, Path::new("C:/new/apache"), 8181, 8445),
            output
        );
        let missing =
            "ErrorLog \"${NSB_ETC}/logs/error.log\"\nLoadModule mime_module modules/mod_mime.so\n";
        let recovered = sync_httpd_config(missing, &paths, Path::new("C:/apache"), 8180, 8444);
        assert!(recovered.starts_with("ServerRoot \"C:/apache\"\nDefine NSB_ETC "));
        assert!(recovered.contains(&format!(
            "IncludeOptional \"{}/*.conf\"",
            nginx_path(&paths.apache_sites_dir())
        )));
        assert_eq!(
            sync_httpd_config(&recovered, &paths, Path::new("C:/apache"), 8180, 8444),
            recovered
        );
        for newline in ["\n", "\r\n"] {
            let user = recovered
                .replace(HTTPD_PROXY_HTTP_MODULE, "")
                .trim_end()
                .replace('\n', newline);
            let synced = sync_httpd_config(&user, &paths, Path::new("C:/apache"), 8180, 8444);
            assert!(synced.contains(&format!(
                "{newline}# NiceEnv HTTP reverse proxy support{newline}"
            )));
            assert_eq!(synced.matches("LoadModule proxy_http_module").count(), 1);
            assert_eq!(
                sync_httpd_config(
                    synced.trim_end(),
                    &paths,
                    Path::new("C:/apache"),
                    8180,
                    8444
                ),
                synced.trim_end()
            );
        }
        let special = paths.base.join("data [group] with spaces");
        #[cfg(unix)]
        let special = special.join(r#"literal\new\tail"quote"#);
        let paths = Paths::new(special);
        let root = paths.base.join("runtime");
        let generated = render_httpd_conf(&paths, &root, &[], 8180, 8444);
        assert_eq!(
            httpd_argument(
                generated
                    .lines()
                    .find_map(|line| line.strip_prefix("ServerRoot "))
                    .unwrap()
            ),
            crate::paths::portable_path_text(&root)
        );
        assert!(generated.contains("data [[]group[]] with spaces"));
        let legacy_include = format!(
            "IncludeOptional \"{}/*.conf\"",
            quoted_config_path(&paths.apache_sites_dir())
        );
        let mixed = format!(
            "{generated}\n{legacy_include}\nIncludeOptional \"${{NSB_ETC}}/sites/*.conf\"\n"
        );
        let repaired = sync_httpd_config(&mixed, &paths, &root, 8180, 8444);
        assert_eq!(repaired.matches("IncludeOptional").count(), 1);
        assert_eq!(
            sync_httpd_config(&repaired, &paths, &root, 8180, 8444),
            repaired
        );
        let site: Site = serde_json::from_value(serde_json::json!({
            "id":"path", "name":"path", "domains":["path.test"], "rootDir":paths.base.join("project {root}"),
            "runtime":{"kind":"php", "webServer":"apache"}, "https":false,
            "rewrite":"none", "createdAt":1, "updatedAt":1
        })).unwrap();
        let rendered = render_httpd_vhost(&site, 8180, 8444, &paths.certs(), Some(19000));
        let docroot = rendered
            .lines()
            .find_map(|line| line.trim().strip_prefix("DocumentRoot "))
            .unwrap();
        assert_eq!(
            httpd_argument(docroot),
            crate::paths::portable_path_text(Path::new(&site.root_dir))
        );
        assert!(rendered.contains("SetHandler \"proxy:fcgi://127.0.0.1:"));
        #[cfg(unix)]
        {
            let other = format!(
                "IncludeOptional \"{}/*.conf\"\n",
                crate::paths::portable_path_text(&paths.apache_sites_dir())
                    .replace('\\', "/")
                    .replace('"', "\\\"")
            );
            let current = format!("{repaired}{other}");
            assert_eq!(
                sync_httpd_config(&current, &paths, &root, 8180, 8444),
                current
            );
        }
    }

    #[test]
    fn generated_changes_keep_versioned_history_and_abort_if_backup_fails() {
        let (_temp, paths) = fixture();
        write_mysql_ini(&paths, "8.0.46", Path::new("C:/mysql"), 3306).unwrap();
        let path = paths.mysql_ini("8.0.46");
        let customized = std::fs::read_to_string(&path)
            .unwrap()
            .replace("max_connections=200", "max_connections=321");
        std::fs::write(&path, &customized).unwrap();
        write_mysql_ini(&paths, "8.0.46", Path::new("C:/mysql"), 23306).unwrap();
        let history = crate::cfgeditor::list_config_backups(&paths);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].target.as_deref(), Some("mysql-ini@8.0.46"));
        assert_eq!(
            std::fs::read_to_string(&history[0].path).unwrap(),
            customized
        );
        write_mysql_ini(&paths, "8.0.46", Path::new("C:/mysql"), 23306).unwrap();
        assert_eq!(crate::cfgeditor::list_config_backups(&paths).len(), 1);

        let (_other_temp, other) = fixture();
        write_redis_conf(&other, "5.0.14", 6379).unwrap();
        let original = std::fs::read_to_string(other.redis_conf("5.0.14")).unwrap();
        std::fs::write(other.backup().join("config"), "backup unavailable").unwrap();
        assert!(write_redis_conf(&other, "5.0.14", 6380).is_err());
        assert_eq!(
            std::fs::read_to_string(other.redis_conf("5.0.14")).unwrap(),
            original
        );
    }
}
