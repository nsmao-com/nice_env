//! 配置文件编辑：读取 / 校验 / 保存 / 回滚。
//!
//! 用户迟早要手改 nginx.conf、php.ini、my.ini 这些东西——flyenv / ServBay 都把
//! 入口放在「打开所在文件夹」然后让用户拿记事本改，改错了服务起不来只能自己排查。
//!
//! 这里的原则是**改坏之前先拦住**：
//! 1. 保存前做语法校验（nginx 能真跑 `-t`；php.ini / my.ini 做结构自检）；
//! 2. 校验不过就拒绝写入，把原始报错（含行号）回给用户，而不是先写坏再说；
//! 3. 允许「强制保存」——校验器偶尔会误报，不该把用户锁死；
//! 4. 每次保存前自动备份（复用 write_with_backup），并列出可回滚的历史。
//!
//! 只暴露**白名单内**的配置文件，不做成任意文件读写接口。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::paths::Paths;

/// 可编辑的配置文件种类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigKind {
    NginxMain,
    PhpIni,
    MySqlIni,
    RedisConf,
    ApacheConf,
    MihomoConfig,
}

impl ConfigKind {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "nginx-main" | "nginx" => Self::NginxMain,
            "php-ini" | "php" => Self::PhpIni,
            "mysql-ini" | "mysql" => Self::MySqlIni,
            "redis-conf" | "redis" => Self::RedisConf,
            "apache-conf" | "apache" => Self::ApacheConf,
            "mihomo-config" | "mihomo" => Self::MihomoConfig,
            _ => return None,
        })
    }

    pub fn id(&self) -> &'static str {
        match self {
            Self::NginxMain => "nginx-main",
            Self::PhpIni => "php-ini",
            Self::MySqlIni => "mysql-ini",
            Self::RedisConf => "redis-conf",
            Self::ApacheConf => "apache-conf",
            Self::MihomoConfig => "mihomo-config",
        }
    }

    /// 语法高亮用的语言标识（前端 CodeBlock 用）
    pub fn language(&self) -> &'static str {
        match self {
            Self::NginxMain | Self::ApacheConf => "nginx",
            Self::PhpIni => "ini",
            Self::MySqlIni => "ini",
            Self::RedisConf => "conf",
            Self::MihomoConfig => "yaml",
        }
    }

    /// 校验方式：能不能跑真校验器
    pub fn has_validator(&self) -> bool {
        matches!(self, Self::NginxMain | Self::ApacheConf)
    }
}

/// 一个可编辑配置项的元信息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFileInfo {
    pub kind: String,
    /// 展示名，如「Nginx 主配置」
    pub label: String,
    /// 一句话说明这文件管什么
    pub description: String,
    pub path: String,
    pub exists: bool,
    pub size_bytes: u64,
    pub language: String,
    /// 是否有真正的语法校验器（否则只做结构自检）
    pub validated: bool,
    /// 是否正在被运行中的服务使用（改完需要 reload/restart）
    pub used_by_service: Option<String>,
    /// 需要哪个套件才能编辑（未安装时前端提示）
    pub requires_package: Option<String>,
}

/// 一次校验的结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigValidation {
    pub ok: bool,
    /// 校验器的原始输出（成功时也可能有警告）
    #[serde(default)]
    pub messages: Vec<String>,
    /// 结构自检发现的问题（行号 + 说明）
    #[serde(default)]
    pub issues: Vec<ConfigIssue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigIssue {
    /// 1-based 行号；0 表示与具体行无关
    pub line: usize,
    pub severity: String, // error / warning
    pub message: String,
}

/// 解析出配置文件路径。只认白名单，其余一律拒绝。
pub fn resolve_path(paths: &Paths, store: &crate::store::Store, kind: ConfigKind) -> Result<PathBuf> {
    let installed = |id: &str| store.find_installed(id, None).map(|p| p.version);
    let p = match kind {
        ConfigKind::NginxMain => paths.nginx_conf(),
        ConfigKind::PhpIni => {
            let v = installed("php")
                .ok_or_else(|| AppError::not_installed("PHP").with_hint("先到「套件 / 服务」安装一个 PHP 版本"))?;
            paths.php_ini(&v)
        }
        ConfigKind::MySqlIni => {
            let v = installed("mysql")
                .ok_or_else(|| AppError::not_installed("MySQL").with_hint("先到「套件 / 服务」安装 MySQL"))?;
            paths.mysql_ini(&v)
        }
        ConfigKind::RedisConf => {
            let v = installed("redis")
                .ok_or_else(|| AppError::not_installed("Redis").with_hint("先到「套件 / 服务」安装 Redis"))?;
            paths.redis_conf(&v)
        }
        ConfigKind::ApacheConf => paths.apache_conf(),
        ConfigKind::MihomoConfig => {
            if installed("mihomo").is_none() {
                return Err(AppError::not_installed("mihomo")
                    .with_hint("先到「套件 / 服务」安装 mihomo"));
            }
            paths.mihomo_config()
        }
    };
    Ok(p)
}

fn label_of(kind: ConfigKind) -> (&'static str, &'static str, Option<&'static str>) {
    match kind {
        ConfigKind::NginxMain => (
            "Nginx 主配置",
            "站点 vhost 是自动生成的；这里改全局项（worker、日志、gzip 等）",
            Some("nginx"),
        ),
        ConfigKind::PhpIni => (
            "php.ini",
            "PHP 运行时设置。扩展开关建议走「PHP 扩展」面板，那里有主动校验",
            Some("php"),
        ),
        ConfigKind::MySqlIni => (
            "my.ini",
            "MySQL 服务配置（端口、缓冲池、字符集）",
            Some("mysql"),
        ),
        ConfigKind::RedisConf => (
            "redis.conf",
            "Redis 配置（端口、持久化、内存上限）",
            Some("redis"),
        ),
        ConfigKind::ApacheConf => (
            "httpd.conf",
            "Apache 主配置",
            Some("apache"),
        ),
        ConfigKind::MihomoConfig => (
            "config.yaml",
            "Clash / mihomo 配置。用「代理」页导入订阅会覆盖这里",
            Some("mihomo"),
        ),
    }
}

/// 列出所有可编辑配置（按种类，不存在也列出来，前端可显示「尚未生成」）
pub fn list_configs(paths: &Paths, store: &crate::store::Store) -> Vec<ConfigFileInfo> {
    let kinds = [
        ConfigKind::NginxMain,
        ConfigKind::PhpIni,
        ConfigKind::MySqlIni,
        ConfigKind::RedisConf,
        ConfigKind::ApacheConf,
        ConfigKind::MihomoConfig,
    ];
    kinds
        .iter()
        .filter_map(|k| {
            let path = resolve_path(paths, store, *k).ok()?;
            let (label, description, pkg) = label_of(*k);
            let meta = std::fs::metadata(&path).ok();
            Some(ConfigFileInfo {
                kind: k.id().to_string(),
                label: label.to_string(),
                description: description.to_string(),
                path: path.to_string_lossy().to_string(),
                exists: meta.is_some(),
                size_bytes: meta.map(|m| m.len()).unwrap_or(0),
                language: k.language().to_string(),
                validated: k.has_validator(),
                used_by_service: pkg.map(|s| s.to_string()),
                requires_package: pkg.map(|s| s.to_string()),
            })
        })
        .collect()
}

/// 读取配置内容
pub fn read_config(paths: &Paths, store: &crate::store::Store, kind: ConfigKind) -> Result<String> {
    let path = resolve_path(paths, store, kind)?;
    if !path.is_file() {
        return Err(AppError::new(
            "CONFIG_NOT_GENERATED",
            "该配置文件还没生成",
        )
        .with_hint("先启动一次对应的服务，配置会自动生成"));
    }
    std::fs::read_to_string(&path).map_err(|e| AppError::io("读取配置文件", e))
}

/* ================= 结构自检 ================= */
//
// 只做「不依赖外部程序也能发现」的问题。目标是拦住最常见的手滑，
// 而不是当一个完整的语法分析器——那正是真正校验器的活。

/// ini 风格自检：PHP 的 php.ini 与 MySQL 的 my.ini 都用这个
pub fn lint_ini(content: &str) -> Vec<ConfigIssue> {
    let mut issues = Vec::new();
    let mut section_seen = false;
    for (i, raw) in content.lines().enumerate() {
        let line_no = i + 1;
        let t = raw.trim();
        if t.is_empty() || t.starts_with(';') || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') {
            if !t.ends_with(']') {
                issues.push(ConfigIssue {
                    line: line_no,
                    severity: "error".into(),
                    message: "段名缺少右方括号 ]".into(),
                });
            } else if t.len() < 3 {
                issues.push(ConfigIssue {
                    line: line_no,
                    severity: "error".into(),
                    message: "段名为空".into(),
                });
            }
            section_seen = true;
            continue;
        }
        // 非段、非注释的行必须是 key=value
        if !t.contains('=') {
            issues.push(ConfigIssue {
                line: line_no,
                severity: if section_seen { "error" } else { "warning" }.into(),
                message: format!("不是 key=value 形式，也不是注释：{t}"),
            });
            continue;
        }
        let (k, v) = t.split_once('=').unwrap();
        if k.trim().is_empty() {
            issues.push(ConfigIssue {
                line: line_no,
                severity: "error".into(),
                message: "键名为空".into(),
            });
        }
        // 引号必须成对，否则 PHP/MySQL 会静默截断值
        let dq = v.matches('"').count();
        if dq % 2 != 0 {
            issues.push(ConfigIssue {
                line: line_no,
                severity: "error".into(),
                message: "双引号没有闭合".into(),
            });
        }
    }
    issues
}

/// ini 风格：分号不是注释 —— 检查「用错注释符」这个高频错误
/// （nginx 用 #，ini 用 ;，yaml 用 #。混用会让配置静默失效）
pub fn lint_ini_comment_style(content: &str) -> Vec<ConfigIssue> {
    let mut issues = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        let t = raw.trim();
        // `# key=value` 在 ini 里是**非法**的（PHP 会忽略，MySQL 直接报错）
        if t.starts_with('#') && t.contains('=') {
            issues.push(ConfigIssue {
                line: i + 1,
                severity: "warning".into(),
                message: "ini 文件的注释符是分号 ; —— 用 # 开头的配置行不会生效".into(),
            });
        }
    }
    issues
}

/// nginx 自检：括号配对 + 指令行必须以 ; 或 { } 收尾
pub fn lint_nginx(content: &str) -> Vec<ConfigIssue> {
    let mut issues = Vec::new();
    let mut depth: i32 = 0;
    for (i, raw) in content.lines().enumerate() {
        let line_no = i + 1;
        // 去掉注释后再判断（# 到行尾），避免注释里的花括号干扰
        // 注意用索引切片而不是 raw.as_str()：后者在稳定版 Rust 上不可用
        let no_comment = match raw.find('#') {
            Some(pos) => &raw[..pos],
            None => &raw[..],
        };
        let t = no_comment.trim();
        if t.is_empty() {
            continue;
        }
        // 引号未闭合会让括号计数失准，先单独报
        if t.matches('"').count() % 2 != 0 {
            issues.push(ConfigIssue {
                line: line_no,
                severity: "error".into(),
                message: "双引号没有闭合".into(),
            });
        }
        depth += t.matches('{').count() as i32;
        depth -= t.matches('}').count() as i32;
        if depth < 0 {
            issues.push(ConfigIssue {
                line: line_no,
                severity: "error".into(),
                message: "多余的右花括号 }".into(),
            });
            depth = 0;
        }
        // 指令行应以 ; 或 { 或 } 结尾
        let last = t.chars().last().unwrap_or(' ');
        if !matches!(last, ';' | '{' | '}') {
            issues.push(ConfigIssue {
                line: line_no,
                severity: "error".into(),
                message: format!("指令行缺少结尾分号 ;：{t}"),
            });
        }
    }
    if depth != 0 {
        issues.push(ConfigIssue {
            line: 0,
            severity: "error".into(),
            message: format!("花括号没有配平：还差 {depth} 个右花括号 }}"),
        });
    }
    issues
}

/// yaml 自检：只查最常见的缩进用 Tab（YAML 明确禁止）
pub fn lint_yaml(content: &str) -> Vec<ConfigIssue> {
    let mut issues = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        if raw.starts_with('\t') {
            issues.push(ConfigIssue {
                line: i + 1,
                severity: "error".into(),
                message: "YAML 不允许用 Tab 缩进，请改成空格".into(),
            });
        }
        // 缩进必须是 2 的倍数（mihomo 的约定）
        let indent = raw.len() - raw.trim_start().len();
        if indent > 0 && indent % 2 != 0 && !raw.starts_with('\t') {
            issues.push(ConfigIssue {
                line: i + 1,
                severity: "warning".into(),
                message: format!("缩进 {indent} 格不是 2 的倍数，YAML 层级可能读错"),
            });
        }
    }
    issues
}

/// 按种类选择自检器
pub fn lint(kind: ConfigKind, content: &str) -> Vec<ConfigIssue> {
    match kind {
        ConfigKind::NginxMain | ConfigKind::ApacheConf => lint_nginx(content),
        ConfigKind::PhpIni | ConfigKind::MySqlIni => {
            let mut v = lint_ini(content);
            v.extend(lint_ini_comment_style(content));
            v
        }
        ConfigKind::RedisConf => {
            // redis.conf 用的是空格分隔的指令，没有 = ，所以不能套 ini 规则
            let mut issues = Vec::new();
            for (i, raw) in content.lines().enumerate() {
                let t = raw.trim();
                if t.is_empty() || t.starts_with('#') {
                    continue;
                }
                if t.matches('"').count() % 2 != 0 {
                    issues.push(ConfigIssue {
                        line: i + 1,
                        severity: "error".into(),
                        message: "双引号没有闭合".into(),
                    });
                }
            }
            issues
        }
        ConfigKind::MihomoConfig => lint_yaml(content),
    }
}

/// 校验配置内容：先自检，再（能跑真校验器的话）跑一次。
///
/// 校验是**在临时文件上**做的，绝不写进真实路径——这样「校验失败」
/// 完全不会影响正在运行的配置。
pub fn validate(
    paths: &Paths,
    store: &crate::store::Store,
    kind: ConfigKind,
    content: &str,
) -> Result<ConfigValidation> {
    let mut issues = lint(kind, content);
    let mut messages: Vec<String> = Vec::new();

    // 有自检错误就不必再跑外部校验器了
    let has_error = issues.iter().any(|i| i.severity == "error");
    if has_error {
        return Ok(ConfigValidation {
            ok: false,
            messages,
            issues,
        });
    }

    if kind.has_validator() {
        match kind {
            ConfigKind::NginxMain => {
                if let Some(exe) = nginx_exe(paths, store) {
                    // 临时文件必须与真实配置同目录：nginx 的相对路径 include 才成立
                    let tmp = paths.etc().join("nginx").join(".nsb-validate.conf");
                    std::fs::create_dir_all(paths.etc().join("nginx")).ok();
                    std::fs::write(&tmp, content).map_err(|e| AppError::io("写入临时校验文件", e))?;
                    let out = std::process::Command::new(&exe)
                        .arg("-t")
                        .arg("-c")
                        .arg(&tmp)
                        .output();
                    let _ = std::fs::remove_file(&tmp);
                    match out {
                        Ok(o) => {
                            let text = format!(
                                "{}{}",
                                String::from_utf8_lossy(&o.stdout),
                                String::from_utf8_lossy(&o.stderr)
                            );
                            let failed = !o.status.success();
                            for l in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                                messages.push(l.to_string());
                                // 从 nginx 报错里抠出行号，前端能直接跳到那一行
                                if let Some(n) = parse_nginx_line_no(l) {
                                    issues.push(ConfigIssue {
                                        line: n,
                                        severity: "error".into(),
                                        message: l.to_string(),
                                    });
                                }
                            }
                            if failed && !issues.iter().any(|i| i.severity == "error") {
                                issues.push(ConfigIssue {
                                    line: 0,
                                    severity: "error".into(),
                                    message: "nginx -t 校验未通过".into(),
                                });
                            }
                        }
                        Err(e) => messages.push(format!("无法运行 nginx -t：{e}")),
                    }
                } else {
                    messages.push("未安装 nginx，跳过 nginx -t 校验".into());
                }
            }
            ConfigKind::ApacheConf => {
                // Apache 的 -t 需要完整环境变量，Windows 上常常误报；
                // 这里只做结构自检，真实生效靠重启时的报错。
                messages.push("Apache 语法在重启服务时会由 httpd 自己校验".into());
            }
            _ => {}
        }
    }

    let ok = !issues.iter().any(|i| i.severity == "error");
    Ok(ConfigValidation {
        ok,
        messages,
        issues,
    })
}

/// 从 nginx 报错里解析行号。
/// 形如：`nginx: [emerg] unknown directive "xxx" in /path/nginx.conf:42`
pub fn parse_nginx_line_no(line: &str) -> Option<usize> {
    let idx = line.rfind(':')?;
    let tail = line[idx + 1..].trim();
    tail.parse::<usize>().ok().filter(|n| *n > 0)
}

fn nginx_exe(paths: &Paths, store: &crate::store::Store) -> Option<PathBuf> {
    let v = store.find_installed("nginx", None)?.version;
    let exe = paths
        .runtime_dir("nginx", &v)
        .join(crate::ops::exe_name("nginx"));
    exe.is_file().then_some(exe)
}

/// 保存配置：先校验，通过了才写；不通过要么拒绝，要么（force）带备份写入。
///
/// force 的用途：校验器偶有误报（比如 nginx -t 对某些第三方模块），
/// 这时不该把用户彻底锁死 —— 但必须让他明确知道「你在跳过校验」。
pub fn save_config(
    paths: &Paths,
    store: &crate::store::Store,
    kind: ConfigKind,
    content: &str,
    force: bool,
) -> Result<ConfigValidation> {
    let v = validate(paths, store, kind, content)?;
    if !v.ok && !force {
        return Err(AppError::new("CONFIG_INVALID", "配置校验未通过，未写入")
            .with_hint("按提示改好再保存；确实需要跳过校验可以用「强制保存」")
            .with_detail(
                v.issues
                    .iter()
                    .take(5)
                    .map(|i| {
                        if i.line > 0 {
                            format!("第 {} 行：{}", i.line, i.message)
                        } else {
                            i.message.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ));
    }

    let path = resolve_path(paths, store, kind)?;
    // 写前备份，用户改坏了可以从历史里回滚
    let backup_dir = paths.backup().join("config");
    std::fs::create_dir_all(&backup_dir).ok();
    crate::paths::write_with_backup(&path, content, &backup_dir)
        .map_err(|e| AppError::io("写入配置文件", e))?;
    Ok(v)
}

/// 配置历史备份（供回滚）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigBackup {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub created_at: i64,
}

pub fn list_config_backups(paths: &Paths) -> Vec<ConfigBackup> {
    let dir = paths.backup().join("config");
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if !name.ends_with(".bak") {
            continue;
        }
        let meta = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push(ConfigBackup {
            name,
            path: p.to_string_lossy().to_string(),
            size_bytes: meta.len(),
            created_at: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        });
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out.truncate(30);
    out
}

/// 回滚到某个备份。目标由备份文件名反推（去掉 .时间戳.bak）。
pub fn rollback_config(paths: &Paths, store: &crate::store::Store, backup_name: &str) -> Result<()> {
    let dir = paths.backup().join("config");
    let src = dir.join(backup_name);
    // 防路径穿越：文件名里不能带分隔符
    if backup_name.contains('/') || backup_name.contains('\\') || backup_name.contains("..") {
        return Err(AppError::new("FORBIDDEN", "非法的备份文件名"));
    }
    if !src.is_file() {
        return Err(AppError::new("FILE_NOT_FOUND", "备份文件不存在"));
    }
    // 反推原始文件名：{name}.{YYYYmmdd-HHMMSS}.bak
    let stem = backup_name.trim_end_matches(".bak");
    let orig = stem
        .rsplit_once('.')
        .map(|(a, _)| a.to_string())
        .ok_or_else(|| AppError::new("BAD_BACKUP", "备份文件名格式不对"))?;

    // 只允许回滚到白名单里真实存在的配置路径
    let target = list_configs(paths, store)
        .into_iter()
        .find(|c| {
            Path::new(&c.path)
                .file_name()
                .map(|f| f.to_string_lossy() == orig)
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            AppError::new("FORBIDDEN", "该备份不对应任何可编辑的配置文件")
        })?;

    let content = std::fs::read_to_string(&src).map_err(|e| AppError::io("读取备份", e))?;
    // 回滚前把当前内容也存一份，免得回滚本身变成不可逆操作
    crate::paths::write_with_backup(Path::new(&target.path), &content, &dir)
        .map_err(|e| AppError::io("回写配置", e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_kind_parses_ids_and_aliases() {
        assert_eq!(ConfigKind::parse("nginx-main"), Some(ConfigKind::NginxMain));
        assert_eq!(ConfigKind::parse("nginx"), Some(ConfigKind::NginxMain));
        assert_eq!(ConfigKind::parse("php-ini"), Some(ConfigKind::PhpIni));
        assert_eq!(ConfigKind::parse("php"), Some(ConfigKind::PhpIni));
        assert_eq!(ConfigKind::parse("mihomo"), Some(ConfigKind::MihomoConfig));
        assert_eq!(ConfigKind::parse("../../etc/passwd"), None);
        assert_eq!(ConfigKind::parse(""), None);
    }

    #[test]
    fn ini_lint_catches_missing_closing_bracket() {
        let issues = lint_ini("[mysqld\nport=3306\n");
        assert!(issues.iter().any(|i| i.message.contains("右方括号")));
    }

    #[test]
    fn ini_lint_catches_unbalanced_quotes() {
        let issues = lint_ini("[mysqld]\ndatadir=\"C:/data\n");
        assert!(issues.iter().any(|i| i.message.contains("双引号")));
    }

    #[test]
    fn ini_lint_errors_on_non_kv_line() {
        let issues = lint_ini("[mysqld]\nthis is not kv\n");
        let e = issues.iter().find(|i| i.line == 2).unwrap();
        assert_eq!(e.severity, "error");
    }

    #[test]
    fn ini_lint_accepts_valid_file() {
        let issues = lint_ini("; comment\n[mysqld]\nport=3306\ndatadir=\"C:/data\"\n");
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn ini_lint_ignores_comments_and_blanks() {
        let issues = lint_ini("# hash comment\n\n; semi comment\n[a]\nk=v\n");
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn ini_lint_warns_about_hash_comment_style() {
        // ini 里用 # 注释是无效的，应给警告
        let issues = lint_ini_comment_style("# port=3306\nport=3307\n");
        assert!(issues.iter().any(|i| i.message.contains("分号")));
    }

    #[test]
    fn ini_hash_without_equals_is_not_flagged() {
        // 纯文字标题（没有 =）不必报警
        let issues = lint_ini_comment_style("# My config\nport=3306\n");
        assert!(issues.is_empty());
    }

    #[test]
    fn nginx_lint_catches_missing_semicolon() {
        let issues = lint_nginx("events {}\nhttp {\n  server {\n    listen 80\n  }\n}\n");
        assert!(
            issues.iter().any(|i| i.message.contains("分号")),
            "应发现缺少分号：{issues:?}"
        );
    }

    #[test]
    fn nginx_lint_catches_unbalanced_braces() {
        let issues = lint_nginx("http {\n  server {\n    listen 80;\n  }\n");
        assert!(issues.iter().any(|i| i.message.contains("配平")));
    }

    #[test]
    fn nginx_lint_catches_extra_closing_brace() {
        let issues = lint_nginx("}\n");
        assert!(issues.iter().any(|i| i.message.contains("多余")));
    }

    #[test]
    fn nginx_lint_ignores_braces_in_comments() {
        // 注释里的花括号不应该影响配平
        let issues = lint_nginx("http {\n  # note: use { } carefully\n  server { listen 80; }\n}\n");
        assert!(issues.is_empty(), "注释里的花括号不该干扰：{issues:?}");
    }

    #[test]
    fn nginx_lint_accepts_valid_config() {
        let good = "events { worker_connections 1024; }\nhttp {\n  server {\n    listen 80;\n  }\n}\n";
        assert!(lint_nginx(good).is_empty());
    }

    #[test]
    fn nginx_lint_catches_unclosed_quote() {
        let issues = lint_nginx("http {\n  server_name \"a.com;\n}\n");
        assert!(issues.iter().any(|i| i.message.contains("双引号")));
    }

    #[test]
    fn parse_nginx_line_no_extracts_number() {
        assert_eq!(
            parse_nginx_line_no(r#"nginx: [emerg] unknown directive "xx" in C:\etc\nginx.conf:42"#),
            Some(42)
        );
        assert_eq!(parse_nginx_line_no("nginx: configuration file test is successful"), None);
        assert_eq!(parse_nginx_line_no(""), None);
    }

    #[test]
    fn yaml_lint_catches_tab_indent() {
        let issues = lint_yaml("mixed-port: 17890\nproxies:\n\t- name: a\n");
        assert!(issues.iter().any(|i| i.message.contains("Tab")));
    }

    #[test]
    fn yaml_lint_warns_on_odd_indent() {
        let issues = lint_yaml("a:\n   b: 1\n");
        assert!(issues.iter().any(|i| i.severity == "warning"));
    }

    #[test]
    fn yaml_lint_accepts_clean_yaml() {
        assert!(lint_yaml("mixed-port: 17890\nproxies:\n  - name: a\n").is_empty());
    }

    #[test]
    fn redis_lint_catches_unclosed_quote() {
        let issues = lint(ConfigKind::RedisConf, "port 6379\nrequirepass \"abc\n");
        assert!(issues.iter().any(|i| i.message.contains("双引号")));
    }

    #[test]
    fn redis_lint_allows_space_separated_directives() {
        // redis.conf 不是 ini，没有 = 号也不该报错
        let issues = lint(ConfigKind::RedisConf, "port 6379\nmaxmemory 256mb\nsave 900 1\n");
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn validate_reports_not_ok_on_lint_error() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-val-1"));
        let store = crate::store::Store::open(paths.base.join("s.sqlite")).unwrap();
        let v = validate(&paths, &store, ConfigKind::NginxMain, "http {\n listen 80\n}\n").unwrap();
        assert!(!v.ok);
        assert!(!v.issues.is_empty());
    }

    #[test]
    fn validate_ok_on_clean_content() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-val-2"));
        let store = crate::store::Store::open(paths.base.join("s.sqlite")).unwrap();
        let v = validate(
            &paths,
            &store,
            ConfigKind::NginxMain,
            "events {}\nhttp { server { listen 80; } }\n",
        )
        .unwrap();
        assert!(v.ok, "{:?}", v.issues);
    }

    #[test]
    fn save_refuses_invalid_without_force() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-val-3"));
        let store = crate::store::Store::open(paths.base.join("s.sqlite")).unwrap();
        let r = save_config(&paths, &store, ConfigKind::NginxMain, "http {\n", false);
        // 路径不存在时会先用 resolve_path 失败，或校验失败 —— 两种情况都不该成功写入
        assert!(r.is_err());
    }

    #[test]
    fn rollback_rejects_path_traversal() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-val-4"));
        let store = crate::store::Store::open(paths.base.join("s.sqlite")).unwrap();
        for bad in ["../../etc/passwd", "..\\win.ini", "a/b.bak"] {
            let r = rollback_config(&paths, &store, bad);
            assert!(r.is_err(), "{bad} 应被拒绝");
        }
    }

    #[test]
    fn list_config_backups_empty_when_no_dir() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-val-5-nonexistent"));
        assert!(list_config_backups(&paths).is_empty());
    }

    #[test]
    fn label_of_covers_all_kinds() {
        for k in [
            ConfigKind::NginxMain,
            ConfigKind::PhpIni,
            ConfigKind::MySqlIni,
            ConfigKind::RedisConf,
            ConfigKind::ApacheConf,
            ConfigKind::MihomoConfig,
        ] {
            let (l, d, _) = label_of(k);
            assert!(!l.is_empty() && !d.is_empty(), "{k:?} 缺展示信息");
            assert!(!k.language().is_empty());
        }
    }
}
