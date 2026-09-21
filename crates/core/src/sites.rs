//! 站点生命周期：模板脚手架 → vhost 生成 → hosts → 证书 → 数据库 → nginx 重载。

use crate::configgen;
use crate::error::{AppError, Result};
use crate::model::{CreateSiteInput, Site, SiteKind, ServiceState};
use crate::paths::{write_with_backup, Paths};
use crate::services::*;
use crate::store::Store;
use std::sync::Arc;

pub fn list(store: &Store) -> Result<Vec<Site>> {
    store.list_sites()
}

pub fn get(store: &Store, id: &str) -> Result<Site> {
    store
        .list_sites()?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| AppError::new("SITE_NOT_FOUND", "站点不存在"))
}

pub fn create(
    input: &CreateSiteInput,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
) -> Result<Site> {
    // ---- 校验 ----
    if input.name.trim().is_empty() {
        return Err(AppError::new("BAD_INPUT", "站点名称不能为空"));
    }
    let domain_re = regex::Regex::new(r"^[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?(\.[a-zA-Z0-9]([a-zA-Z0-9-]*[a-zA-Z0-9])?)+$")
        .unwrap();
    for d in &input.domains {
        if !domain_re.is_match(d) {
            return Err(AppError::new(
                "BAD_DOMAIN",
                format!("域名 “{d}” 格式不正确（示例：myproject.test）"),
            ));
        }
    }
    if let Some(existing) = store.list_sites()?.iter().find(|s| {
        s.domains.iter().any(|d| input.domains.contains(d))
    }) {
        return Err(AppError::new(
            "DOMAIN_CONFLICT",
            format!("域名已被站点「{}」使用", existing.name),
        )
        .with_hint("换一个域名，或到该站点的设置里修改"));
    }

    // ---- 脚手架 ----
    let root = std::path::PathBuf::from(&input.root_dir);
    std::fs::create_dir_all(&root).map_err(|e| {
        AppError::io("创建站点根目录", e).with_hint("检查路径是否正确、磁盘是否可写")
    })?;
    scaffold_template(&input.template, &root, input)?;

    // ---- 数据库 ----
    if let Some(db) = &input.create_db {
        let ports = PortsProfile::from_settings(store);
        let version = store
            .find_installed("mysql", None)
            .map(|p| p.version)
            .ok_or_else(|| AppError::not_installed("MySQL").with_hint("勾选创建数据库前需要先安装 MySQL"))?;
        // 确保运行
        if manager.snapshot(&format!("mysql@{version}")).map(|s| s.state != ServiceState::Running).unwrap_or(true) {
            crate::ops::start_service(store, paths, manager, &format!("mysql@{version}"))?;
        }
        let pass = store.get_setting("mysqlRootPassword").unwrap_or_else(|| "root".into());
        let client = crate::dbadmin::MySqlClient::from_state(paths, &version, ports.mysql, pass);
        client.create_database(&db.database)?;
        client.create_user_grant(&db.username, &db.password, &db.database)?;
        if input.write_env_example {
            let env = crate::dbadmin::render_env_example(&db.database, &db.username, &db.password, ports.mysql);
            std::fs::write(root.join(".env.example"), env).ok();
        }
    }

    // ---- 站点记录 ----
    let now = now_ms();
    let site = Site {
        id: format!("site-{}", now),
        name: input.name.trim().to_string(),
        domains: input.domains.clone(),
        root_dir: input.root_dir.clone(),
        runtime: input.runtime.clone(),
        https: input.https,
        rewrite: input.rewrite.clone(),
        db: input.create_db.as_ref().map(|d| crate::model::SiteDbBinding {
            enabled: true,
            database: d.database.clone(),
            username: d.username.clone(),
            password: d.password.clone(),
        }),
        status: "running".into(),
        created_at: now,
        updated_at: now,
    };
    store.save_site(&site)?;

    // ---- 证书 ----
    if site.https {
        crate::tls::issue_site_cert(paths, store, &site.domains)?;
    }

    // ---- vhost + 重载 ----
    write_site_conf(paths, store, &site)?;
    crate::ops::rebuild_and_reload(store, paths, manager)?;

    // ---- hosts（失败不阻断，报人话） ----
    if let Err(e) = crate::hosts::apply(store, paths, None) {
        crate::emit_hosts_denied(store, paths);
        return Err(e);
    }

    Ok(site)
}

pub fn update(
    site_patch: &Site,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
) -> Result<Site> {
    let mut current = get(store, &site_patch.id)?;
    current.name = site_patch.name.trim().to_string();
    current.domains = site_patch.domains.clone();
    current.root_dir = site_patch.root_dir.clone();
    current.runtime = site_patch.runtime.clone();
    let https_changed = current.https != site_patch.https;
    current.https = site_patch.https;
    current.rewrite = site_patch.rewrite.clone();
    current.updated_at = now_ms();
    store.save_site(&current)?;

    if https_changed && current.https {
        crate::tls::issue_site_cert(paths, store, &current.domains)?;
    }
    write_site_conf(paths, store, &current)?;
    crate::ops::rebuild_and_reload(store, paths, manager)?;
    let _ = crate::hosts::apply(store, paths, None);
    Ok(current)
}

pub fn delete(
    id: &str,
    remove_hosts: bool,
    remove_certs: bool,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
) -> Result<()> {
    let site = get(store, id)?;
    let _ = std::fs::remove_file(paths.nginx_sites_dir().join(format!("{}.conf", site.id)));
    let _ = std::fs::remove_file(paths.apache_sites_dir().join(format!("{}.conf", site.id)));
    let _ = std::fs::remove_file(paths.nginx_sites_dir().join(format!("{}.conf.disabled", site.id)));
    let _ = std::fs::remove_file(paths.apache_sites_dir().join(format!("{}.conf.disabled", site.id)));
    if remove_certs {
        for d in &site.domains {
            let _ = std::fs::remove_file(paths.certs().join("sites").join(format!("{d}.crt")));
            let _ = std::fs::remove_file(paths.certs().join("sites").join(format!("{d}.key")));
        }
        for c in store.list_certs()? {
            if site.domains.contains(&c.subject) {
                let _ = store.delete_cert(&c.id);
            }
        }
    }
    store.delete_site(id)?;
    if remove_hosts {
        let _ = crate::hosts::apply(store, paths, None);
    }
    crate::ops::rebuild_and_reload(store, paths, manager)?;
    Ok(())
}

/// 启动站点 = 确保 web server + php 运行 + 配置生效
pub fn start_site(id: &str, paths: &Paths, store: &Store, manager: &Arc<ServiceManager>) -> Result<()> {
    let site = get(store, id)?;
    if let crate::model::SiteKind::Php = site.runtime.kind {
        let ver = site
            .runtime
            .php_version
            .clone()
            .ok_or_else(|| AppError::new("NO_PHP_VERSION", "站点未绑定 PHP 版本"))?;
        let sid = format!("php@{ver}");
        if manager.snapshot(&sid).map(|s| s.state != ServiceState::Running).unwrap_or(true) {
            crate::ops::start_service(store, paths, manager, &sid)?;
        }
    }
    let web_server = if site.runtime.web_server == "apache" { "apache" } else { "nginx" };
    if manager.snapshot(web_server).map(|s| s.state != ServiceState::Running).unwrap_or(true) {
        crate::ops::start_service(store, paths, manager, web_server)?;
    }
    // 恢复 conf（若被停用）
    let dir = if web_server == "apache" { paths.apache_sites_dir() } else { paths.nginx_sites_dir() };
    let disabled = dir.join(format!("{}.conf.disabled", site.id));
    if disabled.exists() {
        let enabled = dir.join(format!("{}.conf", site.id));
        std::fs::rename(&disabled, &enabled)?;
    }
    write_site_conf(paths, store, &site)?;
    crate::ops::rebuild_and_reload(store, paths, manager)?;
    let _ = crate::hosts::apply(store, paths, None);
    Ok(())
}

/// 停止站点：禁用 vhost 并重载（不动 web server 本体）
pub fn stop_site(id: &str, paths: &Paths, store: &Store, manager: &Arc<ServiceManager>) -> Result<()> {
    let site = get(store, id)?;
    let web_server = if site.runtime.web_server == "apache" { "apache" } else { "nginx" };
    let dir = if web_server == "apache" { paths.apache_sites_dir() } else { paths.nginx_sites_dir() };
    let conf = dir.join(format!("{}.conf", site.id));
    if conf.exists() {
        std::fs::rename(&conf, conf.with_extension("conf.disabled"))?;
    }
    // 另一个 web server 若残留同名 conf 也一并禁用
    let other_dir = if web_server == "apache" { paths.nginx_sites_dir() } else { paths.apache_sites_dir() };
    let other = other_dir.join(format!("{}.conf", site.id));
    if other.exists() {
        let _ = std::fs::rename(&other, other.with_extension("conf.disabled"));
    }
    crate::ops::rebuild_and_reload(store, paths, manager)?;
    let mut s = site;
    s.status = "stopped".into();
    s.updated_at = now_ms();
    store.save_site(&s)?;
    Ok(())
}

pub fn write_site_conf(paths: &Paths, store: &Store, site: &Site) -> Result<()> {
    let ports = PortsProfile::from_settings(store);
    if site.runtime.web_server == "apache" {
        let cert_dir = paths.certs().join("sites");
        let php_pool = match site.runtime.kind {
            crate::model::SiteKind::Php => {
                let ver = site.runtime.php_version.as_deref().unwrap_or("8.3");
                store.get_port_assign(&format!("php@{ver}"))
            }
            _ => None,
        };
        let conf = configgen::render_httpd_vhost(site, ports.apache_http, ports.apache_https, &cert_dir, php_pool);
        let path = paths.apache_sites_dir().join(format!("{}.conf", site.id));
        write_with_backup(&path, &conf, &paths.backup())?;
    } else {
        let fastcgi = paths.etc().join("nginx").join("fastcgi_params");
        let cert_dir = paths.certs().join("sites");
        let conf = configgen::render_site_conf(site, ports.http, ports.https, &fastcgi, &cert_dir);
        let path = paths.nginx_sites_dir().join(format!("{}.conf", site.id));
        write_with_backup(&path, &conf, &paths.backup())?;
    }
    Ok(())
}

/* ---- 模板脚手架 ---- */

pub fn scaffold_template(template: &str, root: &std::path::Path, input: &CreateSiteInput) -> Result<()> {
    let is_php = matches!(input.runtime.kind, SiteKind::Php);
    match template {
        "blank-php" => {
            std::fs::write(
                root.join("index.php"),
                "<?php\nheader('Content-Type: text/html; charset=utf-8');\necho '<h1>It works!</h1><p>NiceServBay PHP site.</p>';\necho '<p>PHP ' . PHP_VERSION . '</p>';\n",
            )?;
            std::fs::write(
                root.join("phpinfo.php"),
                "<?php\nphpinfo();\n",
            )?;
        }
        "laravel" => {
            let public = root.join("public");
            std::fs::create_dir_all(&public)?;
            if !public.join("index.php").exists() {
                std::fs::write(
                    public.join("index.php"),
                    "<?php\nrequire __DIR__.'/../vendor/autoload.php';\n// Laravel 入口占位：把 Laravel 项目放进根目录后自动生效\nphpinfo();\n",
                )?;
            }
        }
        "static" => {
            std::fs::write(root.join("index.html"), STATIC_INDEX_HTML)?;
        }
        "wordpress" => {
            // WordPress 入口在根目录的 index.php，且需要 wp-config.php。
            // 这里放一个**可运行的占位入口**：真装 WordPress 时用官方包覆盖即可，
            // 但在此之前访问站点能看到明确指引，而不是 404 让人以为站点没建好。
            std::fs::write(root.join("index.php"), WORDPRESS_PLACEHOLDER)?;
            let wp = root.join("wp-config-sample.php");
            if !wp.exists() {
                std::fs::write(&wp, WORDPRESS_CONFIG_SAMPLE)?;
            }
        }
        "thinkphp" => {
            // ThinkPHP 6+ 的 web 入口在 public/
            let public = root.join("public");
            std::fs::create_dir_all(&public)?;
            if !public.join("index.php").exists() {
                std::fs::write(public.join("index.php"), THINKPHP_ENTRY)?;
            }
            std::fs::write(root.join("NSB-README.md"), THINKPHP_README)?;
        }
        "symfony" => {
            let public = root.join("public");
            std::fs::create_dir_all(&public)?;
            if !public.join("index.php").exists() {
                std::fs::write(public.join("index.php"), SYMFONY_ENTRY)?;
            }
        }
        "codeigniter" => {
            let public = root.join("public");
            std::fs::create_dir_all(&public)?;
            if !public.join("index.php").exists() {
                std::fs::write(public.join("index.php"), CODEIGNITER_ENTRY)?;
            }
        }
        "next-export" => {
            // Next.js 静态导出：文档根应指向 out/
            let out = root.join("out");
            std::fs::create_dir_all(&out)?;
            std::fs::write(out.join("index.html"), STATIC_INDEX_HTML)?;
            std::fs::write(root.join("NSB-README.md"), NEXT_EXPORT_README)?;
        }
        "spa" => {
            std::fs::write(root.join("index.html"), SPA_INDEX_HTML)?;
        }
        _ => {
            // 现有目录：php 站点若无入口给个占位
            if is_php
                && !root.join("index.php").exists()
                && !root.join("public").join("index.php").exists()
            {
                std::fs::write(
                    root.join("index.php"),
                    "<?php\necho '<h1>It works!</h1><p>PHP ' . PHP_VERSION . '</p>';\n",
                )?;
            }
        }
    }
    Ok(())
}

/* ================= 站点模板内容 ================= */

/// 静态站点首页：给一个像样的落地页，而不是一行裸 <h1>
const STATIC_INDEX_HTML: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>站点已就绪</title>
<style>
  :root { color-scheme: light dark }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center;
         font: 15px/1.6 ui-sans-serif, system-ui, "Segoe UI", sans-serif;
         background: #faf9f7; color: #1c1917 }
  @media (prefers-color-scheme: dark) { body { background: #171614; color: #f5f3f1 } }
  .card { max-width: 520px; padding: 32px 36px; border-radius: 14px;
          background: #fff; border: 1px solid #e6e1dc;
          box-shadow: 0 1px 2px rgba(28,25,23,.05), 0 12px 32px -12px rgba(28,25,23,.14) }
  @media (prefers-color-scheme: dark) { .card { background: #1e1c1a; border-color: #ffffff17 } }
  h1 { margin: 0 0 6px; font-size: 19px; letter-spacing: -.02em }
  p { margin: 0 0 4px; color: #78716c }
  code { font-family: ui-monospace, Consolas, monospace; font-size: 13px }
  ul { margin: 14px 0 0; padding-left: 18px; color: #78716c; font-size: 13.5px }
</style>
</head>
<body>
  <div class="card">
    <h1>站点已就绪</h1>
    <p>把项目文件放到这个目录即可上线。</p>
    <ul>
      <li>入口文件：<code>index.html</code></li>
      <li>改完刷新页面就能看到效果，无需重启服务</li>
    </ul>
  </div>
</body>
</html>
"#;

/// SPA 首页：与静态站点的区别在于提示「路由交给前端框架」
const SPA_INDEX_HTML: &str = r#"<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>SPA 已就绪</title>
<style>
  body { margin: 0; min-height: 100vh; display: grid; place-items: center;
         font: 15px/1.6 ui-sans-serif, system-ui, sans-serif; background: #faf9f7; color: #1c1917 }
  @media (prefers-color-scheme: dark) { body { background: #171614; color: #f5f3f1 } }
  .box { text-align: center }
  code { font-family: ui-monospace, Consolas, monospace }
</style>
</head>
<body>
  <div class="box">
    <h1>SPA 已就绪</h1>
    <p>把构建产物（dist / build）里的文件放到这个目录。</p>
    <p>伪静态已设为 SPA fallback，深链路由会回落到 index.html。</p>
  </div>
</body>
</html>
"#;

/// WordPress 占位入口：能跑、能自我说明，覆盖即可
const WORDPRESS_PLACEHOLDER: &str = r#"<?php
// 这是 NiceServBay 生成的占位入口。
// 把 WordPress 官方包解压到这个目录覆盖即可（保留 wp-config.php 里的库配置）。
header('Content-Type: text/html; charset=utf-8');
?>
<!doctype html><meta charset="utf-8"><title>WordPress 站点已就绪</title>
<style>body{font:15px/1.7 ui-sans-serif,system-ui,sans-serif;max-width:640px;margin:12vh auto;padding:0 24px;color:#1c1917}
code{background:#f2efec;padding:2px 5px;border-radius:4px;font-family:ui-monospace,Consolas,monospace;font-size:13px}
li{margin:4px 0}</style>
<h1>WordPress 站点已就绪</h1>
<p>还没有放置 WordPress 文件。两种做法：</p>
<ul>
  <li>下载官方包，解压到 <code><?= htmlspecialchars(__DIR__) ?></code></li>
  <li>或用 WP-CLI：<code>wp core download --locale=zh_CN</code></li>
</ul>
<p>数据库连接模板已写在同目录的 <code>wp-config-sample.php</code>，
改名为 <code>wp-config.php</code> 并填入站点绑定的库名与账号即可。</p>
<?php phpinfo(INFO_GENERAL); ?>
"#;

/// wp-config 样例：把该填的位置标出来，用户改名即可用
const WORDPRESS_CONFIG_SAMPLE: &str = r#"<?php
/**
 * NiceServBay 生成的 wp-config.php 样例。
 * 把 DB_* 换成「站点绑定的数据库」那一组（可在「数据库」页查到），
 * 然后改名为 wp-config.php。
 */
define( 'DB_NAME', 'changeme_database' );
define( 'DB_USER', 'changeme_user' );
define( 'DB_PASSWORD', 'changeme_password' );
define( 'DB_HOST', '127.0.0.1:3306' );
define( 'DB_CHARSET', 'utf8mb4' );
define( 'DB_COLLATE', '' );

/** 到 https://api.wordpress.org/secret-key/1.1/salt/ 生成后替换这四行 */
define( 'AUTH_KEY',         'put your unique phrase here' );
define( 'SECURE_AUTH_KEY',  'put your unique phrase here' );
define( 'LOGGED_IN_KEY',    'put your unique phrase here' );
define( 'NONCE_KEY',        'put your unique phrase here' );

$table_prefix = 'wp_';

define( 'WP_DEBUG', true );
define( 'WP_DEBUG_LOG', true );
define( 'WP_DEBUG_DISPLAY', false );

if ( ! defined( 'ABSPATH' ) ) {
	define( 'ABSPATH', __DIR__ . '/' );
}
require_once ABSPATH . 'wp-settings.php';
"#;

const THINKPHP_ENTRY: &str = r#"<?php
// ThinkPHP 6+ 的入口文件。真项目会覆盖这里。
$autoload = __DIR__ . '/../vendor/autoload.php';
if (is_file($autoload)) {
    require $autoload;
    (new \think\App())->http->run()->send();
} else {
    header('Content-Type: text/html; charset=utf-8');
    echo '<h1>ThinkPHP 站点已就绪</h1>';
    echo '<p>还没装依赖。在项目根目录执行：</p><pre>composer install</pre>';
    phpinfo(INFO_GENERAL);
}
"#;

const THINKPHP_README: &str = r#"# ThinkPHP 站点

NiceServBay 已按 ThinkPHP 6+ 的约定建好：

- **文档根**：`public/`（入口 `public/index.php`）
- **伪静态**：已设为 `thinkphp`

## 第一次运行

```bash
composer install
```

## 目录约定

```
public/index.php   ← web 入口（站点文档根指向这里）
app/                ← 应用代码
config/             ← 配置
```

如果你的入口不在 `public/`，到站点详情里把「文档根」改成实际位置。
"#;

const SYMFONY_ENTRY: &str = r#"<?php
// Symfony 入口（public/index.php）。真项目会覆盖这里。
use Symfony\Component\HttpFoundation\Request;
require __DIR__ . '/../vendor/autoload.php';
if (class_exists(\App\Kernel::class)) {
    $kernel = new \App\Kernel($_SERVER['APP_ENV'] ?? 'dev', (bool) ($_SERVER['APP_DEBUG'] ?? true));
    $kernel->handle(Request::createFromGlobals())->send();
} else {
    header('Content-Type: text/html; charset=utf-8');
    echo '<h1>Symfony 站点已就绪</h1><p>先执行 <code>composer install</code>。</p>';
    phpinfo(INFO_GENERAL);
}
"#;

const CODEIGNITER_ENTRY: &str = r#"<?php
// CodeIgniter 4 入口（public/index.php）。真项目会覆盖这里。
$paths = __DIR__ . '/../app/Config/Paths.php';
if (is_file($paths)) {
    require $paths;
    require __DIR__ . '/../vendor/autoload.php';
    $app = \Config\Services::codeigniter();
    $app->run();
} else {
    header('Content-Type: text/html; charset=utf-8');
    echo '<h1>CodeIgniter 站点已就绪</h1><p>先执行 <code>composer install</code>。</p>';
    phpinfo(INFO_GENERAL);
}
"#;

const NEXT_EXPORT_README: &str = r#"# Next.js 静态导出站点

NiceServBay 已把文档根指向 `out/`（Next.js 静态导出的默认产物目录）。

## 先在 next.config 里开启导出

```js
// next.config.js
module.exports = { output: 'export' }
```

## 构建

```bash
npm install
npm run build     # 产物落在 out/
```

构建完刷新站点即可，不必重启服务。

> 如果你用的是 `npm run dev` 那种按需渲染的开发方式，请改建为
> 「反向代理」类型的站点，指向 dev server 的端口（通常是 3000）。
"#;

#[cfg(test)]
mod scaffold_tests {
    use super::*;
    use crate::model::{CreateSiteInput, SiteKind, SiteRuntime};
    use std::path::PathBuf;

    fn input(kind: SiteKind) -> CreateSiteInput {
        CreateSiteInput {
            name: "t".into(),
            domains: vec!["t.test".into()],
            root_dir: String::new(),
            runtime: SiteRuntime {
                web_server: "nginx".into(),
                kind,
                php_version: Some("8.3.33".into()),
                proxy_target: None,
                command: None,
                cwd: None,
            },
            https: false,
            rewrite: crate::model::RewritePreset::None,
            create_db: None,
            write_env_example: false,
            template: "none".into(),
        }
    }

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("nsb-scaffold-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn has(&self, rel: &str) -> bool {
            self.0.join(rel).is_file()
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 每个模板都必须把入口文件写在**该框架实际要求的位置**。
    /// 这是最容易写错、又最难自查的地方 —— 写错位置站点直接 404，
    /// 而用户会以为是服务器配置问题。
    #[test]
    fn each_template_writes_entry_at_framework_correct_path() {
        let cases: &[(&str, SiteKind, &str)] = &[
            ("blank-php", SiteKind::Php, "index.php"),
            ("static", SiteKind::Static, "index.html"),
            // WordPress 入口在根目录（不是 public/）
            ("wordpress", SiteKind::Php, "index.php"),
            // 以下三个框架的 web 入口都在 public/
            ("thinkphp", SiteKind::Php, "public/index.php"),
            ("symfony", SiteKind::Php, "public/index.php"),
            ("codeigniter", SiteKind::Php, "public/index.php"),
            // Next.js 静态导出产物在 out/
            ("next-export", SiteKind::Static, "out/index.html"),
            ("spa", SiteKind::Static, "index.html"),
            ("laravel", SiteKind::Php, "public/index.php"),
        ];
        for (tpl, kind, entry) in cases {
            let t = Tmp::new(tpl);
            scaffold_template(tpl, &t.0, &input(kind.clone())).unwrap();
            assert!(
                t.has(entry),
                "模板 {tpl} 应在 {entry} 生成入口（否则站点 404）"
            );
        }
    }

    #[test]
    fn wordpress_ships_config_sample() {
        let t = Tmp::new("wpsample");
        scaffold_template("wordpress", &t.0, &input(SiteKind::Php)).unwrap();
        assert!(t.has("wp-config-sample.php"), "应给出配置样例");
        let c = std::fs::read_to_string(t.0.join("wp-config-sample.php")).unwrap();
        // 关键常量必须齐全，否则用户改名后 WordPress 起不来
        for k in ["DB_NAME", "DB_USER", "DB_PASSWORD", "DB_HOST", "table_prefix"] {
            assert!(c.contains(k), "wp-config 样例缺 {k}");
        }
    }

    #[test]
    fn templates_with_extra_steps_ship_a_readme() {
        // 需要用户额外操作（composer install / npm build）的模板要给出说明
        for tpl in ["thinkphp", "next-export"] {
            let t = Tmp::new(tpl);
            scaffold_template(tpl, &t.0, &input(SiteKind::Static)).unwrap();
            assert!(t.has("NSB-README.md"), "{tpl} 应给出下一步说明");
        }
    }

    #[test]
    fn placeholder_entry_is_runnable_php() {
        // 占位入口必须是合法 PHP（<?php 开头），不能是一段 HTML 说明
        for tpl in ["wordpress", "thinkphp", "symfony", "codeigniter"] {
            let t = Tmp::new(tpl);
            scaffold_template(tpl, &t.0, &input(SiteKind::Php)).unwrap();
            let entry = if tpl == "wordpress" { "index.php" } else { "public/index.php" };
            let c = std::fs::read_to_string(t.0.join(entry)).unwrap();
            assert!(c.starts_with("<?php"), "{tpl} 的入口应以 <?php 开头");
        }
    }

    #[test]
    fn blank_php_template_adds_phpinfo_page() {
        let t = Tmp::new("blankphp");
        scaffold_template("blank-php", &t.0, &input(SiteKind::Php)).unwrap();
        assert!(t.has("index.php"));
        assert!(t.has("phpinfo.php"), "方便用户确认 PHP 跑起来了");
    }

    #[test]
    fn static_template_has_no_php_tag() {
        let t = Tmp::new("statictpl");
        scaffold_template("static", &t.0, &input(SiteKind::Static)).unwrap();
        let c = std::fs::read_to_string(t.0.join("index.html")).unwrap();
        assert!(!c.contains("<?php"), "静态模板不该含 PHP 标签");
        assert!(c.contains("<!doctype html") || c.contains("<!DOCTYPE html"));
    }

    #[test]
    fn existing_entry_is_not_overwritten() {
        // 已有项目：模板不能覆盖用户的入口文件
        let t = Tmp::new("nocover");
        std::fs::create_dir_all(t.0.join("public")).unwrap();
        std::fs::write(t.0.join("public/index.php"), "<?php // user code\n").unwrap();
        scaffold_template("thinkphp", &t.0, &input(SiteKind::Php)).unwrap();
        let c = std::fs::read_to_string(t.0.join("public/index.php")).unwrap();
        assert_eq!(c, "<?php // user code\n", "不该覆盖用户已有入口");
    }

    #[test]
    fn none_template_writes_nothing_to_existing_dir() {
        // "none" 表示用现有代码：只有目标是空 PHP 目录时才放占位
        let t = Tmp::new("none");
        std::fs::write(t.0.join("index.php"), "<?php // mine\n").unwrap();
        scaffold_template("none", &t.0, &input(SiteKind::Php)).unwrap();
        let c = std::fs::read_to_string(t.0.join("index.php")).unwrap();
        assert_eq!(c, "<?php // mine\n");
    }

    #[test]
    fn unknown_template_falls_back_without_error() {
        let t = Tmp::new("unknown");
        assert!(scaffold_template("totally-made-up", &t.0, &input(SiteKind::Php)).is_ok());
    }
}
