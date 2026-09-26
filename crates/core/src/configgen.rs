//! 服务配置生成：首次写入默认值，后续仅同步运行所需的托管项，保留用户配置。

use crate::error::{AppError, Result};
use crate::model::{RewritePreset, Site};
use crate::paths::{nginx_path, write_with_backup, Paths};
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

#[derive(Debug)]
struct NginxDirective {
    start: usize,
    end: usize,
    words: Vec<String>,
    children: Vec<NginxDirective>,
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
fn nginx_directives(content: &str) -> Result<Vec<NginxDirective>> {
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
                word.push(bytes[pos]);
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
    let sites = format!("{}/*.conf", nginx_path(&paths.nginx_sites_dir()));
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
            "include" if node.words.get(1) == Some(&sites) => Some(4),
            _ => None,
        }
    };
    let newline = if current.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut edits = Vec::new();
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
            let file = p.to_string_lossy().replace('\\', "/");
            format!(
                "        location = /_adminer {{ return 302 /_adminer/; }}\n        location /_adminer/ {{\n            fastcgi_pass {upstream};\n            fastcgi_index index.php;\n            fastcgi_param SCRIPT_FILENAME \"{file}\";\n            fastcgi_param DOCUMENT_ROOT \"{dir}\";\n            include {params};\n        }}\n",
                dir = nginx_path(p.parent().unwrap_or(std::path::Path::new("."))),
                params = nginx_path(&paths.etc().join("nginx").join("fastcgi_params")),
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
        listen {http_port};
        listen {https_port} ssl;
        server_name _;
        ssl_certificate     "{certs}/ca.crt";
        ssl_certificate_key "{certs}/ca.key";
        location / {{
            default_type text/html;
            return 200 "<h1>NiceEnv is running</h1><p>创建站点后用你的本地域名访问，例如 http://demo.test:{http_port}</p>";
        }}
{adminer_loc}
    }}

{upstreams}
    include "{sites}/*.conf";
}}
"#,
        pid = nginx_path(&paths.etc().join("nginx").join("run").join("nginx.pid")),
        error_log = nginx_path(&paths.logs().join("nginx").join("error.log")),
        access_log = nginx_path(&paths.logs().join("nginx").join("access.log")),
        mime = nginx_path(&nginx_root.join("conf").join("mime.types")),
        temp = nginx_path(&paths.etc().join("nginx").join("temp")),
        certs = nginx_path(&paths.certs()),
        http_port = http_port,
        https_port = https_port,
        adminer_loc = adminer_loc,
        upstreams = upstreams,
        sites = nginx_path(&paths.nginx_sites_dir()),
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

/// 已导入的证书从同级 imported 目录读取；本地证书继续使用原来的主域名路径。
fn site_certificate_files(site: &Site, cert_dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let (directory, stem) = match &site.runtime.imported_cert_id {
        Some(id) => (
            cert_dir.parent().unwrap_or(cert_dir).join("imported"),
            if crate::certs::valid_imported_id(id) { id.clone() } else { "invalid-certificate".into() },
        ),
        None => (cert_dir.to_path_buf(), site.domains.first().map(String::as_str).unwrap_or("localhost")
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
    let server_names = site.domains.join(" ");
    let listen = if site.https {
        format!("listen {http_port};\n    listen {https_port} ssl")
    } else {
        format!("listen {http_port}")
    };
    let ssl_lines = if site.https {
        let (certificate, key) = site_certificate_files(site, cert_dir);
        format!(
            "    ssl_certificate     \"{}\";\n    ssl_certificate_key \"{}\";",
            nginx_path(&certificate),
            nginx_path(&key),
        )
    } else {
        String::new()
    };

    let body = match &site.runtime.kind {
        crate::model::SiteKind::Php => {
            let upstream =
                nginx_upstream_name(site.runtime.php_version.as_deref().unwrap_or("8.3"));
            let mut s = rewrite_snippet(&site.rewrite).to_string();
            if matches!(site.rewrite, RewritePreset::None) {
                s.push_str("    location / {\n        try_files $uri $uri/ /index.php?$query_string;\n    }\n");
            }
            s.push_str(&format!(
                "    location ~ \\.php$ {{\n        try_files $uri =404;\n        fastcgi_pass {upstream};\n        include \"{}\";\n    }}\n",
                nginx_path(fastcgi_params_path)
            ));
            s
        }
        crate::model::SiteKind::Static => {
            let mut s = rewrite_snippet(&site.rewrite).to_string();
            if matches!(site.rewrite, RewritePreset::None) {
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
                "    location / {{\n        proxy_pass {target};\n        proxy_ssl_server_name on;\n        proxy_http_version 1.1;\n        proxy_set_header Host $proxy_host;\n        proxy_set_header X-Real-IP $remote_addr;\n        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n        proxy_set_header X-Forwarded-Proto $scheme;\n        proxy_set_header Upgrade $http_upgrade;\n        proxy_set_header Connection \"upgrade\";\n        proxy_read_timeout 300s;\n    }}\n"
            )
        }
    };

    format!(
        r#"# site: {name} ({id}) — NiceEnv 托管
server {{
    {listen};
    server_name {server_names};
    {ssl_lines}
    root "{root}";
    index index.php index.html index.htm;
    charset utf-8;

    # 站点级日志（日志页按站点查看就靠它）
    access_log "{access_log}";
    error_log "{error_log}" warn;

    location ~ /\.(?!well-known(?:/|$)) {{ deny all; }}

{body}}}
"#,
        name = site.name,
        id = site.id,
        access_log = nginx_path(&log_dir.join(format!("{}.access.log", site.id))),
        error_log = nginx_path(&log_dir.join(format!("{}.error.log", site.id))),
        listen = listen,
        server_names = server_names,
        ssl_lines = ssl_lines,
        root = nginx_path(std::path::Path::new(&site.root_dir)),
        body = body,
    )
}

/* ================= php.ini ================= */

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
extension=gd
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
        error_log = nginx_path(
            &paths
                .logs()
                .join("php")
                .join(version)
                .join("php_errors.log")
        ),
        ext = nginx_path(&runtime_dir.join("ext")),
        sess = nginx_path(&paths.data().join("php").join(version).join("sess")),
    )
}

/* ================= mysql my.ini ================= */

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

fn sync_mysql_config(
    current: &str,
    paths: &Paths,
    version: &str,
    basedir: &Path,
    port: u16,
) -> String {
    let options = [
        ("mysqld", "basedir", format!("\"{}\"", nginx_path(basedir))),
        (
            "mysqld",
            "datadir",
            format!("\"{}\"", nginx_path(&paths.mysql_data_dir(version))),
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
    sync_managed_lines(current, &groups, &[], |line| {
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
    })
}

fn sync_redis_config(current: &str, paths: &Paths, port: u16) -> String {
    let options = [
        ("port", port.to_string()),
        (
            "dir",
            format!("\"{}\"", nginx_path(&paths.redis_data_dir())),
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
        "mysqlx=OFF\n"
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
        basedir = nginx_path(basedir),
        datadir = nginx_path(&paths.mysql_data_dir(version)),
        port = port,
        mysqlx = mysqlx,
        log_error = nginx_path(&paths.logs().join("mysql").join("error.log")),
    )
}

/* ================= redis.conf ================= */

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
        dir = nginx_path(&paths.redis_data_dir()),
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

/// 激活订阅：把订阅 YAML 的端口/控制端口等强制替换为我们的托管值
pub fn adapt_mihomo_profile(raw: &str) -> String {
    let overrides: &[(&str, String)] = &[
        ("mixed-port:", MIHOMO_MIXED_PORT.to_string()),
        ("port:", "0".to_string()),
        ("socks-port:", "0".to_string()),
        (
            "external-controller:",
            format!("127.0.0.1:{MIHOMO_CONTROLLER_PORT}"),
        ),
        ("secret:", r#""""#.to_string()),
        ("allow-lan:", "false".to_string()),
        ("external-ui:", "".to_string()),
    ];
    let mut seen: Vec<String> = Vec::new();
    let mut out = String::new();
    for line in raw.lines() {
        let trimmed = line.trim_start();
        let mut replaced = line.to_string();
        if !trimmed.starts_with('#') {
            for (key, val) in overrides {
                if trimmed.starts_with(key) {
                    seen.push(key.to_string());
                    if val.is_empty() {
                        replaced = format!("# {line}");
                    } else {
                        let indent = &line[..line.len() - trimmed.len()];
                        replaced = format!("{indent}{key} {val}");
                    }
                    break;
                }
            }
        }
        out.push_str(&replaced);
        out.push('\n');
    }
    for (key, val) in overrides {
        if !seen.iter().any(|s| s == key) && !val.is_empty() {
            out.push_str(&format!("{key} {val}\n"));
        }
    }
    out
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
    publish_config(paths, "nginx-main", &path, &conf, previous.as_deref())?;
    let fp = paths.etc().join("nginx").join("fastcgi_params");
    if !fp.exists() {
        std::fs::write(&fp, FASTCGI_PARAMS)?;
    }
    // 自签 CA 兜底（nginx 默认 server 的 ssl 证书）
    crate::tls::ensure_ca(paths)?;
    Ok(())
}

pub fn write_php_ini(paths: &Paths, version: &str) -> Result<()> {
    let runtime_dir = paths.runtime_dir("php", version);
    std::fs::create_dir_all(paths.logs().join("php").join(version))?;
    std::fs::create_dir_all(paths.data().join("php").join(version).join("sess"))?;
    // php.ini 是扩展开关与配置编辑器共同保存的用户配置，启动时不能重置。
    if paths.php_ini(version).is_file() {
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

pub fn write_mihomo_config(paths: &Paths, content: &str) -> Result<()> {
    std::fs::create_dir_all(paths.mihomo_dir().join("profiles"))?;
    write_with_backup(&paths.mihomo_config(), content, &paths.backup())?;
    Ok(())
}

/// 校验 nginx 配置语法：nginx -t
pub fn validate_nginx(nginx_exe: &std::path::Path, conf: &std::path::Path) -> Result<()> {
    let root = nginx_exe.parent().ok_or_else(nginx_structure_error)?;
    let mut command = platform::command(nginx_exe);
    command
        .current_dir(root)
        .arg("-p")
        .arg(root)
        .arg("-t")
        .arg("-c")
        .arg(conf);
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

fn sync_httpd_config(current: &str, paths: &Paths, root: &Path, http: u16, https: u16) -> String {
    let sites = format!("\"{}/*.conf\"", nginx_path(&paths.apache_sites_dir()));
    let replacements = [
        format!("ServerRoot \"{}\"", nginx_path(root)),
        format!(
            "Define NSB_ETC \"{}\"",
            nginx_path(&paths.etc().join("apache"))
        ),
        format!("Listen 127.0.0.1:{http}\nListen 127.0.0.1:{https} https"),
        format!("TypesConfig \"{}/conf/mime.types\"", nginx_path(root)),
        format!("IncludeOptional {sites}"),
    ];
    let groups: Vec<_> = replacements
        .into_iter()
        .map(|line| (line.clone(), line))
        .collect();
    let mut depth = 0usize;
    sync_managed_lines(current, &groups, &[0, 1], |line| {
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
            "includeoptional" | "include"
                if line
                    .split_once(char::is_whitespace)
                    .is_some_and(|(_, path)| {
                        path.trim() == sites || path.trim() == sites.trim_matches('"')
                    }) =>
            {
                Some(4)
            }
            _ => None,
        }
    })
}

/// 渲染 httpd.conf（ApacheLounge Apache24；pools 为运行中的 php 池）
pub fn render_httpd_conf(
    paths: &Paths,
    apache_root: &std::path::Path,
    _pools: &[(String, u16)],
    http_port: u16,
    https_port: u16,
) -> String {
    let root = nginx_path(apache_root);
    let etc = nginx_path(&paths.etc().join("apache"));
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
LoadModule ssl_module modules/mod_ssl.so
LoadModule socache_shmcb_module modules/mod_socache_shmcb.so

ServerName 127.0.0.1:{http_port}
PidFile "${{NSB_ETC}}/run/httpd.pid"
ErrorLog "${{NSB_ETC}}/logs/error.log"
LogLevel warn
CustomLog "${{NSB_ETC}}/logs/access.log" common

SSLCertificateFile "${{NSB_ETC}}/ssl-dummy.crt"
SSLCertificateKeyFile "${{NSB_ETC}}/ssl-dummy.key"
SSLSessionCache "shmcb:${{NSB_ETC}}/logs/ssl_scache(512000)"

DocumentRoot "${{NSB_ETC}}/htdocs"
<Directory "/">
    AllowOverride All
    Require all granted
</Directory>

TypesConfig "{root}/conf/mime.types"
DirectoryIndex index.php index.html index.htm

IncludeOptional "{etc}/sites/*.conf"
"#,
        root = root,
        etc = etc,
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
    let server_names = site.domains.join(" ");
    let primary = site
        .domains
        .first()
        .cloned()
        .unwrap_or_else(|| "localhost".into());
    let root = nginx_path(std::path::Path::new(&site.root_dir));

    let (listen, ssl_lines) = if site.https {
        let (certificate, key) = site_certificate_files(site, cert_dir);
        (
            format!("<VirtualHost *:{https_port}>"),
            format!(
                "    SSLEngine on\n    SSLCertificateFile \"{}\"\n    SSLCertificateKeyFile \"{}\"",
                nginx_path(&certificate),
                nginx_path(&key),
            ),
        )
    } else {
        (format!("<VirtualHost *:{http_port}>"), String::new())
    };

    let body = match &site.runtime.kind {
        crate::model::SiteKind::Php => {
            let mut s = String::new();
            if let Some(base) = php_pool {
                // mod_proxy_balancer 依赖 slotmem-shm，Windows 重启存在已知缺陷；
                // 改为每站点直连池内一个 worker（按站点 id 轮转，4 worker 跨站点分摊）。
                // 用 RewriteRule [P] 而非 ProxyPassMatch：后者不处理 DirectoryIndex 内部重定向
                let worker = base
                    + (site.id.chars().map(|c| c as usize).sum::<usize>()
                        % PHP_POOL_WORKERS as usize) as u16;
                s.push_str("    RewriteEngine On\n");
                s.push_str(&format!(
                    "    RewriteRule ^/?(.*\\.ph(p[3457]?|t|tml)(/.*)?)$ \"fcgi://127.0.0.1:{worker}/{root}/$1\" [P,L]\n"
                ));
                // Windows 盘符路径带不了 fcgi URL 的前导斜杠 → 显式修正 SCRIPT_FILENAME
                s.push_str(&format!(
                    "    ProxyFCGISetEnvIf \"true\" SCRIPT_FILENAME \"{root}%{{reqenv:SCRIPT_NAME}}\"\n"
                ));
            } else {
                s.push_str(
                    "    <FilesMatch \"\\.php$\">\n        Require all denied\n    </FilesMatch>\n",
                );
            }
            s.push_str("    <Directory \"{root}\">\n        AllowOverride All\n        Require all granted\n    </Directory>\n");
            s.push_str("    RewriteCond %{REQUEST_FILENAME} !-d\n    RewriteCond %{REQUEST_FILENAME} !-f\n    RewriteRule ^ index.php [QSA,L]\n");
            s
        }
        crate::model::SiteKind::Static => {
            let rewrite = match site.rewrite {
                RewritePreset::NextExport => "        RewriteEngine On\n        RewriteCond %{REQUEST_FILENAME} !-f\n        RewriteCond %{REQUEST_FILENAME} !-d\n        RewriteCond %{REQUEST_FILENAME}.html -f\n        RewriteRule ^(.+?)/?$ $1.html [END]\n",
                RewritePreset::SpaFallback => "        RewriteEngine On\n        RewriteCond %{REQUEST_FILENAME} !-f\n        RewriteCond %{REQUEST_FILENAME} !-d\n        RewriteRule ^ index.html [END]\n",
                _ => "",
            };
            let error_page = if matches!(site.rewrite, RewritePreset::NextExport) {
                "    ErrorDocument 404 /404.html\n"
            } else {
                ""
            };
            format!("    <Directory \"{root}\">\n        AllowOverride All\n        Require all granted\n{rewrite}    </Directory>\n{error_page}")
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
                "    SSLProxyEngine On\n    ProxyPreserveHost Off\n    ProxyPass / \"{target}\"\n    ProxyPassReverse / \"{target}\"\n"
            )
        }
    };

    let vhost = format!(
        r#"# site: {name} ({id}) — NiceEnv 托管
{listen}
    ServerName {primary}
    ServerAlias {server_names}
    DocumentRoot "{root}"
    {ssl_lines}

    <FilesMatch "^\.">
        Require all denied
    </FilesMatch>
    <DirectoryMatch "/\.(?!well-known(?:/|$))">
        Require all denied
    </DirectoryMatch>

{body}
</VirtualHost>
"#,
        name = site.name,
        id = site.id,
        listen = listen,
        primary = primary,
        server_names = server_names,
        root = root,
        ssl_lines = ssl_lines,
        body = body,
    )
    .replace("{root}", &root);
    if site.https {
        let http = vhost
            .replace(&listen, &format!("<VirtualHost *:{http_port}>"))
            .replace(&ssl_lines, "");
        format!("{http}\n{vhost}")
    } else {
        vhost
    }
}

pub fn write_httpd_conf(
    paths: &Paths,
    apache_root: &std::path::Path,
    pools: &[(String, u16)],
    http_port: u16,
    https_port: u16,
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
    publish_config(paths, "apache-conf", &path, &conf, previous.as_deref())?;
    // 兜底默认证书（无 https 站点时 Listen https 仍需要证书文件存在）
    let crt = paths.etc().join("apache").join("ssl-dummy.crt");
    let key = paths.etc().join("apache").join("ssl-dummy.key");
    if !crt.exists() || !key.exists() {
        crate::tls::ensure_ca(paths)?;
        std::fs::copy(paths.certs().join("ca.crt"), &crt)?;
        std::fs::copy(paths.certs().join("ca.key"), &key)?;
    }
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
        .arg(root)
        .arg("-t")
        .arg("-f")
        .arg(conf);
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
        assert!(output.contains("listen 8081;"));
        assert!(output.contains("listen 8444 ssl;"));
        assert!(output.contains("nsb_php_8_4_0"));
        assert!(!output.contains("nsb_php_8_2_0"));
        assert!(!output.contains("C:/old/conf-root"));
        assert_eq!(
            sync_nginx_config(&output, &generated, &paths).unwrap(),
            output
        );
        let crlf = output.replace('\n', "\r\n");
        assert_eq!(sync_nginx_config(&crlf, &generated, &paths).unwrap(), crlf);
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
