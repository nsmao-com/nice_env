//! PHP 扩展管理。
//!
//! Windows 官方 PHP zip 自带一整套 `ext/php_*.dll`，但 php.ini 里默认只开了一小撮。
//! ServBay / FlyEnv / phpStudy 都提供「勾选即启用」的扩展面板——这是它们最常被用到的
//! 功能之一（装 xdebug、开 redis、开 intl 全靠它），所以这里也补上。
//!
//! 设计要点：
//! - **扫描真实磁盘**：以 `ext/php_*.dll` 为准，而不是维护一份写死的清单。这样
//!   用户自己丢进去的扩展、以及不同 PHP 版本扩展集合的差异都能正确反映。
//! - **状态来源是 php.ini**：`extension=` / `zend_extension=` 行决定启用与否。
//!   禁用时不删行而是注释掉（`;extension=...`），保留用户原本的顺序与上下文。
//! - **依赖提示**：部分扩展互相依赖（如 `pdo_mysql` 需要 `pdo`，`mysqli` 需要
//!   `mysqlnd`）。启用被依赖项缺失时给出提示，而不是让用户对着一句
//!   "Unable to load dynamic library" 发呆。
//! - **改前备份 + 写后校验**：复用 configgen 的 write_with_backup，并用
//!   `php -n -c <ini> -m` 实测扩展是否真的加载成功——加载失败时把 PHP 的原始
//!   告警回报给用户，而不是假装成功。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};
use crate::model::PhpExtension;
use crate::paths::{write_with_backup, Paths};

/// 需要 `zend_extension=` 而不是 `extension=` 的扩展（Zend 扩展走独立加载器）。
/// 名字按 DLL 去掉 `php_` 前缀、去掉 `.dll` 后缀后的形式比对。
const ZEND_EXTENSIONS: &[&str] = &["xdebug", "opcache", "ioncube_loader_win", "uploadprogress"];

/// 扩展 → 它依赖的其它扩展（同名形式，均需已启用）。
/// 只收录「缺了必然报错且不好排查」的组合，不做完整依赖图求解。
const EXT_DEPS: &[(&str, &[&str])] = &[
    ("pdo_mysql", &["pdo"]),
    ("pdo_sqlite", &["pdo"]),
    ("pdo_pgsql", &["pdo"]),
    ("mysqli", &["mysqlnd"]),
    ("pdo_mysql", &["mysqlnd"]),
    ("gd", &["gd"]), // 8.x 起 GD 自带，7.x 的 gd2 见下
];

/// 展示名与分类：让面板不是一串裸文件名。
struct Meta {
    label: &'static str,
    group: &'static str,
    hint: &'static str,
}

/// 常见扩展的友好名 / 分组 / 一句话说明。
/// 未收录的扩展也能正常启用，只是显示原始名字、归到「其他」。
fn meta_for(name: &str) -> Option<Meta> {
    let (label, group, hint) = match name {
        "core" => ("Core", "basic", "PHP 核心，不可禁用"),
        "standard" => ("Standard", "basic", "标准库函数集"),
        "spl" => ("SPL", "basic", "标准 PHP 库（内置）"),
        "pcre" => ("PCRE", "basic", "正则表达式（内置）"),
        "date" => ("Date", "basic", "日期时间（内置）"),
        "json" => ("JSON", "basic", "JSON 编解码（内置）"),
        "hash" => ("Hash", "basic", "哈希算法（内置）"),
        "reflection" => ("Reflection", "basic", "反射（内置）"),
        "session" => ("Session", "basic", "会话管理"),
        "filter" => ("Filter", "basic", "数据过滤与校验"),
        "ctype" => ("Ctype", "basic", "字符类型检查"),
        "tokenizer" => ("Tokenizer", "basic", "词法分析（Composer 需要）"),
        "iconv" => ("Iconv", "basic", "字符集转换"),
        "zlib" => ("Zlib", "basic", "压缩流（内置）"),

        "openssl" => ("OpenSSL", "security", "HTTPS / 加密 / 证书"),
        "sodium" => ("Sodium", "security", "现代加密库"),
        "mcrypt" => ("Mcrypt", "security", "旧式加密（已废弃，老项目才用）"),

        "curl" => ("cURL", "network", "HTTP 客户端，调第三方接口必备"),
        "sockets" => ("Sockets", "network", "底层 socket"),
        "ftp" => ("FTP", "network", "FTP 客户端"),
        "imap" => ("IMAP", "network", "收邮件"),
        "soap" => ("SOAP", "network", "SOAP 客户端/服务端"),
        "ldap" => ("LDAP", "network", "目录服务"),

        "pdo" => ("PDO", "database", "PDO 抽象层（其它 pdo_* 的前置）"),
        "pdo_mysql" => ("PDO MySQL", "database", "PDO 连 MySQL —— Laravel 等框架默认走这条"),
        "pdo_sqlite" => ("PDO SQLite", "database", "PDO 连 SQLite"),
        "pdo_pgsql" => ("PDO PostgreSQL", "database", "PDO 连 PostgreSQL"),
        "pdo_sqlsrv" => ("PDO SQL Server", "database", "PDO 连 SQL Server"),
        "pdo_oci" => ("PDO Oracle", "database", "PDO 连 Oracle"),
        "mysqli" => ("MySQLi", "database", "MySQL 原生扩展（WordPress 用它）"),
        "mysqlnd" => ("MySQLnd", "database", "MySQL 原生驱动（mysqli / pdo_mysql 的底层）"),
        "pgsql" => ("PostgreSQL", "database", "PostgreSQL 原生扩展"),
        "sqlite3" => ("SQLite3", "database", "SQLite 原生扩展"),
        "mongodb" => ("MongoDB", "database", "MongoDB 驱动（需另外下载）"),
        "oci8" => ("OCI8", "database", "Oracle 原生扩展"),
        "odbc" => ("ODBC", "database", "ODBC 数据源"),
        "sqlsrv" => ("SQL Server", "database", "SQL Server 原生扩展"),
        "redis" => ("Redis", "cache", "Redis 客户端（连接本地 Redis / 队列）"),
        "memcached" => ("Memcached", "cache", "Memcached 客户端"),
        "apcu" => ("APCu", "cache", "用户态缓存（Laravel 可选用）"),
        "igbinary" => ("igbinary", "cache", "更紧凑的序列化，Redis/Session 可选"),

        "gd" => ("GD", "image", "图像处理（验证码 / 缩略图）"),
        "gd2" => ("GD2", "image", "图像处理（PHP 7.4 及以前）"),
        "imagick" => ("ImageMagick", "image", "ImageMagick 绑定（需另外下载）"),
        "exif" => ("EXIF", "image", "读取图片元数据"),
        "gmagick" => ("GraphicsMagick", "image", "GraphicsMagick 绑定"),

        "mbstring" => ("mbstring", "text", "多字节字符串（中文项目几乎必开）"),
        "intl" => ("Intl", "text", "国际化（ICU）—— 时间/货币/多语言格式化"),
        "gettext" => ("Gettext", "text", "gettext 多语言"),
        "pspell" => ("Pspell", "text", "拼写检查"),
        "enchant" => ("Enchant", "text", "拼写检查（Enchant 后端）"),

        "zip" => ("Zip", "archive", "zip 读写（Composer 装包要用）"),
        "bz2" => ("Bzip2", "archive", "bzip2 压缩"),
        "phar" => ("Phar", "archive", "PHP 归档（Composer 依赖）"),
        "zend opcache" => ("OPcache", "performance", "字节码缓存 —— 生产环境必开"),
        "opcache" => ("OPcache", "performance", "字节码缓存 —— 生产环境必开"),
        "xdebug" => ("Xdebug", "debug", "断点调试 / 性能剖析（配合 IDE）"),
        "uploadprogress" => ("Upload Progress", "debug", "上传进度回调（老框架用）"),

        "fileinfo" => ("Fileinfo", "file", "识别文件真实 MIME 类型"),
        "exif_alt" => ("EXIF", "file", "图片元数据"),
        "ffi" => ("FFI", "file", "调用 C 库"),
        "com_dotnet" => ("COM / .NET", "system", "调用 Windows COM 组件"),
        "sysvshm" => ("SysV SHM", "system", "共享内存"),
        "sysvsem" => ("SysV Sem", "system", "信号量"),
        "shmop" => ("Shmop", "system", "共享内存操作"),
        "pcntl" => ("PCNTL", "system", "进程控制（CLI 用；Windows 不支持）"),
        "posix" => ("POSIX", "system", "POSIX 接口（Windows 不支持）"),
        "readline" => ("Readline", "system", "交互式命令行"),
        "snmp" => ("SNMP", "system", "网络设备监控"),
        "yaml" => ("YAML", "system", "YAML 解析（需另外下载）"),
        "dba" => ("DBA", "system", "dbm 风格数据库"),
        "interbase" => ("InterBase", "system", "Firebird / InterBase"),
        "tidy" => ("Tidy", "system", "HTML 清理与修复"),
        "xmlrpc" => ("XML-RPC", "system", "XML-RPC（已废弃）"),
        _ => ("", "", ""),
    };
    if label.is_empty() {
        None
    } else {
        Some(Meta {
            label,
            group,
            hint,
        })
    }
}

/// PHP 自带的、通常不该手动禁用的核心扩展（禁用会把 PHP 弄坏）。
/// 只用于前端提示，不做强制拦截——真有用户想关 wepi 也不会被挡。
fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "core"
            | "standard"
            | "spl"
            | "pcre"
            | "date"
            | "json"
            | "hash"
            | "reflection"
            | "filter"
            | "ctype"
            | "tokenizer"
            | "zlib"
            | "session"
            | "iconv"
            | "phar"
    )
}

/// 把 `php_curl.dll` / `php_xdebug.dll` / `redis.so` → `curl` / `xdebug` / `redis`
///
/// 注意 `.dll` 与 `.so` 都要接受：前者是 Windows，后者是 macOS/Linux。
/// 早期版本在这里写成 `strip_suffix(".dll")?`，会让非 Windows 平台的扩展
/// 一律扫不出来（返回 None 被静默跳过）。
fn ext_name_from_file(file_name: &str) -> Option<String> {
    let stem = file_name
        .strip_suffix(".dll")
        .or_else(|| file_name.strip_suffix(".so"))?;
    let name = stem.strip_prefix("php_").unwrap_or(stem);
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// 该扩展对应的 DLL 文件名（Windows）/ so 名（其它平台）
pub fn dll_file_name(ext: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("php_{ext}.dll")
    } else {
        format!("{ext}.so")
    }
}

/// 旧名保留，内部使用
fn dll_name(ext: &str) -> String {
    dll_file_name(ext)
}

fn is_zend(name: &str) -> bool {
    ZEND_EXTENSIONS.contains(&name)
}

/// 一行 `extension=` / `zend_extension=` 的解析结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IniExtLine {
    pub ext: String,
    pub zend: bool,
    /// 是否为注释状态（`;extension=...`）
    pub commented: bool,
}

/// 解析一行，若不是扩展加载行则返回 None。
///
/// 需要容忍的写法：
/// - `extension=curl`
/// - `extension = "curl"`（值可能带引号）
/// - `extension=php_curl.dll`（旧教程常这么写）
/// - `;extension=curl`（被注释掉 = 已禁用）
/// - `zend_extension="C:\...\php_xdebug.dll"`（绝对路径）
pub fn parse_ext_line(line: &str) -> Option<IniExtLine> {
    let trimmed = line.trim();
    let (body, commented) = match trimmed.strip_prefix(';') {
        Some(rest) => (rest.trim_start(), true),
        None => (trimmed, false),
    };
    let (key, value) = body.split_once('=')?;
    let key = key.trim().to_ascii_lowercase();
    let zend = match key.as_str() {
        "extension" => false,
        "zend_extension" => true,
        _ => return None,
    };
    let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
    if value.is_empty() {
        return None;
    }
    // 路径形式只取文件名
    let base = value
        .rsplit(|c| c == '/' || c == '\\')
        .next()
        .unwrap_or(value);
    // 去掉平台后缀后可能是完整文件名，也可能只是个裸名字
    let ext = {
        let no_dll = base.strip_suffix(".dll").or_else(|| base.strip_suffix(".so"));
        let stem = no_dll.unwrap_or(base);
        stem.strip_prefix("php_").unwrap_or(stem).to_string()
    };
    if ext.is_empty() {
        None
    } else {
        Some(IniExtLine {
            ext,
            zend,
            commented,
        })
    }
}

/// php.ini 里扩展启用状态的快照
#[derive(Debug, Clone, Default)]
pub struct IniExtState {
    /// 已启用（非注释）的扩展名
    pub enabled: BTreeSet<String>,
    /// 出现过但被注释掉的扩展名
    pub disabled: BTreeSet<String>,
    /// 原始内容的行，供改写时保持顺序
    pub lines: Vec<String>,
}

impl IniExtState {
    pub fn parse(content: &str) -> Self {
        let mut st = IniExtState {
            lines: content.lines().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        for line in &st.lines {
            if let Some(parsed) = parse_ext_line(line) {
                if parsed.commented {
                    st.disabled.insert(parsed.ext);
                } else {
                    st.enabled.insert(parsed.ext);
                }
            }
        }
        st
    }

    /// 生成 «启用 ext» 后的完整内容：已有的取消注释，没有的追加到 [Extensions] 段尾。
    pub fn with_enabled(&self, ext: &str) -> String {
        self.set_enabled(ext, true)
    }

    /// 生成 «禁用 ext» 后的完整内容：注释掉已有行（保留位置），不存在则原样返回。
    pub fn with_disabled(&self, ext: &str) -> String {
        self.set_enabled(ext, false)
    }

    fn set_enabled(&self, ext: &str, enable: bool) -> String {
        let mut out: Vec<String> = Vec::with_capacity(self.lines.len() + 1);
        let mut handled = false;

        for line in &self.lines {
            match parse_ext_line(line) {
                Some(p) if p.ext.eq_ignore_ascii_case(ext) => {
                    handled = true;
                    out.push(rewrite_line(line, ext, enable));
                }
                _ => out.push(line.clone()),
            }
        }

        if !handled && enable {
            let key = if is_zend(ext) { "zend_extension" } else { "extension" };
            let new_line = format!("{key}={ext}");
            // 优先塞进 [Extensions] 段末尾，没有该段就追加到文件末尾
            match find_section_end(&out, "Extensions") {
                Some(idx) => out.insert(idx, new_line),
                None => {
                    out.push(String::new());
                    out.push("[Extensions]".to_string());
                    out.push(new_line);
                }
            }
        }

        let mut s = out.join("\n");
        s.push('\n');
        s
    }
}

/// 把一行改成启用/禁用形态，保留原始缩进与注释风格
fn rewrite_line(line: &str, ext: &str, enable: bool) -> String {
    let parsed = match parse_ext_line(line) {
        Some(p) => p,
        None => return line.to_string(),
    };
    let key = if parsed.zend { "zend_extension" } else { "extension" };
    let new_body = format!("{key}={ext}");
    if enable {
        // 去掉行首注释；若原本是绝对路径写法，这里统一收成裸名字，
        // 因为 extension_dir 已由我们生成，裸名字更不容易因迁移而失效。
        new_body
    } else {
        format!(";{new_body}")
    }
}

/// 找某个 `[section]` 段的最后一行位置（用于把新扩展插到段内末尾）
fn find_section_end(lines: &[String], section: &str) -> Option<usize> {
    let header = format!("[{section}]");
    let start = lines
        .iter()
        .position(|l| l.trim().eq_ignore_ascii_case(&header))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, l)| {
            let t = l.trim();
            t.starts_with('[') && t.ends_with(']')
        })
        .map(|(i, _)| i)
        .unwrap_or(lines.len());
    // 回退掉段尾的空行，让新行紧贴已有条目
    let mut idx = end;
    while idx > start + 1 && lines[idx - 1].trim().is_empty() {
        idx -= 1;
    }
    Some(idx)
}

/// 扫描该 PHP 版本 `ext/` 目录下真实存在的扩展 DLL。
pub fn scan_available(paths: &Paths, version: &str) -> Result<Vec<PhpExtension>> {
    let ext_dir = paths.runtime_dir("php", version).join("ext");
    if !ext_dir.is_dir() {
        return Ok(Vec::new());
    }
    let ini_path = paths.php_ini(version);
    let state = if ini_path.is_file() {
        IniExtState::parse(&std::fs::read_to_string(&ini_path).unwrap_or_default())
    } else {
        IniExtState::default()
    };

    let mut names: BTreeSet<String> = BTreeSet::new();
    for entry in std::fs::read_dir(&ext_dir).map_err(|e| AppError::io("读取 PHP ext 目录", e))? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let file_name = entry.file_name().to_string_lossy().to_string();
        if let Some(n) = ext_name_from_file(&file_name) {
            names.insert(n);
        }
    }

    let mut out = Vec::with_capacity(names.len());
    for name in names {
        let enabled = state.enabled.contains(&name);
        let meta = meta_for(&name);
        let deps: Vec<String> = EXT_DEPS
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(&name))
            .flat_map(|(_, ds)| ds.iter().map(|s| s.to_string()))
            .collect();
        // 只报「当前没启用」的缺失依赖，已启用的不啰嗦
        let missing: Vec<String> = deps
            .iter()
            .filter(|d| !state.enabled.contains(*d))
            .cloned()
            .collect();
        out.push(PhpExtension {
            name: name.clone(),
            label: meta.as_ref().map(|m| m.label.to_string()).unwrap_or(name.clone()),
            group: meta
                .as_ref()
                .map(|m| m.group.to_string())
                .unwrap_or_else(|| "other".to_string()),
            hint: meta
                .as_ref()
                .map(|m| m.hint.to_string())
                .unwrap_or_else(|| "第三方扩展".to_string()),
            enabled,
            zend: is_zend(&name),
            builtin: is_builtin(&name),
            dll: dll_name(&name),
            missing_deps: missing,
        });
    }
    Ok(out)
}

/// 启用 / 禁用某个扩展，并返回 PHP 实测的加载告警（成功了就是空）。
///
/// 流程：改 ini（自动备份）→ 用 `php -n -c <ini> -m` 实测 →
/// 若目标扩展没出现在模块列表里，把 stderr 原样带回。
pub fn set_extension(
    paths: &Paths,
    version: &str,
    ext: &str,
    enable: bool,
) -> Result<Vec<String>> {
    let ini_path = paths.php_ini(version);
    if !ini_path.is_file() {
        return Err(AppError::new("NO_INI", format!(
            "PHP {version} 的 php.ini 还不存在"
        ))
        .with_hint("先启动一次该版本 PHP，或到「套件 / 服务」重装 PHP"));
    }
    let content = std::fs::read_to_string(&ini_path).map_err(|e| AppError::io("读取 php.ini", e))?;
    let state = IniExtState::parse(&content);
    let next = if enable {
        state.with_enabled(ext)
    } else {
        state.with_disabled(ext)
    };
    write_with_backup(&ini_path, &next, &paths.backup())
        .map_err(|e| AppError::io("写入 php.ini", e))?;

    // 实测：能加载才算真的成功，否则把 PHP 的原始告警回给前端
    let php_exe = paths.runtime_dir("php", version).join(crate::ops::exe_name("php"));
    let mut warnings = Vec::new();
    if php_exe.is_file() {
        let out = std::process::Command::new(&php_exe)
            .arg("-n")
            .arg("-c")
            .arg(&ini_path)
            .arg("-m")
            .output();
        match out {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                for line in stderr
                    .lines()
                    .chain(stdout.lines())
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                {
                    let low = line.to_ascii_lowercase();
                    if low.contains("warning")
                        || low.contains("unable to load")
                        || low.contains("failed")
                    {
                        warnings.push(line.to_string());
                    }
                }
                if enable {
                    let loaded = stdout
                        .lines()
                        .any(|l| l.trim().eq_ignore_ascii_case(ext));
                    if !loaded && warnings.is_empty() {
                        warnings.push(format!(
                            "PHP 未报告 {ext} 已加载；若扩展名与 DLL 不匹配，请确认 ext 目录下存在 {}",
                            dll_name(ext)
                        ));
                    }
                }
            }
            Err(e) => warnings.push(format!("无法运行 php -m 校验：{e}")),
        }
    }
    Ok(warnings)
}

/// php.ini 中与实际扩展加载无关、但常被调整的几项。
/// 面板上直接给开关，省得用户去翻文件。
#[derive(Debug, Clone, Copy)]
pub struct IniToggle {
    pub key: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
    /// true 表示值形如 `key=1/0`，false 表示 `key=On/Off`
    pub numeric: bool,
}

pub const INI_TOGGLES: &[IniToggle] = &[
    IniToggle {
        key: "display_errors",
        label: "显示错误",
        hint: "开发时打开，把报错直接打在页面上",
        numeric: false,
    },
    IniToggle {
        key: "log_errors",
        label: "记录错误日志",
        hint: "写入 logs/php/<版本>/php_errors.log",
        numeric: false,
    },
    IniToggle {
        key: "opcache.enable",
        label: "OPcache",
        hint: "字节码缓存，生产环境建议开启",
        numeric: true,
    },
];

/// 读取某个 ini 键的当前值（返回规范化后的字符串，找不到为 None）
pub fn read_ini_value(paths: &Paths, version: &str, key: &str) -> Option<String> {
    let ini = paths.php_ini(version);
    let content = std::fs::read_to_string(ini).ok()?;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with(';') || t.is_empty() {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim().eq_ignore_ascii_case(key) {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// 写回某个 ini 键（不存在则追加）。用于面板上的快捷开关。
pub fn write_ini_value(paths: &Paths, version: &str, key: &str, value: &str) -> Result<()> {
    let ini = paths.php_ini(version);
    if !ini.is_file() {
        return Err(AppError::new("NO_INI", format!("PHP {version} 的 php.ini 不存在")));
    }
    let content = std::fs::read_to_string(&ini).map_err(|e| AppError::io("读取 php.ini", e))?;
    let mut replaced = false;
    let mut out: Vec<String> = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        if !replaced && !t.starts_with(';') {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    out.push(format!("{key}={value}"));
                    replaced = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    if !replaced {
        out.push(format!("{key}={value}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    write_with_backup(&ini, &s, &paths.backup())?;
    Ok(())
}

/// 判断 ini 值是否为「开」。
/// PHP 接受 On/Off、1/0、true/false、yes/no。
pub fn ini_truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "on" | "true" | "yes"
    )
}

/// 供 UI 显示用的、该版本 php.ini 的真实路径
pub fn ini_path_for(paths: &Paths, version: &str) -> PathBuf {
    paths.php_ini(version)
}

/// 一个便于测试的入口：给定 ini 内容与操作，返回新内容。
pub fn apply_to_content(content: &str, ext: &str, enable: bool) -> String {
    let st = IniExtState::parse(content);
    if enable {
        st.with_enabled(ext)
    } else {
        st.with_disabled(ext)
    }
}

/// 供测试：文件路径 → 扩展名
pub fn ext_name_of(path: &Path) -> Option<String> {
    ext_name_from_file(&path.file_name()?.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_extension() {
        let p = parse_ext_line("extension=curl").unwrap();
        assert_eq!(p.ext, "curl");
        assert!(!p.zend);
        assert!(!p.commented);
    }

    #[test]
    fn parse_commented_extension() {
        let p = parse_ext_line(";extension=gd").unwrap();
        assert_eq!(p.ext, "gd");
        assert!(p.commented);
    }

    #[test]
    fn parse_spaced_and_quoted() {
        let p = parse_ext_line("extension = \"mbstring\"").unwrap();
        assert_eq!(p.ext, "mbstring");
        assert!(!p.commented);
    }

    #[test]
    fn parse_windows_filename_form() {
        // 旧教程常写完整文件名；也要能识别
        let p = parse_ext_line("extension=php_pdo_mysql.dll").unwrap();
        assert_eq!(p.ext, "pdo_mysql");
    }

    #[test]
    fn parse_zend_extension_absolute_path() {
        let p = parse_ext_line(r#"zend_extension="C:\php\ext\php_xdebug.dll""#).unwrap();
        assert_eq!(p.ext, "xdebug");
        assert!(p.zend);
        assert!(!p.commented);
    }

    #[test]
    fn parse_ignores_unrelated_directives() {
        assert!(parse_ext_line("memory_limit=256M").is_none());
        assert!(parse_ext_line("[Extensions]").is_none());
        assert!(parse_ext_line("; just a comment").is_none());
        assert!(parse_ext_line("").is_none());
        assert!(parse_ext_line("extension=").is_none());
    }

    #[test]
    fn enable_uncomments_existing_line() {
        let src = "[Extensions]\n;extension=gd\nextension=curl\n";
        let out = apply_to_content(src, "gd", true);
        assert!(out.contains("\nextension=gd\n"), "实际：{out}");
        assert!(!out.contains(";extension=gd"), "注释未被去掉：{out}");
    }

    #[test]
    fn disable_comments_the_line() {
        let src = "[Extensions]\nextension=curl\n";
        let out = apply_to_content(src, "curl", false);
        assert!(out.contains(";extension=curl"), "实际：{out}");
        assert!(!out.contains("\nextension=curl\n"));
    }

    #[test]
    fn enable_appends_into_extensions_section_not_eof() {
        let src = "engine=On\n\n[Extensions]\nextension=curl\n\n[Session]\nsession.save_handler=files\n";
        let out = apply_to_content(src, "redis", true);
        let ext_pos = out.find("extension=redis").expect("应追加 redis");
        let session_pos = out.find("[Session]").expect("Session 段还在");
        assert!(
            ext_pos < session_pos,
            "redis 必须落在 [Extensions] 段内\n{out}"
        );
    }

    #[test]
    fn enabling_twice_is_idempotent() {
        let src = "[Extensions]\nextension=curl\n";
        let once = apply_to_content(src, "redis", true);
        let twice = apply_to_content(&once, "redis", true);
        assert_eq!(once, twice);
        assert_eq!(twice.matches("extension=redis").count(), 1);
    }

    #[test]
    fn zend_extension_uses_zend_key() {
        let src = "[Extensions]\nextension=curl\n";
        let out = apply_to_content(src, "xdebug", true);
        assert!(
            out.contains("zend_extension=xdebug"),
            "Xdebug 必须是 zend_extension：{out}"
        );
    }

    #[test]
    fn disabling_missing_extension_is_noop_content_wise() {
        let src = "[Extensions]\nextension=curl\n";
        let out = apply_to_content(src, "nothere", false);
        assert!(!out.contains("nothere"));
        assert!(out.contains("extension=curl"));
    }

    #[test]
    fn parse_state_distinguishes_enabled_and_available() {
        let src = "[Extensions]\nextension=curl\n;extension=gd\n;extension=zip\n";
        let st = IniExtState::parse(src);
        assert!(st.enabled.contains("curl"));
        assert!(st.disabled.contains("gd"));
        assert!(st.disabled.contains("zip"));
        assert!(!st.enabled.contains("gd"));
    }

    #[test]
    fn find_section_end_skips_trailing_blanks_before_next_header() {
        let lines: Vec<String> =
            "a=1\n[Extensions]\nx=1\ny=2\n\n[Session]\nz=1\n".lines().map(String::from).collect();
        let idx = find_section_end(&lines, "Extensions").unwrap();
        // 返回的是「新扩展该插在哪」——应落在 y=2 之后、空行之前，
        // 这样新条目紧贴已有扩展，不会隔着空行
        assert_eq!(lines[idx], "", "应指向段尾空行位置，实际 {:?}", lines[idx]);
        assert!(lines[idx - 1].starts_with("y="));
        let mut copy = lines.clone();
        copy.insert(idx, "extension=redis".to_string());
        let joined = copy.join("\n");
        assert!(joined.contains("y=2\nextension=redis"));
        assert!(joined.contains("extension=redis\n\n[Session]"), "得留在 Session 之前：{joined}");
    }

    #[test]
    fn ini_truthy_variants() {
        for v in ["1", "On", "on", "TRUE", "yes"] {
            assert!(ini_truthy(v), "{v} 应判为真");
        }
        for v in ["0", "Off", "false", "no", ""] {
            assert!(!ini_truthy(v), "{v} 应判为假");
        }
    }

    #[test]
    fn ext_name_of_strips_prefix_and_suffix() {
        assert_eq!(ext_name_of(Path::new("/x/php_gd.dll")).unwrap(), "gd");
        assert_eq!(ext_name_of(Path::new("/x/redis.so")).unwrap(), "redis");
        assert_eq!(ext_name_of(Path::new("/x/php_xdebug.dll")).unwrap(), "xdebug");
        assert!(ext_name_of(Path::new("/x/readme.txt")).is_none());
    }

    #[test]
    fn metainfo_covers_common_extensions() {
        for name in ["curl", "xdebug", "redis", "mbstring", "pdo_mysql", "opcache"] {
            let m = meta_for(name).unwrap_or_else(|| panic!("{name} 应有友好名"));
            assert!(!m.group.is_empty() && !m.hint.is_empty());
        }
        assert!(meta_for("some_obscure_ext").is_none());
    }
}
