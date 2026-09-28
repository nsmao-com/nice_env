/**
 * 浏览器 mock 后端：内存状态实现与 Rust 侧相同的命令面。
 * 仅用于 next dev 下的 UI 开发/演示；桌面端自动走真实 invoke。
 */
import { PackageManifestEntry, HostsEntry as HostsEntrySchema, DiagnosticsBundle as DiagnosticsBundleSchema } from "@nsb/schema";
import { normalizeError } from "./backend";
import type { ConfigCheck, BackupPreview, ConfigResetPreview, TunnelInfo, OllamaModelRow, OllamaPullStatus } from "./api";
import bundledManifest from "../../../../manifest/packages.win.json";
import type {
  DownloadProgress,
  VersionCatalog,
  ServiceStatus,
  Site,
  PackageView,
  ProjectRuntimeVersions,
  SystemStats,
  PortDiagnosis,
  PortScanEntry,
  PortRangeScan,
  ListenerInfo,
  ClosePortOutcome,
  Stack,
  StackInput,
  StackStartReport,
  HostsEntry,
  LogLine,
  DatabaseInfo,
  DatabaseEngine,
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
  SiteCertificateChoice,
  EnvFileView,
  EnvRestorePreview,
  DiagnosticsBundle,
  HealthReport,
  ServiceDiagnosticReport,
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
import type { SiteFileBackup, SiteFileScope, SiteFilePlan, BackupPlanConfig } from "./api";
import { cmpVersionDesc, resolveStackService, normalizeProxyTarget, isPhpSiteSettingValid, isEnvSecretKey, isEnvFileName, applicationRuntime, validApplication } from "./utils";

const delay = (ms: number) => new Promise((r) => setTimeout(r, ms));
const siteFileArchives = new Map<string, SiteFileBackup[]>();
const siteFilePlans = new Map<string, SiteFilePlan>();
function mockSiteFilePlan(id: string): SiteFilePlan {
  if (!sites.has(id)) throw { code: "SITE_NOT_FOUND", message: "站点已不存在" };
  const plan = siteFilePlans.get(id) ?? { status: { config: { enabled: false, frequency: "daily", time: "03:00", weekday: 0, monthDay: 1, keep: 10 }, nextAt: null, lastRunAt: null, finishedAt: null, state: "idle", message: "", files: [] }, project: true, excludeGenerated: true, scope: null, revision: "initial" };
  return structuredClone(plan);
}
function mockSiteFileScope(id: string, project: boolean, exclude: boolean): SiteFileScope {
  const site = sites.get(id);
  if (!site) throw { code: "SITE_NOT_FOUND", message: "站点已不存在，请刷新列表" };
  const root = project ? site.runtime.application?.cwd || site.rootDir.replace(/[/\\](public|out|dist|build)[/\\]?$/, "") : site.rootDir;
  return { root, revision: JSON.stringify([id, root, site.updatedAt, project, exclude]), excluded: exclude ? [".git", "node_modules", ".next", ".nuxt", ".venv", "venv", "__pycache__", "target"] : [] };
}

/** 浏览器预览使用的应用版本；桌面端版本由各端 manifest 注入。 */
const MOCK_APP_VERSION = "0.2.119";
const MOCK_NEXT_VERSION = "0.3.0";

const mockMongoDatabases = new Map<string, Record<string, Record<string, unknown>[]>>([
  ["niceenv_demo", { documents: Array.from({ length: 23 }, (_, i) => ({ _id: { $oid: (i+1).toString(16).padStart(24,"0") }, title: `预览文档 ${i+1}`, active: i%2 === 0, count: { $numberInt: String(i+1) }, createdAt: { $date: { $numberLong: "1790611200000" } } })) }],
]);
const mockMongoAuth = new Map<string, { view: import("@nsb/schema").MongoAuthView; password: string }>();
function mongoAuthPreview(version: string) {
  let item = mockMongoAuth.get(version);
  if (!item) { item = { view: { version, username: "", authDatabase: "admin", hasPassword: false, configured: false, running: false, authorization: null, hasUsers: false, administrator: false, problem: null, revision: "preview-0" }, password: "" }; mockMongoAuth.set(version, item); }
  const service = services.get("mongodb");
  item.view.running = service?.version === version && !!service.pids.length && ["running", "error"].includes(service.state);
  item.view.authorization = item.view.running ? item.view.configured : null;
  return item;
}
const mockMongoBackups = new Map<string, { record: import("@nsb/schema").MongoBackup; data: Record<string, Record<string, unknown>[]> }>();
let mockMongoBackupSequence = 0;
function mockMongoBackup(database: string, version: string, toolsVersion: string, kind: "manual" | "before-restore") {
  const data = mockMongoDatabases.get(database);
  if (!data) throw { code: "MONGO_BACKUP_INVALID", message: "数据库已不存在" };
  const id = `preview-${Date.now()}-${++mockMongoBackupSequence}`;
  const record: import("@nsb/schema").MongoBackup = { id, database, version, toolsVersion, createdAt: Date.now()/1000, sizeBytes: new TextEncoder().encode(JSON.stringify(data)).length, sha256: "0".repeat(64), kind };
  mockMongoBackups.set(id, { record, data: structuredClone(data) }); return structuredClone(record);
}

const certMonitors = new Map<string, CertMonitor>();
let monitorNotifications = { kind: "none", url: "" };

function monitorEndpoint(input: string, port: number): { host: string; port: number } {
  try {
    const raw = input.trim();
    if (!raw || raw.length > 2048 || /[\s\\]/.test(raw) || !Number.isInteger(port) || port < 1 || port > 65535) throw Error();
    const bareIPv6 = !raw.includes("://") && !raw.includes("[") && (raw.match(/:/g)?.length ?? 0) > 1;
    const url = new URL(raw.includes("://") ? raw : `https://${bareIPv6 ? `[${raw}]` : raw}`);
    const authority = raw.replace(/^https:\/\//i, "").split(/[/?#]/)[0];
    if (url.protocol !== "https:" || !url.hostname || url.username || url.password || (!bareIPv6 && authority.endsWith(":"))) throw Error();
    const explicitPort = !bareIPv6 && /:\d+$/.test(authority);
    const parsedPort = explicitPort || raw.includes("://") ? Number(url.port || 443) : port;
    if (!parsedPort) throw Error();
    return { host: url.hostname.replace(/^\[|\]$/g, "").replace(/\.+$/, "").toLowerCase(), port: parsedPort };
  } catch { throw { code: "BAD_HOST", message: "请填写域名、IP、host:port 或 HTTPS 地址；IPv6 带端口时使用 [::1]:8443" }; }
}

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
    mk("pdo", "PDO", "数据库", "PDO 抽象层（其它 pdo_* 的前置）", true, { builtin: true, dll: "" }),
    mk("mysqlnd", "MySQLnd", "数据库", "MySQL 原生驱动", true, { builtin: true, dll: "" }),
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
    mk("redis", "Redis", "缓存", "Redis 客户端（连接本地 Redis / 队列）", false),
    mk("igbinary", "igbinary", "缓存", "更紧凑的序列化，Redis/Session 可选", false),
  ];
}

const mockPhpExtState = new Map<string, PhpExtension[]>();
const mockPhpExtDependencies: Record<string, string[]> = {
  mysqli: ["mysqlnd"],
  pdo_mysql: ["pdo", "mysqlnd"],
  pdo_sqlite: ["pdo"],
  pdo_pgsql: ["pdo"],
};

function mockPhpExtensions(version: string): PhpExtensionView {
  if (!mockPhpExtState.has(version)) mockPhpExtState.set(version, mockPhpExtSeed());
  const extensions = mockPhpExtState.get(version)!;
  return {
    version,
    iniPath: `C:\\NiceEnv\\etc\\php\\${version}\\php.ini`,
    extensions: extensions.map((e) => ({
      ...e,
      missingDeps: (mockPhpExtDependencies[e.name] ?? []).filter((name) => !extensions.some((dependency) => dependency.name === name && dependency.enabled)),
    })),
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
const mockDbBackups = new Map<string, DbBackupFile>();
const mockBackupContents = new Map<string, DatabaseInfo[]>();

const now = () => Date.now();
const uid = () => Math.random().toString(36).slice(2, 10);

/* ---------- 内存状态 ---------- */

const services = new Map<string, ServiceStatus>();
const sites = new Map<string, Site>();
const mockEnvFiles = new Map<string, EnvFileView>();
const mockEnvBackups = new Map<string, { entries: EnvFileView["entries"]; revision: string }>();
const mockEnvBeforeRestore = new Map<string, EnvFileView["entries"]>();
let mockEnvRevision = 0;
function mockEnvView(siteId: string, fileName = ".env"): EnvFileView {
  if (!isEnvFileName(fileName)) throw { code: "BAD_ENV_FILE", message: "请选择 .env 或 .env.* 环境文件，不能选择路径、备份或编译缓存" };
  const site = sites.get(siteId);
  if (!site) throw { code: "SITE_NOT_FOUND", message: "站点不存在，请刷新列表" };
  const prefix = `${siteId}:${site.rootDir}:`;
  const key = prefix + fileName;
  let view = mockEnvFiles.get(key);
  if (!view) {
    const root = site.rootDir.replace(/[\\/](public|out|dist|build)[\\/]?$/, "").replace(/[\\/]+$/, "");
    const values = [["APP_NAME", site.name], ["APP_ENV", "local"], ["APP_KEY", "base64:preview-key"], ["APP_URL", `https://${site.domains[0]}`]];
    if (site.db?.enabled) values.push(["DB_HOST", "127.0.0.1"], ["DB_PORT", "3306"], ["DB_DATABASE", site.db.database], ["DB_USERNAME", site.db.username], ["DB_PASSWORD", "preview-password"]);
    view = { siteId, siteName: site.name, path: `${root}/${fileName}`, fileName, backupExists: false, hasCompiledEnv: false, exists: [".env", ".env.example"].includes(fileName), revision: `mock-env-${++mockEnvRevision}`,
      entries: values.map(([key, value], index) => ({ key, value, commented: false, secret: isEnvSecretKey(key), line: index + 1, needsQuote: false })),
      variants: [".env", ".env.example"] };
    view.entries.push({ key: "REDIS_PASSWORD", value: "preview", commented: true, secret: true, line: 20, needsQuote: false });
    if (!view.exists) view.entries = [];
    mockEnvFiles.set(key, view);
  }
  view.variants = [...new Set([".env", ".env.example", ...Array.from(mockEnvFiles).filter(([key, entry]) => key.startsWith(prefix) && entry.exists).map(([, entry]) => entry.fileName)])].sort();
  view.backupExists = mockEnvBackups.has(view.path);
  view.dbHint = site.db?.enabled ? { database: site.db.database, username: site.db.username, password: site.db.password ?? "", port: site.db.port ?? 3306 } : null;
  return view;
}
function mockEnvDbValues(view: EnvFileView): [string, string][] {
  const hint = view.dbHint;
  if (!hint) throw { code: "NO_DB_BINDING", message: "该站点没有绑定数据库" };
  const values: [string, string][] = [["DB_CONNECTION", "mysql"], ["DB_HOST", "127.0.0.1"], ["DB_PORT", String(hint.port)], ["DB_DATABASE", hint.database], ["DB_USERNAME", hint.username]];
  if (hint.password) values.push(["DB_PASSWORD", hint.password]);
  return values;
}

const packages = new Map<string, PackageView>();

/** 环境变量注入的 mock 状态（浏览器里不碰真实 PATH） */
const mockPathEnv: { enabled: boolean; selected: string[] | null; versions: Record<string, string> } = {
  enabled: false,
  selected: null,
  versions: {},
};

// 浏览器只在内存保存；同一项目目录的站点共用版本选择。
const mockProjectVersions = new Map<string, Record<string, string>>();
const mockProjectVersionFiles = new Map<string, Record<string, string>>();
const mockNodeLtsAliases = new Map<string, string>();
const terminalRuntimeLabel = (id: string, name: string) => {
  switch (id) { case "php": return "PHP"; case "node": return "Node.js"; case "python": return "Python"; case "go": return "Go"; default: return name; }
};
const mockProjectRoot = (site: Site) => site.rootDir.replace(/\\/g, "/").replace(/\/$/, "").replace(/\/(public|out|dist|build)$/, "");
function mockDetectedVersions(root: string): ProjectRuntimeVersions["detected"] {
  const files = mockProjectVersionFiles.get(root) ?? {};
  const numeric = (value: string, node: boolean) => {
    const text = node ? value.replace(/^v/, "") : value;
    return /^(0|[1-9]\d*)(\.(0|[1-9]\d*)){0,2}$/.test(text) ? text.split(".") : null;
  };
  return ([{ id: "node", names: [".nvmrc", ".node-version"] }, { id: "python", names: [".python-version"] }])
    .filter(({ names }) => names.some((name) => Object.hasOwn(files, name)))
    .map(({ id, names }) => {
      const present = names.filter((name) => Object.hasOwn(files, name));
      const requirements: string[] = [], constraints: string[][] = [];
      let issue: string | null = null;
      for (const name of present) {
        const lines = files[name].replace(/^\uFEFF/, "").split(/\r?\n/).map((line) => line.split("#")[0].trim()).filter((line) => line && !(name === ".nvmrc" && line.includes("=")));
        if (lines.length !== 1 || /\s|:/.test(lines[0])) { issue ??= `${name} 需要一个版本号；空文件或多个解释器请在项目版本页明确选择版本`; continue; }
        const value = lines[0], parts = numeric(value, id === "node");
        if (name === ".nvmrc" && /^lts\/(\*|[a-zA-Z-]{1,64})$/.test(value)) {
          const version = mockNodeLtsAliases.get(value.slice(4).toLowerCase());
          const resolved = version ? numeric(version, true) : null;
          requirements.push(`${name}: ${value}${resolved ? ` → ${version}` : ""}`);
          if (resolved?.length === 3) constraints.push(resolved);
          else issue ??= mockNodeLtsAliases.size ? `上次获取的官方索引中没有 ${value}，请刷新 Node.js 版本信息或检查代号` : "尚无可用的 Node.js LTS 信息，请点击刷新 Node.js 版本信息后重试";
          continue;
        }
        if (value.length > 128 || (!parts && !(name === ".nvmrc" && ["node", "stable"].includes(value)))) {
          issue ??= `${name} 使用了无法自动解析的版本写法；请在项目版本页选择已安装版本`; continue;
        }
        requirements.push(`${name}: ${value}`); if (parts) constraints.push(parts);
      }
      if (!issue && constraints.length === 2 && constraints[0].slice(0, Math.min(...constraints.map((parts) => parts.length))).some((part, index) => part !== constraints[1][index])) {
        issue = ".nvmrc 与 .node-version 的版本要求冲突，请统一文件或在项目版本页明确选择 Node.js 版本";
      }
      const resolvedVersion = issue ? null : Array.from(packages.values()).filter((p) => p.id === id && p.category === "runtime" && p.install)
        .filter((p) => { const parts = numeric(p.version, id === "node"); return parts?.length === 3 && constraints.every((constraint) => constraint.every((part, index) => part === parts[index])); })
        .sort((a, b) => cmpVersionDesc(a.version, b.version))[0]?.version ?? null;
      if (!issue && !resolvedVersion) issue = `${requirements.join("；")}：没有符合文件要求的已安装版本，请先安装或在项目版本页明确选择版本`;
      return { id, files: present, requirements, resolvedVersion, issue };
    });
}

function mockProjectReferences(site: Site, id: string, version: string): boolean {
  const root = mockProjectRoot(site), pinned = mockProjectVersions.get(root);
  if (pinned && Object.hasOwn(pinned, id)) return pinned[id] === version;
  const detected = mockDetectedVersions(root).find((entry) => entry.id === id);
  if (detected?.issue) throw { code: "PROJECT_RUNTIME_UNREADABLE", message: `无法检查站点「${site.name}」的项目版本，未卸载运行时`, hint: detected.issue };
  return detected?.resolvedVersion === version;
}

function mockProjectView(siteId: string): ProjectRuntimeVersions {
  const site = sites.get(siteId);
  if (!site) throw { code: "SITE_NOT_FOUND", message: "站点不存在，请刷新列表" };
  const root = mockProjectRoot(site);
  const versions = { ...mockProjectVersions.get(root) };
  const detected = mockDetectedVersions(root);
  const installed = Array.from(packages.values()).filter((p) => p.install && p.category === "runtime" && p.entry && !/\.(phar|php|jar|txt|json|toml|yaml|yml|md|ini)$/i.test(p.entry));
  const ids = [...new Set([...installed.map((p) => p.id), ...Object.keys(versions), ...detected.map((entry) => entry.id)])].sort();
  return { path: `${root}/.niceenv.json`, exists: mockProjectVersions.has(root), revision: JSON.stringify([siteId, root, mockProjectVersions.get(root) ?? null, mockProjectVersionFiles.get(root) ?? null, detected]), versions, detected,
    options: ids.map((id) => ({ id, label: terminalRuntimeLabel(id, Array.from(packages.values()).find((p) => p.id === id)?.displayName ?? id),
      versions: installed.filter((p) => p.id === id).map((p) => p.version).sort(cmpVersionDesc) })),
    sharedSites: Array.from(sites.values()).filter((other) => other.id !== siteId && mockProjectRoot(other) === root).map((other) => other.name),
    phpVersion: site.runtime.kind === "php" ? site.runtime.phpVersion ?? null : null,
  };
}

const certs = new Map<string, CertRecord>();
/* 证书自动化（ACME）：浏览器仅预览配置；签发明确要求桌面端。 */
const certAutos = new Map<string, CertAutomation>();
let certAutosSeeded = false;
function seedCertAutos() {
  if (certAutosSeeded) return;
  certAutosSeeded = true;
  certAutos.set("auto-demo", {
    id: "auto-demo",
    name: "demo.example.com",
    domains: ["demo.example.com", "*.demo.example.com"],
    email: "me@example.com",
    ca: "letsencrypt",
    dns: { kind: "aliyun", accessKey: "AKID…", secret: "…" },
    deployLocal: true,
    deploymentId: "preview-certificate",
    localDeployResult: { ok: true, message: "预览：本地部署已完成", at: Date.now() },
    targets: [
      { id: "t1", kind: "btpanel", name: "我的宝塔",
        config: { url: "http://bt.example.com", apiSk: "…", siteName: "demo.example.com" },
        lastResult: { ok: true, message: "已将证书配置到宝塔站点 demo.example.com", at: Date.now() } },
      { id: "t2", kind: "aliyun", name: "阿里云 SSL",
        config: { accessKeyId: "AKID…", accessKeySecret: "…", region: "cn-hangzhou" },
        lastResult: { ok: false, message: "mock 示例：目标失败不影响其它目标", at: Date.now() } },
    ],
    enabled: true,
    state: "deploy_error", lastError: "预览：一个部署目标未完成，可在桌面应用修正配置后重试部署。",
    keyAlg: "ec256", eabKid: "", eabHmacKey: "", cnameTarget: "",
    dnsWaitSec: 0, renewDaysAhead: 30, retryTimes: 3, retryIntervalMin: 30, failCount: 0,
    notifyKind: "dingtalk", notifyUrl: "https://oapi.dingtalk.com/robot/send?access_token=demo",
    notifySmtp: null,
    manualRecords: [],
    runs: [
      { at: Date.now() - 86400_000 * 3, ok: false, message: "预览：证书已签发，部署未完成", log: ["预览：本地部署完成", "预览：宝塔目标成功，阿里云目标失败"] },
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
const mockUserPasswordRevisions = new Map<string, string>();
const proxyProfiles = new Map<string, ProxyProfile>();
const cronJobs = new Map<string, { id: string; name: string; command: string; intervalMin: number; enabled: boolean; createdAt: number; lastRunAt: number | null; lastExit: string | null; lastOutput: string | null }>();
const mockRedisConnections = new Map<string, { username: string; password: string }>();
const mockPostgresConnections = new Map<string, { password: string; saved: string; passwordRequired: boolean }>();
const mockPostgresBackups = new Map<string, { file: DbBackupFile; database: import("./api").PostgresDatabaseInfo }>();
const mockPostgresPlans = new Map<string, import("./api").PostgresPlan>();
function previewPlan(version: string, engine = "postgresql") {
  version = `${engine}@${version}`;
  let plan = mockPostgresPlans.get(version);
  if (!plan) { plan = { config: { enabled: false, frequency: "daily", time: "03:00", weekday: 0, monthDay: 1, keep: 10 }, nextAt: null, lastRunAt: null, finishedAt: null, state: "idle", message: "", files: [] }; mockPostgresPlans.set(version, plan); }
  return plan;
}
function previewPlanNext(config: import("./api").PostgresPlanConfig) {
  const [hour, minute] = config.time.split(":").map(Number);
  for (let offset = 0; offset < 370; offset++) {
    const at = new Date(); at.setDate(at.getDate() + offset); at.setHours(hour, minute, 0, 0);
    if (at.getTime() <= Date.now()) continue;
    if (config.frequency === "weekly" && (at.getDay() + 6) % 7 !== config.weekday) continue;
    if (config.frequency === "monthly" && at.getDate() !== Math.min(config.monthDay, new Date(at.getFullYear(), at.getMonth() + 1, 0).getDate())) continue;
    return at.getTime();
  }
  throw { code: "BAD_BACKUP_TIME", message: "无法计算下一次备份时间" };
}
const mockPostgresManagement = new Map<string, { nextOid: number; databases: import("./api").PostgresDatabaseInfo[]; roles: import("./api").PostgresRoleInfo[]; passwords: Map<string, string> }>();
function postgresPreview(version: string) {
  let data = mockPostgresManagement.get(version);
  if (!data) {
    data = { nextOid: 100, databases: ["postgres", "template0", "template1"].map((name, index) => ({ oid: index + 1, name, owner: "postgres", encoding: "UTF8", sizeBytes: 8_388_608, protected: true, allowConnections: name !== "template0" })),
      roles: [{ oid: 10, name: "postgres", canLogin: true, connectionLimit: -1, superuser: true, createDb: true, createRole: true, replication: true, bypassRls: true, protected: true, databases: [] }], passwords: new Map() };
    mockPostgresManagement.set(version, data);
  }
  return data;
}
let mockAdminer: import("./api").AdminerStatus | null = null;
const mockTunnels = new Map<string, TunnelInfo>();
const mockOllamaModels = new Map<string, OllamaModelRow>([
  ["qwen2.5:0.5b", { name: "qwen2.5:0.5b", digest: "preview-qwen", size: 398_000_000, modified: "2026-09-26T00:00:00Z", parameters: "0.5B", quantization: "Q4_K_M" }],
  ["llama3.2:3b", { name: "llama3.2:3b", digest: "preview-llama", size: 2_000_000_000, modified: "2026-09-26T00:00:00Z", parameters: "3B", quantization: "Q4_K_M" }],
]);
let mockOllamaPull: OllamaPullStatus | null = null;
function updateMockOllamaPull() {
  const job = mockOllamaPull;
  if (!job || job.state !== "pulling") return;
  const elapsed = Date.now() - job.startedAt;
  if (elapsed < 1000) return;
  job.phase = "pulling preview-file"; job.digest = "preview-file"; job.total = 100_000_000;
  job.completed = Math.min(job.total, Math.round((elapsed - 1000) / 5000 * job.total));
  if (elapsed >= 6000) {
    job.state = "succeeded"; job.phase = "success"; job.endedAt = Date.now();
    const name = job.name.split("/").at(-1)?.includes(":") ? job.name : `${job.name}:latest`;
    mockOllamaModels.set(name, { name, digest: "preview-download", size: job.total, modified: new Date().toISOString(), parameters: "", quantization: "" });
  }
}

const hostsManaged = new Map<string, string[]>();
const mockTextFiles = new Map<string, string>();
const mockDnsInterfaces = ["Ethernet", "Wi-Fi"];
const mockDnsStatus = new Map<string, import("./api").DnsInterfaceStatus>(mockDnsInterfaces.map((name) => [name, {
  current: { interfaceId: name, automatic: name !== "Ethernet", servers: name === "Ethernet" ? ["9.9.9.9", "1.1.1.1"] : [] },
  backup: null, local: false,
}]));
const activeDownloads = new Set<string>();
const cancelledDownloads = new Set<string>();
const settings: AppSettings = {
  rewriteTemplates: [],
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
const mockServiceHistory: import("./api").ServiceHistoryEntry[] = [];
function pushMockServiceHistory(serviceId: string, detail: string) {
  mockServiceHistory.unshift({ ts: Date.now(), serviceId, detail });
  mockServiceHistory.splice(200);
}

function logLinesFor(id: string): string[] {
  if (id.startsWith("site:")) {
    if (!sites.has(id.slice(5))) throw { code: "SITE_NOT_FOUND", message: "站点不存在" };
  } else if (!services.has(id)) {
    throw { code: "UNKNOWN_SERVICE", message: "服务未注册或已卸载" };
  }
  return serviceLogLines.get(id) ?? [];
}

/** 浏览器预览导出真实下载文件，只包含当前演示数据。 */
function downloadLog(content: string, suggestedName: string): string {
  const stem = suggestedName.replace(/\.log$/i, "").replace(/[^a-zA-Z0-9._-]/g, "_").replace(/^[._]+|[._]+$/g, "");
  const name = `${stem || "log"}.log`;
  return downloadPreviewText(content, name);
}

function downloadPreviewText(content: string, name: string): string {
  const url = URL.createObjectURL(new Blob([content], { type: "text/plain;charset=utf-8" }));
  try {
    const link = document.createElement("a");
    link.href = url;
    link.download = name;
    link.click();
  } finally {
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return name;
}
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
    id: "php@8.3.17",
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
    id: "php@7.4.33",
    label: "PHP 7.4 (FPM)",
    state: "stopped",
    pids: [],
    version: "7.4.33",
    category: "runtime",
  });
  mk({
    id: "mysql@8.0.46",
    label: "MySQL 8.0",
    state: "running",
    pids: [10600],
    port: 23306,
    version: "8.0.46",
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
    runtime: { webServer: "nginx", kind: "php", phpVersion: "8.3.17" },
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
    runtime: { webServer: "nginx", kind: "php", phpVersion: "7.4.33" },
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

  // 已注册的历史版本仍是已安装版本；不为它们编造下载地址或校验和。
  for (const service of services.values()) {
    if (!service.version) continue;
    const id = service.id.split("@")[0];
    const key = `${id}@${service.version}`;
    if (packages.has(key)) continue;
    const template = [...packages.values()].find((p) => p.id === id);
    if (!template) continue;
    packages.set(key, {
      ...template, version: service.version, displayName: service.label, url: "", sha256: undefined,
      sizeBytes: 0, mirrors: [], active: false, availableVersions: [],
      install: { version: service.version, installPath: `…/runtimes/${id}/${service.version}`,
        configPath: `…/etc/${id}/${service.version}`, installedAt: now() - 86400_000 },
    });
  }
  for (const id of new Set([...packages.values()].map((p) => p.id))) {
    const group = [...packages.values()].filter((p) => p.id === id);
    const installed = group.filter((p) => p.install).sort((a, b) => cmpVersionDesc(a.version, b.version));
    const active = installed[0];
    for (const p of group) { p.active = p === active; p.availableVersions = group.map((p) => p.version).sort(cmpVersionDesc); }
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
  hostsManaged.set("shop.test", ["127.0.0.1"]);
  hostsManaged.set("admin.test", ["127.0.0.1"]);
  hostsManaged.set("api.test", ["127.0.0.1"]);

  serviceLogLines.set(
    "nginx",
    [
      `2026-09-18 10:02:11 [notice] nginx/${services.get("nginx")?.version ?? "unknown"} (win64) started`,
      "2026-09-18 10:02:11 [notice] config file …/etc/nginx/nginx.conf loaded",
      "2026-09-18 10:15:42 [info] 127.0.0.1 GET /index.php 200 12ms",
      "2026-09-18 10:16:03 [error] 43#0: *1181 upstream timed out (110: Connection timed out) while reading response header",
      "2026-09-18 10:16:05 [info] 127.0.0.1 GET /api/health 200 3ms",
    ]
  );
  const mysqlService = Array.from(services.values()).find((s) => s.id === "mysql" || s.id.startsWith("mysql@"));
  if (mysqlService) serviceLogLines.set(mysqlService.id, [
    "2026-09-18 10:02:14 [System] [MY-010931] [Server] … starting as process 10600",
    "2026-09-18 10:02:16 [System] [MY-013602] [Server] Channel mysqlx configured",
    "2026-09-18 10:02:16 [System] [MY-010931] ready for connections. Port: 23306",
  ]);
}
seed();
for (const service of services.values()) {
  if (service.state === "running") pushMockServiceHistory(service.id, "Stopped → Running");
}

// 浏览器预览也保留上次加载的站点端口，修改设置本身不会替换运行中入口。
function mockSiteUrl(site: Site) {
  const key = site.runtime.webServer === "apache" ? site.https ? "apacheHttps" : "apacheHttp" : site.https ? "https" : "http";
  const defaults = settings.portProfile === "safe" ? { http: 8080, https: 8443, apacheHttp: 8180, apacheHttps: 8444 }
    : { http: 80, https: 443, apacheHttp: 8080, apacheHttps: 8443 };
  const port = settings.portOverrides?.[key] ?? defaults[key];
  const domain = (site.domains.find((domain) => !domain.startsWith("*.")) ?? site.domains[0] ?? "localhost").replace(/^\*\./, "www.");
  return `${site.https ? "https" : "http"}://${domain}${port === (site.https ? 443 : 80) ? "" : `:${port}`}`;
}
for (const site of sites.values()) site.accessUrl = mockSiteUrl(site);

function mockSiteStatus(site: Site): Site["status"] {
  if (site.status !== "running") return site.status;
  const dependencies: string[] = [site.runtime.webServer];
  if (site.runtime.application) dependencies.push(`site-app:${site.id}`);
  if (site.runtime.kind === "php") {
    if (!site.runtime.phpVersion) return "unconfigured";
    dependencies.push(`php@${site.runtime.phpVersion}`);
  }
  const states = dependencies.map((id) => services.get(id)?.state);
  return states.includes("error") ? "error" : states.every((state) => state === "running") ? "running" : "stopped";
}

function mockApplicationBusy(id: string) {
  const service = services.get(`site-app:${id}`);
  return !!service && (!!service.pids.length || ["running", "starting", "stopping"].includes(service.state));
}

function validateMockApplication(runtime: Site["runtime"]) {
  const app = runtime.application;
  if (!app) return;
  const selected = applicationRuntime(runtime.kind);
  if (!selected || !validApplication(app, runtime.proxyTarget ?? "")) {
    throw { code: "APP_INVALID", message: "请检查运行时、入口参数和本机 HTTP 监听地址" };
  }
  if (!packages.get(`${selected.id}@${app.version}`)?.install) {
    throw { code: "APP_RUNTIME_UNAVAILABLE", message: "所选应用运行时尚未安装，请先安装或选择其他版本" };
  }
}

function registerMockApplication(site: Site) {
  const id = `site-app:${site.id}`;
  if (mockApplicationBusy(site.id)) return;
  if (!site.runtime.application) { services.delete(id); return; }
  validateMockApplication(site.runtime);
  const target = new URL(normalizeProxyTarget(site.runtime.proxyTarget ?? "")!);
  services.set(id, { id, label: `${site.name} · 应用`, state: "stopped", pids: [], requires: [], missingRequires: [],
    version: site.runtime.application.version, category: "runtime", port: Number(target.port || 80) });
}

/** 仅演示服务状态；每次失败还原本次涉及的服务，不模拟执行本机程序。 */
async function startMockSiteServices(site: Site) {
  validateMockApplication(site.runtime);
  const ids: string[] = [site.runtime.webServer];
  if (site.runtime.kind === "php" && site.runtime.phpVersion) ids.unshift(`php@${site.runtime.phpVersion}`);
  if (site.runtime.application) ids.unshift(`site-app:${site.id}`);
  const before = new Map(ids.map((id) => [id, structuredClone(services.get(id))]));
  try {
    registerMockApplication(site);
    for (const id of ids) await performServiceAction("start_service", id);
  } catch (error) {
    for (const [id, service] of before) { if (service) services.set(id, service); else services.delete(id); }
    throw error;
  }
}

/** 预览也按运行描述注册服务；Node/Python 等纯运行时只选择版本。 */
function refreshPackageSelection(id: string) {
  const all = Array.from(packages.values()).filter((p) => p.id === id);
  const installed = all.filter((p) => p.install).sort((a, b) => cmpVersionDesc(a.version, b.version));
  const active = installed.find((p) => p.active) ?? installed[0];
  for (const p of all) p.active = p === active;
  const wanted = new Map(installed.filter((p) => p.run && (p.run.singleInstance === false || p === active))
    .map((p) => [p.run?.singleInstance === false ? `${id}@${p.version}` : id, p]));
  for (const sid of services.keys()) {
    if (sid.split("@")[0] === id && !wanted.has(sid)) services.delete(sid);
  }
  for (const [sid, p] of wanted) {
    const current = services.get(sid);
    if (current && (current.state !== "stopped" || current.version === p.version)) continue;
    services.set(sid, {
      id: sid, label: p.displayName, state: "stopped", pids: [],
      requires: p.run?.requires ?? [], missingRequires: [],
      version: p.version, category: p.category, port: p.defaultPort,
    });
  }
}

type PreviewMySql = { databases: Map<string, DatabaseInfo>; users: Map<string, DbUserInfo>; password: string; savedPassword: string };
const previewMySql = new Map<string, PreviewMySql>();
const systemDatabase = (name: string) => ["mysql", "sys", "information_schema", "performance_schema"].includes(name.toLowerCase());
function mysqlPreview(version?: string, requireAuth = true, engine: DatabaseEngine = "mysql") {
  const service = Array.from(services.values()).find((s) => (s.id === engine || s.id.startsWith(`${engine}@`)) && (!version || s.version === version));
  if (!service || !(service.state === "running" || (service.state === "error" && service.pids.length > 0))) throw { code: "MYSQL_NOT_RUNNING", message: "请先启动所选数据库实例" };
  const key = `${engine}@${service.version}`;
  let state = previewMySql.get(key);
  if (!state) {
    const password = `preview-${uid()}-${uid()}`;
    state = { databases: key === "mysql@8.0.46" ? databases : new Map([["mysql", { name: "mysql", tables: 37, sizeKb: 2411 }]]), users: key === "mysql@8.0.46" ? dbUsers : new Map([["root@localhost", { username: "root", host: "localhost" }]]), password, savedPassword: password };
    previewMySql.set(key, state);
  }
  if (requireAuth && state.password !== state.savedPassword) throw { code: "MYSQL_AUTH_REQUIRED", message: "请更新本机连接密码" };
  return { service, state };
}
const mockDatabaseGrants = new Map<string, import("./api").DatabaseGrants>();
const mockGrantKey = (engine: DatabaseEngine, version: string, username: string, host: string) => JSON.stringify([engine, version, username, host]);
const mockGrantPrivileges = ["SELECT", "INSERT", "UPDATE", "DELETE", "CREATE", "DROP", "REFERENCES", "INDEX", "ALTER", "CREATE TEMPORARY TABLES", "LOCK TABLES", "CREATE VIEW", "SHOW VIEW", "CREATE ROUTINE", "ALTER ROUTINE", "EXECUTE", "EVENT", "TRIGGER"];
const mockLiteralScope = (name: string) => name.replace(/\\/g, "\\\\").replace(/[_%]/g, "\\$&");
function mysqlGrantsPreview(engine: DatabaseEngine, version: string, username: string, host: string) {
  const { state } = mysqlPreview(version, true, engine);
  const account = [...state.users.values()].find((item) => item.username === username && item.host === host);
  if (!account) throw { code: "DB_USER_MISSING", message: "所选账号已不存在" };
  const key = mockGrantKey(engine, version, username, host);
  const databases = [...state.databases.keys()].filter((name) => !systemDatabase(name)).sort();
  let data = mockDatabaseGrants.get(key);
  if (!data) {
    const database = account.grants?.match(/^ALL ON (.+)\.\*$/)?.[1];
    const available = engine === "mariadb" ? [...mockGrantPrivileges, "DELETE HISTORY"] : [...mockGrantPrivileges];
    data = { username, host, databases, available, scopes: database ? [{ scope: database, label: database, pattern: /[_%]/.test(database), protected: systemDatabase(database), privileges: [...available], grantOption: false, extraPrivileges: [] }] : [], mariadb: engine === "mariadb", partialRevokes: false, globalPrivileges: username === "root", protected: !username || !host || /^(root$|mysql\.|mariadb\.sys$)/i.test(username), revision: uid() };
    mockDatabaseGrants.set(key, data);
  }
  if (data.databases.join("\n") !== databases.join("\n")) { data.databases = databases; data.revision = uid(); }
  return data;
}
function previewBackup(version: string, data: DatabaseInfo[], label: string, engine: DatabaseEngine = "mysql") {
  const name = `${engine}-${version}-${label}-${Date.now()}-${uid()}.sql`;
  const path = `C:/NiceEnv/backup/db/${name}`;
  const file = { name, path, sizeBytes: Math.max(256, data.reduce((sum, db) => sum + (db.sizeKb ?? 0) * 1024, 0)), createdAt: Math.floor(Date.now() / 1000) };
  mockDbBackups.set(path, file); mockBackupContents.set(path, structuredClone(data));
  return file;
}
const previewSource = [{ name: "legacy_app", tables: 5, sizeKb: 128 }, { name: "wordpress_import", tables: 12, sizeKb: 256 }];

/* ---------- 命令实现 ---------- */

const proxyGroups: ProxyGroupView[] = [
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
];

let proxyRunning = false;
let systemProxyOn = false;
let proxyMode: "rule" | "global" | "direct" = "rule";

const configPreviewContent = new Map<string, string>();
const configPreviewHistory: (ConfigBackup & { content: string })[] = [];

function defaultConfigContent(key: string): string {
  switch (key.split("@")[0]) {
    case "nginx-main": return `worker_processes  1;

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
`;
    case "php-ini": return `[PHP]
engine=On
expose_php=Off
memory_limit=256M
error_reporting=E_ALL
display_errors=On

[Extensions]
extension=curl
extension=mbstring
extension=pdo_mysql
`;
    case "mysql-ini": return `[mysqld]
port=3306
character-set-server=utf8mb4
max_connections=200
`;
    case "mariadb-ini": return `[mysqld]
port=3306
character-set-server=utf8mb4
max_connections=200
`;
    case "redis-conf": return "bind 127.0.0.1\nport 6379\n";
    case "apache-conf": return 'ServerName localhost\nListen 8080\n';
    case "postgres-conf": return `# PostgreSQL preview configuration
listen_addresses = '127.0.0.1'
port = 5432
max_connections = 100
`;
    case "mongo-conf": return `# MongoDB preview configuration
# NiceEnv supplies dbPath, port, bindIp, logpath and authorization at launch.
operationProfiling:
  mode: off
`;
    default: throw { code: "BAD_KIND", message: "找不到对应配置" };
  }
}

function currentConfigContent(kind: string) {
  return configPreviewContent.get(kind) ?? defaultConfigContent(kind);
}

function savePreviewConfig(kind: string, content: string, expected?: string) {
  const previous = currentConfigContent(kind);
  if (expected !== undefined && expected !== previous) throw { code: "CONFIG_CONFLICT", message: "配置已被其他操作修改，当前草稿未覆盖文件" };
  if (content === previous) return;
  const name = `${kind}-${Date.now()}-${configPreviewHistory.length}.bak`;
  configPreviewHistory.unshift({ name, path: `C:/NiceEnv/backup/config/${name}`, sizeBytes: new TextEncoder().encode(previous).length, createdAt: Math.floor(Date.now() / 1000), target: kind, content: previous });
  configPreviewContent.set(kind, content);
}

let serviceActionInProgress = false;

/** 演示启停也保持操作互斥和重启的停止/启动顺序。 */
async function runServiceAction(action: "start_service" | "stop_service" | "restart_service", id: string) {
  return withServiceOperation(() => performServiceAction(action, id));
}

const serviceRunRevisions = new Map<string, number>();
function stopPreview(id: string) {
  const service = services.get(id);
  if (!service) throw { code: "UNKNOWN_SERVICE", message: "服务未注册或已卸载" };
  if (serviceActionInProgress || ["starting", "stopping"].includes(service.state)) throw { code: "SERVICE_BUSY", message: "服务正在操作，请稍后重新读取" };
  return { service: structuredClone(service), revision: JSON.stringify([id, service.version, service.port, service.pids, serviceRunRevisions.get(id) ?? 0]) };
}

async function withServiceOperation<T>(operation: () => Promise<T>): Promise<T> {
  if (serviceActionInProgress) throw { code: "SERVICE_BUSY", message: "服务正在操作，请稍后重试" };
  serviceActionInProgress = true;
  try { return await operation(); }
  finally { serviceActionInProgress = false; }
}

async function performServiceAction(action: "start_service" | "stop_service" | "restart_service", id: string) {
  const service = services.get(id);
  if (!service) throw { code: "UNKNOWN_SERVICE", message: `服务 ${id} 未注册或已卸载` };
  if (["starting", "stopping"].includes(service.state)) throw { code: "SERVICE_BUSY", message: "服务正在切换状态，请稍后重试" };
  const beforeState = service.state;
  let stopping = true;
  try {
    if (action !== "start_service" && (service.state !== "stopped" || service.pids.length)) {
      if (id === "coredns") mockDnsStatus.forEach((status) => {
        if (status.backup) { status.current = status.backup; status.backup = null; status.local = false; }
      });
      service.state = "stopping";
      await delay(500);
      service.state = "stopped";
      service.pids = [];
      if (id === "mihomo") proxyRunning = false;
    }
    stopping = false;
    if (action !== "stop_service" && service.state !== "running") {
      if (service.state === "error" && service.pids.length) throw { code: "SERVICE_BUSY", message: "服务仍有进程，请先停止后重试" };
      if (id.startsWith("site-app:")) {
        const site = sites.get(id.slice("site-app:".length));
        if (!site?.runtime.application) throw { code: "APP_NOT_MANAGED", message: "此站点未开启应用进程托管" };
        validateMockApplication(site.runtime);
        const defaults = settings.portProfile === "safe" ? { http: 8080, https: 8443, apacheHttp: 8180, apacheHttps: 8444 }
          : { http: 80, https: 443, apacheHttp: 8080, apacheHttps: 8443 };
        const ports = site.runtime.webServer === "apache" ? ["apacheHttp", "apacheHttps"] as const : ["http", "https"] as const;
        if (ports.some((key) => (settings.portOverrides?.[key] ?? defaults[key]) === service.port)) {
          throw { code: "APP_WEB_PORT_CONFLICT", message: "应用监听端口与站点 Web 服务相同，请为应用选择另一个端口" };
        }
        if (Array.from(services.values()).some((other) => other.id !== id && other.port === service.port && other.state === "running")) {
          throw { code: "PORT_IN_USE", message: "应用监听端口已被其他演示服务占用" };
        }
        serviceLogLines.set(id, ["[浏览器演示] 模拟应用启停；未读取项目文件或执行本机程序。"]);
      }
      service.state = "starting";
      service.lastError = undefined;
      await delay(700);
      service.state = "running";
      serviceRunRevisions.set(id, (serviceRunRevisions.get(id) ?? 0) + 1);
      service.pids = [Math.floor(Math.random() * 40000) + 1000];
      service.uptimeSec = 0;
      if (id === "mihomo") proxyRunning = true;
    }
    if (action === "restart_service" || beforeState !== service.state) {
      pushMockServiceHistory(id, action === "restart_service" ? "Restarted" : action === "start_service" ? "Stopped → Running" : "Running → Stopped");
    }
    return true;
  } catch (failure) {
    const error = normalizeError(failure);
    if (action === "restart_service") error.message = `${stopping ? "重启中止，停止阶段失败" : "服务已停止，但重新启动失败"}：${error.message}`;
    service.state = "error"; service.lastError = error;
    pushMockServiceHistory(id, `Error · ${error.message}`);
    throw error;
  }
}

const redisSettingsPreview = new Map<string, import("@nsb/schema").RedisSettingsView>();
const redisPasswordsPreview = new Map<string, import("@nsb/schema").RedisPasswordView>();
const redisServerPasswordsPreview = new Map<string, string>();
const redisPersistencePreview = new Map<string, { report: import("@nsb/schema").RedisPersistence; finishAt: number; minimumSaveTime: number }>();
const redisBackupsPreview: import("@nsb/schema").RedisBackup[] = [];
let redisRestoreRevisionPreview = 0;

export async function mockInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  await delay(60 + Math.random() * 120);
  switch (cmd) {
    case "list_service_status":
      return structuredClone(Array.from(services.values())) as T;
    case "service_history":
      return structuredClone(mockServiceHistory.slice(0, Math.min(200, Number(args?.n) || 200))) as T;
    case "service_web_url":
    case "repair_service_web_ui":
      throw { code: "DESKTOP_ONLY", message: "浏览器预览不能确认本机服务的管理台地址，请使用桌面端" };
    case "sftpgo_config_directories":
    case "select_sftpgo_config":
      throw { code: "DESKTOP_ONLY", message: "请在桌面应用中检查和选择 SFTPGo 配置目录；网页预览无法读取本机文件" };
    case "start_service":
    case "stop_service":
    case "restart_service":
      return await runServiceAction(cmd, String(args?.id ?? "")) as T;
    case "service_stop_preview":
      return stopPreview(String(args?.id ?? "")) as T;
    case "force_stop_service": {
      const id = String(args?.id ?? "");
      const preview = stopPreview(id);
      if (preview.revision !== args?.revision) throw { code: "SERVICE_TARGET_CHANGED", message: "服务进程或版本已变化，请重新读取状态并确认" };
      return await runServiceAction("stop_service", id) as T;
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
      if (!input.name.trim()) throw { code: "BAD_STACK", message: "栈名称不能为空" };
      if (!input.items.length) throw { code: "BAD_STACK", message: "栈里至少要有一个服务" };
      if (input.id && !prev) throw { code: "STACK_NOT_FOUND", message: "此服务栈已被删除，请关闭编辑器后刷新列表" };
      if (prev?.builtin) throw { code: "STACK_BUILTIN", message: "内置预设不能直接修改" };
      const stack: Stack = {
        id,
        name: input.name.trim(),
        description: input.description ?? "",
        items: [...input.items].sort((a, b) => a.order - b.order).filter((item, index, all) => all.findIndex((other) => other.serviceId === item.serviceId) === index),
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
      if (!s) throw { code: "STACK_NOT_FOUND", message: "找不到服务栈" };
      if (s.builtin) throw { code: "STACK_BUILTIN", message: "内置预设不能删除" };
      stacks.delete(args!.id as string);
      return true as T;
    }
    case "start_stack":
    case "stop_stack": {
      return withServiceOperation(async () => {
      const stack = stacks.get(args!.id as string);
      if (!stack) throw { code: "STACK_NOT_FOUND", message: "找不到服务栈" };
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
      const seen = new Set<string>();
      for (const item of ordered) {
        const found = resolveStackService(item.serviceId, [...services.values()], [...packages.values()]);
        if (!found) {
          report.skipped.push(item.serviceId);
          continue;
        }
        if (seen.has(found.id)) continue;
        seen.add(found.id);
        if (["starting", "stopping"].includes(found.state)) {
          report.failed.push({ serviceId: found.id, error: { code: "SERVICE_BUSY", message: `服务 ${found.id} 正在切换状态，请稍后重试` } });
          continue;
        }
        if (starting && found.state === "running") {
          report.alreadyRunning.push(found.id);
          continue;
        }
        if (!starting && !found.pids.length && !["running", "starting", "stopping"].includes(found.state)) {
          found.state = "stopped";
          report.alreadyRunning.push(found.id);
          continue;
        }
        try {
          await performServiceAction(starting ? "start_service" : "stop_service", found.id);
          report.started.push(found.id);
        } catch (error) { report.failed.push({ serviceId: found.id, error: normalizeError(error) }); }
      }
      if (starting && !seen.size) throw { code: "STACK_EMPTY", message: `「${stack.name}」里没有可启动的服务，请先安装所需套件` };
      return report as T;
      });
    }
    case "scan_port_range": {
      const from = Math.min(Number(args?.from), Number(args?.to));
      const to = Math.max(Number(args?.from), Number(args?.to));
      if (![from, to].every((port) => Number.isInteger(port) && port >= 1 && port <= 65535)) throw { code: "BAD_PORT", message: "端口必须为 1 到 65535 的整数" };
      const listeners: ListenerInfo[] = [];
      for (const [index, service] of [...services.values()].entries()) {
        if (service.state !== "running" || service.port == null || service.port < from || service.port > to) continue;
        listeners.push({ port: service.port, pid: service.pids[0] ?? 5000 + index,
          processName: service.label, cmdline: `${service.id} — 浏览器演示数据，非真实进程`,
          ownedBySelf: true, serviceId: service.id, ownership: "self", processStartedAt: 1, processStartMarker: `preview:${service.pids[0] ?? 5000 + index}`, canClose: true });
      }
      return { from, to, listeners, scannedAt: Date.now() } as PortRangeScan as T;
    }
    case "close_port": {
      const port = Number(args?.port);
      const expected = args?.expected as ListenerInfo[];
      if (!Array.isArray(expected)) throw { code: "BAD_PORT_TARGETS", message: "请先扫描并选择监听者" };
      const before = await mockInvoke<PortRangeScan>("scan_port_range", { from: port, to: port });
      const selected: ListenerInfo[] = [];
      for (const target of expected) {
        const current = before.listeners.find((row) => row.pid === target.pid);
        if (!current) continue;
        if (target.port !== port || !target.processStartMarker || current.processStartMarker !== target.processStartMarker || current.serviceId !== target.serviceId || current.ownership !== target.ownership) throw { code: "PORT_TARGET_CHANGED", message: "监听者已变化，请重新扫描" };
        selected.push(current);
      }
      for (const serviceId of new Set(selected.map((row) => row.serviceId))) await mockInvoke("stop_service", { id: serviceId });
      const remaining = (await mockInvoke<PortRangeScan>("scan_port_range", { from: port, to: port })).listeners;
      return { port, graceful: selected.length > 0, serviceId: selected[0]?.serviceId, killedPids: selected.map((row) => row.pid), portFree: remaining.length === 0, remaining, errors: [] } as ClosePortOutcome as T;
    }
    case "project_runtime_versions":
      return mockProjectView(args!.siteId as string) as T;
    case "save_project_runtime_versions": {
      const { siteId, versions, expectedRevision } = args as { siteId: string; versions: Record<string, string>; expectedRevision: string };
      const view = mockProjectView(siteId);
      if (view.revision !== expectedRevision) throw { code: "PROJECT_RUNTIME_CHANGED", message: "项目版本文件或目录已变化，未覆盖当前配置", hint: "草稿已保留，请重新读取后核对。" };
      for (const detected of view.detected) {
        if (detected.issue && !Object.hasOwn(versions, detected.id)) throw { code: "PROJECT_RUNTIME_DETECTION", message: detected.issue, hint: "请选择已安装版本覆盖此项，或修正版本文件后重新读取。" };
      }
      for (const [id, version] of Object.entries(versions)) {
        if (!view.options.find((option) => option.id === id)?.versions.includes(version)) throw { code: "TERMINAL_RUNTIME_UNAVAILABLE", message: `${id} ${version} 尚未安装，请改选已安装版本或取消固定` };
      }
      mockProjectVersions.set(mockProjectRoot(sites.get(siteId)!), { ...versions });
      return mockProjectView(siteId) as T;
    }
    case "terminal_environment":
    case "pathenv_status":
    case "pathenv_set_enabled":
    case "pathenv_set_selected":
    case "pathenv_set_version":
    case "pathenv_reapply": {
      // 浏览器 mock：用已装包派生一份状态，不碰真实 PATH
      if (cmd === "pathenv_set_enabled" && args && typeof args.enabled === "boolean") {
        mockPathEnv.enabled = args.enabled;
      }
      if (cmd === "pathenv_set_selected" && args && Array.isArray(args.ids)) {
        mockPathEnv.selected = args.ids as string[];
      }
      const installed = Array.from(packages.values()).filter((p) => p.install && p.entry && !/\.(phar|php|jar|txt|json|toml|yaml|yml|md|ini)$/i.test(p.entry));
      const pathVersion = (id: string) => mockPathEnv.versions[id]
        ?? installed.find((p) => p.id === id && p.active)?.version
        ?? installed.find((p) => p.id === id)?.version;
      if (cmd === "pathenv_set_version") {
        const { id, version, selected } = args as { id: string; version: string; selected: boolean };
        if (!installed.some((p) => p.id === id && p.version === version)) {
          throw { code: "PATH_VERSION_UNAVAILABLE", message: "该版本尚未安装，或没有可加入环境变量的命令" };
        }
        if (selected || pathVersion(id) === version) {
          const ids = mockPathEnv.enabled ? (mockPathEnv.selected ?? [...new Set(installed.map((p) => p.id))]) : [];
          mockPathEnv.selected = ids.filter((value) => value !== id);
          if (selected) {
            mockPathEnv.versions[id] = version;
            mockPathEnv.selected.push(id);
            mockPathEnv.enabled = true;
          }
        }
      }
      const entries = installed
        .map((p) => {
          const parent = p.entry.replace(/\\/g, "/").split("/").slice(0, -1).join("/");
          const binDir = `${p.install!.installPath.replace(/[\\/]$/, "")}${parent ? `/${parent}` : ""}`;
          const selected =
            (mockPathEnv.selected === null || mockPathEnv.selected.includes(p.id)) && pathVersion(p.id) === p.version;
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
      if (cmd === "terminal_environment") {
        const siteId = args?.siteId as string | undefined;
        const site = siteId ? sites.get(siteId) : undefined;
        if (siteId && !site) throw { code: "SITE_NOT_FOUND", message: "站点不存在，请刷新列表" };
        let chosen = entries.filter((entry) => entry.selected);
        const required = site ? { ...mockProjectVersions.get(mockProjectRoot(site)) } : {};
        const sources: Record<string, string> = Object.fromEntries(Object.keys(required).map((id) => [id, ".niceenv.json"]));
        for (const detected of site ? mockDetectedVersions(mockProjectRoot(site)) : []) {
          if (Object.hasOwn(required, detected.id)) continue;
          if (!detected.resolvedVersion) throw { code: "PROJECT_RUNTIME_DETECTION", message: detected.issue ?? "无法确定项目运行时版本", hint: "请在项目版本页选择已安装版本覆盖此项，或修正版本文件后刷新。" };
          required[detected.id] = detected.resolvedVersion; sources[detected.id] = detected.files.join(" + ");
        }
        if (site?.runtime.kind === "php" && !required.php) {
          const php = entries.find((entry) => entry.id === "php" && entry.version === site.runtime.phpVersion);
          if (!php) throw { code: "TERMINAL_RUNTIME_UNAVAILABLE", message: `站点指定的 PHP ${site.runtime.phpVersion ?? ""} 尚未安装，请先安装或更改站点设置` };
          chosen = [...chosen.filter((entry) => entry.id !== "php"), php];
        }
        for (const [id, version] of Object.entries(required)) {
          const selected = entries.find((entry) => entry.id === id && entry.version === version);
          if (!selected) throw { code: "TERMINAL_RUNTIME_UNAVAILABLE", message: `项目指定的 ${id} ${version} 尚未安装，请在项目版本中改选或取消固定` };
          chosen = [...chosen.filter((entry) => entry.id !== id), selected];
        }
        const prioritized = (id: string) => Object.hasOwn(required, id) || (id === "php" && site?.runtime.kind === "php");
        chosen.sort((a, b) => Number(prioritized(b.id)) - Number(prioritized(a.id)) || a.id.localeCompare(b.id));
        const cwd = site ? site.rootDir.replace(/[\\/](public|out|dist|build)[\\/]?$/, "") : "…";
        const quoted = chosen.map((entry) => `'${entry.binDir.replace(/['‘’‚‛]/g, (quote) => quote + quote)}'`).join(",\n    ");
        return {
          shell: "powershell",
          cwd,
          revision: `mock-terminal-${JSON.stringify([siteId, cwd, chosen, mockPathEnv.versions, required, site ? mockProjectView(site.id).revision : null])}`,
          script: chosen.length ? `# Browser demo paths — generate the actual script in the desktop app.
& {
  $nsbDirs = @(
    ${quoted}
  )
  $nsbKeys = @($nsbDirs | ForEach-Object { $_.Replace('/', '\\').TrimEnd('\\') })
  $nsbRest = @()
  if ($env:PATH) {
    $nsbRest = @($env:PATH.Split(';') | Where-Object {
      $nsbKeys -notcontains $_.Replace('/', '\\').TrimEnd('\\')
    })
  }
  $env:PATH = (@($nsbDirs) + $nsbRest) -join ';'
}` : "",
          entries: chosen.map(({ id, label, version, binDir }) => ({ id, label: terminalRuntimeLabel(id, label), version, binDir, source: sources[id] })),
          warnings: Object.entries(mockPathEnv.versions)
            .filter(([id, version]) => !required[id] && !(id === "php" && site?.runtime.kind === "php") && (mockPathEnv.selected === null || mockPathEnv.selected.includes(id))
              && !installed.some((p) => p.id === id && p.version === version))
            .map(([id]) => `${id}：所选 PATH 版本已卸载，请重新选择版本`),
        } as T;
      }
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
      return structuredClone(Array.from(packages.values())) as T;
    case "cancel_download": {
      const taskId = args?.taskId as string | undefined;
      if (!taskId) return false as T;
      const matches = Array.from(activeDownloads).filter((key) => key === taskId || (!taskId.includes("@") && key.startsWith(`${taskId}@`)));
      if (matches.length !== 1) return false as T;
      cancelledDownloads.add(matches[0]);
      return true as T;
    }
    case "set_active_version": {
      const id = args!.id as string;
      const version = args!.version as string;
      const target = packages.get(`${id}@${version}`);
      if (!target?.install) throw { code: "NOT_INSTALLED", message: `${id} ${version} 尚未安装` };
      const current = services.get(id);
      if (current && current.version !== version && current.state !== "stopped") {
        throw { code: "SERVICE_BUSY", message: `${id} 正在运行或启停中，请先停止再切换版本` };
      }
      for (const p of packages.values()) if (p.id === id) p.active = p === target;
      refreshPackageSelection(id);
      return true as T;
    }
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
      const requested = args!.id as string;
      const p = requested.includes("@") ? packages.get(requested)
        : Array.from(packages.values()).filter((pkg) => pkg.id === requested).sort((a, b) => cmpVersionDesc(a.version, b.version))[0];
      if (!p) throw { code: "PACKAGE_NOT_FOUND", message: `找不到套件 ${requested}` };
      const key = `${p.id}@${p.version}`;
      if (activeDownloads.has(key)) throw { code: "DOWNLOAD_BUSY", message: `${key} 正在安装` };
      activeDownloads.add(key);
      const total = p.sizeBytes || 0;
      const report = (state: DownloadProgress["state"], ratio: number, error?: string) => emitLocal("download://progress", {
        taskId: key, received: Math.round(total * ratio), total,
        speedBps: state === "downloading" ? total / 2 : 0, etaSec: 0, state, error,
      } satisfies DownloadProgress);
      const checkCancelled = () => {
        if (cancelledDownloads.has(key)) throw { code: "CANCELLED", message: "安装已取消" };
      };
      try {
        // 浏览器预览沿用桌面端事件结构；仅模拟阶段，不下载或写入本机文件。
        for (const ratio of [0, 0.25, 0.5, 0.75, 1]) {
          report("downloading", ratio);
          await delay(200);
          checkCancelled();
        }
        for (const state of ["verifying", "extracting"] as const) {
          report(state, 1);
          await delay(150);
          checkCancelled();
        }
        // 与原生提交阶段一致，此后不再接受取消。
        activeDownloads.delete(key);
        report("configuring", 1);
        p.install = {
          version: p.version,
          installPath: `…/runtimes/${p.id}/${p.version}`,
          configPath: `…/etc/${p.id}/${p.version}`,
          installedAt: now(),
        };
        refreshPackageSelection(p.id);
        report("installed", 1);
        return true as T;
      } catch (e) {
        const error = normalizeError(e);
        report(error.code === "CANCELLED" ? "cancelled" : "error", 0, error.message);
        throw e;
      } finally {
        activeDownloads.delete(key);
        cancelledDownloads.delete(key);
      }
    }
    case "uninstall_package": {
      const key = args!.id as string;
      const p = packages.get(key);
      if (!p?.install) throw { code: "NOT_INSTALLED", message: `${key} 尚未安装` };
      const installed = Array.from(packages.values()).filter((p) => p.install);
      const hasAlternative = installed.some((other) => other.id === p.id && other.version !== p.version);
      const referencesTarget = (dep: string) => dep.includes("@") ? dep === key : dep === p.id && !hasAlternative;
      const usedBy = [
        ...Array.from(sites.values()).filter((site) =>
          (p.id === "php" && site.runtime.kind === "php" && site.runtime.phpVersion === p.version)
          || (applicationRuntime(site.runtime.kind)?.id === p.id && site.runtime.application?.version === p.version)
          || (p.category === "runtime" && mockProjectReferences(site, p.id, p.version))
          || ((site.runtime.webServer ?? "nginx") === p.id && !hasAlternative)
          || (p.id === "mysql" && site.db?.enabled
            && (site.db.version != null ? site.db.version === p.version : !hasAlternative))
        ).map((site) => site.name),
        ...Array.from(stacks.values()).filter((stack) => !stack.builtin
          && stack.items.some((item) => referencesTarget(item.serviceId))).map((stack) => stack.name),
        ...installed.filter((other) => other !== p && [...(other.requires ?? []), ...(other.depends ?? []), ...(other.run?.requires ?? [])]
          .some(referencesTarget)).map((other) => other.displayName),
      ];
      if (usedBy.length > 0) throw { code: "PACKAGE_IN_USE", message: `无法卸载 ${key}：仍被 ${usedBy.join("、")} 使用` };
      const sid = p.run?.singleInstance === false ? `${p.id}@${p.version}` : p.id;
      if (services.get(sid)?.version === p.version) services.delete(sid);
      p.install = undefined;
      p.active = false;
      refreshPackageSelection(p.id);
      return true as T;
    }
    case "list_sites":
      return Array.from(sites.values()).map((site) => ({ ...site,
        status: mockSiteStatus(site),
        accessUrl: mockSiteStatus(site) === "running" ? site.accessUrl : undefined,
      })) as T;
    case "site_access_url": {
      const site = sites.get(args?.id as string);
      if (!site) throw { code: "SITE_NOT_FOUND", message: "站点不存在" };
      if (mockSiteStatus(site) !== "running" || !site.accessUrl) {
        throw { code: "SITE_URL_UNAVAILABLE", message: "站点未运行或尚未确认本次加载的访问地址", hint: "请启动或重启该站点后重试。" };
      }
      return site.accessUrl as T;
    }
    case "create_site": {
      return withServiceOperation(async () => {
      const input = args!.input as CreateSiteInput;
      if (args?.existingProject && (input.template !== "none" || input.writeEnvExample)) {
        throw { code: "EXISTING_PROJECT_WRITE", message: "使用已有项目时不能生成模板或改写项目配置" };
      }
      if (input.runtime.kind !== "php" && input.runtime.kind !== "static" && !normalizeProxyTarget(input.runtime.proxyTarget ?? "")) {
        throw { code: "BAD_PROXY_TARGET", message: "请填写有效的 HTTP/HTTPS 代理地址，不能包含账号、查询参数或片段" };
      }
      validateMockApplication(input.runtime);
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
      try { await startMockSiteServices(sites.get(id)!); }
      catch (error) { sites.delete(id); services.delete(`site-app:${id}`); throw error; }
      if (input.createDb) databases.set(input.createDb.database, { name: input.createDb.database, tables: 0, sizeKb: 0 });
      sites.get(id)!.accessUrl = mockSiteUrl(sites.get(id)!);
      input.domains.filter((d) => !d.startsWith("*.")).forEach((d) => hostsManaged.set(d, ["127.0.0.1"]));
      return sites.get(id) as T;
      });
    }
    case "update_site": {
      return withServiceOperation(async () => {
      const patch = args!.site as Partial<Site> & { id: string };
      const s = sites.get(patch.id);
      if (!s) throw { code: "SITE_NOT_FOUND", message: "站点不存在，请刷新列表" };
      const next = { ...s, name: patch.name?.trim() ?? s.name, domains: patch.domains ?? s.domains,
        rootDir: patch.rootDir?.trim() ?? s.rootDir, runtime: patch.runtime ?? s.runtime,
        https: patch.https ?? s.https, rewrite: patch.rewrite ?? s.rewrite,
        phpOverrides: patch.phpOverrides ?? s.phpOverrides, updatedAt: now() };
      if (mockApplicationBusy(s.id) && (JSON.stringify(next.runtime.application) !== JSON.stringify(s.runtime.application)
        || next.runtime.kind !== s.runtime.kind || next.rootDir !== s.rootDir || next.runtime.proxyTarget !== s.runtime.proxyTarget)) {
        throw { code: "APP_RUNNING", message: "应用正在运行，请先停止站点再修改入口、参数、目录、运行时或监听地址" };
      }
      validateMockApplication(next.runtime);
      if (next.runtime.kind === "php" && Object.entries(next.phpOverrides ?? {}).some(([key, value]) => !isPhpSiteSettingValid(key, value, s.phpOverrides?.[key]))) {
        throw { code: "BAD_PHP_OVERRIDE", message: "PHP 设置不受支持或值无效，请检查后重试" };
      }
      if (next.runtime.kind !== "php" && next.runtime.kind !== "static" && !normalizeProxyTarget(next.runtime.proxyTarget ?? "")) {
        throw { code: "BAD_PROXY_TARGET", message: "请填写有效的 HTTP/HTTPS 代理地址，不能包含账号、查询参数或片段" };
      }
      if (mockSiteStatus(s) === "running") {
        sites.set(s.id, next);
        try { await startMockSiteServices(next); }
        finally { sites.set(s.id, s); }
      }
      for (const domain of s.domains) {
        if (!next.domains.includes(domain) && ![...sites.values()].some((other) => other.id !== s.id && other.domains.includes(domain))) hostsManaged.delete(domain);
      }
      for (const domain of next.domains.filter((d) => !d.startsWith("*."))) hostsManaged.set(domain, ["127.0.0.1"]);
      Object.assign(s, next);
      registerMockApplication(s);
      s.accessUrl = mockSiteStatus(s) === "running" ? mockSiteUrl(s) : undefined;
      return { ...s, status: mockSiteStatus(s) } as T;
      });
    }
    case "delete_site": {
      return withServiceOperation(async () => {
      const id = args!.id as string;
      const s = sites.get(id);
      if (!s) throw { code: "SITE_NOT_FOUND", message: "站点不存在" };
      if (services.has(`site-app:${id}`)) await performServiceAction("stop_service", `site-app:${id}`);
      const others = Array.from(sites.values()).filter((site) => site.id !== id);
      if (args?.hosts !== false) {
        s.domains.forEach((domain) => {
          if (!others.some((site) => site.domains.includes(domain))) hostsManaged.delete(domain);
        });
      }
      if (args?.certs !== false && !s.runtime.importedCertId && !s.runtime.acmeCertId) {
        const cert = Array.from(certs.values()).find((cert) => cert.kind === "site" && cert.subject === s.domains[0]);
        if (cert && !others.some((site) => !site.runtime.importedCertId && site.domains[0] === cert.subject)
          && !Array.from(certAutos.values()).some((automation) => automation.domains[0] === cert.subject)) {
          certs.delete(cert.id);
        }
      }
      sites.delete(id);
      services.delete(`site-app:${id}`);
      return true as T;
      });
    }
    case "start_site":
    case "stop_site": {
      return withServiceOperation(async () => {
      const s = sites.get(args!.id as string);
      if (!s) throw { code: "SITE_NOT_FOUND", message: "站点不存在" };
      if (s) {
        if (cmd === "start_site") {
          await startMockSiteServices(s);
          s.accessUrl = mockSiteUrl(s);
        } else if (services.has(`site-app:${s.id}`)) {
          await performServiceAction("stop_service", `site-app:${s.id}`);
        }
        s.status = cmd === "start_site" ? "running" : "stopped";
      }
      return true as T;
      });
    }
    case "read_hosts": {
      const list: HostsEntry[] = [];
      hostsManaged.forEach((ips, domain) => ips.forEach((ip) => list.push({ ip, domain, managed: true })));
      return list as T;
    }
    case "apply_hosts": {
      const entries = (args?.entries as HostsEntry[] | undefined) ?? [];
      const expected = args?.expectedEntries as HostsEntry[] | undefined;
      const current = Array.from(hostsManaged, ([domain, ips]) => ips.map((ip) => ({ ip, domain, managed: true }))).flat();
      const snapshot = (list: HostsEntry[]) => JSON.stringify(list.map((e) => JSON.stringify([e.ip, e.domain, e.managed])).sort());
      if (expected && snapshot(expected) !== snapshot(current)) throw { code: "HOSTS_CHANGED", message: "hosts 内容已变化，请刷新并核对后重试" };
      const siteDomains = new Set(Array.from(sites.values()).flatMap((site) => site.domains.filter((d) => !d.startsWith("*."))));
      const next = new Map<string, string[]>();
      for (const entry of entries.filter((entry) => entry.managed)) {
        const result = HostsEntrySchema.safeParse(entry);
        if (!result.success) throw { code: "HOSTS_INVALID", message: "请填写有效的 IPv4/IPv6 地址与主机名" };
        const { ip, domain } = result.data;
        if (siteDomains.has(domain) && ip !== "127.0.0.1") throw { code: "HOSTS_SITE_MANAGED", message: `${domain} 由站点自动维护，请到站点设置修改` };
        const ips = next.get(domain) ?? [];
        if (!ips.includes(ip)) ips.push(ip);
        next.set(domain, ips);
      }
      siteDomains.forEach((domain) => next.set(domain, ["127.0.0.1"]));
      hostsManaged.clear();
      next.forEach((ips, domain) => hostsManaged.set(domain, ips));
      return true as T;
    }
    case "read_text_file": {
      const path = args?.path as string | undefined;
      if (!path) throw { code: "BAD_PATH", message: "文件路径不能为空" };
      const saved = mockTextFiles.get(path);
      if (saved !== undefined) return saved as T;
      if (/hosts(?:\.txt)?$/i.test(path)) {
        return Array.from(hostsManaged, ([domain, ips]) => ips.map((ip) => `${ip}\t${domain}`)).flat().join("\n") as T;
      }
      throw { code: "FILE_NOT_FOUND", message: "演示环境中找不到该文件" };
    }
    case "write_text_file": {
      const path = args?.path as string | undefined;
      const content = args?.content as string | undefined;
      if (!path) throw { code: "BAD_PATH", message: "文件路径不能为空" };
      if (typeof content !== "string") throw { code: "BAD_CONTENT", message: "文件内容无效" };
      mockTextFiles.set(path, content);
      return true as T;
    }
    case "dns_interfaces":
      return [...mockDnsInterfaces] as T;
    case "dns_status_of": {
      const name = args?.name as string | undefined;
      if (!name || !mockDnsStatus.has(name)) throw { code: "DNS_INTERFACE_NOT_FOUND", message: "找不到网络接口" };
      return structuredClone(mockDnsStatus.get(name)!) as T;
    }
    case "dns_takeover": {
      const name = args?.name as string | undefined;
      if (!name || !mockDnsStatus.has(name)) throw { code: "DNS_INTERFACE_NOT_FOUND", message: "找不到网络接口" };
      const service = services.get("coredns");
      if (service?.state !== "running" || service.port !== 53) throw { code: "DNS_NOT_READY", message: "接管前请先让 CoreDNS 在 53 端口运行" };
      const status = mockDnsStatus.get(name)!;
      if (status.local && status.backup) return true as T;
      if (status.backup || status.local) throw { code: "DNS_BACKUP_EXISTS", message: "请先恢复并核对原配置" };
      status.backup = structuredClone(status.current);
      status.current = { interfaceId: name, automatic: false, servers: ["127.0.0.1"] };
      status.local = true;
      return true as T;
    }
    case "dns_restore": {
      const name = args?.name as string | undefined;
      if (!name || !mockDnsStatus.has(name)) throw { code: "DNS_INTERFACE_NOT_FOUND", message: "找不到网络接口" };
      const status = mockDnsStatus.get(name)!;
      if (!status.backup && !args?.automatic) throw { code: "DNS_NO_BACKUP", message: "没有接管前配置记录" };
      status.current = status.backup ?? { interfaceId: name, automatic: true, servers: [] };
      status.backup = null;
      status.local = false;
      return true as T;
    }
    case "log_export": {
      const sid = args!.serviceId as string;
      logLinesFor(sid);
      const content = args!.content as string;
      if (!content.trim()) throw { code: "EMPTY_LOG", message: "没有可导出的日志内容" };
      return downloadLog(content, (args!.suggestedName as string | null) ?? `${sid}-${Date.now()}`) as T;
    }
    case "export_log": {
      const id = args?.id as string | undefined;
      const dest = args?.dest as string | undefined;
      if (!id || !dest) throw { code: "BAD_EXPORT", message: "缺少日志服务或目标路径" };
      const lines = logLinesFor(id);
      if (!lines.length) throw { code: "LOG_EMPTY", message: "尚未生成日志文件", hint: "请先启动服务或访问该站点" };
      const content = `${lines.join("\n")}\n`;
      mockTextFiles.set(dest, content);
      downloadLog(content, dest);
      return new TextEncoder().encode(content).byteLength as T;
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
      const report: SiteBulkReport = { action, succeeded: [], already: [], failed: [] };
      for (const id of new Set(ids)) {
        const site = sites.get(id);
        if (!site) { report.failed.push({siteId: id, error: {code: "SITE_NOT_FOUND", message: "站点不存在"}}); continue; }
        const running = mockSiteStatus(site) === "running";
        if (action === "start" ? running : site.status === "stopped" && !mockApplicationBusy(id)) { report.already.push(id); continue; }
        try {
          await mockInvoke(action === "start" ? "start_site" : "stop_site", { id });
          report.succeeded.push(id);
        } catch (error) { report.failed.push({siteId: id, error: normalizeError(error)}); }
      }
      return report as T;
    }
    case "bulk_start":
    case "bulk_stop":
    case "bulk_restart": {
      return withServiceOperation(async () => {
      const ids = [...new Set(args!.ids as string[])];
      const action = cmd.slice(5) as "start" | "stop" | "restart";
      const tier = (id: string) => {
        if (id.startsWith("site-app:")) return 1;
        const base = id.split("@")[0];
        if (["mysql", "mariadb", "redis", "postgresql", "mongodb", "memcached", "qdrant", "neo4j", "rabbitmq", "elasticsearch", "meilisearch", "zincsearch", "minio", "rustfs", "consul", "etcd", "r-nacos", "temporal"].includes(base)) return 0;
        if (["php", "node", "python", "go", "java", "dotnet", "bun", "deno", "ruby", "rust", "zig", "flutter", "perl", "erlang", "ollama"].includes(base)) return 1;
        if (["nginx", "apache", "caddy", "frankenphp", "tomcat", "roadrunner", "mihomo"].includes(base)) return 2;
        return 3;
      };
      const order = [...ids].sort((a, b) => action === "stop" ? tier(b) - tier(a) : tier(a) - tier(b));
      const report: BulkReport = { action, succeeded: [], already: [], failed: [], order };
      const execute = async (id: string, operation: "start" | "stop") => {
        const service = services.get(id);
        if (!service) throw { code: "UNKNOWN_SERVICE", message: `服务 ${id} 未注册或已卸载` };
        if (["starting", "stopping"].includes(service.state)) {
          throw { code: "SERVICE_BUSY", message: `服务 ${id} 正在切换状态，请稍后重试` };
        }
        const already = operation === "start" ? service.state === "running"
          : !service.pids.length && !["running", "starting", "stopping"].includes(service.state);
        await performServiceAction(operation === "start" ? "start_service" : "stop_service", id);
        return already;
      };
      if (action === "restart") {
        for (const id of [...ids].sort((a, b) => tier(b) - tier(a))) {
          try { await execute(id, "stop"); }
          catch (error) { report.failed.push({ serviceId: id, error: normalizeError(error) }); }
        }
      }
      for (const id of order) {
        if (report.failed.some((f) => f.serviceId === id)) continue;
        try {
          const already = await execute(id, action === "stop" ? "stop" : "start");
          (already ? report.already : report.succeeded).push(id);
        } catch (error) { report.failed.push({ serviceId: id, error: normalizeError(error) }); }
      }
      return report as T;
      });
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
    case "diagnose_service": {
      const service = services.get(args!.id as string);
      if (!service) throw { code: "UNKNOWN_SERVICE", message: "该服务已卸载或未注册，请刷新服务列表" };
      const snapshot = structuredClone(service);
      const stateLabel = { running: "运行中", stopped: "已停止", starting: "正在启动", stopping: "正在停止", error: "发生错误", unknown: "未知" }[snapshot.state];
      const lines = logLinesFor(snapshot.id).slice(-300);
      return {
        service: snapshot, checkedAt: Math.floor(Date.now() / 1000),
        checks: [
          { id: "status", state: snapshot.state === "error" ? "error" : ["unknown", "starting", "stopping"].includes(snapshot.state) ? "unavailable" : "info", method: "demo",
            detail: snapshot.state === "error" ? snapshot.lastError?.message || "演示服务处于错误状态" : `当前演示状态：${stateLabel}；未读取本机进程`, lines: [] },
          { id: "port", state: "unavailable", method: "demo", detail: "浏览器不能读取本机端口与进程归属，请在桌面端重新诊断", lines: [] },
          { id: "config", state: "unavailable", method: "demo", detail: "浏览器未读取本机配置或执行原生校验，请在桌面端重新诊断", lines: [] },
          { id: "logs", state: "info", method: "demo", detail: lines.length ? `当前演示日志有 ${lines.length} 行，以下仅展示演示内容；未读取本机日志` : "当前没有演示日志记录；未读取本机日志", lines: lines.slice(-3).map((line) => line.slice(0, 500)) },
        ],
        warnings: ["浏览器预览使用当前演示服务状态，本机诊断需要在桌面端执行。"],
      } satisfies ServiceDiagnosticReport as T;
    }
    case "health_check": {
      const installed = Array.from(packages.values()).filter((pkg) => pkg.install);
      const snapshotServices = Array.from(services.values());
      const snapshotSites = Array.from(sites.values());
      const items: HealthReport["items"] = [];
      if (installed.length === 0) items.push({ id: "no-packages", severity: "info", title: "还没有安装任何套件", detail: "当前浏览器演示环境没有已安装套件", action: "到「套件 / 服务」选择套件", route: "/packages" });
      for (const site of snapshotSites) {
        const reasons: string[] = [];
        const web = site.runtime.webServer || "nginx";
        if (!installed.some((pkg) => pkg.id === web)) reasons.push(`未安装 ${web}`);
        if (site.runtime.kind === "php" && !installed.some((pkg) => pkg.id === "php" && pkg.version === site.runtime.phpVersion)) reasons.push(`未安装指定 PHP ${site.runtime.phpVersion || "（未指定）"}`);
        if (!site.rootDir.trim()) reasons.push("未配置根目录");
        if (reasons.length) items.push({ id: `broken-site-${site.id}`, severity: "error", title: `站点「${site.name}」配置有问题`, detail: reasons.join("；"), action: "修正站点配置或安装所需套件", route: "/sites" });
      }
      for (const service of snapshotServices) {
        if (service.state === "error") items.push({ id: `service-error-${service.id}`, severity: "error", title: `${service.label} 处于错误状态`, detail: service.lastError?.message || "没有错误详情，请查看日志", route: "/logs" });
        else if (["unknown", "starting", "stopping"].includes(service.state)) items.push({ id: `service-pending-${service.id}`, severity: "warn", title: `${service.label} 状态尚未确定`, detail: "请等待服务操作结束后重新检查", route: "/packages" });
      }
      items.push({ id: "browser-local-checks", severity: "info", title: "浏览器预览未执行本机检查", detail: "这里按当前演示套件、服务和站点生成结果；真实端口、目录、证书、hosts 与 PHP 扩展请在桌面端检查。" });
      const errors = items.filter((item) => item.severity === "error").length;
      const warnings = items.filter((item) => item.severity === "warn").length;
      const infos = items.filter((item) => item.severity === "info").length;
      const rank = (item: HealthReport["items"][number]) => item.severity === "error" ? 0 : item.severity === "warn" ? 1 : 2;
      return {
        items: items.sort((a, b) => rank(a) - rank(b)), errors, warnings, infos,
        summary: errors > 0 ? `演示环境发现 ${errors} 个问题；本机检查未执行` : "演示状态已检查；本机检查未执行",
        checkedAt: Math.floor(Date.now() / 1000),
        checks: [
          { id: "demo-packages", label: "演示安装记录", state: "checked", detail: `${installed.length} 个已安装套件；未访问本机目录` },
          { id: "demo-sites", label: "演示站点依赖", state: "checked", detail: `${snapshotSites.length} 个站点；比对所选 Web 服务器与 PHP 版本` },
          { id: "demo-services", label: "演示服务状态", state: snapshotServices.some((service) => ["unknown", "starting", "stopping"].includes(service.state)) ? "unavailable" : "checked", detail: `${snapshotServices.length} 个服务；状态仅来自浏览器内存` },
          { id: "local-checks", label: "本机环境", state: "unavailable", detail: "需要桌面端检查真实端口、进程、证书、目录、hosts 和扩展" },
          { id: "application-probes", label: "配置语法与业务连通性", state: "skipped", detail: "未运行原生配置校验、HTTP、数据库或 UDP 连通性检查" },
        ],
      } satisfies HealthReport as T;
    }
    case "diagnostics_build": {
      const snapshotServices = Array.from(services.values());
      const snapshotPackages = Array.from(packages.values()).filter((pkg) => pkg.install);
      const snapshotSites = Array.from(sites.values());
      const generatedAt = Math.floor(Date.now() / 1000);
      const warnings = ["浏览器仅演示当前套件与服务状态；未采集本机端口、证书、配置和日志。"];
      const markdown = [
        "# NiceEnv 诊断报告（浏览器演示）", "",
        `- 应用版本：${MOCK_APP_VERSION}`, `- 生成时间：${new Date(generatedAt * 1000).toLocaleString()}`, "",
        "## 服务状态", "", "| 服务 | 状态 | 端口 | 版本 |", "|------|------|------|------|",
        ...snapshotServices.map((row) => `| ${row.id} | ${row.state} | ${row.port ?? "-"} | ${row.version ?? "-"} |`), "",
        "## 已安装套件", "", ...snapshotPackages.map((pkg) => `- ${pkg.id} ${pkg.version}`), "",
        "## 站点", "", ...snapshotSites.map((site) => `- ${site.name}：${site.domains.join(", ")}`), "",
        "## 采集说明", "", ...warnings.map((warning) => `- ${warning}`),
      ].join("\n");
      return { markdown, serviceCount: snapshotServices.length, siteCount: snapshotSites.length, logLines: 0, redacted: 0, generatedAt, warnings } as DiagnosticsBundle as T;
    }
    case "diagnostics_save": {
      const bundle = DiagnosticsBundleSchema.parse(args?.bundle);
      if (!bundle.markdown.trim() || new TextEncoder().encode(bundle.markdown).length > 2 * 1024 * 1024) throw { code: "DIAGNOSTICS_INVALID", message: "报告为空或超过 2 MiB，请重新生成" };
      return downloadPreviewText(bundle.markdown, `niceenv-diagnostics-${bundle.generatedAt}.md`) as T;
    }
    case "site_files_inspect_import": {
      const id = String(args!.id);
      const target = sites.get(id);
      if (!target) throw { code: "SITE_NOT_FOUND", message: "目标站点已不存在" };
      emitLocal("site-files://progress", { operationId: args!.operationId, siteId: id, phase: "inspect", files: 0, bytes: 24576 });
      await delay(600);
      return { sourcePath: String(args!.source), sourceSiteId: "demo-source-site", targetName: target.name, targetRoot: target.rootDir,
        archive: { name: "sample-site.zip", path: String(args!.source), sizeBytes: 24576, createdAt: Date.now() - 86400000, files: 12, originalBytes: 98304, root: "C:/Demo/source-project", excluded: ["node_modules", ".git"], restorable: true, error: null, automatic: false },
        revision: JSON.stringify([id, target.name, target.rootDir, target.updatedAt, args!.source]) } as T;
    }
    case "site_files_import": {
      const id = String(args!.id);
      const target = sites.get(id);
      if (!target) throw { code: "SITE_NOT_FOUND", message: "目标站点已不存在" };
      if (!args!.confirmed) throw { code: "SITE_BACKUP_INVALID", message: "请确认来源可信及目标站点" };
      if (args!.revision !== JSON.stringify([id, target.name, target.rootDir, target.updatedAt, args!.source])) throw { code: "SITE_IMPORT_CHANGED", message: "源归档或目标站点已变化，请重新读取" };
      for (const phase of ["importRead", "import", "complete"]) { emitLocal("site-files://progress", { operationId: args!.operationId, siteId: id, phase, files: phase === "importRead" ? 0 : 12, bytes: phase === "importRead" ? 24576 : 98304 }); await delay(400); }
      const name = `site-${Date.now()}-${crypto.randomUUID().slice(0, 8)}.zip`;
      const archive: SiteFileBackup = { name, path: `C:/NiceEnv/backup/sites/preview/${name}`, sizeBytes: 24576, createdAt: Date.now() - 86400000, files: 12, originalBytes: 98304, root: "C:/Demo/source-project", excluded: ["node_modules", ".git"], restorable: true, error: null, automatic: false };
      siteFileArchives.set(id, [archive, ...(siteFileArchives.get(id) ?? [])]); return structuredClone(archive) as T;
    }
    case "site_files_plan": return mockSiteFilePlan(String(args!.id)) as T;
    case "site_files_plan_save": {
      const id = String(args!.id); const plan = mockSiteFilePlan(id); const config = args!.config as BackupPlanConfig;
      if (plan.revision !== args!.expectedRevision) throw { code: "SITE_PLAN_CHANGED", message: "计划已变化，请重新读取" };
      if (!["daily", "weekly", "monthly"].includes(config.frequency) || !/^([01]\d|2[0-3]):[0-5]\d$/.test(config.time) || !Number.isInteger(config.keep) || config.keep < 0 || config.keep > 100) throw { code: "BAD_BACKUP_PLAN", message: "请检查计划时间和保留数量" };
      if (config.enabled) {
        if (!args!.confirmed) throw { code: "SITE_BACKUP_INVALID", message: "请确认范围与保留策略" };
        const scope = mockSiteFileScope(id, !!args!.project, !!args!.excludeGenerated);
        if (scope.revision !== args!.scopeRevision) throw { code: "SITE_BACKUP_CHANGED", message: "站点范围已变化，请重新读取" };
        plan.scope = scope; plan.project = !!args!.project; plan.excludeGenerated = !!args!.excludeGenerated;
      }
      plan.status.config = { ...config }; plan.status.nextAt = null;
      plan.status.state = "idle"; plan.status.message = "浏览器预览不会自动执行备份";
      plan.revision = crypto.randomUUID(); siteFilePlans.set(id, plan); return structuredClone(plan) as T;
    }
    case "site_files_plan_run": {
      const id = String(args!.id); const plan = mockSiteFilePlan(id);
      const scope = mockSiteFileScope(id, plan.project, plan.excludeGenerated);
      if (!plan.scope || scope.revision !== plan.scope.revision) {
        plan.status.state = "needs-review"; plan.status.config.enabled = false; plan.status.nextAt = null;
        plan.status.message = "站点范围已变化，请重新配置计划"; plan.revision = crypto.randomUUID();
      } else {
        for (const phase of ["scan", "backup", "complete"]) { emitLocal("site-files://progress", { operationId: args!.operationId, siteId: id, phase, files: phase === "scan" ? 0 : 12, bytes: phase === "scan" ? 0 : 98304 }); await delay(350); }
        const name = `site-auto-${Date.now()}-${crypto.randomUUID().slice(0, 8)}.zip`;
        const archive: SiteFileBackup = { name, path: `C:/NiceEnv/backup/sites/preview/${name}`, sizeBytes: 24576, createdAt: Date.now(), files: 12, originalBytes: 98304, root: scope.root, excluded: scope.excluded, restorable: true, error: null, automatic: true };
        let archives = [archive, ...(siteFileArchives.get(id) ?? [])];
        const retained = new Set(archives.filter((item) => item.automatic).slice(0, plan.status.config.keep).map((item) => item.name));
        if (plan.status.config.keep) archives = archives.filter((item) => !item.automatic || retained.has(item.name));
        siteFileArchives.set(id, archives); plan.status.files = [name]; plan.status.state = "success";
        plan.status.message = "演示完成，未写入真实文件";
      }
      plan.status.lastRunAt = Date.now(); plan.status.finishedAt = Date.now(); siteFilePlans.set(id, plan);
      emitLocal("site-backup://status", { siteId: id, state: plan.status.state, message: plan.status.message }); return structuredClone(plan) as T;
    }
    case "site_files_scope": return mockSiteFileScope(String(args!.id), !!args!.project, !!args!.excludeGenerated) as T;
    case "site_files_list": return structuredClone(siteFileArchives.get(String(args!.id)) ?? []) as T;
    case "site_files_create": {
      const id = String(args!.id);
      const scope = mockSiteFileScope(id, !!args!.project, !!args!.excludeGenerated);
      if (!args!.confirmed) throw { code: "SITE_BACKUP_INVALID", message: "请先确认备份范围与敏感文件提示" };
      if (scope.revision !== args!.revision) throw { code: "SITE_BACKUP_CHANGED", message: "站点目录或备份范围已变化，请重新检查" };
      for (const phase of ["scan", "backup", "complete"]) { emitLocal("site-files://progress", { operationId: args!.operationId, siteId: id, phase, files: phase === "scan" ? 0 : 12, bytes: phase === "scan" ? 0 : 98304 }); await delay(400); }
      const name = `site-${new Date().toISOString().replace(/[-:TZ.]/g, "")}-${crypto.randomUUID().slice(0, 8)}.zip`;
      const info: SiteFileBackup = { name, path: `C:/NiceEnv/backup/sites/preview/${name}`, sizeBytes: 24576, createdAt: Date.now(), files: 12, originalBytes: 98304, root: scope.root, excluded: scope.excluded, restorable: true, error: null, automatic: false };
      siteFileArchives.set(id, [info, ...(siteFileArchives.get(id) ?? [])]); return structuredClone(info) as T;
    }
    case "site_files_inspect_restore": {
      const id = String(args!.id);
      const archive = siteFileArchives.get(id)?.find((item) => item.name === args!.name && item.restorable);
      if (!archive || !sites.has(id)) throw { code: "SITE_BACKUP_INVALID", message: "归档或站点已不存在，请刷新列表" };
      const parent = String(args!.parent || "C:/NiceEnv/restored-sites");
      for (const phase of ["verifyRead", "verify", "complete"]) { emitLocal("site-files://progress", { operationId: args!.operationId, siteId: id, phase, files: phase === "verifyRead" ? 0 : archive.files, bytes: phase === "verifyRead" ? archive.sizeBytes : archive.originalBytes }); await delay(400); }
      const entries = Array.from({ length: Math.min(archive.files, 100) }, (_, index) => ({ path: `demo/file-${index + 1}.txt`, directory: false, size: Math.floor(archive.originalBytes / archive.files) }));
      return { archive: structuredClone(archive), parent, sha256: "browser-preview", verifiedAt: Date.now(), entries, totalEntries: archive.files,
        revision: JSON.stringify([id, archive, parent, sites.get(id)?.updatedAt]) } as T;
    }
    case "site_files_restore": {
      const id = String(args!.id);
      if (!args!.trusted) throw { code: "SITE_BACKUP_INVALID", message: "请确认归档来源可信" };
      const archive = siteFileArchives.get(id)?.find((item) => item.name === args!.name && item.restorable);
      if (!archive || !sites.has(id)) throw { code: "SITE_BACKUP_INVALID", message: "归档或站点已不存在，请刷新列表" };
      const parent = String(args!.parent || "C:/NiceEnv/restored-sites");
      if (args!.revision !== JSON.stringify([id, archive, parent, sites.get(id)?.updatedAt])) throw { code: "SITE_RESTORE_CHANGED", message: "归档、站点或恢复目录已变化，请重新校验" };
      for (let count = 0; count <= 3; count++) { emitLocal("site-files://progress", { operationId: args!.operationId, siteId: id, phase: count === 3 ? "complete" : "restore", files: count * 4, bytes: count * 32768 }); await delay(350); }
      return `${args!.parent || "C:/NiceEnv/restored-sites"}/restored-site-${crypto.randomUUID().slice(0, 8)}` as T;
    }
    case "site_files_delete": {
      const id = String(args!.id);
      const existing = siteFileArchives.get(id) ?? [];
      if (!existing.some((item) => item.name === args!.name)) throw { code: "SITE_BACKUP_INVALID", message: "归档已不存在，请刷新列表" };
      siteFileArchives.set(id, existing.filter((item) => item.name !== args!.name)); return undefined as T;
    }
    case "env_read":
      return structuredClone(mockEnvView(String(args!.siteId), args?.fileName as string | undefined)) as T;
    case "env_save": {
      const view = mockEnvView(String(args!.siteId), args?.fileName as string | undefined);
      if (args!.expectedRevision !== view.revision) throw { code: "ENV_CHANGED", message: "环境文件或站点目录已变化，未覆盖当前文件", hint: "请重新读取文件并检查最新内容。" };
      const changes = args!.changes as [string, string][];
      if (new Set(changes.map(([key]) => key)).size !== changes.length || changes.some(([key, value]) => !/^[A-Za-z_][A-Za-z0-9_.]*$/.test(key) || value.includes("\0"))) {
        throw { code: "BAD_ENV_KEY", message: "请检查变量名、重复项和无效字符" };
      }
      for (const [key, value] of changes) {
        if (isEnvSecretKey(key) && /^(true|false|on|off|null|empty|\(true\)|\(false\)|\(null\)|\(empty\))$/i.test(value)) throw { code: "BAD_ENV_VALUE", message: "框架会把此密码或密钥识别为布尔值或空值" };
      }
      if (!changes.length) return structuredClone(view) as T;
      const nextEntries = structuredClone(view.entries);
      for (const [key, value] of changes) {
        const matches = nextEntries.filter((entry) => !entry.commented && entry.key === key);
        if (matches.length) matches.forEach((entry) => { entry.value = value; entry.needsQuote = false; });
        else nextEntries.push({ key, value, commented: false, secret: isEnvSecretKey(key), line: nextEntries.length + 1, needsQuote: false });
      }
      if (view.exists && JSON.stringify(nextEntries) === JSON.stringify(view.entries)) return structuredClone(view) as T;
      if (view.exists) mockEnvBackups.set(view.path, { entries: structuredClone(view.entries), revision: `mock-backup-${++mockEnvRevision}` });
      view.entries = nextEntries;
      view.exists = true; view.revision = `mock-env-${++mockEnvRevision}`;
      return structuredClone(mockEnvView(view.siteId, view.fileName)) as T;
    }
    case "env_preview_db": {
      const view = mockEnvView(String(args!.siteId), args?.fileName as string | undefined);
      if (args!.expectedRevision !== view.revision) throw { code: "ENV_CHANGED", message: "环境文件或站点目录已变化，请重新读取" };
      return mockEnvDbValues(view) as T;
    }
    case "env_restore_preview":
    case "env_restore": {
      const view = mockEnvView(String(args!.siteId), args?.fileName as string | undefined);
      if (args!.expectedRevision !== view.revision) throw { code: "ENV_CHANGED", message: "环境文件或站点目录已变化，请重新读取" };
      const backup = mockEnvBackups.get(view.path);
      if (!backup) throw { code: "ENV_BACKUP_MISSING", message: "此环境文件没有上次保存的备份" };
      const contentChanged = !view.exists || JSON.stringify(view.entries) !== JSON.stringify(backup.entries);
      if (cmd === "env_restore_preview") {
        const values = (entries: EnvFileView["entries"], key: string) => JSON.stringify(entries.filter((entry) => !entry.commented && entry.key === key).map((entry) => entry.value));
        const keys = [...new Set([...view.entries, ...backup.entries].filter((entry) => !entry.commented).map((entry) => entry.key))].sort();
        return { fileName: view.fileName, backupPath: `${view.path}.nsb-backup`, revision: backup.revision, currentExists: view.exists,
          contentChanged, changedKeys: keys.filter((key) => values(view.entries, key) !== values(backup.entries, key)) } satisfies EnvRestorePreview as T;
      }
      if (args!.expectedBackupRevision !== backup.revision) throw { code: "ENV_BACKUP_CHANGED", message: "备份在预览后已变化，请重新预览" };
      if (contentChanged) {
        if (view.exists) mockEnvBeforeRestore.set(view.path, structuredClone(view.entries));
        view.entries = structuredClone(backup.entries); view.exists = true; view.revision = `mock-env-${++mockEnvRevision}`;
      }
      return structuredClone(mockEnvView(view.siteId, view.fileName)) as T;
    }
    case "env_apply_db": {
      const view = mockEnvView(String(args!.siteId));
      const changes = mockEnvDbValues(view);
      await mockInvoke("env_save", { siteId: args!.siteId, changes, expectedRevision: view.revision });
      return changes.map(([key]) => key) as T;
    }
    case "cert_health": {
      const now = Math.floor(Date.now() / 1000);
      const day = 86400;
      const ca = certs.get("ca");
      return {
        certs: [
          ...(ca ? [{ id: "ca", kind: "ca", subject: ca.subject, sans: [], notAfter: ca.notAfter / 1000, daysLeft: Math.floor((ca.notAfter / 1000 - now) / day), status: ca.notAfter / 1000 <= now ? "expired" : "ok", filePresent: true, usedBySites: [], missingSans: [], advice: ca.notAfter / 1000 <= now ? "根 CA 已过期，请恢复有效根证书" : "" }] : []),
          { id: "laravel-shop", kind: "site", subject: "shop.test", sans: ["shop.test"], notAfter: now + 12 * day, daysLeft: 12, status: "warn", filePresent: true, usedBySites: ["laravel-shop"], missingSans: [], advice: "还有 12 天到期，建议尽快重新签发" },
          { id: "legacy-admin", kind: "site", subject: "admin.test", sans: ["admin.test"], notAfter: now - 2 * day, daysLeft: -2, status: "expired", filePresent: true, usedBySites: ["legacy-admin"], missingSans: ["old.admin.test"], advice: "已过期：到站点详情里重新签发证书即可" },
        ],
        expired: 1,
        critical: 0,
        warning: 1,
        caTrusted: !!ca?.trusted && ca.notAfter / 1000 > now,
        checkedAt: now,
      } as CertReport as T;
    }
    case "cert_import_dir": {
      const now = Math.floor(Date.now() / 1000);
      return {
        imported: [
          { id: "a-example", usable: true, usedBySites: [], certPath: "D:/mock/a.crt", keyPath: "D:/mock/a.key", subject: "a.example.com",
            sans: ["a.example.com"], notBefore: now - 86400 * 30, notAfter: now + 86400 * 60, daysLeft: 60 },
        ],
        skipped: ["b.crt：找不到同名私钥"],
      } as T;
    }
    case "cert_imported_list":
      return [
        { id: "corp-wildcard", usable: true, usedBySites: [], certPath: "C:\NiceEnv\certs\imported\corp-wildcard.crt", keyPath: "C:\NiceEnv\certs\imported\corp-wildcard.key", subject: "*.corp.internal", sans: ["*.corp.internal", "corp.internal"], notBefore: 1700000000, notAfter: 1800000000, daysLeft: 210 },
      ] as ImportedCert[] as T;
    case "site_certificate_choices": {
      const imported = await mockInvoke<ImportedCert[]>("cert_imported_list");
      // 浏览器不能读取真实 PEM 或私钥，不把内存中的 ACME 记录标记为已验证。
      return [
        ...Array.from(certs.values()).filter((cert) => cert.kind === "acme" && cert.sans[0] === cert.subject).map((cert) => ({
          id: cert.id, kind: "acme", subject: cert.subject, sans: cert.sans, notBefore: cert.notBefore / 1000,
          notAfter: cert.notAfter / 1000, daysLeft: Math.floor((cert.notAfter - now()) / 86400000),
          usable: false, problem: "浏览器预览无法校验真实证书，请在桌面端签发并部署", usedBySites: [],
        })),
        ...imported.map(({ certPath: _certPath, keyPath: _keyPath, ...cert }) => ({ ...cert, kind: "imported" })),
      ] as SiteCertificateChoice[] as T;
    }
    case "cert_import":
      return { id: "imported", usable: true, usedBySites: [], certPath: "D:/mock/imported.crt", keyPath: "D:/mock/imported.key", subject: "imported", sans: [], notBefore: 0, notAfter: 0, daysLeft: 365 } as ImportedCert as T;
    case "cert_imported_delete":
      return true as T;
    case "cert_imported_replace":
      throw { code: "DESKTOP_ONLY", message: "浏览器预览无法读取或更新真实证书，请在桌面端操作" };
    case "list_certs":
      return structuredClone(Array.from(certs.values())) as T;
    case "issue_cert": {
      const domain = (args!.domain as string).trim().toLowerCase().replace(/\.+$/, "");
      const domains = [...new Set([domain, ...((args!.sans as string[]) ?? [])].map(d => d.trim().toLowerCase().replace(/\.+$/, "")))];
      for (const value of domains) {
        const hostname = value.replace(/^\*\./, "");
        let ip = false;
        try { ip = value.includes(":") && new URL(`http://[${value}]`).hostname.startsWith("["); } catch { /* 继续校验域名 */ }
        if (!ip && (hostname.length > 253 || (hostname !== "localhost" && !hostname.includes("."))
          || (value.startsWith("*.") && (hostname === "localhost" || /^[\d.]+$/.test(hostname)))
          || (/^[\d.]+$/.test(hostname) && (hostname.split(".").length !== 4 || hostname.split(".").some(label => !/^(0|[1-9]\d{0,2})$/.test(label) || Number(label) > 255)))
          || hostname.split(".").some(label => !/^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/i.test(label)))) {
          throw { code: "BAD_DOMAINS", message: `域名或 IP 地址格式不正确：${value}`, hint: "不要包含协议、端口或路径。" };
        }
      }
      for (const site of sites.values()) {
        if (site.https && site.domains[0] === domain) {
          for (const name of site.domains) if (!domains.includes(name)) domains.push(name);
        }
      }
      const id = [...certs.values()].find(c => c.kind === "site" && c.subject === domain)?.id ?? `cert-${domain}`;
      certs.set(id, {
        id,
        kind: "site",
        subject: domain,
        sans: domains,
        notBefore: now(),
        notAfter: now() + 86400_000 * 30,
        certPath: `…/certs/sites/${domain.replace(/\*/g, "_wildcard").replace(/:/g, "_")}.crt`,
        keyPath: `…/certs/sites/${domain.replace(/\*/g, "_wildcard").replace(/:/g, "_")}.key`,
      });
      return certs.get(id) as T;
    }
    case "delete_local_cert": {
      const id = args!.id as string;
      const cert = certs.get(id);
      if (!cert) throw { code: "NOT_FOUND", message: "证书不存在" };
      if (cert.kind !== "site") throw { code: "CERT_DELETE_UNSUPPORTED", message: "这里只能删除本地签发的站点证书" };
      const usedBy = [...sites.values()].filter(site => site.https && site.domains[0] === cert.subject);
      if (usedBy.length) throw { code: "CERT_IN_USE", message: `证书仍被站点 ${usedBy.map(s => s.name).join("、")} 使用`, hint: "先关闭相关站点的 HTTPS 或更换主域名，再重试。" };
      certs.delete(id);
      return true as T;
    }
    case "trust_ca": {
      const ca = certs.get("ca");
      if (ca) ca.trusted = true;
      return true as T;
    }
    case "db_workspace": throw { code: "DESKTOP_REQUIRED", message: "数据库工作台需要在桌面端连接实际实例；浏览器预览不执行 SQL。" };
    case "full_log": return logLinesFor(args!.id as string).join("\n") as T;
    case "tail_logs": {
      const id = args!.id as string;
      const count = Math.max(1, Math.min(Number(args?.lines) || 200, 20000));
      return logLinesFor(id).slice(-count).map((line) => ({ line })) as LogLine[] as T;
    }
    case "diagnose_port": {
      const port = Number(args?.port);
      const result = await mockInvoke<PortRangeScan>("scan_port_range", { from: port, to: port });
      const row = result.listeners[0];
      return { port, inUse: !!row, pid: row?.pid, processName: row?.processName, cmdline: row?.cmdline } as PortDiagnosis as T;
    }
    case "scan_ports": {
      const rows: PortScanEntry[] = [];
      for (const service of services.values()) {
        if (service.port == null) continue;
        const running = service.state === "running";
        rows.push({ serviceId: service.id, label: service.label, port: service.port, ownedBySelf: running,
          pid: running ? service.pids[0] : undefined, processName: running ? service.label : undefined,
          running, verdict: running ? "self" : "free", detail: "浏览器演示服务状态，未读取本机 TCP 监听表", listenerCount: running ? 1 : 0 });
      }
      return rows as T;
    }
    case "list_backups": {
      const files = await mockInvoke<ConfigFileInfo[]>("config_list");
      return configPreviewHistory.map((backup) => {
        const file = files.find((file) => file.kind === backup.target);
        return { name: `config/${backup.name}`, path: backup.path, sizeBytes: backup.sizeBytes, modifiedAt: backup.createdAt * 1000, targetPath: file?.path.replaceAll("\\", "/").split("NiceEnv/")[1] ?? null, restorable: !!file, reason: file ? null : "找不到对应配置" };
      }) as T;
    }
    case "preview_backup": {
      const backup = configPreviewHistory.find((backup) => `config/${backup.name}` === args!.name);
      const file = (await mockInvoke<ConfigFileInfo[]>("config_list")).find((file) => file.kind === backup?.target);
      if (!backup || !file) throw { code: "NOT_FOUND", message: "找不到对应备份或配置" };
      const current = currentConfigContent(file.kind);
      return { name: args!.name, targetPath: file.path, targetRelative: file.path.replaceAll("\\", "/").split("NiceEnv/")[1], currentExists: true, revision: JSON.stringify([args!.name, file.path, backup.content, current]) } as BackupPreview as T;
    }
    case "restore_backup": {
      const preview = await mockInvoke<BackupPreview>("preview_backup", args);
      if (preview.revision !== args!.revision) throw { code: "CONFIG_CONFLICT", message: "配置或备份已变化，请重新预览后恢复" };
      const backup = configPreviewHistory.find((backup) => `config/${backup.name}` === args!.name)!;
      savePreviewConfig(backup.target!, backup.content);
      return preview.targetPath as T;
    }
    case "config_reset_preview": {
      const file = (await mockInvoke<ConfigFileInfo[]>("config_list")).find((file) => file.kind === args!.kind && file.resettable);
      if (!file) throw { code: "NOT_INSTALLED", message: "尚未安装支持重置配置的服务" };
      const current = currentConfigContent(file.kind);
      const content = defaultConfigContent(file.kind);
      return { kind: file.kind, label: file.label, path: file.path, language: file.language, content, currentExists: file.exists, changed: !file.exists || current !== content, usedByService: file.usedByService ?? null, revision: JSON.stringify([file.kind, file.path, file.exists, current, content]) } as ConfigResetPreview as T;
    }
    case "config_reset": {
      const preview = await mockInvoke<ConfigResetPreview>("config_reset_preview", args);
      if (preview.revision !== args!.revision) throw { code: "CONFIG_CONFLICT", message: "配置或服务设置已变化，请重新预览后重置" };
      if (preview.currentExists) savePreviewConfig(preview.kind, preview.content);
      configPreviewContent.set(preview.kind, preview.content);
      return { ...preview, currentExists: true, changed: false, revision: JSON.stringify([preview.kind, preview.path, true, preview.content, preview.content]) } as T;
    }
    case "rebuild_hosts":
      return true as T;
    case "reissue_site_certs":
      return [] as T;
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
    case "config_list": {
      const definitions = [
        ["nginx", "nginx-main", "Nginx 主配置", "nginx.conf", "nginx"],
        ["apache", "apache-conf", "Apache 主配置", "httpd.conf", "apache"],
        ["php", "php-ini", "php.ini", "php.ini", "ini"],
        ["mysql", "mysql-ini", "my.ini", "my.ini", "ini"],
        ["mariadb", "mariadb-ini", "MariaDB my.ini", "my.ini", "ini"],
        ["redis", "redis-conf", "redis.conf", "redis.conf", "conf"],
        ["postgresql", "postgres-conf", "postgresql.conf", "postgresql.conf", "ini"],
        ["mongodb", "mongo-conf", "mongod.conf", "mongod.conf", "yaml"],
      ];
      const installed = Array.from(packages.values()).filter((p) => p.install);
      const files: ConfigFileInfo[] = [];
      for (const [id, base, label, filename, language] of definitions) {
        let versions = installed.filter((p) => p.id === id).sort((a, b) => cmpVersionDesc(a.version, b.version));
        const shared = id === "nginx" || id === "apache";
        if (shared && versions.length) versions = [versions.find((p) => p.active) ?? versions[0]];
        for (const pkg of versions) {
          const kind = shared ? base : `${base}@${pkg.version}`;
          const exists = id !== "redis" || configPreviewContent.has(kind);
          files.push({ kind, label: shared ? label : `${label} · ${pkg.version}`,
            description: "浏览器演示配置；桌面端读取实际安装版本的配置文件",
            path: `C:/NiceEnv/etc/${id}/${shared ? "" : `${pkg.version}/`}${filename}`,
            exists, sizeBytes: exists ? new TextEncoder().encode(currentConfigContent(kind)).length : 0,
            language, validated: shared, usedByService: shared ? id : `${id}@${pkg.version}`,
            requiresPackage: id, resettable: true,
          });
        }
      }
      return files as T;
    }
    case "config_read": {
      const key = args!.kind as string;
      return currentConfigContent(key) as T;
    }
    case "config_validate": {
      const content = args!.content as string;
      const kind = (args!.kind as string).split("@")[0];
      // 只做一个够用的示意：括号配平 + 结尾分号
      const issues: { line: number; severity: string; message: string }[] = [];
      const lines = content.split("\n");
      let depth = 0;
      lines.forEach((raw, i) => {
        const t = raw.split("#")[0].trim();
        if (!t) return;
        if (kind !== "nginx-main") {
          if (t.startsWith(";") || t.startsWith("[")) return;
          if (["php-ini", "mysql-ini"].includes(kind) && !t.includes("=")) {
            issues.push({ line: i + 1, severity: "error", message: "配置项需要使用 key=value 格式" });
          }
          return;
        }
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
    case "config_save": {
      const kind = args!.kind as string;
      const validation = await mockInvoke<ConfigValidation>("config_validate", args);
      if (!validation.ok && !args!.force) throw { code: "CONFIG_INVALID", message: "配置校验未通过，未写入" };
      savePreviewConfig(kind, args!.content as string, args!.expectedContent as string | undefined);
      return validation as T;
    }
    case "config_backups":
      return configPreviewHistory.filter((b) => !args?.kind || b.target === args.kind).map(({ content: _content, ...b }) => b) as T;
    case "config_rollback": {
      const backup = configPreviewHistory.find((b) => b.name === args!.name);
      if (!backup || (args!.kind && args!.kind !== backup.target)) throw { code: "BACKUP_TARGET_MISMATCH", message: "该历史版本不属于当前配置" };
      await mockInvoke("config_save", { kind: backup.target, content: backup.content, force: true, expectedContent: args!.expectedContent });
      return true as T;
    }
    case "project_platform_check":
      throw { code: "DESKTOP_REQUIRED", message: "请在桌面应用中检查真实项目环境；浏览器示例不会运行 PHP 或 Composer。" };
    case "project_php_compatibility":
      return { status: "unavailable", requirement: null, versions: [], matchingVersions: [], message: "浏览器示例不会读取本机项目或运行 Composer，请在桌面应用中检查。" } as T;
    case "scan_projects": {
      const root = args!.root as string;
      return [
        {
          path: `${root}\my-shop`,
          name: "my-shop",
          kind: "laravel",
          documentRoot: `${root}\my-shop\public`,
          documentRootReady: true,
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
          documentRootReady: true,
          siteKind: "node",
          rewrite: "spa-fallback",
          phpMinVersion: null,
          evidence: ["存在 next.config.js（Next.js）"],
          runHint: "需要 Node；开发用 pnpm dev（本应用可按 Node 站点代理），静态导出用 pnpm build + out 目录",
          needsDevServer: true,
          suggestedDomain: "admin-ui.test",
          alreadyConfigured: false,
        },
        {
          path: `${root}\landing`,
          name: "landing",
          kind: "static-html",
          documentRoot: `${root}\landing`,
          documentRootReady: true,
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
        watched: Array.from(services.values()).filter((service) => service.state === "running").map((service) => ({
          id: service.id,
          enabled: true,
          running: true,
          attempts: 0,
          exhausted: false,
          restartCount: 0,
        })),
      } as WatchdogStatus as T;
    case "process_recovery_status":
      return { adopted: [], killed: [], unresolved: [], blockedServices: [] } as T;
    case "recover_processes":
      throw { code: "RECOVERY_PREVIEW", message: "请在桌面端重新检查本机服务进程" };
    case "watchdog_set_enabled": {
      settings.watchdogEnabled = args!.enabled as boolean;
      return true as T;
    }
    case "watchdog_reset":
      throw { code: "WATCHDOG_PREVIEW", message: "浏览器演示不运行本机服务，请在桌面端重试自动恢复" };
    case "db_backup_list":
      return structuredClone(Array.from(mockDbBackups.values()).sort((a, b) => b.createdAt - a.createdAt)) as T;
    case "db_backup_dir": return "C:/NiceEnv/backup/db" as T;
    case "db_backup_dump": {
      const { service, state } = mysqlPreview(args?.version as string | undefined, true, args?.engine as DatabaseEngine | undefined);
      const names = args!.databases as string[];
      if (!names.length || names.some((name) => systemDatabase(name) || !state.databases.has(name))) throw { code: "BAD_DATABASE", message: "请选择有效的业务数据库" };
      emitLocal("db://backup", { database: names.join(", "), bytes: 0, state: "running" });
      await delay(600);
      return previewBackup(service.version!, names.map((name) => state.databases.get(name)!), names.length === 1 ? names[0] : `${names.length}dbs`, args?.engine as DatabaseEngine | undefined).path as T;
    }
    case "db_backup_restore": {
      const { state, service } = mysqlPreview(args?.version as string | undefined, true, args?.engine as DatabaseEngine | undefined);
      const database = args?.database as string | undefined;
      if (database !== undefined && (systemDatabase(database) || !state.databases.has(database))) throw { code: "RESTORE_DATABASE_INVALID", message: "请选择当前实例中已存在的业务数据库" };
      const content = mockBackupContents.get(args!.path as string);
      if (!content) throw { code: "FILE_NOT_FOUND", message: "找不到有效的 SQL 备份" };
      const before = [...state.databases.values()].filter((db) => !systemDatabase(db.name));
      const safety = args?.safetyBackup && before.length ? previewBackup(service.version!, before, "pre-restore", args?.engine as DatabaseEngine | undefined) : undefined;
      emitLocal("db://backup", { database: args!.path, bytes: 0, state: "running", message: "正在执行 SQL" });
      await delay(700);
      for (const db of content) state.databases.set(db.name, structuredClone(db));
      return { ok: true, safetyBackup: safety?.path } as DbRestoreResult as T;
    }
    case "db_backup_delete": {
      const path = args!.path as string;
      if (!mockDbBackups.delete(path)) throw { code: "FILE_NOT_FOUND", message: "备份文件已不存在" };
      mockBackupContents.delete(path); return true as T;
    }
    case "migrate_list_source": {
      mysqlPreview(args?.version as string | undefined, true, args?.engine as DatabaseEngine | undefined);
      if (!args?.host || !args.user || !Number.isInteger(args.port) || Number(args.port) < 1 || Number(args.port) > 65535) throw { code: "BAD_CONNECTION", message: "请检查来源地址、端口和账号" };
      return structuredClone(previewSource) as T;
    }
    case "migrate_import": {
      const { state, service } = mysqlPreview(args?.version as string | undefined, true, args?.engine as DatabaseEngine | undefined);
      if (["localhost", "127.0.0.1"].includes(args!.host as string) && args!.port === service.port) throw { code: "SAME_MYSQL_INSTANCE", message: "源和目标是同一个实例" };
      const names = args!.databases as string[];
      if (!names.length || names.some((name) => !previewSource.some((db) => db.name === name))) throw { code: "BAD_DATABASE", message: "请重新检测源数据库" };
      const before = [...state.databases.values()].filter((db) => !systemDatabase(db.name));
      if (before.length) previewBackup(service.version!, before, "pre-import", args?.engine as DatabaseEngine | undefined);
      await delay(800);
      for (const db of previewSource.filter((db) => names.includes(db.name))) state.databases.set(db.name, structuredClone(db));
      return { imported: names, failed: [] } as T;
    }
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
      if (!target) throw { code: "PHP_EXTENSION_FILE_MISSING", message: `缺少扩展文件：${name}` };
      if (target.builtin && !enabled) throw { code: "PHP_EXTENSION_BUILTIN", message: `${name} 是内置模块，不能单独禁用` };
      if (!enabled) {
        const dependents = Object.entries(mockPhpExtDependencies)
          .filter(([dependent, dependencies]) => target.enabled && dependencies.includes(name) && exts.some((e) => e.name === dependent && e.enabled))
          .map(([dependent]) => dependent);
        if (dependents.length > 0) {
          throw { code: "PHP_EXTENSION_IN_USE", message: `不能禁用 ${name}：仍被 ${dependents.join("、")} 使用` };
        }
      }
      const dependencies = (mockPhpExtDependencies[name] ?? []).map((dependency) => {
        const found = exts.find((e) => e.name === dependency);
        if (!found && enabled) throw { code: "PHP_EXTENSION_FILE_MISSING", message: `缺少扩展文件：${dependency}` };
        return found;
      });
      if (enabled) dependencies.forEach((e) => { if (e) e.enabled = true; });
      target.enabled = enabled;
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
    case "db_list": return structuredClone([...mysqlPreview(args?.version as string | undefined, true, args?.engine as DatabaseEngine | undefined).state.databases.values()]) as T;
    case "db_create": {
      const { state } = mysqlPreview(args?.version as string | undefined, true, args?.engine as DatabaseEngine | undefined);
      const name = args!.name as string;
      if (!/^[A-Za-z0-9_]{1,64}$/.test(name)) throw { code: "BAD_IDENTIFIER", message: "数据库名只能包含字母、数字和下划线" };
      if (!state.databases.has(name)) state.databases.set(name, { name, tables: 0, sizeKb: 0 });
      return true as T;
    }
    case "db_drop": {
      const { state } = mysqlPreview(args?.version as string | undefined, true, args?.engine as DatabaseEngine | undefined);
      const name = args!.name as string;
      if (systemDatabase(name)) throw { code: "SYSTEM_DATABASE", message: "不能删除系统数据库" };
      state.databases.delete(name); return true as T;
    }
    case "db_users": return structuredClone([...mysqlPreview(args?.version as string | undefined, true, args?.engine as DatabaseEngine | undefined).state.users.values()]) as T;
    case "db_user_drop_info":
    case "db_user_drop": {
      const engine = args!.engine as DatabaseEngine; const version = args!.version as string;
      const input = args!.input as import("./api").DatabaseUserDropInput | undefined;
      const username = (input?.username ?? args!.username) as string; const host = (input?.host ?? args!.host) as string;
      const { state } = mysqlPreview(version, true, engine);
      const entry = [...state.users.entries()].find(([, user]) => user.username === username && user.host === host);
      if (!entry) throw { code: "DB_USER_MISSING", message: "所选账号已不存在，请刷新列表" };
      const key = mockGrantKey(engine, version, username, host);
      if (!mockUserPasswordRevisions.has(key)) mockUserPasswordRevisions.set(key, uid());
      const data = mysqlGrantsPreview(engine, version, username, host);
      const revision = `${mockUserPasswordRevisions.get(key)}:${data.revision}`;
      const info: import("./api").DatabaseUserDropInfo = { username, host, protected: data.protected, dependencies: [], moreDependencies: false, roleDependents: 0, proxyDependents: 0, usernameConnections: 0, revision };
      if (cmd === "db_user_drop" && input) {
        if (input.confirmation !== `${username}@${host}`) throw { code: "DB_USER_CONFIRM", message: "请完整输入账号及来源主机" };
        if (info.protected) throw { code: "SYSTEM_ACCOUNT", message: "此系统账号受保护" };
        if (input.revision !== revision) throw { code: "DB_USER_CHANGED", message: "账号已变化，请重新检查" };
        await delay(700);
        if (input.revision !== `${mockUserPasswordRevisions.get(key)}:${data.revision}` || state.users.get(entry[0]) !== entry[1]) throw { code: "DB_USER_CHANGED", message: "账号已变化，请重新检查" };
        state.users.delete(entry[0]); mockDatabaseGrants.delete(key); mockUserPasswordRevisions.delete(key);
        return undefined as T;
      }
      return info as T;
    }
    case "db_user_password_info":
    case "db_user_password_save": {
      const engine = args!.engine as DatabaseEngine; const version = args!.version as string;
      const input = args!.input as import("./api").DatabaseUserPasswordInput | undefined;
      const username = (input?.username ?? args!.username) as string; const host = (input?.host ?? args!.host) as string;
      const { state } = mysqlPreview(version, true, engine);
      const account = [...state.users.values()].find((user) => user.username === username && user.host === host);
      if (!account) throw { code: "DB_USER_MISSING", message: "所选账号已不存在，请刷新列表" };
      const key = JSON.stringify([engine, version, username, host]);
      if (!mockUserPasswordRevisions.has(key)) mockUserPasswordRevisions.set(key, uid());
      const protectedAccount = !username || !host || username.toLowerCase() === "root" || username.toLowerCase().startsWith("mysql.") || username.toLowerCase() === "mariadb.sys";
      if (cmd === "db_user_password_save" && input) {
        if (protectedAccount) throw { code: "SYSTEM_ACCOUNT", message: "系统账号受保护；root 密码请使用专用入口" };
        if (!input.password || new TextEncoder().encode(input.password).length > 4096 || /[\x00-\x1f\x7f-\x9f]/.test(input.password)) throw { code: "BAD_PASSWORD", message: "密码不能为空、超过 4096 字节或包含控制字符" };
        if (input.revision !== mockUserPasswordRevisions.get(key)) throw { code: "DB_PASSWORD_CHANGED", message: "账号认证信息已变化，请重新读取" };
        await delay(700);
        if (input.revision !== mockUserPasswordRevisions.get(key) || ![...state.users.values()].includes(account)) throw { code: "DB_PASSWORD_CHANGED", message: "账号已变化，请重新读取" };
        mockUserPasswordRevisions.set(key, uid());
      }
      const plugin = engine === "mariadb" ? "mysql_native_password" : "caching_sha2_password";
      const info: import("./api").DatabaseUserPasswordInfo = { username, host, plugins: [plugin], targetPlugin: plugin, protected: protectedAccount, supported: !protectedAccount, otherAuthentication: false, revision: mockUserPasswordRevisions.get(key)! };
      return info as T;
    }
    case "db_grants": return structuredClone(mysqlGrantsPreview(args!.engine as DatabaseEngine, args!.version as string, args!.username as string, args!.host as string)) as T;
    case "db_grants_save": {
      const input = args!.input as import("./api").DatabaseGrantInput;
      const engine = args!.engine as DatabaseEngine; const version = args!.version as string;
      const data = mysqlGrantsPreview(engine, version, input.username, input.host);
      if (data.protected) throw { code: "DATABASE_GRANTS_PROTECTED", message: "系统账号不允许在此修改" };
      if (input.revision !== data.revision) throw { code: "DATABASE_GRANTS_CHANGED", message: "授权已变化，请重新读取" };
      if (input.privileges.some((name) => !data.available.includes(name))) throw { code: "BAD_PRIVILEGE", message: "请选择有效权限" };
      const target = input.newDatabase ? mockLiteralScope(input.target) : input.target;
      let scope = data.scopes.find((item) => item.scope === target);
      if (scope?.protected || (input.newDatabase && !data.databases.includes(input.target)) || (!input.newDatabase && !scope)) throw { code: "BAD_DATABASE", message: "请选择有效的业务数据库授权范围" };
      await delay(650);
      if (!scope) { scope = { scope: target, label: input.target, pattern: false, protected: false, privileges: [], grantOption: false, extraPrivileges: [] }; data.scopes.push(scope); }
      scope.privileges = [...new Set(input.privileges)]; scope.grantOption = input.grantOption;
      data.scopes = data.scopes.filter((item) => engine === "mariadb" || item.privileges.length || item.grantOption || item.extraPrivileges.length);
      data.revision = uid();
      const { state } = mysqlPreview(version, true, engine);
      const account = [...state.users.values()].find((item) => item.username === input.username && item.host === input.host)!;
      account.grants = data.scopes.map((item) => `${item.privileges.join(", ")}${item.grantOption ? " + GRANT OPTION" : ""} ON ${item.label}.*`).join("; ");
      return structuredClone(data) as T;
    }
    case "db_create_user": {
      const { state } = mysqlPreview(args?.version as string | undefined, true, args?.engine as DatabaseEngine | undefined);
      const username = args!.username as string; const database = args!.database as string;
      if (!/^[A-Za-z0-9_]{1,32}$/.test(username) || username.toLowerCase() === "root" || !args?.password || !state.databases.has(database) || systemDatabase(database)) throw { code: "BAD_IDENTIFIER", message: "请检查账号、密码和授权数据库" };
      if ([...state.users.values()].some((user) => user.username === username && ["localhost", "127.0.0.1"].includes(user.host))) throw { code: "DB_USER_EXISTS", message: "同名本地账号已存在，未修改密码或权限" };
      for (const host of ["localhost", "127.0.0.1"]) {
        state.users.set(`${username}@${host}`, { username, host, grants: `ALL ON ${database}.*` });
        const data = mysqlGrantsPreview(args?.engine as DatabaseEngine ?? "mysql", args!.version as string, username, host);
        data.scopes[0].scope = mockLiteralScope(database); data.scopes[0].pattern = false;
      }
      return true as T;
    }
    case "db_root_password": return mysqlPreview(args?.version as string | undefined, true, args?.engine as DatabaseEngine | undefined).state.savedPassword as T;
    case "db_reset_root_password": {
      const { state, service } = mysqlPreview(args?.version as string | undefined, !args?.useExisting, args?.engine as DatabaseEngine | undefined);
      const password = args!.newPassword as string;
      if (!password || /[\x00-\x1f\x7f]/.test(password)) throw { code: "BAD_PASSWORD", message: "密码不能为空或包含控制字符" };
      if (args?.useExisting && password !== state.password) throw { code: "MYSQL_AUTH_REQUIRED", message: "密码验证失败，本机记录未修改" };
      if (!args?.useExisting) state.password = password;
      state.savedPassword = password; service.state = "running"; return true as T;
    }
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
      if (args!.enabled && !proxyRunning) {
        throw { code: "PROXY_NOT_RUNNING", message: "mihomo 尚未运行，不能开启系统代理" };
      }
      systemProxyOn = args!.enabled as boolean;
      return true as T;
    }
    case "proxy_set_mode": {
      if (!["rule", "global", "direct"].includes(String(args?.mode))) throw { code: "BAD_PROXY_MODE", message: "代理模式无效" };
      proxyMode = args!.mode as "rule" | "global" | "direct";
      return true as T;
    }
    case "proxy_profiles":
      return Array.from(proxyProfiles.values()) as T;
    case "proxy_activate_profile": {
      const id = args?.id as string | undefined;
      const profile = id ? proxyProfiles.get(id) : undefined;
      if (!profile) throw { code: "PROFILE_NOT_FOUND", message: "找不到代理订阅" };
      for (const item of proxyProfiles.values()) item.active = item.id === id;
      return true as T;
    }
    case "proxy_delete_profile": {
      const id = args?.id as string | undefined;
      const profile = id ? proxyProfiles.get(id) : undefined;
      if (!profile) throw { code: "PROFILE_NOT_FOUND", message: "找不到代理订阅" };
      if (profile.active) throw { code: "PROFILE_ACTIVE", message: "当前订阅正在使用，请先切换到其它订阅" };
      proxyProfiles.delete(id!);
      return true as T;
    }
    case "proxy_import": {
      const name = String(args?.name ?? "").trim();
      const url = String(args?.url ?? "").trim();
      if (!name || [...name].length > 128 || /[\x00-\x1f\x7f]/.test(name)) throw { code: "BAD_PROFILE_NAME", message: "订阅名称无效" };
      try {
        const parsed = new URL(url);
        if (!["http:", "https:"].includes(parsed.protocol) || parsed.username || parsed.password || url.length > 8192) throw new Error();
      } catch { throw { code: "BAD_SUBSCRIPTION_URL", message: "请输入完整的 HTTP 或 HTTPS 订阅地址" }; }
      const p: ProxyProfile = {
        id: uid(),
        name,
        url,
        active: false,
        addedAt: now(),
      };
      proxyProfiles.set(p.id, p);
      return p as T;
    }
    case "proxy_nodes":
      return structuredClone(proxyGroups) as T;
    case "proxy_select_node": {
      if (!proxyRunning) throw { code: "PROXY_NOT_RUNNING", message: "mihomo 尚未运行" };
      const group = proxyGroups.find((item) => item.name === args?.group);
      if (!group || group.type !== "Selector" || !group.nodes.some((node) => node.name === args?.node)) {
        throw { code: "SELECT_FAILED", message: "只能选择手动策略组中的有效节点" };
      }
      group.now = args!.node as string;
      return true as T;
    }
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
      if (!old) throw { code: "PROFILE_NOT_FOUND", message: "找不到代理订阅" };
      if (old.url.startsWith("builtin:")) throw { code: "BAD_SUBSCRIPTION_URL", message: "内置配置无需下载更新" };
      proxyProfiles.set(pid, { ...old });
      return true as T;
    }
    case "cron_jobs":
      return structuredClone([...cronJobs.values()]) as T;
    case "cron_save": {
      const job = args!.job as import("./api").CronJob;
      const name = job.name.trim(); const command = job.command.trim();
      if (!name || [...name].length > 128 || /[\x00-\x1f\x7f]/.test(name) || !command || new TextEncoder().encode(command).length > 8192 || command.includes("\0") || !Number.isInteger(job.intervalMin) || job.intervalMin < 1 || job.intervalMin > 525600) {
        throw { code: "CRON_BAD_JOB", message: "请检查任务名称、命令和执行周期" };
      }
      const old = job.id ? cronJobs.get(job.id) : undefined;
      if (job.id && !old) throw { code: "CRON_NOT_FOUND", message: "待编辑任务不存在" };
      if (old?.lastExit === "running") throw { code: "CRON_BUSY", message: "任务正在运行，请停止后再编辑" };
      const id = job.id || `cron-${uid()}`;
      cronJobs.set(id, old ? { ...old, name, command, intervalMin: job.intervalMin }
        : { ...job, id, name, command, createdAt: Date.now(), lastRunAt: null, lastExit: null, lastOutput: null });
      return true as T;
    }
    case "cron_delete": {
      const job = cronJobs.get(args!.id as string);
      if (!job || job.lastExit === "running") throw { code: "CRON_NOT_REMOVABLE", message: "任务不存在或仍在运行" };
      cronJobs.delete(job.id);
      return true as T;
    }
    case "cron_set_enabled": {
      const job = cronJobs.get(args!.id as string);
      if (!job) throw { code: "CRON_NOT_FOUND", message: "计划任务不存在" };
      cronJobs.set(job.id, { ...job, enabled: args!.enabled as boolean });
      return true as T;
    }
    case "cron_run_now": {
      const job = cronJobs.get(args!.id as string);
      if (!job) throw { code: "CRON_NOT_FOUND", message: "计划任务不存在" };
      if (job.lastExit === "running") throw { code: "CRON_BUSY", message: "计划任务正在运行" };
      cronJobs.set(job.id, { ...job, lastRunAt: Date.now(), lastExit: "running", lastOutput: null });
      await delay(1800);
      const current = cronJobs.get(job.id)!;
      if (current.lastExit === "running") {
        const exit = /^exit(?:\s+\/b)?\s+(-?\d+)$/i.exec(job.command.trim())?.[1] ?? "0";
        cronJobs.set(job.id, { ...current, lastExit: `exit ${exit}`, lastOutput: "（浏览器演示）仅模拟任务结果，没有执行系统命令。" });
      }
      return { ...cronJobs.get(job.id)! } as T;
    }
    case "cron_stop": {
      const job = cronJobs.get(args!.id as string);
      if (!job || job.lastExit !== "running") throw { code: "CRON_NOT_RUNNING", message: "计划任务已结束或未运行" };
      cronJobs.set(job.id, { ...job, lastExit: "cancelled", lastOutput: "（浏览器演示）模拟任务已停止，没有执行系统命令。" });
      return true as T;
    }
    case "tunnel_start":
    case "tunnel_start_site": {
      const site = cmd === "tunnel_start_site" ? sites.get(args!.id as string) : undefined;
      if (cmd === "tunnel_start_site" && (!site || mockSiteStatus(site) !== "running")) throw { code: "TUNNEL_SITE_STOPPED", message: "请先启动所选站点及依赖服务" };
      if (site && !site.accessUrl) throw { code: "SITE_URL_UNAVAILABLE", message: "尚未确认本次加载的站点地址", hint: "请重启对应 Web 服务后重试" };
      const address = site?.accessUrl ? new URL(site.accessUrl) : undefined;
      const port = address ? Number(address.port || (address.protocol === "https:" ? 443 : 80)) : Number(args!.port);
      if (!port || !Number.isInteger(port) || port < 1 || port > 65535) throw { code: "TUNNEL_BAD_PORT", message: "本地 HTTP 端口必须为 1–65535" };
      const target = site?.accessUrl ?? `http://127.0.0.1${port === 80 ? "" : `:${port}`}`;
      const existing = [...mockTunnels.values()].find((row) => row.alive && row.target === target && row.siteId === site?.id);
      if (existing) return structuredClone(existing) as T;
      if (mockTunnels.size >= 20) {
        const ended = [...mockTunnels.values()].find((row) => !row.alive);
        if (!ended) throw { code: "TUNNEL_LIMIT", message: "最多同时运行 20 条隧道" };
        mockTunnels.delete(ended.id);
      }
      const info: TunnelInfo = { id: uid(), port, target, siteId: site?.id, url: null, startedAt: Date.now(), alive: true,
        state: "starting", localReachable: true, logs: ["浏览器演示：没有启动 cloudflared 或创建真实公网隧道。"] };
      mockTunnels.set(info.id, info);
      return structuredClone(info) as T;
    }
    case "tunnel_list": {
      for (const row of mockTunnels.values()) {
        if (row.alive && row.siteId) {
          const site = sites.get(row.siteId);
          if (!site || mockSiteStatus(site) !== "running" || site.accessUrl !== row.target) {
            row.alive = false; row.state = "failed"; row.localReachable = false;
            row.error = "站点已停止或访问地址已变化，请确认站点后重新创建隧道";
          }
        }
        if (row.state === "starting" && row.alive && Date.now() - row.startedAt >= 1500) {
          row.state = "connected"; row.url = `https://preview-${row.id}.example.invalid`;
          row.logs.push("模拟连接完成，示例地址不可访问。");
        }
      }
      return structuredClone([...mockTunnels.values()]) as T;
    }
    case "tunnel_stop": {
      const row = mockTunnels.get(args!.id as string);
      if (!row) throw { code: "TUNNEL_NOT_FOUND", message: "隧道记录不存在，请刷新列表" };
      row.alive = false; row.state = "stopped"; row.error = null;
      return true as T;
    }
    case "tunnel_remove": {
      const row = mockTunnels.get(args!.id as string);
      if (!row) throw { code: "TUNNEL_NOT_FOUND", message: "隧道记录不存在，请刷新列表" };
      if (row.alive) throw { code: "TUNNEL_RUNNING", message: "请先停止隧道，再移除记录" };
      mockTunnels.delete(row.id);
      return true as T;
    }
    case "ollama_models":
      updateMockOllamaPull();
      return structuredClone([...mockOllamaModels.values()]) as T;
    case "ollama_delete": {
      updateMockOllamaPull();
      if (mockOllamaPull?.state === "pulling") throw { code: "OLLAMA_BUSY", message: "请先等待当前拉取完成或取消" };
      if (!mockOllamaModels.delete(args!.name as string)) throw { code: "OLLAMA_MODEL_NOT_FOUND", message: "模型已不存在，请刷新" };
      return true as T;
    }
    case "ollama_pull": {
      updateMockOllamaPull();
      if (mockOllamaPull?.state === "pulling") throw { code: "OLLAMA_BUSY", message: "已有模型正在拉取" };
      const name = String(args!.name ?? "").trim();
      const parts = name.split("/");
      const word = (value: string) => /^[a-zA-Z0-9][a-zA-Z0-9_.-]*$/.test(value);
      const valid = !!name && name.length <= 255 && parts.every((part, index) => {
        const colon = part.indexOf(":");
        if (colon < 0) return word(part);
        const left = part.slice(0, colon), right = part.slice(colon + 1);
        return word(left) && (index === parts.length - 1 ? word(right)
          : index === 0 && /^\d+$/.test(right) && Number(right) > 0 && Number(right) <= 65535);
      });
      if (!valid) throw { code: "OLLAMA_MODEL_INVALID", message: "模型名格式无效，例如 qwen3:0.6b" };
      mockOllamaPull = { id: uid(), name, state: "pulling", phase: "pulling manifest", startedAt: Date.now() };
      return structuredClone(mockOllamaPull) as T;
    }
    case "ollama_pull_status":
      updateMockOllamaPull();
      return structuredClone(mockOllamaPull) as T;
    case "ollama_cancel_pull": {
      updateMockOllamaPull();
      if (!mockOllamaPull || mockOllamaPull.id !== args!.id) throw { code: "OLLAMA_PULL_NOT_FOUND", message: "拉取任务已变更，请刷新" };
      if (mockOllamaPull.state === "pulling") { mockOllamaPull.state = "cancelled"; mockOllamaPull.endedAt = Date.now(); }
      return true as T;
    }
    case "mongodb_auth_status": return structuredClone(mongoAuthPreview(String(args!.version)).view) as T;
    case "mongodb_auth_connection":
    case "mongodb_auth_apply":
    case "mongodb_auth_password":
    case "mongodb_auth_reset": {
      const { view, password: saved } = mongoAuthPreview(String(args!.version));
      const input = args!.input as import("@nsb/schema").MongoAuthApply | undefined;
      if ((input?.revision ?? args!.revision) !== view.revision) throw { code: "MONGO_AUTH_CHANGED", message: "实例或认证已变化，请重新检查" };
      if (!view.running) throw { code: "MONGO_NOT_RUNNING", message: "请先启动 MongoDB" };
      const entry = mockMongoAuth.get(view.version)!;
      if (cmd === "mongodb_auth_connection") {
        const candidate = args!.credentials as import("@nsb/schema").MongoCredentials;
        if (candidate.username ? candidate.username !== view.username || candidate.password !== saved || candidate.authDatabase !== view.authDatabase : view.configured) throw { code: "MONGO_ACCESS_DENIED", message: "用户名或密码未通过验证" };
        view.administrator = !!candidate.username; view.hasPassword = !!candidate.password;
      } else if (cmd === "mongodb_auth_password") {
        if (!view.administrator) throw { code: "MONGO_ADMIN_REQUIRED", message: "请先验证管理员连接" };
        const password = String(args!.password);
        if (Array.from(password).length < 8) throw { code: "MONGO_PASSWORD_SHORT", message: "至少需要 8 个字符" };
        entry.password = password;
      } else if (cmd === "mongodb_auth_reset") {
        if (!view.configured || !view.username) throw { code: "MONGO_RESET_UNAVAILABLE", message: "没有可恢复的本机管理账号记录" };
        const password = String(args!.password);
        if (Array.from(password).length < 8) throw { code: "MONGO_PASSWORD_SHORT", message: "至少需要 8 个字符" };
        entry.password = password; view.hasPassword = true; view.administrator = true; view.authorization = true; view.problem = null;
      } else {
        if (!input!.acknowledgeRestart || (!input!.enabled && !input!.acknowledgeDisable)) throw { code: "MONGO_AUTH_CONFIRM", message: "请确认重启和关闭认证的影响" };
        if (input!.administrator) {
          const account = input!.administrator;
          if (view.hasUsers || view.configured || !input!.enabled || !account.username.trim() || account.authDatabase !== "admin" || Array.from(account.password).length < 8) throw { code: "MONGO_ADMIN_EXISTS", message: "请验证现有管理账号" };
          view.username = account.username; view.authDatabase = "admin"; entry.password = account.password;
          view.hasUsers = true; view.hasPassword = true; view.administrator = true;
        }
        if (!view.administrator) throw { code: "MONGO_ADMIN_REQUIRED", message: "请先验证管理员连接" };
        view.configured = input!.enabled; view.authorization = input!.enabled;
      }
      view.problem = null; view.revision = `preview-${Date.now()}`;
      return structuredClone(view) as T;
    }
    case "mongodb_backup_plan":
    case "db_backup_plan":
    case "postgres_backup_plan": return structuredClone(previewPlan(args!.version as string, cmd.startsWith("mongodb_") ? "mongodb" : args?.engine as string | undefined)) as T;
    case "mongodb_backup_plan_save":
    case "db_backup_plan_save":
    case "postgres_backup_plan_save": {
      const version = args!.version as string;
      const plan = previewPlan(version, cmd.startsWith("mongodb_") ? "mongodb" : args?.engine as string | undefined);
      if (plan.state === "running") throw { code: "BACKUP_BUSY", message: "自动备份正在执行" };
      const config = args!.config as import("./api").PostgresPlanConfig;
      if (!["daily", "weekly", "monthly"].includes(config.frequency) || !/^([01]\d|2[0-3]):[0-5]\d$/.test(config.time) || !Number.isInteger(config.keep) || config.keep < 0 || config.keep > 100 || config.weekday < 0 || config.weekday > 6 || config.monthDay < 1 || config.monthDay > 31) throw { code: "BAD_BACKUP_PLAN", message: "请检查计划时间与保留数量" };
      plan.config = { ...config }; plan.nextAt = config.enabled ? previewPlanNext(config) : null;
      return structuredClone(plan) as T;
    }
    case "mongodb_backup_plan_run":
    case "db_backup_plan_run":
    case "postgres_backup_plan_run": {
      const version = args!.version as string;
      const plan = previewPlan(version, cmd.startsWith("mongodb_") ? "mongodb" : args?.engine as string | undefined);
      if (plan.state === "running") throw { code: "BACKUP_BUSY", message: "自动备份正在执行" };
      plan.state = "running"; plan.lastRunAt = Date.now(); plan.finishedAt = null; plan.files = []; plan.message = "";
      plan.nextAt = plan.config.enabled ? previewPlanNext(plan.config) : null;
      await delay(1200);
      try {
        if (cmd === "mongodb_backup_plan_run") {
          const service = services.get("mongodb");
          if (!service || !service.pids.length || !["running", "error"].includes(service.state) || service.version !== version) throw { message: "所选 MongoDB 实例未运行或版本已变化" };
          const databases = [...mockMongoDatabases.keys()].filter(name => !["admin", "local", "config"].includes(name.toLowerCase()));
          const errors: string[] = [];
          for (const database of databases) {
            try {
              const backup = await mockInvoke<import("@nsb/schema").MongoBackup>("mongodb_backup_create", { version, database });
              mockMongoBackups.get(backup.id)!.record.kind = "automatic"; plan.files.push(backup.id);
              if (plan.config.keep) [...mockMongoBackups.values()].filter(entry => entry.record.kind === "automatic" && entry.record.version === version && entry.record.database === database).reverse().slice(plan.config.keep).forEach(entry => mockMongoBackups.delete(entry.record.id));
            } catch (error) { errors.push(`${database}: ${String((error as { message?: string }).message || error)}`); }
          }
          plan.state = errors.length ? (plan.files.length ? "partial" : "failed") : databases.length ? "success" : "skipped";
          plan.message = `预览：已备份 ${plan.files.length} / ${databases.length} 个业务数据库${errors.length ? "。" + errors.join("；") : ""}`;
        } else if (args?.engine) {
          const engine = args.engine as DatabaseEngine;
          const { state } = mysqlPreview(version, true, engine);
          const databases = [...state.databases.values()].filter((db) => !systemDatabase(db.name));
          for (const db of databases) {
            const hash = [...new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(db.name)))].map((byte) => byte.toString(16).padStart(2, "0")).join("");
            const label = `auto-${hash}`;
            const file = previewBackup(version, [db], `${label}-${db.name.replace(/[^\p{L}\p{N}_.-]/gu, "_").slice(0, 16)}`, engine); plan.files.push(file.name);
            const prefix = `${engine}-${version}-${label}-`;
            if (plan.config.keep) [...mockDbBackups.values()].filter((entry) => entry.name.startsWith(prefix)).reverse().slice(plan.config.keep).forEach((entry) => { mockDbBackups.delete(entry.path); mockBackupContents.delete(entry.path); });
          }
          plan.state = databases.length ? "success" : "skipped";
          plan.message = databases.length ? `预览：已备份 ${databases.length} 个业务数据库` : "没有可备份的业务数据库";
        } else {
        const databases = (await mockInvoke<import("./api").PostgresDatabaseInfo[]>("postgres_databases", { version })).filter((db) => !db.protected && db.allowConnections);
        for (const db of databases) {
          const prefix = `auto-postgresql-${version}-${db.oid}-`;
          const name = `${prefix}${db.name}-${Date.now()}.dump`;
          const file: DbBackupFile = { name, path: `C:/NiceEnv/backup/postgresql/${name}`, sizeBytes: 16384, createdAt: Math.floor(Date.now() / 1000) };
          mockPostgresBackups.set(name, { file, database: structuredClone(db) }); plan.files.push(name);
          if (plan.config.keep) [...mockPostgresBackups.keys()].filter((key) => key.startsWith(prefix)).reverse().slice(plan.config.keep).forEach((key) => mockPostgresBackups.delete(key));
        }
        plan.state = databases.length ? "success" : "skipped";
        plan.message = databases.length ? `预览：已备份 ${databases.length} 个业务数据库` : "没有可备份的业务数据库";
        }
      } catch (error) { plan.state = "failed"; plan.message = String((error as { message?: string }).message || error); }
      plan.finishedAt = Date.now();
      return structuredClone(plan) as T;
    }
    case "postgres_backup_list": return structuredClone([...mockPostgresBackups.values()].map((entry) => entry.file).sort((a, b) => b.createdAt - a.createdAt)) as T;
    case "mongodb_backup_list": return { items: Array.from(mockMongoBackups.values()).map(entry => structuredClone(entry.record)).reverse(), issues: [], unreadable: 0, directory: "preview/backup/mongodb" } as T;
    case "mongodb_backup_inspect_import": return { source: String(args!.source), info: { version: "8.0.4", toolsVersion: "100.19.0", compression: "gzip", databases: [{ name: "demo_external", collections: 1 }, { name: "demo_second", collections: 1 }] }, sizeBytes: 4096, sha256: "a".repeat(64), revision: `preview:${String(args!.source)}` } as T;
    case "mongodb_backup_import": {
      if (args!.revision !== `preview:${String(args!.source)}` || !["demo_external", "demo_second"].includes(String(args!.database))) throw { code: "MONGO_IMPORT_CHANGED", message: "请重新检查归档并选择数据库" };
      const data = { documents: [{ _id: { $oid: "0123456789abcdef01234567" }, title: "外部归档演示" }] };
      const id = `preview-${Date.now()}-${++mockMongoBackupSequence}`;
      const record: import("@nsb/schema").MongoBackup = { id, database: String(args!.database), version: "8.0.4", toolsVersion: "100.19.0", createdAt: Date.now()/1000, sizeBytes: 4096, sha256: "a".repeat(64), kind: "imported" };
      mockMongoBackups.set(id, { record, data }); return structuredClone(record) as T;
    }
    case "mongodb_backup_export": {
      if (!mockMongoBackups.has(String(args!.id))) throw { code: "MONGO_BACKUP_INVALID", message: "备份不存在" };
      return String(args!.destination) as T;
    }
    case "mongodb_backup_removal_preview":
    case "mongodb_backup_delete": {
      const entry = mockMongoBackups.get(String(args!.id));
      if (!entry) throw { code: "MONGO_BACKUP_INVALID", message: "备份不存在" };
      const revision = JSON.stringify(entry);
      if (cmd === "mongodb_backup_removal_preview") return { id:entry.record.id, database:entry.record.database, kind:entry.record.kind, sizeBytes:entry.record.sizeBytes, revision } as T;
      if (args!.revision !== revision) throw { code: "MONGO_BACKUP_CHANGED", message: "备份已变化，请重新检查" };
      mockMongoBackups.delete(entry.record.id); return undefined as T;
    }
    case "mongodb_backup_create":
    case "mongodb_restore_preview":
    case "mongodb_backup_restore": {
      const service = services.get("mongodb");
      if (!service || !["running", "error"].includes(service.state) || !service.pids.length || service.version !== args!.version) throw { code: "MONGO_NOT_RUNNING", message: "所选 MongoDB 实例未运行或运行版本已变化" };
      if (!Array.from(packages.values()).some(pkg => pkg.id === "mongosh" && pkg.install)) throw { code: "MONGO_SHELL_MISSING", message: "请先安装 MongoDB Shell" };
      const tools = Array.from(packages.values()).filter(pkg => pkg.id === "mongodb-database-tools" && pkg.install);
      const selectedTools = tools.find(pkg => pkg.active) ?? tools.sort((a,b) => cmpVersionDesc(a.version,b.version))[0];
      if (!selectedTools) throw { code: "MONGO_TOOLS_MISSING", message: "请先安装 MongoDB Database Tools" };
      const target = String(args!.database ?? args!.target ?? "");
      if (!target || new TextEncoder().encode(target).length > 63 || /[\s\x00-\x1f\x7f-\x9f/\\."$*<>:|?]/.test(target) || ["admin","local","config"].includes(target.toLowerCase())) throw { code: "MONGO_BACKUP_INVALID", message: "数据库名称无效" };
      if (cmd === "mongodb_backup_create") return mockMongoBackup(target,service.version!,selectedTools.version,"manual") as T;
      const entry = mockMongoBackups.get(String(args!.id));
      if (!entry) throw { code: "MONGO_BACKUP_INVALID", message: "备份已不存在" };
      if (entry.record.version.split(".")[0] !== service.version!.split(".")[0]) throw { code: "MONGO_BACKUP_VERSION", message: "备份与目标 MongoDB 主版本不同" };
      if (!tools.some(pkg => pkg.version === entry.record.toolsVersion)) throw { code: "MONGO_TOOLS_MISSING", message: "请安装创建备份时的 Database Tools 版本" };
      const revision = JSON.stringify([entry.record,target,mockMongoDatabases.get(target),service.version,service.port,service.pids]);
      if (cmd === "mongodb_restore_preview") return { backup: structuredClone(entry.record), target, exists: mockMongoDatabases.has(target), revision } as T;
      if (args!.revision !== revision || args!.confirmation !== target) throw { code: "MONGO_RESTORE_CHANGED", message: "请重新检查并确认恢复" };
      const safetyBackup = mockMongoDatabases.has(target) ? mockMongoBackup(target,service.version!,selectedTools.version,"before-restore") : null;
      mockMongoDatabases.set(target,structuredClone(entry.data)); return { target, safetyBackup } as T;
    }
    case "mongodb_browse": {
      const service = services.get("mongodb");
      if (!service || !["running", "error"].includes(service.state) || !service.pids.length || service.version !== args!.version) throw { code: "MONGO_NOT_RUNNING", message: "所选 MongoDB 实例未运行或运行版本已变化" };
      const shells = Array.from(packages.values()).filter(pkg => pkg.id === "mongosh" && pkg.install).sort((a,b) => cmpVersionDesc(a.version,b.version));
      const shell = shells.find(pkg => pkg.active) ?? shells[0];
      if (!shell) throw { code: "MONGO_SHELL_MISSING", message: "请先安装 MongoDB Shell，以启用数据库浏览" };
      const request = args!.request as { action: string; database?: string; collection?: string; search?: string; offset?: number; limit?: number; filter?: import("@nsb/schema").MongoFilter | null };
      if (request.action === "overview") return { kind: "overview", version: service.version, serverVersion: service.version, port: service.port, uri: `mongodb://127.0.0.1:${service.port}`, shellVersion: shell.version, databases: ["admin", "local", ...mockMongoDatabases.keys()], limited: false } as T;
      const collections = mockMongoDatabases.get(request.database ?? "") ?? {};
      if (request.action === "collections") return { kind: "collections", database: request.database, entries: Object.keys(collections).filter(name => name.toLowerCase().includes((request.search ?? "").toLowerCase())).map(name => ({ name, kind: "collection" })), limited: false } as T;
      if (request.action !== "documents" || !collections[request.collection ?? ""]) throw { code: "MONGO_COLLECTION_MISSING", message: "所选集合已不存在，请刷新集合列表" };
      let rows = structuredClone(collections[request.collection!]);
      const filter = request.filter;
      if (filter) rows = rows.filter(row => {
        let current: unknown = row;
        for (const part of filter.field.split(".")) current = current && typeof current === "object" ? (current as Record<string,unknown>)[part] : undefined;
        if (filter.valueType === "null") return current == null;
        if (filter.valueType === "objectId") return !!current && typeof current === "object" && (current as Record<string,unknown>).$oid === filter.value.toLowerCase();
        if (filter.valueType === "number") return !!current && typeof current === "object" && Number((current as Record<string,unknown>).$numberInt) === Number(filter.value);
        if (filter.valueType === "boolean") return current === (filter.value === "true");
        return current === filter.value;
      });
      const offset = request.offset ?? 0, limit = request.limit ?? 10;
      return { kind: "documents", database: request.database, collection: request.collection, offset, limit, documents: rows.slice(offset,offset+limit).map(row => ({ content: JSON.stringify(row,null,2), truncated: false })), hasMore: offset+limit<rows.length } as T;
    }
    case "postgres_backup_dir": return "C:/NiceEnv/backup/postgresql" as T;
    case "postgres_backup_delete": {
      if (!mockPostgresBackups.delete(args!.name as string)) throw { code: "FILE_NOT_FOUND", message: "备份文件已不存在" };
      return undefined as T;
    }
    case "postgres_backup_dump":
    case "postgres_backup_restore":
    case "postgres_backup_replace":
    case "postgres_connection":
    case "postgres_password":
    case "postgres_databases":
    case "postgres_roles":
    case "postgres_role_access":
    case "postgres_role_access_save":
    case "postgres_create_database":
    case "postgres_drop_database":
    case "postgres_create_role":
    case "postgres_set_role_password":
    case "postgres_drop_role":
    case "postgres_set_password": {
      const version = args!.version as string;
      const service = services.get("postgresql");
      if (!service || !["running", "error"].includes(service.state) || !service.pids.length || service.version !== version) {
        throw { code: "POSTGRES_NOT_RUNNING", message: "所选 PostgreSQL 实例未运行或版本已变化" };
      }
      let connection = mockPostgresConnections.get(version);
      if (!connection) {
        const password = `preview-${crypto.randomUUID()}`;
        connection = { password, saved: password, passwordRequired: true };
        mockPostgresConnections.set(version, connection);
      }
      if (cmd === "postgres_set_password") {
        const password = args!.password as string;
        if (!password || new TextEncoder().encode(password).length > 4096 || /[\x00-\x1f\x7f-\x9f]/.test(password)) throw { code: "BAD_PASSWORD", message: "密码须为 1–4096 字节且不能包含控制字符" };
        if (args!.useExisting) {
          if (!connection.passwordRequired) throw { code: "POSTGRES_AUTH_DISABLED", message: "本机连接无需密码，无法验证输入的密码" };
          if (password !== connection.password) throw { code: "POSTGRES_CONNECTION_FAILED", message: "密码不正确，请输入当前 PostgreSQL 实例的 postgres 密码。" };
        } else {
          if (connection.passwordRequired && connection.saved !== connection.password) throw { code: "POSTGRES_CONNECTION_FAILED", message: "请先更新本机连接密码" };
          connection.password = password;
          if (args!.enablePasswordAuth) connection.passwordRequired = true;
        }
        connection.saved = password;
        return undefined as T;
      }
      if (connection.passwordRequired && connection.saved !== connection.password) throw { code: "POSTGRES_CONNECTION_FAILED", message: "请先更新本机连接密码" };
      if (cmd === "postgres_password") {
        if (!connection.passwordRequired) throw { code: "POSTGRES_AUTH_DISABLED", message: "本机连接无需密码，无法验证保存的密码" };
        return connection.saved as T;
      }
      const data = postgresPreview(version);
      if (cmd === "postgres_role_access" || cmd === "postgres_role_access_save") {
        const input = args!.input as import("./api").PostgresRoleAccessInput | undefined;
        const role = data.roles.find((role) => role.name === (input?.name ?? args!.name) && role.oid === (input?.oid ?? args!.oid));
        if (!role) throw { code: "POSTGRES_TARGET_CHANGED", message: "账号已删除或更名，请刷新列表" };
        const snapshot = (): import("./api").PostgresRoleAccess => ({ oid: role.oid, name: role.name, canLogin: role.canLogin, connectionLimit: role.connectionLimit,
          activeConnections: 0, protected: role.protected, revision: JSON.stringify([role.oid, role.name, role.canLogin, role.connectionLimit, role.protected]) });
        if (cmd === "postgres_role_access_save" && input) {
          if (role.protected) throw { code: "POSTGRES_PROTECTED", message: "系统账号或超级用户的连接设置受保护" };
          if (!Number.isInteger(input.connectionLimit) || input.connectionLimit < -1 || input.connectionLimit > 2147483647) throw { code: "POSTGRES_BAD_LIMIT", message: "连接数上限须为 0–2147483647，或选择不限" };
          if (snapshot().revision !== input.revision) throw { code: "POSTGRES_ACCESS_CHANGED", message: "账号已修改，请重新读取后保存" };
          const restricting = (role.canLogin && !input.canLogin) || (input.connectionLimit >= 0 && (role.connectionLimit === -1 || input.connectionLimit < role.connectionLimit));
          if (restricting && !input.confirmRestriction) throw { code: "POSTGRES_CONFIRM_RESTRICTION", message: "请先确认限制新连接的影响" };
          await delay(700);
          if (!data.roles.includes(role) || snapshot().revision !== input.revision) throw { code: "POSTGRES_ACCESS_CHANGED", message: "账号已变化，请重新读取后保存" };
          role.canLogin = input.canLogin; role.connectionLimit = input.connectionLimit;
        }
        return structuredClone(snapshot()) as T;
      }
      if (cmd === "postgres_backup_replace") {
        const input = args!.input as import("./api").PostgresReplaceInput;
        if (!input.trusted) throw { code: "POSTGRES_BACKUP_UNTRUSTED", message: "请先确认备份来源可信" };
        if (input.confirmedName !== input.name) throw { code: "POSTGRES_CONFIRM_NAME", message: "请输入完整数据库名称" };
        const target = data.databases.find((db) => db.name === input.name && db.oid === input.oid);
        if (!target) throw { code: "POSTGRES_TARGET_CHANGED", message: "目标数据库已变化，请刷新后重新确认" };
        if (target.protected || !target.allowConnections) throw { code: "POSTGRES_PROTECTED", message: "只能替换可连接的业务数据库" };
        const backup = [...mockPostgresBackups.values()].find((entry) => entry.file.path === input.path);
        if (!backup) throw { code: "BAD_BACKUP_FILE", message: "找不到有效的 PostgreSQL custom 归档" };
        if (!data.roles.some((role) => role.name === input.owner && role.canLogin)) throw { code: "POSTGRES_OWNER_CHANGED", message: "所选所有者不存在或无法登录" };
        const savedName = `postgresql-${version}-${target.name}-${Date.now()}.dump`;
        const savedFile: DbBackupFile = { name: savedName, path: `C:/NiceEnv/backup/postgresql/${savedName}`, sizeBytes: 16384, createdAt: Math.floor(Date.now() / 1000) };
        mockPostgresBackups.set(savedName, { file: savedFile, database: structuredClone(target) });
        emitLocal("postgres://backup", { operationId: args!.operationId, database: target.name, bytes: 0, state: "running" });
        await delay(1200);
        if (!data.databases.some((db) => db.name === input.name && db.oid === input.oid)) throw { code: "POSTGRES_TARGET_CHANGED", message: "目标数据库已变化，未执行切换" };
        const previousDatabase = `niceenv_previous_${crypto.randomUUID().replaceAll("-", "").slice(0, 16)}`;
        target.name = previousDatabase;
        data.databases.push({ ...structuredClone(backup.database), oid: data.nextOid++, name: input.name, owner: input.owner });
        return { database: input.name, previousDatabase, safetyBackup: savedFile.path } as T;
      }
      if (cmd === "postgres_backup_dump") {
        const database = data.databases.find((db) => db.name === args!.name && db.oid === args!.oid);
        if (!database || database.protected || !database.allowConnections) throw { code: "POSTGRES_TARGET_CHANGED", message: "请选择当前实例中的业务数据库" };
        emitLocal("postgres://backup", { operationId: args!.operationId, database: database.name, bytes: 0, state: "running" });
        await delay(700);
        const name = `postgresql-${version}-${database.name}-${Date.now()}.dump`;
        const file: DbBackupFile = { name, path: `C:/NiceEnv/backup/postgresql/${name}`, sizeBytes: 16384, createdAt: Math.floor(Date.now() / 1000) };
        mockPostgresBackups.set(name, { file, database: structuredClone(database) });
        return file.path as T;
      }
      if (cmd === "postgres_backup_restore") {
        if (!args!.trusted) throw { code: "POSTGRES_BACKUP_UNTRUSTED", message: "请先确认备份来源可信" };
        const backup = [...mockPostgresBackups.values()].find((entry) => entry.file.path === args!.path);
        if (!backup) throw { code: "BAD_BACKUP_FILE", message: "找不到有效的 PostgreSQL custom 归档" };
        const name = args!.name as string;
        if (!/^[A-Za-z0-9_]{1,63}$/.test(name) || /^pg_/i.test(name) || /^(postgres|template0|template1)$/i.test(name)) throw { code: "POSTGRES_BAD_NAME", message: "名称无效或为系统保留名称" };
        if (data.databases.some((db) => db.name === name)) throw { code: "POSTGRES_DATABASE_EXISTS", message: "此数据库已存在，请更换新名称" };
        if (!data.roles.some((role) => role.name === args!.owner && role.canLogin)) throw { code: "POSTGRES_OWNER_CHANGED", message: "所选所有者不存在或无法登录" };
        emitLocal("postgres://backup", { operationId: args!.operationId, database: name, bytes: 0, state: "running" });
        await delay(900);
        data.databases.push({ ...structuredClone(backup.database), oid: data.nextOid++, name, owner: args!.owner as string });
        return undefined as T;
      }
      if (cmd === "postgres_databases") return structuredClone(data.databases) as T;
      if (cmd === "postgres_roles") return structuredClone(data.roles.map((role) => ({ ...role, databases: data.databases.filter((db) => db.owner === role.name).map((db) => db.name) }))) as T;
      const name = args?.name as string;
      if (cmd === "postgres_create_database" || cmd === "postgres_create_role") {
        if (!/^[A-Za-z0-9_]{1,63}$/.test(name) || /^pg_/i.test(name) || /^(postgres|template0|template1)$/i.test(name)) throw { code: "POSTGRES_BAD_NAME", message: "名称无效或为系统保留名称" };
      }
      if (cmd === "postgres_create_database") {
        if (data.databases.some((db) => db.name === name)) throw { code: "POSTGRES_DATABASE_EXISTS", message: "同名数据库已存在，请更换名称" };
        const owner = args!.owner as string;
        if (!data.roles.some((role) => role.name === owner && role.canLogin)) throw { code: "POSTGRES_OWNER_CHANGED", message: "所选所有者不存在或无法登录" };
        data.databases.push({ oid: data.nextOid++, name, owner, encoding: "UTF8", sizeBytes: 8_388_608, protected: false, allowConnections: true });
        return undefined as T;
      }
      if (cmd === "postgres_drop_database") {
        const db = data.databases.find((db) => db.name === name && db.oid === args!.oid);
        if (!db) throw { code: "POSTGRES_TARGET_CHANGED", message: "数据库已变化，请刷新后重试" };
        if (db.protected) throw { code: "POSTGRES_PROTECTED", message: "不能删除系统数据库" };
        data.databases = data.databases.filter((row) => row !== db); return undefined as T;
      }
      if (["postgres_create_role", "postgres_set_role_password", "postgres_drop_role"].includes(cmd)) {
        const role = data.roles.find((role) => role.name === name && role.oid === args!.oid);
        if (cmd !== "postgres_create_role" && !role) throw { code: "POSTGRES_TARGET_CHANGED", message: "账号已变化，请刷新后重试" };
        if (role?.protected) throw { code: "POSTGRES_PROTECTED", message: "系统账号受保护" };
        if (cmd === "postgres_drop_role") {
          if (data.databases.some((db) => db.owner === name)) throw { code: "POSTGRES_ROLE_DEPENDENCY", message: "账号仍拥有数据库，请先处理依赖。账号与数据均已保留。" };
          data.roles = data.roles.filter((row) => row !== role); data.passwords.delete(name); return undefined as T;
        }
        const password = args!.password as string;
        if (!password || new TextEncoder().encode(password).length > 4096 || /[\x00-\x1f\x7f-\x9f]/.test(password)) throw { code: "BAD_PASSWORD", message: "密码须为 1–4096 字节且不能包含控制字符" };
        if (cmd === "postgres_create_role") {
          if (data.roles.some((row) => row.name === name)) throw { code: "POSTGRES_ROLE_EXISTS", message: "同名账号已存在，未修改密码" };
          data.roles.push({ oid: data.nextOid++, name, canLogin: true, connectionLimit: -1, superuser: false, createDb: false, createRole: false, replication: false, bypassRls: false, protected: false, databases: [] });
        } else if (!role?.canLogin) { throw { code: "POSTGRES_ROLE_NOLOGIN", message: "此角色未启用登录" }; }
        data.passwords.set(name, password); return undefined as T;
      }
      const databases = data.databases.filter((db) => !["template0", "template1"].includes(db.name));
      return { version, port: service.port, serverVersion: version, databaseCount: databases.length, sizeBytes: databases.reduce((sum, db) => sum + db.sizeBytes, 0), passwordRequired: connection.passwordRequired } as T;
    }
    case "redis_connection": {
      const version = args!.version as string;
      const credentials = mockRedisConnections.get(version);
      return { version, username: credentials?.username ?? "", hasPassword: !!credentials?.password } as T;
    }
    case "redis_password": {
      const version = args!.version as string;
      if (!redisPasswordsPreview.has(version)) redisPasswordsPreview.set(version, { version, revision: "preview-password-0", enabled: false, blockedReason: null });
      return structuredClone(redisPasswordsPreview.get(version)) as T;
    }
    case "redis_password_save": {
      const version = args!.version as string;
      const previous = redisPasswordsPreview.get(version);
      if (services.get("redis")?.state !== "stopped") throw { code: "REDIS_PASSWORD_RUNNING", message: "请先停止演示 Redis，再保存服务密码。" };
      if (!previous || previous.revision !== args!.revision) throw { code: "CONFIG_CONFLICT", message: "配置已变化，请重新读取。" };
      const password = args!.password as string;
      if (new TextEncoder().encode(password).length > 512 || /[\x00-\x1f\x7f-\x9f]/.test(password) || (password.length > 0 && !password.trim())) throw { code: "REDIS_PASSWORD_INVALID", message: "密码须为 1–512 字节，不能仅为空白或包含控制字符。" };
      if (!password && !args!.acknowledgeDisable) throw { code: "REDIS_PASSWORD_CONFIRM", message: "请先确认关闭密码认证的影响。" };
      const view = { ...previous, enabled: !!password, revision: `preview-password-${Date.now()}` };
      redisPasswordsPreview.set(version, view); redisServerPasswordsPreview.set(version, password); mockRedisConnections.set(version, { username: "", password });
      return { view: structuredClone(view), connectionSaved: true } as T;
    }
    case "redis_password_stop": {
      const service = services.get("redis");
      if (service?.version !== args!.version) throw { code: "REDIS_INSTANCE_CHANGED", message: "演示 Redis 版本已变化，未停止。" };
      return await mockInvoke<T>("stop_service", { id: "redis" });
    }
    case "redis_save_connection": {
      const service = services.get("redis");
      const version = args!.version as string;
      if (service?.state !== "running" || service.version !== version) throw { code: "REDIS_INSTANCE_CHANGED", message: "运行中的 Redis 版本已变化，请重新打开连接设置" };
      const credentials = args!.credentials as { username: string; password: string };
      // 演示认证对应演示服务密码；真实凭据只在桌面应用中验证。
      if ((credentials.username && credentials.username !== "default") || credentials.password !== (redisServerPasswordsPreview.get(version) ?? "")) throw { code: "REDIS_AUTH_FAILED", message: "连接凭据与演示 Redis 的服务密码不一致；真实凭据请在桌面应用中验证。" };
      mockRedisConnections.set(version, { ...credentials });
      return await mockInvoke<T>("redis_stats");
    }
    case "redis_backup_list": return { items: structuredClone(redisBackupsPreview.map(entry => ({ ...entry, problem: null }))), unreadable: 0, directory: "preview/backup/redis" } as T;
    case "redis_backup_removal_preview": {
      const entry = redisBackupsPreview.find(entry => entry.id === args!.id);
      if (!entry) throw { code: "REDIS_BACKUP_INVALID", message: "演示备份已不存在，请刷新列表。" };
      return { entry: { ...structuredClone(entry), problem: null }, revision: `${entry.id}:${entry.sha256}` } as T;
    }
    case "redis_backup_delete": {
      const entry = redisBackupsPreview.find(entry => entry.id === args!.id);
      if (!entry || args!.revision !== `${entry.id}:${entry.sha256}`) throw { code: "REDIS_BACKUP_CHANGED", message: "演示备份已变化，请重新检查。" };
      redisBackupsPreview.splice(redisBackupsPreview.indexOf(entry), 1); return undefined as T;
    }
    case "redis_backup_export": {
      if (!redisBackupsPreview.some(entry => entry.id === args!.id)) throw { code: "REDIS_BACKUP_INVALID", message: "演示备份已不存在。" };
      return String(args!.destination) as T;
    }
    case "redis_backup_inspect_import": return { source: String(args!.source), version: "5.0.14", rdbVersion: 9, sizeBytes: 4096, sha256: "a".repeat(64), revision: `preview:${String(args!.source)}` } as T;
    case "redis_backup_import": {
      const source = String(args!.source);
      if (args!.revision !== `preview:${source}`) throw { code: "REDIS_IMPORT_CHANGED", message: "演示文件已变化，请重新检查。" };
      const entry = { id: `${Date.now()}-${Math.random().toString(16).slice(2)}`, version: "5.0.14", createdAt: Date.now(), sizeBytes: 4096, sha256: "a".repeat(64), kind: "imported" as const };
      redisBackupsPreview.unshift(entry); return structuredClone(entry) as T;
    }
    case "redis_backup_create": {
      const version = String(args!.version), service = services.get("redis");
      if (service?.state !== "running" || service.version !== version) throw { code: "REDIS_NOT_RUNNING", message: "请启动对应 Redis 版本后备份。" };
      const id = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
      const entry = { id, version, createdAt: Date.now(), sizeBytes: 1024, sha256: id.replaceAll("-", "").padEnd(64, "0").slice(0, 64), kind: "snapshot" as const };
      redisBackupsPreview.unshift(entry); return structuredClone(entry) as T;
    }
    case "redis_restore_preview": {
      const version = String(args!.version), service = services.get("redis");
      if (service?.state !== "stopped") throw { code: "REDIS_RESTORE_RUNNING", message: "请先停止 Redis。" };
      const backup = redisBackupsPreview.find(entry => entry.id === args!.id);
      if (!backup || backup.version !== version || service.version !== version) throw { code: "REDIS_RESTORE_VERSION", message: "请选用备份对应的 Redis 版本。" };
      return { backup: structuredClone(backup), target: "preview/data/redis/dump.rdb", existingSize: 1024, revision: `${backup.id}:${redisRestoreRevisionPreview}` } as T;
    }
    case "redis_backup_restore": {
      const preview = await mockInvoke<import("@nsb/schema").RedisRestorePreview>("redis_restore_preview", args);
      if (args!.confirmation !== `Redis ${args!.version}`) throw { code: "REDIS_RESTORE_CONFIRM", message: "请输入 Redis 名称和版本。" };
      if (args!.revision !== preview.revision) throw { code: "REDIS_RESTORE_CHANGED", message: "演示恢复范围已变化，请重新检查。" };
      const id = `${Date.now()}-${Math.random().toString(16).slice(2)}`;
      const safetyBackup = { ...preview.backup, id, kind: "before-restore" as const, createdAt: Date.now() };
      redisBackupsPreview.unshift(safetyBackup); redisRestoreRevisionPreview++;
      return { target: preview.target, safetyBackup } as T;
    }
    case "redis_persistence": {
      const service = services.get("redis"), version = String(args!.version);
      if (service?.state !== "running" || service.version !== version) throw { code: "REDIS_INSTANCE_CHANGED", message: "请启动所选 Redis 版本后重新读取。" };
      const runId = `preview-${version}-${service.pids.join("-")}`;
      let entry = redisPersistencePreview.get(version);
      if (!entry || entry.report.runId !== runId) {
        entry = { report: { version, runId, processId: service.pids[0], loading: false, saving: false, changesSinceSave: 0, lastSaveTime: Math.floor(Date.now() / 1000), lastSaveStatus: "ok", lastSaveDuration: null, aofEnabled: false, aofRewriting: false, aofRewriteScheduled: false, aofLastRewriteStatus: "ok", aofLastWriteStatus: null }, finishAt: 0, minimumSaveTime: 0 };
        redisPersistencePreview.set(version, entry);
      }
      if (entry.report.saving && Date.now() >= entry.finishAt) Object.assign(entry.report, { saving: false, lastSaveTime: entry.minimumSaveTime, lastSaveStatus: "ok", lastSaveDuration: 1, changesSinceSave: 0 });
      return structuredClone(entry.report) as T;
    }
    case "redis_snapshot": {
      const version = String(args!.version);
      await mockInvoke("redis_persistence", { version });
      const entry = redisPersistencePreview.get(version)!;
      if (entry.report.saving) throw { code: "REDIS_PERSISTENCE_BUSY", message: "演示快照仍在生成。" };
      entry.minimumSaveTime = Math.max(Math.floor(Date.now() / 1000), entry.report.lastSaveTime + 1);
      entry.report.saving = true; entry.finishAt = Date.now() + 1200;
      return { version, runId: entry.report.runId, processId: entry.report.processId, minimumSaveTime: entry.minimumSaveTime } as T;
    }
    case "redis_settings": {
      const version = String(args!.version);
      if (!redisSettingsPreview.has(version)) redisSettingsPreview.set(version, { version, path: `preview/etc/redis/${version}/redis.conf`, revision: "preview-0", appendOnly: false,
        settings: { maxMemoryBytes: 268435456, evictionPolicy: "allkeys-lru", timeoutSeconds: 0, maxClients: null, saveRules: [], appendFsync: null } });
      return structuredClone(redisSettingsPreview.get(version)) as T;
    }
    case "redis_settings_save": {
      const version = String(args!.version);
      const previous = redisSettingsPreview.get(version);
      if (!previous || previous.revision !== args!.revision) throw { code: "CONFIG_CONFLICT", message: "演示配置已变化，请重新读取。" };
      const settings = structuredClone(args!.settings) as import("@nsb/schema").RedisSettings;
      if (settings.saveRules?.length === 0 && previous.settings.saveRules?.length !== 0 && !args!.acknowledgeDisable) throw { code: "REDIS_SNAPSHOT_CONFIRM", message: "请确认关闭自动快照。" };
      const view = { ...previous, settings, revision: `preview-${Date.now()}-${Math.random()}` };
      redisSettingsPreview.set(version, view);
      return structuredClone(view) as T;
    }
    case "redis_stats": {
      const service = services.get("redis");
      if (service?.state !== "running") throw { code: "REDIS_NOT_RUNNING", message: "请先启动 Redis 实例" };
      return { reachable: true, port: service.port, usedMemoryHuman: "1.5M", keys: 0, uptimeDays: 0, connectedClients: 1 } as T;
    }
    case "adminer_status": return mockAdminer as T;
    case "adminer_start":
      throw { code: "DESKTOP_ONLY", message: "请在桌面应用中启动数据库管理台；网页预览不能启动本机 PHP 服务。" };
    case "adminer_stop":
      mockAdminer = null;
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
    case "validate_configs": {
      const files = await mockInvoke<ConfigFileInfo[]>("config_list");
      const installed = Array.from(packages.values()).filter((p) => p.install);
      const checks: ConfigCheck[] = [];
      for (const [id, kind, label] of [["nginx", "nginx-main", "Nginx"], ["apache", "apache-conf", "Apache"], ["php", "php-ini", "PHP"], ["mysql", "mysql-ini", "MySQL"], ["redis", "redis-conf", "Redis"]]) {
        const targets = files.filter((file) => file.requiresPackage === id);
        if (!targets.length) checks.push({ kind, name: label, path: null, method: "none", ok: false, status: "skipped", detail: "未安装，未执行检查", checkedAt: now() });
        for (const file of targets) {
          const version = file.kind.split("@")[1] ?? (installed.find((p) => p.id === id && p.active) ?? installed.find((p) => p.id === id))?.version;
          const method = id === "mysql" || id === "redis" ? "readability" : "native";
          const result = file.exists ? await mockInvoke<ConfigValidation>("config_validate", { kind: file.kind, content: currentConfigContent(file.kind) }) : null;
          const ok = file.exists && (method === "readability" || !!result?.ok);
          checks.push({ kind: file.kind, name: `${label} ${version ?? ""}`.trim(), path: file.path, method, ok,
            status: ok ? "ok" : "fail", checkedAt: now(),
            detail: !file.exists ? "配置文件尚未生成，可在修复向导中生成默认配置" : method === "readability" ? "演示文件可读取；未执行原生语法校验或数据库连接" : result?.ok ? "浏览器演示检查通过；桌面端将执行原生校验" : result!.issues.map((issue) => `第 ${issue.line} 行：${issue.message}`).join("\n"),
          });
        }
      }
      const only = args?.only as string[] | null | undefined;
      if (only && (!only.length || only.some((key) => !checks.some((check) => check.kind === key)))) throw { code: "CONFIG_CHECK_TARGET_CHANGED", message: "检查目标已变更，请重新执行完整体检" };
      return (only ? checks.filter((check) => only.includes(check.kind)) : checks) as T;
    }
    case "get_app_version":
      return MOCK_APP_VERSION as T;
    case "get_data_dir":
      return "C:\\Users\\Demo\\AppData\\Local\\NiceEnv" as T;
    case "migrate_data_dir":
      throw { code: "DESKTOP_ONLY", message: "数据目录迁移需要在桌面应用中执行" };
    case "pending_data_dir_migration":
      return null as T;
    case "cancel_data_dir_migration":
      return true as T;
    case "restart_app":
      throw { code: "DESKTOP_ONLY", message: "浏览器无法重启 NiceEnv，请在桌面端执行" };
    case "frontend_ready":
      return false as T;
    case "open_in_folder":
      throw { code: "DESKTOP_ONLY", message: "浏览器无法打开本机文件夹，请使用桌面端" };
    case "open_terminal":
      throw { code: "DESKTOP_ONLY", message: "浏览器无法打开本机终端，请使用桌面端" };
    case "refresh_remote_manifest":
      return { revision: 2, packages: 160, path: "C:\\Users\\Demo\\AppData\\Local\\NiceEnv\\etc\\manifest.json", takesEffect: "restart" } as T;
    case "manifest_status":
      return {
        bundledRevision: bundledManifest.revision,
        bundledPackages: bundledManifest.packages.length,
        effectiveRevision: bundledManifest.revision,
        effectivePackages: bundledManifest.packages.length,
        remoteActive: false,
        userModules: [],
      } as T;
    case "reset_remote_manifest":
      return true as T;
    case "check_updates":
      // 演示模式：报告一个可用新版，方便在浏览器里走通「检查更新 → 弹窗 → 下载」流程
      return {
        appVersion: MOCK_APP_VERSION,
        latestVersion: MOCK_NEXT_VERSION,
        releaseUrl: "https://github.com/nsmao-com/nice_env/releases",
        manifestRevision: 1,
        // 同时演示「套件清单有更新 → 应用新清单」
        manifestUpdate: true,
        appUpdate: true,
        release: {
          tag: `v${MOCK_NEXT_VERSION}`,
          htmlUrl: `https://github.com/nsmao-com/nice_env/releases/tag/v${MOCK_NEXT_VERSION}`,
          body: "## 更新内容\n\n- 设置页新增主题色与字体自定义\n- 代码块支持行号 / 高亮 / 复制\n- 托盘菜单重新设计\n- 修复若干问题",
          publishedAt: new Date(now() - 86400_000).toISOString(),
          assetName: `NiceEnv_${MOCK_NEXT_VERSION}_x64-setup.exe`,
          assetUrl: `https://github.com/nsmao-com/nice_env/releases/download/v${MOCK_NEXT_VERSION}/NiceEnv_${MOCK_NEXT_VERSION}_x64-setup.exe`,
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
        path: `C:\\Users\\demo\\AppData\\Roaming\\NiceEnv\\updates\\NiceEnv_${MOCK_NEXT_VERSION}_x64-setup.exe`,
        fileName: `NiceEnv_${MOCK_NEXT_VERSION}_x64-setup.exe`,
        sizeBytes: total,
      } as T;
    }
    case "certauto_list":
      seedCertAutos();
      return [...certAutos.values()] as T;
    case "certauto_save": {
      seedCertAutos();
      const a = args!.a as CertAutomation;
      const previous = a.id ? certAutos.get(a.id) : undefined;
      if (a.id && !previous) throw { code: "NOT_FOUND", message: "自动化不存在，可能已在其它窗口删除" };
      if (previous && ["issuing", "manual_wait", "deploying"].includes(previous.state)) throw { code: "CERT_AUTO_BUSY", message: "该证书自动化正在执行，请等待当前任务结束" };
      if (previous && a.updatedAt !== previous.updatedAt) throw { code: "CERT_AUTO_CONFLICT", message: "自动化已更新，请重新打开编辑后重试" };
      const next: CertAutomation = {
        ...a, id: previous?.id || `auto-${uid()}`, updatedAt: Math.max(Date.now(), (previous?.updatedAt ?? 0) + 1),
        enabled: previous?.enabled ?? a.enabled, createdAt: previous?.createdAt ?? Date.now(),
        state: previous?.state ?? "idle", lastError: previous?.lastError ?? "", certId: previous?.certId ?? null,
        issuedAt: previous?.issuedAt ?? null, expiresAt: previous?.expiresAt ?? null,
        deploymentId: previous?.deploymentId ?? "",
        localDeployResult: previous?.deployLocal === a.deployLocal ? previous.localDeployResult : null,
        lastRunAt: previous?.lastRunAt ?? 0, nextRenewAt: previous?.nextRenewAt ?? 0,
        runs: previous?.runs ?? [], manualRecords: previous?.manualRecords ?? [], failCount: previous?.failCount ?? 0,
        targets: a.targets.map(target => ({ ...target, lastResult: previous?.targets.find(old => old.id === target.id && old.kind === target.kind && JSON.stringify(old.config) === JSON.stringify(target.config))?.lastResult ?? null })),
      };
      if (previous && (JSON.stringify(a.domains) !== JSON.stringify(previous.domains) || a.ca !== previous.ca || a.keyAlg !== previous.keyAlg)) {
        next.state = "idle"; next.lastError = ""; next.certId = null; next.issuedAt = null; next.expiresAt = null; next.failCount = 0;
        next.deploymentId = ""; next.localDeployResult = null;
        next.nextRenewAt = 0; next.targets = next.targets.map(target => ({ ...target, lastResult: null }));
      } else if (next.deploymentId && next.state !== "deploy_interrupted" && ((next.deployLocal && !next.localDeployResult) || next.targets.some(target => !target.lastResult))) {
        next.state = "deploy_error"; next.lastError = "部署配置已更新，请重试部署以应用已签发证书";
        next.nextRenewAt = next.enabled ? Date.now() : 0;
      }
      if (previous && previous.deployLocal !== next.deployLocal) next.certId = null;
      certAutos.set(next.id, next);
      return next as T;
    }
    case "certauto_delete": {
      const previous = certAutos.get(args!.id as string);
      if (!previous) throw { code: "NOT_FOUND", message: "自动化不存在" };
      if (["issuing", "manual_wait", "deploying"].includes(previous.state)) throw { code: "CERT_AUTO_BUSY", message: "该证书自动化正在执行，请等待当前任务结束" };
      certAutos.delete(args!.id as string);
      return true as T;
    }
    case "certauto_set_enabled": {
      const a0 = certAutos.get(args!.id as string);
      if (!a0) throw { code: "NOT_FOUND", message: "自动化不存在" };
      if (["issuing", "manual_wait", "deploying"].includes(a0.state)) throw { code: "CERT_AUTO_BUSY", message: "该证书自动化正在执行，请等待当前任务结束" };
      if (args!.enabled && a0.state === "deploy_interrupted") throw { code: "CERT_AUTO_INTERRUPTED", message: "请先核对目标端并手动重试部署，完成后再启用自动续签" };
      const next = { ...a0, enabled: args!.enabled as boolean, updatedAt: Math.max(Date.now(), a0.updatedAt + 1) };
      if (!next.enabled && ["waiting", "deploy_waiting"].includes(next.state)) {
        next.state = next.state === "deploy_waiting" ? "deploy_error" : "idle";
        next.lastError = next.state === "deploy_error" ? "已取消等待；已签发证书仍可手动重试部署" : "";
      }
      next.nextRenewAt = next.enabled
        ? next.state === "ok" && next.expiresAt ? Math.max(Date.now(), next.expiresAt - next.renewDaysAhead * 86400_000) : Date.now()
        : 0;
      certAutos.set(a0.id, next);
      return next as T;
    }
    case "certauto_retry_deploy":
    case "certdeploy_probe_ssh":
    case "certauto_issue": {
      throw { code: "DESKTOP_ONLY", message: "证书签发需要在桌面应用中执行；网页预览不会申请或部署真实证书。" };
    }
    case "certmonitor_list":
      return structuredClone([...certMonitors.values()].sort((a, b) => b.createdAt - a.createdAt)) as T;
    case "certmonitor_add": {
      const input = args!.m as CertMonitor;
      if (input.id) throw { code: "BAD_MONITOR_ID", message: "新增监控不能覆盖已有记录" };
      const target = monitorEndpoint(input.host, input.port);
      if ([...certMonitors.values()].some(m => m.host === target.host && m.port === target.port)) throw { code: "MONITOR_EXISTS", message: "该地址和端口已在监控列表中" };
      const m: CertMonitor = { ...input, ...target, id: `mon-${crypto.randomUUID()}`, state: "idle", issuer: "", lastError: "", notificationError: "", expiresAt: null, lastChecked: null, createdAt: Date.now(), updatedAt: Date.now() };
      certMonitors.set(m.id, m);
      return structuredClone(m) as T;
    }
    case "certmonitor_delete":
      if (!certMonitors.delete(args!.id as string)) throw { code: "NOT_FOUND", message: "监控不存在" };
      return true as T;
    case "certmonitor_check":
      throw { code: "DESKTOP_ONLY", message: "真实 TLS 证书检查需要在桌面应用中执行；网页预览不会连接目标网站。" };
    case "certmonitor_notification_get":
      return { ...monitorNotifications } as T;
    case "certmonitor_notification_save": {
      const input = args!.settings as typeof monitorNotifications;
      if (!["none", "generic", "dingtalk", "wecom", "feishu"].includes(input.kind)) throw { code: "BAD_NOTIFY_KIND", message: "请选择支持的通知方式" };
      const url = input.url.trim();
      if (input.kind !== "none") {
        try {
          const parsed = new URL(url);
          if (!["http:", "https:"].includes(parsed.protocol) || parsed.username || parsed.password || parsed.hash) throw Error();
        } catch { throw { code: "BAD_NOTIFY_URL", message: "请填写完整的 HTTP/HTTPS Webhook 地址，不要包含用户名或密码" }; }
      }
      monitorNotifications = { kind: input.kind, url };
      return { ...monitorNotifications } as T;
    }
    case "cert_export_pfx":
    case "cert_export_der":
    case "cert_export_jks":
    case "cert_export_pem":
      throw { code: "DESKTOP_ONLY", message: "证书导出需要在桌面应用中执行；网页预览不会写入真实证书文件。" };
    case "install_update":
      throw { code: "DESKTOP_ONLY", message: "浏览器无法安装桌面更新，请在桌面端执行" };
    case "open_update_dir":
      return true as T;
    case "quit_app":
      throw { code: "DESKTOP_ONLY", message: "浏览器无法退出 NiceEnv 桌面进程" };
    case "check_manifest_update":
      return false as T;
    default:
      throw new Error(`mock: 未实现的命令 ${cmd}`);
  }
}
