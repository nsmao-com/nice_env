//! 人话错误：code + message + hint（下一步建议）+ detail（技术细节）。
//! 所有用户路径上的错误都必须经由 AppError，不允许裸 unwrap/panic。

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 端口冲突时随错误一起带给前端：端口号与占用进程 pid，
    /// 前端据此直接提供「结束占用进程并重试」，不必再去猜是哪个端口
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// 占用进程名（人话展示）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
}

pub type Result<T> = std::result::Result<T, AppError>;

impl AppError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            hint: None,
            detail: None,
            port: None,
            pid: None,
            holder: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// 补上占用进程 pid（端口冲突错误携带给前端，用于一键结束）
    pub fn with_pid(mut self, pid: u32) -> Self {
        self.pid = Some(pid);
        self
    }

    /// 端口被占用 → 给出占用人
    pub fn port_conflict(port: u16, holder: Option<&str>) -> Self {
        let mut e = Self::new(
            "PORT_IN_USE",
            format!("端口 {port} 已被占用"),
        );
        if let Some(h) = holder {
            e.message = format!("端口 {port} 已被 {h} 占用");
            e.holder = Some(h.to_string());
        }
        e.port = Some(port);
        e.hint = Some(
            "可用「结束占用进程并重试」直接收掉它，或到「工具箱 → 端口工具」处理；也可以改用其它端口（设置 → 端口）"
                .to_string(),
        );
        e
    }

    pub fn not_installed(what: &str) -> Self {
        Self::new("NOT_INSTALLED", format!("{what} 尚未安装"))
            .with_hint(format!("先到「套件 / 服务」页安装 {what}"))
    }

    pub fn download(url: &str, detail: impl Into<String>) -> Self {
        Self::new("DOWNLOAD_FAILED", format!("下载失败：{url}"))
            .with_hint("检查网络或到「设置 → 下载镜像源」切换镜像后重试；已下载部分会自动续传")
            .with_detail(detail)
    }

    pub fn io(context: &str, e: std::io::Error) -> Self {
        Self::new("IO_ERROR", format!("{context}：{e}"))
            .with_hint("请检查磁盘空间与目录权限；数据目录可在「设置」中查看")
    }

    pub fn internal(context: &str, e: impl Into<String>) -> Self {
        Self::new("INTERNAL", format!("{context}：{}", e.into()))
            .with_hint("请重试；若持续出现请查看日志页并把最后 80 行反馈给开发者")
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::io("文件操作失败", e)
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::internal("数据库操作失败", e.to_string())
    }
}

impl From<platform::PlatformError> for AppError {
    fn from(e: platform::PlatformError) -> Self {
        let msg = e.to_string();
        if msg.contains("管理员") || msg.contains("权限") {
            AppError::new("PERMISSION_DENIED", msg.clone())
                .with_hint(
                    "Windows：右键应用「以管理员身份运行」；macOS：首次写入 hosts 需输入密码。\
                     你的项目文件不受影响。",
                )
                .with_detail(msg)
        } else {
            AppError::internal("平台操作失败", msg)
        }
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::download("请求", e.to_string())
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
