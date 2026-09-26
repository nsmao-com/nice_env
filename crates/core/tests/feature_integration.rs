//! 本轮新增能力的端到端集成测试。
//!
//! 这些功能在浏览器里用 mock 后端验证过（UI 连得通），单元测试也各自覆盖了
//! 纯逻辑，但两者之间有一段空白：**真实的 Rust 代码路径是否连得起来**。
//! 这个文件补的正是这一段 —— 用真实 Paths/Store，跑真实的读写流程。
//!
//! 覆盖：PHP 扩展改 ini → Xdebug 配置段 → 证书体检 → 诊断包脱敏 →
//! 配置编辑器校验/保存/回滚 → .env 读写 → 日志导出 → 工具链镜像改写。

use nsb_core::paths::Paths;

/// 每个测试用独立临时目录，避免互相干扰
struct Env {
    dir: std::path::PathBuf,
    paths: Paths,
    store: nsb_core::store::Store,
}

impl Env {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("nsb-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::new(dir.clone());
        paths.ensure_dirs().unwrap();
        let store = nsb_core::store::Store::open(dir.join("it.sqlite")).unwrap();
        Env { dir, paths, store }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/* ================= PHP 扩展 ================= */

#[test]
fn php_extension_toggle_round_trip_through_ini() {
    let e = Env::new("phpext");
    // 造一个假的 PHP 安装：ext 目录 + php.ini
    let ext_dir = e.paths.runtime_dir("php", "8.3.33").join("ext");
    std::fs::create_dir_all(&ext_dir).unwrap();
    for f in [
        "php_curl.dll",
        "php_gd.dll",
        "php_xdebug.dll",
        "php_redis.dll",
    ] {
        std::fs::write(ext_dir.join(f), b"MZ fake").unwrap();
    }
    let ini = e.paths.php_ini("8.3.33");
    std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
    std::fs::write(&ini, "[PHP]\nengine=On\n\n[Extensions]\nextension=curl\n").unwrap();

    // 扫描：磁盘上 4 个，ini 里启用 1 个
    let exts = nsb_core::phpext::scan_available(&e.paths, "8.3.33").unwrap();
    let names: Vec<&str> = exts.iter().map(|x| x.name.as_str()).collect();
    for n in ["curl", "gd", "xdebug", "redis"] {
        assert!(names.contains(&n), "应扫到 {n}：{names:?}");
    }
    assert_eq!(exts.iter().filter(|x| x.enabled).count(), 1);

    // 启用 gd（走真实写盘路径）
    nsb_core::phpext::set_extension(&e.paths, "8.3.33", "gd", true).unwrap();
    let after = nsb_core::phpext::scan_available(&e.paths, "8.3.33").unwrap();
    assert!(
        after.iter().any(|x| x.name == "gd" && x.enabled),
        "gd 应变为启用"
    );

    // 禁用 curl
    nsb_core::phpext::set_extension(&e.paths, "8.3.33", "curl", false).unwrap();
    let after2 = nsb_core::phpext::scan_available(&e.paths, "8.3.33").unwrap();
    assert!(
        after2.iter().any(|x| x.name == "curl" && !x.enabled),
        "curl 应变为禁用"
    );

    // 备份应已生成（改前自动备份）
    let backups = std::fs::read_dir(e.paths.backup())
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);
    assert!(backups > 0, "改 php.ini 前应生成备份");
}

#[test]
fn php_extension_set_on_missing_ini_reports_actionable_error() {
    let e = Env::new("noini");
    let r = nsb_core::phpext::set_extension(&e.paths, "9.9.9", "curl", true);
    assert!(r.is_err());
    let err = r.unwrap_err();
    assert_eq!(err.code, "NO_INI");
    assert!(err.hint.is_some(), "应给出下一步建议");
}

/* ================= Xdebug ================= */

#[test]
fn xdebug_status_and_section_write() {
    let e = Env::new("xdebug");
    let ext_dir = e.paths.runtime_dir("php", "8.3.33").join("ext");
    std::fs::create_dir_all(&ext_dir).unwrap();
    // 文件名必须问 dll_file_name：Windows 是 php_xdebug.dll，其它平台是 xdebug.so。
    // 写死 .dll 的话 Linux CI 上 dll_present 永远是 false。
    let dll_name = nsb_core::phpext::dll_file_name("xdebug");
    std::fs::write(ext_dir.join(&dll_name), b"MZ fake").unwrap();
    let ini = e.paths.php_ini("8.3.33");
    std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
    std::fs::write(&ini, "[PHP]\nengine=On\n").unwrap();

    // 状态：DLL 在、没启用
    let st = nsb_core::xdebug::status(&e.paths, "8.3.33").unwrap();
    assert!(st.dll_present, "DLL 存在");
    assert!(!st.enabled, "尚未在 ini 里启用");

    // 写入 [xdebug] 段
    let sec = nsb_core::xdebug::render_xdebug_section(
        &ext_dir.join(&dll_name).to_string_lossy(),
        "debug,develop",
        9003,
    );
    let cur = std::fs::read_to_string(&ini).unwrap();
    let next = nsb_core::xdebug::upsert_xdebug_section(&cur, &sec);
    std::fs::write(&ini, next).unwrap();

    // 再读：段内容应可解析出 mode
    let st2 = nsb_core::xdebug::status(&e.paths, "8.3.33").unwrap();
    assert_eq!(
        st2.settings.get("xdebug.mode").map(String::as_str),
        Some("debug,develop")
    );
    // 段只能有一个
    let content = std::fs::read_to_string(&ini).unwrap();
    assert_eq!(content.matches("[xdebug]").count(), 1);
}

/* ================= 证书 ================= */

#[test]
fn cert_report_on_empty_state_is_clean() {
    let e = Env::new("cert");
    let r = nsb_core::certs::report(&e.paths, &e.store).unwrap();
    assert_eq!(r.expired, 0);
    assert_eq!(r.critical, 0);
    // 没签过证书时 CA 自然未信任，不该误报为「证书有问题」
    assert!(r.certs.is_empty() || r.certs.iter().all(|c| c.days_left > 0));
}

#[test]
fn cert_import_rejects_garbage_and_accepts_pem() {
    let e = Env::new("certimp");
    let bad = e.dir.join("not-a-cert.txt");
    let key = e.dir.join("not-a-key.txt");
    std::fs::write(&bad, "hello").unwrap();
    std::fs::write(&key, "hello").unwrap();
    let r = nsb_core::certs::import_cert_pair(&e.paths, &bad, &key);
    assert!(r.is_err());
    assert_eq!(r.unwrap_err().code, "NOT_A_CERT");

    // 用真 CA 签一张站点证书，再导入它自己（验证 PEM、SAN 和密钥匹配路径打通）
    nsb_core::tls::ensure_ca(&e.paths).unwrap();
    let issued = nsb_core::tls::issue_site_cert(&e.paths, &e.store, &["import.test".into()]).unwrap();
    let imp = nsb_core::certs::import_cert_pair(&e.paths, std::path::Path::new(&issued.cert_path), std::path::Path::new(issued.key_path.as_ref().unwrap()));
    assert!(imp.is_ok(), "应能导入真 PEM：{imp:?}");
    let info = imp.unwrap();
    assert!(info.usable && info.not_after > 0, "应解析出有效期和可用状态");
    assert!(info.sans.contains(&"import.test".to_string()));
}

#[test]
fn cert_import_rejects_mismatched_ca_and_uncovered_domains() {
    let e = Env::new("certimp-validation");
    nsb_core::tls::ensure_ca(&e.paths).unwrap();
    let first = nsb_core::tls::issue_site_cert(&e.paths, &e.store, &["first.test".into()]).unwrap();
    let second = nsb_core::tls::issue_site_cert(&e.paths, &e.store, &["second.test".into()]).unwrap();

    let mismatched = nsb_core::certs::import_cert_pair(
        &e.paths,
        std::path::Path::new(&first.cert_path),
        std::path::Path::new(second.key_path.as_ref().unwrap()),
    )
    .unwrap_err();
    assert_eq!(mismatched.code, "CERT_KEY_MISMATCH");

    let ca = nsb_core::certs::import_cert_pair(
        &e.paths,
        &e.paths.certs().join("ca.crt"),
        &e.paths.certs().join("ca.key"),
    )
    .unwrap_err();
    assert_eq!(ca.code, "CERT_IS_CA");

    let imported = nsb_core::certs::import_cert_pair(
        &e.paths,
        std::path::Path::new(&first.cert_path),
        std::path::Path::new(first.key_path.as_ref().unwrap()),
    )
    .unwrap();
    let domain = nsb_core::certs::validate_imported_domains(
        &e.paths,
        &imported.id,
        &["outside.test".into()],
    )
    .unwrap_err();
    assert_eq!(domain.code, "CERT_DOMAIN_MISMATCH");
}

/* ================= 配置编辑器 ================= */

#[test]
fn config_editor_validate_save_and_rollback() {
    let e = Env::new("cfged");
    // 造一个 nginx.conf
    let conf = e.paths.nginx_conf();
    std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
    std::fs::write(
        &conf,
        "events { worker_connections 1024; }\nhttp { server { listen 80; } }\n",
    )
    .unwrap();
    // nginx 未安装 → 只做结构自检
    use nsb_core::cfgeditor::{save_config, validate, ConfigKind};

    // 合法内容应通过
    let ok = validate(
        &e.paths,
        &e.store,
        ConfigKind::NginxMain,
        "events {}\nhttp { server { listen 80; } }\n",
    )
    .unwrap();
    assert!(ok.ok, "{:?}", ok.issues);

    // 缺分号应被拦
    let bad = validate(
        &e.paths,
        &e.store,
        ConfigKind::NginxMain,
        "http {\n listen 80\n}\n",
    )
    .unwrap();
    assert!(!bad.ok);
    assert!(bad.issues.iter().any(|i| i.line == 2), "应指出行号");

    // 不通过时拒绝写入
    let refused = save_config(&e.paths, &e.store, ConfigKind::NginxMain, "http {\n", false);
    assert!(refused.is_err(), "校验不过不该写入");
    // 原文件未被改动
    let untouched = std::fs::read_to_string(&conf).unwrap();
    assert!(untouched.contains("listen 80"), "原配置应完好");

    // 合法内容可保存并回滚
    let good = "events {}\nhttp { server { listen 8080; } }\n";
    save_config(&e.paths, &e.store, ConfigKind::NginxMain, good, false).unwrap();
    assert_eq!(std::fs::read_to_string(&conf).unwrap(), good);

    let backups = nsb_core::cfgeditor::list_config_backups(&e.paths);
    assert!(!backups.is_empty(), "保存前应有备份");
    nsb_core::cfgeditor::rollback_config(&e.paths, &e.store, &backups[0].name).unwrap();
    let rolled = std::fs::read_to_string(&conf).unwrap();
    assert!(rolled.contains("listen 80"), "回滚后应恢复旧内容：{rolled}");
}

#[test]
fn config_editor_rejects_unknown_kind() {
    assert!(nsb_core::cfgeditor::ConfigKind::parse("../../etc/passwd").is_none());
    assert!(nsb_core::cfgeditor::ConfigKind::parse("ssh-key").is_none());
}

/* ================= .env ================= */

#[test]
fn env_read_save_round_trip() {
    let e = Env::new("env");
    // 建一个站点（直接写 store，避免依赖 nginx 存在）
    let root = e.dir.join("site");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join(".env"),
        "APP_NAME=Old\n# 分组注释\nDB_HOST=localhost\n",
    )
    .unwrap();

    // 直接走 envfile 的纯函数验证「只改指定键、保留注释」
    let src = std::fs::read_to_string(root.join(".env")).unwrap();
    let out = nsb_core::envfile::apply_env_changes(
        &src,
        &[
            ("APP_NAME".into(), "New".into()),
            ("DB_PORT".into(), "23306".into()),
        ],
    );
    assert!(out.contains("APP_NAME=New"));
    assert!(out.contains("# 分组注释"), "注释必须保留：{out}");
    assert!(out.contains("DB_PORT=23306"), "新键应追加");
    assert!(out.contains("DB_HOST=localhost"), "无关键应保留");

    // 敏感键识别
    assert!(nsb_core::envfile::is_secret_key("DB_PASSWORD"));
    assert!(nsb_core::envfile::is_secret_key("APP_KEY"));
    assert!(!nsb_core::envfile::is_secret_key("APP_NAME"));

    // 需要引号的值
    assert!(nsb_core::envfile::needs_quoting("my pass"));
    assert!(!nsb_core::envfile::needs_quoting("simple"));
}

/* ================= 日志导出 ================= */

#[test]
fn log_export_writes_and_lists() {
    let e = Env::new("logexp");
    let p =
        nsb_core::logs_export::write_log_file(&e.paths, "nginx", "[error] boom\n[info] ok\n", None)
            .unwrap();
    assert!(std::path::Path::new(&p).is_file());
    assert!(p.ends_with(".log"));

    let list = nsb_core::logs_export::list_exports(&e.paths);
    assert_eq!(list.len(), 1);
    assert!(list[0].1 > 0, "应记录文件大小");

    // 空内容应被拒
    assert!(nsb_core::logs_export::write_log_file(&e.paths, "nginx", "  \n", None).is_err());
}

/* ================= 工具链镜像 ================= */

#[test]
fn tool_mirror_rewrites_preserve_other_settings() {
    // npm：替换 registry，保留 strict-ssl
    let out = nsb_core::toolmirror::apply_npmrc(
        "strict-ssl=false\nregistry=https://registry.npmjs.org\n",
        "https://registry.npmmirror.com",
    );
    assert!(out.contains("registry=https://registry.npmmirror.com"));
    assert!(out.contains("strict-ssl=false"));
    assert!(!out.contains("registry.npmjs.org"));

    // composer：写 repositories，保留其它键
    let c = nsb_core::toolmirror::apply_composer_registry(
        r#"{"name":"a/b","config":{"optimize-autoloader":true}}"#,
        "https://mirrors.aliyun.com/composer/",
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&c).unwrap();
    assert_eq!(v["name"], "a/b");
    assert_eq!(
        v["repositories"]["packagist"]["url"],
        "https://mirrors.aliyun.com/composer/"
    );
    // 恢复官方应删掉覆盖
    let reset = nsb_core::toolmirror::reset_composer_registry(&c).unwrap();
    let v2: serde_json::Value = serde_json::from_str(&reset).unwrap();
    assert!(v2.get("repositories").is_none());

    // pip：保证落在 [global] 段内
    let pip = nsb_core::toolmirror::apply_pip_ini(
        "[global]\ntimeout = 60\n\n[install]\nno-warn = true\n",
        "https://pypi.tuna.tsinghua.edu.cn/simple",
    );
    let idx = pip.find("index-url").unwrap();
    let install_idx = pip.find("[install]").unwrap();
    assert!(idx < install_idx, "index-url 必须在 [global] 段内：{pip}");
}

/* ================= 批量服务操作 ================= */

#[test]
fn bulk_ordering_and_summary() {
    let ids = vec![
        "nginx".to_string(),
        "redis".to_string(),
        "php@8.3.33".to_string(),
        "mysql@8.0.46".to_string(),
    ];
    let start = nsb_core::bulk::order_for_start(&ids);
    // 数据/缓存层（redis、mysql）必须在运行层（php）与前端层（nginx）之前。
    // 同层内保持用户勾选顺序，所以不断言 mysql/redis 的相对位置。
    let tier = |id: &str| {
        let pos = start.iter().position(|x| x == id).unwrap();
        match id {
            "redis" | "mysql@8.0.46" => (0, pos),
            "php@8.3.33" => (1, pos),
            "nginx" => (2, pos),
            _ => (9, pos),
        }
    };
    assert!(
        tier("redis").1 < tier("php@8.3.33").1,
        "数据层应先于运行层：{start:?}"
    );
    assert!(tier("mysql@8.0.46").1 < tier("php@8.3.33").1, "{start:?}");
    assert!(
        tier("php@8.3.33").1 < tier("nginx").1,
        "nginx 必须最后：{start:?}"
    );

    let stop = nsb_core::bulk::order_for_stop(&ids);
    let pos = |id: &str| stop.iter().position(|x| x == id).unwrap();
    assert!(pos("nginx") < pos("php@8.3.33"), "停止应先断流量：{stop:?}");
    assert!(
        pos("php@8.3.33") < pos("mysql@8.0.46"),
        "数据库最后停：{stop:?}"
    );
    assert!(pos("php@8.3.33") < pos("redis"), "{stop:?}");

    let m = std::sync::Arc::new(nsb_core::services::ServiceManager::new());
    let sum = nsb_core::bulk::summarize(&m, &ids);
    assert_eq!(sum.total, 4);
    assert!(sum.can_start && !sum.can_stop, "都未注册运行 → 只能启动");
}

/* ================= 诊断包脱敏 ================= */

#[test]
fn diagnostics_redacts_secrets_end_to_end() {
    let e = Env::new("diag");
    // 造一份带密码的 php.ini 与一个带密码的 mihomo 配置
    let ini = e.paths.php_ini("8.3.33");
    std::fs::create_dir_all(ini.parent().unwrap()).unwrap();
    std::fs::write(
        &ini,
        "[PHP]\nengine=On\nmysql_root_password=SuperSecret123\nmemory_limit=256M\n",
    )
    .unwrap();

    // resolve_path 需要 store 里登记了 PHP，否则配置摘要会被跳过。
    // 这里如实登记，模拟真实安装后的状态。
    e.store
        .upsert_installed(&nsb_core::model::InstalledPackage {
            id: "php".into(),
            version: "8.3.33".into(),
            category: "runtime".into(),
            install_path: e
                .paths
                .runtime_dir("php", "8.3.33")
                .to_string_lossy()
                .to_string(),
            config_path: e
                .paths
                .etc_dir("php", "8.3.33")
                .to_string_lossy()
                .to_string(),
            installed_at: chrono::Local::now().timestamp(),
        })
        .unwrap();

    let m = std::sync::Arc::new(nsb_core::services::ServiceManager::new());
    let bundle = nsb_core::diagnostics::build(&e.paths, &e.store, &m, "0.1.0").unwrap();

    assert!(!bundle.markdown.is_empty());
    assert!(
        !bundle.markdown.contains("SuperSecret123"),
        "密码绝不能出现在诊断包里"
    );
    assert!(bundle.redacted > 0, "应记录打码条目数");
    // 非敏感项要保留，否则诊断包没用了
    assert!(bundle.markdown.contains("memory_limit=256M"));
    // 采集到的配置必须真的出现在报告里
    assert!(bundle.markdown.contains("## 配置摘要"), "应有配置摘要段");
}

/* ================= 环境体检 ================= */

#[test]
fn health_check_reports_empty_env_guidance() {
    let e = Env::new("health");
    let m = std::sync::Arc::new(nsb_core::services::ServiceManager::new());
    let r = nsb_core::health::check(&e.paths, &e.store, &m).unwrap();
    assert!(
        r.items.iter().any(|i| i.id == "no-packages"),
        "空环境应引导用户去装套件：{:?}",
        r.items.iter().map(|i| i.id.clone()).collect::<Vec<_>>()
    );
    // errors 必须排在 infos 前面
    let mut seen_non_error = false;
    for i in &r.items {
        let is_err = i.severity == nsb_core::health::Severity::Error;
        if !is_err {
            seen_non_error = true;
        } else if seen_non_error {
            panic!("error 项应排在最前");
        }
    }
}

/* ================= 项目扫描 ================= */

#[test]
fn scanner_detects_projects_and_skips_noise() {
    let e = Env::new("scan");
    let root = e.dir.join("code");
    std::fs::create_dir_all(&root).unwrap();
    // Laravel 项目
    std::fs::create_dir_all(root.join("shop/public")).unwrap();
    std::fs::write(root.join("shop/artisan"), "").unwrap();
    std::fs::write(root.join("shop/public/index.php"), "<?php").unwrap();
    // 噪音目录
    std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
    std::fs::write(root.join("node_modules/x/index.js"), "1").unwrap();

    let m = std::sync::Arc::new(nsb_core::services::ServiceManager::new());
    let _ = &m;
    let found = nsb_core::scanner::scan_dir(&e.paths, &e.store, &root).unwrap();
    let names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"shop"), "应识别 Laravel 项目：{names:?}");
    assert!(
        !names.contains(&"node_modules"),
        "不该把依赖目录当项目：{names:?}"
    );
    let shop = found.iter().find(|p| p.name == "shop").unwrap();
    assert_eq!(shop.site_kind, "php");
    assert_eq!(shop.rewrite, "laravel");
    assert!(
        shop.document_root.replace('\\', "/").ends_with("/public"),
        "文档根应指向 public：{}",
        shop.document_root
    );
}

/* ================= 看门狗 ================= */

#[test]
fn watchdog_respects_user_stop_and_backs_off() {
    let w = nsb_core::watchdog::Watchdog::new();
    let cfg = nsb_core::watchdog::WatchdogConfig {
        enabled: true,
        ..Default::default()
    };
    // 用户启动 → 可监控
    w.note_started("mysql@8.0.46");
    assert!(w.should_restart("mysql@8.0.46", &cfg));
    // 用户主动停止 → 不再拉起（这是最关键的一条）
    w.note_user_stopped("mysql@8.0.46");
    assert!(!w.should_restart("mysql@8.0.46", &cfg));
    // 重启失败 → 退避
    w.note_started("nginx");
    w.note_restart("nginx", false, &cfg);
    assert!(!w.should_restart("nginx", &cfg), "失败后应退避");
    // 默认关闭
    assert!(!nsb_core::watchdog::WatchdogConfig::default().enabled);
}

/// 采不到的配置必须出现在报告里。
///
/// 这条守的是一个很容易被忽略的点：如果只是 `continue` 跳过，
/// 用户报「PHP 起不来」时拿到的诊断包里没有 php.ini，
/// 看的人无法区分「配置正常」和「压根没采到」。
#[test]
fn diagnostics_lists_configs_it_could_not_collect() {
    let e = Env::new("diagskip");
    // 造一份 nginx.conf（不需要 store 登记），但不装 PHP
    let conf = e.paths.nginx_conf();
    std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
    std::fs::write(
        &conf,
        "events {}
",
    )
    .unwrap();

    let m = std::sync::Arc::new(nsb_core::services::ServiceManager::new());
    let bundle = nsb_core::diagnostics::build(&e.paths, &e.store, &m, "0.1.0").unwrap();

    // PHP 未登记 → 应出现在「未能采集」里
    assert!(
        bundle.markdown.contains("未能采集的配置"),
        "应明确列出未采集的配置：{}",
        &bundle.markdown[..bundle.markdown.len().min(1200)]
    );
    assert!(bundle.markdown.contains("php.ini"), "应点名 php.ini 没采到");
}
