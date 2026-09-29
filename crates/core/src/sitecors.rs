//! 站点级 CORS：显式来源白名单、双 Web 服务响应头和预检处理。
use crate::{error::{AppError, Result}, model::SiteCors};
use sha2::{Digest, Sha256};

pub const METHODS: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];
pub const HEADERS: &[&str] = &["Access-Control-Allow-Origin", "Access-Control-Allow-Credentials",
    "Access-Control-Allow-Methods", "Access-Control-Allow-Headers", "Access-Control-Expose-Headers", "Access-Control-Max-Age"];

pub fn normalize(cors: &SiteCors) -> Result<SiteCors> {
    let invalid = |message| AppError::new("BAD_CORS", message);
    if cors.origins.is_empty() || cors.origins.len() > 32 {
        return Err(invalid("请添加 1–32 个允许访问的来源"));
    }
    let mut normalized = cors.clone();
    normalized.origins.clear();
    for origin in &cors.origins {
        let raw = origin.trim();
        let value = if raw == "*" { "*".into() } else {
            if raw.len() > 2048 || raw.chars().any(|c| c.is_control() || c.is_whitespace())
                || !(raw.to_ascii_lowercase().starts_with("https://") || raw.to_ascii_lowercase().starts_with("http://")) {
                return Err(invalid("来源必须是完整的 HTTP/HTTPS 地址，不含路径、参数或账号"));
            }
            let url = reqwest::Url::parse(raw).map_err(|_| invalid("跨域来源地址无效"))?;
            if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some()
                || url.port() == Some(0) || url.path() != "/" || url.query().is_some() || url.fragment().is_some()
                || raw.contains(['\\', '"', '\'', '$', ';', '{', '}', '<', '>']) {
                return Err(invalid("来源只能包含协议、主机和端口，不含路径、参数或账号"));
            }
            url.origin().ascii_serialization()
        };
        if !normalized.origins.contains(&value) { normalized.origins.push(value); }
    }
    if normalized.origins.iter().any(|origin| origin == "*") && (normalized.origins.len() != 1 || cors.credentials) {
        return Err(invalid("允许所有来源时不能启用 Cookie 凭据，也不能混用具体来源"));
    }
    if cors.methods.is_empty() || cors.methods.len() > METHODS.len() || cors.methods.iter().any(|method| !METHODS.contains(&method.as_str())) {
        return Err(invalid("请选择有效的跨域请求方法"));
    }
    normalized.methods = METHODS.iter().filter(|method| cors.methods.iter().any(|value| value == **method)).map(|method| method.to_string()).collect();
    fn headers(values: &[String]) -> Result<Vec<String>> {
        if values.len() > 64 { return Err(AppError::new("BAD_CORS", "请求头或响应头最多设置 64 项")); }
        let mut out: Vec<String> = Vec::new();
        for value in values {
            let value = value.trim();
            if value.is_empty() || value.len() > 128 || !value.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')) {
                return Err(AppError::new("BAD_CORS", "头名称只能包含字母、数字、短横线或下划线"));
            }
            if !out.iter().any(|current| current.eq_ignore_ascii_case(value)) { out.push(value.into()); }
        }
        Ok(out)
    }
    normalized.allowed_headers = headers(&cors.allowed_headers)?;
    normalized.exposed_headers = headers(&cors.exposed_headers)?;
    if cors.max_age > 86400 { return Err(invalid("预检缓存时间需在 0–86400 秒之间")); }
    Ok(normalized)
}

pub(crate) struct NginxCors {
    pub maps: String,
    pub headers: String,
    pub before_content: String,
}
pub(crate) fn nginx(id: &str, cors: &SiteCors) -> NginxCors {
    let Ok(cors) = normalize(cors) else {
        return NginxCors { maps: String::new(), headers: String::new(), before_content: "    return 400;\n".into() };
    };
    let prefix = format!("nsb_cors_{}", &hex::encode(Sha256::digest(id.as_bytes()))[..16]);
    let mut maps = format!("map $http_origin ${prefix}_origin {{\n    default \"\";\n");
    if cors.origins == ["*"] {
        maps = format!("map $http_origin ${prefix}_origin {{\n    default \"*\";\n    \"\" \"\";\n");
    } else {
        for origin in &cors.origins { maps.push_str(&format!("    \"{origin}\" $http_origin;\n")); }
    }
    maps.push_str("}\n");
    let values = [("credentials", if cors.credentials { "true".into() } else { String::new() }),
        ("methods", cors.methods.join(", ")), ("allowed", cors.allowed_headers.join(", ")),
        ("exposed", cors.exposed_headers.join(", ")), ("age", cors.max_age.to_string())];
    for (key, value) in values {
        maps.push_str(&format!("map ${prefix}_origin ${prefix}_{key} {{\n    \"\" \"\";\n    default \"{value}\";\n}}\n"));
    }
    maps.push_str(&format!("map \"$request_method:${prefix}_origin:$http_access_control_request_method\" ${prefix}_preflight {{\n    default 0;\n    \"~^OPTIONS:.+:(?:{})$\" 1;\n    \"~^OPTIONS:.*:.+$\" 2;\n}}\n", cors.methods.join("|")));
    let mut headers = String::new();
    for (header, key) in HEADERS.iter().zip(["origin", "credentials", "methods", "allowed", "exposed", "age"]) {
        headers.push_str(&format!("    add_header {header} ${prefix}_{key} always;\n"));
    }
    headers.push_str("    add_header Vary \"Origin\" always;\n");
    let mut before_content = format!("    if (${prefix}_preflight = 2) {{ return 403; }}\n    if (${prefix}_preflight = 1) {{ return 204; }}\n");
    for header in HEADERS {
        before_content.push_str(&format!("    proxy_hide_header {header};\n    fastcgi_hide_header {header};\n"));
    }
    NginxCors { maps, headers, before_content }
}

pub(crate) fn apache(cors: &SiteCors) -> String {
    let Ok(cors) = normalize(cors) else { return "    RewriteEngine On\n    RewriteRule ^ - [F,L]\n".into(); };
    let origins = if cors.origins == ["*"] { ".+".into() } else { cors.origins.iter().map(|origin| regex::escape(origin)).collect::<Vec<_>>().join("|") };
    let pattern = format!("^(?:{origins})$");
    let condition = format!("expr=req('Origin') =~ m#{pattern}#");
    let mut directives = String::new();
    // 两个头表都清理，避免 FastCGI/代理返回的 CORS 头与托管策略重复或绕过白名单。
    for header in HEADERS {
        directives.push_str(&format!("    Header onsuccess unset {header}\n    Header always unset {header}\n"));
    }
    let origin_value = if cors.origins == ["*"] { "*".into() } else { "expr=%{req:Origin}".into() };
    for (header, value) in HEADERS.iter().zip([origin_value, if cors.credentials { "true".into() } else { String::new() },
        cors.methods.join(", "), cors.allowed_headers.join(", "), cors.exposed_headers.join(", "), cors.max_age.to_string()]) {
        if !value.is_empty() { directives.push_str(&format!("    Header always set {header} \"{value}\" \"{condition}\"\n")); }
    }
    directives.push_str("    Header always merge Vary Origin\n    RewriteEngine On\n");
    directives.push_str(&format!("    RewriteCond %{{REQUEST_METHOD}} =OPTIONS\n    RewriteCond %{{HTTP:Origin}} \"{pattern}\"\n    RewriteCond %{{HTTP:Access-Control-Request-Method}} \"^(?:{})$\"\n    RewriteRule ^ - [R=204,L]\n", cors.methods.join("|")));
    directives.push_str("    RewriteCond %{REQUEST_METHOD} =OPTIONS\n    RewriteCond %{HTTP:Access-Control-Request-Method} !^$\n    RewriteRule ^ - [F,L]\n");
    directives
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::{Read, Write}, path::PathBuf, sync::{Arc, atomic::{AtomicBool, Ordering}}};

    fn settings(origin: &str) -> SiteCors {
        SiteCors { origins: vec![origin.into()], methods: vec!["GET".into(), "HEAD".into(), "POST".into(), "PUT".into()],
            allowed_headers: vec!["Content-Type".into(), "Authorization".into()], exposed_headers: vec!["X-Total-Count".into()],
            credentials: true, max_age: 600 }
    }

    #[test]
    fn cors_rejects_invalid_policy_and_normalizes_origins() {
        let mut cors = settings("HTTPS://EXAMPLE.test:443/");
        cors.origins.push("https://example.test".into());
        cors.allowed_headers.push("authorization".into());
        let result = normalize(&cors).unwrap();
        assert_eq!(result.origins, ["https://example.test"]);
        assert_eq!(result.allowed_headers, ["Content-Type", "Authorization"]);
        for origin in ["", "null", "https:example.test", "ftp://example.test", "https://example.test/path",
            "https://example.test/?x=1", "https://example.test/#", "https://user:pass@example.test", "https://example.test:0",
            "https://example.test\\evil", "https://example.test\nHeader", "*"] {
            assert!(normalize(&settings(origin)).is_err(), "{origin}");
        }
        let mut wildcard = settings("*"); wildcard.credentials = false; assert!(normalize(&wildcard).is_ok());
        wildcard.origins.push("https://example.test".into()); assert!(normalize(&wildcard).is_err());
        let mut invalid = settings("http://[::1]:3000");
        assert!(normalize(&invalid).is_ok());
        invalid.methods = vec!["TRACE".into()]; assert!(normalize(&invalid).is_err());
        invalid.methods = vec!["GET".into()]; invalid.allowed_headers.push("X-Test: value".into());
        assert!(normalize(&invalid).is_err());
        invalid.allowed_headers.clear(); invalid.max_age = 86401; assert!(normalize(&invalid).is_err());
        let old: crate::model::SiteRuntime = serde_json::from_value(serde_json::json!({"kind":"static","webServer":"nginx"})).unwrap();
        assert!(old.cors.is_none());
    }

    #[test]
    #[ignore = "requires NSB_NGINX_ROOT, NSB_APACHE_ROOT and NSB_SKIP_HOSTS=1; isolated CORS HTTP/browser checks"]
    fn cors_native_nginx_and_apache() {
        assert_eq!(std::env::var("NSB_SKIP_HOSTS").as_deref(), Ok("1"));
        let source = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let source_port = source.local_addr().unwrap().port();
        let source_origin = format!("http://127.0.0.1:{source_port}");
        source.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false)); let stopping = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = source.accept() else { std::thread::sleep(std::time::Duration::from_millis(10)); continue; };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(1))).unwrap();
                let mut request = Vec::new(); let mut chunk = [0; 4096];
                // 读完头和请求体再关闭连接，避免拆包或 PUT 请求留下未读数据而触发 TCP reset。
                while request.len() < 65536 {
                    let length = stream.read(&mut chunk).unwrap_or(0); if length == 0 { break; }
                    request.extend_from_slice(&chunk[..length]);
                    if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let body_length = headers.lines().filter_map(|line| line.split_once(':'))
                            .find(|(key, _)| key.eq_ignore_ascii_case("Content-Length"))
                            .and_then(|(_, value)| value.trim().parse::<usize>().ok()).unwrap_or(0);
                        if request.len() >= end + 4 + body_length { break; }
                    }
                }
                if request.is_empty() { continue; }
                let request = String::from_utf8_lossy(&request);
                let status = if request.contains(" /failure") { "418 I'm a teapot" } else { "200 OK" };
                let body = "<!doctype html><title>CORS native fixture</title>cors-native";
                let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Credentials: true\r\nAccess-Control-Allow-Headers: X-Unsafe\r\nX-Total-Count: 42\r\nVary: Accept-Encoding\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        struct Source(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
        impl Drop for Source { fn drop(&mut self) { self.0.store(true, Ordering::Relaxed); let _ = self.1.take().unwrap().join(); } }
        let _source = Source(stop, Some(worker));
        for (server, version, variable, port_key, tls_key) in [
            ("nginx", "1.28.1", "NSB_NGINX_ROOT", "http", "https"),
            ("apache", "2.4.66", "NSB_APACHE_ROOT", "apacheHttp", "apacheHttps"),
        ] {
            if std::env::var("NSB_CORS_SERVER").is_ok_and(|selected| selected != server) { continue; }
            let root = PathBuf::from(std::env::var(variable).expect(variable));
            let temp = tempfile::tempdir().unwrap();
            let state = crate::CoreState::init(Some(temp.path().join("cors native with spaces")), Arc::new(|_| {})).unwrap();
            let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let https = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = http.local_addr().unwrap().port();
            let tls_port = https.local_addr().unwrap().port();
            state.store.set_port_override(port_key, Some(port)).unwrap();
            state.store.set_port_override(tls_key, Some(tls_port)).unwrap();
            state.store.upsert_installed(&crate::model::InstalledPackage { id: server.into(), version: version.into(),
                category: "web-server".into(), install_path: root.parent().unwrap().to_string_lossy().into(), config_path: String::new(), installed_at: 1 }).unwrap();
            struct Cleanup(Arc<crate::CoreState>, &'static str);
            impl Drop for Cleanup { fn drop(&mut self) { let _ = self.0.stop_service(self.1); } }
            let _cleanup = Cleanup(state.clone(), server);
            let project = state.paths.base.join("project"); std::fs::create_dir_all(&project).unwrap();
            std::fs::write(project.join("index.html"), "static-cors").unwrap();
            let creation: crate::model::CreateSiteInput = serde_json::from_value(serde_json::json!({
                "name":"CORS fixture", "domains":["127.0.0.1","cors.test"], "rootDir":project,
                "runtime":{"kind":"static","webServer":server,"cors":settings(&source_origin)},
                "https":true, "rewrite":"none", "template":"none","writeEnvExample":false
            })).unwrap();
            drop((http, https));
            let mut site = crate::sites::create(&creation, &state.paths, &state.store, &state.manager).unwrap();
            let client = reqwest::blocking::Client::builder().no_proxy().danger_accept_invalid_certs(true)
                .redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(4)).build().unwrap();
            let address = format!("http://127.0.0.1:{port}");
            let assert_headers = |response: &reqwest::blocking::Response, origin: Option<&str>, credentials: bool| {
                let h = response.headers();
                assert_eq!(h.get_all("access-control-allow-origin").iter().count(), usize::from(origin.is_some()), "{server} duplicate or missing origin: {h:?}");
                assert_eq!(h.get("access-control-allow-origin").map(|v| v.to_str().unwrap()), origin, "{server}");
                assert_eq!(h.get("access-control-allow-credentials").map(|v| v.to_str().unwrap()), credentials.then_some("true"), "{server}");
                assert!(h.get_all("vary").iter().any(|v| v.to_str().unwrap().split(',').any(|s| s.trim().eq_ignore_ascii_case("Origin"))), "{server} cache partition");
            };
            for (resource, status) in [("/", 200), ("/missing", 404)] {
                let response = client.get(format!("{address}{resource}")).header("Origin", &source_origin).send().unwrap();
                assert_eq!(response.status().as_u16(), status, "{server}"); assert_headers(&response, Some(&source_origin), true);
            }
            let response = client.get(format!("https://127.0.0.1:{tls_port}/")).header("Origin", &source_origin).send().unwrap();
            assert_eq!(response.status().as_u16(), 200); assert_headers(&response, Some(&source_origin), true);
            for origin in [None, Some("http://untrusted.test"), Some("null"), Some("http://127.0.0.1:1")] {
                let mut request = client.get(&address); if let Some(origin) = origin { request = request.header("Origin", origin); }
                assert_headers(&request.send().unwrap(), None, false);
            }
            let preflight = |origin: &str, method: &str| client.request(reqwest::Method::OPTIONS, format!("{address}/missing"))
                .header("Origin", origin).header("Access-Control-Request-Method", method).header("Access-Control-Request-Headers", "authorization,content-type").send().unwrap();
            let response = preflight(&source_origin, "PUT"); assert_eq!(response.status().as_u16(), 204, "{server} preflight");
            assert_headers(&response, Some(&source_origin), true);
            assert_eq!(response.headers()["access-control-max-age"], "600");
            assert!(response.headers()["access-control-allow-headers"].to_str().unwrap().contains("Authorization"));
            assert_eq!(preflight(&source_origin, "TRACE").status().as_u16(), 403);
            let response = preflight("https://untrusted.test", "PUT"); assert_eq!(response.status().as_u16(), 403); assert_headers(&response, None, false);
            assert_ne!(client.request(reqwest::Method::OPTIONS, &address).send().unwrap().status().as_u16(), 204);
            if server == "nginx" {
                site.runtime.custom_rewrite = Some(crate::model::CustomRewrite { name:"custom headers".into(), server:server.into(),
                    content:"location / { add_header X-Custom keep always; add_header Access-Control-Allow-Origin *; try_files $uri $uri/ =404; }".into() });
                site = crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
                let response = client.get(&address).header("Origin", &source_origin).send().unwrap();
                assert_eq!(response.headers()["x-custom"], "keep"); assert_headers(&response, Some(&source_origin), true);
                site.runtime.custom_rewrite.as_mut().unwrap().content.push_str(&format!("\nlocation = /proxied {{ proxy_pass {source_origin}; proxy_hide_header X-Total-Count; }}\n"));
                site = crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
                let response = client.get(format!("{address}/proxied")).header("Origin", &source_origin).send().unwrap();
                assert_eq!(response.status().as_u16(), 200); assert!(response.headers().get("x-total-count").is_none());
                assert_headers(&response, Some(&source_origin), true);
            }
            site.runtime.custom_rewrite = None; site.runtime.kind = crate::model::SiteKind::Redirect;
            site.runtime.redirect = Some(crate::model::SiteRedirect { target: "https://destination.test/".into(), status: 307, preserve_path: true });
            site = crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
            let response = client.get(format!("{address}/resource?cors=1")).header("Origin", &source_origin).send().unwrap();
            assert_eq!(response.status().as_u16(), 307, "{server} redirect");
            assert_eq!(response.headers()["location"], "https://destination.test/resource?cors=1");
            assert_headers(&response, Some(&source_origin), true);
            assert_eq!(preflight(&source_origin, "PUT").status().as_u16(), 204, "{server} preflight before redirect");
            site.runtime.kind = crate::model::SiteKind::ReverseProxy; site.runtime.redirect = None;
            site.root_dir = project.to_string_lossy().into_owned();
            site.runtime.proxy_target = Some(source_origin.clone());
            site = crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
            for (resource, status) in [("/",200),("/failure",418)] {
                let response = client.get(format!("{address}{resource}")).header("Origin", &source_origin).send().unwrap();
                assert_eq!(response.status().as_u16(), status, "{server} {resource}: {:?}", state.tail_logs_checked(&format!("site-error:{}", site.id), 20).unwrap()); assert_headers(&response, Some(&source_origin), true);
                assert_eq!(response.headers()["access-control-expose-headers"], "X-Total-Count");
            }
            assert_headers(&client.get(&address).header("Origin","https://untrusted.test").send().unwrap(), None, false);
            // 可选浏览器验收：仅写入调用者指定的临时握手目录，限时等候，不保持服务常驻。
            if let Some(directory) = std::env::var_os("NSB_CORS_BROWSER") {
                let directory = PathBuf::from(directory); assert!(directory.starts_with(std::env::temp_dir()));
                std::fs::create_dir_all(&directory).unwrap();
                std::fs::write(directory.join(format!("{server}.json")), serde_json::to_vec(&serde_json::json!({"source":source_origin,"target":address})).unwrap()).unwrap();
                let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
                while !directory.join(format!("{server}.done")).is_file() {
                    assert!(std::time::Instant::now() < until, "{server} browser check timeout");
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
            site.runtime.cors.as_mut().unwrap().origins = vec!["*".into()];
            assert_eq!(crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap_err().code, "BAD_CORS");
            assert_headers(&client.get(&address).header("Origin", &source_origin).send().unwrap(), Some(&source_origin), true);
            site.runtime.cors.as_mut().unwrap().credentials = false;
            site = crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
            assert_headers(&client.get(&address).header("Origin","null").send().unwrap(), Some("*"), false);
            let backup = temp.path().join("cors.json"); crate::transfer::export_to(&state.store, &backup).unwrap();
            let imported_paths = crate::paths::Paths::new(temp.path().join("imported")); imported_paths.ensure_dirs().unwrap();
            let imported_store = crate::store::Store::open(imported_paths.db()).unwrap();
            // 配置备份不携带证书；先恢复相同域名的本地证书，再走既有导入流程。
            crate::tls::issue_site_cert(&imported_paths, &imported_store, &site.domains).unwrap();
            crate::transfer::import_from(&backup, &imported_paths, &imported_store, &Arc::new(crate::services::ServiceManager::new())).unwrap();
            assert_eq!(imported_store.list_sites().unwrap()[0].runtime.cors.as_ref().unwrap().origins, ["*"]);
            site.runtime.cors = None;
            crate::sites::update(&site, &state.paths, &state.store, &state.manager).unwrap();
            let response = client.get(&address).header("Origin","https://untrusted.test").send().unwrap();
            assert_eq!(response.headers()["access-control-allow-origin"], "*"); // 关闭后恢复应用自身策略。
            assert_eq!(response.headers()["access-control-allow-headers"], "X-Unsafe");
            let pids = state.manager.snapshot(server).unwrap().pids; state.stop_service(server).unwrap();
            assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
            println!("{server}: real CORS static/proxy/redirect/error/HTTPS headers, preflight, denied origins/methods, custom headers, upstream override, credentials, wildcard, rollback, disable and backup roundtrip passed");
        }
    }
}
