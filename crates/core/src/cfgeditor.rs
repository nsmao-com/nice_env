//! 配置文件编辑：读取 / 校验 / 保存 / 回滚。
//!
//! 按套件与版本读取配置，保存前校验，历史备份绑定准确的目标文件。
//!
//! 这里的原则是**改坏之前先拦住**：
//! 1. 保存前做语法校验（nginx 能真跑 `-t`；php.ini / my.ini 做结构自检）；
//! 2. 校验不过就拒绝写入，把原始报错（含行号）回给用户，而不是先写坏再说；
//! 3. 允许「强制保存」——校验器偶尔会误报，不该把用户锁死；
//! 4. 每次保存前自动备份，原子替换文件，并列出按配置与版本隔离的历史。
//!
//! 只暴露**白名单内**的配置文件，不做成任意文件读写接口。

use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
            Self::NginxMain => "nginx",
            Self::ApacheConf => "apache",
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

#[derive(Clone)]
struct ConfigTarget {
    kind: ConfigKind,
    version: Option<String>,
}

impl ConfigTarget {
    fn parse(key: &str) -> Result<Self> {
        let (kind, version) = key
            .split_once('@')
            .map_or((key, None), |(k, v)| (k, Some(v)));
        let kind =
            ConfigKind::parse(kind).ok_or_else(|| AppError::new("BAD_KIND", "未知的配置类型"))?;
        if let Some(version) = version {
            if !matches!(
                kind,
                ConfigKind::PhpIni | ConfigKind::MySqlIni | ConfigKind::RedisConf
            ) || version.is_empty()
                || version.ends_with('.')
                || !version
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._+-".contains(&c))
            {
                return Err(AppError::new("BAD_CONFIG_VERSION", "配置版本无效"));
            }
        }
        Ok(Self {
            kind,
            version: version.map(str::to_owned),
        })
    }

    fn selected(mut self, store: &crate::store::Store) -> Result<Self> {
        if matches!(
            self.kind,
            ConfigKind::PhpIni | ConfigKind::MySqlIni | ConfigKind::RedisConf
        ) {
            let id = label_of(self.kind).2.unwrap();
            let package = match &self.version {
                Some(version) => store.find_installed(id, Some(version)),
                None => crate::ops::installed_by_choice(store, id),
            }
            .ok_or_else(|| AppError::not_installed(id))?;
            self.version = Some(package.version);
            Self::parse(&self.key())?;
        }
        Ok(self)
    }

    fn key(&self) -> String {
        self.version.as_ref().map_or_else(
            || self.kind.id().to_string(),
            |v| format!("{}@{v}", self.kind.id()),
        )
    }

    fn path(&self, paths: &Paths, store: &crate::store::Store) -> Result<PathBuf> {
        let target = self.clone().selected(store)?;
        Ok(match target.kind {
            ConfigKind::NginxMain => paths.nginx_conf(),
            ConfigKind::PhpIni => paths.php_ini(target.version.as_deref().unwrap()),
            ConfigKind::MySqlIni => paths.mysql_ini(target.version.as_deref().unwrap()),
            ConfigKind::RedisConf => paths.redis_conf(target.version.as_deref().unwrap()),
            ConfigKind::ApacheConf => paths.apache_conf(),
            ConfigKind::MihomoConfig => {
                if crate::ops::installed_by_choice(store, "mihomo").is_none() {
                    return Err(AppError::not_installed("mihomo"));
                }
                paths.mihomo_config()
            }
        })
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
    #[serde(default)]
    pub resettable: bool,
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
pub fn resolve_path(
    paths: &Paths,
    store: &crate::store::Store,
    kind: ConfigKind,
) -> Result<PathBuf> {
    ConfigTarget {
        kind,
        version: None,
    }
    .path(paths, store)
}

fn label_of(kind: ConfigKind) -> (&'static str, &'static str, Option<&'static str>) {
    match kind {
        ConfigKind::NginxMain => (
            "Nginx 主配置",
            "自定义全局设置在重启后保留；端口、默认站点、PHP 连接池和站点入口由应用维护",
            Some("nginx"),
        ),
        ConfigKind::PhpIni => (
            "php.ini",
            "PHP 运行时设置。扩展开关建议走「PHP 扩展」面板，那里有主动校验",
            Some("php"),
        ),
        ConfigKind::MySqlIni => (
            "my.ini",
            "自定义参数在重启后保留；运行目录和数据目录由应用维护，端口请在设置页修改",
            Some("mysql"),
        ),
        ConfigKind::RedisConf => (
            "redis.conf",
            "内存、持久化等设置在重启后保留；端口、数据目录和前台运行方式由应用维护",
            Some("redis"),
        ),
        ConfigKind::ApacheConf => (
            "httpd.conf",
            "自定义模块与全局设置在重启后保留；运行目录、监听端口和站点入口由应用维护",
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
    let installed = store.list_installed().unwrap_or_default();
    kinds
        .into_iter()
        .flat_map(|kind| {
            if matches!(
                kind,
                ConfigKind::PhpIni | ConfigKind::MySqlIni | ConfigKind::RedisConf
            ) {
                let id = label_of(kind).2.unwrap();
                let mut targets: Vec<_> = installed
                    .iter()
                    .filter(|p| p.id == id)
                    .map(|p| ConfigTarget {
                        kind,
                        version: Some(p.version.clone()),
                    })
                    .collect();
                targets.sort_by(|a, b| {
                    crate::versions::cmp_version_desc(
                        a.version.as_deref().unwrap(),
                        b.version.as_deref().unwrap(),
                    )
                });
                targets
            } else {
                vec![ConfigTarget {
                    kind,
                    version: None,
                }]
            }
        })
        .filter_map(|target| {
            let path = target.path(paths, store).ok()?;
            let (label, description, pkg) = label_of(target.kind);
            let meta = std::fs::metadata(&path).ok();
            Some(ConfigFileInfo {
                kind: target.key(),
                label: target
                    .version
                    .as_ref()
                    .map_or_else(|| label.to_string(), |v| format!("{label} · {v}")),
                description: description.to_string(),
                path: path.to_string_lossy().to_string(),
                exists: meta.as_ref().is_some_and(|m| m.is_file()),
                size_bytes: meta.map(|m| m.len()).unwrap_or(0),
                language: target.kind.language().to_string(),
                validated: match target.kind {
                    ConfigKind::NginxMain => crate::ops::nginx_exe(store).is_ok(),
                    ConfigKind::ApacheConf => crate::ops::apache_paths(store).is_ok(),
                    _ => false,
                },
                used_by_service: pkg.map(|s| match &target.version {
                    Some(v) if matches!(target.kind, ConfigKind::PhpIni | ConfigKind::MySqlIni) => {
                        format!("{s}@{v}")
                    }
                    _ => s.to_string(),
                }),
                requires_package: pkg.map(|s| s.to_string()),
                resettable: target.kind != ConfigKind::MihomoConfig
                    && pkg.is_some_and(|id| installed.iter().any(|package| package.id == id)),
            })
        })
        .collect()
}

/// 读取配置内容
pub fn read_config(paths: &Paths, store: &crate::store::Store, kind: ConfigKind) -> Result<String> {
    read_config_selected(paths, store, kind.id())
}

pub fn read_config_selected(
    paths: &Paths,
    store: &crate::store::Store,
    key: &str,
) -> Result<String> {
    let path = ConfigTarget::parse(key)?.path(paths, store)?;
    if !path.is_file() {
        return Err(AppError::new("CONFIG_NOT_GENERATED", "该配置文件还没生成")
            .with_hint("先启动一次对应的服务，配置会自动生成"));
    }
    std::fs::read_to_string(&path).map_err(|e| AppError::io("读取配置文件", e))
}

/* ================= 结构自检 ================= */

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigResetPreview {
    pub kind: String,
    pub label: String,
    pub path: String,
    pub content: String,
    pub language: String,
    pub current_exists: bool,
    pub changed: bool,
    pub revision: String,
    pub used_by_service: Option<String>,
}

pub fn preview_config_reset(
    paths: &Paths,
    store: &crate::store::Store,
    key: &str,
) -> Result<ConfigResetPreview> {
    let target = ConfigTarget::parse(key)?.selected(store)?;
    let path = target.path(paths, store)?;
    let relative = path
        .strip_prefix(&paths.base)
        .map_err(|_| AppError::new("BAD_CONFIG_PATH", "配置目录无效"))?;
    crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
    let ports = crate::services::PortsProfile::from_settings(store);
    let mut pools: Vec<_> = store
        .all_port_assigns()
        .into_iter()
        .filter_map(|(id, port)| id.strip_prefix("php@").map(|v| (v.to_string(), port)))
        .collect();
    pools.sort_by(|a, b| a.0.cmp(&b.0));
    let content = match target.kind {
        ConfigKind::NginxMain => {
            let (root, _) = crate::ops::nginx_exe(store)?;
            crate::configgen::render_nginx_conf(
                paths,
                &root,
                ports.http,
                ports.https,
                &pools,
                crate::configgen::adminer_path(paths).as_deref(),
            )
        }
        ConfigKind::ApacheConf => {
            let (root, _) = crate::ops::apache_paths(store)?;
            crate::configgen::render_httpd_conf(
                paths,
                &root,
                &pools,
                ports.apache_http,
                ports.apache_https,
            )
        }
        ConfigKind::PhpIni => {
            let version = target.version.as_deref().unwrap();
            crate::configgen::render_php_ini(paths, version, &paths.runtime_dir("php", version))
        }
        ConfigKind::MySqlIni => {
            let version = target.version.as_deref().unwrap();
            let package = store
                .find_installed("mysql", Some(version))
                .ok_or_else(|| AppError::not_installed("MySQL"))?;
            let root =
                PathBuf::from(package.install_path).join(crate::ops::mysql_root_name(version));
            crate::configgen::render_mysql_ini(paths, version, &root, ports.mysql)
        }
        ConfigKind::RedisConf => crate::configgen::render_redis_conf(
            paths,
            target.version.as_deref().unwrap(),
            ports.redis,
        ),
        ConfigKind::MihomoConfig => {
            return Err(AppError::new("BAD_KIND", "代理配置请在代理页面管理订阅"))
        }
    };
    let current = match std::fs::read(&path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let info = list_configs(paths, store)
        .into_iter()
        .find(|info| info.kind == target.key())
        .ok_or_else(|| AppError::new("BAD_KIND", "找不到对应配置"))?;
    let revision = hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            target.key(),
            &path,
            current
                .as_ref()
                .map(|value| hex::encode(Sha256::digest(value))),
            &content,
        ))
        .map_err(std::io::Error::other)?,
    ));
    Ok(ConfigResetPreview {
        kind: target.key(),
        label: info.label,
        path: path.to_string_lossy().into(),
        current_exists: current.is_some(),
        changed: current.as_deref() != Some(content.as_bytes()),
        language: info.language,
        used_by_service: info.used_by_service,
        content,
        revision,
    })
}

pub fn reset_config(
    paths: &Paths,
    store: &crate::store::Store,
    key: &str,
    revision: &str,
) -> Result<ConfigResetPreview> {
    let target = ConfigTarget::parse(key)?.selected(store)?;
    let path = target.path(paths, store)?;
    let relative = path
        .strip_prefix(&paths.base)
        .map_err(|_| AppError::new("BAD_CONFIG_PATH", "配置目录无效"))?;
    crate::paths::checked_data_path(&paths.base, &crate::paths::nginx_path(relative))?;
    let expected = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let preview = preview_config_reset(paths, store, key)?;
    if preview.revision != revision {
        return Err(AppError::new(
            "CONFIG_CONFLICT",
            "配置或服务设置已变化，请重新预览后重置",
        ));
    }
    write_config_version_checked(
        paths,
        &target,
        &path,
        &preview.content,
        Some(expected.as_deref()),
    )?;
    preview_config_reset(paths, store, key)
}

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
        ConfigKind::NginxMain => lint_nginx(content),
        // Apache 不使用分号或花括号；真实语法由 httpd -t 校验。
        ConfigKind::ApacheConf => Vec::new(),
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
    let runtime = match kind {
        ConfigKind::NginxMain => Some(crate::ops::nginx_exe(store)),
        ConfigKind::ApacheConf => Some(crate::ops::apache_paths(store)),
        _ => None,
    };
    // 有真实校验器时以其解析结果为准，避免多行指令、正则或引号内 # 被简易检查误判。
    let mut issues = if matches!(&runtime, Some(Ok(_))) {
        Vec::new()
    } else {
        lint(kind, content)
    };
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

    if let Some(runtime) = runtime {
        match runtime {
            Ok((root, exe)) => {
                let dir = resolve_path(paths, store, kind)?
                    .parent()
                    .unwrap()
                    .to_path_buf();
                std::fs::create_dir_all(&dir)?;
                let mut tmp = tempfile::Builder::new()
                    .prefix(".nsb-validate-")
                    .suffix(".conf")
                    .tempfile_in(&dir)?;
                tmp.write_all(content.as_bytes())?;
                tmp.flush()?;
                let mut command = platform::command(&exe);
                command.current_dir(&root).arg("-t");
                if kind == ConfigKind::NginxMain {
                    command.arg("-p").arg(&root).arg("-c").arg(tmp.path());
                } else {
                    command.arg("-d").arg(&root).arg("-f").arg(tmp.path());
                }
                match run_validator(&mut command) {
                    Ok((ok, output)) => {
                        for line in output.lines().map(str::trim).filter(|l| !l.is_empty()) {
                            messages.push(line.to_string());
                            let is_current = line
                                .replace('\\', "/")
                                .contains(&tmp.path().to_string_lossy().replace('\\', "/"));
                            if !ok || line.contains("[warn]") {
                                issues.push(ConfigIssue {
                                    line: if is_current {
                                        parse_nginx_line_no(line).unwrap_or(0)
                                    } else {
                                        0
                                    },
                                    severity: if ok { "warning" } else { "error" }.into(),
                                    message: line.to_string(),
                                });
                            }
                        }
                        if !ok && issues.is_empty() {
                            issues.push(ConfigIssue {
                                line: 0,
                                severity: "error".into(),
                                message: "服务原生配置校验未通过".into(),
                            });
                        }
                    }
                    Err(err) => issues.push(ConfigIssue {
                        line: 0,
                        severity: "error".into(),
                        message: err.message,
                    }),
                }
            }
            Err(err) if err.code == "NOT_INSTALLED" => {
                messages.push("未安装对应服务，未执行原生语法校验；当前结果仅包含基础检查".into());
            }
            Err(err) => issues.push(ConfigIssue {
                line: 0,
                severity: "error".into(),
                message: err.message,
            }),
        }
    } else {
        messages.push("已完成基础格式检查；完整配置是否生效需由对应服务确认".into());
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

pub(crate) fn run_validator(command: &mut std::process::Command) -> Result<(bool, String)> {
    let mut output = tempfile::tempfile()?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output.try_clone()?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(platform::spawn_pre_exec);
        }
    }
    let mut group = platform::ProcessGroup::new()?;
    let mut child = command
        .spawn()
        .map_err(|e| AppError::io("运行配置校验器", e))?;
    if let Err(e) = group.attach(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.into());
    }
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < std::time::Duration::from_secs(15) => {
                std::thread::sleep(std::time::Duration::from_millis(40))
            }
            Ok(None) => {
                break Err(AppError::new(
                    "VALIDATION_TIMEOUT",
                    "配置校验超时，已停止校验进程",
                ))
            }
            Err(e) => break Err(AppError::io("等待配置校验器", e)),
        }
    };
    if status.is_err() {
        let _ = group.terminate(true);
        let _ = child.kill();
    }
    let _ = child.wait();
    let status = status?;
    output.rewind()?;
    let mut bytes = Vec::new();
    output.take(128 * 1024).read_to_end(&mut bytes)?;
    Ok((
        status.success(),
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
}

pub fn validate_selected(
    paths: &Paths,
    store: &crate::store::Store,
    key: &str,
    content: &str,
) -> Result<ConfigValidation> {
    let target = ConfigTarget::parse(key)?;
    target.path(paths, store)?;
    validate(paths, store, target.kind, content)
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
    save_config_selected(paths, store, kind.id(), content, force, None)
}

pub fn save_config_selected(
    paths: &Paths,
    store: &crate::store::Store,
    key: &str,
    content: &str,
    force: bool,
    expected_content: Option<&str>,
) -> Result<ConfigValidation> {
    let target = ConfigTarget::parse(key)?.selected(store)?;
    let path = target.path(paths, store)?;
    let v = validate(paths, store, target.kind, content)?;
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

    write_config_version(paths, &target, &path, content, expected_content)?;
    Ok(v)
}

/// 服务更新托管项时也保留准确版本的历史，不能绕过备份失败或外部修改检查。
pub(crate) fn write_generated_config(
    paths: &Paths,
    key: &str,
    path: &Path,
    content: &str,
    expected: Option<&str>,
) -> Result<()> {
    write_config_version(paths, &ConfigTarget::parse(key)?, path, content, expected)
}

fn write_config_version(
    paths: &Paths,
    target: &ConfigTarget,
    path: &Path,
    content: &str,
    expected: Option<&str>,
) -> Result<()> {
    write_config_version_checked(
        paths,
        target,
        path,
        content,
        expected.map(|text| Some(text.as_bytes())),
    )
}

fn write_config_version_checked(
    paths: &Paths,
    target: &ConfigTarget,
    path: &Path,
    content: &str,
    expected: Option<Option<&[u8]>>,
) -> Result<()> {
    let relative = path
        .strip_prefix(&paths.base)
        .map_err(|_| AppError::new("BAD_CONFIG_PATH", "配置不在应用数据目录内"))?;
    let relative = crate::paths::nginx_path(relative);
    crate::paths::checked_data_path(&paths.base, &relative)?;
    let previous = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(AppError::io("读取当前配置", e)),
    };
    if expected.is_some_and(|bytes| previous.as_deref() != bytes) {
        return Err(AppError::new(
            "CONFIG_CONFLICT",
            "配置已被其他操作修改，当前草稿未覆盖文件",
        )
        .with_hint("保留草稿后重新读取最新配置，再合并修改"));
    }
    if previous.as_deref() == Some(content.as_bytes()) {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| AppError::new("BAD_CONFIG_PATH", "配置目录无效"))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(content.as_bytes())?;
    file.as_file().sync_all()?;
    if let Ok(meta) = std::fs::metadata(path) {
        file.as_file().set_permissions(meta.permissions())?;
    }
    if let Some(previous) = &previous {
        let dir = crate::paths::checked_data_path(&paths.base, "backup/config")?;
        std::fs::create_dir_all(&dir)?;
        // 版本标识编码进文件名；随机后缀避免同一秒连续保存互相覆盖。
        let mut backup = tempfile::Builder::new()
            .prefix(&format!(
                "{}-{}-",
                hex::encode(target.key()),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ))
            .suffix(".bak")
            .tempfile_in(dir)?;
        backup.write_all(&previous)?;
        backup.as_file().sync_all()?;
        backup
            .keep()
            .map_err(|e| AppError::io("保存配置历史", e.error))?;
    }
    crate::paths::checked_data_path(&paths.base, &relative)?;
    let current = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if current != previous {
        return Err(AppError::new(
            "CONFIG_CONFLICT",
            "配置在写入前已变化，未覆盖当前文件，请重新读取",
        ));
    }
    file.persist(path)
        .map_err(|e| AppError::io("发布配置文件", e.error))?;
    Ok(())
}

/// 配置历史备份（供回滚）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigBackup {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub created_at: i64,
    pub target: Option<String>,
}

pub fn list_config_backups(paths: &Paths) -> Vec<ConfigBackup> {
    scan_config_backups(paths, None).unwrap_or_default()
}

pub fn list_config_backups_selected(
    paths: &Paths,
    store: &crate::store::Store,
    key: Option<&str>,
) -> Result<Vec<ConfigBackup>> {
    let target = key
        .map(|k| ConfigTarget::parse(k)?.selected(store).map(|t| t.key()))
        .transpose()?;
    scan_config_backups(paths, target.as_deref())
}

fn backup_target(name: &str) -> Option<ConfigTarget> {
    if let Some((prefix, _)) = name.strip_suffix(".bak")?.split_once('-') {
        if let Ok(bytes) = hex::decode(prefix) {
            let key = String::from_utf8(bytes).ok()?;
            let target = ConfigTarget::parse(&key).ok()?;
            if target.version.is_none()
                && matches!(
                    target.kind,
                    ConfigKind::PhpIni | ConfigKind::MySqlIni | ConfigKind::RedisConf
                )
            {
                return None;
            }
            return Some(target);
        }
    }
    // 历史文件没有版本信息：仅共用配置可明确定位，php.ini/my.ini/redis.conf 不猜版本。
    let kind = if name.starts_with("nginx.conf.") {
        ConfigKind::NginxMain
    } else if name.starts_with("httpd.conf.") {
        ConfigKind::ApacheConf
    } else if name.starts_with("config.yaml.") {
        ConfigKind::MihomoConfig
    } else {
        return None;
    };
    Some(ConfigTarget {
        kind,
        version: None,
    })
}

pub(crate) fn backup_relative_target(name: &str) -> Option<String> {
    let target = backup_target(name)?;
    Some(match target.kind {
        ConfigKind::NginxMain => "etc/nginx/nginx.conf".into(),
        ConfigKind::ApacheConf => "etc/apache/httpd.conf".into(),
        ConfigKind::MihomoConfig => "etc/mihomo/config.yaml".into(),
        ConfigKind::PhpIni => format!("etc/php/{}/php.ini", target.version?),
        ConfigKind::MySqlIni => format!("etc/mysql/{}/my.ini", target.version?),
        ConfigKind::RedisConf => format!("etc/redis/{}/redis.conf", target.version?),
    })
}

fn config_key_for_relative(relative: &str) -> Option<String> {
    let parts: Vec<_> = relative.split('/').collect();
    let key = match parts.as_slice() {
        ["etc", "nginx", "nginx.conf"] => "nginx-main".into(),
        ["etc", "apache", "httpd.conf"] => "apache-conf".into(),
        ["etc", "mihomo", "config.yaml"] => "mihomo-config".into(),
        ["etc", "php", version, "php.ini"] => format!("php-ini@{version}"),
        ["etc", "mysql", version, "my.ini"] => format!("mysql-ini@{version}"),
        ["etc", "redis", version, "redis.conf"] => format!("redis-conf@{version}"),
        _ => return None,
    };
    ConfigTarget::parse(&key).ok().map(|target| target.key())
}

fn scan_config_backups(paths: &Paths, selected: Option<&str>) -> Result<Vec<ConfigBackup>> {
    let mut out = Vec::new();
    for backup in crate::paths::list_backup_files(&paths.base)? {
        let target = backup
            .target_path
            .as_deref()
            .and_then(config_key_for_relative);
        if target.is_some() && !backup.restorable {
            continue;
        }
        if target.is_none() && !backup.name.starts_with("config/") {
            continue;
        }
        if selected.is_some_and(|key| target.as_deref() != Some(key)) {
            continue;
        }
        out.push(ConfigBackup {
            target,
            name: if let Some(name) = backup.name.strip_prefix("config/") {
                name.to_string()
            } else if backup.name.starts_with("files/") {
                backup.name
            } else {
                format!("legacy/{}", backup.name)
            },
            path: backup.path,
            size_bytes: backup.size_bytes,
            created_at: backup.modified_at / 1000,
        });
    }
    out.truncate(30);
    Ok(out)
}

/// 回滚到备份明确绑定的配置与版本。
pub fn rollback_config(
    paths: &Paths,
    store: &crate::store::Store,
    backup_name: &str,
) -> Result<()> {
    rollback_config_selected(paths, store, backup_name, None, None)
}

pub fn rollback_config_selected(
    paths: &Paths,
    store: &crate::store::Store,
    backup_name: &str,
    key: Option<&str>,
    expected_content: Option<&str>,
) -> Result<()> {
    if backup_name.starts_with("files/") || backup_name.starts_with("legacy/") {
        let backup_name = backup_name.strip_prefix("legacy/").unwrap_or(backup_name);
        let preview = crate::paths::preview_backup(&paths.base, backup_name)?;
        let target = config_key_for_relative(&preview.target_relative)
            .ok_or_else(|| AppError::new("BAD_KIND", "该备份不是当前编辑器支持的配置"))?;
        ConfigTarget::parse(&target)?.selected(store)?;
        if key.is_some_and(|key| {
            ConfigTarget::parse(key)
                .and_then(|t| t.selected(store))
                .map(|t| t.key() != target)
                .unwrap_or(true)
        }) {
            return Err(AppError::new(
                "BACKUP_TARGET_MISMATCH",
                "该历史版本不属于当前配置",
            ));
        }
        if let Some(expected) = expected_content {
            if std::fs::read(&preview.target_path).ok().as_deref() != Some(expected.as_bytes()) {
                return Err(AppError::new(
                    "CONFIG_CONFLICT",
                    "配置已被其他操作修改，当前草稿未覆盖文件",
                ));
            }
        }
        crate::paths::restore_backup_checked(&paths.base, backup_name, Some(&preview.revision))?;
        return Ok(());
    }
    let dir = paths.backup().join("config");
    let src = dir.join(backup_name);
    if backup_name.is_empty()
        || !backup_name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        || backup_name.contains("..")
    {
        return Err(AppError::new("FORBIDDEN", "非法的备份文件名"));
    }
    if !std::fs::symlink_metadata(&src).is_ok_and(|m| m.file_type().is_file()) {
        return Err(AppError::new("FILE_NOT_FOUND", "备份文件不存在"));
    }
    crate::paths::checked_data_path(&paths.base, &format!("backup/config/{backup_name}"))?;
    let target = backup_target(backup_name)
        .ok_or_else(|| {
            AppError::new(
                "AMBIGUOUS_BACKUP",
                "此历史文件没有准确的版本信息，无法自动回滚",
            )
            .with_hint("原文件仍保留在备份目录，可先查看内容并确认所属版本")
        })?
        .selected(store)?;
    if let Some(key) = key {
        if ConfigTarget::parse(key)?.selected(store)?.key() != target.key() {
            return Err(AppError::new(
                "BACKUP_TARGET_MISMATCH",
                "该历史版本不属于当前配置",
            ));
        }
    }
    let path = target.path(paths, store)?;
    let content = std::fs::read_to_string(&src).map_err(|e| AppError::io("读取备份", e))?;
    // 回滚前把当前内容也存一份，免得回滚本身变成不可逆操作
    write_config_version(paths, &target, &path, &content, expected_content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unambiguous_legacy_root_backups_can_be_rolled_back_from_editor_history() {
        let (_temp, paths, store) = fixture();
        std::fs::write(paths.nginx_conf(), "current").unwrap();
        std::fs::write(paths.backup().join("nginx.conf.20260101.bak"), "legacy").unwrap();
        let history = list_config_backups_selected(&paths, &store, Some("nginx-main")).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].name, "legacy/nginx.conf.20260101.bak");
        rollback_config_selected(&paths, &store, &history[0].name, Some("nginx-main"), Some("current")).unwrap();
        assert_eq!(std::fs::read_to_string(paths.nginx_conf()).unwrap(), "legacy");
    }

    #[test]
    fn reset_targets_exact_version_backs_up_and_can_restore_from_toolbox() {
        let (_temp, paths, store) = fixture();
        register_php(&paths, &store, "8.2.0", "memory_limit=123M\n");
        register_php(&paths, &store, "8.4.0", "memory_limit=456M\n");
        let preview = preview_config_reset(&paths, &store, "php-ini@8.2.0").unwrap();
        assert!(preview.changed && preview.current_exists);
        assert!(
            preview.path.ends_with("8.2.0/php.ini") || preview.path.ends_with("8.2.0\\php.ini")
        );
        assert_eq!(
            std::fs::read_to_string(paths.php_ini("8.2.0")).unwrap(),
            "memory_limit=123M\n"
        );
        let result = reset_config(&paths, &store, "php-ini@8.2.0", &preview.revision).unwrap();
        assert!(!result.changed);
        assert_eq!(
            std::fs::read_to_string(paths.php_ini("8.2.0")).unwrap(),
            preview.content
        );
        assert_eq!(
            std::fs::read_to_string(paths.php_ini("8.4.0")).unwrap(),
            "memory_limit=456M\n"
        );
        let history = crate::paths::list_backup_files(&paths.base).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(
            history[0].target_path.as_deref(),
            Some("etc/php/8.2.0/php.ini")
        );
        reset_config(&paths, &store, "php-ini@8.2.0", &result.revision).unwrap();
        assert_eq!(
            crate::paths::list_backup_files(&paths.base).unwrap().len(),
            1
        );
        let restore = crate::paths::preview_backup(&paths.base, &history[0].name).unwrap();
        crate::paths::restore_backup_checked(
            &paths.base,
            &history[0].name,
            Some(&restore.revision),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(paths.php_ini("8.2.0")).unwrap(),
            "memory_limit=123M\n"
        );
    }

    #[test]
    fn reset_rejects_stale_confirmation_and_missing_install_but_repairs_missing_or_invalid_file() {
        let (_temp, paths, store) = fixture();
        register_php(&paths, &store, "8.4.0", "memory_limit=123M\n");
        let preview = preview_config_reset(&paths, &store, "php-ini@8.4.0").unwrap();
        std::fs::write(paths.php_ini("8.4.0"), "external edit").unwrap();
        assert_eq!(
            reset_config(&paths, &store, "php-ini@8.4.0", &preview.revision)
                .unwrap_err()
                .code,
            "CONFIG_CONFLICT"
        );
        assert_eq!(
            std::fs::read_to_string(paths.php_ini("8.4.0")).unwrap(),
            "external edit"
        );
        assert!(crate::paths::list_backup_files(&paths.base)
            .unwrap()
            .is_empty());
        for key in [
            "php-ini@8.1.0",
            "nginx-main",
            "apache-conf",
            "mihomo-config",
        ] {
            assert!(preview_config_reset(&paths, &store, key).is_err(), "{key}");
        }
        std::fs::remove_file(paths.php_ini("8.4.0")).unwrap();
        let preview = preview_config_reset(&paths, &store, "php-ini@8.4.0").unwrap();
        assert!(!preview.current_exists);
        reset_config(&paths, &store, "php-ini@8.4.0", &preview.revision).unwrap();
        assert!(paths.php_ini("8.4.0").is_file());
        assert!(crate::paths::list_backup_files(&paths.base)
            .unwrap()
            .is_empty());
        std::fs::write(paths.php_ini("8.4.0"), [0xff, 0xfe, 0x00]).unwrap();
        let preview = preview_config_reset(&paths, &store, "php-ini@8.4.0").unwrap();
        reset_config(&paths, &store, "php-ini@8.4.0", &preview.revision).unwrap();
        let history = crate::paths::list_backup_files(&paths.base).unwrap();
        assert_eq!(std::fs::read(&history[0].path).unwrap(), [0xff, 0xfe, 0x00]);
    }

    #[test]
    fn generic_backups_join_editor_history_and_rollback_obeys_version_and_conflicts() {
        let (_temp, paths, store) = fixture();
        register_php(&paths, &store, "8.2.0", "memory_limit=123M\n");
        register_php(&paths, &store, "8.4.0", "memory_limit=456M\n");
        crate::paths::write_with_backup(
            &paths.php_ini("8.2.0"),
            "memory_limit=512M\n",
            &paths.backup(),
        )
        .unwrap();
        let history = list_config_backups_selected(&paths, &store, Some("php-ini@8.2.0")).unwrap();
        assert_eq!(history.len(), 1);
        assert!(history[0].name.starts_with("files/"));
        assert!(
            list_config_backups_selected(&paths, &store, Some("php-ini@8.4.0"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            rollback_config_selected(
                &paths,
                &store,
                &history[0].name,
                Some("php-ini@8.4.0"),
                None
            )
            .unwrap_err()
            .code,
            "BACKUP_TARGET_MISMATCH"
        );
        assert_eq!(
            rollback_config_selected(
                &paths,
                &store,
                &history[0].name,
                Some("php-ini@8.2.0"),
                Some("stale")
            )
            .unwrap_err()
            .code,
            "CONFIG_CONFLICT"
        );
        rollback_config_selected(
            &paths,
            &store,
            &history[0].name,
            Some("php-ini@8.2.0"),
            Some("memory_limit=512M\n"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(paths.php_ini("8.2.0")).unwrap(),
            "memory_limit=123M\n"
        );
        assert_eq!(
            std::fs::read_to_string(paths.php_ini("8.4.0")).unwrap(),
            "memory_limit=456M\n"
        );
    }

    #[test]
    fn reset_database_config_tracks_port_settings_without_starting_services() {
        let (_temp, paths, store) = fixture();
        for (id, version, key, path) in [
            (
                "mysql",
                "8.0.46",
                "mysql-ini@8.0.46",
                paths.mysql_ini("8.0.46"),
            ),
            (
                "redis",
                "5.0.14",
                "redis-conf@5.0.14",
                paths.redis_conf("5.0.14"),
            ),
        ] {
            store
                .upsert_installed(&crate::model::InstalledPackage {
                    id: id.into(),
                    version: version.into(),
                    category: "database".into(),
                    install_path: paths.runtime_dir(id, version).to_string_lossy().into(),
                    config_path: paths.etc_dir(id, version).to_string_lossy().into(),
                    installed_at: 0,
                })
                .unwrap();
            let preview = preview_config_reset(&paths, &store, key).unwrap();
            assert!(!path.exists());
            store.set_setting("portProfile", "safe").unwrap();
            assert_eq!(
                reset_config(&paths, &store, key, &preview.revision)
                    .unwrap_err()
                    .code,
                "CONFIG_CONFLICT"
            );
            let preview = preview_config_reset(&paths, &store, key).unwrap();
            assert!(preview
                .content
                .contains(if id == "mysql" { "23306" } else { "26379" }));
            reset_config(&paths, &store, key, &preview.revision).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), preview.content);
            store.set_setting("portProfile", "standard").unwrap();
        }
    }

    fn fixture() -> (tempfile::TempDir, Paths, crate::store::Store) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        let store = crate::store::Store::open(paths.db()).unwrap();
        (temp, paths, store)
    }

    fn register_php(paths: &Paths, store: &crate::store::Store, version: &str, content: &str) {
        store
            .upsert_installed(&crate::model::InstalledPackage {
                id: "php".into(),
                version: version.into(),
                category: "runtime".into(),
                install_path: paths.runtime_dir("php", version).to_string_lossy().into(),
                config_path: paths.etc_dir("php", version).to_string_lossy().into(),
                installed_at: 0,
            })
            .unwrap();
        std::fs::create_dir_all(paths.etc_dir("php", version)).unwrap();
        std::fs::write(paths.php_ini(version), content).unwrap();
    }

    #[test]
    fn versions_have_separate_files_and_histories_and_rollback_never_crosses_targets() {
        let (_temp, paths, store) = fixture();
        register_php(&paths, &store, "8.2.0", "memory_limit=128M\n");
        register_php(&paths, &store, "8.4.0", "memory_limit=256M\n");
        let listed = list_configs(&paths, &store);
        assert!(listed.iter().any(
            |c| c.kind == "php-ini@8.2.0" && c.used_by_service.as_deref() == Some("php@8.2.0")
        ));
        assert!(listed.iter().any(|c| c.kind == "php-ini@8.4.0"));
        store.set_setting("activephpVersion", "8.2.0").unwrap();
        assert_eq!(
            read_config(&paths, &store, ConfigKind::PhpIni).unwrap(),
            "memory_limit=128M\n"
        );
        save_config_selected(
            &paths,
            &store,
            "php-ini@8.2.0",
            "memory_limit=321M\n",
            false,
            Some("memory_limit=128M\n"),
        )
        .unwrap();
        let first = list_config_backups_selected(&paths, &store, Some("php-ini@8.2.0")).unwrap();
        assert_eq!(first.len(), 1);
        save_config_selected(
            &paths,
            &store,
            "php-ini@8.2.0",
            "memory_limit=512M\n",
            false,
            Some("memory_limit=321M\n"),
        )
        .unwrap();
        let history = list_config_backups_selected(&paths, &store, Some("php-ini@8.2.0")).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(
            std::fs::read_to_string(&history[0].path).unwrap(),
            "memory_limit=321M\n"
        );
        assert!(
            list_config_backups_selected(&paths, &store, Some("php-ini@8.4.0"))
                .unwrap()
                .is_empty()
        );
        store.set_setting("activephpVersion", "8.4.0").unwrap();
        assert_eq!(
            rollback_config_selected(&paths, &store, &first[0].name, Some("php-ini@8.4.0"), None)
                .unwrap_err()
                .code,
            "BACKUP_TARGET_MISMATCH"
        );
        rollback_config_selected(
            &paths,
            &store,
            &first[0].name,
            Some("php-ini@8.2.0"),
            Some("memory_limit=512M\n"),
        )
        .unwrap();
        assert_eq!(
            read_config_selected(&paths, &store, "php-ini@8.2.0").unwrap(),
            "memory_limit=128M\n"
        );
        assert_eq!(
            read_config_selected(&paths, &store, "php-ini@8.4.0").unwrap(),
            "memory_limit=256M\n"
        );
        crate::configgen::write_php_ini(&paths, "8.2.0").unwrap();
        assert_eq!(
            read_config_selected(&paths, &store, "php-ini@8.2.0").unwrap(),
            "memory_limit=128M\n"
        );
    }

    #[test]
    fn stale_save_and_failed_backup_preserve_current_config_even_when_forced() {
        let (_temp, paths, store) = fixture();
        register_php(&paths, &store, "8.4.0", "memory_limit=256M\n");
        for force in [false, true] {
            assert_eq!(
                save_config_selected(
                    &paths,
                    &store,
                    "php-ini@8.4.0",
                    "memory_limit=512M\n",
                    force,
                    Some("stale content")
                )
                .unwrap_err()
                .code,
                "CONFIG_CONFLICT"
            );
        }
        std::fs::write(
            paths.backup().join("config"),
            "prevent backup directory creation",
        )
        .unwrap();
        assert!(save_config_selected(
            &paths,
            &store,
            "php-ini@8.4.0",
            "memory_limit=512M\n",
            false,
            None
        )
        .is_err());
        assert_eq!(
            read_config_selected(&paths, &store, "php-ini@8.4.0").unwrap(),
            "memory_limit=256M\n"
        );
    }

    #[test]
    fn legacy_ambiguous_backups_and_invalid_version_paths_are_rejected() {
        let (_temp, paths, store) = fixture();
        register_php(&paths, &store, "8.4.0", "memory_limit=256M\n");
        std::fs::create_dir_all(paths.backup().join("config")).unwrap();
        std::fs::write(
            paths.backup().join("config/php.ini.20260926-000000.bak"),
            "memory_limit=64M\n",
        )
        .unwrap();
        assert_eq!(
            rollback_config(&paths, &store, "php.ini.20260926-000000.bak")
                .unwrap_err()
                .code,
            "AMBIGUOUS_BACKUP"
        );
        for kind in ["php-ini", "mysql-ini", "redis-conf"] {
            let name = format!("{}-123456-random.bak", hex::encode(kind));
            std::fs::write(paths.backup().join("config").join(&name), "key=value\n")
                .unwrap();
            assert_eq!(
                rollback_config(&paths, &store, &name).unwrap_err().code,
                "AMBIGUOUS_BACKUP"
            );
        }
        for key in [
            "php-ini@../8.4",
            "php-ini@C:secret",
            "php-ini@",
            "php-ini@8.4.0.",
            "php-ini@.",
            "php-ini@..",
            "nginx@8.4",
        ] {
            assert!(read_config_selected(&paths, &store, key).is_err());
        }
        assert!(read_config_selected(&paths, &store, "php-ini@9.9.9").is_err());
        assert!(rollback_config(&paths, &store, "file.bak:stream").is_err());
    }

    #[test]
    fn apache_does_not_use_nginx_semicolon_rules_and_missing_native_binary_is_not_success() {
        let (_temp, paths, store) = fixture();
        let conf = "ServerName localhost\n<Directory \"/\">\n Require all granted\n</Directory>\n";
        let initial = validate(&paths, &store, ConfigKind::ApacheConf, conf).unwrap();
        assert!(initial.ok);
        assert!(initial.messages.iter().any(|m| m.contains("未执行原生")));
        store
            .upsert_installed(&crate::model::InstalledPackage {
                id: "apache".into(),
                version: "2.4.0".into(),
                category: "web-server".into(),
                install_path: paths
                    .runtime_dir("apache", "2.4.0")
                    .to_string_lossy()
                    .into(),
                config_path: String::new(),
                installed_at: 0,
            })
            .unwrap();
        assert!(
            !validate(&paths, &store, ConfigKind::ApacheConf, conf)
                .unwrap()
                .ok
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires NSB_NGINX_ROOT and NSB_APACHE_ROOT; validates reset and restored configs without starting services"]
    fn native_web_configs_validate_after_reset_and_toolbox_restore() {
        let (_temp, paths, store) = fixture();
        let nginx_source = PathBuf::from(std::env::var("NSB_NGINX_ROOT").expect("NSB_NGINX_ROOT"));
        let nginx_version = nginx_source.file_name().unwrap().to_string_lossy().strip_prefix("nginx-").unwrap().to_string();
        let install = paths.runtime_dir("nginx", &nginx_version);
        let root = install.join(format!("nginx-{nginx_version}"));
        for dir in ["logs", "temp", "conf"] { std::fs::create_dir_all(root.join(dir)).unwrap(); }
        for file in ["nginx.exe", "conf/mime.types"] { std::fs::copy(nginx_source.join(file), root.join(file)).unwrap(); }
        let apache_root = PathBuf::from(std::env::var("NSB_APACHE_ROOT").expect("NSB_APACHE_ROOT"));
        for (id, version, location) in [("nginx", nginx_version.as_str(), install.as_path()), ("apache", "2.4.66", apache_root.parent().unwrap())] {
            store.upsert_installed(&crate::model::InstalledPackage {
                id: id.into(), version: version.into(), category: "web-server".into(),
                install_path: location.to_string_lossy().into(), config_path: String::new(), installed_at: 0,
            }).unwrap();
        }
        let ports = crate::services::PortsProfile::from_settings(&store);
        crate::configgen::write_nginx_conf(&paths, &root, &[], ports.http, ports.https).unwrap();
        crate::configgen::write_httpd_conf(&paths, &apache_root, &[], ports.apache_http, ports.apache_https).unwrap();
        for (kind, path, extra) in [
            (ConfigKind::NginxMain, paths.nginx_conf(), "\nworker_rlimit_nofile 4096;\n"),
            (ConfigKind::ApacheConf, paths.apache_conf(), "\nTimeout 123\n"),
        ] {
            let custom = format!("{}{extra}", std::fs::read_to_string(&path).unwrap());
            std::fs::write(&path, &custom).unwrap();
            let preview = preview_config_reset(&paths, &store, kind.id()).unwrap();
            assert!(preview.changed);
            reset_config(&paths, &store, kind.id(), &preview.revision).unwrap();
            let result = validate(&paths, &store, kind, &std::fs::read_to_string(&path).unwrap()).unwrap();
            assert!(result.ok, "reset {:?}: {:?} {:?}", kind, result.messages, result.issues);
            let backup = crate::paths::list_backup_files(&paths.base).unwrap().into_iter()
                .find(|b| std::fs::read_to_string(&b.path).ok().as_deref() == Some(custom.as_str())).unwrap();
            let preview = crate::paths::preview_backup(&paths.base, &backup.name).unwrap();
            crate::paths::restore_backup_checked(&paths.base, &backup.name, Some(&preview.revision)).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), custom);
            let result = validate(&paths, &store, kind, &custom).unwrap();
            assert!(result.ok, "restore {:?}: {:?} {:?}", kind, result.messages, result.issues);
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires NSB_NGINX_ROOT; runs only nginx -t against temporary configs"]
    fn native_nginx_validator_uses_selected_runtime_and_preserves_live_file() {
        let source = PathBuf::from(std::env::var("NSB_NGINX_ROOT").expect("NSB_NGINX_ROOT"));
        let version = source
            .file_name()
            .unwrap()
            .to_string_lossy()
            .strip_prefix("nginx-")
            .unwrap()
            .to_string();
        let (_temp, paths, store) = fixture();
        let install = paths.runtime_dir("nginx", &version);
        let root = install.join(format!("nginx-{version}"));
        std::fs::create_dir_all(root.join("logs")).unwrap();
        std::fs::create_dir_all(root.join("temp")).unwrap();
        std::fs::copy(source.join("nginx.exe"), root.join("nginx.exe")).unwrap();
        for (v, path) in [
            (&version, install),
            (&"99.0.0".to_string(), paths.runtime_dir("nginx", "99.0.0")),
        ] {
            store
                .upsert_installed(&crate::model::InstalledPackage {
                    id: "nginx".into(),
                    version: v.clone(),
                    category: "web-server".into(),
                    install_path: path.to_string_lossy().into(),
                    config_path: String::new(),
                    installed_at: 0,
                })
                .unwrap();
        }
        store.set_setting("activenginxVersion", &version).unwrap();
        std::fs::create_dir_all(paths.nginx_conf().parent().unwrap()).unwrap();
        std::fs::write(paths.nginx_conf(), "original live configuration").unwrap();
        let good = "events {}\nhttp { server { listen 127.0.0.1:18123; location / { return\n 200 \"literal # { text\"; } } }\n";
        let valid = validate(&paths, &store, ConfigKind::NginxMain, good).unwrap();
        assert!(valid.ok, "{:?} {:?}", valid.messages, valid.issues);
        assert!(
            valid.messages.iter().any(|m| m.contains("successful")),
            "{:?}",
            valid.messages
        );
        let invalid = validate(
            &paths,
            &store,
            ConfigKind::NginxMain,
            "events {}\nunknown_directive yes;\n",
        )
        .unwrap();
        assert!(!invalid.ok);
        assert!(
            invalid.issues.iter().any(|i| i.line == 2),
            "{:?}",
            invalid.issues
        );
        assert_eq!(
            std::fs::read_to_string(paths.nginx_conf()).unwrap(),
            "original live configuration"
        );
        assert!(!std::fs::read_dir(paths.nginx_conf().parent().unwrap())
            .unwrap()
            .flatten()
            .any(|e| e
                .file_name()
                .to_string_lossy()
                .starts_with(".nsb-validate-")));
        println!("Nginx {version}: actual -t accepts quoted #/braces and multiline directives, rejects invalid line 2, preserves current config, removes temporary validation files");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires NSB_PHP_ROOT; copies PHP to a temporary directory and verifies an isolated FastCGI pool"]
    fn real_php_config_and_extension_changes_survive_service_restarts() {
        use std::sync::Arc;
        let source = PathBuf::from(std::env::var("NSB_PHP_ROOT").expect("NSB_PHP_ROOT"));
        let version = source.file_name().unwrap().to_string_lossy().to_string();
        let (_temp, paths, store) = fixture();
        let runtime = paths.runtime_dir("php", &version);
        std::fs::create_dir_all(runtime.join("ext")).unwrap();
        for file in std::fs::read_dir(&source)
            .unwrap()
            .flatten()
            .filter(|f| f.path().is_file())
        {
            std::fs::copy(file.path(), runtime.join(file.file_name())).unwrap();
        }
        std::fs::copy(
            source.join("ext/php_gettext.dll"),
            runtime.join("ext/php_gettext.dll"),
        )
        .unwrap();
        let initial = format!("[PHP]\nmemory_limit=321M\ndisplay_errors=Off\nextension_dir=\"{}\"\nextension=gettext\n", crate::paths::nginx_path(&runtime.join("ext")));
        register_php(&paths, &store, &version, &initial);
        let script = paths.base.join("settings-probe.php");
        std::fs::write(&script, "<?php echo json_encode(['limit'=>ini_get('memory_limit'),'gettext'=>extension_loaded('gettext')]);").unwrap();
        let state = crate::CoreState {
            paths,
            store,
            manager: Arc::new(crate::services::ServiceManager::new()),
            installer: crate::install::Installer::bundled(),
            downloader: Arc::new(crate::download::Downloader::new()),
            emit: Arc::new(|_| {}),
            watchdog: Arc::new(crate::watchdog::Watchdog::new()),
        };
        let sid = format!("php@{version}");
        struct Cleanup<'a>(&'a crate::CoreState, String);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = self.0.stop_service(&self.1);
            }
        }
        let _cleanup = Cleanup(&state, sid.clone());
        let reserved = loop {
            let first = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let base = first.local_addr().unwrap().port();
            if base > 65000 {
                continue;
            }
            let mut held = vec![first];
            for port in base + 1..base + 4 {
                if let Ok(listener) = std::net::TcpListener::bind(("127.0.0.1", port)) {
                    held.push(listener);
                }
            }
            if held.len() == 4 {
                break held;
            }
        };
        let base = reserved[0].local_addr().unwrap().port();
        state.store.set_port_assign(&sid, base).unwrap();
        drop(reserved);

        fn request(port: u16, script: &Path) -> String {
            fn record(stream: &mut std::net::TcpStream, kind: u8, body: &[u8]) {
                let len = body.len() as u16;
                stream
                    .write_all(&[1, kind, 0, 1, (len >> 8) as u8, len as u8, 0, 0])
                    .unwrap();
                stream.write_all(body).unwrap();
            }
            fn len(out: &mut Vec<u8>, value: usize) {
                if value < 128 {
                    out.push(value as u8);
                } else {
                    out.extend_from_slice(&((value as u32) | 0x80000000).to_be_bytes());
                }
            }
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            record(&mut stream, 1, &[0, 1, 0, 0, 0, 0, 0, 0]);
            let filename = crate::paths::nginx_path(script);
            let mut params = Vec::new();
            for (name, value) in [
                ("SCRIPT_FILENAME", filename.as_str()),
                ("REQUEST_METHOD", "GET"),
                ("SCRIPT_NAME", "/settings-probe.php"),
                ("SERVER_PROTOCOL", "HTTP/1.1"),
                ("REDIRECT_STATUS", "200"),
                ("CONTENT_LENGTH", "0"),
            ] {
                len(&mut params, name.len());
                len(&mut params, value.len());
                params.extend_from_slice(name.as_bytes());
                params.extend_from_slice(value.as_bytes());
            }
            record(&mut stream, 4, &params);
            record(&mut stream, 4, &[]);
            record(&mut stream, 5, &[]);
            let mut result = Vec::new();
            loop {
                let mut header = [0; 8];
                stream.read_exact(&mut header).unwrap();
                let size = u16::from_be_bytes([header[4], header[5]]) as usize;
                let mut content = vec![0; size + header[6] as usize];
                stream.read_exact(&mut content).unwrap();
                if header[1] == 6 || header[1] == 7 {
                    result.extend_from_slice(&content[..size]);
                }
                if header[1] == 3 {
                    break;
                }
            }
            String::from_utf8_lossy(&result).into_owned()
        }

        state.start_service(&sid).unwrap();
        let port = state.store.get_port_assign(&sid).unwrap();
        let first = request(port, &script);
        assert!(
            first.contains("\"limit\":\"321M\"") && first.contains("\"gettext\":true"),
            "{first}"
        );
        let old_pids = state.manager.snapshot(&sid).unwrap().pids;
        let result = state.set_php_extension(&version, "gettext", false).unwrap();
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        let second = request(state.store.get_port_assign(&sid).unwrap(), &script);
        assert!(
            second.contains("\"limit\":\"321M\"") && second.contains("\"gettext\":false"),
            "{second}"
        );
        assert!(old_pids.iter().all(|pid| !platform::process_alive(*pid)));
        let target = format!("php-ini@{version}");
        let current = read_config_selected(&state.paths, &state.store, &target).unwrap();
        state
            .save_config(
                &target,
                &current.replace("321M", "512M"),
                false,
                Some(&current),
            )
            .unwrap();
        state.stop_service(&sid).unwrap();
        state.start_service(&sid).unwrap();
        let final_response = request(state.store.get_port_assign(&sid).unwrap(), &script);
        assert!(
            final_response.contains("\"limit\":\"512M\"")
                && final_response.contains("\"gettext\":false"),
            "{final_response}"
        );
        let pids = state.manager.snapshot(&sid).unwrap().pids;
        state.stop_service(&sid).unwrap();
        assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        println!("PHP {version}: FastCGI confirmed memory_limit 321M → 512M; gettext disable survived automatic and manual restarts; all owned workers stopped");
    }

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
        let issues =
            lint_nginx("http {\n  # note: use { } carefully\n  server { listen 80; }\n}\n");
        assert!(issues.is_empty(), "注释里的花括号不该干扰：{issues:?}");
    }

    #[test]
    fn nginx_lint_accepts_valid_config() {
        let good =
            "events { worker_connections 1024; }\nhttp {\n  server {\n    listen 80;\n  }\n}\n";
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
        assert_eq!(
            parse_nginx_line_no("nginx: configuration file test is successful"),
            None
        );
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
        let issues = lint(
            ConfigKind::RedisConf,
            "port 6379\nmaxmemory 256mb\nsave 900 1\n",
        );
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn validate_reports_not_ok_on_lint_error() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-val-1"));
        let store = crate::store::Store::open(paths.base.join("s.sqlite")).unwrap();
        let v = validate(
            &paths,
            &store,
            ConfigKind::NginxMain,
            "http {\n listen 80\n}\n",
        )
        .unwrap();
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
