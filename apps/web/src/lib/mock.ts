/**
 * 浏览器 mock 后端：内存状态实现与 Rust 侧相同的命令面。
 * 仅用于 next dev 下的 UI 开发/演示；桌面端自动走真实 invoke。
 */
import { PackageManifestEntry } from "@nsb/schema";
import bundledManifest from "../../../../manifest/packages.win.json";
import type {
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
  CertAutomation,
  CertMonitor,
  CertReport,
  ImportedCert,
  EnvFileView,
  DiagnosticsBundle,
  HealthReport,
  BulkReport,
  BulkSelectionSummary,
  SiteBulkReport,
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
    iniPath: `C:\\NiceEnv\\etc\\php\\${version}\\php.ini`,
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
    const path = `C:\NiceEnv\backup\db\${name}`;
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

/** 环境变量注入的 mock 状态（浏览器里不碰真实 PATH） */
const mockPathEnv: { enabled: boolean; selected: string[] | null } = {
  enabled: false,
  selected: null,
};

const certs = new Map<string, CertRecord>();
/* 证书自动化（ACME）：mock 一条样例，覆盖列表/编辑/签发的浏览器预览 */
const certAutos = new Map<string, CertAutomation>();
function seedCertAutos() {
  if (certAutos.size > 0) return;
  certAutos.set("auto-demo", {
    id: "auto-demo",
    name: "demo.example.com",
    domains: ["demo.example.com", "*.demo.example.com"],
    email: "me@example.com",
    ca: "letsencrypt",
    dns: { kind: "aliyun", accessKey: "AKID…", secret: "…" },
    deployLocal: true,
    targets: [
      { id: "t1", kind: "btpanel", name: "我的宝塔",
        config: { url: "http://bt.example.com", apiSk: "…", siteName: "demo.example.com" },
        lastResult: { ok: true, message: "已将证书配置到宝塔站点 demo.example.com", at: Date.now() } },
      { id: "t2", kind: "aliyun", name: "阿里云 SSL",
        config: { accessKeyId: "AKID…", accessKeySecret: "…", region: "cn-hangzhou" },
        lastResult: { ok: false, message: "mock 示例：目标失败不影响其它目标", at: Date.now() } },
    ],
    enabled: true,
    state: "ok", lastError: "",
    keyAlg: "ec256", eabKid: "", eabHmacKey: "", cnameTarget: "",
    dnsWaitSec: 0, renewDaysAhead: 30, retryTimes: 3, retryIntervalMin: 30, failCount: 0,
    notifyKind: "dingtalk", notifyUrl: "https://oapi.dingtalk.com/robot/send?access_token=demo",
    notifySmtp: null,
    manualRecords: [],
    runs: [
      { at: Date.now() - 86400_000 * 3, ok: true, message: "签发成功", log: ["开始处理：demo.example.com", "ACME 签发成功，开始部署", "本地部署完成：…/certs/sites/demo.example.com.crt", "已上传到阿里云 SSL 证书服务（单号 12345）"] },
      { at: Date.now() - 86400_000 * 93, ok: true, message: "签发成功", log: ["开始处理：demo.example.com", "完成"] },
    ],
    certId: "acme-demo.example.com",
    issuedAt: Date.now() - 86400_000 * 3,
    expiresAt: Date.now() + 86400_000 * 87,
    nextRenewAt: Date.now() + 86400_000 * 57,
    lastRunAt: Date.now() - 86400_000 * 3,
    createdAt: Date.now() - 86400_000 * 3,
    updatedAt: Date.now() - 86400_000 * 3,
  });
}
const databases = new Map<string, DatabaseInfo>();
const dbUsers = new Map<string, DbUserInfo>();
const proxyProfiles = new Map<string, ProxyProfile>();
const cronJobs = new Map<string, { id: string; name: string; command: string; intervalMin: number; enabled: boolean; createdAt: number; lastRunAt: number | null; lastExit: string | null; lastOutput: string | null }>();
let mockTunnel: { id: string; port: number; url: string; startedAt: number; alive: boolean } | null = null;
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
  codeWrap: true,
  codeTheme: "auto",
  codeBg: "",
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
      // schema 里两个数组带 default，但 mock 不走 zod 解析，这里必须自己带上
      requires: [],
      missingRequires: [],
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

  // 浏览器复用正式清单，不再维护另一份过期版本表或编造上游历史。
  for (const raw of bundledManifest.packages) {
    const entry = PackageManifestEntry.parse(raw);
    const service = [...services.values()].find((s) => s.id.split("@")[0] === entry.id && s.version === entry.version);
    const installed = !!service;
    packages.set(`${entry.id}@${entry.version}`, {
      ...entry,
      availableVersions: bundledManifest.packages.filter((p) => p.id === entry.id).map((p) => p.version),
      active: installed,
      ...(installed ? { install: {
        version: entry.version,
        installPath: `…/runtimes/${entry.id}/${entry.version}`,
        configPath: `…/etc/${entry.id}/${entry.version}`,
        installedAt: now() - 86400_000 * 12,
      } } : {}),
    });
  }

  certs.set("ca", {
    id: "ca",
    kind: "ca",
    subject: "NiceEnv Local Root CA",
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
          processName: "NiceEnv (demo)",
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
      // 浏览器仅展示正式清单快照；实时上游查询由桌面端执行。
      const one = (id: string): VersionCatalog => ({
        id,
        remote: [],
        online: false,
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
            // 清单里的 requires 在 mock 里没有，给个空数组即可
            requires: [],
            missingRequires: [],
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
    case "log_export": {
      const sid = args!.serviceId as string;
      // 后端会强制 .log 后缀并清洗文件名，mock 也照做，避免演示时出现假路径
      const raw = (args!.suggestedName as string | null) ?? `${sid}-20260921-210000`;
      const base = raw.replace(/\.log$/, "");
      return `C:\\NiceEnv\\logs\\export\\${base}.log` as T;
    }
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
    case "sites_start_many":
    case "sites_stop_many": {
      const ids = args!.ids as string[];
      // 用命令名判断动作，别依赖一个不存在的参数
      const action = cmd === "sites_start_many" ? "start" : "stop";
      if (cmd === "sites_start_many") {
        for (const id of ids) {
          const st = sites.get(id);
          if (st) st.status = "running";
        }
      } else {
        for (const id of ids) {
          const st = sites.get(id);
          if (st) st.status = "stopped";
        }
      }
      return {
        action,
        succeeded: ids,
        already: [],
        failed: [],
      } as SiteBulkReport as T;
    }
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
        "# NiceEnv 诊断报告",
        "",
        "- 应用版本：0.1.0",
        `- 生成时间：${new Date().toLocaleString()}`,
        "- 操作系统：windows x86_64",
        "- 数据目录：C:\NiceEnv",
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
      return "C:\NiceEnv\diagnostics\niceenv-diagnostics-20260921-210000.md" as T;
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
          { id: "ca", kind: "ca", subject: "NiceEnv Local Root CA", sans: [], notAfter: now + 3600 * day, daysLeft: 3600, status: "ok", filePresent: true, usedBySites: [], missingSans: [], advice: "" },
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
    case "cert_import_dir":
      return {
        imported: [
          { certPath: "D:\mock\a.crt", keyPath: "D:\mock\a.key", subject: "a.example.com",
            sans: ["a.example.com"], notBefore: Date.now() - 86400_000 * 30, notAfter: Date.now() + 86400_000 * 60, daysLeft: 60 },
        ],
        skipped: ["b.crt：找不到同名私钥"],
      } as T;
    case "cert_imported_list":
      return [
        { certPath: "C:\NiceEnv\certs\imported\corp-wildcard.crt", keyPath: "C:\NiceEnv\certs\imported\corp-wildcard.key", subject: "*.corp.internal", sans: ["*.corp.internal", "corp.internal"], notBefore: 1700000000, notAfter: 1800000000, daysLeft: 210 },
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
          processName: "NiceEnv (demo)",
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
          processName: running ? "NiceEnv (demo)" : undefined,
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
        { kind: "nginx-main", label: "Nginx 主配置", description: "站点 vhost 是自动生成的；这里改全局项（worker、日志、gzip 等）", path: "C:\\NiceEnv\\etc\\nginx\\nginx.conf", exists: true, sizeBytes: 4096, language: "nginx", validated: true, usedByService: "nginx", requiresPackage: "nginx" },
        { kind: "php-ini", label: "php.ini", description: "PHP 运行时设置。扩展开关建议走「PHP 扩展」面板，那里有主动校验", path: "C:\\NiceEnv\\etc\\php\\8.3.33\\php.ini", exists: true, sizeBytes: 2048, language: "ini", validated: false, usedByService: "php", requiresPackage: "php" },
        { kind: "mysql-ini", label: "my.ini", description: "MySQL 服务配置（端口、缓冲池、字符集）", path: "C:\\NiceEnv\\etc\\mysql\\8.0.46\\my.ini", exists: true, sizeBytes: 1024, language: "ini", validated: false, usedByService: "mysql", requiresPackage: "mysql" },
        { kind: "redis-conf", label: "redis.conf", description: "Redis 配置（端口、持久化、内存上限）", path: "C:\\NiceEnv\\etc\\redis\\redis.conf", exists: false, sizeBytes: 0, language: "conf", validated: false, usedByService: "redis", requiresPackage: "redis" },
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
        { name: "nginx.conf.20260921-203045.bak", path: "C:\\NiceEnv\\backup\\config\\nginx.conf.20260921-203045.bak", sizeBytes: 4010, createdAt: Math.floor(Date.now() / 1000) - 3600 },
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
      return `C:\NiceEnv\backup\db` as T;
    case "db_backup_dump": {
      const dbs = args!.databases as string[];
      const name = (args!.outName as string | null) ?? `${dbs[0] ?? "db"}-${Date.now()}.sql`;
      const f: DbBackupFile = {
        name,
        path: `C:\NiceEnv\backup\db\${name}`,
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
        dllPath: `C:\NiceEnv\runtimes\php\${version}\ext\php_xdebug.dll`,
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
    case "proxy_connections": {
      const total = proxyRunning ? 128 + Math.floor(Math.random() * 40) : 0;
      return {
        downloadTotal: proxyRunning ? 8 * 1024 * 1024 * 1024 + total * 913 : 0,
        uploadTotal: proxyRunning ? 640 * 1024 * 1024 + total * 231 : 0,
        connections: proxyRunning
          ? [
              { id: uid(), upload: 91300, download: 41_300_000, start: 't', chains: ['🇭🇰 香港 01', 'PROXY'], metadata: { type: 'HTTPS', host: 'www.youtube.com', destinationIP: '142.250.7.106', destinationPort: '443' } },
              { id: uid(), upload: 51200, download: 18_800_000, start: 't', chains: ['🇯🇵 日本 01', 'PROXY'], metadata: { type: 'TLS', host: 'api.openai.com', destinationIP: '104.18.33.45', destinationPort: '443' } },
              { id: uid(), upload: 22000, download: 2_300_000, start: 't', chains: ['DIRECT'], metadata: { type: 'HTTP', host: 'cn.bing.com', destinationIP: '202.89.233.100', destinationPort: '80' } },
            ]
          : [],
      } as T;
    }
    case "proxy_update_profile": {
      const pid = args!.id as string;
      const old = proxyProfiles.get(pid);
      if (old) proxyProfiles.set(pid, { ...old });
      return true as T;
    }
    case "cron_jobs":
      return Array.from(cronJobs.values()) as T;
    case "cron_save": {
      const j = args!.job as { id: string; name: string; command: string; intervalMin: number; enabled: boolean; createdAt: number };
      const id = j.id || `cron-${Date.now()}`;
      cronJobs.set(id, { ...j, id, lastRunAt: null, lastExit: null, lastOutput: null });
      return true as T;
    }
    case "cron_delete":
      cronJobs.delete(args!.id as string);
      return true as T;
    case "cron_set_enabled": {
      const c = cronJobs.get(args!.id as string);
      if (c) c.enabled = args!.enabled as boolean;
      return true as T;
    }
    case "cron_run_now": {
      const c = cronJobs.get(args!.id as string);
      if (c) {
        c.lastRunAt = Date.now();
        c.lastExit = "exit 0";
        c.lastOutput = "（mock）任务已执行";
        return { ...c } as T;
      }
      throw new Error("cron job not found");
    }
    case "tunnel_start": {
      mockTunnel = {
        id: uid(),
        port: args!.port as number,
        url: `https://mock-${Math.floor(Math.random() * 9999)}.trycloudflare.com`,
        startedAt: Date.now(),
        alive: true,
      };
      return { ...mockTunnel } as T;
    }
    case "tunnel_list":
      return (mockTunnel ? [mockTunnel] : []) as T;
    case "tunnel_stop":
      mockTunnel = null;
      return true as T;
    case "ollama_models":
      return [
        { name: "qwen2.5:0.5b", digest: "a8b0c5e2d110", size: "398 MB", modified: "2 hours ago" },
        { name: "llama3.2:3b", digest: "de729584469e", size: "2.0 GB", modified: "3 days ago" },
      ] as T;
    case "ollama_delete":
    case "ollama_pull":
      return true as T;
    case "adminer_start":
      return { port: 8991, file: "adminer-6.1.0-en.php" } as T;
    case "adminer_stop":
      return true as T;
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
        certAutomations: 0, certMonitors: 0,
        missingPackages: [],
      } as T;
    case "import_config_text": {
      // 演示模式：只校验 JSON 可解析，不做真实导入
      const raw = args!.json as string;
      try {
        const parsed = JSON.parse(raw) as { format?: string };
        if (!parsed?.format?.startsWith("niceservbay/")) {
          throw new Error("不是 NiceEnv 的备份文件");
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
      return "C:\\Users\\Demo\\AppData\\Local\\NiceEnv" as T;
    case "open_in_browser": {
      const url = args?.url as string | undefined;
      if (url && typeof window !== "undefined") window.open(url, "_blank", "noopener,noreferrer");
      return true as T;
    }
    case "open_in_folder":
      return true as T;
    case "refresh_remote_manifest":
      return { revision: 2, packages: 160, path: "C:\\Users\\Demo\\AppData\\Local\\NiceEnv\\etc\\manifest.json", takesEffect: "restart" } as T;
    case "reset_remote_manifest":
      return true as T;
    case "check_updates":
      // 演示模式：报告一个可用新版，方便在浏览器里走通「检查更新 → 弹窗 → 下载」流程
      return {
        appVersion: "0.1.0",
        latestVersion: "0.2.0",
        releaseUrl: "https://github.com/nsmao-com/nice_env/releases",
        manifestRevision: 1,
        // 同时演示「套件清单有更新 → 应用新清单」
        manifestUpdate: true,
        appUpdate: true,
        release: {
          tag: "v0.2.0",
          htmlUrl: "https://github.com/nsmao-com/nice_env/releases/tag/v0.2.0",
          body: "## 更新内容\n\n- 设置页新增主题色与字体自定义\n- 代码块支持行号 / 高亮 / 复制\n- 托盘菜单重新设计\n- 修复若干问题",
          publishedAt: new Date(now() - 86400_000).toISOString(),
          assetName: "NiceEnv_0.2.0_x64-setup.exe",
          assetUrl: "https://github.com/nsmao-com/nice_env/releases/download/v0.2.0/NiceEnv_0.2.0_x64-setup.exe",
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
        path: "C:\\Users\\demo\\AppData\\Roaming\\NiceEnv\\updates\\NiceEnv_0.2.0_x64-setup.exe",
        fileName: "NiceEnv_0.2.0_x64-setup.exe",
        sizeBytes: total,
      } as T;
    }
    case "certauto_list":
      seedCertAutos();
      return [...certAutos.values()] as T;
    case "certauto_save": {
      seedCertAutos();
      const a = args!.a as CertAutomation;
      const next: CertAutomation = { ...a, id: a.id || `auto-${uid()}`, updatedAt: Date.now() };
      certAutos.set(next.id, next);
      return next as T;
    }
    case "certauto_delete": {
      certAutos.delete(args!.id as string);
      return true as T;
    }
    case "certauto_set_enabled": {
      const a0 = certAutos.get(args!.id as string);
      if (a0) certAutos.set(a0.id, { ...a0, enabled: args!.enabled as boolean });
      return a0 as T;
    }
    case "certauto_issue": {
      seedCertAutos();
      const a1 = certAutos.get(args!.id as string);
      if (a1) {
        const next = { ...a1, state: "ok", issuedAt: Date.now(), expiresAt: Date.now() + 86400_000 * 90,
          nextRenewAt: Date.now() + 86400_000 * 60, lastRunAt: Date.now() };
        certAutos.set(next.id, next);
        return next as T;
      }
      throw new Error("mock: 自动化不存在");
    }
    case "certmonitor_list":
      return [
        {
          id: "mon-demo", host: "demo.example.com", port: 443, name: "",
          state: "ok", issuer: "CN=R11, O=Let's Encrypt, C=US",
          expiresAt: Date.now() + 86400_000 * 62,
          lastChecked: Date.now() - 3600_000, lastError: "",
          createdAt: Date.now(), updatedAt: Date.now(),
        },
        {
          id: "mon-old", host: "old-router.lan", port: 443, name: "",
          state: "expiring", issuer: "CN=self-signed",
          expiresAt: Date.now() + 86400_000 * 5,
          lastChecked: Date.now() - 3600_000, lastError: "",
          createdAt: Date.now(), updatedAt: Date.now(),
        },
      ] as T;
    case "certmonitor_add":
      return { ...(args!.m as CertMonitor), id: `mon-${uid()}` } as T;
    case "certmonitor_delete":
      return true as T;
    case "certmonitor_check": {
      const m = args!.id as string;
      return {
        id: m, host: m, port: 443, name: "", state: "ok",
        issuer: "CN=R11, O=Let's Encrypt, C=US",
        expiresAt: Date.now() + 86400_000 * 62,
        lastChecked: Date.now(), lastError: "",
        createdAt: Date.now(), updatedAt: Date.now(),
      } as T;
    }
    case "cert_export_pfx":
      return "D:\\mock\\cert.pfx" as T;
    case "cert_export_der":
      return "D:\\mock\\cert.der" as T;
    case "cert_export_jks":
      return "D:\\mock\\cert.jks" as T;
    case "cert_export_pem":
      return "D:\\mock\\cert.pem" as T;
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
