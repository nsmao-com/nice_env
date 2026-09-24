//! core 单元测试：hosts 合并 / mihomo 适配 / 配置生成 / 下载断点续传。

use nsb_core::paths::Paths;

/* ---------- hosts 标记块合并 ---------- */

#[test]
fn hosts_merge_adds_block() {
    let original = "127.0.0.1 localhost\n::1 localhost\n";
    let out = platform::merge_hosts_content(
        original,
        &[("127.0.0.1".into(), "a.test".into())],
    );
    assert!(out.contains("127.0.0.1 localhost"));
    assert!(out.contains(platform::HOSTS_BEGIN));
    assert!(out.contains(platform::HOSTS_END));
    assert!(out.contains("a.test"));
}

#[test]
fn hosts_merge_replaces_existing_block_and_keeps_user_lines() {
    let original = "127.0.0.1 localhost\n# BEGIN NiceEnv (managed)\n127.0.0.1 old.test\n# END NiceEnv (managed)\n10.0.0.1 myserver\n";
    let out = platform::merge_hosts_content(
        original,
        &[("127.0.0.1".into(), "new.test".into())],
    );
    assert!(!out.contains("old.test"));
    assert!(out.contains("new.test"));
    assert!(out.contains("10.0.0.1 myserver"), "块外用户行必须保留");
    // 只有一个托管块
    assert_eq!(out.matches(platform::HOSTS_BEGIN).count(), 1);
}

/// 产品由 NiceServBay 改名而来：老用户 hosts 文件里是旧标记块，
/// 合并时必须把旧块替换成新标记块，而不是追加第二个托管块
#[test]
fn hosts_merge_replaces_legacy_niceservbay_block() {
    let original = "127.0.0.1 localhost\n# BEGIN NiceServBay (managed)\n127.0.0.1 old.test\n# END NiceServBay (managed)\n";
    let out = platform::merge_hosts_content(
        original,
        &[("127.0.0.1".into(), "new.test".into())],
    );
    assert!(!out.contains("old.test"), "旧块内容必须被清掉");
    assert!(!out.contains("NiceServBay"), "旧标记必须消失");
    assert_eq!(out.matches(platform::HOSTS_BEGIN).count(), 1, "只应剩一个新标记块");
    assert!(out.contains("new.test"));
}

/* ---------- mihomo 订阅适配 ---------- */

#[test]
fn mihomo_adapt_overrides_ports() {
    let raw = "mixed-port: 7890\nport: 7891\nexternal-controller: 0.0.0.0:9090\nallow-lan: true\nproxies: []\nrules:\n  - MATCH,DIRECT\n";
    let adapted = nsb_core::configgen::adapt_mihomo_profile(raw);
    assert!(adapted.contains(&format!("mixed-port: {}", nsb_core::configgen::MIHOMO_MIXED_PORT)));
    assert!(adapted.contains(&format!(
        "external-controller: 127.0.0.1:{}",
        nsb_core::configgen::MIHOMO_CONTROLLER_PORT
    )));
    assert!(adapted.contains("allow-lan: false"));
    assert!(!adapted.contains("0.0.0.0:9090"));
}

/* ---------- 配置生成关键内容 ---------- */

#[test]
fn php_ini_enables_pdo_mysql() {
    let paths = Paths::new(std::env::temp_dir().join("nsb-test-ini"));
    let ini = nsb_core::configgen::render_php_ini(&paths, "8.3.33", &paths.runtime_dir("php", "8.3.33"));
    assert!(ini.contains("extension=pdo_mysql"));
    assert!(ini.contains("cgi.force_redirect=0"));
}

#[test]
fn site_conf_php_has_fastcgi_upstream() {
    let site = nsb_core::model::Site {
        id: "site-x".into(),
        name: "x".into(),
        domains: vec!["x.test".into()],
        root_dir: "D:/code/x".into(),
        runtime: nsb_core::model::SiteRuntime {
            web_server: "nginx".into(),
            kind: nsb_core::model::SiteKind::Php,
            php_version: Some("8.3.33".into()),
            proxy_target: None,
            command: None,
            cwd: None,
        },
        https: true,
        rewrite: nsb_core::model::RewritePreset::Laravel,
        db: None,
        php_overrides: None,
        status: "running".into(),
        created_at: 0,
        updated_at: 0,
    };
    let paths = Paths::new(std::env::temp_dir().join("nsb-test-conf"));
    let conf = nsb_core::configgen::render_site_conf(&site, 8080, 8443, &paths.etc().join("nginx/fastcgi_params"), &paths.certs().join("sites"), &paths.logs().join("nginx"));
    assert!(conf.contains("fastcgi_pass nsb_php_8_3_33;"));
    assert!(conf.contains("server_name x.test;"));
    assert!(conf.contains("listen 8443 ssl;"));
    assert!(conf.contains("x.test.crt"));
    assert!(conf.contains("try_files $uri $uri/ /index.php?$query_string;"), "laravel 伪静态");
}

#[test]
fn site_conf_proxy_has_websocket_headers() {
    let site = nsb_core::model::Site {
        id: "site-p".into(),
        name: "p".into(),
        domains: vec!["p.test".into()],
        root_dir: "D:/code/p".into(),
        runtime: nsb_core::model::SiteRuntime {
            web_server: "nginx".into(),
            kind: nsb_core::model::SiteKind::ReverseProxy,
            php_version: None,
            proxy_target: Some("127.0.0.1:5173".into()),
            command: None,
            cwd: None,
        },
        https: false,
        rewrite: nsb_core::model::RewritePreset::None,
        db: None,
        php_overrides: None,
        status: "running".into(),
        created_at: 0,
        updated_at: 0,
    };
    let paths = Paths::new(std::env::temp_dir().join("nsb-test-conf2"));
    let conf = nsb_core::configgen::render_site_conf(&site, 8080, 8443, &paths.etc().join("nginx/fastcgi_params"), &paths.certs().join("sites"), &paths.logs().join("nginx"));
    assert!(conf.contains("proxy_pass http://127.0.0.1:5173;"));
    assert!(conf.contains("proxy_set_header Upgrade $http_upgrade;"));
}

/* ---------- 下载器：本地 HTTP 服务测断点续传 + sha256 校验 ---------- */

#[tokio::test]
async fn downloader_resume_and_checksum() {
    // 内容 100KB，已知 sha256
    let payload: Vec<u8> = (0..1024 * 100).map(|i| (i % 251) as u8).collect();
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(&payload);
    let sha = hex::encode(h.finalize());

    // 支持 Range 的极简 HTTP 服务
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let payload_clone = payload.clone();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for _ in 0..8 {
            let Ok((mut s, _)) = listener.accept() else { break };
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let range = req
                .lines()
                .find(|l| l.to_lowercase().starts_with("range:"))
                .and_then(|l| l.split(':').nth(1))
                .and_then(|r| r.trim().strip_prefix("bytes="))
                .and_then(|r| r.split('-').next())
                .and_then(|s| s.parse::<usize>().ok());
            let (status, body) = match range {
                Some(off) => ("206 Partial Content", payload_clone[off..].to_vec()),
                None => ("200 OK", payload_clone.clone()),
            };
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(headers.as_bytes());
            let _ = s.write_all(&body);
        }
    });

    let base = tempfile::tempdir().unwrap();
    let paths = Paths::new(base.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let downloader = nsb_core::download::Downloader::new();

    // 第一次：先造半截 .part，验证续传合并
    let task = "resume-test";
    let part = paths.downloads().join(format!("{task}.part"));
    std::fs::write(&part, &payload[..40_000]).unwrap();

    let url = format!("http://127.0.0.1:{port}/file.bin");
    let result = downloader
        .download(task, &[url], &sha, payload.len() as u64, &paths, &|_| {})
        .await
        .expect("下载应成功（从 40000 偏移续传）");

    let got = std::fs::read(&result).unwrap();
    assert_eq!(got, payload, "续传合并后内容必须一致");
}

#[tokio::test]
async fn downloader_rejects_bad_checksum() {
    let payload = b"hello world this is a test payload".to_vec();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let p2 = payload.clone();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for _ in 0..4 {
            let Ok((mut s, _)) = listener.accept() else { break };
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let _ = n;
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", p2.len());
            let _ = s.write_all(resp.as_bytes());
            let _ = s.write_all(&p2);
        }
    });
    let base = tempfile::tempdir().unwrap();
    let paths = Paths::new(base.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let downloader = nsb_core::download::Downloader::new();
    let url = format!("http://127.0.0.1:{port}/f.bin");
    let err = downloader
        .download("badsum", &[url], "deadbeef", payload.len() as u64, &paths, &|_| {})
        .await
        .expect_err("错误 sha 必须被拒绝");
    assert_eq!(err.code, "CHECKSUM_MISMATCH");
}

/* ---------- 端口池分配幂等 ---------- */

#[test]
fn php_pool_allocation_persists() {
    let base = tempfile::tempdir().unwrap();
    let paths = Paths::new(base.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let store = nsb_core::store::Store::open(paths.db()).unwrap();
    let a = nsb_core::services::allocate_php_pool(&store, "php@8.3.33").unwrap();
    let b = nsb_core::services::allocate_php_pool(&store, "php@8.3.33").unwrap();
    assert_eq!(a, b, "同服务端口分配必须幂等");
    let c = nsb_core::services::allocate_php_pool(&store, "php@7.4.33").unwrap();
    assert!(c >= a + 4 || c < a, "不同 php 版本池不重叠");
}

/* ---------- 清单入口路径：跨平台必须按段 join ---------- */

#[test]
fn manifest_entry_paths_cross_platform() {
    // 清单里一律用 '/'，且实盘必须来自同一个 join 逻辑，
    // 否则 macOS 上会把 "mysql-8.0.46-macos15-arm64/bin/mysqld"
    // 当成单个含反斜杠的文件名，永远认定「找不到主程序」
    let win = nsb_core::install::entry_relative_path("mysql-8.0.46-winx64/bin/mysqld.exe");
    let mac = nsb_core::install::entry_relative_path("mysql-8.0.46-macos15-arm64/bin/mysqld");
    assert_eq!(win.components().count(), 3, "Windows 入口应为 3 段");
    assert_eq!(mac.components().count(), 3, "macOS 入口应为 3 段");
    assert_eq!(
        mac.to_string_lossy().replace('\\', "/"),
        "mysql-8.0.46-macos15-arm64/bin/mysqld"
    );
    // 单文件入口保持一层
    assert_eq!(
        nsb_core::install::entry_relative_path("composer.phar").to_string_lossy(),
        "composer.phar"
    );
    // 反斜杠写法（防御性兼容）也要拆开
    assert_eq!(
        nsb_core::install::entry_relative_path(r"go\bin\go.exe")
            .components()
            .count(),
        3
    );
}

/* ---------- 端口体检：区分「自己的服务」与「被别人占」 ---------- */

#[test]
fn scan_app_ports_distinguishes_self_and_conflict() {
    let base = tempfile::tempdir().unwrap();
    let paths = Paths::new(base.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let store = nsb_core::store::Store::open(paths.db()).unwrap();
    let manager = std::sync::Arc::new(nsb_core::services::ServiceManager::new());
    nsb_core::ops::register_services(&paths, &store, &manager);

    // 占一个不在应用端口表里的高位端口，确认体检只报告应用关心的端口
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let foreign_port = listener.local_addr().unwrap().port();

    let rows = nsb_core::ports::scan_app_ports(&store, &manager).unwrap();
    assert!(!rows.is_empty(), "体检结果不应为空");
    // 每个端口都应有明确结论
    for r in &rows {
        assert!(
            ["free", "self", "conflict"].contains(&r.verdict.as_str()),
            "结论必须是 free/self/conflict，得到 {}",
            r.verdict
        );
        assert!(!r.verdict.eq("conflict") || r.pid.is_some(), "冲突必须给出 pid");
    }
    // 应用自己的端口不会因为「被占」而报 conflict（没有进程在跑时都是 free）
    assert!(
        !rows.iter().any(|r| r.port == foreign_port),
        "不在应用端口表里的端口不应出现在体检结果中"
    );
    // 覆盖到 https 与 mihomo 控制端口（这两个之前完全没被预检）
    let covered: Vec<u16> = rows.iter().map(|r| r.port).collect();
    assert!(covered.contains(&8443), "Nginx HTTPS 端口必须被体检覆盖");
    assert!(covered.contains(&19090), "mihomo 控制端口必须被体检覆盖");
}

/* ---------- hosts：用户手动条目持久化 ---------- */

#[test]
fn hosts_extra_entries_persist_and_merge() {
    let base = tempfile::tempdir().unwrap();
    let paths = Paths::new(base.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let store = nsb_core::store::Store::open(paths.db()).unwrap();

    // 用户添加两个自定义条目（其中一个是非 127.0.0.1 的 IP）
    let entries = vec![
        nsb_core::model::HostsEntry { ip: "127.0.0.1".into(), domain: "mytest.test".into(), managed: true },
        nsb_core::model::HostsEntry { ip: "10.0.0.9".into(), domain: "remote.test".into(), managed: true },
    ];
    std::env::set_var("NSB_SKIP_HOSTS", "1");
    nsb_core::hosts::apply(&store, &paths, Some(entries)).unwrap();

    let extras = nsb_core::hosts::extra_entries(&store);
    assert_eq!(extras.len(), 2, "自定义条目必须被持久化");
    assert!(extras.iter().any(|(ip, d)| ip == "10.0.0.9" && d == "remote.test"), "非 127.0.0.1 的 IP 不能被丢掉");

    // 托管条目 = 自定义条目（此时没有站点）
    let merged = nsb_core::hosts::managed_entries(&store);
    assert_eq!(merged.len(), 2);

    // 重建（修复向导）不应清空自定义条目
    nsb_core::hosts::rebuild(&store, &paths).unwrap();
    assert_eq!(nsb_core::hosts::extra_entries(&store).len(), 2, "重建不得丢失自定义条目");
}

/* ---------- 备份：可列出、可恢复 ---------- */

#[test]
fn backup_list_and_restore() {
    let base = tempfile::tempdir().unwrap();
    let paths = Paths::new(base.path().to_path_buf());
    paths.ensure_dirs().unwrap();

    // write_with_backup 会在第二次写入时留下 .bak
    let target = paths.etc().join("nginx").join("nginx.conf");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    nsb_core::paths::write_with_backup(&target, "v1\n", &paths.backup()).unwrap();
    nsb_core::paths::write_with_backup(&target, "v2\n", &paths.backup()).unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v2\n");

    let list = nsb_core::paths::list_backups(&paths.base);
    assert_eq!(list.len(), 1, "应有 1 个备份");
    let name = &list[0].0;

    let restored = nsb_core::paths::restore_backup(&paths.base, name).unwrap();
    assert_eq!(restored, target);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "v1\n", "恢复应写回旧内容");
}

/* ---------- 端口方案：默认标准端口 + 逐个覆盖 ---------- */

#[test]
fn port_profile_defaults_to_standard() {
    let base = tempfile::tempdir().unwrap();
    let paths = Paths::new(base.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let store = nsb_core::store::Store::open(paths.db()).unwrap();

    // 全新安装（没有任何 portProfile 设置）：必须走标准端口，而不是安全端口
    let p = nsb_core::services::PortsProfile::from_settings(&store);
    assert_eq!(p.http, 80, "默认必须是约定俗成的 80");
    assert_eq!(p.https, 443);
    assert_eq!(p.mysql, 3306);
    assert_eq!(p.redis, 6379);
    assert_eq!(p.postgres, 5432);
    assert_eq!(p.mongodb, 27017);

    // 显式切到安全档
    store.set_setting("portProfile", "safe").unwrap();
    let p = nsb_core::services::PortsProfile::from_settings(&store);
    assert_eq!(p.http, 8080);
    assert_eq!(p.mysql, 23306);

    // 单个端口覆盖优先于档位默认值
    store.set_port_override("mysql", Some(3400)).unwrap();
    let p = nsb_core::services::PortsProfile::from_settings(&store);
    assert_eq!(p.mysql, 3400, "覆盖值必须生效");
    assert_eq!(p.http, 8080, "未覆盖的项仍跟随档位");
    assert!(!store.port_overrides().is_empty());

    // 清除覆盖 → 回到档位默认
    store.set_port_override("mysql", None).unwrap();
    let p = nsb_core::services::PortsProfile::from_settings(&store);
    assert_eq!(p.mysql, 23306, "清除后回到档位默认");
    assert!(store.port_overrides().is_empty());
}

/* ---------- 服务栈：预设 / 增删改 / 内置保护 ---------- */

fn fresh_store() -> (tempfile::TempDir, Paths, nsb_core::store::Store) {
    let base = tempfile::tempdir().unwrap();
    let paths = Paths::new(base.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let store = nsb_core::store::Store::open(paths.db()).unwrap();
    (base, paths, store)
}

#[test]
fn stacks_presets_and_crud() {
    let (_base, _paths, store) = fresh_store();

    // 首次列出时自动写入内置预设
    let list = nsb_core::stacks::list(&store).unwrap();
    assert!(list.len() >= 3, "应有 LNMP / 前端 / 数据栈三个预设");
    assert!(list.iter().all(|s| s.builtin), "预设都是 builtin");
    let lnmp = list.iter().find(|s| s.id == "builtin-lnmp").unwrap();
    // 顺序必须是「先库后 web」，否则 nginx 起来时 php 池还没就绪
    let order: Vec<&str> = lnmp.items.iter().map(|i| i.service_id.as_str()).collect();
    assert_eq!(order, vec!["mysql", "php", "nginx"]);

    // 再列一次不会重复插入预设
    let again = nsb_core::stacks::list(&store).unwrap();
    assert_eq!(again.len(), list.len(), "预设不得重复生成");

    // 新建自定义栈
    let saved = nsb_core::stacks::save(
        &store,
        nsb_core::model::StackInput {
            id: None,
            name: "  我的栈  ".into(),
            description: "desc".into(),
            // 故意乱序 + 重复，验证排序与去重
            items: vec![
                nsb_core::model::StackItem { service_id: "nginx".into(), label: None, order: 30 },
                nsb_core::model::StackItem { service_id: "mysql".into(), label: None, order: 10 },
                nsb_core::model::StackItem { service_id: "nginx".into(), label: None, order: 20 },
            ],
        },
    )
    .unwrap();
    assert_eq!(saved.name, "我的栈", "名称应 trim");
    assert!(!saved.builtin);
    let ids: Vec<&str> = saved.items.iter().map(|i| i.service_id.as_str()).collect();
    assert_eq!(ids, vec!["mysql", "nginx"], "按 order 排序且去重");

    // 空栈 / 空名被拒
    assert!(nsb_core::stacks::save(
        &store,
        nsb_core::model::StackInput { id: None, name: " ".into(), description: String::new(), items: vec![] },
    )
    .is_err());
    assert!(nsb_core::stacks::save(
        &store,
        nsb_core::model::StackInput { id: None, name: "x".into(), description: String::new(), items: vec![] },
    )
    .is_err(), "空栈必须被拒（否则一键启动毫无意义）");

    // 复制预设 → 可编辑副本
    let copy = nsb_core::stacks::duplicate(&store, "builtin-lnmp", Some("副本".into())).unwrap();
    assert!(!copy.builtin);
    assert_eq!(copy.name, "副本");
    assert_eq!(copy.items.len(), 3, "副本保留条目");

    // 内置预设不可修改、不可删除
    assert!(nsb_core::stacks::delete(&store, "builtin-lnmp").is_err());
    assert!(nsb_core::stacks::save(
        &store,
        nsb_core::model::StackInput {
            id: Some("builtin-lnmp".into()),
            name: "改名".into(),
            description: String::new(),
            items: vec![nsb_core::model::StackItem { service_id: "nginx".into(), label: None, order: 10 }],
        },
    )
    .is_err(), "内置预设不能被直接改写");

    // 自定义栈可删
    nsb_core::stacks::delete(&store, &saved.id).unwrap();
    assert!(store.get_stack(&saved.id).unwrap().is_none());
}

/* ---------- 服务栈：启动顺序解析（未安装项跳过） ---------- */

#[test]
fn stack_start_skips_uninstalled_and_reports() {
    let (_base, paths, store) = fresh_store();
    let manager = std::sync::Arc::new(nsb_core::services::ServiceManager::new());
    // 什么都没装：栈里所有服务都解析不出来 → 报「没有可启动的服务」
    let err = nsb_core::stacks::start(&store, &paths, &manager, "builtin-lnmp").unwrap_err();
    assert_eq!(err.code, "STACK_EMPTY");
    assert!(err.hint.is_some(), "必须告诉用户下一步该做什么");
}

/* ---------- 端口冲突错误：携带端口与占用信息 ---------- */

#[test]
fn port_conflict_error_carries_port_and_holder() {
    let e = nsb_core::AppError::port_conflict(3306, Some("mysqld.exe")).with_pid(4242);
    assert_eq!(e.code, "PORT_IN_USE");
    assert!(e.message.contains("3306"));
    assert!(e.message.contains("mysqld.exe"));
    assert_eq!(e.port, Some(3306));
    assert_eq!(e.pid, Some(4242));
    assert_eq!(e.holder.as_deref(), Some("mysqld.exe"));
    // 序列化后前端能直接拿到这些字段（camelCase，无值时省略）
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["port"], 3306);
    assert_eq!(v["pid"], 4242);

    let info = nsb_core::model::AppErrorInfo::from(e);
    assert_eq!(info.port, Some(3306));
    assert_eq!(info.pid, Some(4242));
}

/* ---------- 导入导出：服务栈随配置走 ---------- */

#[test]
fn export_import_roundtrips_stacks_and_settings() {
    let (_b1, p1, s1) = fresh_store();
    let mine = nsb_core::stacks::save(
        &s1,
        nsb_core::model::StackInput {
            id: None,
            name: "导出用栈".into(),
            description: "d".into(),
            items: vec![
                nsb_core::model::StackItem { service_id: "mysql".into(), label: None, order: 10 },
                nsb_core::model::StackItem { service_id: "nginx".into(), label: None, order: 20 },
            ],
        },
    )
    .unwrap();
    s1.set_port_override("http", Some(8088)).unwrap();

    let file = p1.base.join("backup.json");
    nsb_core::transfer::export_to(&s1, &file).unwrap();

    // 导入到全新环境
    let (_b2, p2, s2) = fresh_store();
    let manager = std::sync::Arc::new(nsb_core::services::ServiceManager::new());
    let report = nsb_core::transfer::import_from(&file, &p2, &s2, &manager).unwrap();

    assert_eq!(report.stacks, 1, "自定义栈应被导入（内置预设不导出）");
    assert!(report.settings > 0, "设置应随备份导入");
    let imported = nsb_core::stacks::list(&s2)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "导出用栈")
        .expect("导入后应存在同名栈");
    assert!(!imported.builtin);
    assert_eq!(imported.items.len(), 2);
    assert_ne!(imported.id, mine.id, "导入端要重新分配 id，避免覆盖本机同名栈");

    // 设置里的端口覆盖跟着走
    let p = nsb_core::services::PortsProfile::from_settings(&s2);
    assert_eq!(p.http, 8088, "导入的端口覆盖必须生效");
}

/* ---------- 非本应用备份文件被拒 ---------- */

#[test]
fn import_rejects_foreign_json() {
    let (_base, paths, store) = fresh_store();
    let manager = std::sync::Arc::new(nsb_core::services::ServiceManager::new());
    let bogus = paths.base.join("bogus.json");
    std::fs::write(&bogus, "{\"format\":\"something-else\",\"sites\":[]}").unwrap();
    let err = nsb_core::transfer::import_from(&bogus, &paths, &store, &manager).unwrap_err();
    assert_eq!(err.code, "BAD_BACKUP");

    let broken = paths.base.join("broken.json");
    std::fs::write(&broken, "{not json").unwrap();
    let err = nsb_core::transfer::import_from(&broken, &paths, &store, &manager).unwrap_err();
    assert_eq!(err.code, "BAD_BACKUP");
}

/* ---------- 通配符证书：*.test 域名可签发且 SAN 正确 ---------- */

#[test]
fn wildcard_cert_can_be_issued_and_recorded() {
    let base = tempfile::tempdir().unwrap();
    let paths = Paths::new(base.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let store = nsb_core::store::Store::open(paths.db()).unwrap();

    let rec = nsb_core::tls::issue_site_cert(
        &paths,
        &store,
        &["*.dev.test".to_string(), "dev.test".to_string()],
    )
    .unwrap();

    assert_eq!(rec.subject, "*.dev.test");
    assert!(rec.sans.iter().any(|s| s == "*.dev.test"), "SAN 必须含通配符");
    assert!(rec.sans.iter().any(|s| s == "dev.test"), "SAN 应含裸域名");
    // 证书与私钥文件真实落盘
    assert!(std::path::Path::new(&rec.cert_path).is_file());
    assert!(std::path::Path::new(&rec.key_path.clone().unwrap()).is_file());
    // CA 也一并生成
    assert!(paths.certs().join("ca.crt").is_file());
}

/* ---------- 状态历史：变更被记录、错误被记录 ---------- */

#[test]
fn service_history_records_transitions() {
    let manager = std::sync::Arc::new(nsb_core::services::ServiceManager::new());
    manager.register("nginx", "Nginx", None, None, None, std::env::temp_dir().join("h.log"));
    manager.set_state("nginx", nsb_core::model::ServiceState::Starting);
    manager.set_state("nginx", nsb_core::model::ServiceState::Running);
    manager.set_error("nginx", nsb_core::AppError::new("X", "boom"));

    let h = manager.history_tail(10);
    assert!(h.len() >= 3, "至少 3 条：Starting/Running/Error");
    // 新→旧：最新一条是 Error
    assert_eq!(h[0].1, "nginx");
    assert!(h[0].2.contains("Error"), "最新一条应为 Error：{}", h[0].2);
}

/* ---------- 站点级 PHP 覆盖：.user.ini 写入与防注入 ---------- */

#[test]
fn user_ini_written_for_php_sites_only() {
    let mk = |kind: nsb_core::model::SiteKind| nsb_core::model::Site {
        id: "s1".into(), name: "t".into(), domains: vec!["a.test".into()],
        root_dir: std::env::temp_dir().join(format!("nsb-userini-{}", std::process::id()))
            .to_string_lossy().to_string(),
        runtime: nsb_core::model::SiteRuntime {
            web_server: "nginx".into(), kind,
            php_version: Some("8.3".into()), proxy_target: None, command: None, cwd: None,
        },
        https: false, rewrite: nsb_core::model::RewritePreset::None, db: None,
        php_overrides: Some(
            [
                ("memory_limit".to_string(), "512M".to_string()),
                ("evil\nkey".to_string(), "x".to_string()), // 非法键 → 丢弃
            ]
            .into_iter()
            .collect(),
        ),
        status: "stopped".into(), created_at: 0, updated_at: 0,
    };

    let root = std::path::PathBuf::from(
        mk(nsb_core::model::SiteKind::Php).root_dir.clone()
    );
    std::fs::create_dir_all(&root).unwrap();

    // php 站点：写入；合法键在、非法键被丢弃、值内无换行
    let site = mk(nsb_core::model::SiteKind::Php);
    nsb_core::sites::write_user_ini(&site);
    let ini = std::fs::read_to_string(root.join(".user.ini")).unwrap();
    assert!(ini.contains("memory_limit=512M"));
    assert!(!ini.contains("evil"));
    assert!(!ini.contains('\r'));

    // 非 php 站点：不写
    let static_site = mk(nsb_core::model::SiteKind::Static);
    nsb_core::sites::write_user_ini(&static_site);
    assert!(!root.join(".user.ini").exists() || {
        // 若上面 php 用例写过则文件存在；这里只断言 static 没改内容 —— 直接删掉重验
        let _ = std::fs::remove_file(root.join(".user.ini"));
        nsb_core::sites::write_user_ini(&static_site);
        !root.join(".user.ini").exists()
    });
    let _ = std::fs::remove_dir_all(&root);
}

/* ---------- 项目级运行时锁定 .nsb.json ---------- */

#[test]
fn project_pin_read_and_validation() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("proj");
    std::fs::create_dir_all(&root).unwrap();

    // 没有文件 → None
    assert!(nsb_core::sites::read_project_pin(root.to_str().unwrap()).is_none());

    // 合法 → Some
    std::fs::write(root.join(".nsb.json"), r#"{ "php": "8.3.33" }"#).unwrap();
    let pin = nsb_core::sites::read_project_pin(root.to_str().unwrap()).unwrap();
    assert_eq!(pin, ("php".to_string(), "8.3.33".to_string()));

    // 非法版本串（注入尝试）→ None
    std::fs::write(root.join(".nsb.json"), r#"{ "php": "8.3; drop" }"#).unwrap();
    assert!(nsb_core::sites::read_project_pin(root.to_str().unwrap()).is_none());

    // 坏 JSON → None
    std::fs::write(root.join(".nsb.json"), "not json").unwrap();
    assert!(nsb_core::sites::read_project_pin(root.to_str().unwrap()).is_none());
}

/* ---------- 伪静态模板：新增框架片段语法有效 ---------- */

#[test]
fn new_rewrite_presets_render_nginx_snippets() {
    use nsb_core::configgen::rewrite_snippet;
    // 每个新预设都能产出非空、含 try_files/rewrite 的片段
    for preset in [
        (nsb_core::model::RewritePreset::Symfony, "index.php"),
        (nsb_core::model::RewritePreset::Yii2, "index.php"),
        (nsb_core::model::RewritePreset::Codeigniter, "deny all"),
        (nsb_core::model::RewritePreset::Cakephp, "index.php"),
        (nsb_core::model::RewritePreset::Drupal, "deny all"),
        (nsb_core::model::RewritePreset::Joomla, "index.php"),
    ] {
        let snip = rewrite_snippet(&preset.0);
        assert!(!snip.is_empty());
        assert!(snip.contains(preset.1), "{:?} 应含 {}：{}", preset.0, preset.1, snip);
    }
}

/* ---------- 站点级访问日志指令 ---------- */

#[test]
fn site_conf_contains_per_site_access_log() {
    let base = tempfile::tempdir().unwrap();
    let paths = Paths::new(base.path().to_path_buf());
    paths.ensure_dirs().unwrap();
    let store = nsb_core::store::Store::open(paths.db()).unwrap();
    let site = nsb_core::sites::list(&store).unwrap();
    let _ = site;

    let s = nsb_core::model::Site {
        id: "site-log".into(),
        name: "logtest".into(),
        domains: vec!["log.test".into()],
        root_dir: "D:/code/logtest".into(),
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
        db: None,
        php_overrides: None,
        status: "stopped".into(),
        created_at: 0,
        updated_at: 0,
    };
    let conf = nsb_core::configgen::render_site_conf(
        &s,
        8080,
        8443,
        std::path::Path::new("fastcgi_params"),
        std::path::Path::new("certs"),
        &paths.logs().join("nginx"),
    );
    assert!(
        conf.contains("access_log"),
        "站点 conf 必须带 access_log：{conf}"
    );
    assert!(conf.contains("site-log.access.log"));
    assert!(conf.contains("error_log"));
}
