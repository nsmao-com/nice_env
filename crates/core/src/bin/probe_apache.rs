//! 常驻复现：起 php 池 + apache，反复请求站点，打印每次响应
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn main() {
    let base = PathBuf::from(".extra-home");
    std::env::set_var("NSB_SKIP_HOSTS", "1");
    let state = nsb_core::CoreState::init(Some(base), Arc::new(|_| {})).expect("init");
    state.store.set_setting("portProfile", "safe").expect("safe");

    state.start_service("php@8.5.10").expect("php pool");
    state.start_service("apache").expect("apache");
    // 复刻验收路径：确认当前 use 的 vhost 与 DocumentRoot
    println!("— vhost 文件:");
    for e in std::fs::read_dir(state.paths.apache_sites_dir()).into_iter().flatten().flatten() {
        let p = e.path();
        println!("   {} ({:?})", p.file_name().unwrap_or_default().to_string_lossy(), std::fs::metadata(&p).map(|m| m.len()));
    }
    // 复刻验收的幂等清理：删掉同域站点再重建
    if let Ok(sites) = state.store.list_sites() {
        for s in sites.iter().filter(|s| s.domains.iter().any(|d| d == "extra-apache.nsb.test")) {
            println!("— 删除旧站点 {}", s.id);
            let _ = nsb_core::sites::delete(&s.id, true, true, &state.paths, &state.store, &state.manager);
        }
    }
    let input = nsb_core::model::CreateSiteInput {
        name: "extra-apache".into(),
        domains: vec!["extra-apache.nsb.test".into()],
        root_dir: std::env::current_dir().unwrap().join(".extra-home/site-apache").to_string_lossy().to_string(),
        runtime: nsb_core::model::SiteRuntime {
            web_server: "apache".into(),
            kind: nsb_core::model::SiteKind::Php,
            php_version: Some("8.5.10".into()),
            proxy_target: None, command: None, cwd: None,
        },
        https: false,
        rewrite: nsb_core::model::RewritePreset::default(),
        create_db: None,
        write_env_example: false,
        template: "blank-php".into(),
        php_overrides: None,
    };
    match nsb_core::sites::create(&input, &state.paths, &state.store, &state.manager) {
        Ok(s) => println!("— 重建站点 {}", s.id),
        Err(e) => println!("— 重建失败: {e}"),
    }
    println!("— rebuild 后 vhost:");
    for e in std::fs::read_dir(state.paths.apache_sites_dir()).into_iter().flatten().flatten() {
        let p = e.path();
        println!("   {} ({:?})", p.file_name().unwrap_or_default().to_string_lossy(), std::fs::metadata(&p).map(|m| m.len()));
    }
    println!("— 服务已启动，端口池 = {:?}", state.store.get_port_assign("php@8.5.10"));

    for i in 1..=3 {
        std::thread::sleep(Duration::from_millis(600));
        match http_get("127.0.0.1", 8180, "extra-apache.nsb.test", "/") {
            Ok(b) => {
                let head: String = b.chars().take(200).collect();
                println!("--- 请求 #{i} ---\n{head}\n");
            }
            Err(e) => println!("--- 请求 #{i} 失败: {e}\n"),
        }
    }
    println!("== Apache error.log 末尾 ==");
    if let Ok(s) = std::fs::read_to_string(".extra-home/etc/apache/logs/error.log") {
        let tail: String = s.chars().rev().take(900).collect::<Vec<_>>().into_iter().rev().collect();
        println!("{tail}");
    }
    println!("== php 池日志末尾 ==");
    if let Ok(s) = std::fs::read_to_string(".extra-home/logs/php_8.5.10/out.log") {
        let tail: String = s.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect();
        println!("{tail}");
    }
    nsb_core::ops::stop_all(&state.store, &state.paths, &state.manager);
}

fn http_get(host: &str, port: u16, host_header: &str, path: &str) -> Result<String, String> {
    let mut s = TcpStream::connect((host, port)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(8))).ok();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}
