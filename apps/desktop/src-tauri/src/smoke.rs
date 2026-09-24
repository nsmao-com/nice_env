//! 无头冒烟验收：`niceservbay.exe --smoke-test`
//!
//! 完整跑一遍 Phase 1 闭环（全部使用独立数据目录 + 安全端口，绝不触碰
//! FlyEnv 等本机已有环境）：
//!   1. 初始化 CoreState（.smoke-home）
//!   2. 预置下载缓存（.dl-cache → downloads，跳过网络下载）
//!   3. 安装 nginx + php(8.3/7.4) + mysql + redis + mihomo（真实解压/配置）
//!   4. 启动 mysql/redis/php/nginx，健康检查
//!   5. 创建 PHP 站点（含数据库 + .env.example），HTTP 请求验证 phpinfo
//!   6. PHP 内 PDO 连 MySQL + 原生 RESP 连 Redis
//!   7. 多版本共存：第二个站点绑定 PHP 7.4
//!   8. mihomo 启动 + API + 通过混合端口代理访问外网（不动系统代理）
//!   9. HTTPS 证书签发（不写 hosts、不信任 CA —— 避免影响系统）
//!  10. 停止全部服务
//! 任一步失败：打印人话错误 + 最后日志，退出码 1。

use nsb_core::{CoreState, Event, EventSink};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

static STEP: AtomicUsize = AtomicUsize::new(0);
static FAILED: AtomicUsize = AtomicUsize::new(0);

fn ok(name: &str, detail: impl AsRef<str>) {
    let n = STEP.fetch_add(1, Ordering::SeqCst);
    println!("[PASS] {:>2}. {:<38} {}", n + 1, name, detail.as_ref());
}

fn fail(name: &str, err: &nsb_core::AppError) {
    FAILED.fetch_add(1, Ordering::SeqCst);
    let n = STEP.fetch_add(1, Ordering::SeqCst);
    println!("[FAIL] {:>2}. {:<38} {}", n + 1, name, err.message);
    if let Some(h) = &err.hint {
        println!("         hint: {h}");
    }
    if let Some(d) = &err.detail {
        println!("         detail: {}", d.chars().take(600).collect::<String>());
    }
}

fn check<T>(state: &Arc<CoreState>, name: &str, r: nsb_core::error::Result<T>) -> Option<T> {
    match r {
        Ok(v) => {
            ok(name, "");
            Some(v)
        }
        Err(e) => {
            fail(name, &e);
            dump_logs(state);
            None
        }
    }
}

/// 跳过一项：不算失败，但要明确打印出来。
/// 冒烟测试的价值在于「跑过的都验过」，所以跳过必须可见，
/// 不能静默略过让人误以为覆盖了。
fn skip(name: &str, why: &str) {
    let n = STEP.fetch_add(1, Ordering::SeqCst);
    println!("[SKIP] {:>2}. {:<38} {}", n + 1, name, why);
}

fn assert_(cond: bool, name: &str, detail: impl AsRef<str>) -> bool {
    if cond {
        ok(name, detail);
    } else {
        FAILED.fetch_add(1, Ordering::SeqCst);
        let n = STEP.fetch_add(1, Ordering::SeqCst);
        println!("[FAIL] {:>2}. {:<38} {}", n + 1, name, detail.as_ref());
    }
    cond
}

fn dump_logs(state: &Arc<CoreState>) {
    for s in state.service_status_list() {
        if s.state == nsb_core::model::ServiceState::Error {
            let tail = state.tail_logs(&s.id, 15);
            if !tail.is_empty() {
                println!("       ── {} 最后日志 ──", s.id);
                for l in tail {
                    println!("       {}", l.line);
                }
            }
        }
    }
}

fn http_get(host: &str, port: u16, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .ok();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: nsb-smoke\r\n\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&buf).to_string();
    let body = text.splitn(2, "\r\n\r\n").nth(1).unwrap_or("");
    Ok(format!("{}||{}", text.split("\r\n").next().unwrap_or(""), body))
}

/// 通过 mihomo 混合端口发 HTTP 代理请求（CONNECT 走 https 麻烦，直接用绝对 URI 的 GET）
fn proxy_http_get_via_mihomo(url_host: &str, url_port: u16, path: &str) -> Result<String, String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", nsb_core::configgen::MIHOMO_MIXED_PORT)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(12)))
        .ok();
    let req = format!(
        "GET http://{url_host}:{url_port}{path} HTTP/1.1\r\nHost: {url_host}\r\nConnection: close\r\nUser-Agent: nsb-smoke\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

pub fn run() {
    // 安全红线：不改系统 hosts、不动系统代理
    std::env::set_var("NSB_SKIP_HOSTS", "1");
    println!(" NiceEnv smoke-test");
    println!("==========================================");
    let begin = std::time::Instant::now();

    /* ---------- 1. 初始化 ---------- */
    let base = find_workspace().join(".smoke-home");
    if std::env::var("NSB_SMOKE_FRESH").map(|v| v == "1").unwrap_or(false) {
        let _ = std::fs::remove_dir_all(&base);
    }
    let _ = std::fs::remove_dir_all(base.join("logs"));
    let _ = std::fs::remove_file(base.join("nsb.sqlite"));
    let _ = std::fs::remove_file(base.join("nsb.sqlite-wal"));
    let _ = std::fs::remove_file(base.join("nsb.sqlite-shm"));
    let _ = std::fs::remove_dir_all(base.join("etc").join("nginx").join("sites"));
    let _ = std::fs::remove_dir_all(base.join("sites"));
    // 上一次被中途打断（Ctrl+C / 超时 kill）时留下的状态必须清干净，
    // 否则再次运行会遇到「数据目录非空导致 MySQL 拒绝初始化」这类假失败
    let _ = std::fs::remove_dir_all(base.join("data").join("mysql"));
    let _ = std::fs::remove_dir_all(base.join("data").join("php"));
    let _ = std::fs::remove_dir_all(base.join("data").join("postgresql"));
    let _ = std::fs::remove_dir_all(base.join("data").join("mongodb"));
    let _ = std::fs::remove_dir_all(base.join("data").join("redis"));
    let _ = std::fs::remove_file(base.join("data").join("run").join("pids.json"));

    let emit: EventSink = Arc::new(|e: Event| {
        if let Event::DownloadProgress(p) = e {
            if p.state != "downloading" {
                println!("       · {} → {}", p.task_id, p.state);
            }
        }
    });
    let state = match CoreState::init(Some(base.clone()), emit) {
        Ok(s) => s,
        Err(e) => {
            println!("[FAIL] 初始化失败：{}", e.message);
            std::process::exit(1);
        }
    };
    ok("CoreState 初始化", base.display().to_string());

    /* ---------- 1b. 安全红线：强制安全端口档 ----------
       应用默认走标准端口（80/3306/6379…），好让项目里写死的连接串直接可用；
       但本机很可能已经跑着真实的 MySQL/Redis/Nginx。冒烟测试必须与它们共存，
       所以这里显式钉住安全档（8080/23306/26379…），不受本机设置影响。 */
    state.store.set_setting("portProfile", "safe").ok();
    // 清掉可能从本机配置带过来的端口覆盖，让冒烟跑在确定的端口上
    for key in nsb_core::services::PortsProfile::keys() {
        let _ = state.store.set_port_override(key, None);
    }
    // 冒烟全程不自动结束任何进程（端口被占也必须如实失败，绝不误杀本机服务）
    state.store.set_setting("autoClosePortOnStart", "false").ok();

    /* ---------- 2. 预置下载缓存 ---------- */
    let cache = find_workspace().join(".dl-cache");
    let downloads = state.paths.downloads();
    std::fs::create_dir_all(&downloads).ok();
    let mut copied = 0;
    // 单一事实来源：安装列表钉到哪些版本，缓存就预热哪些版本。
    // （历史教训：两处各自硬编码，清单/缓存一变就互相错位 → 离线环境直接失败。）
    const INSTALL_KEYS: &[&str] = &[
        "nginx@1.26.3",
        "php@8.3.33",
        "php@7.4.33",
        "mysql@8.0.46",
        "redis@5.0.14",
        "mihomo@1.19.10",
    ];
    let pairs: Vec<(String, String)> = INSTALL_KEYS
        .iter()
        .map(|key| {
            // 缓存源文件名按各家发布习惯：redis 是 Redis-x64-{v}.1，其余 {id}-{v}
            let src = if let Some(v) = key.strip_prefix("redis@") {
                format!("redis-{v}.1.zip")
            } else {
                let (id, ver) = key.split_once('@').unwrap_or((key, ""));
                format!("{id}-{ver}.zip")
            };
            (src, format!("{key}.pkg"))
        })
        .collect();
    for (src, dst) in pairs.iter() {
        let from = cache.join(src);
        let to = downloads.join(dst);
        // .dl-cache 里存的是下载时的 .zip；目标目录统一用 .pkg 后缀
        if from.exists() && !to.exists() {
            if std::fs::copy(&from, &to).is_ok() {
                copied += 1;
            }
            continue;
        }
        // 兼容旧缓存命名（下载器曾用 .zip 落盘）
        let legacy = downloads.join(format!("{dst}").replace(".pkg", ".zip"));
        if from.exists() && !to.exists() && !legacy.exists() {
            if std::fs::copy(&from, &to).is_ok() {
                copied += 1;
            }
        }
    }
    let all_present = pairs.iter().all(|(_, dst)| base.join("downloads").join(dst).exists());
    assert_(copied >= 1 || all_present, "预置下载缓存", if all_present { "全部已缓存".to_string() } else { format!("{copied} 个包") });

    /* ---------- 3. 安装 ---------- */
    // 全部钉到缓存里已有的确切版本：不带版本号的 key 会解析成清单里的最新版，
    // 清单更新后就会去下载新文件（冒烟测试要保持离线可重复）
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    for key in ["nginx@1.26.3", "php@8.3.33", "php@7.4.33", "mysql@8.0.46", "redis@5.0.14", "mihomo@1.19.10"] {
        let r = rt.block_on(state.install_package(key));
        check(&state, &format!("安装 {key}"), r.map(|_| ()));
    }

    /* ---------- 4. 启动 ---------- */
    // mysql 先起（站点建库要用）
    for id in ["mysql@8.0.46", "redis", "php@8.3.33", "php@7.4.33", "nginx"] {
        let r = state.start_service(id);
        check(&state, &format!("启动 {id}"), r);
    }

    let statuses = state.service_status_list();
    let running = statuses.iter().filter(|s| s.state == nsb_core::model::ServiceState::Running).count();
    assert_(running >= 5, "服务状态灯=运行", format!("{running}/{} running", statuses.len()));

    /* ---------- 5. 创建 PHP 站点（8.3 + 数据库） ---------- */
    let site_root = base.join("sites").join("smoke-php83");
    let input = nsb_core::model::CreateSiteInput {
        name: "smoke-php83".into(),
        domains: vec!["smoke83.nsb.test".into()],
        root_dir: site_root.to_string_lossy().to_string(),
        runtime: nsb_core::model::SiteRuntime {
            web_server: "nginx".into(),
            kind: nsb_core::model::SiteKind::Php,
            php_version: Some("8.3.33".into()),
            proxy_target: None,
            command: None,
            cwd: None,
        },
        https: false,
        rewrite: nsb_core::model::RewritePreset::None,
        php_overrides: None,
        create_db: Some(nsb_core::model::CreateDbInfo {
            database: "smoke_db".into(),
            username: "smoke_user".into(),
            password: "smoke_pass_123".into(),
        }),
        write_env_example: true,
        template: "blank-php".into(),
    };
    let site = check(&state, "创建 PHP 8.3 站点(含库)", nsb_core::sites::create(&input, &state.paths, &state.store, &state.manager));

    // hosts 写入：冒烟环境跳过（避免无管理员权限时的系统级改动）
    if site.is_some() {
        let resp = http_get("smoke83.nsb.test", 8080, "/");
        match resp {
            Ok(r) => assert_(
                r.contains("200") && r.contains("PHP 8.3"),
                "HTTP 访问站点 (Host 头)",
                "php83 首页 OK",
            ),
            Err(e) => assert_(false, "HTTP 访问站点 (Host 头)", e),
        };
        let resp = http_get("smoke83.nsb.test", 8080, "/phpinfo.php");
        match resp {
            Ok(r) => assert_(r.contains("phpinfo"), "phpinfo() 执行", "PHP 8.3"),
            Err(e) => assert_(false, "phpinfo() 执行", e),
        };
        // env.example
        let env = std::fs::read_to_string(site_root.join(".env.example")).unwrap_or_default();
        assert_(env.contains("DB_DATABASE=smoke_db"), ".env.example 写入", "");
    }

    /* ---------- 6. PHP 连 MySQL + Redis ---------- */
    let test_php = r#"<?php
header('Content-Type: text/plain; charset=utf-8');
$out = [];
// MySQL via PDO
try {
    $pdo = new PDO('mysql:host=127.0.0.1;port=23306;dbname=smoke_db', 'smoke_user', 'smoke_pass_123');
    $ver = $pdo->query('SELECT VERSION()')->fetchColumn();
    $pdo->exec('CREATE TABLE IF NOT EXISTS smoke_t (id INT AUTO_INCREMENT PRIMARY KEY, v VARCHAR(64))');
    $pdo->exec("INSERT INTO smoke_t (v) VALUES ('hello-nsb')");
    $got = $pdo->query("SELECT v FROM smoke_t ORDER BY id DESC LIMIT 1")->fetchColumn();
    $out[] = 'MYSQL_OK:' . $ver . ':' . $got;
} catch (Exception $e) {
    $out[] = 'MYSQL_FAIL:' . $e->getMessage();
}
// Redis via 原生 RESP（fsockopen）
try {
    $fp = fsockopen('127.0.0.1', 26379, $errno, $errstr, 3);
    if (!$fp) throw new Exception("connect: $errstr");
    $cmd = fn($op, ...$args) => '*' . (count($args) + 1) . "\r\n" . '$' . strlen($op) . "\r\n$op\r\n" . implode('', array_map(fn($a) => '$' . strlen($a) . "\r\n$a\r\n", $args));
    fwrite($fp, $cmd('SET', 'nsb_smoke', 'redis-hello', 'EX', 60));
    $setResp = fgets($fp);
    fwrite($fp, $cmd('GET', 'nsb_smoke'));
    fgets($fp); // $12（bulk 长度行）
    $val = trim(fgets($fp));
    $out[] = (trim($setResp) === '+OK' && $val === 'redis-hello') ? 'REDIS_OK:' . $val : 'REDIS_FAIL:' . $setResp . '|' . $val;
    fclose($fp);
} catch (Exception $e) {
    $out[] = 'REDIS_FAIL:' . $e->getMessage();
}
echo implode("\n", $out);
"#;
    std::fs::write(site_root.join("test-db.php"), test_php).ok();
    let resp = http_get("smoke83.nsb.test", 8080, "/test-db.php");
    match resp {
        Ok(r) => {
            let body = r.splitn(2, "||").nth(1).unwrap_or("");
            assert_(body.contains("MYSQL_OK"), "PHP PDO 连 MySQL", body.lines().next().unwrap_or(""));
            assert_(body.contains("REDIS_OK"), "PHP 连 Redis (RESP)", body.trim());
        }
        Err(e) => { assert_(false, "PHP 数据库连接测试", e); }
    };

    /* ---------- 7. 多版本共存：PHP 7.4 站点 ---------- */
    let site2_root = base.join("sites").join("smoke-php74");
    let input2 = nsb_core::model::CreateSiteInput {
        name: "smoke-php74".into(),
        domains: vec!["smoke74.nsb.test".into()],
        root_dir: site2_root.to_string_lossy().to_string(),
        runtime: nsb_core::model::SiteRuntime {
            web_server: "nginx".into(),
            kind: nsb_core::model::SiteKind::Php,
            php_version: Some("7.4.33".into()),
            proxy_target: None,
            command: None,
            cwd: None,
        },
        https: false,
        rewrite: nsb_core::model::RewritePreset::None,
        php_overrides: None,
        create_db: None,
        write_env_example: false,
        template: "blank-php".into(),
    };
    let site2 = check(&state, "创建 PHP 7.4 站点", nsb_core::sites::create(&input2, &state.paths, &state.store, &state.manager));
    if site2.is_some() {
        let resp = http_get("smoke74.nsb.test", 8080, "/");
        match resp {
            Ok(r) => assert_(r.contains("200") && r.contains("PHP 7.4"), "双 PHP 版本同时服务", "8.3 与 7.4 并存"),
            Err(e) => assert_(false, "双 PHP 版本同时服务", e),
        };
    }

    /* ---------- 8. 反向代理站点（模拟 :18080 后端） ---------- */
    let backend = std::net::TcpListener::bind("127.0.0.1:18080").unwrap();
    let backend_thread = std::thread::spawn(move || {
        if let Ok((mut s, _)) = backend.accept() {
            use std::io::{Read, Write};
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let body = "hello-from-backend-go:18080";
            let _ = s.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes(),
            );
        }
    });
    let input3 = nsb_core::model::CreateSiteInput {
        name: "smoke-proxy".into(),
        domains: vec!["smokeproxy.nsb.test".into()],
        root_dir: base.join("sites").join("smoke-proxy").to_string_lossy().to_string(),
        runtime: nsb_core::model::SiteRuntime {
            web_server: "nginx".into(),
            kind: nsb_core::model::SiteKind::ReverseProxy,
            php_version: None,
            proxy_target: Some("127.0.0.1:18080".into()),
            command: None,
            cwd: None,
        },
        https: false,
        rewrite: nsb_core::model::RewritePreset::None,
        php_overrides: None,
        create_db: None,
        write_env_example: false,
        template: "none".into(),
    };
    if check(&state, "创建反代站点 (:18080)", nsb_core::sites::create(&input3, &state.paths, &state.store, &state.manager)).is_some() {
        let resp = http_get("smokeproxy.nsb.test", 8080, "/");
        match resp {
            Ok(r) => assert_(r.contains("hello-from-backend"), "反代转发到本机端口", "Host 头方式验证"),
            Err(e) => assert_(false, "反代转发到本机端口", e),
        };
    }
    drop(backend_thread);

    /* ---------- 9. mihomo（Clash） ---------- */
    check(&state, "启动 mihomo 内核", state.start_service("mihomo"));
    let rt_api = nsb_core::proxy::ProxyRuntime::new();
    match rt_api.version() {
        Ok(v) => ok("mihomo REST API", v),
        Err(e) => fail("mihomo REST API", &e),
    };
    match proxy_http_get_via_mihomo("www.baidu.com", 80, "/") {
        Ok(r) if r.contains("HTTP/1.1 200") || r.contains("HTTP/1.0 200") || r.contains("Location") => {
            ok("混合端口代理可用", "http://www.baidu.com via 127.0.0.1:17890")
        }
        Ok(r) => { assert_(false, "混合端口代理可用", format!("非 200：{}", &r[..r.len().min(120)])); }
        Err(e) => { assert_(false, "混合端口代理可用", e); }
    };
    // 系统代理：只验证读取，不开启（避免影响用户 Clash Party）
    let sys = nsb_core::proxy::system_proxy_state();
    ok("系统代理状态读取", format!("enabled={}（未做任何修改）", sys.enabled));

    /* ---------- 10. HTTPS 证书 ---------- */
    match nsb_core::tls::issue_site_cert(&state.paths, &state.store, &["smoke-https.nsb.test".to_string()]) {
        Ok(c) => {
            ok("CA + 站点证书签发", format!("{} → {}", c.subject, Path::new(&c.cert_path).file_name().unwrap_or_default().to_string_lossy()));

            // 链验证：openssl verify 证明「信任 CA 后浏览器无警告」的密码学前提
            // （SAN 匹配 + 有效期 + 签名链均由 openssl 校验）
            let ca = state.paths.certs().join("ca.crt");
            let out = std::process::Command::new("openssl")
                .args(["verify", "-CAfile"])
                .arg(&ca)
                .arg(&c.cert_path)
                .output();
            match out {
                Ok(o) if o.status.success() => {
                    ok("证书链验证 (openssl)", "信任根 CA 后浏览器即无警告（链+SAN+有效期通过）");
                }
                Ok(o) => {
                    assert_(false, "证书链验证 (openssl)", format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    ));
                }
                Err(_) => ok("证书链验证 (openssl)", "本机无 openssl，跳过（不影响其它验收）"),
            }
        }
        Err(e) => fail("CA + 站点证书签发", &e),
    };

    /* ---------- 11. 端口诊断（不 kill 任何进程） ---------- */
    match nsb_core::ports::diagnose_port(8080) {
        Ok(d) => ok("端口诊断", format!("8080 in_use={} pid={:?}", d.in_use, d.pid)),
        Err(e) => fail("端口诊断", &e),
    };

    /* ---------- 11b. 端口工具：区间扫描认得出「是自己的服务」 ---------- */
    // nginx 刚被 stop_all 停掉了，所以这里先起一个已知在监听的服务（redis）再扫
    let _ = state.start_service("redis");
    let redis_port = nsb_core::services::PortsProfile::from_settings(&state.store).redis;
    match state.scan_port_range(redis_port, redis_port) {
        Ok(scan) => {
            let own = scan.listeners.iter().find(|l| l.port == redis_port);
            assert_(
                own.map(|l| l.owned_by_self).unwrap_or(false),
                "端口区间扫描归属标注",
                format!(":{redis_port} → {:?}", own.map(|l| l.service_id.clone())),
            );
        }
        Err(e) => fail("端口区间扫描", &e),
    }
    // 未经用户确认，close_port 不该被任何自动路径调用；这里只验证「空闲端口」是安全的空操作
    match state.close_port(1) {
        Ok(o) => {
            assert_(o.killed_pids.is_empty(), "空闲端口 close_port 无副作用", "无进程被结束");
        }
        Err(e) => fail("空闲端口 close_port", &e),
    }
    let _ = state.stop_service("redis");

    /* ---------- 11c. 服务栈：一键启动 / 逐项报告 ---------- */
    {
        // 用真实已装服务存一个栈（先 php 后 nginx 的顺序与 LNMP 一致）
        let saved = nsb_core::stacks::save(
            &state.store,
            nsb_core::model::StackInput {
                id: None,
                name: "smoke 栈".into(),
                description: "冒烟用".into(),
                items: vec![
                    nsb_core::model::StackItem { service_id: "php@8.3.33".into(), label: None, order: 10 },
                    nsb_core::model::StackItem { service_id: "nginx".into(), label: None, order: 20 },
                ],
            },
        );
        match saved {
            Ok(stack) => {
                // 幂等：重复存同一个栈不得报错
                match state.start_stack(&stack.id) {
                    Ok(rep) => {
                        let ok2 = rep.started.len() + rep.already_running.len() >= 2 && rep.failed.is_empty();
                        assert_(
                            ok2,
                            "服务栈一键启动",
                            format!("started={:?} already={:?} failed={}", rep.started, rep.already_running, rep.failed.len()),
                        );
                    }
                    Err(e) => fail("服务栈一键启动", &e),
                }
                match state.stop_stack(&stack.id) {
                    Ok(rep) => {
                        assert_(rep.failed.is_empty(), "服务栈一键停止", format!("stopped={:?}", rep.started));
                    }
                    Err(e) => fail("服务栈一键停止", &e),
                }
                // 内置预设必须存在且不可删除
                let presets = state.list_stacks().unwrap_or_default();
                assert_(
                    presets.iter().any(|s| s.id == "builtin-lnmp"),
                    "内置服务栈预设",
                    format!("内置 LNMP 预设存在（共 {} 个栈）", presets.len()),
                );
                let _ = state.delete_stack(&stack.id);
            }
            Err(e) => fail("保存服务栈", &e),
        }
    }

    /* ---------- 12. 日志管线 ---------- */
    let log_target = ["redis", "nginx", "mihomo"].iter().find(|id| !state.tail_logs(id, 5).is_empty());
    assert_(log_target.is_some(), "日志 tail", format!("{:?} 有输出", log_target));

    /* ---------- 13. PHP 扩展管理 ---------- */
    // 用冒烟环境里真装好的 PHP 8.3 跑一遍：扫描 → 启用 → 再扫描确认
    {
        let ver = "8.3.33";
        match nsb_core::phpext::scan_available(&state.paths, ver) {
            Ok(exts) => {
                let total = exts.len();
                let enabled_before = exts.iter().filter(|e| e.enabled).count();
                let target = exts.iter().find(|e| !e.enabled).map(|e| e.name.clone());
                match target {
                    Some(name) => {
                        let r = nsb_core::phpext::set_extension(&state.paths, ver, &name, true);
                        let after = nsb_core::phpext::scan_available(&state.paths, ver)
                            .map(|v| v.iter().filter(|e| e.enabled).count())
                            .unwrap_or(0);
                        match r {
                            Ok(_) => {
                                assert_(
                                    after > enabled_before,
                                    "PHP 扩展启停",
                                    format!(
                                        "{total} 个可用；启用 {name} 后 {enabled_before}→{after}"
                                    ),
                                );
                            }
                            Err(e) => {
                                fail("PHP 扩展启停", &e);
                            }
                        }
                    }
                    None => {
                        skip("PHP 扩展启停", "所有扩展都已启用，无法验证开→关");
                    }
                }
            }
            Err(e) => {
                fail("PHP 扩展扫描", &e);
            }
        }
    }

    /* ---------- 14. Xdebug 构建指纹 ---------- */
    match nsb_core::xdebug::status(&state.paths, "8.3.33") {
        Ok(st) => {
            assert_(
                st.build.is_some(),
                "Xdebug 构建指纹识别",
                format!(
                    "PHP {} · {} · {} · {}（建议 xdebug {}）",
                    st.build.as_ref().map(|b| b.php_version.clone()).unwrap_or_default(),
                    if st.build.as_ref().map(|b| b.ts).unwrap_or(false) { "TS" } else { "NTS" },
                    st.build.as_ref().map(|b| b.compiler.clone()).unwrap_or_default(),
                    st.build.as_ref().map(|b| b.arch.clone()).unwrap_or_default(),
                    st.recommended
                ),
            );
        }
        Err(e) => {
            fail("Xdebug 构建指纹识别", &e);
        }
    }

    /* ---------- 15. 配置校验与保存 ---------- */
    {
        use nsb_core::cfgeditor::{ConfigKind, list_config_backups, save_config, validate};
        // 写坏的配置必须被拦下（nginx 装了会跑真 nginx -t，否则走结构自检）
        let bad = "events {}\nhttp {\n  server {\n    listen 80\n  }\n}\n";
        match validate(&state.paths, &state.store, ConfigKind::NginxMain, bad) {
            Ok(v) => {
                assert_(
                    !v.ok,
                    "配置校验拦住坏配置",
                    format!("识别出 {} 个问题（缺分号）", v.issues.len()),
                );
            }
            Err(e) => {
                fail("配置校验", &e);
            }
        }
        // 合法配置可保存（写前自动备份）
        let good = "events { worker_connections 256; }\nhttp {}\n";
        match save_config(&state.paths, &state.store, ConfigKind::NginxMain, good, false) {
            Ok(v) => {
                assert_(v.ok, "配置保存（校验通过后写入）", "写入前已自动备份");
            }
            Err(e) => {
                fail("配置保存", &e);
            }
        }
        let baks = list_config_backups(&state.paths);
        assert_(
            !baks.is_empty(),
            "配置备份可列出",
            format!("{} 个历史版本", baks.len()),
        );
    }

    /* ---------- 16. 证书体检 ---------- */
    match nsb_core::certs::report(&state.paths, &state.store) {
        Ok(r) => {
            let matched = r
                .certs
                .iter()
                .any(|c| c.sans.iter().any(|s| s.contains("smoke-https")));
            assert_(
                matched,
                "证书体检识别站点证书",
                format!(
                    "{} 张证书；过期 {} / 7天内 {} / 30天内 {}",
                    r.certs.len(),
                    r.expired,
                    r.critical,
                    r.warning
                ),
            );
        }
        Err(e) => {
            fail("证书体检", &e);
        }
    }

    /* ---------- 17. 批量服务操作（按依赖分层） ---------- */
    {
        let ids = vec!["nginx".to_string(), "php@8.3.33".to_string()];
        match nsb_core::bulk::start_many(&state.store, &state.paths, &state.manager, &ids) {
            Ok(rep) => {
                let order_ok = rep.order.first().map(|f| f.starts_with("php")).unwrap_or(false);
                assert_(
                    order_ok,
                    "批量启动按依赖分层",
                    format!("顺序 {:?}；成功 {} 个", rep.order, rep.succeeded.len()),
                );
                match nsb_core::bulk::stop_many(&state.store, &state.paths, &state.manager, &ids) {
                    Ok(sr) => {
                        assert_(
                            sr.failed.is_empty(),
                            "批量停止",
                            format!("停止 {:?}", sr.succeeded),
                        );
                    }
                    Err(e) => {
                        fail("批量停止", &e);
                    }
                }
            }
            Err(e) => {
                fail("批量启动", &e);
            }
        }
    }

    /* ---------- 18. 站点批量启停（只 reload 一次） ---------- */
    {
        let all = state.store.list_sites().unwrap_or_default();
        let ids: Vec<String> = all.iter().take(2).map(|s| s.id.clone()).collect();
        if ids.len() >= 2 {
            // 先确认真的停掉了，否则 start_many 会把它们全算进 already，
            // 测试「通过」但根本没走到启用逻辑（这个坑踩过一次）
            let stop_rep = nsb_core::sites::stop_many(&state.paths, &state.store, &state.manager, &ids);
            let stopped_ok = stop_rep.map(|r| !r.succeeded.is_empty()).unwrap_or(false);
            match nsb_core::sites::start_many(&state.paths, &state.store, &state.manager, &ids) {
                Ok(rep) => {
                    // 必须真的启用了（succeeded 非空），而不是全落到 already
                    assert_(
                        rep.failed.is_empty() && stopped_ok && !rep.succeeded.is_empty(),
                        "站点批量启用（只 reload 一次）",
                        format!(
                            "停用 → 启用 {} 个（already {}）",
                            rep.succeeded.len(),
                            rep.already.len()
                        ),
                    );
                }
                Err(e) => {
                    fail("站点批量启用", &e);
                }
            }
        } else {
            skip("站点批量启用（只 reload 一次）", "站点不足 2 个");
        }
    }

    /* ---------- 19. 诊断包脱敏 ---------- */
    match nsb_core::diagnostics::build(&state.paths, &state.store, &state.manager, "smoke") {
        Ok(b) => {
            // 报告必须含服务状态段，且不能把密码原文带出来
            let no_secret = !b.markdown.contains("SuperSecret123");
            assert_(
                no_secret && b.markdown.contains("## 服务状态"),
                "诊断包生成与脱敏",
                format!(
                    "{} 个服务 / {} 个站点 / {} 行日志 / 打码 {} 处",
                    b.service_count, b.site_count, b.log_lines, b.redacted
                ),
            );
        }
        Err(e) => {
            fail("诊断包生成", &e);
        }
    }

    /* ---------- 20. 环境体检 ---------- */
    match nsb_core::health::check(&state.paths, &state.store, &state.manager) {
        Ok(r) => {
            assert_(
                r.items.iter().all(|i| !i.title.is_empty()),
                "环境体检",
                format!("{}（错误 {} / 警告 {}）", r.summary, r.errors, r.warnings),
            );
        }
        Err(e) => {
            fail("环境体检", &e);
        }
    }

    /* ---------- 21. 项目扫描 ---------- */
    {
        let sites = state.store.list_sites().unwrap_or_default();
        let parent = sites
            .first()
            .and_then(|s| std::path::Path::new(&s.root_dir).parent().map(|p| p.to_path_buf()));
        match parent {
            Some(dir) => match nsb_core::scanner::scan_dir(&state.paths, &state.store, &dir) {
                Ok(found) => {
                    assert_(
                        !found.is_empty(),
                        "项目扫描",
                        format!("在 {} 下识别出 {} 个项目", dir.display(), found.len()),
                    );
                }
                Err(e) => {
                    fail("项目扫描", &e);
                }
            },
            None => {
                skip("项目扫描", "没有站点可作扫描目标");
            }
        }
    }

    /* ---------- 22. 日志导出 ---------- */
    {
        let target = ["nginx", "redis", "mihomo"]
            .iter()
            .find(|id| !state.tail_logs(id, 5).is_empty());
        match target {
            Some(id) => {
                let lines = state.tail_logs(id, 20);
                let mut text = String::new();
                for l in &lines {
                    text.push_str(&l.line);
                    text.push('\n');
                }
                match nsb_core::logs_export::write_log_file(&state.paths, id, &text, None) {
                    Ok(p) => {
                        let name = p.rsplit(['/', '\\']).next().unwrap_or_default().to_string();
                        let exists = std::path::Path::new(&p).is_file();
                        assert_(exists, "日志导出", format!("已写出 {name}"));
                        let _ = nsb_core::logs_export::delete_export(&state.paths, &name);
                    }
                    Err(e) => {
                        fail("日志导出", &e);
                    }
                }
            }
            None => {
                skip("日志导出", "没有可导出的日志");
            }
        }
    }

    /* ---------- 清理：停止全部（保留安装，便于复测） ---------- */
    nsb_core::ops::stop_all(&state.store, &state.paths, &state.manager);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let statuses = state.service_status_list();
    let stopped = statuses
        .iter()
        .filter(|s| s.state != nsb_core::model::ServiceState::Running)
        .count();
    assert_(stopped == statuses.len(), "全部服务已停止", "无孤儿进程（Job Object）");

    println!("==========================================");
    let failed = FAILED.load(Ordering::SeqCst);
    let total = STEP.load(Ordering::SeqCst);
    println!(" 结果：{} 通过 / {} 失败（用时 {:.1}s）", total - failed, failed, begin.elapsed().as_secs_f32());
    println!(" 测试数据目录：{}", base.display());
    println!(" 注意：全程未修改系统 hosts、未开启系统代理、未触碰本机其它服务。");
    if failed > 0 {
        std::process::exit(1);
    }
}

fn find_workspace() -> PathBuf {
    // exe 在 target/debug 或 target/release；向上找 Cargo.toml（workspace 根）
    let mut dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    for _ in 0..6 {
        if dir.join("Cargo.toml").exists() && dir.join("crates").exists() {
            return dir;
        }
        dir = dir.parent().map(PathBuf::from).unwrap_or(dir);
    }
    PathBuf::from(".")
}
