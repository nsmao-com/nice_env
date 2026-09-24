//! 与前端 @nsb/schema 一一对应的数据模型（serde camelCase）。

use serde::{Deserialize, Serialize};

pub use crate::serde_proxy;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error,
    Unknown,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AppErrorInfo {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 端口冲突：端口号 / 占用进程 pid / 占用进程名，前端据此提供「结束占用并重试」
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
}

impl From<crate::error::AppError> for AppErrorInfo {
    fn from(e: crate::error::AppError) -> Self {
        Self {
            code: e.code,
            message: e.message,
            hint: e.hint,
            detail: e.detail,
            port: e.port,
            pid: e.pid,
            holder: e.holder,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub id: String,
    pub label: String,
    pub state: ServiceState,
    pub pids: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_sec: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<AppErrorInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// 清单声明的前置依赖（服务 id）
    #[serde(default)]
    pub requires: Vec<String>,
    /// 已声明但当前未安装的前置依赖 —— 让前端在启动前就能提示，
    /// 而不是等启动失败才报错（如 Tomcat 缺 JDK）
    #[serde(default)]
    pub missing_requires: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SiteKind {
    Php,
    Static,
    ReverseProxy,
    #[allow(non_camel_case_types)]
    Node,
    Python,
    Java,
    Go,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RewritePreset {
    #[default]
    None,
    Laravel,
    Thinkphp,
    Wordpress,
    SpaFallback,
    NextExport,
    #[serde(rename = "symfony")]
    Symfony,
    #[serde(rename = "yii2")]
    Yii2,
    #[serde(rename = "codeigniter")]
    Codeigniter,
    #[serde(rename = "cakephp")]
    Cakephp,
    #[serde(rename = "drupal")]
    Drupal,
    #[serde(rename = "joomla")]
    Joomla,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SiteRuntime {
    #[serde(default = "default_web_server")]
    pub web_server: String,
    pub kind: SiteKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub php_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

fn default_web_server() -> String {
    "nginx".into()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SiteDbBinding {
    pub enabled: bool,
    pub database: String,
    pub username: String,
    #[serde(skip_serializing)]
    pub password: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Site {
    pub id: String,
    pub name: String,
    pub domains: Vec<String>,
    pub root_dir: String,
    pub runtime: SiteRuntime,
    pub https: bool,
    pub rewrite: RewritePreset,
    pub db: Option<SiteDbBinding>,
    #[serde(default = "default_site_status")]
    pub status: String,
    /// 站点级 PHP 覆盖（写入 rootDir/.user.ini）；仅 kind=php 时生效
    #[serde(default)]
    pub php_overrides: Option<std::collections::BTreeMap<String, String>>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn default_site_status() -> String {
    "running".into()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CreateSiteInput {
    pub name: String,
    pub domains: Vec<String>,
    pub root_dir: String,
    pub runtime: SiteRuntime,
    #[serde(default)]
    pub https: bool,
    #[serde(default)]
    pub rewrite: RewritePreset,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub create_db: Option<CreateDbInfo>,
    #[serde(default = "default_true")]
    pub write_env_example: bool,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub php_overrides: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CreateDbInfo {
    pub database: String,
    pub username: String,
    pub password: String,
}

fn default_true() -> bool {
    true
}

/* ============ 套件清单 ============ */

/// 服务运行描述（与 packages/schema 的 ServiceRunSpec 对齐）。
/// 清单里声明 run 的包 = 守护进程型，走「通用启停」路径，不必写 Rust 分支。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServiceRunSpec {
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Option<std::collections::HashMap<String, String>>,
    #[serde(default = "default_health")]
    pub health: String,
    #[serde(default = "default_health_timeout")]
    pub health_timeout_sec: u64,
    #[serde(default)]
    pub config_file: Option<String>,
    #[serde(default)]
    pub config_template: Option<String>,
    #[serde(default)]
    pub stop_args: Option<Vec<String>>,
    /// 首次启动前执行一次（如 MariaDB 的 mariadb-install-db）
    #[serde(default)]
    pub init_args: Option<Vec<String>>,
    /// 初始化完成标记（相对 {data}）；默认 {data}/.nsb-initialized
    #[serde(default)]
    pub init_marker: Option<String>,
    /// 初始化程序名（默认取 {bin} 同目录同名）
    #[serde(default)]
    pub init_bin: Option<String>,
    /// 启动前自建的数据子目录（相对 {data}）
    #[serde(default)]
    pub init_dirs: Vec<String>,
    #[serde(default = "default_true")]
    pub single_instance: bool,
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default)]
    pub requires: Vec<String>,
}

fn default_health() -> String {
    "tcp".into()
}

fn default_health_timeout() -> u64 {
    15
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PackageManifestEntry {
    pub id: String,
    pub version: String,
    pub category: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub description: String,
    #[serde(default)]
    pub homepage: Option<String>,
    pub os: Vec<String>,
    pub arch: Vec<String>,
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub mirrors: Vec<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(rename = "sizeBytes")]
    pub size_bytes: u64,
    pub entry: String,
    #[serde(default)]
    pub default_port: Option<u16>,
    #[serde(default)]
    pub depends: Vec<String>,
    /// 声明后该包注册为可启停服务（清单驱动的通用启停路径）
    #[serde(default)]
    pub run: Option<ServiceRunSpec>,
    /// 前置依赖（纯运行时用：composer 需要 php、gradle 需要 JDK）。
    /// 服务类套件请用 `run.requires` —— 那条路会在服务列表里显示缺失提示。
    #[serde(default)]
    pub requires: Vec<String>,
    /// 声明后可从上游枚举该包的历史版本（含最新）
    #[serde(default)]
    pub version_source: Option<VersionSource>,
}

/// 版本源（与 packages/schema 的 VersionSource 对齐）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct VersionSource {
    /// github | nodejs | php | go | nginx | python | static
    pub kind: String,
    #[serde(default)]
    pub repo: Option<String>,
    /// github：匹配发行包文件名的正则
    #[serde(default)]
    pub asset_match: Option<String>,
    #[serde(default)]
    pub tag_prefix: Option<String>,
    #[serde(default)]
    pub url_template: Option<String>,
    #[serde(default)]
    pub entry_template: Option<String>,
    #[serde(default)]
    pub checksum_url: Option<String>,
    #[serde(default)]
    pub version_filter: Option<String>,
    /// 版本号规范化正则（应用于 tag 解析后的版本，如 memcached 的 tag
    /// `1.6.8_mingw_libressl` → `1.6.8`）；用于 URL/入口路径拼装
    #[serde(default)]
    pub version_strip: Option<String>,
    #[serde(default)]
    pub max_versions: Option<usize>,
    #[serde(default)]
    pub include_prerelease: Option<bool>,
}

/// 从上游枚举出的单个可安装版本
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RemoteVersion {
    pub version: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    pub entry: String,
    pub kind: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
}

/// 某个包的版本目录（远程枚举 + 元信息）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct VersionCatalog {
    pub id: String,
    #[serde(default)]
    pub remote: Vec<RemoteVersion>,
    #[serde(default)]
    pub online: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub revision: u32,
    pub packages: Vec<PackageManifestEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPackage {
    pub id: String,
    pub version: String,
    pub category: String,
    pub install_path: String,
    pub config_path: String,
    pub installed_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
/// 环境变量注入状态（前端「环境变量」卡片用）
pub struct PathEnvStatus {
    pub enabled: bool,
    pub managed_dirs: Vec<String>,
    pub entries: Vec<PathEnvEntry>,
    pub note: String,
    pub drift: bool,
}

/// 单个已安装包在 PATH 注入里的呈现
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathEnvEntry {
    pub id: String,
    pub label: String,
    pub version: String,
    pub bin_dir: String,
    pub exists: bool,
    pub selected: bool,
    pub in_path: bool,
    pub commands: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PackageView {
    #[serde(flatten)]
    pub manifest: PackageManifestEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub install: Option<InstalledPackage>,
    #[serde(default)]
    pub available_versions: Vec<String>,
    /// 单实例服务的「使用中版本」标记（多版本同 id 时仅一个为 true）
    #[serde(default)]
    pub active: bool,
}

/* ============ 证书 ============ */

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CertRecord {
    pub id: String,
    pub kind: String,
    pub subject: String,
    #[serde(default)]
    pub sans: Vec<String>,
    pub not_before: i64,
    pub not_after: i64,
    pub cert_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trusted: Option<bool>,
}

/* ============ 证书自动化（ACME 签发 / 定时续签 / 多平台部署） ============ */

/// 部署结果（每个目标一份，失败不阻断其它目标）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeployResult {
    pub ok: bool,
    pub message: String,
    pub at: i64,
}

/// 一次签发/续签执行的留痕（参考 certd 的执行日志；最近 N 条随自动化保存）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CertRunRecord {
    pub at: i64,
    pub ok: bool,
    /// 一步人话结论
    pub message: String,
    /// 过程日志行（下单 / TXT 写入 / 验证 / 部署…）
    #[serde(default)]
    pub log: Vec<String>,
}

/// 部署目标。config 按平台放各自的连接参数，避免字段爆炸：
/// - btpanel:  url, apiSk, siteName(把证书直接配到该站点，缺省仅入库证书列表)
/// - onepanel: url, token
/// - aliyun:   accessKeyId, accessKeySecret, region(缺省 cn-hangzhou；上传到 SSL 证书服务)
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DeployTarget {
    pub id: String,
    /// btpanel | onepanel | aliyun
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub config: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<DeployResult>,
}

/// DNS 服务商凭据（DNS-01 验证；域名解析权在你手里才能签发）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct DnsProvider {
    /// aliyun | cloudflare | dnspod
    pub kind: String,
    /// aliyun AccessKeyId / cloudflare API Token / dnspod ID
    #[serde(default)]
    pub access_key: String,
    /// aliyun AccessKeySecret / dnspod Token（cloudflare 不用）
    #[serde(default)]
    pub secret: String,
}

/// 一条自动化：要给哪些域名签发、用什么 DNS 验证、签完部署到哪。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CertAutomation {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub email: String,
    /// letsencrypt | letsencrypt-staging | zerossl
    #[serde(default = "default_ca")]
    pub ca: String,
    #[serde(default)]
    pub dns: DnsProvider,
    /// 签发后写入本地站点证书（按主域名落 certs/sites/，命中站点即重载）
    #[serde(default)]
    pub deploy_local: bool,
    #[serde(default)]
    pub targets: Vec<DeployTarget>,
    #[serde(default = "default_true_fn")]
    pub enabled: bool,
    /// idle | issuing | ok | error
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub last_error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issued_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// 到期前 30 天自动续签；0 = 未计划
    #[serde(default)]
    pub next_renew_at: i64,
    #[serde(default)]
    pub last_run_at: i64,
    /* ---- certd 式高级选项 ---- */
    /// 证书私钥算法：ec256(默认) | ec384 | rsa2048 | rsa3072 | rsa4096
    #[serde(default = "default_key_alg")]
    pub key_alg: String,
    /// ACME 外部账号绑定（ZeroSSL / Google Trust Services / BuyPass 需要）
    #[serde(default)]
    pub eab_kid: String,
    #[serde(default)]
    pub eab_hmac_key: String,
    /// TXT 写入后等待生效的秒数（DNS 同步慢的服务商可调大）
    #[serde(default)]
    pub dns_wait_sec: i64,
    /// CNAME 代理验证（certd 的别名模式）：_acme-challenge.<域名> CNAME 到该授权域。
    /// 支持 {domain} 占位符（每个域名映射到自己的子域），留空 = 不用代理。
    #[serde(default)]
    pub cname_target: String,
    /// 到期前多少天续签（默认 30）
    #[serde(default = "default_renew_days")]
    pub renew_days_ahead: i64,
    /// 失败重试：最多 N 次，间隔 M 分钟；超过后每天再试一次等人工介入
    #[serde(default = "default_retry_times")]
    pub retry_times: i64,
    #[serde(default = "default_retry_interval")]
    pub retry_interval_min: i64,
    #[serde(default)]
    pub fail_count: i64,
    /// 通知：none | generic | dingtalk | wecom | feishu | email
    #[serde(default)]
    pub notify_kind: String,
    /// 邮件通知的 SMTP 配置（notify_kind=email 时必填）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_smtp: Option<NotifySmtp>,
    #[serde(default)]
    pub notify_url: String,
    /// 执行历史（最近 20 条，新的在前）
    #[serde(default)]
    pub runs: Vec<CertRunRecord>,
    /// 手动 DNS（kind=manual）等待用户添加的 TXT 记录
    #[serde(default)]
    pub manual_records: Vec<DnsTxtRecord>,
    pub created_at: i64,
    pub updated_at: i64,
}
fn default_key_alg() -> String {
    "ec256".into()
}
fn default_renew_days() -> i64 {
    30
}
fn default_retry_times() -> i64 {
    3
}
fn default_retry_interval() -> i64 {
    30
}

/// SMTP 邮件通知配置（notify_kind=email 时使用；对齐 certd 的邮件通知插件）
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NotifySmtp {
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    pub username: String,
    pub password: String,
    /// 发件人地址（多数服务商要求与 username 一致）
    pub from: String,
    /// 收件人，逗号分隔
    pub to: String,
    /// 走 TLS(465) 还是 STARTTLS(587，默认)
    #[serde(default = "default_smtp_tls")]
    pub implicit_tls: bool,
}
fn default_smtp_port() -> u16 {
    587
}
fn default_smtp_tls() -> bool {
    false
}

/// 手动 DNS 模式下等待用户添加的 TXT 记录（等待期非空，前端展示+复制）
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DnsTxtRecord {
    pub name: String,
    pub value: String,
}

/// 第三方网站证书监控（certd 的「站点证书监控」）：盯任意 host:port 的
/// 对端证书到期时间，不动它的配置。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CertMonitor {
    pub id: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub name: String,
    /// idle | ok | expiring | expired | error
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub issuer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked: Option<i64>,
    #[serde(default)]
    pub last_error: String,
    pub created_at: i64,
    pub updated_at: i64,
}
fn default_port() -> u16 {
    443
}
fn default_ca() -> String {
    "letsencrypt".into()
}
fn default_true_fn() -> bool {
    true
}

/* ============ 统计 / 诊断 / 日志 ============ */

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SystemStats {
    pub cpu_percent: f32,
    pub mem_used_mb: f64,
    pub mem_total_mb: f64,
    pub disk_free_gb: f64,
    pub disk_total_gb: f64,
    pub history: Vec<StatsPoint>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct StatsPoint {
    pub t: i64,
    pub cpu: f32,
    pub mem: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PortDiagnosis {
    pub port: u16,
    pub in_use: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
}

/// 全量端口体检的一行：本应用某个服务该用的端口 + 当前实际占用者
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PortScanEntry {
    /// 服务 id（nginx / apache / mysql@8.0.46 / php@8.3.33 / …）
    pub service_id: String,
    pub label: String,
    pub port: u16,
    /// 占用者是否就是本应用管理的这个服务（正常）
    pub owned_by_self: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
    /// 该服务当前是否处于运行态
    pub running: bool,
    /// 结论：free（空闲）/ self（自己的服务）/ conflict（被他人占用）
    pub verdict: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LogLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<i64>,
    pub line: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tables: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_kb: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DbUserInfo {
    pub username: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grants: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct HostsEntry {
    pub ip: String,
    pub domain: String,
    #[serde(default)]
    pub managed: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TunnelInfo {
    pub id: String,
    pub port: u16,
    pub url: Option<String>,
    pub started_at: i64,
    pub alive: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub task_id: String,
    pub received: u64,
    pub total: u64,
    pub speed_bps: u64,
    pub eta_sec: f64,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/* ============ 服务栈（用户自定义的一整套服务组合 + 一键启动） ============ */

/// 栈里的一项：服务 id（nginx / php@8.3.33 / mysql@8.0.46 / caddy …）
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StackItem {
    pub service_id: String,
    /// 展示名（可留空，前端回落到服务自身 label）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 启动顺序（升序；同序按数组顺序）
    #[serde(default)]
    pub order: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Stack {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub items: Vec<StackItem>,
    /// 内置预设（不可删除，可复制）
    #[serde(default)]
    pub builtin: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StackInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub items: Vec<StackItem>,
}

/// 一键启动栈的结果：逐项结果（失败不阻断后面的项）
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StackStartReport {
    pub stack_id: String,
    pub started: Vec<String>,
    pub already_running: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<StackItemFailure>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct StackItemFailure {
    pub service_id: String,
    pub error: AppErrorInfo,
}

/* ============ 端口监听者清单（工具箱：扫描某端口 → 结束进程） ============ */

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ListenerInfo {
    pub port: u16,
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
    /// 该 pid 是否属于本应用拉起的服务
    pub owned_by_self: bool,
    /// 属于本应用时对应的服务 id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_id: Option<String>,
}

/// 端口范围内的扫描结果（含每个端口一条，空闲端口 pid=0 不返回）
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PortRangeScan {
    pub from: u16,
    pub to: u16,
    pub listeners: Vec<ListenerInfo>,
    /// 出现在该范围、但非监听状态的占用印象（UDP 等）不作断言，仅留空
    pub scanned_at: i64,
}

/* ================= PHP 扩展 ================= */

/// 一个 PHP 扩展在某个版本下的可用/启用状态。
/// 可用性来自 `ext/` 目录里真实存在的 DLL，启用状态来自 php.ini。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PhpExtension {
    /// 扩展名（去 php_ 前缀与 .dll 后缀），如 `curl` / `pdo_mysql`
    pub name: String,
    /// 友好名，如 `PDO MySQL`
    pub label: String,
    /// 分组：基础 / 数据库 / 缓存 / 图像 / 文本 / 网络 / 性能 / 调试 / 安全 / 归档 / 系统 / 其他
    pub group: String,
    /// 一句话说明（这东西干什么用的）
    pub hint: String,
    pub enabled: bool,
    /// 需要 zend_extension= 加载（Xdebug / OPcache 等）
    pub zend: bool,
    /// PHP 内置扩展：禁用可能弄坏运行时，前端据此给出提示
    pub builtin: bool,
    /// 对应文件名，方便用户自己核对
    pub dll: String,
    /// 已启用但缺少的依赖扩展名（前端高亮提示）
    #[serde(default)]
    pub missing_deps: Vec<String>,
}

/// 某版本 PHP 的扩展面板数据
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PhpExtensionView {
    pub version: String,
    pub ini_path: String,
    pub extensions: Vec<PhpExtension>,
    /// php.ini 快捷开关的当前值（display_errors 等）
    pub toggles: Vec<PhpIniToggle>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PhpIniToggle {
    pub key: String,
    pub label: String,
    pub hint: String,
    pub value: bool,
}

/// 启用/禁用扩展的结果：改了没有 + PHP 实测的告警
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PhpExtensionChange {
    pub name: String,
    pub enabled: bool,
    /// PHP 加载该扩展时的原始告警（正常为空）
    #[serde(default)]
    pub warnings: Vec<String>,
    /// 生效建议：改了 php.ini 需要重启 php-cgi 才生效
    pub needs_restart: bool,
}

/// Xdebug 一键配置的结果
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct XdebugSetupResult {
    pub version: String,
    /// 实测确认 PHP 真的加载了 Xdebug
    pub installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dll_path: Option<String>,
    /// 实测到的 Xdebug 版本号
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loaded_version: Option<String>,
    /// PHP 的原始告警（正常为空）
    #[serde(default)]
    pub warnings: Vec<String>,
    /// 自动下载失败时给用户的手动指引
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manual_hint: Option<String>,
}

/* ================= 数据库备份 ================= */

/// 备份目录里的一个 .sql 文件
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DbBackupFile {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    /// Unix 秒
    pub created_at: i64,
}

/// 备份/还原进度（事件回传）
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DbBackupProgress {
    pub database: String,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// running / done / error
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 还原结果：带回「还原前自动备份」的位置
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DbRestoreResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safety_backup: Option<String>,
}
