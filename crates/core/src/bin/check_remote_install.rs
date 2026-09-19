//! 验证「清单里没有、但版本源枚举得到」的版本可以真实安装。
//! 用法：cargo run -p nsb-core --bin check_remote_install <id@version> [...]
//! 不带参数时用 nginx@1.28.1（清单里只有 1.24/1.26.3/1.27.5/1.28.0）

#[tokio::main]
async fn main() {
    let base = std::path::PathBuf::from(".remote-home");
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var("NSB_SKIP_HOSTS", "1");
    let state = nsb_core::CoreState::init(Some(base.clone()), std::sync::Arc::new(|_| {})).unwrap();
    // 固定用安全档（8080 等高位端口），避免与机器上其它 Web 服务器抢 80
    state.store.set_setting("portProfile", "safe").unwrap();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let keys: Vec<String> = if args.is_empty() {
        vec!["nginx@1.28.1".into()]
    } else {
        args
    };

    let mut fail = 0usize;
    for key in &keys {
        let (id, ver) = key.split_once('@').expect("格式：id@version");

        // 1) 必须是「清单里没有」的版本，否则测不到远程路径
        let in_manifest = state.installer.manifest.packages.iter()
            .any(|p| p.id == id && p.version == ver);
        if in_manifest {
            println!("[SKIP] {key} 清单里已有该版本，换一个清单外的版本才测得到远程路径");
            continue;
        }

        // 2) 版本源应能枚举到它
        let cat = state.version_catalog(id, true).await.unwrap();
        let Some(rv) = cat.remote.iter().find(|r| r.version == ver) else {
            println!("[FAIL] {key} 版本源里没有该版本（枚举 {} 个, online={}）", cat.remote.len(), cat.online);
            if let Some(e) = &cat.error { println!("       error: {e}"); }
            fail += 1;
            continue;
        };
        println!("[PASS] 版本枚举       {key} url={} sha={}", 
            rv.url.rsplit('/').next().unwrap_or(""), rv.sha256.is_some());

        // 3) 真实安装（含下载/校验/解压）
        match state.install_package(key).await {
            Ok(p) => {
                let entry_ok = std::path::Path::new(&p.install_path)
                    .join(if cfg!(windows) { format!("nginx-{ver}/nginx.exe") } else { "sbin/nginx".into() })
                    .exists();
                println!("[PASS] 远程版本安装   {key} → {}（入口存在={entry_ok}）", p.install_path);
                if !entry_ok { fail += 1; continue; }

                // 4) 应注册为可启停服务并能启动（nginx 走内置编排）
                match state.start_service(id) {
                    Ok(_) => {
                        println!("[PASS] 启动已装远程版 {id} 健康检查通过");
                        let _ = state.stop_service(id);
                    }
                    Err(e) => { println!("[FAIL] 启动 {id}：{}", e.message); fail += 1; }
                }
            }
            Err(e) => { println!("[FAIL] 安装 {key}：{}", e.message); fail += 1; }
        }
    }
    println!("\n==== check_remote_install: {} fail ====", fail);
    if fail > 0 { std::process::exit(1); }
}
