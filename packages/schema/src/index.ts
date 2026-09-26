import { z } from "zod";

/* ============ 套件 / Packages ============ */

export const PackageCategory = z.enum([
  "web-server",
  "runtime",
  "database",
  "cache",
  "tool",
  /* ---- 扩展目录（对标 ServBay / FlyEnv 全景清单） ---- */
  "ai-coding",
  "ai",
  "container",
  "tunnel",
  "service-mesh",
  "mail",
  "dns",
  "ftp",
  "search",
  "object-storage",
  "other",
]);
export type PackageCategory = z.infer<typeof PackageCategory>;

/** 类别展示顺序（前端页签与分组用；与清单里出现的顺序无关） */
export const PACKAGE_CATEGORY_ORDER: PackageCategory[] = [
  "web-server",
  "runtime",
  "database",
  "cache",
  "search",
  "object-storage",
  "service-mesh",
  "mail",
  "dns",
  "ftp",
  "tunnel",
  "container",
  "ai",
  "ai-coding",
  "tool",
  "other",
];

export const Os = z.enum(["windows", "macos"]);
export type Os = z.infer<typeof Os>;

export const Arch = z.enum(["x64", "arm64"]);
export type Arch = z.infer<typeof Arch>;

/**
 * 服务运行描述：清单声明「这个包怎么作为服务跑」。
 * 有 run 的包 = 守护进程型（可启停）；没有 = 纯运行时/CLI（只装不用起）。
 * 占位符：{root} 入口所在目录 | {data} 数据目录 | {etc} 配置目录
 *        | {port} 分配端口 | {log} 日志文件 | {bin} 入口可执行文件绝对路径
 */
export const ServiceRunSpec = z.object({
  /** 启动参数模板（按顺序展开；{root}/{data}/{etc}/{port}/{log}/{bin} 占位符） */
  args: z.array(z.string()).default([]),
  /** 工作目录模板（默认入口所在目录） */
  cwd: z.string().optional(),
  /** 环境变量 */
  env: z.record(z.string(), z.string()).optional(),
  /** 健康检查方式：tcp=端口可连 | process=进程存活 | none=不检查 */
  health: z.enum(["tcp", "process", "none"]).default("tcp"),
  /** 健康检查超时（秒） */
  healthTimeoutSec: z.number().default(15),
  /** 首次启动前需要生成的默认配置文件（相对于 {etc}），内容由 config 模板生成 */
  configFile: z.string().optional(),
  /** 配置模板语言：none=不生成 | raw=直接用 configTemplate 内容 */
  configTemplate: z.string().optional(),
  /** 停止命令（相对 {bin} 的替代可执行文件 + 参数）；缺省=直接终止进程组 */
  stopArgs: z.array(z.string()).optional(),
  /** 首次启动前执行一次（如 MariaDB 的 mariadb-install-db --datadir={data}） */
  initArgs: z.array(z.string()).optional(),
  /** 初始化程序名（默认与 {bin} 同目录同名）；如 mariadb-install-db */
  initBin: z.string().optional(),
  /** 初始化完成标记（相对 {data}）；默认 {data}/.nsb-initialized */
  initMarker: z.string().optional(),
  /** 启动前自建的数据子目录（相对 {data}） */
  initDirs: z.array(z.string()).optional(),
  /** 单实例：同一 id 只允许一个版本运行（默认 true） */
  singleInstance: z.boolean().default(true),
  /** 数据目录（相对于应用数据根的 data/），声明后启动前自动创建 */
  dataDir: z.string().optional(),
  /** 服务面板提示（如「需要 Java 21」/「需先启动 Erlang」） */
  requires: z.array(z.string()).optional(),
});
export type ServiceRunSpec = z.infer<typeof ServiceRunSpec>;

/**
 * 版本源：声明「这个包的全部历史版本去哪里枚举」。
 * 清单里只放少量常用版本作为离线兜底，其余版本按需从上游拉取（带缓存）。
 */
export const VersionSource = z.object({
  /** github=Release API；其余动态源读取对应的官方发行索引或下载页。
   * static=仅用清单内固定版本。 */
  kind: z.enum(["github", "nodejs", "php", "go", "nginx", "python", "composer", "consul", "gradle", "zig", "dotnet", "flutter", "mongodb", "mysql", "mariadb", "postgresql", "apache", "tomcat", "elasticsearch", "neo4j", "rustup", "static"]),
  /** github：owner/repo */
  repo: z.string().optional(),
  /** github：匹配发行包文件名的正则（每个 release 取第一个命中的 asset） */
  assetMatch: z.string().optional(),
  /** github：tag 前缀（如 "v"），解析版本号时剥掉 */
  tagPrefix: z.string().optional(),
  /** 下载地址模板（非 github 或需自定义时用）；{version} 占位 */
  urlTemplate: z.string().optional(),
  /** 解压后入口路径模板；{version} 占位（多数包入口带版本号目录） */
  entryTemplate: z.string().optional(),
  /** 校验和地址模板；{version}/{file} 占位，内容是 `<sha256>  <文件名>` 或纯哈希 */
  checksumUrl: z.string().optional(),
  /** 只要匹配该正则的版本（如只列 8.x） */
  versionFilter: z.string().optional(),
  /** 版本号规范化正则（如 memcached 的 tag 1.6.8_mingw_libressl → 1.6.8） */
  versionStrip: z.string().optional(),
  /** 最多返回多少个版本（默认 60，防列表过长） */
  maxVersions: z.number().optional(),
  /** 是否包含预发布版（默认 false） */
  includePrerelease: z.boolean().optional(),
});
export type VersionSource = z.infer<typeof VersionSource>;

/** 从上游枚举出的单个可安装版本 */
export const RemoteVersion = z.object({
  version: z.string(),
  url: z.string(),
  sha256: z.string().optional(),
  sizeBytes: z.number().optional(),
  entry: z.string(),
  kind: z.string(),
  /** 是否预发布 */
  prerelease: z.boolean().default(false),
  /** 人类可读注记，如 LTS / 最新稳定 */
  note: z.string().optional(),
  releasedAt: z.string().optional(),
});
export type RemoteVersion = z.infer<typeof RemoteVersion>;

/** 某个包的版本清单（远程 + 本地已装） */
export const VersionCatalog = z.object({
  id: z.string(),
  /** 远程枚举到的版本（按版本号降序） */
  remote: z.array(RemoteVersion).default([]),
  /** 上游源是否可用；false 时 remote 可能为空或来自缓存 */
  online: z.boolean().default(false),
  /** 数据来自缓存的时间戳（毫秒） */
  cachedAt: z.number().optional(),
  /** 拉取失败原因（人话） */
  error: z.string().optional(),
});
export type VersionCatalog = z.infer<typeof VersionCatalog>;

/** 清单条目（本地打包 + 可远程更新） */
export const PackageManifestEntry = z.object({
  id: z.string(),
  version: z.string(),
  category: PackageCategory,
  displayName: z.string(),
  description: z.string(),
  homepage: z.string().optional(),
  os: z.array(Os),
  arch: z.array(Arch),
  kind: z.enum(["archive", "binary", "targz"]),
  url: z.string(),
  mirrors: z.array(z.string()).optional(),
  sha256: z.string().optional(),
  sizeBytes: z.number(),
  /** 解压后主程序相对路径，如 nginx/nginx.exe */
  entry: z.string(),
  defaultPort: z.number().optional(),
  depends: z.array(z.string()).optional(),
  /** 纯运行时的前置依赖，与后端清单 requires 保持一致。 */
  requires: z.array(z.string()).optional(),
  /** 声明后该包注册为可启停服务（清单驱动的通用启停路径） */
  run: ServiceRunSpec.optional(),
  /** 声明后可从上游枚举该包的历史版本（含最新） */
  versionSource: VersionSource.optional(),
});
export type PackageManifestEntry = z.infer<typeof PackageManifestEntry>;

export const InstallState = z.enum([
  "not-installed",
  "downloading",
  "downloaded",
  "verifying",
  "extracting",
  "configuring",
  "installed",
  "cancelled",
  "error",
]);
export type InstallState = z.infer<typeof InstallState>;

/** 前端聚合视图：清单条目 + 安装状态 */
export const PackageView = PackageManifestEntry.extend({
  install: z
    .object({
      version: z.string(),
      installPath: z.string(),
      configPath: z.string(),
      installedAt: z.number(),
    })
    .optional(),
  /** 同包其它可选版本（多版本共存） */
  availableVersions: z.array(z.string()).default([]),
  /** 单实例服务的「使用中版本」标记 */
  active: z.boolean().default(false),
});
export type PackageView = z.infer<typeof PackageView>;

/* ============ 环境变量注入 / PathEnv ============ */

/** 单个已安装版本在 PATH 注入里的呈现，使用 id + version 标识 */
export const PathEnvEntry = z.object({
  id: z.string(),
  label: z.string(),
  version: z.string(),
  /** 要加进 PATH 的目录（入口程序所在目录） */
  binDir: z.string(),
  /** 目录当前是否真实存在（卸载/移动后为 false） */
  exists: z.boolean(),
  /** 用户是否勾选注入 */
  selected: z.boolean(),
  /** 该目录当前是否已在系统 PATH 中 */
  inPath: z.boolean(),
  /** 目录里可用的命令名（`php`、`mysql` 等），供 UI 直观展示 */
  commands: z.array(z.string()).default([]),
});
export type PathEnvEntry = z.infer<typeof PathEnvEntry>;

export const PathEnvStatus = z.object({
  /** 总开关是否打开 */
  enabled: z.boolean(),
  /** 实际已写入系统 PATH 的托管目录 */
  managedDirs: z.array(z.string()).default([]),
  entries: z.array(PathEnvEntry).default([]),
  /** 需要用户注意的事项（如 Windows 需新开终端） */
  note: z.string().default(""),
  /** 系统 PATH 与「应该注入的」不一致（需重新应用） */
  drift: z.boolean().default(false),
});
export type PathEnvStatus = z.infer<typeof PathEnvStatus>;

/* ============ 服务 / Services ============ */

export const ServiceState = z.enum([
  "stopped",
  "starting",
  "running",
  "stopping",
  "error",
  "unknown",
]);
export type ServiceState = z.infer<typeof ServiceState>;

export const AppErrorInfo = z.object({
  code: z.string(),
  message: z.string(),
  hint: z.string().optional(),
  detail: z.string().optional(),
  /** 端口冲突时带上：端口 / 占用进程 pid / 占用进程名 → 可直接「结束占用并重试」 */
  port: z.number().optional(),
  pid: z.number().optional(),
  holder: z.string().optional(),
});
export type AppErrorInfo = z.infer<typeof AppErrorInfo>;

export const ServiceStatus = z.object({
  id: z.string(),
  label: z.string(),
  state: ServiceState,
  pids: z.array(z.number()),
  port: z.number().optional(),
  version: z.string().optional(),
  memoryMb: z.number().optional(),
  uptimeSec: z.number().optional(),
  lastError: AppErrorInfo.optional(),
  logFile: z.string().optional(),
  category: PackageCategory.optional(),
  /** 清单声明的前置依赖（服务 id） */
  requires: z.array(z.string()).default([]),
  /** 已声明但当前未安装的前置依赖 —— 前端在启动前就能提示 */
  missingRequires: z.array(z.string()).default([]),
});
export type ServiceStatus = z.infer<typeof ServiceStatus>;

/* ============ 站点 / Sites ============ */

export const SiteKind = z.enum([
  "php",
  "static",
  "reverse-proxy",
  "node",
  "python",
  "java",
  "go",
]);
export type SiteKind = z.infer<typeof SiteKind>;

export const RewritePreset = z.enum([
  "none",
  "laravel",
  "thinkphp",
  "wordpress",
  "spa-fallback",
  "next-export",
  "symfony",
  "yii2",
  "codeigniter",
  "cakephp",
  "drupal",
  "joomla",
]);
export type RewritePreset = z.infer<typeof RewritePreset>;

export const SiteRuntime = z.object({
  importedCertId: z.string().optional(),
  webServer: z.enum(["nginx", "apache"]).default("nginx"),
  kind: SiteKind,
  phpVersion: z.string().optional(),
  proxyTarget: z.string().optional(),
  command: z.string().optional(),
  cwd: z.string().optional(),
});
export type SiteRuntime = z.infer<typeof SiteRuntime>;

export const SiteDbBinding = z.object({
  enabled: z.boolean(),
  database: z.string(),
  username: z.string(),
  password: z.string().optional(),
  version: z.string().optional(),
  port: z.number().int().min(1).max(65535).optional(),
});
export type SiteDbBinding = z.infer<typeof SiteDbBinding>;

export const SiteState = z.enum(["running", "stopped", "error", "unconfigured"]);
export type SiteState = z.infer<typeof SiteState>;

export const Site = z.object({
  id: z.string(),
  name: z.string(),
  domains: z.array(z.string()),
  rootDir: z.string(),
  runtime: SiteRuntime,
  https: z.boolean(),
  rewrite: RewritePreset,
  db: SiteDbBinding.nullable(),
  status: SiteState,
  /** 站点级 PHP 覆盖（写入 rootDir/.user.ini）；仅 kind=php 时生效 */
  phpOverrides: z.record(z.string(), z.string()).optional(),
  createdAt: z.number(),
  updatedAt: z.number(),
});
export type Site = z.infer<typeof Site>;

export const CreateSiteInput = z.object({
  name: z.string().min(1),
  domains: z.array(z.string()).min(1),
  rootDir: z.string().min(1),
  runtime: SiteRuntime,
  https: z.boolean().default(false),
  rewrite: RewritePreset.default("none"),
  /** 可选：同时创建数据库 */
  createDb: z
    .object({ database: z.string(), username: z.string(), password: z.string() })
    .optional(),
  writeEnvExample: z.boolean().default(true),
  template: z
    .enum([
      "blank-php",
      "laravel",
      "static",
      "wordpress",
      "thinkphp",
      "symfony",
      "codeigniter",
      "next-export",
      "spa",
      "none",
    ])
    .default("none"),
});
export type CreateSiteInput = z.infer<typeof CreateSiteInput>;

export const SiteCreateProgress = z.object({
  rootDir: z.string(),
  stage: z.enum(["preparing", "downloading", "extracting", "installing", "initializing", "validating", "building", "database", "configuring", "starting"]),
  percent: z.number().min(0).max(100).nullable(),
});
export type SiteCreateProgress = z.infer<typeof SiteCreateProgress>;

/* ============ 服务栈（用户自定义的一整套服务 + 一键启动） ============ */

export const StackItem = z.object({
  /** 服务 id：nginx / php@8.3.33 / mysql@8.0.46 / caddy … */
  serviceId: z.string(),
  label: z.string().optional(),
  /** 启动顺序（升序） */
  order: z.number().default(0),
});
export type StackItem = z.infer<typeof StackItem>;

export const Stack = z.object({
  id: z.string(),
  name: z.string(),
  description: z.string().default(""),
  items: z.array(StackItem).default([]),
  builtin: z.boolean().default(false),
  createdAt: z.number(),
  updatedAt: z.number(),
});
export type Stack = z.infer<typeof Stack>;

export const StackInput = z.object({
  id: z.string().optional(),
  name: z.string().min(1),
  description: z.string().default(""),
  items: z.array(StackItem).min(1),
});
export type StackInput = z.infer<typeof StackInput>;

export const StackItemFailure = z.object({
  serviceId: z.string(),
  error: AppErrorInfo,
});
export type StackItemFailure = z.infer<typeof StackItemFailure>;

/** 一键启动/停止整栈的逐项结果（单项失败不阻断其它项） */
export const StackStartReport = z.object({
  stackId: z.string(),
  started: z.array(z.string()).default([]),
  alreadyRunning: z.array(z.string()).default([]),
  skipped: z.array(z.string()).default([]),
  failed: z.array(StackItemFailure).default([]),
});
export type StackStartReport = z.infer<typeof StackStartReport>;

/* ============ 端口监听者（工具箱：扫描端口 → 结束进程） ============ */

export const ListenerInfo = z.object({
  port: z.number(),
  pid: z.number(),
  processName: z.string().optional(),
  cmdline: z.string().optional(),
  ownedBySelf: z.boolean().default(false),
  serviceId: z.string().optional(),
});
export type ListenerInfo = z.infer<typeof ListenerInfo>;

export const PortRangeScan = z.object({
  from: z.number(),
  to: z.number(),
  listeners: z.array(ListenerInfo).default([]),
  scannedAt: z.number(),
});
export type PortRangeScan = z.infer<typeof PortRangeScan>;

/** 结束占用端口的进程后的结果 */
export const ClosePortOutcome = z.object({
  port: z.number(),
  /** true = 走的是本应用服务停止流程（优雅），false = 直接结束外部进程 */
  graceful: z.boolean(),
  serviceId: z.string().optional(),
  killedPids: z.array(z.number()).default([]),
});
export type ClosePortOutcome = z.infer<typeof ClosePortOutcome>;

/* ============ 证书 / TLS ============ */

export const CertRecord = z.object({
  id: z.string(),
  kind: z.enum(["ca", "site", "acme"]),
  subject: z.string(),
  sans: z.array(z.string()).default([]),
  notBefore: z.number(),
  notAfter: z.number(),
  certPath: z.string(),
  keyPath: z.string().optional(),
  trusted: z.boolean().optional(),
});
export type CertRecord = z.infer<typeof CertRecord>;

/* ============ 下载进度 / 系统统计 ============ */

export const DownloadProgress = z.object({
  taskId: z.string(),
  received: z.number(),
  total: z.number(),
  speedBps: z.number(),
  etaSec: z.number(),
  state: InstallState,
  error: z.string().optional(),
});
export type DownloadProgress = z.infer<typeof DownloadProgress>;

export const SystemStats = z.object({
  cpuPercent: z.number(),
  memUsedMb: z.number(),
  memTotalMb: z.number(),
  diskFreeGb: z.number(),
  diskTotalGb: z.number(),
  history: z.array(
    z.object({ t: z.number(), cpu: z.number(), mem: z.number() })
  ),
});
export type SystemStats = z.infer<typeof SystemStats>;

/* ============ 端口诊断 / hosts / 日志 ============ */

export const PortDiagnosis = z.object({
  port: z.number(),
  inUse: z.boolean(),
  pid: z.number().optional(),
  processName: z.string().optional(),
  cmdline: z.string().optional(),
});
export type PortDiagnosis = z.infer<typeof PortDiagnosis>;

/** 全量端口体检的一行：本应用某服务该用的端口 + 实际占用者 */
export const PortScanEntry = z.object({
  serviceId: z.string(),
  label: z.string(),
  port: z.number(),
  ownedBySelf: z.boolean(),
  pid: z.number().optional(),
  processName: z.string().optional(),
  cmdline: z.string().optional(),
  running: z.boolean(),
  /** free（空闲）/ self（自己的服务在跑）/ conflict（被他人占用） */
  verdict: z.enum(["free", "self", "conflict"]),
});
export type PortScanEntry = z.infer<typeof PortScanEntry>;

export const HostsEntry = z.object({
  ip: z.string(),
  domain: z.string(),
  managed: z.boolean(),
});
export type HostsEntry = z.infer<typeof HostsEntry>;

export const LogLine = z.object({
  ts: z.number().optional(),
  line: z.string(),
});
export type LogLine = z.infer<typeof LogLine>;

/* ============ 数据库管理 ============ */

export const DatabaseInfo = z.object({
  name: z.string(),
  tables: z.number().optional(),
  sizeKb: z.number().optional(),
});
export type DatabaseInfo = z.infer<typeof DatabaseInfo>;

export const DbUserInfo = z.object({
  username: z.string(),
  host: z.string(),
  grants: z.string().optional(),
});
export type DbUserInfo = z.infer<typeof DbUserInfo>;

/* ============ PHP 扩展 ============ */

export const PhpExtension = z.object({
  name: z.string(),
  label: z.string(),
  group: z.string(),
  hint: z.string(),
  enabled: z.boolean(),
  /** 需要 zend_extension= 加载（Xdebug / OPcache） */
  zend: z.boolean(),
  /** 当前运行时实测的内置模块，不能通过 php.ini 单独禁用 */
  builtin: z.boolean(),
  dll: z.string(),
  /** 已启用但缺依赖的扩展名 */
  missingDeps: z.array(z.string()).default([]),
});
export type PhpExtension = z.infer<typeof PhpExtension>;

export const PhpIniToggle = z.object({
  key: z.string(),
  label: z.string(),
  hint: z.string(),
  value: z.boolean(),
});
export type PhpIniToggle = z.infer<typeof PhpIniToggle>;

export const PhpExtensionView = z.object({
  version: z.string(),
  iniPath: z.string(),
  extensions: z.array(PhpExtension),
  toggles: z.array(PhpIniToggle),
});
export type PhpExtensionView = z.infer<typeof PhpExtensionView>;

export const PhpExtensionChange = z.object({
  name: z.string(),
  enabled: z.boolean(),
  /** PHP 实测加载告警（正常为空） */
  warnings: z.array(z.string()).default([]),
  /** 需重启 php-cgi 才生效 */
  needsRestart: z.boolean(),
});
export type PhpExtensionChange = z.infer<typeof PhpExtensionChange>;

/* ============ 工具链镜像源 ============ */

export const MirrorOption = z.object({
  id: z.string(),
  label: z.string(),
  url: z.string(),
  note: z.string(),
  official: z.boolean(),
});
export type MirrorOption = z.infer<typeof MirrorOption>;

export const ToolMirrorStatus = z.object({
  /** composer / npm / pip */
  manager: z.string(),
  current: z.string().nullable().optional(),
  /** 匹配到的预设 id（自定义地址时为空） */
  matched: z.string().nullable().optional(),
  available: z.boolean(),
  /** 会改动哪个文件 */
  configPath: z.string().nullable().optional(),
  options: z.array(MirrorOption),
});
export type ToolMirrorStatus = z.infer<typeof ToolMirrorStatus>;

/* ============ 批量站点操作 ============ */

export const SiteBulkFailure = z.object({
  siteId: z.string(),
  error: AppErrorInfo,
});
export type SiteBulkFailure = z.infer<typeof SiteBulkFailure>;

export const SiteBulkReport = z.object({
  /** start / stop */
  action: z.string(),
  succeeded: z.array(z.string()).default([]),
  already: z.array(z.string()).default([]),
  failed: z.array(SiteBulkFailure).default([]),
});
export type SiteBulkReport = z.infer<typeof SiteBulkReport>;

/* ============ 批量服务操作 ============ */

export const BulkFailure = z.object({
  serviceId: z.string(),
  error: AppErrorInfo,
});
export type BulkFailure = z.infer<typeof BulkFailure>;

export const BulkReport = z.object({
  /** start / stop / restart */
  action: z.string(),
  succeeded: z.array(z.string()).default([]),
  /** 本来就在目标状态，跳过 */
  already: z.array(z.string()).default([]),
  failed: z.array(BulkFailure).default([]),
  /** 实际执行顺序（按依赖排序后） */
  order: z.array(z.string()).default([]),
});
export type BulkReport = z.infer<typeof BulkReport>;

export const BulkSelectionSummary = z.object({
  total: z.number(),
  running: z.number(),
  stopped: z.number(),
  canStop: z.boolean(),
  canStart: z.boolean(),
});
export type BulkSelectionSummary = z.infer<typeof BulkSelectionSummary>;

/* ============ 环境体检 ============ */

export const HealthItem = z.object({
  id: z.string(),
  /** info / warn / error */
  severity: z.string(),
  title: z.string(),
  detail: z.string(),
  action: z.string().nullable().optional(),
  route: z.string().nullable().optional(),
});
export type HealthItem = z.infer<typeof HealthItem>;

export const HealthReport = z.object({
  items: z.array(HealthItem),
  errors: z.number(),
  warnings: z.number(),
  infos: z.number(),
  summary: z.string(),
  checkedAt: z.number(),
});
export type HealthReport = z.infer<typeof HealthReport>;

/* ============ 诊断包 ============ */

export const DiagnosticsBundle = z.object({
  markdown: z.string(),
  serviceCount: z.number(),
  siteCount: z.number(),
  logLines: z.number(),
  /** 被打码的敏感条目数 */
  redacted: z.number(),
  generatedAt: z.number(),
});
export type DiagnosticsBundle = z.infer<typeof DiagnosticsBundle>;

/* ============ 站点 .env ============ */

export const EnvEntry = z.object({
  key: z.string(),
  value: z.string(),
  /** 注释行，保留但不生效 */
  commented: z.boolean(),
  /** 值像敏感信息，前端默认打码 */
  secret: z.boolean(),
  line: z.number(),
  /** 值含空格/# 但没加引号 —— 会被 dotenv 解析错 */
  needsQuote: z.boolean(),
});
export type EnvEntry = z.infer<typeof EnvEntry>;

export const EnvFileView = z.object({
  siteId: z.string(),
  siteName: z.string(),
  path: z.string(),
  exists: z.boolean(),
  entries: z.array(EnvEntry),
  dbHint: z
    .object({
      database: z.string(),
      username: z.string(),
      password: z.string(),
      port: z.number(),
    })
    .nullable()
    .optional(),
  variants: z.array(z.string()).default([]),
});
export type EnvFileView = z.infer<typeof EnvFileView>;

/* ============ 证书体检 ============ */

export const CertHealth = z.object({
  id: z.string(),
  /** ca / site */
  kind: z.string(),
  subject: z.string(),
  sans: z.array(z.string()).default([]),
  notAfter: z.number(),
  /** 剩余天数；负数表示已过期 */
  daysLeft: z.number(),
  /** ok / warn / critical / expired */
  status: z.string(),
  filePresent: z.boolean(),
  usedBySites: z.array(z.string()).default([]),
  /** 站点域名里证书没覆盖的 */
  missingSans: z.array(z.string()).default([]),
  advice: z.string(),
});
export type CertHealth = z.infer<typeof CertHealth>;

export const CertReport = z.object({
  certs: z.array(CertHealth),
  expired: z.number(),
  critical: z.number(),
  warning: z.number(),
  caTrusted: z.boolean(),
  checkedAt: z.number(),
});
export type CertReport = z.infer<typeof CertReport>;

export const ImportedCert = z.object({
  id: z.string(),
  usable: z.boolean(),
  problem: z.string().optional(),
  usedBySites: z.array(z.string()).default([]),
  certPath: z.string(),
  keyPath: z.string(),
  subject: z.string(),
  sans: z.array(z.string()).default([]),
  notBefore: z.number(),
  notAfter: z.number(),
  daysLeft: z.number(),
});
export type ImportedCert = z.infer<typeof ImportedCert>;

/* ============ 配置文件编辑 ============ */

export const ConfigFileInfo = z.object({
  kind: z.string(),
  label: z.string(),
  description: z.string(),
  path: z.string(),
  exists: z.boolean(),
  sizeBytes: z.number(),
  language: z.string(),
  /** 是否有真正的语法校验器 */
  validated: z.boolean(),
  usedByService: z.string().nullable().optional(),
  requiresPackage: z.string().nullable().optional(),
  resettable: z.boolean().optional(),
});
export type ConfigFileInfo = z.infer<typeof ConfigFileInfo>;

export const ConfigIssue = z.object({
  /** 1-based 行号；0 表示与具体行无关 */
  line: z.number(),
  /** error / warning */
  severity: z.string(),
  message: z.string(),
});
export type ConfigIssue = z.infer<typeof ConfigIssue>;

export const ConfigValidation = z.object({
  ok: z.boolean(),
  messages: z.array(z.string()).default([]),
  issues: z.array(ConfigIssue).default([]),
});
export type ConfigValidation = z.infer<typeof ConfigValidation>;

export const ConfigBackup = z.object({
  name: z.string(),
  path: z.string(),
  sizeBytes: z.number(),
  createdAt: z.number(),
  target: z.string().nullable().optional(),
});
export type ConfigBackup = z.infer<typeof ConfigBackup>;

/* ============ 项目扫描 ============ */

export const ScannedProject = z.object({
  path: z.string(),
  name: z.string(),
  /** laravel / think-php / wordpress / next-js / vite / go / python … */
  kind: z.string(),
  documentRoot: z.string(),
  siteKind: z.string(),
  rewrite: z.string(),
  phpMinVersion: z.string().nullable().optional(),
  /** 识别依据，供用户核对 */
  evidence: z.array(z.string()).default([]),
  runHint: z.string(),
  needsDevServer: z.boolean(),
  suggestedDomain: z.string(),
  alreadyConfigured: z.boolean(),
});
export type ScannedProject = z.infer<typeof ScannedProject>;

/* ============ 服务看门狗 ============ */

export const WatchedService = z.object({
  id: z.string(),
  enabled: z.boolean(),
  attempts: z.number(),
  /** 重试次数已用尽，停止自动重启 */
  exhausted: z.boolean(),
  restartCount: z.number(),
  lastRestartAt: z.number().nullable().optional(),
});
export type WatchedService = z.infer<typeof WatchedService>;

export const WatchdogStatus = z.object({
  enabled: z.boolean(),
  maxAttempts: z.number(),
  intervalSec: z.number(),
  watched: z.array(WatchedService),
});
export type WatchdogStatus = z.infer<typeof WatchdogStatus>;

/* ============ 数据库备份 ============ */

export const DbBackupFile = z.object({
  name: z.string(),
  path: z.string(),
  sizeBytes: z.number(),
  createdAt: z.number(),
});
export type DbBackupFile = z.infer<typeof DbBackupFile>;

export const DbBackupProgress = z.object({
  database: z.string(),
  bytes: z.number(),
  total: z.number().nullable().optional(),
  /** running / done / error */
  state: z.string(),
  message: z.string().nullable().optional(),
});
export type DbBackupProgress = z.infer<typeof DbBackupProgress>;

export const DbRestoreResult = z.object({
  ok: z.boolean(),
  /** 还原前自动备份的位置 */
  safetyBackup: z.string().nullable().optional(),
});
export type DbRestoreResult = z.infer<typeof DbRestoreResult>;

/* ============ Xdebug ============ */

/** PHP 构建指纹：决定该装哪个 Xdebug DLL */
export const PhpBuild = z.object({
  phpVersion: z.string(),
  api: z.string(),
  ts: z.boolean(),
  compiler: z.string(),
  arch: z.string(),
});
export type PhpBuild = z.infer<typeof PhpBuild>;

export const XdebugStatus = z.object({
  version: z.string(),
  build: PhpBuild.nullable().optional(),
  dllPresent: z.boolean(),
  enabled: z.boolean(),
  /** php -m 实测是否真的加载了 */
  loaded: z.boolean(),
  loadedVersion: z.string().nullable().optional(),
  recommended: z.string(),
  dllCandidates: z.array(z.string()).default([]),
  manualHint: z.string(),
  settings: z.record(z.string(), z.string()).default({}),
});
export type XdebugStatus = z.infer<typeof XdebugStatus>;

export const XdebugSetupResult = z.object({
  version: z.string(),
  installed: z.boolean(),
  dllPath: z.string().nullable().optional(),
  loadedVersion: z.string().nullable().optional(),
  warnings: z.array(z.string()).default([]),
  /** 自动下载失败时的手动指引 */
  manualHint: z.string().nullable().optional(),
});
export type XdebugSetupResult = z.infer<typeof XdebugSetupResult>;

/* ============ Clash / mihomo 代理 ============ */

export const ProxyProfile = z.object({
  id: z.string(),
  name: z.string(),
  url: z.string(),
  active: z.boolean(),
  addedAt: z.number(),
});
export type ProxyProfile = z.infer<typeof ProxyProfile>;

export const ProxyNode = z.object({
  name: z.string(),
  type: z.string(),
  alive: z.boolean().optional(),
  history: z.array(z.number()).optional(),
});
export type ProxyNode = z.infer<typeof ProxyNode>;

export const ProxyGroupView = z.object({
  name: z.string(),
  type: z.string(),
  now: z.string(),
  nodes: z.array(ProxyNode),
});
export type ProxyGroupView = z.infer<typeof ProxyGroupView>;

export const ProxyStatusInfo = z.object({
  running: z.boolean(),
  mixedPort: z.number(),
  controllerPort: z.number(),
  mode: z.enum(["rule", "global", "direct"]),
  systemProxyEnabled: z.boolean(),
  version: z.string().optional(),
});
export type ProxyStatusInfo = z.infer<typeof ProxyStatusInfo>;

/* ============ 设置 ============ */

/** 可选界面字体（stack 即 CSS font-family；随应用打包，离线可用） */
export const UI_FONT_OPTIONS = [
  {
    id: "sf",
    label: "SF Pro / 系统（默认）",
    stack: '-apple-system, BlinkMacSystemFont, "SF Pro Text", "SF Pro Display", "Helvetica Neue", "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif',
  },
  {
    id: "plex",
    label: "IBM Plex Sans",
    stack: '"IBM Plex Sans Variable", "Inter Variable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif',
  },
  {
    id: "inter",
    label: "Inter",
    stack: '"Inter Variable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif',
  },
  {
    id: "system",
    label: "系统界面字体",
    stack: 'system-ui, -apple-system, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif',
  },
  {
    id: "serif",
    label: "衬线（宋体 / Georgia）",
    stack: 'Georgia, "Times New Roman", "Songti SC", "SimSun", serif',
  },
  {
    id: "mono",
    label: "等宽（JetBrains Mono）",
    stack: '"JetBrains Mono Variable", ui-monospace, "Cascadia Code", Consolas, monospace',
  },
] as const;

/** 可选代码字体（日志 / 代码块 / 终端片段共用） */
export const MONO_FONT_OPTIONS = [
  {
    id: "sf-mono",
    label: "SF Mono / 系统（默认）",
    stack: 'ui-monospace, "SF Mono", "Cascadia Code", "JetBrains Mono Variable", Consolas, monospace',
  },
  {
    id: "plex-mono",
    label: "IBM Plex Mono",
    stack: '"IBM Plex Mono", "JetBrains Mono Variable", ui-monospace, "Cascadia Code", Consolas, monospace',
  },
  {
    id: "jetbrains",
    label: "JetBrains Mono",
    stack: '"JetBrains Mono Variable", ui-monospace, "Cascadia Code", Consolas, monospace',
  },
  {
    id: "fira",
    label: "Fira Code",
    stack: '"Fira Code Variable", "JetBrains Mono Variable", ui-monospace, Consolas, monospace',
  },
  {
    id: "system",
    label: "系统等宽",
    stack: 'ui-monospace, "Cascadia Code", Consolas, "Courier New", monospace',
  },
] as const;

/** 代码块配色主题。colors 缺省（auto）= 跟随界面明暗在 dark/light 间切换。 */
export interface CodePalette {
  bg: string;
  fg: string;
  comment: string;
  key: string;
  keyword: string;
  string: string;
  number: string;
  variable: string;
}

export const CODE_THEME_OPTIONS = [
  { id: "auto", label: "跟随界面（推荐）" },
  {
    id: "dark",
    label: "暗黑 · 默认",
    dark: true,
    colors: {
      bg: "#0A0C0F",
      fg: "#D6DBE3",
      comment: "#6E7A8A",
      key: "#7CB7FF",
      keyword: "#59A7FF",
      string: "#5CC97F",
      number: "#E0A869",
      variable: "#56C8D8",
    },
  },
  {
    id: "light",
    label: "浅色 · GitHub",
    dark: false,
    colors: {
      bg: "#F5F6F8",
      fg: "#24292F",
      comment: "#8A929C",
      key: "#0550AE",
      keyword: "#0963CE",
      string: "#1A7F37",
      number: "#B45309",
      variable: "#8250DF",
    },
  },
  {
    id: "midnight",
    label: "午夜蓝",
    dark: true,
    colors: {
      bg: "#0F1526",
      fg: "#D8E0F0",
      comment: "#5E6C8F",
      key: "#8FB8FF",
      keyword: "#7AA2F7",
      string: "#9ECE6A",
      number: "#FFC777",
      variable: "#7DCFFF",
    },
  },
  {
    id: "monokai",
    label: "Monokai",
    dark: true,
    colors: {
      bg: "#272822",
      fg: "#F8F8F2",
      comment: "#8C8775",
      key: "#66D9EF",
      keyword: "#F92672",
      string: "#E6DB74",
      number: "#AE81FF",
      variable: "#A6E22E",
    },
  },
  {
    id: "solarized-dark",
    label: "Solarized 暗色",
    dark: true,
    colors: {
      bg: "#002B36",
      fg: "#93A1A1",
      comment: "#586E75",
      key: "#268BD2",
      keyword: "#859900",
      string: "#2AA198",
      number: "#6C71C4",
      variable: "#B58900",
    },
  },
  {
    id: "solarized-light",
    label: "Solarized 亮色",
    dark: false,
    colors: {
      bg: "#FDF6E3",
      fg: "#586E75",
      comment: "#93A1A1",
      key: "#268BD2",
      keyword: "#859900",
      string: "#2AA198",
      number: "#6C71C4",
      variable: "#B58900",
    },
  },
] as const;

/** 主题色预设：写 accentHue；选「自定义」则写 accentHex。
    默认蔚蓝 = systemBlue（Light #0088FF / Dark #0091FF），Apple 强调色。 */
export const ACCENT_PRESETS = [
  { id: "blue", label: "蔚蓝", hue: 211 },
  { id: "teal", label: "青碧", hue: 187 },
  { id: "green", label: "苔绿", hue: 135 },
  { id: "indigo", label: "靛蓝", hue: 235 },
  { id: "violet", label: "紫罗兰", hue: 262 },
  { id: "pink", label: "品红", hue: 320 },
  { id: "rose", label: "玫瑰", hue: 350 },
  { id: "amber", label: "琥珀", hue: 35 },
] as const;

export const AppSettings = z.object({
  language: z.enum(["zh", "en"]).default("zh"),
  appearance: z.enum(["dark", "light", "system"]).default("light"),
  accentHue: z.number().default(211),
  /** 自定义主题色（#RRGGBB）；非空时优先于 accentHue */
  accentHex: z.string().default(""),
  /** 界面字体 id（见 UI_FONT_OPTIONS） */
  uiFont: z.string().default("sf"),
  /** 界面缩放（0.85–1.25，1 = 默认） */
  uiScale: z.number().default(1),
  /** 代码字体 id（见 MONO_FONT_OPTIONS） */
  codeFont: z.string().default("sf-mono"),
  /** 代码块 / 日志字号（px，10–18） */
  codeFontSize: z.number().default(11.5),
  /** 代码块默认显示行号 */
  codeLineNumbers: z.boolean().default(true),
  /** 代码块默认自动换行 */
  codeWrap: z.boolean().default(true),
  /** 代码块配色主题 id（见 CODE_THEME_OPTIONS；auto = 跟随界面明暗） */
  codeTheme: z.string().default("auto"),
  /** 自定义代码块背景色（#RRGGBB）；非空时覆盖主题自带的背景 */
  codeBg: z.string().default(""),
  reduceMotion: z.boolean().default(false),
  defaultTld: z.string().default("test"),
  defaultWebServer: z.string().default("nginx"),
  /** safe = 8080/23306… 避开本机其它环境；standard = 80/3306… 约定俗成（默认） */
  portProfile: z.enum(["safe", "standard"]).default("standard"),
  /** 逐个端口的用户覆盖：http/https/mysql/redis/apacheHttp/apacheHttps/postgres/mongodb */
  portOverrides: z.record(z.string(), z.number()).default({}),
  mirror: z.enum(["official", "ghproxy", "custom"]).default("official"),
  customMirror: z.string().default(""),
  autostart: z.boolean().default(false),
  minimizeToTray: z.boolean().default(true),
  /** 启动应用时自动拉起的服务栈 id（空 = 不自动启动） */
  startStackOnLaunch: z.string().default(""),
  /** 启动服务前自动收掉端口占用者（默认为真） */
  autoClosePortOnStart: z.boolean(),
  /** 服务意外退出时自动拉起（用户主动停止的不重启） */
  watchdogEnabled: z.boolean().default(false).default(true),
  /** 远端套件清单地址（用于「检查更新」） */
  manifestUrl: z.string().default(""),
  /** 启动应用后自动检查一次更新（有新版弹窗提示） */
  checkUpdateOnLaunch: z.boolean().default(true),
  /** 发现新版后自动开始后台下载安装包 */
  autoDownloadUpdate: z.boolean().default(false),
  /** 日志页默认拉取行数 */
  logTailLines: z.number().default(500),
  /** 日志页默认自动刷新 */
  logAutoRefresh: z.boolean().default(true),
  /** 结束进程前二次确认 */
  confirmKill: z.boolean().default(true),
  /** 隐藏滚动条（内容照样可以滚动，只是不画滚动条） */
  hideScrollbars: z.boolean().default(true),
  onboardingDone: z.boolean().default(false),
});
export type AppSettings = z.infer<typeof AppSettings>;

/* ============ 应用更新（检查 / 下载 / 安装） ============ */

/** GitHub Release 里与当前平台匹配的安装包 */
export const ReleaseAsset = z.object({
  tag: z.string(),
  htmlUrl: z.string(),
  /** Release 说明（Markdown） */
  body: z.string().default(""),
  publishedAt: z.string().default(""),
  assetName: z.string().nullable().default(null),
  assetUrl: z.string().nullable().default(null),
  assetSize: z.number().nullable().default(null),
});
export type ReleaseAsset = z.infer<typeof ReleaseAsset>;

export const UpdateCheckResult = z.object({
  appVersion: z.string(),
  latestVersion: z.string().nullable(),
  releaseUrl: z.string().nullable(),
  manifestRevision: z.number(),
  /** null = 无法判定（未配置远端清单地址或网络失败） */
  manifestUpdate: z.boolean().nullable(),
  /** null = GitHub 不可达 */
  appUpdate: z.boolean().nullable(),
  /** 最新 Release 详情；仓库尚未发 Release 时为 null */
  release: ReleaseAsset.nullable().default(null),
});
export type UpdateCheckResult = z.infer<typeof UpdateCheckResult>;

/** 更新包下载进度（事件 update://progress） */
export const UpdateProgress = z.object({
  received: z.number(),
  total: z.number(),
  speedBps: z.number(),
  etaSec: z.number(),
  state: z.enum(["downloading", "downloaded"]),
});
export type UpdateProgress = z.infer<typeof UpdateProgress>;

export const DownloadUpdateResult = z.object({
  path: z.string(),
  fileName: z.string(),
  sizeBytes: z.number(),
});
export type DownloadUpdateResult = z.infer<typeof DownloadUpdateResult>;

/* ============ 证书自动化（ACME 签发 / 定时续签 / 多平台部署，参考 certd） ============ */

/** 每个部署目标一次推送的结果 */
export const DeployResult = z.object({
  ok: z.boolean(),
  message: z.string(),
  at: z.number(),
});
export type DeployResult = z.infer<typeof DeployResult>;

/** 部署目标。config 按平台放各自参数：
 *  - btpanel:  url / apiSk / siteName（配置到指定站点）
 *  - onepanel: url / token（上传到证书库）
 *  - aliyun:   accessKeyId / accessKeySecret / region（上传 SSL 证书服务） */
export const DeployTarget = z.object({
  id: z.string(),
  kind: z.string(), // btpanel | onepanel | aliyun
  name: z.string(),
  config: z.record(z.string(), z.string()).default({}),
  lastResult: DeployResult.nullable().default(null),
});
export type DeployTarget = z.infer<typeof DeployTarget>;

/** DNS 服务商凭据（DNS-01 验证） */
export const DnsProvider = z.object({
  kind: z.string(), // aliyun | cloudflare | dnspod
  accessKey: z.string().default(""),
  secret: z.string().default(""),
});
export type DnsProvider = z.infer<typeof DnsProvider>;

/** 一次签发/续签执行的留痕（certd 式执行日志） */
export const CertRunRecord = z.object({
  at: z.number(),
  ok: z.boolean(),
  message: z.string(),
  log: z.array(z.string()).default([]),
});
export type CertRunRecord = z.infer<typeof CertRunRecord>;

/** 手动 DNS 模式等待用户添加的 TXT 记录 */
export const DnsTxtRecord = z.object({
  name: z.string(),
  value: z.string(),
});
export type DnsTxtRecord = z.infer<typeof DnsTxtRecord>;

/** 第三方网站证书监控（TLS 握手读对端证书链） */
export const CertMonitor = z.object({
  id: z.string(),
  host: z.string(),
  port: z.number().default(443),
  name: z.string().default(""),
  /** idle | ok | expiring | expired | error */
  state: z.string().default("idle"),
  issuer: z.string().default(""),
  expiresAt: z.number().nullable().default(null),
  lastChecked: z.number().nullable().default(null),
  lastError: z.string().default(""),
  createdAt: z.number(),
  updatedAt: z.number(),
});
export type CertMonitor = z.infer<typeof CertMonitor>;

/** 一条自动化：签哪些域名、怎么验证、部署到哪、何时续 */
export const CertAutomation = z.object({
  id: z.string(),
  name: z.string(),
  domains: z.array(z.string()).default([]),
  email: z.string().default(""),
  /** letsencrypt | letsencrypt-staging | zerossl */
  ca: z.string().default("letsencrypt"),
  dns: DnsProvider.default({ kind: "aliyun", accessKey: "", secret: "" }),
  /** 签发后写入本地站点证书并按需重载 */
  deployLocal: z.boolean().default(true),
  targets: z.array(DeployTarget).default([]),
  enabled: z.boolean().default(true),
  /** idle | issuing | ok | error */
  state: z.string().default("idle"),
  lastError: z.string().default(""),
  certId: z.string().nullable().default(null),
  issuedAt: z.number().nullable().default(null),
  expiresAt: z.number().nullable().default(null),
  /** 到期前 30 天自动续签；0 = 待首签 */
  nextRenewAt: z.number().default(0),
  lastRunAt: z.number().default(0),
  /* ---- certd 式高级选项 ---- */
  /** 证书私钥算法：ec256(默认) | ec384 | rsa2048 | rsa3072 | rsa4096 */
  keyAlg: z.string().default("ec256"),
  /** ACME 外部账号绑定（ZeroSSL / Google / BuyPass 需要） */
  eabKid: z.string().default(""),
  eabHmacKey: z.string().default(""),
  /** TXT 写入后等待生效秒数 */
  dnsWaitSec: z.number().default(0),
  /** CNAME 代理验证：_acme-challenge.<域名> CNAME 到该授权域；支持 {domain} 占位符 */
  cnameTarget: z.string().default(""),
  /** 到期前多少天续签 */
  renewDaysAhead: z.number().default(30),
  retryTimes: z.number().default(3),
  retryIntervalMin: z.number().default(30),
  failCount: z.number().default(0),
  /** none | generic | dingtalk | wecom | feishu | email */
  notifyKind: z.string().default("none"),
  notifyUrl: z.string().default(""),
  notifySmtp: z
    .object({
      host: z.string().default(""),
      port: z.number().default(587),
      username: z.string().default(""),
      password: z.string().default(""),
      from: z.string().default(""),
      to: z.string().default(""),
      implicitTls: z.boolean().default(false),
    })
    .nullable()
    .default(null),
  runs: z.array(CertRunRecord).default([]),
  /** 手动 DNS（kind=manual）等待用户添加的 TXT 记录 */
  manualRecords: z.array(DnsTxtRecord).default([]),
  createdAt: z.number(),
  updatedAt: z.number(),
});
export type CertAutomation = z.infer<typeof CertAutomation>;
