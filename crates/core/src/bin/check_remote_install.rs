//! 验证「清单里没有、但版本源枚举得到」的版本可以真实安装。
//! 用法：cargo run -p nsb-core --bin check_remote_install <id@version> [...]
//! 不带参数时用 nginx@1.28.1（清单里只有 1.24/1.26.3/1.27.5/1.28.0）
//! --no-start：使用独立验收目录，只验证安装、列表与重载，不启动服务或修改系统配置。

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let no_start = args.iter().any(|arg| arg == "--no-start");
    let base = std::path::PathBuf::from(if no_start {
        ".remote-home/catalog-visibility"
    } else {
        ".remote-home"
    });
    std::fs::create_dir_all(&base).unwrap();
    std::env::set_var("NSB_SKIP_HOSTS", "1");
    let state = if no_start {
        use std::sync::Arc;
        let paths = nsb_core::paths::Paths::new(std::fs::canonicalize(&base).unwrap());
        paths.ensure_dirs().unwrap();
        let store = nsb_core::store::Store::open(paths.db()).unwrap();
        store.set_setting("pathEnvEnabled", "0").unwrap();
        Arc::new(nsb_core::CoreState {
            installer: nsb_core::install::Installer::bundled(),
            paths,
            store,
            manager: Arc::new(nsb_core::services::ServiceManager::new()),
            downloader: Arc::new(nsb_core::download::Downloader::new()),
            emit: Arc::new(|_| {}),
            watchdog: Arc::new(nsb_core::watchdog::Watchdog::new()),
        })
    } else {
        nsb_core::CoreState::init(Some(base.clone()), std::sync::Arc::new(|_| {})).unwrap()
    };
    // 固定用安全档（8080 等高位端口），避免与机器上其它 Web 服务器抢 80
    state.store.set_setting("portProfile", "safe").unwrap();

    let args: Vec<_> = args.into_iter().filter(|arg| arg != "--no-start").collect();
    let keys: Vec<String> = if args.is_empty() {
        vec!["nginx@1.28.1".into()]
    } else {
        args
    };

    let mut fail = 0usize;
    for key in &keys {
        let (id, ver) = key.split_once('@').expect("格式：id@version");

        // 1) 必须是「清单里没有」的版本，否则测不到远程路径
        let in_manifest = state
            .installer
            .manifest
            .packages
            .iter()
            .any(|p| p.id == id && p.version == ver);
        if in_manifest {
            println!("[SKIP] {key} 清单里已有该版本，换一个清单外的版本才测得到远程路径");
            continue;
        }

        // 2) 版本源应能枚举到它
        let cat = state.version_catalog(id, true).await.unwrap();
        let Some(rv) = cat.remote.iter().find(|r| r.version == ver) else {
            println!(
                "[FAIL] {key} 版本源里没有该版本（枚举 {} 个, online={}）",
                cat.remote.len(),
                cat.online
            );
            if let Some(e) = &cat.error {
                println!("       error: {e}");
            }
            fail += 1;
            continue;
        };
        println!(
            "[PASS] 版本枚举       {key} url={} sha={}",
            rv.url.rsplit('/').next().unwrap_or(""),
            rv.sha256.is_some()
        );

        // 3) 真实安装（含下载/校验/解压）
        match state.install_package(key).await {
            Ok(p) => {
                let entry = state.installer.installed_entry(&p);
                let entry_ok = std::path::Path::new(&p.install_path)
                    .join(nsb_core::install::entry_relative_path(&entry.entry))
                    .exists();
                println!(
                    "[PASS] 远程版本安装   {key} → {}（入口存在={entry_ok}）",
                    p.install_path
                );
                if !entry_ok {
                    fail += 1;
                    continue;
                }

                // UI 读取的同一接口必须包含这条清单外的真实安装记录，且只出现一次。
                let listed = state.list_packages().unwrap();
                let matches: Vec<_> = listed
                    .iter()
                    .filter(|v| v.manifest.id == id && v.manifest.version == ver)
                    .collect();
                if matches.len() != 1
                    || matches[0].install.is_none()
                    || !matches[0].available_versions.contains(&ver.to_string())
                {
                    println!("[FAIL] 已安装版本列表 {key} 未正确合并");
                    fail += 1;
                    continue;
                }
                println!("[PASS] 已安装版本列表 {key} 安装记录、版本列表与去重正确");

                // 重新打开本地记录，清空上游目录后仍然能恢复实际入口。
                let reopened = nsb_core::store::Store::open(state.paths.db()).unwrap();
                nsb_core::versions::clear_cache(&reopened);
                let recovered = reopened.find_installed(id, Some(ver)).unwrap();
                let empty = nsb_core::install::Installer {
                    manifest: nsb_core::model::Manifest {
                        revision: 0,
                        packages: vec![],
                    },
                };
                let recovered_views = empty.package_views(&[recovered.clone()]);
                if recovered_views.len() != 1
                    || recovered_views[0].install.is_none()
                    || recovered_views[0].manifest.entry != entry.entry
                {
                    println!("[FAIL] 离线重载 {key} 安装描述未保存");
                    fail += 1;
                    continue;
                }
                println!("[PASS] 离线重载       {key} 无清单、无上游缓存时仍可识别");

                // 旧版本安装没有元信息文件，也必须根据安装记录出现在列表中。
                let mut legacy = recovered.clone();
                legacy.install_path = base
                    .join("legacy-without-snapshot")
                    .to_string_lossy()
                    .into_owned();
                let legacy_views = state.installer.package_views(&[legacy]);
                if !legacy_views.iter().any(|v| {
                    v.manifest.id == id
                        && v.manifest.version == ver
                        && v.install.is_some()
                        && v.manifest.entry == entry.entry
                }) {
                    println!("[FAIL] 旧版记录恢复 {key}");
                    fail += 1;
                    continue;
                }
                println!("[PASS] 旧版记录恢复   {key} 无安装描述的历史记录仍显示已安装");

                nsb_core::ops::set_active_version(&reopened, id, ver).unwrap();
                let expected_bin = nsb_core::pathenv::bin_dir_for(&p.install_path, &entry.entry);
                let actual_dirs =
                    nsb_core::pathenv::desired_dirs(&reopened, &state.installer.manifest);
                if expected_bin.is_some_and(|bin| !actual_dirs.contains(&bin)) {
                    println!("[FAIL] 命令目录恢复 {key}");
                    fail += 1;
                    continue;
                }
                println!("[PASS] 命令目录恢复   {key} 使用实际安装版本的入口");

                if entry.run.is_some() {
                    let service_id = nsb_core::generic::service_id_of(&entry);
                    nsb_core::ops::register_services(&state.paths, &reopened, &state.manager);
                    nsb_core::generic::register_services(&state.paths, &reopened, &state.manager);
                    let metadata = nsb_core::generic::manifest_entry_for(&reopened, &service_id);
                    if !state
                        .manager
                        .list_status()
                        .iter()
                        .any(|s| s.id == service_id)
                        || metadata.is_none_or(|m| m.version != ver || m.entry != entry.entry)
                    {
                        println!("[FAIL] 服务信息恢复 {key}");
                        fail += 1;
                        continue;
                    }
                    println!("[PASS] 服务信息恢复   {key} 可注册且入口与已安装版本一致");
                }

                if no_start {
                    println!("[SKIP] 服务启动       --no-start");
                    continue;
                }

                // 4) 应注册为可启停服务并能启动（nginx 走内置编排）
                match state.start_service(id) {
                    Ok(_) => {
                        println!("[PASS] 启动已装远程版 {id} 健康检查通过");
                        let _ = state.stop_service(id);
                    }
                    Err(e) => {
                        println!("[FAIL] 启动 {id}：{}", e.message);
                        fail += 1;
                    }
                }
            }
            Err(e) => {
                println!("[FAIL] 安装 {key}：{}", e.message);
                fail += 1;
            }
        }
    }
    println!("\n==== check_remote_install: {} fail ====", fail);
    if fail > 0 {
        std::process::exit(1);
    }
}
