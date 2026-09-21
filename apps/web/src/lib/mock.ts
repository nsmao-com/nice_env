/**
 * 浏览器 mock 后端：内存状态实现与 Rust 侧相同的命令面。
 * 仅用于 next dev 下的 UI 开发/演示；桌面端自动走真实 invoke。
 */
import type {
  RemoteVersion,
  VersionCatalog,
  ServiceStatus,
  Site,
  PackageView,
  SystemStats,
  PortDiagnosis,
  PortScanEntry,
  PortRangeScan,
  ClosePortOutcome,
  Stack,
  StackInput,
  StackStartReport,
  HostsEntry,
  LogLine,
  DatabaseInfo,
  DbUserInfo,
  PhpExtensionView,
  PhpExtension,
  PhpExtensionChange,
  XdebugStatus,
  XdebugSetupResult,
  DbBackupFile,
  DbRestoreResult,
  WatchdogStatus,
  ScannedProject,
  ConfigFileInfo,
  ConfigValidation,
  ConfigBackup,
  CertRecord,
  CertReport,
  ImportedCert,
  EnvFileView,
  DiagnosticsBundle,
  HealthReport,
  BulkReport,
  BulkSelectionSummary,
  ToolMirrorStatus,
  ProxyProfile,
  ProxyGroupView,
  ProxyStatusInfo,
  AppSettings,
  CreateSiteInput,
} from "@nsb/schema";
import { emitLocal } from "./backend";

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** 本应用会占用的端口清单（按端口方案；与 Rust 侧 PortsProfile 对齐） */
function ownPorts(): [string, string, number][] {
  const safe = settings.portProfile === "safe";
  const p = safe
    ? { http: 8080, https: 8443, mysql: 23306, redis: 26379, apacheHttp: 8180, apacheHttps: 8444, postgres: 25432, mongodb: 28017 }
    : { http: 80, https: 443, mysql: 3306, redis: 6379, apacheHttp: 8080, apacheHttps: 8443, postgres: 5432, mongodb: 27017 };
  const merged = { ...p, ...settings.portOverrides };
  return [
    ["nginx", "Nginx", merged.http],
    ["nginx", "Nginx (HTTPS)", merged.https],
    ["apache", "Apache", merged.apacheHttp],
    ["apache", "Apache (HTTPS)", merged.apacheHttps],
    ["mysql@8.0.46", "MySQL", merged.mysql],
    ["postgresql", "PostgreSQL", merged.postgres],
    ["mongodb", "MongoDB", merged.mongodb],
    ["redis", "Redis", merged.redis],
    ["mihomo", "mihomo 混合端口", 17890],
    ["mihomo", "mihomo 控制端口", 19090],
  ];
}

/* ---------- PHP 扩展（mock） ---------- */

/** mock 里塞几个有代表性的扩展，覆盖「内置 / 普通 / zend / 缺依赖」四种形态 */
function mockPhpExtSeed(): PhpExtension[] {
  const mk = (
    name: string,
    label: string,
    group: string,
    hint: string,
    enabled: boolean,
    extra: Partial<PhpExtension> = {}
  ): PhpExtension => ({
    name,
    label,
    group,
    hint,
    enabled,
    zend: false,
    builtin: false,
    dll: `php_${name}.dll`,
    missingDeps: [],
    ...extra,
  });
  return [
    mk("core", "Core", "基础", "PHP 核心，不可禁用", true, { builtin: true }),
    mk("standard", "Standard", "基础", "标准库函数集", true, { builtin: true }),
    mk("pdo", "PDO", "数据库", "PDO 抽象层（其它 pdo_* 的前置）", true),
    mk("mysqlnd", "MySQLnd", "数据库", "MySQL 原生驱动", true),
    mk("pdo_mysql", "PDO MySQL", "数据库", "PDO 连 MySQL —— Laravel 等框架默认走这条", true),
    mk("mysqli", "MySQLi", "数据库", "MySQL 原生扩展（WordPress 用它）", true),
    mk("curl", "cURL", "网络", "HTTP 客户端，调第三方接口必备", true),
    mk("mbstring", "mbstring", "文本", "多字节字符串（中文项目几乎必开）", true),
    mk("openssl", "OpenSSL", "安全", "HTTPS / 加密 / 证书", true),
    mk("fileinfo", "Fileinfo", "文件", "识别文件真实 MIME 类型", true),
    mk("sockets", "Sockets", "网络", "底层 socket", true),
    mk("gd", "GD", "图像", "图像处理（验证码 / 缩略图）", true),
    mk("zip", "Zip", "归档", "zip 读写（Composer 装包要用）", false),
    mk("intl", "Intl", "文本", "国际化（ICU）—— 时间/货币/多语言格式化", true),
    mk("opcache", "OPcache", "性能", "字节码缓存 —— 生产环境必开", true, { zend: true }),
    mk("xdebug", "Xdebug", "调试", "断点调试 / 性能剖析（配合 IDE）", false, { zend: true }),
    mk("redis", "Redis", "缓存", "Redis 客户端（连接本地 Redis / 队列）", false, {
      missingDeps: ["igbinary"],
    }),
    mk("igbinary", "igbinary", "缓存", "更紧凑的序列化，Redis/Session 可选", false),
  ];
}

const mockPhpExtState = new Map<string, PhpExtension[]>();

function mockPhpExtensions(version: string): PhpExtensionView {
  if (!mockPhpExtState.has(version)) mockPhpExtState.set(version, mockPhpExtSeed());
  return {
    version,
    iniPath: `C:\\NiceServBay\\etc\\php\\${version}\\php.ini`,
    extensions: mockPhpExtState.get(version)!,
    toggles: mockPhpToggleState.get(version) ?? mockPhpToggleSeed(),
  };
}

const mockPhpToggleState = new Map<string, { key: string; label: string; hint: string; value: boolean }[]>();

function mockPhpToggleSeed() {
  return [
    { key: "display_errors", label: "显示错误", hint: "开发时打开，把报错直接打在页面上", value: true },
    { key: "log_errors", label: "记录错误日志", hint: "写入 logs/php/<版本>/php_errors.log", value: true },
    { key: "opcache.enable", label: "OPcache", hint: "字节码缓存，生产环境建议开启", value: true },
  ];
}

/** 备份文件（mock）：预置两条，方便看列表样式 */
const mockDbBackups = new Map<string, DbBackupFile>(
  [
    ["shop-20260921-113000.sql", 1024 * 512],
    ["wordpress-20260920-220000.sql", 1024 * 180],
  ].map(([name, size], i) => {
    const path = `C:\NiceServBay\backup\db\${name}`;
    return [
      path,
      {
        name: name as string,
        path,
        sizeBytes: size as number,
        createdAt: Math.floor((Date.now() - (i + 1) * 86400000) / 1000),
      },
    ] as const;
  })
);

const now = () => Date.now();
const uid = () => Math.random().toString(36).slice(2, 10);

/* ---------- 内存状态 ---------- */

const services = new Map<string, ServiceStatus>();
const sites = new Map<string, Site>();
const packages = new Map<string, PackageView>();
/** mock 远程版本目录：id → 上游枚举到的版本（浏览器开发用） */
const mockVersionCatalogs = new Map<string, RemoteVersion[]>();

/** 环境变量注入的 mock 状态（浏览器里不碰真实 PATH） */
const mockPathEnv: { enabled: boolean; selected: string[] | null } = {
  enabled: false,
  selected: null,
};

/** mock 用的最小 run 描述：只需 singleInstance 供前端判定服务语义，
 *  其余字段按 schema 默认值补齐（真实清单由 Rust 侧完整声明）。 */
const mockRun = (singleInstance: boolean): NonNullable<PackageView["run"]> => ({
  args: [],
  health: "tcp",
  healthTimeoutSec: 15,
  singleInstance,
});
const certs = new Map<string, CertRecord>();
const databases = new Map<string, DatabaseInfo>();
const dbUsers = new Map<string, DbUserInfo>();
const proxyProfiles = new Map<string, ProxyProfile>();
const hostsManaged = new Map<string, string>();
const settings: AppSettings = {
  language: "zh",
  appearance: "light",
  accentHue: 211,
  accentHex: "",
  uiFont: "sf",
  uiScale: 1,
  codeFont: "sf-mono",
  codeFontSize: 11.5,
  codeLineNumbers: true,
  codeWrap: false,
  reduceMotion: false,
  defaultTld: "test",
  defaultWebServer: "nginx",
  portProfile: "standard",
  portOverrides: {},
  mirror: "official",
  customMirror: "",
  autostart: false,
  minimizeToTray: true,
  startStackOnLaunch: "",
  autoClosePortOnStart: true,
  watchdogEnabled: false,
  manifestUrl: "",
  checkUpdateOnLaunch: true,
  autoDownloadUpdate: false,
  logTailLines: 500,
  logAutoRefresh: true,
  confirmKill: true,
  hideScrollbars: true,
  onboardingDone: true,
};

/* ---------- 服务栈（演示数据：内置预设 + 一个用户自定义栈） ---------- */
const stacks = new Map<string, Stack>();
function seedStacks() {
  const mk = (s: Partial<Stack> & Pick<Stack, "id" | "name" | "items">) =>
    stacks.set(s.id, {
      description: "",
      builtin: false,
      createdAt: now() - 86400_000,
      updatedAt: now() - 3600_000,
      ...s,
    } as Stack);
  mk({
    id: "builtin-lnmp",
    name: "LNMP 经典",
    description: "Nginx + MySQL + PHP，最通用的 PHP 本地开发环境",
    items: [
      { serviceId: "mysql", order: 10 },
      { serviceId: "php", order: 20 },
      { serviceId: "nginx", order: 30 },
    ],
    builtin: true,
  });
  mk({
    id: "builtin-web",
    name: "前端 / 静态站点",
    description: "只要一个 Web 服务器，托管静态产物或反代 dev server",
    items: [{ serviceId: "nginx", order: 10 }],
    builtin: true,
  });
  mk({
    id: "builtin-data",
    name: "数据栈",
    description: "MySQL + Redis，跑后端服务或调试数据时常用",
    items: [
      { serviceId: "mysql", order: 10 },
      { serviceId: "redis", order: 20 },
    ],
    builtin: true,
  });
  mk({
    id: "stack-demo",
    name: "我的 Laravel 环境",
    description: "Nginx + PHP 8.3 + MySQL + Redis，日常开发用",
    items: [
      { serviceId: "mysql", order: 10 },
      { serviceId: "redis", order: 20 },
      { serviceId: "php", order: 30 },
      { serviceId: "nginx", order: 40 },
    ],
  });
}
seedStacks();

const serviceLogLines = new Map<string, string[]>();
const statsHistory: { t: number; cpu: number; mem: number }[] = [];
for (let i = 0; i < 60; i++) {
  statsHistory.push({
    t: now() - (60 - i) * 5_000,
    cpu: 8 + Math.random() * 22 + Math.sin(i / 6) * 6,
    mem: 46 + Math.random() * 8,
  });
}

function seed() {
  const mk = (s: Partial<ServiceStatus> & Pick<ServiceStatus, "id" | "label">) =>
    services.set(s.id, {
      state: "stopped",
      pids: [],
      category: "web-server",
      ...s,
    } as ServiceStatus);

  mk({
    id: "nginx",
    label: "Nginx",
    state: "running",
    pids: [10412],
    port: 8080,
    version: "1.26.3",
    memoryMb: 12.4,
    uptimeSec: 4523,
    logFile: "C:/…/logs/nginx/out.log",
  });
  mk({
    id: "php@8.3",
    label: "PHP 8.3 (FPM)",
    state: "running",
    pids: [10500, 10501],
    port: 9101,
    version: "8.3.17",
    memoryMb: 38.2,
    uptimeSec: 4520,
    category: "runtime",
  });
  mk({
    id: "php@7.4",
    label: "PHP 7.4 (FPM)",
    state: "stopped",
    pids: [],
    version: "7.4.33",
    category: "runtime",
  });
  mk({
    id: "mysql@8.0",
    label: "MySQL 8.0",
    state: "running",
    pids: [10600],
    port: 23306,
    version: "8.0.42",
    memoryMb: 412,
    uptimeSec: 4518,
    category: "database",
  });
  mk({
    id: "redis",
    label: "Redis",
    state: "running",
    pids: [10700],
    port: 26379,
    version: "5.0.14",
    memoryMb: 8.1,
    uptimeSec: 4515,
    category: "cache",
  });
  mk({
    id: "mihomo",
    label: "mihomo (Clash)",
    state: "stopped",
    port: 17890,
    version: "1.19.0",
    category: "tool",
  });

  sites.set("site-1", {
    id: "site-1",
    name: "laravel-shop",
    domains: ["shop.test"],
    rootDir: "D:/code/laravel-shop/public",
    runtime: { webServer: "nginx", kind: "php", phpVersion: "8.3" },
    https: true,
    rewrite: "laravel",
    db: {
      enabled: true,
      database: "laravel_shop",
      username: "shop_user",
      password: "dev123456",
    },
    status: "running",
    createdAt: now() - 86400_000 * 12,
    updatedAt: now() - 3600_000 * 5,
  });
  sites.set("site-2", {
    id: "site-2",
    name: "legacy-admin",
    domains: ["admin.test", "old.admin.test"],
    rootDir: "D:/code/legacy-admin",
    runtime: { webServer: "nginx", kind: "php", phpVersion: "7.4" },
    https: false,
    rewrite: "thinkphp",
    db: null,
    status: "running",
    createdAt: now() - 86400_000 * 40,
    updatedAt: now() - 86400_000 * 2,
  });
  sites.set("site-3", {
    id: "site-3",
    name: "go-api",
    domains: ["api.test"],
    rootDir: "D:/code/go-api",
    runtime: { webServer: "nginx", kind: "reverse-proxy", proxyTarget: "127.0.0.1:8080" },
    https: false,
    rewrite: "none",
    db: null,
    status: "stopped",
    createdAt: now() - 86400_000 * 3,
    updatedAt: now() - 86400_000,
  });

  const pkg = (
    id: string,
    version: string,
    category: PackageView["category"],
    displayName: string,
    description: string,
    extra: Partial<PackageView> = {}
  ) => {
    const base: PackageView = {
      id,
      version,
      category,
      displayName,
      description,
      os: ["windows", "macos"],
      arch: ["x64", "arm64"],
      kind: "archive",
      url: `https://example.com/${id}-${version}.zip`,
      sha256: "0".repeat(64),
      sizeBytes: 25_000_000,
      entry: `${id}/${id}`,
      availableVersions: [],
      active: false,
      ...extra,
    };
    packages.set(`${id}@${version}`, base);
  };

  pkg("nginx", "1.28.0", "web-server", "Nginx 1.28", "最新稳定线", { sizeBytes: 2_110_315, defaultPort: 8080, run: mockRun(true) });
  pkg("nginx", "1.27.5", "web-server", "Nginx 1.27", "主线版本", { sizeBytes: 2_110_044, defaultPort: 8080, run: mockRun(true) });
  pkg("nginx", "1.24.0", "web-server", "Nginx 1.24", "旧稳定线（兼容老配置）", { sizeBytes: 1_759_722, defaultPort: 8080, run: mockRun(true) });
  pkg("nginx", "1.26.3", "web-server", "Nginx", "高性能 HTTP 服务器与反向代理", {
    install: { version: "1.26.3", installPath: "…/runtimes/nginx/1.26.3", configPath: "…/etc/nginx/1.26.3", installedAt: now() - 86400_000 * 12 },
    sizeBytes: 1_700_000,
    defaultPort: 8080,
    run: mockRun(true),
  });
  pkg("apache", "2.4.66", "web-server", "Apache 2.4", "Apache HTTP Server，与 Nginx 并存，站点可自选承载", { sizeBytes: 11_527_838, defaultPort: 8180, run: mockRun(true) });
  pkg("php", "8.3.33", "runtime", "PHP 8.3", "服务器端脚本语言", {
    install: { version: "8.3.33", installPath: "…/runtimes/php/8.3.33", configPath: "…/etc/php/8.3.33", installedAt: now() - 86400_000 * 12 },
    sizeBytes: 34_000_000,
    run: mockRun(false),
  });
  pkg("php", "7.4.33", "runtime", "PHP 7.4", "旧项目兼容版本", {
    install: { version: "7.4.33", installPath: "…/runtimes/php/7.4.33", configPath: "…/etc/php/7.4.33", installedAt: now() - 86400_000 * 40 },
    sizeBytes: 24_000_000,
    run: mockRun(false),
  });
  pkg("php", "8.5.10", "runtime", "PHP 8.5", "最新大版本", { sizeBytes: 36_000_000, run: mockRun(false) });
  pkg("php", "8.4.25", "runtime", "PHP 8.4", "", { sizeBytes: 35_000_000, run: mockRun(false) });
  pkg("php", "8.2.33", "runtime", "PHP 8.2", "", { sizeBytes: 33_500_000, run: mockRun(false) });
  pkg("php", "8.1.34", "runtime", "PHP 8.1", "", { sizeBytes: 30_900_000, run: mockRun(false) });
  pkg("php", "8.0.30", "runtime", "PHP 8.0", "老项目兼容", { sizeBytes: 26_877_638, run: mockRun(false) });
  pkg("php", "7.3.33", "runtime", "PHP 7.3", "vc15 构建，老项目兼容", { sizeBytes: 25_760_881, run: mockRun(false) });
  pkg("php", "7.2.34", "runtime", "PHP 7.2", "vc15 构建，老项目兼容", { sizeBytes: 26_265_301, run: mockRun(false) });
  pkg("node", "22.14.0", "runtime", "Node.js 22 LTS", "JavaScript 运行时（node/npm/npx）", { sizeBytes: 34_900_000 });
  pkg("node", "20.19.5", "runtime", "Node.js 20 LTS", "上一代 LTS", { sizeBytes: 29_893_678 });
  pkg("node", "18.20.8", "runtime", "Node.js 18 LTS", "老项目兼容", { sizeBytes: 28_782_590 });
  pkg("python", "3.12.9", "runtime", "Python 3.12", "嵌入式运行时，轻量", { sizeBytes: 11_100_000 });
  pkg("python", "3.13.7", "runtime", "Python 3.13", "最新版", { sizeBytes: 10_922_561 });
  pkg("python", "3.11.9", "runtime", "Python 3.11", "老项目兼容", { sizeBytes: 11_249_023 });
  pkg("go", "1.24.1", "runtime", "Go 1.24", "Go 编译工具链", { sizeBytes: 87_200_000 });
  pkg("go", "1.23.6", "runtime", "Go 1.23", "上一版工具链", { sizeBytes: 81_944_656 });
  pkg("postgresql", "17.6", "database", "PostgreSQL 17", "最新大版本", { sizeBytes: 329_891_687, defaultPort: 25432, run: mockRun(true) });
  pkg("postgresql", "16.9", "database", "PostgreSQL 16", "高级开源关系数据库，首次启动自动 initdb", { sizeBytes: 314_500_000, defaultPort: 25432, run: mockRun(true) });
  pkg("mongodb", "8.0.4", "database", "MongoDB 8.0", "文档数据库", { sizeBytes: 775_800_000, defaultPort: 28017, run: mockRun(true) });
  pkg("mongodb", "7.0.24", "database", "MongoDB 7.0", "上一代 LTS（老驱动兼容）", { sizeBytes: 628_810_191, defaultPort: 28017, run: mockRun(true) });
  pkg("composer", "2.8.5", "tool", "Composer 2.8", "PHP 依赖管理器（php composer.phar）", { sizeBytes: 3_060_000 });
  pkg("composer", "2.7.9", "tool", "Composer 2.7", "2.7 LTS 线", { sizeBytes: 3_018_138 });
  pkg("mysql", "5.7.44", "database", "MySQL 5.7", "经典 5.7（老项目兼容）", { sizeBytes: 352_891_656, defaultPort: 23306, run: mockRun(false) });
  pkg("mysql", "8.0.42", "database", "MySQL 8.0", "关系型数据库", {
    install: { version: "8.0.42", installPath: "…/runtimes/mysql/8.0.42", configPath: "…/etc/mysql/8.0.42", installedAt: now() - 86400_000 * 12 },
    availableVersions: ["8.0.42", "5.7.44"],
    sizeBytes: 230_000_000,
    defaultPort: 23306,
    run: mockRun(false),
  });
  pkg("redis", "5.0.14", "cache", "Redis", "内存 KV 缓存", {
    install: { version: "5.0.14", installPath: "…/runtimes/redis/5.0.14", configPath: "…/etc/redis/5.0.14", installedAt: now() - 86400_000 * 12 },
    sizeBytes: 6_000_000,
    defaultPort: 26379,
    run: mockRun(true),
  });
  pkg("mihomo", "1.19.11", "tool", "mihomo 1.19", "Clash Meta 内核（最新）", { sizeBytes: 11_578_659, defaultPort: 17890, run: mockRun(true) });
  pkg("mihomo", "1.18.10", "tool", "mihomo 1.18", "Clash Meta 内核（稳定旧版）", { sizeBytes: 10_679_111, defaultPort: 17890, run: mockRun(true) });
  pkg("mihomo", "1.19.10", "tool", "mihomo (Clash 内核)", "代理内核：规则分流 / 系统代理 / 节点测速", {
    sizeBytes: 12_000_000,
    defaultPort: 17890,
    run: mockRun(true),
  });
  pkg("mailpit", "1.21.8", "tool", "Mailpit", "本地邮件捕获（二期）", { sizeBytes: 14_000_000 });
  pkg("adminer", "4.8.1", "tool", "Adminer", "单文件数据库管理器", { sizeBytes: 600_000 });

  // 扩展目录：让 mock 覆盖全部类别（真实清单有 13 类，浏览器端要能复现
  // 「分类页签很多」的布局场景，否则分段控件的溢出修复无法在 mock 下验证）
  pkg("caddy", "2.11.4", "web-server", "Caddy", "自动 HTTPS 的现代 Web 服务器", { sizeBytes: 17_559_418, defaultPort: 80 , run: mockRun(true) });
  pkg("frankenphp", "1.12.7", "web-server", "FrankenPHP", "自带 PHP 的应用服务器", { sizeBytes: 59_415_145, defaultPort: 80, run: mockRun(true) });
  pkg("meilisearch", "1.53.2", "search", "Meilisearch", "轻量全文搜索引擎", { sizeBytes: 346_635_776, defaultPort: 7700, run: mockRun(true) });
  pkg("zincsearch", "0.4.10", "search", "ZincSearch", "Go 实现的轻量搜索", { sizeBytes: 22_960_611, defaultPort: 4080, run: mockRun(true) });
  pkg("minio", "2025-09-07", "object-storage", "MinIO", "S3 兼容对象存储", { sizeBytes: 113_115_136, defaultPort: 9000, run: mockRun(true) });
  pkg("consul", "2.0.4", "service-mesh", "Consul", "服务发现与配置中心", { sizeBytes: 72_737_934, defaultPort: 8500, run: mockRun(true) });
  pkg("etcd", "3.7.1", "service-mesh", "etcd", "分布式 KV 存储", { sizeBytes: 24_404_364, defaultPort: 2379, run: mockRun(true) });
  pkg("temporal-cli", "1.9.1", "service-mesh", "Temporal CLI", "工作流引擎", { sizeBytes: 46_408_338, defaultPort: 7233, run: mockRun(true) });
  pkg("cloudflared", "2026.9.1", "tunnel", "Cloudflare Tunnel", "把本机服务暴露到公网", { sizeBytes: 54_976_432, run: mockRun(true) });
  pkg("coredns", "1.14.7", "dns", "CoreDNS", "可插拔 DNS 服务器", { sizeBytes: 23_421_098, defaultPort: 5353, run: mockRun(true) });
  pkg("sftpgo", "2.7.6", "ftp", "SFTPGo", "SFTP / FTP / WebDAV 服务器", { sizeBytes: 60_263_734, defaultPort: 2022, run: mockRun(true) });
  pkg("ollama", "0.34.2", "ai", "Ollama", "本地大模型运行时", { sizeBytes: 1_460_928_014, defaultPort: 11434, run: mockRun(true) });
  pkg("memcached", "1.6.8", "cache", "Memcached", "内存对象缓存", { sizeBytes: 3_540_169, defaultPort: 11211, run: mockRun(true) });
  pkg("qdrant", "1.19.1", "database", "Qdrant", "向量数据库", { sizeBytes: 29_671_153, defaultPort: 6333, run: mockRun(true) });
  pkg("neo4j", "5.26.30", "database", "Neo4j", "图数据库", { sizeBytes: 162_637_544, defaultPort: 7474, run: mockRun(true) });
  pkg("mariadb", "12.3.3", "database", "MariaDB", "MySQL 兼容数据库", { sizeBytes: 104_081_715, defaultPort: 3306, run: mockRun(false) });
  pkg("bun", "1.4.2", "runtime", "Bun", "极快的 JS/TS 运行时", { sizeBytes: 39_807_510 });
  pkg("deno", "2.9.7", "runtime", "Deno", "安全的 JS/TS 运行时", { sizeBytes: 42_630_221 });
  pkg("ruby", "4.0.7", "runtime", "Ruby", "Ruby 运行时", { sizeBytes: 17_742_012 });
  pkg("rust", "1.29.1", "runtime", "Rust (rustup)", "Rust 工具链引导器", { sizeBytes: 12_721_664 });
  pkg("tomcat", "11.0.26", "web-server", "Tomcat 11", "Servlet 容器", { sizeBytes: 16_481_996, defaultPort: 8080, run: mockRun(true) });

  // 远程版本目录（浏览器 mock）：从已播种的包派生「上游枚举结果」，
  // 让版本下拉在有真实后端前也能展示分组/预发布/最新等形态。
  for (const [id, base] of (() => {
    const m = new Map<string, PackageView>();
    for (const p of packages.values()) if (!m.has(p.id)) m.set(p.id, p);
    return m;
  })()) {
    const known = Array.from(packages.values()).filter((p) => p.id === id);
    // 用已知版本倒推几个更老的版本，模拟完整历史
    const seeds = known.map((p) => p.version);
    const extra: RemoteVersion[] = [];
    const nums = (v: string) => v.split(/[.-]/).map((n) => parseInt(n, 10) || 0);
    const newest = seeds.slice().sort((a, b) => {
      const pa = nums(a), pb = nums(b);
      for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
        const d = (pb[i] ?? 0) - (pa[i] ?? 0);
        if (d !== 0) return d;
      }
      return 0;
    })[0];
    if (newest) {
      const [maj, min, patch] = nums(newest);
      // 生成 8 个「更老」的补丁版本，模拟上游还有更多历史
      for (let i = 1; i <= 8; i++) {
        const v = `${maj}.${min}.${Math.max(0, (patch ?? 0) - i)}`;
        if (seeds.includes(v) || v.endsWith(".0") && i > 1) continue;
        extra.push({
          version: v,
          url: `https://example.com/${id}-${v}.zip`,
          sha256: "0".repeat(64),
          sizeBytes: base.sizeBytes,
          entry: base.entry.replace(newest, v),
          kind: "archive",
          prerelease: false,
        });
      }
      // 追加一个预发布版本（用同一 base 版本的 -rc1，确保它排在正式版之后，
      // 与真实 semver 一致：1.0.0-rc1 < 1.0.0）
      extra.push({
        version: `${maj}.${min}.${(patch ?? 0)}-rc1`,
        url: `https://example.com/${id}-${maj}.${min}.${(patch ?? 0)}-rc1.zip`,
        sha256: "0".repeat(64),
        sizeBytes: base.sizeBytes,
        entry: base.entry.replace(newest, `${maj}.${min}.${(patch ?? 0)}-rc1`),
        kind: "archive",
        prerelease: true,
        note: "RC",
      });
    }
    if (extra.length) mockVersionCatalogs.set(id, extra);
  }

  certs.set("ca", {
    id: "ca",
    kind: "ca",
    subject: "NiceServBay Local Root CA",
    sans: [],
    notBefore: now() - 86400_000 * 12,
    notAfter: now() + 86400_000 * 3650,
    certPath: "…/certs/ca.crt",
    keyPath: "…/certs/ca.key",
    trusted: true,
  });
  certs.set("c1", {
    id: "c1",
    kind: "site",
    subject: "shop.test",
    sans: ["shop.test"],
    notBefore: now() - 86400_000 * 6,
    notAfter: now() + 86400_000 * 24,
    certPath: "…/certs/sites/shop.test.crt",
    keyPath: "…/certs/sites/shop.test.key",
  });

  databases.set("laravel_shop", { name: "laravel_shop", tables: 27, sizeKb: 4308 });
  databases.set("legacy_admin", { name: "legacy_admin", tables: 15, sizeKb: 1893 });
  databases.set("mysql", { name: "mysql", tables: 37, sizeKb: 2411 });
  dbUsers.set("u1", { username: "shop_user", host: "127.0.0.1", grants: "ALL ON laravel_shop.*" });
  dbUsers.set("u2", { username: "root", host: "localhost", grants: "ALL PRIVILEGES" });

  proxyProfiles.set("p1", { id: "p1", name: "默认 DIRECT", url: "builtin://direct", active: true, addedAt: now() - 86400_000 });
  hostsManaged.set("shop.test", "127.0.0.1");
  hostsManaged.set("admin.test", "127.0.0.1");
  hostsManaged.set("api.test", "127.0.0.1");

  serviceLogLines.set(
    "nginx",
    [
      "2026-09-18 10:02:11 [notice] nginx/1.26.3 (win64) started",
      "2026-09-18 10:02:11 [notice] config file …/etc/nginx/nginx.conf loaded",
      "2026-09-18 10:15:42 [info] 127.0.0.1 GET /index.php 200 12ms",
      "2026-09-18 10:16:03 [error] 43#0: *1181 upstream timed out (110: Connection timed out) while reading response header",
      "2026-09-18 10:16:05 [info] 127.0.0.1 GET /api/health 200 3ms",
    ]
  );
  serviceLogLines.set("mysql@8.0", [
    "2026-09-18 10:02:14 [System] [MY-010931] [Server] … starting as process 10600",
    "2026-09-18 10:02:16 [System] [MY-013602] [Server] Channel mysqlx configured",
    "2026-09-18 10:02:16 [System] [MY-010931] ready for connections. Port: 23306",
  ]);
}
seed();

/* ---------- 命令实现 ---------- */

let proxyRunning = false;
let systemProxyOn = false;
let proxyMode: "rule" | "global" | "direct" = "rule";

export async function mockInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  await delay(60 + Math.random() * 120);
  switch (cmd) {
    case "list_service_status":
      return Array.from(services.values()) as T;
    case "start_service": {
      const id = args!.id as string;
      const s = services.get(id);
      if (s) {
        s.state = "starting";
        await delay(700);
        s.state = "running";
        s.pids = [Math.floor(Math.random() * 40000) + 1000];
        s.uptimeSec = 0;
        if (id === "mihomo") proxyRunning = true;
      }
      return true as T;
    }
    case "stop_service": {
      const id = args!.id as string;
      const s = services.get(id);
      if (s) {
        s.state = "stopping";
        await delay(500);
        s.state = "stopped";
        s.pids = [];
        if (id === "mihomo") proxyRunning = false;
      }
      return true as T;
    }
    case "restart_service": {
      await mockInvoke("stop_service", args);
      await mockInvoke("start_service", args);
      return true as T;
    }

    /* ---------- 服务栈 ---------- */
    case "list_stacks": {
      const list = Array.from(stacks.values()).sort(
        (a, b) => Number(b.builtin) - Number(a.builtin) || b.updatedAt - a.updatedAt
      );
      return list as T;
    }
    case "save_stack": {
      const input = args!.input as StackInput;
      const id = input.id ?? `stack-${uid()}`;
      const prev = stacks.get(id);
      const stack: Stack = {
        id,
        name: input.name,
        description: input.description ?? "",
        items: [...input.items].sort((a, b) => a.order - b.order),
        builtin: false,
        createdAt: prev?.createdAt ?? now(),
        updatedAt: now(),
      };
      stacks.set(id, stack);
      return stack as T;
    }
    case "duplicate_stack": {
      const src = stacks.get(args!.id as string);
      if (!src) throw new Error("找不到服务栈");
      const id = `stack-${uid()}`;
      const stack: Stack = {
        ...src,
        id,
        name: (args!.name as string) || `${src.name} 副本`,
        builtin: false,
        createdAt: now(),
        updatedAt: now(),
      };
      stacks.set(id, stack);
      return stack as T;
    }
    case "delete_stack": {
      const s = stacks.get(args!.id as string);
      if (s?.builtin) throw new Error("内置预设不能删除");
      stacks.delete(args!.id as string);
      return true as T;
    }
    case "start_stack":
    case "stop_stack": {
      const stack = stacks.get(args!.id as string);
      if (!stack) throw new Error("找不到服务栈");
      const starting = cmd === "start_stack";
      const report: StackStartReport = {
        stackId: stack.id,
        started: [],
        alreadyRunning: [],
        skipped: [],
        failed: [],
      };
      // 演示模式：按启动顺序逐个改状态（停栈时逆序）
      const items = [...stack.items].sort((a, b) => a.order - b.order);
      const ordered = starting ? items : items.reverse();
      for (const item of ordered) {
        const base = item.serviceId.split("@")[0];
        const found =
          services.get(item.serviceId) ??
          Array.from(services.values()).find((s) => s.id.startsWith(`${base}@`));
        if (!found) {
          report.skipped.push(item.serviceId);
          continue;
        }
        if (starting && found.state === "running") {
          report.alreadyRunning.push(found.id);
          continue;
        }
        if (!starting && found.state !== "running") {
          report.alreadyRunning.push(found.id);
          continue;
        }
        await mockInvoke(starting ? "start_service" : "stop_service", { id: found.id });
        report.started.push(found.id);
      }
      return report as T;
    }
    case "scan_port_range": {
      const from = args!.from as number;
      const to = args!.to as number;
      const listeners = ownPorts()
        .filter(([, , port]) => port >= from && port <= to)
        .filter(([serviceId]) => services.get(serviceId)?.state === "running")
        .map(([serviceId, label, port]) => ({
          port,
          pid: 4528,
          processName: "NiceServBay (demo)",
          cmdline: `${label} — 浏览器演示数据，非真实进程`,
          ownedBySelf: true,
          serviceId,
        }));
      return { from, to, listeners, scannedAt: now() } as PortRangeScan as T;
    }
    case "close_port": {
      const port = args!.port as number;
      // 演示模式：把占用该端口的演示服务停掉，让「结束占用」有可见效果
      const row = ownPorts().find(([, , p]) => p === port);
      let serviceId: string | undefined;
      if (row) {
        const [sid] = row;
        const found = services.get(sid);
        if (found?.state === "running") {
          await mockInvoke("stop_service", { id: sid });
          serviceId = sid;
        }
      }
      return {
        port,
        graceful: Boolean(serviceId),
        serviceId,
        killedPids: [4528],
      } as ClosePortOutcome as T;
    }
    case "pathenv_status":
    case "pathenv_set_enabled":
    case "pathenv_set_selected":
    case "pathenv_reapply": {
      // 浏览器 mock：用已装包派生一份状态，不碰真实 PATH
      if (cmd === "pathenv_set_enabled" && args && typeof args.enabled === "boolean") {
        mockPathEnv.enabled = args.enabled;
      }
      if (cmd === "pathenv_set_selected" && args && Array.isArray(args.ids)) {
        mockPathEnv.selected = args.ids as string[];
      }
      const installed = Array.from(packages.values()).filter((p) => p.install);
      const seen = new Set<string>();
      const entries = installed
        .filter((p) => {
          if (seen.has(p.id)) return false;
          seen.add(p.id);
          return true;
        })
        .map((p) => {
          const binDir = `…/runtimes/${p.id}/${p.version}`;
          const selected =
            mockPathEnv.selected === null || mockPathEnv.selected.includes(p.id);
          return {
            id: p.id,
            label: p.displayName,
            version: p.version,
            binDir,
            exists: true,
            selected,
            inPath: mockPathEnv.enabled && selected,
            commands: [p.id],
          };
        });
      const managedDirs = mockPathEnv.enabled
        ? entries.filter((e) => e.selected).map((e) => e.binDir)
        : [];
      return {
        enabled: mockPathEnv.enabled,
        managedDirs,
        entries,
        note: "浏览器 mock：不会真的修改系统 PATH",
        drift: false,
      } as T;
    }
    case "list_packages":
      return Array.from(packages.values()) as T;
    case "version_catalog":
    case "version_catalogs": {
      // 浏览器 mock：返回派生的远程版本，不访问网络
      const one = (id: string): VersionCatalog => ({
        id,
        remote: mockVersionCatalogs.get(id) ?? [],
        online: true,
        cachedAt: now(),
      });
      if (args && typeof args.id === "string") return one(args.id) as T;
      const ids = new Set(Array.from(packages.values()).map((p) => p.id));
      return Array.from(ids).sort().map(one) as T;
    }
    case "install_package": {
      const key = args!.id as string;
      const p = packages.get(key);
      if (p) {
        p.install = {
          version: p.version,
          installPath: `…/runtimes/${p.id}/${p.version}`,
          configPath: `…/etc/${p.id}/${p.version}`,
          installedAt: now(),
        };
        const id = p.category === "runtime" ? `${p.id}@${p.version}` : p.id;
        if (!services.has(id))
          services.set(id, {
            id,
            label: `${p.displayName}`,
            state: "stopped",
            pids: [],
            version: p.version,
            category: p.category,
            port: p.defaultPort,
          });
      }
      return true as T;
    }
    case "uninstall_package": {
      const key = args!.id as string;
      const p = packages.get(key);
      if (p) p.install = undefined;
      return true as T;
    }
    case "list_sites":
      return Array.from(sites.values()) as T;
    case "create_site": {
      const input = args!.input as CreateSiteInput;
      const id = `site-${uid()}`;
      sites.set(id, {
        id,
        name: input.name,
        domains: input.domains,
        rootDir: input.rootDir,
        runtime: input.runtime,
        https: input.https,
        rewrite: input.rewrite,
        db: input.createDb
          ? { enabled: true, ...input.createDb }
          : null,
        status: "running",
        createdAt: now(),
        updatedAt: now(),
      });
      if (input.createDb) databases.set(input.createDb.database, { name: input.createDb.database, tables: 0, sizeKb: 0 });
      input.domains.forEach((d) => hostsManaged.set(d, "127.0.0.1"));
      return sites.get(id) as T;
    }
    case "update_site": {
      const patch = args!.site as Partial<Site> & { id: string };
      const s = sites.get(patch.id);
      if (s) Object.assign(s, patch, { updatedAt: now() });
      return s as T;
    }
    case "delete_site": {
      const id = args!.id as string;
      const s = sites.get(id);
      s?.domains.forEach((d) => hostsManaged.delete(d));
      sites.delete(id);
      return true as T;
    }
    case "start_site":
    case "stop_site": {
      const s = sites.get(args!.id as string);
      if (s) s.status = cmd === "start_site" ? "running" : "stopped";
      return true as T;
    }
    case "read_hosts": {
      const list: HostsEntry[] = [];
      hostsManaged.forEach((ip, domain) => list.push({ ip, domain, managed: true }));
      return list as T;
    }
    case "apply_hosts":
      return true as T;
    case "tool_mirrors":
      return [
        {
          manager: "composer",
          current: "https://mirrors.aliyun.com/composer/",
          matched: "aliyun",
          available: true,
          configPath: "C:\\Users\\demo\\AppData\\Roaming\\Composer\\config.json",
          options: [
            { id: "official", label: "Packagist 官方", url: "https://repo.packagist.org", note: "官方源，国内直连较慢", official: true },
            { id: "aliyun", label: "阿里云", url: "https://mirrors.aliyun.com/composer/", note: "覆盖全、长期稳定，最常用", official: false },
            { id: "tencent", label: "腾讯云", url: "https://mirrors.cloud.tencent.com/composer/", note: "国内速度快", official: false },
            { id: "huawei", label: "华为云", url: "https://repo.huaweicloud.com/repository/php/", note: "国内速度快", official: false },
          ],
        },
        {
          manager: "npm",
          current: "https://registry.npmmirror.com",
          matched: "npmmirror",
          available: true,
          configPath: "C:\\Users\\demo\\.npmrc",
          options: [
            { id: "official", label: "npm 官方", url: "https://registry.npmjs.org", note: "官方源，国内直连较慢", official: true },
            { id: "npmmirror", label: "淘宝 npmmirror", url: "https://registry.npmmirror.com", note: "同步频率高，国内最常用", official: false },
            { id: "tencent", label: "腾讯云", url: "https://mirrors.cloud.tencent.com/npm/", note: "国内速度快", official: false },
          ],
        },
        {
          manager: "pip",
          current: null,
          matched: "official",
          available: true,
          configPath: "C:\\Users\\demo\\AppData\\Roaming\\pip\\pip.ini",
          options: [
            { id: "official", label: "PyPI 官方", url: "https://pypi.org/simple", note: "官方源，国内直连较慢", official: true },
            { id: "tsinghua", label: "清华 TUNA", url: "https://pypi.tuna.tsinghua.edu.cn/simple", note: "覆盖全、稳定，最常用", official: false },
            { id: "aliyun", label: "阿里云", url: "https://mirrors.aliyun.com/pypi/simple/", note: "国内速度快", official: false },
          ],
        },
      ] as ToolMirrorStatus[] as T;
    case "tool_mirror_set":
      return true as T;
    case "tool_mirror_reset":
      return true as T;
    case "bulk_start":
    case "bulk_stop":
    case "bulk_restart": {
      const ids = args!.ids as string[];
      const action = (args?.action as string) ?? "start";
      // mock：按依赖分层排序，让 UI 的顺序展示是真的
      const tier = (id: string) => {
        const base = id.split("@")[0];
        if (["mysql", "redis", "postgresql", "mongodb", "memcached"].includes(base)) return 0;
        if (["php", "node", "python", "go", "java"].includes(base)) return 1;
        if (["nginx", "apache", "caddy", "mihomo"].includes(base)) return 2;
        return 3;
      };
      const order = [...ids].sort((a, b) => tier(a) - tier(b));
      return {
        action,
        succeeded: order,
        already: [],
        failed: [],
        order,
      } as BulkReport as T;
    }
    case "bulk_summary": {
      const ids = args!.ids as string[];
      const running = ids.filter((id) => {
        const st = services.get(id);
        return st?.state === "running";
      }).length;
      return {
        total: ids.length,
        running,
        stopped: ids.length - running,
        canStop: running > 0,
        canStart: ids.length > running,
      } as BulkSelectionSummary as T;
    }
    case "health_check": {
      const inst: unknown[] = [];
      const items: { id: string; severity: string; title: string; detail: string; action?: string; route?: string }[] = [];
      if (inst.length === 0) {
        items.push({ id: "no-packages", severity: "info", title: "还没有安装任何套件", detail: "本地环境是空的，先装 Web 服务器与运行时才能建站", action: "到「套件 / 服务」安装 Nginx + PHP + MySQL", route: "/packages" });
      }
      items.push({ id: "cert-warn", severity: "warn", title: "1 张证书 30 天内到期", detail: "还有时间，但建议早点处理", route: "/tls" });
      items.push({ id: "hosts-drift", severity: "warn", title: "hosts 里的托管记录与站点列表不一致", detail: "应有 3 条，实际 2 条 —— 域名可能解析不到本机", action: "到「工具箱 → 重建 hosts」一键同步", route: "/tools" });
      items.push({ id: "broken-sites", severity: "error", title: "1 个站点配置有问题", detail: "legacy-admin（目录不存在：D:/code/legacy-admin）", action: "到「站点」修正路径，或到「套件 / 服务」补装对应版本", route: "/sites" });
      const errors = items.filter((i) => i.severity === "error").length;
      const warnings = items.filter((i) => i.severity === "warn").length;
      const infos = items.filter((i) => i.severity === "info").length;
      return {
        items: items.sort((a, b) => {
          const rank = (x: { severity: string }) => (x.severity === "error" ? 0 : x.severity === "warn" ? 1 : 2);
          return rank(a) - rank(b);
        }),
        errors,
        warnings,
        infos,
        summary: errors > 0 ? `发现 ${errors} 个需要处理的问题` : warnings > 0 ? `${warnings} 项建议处理，当前可用` : "环境正常",
        checkedAt: Math.floor(Date.now() / 1000),
      } as HealthReport as T;
    }
    case "diagnostics_build": {
      const now = Math.floor(Date.now() / 1000);
      const md = [
        "# NiceServBay 诊断报告",
        "",
        "- 应用版本：0.1.0",
        `- 生成时间：${new Date().toLocaleString()}`,
        "- 操作系统：windows x86_64",
        "- 数据目录：C:\NiceServBay",
        "",
        "## 服务状态",
        "",
        "| 服务 | 状态 | 端口 | 版本 |",
        "|------|------|------|------|",
        "| nginx | Running | 80 | 1.26.2 |",
        "| mysql@8.0.46 | Running | 3306 | 8.0.46 |",
        "| redis | Stopped | 6379 | 7.2.5 |",
        "",
        "## 端口",
        "",
        "| 用途 | 端口 | 占用者 |",
        "|------|------|--------|",
        "| HTTP (nginx) | 80 | nginx.exe(pid 1234) |",
        "| MySQL | 3306 | mysqld.exe(pid 5678) |",
        "| Redis | 6379 | 空闲 |",
        "",
        "## 配置摘要（已脱敏）",
        "",
        "```",
        "[mysqld]",
        "port=3306",
        "password=se******",
        "```",
      ].join("\n");
      return {
        markdown: md,
        serviceCount: 3,
        siteCount: 3,
        logLines: 82,
        redacted: 4,
        generatedAt: now,
      } as DiagnosticsBundle as T;
    }
    case "diagnostics_save":
      return "C:\NiceServBay\diagnostics\niceservbay-diagnostics-20260921-210000.md" as T;
    case "env_read": {
      return {
        siteId: args!.siteId as string,
        siteName: "laravel-shop",
        path: "D:\code\laravel-shop\.env",
        exists: true,
        entries: [
          { key: "APP_NAME", value: "Laravel", commented: false, secret: false, line: 1, needsQuote: false },
          { key: "APP_ENV", value: "local", commented: false, secret: false, line: 2, needsQuote: false },
          { key: "APP_KEY", value: "base64:abcdefghijklmnop=", commented: false, secret: true, line: 3, needsQuote: false },
          { key: "APP_DEBUG", value: "true", commented: false, secret: false, line: 4, needsQuote: false },
          { key: "APP_URL", value: "https://shop.test", commented: false, secret: false, line: 5, needsQuote: false },
          { key: "DB_CONNECTION", value: "mysql", commented: false, secret: false, line: 7, needsQuote: false },
          { key: "DB_HOST", value: "127.0.0.1", commented: false, secret: false, line: 8, needsQuote: false },
          { key: "DB_PORT", value: "3306", commented: false, secret: false, line: 9, needsQuote: false },
          { key: "DB_DATABASE", value: "laravel_shop", commented: false, secret: false, line: 10, needsQuote: false },
          { key: "DB_USERNAME", value: "shop_user", commented: false, secret: false, line: 11, needsQuote: false },
          { key: "DB_PASSWORD", value: "my secret pass", commented: false, secret: true, line: 12, needsQuote: true },
          { key: "REDIS_PASSWORD", value: "abc", commented: true, secret: true, line: 14, needsQuote: false },
        ],
        dbHint: { database: "laravel_shop", username: "shop_user", password: "my secret pass", port: 3306 },
        variants: [".env", ".env.example"],
      } as EnvFileView as T;
    }
    case "env_save":
      return true as T;
    case "env_apply_db":
      return ["DB_CONNECTION", "DB_HOST", "DB_PORT", "DB_DATABASE", "DB_USERNAME", "DB_PASSWORD"] as string[] as T;
    case "cert_health": {
      const now = Math.floor(Date.now() / 1000);
      const day = 86400;
      return {
        certs: [
          { id: "ca", kind: "ca", subject: "NiceServBay Local CA", sans: [], notAfter: now + 3600 * day, daysLeft: 3600, status: "ok", filePresent: true, usedBySites: [], missingSans: [], advice: "" },
          { id: "laravel-shop", kind: "site", subject: "shop.test", sans: ["shop.test"], notAfter: now + 12 * day, daysLeft: 12, status: "warn", filePresent: true, usedBySites: ["laravel-shop"], missingSans: [], advice: "还有 12 天到期，建议尽快重新签发" },
          { id: "legacy-admin", kind: "site", subject: "admin.test", sans: ["admin.test"], notAfter: now - 2 * day, daysLeft: -2, status: "expired", filePresent: true, usedBySites: ["legacy-admin"], missingSans: ["old.admin.test"], advice: "已过期：到站点详情里重新签发证书即可" },
        ],
        expired: 1,
        critical: 0,
        warning: 1,
        caTrusted: true,
        checkedAt: now,
      } as CertReport as T;
    }
    case "cert_imported_list":
      return [
        { certPath: "C:\NiceServBay\certs\imported\corp-wildcard.crt", keyPath: "C:\NiceServBay\certs\imported\corp-wildcard.key", subject: "*.corp.internal", sans: ["*.corp.internal", "corp.internal"], notBefore: 1700000000, notAfter: 1800000000, daysLeft: 210 },
      ] as ImportedCert[] as T;
    case "cert_import":
      return { certPath: "x", keyPath: "y", subject: "imported", sans: [], notBefore: 0, notAfter: 0, daysLeft: 365 } as ImportedCert as T;
    case "cert_imported_delete":
      return true as T;
    case "list_certs":
      return Array.from(certs.values()) as T;
    case "issue_cert": {
      const domain = args!.domain as string;
      const id = uid();
      certs.set(id, {
        id,
        kind: "site",
        subject: domain,
        sans: args!.sans ? (args!.sans as string[]) : [domain],
        notBefore: now(),
        notAfter: now() + 86400_000 * 30,
        certPath: `…/certs/sites/${domain}.crt`,
        keyPath: `…/certs/sites/${domain}.key`,
      });
      return certs.get(id) as T;
    }
    case "trust_ca": {
      const ca = certs.get("ca");
      if (ca) ca.trusted = true;
      return true as T;
    }
    case "tail_logs": {
      const id = args!.id as string;
      return (serviceLogLines.get(id) ?? []).map((line) => ({ line })) as LogLine[] as T;
    }
    case "diagnose_port": {
      // 浏览器演示模式：不编造「被某某软件占用」的假结论，
      // 只声明这些端口在演示里是「本应用自己的服务在用」
      const port = args!.port as number;
      const own = [8080, 8443, 8180, 8444, 23306, 25432, 28017, 26379, 17890, 19090];
      if (own.includes(port)) {
        return {
          port,
          inUse: true,
          pid: 4528,
          processName: "NiceServBay (demo)",
          cmdline: "浏览器演示数据，非真实进程",
        } as PortDiagnosis as T;
      }
      return { port, inUse: false } as PortDiagnosis as T;
    }
    case "scan_ports": {
      const rows: PortScanEntry[] = [];
      for (const [serviceId, label, port] of ownPorts()) {
        const running = services.get(serviceId)?.state === "running";
        rows.push({
          serviceId,
          label,
          port,
          ownedBySelf: running,
          pid: running ? 4528 : undefined,
          processName: running ? "NiceServBay (demo)" : undefined,
          running,
          verdict: running ? "self" : "free",
        });
      }
      return rows as T;
    }
    case "list_backups":
      return [] as T;
    case "restore_backup":
      return "demo" as T;
    case "rebuild_hosts":
      return true as T;
    case "reissue_site_certs":
      return [] as T;
    case "kill_pid":
      return true as T;
    case "get_system_stats": {
      const last = statsHistory[statsHistory.length - 1];
      const point = {
        t: now(),
        cpu: Math.max(2, last.cpu + (Math.random() - 0.5) * 8),
        mem: Math.max(20, last.mem + (Math.random() - 0.5) * 2),
      };
      statsHistory.push(point);
      if (statsHistory.length > 60) statsHistory.shift();
      return {
        cpuPercent: point.cpu,
        memUsedMb: point.mem * 128,
        memTotalMb: 32 * 1024,
        diskFreeGb: 187,
        diskTotalGb: 953,
        history: statsHistory.slice(-60),
      } as SystemStats as T;
    }
    case "config_list":
      return [
        { kind: "nginx-main", label: "Nginx 主配置", description: "站点 vhost 是自动生成的；这里改全局项（worker、日志、gzip 等）", path: "C:\\NiceServBay\\etc\\nginx\\nginx.conf", exists: true, sizeBytes: 4096, language: "nginx", validated: true, usedByService: "nginx", requiresPackage: "nginx" },
        { kind: "php-ini", label: "php.ini", description: "PHP 运行时设置。扩展开关建议走「PHP 扩展」面板，那里有主动校验", path: "C:\\NiceServBay\\etc\\php\\8.3.33\\php.ini", exists: true, sizeBytes: 2048, language: "ini", validated: false, usedByService: "php", requiresPackage: "php" },
        { kind: "mysql-ini", label: "my.ini", description: "MySQL 服务配置（端口、缓冲池、字符集）", path: "C:\\NiceServBay\\etc\\mysql\\8.0.46\\my.ini", exists: true, sizeBytes: 1024, language: "ini", validated: false, usedByService: "mysql", requiresPackage: "mysql" },
        { kind: "redis-conf", label: "redis.conf", description: "Redis 配置（端口、持久化、内存上限）", path: "C:\\NiceServBay\\etc\\redis\\redis.conf", exists: false, sizeBytes: 0, language: "conf", validated: false, usedByService: "redis", requiresPackage: "redis" },
      ] as ConfigFileInfo[] as T;
    case "config_read": {
      const kind = args!.kind as string;
      if (kind === "nginx-main") {
        return `worker_processes  1;

events {
    worker_connections  1024;
}

http {
    include       mime.types;
    default_type  application/octet-stream;
    sendfile      on;
    keepalive_timeout  65;

    include sites/*.conf;
}
` as T;
      }
      if (kind === "php-ini") {
        return `[PHP]
engine=On
expose_php=Off
memory_limit=256M
error_reporting=E_ALL
display_errors=On

[Extensions]
extension=curl
extension=mbstring
extension=pdo_mysql
` as T;
      }
      return `[mysqld]
port=3306
character-set-server=utf8mb4
max_connections=200
` as T;
    }
    case "config_validate": {
      const content = args!.content as string;
      // 只做一个够用的示意：括号配平 + 结尾分号
      const issues: { line: number; severity: string; message: string }[] = [];
      const lines = content.split("\n");
      let depth = 0;
      lines.forEach((raw, i) => {
        const t = raw.split("#")[0].trim();
        if (!t) return;
        depth += (t.match(/\{/g) || []).length - (t.match(/\}/g) || []).length;
        const last = t[t.length - 1];
        if (![";", "{", "}"].includes(last)) {
          issues.push({ line: i + 1, severity: "error", message: `指令行缺少结尾分号 ;：${t}` });
        }
      });
      if (depth !== 0) {
        issues.push({ line: 0, severity: "error", message: `花括号没有配平：还差 ${depth} 个右花括号 }` });
      }
      return {
        ok: !issues.some((x) => x.severity === "error"),
        messages: [],
        issues,
      } as ConfigValidation as T;
    }
    case "config_save":
      return { ok: true, messages: [], issues: [] } as ConfigValidation as T;
    case "config_backups":
      return [
        { name: "nginx.conf.20260921-203045.bak", path: "C:\\NiceServBay\\backup\\config\\nginx.conf.20260921-203045.bak", sizeBytes: 4010, createdAt: Math.floor(Date.now() / 1000) - 3600 },
      ] as ConfigBackup[] as T;
    case "config_rollback":
      return true as T;
    case "scan_projects": {
      const root = args!.root as string;
      return [
        {
          path: `${root}\my-shop`,
          name: "my-shop",
          kind: "laravel",
          documentRoot: `${root}\my-shop\public`,
          siteKind: "php",
          rewrite: "laravel",
          phpMinVersion: ">=8.2",
          evidence: ["存在 artisan（Laravel 命令行入口）", "composer.json 要求 php >=8.2"],
          runHint: "需要 PHP + Composer；首次运行前执行 composer install",
          needsDevServer: false,
          suggestedDomain: "my-shop.test",
          alreadyConfigured: false,
        },
        {
          path: `${root}\admin-ui`,
          name: "admin-ui",
          kind: "next-js",
          documentRoot: `${root}\admin-ui\out`,
          siteKind: "node",
          rewrite: "spa-fallback",
          phpMinVersion: null,
          evidence: ["存在 next.config.js（Next.js）"],
          runHint: "需要 Node；开发用 npm run dev（本应用可按 Node 站点代理），静态导出用 npm run build + out 目录",
          needsDevServer: true,
          suggestedDomain: "admin-ui.test",
          alreadyConfigured: false,
        },
        {
          path: `${root}\landing`,
          name: "landing",
          kind: "static-html",
          documentRoot: `${root}\landing`,
          siteKind: "static",
          rewrite: "none",
          phpMinVersion: null,
          evidence: ["存在 index.html"],
          runHint: "纯静态，无需运行时",
          needsDevServer: false,
          suggestedDomain: "landing.test",
          alreadyConfigured: true,
        },
      ] as ScannedProject[] as T;
    }
    case "watchdog_status":
      return {
        enabled: settings.watchdogEnabled === true,
        maxAttempts: 5,
        intervalSec: 3,
        watched: Array.from(services.keys()).map((id) => ({
          id,
          enabled: true,
          attempts: 0,
          exhausted: false,
          restartCount: 0,
        })),
      } as WatchdogStatus as T;
    case "watchdog_set_enabled": {
      settings.watchdogEnabled = args!.enabled as boolean;
      return true as T;
    }
    case "watchdog_reset":
      return true as T;
    case "db_backup_list":
      return Array.from(mockDbBackups.values()).sort((a, b) => b.createdAt - a.createdAt) as T;
    case "db_backup_dir":
      return `C:\NiceServBay\backup\db` as T;
    case "db_backup_dump": {
      const dbs = args!.databases as string[];
      const name = (args!.outName as string | null) ?? `${dbs[0] ?? "db"}-${Date.now()}.sql`;
      const f: DbBackupFile = {
        name,
        path: `C:\NiceServBay\backup\db\${name}`,
        sizeBytes: 1024 * (40 + Math.floor(Math.random() * 400)),
        createdAt: Math.floor(Date.now() / 1000),
      };
      mockDbBackups.set(f.path, f);
      return f.path as T;
    }
    case "db_backup_restore":
      return { ok: true } as DbRestoreResult as T;
    case "db_backup_delete":
      mockDbBackups.delete(args!.path as string);
      return true as T;
    case "xdebug_status": {
      const version = args!.version as string;
      const exts = mockPhpExtState.get(version) ?? mockPhpExtSeed();
      const xd = exts.find((e) => e.name === "xdebug");
      return {
        version,
        build: {
          phpVersion: version,
          api: "20240924",
          ts: true,
          compiler: "VS17",
          arch: "x64",
        },
        dllPresent: false,
        enabled: !!xd?.enabled,
        loaded: false,
        loadedVersion: null,
        recommended: "3.4.1",
        dllCandidates: [`php_xdebug-3.4.1-${version.split(".").slice(0, 2).join(".")}-vs17-x86_64-ts.dll`],
        manualHint: `PHP ${version} · TS（线程安全） · VS17 · x64`,
        settings: { "xdebug.mode": "debug,develop", "xdebug.client_port": "9003" },
      } as XdebugStatus as T;
    }
    case "xdebug_setup": {
      const version = (args!.input as { version: string }).version;
      const exts = mockPhpExtState.get(version) ?? mockPhpExtSeed();
      const xd = exts.find((e) => e.name === "xdebug");
      if (xd) xd.enabled = true;
      mockPhpExtState.set(version, exts);
      return {
        version,
        installed: true,
        dllPath: `C:\NiceServBay\runtimes\php\${version}\ext\php_xdebug.dll`,
        loadedVersion: "3.4.1",
        warnings: [],
      } as XdebugSetupResult as T;
    }
    case "xdebug_toggle":
      return [] as string[] as T;
    case "php_extensions": {
      const version = args!.version as string;
      return mockPhpExtensions(version) as T;
    }
    case "set_php_extension": {
      const version = args!.version as string;
      const name = args!.name as string;
      const enabled = args!.enabled as boolean;
      const exts = mockPhpExtState.get(version) ?? mockPhpExtSeed();
      const target = exts.find((e) => e.name === name);
      if (target) target.enabled = enabled;
      mockPhpExtState.set(version, exts);
      return {
        name,
        enabled,
        warnings: [],
        needsRestart: false,
      } as PhpExtensionChange as T;
    }
    case "set_php_ini_toggle": {
      const version = args!.version as string;
      const key = args!.key as string;
      const value = args!.value as boolean;
      const toggles = mockPhpToggleState.get(version) ?? mockPhpToggleSeed();
      const t = toggles.find((x) => x.key === key);
      if (t) t.value = value;
      mockPhpToggleState.set(version, toggles);
      return true as T;
    }
    case "db_list":
      return Array.from(databases.values()) as T;
    case "db_create": {
      databases.set(args!.name as string, { name: args!.name as string, tables: 0, sizeKb: 0 });
      return true as T;
    }
    case "db_users":
      return Array.from(dbUsers.values()) as T;
    case "db_reset_root_password":
      return true as T;
    case "proxy_status":
      return {
        running: proxyRunning,
        mixedPort: 17890,
        controllerPort: 19090,
        mode: proxyMode,
        systemProxyEnabled: systemProxyOn,
        version: "v1.19.10",
      } as ProxyStatusInfo as T;
    case "proxy_start": {
      proxyRunning = true;
      return true as T;
    }
    case "proxy_stop": {
      proxyRunning = false;
      systemProxyOn = false;
      return true as T;
    }
    case "proxy_set_system": {
      systemProxyOn = args!.enabled as boolean;
      return true as T;
    }
    case "proxy_set_mode": {
      proxyMode = args!.mode as "rule" | "global" | "direct";
      return true as T;
    }
    case "proxy_profiles":
      return Array.from(proxyProfiles.values()) as T;
    case "proxy_import": {
      const p: ProxyProfile = {
        id: uid(),
        name: args!.name as string,
        url: args!.url as string,
        active: false,
        addedAt: now(),
      };
      proxyProfiles.set(p.id, p);
      return p as T;
    }
    case "proxy_nodes":
      return [
        {
          name: "PROXY",
          type: "Selector",
          now: "🇭🇰 香港 01",
          nodes: [
            { name: "🇭🇰 香港 01", type: "Shadowsocks", alive: true, history: [86, 92, 88] },
            { name: "🇭🇰 香港 02", type: "Vmess", alive: true, history: [120, 115, 130] },
            { name: "🇯🇵 日本 01", type: "Trojan", alive: true, history: [156, 149] },
            { name: "🇺🇸 美国 01", type: "Shadowsocks", alive: false, history: [] },
          ],
        },
        {
          name: "AUTO",
          type: "URLTest",
          now: "🇭🇰 香港 01",
          nodes: [
            { name: "🇭🇰 香港 01", type: "Shadowsocks", alive: true, history: [86] },
            { name: "🇯🇵 日本 01", type: "Trojan", alive: true, history: [156] },
          ],
        },
      ] as ProxyGroupView[] as T;
    case "proxy_select_node":
      return true as T;
    case "proxy_delay_test": {
      await delay(800);
      return Math.floor(60 + Math.random() * 220) as T;
    }
    case "get_settings":
      return { ...settings } as T;
    case "set_setting": {
      Object.assign(settings, { [args!.key as string]: args!.value });
      return true as T;
    }
    case "set_port_override": {
      const key = args!.key as string;
      const port = (args ?? {}).port as number | null | undefined;
      const next = { ...settings.portOverrides };
      if (port == null) delete next[key];
      else next[key] = port;
      settings.portOverrides = next;
      return true as T;
    }
    case "export_config":
      return 12 as T;
    case "import_config":
      return {
        sites: 0,
        skippedSites: 0,
        settings: 0,
        proxyProfiles: 0,
        stacks: 0,
        missingPackages: [],
      } as T;
    case "import_config_text": {
      // 演示模式：只校验 JSON 可解析，不做真实导入
      const raw = args!.json as string;
      try {
        const parsed = JSON.parse(raw) as { format?: string };
        if (!parsed?.format?.startsWith("niceservbay/")) {
          throw new Error("不是 NiceServBay 的备份文件");
        }
      } catch (e) {
        throw new Error(String((e as Error).message ?? e));
      }
      return {
        sites: 0,
        skippedSites: 0,
        settings: 0,
        proxyProfiles: 0,
        stacks: 0,
        missingPackages: [],
      } as T;
    }
    case "get_app_version":
      return "0.1.0" as T;
    case "get_data_dir":
      return "C:\\Users\\Demo\\AppData\\Local\\NiceServBay" as T;
    case "open_in_browser": {
      const url = args?.url as string | undefined;
      if (url && typeof window !== "undefined") window.open(url, "_blank", "noopener,noreferrer");
      return true as T;
    }
    case "open_in_folder":
      return true as T;
    case "check_updates":
      // 演示模式：报告一个可用新版，方便在浏览器里走通「检查更新 → 弹窗 → 下载」流程
      return {
        appVersion: "0.1.0",
        latestVersion: "0.2.0",
        releaseUrl: "https://github.com/nsmao-com/nice_env/releases",
        manifestRevision: 1,
        manifestUpdate: false,
        appUpdate: true,
        release: {
          tag: "v0.2.0",
          htmlUrl: "https://github.com/nsmao-com/nice_env/releases/tag/v0.2.0",
          body: "## 更新内容\n\n- 设置页新增主题色与字体自定义\n- 代码块支持行号 / 高亮 / 复制\n- 托盘菜单重新设计\n- 修复若干问题",
          publishedAt: new Date(now() - 86400_000).toISOString(),
          assetName: "NiceServBay_0.2.0_x64-setup.exe",
          assetUrl: "https://github.com/nsmao-com/nice_env/releases/download/v0.2.0/NiceServBay_0.2.0_x64-setup.exe",
          assetSize: 8_400_000,
        },
      } as T;
    case "download_update": {
      // 演示模式：走一遍进度事件（约 1.5s 从 0 到 100%）
      const total = 8_400_000;
      const steps = 24;
      for (let i = 1; i <= steps; i++) {
        await delay(60);
        emitLocal("update://progress", {
          received: Math.round((total * i) / steps),
          total,
          speedBps: 3_500_000,
          etaSec: 0,
          state: i === steps ? "downloaded" : "downloading",
        });
      }
      return {
        path: "C:\\Users\\demo\\AppData\\Roaming\\NiceServBay\\updates\\NiceServBay_0.2.0_x64-setup.exe",
        fileName: "NiceServBay_0.2.0_x64-setup.exe",
        sizeBytes: total,
      } as T;
    }
    case "install_update":
      return true as T;
    case "open_update_dir":
      return true as T;
    case "quit_app":
      return true as T;
    case "check_manifest_update":
      return false as T;
    default:
      throw new Error(`mock: 未实现的命令 ${cmd}`);
  }
}
