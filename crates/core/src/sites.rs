//! 站点生命周期：模板脚手架 → vhost 生成 → hosts → 证书 → 数据库 → nginx 重载。

use crate::configgen;
use crate::error::{AppError, Result};
use crate::model::{CreateSiteInput, ServiceState, Site, SiteKind};
use crate::paths::{write_with_backup, Paths};
use crate::services::*;
use crate::store::Store;
use std::sync::Arc;

// 域名校验、站点记录和共享 Web 配置必须串行变更，避免并发创建/启停互相覆盖。
pub(crate) static SITE_CHANGES: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// 站点状态是**派生**的，不是持久化的字段。
///
/// 之前 store 里把 status 硬编码成 "running"，导致所有站点都显示运行中，
/// 批量启停也因此永远认为「已经在跑」。真实依据是磁盘上的 vhost 文件：
/// `.conf` 存在 = 已启用（nginx 会加载），`.conf.disabled` = 已停用。
/// 这比在数据库里存一个会漂移的布尔值可靠 —— 用户手动删过文件也能反映出来。
pub fn derive_status(paths: &Paths, site: &Site) -> &'static str {
    let dirs = [if site.runtime.web_server == "apache" {
        paths.apache_sites_dir()
    } else {
        paths.nginx_sites_dir()
    }];
    let base = format!("{}.conf", site.id);
    let disabled = format!("{}.conf.disabled", site.id);
    let mut any_enabled = false;
    let mut any_disabled = false;
    for d in &dirs {
        if d.join(&base).is_file() {
            any_enabled = true;
        }
        if d.join(&disabled).is_file() {
            any_disabled = true;
        }
    }
    if any_enabled {
        "running"
    } else if any_disabled {
        "stopped"
    } else {
        // 两边都没有：还没写过配置（新建但从未启用）
        "unconfigured"
    }
}

pub fn list(store: &Store) -> Result<Vec<Site>> {
    store.list_sites()
}

/// 带真实状态的站点列表（需要 Paths 才能判断 vhost 在不在）
pub fn list_with_status(
    paths: &Paths,
    store: &Store,
    manager: &ServiceManager,
) -> Result<Vec<Site>> {
    let mut sites = store.list_sites()?;
    for s in sites.iter_mut() {
        s.status = runtime_status(paths, s, manager).to_string();
    }
    Ok(sites)
}

/// 配置启用不代表服务已启动；站点状态还取决于对应 Web 服务与 PHP 池。
pub fn runtime_status(paths: &Paths, site: &Site, manager: &ServiceManager) -> &'static str {
    let configured = derive_status(paths, site);
    if configured != "running" {
        return configured;
    }
    let mut dependencies = vec![site.runtime.web_server.clone()];
    if site.runtime.kind == SiteKind::Php {
        let Some(version) = &site.runtime.php_version else {
            return "unconfigured";
        };
        dependencies.push(format!("php@{version}"));
    }
    let states: Vec<_> = dependencies
        .iter()
        .map(|id| manager.snapshot(id).map(|s| s.state))
        .collect();
    if states
        .iter()
        .any(|s| matches!(s, Some(ServiceState::Error)))
    {
        "error"
    } else if states
        .iter()
        .all(|s| matches!(s, Some(ServiceState::Running)))
    {
        "running"
    } else {
        "stopped"
    }
}

/// 创建与编辑使用相同校验，所有写文件和保存记录操作必须在校验之后。
fn validate_site_fields(
    name: &str,
    domains: &[String],
    root_dir: &str,
    runtime: &crate::model::SiteRuntime,
    store: &Store,
    exclude_id: Option<&str>,
) -> Result<()> {
    if exclude_id.is_some_and(|id| {
        id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }) {
        return Err(AppError::new(
            "BAD_SITE_ID",
            "站点标识无效，无法生成配置文件",
        ));
    }
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(AppError::new("BAD_INPUT", "站点名称不能为空或包含换行符"));
    }
    if domains.is_empty() {
        return Err(AppError::new("BAD_DOMAIN", "请至少填写一个站点域名"));
    }
    for domain in domains {
        let hostname = domain.strip_prefix("*.").unwrap_or(domain);
        if hostname.len() > 253
            || !hostname.contains('.')
            || hostname.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            })
        {
            return Err(AppError::new(
                "BAD_DOMAIN",
                format!("域名 “{domain}” 格式不正确（示例：myproject.test）"),
            ));
        }
    }
    if let Some(existing) = store.list_sites()?.into_iter().find(|site| {
        Some(site.id.as_str()) != exclude_id
            && site
                .domains
                .iter()
                .any(|old| domains.iter().any(|new| old.eq_ignore_ascii_case(new)))
    }) {
        return Err(AppError::new(
            "DOMAIN_CONFLICT",
            format!("域名已被站点「{}」使用", existing.name),
        )
        .with_hint("换一个域名，或到该站点的设置里修改"));
    }
    if !std::path::Path::new(root_dir).is_absolute()
        || root_dir
            .chars()
            .any(|c| c.is_control() || matches!(c, '"' | '$' | ';' | '{' | '}'))
    {
        return Err(AppError::new(
            "BAD_ROOT_DIR",
            "请选择站点的完整目录路径，路径不能包含配置控制字符",
        ));
    }
    if !matches!(runtime.web_server.as_str(), "nginx" | "apache") {
        return Err(AppError::new("BAD_RUNTIME", "请选择 Nginx 或 Apache"));
    }
    if runtime.imported_cert_id.as_deref().is_some_and(|id| !crate::certs::valid_imported_id(id)) {
        return Err(AppError::new("BAD_CERT_ID", "请选择有效的导入证书"));
    }
    if runtime.kind == SiteKind::Php {
        let version = runtime.php_version.as_deref().unwrap_or_default();
        if version.is_empty() || !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return Err(AppError::new(
                "NO_PHP_VERSION",
                "请选择 PHP 版本，或先设置项目的 PHP 版本",
            ));
        }
    } else if runtime.kind != SiteKind::Static {
        proxy_url(runtime.proxy_target.as_deref().unwrap_or_default())?;
    }
    Ok(())
}

fn normalize_domains(domains: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for domain in domains {
        let domain = domain.trim().to_ascii_lowercase();
        if !result.contains(&domain) {
            result.push(domain);
        }
    }
    result
}

/// 兼容 host:port 和完整 HTTP(S) 地址；阻止地址成为服务器配置指令。
pub fn proxy_url(target: &str) -> Result<String> {
    let target = target.trim();
    if target.is_empty()
        || target.chars().any(|c| {
            c.is_whitespace() || matches!(c, '"' | '\'' | ';' | '{' | '}' | '$' | '\\' | '<' | '>')
        })
    {
        return Err(AppError::new(
            "BAD_PROXY_TARGET",
            "请填写有效的 HTTP 或 HTTPS 代理地址",
        ));
    }
    let address = if target.contains("://") {
        target.to_string()
    } else {
        format!("http://{target}")
    };
    let url = reqwest::Url::parse(&address).map_err(|_| {
        AppError::new(
            "BAD_PROXY_TARGET",
            "代理地址格式不正确（示例：http://127.0.0.1:3000）",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
    {
        return Err(AppError::new(
            "BAD_PROXY_TARGET",
            "代理地址仅支持 HTTP/HTTPS，不能包含账号、查询参数或片段",
        ));
    }
    Ok(url.to_string())
}

/// 单个站点 + 真实状态
pub fn get_with_status(paths: &Paths, store: &Store, id: &str) -> Result<Site> {
    let mut site = get(store, id)?;
    site.status = derive_status(paths, &site).to_string();
    Ok(site)
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
    create_with_progress(input, paths, store, manager, &|_, _| {})
}

pub fn create_with_progress(
    input: &CreateSiteInput,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
    progress: &dyn Fn(&str, Option<u8>),
) -> Result<Site> {
    // ---- 校验 ----
    progress("preparing", None);
    let _change = SITE_CHANGES.lock();
    let mut normalized = input.clone();
    normalized.domains = normalize_domains(&input.domains);
    normalized.root_dir = input.root_dir.trim().to_string();
    if normalized.runtime.kind == SiteKind::Php
        && normalized
            .runtime
            .php_version
            .as_deref()
            .unwrap_or_default()
            .is_empty()
    {
        normalized.runtime.php_version =
            read_project_pin(&normalized.root_dir).map(|(_, version)| version);
    }
    let input = &normalized;
    if input.https {
        if let Some(id) = &input.runtime.imported_cert_id {
            crate::certs::validate_imported_domains(paths, id, &input.domains)?;
        }
    }
    validate_site_fields(
        &input.name,
        &input.domains,
        &input.root_dir,
        &input.runtime,
        store,
        None,
    )?;
    if store
        .find_installed(&input.runtime.web_server, None)
        .is_none()
    {
        return Err(AppError::not_installed(&input.runtime.web_server));
    }
    if input.runtime.kind == SiteKind::Php
        && store
            .find_installed("php", input.runtime.php_version.as_deref())
            .is_none()
    {
        return Err(AppError::not_installed(&format!(
            "PHP {}",
            input.runtime.php_version.as_deref().unwrap_or_default()
        )));
    }

    if !matches!(
        input.template.as_str(),
        "" | "none"
            | "blank-php"
            | "static"
            | "spa"
            | "next-export"
            | "wordpress"
            | "laravel"
            | "thinkphp"
            | "symfony"
            | "codeigniter"
    ) {
        return Err(AppError::new(
            "BAD_TEMPLATE",
            "不支持此项目模板，请重新选择",
        ));
    }
    if (matches!(input.template.as_str(), "wordpress" | "blank-php")
        || composer_package(&input.template).is_some())
        && input.runtime.kind != SiteKind::Php
    {
        return Err(AppError::new(
            "BAD_TEMPLATE",
            "所选项目模板需要 PHP 运行环境",
        ));
    }
    if input.template == "next-export"
        && (input.runtime.kind != SiteKind::Static || input.create_db.is_some())
    {
        return Err(AppError::new(
            "BAD_TEMPLATE",
            "Next.js 静态导出需要静态站点类型，不使用数据库绑定",
        ));
    }
    if let Some(db) = &input.create_db {
        crate::dbadmin::validate_create_db(&db.database, &db.username, &db.password)?;
        if store.find_installed("mysql", None).is_none() {
            return Err(AppError::not_installed("MySQL"));
        }
    }

    // 网络下载不持有共享站点锁；落地前再次校验，防止下载期间出现域名冲突。
    drop(_change);
    // ---- 脚手架 ----
    let root = std::path::PathBuf::from(&input.root_dir);
    if input.template == "wordpress" {
        scaffold_wordpress(&root, progress)?;
    } else if let Some(package) = composer_package(&input.template) {
        scaffold_composer(&root, input, package, paths, store, progress)?;
    } else if input.template == "next-export" {
        scaffold_next_export(&root, paths, store, progress)?;
    } else {
        std::fs::create_dir_all(&root).map_err(|e| {
            AppError::io("创建站点根目录", e).with_hint("检查路径是否正确、磁盘是否可写")
        })?;
        scaffold_template(&input.template, &root, input)?;
    }
    let _change = SITE_CHANGES.lock();
    validate_site_fields(
        &input.name,
        &input.domains,
        &input.root_dir,
        &input.runtime,
        store,
        None,
    )
    .map_err(|e| e.with_hint("项目文件已保留，请修改冲突的域名后重试"))?;

    // ---- 数据库 ----
    let mut db_binding = None;
    if let Some(db) = &input.create_db {
        progress("database", None);
        let _database_operation = manager.lifecycle.lock();
        let package = crate::ops::installed_by_choice(store, "mysql")
            .ok_or_else(|| AppError::not_installed("MySQL"))?;
        let version = &package.version;
        let id = format!("mysql@{version}");
        if !manager.snapshot(&id).is_some_and(|s| s.state == ServiceState::Running) {
            crate::ops::start_service(store, paths, manager, &id)?;
        }
        let port = manager.snapshot(&id).and_then(|s| s.port)
            .ok_or_else(|| AppError::new("MYSQL_NOT_RUNNING", "无法确定 MySQL 实际端口"))?;
        let pass = crate::dbadmin::saved_password(store, version)
            .ok_or_else(|| AppError::new("MYSQL_AUTH_REQUIRED", "请先在数据库页更新该实例的 root 连接密码"))?;
        let client = crate::dbadmin::MySqlClient::from_install_dir(std::path::Path::new(&package.install_path), version, port, pass);
        client.verify_data_dir(&paths.mysql_data_dir(version))?;
        client.create_database(&db.database)?;
        client.create_user_grant(&db.username, &db.password, &db.database)?;
        if composer_package(&input.template).is_some() {
            let env_path = root.join(".env");
            let content = std::fs::read_to_string(&env_path).map_err(|error| AppError::io("读取项目数据库配置", error))?;
            let changes = crate::envfile::project_db_env_vars(&root, &crate::envfile::DbHint {
                database: db.database.clone(), username: db.username.clone(), password: db.password.clone(), port,
            })?;
            let updated = crate::envfile::apply_project_env_changes(&root, &content, &changes)?;
            if updated != content {
                use std::io::Write;
                let mut pending = tempfile::NamedTempFile::new_in(&root)?;
                pending.write_all(updated.as_bytes())?;
                pending.as_file().sync_all()?;
                if std::fs::read_to_string(&env_path)? != content {
                    return Err(AppError::new("ENV_CHANGED", "项目配置已变化，请检查后重试"));
                }
                std::fs::write(root.join(".env.nsb-backup"), &content)?;
                pending.persist(&env_path).map_err(|error| AppError::io("更新项目数据库配置", error.error))?;
            }
        }
        if input.template == "wordpress" {
            write_wordpress_config(&root, db, port)?;
        }
        if input.write_env_example {
            let env = crate::dbadmin::render_env_example(
                &db.database,
                &db.username,
                &db.password,
                port,
            );
            write_scaffold_file(root.join(".env.example"), &env)?;
        }
        db_binding = Some(crate::model::SiteDbBinding {
            enabled: true, database: db.database.clone(), username: db.username.clone(),
            password: db.password.clone(), version: Some(version.clone()), port: Some(port),
        });
    }

    // ---- 站点记录 ----
    progress("configuring", None);
    // 项目级运行时锁定：rootDir/.nsb.json 里 {"php": "8.3.33"} 优先于向导选择
    // （向导留空「跟随项目」时生效；显式选了版本则以向导为准）
    let mut runtime = input.runtime.clone();
    if runtime.kind == crate::model::SiteKind::Php && runtime.php_version.is_none() {
        if let Some((_, ver)) = read_project_pin(&input.root_dir) {
            runtime.php_version = Some(ver);
        }
    }
    let now = now_ms();
    let site = Site {
        id: format!("site-{}-{:08x}", now, rand::random::<u32>()),
        name: input.name.trim().to_string(),
        domains: input.domains.clone(),
        root_dir: match input.template.as_str() {
            "laravel" | "thinkphp" | "symfony" | "codeigniter" => root.join("public"),
            "next-export" => root.join("out"),
            _ => root.clone(),
        }
        .to_string_lossy()
        .to_string(),
        runtime,
        https: input.https,
        rewrite: input.rewrite.clone(),
        db: db_binding,
        php_overrides: input.php_overrides.clone(),
        status: "running".into(),
        created_at: now,
        updated_at: now,
    };
    store.save_site(&site)?;
    write_user_ini(&site);

    let result: Result<()> = (|| {
        if site.https && site.runtime.imported_cert_id.is_none() {
            crate::tls::issue_site_cert(paths, store, &site.domains)?;
        }
        progress("starting", None);
        start_site_inner(&site.id, paths, store, manager)
    })();
    if let Err(error) = result {
        store.delete_site(&site.id)?;
        return Err(error.with_hint("站点未创建成功，项目文件和数据库已保留；请根据错误修复后重试"));
    }

    Ok(site)
}

pub fn update(
    site_patch: &Site,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
) -> Result<Site> {
    let _change = SITE_CHANGES.lock();
    let original = get(store, &site_patch.id)?;
    let enabled = derive_status(paths, &original) == "running";
    let was_running = runtime_status(paths, &original, manager) == "running";
    let mut current = original.clone();
    current.name = site_patch.name.trim().to_string();
    current.domains = normalize_domains(&site_patch.domains);
    current.root_dir = site_patch.root_dir.trim().to_string();
    current.runtime = site_patch.runtime.clone();
    let certificate_changed =
        current.https != site_patch.https || current.domains != original.domains
            || current.runtime.imported_cert_id != original.runtime.imported_cert_id;
    current.https = site_patch.https;
    current.rewrite = site_patch.rewrite.clone();
    current.php_overrides = site_patch.php_overrides.clone();
    current.updated_at = now_ms();
    validate_site_fields(
        &current.name,
        &current.domains,
        &current.root_dir,
        &current.runtime,
        store,
        Some(&current.id),
    )?;
    if !std::path::Path::new(&current.root_dir).is_dir() {
        return Err(AppError::new(
            "BAD_ROOT_DIR",
            "站点根目录不存在，请重新选择目录",
        ));
    }
    crate::certs::validate_site_certificate(paths, &current)?;
    let local_certificate_changed = certificate_changed && current.https && current.runtime.imported_cert_id.is_none();
    let mut snapshots = snapshot_site_configs(paths, &current)?;
    let certificate_id = format!("cert-{}", current.domains[0]);
    let previous_certificate = if local_certificate_changed {
        let stem = current.domains[0].replace('*', "_wildcard");
        for extension in ["crt", "key"] {
            let path = paths
                .certs()
                .join("sites")
                .join(format!("{stem}.{extension}"));
            let content = match std::fs::read(&path) {
                Ok(content) => Some(content),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            snapshots.push((path, content));
        }
        store
            .list_certs()?
            .into_iter()
            .find(|cert| cert.id == certificate_id)
    } else {
        None
    };
    let result: Result<()> = (|| {
        if local_certificate_changed {
            crate::tls::issue_site_cert(paths, store, &current.domains)?;
        }
        // 更换 PHP 版本时先启动新的池，确保新配置引用的 upstream 已就绪。
        if was_running {
            ensure_php_running(paths, store, manager, &current)?;
        }
        write_site_conf_state(paths, store, &current, enabled)?;
        store.save_site(&current)?;
        if was_running
            && manager
                .snapshot(&current.runtime.web_server)
                .is_none_or(|s| s.state != ServiceState::Running)
        {
            crate::ops::start_service(store, paths, manager, &current.runtime.web_server)?;
        }
        crate::ops::rebuild_and_reload(store, paths, manager)?;
        if current.domains != original.domains {
            crate::hosts::apply(store, paths, None)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        restore_site_configs(&snapshots)?;
        store.save_site(&original)?;
        if local_certificate_changed {
            if let Some(certificate) = previous_certificate {
                store.save_cert(&certificate)?;
            } else {
                store.delete_cert(&certificate_id)?;
            }
        }
        if let Err(restore_error) = crate::ops::rebuild_and_reload(store, paths, manager) {
            return Err(error.with_hint(format!(
                "已恢复原站点配置，但服务恢复失败：{}",
                restore_error.message
            )));
        }
        return Err(error);
    }
    write_user_ini(&current);
    current.status = runtime_status(paths, &current, manager).to_string();
    Ok(current)
}

fn snapshot_site_configs(
    paths: &Paths,
    site: &Site,
) -> Result<Vec<(std::path::PathBuf, Option<Vec<u8>>)>> {
    let mut snapshots = Vec::new();
    for dir in [paths.nginx_sites_dir(), paths.apache_sites_dir()] {
        for suffix in ["conf", "conf.disabled"] {
            let path = dir.join(format!("{}.{suffix}", site.id));
            let content = match std::fs::read(&path) {
                Ok(content) => Some(content),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            snapshots.push((path, content));
        }
    }
    Ok(snapshots)
}

fn restore_site_configs(snapshots: &[(std::path::PathBuf, Option<Vec<u8>>)]) -> Result<()> {
    for (path, content) in snapshots {
        if let Some(content) = content {
            std::fs::write(path, content)?;
        } else if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    Ok(())
}

pub fn delete(
    id: &str,
    remove_hosts: bool,
    remove_certs: bool,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
) -> Result<()> {
    let _change = SITE_CHANGES.lock();
    let _operation = manager.lifecycle.lock();
    let _files = crate::tls::CERT_FILES.lock();
    let _hosts = crate::hosts::HOSTS_CHANGES.lock();
    let site = get(store, id)?;
    let mut files = Vec::new();
    for server in ["nginx", "apache"] {
        for suffix in ["conf", "conf.disabled"] {
            files.push(crate::paths::checked_data_path(
                &paths.base, &format!("etc/{server}/sites/{}.{suffix}", site.id),
            )?);
        }
    }
    // 只清理此站点主域名对应的本地签发证书。导入证书、ACME 证书和其它站点
    // 使用的证书保留；别名不能成为删除另一个证书的依据。
    let certificate = if remove_certs && site.runtime.imported_cert_id.is_none() {
        let certs = store.list_certs()?;
        let all_sites = store.list_sites()?;
        let automations = store.list_cert_automations()?;
        certs.iter().find(|cert| {
            cert.kind == "site"
                && site.domains.first() == Some(&cert.subject)
                && !all_sites.iter().any(|other| other.id != site.id
                    && other.runtime.imported_cert_id.is_none()
                    && other.domains.first() == Some(&cert.subject))
                && !automations.iter().any(|a| a.domains.first() == Some(&cert.subject))
        }).cloned()
    } else {
        None
    };
    if let Some(cert) = &certificate {
        let primary = crate::tls::normalize_domains(&[cert.subject.clone()])?.remove(0);
        let stem = primary.replace('*', "_wildcard").replace(':', "_");
        for extension in ["crt", "key"] {
            files.push(crate::paths::checked_data_path(
                &paths.base, &format!("certs/sites/{stem}.{extension}"),
            )?);
        }
    }
    let previous_hosts = crate::hosts::extra_entries(store)?;
    let running: Vec<_> = ["nginx", "apache"].into_iter().filter(|server|
        manager.snapshot(server).is_some_and(|s| s.state == ServiceState::Running)
    ).collect();
    // 先移动到同一数据目录的暂存区，保留文件内容和权限。全部生效后才真正删除，
    // 恢复失败时保留暂存文件，不能让 TempDir::drop 把仅存的副本清掉。
    let backup = crate::paths::checked_data_path(&paths.base, "backup")?;
    let staging = tempfile::Builder::new().prefix("site-delete-").tempdir_in(backup)?;
    let mut moved = Vec::new();
    let mut record_removed = false;
    let mut cert_removed = false;
    let mut hosts_changed = false;
    let mut web_changed = false;
    let result: Result<()> = (|| {
        for (index, source) in files.iter().enumerate() {
            match std::fs::symlink_metadata(source) {
                Ok(meta) if meta.is_file() => {
                    let target = staging.path().join(index.to_string());
                    std::fs::rename(source, &target)
                        .map_err(|e| AppError::io(&format!("暂存待删除文件 {}", source.display()), e))?;
                    moved.push((source.clone(), target));
                }
                Ok(_) => return Err(AppError::new("SITE_FILE_INVALID", "站点配置或证书路径不是普通文件，未删除")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(cert) = &certificate {
            store.delete_cert(&cert.id)?;
            cert_removed = true;
        }
        store.delete_site(id)?;
        record_removed = true;
        if remove_hosts {
            hosts_changed = true;
            crate::hosts::apply(store, paths, None)?;
        } else {
            // 保留为手动托管条目，避免下次重建 hosts 时又被移除。
            let mut retained = previous_hosts.clone();
            for domain in site.domains.iter().filter(|domain| !domain.starts_with("*.") && domain.parse::<std::net::IpAddr>().is_err()) {
                if !retained.iter().any(|(_, host)| host == domain) {
                    retained.push(("127.0.0.1".into(), domain.clone()));
                }
            }
            crate::hosts::set_extra_entries(store, &retained)?;
            hosts_changed = true;
        }
        web_changed = true;
        crate::ops::rebuild_and_reload(store, paths, manager)?;
        Ok(())
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        for (source, temporary) in moved.iter().rev() {
            // 不覆盖删除过程中由外部程序新建的同名文件。
            let restored = match std::fs::symlink_metadata(source) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::rename(temporary, source),
                Err(e) => Err(e),
                Ok(_) => Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "原路径已被占用")),
            };
            if let Err(e) = restored { failures.push(format!("{}：{e}", source.display())); }
        }
        if record_removed {
            if let Err(e) = store.save_site(&site) { failures.push(format!("恢复站点记录：{e}")); }
        }
        if cert_removed {
            if let Some(cert) = &certificate {
                if let Err(e) = store.save_cert(cert) { failures.push(format!("恢复证书记录：{e}")); }
            }
        }
        if hosts_changed {
            if remove_hosts {
                if let Err(e) = crate::hosts::apply(store, paths, None) { failures.push(format!("恢复 hosts：{e}")); }
            } else if let Err(e) = crate::hosts::set_extra_entries(store, &previous_hosts) {
                failures.push(format!("恢复 hosts 选项：{e}"));
            }
        }
        if web_changed && failures.is_empty() {
            if let Err(e) = crate::ops::rebuild_and_reload(store, paths, manager) {
                failures.push(format!("恢复 Web 配置：{e}"));
            }
            for server in running {
                if let Err(e) = crate::ops::start_service(store, paths, manager, server) {
                    failures.push(format!("恢复 {server}：{e}"));
                }
            }
        }
        if !failures.is_empty() {
            let recovery = staging.keep();
            return Err(AppError::new("SITE_DELETE_ROLLBACK_FAILED", "删除未完成，部分站点状态未能恢复")
                .with_hint(format!("请勿删除恢复目录 {}；查看错误详情后恢复原配置。", recovery.display()))
                .with_detail(format!("{}；{}；文件映射：{moved:?}", error.message, failures.join("；"))));
        }
        let hint = error.hint.clone().unwrap_or_default();
        return Err(error.with_hint(format!("删除未完成，原站点配置已恢复。{hint}")));
    }
    let staging_path = staging.path().to_path_buf();
    staging.close().map_err(|e| AppError::new("SITE_DELETE_CLEANUP_FAILED", "站点已删除，但暂存文件清理失败")
        .with_hint(format!("无需再次删除站点。请检查并清理暂存目录 {}。", staging_path.display()))
        .with_detail(e.to_string()))?;
    Ok(())
}

/// 启动站点 = 确保 web server + php 运行 + 配置生效
pub fn start_site(
    id: &str,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
) -> Result<()> {
    let _change = SITE_CHANGES.lock();
    start_site_inner(id, paths, store, manager)
}

fn start_site_inner(
    id: &str,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
) -> Result<()> {
    let site = get(store, id)?;
    validate_site_fields(
        &site.name,
        &site.domains,
        &site.root_dir,
        &site.runtime,
        store,
        Some(id),
    )?;
    let snapshots = snapshot_site_configs(paths, &site)?;
    let result: Result<()> = (|| {
        ensure_php_running(paths, store, manager, &site)?;
        write_site_conf(paths, store, &site)?;
        let web_server = &site.runtime.web_server;
        if manager
            .snapshot(web_server)
            .is_none_or(|s| s.state != ServiceState::Running)
        {
            crate::ops::start_service(store, paths, manager, web_server)?;
        } else {
            crate::ops::rebuild_and_reload(store, paths, manager)?;
        }
        crate::hosts::apply(store, paths, None)?;
        Ok(())
    })();
    if let Err(error) = result {
        restore_site_configs(&snapshots)?;
        if let Err(restore_error) = crate::ops::rebuild_and_reload(store, paths, manager) {
            return Err(error.with_hint(format!(
                "已恢复站点配置，但服务恢复失败：{}",
                restore_error.message
            )));
        }
        return Err(error);
    }
    Ok(())
}

fn ensure_php_running(
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
    site: &Site,
) -> Result<()> {
    if let crate::model::SiteKind::Php = site.runtime.kind {
        let ver = site
            .runtime
            .php_version
            .clone()
            .ok_or_else(|| AppError::new("NO_PHP_VERSION", "站点未绑定 PHP 版本"))?;
        let sid = format!("php@{ver}");
        if manager
            .snapshot(&sid)
            .map(|s| s.state != ServiceState::Running)
            .unwrap_or(true)
        {
            crate::ops::start_service(store, paths, manager, &sid)?;
        }
    }
    Ok(())
}

/// 停止站点：禁用 vhost 并重载（不动 web server 本体）
pub fn stop_site(
    id: &str,
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
) -> Result<()> {
    let _change = SITE_CHANGES.lock();
    let site = get(store, id)?;
    // 与批量停止共用同一段禁用逻辑，避免两处实现漂移
    let snapshots = snapshot_site_configs(paths, &site)?;
    disable_site_conf(paths, &site)?;
    if let Err(error) = crate::ops::rebuild_and_reload(store, paths, manager) {
        restore_site_configs(&snapshots)?;
        return Err(error);
    }
    let mut s = site;
    s.status = "stopped".into();
    s.updated_at = now_ms();
    store.save_site(&s)?;
    Ok(())
}

pub fn write_site_conf(paths: &Paths, store: &Store, site: &Site) -> Result<()> {
    write_site_conf_state(paths, store, site, true)
}

fn write_site_conf_state(paths: &Paths, store: &Store, site: &Site, enabled: bool) -> Result<()> {
    crate::certs::validate_site_certificate(paths, site)?;
    let extension = if enabled { "conf" } else { "conf.disabled" };
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
        let conf = configgen::render_httpd_vhost(
            site,
            ports.apache_http,
            ports.apache_https,
            &cert_dir,
            php_pool,
        );
        let path = paths
            .apache_sites_dir()
            .join(format!("{}.{extension}", site.id));
        write_with_backup(&path, &conf, &paths.backup())?;
    } else {
        let fastcgi = paths.etc().join("nginx").join("fastcgi_params");
        let cert_dir = paths.certs().join("sites");
        let conf = configgen::render_site_conf(
            site,
            ports.http,
            ports.https,
            &fastcgi,
            &cert_dir,
            &paths.logs().join("nginx"),
        );
        let path = paths
            .nginx_sites_dir()
            .join(format!("{}.{extension}", site.id));
        write_with_backup(&path, &conf, &paths.backup())?;
    }
    // 切换服务器或启停状态后只保留目标配置，避免两个服务器同时加载旧站点。
    for (server, dir) in [
        ("nginx", paths.nginx_sites_dir()),
        ("apache", paths.apache_sites_dir()),
    ] {
        for suffix in ["conf", "conf.disabled"] {
            if server == site.runtime.web_server && suffix == extension {
                continue;
            }
            let stale = dir.join(format!("{}.{suffix}", site.id));
            if stale.exists() {
                std::fs::remove_file(stale)?;
            }
        }
    }
    Ok(())
}

/* ---- 模板脚手架 ---- */

fn composer_package(template: &str) -> Option<&'static str> {
    match template {
        "laravel" => Some("laravel/laravel"),
        "thinkphp" => Some("topthink/think"),
        "symfony" => Some("symfony/skeleton"),
        "codeigniter" => Some("codeigniter4/appstarter"),
        _ => None,
    }
}

fn validate_template_php(template: &str, version: &str) -> Result<()> {
    let mut numbers = version.split('.').map(|n| n.parse::<u16>().unwrap_or(0));
    let actual = (numbers.next().unwrap_or(0), numbers.next().unwrap_or(0));
    let minimum = if template == "thinkphp" {
        (8, 0)
    } else {
        (8, 2)
    };
    if actual < minimum {
        return Err(AppError::new(
            "PHP_VERSION_TOO_OLD",
            format!("此模板需要 PHP {}.{} 或更高版本", minimum.0, minimum.1),
        )
        .with_hint("请在运行时步骤选择兼容的 PHP，缺少时先到套件页安装"));
    }
    Ok(())
}

struct ProjectPhp {
    php: std::path::PathBuf,
    composer: std::path::PathBuf,
    ini: std::path::PathBuf,
    path: std::ffi::OsString,
}

impl ProjectPhp {
    fn resolve(paths: &Paths, store: &Store, version: &str) -> Result<Self> {
        let php = store
            .find_installed("php", Some(version))
            .ok_or_else(|| AppError::not_installed("PHP"))?;
        let composer = crate::ops::installed_by_choice(store, "composer")
            .ok_or_else(|| AppError::not_installed("Composer"))?;
        let installer = crate::install::Installer::effective(paths);
        let entry = std::path::PathBuf::from(&php.install_path).join(
            crate::install::entry_relative_path(&installer.installed_entry(&php).entry),
        );
        let bin = entry
            .parent()
            .ok_or_else(|| AppError::new("BROKEN_INSTALL", "PHP 安装入口无效"))?;
        let php_exe = [
            bin.join(crate::ops::exe_name("php")),
            bin.join("../bin/php"),
            std::path::Path::new(&php.install_path).join(crate::ops::exe_name("php")),
            std::path::Path::new(&php.install_path).join("bin/php"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            AppError::new(
                "BROKEN_INSTALL",
                "所选版本缺少 PHP CLI，请修复或重新安装该版本",
            )
        })?
        .canonicalize()?;
        let composer_exe = std::path::PathBuf::from(&composer.install_path).join(
            crate::install::entry_relative_path(&installer.installed_entry(&composer).entry),
        );
        let ini = paths.php_ini(version);
        if !composer_exe.is_file() || !ini.is_file() {
            return Err(
                AppError::new("BROKEN_INSTALL", "缺少 Composer 程序或所选 PHP 的配置文件")
                    .with_hint("请在套件页修复 PHP 和 Composer 后重试"),
            );
        }
        let mut dirs = vec![php_exe
            .parent()
            .ok_or_else(|| AppError::new("BROKEN_INSTALL", "PHP 路径无效"))?
            .to_path_buf()];
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        let path = std::env::join_paths(dirs)
            .map_err(|e| AppError::internal("准备 PHP 命令路径", e.to_string()))?;
        Ok(Self {
            php: php_exe,
            composer: composer_exe,
            ini,
            path,
        })
    }

    fn command(&self, cwd: &std::path::Path) -> std::process::Command {
        let mut cmd = platform::command(&self.php);
        cmd.arg("-c")
            .arg(&self.ini)
            .args(["-d", "memory_limit=-1"])
            .current_dir(cwd)
            .env("PATH", &self.path)
            .env("PHPRC", &self.ini)
            .env("PHP_INI_SCAN_DIR", "")
            .env("COMPOSER_PROCESS_TIMEOUT", "300")
            .env("COMPOSER_NO_INTERACTION", "1");
        // 项目路径和平台校验必须由本次创建决定，不能被启动应用时继承的变量改写。
        for key in [
            "COMPOSER",
            "COMPOSER_VENDOR_DIR",
            "COMPOSER_BIN_DIR",
            "COMPOSER_ROOT_VERSION",
            "COMPOSER_IGNORE_PLATFORM_REQS",
            "COMPOSER_IGNORE_PLATFORM_REQ",
            "COMPOSER_NO_DEV",
        ] {
            cmd.env_remove(key);
        }
        cmd
    }

    fn composer_command(&self, cwd: &std::path::Path) -> std::process::Command {
        let mut cmd = self.command(cwd);
        cmd.arg(&self.composer)
            .args(["--no-interaction", "--no-ansi"]);
        cmd
    }
}

/// 有界运行且收回整个自有进程组，日志写文件避免 stdout/stderr 管道互相等待。
fn run_project_command(
    cmd: &mut std::process::Command,
    label: &str,
    timeout: std::time::Duration,
) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    use std::process::Stdio;
    let mut output = tempfile::tempfile()?;
    cmd.stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output.try_clone()?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(platform::spawn_pre_exec);
        }
    }
    let mut group = platform::ProcessGroup::new()?;
    let mut child = cmd.spawn().map_err(|e| AppError::io(label, e))?;
    if let Err(e) = group.attach(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.into());
    }
    let started = std::time::Instant::now();
    let result = (|| loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            return Err(AppError::new(
                "PROJECT_INSTALL_TIMEOUT",
                format!("{label}超时"),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    })();
    // 包括命令成功后遗留的子进程，均不能继续占用临时项目目录。
    let cleanup = group.terminate(true);
    if result.is_err() || cleanup.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let len = output.metadata()?.len();
    output.seek(SeekFrom::Start(len.saturating_sub(65536)))?;
    let mut tail = Vec::new();
    output.read_to_end(&mut tail)?;
    let detail = String::from_utf8_lossy(&tail).into_owned();
    let status = result.map_err(|e: AppError| {
        e.with_detail(&detail)
            .with_hint("请检查网络和项目运行环境后重试；原项目目录中的文件未改动")
    })?;
    cleanup?;
    if !status.success() {
        return Err(
            AppError::new("PROJECT_INSTALL_FAILED", format!("{label}失败"))
                .with_hint("展开错误详情查看依赖或网络错误，修复后可直接重试")
                .with_detail(detail),
        );
    }
    Ok(detail)
}

struct ProjectNode {
    node: std::path::PathBuf,
    path: std::ffi::OsString,
}

impl ProjectNode {
    fn resolve(paths: &Paths, store: &Store) -> Result<Self> {
        let installed = crate::ops::installed_by_choice(store, "node").ok_or_else(|| {
            AppError::not_installed("Node.js")
                .with_hint("请先在套件页安装并启用 Node.js 20.9 或更高版本")
        })?;
        let mut version = installed
            .version
            .split('.')
            .map(|part| part.parse::<u16>().unwrap_or(0));
        if (version.next().unwrap_or(0), version.next().unwrap_or(0)) < (20, 9) {
            return Err(AppError::new(
                "NODE_VERSION_TOO_OLD",
                "此模板需要 Node.js 20.9 或更高版本",
            )
            .with_hint("请在套件页切换正在使用的 Node.js 版本"));
        }
        let entry = crate::install::Installer::effective(paths).installed_entry(&installed);
        let node = std::path::PathBuf::from(&installed.install_path)
            .join(crate::install::entry_relative_path(&entry.entry));
        if !node.is_file() {
            return Err(AppError::new(
                "BROKEN_INSTALL",
                "找不到所选 Node.js 的可执行文件，请修复该套件",
            ));
        }
        let node = node.canonicalize()?;
        let mut dirs = vec![node
            .parent()
            .ok_or_else(|| AppError::new("BROKEN_INSTALL", "Node.js 路径无效"))?
            .to_path_buf()];
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        let path = std::env::join_paths(dirs)
            .map_err(|e| AppError::internal("准备 Node.js 路径", e.to_string()))?;
        Ok(Self { node, path })
    }

    fn command(&self, cwd: &std::path::Path) -> std::process::Command {
        let mut cmd = platform::command(&self.node);
        cmd.current_dir(cwd)
            .env("PATH", &self.path)
            .env("CI", "1")
            .env("NEXT_TELEMETRY_DISABLED", "1")
            .env_remove("NODE_OPTIONS")
            .env_remove("NODE_PATH");
        cmd
    }
}

/// pnpm 自包含的 JS 发行线可直接用所选 Node 执行，无需全局安装或修改用户配置。
fn prepare_project_pnpm(tools_dir: &std::path::Path) -> Result<(std::path::PathBuf, String)> {
    use base64::Engine;
    use sha2::{Digest, Sha512};
    use std::io::Read;
    std::fs::create_dir_all(tools_dir)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    // latest-10 保持 Node 可执行的包结构；最新 12.x 已改为依赖平台二进制的引导包。
    let metadata: serde_json::Value = client
        .get("https://registry.npmjs.org/pnpm/latest-10")
        .send()?
        .error_for_status()?
        .json()?;
    let version = metadata["version"]
        .as_str()
        .filter(|s| s.starts_with("10.") && s.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .ok_or_else(|| AppError::new("BAD_PACKAGE_METADATA", "pnpm 版本信息无效"))?
        .to_string();
    let integrity = metadata["dist"]["integrity"]
        .as_str()
        .and_then(|s| s.strip_prefix("sha512-"))
        .ok_or_else(|| AppError::new("BAD_PACKAGE_METADATA", "pnpm 缺少完整性校验信息"))?;
    let expected = base64::engine::general_purpose::STANDARD
        .decode(integrity)
        .map_err(|e| AppError::internal("读取 pnpm 校验信息", e.to_string()))?;
    let mut bytes = Vec::new();
    client
        .get(format!(
            "https://registry.npmjs.org/pnpm/-/pnpm-{version}.tgz"
        ))
        .send()?
        .error_for_status()?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 32 * 1024 * 1024 || Sha512::digest(&bytes).as_slice() != expected.as_slice() {
        return Err(AppError::new(
            "CHECKSUM_MISMATCH",
            "pnpm 下载包完整性校验失败，请重试",
        ));
    }
    let archive = tools_dir.join("pnpm.tgz");
    std::fs::write(&archive, bytes)?;
    // 仅展开通过官方 SHA-512 校验的发行包，使用现有系统 tar，不经过 shell。
    run_project_command(
        platform::command("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(tools_dir),
        "解压 pnpm",
        std::time::Duration::from_secs(60),
    )?;
    let pnpm = tools_dir.join("package/bin/pnpm.cjs");
    if !pnpm.is_file() {
        return Err(AppError::new(
            "INCOMPLETE_PACKAGE",
            "pnpm 发行包缺少运行入口",
        ));
    }
    Ok((pnpm, version))
}

fn prepare_next_project(
    project: &std::path::Path,
    staging: &std::path::Path,
    paths: &Paths,
    store: &Store,
    progress: &dyn Fn(&str, Option<u8>),
) -> Result<ProjectNode> {
    let node = ProjectNode::resolve(paths, store)?;
    let timeout = std::time::Duration::from_secs(600);
    progress("installing", None);
    let tools_dir = staging.join("tools");
    let (pnpm, pnpm_version) = prepare_project_pnpm(&tools_dir)?;
    let config_home = tools_dir.join("config");
    std::fs::create_dir_all(&config_home)?;
    let npmrc = config_home.join("npmrc");
    std::fs::write(&npmrc, "")?;
    let registry = store
        .get_setting("npmRegistry")
        .unwrap_or_else(|| "https://registry.npmjs.org".into());
    let command = |cwd: &std::path::Path| {
        let mut cmd = node.command(cwd);
        // 配置与缓存限定在 NiceEnv 内，不写用户的 pnpm 配置或 create-next-app 偏好。
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_lowercase()
                .starts_with("npm_config_")
            {
                cmd.env_remove(key);
            }
        }
        cmd.arg(&pnpm)
            .env("npm_config_registry", &registry)
            .env("npm_config_userconfig", &npmrc)
            .env("npm_config_globalconfig", &npmrc)
            .env("npm_config_store_dir", paths.downloads().join("pnpm-store"))
            .env("npm_config_cache", paths.downloads().join("pnpm-cache"))
            .env("npm_config_manage_package_manager_versions", "false")
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_CACHE_HOME", paths.downloads().join("node-cache"))
            .env("APPDATA", &config_home)
            .env("LOCALAPPDATA", &config_home);
        cmd
    };
    run_project_command(
        command(staging).args([
            "dlx",
            "create-next-app@latest",
            "project",
            "--use-pnpm",
            "--skip-install",
            "--disable-git",
            "--yes",
            "--ts",
            "--app",
            "--src-dir",
            "--no-tailwind",
            "--no-linter",
            "--no-react-compiler",
            "--no-cache-components",
            "--empty",
            "--import-alias",
            "@/*",
        ]),
        "生成官方 Next.js 项目",
        timeout,
    )?;
    progress("initializing", None);
    // 这里只修改本次临时目录里由官方生成器创建的项目，既有项目不会进入此分支。
    let manifest_path = project.join("package.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path)?)
            .map_err(|e| AppError::internal("读取 Next.js 项目清单", e.to_string()))?;
    manifest["packageManager"] = serde_json::json!(format!("pnpm@{pnpm_version}"));
    // next start 不支持 output: export；访问由 NiceEnv 的 Web 服务负责。
    if let Some(scripts) = manifest.get_mut("scripts").and_then(|v| v.as_object_mut()) {
        scripts.remove("start");
    }
    std::fs::write(
        manifest_path,
        serde_json::to_vec_pretty(&manifest)
            .map_err(|e| AppError::internal("保存 Next.js 项目清单", e.to_string()))?,
    )?;
    std::fs::write(project.join("next.config.ts"), NEXT_EXPORT_CONFIG)?;
    std::fs::write(project.join("src/app/layout.tsx"), NEXT_EXPORT_LAYOUT)?;
    std::fs::write(project.join("src/app/page.tsx"), NEXT_EXPORT_PAGE)?;
    std::fs::write(project.join("README.md"), NEXT_EXPORT_README)?;
    progress("installing", None);
    run_project_command(
        command(project).args(["install", "--ignore-scripts", "--strict-peer-dependencies"]),
        "安装 Next.js 依赖",
        timeout,
    )?;
    progress("validating", None);
    run_project_command(
        node.command(project)
            .args(["node_modules/next/dist/bin/next", "typegen"]),
        "生成 Next.js 路由类型",
        timeout,
    )?;
    run_project_command(
        node.command(project)
            .args(["node_modules/typescript/bin/tsc", "--noEmit"]),
        "检查 Next.js 项目类型",
        timeout,
    )?;
    Ok(node)
}

fn next_export_ready(root: &std::path::Path) -> bool {
    root.join("out/index.html").is_file()
        && root.join("out/_next").is_dir()
        && std::fs::read_to_string(root.join("package.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .is_some_and(|json| json["dependencies"]["next"].is_string())
}

fn scaffold_next_export(
    root: &std::path::Path,
    paths: &Paths,
    store: &Store,
    progress: &dyn Fn(&str, Option<u8>),
) -> Result<()> {
    // 允许接入或重试已构建项目，不执行其用户脚本，也不覆盖源码与构建结果。
    if next_export_ready(root) {
        return Ok(());
    }
    ensure_empty_project_dir(root)?;
    ProjectNode::resolve(paths, store)?;
    let parent = root
        .parent()
        .filter(|_| root.file_name().is_some())
        .ok_or_else(|| AppError::new("BAD_ROOT_DIR", "请选择独立项目目录"))?;
    std::fs::create_dir_all(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".niceenv-next-")
        .tempdir_in(parent)?;
    let project = staging.path().join("project");
    let node = prepare_next_project(&project, staging.path(), paths, store, progress)?;
    progress("building", None);
    run_project_command(
        node.command(&project)
            .args(["node_modules/next/dist/bin/next", "build"]),
        "生成 Next.js 静态导出",
        std::time::Duration::from_secs(600),
    )?;
    if !next_export_ready(&project) {
        return Err(
            AppError::new("INCOMPLETE_PROJECT", "Next.js 未生成完整的静态导出文件")
                .with_hint("原目录未改动，请查看构建错误后重试"),
        );
    }
    ensure_empty_project_dir(root)?;
    if root.exists() {
        std::fs::remove_dir(root)?;
    }
    std::fs::rename(project, root).map_err(|e| AppError::io("写入完整 Next.js 项目", e))?;
    Ok(())
}

fn composer_project_ready(root: &std::path::Path, package: &str) -> bool {
    let dependency = match package {
        "laravel/laravel" => "laravel/framework",
        "topthink/think" => "topthink/framework",
        "symfony/skeleton" => "symfony/framework-bundle",
        "codeigniter4/appstarter" => "codeigniter4/framework",
        _ => return false,
    };
    root.join("public/index.php").is_file()
        && root.join("vendor/autoload.php").is_file()
        && std::fs::read_to_string(root.join("composer.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            // Flex 会重写项目清单，用户也可以修改包名；用框架依赖识别项目。
            .is_some_and(|json| {
                json.get("require")
                    .and_then(|requires| requires.get(dependency))
                    .and_then(|version| version.as_str())
                    .is_some()
            })
}

fn scaffold_composer(
    root: &std::path::Path,
    input: &CreateSiteInput,
    package: &str,
    paths: &Paths,
    store: &Store,
    progress: &dyn Fn(&str, Option<u8>),
) -> Result<()> {
    validate_template_php(
        &input.template,
        input.runtime.php_version.as_deref().unwrap_or_default(),
    )?;
    let php = ProjectPhp::resolve(
        paths,
        store,
        input.runtime.php_version.as_deref().unwrap_or_default(),
    )?;
    let timeout = std::time::Duration::from_secs(600);
    if composer_project_ready(root, package) {
        // 站点启动失败后的重试可复用已安装项目，但换 PHP 后仍须验证依赖兼容性。
        progress("validating", None);
        run_project_command(
            php.composer_command(root).arg("check-platform-reqs"),
            "检查项目运行环境",
            timeout,
        )?;
        return Ok(());
    }
    ensure_empty_project_dir(root)?;
    let parent = root
        .parent()
        .filter(|_| root.file_name().is_some())
        .ok_or_else(|| AppError::new("BAD_ROOT_DIR", "请选择独立项目目录"))?;
    std::fs::create_dir_all(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".niceenv-composer-")
        .tempdir_in(parent)?;
    let project = staging.path().join("project");
    progress("installing", None);
    // 不执行脚手架自带的迁移/build 脚本；后续仅初始化框架运行所必需的项目文件。
    run_project_command(
        php.composer_command(staging.path())
            .args([
                "create-project",
                "--prefer-dist",
                "--no-scripts",
                "--remove-vcs",
                package,
            ])
            .arg(&project),
        "安装项目及 Composer 依赖",
        timeout,
    )?;
    progress("initializing", None);
    initialize_php_project(&project, input, store)?;
    match input.template.as_str() {
        "laravel" => {
            run_project_command(
                php.command(&project)
                    .args(["artisan", "package:discover", "--no-interaction"]),
                "初始化 Laravel",
                timeout,
            )?;
        }
        "thinkphp" => {
            for command in ["service:discover", "vendor:publish"] {
                run_project_command(
                    php.command(&project)
                        .args(["think", command, "--no-interaction"]),
                    "初始化 ThinkPHP",
                    timeout,
                )?;
            }
        }
        _ => {}
    }
    progress("validating", None);
    run_project_command(
        php.composer_command(&project).arg("check-platform-reqs"),
        "检查项目运行环境",
        timeout,
    )?;
    if !composer_project_ready(&project, package) {
        return Err(
            AppError::new("INCOMPLETE_PROJECT", "项目缺少框架入口或自动加载文件")
                .with_hint("安装未完成，原项目目录中的文件未改动，请检查错误详情后重试")
                .with_detail(format!(
                    "Template: {package}\npublic/index.php: {}\nvendor/autoload.php: {}\ncomposer.json: {}\nThe manifest must require the selected framework.",
                    project.join("public/index.php").is_file(),
                    project.join("vendor/autoload.php").is_file(),
                    project.join("composer.json").is_file(),
                )),
        );
    }
    ensure_empty_project_dir(root)?;
    if root.exists() {
        std::fs::remove_dir(root)?;
    }
    std::fs::rename(project, root).map_err(|e| AppError::io("写入完整项目", e))?;
    Ok(())
}

fn initialize_php_project(
    root: &std::path::Path,
    input: &CreateSiteInput,
    store: &Store,
) -> Result<()> {
    use base64::Engine;
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut secret)
        .map_err(|e| AppError::internal("生成项目密钥", e.to_string()))?;
    let ports = PortsProfile::from_settings(store);
    let domain = input
        .domains
        .iter()
        .find(|d| !d.starts_with("*."))
        .or_else(|| input.domains.first())
        .ok_or_else(|| AppError::new("BAD_DOMAIN", "缺少站点域名"))?
        .replacen("*.", "www.", 1);
    let port = match (input.runtime.web_server.as_str(), input.https) {
        ("apache", true) => ports.apache_https,
        ("apache", false) => ports.apache_http,
        (_, true) => ports.https,
        (_, false) => ports.http,
    };
    let scheme = if input.https { "https" } else { "http" };
    let suffix = if (input.https && port == 443) || (!input.https && port == 80) {
        String::new()
    } else {
        format!(":{port}")
    };
    let url = format!("{scheme}://{domain}{suffix}");
    let mut changes: Vec<(String, String)> = Vec::new();
    let sample = match input.template.as_str() {
        "laravel" => {
            changes.extend([
                (
                    "APP_KEY".into(),
                    format!(
                        "base64:{}",
                        base64::engine::general_purpose::STANDARD.encode(secret)
                    ),
                ),
                ("APP_URL".into(), url),
                ("SESSION_DRIVER".into(), "file".into()),
                ("CACHE_STORE".into(), "file".into()),
                ("CACHE_DRIVER".into(), "file".into()),
                ("QUEUE_CONNECTION".into(), "sync".into()),
            ]);
            ".env.example"
        }
        "thinkphp" => {
            // 官方首页嵌入远端 iframe；本地项目需要离线也能确认框架已运行。
            std::fs::create_dir_all(root.join("route"))?;
            write_scaffold_file(root.join("route/niceenv.php"), "<?php\nuse think\\facade\\Route;\nRoute::get('/', function () {\n    return '<!doctype html><meta charset=\"utf-8\"><title>ThinkPHP</title><h1>ThinkPHP ' . \\think\\facade\\App::version() . ' is ready</h1><p>Your application is running in NiceEnv.</p>';\n});\n")?;
            ".example.env"
        }
        "symfony" => {
            changes.push(("APP_SECRET".into(), hex::encode(secret)));
            std::fs::create_dir_all(root.join("src/Controller"))?;
            std::fs::create_dir_all(root.join("config/routes"))?;
            write_scaffold_file(root.join("src/Controller/NiceEnvController.php"), "<?php\nnamespace App\\Controller;\nuse Symfony\\Bundle\\FrameworkBundle\\Controller\\AbstractController;\nuse Symfony\\Component\\HttpFoundation\\Response;\nfinal class NiceEnvController extends AbstractController {\n    public function __invoke(): Response {\n        return new Response('<!doctype html><meta charset=\"utf-8\"><title>Symfony</title><h1>Symfony is ready</h1><p>Your application is running in NiceEnv.</p>');\n    }\n}\n")?;
            write_scaffold_file(
                root.join("config/routes/niceenv.yaml"),
                "niceenv_home:\n    path: /\n    controller: App\\Controller\\NiceEnvController\n",
            )?;
            ".env"
        }
        "codeigniter" => {
            changes.extend([
                ("CI_ENVIRONMENT".into(), "development".into()),
                ("app.baseURL".into(), format!("{url}/")),
            ]);
            "env"
        }
        _ => return Ok(()),
    };
    if let Some(db) = &input.create_db {
        changes.extend(crate::envfile::project_db_env_vars(
            root,
            &crate::envfile::DbHint {
                database: db.database.clone(),
                username: db.username.clone(),
                password: db.password.clone(),
                port: ports.mysql,
            },
        )?);
    }
    let original = std::fs::read_to_string(root.join(sample))
        .map_err(|e| AppError::io("读取框架环境配置", e))?;
    // 这里只写本次下载的临时项目，绝不覆盖用户已有项目配置。
    std::fs::write(
        root.join(".env"),
        crate::envfile::apply_project_env_changes(root, &original, &changes)?,
    )?;
    Ok(())
}

/// WordPress 使用官方完整发行包。下载、解压均在目标盘的临时目录完成，
/// 验证成功后才发布；失败不会给用户目录留下半套程序或覆盖已有代码。
fn scaffold_wordpress(root: &std::path::Path, progress: &dyn Fn(&str, Option<u8>)) -> Result<()> {
    if wordpress_ready(root) {
        return Ok(());
    }
    ensure_empty_project_dir(root)?;
    let parent = root
        .parent()
        .filter(|_| root.file_name().is_some())
        .ok_or_else(|| {
            AppError::new(
                "BAD_ROOT_DIR",
                "请选择独立的项目目录，不能直接使用磁盘根目录",
            )
        })?;
    std::fs::create_dir_all(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".niceenv-wordpress-")
        .tempdir_in(parent)?;
    let download_paths = Paths::new(staging.path().to_path_buf());
    std::fs::create_dir_all(download_paths.downloads())?;
    let downloader = crate::download::Downloader::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| AppError::io("准备 WordPress 下载", e))?;
    progress("downloading", None);
    let archive = runtime
        .block_on(downloader.download(
            "wordpress",
            &["https://wordpress.org/latest.zip".to_string()],
            "0",
            0,
            &download_paths,
            &|event| {
                if let crate::Event::DownloadProgress(p) = event {
                    let percent = (p.total > 0)
                        .then(|| ((p.received.saturating_mul(100) / p.total).min(100)) as u8);
                    progress("downloading", percent);
                }
            },
        ))
        .map_err(|e| {
            e.with_hint("WordPress 官方包下载失败；请检查网络后重试，目标目录中的文件未改动")
        })?;
    progress("extracting", None);
    crate::install::extract_zip(&archive, staging.path())?;
    let project = staging.path().join("wordpress");
    if !wordpress_ready(&project) {
        return Err(
            AppError::new("INVALID_WORDPRESS_ARCHIVE", "下载的 WordPress 安装包不完整")
                .with_hint("请检查网络后重试，目标目录中的文件未改动"),
        );
    }
    // 下载期间用户可能往目标目录放入文件，发布前必须重新确认。
    ensure_empty_project_dir(root)?;
    if root.exists() {
        std::fs::remove_dir(root).map_err(|e| AppError::io("准备空项目目录", e))?;
    }
    std::fs::rename(&project, root).map_err(|e| AppError::io("写入 WordPress 项目", e))?;
    Ok(())
}

fn wordpress_ready(root: &std::path::Path) -> bool {
    [
        "index.php",
        "wp-load.php",
        "wp-settings.php",
        "wp-includes/version.php",
        "wp-admin/install.php",
        "wp-config-sample.php",
    ]
    .iter()
    .all(|entry| root.join(entry).is_file())
}

fn ensure_empty_project_dir(root: &std::path::Path) -> Result<()> {
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AppError::io("检查项目目录", e)),
        Ok(meta)
            if meta.is_dir()
                && !meta.file_type().is_symlink()
                && std::fs::read_dir(root)?.next().is_none() =>
        {
            Ok(())
        }
        Ok(_) => Err(AppError::new(
            "PROJECT_DIR_NOT_EMPTY",
            "目标目录中已有文件，无法安装项目模板",
        )
        .with_hint("请选择空目录；如果要接入已有项目，请选择「使用现有目录」")),
    }
}

fn write_wordpress_config(
    root: &std::path::Path,
    db: &crate::model::CreateDbInfo,
    port: u16,
) -> Result<()> {
    // 已有配置属于用户，重试建站不能重新生成密钥或覆盖数据库连接。
    if root.join("wp-config.php").exists() {
        return Ok(());
    }
    let quote = |value: &str| value.replace('\\', "\\\\").replace('\'', "\\'");
    let mut config = format!(
        "<?php\n/** Database connection managed when this project was created. */\ndefine('DB_NAME', '{}');\ndefine('DB_USER', '{}');\ndefine('DB_PASSWORD', '{}');\ndefine('DB_HOST', '127.0.0.1:{}');\ndefine('DB_CHARSET', 'utf8mb4');\ndefine('DB_COLLATE', '');\n",
        quote(&db.database), quote(&db.username), quote(&db.password), port,
    );
    use rand::RngCore;
    for key in [
        "AUTH_KEY",
        "SECURE_AUTH_KEY",
        "LOGGED_IN_KEY",
        "NONCE_KEY",
        "AUTH_SALT",
        "SECURE_AUTH_SALT",
        "LOGGED_IN_SALT",
        "NONCE_SALT",
    ] {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut bytes)
            .map_err(|e| AppError::internal("生成 WordPress 登录密钥", e.to_string()))?;
        config.push_str(&format!("define('{key}', '{}');\n", hex::encode(bytes)));
    }
    config.push_str("\n$table_prefix = 'wp_';\ndefine('WP_DEBUG', false);\nif (!defined('ABSPATH')) { define('ABSPATH', __DIR__ . '/'); }\nrequire_once ABSPATH . 'wp-settings.php';\n");
    write_scaffold_file(root.join("wp-config.php"), &config)
}

pub fn scaffold_template(
    template: &str,
    root: &std::path::Path,
    _input: &CreateSiteInput,
) -> Result<()> {
    match template {
        "laravel" | "thinkphp" | "symfony" | "codeigniter" => {
            return Err(AppError::new(
                "COMPOSER_REQUIRED",
                "框架项目需要通过所选 PHP 和 Composer 安装",
            ));
        }
        "blank-php" => {
            write_scaffold_file(
                root.join("index.php"),
                "<?php\nheader('Content-Type: text/html; charset=utf-8');\necho '<h1>It works!</h1><p>NiceEnv PHP site.</p>';\necho '<p>PHP ' . PHP_VERSION . '</p>';\n",
            )?;
            write_scaffold_file(root.join("phpinfo.php"), "<?php\nphpinfo();\n")?;
        }
        "static" => {
            write_scaffold_file(root.join("index.html"), STATIC_INDEX_HTML)?;
        }
        "wordpress" => {
            scaffold_wordpress(root, &|_, _| {})?;
        }
        "next-export" => {
            return Err(AppError::new(
                "NODE_REQUIRED",
                "Next.js 项目需要通过 Node.js 和 pnpm 创建并导出",
            ));
        }
        "spa" => {
            write_scaffold_file(root.join("index.html"), SPA_INDEX_HTML)?;
        }
        // 接入现有代码只注册站点，不向用户项目补写占位入口。
        "none" | "" => {}
        _ => {
            return Err(AppError::new(
                "BAD_TEMPLATE",
                "不支持此项目模板，请重新选择",
            ))
        }
    }
    Ok(())
}

/* ================= 站点模板内容 ================= */

/// 模板只能补充缺失文件，不能覆盖用户已有的项目入口或配置。
fn write_scaffold_file(path: impl AsRef<std::path::Path>, content: &str) -> Result<()> {
    use std::io::Write;
    let path = path.as_ref();
    if path.is_file() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| AppError::new("BAD_ROOT_DIR", "模板文件目录无效"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(content.as_bytes())?;
    file.as_file().sync_all()?;
    // 整份内容写入成功才发布，且不得替换用户在此期间创建的同名文件。
    match file.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists && path.is_file() => {
            Ok(())
        }
        Err(error) => Err(AppError::io("创建项目模板文件", error.error)),
    }
}

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

const NEXT_EXPORT_CONFIG: &str = r#"import type { NextConfig } from "next";
const nextConfig: NextConfig = {
  output: "export",
  trailingSlash: true,
  images: { unoptimized: true },
};
export default nextConfig;
"#;

const NEXT_EXPORT_LAYOUT: &str = r#"import type { Metadata } from "next";
import type { ReactNode } from "react";
export const metadata: Metadata = {
  title: "My Next.js site",
  description: "A Next.js static website created with NiceEnv",
};
export default function RootLayout({ children }: { children: ReactNode }) {
  return <html lang="en"><body style={{ margin: 0, fontFamily: "system-ui, sans-serif" }}>{children}</body></html>;
}
"#;

const NEXT_EXPORT_PAGE: &str = r##"export default function Home() {
  return (
    <main style={{ minHeight: "100vh", boxSizing: "border-box", padding: "48px 24px", background: "#faf9f7", color: "#292524", display: "grid", placeItems: "center" }}>
      <section style={{ width: "100%", maxWidth: 600 }}>
        <p style={{ color: "#57534e" }}>NICEENV / NEXT.JS</p>
        <h1 style={{ fontSize: "clamp(32px, 6vw, 48px)", lineHeight: 1.15 }}>Your next idea starts here.</h1>
        <p style={{ lineHeight: 1.7 }}>This page is rendered by Next.js and exported as a static website.</p>
        <p style={{ lineHeight: 1.7 }}>Edit <code>src/app/page.tsx</code>, then run <code>pnpm build</code> to publish your changes to this site.</p>
        <a href="https://nextjs.org/docs" style={{ color: "#166534", textUnderlineOffset: 4 }}>Explore the Next.js documentation →</a>
      </section>
    </main>
  );
}
"##;

const NEXT_EXPORT_README: &str = r#"# Next.js 静态导出站点

此项目由官方 create-next-app 生成，使用 TypeScript + App Router + pnpm。
NiceEnv 在首次创建时安装依赖、检查类型并静态导出，完整成功后才写入目标目录。
文档根指向 `out/`，`src/app/` 是可继续开发的真实源码。

## 后续修改

修改 `src/app/page.tsx` 后，在项目根目录执行：

```bash
pnpm install   # 依赖变化后执行
pnpm build     # 重新生成 out/，不需要重启 Web 服务
```

`next.config.ts` 已开启 `output: 'export'`、`trailingSlash: true` 和图片免优化。
静态导出不支持运行时服务端接口、依赖请求的 Server Actions 或动态服务器渲染。
需要这些能力时，使用反向代理站点接入自己的 Next.js 服务。

> 如果你用的是 `pnpm dev` 那种按需渲染的开发方式，请改建为
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
                imported_cert_id: None,
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
            php_overrides: None,
        }
    }

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            // 目录名必须全局唯一：不同测试会用同一个 tag（如两个测试都建
            // "wordpress"），并行执行时一个测试的 Drop 清理会删掉另一个
            // 正在写的目录。pid 只隔离进程，加原子计数才能隔离同进程内的测试。
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let n = SEQ.fetch_add(1, Ordering::Relaxed);
            let p =
                std::env::temp_dir().join(format!("nsb-scaffold-{tag}-{}-{n}", std::process::id()));
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
            ("spa", SiteKind::Static, "index.html"),
        ];
        for (tpl, kind, entry) in cases {
            let t = Tmp::new(tpl);
            scaffold_template(tpl, &t.0, &input(kind.clone())).unwrap();
            assert!(
                t.has(entry),
                "模板 {tpl} 应在 {entry} 生成入口（否则站点 404）"
            );
        }
        for tpl in ["laravel", "thinkphp", "symfony", "codeigniter"] {
            let temp = Tmp::new(tpl);
            assert_eq!(
                scaffold_template(tpl, &temp.0, &input(SiteKind::Php))
                    .unwrap_err()
                    .code,
                "COMPOSER_REQUIRED"
            );
            assert!(!temp.has("public/index.php"), "不能生成假框架入口");
            assert!(validate_template_php(tpl, "7.4.33").is_err());
            assert!(validate_template_php(tpl, "8.4.26").is_ok());
        }
    }

    #[test]
    #[ignore = "downloads the official WordPress package; no services or databases are started"]
    fn wordpress_downloads_complete_distribution() {
        let t = Tmp::new("wpsample");
        scaffold_template("wordpress", &t.0, &input(SiteKind::Php)).unwrap();
        assert!(wordpress_ready(&t.0));
        assert!(
            !t.has("wp-config.php"),
            "未创建数据库时应进入 WordPress 自带配置向导"
        );
        let index = std::fs::read_to_string(t.0.join("index.php")).unwrap();
        assert!(index.contains("wp-blog-header.php"), "必须使用官方程序入口");
        let c = std::fs::read_to_string(t.0.join("wp-config-sample.php")).unwrap();
        // 关键常量必须齐全，否则用户改名后 WordPress 起不来
        for k in [
            "DB_NAME",
            "DB_USER",
            "DB_PASSWORD",
            "DB_HOST",
            "table_prefix",
        ] {
            assert!(c.contains(k), "wp-config 样例缺 {k}");
        }
        // 重试复用完整项目，不能覆盖用户刚编辑过的入口。
        std::fs::write(t.0.join("index.php"), "<?php // edited by user\n").unwrap();
        scaffold_wordpress(&t.0, &|_, _| panic!("重试不应重新下载")).unwrap();
        assert_eq!(
            std::fs::read_to_string(t.0.join("index.php")).unwrap(),
            "<?php // edited by user\n"
        );
    }

    #[test]
    fn next_export_rejects_placeholder_projects() {
        let t = Tmp::new("next-export");
        assert_eq!(
            scaffold_template("next-export", &t.0, &input(SiteKind::Static))
                .unwrap_err()
                .code,
            "NODE_REQUIRED"
        );
        assert!(
            !t.has("out/index.html"),
            "不能用普通 HTML 冒充 Next.js 构建结果"
        );
        std::fs::create_dir_all(t.0.join("out")).unwrap();
        std::fs::write(t.0.join("out/index.html"), STATIC_INDEX_HTML).unwrap();
        assert!(!next_export_ready(&t.0), "旧版占位页不能被判定为完整项目");
    }

    #[test]
    #[ignore = "requires NSB_TEMPLATE_VERIFY_ROOT under .verify-home; downloads runtimes and official project dependencies"]
    fn framework_templates_install_and_verify_real_projects() {
        let base = PathBuf::from(
            std::env::var("NSB_TEMPLATE_VERIFY_ROOT").expect("NSB_TEMPLATE_VERIFY_ROOT"),
        );
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();
        std::fs::create_dir_all(&base).unwrap();
        assert!(base
            .canonicalize()
            .unwrap()
            .starts_with(workspace.join(".verify-home")));
        let paths = Paths::new(base.join("runtime"));
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let installer = crate::install::Installer::bundled();
        let downloader = Arc::new(crate::download::Downloader::new());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let selected = std::env::var("NSB_TEMPLATE").ok();
        if selected.as_deref() == Some("next-export") {
            runtime
                .block_on(installer.install("node@24.21.0", &paths, &store, &downloader, &|_| {}))
                .unwrap();
            let mut older = store.find_installed("node", Some("24.21.0")).unwrap();
            older.version = "9.99.0".into();
            store.upsert_installed(&older).unwrap();
            store.set_setting("activenodeVersion", "").unwrap();
            assert_eq!(
                crate::ops::installed_by_choice(&store, "node")
                    .unwrap()
                    .version,
                "24.21.0"
            );
            store.set_setting("activenodeVersion", "9.99.0").unwrap();
            assert_eq!(
                ProjectNode::resolve(&paths, &store).err().unwrap().code,
                "NODE_VERSION_TOO_OLD"
            );
            store.remove_installed("node", "9.99.0").unwrap();
            store.set_setting("activenodeVersion", "24.21.0").unwrap();
            let staging = tempfile::Builder::new()
                .prefix("next-sources-")
                .tempdir_in(&base)
                .unwrap();
            let project = staging.path().join("project");
            prepare_next_project(&project, staging.path(), &paths, &store, &|stage, _| {
                eprintln!("next-export: {stage}")
            })
            .unwrap();
            assert!(project.join("src/app/page.tsx").is_file());
            assert!(project.join("pnpm-lock.yaml").is_file());
            assert!(
                !project.join("out/index.html").exists(),
                "本验证只安装依赖和检查类型，不执行前端 build"
            );
            let original = std::fs::read(project.join("src/app/page.tsx")).unwrap();
            assert_eq!(
                scaffold_next_export(&project, &paths, &store, &|_, _| {})
                    .unwrap_err()
                    .code,
                "PROJECT_DIR_NOT_EMPTY"
            );
            assert_eq!(
                std::fs::read(project.join("src/app/page.tsx")).unwrap(),
                original
            );
            eprintln!("Next.js official scaffold, dependency installation and TypeScript validation passed; no build executed. Evidence: {}", staging.keep().display());
            return;
        }
        for key in ["php@8.4.26", "composer@2.10.3"] {
            eprintln!("Preparing {key}");
            runtime
                .block_on(installer.install(key, &paths, &store, &downloader, &|_| {}))
                .unwrap();
        }
        for extension in ["zip", "intl"] {
            assert!(
                crate::phpext::set_extension(&paths, "8.4.26", extension, true)
                    .unwrap()
                    .is_empty()
            );
        }
        let php = ProjectPhp::resolve(&paths, &store, "8.4.26").unwrap();
        for tpl in ["laravel", "thinkphp", "symfony", "codeigniter"] {
            if selected.as_deref().is_some_and(|selected| selected != tpl) {
                continue;
            }
            let project = base.join(format!("{tpl}-{}", now_ms()));
            let mut config = input(SiteKind::Php);
            config.template = tpl.into();
            config.runtime.php_version = Some("8.4.26".into());
            config.root_dir = project.to_string_lossy().to_string();
            eprintln!("Installing {tpl} at {}", project.display());
            scaffold_composer(
                &project,
                &config,
                composer_package(tpl).unwrap(),
                &paths,
                &store,
                &|stage, _| eprintln!("{tpl}: {stage}"),
            )
            .unwrap();
            struct HttpProcess {
                child: std::process::Child,
                group: platform::ProcessGroup,
            }
            impl Drop for HttpProcess {
                fn drop(&mut self) {
                    let _ = self.group.terminate(true);
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                }
            }
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            drop(listener);
            let mut command = php.command(&project);
            command
                .args([
                    "-S",
                    &address.to_string(),
                    "-t",
                    "public",
                    "public/index.php",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                unsafe {
                    command.pre_exec(platform::spawn_pre_exec);
                }
            }
            let mut server = HttpProcess {
                child: command.spawn().unwrap(),
                group: platform::ProcessGroup::new().unwrap(),
            };
            server.group.attach(server.child.id()).unwrap();
            let client = reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let started = std::time::Instant::now();
            let response = loop {
                match client
                    .get(format!("http://{address}/"))
                    .header("Host", "t.test")
                    .send()
                {
                    Ok(response) => break response,
                    Err(error) if started.elapsed() < std::time::Duration::from_secs(10) => {
                        assert!(
                            server.child.try_wait().unwrap().is_none(),
                            "PHP exited: {error}"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(error) => panic!("{tpl} HTTP check failed: {error}"),
                }
            };
            let status = response.status();
            let body = response.text().unwrap();
            assert_eq!(
                status.as_u16(),
                200,
                "{tpl}: {}",
                body.chars().take(1800).collect::<String>()
            );
            assert!(
                body.to_lowercase().contains("<html")
                    || body.to_lowercase().contains("<!doctype html"),
                "{tpl} did not render HTML"
            );
            drop(server);
            eprintln!("{tpl}: HTTP 200 with real framework homepage");
            let original = std::fs::read(project.join(".env")).unwrap();
            // 改名后的真实项目也必须能重试接入，不能依赖脚手架包名。
            let manifest_path = project.join("composer.json");
            let mut manifest: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
            manifest["name"] = serde_json::json!("local/renamed-project");
            std::fs::write(
                &manifest_path,
                serde_json::to_vec_pretty(&manifest).unwrap(),
            )
            .unwrap();
            scaffold_composer(
                &project,
                &config,
                composer_package(tpl).unwrap(),
                &paths,
                &store,
                &|_, _| {},
            )
            .unwrap();
            assert_eq!(std::fs::read(project.join(".env")).unwrap(), original);
            // 使用各框架自己的解析器验证连接密码，不连接数据库。
            let password = "quoted'\"\\value $dollar # @ / : + &";
            let vars = crate::envfile::project_db_env_vars(
                &project,
                &crate::envfile::DbHint {
                    database: "project_verify".into(),
                    username: "project_user".into(),
                    password: password.into(),
                    port: 23306,
                },
            )
            .unwrap();
            let env_text = crate::envfile::apply_project_env_changes(
                &project,
                &String::from_utf8(original.clone()).unwrap(),
                &vars,
            )
            .unwrap();
            std::fs::write(project.join(".env"), env_text).unwrap();
            let parser = match tpl {
                "laravel" => "require 'vendor/autoload.php'; $v=Dotenv\\Dotenv::parse(file_get_contents('.env')); echo json_encode($v['DB_PASSWORD']);",
                "thinkphp" => "require 'vendor/autoload.php'; $v=new think\\Env(); $v->load('.env'); echo json_encode($v->get('DB_PASS'));",
                "symfony" => "require 'vendor/autoload.php'; $v=(new Symfony\\Component\\Dotenv\\Dotenv())->parse(file_get_contents('.env')); echo json_encode(rawurldecode(parse_url($v['DATABASE_URL'], PHP_URL_PASS)));",
                _ => "require 'vendor/autoload.php'; $v=(new CodeIgniter\\Config\\DotEnv(getcwd()))->parse(); echo json_encode($v['database.default.password']);",
            };
            let parsed = run_project_command(
                php.command(&project).args(["-r", parser]),
                "验证框架环境配置",
                std::time::Duration::from_secs(30),
            )
            .unwrap();
            assert_eq!(
                serde_json::from_str::<String>(&parsed).unwrap(),
                password,
                "{tpl}"
            );
            std::fs::write(project.join(".env"), original).unwrap();
            eprintln!("{tpl}: database credentials parsed correctly by framework");
        }
        let timeout = run_project_command(
            php.command(&base).args(["-r", "sleep(10);"]),
            "超时回收验证",
            std::time::Duration::from_millis(150),
        )
        .unwrap_err();
        assert_eq!(timeout.code, "PROJECT_INSTALL_TIMEOUT");
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
        write_scaffold_file(t.0.join("public/index.php"), "<?php // template\n").unwrap();
        let c = std::fs::read_to_string(t.0.join("public/index.php")).unwrap();
        assert_eq!(c, "<?php // user code\n", "不该覆盖用户已有入口");
        assert_eq!(
            scaffold_wordpress(&t.0, &|_, _| {}).unwrap_err().code,
            "PROJECT_DIR_NOT_EMPTY"
        );
        let db = crate::model::CreateDbInfo {
            database: "wordpress_local".into(),
            username: "wordpress_user".into(),
            password: "quoted'and\\backslash".into(),
        };
        write_wordpress_config(&t.0, &db, 23306).unwrap();
        let config = std::fs::read_to_string(t.0.join("wp-config.php")).unwrap();
        assert!(config.contains("127.0.0.1:23306"));
        assert!(config.contains("quoted\\'and\\\\backslash"));
        for key in [
            "AUTH_KEY",
            "SECURE_AUTH_KEY",
            "LOGGED_IN_KEY",
            "NONCE_KEY",
            "AUTH_SALT",
            "SECURE_AUTH_SALT",
            "LOGGED_IN_SALT",
            "NONCE_SALT",
        ] {
            assert!(config.contains(&format!("define('{key}', '")));
        }
        write_wordpress_config(&t.0, &db, 3306).unwrap();
        assert_eq!(
            std::fs::read_to_string(t.0.join("wp-config.php")).unwrap(),
            config
        );
    }

    #[test]
    fn none_template_writes_nothing_to_existing_dir() {
        // "none" 表示接入现有代码，即便没有 PHP 入口也不能修改项目。
        let t = Tmp::new("none");
        std::fs::write(t.0.join("composer.json"), "{}").unwrap();
        scaffold_template("none", &t.0, &input(SiteKind::Php)).unwrap();
        assert!(!t.has("index.php"));
        std::fs::write(t.0.join("index.php"), "<?php // mine\n").unwrap();
        scaffold_template("none", &t.0, &input(SiteKind::Php)).unwrap();
        let c = std::fs::read_to_string(t.0.join("index.php")).unwrap();
        assert_eq!(c, "<?php // mine\n");
    }

    #[test]
    fn unknown_template_is_rejected_without_writing_files() {
        let t = Tmp::new("unknown");
        assert_eq!(
            scaffold_template("totally-made-up", &t.0, &input(SiteKind::Php))
                .unwrap_err()
                .code,
            "BAD_TEMPLATE"
        );
        assert!(std::fs::read_dir(&t.0).unwrap().next().is_none());
    }

    fn saved_site(paths: &Paths, store: &Store) -> Site {
        let site = Site {
            id: "site-lifecycle".into(),
            name: "Lifecycle".into(),
            domains: vec!["lifecycle.test".into()],
            root_dir: paths.base.to_string_lossy().into(),
            runtime: input(SiteKind::Static).runtime,
            https: false,
            rewrite: crate::model::RewritePreset::None,
            db: None,
            status: "stopped".into(),
            php_overrides: None,
            created_at: 1,
            updated_at: 1,
        };
        store.save_site(&site).unwrap();
        site
    }

    #[test]
    fn delete_site_preserves_project_alias_certificate_and_retained_hosts() {
        let temp = Tmp::new("delete-site");
        let paths = Paths::new(temp.0.clone());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let mut site = saved_site(&paths, &store);
        site.domains.push("alias.test".into());
        store.save_site(&site).unwrap();
        let cert = crate::tls::issue_site_cert(&paths, &store, &site.domains).unwrap();
        let alias = crate::tls::issue_site_cert(&paths, &store, &["alias.test".into()]).unwrap();
        let project = paths.base.join("index.php");
        std::fs::write(&project, "project stays").unwrap();
        write_site_conf_state(&paths, &store, &site, false).unwrap();
        delete(&site.id, false, true, &paths, &store, &Arc::new(ServiceManager::new())).unwrap();
        assert!(store.list_sites().unwrap().is_empty());
        assert!(!paths.nginx_sites_dir().join(format!("{}.conf.disabled", site.id)).exists());
        assert_eq!(std::fs::read_to_string(project).unwrap(), "project stays");
        assert!(!std::path::Path::new(&cert.cert_path).exists());
        assert!(!std::path::Path::new(cert.key_path.as_ref().unwrap()).exists());
        assert!(std::path::Path::new(&alias.cert_path).is_file());
        assert_eq!(store.list_certs().unwrap().len(), 1);
        let retained = crate::hosts::managed_entries(&store).unwrap();
        assert!(site.domains.iter().all(|domain| retained.contains(&("127.0.0.1".into(), domain.clone()))));
    }

    #[test]
    fn delete_site_restores_moved_config_on_file_failure() {
        let temp = Tmp::new("delete-file-failure");
        let paths = Paths::new(temp.0.clone());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let site = saved_site(&paths, &store);
        let config = paths.nginx_sites_dir().join(format!("{}.conf", site.id));
        std::fs::write(&config, "original config").unwrap();
        // 第一个文件已经移动后，后续配置路径不是普通文件，必须恢复已移动的文件。
        std::fs::create_dir(paths.apache_sites_dir().join(format!("{}.conf", site.id))).unwrap();
        let error = delete(&site.id, false, false, &paths, &store, &Arc::new(ServiceManager::new())).unwrap_err();
        assert_eq!(error.code, "SITE_FILE_INVALID");
        assert_eq!(std::fs::read_to_string(config).unwrap(), "original config");
        assert_eq!(get(&store, &site.id).unwrap().domains, site.domains);
        assert!(crate::hosts::extra_entries(&store).unwrap().is_empty());
    }

    #[test]
    fn delete_site_restores_records_certificates_and_hosts_after_config_failure() {
        let temp = Tmp::new("delete-config-failure");
        let paths = Paths::new(temp.0.clone());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let site = saved_site(&paths, &store);
        let cert = crate::tls::issue_site_cert(&paths, &store, &site.domains).unwrap();
        let original_cert = std::fs::read(&cert.cert_path).unwrap();
        write_site_conf_state(&paths, &store, &site, false).unwrap();
        let runtime = paths.runtime_dir("nginx", "1.0");
        let root = runtime.join("nginx-1.0");
        std::fs::create_dir_all(&root).unwrap();
        // 只让入口解析命中；服务未注册，不会执行此文件或启动任何服务。
        std::fs::write(root.join(if cfg!(windows) { "nginx.exe" } else { "nginx" }), b"fixture").unwrap();
        store.upsert_installed(&crate::model::InstalledPackage {
            id: "nginx".into(), version: "1.0".into(), category: "web-server".into(),
            install_path: runtime.to_string_lossy().into(), config_path: String::new(), installed_at: 1,
        }).unwrap();
        std::fs::create_dir(paths.nginx_conf()).unwrap();
        let error = delete(&site.id, false, true, &paths, &store, &Arc::new(ServiceManager::new())).unwrap_err();
        assert_eq!(error.code, "SITE_DELETE_ROLLBACK_FAILED");
        assert!(error.detail.unwrap().contains("恢复 Web 配置"));
        assert_eq!(get(&store, &site.id).unwrap().domains, site.domains);
        assert!(paths.nginx_sites_dir().join(format!("{}.conf.disabled", site.id)).is_file());
        assert_eq!(std::fs::read(&cert.cert_path).unwrap(), original_cert);
        assert!(store.list_certs().unwrap().iter().any(|c| c.id == cert.id));
        assert!(crate::hosts::extra_entries(&store).unwrap().is_empty());
    }

    #[test]
    fn delete_site_keeps_imported_acme_and_shared_local_certificates() {
        for mode in ["imported", "acme", "shared"] {
            let temp = Tmp::new(&format!("delete-keeps-{mode}"));
            let paths = Paths::new(temp.0.clone());
            paths.ensure_dirs().unwrap();
            let store = Store::open(paths.db()).unwrap();
            let mut site = saved_site(&paths, &store);
            let mut cert = crate::tls::issue_site_cert(&paths, &store, &site.domains).unwrap();
            match mode {
                "imported" => {
                    site.runtime.imported_cert_id = Some("external".into());
                    store.save_site(&site).unwrap();
                }
                "acme" => {
                    store.delete_cert(&cert.id).unwrap();
                    cert.kind = "acme".into();
                    store.save_cert(&cert).unwrap();
                }
                _ => {
                    let mut other = site.clone();
                    other.id = "other-site".into();
                    store.save_site(&other).unwrap();
                }
            }
            delete(&site.id, false, true, &paths, &store, &Arc::new(ServiceManager::new())).unwrap();
            assert!(std::path::Path::new(&cert.cert_path).is_file(), "{mode}");
            assert!(store.list_certs().unwrap().iter().any(|c| c.id == cert.id), "{mode}");
        }
    }

    #[test]
    fn site_validation_rejects_conflicts_and_config_injection_before_writing() {
        let temp = Tmp::new("validation");
        let paths = Paths::new(temp.0.clone());
        let store = Store::open(paths.db()).unwrap();
        let site = saved_site(&paths, &store);
        for domains in [
            vec![],
            vec!["bad.test;\nserver {}".into()],
            vec!["-bad.test".into()],
        ] {
            assert!(validate_site_fields(
                &site.name,
                &domains,
                &site.root_dir,
                &site.runtime,
                &store,
                Some(&site.id)
            )
            .is_err());
        }
        let conflict = validate_site_fields(
            &site.name,
            &["LIFECYCLE.TEST".into()],
            &site.root_dir,
            &site.runtime,
            &store,
            None,
        )
        .unwrap_err();
        assert_eq!(conflict.code, "DOMAIN_CONFLICT");
        assert!(validate_site_fields(
            "bad\nname",
            &site.domains,
            &site.root_dir,
            &site.runtime,
            &store,
            Some(&site.id)
        )
        .is_err());
        assert!(validate_site_fields(
            &site.name,
            &["*.demo.test".into()],
            &site.root_dir,
            &site.runtime,
            &store,
            Some(&site.id)
        )
        .is_ok());
        let mut invalid = site.clone();
        invalid.domains.clear();
        assert!(update(&invalid, &paths, &store, &Arc::new(ServiceManager::new())).is_err());
        assert_eq!(get(&store, &site.id).unwrap().domains, site.domains);
    }

    #[test]
    fn saving_disabled_site_keeps_it_disabled_and_removes_old_server_config() {
        let temp = Tmp::new("disabled-update");
        let paths = Paths::new(temp.0.clone());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let mut site = saved_site(&paths, &store);
        write_site_conf_state(&paths, &store, &site, false).unwrap();
        site.name = "Edited".into();
        let manager = Arc::new(ServiceManager::new());
        let updated = update(&site, &paths, &store, &manager).unwrap();
        assert_eq!(updated.status, "stopped");
        assert!(!paths.nginx_sites_dir().join("site-lifecycle.conf").exists());
        assert!(paths
            .nginx_sites_dir()
            .join("site-lifecycle.conf.disabled")
            .exists());
        site.runtime.web_server = "apache".into();
        update(&site, &paths, &store, &manager).unwrap();
        assert!(!paths
            .nginx_sites_dir()
            .join("site-lifecycle.conf.disabled")
            .exists());
        assert!(paths
            .apache_sites_dir()
            .join("site-lifecycle.conf.disabled")
            .exists());
    }

    #[test]
    fn enabled_config_requires_live_dependencies_and_bulk_start_reports_failure() {
        let temp = Tmp::new("runtime-state");
        let paths = Paths::new(temp.0.clone());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let mut site = saved_site(&paths, &store);
        write_site_conf(&paths, &store, &site).unwrap();
        let manager = Arc::new(ServiceManager::new());
        assert_eq!(runtime_status(&paths, &site, &manager), "stopped");
        let report = start_many(&paths, &store, &manager, &[site.id.clone()]).unwrap();
        assert!(report.succeeded.is_empty());
        assert!(report.already.is_empty());
        assert_eq!(report.failed.len(), 1);
        manager.register(
            "nginx",
            "Nginx",
            None,
            None,
            None,
            paths.service_log("nginx"),
        );
        manager.set_state("nginx", ServiceState::Running);
        // 仅把状态标记为 Running 但没有真实 PID 时，snapshot 会回写为 Stopped。
        assert_eq!(runtime_status(&paths, &site, &manager), "stopped");
        site.runtime.kind = SiteKind::Php;
        site.runtime.php_version = Some("8.3.33".into());
        assert_eq!(runtime_status(&paths, &site, &manager), "stopped");
        manager.register(
            "php@8.3.33",
            "PHP",
            None,
            None,
            None,
            paths.service_log("php"),
        );
        manager.set_state("php@8.3.33", ServiceState::Error);
        assert_eq!(runtime_status(&paths, &site, &manager), "error");
    }

    #[test]
    fn proxy_urls_support_https_ipv6_and_reject_unsafe_configuration() {
        assert_eq!(
            proxy_url("127.0.0.1:3000").unwrap(),
            "http://127.0.0.1:3000/"
        );
        assert_eq!(
            proxy_url("https://api.example.com/v1/").unwrap(),
            "https://api.example.com/v1/"
        );
        assert_eq!(
            proxy_url("http://[::1]:3000").unwrap(),
            "http://[::1]:3000/"
        );
        for invalid in [
            "",
            "http://",
            "ftp://example.com",
            "http://a.test;include",
            "http://u:p@a.test",
            "http://a.test/?x=1",
        ] {
            assert!(proxy_url(invalid).is_err(), "{invalid}");
        }
        assert!(proxy_url("http://a.\ntest").is_err());
    }

    #[test]
    fn https_proxy_and_wildcard_cert_paths_match_generated_files() {
        let temp = Tmp::new("https-render");
        let paths = Paths::new(temp.0.clone());
        let store = Store::open(paths.db()).unwrap();
        let mut site = saved_site(&paths, &store);
        site.runtime.kind = SiteKind::ReverseProxy;
        site.runtime.proxy_target = Some("https://api.example.com/v1/".into());
        site.domains = vec!["*.demo.test".into()];
        site.https = true;
        let nginx = configgen::render_site_conf(
            &site,
            8080,
            8443,
            &paths.base.join("fastcgi_params"),
            &paths.certs().join("sites"),
            &paths.base,
        );
        assert!(nginx.contains("proxy_pass https://api.example.com/v1/;"));
        assert!(!nginx.contains("http://https://"));
        assert!(nginx.contains("_wildcard.demo.test.crt"));
        let apache = configgen::render_httpd_vhost(&site, 8180, 8444, &paths.certs().join("sites"), None);
        assert!(apache.contains("<VirtualHost *:8180>"));
        assert!(apache.contains("<VirtualHost *:8444>"));
        assert!(apache.contains("ProxyPass / \"https://api.example.com/v1/\""));
        assert!(apache.contains("_wildcard.demo.test.crt"));

        site.runtime.imported_cert_id = Some("corp-wildcard".into());
        let imported_nginx = configgen::render_site_conf(
            &site,
            8080,
            8443,
            &paths.base.join("fastcgi_params"),
            &paths.certs().join("sites"),
            &paths.base,
        );
        let imported_apache = configgen::render_httpd_vhost(&site, 8180, 8444, &paths.certs().join("sites"), None);
        assert!(imported_nginx.contains("certs/imported/corp-wildcard.crt"));
        assert!(imported_nginx.contains("certs/imported/corp-wildcard.key"));
        assert!(imported_apache.contains("certs/imported/corp-wildcard.crt"));
        assert!(imported_apache.contains("certs/imported/corp-wildcard.key"));

        site.runtime.imported_cert_id = Some("../outside".into());
        let rejected = configgen::render_site_conf(
            &site,
            8080,
            8443,
            &paths.base.join("fastcgi_params"),
            &paths.certs().join("sites"),
            &paths.base,
        );
        assert!(!rejected.contains("../outside"));
    }

    #[test]
    fn database_binding_roundtrips_without_exposing_password() {
        let binding = crate::model::SiteDbBinding {
            enabled: true,
            database: "app".into(),
            username: "app_user".into(),
            password: "private".into(),
            version: Some("8.0.46".into()),
            port: Some(23306),
        };
        let json = serde_json::to_value(binding).unwrap();
        assert!(json.get("password").is_none());
        let restored: crate::model::SiteDbBinding = serde_json::from_value(json).unwrap();
        assert_eq!(restored.database, "app");
        assert_eq!(restored.version.as_deref(), Some("8.0.46"));
        assert_eq!(restored.port, Some(23306));
        assert!(restored.password.is_empty());
        let temp = Tmp::new("db-binding");
        let paths = Paths::new(temp.0.clone());
        let store = Store::open(paths.db()).unwrap();
        let mut site = saved_site(&paths, &store);
        site.db = Some(crate::model::SiteDbBinding {
            password: "local-only".into(),
            ..restored
        });
        store.save_site(&site).unwrap();
        let loaded = get(&store, &site.id).unwrap();
        assert_eq!(loaded.db.as_ref().unwrap().password, "local-only");
        store.set_port_override("mysql", Some(29999)).unwrap();
        assert_eq!(crate::envfile::read_env(&paths, &store, &site.id).unwrap().db_hint.unwrap().port, 23306);
        store.set_setting(&crate::dbadmin::port_key("8.0.46"), "23307").unwrap();
        assert_eq!(crate::envfile::read_env(&paths, &store, &site.id).unwrap().db_hint.unwrap().port, 23307);
        assert!(serde_json::to_value(loaded).unwrap()["db"]
            .get("password")
            .is_none());
    }

    #[test]
    #[ignore = "requires NSB_NGINX_ROOT; checks config and briefly serves isolated static route fixtures"]
    fn nginx_accepts_configs_and_serves_static_routes() {
        let nginx_root = PathBuf::from(std::env::var("NSB_NGINX_ROOT").expect("NSB_NGINX_ROOT"));
        let temp = tempfile::Builder::new()
            .prefix("niceenv config with spaces ")
            .tempdir()
            .unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let mut site = saved_site(&paths, &store);
        site.domains = vec!["*.demo.test".into()];
        site.https = true;
        crate::tls::issue_site_cert(&paths, &store, &site.domains).unwrap();
        let pools = vec![("8.3.33".to_string(), 29100)];
        configgen::write_nginx_conf(&paths, &nginx_root, &pools, 28080, 28443).unwrap();
        for (kind, rewrite) in [
            (SiteKind::Static, crate::model::RewritePreset::None),
            (SiteKind::Static, crate::model::RewritePreset::NextExport),
            (SiteKind::Static, crate::model::RewritePreset::SpaFallback),
            (SiteKind::Php, crate::model::RewritePreset::Laravel),
            (SiteKind::ReverseProxy, crate::model::RewritePreset::None),
        ] {
            site.runtime.kind = kind;
            site.rewrite = rewrite;
            site.runtime.php_version = Some("8.3.33".into());
            site.runtime.proxy_target = Some("https://127.0.0.1:24443/api/".into());
            write_site_conf(&paths, &store, &site).unwrap();
            let binary = nginx_root.join(if cfg!(windows) { "nginx.exe" } else { "nginx" });
            let output = platform::command(binary)
                .arg("-t")
                .arg("-e")
                .arg("stderr")
                .arg("-p")
                .arg(temp.path())
                .arg("-c")
                .arg(paths.nginx_conf())
                .current_dir(temp.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // 使用本次生成的规则请求真实 Nginx；静态路由样例不依赖前端 build。
        let public = temp.path().join("static routes");
        std::fs::create_dir_all(public.join("guide")).unwrap();
        std::fs::create_dir_all(public.join("_next")).unwrap();
        std::fs::create_dir_all(temp.path().join("temp")).unwrap();
        for (path, body) in [
            ("index.html", "route-home"),
            ("about.html", "route-about"),
            ("guide/index.html", "route-guide"),
            ("404.html", "route-not-found"),
            ("_next/app.js", "route-asset"),
        ] {
            std::fs::write(public.join(path), body).unwrap();
        }
        site.root_dir = public.to_string_lossy().to_string();
        site.runtime.kind = SiteKind::Static;
        site.rewrite = crate::model::RewritePreset::NextExport;
        site.https = false;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let vhost = configgen::render_site_conf(
            &site,
            port,
            0,
            &paths.nginx_conf(),
            &paths.certs(),
            &paths.logs().join("nginx"),
        );
        let conf =
            format!("pid nginx.pid;\nerror_log stderr;\nevents {{}}\nhttp {{\n{vhost}\n}}\n");
        std::fs::write(paths.nginx_conf(), conf).unwrap();
        struct Server {
            child: std::process::Child,
            group: platform::ProcessGroup,
        }
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.group.terminate(true);
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        let binary = nginx_root.join(if cfg!(windows) { "nginx.exe" } else { "nginx" });
        let mut command = platform::command(binary);
        command
            .args(["-e", "stderr", "-g", "daemon off;"])
            .arg("-p")
            .arg(temp.path())
            .arg("-c")
            .arg(paths.nginx_conf())
            .current_dir(temp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                command.pre_exec(platform::spawn_pre_exec);
            }
        }
        drop(listener);
        let mut server = Server {
            child: command.spawn().unwrap(),
            group: platform::ProcessGroup::new().unwrap(),
        };
        server.group.attach(server.child.id()).unwrap();
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        for (path, status, body) in [
            ("/", 200, "route-home"),
            ("/about", 200, "route-about"),
            ("/guide/", 200, "route-guide"),
            ("/_next/app.js", 200, "route-asset"),
            ("/missing", 404, "route-not-found"),
        ] {
            let start = std::time::Instant::now();
            let response = loop {
                match client
                    .get(format!("http://127.0.0.1:{port}{path}"))
                    .header("Host", "routes.demo.test")
                    .send()
                {
                    Ok(response) => break response,
                    Err(error) if start.elapsed() < std::time::Duration::from_secs(5) => {
                        assert!(
                            server.child.try_wait().unwrap().is_none(),
                            "Nginx exited: {error}"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(error) => panic!("Nginx route {path}: {error}"),
                }
            };
            assert_eq!(response.status().as_u16(), status, "{path}");
            assert_eq!(response.text().unwrap(), body, "{path}");
        }
        drop(server);
    }
}

/* ================= 批量站点操作 ================= */

/// 批量启停的结果（与服务的 BulkReport 同形，便于前端复用展示）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteBulkReport {
    /// start / stop
    pub action: String,
    pub succeeded: Vec<String>,
    /// 本来就在目标状态
    pub already: Vec<String>,
    pub failed: Vec<SiteBulkFailure>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteBulkFailure {
    pub site_id: String,
    pub error: crate::model::AppErrorInfo,
}

/// 批量启动站点。
///
/// 与单站点启动共用依赖启动、配置校验和失败恢复，不能只写配置就报告成功。
pub fn start_many(
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
    ids: &[String],
) -> Result<SiteBulkReport> {
    let mut report = SiteBulkReport {
        action: "start".into(),
        succeeded: Vec::new(),
        already: Vec::new(),
        failed: Vec::new(),
    };
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            continue;
        }
        let site = match get(store, id) {
            Ok(s) => s,
            Err(e) => {
                report.failed.push(SiteBulkFailure {
                    site_id: id.clone(),
                    error: e.into(),
                });
                continue;
            }
        };
        if runtime_status(paths, &site, manager) == "running" {
            report.already.push(id.clone());
            continue;
        }
        match start_site(id, paths, store, manager) {
            Ok(()) => {
                let mut s = site;
                s.status = "running".into();
                s.updated_at = now_ms();
                if let Err(e) = store.save_site(&s) {
                    report.failed.push(SiteBulkFailure {
                        site_id: id.clone(),
                        error: e.into(),
                    });
                } else {
                    report.succeeded.push(id.clone());
                }
            }
            Err(e) => report.failed.push(SiteBulkFailure {
                site_id: id.clone(),
                error: e.into(),
            }),
        }
    }

    Ok(report)
}

/// 批量停止站点：把 vhost 全部禁用后**只 reload 一次**
pub fn stop_many(
    paths: &Paths,
    store: &Store,
    manager: &Arc<ServiceManager>,
    ids: &[String],
) -> Result<SiteBulkReport> {
    let _change = SITE_CHANGES.lock();
    let mut report = SiteBulkReport {
        action: "stop".into(),
        succeeded: Vec::new(),
        already: Vec::new(),
        failed: Vec::new(),
    };
    let mut dirty = false;

    for id in ids {
        let site = match get(store, id) {
            Ok(s) => s,
            Err(e) => {
                report.failed.push(SiteBulkFailure {
                    site_id: id.clone(),
                    error: e.into(),
                });
                continue;
            }
        };
        if derive_status(paths, &site) != "running" {
            report.already.push(id.clone());
            continue;
        }
        match disable_site_conf(paths, &site) {
            Ok(()) => {
                let mut s = site;
                s.status = "stopped".into();
                s.updated_at = now_ms();
                if let Err(e) = store.save_site(&s) {
                    report.failed.push(SiteBulkFailure {
                        site_id: id.clone(),
                        error: e.into(),
                    });
                } else {
                    report.succeeded.push(id.clone());
                    dirty = true;
                }
            }
            Err(e) => report.failed.push(SiteBulkFailure {
                site_id: id.clone(),
                error: e.into(),
            }),
        }
    }

    if dirty {
        crate::ops::rebuild_and_reload(store, paths, manager)?;
    }
    Ok(report)
}

/// 禁用 vhost（不做 reload）
fn disable_site_conf(paths: &Paths, site: &Site) -> Result<()> {
    let web_server = if site.runtime.web_server == "apache" {
        "apache"
    } else {
        "nginx"
    };
    let dir = if web_server == "apache" {
        paths.apache_sites_dir()
    } else {
        paths.nginx_sites_dir()
    };
    let conf = dir.join(format!("{}.conf", site.id));
    if conf.exists() {
        std::fs::rename(&conf, conf.with_extension("conf.disabled"))?;
    }
    let other_dir = if web_server == "apache" {
        paths.nginx_sites_dir()
    } else {
        paths.apache_sites_dir()
    };
    let other = other_dir.join(format!("{}.conf", site.id));
    if other.exists() {
        let _ = std::fs::rename(&other, other.with_extension("conf.disabled"));
    }
    Ok(())
}

/// 站点级 PHP 覆盖 → rootDir/.user.ini（PHP 默认的用户级 ini 文件名）。
/// 仅 php 站点写；键值做白名单字符校验，防止换行注入任意 ini 指令。
pub fn write_user_ini(site: &Site) {
    if site.runtime.kind != crate::model::SiteKind::Php {
        return;
    }
    let Some(overrides) = &site.php_overrides else {
        return;
    };
    let root = std::path::PathBuf::from(&site.root_dir);
    if !root.is_dir() {
        return;
    }
    let mut body = String::from("; NiceEnv managed .user.ini\n");
    for (k, v) in overrides {
        let k = k.trim();
        let v = v.trim();
        if k.is_empty() {
            continue;
        }
        // 键只允许 ini 键字符；值不允许换行（防注入第二条指令）
        if !k
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            continue;
        }
        let v = v.replace('\n', " ").replace('\r', " ");
        body.push_str(&format!("{k}={v}\n"));
    }
    std::fs::write(root.join(".user.ini"), body).ok();
}

/// 读取项目运行时锁定文件 {rootDir}/.nsb.json。
/// 目前支持 {"php": "x.y.z"}；返回 (kind, version)。文件损坏/字段非法 → None。
pub fn read_project_pin(root_dir: &str) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(std::path::Path::new(root_dir).join(".nsb.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let ver = v.get("php")?.as_str()?.trim().to_string();
    // 版本串防注入：只允许数字与点
    if ver.is_empty() || !ver.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    Some(("php".to_string(), ver))
}
