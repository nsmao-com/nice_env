//! core：NiceEnv 全部业务逻辑（无 Tauri 依赖，可独立测试/无头运行）。

pub mod acme;
pub mod bulk;
pub mod certauto;
pub mod certdeploy;
pub mod certmonitor;
pub mod certs;
pub mod cfgeditor;
pub mod configgen;
pub mod cron;
pub mod dbadmin;
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
pub mod toolbox;
pub mod tunnel;
use paths::write_with_backup;
pub mod backup_job;
pub mod mcp;
pub mod pathenv;
pub mod phpext;
pub mod ports;
pub mod proxy;
pub mod scanner;
pub mod serde_proxy;
pub mod services;
pub mod sites;
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
    DownloadProgress(DownloadProgress),
    HostsDenied,
    /// 数据库备份 / 还原进度
    DbBackup(model::DbBackupProgress),
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
            Event::DownloadProgress(_) => "download://progress",
            Event::HostsDenied => "hosts://denied",
            Event::DbBackup(_) => "db://backup",
            Event::CertAuto { .. } => "certauto://status",
            Event::CertMonitorAlert { .. } => "certmonitor://alert",
        }
    }
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Event::DownloadProgress(p) => serde_json::to_value(p).unwrap_or_default(),
            Event::HostsDenied => serde_json::json!({}),
            Event::DbBackup(p) => serde_json::to_value(p).unwrap_or_default(),
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

pub struct CoreState {
    pub paths: paths::Paths,
    pub store: store::Store,
    pub manager: Arc<ServiceManager>,
    pub downloader: Arc<Downloader>,
    pub installer: install::Installer,
    pub emit: EventSink,
    /// 服务看门狗：意外退出后自动拉起（用户主动停止的除外）
    pub watchdog: Arc<watchdog::Watchdog>,
}

impl CoreState {
    pub fn init(base: Option<std::path::PathBuf>, emit: EventSink) -> Result<Arc<Self>> {
        let base = paths::Paths::resolve(base);
        let paths = paths::Paths::new(base);
        paths.ensure_dirs().map_err(|e| {
            error::AppError::io("初始化数据目录", e)
                .with_hint("数据目录不可写，可在环境变量 NSB_HOME 指定其它位置")
        })?;
        let store = store::Store::open(paths.db())?;
        // 服务栈内置预设（首次运行写入；用户改过的不动）
        let _ = stacks::ensure_presets(&store);
        let manager = Arc::new(ServiceManager::new());
        ops::register_services(&paths, &store, &manager);
        generic::register_services(&paths, &store, &manager);
        // 上次会话崩溃/被强杀时留下的进程：启动即清理，否则它们占着端口让服务起不来
        let orphans = ops::sweep_orphans(&paths, &manager);
        // effective() 要在 paths 被 move 进 state 之前用掉
        let installer = install::Installer::effective(&paths);
        let state = Arc::new(Self {
            paths,
            store,
            manager,
            downloader: Arc::new(Downloader::new()),
            installer,
            emit,
            watchdog: Arc::new(watchdog::Watchdog::new()),
        });
        if !orphans.adopted.is_empty() || !orphans.killed.is_empty() {
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
            (state.emit)(Event::DownloadProgress(model::DownloadProgress {
                task_id: "orphans".into(),
                received: 0,
                total: 0,
                speed_bps: 0,
                eta_sec: 0.0,
                state: "orphans-resolved".into(),
                error: Some(parts.join("；")),
            }));
        }
        // 自动备份调度（off/daily/weekly；线程内自己判断档位）
        backup_job::spawn_scheduler(state.paths.clone());
        // 计划任务调度（应用级 cron，应用退出即停）
        crate::cron::spawn_scheduler(state.paths.clone());
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
            self.watchdog.forget(&sid);
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
        if job.id.trim().is_empty() {
            job.id = crate::cron::new_id();
        }
        if job.command.trim().is_empty() {
            return Err(AppError::new("CRON_BAD_JOB", "命令不能为空"));
        }
        if job.interval_min <= 0 {
            job.interval_min = 5;
        }
        self.store.upsert_cron_job(&job)
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

    pub fn tunnel_start(&self, port: u16) -> Result<model::TunnelInfo> {
        let exe = toolbox::resolve_exe(&self.store, &self.paths, &self.installer, "cloudflared")?;
        crate::tunnel::start(&exe, port)
    }

    pub fn tunnel_list(&self) -> Vec<model::TunnelInfo> {
        crate::tunnel::list()
    }

    pub fn tunnel_stop(&self, id: &str) -> Result<()> {
        crate::tunnel::stop(id)
    }

    pub fn ollama_models(&self) -> Result<Vec<toolbox::OllamaModelRow>> {
        toolbox::ollama_models(&self.store, &self.paths, &self.installer)
    }

    pub fn ollama_delete(&self, name: &str) -> Result<()> {
        toolbox::ollama_delete(&self.store, &self.paths, &self.installer, name)
    }

    pub fn ollama_pull(&self, name: &str) -> Result<()> {
        toolbox::ollama_pull(&self.store, &self.paths, &self.installer, name)
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
        let _operation = self.manager.lifecycle.lock();
        let (version, client) = dbadmin::selected_client(self, version)?;
        operation(&version, &client)
    }

    /// use_existing 只更新本机连接凭据；false 同时修改实例中现有的本地 root 账号。
    pub fn set_mysql_password(
        &self,
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
            version,
            use_existing.then(|| password.to_string()),
        )?;
        let key = dbadmin::password_key(&version);
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
                    .verify_data_dir(&self.paths.mysql_data_dir(&version))
                    .is_ok()
                {
                    return Ok(());
                }
                if client
                    .verify_data_dir(&self.paths.mysql_data_dir(&version))
                    .is_ok()
                {
                    self.store.set_setting(&key, &client.root_password)?;
                }
                return Err(error.with_hint(
                    "请确认实例是否仍在运行；若连接中断，请用当前实例密码更新本机连接记录",
                ));
            }
            new_client.verify_data_dir(&self.paths.mysql_data_dir(&version))?;
        }
        Ok(())
    }

    pub fn migrate_list_source(
        &self,
        host: String,
        port: u16,
        user: String,
        password: String,
        version: Option<&str>,
    ) -> Result<Vec<dbmigrate::SourceDb>> {
        let _operation = self.manager.lifecycle.lock();
        let bin = self.installed_mysql_bin_dir(version)?;
        dbmigrate::list_source_databases(
            &bin,
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
    ) -> Result<dbmigrate::ImportReport> {
        let src = dbmigrate::SourceConn {
            host,
            port,
            user,
            password,
        };
        self.with_mysql(version, |version, client| {
            let target = dbbackup::ConnInfo {
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
    fn installed_mysql_bin_dir(&self, version: Option<&str>) -> Result<std::path::PathBuf> {
        let package = match version {
            Some(version) => self.store.find_installed("mysql", Some(version)),
            None => crate::ops::installed_by_choice(&self.store, "mysql"),
        }
        .ok_or_else(|| AppError::not_installed("MySQL"))?;
        Ok(std::path::Path::new(&package.install_path)
            .join(crate::ops::mysql_root_name(&package.version))
            .join("bin"))
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

    pub fn start_service(&self, id: &str) -> Result<()> {
        let _operation = self.manager.lifecycle.lock();
        let r = ops::start_service(&self.store, &self.paths, &self.manager, id);
        // 记录托管 pid：崩溃后下次启动靠它找回残留进程
        ops::save_pidfile(&self.paths, &self.manager);
        // 只有真的起来了才算「用户希望它运行」，失败时不该纳入看门狗监控
        if r.is_ok() {
            self.watchdog.note_started(id);
        }
        r
    }

    pub fn stop_service(&self, id: &str) -> Result<()> {
        let _operation = self.manager.lifecycle.lock();
        let r = ops::stop_service(&self.store, &self.paths, &self.manager, id);
        ops::save_pidfile(&self.paths, &self.manager);
        // 用户主动停止 → 标记，看门狗不得再拉起它（否则点了停止又被拉起来，
        // 那个体验比不自动重启还糟）
        if r.is_ok() {
            self.watchdog.note_user_stopped(id);
        }
        r
    }

    /// 将运行时、配置、服务数据、证书和本地数据库迁移到新目录。
    /// 迁移期间必须没有安装任务；受管服务会先全部优雅停止，桌面端随后重启进程。
    pub fn migrate_data_dir(&self, target: &std::path::Path) -> Result<paths::DataDirMigration> {
        let _operation = self.manager.lifecycle.lock();
        if self.downloader.has_tasks() {
            return Err(AppError::new(
                "PACKAGE_BUSY",
                "当前仍有套件安装或卸载任务，请等待完成后再迁移",
            ));
        }
        ops::stop_all(&self.store, &self.paths, &self.manager);
        let active = self
            .manager
            .list_status()
            .into_iter()
            .filter(|status| {
                matches!(
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
        self.store.checkpoint()?;
        paths::copy_data_dir(&self.paths.base, target)
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
        stacks::start(&self.store, &self.paths, &self.manager, id)
    }

    pub fn stop_stack(&self, id: &str) -> Result<model::StackStartReport> {
        stacks::stop(&self.store, &self.paths, &self.manager, id)
    }

    /// 占用了某端口的进程：本应用服务则优雅停止，外部进程则直接结束
    pub fn close_port(&self, port: u16) -> Result<ports::ClosePortOutcome> {
        ports::close_port(&self.store, &self.paths, &self.manager, port)
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
            match ops::stop_service(&self.store, &self.paths, &self.manager, &service_id).and_then(
                |_| ops::start_service(&self.store, &self.paths, &self.manager, &service_id),
            ) {
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
        let r = ops::stop_service(&self.store, &self.paths, &self.manager, &service_id)
            .and_then(|_| ops::start_service(&self.store, &self.paths, &self.manager, &service_id));
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

    pub fn watchdog_status(&self) -> watchdog::WatchdogStatus {
        self.watchdog.status(&self.watchdog_config())
    }

    pub fn watchdog_set_enabled(&self, on: bool) -> Result<()> {
        self.store
            .set_setting("watchdogEnabled", if on { "true" } else { "false" })?;
        Ok(())
    }

    pub fn watchdog_reset(&self, id: &str) {
        self.watchdog.reset(id);
    }

    /// 看门狗单轮检查：把所有「非用户停止、且确实不在跑」的受监控服务拉起来。
    ///
    /// 返回本轮实际尝试过的 (服务 id, 是否成功)。调用方（desktop 的背景线程）
    /// 自己决定多久跑一次；间隔由 `WatchdogConfig::interval_sec` 提供。
    pub fn watchdog_tick(&self) -> Vec<(String, bool)> {
        let _operation = self.manager.lifecycle.lock();
        let cfg = self.watchdog_config();
        if !cfg.enabled {
            return Vec::new();
        }
        let statuses = self.manager.list_status();
        let mut acted = Vec::new();
        for st in statuses {
            if matches!(
                st.state,
                model::ServiceState::Running | model::ServiceState::Starting
            ) {
                continue;
            }
            if !self.watchdog.should_restart(&st.id, &cfg) {
                continue;
            }
            // 只重启「曾经成功跑起来过」的服务：note_started 只在启动成功时调用，
            // 所以没被 note 过的服务压根不在 entries 里，should_restart 会返回 false。
            let ok = ops::start_service(&self.store, &self.paths, &self.manager, &st.id).is_ok();
            self.watchdog.note_restart(&st.id, ok, &cfg);
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
