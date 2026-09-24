//! 工具箱扩展：Ollama 模型管理 + Adminer 数据库管理台（php 内置服务器托管）。

use crate::error::{AppError, Result};
use crate::paths::Paths;
use crate::store::Store;
use parking_lot::Mutex;
use serde::Serialize;
use std::io::{BufRead, BufReader};
use std::sync::OnceLock;

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
    let out = std::process::Command::new(exe)
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
    let out = std::process::Command::new(exe)
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
    let mut child = std::process::Command::new(exe)
        .args(["pull", name])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AppError::io("启动模型拉取", e))?;
    // 排水管：不读的话子进程写满管道缓冲会卡死
    if let Some(so) = child.stdout.take() {
        std::thread::spawn(move || {
            for _ in BufReader::new(so).lines() {}
        });
    }
    if let Some(se) = child.stderr.take() {
        std::thread::spawn(move || {
            for _ in BufReader::new(se).lines() {}
        });
    }
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/* ================= Adminer 数据库管理台 ================= */

static ADMINER: OnceLock<Mutex<Option<std::process::Child>>> = OnceLock::new();
pub const ADMINER_PORT: u16 = 8991;

fn adminer_child() -> &'static Mutex<Option<std::process::Child>> {
    ADMINER.get_or_init(|| Mutex::new(None))
}

/// 用已安装的 PHP 内置服务器托管 Adminer；返回 (端口, 入口文件名)。
/// 前缀约定：http://127.0.0.1:{port}/{entry 文件名}
pub fn adminer_start(
    store: &Store,
    paths: &Paths,
    installer: &crate::install::Installer,
) -> Result<(u16, String)> {
    {
        let mut guard = adminer_child().lock();
        if let Some(c) = guard.as_mut() {
            if matches!(c.try_wait(), Ok(None)) {
                return Ok((ADMINER_PORT, entry_of(store, installer)?));
            }
        }
        *guard = None;
    }

    // PHP 主程序：php 包的 runtime 目录里与 php-cgi.exe 同级的 php.exe
    let php = store
        .find_installed("php", None)
        .ok_or_else(|| AppError::not_installed("php").with_hint("先到「套件 / 服务」安装任意版本的 PHP"))?;
    let php_exe = paths.runtime_dir("php", &php.version).join("php.exe");
    if !php_exe.exists() {
        return Err(AppError::new(
            "PHP_EXE_MISSING",
            format!("未找到 {}", php_exe.display()),
        ));
    }

    let adm = store.find_installed("adminer", None).ok_or_else(|| {
        AppError::not_installed("adminer").with_hint("先到「套件 / 服务」安装 Adminer")
    })?;
    let adm_entry = installer
        .template_for("adminer")
        .map(|t| t.entry)
        .unwrap_or_else(|| "adminer.php".to_string());
    let adm_dir = paths.runtime_dir("adminer", &adm.version);

    let mut child = std::process::Command::new(php_exe)
        .args([
            "-S",
            &format!("127.0.0.1:{ADMINER_PORT}"),
            "-t",
            &adm_dir.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AppError::io("启动 Adminer 管理台", e))?;
    if let Some(so) = child.stdout.take() {
        std::thread::spawn(move || {
            for _ in BufReader::new(so).lines() {}
        });
    }
    if let Some(se) = child.stderr.take() {
        std::thread::spawn(move || {
            for _ in BufReader::new(se).lines() {}
        });
    }
    *adminer_child().lock() = Some(child);

    // entry 可能带版本号（adminer-6.1.0-en.php），取实际文件名
    let file = std::fs::read_dir(&adm_dir)
        .ok()
        .and_then(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .find(|n| n.starts_with("adminer") && n.ends_with(".php"))
        })
        .unwrap_or(adm_entry);
    Ok((ADMINER_PORT, file))
}

fn entry_of(store: &Store, installer: &crate::install::Installer) -> Result<String> {
    // 只校验已安装；入口文件名取清单模板
    store.find_installed("adminer", None).ok_or_else(|| {
        AppError::not_installed("adminer").with_hint("先到「套件 / 服务」安装 Adminer")
    })?;
    Ok(installer
        .template_for("adminer")
        .map(|t| t.entry)
        .unwrap_or_else(|| "adminer.php".to_string()))
}

pub fn adminer_stop() -> Result<()> {
    let mut guard = adminer_child().lock();
    if let Some(mut c) = guard.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
    Ok(())
}
