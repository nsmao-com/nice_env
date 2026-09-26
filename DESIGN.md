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

## 5. 功能完善审查（2026-09-26，持续进行）

目标：以 ServBay 的网站、运行环境和服务管理流程为主要参考，结合 FlyEnv、
phpStudy 的日常开发场景，补齐缺漏功能、修复真实执行链路，并改善各页面的间距、
状态反馈和错误恢复。以下是实施记录，不代表整个目标已完成。

主要参考：

- [ServBay 网站管理面板](https://support.servbay.com/basic-usage/websites/website-management-panel)
- [ServBay 添加网站](https://support.servbay.com/basic-usage/websites/adding-first-website)
- [ServBay 反向代理](https://support.servbay.com/basic-usage/websites/reverse-proxy-web-website)
- [ServBay 安装软件包](https://support.servbay.com/basic-usage/services-and-packages/installing-packages)
- [ServBay 服务与软件包管理](https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management)
- [ServBay 卸载软件包](https://support.servbay.com/basic-usage/services-and-packages/uninstalling-packages)
- [ServBay PHP 配置管理](https://support.servbay.com/advanced-settings/modify-configurations/modify-php-settings)
- [ServBay 查看配置文件](https://support.servbay.com/advanced-settings/modify-configurations/view-config-files)
- [ServBay 使用 .user.ini](https://support.servbay.com/php/how-to-use-user-ini)
- [ServBay Nginx 全局配置](https://support.servbay.com/advanced-settings/modify-configurations/modify-nginx-settings)
- [Redis 配置与启动参数](https://redis.io/docs/latest/operate/oss_and_stack/management/config/)
- [MySQL 配置文件与启动参数顺序](https://dev.mysql.com/doc/refman/8.4/en/option-file-options.html)
- [FlyEnv 本地域名与 HTTPS](https://flyenv.com/guide/host)
- [phpStudy 站点创建与管理](https://old.xp.cn/phpstudy-v8/site.html)

| 范围 | 当前进展与后续验收 |
| --- | --- |
| 站点生命周期 | 已修复配置存在即显示运行、批量启动不启动依赖、停用站点保存后重新启用、域名编辑校验缺失、切换服务器残留旧配置、HTTPS 域名变更不重签证书；需继续验证真实 HTTP 请求、Apache 和跨平台行为。 |
| 站点配置 | 已修复 Apache 地址使用 Nginx 端口、HTTPS 代理重复拼接协议、通配符证书文件名不一致、Nginx 路径包含空格时配置失败；已用本机 Nginx 执行实际配置校验。 |
| 项目创建 | 已防止模板覆盖既有文件、修正 public/out 文档根、增加依赖预检；WordPress 已改为官方完整发行包下载；Laravel、ThinkPHP、Symfony、CodeIgniter 已通过所选 PHP CLI 和 Composer 安装真实项目，四个框架首页均已实际返回 HTTP 200。Next-export 已接入官方 create-next-app、pnpm 安装、类型检查与最终导出代码，源码生成和依赖安装已实际通过，最终 build/产物验收尚未执行。接入现有目录不再补写 PHP 占位入口；桌面端建站整链路和跨平台行为仍待验收。 |
| 环境变量 | 已区分 Web 根和项目根，按当前端口方案补全数据库连接，保留本机数据库凭据并从站点响应中隐藏，旧版未知密码不覆盖现有配置；新增 ThinkPHP、CodeIgniter 和 Symfony 的连接字段及对应转义规则，特殊字符密码已由四个框架各自解析器验证；需继续核对导入导出和目录变更边界。 |
| 套件与服务 | 已修复当前版本选择、停止态服务元数据、启停/切换/卸载互斥、固定版本依赖和进程回收；已用隔离 Nginx 实例验证 HTTP 200 与停止回收。下载器已补 HTTP Range/416/校验失败换源、可中断的网络等待，安装改为暂存发布和失败回滚；官方 Nginx 1.31.6 已实际下载、安装、执行版本检查并卸载。其余套件、完整桌面 IPC、跨进程安装互斥、终端 PATH、跨平台归档与配置生命周期仍需继续验收。 |
| 数据库 | 已修复建号密码直接拼 SQL 的问题，在短连接中明确字面量转义模式，并提前校验建库参数；仍待核对已有账号密码不一致、多实例连接、客户端入口、导入导出、备份恢复，不能只凭页面或 mock 成功认定可用。 |
| 配置编辑 | PHP 配置与扩展开关持久化已通过真实 FastCGI 验证；Nginx/Apache/MySQL/Redis 改为只同步托管项，保留用户参数。Nginx/Apache 已验证真实 HTTP、重建和端口变化后仍保留自定义响应头；Redis 已验证重启后的实际设置与稳定回落端口；MySQL 已用真实程序解析配置，未初始化数据库或执行完整启动。工具箱已支持按服务/版本预览并重置默认配置，统一备份列表与准确目标恢复；Nginx/Apache 重置和恢复后的原生校验通过。完整桌面 IPC、跨进程并发改写、跨平台和其他套件仍需验收。 |
| 证书、代理与工具 | 已补 DNS 接管的真实网卡解析、退出码检查、状态刷新和失败重试；代理状态读取、环境体检、总览端口/证书异常与历史日志均补齐错误反馈和恢复入口。ACME 自动签发、证书部署、隧道、计划任务、配置备份与还原的完整链路仍需继续验收。 |
| UI 与交互 | 已改善站点搜索筛选、结果为空/加载失败、操作防重复、草稿保留、固定保存区、通用弹窗左右留白和页头换行；创建向导新增实际阶段进度、持续错误提示、建库依赖/参数检查、运行类型与模板联动、可访问标签和安全随机密码。套件页已补窄屏适配、准确的版本状态，安装弹窗区分取消请求与后端确认、失败可重试、长错误换行和固定操作区；其余页面及深浅主题仍需逐页检查。 |
| 发布 | 当前完善工作尚未提交或打 tag；用户要求发布时，必须同步版本号、推送发布提交与新 tag，并核对 Release 运行状态。 |

验证边界：网页开发环境使用预览适配层，浏览器验证仅证明交互与布局；
Rust 单元验证使用临时数据目录；Nginx 先执行配置检查，第四轮额外在隔离端口短暂启动
自有实例验证静态路由并回收进程；不修改系统 hosts、系统代理或用户数据库。
未运行前端 build，也未新增测试文件。

首轮检查结果：前端 TypeScript 检查、桌面 Rust 编译检查、16 项站点验证（含真实
Nginx 配置检查）、22 项环境变量验证通过；浏览器已验证桌面与 390px 窄屏下的
搜索、无结果、草稿保留、放弃修改确认及弹窗操作区。尚未执行完整桌面端端到端验收。

第二轮检查结果：实际下载 WordPress 官方完整发行包、校验入口与安装向导、确认重试不覆盖
已有文件；14 项离线站点检查和 1 项联网 WordPress 检查通过，包含配置端口、密码转义、
8 项登录密钥和已有配置保护。模板文件改为临时文件原子发布，失败不会留下半份配置。
TypeScript 检查、桌面 Rust 编译检查及 diff 空白检查通过。浏览器在 390px 下验证了
创建中的提示和禁用状态、MySQL 缺失拦截、模拟失败后的错误提示与草稿保留；左右各 12px，
底部操作可见。浏览器错误为验收时在页面内注入的模拟错误，未改动项目 mock 源码；
尚未进行 WordPress 实际建库、HTTP 请求和桌面 IPC 的完整端到端验收。

第三轮检查结果：Laravel、ThinkPHP、Symfony、CodeIgniter 改为真实 Composer 脚手架。
在 `.verify-home/composer-projects` 的隔离环境中使用 PHP 8.4.26 和 Composer 2.10.3，
实际安装四个项目，短暂启动各自 PHP HTTP 入口，均返回 HTTP 200 和框架首页。
验证了项目改名后重试不覆盖 `.env`、四个框架原生解析器正确读取含引号/反斜杠/美元符号等
特殊字符的合成密码，以及命令超时后回收自有进程；结束后未发现遗留 PHP 进程。
Symfony 项目通过框架依赖识别，避免 Flex 改写项目名称造成安装完整性误判。
普通站点检查 13 项通过（3 项显式忽略，Composer 联网检查另行执行并通过）、环境变量
检查 22 项通过，桌面 Rust 编译检查、前端 TypeScript 检查和 diff 空白检查通过。

建站向导新增 Composer 缺失提示和安装按钮，复用全局安装任务并传入清单中的具体版本；
浏览器确认安装后仍保留名称和目录，PHP 7.4 无法继续创建要求 PHP 8.2 的模板。
错误详情折叠区在 390×844 和 1440×1000 下可展开、局部滚动，底部创建按钮保持可见，
390px 下左右各留 12px，无横向溢出。截图位于 `artifacts/site-improvements-20260926/`。
这些界面检查使用浏览器预览适配层，错误详情为仅注入自动化浏览器内存的模拟状态，
未改动 mock 源码；尚未完成桌面 IPC、真实 Nginx/Apache 及数据库连接的整链路验收。
本轮未执行用户数据库 SQL、未改动表结构或 `update.sql`，未启动前端 dev/build，
未新增测试文件。当前完善修改仍未提交发布，远程 main 与 v0.2.5 均核对为 `81efd9f`。

第四轮实施与验证：移除 Next-export 的普通 HTML 占位实现，新增通过当前 Node.js
生成官方 TypeScript/App Router 项目、独立下载并验证 pnpm SHA-512、安装依赖、
生成路由类型和 TypeScript 检查的流程。静态导出配置开启目录式路由与图片免优化，
移除不适用于静态导出的 `next start` 脚本，并提供 pnpm 开发说明。产品创建链路在最终
build 成功且检查到完整 `out/` 后才发布到目标目录；本轮严格未执行该 build 阶段，
因此不能把 Next.js 的整条建站链路记为验收完成。

实际验证使用隔离 Node.js 24.21.0、pnpm 10.34.5，官方生成器安装了 Next.js 16.3.6
和 React 19.2.8；源码、锁文件、依赖和类型检查通过，已有非空目录被拒绝且源码未改动。
验证目录为 `.verify-home/next-projects/next-sources-OvSRHs/project`。
修复运行时默认选择的字符串排序，验证 24.21.0 优先于 9.99.0，并确认显式选择过旧
Node.js 时会提示切换版本；套件响应同步标记 Node.js 当前启用版本。

Nginx 已实际验证首页、HTML 路由、目录索引、静态资源及自定义 404 响应，使用现有
1.28.1 程序与临时路由样例，没有执行前端构建；测试实例退出后无遗留 Nginx 进程。
Apache 同步补上 Next-export HTML 路由与 SPA 回退规则，真实 Apache 请求仍待验收。
浏览器验证了 Node.js 缺失/版本过低的拦截、安装后保留草稿、静态模板的数据库说明，
并发现及修复 fieldset 滚动溢出：改用独立滚动容器后，390px 下标题与底部按钮固定，
左右各 12px；1440px 下无横向溢出。截图为 `next-export-mobile.png` 和
`next-export-desktop.png`，位于前述 artifacts 目录。浏览器仍为预览适配层，
不替代桌面 IPC 验收；本轮没有用户数据库或 `update.sql` 变更，也未新增测试文件。

第五轮实施与验证：修复套件版本选择、服务注册和卸载的一致性。Nginx 启动遵循当前
选择的版本；所有套件（含纯运行时和通用服务）返回当前版本标记，停止态注册会更新版本
与端口，运行态保留实际进程对应的元数据。启停、切换、卸载与看门狗检查共用操作锁，
后端拒绝运行中切换单实例版本；单实例启动时固定选择，安装更高版本不会自动替换运行版本。
启动失败回收进程，进程消失后清理 PID/运行时长；收养的进程补上按 PID 停止兜底。
Redis 停止使用对应安装目录的 redis-cli 和实际启动端口，不再传 shutdown nosave。

卸载先确认具体安装记录及站点、用户服务栈、清单依赖；停止失败即终止卸载。
卸载非运行版本不会停止另一个版本，卸载后清理注册和看门狗记录、刷新回退版本及界面缓存。
固定版本的服务栈缺失该版本时报告缺失，不再静默使用另一版本。桌面卸载和版本切换命令
移至后台执行，避免停止进程或等待操作锁阻塞界面线程。

版本下拉只给实际运行版本显示运行状态，Node 等纯运行时可切换当前版本；修正运行灯、
多版本中文计数和操作标签。卸载按钮常显且支持键盘，弹窗执行期间禁用重复操作，失败保留
确认对象；保留分隔线两侧 12px 留白和虚线。窄屏侧栏自动收起且不覆盖桌面偏好，套件筛选
可换行、内容区缩小边距，长版本名可截断并在下拉查看。浏览器预览适配层补上版本选择、
正确的服务注册与依赖拦截，列表返回独立快照，避免原地修改造成查询缓存不刷新；预览仍不
代表真实安装或真实服务进程。

本轮 22 项套件/服务/服务栈检查通过（1 项真实 Nginx 检查默认忽略，另行执行通过）。
真实检查从现有 Nginx 1.28.1 复制程序到临时目录，使用临时端口请求得到 HTTP 200；
验证运行中切换被阻止、卸载其他版本不改变运行 PID、新增版本不替换选择、停止后无残留。
另外使用自有短暂子进程验证收养进程可停止及卸载不影响其他版本，临时进程均已回收。
桌面 Rust 编译检查、前端 TypeScript 检查通过；新增断言均在已有 Rust 源文件内，
未创建测试文件，未运行前端 dev/build。没有用户数据库、表结构或 `update.sql` 变更。
浏览器使用独立上下文验证纯运行时切换/卸载回退、搜索无结果和卸载确认，检查 390px、
320px 与 1440px 布局；截图保存为前述 artifacts 目录中的 `package-versions-mobile.png`
和 `package-versions-desktop.png`。完整桌面 IPC 和各服务全部生命周期仍需继续验收。
发布要求已核对根 AGENTS.md：每次提交/push/发布必须同步版本并新增、推送 tag；
目前远程 main 与 v0.2.5 仍为 `81efd9f`，本轮完善修改尚未提交发布。

第六轮实施与验证：下载与安装共用任务守卫，同版本安装/卸载互斥，任务结束后释放。
网络等待可取消；连接和读取各自超时，大文件不再因固定总时长被中断。416 清理片段后
重新请求，206 校验续传范围和长度；无 SHA-256 时仅对同源且有强 ETag/Last-Modified
的片段发送 If-Range，响应文件标识改变则重新下载。校验失败清理坏片段并尝试其他源，
缓存只有在哈希匹配时复用。不同 NiceEnv 进程之间的安装互斥尚未实现。

安装先解压到同级临时目录，校验入口并保存安装描述后才发布正式目录；取消或失败不会
登记半份安装。已有未登记目录先备份，配置/记录保存失败时恢复旧目录，回滚失败会说明
备份位置。安装保留已有 PHP/Apache 配置；已安装记录缺失主程序时明确报错。ZIP/单文件
复制可取消，tar/gzip 子进程纳入回收；tar 包解压失败不再回退为单文件 gzip，以免把 tar
内容误当主程序。macOS 单文件 gzip 和 tar 路径/软链接完整安全边界仍需验收；本轮没有
新增依赖。服务启动时的配置生成行为还需另行审查，不能把安装保留配置等同于整个生命周期。

界面仅在后端返回 CANCELLED 时标记已取消；请求期间显示正在取消，最后提交阶段禁用
取消，拒绝取消或真实错误不会被误报为成功。完成/失败/取消后清除行内进度，未知文件大小
不显示虚假的完成百分比。安装后启动单实例服务会先选择新装版本；启动失败保留弹窗，
成功后再关闭。弹窗内容独立滚动、操作区固定，长路径换行，错误通知限制两行，阶段说明
在窄屏折行，避免文本挤出容器。

本轮最终检查：下载器 5 项、安装相关 10 项、原有下载集成检查 2 项全部通过；另有 1 项
官方联网检查已单独执行通过：在临时目录下载并安装 Nginx 1.31.6，运行 nginx.exe -v 得到
nginx/1.31.6，重复安装幂等，卸载后无运行时目录或安装记录。本轮未启动 Nginx 服务。
tar 验证使用系统 tar 生成的真实压缩包，正常安装通过，损坏包失败且无正式目录/记录。
Rust 桌面编译检查、前端 TypeScript 检查通过，断言均补在已有源文件或已有检查文件中，
没有新增测试文件，未启动前端 dev 或执行前端 build。

浏览器使用独立上下文和仅存在内存的受控 IPC 响应，验证等待取消、确认取消、拒绝取消、
取消请求后真实失败、失败重试、未知大小、进度清理及先选新版本再启动的调用顺序；
启动失败保留弹窗，重试成功关闭；后台安装可从列表取消，启动等待期间禁用重复操作并阻止
意外关闭。320×568、390×844 和 1440×1000 检查无内容横向溢出，
窄屏两侧各留 12px，短屏内容滚动而底部按钮保持可见；浅色长错误和深色取消状态已查看。
截图为 `artifacts/site-improvements-20260926/install-error-mobile.png`、
`install-error-desktop.png`、`install-cancelled-mobile.png` 和 `install-cancelled-dark.png`。
这些模拟响应只证明前端交互，不替代真实桌面 IPC 的端到端验收。验证上下文在结束时关闭，
没有修改用户数据库、hosts、系统代理或 FlyEnv 文件，没有表结构及 `update.sql` 变更。
整体完善仍在进行中，当前修改未提交发布；远程 main 与 v0.2.5 再次核对为 `81efd9f`。

第七轮实施与验证：修复 PHP 启动时无条件重写 php.ini，已有文件保留，首次运行才生成。
配置编辑器列出每个已装 PHP/MySQL/Redis 版本，读取、保存、历史与回滚均绑定版本；
服务诊断同步使用被诊断服务的实际版本，MariaDB 不再误读 MySQL 配置。保留旧无版本
调用的兼容入口，但无法确认版本的历史备份禁止自动回滚，原备份文件仍保留。

保存先检查格式，再核对磁盘内容与编辑时快照，发现外部修改即保留草稿并报告冲突，
强制保存也不能绕过冲突。备份包含准确目标、纳秒时间与随机后缀，同秒连续保存不覆盖；
历史按目标过滤后取最近 30 条。备份成功后才用同目录临时文件原子替换原配置，保留权限；
回滚也先备份当前文件。拒绝跨版本回滚、路径穿越、Windows ADS、符号链接备份和
无版本目标的含糊备份。保存、回滚、原生校验及 PHP 扩展开关共用生命周期锁。

Nginx/Apache 校验使用实际选择的程序与正确工作目录、配置前缀，临时文件位于配置目录；
有原生程序时以原生校验结果为准，不再误拒绝引号内的 #、花括号及多行指令。
Apache 不再套用 Nginx 分号规则；缺少已安装服务的程序或程序执行失败，不显示校验通过。
校验有超时与进程回收，输出限量读取，结束清理临时文件；桌面命令移到后台线程避免阻塞 UI。

界面补上读取失败重试、持久错误提示、保存前校验、行号定位、强制保存确认、草稿丢弃确认、
外部冲突提示及按版本查看历史。保存期间只读、防重复且阻止关闭；父列表刷新不再重读覆盖
后续草稿。长校验信息可展开并滚动，短屏编辑区至少 176px，标题和底部操作保持可见。
预览适配层同步保存内容和历史，不再无条件返回保存成功；预览仍不代表真实服务执行。

本轮最终配置检查 30 项通过，2 项真实程序检查默认忽略并已分别显式执行通过；原有配置
集成检查 2 项通过。真实 PHP 8.4.26 从已有运行时复制到临时目录，FastCGI 请求确认
memory_limit 从 321M 改为 512M 后保持，gettext 禁用经过自动和手动重启仍生效；
自有 worker 全部停止。真实 Nginx 1.28.1 执行 -t，确认选择正确版本、合法复杂内容通过、
无效指令准确定位第 2 行、原配置不变及临时文件清理；未启动 Nginx 服务。
Apache 原生调用已接入但尚未用实际二进制验收，不能把接入视为验证通过。

前端 TypeScript 检查、桌面 Rust 编译检查和 diff 空白检查通过。浏览器独立上下文验证了
草稿保护、失败重试、校验拦截、保存等待只读/防重复/阻止关闭、版本隔离和回滚；
320×568、390×844、1440×1000 下无横向溢出，窄屏左右各 12px，编辑区和操作区可用。
已查看浅色长错误与深色编辑截图，位于 `artifacts/site-improvements-20260926/`：
`config-editor-errors-320.png`、`config-editor-errors-desktop.png`、`config-editor-dark-mobile.png`。
浏览器使用仅存在内存的受控 IPC 响应，无 pageerror；完整桌面 IPC 与跨平台仍待验收。

截至第七轮的剩余实质问题：Nginx、Apache、MySQL、Redis 的配置仍会在启动或站点更新时重新生成，
手动编辑的持久化尚未修复；界面已说明当前限制，该说明不替代后续功能修复。
本轮无用户数据库、表结构或 `update.sql` 变更，未修改 hosts、系统代理或 FlyEnv 文件，
未启动前端 dev/build，未新增测试文件。验证进程已回收，整体目标继续进行。
当前完善修改尚未提交发布；远程 main 与 v0.2.5 的目标提交仍核对为 `81efd9f`。
后续每次提交、push、发布必须同步版本号，并创建、推送新的版本 tag，按根 AGENTS.md 执行。

第八轮实施与验证：修复 Nginx、Apache、MySQL、Redis 整份配置在启动/更新站点时被
默认模板覆盖。首次运行仍生成默认值，后续保留用户内容，仅同步应用维持服务和站点管理
所必需的项。已有配置直接采用相同更新逻辑，无须用户重建配置；无法安全解析的 Nginx
结构返回错误并保留原文件，不静默替换。界面和生成文件说明哪些设置由应用维护。

Nginx 按指令边界更新 PID、运行时 mime.types、默认 `server_name _`、`nsb_php_*`
连接池和站点 include；其他全局参数、用户 server/upstream/map、注释保留。
解析支持引号、转义、注释、`${变量}`、紧凑单行和 CRLF，重复同步不产生额外内容。
Apache 保留模块、日志级别、自定义指令和嵌套配置，仅同步 ServerRoot、NSB_ETC、
顶层 Listen、TypesConfig 和应用站点 include；缺失的根目录定义在使用前补入。
这些托管项仍由应用控制，不能将“保留自定义配置”理解成任何托管项都能任意覆盖。

MySQL 保留缓冲池、连接数、SQL 模式、自定义节和 include，仅同步运行目录、数据目录
及服务/客户端端口；启动和初始化命令以命令行再次约束这些路径与端口，避免 include
重新覆盖。Redis 保留内存、淘汰策略、持久化、认证及重复 save 规则，仅同步端口、
数据目录和前台运行方式，并在启动参数中确保托管项生效。MySQL 新版本默认模板继续关闭
独立 X Protocol 端口，5.7 不传不支持的 mysqlx 参数。

配置生成复用编辑器的按版本备份、外部改动检查和原子发布；备份失败不覆盖原文件，
内容不变不重复备份。站点重建与编辑器共用生命周期锁。服务启动/重载中的 Nginx/Apache
原生校验补正确工作目录和前缀，并复用带超时、输出限量和进程回收的校验执行器。
修复自动回落端口无条件递增、MySQL 启动后检查旧端口、MySQL/Redis 运行状态记录旧端口
的问题；记录实际端口后，停止命令也能准确定位本实例。

本轮 6 项配置同步检查、30 项配置编辑检查、3 项既有服务/端口检查通过。新增断言在已有
Rust 源文件内，未创建测试文件。另显式执行了 4 项真实程序验证，全部通过：

- Nginx 1.28.1：临时目录中的自有实例通过真实 HTTP 确认 gzip/自定义响应头在重建、
  PHP 池变化和端口变化后保留；原有版本选择/卸载隔离断言同时通过。
- Apache 2.4.66：真实配置校验和 HTTP 200 通过，自定义 Header 与 Timeout 在重建、
  重启及端口变化后保留，使用带空格的配置目录。
- Redis 5.0.14：真实 CONFIG GET 确认 64MB 内存、noeviction、32 个逻辑库设置在重启后
  保留；占用的端口仅为验证自身监听器，回落后状态记录正确，再次重启保持同一端口。
- MySQL 8.0.46：仅执行 mysqld --verbose --help，实际选项解析确认连接数 321、缓冲池
  32MB、正确端口与数据目录，include 中的旧托管参数被启动参数覆盖；没有初始化数据目录、
  启动 MySQL 服务或执行 SQL，完整数据库启动/连接仍不算验收通过。

Apache、Redis、MySQL 验证包来自项目清单中的发行地址，SHA-256 均与清单匹配。
下载期间发现 D 盘空间不足，已删除本轮未解压完整的 MySQL 调试符号文件，并仅将本轮
新建的验证包目录移动至 `C:/Users/Carefree/AppData/Local/Temp/niceenv-config-lifecycle-20260926/`；
未清理用户文件、已有构建或此前验证目录。实际用例数据均为临时目录，自有服务进程已停止。

前端 TypeScript 检查、桌面 Rust 编译检查和 diff 空白检查通过。浏览器独立上下文检查
新的配置说明：320×568 与 1440×1000 无横向溢出，窄屏两侧各 12px，保存操作保持可见；
截图为 `artifacts/site-improvements-20260926/config-managed-mobile.png` 与
`config-managed-desktop.png`。浏览器使用项目预览适配层，不代替桌面 IPC 验收。

下一步已确认的缺漏：工具箱“重写默认配置”实际只重启 Nginx；通用备份恢复仍按文件名
查找目标，未覆盖多版本歧义及完整路径保护；这些入口需要继续修复。另需验收 MySQL 完整
启动、认证配置对应的客户端行为、其他套件、完整桌面 IPC 与跨平台。整体目标仍在进行。
本轮没有用户数据库、表结构或 `update.sql` 变更，未修改 hosts、系统代理或 FlyEnv 文件，
未启动前端 dev/build。当前修改未提交发布；发布时仍必须同步版本、新建并推送 tag。

第九轮实施与验证：工具箱“重写默认配置”改为实际的配置重置，不再伪装成重启 Nginx。
支持 Nginx、Apache、PHP、MySQL、Redis，按已安装服务和版本选择，预览准确文件路径与
默认内容，确认后先备份再写入。仅影响选中的文件，不自动重启服务；界面提示重启后生效。
不存在的配置可生成，损坏的非 UTF-8 当前文件也可备份后重置；未安装或不支持的类型拒绝操作。

普通配置备份使用独立目录记录相对目标路径、SHA-256 和原文件内容，纳秒时间与随机后缀
防止连续保存覆盖。内容未变不重复备份；备份成功后才原子替换目标，备份失败保留原文件。
Windows 发布备份目录前显式关闭内容与元数据文件句柄，解决目录重命名“拒绝访问”。
新普通备份、配置编辑器历史与旧根目录备份在工具箱统一显示；新普通备份可从编辑器回滚，
编辑器历史也可从工具箱恢复。旧根目录备份在编辑器中使用明确的 legacy 标识，避免误从
backup/config 读取。无版本信息的旧 PHP/MySQL/Redis 备份以及目标不唯一的旧备份禁止
自动恢复，保留文件并说明原因；损坏条目不隐藏其他有效历史。

恢复和重置确认绑定目标路径、当前内容与待写入内容，快照失效时要求重新预览。
服务操作与这两个入口共用生命周期锁，写入前再次检查当前文件；路径检查拒绝目录越界、
盘符、ADS、Windows 设备名、尾点/尾空格、符号链接和目录联接。工具箱恢复仅允许 etc 下
的配置，证书/密钥等其他备份显示来源并禁用此入口恢复，避免绕过对应功能的状态管理。

界面使用真实的加载、错误与重试状态，支持按路径/备份名搜索，显示准确版本、时间和大小。
失败保留对话框；执行时锁定选择与关闭入口并防重复提交；关闭后焦点回到触发按钮。
浏览器预览适配层复用内存配置历史，重置与恢复实际更新该内存状态，不能无条件返回成功。
配置编辑保存/回滚后会刷新统一备份列表。

本轮验证：6 项路径/备份检查、35 项配置编辑检查、6 项托管配置同步检查、既有备份恢复与
证书签发各 1 项检查通过。新增验证位于已有 Rust 源文件，未新建测试文件。
另显式运行 1 项真实程序检查：Nginx 1.28.1 与 Apache 2.4.66 的默认配置重置、工具箱恢复
均通过各自原生配置校验；只执行 nginx -t/httpd -t，未启动服务。之前需要独立环境的
FastCGI 和旧原生校验用例没有在本轮重复执行，不把默认跳过当作本轮通过。
补充证书检查曾遇到并行链接 PDB 的 LNK1318 错误，改用 -j 1 后已通过。
前端 TypeScript、桌面 Rust 编译与 diff 空白检查通过，未执行前端 dev/build。

浏览器独立上下文验证编辑→重置→备份恢复、搜索无结果、读取失败、预览失败、写入失败、
冲突重试、执行中禁止关闭、准确版本和无可重置服务状态，未出现 pageerror。
320×568、390×844、1440×1000 以及深浅主题已检查，窄屏左右各 12px，长内容滚动且底部
操作保持可见，键盘焦点在弹窗内并在关闭后返回。截图位于 artifacts/site-improvements-20260926：
config-reset-mobile.png、backup-restore-error-mobile.png、config-reset-error-desktop.png、
config-reset-dark-mobile.png。浏览器适配层和受控 IPC 响应用于交互验证，不替代完整桌面 IPC。
自建浏览器上下文已关闭，未改变原始浏览器页面，原生校验子进程已退出。

本轮没有用户数据库或表结构变更，未修改 update.sql、hosts、系统代理或 FlyEnv 文件。
后续仍需核对 MySQL 完整启动/认证、多实例客户端与数据库备份恢复、其他套件、完整桌面 IPC
和跨平台链路。整体目标继续进行，当前累计修改仍未提交发布。已再次核实远程 main 与
v0.2.5 指向 81efd9f；提交/push 新版本必须同步版本号、新建并推送 tag，按根 AGENTS.md 执行。


第十轮实施与验证：补齐 MySQL 认证、实例选择、数据库管理、备份恢复和跨环境导入。
数据库操作现在按明确版本选择安装路径，要求实例实际运行，使用本次启动记录的端口，
并查询 @@datadir 核对数据目录，避免连接到另一个版本或占用同端口的外部 MySQL。
密码按 mysqlRootPassword@版本保存，兼容旧全局记录；首次初始化仅存在 root@localhost
也能设密，不再忽略第二个本地账号不存在导致的失败。初始化先写同级临时数据目录，
成功才发布，不再递归删除最终数据目录。首次启动延长就绪等待，并在端口可连接后短暂重试
认证；失败包含实际日志。已有实例未知密码可继续运行，由数据库页验证并更新本机凭据。
修改密码遇到连接中断时先验证新旧凭据，避免服务器已修改成功却无条件回退本机记录。

mysql、mysqladmin、mysqldump 统一使用私有临时 defaults 文件，第一参数传入配置，
隔离 login-path 文件，清除 MYSQL_PWD；密码及含密码 SQL 不出现在进程命令行。
SQL 经临时 stdin 输入，输出与错误重定向文件，带超时、限量读取和进程回收。
真实 8.0.46 不支持 --no-login-paths，mysqldump 不支持 --connect-timeout，均未误用。
列库改为一次聚合查询，库名 HEX 编码避免响应分隔符歧义；后端拒绝删除系统库，
创建账号拒绝已有同名本地账号，授权失败回收本次新账号，不再把未修改密码视为成功。

备份先写临时 SQL，完整成功且非空才发布；同名目标不覆盖，失败不删除既有文件。
文件名限制在备份目录内，拒绝路径越界和 Windows 数据流。备份列表读取错误会显示重试。
恢复前保护性备份失败直接中止；恢复失败明确提示部分 SQL 可能已执行，保留保护备份位置。
导入先完整导出源库，再备份目标和恢复，带 --databases，不再边导出边修改目标；
拒绝系统库、过期选择及同实例自导入。MySQL 8+ 客户端禁用 column statistics，
以兼容较老来源的选项差异；这不代表所有跨版本组合都已原生验收。

数据库页支持选择 MySQL 实例，所有请求和查询缓存按版本隔离；错误、加载、未启动和空列表
分别展示。连接信息使用实际端口，CLI 提示交互输入密码。root 操作区分“修改实例密码”与
“更新本机连接密码”，可显式查看/复制已保存密码。创建账号使用业务库下拉，密码默认隐藏。
备份和恢复操作在触屏可见，失败保留弹窗、成功或部分执行后刷新数据；导入来源变化清空
旧库列表，要求确认目标，部分失败保留报告。忙时禁止重复、切换实例和关闭弹窗。
修复弹窗重复 key，并为无 Trigger 的弹窗恢复入口焦点；小屏导入操作区固定可见。
网页预览也维护每个实例的数据库、账号、密码和备份快照，不再无条件返回操作成功。

建站数据库连接使用选定安装、实际端口与版本密码，连接前核对数据目录；WordPress、
框架 .env 和 .env.example 使用实际端口与正确的密码转义。站点已有 db JSON 保存可选的
MySQL 版本和端口，启动后记录各版本最后实际端口，后续 .env 补全不受另一个实例的全局
端口变更影响。兼容旧站点记录，没有新增数据库表、字段或 migration；站点列表继续隐藏密码。

验证：17 项数据库相关断言、1 项站点绑定持久化/端口隔离检查通过；断言均在已有 Rust
源文件中，未创建测试文件。真实 MySQL 8.0.46 在两个独立临时数据目录与临时端口完成
首次设密、特殊字符/中文密码、重启、更新本机凭据、错误数据目录拒绝、业务库/账号创建、
重复账号与系统库删除拦截、导出/恢复、保护性备份失败中止、同名备份保留、跨实例导入和
自导入拒绝。两实例复用同一 8.0.46 二进制，不冒充跨 MySQL 大版本验证；自有进程已回收。
首次原生检查曾出现认证/启动就绪失败，增加就绪重试与日志后，本轮最终完整用例通过。
前端 TypeScript、桌面 Rust 编译检查与 diff 空白检查通过，未执行前端 dev/build。

浏览器采用独立上下文，检查项目预览适配层和仅存在浏览器内存中的受控响应：
版本切换/缓存隔离、准确版本请求、认证失败、备份读取重试、恢复失败、部分导入失败、
来源变动后重新检测、执行中锁定及焦点恢复。320×568、390×844、1440×1000，深浅主题
均已检查；320px 导入弹窗左右各 12px，内部无横向溢出，底部按钮保持可见。
受控场景没有 pageerror，不代替完整桌面 IPC 验收。截图位于 artifacts/site-improvements-20260926：
mysql-databases-320.png、mysql-databases-desktop.png、mysql-restore-error-mobile.png、
mysql-import-partial-mobile.png、mysql-import-dark-320.png。

本轮仅在自建隔离实例操作验证数据，未操作用户数据库、hosts、系统代理或 FlyEnv 文件，
未修改 update.sql，未安装依赖。整体完善目标继续进行：仍需完整桌面 IPC、其他 MySQL
大版本/来源组合、跨平台及其他套件链路验收。当前累计修改尚未提交发布；远程 main 与
v0.2.5 核对仍为 81efd9f。后续提交/push 新版本必须同步版本号、新建并推送 tag，
按根 AGENTS.md 执行，禁止只推送分支或移动已有 tag。

两个自建浏览器上下文已关闭，原始页面保持 /sites；收尾未发现 mysqld 残留进程。
收尾再次检查远程时 GitHub 443 连接超时，远程分支/tag 状态以上述本轮开始时成功的
ls-remote 结果为准；本轮没有执行 commit、push 或新增 tag。


第十一轮实施与验证：统一 Adminer 入口与生命周期，修复 Redis 统计，并补上外部 SQL 文件恢复入口。
数据库页不再直接跳转旧 Nginx /_adminer/ 路由，与工具箱共用管理台启动和状态查询。
后端按用户选定的 PHP/Adminer 版本、安装记录真实目录和安装时入口描述解析程序，
加载对应 php.ini；入口可包含子目录与空格，URL 正确编码。仅绑定 127.0.0.1，
启动前检查端口，启动后核对 HTTP 登录表单和监听 PID，未就绪则回收进程并报告日志。
真实验证发现 Windows canonicalize 的 \\?\ 前缀会令 PHP 返回 200 但加载脚本失败，
现已为 PHP 文档根目录转换路径，保留规范路径用于安装目录边界检查。

管理台状态归属 CoreState 的服务管理器，保存本次进程实际入口、端口和两项版本；
重复启动复用原进程与原 URL，换页后仍可打开和停止。生命周期锁防止并发启动及卸载竞态，
运行期间拒绝卸载正在使用的 PHP/Adminer 版本。进程加入 ProcessGroup，停止和应用正常退出
执行回收，PID 同步既有 pidfile；异常退出后的残留由原有归属核对流程处理。
桌面启动、状态和停止命令使用工作线程，打开浏览器失败明确显示，管理台状态不会因此丢失。
网页预览明确说明不能启动本机 PHP，不再模拟“管理台已经启动”。旧 Nginx 兼容路由仍保留，
本轮没有将该路由作为新入口的依赖，也没有对其跨平台配置做验收。

Redis 统计改为按 RESP2 bulk 长度读取，不再等持久连接关闭或把 NOAUTH/ERR 当作可用统计。
连接、读写有超时，响应长度有限制；认证、权限、协议和未运行错误分别返回。
INFO 字段精确匹配，键数汇总所有逻辑库；缺少 keyspace 时显示未知，不虚构 0。
CoreState 使用实际运行服务的端口，并核对 INFO process_id 属于托管进程，避免读取外部实例。
前端仅对运行实例轮询，按版本/端口/PID 隔离缓存，显示错误与重试；移除固定“无密码”说明。
本轮未新增 Redis 凭据保存或自动认证：启用认证时明确提示通过 Redis 客户端认证使用 INFO，
不会读取配置中的疑似密码或声称已支持完整 ACL 管理。

数据库备份卡增加本机 .sql 文件选择，选择后进入已有恢复确认流程，显示完整路径、
目标 MySQL 版本与实际端口，仍先做保护性备份。选择文件、执行恢复时锁定实例和重复操作；
恢复失败保留确认框与错误，成功刷新相关数据。SQL 需包含 USE 语句选择库；没有库选择的
文件明确指引到 Adminer 选库导入，不把此入口描述成支持所有 SQL 文件格式。
网页预览不访问本机文件或伪造还原成功。恢复完成、取消确认和取消文件选择后恢复入口焦点。
共同确认弹窗限制长文件名换行与横向宽度，正文内部滚动，底部操作区保持可见。

验证：4 项既有 Rust 源文件内的 Redis 协议检查通过，覆盖持续连接、多逻辑库、
认证/权限/命令错误、超长及不完整帧。1 项真实 PHP 8.4.26 + 官方 Adminer 6.1.0 检查通过，
Adminer 下载 SHA-256 与清单 d21891f420eac5553e9a85d8af2ef3375c1066c4b5c0f573d8e4f936666d2000 一致。
在隔离临时目录/端口验证真实登录页、含空格入口、重复启动、版本切换后保持原运行入口、
卸载保护、PID 记录、端口冲突、损坏/缺失入口拒绝及应用停机回收。
另 1 项真实 Redis 5.0.14 检查通过，覆盖设置端口与实际端口分离、两个逻辑库的键数、
运行期开启 requirepass 后拒绝假成功、认证移除后恢复统计，并保留既有配置/重启检查。
没有新建测试文件；新增断言位于已有源文件测试模块。自建 PHP/Redis 进程均已退出。
TypeScript 检查、桌面 Rust 编译检查和 diff 空白检查通过，未启动前端 dev 或执行 build。

浏览器使用两个独立上下文：原生网页预览检查桌面专属操作提示；仅在独立上下文内替换
模块响应验证文件选择、目标参数、确认前无恢复调用、忙时禁止取消/Esc/版本切换、
失败保留与重试成功、焦点恢复，以及 Adminer 跨页面状态、准确打开 URL、浏览器打开失败、
启动失败、停止与状态读取恢复。Redis 认证错误不显示虚构键数，恢复后可重试获取统计。
上述受控响应没有写入生产调试入口，不代替真实 Tauri 文件选择器及完整桌面 IPC 验收。
320×568、390×844、1440×1000 与深浅主题已检查，无 pageerror；320px 弹窗左右各 12px，
极长文件名不产生横向溢出，底部操作按钮仍在弹窗内可见。截图位于 artifacts/site-improvements-20260926：
sql-file-restore-error-320.png、sql-file-dark-320.png、adminer-console-320.png、redis-auth-dark-320.png。

参考官方 PHP 内置服务器与 Redis RESP 协议文档：
https://www.php.net/manual/en/features.commandline.webserver.php
https://redis.io/docs/latest/develop/reference/protocol-spec/
本轮开始的 fast-context 因网络失败不可用，按已定位调用链进行本地定点搜索。
本轮没有用户数据库、表结构、hosts、系统代理或 FlyEnv 文件变更，未修改 update.sql，未安装依赖。
整体完善目标仍在进行：Redis 认证连接管理、旧兼容入口、其他套件、跨平台及完整桌面 IPC 尚需继续核对。
累计功能修改未 commit/push/发布。远程复核前次连接失败，之后 ls-remote 成功确认 main 与
v0.2.5^{} 均为 81efd9f860590e84a75cd33f9dc93370744235be，annotated tag 对象为 24c76a93。
提交/push 新版本必须同步项目版本号、新建并推送 tag；根 AGENTS.md 的强制发布规则继续有效。

本轮两个自建浏览器上下文已关闭，原始页面仍为 /sites；无遗留验证服务，已发送桌面通知。


第十二轮实施与验证：补上 Redis 本机连接认证，以及外部 SQL 恢复的默认目标库选择。
Redis 凭据按 redisConnection@版本保存，先核对 loopback 监听 PID 归属，再发送 AUTH，
并核对 INFO process_id；错误密码、过期版本和非托管端口均不写入凭据。
兼容 Redis 5 的 AUTH password 和 Redis 6+ 的 AUTH username password；元信息只返回
用户名与是否已保存密码，不回填密码，AUTH 错误不回显服务端正文。配置导入/导出排除
这些本机连接记录。停止 Redis 同样使用对应版本凭据，拒绝认证/权限失败后的强杀；
实例仍存活的 Error 状态可重新验证凭据，成功后恢复 Running。Windows 关闭连接时的
ConnectionReset/ConnectionAborted 交由后续 PID 退出检查判断，不能仅据此报告停机完成。
启动超时附加已有服务日志，便于区分配置加载与监听就绪阶段；临时排查输出已移除。

数据库页新增连接认证弹窗：密码默认隐藏，支持可选 ACL 用户名、无认证连接、保存前
验证、错误后继续编辑和重试；执行中锁定输入、关闭和重复提交。绑定打开时的实例版本
和实际端口，成功后刷新统计/凭据/服务缓存，关闭恢复入口焦点。
SQL 恢复新增“默认目标库”：外部文件必须明确选择业务库或按文件库名执行，应用内
备份默认按文件库名。后端检查所选库存在且不是系统库，通过客户端 --database 指定
默认库；不重写 SQL，文件中的 USE 或明确库名仍然有效，界面明确提示可能影响其他库。
保护性备份仍覆盖当前所有业务库。恢复失败保留确认框、所选库和保护备份信息。
窄屏检查发现弹窗错误与底部通知重复，可能遮挡按钮；连接认证和备份操作在弹窗内
显示错误，外部文件选择失败继续通知，避免在弹窗中重复提示同一错误。

验证通过：pnpm --filter @nsb/web check、cargo check -p niceservbay --locked --offline -j 1、
git diff --check；Redis 相关既有 Rust 源文件检查 10 项通过，原生用例默认 ignored。
断言覆盖旧式/ACL AUTH 编码、UTF-8 凭据、错误不泄密、发送凭据前的端口归属检查、
RESP 错误/不完整响应、多逻辑库统计和配置导入/导出凭据隔离。没有新增测试文件。
真实 MySQL 8.0.46 的既有隔离用例通过：无 USE 的 SQL 写入指定业务库，拒绝系统库与
不存在的库；显式 USE 仍指向文件指定库，符合界面说明，恢复前生成保护性备份。
两个 MySQL 实例均为临时数据目录和端口，不代表跨大版本兼容验收。

未通过、必须继续：真实 Redis 5.0.14 原生生命周期用例已显式运行，当前仍在首次启动
阶段遇到 REDIS_START_TIMEOUT，日志停在 Configuration loaded。此前一次执行已通过
认证拒绝、错误密码不覆盖、正确密码保存/统计、版本隔离等步骤，但停机读连接重置被
误报超时；修正后仍被启动问题阻挡，尚未完整通过认证停机、RDB 保存和重启后的验证。
原生用例仅在隔离配置中启用 save 3600 1，不改变生产默认 save "" / appendonly no。
对照使用同一 Redis 二进制、临时配置/端口、含空格路径和 Windows Job Object 直接
启动可监听；复用二进制替代复制、脱离 cargo 直接运行用例均未消除托管启动失败。
现有证据不足以断言是配置、磁盘空间、杀毒或 Job Object 问题，不能宣称已经修复。
Redis 6+ ACL 仅做协议仿真，未做原生 Redis 6+ 验收。完整桌面 IPC 与跨平台验证仍待继续。

浏览器：独立预览与受控响应上下文完成密码隐藏/空密码禁用、认证失败、过期版本错误、
保存成功、慢请求锁定、Esc 不关闭、焦点恢复，以及 SQL 必选恢复方式、准确传递版本和
默认库、按文件模式省略 database、失败保留/重试。真实网页预览不会把任意密码当成功。
320×568、390×844、1440×1000 与深浅主题已检查；320px 弹窗左右各 12px，内部正文滚动，
底部按钮可见，无横向溢出；受控页面无 pageerror。响应替换只存在浏览器验证上下文，
没有写入生产调试入口，不代替完整 Tauri IPC。截图位于 artifacts/site-improvements-20260926：
redis-credentials-dark-320.png、redis-credentials-busy-desktop.png、redis-credentials-390.png、
sql-target-error-320.png、sql-target-dark-320.png。

本轮没有用户数据库或表结构变更，未修改 update.sql，未安装依赖，未启动前端 dev/build。
未操作用户 hosts、系统代理或 FlyEnv 文件。根 AGENTS.md 已在 81efd9f/v0.2.5 中固化
“提交/push/发布必须同步版本号并新建、推送 annotated tag”的规则；本地 tag 类型与
指向已核对。本轮远程 ls-remote 连接被重置，没有据此声称远程核对成功。
累计修改尚未 commit/push/tag；整体功能完善目标保持进行中，尤其 Redis 原生启动问题
仍需处理，不能将此阶段视为整体完成或已发布。


第十三轮实施与验证：解决上轮 Redis 原生启动失败，补齐端口冲突恢复和服务操作可访问性。
本轮修复日志读取后确认实际错误为监听端口绑定失败。旧日志读取使用 UTF-8 lines，遇到
原生程序输出的非 UTF-8 系统错误会中止，导致日志停留在 Configuration loaded；现改为
按换行读取字节并宽容解码，保留错误及后续日志。部分本地字符可能显示替换符，不再丢掉整行。
进程句柄也确认 Redis 早期已以退出码 1 结束，不能继续将其描述为未完成配置初始化。

端口预检、相邻端口回落与 PHP 池分配从“连接不上即空闲”改为实际 loopback 绑定检查，
识别已绑定但未监听等不可分配端口。绑定失败且存在监听者时仍返回 PORT_IN_USE；
没有监听者时返回 PORT_UNAVAILABLE，提示换端口或稍后重试，不提供结束进程操作。
端口 0 明确拒绝，接近 65535 的搜索和旧 PHP 池数据用 checked_add 防止溢出。
自动回落优先沿用仍可绑定的已保存端口，再查相邻 32 个；耗尽时通过 bind 0 请求系统
分配端口，排除原端口和 avoid 列表，沿用现有覆盖设置持久化。关闭自动回落时不会分配。
检查本机动态端口范围为 1024 起、共 13977 个，未修改系统配置；端口检查并不保证启动前
不会发生外部进程抢占。没有引入 WinSock feature 或其他依赖，也未修改 Cargo.toml。

前端端口恢复只处理明确的 PORT_IN_USE，不再将所有携带 port 的错误视为可结束进程。
释放操作防止同一按钮重复提交，等待释放成功后再等待原操作重试；释放失败不继续启动。
卡片、列表的启动/重启回调由单纯刷新缓存改为实际重跑操作，服务栈部分失败报告也传递
原栈的异步恢复回调；没有回调的入口仅显示“结束占用进程”，不承诺重试。
服务开关增加实例可访问名称、button 类型和忙碌状态；共用 Button 对 icon/icon-sm
尺寸以 Tooltip 标题补全缺省名称，保留显式 aria-label，不替换普通文字按钮名称。
服务列表图标在小屏默认可见，桌面悬停或键盘聚焦可见；320px 下名称和操作分行，
总览健康矩阵工具栏换行，避免标题竖排、名称消失及开关被裁切。桌面继续单行显示。

最终验证：既有 fallback_tests 模块 7 项通过，覆盖非 UTF-8 日志继续读取、已绑定未监听、
无效端口/边界、相邻候选耗尽后的系统分配、回落持久化与关闭回落，以及损坏 PHP 池数据。
真实 Redis 5.0.14 生命周期用例最终连续三次通过，推翻第十二轮“启动问题仍未解决”的状态：
验证 requirepass、认证停机失败保留进程、错误密码不覆盖、正确密码保存/统计、版本隔离、
RDB 保存、停机重启、移除认证后的凭据更新，以及实际端口稳定。真实 MySQL 8.0.46 隔离
用例也通过，确认共享端口检查未破坏初始化、认证、备份、恢复和导入链路。
cargo check -p niceservbay --locked --offline -j 1 通过；最后前端修改后重新运行
pnpm --filter @nsb/web check 与 git diff --check 均通过。没有新建测试文件或执行前端 dev/build。

浏览器独立受控上下文验证卡片/列表启动与重启、服务栈部分失败的准确调用顺序：
原操作失败 → close_port → 重跑原服务/栈操作；释放失败不重试，同一按钮重复点击只释放一次。
PORT_UNAVAILABLE 不出现结束进程按钮。开关可用 Space 操作，图标按钮名称可被辅助技术识别，
键盘聚焦时可见。320×568、390×844、1440×1000 与深浅主题已检查，服务操作未越出容器，
无横向页面溢出或受控页面错误。端口释放在浏览器中使用受控响应，没有结束用户占用进程。
截图位于 artifacts/site-improvements-20260926：port-unavailable-320.png、
port-unavailable-dark-390.png、port-conflict-desktop.png、service-actions-dark-320.png、
service-actions-light-390.png、service-actions-desktop.png。

本轮没有用户数据库或表结构变更，未修改 update.sql，未操作用户 hosts、系统代理或 FlyEnv
文件。收尾未发现 Redis/MySQL 原生验证残留进程。整体目标继续进行，原生 Redis 6+ ACL、
完整 Tauri IPC、跨平台及其他套件仍未完成全面验收，不把受控浏览器响应视为桌面端通过。
累计功能修改尚未提交发布，版本仍为 0.2.5；本轮未重新验证远程，也未创建 commit/push/tag。
根 AGENTS.md 的强制发布规则持续有效：提交/push/发布时同步版本号并新建、推送 annotated tag，
不能只推分支、覆盖旧 tag 或将流水线已触发说成安装包已发布。

本轮自建 qa=ports-13 浏览器上下文已关闭，原始 /sites 页面保留，已发送桌面通知。


第十四轮实施与验证：补齐批量服务启停的真实结果、失败恢复和小屏操作界面。
参考 ServBay 官方服务管理说明，继续核对快捷启停、服务状态和日志入口：
https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management
https://support.servbay.com/basic-usage/services-and-packages/service-management-panel

后端批量启停与整栈操作持有现有可重入生命周期锁，避免多项编排被另一启停/卸载操作穿插。
批量输入先稳定去重，再按原依赖层级排序；未注册服务返回明确失败，Starting/Stopping
不再当作启动完成。停止依据实际存活 PID 和过渡状态判断，Error 且仍持有进程的服务不会
被记成“已停止”。重启先逆序停止，仅把未发生停止错误的服务带入启动阶段，同一服务不再
同时出现在失败与成功/跳过集合。服务栈解析也按最终 service id 去重，固定版本缺失不能
替换成另一版本。桌面 bulk_start/stop/restart 改用已有 spawn_blocking 模式并刷新托盘。

总览和命令面板共用快捷操作 hook，删除逐项 catch 后一律成功的旧逻辑。无保存服务栈时
调用现有 bulk_start，按报告提示；所有完成或异常路径刷新服务和栈缓存。栈报告显示成功、
已在目标状态、失败及未安装/不可用项，有缺失项也不能提示全成功，重试保持原栈/原集合。
“全部停止”确认时固定服务集合，包含报错但有 PID 的服务；部分失败保留弹窗和逐项报告，
重试只提交失败 id。IPC/请求级错误显示在弹窗中并保留上一份报告，不假定原操作全部未执行。

批量弹窗使用同一结果组件，失败优先于成功标记，分别统计真实执行、已在目标状态与失败。
结果提供对应服务日志入口和“重试失败项”；重启说明先停止后启动，不把单个排序冒充两个
阶段的全部执行顺序。使用同步 ref 防重复提交，执行时锁定勾选、快捷选择、清空和关闭，
Esc 也不能提前关闭；关闭后焦点返回批量入口。正文独立滚动，错误/长路径换行，底部操作
保持可见。服务栈小屏操作换行，停止按钮在 Error + 活跃 PID 时仍可用；固定版本不存在时
不显示另一版本的状态。默认无版本服务的“使用中版本”状态映射仍需进一步统一。

网页预览批量命令按命令名区分动作，真实更新内存服务状态，逆序停止、正序启动并记录失败，
不再直接返回全成功；未知服务、重复 id、过渡状态和停止失败后的重启符合批量规则。
这只是预览适配层行为，不能据此声称本机服务或完整 Tauri IPC 已验收。

验证：已有 bulk.rs 源文件模块 15 项通过，包含去重、未注册服务、过渡状态不假成功；
stacks.rs 已有模块 1 项通过，包含固定版本缺失与通用/固定 id 映射去重。断言均在已有源文件，
没有创建测试文件。真实 Redis 5.0.14 隔离生命周期用例最终通过，新增验证认证停机失败后
批量停止/重启/整栈停止均准确失败、原 PID 保留、同一服务只出现一次结果；原有凭据、统计、
RDB、重启与端口稳定验证也通过。中间一次复核因旧临时源文件缺失在复制阶段中止；恢复
项目清单中的官方 Redis 发行包后重新完整通过，SHA-256 为
018ea18a35876383cbb5f4cd0258adfc87747cf9d619bce1cf73a2e36f720ccf。
新隔离验证源保存在系统临时目录 niceenv-bulk-validation-20260926/runtime，自建进程已回收。
桌面 cargo check、最终 pnpm --filter @nsb/web check、git diff --check 通过，未运行前端 dev/build。

浏览器独立上下文验证：Nginx 停止成功、Redis 停止失败时只重启 Nginx；重试失败项只请求
Redis；慢请求双击只发一个批量命令，执行中勾选/快捷选择/关闭锁定。总览与命令面板的全部
停止保留准确部分失败报告，连接中断后报告仍在，恢复后只重试 Redis。无保存栈的快捷启动
失败不报全成功，服务栈缺少指定版本显示缺失项。报错但 PID 存活的整栈停止仍可重试。
320×568、390×844、1440×1000，中英文、深浅主题已检查；320px 弹窗左右各 12px，长错误无
横向溢出，底部按钮在弹窗内可见。受控页面无 pageerror，响应替换只存在验证上下文内存。
截图位于 artifacts/site-improvements-20260926：bulk-partial-dark-320.png、
bulk-partial-light-390.png、bulk-partial-en-320.png、stop-all-partial-desktop.png、
stop-all-error-dark-320.png、stacks-controls-320.png。

本轮没有用户数据库或表结构变更，未修改 update.sql；未操作用户 hosts、系统代理或 FlyEnv
文件，未安装依赖。完整桌面 IPC、跨平台、多版本服务栈状态匹配及其他套件链路仍需继续，
整体目标保持进行中。累计修改尚未 commit/push/发布，版本仍为 0.2.5；本轮未重新核对远程。
提交/push/发布必须同步版本号、新建并推送 annotated tag 的根 AGENTS.md 规则继续有效。

本轮自建 qa=bulk-14 浏览器上下文已关闭，原始 /sites 页面保留，已发送桌面通知。


第十五轮实施与验证：统一服务栈的多版本规则、运行统计与编辑保存。
修复第十四轮遗留的版本映射问题：无版本服务 ID 按实际“使用中版本”解析，没有选择时
使用最新已安装版本；完整 service ID 精确匹配，固定版本缺失不会替换成其他版本。
前端卡片、预览适配层和托盘统计与后端执行采用同一规则，运行数量按最终实例去重，
缺失项仍计入总数，避免缺少服务时显示全运行。后端解析适用于所有按版本注册的服务。

编辑器按服务家族添加，支持“跟随使用中版本”与固定已安装版本，卡片显示规则和最终版本。
缺失的固定版本保留原配置与警告，可改选已安装版本；同家族允许多个版本，禁用重复 raw ID。
顺序可用键盘操作上下移动，保存后重开仍保持规则与顺序。编辑内置预设保存为用户副本。
编辑项使用独立且稳定的本地 React key，切换 service ID 不再重建下拉框或丢失键盘焦点，
此 key 不写入保存数据。弹窗保留淡入淡出、取消缩放，解决快速打开版本下拉时浮层尺寸
测量产生的 ResizeObserver 循环；没有改动共用下拉组件或全局动画。

保存、复制、删除通过同步锁防重复请求，保存期间禁止修改或关闭（包括 Esc），失败保留
输入、规则、顺序与内联错误；删除失败保留确认框，成功后才移除卡片。弹窗正文独立滚动，
长错误限制高度并换行，底部操作保持可见。读取服务或套件信息失败显示明确错误和重试，
以 —/— 表示尚不能确认的状态并禁用启停，不把未知状态说成尚未安装或全部运行。
后端新建/复制 ID 加随机后缀，连续复制不覆盖同毫秒创建的配置；保存已被删除的栈返回
STACK_NOT_FOUND，拒绝旧编辑器重新创建已删除配置。没有新增字段、API 或数据库表。

预览适配层的 PHP 服务和站点引用改用完整版本号；已注册且不在当前清单中的历史安装
版本补回包列表，使用实际安装版本和现有包模板的展示信息。没有编造历史版本下载地址、
SHA-256 或镜像，默认 active 按已装版本选择。预览保存/删除也校验空配置、内置预设和
不存在的 ID。浏览器操作仅作用于独立预览内存，不当作真实本机多版本服务已验收。

验证：cargo test -p nsb-core --lib stacks::tests --locked --offline -j 1 的 3 项通过；
cargo test -p nsb-core --test core_tests stack --locked --offline -j 1 的 3 项通过。
覆盖固定版本缺失、默认版本改变后的解析与统计、重复实例、规则持久化、连续 32 次复制
以及删除后的过期保存。新增断言位于已有 Rust 源文件测试模块，未新增测试文件；状态验证
仅借用测试进程 PID 判断存活，不启动/结束该进程。桌面 cargo check 通过，最后 UI 修改后
pnpm --filter @nsb/web check 与 git diff --check 通过，未执行前端 dev/build 或安装依赖。

独立浏览器验证：通过受控调用切换 PHP 使用中版本并重新查询，默认栈显示/启停目标一致；
固定 7.4.33 的栈不随默认选择变化，停止调用准确指向 php@7.4.33，8.3.17 保持运行。
固定缺失版本报告未安装，不调用另一 PHP 实例；通用与固定 ID 指向同实例时显示 1/1，
启停各只执行一次。内置栈另存副本、重开保存内容、修复缺失版本、键盘选择和排序通过。
保存慢请求双击只发送一次，Esc/输入锁定；失败重试保持相同数据。删除慢请求防重复、
失败保留、重试成功通过。初始 packages 读取失败显示 —/—，点击重试后恢复真实预览统计。
320×568、390×844、1440×1000、中英文和深浅主题已检查；320px 编辑器左右各 12px，
宽度和 scrollWidth 均为 296px；英文版本下拉无横向溢出。修复焦点与动画问题后最终流程
未出现受控页面错误。错误注入、调用记录和尺寸观察仅在验证上下文内存，没有生产调试入口。
截图位于 artifacts/site-improvements-20260926：stack-editor-error-dark-320.png、
stack-version-select-light-390.png、stack-delete-error-320.png、stack-read-error-320.png、
stack-version-cards-desktop.png、stack-version-select-en-320.png、stack-editor-light-390.png、
stack-editor-error-en-dark-320.png，已目视检查。

本轮没有用户数据库或表结构变更，未修改 update.sql；未操作用户 hosts、系统代理或
FlyEnv 文件。真实多版本服务生命周期、完整 Tauri IPC、跨平台及其余套件仍待继续验收，
整体目标保持进行中。累计功能修改未 commit/push/发布，版本仍为 0.2.5。
已重新核对远程：main 与 annotated tag v0.2.5 均指向 81efd9f，tag 对象为 24c76a9。
根 AGENTS.md 已持久记录强制发布规则：用户要求提交/push/发布新版本时同步版本号，
新建并推送 annotated tag，不覆盖旧 tag；这次核对不代表新的构建或安装包已经发布。


第十六轮实施与验证：补齐套件默认版本的真实入口与 PATH 联动。
重新参考 ServBay 官方文档，核对默认 CLI 版本与多版本服务运行状态的区别：
https://support.servbay.com/basic-usage/set-default-cli-version
https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management

发现 PHP/MySQL 多实例版本下拉只能启停，无法选择服务栈所跟随的默认版本。现为多实例
已装版本增加独立“设为默认”按钮，默认标记与运行状态分开，纯运行时也使用默认标记。
服务栈跟随模式的中英文文案同步改为“跟随默认版本”。沿用 set_active_version 命令和
已有 active{id}Version 设置，无新 API、模型字段或表。操作后重新查询套件、服务、服务栈、
默认数据库和 PATH，桌面命令同时刷新托盘。默认切换不会启停多实例服务或修改站点版本绑定。

版本下拉加入同步防重复锁，执行中锁定选择、搜索、卸载和关闭；失败保留搜索内容，显示
内联错误与重试。默认版本已保存但 PATH 同步失败时，核心返回明确的部分完成错误和下一步
提示，不再吞掉同步错误而报告全成功。前端无论成功或失败都读取实际状态，仍可通过错误区
重试已经保存的同一版本。单实例运行中禁止切换；停止后等待缓存更新完成再恢复按钮，修复
紧接着切换时被过期运行状态拦截的问题。Error 且持有 PID 的实例仍显示停止操作，启停中
禁用版本行操作。下拉移除不必要的 layout 测量动画，尺寸受可用视口约束，正文独立滚动，
错误与说明换行；分隔线保留虚线及左右各 12px。修复未知安装包大小为 0 时冒出裸“0”。

PATH 行按钮原先把 selected 偏好当成已生效，总开关关闭仍显示已加入，首击反而移除选择。
现在按 enabled + selected + 实际 inPath 显示，提供包名/版本和 aria-pressed；关闭时首次
加入先只选择当前包，再打开总开关，不把其他默认勾选的包同时加入。加入/移除防重复提交，
按返回状态更新缓存并等待重新读取，失败也刷新；移除一个包保留其他选择。核心 PATH 修改
门面复用生命周期锁，避免和默认版本切换互相穿插。没有增加依赖或修改锁文件。

验证：已有 ops.rs 源文件测试模块新增 2 项，cargo test -p nsb-core --lib default_version_
--locked --offline -j 1 通过；验证 PHP/MySQL 默认选择持久化、唯一 active、目标 PATH 目录、
存活 PID 保留、站点绑定不变、未安装版本拒绝，以及运行中 Nginx 单实例切换拒绝/同版本重试。
验证仅使用临时 SQLite 和临时目录，PATH 总开关关闭，仅借用测试进程 PID 查询存活，不执行
启停或修改系统 PATH。现有 pathenv::tests 10 项全部通过，覆盖 PATH 合并、仅清理托管条目、
幂等、大小写和尾斜杠等。cargo check -p niceservbay --locked --offline -j 1 通过；最后改动后
pnpm --filter @nsb/web check、git diff --check 通过。未新增测试文件、未执行前端 dev/build。

独立浏览器实际点击“设为默认”将 PHP 8.3.17 改为 7.4.33：服务栈立即显示/统计所选实例，
服务状态和站点绑定不变，PATH 预览目录跟随更新。慢请求双击只发一个命令，Esc 不能关闭；
保存失败保持原默认版本与搜索内容，重试恢复；版本保存后 PATH 失败准确显示部分完成并可
再次同步。Nginx 运行中切换在前端拦截，停止完成后立即切换成功，调用顺序准确且不自动启动。
PATH 首次加入只提交 php，失败不误显示开启；随后加入 mysql、移除 php 时 mysql 保留。
上述 PATH 写入、错误和服务切换均作用于受控浏览器预览内存，未写本机注册表或 shell 配置。
320×568、390×844、1440×1000，中英文、深浅主题、键盘 Space/Esc、搜索空结果均已检查；
320px 下拉左右各 12px、clientWidth 与 scrollWidth 均为 296px，无横向溢出，最终页面无
受控 pageerror。截图已目视检查，位于 artifacts/site-improvements-20260926：
default-version-failure-dark-320.png、default-version-path-failure-320.png、
default-version-desktop.png、default-version-light-390.png、default-version-en-320.png。

本轮主要修改 packages/page.tsx、version-picker.tsx、path-env-toggle.tsx、i18n.ts，核心
lib.rs/ops.rs 和桌面 lib.rs。没有用户数据库变更，未修改 update.sql；未改用户 hosts、
系统代理、FlyEnv 文件或生成文件。完整桌面 IPC、真实系统 PATH 写入失败、跨平台和全部
套件验收仍需继续，整体目标保持进行中。累计修改未提交发布，版本仍为 0.2.5；发布时
同步版本号并新建、推送 annotated tag 的强制规则继续有效，本轮未创建 commit/push/tag。


第十七轮实施与验证：安装中心读取失败、批量操作报告和卸载恢复。
沿用前轮已核对的 ServBay 套件管理参考，复用现有 bulk_start/bulk_stop、BulkResult、
ConfirmDialog 和 pathenv_reapply，不增加 API、依赖或数据表。

套件与服务首次读取未完成时展示加载；读取失败展示来源与“重新读取”，保留已有缓存，
暂时禁用安装、切换、卸载和 PATH 按钮。服务状态不可用时已装版本明确显示“状态未知”，
不把缺少数据解释为已停止。查询恢复后继续操作，搜索和分类保留。320px 下错误与重试按钮
上下排列，避免文字被挤窄；弹窗内也可重新读取，无需退出确认流程。

安装中心全部启停改用真实批量 API，确认时固定本次目标、显示服务清单与范围说明：
面向全部已安装套件服务，不受搜索/分类影响；单实例去重，多实例按已装版本处理。
全部停止包含 Error 但仍有 PID 的套件服务，不混入站点独立进程或工具临时服务。
结果按依赖顺序列出成功、已处于目标状态、失败及原因/日志入口。部分失败保留弹窗，
重试仅发送失败项，并保留之前的成功记录；请求整体失败也保留已有报告。
部分失败直接展示弹窗报告，不再重复弹出会遮住小屏底部重试按钮的 toast。
同步 ref 防连续点击，处理期间禁止关闭，等待实际状态刷新后解锁。启停不刷新上游目录，
仅安装/卸载后刷新目录及相关套件、服务、PATH、服务栈和数据库查询。

卸载确认补充内联错误、同步防重复锁、安装中版本保护与等待刷新。运行时已卸载但 PATH
同步失败时，核心返回 UNINSTALL_PATH_SYNC_FAILED，明确“已卸载，PATH 待清理”。
前端保留独立清理状态，后续重试只调用 pathenv_reapply，即使清理再次失败也不会重做卸载。
桌面卸载命令成功或部分完成后刷新托盘。版本下拉移交卸载弹窗时不抢焦点，关闭或取消后
把焦点送回对应套件版本按钮；Space 打开后初始焦点落在取消按钮。

核心引用检查修复 MySQL 固定绑定漏洞：启用的站点数据库绑定若指定版本，无论是否有其他
MySQL 版本都阻止卸载该版本；跟随默认仅保护最后一个版本，禁用绑定不阻止。浏览器预览
的同一逻辑同步修正。已有 install.rs 测试模块增加隔离断言，无新测试文件。

验证：cargo test -p nsb-core --lib uninstall_ --locked --offline -j 1，5 项通过、
1 项按原设置忽略（需 NSB_NGINX_ROOT 的真实 Nginx 检查）。覆盖精确 MySQL 绑定、跟随默认、
禁用绑定、安装记录/目录保留、依赖保护、默认回落、安装互斥及自有短暂进程清理。
验证只使用临时 SQLite、临时目录及测试自建进程，不改用户数据或系统 PATH。
cargo check -p niceservbay --locked --offline -j 1、pnpm --filter @nsb/web check、
git diff --check 通过。未启动前端 dev，未执行前端 build，未新增依赖或修改锁文件。

独立浏览器预览验证初始读取失败/重试、缓存保留与状态未知、单实例去重、搜索不缩小全部
操作范围、Error+PID 停止、批量部分失败、请求失败报告保留、仅重试失败项、双击防重和
忙碌期间 Esc 锁定。卸载普通失败保留目标、PATH 部分完成后的列表更新及清理失败再重试、
固定 MySQL 绑定拒绝也已验证。PATH 写入与错误为浏览器内存模拟，不代表原生系统写入验收。
验证脚本中的过渡期选择器和不完整站点样例已纠正，最后两个独立 context 的页面错误记录
均为空；脚本只在浏览器 context 内存，没有写入生产调试入口。

目视检查 320×568、390×844、1440×1000，中英文、深浅主题、长错误换行、滚动正文和固定
底部按钮。320px 弹窗 clientWidth 与 scrollWidth 均为 296px，页面无横向溢出。
验证批量弹窗关闭回到原按钮，卸载弹窗关闭回到对应版本按钮。
截图在 artifacts/site-improvements-20260926：packages-read-error-320.png、
packages-bulk-failure-dark-320.png、packages-uninstall-error-320.png、
packages-uninstall-path-retry-320.png、packages-uninstall-mysql-protected-320.png、
packages-bulk-confirm-en-320.png、packages-bulk-error-en-320.png、
packages-bulk-error-en-light-390.png、packages-bulk-start-desktop.png。

本轮主要修改 packages/page.tsx、version-picker.tsx、path-env-toggle.tsx、i18n.ts、mock.ts、
核心 install.rs/lib.rs 与桌面 lib.rs。本次没有用户数据库变更，未修改 update.sql。
真实桌面 IPC、原生 PATH 清理失败和跨平台/全部套件仍待继续验收，整体目标保持进行中。
累计开发修改未提交发布，版本仍为 0.2.5。已核对远程 main 与 annotated tag v0.2.5 对应的
提交均为 81efd9f860590e84a75cd33f9dc93370744235be；tag 对象为
24c76a93d9b76b4a3aa5f9d6043dfbe36cb63aaf。根 AGENTS.md 的强制发布约定有效：提交/push/
发布时同步版本号并新增、推送 annotated tag，不移动旧 tag。本轮未创建新 commit/push/tag。


第十八轮实施与验证：本地证书生命周期、根 CA 真实性、导出保护和证书页交互。
参考 ServBay 官方本地根证书管理、SSL 故障排查、自签证书和站点 SSL 使用说明，保留项目
30 天本地证书策略。使用已有 rcgen、x509-parser、rustls、tempfile 和导出依赖，不新增依赖。

本地 CA 列表从真实 PEM 读取名称和有效期；Windows 按当前 CA 指纹查询用户/机器 Root
证书库，避免同名旧 CA 被当成当前根已信任。导入信任后重新检查结果。CA 文件缺一份、
证书私钥不匹配、CA 过期/未生效均明确报错并保留文件；两份 CA 都丢失但仍有站点证书时
拒绝自动换根，提示恢复原备份。macOS 信任检查调整为指定证书链校验，尚未原生验收。

签发统一规范化主域名、SAN 和 IP，去重，拒绝协议、端口、路径、不合法标签及无效 IP，
限制数量，并通过现有托管路径检查防止写到链接目录。证书、私钥和记录作为同一次操作
保存，写文件或入库失败时恢复旧内容。此处是进程内失败回滚，不保证双文件在掉电时原子
替换。修复阈值调整为 7 天，检查实际文件、私钥匹配、SAN 覆盖和有效期；用当前 CA 作为
唯一信任锚，通过 rustls 验证站点证书的真实签名链，不能只比较名称或记录。

桌面签发/修复入口共用生命周期锁，手动重签保留同主域名 HTTPS 站点需要的 SAN；签发后
重载受影响的运行站点。重载失败返回 CERT_RELOAD_FAILED，明确文件已更新、服务未加载，
不把这类部分完成误报为全部失败或成功。Windows 服务重载可能短暂中断连接。
根证书列表、签发、信任、修复、删除及证书体检等涉及磁盘/进程的相关桌面入口使用
blocking worker，避免同步阻塞界面。真实 Nginx 握手返回新证书仍待后续隔离验收。

原“吊销”占位按钮改为真实“删除本地证书”。只允许删除本地 site 证书，阻止删除 CA、
ACME 证书和仍被 HTTPS 站点引用的证书。删除先暂存证书/私钥，再删除记录；失败恢复文件，
无法恢复则保留临时恢复目录并提示。文案说明已导出副本不受影响，不冒充公有 CA 撤销。

PFX、JKS、PEM、DER 导出与本地证书写入使用同一文件锁。导出禁止写入托管证书目录、
覆盖已登记的证书/私钥；先写临时文件再替换输出，避免截断现有文件，也避免经外部硬链接
改坏原证书。目录创建失败如实返回，JKS 校验至少 6 个字符，PEM 拼接保留正确换行。
这不代表 ACME 写入已全部纳入同一锁；ACME 生命周期仍待继续审查。

证书页和体检卡显示加载、读取失败和重试，保留缓存但标记状态未知，不把失败显示为空
列表或全部正常。签发、重签、删除、信任与原生文件选择增加同步防重复锁；处理期间禁止
关闭，失败保留输入/目标和内联错误，相关查询刷新后解锁。导出格式改成两列短按钮和独立
说明，正确切换密码要求；浏览器预览明确桌面文件操作边界。ACME 卡跳转自动签发页签。
日期跟随中英文设置，导入证书删除按钮在触屏可见，长名称截断，根 CA 未知时不显示绿色
可信图标。共用确认弹窗标题避开关闭按钮，长名称换行，窄屏操作按钮可换行。

修复了带自动聚焦输入的签发/导出弹窗丢失原触发按钮的问题：显式保存入口，取消和关闭
回到原按钮；删除成功后原按钮消失则回到“签发站点证书”。重签/删除成功等待刷新和解锁
后再关闭，避免焦点落到禁用按钮。验证了键盘 Enter 打开、Esc 关闭和忙碌期间 Esc 锁定。

验证通过：cargo check -p nsb-core、cargo check -p niceservbay，均使用 --locked --offline
-j 1；cargo test -p nsb-core --lib local_certificate_tests --locked --offline -j 1，7 项通过。
覆盖非法域名不落盘、SAN 去重、CA 缺失和不匹配时保留、真实日期/指纹、入库失败恢复、
修复幂等、HTTPS 引用保护、不同同名 CA 签名不可互认、更换 CA 后修复、四种导出的真实
读回、错误密码、导出源文件保护与硬链接安全。只使用临时目录和临时 SQLite；未向系统
信任库写入证书。验证代码位于现有 Rust 源文件模块，无新增测试文件。
pnpm --filter @nsb/web check 和 git -c core.safecrlf=false diff --check 通过。

独立浏览器 context 验证首次读取失败/重试、缓存错误与未知状态、非法域名草稿保留、
规范化与重复 SAN、重签不重复卡片、重载部分完成提示、删除引用保护及成功删除、连续点击
只请求一次、信任失败重试、导出格式切换和关闭焦点。错误、信任和操作返回由浏览器内存
模拟，不等同真实桌面 IPC/UAC 验收。两个独立 context 的 pageerror 记录均为空。
检查 320×568、390×844、1440×1000、中英文及深浅主题；320px 弹窗宽度和 scrollWidth
均为 296px，无横向溢出。测试脚本曾遇到选择器大小写和热更新重置弹窗，纠正后已复核。
截图位于 artifacts/site-improvements-20260926：tls-initial-read-failure-320.png、
tls-reissue-partial-320.png、tls-delete-protected-320.png、tls-delete-en-dark-320.png、
tls-delete-long-en-320.png、tls-export-en-dark-320.png、tls-export-en-light-390.png、
tls-page-light-1440.png；主要弹窗截图已目视检查。

主要涉及 crates/core/src/tls.rs、桌面 lib.rs、TLS 页面、cert-health.tsx、misc.tsx、api.ts、
mock.ts 和 i18n.ts。本次没有用户数据库或表结构变更，未修改 update.sql，未运行前端 dev/
build，未改锁文件、用户 hosts、系统代理、信任库或 FlyEnv 文件。证书导入后端的解析与
私钥匹配、ACME、部署和监控尚未全面审查；整个 TLS 模块与整体功能目标继续进行中。
版本仍为 0.2.5，本轮未发布累计开发修改。再次核对远程 main 与 annotated tag v0.2.5
对应同一提交，根 AGENTS.md 已持久记录用户的强制要求：提交、push 或发布时必须同步
版本号并新建、推送 annotated tag，禁止仅推分支或覆盖旧 tag。

第十九轮实施与验证：第三方证书真实解析、私钥匹配和站点应用。
导入证书使用已有 x509-parser 读取 CN、DNS/IP SAN、有效期和 Unix 秒时间戳，并用 rustls
校验证书与私钥确实匹配；拒绝根 CA/中间 CA、缺少 serverAuth、损坏或超过 4 MiB 的文件。
导入先验证再落盘，使用随机 ID、临时文件、禁止覆盖和 Unix 0600 私钥权限，证书文件操作
与站点保存/删除共用锁，失败时恢复暂存文件。批量目录导入按子目录独立配对，fullchain
优先、私钥不重复使用、跳过符号链接和未配对文件，并限制扫描数量。

站点创建和编辑可选择已导入的第三方证书；保存前验证证书存在、可用、未过期、私钥匹配
且覆盖全部站点域名。选择导入证书时跳过本地 CA 自动签发，Nginx/Apache 配置引用受保护
的 `certs/imported/{id}.crt/.key` 路径；更换证书会更新配置并重载服务。导入证书列表保留
不可用项和具体原因，显示引用站点，正在引用的证书不能删除。证书体检改为从磁盘读取，
修复毫秒/秒单位错误，并把导入证书纳入有效期、文件、私钥、SAN 覆盖和建议检查。

桌面导入 IPC 使用 spawn_blocking；前端详情页与新建向导均增加证书来源选择，加载失败、
不可用证书、保存错误和触屏删除操作均有明确状态。浏览器 mock 补齐导入目录结果的 id、
usable、usedBySites 字段并统一 Unix 秒时间戳。浏览器验证了 390px 站点设置中选择
`*.corp.internal`、保存后重新打开仍保持选择，证书体检页显示导入证书；320px 页面
scrollWidth 与 clientWidth 均为 320px，无横向溢出。

验证通过：`cargo test -p nsb-core --lib --locked --offline -j 1`（377 项通过、13 项忽略）、
`cargo check -p niceservbay --locked --offline -j 1`、`pnpm --filter @nsb/web check` 和
`git -c core.safecrlf=false diff --check`。未运行前端 dev/build，未新增测试文件和数据库
变更，未修改 update.sql。ACME 自动签发、部署服务和真实 Nginx/Apache TLS 握手仍需后续
隔离环境验收；整体功能完善目标保持进行中。

本轮主要涉及 crates/core/src/certs.rs、sites.rs、configgen.rs、model.rs、health.rs、
桌面端 lib.rs、smoke.rs、证书相关站点组件、schema、api.ts、mock.ts 和 DESIGN.md。版本仍
为 0.2.5，本轮没有 commit/push/tag；用户强制发布约定继续有效：提交、push 或发布时必须
先同步版本号，再在发布提交上创建并推送新的 annotated tag，禁止只推分支或移动旧 tag。

第二十轮实施与验证：浏览器 mock 命令面和发布版本同步。
补齐浏览器预览中缺失的 `cancel_download`、DNS 接口查询/接管/恢复、日志导出、打开终端、
代理订阅切换/删除、文本文件读写和配置体检命令。安装 mock 现在保留可取消窗口并在取消时
返回 `CANCELLED`，代理订阅删除会保护当前激活项，hosts 应用、文本文件和 DNS 操作都会
更新内存状态，日志导出返回实际字节数，不再把按钮点击静默当作成功。浏览器版本展示与
桌面版本统一，诊断报告、更新检查和设置页不再回退到旧的 0.1.0。

本轮将版本升至 0.2.6，同步根 package、Web/桌面 package、工作区 Cargo、桌面 tauri 配置
和 Cargo.lock。验证通过：`pnpm --filter @nsb/web check`、`cargo check -p niceservbay
--offline -j 1`、`cargo test -p nsb-core --lib --locked --offline -j 1`（377 项通过、13 项
忽略）以及 `git -c core.safecrlf=false diff --check`。未运行前端 dev/build，未新增测试文件，
本轮没有数据库变更，未修改 update.sql。发布提交完成后必须创建并推送新的 annotated tag
`v0.2.6`，保留已存在的 `v0.2.5` 不动。

第二十一轮实施与验证：PHP 扩展真实状态与上游版本清单补齐。
PHP 扩展面板现在区分 PHP 运行时内置模块与 ext 目录中的可加载文件，内置模块显示说明并禁止
单独关闭；扩展切换、php.ini 快捷开关和依赖修复使用统一忙碌锁，避免并发点击互相覆盖。后端
通过真实 `php -n -m` 识别内置模块，扫描 Windows DLL/Unix SO，解析 ini 状态，按依赖顺序写入，
写入前备份并用 `php -c <ini> -m` 验证；启用后在 PHP 服务运行时自动重启，缺失文件、内置模块、
仍被其它扩展使用的依赖都会返回可理解的错误。浏览器 mock 与真实依赖关系、错误保护和内置模块
状态保持一致，扩展面板补充重新检测、依赖一键修复、部分失败明细和加载失败提示。

Windows 套件清单修订为 40，Nginx 保留历史版本并把 `1.31.6` 作为无版本查询的最新可安装版本，
补齐 Gradle、Neo4j、MariaDB、PostgreSQL、PHP 和 MySQL 条目的真实大小；新增安装测试锁定 Nginx
版本、大小和 SHA-256 字段，避免后续清单回退。应用版本同步升至 0.2.7，根 package、Web/桌面
package、共享 schema、工作区 Cargo、桌面 Cargo、Tauri 配置、Cargo.lock、浏览器版本展示和下
一版本演示值均同步更新。

验证通过：`cargo test -p nsb-core --lib bundled_nginx_prefers_the_latest_manifest_version
--locked --offline -j 1`（1 项）、`cargo test -p nsb-core --lib phpext::tests --locked
--offline -j 1`（17 项）、`cargo test -p nsb-core --test feature_integration php_extension
--locked --offline -j 1`（2 项）、`cargo check -p niceservbay --offline -j 1`、
`pnpm --filter @nsb/web check` 和 `git -c core.safecrlf=false diff --check`。未运行前端 dev/build，
未新增测试文件，本轮没有数据库变更，未修改 update.sql。提交和 push 时必须创建新的 annotated
tag `v0.2.7`，保留全部历史 tag，不覆盖或移动旧 tag。

第二十二轮实施与验证：数据目录迁移从占位提示变为可用闭环。
设置页的“迁移数据目录”现在使用桌面目录选择器，迁移前展示目标路径和影响范围，窄屏下路径与
操作按钮自动换行。后端拒绝当前目录、嵌套目录、软链接/目录联接、非空目标和重复迁移；确认后
检查安装/卸载任务，停止所有 NiceEnv 受管服务，先执行 SQLite WAL checkpoint，再把运行时、配置、
数据库、证书、日志、下载缓存和备份复制到同父目录的暂存目录，校验 `nsb.sqlite` 后原子改名提交。
复制失败会清理暂存目录而保留源目录，目标通过 `NSB_HOME` 传给新进程，桌面端随后自动重启；浏览器
预览明确提示该操作需要桌面版，不再把 toast 当作迁移成功。新增的路径测试覆盖完整复制、文件计数、
非空目标保护和源文件不被覆盖。

应用版本同步升至 0.2.8，保留 `v0.2.7` 不动。验证通过：`pnpm --filter @nsb/web check`、
`cargo check -p nsb-core --offline -j 1`、`cargo check -p niceservbay --offline -j 1`、
`cargo test -p nsb-core --lib paths::tests::data_dir_copy_is_atomic_and_requires_an_empty_target
--offline -j 1`（1 项）和 `git -c core.safecrlf=false diff --check`。未运行前端 dev/build，
本轮没有数据库结构变更，未修改 update.sql；提交时必须创建新的 annotated tag `v0.2.8`。

第二十三轮实施与验证：运行状态与窄屏反馈补齐。
DNS 接管在 Windows 上正确保留带空格的网卡名，使用 `name=<interface>` 传参，并检查
`netsh`/`networksetup` 退出码；工具页显示每个网卡的当前 DNS 配置，读取失败可重试，接管或恢复
后会重新读取状态。代理页、环境体检卡和总览异常卡不再把状态读取失败显示成关闭、无异常或全部正常，
均保留已有结果并明确标出错误和重试入口。服务日志在内存 ring 为空或应用重启后会从日志文件尾部
读取，历史日志页可以继续查看已落盘内容。

顶栏搜索框、日志服务选择和日志区域增加窄屏布局约束，320px 宽度下不再被固定双列或固定宽度
撑破。版本同步升至 0.2.9，根 package、Web/桌面 package、共享 schema、工作区 Cargo、桌面
Cargo、Tauri 配置、Cargo.lock 和浏览器版本展示均同步更新；保留 `v0.2.8`，发布时必须新增
annotated tag `v0.2.9`。

验证通过：`pnpm --filter @nsb/web check`、`cargo check -p nsb-core --locked --offline -j 1`、
`cargo check -p niceservbay --locked --offline -j 1`、`cargo test -p platform --lib --locked --offline
-j 1`（7 项）、`cargo test -p nsb-core --lib services::fallback_tests::tail_reads_persisted_log_after_process_restart
--locked --offline -j 1`（1 项）和 `git -c core.safecrlf=false diff --check`。未运行前端 dev/build，
未新增测试文件，本轮没有数据库变更，未修改 update.sql。

第二十四轮实施与验证：真实执行失败状态、工具箱生命周期和发布版本规则收尾。
ACME 自动签发等待真实 run_once 结果，DNS 验证、部署或 HTTPS reload 失败会返回明确错误；代理模式、
系统代理和 mihomo 订阅启停检查真实响应，停止前先关闭系统代理，避免留下指向已停止端口的系统配置。
计划任务执行移入 blocking worker，增加输出排水、30 分钟超时、kill 和失败状态，重复手动执行会被拒绝。
顶栏常用栈批量启停收集逐项失败原因，设置页恢复默认外观会报告未保存项。隧道读取失败显示重试，
cloudflared 启动后立即退出不再短暂显示为存活；Ollama 模型读取失败不再伪装成空列表。

版本同步升至 0.2.10，更新根 package、Web/桌面 package、共享 schema、工作区 Cargo、桌面 Cargo、
Tauri 配置、Cargo.lock 和浏览器版本回退值。保留 `v0.2.9` 及全部历史 tag，发布时创建新的 annotated
tag `v0.2.10`，不覆盖或移动旧 tag。验证通过：`pnpm --filter @nsb/web check`、`cargo check -p nsb-core
--locked --offline -j 1`、`cargo check -p niceservbay --locked --offline -j 1`、`cargo test -p platform
--lib --locked --offline -j 1`（7 项）和 `git -c core.safecrlf=false diff --check`。未运行前端 dev/build，
未新增测试文件，本轮没有数据库变更，未修改 update.sql。

第二十五轮实施与验证：启动链路真实结果与总览初始状态修复。
总览页在服务、站点和服务栈首次读取完成前显示加载状态，读取失败时保留错误提示和重试入口，快捷启动在状态未准备好前不可点击。PHP 版本启动后会按当前运行中的 PHP 池重建并校验 Nginx/Apache 配置；Web 服务重载失败会返回明确错误，Windows Apache 重启不再吞掉停止失败。Ollama 模型拉取等待真实命令退出，只有成功退出才显示完成，失败会展示命令输出摘要和重试建议；Tauri 端把阻塞拉取放入 blocking worker，避免冻结 UI。

版本同步升至 0.2.11，根 package、Web/桌面 package、共享 schema、工作区 Cargo、桌面 Cargo、Tauri 配置、Cargo.lock、浏览器版本展示和回退值均同步更新。保留 `v0.2.10` 及全部历史 tag，发布时创建新的 annotated tag `v0.2.11`，不覆盖或移动旧 tag。

验证通过：`pnpm --filter @nsb/web check`、`cargo check -p nsb-core --locked --offline -j 1`、`cargo check -p niceservbay --locked --offline -j 1` 和 `git -c core.safecrlf=false diff --check`。未运行前端 dev/build，未新增测试文件，本轮没有数据库变更，未修改 update.sql。

第二十六轮实施与验证：站点删除失败恢复与详情布局。
站点删除不再忽略配置、证书或 hosts 操作失败。后端先把配置和允许清理的本地主域名证书移动到数据目录内的暂存区，待站点记录、hosts 和 Web 配置生效后再清理；中途失败时尝试恢复文件与记录，无法完全恢复则保留暂存副本并报告恢复路径。导入、ACME、别名对应和仍被其它站点或自动化引用的证书保留；取消 hosts 清理会把域名保存为手动托管条目，后续重建仍保留。项目文件和业务数据库不受删除影响。暂存清理失败明确提示站点已经删除，避免误导重复操作。

详情页在删除期间锁定确认选项和关闭操作，失败保留选项并展示错误详情，同时刷新相关状态；证书来源移到 HTTPS 配置旁，读取失败提供重试，下拉分隔线沿用左右各 8px 留白和虚线。页脚按钮允许换行。代理停止使用后端已有的完整关闭流程，移除前端吞掉系统代理关闭错误的重复调用。浏览器 mock 同步删除选项和证书保护规则。

版本同步升至 0.2.12，发布约定明确必须创建并推送新的 annotated tag `v0.2.12`，保留所有历史 tag。同步根及工作区 package、Cargo、Tauri、Cargo.lock 中项目版本与界面版本展示。

验证通过：`pnpm --filter @nsb/web check`、`cargo check -p niceservbay --locked --offline -j 1`、`cargo test -p nsb-core --lib sites::scaffold_tests --locked --offline -j 1`（17 项通过、3 项忽略，包含 4 项删除回归检查）及 diff 空白检查。利用已有预览服务检查 1280×800、390×844、320×740 的详情、证书选择和删除确认布局，并确认取消清理选项后浏览器 mock 的 hosts 与证书保留。浏览器验证仅代表界面和 mock；真实系统 hosts 权限失败及运行中的 Nginx/Apache 重载仍需隔离环境验收。不启动新的服务，不执行前端 dev/build，不新增测试文件；本轮没有数据库结构变更，未修改 update.sql。整体功能完善工作仍在进行。

第二十七轮实施与验证：hosts 编辑与失败一致性。
对照 ServBay 官方 hosts / DNS 管理说明（https://support.servbay.com/basic-usage/dns/manage-local-hosts-file 、https://support.servbay.com/basic-usage/dns/manage-local-dns-service），补齐手动映射编辑，并区分站点、手动和只读系统条目。新增、编辑、删除、文本保存与文件导入共用操作锁；读取失败会保留已有列表并阻止写入，错误显示在当前操作区域。文本和导入支持同一行多个主机名、IPv6、行尾注释与双栈映射，格式错误明确标出行号并阻止整次写入，不再默默跳过。清空手动映射需要确认；站点记录会按站点配置重建，不能从编辑器改写站点 IP。导出仅包含可重新导入的手动条目，浏览器预览可生成真实下载文件。

后端校验所有托管 IP 和主机名，防止换行、协议、端口、路径和通配符进入 hosts。读取设置和站点失败不再当成空列表。系统写入失败时恢复应用内旧记录，恢复失败单独报告；编辑器提交读取快照，拒绝已发生变化的 hosts 内容，文本草稿保留最初快照，避免后台刷新后覆盖其它修改。hosts 操作串行处理，桌面命令放入 blocking worker，避免等待文件或站点删除操作时阻塞界面。环境体检比较具体 IP/域名映射，条目数相同但内容错误也会提示，读取失败明确标记无法检查。站点删除流程适配严格读取，并跳过无需 hosts 映射的 IP 字面量。

界面将 hosts 工具栏移入卡片内容区，按钮换行，表单增加固定标签与窄屏单列布局，长主机名和 IPv6 地址可换行，条目分隔线使用左右留白虚线。使用已有预览服务完成新增、编辑、删除、错误草稿保留、多别名、IPv6 双栈、取消清空及确认清空的交互检查，并检查 1280×800、390×844、320×740 布局；浏览器导出文件已核对只含手动映射，控制台无错误。

版本同步升至 0.2.13，提交时必须创建并推送新的 annotated tag `v0.2.13`。验证通过：`pnpm --filter @nsb/web check`、`cargo check -p niceservbay --locked --offline -j 1`、`cargo test -p nsb-core --lib hosts::tests --locked --offline -j 1`（5 项）、`cargo test -p nsb-core --lib sites::scaffold_tests::delete_site --locked --offline -j 1`（4 项）、`cargo test -p nsb-core --test core_tests hosts_ --locked --offline -j 1`（6 项）及 diff 空白检查。未运行前端 dev/build，未新增测试文件，没有数据库结构变更，未修改 update.sql。测试未改动真实系统 hosts；权限失败采用注入写入错误验证应用记录恢复，系统文件写入与桌面文件选择交互仍需隔离环境验收。上一版 v0.2.12 已核对 Windows / macOS 双架构构建成功且安装包已发布。整体目标仍在进行，解析记录暂停/恢复与 DNS 配置完整性等后续项目尚未验收。

第二十八轮实施与验证：DNS 接管、原配置恢复与真实启动检查。
接管前持久保存接口标识、自动获取模式及完整静态 DNS 地址顺序，恢复时还原原配置；取消授权或写入失败保留恢复记录，成功核验系统配置后才清理记录。自动停止 CoreDNS 前先恢复本应用接管过的接口，恢复失败保留解析服务；其它程序修改过 DNS 或接口被替换时拒绝自动覆盖。恢复记录留在本机现有 settings 中，配置导入导出均排除这些记录，没有新增表或字段。

Windows 使用结构化 PowerShell 读取接口与 IPv4 DNS，保留 IPv6 配置；提权参数正确处理空格、引号和末尾反斜杠，并检查被提权子进程的真实退出码，同一修复也适用于现有 CA 信任调用。桌面 DNS 命令放入 blocking worker，避免授权等待冻结界面。CoreDNS 配置写入启动参数实际引用的版本目录，校验域名后缀和上游地址，绑定本机回环；A 查询返回 127.0.0.1，其它查询返回空答案。启动前检查 UDP 端口，启动后用实际 DNS 应答及受管进程存活情况判断是否就绪，接管必须使用运行中的 53 端口。

工具页区分未安装、加载、读取失败、停止、运行和异常状态，展示当前配置及原配置，接管与恢复均需确认，失败保留弹窗和错误说明。DNS 卡片使用留白虚线、可换行按钮和地址。已有浏览器预览完成 mock 安装、启动、静态双地址接管与恢复、自动获取接管与恢复、停止时自动恢复和取消确认；检查 1280×800、390×844、320×740 布局，控制台无错误。浏览器验证仅代表界面与 mock，没有修改真实系统 DNS；真实 UAC、系统 DNS 写入及 macOS 行为仍需隔离环境验收。

版本同步升至 0.2.14，提交必须创建并推送新的 annotated tag `v0.2.14`，保留所有历史 tag。已完成 DNS 核心回归 7 项、平台 DNS 参数与配置比较 2 项、配置迁移 3 项；Web 类型检查、桌面 Rust 编译检查与 diff 空白检查均已通过。未启动新的服务、未执行前端 dev/build、未新增测试文件，没有数据库结构变更，未修改 update.sql。上一版 v0.2.13 已核对 Release 构建成功且安装包公开发布。整体完善目标仍在进行。

第二十九轮实施与验证：套件筛选、实际服务状态与日志跳转。
参照 ServBay 官方套件与服务管理说明（https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management），补齐套件中心的已安装 / 运行中筛选，搜索支持名称、ID、描述和版本号，可与大类及小类组合，并可一键重置。修复退出动画保留旧套件行的问题：搜索数量、可操作行和空状态现在同步变化，停止服务后会立即移出运行中结果。读取服务状态失败时明确提示无法筛选，计数显示未知并禁用启停，不能把读取失败显示成没有运行服务。

套件行按后端返回显示每个已安装服务的状态、版本、端口、PID 和错误信息，并可直接打开对应日志。未安装服务的端口明确标为默认端口；CoreDNS 描述使用实际设置的域名后缀，不再显示模板占位符。多版本状态与操作布局可换行，纯运行时说明不再误称所有套件都由 PHP 站点管理。日志页修复 URL 目标与默认选中项竞争，指定 Redis 或 PHP 版本时不再被覆盖成首个 Nginx；手动切换后轮询保持选择，补充选择状态与搜索可访问名称。

往返页面复现路由容器及标题停留在 opacity:0 的问题，去掉路由内容和共享标题的初始隐藏动画，由 Next 管理页面生命周期。保留既有留白、布局和其它交互动效。反复检查套件、工具与日志页面，标题及其祖先容器均保持可见。

版本同步升至 0.2.15，提交必须新增并推送 annotated tag `v0.2.15`，保留所有历史 tag。验证通过：`pnpm --filter @nsb/web check`、`cargo check -p niceservbay --locked --offline -j 1` 与 diff 空白检查。现有浏览器预览验证大小写及空格搜索、版本搜索、无结果、已安装与运行中筛选、分类组合、停止后移除、重置、指定服务日志与手动切换；使用临时查询状态注入核对读取失败和端口冲突，验证后恢复，未写入应用调试代码。检查 1280×800、390×844、320×740 布局，版本下拉分隔线左右各 12px 留白且为虚线；最终刷新后控制台无错误。浏览器操作仅使用 mock，没有启停真实系统服务。未运行前端 dev/build，未新增测试文件，本轮没有数据库变更，未修改 update.sql。上一版 v0.2.14 的 Windows / macOS 双架构构建成功且安装包已公开发布。整体目标仍在进行。
