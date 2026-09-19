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

fn scaffold_template(template: &str, root: &std::path::Path, input: &CreateSiteInput) -> Result<()> {
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
            std::fs::write(
                root.join("index.html"),
                "<!doctype html><meta charset=\"utf-8\"><title>Site</title><h1>It works!</h1><p>NiceServBay 静态站点</p>\n",
            )?;
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
