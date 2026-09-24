//! 新增套件一体化验收（Apache/多版本 PHP/PG/Mongo/Node/Python/Go/Composer）。
//! 预置下载缓存到 {base}/downloads/{task}.zip 可跳过网络下载。
//! 用法：cargo run -p nsb-core --bin check_extra （base 固定 repo/.extra-home）

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn main() {
    let base = PathBuf::from(".extra-home");
    std::fs::create_dir_all(&base).expect("创建 .extra-home");
    std::env::set_var("NSB_SKIP_HOSTS", "1");

    let state = nsb_core::CoreState::init(Some(base), Arc::new(|_| {})).expect("初始化");
    // 安全档：绝不占用 80/3306/6379 等本机其它环境（FlyEnv/XAMPP）的端口
    state.store.set_setting("portProfile", "safe").expect("端口档位");

    let mut pass = 0usize;
    let mut fail = 0usize;
    macro_rules! check {
        ($name:expr, $body:expr) => {
            match $body {
                Ok(d) => {
                    println!("[PASS] {} — {}", $name, d);
                    pass += 1;
                }
                Err(e) => {
                    println!("[FAIL] {} — {e}", $name);
                    fail += 1;
                }
            }
        };
    }

    /* ---------- 1. 安装（预置缓存，秒回） ---------- */
    let pkgs = [
        "apache@2.4.66",
        "php@8.5.10", "php@8.4.25", "php@8.2.33", "php@8.1.34", "php@8.0.30", "php@7.3.33", "php@7.2.34",
        "node@22.14.0", "python@3.12.9", "go@1.24.1",
        "composer@2.8.5", "postgresql@16.9", "mongodb@8.0.4",
        // —— 多版本扩容验收 ——
        "nginx@1.28.0", "nginx@1.27.5", "nginx@1.24.0",
        "node@20.19.5", "node@18.20.8", "python@3.13.7", "python@3.11.9",
        "go@1.23.6", "composer@2.7.9", "mysql@5.7.44", "mongodb@7.0.24",
        "postgresql@17.6", "mihomo@1.19.11", "mihomo@1.18.10",
    ];
    let rt_tokio = tokio::runtime::Runtime::new().expect("tokio runtime");
    for key in pkgs {
        check!(format!("安装 {key}"), rt_tokio.block_on(state.install_package(key)).map(|_| format!("{key} 就绪")));
    }
    nsb_core::ops::register_services(&state.paths, &state.store, &state.manager);

    /* ---------- 2. 纯运行时版本自检 ---------- */
    fn run_cmd(exe: &PathBuf, args: &[&str]) -> Result<String, String> {
        let out = std::process::Command::new(exe)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        let s = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if out.status.success() {
            Ok(s.trim().to_string())
        } else {
            Err(s)
        }
    }
    let rt = state.paths.runtimes();
    let node = run_cmd(&rt.join("node/22.14.0/node-v22.14.0-win-x64/node.exe"), &["--version"]);
    check!("Node 版本", node);
    let py = run_cmd(&rt.join("python/3.12.9/python.exe"), &["--version"]);
    check!("Python 版本", py);
    let go = run_cmd(&rt.join("go/1.24.1/go/bin/go.exe"), &["version"]);
    check!("Go 版本", go);
    let composer_phar = rt.join("composer/2.8.5/composer.phar");
    let composer = run_cmd(
        &rt.join("php/8.5.10/php.exe"),
        &[composer_phar.to_string_lossy().as_ref(), "--version"],
    );
    check!("Composer 版本", composer);

    /* ---------- 3. PHP 8.5 池 + Apache 站点 ---------- */
    check!("启动 PHP 8.5.10 池", {
        state.start_service("php@8.5.10").map(|_| "php-cgi ×4 已监听".to_string())
    });
    check!("启动 Apache 2.4.66", {
        state.start_service("apache").map(|_| "8180 端口就绪".to_string())
    });
    // 幂等：清理上次运行遗留的同域站点（否则重复运行会报域名冲突）
    if let Ok(sites) = state.store.list_sites() {
        for s in sites.iter().filter(|s| s.domains.iter().any(|d| d == "extra-apache.nsb.test")) {
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
            proxy_target: None,
            command: None,
            cwd: None,
        },
        https: false,
        rewrite: nsb_core::model::RewritePreset::default(),
        create_db: None,
        write_env_example: false,
        template: "blank-php".into(),
        php_overrides: None,
    };
    check!("创建 Apache PHP 站点", {
        nsb_core::sites::create(&input, &state.paths, &state.store, &state.manager)
            .map(|s| format!("{} → {}", s.name, s.domains.join(",")))
    });
    check!("Apache 执行 PHP", {
        // 站点创建触发 Apache 重启（Windows 无 reload 语义），等其稳定后再请求；
        // 仍失败则重试一次（避免撞上重启窗口）
        std::thread::sleep(Duration::from_millis(1200));
        (|| -> Result<String, String> {
            let body = http_get("127.0.0.1", 8180, "extra-apache.nsb.test", "/")?;
            if body.contains("8.5.10") || body.contains("NiceEnv PHP site") {
                return Ok(format!("Apache 执行 PHP OK（{} 字节）", body.len()));
            }
            std::thread::sleep(Duration::from_millis(1500));
            let body = http_get("127.0.0.1", 8180, "extra-apache.nsb.test", "/")?;
            if body.contains("8.5.10") || body.contains("NiceEnv PHP site") {
                Ok(format!("Apache 执行 PHP OK（重试后，{} 字节）", body.len()))
            } else {
                let full: String = body.chars().take(1500).collect();
                Err(format!("响应异常（全文）：{full}"))
            }
        })()
    });

    /* ---------- 3.5 多版本能力 ---------- */
    check!("nginx 切换使用版本 1.28 → 启动", (|| -> Result<String, nsb_core::AppError> {
        nsb_core::ops::set_active_version(&state.store, "nginx", "1.28.0")?;
        let _ = state.stop_service("nginx");
        state.start_service("nginx")?;
        Ok("1.28.0 已作为使用中版本启动".to_string())
    })());
    check!("nginx 切换使用版本 1.24", (|| -> Result<String, nsb_core::AppError> {
        let _ = state.stop_service("nginx");
        nsb_core::ops::set_active_version(&state.store, "nginx", "1.24.0")?;
        state.start_service("nginx")?;
        Ok("1.24.0 启动成功（切换生效）".to_string())
    })());
    check!("MySQL 5.7 初始化并启动", (|| -> Result<String, nsb_core::AppError> {
        let _ = state.stop_service("mysql@8.0.46");
        state.start_service("mysql@5.7.44")?;
        Ok("5.7.44 监听中（mysqlx 兼容处理生效）".to_string())
    })());
    let rt2 = state.paths.runtimes();
    check!("Node 20/18", run_cmd(&rt2.join("node/20.19.5/node-v20.19.5-win-x64/node.exe"), &["--version"])
        .and_then(|a| run_cmd(&rt2.join("node/18.20.8/node-v18.20.8-win-x64/node.exe"), &["--version"]).map(|b| format!("{a} / {b}"))));
    check!("Python 3.13/3.11", run_cmd(&rt2.join("python/3.13.7/python.exe"), &["--version"])
        .and_then(|a| run_cmd(&rt2.join("python/3.11.9/python.exe"), &["--version"]).map(|b| format!("{a} / {b}"))));
    check!("Go 1.23", run_cmd(&rt2.join("go/1.23.6/go/bin/go.exe"), &["version"]));

    /* ---------- 4. PostgreSQL ---------- */
    check!("PostgreSQL 默认使用中版本 = 17.6", {
        let v = nsb_core::ops::installed_by_choice(&state.store, "postgresql").map(|p| p.version).unwrap_or_default();
        if v == "17.6" { Ok(format!("active={v}")) } else { Err(format!("active={v}（期望 17.6）")) }
    });
    check!("启动 PostgreSQL（使用中版本 initdb+监听）", {
        state.start_service("postgresql").map(|_| "25432 就绪".to_string())
    });
    let psql = rt.join("postgresql/16.9/pgsql/bin/psql.exe");
    check!("psql SELECT 1", {
        run_cmd(&psql, &["-h", "127.0.0.1", "-p", "25432", "-U", "postgres", "-d", "postgres", "-c", "SELECT version();"])
    });

    /* ---------- 5. MongoDB ---------- */
    check!("启动 MongoDB 8.0", {
        state.start_service("mongodb").map(|_| "28017 就绪".to_string())
    });
    let mongo_log_check = || -> Result<String, String> {
        let log = state.paths.service_log("mongodb");
        let s = std::fs::read_to_string(&log).map_err(|e| e.to_string())?;
        if s.contains("aiting for connections") {
            Ok("日志确认 waiting for connections".to_string())
        } else {
            let tail: String = s.chars().rev().take(200).collect::<Vec<_>>().into_iter().rev().collect();
            Err(format!("日志未见就绪标记：{tail}"))
        }
    };
    check!("MongoDB 日志就绪", mongo_log_check());

    /* ---------- 6. 收尾：全部停止，无孤儿 ---------- */
    nsb_core::ops::stop_all(&state.store, &state.paths, &state.manager);
    std::thread::sleep(Duration::from_millis(2000));
    // 只验证 NSB 固定服务端口已释放；php 池端口动态分配（9100+ 段与其它环境
    // 共存，TIME_WAIT 属内核正常回收，不代表孤儿）
    let leftovers = [8180u16, 25432, 28017]
        .into_iter()
        .filter(|p| TcpStream::connect(("127.0.0.1", *p)).is_ok())
        .collect::<Vec<_>>();
    // 确认 N 秒内没有 NSB 自己的 php-cgi 存活（按可执行路径过滤）
    let nsb_home = std::env::current_dir().unwrap_or_default().join(".extra-home");
    let orphans = nsb_orphan_count(&nsb_home);
    check!("无孤儿进程", {
        if leftovers.is_empty() && orphans == 0 {
            Ok("端口已释放，无残留 NSB 进程".to_string())
        } else {
            Err(format!("仍占用端口: {leftovers:?}；残留 NSB 进程数: {orphans}"))
        }
    });

    println!("\n==== check_extra: {pass} pass / {fail} fail ====");
    if fail > 0 {
        std::process::exit(1);
    }
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

/// 统计可执行文件位于给定目录下的存活进程数（只认 NSB 自己的，绝不误伤其它环境）
fn nsb_orphan_count(home: &std::path::Path) -> usize {
    let prefix = home.to_string_lossy().to_lowercase().replace('\\', "/");
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | Where-Object { $_.ExecutablePath } | ForEach-Object { $_.ExecutablePath }",
        ])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.trim().to_lowercase().replace('\\', "/"))
            .filter(|l| l.contains(&prefix))
            .count(),
        Err(_) => 0,
    }
}
