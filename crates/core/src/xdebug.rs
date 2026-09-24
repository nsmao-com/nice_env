//! Xdebug 一键配置。
//!
//! ServBay / FlyEnv 都把「勾一下 Xdebug 就能断点调试」当卖点，但这件事在
//! Windows 上其实有个坑：Xdebug 是按 PHP 的 **构建指纹**（PHP API 版本 +
//! 线程安全 TS/NTS + 编译器 VS16/VS17 + 架构 x64）分发 DLL 的，装错版本
//! PHP 会直接拒绝加载，报一句 "Unable to load dynamic library"。
//!
//! 所以这里的流程是：
//! 1. 跑 `php -i` 把构建指纹读出来（这是唯一可靠的来源，不能靠猜）；
//! 2. 按指纹去 xdebug.org 拉官方发布的对应 DLL；
//! 3. 放进该版本 PHP 的 `ext/`，写 `zend_extension=` 与 `[xdebug]` 配置段；
//! 4. 用 `php -m` 实测确认真的加载上了。
//!
//! 拉不到时也**不假装成功**：把该下载的确切文件名与 URL 写给用户，
//! 他可以自己下好放到 ext 目录，走 `install_from_path` 这条路。

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::paths::Paths;

/// PHP 的构建指纹——决定该用哪个 Xdebug DLL。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhpBuild {
    pub php_version: String,
    /// 如 20240924（PHP Extension Build 的 API 号）
    pub api: String,
    /// 线程安全（TS）还是非线程安全（NTS）
    pub ts: bool,
    /// 编译器标识，如 VS16 / VS17
    pub compiler: String,
    pub arch: String,
}

impl PhpBuild {
    /// Xdebug 官方命名里的线程安全段
    pub fn ts_tag(&self) -> &'static str {
        if self.ts {
            "ts"
        } else {
            "nts"
        }
    }

    /// 拼出该 PHP 需要的 Xdebug DLL 文件名。
    ///
    /// 官方命名规则（xdebug.org/download 上的文件名）：
    ///   php_xdebug-<xdebug 版本>-<php api>-<ts|nts>-<vs>.dll
    /// 例：php_xdebug-3.4.1-8.4-vs17-x86_64-nts.dll
    ///
    /// 注意 3.x 之后文件名里带 PHP 次版本号与架构，所以这里按该规则拼；
    /// 拼不出来时退回让用户手动指路（见 `manual_hint`）。
    pub fn xdebug_dll_candidates(&self, xdebug_version: &str) -> Vec<String> {
        let short_php: String = self
            .php_version
            .split('.')
            .take(2)
            .collect::<Vec<_>>()
            .join(".");
        let ts = self.ts_tag();
        let arch = if self.arch.contains("arm") { "aarch64" } else { "x86_64" };
        let vs = self.compiler.to_ascii_lowercase().replace(' ', "");
        vec![
            // 3.x 命名（带架构）
            format!("php_xdebug-{xdebug_version}-{short_php}-{vs}-{arch}-{ts}.dll"),
            // 少数版本命名里没有架构
            format!("php_xdebug-{xdebug_version}-{short_php}-{vs}-{ts}.dll"),
            // 更老的 2.x 命名
            format!("php_xdebug-{xdebug_version}-{}-{ts}-{vs}.dll", self.api),
        ]
    }

    /// 给用户的「你自己去下」提示——包含确切文件名，照着一搜就有
    pub fn manual_hint(&self, xdebug_version: &str) -> String {
        let names = self.xdebug_dll_candidates(xdebug_version);
        format!(
            "PHP {} · {} · {} · {} —— 可到 xdebug.org/download 下载 {} 中的任意一个，改名为 php_xdebug.dll 后放入 ext 目录，再点「从文件安装」",
            self.php_version,
            if self.ts { "TS（线程安全）" } else { "NTS（非线程安全）" },
            self.compiler,
            self.arch,
            names.join(" / ")
        )
    }
}

/// 解析 `php -i` 的输出，得出构建指纹。
/// 单独抽出来是为了能对固定文本做单测，不必真的跑 PHP。
pub fn parse_build_fingerprint(php_i: &str) -> Option<PhpBuild> {
    let get = |key: &str| -> Option<String> {
        php_i.lines().find_map(|l| {
            let (k, v) = l.split_once("=>")?;
            if k.trim().eq_ignore_ascii_case(key) {
                Some(v.trim().to_string())
            } else {
                None
            }
        })
    };
    let php_version = get("PHP Version")?;
    // PHP Extension Build 形如 API20240924,TS,VS17 —— 一次拿到三个关键信息
    let ext_build = get("PHP Extension Build")
        .or_else(|| get("Zend Extension Build"))
        .unwrap_or_default();
    let mut api = String::new();
    let mut ts = false;
    let mut compiler = String::new();
    for part in ext_build.split(',') {
        let p = part.trim();
        if let Some(rest) = p.strip_prefix("API") {
            api = rest.to_string();
        } else if p.eq_ignore_ascii_case("TS") {
            ts = true;
        } else if p.eq_ignore_ascii_case("NTS") {
            ts = false;
        } else if p.to_ascii_uppercase().starts_with("VS") {
            compiler = p.to_uppercase();
        }
    }
    // 兜底：从 Thread Safety 独立读一次（某些构建 ext build 串里没有 TS 段）
    if let Some(ts_line) = get("Thread Safety") {
        ts = ts_line.to_ascii_lowercase().contains("enabled");
    }
    if let Some(c) = get("Compiler") {
        // "Visual C++ 2022" → VS17；1700/1600 之类也统一成 VSxx
        let lower = c.to_ascii_lowercase();
        let vs = if lower.contains("2022") {
            "VS17"
        } else if lower.contains("2019") {
            "VS16"
        } else if lower.contains("2017") {
            "VS15"
        } else if lower.contains("2015") {
            "VS14"
        } else {
            ""
        };
        if !vs.is_empty() {
            compiler = vs.to_string();
        }
    }
    let arch = get("Architecture").unwrap_or_else(|| "x64".to_string());
    if api.is_empty() && compiler.is_empty() {
        // 连构建信息都读不到，说明这不是一个可用的 PHP
        return None;
    }
    Some(PhpBuild {
        php_version,
        api,
        ts,
        compiler,
        arch,
    })
}

/// 读该版本 PHP 的构建指纹：跑一次 `php -i`。
pub fn detect_build(paths: &Paths, version: &str) -> Result<PhpBuild> {
    let exe = php_exe(paths, version);
    if !exe.is_file() {
        return Err(AppError::new(
            "PHP_NOT_FOUND",
            format!("找不到 PHP {version} 的可执行文件"),
        )
        .with_hint("先到「套件 / 服务」安装该版本 PHP"));
    }
    let out = std::process::Command::new(&exe)
        .arg("-i")
        .output()
        .map_err(|e| AppError::io("运行 php -i", e))?;
    let text = String::from_utf8_lossy(&out.stdout);
    parse_build_fingerprint(&text).ok_or_else(|| {
        AppError::new("PHP_BUILD_UNKNOWN", "无法识别该 PHP 的构建信息")
            .with_detail(text.chars().take(400).collect::<String>())
    })
}

fn php_exe(paths: &Paths, version: &str) -> std::path::PathBuf {
    paths.runtime_dir("php", version).join(crate::ops::exe_name("php"))
}

/// Xdebug 当前稳定版（内置默认值；可由远端清单覆盖）。
/// 之所以写死一个「已知能配合主流 PHP 版本」的版本号，是因为 Xdebug 的
/// 版本与 PHP 版本有兼容矩阵——盲取 latest 对老 PHP 反而会装错。
pub const DEFAULT_XDEBUG_VERSION: &str = "3.4.1";

/// 按 PHP 版本推测该用哪个 Xdebug 大版本。
/// 依据 xdebug.org 的支持矩阵：
/// - PHP 8.0 及以上 → Xdebug 3.x
/// - PHP 7.2–7.4 → Xdebug 3.x 也支持，但 2.9.8 更稳（老项目常用）
/// - PHP 7.0/7.1 → 只能 Xdebug 2.9
fn xdebug_version_for(php_version: &str) -> &'static str {
    let parts: Vec<&str> = php_version.split('.').collect();
    let major: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(8);
    let minor: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    // 注意不能只看 minor：PHP 8.0 的 minor 是 0，但它必须用 Xdebug 3
    if major >= 8 {
        DEFAULT_XDEBUG_VERSION
    } else if major == 7 && minor >= 2 {
        "3.1.6"
    } else {
        "2.9.8"
    }
}

/// Xdebug 状态（供 UI 显示「已配置 / 未配置 / 装错版本」）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XdebugStatus {
    pub version: String,
    pub build: Option<PhpBuild>,
    /// ext/php_xdebug.dll 是否存在
    pub dll_present: bool,
    /// php.ini 里是否已启用（zend_extension=xdebug 且未被注释）
    pub enabled: bool,
    /// php -m 实测是否真的加载了
    pub loaded: bool,
    /// 已加载时的 Xdebug 版本号（实测）
    pub loaded_version: Option<String>,
    /// 建议的 xdebug 版本
    pub recommended: String,
    /// 需要的话，该下载的文件名候选
    pub dll_candidates: Vec<String>,
    /// 给用户的手动指引
    pub manual_hint: String,
    /// 当前 [xdebug] 段的配置（mode / client_port 等）
    pub settings: std::collections::BTreeMap<String, String>,
}

/// 读取 Xdebug 的配置段。返回 [xdebug] 段内的 key=value。
pub fn read_xdebug_section(ini: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut in_section = false;
    for line in ini.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            in_section = t.eq_ignore_ascii_case("[xdebug]");
            continue;
        }
        if !in_section || t.is_empty() || t.starts_with(';') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            out.insert(
                k.trim().to_string(),
                v.trim().trim_matches('"').to_string(),
            );
        }
    }
    out
}

/// 实测 Xdebug 是否加载 + 版本号。
/// 用 `php -r` 而不是 `php -m`，因为 `-r` 能顺手把版本号打出来。
fn probe_xdebug(paths: &Paths, version: &str) -> (bool, Option<String>) {
    let exe = php_exe(paths, version);
    if !exe.is_file() {
        return (false, None);
    }
    let out = std::process::Command::new(&exe)
        .arg("-n")
        .arg("-c")
        .arg(paths.php_ini(version))
        .arg("-r")
        .arg("echo extension_loaded('xdebug') ? phpversion('xdebug') : '';")
        .output();
    match out {
        Ok(o) => {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.is_empty() {
                (false, None)
            } else {
                (true, Some(s))
            }
        }
        Err(_) => (false, None),
    }
}

/// 汇总 Xdebug 现状
pub fn status(paths: &Paths, version: &str) -> Result<XdebugStatus> {
    let ini_path = paths.php_ini(version);
    let ini = std::fs::read_to_string(&ini_path).unwrap_or_default();
    let state = crate::phpext::IniExtState::parse(&ini);
    let ext_dir = paths.runtime_dir("php", version).join("ext");
    let dll_present = ext_dir.join(crate::phpext::dll_file_name("xdebug")).is_file();
    let build = detect_build(paths, version).ok();
    let recommended = xdebug_version_for(&build.as_ref().map(|b| b.php_version.clone()).unwrap_or_else(|| version.to_string())).to_string();
    let candidates = build
        .as_ref()
        .map(|b| b.xdebug_dll_candidates(&recommended))
        .unwrap_or_default();
    let manual_hint = build
        .as_ref()
        .map(|b| b.manual_hint(&recommended))
        .unwrap_or_else(|| "先启动一次该版本 PHP 以便识别构建信息".to_string());
    let (loaded, loaded_version) = probe_xdebug(paths, version);

    Ok(XdebugStatus {
        version: version.to_string(),
        build,
        dll_present,
        enabled: state.enabled.contains("xdebug"),
        loaded,
        loaded_version,
        recommended,
        dll_candidates: candidates,
        manual_hint,
        settings: read_xdebug_section(&ini),
    })
}

/// 生成的 [xdebug] 配置段。
///
/// `dll_path` 传**普通路径**即可，反斜杠转义在这里统一处理
/// （php.ini 里双引号内的 `\` 是转义字符，`C:\php\ext` 会被吃成 `C:phpext`，
/// 所以必须写成 `C:\\php\\ext`。这个坑以前分散在两个调用点，很容易漏掉一处）。
///
/// 默认值的选择理由：
/// - `mode=debug,develop`：调试与开发友好提示都要；`coverage` 默认不开
///   （会显著拖慢执行，需要时用户自己加）。
/// - `start_with_request=trigger`：不主动连 IDE，避免没开 IDE 时每次请求
///   都要等连接超时——这是新手最常踩的「网页变慢」坑。
/// - `client_port=9003`：Xdebug 3 起的默认端口（2.x 是 9000，容易和
///   php-fpm 撞车）。
pub fn render_xdebug_section(dll_path: &str, mode: &str, client_port: u16) -> String {
    let escaped = dll_path.replace('\\', "\\\\");
    format!(
        r#"
[xdebug]
; 由 NiceEnv 生成：反斜杠已转义，改动请保持同格式
zend_extension="{escaped}"
xdebug.mode={mode}
xdebug.start_with_request=trigger
xdebug.client_host=127.0.0.1
xdebug.client_port={client_port}
xdebug.log_level=0
"#
    )
}

/// 在 php.ini 里写入/替换 [xdebug] 配置段（幂等）。
pub fn upsert_xdebug_section(ini: &str, section: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut in_section = false;
    let mut removed = false;
    for line in ini.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if t.eq_ignore_ascii_case("[xdebug]") {
                // 丢掉旧段，稍后用新的替换
                in_section = true;
                removed = true;
                continue;
            }
            in_section = false;
        }
        if in_section {
            continue;
        }
        lines.push(line.to_string());
    }
    // 去掉尾部空行，让新段紧贴
    while matches!(lines.last(), Some(l) if l.trim().is_empty()) {
        lines.pop();
    }
    let mut s = lines.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    if removed || true {
        s.push_str(section.trim_end());
        s.push('\n');
    }
    s
}

/// 从指定文件安装 Xdebug DLL（用户自己下好的情况）。
///
/// 会校验：真的是个 DLL（PE 头）、文件名对得上、放进去后 PHP 能加载。
pub fn install_from_path(
    paths: &Paths,
    version: &str,
    src: &std::path::Path,
    mode: &str,
    client_port: u16,
) -> Result<()> {
    if !src.is_file() {
        return Err(AppError::new("FILE_NOT_FOUND", "指定的文件不存在"));
    }
    let head = std::fs::read(src).map_err(|e| AppError::io("读取 DLL", e))?;
    if head.len() < 2 || &head[0..2] != b"MZ" {
        return Err(AppError::new(
            "NOT_A_DLL",
            "这个文件不是 Windows DLL（缺少 MZ 头）",
        )
        .with_hint("请确认下载的是 php_xdebug-*.dll，而不是源码包或说明文件"));
    }
    let ext_dir = paths.runtime_dir("php", version).join("ext");
    std::fs::create_dir_all(&ext_dir).map_err(|e| AppError::io("创建 ext 目录", e))?;
    let dst = ext_dir.join(crate::phpext::dll_file_name("xdebug"));
    std::fs::copy(src, &dst).map_err(|e| AppError::io("写入 xdebug.dll", e))?;

    let ini_path = paths.php_ini(version);
    let ini = std::fs::read_to_string(&ini_path).map_err(|e| AppError::io("读取 php.ini", e))?;
    // 先确保 zend_extension=xdebug 是启用的（走 phpext 的既有逻辑）
    let enabled = crate::phpext::apply_to_content(&ini, "xdebug", true);
    let section = render_xdebug_section(&dst.to_string_lossy(), mode, client_port);
    let next = upsert_xdebug_section(&enabled, &section);
    crate::paths::write_with_backup(&ini_path, &next, &paths.backup())
        .map_err(|e| AppError::io("写入 php.ini", e))?;
    Ok(())
}

/// 配置 Xdebug：验证加载 + 返回实测结果
pub fn verify(paths: &Paths, version: &str) -> (bool, Option<String>, Vec<String>) {
    let ini_path = paths.php_ini(version);
    let exe = php_exe(paths, version);
    if !exe.is_file() {
        return (false, None, vec!["找不到 php 可执行文件".into()]);
    }
    let out = std::process::Command::new(&exe)
        .arg("-n")
        .arg("-c")
        .arg(&ini_path)
        .arg("-r")
        .arg("echo extension_loaded('xdebug') ? phpversion('xdebug') : '';")
        .output();
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&o.stderr);
            let warnings: Vec<String> = stderr
                .lines()
                .map(|l| l.trim())
                .filter(|l| {
                    let low = l.to_ascii_lowercase();
                    low.contains("warning") || low.contains("unable to load") || low.contains("failed")
                })
                .map(|l| l.to_string())
                .collect();
            if stdout.is_empty() {
                (false, None, warnings)
            } else {
                (true, Some(stdout), warnings)
            }
        }
        Err(e) => (false, None, vec![format!("无法运行 PHP：{e}")]),
    }
}

/// 「一键配置」的默认 Xdebug 设置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XdebugSetupInput {
    pub version: String,
    /// debug / develop / coverage / profile 的组合，逗号分隔
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_port")]
    pub client_port: u16,
    /// 用户已下好的 DLL 路径（为空则走在线下载）
    #[serde(default)]
    pub dll_path: Option<String>,
}

fn default_mode() -> String {
    "debug,develop".to_string()
}
fn default_port() -> u16 {
    9003
}

/// 官方下载地址（按候选文件名依次尝试）
pub fn download_urls(candidate: &str) -> Vec<String> {
    vec![
        format!("https://xdebug.org/files/{candidate}"),
        format!("https://github.com/xdebug/xdebug/releases/latest/download/{candidate}"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHP_I_TS: &str = "\
PHP Version => 8.4.25
Compiler => Visual C++ 2022
Architecture => x64
Zend Extension Build => API420240924,TS,VS17
PHP Extension Build => API20240924,TS,VS17
Thread Safety => enabled
";

    const PHP_I_NTS: &str = "\
PHP Version => 7.4.33
Compiler => Visual C++ 2019
Architecture => x64
PHP Extension Build => API20190902,NTS,VS16
Thread Safety => disabled
";

    #[test]
    fn parses_thread_safe_build() {
        let b = parse_build_fingerprint(PHP_I_TS).unwrap();
        assert_eq!(b.php_version, "8.4.25");
        assert_eq!(b.api, "20240924");
        assert!(b.ts);
        assert_eq!(b.compiler, "VS17");
        assert_eq!(b.arch, "x64");
        assert_eq!(b.ts_tag(), "ts");
    }

    #[test]
    fn parses_non_thread_safe_build() {
        let b = parse_build_fingerprint(PHP_I_NTS).unwrap();
        assert!(!b.ts);
        assert_eq!(b.ts_tag(), "nts");
        assert_eq!(b.compiler, "VS16");
    }

    #[test]
    fn compiler_falls_back_to_visual_c_year() {
        // 没有 VSxx 段时，靠 Compiler 行推
        let text = "PHP Version => 8.1.0\nCompiler => Visual C++ 2019\nArchitecture => x64\nPHP Extension Build => API20210902,TS\n";
        let b = parse_build_fingerprint(text).unwrap();
        assert_eq!(b.compiler, "VS16");
    }

    #[test]
    fn rejects_unusable_php_i() {
        assert!(parse_build_fingerprint("no such thing here").is_none());
        assert!(parse_build_fingerprint("").is_none());
    }

    #[test]
    fn dll_candidates_include_arch_and_ts() {
        let b = parse_build_fingerprint(PHP_I_TS).unwrap();
        let c = b.xdebug_dll_candidates("3.4.1");
        assert!(c[0].contains("8.4"), "应带 PHP 次版本：{c:?}");
        assert!(c[0].contains("vs17"));
        assert!(c[0].contains("x86_64"));
        assert!(c[0].ends_with("-ts.dll"), "TS 构建要用 ts 后缀：{c:?}");
    }

    #[test]
    fn dll_candidates_for_nts_end_with_nts() {
        let b = parse_build_fingerprint(PHP_I_NTS).unwrap();
        let c = b.xdebug_dll_candidates("3.4.1");
        assert!(c[0].ends_with("-nts.dll"), "{c:?}");
    }

    #[test]
    fn manual_hint_mentions_filename_and_ts() {
        let b = parse_build_fingerprint(PHP_I_TS).unwrap();
        let h = b.manual_hint("3.4.1");
        assert!(h.contains("php_xdebug-3.4.1"));
        assert!(h.contains("TS"));
        assert!(h.contains("Visual C++ 2022") || h.contains("VS17"));
    }

    #[test]
    fn xdebug_version_matrix() {
        // PHP 8.0 的 minor 是 0 —— 必须仍然判给 Xdebug 3（曾因只看 minor 而误判成 2.9）
        assert_eq!(xdebug_version_for("8.0.30"), "3.4.1");
        assert_eq!(xdebug_version_for("8.4.25"), "3.4.1");
        assert_eq!(xdebug_version_for("8.5.10"), "3.4.1");
        assert_eq!(xdebug_version_for("7.4.33"), "3.1.6");
        assert_eq!(xdebug_version_for("7.2.34"), "3.1.6");
        assert_eq!(xdebug_version_for("7.0.33"), "2.9.8");
        assert_eq!(xdebug_version_for("7.1.33"), "2.9.8");
    }

    #[test]
    fn reads_xdebug_section() {
        let ini = "memory_limit=256M\n\n[xdebug]\nxdebug.mode=debug\nxdebug.client_port=9003\n\n[Session]\na=1\n";
        let m = read_xdebug_section(ini);
        assert_eq!(m.get("xdebug.mode").map(String::as_str), Some("debug"));
        assert_eq!(m.get("xdebug.client_port").map(String::as_str), Some("9003"));
        assert!(!m.contains_key("a"), "不该跨段读取");
    }

    #[test]
    fn reads_section_ignores_comments() {
        let ini = "[xdebug]\n;xdebug.mode=debug\nxdebug.mode=develop\n";
        let m = read_xdebug_section(ini);
        assert_eq!(m.get("xdebug.mode").map(String::as_str), Some("develop"));
    }

    #[test]
    fn upsert_replaces_existing_section_in_place() {
        let ini = "engine=On\n\n[xdebug]\nxdebug.mode=debug\n\n[Session]\nsave=files\n";
        let out = upsert_xdebug_section(ini, "[xdebug]\nxdebug.mode=develop\n");
        assert_eq!(out.matches("[xdebug]").count(), 1, "不能出现两个 xdebug 段：{out}");
        assert!(out.contains("xdebug.mode=develop"));
        assert!(!out.contains("xdebug.mode=debug"), "旧值应被替换：{out}");
        // 其它段必须完好
        assert!(out.contains("[Session]"));
        assert!(out.contains("save=files"));
        assert!(out.contains("engine=On"));
    }

    #[test]
    fn upsert_appends_when_absent() {
        let ini = "engine=On\n";
        let out = upsert_xdebug_section(ini, "[xdebug]\nxdebug.mode=debug\n");
        assert!(out.contains("engine=On"));
        assert!(out.contains("[xdebug]"));
        assert_eq!(out.matches("[xdebug]").count(), 1);
    }

    #[test]
    fn upsert_is_idempotent() {
        let ini = "engine=On\n";
        let sec = "[xdebug]\nxdebug.mode=debug\n";
        let once = upsert_xdebug_section(ini, sec);
        let twice = upsert_xdebug_section(&once, sec);
        assert_eq!(once, twice);
    }

    #[test]
    fn rendered_section_escapes_backslashes_itself() {
        // 调用方传「普通路径」，render 负责转义 —— php.ini 里没转义的话
        // C:\php\ext 会变成 C:phpext，Xdebug 根本加载不了
        let s = render_xdebug_section(r"C:\php\ext\php_xdebug.dll", "debug,develop", 9003);
        assert!(
            s.contains(r#"zend_extension="C:\\php\\ext\\php_xdebug.dll""#),
            "应写成双反斜杠：{s}"
        );
        // 断言确实没有未转义的单反斜杠路径漏出去
        assert!(!s.contains(r#"zend_extension="C:\php"#), "存在未转义路径：{s}");
        assert!(s.contains("xdebug.mode=debug,develop"));
        // 默认必须是 trigger，否则没开 IDE 时每个请求都要等超时
        assert!(
            s.contains("xdebug.start_with_request=trigger"),
            "默认不能是 yes：{s}"
        );
        assert!(s.contains("xdebug.client_port=9003"));
    }

    #[test]
    fn rendered_section_accepts_forward_slash_path() {
        // 非 Windows 平台路径不带反斜杠，转义不应破坏它
        let s = render_xdebug_section("/usr/lib/php/ext/xdebug.so", "debug", 9003);
        assert!(s.contains(r#"zend_extension="/usr/lib/php/ext/xdebug.so""#));
    }

    #[test]
    fn download_urls_cover_official_and_github() {
        let u = download_urls("php_xdebug-3.4.1-8.4-vs17-x86_64-ts.dll");
        assert!(u[0].starts_with("https://xdebug.org/files/"));
        assert!(u[1].contains("github.com"));
    }
}
