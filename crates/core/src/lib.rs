//! core：NiceServBay 全部业务逻辑（无 Tauri 依赖，可独立测试/无头运行）。

pub mod bulk;
pub mod certs;
pub mod cfgeditor;
pub mod configgen;
pub mod dbadmin;
pub mod dbbackup;
pub mod diagnostics;
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
use paths::write_with_backup;
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
pub mod transfer;
pub mod tls;
pub mod toolmirror;
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
}

impl Event {
    pub fn channel(&self) -> &'static str {
        match self {
            Event::DownloadProgress(_) => "download://progress",
            Event::HostsDenied => "hosts://denied",
            Event::DbBackup(_) => "db://backup",
        }
    }
    pub fn payload(&self) -> serde_json::Value {
        match self {
            Event::DownloadProgress(p) => serde_json::to_value(p).unwrap_or_default(),
            Event::HostsDenied => serde_json::json!({}),
            Event::DbBackup(p) => serde_json::to_value(p).unwrap_or_default(),
        }
    }
    pub fn progress(task_id: &str, received: u64, total: u64, speed: u64, eta: f64, state: &str) -> Self {
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
        paths
            .ensure_dirs()
            .map_err(|e| error::AppError::io("初始化数据目录", e).with_hint("数据目录不可写，可在环境变量 NSB_HOME 指定其它位置"))?;
        let store = store::Store::open(paths.db())?;
        // 服务栈内置预设（首次运行写入；用户改过的不动）
        let _ = stacks::ensure_presets(&store);
        let manager = Arc::new(ServiceManager::new());
        ops::register_services(&paths, &store, &manager);
        generic::register_services(&paths, &store, &manager);
        // 上次会话崩溃/被强杀时留下的进程：启动即清理，否则它们占着端口让服务起不来
        let orphans = ops::sweep_orphans(&paths, &manager);
        let state = Arc::new(Self {
            paths,
            store,
            manager,
            downloader: Arc::new(Downloader::new()),
            installer: install::Installer::bundled(),
            emit,
            watchdog: Arc::new(watchdog::Watchdog::new()),
        });
        if !orphans.is_empty() {
            let detail = orphans
                .iter()
                .map(|(sid, pid)| format!("{sid}(pid {pid})"))
                .collect::<Vec<_>>()
                .join(", ");
            (state.emit)(Event::DownloadProgress(model::DownloadProgress {
                task_id: "orphans".into(),
                received: 0,
                total: 0,
                speed_bps: 0,
                eta_sec: 0.0,
                state: "orphans-cleaned".into(),
                error: Some(detail),
            }));
        }
        Ok(state)
    }

    /// 供外部调用的便捷（下载进度等）
    pub fn emit_event(&self, e: Event) {
        (self.emit)(e);
    }
}

/// hosts 写入失败的提示事件（带当前应写条目数）
pub fn emit_hosts_denied(store: &store::Store, _paths: &paths::Paths) {
    let n = hosts::managed_entries(store).len();
    let e = Event::HostsDenied;
    let _ = n;
    // desktop 侧 listen 后 toast 提示
    let _ = e;
}

/* ================= 门面 API（desktop 命令与 smoke 测试共用） ================= */

impl CoreState {
    pub fn list_packages(&self) -> Result<Vec<model::PackageView>> {
        let installed = self.store.list_installed()?;
        // 每个「id+版本」一条（含全部可选版本）；前端按 id 聚合成服务条
        let views: Vec<model::PackageView> = self
            .installer
            .manifest
            .packages
            .iter()
            .map(|m| {
                let install = installed
                    .iter()
                    .find(|i| i.id == m.id && i.version == m.version)
                    .cloned();
                let available_versions = self
                    .installer
                    .manifest
                    .packages
                    .iter()
                    .filter(|p| p.id == m.id)
                    .map(|p| p.version.clone())
                    .collect();
                model::PackageView {
                    manifest: m.clone(),
                    install,
                    available_versions,
                    active: false,
                }
            })
            .collect();
        // 标记「使用中版本」：单实例服务（nginx/apache/mysql/redis/postgresql/mongodb/mihomo）
        // 由 activeXxxVersion 设置决定，缺省为最高版本
        let mut active_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for id in ["nginx", "apache", "mysql", "redis", "postgresql", "mongodb", "mihomo"] {
            if let Some(p) = ops::installed_by_choice(&self.store, id) {
                active_map.insert(id.to_string(), p.version);
            }
        }
        let mut views = views;
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
            .install(key, &self.paths, &self.store, &self.downloader, &|e| (self.emit)(e))
            .await?;
        ops::register_services(&self.paths, &self.store, &self.manager);
        generic::register_services(&self.paths, &self.store, &self.manager);
        // 装完即让命令可用（开关开着才真正写盘；失败不阻断安装）
        let _ = pathenv::sync(&self.store, &self.paths, &self.installer.manifest);
        Ok(installed)
    }

    pub fn uninstall_package(&self, key: &str) -> Result<()> {
        let r = self.installer.uninstall(key, &self.paths, &self.store, &self.manager);
        // 卸载后目录已不存在，必须把托管条目摘掉，否则 PATH 里留死路径
        if r.is_ok() {
            let _ = pathenv::sync(&self.store, &self.paths, &self.installer.manifest);
        }
        r
    }

    /// 切换「使用中版本」；顺带把 PATH 里的对应目录指到新版本
    pub fn set_active_version(&self, id: &str, version: &str) -> Result<()> {
        ops::set_active_version(&self.store, id, version)?;
        let _ = pathenv::sync(&self.store, &self.paths, &self.installer.manifest);
        Ok(())
    }

    /* ---------- 环境变量（PATH 注入） ---------- */

    /// 环境变量注入的完整状态（含每个已安装包可注入的命令）
    pub fn pathenv_status(&self) -> model::PathEnvStatus {
        pathenv::status(&self.store, &self.installer.manifest)
    }

    /// 开/关总开关
    pub fn pathenv_set_enabled(&self, enabled: bool) -> Result<model::PathEnvStatus> {
        pathenv::set_enabled(&self.store, &self.paths, &self.installer.manifest, enabled)
    }

    /// 设置要注入 PATH 的包集合
    pub fn pathenv_set_selected(&self, ids: Vec<String>) -> Result<model::PathEnvStatus> {
        pathenv::set_selected(&self.store, &self.paths, &self.installer.manifest, &ids)
    }

    /// 强制重新应用（修漂移：用户手动改过 PATH 或换了版本）
    pub fn pathenv_reapply(&self) -> Result<model::PathEnvStatus> {
        pathenv::apply(&self.store, &self.paths, &self.installer.manifest)
    }

    /// 服务列表 + 前置依赖信息。
    ///
    /// 依赖来自清单的 `run.requires`，在这里补齐而不是塞进 ServiceManager：
    /// - manager 只关心进程，不该知道清单；
    /// - 判断「依赖是否已安装」需要 store，而 manager 拿不到 store。
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
        let template = self.installer.template_for(id).ok_or_else(|| {
            AppError::new("PACKAGE_NOT_FOUND", format!("清单里没有套件 {id}"))
        })?;
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
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            match self.version_catalog(&id, force).await {
                Ok(c) => out.push(c),
                Err(e) => out.push(model::VersionCatalog {
                    id: id.clone(),
                    remote: vec![],
                    online: false,
                    cached_at: None,
                    error: Some(e.message),
                }),
            }
        }
        out
    }

    pub fn start_service(&self, id: &str) -> Result<()> {
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
        let r = ops::stop_service(&self.store, &self.paths, &self.manager, id);
        ops::save_pidfile(&self.paths, &self.manager);
        // 用户主动停止 → 标记，看门狗不得再拉起它（否则点了停止又被拉起来，
        // 那个体验比不自动重启还糟）
        if r.is_ok() {
            self.watchdog.note_user_stopped(id);
        }
        r
    }

    pub fn tail_logs(&self, id: &str, lines: usize) -> Vec<model::LogLine> {
        let from_ring = self.manager.tail(id, lines);
        let mapped: Vec<model::LogLine> = from_ring
            .into_iter()
            .map(|line| model::LogLine { ts: None, line })
            .collect();
        if !mapped.is_empty() {
            return mapped;
        }
        // 未运行：从文件读
        let path = self.paths.service_log(id);
        if let Ok(content) = std::fs::read_to_string(path) {
            let all: Vec<&str> = content.lines().collect();
            return all
                .into_iter()
                .rev()
                .take(lines)
                .map(|l| model::LogLine { ts: None, line: l.to_string() })
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
        }
        Vec::new()
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

    /* ---------- PHP 扩展 ---------- */

    /// 某版本 PHP 的扩展面板：磁盘上有什么 + php.ini 里开了什么
    pub fn php_extensions(&self, version: &str) -> Result<model::PhpExtensionView> {
        let extensions = phpext::scan_available(&self.paths, version)?;
        let toggles = phpext::INI_TOGGLES
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
            match ops::stop_service(&self.store, &self.paths, &self.manager, &service_id)
                .and_then(|_| ops::start_service(&self.store, &self.paths, &self.manager, &service_id))
            {
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
        // 找到该键的声明方式（On/Off 还是 1/0）
        let numeric = phpext::INI_TOGGLES
            .iter()
            .find(|t| t.key == key)
            .map(|t| t.numeric)
            .unwrap_or(false);
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
        let r = ops::stop_service(&self.store, &self.paths, &self.manager, &service_id).and_then(|_| {
            ops::start_service(&self.store, &self.paths, &self.manager, &service_id)
        });
        if let Err(e) = r {
            warnings.push(format!("PHP {version} 重启失败：{}", e.message));
        }
    }

    /// 启用/禁用 Xdebug（复用扩展开关，但会同步维护 [xdebug] 段）
    pub fn xdebug_toggle(&self, version: &str, enabled: bool, mode: &str, port: u16) -> Result<Vec<String>> {
        let ini_path = self.paths.php_ini(version);
        let ini = std::fs::read_to_string(&ini_path).map_err(|e| AppError::io("读取 php.ini", e))?;
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
        self.store.set_setting("watchdogEnabled", if on { "true" } else { "false" })?;
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
        let cfg = self.watchdog_config();
        if !cfg.enabled {
            return Vec::new();
        }
        let statuses = self.manager.list_status();
        let mut acted = Vec::new();
        for st in statuses {
            if matches!(st.state, model::ServiceState::Running | model::ServiceState::Starting) {
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
    /// 清单里声明的依赖必须真的能被读到。
    ///
    /// 这个测试专门守住一个真实踩过的坑：`requires` 写在顶层时 Rust 侧读不到
    /// （要么在 run.requires，要么在顶层 requires，两处都得查）。
    /// 当时是 manifest 写了、界面不提示，很难发现。
    #[test]
    fn manifest_dependencies_are_readable() {
        let manifest = crate::install::Installer::bundled();
        let found: Vec<(String, Vec<String>)> = manifest
            .manifest
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

    /// 依赖不能指向清单里不存在的套件 id，否则用户永远装不上
    #[test]
    fn declared_dependencies_exist_in_manifest() {
        let manifest = crate::install::Installer::bundled();
        let all: std::collections::HashSet<&str> = manifest
            .manifest
            .packages
            .iter()
            .map(|p| p.id.as_str())
            .collect();
        for p in &manifest.manifest.packages {
            let mut deps: Vec<String> = Vec::new();
            if let Some(run) = &p.run {
                deps.extend(run.requires.iter().cloned());
            }
            deps.extend(p.requires.iter().cloned());
            for d in deps {
                assert!(
                    all.contains(d.as_str()),
                    "{} 声明了不存在的依赖 {d}",
                    p.id
                );
            }
        }
    }
}
