//! core：NiceEnv 全部业务逻辑（无 Tauri 依赖，可独立测试/无头运行）。

pub mod acme;
pub mod applications;
pub mod bulk;
pub mod certauto;
pub mod certdeploy;
pub mod certmonitor;
pub mod certs;
pub mod cfgeditor;
pub mod configgen;
pub mod cron;
pub mod dbadmin;
pub mod dbworkspace;
pub mod dbbackup;
pub mod dbmigrate;
pub mod diagnostics;
pub mod dns;
pub mod dnsprov;
pub mod download;
pub mod envfile;
pub mod error;
pub use error::AppError;
pub mod generic;
pub mod health;
pub mod hosts;
pub mod install;
pub mod logs_export;
pub mod model;
pub mod ops;
pub mod paths;
pub mod restart;
pub mod redis_settings;
pub mod toolbox;
pub mod tunnel;
use paths::write_with_backup;
pub mod backup_job;
pub mod mcp;
pub mod pathenv;
pub mod phpext;
pub mod php_platform;
pub mod ports;
pub mod proxy;
pub mod scanner;
pub mod serde_proxy;
pub mod services;
pub mod sites;
pub mod sitebackup;
pub mod stacks;
pub mod stats;
pub mod store;
pub mod tls;
pub mod toolmirror;
pub mod transfer;
pub mod versions;
pub mod watchdog;
pub mod xdebug;

use download::Downloader;
use error::Result;
use model::DownloadProgress;
use services::ServiceManager;
use std::sync::Arc;

/// 事件：core → 前端。desktop 侧转 tauri emit；测试侧可打印。
#[derive(Clone, Debug)]
pub enum Event {
    SiteBackupStatus { site_id: String, state: String, message: String },
    DownloadProgress(DownloadProgress),
    HostsDenied,
    /// 数据库备份 / 还原进度
    DbBackup(model::DbBackupProgress),
    PostgresBackup(model::PostgresBackupProgress),
    /// 证书自动化状态变化（签发中 / 成功 / 失败），前端可在任意页面监听
    CertAuto {
        id: String,
        state: String,
        message: String,
    },
    /// 网站证书监控告警：状态跃迁为 expiring / expired / error 时发
    CertMonitorAlert {
        host: String,
        state: String,
        message: String,
    },
}

impl Event {
    pub fn channel(&self) -> &'static str {
        match self {
            Event::SiteBackupStatus { .. } => "site-backup://status",
            Event::DownloadProgress(_) => "download://progress",
            Event::HostsDenied => "hosts://denied",
            Event::DbBackup(_) => "db://backup",
            Event::PostgresBackup(_) => "postgres://backup",
            Event::CertAuto { .. } => "certauto://status",
            Event::CertMonitorAlert { .. } => "certmonitor://alert",
        }
    }
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Event::SiteBackupStatus { site_id, state, message } => serde_json::json!({ "siteId": site_id, "state": state, "message": message }),
            Event::DownloadProgress(p) => serde_json::to_value(p).unwrap_or_default(),
            Event::HostsDenied => serde_json::json!({}),
            Event::DbBackup(p) => serde_json::to_value(p).unwrap_or_default(),
            Event::PostgresBackup(p) => serde_json::to_value(p).unwrap_or_default(),
            Event::CertAuto { id, state, message } => {
                serde_json::json!({ "id": id, "state": state, "message": message })
            }
            Event::CertMonitorAlert {
                host,
                state,
                message,
            } => {
                serde_json::json!({ "host": host, "state": state, "message": message })
            }
        }
    }
    pub fn progress(
        task_id: &str,
        received: u64,
        total: u64,
        speed: u64,
        eta: f64,
        state: &str,
    ) -> Self {
        Event::DownloadProgress(DownloadProgress {
            task_id: task_id.to_string(),
            received,
            total,
            speed_bps: speed,
            eta_sec: eta,
            state: state.to_string(),
            error: None,
        })
    }
    pub fn state(task_id: &str, state: &str) -> Self {
        Event::DownloadProgress(DownloadProgress {
            task_id: task_id.to_string(),
            received: 0,
            total: 0,
            speed_bps: 0,
            eta_sec: 0.0,
            state: state.to_string(),
            error: None,
        })
    }
}

pub type EventSink = Arc<dyn Fn(Event) + Send + Sync>;

static AUXILIARY_SHUTDOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Default)]
struct BackgroundState {
    paused: bool,
    next_id: u64,
    tasks: std::collections::BTreeMap<u64, String>,
}

#[derive(Default)]
struct BackgroundTasks {
    state: parking_lot::Mutex<BackgroundState>,
    changed: parking_lot::Condvar,
}

static BACKGROUND_TASKS: once_cell::sync::Lazy<Arc<BackgroundTasks>> =
    once_cell::sync::Lazy::new(|| Arc::new(BackgroundTasks::default()));

/// 注册与暂停共用一把锁；持有到文件/数据库写入、部署和通知全部结束。
pub(crate) struct BackgroundWork { registry: Arc<BackgroundTasks>, id: u64 }
impl BackgroundWork {
    pub(crate) fn begin(label: impl Into<String>) -> Result<Self> {
        BACKGROUND_TASKS.begin(label.into())
    }
}
impl Drop for BackgroundWork {
    fn drop(&mut self) {
        self.registry.state.lock().tasks.remove(&self.id);
        self.registry.changed.notify_all();
    }
}
impl BackgroundTasks {
    fn begin(self: &Arc<Self>, label: String) -> Result<BackgroundWork> {
        let mut state = self.state.lock();
        if state.paused {
            return Err(AppError::new("APP_BUSY", "应用正在退出、重启或迁移，暂时无法开始证书或备份任务"));
        }
        state.next_id += 1;
        let id = state.next_id;
        state.tasks.insert(id, label);
        Ok(BackgroundWork { registry: self.clone(), id })
    }
    fn wait_until_idle(&self, timeout: std::time::Duration) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        let mut state = self.state.lock();
        while !state.tasks.is_empty() {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                let mut labels: Vec<_> = state.tasks.values().cloned().collect();
                labels.sort(); labels.dedup();
                return Err(AppError::new("AUXILIARY_STOP_FAILED", format!(
                    "仍有后台任务未完成，操作已中止：{}", labels.join("、")))
                    .with_hint("应用保持打开，未中断这些任务。请完成证书验证或等待备份、监控结束后重试"));
            }
            self.changed.wait_for(&mut state, remaining);
        }
        Ok(())
    }
}

/// 退出/更新/迁移的可恢复准备阶段。后续失败或取消时恢复入口，不自动重跑已取消任务。
pub struct AuxiliaryShutdown { committed: bool }
fn ensure_application_accepts_work() -> Result<()> {
    if AUXILIARY_SHUTDOWN.load(std::sync::atomic::Ordering::Acquire) {
        return Err(AppError::new("APP_BUSY", "应用正在退出、重启或迁移，无法启动新服务"));
    }
    Ok(())
}
impl AuxiliaryShutdown {
    pub fn prepare() -> Result<Self> {
        Self::prepare_with_timeout(std::time::Duration::from_secs(25))
    }
    fn prepare_with_timeout(timeout: std::time::Duration) -> Result<Self> {
        use std::sync::atomic::Ordering;
        AUXILIARY_SHUTDOWN.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AppError::new("APP_BUSY", "应用正在收尾后台任务，请稍候"))?;
        let guard = Self { committed: false };
        BACKGROUND_TASKS.state.lock().paused = true;
        // 先等证书部署、备份写入等自然收尾，再停止它们可能依赖的服务和辅助任务。
        BACKGROUND_TASKS.wait_until_idle(timeout)?;
        let mut errors = Vec::new();
        for (label, result) in [
            ("计划任务", cron::shutdown_checked(std::time::Duration::from_secs(12))),
            ("临时隧道", tunnel::shutdown_checked()),
            ("模型下载", toolbox::ollama_shutdown_checked()),
        ] {
            if let Err(error) = result { errors.push(format!("{label}：{}", error.message)); }
        }
        if !errors.is_empty() {
            return Err(AppError::new("AUXILIARY_STOP_FAILED", format!("后台任务未能全部结束，操作已中止。{}", errors.join("；")))
                .with_hint("应用保持打开。请检查计划任务、隧道或模型下载状态后重试；已停止的任务不会自动重跑"));
        }
        Ok(guard)
    }
    pub fn commit(&mut self) { self.committed = true; }
}
impl Drop for AuxiliaryShutdown {
    fn drop(&mut self) {
        if !self.committed {
            cron::resume_after_shutdown();
            tunnel::resume_after_shutdown();
            toolbox::ollama_resume_after_shutdown();
            BACKGROUND_TASKS.state.lock().paused = false;
            AUXILIARY_SHUTDOWN.store(false, std::sync::atomic::Ordering::Release);
        }
    }
}

pub struct CoreState {
    pub paths: paths::Paths,
    pub store: store::Store,
    pub manager: Arc<ServiceManager>,
    pub downloader: Arc<Downloader>,
    pub installer: install::Installer,
    pub emit: EventSink,
}

impl CoreState {
    pub fn init(base: Option<std::path::PathBuf>, emit: EventSink) -> Result<Arc<Self>> {
        let base = paths::Paths::resolve(base)?;
        let paths = paths::Paths::new(base);
        std::fs::create_dir_all(&paths.base)?;
        let _activity = paths::DataDirActivity::shared(&paths.base)?;
        paths.ensure_dirs().map_err(|e| {
            error::AppError::io("初始化数据目录", e)
                .with_hint("数据目录不可写，可在环境变量 NSB_HOME 指定其它位置")
        })?;
        let store = store::Store::open(paths.db())?;
        paths::finish_data_dir_activation(&paths, || {
            pathenv::sync(&store, &paths, &install::Installer::effective(&paths).manifest)
        })?;
        // 服务栈内置预设（首次运行写入；用户改过的不动）
        let _ = stacks::ensure_presets(&store);
        let manager = Arc::new(ServiceManager::new());
        ops::register_services(&paths, &store, &manager);
        generic::register_services(&paths, &store, &manager);
        // 上次会话崩溃/被强杀时留下的进程：启动即清理，否则它们占着端口让服务起不来
        let orphans = ops::sweep_orphans(&paths, &store, &manager);
        // effective() 要在 paths 被 move 进 state 之前用掉
        let installer = install::Installer::effective(&paths);
        let state = Arc::new(Self {
            paths,
            store,
            manager,
            downloader: Arc::new(Downloader::new()),
            installer,
            emit,
        });
        if !orphans.adopted.is_empty() || !orphans.killed.is_empty() || !orphans.unresolved.is_empty() {
            let mut parts = Vec::new();
            if !orphans.adopted.is_empty() {
                let d = orphans
                    .adopted
                    .iter()
                    .map(|(sid, pid)| format!("{sid}(pid {pid})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                parts.push(format!("接管 {}", d));
            }
            if !orphans.killed.is_empty() {
                let d = orphans
                    .killed
                    .iter()
                    .map(|(sid, pid)| format!("{sid}(pid {pid})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                parts.push(format!("清理 {}", d));
            }
            parts.extend(orphans.unresolved.iter().cloned());
            (state.emit)(Event::DownloadProgress(model::DownloadProgress {
                task_id: "orphans".into(),
                received: 0,
                total: 0,
                speed_bps: 0,
                eta_sec: 0.0,
                state: if orphans.unresolved.is_empty() { "orphans-resolved" } else { "orphans-pending" }.into(),
                error: Some(parts.join("；")),
            }));
        }
        // 调度器由桌面端在启动交接提交后放行；CLI/MCP 初始化不启动后台任务。
        Ok(state)
    }

    /// 供外部调用的便捷（下载进度等）
    pub fn emit_event(&self, e: Event) {
        (self.emit)(e);
    }
}

/// hosts 写入失败的提示事件（带当前应写条目数）
pub fn emit_hosts_denied(store: &store::Store, _paths: &paths::Paths) {
    let n = hosts::managed_entries(store).map(|entries| entries.len());
    let e = Event::HostsDenied;
    let _ = n;
    // desktop 侧 listen 后 toast 提示
    let _ = e;
}

/* ================= 门面 API（desktop 命令与 smoke 测试共用） ================= */

impl CoreState {
    pub fn list_packages(&self) -> Result<Vec<model::PackageView>> {
        let installed = self.store.list_installed()?;
        // 清单与安装记录取并集，清单外的已安装版本不能因上游目录变化而消失。
        let mut views = self.installer.package_views(&installed);
        // 所有套件（包括纯运行时和清单扩展服务）都按实际选择标记使用中版本。
        let mut active_map: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let ids: std::collections::HashSet<_> = installed.iter().map(|p| p.id.as_str()).collect();
        for id in ids {
            if let Some(p) = ops::installed_by_choice(&self.store, id) {
                active_map.insert(id.to_string(), p.version);
            }
        }
        for v in views.iter_mut() {
            if let Some(av) = active_map.get(&v.manifest.id) {
                v.active = v.manifest.version == *av;
            }
        }
        Ok(views)
    }

    pub async fn install_package(&self, key: &str) -> Result<model::InstalledPackage> {
        let installed = self
            .installer
            .install(key, &self.paths, &self.store, &self.downloader, &|e| {
                (self.emit)(e)
            })
            .await?;
        ops::register_services(&self.paths, &self.store, &self.manager);
        generic::register_services(&self.paths, &self.store, &self.manager);
        // 装完即让命令可用（开关开着才真正写盘；失败不阻断安装）
        let _ = pathenv::sync(&self.store, &self.paths, &self.installer.manifest);
        Ok(installed)
    }

    pub fn uninstall_package(&self, key: &str) -> Result<()> {
        let _sites = sites::SITE_CHANGES.lock();
        let _operation = self.manager.lifecycle.lock();
        let task_id = match key.split_once('@') {
            Some(_) => key.to_string(),
            None => ops::installed_by_choice(&self.store, key)
                .map(|p| format!("{}@{}", p.id, p.version))
                .ok_or_else(|| AppError::not_installed(key))?,
        };
        let operation = self.downloader.begin_task(&task_id)?;
        operation.begin_commit()?;
        let stopped_service =
            self.installer
                .uninstall(key, &self.paths, &self.store, &self.manager)?;
        if let Some(sid) = stopped_service {
            self.manager.watchdog.forget(&sid);
        }
        // 卸载后目录已不存在，必须把托管条目摘掉，否则 PATH 里留死路径
        pathenv::sync(&self.store, &self.paths, &self.installer.manifest).map_err(|error| {
            AppError::new(
                "UNINSTALL_PATH_SYNC_FAILED",
                format!("{task_id} 已卸载，但 PATH 清理失败"),
            )
            .with_hint("无需再次卸载。请重试 PATH 清理，或到「工具箱 → 环境变量」重新应用 PATH；完成后新开终端。")
            .with_detail(format!("{}: {}", error.code, error.message))
        })
    }

    /// 切换「使用中版本」并同步 PATH；已单独选择的 PATH 版本保持不变
    pub fn set_active_version(&self, id: &str, version: &str) -> Result<()> {
        let _operation = self.manager.lifecycle.lock();
        if ops::installed_by_choice(&self.store, id).is_some_and(|p| p.version != version)
            && self.manager.is_busy(id)
        {
            return Err(AppError::new(
                "SERVICE_BUSY",
                format!("{id} 正在运行或启停中，请先停止再切换版本"),
            ));
        }
        ops::set_active_version(&self.store, id, version)?;
        ops::register_services(&self.paths, &self.store, &self.manager);
        generic::register_services(&self.paths, &self.store, &self.manager);
        pathenv::sync(&self.store, &self.paths, &self.installer.manifest).map_err(|error| {
            AppError::new(
                "ACTIVE_VERSION_PATH_SYNC_FAILED",
                format!("{id} 默认版本已设为 {version}，但 PATH 同步失败"),
            )
            .with_hint("可重试本次操作，或到「工具箱 → 环境变量」重新应用 PATH；完成后新开终端。")
            .with_detail(format!("{}: {}", error.code, error.message))
        })
    }

    /* ---------- 工具箱扩展（计划任务 / 快速隧道 / Ollama / Adminer） ---------- */

    pub fn cron_jobs(&self) -> Result<Vec<crate::cron::CronJob>> {
        self.store.list_cron_jobs()
    }

    pub fn cron_save(&self, mut job: crate::cron::CronJob) -> Result<()> {
        job.name = job.name.trim().to_string();
        job.command = job.command.trim().to_string();
        let create = job.id.trim().is_empty();
        if create {
            job.id = crate::cron::new_id();
            job.created_at = crate::services::now_ms();
        } else if self.store.get_cron_job(&job.id)?.is_none() {
            return Err(AppError::new("CRON_NOT_FOUND", "待编辑任务不存在，请刷新列表"));
        }
        crate::cron::validate_job(&job)?;
        self.store.save_cron_job(&job, create)
    }

    pub fn cron_delete(&self, id: &str) -> Result<()> {
        self.store.delete_cron_job(id)
    }

    pub fn cron_set_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        self.store.set_cron_enabled(id, enabled)
    }

    pub fn cron_run_now(&self, id: &str) -> Result<crate::cron::CronJob> {
        crate::cron::run_job(&self.store, id, true)
    }

    pub fn cron_stop(&self, id: &str) -> Result<()> {
        crate::cron::stop_job(&self.store, id)
    }

    pub fn tunnel_start(&self, port: u16) -> Result<model::TunnelInfo> {
        let exe = toolbox::resolve_exe(&self.store, &self.paths, &self.installer, "cloudflared")?;
        crate::tunnel::start(&exe, port)
    }

    pub fn tunnel_start_site(&self, id: &str) -> Result<model::TunnelInfo> {
        let _sites = sites::SITE_CHANGES.lock();
        let _operation = self.manager.lifecycle.lock();
        let site = self
            .store
            .list_sites()?
            .into_iter()
            .find(|site| site.id == id)
            .ok_or_else(|| AppError::new("SITE_NOT_FOUND", "站点不存在，请刷新列表"))?;
        if sites::runtime_status(&self.paths, &site, &self.manager) != "running" {
            return Err(AppError::new(
                "TUNNEL_SITE_STOPPED",
                "请先启动此站点及其依赖服务，再创建隧道",
            ));
        }
        let address = sites::access_url(&self.paths, &self.store, &self.manager, id)?;
        let target = crate::tunnel::Target::site(&site, &address, &self.paths, &self.store, self.manager.clone())?;
        let exe = toolbox::resolve_exe(&self.store, &self.paths, &self.installer, "cloudflared")?;
        crate::tunnel::start_target(&exe, target)
    }

    pub fn tunnel_list(&self) -> Vec<model::TunnelInfo> {
        crate::tunnel::list()
    }

    pub fn tunnel_stop(&self, id: &str) -> Result<()> {
        crate::tunnel::stop(id)
    }

    pub fn tunnel_remove(&self, id: &str) -> Result<()> {
        crate::tunnel::remove(id)
    }

    pub fn ollama_models(&self) -> Result<Vec<toolbox::OllamaModelRow>> {
        toolbox::ollama_models(self.manager.clone())
    }

    pub fn ollama_delete(&self, name: &str) -> Result<()> {
        toolbox::ollama_delete(self.manager.clone(), name)
    }

    pub fn ollama_pull(&self, name: &str) -> Result<toolbox::OllamaPullStatus> {
        toolbox::ollama_pull(self.manager.clone(), name)
    }

    pub fn adminer_start(&self) -> Result<toolbox::AdminerStatus> {
        let status = toolbox::adminer_start(&self.store, &self.paths, &self.installer, &self.manager)?;
        ops::save_pidfile(&self.paths, &self.manager);
        Ok(status)
    }

    pub fn adminer_status(&self) -> Result<Option<toolbox::AdminerStatus>> {
        toolbox::adminer_status(&self.manager)
    }

    pub fn adminer_stop(&self) -> Result<()> {
        toolbox::adminer_stop(&self.manager)?;
        ops::save_pidfile(&self.paths, &self.manager);
        Ok(())
    }

    /* ---------- 环境变量（PATH 注入） ---------- */

    /// 环境变量注入的完整状态（含每个已安装包可注入的命令）
    pub fn pathenv_status(&self) -> model::PathEnvStatus {
        pathenv::status(&self.store, &self.installer.manifest)
    }

    pub fn terminal_environment(&self) -> Result<model::TerminalEnvironment> {
        let _operation = self.manager.lifecycle.lock();
        pathenv::terminal_environment(&self.store, &self.paths, &self.installer.manifest)
    }

    pub fn site_terminal_environment(&self, site_id: &str) -> Result<model::TerminalEnvironment> {
        let _sites = sites::SITE_CHANGES.lock();
        let _operation = self.manager.lifecycle.lock();
        pathenv::site_terminal_environment(&self.store, &self.paths, &self.installer.manifest, site_id)
    }

    pub fn project_runtime_versions(&self, site_id: &str) -> Result<model::ProjectRuntimeVersions> {
        let _sites = sites::SITE_CHANGES.lock();
        let _operation = self.manager.lifecycle.lock();
        pathenv::project_runtime_versions(&self.store, &self.installer.manifest, site_id)
    }

    pub fn save_project_runtime_versions(&self, site_id: &str, versions: &std::collections::BTreeMap<String, String>, expected_revision: &str) -> Result<model::ProjectRuntimeVersions> {
        let _sites = sites::SITE_CHANGES.lock();
        let _operation = self.manager.lifecycle.lock();
        pathenv::save_project_runtime_versions(&self.store, &self.installer.manifest, site_id, versions, expected_revision)
    }

    /// 在启动前重新生成快照，并把版本和站点变更锁持有到进程创建完成。
    pub fn with_terminal_environment<T>(&self, site_id: Option<&str>, expected_revision: &str, launch: impl FnOnce(&model::TerminalEnvironment) -> Result<T>) -> Result<T> {
        let _sites = sites::SITE_CHANGES.lock();
        let _operation = self.manager.lifecycle.lock();
        let environment = match site_id {
            Some(id) => pathenv::site_terminal_environment(&self.store, &self.paths, &self.installer.manifest, id)?,
            None => pathenv::terminal_environment(&self.store, &self.paths, &self.installer.manifest)?,
        };
        if environment.revision != expected_revision {
            return Err(AppError::new("TERMINAL_ENV_CHANGED", "终端目录或版本选择已变化，未打开终端").with_hint("请刷新环境预览后重试。"));
        }
        launch(&environment)
    }

    /// 开/关总开关
    pub fn pathenv_set_enabled(&self, enabled: bool) -> Result<model::PathEnvStatus> {
        let _operation = self.manager.lifecycle.lock();
        pathenv::set_enabled(&self.store, &self.paths, &self.installer.manifest, enabled)
    }

    /// 设置要注入 PATH 的包集合
    pub fn pathenv_set_selected(&self, ids: Vec<String>) -> Result<model::PathEnvStatus> {
        let _operation = self.manager.lifecycle.lock();
        pathenv::set_selected(&self.store, &self.paths, &self.installer.manifest, &ids)
    }

    pub fn pathenv_set_version(
        &self,
        id: &str,
        version: &str,
        selected: bool,
    ) -> Result<model::PathEnvStatus> {
        let _operation = self.manager.lifecycle.lock();
        pathenv::set_version(
            &self.store,
            &self.paths,
            &self.installer.manifest,
            id,
            version,
            selected,
        )
    }

    /// 强制重新应用（修漂移：用户手动改过 PATH 或换了版本）
    pub fn pathenv_reapply(&self) -> Result<model::PathEnvStatus> {
        let _operation = self.manager.lifecycle.lock();
        pathenv::apply(&self.store, &self.paths, &self.installer.manifest)
    }

    /// 服务列表 + 前置依赖信息。
    ///
    /// 依赖来自清单的 `run.requires`，在这里补齐而不是塞进 ServiceManager：
    /// - manager 只关心进程，不该知道清单；
    /// - 判断「依赖是否已安装」需要 store，而 manager 拿不到 store。
    pub fn with_mysql<T>(
        &self,
        version: Option<&str>,
        operation: impl FnOnce(&str, &dbadmin::MySqlClient) -> Result<T>,
    ) -> Result<T> {
        self.with_database(dbadmin::DatabaseEngine::Mysql, version, operation)
    }

    pub fn with_database<T>(
        &self,
        engine: dbadmin::DatabaseEngine,
        version: Option<&str>,
        operation: impl FnOnce(&str, &dbadmin::MySqlClient) -> Result<T>,
    ) -> Result<T> {
        let _operation = self.manager.lifecycle.lock();
        let (version, client) = dbadmin::authenticated_client(self, engine, version, None)?;
        operation(&version, &client)
    }

    /// use_existing 只更新本机连接凭据；false 同时修改实例中现有的本地 root 账号。
    pub fn set_mysql_password(
        &self,
        version: Option<&str>,
        password: &str,
        use_existing: bool,
    ) -> Result<()> {
        self.set_database_password(dbadmin::DatabaseEngine::Mysql, version, password, use_existing)
    }

    pub fn set_database_password(
        &self,
        engine: dbadmin::DatabaseEngine,
        version: Option<&str>,
        password: &str,
        use_existing: bool,
    ) -> Result<()> {
        let _operation = self.manager.lifecycle.lock();
        if password.is_empty() || password.chars().any(char::is_control) {
            return Err(AppError::new("BAD_PASSWORD", "密码不能为空或包含控制字符"));
        }
        let (version, client) = dbadmin::authenticated_client(
            self,
            engine,
            version,
            use_existing.then(|| password.to_string()),
        )?;
        let key = engine.password_key(&version);
        let data = engine.data_dir(&self.paths, &version)?;
        self.store.set_setting(&key, password)?;
        if !use_existing {
            let new_client = dbadmin::MySqlClient {
                exe: client.exe.clone(),
                port: client.port,
                root_password: password.into(),
            };
            if let Err(error) = client.reset_root_password(password) {
                // 网络中断可能发生在 ALTER 已成功后，先验证新凭据再决定回退本机记录。
                if new_client
                    .verify_data_dir(&data)
                    .is_ok()
                {
                    return Ok(());
                }
                if client
                    .verify_data_dir(&data)
                    .is_ok()
                {
                    self.store.set_setting(&key, &client.root_password)?;
                }
                return Err(error.with_hint(
                    "请确认实例是否仍在运行；若连接中断，请用当前实例密码更新本机连接记录",
                ));
            }
            new_client.verify_data_dir(&data)?;
        }
        let id = if engine == dbadmin::DatabaseEngine::Mysql { format!("mysql@{version}") } else { "mariadb".into() };
        self.manager.set_state(&id, model::ServiceState::Running);
        Ok(())
    }

    pub fn migrate_list_source(
        &self,
        host: String,
        port: u16,
        user: String,
        password: String,
        version: Option<&str>,
        engine: dbadmin::DatabaseEngine,
    ) -> Result<Vec<dbmigrate::SourceDb>> {
        let _operation = self.manager.lifecycle.lock();
        let bin = self.installed_database_bin_dir(engine, version)?;
        dbmigrate::list_source_databases_with_engine(
            &bin,
            engine,
            &dbmigrate::SourceConn {
                host,
                port,
                user,
                password,
            },
        )
    }

    pub fn migrate_import(
        &self,
        host: String,
        port: u16,
        user: String,
        password: String,
        databases: Vec<String>,
        version: Option<&str>,
        engine: dbadmin::DatabaseEngine,
    ) -> Result<dbmigrate::ImportReport> {
        let src = dbmigrate::SourceConn {
            host,
            port,
            user,
            password,
        };
        self.with_database(engine, version, |version, client| {
            let target = dbbackup::ConnInfo {
                engine,
                version: version.into(),
                port: client.port,
                root_password: client.root_password.clone(),
                bin_dir: client.exe.parent().map(std::path::Path::to_path_buf),
            };
            dbmigrate::import_databases(&self.paths, &src, &databases, &target, |db, status| {
                (self.emit)(crate::Event::progress(
                    "db-import",
                    0,
                    0,
                    0,
                    0.0,
                    &format!("{db}: {status}"),
                ));
            })
        })
    }

    /// 使用所选安装的真实路径，避免客户端版本与目标实例不一致。
    fn installed_database_bin_dir(&self, engine: dbadmin::DatabaseEngine, version: Option<&str>) -> Result<std::path::PathBuf> {
        let package = match version {
            Some(version) => self.store.find_installed(engine.id(), Some(version)),
            None => crate::ops::installed_by_choice(&self.store, engine.id()),
        }
        .ok_or_else(|| AppError::not_installed(engine.label()))?;
        engine.bin_dir(&package)
    }

    pub fn with_postgres<T>(&self, version: &str, operation: impl FnOnce(&dbadmin::PostgresClient) -> Result<T>) -> Result<T> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.lock();
        let client = dbadmin::selected_postgres(self, version, None)?;
        operation(&client)
    }

    pub fn postgres_connection(&self, version: &str) -> Result<dbadmin::PostgresConnectionInfo> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.lock();
        dbadmin::selected_postgres(self, version, None)?.info(version)
    }

    pub fn postgres_password(&self, version: &str) -> Result<String> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.lock();
        let client = dbadmin::selected_postgres(self, version, None)?;
        if !client.password_required()? {
            return Err(AppError::new("POSTGRES_AUTH_DISABLED", "本机连接未要求密码，无法验证保存的密码；请先设置密码并启用认证"));
        }
        Ok(client.password)
    }

    pub fn set_postgres_password(&self, version: &str, password: &str, use_existing: bool, enable_password_auth: bool) -> Result<()> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.lock();
        if password.is_empty() || password.len() > 4096 || password.chars().any(char::is_control) {
            return Err(AppError::new("BAD_PASSWORD", "密码须为 1–4096 字节且不能包含控制字符"));
        }
        let client = dbadmin::selected_postgres(self, version, use_existing.then(|| password.to_string()))?;
        let requires_password = client.password_required()?;
        if use_existing && !requires_password {
            return Err(AppError::new("POSTGRES_AUTH_DISABLED", "当前本机连接无需密码，无法验证输入的密码")
                .with_hint("请选择修改 postgres 密码，并同时启用本机密码认证。"));
        }
        // 在改密前验证可转换的规则，不能先改密码再发现用户配置不受支持。
        let auth_update = if !use_existing && enable_password_auth && !requires_password {
            Some(client.local_auth_update(&self.paths, version)?)
        } else { None };
        let key = dbadmin::postgres_password_key(version);
        self.store.set_setting(&key, password)?;
        if !use_existing {
            let new_client = dbadmin::PostgresClient { exe: client.exe.clone(), port: client.port, password: password.into() };
            let data = self.paths.postgres_data_dir(version);
            if let Err(error) = client.change_password(password) {
                // 仅密码认证可证明新凭据已生效；trust 下不能靠查询成功推断改密成功。
                if !requires_password || new_client.verify_data_dir(&data).is_err() {
                    if requires_password && client.verify_data_dir(&data).is_ok() {
                        self.store.set_setting(&key, &client.password)?;
                        return Err(error.with_hint("新密码未通过验证，本机记录已恢复为仍可连接的原密码；请检查实例日志后重试。"));
                    }
                    return Err(error.with_hint("改密结果未确认，已保留新的本机凭据；请检查实例日志并验证当前密码。"));
                }
            }
            if let Some((file, previous, updated)) = auth_update {
                let apply = || -> Result<()> {
                    paths::write_with_backup_expected(&file, &updated, &self.paths.backup(), Some(Some(previous.as_bytes())))?;
                    if new_client.query("SELECT pg_reload_conf();")?.trim() != "t" {
                        return Err(AppError::new("POSTGRES_AUTH_RELOAD", "PostgreSQL 未接受认证配置重载"));
                    }
                    let started = std::time::Instant::now();
                    loop {
                        if new_client.password_required()? { return Ok(()); }
                        if started.elapsed() >= std::time::Duration::from_secs(5) { break; }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    Err(AppError::new("POSTGRES_AUTH_RELOAD", "未确认本机密码认证生效"))
                };
                apply().map_err(|error| error.with_hint("账号密码已修改并保存，但认证配置应用未确认。请检查 pg_hba.conf 和实例日志；旧配置如已被替换，可在配置备份中恢复。"))?;
            }
            new_client.verify_data_dir(&data)?;
        }
        self.manager.set_state("postgresql", model::ServiceState::Running);
        Ok(())
    }

    fn running_redis(&self, version: Option<&str>) -> Result<model::ServiceStatus> {
        let service = self.manager.snapshot("redis")
            .filter(|s| matches!(s.state, model::ServiceState::Running | model::ServiceState::Error) && s.pids.iter().any(|pid| platform::process_alive(*pid)))
            .ok_or_else(|| AppError::new("REDIS_NOT_RUNNING", "请先启动 Redis 实例"))?;
        if version.is_some_and(|v| service.version.as_deref() != Some(v)) {
            return Err(AppError::new("REDIS_INSTANCE_CHANGED", "运行中的 Redis 版本已变化，请重新打开连接设置"));
        }
        Ok(service)
    }

    fn verify_redis(&self, service: &model::ServiceStatus, credentials: &stats::RedisCredentials) -> Result<stats::RedisStats> {
        let port = service.port.ok_or_else(|| AppError::new("REDIS_PORT_UNKNOWN", "无法确认 Redis 实际端口，请重新启动该实例"))?;
        let stats = stats::redis_stats_authenticated(port, credentials, Some(&service.pids))?;
        if !service.pids.contains(&stats.process_id) {
            return Err(AppError::new("REDIS_INSTANCE_MISMATCH", "该端口的 Redis 不属于当前托管实例，未显示其统计数据"));
        }
        Ok(stats)
    }

    pub fn redis_stats(&self) -> Result<stats::RedisStats> {
        let _operation = self.manager.lifecycle.lock();
        let service = self.running_redis(None)?;
        let version = service.version.as_deref().ok_or_else(|| AppError::new("REDIS_VERSION_UNKNOWN", "无法确认 Redis 运行版本"))?;
        self.verify_redis(&service, &stats::RedisCredentials::load(&self.store, version)?)
    }

    pub fn redis_settings(&self, version: &str) -> Result<redis_settings::RedisSettingsView> {
        let _operation = self.manager.lifecycle.lock();
        redis_settings::get(&self.paths, &self.store, version)
    }

    pub fn redis_persistence(&self, version: &str) -> Result<stats::RedisPersistence> {
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后读取持久化状态"))?;
        let service = self.running_redis(Some(version))?;
        let port = service.port.ok_or_else(|| AppError::new("REDIS_PORT_UNKNOWN", "无法确认 Redis 实际端口"))?;
        stats::redis_persistence(port, &stats::RedisCredentials::load(&self.store, version)?, &service.pids, version)
    }

    pub fn redis_snapshot(&self, version: &str) -> Result<stats::RedisSnapshotReceipt> {
        let _work = BackgroundWork::begin("请求 Redis RDB 快照")?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "已有服务操作正在进行，请稍后重试"))?;
        let service = self.running_redis(Some(version))?;
        let port = service.port.ok_or_else(|| AppError::new("REDIS_PORT_UNKNOWN", "无法确认 Redis 实际端口"))?;
        stats::redis_snapshot(port, &stats::RedisCredentials::load(&self.store, version)?, &service.pids, version)
    }

    pub fn save_redis_settings(&self, version: &str, revision: &str, settings: &redis_settings::RedisSettings, acknowledge_disable: bool) -> Result<redis_settings::RedisSettingsView> {
        let _operation = self.manager.lifecycle.lock();
        redis_settings::save(&self.paths, &self.store, version, revision, settings, acknowledge_disable)
    }

    pub fn redis_connection(&self, version: &str) -> Result<stats::RedisConnectionInfo> {
        self.store.find_installed("redis", Some(version)).ok_or_else(|| AppError::not_installed("Redis"))?;
        let credentials = stats::RedisCredentials::load(&self.store, version)?;
        Ok(stats::RedisConnectionInfo { version: version.into(), username: credentials.username, has_password: !credentials.password.is_empty() })
    }

    pub fn save_redis_connection(&self, version: &str, credentials: stats::RedisCredentials) -> Result<stats::RedisStats> {
        let _operation = self.manager.lifecycle.lock();
        let service = self.running_redis(Some(version))?;
        let stats = self.verify_redis(&service, &credentials)?;
        self.store.set_setting_json(&stats::RedisCredentials::key(version), &credentials)?;
        self.manager.set_state("redis", model::ServiceState::Running);
        Ok(stats)
    }

    pub fn service_history(&self, n: usize) -> Vec<(i64, String, String)> {
        self.manager.history_tail(n)
    }

    pub fn service_web_url(&self, id: &str) -> Result<String> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后打开管理台"))?;
        generic::service_web_url(&self.manager, id)
    }

    pub async fn repair_service_web_ui(self: &Arc<Self>, id: &str, version: &str) -> Result<String> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        ensure_application_accepts_work()?;
        let (before, installed) = {
            let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
            let service = self.manager.snapshot(id).filter(|s| id == "qdrant" && s.state == model::ServiceState::Running
                && s.version.as_deref() == Some(version) && !s.pids.is_empty())
                .ok_or_else(|| AppError::new("SERVICE_CHANGED", "Qdrant 状态或版本已变化，请刷新后重试"))?;
            if !self.manager.web_target(id).is_err_and(|error| error.code == "QDRANT_WEB_MISSING") {
                return Err(AppError::new("QDRANT_WEB_CUSTOM", "当前服务没有可自动补齐的管理台，请检查配置"));
            }
            let installed = self.store.find_installed(id, Some(version)).ok_or_else(|| AppError::not_installed("Qdrant"))?;
            (service, installed)
        };
        // 下载期间服务继续运行；若用户停止或切换版本，不能在下载完成后擅自拉起。
        self.installer.repair_qdrant_web_ui(&installed, &self.paths, &self.store, &self.downloader, &|e| (self.emit)(e)).await?;
        let state = self.clone(); let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            let _activity = _activity;
            let _operation = state.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "管理台已补齐；请在当前操作完成后重启 Qdrant"))?;
            let unchanged = state.manager.snapshot(&id).is_some_and(|s| s.state == model::ServiceState::Running
                && s.version == before.version && s.pids == before.pids);
            if !unchanged { return Err(AppError::new("SERVICE_CHANGED", "管理台已补齐，但服务状态已变化；请手动启动或重启 Qdrant")); }
            state.restart_service(&id)?;
            state.service_web_url(&id)
        }).await.map_err(|error| AppError::internal("补齐管理台", error.to_string()))?
    }

    pub fn service_status_list(&self) -> Vec<model::ServiceStatus> {
        let mut list = self.manager.list_status();
        let installed = self.store.list_installed().unwrap_or_default();
        for st in list.iter_mut() {
            // 服务 id 形如 php@8.3.33，清单里查的是基础 id
            let base = st.id.split('@').next().unwrap_or(&st.id);
            if let Some(entry) = self
                .installer
                .manifest
                .packages
                .iter()
                .find(|p| p.id == base)
            {
                // 服务类看 run.requires；纯运行时（composer/gradle）看顶层 requires。
                // 两处都查，才不会出现「清单里写了但界面不提示」。
                let mut deps: Vec<String> = Vec::new();
                if let Some(run) = &entry.run {
                    deps.extend(run.requires.iter().cloned());
                }
                deps.extend(entry.requires.iter().cloned());
                deps.sort();
                deps.dedup();
                if !deps.is_empty() {
                    st.missing_requires = deps
                        .iter()
                        .filter(|dep| !installed.iter().any(|i| &i.id == *dep))
                        .cloned()
                        .collect();
                    st.requires = deps;
                }
            }
        }
        list
    }

    /// 某包的完整版本目录：远程枚举（带缓存）+ 本地已装标记。
    /// `force` 忽略缓存（用户点「刷新版本」）。
    pub async fn version_catalog(&self, id: &str, force: bool) -> Result<model::VersionCatalog> {
        let template = self
            .installer
            .template_for(id)
            .ok_or_else(|| AppError::new("PACKAGE_NOT_FOUND", format!("清单里没有套件 {id}")))?;
        if !install::Installer::is_platform_compatible(&template) {
            return Ok(model::VersionCatalog {
                id: id.to_string(),
                error: Some("该套件暂未提供适用于当前系统和架构的版本".into()),
                ..Default::default()
            });
        }
        Ok(versions::catalog(&self.store, &template, force).await)
    }

    /// 批量版本目录（前端一次拉全部有版本源的包，避免 N 次 invoke）
    pub async fn version_catalogs(&self, force: bool) -> Vec<model::VersionCatalog> {
        let mut ids: Vec<String> = self
            .installer
            .manifest
            .packages
            .iter()
            .filter(|p| versions::source_for(p).is_some())
            .map(|p| p.id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        use futures_util::{stream, StreamExt};
        let mut out: Vec<_> = stream::iter(ids)
            .map(|id| async move {
                match self.version_catalog(&id, force).await {
                    Ok(c) => c,
                    Err(e) => model::VersionCatalog {
                        id: id.clone(),
                        remote: vec![],
                        online: false,
                        cached_at: None,
                        error: Some(e.message),
                    },
                }
            })
            .buffer_unordered(6)
            .collect()
            .await;
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn sftpgo_config_directories(&self) -> Result<model::SftpgoConfigDirectories> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后读取配置目录"))?;
        generic::sftpgo_config_directories(&self.store, &self.paths)
    }

    pub fn select_sftpgo_config(&self, directory: &str, version: &str, expected_current: Option<&str>) -> Result<()> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后选择配置目录"))?;
        ensure_application_accepts_work()?;
        if self.downloader.has_tasks() { return Err(AppError::new("PACKAGE_BUSY", "套件正在安装或卸载，请完成后再选择配置目录")); }
        generic::select_sftpgo_config(&self.store, &self.paths, &self.manager, directory, version, expected_current)
    }

    pub fn start_service(&self, id: &str) -> Result<()> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后启动"))?;
        ensure_application_accepts_work()?;
        let r = ops::start_service(&self.store, &self.paths, &self.manager, id);
        // 记录托管 pid：崩溃后下次启动靠它找回残留进程
        ops::save_pidfile(&self.paths, &self.manager);
        r
    }

    pub fn stop_service(&self, id: &str) -> Result<()> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后停止"))?;
        self.manager.snapshot(id).ok_or_else(|| AppError::new("UNKNOWN_SERVICE", format!("服务 {id} 未注册或已卸载")))?;
        let r = ops::stop_service(&self.store, &self.paths, &self.manager, id);
        ops::save_pidfile(&self.paths, &self.manager);
        r
    }

    /// 强制停止须先展示当前实例，再用同一份进程身份修订号确认。
    pub fn service_stop_preview(&self, id: &str) -> Result<model::ServiceStopPreview> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重新读取"))?;
        ops::service_stop_preview(&self.manager, id)
    }

    pub fn force_stop_service(&self, id: &str, revision: &str) -> Result<()> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后停止"))?;
        let result = ops::force_stop_service(&self.store, &self.paths, &self.manager, id, revision);
        ops::save_pidfile(&self.paths, &self.manager);
        result
    }

    /// 一次用户重启是不可交错的停止与启动；失败阶段保留原错误码和诊断字段。
    pub fn restart_service(&self, id: &str) -> Result<()> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重启"))?;
        ensure_application_accepts_work()?;
        let before = self.manager.snapshot(id).ok_or_else(|| AppError::new("UNKNOWN_SERVICE", format!("服务 {id} 未注册或已卸载")))?;
        if matches!(before.state, model::ServiceState::Starting | model::ServiceState::Stopping) {
            return Err(AppError::new("SERVICE_BUSY", "服务正在切换状态，请稍后重启"));
        }
        self.stop_service(id).map_err(|mut error| {
            error.message = format!("重启中止，停止阶段失败：{}", error.message);
            self.manager.set_error(id, error.clone());
            error
        })?;
        self.start_service(id).map_err(|mut error| {
            error.message = format!("服务已停止，但重新启动失败：{}", error.message);
            self.manager.set_error(id, error.clone());
            error
        })
    }

    /// 桌面启动策略：只处理启动实际报告的 TCP 冲突，不预先结束一组计划端口。
    /// 回调保留已完成的端口处理，即使后续启动失败也可向用户报告。
    pub fn start_service_with_port_policy(
        &self,
        id: &str,
        mut on_freed: impl FnMut(u16),
    ) -> Result<()> {
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| {
            AppError::new("SERVICE_BUSY", "服务正在操作，请稍后启动")
        })?;
        let mut handled = std::collections::HashSet::new();
        let result = (|| loop {
            let error = match self.start_service(id) {
                Ok(()) => return Ok(()),
                Err(error) => error,
            };
            if error.code != "PORT_IN_USE" {
                return Err(error);
            }
            let enabled = match self.store.get_setting_checked("autoClosePortOnStart")?.as_deref() {
                None | Some("true") => true,
                Some("false") => false,
                Some(_) => return Err(AppError::new("BAD_SETTING", "自动释放端口设置无效，请在设置中重新选择")),
            };
            let (Some(port), Some(pid)) = (error.port, error.pid) else {
                return Err(error);
            };
            if !enabled || handled.len() >= 32 || !handled.insert((port, pid)) {
                return Err(error);
            }
            let current = self.scan_port_range(port, port)?;
            let targets = current.listeners.into_iter()
                .filter(|row| row.pid == pid)
                .collect::<Vec<_>>();
            // 扫描时旧进程已经退出，也只复查结果；不能改为结束另一个新占用者。
            let outcome = self.close_port_checked(port, &targets)?;
            if !outcome.port_free || !outcome.errors.is_empty() {
                return Err(AppError::new("PORT_AUTO_CLOSE_FAILED", format!("端口 {port} 未确认释放，启动已中止"))
                    .with_hint("到工具箱重新查看监听者后重试")
                    .with_detail(outcome.errors.join("；")));
            }
            if !outcome.killed_pids.is_empty() {
                on_freed(port);
            }
        })();
        if let Err(error) = &result {
            if self.manager.snapshot(id).is_some_and(|status| status.state == model::ServiceState::Error) {
                self.manager.set_error(id, error.clone());
            }
        }
        result
    }

    /// 将运行时、配置、服务数据、证书和本地数据库迁移到新目录。
    /// 迁移期间必须没有安装任务；受管服务会先全部优雅停止，桌面端随后重启进程。
    pub fn migrate_data_dir(&self, target: &std::path::Path) -> Result<paths::DataDirMigration> {
        self.prepare_data_dir_migration(target).map(|(result, _guard)| result)
    }

    pub fn prepare_data_dir_migration(&self, target: &std::path::Path) -> Result<(paths::DataDirMigration, paths::DataDirActivity)> {
        // 有在途请求时先返回，不为一次尚不能开始的复制提前停掉服务。
        drop(paths::DataDirActivity::exclusive(&self.paths.base)?);
        self.with_stopped_services(|| {
            let guard = paths::DataDirActivity::exclusive(&self.paths.base)?;
            paths::copy_data_dir(&self.paths.base, target).map(|result| (result, guard))
        })
    }

    /// 退出、重启、更新及迁移的共同前置条件；只有完整停机后才执行后续动作。
    /// 生命周期锁贯穿检查与后续动作，失败保留原应用和恢复记录。
    pub fn with_stopped_services<T>(&self, next: impl FnOnce() -> Result<T>) -> Result<T> {
        let _operation = self.manager.lifecycle.try_lock()
            .ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请完成后再退出、重启或迁移"))?;
        if self.downloader.has_tasks() {
            return Err(AppError::new(
                "PACKAGE_BUSY",
                "当前仍有套件安装或卸载任务，请等待完成后再退出、重启或迁移",
            ));
        }
        let report = self.stop_all_services()?;
        ops::save_pidfile_checked(&self.paths, &self.manager)?;
        if !report.failed.is_empty() {
            let failures = report.failed.iter()
                .map(|item| format!("{}：{}", item.service_id, item.error.message)).collect::<Vec<_>>();
            return Err(AppError::new("SERVICES_STOP_FAILED",
                format!("未能停止全部服务，操作已中止。{}", failures.join("；")))
                .with_hint("应用保持打开，已停止的服务不会自动恢复；请查看服务日志，处理失败项后重试")
                .with_detail(serde_json::to_string(&report).unwrap_or_default()));
        }
        let active = self
            .manager
            .list_status()
            .into_iter()
            .filter(|status| {
                !status.pids.is_empty() || matches!(
                    status.state,
                    model::ServiceState::Running
                        | model::ServiceState::Starting
                        | model::ServiceState::Stopping
                )
            })
            .map(|status| status.label)
            .collect::<Vec<_>>();
        if !active.is_empty() {
            return Err(AppError::new(
                "SERVICES_BUSY",
                format!("仍有服务未停止：{}", active.join("、")),
            )
            .with_hint("请先在套件页停止服务，确认没有安装任务后再重试"));
        }
        next()
    }

    pub fn tail_logs(&self, id: &str, lines: usize) -> Vec<model::LogLine> {
        self.tail_logs_checked(id, lines).unwrap_or_default()
    }

    /// 只允许已注册服务或真实站点，不能把调用方的任意路径拼入日志目录。
    pub fn log_source_path(&self, id: &str) -> Result<std::path::PathBuf> {
        if let Some(site_id) = id.strip_prefix("site:") {
            if site_id.is_empty() || !site_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err(AppError::new("BAD_SITE_ID", "站点标识无效，无法读取日志"));
            }
            let site = sites::get(&self.store, site_id)?;
            let dir = if site.runtime.web_server == "apache" {
                self.paths.etc().join("apache").join("logs")
            } else {
                self.paths.logs().join("nginx")
            };
            return Ok(dir.join(format!("{site_id}.access.log")));
        }
        self.manager.log_path(id)
    }

    pub fn tail_logs_checked(&self, id: &str, lines: usize) -> Result<Vec<model::LogLine>> {
        let raw = if id.starts_with("site:") {
            services::read_log_tail(&self.log_source_path(id)?, lines)?
        } else {
            self.manager.tail_checked(id, lines)?
        };
        Ok(raw.into_iter().map(|line| model::LogLine { ts: None, line }).collect())
    }

    /* ---------- 服务栈 ---------- */

    pub fn list_stacks(&self) -> Result<Vec<model::Stack>> {
        stacks::list(&self.store)
    }

    pub fn save_stack(&self, input: model::StackInput) -> Result<model::Stack> {
        stacks::save(&self.store, input)
    }

    pub fn duplicate_stack(&self, id: &str, name: Option<String>) -> Result<model::Stack> {
        stacks::duplicate(&self.store, id, name)
    }

    pub fn delete_stack(&self, id: &str) -> Result<()> {
        stacks::delete(&self.store, id)
    }

    /// 一键启动整栈；单项失败不阻断其它项，结果逐项回报
    pub fn start_stack(&self, id: &str) -> Result<model::StackStartReport> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        stacks::start_with(&self.store, &self.paths, &self.manager, id, |sid| self.start_service(sid))
    }

    pub fn stop_stack(&self, id: &str) -> Result<model::StackStartReport> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        stacks::stop_with(&self.store, &self.paths, &self.manager, id, |sid| self.stop_service(sid))
    }

    pub fn bulk_start(&self, ids: &[String]) -> Result<bulk::BulkReport> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        bulk::start_many_with(&self.paths, &self.manager, ids, |id| self.start_service(id))
    }

    pub fn bulk_stop(&self, ids: &[String]) -> Result<bulk::BulkReport> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        bulk::stop_many_with(&self.paths, &self.manager, ids, |id| self.stop_service(id))
    }

    pub fn bulk_restart(&self, ids: &[String]) -> Result<bulk::BulkReport> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        bulk::restart_many_with(&self.paths, &self.manager, ids,
            |id| self.start_service(id), |id| self.stop_service(id))
    }

    /// 全部停止包含独立管理台；任何失败都保留结果及剩余 PID。
    pub fn stop_all_services(&self) -> Result<bulk::BulkReport> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock()
            .ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后停止全部服务"))?;
        let mut ids = self.manager.list_status().into_iter().map(|s| s.id).collect::<Vec<_>>();
        ids.sort();
        // 先停管理台，避免关闭数据库时仍有来自管理台的新请求。
        let has_adminer = self.manager.adminer.lock().is_some();
        let adminer_result = has_adminer.then(|| toolbox::adminer_stop(&self.manager));
        let mut report = self.bulk_stop(&ids)?;
        if let Some(adminer_result) = adminer_result {
            let id = "adminer-console".to_string();
            report.order.insert(0, id.clone());
            match adminer_result {
                Ok(()) => report.succeeded.push(id),
                Err(error) => report.failed.push(bulk::BulkFailure {
                    service_id: id, error: model::AppErrorInfo::from(error),
                }),
            }
        }
        ops::save_pidfile(&self.paths, &self.manager);
        Ok(report)
    }

    /// 占用了某端口的进程：本应用服务则优雅停止，外部进程则直接结束
    pub fn close_port(&self, port: u16) -> Result<ports::ClosePortOutcome> {
        let expected = self.scan_port_range(port, port)?.listeners;
        self.close_port_checked(port, &expected)
    }

    /// 从端口工具主动停止受管服务，同样要告知看门狗，避免刚停就被自动拉起。
    pub fn close_port_checked(&self, port: u16, expected: &[model::ListenerInfo]) -> Result<ports::ClosePortOutcome> {
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试"))?;
        let result = ports::close_port_checked(&self.store, &self.paths, &self.manager, port, expected);
        for id in expected.iter().filter_map(|row| row.service_id.as_deref()) {
            if self.manager.snapshot(id).is_some_and(|status| status.state == model::ServiceState::Stopped && status.pids.is_empty()) {
                self.manager.watchdog.note_user_stopped(id);
            }
        }
        result
    }

    /// 端口区间扫描（工具箱；单端口传 from == to）
    pub fn scan_port_range(&self, from: u16, to: u16) -> Result<model::PortRangeScan> {
        ports::scan_port_range(&self.manager, from, to)
    }

    /* ---------- 配置编辑 ---------- */
    pub fn preview_config_reset(&self, kind: &str) -> Result<cfgeditor::ConfigResetPreview> {
        let _operation = self.manager.lifecycle.lock();
        cfgeditor::preview_config_reset(&self.paths, &self.store, kind)
    }

    pub fn reset_config(&self, kind: &str, revision: &str) -> Result<cfgeditor::ConfigResetPreview> {
        let _operation = self.manager.lifecycle.lock();
        cfgeditor::reset_config(&self.paths, &self.store, kind, revision)
    }

    pub fn preview_backup(&self, name: &str) -> Result<paths::BackupPreview> {
        let _operation = self.manager.lifecycle.lock();
        paths::preview_backup(&self.paths.base, name).map_err(|error| AppError::io("预览配置备份", error))
    }

    pub fn restore_backup(&self, name: &str, revision: &str) -> Result<std::path::PathBuf> {
        let _operation = self.manager.lifecycle.lock();
        paths::restore_backup_checked(&self.paths.base, name, Some(revision)).map_err(|error| AppError::io("恢复配置备份", error))
    }

    pub fn validate_configs(&self, only: Option<&[String]>) -> Result<Vec<ops::ConfigCheck>> {
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| {
            AppError::new("CONFIG_CHECK_BUSY", "其他服务或配置操作正在进行，请稍后重试体检")
        })?;
        ops::validate_configs(&self.store, &self.paths, only)
    }

    pub fn validate_config(
        &self,
        kind: &str,
        content: &str,
    ) -> Result<cfgeditor::ConfigValidation> {
        let _operation = self.manager.lifecycle.lock();
        cfgeditor::validate_selected(&self.paths, &self.store, kind, content)
    }

    pub fn save_config(
        &self,
        kind: &str,
        content: &str,
        force: bool,
        expected: Option<&str>,
    ) -> Result<cfgeditor::ConfigValidation> {
        let _operation = self.manager.lifecycle.lock();
        cfgeditor::save_config_selected(&self.paths, &self.store, kind, content, force, expected)
    }

    pub fn rollback_config(
        &self,
        name: &str,
        kind: Option<&str>,
        expected: Option<&str>,
    ) -> Result<()> {
        let _operation = self.manager.lifecycle.lock();
        cfgeditor::rollback_config_selected(&self.paths, &self.store, name, kind, expected)
    }

    /* ---------- PHP 扩展 ---------- */

    /// 某版本 PHP 的扩展面板：磁盘上有什么 + php.ini 里开了什么
    pub fn php_extensions(&self, version: &str) -> Result<model::PhpExtensionView> {
        self.store
            .find_installed("php", Some(version))
            .ok_or_else(|| AppError::not_installed("PHP"))?;
        let extensions = phpext::scan_available(&self.paths, version)?;
        let toggles =
            phpext::INI_TOGGLES
                .iter()
                .map(|t| {
                    let v = phpext::read_ini_value(&self.paths, version, t.key)
                        .unwrap_or_else(|| if t.numeric { "0".into() } else { "Off".into() });
                    model::PhpIniToggle {
                        key: t.key.to_string(),
                        label: t.label.to_string(),
                        hint: t.hint.to_string(),
                        value: phpext::ini_truthy(&v),
                    }
                })
                .collect();
        Ok(model::PhpExtensionView {
            version: version.to_string(),
            ini_path: phpext::ini_path_for(&self.paths, version)
                .to_string_lossy()
                .to_string(),
            extensions,
            toggles,
        })
    }

    /// 启用/禁用扩展。改了 php.ini 只有重启 php-cgi 才生效，
    /// 所以这里顺带告诉前端「需不需要重启」，并在该版本正在运行时真正重启它——
    /// 否则用户勾了半天发现没效果，会以为功能坏了。
    pub fn set_php_extension(
        &self,
        version: &str,
        ext: &str,
        enable: bool,
    ) -> Result<model::PhpExtensionChange> {
        let _operation = self.manager.lifecycle.lock();
        self.store
            .find_installed("php", Some(version))
            .ok_or_else(|| AppError::not_installed("PHP"))?;
        let mut warnings = phpext::set_extension(&self.paths, version, ext, enable)?;
        let service_id = format!("php@{version}");
        let running = self
            .manager
            .list_status()
            .iter()
            .any(|s| s.id == service_id && matches!(s.state, model::ServiceState::Running));
        let mut restarted = false;
        if running {
            // 重启失败不该掩盖「配置已改成功」这个事实，忽略错误但记在告警里
            match self.restart_service(&service_id) {
                Ok(_) => restarted = true,
                Err(e) => warnings.push(format!("PHP {version} 重启失败：{}", e.message)),
            }
        }
        Ok(model::PhpExtensionChange {
            name: ext.to_string(),
            enabled: enable,
            warnings,
            needs_restart: running && !restarted,
        })
    }

    /// php.ini 快捷开关（display_errors / log_errors / opcache.enable）
    pub fn set_php_ini_toggle(&self, version: &str, key: &str, value: bool) -> Result<()> {
        let _operation = self.manager.lifecycle.lock();
        self.store
            .find_installed("php", Some(version))
            .ok_or_else(|| AppError::not_installed("PHP"))?;
        // 找到该键的声明方式（On/Off 还是 1/0）
        let numeric = phpext::INI_TOGGLES
            .iter()
            .find(|t| t.key == key)
            .map(|t| t.numeric)
            .ok_or_else(|| AppError::new("BAD_PHP_SETTING", "未知的 PHP 开关设置"))?;
        let v = match (numeric, value) {
            (true, true) => "1",
            (true, false) => "0",
            (false, true) => "On",
            (false, false) => "Off",
        };
        phpext::write_ini_value(&self.paths, version, key, v)
    }

    /* ---------- Xdebug ---------- */

    /// Xdebug 现状：构建指纹 / DLL 在不在 / 真加载了没有
    pub fn xdebug_status(&self, version: &str) -> Result<xdebug::XdebugStatus> {
        xdebug::status(&self.paths, version)
    }

    /// 一键配置 Xdebug。
    ///
    /// dll_path 给了就从本地装（用户自己下好的），否则按 PHP 构建指纹在线拉。
    /// 无论哪条路，最后都跑一次实测；加载失败就把 PHP 的原始告警带回去，
    /// 不把「写进 php.ini 了」当成「配置成功了」。
    pub async fn xdebug_setup(
        &self,
        input: xdebug::XdebugSetupInput,
    ) -> Result<model::XdebugSetupResult> {
        let version = input.version.clone();
        // 用户直接给了 DLL：跳过下载
        if let Some(p) = input.dll_path.as_deref().filter(|p| !p.trim().is_empty()) {
            xdebug::install_from_path(
                &self.paths,
                &version,
                std::path::Path::new(p),
                &input.mode,
                input.client_port,
            )?;
            let (loaded, loaded_version, warnings) = xdebug::verify(&self.paths, &version);
            self.restart_php_if_running(&version, &mut Vec::new());
            return Ok(model::XdebugSetupResult {
                version,
                installed: loaded,
                dll_path: Some(p.to_string()),
                loaded_version,
                warnings,
                manual_hint: None,
            });
        }

        // 在线下载：按构建指纹拼候选文件名，逐个试
        let build = xdebug::detect_build(&self.paths, &version)?;
        let candidates = build.xdebug_dll_candidates(xdebug::DEFAULT_XDEBUG_VERSION);
        let mut last_err: Option<String> = None;
        for cand in &candidates {
            let urls = xdebug::download_urls(cand);
            // 这些 DLL 没有随包提供 sha256（官方未公开校验文件），
            // 因此下载完必须靠 PHP 实测来兜底验真——装不上就会报出来。
            match self
                .downloader
                .download(
                    &format!("xdebug-{version}"),
                    &urls,
                    "",
                    0,
                    &self.paths,
                    &|e| (self.emit)(e),
                )
                .await
            {
                Ok(path) => {
                    xdebug::install_from_path(
                        &self.paths,
                        &version,
                        &path,
                        &input.mode,
                        input.client_port,
                    )?;
                    let (loaded, loaded_version, mut warnings) =
                        xdebug::verify(&self.paths, &version);
                    if loaded {
                        self.restart_php_if_running(&version, &mut warnings);
                        return Ok(model::XdebugSetupResult {
                            version,
                            installed: true,
                            dll_path: Some(path.to_string_lossy().to_string()),
                            loaded_version,
                            warnings,
                            manual_hint: None,
                        });
                    }
                    // 下载到了但加载失败：大概率是构建指纹不匹配
                    last_err = Some(format!(
                        "已下载 {cand}，但 PHP 加载失败：{}",
                        warnings.join("；")
                    ));
                }
                Err(e) => {
                    last_err = Some(format!("{cand} 下载失败：{}", e.message));
                }
            }
        }
        Ok(model::XdebugSetupResult {
            version,
            installed: false,
            dll_path: None,
            loaded_version: None,
            warnings: last_err.into_iter().collect(),
            manual_hint: Some(build.manual_hint(xdebug::DEFAULT_XDEBUG_VERSION)),
        })
    }

    /// PHP 在跑就重启，让 php.ini 改动立刻生效；失败只记告警不抛错
    fn restart_php_if_running(&self, version: &str, warnings: &mut Vec<String>) {
        let service_id = format!("php@{version}");
        let running = self
            .manager
            .list_status()
            .iter()
            .any(|s| s.id == service_id && matches!(s.state, model::ServiceState::Running));
        if !running {
            return;
        }
        let r = self.restart_service(&service_id);
        if let Err(e) = r {
            warnings.push(format!("PHP {version} 重启失败：{}", e.message));
        }
    }

    /// 启用/禁用 Xdebug（复用扩展开关，但会同步维护 [xdebug] 段）
    pub fn xdebug_toggle(
        &self,
        version: &str,
        enabled: bool,
        mode: &str,
        port: u16,
    ) -> Result<Vec<String>> {
        let ini_path = self.paths.php_ini(version);
        let ini =
            std::fs::read_to_string(&ini_path).map_err(|e| AppError::io("读取 php.ini", e))?;
        let mut next = phpext::apply_to_content(&ini, "xdebug", enabled);
        if enabled {
            let dll = self
                .paths
                .runtime_dir("php", version)
                .join("ext")
                .join(phpext::dll_file_name("xdebug"));
            let section = xdebug::render_xdebug_section(&dll.to_string_lossy(), mode, port);
            next = xdebug::upsert_xdebug_section(&next, &section);
        }
        write_with_backup(&ini_path, &next, &self.paths.backup())
            .map_err(|e| AppError::io("写入 php.ini", e))?;
        let mut warnings = Vec::new();
        self.restart_php_if_running(version, &mut warnings);
        Ok(warnings)
    }

    /* ---------- 服务看门狗 ---------- */

    pub fn watchdog_config(&self) -> watchdog::WatchdogConfig {
        watchdog::config_from_store(&self.store)
    }

    pub fn process_recovery_status(&self) -> ops::OrphanReport {
        self.manager.recovery.lock().clone()
    }

    pub fn recover_processes(&self) -> Result<ops::OrphanReport> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重新检查"))?;
        ensure_application_accepts_work()?;
        Ok(ops::sweep_orphans(&self.paths, &self.store, &self.manager))
    }

    pub fn watchdog_status(&self) -> watchdog::WatchdogStatus {
        let cfg = self.watchdog_config();
        // 启停正在执行时只返回已有记录，避免把中间态当成崩溃。
        if let Some(_operation) = self.manager.lifecycle.try_lock() {
            for status in self.manager.list_status() {
                self.manager.watchdog.observe(&status.id, status.state, &cfg);
            }
        }
        self.manager.watchdog.status(&cfg)
    }

    pub fn watchdog_set_enabled(&self, on: bool) -> Result<()> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        self.store
            .set_setting("watchdogEnabled", if on { "true" } else { "false" })?;
        Ok(())
    }

    pub fn watchdog_reset(&self, id: &str) -> Result<()> {
        let _activity = paths::DataDirActivity::shared(&self.paths.base)?;
        let _operation = self.manager.lifecycle.try_lock().ok_or_else(|| AppError::new("SERVICE_BUSY", "服务正在操作，请稍后重试恢复"))?;
        ensure_application_accepts_work()?;
        let cfg = self.watchdog_config();
        if !cfg.enabled { return Err(AppError::new("WATCHDOG_DISABLED", "请先开启服务崩溃后自动重启")); }
        let status = self.manager.snapshot(id).ok_or_else(|| AppError::new("UNKNOWN_SERVICE", "服务已移除，请刷新列表"))?;
        if self.manager.is_busy(id) { return Err(AppError::new("SERVICE_BUSY", "服务仍在运行或正在操作，无需重试恢复")); }
        if let Some(site_id) = applications::site_id(id) {
            let site = sites::get(&self.store, site_id)?;
            if site.runtime.application.is_none() || sites::derive_status(&self.paths, &site) != "running" {
                return Err(AppError::new("SITE_STOPPED", "站点已停止或取消托管，请从站点页面启动"));
            }
        }
        self.manager.watchdog.observe(id, status.state, &cfg);
        if !self.manager.watchdog.exhausted(id, &cfg) {
            return Err(AppError::new("WATCHDOG_NOT_EXHAUSTED", "仅达到恢复上限的服务需要重试；主动停止的服务请手动启动"));
        }
        self.manager.watchdog.reset(id);
        Ok(())
    }

    /// 看门狗单轮检查：把所有「非用户停止、且确实不在跑」的受监控服务拉起来。
    ///
    /// 返回本轮实际尝试过的 (服务 id, 是否成功)。调用方（desktop 的背景线程）
    /// 自己决定多久跑一次；间隔由 `WatchdogConfig::interval_sec` 提供。
    pub fn watchdog_tick(&self) -> Vec<(String, bool)> {
        let Ok(_activity) = paths::DataDirActivity::shared(&self.paths.base) else { return Vec::new(); };
        let _operation = self.manager.lifecycle.lock();
        if ensure_application_accepts_work().is_err() { return Vec::new(); }
        let cfg = self.watchdog_config();
        if !cfg.enabled {
            return Vec::new();
        }
        let statuses = self.manager.list_status();
        let mut acted = Vec::new();
        for st in statuses {
            if let Some(id) = applications::site_id(&st.id) {
                if sites::get(&self.store, id).ok().is_none_or(|site| site.runtime.application.is_none() || sites::derive_status(&self.paths, &site) != "running") {
                    self.manager.watchdog.note_user_stopped(&st.id);
                    continue;
                }
            }
            self.manager.watchdog.observe(&st.id, st.state, &cfg);
            if self.manager.is_busy(&st.id) {
                continue;
            }
            if !self.manager.watchdog.should_restart(&st.id, &cfg) {
                continue;
            }
            // 只重启「曾经成功跑起来过」的服务：note_started 只在启动成功时调用，
            // 所以没被 note 过的服务压根不在 entries 里，should_restart 会返回 false。
            let ok = ops::start_service_for_watchdog(&self.store, &self.paths, &self.manager, &st.id).is_ok();
            self.manager.watchdog.note_restart(&st.id, ok, &cfg);
            if ok {
                ops::save_pidfile(&self.paths, &self.manager);
            }
            acted.push((st.id.clone(), ok));
        }
        acted
    }
}

#[cfg(test)]
mod dep_tests {
    #[test]
    fn background_shutdown_waits_for_real_monitor_and_restores_after_timeout() {
        let output = platform::command(std::env::current_exe().unwrap())
            .args(["--exact", "dep_tests::background_shutdown_probe", "--nocapture"])
            .env("NSB_BACKGROUND_SHUTDOWN_PROBE", "1").output().unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn background_shutdown_probe() {
        if std::env::var_os("NSB_BACKGROUND_SHUTDOWN_PROBE").is_none() { return; }
        use super::*;
        use std::time::{Duration, Instant};
        let temp = tempfile::tempdir().unwrap();
        let state = CoreState::init(Some(temp.path().to_path_buf()), Arc::new(|_| {})).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (accepted, connected) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline); std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("fixture listener: {e}"),
                }
            };
            accepted.send(()).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(5));
            drop(socket);
        });
        state.store.save_cert_monitor(&model::CertMonitor {
            id: "fixture".into(), host: "127.0.0.1".into(), port, name: "fixture".into(),
            state: "idle".into(), issuer: String::new(), expires_at: None, last_checked: None,
            last_error: String::new(), notification_error: String::new(), created_at: 1, updated_at: 1,
        }).unwrap();
        let worker_state = state.clone();
        let worker = std::thread::spawn(move || certmonitor::check(&worker_state, "fixture"));
        connected.recv_timeout(Duration::from_secs(5)).unwrap();
        let error = AuxiliaryShutdown::prepare_with_timeout(Duration::from_millis(40)).err().unwrap();
        assert_eq!(error.code, "AUXILIARY_STOP_FAILED");
        assert!(error.message.contains("证书监控（fixture）"));
        assert!(state.store.get_cert_monitor("fixture").unwrap().unwrap().last_checked.is_none());
        drop(BackgroundWork::begin("失败后恢复入口").unwrap());
        assert!(!AUXILIARY_SHUTDOWN.load(std::sync::atomic::Ordering::Acquire));

        let shutdown = std::thread::spawn(|| AuxiliaryShutdown::prepare_with_timeout(Duration::from_secs(5)));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !BACKGROUND_TASKS.state.lock().paused {
            assert!(Instant::now() < deadline); std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!shutdown.is_finished());
        assert_eq!(certauto::run_once(&state, "missing").unwrap_err().code, "APP_BUSY");
        assert_eq!(certmonitor::check(&state, "fixture").unwrap_err().code, "APP_BUSY");
        assert_eq!(backup_job::run_backup_now(&state.store, &state.paths).unwrap_err().code, "APP_BUSY");
        let file = temp.path().join("blocked.json");
        assert_eq!(transfer::export_to(&state.store, &file).unwrap_err().code, "APP_BUSY");
        assert!(!file.exists());
        assert!(certauto::tick(&state).is_empty());
        certmonitor::tick_all(&state);
        release.send(()).unwrap(); server.join().unwrap();
        let guard = shutdown.join().unwrap().unwrap();
        assert_eq!(worker.join().unwrap().unwrap().state, "error");
        assert!(state.store.get_cert_monitor("fixture").unwrap().unwrap().last_checked.is_some());
        assert!(BackgroundWork::begin("暂停中").is_err());
        drop(guard);
        transfer::export_to(&state.store, &file).unwrap();
        backup_job::run_backup_now(&state.store, &state.paths).unwrap();
        let mut guard = AuxiliaryShutdown::prepare().unwrap();
        guard.commit(); drop(guard);
        assert!(BackgroundWork::begin("提交退出后").is_err());
    }

    #[test]
    fn background_registration_and_pause_cannot_miss_each_other() {
        use super::*;
        // 独立 registry 不影响并行执行的其它回归。
        for _ in 0..64 {
            let registry = Arc::new(BackgroundTasks::default());
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let other = registry.clone(); let sync = barrier.clone();
            let thread = std::thread::spawn(move || { sync.wait(); other.begin("正在派生的任务".into()) });
            barrier.wait();
            registry.state.lock().paused = true;
            let work = thread.join().unwrap();
            match work {
                Ok(work) => {
                    assert!(registry.wait_until_idle(std::time::Duration::ZERO).is_err());
                    drop(work);
                }
                Err(error) => assert_eq!(error.code, "APP_BUSY"),
            }
            registry.wait_until_idle(std::time::Duration::ZERO).unwrap();
        }
    }

    /// 从磁盘直接读指定清单文件。
    ///
    /// 不能用 `Installer::bundled()`：它按编译目标 OS 选清单
    /// （Windows→win、其它→mac），Linux CI 上读到的是 mac 清单，
    /// 断言 win 清单的内容必然失败。检查对象是仓库里的文件本身，
    /// 与运行平台无关，所以显式读文件。
    fn manifest_from_disk(name: &str) -> crate::model::Manifest {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../manifest/");
        let raw = std::fs::read_to_string(format!("{dir}{name}")).expect("清单文件应在仓库内");
        serde_json::from_str(&raw).expect("清单 JSON 必须合法")
    }

    /// 清单里声明的依赖必须真的能被读到。
    ///
    /// 这个测试专门守住一个真实踩过的坑：`requires` 写在顶层时 Rust 侧读不到
    /// （要么在 run.requires，要么在顶层 requires，两处都得查）。
    /// 当时是 manifest 写了、界面不提示，很难发现。
    #[test]
    fn manifest_dependencies_are_readable() {
        // 依赖声明目前只写在 win 清单（mac 清单尚无可声明依赖的套件组合）
        let manifest = manifest_from_disk("packages.win.json");
        let found: Vec<(String, Vec<String>)> = manifest
            .packages
            .iter()
            .filter_map(|p| {
                let mut deps: Vec<String> = Vec::new();
                if let Some(run) = &p.run {
                    deps.extend(run.requires.iter().cloned());
                }
                deps.extend(p.requires.iter().cloned());
                if deps.is_empty() {
                    None
                } else {
                    deps.sort();
                    deps.dedup();
                    Some((p.id.clone(), deps))
                }
            })
            .collect();
        assert!(
            !found.is_empty(),
            "清单里应至少有一个套件声明了依赖（否则说明字段又写错位置了）"
        );
        // 抽查几个语义上必然有依赖的
        let ids: Vec<&str> = found.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"composer"), "composer 依赖 php：{found:?}");
        assert!(ids.contains(&"tomcat"), "tomcat 依赖 JDK：{found:?}");
    }

    /// 依赖不能指向清单里不存在的套件 id，否则用户永远装不上。
    /// 两份清单都查 —— 将来给 mac 清单补声明时同样受保护。
    #[test]
    fn declared_dependencies_exist_in_manifest() {
        for name in ["packages.win.json", "packages.mac.json"] {
            declared_dependencies_exist_in(name);
        }
    }

    fn declared_dependencies_exist_in(name: &str) {
        let manifest = manifest_from_disk(name);
        let all: std::collections::HashSet<&str> =
            manifest.packages.iter().map(|p| p.id.as_str()).collect();
        for p in &manifest.packages {
            let mut deps: Vec<String> = Vec::new();
            if let Some(run) = &p.run {
                deps.extend(run.requires.iter().cloned());
            }
            deps.extend(p.requires.iter().cloned());
            for d in deps {
                assert!(all.contains(d.as_str()), "{} 声明了不存在的依赖 {d}", p.id);
            }
        }
    }
}
