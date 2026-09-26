//! 工具箱扩展：Ollama 模型管理 + Adminer 数据库管理台（php 内置服务器托管）。

use crate::error::{AppError, Result};
use crate::paths::Paths;
use crate::store::Store;
use serde::Serialize;
use std::io::{BufRead, BufReader};

/// 解析已安装包的主程序完整路径
pub fn resolve_exe(
    store: &Store,
    paths: &Paths,
    installer: &crate::install::Installer,
    id: &str,
) -> Result<std::path::PathBuf> {
    let inst = store.find_installed(id, None).ok_or_else(|| {
        AppError::not_installed(id).with_hint("先到「套件 / 服务」安装后再使用本功能")
    })?;
    let entry = installer
        .template_for(id)
        .ok_or_else(|| AppError::new("PACKAGE_NOT_FOUND", format!("清单里没有 {id}")))?
        .entry;
    Ok(paths
        .runtime_dir(id, &inst.version)
        .join(crate::install::entry_relative_path(&entry)))
}

/* ================= Ollama 模型管理 ================= */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaModelRow {
    pub name: String,
    pub digest: String,
    pub size: String,
    pub modified: String,
}

/// `ollama list`：跳过表头，按 NAME / ID / SIZE(两段) / MODIFIED 解析
pub fn ollama_models(
    store: &Store,
    paths: &Paths,
    installer: &crate::install::Installer,
) -> Result<Vec<OllamaModelRow>> {
    let exe = resolve_exe(store, paths, installer, "ollama")?;
    let out = platform::command(exe)
        .arg("list")
        .output()
        .map_err(|e| AppError::io("查询模型列表", e))?;
    if !out.status.success() {
        return Err(AppError::new(
            "OLLAMA_LIST_FAILED",
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut rows = Vec::new();
    for line in text.lines().skip(1) {
        let mut it = line.split_whitespace();
        let (Some(name), Some(digest), Some(size_num), Some(size_unit)) =
            (it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        let modified = it.collect::<Vec<_>>().join(" ");
        rows.push(OllamaModelRow {
            name: name.to_string(),
            digest: digest.to_string(),
            size: format!("{size_num} {size_unit}"),
            modified,
        });
    }
    Ok(rows)
}

pub fn ollama_delete(
    store: &Store,
    paths: &Paths,
    installer: &crate::install::Installer,
    name: &str,
) -> Result<()> {
    let exe = resolve_exe(store, paths, installer, "ollama")?;
    let out = platform::command(exe)
        .args(["rm", name])
        .output()
        .map_err(|e| AppError::io("删除模型", e))?;
    if !out.status.success() {
        return Err(AppError::new(
            "OLLAMA_RM_FAILED",
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(())
}

/// 后台拉取模型：`ollama pull` 自带进度输出，这里丢进独立线程排水管，
/// 主进程立即返回；拉取结果用「刷新列表」确认。
pub fn ollama_pull(
    store: &Store,
    paths: &Paths,
    installer: &crate::install::Installer,
    name: &str,
) -> Result<()> {
    let exe = resolve_exe(store, paths, installer, "ollama")?;
    let mut child = platform::command(exe)
        .args(["pull", name])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AppError::io("启动模型拉取", e))?;
    // 排水管：不读的话子进程写满管道缓冲会卡死
    if let Some(so) = child.stdout.take() {
        std::thread::spawn(move || for _ in BufReader::new(so).lines() {});
    }
    if let Some(se) = child.stderr.take() {
        std::thread::spawn(move || for _ in BufReader::new(se).lines() {});
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/* ================= Adminer 数据库管理台 ================= */

pub const ADMINER_PORT: u16 = 8991;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminerStatus {
    pub port: u16,
    pub file: String,
    pub url: String,
    pub php_version: String,
    pub adminer_version: String,
}

pub(crate) struct AdminerRuntime {
    child: std::process::Child,
    group: platform::ProcessGroup,
    status: AdminerStatus,
}

impl Drop for AdminerRuntime {
    fn drop(&mut self) {
        let _ = self.group.terminate(true);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn adminer_status(manager: &crate::services::ServiceManager) -> Result<Option<AdminerStatus>> {
    let _operation = manager.lifecycle.lock();
    let mut guard = manager.adminer.lock();
    if let Some(runtime) = guard.as_mut() {
        match runtime
            .child
            .try_wait()
            .map_err(|e| AppError::io("读取管理台状态", e))?
        {
            None => return Ok(Some(runtime.status.clone())),
            Some(_) => {
                guard.take();
            }
        }
    }
    Ok(None)
}

pub(crate) fn adminer_pid(manager: &crate::services::ServiceManager) -> Option<u32> {
    manager.adminer.lock().as_mut().and_then(|runtime| {
        matches!(runtime.child.try_wait(), Ok(None)).then(|| runtime.child.id())
    })
}

/// 与服务启停、卸载共用生命周期锁；实际入口随本次进程保存。
pub fn adminer_start(
    store: &Store,
    paths: &Paths,
    installer: &crate::install::Installer,
    manager: &crate::services::ServiceManager,
) -> Result<AdminerStatus> {
    adminer_start_on_port(store, paths, installer, manager, ADMINER_PORT)
}

pub(crate) fn adminer_start_on_port(
    store: &Store,
    paths: &Paths,
    installer: &crate::install::Installer,
    manager: &crate::services::ServiceManager,
    port: u16,
) -> Result<AdminerStatus> {
    use std::io::Read;
    use std::path::PathBuf;
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let _operation = manager.lifecycle.lock();
    if manager
        .shutting_down
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return Err(AppError::new(
            "SHUTTING_DOWN",
            "应用正在退出，无法启动管理台",
        ));
    }
    if let Some(status) = adminer_status(manager)? {
        return Ok(status);
    }
    let php = crate::ops::installed_by_choice(store, "php")
        .ok_or_else(|| AppError::not_installed("PHP").with_hint("先到套件页安装 PHP"))?;
    let adm = crate::ops::installed_by_choice(store, "adminer")
        .ok_or_else(|| AppError::not_installed("Adminer").with_hint("先到套件页安装 Adminer"))?;
    let php_entry = PathBuf::from(&php.install_path).join(crate::install::entry_relative_path(
        &installer.installed_entry(&php).entry,
    ));
    let bin = php_entry
        .parent()
        .ok_or_else(|| AppError::new("BROKEN_INSTALL", "PHP 入口无效"))?;
    let php_exe = [
        bin.join(crate::ops::exe_name("php")),
        bin.join("../bin/php"),
        PathBuf::from(&php.install_path).join(crate::ops::exe_name("php")),
        PathBuf::from(&php.install_path).join("bin/php"),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .ok_or_else(|| {
        AppError::new(
            "PHP_EXE_MISSING",
            "所选 PHP 缺少 CLI 程序，请在套件页修复安装",
        )
    })?
    .canonicalize()?;
    let ini = paths.php_ini(&php.version);
    if !ini.is_file() {
        return Err(AppError::new(
            "PHP_CONFIG_MISSING",
            "所选 PHP 缺少配置文件，请在工具箱重置该版本的 PHP 配置后重试",
        ));
    }
    let root = PathBuf::from(&adm.install_path).canonicalize()?;
    let entry = root.join(crate::install::entry_relative_path(
        &installer.installed_entry(&adm).entry,
    ));
    if !entry.is_file() {
        return Err(AppError::new(
            "ADMINER_ENTRY_MISSING",
            "所选 Adminer 的入口文件不存在，请修复该版本安装",
        ));
    }
    let entry = entry.canonicalize()?;
    if !entry.starts_with(&root) || entry.extension().is_none_or(|e| e != "php") {
        return Err(AppError::new(
            "ADMINER_ENTRY_INVALID",
            "Adminer 入口必须是安装目录内的 PHP 文件",
        ));
    }
    let dir = entry
        .parent()
        .ok_or_else(|| AppError::new("ADMINER_ENTRY_INVALID", "Adminer 入口无效"))?;
    // PHP 内置服务器不能加载 Windows canonicalize 产生的 verbatim 文档根目录。
    #[cfg(windows)]
    let dir = {
        let text = dir.to_string_lossy();
        PathBuf::from(if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            text.strip_prefix(r"\\?\").unwrap_or(&text).to_string()
        })
    };
    let file = entry
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            AppError::port_conflict(port, None)
        } else {
            AppError::io("预检管理台端口", e)
        }
    })?;
    let port = listener.local_addr()?.port();
    let mut url = reqwest::Url::parse(&format!("http://127.0.0.1:{port}/"))
        .map_err(|e| AppError::internal("管理台地址无效", e.to_string()))?;
    url.set_path(&format!("/{file}"));
    let status = AdminerStatus {
        port,
        file,
        url: url.to_string(),
        php_version: php.version,
        adminer_version: adm.version,
    };
    let log_path = paths.service_log("adminer");
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 每次启动独立日志，读取错误时不会混入上一次启动的内容。
    let log = std::fs::File::create(&log_path)?;
    let mut command = platform::command(&php_exe);
    command
        .arg("-c")
        .arg(&ini)
        .args(["-S", &format!("127.0.0.1:{port}"), "-t"])
        .arg(&dir)
        .current_dir(&dir)
        .env("PHPRC", &ini)
        .env("PHP_INI_SCAN_DIR", "")
        .env_remove("PHP_CLI_SERVER_WORKERS")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| platform::spawn_pre_exec());
        }
    }
    let mut group = platform::ProcessGroup::new_detached(false)?;
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(800))
        .build()
        .map_err(|e| AppError::internal("准备管理台检查", e.to_string()))?;
    drop(listener);
    let mut child = command
        .spawn()
        .map_err(|e| AppError::io("启动 Adminer", e))?;
    if let Err(error) = group.attach(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error.into());
    }
    let mut runtime = AdminerRuntime {
        child,
        group,
        status: status.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if runtime.child.try_wait()?.is_some() {
            break;
        }
        if let Ok(response) = client.get(&status.url).send() {
            if response.status().is_success() {
                let mut body = String::new();
                if response.take(1024 * 1024).read_to_string(&mut body).is_ok()
                    && body.to_ascii_lowercase().contains("adminer")
                    && body.contains("<form")
                    && runtime.child.try_wait()?.is_none()
                    && crate::ports::listeners()?
                        .iter()
                        .any(|(p, pid)| *p == port && *pid == runtime.child.id())
                {
                    *manager.adminer.lock() = Some(runtime);
                    return Ok(status);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(runtime);
    let detail = std::fs::File::open(&log_path)
        .ok()
        .map(|f| {
            let mut s = String::new();
            let _ = f.take(16 * 1024).read_to_string(&mut s);
            s
        })
        .unwrap_or_default();
    Err(
        AppError::new("ADMINER_START_FAILED", "Adminer 页面未就绪，启动已取消")
            .with_hint(format!(
                "检查 PHP 配置、数据库扩展与日志：{}",
                log_path.display()
            ))
            .with_detail(detail),
    )
}

pub fn adminer_stop(manager: &crate::services::ServiceManager) -> Result<()> {
    let _operation = manager.lifecycle.lock();
    let mut guard = manager.adminer.lock();
    if let Some(runtime) = guard.as_mut() {
        if runtime.child.try_wait()?.is_none() {
            runtime.group.terminate(true)?;
            if runtime.child.try_wait()?.is_none() {
                runtime.child.kill()?;
            }
        }
        runtime
            .child
            .wait()
            .map_err(|e| AppError::io("回收 Adminer 进程", e))?;
        guard.take();
    }
    Ok(())
}
