//! 各服务默认配置生成。所有生成均带备份；模板集中在这里，便于「修复向导」重写。

use crate::error::{AppError, Result};
use crate::model::{RewritePreset, Site};
use crate::paths::{nginx_path, write_with_backup, Paths};

/* ================= nginx ================= */

/// php-cgi 池端口：base..base+3（4 worker）
pub const PHP_POOL_WORKERS: u16 = 4;

pub fn nginx_upstream_name(php_version: &str) -> String {
    format!("nsb_php_{}", php_version.replace('.', "_"))
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
        r#"# NiceEnv managed nginx.conf — 修改会被「修复向导」备份重写
worker_processes  2;
pid        {pid};
error_log  {error_log} warn;

events {{
    worker_connections  1024;
}}

http {{
    include       {mime};
    default_type  application/octet-stream;

    sendfile        on;
    tcp_nopush      on;
    keepalive_timeout  65;
    server_tokens   off;
    client_max_body_size 128m;

    access_log  {access_log};

    client_body_temp_path   {temp}/client_body;
    proxy_temp_path         {temp}/proxy;
    fastcgi_temp_path       {temp}/fastcgi;
    uwsgi_temp_path         {temp}/uwsgi;
    scgi_temp_path          {temp}/scgi;

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
        ssl_certificate     {certs}/ca.crt;
        ssl_certificate_key {certs}/ca.key;
        location / {{
            default_type text/html;
            return 200 "<h1>NiceEnv is running</h1><p>创建站点后用你的本地域名访问，例如 http://demo.test:{http_port}</p>";
        }}
{adminer_loc}
    }}

{upstreams}
    include {sites}/*.conf;
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
            "    location / {\n        try_files $uri $uri/ /index.html;\n    }\n"
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
    let primary = site
        .domains
        .first()
        .cloned()
        .unwrap_or_else(|| "localhost".into());
    let listen = if site.https {
        format!("listen {http_port};\n    listen {https_port} ssl")
    } else {
        format!("listen {http_port}")
    };
    let ssl_lines = if site.https {
        format!(
            "    ssl_certificate     {};\n    ssl_certificate_key {};",
            nginx_path(&cert_dir.join(format!("{primary}.crt"))),
            nginx_path(&cert_dir.join(format!("{primary}.key"))),
        )
    } else {
        String::new()
    };

    let body = match &site.runtime.kind {
        crate::model::SiteKind::Php => {
            let upstream = nginx_upstream_name(site.runtime.php_version.as_deref().unwrap_or("8.3"));
            let mut s = rewrite_snippet(&site.rewrite).to_string();
            if matches!(site.rewrite, RewritePreset::None) {
                s.push_str("    location / {\n        try_files $uri $uri/ /index.php?$query_string;\n    }\n");
            }
            s.push_str(&format!(
                "    location ~ \\.php$ {{\n        try_files $uri =404;\n        fastcgi_pass {upstream};\n        include {};\n    }}\n",
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
            format!(
                "    location / {{\n        proxy_pass http://{target};\n        proxy_http_version 1.1;\n        proxy_set_header Host $host;\n        proxy_set_header X-Real-IP $remote_addr;\n        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n        proxy_set_header X-Forwarded-Proto $scheme;\n        proxy_set_header Upgrade $http_upgrade;\n        proxy_set_header Connection \"upgrade\";\n        proxy_read_timeout 300s;\n    }}\n"
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
    access_log {access_log};
    error_log {error_log} warn;

    location ~ /\.ht {{ deny all; }}

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
        error_log = nginx_path(&paths.logs().join("php").join(version).join("php_errors.log")),
        ext = nginx_path(&runtime_dir.join("ext")),
        sess = nginx_path(&paths.data().join("php").join(version).join("sess")),
    )
}

/* ================= mysql my.ini ================= */

pub fn render_mysql_ini(
    paths: &Paths,
    version: &str,
    basedir: &std::path::Path,
    port: u16,
) -> String {
    // mysqlx（X Protocol）仅 8.x 有；5.7 传入会拒启
    let mysqlx = if version.starts_with('8') { "mysqlx=OFF\n" } else { "" };
    format!(
        r#"# NiceEnv managed my.ini ({version})
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
        r#"# NiceEnv managed redis.conf ({version})
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
        ("external-controller:", format!("127.0.0.1:{MIHOMO_CONTROLLER_PORT}")),
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

pub fn ensure_all_configs(paths: &Paths, _pools: &[(String, u16)], _http_port: u16, _https_port: u16, _mysql_port: u16, _redis_port: u16) -> Result<()> {
    // nginx 主配置需要 nginx 运行时目录（mime.types）；由调用方传入已安装 nginx
    // 这里只生成 fastcgi_params / rewrites / 各服务配置
    let fp = paths.etc().join("nginx").join("fastcgi_params");
    write_with_backup(&fp, FASTCGI_PARAMS, &paths.backup())?;
    Ok(())
}

pub fn write_nginx_conf(paths: &Paths, nginx_root: &std::path::Path, pools: &[(String, u16)], http_port: u16, https_port: u16) -> Result<()> {
    std::fs::create_dir_all(paths.logs().join("nginx"))?;
    std::fs::create_dir_all(paths.etc().join("nginx").join("temp"))?;
    std::fs::create_dir_all(paths.etc().join("nginx").join("run"))?;
    let adminer = adminer_path(paths);
    let conf = render_nginx_conf(paths, nginx_root, http_port, https_port, pools, adminer.as_deref());
    write_with_backup(&paths.nginx_conf(), &conf, &paths.backup())?;
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
    let ini = render_php_ini(paths, version, &runtime_dir);
    write_with_backup(&paths.php_ini(version), &ini, &paths.backup())?;
    Ok(())
}

pub fn write_mysql_ini(paths: &Paths, version: &str, basedir: &std::path::Path, port: u16) -> Result<()> {
    std::fs::create_dir_all(paths.logs().join("mysql"))?;
    let ini = render_mysql_ini(paths, version, basedir, port);
    write_with_backup(&paths.mysql_ini(version), &ini, &paths.backup())?;
    Ok(())
}

pub fn write_redis_conf(paths: &Paths, version: &str, port: u16) -> Result<()> {
    std::fs::create_dir_all(paths.redis_data_dir())?;
    let conf = render_redis_conf(paths, version, port);
    write_with_backup(&paths.redis_conf(version), &conf, &paths.backup())?;
    Ok(())
}

pub fn write_mihomo_config(paths: &Paths, content: &str) -> Result<()> {
    std::fs::create_dir_all(paths.mihomo_dir().join("profiles"))?;
    write_with_backup(&paths.mihomo_config(), content, &paths.backup())?;
    Ok(())
}

/// 校验 nginx 配置语法：nginx -t
pub fn validate_nginx(nginx_exe: &std::path::Path, conf: &std::path::Path) -> Result<()> {
    let out = platform::command(nginx_exe)
        .arg("-t")
        .arg("-c")
        .arg(conf)
        .output()
        .map_err(|e| AppError::io("运行 nginx -t", e))?;
    if !out.status.success() {
        return Err(AppError::new(
            "NGINX_CONF_INVALID",
            "nginx 配置语法校验失败",
        )
        .with_hint("请检查高级设置里改过的内容；系统会在修改前自动备份")
        .with_detail(String::from_utf8_lossy(&out.stderr).to_string()));
    }
    Ok(())
}

/* ================= Apache httpd ================= */

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
        r#"# NiceEnv 托管 httpd.conf — 修改会被覆盖（备份在 backup/）
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
    let primary = site.domains.first().cloned().unwrap_or_else(|| "localhost".into());
    let root = nginx_path(std::path::Path::new(&site.root_dir));

    let (listen, ssl_lines) = if site.https {
        (
            format!("<VirtualHost *:{https_port}>"),
            format!(
                "    SSLEngine on\n    SSLCertificateFile \"{}\"\n    SSLCertificateKeyFile \"{}\"",
                nginx_path(&cert_dir.join(format!("{primary}.crt"))),
                nginx_path(&cert_dir.join(format!("{primary}.key"))),
            ),
        )
    } else {
        (
            format!("<VirtualHost *:{http_port}>"),
            String::new(),
        )
    };

    let body = match &site.runtime.kind {
        crate::model::SiteKind::Php => {
            let mut s = String::new();
            if let Some(base) = php_pool {
                // mod_proxy_balancer 依赖 slotmem-shm，Windows 重启存在已知缺陷；
                // 改为每站点直连池内一个 worker（按站点 id 轮转，4 worker 跨站点分摊）。
                // 用 RewriteRule [P] 而非 ProxyPassMatch：后者不处理 DirectoryIndex 内部重定向
                let worker = base + (site.id.chars().map(|c| c as usize).sum::<usize>() % PHP_POOL_WORKERS as usize) as u16;
                s.push_str("    RewriteEngine On\n");
                s.push_str(&format!(
                    "    RewriteRule ^/?(.*\\.ph(p[3457]?|t|tml)(/.*)?)$ \"fcgi://127.0.0.1:{worker}/{root}/$1\" [P,L]\n"
                ));
                // Windows 盘符路径带不了 fcgi URL 的前导斜杠 → 显式修正 SCRIPT_FILENAME
                s.push_str(&format!(
                    "    ProxyFCGISetEnvIf \"true\" SCRIPT_FILENAME \"{root}%{{reqenv:SCRIPT_NAME}}\"\n"
                ));
            } else {
                s.push_str("    # PHP 池未分配（未安装/未启动该版本），php 请求将 503\n");
            }
            s.push_str("    <Directory \"{root}\">\n        AllowOverride All\n        Require all granted\n    </Directory>\n");
            s.push_str("    RewriteCond %{REQUEST_FILENAME} !-d\n    RewriteCond %{REQUEST_FILENAME} !-f\n    RewriteRule ^ index.php [QSA,L]\n");
            s
        }
        crate::model::SiteKind::Static => {
            format!(
                "    <Directory \"{root}\">\n        AllowOverride All\n        Require all granted\n    </Directory>\n"
            )
        }
        _ => {
            let target = site
                .runtime
                .proxy_target
                .clone()
                .unwrap_or_else(|| "127.0.0.1:8080".into());
            format!(
                "    ProxyPreserveHost On\n    ProxyPass / \"http://{target}/\"\n    ProxyPassReverse / \"http://{target}/\"\n"
            )
        }
    };

    format!(
        r#"# site: {name} ({id}) — NiceEnv 托管
{listen}
    ServerName {primary}
    ServerAlias {server_names}
    DocumentRoot "{root}"
    {ssl_lines}

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
    .replace("{root}", &root)
}

pub fn write_httpd_conf(paths: &Paths, apache_root: &std::path::Path, pools: &[(String, u16)], http_port: u16, https_port: u16) -> Result<()> {
    std::fs::create_dir_all(paths.apache_sites_dir())?;
    std::fs::create_dir_all(paths.apache_run_dir())?;
    std::fs::create_dir_all(paths.etc().join("apache").join("logs"))?;
    std::fs::create_dir_all(paths.etc().join("apache").join("htdocs"))?;
    let conf = render_httpd_conf(paths, apache_root, pools, http_port, https_port);
    write_with_backup(&paths.apache_conf(), &conf, &paths.backup())?;
    // 兜底默认证书（无 https 站点时 Listen https 仍需要证书文件存在）
    let crt = paths.etc().join("apache").join("ssl-dummy.crt");
    let key = paths.etc().join("apache").join("ssl-dummy.key");
    if !crt.exists() || !key.exists() {
        crate::tls::ensure_ca(paths)?;
        let _ = std::fs::copy(paths.certs().join("ca.crt"), &crt);
        let _ = std::fs::copy(paths.certs().join("ca.key"), &key);
    }
    Ok(())
}

/// 校验 httpd 配置语法：httpd -t
pub fn validate_httpd(httpd_exe: &std::path::Path, conf: &std::path::Path) -> Result<()> {
    let out = platform::command(httpd_exe)
        .arg("-t")
        .arg("-f")
        .arg(conf)
        .output()
        .map_err(|e| AppError::io("运行 httpd -t", e))?;
    if !out.status.success() {
        return Err(AppError::new(
            "HTTPD_CONF_INVALID",
            "Apache 配置校验未通过",
        )
        .with_detail(String::from_utf8_lossy(&out.stderr).to_string()));
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
        let p = paths.runtimes().join("adminer").join(&v).join("adminer.php");
        if p.exists() {
            return Some(p);
        }
    }
    None
}
