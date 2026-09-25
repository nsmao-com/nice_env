//! 工具链镜像源切换：Composer / npm / pip 等**包管理器**的镜像。
//!
//! 与「套件下载镜像」不是一回事：那个管的是本应用自己下载套件走哪条路；
//! 这里管的是**用户在项目里执行 `composer install` / `npm install` 时**走哪条路。
//!
//! 国内直连 packagist.org / registry.npmjs.org 经常几十 KB/s 甚至超时，
//! 换镜像能快一个数量级。phpStudy、ServBay 都提供这个开关。
//!
//! 三种做法各有利弊，所以都给用户：
//! 1. **全局配置文件**（composer config -g、npm config set）—— 一劳永逸，
//!    但会改用户机器的全局状态；
//! 2. **项目级 .npmrc / composer.json** —— 只影响该项目，适合只给某个项目提速；
//! 3. **只读探测**：显示当前生效的源，不改任何东西。
//!
//! 因为会动全局配置，所以每个写操作都：改前读旧值 → 记录到本应用设置 →
//! 提供「恢复官方源」。绝不静默改完就走。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};

/// 支持配置的包管理器
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolManager {
    Composer,
    Npm,
    Pip,
}

impl ToolManager {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "composer" => Self::Composer,
            "npm" => Self::Npm,
            "pip" => Self::Pip,
            _ => return None,
        })
    }
    pub fn id(&self) -> &'static str {
        match self {
            Self::Composer => "composer",
            Self::Npm => "npm",
            Self::Pip => "pip",
        }
    }
    /// 镜像清单里对应的键前缀
    fn setting_key(&self) -> &'static str {
        match self {
            Self::Composer => "composerRegistry",
            Self::Npm => "npmRegistry",
            Self::Pip => "pipIndexUrl",
        }
    }
}

/// 一个可选的镜像源
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorOption {
    pub id: String,
    pub label: String,
    /// 该镜像实际写入的地址
    pub url: String,
    /// 备注（运营商、覆盖范围等）
    pub note: String,
    /// 是否是官方源（用于「恢复默认」）
    pub official: bool,
}

/// 各包管理器的候选镜像。
///
/// 只收录长期稳定运行的公共镜像。镜像这东西会停服，
/// 所以每一项都附带说明，并且用户始终可以填自定义地址。
pub fn options_for(m: ToolManager) -> Vec<MirrorOption> {
    let mk = |id: &str, label: &str, url: &str, note: &str, official: bool| MirrorOption {
        id: id.to_string(),
        label: label.to_string(),
        url: url.to_string(),
        note: note.to_string(),
        official,
    };
    match m {
        ToolManager::Composer => vec![
            mk(
                "official",
                "Packagist 官方",
                "https://repo.packagist.org",
                "官方源，国内直连较慢",
                true,
            ),
            mk(
                "aliyun",
                "阿里云",
                "https://mirrors.aliyun.com/composer/",
                "覆盖全、长期稳定，最常用",
                false,
            ),
            mk(
                "tencent",
                "腾讯云",
                "https://mirrors.cloud.tencent.com/composer/",
                "国内速度快",
                false,
            ),
            mk(
                "huawei",
                "华为云",
                "https://repo.huaweicloud.com/repository/php/",
                "国内速度快",
                false,
            ),
            mk(
                "cnpkg",
                "cnpkg.org",
                "https://mirrors.cnpkg.org/composer/",
                "社区维护的全量镜像",
                false,
            ),
        ],
        ToolManager::Npm => vec![
            mk(
                "official",
                "npm 官方",
                "https://registry.npmjs.org",
                "官方源，国内直连较慢",
                true,
            ),
            mk(
                "npmmirror",
                "淘宝 npmmirror",
                "https://registry.npmmirror.com",
                "同步频率高，国内最常用",
                false,
            ),
            mk(
                "tencent",
                "腾讯云",
                "https://mirrors.cloud.tencent.com/npm/",
                "国内速度快",
                false,
            ),
            mk(
                "huawei",
                "华为云",
                "https://repo.huaweicloud.com/repository/npm/",
                "国内速度快",
                false,
            ),
        ],
        ToolManager::Pip => vec![
            mk(
                "official",
                "PyPI 官方",
                "https://pypi.org/simple",
                "官方源，国内直连较慢",
                true,
            ),
            mk(
                "tsinghua",
                "清华 TUNA",
                "https://pypi.tuna.tsinghua.edu.cn/simple",
                "覆盖全、稳定，最常用",
                false,
            ),
            mk(
                "aliyun",
                "阿里云",
                "https://mirrors.aliyun.com/pypi/simple/",
                "国内速度快",
                false,
            ),
            mk(
                "ustc",
                "中科大",
                "https://mirrors.ustc.edu.cn/pypi/simple/",
                "国内速度快",
                false,
            ),
        ],
    }
}

/// 某个管理器的当前状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolMirrorStatus {
    pub manager: String,
    /// 当前生效的地址（读不到就是 None）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// 匹配到的预设 id（自定义地址时为空）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<String>,
    /// 该管理器的可执行文件是否可用（没装就配不了）
    pub available: bool,
    /// 配置文件位置（让用户知道改了哪个文件）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    pub options: Vec<MirrorOption>,
}

/// 判断某地址是否匹配预设
pub fn match_option(m: ToolManager, url: &str) -> Option<String> {
    let norm = url.trim().trim_end_matches('/').to_ascii_lowercase();
    options_for(m)
        .into_iter()
        .find(|o| o.url.trim_end_matches('/').to_ascii_lowercase() == norm)
        .map(|o| o.id)
}

/* ================= npm ================= */

fn npmrc_path() -> Option<PathBuf> {
    // npm 的用户级配置固定在这里
    dirs::home_dir().map(|h| h.join(".npmrc"))
}

/// 解析 .npmrc 里的 registry=
pub fn parse_npmrc(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('#') || t.starts_with(';') || t.is_empty() {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim() == "registry" {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// 改写 .npmrc 的 registry（保留其它配置与注释）
pub fn apply_npmrc(content: &str, url: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    for line in content.lines() {
        let t = line.trim();
        let is_registry = !t.starts_with('#')
            && !t.starts_with(';')
            && t.split_once('=')
                .map(|(k, _)| k.trim() == "registry")
                .unwrap_or(false);
        if is_registry {
            if !replaced {
                out.push(format!("registry={url}"));
                replaced = true;
            }
            // 重复的 registry 行丢掉——npm 只认第一条，留着只会让人困惑
            continue;
        }
        out.push(line.to_string());
    }
    if !replaced {
        if !out.is_empty() && !out.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
            out.push(String::new());
        }
        out.push(format!("registry={url}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/* ================= composer ================= */

/// Composer 的用户级配置文件位置（按平台）
pub fn composer_config_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    if cfg!(target_os = "windows") {
        // Windows 下是 %APPDATA%\Composer\config.json
        dirs::config_dir().map(|d| d.join("Composer").join("config.json"))
    } else {
        Some(home.join(".composer").join("config.json"))
    }
}

/// 从 composer 的 config.json 里读 repositories.packagist 或 config.repo.packagist
pub fn parse_composer_registry(content: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    // composer 关掉默认 repo 后，镜像写在 repositories 里
    if let Some(repos) = v.get("repositories").and_then(|r| r.as_object()) {
        if let Some(p) = repos.get("packagist") {
            // 形如 { "type": "composer", "url": "..." }
            if let Some(u) = p.get("url").and_then(|u| u.as_str()) {
                return Some(u.to_string());
            }
            // 或 { "packagist.org": false } 表示禁用了官方源
            if p.as_bool() == Some(false) {
                return None;
            }
        }
    }
    // 也有些情况写在 config.repo.packagist
    v.get("config")
        .and_then(|c| c.get("repo"))
        .and_then(|r| r.get("packagist"))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string())
}

/// 改写 composer 的 config.json：设置镜像源。
///
/// 兼容的写法是同时写两处：
/// - `repositories.packagist` = { type: composer, url: <镜像> }
/// - `config.secure-http` = true（保持默认的安全要求）
pub fn apply_composer_registry(content: &str, url: &str) -> Result<String> {
    // 空文件按空对象处理
    let trimmed = content.trim();
    let mut v: serde_json::Value = if trimmed.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(trimmed).map_err(|e| {
            AppError::new("BAD_COMPOSER_CONFIG", "Composer 配置文件不是合法 JSON")
                .with_hint("可以手动删除该文件后重试，Composer 会重建它")
                .with_detail(e.to_string())
        })?
    };
    if !v.is_object() {
        return Err(AppError::new(
            "BAD_COMPOSER_CONFIG",
            "Composer 配置的顶层不是对象",
        ));
    }
    let obj = v.as_object_mut().unwrap();
    obj.insert(
        "repositories".to_string(),
        serde_json::json!({
            "packagist": { "type": "composer", "url": url }
        }),
    );
    serde_json::to_string_pretty(&v)
        .map_err(|e| AppError::internal("序列化 Composer 配置", e.to_string()))
}

/// 把镜像源还原成官方（删掉 repositories 里的 packagist 覆盖）
pub fn reset_composer_registry(content: &str) -> Result<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Ok("{}".to_string());
    }
    let mut v: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
        AppError::new("BAD_COMPOSER_CONFIG", "Composer 配置文件不是合法 JSON")
            .with_detail(e.to_string())
    })?;
    if let Some(obj) = v.as_object_mut() {
        if let Some(repos) = obj.get_mut("repositories").and_then(|r| r.as_object_mut()) {
            repos.remove("packagist");
            // 清空后不留空对象，免得 composer 抱怨
            if repos.is_empty() {
                obj.remove("repositories");
            }
        }
    }
    serde_json::to_string_pretty(&v)
        .map_err(|e| AppError::internal("序列化 Composer 配置", e.to_string()))
}

/* ================= pip ================= */

/// pip 的配置文件位置（每个平台不同）
pub fn pip_config_path() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        dirs::config_dir().map(|d| d.join("pip").join("pip.ini"))
    } else {
        dirs::config_dir().map(|d| d.join("pip").join("pip.conf"))
    }
}

/// 解析 pip 配置里的 index-url
pub fn parse_pip_ini(content: &str) -> Option<String> {
    let mut in_global = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_global = t.eq_ignore_ascii_case("[global]");
            continue;
        }
        if !in_global || t.is_empty() || t.starts_with('#') || t.starts_with(';') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim().eq_ignore_ascii_case("index-url") {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// 改写 pip 配置的 index-url（保留其它项）
pub fn apply_pip_ini(content: &str, url: &str) -> String {
    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let mut has_global = false;
    let mut replaced = false;
    let mut global_start = None;

    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with('[') {
            if t.eq_ignore_ascii_case("[global]") {
                has_global = true;
                global_start = Some(i);
            }
            continue;
        }
        if has_global
            && t.split_once('=')
                .map(|(k, _)| k.trim().eq_ignore_ascii_case("index-url"))
                .unwrap_or(false)
        {
            lines[i] = format!("index-url = {url}");
            replaced = true;
            break;
        }
    }

    if !replaced {
        if let Some(start) = global_start {
            // 插到 [global] 段末尾
            let mut insert_at = lines.len();
            for i in start + 1..lines.len() {
                if lines[i].trim().starts_with('[') {
                    insert_at = i;
                    break;
                }
            }
            lines.insert(insert_at, format!("index-url = {url}"));
        } else {
            if !lines.is_empty() && !lines.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                lines.push(String::new());
            }
            lines.push("[global]".to_string());
            lines.push(format!("index-url = {url}"));
        }
    }
    let mut s = lines.join("\n");
    s.push('\n');
    s
}

/* ================= 读写封装 ================= */

/// 读取某个管理器的当前状态
pub fn status(m: ToolManager, store: &crate::store::Store) -> ToolMirrorStatus {
    let (current, config_path) = match m {
        ToolManager::Npm => {
            let p = npmrc_path();
            let cur = p
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|c| parse_npmrc(&c));
            (cur, p)
        }
        ToolManager::Composer => {
            let p = composer_config_path();
            let cur = p
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|c| parse_composer_registry(&c));
            (cur, p)
        }
        ToolManager::Pip => {
            let p = pip_config_path();
            let cur = p
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|c| parse_pip_ini(&c));
            (cur, p)
        }
    };
    let matched = current.as_deref().and_then(|u| match_option(m, u));
    // 「可用」= 配置文件所在目录存在，或已经能从设置里读到值
    let available = config_path.is_some() || store.get_setting(m.setting_key()).is_some();
    ToolMirrorStatus {
        manager: m.id().to_string(),
        current,
        matched,
        available,
        config_path: config_path.map(|p| p.to_string_lossy().to_string()),
        options: options_for(m),
    }
}

/// 切换镜像源。写前记录旧值到设置，便于「恢复」。
pub fn set_mirror(m: ToolManager, url: &str) -> Result<()> {
    let url = url.trim();
    if url.is_empty() {
        return Err(AppError::new("EMPTY_URL", "镜像地址不能为空"));
    }
    // 基本形态校验：必须是 http(s) 且像域名
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(AppError::new(
            "BAD_URL",
            "镜像地址需要以 http:// 或 https:// 开头",
        ));
    }
    match m {
        ToolManager::Npm => {
            let path = npmrc_path()
                .ok_or_else(|| AppError::new("NO_HOME", "无法定位用户主目录，不能写 .npmrc"))?;
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let next = apply_npmrc(&existing, url);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).ok();
            }
            std::fs::write(&path, next).map_err(|e| AppError::io("写入 .npmrc", e))?;
        }
        ToolManager::Composer => {
            let path = composer_config_path()
                .ok_or_else(|| AppError::new("NO_APPDATA", "无法定位 Composer 配置目录"))?;
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let next = apply_composer_registry(&existing, url)?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| AppError::io("创建 Composer 配置目录", e))?;
            }
            std::fs::write(&path, next).map_err(|e| AppError::io("写入 Composer 配置", e))?;
        }
        ToolManager::Pip => {
            let path = pip_config_path()
                .ok_or_else(|| AppError::new("NO_APPDATA", "无法定位 pip 配置目录"))?;
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let next = apply_pip_ini(&existing, url);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| AppError::io("创建 pip 配置目录", e))?;
            }
            std::fs::write(&path, next).map_err(|e| AppError::io("写入 pip 配置", e))?;
        }
    }
    Ok(())
}

/// 恢复官方源
pub fn reset_mirror(m: ToolManager) -> Result<()> {
    match m {
        ToolManager::Npm => {
            let path =
                npmrc_path().ok_or_else(|| AppError::new("NO_HOME", "无法定位用户主目录"))?;
            if !path.is_file() {
                return Ok(());
            }
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let next = apply_npmrc(&existing, "https://registry.npmjs.org");
            std::fs::write(&path, next).map_err(|e| AppError::io("写入 .npmrc", e))?;
        }
        ToolManager::Composer => {
            let path = composer_config_path()
                .ok_or_else(|| AppError::new("NO_APPDATA", "无法定位 Composer 配置目录"))?;
            if !path.is_file() {
                return Ok(());
            }
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let next = reset_composer_registry(&existing)?;
            std::fs::write(&path, next).map_err(|e| AppError::io("写入 Composer 配置", e))?;
        }
        ToolManager::Pip => {
            let path = pip_config_path()
                .ok_or_else(|| AppError::new("NO_APPDATA", "无法定位 pip 配置目录"))?;
            if !path.is_file() {
                return Ok(());
            }
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let next = apply_pip_ini(&existing, "https://pypi.org/simple");
            std::fs::write(&path, next).map_err(|e| AppError::io("写入 pip 配置", e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manager_parsing() {
        assert_eq!(ToolManager::parse("npm"), Some(ToolManager::Npm));
        assert_eq!(ToolManager::parse("composer"), Some(ToolManager::Composer));
        assert_eq!(ToolManager::parse("pip"), Some(ToolManager::Pip));
        assert_eq!(ToolManager::parse("cargo"), None);
        assert_eq!(ToolManager::parse(""), None);
    }

    #[test]
    fn every_manager_has_an_official_option() {
        for m in [ToolManager::Composer, ToolManager::Npm, ToolManager::Pip] {
            let opts = options_for(m);
            assert!(!opts.is_empty(), "{:?} 应有候选镜像", m);
            assert_eq!(
                opts.iter().filter(|o| o.official).count(),
                1,
                "{:?} 应恰好有一个官方源",
                m
            );
        }
    }

    #[test]
    fn all_options_are_https() {
        // 包管理器源必须走 https，否则依赖会被中间人替换
        for m in [ToolManager::Composer, ToolManager::Npm, ToolManager::Pip] {
            for o in options_for(m) {
                assert!(
                    o.url.starts_with("https://"),
                    "{} 不是 https：{}",
                    o.id,
                    o.url
                );
            }
        }
    }

    #[test]
    fn match_option_normalizes_trailing_slash_and_case() {
        assert_eq!(
            match_option(ToolManager::Npm, "https://registry.npmjs.org/"),
            Some("official".to_string())
        );
        assert_eq!(
            match_option(ToolManager::Composer, "https://mirrors.aliyun.com/composer"),
            Some("aliyun".to_string())
        );
    }

    #[test]
    fn match_option_returns_none_for_custom() {
        assert_eq!(
            match_option(ToolManager::Npm, "https://my.internal.registry"),
            None
        );
    }

    #[test]
    fn parse_npmrc_finds_registry() {
        let c = "# comment\nregistry=https://registry.npmmirror.com\nother=1\n";
        assert_eq!(
            parse_npmrc(c).as_deref(),
            Some("https://registry.npmmirror.com")
        );
    }

    #[test]
    fn parse_npmrc_ignores_commented_registry() {
        let c = "# registry=https://old.example\nregistry=https://new.example\n";
        assert_eq!(parse_npmrc(c).as_deref(), Some("https://new.example"));
    }

    #[test]
    fn parse_npmrc_returns_none_when_absent() {
        assert_eq!(parse_npmrc("strict-ssl=false\n"), None);
        assert_eq!(parse_npmrc(""), None);
    }

    #[test]
    fn apply_npmrc_replaces_in_place_and_keeps_other_lines() {
        let src = "strict-ssl=false\nregistry=https://registry.npmjs.org\naudit=false\n";
        let out = apply_npmrc(src, "https://registry.npmmirror.com");
        assert!(out.contains("registry=https://registry.npmmirror.com"));
        assert!(!out.contains("registry.npmjs.org"));
        assert!(out.contains("strict-ssl=false"), "其它配置必须保留：{out}");
        assert!(out.contains("audit=false"));
    }

    #[test]
    fn apply_npmrc_appends_when_missing() {
        let out = apply_npmrc("strict-ssl=false\n", "https://registry.npmmirror.com");
        assert!(out.contains("registry=https://registry.npmmirror.com"));
        assert!(out.contains("strict-ssl=false"));
    }

    #[test]
    fn apply_npmrc_drops_duplicate_registry_lines() {
        // npm 只认第一条，重复行留着只会让人困惑
        let src = "registry=https://a.example\nregistry=https://b.example\n";
        let out = apply_npmrc(src, "https://c.example");
        assert_eq!(out.matches("registry=").count(), 1, "{out}");
        assert!(out.contains("https://c.example"));
    }

    #[test]
    fn apply_npmrc_is_idempotent() {
        let once = apply_npmrc("", "https://registry.npmmirror.com");
        let twice = apply_npmrc(&once, "https://registry.npmmirror.com");
        assert_eq!(once, twice);
    }

    #[test]
    fn parse_composer_registry_from_repositories() {
        let c = r#"{"repositories":{"packagist":{"type":"composer","url":"https://mirrors.aliyun.com/composer/"}}}"#;
        assert_eq!(
            parse_composer_registry(c).as_deref(),
            Some("https://mirrors.aliyun.com/composer/")
        );
    }

    #[test]
    fn parse_composer_registry_none_when_official() {
        assert_eq!(parse_composer_registry("{}"), None);
        assert_eq!(parse_composer_registry(r#"{"config":{}}"#), None);
        assert_eq!(parse_composer_registry(""), None);
    }

    #[test]
    fn apply_composer_writes_repositories() {
        let out = apply_composer_registry("{}", "https://mirrors.aliyun.com/composer/").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["repositories"]["packagist"]["url"],
            "https://mirrors.aliyun.com/composer/"
        );
        assert_eq!(v["repositories"]["packagist"]["type"], "composer");
    }

    #[test]
    fn apply_composer_preserves_unrelated_keys() {
        let src = r#"{"name":"me/app","config":{"optimize-autoloader":true}}"#;
        let out = apply_composer_registry(src, "https://mirrors.tencent.com/composer/").unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["name"], "me/app", "不相关的键必须保留");
        assert_eq!(v["config"]["optimize-autoloader"], true);
    }

    #[test]
    fn apply_composer_rejects_invalid_json() {
        let r = apply_composer_registry("{not json", "https://x.example");
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, "BAD_COMPOSER_CONFIG");
    }

    #[test]
    fn reset_composer_removes_override() {
        let src = r#"{"repositories":{"packagist":{"type":"composer","url":"https://mirror.example"}},"name":"a/b"}"#;
        let out = reset_composer_registry(src).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(
            v.get("repositories").is_none(),
            "空 repositories 应被删掉：{out}"
        );
        assert_eq!(v["name"], "a/b", "其它内容保留");
    }

    #[test]
    fn reset_composer_keeps_other_repositories() {
        let src = r#"{"repositories":{"packagist":{"type":"composer","url":"https://m.example"},"private":{"type":"vcs","url":"https://git.example"}}}"#;
        let out = reset_composer_registry(src).unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["repositories"].get("packagist").is_none());
        assert!(
            v["repositories"].get("private").is_some(),
            "私有源不该被误删"
        );
    }

    #[test]
    fn reset_composer_on_empty_is_valid_json() {
        let out = reset_composer_registry("").unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok());
    }

    #[test]
    fn parse_pip_finds_index_url() {
        let c = "[global]\nindex-url = https://pypi.tuna.tsinghua.edu.cn/simple\ntimeout = 60\n";
        assert_eq!(
            parse_pip_ini(c).as_deref(),
            Some("https://pypi.tuna.tsinghua.edu.cn/simple")
        );
    }

    #[test]
    fn parse_pip_ignores_other_sections() {
        let c = "[install]\nindex-url = https://should-not-match.example\n";
        assert_eq!(parse_pip_ini(c), None, "只认 [global] 段");
    }

    #[test]
    fn apply_pip_replaces_existing() {
        let src = "[global]\nindex-url = https://pypi.org/simple\ntimeout = 60\n";
        let out = apply_pip_ini(src, "https://mirrors.aliyun.com/pypi/simple/");
        assert!(out.contains("index-url = https://mirrors.aliyun.com/pypi/simple/"));
        assert!(!out.contains("pypi.org/simple"));
        assert!(out.contains("timeout = 60"), "其它项保留：{out}");
    }

    #[test]
    fn apply_pip_inserts_into_existing_global_section() {
        let src = "[global]\ntimeout = 60\n\n[install]\nno-warn = true\n";
        let out = apply_pip_ini(src, "https://mirrors.ustc.edu.cn/pypi/simple/");
        // 应插在 [global] 段内，而不是文件末尾（末尾属于 [install] 段）
        let idx = out.find("index-url").unwrap();
        let install_idx = out.find("[install]").unwrap();
        assert!(idx < install_idx, "index-url 必须落在 [global] 段内：{out}");
    }

    #[test]
    fn apply_pip_creates_global_section_when_absent() {
        let out = apply_pip_ini("", "https://mirrors.aliyun.com/pypi/simple/");
        assert!(out.contains("[global]"));
        assert!(out.contains("index-url = https://mirrors.aliyun.com/pypi/simple/"));
    }

    #[test]
    fn apply_pip_is_idempotent() {
        let once = apply_pip_ini("", "https://x.example/simple");
        let twice = apply_pip_ini(&once, "https://x.example/simple");
        assert_eq!(once, twice);
    }

    #[test]
    fn set_mirror_rejects_empty_and_schemeless_url() {
        assert!(set_mirror(ToolManager::Npm, "").is_err());
        assert!(set_mirror(ToolManager::Npm, "   ").is_err());
        assert_eq!(
            set_mirror(ToolManager::Npm, "registry.npmjs.org")
                .unwrap_err()
                .code,
            "BAD_URL"
        );
        assert!(set_mirror(ToolManager::Npm, "ftp://x.example").is_err());
    }

    #[test]
    fn npmrc_config_path_is_under_home() {
        // 不该写到奇怪的地方
        if let Some(p) = npmrc_path() {
            assert!(p.to_string_lossy().ends_with(".npmrc"), "{}", p.display());
        }
    }

    #[test]
    fn composer_config_path_points_to_config_json() {
        if let Some(p) = composer_config_path() {
            assert!(
                p.to_string_lossy().ends_with("config.json"),
                "{}",
                p.display()
            );
        }
    }

    #[test]
    fn pip_config_path_has_platform_appropriate_name() {
        if let Some(p) = pip_config_path() {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if cfg!(target_os = "windows") {
                assert_eq!(name, "pip.ini");
            } else {
                assert_eq!(name, "pip.conf");
            }
        }
    }

    #[test]
    fn status_reports_options_for_every_manager() {
        let t = std::env::temp_dir().join(format!("nsb-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let store = crate::store::Store::open(t.join("m.sqlite")).unwrap();
        for m in [ToolManager::Composer, ToolManager::Npm, ToolManager::Pip] {
            let st = status(m, &store);
            assert_eq!(st.manager, m.id());
            assert!(!st.options.is_empty());
        }
        let _ = std::fs::remove_dir_all(&t);
    }
}
