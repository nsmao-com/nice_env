//! NiceEnv 桌面壳：Tauri 命令接线 + 托盘 + 窗口行为。
//! 业务逻辑全部在 crates/core，这里只做参数转换与 UI 适配。

pub mod smoke;
pub mod tray;

use nsb_core::{AppError, CoreState, Event, EventSink};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use tauri::{Emitter, Manager, State};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_dialog::DialogExt;

// 0=空闲，1=正在准备，2=已提交退出，3=副本已就绪等待重启/取消。
static APP_TRANSITION: AtomicU8 = AtomicU8::new(0);
struct StartupFailure(AppError);
struct DesktopStartup {
    child: std::sync::Mutex<Option<nsb_core::restart::RestartChild>>,
    gate: Arc<nsb_core::restart::StartupGate>,
    frontend: nsb_core::restart::StartupGate,
}
struct PendingDataDir {
    result: nsb_core::paths::DataDirMigration,
    activity: nsb_core::paths::DataDirActivity,
    shutdown: nsb_core::AuxiliaryShutdown,
}
static PENDING_DATA_DIR: std::sync::Mutex<Option<PendingDataDir>> = std::sync::Mutex::new(None);
fn pending_data_dir() -> std::sync::MutexGuard<'static, Option<PendingDataDir>> {
    PENDING_DATA_DIR.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
struct AppTransition { committed: bool, rollback_state: u8 }
impl AppTransition {
    fn begin() -> nsb_core::error::Result<Self> {
        Self::begin_from(0, 0)
    }
    fn begin_from(from: u8, rollback_state: u8) -> nsb_core::error::Result<Self> {
        APP_TRANSITION.compare_exchange(from, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AppError::new("APP_BUSY", "应用正在退出、重启或迁移，请稍候"))?;
        Ok(Self { committed: false, rollback_state })
    }
    fn prepared(&mut self) {
        self.committed = true;
        APP_TRANSITION.store(3, Ordering::Release);
    }
    fn commit(&mut self) {
        self.committed = true;
        APP_TRANSITION.store(2, Ordering::Release);
    }
}
impl Drop for AppTransition {
    fn drop(&mut self) {
        if !self.committed { APP_TRANSITION.store(self.rollback_state, Ordering::Release); }
    }
}

pub fn run() {
    let (child, channel_error) = match nsb_core::restart::RestartChild::from_env() {
        Ok(child) => (child,None),
        Err(error) => (None,Some(error)),
    };
    if child.is_some() { APP_TRANSITION.store(1,Ordering::Release); }
    let startup = Arc::new(DesktopStartup {
        child: std::sync::Mutex::new(child),
        gate: Arc::new(nsb_core::restart::StartupGate::default()),
        frontend: nsb_core::restart::StartupGate::default(),
    });
    let setup_startup = startup.clone();
    tauri::Builder::default()
        .manage(startup.clone())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(move |app| {
            let outcome = (|| -> std::result::Result<(),Box<dyn std::error::Error>> {
            if let Some(error) = channel_error.clone() { return Err(error.into()); }
            if setup_startup.child.lock().unwrap_or_else(|e|e.into_inner()).is_some() {
                if let Some(window) = app.get_webview_window("main") { window.hide()?; }
            }
            let handle = app.handle().clone();
            let emit: EventSink = std::sync::Arc::new(move |e: Event| {
                let _ = handle.emit(e.channel(), e.payload());
            });
            let state = CoreState::init(None, emit)?;
            if let Some(child) = setup_startup.child.lock().unwrap_or_else(|e|e.into_inner()).as_ref() {
                child.verify_path(&state.paths.base)?;
            }
            // 仅桌面应用启动计划任务；CLI/MCP 的只读调用不应触发用户命令。
            nsb_core::cron::spawn_scheduler_when_ready(state.paths.clone(),Some(setup_startup.gate.clone()))?;
            nsb_core::backup_job::spawn_scheduler_when_ready(state.paths.clone(),Some(setup_startup.gate.clone()));
            nsb_core::backup_job::spawn_database_scheduler_when_ready(state.clone(), setup_startup.gate.clone())?;
            app.manage(state);

            /* ---------- 证书自动化调度：启动 30s 后先补一轮，之后每小时检查到期 ---------- */
            {
                let st = app.state::<Arc<CoreState>>().inner().clone();
                nsb_core::certauto::spawn_scheduler_when_ready(st,Some(setup_startup.gate.clone()));
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
                let gate = setup_startup.gate.clone();
                std::thread::spawn(move || {
                    if !gate.wait() { return; }
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
                let gate = setup_startup.gate.clone();
                std::thread::spawn(move || {
                    if !gate.wait() { return; }
                    loop {
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
                        api.prevent_close();
                        request_app_exit(handle_for_close.clone());
                    }
                }
            });

            Ok(())
            })();
            if let Err(error) = outcome {
                let error = match error.downcast::<AppError>() {
                    Ok(error) => *error,
                    Err(error) => AppError::new("APP_INITIALIZATION_FAILED",format!("初始化 NiceEnv 失败：{error}")),
                };
                setup_startup.gate.cancel();
                app.manage(StartupFailure(error.clone()));
                if let Some(window) = app.get_webview_window("main") { let _ = window.hide(); }
                if let Some(mut child) = setup_startup.child.lock().unwrap_or_else(|e|e.into_inner()).take() {
                    child.fail(error);
                    APP_TRANSITION.store(2,Ordering::Release);
                    app.handle().exit(1);
                } else {
                    let handle = app.handle().clone();
                    app.dialog().message(format!("{}\n{}",error.message,error.hint.unwrap_or_default()))
                        .title("无法打开 NiceEnv").kind(tauri_plugin_dialog::MessageDialogKind::Error).show(move |_| {
                            APP_TRANSITION.store(2,Ordering::Release);
                            handle.exit(1);
                        });
                }
            }
            Ok(())
        })
        .invoke_handler(off_main_thread(tauri::generate_handler![
            frontend_ready,
            // 套件
            list_packages,
            install_package,
            uninstall_package,
            cancel_download,
            set_active_version,
            pathenv_status,
            terminal_environment,
            project_runtime_versions,
            save_project_runtime_versions,
            pathenv_set_enabled,
            pathenv_set_selected,
            pathenv_set_version,
            pathenv_reapply,
            version_catalog,
            version_catalogs,
            // 服务
            list_service_status,
            service_web_url,
            repair_service_web_ui,
            sftpgo_config_directories,
            select_sftpgo_config,
            start_service,
            stop_service,
            service_stop_preview,
            force_stop_service,
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
            process_recovery_status,
            recover_processes,
            // 服务栈
            list_stacks,
            save_stack,
            duplicate_stack,
            delete_stack,
            start_stack,
            stop_stack,
            // 站点
            list_sites,
            site_access_url,
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
            delete_local_cert,
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
            diagnose_service,
            // 诊断包
            diagnostics_build,
            diagnostics_save,
            // 站点 .env
            env_read,
            env_save,
            env_apply_db,
            env_preview_db,
            env_restore_preview,
            env_restore,
            // 证书体检
            cert_health,
            cert_import,
            cert_imported_list,
            site_certificate_choices,
            cert_imported_delete,
            cert_imported_replace,
            cert_import_dir,
            // 证书自动化（ACME 签发 / 定时续签 / 多平台部署）
            certauto_list,
            certauto_save,
            certauto_delete,
            certauto_set_enabled,
            certauto_issue,
            certauto_retry_deploy,
            certdeploy_probe_ssh,
            // 证书监控 + PFX 导出
            certmonitor_list,
            certmonitor_add,
            certmonitor_delete,
            certmonitor_check,
            certmonitor_notification_get,
            certmonitor_notification_save,
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
            config_reset_preview,
            config_reset,
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
            get_system_stats,
            list_backups,
            preview_backup,
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
            db_grants,
            db_grants_save,
            db_create_user,
            db_user_password_info,
            db_user_password_save,
            db_reset_root_password,
            db_root_password,
            redis_stats,
            redis_connection,
            redis_save_connection,
            postgres_connection,
            postgres_password,
            postgres_set_password,
            postgres_databases,
            postgres_roles,
            postgres_role_access,
            postgres_role_access_save,
            postgres_create_database,
            postgres_drop_database,
            postgres_create_role,
            postgres_set_role_password,
            postgres_drop_role,
            postgres_backup_list,
            postgres_backup_dir,
            postgres_backup_dump,
            postgres_backup_restore,
            postgres_backup_replace,
            postgres_backup_plan,
            postgres_backup_plan_save,
            postgres_backup_plan_run,
            postgres_backup_delete,
            // 数据库备份 / 还原
            db_backup_plan,
            db_backup_plan_save,
            db_backup_plan_run,
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
            cron_stop,
            tunnel_start,
            tunnel_start_site,
            tunnel_list,
            tunnel_stop,
            tunnel_remove,
            ollama_models,
            ollama_delete,
            ollama_pull,
            ollama_pull_status,
            ollama_cancel_pull,
            adminer_start,
            adminer_status,
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
            migrate_data_dir,
            pending_data_dir_migration,
            cancel_data_dir_migration,
            restart_app,
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
        ]))
        .build(tauri::generate_context!())
        .expect("NiceEnv 启动失败")
        .run(move |app, event| {
            if matches!(event,tauri::RunEvent::Ready) && app.try_state::<StartupFailure>().is_none() {
                let child = startup.child.lock().unwrap_or_else(|e|e.into_inner()).take();
                if let Some(mut child) = child {
                    let handle = app.clone();
                    let gate = startup.gate.clone();
                    let ready = startup.clone();
                    let started = std::thread::Builder::new().name("restart-handoff".into()).spawn(move || {
                        if !ready.frontend.wait_timeout(std::time::Duration::from_secs(35)) {
                            child.fail(AppError::new("RESTART_FRONTEND_TIMEOUT", "新窗口页面未能完成加载，请检查安装文件后重试"));
                            gate.cancel();
                            APP_TRANSITION.store(2, Ordering::Release);
                            handle.exit(1);
                            return;
                        }
                        let result = child.ready(|| {
                            let window = handle.get_webview_window("main").ok_or_else(||AppError::new("APP_WINDOW_MISSING","新进程缺少主窗口"))?;
                            window.show().map_err(|e|AppError::internal("显示新窗口",e.to_string()))
                        });
                        if result.is_ok() {
                            APP_TRANSITION.store(0,Ordering::Release);
                            gate.start();
                        } else {
                            gate.cancel();
                            APP_TRANSITION.store(2,Ordering::Release);
                            handle.exit(1);
                        }
                    });
                    if started.is_err() { startup.gate.cancel(); APP_TRANSITION.store(2,Ordering::Release); app.exit(1); }
                } else { startup.gate.start(); }
            }
            if let tauri::RunEvent::ExitRequested { api, .. } = &event {
                if APP_TRANSITION.load(Ordering::Acquire) != 2 {
                    api.prevent_exit();
                    request_app_exit(app.clone());
                }
            }
            if matches!(event, tauri::RunEvent::Exit) {
                nsb_core::cron::shutdown();
                nsb_core::tunnel::shutdown();
                nsb_core::toolbox::ollama_shutdown();
            }
        });
}

/// 所有命令统一派发到阻塞线程池执行，不占用 UI 主线程。
///
/// Tauri 的同步命令（非 async fn）默认在主线程执行，而主线程同时也是 WebView 的
/// UI 线程：命令里只要有进程调用、端口探测、磁盘枚举、网络请求，整个窗口就会卡住，
/// 表现为切换菜单卡顿、点什么都没反应。这里一次性把派发挪出主线程：
/// - 同步命令在阻塞线程池里执行（该线程池允许 reqwest::blocking / block_on）；
/// - async 命令只是在这里把 future 交给 async runtime，行为与原来一致；
/// - ACL 校验在调用本 handler 之前已由 Tauri 完成，不受影响。
fn off_main_thread<F>(
    handler: F,
) -> impl Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static
where
    F: Fn(tauri::ipc::Invoke<tauri::Wry>) -> bool + Send + Sync + 'static,
{
    let handler = Arc::new(handler);
    move |invoke| {
        let handler = Arc::clone(&handler);
        tauri::async_runtime::spawn_blocking(move || {
            let resolver = invoke.resolver.clone();
            let cmd = invoke.message.command().to_string();
            if let Some(error) = invoke.message.webview_ref().try_state::<StartupFailure>() {
                resolver.reject(serde_json::to_string(&error.0).unwrap_or_default());
                return;
            }
            // 旧实例保留必要的显示与退出/重新打开入口，其余请求不能继续读取或写入旧数据。
            let recovery_action = matches!(cmd.as_str(), "get_app_version" | "get_data_dir" | "get_settings"
                | "frontend_ready" | "restart_app" | "quit_app" | "tray_panel_resize" | "tray_panel_hide" | "tray_open_main");
            if !recovery_action {
                let state = invoke.message.webview_ref().state::<Arc<CoreState>>();
                if let Err(error) = nsb_core::paths::ensure_data_dir_current(&state.paths.base) {
                    resolver.reject(serde_json::to_string(&error).unwrap_or_default());
                    return;
                }
            }
            let transition = APP_TRANSITION.load(Ordering::Acquire);
            let transition_read = matches!(cmd.as_str(),
                "list_service_status" | "tray_panel_state" | "tray_panel_resize" | "tray_panel_hide" | "tray_open_main"
                | "get_app_version" | "get_data_dir" | "get_settings" | "pending_data_dir_migration" | "frontend_ready");
            let pending_action = transition == 3 && matches!(cmd.as_str(), "restart_app" | "cancel_data_dir_migration" | "quit_app");
            if transition != 0 && !transition_read && !pending_action {
                resolver.reject(serde_json::to_string(&AppError::new("APP_BUSY", "应用正在退出、重启或迁移，请稍候")).unwrap_or_default());
                return;
            }
            // 同步命令在这里持锁；异步命令另外在自身 future 中持锁直到实际完成。
            let _activity = if transition_read || pending_action || matches!(cmd.as_str(), "migrate_data_dir" | "restart_app" | "quit_app" | "install_update" | "cancel_data_dir_migration") {
                None
            } else {
                let state = invoke.message.webview_ref().state::<Arc<CoreState>>();
                match nsb_core::paths::DataDirActivity::shared(&state.paths.base) {
                    Ok(guard) => Some(guard),
                    Err(error) => { resolver.reject(serde_json::to_string(&error).unwrap_or_default()); return; }
                }
            };
            // 已经对 Tauri 返回了 true，找不到命令时要自己 reject，否则前端 Promise 永远挂起
            if !handler(invoke) {
                resolver.reject(format!("Command {cmd} not found"));
            }
        });
        true
    }
}

/// 系统关闭和菜单退出走同一受控流程；不能在 UI 线程等待数据库停机。
fn request_app_exit(app: tauri::AppHandle) {
    if app.try_state::<Arc<CoreState>>().is_none() {
        APP_TRANSITION.store(2, Ordering::Release);
        app.exit(1);
        return;
    }
    if APP_TRANSITION.load(Ordering::Acquire) == 3 { let _ = cancel_data_dir_migration(); }
    let Ok(transition) = AppTransition::begin() else { return; };
    tauri::async_runtime::spawn_blocking(move || {
        if let Err(error) = exit_after_stop(app.clone(), transition) {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
            let raw = error.to_string();
            let message = serde_json::from_str::<AppError>(&raw).map(|error|
                format!("{}\n{}", error.message, error.hint.unwrap_or_default())).unwrap_or(raw);
            app.dialog().message(message).title("未能退出 NiceEnv")
                .kind(tauri_plugin_dialog::MessageDialogKind::Error).show(|_| {});
        }
    });
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
async fn uninstall_package(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.uninstall_package(&id).map(|_| true))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    // 即使后续 PATH 清理失败，卸载及默认版本回落仍已生效。
    crate::tray::refresh(&app);
    result
}

/// 设置套件默认版本（仅已装版本）；多实例服务的运行状态独立保留。
#[tauri::command]
async fn set_active_version(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    version: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    // 走门面而非直接 ops：切版本后要把 PATH 里的目录一并指过去
    let st = state.inner().clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.set_active_version(&id, &version).map(|_| true))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    // PATH 失败时默认版本仍可能已保存，托盘也应反映实际服务栈选择。
    crate::tray::refresh(&app);
    result
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
async fn terminal_environment(
    state: State<'_, std::sync::Arc<CoreState>>,
    site_id: Option<String>,
) -> Result<nsb_core::model::TerminalEnvironment, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(match site_id {
        Some(id) => state.site_terminal_environment(&id),
        None => state.terminal_environment(),
    }))
        .await
        .map_err(|e| box_err(nsb_core::AppError::internal("读取终端环境", e.to_string())))?
}

#[tauri::command]
async fn project_runtime_versions(
    state: State<'_, std::sync::Arc<CoreState>>,
    site_id: String,
) -> Result<nsb_core::model::ProjectRuntimeVersions, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(state.project_runtime_versions(&site_id)))
        .await.map_err(|e| box_err(nsb_core::AppError::internal("读取项目版本", e.to_string())))?
}

#[tauri::command]
async fn save_project_runtime_versions(
    state: State<'_, std::sync::Arc<CoreState>>,
    site_id: String,
    versions: std::collections::BTreeMap<String, String>,
    expected_revision: String,
) -> Result<nsb_core::model::ProjectRuntimeVersions, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(state.save_project_runtime_versions(&site_id, &versions, &expected_revision)))
        .await.map_err(|e| box_err(nsb_core::AppError::internal("保存项目版本", e.to_string())))?
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

#[tauri::command]
fn pathenv_set_version(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    version: String,
    selected: bool,
) -> Result<nsb_core::model::PathEnvStatus, tauri::Error> {
    map_jh(state.pathenv_set_version(&id, &version, selected))
}

/// 某包的完整版本目录（远程枚举 + 缓存）；force=true 忽略缓存
#[tauri::command]
async fn version_catalog(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    force: Option<bool>,
) -> Result<nsb_core::model::VersionCatalog, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
    Ok(state.downloader.cancel(&task_id))
}

/* ================= 服务 ================= */

#[tauri::command]
async fn sftpgo_config_directories(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> Result<nsb_core::model::SftpgoConfigDirectories, tauri::Error> {
    let activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || { let _activity = activity; map_jh(st.sftpgo_config_directories()) })
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn select_sftpgo_config(app: tauri::AppHandle, state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    directory: String, version: String, expected_current: Option<String>) -> Result<bool, tauri::Error> {
    let activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _activity = activity;
        map_jh(st.select_sftpgo_config(&directory, &version, expected_current.as_deref()).map(|_| true))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    crate::tray::refresh(&app);
    result
}

#[tauri::command]
async fn service_web_url(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, id: String) -> Result<String, tauri::Error> {
    let activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || { let _activity = activity; map_jh(st.service_web_url(&id)) })
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn repair_service_web_ui(app: tauri::AppHandle, state: State<'_, std::sync::Arc<nsb_core::CoreState>>, id: String, version: String) -> Result<String, tauri::Error> {
    let activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let _activity = activity;
        tauri::async_runtime::block_on(async move { map_jh(st.repair_service_web_ui(&id, &version).await) })
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    crate::tray::refresh(&app);
    result
}

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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    let service_id = id.clone();
    let (result, freed) = tauri::async_runtime::spawn_blocking(move || {
        let mut freed = Vec::new();
        let result = st.start_service_with_port_policy(&id, |port| freed.push(port));
        (result, freed)
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    if !freed.is_empty() {
        let _ = app.emit("ports://auto-freed", serde_json::json!({ "serviceId": service_id, "freed": freed }));
    }
    let r = map_jh(result.map(|_| true));
    crate::tray::refresh(&app);
    r
}

#[tauri::command]
async fn stop_service(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    let r = tauri::async_runtime::spawn_blocking(move || map_jh(st.restart_service(&id).map(|_| true)))
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?;
    crate::tray::refresh(&app);
    r
}

#[tauri::command]
async fn service_stop_preview(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, id: String) -> Result<nsb_core::model::ServiceStopPreview, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.service_stop_preview(&id))).await
        .map_err(|error| tauri::Error::Anyhow(anyhow::anyhow!("{error}")))?
}

#[tauri::command]
async fn force_stop_service(app: tauri::AppHandle, state: State<'_, std::sync::Arc<nsb_core::CoreState>>, id: String, revision: String) -> Result<bool, tauri::Error> {
    let st = state.inner().clone();
    let result = tauri::async_runtime::spawn_blocking(move || map_jh(st.force_stop_service(&id, &revision).map(|()| true))).await
        .map_err(|error| tauri::Error::Anyhow(anyhow::anyhow!("{error}")))?;
    crate::tray::refresh(&app);
    result
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
        &state.manager,
    ))
}

#[tauri::command]
async fn site_access_url(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<String, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::sites::access_url(&st.paths, &st.store, &st.manager, &id)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn create_site(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    input: nsb_core::model::CreateSiteInput,
) -> Result<nsb_core::model::Site, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::sites::create_with_progress(
            &input,
            &st.paths,
            &st.store,
            &st.manager,
            &|stage, percent| {
                let _ = app.emit(
                    "site://create-progress",
                    serde_json::json!({
                        "rootDir": input.root_dir.trim(), "stage": stage, "percent": percent,
                    }),
                );
            },
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::sites::stop_site(&id, &st.paths, &st.store, &st.manager).map(|_| true))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/* ================= hosts / 证书 ================= */

#[tauri::command]
async fn read_hosts() -> Result<Vec<nsb_core::model::HostsEntry>, tauri::Error> {
    tauri::async_runtime::spawn_blocking(|| map_jh(nsb_core::hosts::read_all()))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn apply_hosts(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    entries: Vec<nsb_core::model::HostsEntry>,
    expected_entries: Option<Vec<nsb_core::model::HostsEntry>>,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::hosts::apply_checked(&st.store, &st.paths, Some(entries), expected_entries.as_deref()).map(|_| true))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
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
async fn certauto_issue(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<nsb_core::model::CertAutomation, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::certauto::run_once(&st, &id)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn list_certs(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::CertRecord>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::tls::list_certs(&st.paths, &st.store)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn certauto_retry_deploy(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<nsb_core::model::CertAutomation, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.certauto_retry_deploy(&id)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn certdeploy_probe_ssh(host: String, port: u16) -> Result<nsb_core::certdeploy::SshHostKey, tauri::Error> {
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::certdeploy::probe_ssh(&host, port)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn cert_export_pem(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    cert_id: String,
    out_path: String,
) -> Result<String, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::tls::export_pem_bundle(
            &st.paths,
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::tls::export_jks(
            &st.paths,
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::tls::export_der(
            &st.paths,
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.certmonitor_check(&id)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn certmonitor_notification_get(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> Result<nsb_core::certmonitor::MonitorNotificationSettings, tauri::Error> {
    map_jh(state.certmonitor_notification_get())
}

#[tauri::command]
fn certmonitor_notification_save(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, settings: nsb_core::certmonitor::MonitorNotificationSettings) -> Result<nsb_core::certmonitor::MonitorNotificationSettings, tauri::Error> {
    map_jh(state.certmonitor_notification_save(settings))
}

// ---- PFX 导出：本机证书 + 私钥打包 PKCS#12（Windows IIS / 设备导入用） ----
#[tauri::command]
async fn cert_export_pfx(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    cert_id: String,
    password: String,
    out_path: String,
) -> Result<String, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::tls::export_pfx(
            &st.paths,
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
async fn issue_cert(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    domain: String,
    sans: Vec<String>,
) -> Result<nsb_core::model::CertRecord, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.issue_certificate(&domain, &sans)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn delete_local_cert(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, id: String) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.delete_local_certificate(&id).map(|_| true)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn trust_ca(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::tls::trust_ca(&st.paths).map(|_| true)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 按当前站点重建 hosts 托管块（保留用户手动条目）
#[tauri::command]
async fn rebuild_hosts(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::hosts::rebuild(&st.store, &st.paths).map(|_| true)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 补齐缺失/过期的站点证书，返回重新签发的域名
#[tauri::command]
async fn reissue_site_certs(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<String>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.repair_site_certificates()))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/* ================= 日志 / 诊断 / 统计 ================= */

#[tauri::command]
async fn tail_logs(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    lines: Option<usize>,
) -> Result<Vec<nsb_core::model::LogLine>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.tail_logs_checked(&id, lines.unwrap_or(200))))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn diagnose_port(port: u16) -> Result<nsb_core::model::PortDiagnosis, tauri::Error> {
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::ports::diagnose_port(port)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 全量端口体检：本应用所有待绑定端口 vs 实际占用者
#[tauri::command]
async fn scan_ports(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::PortScanEntry>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::ports::scan_app_ports(&st.store, &st.manager)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 端口区间扫描（工具箱）：from == to 即单端口查询，返回占用者与归属
#[tauri::command]
async fn scan_port_range(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    from: u16,
    to: u16,
) -> Result<nsb_core::model::PortRangeScan, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.scan_port_range(from, to)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 结束占用某端口的进程：本应用服务走优雅停止，外部进程直接 kill。
/// 用户明确点了按钮才会调到这里。
#[tauri::command]
async fn close_port(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    port: u16,
    expected: Vec<nsb_core::model::ListenerInfo>,
) -> Result<nsb_core::ports::ClosePortOutcome, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || map_jh(st.close_port_checked(port, &expected)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))??;
    crate::tray::refresh(&app);
    Ok(outcome)
}

/// 备份目录列表（配置变更前自动生成的 .bak）
#[tauri::command]
async fn list_backups(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::paths::BackupFile>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::paths::list_backup_files(&st.paths.base).map_err(Into::into))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn preview_backup(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
) -> Result<nsb_core::paths::BackupPreview, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.preview_backup(&name)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 从备份恢复单个配置文件
#[tauri::command]
async fn restore_backup(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
    revision: String,
) -> Result<String, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(
            st.restore_backup(&name, &revision)
                .map(|path| path.to_string_lossy().to_string()),
        )
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn get_system_stats() -> nsb_core::model::SystemStats {
    nsb_core::stats::get_system_stats()
}

/// 配置体检放入阻塞任务线程；不把文件可读当成原生语法通过。
#[tauri::command]
async fn validate_configs(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    only: Option<Vec<String>>,
) -> Result<Vec<nsb_core::ops::ConfigCheck>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(state.validate_configs(only.as_deref())))
        .await.map_err(|e| box_err(nsb_core::AppError::internal("检查配置", e.to_string())))?
}

#[tauri::command]
async fn postgres_connection(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String) -> Result<nsb_core::dbadmin::PostgresConnectionInfo, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.postgres_connection(&version))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_password(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String) -> Result<String, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.postgres_password(&version))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_set_password(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, password: String, use_existing: bool, enable_password_auth: bool) -> Result<(), tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.set_postgres_password(&version, &password, use_existing, enable_password_auth))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_databases(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String) -> Result<Vec<nsb_core::dbadmin::PostgresDatabaseInfo>, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| client.list_databases()))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_roles(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String) -> Result<Vec<nsb_core::dbadmin::PostgresRoleInfo>, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| client.list_roles()))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_role_access(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, name: String, oid: u32) -> Result<nsb_core::dbadmin::PostgresRoleAccess, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| client.role_access(&name, oid)))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_role_access_save(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, input: nsb_core::dbadmin::PostgresRoleAccessInput) -> Result<nsb_core::dbadmin::PostgresRoleAccess, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::dbadmin::update_postgres_role_access(&st, &version, &input))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_create_database(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, name: String, owner: String) -> Result<(), tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| client.create_database(&name, &owner)))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_drop_database(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, name: String, oid: u32) -> Result<(), tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| client.drop_database(&name, oid)))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_create_role(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, name: String, password: String) -> Result<(), tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| client.create_role(&name, &password)))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_set_role_password(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, name: String, oid: u32, password: String) -> Result<(), tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| client.set_role_password(&name, oid, &password)))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_drop_role(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, name: String, oid: u32) -> Result<(), tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| client.drop_role(&name, oid)))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// Redis 运行统计（内存 / 键数 / 连接数 / 运行天数）
#[tauri::command]
async fn redis_stats(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<nsb_core::stats::RedisStats, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.redis_stats())).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn redis_connection(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String) -> Result<nsb_core::stats::RedisConnectionInfo, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.redis_connection(&version))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn redis_save_connection(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, credentials: nsb_core::stats::RedisCredentials) -> Result<nsb_core::stats::RedisStats, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.save_redis_connection(&version, credentials))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 已连接的网络接口（供 DNS 接管选择）
#[tauri::command]
async fn dns_interfaces() -> Result<Vec<String>, tauri::Error> {
    tauri::async_runtime::spawn_blocking(|| platform::connected_interfaces()
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 指定接口当前 DNS 状态与接管前备份。
#[tauri::command]
async fn dns_status_of(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, name: String) -> Result<nsb_core::dns::InterfaceStatus, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::dns::interface_status(&st.store, &name))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 提权把接口 DNS 指向 127.0.0.1（本地域名解析接管；触发 UAC）
#[tauri::command]
async fn dns_takeover(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, name: String) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::dns::takeover(&st.store, &st.manager, &name).map(|_| true))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 恢复接管前 DNS；无备份的旧接口可由用户明确选择自动获取。
#[tauri::command]
async fn dns_restore(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, name: String, automatic: Option<bool>) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::dns::restore(&st.store, &name, automatic.unwrap_or(false)).map(|_| true))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 列出源 MySQL 实例（FlyEnv/phpStudy/ServBay/XAMPP 等）上的用户数据库
#[tauri::command]
async fn migrate_list_source(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    host: String,
    port: u16,
    user: String,
    password: String,
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
) -> Result<Vec<nsb_core::dbmigrate::SourceDb>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.migrate_list_source(host, port, user, password, version.as_deref(), engine.unwrap_or_default()))
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
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
    databases: Vec<String>,
) -> Result<nsb_core::dbmigrate::ImportReport, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.migrate_import(host, port, user, password, databases, version.as_deref(), engine.unwrap_or_default()))
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

/// 导出服务或站点的完整日志到用户指定路径（前端走保存对话框）。
#[tauri::command]
async fn export_log(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    dest: String,
) -> Result<u64, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let src = map_jh(st.log_source_path(&id))?;
        map_jh(nsb_core::logs_export::copy_log_file(&src, std::path::Path::new(&dest)))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
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
async fn open_in_browser(url: String) -> Result<bool, tauri::Error> {
    tauri::async_runtime::spawn_blocking(move || map_jh(open_browser_with(&url, |target| {
        tauri_plugin_opener::open_url(target, None::<&str>).map_err(|e| e.to_string())
    }))).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn open_in_folder(path: String) -> Result<bool, tauri::Error> {
    tauri::async_runtime::spawn_blocking(move || map_jh(open_folder(&path)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 网页只能交给系统浏览器；URL 作为完整数据传递，不能进入 cmd/start 命令串。
fn open_browser_with(target: &str, open: impl FnOnce(&str) -> Result<(), String>) -> nsb_core::error::Result<bool> {
    let invalid = || AppError::new("BROWSER_URL_INVALID", "无法打开网页：请使用完整的 HTTP 或 HTTPS 地址")
        .with_hint("地址不能包含登录凭据、反斜杠或控制字符；本地文件请使用“打开所在文件夹”。");
    if target.chars().any(char::is_control) || target.contains('\\') { return Err(invalid()); }
    let target = target.trim();
    if !target.get(..7).is_some_and(|v| v.eq_ignore_ascii_case("http://"))
        && !target.get(..8).is_some_and(|v| v.eq_ignore_ascii_case("https://")) { return Err(invalid()); }
    if target.split_once("://").is_none_or(|(_, rest)| rest.is_empty() || rest.starts_with(['/', '?', '#'])) { return Err(invalid()); }
    let url = reqwest::Url::parse(target).map_err(|_| invalid())?;
    if url.host_str().is_none_or(str::is_empty) || !url.username().is_empty() || url.password().is_some()
        || url.port() == Some(0) { return Err(invalid()); }
    open(url.as_str()).map_err(|_| AppError::new("OPEN_FAILED", "系统未能打开网页，请检查默认浏览器设置后重试"))?;
    Ok(true)
}

/// 文件路径只用于定位，不通过文件关联执行配置、脚本或快捷方式。
fn folder_target(target: &str) -> nsb_core::error::Result<(std::path::PathBuf, bool)> {
    if target.trim().is_empty() || target.chars().any(char::is_control) {
        return Err(AppError::new("FOLDER_PATH_INVALID", "请选择要打开的文件或文件夹"));
    }
    let path = std::path::absolute(target).map_err(|_| AppError::new("FOLDER_PATH_INVALID", "文件路径无效"))?;
    let metadata = std::fs::metadata(&path).map_err(|_| AppError::new("FOLDER_PATH_UNAVAILABLE", "文件或文件夹不存在，或当前没有读取权限"))?;
    if !metadata.is_dir() && !metadata.is_file() {
        return Err(AppError::new("FOLDER_PATH_INVALID", "此位置不是普通文件或文件夹"));
    }
    Ok((path, metadata.is_dir()))
}

fn open_folder(target: &str) -> nsb_core::error::Result<bool> {
    let (path, directory) = folder_target(target)?;
    let result = if directory {
        tauri_plugin_opener::open_path(&path, None::<&str>)
    } else {
        tauri_plugin_opener::reveal_item_in_dir(&path)
    };
    result.map_err(|_| AppError::new("OPEN_FAILED", "系统未能打开所在文件夹，请检查文件管理器后重试"))?;
    Ok(true)
}

#[cfg(test)]
mod external_open_tests {
    use super::*;

    #[test]
    fn browser_urls_preserve_parameters_and_never_launch_invalid_targets() {
        for (input, expected) in [
            (" https://example.test/?a=1&b=two#result ", "https://example.test/?a=1&b=two#result"),
            ("HTTP://127.0.0.1:28080/web/admin", "http://127.0.0.1:28080/web/admin"),
            ("https://[::1]:8443/中文 文档?q=a%26b&value=%PATH%", "https://[::1]:8443/%E4%B8%AD%E6%96%87%20%E6%96%87%E6%A1%A3?q=a%26b&value=%PATH%"),
            ("http://example.test/?next=a&echo=hello|world", "http://example.test/?next=a&echo=hello|world"),
        ] {
            let mut launched = Vec::new();
            assert!(open_browser_with(input, |url| { launched.push(url.to_string()); Ok(()) }).unwrap());
            assert_eq!(launched, [expected]);
        }
        for input in ["", "example.test", "//example.test", "http:example.test", "http:///example.test", "https://",
            "javascript:alert(1)", "data:text/html,hello", "file:///C:/Windows/system32/cmd.exe", "ms-settings:display",
            "ftp://example.test", "https://user:secret@example.test", "https://user@example.test", "http://example.test:0",
            "http://example.test:65536", "https://example.test\nextra", "https://example.test\u{85}extra", "https://example.test\\other"] {
            let error = open_browser_with(input, |_| panic!("invalid target reached OS launcher")).unwrap_err();
            assert_eq!(error.code, "BROWSER_URL_INVALID", "{input}");
        }
    }

    #[test]
    fn browser_launcher_failure_is_reported_without_leaking_the_target() {
        let error = open_browser_with("https://example.test/?token=private", |_| Err("launcher failed: token=private".into())).unwrap_err();
        assert_eq!(error.code, "OPEN_FAILED");
        assert!(!format!("{error:?}").contains("private"));
    }

    #[test]
    fn folder_targets_distinguish_files_from_directories_and_reject_missing_paths() {
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
        let root = std::env::temp_dir().join(format!("niceenv-open-{}", rand::random::<u64>()));
        std::fs::create_dir(&root).unwrap();
        let _cleanup = Cleanup(root.clone());
        let directory = root.join("中文 space & [folder]");
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("config & [one].cmd");
        std::fs::write(&file, b"must only be revealed").unwrap();
        assert_eq!(folder_target(directory.to_str().unwrap()).unwrap(), (directory, true));
        assert_eq!(folder_target(file.to_str().unwrap()).unwrap(), (file, false));
        assert_eq!(folder_target(root.join("missing").to_str().unwrap()).unwrap_err().code, "FOLDER_PATH_UNAVAILABLE");
        for input in ["", " ", "a\0b", "a\nb"] {
            assert_eq!(folder_target(input).unwrap_err().code, "FOLDER_PATH_INVALID");
        }
    }
}

#[tauri::command]
async fn open_terminal(state: State<'_, std::sync::Arc<CoreState>>, site_id: Option<String>, expected_revision: String) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || state.with_terminal_environment(site_id.as_deref(), &expected_revision, |environment| {
        let directory = nsb_core::pathenv::terminal_directory(&environment.cwd)?;
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
                nsb_core::AppError::new("TERMINAL_UNAVAILABLE", "无法确定系统 PowerShell 位置")
            })?;
            let powershell = std::path::PathBuf::from(system_root)
                .join("System32/WindowsPowerShell/v1.0/powershell.exe");
            // 用户明确点击打开终端：创建可见的独立控制台。cwd 只传给进程 API，不经 shell 解释。
            let mut child = std::process::Command::new(powershell)
                .args(nsb_core::pathenv::powershell_terminal_args(environment)?)
                .current_dir(&directory)
                .creation_flags(0x0000_0010) // CREATE_NEW_CONSOLE
                .spawn()
                .map_err(|e| nsb_core::AppError::io("打开 PowerShell", e))?;
            std::thread::sleep(std::time::Duration::from_millis(150));
            if let Some(status) = child
                .try_wait()
                .map_err(|e| nsb_core::AppError::io("检查 PowerShell", e))?
            {
                return Err(nsb_core::AppError::new(
                    "TERMINAL_EXITED",
                    format!("PowerShell 提前退出：{status}"),
                ));
            }
            Ok(true)
        }
        #[cfg(target_os = "macos")]
        {
            let _ = directory;
            let command = nsb_core::pathenv::posix_terminal_command(environment)?;
            let output = platform::command("/usr/bin/osascript")
                .args(["-e", "on run argv\ntell application \"Terminal\"\ndo script (item 1 of argv)\nactivate\nend tell\nend run", "--", &command])
                .output()
                .map_err(|e| nsb_core::AppError::io("打开终端", e))?;
            if !output.status.success() {
                return Err(nsb_core::AppError::new(
                    "TERMINAL_OPEN_FAILED",
                    "无法打开已配置环境的终端",
                ).with_hint("请检查系统设置中 NiceEnv 对 Terminal 的自动化权限。").with_detail(String::from_utf8_lossy(&output.stderr).into_owned()));
            }
            Ok(true)
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            Err(nsb_core::AppError::new(
                "TERMINAL_UNSUPPORTED",
                "此平台请手动打开 Bash / Zsh 并粘贴脚本",
            ))
        }
    }))
    .await
    .map_err(|e| box_err(nsb_core::AppError::internal("打开终端", e.to_string())))?
    .map_err(box_err)
}

/* ================= 数据库 ================= */

async fn run_database<T, F>(
    state: &std::sync::Arc<CoreState>,
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
    operation: F,
) -> Result<T, tauri::Error>
where
    T: Send + 'static,
    F: FnOnce(&CoreState, &str, &nsb_core::dbadmin::MySqlClient) -> nsb_core::error::Result<T>
        + Send
        + 'static,
{
    let st = state.clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.with_database(engine.unwrap_or_default(), version.as_deref(), |version, client| {
            operation(&st, version, client)
        }))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn db_list(
    state: State<'_, std::sync::Arc<CoreState>>,
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
) -> Result<Vec<nsb_core::model::DatabaseInfo>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    run_database(&state, version, engine, |_, _, client| client.list_databases()).await
}

#[tauri::command]
async fn db_create(
    state: State<'_, std::sync::Arc<CoreState>>,
    name: String,
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    run_database(&state, version, engine, move |_, _, client| {
        client.create_database(&name).map(|_| true)
    })
    .await
}

#[tauri::command]
async fn db_drop(
    state: State<'_, std::sync::Arc<CoreState>>,
    name: String,
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    run_database(&state, version, engine, move |_, _, client| {
        client.drop_database(&name).map(|_| true)
    })
    .await
}

#[tauri::command]
async fn db_users(
    state: State<'_, std::sync::Arc<CoreState>>,
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
) -> Result<Vec<nsb_core::model::DbUserInfo>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    run_database(&state, version, engine, |_, _, client| client.list_users()).await
}

#[tauri::command]
async fn db_create_user(
    state: State<'_, std::sync::Arc<CoreState>>,
    username: String,
    password: String,
    database: String,
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    run_database(&state, version, engine, move |_, _, client| {
        client
            .create_user_grant(&username, &password, &database)
            .map(|_| true)
    })
    .await
}

#[tauri::command]
async fn db_user_password_info(state: State<'_, std::sync::Arc<CoreState>>, engine: nsb_core::dbadmin::DatabaseEngine, version: String, username: String, host: String) -> Result<nsb_core::dbadmin::DatabaseUserPasswordInfo, tauri::Error> {
    run_database(&state, Some(version), Some(engine), move |_, _, client| client.user_password_info(&username, &host)).await
}

#[tauri::command]
async fn db_user_password_save(state: State<'_, std::sync::Arc<CoreState>>, engine: nsb_core::dbadmin::DatabaseEngine, version: String, input: nsb_core::dbadmin::DatabaseUserPasswordInput) -> Result<nsb_core::dbadmin::DatabaseUserPasswordInfo, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::dbadmin::update_database_user_password(&st, engine, &version, &input))).await.map_err(|error| tauri::Error::Anyhow(anyhow::anyhow!("{error}")))?
}

#[tauri::command]
async fn db_grants(state: State<'_, std::sync::Arc<CoreState>>, engine: nsb_core::dbadmin::DatabaseEngine, version: String, username: String, host: String) -> Result<nsb_core::dbadmin::DatabaseGrants, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    run_database(&state, Some(version), Some(engine), move |_, _, client| client.grants(&username, &host)).await
}

#[tauri::command]
async fn db_grants_save(state: State<'_, std::sync::Arc<CoreState>>, engine: nsb_core::dbadmin::DatabaseEngine, version: String, input: nsb_core::dbadmin::DatabaseGrantInput) -> Result<nsb_core::dbadmin::DatabaseGrants, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::dbadmin::update_database_grants(&st, engine, &version, &input))).await.map_err(|error| tauri::Error::Anyhow(anyhow::anyhow!("{error}")))?
}

#[tauri::command]
async fn db_reset_root_password(
    state: State<'_, std::sync::Arc<CoreState>>,
    new_password: String,
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
    use_existing: Option<bool>,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(
            st.set_database_password(
                engine.unwrap_or_default(),
                version.as_deref(),
                &new_password,
                use_existing.unwrap_or(false),
            )
            .map(|_| true),
        )
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn db_root_password(
    state: State<'_, std::sync::Arc<CoreState>>,
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
) -> Result<String, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    run_database(&state, version, engine, |_, _, client| {
        Ok(client.root_password.clone())
    })
    .await
}

/* ================= 代理（Clash/mihomo） ================= */

#[tauri::command]
async fn proxy_status(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<nsb_core::model::serde_proxy::ProxyStatusInfo, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let running = st
            .manager
            .snapshot("mihomo")
            .map(|s| s.state == nsb_core::model::ServiceState::Running)
            .unwrap_or(false);
        let sys = map_jh(platform::get_system_proxy().map_err(AppError::from))?;
        let runtime = nsb_core::proxy::ProxyRuntime::new();
        let mode = if running {
            map_jh(runtime.mode())?
        } else {
            map_jh(nsb_core::proxy::configured_mode(&st.store))?
        };
        let version = if running {
            Some(map_jh(runtime.version())?)
        } else {
            None
        };
        Ok(nsb_core::model::serde_proxy::ProxyStatusInfo {
            running,
            mixed_port: nsb_core::configgen::MIHOMO_MIXED_PORT,
            controller_port: nsb_core::configgen::MIHOMO_CONTROLLER_PORT,
            mode,
            system_proxy_enabled: sys.enabled,
            version,
        })
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn proxy_start(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.start_service("mihomo").map(|_| true)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn proxy_stop(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    let sys = map_jh(platform::get_system_proxy().map_err(AppError::from))?;
    tauri::async_runtime::spawn_blocking(move || {
        if sys.enabled {
            // 先恢复系统代理，再停内核；否则失败时会把系统留在一个不可用的代理地址上。
            map_jh(nsb_core::proxy::system_proxy_off())?;
        }
        map_jh(st.stop_service("mihomo").map(|_| true))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn proxy_set_system(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    enabled: bool,
) -> Result<bool, tauri::Error> {
    if enabled {
        let running = state
            .manager
            .snapshot("mihomo")
            .map(|s| s.state == nsb_core::model::ServiceState::Running)
            .unwrap_or(false);
        if !running {
            return map_jh(Err(nsb_core::error::AppError::new(
                "PROXY_NOT_RUNNING",
                "mihomo 尚未运行，不能开启系统代理",
            )
            .with_hint("先启动 mihomo，再开启系统代理")));
        }
    }
    let r = if enabled {
        nsb_core::proxy::system_proxy_on()
    } else {
        nsb_core::proxy::system_proxy_off()
    };
    map_jh(r.map(|_| true))
}

#[tauri::command]
async fn proxy_set_mode(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    mode: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::proxy::set_mode(&st.paths, &st.store, &st.manager, &mode).map(|_| true))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let id = map_jh(tauri::async_runtime::block_on(async {
            nsb_core::proxy::import_profile(&name, &url, &st.paths, &st.store, &st.manager).await
        }))?;
        Ok(nsb_core::model::serde_proxy::ProxyProfile {
            id,
            name: name.trim().to_string(),
            url: url.trim().to_string(),
            active: false,
            added_at: nsb_core::services::now_ms(),
        })
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn proxy_activate_profile(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(
            nsb_core::proxy::activate_profile(&st.paths, &st.store, &st.manager, &id).map(|_| true),
        )
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn proxy_delete_profile(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(
            nsb_core::proxy::delete_profile(&st.paths, &st.store, &st.manager, &id).map(|_| true),
        )
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn proxy_nodes() -> Result<Vec<nsb_core::model::serde_proxy::ProxyGroupView>, tauri::Error> {
    tauri::async_runtime::spawn_blocking(move || {
        let rt = nsb_core::proxy::ProxyRuntime::new();
        let v = map_jh(rt.proxies())?;
        map_jh(nsb_core::model::serde_proxy::parse_groups(&v))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
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
async fn cron_run_now(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<nsb_core::cron::CronJob, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.cron_run_now(&id)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn cron_stop(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    map_jh(state.cron_stop(&id).map(|_| true))
}

#[tauri::command]
async fn tunnel_start(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    port: u16,
) -> Result<nsb_core::model::TunnelInfo, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.tunnel_start(port)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn tunnel_start_site(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<nsb_core::model::TunnelInfo, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.tunnel_start_site(&id)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn tunnel_remove(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.tunnel_remove(&id).map(|_| true)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn tunnel_list(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::TunnelInfo>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || st.tunnel_list())
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))
}

#[tauri::command]
async fn tunnel_stop(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.tunnel_stop(&id).map(|_| true)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn ollama_models(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::toolbox::OllamaModelRow>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.ollama_models()))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn ollama_delete(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.ollama_delete(&name).map(|_| true)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn ollama_pull(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
) -> Result<nsb_core::toolbox::OllamaPullStatus, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.ollama_pull(&name)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?
}

#[tauri::command]
fn ollama_pull_status() -> Option<nsb_core::toolbox::OllamaPullStatus> {
    nsb_core::toolbox::ollama_pull_status()
}

#[tauri::command]
fn ollama_cancel_pull(id: String) -> Result<bool, tauri::Error> {
    map_jh(nsb_core::toolbox::ollama_cancel_pull(&id).map(|_| true))
}

/// Adminer 的启动检查与进程回收运行在工作线程。
#[tauri::command]
async fn adminer_start(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<nsb_core::toolbox::AdminerStatus, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.adminer_start())).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn adminer_status(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Option<nsb_core::toolbox::AdminerStatus>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.adminer_status())).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn adminer_stop(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.adminer_stop().map(|_| true))).await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn proxy_connections() -> Result<serde_json::Value, tauri::Error> {
    tauri::async_runtime::spawn_blocking(move || {
        let rt = nsb_core::proxy::ProxyRuntime::new();
        map_jh(rt.connections())
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 重新拉取并校验订阅；当前订阅热重载失败时恢复旧配置。
#[tauri::command]
async fn proxy_update_profile(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(
            tauri::async_runtime::block_on(nsb_core::proxy::update_profile(
                &st.paths,
                &st.store,
                &st.manager,
                &id,
            ))
            .map(|_| true),
        )
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn proxy_select_node(group: String, node: String) -> Result<bool, tauri::Error> {
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(
            nsb_core::proxy::ProxyRuntime::new()
                .select(&group, &node)
                .map(|_| true),
        )
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
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
    let mut val = match value {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    if key == "defaultTld" { val = nsb_core::dns::normalize_tld(&val).map_err(box_err)?; }
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
fn frontend_ready(window: tauri::WebviewWindow, startup: State<'_, Arc<DesktopStartup>>) -> bool {
    if window.label() != "main" { return false; }
    startup.frontend.start();
    startup.gate.wait_timeout(std::time::Duration::from_secs(45))
}

#[tauri::command]
fn get_app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[tauri::command]
fn get_data_dir(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> String {
    state.paths.base.to_string_lossy().to_string()
}

/// 停止受管服务并复制完整数据目录，当前进程仍使用原目录。
/// 前端收到复制结果后，再通过 restart_app 将目标路径传给新进程。
#[tauri::command]
async fn migrate_data_dir(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    path: String,
) -> Result<nsb_core::paths::DataDirMigration, tauri::Error> {
    let mut transition = map_jh(AppTransition::begin())?;
    if std::env::var_os("NSB_HOME").is_some_and(|value| !value.is_empty()) {
        return Err(box_err(AppError::new("DATA_DIR_OVERRIDE", "当前目录由 NSB_HOME 环境变量指定，无法持久切换")
            .with_hint("请移除启动配置中的 NSB_HOME 后重新打开应用，再迁移数据目录")));
    }
    let st = state.inner().clone();
    let target = std::path::PathBuf::from(path);
    let (result, activity, shutdown) = tauri::async_runtime::spawn_blocking(move || {
        let shutdown = map_jh(nsb_core::AuxiliaryShutdown::prepare())?;
        let (result, activity) = map_jh(st.prepare_data_dir_migration(&target))?;
        Ok::<_, tauri::Error>((result, activity, shutdown))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))??;
    *pending_data_dir() = Some(PendingDataDir { result: result.clone(), activity, shutdown });
    transition.prepared();
    // 页面在复制中重新加载也能接回已准备的副本，不留下无法操作的等待状态。
    let _ = app.emit("data-dir://prepared", &result);
    Ok(result)
}

#[tauri::command]
fn pending_data_dir_migration() -> Option<nsb_core::paths::DataDirMigration> {
    pending_data_dir().as_ref().map(|pending| pending.result.clone())
}

#[tauri::command]
fn cancel_data_dir_migration() -> Result<bool, tauri::Error> {
    if APP_TRANSITION.load(Ordering::Acquire) == 0 { return Ok(true); }
    let _transition = map_jh(AppTransition::begin_from(3, 0))?;
    pending_data_dir().take();
    Ok(true)
}

#[tauri::command]
fn restart_app(app: tauri::AppHandle, state: State<'_, Arc<CoreState>>, data_dir: Option<String>) -> Result<bool, tauri::Error> {
    let prepared = data_dir.is_some();
    let relocated = map_jh(nsb_core::paths::redirected_data_dir(&state.paths.base))?.is_some();
    let mut transition = if let Some(path) = &data_dir {
        if !pending_data_dir().as_ref().is_some_and(|pending| &pending.result.path == path) {
            return Err(box_err(AppError::new("DATA_DIR_NOT_PREPARED", "迁移副本尚未准备好，请重新选择目录开始迁移")));
        }
        map_jh(AppTransition::begin_from(3, 3))?
    } else { map_jh(AppTransition::begin())? };
    // 清理先于服务生命周期锁，避免与模型删除/服务操作的锁顺序相反。
    let mut shutdown = if prepared { None } else { Some(map_jh(nsb_core::AuxiliaryShutdown::prepare())?) };
    let executable = std::env::current_exe()
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e.to_string())))?;
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if let Some(path) = &data_dir {
        if !std::path::Path::new(path).is_absolute() || !std::path::Path::new(path).join("nsb.sqlite").is_file() {
            return Err(box_err(AppError::new("DATA_DIR_INVALID", "新的数据目录缺少 NiceEnv 数据库，未重启")));
        }
    }
    let launch = || {
        let mut command = platform::command(executable);
        command.args(args);
        if let Some(path) = data_dir {
            // 子进程与今后从快捷方式启动都读取同一持久选择；启动失败恢复原选择。
            command.env_remove("NSB_HOME");
            let mut pending = pending_data_dir();
            let pending = pending.as_mut().ok_or_else(|| AppError::new("DATA_DIR_NOT_PREPARED", "迁移副本尚未准备好"))?;
            pending.activity.with_selected_data_dir(std::path::Path::new(&path), || {
                let rollback = nsb_core::pathenv::MigrationActivationRollback::capture(std::path::Path::new(&path))?;
                match nsb_core::restart::launch_and_wait(&mut command,std::path::Path::new(&path),std::time::Duration::from_secs(45)) {
                    Ok(()) => Ok(()),
                    Err(error) if error.code == "RESTART_CHILD_CLEANUP_FAILED" => Err(error),
                    Err(error) => {
                        rollback.restore().map_err(|restore|AppError::new(&restore.code,format!("{}；{}",error.message,restore.message))
                            .with_hint(restore.hint.unwrap_or_default()).with_detail(restore.detail.unwrap_or_default()))?;
                        Err(error)
                    }
                }
            })?;
            pending.shutdown.commit();
        } else {
            if relocated { command.env_remove("NSB_HOME"); }
            command.spawn().map_err(|e| AppError::io("启动新的 NiceEnv 进程", e))?;
        }
        if let Some(shutdown) = shutdown.as_mut() { shutdown.commit(); }
        transition.commit();
        app.exit(0);
        Ok(true)
    };
    // 已准备的迁移持有源目录独占锁，服务已停止且不能再启动；普通重启仍执行停机。
    let result = if prepared || relocated { launch() } else { state.with_stopped_services(launch) };
    if result.is_err() && nsb_core::paths::redirected_data_dir(&state.paths.base).ok().flatten().is_some() {
        // 清理/选择回滚未确认：旧实例只保留退出和重新打开入口，不能提示继续重试旧副本。
        pending_data_dir().take();
        transition.rollback_state = 0;
    }
    map_jh(result)
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
fn quit_app(app: tauri::AppHandle) -> Result<bool, tauri::Error> {
    if APP_TRANSITION.load(Ordering::Acquire) == 3 { cancel_data_dir_migration()?; }
    let transition = map_jh(AppTransition::begin())?;
    exit_after_stop(app, transition)
}

fn exit_after_stop(app: tauri::AppHandle, mut transition: AppTransition) -> Result<bool, tauri::Error> {
    let state = app.state::<Arc<CoreState>>();
    let relocated = map_jh(nsb_core::paths::redirected_data_dir(&state.paths.base))?.is_some();
    let mut shutdown = map_jh(nsb_core::AuxiliaryShutdown::prepare())?;
    let mut exit = || {
        shutdown.commit();
        transition.commit();
        app.exit(0);
        Ok(true)
    };
    // 目录已由其它进程接管时，只退出旧实例，不覆写旧 PID 记录或处理新实例的服务。
    map_jh(if relocated { exit() } else { state.with_stopped_services(exit) })
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
/// - macOS：打开 dmg 后退出，用户仍需手动将 .app 拖入 Applications。
#[tauri::command]
fn install_update(app: tauri::AppHandle, path: String) -> Result<bool, tauri::Error> {
    let mut transition = map_jh(AppTransition::begin())?;
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(box_err(nsb_core::AppError::new(
            "UPDATE_FILE_MISSING",
            "更新包不存在，请重新下载",
        )));
    }
    let state = app.state::<Arc<CoreState>>();
    let mut shutdown = map_jh(nsb_core::AuxiliaryShutdown::prepare())?;
    map_jh(state.with_stopped_services(|| {
        #[cfg(windows)]
        {
            std::process::Command::new(p)
                .spawn()
                .map_err(|e| AppError::io("启动安装器", e))?;
        }
        #[cfg(not(windows))]
        {
            // 打开 dmg，用户把 .app 拖进 Applications（比脚本替换更安全）
            std::process::Command::new("open")
                .arg(p)
                .spawn()
                .map_err(|e| AppError::io("打开 dmg", e))?;
        }
        // 安装器需要独占替换可执行文件：先收尾再退出
        shutdown.commit();
        transition.commit();
        app.exit(0);
        Ok(true)
    }))
}

/// 打开更新包所在目录（下载完想让用户自己看一眼时用）
#[tauri::command]
async fn open_update_dir(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<bool, tauri::Error> {
    let dir = update_dir(&state);
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(std::fs::create_dir_all(&dir).map_err(|e| AppError::io("创建更新目录", e)))?;
        map_jh(open_folder(&dir.to_string_lossy()))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
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
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
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
fn db_conn_of(
    engine: nsb_core::dbadmin::DatabaseEngine,
    version: &str,
    client: &nsb_core::dbadmin::MySqlClient,
) -> nsb_core::dbbackup::ConnInfo {
    nsb_core::dbbackup::ConnInfo {
        engine,
        version: version.into(),
        port: client.port,
        root_password: client.root_password.clone(),
        bin_dir: client.exe.parent().map(std::path::Path::to_path_buf),
    }
}

#[tauri::command]
fn db_backup_list(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::model::DbBackupFile>, tauri::Error> {
    map_jh(nsb_core::dbbackup::list_backups(&state.paths))
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
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
) -> Result<String, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    run_database(&state, version, engine, move |st, version, client| {
        let conn = db_conn_of(engine.unwrap_or_default(), version, client);
        let name = out_name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| nsb_core::dbbackup::default_dump_name(&databases));
        let path = nsb_core::dbbackup::dump_path_for(&st.paths, conn.engine, version, &name)?;
        nsb_core::dbbackup::dump_databases(&st.paths, &conn, &databases, &path, &|prog| {
            (st.emit)(nsb_core::Event::DbBackup(prog))
        })?;
        Ok(path.to_string_lossy().to_string())
    })
    .await
}

/// 从 .sql 还原；默认先自动备份一份当前状态
#[tauri::command]
async fn db_backup_restore(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    path: String,
    safety_backup: Option<bool>,
    version: Option<String>,
    engine: Option<nsb_core::dbadmin::DatabaseEngine>,
    database: Option<String>,
) -> Result<nsb_core::model::DbRestoreResult, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    run_database(&state, version, engine, move |st, version, client| {
        let conn = db_conn_of(engine.unwrap_or_default(), version, client);
        let safety = nsb_core::dbbackup::restore_from_file_into(
            &st.paths,
            &conn,
            std::path::Path::new(&path),
            database.as_deref(),
            safety_backup.unwrap_or(true),
            &|prog| (st.emit)(nsb_core::Event::DbBackup(prog)),
        )?;
        Ok(nsb_core::model::DbRestoreResult {
            ok: true,
            safety_backup: safety.map(|p| p.to_string_lossy().to_string()),
        })
    })
    .await
}

#[tauri::command]
fn db_backup_delete(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    path: String,
) -> Result<bool, tauri::Error> {
    map_jh(nsb_core::dbbackup::delete_backup(&state.paths, &path).map(|_| true))
}

/* ================= PostgreSQL 备份 / 还原 ================= */

#[tauri::command]
fn postgres_backup_list(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> Result<Vec<nsb_core::model::DbBackupFile>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    map_jh(nsb_core::dbbackup::postgres_list_backups(&state.paths))
}

#[tauri::command]
fn postgres_backup_dir(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> Result<String, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    map_jh((|| { let dir = nsb_core::dbbackup::postgres_backup_dir(&state.paths)?; std::fs::create_dir_all(&dir)?; Ok(dir.to_string_lossy().into()) })())
}

#[tauri::command]
async fn postgres_backup_dump(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, name: String, oid: u32, operation_id: String) -> Result<String, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| {
        nsb_core::dbbackup::postgres_dump(&st.paths, client, &version, &name, oid, &|progress| {
            (st.emit)(nsb_core::Event::PostgresBackup(nsb_core::model::PostgresBackupProgress { operation_id: operation_id.clone(), progress }));
        }).map(|path| path.to_string_lossy().into())
    }))).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_backup_restore(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, path: String, name: String, owner: String, trusted: bool, operation_id: String) -> Result<(), tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| {
        nsb_core::dbbackup::postgres_restore(&st.paths, client, std::path::Path::new(&path), &name, &owner, trusted, &|progress| {
            (st.emit)(nsb_core::Event::PostgresBackup(nsb_core::model::PostgresBackupProgress { operation_id: operation_id.clone(), progress }));
        })
    }))).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn postgres_backup_delete(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, name: String) -> Result<(), tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    map_jh(nsb_core::dbbackup::postgres_delete_backup(&state.paths, &name))
}

#[tauri::command]
async fn postgres_backup_replace(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, input: nsb_core::dbbackup::PostgresReplaceInput, operation_id: String) -> Result<nsb_core::dbbackup::PostgresReplaceResult, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.with_postgres(&version, |client| {
        nsb_core::dbbackup::postgres_replace_from_file(&st.paths, client, &version, &input, &|progress| {
            (st.emit)(nsb_core::Event::PostgresBackup(nsb_core::model::PostgresBackupProgress { operation_id: operation_id.clone(), progress }));
        })
    }))).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn postgres_backup_plan(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String) -> Result<nsb_core::backup_job::PostgresPlan, tauri::Error> {
    map_jh(nsb_core::backup_job::postgres_plan(&state, &version))
}

#[tauri::command]
async fn postgres_backup_plan_save(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String, config: nsb_core::backup_job::PostgresPlanConfig) -> Result<nsb_core::backup_job::PostgresPlan, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::backup_job::save_postgres_plan(&st, &version, config))).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn postgres_backup_plan_run(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, version: String) -> Result<nsb_core::backup_job::PostgresPlan, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::backup_job::run_postgres_plan(&st, &version, true))).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn db_backup_plan(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, engine: nsb_core::dbadmin::DatabaseEngine, version: String) -> Result<nsb_core::backup_job::BackupPlan, tauri::Error> {
    map_jh(nsb_core::backup_job::database_plan(&state, engine, &version))
}

#[tauri::command]
async fn db_backup_plan_save(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, engine: nsb_core::dbadmin::DatabaseEngine, version: String, config: nsb_core::backup_job::BackupPlanConfig) -> Result<nsb_core::backup_job::BackupPlan, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::backup_job::save_database_plan(&st, engine, &version, config))).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn db_backup_plan_run(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, engine: nsb_core::dbadmin::DatabaseEngine, version: String) -> Result<nsb_core::backup_job::BackupPlan, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::backup_job::run_database_plan(&st, engine, &version, true))).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
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
fn watchdog_reset(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, id: String) -> Result<bool, tauri::Error> {
    map_jh(state.watchdog_reset(&id).map(|_| true))
}

#[tauri::command]
fn process_recovery_status(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> nsb_core::ops::OrphanReport {
    state.process_recovery_status()
}

#[tauri::command]
async fn recover_processes(state: State<'_, std::sync::Arc<nsb_core::CoreState>>) -> Result<nsb_core::ops::OrphanReport, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.recover_processes()))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
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
    map_jh(nsb_core::cfgeditor::read_config_selected(
        &state.paths,
        &state.store,
        &kind,
    ))
}

/// 只校验不写入：前端可做实时校验
#[tauri::command]
async fn config_validate(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    kind: String,
    content: String,
) -> Result<nsb_core::cfgeditor::ConfigValidation, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.validate_config(&kind, &content)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/// 保存：默认校验不过就拒写；force=true 才允许跳过（带备份）
#[tauri::command]
async fn config_save(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    kind: String,
    content: String,
    force: Option<bool>,
    expected_content: Option<String>,
) -> Result<nsb_core::cfgeditor::ConfigValidation, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.save_config(
            &kind,
            &content,
            force.unwrap_or(false),
            expected_content.as_deref(),
        ))
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
fn config_backups(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    kind: Option<String>,
) -> Result<Vec<nsb_core::cfgeditor::ConfigBackup>, tauri::Error> {
    map_jh(nsb_core::cfgeditor::list_config_backups_selected(
        &state.paths,
        &state.store,
        kind.as_deref(),
    ))
}

#[tauri::command]
async fn config_rollback(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    name: String,
    kind: Option<String>,
    expected_content: Option<String>,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(
            st.rollback_config(&name, kind.as_deref(), expected_content.as_deref())
                .map(|_| true),
        )
    })
    .await
    .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn config_reset_preview(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    kind: String,
) -> Result<nsb_core::cfgeditor::ConfigResetPreview, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.preview_config_reset(&kind)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn config_reset(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    kind: String,
    revision: String,
) -> Result<nsb_core::cfgeditor::ConfigResetPreview, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(st.reset_config(&kind, &revision)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/* ================= 证书体检 ================= */

#[tauri::command]
async fn cert_health(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<nsb_core::certs::CertReport, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::certs::report(&st.paths, &st.store)))
        .await
        .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn cert_import(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    cert_path: String,
    key_path: String,
) -> Result<nsb_core::certs::ImportedCert, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::certs::import_cert_pair(&st.paths, std::path::Path::new(&cert_path), std::path::Path::new(&key_path))))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn cert_import_dir(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    dir: String,
) -> Result<nsb_core::certs::DirImportResult, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::certs::import_cert_dir(&st.paths, std::path::Path::new(&dir))))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn cert_imported_replace(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
    cert_path: String,
    key_path: String,
) -> Result<nsb_core::certs::ImportedCert, tauri::Error> {
    let activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _activity = activity;
        map_jh(st.replace_imported_certificate(&id, std::path::Path::new(&cert_path), std::path::Path::new(&key_path)))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn cert_imported_list(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::certs::ImportedCert>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::certs::list_imported(&st.paths, &st.store)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn site_certificate_choices(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<Vec<nsb_core::certs::SiteCertificateChoice>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::certs::site_certificate_choices(&st.paths, &st.store)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn cert_imported_delete(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    cert_path: String,
) -> Result<bool, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::certs::delete_imported(&st.paths, &st.store, &cert_path).map(|_| true)))
        .await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/* ================= 站点 .env ================= */

#[tauri::command]
fn env_read(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    site_id: String,
    file_name: Option<String>,
) -> Result<nsb_core::envfile::EnvFileView, tauri::Error> {
    map_jh(nsb_core::envfile::read_env_named(
        &state.paths,
        &state.store,
        &site_id,
        file_name.as_deref().unwrap_or(".env"),
    ))
}

/// 保存 .env：只改传进来的键，其余连注释一起原样保留；写前备份到 .env.nsb-backup
#[tauri::command]
fn env_save(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    site_id: String,
    changes: Vec<(String, String)>,
    expected_revision: String,
    file_name: Option<String>,
) -> Result<nsb_core::envfile::EnvFileView, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    map_jh(
        nsb_core::envfile::save_env_named(&state.paths, &state.store, &site_id, file_name.as_deref().unwrap_or(".env"), &changes, &expected_revision),
    )
}

/// 仅准备数据库变量供编辑器确认，保存草稿前不修改项目文件。
#[tauri::command]
fn env_preview_db(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    site_id: String,
    expected_revision: String,
    file_name: Option<String>,
) -> Result<Vec<(String, String)>, tauri::Error> {
    map_jh(nsb_core::envfile::preview_db_vars_named(&state.paths, &state.store, &site_id, file_name.as_deref().unwrap_or(".env"), &expected_revision))
}

#[tauri::command]
fn env_restore_preview(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, site_id: String, file_name: String, expected_revision: String) -> Result<nsb_core::envfile::EnvRestorePreview, tauri::Error> {
    map_jh(nsb_core::envfile::preview_env_restore(&state.paths, &state.store, &site_id, &file_name, &expected_revision))
}

#[tauri::command]
fn env_restore(state: State<'_, std::sync::Arc<nsb_core::CoreState>>, site_id: String, file_name: String, expected_revision: String, expected_backup_revision: String) -> Result<nsb_core::envfile::EnvFileView, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    map_jh(nsb_core::envfile::restore_env(&state.paths, &state.store, &site_id, &file_name, &expected_revision, &expected_backup_revision))
}

/// 一键把站点绑定的数据库信息写进 .env（DB_* 变量）
#[tauri::command]
fn env_apply_db(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    site_id: String,
) -> Result<Vec<String>, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    map_jh(nsb_core::envfile::apply_db_vars(
        &state.paths,
        &state.store,
        &site_id,
    ))
}

/* ================= 诊断包 ================= */

/// 生成诊断报告（Markdown）。内容已脱敏：密码/token 打码、用户主目录替换为 <home>。
#[tauri::command]
async fn diagnostics_build(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<nsb_core::diagnostics::DiagnosticsBundle, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::diagnostics::build(
        &state.paths, &state.store, &state.manager, env!("CARGO_PKG_VERSION"),
    ))).await.map_err(|e| box_err(nsb_core::AppError::internal("生成诊断报告", e.to_string())))?
}

/// 把诊断包另存为 .md 文件，返回路径
#[tauri::command]
async fn diagnostics_save(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    bundle: nsb_core::diagnostics::DiagnosticsBundle,
) -> Result<String, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || map_jh(nsb_core::diagnostics::save_to_file(&state.paths, &bundle)))
        .await.map_err(|e| box_err(nsb_core::AppError::internal("保存诊断报告", e.to_string())))?
}

/* ================= 环境体检 ================= */

/// 汇总各项检查（端口/证书/hosts/站点/服务/扩展）成一个按严重程度排序的清单
#[tauri::command]
async fn health_check(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
) -> Result<nsb_core::health::HealthReport, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::health::check(&state.paths, &state.store, &state.manager))
    }).await.map_err(|error| tauri::Error::Anyhow(anyhow::anyhow!("{error}")))?
}

/* ================= 批量服务操作 ================= */

#[tauri::command]
async fn diagnose_service(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    id: String,
) -> Result<nsb_core::diagnostics::ServiceDiagnosticReport, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(nsb_core::diagnostics::diagnose_service(&state.paths, &state.store, &state.manager, &id))
    }).await.map_err(|error| box_err(nsb_core::AppError::internal("诊断服务", error.to_string())))?
}

/// 批量启动：按依赖分层排序（数据层 → 运行时 → Web 服务器），逐项回报
#[tauri::command]
async fn bulk_start(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::bulk::BulkReport, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.bulk_start(&ids))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))??;
    crate::tray::refresh(&app);
    Ok(report)
}

#[tauri::command]
async fn bulk_stop(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::bulk::BulkReport, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.bulk_stop(&ids))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))??;
    crate::tray::refresh(&app);
    Ok(report)
}

#[tauri::command]
async fn bulk_restart(
    app: tauri::AppHandle,
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::bulk::BulkReport, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    let report = tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.bulk_restart(&ids))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))??;
    crate::tray::refresh(&app);
    Ok(report)
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
async fn log_export(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    service_id: String,
    content: String,
    suggested_name: Option<String>,
) -> Result<String, tauri::Error> {
    let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&state.paths.base))?;
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        map_jh(st.log_source_path(&service_id))?;
        map_jh(nsb_core::logs_export::write_log_file(
            &st.paths, &service_id, &content, suggested_name.as_deref(),
        ))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

/* ================= 批量站点操作 ================= */

/// 批量启用站点：只为每个站点写 vhost，最后统一 reload 一次
#[tauri::command]
async fn sites_start_many(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::sites::SiteBulkReport, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&st.paths.base))?;
        map_jh(nsb_core::sites::start_many(&st.paths, &st.store, &st.manager, &ids))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}

#[tauri::command]
async fn sites_stop_many(
    state: State<'_, std::sync::Arc<nsb_core::CoreState>>,
    ids: Vec<String>,
) -> Result<nsb_core::sites::SiteBulkReport, tauri::Error> {
    let st = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _activity = map_jh(nsb_core::paths::DataDirActivity::shared(&st.paths.base))?;
        map_jh(nsb_core::sites::stop_many(&st.paths, &st.store, &st.manager, &ids))
    }).await.map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!("{e}")))?
}
