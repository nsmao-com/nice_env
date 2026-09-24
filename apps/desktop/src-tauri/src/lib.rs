//! NiceEnv 桌面壳：Tauri 命令接线 + 托盘 + 窗口行为。
//! 业务逻辑全部在 crates/core，这里只做参数转换与 UI 适配。

pub mod smoke;
pub mod tray;

use nsb_core::{AppError, CoreState, Event, EventSink};
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use tauri_plugin_autostart::MacosLauncher;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let handle = app.handle().clone();
            let emit: EventSink = std::sync::Arc::new(move |e: Event| {
                let _ = handle.emit(e.channel(), e.payload());
            });
            let state = CoreState::init(None, emit).map_err(|e| {
                Box::new(std::io::Error::other(format!("{e}"))) as Box<dyn std::error::Error>
            })?;
            app.manage(state);

            /* ---------- 证书自动化调度：启动 30s 后先补一轮，之后每小时检查到期 ---------- */
            {
                let st = app.state::<Arc<CoreState>>().inner().clone();
                nsb_core::certauto::spawn_scheduler(st);
            }

            /* ---------- 托盘 ---------- */
            let tray_state = app.state::<Arc<CoreState>>().inner().clone();
            tray::build(app.handle(), tray_state.clone())?;

            /* ---------- 启动时自动拉起指定服务栈（设置里可配） ---------- */
            if let Some(stack_id) = tray_state
                .store
                .get_setting("startStackOnLaunch")
                .filter(|s| !s.trim().is_empty())
            {
                let st = tray_state.clone();
                let handle_for_stack = app.handle().clone();
                std::thread::spawn(move || {
                    let _ = st.start_stack(&stack_id);
                    tray::refresh(&handle_for_stack);
                });
            }

            /* ---------- 服务看门狗：意外退出自动拉起 ---------- */
            // 只在用户开启时真正做事（enabled 由 CoreState 判断）。
            // 轮询间隔取自设置，默认 3s——够快，又不至于让服务列表被不停地查。
            {
                let st = tray_state.clone();
                let handle_for_wd = app.handle().clone();
                std::thread::spawn(move || loop {
                    let cfg = st.watchdog_config();
                    std::thread::sleep(std::time::Duration::from_secs(cfg.interval_sec.max(1)));
                    if !st.watchdog_config().enabled {
                        continue;
                    }
                    let acted = st.watchdog_tick();
                    if !acted.is_empty() {
                        // 重启改变了运行态，托盘勾选/角标要跟着刷新
                        tray::refresh(&handle_for_wd);
                    }
                });
            }

            /* ---------- 关闭窗口 → 最小化到托盘 ---------- */
            let window = app.get_webview_window("main").ok_or("找不到主窗口")?;
            /* 窗口在配置里以无装饰创建（Windows/Linux 自定义标题栏）；
            macOS 恢复装饰并改为 Overlay 标题栏，保留系统红绿灯 */
            #[cfg(target_os = "macos")]
            {
                let _ = window.set_decorations(true);
                let _ = window.set_title_bar_style(tauri::TitleBarStyle::Overlay);
            }
            let handle_for_close = app.handle().clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    let state = handle_for_close.state::<Arc<CoreState>>();
                    let minimize = state.store.get_setting_or::<bool>("minimizeToTray");
                    if minimize {
                        api.prevent_close();
                        if let Some(w) = handle_for_close.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    } else {
                        stop_all_and_clear_pidfile(&state);
                    }
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // 套件
            list_packages,
            install_package,
            uninstall_package,
            cancel_download,
            set_active_version,
            pathenv_status,
            pathenv_set_enabled,
            pathenv_set_selected,
            pathenv_reapply,
            version_catalog,
            version_catalogs,
            // 服务
            list_service_status,
            start_service,
            stop_service,
            restart_service,
            service_history,
            export_log,
            validate_configs,
            read_text_file,
            write_text_file,
            migrate_list_source,
            migrate_import,
            dns_interfaces,
            dns_status_of,
            dns_takeover,
            dns_restore,
            // 看门狗
            watchdog_status,
            watchdog_set_enabled,
            watchdog_reset,
            // 服务栈
            list_stacks,
            save_stack,
            duplicate_stack,
            delete_stack,
            start_stack,
            stop_stack,
            // 站点
            list_sites,
            create_site,
            update_site,
            delete_site,
            start_site,
            stop_site,
            // hosts / 证书
            read_hosts,
            apply_hosts,
            rebuild_hosts,
            list_certs,
            issue_cert,
            reissue_site_certs,
            trust_ca,
            // 项目扫描
            scan_projects,
            // 日志导出
            log_export,
            // 工具链镜像
            tool_mirrors,
            tool_mirror_set,
            tool_mirror_reset,
            // 批量站点操作
            sites_start_many,
            sites_stop_many,
            // 批量服务操作
            bulk_start,
            bulk_stop,
            bulk_restart,
            bulk_summary,
            // 环境体检
            health_check,
            // 诊断包
            diagnostics_build,
            diagnostics_save,
            // 站点 .env
            env_read,
            env_save,
            env_apply_db,
            // 证书体检
            cert_health,
            cert_import,
            cert_imported_list,
            cert_imported_delete,
            cert_import_dir,
            // 证书自动化（ACME 签发 / 定时续签 / 多平台部署）
            certauto_list,
            certauto_save,
            certauto_delete,
            certauto_set_enabled,
            certauto_issue,
            // 证书监控 + PFX 导出
            certmonitor_list,
            certmonitor_add,
            certmonitor_delete,
            certmonitor_check,
            cert_export_pfx,
            cert_export_der,
            cert_export_jks,
            cert_export_pem,
            // 配置文件编辑
            config_list,
            config_read,
            config_validate,
            config_save,
            config_backups,
            config_rollback,
            // PHP 扩展
            php_extensions,
            set_php_extension,
            set_php_ini_toggle,
            // Xdebug 调试
            xdebug_status,
            xdebug_setup,
            xdebug_toggle,
            // 日志 / 诊断 / 统计
            tail_logs,
            diagnose_port,
            scan_ports,
            scan_port_range,
            close_port,
            kill_pid,
            get_system_stats,
            list_backups,
            restore_backup,
            // 打开外部
            open_in_browser,
            open_in_folder,
            open_terminal,
            // 数据库
            db_list,
            db_create,
            db_drop,
            db_users,
            db_create_user,
            db_reset_root_password,
            db_root_password,
            redis_stats,
            // 数据库备份 / 还原
            db_backup_list,
            db_backup_dump,
            db_backup_restore,
            db_backup_delete,
            db_backup_dir,
            // 代理
            proxy_status,
            proxy_start,
            proxy_stop,
            proxy_set_system,
            proxy_set_mode,
            proxy_profiles,
            proxy_import,
            proxy_activate_profile,
            proxy_delete_profile,
            proxy_nodes,
            proxy_select_node,
            proxy_delay_test,
            proxy_connections,
            proxy_update_profile,
            // 工具箱扩展：计划任务 / 快速隧道 / Ollama / Adminer
            cron_jobs,
            cron_save,
            cron_delete,
            cron_set_enabled,
            cron_run_now,
            tunnel_start,
            tunnel_list,
            tunnel_stop,
            ollama_models,
            ollama_delete,
            ollama_pull,
            adminer_start,
            adminer_stop,
            // 设置
            get_settings,
            set_setting,
            set_port_override,
            get_app_version,
            check_updates,
            // 套件清单：远端刷新 / 恢复内置 / 状态
            refresh_remote_manifest,
            reset_remote_manifest,
            manifest_status,
            export_config,
            import_config,
            import_config_text,
            get_data_dir,
            quit_app,
            // 应用更新（在线下载 + 就地安装）
            download_update,
            install_update,
            open_update_dir,
            // 托盘面板（自绘小窗：数据 / 动作 / 尺寸与显隐）
            tray::tray_panel_state,
            tray::tray_stop_all,
            tray::tray_open_main,
            tray::tray_panel_resize,
            tray::tray_panel_hide,
        ])
        .run(tauri::generate_context!())
        .expect("NiceEnv 启动失败");
}

/// 退出前收尾：停掉本应用拉起的服务，并清掉 pidfile
/// （清掉是必要的——否则下次启动会把「已经正常停掉的」pid 当成残留再去 kill 一遍，
/// 而那些 pid 可能已被系统分配给无关进程）
fn stop_all_and_clear_pidfile(state: &CoreState) {
    nsb_core::ops::stop_all(&state.store, &state.paths, &state.manager);
    let path = state.paths.data().join("run").join("pids.json");
    let _ = std::fs::remove_file(path);
}

/* ================= 错误转换 ================= */

fn box_err(e: AppError) -> tauri::Error {
    let msg = serde_json::to_string(&e).unwrap_or_else(|_| format!("{e}"));
    tauri::Error::Anyhow(anyhow::anyhow!(msg))
}

fn map_jh<T>(r: nsb_core::error::Result<T>) -> Result<T, tauri::Error> {
    r.map_err(box_err)
}

/* ================= 套件 ================= */

#[tauri::command]
fn list_packages(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::PackageView>, tauri::Error> {
    map_jh(state.list_packages())
}

#[tauri::command]
async fn install_package(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        tauri::async_runtime::block_on(async move {
            map_jh(st.install_package(&id).await.map(|_| true))
        })
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn uninstall_package(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    map_jh(state.uninstall_package(&id).map(|_| true))
}

/// 切换单实例服务的「使用中版本」（仅已装版本）
#[tauri::command]
fn set_active_version(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    version: String,
) -> Result<bool, tauri::Error> {
    // 走门面而非直接 ops：切版本后要把 PATH 里的目录一并指过去
    map_jh(state.set_active_version(&id, &version).map(|_| true))
}

/* ================= 环境变量（PATH 注入） ================= */

/// 环境变量注入状态：总开关、已写入的目录、每个包可用的命令
#[tauri::command]
fn pathenv_status(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> nsb_core::model::PathEnvStatus {
    state.pathenv_status()
}

#[tauri::command]
fn pathenv_set_enabled(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    enabled: bool,
) -> Result<nsb_core::model::PathEnvStatus, tauri::Error> {
    map_jh(state.pathenv_set_enabled(enabled))
}

#[tauri::command]
fn pathenv_set_selected(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::model::PathEnvStatus, tauri::Error> {
    map_jh(state.pathenv_set_selected(ids))
}

#[tauri::command]
fn pathenv_reapply(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<nsb_core::model::PathEnvStatus, tauri::Error> {
    map_jh(state.pathenv_reapply())
}

/// 某包的完整版本目录（远程枚举 + 缓存）；force=true 忽略缓存
#[tauri::command]
async fn version_catalog(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    force: Option<bool>,
) -> Result<nsb_core::model::VersionCatalog, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        tauri::async_runtime::block_on(async move {
            map_jh(st.version_catalog(&id, force.unwrap_or(false)).await)
        })
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 批量版本目录（一次拉全部有版本源的包）
#[tauri::command]
async fn version_catalogs(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    force: Option<bool>,
) -> Result<Vec<nsb_core::model::VersionCatalog>, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        tauri::async_runtime::block_on(
            async move { st.version_catalogs(force.unwrap_or(false)).await },
        )
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))
}

#[tauri::command]
fn cancel_download(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    task_id: String,
) -> Result<bool, tauri::Error> {
    state.downloader.cancel(&task_id);
    Ok(true)
}

/* ================= 服务 ================= */

#[tauri::command]
fn list_service_status(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Vec<nsb_core::model::ServiceStatus> {
    state.service_status_list()
}

#[tauri::command]
async fn start_service(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    // 端口被占：默认先把占用人收掉再启动（设置里可关掉这个行为）。
    // 收掉了哪些端口要如实告诉用户——悄悄结束别人的进程是不可接受的。
    if st
        .store
        .get_setting("autoClosePortOnStart")
        .map(|v| v != "false")
        .unwrap_or(true)
    {
        let st2 = st.clone();
        let sid = id.clone();
        let freed = tauri::async_runtime::spawn_blocking(move || free_ports_for(&st2, &sid))
            .await
            .unwrap_or_default();
        if !freed.is_empty() {
            let _ = app.emit(
                "ports://auto-freed",
                serde_json::json!({ "serviceId": id, "freed": freed }),
            );
        }
    }
    let r =
        tauri::async_runtime::spawn_blocking(move || map_jh(st.start_service(&id).map(|_| true)))
            .await
            .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    crate::tray::refresh(&app);
    r
}

#[tauri::command]
async fn stop_service(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    let r =
        tauri::async_runtime::spawn_blocking(move || map_jh(st.stop_service(&id).map(|_| true)))
            .await
            .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    crate::tray::refresh(&app);
    r
}

#[tauri::command]
async fn restart_service(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    let r = tauri::async_runtime::spawn_blocking(move || {
        st.stop_service(&id).ok();
        map_jh(st.start_service(&id).map(|_| true))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    crate::tray::refresh(&app);
    r
}

/// 启动服务前，把这个服务需要绑定的端口上的占用者收掉。
/// - 占用者是本应用自己的服务 → 走优雅停止（MySQL 干净关库）
/// - 是外部进程 → 直接结束（用户已在设置里确认过这个默认行为）
/// 任何失败都静默忽略：真正的启动错误会在随后的 start_service 里如实报出来。
fn free_ports_for(state: &std::sync::Arc<nsb_core::CoreState>, service_id: &str) -> Vec<u16> {
    let mut freed = Vec::new();
    for port in ports_of_service(state, service_id) {
        if nsb_core::services::tcp_port_open(port) {
            if state.close_port(port).is_ok() {
                freed.push(port);
            }
        }
    }
    freed
}

/// 某服务启动时会绑定的端口（与 ops 里各 start_* 的 precheck 对齐）
fn ports_of_service(state: &std::sync::Arc<nsb_core::CoreState>, service_id: &str) -> Vec<u16> {
    let p = nsb_core::services::PortsProfile::from_settings(&state.store);
    match service_id {
        "nginx" => vec![p.http, p.https],
        "apache" => vec![p.apache_http, p.apache_https],
        "redis" => vec![p.redis],
        "postgresql" => vec![p.postgres],
        "mongodb" => vec![p.mongodb],
        "mihomo" => vec![
            nsb_core::configgen::MIHOMO_MIXED_PORT,
            nsb_core::configgen::MIHOMO_CONTROLLER_PORT,
        ],
        s if s.starts_with("mysql@") => vec![p.mysql],
        s if s.starts_with("php@") => state
            .store
            .get_port_assign(s)
            .map(|base| {
                (0..nsb_core::configgen::PHP_POOL_WORKERS)
                    .map(|i| base + i)
                    .collect()
            })
            .unwrap_or_default(),
        // 清单驱动的通用服务：按当前分配表/清单推导
        other => nsb_core::generic::planned_port(&state.store, other)
            .into_iter()
            .collect(),
    }
}

/* ================= 服务栈 ================= */

#[tauri::command]
fn list_stacks(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::Stack>, tauri::Error> {
    map_jh(state.list_stacks())
}

#[tauri::command]
fn save_stack(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    input: nsb_core::model::StackInput,
) -> Result<nsb_core::model::Stack, tauri::Error> {
    map_jh(state.save_stack(input))
}

#[tauri::command]
fn duplicate_stack(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    name: Option<String>,
) -> Result<nsb_core::model::Stack, tauri::Error> {
    map_jh(state.duplicate_stack(&id, name))
}

#[tauri::command]
fn delete_stack(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    map_jh(state.delete_stack(&id).map(|_| true))
}

/// 一键启动整栈；返回逐项结果（单项失败不阻断其它项）
#[tauri::command]
async fn start_stack(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<nsb_core::model::StackStartReport, tauri::Error> {
    let st = state.inner().clone();
    let report = tauri::async_runtime::spawn_blocking(move || map_jh(st.start_stack(&id)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))??;
    // 栈可能改变了服务状态：让托盘菜单与前端同步刷新
    crate::tray::refresh(&app);
    Ok(report)
}

#[tauri::command]
async fn stop_stack(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<nsb_core::model::StackStartReport, tauri::Error> {
    let st = state.inner().clone();
    let report = tauri::async_runtime::spawn_blocking(move || map_jh(st.stop_stack(&id)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))??;
    crate::tray::refresh(&app);
    Ok(report)
}

/* ================= 站点 ================= */

#[tauri::command]
fn list_sites(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::Site>, tauri::Error> {
    map_jh(nsb_core::sites::list_with_status(
        &state.paths,
        &state.store,
    ))
}

#[tauri::command]
async fn create_site(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    input: nsb_core::model::CreateSiteInput,
) -> Result<nsb_core::model::Site, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::sites::create(
            &input,
            &st.paths,
            &st.store,
            &st.manager,
        ))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn update_site(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    site: nsb_core::model::Site,
) -> Result<nsb_core::model::Site, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::sites::update(
            &site,
            &st.paths,
            &st.store,
            &st.manager,
        ))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn delete_site(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    hosts: Option<bool>,
    certs: Option<bool>,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(
            nsb_core::sites::delete(
                &id,
                hosts.unwrap_or(true),
                certs.unwrap_or(true),
                &st.paths,
                &st.store,
                &st.manager,
            )
            .map(|_| true),
        )
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn start_site(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::sites::start_site(&id, &st.paths, &st.store, &st.manager).map(|_| true))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn stop_site(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::sites::stop_site(&id, &st.paths, &st.store, &st.manager).map(|_| true))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/* ================= hosts / 证书 ================= */

#[tauri::command]
fn read_hosts() -> Result<Vec<nsb_core::model::HostsEntry>, tauri::Error> {
    map_jh(nsb_core::hosts::read_all())
}

#[tauri::command]
fn apply_hosts(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    entries: Vec<nsb_core::model::HostsEntry>,
) -> Result<bool, tauri::Error> {
    map_jh(nsb_core::hosts::apply(&state.store, &state.paths, Some(entries)).map(|_| true))
}

// ---- 证书自动化：签发耗时（ACME 全流程 1–2 分钟），手动签发放后台线程，前端轮询列表 ----
#[tauri::command]
fn certauto_list(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::CertAutomation>, tauri::Error> {
    map_jh(state.certauto_list())
}

#[tauri::command]
fn certauto_save(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    a: nsb_core::model::CertAutomation,
) -> Result<nsb_core::model::CertAutomation, tauri::Error> {
    map_jh(state.certauto_save(a))
}

#[tauri::command]
fn certauto_delete(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    map_jh(state.certauto_delete(&id))
}

#[tauri::command]
fn certauto_set_enabled(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    enabled: bool,
) -> Result<nsb_core::model::CertAutomation, tauri::Error> {
    map_jh(state.certauto_set_enabled(&id, enabled))
}

#[tauri::command]
fn certauto_issue(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    std::thread::spawn(move || {
        let _ = nsb_core::certauto::run_once(&st, &id);
    });
    Ok(true)
}

#[tauri::command]
fn list_certs(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::CertRecord>, tauri::Error> {
    map_jh(nsb_core::tls::list_certs(&state.paths, &state.store))
}

#[tauri::command]
async fn cert_export_pem(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    cert_id: String,
    out_path: String,
) -> Result<String, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::tls::export_pem_bundle(
            &st.store,
            &cert_id,
            std::path::Path::new(&out_path),
        ))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn cert_export_jks(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    cert_id: String,
    password: String,
    out_path: String,
) -> Result<String, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::tls::export_jks(
            &st.store,
            &cert_id,
            &password,
            std::path::Path::new(&out_path),
        ))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn cert_export_der(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    cert_id: String,
    out_path: String,
) -> Result<String, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::tls::export_der(
            &st.store,
            &cert_id,
            std::path::Path::new(&out_path),
        ))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

// ---- 网站证书监控：盯任意站点/设备的证书到期时间 ----
#[tauri::command]
fn certmonitor_list(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::CertMonitor>, tauri::Error> {
    map_jh(state.certmonitor_list())
}

#[tauri::command]
fn certmonitor_add(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    m: nsb_core::model::CertMonitor,
) -> Result<nsb_core::model::CertMonitor, tauri::Error> {
    map_jh(state.certmonitor_add(m))
}

#[tauri::command]
fn certmonitor_delete(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    map_jh(state.certmonitor_delete(&id))
}

#[tauri::command]
async fn certmonitor_check(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<nsb_core::model::CertMonitor, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.certmonitor_check(&id)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

// ---- PFX 导出：本机证书 + 私钥打包 PKCS#12（Windows IIS / 设备导入用） ----
#[tauri::command]
async fn cert_export_pfx(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    cert_id: String,
    password: String,
    out_path: String,
) -> Result<String, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::tls::export_pfx(
            &st.store,
            &cert_id,
            &password,
            std::path::Path::new(&out_path),
        ))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn issue_cert(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    domain: String,
    sans: Vec<String>,
) -> Result<nsb_core::model::CertRecord, tauri::Error> {
    let mut domains = vec![domain];
    domains.extend(sans);
    map_jh(nsb_core::tls::issue_site_cert(
        &state.paths,
        &state.store,
        &domains,
    ))
}

#[tauri::command]
fn trust_ca(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> Result<bool, tauri::Error> {
    map_jh(nsb_core::tls::trust_ca(&state.paths).map(|_| true))
}

/// 按当前站点重建 hosts 托管块（保留用户手动条目）
#[tauri::command]
fn rebuild_hosts(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    map_jh(nsb_core::hosts::rebuild(&state.store, &state.paths).map(|_| true))
}

/// 补齐缺失/过期的站点证书，返回重新签发的域名
#[tauri::command]
fn reissue_site_certs(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<String>, tauri::Error> {
    map_jh(nsb_core::tls::reissue_missing_site_certs(
        &state.paths,
        &state.store,
    ))
}

/* ================= 日志 / 诊断 / 统计 ================= */

#[tauri::command]
fn tail_logs(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    lines: Option<usize>,
) -> Vec<nsb_core::model::LogLine> {
    state.tail_logs(&id, lines.unwrap_or(200))
}

#[tauri::command]
fn diagnose_port(port: u16) -> Result<nsb_core::model::PortDiagnosis, tauri::Error> {
    map_jh(nsb_core::ports::diagnose_port(port))
}

/// 全量端口体检：本应用所有待绑定端口 vs 实际占用者
#[tauri::command]
fn scan_ports(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::PortScanEntry>, tauri::Error> {
    map_jh(nsb_core::ports::scan_app_ports(
        &state.store,
        &state.manager,
    ))
}

/// 端口区间扫描（工具箱）：from == to 即单端口查询，返回占用者与归属
#[tauri::command]
fn scan_port_range(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    from: u16,
    to: u16,
) -> Result<nsb_core::model::PortRangeScan, tauri::Error> {
    map_jh(state.scan_port_range(from, to))
}

/// 结束占用某端口的进程：本应用服务走优雅停止，外部进程直接 kill。
/// 用户明确点了按钮才会调到这里。
#[tauri::command]
async fn close_port(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    port: u16,
) -> Result<nsb_core::ports::ClosePortOutcome, tauri::Error> {
    let st = state.inner().clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || map_jh(st.close_port(port)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))??;
    crate::tray::refresh(&app);
    Ok(outcome)
}

/// 备份目录列表（配置变更前自动生成的 .bak）
#[tauri::command]
fn list_backups(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> Vec<serde_json::Value> {
    nsb_core::paths::list_backups(&state.paths.base)
        .into_iter()
        .map(|(name, path, size, mtime)| {
            serde_json::json!({ "name": name, "path": path, "sizeBytes": size, "modifiedAt": mtime })
        })
        .collect()
}

/// 从备份恢复单个配置文件
#[tauri::command]
fn restore_backup(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
) -> Result<String, tauri::Error> {
    let r = nsb_core::paths::restore_backup(&state.paths.base, &name)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
    Ok(r.to_string_lossy().to_string())
}

#[tauri::command]
fn kill_pid(pid: u32) -> Result<bool, tauri::Error> {
    map_jh(nsb_core::ports::kill_pid(pid))
}

#[tauri::command]
fn get_system_stats() -> nsb_core::model::SystemStats {
    nsb_core::stats::get_system_stats()
}

/// 配置只读体检：nginx -t / httpd -t / php ini 加载 / 配置文件存在性。
/// 与「重写配置」不同：这里不改任何文件。
#[tauri::command]
fn validate_configs(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::ops::ConfigCheck>, tauri::Error> {
    map_jh(Ok(nsb_core::ops::validate_configs(
        &state.store,
        &state.paths,
    )))
}

/// Redis 运行统计（内存 / 键数 / 连接数 / 运行天数）
#[tauri::command]
fn redis_stats(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> nsb_core::stats::RedisStats {
    let ports = nsb_core::services::PortsProfile::from_settings(&state.store);
    state.redis_stats(ports.redis)
}

/// 已连接的网络接口（供 DNS 接管选择）
#[tauri::command]
fn dns_interfaces() -> Result<Vec<String>, tauri::Error> {
    platform::connected_interfaces()
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))
}

/// 指定接口当前 DNS 状态（原始文本）
#[tauri::command]
fn dns_status_of(name: String) -> Result<String, tauri::Error> {
    platform::interface_dns_status(&name)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))
}

/// 提权把接口 DNS 指向 127.0.0.1（本地域名解析接管；触发 UAC）
#[tauri::command]
fn dns_takeover(name: String) -> Result<bool, tauri::Error> {
    platform::set_dns_localhost_elevated(&name)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
    Ok(true)
}

/// 恢复接口 DNS 为自动获取（触发 UAC）
#[tauri::command]
fn dns_restore(name: String) -> Result<bool, tauri::Error> {
    platform::restore_dns_elevated(&name)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
    Ok(true)
}

/// 列出源 MySQL 实例（FlyEnv/phpStudy/ServBay/XAMPP 等）上的用户数据库
#[tauri::command]
async fn migrate_list_source(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    host: String,
    port: u16,
    user: String,
    password: String,
) -> Result<Vec<nsb_core::dbmigrate::SourceDb>, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.migrate_list_source(host, port, user, password))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 把源实例的指定库导入到本地托管 MySQL（mysqldump | mysql 流式管道）
#[tauri::command]
async fn migrate_import(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    host: String,
    port: u16,
    user: String,
    password: String,
    databases: Vec<String>,
) -> Result<nsb_core::dbmigrate::ImportReport, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.migrate_import(host, port, user, password, databases))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// hosts 文件导入用的纯文本读取（限单个文件，返回全文）
#[tauri::command]
fn read_text_file(path: String) -> Result<String, tauri::Error> {
    std::fs::read_to_string(&path)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("读取失败：{e}")))
}

/// hosts 导出用纯文本写入
#[tauri::command]
fn write_text_file(path: String, content: String) -> Result<bool, tauri::Error> {
    std::fs::write(&path, content)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("写入失败：{e}")))?;
    Ok(true)
}

/// 导出服务日志到用户指定路径（前端走保存对话框）。
/// 源 = 该服务的 log 文件（ring 之外还有完整落盘）；不存在时报 LOG_EMPTY。
#[tauri::command]
fn export_log(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    dest: String,
) -> Result<u64, tauri::Error> {
    let src = {
        let status = state.service_status_list().into_iter().find(|s| s.id == id);
        status
            .and_then(|s| s.log_file)
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_file())
            .ok_or_else(|| {
                tauri::Error::Anyhow(anyhow::anyhow!("该服务还没有日志文件（先启动一次）"))
            })?
    };
    let dest_path = std::path::PathBuf::from(&dest);
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("创建目标目录失败：{e}")))?;
    }
    std::fs::copy(&src, &dest_path)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("复制日志失败：{e}")))
}

/// 最近的服务状态变更（新→旧，最多 200 条）
#[tauri::command]
fn service_history(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    n: Option<usize>,
) -> Vec<serde_json::Value> {
    state
        .service_history(n.unwrap_or(50))
        .into_iter()
        .map(|(ts, id, detail)| serde_json::json!({ "ts": ts, "serviceId": id, "detail": detail }))
        .collect()
}

/* ================= 打开外部 ================= */

#[tauri::command]
fn open_in_browser(url: String) -> Result<bool, tauri::Error> {
    open_target(&url, false).map_err(|e| box_err(nsb_core::AppError::new("OPEN_FAILED", e)))
}

#[tauri::command]
fn open_in_folder(path: String) -> Result<bool, tauri::Error> {
    open_target(&path, true).map_err(|e| box_err(nsb_core::AppError::new("OPEN_FAILED", e)))
}

fn open_target(target: &str, folder: bool) -> Result<bool, String> {
    #[cfg(windows)]
    {
        let prog = if folder { "explorer" } else { "cmd" };
        let args: Vec<String> = if folder {
            vec![target.to_string()]
        } else {
            vec!["/c".into(), "start".into(), "".into(), target.to_string()]
        };
        std::process::Command::new(prog)
            .args(&args)
            .spawn()
            .map(|_| true)
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = folder;
        std::process::Command::new("open")
            .arg(target)
            .spawn()
            .map(|_| true)
            .map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn open_terminal(cwd: String) -> Result<bool, tauri::Error> {
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", "wt", "-d"])
            .arg(&cwd)
            .spawn()
            .or_else(|_| {
                std::process::Command::new("cmd")
                    .args(["/c", "start", "cmd", "/K", &format!("cd /d {cwd}")])
                    .spawn()
            })
            .map(|_| true)
            .map_err(|e| box_err(nsb_core::AppError::io("打开终端", e)))
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("open")
            .args(["-a", "Terminal", &cwd])
            .spawn()
            .map(|_| true)
            .map_err(|e| box_err(nsb_core::AppError::io("打开终端", e)))
    }
}

/* ================= 数据库 ================= */

fn mysql_client_of(state: &CoreState) -> nsb_core::error::Result<nsb_core::dbadmin::MySqlClient> {
    let version = state
        .store
        .find_installed("mysql", None)
        .map(|p| p.version)
        .ok_or_else(|| nsb_core::AppError::not_installed("MySQL"))?;
    let ports = nsb_core::services::PortsProfile::from_settings(&state.store);
    let pass = state
        .store
        .get_setting("mysqlRootPassword")
        .unwrap_or_else(|| "root".into());
    Ok(nsb_core::dbadmin::MySqlClient::from_state(
        &state.paths,
        &version,
        ports.mysql,
        pass,
    ))
}

#[tauri::command]
fn db_list(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::DatabaseInfo>, tauri::Error> {
    map_jh(mysql_client_of(&state).and_then(|c| c.list_databases()))
}

#[tauri::command]
fn db_create(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
) -> Result<bool, tauri::Error> {
    map_jh(
        mysql_client_of(&state)
            .and_then(|c| c.create_database(&name))
            .map(|_| true),
    )
}

#[tauri::command]
fn db_drop(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
) -> Result<bool, tauri::Error> {
    map_jh(
        mysql_client_of(&state)
            .and_then(|c| c.drop_database(&name))
            .map(|_| true),
    )
}

#[tauri::command]
fn db_users(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::DbUserInfo>, tauri::Error> {
    map_jh(mysql_client_of(&state).and_then(|c| c.list_users()))
}

#[tauri::command]
fn db_create_user(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    username: String,
    password: String,
    database: String,
) -> Result<bool, tauri::Error> {
    map_jh(
        mysql_client_of(&state)
            .and_then(|c| c.create_user_grant(&username, &password, &database))
            .map(|_| true),
    )
}

#[tauri::command]
fn db_reset_root_password(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    new_password: String,
) -> Result<bool, tauri::Error> {
    let pass = if new_password.is_empty() {
        format!("nsb_{}", rand_string(8))
    } else {
        new_password
    };
    let client = map_jh(mysql_client_of(&state))?;
    map_jh(client.reset_root_password(&pass))?;
    map_jh(
        state
            .store
            .set_setting("mysqlRootPassword", &pass)
            .map(|_| true),
    )?;
    Ok(true)
}

#[tauri::command]
fn db_root_password(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<String, tauri::Error> {
    Ok(state
        .store
        .get_setting("mysqlRootPassword")
        .unwrap_or_else(|| "root".into()))
}

fn rand_string(n: usize) -> String {
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(n)
        .map(char::from)
        .collect()
}

/* ================= 代理（Clash/mihomo） ================= */

#[tauri::command]
fn proxy_status(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> nsb_core::model::serde_proxy::ProxyStatusInfo {
    let running = state
        .manager
        .snapshot("mihomo")
        .map(|s| s.state == nsb_core::model::ServiceState::Running)
        .unwrap_or(false);
    let sys = nsb_core::proxy::system_proxy_state();
    let mode = state
        .store
        .get_setting("proxyMode")
        .unwrap_or_else(|| "rule".into());
    let version = if running {
        nsb_core::proxy::ProxyRuntime::new().version().ok()
    } else {
        None
    };
    nsb_core::model::serde_proxy::ProxyStatusInfo {
        running,
        mixed_port: nsb_core::configgen::MIHOMO_MIXED_PORT,
        controller_port: nsb_core::configgen::MIHOMO_CONTROLLER_PORT,
        mode,
        system_proxy_enabled: sys.enabled,
        version,
    }
}

#[tauri::command]
async fn proxy_start(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.start_service("mihomo").map(|_| true)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn proxy_stop(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    let sys = nsb_core::proxy::system_proxy_state();
    tauri::async_runtime::spawn_blocking(move || {
        if sys.enabled {
            let _ = nsb_core::proxy::system_proxy_off();
        }
        map_jh(st.stop_service("mihomo").map(|_| true))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn proxy_set_system(enabled: bool) -> Result<bool, tauri::Error> {
    let r = if enabled {
        nsb_core::proxy::system_proxy_on()
    } else {
        nsb_core::proxy::system_proxy_off()
    };
    map_jh(r.map(|_| true))
}

#[tauri::command]
fn proxy_set_mode(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    mode: String,
) -> Result<bool, tauri::Error> {
    let rt = nsb_core::proxy::ProxyRuntime::new();
    let client = reqwest::blocking::Client::new();
    let _ = client
        .patch(format!("{}/configs", rt.base_url))
        .body(format!("{{\"mode\":\"{mode}\"}}"))
        .header("Content-Type", "application/json")
        .send();
    map_jh(state.store.set_setting("proxyMode", &mode).map(|_| true))
}

#[tauri::command]
fn proxy_profiles(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::serde_proxy::ProxyProfile>, tauri::Error> {
    let list = map_jh(state.store.list_proxy_profiles())?;
    Ok(list
        .into_iter()
        .map(
            |(id, name, url, active, added_at)| nsb_core::model::serde_proxy::ProxyProfile {
                id,
                name,
                url,
                active,
                added_at,
            },
        )
        .collect())
}

#[tauri::command]
async fn proxy_import(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
    url: String,
) -> Result<nsb_core::model::serde_proxy::ProxyProfile, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let id = map_jh(tauri::async_runtime::block_on(async {
            nsb_core::proxy::import_profile(&name, &url, &st.paths, &st.store).await
        }))?;
        Ok(nsb_core::model::serde_proxy::ProxyProfile {
            id,
            name,
            url,
            active: false,
            added_at: nsb_core::services::now_ms(),
        })
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn proxy_activate_profile(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let running = state
        .manager
        .snapshot("mihomo")
        .map(|s| s.state == nsb_core::model::ServiceState::Running)
        .unwrap_or(false);
    map_jh(nsb_core::proxy::activate_profile(
        &state.paths,
        &state.store,
        &id,
    ))?;
    if running {
        let _ = state.stop_service("mihomo");
        let _ = state.start_service("mihomo");
    }
    Ok(true)
}

#[tauri::command]
fn proxy_delete_profile(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    map_jh(state.store.delete_proxy_profile(&id).map(|_| true))
}

#[tauri::command]
fn proxy_nodes() -> Result<Vec<nsb_core::model::serde_proxy::ProxyGroupView>, tauri::Error> {
    let rt = nsb_core::proxy::ProxyRuntime::new();
    let v = map_jh(rt.proxies())?;
    Ok(nsb_core::model::serde_proxy::parse_groups(&v))
}

/* ================= 工具箱扩展（计划任务 / 快速隧道 / Ollama / Adminer） ================= */

#[tauri::command]
fn cron_jobs(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::cron::CronJob>, tauri::Error> {
    map_jh(state.store.list_cron_jobs())
}

#[tauri::command]
fn cron_save(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    job: nsb_core::cron::CronJob,
) -> Result<bool, tauri::Error> {
    map_jh(state.cron_save(job).map(|_| true))
}

#[tauri::command]
fn cron_delete(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    map_jh(state.cron_delete(&id).map(|_| true))
}

#[tauri::command]
fn cron_set_enabled(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    enabled: bool,
) -> Result<bool, tauri::Error> {
    map_jh(state.cron_set_enabled(&id, enabled).map(|_| true))
}

#[tauri::command]
fn cron_run_now(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<nsb_core::cron::CronJob, tauri::Error> {
    map_jh(state.cron_run_now(&id))
}

#[tauri::command]
fn tunnel_start(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    port: u16,
) -> Result<nsb_core::model::TunnelInfo, tauri::Error> {
    map_jh(state.tunnel_start(port))
}

#[tauri::command]
fn tunnel_list(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Vec<nsb_core::model::TunnelInfo> {
    state.tunnel_list()
}

#[tauri::command]
fn tunnel_stop(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    map_jh(state.tunnel_stop(&id).map(|_| true))
}

#[tauri::command]
async fn ollama_models(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::toolbox::OllamaModelRow>, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.ollama_models()))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn ollama_delete(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
) -> Result<bool, tauri::Error> {
    map_jh(state.ollama_delete(&name).map(|_| true))
}

#[tauri::command]
fn ollama_pull(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
) -> Result<bool, tauri::Error> {
    map_jh(state.ollama_pull(&name).map(|_| true))
}

/// 启动 Adminer 管理台，浏览器打开 http://127.0.0.1:{port}/{file}
#[tauri::command]
fn adminer_start(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<serde_json::Value, tauri::Error> {
    let (port, file) = map_jh(state.adminer_start())?;
    Ok(serde_json::json!({ "port": port, "file": file }))
}

#[tauri::command]
fn adminer_stop(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    map_jh(state.adminer_stop().map(|_| true))
}

#[tauri::command]
fn proxy_connections() -> Result<serde_json::Value, tauri::Error> {
    let rt = nsb_core::proxy::ProxyRuntime::new();
    map_jh(rt.connections())
}

/// 重新拉取订阅并覆盖原文件；该订阅处于激活态且内核在跑时，重启内核生效
#[tauri::command]
async fn proxy_update_profile(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(tauri::async_runtime::block_on(async {
            nsb_core::proxy::update_profile(&st.paths, &st.store, &id).await
        }))?;
        let running = st
            .manager
            .snapshot("mihomo")
            .map(|s| s.state == nsb_core::model::ServiceState::Running)
            .unwrap_or(false);
        if running {
            let _ = st.stop_service("mihomo");
            let _ = st.start_service("mihomo");
        }
        Ok(true)
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn proxy_select_node(group: String, node: String) -> Result<bool, tauri::Error> {
    map_jh(
        nsb_core::proxy::ProxyRuntime::new()
            .select(&group, &node)
            .map(|_| true),
    )
}

#[tauri::command]
async fn proxy_delay_test(node: String) -> Result<u32, tauri::Error> {
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::proxy::ProxyRuntime::new().delay(&node))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/* ================= 设置 ================= */

#[tauri::command]
fn get_settings(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> serde_json::Value {
    let overrides: serde_json::Map<String, serde_json::Value> = state
        .store
        .port_overrides()
        .into_iter()
        .filter_map(|(k, v)| v.parse::<u16>().ok().map(|p| (k, serde_json::json!(p))))
        .collect();
    serde_json::json!({
        "language": state.store.get_setting("language").unwrap_or_else(|| "zh".into()),
        "appearance": state.store.get_setting("appearance").unwrap_or_else(|| "light".into()),
        "accentHue": state.store.get_setting("accentHue").map(|v| v.parse::<i64>().unwrap_or(250)).unwrap_or(250),
        "accentHex": state.store.get_setting("accentHex").unwrap_or_default(),
        "uiFont": state.store.get_setting("uiFont").unwrap_or_else(|| "plex".into()),
        "uiScale": state.store.get_setting("uiScale").map(|v| v.parse::<f64>().unwrap_or(1.0)).unwrap_or(1.0),
        "codeFont": state.store.get_setting("codeFont").unwrap_or_else(|| "plex-mono".into()),
        "codeFontSize": state.store.get_setting("codeFontSize").map(|v| v.parse::<f64>().unwrap_or(11.5)).unwrap_or(11.5),
        "codeLineNumbers": state.store.get_setting("codeLineNumbers").map(|v| v != "false").unwrap_or(true),
        "codeWrap": state.store.get_setting("codeWrap").map(|v| v == "true").unwrap_or(true),
        "codeTheme": state.store.get_setting("codeTheme").unwrap_or_else(|| "auto".into()),
        "codeBg": state.store.get_setting("codeBg").unwrap_or_default(),
        "reduceMotion": state.store.get_setting("reduceMotion").map(|v| v == "true").unwrap_or(false),
        "defaultTld": state.store.get_setting("defaultTld").unwrap_or_else(|| "test".into()),
        "defaultWebServer": state.store.get_setting("defaultWebServer").unwrap_or_else(|| "nginx".into()),
        "portProfile": state.store.get_setting("portProfile").unwrap_or_else(|| "standard".into()),
        "portOverrides": overrides,
        "mirror": state.store.get_setting("mirror").unwrap_or_else(|| "official".into()),
        "customMirror": state.store.get_setting("customMirror").unwrap_or_default(),
        "autostart": state.store.get_setting("autostart").map(|v| v == "true").unwrap_or(false),
        "minimizeToTray": state.store.get_setting("minimizeToTray").map(|v| v != "false").unwrap_or(true),
        "startStackOnLaunch": state.store.get_setting("startStackOnLaunch").unwrap_or_default(),
        "autoClosePortOnStart": state.store.get_setting("autoClosePortOnStart").map(|v| v != "false").unwrap_or(true),
        "manifestUrl": state.store.get_setting("manifestUrl").unwrap_or_default(),
        "checkUpdateOnLaunch": state.store.get_setting("checkUpdateOnLaunch").map(|v| v != "false").unwrap_or(true),
        "autoDownloadUpdate": state.store.get_setting("autoDownloadUpdate").map(|v| v == "true").unwrap_or(false),
        "logTailLines": state.store.get_setting("logTailLines").map(|v| v.parse::<i64>().unwrap_or(500)).unwrap_or(500),
        "logAutoRefresh": state.store.get_setting("logAutoRefresh").map(|v| v != "false").unwrap_or(true),
        "confirmKill": state.store.get_setting("confirmKill").map(|v| v != "false").unwrap_or(true),
        "hideScrollbars": state.store.get_setting("hideScrollbars").map(|v| v != "false").unwrap_or(true),
        "onboardingDone": state.store.get_setting("onboardingDone").map(|v| v == "true").unwrap_or(false),
    })
}

#[tauri::command]
fn set_setting(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    key: String,
    value: serde_json::Value,
) -> Result<bool, tauri::Error> {
    let val = match value {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    state.store.set_setting(&key, &val).map_err(box_err)?;
    // autostart 需要真正落到操作系统（注册表 Run / LaunchAgent），不能只存一个布尔值
    if key == "autostart" {
        use tauri_plugin_autostart::ManagerExt;
        let mgr = app.autolaunch();
        let enable = val == "true";
        let r = if enable { mgr.enable() } else { mgr.disable() };
        if let Err(e) = r {
            return Err(tauri::Error::Anyhow(anyhow::anyhow!(format!(
                "设置开机自启动失败：{e}"
            ))));
        }
    }
    Ok(true)
}

#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
fn get_data_dir(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> String {
    state.paths.base.to_string_lossy().to_string()
}

/* ================= 配置导入/导出 ================= */

#[tauri::command]
fn export_config(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    path: String,
) -> Result<usize, tauri::Error> {
    map_jh(nsb_core::transfer::export_to(
        &state.store,
        std::path::Path::new(&path),
    ))
}

#[tauri::command]
fn import_config(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    path: String,
) -> Result<nsb_core::transfer::ImportReport, tauri::Error> {
    map_jh(nsb_core::transfer::import_from(
        std::path::Path::new(&path),
        &state.paths,
        &state.store,
        &state.manager,
    ))
}

/// 从 JSON 文本导入配置。
/// WebView 里拖进来的文件拿不到真实路径（安全限制），所以前端读文本后走这里：
/// 落到备份目录下的临时文件再复用同一条解析路径，避免两套导入逻辑。
#[tauri::command]
fn import_config_text(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    json: String,
) -> Result<nsb_core::transfer::ImportReport, tauri::Error> {
    let dir = state.paths.base.join("backup");
    std::fs::create_dir_all(&dir)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
    let tmp = dir.join(format!("dropped-{}.json", nsb_core::services::now_ms()));
    std::fs::write(&tmp, json.as_bytes())
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
    let r = nsb_core::transfer::import_from(&tmp, &state.paths, &state.store, &state.manager);
    // 临时文件用完即删：内容已经进库了
    let _ = std::fs::remove_file(&tmp);
    map_jh(r)
}

/// 设置 `portOverride.<key>`：写端口值，传 null 则清除覆盖（回到档位默认）
#[tauri::command]
fn set_port_override(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    key: String,
    port: Option<u16>,
) -> Result<bool, tauri::Error> {
    if port == Some(0) {
        return Err(box_err(nsb_core::AppError::new(
            "BAD_PORT",
            "端口必须是 1–65535 之间的整数",
        )));
    }
    map_jh(state.store.set_port_override(&key, port).map(|_| true))
}

const APP_RELEASES_API: &str = "https://api.github.com/repos/nsmao-com/nice_env/releases/latest";
const APP_RELEASES_PAGE: &str = "https://github.com/nsmao-com/nice_env/releases";

fn version_newer(remote: &str, local: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.trim()
            .trim_start_matches(|c: char| c == 'v' || c == 'V')
            .split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse::<u64>().ok())
            .collect()
    };
    let a = parse(remote);
    let b = parse(local);
    a > b
}

/// GitHub Release 的完整信息：版本 / 页面 / 说明 / 可下载的安装包
struct ReleaseInfo {
    tag: String,
    html_url: String,
    body: String,
    published_at: String,
    /// 与当前平台匹配的安装包（Windows: NSIS .exe；macOS: .dmg），取第一个
    asset_name: Option<String>,
    asset_url: Option<String>,
    asset_size: Option<u64>,
}

/// 当前平台该下哪种安装包（Windows: NSIS 安装器 / macOS: 对应架构的 DMG）。
///
/// Release 里同时有 aarch64 与 x64 两份 macOS 包，只看扩展名会把 Intel 机器
/// 的 .dmg 挑给 Apple Silicon（反之亦然），所以还要匹配 CPU 架构。
fn asset_matches_platform(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    #[cfg(windows)]
    {
        // 只认安装器：x64-setup.exe；.app.tar.gz 之类不是 Windows 包
        n.ends_with(".exe") || n.ends_with(".msi")
    }
    #[cfg(not(windows))]
    {
        let is_mac_pkg = n.ends_with(".dmg") || n.ends_with(".app.tar.gz");
        if !is_mac_pkg {
            return false;
        }
        // 文件名里带 aarch64/arm64 的是 Apple Silicon 包；其余（x64/x86_64）按 Intel 处理
        let arm = n.contains("aarch64") || n.contains("arm64");
        let want_arm = cfg!(target_arch = "aarch64");
        if n.contains("aarch64") || n.contains("arm64") || n.contains("x64") || n.contains("x86_64")
        {
            arm == want_arm
        } else {
            // 名字里没写架构：不冒险挑给用户，交给下面的兜底逻辑
            false
        }
    }
}

fn fetch_latest_release_info(
    client: &reqwest::blocking::Client,
) -> Result<Option<ReleaseInfo>, ()> {
    let resp = client
        .get(APP_RELEASES_API)
        .header("User-Agent", "NiceEnv")
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|_| ())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let v = resp
        .error_for_status()
        .map_err(|_| ())?
        .json::<serde_json::Value>()
        .map_err(|_| ())?;
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(())?
        .to_string();
    let html_url = v
        .get("html_url")
        .and_then(|t| t.as_str())
        .unwrap_or(APP_RELEASES_PAGE)
        .to_string();
    let body = v
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();
    let published_at = v
        .get("published_at")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_string();

    // 优先挑与当前平台匹配的安装包；没有就退到第一个非源码包
    let mut asset_name = None;
    let mut asset_url = None;
    let mut asset_size = None;
    let mut fallback: Option<(String, String, u64)> = None;
    if let Some(list) = v.get("assets").and_then(|a| a.as_array()) {
        for a in list {
            let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let url = a
                .get("browser_download_url")
                .and_then(|u| u.as_str())
                .unwrap_or("");
            if name.is_empty() || url.is_empty() {
                continue;
            }
            let size = a.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
            if asset_matches_platform(name) && asset_name.is_none() {
                asset_name = Some(name.to_string());
                asset_url = Some(url.to_string());
                asset_size = Some(size);
            }
            if fallback.is_none()
                && (name.ends_with(".exe") || name.ends_with(".dmg") || name.ends_with(".msi"))
            {
                fallback = Some((name.to_string(), url.to_string(), size));
            }
        }
    }
    if asset_name.is_none() {
        if let Some((n, u, s)) = fallback {
            asset_name = Some(n);
            asset_url = Some(u);
            asset_size = Some(s);
        }
    }

    Ok(Some(ReleaseInfo {
        tag,
        html_url,
        body,
        published_at,
        asset_name,
        asset_url,
        asset_size,
    }))
}

/* ================= 清单：远端刷新 / 用户模块 / 状态 ================= */

/// 拉取远端清单（设置项 manifestUrl）→ 校验 → 落盘为 etc/manifest.json 快照。
/// 快照在**下次启动**生效（Installer::effective 会叠加它）；
/// 返回快照的 revision / 条目数，前端提示「重启后生效」。
#[tauri::command]
fn refresh_remote_manifest(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    url: Option<String>,
) -> Result<serde_json::Value, tauri::Error> {
    let target = url
        .or_else(|| state.store.get_setting("manifestUrl"))
        .filter(|u| !u.trim().is_empty())
        .ok_or_else(|| {
            tauri::Error::Anyhow(anyhow::anyhow!(
                "未配置远端清单地址（设置 → 更新 → manifestUrl）"
            ))
        })?;
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    let raw = client
        .get(&target)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("下载失败：{e}")))?;
    let m = nsb_core::install::parse_manifest_str(&raw)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
    let dest = state.paths.etc().join("manifest.json");
    std::fs::create_dir_all(state.paths.etc()).ok();
    std::fs::write(&dest, &raw)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("写入快照失败：{e}")))?;
    Ok(serde_json::json!({
        "revision": m.revision,
        "packages": m.packages.len(),
        "path": dest.to_string_lossy(),
        "takesEffect": "restart",
    }))
}

/// 删除远端清单快照，回退到内置清单（下次启动生效）
#[tauri::command]
fn reset_remote_manifest(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    let dest = state.paths.etc().join("manifest.json");
    if dest.exists() {
        std::fs::remove_file(&dest)
            .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("删除快照失败：{e}")))?;
    }
    Ok(true)
}

/// 当前生效清单的状态：来源（内置/远端/含用户模块）、revision、条目数、
/// 用户模块文件列表（含解析失败的文件，便于用户自查）。
#[tauri::command]
fn manifest_status(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> serde_json::Value {
    let bundled = nsb_core::install::Installer::bundled();
    let eff = nsb_core::install::Installer::effective(&state.paths);
    let snap = state.paths.etc().join("manifest.json");
    let remote_active = snap.is_file();
    let dir = state.paths.base.join("user-modules");
    let mut modules = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.extension().map(|x| x == "json").unwrap_or(false) {
                let ok = std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|r| nsb_core::install::parse_manifest_str(&r).ok())
                    .is_some();
                modules.push(serde_json::json!({
                    "file": p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                    "valid": ok,
                }));
            }
        }
    }
    serde_json::json!({
        "bundledRevision": bundled.manifest.revision,
        "bundledPackages": bundled.manifest.packages.len(),
        "effectiveRevision": eff.manifest.revision,
        "effectivePackages": eff.manifest.packages.len(),
        "remoteActive": remote_active,
        "userModules": modules,
    })
}

/// 检查更新。
/// - 应用本体：拉 GitHub Releases（`nsmao-com/nice_env`）最新 tag，与当前版本比较。
/// - 套件清单：拉取远端 manifest 的 revision 与内置清单比对（`manifestUpdate`）。
///   远端地址可用设置项 `manifestUrl` 覆盖；未配置或网络失败时该字段为 null（未知）。
#[tauri::command]
fn check_updates(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<serde_json::Value, tauri::Error> {
    // 与「当前生效」的清单比：已应用过远端快照后，再拿内置 revision 比会永远提示有更新
    let current_rev = nsb_core::install::Installer::effective(&state.paths)
        .manifest
        .revision;
    let current_ver = env!("CARGO_PKG_VERSION");
    let url = state
        .store
        .get_setting("manifestUrl")
        .filter(|u| !u.trim().is_empty());

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;

    let manifest_update: Option<bool> = match url {
        Some(u) => match client
            .get(&u)
            .send()
            .and_then(|r| r.json::<serde_json::Value>())
        {
            Ok(v) => {
                let remote_rev = v.get("revision").and_then(|r| r.as_i64()).unwrap_or(0);
                let remote_updated = v
                    .get("updated")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string();
                state
                    .store
                    .set_setting("lastRemoteManifestRevision", &remote_rev.to_string())
                    .ok();
                state
                    .store
                    .set_setting("lastRemoteManifestUpdated", &remote_updated)
                    .ok();
                Some(remote_rev > current_rev as i64)
            }
            Err(_) => None,
        },
        None => None,
    };

    let (latest_version, release_url, app_update, release) =
        match fetch_latest_release_info(&client) {
            Ok(Some(info)) => {
                let newer = version_newer(&info.tag, current_ver);
                let rel = serde_json::json!({
                    "tag": info.tag,
                    "htmlUrl": info.html_url,
                    "body": info.body,
                    "publishedAt": info.published_at,
                    "assetName": info.asset_name,
                    "assetUrl": info.asset_url,
                    "assetSize": info.asset_size,
                });
                (Some(info.tag), Some(info.html_url), Some(newer), Some(rel))
            }
            Ok(None) => (
                Some(current_ver.to_string()),
                Some(APP_RELEASES_PAGE.to_string()),
                Some(false),
                None,
            ),
            Err(()) => (None, Some(APP_RELEASES_PAGE.to_string()), None, None),
        };

    Ok(serde_json::json!({
        "appVersion": current_ver,
        "latestVersion": latest_version,
        "releaseUrl": release_url,
        "manifestRevision": current_rev,
        "manifestUpdate": manifest_update,
        "appUpdate": app_update,
        "release": release,
    }))
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) -> bool {
    let state = app.state::<Arc<CoreState>>();
    stop_all_and_clear_pidfile(&state);
    app.exit(0);
    true
}

/* ================= 应用更新：在线下载 + 就地安装 ================= */

/// 更新包下载目录（数据目录下独立子目录，与套件下载缓存分开）
fn update_dir(state: &CoreState) -> std::path::PathBuf {
    state.paths.base.join("updates")
}

/// 在线下载更新包。事件 `update://progress` 持续回报进度（前端画进度条），
/// 完成后返回落盘路径与文件名，供「安装」步骤使用。
#[tauri::command]
async fn download_update(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    url: String,
    version: String,
    asset_name: Option<String>,
) -> Result<serde_json::Value, tauri::Error> {
    use std::io::Write;

    if !url.starts_with("https://") {
        return Err(box_err(nsb_core::AppError::new(
            "BAD_URL",
            "更新包地址必须是 https 链接",
        )));
    }
    let dir = update_dir(&state);
    std::fs::create_dir_all(&dir)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
    // 文件名：优先用 Release 里的 asset 名，退到版本号 + 扩展名
    let file_name = asset_name
        .filter(|n| !n.trim().is_empty() && !n.contains(['/', '\\']))
        .unwrap_or_else(|| {
            let ext = if url.to_ascii_lowercase().ends_with(".dmg") {
                "dmg"
            } else {
                "exe"
            };
            format!("NiceEnv-{version}-setup.{ext}")
        });
    let dest = dir.join(&file_name);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 30))
        .build()
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    let resp = client
        .get(&url)
        .header("User-Agent", "NiceEnv")
        .send()
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("下载失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(box_err(
            nsb_core::AppError::new(
                "UPDATE_DOWNLOAD_FAILED",
                format!("下载更新包失败：HTTP {}", resp.status()),
            )
            .with_hint("可在浏览器打开 Release 页手动下载安装"),
        ));
    }
    let total = resp.content_length().unwrap_or(0);
    let mut received: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    let mut file = std::fs::File::create(&dest)
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
    let mut resp = resp;
    let mut buf = vec![0u8; 64 * 1024];
    let begin = std::time::Instant::now();
    loop {
        use std::io::Read;
        let n = resp
            .read(&mut buf)
            .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
        received += n as u64;
        // 节流：每 200ms 或结束时回报一次，避免刷爆前端
        if last_emit.elapsed().as_millis() >= 200 {
            last_emit = std::time::Instant::now();
            let secs = begin.elapsed().as_secs_f64().max(0.001);
            let speed = (received as f64 / secs) as u64;
            let eta = if total > received && speed > 0 {
                (total - received) as f64 / speed as f64
            } else {
                0.0
            };
            let _ = app.emit(
                "update://progress",
                serde_json::json!({
                    "received": received, "total": total,
                    "speedBps": speed, "etaSec": eta, "state": "downloading",
                }),
            );
        }
    }
    let _ = file.flush();
    if total > 0 && received < total {
        let _ = std::fs::remove_file(&dest);
        return Err(box_err(nsb_core::AppError::new(
            "UPDATE_DOWNLOAD_INCOMPLETE",
            "更新包下载不完整，请重试",
        )));
    }
    let _ = app.emit(
        "update://progress",
        serde_json::json!({
            "received": received, "total": total.max(received),
            "speedBps": 0, "etaSec": 0, "state": "downloaded",
        }),
    );
    Ok(serde_json::json!({
        "path": dest.to_string_lossy(),
        "fileName": file_name,
        "sizeBytes": received,
    }))
}

/// 就地安装已下载的更新包：启动系统安装器后退出本应用。
///
/// - Windows：运行 NSIS 安装器（`/S` 静默由用户决定，这里用交互式安装器让用户看到进度），
///   安装器会自行结束并替换本程序；我们随即退出，避免文件占用导致安装失败。
/// - macOS：挂载 dmg 并把 .app 拷到 /Applications（需要用户授权），完成后退出。
#[tauri::command]
fn install_update(app: tauri::AppHandle, path: String) -> Result<bool, tauri::Error> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(box_err(nsb_core::AppError::new(
            "UPDATE_FILE_MISSING",
            "更新包不存在，请重新下载",
        )));
    }
    #[cfg(windows)]
    {
        std::process::Command::new(p)
            .spawn()
            .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("启动安装器失败：{e}")))?;
    }
    #[cfg(not(windows))]
    {
        // 打开 dmg，用户把 .app 拖进 Applications（比脚本替换更安全）
        std::process::Command::new("open")
            .arg(p)
            .spawn()
            .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("打开 dmg 失败：{e}")))?;
    }
    // 安装器需要独占替换可执行文件：先收尾再退出
    let state = app.state::<Arc<CoreState>>();
    stop_all_and_clear_pidfile(&state);
    app.exit(0);
    Ok(true)
}

/// 打开更新包所在目录（下载完想让用户自己看一眼时用）
#[tauri::command]
fn open_update_dir(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    let dir = update_dir(&state);
    std::fs::create_dir_all(&dir).ok();
    open_target(&dir.to_string_lossy(), true)
        .map(|_| true)
        .map_err(|e| box_err(nsb_core::AppError::new("OPEN_FAILED", e)))
}

/* ================= PHP 扩展 ================= */

#[tauri::command]
fn php_extensions(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    version: String,
) -> Result<nsb_core::model::PhpExtensionView, tauri::Error> {
    map_jh(state.php_extensions(&version))
}

/// 启用/禁用扩展；该版本 PHP 正在运行时顺带重启，让改动立即生效
#[tauri::command]
fn set_php_extension(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    version: String,
    name: String,
    enabled: bool,
) -> Result<nsb_core::model::PhpExtensionChange, tauri::Error> {
    map_jh(state.set_php_extension(&version, &name, enabled))
}

/// php.ini 快捷开关（display_errors / log_errors / opcache.enable）
#[tauri::command]
fn set_php_ini_toggle(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    version: String,
    key: String,
    value: bool,
) -> Result<bool, tauri::Error> {
    map_jh(
        state
            .set_php_ini_toggle(&version, &key, value)
            .map(|_| true),
    )
}

/* ================= Xdebug ================= */

#[tauri::command]
fn xdebug_status(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    version: String,
) -> Result<nsb_core::xdebug::XdebugStatus, tauri::Error> {
    map_jh(state.xdebug_status(&version))
}

/// 一键配置 Xdebug：dllPath 给了就从本地装，否则按 PHP 构建指纹在线拉
#[tauri::command]
async fn xdebug_setup(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    input: nsb_core::xdebug::XdebugSetupInput,
) -> Result<nsb_core::model::XdebugSetupResult, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        tauri::async_runtime::block_on(async move { map_jh(st.xdebug_setup(input).await) })
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn xdebug_toggle(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    version: String,
    enabled: bool,
    mode: String,
    port: u16,
) -> Result<Vec<String>, tauri::Error> {
    map_jh(state.xdebug_toggle(&version, enabled, &mode, port))
}

/* ================= 数据库备份 / 还原 ================= */

/// 复用既有连接信息（版本 / 端口 / root 密码）
fn db_conn_of(state: &CoreState) -> nsb_core::error::Result<nsb_core::dbbackup::ConnInfo> {
    let version = state
        .store
        .find_installed("mysql", None)
        .map(|p| p.version)
        .ok_or_else(|| nsb_core::AppError::not_installed("MySQL"))?;
    let ports = nsb_core::services::PortsProfile::from_settings(&state.store);
    let root_password = state
        .store
        .get_setting("mysqlRootPassword")
        .unwrap_or_else(|| "root".into());
    Ok(nsb_core::dbbackup::ConnInfo {
        version,
        port: ports.mysql,
        root_password,
    })
}

#[tauri::command]
fn db_backup_list(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Vec<nsb_core::model::DbBackupFile> {
    nsb_core::dbbackup::list_backups(&state.paths)
}

#[tauri::command]
fn db_backup_dir(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> String {
    nsb_core::dbbackup::backup_dir(&state.paths)
        .to_string_lossy()
        .to_string()
}

/// 导出选中的数据库。进度走 db://backup 事件回传。
#[tauri::command]
async fn db_backup_dump(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    databases: Vec<String>,
    out_name: Option<String>,
) -> Result<String, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db_conn_of(&st)?;
        let dir = nsb_core::dbbackup::backup_dir(&st.paths);
        let name = out_name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| nsb_core::dbbackup::default_dump_name(&databases));
        let path = dir.join(name);
        let emit = st.emit.clone();
        let p = path.clone();
        nsb_core::dbbackup::dump_databases(&st.paths, &conn, &databases, &path, &move |prog| {
            emit(nsb_core::Event::DbBackup(prog));
            let _ = &p;
        })?;
        Ok(path.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
    .map_err(box_err)
}

/// 从 .sql 还原；默认先自动备份一份当前状态
#[tauri::command]
async fn db_backup_restore(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    path: String,
    safety_backup: Option<bool>,
) -> Result<nsb_core::model::DbRestoreResult, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db_conn_of(&st)?;
        let emit = st.emit.clone();
        let safety = nsb_core::dbbackup::restore_from_file(
            &st.paths,
            &conn,
            std::path::Path::new(&path),
            safety_backup.unwrap_or(true),
            &move |prog| emit(nsb_core::Event::DbBackup(prog)),
        )?;
        Ok(nsb_core::model::DbRestoreResult {
            ok: true,
            safety_backup: safety.map(|p| p.to_string_lossy().to_string()),
        })
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
    .map_err(box_err)
}

#[tauri::command]
fn db_backup_delete(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    path: String,
) -> Result<bool, tauri::Error> {
    map_jh(nsb_core::dbbackup::delete_backup(&state.paths, &path).map(|_| true))
}

/* ================= 服务看门狗 ================= */

#[tauri::command]
fn watchdog_status(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> nsb_core::watchdog::WatchdogStatus {
    state.watchdog_status()
}

#[tauri::command]
fn watchdog_set_enabled(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    enabled: bool,
) -> Result<bool, tauri::Error> {
    map_jh(state.watchdog_set_enabled(enabled).map(|_| true))
}

/// 清空某服务的重试计数（重试次数用尽后按钮走这里）
#[tauri::command]
fn watchdog_reset(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, id: String) -> bool {
    state.watchdog_reset(&id);
    true
}

/* ================= 项目扫描 ================= */

/// 扫描一个目录，识别其中的项目并给出建站建议（只读，不改任何东西）
#[tauri::command]
fn scan_projects(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    root: String,
) -> Result<Vec<nsb_core::scanner::ScannedProject>, tauri::Error> {
    map_jh(nsb_core::scanner::scan_dir(
        &state.paths,
        &state.store,
        std::path::Path::new(&root),
    ))
}

/* ================= 配置文件编辑 ================= */

#[tauri::command]
fn config_list(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Vec<nsb_core::cfgeditor::ConfigFileInfo> {
    nsb_core::cfgeditor::list_configs(&state.paths, &state.store)
}

#[tauri::command]
fn config_read(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    kind: String,
) -> Result<String, tauri::Error> {
    let k = nsb_core::cfgeditor::ConfigKind::parse(&kind)
        .ok_or_else(|| box_err(nsb_core::AppError::new("BAD_KIND", "未知的配置类型")))?;
    map_jh(nsb_core::cfgeditor::read_config(
        &state.paths,
        &state.store,
        k,
    ))
}

/// 只校验不写入：前端可做实时校验
#[tauri::command]
fn config_validate(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    kind: String,
    content: String,
) -> Result<nsb_core::cfgeditor::ConfigValidation, tauri::Error> {
    let k = nsb_core::cfgeditor::ConfigKind::parse(&kind)
        .ok_or_else(|| box_err(nsb_core::AppError::new("BAD_KIND", "未知的配置类型")))?;
    map_jh(nsb_core::cfgeditor::validate(
        &state.paths,
        &state.store,
        k,
        &content,
    ))
}

/// 保存：默认校验不过就拒写；force=true 才允许跳过（带备份）
#[tauri::command]
fn config_save(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    kind: String,
    content: String,
    force: Option<bool>,
) -> Result<nsb_core::cfgeditor::ConfigValidation, tauri::Error> {
    let k = nsb_core::cfgeditor::ConfigKind::parse(&kind)
        .ok_or_else(|| box_err(nsb_core::AppError::new("BAD_KIND", "未知的配置类型")))?;
    map_jh(nsb_core::cfgeditor::save_config(
        &state.paths,
        &state.store,
        k,
        &content,
        force.unwrap_or(false),
    ))
}

#[tauri::command]
fn config_backups(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Vec<nsb_core::cfgeditor::ConfigBackup> {
    nsb_core::cfgeditor::list_config_backups(&state.paths)
}

#[tauri::command]
fn config_rollback(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
) -> Result<bool, tauri::Error> {
    map_jh(nsb_core::cfgeditor::rollback_config(&state.paths, &state.store, &name).map(|_| true))
}

/* ================= 证书体检 ================= */

#[tauri::command]
fn cert_health(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<nsb_core::certs::CertReport, tauri::Error> {
    map_jh(nsb_core::certs::report(&state.paths, &state.store))
}

#[tauri::command]
fn cert_import(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    cert_path: String,
    key_path: String,
) -> Result<nsb_core::certs::ImportedCert, tauri::Error> {
    map_jh(nsb_core::certs::import_cert_pair(
        &state.paths,
        std::path::Path::new(&cert_path),
        std::path::Path::new(&key_path),
    ))
}

#[tauri::command]
fn cert_import_dir(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    dir: String,
) -> Result<nsb_core::certs::DirImportResult, tauri::Error> {
    map_jh(nsb_core::certs::import_cert_dir(
        &state.paths,
        std::path::Path::new(&dir),
    ))
}

#[tauri::command]
fn cert_imported_list(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Vec<nsb_core::certs::ImportedCert> {
    nsb_core::certs::list_imported(&state.paths)
}

#[tauri::command]
fn cert_imported_delete(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    cert_path: String,
) -> Result<bool, tauri::Error> {
    map_jh(nsb_core::certs::delete_imported(&state.paths, &cert_path).map(|_| true))
}

/* ================= 站点 .env ================= */

#[tauri::command]
fn env_read(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    site_id: String,
) -> Result<nsb_core::envfile::EnvFileView, tauri::Error> {
    map_jh(nsb_core::envfile::read_env(
        &state.paths,
        &state.store,
        &site_id,
    ))
}

/// 保存 .env：只改传进来的键，其余连注释一起原样保留；写前备份到 .env.nsb-backup
#[tauri::command]
fn env_save(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    site_id: String,
    changes: Vec<(String, String)>,
) -> Result<bool, tauri::Error> {
    map_jh(
        nsb_core::envfile::save_env(&state.paths, &state.store, &site_id, &changes).map(|_| true),
    )
}

/// 一键把站点绑定的数据库信息写进 .env（DB_* 变量）
#[tauri::command]
fn env_apply_db(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    site_id: String,
) -> Result<Vec<String>, tauri::Error> {
    map_jh(nsb_core::envfile::apply_db_vars(
        &state.paths,
        &state.store,
        &site_id,
    ))
}

/* ================= 诊断包 ================= */

/// 生成诊断报告（Markdown）。内容已脱敏：密码/token 打码、用户主目录替换为 <home>。
#[tauri::command]
fn diagnostics_build(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<nsb_core::diagnostics::DiagnosticsBundle, tauri::Error> {
    map_jh(nsb_core::diagnostics::build(
        &state.paths,
        &state.store,
        &state.manager,
        env!("CARGO_PKG_VERSION"),
    ))
}

/// 把诊断包另存为 .md 文件，返回路径
#[tauri::command]
fn diagnostics_save(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<String, tauri::Error> {
    let bundle = map_jh(nsb_core::diagnostics::build(
        &state.paths,
        &state.store,
        &state.manager,
        env!("CARGO_PKG_VERSION"),
    ))?;
    map_jh(nsb_core::diagnostics::save_to_file(&state.paths, &bundle))
}

/* ================= 环境体检 ================= */

/// 汇总各项检查（端口/证书/hosts/站点/服务/扩展）成一个按严重程度排序的清单
#[tauri::command]
fn health_check(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<nsb_core::health::HealthReport, tauri::Error> {
    map_jh(nsb_core::health::check(
        &state.paths,
        &state.store,
        &state.manager,
    ))
}

/* ================= 批量服务操作 ================= */

/// 批量启动：按依赖分层排序（数据层 → 运行时 → Web 服务器），逐项回报
#[tauri::command]
fn bulk_start(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::bulk::BulkReport, tauri::Error> {
    map_jh(nsb_core::bulk::start_many(
        &state.store,
        &state.paths,
        &state.manager,
        &ids,
    ))
}

#[tauri::command]
fn bulk_stop(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::bulk::BulkReport, tauri::Error> {
    map_jh(nsb_core::bulk::stop_many(
        &state.store,
        &state.paths,
        &state.manager,
        &ids,
    ))
}

#[tauri::command]
fn bulk_restart(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::bulk::BulkReport, tauri::Error> {
    map_jh(nsb_core::bulk::restart_many(
        &state.store,
        &state.paths,
        &state.manager,
        &ids,
    ))
}

/// 选中集合的运行统计（前端据此决定按钮可点性）
#[tauri::command]
fn bulk_summary(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> nsb_core::bulk::BulkSelectionSummary {
    nsb_core::bulk::summarize(&state.manager, &ids)
}

/* ================= 工具链镜像源 ================= */

/// 列出 Composer / npm / pip 的当前源与可选项
#[tauri::command]
fn tool_mirrors(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Vec<nsb_core::toolmirror::ToolMirrorStatus> {
    use nsb_core::toolmirror::ToolManager;
    [ToolManager::Composer, ToolManager::Npm, ToolManager::Pip]
        .into_iter()
        .map(|m| nsb_core::toolmirror::status(m, &state.store))
        .collect()
}

/// 切换某个包管理器的镜像源（会改全局配置文件，前端需二次确认）
#[tauri::command]
fn tool_mirror_set(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    manager: String,
    url: String,
) -> Result<bool, tauri::Error> {
    let m = nsb_core::toolmirror::ToolManager::parse(&manager)
        .ok_or_else(|| box_err(nsb_core::AppError::new("BAD_MANAGER", "未知的包管理器")))?;
    // 记下用户选过的源，便于设置页展示「你上次选的是哪个」
    let _ = state.store.set_setting(m_setting_key(&manager), &url);
    map_jh(nsb_core::toolmirror::set_mirror(m, &url).map(|_| true))
}

/// 恢复官方源
#[tauri::command]
fn tool_mirror_reset(
    _state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    manager: String,
) -> Result<bool, tauri::Error> {
    let m = nsb_core::toolmirror::ToolManager::parse(&manager)
        .ok_or_else(|| box_err(nsb_core::AppError::new("BAD_MANAGER", "未知的包管理器")))?;
    map_jh(nsb_core::toolmirror::reset_mirror(m).map(|_| true))
}

fn m_setting_key(manager: &str) -> &'static str {
    match manager {
        "composer" => "composerRegistry",
        "npm" => "npmRegistry",
        _ => "pipIndexUrl",
    }
}

/* ================= 日志导出 ================= */

/// 把一段日志文本存成文件（用户在「日志」页选好过滤/搜索后导出当前视图）。
///
/// 内容由前端传进来，而不是后端重新读一遍 —— 因为用户看到的是
/// **过滤后**的视图，导出必须和他看到的一致，否则「我明明搜了 error 导出却是全量」。
#[tauri::command]
fn log_export(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    service_id: String,
    content: String,
    suggested_name: Option<String>,
) -> Result<String, tauri::Error> {
    map_jh(nsb_core::logs_export::write_log_file(
        &state.paths,
        &service_id,
        &content,
        suggested_name.as_deref(),
    ))
}

/* ================= 批量站点操作 ================= */

/// 批量启用站点：只为每个站点写 vhost，最后统一 reload 一次
#[tauri::command]
fn sites_start_many(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::sites::SiteBulkReport, tauri::Error> {
    map_jh(nsb_core::sites::start_many(
        &state.paths,
        &state.store,
        &state.manager,
        &ids,
    ))
}

#[tauri::command]
fn sites_stop_many(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::sites::SiteBulkReport, tauri::Error> {
    map_jh(nsb_core::sites::stop_many(
        &state.paths,
        &state.store,
        &state.manager,
        &ids,
    ))
}
