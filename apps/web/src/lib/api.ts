import type {
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
  CertReport,
  ImportedCert,
  EnvFileView,
  DiagnosticsBundle,
  HealthReport,
  BulkReport,
  BulkSelectionSummary,
  SiteBulkReport,
  ToolMirrorStatus,
  CertRecord,
  ProxyProfile,
  ProxyGroupView,
  ProxyStatusInfo,
  AppSettings,
  CreateSiteInput,
  VersionCatalog,
  PathEnvStatus,
  UpdateCheckResult,
  CertAutomation,
  CertMonitor,
  DownloadUpdateResult,
} from "@nsb/schema";
import { invoke, safe } from "./backend";

/* 服务 */
export const listServiceStatus = () =>
  safe(invoke<ServiceStatus[]>("list_service_status"));
export const startService = (id: string) =>
  safe(invoke<boolean>("start_service", { id }));
export const stopService = (id: string) =>
  safe(invoke<boolean>("stop_service", { id }));
export const restartService = (id: string) =>
  safe(invoke<boolean>("restart_service", { id }));

/* 服务栈（用户自定义的一整套服务 + 一键启动） */
export const listStacks = () => safe(invoke<Stack[]>("list_stacks"));
export const saveStack = (input: StackInput) => safe(invoke<Stack>("save_stack", { input }));
export const duplicateStack = (id: string, name?: string) =>
  safe(invoke<Stack>("duplicate_stack", { id, name }));
export const deleteStack = (id: string) => safe(invoke<boolean>("delete_stack", { id }));
/** 一键启动整栈；逐项结果回报，单项失败不阻断其它项 */
export const startStack = (id: string) =>
  safe(invoke<StackStartReport>("start_stack", { id }));
export const stopStack = (id: string) =>
  safe(invoke<StackStartReport>("stop_stack", { id }));

/* 套件 */
export const listPackages = () => safe(invoke<PackageView[]>("list_packages"));
export const installPackage = (id: string) =>
  safe(invoke<boolean>("install_package", { id }));
export const uninstallPackage = (id: string) =>
  safe(invoke<boolean>("uninstall_package", { id }));
export const cancelDownload = (taskId: string) =>
  safe(invoke<boolean>("cancel_download", { taskId }));
export const setActiveVersion = (id: string, version: string) =>
  safe(invoke<boolean>("set_active_version", { id, version }));

/* 远程版本目录：从上游枚举完整版本历史（带缓存） */
export const versionCatalog = (id: string, force = false) =>
  safe(invoke<VersionCatalog>("version_catalog", { id, force }));
export const versionCatalogs = (force = false) =>
  safe(invoke<VersionCatalog[]>("version_catalogs", { force }));

/* 站点 */
export const listSites = () => safe(invoke<Site[]>("list_sites"));
export const createSite = (input: CreateSiteInput) =>
  safe(invoke<Site>("create_site", { input }));
export const updateSite = (site: Partial<Site> & { id: string }) =>
  safe(invoke<Site>("update_site", { site }));
export const deleteSite = (id: string, opts: { hosts?: boolean; certs?: boolean }) =>
  safe(invoke<boolean>("delete_site", { id, ...opts }));
export const startSite = (id: string) => safe(invoke<boolean>("start_site", { id }));
export const stopSite = (id: string) => safe(invoke<boolean>("stop_site", { id }));

/* hosts / 证书 */
export const readHosts = () => safe(invoke<HostsEntry[]>("read_hosts"));
export const applyHosts = (entries: HostsEntry[], expectedEntries?: HostsEntry[]) =>
  safe(invoke<boolean>("apply_hosts", { entries, expectedEntries }));
export const listCerts = () => safe(invoke<CertRecord[]>("list_certs"));
export const issueCert = (domain: string, sans: string[] = []) =>
  safe(invoke<CertRecord>("issue_cert", { domain, sans }));
export const trustCa = () => safe(invoke<boolean>("trust_ca"));
export const deleteLocalCert = (id: string) => safe(invoke<boolean>("delete_local_cert", { id }));
/** 按当前站点重建 hosts（保留用户手动条目） */
export const rebuildHosts = () => safe(invoke<boolean>("rebuild_hosts"));
/** 补齐缺失/过期的站点证书，返回重新签发的域名 */
export const reissueSiteCerts = () => safe(invoke<string[]>("reissue_site_certs"));

/* 证书自动化（ACME 签发 / 定时续签 / 多平台部署） */
export const certAutoList = () => safe(invoke<CertAutomation[]>("certauto_list"));
export const certAutoSave = (a: CertAutomation) =>
  safe(invoke<CertAutomation>("certauto_save", { a }));
export const certAutoDelete = (id: string) =>
  safe(invoke<boolean>("certauto_delete", { id }));
export const certAutoSetEnabled = (id: string, enabled: boolean) =>
  safe(invoke<CertAutomation>("certauto_set_enabled", { id, enabled }));
/** 立即签发/续签：同步跑完整 ACME 流程（约 1–2 分钟），前端要提示等待 */
export const certAutoIssue = (id: string) =>
  safe(invoke<CertAutomation>("certauto_issue", { id }));


/* 网站证书监控 */
export const certMonitorList = () => safe(invoke<CertMonitor[]>("certmonitor_list"));
export const certMonitorAdd = (m: CertMonitor) =>
  safe(invoke<CertMonitor>("certmonitor_add", { m }));
export const certMonitorDelete = (id: string) =>
  safe(invoke<boolean>("certmonitor_delete", { id }));
/** 同步做一次 TLS 握手刷新到期时间 */
export const certMonitorCheck = (id: string) =>
  safe(invoke<CertMonitor>("certmonitor_check", { id }));
/** 导出本机证书为 PFX (PKCS#12)，返回保存路径 */
export const certExportPfx = (certId: string, password: string, outPath: string) =>
  safe(invoke<string>("cert_export_pfx", { certId, password, outPath }));
/** 导出 DER（二进制 X.509） */
export const certExportDer = (certId: string, outPath: string) =>
  safe(invoke<string>("cert_export_der", { certId, outPath }));
/** 导出 JKS（Java Keystore；密码至少 6 位） */
export const certExportJks = (certId: string, password: string, outPath: string) =>
  safe(invoke<string>("cert_export_jks", { certId, password, outPath }));
/** 导出 PEM 打包（证书链+私钥 单文件） */
export const certExportPem = (certId: string, outPath: string) =>
  safe(invoke<string>("cert_export_pem", { certId, outPath }));
/** 从文件夹批量导入证书（certd 输出目录 / 任意一堆 crt+key） */
export interface ImportedCertSummary {
  id: string;
  usable: boolean;
  problem?: string;
  usedBySites: string[];
  certPath: string;
  keyPath: string;
  subject: string;
  sans: string[];
  notBefore: number;
  notAfter: number;
  daysLeft: number;
}
export interface DirImportResult {
  imported: ImportedCertSummary[];
  skipped: string[];
}
export const certImportDir = (dir: string) =>
  safe(invoke<DirImportResult>("cert_import_dir", { dir }));

/* 日志 / 诊断 / 统计 */
export const tailLogs = (id: string, lines = 200) =>
  safe(invoke<LogLine[]>("tail_logs", { id, lines }));
export const diagnosePort = (port: number) =>
  safe(invoke<PortDiagnosis>("diagnose_port", { port }));
/** 全量端口体检：本应用所有待绑定端口 vs 实际占用者 */
export const scanPorts = () => safe(invoke<PortScanEntry[]>("scan_ports"));
/** 端口区间扫描（from == to 即单端口）：返回占用者与归属 */
export const scanPortRange = (from: number, to: number) =>
  safe(invoke<PortRangeScan>("scan_port_range", { from, to }));
/** 结束占用某端口的进程：本应用服务走优雅停止，外部进程直接结束 */
export const closePort = (port: number) =>
  safe(invoke<ClosePortOutcome>("close_port", { port }));
export const killPid = (pid: number) => safe(invoke<boolean>("kill_pid", { pid }));

/* 备份（配置变更前自动生成的 .bak） */
export interface BackupFile {
  name: string;
  path: string;
  sizeBytes: number;
  modifiedAt: number;
  targetPath: string | null;
  restorable: boolean;
  reason: string | null;
}
export interface BackupPreview {
  name: string;
  targetPath: string;
  targetRelative: string;
  currentExists: boolean;
  revision: string;
}
export interface ConfigResetPreview {
  kind: string;
  label: string;
  path: string;
  content: string;
  language: string;
  currentExists: boolean;
  changed: boolean;
  revision: string;
  usedByService: string | null;
}
export const listBackups = () => safe(invoke<BackupFile[]>("list_backups"));
export const previewBackup = (name: string) => safe(invoke<BackupPreview>("preview_backup", { name }));
export const restoreBackup = (name: string, revision: string) => safe(invoke<string>("restore_backup", { name, revision }));
export const configResetPreview = (kind: string) => safe(invoke<ConfigResetPreview>("config_reset_preview", { kind }));
export const configReset = (kind: string, revision: string) => safe(invoke<ConfigResetPreview>("config_reset", { kind, revision }));
export const getSystemStats = () => safe(invoke<SystemStats>("get_system_stats"));

/* 打开外部 */
export const openInBrowser = (url: string) =>
  safe(invoke<boolean>("open_in_browser", { url }));
export const openInFolder = (path: string) =>
  safe(invoke<boolean>("open_in_folder", { path }));
export const openTerminal = (cwd: string) =>
  safe(invoke<boolean>("open_terminal", { cwd }));

/* 数据库 */
export const dbList = (version?: string) => safe(invoke<DatabaseInfo[]>("db_list", { version }));
export const dbCreate = (name: string, version?: string) => safe(invoke<boolean>("db_create", { name, version }));
export const dbDrop = (name: string, version?: string) => safe(invoke<boolean>("db_drop", { name, version }));
export const dbUsers = (version?: string) => safe(invoke<DbUserInfo[]>("db_users", { version }));
export const dbCreateUser = (username: string, password: string, database: string, version?: string) =>
  safe(invoke<boolean>("db_create_user", { username, password, database, version }));
export const dbResetRootPassword = (newPassword: string, version?: string, useExisting = false) =>
  safe(invoke<boolean>("db_reset_root_password", { newPassword, version, useExisting }));
export const dbRootPassword = (version?: string) => safe(invoke<string>("db_root_password", { version }));
export interface RedisStats {
  reachable: boolean;
  port: number;
  usedMemoryHuman?: string;
  keys?: number;
  uptimeDays?: number;
  connectedClients?: number;
}
export interface RedisConnectionInfo { version: string; username: string; hasPassword: boolean }
export const redisConnection = (version: string) => safe(invoke<RedisConnectionInfo>("redis_connection", { version }));
export const redisSaveConnection = (version: string, credentials: { username: string; password: string }) =>
  safe(invoke<RedisStats>("redis_save_connection", { version, credentials }));
export const redisStats = () => safe(invoke<RedisStats>("redis_stats"));

/* PHP 扩展 */
export const phpExtensions = (version: string) =>
  safe(invoke<PhpExtensionView>("php_extensions", { version }));
export const setPhpExtension = (version: string, name: string, enabled: boolean) =>
  safe(invoke<PhpExtensionChange>("set_php_extension", { version, name, enabled }));
export const setPhpIniToggle = (version: string, key: string, value: boolean) =>
  safe(invoke<boolean>("set_php_ini_toggle", { version, key, value }));

/* Xdebug */
export const xdebugStatus = (version: string) =>
  safe(invoke<XdebugStatus>("xdebug_status", { version }));
export const xdebugSetup = (input: {
  version: string;
  mode?: string;
  clientPort?: number;
  dllPath?: string | null;
}) => safe(invoke<XdebugSetupResult>("xdebug_setup", { input }));
export const xdebugToggle = (version: string, enabled: boolean, mode: string, port: number) =>
  safe(invoke<string[]>("xdebug_toggle", { version, enabled, mode, port }));

/* 数据库备份 / 还原 */
export const dbBackupList = () => safe(invoke<DbBackupFile[]>("db_backup_list"));
export const dbBackupDir = () => safe(invoke<string>("db_backup_dir"));
export const dbBackupDump = (databases: string[], outName?: string, version?: string) =>
  safe(invoke<string>("db_backup_dump", { databases, outName: outName ?? null, version }));
export const dbBackupRestore = (path: string, safetyBackup = true, version?: string, database?: string) =>
  safe(invoke<DbRestoreResult>("db_backup_restore", { path, safetyBackup, version, database }));
export const dbBackupDelete = (path: string) =>
  safe(invoke<boolean>("db_backup_delete", { path }));

/* 服务看门狗 */
export const watchdogStatus = () => safe(invoke<WatchdogStatus>("watchdog_status"));
export const watchdogSetEnabled = (enabled: boolean) =>
  safe(invoke<boolean>("watchdog_set_enabled", { enabled }));
export const watchdogReset = (id: string) => safe(invoke<boolean>("watchdog_reset", { id }));

/* 项目扫描 */
export const scanProjects = (root: string) =>
  safe(invoke<ScannedProject[]>("scan_projects", { root }));

/* 配置文件编辑 */
export const configList = () => safe(invoke<ConfigFileInfo[]>("config_list"));
export const configRead = (kind: string) => safe(invoke<string>("config_read", { kind }));
export const configValidate = (kind: string, content: string) =>
  safe(invoke<ConfigValidation>("config_validate", { kind, content }));
export const configSave = (kind: string, content: string, force = false, expectedContent?: string) =>
  safe(invoke<ConfigValidation>("config_save", { kind, content, force, expectedContent }));
export const configBackups = (kind?: string) => safe(invoke<ConfigBackup[]>("config_backups", { kind }));
export const configRollback = (name: string, kind?: string, expectedContent?: string) =>
  safe(invoke<boolean>("config_rollback", { name, kind, expectedContent }));

/* 证书体检 */
export const certHealth = () => safe(invoke<CertReport>("cert_health"));
export const certImport = (certPath: string, keyPath: string) =>
  safe(invoke<ImportedCert>("cert_import", { certPath, keyPath }));
export const certImportedList = () => safe(invoke<ImportedCert[]>("cert_imported_list"));
export const certImportedDelete = (certPath: string) =>
  safe(invoke<boolean>("cert_imported_delete", { certPath }));

/* 站点 .env */
export const envRead = (siteId: string) => safe(invoke<EnvFileView>("env_read", { siteId }));
export const envSave = (siteId: string, changes: [string, string][]) =>
  safe(invoke<boolean>("env_save", { siteId, changes }));
export const envApplyDb = (siteId: string) =>
  safe(invoke<string[]>("env_apply_db", { siteId }));

/* 诊断包 */
export const diagnosticsBuild = () => safe(invoke<DiagnosticsBundle>("diagnostics_build"));
export const diagnosticsSave = () => safe(invoke<string>("diagnostics_save"));

/* 环境体检 */
export const healthCheck = () => safe(invoke<HealthReport>("health_check"));

/* 批量服务操作 */
export const bulkStart = (ids: string[]) => safe(invoke<BulkReport>("bulk_start", { ids }));
export const bulkStop = (ids: string[]) => safe(invoke<BulkReport>("bulk_stop", { ids }));
export const bulkRestart = (ids: string[]) => safe(invoke<BulkReport>("bulk_restart", { ids }));
export const bulkSummary = (ids: string[]) =>
  safe(invoke<BulkSelectionSummary>("bulk_summary", { ids }));

/* 批量站点操作 */
export const sitesStartMany = (ids: string[]) =>
  safe(invoke<SiteBulkReport>("sites_start_many", { ids }));
export const sitesStopMany = (ids: string[]) =>
  safe(invoke<SiteBulkReport>("sites_stop_many", { ids }));

/* 日志导出 */
export const logExport = (serviceId: string, content: string, suggestedName?: string) =>
  safe(invoke<string>("log_export", { serviceId, content, suggestedName: suggestedName ?? null }));

/* 工具链镜像源 */
export const toolMirrors = () => safe(invoke<ToolMirrorStatus[]>("tool_mirrors"));
export const toolMirrorSet = (manager: string, url: string) =>
  safe(invoke<boolean>("tool_mirror_set", { manager, url }));
export const toolMirrorReset = (manager: string) =>
  safe(invoke<boolean>("tool_mirror_reset", { manager }));

/* 代理（Clash/mihomo） */
export const proxyStatus = () => safe(invoke<ProxyStatusInfo>("proxy_status"));
export const proxyStart = () => safe(invoke<boolean>("proxy_start"));
export const proxyStop = () => safe(invoke<boolean>("proxy_stop"));
export const proxySetSystem = (enabled: boolean) =>
  safe(invoke<boolean>("proxy_set_system", { enabled }));
export const proxySetMode = (mode: "rule" | "global" | "direct") =>
  safe(invoke<boolean>("proxy_set_mode", { mode }));
export const proxyProfiles = () => safe(invoke<ProxyProfile[]>("proxy_profiles"));
export const proxyImport = (name: string, url: string) =>
  safe(invoke<ProxyProfile>("proxy_import", { name, url }));
export const proxyActivateProfile = (id: string) =>
  safe(invoke<boolean>("proxy_activate_profile", { id }));
export const proxyDeleteProfile = (id: string) =>
  safe(invoke<boolean>("proxy_delete_profile", { id }));
export const proxyNodes = () => safe(invoke<ProxyGroupView[]>("proxy_nodes"));
export const proxySelectNode = (group: string, node: string) =>
  safe(invoke<boolean>("proxy_select_node", { group, node }));
export const proxyDelayTest = (node: string) =>
  safe(invoke<number>("proxy_delay_test", { node }));

/** mihomo 实时连接（GET /connections 直通） */
export interface ProxyConnection {
  id: string;
  upload: number;
  download: number;
  start: string;
  chains: string[];
  metadata?: {
    network?: string;
    type?: string;
    host?: string;
    destinationIP?: string;
    destinationPort?: string;
    sourceIP?: string;
  };
}
export interface ProxyConnectionsInfo {
  downloadTotal?: number;
  uploadTotal?: number;
  connections?: ProxyConnection[] | null;
}
export const proxyConnections = () =>
  safe(invoke<ProxyConnectionsInfo>("proxy_connections"));
/** 重新拉取订阅并覆盖原文件（激活中的订阅会自动重写主配置并重启内核） */
export const proxyUpdateProfile = (id: string) =>
  safe(invoke<boolean>("proxy_update_profile", { id }));

/* ================= 工具箱扩展（计划任务 / 快速隧道 / Ollama / Adminer） ================= */

export interface CronJob {
  id: string;
  name: string;
  command: string;
  intervalMin: number;
  enabled: boolean;
  createdAt: number;
  lastRunAt?: number | null;
  lastExit?: string | null;
  lastOutput?: string | null;
}
export const cronJobs = () => safe(invoke<CronJob[]>("cron_jobs"));
export const cronSave = (job: CronJob) =>
  safe(invoke<boolean>("cron_save", { job }));
export const cronDelete = (id: string) =>
  safe(invoke<boolean>("cron_delete", { id }));
export const cronSetEnabled = (id: string, enabled: boolean) =>
  safe(invoke<boolean>("cron_set_enabled", { id, enabled }));
export const cronRunNow = (id: string) =>
  safe(invoke<CronJob>("cron_run_now", { id }));
export const cronStop = (id: string) =>
  safe(invoke<boolean>("cron_stop", { id }));

export interface TunnelInfo {
  id: string;
  port: number;
  url?: string | null;
  startedAt: number;
  alive: boolean;
  state: "starting" | "connected" | "reconnecting" | "failed" | "stopped";
  target: string;
  siteId?: string | null;
  error?: string | null;
  logs: string[];
  localReachable?: boolean | null;
}
export const tunnelStart = (port: number) =>
  safe(invoke<TunnelInfo>("tunnel_start", { port }));
export const tunnelList = () => safe(invoke<TunnelInfo[]>("tunnel_list"));
export const tunnelStop = (id: string) =>
  safe(invoke<boolean>("tunnel_stop", { id }));
export const tunnelStartSite = (id: string) => safe(invoke<TunnelInfo>("tunnel_start_site", { id }));
export const tunnelRemove = (id: string) => safe(invoke<boolean>("tunnel_remove", { id }));

export interface OllamaModelRow {
  name: string;
  digest: string;
  size: number;
  modified: string;
  parameters: string;
  quantization: string;
}
export interface OllamaPullStatus {
  id: string;
  name: string;
  state: "pulling" | "cancelling" | "succeeded" | "failed" | "cancelled";
  phase: string;
  digest?: string | null;
  completed?: number | null;
  total?: number | null;
  error?: string | null;
  startedAt: number;
  endedAt?: number | null;
}
export const ollamaModels = () =>
  safe(invoke<OllamaModelRow[]>("ollama_models"));
export const ollamaDelete = (name: string) =>
  safe(invoke<boolean>("ollama_delete", { name }));
export const ollamaPull = (name: string) =>
  safe(invoke<OllamaPullStatus>("ollama_pull", { name }));
export const ollamaPullStatus = () => safe(invoke<OllamaPullStatus | null>("ollama_pull_status"));
export const ollamaCancelPull = (id: string) => safe(invoke<boolean>("ollama_cancel_pull", { id }));

export interface AdminerStatus { port: number; file: string; url: string; phpVersion: string; adminerVersion: string }
export const adminerStart = () => safe(invoke<AdminerStatus>("adminer_start"));
export const adminerStatus = () => safe(invoke<AdminerStatus | null>("adminer_status"));
export const adminerStop = () => safe(invoke<boolean>("adminer_stop"));

/* 环境变量注入（PATH） */
export const pathenvStatus = () => safe(invoke<PathEnvStatus>("pathenv_status"));
export const pathenvSetEnabled = (enabled: boolean) =>
  safe(invoke<PathEnvStatus>("pathenv_set_enabled", { enabled }));
export const pathenvSetSelected = (ids: string[]) =>
  safe(invoke<PathEnvStatus>("pathenv_set_selected", { ids }));
export const pathenvSetVersion = (id: string, version: string, selected: boolean) =>
  safe(invoke<PathEnvStatus>("pathenv_set_version", { id, version, selected }));
export const pathenvReapply = () => safe(invoke<PathEnvStatus>("pathenv_reapply"));

/* 设置 */
export const getSettings = () => safe(invoke<AppSettings>("get_settings"));
export const setSetting = (key: string, value: unknown) =>
  safe(invoke<boolean>("set_setting", { key, value }));
/** 覆盖单个端口；port = null 表示恢复档位默认 */
export const setPortOverride = (key: string, port: number | null) =>
  safe(invoke<boolean>("set_port_override", { key, port }));
export const getAppVersion = () => safe(invoke<string>("get_app_version"));
export const getDataDir = () => safe(invoke<string>("get_data_dir"));
export interface DataDirMigration {
  path: string;
  files: number;
  bytes: number;
}
export const migrateDataDir = (path: string) =>
  safe(invoke<DataDirMigration>("migrate_data_dir", { path }));
export const restartApp = () => safe(invoke<boolean>("restart_app"));
export const checkUpdates = () => safe(invoke<UpdateCheckResult>("check_updates"));

/** 拉取远端套件清单（设置项 manifestUrl）并落盘为快照；下次启动生效 */
export const refreshRemoteManifest = (url?: string) =>
  safe(
    invoke<{ revision: number; packages: number; path: string; takesEffect: "restart" }>(
      "refresh_remote_manifest",
      { url: url ?? null }
    )
  );
/** 删除远端清单快照，回退到内置清单；下次启动生效 */
export const resetRemoteManifest = () => safe(invoke<boolean>("reset_remote_manifest"));

/** 在线下载更新包；进度通过 `update://progress` 事件回报 */
export const downloadUpdate = (url: string, version: string, assetName?: string | null) =>
  safe(invoke<DownloadUpdateResult>("download_update", { url, version, assetName: assetName ?? null }));
/** 就地安装已下载的更新包（Windows 拉起安装器 / macOS 挂载 dmg），随后应用退出 */
export const installUpdate = (path: string) => safe(invoke<boolean>("install_update", { path }));
/** 打开更新包下载目录 */
export const openUpdateDir = () => safe(invoke<boolean>("open_update_dir"));

export const quitApp = () => safe(invoke<boolean>("quit_app"));

/* 配置导入/导出 */
export const exportConfig = (path: string) => safe(invoke<number>("export_config", { path }));
export interface ImportReport {
  sites: number;
  skippedSites: number;
  settings: number;
  proxyProfiles: number;
  stacks: number;
  certAutomations: number;
  certMonitors: number;
  missingPackages: string[];
}
export const importConfig = (path: string) => safe(invoke<ImportReport>("import_config", { path }));
/** 拖拽导入：WebView 拿不到文件真实路径，读文本交给后端解析 */
export const importConfigText = (json: string) =>
  safe(invoke<ImportReport>("import_config_text", { json }));

export const exportLog = (id: string, dest: string) =>
  safe(invoke<number>("export_log", { id, dest }));

export interface ConfigCheck {
  name: string;
  ok: boolean;
  status: "ok" | "fail" | "skipped";
  detail: string;
}
export const validateConfigs = () => safe(invoke<ConfigCheck[]>("validate_configs"));

/* hosts 文件导入/导出用的纯文本读写 */
export const readTextFile = (path: string) => safe(invoke<string>("read_text_file", { path }));
export const writeTextFile = (path: string, content: string) =>
  safe(invoke<boolean>("write_text_file", { path, content }));

/* ===== 从其它环境（FlyEnv/phpStudy/ServBay/XAMPP）迁移 MySQL ===== */
export interface SourceDb {
  name: string;
  sizeKb?: number;
}
export interface ImportReport {
  imported: string[];
  failed: [string, string][];
}
export const migrateListSource = (host: string, port: number, user: string, password: string, version?: string) =>
  safe(invoke<SourceDb[]>("migrate_list_source", { host, port, user, password, version }));
export const migrateImport = (
  host: string, port: number, user: string, password: string, databases: string[], version?: string
) => safe(invoke<ImportReport>("migrate_import", { host, port, user, password, databases, version }));

/* ===== DNS 一键接管（本地域名解析配套） ===== */
export const dnsInterfaces = () => safe(invoke<string[]>("dns_interfaces"));
export interface DnsConfiguration { interfaceId: string; automatic: boolean; servers: string[] }
export interface DnsInterfaceStatus { current: DnsConfiguration; backup: DnsConfiguration | null; local: boolean }
export const dnsStatusOf = (name: string) => safe(invoke<DnsInterfaceStatus>("dns_status_of", { name }));
export const dnsTakeover = (name: string) => safe(invoke<boolean>("dns_takeover", { name }));
export const dnsRestore = (name: string, automatic = false) => safe(invoke<boolean>("dns_restore", { name, automatic }));
