//! 同一站点按路径代理；路径以段为边界，较长规则优先。
use crate::{error::{AppError, Result}, model::{SiteKind, SiteProxyRule, SiteRuntime}};

pub fn normalize(runtime: &SiteRuntime) -> Result<Vec<SiteProxyRule>> {
    let invalid = |message| AppError::new("BAD_PROXY_RULE", message);
    if runtime.proxy_rules.len() > 16 { return Err(invalid("每个站点最多设置 16 条代理规则")); }
    if !runtime.proxy_rules.is_empty() && runtime.kind == SiteKind::Redirect {
        return Err(invalid("域名跳转站点不使用路径代理规则"));
    }
    let mut rules: Vec<SiteProxyRule> = Vec::new();
    for rule in &runtime.proxy_rules {
        let raw = rule.path.trim();
        let path = raw.trim_end_matches('/');
        if path.is_empty() || path.len() > 256 || !path.starts_with('/') || raw.contains("//")
            || !raw.bytes().all(|c| c.is_ascii_alphanumeric() || b"/-._~".contains(&c))
            || path.split('/').skip(1).any(|segment| matches!(segment, "." | "..")) {
            return Err(invalid("匹配路径需以 / 开头，如 /api；不能是根路径、正则表达式或包含空格、参数、.."));
        }
        if rules.iter().any(|existing| existing.path == path) {
            return Err(invalid("代理规则存在重复路径，请合并或修改"));
        }
        if rule.target.len() > 8192 { return Err(invalid("代理目标地址过长")); }
        let target = crate::sites::proxy_url(&rule.target)
            .map_err(|error| AppError::new("BAD_PROXY_RULE", error.message))?;
        rules.push(SiteProxyRule { path: path.into(), target, strip_prefix: rule.strip_prefix });
    }
    if let Some(custom) = runtime.custom_rewrite.as_ref().filter(|custom| custom.server == "nginx") {
        fn conflicts(nodes: &[crate::configgen::NginxDirective], rules: &[SiteProxyRule]) -> bool {
            nodes.iter().any(|node| (node.words.first().is_some_and(|word| word == "location")
                && node.words.last().is_some_and(|path| rules.iter().any(|rule| path.trim_end_matches('/') == rule.path)))
                || conflicts(&node.children, rules))
        }
        if !rules.is_empty() && conflicts(&crate::configgen::nginx_directives(&custom.content)?, &rules) {
            return Err(invalid("代理规则与自定义伪静态的 location 路径重复，请保留一处配置"));
        }
    }
    Ok(rules)
}

fn destination(rule: &SiteProxyRule) -> String {
    if rule.strip_prefix { rule.target.clone() } else { format!("{}{}/", rule.target, rule.path.trim_start_matches('/')) }
}

fn ordered(runtime: &SiteRuntime) -> Result<Vec<SiteProxyRule>> {
    let mut rules = normalize(runtime)?;
    rules.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then_with(|| a.path.cmp(&b.path)));
    Ok(rules)
}

pub(crate) fn nginx(runtime: &SiteRuntime) -> String {
    let Ok(rules) = ordered(runtime) else { return "    return 400;\n".into(); };
    let mut output = String::new();
    for rule in rules {
        let target = destination(&rule);
        let cookie_path = reqwest::Url::parse(&target).expect("normalized proxy target").path().trim_end_matches('/').to_string();
        let cookie_pattern = if cookie_path.is_empty() { "^(/.*)$".into() } else { format!("^{}(/.*)?$", regex::escape(&cookie_path)) };
        for location in [format!("= {}", rule.path), format!("^~ {}/", rule.path)] {
            output.push_str(&format!(
                "    location {location} {{\n        proxy_pass {target};\n        proxy_redirect {target} {path}/;\n        proxy_cookie_path \"~{cookie_pattern}\" \"{path}$1\";\n        proxy_ssl_server_name on;\n        proxy_http_version 1.1;\n        proxy_set_header Host $proxy_host;\n        proxy_set_header X-Real-IP $remote_addr;\n        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n        proxy_set_header X-Forwarded-Proto $scheme;\n        proxy_set_header Upgrade $http_upgrade;\n        proxy_set_header Connection \"upgrade\";\n        proxy_read_timeout 300s;\n    }}\n", path = rule.path));
        }
    }
    output
}

pub(crate) fn apache(runtime: &SiteRuntime) -> String {
    let Ok(rules) = ordered(runtime) else { return "    RewriteEngine On\n    RewriteRule ^ - [F,L]\n".into(); };
    if rules.is_empty() { return String::new(); }
    let mut output = "    SSLProxyEngine On\n    ProxyPreserveHost Off\n    RewriteEngine On\n".to_string();
    for rule in &rules {
        let target = destination(rule);
        let cookie_path = reqwest::Url::parse(&target).expect("normalized proxy target").path().trim_end_matches('/').to_string();
        let cookie_pattern = format!("((?i:;[ \\t]*path=)){}{}", regex::escape(&cookie_path), if cookie_path.is_empty() { "(?=/)" } else { "(?=/|;|$)" });
        let pattern = regex::escape(&rule.path);
        // 含 $1 的替换避免 ProxyPassMatch 在无捕获替换时再次追加原 URI。
        output.push_str(&format!("    ProxyPassMatch \"^{pattern}(?:/(.*))?$\" \"{target}$1\" upgrade=websocket\n"));
        // Location 按完整路径段匹配，并将重定向映射限制在当前规则内，避免相同上游的规则相互抢占。
        output.push_str(&format!(
            "    <Location \"{path}\">\n        ProxyPassReverse \"{reverse}\"\n        Header onsuccess edit* Set-Cookie \"{cookie_pattern}\" \"$1{path}\"\n        Header always edit* Set-Cookie \"{cookie_pattern}\" \"$1{path}\"\n        RequestHeader set X-Forwarded-Proto \"expr=%{{REQUEST_SCHEME}}\"\n    </Location>\n",
            path = rule.path, reverse = target.trim_end_matches('/')));
    }
    let paths = rules.iter().map(|rule| regex::escape(&rule.path)).collect::<Vec<_>>().join("|");
    // 保留 ProxyPass 的处理，让 PHP 与自定义 server 级 rewrite 不抢走已匹配的代理请求。
    output.push_str(&format!("    RewriteRule \"^(?:{paths})(?:/|$)\" \"-\" [L]\n"));
    output
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::{Read, Write}, path::PathBuf, sync::{Arc, atomic::{AtomicBool, Ordering}}};

    fn rule(path: &str, target: &str, strip_prefix: bool) -> SiteProxyRule {
        SiteProxyRule { path: path.into(), target: target.into(), strip_prefix }
    }

    #[test]
    fn proxy_rules_validate_and_preserve_old_sites() {
        let mut runtime: SiteRuntime = serde_json::from_value(serde_json::json!({"kind":"static","webServer":"nginx"})).unwrap();
        assert!(runtime.proxy_rules.is_empty());
        runtime.proxy_rules = vec![rule(" /api/ ", "127.0.0.1:8081/v1", true)];
        let normalized = normalize(&runtime).unwrap();
        assert_eq!(normalized[0].path, "/api"); assert_eq!(normalized[0].target, "http://127.0.0.1:8081/v1/");
        for invalid in ["/", "", "api", "/a//b", "/../api", "/a/./b", "/api?x", "/a%20b", "/a b", "/a\nb", "/a;return", "/api.*$"] {
            runtime.proxy_rules[0].path = invalid.into(); assert!(normalize(&runtime).is_err(), "{invalid}");
        }
        runtime.proxy_rules = vec![rule("/api", "127.0.0.1:8081", true), rule("/api/", "127.0.0.1:8082", false)];
        assert!(normalize(&runtime).is_err());
        runtime.proxy_rules.pop();
        runtime.kind = SiteKind::Redirect; assert!(normalize(&runtime).is_err());
        runtime.kind = SiteKind::Static;
        runtime.custom_rewrite = Some(crate::model::CustomRewrite { name:"conflict".into(), server:"nginx".into(), content:"location /api/ { return 200; }".into() });
        assert!(normalize(&runtime).is_err());
        runtime.custom_rewrite.as_mut().unwrap().content = "location / { try_files $uri =404; }".into();
        assert!(normalize(&runtime).is_ok());
        for invalid in ["https://user:pass@example.test", "http://x.test/?token=bad", "http://x.test/#frag", "file:///tmp", "http://localhost:0"] {
            runtime.proxy_rules[0].target = invalid.into(); assert!(normalize(&runtime).is_err(), "{invalid}");
        }
    }

    #[test]
    #[ignore = "requires isolated native Nginx/Apache and NSB_SKIP_HOSTS=1"]
    fn proxy_rules_native_nginx_and_apache() {
        assert_eq!(std::env::var("NSB_SKIP_HOSTS").as_deref(), Ok("1"));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let source_port = listener.local_addr().unwrap().port();
        let source_origin = format!("http://127.0.0.1:{source_port}");
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false)); let stopped = stop.clone();
        let origin = source_origin.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else { std::thread::sleep(std::time::Duration::from_millis(10)); continue; };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(1))).unwrap();
                let mut raw = Vec::new(); let mut chunk = [0;4096]; let mut header_end = 0;
                while raw.len() < 65536 {
                    let count = stream.read(&mut chunk).unwrap_or(0); if count == 0 { break; } raw.extend_from_slice(&chunk[..count]);
                    if let Some(end) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        header_end = end + 4;
                        let text = String::from_utf8_lossy(&raw[..end]);
                        let length = text.lines().filter_map(|line| line.split_once(':')).find(|(key,_)| key.eq_ignore_ascii_case("Content-Length"))
                            .and_then(|(_, value)| value.trim().parse::<usize>().ok()).unwrap_or(0);
                        if raw.len() >= header_end + length { break; }
                    }
                }
                if header_end == 0 { continue; }
                let text = String::from_utf8_lossy(&raw[..header_end]);
                let mut start = text.lines().next().unwrap().split_whitespace();
                let method = start.next().unwrap_or_default(); let uri = start.next().unwrap_or_default();
                let headers = text.lines().skip(1).filter_map(|line| line.split_once(':')).map(|(k,v)|(k.to_ascii_lowercase(),v.trim().to_string())).collect::<std::collections::BTreeMap<_,_>>();
                if let Some(key) = headers.get("sec-websocket-key") {
                    use sha1::{Digest, Sha1}; use base64::Engine;
                    let accept = base64::engine::general_purpose::STANDARD.encode(Sha1::digest(format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11")));
                    let response = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n");
                    let _ = stream.write_all(response.as_bytes());
                    let message = format!("route-websocket:{uri}");
                    let _ = stream.write_all(&[0x81, message.len() as u8]); let _ = stream.write_all(message.as_bytes());
                    continue;
                }
                let body = serde_json::json!({"method":method,"uri":uri,"headers":headers,"body":String::from_utf8_lossy(&raw[header_end..])}).to_string();
                let redirect = uri.split('?').next().unwrap().strip_suffix("/redirect").map(|base| format!("Location: {origin}{base}/login\r\n"));
                let status = if redirect.is_some() { "302 Found" } else { "200 OK" };
                let cookie_base = if uri.starts_with("/v1/keep/") { "/v1/keep" } else if uri.starts_with("/admin/") { "/admin" } else { "/v1" };
                let cookies = format!("Set-Cookie: route_session=fixture; Path={cookie_base}/; HttpOnly; SameSite=Lax\r\nSet-Cookie: route_private=fixture; Path={cookie_base}/private; HttpOnly\r\nSet-Cookie: route_exact=fixture; Path={cookie_base}; HttpOnly\r\n");
                let response = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{}{cookies}Connection: close\r\n\r\n{body}", body.len(), redirect.unwrap_or_default());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        struct Source(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
        impl Drop for Source { fn drop(&mut self) { self.0.store(true,Ordering::Relaxed); let _ = self.1.take().unwrap().join(); } }
        let _source = Source(stop,Some(worker));
        for (server, version, variable, port_key, tls_key) in [
            ("nginx","1.28.1","NSB_NGINX_ROOT","http","https"),
            ("apache","2.4.66","NSB_APACHE_ROOT","apacheHttp","apacheHttps"),
            ("caddy","2.11.4","NSB_VERIFY_CADDY","caddy","caddyHttps"),
        ] {
            if std::env::var("NSB_PROXY_SERVER").is_ok_and(|selected| selected != server) { continue; }
            if server == "caddy" && std::env::var_os(variable).is_none() { continue; }
            let root = PathBuf::from(std::env::var(variable).expect(variable));
            let temp = tempfile::tempdir().unwrap();
            let state = crate::CoreState::init(Some(temp.path().join("proxy routes with spaces")), Arc::new(|_|{})).unwrap();
            let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let https = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = http.local_addr().unwrap().port(); let tls_port = https.local_addr().unwrap().port();
            state.store.set_port_override(port_key,Some(port)).unwrap(); state.store.set_port_override(tls_key,Some(tls_port)).unwrap();
            state.store.upsert_installed(&crate::model::InstalledPackage { id:server.into(),version:version.into(), category:"web-server".into(),
                install_path:root.parent().unwrap().to_string_lossy().into(),config_path:String::new(),installed_at:1 }).unwrap();
            struct Cleanup(Arc<crate::CoreState>, &'static str);
            impl Drop for Cleanup { fn drop(&mut self) { let _ = self.0.stop_service(self.1); let _ = self.0.stop_service("php@8.4.26"); } }
            let _cleanup = Cleanup(state.clone(),server);
            let project = state.paths.base.join("project"); std::fs::create_dir_all(&project).unwrap(); std::fs::write(project.join("index.html"),"static-front").unwrap();
            std::fs::create_dir_all(project.join("tls-upstream")).unwrap(); std::fs::write(project.join("tls-upstream/check.html"), "secure-backend").unwrap();
            let routes = vec![rule("/api",&format!("{source_origin}/v1"),true),rule("/api/admin",&format!("{source_origin}/admin"),true),
                rule("/keep",&format!("{source_origin}/v1"),false),rule("/alias",&format!("{source_origin}/v1"),true),rule("/socket",&source_origin,true),
                rule("/secure",&format!("https://127.0.0.1:{tls_port}/tls-upstream/"),true)];
            let creation: crate::model::CreateSiteInput = serde_json::from_value(serde_json::json!({
                "name":"Proxy routes", "domains":["127.0.0.1","proxy.test"], "rootDir":project, "https":true,"rewrite":"none","template":"none","writeEnvExample":false,
                "runtime":{"kind":"static","webServer":server,"proxyRules":routes,"cors":{"origins":["https://frontend.test"],"methods":["GET","POST","OPTIONS"],"allowedHeaders":["Content-Type"],"exposedHeaders":[],"credentials":false,"maxAge":600}}
            })).unwrap();
            drop((http,https));
            let mut site = crate::sites::create(&creation,&state.paths,&state.store,&state.manager).unwrap();
            let client = reqwest::blocking::Client::builder().no_proxy().danger_accept_invalid_certs(true).redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(5)).build().unwrap();
            let address = format!("http://127.0.0.1:{port}");
            // 静态目录可能同时包含未清理的 PHP 源码；首页和直接请求都不能下载源码。
            let sources = ["index.php", "config.PHP", "legacy.php8", "view.phtml", "view.pht", "archive.phar", "config.php.bak"];
            for name in sources { std::fs::write(project.join(name), "<?php /* static-source-must-stay-private */").unwrap(); }
            std::fs::create_dir_all(project.join("private-dir")).unwrap();
            std::fs::write(project.join("private-dir/secret.txt"), "directory-listing-must-stay-private").unwrap();
            for origin in [&address, &format!("https://127.0.0.1:{tls_port}")] {
                for name in sources {
                    let response = client.get(format!("{origin}/{name}")).send().unwrap();
                    assert_eq!(response.status().as_u16(), 403, "{server}: static source {name}");
                    assert!(!response.text().unwrap().contains("static-source-must-stay-private"));
                }
                let directory = client.get(format!("{origin}/private-dir/")).send().unwrap();
                assert!(directory.status().is_client_error(), "{server}: static directory listing");
                assert!(!directory.text().unwrap().contains("directory-listing-must-stay-private"));
                for path in ["/config.%50HP", "/index.%70hp", "/index.php/extra"] {
                    let response = client.get(format!("{origin}{path}")).send().unwrap();
                    assert!(response.status().is_client_error(), "{server}: encoded or PATH_INFO source {path}");
                    assert!(!response.text().unwrap().contains("static-source-must-stay-private"));
                }
                assert_eq!(client.get(origin).send().unwrap().text().unwrap(), "static-front", "{server}: HTML takes precedence over PHP in static sites");
            }
            assert_eq!(client.get(&address).send().unwrap().text().unwrap(),"static-front");
            let secure=client.get(format!("{address}/secure/check.html")).send().unwrap();
            assert_eq!(secure.status().as_u16(),200,"{server} HTTPS backend: {:?}",state.tail_logs_checked(&format!("site-error:{}",site.id),20).unwrap());
            assert_eq!(secure.text().unwrap(),"secure-backend");
            assert_eq!(client.get(format!("{address}/apiary")).send().unwrap().status().as_u16(),404);
            for (path,expected) in [("/api","/v1/"),("/api/","/v1/"),("/api/users?x=a%2Fb&n=2","/v1/users?x=a%2Fb&n=2"),
                ("/api/a%20b","/v1/a%20b"),("/api/admin/users","/admin/users"),("/keep/users","/v1/keep/users"),("/alias/users","/v1/users"),("/api/users.php","/v1/users.php")] {
                let response = client.get(format!("{address}{path}")).header("Origin","https://frontend.test").send().unwrap();
                assert_eq!(response.status().as_u16(),200,"{server} {path}: {:?}",state.tail_logs_checked(&format!("site-error:{}",site.id),20).unwrap());
                assert_eq!(response.headers()["access-control-allow-origin"],"https://frontend.test");
                let payload: serde_json::Value = response.json().unwrap(); assert_eq!(payload["uri"],expected,"{server} {path}");
            }
            let payload: serde_json::Value = client.post(format!("{address}/api/items?mode=create")).header("Content-Type","application/json").body("{\"hello\":\"world\"}").send().unwrap().json().unwrap();
            assert_eq!(payload["method"],"POST"); assert_eq!(payload["body"],"{\"hello\":\"world\"}"); assert_eq!(payload["headers"]["x-forwarded-proto"],"http");
            let payload: serde_json::Value = client.get(format!("https://127.0.0.1:{tls_port}/api/users")).send().unwrap().json().unwrap();
            assert_eq!(payload["uri"],"/v1/users"); assert_eq!(payload["headers"]["x-forwarded-proto"],"https");
            for path in ["/api","/alias","/api/admin"] {
                let response = client.get(format!("{address}{path}/redirect")).send().unwrap();
                assert_eq!(response.status().as_u16(),302,"{server} {path}");
                let location = reqwest::Url::parse(&address).unwrap().join(response.headers()["location"].to_str().unwrap()).unwrap();
                assert_eq!(location.to_string(),format!("{address}{path}/login"),"{server} scoped reverse mapping");
                let cookies=response.headers().get_all("set-cookie").iter().map(|value|value.to_str().unwrap()).collect::<Vec<_>>();
                assert_eq!(cookies.len(),3,"{server} cookies");
                for expected in [format!("Path={path}/;"),format!("Path={path}/private;"),format!("Path={path};")] {
                    assert!(cookies.iter().any(|cookie|cookie.contains(&expected)),"{server} cookie path {expected}: {cookies:?}");
                }
            }
            let preflight = client.request(reqwest::Method::OPTIONS,format!("{address}/api/users")).header("Origin","https://frontend.test")
                .header("Access-Control-Request-Method","POST").header("Access-Control-Request-Headers","Content-Type").send().unwrap();
            assert_eq!(preflight.status().as_u16(),204,"{server} CORS and routes");
            let mut socket = std::net::TcpStream::connect(("127.0.0.1",port)).unwrap(); socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            socket.write_all(format!("GET /socket/chat HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n").as_bytes()).unwrap();
            let mut received = Vec::new(); let mut chunk = [0;1024];
            while !String::from_utf8_lossy(&received).contains("route-websocket:/chat") {
                let count = socket.read(&mut chunk).unwrap(); assert!(count>0,"{server} websocket: {:?}",String::from_utf8_lossy(&received)); received.extend_from_slice(&chunk[..count]);
            }
            assert!(String::from_utf8_lossy(&received).starts_with("HTTP/1.1 101"),"{server} websocket");
            if let Some(directory) = std::env::var_os("NSB_PROXY_BROWSER") {
                let directory=PathBuf::from(directory);assert!(directory.starts_with(std::env::temp_dir()));std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(directory.join(format!("{server}.json")),serde_json::to_vec(&serde_json::json!({"address":address})).unwrap()).unwrap();
                let until=std::time::Instant::now()+std::time::Duration::from_secs(60);
                while !directory.join(format!("{server}.done")).is_file(){assert!(std::time::Instant::now()<until,"browser timeout");std::thread::sleep(std::time::Duration::from_millis(100));}
            }
            let original = site.runtime.proxy_rules.clone(); site.runtime.proxy_rules.push(rule("/api/",&source_origin,true));
            assert_eq!(crate::sites::update(&site,&state.paths,&state.store,&state.manager).unwrap_err().code,"BAD_PROXY_RULE");
            assert_eq!(state.store.list_sites().unwrap()[0].runtime.proxy_rules.len(),6); site.runtime.proxy_rules=original;
            if let Some(php_root)=std::env::var_os("NSB_PHP_ROOT") {
                state.store.upsert_installed(&crate::model::InstalledPackage{id:"php".into(),version:"8.4.26".into(),category:"language".into(),install_path:PathBuf::from(php_root).to_string_lossy().into(),config_path:String::new(),installed_at:1}).unwrap();
                let base=loop {
                    let first=std::net::TcpListener::bind("127.0.0.1:0").unwrap();let base=first.local_addr().unwrap().port();
                    if base>65531 {continue;} let rest=(1..4).map(|i|std::net::TcpListener::bind(("127.0.0.1",base+i))).collect::<std::io::Result<Vec<_>>>();
                    if rest.is_ok(){break base;}
                };
                state.store.set_port_assign("php@8.4.26",base).unwrap();
                std::fs::write(project.join("index.php"),"<?php echo 'php-front';").unwrap();
                site.runtime.kind=SiteKind::Php;site.runtime.php_version=Some("8.4.26".into());
                site=crate::sites::update(&site,&state.paths,&state.store,&state.manager).unwrap_or_else(|error| panic!("{server}: {error:?}; service logs: {:?}", state.tail_logs_checked(server,40).unwrap()));
                assert_eq!(client.get(format!("{address}/index.php")).send().unwrap().text().unwrap(),"php-front","{server} PHP page");
                let payload:serde_json::Value=client.get(format!("{address}/api/users.php")).send().unwrap().json().unwrap();assert_eq!(payload["uri"],"/v1/users.php","{server} proxy before PHP");
                site.runtime.kind=SiteKind::Static;
                site=crate::sites::update(&site,&state.paths,&state.store,&state.manager).unwrap();
                assert_eq!(client.get(&address).send().unwrap().text().unwrap(),"static-front","{server}: returning to static restores HTML index");
                assert_eq!(client.get(format!("{address}/index.php")).send().unwrap().status().as_u16(),403,"{server}: returning to static blocks PHP source");
            }
            site.runtime.kind=SiteKind::ReverseProxy;site.runtime.proxy_target=Some(format!("{source_origin}/default/"));
            site=crate::sites::update(&site,&state.paths,&state.store,&state.manager).unwrap_or_else(|error| panic!("{server}: {error:?}; service logs: {:?}", state.tail_logs_checked(server,40).unwrap()));
            for (path,expected) in [("/home","/default/home"),("/api/users","/v1/users")]{
                let response=client.get(format!("{address}{path}")).send().unwrap();assert_eq!(response.status().as_u16(),200,"{server} proxy coexistence");
                let payload:serde_json::Value=response.json().unwrap();assert_eq!(payload["uri"],expected,"{server}");
            }
            let backup=temp.path().join("routes.json");crate::transfer::export_to(&state.store,&backup).unwrap();
            let imported_paths=crate::paths::Paths::new(temp.path().join("imported"));imported_paths.ensure_dirs().unwrap();
            let imported_store=crate::store::Store::open(imported_paths.db()).unwrap();
            crate::tls::issue_site_cert(&imported_paths,&imported_store,&site.domains).unwrap();
            crate::transfer::import_from(&backup,&imported_paths,&imported_store,&Arc::new(crate::services::ServiceManager::new())).unwrap();
            assert_eq!(imported_store.list_sites().unwrap()[0].runtime.proxy_rules.len(),6);
            site.runtime.proxy_rules.clear();
            crate::sites::update(&site,&state.paths,&state.store,&state.manager).unwrap_or_else(|error| panic!("{server}: {error:?}; service logs: {:?}", state.tail_logs_checked(server,40).unwrap()));
            let payload:serde_json::Value=client.get(format!("{address}/api/users")).send().unwrap().json().unwrap();assert_eq!(payload["uri"],"/default/api/users");
            let pids=state.manager.snapshot(server).unwrap().pids;state.stop_service(server).unwrap();assert!(pids.iter().all(|pid|!platform::process_alive(*pid)));
            println!("{server}: path boundaries, longest match, URI/query/body, HTTPS, scoped redirects, WebSocket, CORS, proxy coexistence, invalid update, backup and removal passed");
        }
    }
}
