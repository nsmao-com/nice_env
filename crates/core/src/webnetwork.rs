//! Web 站点局域网访问：显式选择、真实监听核对、同服务器重启与失败恢复。
use crate::{error::{AppError, Result}, model::ServiceState, paths::Paths, services::ServiceManager, store::Store};
use std::{net::{IpAddr, Ipv4Addr, SocketAddr}, path::PathBuf, sync::Arc};

fn server_key(server: &str) -> Result<String> {
    if !matches!(server, "nginx" | "apache" | "caddy") {
        return Err(AppError::new("WEB_NETWORK_SERVER", "请选择 Nginx、Apache 或 Caddy"));
    }
    Ok(format!("webLan.{server}"))
}

/// 未设置时保留原来的配置行为；恢复失败操作时 default 等同于未设置。
pub(crate) fn mode(store: &Store, server: &str) -> Result<Option<bool>> {
    match store.get_setting_checked(&server_key(server)?)?.as_deref() {
        None | Some("default") => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        _ => Err(AppError::new("WEB_NETWORK_SETTING", "局域网访问设置无效，请重新保存")),
    }
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkAddress {
    pub interface: String,
    pub address: String,
    pub listening: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SiteNetworkInfo {
    pub server: String,
    pub enabled: bool,
    pub running: bool,
    pub affected_sites: Vec<String>,
    pub addresses: Vec<NetworkAddress>,
    pub access_url: Option<String>,
    pub local_ca: bool,
}

fn interface_addresses() -> Vec<(String, Ipv4Addr)> {
    let networks = sysinfo::Networks::new_with_refreshed_list();
    let mut addresses: Vec<_> = networks.iter().flat_map(|(name, network)| {
        network.ip_networks().iter().filter_map(move |network| match network.addr {
            IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast() => Some((name.clone(), ip)),
            _ => None,
        })
    }).collect();
    addresses.sort_by_key(|(name, ip)| (!ip.is_private(), ip.is_link_local(), name.clone(), *ip));
    addresses.dedup();
    addresses
}

pub fn info(paths: &Paths, store: &Store, manager: &Arc<ServiceManager>, site_id: &str) -> Result<SiteNetworkInfo> {
    let _operation = manager.lifecycle.lock();
    let sites = store.list_sites()?;
    let mut site = sites.iter().find(|site| site.id == site_id).cloned()
        .ok_or_else(|| AppError::new("SITE_NOT_FOUND", "站点不存在，请刷新列表"))?;
    let server = &site.runtime.web_server;
    let enabled = mode(store, server)? == Some(true);
    let status = manager.snapshot(server);
    let running = status.as_ref().is_some_and(|s| s.state == ServiceState::Running);
    site.status = crate::sites::runtime_status(paths, &site, manager).into();
    let endpoint = crate::sites::loaded_endpoint(manager, &site);
    let access_url = crate::sites::running_url(paths, site.clone(), manager).ok().and_then(|address| {
        let mut url = reqwest::Url::parse(&address).ok()?;
        let domain = site.domains.iter().find(|name| !name.starts_with("*.") && !name.eq_ignore_ascii_case("localhost")
            && !name.to_ascii_lowercase().ends_with(".localhost") && name.parse::<IpAddr>().is_err())
            .cloned().or_else(|| site.domains.iter().find(|name| name.starts_with("*.") && !name.to_ascii_lowercase().ends_with(".localhost"))
                .map(|name| name.replacen("*.", "www.", 1)))?;
        url.set_host(Some(&domain)).ok()?;
        Some(url.to_string().trim_end_matches('/').to_owned())
    });
    let mut addresses = Vec::new();
    for (interface, ip) in interface_addresses() {
        let listening = match (&endpoint, &status) {
            (Some(endpoint), Some(status)) if endpoint.lan && access_url.is_some() =>
                crate::ports::owns_interface_listener(SocketAddr::new(ip.into(), endpoint.port), &status.pids)?,
            _ => false,
        };
        addresses.push(NetworkAddress { interface, address: ip.to_string(), listening });
    }
    Ok(SiteNetworkInfo {
        server: server.clone(), enabled, running, addresses, access_url,
        affected_sites: sites.iter().filter(|s| s.runtime.web_server == *server).map(|s| s.name.clone()).collect(),
        local_ca: site.https && site.runtime.acme_cert_id.is_none() && site.runtime.imported_cert_id.is_none(),
    })
}

fn snapshot(paths: &Paths, store: &Store, server: &str, enabled: bool) -> Result<Vec<(PathBuf, Option<Vec<u8>>)>> {
    let main = match server {
        "nginx" => paths.nginx_conf(), "apache" => paths.apache_conf(), _ => crate::caddy::config_path(paths, store)?,
    };
    let mut files = vec![main];
    for site in store.list_sites()?.iter().filter(|site| site.runtime.web_server == server) {
        if site.id.is_empty() || !site.id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte)) {
            return Err(AppError::new("BAD_SITE_ID", "站点标识无效，请检查站点记录"));
        }
        for suffix in ["conf", "conf.disabled"] {
            let path = crate::paths::checked_data_path(&paths.base, &format!("etc/{server}/sites/{}.{suffix}", site.id))?;
            if let Ok(content) = std::fs::read_to_string(&path) {
                match server {
                    "nginx" => {
                        if !content.starts_with("# site: ") || !content.lines().next().is_some_and(|line| line.contains("NiceEnv 托管")) {
                            return Err(AppError::new("WEB_NETWORK_CUSTOM", "站点配置已改为自定义结构，请先在站点详情重新保存后再切换局域网访问"));
                        }
                        crate::configgen::network_listeners(&content, enabled, false)?;
                    },
                    "caddy" => { crate::caddy::network_bind(&content, enabled, None)?; },
                    _ => {},
                }
            }
            files.push(path);
        }
    }
    files.into_iter().map(|path| {
        let relative = path.strip_prefix(&paths.base).map_err(|_| AppError::new("WEB_NETWORK_PATH", "配置路径不在托管目录内"))?;
        let path = crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
        let content = match std::fs::read(&path) {
            Ok(content) => Some(content), Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(AppError::io("读取 Web 配置", e)),
        };
        Ok((path, content))
    }).collect()
}

pub fn apply(paths: &Paths, store: &Store, manager: &Arc<ServiceManager>, site_id: &str, server: &str, enabled: bool) -> Result<()> {
    let _sites = crate::sites::SITE_CHANGES.lock();
    let _operation = manager.lifecycle.lock();
    let key = server_key(server)?;
    let site = store.list_sites()?.into_iter().find(|site| site.id == site_id)
        .ok_or_else(|| AppError::new("SITE_NOT_FOUND", "站点不存在，请刷新列表"))?;
    if site.runtime.web_server != server { return Err(AppError::new("WEB_NETWORK_CHANGED", "站点已切换 Web 服务器，请刷新后重试")); }
    if crate::ops::installed_by_choice(store, server).is_none() { return Err(AppError::not_installed(server)); }
    let old = store.get_setting_checked(&key)?;
    let _ = mode(store, server)?;
    let status = manager.snapshot(server);
    let running = status.is_some_and(|s| s.state == ServiceState::Running);
    if !running && manager.is_busy(server) { return Err(AppError::new("SERVICE_BUSY", "服务尚有运行中的进程，请先处理服务状态再修改局域网访问")); }
    let files = snapshot(paths, store, server, enabled)?;
    store.set_setting(&key, if enabled { "true" } else { "false" })?;
    // 停止的服务只保存选择；下次正常启动会同步配置，不擅自启动 PHP 或应用依赖。
    if !running { return Ok(()); }
    let mut stopped = false;
    let result = (|| {
        crate::ops::stop_service(store, paths, manager, server)?;
        stopped = true;
        crate::ops::start_service(store, paths, manager, server)
    })();
    if let Err(error) = result {
        let mut failures = Vec::new();
        if stopped && manager.is_busy(server) {
            if let Err(e) = crate::ops::stop_service(store, paths, manager, server) { failures.push(e.to_string()); }
        }
        if let Err(e) = store.set_setting(&key, old.as_deref().unwrap_or("default")) { failures.push(e.to_string()); }
        for (path, bytes) in files {
            let restored = match bytes {
                Some(bytes) => crate::paths::write_atomic(&path, &bytes),
                None => match std::fs::remove_file(&path) { Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), result => result },
            };
            if let Err(e) = restored { failures.push(format!("{}: {e}", path.display())); }
        }
        if stopped && failures.is_empty() {
            if let Err(e) = crate::ops::start_service(store, paths, manager, server) { failures.push(e.to_string()); }
        }
        if !failures.is_empty() {
            return Err(AppError::new("WEB_NETWORK_ROLLBACK", "局域网设置未能应用，部分配置或服务恢复失败")
                .with_detail(format!("{}；{}", error, failures.join("；"))));
        }
        return Err(error.with_hint("局域网设置未生效，已恢复原设置；请检查 Web 服务配置与日志后重试。"));
    }
    Ok(())
}

#[cfg(test)]
mod checks {
    use super::*;

    #[test]
    fn listeners_preserve_body_ports_and_custom_main_blocks() {
        let site = "# site: demo NiceEnv 托管\nserver { listen 127.0.0.1:18880; listen 127.0.0.1:18443 ssl; server_name demo.test; location / { return 200 '127.0.0.1:18880'; } }";
        let lan = crate::configgen::network_listeners(site, true, false).unwrap();
        assert!(lan.contains("listen 0.0.0.0:18443 ssl;"));
        assert!(lan.contains("return 200 '127.0.0.1:18880'"));
        assert_eq!(crate::configgen::network_listeners(&lan, false, false).unwrap(), site);
        assert!(crate::configgen::network_listeners("server { listen 192.0.2.1:80; }", true, false).is_err());
        let main = "events {} http { server { listen 127.0.0.1:80; server_name _; } server { listen 127.0.0.1:81; server_name custom.test; } }";
        let updated = crate::configgen::network_listeners(main, true, true).unwrap();
        assert!(updated.contains("listen 0.0.0.0:80;")); assert!(updated.contains("listen 127.0.0.1:81;"));
        let caddy = "# NiceEnv site: demo\nhttp://demo.test:8080 {\n\tbind 127.0.0.1\n\trespond \"127.0.0.1\"\n}\n";
        let lan = crate::caddy::network_bind(caddy, true, None).unwrap();
        assert!(lan.contains("bind 0.0.0.0")); assert!(lan.contains("respond \"127.0.0.1\""));
        assert_eq!(crate::caddy::network_bind(&lan, false, None).unwrap(), caddy);
        assert!(crate::caddy::network_bind(&caddy.replace("bind 127.0.0.1", "bind 192.0.2.1"), true, None).is_err());
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().into()); paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        assert_eq!(mode(&store, "nginx").unwrap(), None);
        store.set_setting("webLan.nginx", "true").unwrap();
        let (bytes, _) = crate::transfer::encode_export(&store).unwrap();
        let mut bundle: crate::transfer::ExportBundle = serde_json::from_slice(&bytes).unwrap();
        assert!(!bundle.settings.iter().any(|(key, _)| key.starts_with("webLan.")));
        store.set_setting("webLan.nginx", "false").unwrap();
        bundle.settings.push(("webLan.nginx".into(), "true".into()));
        let backup = temp.path().join("settings.json"); std::fs::write(&backup, serde_json::to_vec(&bundle).unwrap()).unwrap();
        crate::transfer::import_from(&backup, &paths, &store, &Arc::new(ServiceManager::new())).unwrap();
        assert_eq!(mode(&store, "nginx").unwrap(), Some(false), "import cannot open this computer's LAN listeners");
        let adminer = temp.path().join("adminer.php"); std::fs::write(&adminer, "<?php").unwrap();
        let main = crate::configgen::render_nginx_conf(&paths, temp.path(), 18080, 18443, &[("8.4".into(), 19100)], Some(&adminer));
        assert!(main.contains("allow 127.0.0.1;\n            deny all;"));
    }

    #[test]
    #[ignore = "requires official NSB_NGINX_ROOT, NSB_APACHE_ROOT, NSB_VERIFY_CADDY and NSB_SKIP_HOSTS=1; uses temporary sites only"]
    fn native_lan_lifecycle() {
        assert_eq!(std::env::var("NSB_SKIP_HOSTS").unwrap(), "1");
        let address = interface_addresses().into_iter().find(|(_, ip)| ip.is_private()).expect("a local IPv4 interface is required").1;
        for (server, version, variable, http_key, https_key) in [
            ("nginx", "1.28.1", "NSB_NGINX_ROOT", "http", "https"),
            ("apache", "2.4.66", "NSB_APACHE_ROOT", "apacheHttp", "apacheHttps"),
            ("caddy", "2.11.4", "NSB_VERIFY_CADDY", "caddy", "caddyHttps"),
        ] {
            if std::env::var("NSB_LAN_SERVER").is_ok_and(|value| value != server) { continue; }
            let root = PathBuf::from(std::env::var_os(variable).expect(variable));
            let temp = tempfile::tempdir().unwrap();
            let state = crate::CoreState::init(Some(temp.path().join("LAN settings")), Arc::new(|_| {})).unwrap();
            struct Cleanup(Arc<crate::CoreState>, &'static str);
            impl Drop for Cleanup { fn drop(&mut self) { let _ = self.0.stop_service(self.1); } }
            let _cleanup = Cleanup(state.clone(), server);
            let http = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
            let tls = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
            let port = http.local_addr().unwrap().port(); let tls_port = tls.local_addr().unwrap().port();
            state.store.set_port_override(http_key, Some(port)).unwrap(); state.store.set_port_override(https_key, Some(tls_port)).unwrap();
            state.store.set_setting("autoFallbackPort", "false").unwrap();
            state.store.upsert_installed(&crate::model::InstalledPackage { id: server.into(), version: version.into(), category: "web-server".into(),
                install_path: root.parent().unwrap().to_string_lossy().into(), config_path: String::new(), installed_at: 1 }).unwrap();
            let project = temp.path().join("project"); std::fs::create_dir_all(&project).unwrap(); std::fs::write(project.join("index.html"), "lan-site").unwrap();
            let mut input: crate::model::CreateSiteInput = serde_json::from_value(serde_json::json!({
                "name":"LAN fixture","domains":["lan-fixture.test"],"rootDir":project,"https":true,"rewrite":"none","template":"none","writeEnvExample":false,
                "runtime":{"kind":"static","webServer":server,"httpsRedirect":307}
            })).unwrap();
            drop((http, tls));
            let site = crate::sites::create(&input, &state.paths, &state.store, &state.manager).unwrap();
            let ca = reqwest::Certificate::from_pem(&std::fs::read(state.paths.certs().join("ca.crt")).unwrap()).unwrap();
            let client = |ip| reqwest::blocking::Client::builder().no_proxy().timeout(std::time::Duration::from_secs(3))
                .add_root_certificate(ca.clone()).resolve("lan-fixture.test", SocketAddr::new(ip, 0))
                .resolve("lan-sibling.test", SocketAddr::new(ip, 0))
                .build().unwrap();
            let local = client(Ipv4Addr::LOCALHOST.into()); let lan = client(address.into());
            let url = format!("https://lan-fixture.test:{tls_port}/");
            assert_eq!(local.get(&url).send().unwrap().text().unwrap(), "lan-site", "{server}");
            assert!(lan.get(&url).send().is_err(), "{server} unexpectedly reachable before opt-in");
            assert!(info(&state.paths, &state.store, &state.manager, &site.id).unwrap().addresses.iter().all(|entry| !entry.listening));
            // 第一次启用失败也必须能恢复未设置的默认本机模式。
            let external = std::net::TcpListener::bind((address, port)).unwrap();
            let error = apply(&state.paths, &state.store, &state.manager, &site.id, server, true).expect_err("must report initial bind failure");
            assert_ne!(error.code, "WEB_NETWORK_ROLLBACK", "{server}: {error:?}");
            assert_eq!(mode(&state.store, server).unwrap(), None);
            assert_eq!(local.get(&url).send().unwrap().text().unwrap(), "lan-site");
            drop(external);
            apply(&state.paths, &state.store, &state.manager, &site.id, server, true).unwrap_or_else(|error| panic!("{server}: {error:?}"));
            assert_eq!(local.get(&url).send().unwrap().text().unwrap(), "lan-site", "{server} local");
            assert_eq!(lan.get(&url).send().unwrap().text().unwrap(), "lan-site", "{server} LAN TLS");
            assert_eq!(lan.get(format!("http://lan-fixture.test:{port}/")).send().unwrap().text().unwrap(), "lan-site", "{server} redirect");
            let current = info(&state.paths, &state.store, &state.manager, &site.id).unwrap();
            assert!(current.enabled && current.local_ca && current.addresses.iter().any(|entry| entry.address == address.to_string() && entry.listening));
            input.name = "LAN sibling".into(); input.domains = vec!["lan-sibling.test".into()];
            let sibling = crate::sites::create(&input, &state.paths, &state.store, &state.manager).unwrap();
            assert_eq!(lan.get(format!("https://lan-sibling.test:{tls_port}/")).send().unwrap().text().unwrap(), "lan-site", "new sites inherit LAN mode");
            crate::sites::stop_site(&sibling.id, &state.paths, &state.store, &state.manager).unwrap();
            // 更新站点、普通重启均不能悄悄恢复回环。
            crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
            state.stop_service(server).unwrap(); state.start_service(server).unwrap();
            assert_eq!(lan.get(&url).send().unwrap().text().unwrap(), "lan-site");
            apply(&state.paths, &state.store, &state.manager, &site.id, server, false).unwrap();
            assert_eq!(local.get(&url).send().unwrap().text().unwrap(), "lan-site");
            assert!(lan.get(&url).send().is_err(), "{server} still reachable after disabling");
            assert!(info(&state.paths, &state.store, &state.manager, &site.id).unwrap().addresses.iter().all(|entry| !entry.listening));
            // 只有局域网 IP 上的外部监听与新通配地址冲突；原来的回环站点应能恢复。
            let external = std::net::TcpListener::bind((address, port)).unwrap();
            let error = apply(&state.paths, &state.store, &state.manager, &site.id, server, true).expect_err("must report bind failure");
            assert_ne!(error.code, "WEB_NETWORK_ROLLBACK", "{server}: {error:?}");
            assert_eq!(mode(&state.store, server).unwrap(), Some(false));
            assert_eq!(local.get(&url).send().unwrap().text().unwrap(), "lan-site", "{server} rollback did not restore local site");
            assert_eq!(external.local_addr().unwrap().port(), port, "external listener must remain untouched");
            drop(external);
            state.stop_service(server).unwrap();
            apply(&state.paths, &state.store, &state.manager, &site.id, server, true).unwrap();
            assert!(!state.manager.is_busy(server), "must not start a stopped service");
            state.start_service(server).unwrap();
            assert_eq!(lan.get(&url).send().unwrap().text().unwrap(), "lan-site");
            assert_eq!(apply(&state.paths, &state.store, &state.manager, &site.id, "other", true).unwrap_err().code, "WEB_NETWORK_SERVER");
            println!("{server}: opt-in, true interface HTTP/TLS, redirect, loopback, update, restart, disable and deferred apply passed");
        }
    }
}
