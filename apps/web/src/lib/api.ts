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
  CertRecord,
  ProxyProfile,
  ProxyGroupView,
  ProxyStatusInfo,
  AppSettings,
  CreateSiteInput,
  VersionCatalog,
  PathEnvStatus,
  UpdateCheckResult,
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
export const applyHosts = (entries: HostsEntry[]) =>
  safe(invoke<boolean>("apply_hosts", { entries }));
export const listCerts = () => safe(invoke<CertRecord[]>("list_certs"));
export const issueCert = (domain: string, sans: string[] = []) =>
  safe(invoke<CertRecord>("issue_cert", { domain, sans }));
export const trustCa = () => safe(invoke<boolean>("trust_ca"));
/** 按当前站点重建 hosts（保留用户手动条目） */
export const rebuildHosts = () => safe(invoke<boolean>("rebuild_hosts"));
/** 补齐缺失/过期的站点证书，返回重新签发的域名 */
export const reissueSiteCerts = () => safe(invoke<string[]>("reissue_site_certs"));

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
}
export const listBackups = () => safe(invoke<BackupFile[]>("list_backups"));
export const restoreBackup = (name: string) => safe(invoke<string>("restore_backup", { name }));
export const getSystemStats = () => safe(invoke<SystemStats>("get_system_stats"));

/* 打开外部 */
export const openInBrowser = (url: string) =>
  safe(invoke<boolean>("open_in_browser", { url }));
export const openInFolder = (path: string) =>
  safe(invoke<boolean>("open_in_folder", { path }));
export const openTerminal = (cwd: string) =>
  safe(invoke<boolean>("open_terminal", { cwd }));

/* 数据库 */
export const dbList = () => safe(invoke<DatabaseInfo[]>("db_list"));
export const dbCreate = (name: string) => safe(invoke<boolean>("db_create", { name }));
export const dbDrop = (name: string) => safe(invoke<boolean>("db_drop", { name }));
export const dbUsers = () => safe(invoke<DbUserInfo[]>("db_users"));
export const dbCreateUser = (username: string, password: string, database: string) =>
  safe(invoke<boolean>("db_create_user", { username, password, database }));
export const dbResetRootPassword = (newPassword: string) =>
  safe(invoke<boolean>("db_reset_root_password", { newPassword }));
export const dbRootPassword = () => safe(invoke<string>("db_root_password"));

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
export const dbBackupDump = (databases: string[], outName?: string) =>
  safe(invoke<string>("db_backup_dump", { databases, outName: outName ?? null }));
export const dbBackupRestore = (path: string, safetyBackup = true) =>
  safe(invoke<DbRestoreResult>("db_backup_restore", { path, safetyBackup }));
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
export const configSave = (kind: string, content: string, force = false) =>
  safe(invoke<ConfigValidation>("config_save", { kind, content, force }));
export const configBackups = () => safe(invoke<ConfigBackup[]>("config_backups"));
export const configRollback = (name: string) =>
  safe(invoke<boolean>("config_rollback", { name }));

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

/* 环境变量注入（PATH） */
export const pathenvStatus = () => safe(invoke<PathEnvStatus>("pathenv_status"));
export const pathenvSetEnabled = (enabled: boolean) =>
  safe(invoke<PathEnvStatus>("pathenv_set_enabled", { enabled }));
export const pathenvSetSelected = (ids: string[]) =>
  safe(invoke<PathEnvStatus>("pathenv_set_selected", { ids }));
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
export const checkUpdates = () => safe(invoke<UpdateCheckResult>("check_updates"));

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
  missingPackages: string[];
}
export const importConfig = (path: string) => safe(invoke<ImportReport>("import_config", { path }));
/** 拖拽导入：WebView 拿不到文件真实路径，读文本交给后端解析 */
export const importConfigText = (json: string) =>
  safe(invoke<ImportReport>("import_config_text", { json }));
