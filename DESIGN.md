# NiceEnv — 本地开发环境集成管理器

Windows + macOS 一站式本地开发环境管理器（对标 ServBay / FlyEnv / phpStudy，交互与视觉更现代）。
Tauri 2 + Rust 后端能力 + Next.js (App Router, 静态导出) 前端。

## 1. 最终目录结构

```
nice-servbay/
├── package.json                  # pnpm workspace 根脚本
├── pnpm-workspace.yaml
├── Cargo.toml                    # Cargo workspace 根
├── DESIGN.md / README.md
├── manifest/                     # 套件清单（打包进二进制 + 可远程更新）
│   └── packages.win.json
│   └── packages.mac.json
├── apps/
│   ├── desktop/                  # Tauri 2 壳
│   │   ├── package.json          # tauri CLI scripts
│   │   └── src-tauri/
│   │       ├── Cargo.toml        # bin: niceservbay
│   │       ├── tauri.conf.json   # NSIS + dmg 配置
│   │       ├── capabilities/     # 权限声明
│   │       ├── icons/
│   │       └── src/
│   │           ├── main.rs
│   │           ├── lib.rs        # command 注册、窗口行为
│   │           ├── tray.rs       # 状态感知的托盘菜单（栈/服务/站点/导航 + 运行数角标）
│   │           └── smoke.rs      # --smoke-test 无头验收管线
│   └── web/                      # Next.js App Router（output: 'export'）
│       ├── next.config.ts
│       └── src/
│           ├── app/              # /  sites/  packages/  stacks/  databases/  tls/
│           │                     # proxy/  tools/  logs/  settings/
│           ├── components/
│           │   ├── ui/           # shadcn 组件（手写迁移）
│           │   ├── layout/       # AppShell / Sidebar / TopBar / CommandPalette
│           │   └── ...           # 业务组件
│           └── lib/              # backend 适配层(真实 invoke + 浏览器 mock)、hooks、i18n
│                                 # log-highlight.ts 日志解析与配色/__tests__ 纯逻辑单测
├── packages/
│   └── schema/                   # Zod schema（TS 侧唯一事实，Rust serde 对齐）
└── crates/
    ├── core/                     # 平台无关核心
    │   └── src/
    │       ├── error.rs          # AppError: code/message/hint(人话+建议)
    │       ├── model.rs          # 与 packages/schema 对齐的 serde 模型
    │       ├── paths.rs          # {appLocalData}/ 目录布局
    │       ├── store.rs          # SQLite(rusqlite): sites/packages/certs/settings
    │       ├── packages/         # 清单解析 + 安装管线(下载/校验/解压/配置/卸载)
    │       ├── download.rs       # 断点续传 + sha256 + 取消 + 进度事件
    │       ├── services/         # ServiceManager: 状态机/进程树/健康检查/日志
    │       ├── configgen/        # nginx.conf / php.ini / my.ini / redis.conf / mihomo yaml
    │       ├── sites.rs          # 站点 CRUD → vhost + hosts + 证书 + 重载
    │       ├── hosts.rs          # 标记块合并写 hosts
    │       ├── tls.rs            # rcgen 根 CA + 站点证书签发/续签
    │       ├── ports.rs          # 端口诊断/区间扫描/结束占用者(自有服务优雅停,外部进程才 kill)
    │       ├── stacks.rs         # 服务栈:内置预设 + 自定义组合,一键按序启动/逆序停止
    │       ├── stats.rs          # CPU/RAM/磁盘 + 每服务内存
    │       ├── dbadmin.rs        # MySQL 建库建号/改密/连接串
    │       └── proxy/            # mihomo(Clash) 生命周期 + REST API + 系统代理
    └── platform/                 # win / mac 差异
        └── src/
            ├── job.rs            # Windows Job Object(KILL_ON_JOB_CLOSE) / unix 进程组
            ├── hosts_write.rs    # hosts 写入与权限指引
            ├── sysproxy.rs       # Windows 注册表系统代理 + wininet 刷新 / mac networksetup
            └── elevate.rs        # 提权执行/签名公证预留
```

说明：`packages/ui` 并入 `apps/web/src/components/ui`（Tailwind v4 跨包配置成本高，收益低）；
`packages/schema` 保留，TS 唯一事实来源，Rust `model.rs` 字段一一对应。

## 2. 数据结构（TS / Rust 对齐）

### TS（packages/schema，Zod）

```ts
type PackageCategory = "web-server" | "runtime" | "database" | "cache" | "tool";
type Os = "windows" | "macos";
type Arch = "x64" | "arm64";

interface PackageManifestEntry {          // 清单条目
  id: string;                             // "php" | "nginx" | "mysql" | ...
  version: string;                        // "8.3.17"
  category: PackageCategory;
  displayName: string; description: string;
  os: Os[]; arch: Arch[];
  kind: "archive" | "binary";
  url: string; mirrors?: string[];        // 主源 + 镜像
  sha256: string; sizeBytes: number;
  entry: string;                          // 解压后主程序相对路径
  defaultPort?: number;
  depends?: string[];
}

type InstallState = "not-installed" | "downloading" | "downloaded" | "verifying"
                  | "extracting" | "configuring" | "installed" | "error";
interface InstalledPackage {
  id: string; version: string; category: PackageCategory;
  installPath: string; configPath: string; installedAt: number;
}

type ServiceState = "stopped" | "starting" | "running" | "stopping" | "error" | "unknown";
interface ServiceStatus {
  id: string;                             // "nginx" | "php@8.3" | "mysql@8.0" | "mihomo"
  label: string; state: ServiceState;
  pids: number[]; port?: number; version?: string;
  memoryMb?: number; uptimeSec?: number;
  lastError?: AppErrorInfo; logFile?: string;
}

type SiteKind = "php" | "static" | "reverse-proxy" | "node" | "python" | "java" | "go";
type RewritePreset = "none" | "laravel" | "thinkphp" | "wordpress" | "spa-fallback" | "next-export";
interface SiteRuntime {
  webServer: "nginx";                     // Phase3 扩展 caddy/apache
  kind: SiteKind;
  phpVersion?: string;                    // kind=php
  proxyTarget?: string;                   // kind=reverse-proxy, "127.0.0.1:8080"
  command?: string; cwd?: string;         // kind=node/python/java/go 的启动命令
}
interface SiteDbBinding { enabled: boolean; database: string; username: string; password: string }
interface Site {
  id: string; name: string;
  domains: string[];                      // ["demo.test"]
  rootDir: string;
  runtime: SiteRuntime;
  https: boolean;
  rewrite: RewritePreset;
  db: SiteDbBinding | null;
  status: "running" | "stopped" | "error" | "unconfigured";
  createdAt: number; updatedAt: number;
}

interface AppErrorInfo { code: string; message: string; hint?: string; detail?: string }

interface DownloadProgress {              // 事件 download://progress
  taskId: string; received: number; total: number;
  speedBps: number; etaSec: number; state: InstallState; error?: string;
}
interface SystemStats {
  cpuPercent: number; memUsedMb: number; memTotalMb: number;
  diskFreeGb: number; diskTotalGb: number; history: { t: number; cpu: number; mem: number }[];
}
```

### Rust（crates/core::model，serde 字段名一一对应）

```rust
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceState { Stopped, Starting, Running, Stopping, Error, Unknown }

#[derive(Serialize, Deserialize, Clone)]
pub struct ServiceStatus {
    pub id: String, pub label: String, pub state: ServiceState,
    pub pids: Vec<u32>, #[serde(skip_serializing_if="Option::is_none")] pub port: Option<u16>,
    #[serde(skip_serializing_if="Option::is_none")] pub version: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub memory_mb: Option<f64>,
    #[serde(skip_serializing_if="Option::is_none")] pub uptime_sec: Option<u64>,
    #[serde(skip_serializing_if="Option::is_none")] pub last_error: Option<AppErrorInfo>,
    #[serde(skip_serializing_if="Option::is_none")] pub log_file: Option<String>,
}
// Site / InstalledPackage / PackageManifestEntry / SystemStats 同构，serde(rename_all="camelCase")
```

命令面（apps/desktop 注册，前端一律走 invoke）：
`list_packages/install_package/uninstall_package/cancel_download/set_active_version` ·
`start_service/stop_service/restart_service/list_service_status` ·
**服务栈** `list_stacks/save_stack/duplicate_stack/delete_stack/start_stack/stop_stack` ·
`list_sites/create_site/update_site/delete_site/start_site/stop_site` ·
`read_hosts/apply_hosts/rebuild_hosts` · `issue_cert/trust_ca/list_certs/reissue_site_certs` ·
**端口** `tail_logs/diagnose_port/scan_ports/scan_port_range/close_port/kill_pid` · `get_system_stats` ·
`list_backups/restore_backup` ·
`open_in_browser/open_in_folder/open_terminal` · `db_list/db_create/db_users/db_reset_root_password` ·
`proxy_status/proxy_start/proxy_stop/proxy_set_system/proxy_profiles/proxy_import/proxy_nodes/proxy_delay_test` ·
**设置** `get_settings/set_setting/set_port_override/get_data_dir/check_updates` ·
**配置迁移** `export_config/import_config/import_config_text`（拖拽导入走 `import_config_text`：
WebView 拿不到拖入文件的真实路径，改由前端读文本交给后端复用同一条解析链）

## 3. Phase 1 任务拆分

| # | 任务 | 产物/验收 |
|---|------|----------|
| P0-1 | Monorepo 脚手架 | pnpm + cargo workspace 可构建 |
| P0-2 | 前端壳 | 侧栏/顶栏/Cmd+K/路由动效/主题（暗色优先） |
| P0-3 | schema + mock 后端 | 浏览器可开发，数据结构即最终 schema |
| P1-1 | paths + SQLite store | 首次启动建目录建库 |
| P1-2 | 下载器 | 断点续传/sha256/取消/进度事件（单测：本地 HTTP 服务） |
| P1-3 | 清单 + 安装管线 | manifest JSON → 安装到 runtimes/{id}/{ver} |
| P1-4 | 服务管理器 | Job Object 进程树、启停、健康检查(TCP+进程)、日志 tail |
| P1-5 | 配置生成 | nginx/php.ini(+扩展)/my.ini/redis.conf，改前备份 |
| P1-6 | PHP-CGI 池 | Windows 多 php-cgi -b 端口池 + nginx upstream |
| P1-7 | MySQL 初始化 | --initialize-insecure → 改密 → 建库建号 |
| P1-8 | 站点闭环 | create_site → vhost + hosts + php 绑定 + nginx reload |
| P1-9 | 端口诊断 | netstat → pid → 进程名，冲突时给"谁占了+可结束" |
| P1-10 | Dashboard 真数据 | 健康矩阵/一键 LNMP/异常卡/资源图 |
| P1-11 | 冒烟验收 | 无头 --smoke-test：装 4 件套(安全端口) → 建站 → curl phpinfo → PDO 连 MySQL → 原生 RESP 连 Redis → 清理，全程不触碰 FlyEnv |

端口默认（**标准档**，可切安全档；每个端口还能在设置里逐个覆盖）：
nginx 80/443 · apache 8080/8443 · php-cgi 9100+ · MySQL 3306 · PostgreSQL 5432 · MongoDB 27017 · Redis 6379 · mihomo 17890/19090。
默认走标准端口是为了让项目里写死的 `127.0.0.1:3306` 这类连接串开箱即用；
起冲突时启动前预检会把「端口 + 占用进程 pid」带进 AppError，前端据此提供「结束占用并重试」。

## 4. 安全红线（本机测试）

- 独立数据目录（`{LocalAppData}/NiceEnv`，冒烟测试用 `.smoke-home`），绝不读写 FlyEnv 目录
- 默认端口走标准档（80/3306/6379…），但本机已有环境占用时会**明确报错并给出占用进程**，
  绝不静默改端口；要并存可在设置切安全档（8080/23306/26379…，避开 FlyEnv/Clash Party 占用的 7890 等）
- 冒烟测试强制切到安全档并关掉「自动释放端口」，保证 **--smoke-test 全程不结束任何进程**
- 不 kill 任何非本应用拉起的进程；hosts 写失败只提示不强写；冒烟测试跳过 hosts 与系统代理
