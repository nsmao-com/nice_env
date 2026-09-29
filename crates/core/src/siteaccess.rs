//! 站点来源 IP 限制；在跳转、CORS 预检和内容处理之前拒绝，不信任转发头。
use crate::{error::{AppError, Result}, model::{SiteAccess, SiteAccessMode}};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

fn address(raw: &str) -> Result<String> {
    let invalid = || AppError::new("BAD_SITE_ACCESS", "请输入有效 IPv4、IPv6 地址或 CIDR 网段，例如 192.168.1.20 或 192.168.1.0/24");
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 64 { return Err(invalid()); }
    let (host, prefix) = match raw.split_once('/') {
        Some((host, prefix)) if !prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_digit()) =>
            (host, Some(prefix.parse::<u8>().map_err(|_| invalid())?)),
        Some(_) => return Err(invalid()),
        None => (raw, None),
    };
    let mut ip: IpAddr = host.parse().map_err(|_| invalid())?;
    let mut prefix = prefix;
    // IPv4-mapped IPv6 的服务端处理存在差异，统一按其对应的 IPv4 地址保存。
    if let IpAddr::V6(v6) = ip {
        if let Some(v4) = v6.to_ipv4_mapped() {
            prefix = prefix.map(|prefix| prefix.checked_sub(96).ok_or_else(invalid)).transpose()?;
            ip = IpAddr::V4(v4);
        }
    }
    let Some(prefix) = prefix else { return Ok(ip.to_string()); };
    let network = match ip {
        IpAddr::V4(ip) if prefix <= 32 => IpAddr::V4(Ipv4Addr::from(u32::from(ip) & if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) })),
        IpAddr::V6(ip) if prefix <= 128 => IpAddr::V6(Ipv6Addr::from(u128::from(ip) & if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) })),
        _ => return Err(invalid()),
    };
    Ok(format!("{network}/{prefix}"))
}

pub fn normalize(policy: &SiteAccess) -> Result<SiteAccess> {
    if policy.addresses.is_empty() || policy.addresses.len() > 32 {
        return Err(AppError::new("BAD_SITE_ACCESS", "访问限制需填写 1–32 个 IP 地址或网段"));
    }
    let mut addresses = Vec::new();
    for raw in &policy.addresses {
        let value = address(raw)?;
        if !addresses.contains(&value) { addresses.push(value); }
    }
    Ok(SiteAccess { mode: policy.mode.clone(), addresses })
}

/// geo 在 http 上下文声明；server 级 rewrite 在 return、预检和 location 之前执行。
pub(crate) fn nginx(id: &str, policy: Option<&SiteAccess>) -> (String, String) {
    let Some(policy) = policy else { return (String::new(), String::new()); };
    let Ok(policy) = normalize(policy) else { return (String::new(), "    return 403;\n".into()); };
    let variable = format!("nsb_access_{}", &hex::encode(Sha256::digest(id.as_bytes()))[..16]);
    let (default, matching) = if policy.mode == SiteAccessMode::Allow { (1, 0) } else { (0, 1) };
    let mut geo = format!("geo ${variable} {{\n    default {default};\n");
    for ip in &policy.addresses { geo.push_str(&format!("    {ip} {matching};\n")); }
    geo.push_str("}\n");
    (geo, format!("    if (${variable}) {{ return 403; }}\n"))
}

pub(crate) fn apache(policy: Option<&SiteAccess>) -> String {
    let Some(policy) = policy else { return String::new(); };
    let Ok(policy) = normalize(policy) else { return "    RewriteEngine On\n    RewriteRule ^ - [F,END]\n".into(); };
    let matches = policy.addresses.iter().map(|ip| format!("%{{CONN_REMOTE_ADDR}} -ipmatch '{ip}'")).collect::<Vec<_>>().join(" || ");
    let negate = if policy.mode == SiteAccessMode::Allow { "!" } else { "" };
    format!("    RewriteEngine On\n    RewriteCond expr \"{negate}({matches})\"\n    RewriteRule ^ - [F,END]\n")
}

/// 放在显式 route 的第一条处理器之前，HTTP 跳转块也必须应用。
pub(crate) fn caddy(policy: Option<&SiteAccess>) -> Result<String> {
    let Some(policy) = policy else { return Ok(String::new()); };
    let policy = normalize(policy)?;
    let negate = if policy.mode == SiteAccessMode::Allow { "not " } else { "" };
    Ok(format!("\t\t@nsb_access_denied {negate}remote_ip {}\n\t\trespond @nsb_access_denied 403\n", policy.addresses.join(" ")))
}

#[cfg(test)]
mod checks {
    use super::*;

    #[test]
    fn networks_are_canonical_and_invalid_rules_never_become_unrestricted() {
        for (raw, expected) in [("192.168.3.44/24", "192.168.3.0/24"), ("2001:db8::44/64", "2001:db8::/64"),
            ("::ffff:127.0.0.1/128", "127.0.0.1/32"), ("1.2.3.4/0", "0.0.0.0/0"), ("::1/0", "::/0")] {
            assert_eq!(address(raw).unwrap(), expected);
        }
        for raw in ["", "localhost", "127.0.0.1:80", "127.0.0.1; allow all", "127.1", "01.2.3.4", "0.0.0.0/33", "::/129", "::1/-1", "::1/64/1", "fe80::1%12", "::ffff:1.2.3.4/64"] {
            assert!(address(raw).is_err(), "{raw}");
        }
        let invalid = SiteAccess { mode: SiteAccessMode::Allow, addresses: vec![] };
        assert!(normalize(&invalid).is_err());
        assert!(nginx("site", Some(&invalid)).1.contains("return 403"));
        assert!(apache(Some(&invalid)).contains("[F,END]"));
        assert!(caddy(Some(&invalid)).is_err());
        let policy = SiteAccess { mode: SiteAccessMode::Allow, addresses: vec!["192.168.1.2/24".into(), "192.168.1.99/24".into()] };
        assert_eq!(normalize(&policy).unwrap().addresses, ["192.168.1.0/24"]);
        assert_eq!(nginx("site", None), (String::new(), String::new()));
    }

    #[test]
    #[ignore = "requires official NSB_NGINX_ROOT, NSB_APACHE_ROOT, NSB_VERIFY_CADDY and NSB_SKIP_HOSTS=1; isolated loopback sites only"]
    fn native_access_rules_cover_content_preflight_proxy_and_redirects() {
        use std::{io::{Read, Write}, net::{SocketAddr, TcpListener}, path::PathBuf, sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}}, time::Duration};
        use crate::model::{CreateSiteInput, InstalledPackage, SiteKind, SiteRedirect};
        assert_eq!(std::env::var("NSB_SKIP_HOSTS").unwrap(), "1");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stopped = Arc::new(AtomicBool::new(false)); let requests = Arc::new(AtomicUsize::new(0));
        let stop = stopped.clone(); let count = requests.clone();
        let worker = std::thread::spawn(move || while !stop.load(Ordering::Relaxed) {
            if let Ok((mut stream, _)) = listener.accept() {
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut request = Vec::new(); let mut bytes = [0; 4096];
                while request.len() < 16384 && !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    match stream.read(&mut bytes) { Ok(0) | Err(_) => break, Ok(count) => request.extend_from_slice(&bytes[..count]) }
                }
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    count.fetch_add(1, Ordering::Relaxed);
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\naccess-proxy");
                }
            } else { std::thread::sleep(Duration::from_millis(10)); }
        });
        struct Upstream(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
        impl Drop for Upstream { fn drop(&mut self) { self.0.store(true, Ordering::Relaxed); self.1.take().unwrap().join().unwrap(); } }
        let _upstream = Upstream(stopped, Some(worker));
        for (server, version, variable, http_key, https_key) in [
            ("nginx", "1.28.1", "NSB_NGINX_ROOT", "http", "https"),
            ("apache", "2.4.66", "NSB_APACHE_ROOT", "apacheHttp", "apacheHttps"),
            ("caddy", "2.11.4", "NSB_VERIFY_CADDY", "caddy", "caddyHttps"),
        ] {
            if std::env::var("NSB_ACCESS_SERVER").is_ok_and(|value| value != server) { continue; }
            let root = PathBuf::from(std::env::var_os(variable).expect(variable));
            let temp = tempfile::tempdir().unwrap();
            let state = crate::CoreState::init(Some(temp.path().join("Access rules")), Arc::new(|_| {})).unwrap();
            struct Cleanup(Arc<crate::CoreState>, &'static str);
            impl Drop for Cleanup { fn drop(&mut self) { let _ = self.0.stop_service(self.1); let _ = self.0.stop_service("php@8.4.26"); } }
            let _cleanup = Cleanup(state.clone(), server);
            let http = TcpListener::bind("127.0.0.1:0").unwrap(); let https = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = http.local_addr().unwrap().port(); let tls_port = https.local_addr().unwrap().port();
            state.store.set_port_override(http_key, Some(port)).unwrap(); state.store.set_port_override(https_key, Some(tls_port)).unwrap();
            state.store.set_setting("autoFallbackPort", "false").unwrap();
            state.store.upsert_installed(&InstalledPackage { id: server.into(), version: version.into(), category: "web-server".into(),
                install_path: root.parent().unwrap().to_string_lossy().into(), config_path: String::new(), installed_at: 1 }).unwrap();
            let project = temp.path().join("project"); std::fs::create_dir_all(&project).unwrap(); std::fs::write(project.join("index.html"), "access-static").unwrap();
            let input: CreateSiteInput = serde_json::from_value(serde_json::json!({
                "name":"Access fixture","domains":["access-fixture.test"],"rootDir":project,"https":true,"rewrite":"none","template":"none","writeEnvExample":false,
                "runtime":{"kind":"static","webServer":server,"proxyRules":[{"path":"/api","target":upstream,"stripPrefix":true}],
                    "cors":{"origins":["https://client.test"],"methods":["GET","HEAD","POST","OPTIONS"],"allowedHeaders":[],"exposedHeaders":[],"credentials":false,"maxAge":0}}
            })).unwrap();
            drop((http, https));
            let mut site = crate::sites::create(&input, &state.paths, &state.store, &state.manager).unwrap();
            assert!(site.runtime.access.is_none());
            let certificate = reqwest::Certificate::from_pem(&std::fs::read(state.paths.certs().join("ca.crt")).unwrap()).unwrap();
            let client = |source| reqwest::blocking::Client::builder().no_proxy().timeout(Duration::from_secs(5)).pool_max_idle_per_host(0)
                .local_address(IpAddr::V4(source)).redirect(reqwest::redirect::Policy::none()).add_root_certificate(certificate.clone())
                .resolve("access-fixture.test", SocketAddr::from(([127, 0, 0, 1], 0))).build().unwrap();
            let local = client(Ipv4Addr::LOCALHOST); let other = client(Ipv4Addr::new(127, 0, 0, 2));
            let urls = [format!("http://access-fixture.test:{port}"), format!("https://access-fixture.test:{tls_port}")];
            assert_eq!(other.get(&urls[1]).send().unwrap().text().unwrap(), "access-static");
            site.runtime.access = Some(SiteAccess { mode: SiteAccessMode::Allow, addresses: vec!["127.0.0.1/32".into(), "::1/128".into()] });
            let update = |site: &crate::model::Site| crate::sites::update(site, &state.paths, &state.store, &state.manager)
                .unwrap_or_else(|e| panic!("{server}: {e:?}; {:?}", state.manager.tail(server, 30)));
            site = update(&site);
            for url in &urls {
                assert_eq!(local.get(url).send().unwrap().text().unwrap(), "access-static", "{server}");
                assert_eq!(local.get(format!("{url}/api/item")).send().unwrap().text().unwrap(), "access-proxy");
                assert_eq!(local.request(reqwest::Method::OPTIONS, url).header("Origin", "https://client.test").header("Access-Control-Request-Method", "GET").send().unwrap().status().as_u16(), 204);
                let before = requests.load(Ordering::Relaxed);
                for path in ["/", "/api/item", "/missing", "/.well-known/check"] {
                    assert_eq!(other.get(format!("{url}{path}")).header("X-Forwarded-For", "127.0.0.1").header("X-Real-IP", "127.0.0.1").send().unwrap().status().as_u16(), 403, "{server} {path}");
                }
                assert_eq!(other.request(reqwest::Method::OPTIONS, url).header("Origin", "https://client.test").header("Access-Control-Request-Method", "GET").send().unwrap().status().as_u16(), 403);
                assert_eq!(other.post(format!("{url}/api/submit")).body("blocked-payload").send().unwrap().status().as_u16(), 403);
                assert_eq!(requests.load(Ordering::Relaxed), before, "denied requests reached the upstream");
            }
            // 无效输入不能覆盖已有规则；备份导入仍保留限制。
            let mut invalid = site.clone(); invalid.runtime.access.as_mut().unwrap().addresses = vec!["0.0.0.0/0;".into()];
            assert_eq!(crate::sites::update(&invalid, &state.paths, &state.store, &state.manager).unwrap_err().code, "BAD_SITE_ACCESS");
            assert_eq!(state.store.list_sites().unwrap()[0].runtime.access, site.runtime.access);
            let backup = temp.path().join("sites.json"); crate::transfer::export_to(&state.store, &backup).unwrap();
            let imported = crate::paths::Paths::new(temp.path().join("imported")); imported.ensure_dirs().unwrap();
            let imported_store = crate::store::Store::open(imported.db()).unwrap(); crate::tls::issue_site_cert(&imported, &imported_store, &site.domains).unwrap();
            crate::transfer::import_from(&backup, &imported, &imported_store, &Arc::new(crate::services::ServiceManager::new())).unwrap();
            assert_eq!(imported_store.list_sites().unwrap()[0].runtime.access, site.runtime.access);
            site.runtime.access = Some(SiteAccess { mode: SiteAccessMode::Deny, addresses: vec!["127.0.0.2/32".into(), "2001:db8::/32".into()] });
            site.runtime.https_redirect = Some(307); site = update(&site);
            assert_eq!(local.get(&urls[0]).send().unwrap().status().as_u16(), 307);
            assert_eq!(other.get(&urls[0]).send().unwrap().status().as_u16(), 403, "HTTP redirect bypassed restrictions");
            if let Some(php_root) = std::env::var_os("NSB_PHP_ROOT") {
                state.store.upsert_installed(&InstalledPackage { id: "php".into(), version: "8.4.26".into(), category: "language".into(),
                    install_path: PathBuf::from(php_root).to_string_lossy().into(), config_path: String::new(), installed_at: 1 }).unwrap();
                let base = loop {
                    let first = TcpListener::bind("127.0.0.1:0").unwrap(); let base = first.local_addr().unwrap().port();
                    if base > 65531 { continue; }
                    let rest = (1..4).map(|i| TcpListener::bind(("127.0.0.1", base + i))).collect::<std::io::Result<Vec<_>>>();
                    if rest.is_ok() { break base; }
                };
                state.store.set_port_assign("php@8.4.26", base).unwrap();
                std::fs::write(project.join("index.php"), "<?php echo 'access-php';").unwrap();
                site.runtime.kind = SiteKind::Php; site.runtime.php_version = Some("8.4.26".into()); site = update(&site);
                assert_eq!(local.get(format!("{}/index.php", urls[1])).send().unwrap().text().unwrap(), "access-php");
                assert_eq!(other.get(format!("{}/index.php", urls[1])).send().unwrap().status().as_u16(), 403);
            }
            site.runtime.proxy_rules.clear(); site.runtime.kind = SiteKind::ReverseProxy; site.runtime.proxy_target = Some(upstream.clone()); site = update(&site);
            assert_eq!(local.get(&urls[1]).send().unwrap().text().unwrap(), "access-proxy");
            assert_eq!(other.get(&urls[1]).send().unwrap().status().as_u16(), 403);
            site.runtime.kind = SiteKind::Redirect; site.runtime.redirect = Some(SiteRedirect { target: "https://example.invalid/".into(), status: 302, preserve_path: false });
            site = update(&site);
            assert_eq!(local.get(&urls[1]).send().unwrap().status().as_u16(), 302);
            assert_eq!(other.get(&urls[1]).send().unwrap().status().as_u16(), 403, "domain redirect bypassed restrictions");
            state.stop_service(server).unwrap(); state.start_service(server).unwrap();
            assert_eq!(other.get(&urls[1]).send().unwrap().status().as_u16(), 403, "restart lost restrictions");
            site.runtime.access = None; site = update(&site);
            assert_eq!(other.get(&urls[1]).send().unwrap().status().as_u16(), 302, "disabled rule still blocks requests");
            crate::sites::stop_site(&site.id, &state.paths, &state.store, &state.manager).unwrap();
            site.runtime.access = Some(SiteAccess { mode: SiteAccessMode::Allow, addresses: vec!["127.0.0.99/8".into()] });
            let saved = update(&site);
            assert_eq!(saved.runtime.access.unwrap().addresses, ["127.0.0.0/8"]);
            assert_eq!(crate::sites::derive_status(&state.paths, &site), "stopped");
            println!("{server}: source IP allow/deny, HTTP/TLS, spoofed headers, CORS, proxies, redirects, persistence, backup, removal and stopped-site edits passed");
        }
    }
}
