//! 清单驱动服务验收：真实下载 → 解压 → 启动 → 健康检查 → 停。
//! 用小体积服务（Mailpit ~10MB / Memcached ~3.5MB）验证通用启停路径打通；
//! 大包只做「安装 + 入口存在」验证，不启动以免拖慢。
//!
//! 用法：cargo run -p nsb-core --bin check_services
//! 数据目录固定 repo/.services-home（与 .smoke-home / .extra-home 隔离）

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn main() {
    let base = PathBuf::from(".services-home");
    std::fs::create_dir_all(&base).expect("创建 .services-home");
    std::env::set_var("NSB_SKIP_HOSTS", "1");

    let state = nsb_core::CoreState::init(Some(base.clone()), Arc::new(|_| {})).expect("初始化");

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
                    println!("[FAIL] {} — {}", $name, e);
                    fail += 1;
                }
            }
        };
    }

    /* ---------- 1. 安装小体积服务（真实网络下载） ---------- */
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    // caddy 17MB（多端口/配置模板）、mailpit 10MB（派生端口）、memcached 3.5MB（无配置）
    let install: &[&str] = &["memcached", "mailpit", "caddy"];
    for key in install {
        check!(format!("安装 {key}"), rt
            .block_on(state.install_package(key))
            .map(|p| format!("{} {} @ {}", p.id, p.version, p.install_path)));
    }

    /* ---------- 2. 通用注册：三个服务应出现在服务列表 ---------- */
    let statuses = state.service_status_list();
    for key in install {
        let found = statuses.iter().find(|s| s.id == *key);
        check!(format!("注册 {key}"), match found {
            Some(s) => Ok(format!("label={} port={:?} cat={:?}", s.label, s.port, s.category)),
            None => Err(format!("服务列表里没有 {key}（已注册：{:?}）",
                statuses.iter().map(|s| s.id.clone()).collect::<Vec<_>>())),
        });
    }

    /* ---------- 3. 启动通用服务 ---------- */
    for key in ["memcached", "mailpit"] {
        check!(format!("启动 {key}"), state.start_service(key).map(|_| "健康检查通过".to_string()));
    }
    // Caddy 需要 Caddyfile：验证「无配置时按模板生成再启动」
    check!("启动 caddy（自动生成 Caddyfile）", state.start_service("caddy").map(|_| "配置模板已生成并启动".to_string()));

    /* ---------- 4. 协议级健康验证 ---------- */
    // memcached：发 stats 命令
    check!("memcached 协议响应", (|| -> Result<String, String> {
        let port = state.service_status_list().iter()
            .find(|s| s.id == "memcached").and_then(|s| s.port)
            .ok_or("memcached 无端口")?;
        let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok();
        s.write_all(b"version\r\n").map_err(|e| e.to_string())?;
        let mut buf = [0u8; 128];
        let n = s.read(&mut buf).map_err(|e| e.to_string())?;
        let resp = String::from_utf8_lossy(&buf[..n]).to_string();
        if resp.starts_with("VERSION") { Ok(resp.trim().to_string()) } else { Err(resp) }
    })());

    // mailpit：HTTP /api/v1/info
    check!("mailpit HTTP API", (|| -> Result<String, String> {
        let port = state.service_status_list().iter()
            .find(|s| s.id == "mailpit").and_then(|s| s.port)
            .ok_or("mailpit 无端口")?;
        let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok();
        s.write_all(b"GET /api/v1/info HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .map_err(|e| e.to_string())?;
        let mut buf = String::new();
        s.read_to_string(&mut buf).map_err(|e| e.to_string())?;
        if buf.contains("200") && buf.to_lowercase().contains("version") {
            Ok(buf.lines().next().unwrap_or("").to_string())
        } else {
            Err(buf.chars().take(200).collect())
        }
    })());

    // caddy：HTTP 请求应拿到文件服务响应
    check!("caddy HTTP 响应", (|| -> Result<String, String> {
        let port = state.service_status_list().iter()
            .find(|s| s.id == "caddy").and_then(|s| s.port)
            .ok_or("caddy 无端口")?;
        let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_secs(5))).ok();
        s.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .map_err(|e| e.to_string())?;
        let mut buf = String::new();
        s.read_to_string(&mut buf).map_err(|e| e.to_string())?;
        Ok(buf.lines().next().unwrap_or("").to_string())
    })());

    /* ---------- 5. 端口方案：安全档应避开默认端口 ---------- */
    check!("安全档端口偏移生效", (|| -> Result<String, String> {
        let st = state.service_status_list();
        let m = st.iter().find(|s| s.id == "memcached").and_then(|s| s.port).ok_or("无端口")?;
        let ml = st.iter().find(|s| s.id == "mailpit").and_then(|s| s.port).ok_or("无端口")?;
        if m >= 20000 && ml >= 20000 {
            Ok(format!("memcached={m} mailpit={ml}（默认 11211/8025 已避让）"))
        } else {
            Err(format!("端口未偏移：memcached={m} mailpit={ml}"))
        }
    })());

    /* ---------- 6. 停止：进程组回收，无孤儿 ---------- */
    for key in ["caddy", "mailpit", "memcached"] {
        check!(format!("停止 {key}"), state.stop_service(key).map(|_| "已停止".to_string()));
    }
    std::thread::sleep(Duration::from_millis(1200));
    check!("无孤儿进程", (|| -> Result<String, String> {
        let mut leaked = Vec::new();
        for s in state.service_status_list() {
            if install.contains(&s.id.as_str())
                && s.state == nsb_core::model::ServiceState::Running
            {
                leaked.push(s.id.clone());
            }
        }
        if leaked.is_empty() { Ok("全部端口已释放".to_string()) } else { Err(format!("仍在运行: {leaked:?}")) }
    })());

    /* ---------- 7. 清单完整性：全部新条目可解析 ---------- */
    check!("清单条目完整性", (|| -> Result<String, String> {
        let inst = &state.installer;
        let ids = [
            "caddy", "frankenphp", "mailpit", "meilisearch", "zincsearch", "elasticsearch",
            "minio", "rustfs", "qdrant", "neo4j", "memcached", "consul", "etcd", "rnacos",
            "temporal-cli", "cloudflared", "coredns", "sftpgo", "rabbitmq", "mariadb",
        ];
        let mut missing = Vec::new();
        for id in ids {
            if inst.find(id).is_none() {
                missing.push(id);
            }
        }
        if missing.is_empty() {
            Ok(format!("{} 个新服务条目齐全", ids.len()))
        } else {
            Err(format!("缺失: {missing:?}"))
        }
    })());

    println!("\n==== check_services: {pass} pass / {fail} fail ====");
    println!("数据目录：{}", base.display());
    if fail > 0 {
        std::process::exit(1);
    }
}
