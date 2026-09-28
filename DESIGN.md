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

第三十轮实施与验证：日志来源隔离、可恢复读取与真实导出。
参照 ServBay 官方日志管理说明（https://support.servbay.com/basic-usage/view-log-files）和 Apache 2.4 CustomLog 官方文档（https://httpd.apache.org/docs/2.4/mod/mod_log_config.html#customlog），完善服务及站点日志链路。日志数据按来源与行数隔离；暂停只停止自动轮询，首次、切换来源和手动重读仍可读取。延迟返回的旧来源数据不再覆盖当前服务。读取错误提供原因与重试，保留同来源上次成功数据并标明可能过期，首次加载、无日志与无匹配结果各有明确状态。警告筛选仅匹配警告，与计数一致。

复制失败不再显示成功，共享复制按钮只在写入剪贴板成功后显示完成。筛选导出与完整日志导出明确区分，提供防重复提交；浏览器下载实际演示日志文件，不再返回虚构本地路径。修复 mock MySQL 日志绑定旧版本 ID 的问题。完整导出支持站点，空日志、未知来源与读取失败有真实反馈。工具条可换行，按钮有可访问名称与选中状态；移除日志内容初始隐藏动画。日志页改为随内容增长，避免窄屏来源列表被压缩为零高度，320 / 390 / 1280 宽度均无横向溢出，来源列表与底部操作均可滚动访问。

核心从文件末尾分块读取，限制最多 20000 行及 8 MB，超限明确提示；不再每次轮询扫描完整大文件。不存在的日志视为空，目录、权限和读取错误传给界面及 CLI，非 UTF-8 字节替换显示且保留后续行。只接受已注册服务和真实站点，拒绝站点路径穿越；按照站点实际 Web 服务器选择路径。Apache vhost 增加独立访问与错误日志，访问日志读取和导出对应同一路径。旧诊断调用保留兼容读取入口，主界面及 CLI 使用可返回错误的接口，桌面磁盘操作放入阻塞任务线程。

筛选导出使用同目录临时文件和不覆盖保存，防止并发同名导出覆盖；999 个名字均占用时明确失败。完整导出按开始时的文件长度复制到临时文件，成功后替换目标，拒绝源目标同文件，硬链接目标也不会截断源日志，失败保留原目标。复用 tempfile，未增加依赖。

版本同步升至 0.2.16，提交必须新增并推送 annotated tag v0.2.16，保留历史 tag。验证通过：20 项既有 Rust 文件内的日志回归（15 项导出、4 项尾部读取及编码、1 项站点来源与配置），Web 类型检查、桌面 Rust 编译检查及 diff 空白检查。浏览器预览验证暂停切换服务 / 站点、延迟旧请求隔离、读取失败与重试、剪贴板拒绝、无匹配、下载文件内容和三个宽度布局；临时状态注入均已恢复，未添加应用调试入口。浏览器使用 mock，核心验证只使用临时目录，没有操作真实服务及用户日志；Apache 真实访问落盘和 macOS 文件保存尚未在运行环境验收。未启动前端 dev、未执行前端 build、未新增测试文件，没有数据库变更，未修改 update.sql。已确认上一版 v0.2.15 Release workflow 构建成功。整体完善目标仍在进行。

第三十一轮实施与验证：代理订阅正确适配、配置热重载与失败恢复。
修复逐行替换所有 port 字段导致节点远端端口被改为 0 的问题。使用 yaml_serde 解析 YAML 并展开 merge，只覆盖根层的托管端口、控制接口、secret、allow-lan 和代理模式，保留节点与 provider 的嵌套配置。接受标准 YAML、JSON 形式及 base64 编码的 Clash 配置，拒绝裸节点列表、网页、无效结构和重复键。YAML 错误只显示位置，订阅下载错误移除 URL，页面只展示订阅域名。下载限制为 HTTP/HTTPS、60 秒和 8 MB；名称、URL 和本地订阅 ID 均校验。新增直接依赖 yaml_serde 0.10.7，锁文件新增 yaml_serde 与 libyaml-rs，未升级其它依赖。

订阅导入、更新、激活与模式修改在已安装内核时先运行 mihomo -t 校验。激活与当前订阅更新使用官方 PUT /configs?force=true 热重载，无需先停止内核；磁盘配置、内核接受与选中记录都成功才返回成功，任一步失败恢复旧文件和运行配置，恢复失败明确报告。选中订阅使用现有 SQLite 事务，先检查目标再修改 active。下载完成后重新检查订阅记录和文件，避免覆盖期间发生的更新；删除拒绝当前订阅，并清理对应配置文件，记录删除失败时恢复文件。代理模式写入主配置和设置，启动时再次适配并校验，检查托管 PID、两个端口及原生 API。桌面耗时调用放入阻塞任务线程。

页面使用独立查询读取状态、订阅、节点和连接，显示加载、空状态、失败与重试，避免重叠轮询。错误时仍可停止已运行内核或关闭已开启系统代理。订阅动作互斥，删除失败保留确认框；导入有 URL 校验、忙碌锁和失败草稿，浏览器明确说明只做演示。停机选中的订阅显示启动后生效。订阅分隔线左右各 12px 留白并使用虚线，工具条、节点按钮和长名称可换行。

修复桌面节点类型字段 kind/type 不一致，返回值与前端 schema 统一。仅 Selector 组允许手动切换，按钮支持键盘，URLTest 等自动组禁用手动选择；浏览器演示真正更新选中节点。全部测速包含之前失败的节点，窄屏始终保留测速操作。连接读取失败保留上次数据并提示，实时速度显示未知，连接表格局部横向滚动。

验证通过：既有 Rust 文件内 9 项代理回归、2 项 YAML 适配回归，以及显式运行的 1 项原生 mihomo -t 回归；包括真实有限 HTTP 下载、无效更新保留原文件与选中项、热重载拒绝与记录失败回滚、恢复失败报告、URL token 隐藏、路径约束和节点 JSON 字段契约。原生验证使用官方 v1.19.31 二进制并核对 SHA256，仅执行 -t，没有启动长期代理服务或修改系统代理。Web 类型检查、桌面 Rust 编译检查及 diff 空白检查通过。浏览器验证导入、离线激活、模式选择、键盘节点切换、自动组禁用、失败节点重测、状态/订阅/连接错误注入与恢复，以及 320 / 390 / 1280 宽度；页面无横向溢出，连接表格按区域滚动。浏览器使用 mock，错误注入只存在于验证会话，结束后刷新清除。真实运行内核热重载及系统代理尚未做运行环境验收。

版本同步至 0.2.17，必须创建并推送新的 annotated tag v0.2.17，保留历史 tag。旧版已经保存成 0 端口的订阅需要点击更新重新下载，不能从损坏文件还原原端口。未运行前端 dev/build、未新增测试文件；未操作真实业务数据库或更改表结构，未修改 update.sql。上一版 v0.2.16 Release workflow 已成功。整体完善目标仍在进行。

第三十二轮实施与验证：计划任务真实执行、停止、编辑及中断恢复。
参照 ServBay 的服务状态与日志管理（https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management）以及 FlyEnv Laravel 调度器说明（https://flyenv.com/guide/run-laravel-use-flyenv.html），本轮完善已有应用级计划任务，不伪装成系统 cron。新增编辑定义、停止当前命令、常用周期预设与自定义周期、下次执行时间、删除确认。首次等待完整周期，之后按最近启动时间计算；页面说明最多 20 秒调度延迟、30 分钟运行上限及退出应用后的行为。停用自动调度不影响当前执行，停止命令也不撤销已发生的文件或数据变更。

修复先读状态后写 running 的竞争条件，使用现有 SQLite 写事务原子读取定义、复核到期/启用条件并占用任务；同一任务持有 OS 文件运行锁，跨连接和应用进程不会重复执行。运行时禁止编辑和删除，保存不覆盖上次结果及当前启用状态，未知任务的编辑/删除/开关返回错误。记录结果带本次开始时间校验，避免覆盖其它执行结果。启动和调度时仅恢复无人持锁的遗留 running 状态，标为 interrupted 并暂停自动执行，提示检查上次命令影响后再启用。ID 添加随机后缀，名称、命令与周期严格校验，非法周期不再悄悄改成默认值。

命令复用平台 ProcessGroup，停止、超时和应用退出清理命令树，shell 提前退出也清理持有输出管道的子进程。输出持续排空、每路最多保存 32 KiB，最终每路最多展示 4000 字符，截断明确标记；stdin 关闭，避免等待交互输入。Windows 原生验证发现带空格的脚本路径被 C argv 规则错误转义，改为 cmd /D /S /C 的原始命令传递，保留用户完整 shell 语法。输出优先解析 UTF-8，Windows 传统输出回退系统 OEM 代码页，中文输出可读。仅启用既有 windows-sys 的 Globalization 特性，未新增依赖或升级依赖版本。

调度仅由桌面应用启动，CLI/MCP 初始化与只读调用不再启动任务。退出、重启、安装更新与最终 Tauri Exit 均清理本进程任务。前端独立轮询去重，首次加载、空列表、读取失败与重试分开；已有结果在读取失败时保留并标明可能过期。失败退出不再弹成功提示，而是展开本次输出；停止请求和停止结果分开。新增/编辑有可见标签、防重复提交和失败草稿保留；控制区和长命令可换行，窄屏减少卡片嵌套留白，操作区使用留白虚线。浏览器明确声明不执行系统命令或自动调度，模拟编辑保留历史，并支持失败退出与停止状态。

验证通过：8 项既有 cron.rs 内的回归检查，涵盖真实 shell 成功/非零退出、到期自动执行路径、带空格脚本和参数、中文输出、两 SQLite 连接并发占用、运行锁恢复、编辑删除约束、停止后进程退出、超时不被输出管道挂住及输出上限。所有原生命令有限运行且使用临时目录，未启动新的长期服务。Web 类型检查、桌面 Rust 编译检查和 diff 空白检查通过。浏览器 mock 验证新增、编辑、运行成功/失败、停止、暂停调度、删除确认及取消、非法周期、保存失败保留草稿、读取失败保留列表与重试；320 / 390 / 1280 宽度无页面或卡片横向溢出，临时查询注入结束后清理。没有验收 macOS/Linux 原生进程树或打包桌面应用退出，20 秒常驻调度未在真实用户任务上运行。

版本同步至 0.2.18，必须创建并推送新 annotated tag v0.2.18，保留历史 tag。本轮不含隧道后续完善，整体目标继续进行。未运行前端 dev/build、未新增测试文件、未操作真实业务数据库或更改表结构，未修改 update.sql。已核对 v0.2.17 Release workflow completed/success。

第三十三轮实施与验证：快速隧道真实连接状态、站点选择与进程清理。
参照 ServBay 的 Cloudflared 与互联网访问说明（https://support.servbay.com/advanced-settings/how-to-use-cloudflared、https://support.servbay.com/advanced-settings/access-from-internet），补齐已有站点下拉选择及自定义 HTTP 端口入口。站点启动前重新读取真实记录与运行状态，使用当前 Web 服务的 HTTP 端口，并通过 --http-host-header 转发站点首个域名，避免落到同端口默认站点。启动前使用无代理、不跟随跳转的回环 HTTP HEAD 检查；拒绝不可达或非 HTTP 服务，有效 HTTP 错误响应本身不等于连接失败。不会自动修改网站自身的固定域名跳转。

不再仅凭输出中的公网 URL 宣告成功。对照 cloudflared 2026.9.3 官方 metrics/readiness.go 与 tunnel 源码，使用仅绑定回环的随机 metrics 端口；/ready 必须返回 HTTP 200 且 readyConnections > 0，并已取得有效隧道 URL，才显示 Cloudflare 已连接。区分连接中、已连接、重连中、失败、已停止；另外检查本地 TCP 端口可达性，不把云端连接等同网站业务可用。启动或断线 90 秒无恢复后结束进程并保留原因。公网 URL 解析排除 API 地址、非法子域与伪造后缀，metrics 地址仅接受回环有效端口。

按端口和 Host 串行去重，允许同端口不同站点；最多 20 条记录，回收已结束记录时保留活跃隧道。进程自行退出、启动监测失败和停止失败均保留诊断，不再静默丢记录。复用 ProcessGroup 管理进程组，停止成功需确认实际退出，应用退出、重启、更新退出路径均清理本应用隧道；清理等待有上限。停止后迟到的连接检查或超时不能覆盖状态。输出持续排空，单行读取最多 8 KiB，展示每行最多 2048 字符及最近 80 行，截断明确说明，非 UTF-8 不阻断后续输出。每次启动使用独立空 YAML 配置并清除子进程继承的 TUNNEL_* 参数，不修改用户现有配置或系统环境。桌面耗时调用放入阻塞任务线程。

原生检查发现当前官方 CLI 不支持 --logformat；修正为 --output json，并显式用校验 SHA256 的官方 Windows 2026.9.3 程序执行 --version 和完整参数加 --help，确认没有 Incorrect Usage（此版本错误参数也可能退出 0）。没有建立真实公网隧道。浏览器 mock 改为多条独立记录、去重、异步状态及停止/移除校验；明确提示只是演示，使用保留域 example.invalid，禁用打开示例链接。

前端隧道与站点独立查询，避免重叠轮询；首次加载、空状态、读取失败和重试分开，读取失败保留旧记录并标明可能过期，仍可停止已运行隧道。新增输出查看、重新创建与移除记录；操作防重复，站点重试重新解析当前目标，自定义端口重试不会误用表单中另外选中的站点。浏览器打开错误真实反馈。目标和端口有标签，未选择目标不能创建，未运行站点不可选；长域名、URL、错误及输出换行，按钮可换行，下拉及操作区采用留白虚线。

已通过既有 tunnel.rs 内 11 项回归，包括真实有限子进程启动/退出、并发去重、断线恢复、本地端口不可达、停止确认、迟到响应不覆盖停止、超时诊断、输出上限、真实回环 HTTP Host/重定向及 readiness 校验；另显式执行并通过 1 项官方 cloudflared CLI 参数回归。所有验证只用临时本机回环服务与有限子进程，不启动长期服务或暴露用户站点。浏览器预览验证创建、去重、停止、重开、移除、非法端口、停止站点禁选、读取失败保留操作、站点读取失败退回自定义端口、错误输出及重试，检查 320 / 390 / 1280 宽度均无横向溢出；临时查询注入结束后刷新清除，控制台无错误。真实 Cloudflare 公网连通、macOS 进程组和打包应用退出尚未在运行环境验收。

Web 类型检查、桌面 Rust 编译检查、版本一致性及 diff 空白检查均已通过。另修正既有 core_tests.rs 中代理适配测试的旧函数签名调用，并通过该项回归；没有改动代理业务逻辑。验证用子进程已退出。

版本同步至 0.2.19，必须创建并推送新的 annotated tag v0.2.19，保留全部历史 tag。未运行前端 dev/build，未新增测试文件；本轮没有数据库变更，未修改 update.sql。上一版 v0.2.18 的 Release workflow 已成功，Windows 与 macOS 双架构安装包已公开发布。整体完善目标继续进行。

第三十四轮实施与验证：Ollama 实例隔离、真实下载进度、取消与模型结果核验。
参照 ServBay 官方模型管理说明（https://support.servbay.com/ai/using-ollama），补齐常用模型选择、模型元数据、下载进度及本地管理流程。官方模型库已确认 qwen3:0.6b、gemma3:1b、llama3.2:1b 均可访问；界面明确这些是常用示例，支持自定义 namespace/model:tag 及浏览官方库，不声称是完整或最新模型目录。

移除依赖 CLI 输出列宽的列表解析及继承系统 OLLAMA_HOST 的操作方式，改用 Ollama 官方 GET /api/tags、POST /api/pull 和 DELETE /api/delete（https://docs.ollama.com/api/tags、https://docs.ollama.com/api/pull、https://docs.ollama.com/api/delete、https://docs.ollama.com/api/errors）。只连接 NiceEnv 当前托管进程的实际回环端口，核对运行状态、PID 与监听归属，拒绝存在其它进程监听的同端口；禁用 HTTP 代理与重定向。删除前检查本地模型，响应成功后再次核对模型已经消失；拉取完成也必须在原实例的本地列表中找到模型。失败读取不再变成空列表，成功响应但模型未落地也不会报成功。

拉取使用独立受管任务，前端请求立即返回任务记录，切换页面可恢复查看。按 NDJSON 分块读取，支持跨网络分块及无末尾换行；识别 HTTP 错误与 HTTP 200 内的 error，断流、缺少最终 success、非法 JSON 和过大行明确失败。展示当前文件的 total/completed，切换到校验和写入阶段时清除旧百分比，不把单个文件进度误称整个模型进度。单条进度最多 64 KiB，模型列表最多 4 MiB，错误与阶段文本有上限；普通请求最长 8 秒，拉取连续 120 秒无响应或总计超过 2 小时结束。

新增取消与重试；取消信号通过 watch 保留，即使工作线程尚未启动也不会丢失，并关闭本次 HTTP 请求。按 Ollama 官方约定，取消保留缓存，重试可续传，其它客户端共享的下载可能继续；不承诺删除已下载数据。应用退出、重启和更新退出均取消本应用拉取。拉取和删除互斥，后端拒绝重复任务及错误任务 ID，前端也防重复点击。删除失败保留确认框，模型名格式错误保留草稿。下载状态独立轮询，读取失败仍可取消已知任务，不会误开第二条下载；后台任务结果不依赖页面生命周期。

列表展示真实字节大小、修改时间、参数规模与量化方式，支持名称搜索、无匹配状态、复制名称、检查更新和删除。区分初始加载、空列表、失败、过期数据及任务终态。提供套件 / 服务入口，服务未启动时明确反馈，不自动操作其它 Ollama 实例。浏览器 mock 使用独立模型集合与任务，真实模拟增删、进度、取消、跨页面恢复和校验，明确说明不下载或删除电脑文件。窄屏下任务名称与状态换行，模型操作保持可见；下拉和操作区使用留白虚线，较长的下载与取消说明可展开查看。

验证：既有 toolbox.rs 内 7 项回归通过，使用有界回环 HTTP fixture，覆盖真实 NDJSON 分块、元数据解析、实例归属、服务停止、HTTP/流式错误、提前断流、超限响应、成功未落地、并发操作拒绝、取消后连接关闭、退出取消、空闲/总时限以及删除结果核验。没有运行真实 Ollama 服务、下载大模型或读取其它应用数据；真实 Ollama 缓存续传、推理调用及 macOS 运行行为仍待实际环境验收。浏览器完成预设选择、进度、取消、重试、跨页面继续、完成后列表更新、大小写搜索、无匹配、删除确认取消/成功/失败、非法名称草稿、读取失败保留列表与操作、任务读取失败仍可取消、长名称与长错误检查。检查 320 / 390 / 1280 宽度，Ollama 卡片无横向溢出；临时查询注入已刷新清除，控制台无错误。

Web 类型检查、桌面 Rust 编译检查、版本一致性与 diff 空白检查通过；收紧同端口多进程归属检查后，对应实例归属回归再次通过。

版本同步至 0.2.20，必须创建并推送新的 annotated tag v0.2.20，保留历史 tag。未运行前端 dev/build，未新增测试文件、依赖或迁移文件；本次没有数据库变更，未修改 update.sql。已确认 v0.2.19 Release workflow 构建成功，Windows 与 macOS 双架构安装包已公开发布。整体完善目标继续进行。


第三十五轮实施与验证：终端环境使用真实入口、脚本转义与 shell 配置保护。
参照 ServBay 命令行支持与默认 CLI 版本说明（https://support.servbay.com/basic-usage/command-line-support、https://support.servbay.com/basic-usage/set-default-cli-version），复用已有独立 PATH 版本选择和实际安装入口。终端脚本由后端只读快照生成，同套件只使用一个选定版本，正确处理 MySQL、Go、Nginx 等嵌套入口；总开关关闭时仍可生成当前会话脚本。不再由前端根据平台和包名猜目录，不再把所有已安装版本同时前置。读取记录或版本设置失败返回错误；所选版本卸载、入口缺失、越出安装目录及无执行权限均说明原因，不悄悄替换版本。

PowerShell 使用安全单引号字面量（包括弯引号）、局部脚本作用域和当前进程 PATH；Bash / Zsh 使用子 shell 和单引号字面量。重复执行去掉已选目录的旧重复项，其余 PATH 顺序、重复项和空段保留，临时变量不泄露。拒绝含 PATH 分隔符或控制字符的目录。没有写入系统 PATH、shell profile、服务默认版本或站点配置，也不声称实现项目级版本隔离。

shell profile 更新只替换完整同名标记块，保留孤立 BEGIN/END、嵌套标记前的用户内容、块外换行格式及无末尾换行文本；兼容旧产品名标记。托管路径转义反斜杠、双引号、美元符与反引号，读取时对应解码。Windows 打开终端直接启动系统 PowerShell 的可见独立控制台，工作目录经进程 API 传递，移除 cmd start 中转；检查目录和提前退出。macOS 检查 open 的退出状态，其它未支持平台明确返回错误。耗时桌面调用放入 blocking worker。

终端卡片新增加载、刷新、失败重试、空状态、不可用环境提示和版本目录预览，跳转到现有 PATH 选择卡片。说明复制粘贴后仅影响当前会话，打开终端不会自动注入。浏览器明确使用演示目录并禁用本机终端按钮，mock 打开终端返回桌面端限定错误。共享代码块在剪贴板拒绝时给出错误，复制按钮有可访问名称；卡片和代码工具条支持窄屏换行。

验证：既有 Rust 文件内 16 项 core PATH 回归与 10 项平台 profile 回归通过，其中新增 6 项终端及 3 项 profile 回归；使用受控 PowerShell / 已有 Git Bash 子进程验证特殊字符、字面路径、重复执行、原 PATH 保留与变量作用域，使用临时目录验证版本、实际入口和失效选择。没有新增测试文件或修改真实系统环境。浏览器预览核对加载、读取失败及重试、空状态、缺失版本提示、复制成功与剪贴板拒绝，320 / 390 / 1280 宽度终端卡片和代码工具条无横向溢出；临时注入已刷新清理，控制台无错误。未实机验收 macOS Terminal / Zsh 和打包应用新终端窗口，未运行前端 dev/build。

版本同步至 0.2.21，必须新增并推送 annotated tag v0.2.21，不移动历史 tag。Web 类型检查、桌面 Rust 编译检查、版本一致性与 diff 空白检查均已通过。浏览器另验证独立 PHP PATH 版本切换后脚本即时更新且排除其它版本，关闭持久 PATH 总开关仍保留当前会话脚本。已核对上一版 v0.2.20 Release workflow completed/success。没有数据库变更，未修改 update.sql；原有本地未跟踪文件保持未提交。整体完善目标继续进行。

第三十六轮实施与验证：配置体检真实诊断、版本目标与可重试报告。
修复已安装但运行目录损坏时被误报为未安装的问题。Nginx / Apache 按当前选中版本校验共享配置；PHP / MySQL / Redis 列出全部已装版本，报告包含具体版本、路径、检查方式及时间。安装记录读取失败明确返回错误；未安装标为未检查。MySQL / Redis 仅检查配置文件存在、为普通文件且可读取，界面明确说明这不能证明原生语法、数据库连接或业务正常。不改写配置，不启动服务。

PHP 通过 -c 明确指定 ini，并使用 php_ini_loaded_file() 核对实际加载路径，隔离额外 ini 扫描与 PHPRC，关闭此次校验进程的 prepend / append / preload 及错误日志写入。实测 PHP 8.4.26 的旧 -n -c ini -v 调用仍会报告 ini 语法错误，但退出码为 0；因此不能只凭退出码判断成功。新检查同时识别语法、启动及扩展加载诊断，普通警告单独显示，并说明 PHP CLI 通过不代表 Web 请求或站点覆盖配置已验证。

桌面体检在 blocking worker 执行，生命周期锁被占用时直接提示稍后重试。每个原生校验最多运行 15 秒，整体采用 60 秒检查预算，剩余未检查项明确提示单独重试。共用校验器改为持续排空 stdout / stderr，每路最多保留 64 KiB；超限、超时、输出不完整均不能报成功。主进程退出后清理其进程组，防止子进程持有输出管道导致长期等待；复用现有平台进程管理和编码处理，没有新增依赖。

修复向导提供可保留的逐项报告、仅看问题、单项重查、重试失败项和复制报告。失败时保留上次成功结果并聚焦错误说明，检查中防止重复操作。复用现有配置编辑和默认配置预览弹窗，按具体版本打开；缺失配置可预选目标生成后重查。报告分隔线为两侧有留白的虚线，操作按钮可换行。浏览器 mock 使用实际演示安装列表和共享配置内容，生成缺失文件后状态更新，不再制造不存在文件的备份；明确标注没有运行本机校验器。证书动作按实际返回数量反馈，零项时说明无需更新。

验证：既有 Rust 文件内 75 项相关回归通过，覆盖配置编辑、配置生成、PHP 扩展、代理及体检；另显式执行 3 项原生 PHP / Nginx 回归通过，使用临时目录及有限校验子进程。PHP 覆盖退出码为 0 的语法错误、缺失扩展、选定 ini 与不执行自动前置脚本；Nginx 覆盖选中版本、错误输出及原配置保留。浏览器 mock 完成缺失 Redis 配置生成后重试、指定 PHP 版本编辑后重查、请求延迟与拒绝、保留旧报告、错误焦点、复制成功与拒绝、无问题及全未安装状态。320 / 390 / 1280 宽度无横向溢出，临时状态注入均已刷新清理。原生 macOS 行为尚未实机验收；MySQL / Redis 的原生语法与数据库健康不在本轮检查范围。

版本同步至 0.2.22，必须创建并推送新的 annotated tag v0.2.22，保留历史 tag。Web 类型检查、桌面 Rust 编译检查、版本一致性及 diff 空白检查通过；刷新后浏览器控制台无错误。上一版 v0.2.21 Release workflow 已核对 completed/success。未运行前端 dev/build，未新增测试文件、依赖或迁移；本次没有数据库变更，未修改 update.sql。原有本地未跟踪文件保持未提交，整体完善目标继续进行。


第三十七轮实施与验证：诊断报告真实采集、敏感内容处理与快照导出。
参照 ServBay 故障排查工具的诊断范围与明确失败反馈（https://support.servbay.com/faq/troubleshooting），完善已有诊断报告链路。安装记录、站点或设置读取失败直接返回错误，不再生成“无套件 / 无站点”的误导报告；配置、日志、端口及证书的采集失败单独列出。端口表只读取一次系统监听列表，包含已注册服务实际端口；netstat / lsof 执行失败和带诊断的部分结果不能当作空闲，保留 lsof 无匹配且输出为空的正常退出语义。TCP 无监听不等同于 UDP 或其它地址可用，报告明确为采集时快照，不声称完整业务健康检查。

配置复用现有按版本目标列表，补齐各 PHP / MySQL / Redis 版本及 Apache 配置，记录具体路径；未安装、缺失、非法路径、读取错误、编码错误和截断都有说明。仅读取应用数据目录内的普通文件，拒绝外跳链接路径；每个配置最多读取 64 KiB，摘要最多 60 行且最多 24 份。服务日志复用可返回错误的历史读取接口，停止的服务也可收录；最多 20 个服务各 40 行，每份日志最多 64 KiB。报告保留服务错误、处理建议与技术详情，错误段放在服务表格之后，避免破坏后续行。总报告限制 2 MiB，超限明确提示分别导出。

敏感值完全隐藏，不再保留密码前两位；覆盖常见 env / ini / JSON / YAML 赋值、Redis 认证字段、授权与 Cookie 头、URL 凭据、带 token 的查询、多行引号或缩进凭据和 PEM 私钥。Windows 主目录处理正反斜杠、大小写及 JSON 转义路径，保持用户名边界。服务错误、站点文本与设置同样经过处理。界面移除“可直接公开粘贴”的绝对承诺，说明自动识别范围与分享前检查自定义日志、域名和项目路径的必要性，不将计数当成完整隐私检查结论。

生成和保存命令放入 blocking worker。保存接收当前预览快照，写入数据目录 diagnostics 内的独立临时文件后保留，拒绝空报告、过大报告和非法输出目录；重复保存不覆盖同一秒的旧报告。浏览器 mock 按当前演示服务、安装与站点生成报告，未采集的本机项目明确标注，统计与内容一致；下载真实 Markdown，移除返回虚构 Windows 文件路径的实现。沿用现有下载助手并保持日志导出文件名处理不变。

界面缓存已生成报告，跨页面返回可继续查看；生成失败保留旧报告并聚焦错误，生成与保存期间防止重复提交。复制、保存使用同一份快照，显示生成时间、采集说明和实际导出反馈。按钮允许换行，长错误与路径可换行，说明项使用有左右留白的虚线分隔。浏览器验证下载字节与预览一致、复制成功与剪贴板拒绝、生成延迟与失败、保存失败、重试和跨页面保留；320 / 390 / 1280 宽度及长错误无横向溢出，已检查 320 宽度截图，临时注入刷新清除后控制台无错误。

验证：最终版本的 20 项诊断单元回归与 2 项既有集成回归通过，覆盖常见凭据及多行私钥、路径替换、多版本配置、停止服务日志、采集失败、超限输入、预览快照保存一致性和同秒重复保存不覆盖。Web 类型检查、桌面 Rust 编译检查、版本一致性与 diff 空白检查通过。集成回归只读调用 Windows 端口查询；macOS 原生端口采集与导出尚未实机验收。

版本同步至 0.2.23，必须创建并推送新的 annotated tag v0.2.23，不移动历史 tag。已确认 v0.2.22 Release workflow completed/success。本轮没有数据库变更，未修改 update.sql；回归只使用既有测试模块、临时目录及临时 SQLite，未新增测试文件、依赖或迁移。未启动新的服务、未运行前端 dev/build，未修改真实系统配置或 FlyEnv 数据，整体完善目标继续进行。


## 第三十八轮：环境体检的真实范围、进程归属与失败反馈（v0.2.24）

继续参考 ServBay 故障排查范围（https://support.servbay.com/faq/troubleshooting），修复首页环境体检的错误归属、静默失败与误导状态。安装和站点记录读取失败直接返回错误；损坏的站点 JSON 不再退化成空域名、默认静态站点或丢失数据库绑定。端口方案和分配使用可报错读取，非法范围也明确提示。报告新增检查范围，区分已检查、未完成、未检查；已检查不等于通过，问题仍独立列出。严重问题优先于空环境引导，未完成项不会显示“当前可用”或“环境正常”。

端口表只采集一次，按已注册服务的实际运行版本与端口检查；停止服务使用当前端口方案或分配记录。逐一检查 PHP 池端口并防止溢出。监听归属按当前服务受管 PID 及父进程链判断，同名外部服务、另一受管服务和同端口多个监听者不能被误认为当前服务。缺失或循环的进程信息标记未完成，运行服务缺少监听报错；停止服务端口被占用只提示启动前处理，不声称当前服务已经故障。本轮覆盖已注册服务主 TCP 端口和 PHP 池，未覆盖 HTTPS 等附加监听、UDP、所有地址的可绑定性及业务连通性，报告明确显示此边界。

站点同时报告根目录、Web 服务器、指定 PHP 版本与代理地址格式问题，不再只展示第一个原因。安装目录不存在或无法访问单独提示。证书按每张实际状态报告文件缺失、无效、过期及即将到期，允许多种问题并存；补查 HTTPS 站点引用的证书不存在，按域名与证书标识匹配，避免同名站点掩盖证书缺失。未初始化且无人引用的 CA 不当成损坏；CA 信任提示仅作用于使用本地签发证书的 HTTPS 站点，不套用到 HTTP 或导入证书，并使用“尚未确认信任”措辞。

数据目录可写性使用唯一临时文件，写入、flush 与清理错误如实报告，保留用户已有 .health-probe。PHP 扩展扫描检查可执行文件和 php.ini，执行或目录枚举错误不能退化成空扩展列表；已启用但文件缺失或缺少依赖分别说明。扩展检查范围是内置模块、配置、文件和已知依赖，不宣称业务进程已成功加载。受管服务的未知、切换中和未注册状态标记未完成，已停止不视为故障；明确未运行原生配置语法与 HTTP / 数据库连通性检查。体检移入 blocking worker，通过已有服务生命周期锁拒绝与启停或其它体检重叠，避免采集混合状态。

首页体检使用共享查询缓存，重复触发和离开页面再返回复用在途请求；离线仍可发起本机体检。失败保留旧报告及检查时间，并区分刷新失败和首次读取失败。隐藏仅作用于当前报告，全部隐藏后仍保留恢复入口，重新检查恢复提示；空列表不再被一律显示成全绿。检查范围可用键盘展开，分隔线带左右留白与虚线，图标按钮有名称，长路径、提示与操作能在窄屏换行。浏览器 mock 根据当前安装、服务与站点生成结果，移除固定的虚假证书、hosts 和站点故障，并明确本机检查只在桌面端执行。

验证：22 项环境体检回归与既有空环境集成回归通过，既有站点数据库绑定持久化及密码不外泄回归也通过，覆盖同名外部进程、子进程归属、多监听者、其他受管服务、实际版本端口、端口池溢出、损坏数据库记录、证书问题并存、扩展缺失与执行失败、临时文件保护及服务操作互斥。Web 类型检查、桌面 Rust 编译检查通过。浏览器验证首次失败、刷新失败保留结果、重试、全部隐藏与恢复、刷新恢复提示、重复请求只执行一次、跨页面请求复用、离线检查、处理跳转和键盘展开；320 / 390 / 1280 宽度无横向溢出，检查了窄屏及桌面截图。临时注入已刷新清理，控制台无应用错误。macOS 进程归属与原生采集仍需实机验收。

版本同步至 0.2.24，创建并推送新的 annotated tag v0.2.24，保留历史 tag。上一版 v0.2.23 Release workflow 已确认 completed/success。本次没有数据库变更，未修改 update.sql；仅在既有测试模块使用临时 SQLite 和目录验证读取失败，没有新增测试文件、依赖或迁移。未启动新的服务、未运行前端 dev/build，未修改真实系统 PATH、hosts、DNS、代理或 FlyEnv 数据。本轮完成环境体检，单服务诊断与整体产品完善继续推进。

## 第三十九轮：单服务诊断与套件排查入口（v0.2.25）

继续参考 ServBay 故障排查流程（https://support.servbay.com/faq/troubleshooting），将单服务诊断改为桌面端统一采集。诊断只接受已注册的服务，重新读取服务快照与安装记录，不再依据卡片旧状态拼接结果；读取失败明确返回错误。复用服务生命周期锁，服务启停或其它检查进行中立即提示稍后重试。报告包含实际版本、端口、采集时间、每项状态与检查方法；采集结束检测版本、状态、端口和 PID 集合变化，提示结果可能过期。

端口复用环境体检的进程归属判断，覆盖服务主 TCP 端口及 PHP 池；其它进程监听不能当成当前服务成功。配置按服务快照的具体版本选择程序，避免默认版本变化后校验错误版本；Nginx / Apache / PHP 沿用原生有限时长校验，MySQL / Redis 仅检查文件可读性，未支持的检查明确显示未检查。运行进程存在不代表业务请求正常；未测试 HTTP 请求、数据库登录、HTTPS 附加监听和 UDP。

日志读取失败显示未完成，空日志或未匹配关键词仅提供信息，不再显示“没有异常”。扫描最近 300 行，命中只作为可能包含历史问题的线索，最多展示最近 5 条，每条限制 500 字符。先匹配原文再展示对应的脱敏行，避免隐藏凭据时抹掉错误关键词；复用既有凭据处理与内容长度限制。

套件页为每个已注册实例增加诊断入口，多版本 PHP / MySQL 可分别检查。弹窗使用按服务隔离的查询缓存，关闭重开复用在途请求，切换服务不被旧请求覆盖；重新诊断失败保留原报告和时间，离线仍可发起本机调用。提供日志页与修复向导入口，跳转前关闭弹窗；中间结果可滚动并支持键盘，底部操作始终可见。上下分隔线为虚线，窄屏两侧留白 16 px、桌面 20 px，长路径与操作按钮可换行。浏览器 mock 明确标注演示范围，本机端口和原生配置检查显示未完成，不编造通过状态。

验证：既有 Rust 测试模块新增的 9 项单服务诊断回归、原有 22 项环境体检回归与 2 项配置错误回归通过。另显式执行既有 Nginx / PHP 原生校验回归，通过真实有限子进程验证实际版本选择、无效指令、PHP 零退出码语法错误及缺失扩展；未启动长期服务。临时 TCP listener 验证受管 PID 归属，所有数据库破坏夹具仅作用于临时 SQLite。Web 类型检查、桌面 Rust 编译检查通过。浏览器验证首次失败、刷新失败保留报告、重试、重复请求合并、跨服务请求隔离、日志和修复跳转、Escape 焦点恢复及键盘滚动；离线重新诊断的查询更新计数递增且完成成功。320 / 390 / 1280 宽度无横向溢出，分隔线及留白已检查，临时注入已清除。macOS 原生采集及 Apache 原生程序仍需实机验收。

版本同步至 0.2.25，创建并推送新的 annotated tag v0.2.25，保留历史 tag。上一版 v0.2.24 Release workflow 已确认 completed/success。本次没有数据库变更，未修改 update.sql；未新增测试文件、依赖或迁移，未运行前端 dev/build，未修改真实系统配置或 FlyEnv 数据。保留原有未跟踪文件，整体产品完善目标继续推进。


## 第四十轮：端口查询、监听者归属与处理结果（v0.2.26）

参考 ServBay 排障工具的端口占用检查（https://support.servbay.com/faq/troubleshooting），统一工具箱端口查询、端口体检及启动冲突恢复。原范围查询仅按直接 PID 识别受管服务，直接结束端口可能处理未选中的其它监听者；原体检只看第一个 PID，结束命令返回 false 仍可能被界面当作成功。本轮改为逐一展示不同的 TCP 端口与 PID，合并同一进程的 IPv4 / IPv6 重复记录，并复用环境体检的父进程链归属判断。无法确认归属或读取身份时明确说明并禁用结束操作。

处理请求携带用户选中的监听者快照；执行前及每个动作前核对 PID、启动时间、进程名、命令行与服务归属。旧进程已经退出时不替换为新占用者，PID 或归属变化要求重新扫描。系统进程、NiceEnv 自身及其所在进程链不能从此处结束；受管子进程通过所属服务的停止流程处理，外部进程结束命令失败返回错误。结果保留逐项失败及剩余监听者，只有复查确认无 TCP 监听才显示端口已释放。其它地址的可绑定性、UDP 和业务连接不在本轮结论内。生命周期锁避免与服务启停并行；桌面端扫描移入 blocking worker。

端口体检依据已注册实例的实际主端口或停止实例的计划端口，包含各 PHP 池和多版本服务，删除未安装服务的固定占位端口。附加 HTTPS / mihomo 控制端口仍列出，但明确为计划检查，不声称当前一定启用了监听。多监听者全部参与判断，区分属于当前服务、外部占用、归属未知、运行中无监听及没有监听。非法设置、端口池溢出和系统读取失败不能退化成空闲。首页卡片、列表及错误提示的冲突恢复统一重新扫描原占用者，复查端口释放后才重试启动；自动释放设置也按实际结果记录释放端口。移除无身份校验的前端裸 PID 结束入口。

界面将单端口和范围输入分开，校验 1–65535，范围反向输入自动归一化；额外端口接受最多 32 个逗号或空格分隔值，常用及额外端口使用一次监听表扫描。扫描失败保留原结果和时间，提供重试；请求期间防止重复提交，离线仍可调用本机。处理确认展示端口、进程、PID 与所属服务，说明停止整个服务的影响；失败留在确认框，部分处理展示剩余监听者。体检行跳转到同一个监听者查询入口。分隔线使用左右留白的虚线，命令行可展开，长内容换行，窄屏操作排在内容下方，关闭确认框恢复操作焦点。浏览器 mock 按实际演示服务生成监听结果和停止反馈，明确未读取本机进程。

验证：9 项端口回归和原有 22 项环境体检回归通过，覆盖目标范围、PID 变化、受管子进程、多监听者、未知归属、保护对象、非法设置及互斥；通过真实临时 TCP listener 验证采集与未释放结果。另创建两个最多存活 30 秒的独立验证进程，实际结束选中的临时监听进程，确认另一个仍运行并监听，最终清理两个进程；未处理真实用户服务或进程。浏览器验证输入错误、反向范围、保留旧报告、重复请求只执行一次、身份变化拒绝、部分处理不报释放、离线请求、体检跳转及确认框焦点；320 / 390 / 1280 宽度无横向溢出，已目检桌面与窄屏，临时注入刷新清除。Web 类型检查、桌面 Rust 编译检查、版本一致性及 diff 空白检查通过。macOS 原生监听采集与结束流程仍需实机验收。

版本同步至 0.2.26，创建并推送新的 annotated tag v0.2.26，保留历史 tag。已确认 v0.2.25 Release workflow completed/success。本次没有数据库变更，未修改 update.sql；仅使用临时 SQLite 与既有源码内的测试模块，没有新增测试文件、依赖或迁移。未启动前端 dev、未执行前端 build，未修改真实系统配置及 FlyEnv 数据，原有未跟踪文件继续保留，整体完善目标仍在进行。

## 第四十一轮：服务重启、实际启动冲突与自动重启监控（v0.2.27）

统一桌面、CLI、PHP 扩展及 Xdebug 配置后的重启流程。停止和启动持有同一生命周期锁，连续操作立即返回忙碌提示，避免排队后执行过期请求或重启步骤交错。停止失败立即中止重启，保留原错误码与诊断字段并说明失败阶段；停止成功但启动失败单独说明。只有实际处于运行状态且仍有存活 PID 才确认启动成功，PID 文件与自动重启监控同步更新；未知服务明确拒绝。

桌面启动先执行真正的启动检查，只有收到实际 TCP 端口冲突且启用自动处理才尝试释放。只处理本次错误所指向的 PID，沿用监听者身份校验与系统进程保护；旧进程退出后不改为处理新的占用者。复查端口释放且没有处理错误后才重试，同一冲突不重复处理。重复启动已运行服务保持原 PID，不再先结束计划端口上的服务。已完成的端口释放通过回调保留，即使随后的启动失败仍可报告。策略失败同步服务错误状态，避免请求结果与卡片提示不同。

从端口工具停止受管服务后，同步记录用户主动停止，避免刚停就被 watchdog 拉起；Error 状态但仍有活进程或正在切换的服务不再消耗自动重试次数。浏览器演示启停也使用互斥和分阶段状态，未知对象不再返回成功，重复启动不换 PID，重启按停止、启动顺序执行。

验证：6 项服务生命周期回归、14 项既有 watchdog 回归和 9 项既有端口回归通过，共 29 项。隔离临时目录、SQLite 和最多存活 30 秒的临时监听进程验证真实冲突处理、受管服务停止、重启 PID 更换、PID 文件及 watchdog 状态同步；未操作真实用户服务。Node 内联脚本加载实际 mock，确认启动幂等、重复重启拒绝、状态转换、未知服务报错和停止清理 PID。浏览器通过首页 Nginx 重启确认状态恢复和 PID 变化，控制台无应用错误。macOS 原生启停仍未实机验收。

Web 类型检查、桌面和 CLI Rust 编译检查（使用锁定依赖）、版本一致性及 diff 空白检查通过。

版本同步至 0.2.27，创建并推送新的 annotated tag v0.2.27，保留历史 tag。已确认 v0.2.26 Release workflow completed/success。本次没有数据库变更，未修改 update.sql；没有新增测试文件、依赖或迁移，未启动前端 dev、未运行前端 build，未修改真实系统配置或 FlyEnv 数据。保留原有未跟踪文件，整体产品完善目标继续进行。

## 第四十二轮：批量启停的监控一致性与托盘真实反馈（v0.2.28）

参考 ServBay 套件与服务管理中的菜单栏快捷操作（https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management），排查全部启停和服务栈的调用链。原批量与整栈直接走 ops，绕过 CoreState 的启动后存活检查和 watchdog 记录；主动停止后可能再次被自动拉起。本轮保留已有排序、去重和逐项报告，将桌面、CLI、smoke 入口统一接入 CoreState；整组持有可重入生命周期锁，操作重叠立即拒绝，不再等待后执行过期请求。原低层接口保留供既有调用和回归使用。批量重启仍先逆序停止、再正序启动，停止失败项不再进入启动阶段，报告和服务状态注明失败阶段。

整栈和批量的成功启动、重复启动、主动停止及已经停止均同步 watchdog；Error 但有存活 PID 仍尝试停止，已经没有进程的 Error 不再发送无目标的停机命令。CLI 全部启动改为先数据库后 Web，全部停止反向执行，去掉将 STOP_FAILED 当跳过的分支，并支持现有 --json 参数输出批量报告。托盘全部停止等待 blocking worker 返回真实结果，先停止独立管理台再逐项停止已注册服务；失败继续保留 PID 文件，不再一律删除它。

Windows 进程组终止补充实际完成检查。参照 Microsoft TerminateJobObject / QueryInformationJobObject 文档（https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-terminatejobobject），结束 Job 后最多等待 5 秒，查询活动进程归零；失败或超时返回错误。服务停止不再吞掉进程组终止失败，启动失败后的清理异常也附在原错误详情里。隔离验证曾在停止后单次连接探针与监听表结果之间观察到时序差异，最终使用原监听 PID 全部退出、监听表无该端口及端口能够重新绑定共同验证停止结果。

托盘新增持续的逐项结果区域，显示成功、已经处于目标状态、失败和缺失项；失败可查看日志或重试，启动重试只提交失败项，全部停止再次执行时明确标注“再次停止全部”。请求失败保留上一份报告，状态读取失败显示重试并禁用服务操作，后到的旧读取结果不能覆盖最新状态推送。状态事件不再清除执行中的忙碌标记；单项、服务栈和全部操作互斥，Error 且仍有 PID 的行执行停止。栈或站点列表读取错误不再悄悄变成空列表。浏览器主 mock 对批量、整栈及单服务共用完整操作锁；托盘独立演示明确不操作本机服务，缺失项目如实报告。

托盘行改为可键盘操作的按钮，增加状态文字、焦点提示和结果区域朗读；错误与长路径可换行，明细独立滚动。分隔线统一为左右 14 px 留白的虚线，修复 320 宽度快捷按钮继承标题 padding 导致的不正常换行，使用纯色图标背景并支持减少动画。浏览器验证部分失败、请求中断保留报告、重试目标、读取失败与恢复、重复点击、状态推送期间锁定、过期请求不能覆盖新状态、Error+PID 的停止动作、键盘启动服务栈及缺失项提示。320 / 390 / 1280 宽度无横向溢出，检查了浅色和深色截图，长错误以文本转义展示。

验证：6 项扩展后的生命周期回归、16 项批量回归、3 项服务栈回归、9 项端口回归及 14 项 watchdog 回归通过，共 48 项。真实有限监听子进程验证整栈及批量启动、重启 PID 更换、全部停止、端口重新绑定和 watchdog 状态；停止失败使用受保护的测试 PID，只验证错误与 PID 文件保留，不结束测试进程。Node 内联脚本加载实际 mock 验证整组互斥、去重、未知对象、缺失项与重复启动保持 PID。使用临时目录与 SQLite，无真实用户进程或系统配置修改。macOS 原生流程和原生托盘窗口尚未实机验收，浏览器验证使用隔离的 IPC 响应夹具，未把它当作完整桌面运行验收。

Web 类型检查、桌面及 CLI Rust 编译检查（锁定依赖）、版本一致性及 diff 空白检查通过。隔离浏览器上下文已关闭，主浏览器恢复套件页，控制台无应用错误。

版本同步至 0.2.28，创建并推送新的 annotated tag v0.2.28，保留历史 tag。已确认 v0.2.27 Release workflow completed/success。本次没有数据库变更，未修改 update.sql；没有新增测试文件、依赖或迁移，未启动前端 dev、未运行前端 build。原有未跟踪文件保持未提交，整体产品完善目标继续进行。

## 第四十三轮：退出、重启和安装更新的停机前置条件（v0.2.29）

继续落实 ServBay 服务管理中“停止所有服务并退出”的语义。原关闭窗口和退出命令忽略停止错误并删除 PID 文件，重启及更新安装先启动新进程再尝试停机。本轮将退出、重启、安装更新与目录复制统一接入 CoreState 的停机前置检查：生命周期锁贯穿安装任务检查、全部停止、PID 记录保存与后续动作。停止失败、残留 PID、记录写入失败均阻止后续动作，返回真实错误，保留应用和恢复记录。已停止的服务不会自动恢复，界面明确说明。PID 文件使用同目录临时文件替换，写入失败不截断旧记录。

桌面退出、重启和迁移使用可恢复的互斥状态；准备期间拒绝新操作，只允许必要的状态读取和托盘隐藏。窗口关闭和系统退出转入后台受控停机，失败重新显示主窗口并给出可读错误。关闭到托盘的原行为保留。新进程或安装器只有在受管服务停止成功后才启动；启动失败恢复操作入口。辅助任务仍沿用既有 cron、tunnel、Ollama 的 best-effort shutdown，尚未完成其错误传播和完整交接验收。

退出确认和更新安装失败后保留弹窗及内联错误，聚焦错误区域并允许重试，忙碌期间阻止关闭与重复提交。更新安装保留下载路径，重试无需重新下载。更新弹窗头尾固定、中间滚动，分隔线使用左右留白的虚线，更新说明区域也补齐虚线与留白，长错误可换行。浏览器演示的退出、重启与安装更新明确返回仅桌面端可用，不再伪报成功。

迁移复制成功后显式把目标目录交给 restart_app，仅对新子进程设置 NSB_HOME。复制与重启分开记录，重启失败保留副本，重试只重启；取消后当前应用仍使用原目录。此轮仅修复停机和失败恢复，迁移目标的持久化选择、已安装记录和配置中绝对路径的重定位仍需后续完善，不能据此认定目录迁移已完整验收。

验证：7 项服务生命周期回归和已有目录复制原子性回归通过。隔离临时 SQLite、目录与最多存活 30 秒的子进程验证停止失败阻止退出/复制、安装任务阻止切换、PID 记录写入失败阻止后续动作、实际停止先于回调、回调失败后锁可释放并重新启停。Node 内联验证 3 个浏览器演示入口均返回 DESKTOP_ONLY。未新建测试文件。

浏览器隔离 IPC 夹具验证退出与安装忙碌时 Escape/外部点击不能关闭、失败后错误聚焦、退出重试、更新重试使用相同下载路径且不重复下载，以及复制一次后重启失败再重试只调用重启。迁移取消后焦点恢复到原按钮。320 / 390 / 1280 宽度检查弹窗边界、长错误换行和底部操作可达，窄屏截图已目检；窗口尺寸检查等待布局稳定。夹具上下文已关闭，主浏览器保留套件页。未实际退出用户应用、启动新 NiceEnv 或安装器，macOS 原生流程仍需实机验证。

版本同步至 0.2.29，按项目规则创建新的 annotated tag 并与 main 一起推送，保留历史 tag。已确认 v0.2.28 Release workflow completed/success。Web 类型检查、桌面及 CLI Rust 编译检查使用锁定依赖，发布前再次核对版本一致性与 diff。没有数据库变更，未修改 update.sql；没有新增依赖或迁移，未启动前端 dev、未运行前端 build，未修改真实用户服务和系统配置。原有未跟踪文件保持未提交，整体完善目标继续进行。

## 第四十四轮：数据目录持久迁移、路径修正和失败恢复（v0.2.30）

参考 ServBay 数据库文件管理与迁移指导（https://support.servbay.com/database-management/getting-started/database-file-management-and-migration）及 SQLite VACUUM INTO 文档（https://www.sqlite.org/lang_vacuum.html）。原流程只复制文件并向新子进程传 NSB_HOME，从快捷方式重新打开可能返回旧目录，安装记录和配置中的绝对路径也可能仍指向原位置。本轮将目录选择独立保存到用户配置目录，优先级为显式 base、NSB_HOME、持久选择、原有默认目录；已保存的目录失联、配置损坏或数据库无效时明确报错，不回退创建空库。保存选择使用进程间锁与原子替换，启动新进程失败恢复原选择，恢复失败也单独提示。桌面初始化失败使用原生错误提示，不因缺少 CoreState 导致后续 IPC panic。

迁移在受管服务停止后复制至暂存目录，源 SQLite 使用只读连接和 VACUUM INTO 生成事务一致快照，不逐个复制活动 WAL 文件。在副本事务中修正安装路径、站点目录与运行命令、本地证书、计划任务及证书自动化的本地部署路径；密码、外部路径、远程部署和历史内容保留。按完整目录边界处理正反斜杠、Windows 转义与规范路径别名，防止误改同名前缀的其它目录。只扫描指定配置和脚本，业务数据库二进制、日志、下载和备份原文保持不变；链接、含歧义的路径、无法安全引用的新目录、过大或无法转换的配置明确失败，源目录保留。全部校验成功后才提交目标副本。

原 pathEnvDirs 保留，用于新目录首次启动时精确清理原托管 PATH 条目；同步失败保留激活标记供下次重试，不伪报已完成。迁移历史单独记录，恢复旧备份时先验证原始校验值，再按历史目录转换到当前路径，预览 revision 包含转换后内容；连续迁移后仍能恢复早期备份，不修改备份原文。

复制和等待重启期间持有源目录独占活动锁；桌面同步/异步命令、CLI/MCP 调用、服务生命周期和相关后台任务持共享锁，避免副本准备好之后继续改动源目录。开始迁移前预检活动锁，忙时先返回，不提前停服务。待重启结果由桌面进程保留，重启失败保留副本与锁，重试只重启；取消明确释放锁，恢复原目录操作且保留副本。设置页初次查询并订阅 data-dir://prepared，先订阅再读取快照，覆盖复制过程中刷新、复制随后完成的情况；清理迟到的订阅和旧结果。迁移期间允许读取设置，让刷新后的页面仍能显示恢复操作。确认框显示复制文件数、大小及修正配置数，取消失败保留错误与重试入口。

验证：41 项既有源码内定向回归通过，覆盖路径/备份保护、持久选择与回滚、SQLite WAL 快照、路径映射、活动锁、服务生命周期、计划任务、备份任务、MCP 和配置历史。隔离临时目录中将源目录移走后，实际运行副本安装路径下的短命脚本，确认运行不依赖原目录。浏览器隔离 IPC 夹具验证复制一次后重启失败、刷新恢复、重复点击互斥、忙碌时不能关闭、取消失败后重试成功；追加“复制中刷新→初查无副本→完成事件到达→恢复确认→重启失败→取消”的检查，只有一个有效订阅、没有重新复制，页面错误为零。320 / 390 / 1280 宽度检查无横向溢出，窄屏底部操作可达并已目检截图。最终 Web 类型检查、桌面及 CLI Rust 编译检查使用锁定依赖通过。

本轮没有表结构变更；update.sql 记录只在目标 SQLite 副本执行的参数化路径更新模板，不需要手工部署，未操作真实用户数据库。没有新增测试文件、依赖或 migration，未启动前端 dev、未运行前端 build，未修改真实系统 PATH、hosts、DNS、代理或 FlyEnv 数据。没有实际启动新 NiceEnv 进程或退出用户应用；原生初始化错误提示、完整父子进程交接及 macOS 仍需实机验收。spawn 成功不代表新进程已完成初始化，父子初始化握手及其它已打开实例在目录接管后的自动失效尚待完善。

版本同步至 0.2.30，创建新的 annotated tag v0.2.30 并与 main 一起推送，核对远程 peeled tag 与发布提交及 Release workflow 实际状态，不移动历史 tag。已确认 v0.2.29 Release workflow completed/success。原有未跟踪文件保持未提交，整体产品完善目标继续进行。

## 第四十五轮：迁移后的旧实例失效、后台入口与端口释放时序（v0.2.31）

继续对照 ServBay 的数据库迁移指导，完善停止、复制、切换之后的操作边界。v0.2.30 的源目录锁在迁移进程退出后释放，其它已打开实例仍缓存原 CoreState，可能继续读取或修改旧目录。本轮将已迁出的源目录列表与当前目录选择保存在同一个原子 JSON 文件中，源目录仅保留指向该选择文件的交接记录。独占活动锁持有者才可提交切换；新选择和源目录失效同时生效，启动失败恢复原选择原文时也同步恢复源目录可用性。首次提交前中断留下的未生效引用不阻止继续使用原目录，损坏的选择或交接记录明确失败。连续迁移保留全部已迁出目录，早期实例收到的恢复提示指向当前最终目录；交接记录不会复制到新目录。

每次取得数据目录活动锁都会核对交接记录，旧实例的桌面数据请求、CLI/MCP 调用、服务操作、备份和计划任务明确拒绝继续操作，错误说明退出旧窗口、重新打开应用或重新连接 MCP。旧实例仍可退出或重新打开，跳过对旧 PID 文件和已接管服务的停机写入；正常实例保留完整停机前置检查。托盘主动推送也检查目录是否有效，旧状态不能通过另一条推送链路重新显示成当前状态；状态错误时显示处理提示，不再使用“尚未安装服务”的空状态，服务操作禁用而退出入口保留。

复查发现 watchdog_tick 直接调用低层 ops，绕过了上一轮的活动锁。本轮为整次 tick 持共享锁，迁移准备期间和源目录失效后均不尝试启动、不消耗重试次数。计划任务调度器初始化以及派生工作线程在打开 SQLite 之前取得活动锁，避免检查之前已有数据库写入。CLI/MCP 初始化错误保留具体恢复建议。

扩展生命周期回归时两次观察到 Windows 原进程退出且监听已消失后，立即重新绑定仍返回 AddrInUse，导致重启失败。端口预检现在仅对“无监听者且 AddrInUse”的情况最多等待 2 秒并重试绑定；监听冲突、非法端口和权限错误不被忽略，等待结束重新查询占用者身份。没有绕过绑定检查，也不会通过这段等待结束其它进程。补充有限套接字回归，覆盖延迟释放、仍绑定但未监听的端口以及真实监听者。

验证：18 项路径/备份回归、9 项服务生命周期、14 项 watchdog、8 项计划任务、6 项 MCP 和 2 项端口预检，共 57 项通过。使用临时目录与 SQLite 验证原选择回滚、跨多次迁移的旧实例拒绝、原设置与 PID 文件保持；实际启动独立的有限测试子进程检查源目录已失效。实际临时监听子进程验证重启 PID 变化与端口策略。没有处理真实用户服务、迁移真实数据或修改系统 PATH/hosts/DNS/代理。

浏览器隔离 IPC 夹具验证托盘错误说明、重新读取仍失败时保持禁用、退出请求可用、恢复后显示正常空状态，页面错误为零。320 / 390 / 1280 宽度无横向溢出，320 宽度截图已目检，长目录提示可换行；夹具上下文已关闭。沿用 ui-ux-coding 的状态区分、可访问操作和间距规范。最终执行 Web 类型检查、桌面及 CLI Rust 编译检查、版本一致性与 diff 检查，未启动前端 dev、未运行前端 build。

本轮没有数据库变更，未修改 update.sql；目录交接记录属于文件配置。没有新增测试文件、依赖或 migration。此保护适用于含本轮检查的新版本实例，无法让此前版本的运行中程序自动获得检查逻辑。完整桌面父子进程初始化握手、原生窗口退出/重开与 macOS 尚需继续验证；spawn 成功仍不等于新进程完成初始化。整体完善目标保持进行。

版本同步至 0.2.31，新增 annotated tag v0.2.31 并随 main 推送，核对远程提交与 tag 以及 Release 实际状态，保留历史 tag 和原有未跟踪文件。已确认 v0.2.30 Release workflow completed/success。

## 第四十六轮：迁移重启启动确认与失败回退（v0.2.32）

继续补齐目录迁移中“新进程创建成功”与“应用实际可用”的差距。迁移重启新增本机回环 TCP 交接，使用随机凭据、实际子进程 PID 和目标目录核对身份。父端关联进程组后才允许子端继续初始化，避免 Windows WebView 在 Job 关联前创建子进程。新进程完成 CoreState 初始化、Tauri Ready、React 挂载及设置读取后报告就绪，收到父端提交才显示主窗口；父端收到窗口确认并发出最终启动消息后，子端才放行用户后台任务。总等待有界，原窗口在确认完成前保留。此流程目前用于迁移重启，普通重启和已迁出实例重新打开仍沿用原有启动流程。

计划任务、备份、证书自动化、开机服务栈和 watchdog 在线程开始处等待启动放行，取消则退出。备份调度从 CoreState 初始化移到桌面启动层，CLI/MCP 的初始化不再自动创建备份线程。前端只允许 main WebView 确认就绪，交接完成后刷新准备期间可能受限的查询；托盘 WebView 不能代替主窗口确认。

初始化失败、窗口显示失败、提前退出、断开和超时均返回错误。失败先终止本次创建的进程组并确认子进程退出；Windows 检查 Job 活跃进程归零，Unix 另外检查进程组消失。关联失败或清理不能确认时保守返回错误，不恢复目录选择，不重新启用源目录。清理确认后恢复旧目录选择原文，同时恢复副本的 pathEnvDirs 与激活标记供重试。已启用 PATH 时保存原注册表值/类型或 shell profile 内容，仅在当前值仍等于本次预期写入值时恢复，遇到并发修改明确报错并保留外部改动。这里只回退迁移激活相关状态，不回滚副本内全部初始化记录。

迁移确认框显示正在等待新窗口和页面加载，最长约一分钟；忙碌时禁用重复提交和关闭。一般失败保留副本、错误聚焦、重试只启动，不重新复制。无法确认清理或目录选择回退时，提示原目录仍受保护，提供关闭旧窗口入口，不再声称取消一定能恢复原目录。

验证：57 项定向源码内回归通过（启动交接 4、目录/备份保护 19、PATH 18、计划任务 8、自动备份 2、证书自动化 6）。独立有限子进程覆盖真实 CoreState 初始化、迟到就绪、错误凭据、初始化/窗口失败、提前退出、超时、父端在提交前及最终放行前断开；额外实际创建孙进程并核对失败后子树退出。临时 SQLite 与目录验证真实子进程失败后选择原文恢复、激活记录可重试，以及无法确认清理时源目录仍失效。未改动真实系统 PATH 或用户服务。

浏览器隔离 IPC 夹具验证设置未完成不发送页面就绪、放行后重取查询、三次重启不重新复制、初始化/超时失败、取消以及清理未确认时关闭旧窗口；错误聚焦且页面错误为零。320 / 390 / 1280 宽度无横向溢出，390 宽度等待界面截图已目检。最终 Web 类型检查和桌面/CLI Rust 编译检查使用锁定依赖。没有启动前端 dev、运行前端 build、新建独立测试文件或引入依赖。

没有数据库表结构变更；update.sql 同步记录仅目标 SQLite 副本的 pathEnvDirs 参数化恢复语句模板，不需手工执行。没有操作真实用户数据库、系统 PATH/hosts/DNS/代理或 FlyEnv 数据。完整原生 WebView 父子交接、实际系统 PATH 回退与 macOS/Linux 仍需实机验收，不能以隔离进程与浏览器夹具代替整机验证。整体产品完善目标继续进行。

同步版本至 0.2.32，新增 annotated tag v0.2.32 并与 main 一起推送，核对远程提交、peeled tag 和 Release workflow 实际状态；不移动旧 tag，保留原有未跟踪文件。已确认 v0.2.31 Release workflow completed/success。

## 第四十七轮：退出前确认辅助任务结束及失败后恢复（v0.2.33）

继续对照 ServBay 的“Stop All Services and Exit”（https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management）。现有退出、重启和更新流程在启动安装器/新进程后才调用忽略错误的辅助清理，计划任务仍可能写记录，隧道可能没有结束，模型拉取也仅发送取消。新增可恢复的 AuxiliaryShutdown 准备阶段：先禁止新任务并确认计划任务、临时隧道、Ollama 拉取已收尾，再停止受管服务，最后才启动安装器、重启或退出。清理失败聚合具体错误，保留当前应用。后续服务停止、复制或启动失败，准备对象释放时恢复新任务入口；已经停止的任务不会自动重跑。避免在持有服务生命周期锁时等待模型操作锁。正常服务启动、重启及 watchdog 同时检查准备状态，防止停机窗口中重新启动服务。

迁移在复制前收尾这些任务，待重启状态同时持有目录独占锁与辅助任务准备对象。重启失败保留准备状态和副本供重试，取消释放状态恢复入口；成功交接后提交退出。已迁出实例仍只处理自己拥有的任务，不写旧 PID 文件或停止新实例服务。RunEvent::Exit 保留最终 best-effort 清理，但正常退出不再依赖它判断成功。

计划任务在注册处和开始创建命令时检查关闭状态，关闭快照与注册互斥。等待执行线程完成命令树清理和结果保存；超时或保存失败不报成功。清理失败及线程异常保留受管组与 PID，后续停止/退出仍能重试；命令未能加入受管组时不能声称子树已经清理。取消期间调度器暂停检查而不永久退出，退出中止后可以恢复。命令退出等待有界，Unix 发送终止信号的错误开始向上传播，并核对进程组是否消失。

隧道关闭返回真实错误，创建锁或进程锁忙时有界失败，停止失败保留记录和句柄；主进程提前退出后也检查剩余子树，不能将清理失败标成已停止。Ollama 拉取保存工作线程句柄，关闭时取消请求并等待线程结束；操作锁忙、超时或线程异常均明确返回。失败恢复只重新允许操作，不代替用户启动新的下载。没有发送新的远端取消 API，也不承诺取消其它客户端共享的下载。

退出确认说明受影响的任务、失败保留窗口及不自动重跑，处理中显示等待提示。更新按钮改为“安装更新”，不再承诺当前实现没有保证的自动重启；提示先完成清理和停机，再打开安装程序。清理失败保留下载路径和错误，重试不重新下载。安装等待区的图标固定宽度、文本可收缩换行，底部只保留简短状态，沿用已有左右留白的虚线分隔。

验证：75 项定向回归通过（计划任务 10、隧道 12、Ollama 7、服务生命周期 9、watchdog 14、启动交接 4、目录/备份保护 19）。计划任务在独立有限子进程中验证真实命令取消、被锁阻塞时超时、结果持久化、清理失败撤销关闭标记、后续安装器启动回调失败后恢复，以及提交退出后不能接收新任务。隧道用实际有限子进程验证锁忙失败后保留进程、恢复入口和重新停止。模型下载只访问临时回环 HTTP 夹具，验证请求关闭、线程结束和操作锁忙后的重试。2 项需要显式启动的隧道辅助/官方 CLI 测试保持忽略；本轮未下载或运行真实 cloudflared/Ollama。

浏览器隔离 IPC 夹具验证退出等待时 Escape 无效、清理错误聚焦、退出重试、安装准备期间不能关闭以及两次安装仅一次下载且使用相同路径，页面错误为零。320 / 390 / 1280 宽度无横向溢出，390 宽度退出错误与安装等待截图已目检。Web 类型检查、桌面/CLI Rust 编译检查、版本与 diff 检查通过。未启动前端 dev、未运行前端 build、未新增测试文件或依赖。

本轮没有数据库变更，未修改 update.sql；任务结果沿用现有存储接口。未操作真实用户服务、系统 PATH/hosts/DNS/代理或 FlyEnv 数据。验证没有真正退出用户应用、启动安装器或更换已安装版本。完整原生退出/安装流程及 macOS/Linux 仍需实机验收；本轮收尾范围为计划任务、隧道和模型下载，证书续签、备份等其它后台工作完整退出验收继续完善，整体目标保持进行。

同步版本至 0.2.33，新增 annotated tag v0.2.33 并与 main 推送，核对远程提交、peeled tag 及 Release workflow；保留旧 tag 和原有未跟踪文件。已确认 v0.2.32 Release workflow completed/success。

## 第四十八轮：退出等待证书任务与完整配置备份（v0.2.34）

继续补齐退出、更新与迁移前的在途工作保护。证书签发及其部署/通知、证书监控、自动备份和手动配置导出接入同一后台任务登记。登记与暂停共用互斥锁，工作句柄一直持有到写入和通知结束；自动签发在线程创建前登记，避免退出漏掉尚未运行的工作。退出准备先暂停新任务，最多等待 25 秒让已有工作自然结束，再执行上一轮的计划任务、隧道、模型下载收尾和服务停止。等待超时返回具体任务清单，保留当前应用和在途任务，解除暂停供用户完成验证后重试。后续退出/更新/迁移失败或取消同样恢复入口；提交退出后保持暂停。没有强制中断证书流程，也没有提前停止证书部署可能依赖的服务。手动 DNS 等待状态与签发中状态一样拒绝重复发起。

配置导出通过独立只读 SQLite 事务获取跨表一致快照，复用原有查询接口且不重入主连接锁，不执行建表或初始化语句。完成序列化后同目录临时写入、同步文件，再原子替换目标；替换失败保留原备份并清理临时文件。导出条数补计代理订阅，Redis 本机凭据和 DNS 恢复记录继续留在本机，旧备份格式仍可读取。

自动备份使用时间、纳秒与随机后缀命名，并以禁止覆盖方式发布，解决同一秒生成的文件相互覆盖。操作系统文件锁串行保护不同窗口的发布与轮转，忙时真实返回错误。轮转只处理合法时间戳名称、普通文件和可解析的本程序备份，保留链接、目录、损坏文件和其它资料；读取/删除失败向调用方返回，不再伪报成功。新备份已生成但轮转失败时提示其保留路径，不更新上次成功时间；即使系统时间回拨或目录存在未来命名的备份，也优先保留本次新文件。

沿用 ui-ux-coding 的状态说明、错误聚焦及窄屏间距规范，中英文退出、更新和迁移提示说明证书/备份等待与超时保留窗口。浏览器独立上下文使用隔离 IPC 夹具验证退出等待时不可关闭、超时提示聚焦、失败后重试、更新重试沿用下载文件且只下载一次。320 / 390 / 1280 宽度无横向溢出，按钮可达；390 宽度截图已目检。夹具页面错误为零，上下文已关闭，用户主标签保留套件页。

验证：96 项定向源码内回归通过（后台工作 3、自动备份 4、配置导出 5、证书自动化 6、证书监控 1、计划任务 10、隧道 12、Ollama 7、生命周期相关 11、watchdog 14、启动交接 4、目录保护 19），2 项隧道显式辅助/官方 CLI 测试继续忽略。使用独立有限子进程和本机 TCP 连接，实际让证书监控停留在握手阶段，确认退出超时、暂停时拒绝签发/监控/备份/导出、结果保存后才允许下一步、失败恢复与提交后拒绝新任务；没有访问真实签发或通知服务。临时目录与 SQLite 验证跨表快照、只读事务、同秒多份备份、文件被占用时旧内容保留、轮转失败不报成功、未来命名不挤掉新备份及无关文件保留。Web 类型检查和 workspace 全 targets Rust 编译检查使用锁定依赖通过，版本一致性与 diff 已检查。

本轮没有数据库变更，未修改 update.sql；新增事务只读，现有备份时间写入沿用原存储接口。没有新增测试文件、依赖或 migration，未启动前端 dev、未运行前端 build，未修改真实用户服务、PATH、hosts、DNS、代理或 FlyEnv 数据。退出保护针对本进程登记的这些工作；并不代表所有后台模块已完整验收。真实 ACME/DNS 部署、原生窗口退出/安装和 macOS/Linux 仍需实机验证；证书任务的跨进程互斥和中断恢复另行完善。整体产品完善目标继续进行。

版本同步至 0.2.34，新增 annotated tag v0.2.34 并与 main 一起推送，核对远程提交、peeled tag 与 Release 实际状态；不移动旧 tag，不提交原有未跟踪文件。已确认 v0.2.33 Release workflow completed/success。项目根 AGENTS.md 已固定“提交或 push 新版本必须新增 tag”的发布约定。

## 第四十九轮：证书自动化并发保护、中断恢复与编辑状态（v0.2.35）

参考 ServBay 的 ACME 签发与续期流程（https://support.servbay.com/basic-usage/ssl/using-acme-to-issue-ssl-certificate）。同一自动化的签发、编辑、启停自动续签、删除和状态恢复共用操作系统文件锁，跨窗口/进程串行执行。锁覆盖签发、DNS 验证、部署、结果保存和通知；进程异常结束后操作系统自动释放。调度线程取得锁后重新读取开关和到期时间，防止旧快照导致关闭后仍执行或刚续完又执行。手动续签提醒也在锁内复核状态，不覆盖另一窗口正在签发的状态。

列表读取与调度检查只在取得任务锁且仍保存 issuing/manual_wait 状态时认定中断，标记错误、暂停自动续签、记录一次失败历史，并保留手动 TXT 信息供核对清理。有其它进程持锁的任务保持运行状态；重复检查不重复追加中断记录。用户检查后可手动签发，或重新启用自动续签。手动签发与自动续签开关分开，关闭自动续签仍可主动签发。运行中仍受上一轮的退出等待保护，保存/删除/状态恢复也加入该后台工作登记。

保存编辑前比较更新版本，旧表单不覆盖新配置和新结果；更新版本单调递增，避免同毫秒变化漏检。客户端不能伪造执行状态、证书结果、历史、排期和部署结果；新记录重置这些字段，编辑保留由后台维护的值。域名、算法或 CA 改变后改为待签发，清除当前配置的成功标记并重新排期，原证书文件与历史保留给已有站点。已删除记录不能通过迟到的保存重新创建。存储读取失败、损坏 JSON 或不一致的记录标识明确报错，不再悄悄从列表隐藏。删除账号密钥失败时保留记录，数据库删除失败时恢复密钥；不存在的任务返回错误。

复用已有域名规范化和校验，防止路径型域名写入证书目录；任务 ID 校验限制账号密钥和锁文件路径。账号文件仅 NotFound 视作不存在，其它读取错误/链接/目录明确失败；新账号密钥采用原子保存。修正手动 DNS 等待把秒乘为分钟的问题，限制排期数值范围；相同 TXT 名称的不同验证值分别展示，不能只保存首个值。

沿用 ui-ux-coding 的状态、恢复操作与窄屏规范。每个签发任务独立显示忙碌，后端轮询到的 issuing/manual_wait 同样禁用冲突操作；一个任务完成不会解除另一个任务的忙碌。编辑器保留输入，后台开始执行时锁定控件；保存和删除过程中禁止重复提交/关闭，失败就地显示、聚焦并保留重试入口。运行历史读取最新列表结果。手动 TXT 名称和值完整换行且分别可复制；中断记录说明核对清理用途。编辑区独立滚动，标题、错误和操作按钮保持可见，避免错误聚焦带动整个弹窗滚动；局部分隔线改为有留白的虚线。网页预览签发返回 DESKTOP_ONLY，不能伪报已申请真实证书；预览编辑同样保护运行字段与旧表单，删除全部样例后保持空列表。

验证：50 项源码内定向回归通过（证书自动化 12、后台工作 3、配置导出 5、自动备份 4、本地证书 7、目录保护 19）。独立有限子进程实际持锁并保存手动等待状态，验证其它窗口所有冲突操作被拒绝；主动结束该夹具后验证锁释放、仅一次中断恢复、TXT 留存与暂停排期。临时 SQLite 与目录验证过期调度复核、运行字段保护、旧表单冲突、已删除记录不复活、证书配置改变不沿用旧成功状态、损坏记录不隐藏、账号读取失败及删除失败恢复。没有调用真实 CA、DNS、部署目标或通知接口。

浏览器隔离 IPC 夹具验证两个并行签发各自的忙碌状态、自动续签关闭时仍可手动发起、编辑中轮询到其它窗口开始任务后禁用控件、完成后保留输入、保存冲突错误聚焦、删除失败保留并重试。320 / 390 / 1280 宽度下编辑器无横向溢出，外层不因错误聚焦而滚动，底部操作可达；390 宽度截图已目检，页面错误为零，夹具上下文已关闭。Node 内联执行实际预览模块确认签发拒绝、字段保护、旧表单拒绝和删除全部后不重新播种样例。Web 类型检查、Rust workspace 全 targets 编译检查、版本一致性和 diff 检查通过。

没有数据库表结构变更，update.sql 同步记录应用在运行时执行的参数化状态恢复/保存/删除模板，无需手工部署。没有新增独立测试文件、依赖或 migration，未启动前端 dev、未运行前端 build，未改动真实用户数据库、证书、服务或系统配置。跨进程保护适用于执行本轮锁协议的版本；真实 ACME/DNS、各部署目标、原生桌面和 macOS/Linux 仍需实机验收。不同自动化之间重叠域名的协调、签发成功但部分远程部署失败的整体状态，以及远程部署专用证书到期时间仍需继续完善，整体产品目标保持进行。

同步版本至 0.2.35，创建新的 annotated tag v0.2.35 并与 main 一起推送，核对远程分支、peeled tag 和 Release 实际状态；保留旧 tag 与原有未跟踪文件。已确认 v0.2.34 Release workflow completed/success。

## 第五十轮：证书部署结果与复用证书重试（v0.2.36）

签发与部署分开记录。ACME 返回后直接解析证书有效期，并验证证书/私钥、服务端用途及 SAN；只部署远程也保存真实到期时间。证书链和私钥先写入受管目录下单个原子材料文件，再逐一部署。本地站点与外部目标各自保存结果，一处失败不会阻断其余目标；全部配置的目标成功后才显示完成、写成功历史和发送成功通知。本地站点证书沿用成对回滚工具，文件或记录写入失败恢复旧证书和私钥。

新增重试部署入口，复用当前批次证书，不重新申请 CA 或修改 DNS。每完成一个目标就持久保存进度，重试跳过同批且配置未变的成功目标；更改目标配置会清除该目标的成功结果。正常部署失败按原重试策略排期；材料丢失、损坏、错配、过期、持久化失败或进程中断则暂停，等待人工核对。重试同样受跨进程任务锁和退出等待保护。中断后的重试在界面提示未确认的操作及脚本可能再次执行，成功后自动续签仍保持暂停；重新开启成功任务会依据真实有效期排期，不立即重复申请。不同自动化的重叠域名协调、SSH/任意目录部署的整体回滚和脚本超时仍待后续完善。

材料路径检查链接/目录联接，读取限制为 8 MiB，核对自动化、批次、域名、CA 与算法后再校验证书。证书私钥不进入自动化 JSON 或配置导出；导入配置清除来源机器的证书批次与部署状态。删除自动化时清理账号密钥和重试材料，失败时恢复已删除的文件；已部署站点证书继续保留。旧版本把失败目标记为整体成功，或远程专用证书有效期为零的记录，会在持锁复核后纠正一次并暂停，明确提示需要重新签发，不伪造不存在的材料。

宝塔、1Panel、阿里云 CAS、腾讯云上传统一检查 HTTP 成功和 JSON 对象，宝塔 SetSSL 必须明确 status=true，云平台必须返回有效证书 ID。按照官方文档修复腾讯云 TC3 二进制 HMAC 派生、规范头的小写 Action 和 CertificateId 字段；阿里云使用匹配 UploadUserCertificate 的 2020-04-07 API、原始 PEM、POST 签名、签名值 URL 编码及 SDK 区域映射。阿里云新证书名称包含指纹后缀、重试携带稳定 ClientToken；腾讯云关闭重复上传。两种云平台使用验证服务器证书且不跟随重定向的客户端。面板/云平台的真实版本兼容与端到端部署仍需实机验证，本轮没有访问真实部署接口。

官方依据：腾讯云 UploadCertificate https://cloud.tencent.com/document/api/400/41665 ，TC3 签名 https://cloud.tencent.com/document/api/1076/35205 ；阿里云 UploadUserCertificate https://help.aliyun.com/zh/ssl-certificate/developer-reference/api-cas-2020-04-07-uploadusercertificate ，RPC 签名 https://help.aliyun.com/zh/sdk/product-overview/rpc-mechanism ，官方 CAS SDK https://github.com/aliyun/alibabacloud-typescript-sdk/blob/master/cas-20200407/src/client.ts 。延续 ServBay ACME 文档关于失败检查、自动续签和避免过度请求 CA 的设计依据。

界面显示部署中、部署未完成及中断待检查，增加复用证书重试和重新签发入口。本地结果与外部错误完整换行，操作分隔线保留两侧留白和虚线；运行期间禁用冲突操作。部署类型下拉只列出后端校验已接受的六种目标，不再让用户选择后端会拒绝保存的类型。网页预览同样拒绝真实签发/部署，样例失败状态与历史保持一致。

验证：64 项证书相关源码内回归通过，包含临时 SQLite/目录中的远程专用到期时间、部分失败、保存证书复用、成功目标不重复推送、本地成对回滚、缺失/损坏/错配/过期材料拒绝、中断状态和旧记录修复、配置导入去除来源批次。真实本机回环 HTTP 覆盖 500/503、HTML、空对象、业务拒绝和明确成功；TC3 使用 Node crypto 独立计算的固定向量。另检查新增重试入口的后台收尾与配置备份兼容。未调用真实 CA、DNS、部署或通知接口，没有改动真实用户服务和系统配置。

隔离浏览器验证重试失败后可再试、成功恢复、中断确认前不调用后端、忙时所有冲突操作禁用；三次重试均仅调用 certauto_retry_deploy，未触发 certauto_issue。320 / 390 / 1280 宽度无横向溢出，390 与 1280 截图已目检，独立页面错误为零，夹具上下文已关闭。Web 类型检查、Rust workspace 全 targets 编译检查、版本与 diff 检查通过。没有启动前端 dev、执行前端 build、新增独立测试文件或引入依赖。

没有表结构变更；update.sql 同步记录现有 cert_automations JSON 的批次、本地结果、逐目标进度、旧结果纠正与导入重置语句模板，不需手工执行。同步版本至 0.2.36，新增 annotated tag v0.2.36，与 main 一起推送并核对远程指向和 Release 状态；原有未跟踪文件保持不提交。已确认 v0.2.35 Release workflow completed/success。整体产品完善目标保持进行。

## 第五十一轮：SSH 与本地证书部署可执行性及失败恢复（v0.2.37）

修复 SSH 登录私钥与远程证书私钥共用 keyPath 的错误。登录文件使用 identityFile，兼容原 privateKey 字段，远程输出保留 certPath/keyPath。界面增加密码/密钥登录选择、可选私钥密码和本机文件选择；本地部署同样支持选择保存位置。各输入增加可访问标签，文件选择与远程路径互不覆盖。首次使用或更换主机、端口后，先只读取主机公钥并由用户核对 SHA256 指纹，再明确确认保存；探测不发送认证请求。实际部署只接受已确认指纹，变化时在发送凭据前终止，替换原无条件信任逻辑和过时提示。迟到的旧主机探测结果不能写入新主机配置。

SSH 建连、认证、开通道及 SFTP 初始化有超时，明确请求 sftp 子系统。远程路径按 Unix 路径处理，不再用 Windows 路径规则；父目录逐层检查，权限错误不当作不存在，拒绝软链接和非普通目标。两份文件先独占创建暂存文件并完成写入、同步和关闭检查，再保留原文件、发布新文件。新私钥权限默认 0600，已有私钥权限限制在 0660 以内，保留已有属主信息。发布失败尝试逆序恢复并清理；只要已开始远程发布或清理结果不确定，就明确要求核对远程状态，不声称网络故障后一定恢复。成功后旧备份清理失败在成功消息中保留路径提示。

本地目标复用路径防护，拒绝相同输出、相对路径、链接/目录联接及 Windows ADS 等别名路径。读取有大小限制，两个文件先在各自目录暂存并同步，再发布；第二份发布失败时恢复之前写入的原文件及权限。脚本复用现有进程组与双路限量输出执行器，超时清理子树，禁止用作长期后台任务启动入口。Windows 使用与计划任务一致的 cmd 原始命令传参，修复带引号的 PowerShell 命令被当作文本、误报成功的问题。远程脚本等待实际退出状态，EOF 不提前表示成功；非零、缺少退出状态、信号、超时和输出超限均不能成功，尝试发 TERM 并关闭通道，明确提示远程进程是否停止仍需核对。

保存和签发前校验 SSH/本地配置。调度遇到无效旧配置时暂停并记录原因，已有签发材料继续保留可重试状态，不退回申请新证书。脚本或发布结果不确定时逐目标保存结果，继续其它目标，最终标记 deploy_interrupted 并关闭自动续签，保留材料供人工核对后重试。数据目录迁移仅重写 SSH 的本机 identityFile/privateKey，不改远程证书、私钥和脚本；部署进行中拒绝迁移。update.sql 同步记录原 SQLite JSON 写入与副本路径更新模板，没有表结构变化，也没有操作真实用户数据库。

验证：73 项证书相关源码内回归通过，其中新增 9 项覆盖真实回环 SSH/SFTP 协议、探测和指纹不符时无认证、密码与密钥登录、SFTP 子系统、EOF 后退出状态、私钥权限、远程第二文件发布失败恢复、权限/链接拒绝、本地第二文件失败回滚、脚本引号/带空格路径/退出异常/超时/输出超限，以及自动暂停和目录迁移范围。夹具只用有限回环连接、内存 SFTP、临时文件和 SQLite，没有访问真实 CA/DNS/远程部署或通知接口。验证过程中 D 盘空间耗尽，使用 cargo clean -p nsb-core 清理当前包的历史构建缓存后恢复验证，未清理用户数据。

沿用 ui-ux-coding 技能，在隔离浏览器上下文验证认证字段切换、文件选择、主机变更时清除信任、丢弃迟到指纹、读取失败后重试、必须明确确认、保存失败保留输入、再次保存成功和本地路径选择。320 / 390 / 1280 宽度及 844 / 800 高度无横向溢出，底部按钮可达；截图已目检，无页面异常或重复 key 警告，夹具上下文已关闭。Web 类型检查及 workspace 全 targets Rust 编译检查通过，未新启动 dev、未执行前端 build、未新增独立测试文件或依赖。

另有 19 项目录迁移与路径保护回归、3 项脚本退出/输出/进程树清理回归通过。仍需真实 OpenSSH/SFTP 服务器与原生桌面跨平台验收；突然断电、跨进程或不同自动化写入重叠目标不具备整个部署过程的事务保证。远程异常需人工核对后重试，不能把自动化操作声明为恰好执行一次。同步版本至 0.2.37，新增 annotated tag v0.2.37 并与 main 一起推送，核对远程分支、peeled tag 与 Release workflow；不改动旧 tag，原有五项未跟踪文件保持不提交。已确认 v0.2.36 Release workflow completed/success，整体产品完善目标保持进行。

## 第五十二轮：不同证书任务的共用资源互斥与等待（v0.2.38）

原有执行锁只覆盖同一自动化标识，不同自动化仍可能同时写入同一个 DNS TXT 记录或覆盖同一证书文件。现在在改变任务为执行中、申请 CA 或进行部署之前，按实际验证记录及声明的输出位置取得操作系统资源锁。根域名与通配符的 DNS-01 验证归为同一记录，CNAME 代理使用实际授权域并规范大小写与尾点。本机文件使用已有父目录的规范路径，Windows/macOS 统一大小写；内置站点输出与任意本地目录目标使用同一种文件资源键。SSH 同时用规范化地址/端口和已确认指纹匹配输出路径，覆盖同一主机的 DNS 别名及原地址更换指纹；宝塔按面板地址和站点匹配，凭据变化不会绕过互斥。上传独立证书库条目的目标不据此串行。

资源按固定顺序去重并尝试获取，遇到占用立即释放本轮取得的其它锁，不阻塞持锁等待。锁持续覆盖签发、文件发布和部署脚本；互不冲突的任务仍可并行。锁文件只用 SHA256 摘要命名且内容为空，不写入密钥、密码或目标配置；持有文件句柄的进程退出后由系统释放。任务锁和资源锁路径均复用现有链接/目录联接防护。复用证书部署重试只预留未确认成功目标，不再占用 DNS 或已成功目标，避免无关等待。

自动任务遇到资源占用时记录 waiting 或 deploy_waiting，保留原证书批次、逐目标结果、历史、上次执行时间和失败次数，一分钟后再检查。调度改为每分钟检查到期任务，证书监控继续每小时执行。部署等待释放后自动复用保存材料，不重新请求 CA/DNS；手动操作遇到冲突直接返回可理解提示，不偷偷排入自动执行。等待期间可编辑、删除或关闭自动续签；取消签发等待回到空闲，取消部署等待保留可手动重试状态。准备阶段的非占用错误暂停并记录，防止静默循环；已有中断状态仍必须人工检查。

界面按 ui-ux-coding 的状态说明和可恢复操作规范增加等待徽标、原因、下次检查和取消方式，使用现有警示色及左右留白。等待没有伪装为运行中，不禁用编辑/开关，也不将其显示成部署失败；关闭后保留部署重试入口。中英文排期说明与实际一分钟检查频率同步，网页预览的开关和取消等待行为同步。没有新增表或字段；update.sql 记录现有 SQLite JSON 状态的参数化更新模板，无需手工部署。

80 项证书相关源码内回归、Web 类型检查与 workspace 全 targets Rust 编译检查通过。定向验证覆盖通配符/根域名、CNAME、部分取得锁后释放、不同域名并行、本地站点与目录输出重合、Windows 路径大小写和分隔符、SSH 别名/指纹变化、宝塔同站点不同凭据、跳过成功目标及 DNS。真实有限子进程持锁期间另一进程不能占用，退出后可恢复；实际本地部署脚本执行时另一自动化被挡住，脚本结束后才可替换。调度 tick 在等待释放后部署保留材料，未创建 ACME 账号文件且成功目标标记保持原样。使用临时 SQLite/文件和有限脚本，没有访问真实 CA、DNS、远程部署或通知服务，也没有操作真实用户服务或系统配置。

隔离浏览器在 320 / 390 / 1280 宽度验证等待状态、超长原因换行、编辑可用、资源仍占用时手动重试错误、关闭等待及之后重试成功；两次部署重试均只调用 certauto_retry_deploy。页面错误为零，截图已目检，上下文已关闭。没有启动新的 dev、执行前端 build、新增独立测试文件或依赖。

互斥范围是使用同一数据目录且支持本轮锁协议的自动化。外部工具、其它数据目录、旧版本进程和脚本中未声明的任意副作用不受这些锁协调；主机指纹复用可能让独立 SSH 主机的同名路径保守串行，远程主机不同密钥且不同别名也不能凭空判定相同存储。原生桌面及真实多平台服务器仍需验收。同步版本至 0.2.38，新增 annotated tag v0.2.38 与 main 一起推送，并核对远程 SHA 与 Release workflow；原有未跟踪文件继续保留。已确认 v0.2.37 Release workflow completed/success，整体产品完善目标保持进行。

## 第五十三轮：ACME 多域名站点证书实际同步（v0.2.39）

自动化证书包含多个 SAN 时，除了主域名证书文件，还会同步到所有 HTTPS 站点的主域名证书路径。例如 `example.com` 与 `www.example.com` 同批签发时，两个站点都会得到相同的新证书和私钥，站点配置不再继续读取旧文件。使用导入证书的站点不会被 ACME 自动化覆盖，也不会因命中域名而误报“已应用”。本地部署完成消息会说明实际同步的站点数量。

资源锁与实际写入范围保持一致，预先锁住所有匹配站点的证书和私钥文件，避免另一项自动化在等待或部署过程中覆盖其中一个 SAN 站点。多文件写入和证书记录保存失败时统一逆序恢复本轮所有旧文件，避免留下部分站点已更新、部分站点仍为旧证书的混合状态。

新增源码内回归覆盖 SAN 主域名站点、导入证书跳过、资源锁扩展和多文件回滚；证书自动化定向回归 30 项通过，核心 crate 编译检查通过。没有访问真实 CA、DNS、部署服务或用户站点，没有新增独立测试文件、依赖或数据库表；update.sql 仅记录现有 cert_automations JSON 的状态模板，未创建 migration，未启动前端 dev 或 build。

同步版本至 0.2.39，创建新的 annotated tag v0.2.39 并与 main 一起推送，核对远程 main、peeled tag 和 Release workflow；旧 tag 与原有未跟踪文件保持不变，整体产品完善目标继续进行。

## 第五十四轮：ACME 完整域名覆盖与本地维护保护（v0.2.40）

继续参考 ServBay ACME 的证书应用和续期流程（https://support.servbay.com/basic-usage/ssl/using-acme-to-issue-ssl-certificate）。发现上一轮以任一 SAN 命中即覆盖多域名站点，可能让其它域名失去有效证书；同时本地修复会把不由本地 CA 签名的 ACME 文件重新签成本地证书。本轮在实际文件、记录和维护入口一起修复。

部署计划要求证书覆盖站点全部域名，通配符沿用单标签匹配规则，导入证书站点继续保留。部分命中的非主输出站点跳过并在结果中说明；若自动化主输出或共享文件会覆盖一个未完全覆盖的站点，则在申请前拒绝，部署前再次复核。结果根据实际站点计数，不再把自动化主域名的独立文件算成一个站点；零命中明确显示仅保存证书。部署取得站点修改、服务生命周期和证书文件锁，遵循已有锁顺序，避免本进程编辑/修复与发布交错或锁顺序颠倒。

每个输出路径保存自己的 ACME 记录，事务内替换同路径的过时 site/acme 元数据，列表不再显示旧自签有效期。第二条记录失败时数据库整体回滚，所有已发布文件恢复；第二站点文件写入失败时只恢复已完成的输出，当前失败证书对沿用已有成对回滚。本地明确重新签发也同步更换证书类型；站点 HTTPS 编辑失败恢复原证书类型、记录及文件，包含原有 ACME 记录。

本地证书修复先识别、检查全部 ACME 文件，正常 ACME 不重新签为本地 CA；缺文件、私钥不符、过期或站点域名不覆盖时保留原文件，提示在自动化中重试部署或重新签发。旧版 SAN 副本与受管 ACME 主文件内容一致时补齐元数据并清理旧记录；只有 ACME 站点时不要求先创建本地 CA。该兼容识别需要原 ACME 文件可读取，无法据配置推断丢失文件的真实来源。

中英文说明同步完整覆盖、导入证书保留与零命中含义。本地部署开关增加可访问名称与描述，说明区可换行，开关不被挤压。隔离浏览器检查 320 / 390 / 1280 宽度，无横向溢出，开关状态真实切换，底部保存/取消可达；390 / 1280 截图已目检，没有页面异常，上下文已关闭，用户原标签保留套件页。

88 项证书相关源码内回归通过，包含新增的部分覆盖保护、真实站点数量、通配符、缺私钥/过期保护、旧 SAN 记录恢复、第二条数据库记录失败、Windows 文件占用回滚和站点编辑失败恢复；资源锁回归改用独立域名的本地输出重试，避免 DNS 锁掩盖文件锁验证。只使用临时 SQLite、临时证书与有限本机夹具，未操作真实 CA/DNS/服务器或系统配置。update.sql 记录现有 certs 表的事务模板，无 schema 变更，无新增独立测试文件或依赖。原生桌面与真实多平台部署仍需验收；整体目标继续进行。

Web 类型检查和 workspace 全 targets Rust 编译检查通过，git diff --check 通过；没有启动前端 dev 或执行前端 build。版本同步至 0.2.40，新增 annotated tag v0.2.40 并与 main 原子推送，按远程实际状态核对分支、peeled tag 和 Release workflow；保留旧 tag 及原有未跟踪文件。已确认 v0.2.39 Release workflow completed/success。

## 第五十五轮：站点直接选择 ACME 证书（v0.2.41）

接续 ServBay 签发后在站点 SSL 下拉选择证书的流程，新建和编辑站点现在共用本地 CA、已签发 ACME、导入证书三组选择器。候选接口只读取记录与受管文件，不调用会生成本地 CA 的 list_certs。列表读取真实 SAN、有效期、私钥匹配和用途；失效证书保留原因，全部域名必须覆盖。通配符只匹配一个标签，改域名不清空选择，当前证书缺失可见，读取失败可重试，保存失败保留草稿。向导重新打开会重置上次证书，最终摘要显示实际来源。

SiteRuntime 新增可选 acmeCertId，与 importedCertId 互斥，存入原有 runtime JSON。选择自动化主输出，排除按站点生成的同步副本，避免副本站点删除后丢失续签路径。保存与写配置前校验证书类型、规范标识、受管文件位置、真实材料和全部域名；Nginx 和 Apache 都引用被选证书路径。只使用 ACME 的站点无需先创建或信任本地 CA。HTTPS 关闭会保留已保存选择，健康报告按实际绑定关联，删除站点保留共享证书。

自动部署统计引用主证书的站点并按原流程重载，显式选择其它证书的站点不自动改绑；默认站点的同步输出若被其它站点选为另一 ACME 主证书，也保留原文件并说明。续签移除已绑定站点必需域名时在准备阶段和部署阶段拒绝，即使该站点暂时关闭 HTTPS。手动自签或其它站点切回本地 CA 不得覆盖被引用的 ACME 文件；站点改回默认证书时只解除自身引用，失败恢复原选择与文件。站点修改与证书维护按站点锁、服务生命周期锁、文件锁顺序协调。

配置导出保留绑定；备份不含私钥，导入时缺少所选证书会在任何导入写入前明确失败，需先恢复或重新部署。未新增数据库表、列或依赖，update.sql 记录现有 JSON 写入的兼容规则，没有操作真实用户数据库。

94 项证书相关源码内检查通过，覆盖主证书选择、双绑定/非法标识、多 SAN/通配符、真实过期/私钥错误/缺文件/路径错误、无本地 CA、续签共用文件、其它自动化保护、保存回滚、配置导入预检及健康关联。未新增独立测试文件。隔离浏览器验证 320 / 390 / 1280 下拉与编辑；320 / 1280 验证完整新建向导、提交绑定和重新打开的重置。证书加载失败重试、保存失败保留输入、域名变更阻止保存均通过，无页面异常或横向溢出，分割线左右 8px 留白并使用 dashed。UI 夹具仅注入隔离上下文，不写入产品代码；真实材料验证使用临时目录和 SQLite，没有访问真实 CA、DNS 或远程服务，也未改动用户系统配置。

Web 类型检查、workspace 全 targets Rust 编译检查和 git diff --check 通过。沿用已有开发页面检查，没有启动新的前端 dev 或执行 build。版本同步至 0.2.41，本轮使用新增 annotated tag v0.2.41 与 main 原子推送，并按远程实际结果核对 SHA 和 Release 状态；旧 tag 和原有未跟踪文件保留。已确认 v0.2.40 Release completed/success。真实桌面及多平台站点服务重载仍需验收，整体产品完善目标继续进行。

## 第五十六轮：证书列表与默认 Web TLS 解除本地 CA 依赖（v0.2.42）

证书列表改为只读，不再创建根 CA 或写入 CA 记录。根 CA 缺失、损坏、私钥不匹配或实际不是 CA 时，其它本地、ACME 和导入证书仍可查看，问题由健康报告单独展示。根证书导出直接读取已有受管文件，不依赖浏览列表时写库；信任操作只校验已有材料，不自动创建根 CA。历史本地记录及未识别的站点文件继续要求恢复原 CA，避免静默换根；路径匹配的 ACME 文件不阻止之后按需创建第一份本地 CA。

Nginx 和 Apache 默认欢迎页改用独立的 localhost 非 CA 自签证书，覆盖 localhost、127.0.0.1 与 ::1，不再向 Web 服务提供根 CA 私钥。默认材料成对写入，有效且距离到期超过七天时保留原文件；生成和更新不触碰原根证书、站点文件或系统信任。Apache 升级只替换已知受管 ssl-dummy 路径，保留用户自定义全局 TLS 路径和嵌套 VirtualHost 配置，不额外追加覆盖项；旧 ssl-dummy 文件保留。默认自签欢迎页不具备公共可信证书的信任保证。

空环境和仅 ACME/导入证书的环境不再误报根 CA 缺失或未信任。证书页使用共用健康查询区分尚未创建、需要修复、有效但未信任和已信任；损坏状态隐藏信任按钮，读取失败可以重试，其它证书保持可用。健康行正确显示无效图标和证书来源；没有有效期时显示状态未知，避免 1970 日期及虚假的过期天数。中英文说明明确本地 CA 的适用范围，长原因可换行。

98 项证书相关源码内检查和 9 项托管配置检查通过，覆盖只读列表/导出、纯 ACME 后按需本地签发、根材料五种故障保护、独立默认 TLS、重复生成幂等、受管旧配置升级和自定义配置保留。使用临时目录、SQLite 和生成的验证证书，没有访问真实 CA、DNS 或远程服务，也未改动用户系统信任、站点服务及配置。没有新增独立测试文件、依赖、表结构或 SQL 写入；本次没有数据库变更，未修改 update.sql。

沿用 ui-ux-coding 技能，在隔离浏览器验证 320 / 390 / 1280 宽度下的无 CA、损坏、缺文件、未信任及读取失败状态；模拟信任后状态更新，重试后恢复，其它证书可见。无页面异常或横向溢出，320 / 390 截图已目检。夹具只注入独立上下文并在结束时关闭，没有进入产品代码，用户原套件标签保留。Web 类型检查、workspace 全 targets Rust 编译检查和 diff 检查通过，未启动新的前端 dev 或执行前端 build。

同步版本至 0.2.42，新增 annotated tag v0.2.42 并与 main 原子推送，发布后核对远程分支、peeled tag 和 Release workflow；旧 tag 及原有未跟踪文件保留。已确认 v0.2.41 Release completed/success。原生桌面及真实跨平台 Web 服务仍需验收，整体产品完善目标继续进行。

## 第五十七轮：导入证书原位更新与站点绑定保留（v0.2.43）

参考 ServBay 第三方证书管理与续期说明（https://support.servbay.com/basic-usage/ssl/using-third-party-ssl-certificate），补齐导入证书到期后的更新入口。原先每次导入生成新标识，需要逐个站点重新选择；现在选择新证书链和匹配私钥后，在原受管位置更新，所有站点绑定和源文件保持不变。证书与私钥都丢失时，列表仍按已有站点引用展示缺失条目，可直接补回材料。

更新前读取真实 X.509、SAN、有效期、用途和私钥匹配；覆盖校验包含暂时停用和关闭 HTTPS 的所有引用站点，不能用只覆盖部分域名的续期证书破坏原绑定。非法标识、目录越界、链接及非普通目标拒绝写入。复用本地部署的成对暂存、权限保留、发布失败逆序恢复逻辑，不把私钥保存到数据库或新的配置备份。站点、服务生命周期与证书文件按现有锁顺序串行；工作登记覆盖写入与重载，退出/更新需等待完成，桌面 IPC 持有数据目录活动句柄到工作线程结束。

只对已启用 HTTPS 站点所引用且正在运行的 Nginx/Apache 应用更新，复用配置验证和平台重载顺序；停止中的服务保持停止，下次启动读取新文件。运行服务的入口缺失明确失败，不被重建逻辑跳过。重载失败保留已经验证的新证书，明确区分文件已保存和服务未完成加载，避免部分服务已加载后又悄悄回滚到旧文件；提示检查配置并启动/重启相应服务，运行中的服务可再次应用。

界面沿用 ui-ux-coding 技能及现有确认弹窗，展示引用站点、两个原生文件选择按钮、选中文件路径和固定操作区；缺任一文件禁止提交，取消选择和提交失败保留草稿，成功后重新打开不保留旧路径。重复提交及忙时关闭受保护，失败后同步刷新实际证书信息和服务状态。列表的长域名、路径和错误说明可换行，错误原因不再整段挤在不可收缩徽标里；操作区使用左右 8px 留白的虚线。浏览器预览拒绝真实更新，临时验证状态没有写入产品代码。

103 项证书相关源码内回归通过；本轮新增四项涵盖身份/绑定/源文件保留、通配符与关闭 HTTPS 站点覆盖、错误私钥/过期/损坏/CA 拒绝、全部文件丢失恢复、重载失败保留新材料及 Windows 第二文件发布失败恢复。另显式运行一项原生 Nginx 验证：使用已有二进制的临时副本、临时 SQLite 和动态端口，通过实际 HTTPS 返回内容及握手证书 DER 确认从旧证书切换为新证书，结束后验证进程退出和两端口关闭。首次夹具调用的配置参数不符与证书目录参数错误已修正后通过；没有改动真实用户服务、hosts、CA 或系统信任，没有访问真实 CA/DNS/外部部署接口。

隔离浏览器验证 320 / 390 / 1280 宽度的长错误和文件路径、缺文件拦截、取消选择、校验失败保留输入、重载失败后重试、成功重开重置及关闭后焦点恢复，均无页面异常或横向溢出；320 / 390 / 1280 截图已目检，上下文已关闭。Web 类型检查、workspace 全 targets Rust 编译检查和 diff 检查通过。未启动前端 dev 或执行前端 build，未新增独立测试文件或依赖。本次没有数据库变更，未修改 update.sql。

同步版本至 0.2.43，新增 annotated tag v0.2.43，与 main 原子推送并核对远程指向及 Release workflow；旧 tag 和原有未跟踪文件保持。已确认 v0.2.42 Release completed/success。原生文件选择、Apache 与 macOS 实机续期链路仍需验收，整体产品完善目标继续进行。

## 第五十八轮：网站证书监控与告警状态可靠性（v0.2.44）

结合 ServBay 的证书有效期与维护说明（https://support.servbay.com/basic-usage/ssl/using-ssl-certificate），完善已有网站证书监控；不把本项目的远端监控细节归为 ServBay 的相同功能。输入统一在后端解析，支持 HTTPS URL、域名与端口、IPv4、裸 IPv6 和带端口的方括号 IPv6；仅握手读取证书，不访问 URL 业务路径。拒绝非法 scheme、凭据、空主机、无效端口与歧义地址，规范化大小写、尾点和 IP 表示。随机生成身份，清空客户端伪造运行字段，在 SQLite 写事务内按实际端点去重。配置导入复用相同规则，在写入前预检，清空来源运行状态，同一备份内与本机已有目标都去重。

同一监控使用 OS 文件执行锁，网络探测后用 updated_at 比较更新，防止并发检查与删除/编辑后的迟到结果覆盖；推送结果再次保存也遵循版本比较，不使用 upsert 恢复已删除记录。列表坏 JSON 和标识不一致明确报错，不再静默隐藏；单项查询不依赖其它记录是否有效。失败保留最近成功读取的材料并明确显示新错误，尚未生效的证书归异常，过期不足一天不显示零天健康状态。TLS 仍允许读取过期、自签和域名不匹配的证书，但验证握手签名；设置总超时，并限制 runtime 退出等待，避免系统 DNS 阻塞任务拖住调用。

新增监控通知读写 IPC，修复 get_settings 未返回监控通知字段造成的错误回显；方式与 URL 同事务保存、快照读取，禁止一半成功。Webhook 使用正常 TLS 校验、有限超时和禁止重定向；钉钉、企微、飞书需明确的业务成功码，HTTP 200 不再直接当推送成功。失败保存不含凭据的独立原因，下次检查重试，成功后清除；稳定故障不重复弹窗，恢复后再故障可再次告警。前端去掉永久 host/state 去重，并修复异步事件注册在组件卸载后的清理。

界面补齐列表/通知读取失败重试、独立行忙态、重复提交保护、添加已保存但检查未完成提示；删除失败保持确认框，通知保存失败保留草稿，保存中禁止编辑和关闭。Webhook 隐藏明文输入；关闭通知时隐藏地址，启用时缺地址禁止保存。长地址和错误可换行，显示端口、完整到期与检查时间，新错误不会被旧签发者或绿色徽标遮住。标题说明独立换行，移除导致滚动错位的列表 layout 动画，分隔线左右各 8px 留白并使用 dashed。浏览器预览增删与设置读写使用一致内存状态，真实检查明确要求桌面端，不伪造检查成功。

证书相关检查累计 114 项通过，含 11 项监控检查与回环 HTTP 通知确认、事务回滚、坏数据、地址规范化、删除/编辑中的探测、执行互斥、故障恢复再告警、推送成功重试及发送期间删除保护。真实回环 TLS 读取自签、过期和未来证书，验证没有业务数据；停滞握手受总超时约束。沿用既有源码测试模块，没有新增独立测试文件或依赖；原生 Nginx 专项本轮保持 ignored，上一轮已单独验证。所有夹具使用临时目录、SQLite 和有限回环端口，没有操作真实 CA、DNS、通知服务、用户站点或系统配置。

隔离浏览器检查 320 / 390 / 1280 宽度，无横向溢出或页面异常；覆盖读取失败恢复、历史信息与新错误、IPv6 新增与去重、桌面检查限制、独立忙态、通知失败保留与重开回显、删除失败重试。首次浏览器检查发现 mock 对 HTTPS 地址的分隔判断错误，修复后通过；截图目检发现并修复窄屏标题与滚动动画错位。所有上下文在检查结束时关闭，用户原套件标签保留。参数化 SQL 记录同步 update.sql，无 schema 变化、无 migration、无需手工执行。

Web 类型检查、workspace 全 targets Rust 编译检查和 git diff --check 通过。版本同步至 0.2.44，发布流程必须新增 annotated tag v0.2.44 与 main 原子推送，并核对远程分支、peeled tag 和 Release workflow 实际状态；不移动旧 tag，保留原有未跟踪文件。已确认 v0.2.43 Release completed/success。本轮未启动前端 dev 或执行前端 build；整体产品完善目标继续进行，原生桌面和真实外部通知渠道仍需实机验收。

## 第五十九轮：导入证书导出与格式互操作（v0.2.45）

继续补齐第三方证书迁移与维护流程，参考 ServBay 第三方证书说明（https://support.servbay.com/basic-usage/ssl/using-third-party-ssl-certificate）。导入证书原先只有更新/删除，现在复用证书页已有弹窗导出 PFX、JKS、PEM 或 DER，无需手动查找受管文件。导入来源用内部 imported:<id> 标识与已有 cert-/acme- 标识区分，不新增命令、数据库记录或组件文件。允许过期证书导出归档；缺失私钥仍可导出仅含叶证书的 DER，含私钥格式会明确失败，不能把陈旧列表状态当作导出成功。

四种格式共用实际材料读取：复用已有 PEM/X.509 校验、4 MiB 限制、普通文件约束及证书/私钥匹配检查。PFX 和 JKS 将 PKCS#1 RSA、SEC1 EC 私钥转换为标准 PKCS#8，使用经匹配检查的叶证书算法参数，避免把传统私钥的裸 DER 直接当作 PKCS#8。保留输入证书链和叶证书顺序，PEM 保留原材料；不自动添加不存在的中间证书或创建本地 CA。沿用同目录临时文件原子替换及受管证书/私钥覆盖保护，错误不覆盖已有导出，也不改来源文件。导出登记后台工作并持有数据目录活动句柄及证书文件锁。

发现 JKS 0.3.3 的默认密码转换按 UTF-8 每字节补零，仅对 ASCII 与 Java 兼容；其 store/load 还忽略自定义转换选项。按 OpenJDK JavaKeyStore/char[] 规则使用 UTF-16 大端字节，私钥加密使用库的自定义转换，外层文件用现有 Encoder 写标准结构与同一编码的摘要，未引入新依赖或自写加密算法。用硬编码的中文和代理对字节，独立核对 SHA-1(password + Mighty Aphrodite + 文件内容)，避免同库错误往返产生假阳性。参考官方源码：https://raw.githubusercontent.com/openjdk/jdk/master/src/java.base/share/classes/sun/security/provider/JavaKeyStore.java 。

PFX 采用的 BMPString 密码不支持部分扩展字符，前后端提前校验并提供可操作提示，避免保存后才出现 ASN.1 错误；空密码文案改为“使用空密码”，不再声称完全不加密。当前 PFX 编码库还会把每张证书的主体写作 BMPString，因此证书名称含补充平面字符时，仍无法导出 PFX；明确提示选择 PEM/JKS/DER 并保留原文件，这项编码库限制尚未消除。JKS 在此情况下仅将别名回退到稳定标识，原证书主体不修改。弹窗按格式说明是否包含私钥，取消文件选择和导出失败保留草稿，成功关闭后重开清空密码并恢复 PFX。密码控件添加对应校验描述，底部分隔线左右 8px 留白并使用虚线；关闭后恢复到导入证书的导出按钮。浏览器 mock 导出明确拒绝真实写文件，不再返回不存在的假路径。

14 项本地 TLS 检查通过，包含新增三项覆盖双证书链与真实私钥往返、中文/扩展字符密码边界、非 BMP 名称、RSA/EC 传统格式转换、过期归档、密钥不匹配、缺私钥 DER、损坏材料、目标覆盖保护和发布失败清理。最初新增夹具的库返回类型调用错误已纠正；独立摘要检查揭示 store 忽略密码转换选项的问题，改用 Encoder 后通过。仅用临时目录、SQLite 和生成的证书，未操作真实用户证书、CA、站点或系统配置，没有新增测试文件。本次没有数据库变更，未修改 update.sql。

沿用 ui-ux-coding 的弹窗和表单规则，在隔离浏览器验证 320 / 390 / 1280 宽度的四种格式、密码边界、取消路径选择、失败重试、忙时禁用、重开重置和焦点恢复；导入证书标识与输出扩展名正确，无横向溢出或页面异常，三种宽度截图已目检。夹具只注入隔离上下文，结束后全部关闭，用户原套件标签保留。真实桌面文件选择、IIS 与 Java 应用导入仍需实机验收，当前通过材料往返、Rustls 私钥匹配及 OpenJDK 摘要规则验证格式。

Web 类型检查、workspace 全 targets Rust 编译检查及 git diff --check 通过。同步版本至 0.2.45，必须新增 annotated tag v0.2.45 与 main 原子推送并核对远程指向和 Release workflow 状态，不移动旧 tag。已确认 v0.2.44 Release completed/success。未启动前端 dev 或执行前端 build，保留原有未跟踪文件；整体产品完善目标继续进行。

## 第六十轮：PFX Unicode 密码与原生互操作（v0.2.46）

解除上一轮记录的 PFX 限制：中文、emoji 和补充平面汉字密码均可导出，叶证书及链证书的主体包含这些字符时也可保留原始 DER。确认 p12-keystore 0.3.2 的高层 writer 仍将密码及每张证书主体强制写为 BMPString；复用已有 RustCrypto ASN.1、PBES2、PKCS#12 KDF 和 HMAC 组装标准 PFX，没有自行实现密码学算法或调用外部程序完成产品导出。直接依赖 pkcs12、pkcs5 和 sha2 0.11，均为现有锁文件中已使用的版本，原 p12-keystore 移到开发依赖用于独立读回，Cargo.lock 仅调整直接依赖关系及本项目版本。

证书 safe 和私钥 bag 继续使用 PBKDF2-HMAC-SHA256 / AES-256-CBC，完整性使用 PKCS#12 KDF / HMAC-SHA256，维持 10,000 次迭代并使用独立随机盐和 IV。PBES2 密码沿用 UTF-8，MAC 密码显式转换为以零结尾的 UTF-16BE，支持代理对及空密码；临时密码字节和 MAC 密钥使用已有 Zeroizing。省略证书 bag 可选的 friendlyName，通过叶证书摘要关联私钥，私钥友好名沿用已有别名回退规则；不修改证书主体或链顺序。保留材料匹配检查、原子导出、受管目录覆盖保护、后台工作登记与文件锁。前后端仅拒绝原生接口会截断的嵌入 NUL，更新中英文提示。

14 项本地 TLS 回归通过，覆盖现有归档、传统 RSA/EC 私钥转换、坏材料保护和新增 Unicode 导出行为。在已有源码测试模块加入默认忽略、显式运行的原生互操作检查，没有新增测试文件。实际使用 PowerShell 7 的 Windows X509Certificate2Collection/EphemeralKeySet 及 OpenSSL 3.2.3，验证 RSA/EC × ASCII/中文/emoji 与扩展汉字/空密码共 8 组材料。两种读取器均核对完整双证书链和原始 DER；Windows 执行私钥签名及公钥验签，OpenSSL 输出材料经 Rustls 核对私钥匹配，并核对加密算法及 MAC 参数。两种读取器均拒绝错误密码和被篡改的 MAC。最初原生夹具在捕获预期异常后保留 PowerShell 失败退出码，已改为显式成功退出后完整通过。所有材料均在临时目录，Windows 仅内存导入，没有修改系统证书库、信任状态或真实用户文件。

隔离浏览器在 320 / 390 / 1280 宽度验证 NUL 提示、完整 Unicode 密码传参、取消选择和失败保留输入、忙态、重试、四种格式、重开清空及关闭后焦点恢复。分隔线保持左右各 8px 留白和虚线，没有页面异常或横向溢出，三种宽度截图均已目检，检查后关闭上下文并保留用户原套件标签。Web 类型检查、workspace 全 targets Rust 编译检查通过；没有启动前端 dev 或执行前端 build。本次没有数据库变更，未修改 update.sql。

根 AGENTS.md 已明确每次提交、push 或发布必须新增 annotated tag，不能只推分支。本轮同步全部包、crate、Tauri 及界面回退版本至 0.2.46，新增 v0.2.46 并与 main 原子推送，推送后核对远程 SHA 和 Release workflow 实际状态；不移动旧 tag，保留原有未跟踪文件。已确认 v0.2.45 Release completed/success。IIS 实际绑定与 macOS 原生导入仍需实机验收，整体产品完善目标保持进行。

## 第六十一轮：通用服务端口与实际配置保持一致（v0.2.47）

参照 ServBay 套件/服务管理说明（https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management），继续检查安装版本与服务实际状态。确认套件清单与安装记录已有并集合并；本轮发现通用服务先写默认配置再自动回落端口，Caddy、MariaDB、Qdrant 等可能仍使用旧配置端口，界面却显示新端口。调整为启动前先确定完整端口组，再依次准备配置、初始化、命令参数和环境变量，各阶段使用相同的主端口及派生端口。

读取端口覆盖和已分配记录使用可报错接口，覆盖优先；非法端口明确失败，不静默使用默认值。选择端口使用实际 TCP 绑定检查，同时验证模板 args/env/config/init/stop 中声明的派生端口，CoreDNS 还检查 UDP。拒绝非法占位符、偏移和 1–65535 范围外的端口，不把溢出值钳到 1 或 65535。自动回落整体移动端口组，先找相邻端口，再使用系统分配候选；安全档首次分配仍自动找可用组。未声明端口调整参数的模块沿用原生默认端口，不能只更改健康检查端口；遇到不受支持的历史覆盖明确报错。端口检查不是持续占有套接字，外部进程在预检后抢占仍可能导致实际启动失败。

已有配置只同步模板声明的端口数字，保留其他自定义行、缩进、行尾注释和 CRLF；INI 按节定位，YAML 按父级路径定位，其他配置段同名字段保持不变。模板项缺失或重复时拒绝猜测，显示具体文件和模板行，保留整个原文件；已有旧版本生成的端口也可更新，不要求正好等于当前分配记录。配置路径复用受管目录检查，拒绝路径外跳、目录、大于 1 MiB 或非 UTF-8 文件。写入复用现有备份、原子替换和原内容比较，不因重复启动生成相同内容备份。配置准备失败不保存新端口；准备成功后端口分配与自动回落覆盖在同一 SQLite 事务内保存，第二句失败整体回滚。update.sql 已同步参数化记录，无 schema 变更、无 migration，未操作真实用户数据库。

新增检查位于已有 generic.rs 源码内，没有创建测试文件。6 项通用启动检查通过，覆盖 Caddy/MariaDB/Qdrant/r-nacos 模板同步、INI/YAML 配置段隔离、注释及换行保留、重复写幂等、错误/歧义/越界配置保护、附加端口被占、已绑定未监听、安全档、端口覆盖优先和事务失败回滚。显式运行第 7 项原生检查：下载官方 Caddy 2.11.4 并核对清单 SHA-256，在临时数据目录和回环地址实际启动、停止，两次制造主端口冲突，验证 HTTP 返回、真实监听端口与保存记录一致，自定义响应头和注释保留，再次重启端口稳定；有自动清理守卫，结束后停止临时进程。另有 11 项既有安装/卸载/版本切换检查通过，原官方 Nginx 下载检查保持 ignored。初始夹具的两个 API 名称错误已修正后通过；所有修改均经过最终 workspace 全 targets Rust 编译、Web 类型及 diff 检查。

本轮没有修改界面布局；仅同步界面版本回退及包/crate/Tauri 版本至 0.2.47。未启动前端 dev 或执行前端 build，没有新增依赖，Cargo.lock 只更新本项目版本。已确认 v0.2.46 Release completed/success；本轮必须创建新 annotated tag v0.2.47，与 main 原子推送并核对远程 SHA 和构建实际状态，保留原有未跟踪文件。

本轮原生验证覆盖 Windows Caddy，不等于已验收所有通用服务。Tomcat/Neo4j/SFTPGo 的原生端口适配、r-nacos .env 的实际加载位置、通用健康检查的监听 PID 归属和更多服务的真实多端口启动仍需继续完善；macOS 实机亦待验证。整体产品完善目标保持进行。

## 第六十二轮：r-nacos 配置加载与真实启动故障识别（v0.2.48）

核对 r-nacos v0.8.7 的官方部署参数与启动源码（https://github.com/nacos-group/r-nacos/blob/v0.8.7/book/src/deplay_env.md ，https://github.com/nacos-group/r-nacos/blob/v0.8.7/src/main.rs），确认未指定 -e 时只寻找工作目录的 .env，而原清单将配置写在 etc/rnacos/<version>，服务却从程序目录启动。本轮为 Windows 清单的 0.8.4–0.8.7 显式传入托管配置路径，并将 HTTP、gRPC、独立控制台三个端口统一由主端口及 +1000/+2000 派生。受管端口及原数据目录通过子进程环境传入，不受继承环境里的旧端口干扰；自定义变量仍由 r-nacos 按其原生 dotenv 规则加载。

安装时保存的 .niceenv-package.json 优先级高于新清单，因此只改清单无法修复已安装服务。读取安装快照、查找安装条目、合成远端版本模板和套件列表时，仅识别并升级原内置运行描述的完整形状；实际入口、下载来源、校验值及用户修改过的 args/env/cwd/template 保持不变。兼容在内存完成，不覆盖安装快照。新配置去除上游已废弃或未使用的数据库文件参数；已有文件保留这些行，只对旧模板生成的受管路径补充引号及转义，兼容空格路径。端口同步、用户注释、CRLF、备份、原子替换和原内容比较沿用上一轮实现。

上游忽略 dotenv 加载错误，可能在配置只读取一部分后继续启动。为保持相同语法，新增与上游一致的 dotenv 0.15 解析依赖，复用已有 tempfile 在受管配置目录暂存并完整校验，成功或失败均自动删除暂存文件；不修改 NiceEnv 的进程环境，不把配置值写进错误信息。非法赋值、引号、重复键和 NUL 明确拒绝，在保存配置、提交端口和创建进程前返回错误。实际变量插值仍由子进程执行，避免在父进程提前展开而误用旧端口。

启动检查除 TCP 连通外，核对三个监听端口属于本次启动的进程，并请求官方 /health 接口。只检查本次启动分界后的日志；内部线程发生 panic 时，即使 HTTP 仍能应答，也返回 SERVICE_RUNTIME_PANIC 并清理进程，不能把服务显示为正常。上一轮启动的错误日志不影响本次判断；失败或超时复用原停止流程。此检查覆盖启动阶段，不能保证运行后不会出现新的上游故障。

8 项通用启动检查、12 项安装/卸载/版本切换检查通过；新增覆盖旧快照与自定义描述保留、含空格路径、无父进程环境污染、坏配置原样保留、三端口归属和旧 panic 日志隔离。显式执行官方 r-nacos 0.8.7 Windows 二进制的故障检查，通过实际启动、错误识别、进程清理及端口关闭验证；下载前后核对清单 SHA-256。所有检查位于已有 Rust 源码模块，没有新增测试文件；仅使用临时目录、SQLite 和回环监听，不操作用户实际配置或服务。

完整原生读写验收尚未通过：0.8.7 多次在 raft/filestore/raftapply.rs 初始化时出现 Option::unwrap panic，HTTP 写入仍返回 true，但读回失败。对照官方 0.8.6 曾完成一轮真实配置写入、端口冲突回落、重启读回、启用鉴权和控制台失败清理；在增加启动故障识别后的重复检查中，0.8.6 也触发上游 panic，因此不能称这两个版本已稳定可用。保留默认忽略的严格读写验收以继续定位，不放宽断言来假装通过。Windows 停止后 TCP 释放延迟也曾使夹具立即重占旧端口失败，冲突验证改为新端口组，关闭检查使用有上限的短暂等待。最初夹具错误调用不存在的 unregister 已纠正。上游启动稳定性及正常读写验收仍是明确待办。

Web 类型检查、workspace 全 targets Rust 编译检查及 diff 检查通过；未启动前端 dev 或执行前端 build，没有修改页面布局，仅同步包/crate/Tauri 与界面回退版本至 0.2.48。本次没有数据库变更，未修改 update.sql；Cargo.lock 仅加入 dotenv 和更新本项目版本。已确认 v0.2.47 Release completed/success。本轮新增 annotated tag v0.2.48，与 main 原子推送并核对远程指向及 Release 实际状态，旧 tag 和原有未跟踪文件保持不变。

SFTPGo 的 portable 包选择、托管端口、跨版本数据与密钥目录仍在调查，未混入本轮发布；其他通用服务及 macOS 原生运行也需继续验收。整体产品完善目标保持进行。

## 第六十三轮：SFTPGo 安装包、托管端口与跨版本状态保留（v0.2.49）

核对 SFTPGo 官方 v2.7.5/v2.7.6 portable 包及对应配置、服务、资源定位和 Windows 安装脚本源码，确认原清单的 2.7.3–2.7.5 错把需要管理员权限的安装器作为服务程序。本轮改为官方 portable ZIP 并同步下载大小和 SHA-256，2.7.6 保持 portable。可下载的旧官方条目兼容升级为正确包；已安装快照保留实际入口，不把安装器假装改名成服务，启动时明确提示卸载该程序版本后重新安装，沿用卸载保留配置和数据的行为。只有完整匹配原内置运行描述时才补充托管端口，用户自定义运行参数保持不变。

SFTP 和 Web 端口通过上游环境变量设置，默认 2022/8080，Web 固定由主端口 +6058 派生，复用端口组冲突预检和整体回落。启动检查确认两个监听端口均属于本次服务进程并稳定存在，失败或超时停止服务。r-nacos 继续复用原有多端口检查，没有放宽启动故障识别。

配置目录首次运行选择唯一非空旧目录并原位复用，没有旧状态时使用 etc/sftpgo/shared；多份旧目录明确拒绝自动选择或合并。仅在实际启动成功后将相对目录保存为现有 SQLite settings 表的 sftpgoConfigDir，保存失败停止本次进程。切换版本继续使用绑定目录，不复制或移动数据库和 SSH 主机密钥；路径越界、链接和目录联接拒绝使用。绑定目录、原配置或可确认的本地数据库/主机密钥缺失时明确报错，不重新生成空库或更换主机身份。

配置支持 JSON/YAML，限制大小、校验语法并显式传入配置文件路径；缺少初始配置时优先沿用旧版本 portable 原配置。官方 portable 使用 Bolt，不能强制改为 SQLite。默认模板、静态文件、OpenAPI 和 SMTP 模板从当前程序包加载，保留自定义资源路径、运行环境和 env.d 声明。env.d 只扫描键名，实际引号、值和插值由上游 gotenv 处理；数据库或密钥被 env.d 覆盖时，不根据文件配置错误推断本地状态。多配置文件明确报错，其他格式需使用自定义模块。

10 项通用启动、13 项安装/卸载及 1 项显式原生 SFTPGo 检查共 24 项通过，均位于已有 Rust 源码模块，没有新增测试文件。原生验证使用校验过的官方 Windows 2.7.5/2.7.6 程序临时副本、临时 SQLite 管理记录和回环端口：实际登录管理 API 与管理台、创建用户、SFTP 密码认证、上传并读回文件；切换版本后核对原管理员和用户、文件内容、SSH 指纹及自定义配置原字节均保留。分别制造 Web/SFTP 端口冲突，确认整体回落；临时移开数据库或主机密钥时拒绝启动且不重建，损坏 JSON 也被拦截。临时服务由清理守卫停止，没有操作用户实际服务、数据库或配置。

Web 类型检查、nsb-core 编译检查、workspace 全 targets Rust 编译检查和 diff 检查通过。未启动前端 dev 或执行前端 build，没有修改 UI 布局或新增依赖，Cargo.lock 仅同步本项目版本。update.sql 已记录现有 settings 表的参数化幂等写入，无表结构变更、无 migration，无需手工执行。版本统一为 0.2.49，本轮必须新建 annotated tag v0.2.49，与 main 原子推送并核对远程分支、peeled tag 和 Release 实际状态；旧 tag 及原有未跟踪文件保持不变。已确认 v0.2.48 Release completed/success。

本轮原生验收范围为 Windows SFTPGo 2.7.5 → 2.7.6，其他平台及其他服务仍需继续验证。多份旧配置目录目前提供明确恢复提示，尚无目录选择界面；管理台直接入口及 UI 完善仍待继续。r-nacos 上游启动 panic 与严格读写验收仍未解决，未宣称稳定可用。整体产品完善目标保持进行。

## 第六十四轮：网页打开与文件定位可靠性（v0.2.50）

继续参照 ServBay 套件和服务管理中的快捷操作（https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management）。检查管理台入口时发现桌面所有网页打开共用 cmd /c start，网址可能被命令解释器再次解析；配置编辑器的“打开所在文件夹”又把文件本身交给文件关联程序。本轮先修复这个共用入口：沿用已有 tauri-plugin-opener 2.5.5，Windows 依赖树确认启用 open 5.4.4 的 shellexecute-on-windows 且未启用 insecure，最终使用 ShellExecuteExW；移除应用自行拼接的 cmd/start 路径，不新增依赖。

网页仅接受完整 HTTP/HTTPS 地址，统一解析后保留查询、片段、编码字符和 IPv6，拒绝控制字符、反斜杠、登录凭据、无主机及非法端口，不能借网页入口执行本地文件或其他协议。操作系统打开失败明确返回可重试错误，不把包含令牌的完整网址带进错误详情。桌面 IPC 在工作线程执行系统打开操作。目录入口先确认实际存在，普通文件通过系统文件管理器定位而非执行；目录直接打开。更新目录复用该入口，创建失败也不再被忽略。

浏览器预览的网页打开移到点击同步阶段，避免 mock 延迟丢失用户手势。先打开空白窗口，断开 opener，再通过带 noopener/noreferrer 的链接导航；弹窗被拦截明确提示允许后重试，导航异常关闭空白窗口并报错。预览无法打开本机目录时明确要求使用桌面端，不再假装返回成功。原网页调用方及错误提示复用不变，没有修改业务数据、界面布局或下拉样式。

3 项桌面 Rust 源码内检查通过，覆盖完整网址交付、危险或非法输入不触达系统打开回调、启动器失败且不泄露目标、中文/空格/符号目录、普通脚本文件只识别为定位目标以及路径不存在。另通过 27 项内联 TypeScript 检查，涵盖同组 URL、同步开窗、失败清理与桌面 IPC 传参；没有新增测试文件。系统默认浏览器和文件管理器的实际桌面窗口尚未通过自动化验收，Rust 检查使用可控打开回调，不将其称为已验收系统 UI。

按 ui-ux-coding 技能使用已有前端服务，在隔离浏览器真实点击站点网页、目录和被拦截的弹窗入口；目标网页通过隔离路由返回验证 HTML，不访问用户真实站点。确认新窗口地址正确、opener 为空、Referer 未发送且 document.referrer 为空，错误提示可见。320/390/1280 宽度无横向溢出或页面异常，390 截图已目检；首次截图处于侧栏宽度动画途中，待动画结束确认侧栏为原定 68px，因此没有据此改动布局。验证创建的上下文在结束时关闭，原套件标签保留。

Web 类型、workspace 全 targets Rust 编译和 diff 检查通过，未启动前端 dev 或运行前端 build。本次没有数据库变更，未修改 update.sql；未修改依赖，Cargo.lock 仅同步本项目版本。版本统一至 0.2.50，本轮必须新建 annotated tag v0.2.50 与 main 原子推送，核对远程指向及 Release 实际状态，不移动旧 tag。已确认 v0.2.49 Release 的 Windows、macOS arm64/x64 三项构建全部 completed/success。服务管理台直接入口、SFTPGo 多目录选择、r-nacos 上游故障以及更多服务原生验收仍需继续，整体产品完善目标保持进行。

## 第六十五轮：服务管理台快捷入口（v0.2.51）

继续参照 ServBay 套件与服务管理的快捷操作（https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management）。总览卡片、列表和套件页共用管理台按钮，支持已知运行描述下的 SFTPGo、Mailpit、MinIO、Consul、r-nacos 和 Qdrant。复用现有按钮、提示、错误处理和网页打开入口；显示查询中状态并拦截重复点击，服务停止时禁用，查询期间停止或组件卸载后不再打开网页。中英文标签和键盘操作保持完整，浏览器预览明确提示使用桌面端，不伪造本机服务地址。

入口在准备启动时按实际分配端口、配置和运行环境计算，健康检查成功后仅记录到内存；运行期间修改文件或计划端口不改变当前进程入口，停止或发现进程退出时清除。SFTPGo 读取 JSON/YAML、run.env 和继承环境，识别 Web Admin 开关、HTTPS、IPv6 和 web_root，Web 端口按主端口 +6058 派生；env.d 自定义地址且不能从显式环境确认时给出具体提示，不猜测上游 gotenv 的最终值。Mailpit 支持 --webroot 两种参数写法和 MP_WEBROOT。未知运行描述、非回环网卡地址以及未记录启动入口的收养进程给出恢复提示；后者需重启服务后获取入口。

桌面异步 IPC 在工作线程执行，持有数据目录活动句柄并复用服务生命周期互斥。查询要求服务 Running、有活 PID，所有同端口监听均属于当前服务，再以不使用代理、不携带凭据、不跟随重定向的本机 GET 检查响应头；HTML 成功响应、带 Location 的跳转或 401 可打开，普通 JSON API 不能误报管理台。首选记录协议，失败后在同一端口尝试另一协议，每次 1.5 秒上限，探测后再次核对状态和归属。仅本机无凭据探测接受自签证书，实际浏览器仍正常校验证书。URL 按路径段编码并处理点段，不把配置路径拼接为查询参数或系统命令。

通用启动源码内检查覆盖六种入口、端口派生、自定义路径与环境覆盖、HTTPS/IPv6、关闭状态、监听归属、无认证 GET、不跟随外部跳转、错误响应和停止清理。显式原生验证使用已校验的官方 Windows SFTPGo 2.7.5/2.7.6 和 Mailpit 1.31.1，确认端口冲突回落后的实际管理台页面及 Mailpit 邮件列表 API 可访问，运行中修改计划配置不影响入口，停止后拒绝打开；SFTPGo 跨版本保留管理员、用户、真实 SFTP 文件、自定义路径和 SSH 指纹。所有检查沿用已有 Rust 源码测试模块、临时数据目录和回环端口，没有新增测试文件或操作用户实际服务。临时进程在检查结束时清理。

按 ui-ux-coding 使用既有前端服务，在隔离浏览器上下文验证总览卡片、列表和套件页，覆盖成功、失败重试、忙态、重复点击、停止禁用、查询期间停止和键盘 Enter。真实新窗口使用隔离路由返回验证 HTML，核对目标路径和 window.opener 为空；320/390/1280 宽度无横向溢出或页面异常，截图已目检。夹具仅注入隔离上下文，结束后关闭并保留用户原套件标签，没有启动前端 dev 或执行前端 build。

Web 类型检查、workspace 全 targets Rust 编译检查、13 项通用启动和 11 项服务状态回归通过，diff 检查无空白错误。5 项原生专项在默认检查中保持 ignored，其中 SFTPGo、Mailpit 两项已在本轮显式执行通过。没有数据库、schema 或依赖变更，未修改 update.sql；Cargo.lock 仅同步本项目版本。版本统一至 0.2.51，必须创建新 annotated tag v0.2.51，与 main 原子推送并核对远程分支、peeled tag 和 Release 实际状态，不移动旧 tag，保留原有未跟踪文件。已确认 v0.2.50 Release completed/success。

本轮原生验收覆盖 Windows SFTPGo 和 Mailpit，其他四种服务及 macOS 尚未完成原生管理台验收；Qdrant 包不含 dashboard 时会明确报错。SFTPGo 多配置目录选择、env.d 地址解析、非回环快捷入口及 r-nacos 上游启动 panic/严格读写验收仍需继续完善，不能将入口支持等同于所有服务已稳定可用。整体产品完善目标保持进行。

## 第六十六轮：Qdrant 管理台资源补齐与原生数据验收（v0.2.52）

沿用 ServBay 服务管理的完整快捷操作思路，继续检查上一轮入口对应的真实程序。下载并核对官方 Qdrant v1.19.1 Windows 包大小和 SHA-256，确认 ZIP 只有 qdrant.exe；上游 /dashboard 需要另行提供 static 目录，因此原“自带 Web 面板”描述缺少实际安装步骤。参考官方 Web UI 文档（https://qdrant.tech/documentation/web-ui/）及 v1.19.1 的 src/actix/web_ui.rs、tools/sync-web-ui.sh、src/settings.rs，安装器现在为官方原生运行描述补齐 qdrant-web-ui v0.2.18 的 dist-qdrant.zip，固定 7,189,135 字节及 SHA-256 fdce24c04ec1627d2369cb8fe610ee06ad9236f82aad214aa7f294ac37372859，不依赖随时间变化的 latest 内容。

附属资源复用现有下载器、镜像设置、校验、续传、取消和安装任务互斥，采用独立缓存名，不能覆盖主程序压缩包。新安装在暂存目录内完成程序和管理台后统一发布；旧安装仅原子发布 static，保留程序、数据库和配置。资源包含官方 index.html、assets、openapi.json 及发行包内的许可证清单。已有有效静态目录不覆盖，已有残缺目录明确提示先备份修复；目录外跳和链接通过受管路径检查拦截。取消或提取失败不留下半成品目录，不创建额外包记录或引入依赖。

已安装旧版本点击管理台时，缺少默认静态文件会显示“补齐并重启”弹窗，明确约 7 MB 下载量和短暂中断连接。复用 ConfirmDialog、错误提示和安装进度，支持失败重试与取消下载；前端不自行执行无条件重启。新 repair_service_web_ui IPC 在后端核对 Qdrant、版本、运行状态及缺失原因，下载期间维持服务运行，完成后在生命周期锁内核对原版本和 PID，再重启并确认入口。中途主动停止或换版本时，资源可保留但不会自动拉起；重启失败也明确返回错误。工作线程持有数据目录活动句柄，实际网页打开仍沿用上一轮的系统入口。

Qdrant 入口读取本次启动的静态目录、启用开关、监听地址和 TLS 配置，以及 run.env/继承环境的对应覆盖，尊重自定义目录和关闭状态。启动检查同时确认 HTTP/gRPC 两个端口归属当前服务并稳定存在；共有监听归属检查也拒绝同端口属于其他进程的情况。首次原生验收在立即停止后卸载遇到 Windows 临时文件占用；卸载现在在确认停止后对指定 Windows 文件错误最多重试 2 秒，每次重新检查受管路径，不修改文件权限，永久失败仍保留安装记录并报错。

14 项通用启动、15 项安装/卸载和 5 项下载回归共 34 项通过，包括静态目录保护、取消不发布、关闭/自定义/TLS/环境配置及真实 Windows 文件句柄延迟释放。新增验证位于已有 Rust 源码模块，没有创建测试文件。显式原生 Qdrant 验收通过：使用官方程序和 UI、临时目录/SQLite/回环端口，制造 gRPC 端口冲突验证整体回落，真实创建 collection 和写入带中文 payload 的向量，旧安装补齐后读取实际管理台 HTML/JavaScript，重启及卸载重装后读回原向量并保留配置注释。还在下载事件中实际停止服务，确认补齐后返回状态变化且没有拉起，再手动启动并完成正常修复路径。结束后停止并核对 PID 退出；未操作用户实际数据或服务。

按 ui-ux-coding 复用现有弹窗，在隔离浏览器验证错误保留、失败重试、忙时禁用、取消安装、正确版本传参、成功新窗口地址及 opener 为空。320/390/1280 宽度没有横向溢出或页面异常，弹窗间距和截图已目检，关闭后焦点恢复管理台按钮。最初浏览器检查在新窗口从空白页导航时过早读取上下文，改为等待目标 URL 和内容后通过；没有把工具等待错误作为产品缺陷修改。所有验证上下文均关闭，用户原套件标签保留。未启动前端 dev 或执行前端 build。

最终 Web 类型检查、workspace 全 targets Rust 编译及 diff 检查通过。本次没有数据库或 schema 变更，未修改 update.sql；Cargo.lock 仅同步本项目版本。版本统一至 0.2.52，发布必须新建 annotated tag v0.2.52，与 main 原子推送并核对远程指向和实际 Release 状态，不移动旧 tag。已确认 v0.2.51 Release completed/success，原有未跟踪文件保持不变。

原生验收范围为 Windows Qdrant v1.19.1 与官方 UI v0.2.18；其他 Qdrant 版本、macOS、MinIO/Consul 管理台以及服务端自定义配置叠加仍需进一步验收。Qdrant 快照目录目前还可能使用程序目录下的上游默认路径，跨版本快照保留需继续完善；本轮真实持久化验证覆盖 collection/向量数据。SFTPGo 多配置目录选择与 r-nacos 上游故障仍未解决，整体完善目标保持进行。

## 第六十七轮：Qdrant 跨版本快照保留（v0.2.53）

继续处理上一轮发现的默认快照目录问题。参考官方快照文档（https://qdrant.tech/documentation/concepts/snapshots/）与上游配置加载实现，Windows 清单中的四个 Qdrant 版本均明确将快照写入 data/qdrant/snapshots。对于仍使用旧安装描述和旧配置的官方原生单实例，启动时通过子进程环境修正上游 runtime 内的默认路径，不重写用户配置、注释或 CRLF；已有自定义数据目录和运行描述不纳入自动处理。快照路径及 local/s3 类型依次读取 run.env、继承环境、显式配置、local、RUN_MODE 和基础配置，支持 YAML/YML/JSON，Windows 环境变量名按大小写不敏感匹配并拒绝重复名称。未被高优先级值覆盖的多份同名配置或未支持的 TOML/INI/RON/JSON5 格式明确报错，提示在主配置写明设置，不猜测路径。

启动前扫描已安装官方版本的旧默认 snapshots 目录，卸载时仅处理目标版本，并在删除 runtime 和安装记录前完成保留。先扫描受管路径、拒绝链接和特殊文件、计算 SHA-256，检查同名文件及祖先路径冲突；同名同内容复用，异内容中止且不覆盖。缺少的文件在同卷暂存，校验和刷盘后独占发布，再将原整个目录移至 backup/qdrant-snapshots-<version>-<随机>/snapshots。源目录退出扫描范围，因此通过 Qdrant API 删除的快照不会在下次启动时再次导入。发生中断时可重试，可能已有部分新文件发布，但不覆盖目标既有文件，原目录保留到该来源备份完成；这不是整个目录的事务。旧目录仍有内容且服务存活时拒绝迁移；来源已保留时，卸载未使用旧版不会打断新版 PID。用户自定义快照仍在将删除的 runtime 内时明确阻止卸载，并提示先搬移和更新配置。

17 项通用启动及 15 项安装/卸载回归共 32 项通过，新增覆盖快照合并、原目录备份、冲突保护、删除不复活、配置优先级、环境展开、大小写环境名、原配置字节保留、自定义 runtime 目录卸载保护和活动进程保护。验证位于已有 generic.rs 源码模块，没有新增测试文件。显式运行两项原生检查均通过：使用经官方 SHA-256 核对的 Qdrant 1.19.0、1.19.1 与 UI 0.2.18，在临时目录、SQLite 和回环地址复现旧版默认路径，实际创建集合、写入中文 payload、生成集合快照与全库快照。分别走“先卸载旧版再安装新版”和“先切换新版再卸载未使用旧版”，通过新版 API 下载并核对快照摘要，从真实快照恢复原向量，确认新快照落入独立数据目录、删除后重启不复活、卸载新版后快照仍在。结束后确认无 fixture.exe 或 qdrant.exe 临时进程残留，没有操作用户实际数据。

初次普通检查的 Windows 路径分隔符比较已改为规范化比较；先前并行编译与运行同一 Windows 测试可执行文件发生 LNK1104，改为顺序执行后通过。最终两项原生检查共耗时 83.22 秒。Web 类型检查、workspace 全 targets Rust 编译检查、版本及四个清单模板检查、git diff --check 均通过。没有新增依赖、schema 或数据库变更，未修改 update.sql；Cargo.lock 仅同步本项目三个 crate 的版本。未改 UI 布局、启动前端 dev 或执行前端 build。

根 AGENTS.md 已固定每次提交、push 或发布必须新增 annotated tag 的要求。本轮统一版本至 0.2.53，必须将发布提交与新 tag v0.2.53 原子推送，再核对远程分支、tag 及 Release workflow 的实际状态；不移动旧 tag，保留原有未跟踪文件。已确认 v0.2.52 Release completed/success。原生验收范围为 Windows Qdrant 1.19.0→1.19.1；macOS、S3 和自定义运行描述仍需继续验收。SFTPGo 多配置目录选择、r-nacos 上游 panic、MinIO/Consul 原生管理台等仍待完善，整体产品完善目标保持进行。

## 第六十八轮：SFTPGo env.d 管理台入口与状态保护（v0.2.54）

继续补齐服务快捷入口的实际可用性。确认原代码仅收集 env.d 的变量名，遇到其中声明的管理台地址就拒绝提供入口；数据库名称或 SSH 主机密钥存在环境配置时，也会跳过原有文件缺失检查。核对官方 SFTPGo 2.7.6 internal/config/config.go、go.mod 及其 gotenv v1.6.0 源码，官方配置说明为 https://docs.sftpgo.com/2.7/config-file/。Context7 查询失败后直接核对官方文档与版本源码；缓存源码与重新下载的 2.7.6 文件 SHA-256 一致。

现在一次读取受管 env.d 文件，按上游文件名排序，解析继承环境、展开后的 run.env、文件内赋值及文件间的覆盖顺序。同一文件同名变量最后一次赋值生效，进程已有值优先，后续文件不覆盖已存在值，空环境值也保留。支持 export、冒号赋值、注释、单双引号、多行内容、转义、变量插值、CR/LF/CRLF、UTF-8 BOM 与 UTF-16 大小端 BOM；Windows 变量名按大小写不敏感处理，同一来源的大小写重名明确拒绝，避免上游 map 遍历导致不确定结果。沿用上游 1 MiB 文件限制，不读取目录；非法编码、坏赋值、缺失引号和 NUL 在启动前报出文件及行号，不回显配置值、不改动原文件、不执行 shell、不修改 NiceEnv 的进程环境。解析规则参考的 gotenv MIT 版权及许可已保留在源码注释中，没有新增依赖。

默认资源目录仍指向当前程序版本，显式环境或 env.d 的自定义值保持优先；资源目录作为子进程环境参与第二次插值，使预检与实际启动一致。管理台路径、地址、HTTPS 与开关使用解析后的环境值，布尔覆盖按上游 strconv.ParseBool 的有效值处理，非法覆盖回退到主配置。实际环境文件仍由 SFTPGo 自己加载，管理台入口继续只在健康启动后记录，修改待生效配置不改变当前运行入口。本地 Bolt/SQLite 的文件名称以及自定义逗号分隔主机密钥列表也使用实际环境值，文件丢失时阻止启动，不自动创建空库或更换身份。显式数据库连接串及外部数据库沿用原运行方式，不把它们当成本地文件。

19 项通用启动检查通过，新增覆盖文件顺序、同文件/跨文件重复赋值、继承空值、引号及插值、大小写冲突、BOM 编码、无父环境污染、环境指定的状态文件保护和错误信息不泄露值。验证位于已有 generic.rs 源码模块，没有新增测试文件。核对官方 2.7.5/2.7.6 portable ZIP 的 manifest SHA-256，并核对待执行文件与 ZIP 内主程序摘要一致。两项显式原生验收通过，分别保留既有 JSON 配置流程与增加 env.d 流程：实际登录管理 API、打开管理台、创建用户、SFTP 上传和读回文件、制造端口冲突回落、跨版本保留账户、文件及 SSH 指纹。env.d 额外验证 UTF-8/UTF-16、同文件最后赋值、后续文件不覆盖、运行描述端口和管理员优先、自定义数据库，以及将三个真实密钥移到配置目录内的子目录后继续使用。移开真实数据库或自定义密钥时启动被阻止，坏环境配置也不创建进程。最终两项原生检查耗时 8.36 秒；均在临时目录、SQLite 和回环端口运行，结束后确认无 fixture.exe/sftpgo.exe 残留，没有操作用户实际数据。

最终 Web 类型检查、workspace 全 targets Rust 编译、版本一致性和 diff 检查通过。未改 UI 布局、启动前端 dev 或执行前端 build；本次没有数据库或 schema 变更，未修改 update.sql。同步包/crate/Tauri 与界面版本回退至 0.2.54，Cargo.lock 仅更新本项目三个 crate 版本。必须新建 annotated tag v0.2.54，与 main 原子推送并核对远程分支、tag 和实际 Release 状态，不移动旧 tag，保留原有未跟踪文件。已确认 v0.2.53 Release completed/success。

原生验收范围仍为 Windows SFTPGo 2.7.5→2.7.6，macOS 和外部数据库尚未验收。多份旧配置目录的可视化选择、非回环网卡管理台入口、r-nacos 上游 panic，以及 MinIO/Consul 原生管理台仍待完善；不能将本轮环境配置修复等同于整体产品目标完成，整体目标保持进行。

## 第六十九轮：SFTPGo 多份配置目录可视化选择（v0.2.55）

补齐多份旧配置目录无法明确选择的问题。在套件页、总览卡片和总览列表增加 SFTPGo“配置目录”入口，列出现有非空目录的名称、相对路径、配置文件、实际本地数据库和 SSH 主机密钥文件名及更新时间。只读预检复用现有配置与 env.d 解析，不创建目录、写配置或读取账户内容，不展示连接串；坏目录作为带原因的禁选项，不影响其他正常目录。已有绑定正常显示，未绑定时不替用户默认选择另一份数据。浏览器预览明确提示需要桌面端，不伪造真实文件操作。

新增读取与保存 IPC，复用数据目录活动登记、生命周期锁、现有设置存储和托盘刷新。保存要求没有安装/卸载任务，SFTPGo 已停止且没有活 PID；重新检查程序版本、旧绑定、目标目录及状态文件，阻止过期选择覆盖其他操作。受管路径检查拒绝路径跳转、链接和目录联接。成功只保存现有 settings 中的 sftpgoConfigDir，用户随后自行启动，不复制、移动、合并或删除原目录；重复选择同一目录可幂等。普通启动仍沿用健康启动后保存首次绑定的流程。update.sql 已同步参数化写入说明，无 schema、依赖或 migration 变更，没有操作用户实际数据库。

前端复用现有 Button、Dialog、Skeleton 和 React Query，提供中英文、加载、空状态、读取失败与刷新、保存失败保留选择和重试。运行中可以查看但不能修改；保存中阻止重复提交和关闭，成功或取消后焦点回到入口。原生 radio 支持 Space 与方向键，弹窗内容独立滚动，底部操作在矮窗口仍可达。上下分隔线均为虚线且左右各留 8px，状态说明使用已有次要文本色。隔离浏览器验证三个入口、键盘与焦点、忙态、失败重试、运行中禁用、坏目录、空列表、中英文和暗色；320/390/1280 宽度及 390×600 窗口没有横向溢出，截图已目检，没有页面异常。验收后关闭本轮隔离上下文，保留用户原套件页；未启动前端 dev 或执行前端 build。

20 项通用启动检查通过，9 项原生专项默认忽略；新增覆盖只读预检无写入、多目录及状态缺失、非法路径、过期版本/绑定、活 PID 拒绝选择、选择后两份目录内容完整保留。验证位于已有 generic.rs 源码模块，没有新增测试文件。显式执行两项官方 SFTPGo 原生检查均通过，耗时 11.59 秒：先核对 2.7.5/2.7.6 portable ZIP 与 manifest 的 SHA-256，以及待执行程序与 ZIP 内主程序摘要；沿用登录、SFTP 上传读回、端口冲突和升级后的账户/文件/SSH 指纹保留检查。env.d 场景另建临时 archive 目录，通过实际 API 选择并启动、创建独有账户，运行中切换被拒绝；停止后切回原目录并升级到 2.7.6，原数据仍可用且查询 archive 独有账户返回 404，确认两个数据库没有混合，archive 文件仍完整保留。仅使用临时目录、SQLite 和回环端口，结束确认无 fixture.exe/sftpgo.exe 残留。

最终 pnpm --filter @nsb/web check、cargo check --workspace --all-targets --locked、版本一致性和 git diff --check 均通过。包、crate、Tauri 和界面回退版本统一至 0.2.55，Cargo.lock 只同步本项目三个 crate 版本。已确认 v0.2.54 Release completed/success；本轮发布必须新建 annotated tag v0.2.55，与 main 原子推送，再核对远程分支、peeled tag 和 Release workflow 实际状态，不移动旧 tag，保留原有未跟踪文件。

原生验收范围为 Windows SFTPGo 2.7.5→2.7.6，macOS 和外部数据库仍未验收。非回环管理台入口、r-nacos 上游启动 panic、MinIO/Consul 原生验收等仍待完善，整体产品目标保持进行。

## 第七十轮：Consul 持久化、完整端口组与真实管理台验收（v0.2.56）

继续参照 ServBay 的服务管理与卸载保留数据流程（https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management）。检查发现原 Consul 运行描述使用 agent -dev，官方明确此模式为内存服务、关闭持久化，重启丢失 KV；原清单还只设置 HTTP 端口，DNS、RPC、gossip 和 gRPC 仍使用上游默认端口。核对官方 CLI 文档（https://developer.hashicorp.com/consul/docs/agent/config/cli-flags）及实际 2.0.4 agent -help，将 Windows 清单中的 2.0.2–2.0.4 改为本机单节点 server，bootstrap-expect 1，固定节点名称，客户端及集群通信绑定回环地址，显式启用 UI 和原开发模式已提供的 service mesh，数据写入版本无关的 data/consul。

首次原生验收发现 Consul 2.0.3 默认 WAL 后端在 Windows 同步 raft/wal 目录时报 Access is denied，进程退出；没有放宽健康检查或改动目录权限来绕过。按官方 Raft 存储配置（https://developer.hashicorp.com/consul/docs/reference/agent/configuration-file/raft）显式使用 boltdb，完整验收随后通过。此次只更改原内置运行描述，旧安装快照、远端旧清单和版本模板在读取时做精确形状兼容，实际入口、下载来源、校验值及原快照文件保持不变；用户修改过 args、cwd、env、配置或超时的旧描述不被替换。既有内存模式已经丢失的数据无法恢复，本轮不声称将运行中的旧内存状态自动迁移到磁盘。

以 HTTP 主端口为基准同时分配 RPC -200、LAN -199、WAN -198、gRPC +2、gRPC TLS +3 和 DNS +100，复用现有端口组冲突、自动回落和持久化分配。DNS 与两种 gossip 额外检查 UDP，避免仅 TCP 可绑定却实际启动失败；非法端口或派生值越界明确拒绝。数据目录在创建前复用受管路径检查。内置编排启动后要求七个 TCP 端口都属于本次活进程，再确认 /v1/status/leader 返回本节点当前 RPC 地址并稳定存在，不能仅 HTTP 端口打开就显示运行成功；超时或进程退出沿用启动失败清理，不残留服务。自定义集群配置不应用单节点判定。

21 项通用启动与 16 项安装/卸载回归共 37 项通过。新增覆盖三个 UDP 端口冲突、附加 TCP 端口冲突、整体回落、预检不提前提交端口、越界输入和旧运行描述兼容/自定义保留；所有验证位于已有 Rust 源码模块，没有新增测试文件。显式原生 Consul 验收通过，耗时 63.95 秒：官方 2.0.3/2.0.4 ZIP 先核对清单 SHA-256 再解压执行；临时 SQLite、含空格的数据路径和回环端口下，旧安装快照自动采用持久化描述，制造 DNS UDP 冲突后启动，实际写入中文 KV、注册服务，访问管理台 HTML 及真实 JavaScript，并通过 UDP DNS 解析 consul.service.consul。依次重启、移动整组端口、升级 2.0.4、运行中卸载未使用旧版、卸载新版并重新安装，每阶段均确认 KV、服务注册和 NodeID 保留，旧安装文件未被兼容逻辑改写；结束确认无 fixture.exe/consul.exe 残留，没有操作用户真实数据或服务。

最终 pnpm --filter @nsb/web check、cargo check --workspace --all-targets --locked、版本一致性和 git diff --check 均通过。未改界面布局，未启动前端 dev 或执行前端 build。没有新增依赖或数据库变更，未修改 update.sql；Cargo.lock 只同步本项目三个 crate 版本。Windows 清单 revision 更新至 41，包、crate、Tauri 与界面回退版本统一至 0.2.56。已确认 v0.2.55 Release completed/success；本轮必须创建新 annotated tag v0.2.56，与 main 原子推送并核对远程指向及 Release 实际状态，保留旧 tag 和既有未跟踪文件。

本轮原生验收范围为 Windows Consul 2.0.3→2.0.4 的内置本机单节点，2.0.2、macOS 和自定义集群未作原生验收。MinIO 管理台配置及原生读写、非回环管理台入口、r-nacos 上游 panic 等仍需继续完善，整体产品目标保持进行。

## 第七十一轮：MinIO 管理台代理入口、启动检查与真实对象验收（v0.2.57）

沿用 ServBay 服务管理的快捷入口与保留数据流程，继续验收 MinIO。原逻辑只检查 S3 主端口，管理台固定为本机根路径，不能正确处理 MINIO_BROWSER、代理子路径及控制台延迟就绪。核对官方 RELEASE.2025-09-07T16-13-09Z 的 cmd/common-main.go、server-main.go、config-dir.go、internal/config/bool-flag.go，以及 minio/pkg v3.1.3 env.Get；配置说明为 https://github.com/minio/minio/blob/RELEASE.2025-09-07T16-13-09Z/docs/config/README.md。按上游规则读取继承环境、展开后的 run.env 与 MINIO_CONFIG_ENV_FILE，文件内最后一次赋值覆盖前值，不执行 shell 插值；支持 export、字面量单双引号、空行与整行注释，布尔值和空值回退遵循上游语义。Windows 环境名称大小写冲突明确拒绝。坏赋值、NUL、非法编码或超长文本在启动前报错，不回显配置值、不修改原文件或父进程环境；环境文件不存在时沿用上游忽略行为。

首次真实代理子路径检查发现：直接访问 MinIO 本机端口下的子路径会返回 SPA HTML，却把 JavaScript 请求也返回成 HTML，页面实际不可用。上游通过 MINIO_BROWSER_REDIRECT_URL 改变页面 base，仍要求外部代理剥离前缀。因此入口现在保存本次启动的浏览器目标和本机探测地址：先确认本机控制台端口属于当前活进程并返回网页，再将经过校验的完整代理 URL 交给已有浏览器打开流程。后端不请求代理地址、不跟随远端跳转、不携带凭据；拒绝非 HTTP/HTTPS、内嵌凭据、查询和片段配置。没有代理设置时保留本机直接访问，并通过实际探测识别 HTTP/HTTPS；只设置 CONSOLE_SUBPATH 却没有完整代理地址时明确提示所缺配置。服务停止或重新启动清除旧入口，运行中编辑待生效配置不改变当前记录，其他服务的本机入口约束保持不变。

内置本地单盘运行描述允许已有证书目录和日志参数。开启管理台时检查 S3 与控制台两个端口的进程归属，再请求本机 /minio/health/ready 并等待稳定；关闭管理台时只检查 S3，不因未使用的控制台端口冲突而回落或阻止启动。自签证书只在受管回环探测时允许，浏览器仍正常处理证书信任。对象目录在初始化前复用受管路径检查；MINIO_CONFIG 的 YAML 若覆盖托管监听端口，会明确报出配置冲突并保留原文件，不能只改健康检查端口。没有改变凭据设置、对象目录、下载来源或既有安装描述，不引入依赖。

23 项通用启动与 11 项服务状态回归共 34 项通过，新增覆盖环境文件优先级、父环境不污染、关闭控制台不占端口、错误不泄露值、配置原样保留、代理目标仅经本机探测、凭据与非回环探测地址拒绝、停机清除入口。验证位于已有 generic.rs 源码模块，没有新增测试文件。两项显式原生验收最终通过，耗时 59.04 秒：使用官方 MinIO 2025-07-23 与 2025-09-07 Windows 程序，下载后核对清单 SHA-256；真实代理使用官方 Caddy 2.11.4，核对 ZIP 摘要和 ZIP 内程序与待执行程序一致。所有数据、SQLite、证书和代理配置均在临时目录，未修改用户实际服务、配置或系统信任库。

HTTP 与自签证书 HTTPS 两条路径均完成真实 AWS Signature V4 请求、创建 bucket、写入和下载中文对象内容，分别验证重启、升级新版、运行中卸载未使用旧版、卸载新版再重装后的数据保留。Caddy 实际剥离 /niceenv/ 前缀，转发管理台 HTML、真实 JavaScript 与登录 API；HTTPS 场景同时验证代理与 MinIO 两段 TLS。另验证运行中编辑环境文件不会改写入口，关闭控制台后即使其端口被占用仍可使用 S3，移除代理后可直接打开本机根入口并登录，坏开关配置不创建进程。最终核对无 fixture.exe/minio.exe/caddy.exe 临时进程残留。没有仅凭 HTML 状态码宣称代理管理台可用。

最终 pnpm --filter @nsb/web check、cargo check --workspace --all-targets --locked、版本一致性和 git diff --check 均通过。未改界面布局、启动前端 dev 或执行前端 build；本次没有数据库变更，未修改 update.sql。包、crate、Tauri 与界面回退版本同步至 0.2.57，Cargo.lock 只更新本项目三个 crate 版本。已确认 v0.2.56 Release completed/success；本轮必须新增 annotated tag v0.2.57，与 main 原子推送，再核对远程分支、tag 和实际 Release 状态，保留旧 tag 及原有未跟踪文件。

原生验收范围为 Windows MinIO 2025-07-23→2025-09-07 的本地单盘模式与 Caddy 代理，macOS、分布式布局、外部身份提供商和其他历史版本未验收。此功能只在用户点击管理台时打开其明确配置的代理地址，代理的运行状态和外部网络可达性仍由浏览器体现。其他服务的非回环管理台入口、r-nacos 上游 panic 等继续完善，整体产品目标保持进行。

## 第七十二轮：r-nacos Raft 就绪检查与初始化故障诊断（v0.2.58）

继续追查官方 Windows r-nacos 0.8.6 / 0.8.7 的启动 panic。重新核对两个 ZIP 的 SHA-256 与清单一致，并确认待执行程序与 ZIP 内 rnacos.exe 摘要相同。上游最新 Release 仍为 v0.8.7；核对对应版本 starter.rs、raft/filestore/raftapply.rs、naming/instance_meta_manager.rs、health/core.rs、raft/network/management.rs 与 openapi/middle/auth_middle.rs。源码表明 Raft 在 BeanFactory 注入之前启动，实例元数据持久化初始化的异步 I/O 可能让 apply 请求先到达未注入的 StateApplyManager。真实运行捕获到 raftapply.rs:421:52 的 Option::unwrap panic，也观察到同样配置偶尔正常启动，不能把这个竞态当作每次必现的版本行为。

更关键的是，失败启动遗留的数据在重启后可能没有 leader，但上游 /health 初始化时会给 RaftCluster 12.5 秒宽限，期间返回 success，写配置却持续返回 500。generic.rs 现在同时核对三个监听端口的进程归属、本轮 panic、/health 及只读 /nacos/v1/raft/metrics。Raft 必须有有效节点 ID、leader、已应用日志，以及相符的 Leader/Follower 角色；没有 leader、尚未应用日志、候选状态、畸形响应和请求错误均不能提前判定就绪。状态接口返回 401/403/404 时，不自动登录或关闭鉴权，改为等首次健康响应之后至少 13 秒再核对 /health，并保持半秒稳定窗口。开启鉴权的正常启动因此通常至少需要约 14 秒，默认 20 秒超时仍适用。进程退出会结束检查，超时会停止服务并明确提示 Raft 未就绪。

对 Windows 0.8.6 / 0.8.7 本轮日志中 raftapply.rs 第 421–424 行的依赖读取 panic，提供精确的初始化失败说明；其他版本、其他 panic 位置和历史启动日志不会套用该诊断。经原生验证，在全新或正常数据目录中显式设置 RNACOS_NAMING_INSTANCE_METADATA_PERSISTENCE_ENABLE=false 可规避已观察到的初始化路径，配置中心写入、重启后读回仍有效。但此开关会停止加载和保存注册实例元数据，也不能修复失败启动遗留的异常 Raft 数据，所以只作为用户明确选择的临时方案提示，不修改默认模板、不自动关闭该功能、不删除数据、不替换官方程序。README 同步说明限制、环境变量优先级、鉴权检查耗时及已有异常数据需要备份恢复的边界。

现有 generic.rs 源码验收模块覆盖两个官方版本的实际写入读回、重启保留、HTTP/gRPC/控制台端口回落、管理台 HTML、启用 Open API 鉴权后匿名访问被拒、真实账号登录及携带 token 读回。失败场景保留原 .env 与数据目录，再按明确配置重试：能恢复时必须真实读写且重启保留，仍无 leader 时必须报错、释放端口并保留数据。针对竞态分别接受经过真实读写证明的成功和经过日志、状态、端口核对的失败，不再要求每次必现 panic。额外使用鉴权开启、RAFT_AUTO_INIT=false 的官方进程确认 /health 初始宽限不能提前放行。两项原生验收通过，最终补充的鉴权与重复启动故障验收通过，耗时 82.49 秒；此前完整两项运行耗时 104.69 秒。全部使用临时目录和回环地址，无测试文件新增；检查后未发现 fixture.exe / rnacos.exe 残留。

通用启动回归 23 项通过，12 项需要独立官方程序的原生检查默认忽略；本轮相关的两项 r-nacos 检查已按上述方式显式执行。pnpm --filter @nsb/web check 与 cargo check --workspace --all-targets --locked 均通过。包、crate、Tauri 和界面版本回退值同步至 0.2.58，Cargo.lock 只修改本项目三个 crate 版本，无依赖变更。未启动本地前端 dev 或执行前端 build，本次没有数据库变更，未修改 update.sql。发布遵循根 AGENTS.md：必须新增 annotated tag v0.2.58，与 main 原子推送并核对远程提交和实际构建状态；v0.2.57 Release 已确认 completed/success。

本轮解决 NiceEnv 的 r-nacos 启动误报和诊断缺失，上游初始化竞态本身仍未修复。没有验收 macOS r-nacos、其他历史版本或真实多节点集群，不能将临时关闭实例元数据持久化表述为完整恢复该功能。其他服务管理台、自定义配置和 UI 仍需继续完善，整体产品目标保持进行。

## 第七十三轮：IPv6 监听扫描、指定网卡管理台与真实服务验收（v0.2.59）

继续完善服务管理台快捷入口，确认两个问题：通用启动就绪检查只连接 127.0.0.1，管理台 URL 也拒绝指定本机网卡 IP。使用真实 ::1 临时监听进一步发现 Windows 的 netstat -ano -p tcp 不列出 TCPv6，而 netstat -ano 能看到同一端口和 PID；这不仅影响启动和管理台，还会让端口工具漏报 IPv6 占用。ports.rs 现在统一读取 netstat -ano，仅解析 TCP LISTENING 行；保留监听 SocketAddr、端口和 PID，并继续通过原 (port, pid) 接口供端口工具使用，保持既有去重和结束进程保护流程。macOS lsof 路径保留原列解析，并区分 IPv4 / IPv6 通配地址；无法识别地址时仍保留端口和 PID，不把未知监听报告为空闲。

generic.rs 的端口分配先校验整组端口范围，再同时核对 IPv4 / IPv6 监听占用和原有绑定预检；关闭自动回落时准确返回冲突端口、PID 和进程名，启用时整组回落。就绪检查先验证监听 PID 属于当前活进程，再连接系统报告的实际地址；通配监听转换成同地址族的回环地址。管理台允许配置中的明确 IP，打开前后均核对本次进程、端口及实际监听地址；非回环地址必须精确匹配系统监听，不能借用本机同端口去探测远端 IP。仍不解析任意主机名、不发送认证信息、不跟随重定向；MinIO 已配置代理入口继续只探测其本机端点。监听配置、服务端口模板、数据库位置、原配置目录、运行中入口快照与界面布局没有改变。

33 项回归通过：通用启动 23 项、端口扫描及进程保护 10 项。覆盖 IPv4 / IPv6 / 通配监听、数值区域标识保留、未知地址仍保留占用信息、真实 IPv6 端口扫描、IPv6 次要端口冲突的整组回落及禁止回落、错误地址拒绝、无凭据 GET、不跟随外部跳转、停止后清除入口，以及只能结束已选临时监听进程的保护。所有验证扩展已有 Rust 源文件，没有新增测试文件。

三项既有原生验收覆盖四个运行场景：官方 SFTPGo 2.7.5→2.7.6 的 IPv4 JSON 配置与 IPv6 env.d 配置，两项通过，耗时 10.34 秒；相同 env.d 验收另在本机 Default Switch 虚拟网卡的明确 IPv4 地址上通过，耗时 5.12 秒。各场景实际登录管理 API、访问自定义路径管理台、建立 SFTP 连接、核对主机指纹、写入并读回文件，制造相应监听地址上的端口冲突，验证重启、切换配置目录、跨版本后账户、文件、数据库及密钥保留。仅使用临时目录、临时账户和随机密码，未修改用户服务或防火墙。Qdrant 1.19.1 全程绑定 ::1，制造 IPv6 gRPC 冲突，实际创建集合、写入并读回向量，修复管理台并下载真实脚本，卸载重装后向量与自定义配置保留；原生验收通过，耗时 12.20 秒。执行前核对 SFTPGo 两版及 Qdrant ZIP 的清单 SHA-256，并核对实际程序与归档内程序一致，Qdrant UI 包由既有摘要断言校验。检查后未发现 fixture.exe / sftpgo.exe / qdrant.exe 残留。

验证中遇到 D 盘空间耗尽导致一次 Rust 编译中断；按 Cargo 包范围预览后，使用 cargo clean -p nsb-core --profile dev --target-dir 指向本项目 target 清理可重新生成的构建产物，恢复空间后重新编译并完成验收。未清理源码、用户配置、原有未跟踪文件或官方验证资产。前端 pnpm --filter @nsb/web check 与 cargo check --workspace --all-targets --locked 均通过，版本同步至 0.2.59，Cargo.lock 仅更新三个本项目 crate 版本，无依赖和数据库变更，未修改 update.sql；未启动本地前端 dev 或执行前端 build。v0.2.58 Release 已确认 completed/success。本轮继续新增 annotated tag v0.2.59，与 main 原子推送，并核对远程提交和实际 Release 状态。

本轮原生验收范围为 Windows SFTPGo 2.7.5 / 2.7.6 和 Qdrant 1.19.1。macOS、其他历史版本、监听主机名和带区域标识的 IPv6 管理台 URL 尚未原生覆盖；r-nacos 自定义 HTTP/控制台地址仍需单独完善，上游 Raft 竞态也仍存在。UI 间距和其他缺漏功能继续按真实问题推进，整体产品目标保持进行。

## 第七十四轮：r-nacos 自定义 SDK/控制台监听与入口一致性（v0.2.60）

继续完善服务管理的真实可用性，确认 r-nacos 仍有上一轮未覆盖的问题：端口归属检测已识别 IPv6，但 /health、Raft metrics 和控制台入口仍写死 127.0.0.1。通过 Context7 查询官方配置说明，再核对 v0.8.6 / v0.8.7 的 src/common/mod.rs 与启动实现：SDK 与控制台地址可独立配置，控制台默认继承 SDK，.env 插值及进程环境覆盖由上游负责；IPv6 使用 [::1] 这样的带方括号格式。

generic.rs 现在从当前活进程实际建立的系统监听中分别获取 SDK 与控制台地址，仅处理原有受管运行描述和已分配端口。保留三个端口的进程归属检查，同一端口存在其他所有者时拒绝生成入口；优先回环地址，多地址时稳定选择 IPv4，通配地址转换为同地址族回环。健康检查请求真实 SDK 地址，仍核对 Raft leader、角色、已应用日志、鉴权场景的初始宽限期及本轮 panic；返回成功前再次检查端口归属和探测入口。控制台地址只在本次健康启动后记录，运行中编辑待生效配置不改变入口，重启后再更新。NiceEnv 不额外解析任意主机名、不携带鉴权信息、不跟随健康请求跳转；控制台主机名由上游完成解析，入口使用系统确认的本机 IP。没有重写监听设置、默认模板、鉴权开关或数据路径。

同时修正 ports.rs 通配地址匹配遗漏的地址族条件：IPv4 通配监听不能证明 IPv6 回环归属，IPv6 通配监听也不能直接证明 IPv4 回环归属。未知监听仍保留端口与 PID，带区域标识的 IPv6 HTTP 地址不丢弃 scope 后冒充普通地址，而是暂不生成入口。README 更新配置格式、生效时机和验证边界。

33 项现有模块回归通过：通用启动 23 项、端口扫描和进程保护 10 项。扩展原有 Rust 源码内验收，覆盖失效 PID、SDK 与控制台不同地址族、缺失监听拒绝、未启动时不猜测入口、跨地址族通配匹配拒绝。未新增测试文件或依赖。两个官方版本的 ZIP SHA-256 与清单一致，并核对实际执行程序与 ZIP 内 rnacos.exe 的 SHA-256 一致。

显式原生验收先在 ::1 上完成两个版本的读写、重启、端口回落和鉴权检查，耗时 39.30 秒；再以本机 Default Switch 虚拟网卡 IPv4 作为 SDK 地址、localhost 作为控制台地址，复核两个版本，并执行既有上游故障验收，两项共耗时 121.82 秒。实际验证 .env 变量插值、SDK/gRPC/控制台端口成组回落、配置中心真实写入读回、重启数据保留、管理台 HTML 与真实 JavaScript、停机清除入口、运行中编辑配置保持当前入口、运行描述环境覆盖 .env 后控制台地址随重启更新。开启 Open API 鉴权后匿名请求仍被拒绝，真实登录取得 token 后可读回原数据；配置为无法绑定的控制台地址时停止整个临时服务。故障验收确认无 leader 的健康宽限期不会提前放行，上游竞态成功须真实读写、失败须清理进程并保留配置和数据。全部使用临时配置、数据库与随机凭据，未操作用户实际服务；结束后未发现 fixture.exe/rnacos.exe 残留。

包、crate、Tauri 和界面回退版本统一至 0.2.60，Cargo.lock 只同步三个本项目 crate 版本。pnpm --filter @nsb/web check 与 cargo check --workspace --all-targets --locked 均通过。未启动本地前端 dev 或执行前端 build，本次没有数据库变更，未修改 update.sql。发布必须新增 annotated tag v0.2.60，与 main 原子推送并核对远程指向和实际 Release 状态；保留旧 tag 和原有未跟踪文件。

本轮原生范围为 Windows r-nacos 0.8.6 / 0.8.7 的 IPv6 回环、指定本机 IPv4、localhost 控制台及运行描述覆盖；macOS、其他版本、真实多节点集群和带 scope 的 IPv6 HTTP 入口尚未验收。官方 Raft 初始化竞态仍存在，显式原生正常流程沿用此前说明的临时元数据持久化开关，不代表已修复上游。UI 间距、其他服务和缺漏功能继续推进，整体产品目标保持进行。

## 第七十五轮：套件版本下拉间距、滚动与安装焦点交接（v0.2.61）

重新从套件页实际交互检查 UI，参考 ServBay 官方服务/套件管理页 https://support.servbay.com/basic-usage/services-and-packages/service-and-package-management 的版本状态分组与明确操作入口，并按 ui-ux-coding 检查现有组件和完成清单。通过隔离浏览器上下文复现两处问题：从版本列表打开安装弹窗时两个浮层同时保持打开，转入后台后键盘焦点最终落到 BODY；320×320 的窗口中，固定 PATH/默认版本说明将版本列表压缩到约 52px，搜索无结果时正文也需滚动。已有上下分隔线的虚线及边距正确，不重复改动全局菜单组件。

VersionPicker 现在在打开安装流程前关闭版本浮层，并沿用原卸载流程的焦点交接方式，将稳定的套件版本按钮交给页面。InstallDialog 透传 Radix onCloseAutoFocus，完成、取消、关闭或后台安装后恢复该按钮的焦点；安装任务仍使用原全局任务和 IPC，未修改下载、校验或取消语义。根据 Context7 的 Radix Popover/ Dialog 文档处理自动聚焦，不新增组件库或全局事件。原卸载确认回退行为保持并经过浏览器复核。

搜索栏和来源栏保留，离线提示、版本分组、错误及 PATH 说明改为同一滚动区，避免长说明和失败消息挤掉全部操作区域；错误出现时自动滚动到提示及重试按钮，保留搜索和原操作。增加清除搜索按钮、键盘激活后回到搜索框、匹配/总数反馈，以及无版本和无匹配的不同文案。弹层与分组提供可访问名称；已安装、可安装和预发布组之间统一增加左右各 12px 的虚线。沿用既有主题 token，提高操作文字、版本说明和分组标签的可读性，不改变其他页面布局或现有已安装版本合并规则。新增文案同步中英字典。

浏览器预览交互验收覆盖 1280×800、390×844、390×480 与 320×320，检查实际边界和横向溢出，并目检浅色/深色及中文/英文截图。两端与组间分隔线计算样式均为 dashed、左右各 12px；极小窗口示例的版本滚动区从约 52px 增至约 90px。验证清空搜索、无匹配数量、长 MinIO 版本名、Escape、键盘 Enter 清空及搜索聚焦。沿用浏览器内置模拟后端，验证安装完成和转后台时仅一个打开的弹层、焦点回到原版本按钮、安装后条目进入已安装组、取消安装仍留在可安装组；取消卸载也恢复原入口。使用运行中 Nginx 切换版本的既有拒绝流程，确认错误与重试在小窗口中可见、查询保留。最后一组交互捕获的页面异常为 0。首次自动化遇到定位名称变化、热更新关闭浮层和关闭动画尚未结束的等待错误，均按实际可访问名称及终态重新检查，没有把工具等待问题当作产品缺陷。

pnpm --filter @nsb/web check 通过；cargo metadata --locked --no-deps 确认三个工作区 crate 和锁文件统一至 0.2.61，其他包/Tauri/界面回退版本同步。此轮没有 Rust 业务修改，未运行后端编译或原生服务安装验收；浏览器数据模拟仅用于交互验证，不能代替实际下载或服务运行证明。未新增测试文件、依赖或调试入口，未启动前端 dev 或执行本地前端 build。本次没有数据库变更，未修改 update.sql。工作区出现的其他 configgen.rs 修改保持原样，不纳入本轮提交。

已确认 v0.2.60 Release completed/success。发布继续遵循根 AGENTS.md：新增 annotated tag v0.2.61，与 main 原子推送，核对远程提交与实际 Release 状态。只提交本轮 UI、文档和版本同步文件，保留原有未跟踪内容。桌面 Tauri WebView 的原生焦点表现尚未单独验收；整体功能完善、其他 UI 问题以及此前记录的服务边界继续处理，整体目标保持进行。

## 第七十六轮：后台安装任务、历史详情与自动选版进度（v0.2.62）

沿用既有全局安装任务、下载事件和套件安装入口补齐任务管理。原安装弹窗关闭后没有重新查看详情的入口，失败信息主要依赖短暂通知；直接复用原弹窗会在打开时重新发起安装。套件页现在增加本次会话的任务列表，按进行中、失败、取消、完成排序，显示版本、阶段、下载量、速度与错误摘要。任务列表不受套件搜索和分类筛选影响，支持折叠、取消、查看详情、移除单条已结束记录和清除已结束记录。清除只影响会话记录，不删除安装文件；最后一条移除后保留空态和键盘焦点。

InstallDialog 区分新安装与查看已有任务，后者不会调用安装接口，失败和取消后的重试仍须明确操作。自动选版请求使用包 id，后端事件可能使用 id@实际版本；任务记录关联已确认的进度键和版本，优先匹配正在运行的精确版本任务，防止同包其他版本的事件覆盖已关联版本。取消使用已确认的进度键，结束或重试仅清理自己的进度，并保留其他运行任务仍在使用的键。安装是否成功以原 IPC Promise 结果为准，收到 installed 事件后仍等待后端完成注册。历史成功记录的启动入口只在对应版本当前仍安装时提供，未知版本不生成 PATH 快捷操作。

浏览器既有模拟后端补齐与桌面端相同的 id / id@version 解析、安装阶段事件和唯一前缀取消规则；模拟阶段不下载或写入本机文件。没有修改原生 IPC 合同、安装器或数据库。任务列表和新增提示使用中英字典、既有 Card/Button/Dialog 与主题 token，分隔线为虚线且左右各留 12px。目检发现英文窄屏下清除按钮挤压标题，改为窄屏独占标题行、状态可换行、清除按钮另起一行；宽屏保持横向排列。

通过无新文件的内联 Node 校验验证真实 store 的去重、运行记录不可移除、自动选版关联、不同版本并行、Promise 终态、精确取消、错误/提示保留、显式重试、结束清理保护，以及已结束精确版本记录不阻挡新自动选版。隔离浏览器上下文中验收后台后重新打开、路由离开再返回、运行/完成/失败/取消详情查看、显式重试、自动选版实际版本显示、清除空态、键盘折叠和焦点返回。仅在该隔离上下文的脚本响应中临时记录模拟安装调用次数、注入长失败信息与延长模拟阶段，未写入产品代码或用户页面；打开详情前后安装次数不变，点击重试才新增一次调用。按 320×480、390×844、1280×800 检查浅色/深色、中英、长错误换行和横向边界；搜索不存在套件后任务仍可见，分隔线计算样式为 dashed、两侧 12px，清空任务后焦点保留。最终浏览器交互未捕获页面异常；前期自动化的按钮名称精确匹配等待失败按实际可访问名称修正后复核。

pnpm --filter @nsb/web check 通过，cargo metadata --locked --no-deps 确认三个工作区 crate 统一为 0.2.62；Cargo.lock 仅同步本项目三个 crate，包、Tauri 和界面版本同步。未新增依赖或测试文件，未启动前端 dev 或执行前端 build。本轮没有 Rust 业务修改，未运行后端编译或原生下载/安装验收，浏览器模拟不能代替原生验证。本次没有数据库变更，未修改 update.sql。用户已有 configgen.rs 修改与原有未跟踪文件保留，不纳入发布。

已确认 v0.2.61 Release completed/success。v0.2.62 发布须新建 annotated tag，与 main 原子推送并核对远程分支、tag 与实际 Release 状态。任务仅在当前前端会话保留，跨重启任务恢复、桌面 WebView 原生交互与此前记录的服务边界仍待继续完善，整体目标保持进行。

## 第七十七轮：首次启动引导真实结果、恢复操作与场景交接（v0.2.63）

继续检查从首次启动到安装再到使用的流程，并参考 ServBay 安装指南 https://support.servbay.com/getting-started/installation 中按场景准备环境、后续管理套件的设计。现有引导仅把任务 error 字符串算作失败，取消后没有 error 就会提示整套准备完成；失败结果也仍使用绿色完成标题，数据库场景最终一律创建网站。前端场景实际只安装 Nginx，Node.js 缺失。首启判断还把读取套件失败当成空列表，并在检测到任一已安装套件时静默写入完成标记。此次沿用 React Query、全局安装任务、现有 IPC 和组件库修复，不新增后端接口或依赖。

引导现在等待设置和真实套件列表成功读取后才决定是否显示；已有安装时仍保持不打扰，但不再静默修改完成标记。选择场景时显示实际套件清单，并检查当前平台是否存在可用条目。前端场景包含 Nginx 与 Node.js；安装仍由后端按包 id 自动选择版本，去掉前端对 PHP 无效的字符串排序。每次发起前重新读取安装状态，复用已有套件；本次结果区区分等待、运行、完成、失败与已取消，展示后端确认版本和完整错误。只有全部成功才提示套件准备完成，取消不算成功，双击开始由同步运行锁去重。重试仅处理未完成项；读取失败保留待执行列表并停止继续发起，后台也有通知和返回本次详情的入口。

后台继续安装保留原队列并进入套件页，由上一轮任务面板查看实际任务。成功后的数据库场景进入数据库页；前端场景通过现有 UI store 传递站点类型，打开静态站点向导，PHP 场景仍默认 PHP。SiteWizard 仅增加可选 initialKind，普通新建入口每次仍回到 PHP，关闭后不泄漏上一次的场景类型；切换发生在引导关闭的焦点交接时。这里的完成表示套件已经安装，不冒充服务已启动。设置写入失败保留窗口和结果，显示可重试错误，并允许用户明确选择本次继续但不保存完成标记，避免磁盘只读时被困在引导中。

使用既有 Dialog、Button、RingProgress 与主题 token。小窗口场景卡改为纵向排列，选中项具有 aria-pressed 和键盘焦点；固定标题和底部操作之间的内容独立滚动。首次目检发现英文长说明压缩 320×320 的结果区，随后将说明和错误纳入正文滚动区，步骤切换和保存失败时回到正文顶部，最终结果区约 117px。长保存错误也不会挤掉底部按钮。上下分隔线均为虚线，窄屏左右各 16px、宽屏 24px，与正文边距一致；任务条目沿用带左右留白的虚线。文案同步中英，不再给取消或未完成项目绿色成功反馈。

在仅本轮新建的隔离浏览器上下文中，使用既有模拟后端，通过隔离脚本响应设置首次启动状态、模拟下载失败/读取失败/只读设置、计数安装调用和调整阶段延迟，未修改产品代码中的数据或用户原页面。PHP 场景首次四次调用中取消 Nginx、让 PHP 失败，最终显示 2/4 完成且没有成功通知；重试只增加 nginx、php 两次调用，MySQL 和 Redis 未重装。前端场景实际调用 nginx、node，站点向导第三步确认为静态，随后从普通新建入口再次进入则默认为 PHP。数据库场景调用 mysql、redis 后进入 /databases。预先完成 Nginx 后再开始前端场景只新增 Node.js 调用；转后台导航后任务继续并全部完成。读取失败时没有安装请求，恢复后可重试；初次读取失败时没有误弹引导或写设置，刷新恢复后正常显示。移除 Node.js 清单条目的隔离场景会提示缺少套件、禁用安装，并可进入套件页。已安装环境没有误弹引导或产生安装/设置写入。

保存失败验收覆盖保留单一弹窗、重试成功后的站点交接，以及用户选择不保存后进入套件/数据库页。尺寸覆盖 1280×800、390×844、320×480 与 320×320，检查浅色/深色、中英文、错误长链接、按钮边界和实际滚动区；窄屏最终对话框位于 x=12、宽 296px，无横向溢出。最终一组交互的页面异常为 0。早期截图在视口调整动画期间曾捕获旧尺寸，等待布局稳定后重验；一次宽泛 PHP 名称匹配同时命中 FrankenPHP，收紧选择器后完成验证。浏览器仅证明前端交互与调用语义，本轮未执行原生首次安装、创建真实站点或启动用户服务。

pnpm --filter @nsb/web check 与 git diff --check 通过。包、crate、Tauri 和界面回退版本统一至 0.2.63，Cargo.lock 仅同步本项目三个 crate，并用 cargo metadata --locked --no-deps 核对。未启动前端 dev、未执行前端 build，未新增测试文件、依赖或数据库变更，未修改 update.sql。本轮没有 Rust 业务变更，未执行后端编译；用户已有 configgen.rs 修改及原有未跟踪文件保留，不纳入发布。

已确认 v0.2.62 Release completed/success。本轮继续创建新 annotated tag v0.2.63，与 main 原子推送并核对远程提交和实际 Release 状态，不移动旧 tag。跨重启恢复安装队列、其他服务和此前列出的原生平台边界仍需继续完善，整体目标保持进行。


## 第七十八轮：站点实际访问地址、监听归属与批量结果（v0.2.64）

原站点入口由前端当前端口设置拼接，服务运行期间修改端口方案会提前改变链接；主配置的监听端口与旧 vhost 也可能不同。现在在 Web 服务启动前读取托管主配置与站点配置，解析域名、HTTP/HTTPS 和实际端口，成功启动后记录内存快照。主配置须直接包含托管站点目录，加载前后文件须保持一致；未能可靠解析的条件、变量或仅外部网卡监听不生成猜测地址。Site 增加只读派生 accessUrl，不持久化、不接受客户端反序列化写入。服务停止、退出或跨会话接管时清除旧记录。

新 site_access_url IPC 在打开、复制前确认站点依赖运行状态、入口快照与本机监听者归属。复用现有进程树规则识别 Nginx worker / Apache 子进程，拒绝外部或未知归属。运行中修改设置或磁盘配置保留旧快照；异步重载仅保留新旧一致的入口，不能把信号发送成功当作新地址已生效。Windows 沿用同步重启路径，类 Unix 在站点入口新增或变化时同步重启，入口不变才使用热重载。跨会话接管进程无法证明已加载配置，需要重启 Web 服务才能恢复快捷入口。

总览、站点卡片、详情、创建结果和命令面板统一消费后端入口；打开和复制使用实时校验。未知地址显示启动后确认提示，批量复制只写入成功解析的地址，批量打开分别统计成功与失败，并列出失败站点名；同步操作锁防止重复批量动作。CopyButton 支持异步取值并保留原静态文本用法。浏览器打开先保留空白窗口，在地址确认后导航，确认失败关闭窗口并保留原错误。浏览器模拟后端同样保留本次加载的地址，创建和启动站点通过已有服务动作准备依赖。

在已有 Rust 测试模块补充 HTTP/HTTPS、标准端口省略、通配域名、Apache 大小写和空白、条件配置拒绝、主配置 include、加载期间文件变化、异步重载不提前发布、派生字段不可反序列化等验证，未新建测试文件。隔离真实 Nginx 使用随机端口、临时配置与静态文件，验证首页、导出路由、资源与 404，确认设置和 vhost 修改后旧端口仍服务且入口不变，外部监听不被认领，停止后拒绝入口，重启后地址和真实请求切到新端口。测试进程均由既有 RAII 清理。本轮没有 Apache 原生可执行文件，Apache 仅验证解析逻辑和编译。

隔离浏览器上下文中使用既有模拟后端，临时脚本响应注入仅用于记录 site_access_url 调用、模拟失败与收集打开/复制结果，未写入产品调试代码。修改端口设置后批量操作仍使用原运行地址；单站点击会读取最新后端地址；停用站点不计入成功或剪贴板；地址读取失败、剪贴板拒绝均有错误反馈。中英文以及 1280、390、320 宽度检查无页面异常或横向溢出，长地址保持截断且可通过复制取得完整值。浏览器模拟不能替代桌面 WebView 与原生服务验收。

工作区 pnpm --filter @nsb/web check、cargo check --workspace --all-targets --locked 和定向入口测试、真实 Nginx 验证通过。扩大 Rust 回归时 609 通过、12 ignored、1 条既有 MinIO 测试因并行临时端口冲突失败，该条独立复核通过。精确暂存树导出到独立临时目录后，cargo check --workspace --all-targets --locked 通过；同一回归子集串行运行 610 通过、0 失败、12 ignored、27 filtered out，真实 Nginx 验证再次通过。此验证不依赖工作区保留的 configgen.rs 其他修改。版本与 Cargo.lock 中本项目三个 crate 统一为 0.2.64，cargo metadata --locked --no-deps 核对通过。未启动前端 dev、未执行本地前端 build，未新增依赖。本次没有数据库变更，未修改 update.sql。

configgen.rs 原有未提交修改保留，本轮仅纳入复用解析器所需的四处 pub(crate) 可见性调整；其他本地生成文件不提交。已确认 v0.2.63 Release completed/success。本轮必须创建新 annotated tag v0.2.64，与 main 原子推送，并核对远程提交和实际构建状态。其他功能完善与跨平台原生边界继续处理，整体目标保持进行。


## 第七十九轮：站点停止回滚、服务影响范围与批量结果（v0.2.65）

参考 ServBay 官方站点管理面板 https://support.servbay.com/basic-usage/websites/website-management-panel 中按站点提供启停、重载和日志操作的流程，继续验证 NiceEnv 的真实启停链路。发现批量停止在重载前已重命名 vhost 并保存记录，重载失败只向前端抛出总错误，配置未恢复；单站停止虽恢复文件，却未让已经应用变更的服务重新加载。单个 Nginx 站点启停还会无条件重建 Apache 配置，受不相关服务故障影响。此次复用既有 ServiceManager、配置快照、批量响应和重载接口，不新增数据库字段或依赖。

单站与批量停止统一走同一条事务式编排：先去重、读取记录并检查配置路径，无法读取或无效标识逐项报告；已停用项不再重复执行。需要变更的站点先保留文件和记录快照，再禁用配置、更新记录，实际涉及的 Nginx / Apache 各应用一次。任何变更或重载失败，会恢复本批文件及已保存记录；已成功应用变更的服务重新加载原站点，停止过的运行实例尝试重新启动。恢复未完成返回明确的 SITE_STOP_ROLLBACK_FAILED 和细节，不能报告成功。旧版本在另一 Web 服务下遗留的启用配置也纳入影响范围，重命名失败不能被忽略。未进入重载阶段不多余重启，Windows 校验失败且原 PID/运行态保持不变时保留原运行实例。

站点启动与停止全过程持有既有生命周期锁，保持与版本切换、服务启停的互斥；启动只处理该站点及其遗留配置实际涉及的 Web 服务。配置快照沿用受控路径校验，拒绝无效 ID、软链接/目录联接，防止损坏记录导致越界操作。保存和删除站点的其他编排仍沿用原流程，本轮没有将其改造成同一套停止事务。

批量弹窗保留逐站结果，展示站点名、成功/已处于目标状态/失败、错误建议和可展开详情。失败项保持选中，并有仅重试失败项按钮；顶层请求失败保留选择，所有结果均刷新站点和服务状态。同步操作锁防重复，运行期间禁用选择和关闭，结束后焦点进入结果区。站点名和域名分两行，窄屏底部操作分行，正文独立滚动；上下分隔线为虚线，两边窄屏 16px、宽屏 20px 留白。站点详情的启动/重载使用已有 start_site 链路，准备站点依赖并应用保存配置，不再只重启 Web 服务；显示共享服务可能短暂中断的说明。模拟后端同步真实状态派生与批量逐项语义，缺失记录、未启动的 PHP 依赖不再冒充成功。

在已有 Rust 模块内补充验证，未新增测试文件：记录保存失败后的整批恢复、重复及缺失 ID、无效路径标识、重载失败和恢复失败报告、无关 Web 配置隔离以及生命周期锁。隔离真实 Nginx 使用临时目录、随机端口和两个静态站点：批量停止先让 Nginx 应用变更，再令另一个相关服务写配置失败，验证原站点和保留站点均能恢复 HTTP 内容；随后单独停止/启动 Nginx 站点不触碰损坏的 Apache 配置，快捷入口与实际内容一致。设置 NSB_SKIP_HOSTS=1，未修改系统 hosts；进程由 RAII 清理。Apache 故障使用不执行的入口文件，仅验证编排失败恢复，不冒充 Apache 原生验收。首次原生用例因测试样例重复域名而被业务校验拒绝，改成独立域名后复核通过。

隔离浏览器通过脚本响应注入统计调用和模拟错误，不修改产品调试代码或用户页面。验证两站操作部分失败后重试仅发送失败 ID，成功结果保留；请求异常保留选择；运行时 Escape 不关闭、选择禁用；完成后焦点进入结果；错误详情长文本换行。站点详情启动已停止 PHP 站点会调用 start_site 并恢复依赖状态。已检查 1280、390、320 宽度，中英文和主题场景；320×480 英文深色目检发现重复通知遮挡底部操作，已删除这处重复 toast，保留弹窗结果与焦点提示；最终弹窗宽 296px、x=12，正文独立滚动、无横向溢出，底部按钮可点击，关闭后焦点回到批量入口。pnpm --filter @nsb/web check 通过；精确暂存树导出到独立临时目录后 cargo check --workspace --all-targets --locked 通过，Rust 回归串行执行 613 通过、0 失败、12 ignored、28 filtered out，真实 Nginx 停止回滚及隔离流程在该发布树再次通过。未把 ignored 或 filtered 项报告为已验证。

已确认 v0.2.64 Release completed/success；本轮版本统一为 0.2.65，新增 annotated tag 并与 main 原子推送，核对远程提交及构建实际状态。原有 configgen.rs 178 additions / 9 deletions 与未跟踪生成文件保留，不纳入本轮提交。未运行本地前端 dev/build，未新增依赖。本次没有数据库变更，未修改 update.sql。站点保存/删除的进一步隔离、隧道入口端口一致性和此前跨平台原生边界仍待继续验证，整体目标保持进行。

## 第八十轮：临时隧道实际入口、HTTPS 验证与站点失效处理（v0.2.66）

参考 ServBay 官方临时隧道与公网访问说明（https://support.servbay.com/advanced-settings/how-to-use-cloudflared、https://support.servbay.com/advanced-settings/access-from-internet），继续核对已运行站点的分享流程。原逻辑从 Web 服务主端口构造 HTTP 地址，即便站点已经启用 HTTPS 或使用独立 vhost 端口，仍可能转发错误；首域名是通配域名时也无法创建。本轮复用 v0.2.64 的已加载入口快照及端口归属检查，按真实协议、域名和端口构造隧道。通配站点沿用实际访问地址的域名展开规则，设置中尚未生效的新端口不改变当前转发目标。

通过 Context7 核对 cloudflared 和 reqwest 官方实现后，HTTPS 本地探测使用 URL 域名进行 SNI 与证书校验，并将 DNS 固定到 127.0.0.1；请求不使用代理、不跟随跳转。cloudflared 的上游 URL 固定使用回环地址，通过 --http-host-header 和 --origin-server-name 匹配站点，--origin-ca-pool 使用独立临时证书快照。默认站点读取本地 CA，显式导入/ACME 站点读取其既有证书链并复用来源校验；不向隧道传递私钥，不关闭 TLS 校验。独立目录随隧道记录保存配置和公开证书，避免用户已有 cloudflared 配置或后续证书文件变动改变本次启动参数。拒绝非 127.0.0.1 的 IP 字面量入口并提示添加本地域名，因为 reqwest 的 DNS 覆盖不会改写字面 IP。没有引入依赖或修改证书持久化结构。

站点隧道保留创建时的站点、进程及已加载地址；监测发现配置停用、服务重启、入口变化或监听归属不符后，停止隧道并保留可重试的失败记录。生命周期锁忙时跳过该次归属检查，下一轮继续，避免退出与服务启停互相阻塞；此检查是轮询，不承诺站点切换的零时间窗口。所有隧道改为真实 HTTP/HTTPS 可达性探测，不再只检查 TCP 端口；本地服务不可达时不能显示 connected，持续不可用沿用 90 秒超时结束。去重同时考虑站点、协议和可信证书，旧入口失效后不能直接复用旧连接。

工具箱下拉及创建前提示显示后端实际地址；未运行或地址未确认的站点不能创建。创建失败保留后端错误建议，停止或失效记录不再提供复制失效分享链接的按钮。浏览器模拟使用已加载 accessUrl 和派生依赖状态，支持地址失效后终止模拟隧道，保留浏览器预览不创建公网连接的说明。同步中英文文案，提高本轮说明文字对比度；沿用下拉虚线和左右留白，不改全局组件。

在已有 Rust 模块补充 HTTPS、SNI、Host、临时证书目录、域名不匹配、证书过期、不受信任、自签名证书及站点失效停止的验证，未新增测试文件。首次隧道回归中一条旧断线测试仍期望本地停止后 connected，已按新的真实连接语义改为 reconnecting 后通过。隔离真实 Nginx 使用临时配置、随机端口、通配域名和本地 CA 验证 HTTPS HEAD 与实际静态内容，并验证修改端口设置仍保持原运行地址；NSB_SKIP_HOSTS=1，不修改用户 hosts，进程由 RAII 清理。官方 cloudflared 可执行文件验证 HTTPS 参数被接受，--help 不建立公网隧道。本轮未执行公网端到端分享，也未单独验收 Apache 或 macOS 原生 HTTPS。

浏览器隔离上下文覆盖 1280、390、320 宽度，检查中文浅色、英文深色、长 HTTPS 地址、证书错误与建议、目标失效后的禁用/失败状态、重新创建及自定义 HTTP 端口。站点创建仅发送站点 id，自定义端口发送实际端口；失效条目不提供复制，键盘打开下拉后 Escape 恢复入口焦点。下拉分隔线为 dashed，两侧约 12px，无横向溢出，最终页面异常为 0。注入脚本只用于浏览器响应中的临时样例和调用统计，未写入产品；首次错误样例缺少 code 导致错误规范化失败，补齐真实接口结构后验证通过。隔离上下文已关闭，用户原页面保留。

版本和 Cargo.lock 中三个本项目 crate 统一为 0.2.66；pnpm --filter @nsb/web check、cargo metadata --locked --no-deps 与 git diff --check 通过。精确暂存树导出到系统临时目录后，cargo check --workspace --all-targets --locked 通过；Rust 回归串行运行 615 通过、0 失败、12 ignored、29 filtered out。该发布树另行通过 3 项隧道原生生命周期检查、真实 Nginx HTTPS 验证和官方 cloudflared 参数验证，不依赖工作区 configgen.rs 的原有未提交修改。未启动前端 dev、未执行本地前端 build。本次没有数据库变更，未修改 update.sql。

已确认 v0.2.65 Release completed/success。遵循根 AGENTS.md，新建 annotated tag v0.2.66，与 main 原子推送，并核对远程指向及实际构建状态；不移动旧 tag。保留原有 configgen.rs 修改和本地生成文件。其他功能缺漏、站点保存/删除隔离及跨平台验证继续处理，整体目标保持进行。

## 第八十一轮：站点保存与删除的服务隔离、失败恢复及草稿反馈（v0.2.67）

继续参考 ServBay 站点管理面板 https://support.servbay.com/basic-usage/websites/website-management-panel，沿用按站点管理配置且保留项目文件的操作语义。通过 fast-context 和现有链路确认：保存、删除站点仍无条件重建 Nginx 与 Apache 配置，无关服务损坏会阻塞操作；停用站点的修改也可能重启其他运行实例。保存失败的恢复流程使用连续问号返回，首个恢复错误会跳过后续记录、证书及服务处理；切换 Web 服务期间新启动的实例也可能遗留。

保存与删除现在根据已有启用 vhost、遗留在另一服务下的启用配置，以及保存后实际启用的目标服务确定重载范围。仅处理停用配置时不重载服务；切换服务器则同时处理原服务和目标服务，不能静默保留旧 vhost。共用 SiteWebChanges 记录本次尝试、成功应用及之前的进程状态，避免重复重启刚启动的实例。失败后只恢复本次已触及的运行服务；原先未运行而由本次操作启动的 Web 实例会停止，Windows 配置校验失败且原进程未变时不再多余重启。删除继续保留项目目录、数据库、导入/ACME 及受引用证书，原有 hosts 保留选项与暂存文件恢复语义保持。

文件恢复会尝试所有快照并汇总具体路径，记录写入未成功时不再尝试无意义的记录回写。保存的回滚分别恢复配置、站点记录、证书记录、hosts 和相关服务；任何步骤恢复不完整都返回 SITE_UPDATE_ROLLBACK_FAILED 及原始错误、恢复失败细节。另将原文件内容与站点/证书元数据保留在 backup/site-update-recovery-* 目录，通过 recovery.json 记录原路径和副本映射，供恢复；目录不能自动删除。若恢复副本写入也失败，明确报告部分副本所在路径或已有备份提示，不声称副本完整。Unix 恢复目录设为 0700，错误中不输出证书私钥。保存全过程沿用站点、生命周期和证书锁，并与 hosts 编辑串行。

详情面板复用现有错误结构、色彩与确认弹窗，保存失败显示错误、建议及可展开详情，焦点进入反馈区；不再依赖重复 toast。底部错误区限制高度并独立滚动，保持取消、保存按钮可见，虚线和左右留白保留。保存、删除、重载共用同步互斥检查，PHP 版本选择随 busy 禁用；保存期间日志导航也禁用，避免操作未结束时离开详情。失败也刷新站点、服务、证书及 hosts 状态，但同站点草稿保留。模拟后端不再接受前端覆盖派生状态、访问地址和数据库绑定，缺失站点明确报错；运行中切换 Web/PHP 依赖沿用已有服务动作，停用站点编辑保持停用，并同步域名映射。

在已有 Rust 测试模块补充停用站点不触碰损坏主配置、记录恢复失败、后续文件继续恢复和恢复副本内容验证，未新增测试文件。既有删除配置失败用例改为启用 vhost：停用 vhost 不再触发主配置重建，因此不能再用该旧行为制造故障。隔离真实 Nginx 验收扩展到修改根目录后的 HTTP 内容、无关 Apache 故障隔离、切换 Apache 失败时 Nginx PID 保持、第二个相关服务出错后保存/删除回滚、项目文件保留及收回本次启动的 Web 进程。Apache 使用不会执行的故障入口，不代表真实 Apache 验收；Nginx 使用临时目录和随机端口，NSB_SKIP_HOSTS=1，RAII 清理进程。新增恢复清单的初次编译发现 serde_json 错误未转换，补齐项目既有 AppError 转换后验证通过。

隔离浏览器覆盖中文浅色、英文深色与 1280、390、320 宽度。通过响应临时注入模拟长恢复错误和延迟请求，验证运行时 Escape 不关闭、输入与日志导航禁用、失败刷新列表但保留草稿、错误焦点、展开详情、取消放弃后继续编辑、重试成功清除错误，以及同一时刻连点只产生一次 update_site。320×480 下详情宽 296px、左右各 12px，错误区最高 160px且可滚动，保存按钮仍可点击，无横向溢出。删除确认保留项目/数据库不删除的说明和可选清理项，取消后仍回到详情。浏览器热更新曾关闭正在检查的面板，按稳定刷新后的状态重新验证；最终页面异常为 0，未向产品写入临时调试入口，隔离上下文已关闭。

包、crate、Tauri 与界面版本统一至 0.2.67，Cargo.lock 只更新三个本项目 crate；pnpm --filter @nsb/web check、cargo metadata --locked --no-deps 与 git diff --check 通过。精确暂存树导出到独立临时目录后，cargo check --workspace --all-targets --locked 通过，Rust 串行回归 617 通过、0 失败、12 ignored、29 filtered out；该发布树上的真实 Nginx 完整保存/删除/恢复流程另行通过。未执行的 ignored/filtered 项不计入验证，Apache 和 macOS 原生恢复尚未验收。未启动前端 dev、未执行本地前端 build，未增加依赖。本次没有数据库结构或数据修复 SQL 变更，未修改 update.sql；应用正常的站点保存/删除沿用现有存储结构。

已确认 v0.2.66 Release completed/success。本轮必须新建 annotated tag v0.2.67，与 main 原子推送并核对远程提交和构建实际状态。原有 configgen.rs 178 additions / 9 deletions 及未跟踪生成文件不纳入发布。继续核对站点级 PHP 配置写入反馈、反向代理编辑与其他功能缺漏，整体目标保持进行。

## 第八十二轮：反向代理路径一致性、地址反馈与服务器选择（v0.2.68）

沿用现有反向代理创建、编辑和站点应用链路。fast-context 确认目标地址的编辑入口已经存在；真正的问题是 Nginx 与 Apache 对基础路径末尾斜杠的处理不一致。通过 Context7 核对官方 proxy_pass 与 ProxyPass 文档（https://nginx.org/en/docs/http/ngx_http_proxy_module.html、https://httpd.apache.org/docs/2.4/howto/reverse_proxy.html），并在已有原生验证中复现：填写 /api 时根请求被转发成 /api，而非 /api/；URI 替换也会使 /users 拼接为 /apiusers。现在 sites::proxy_url 统一为目录形式，省略协议时继续补 HTTP，已有斜杠不重复追加，保留编码路径；同时拒绝控制字符。两种配置生成器继续复用这个入口，不修改工作区原有 configgen.rs。

创建与编辑复用同一个前端地址规范化函数，显示实际基础地址与路径追加示例，禁止账号密码、查询参数、片段、非法协议、端口零及配置控制字符；无效时禁用下一步或保存，并在输入旁给出中英文提示。提交使用预览中的地址，浏览器模拟后端在副作用前执行相同校验。新建反向代理原先隐藏了 Web 服务器选择，现恢复 Nginx/Apache 选项并保留已安装检查；选择按钮补充 aria-pressed，代理输入补齐标签关联及描述。确认页的长地址换行显示，底部动作与虚线两侧留白沿用现有布局。已有站点重新保存或重载后生成新规则，不主动重启用户运行中的服务。

扩展已有 Rust 验证模块，未新增测试文件。隔离真实 Nginx 使用临时目录、随机端口和本地回显上游，对无末尾斜杠、已有斜杠、根地址及编码路径逐一应用配置并请求根路径、嵌套路径及带查询参数的地址，核对上游实际收到的 URI；用例在修复前失败、修复后通过。保留原有 HTTP/HTTPS 静态路由与实际入口检查。设置 NSB_SKIP_HOSTS=1，进程由 RAII 清理，不修改系统 hosts。本轮未原生验收 Apache、HTTPS 上游、WebSocket 或 macOS；这些边界不能由配置字符串验证代替。

隔离浏览器验证创建、编辑、非法地址阻止提交、HTTP 默认协议、HTTPS 与 IPv6 地址预览、步骤返回保留输入、失败草稿保留及重试。通过脚本响应注入统计请求和模拟错误，非法输入零次 update_site，成功提交地址与预览一致；Apache 未安装时阻止继续，模拟已安装后实际 create_site 参数为 apache。注入只存在于隔离上下文，不写产品调试代码。检查中英文、浅深主题与 1280/390/320 宽度；320×480 弹窗宽 296px、两侧 12px，正文可滚动，底部动作可见，长地址无横向溢出。初次截图位于过渡动画期间，待布局稳定后重新目检；一次脚本按旧排序打开了其他站点，按实际列表顺序纠正后通过。最终页面异常为零。

版本统一至 0.2.68，Cargo.lock 只更新三个本项目 crate。pnpm --filter @nsb/web check 通过；本次暂存树导出到独立临时目录后，cargo check --workspace --all-targets --locked 通过，Rust 串行回归 617 通过、0 失败、12 ignored、29 filtered out，另行执行真实 Nginx 检查通过。未执行的忽略或筛选项不计入验收。未启动前端 dev、未执行本地前端 build、未新增依赖。本次没有数据库变更，未修改 update.sql。

已确认 v0.2.67 Release completed/success。遵循根 AGENTS.md，新建 annotated tag v0.2.68，与 main 原子推送并核对远程指向和实际构建状态；不能只推分支，也不能把已触发构建称为安装包已发布。保留原有 configgen.rs 178 additions / 9 deletions 及本地生成文件。站点级 PHP 写入反馈和其他真实功能、跨平台边界继续核对，整体目标保持进行。

## 第八十三轮：站点 PHP 设置、项目文件保护与保存反馈（v0.2.69）

参考 ServBay 的项目 PHP 配置说明 https://support.servbay.com/php/how-to-use-user-ini，并通过 Context7 核对 PHP 官方 .user.ini 文档：仅 CGI/FastCGI Web 请求读取项目配置，CLI 不读取；默认 user_ini.cache_ttl 为 300 秒，保存文件不代表当前 PHP worker 立即清除缓存。fast-context 确认原有 phpOverrides 已有存储字段和写入入口，但写入错误被忽略、整份项目文件被覆盖，详情也没有设置入口。本轮复用现有站点 API、存储与错误结构，没有新增接口、依赖或数据库结构。

站点详情增加 PHP 页签，按需添加九项常用设置：内存、上传大小、POST 大小、执行时间、输入时间、输入变量数、上传数量、显示错误和记录错误。大小采用数值与单位，内存支持不限，开关使用 Switch，禁止重复添加；删除和清空只修改草稿，保存才生效。显示 .user.ini 路径、缓存说明与折叠的立即生效条件，不把默认设置误称为即时生效。非法值阻止保存，页签与底部都有提示；通过 MutationObserver 等待页签实际激活后聚焦错误项。保存期间禁用控件并沿用同步互斥，失败保留草稿及错误建议。成功返回后立即更新站点查询缓存，修复立即关闭重开显示旧值的问题。

前后端共同校验常用设置的范围与格式，拒绝新增未知项和配置注入。旧记录中未修改且语法安全的自定义项允许保留，例如 date.timezone=Asia/Taipei；界面只读展示并允许主动移除，说明其生效仍取决于 PHP 配置范围。不能通过修改这些旧项绕过新增校验，mock 使用已保存记录进行同样判断。独立写入入口没有可验证的旧记录，继续只接受支持的常用项；生产生命周期通过 UserIniChanges 读取原记录校验。

.user.ini 改为只维护明确的 NiceEnv BEGIN/END 区块，保留手写内容和 CRLF，清空设置只删除托管区，未托管空文件保持。旧版整文件托管格式只在与原站点记录完全一致时迁移，手动改过或标记损坏则报错并说明处理方法。拒绝链接、目录、超过 1 MiB 和非 UTF-8 文件；只读、写入和备份失败明确反馈。写前备份原文件与 files.json 路径映射至 backup/site-php-settings-*，同目录临时文件同步后原子替换并保留权限。写入与恢复前比对内容，避免覆盖已检测到的外部修改；恢复只处理已写文件并逆序尝试全部，失败保留备份提示。共享根目录不能应用不同覆盖项，移动根目录或切换非 PHP 时清理旧托管区，但仍被其他 PHP 站点使用则保留。

创建、保存、启动均接入文件计划与恢复，配置和记录保存失败会恢复已写 .user.ini。启动失败时即使 PHP 文件恢复失败，仍尝试恢复 Web 配置；配置恢复成功后继续尝试服务恢复，汇总原始错误和恢复错误。已有 Rust 模块补充四项验证，未新增测试文件，覆盖手写保留、数据库写入失败回滚、旧自定义项保留及非法修改拒绝、旧文件迁移、共享目录、外部修改、只读文件与创建失败。创建失败样例注册已安装套件但不提供可执行文件，确保经过 .user.ini 写入后才启动失败，并验证备份与原文恢复，避免只验证提前拒绝。

已有真实 PHP 原生检查扩展为独立 FastCGI 请求：通过 sites::update 写入 192M，清除隔离 PHP 池缓存后读取到 memory_limit=192M，同时手写 precision=12 保持；清空覆盖后读取到全局 512M，项目文件原文恢复。保留原版本和扩展启停验收，使用临时目录、复制的 PHP runtime、随机端口、NSB_SKIP_HOSTS=1 与 RAII 清理，不修改用户 hosts 或用户运行中的服务。另用 PHP 8.4.26 的 ini_get_all 确认九项设置均支持 PERDIR/ALL。本轮未原生验证 macOS，也未将忽略和筛选用例计入通过数。

全量回归定位到端口自动回落的真实问题：附近 32 个端口均被排除时，固定次数反复向操作系统申请并立即释放端口可能重复分配同一被排除端口。现保留被拒绝的监听器直到选择结束，尝试次数覆盖排除列表；已有回落与端口稳定性用例通过。独立发布树不含原有 configgen.rs 修改，Rust 串行回归 621 通过、0 失败、12 ignored、29 filtered out；真实 PHP FastCGI 与已有 user_ini_written_for_php_sites_only 集成用例另行通过。最终兼容性改动的四项 PHP 用例、pnpm --filter @nsb/web check、cargo check --workspace --all-targets --locked 及 git diff --check 均通过。

隔离浏览器验证九种控件、重复添加限制、非法数值、不限内存、清空大小保留单位、删除焦点、跨页签错误定位、保存禁用、重复点击仅一次请求、失败保留与重试、立即重开及清空提交。响应内注入初始 date.timezone 样例，验证只读保留可与内存设置一起保存，主动移除才删除；注入仅用于隔离浏览器，未写入产品。检查中英文、浅深主题与 1280/390/320 宽度，最终 320×480 面板宽 296px、两侧各 12px，无横向溢出，正文滚动且底部保存可用。提高说明与标签对比度，保留虚线及左右留白；最终页面异常为零。

已确认 v0.2.68 Release completed/success。版本、Tauri 和本项目 crate 统一至 0.2.69；遵循根 AGENTS.md 创建新的 annotated tag，与 main 原子推送并核对远程提交及实际构建状态，不覆盖旧 tag，不把已触发称为安装包发布成功。保留原有 configgen.rs 178 additions / 9 deletions 及未跟踪本地文件。未启动前端 dev、未执行本地前端 build。本次没有数据库变更，未修改 update.sql。其他功能缺漏和跨平台验证继续处理，整体目标保持进行。

## 第八十四轮：环境变量草稿、冲突检测与项目文件保存（v0.2.70）

继续参考 ServBay 的 Laravel 项目配置说明 https://support.servbay.com/php/frameworks/create-and-run-laravel-project；项目根目录的 .env、Web public 目录、实际数据库连接与应用缓存应明确区分。fast-context 沿前端 EnvEditor、Tauri 命令及 envfile.rs 核对到：新增变量不显示待保存行，补全数据库直接写文件并清空草稿，预览后修改目录仍可能写向另一个项目，保存不检查外部修改且直接截断文件，mock 保存始终返回成功但不持久化状态。原 UI 固定键名列挤压窄屏输入，聚焦密码会自动显示明文，父面板关闭没有检查环境草稿。

编辑器改成可见的逐项草稿，新增后显示行并聚焦，支持保留大小写和框架使用的点分键名，新增前检查格式与重复项，已输入但未添加的内容也触发离开提醒。恢复单项和放弃草稿都不写文件；放弃全部或重新读取须明确确认。敏感值使用密码输入，聚焦不再显示明文，通过带名称的按钮显式切换；多行值用文本区编辑。数据库补全通过新增的只读预览命令按后端框架规则生成变量，保留同名用户草稿，保存前不写文件。空值、空文件、文件缺失、读取失败重试、保存失败与恢复建议均有状态；保存返回最新视图直接更新，不依赖二次读取成功来确认写入。

环境文件与站点设置分别保存，父面板固定底栏根据页签显示 Save .env 或站点保存，关闭保护覆盖两份草稿与新增输入。加载、补全和保存期间有同步互斥，阻止重复提交与关闭；切换页签保留输入。目录存在未保存变更时环境编辑暂停，已有环境草稿必须保存或放弃后才能提交目录切换，切换成功重新读取对应项目文件。日志跳转在有未保存内容时禁用，避免丢稿。错误区可聚焦且位于滚动正文内。新行采用上下排列的标签与值，长路径可换行；分隔线保留虚线与左右留白。截图发现成功 toast 覆盖窄屏底部后，改为编辑器内的状态提示，底栏显示无待保存更改。

后端 EnvFileView 增加绑定站点 ID、保存的根目录、规范化项目路径、语法、文件存在性和内容的 revision。读取、保存和数据库预览与站点变更使用同一锁；env_save 必须提交读取版本，文件内容、目录、框架语法或存在性变化即拒绝，不把旧草稿写入新项目。原文件与 .env.nsb-backup 均检查普通文件、大小、UTF-8、只读与链接/Windows reparse 状态，备份写入失败不修改原文件。保存通过同目录临时文件、同步与原子替换进行，保留已有权限，新建文件沿用 tempfile 的限制权限。写前再次比较内容，缺失文件使用不覆盖的创建方式。保留原内部数据库补全入口，但也必须按读取版本保存；Tauri 写入沿用数据目录共享活动锁。未新增依赖和数据库字段。

环境文本处理按逻辑赋值读取多行引号值，保留 export、行尾注释、CRLF 和末尾换行状态。注释示例保持原样，使用示例会追加有效赋值；同一作用域的重复有效键同步更新，界面有说明。未闭合引号和不支持的行尾组合明确报错，不自动重写损坏文本。ThinkPHP 的 INI 分节以 section.key 表示，修改数据库分节不会改 Redis 的同名变量；新增全局键放在首个分节之前，新增已有分节的键放入对应分节。根据真实 INI_SCANNER_RAW 结果区分分号注释与值内 #，含分号的值正确加引号。CodeIgniter 与普通 dotenv 延续各自已有转义规则。

通过 Context7 核对 /vlucas/phpdotenv 的 Dotenv::parse、注释、字面美元符与多行规则。在已有 envfile.rs 模块增加验证，未新增测试文件：预览零写入、真实文件保存与备份、外部内容变化、目录变更、缺失文件被外部创建、只读和备份目录冲突、非法键名、空文件创建、注释、多行、重复键、分节和 CRLF。显式原生用例使用现有隔离 PHP 8.4.26 与 Laravel vendor，对真实 save_env 输出调用 phpdotenv 解析，确认中文和特殊字符密码、美元符、多行、空值与未修改项；另用 PHP INI_SCANNER_RAW 验证 ThinkPHP 分节、分号和 # 值。仅使用临时项目文件与现有隔离 runtime，不启动服务，不读取用户项目凭据。

精确暂存树导出到独立临时目录，排除原有 configgen.rs 修改。Rust 串行回归 624 通过、0 失败、12 ignored、30 filtered out；已有 env_read_save_round_trip 集成检查通过。最终分节处理补充后单独复核全部环境模块用例（包含显式原生项）26 通过，cargo check --workspace --all-targets --locked 通过。前端 pnpm --filter @nsb/web check 通过。忽略和筛选用例不算已执行，未把 Windows 的 PHP 验证等同于 macOS 验收。

隔离浏览器验证新增可见、点分与大小写键名、非法及重复名称、constructor 等特殊名称、聚焦后密码仍隐藏、数据库补全零写入且保留草稿、跨页签草稿、关闭确认、重复点击仅一次请求、运行期间禁用、失败保留及重试、保存后立即重开。注入外部修改后验证拒绝写入，取消重新读取保留草稿，确认后显示最新内容；模拟读取失败可重试；目录变更在有草稿时阻止提交，放弃草稿后成功保存目录并加载对应文件。检查中英文、浅深主题和 1280/390/320 宽度；320×480 面板宽 296px、左右各 12px，无横向溢出，失败和成功时保存按钮均保持可见，最终成功没有浮动 toast，页面异常为零。临时注入只存在于自建浏览器上下文，未写入产品。

已确认 v0.2.69 Release completed/success。本轮版本同步为 0.2.70，按根 AGENTS.md 新建 annotated tag，与 main 原子推送，核对远程提交与实际构建状态。保留 configgen.rs 的 178 additions / 9 deletions 和未跟踪本地文件。未启动前端 dev、未执行本地前端 build。本次没有数据库变更，未修改 update.sql。环境变体文件的进一步管理、项目运行时锁、其他真实功能缺漏与跨平台验收仍待继续，整体目标保持进行。

## 第八十五轮：环境文件选择、备份还原与变量搜索（v0.2.71）

沿 fast-context 核对 EnvEditor、Tauri 与 envfile.rs：原编辑器只能修改固定 .env，其他变体仅列为文字，保存产生的备份没有还原入口。本轮继续按 ServBay 的项目环境配置设计补齐真实文件操作；使用已安装 Next 文档和 Context7 的 Symfony 文档核对变体加载与缓存规则。选择文件只代表编辑对象，不切换项目运行环境，也不宣称不同框架有相同的加载顺序。

新增现有文件、常见新文件和自定义文件选择；缺失文件只有添加变量并保存才创建，读取、补全预览与空变更保存均不创建。合法自定义文件可被扫描发现，名称限制在当前项目的 .env / .env.*，拒绝路径、Windows ADS、尾点、过长名称以及大小写变体的工具备份和 .env.local.php。Symfony 已生成的 .env.local.php 单独提示按框架流程刷新，不自动删除或编辑缓存。.env.example / .env.dist 有模板说明。文件选择分组复用虚线和左右 8px 留白；长文件名和固定底栏可截断，正文路径可换行。

后端保留默认 .env 的旧 wrapper，新增 named read/save/DB preview 与 restore 命令。revision 加入文件名，相同内容不能跨文件提交；各文件使用独立的 <name>.nsb-backup。Symfony 未保存数据库密码时只从所选文件的 DATABASE_URL 保留已有密码，不误用默认 .env 的凭据。还原预览只返回受影响变量名，重复定义也参与比较；另有全文差异标记，只有注释或格式变化仍可还原。真正还原时同时校验当前文件和备份 revision，将当前全文保存为 <name>.nsb-before-restore，再原子替换；保留源备份，缺失文件也可从备份恢复。延续普通文件、大小、UTF-8、链接/重解析点、权限与原子写入保护，写入使用数据目录活动锁。只读目标的恢复操作可能先保存恢复副本，但不会覆盖当前文件或备份源。

切换有草稿的文件须确认放弃；取消和读取失败保留当前文件及草稿。搜索只按变量名过滤，隐藏行仍保留编辑；空结果提供清空搜索。自定义名称有长度、字符和格式提示。还原确认解释全文替换、草稿清除、备份来源和恢复副本，敏感值不进入预览。失败留在确认框并保留草稿，可重新预览；成功更新返回视图并显示内联状态。同步互斥阻止连续点击重复写入，还原期间阻止关闭。浏览器 mock 增加各文件、独立备份和恢复状态，明确只模拟内存操作。

在已有 Rust 环境模块扩展验证，没有新增测试文件。覆盖跨文件 revision、独立备份、缺失文件零写入、自定义变体发现、保留文件名拒绝、所选 Symfony 密码、全文与注释还原、预览不泄露值、当前和备份冲突、目录冲突及只读目标。30 项环境模块检查全部通过，含已有隔离 PHP 8.4.26 对保存/还原文件调用真实 phpdotenv，以及 INI_SCANNER_RAW 验证。前端 pnpm --filter @nsb/web check 通过。

隔离浏览器确认新文件保存、同名变量在不同文件独立、备份还原、搜索保留草稿、取消/读取失败保留草稿、保存与还原失败保留及重试、自定义非法文件名限制、对话框焦点、重复点击只发一次请求、运行期间禁用和 Escape 关闭保护。检查中文浅色、英文深色和 1280 / 320 宽度；320×480 面板宽 296px、左右各 12px，长文件名不撑宽，保存按钮可见，下拉分隔线为 dashed 且两侧 8px。临时失败注入仅位于自建浏览器上下文，未写入产品；页面异常为零。

精确暂存内容导出为独立发布树，排除原有 configgen.rs 修改；cargo check --workspace --all-targets --locked 通过，Rust 串行常规回归 628 通过、0 失败、12 ignored、30 filtered out。显式原生 PHP 环境文件用例在同一发布树另行通过。最终版本的前端类型检查及 git diff --check 通过；忽略、筛选项和 macOS 运行验证不算已执行。

已核实 v0.2.70 Release completed/success。版本统一至 0.2.71，并按根 AGENTS.md 新建 annotated tag，与 main 原子推送，核对远程提交和 Release 实际状态。保留原有 configgen.rs 的 178 additions / 9 deletions 及未跟踪本地文件。未启动前端 dev、未执行本地前端 build。本次没有数据库变更，未修改 update.sql。项目运行时锁、其他真实功能缺漏与跨平台验收仍需继续，整体目标保持进行。

## 第八十六轮：站点终端版本一致性与自动注入（v0.2.72）

上一轮完成实质发布与验证，本轮重新检查 main、工作区和 Release：v0.2.71 已 completed/success。fast-context 沿站点/总览终端按钮、open_terminal、pathenv 与 SiteRuntime 追踪，确认现有打开动作仅设置工作目录，未使用应用选择的 PATH 版本，更没有使用站点绑定的 PHP。参考 ServBay 的 CLI 与网站环境说明 https://support.servbay.com/php/set-different-php-for-each-project 和项目配置说明 https://support.servbay.com/advanced-settings/using-servbay-config ，本轮先补足站点 Web PHP 与新终端 CLI PHP 的明确对应，不把这项功能描述成完整的项目版本文件或目录自动切换。

复用现有 terminal_environment 与 PATH 选择规则，新增按站点读取的快照。PHP 站点强制使用已保存的 phpVersion，即使全局 PATH 选择未勾选 PHP，也注入该站点版本；其他命令沿用全局 PATH 选择。指定 PHP 未安装、缺少真实 CLI 入口、入口越界、无执行权限或路径不适合 PATH 时明确失败，不能悄悄换另一版。终端校验 php.exe / php，而非清单中用于服务的 php-cgi / php-fpm；Unix sbin/php-fpm 对应 bin/php。工作目录复用环境文件的项目根目录识别，Laravel public 等在存在项目标识时回到项目目录，不自动执行 runtime.command 或项目脚本。

TerminalEnvironment 新增绑定站点、目录、版本目录、脚本和警告的 revision。打开命令只接收站点标识与 expectedRevision，脚本和目录均由后端重新生成；版本、目录或可用项变化则要求刷新预览。沿 SITE_CHANGES -> lifecycle 顺序持锁直到终端进程创建，Tauri 同时持有数据目录活动锁，避免预览后站点编辑、卸载或迁移与启动交错。不修改系统 PATH、已保存的版本选择、站点记录或项目文件。

Windows 使用现有可见独立 PowerShell 启动方式，加入由后端生成的 UTF-16LE EncodedCommand，当前目录通过进程 API 传入；注入只影响新终端及子进程。参数长度有上限，脚本沿用引号与 PATH 分隔符保护。macOS 通过固定 AppleScript 和单独 argv 将命令交给 Terminal，以无用户启动配置的 Zsh 建立所选环境，失败返回系统错误与权限提示；目录和脚本不插入 AppleScript 源码。现有工具箱也使用同一快照校验和自动注入入口，保留复制脚本到已有会话的高级用途。浏览器继续明确只演示，不假装打开系统终端。

站点列表与总览复用新的 SiteTerminalButton：先显示工作目录及将使用的命令版本，站点 PHP 排第一，CLI 条目不再标成 FPM。说明明确会话范围和切换目录不会自动更改版本。窄窗口中优先呈现目录和版本，将长说明、脚本放在可滚动正文后部；固定底栏显示刷新与打开，长名称限两行、路径与标签可换行。版本变化错误需刷新后再打开，读取错误不允许使用旧缓存启动；错误区聚焦，关闭后恢复触发按钮焦点。同步互斥阻止重复打开和启动期间关闭。

在已有 pathenv.rs 验证模块中增加真实文件和启动前检查，未新增测试文件。覆盖站点 PHP 覆盖全局选择、未勾选 PHP、CLI 缺失、项目根目录、预览后 PHP/目录/全局选择变化、跨线程检查锁持有以及不写持久 PATH。21 项 PATH/终端检查通过，包含显式原生用例：启动短时隐藏 PowerShell，使用生产生成的参数（去掉交互 NoExit），将子进程初始 PATH 指向无 PHP 的目录，再执行真实 PHP 8.4.26，核对 Get-Command php 的路径、PHP_VERSION 和含中文、空格、引号及特殊字符的 cwd，父进程 PATH 保持原样。PowerShell 捕获验证输出时单独指定 UTF-8，避免系统代码页影响断言；没有启动长期服务或交互终端。

隔离浏览器验证两个站点分别显示 PHP 8.3.17 与 7.4.33、项目根目录、总览入口、读取/启动失败、刷新重试、错误聚焦、关闭焦点恢复、重复点击一次请求和启动期间 Escape 保护。临时注入只存在自建上下文，模拟打开不会启动系统终端；请求只含站点和 revision，不携带脚本。检查中文浅色、英文深色、1280 与 320×480；窄屏面板宽 296px、两侧各 12px、正文可滚动且底部按钮可见，版本分隔线为内缩虚线。自建上下文已关闭，原有页面保留，页面异常为零。最终 0.2.72 前端类型检查通过。

精确暂存内容导出为独立发布树，排除原有 configgen.rs 修改。cargo check --workspace --all-targets --locked 通过；Rust 串行常规回归 630 通过、0 失败、12 ignored、31 filtered out，显式原生 PowerShell/PHP 用例在同一发布树另行通过。忽略和筛选项不算已执行；Windows 验证不等于 macOS Terminal 实机验收。

版本号统一为 0.2.72，依项目规则新建 annotated tag，与 main 原子推送并核对实际 Release 状态。保留原有 configgen.rs 178 additions / 9 deletions 及未跟踪本地文件。本次没有数据库变更，未修改 update.sql；未启动前端 dev、未执行本地前端 build。项目版本文件、其他语言的项目级选择、cd 自动切换及 macOS 原生终端验收仍需继续，整体目标保持进行。


## 第八十七轮：项目运行时版本、文件保存与终端应用（v0.2.73）

参考 ServBay 项目版本文件与 CLI 版本的设计（https://support.servbay.com/advanced-settings/using-servbay-config、https://support.servbay.com/nodejs/set-different-nodejs-for-each-project），沿用已有站点终端、PATH 快照、安装元数据和项目根目录识别。项目 .niceenv.json 使用 schemaVersion: 1 与 runtimes 版本映射；界面从已安装运行时派生下拉选项，无需手填 JSON。明确固定的项目 CLI 版本优先，未固定 PHP 继续跟随站点，其他命令沿用全局 PATH 选择。CLI PHP 与 Web PHP 独立，不改站点绑定或系统 PATH；切换目录不会自动更换版本。

读取不写盘，缺失或未知的固定项仍显示并允许解除，不依赖终端预览成功。保存以站点、真实项目目录、文件内容计算 revision，拒绝覆盖外部修改；保留未知顶层字段，拒绝不支持的 schemaVersion 和无效 JSON。复用 envfile 的普通文件、链接、容量、只读、原子替换检查；覆盖前在同目录保留 .niceenv.json.nsb-backup。共享目录的站点使用同一文件并在界面提示。public/out/dist/build 上级项目识别补充 Python、Go 及已有 NiceEnv 配置标记。没有引入依赖、数据库字段或 migration。

终端启动前再次验证固定版本及 CLI 文件，并把项目文件 revision 纳入环境快照；缺失或损坏时明确报错，不悄悄回退。卸载会检查已登记站点的项目固定版本，文件存在但无法读取时要求先修复；站点编辑、项目保存与卸载沿 SITE_CHANGES -> lifecycle 顺序持锁。保护范围是站点列表可访问的项目，不扫描任意磁盘目录或跟踪已移除站点。

站点终端新增“终端预览 / 项目版本”页签，保留草稿、关闭与重新读取确认、保存失败恢复、错误聚焦及重复提交互斥。未保存时阻止启动，保存成功后失效相关终端缓存，同目录站点下次读取即使用新配置。路径和备份说明折叠在选项之后，窄屏下选择控件堆叠，正文滚动而底部操作固定；下拉分隔线沿用内缩虚线。PHP、Node.js、Python、Go 的标签去除旧版本前缀，实际版本单独显示。浏览器只演示内存选择，明确不写文件、不启动系统终端。

已有 pathenv.rs 验证模块扩展真实文件检查：多运行时覆盖、共享目录、解除固定、保留未知字段、外部修改和目录变更冲突、无效配置、只读与备份失败、CLI 不可用、卸载保护及新项目标记。未新增测试文件。25 项 PATH/终端检查（含显式原生用例）通过：短时隐藏 PowerShell 在空的运行时 PATH 中读取真实 Node 22.21.0、Python 3.13.3 与 PHP 8.4.26，核对实际命令路径、版本及含中文、空格、引号的工作目录，父进程 PATH 不变。安装器定向回归 16 通过、1 个需联网下载的检查保持忽略。

隔离浏览器完成多版本选择、保存同步预览、草稿跨页签保留、关闭取消、缺失版本修复、保存冲突保留草稿、错误聚焦、重试及双击互斥检查；故障注入仅存在自建上下文的脚本响应，不写入产品。中英文、浅深色、1280 与 320×480 窗口检查通过，窄屏面板宽 296px、左右各 12px，保存按钮可见且没有横向溢出。未启动前端 dev、未执行本地前端 build。

版本统一为 0.2.73，Cargo.lock 仅更新三个本项目 crate。最终前端类型检查通过；提交内容导出的独立发布树通过 cargo check --workspace --all-targets --locked，pathenv/envfile/install 相关模块回归 68 通过、0 失败、4 ignored、605 filtered，另行显式执行两项原生终端验证均通过。未执行的其他模块、忽略项不计入验收。排除原有 configgen.rs 178 additions / 9 deletions 与本地生成文件。已确认 v0.2.72 三个平台的 Release 构建均 completed/success。本轮按根 AGENTS.md 新建 annotated tag v0.2.73，与 main 原子推送并核对远程指向及实际发布状态，不能只推送分支。

本次没有数据库变更，未修改 update.sql。仍需后续推进其他功能与 macOS 实机验收；.nvmrc / .python-version 自动识别及 cd 动态切换不在本轮支持范围，整体目标保持进行。

## 第八十八轮：项目标准版本文件识别与显式覆盖（v0.2.74）

延续 ServBay 项目版本设计，复用已有项目版本编辑、终端快照和运行时卸载检查。通过 fast-context 追踪调用链，并核对 nvm、nodenv、pyenv 官方版本文件规则。本轮只读取项目目录中的 .nvmrc、.node-version 和 .python-version，不向上查找用户级配置，不执行文件内容，也不改写原文件。明确固定的 .niceenv.json 版本优先，其次自动匹配文件要求，未指定的 PHP 跟随站点，其他命令沿用全局 PATH。

支持数字完整版本及主/次版本前缀、Node 的 v 前缀、BOM、空白与井号注释；.nvmrc 的保留 key=value 字段忽略，node/stable 选择最高已安装正式数字版本。两个 Node 文件同时存在时要求兼容并取交集；冲突、缺少匹配版本、空文件、无法读取及不支持的写法均明确反馈。LTS 别名、多个 Python 解释器、PyPy、预发布版本和其他管理器格式不猜测解析，用户可在项目版本页选择具体版本覆盖。不会自动下载安装，切换目录也不会自动换版。

读取异常保留为检测结果，编辑器仍可打开并修复。版本文件内容、存在性和错误纳入 revision，保存前重新读取检查，终端启动前同时检查文件和实际选中的已安装版本；新增更高版本导致自动选择变化时也要求刷新。恢复自动选择必须能匹配版本且真实 CLI 可用。卸载保护涵盖自动选中的版本，无法确认引用关系时返回具体提示；保护范围仍是已登记站点的项目。

项目版本下拉显示自动匹配结果和来源，终端预览也展示来源文件；未保存的覆盖使用“保存后”提示。未解决的自动选择错误阻止保存，但明确选版后可保存，保持原文件不变。沿用草稿保留、冲突反馈、错误聚焦和重复提交互斥。浏览器 mock 使用按目录保存的内存版本文件并实现相同主要规则，默认不注入文件，不假装读取真实磁盘。

已有 pathenv.rs 验证模块增加文件自动识别、Node 冲突、显式覆盖、最高匹配、保存/启动快照变化及卸载保护检查，未新增测试文件。28 项 PATH/终端检查显式包含原生用例后全部通过。原生短时隐藏 PowerShell 分别验收自动选择和明确固定，执行真实 Node 22.21.0、Python 3.13.3 及 PHP 8.4.26，核对路径、版本、中文/空格/引号工作目录和父进程 PATH 不变。初次新增夹具缺少版本 manifest，补齐后完整重跑通过。

隔离浏览器验证自动选择、外部文件修改导致保存冲突、重新读取、明确固定覆盖、恢复自动时的冲突提示、多个解释器错误修复及双击保存仅一次请求。中英文、浅深主题、1280 和 320×480 均检查，窄屏面板宽 296px、左右各 12px，保存按钮可见，无横向溢出；下拉分隔线为虚线且左右各 8px。四张截图已目检，页面异常为零。故障注入仅在自建浏览器上下文，该上下文已关闭，没有写入产品调试入口。

版本统一为 0.2.74，Cargo.lock 仅更新三个本项目 crate。最终前端类型检查及工作区 core 编译检查通过；精确暂存内容导出的独立发布树通过 cargo check --workspace --all-targets --locked，pathenv/envfile/install 模块回归 71 通过、0 失败、4 ignored、605 filtered out；同一发布树另行显式执行两项原生终端验证，均通过。未执行的其他模块和忽略项不计入验收。原有 configgen.rs 的 178 additions / 9 deletions 与未跟踪本地文件保留，不纳入提交。已核实 v0.2.73 Release completed/success。依根 AGENTS.md 必须新建 annotated tag v0.2.74，与 main 原子推送，并核对远程指向与实际 Release 状态。

本次没有数据库变更，未修改 update.sql。未启动前端 dev、未执行本地前端 build、未新增依赖。macOS 原生终端和目录自动切换尚未验收；整体功能完善目标保持进行。

## 第八十九轮：Node.js LTS 别名与项目版本刷新（v0.2.75）

继续参考 ServBay 项目 CLI 版本隔离，补齐 .nvmrc 常见 lts/* 和 lts/代号写法。fast-context 确认上游 Node 索引已提供 LTS 名称，但仅用于套件列表备注，项目版本无法使用。通过 Context7 核对 nvm 官方 README：LTS 别名由访问 Node 官方索引时更新，本地别名指向相应系列最新正式版本。本轮不根据偶数主版本或显示名称猜测 LTS，也不静态硬编码未来系列。

现有 Node.js 版本查询成功时，从完整官方 index.json 提取各 LTS 代号及最新版本，另存为本机已有 settings 存储中的别名快照，不受平台过滤和版本列表显示数量限制。lts/* 指向最新 LTS，lts/代号大小写不敏感，要求安装精确解析版本；缺失时显示文件要求和具体版本，不悄悄退到旧版。项目读取只用缓存，不自动联网、安装或执行文件；索引刷新失败保留上次快照，清理普通套件列表缓存也不清除项目仍需使用的 LTS 信息。缓存写入与项目保存、卸载及终端启动通过已有站点锁串行。

项目 revision 纳入自动检测结果，LTS 别名或已安装匹配结果变化时，旧保存与启动预览均需重新核对。明确固定的 .niceenv.json 仍优先，原 .nvmrc 保持。无法读取缓存、未知代号、两个 Node 文件冲突和解析版本未安装均可通过固定版本修复。未新增 API 或 schema 字段，沿用 version_catalog 与已有项目读写接口。

使用 LTS 的选项显示来源、解析版本和缓存规则，并提供“刷新 Node.js 版本信息”。有草稿时沿用放弃确认，取消保留编辑；刷新失败或后续项目读取失败也保留草稿及错误焦点，成功才重新读取选择并更新终端缓存。同步互斥防止重复请求，运行中禁用控件、页签和关闭。长按钮在窄屏换行，底部保存仍固定可见。浏览器默认不查询真实上游，其内存模拟支持相同别名选择；验收注入只位于自建浏览器上下文，没有产品调试入口。

已有 Rust 模块内补充别名排序、排除非 LTS 与预发布、损坏索引保留、重开离线缓存、精确版本缺失、显式覆盖、文件冲突及缓存刷新使旧快照失效的验证，未新增测试文件。相关 33 项检查全部通过，含短时隐藏 PowerShell 的真实 Node/Python/PHP 执行；Node 验收扩展至读取真实 process.release.lts 后验证 LTS 自动选择。另行执行官方联网用例，通过生产 catalog 流程获取 index.json，将显示上限设为 1 后仍可解析历史代号，再用独立 index.tab 核对最新 LTS，验证通过；不下载套件、不启动长期服务。

隔离浏览器验收缺少缓存、刷新失败及重试、双击仅一次请求、运行中 Escape 保护、放弃确认取消、失败保留草稿、固定保存及预览来源、恢复自动和刷新切换到新版本。发现新增刷新异常缺少规范 code 导致显示 [object Object]，按项目错误结构修复后重新确认中文提示及焦点。中英文、浅深色、1280 与 320×480 检查并目检截图；窄屏面板宽 296px、两侧 12px，没有横向溢出，英文刷新按钮可换行，下拉虚线两侧各 8px。页面异常为零，自建上下文已关闭，原页面保留。

版本统一至 0.2.75，Cargo.lock 只更新三个本项目 crate。最终前端类型检查通过；精确暂存内容导出的独立发布树通过 cargo check --workspace --all-targets --locked，pathenv/versions/envfile/install 回归 76 通过、0 失败、5 ignored、602 filtered out，同一发布树另行显式执行两项原生终端和一项官方索引验证，3 项均通过。未执行的其他模块与忽略项不计入验收。原有 configgen.rs 的 178 additions / 9 deletions 和本地生成文件不纳入提交。已核实 v0.2.74 Release completed/success。按根 AGENTS.md 新建 annotated tag v0.2.75，与 main 原子推送并核对实际 Release 状态。

本次没有数据库结构或数据修复 SQL 变更，未修改 update.sql；运行时缓存沿用既有 settings 存储。未启动前端 dev、未执行本地前端 build、未新增依赖。自定义 nvm 别名、多解释器、目录自动切换和 macOS 原生终端验收尚需后续推进，整体目标保持进行。

## 第九十轮：站点启动与 PHP 切换失败的服务回滚（v0.2.76）

沿现有站点生命周期继续补齐真实服务行为。fast-context 和调用链检查发现：创建/启动站点成功拉起 Web 后，后续步骤失败只恢复 vhost，可能把本次新启动的服务留在运行状态；运行中站点切换 PHP 后保存失败，也未收回新池。先扩展已有真实 Nginx 用例复现：遗留 Apache 配置失败后，Nginx 状态仍为 Running。

启动流程复用已有 SiteWebChanges，按本次尝试、成功应用及启动前状态恢复：收回新增实例、恢复原运行服务，只处理实际触及的服务。新增站点内部 PHP 启动入口，继续复用服务注册、忙状态检查、进程组和健康检查；池就绪后由站点事务统一应用受影响的 Web 配置，避免提前重载导致记录保存失败也干扰原 Web。套件页单独启动 PHP 仍沿用原来的 Web 同步行为。

站点更新和启动持有现有 lifecycle 锁，记录本次从空闲状态启动的 PHP 池。失败时先清理新池，再恢复 Web，避免恢复后的主配置带上失败操作的 upstream；已运行共享池和 Error 状态下原有活进程不被认领或终止。文件或站点记录恢复失败也继续尝试进程清理，沿用部分恢复错误与恢复副本，启动错误保留原错误码和详情。创建失败仍保留项目与数据库，不改变现有数据保留语义。

验证扩展位于已有 sites.rs 模块内，未新增测试文件。原生夹具使用临时目录、独立端口、真实 Nginx 和 PHP，跳过系统 hosts 写入；两个逻辑 PHP 版本引用同一真实 PHP 二进制，验证独立池切换，不冒充不同发行版兼容性验收。覆盖记录保存失败前无额外 Web 重载、部分记录恢复失败仍清理新池、创建和启动在后续 hosts 阶段失败、共享池及忙状态进程保留、正常切换与原池站点同时返回真实 PHP 响应，以及原先服务停止时创建失败能同时收回新 PHP/Web。所有隔离实例均通过清理守卫停止。

版本统一为 0.2.76，Cargo.lock 仅更新三个本项目 crate；界面只同步版本显示，未改布局。原有 configgen.rs 的 178 additions / 9 deletions 与未跟踪本地文件保留，不纳入提交。发布前以精确暂存内容导出的独立目录验证，排除本地原有改动影响。已核实 v0.2.75 Release completed/success；本轮按根 AGENTS.md 新建 annotated tag v0.2.76，与 main 原子推送并核对远程指向及实际构建状态。

最终前端类型检查通过；精确暂存内容导出的独立发布树通过 cargo check --workspace --all-targets --locked，sites/ops/services/hosts 回归 60 通过、0 失败、13 ignored、611 filtered out。同一发布树另行显式执行本轮两项真实 Nginx/PHP 验证，2 通过、0 失败；其余忽略项和未执行模块不计入验收。首次 PHP 夹具因共用目录的已有设置保护而未到达目标故障点，拆分项目目录后验证通过，并增加错误来源断言以保证确实覆盖记录保存失败。

本次没有数据库变更，未修改 update.sql；故障注入只作用于临时 SQLite 夹具。未启动前端 dev、未执行本地前端 build、未新增依赖。macOS 与真实 Apache 的服务回滚尚未实机验收，应用运行时进程托管等其他缺漏继续后续推进，整体目标保持进行。

## 第九十一轮：可选的站点应用进程托管（v0.2.77）

参考 ServBay 官方 Node.js 站点文档（https://support.servbay.com/basic-usage/websites/adding-nodejs-development-website）的应用运行与反向代理分工，保留用户在终端启动应用的既有方式。本轮新增明确开启的应用托管，支持 Node.js、Python、Temurin JDK 21 和 Go，默认关闭。历史 runtime.command/cwd 只保留数据，不因升级而执行。新配置沿用现有站点 runtime JSON，记录指定安装版本、独立参数和可选工作目录，没有新增数据库字段。

启动复用已安装套件入口校验、进程组和服务日志，以可执行文件绝对路径和独立 argv 启动，不拼接 shell 命令。指定版本前置到本次进程 PATH，并提供 PORT/HOST；Java 设置 JAVA_HOME，Python 开启无缓冲输出，不更改系统 PATH 或项目版本文件。托管应用只接受本机回环 HTTP 地址，拒绝路径、凭据、查询和片段；HTTPS 由 Web 服务处理。检查应用与 Web 端口冲突，最多等待 60 秒，并确认监听 PID 属于本次启动的进程或其子进程后才报告启动成功。依赖安装和项目构建仍由用户准备，不自动执行项目安装脚本。

应用服务以 site-app:<站点 ID> 注册，站点状态包含该依赖。启动或保存失败收回本次新启动的应用，保留原有运行实例；运行时、参数、目录和监听地址变更要求先停止应用。停止、批量停止和删除复用既有站点事务，Web 配置应用后再停止应用，失败尽可能恢复原记录、配置和运行服务，部分恢复失败明确报告。删除保留项目文件，卸载保护覆盖站点绑定的精确运行时版本；看门狗不会重启已禁用站点的应用。数据目录搬移同步重定位应用工作目录和独立路径参数，外部路径和普通文字保留。

新建站点向导增加四种应用语言及可选托管设置；详情增加应用页签、状态、日志入口和停止站点与应用操作。版本从已安装列表选择，参数逐项填写，桌面端支持文件及目录选择，工作目录收在可选区域。运行中锁定相关配置；停止时保留尚未保存的站点草稿。缺失运行时和无效参数显示错误并阻止保存，浏览器明确只模拟状态，不执行本机程序或访问项目文件。复用现有组件和样式，没有新依赖。

隔离浏览器验证缺失版本、空入口拦截、跨步骤保留参数、创建启停、运行中锁定、停止保留草稿和保存后再启动。检查中文浅色及英文深色、1360/390/320px 视口；修复新建弹窗无效 calc 宽度，并收紧窄屏页签间距。320px 面板宽 296px、两侧各 12px，英文页签实际宽度与滚动宽度一致，底部操作可见；截图已目检。本轮自建上下文已关闭，原有页面保留。

验证扩展位于已有 Rust 模块，未新增测试文件。真实 Nginx 配合 Node、Python 和 Go 的临时项目验证代理响应、字面参数、中文及空格目录、日志、重复启动、运行中编辑拒绝、端口冲突保留外部监听、入口失败清理、后续 hosts 失败回滚、批量记录失败保留原进程、正常批量停止、删除及创建失败清理。Windows 进程退出后曾出现首个 TCP 探测短暂成功，诊断确认 PID 已退出、监听表为空且后续探测失败，断言改为限时等待释放并同时核对监听表，未放过持续占用。Go 还验证派生监听进程被一并停止。安装元数据使用夹具版本，不代表验证了多个真实发行版本；所有隔离实例由清理守卫停止。

版本同步为 0.2.77，Cargo.lock 仅更新三个本项目 crate。精确暂存并导出独立发布树，排除原有 configgen.rs 的 178 additions / 9 deletions 与未跟踪本地文件。已确认 v0.2.76 Release completed/success；本轮仍须新建 annotated tag v0.2.77，与 main 原子推送并核对远程指向及实际 Release 状态。

最终 0.2.77 前端与 schema 类型检查通过。独立发布树通过 cargo check --workspace --all-targets --locked，sites/ops/services/bulk/install/paths 回归 120 通过、0 失败、15 ignored、551 filtered out，包含应用参数校验和数据目录路径重定位。另在同一发布树显式执行真实 Nginx/Node/Python/Go 生命周期用例，1 通过、0 失败；其余忽略项和未执行模块不计入验收。全仓 rustfmt 检查因既有格式差异未通过，没有全仓格式化；新增 applications.rs 单独格式化。

本次没有数据库变更，未修改 update.sql；故障注入只作用于临时 SQLite 夹具。未启动前端 dev、未执行本地前端 build。Java 与 macOS 应用启停尚未实机验收，Unix 异常退出后、可执行文件位于 runtimes 之外的孤儿子进程恢复仍受既有归属校验限制，不宣称完整恢复；其他功能缺漏继续后续推进，整体目标保持进行。

## 第九十二轮：统一服务自动恢复与重试上限（v0.2.78）

继续完善上一轮站点应用托管。通过 fast-context 追踪发现，服务列表经 CoreState 启动时登记看门狗，而站点及隐式依赖直接调用 ops，成功启动后未纳入监控；恢复收养的进程也存在同样缺口。另外，自动启动只要短暂成功就将计数清零，反复崩溃可以永远绕过五次上限。

看门狗现统一归 ServiceManager 所有，站点、批量、服务列表及收养入口共享同一份记录。普通启动确认有真实存活 PID 后登记，确实停止后取消运行意图，停止失败且仍有进程时保留原监控；卸载、删除站点、取消托管及创建失败清理同时移除记录。CoreState 及已有验证夹具改为引用共享管理器，不保留第二套看门狗。批量站点启停的桌面命令改为 spawn_blocking，并在工作线程持有数据目录活动守卫，避免等待应用就绪时阻塞界面及与目录搬移交错。

自动恢复使用独立启动入口，成功与失败均计入连续尝试次数，运行满 60 秒才清零。观察到运行转为退出后先退避，间隔为 2、4、8 秒递增并封顶 60 秒；重复观察不会延后既定时间。最后一次恢复仍在运行时不会误报达到上限，再次退出后停止尝试。重试恢复校验总开关、实际服务、运行状态、站点是否启用与是否耗尽额度，并通过生命周期锁防止并发交错；重置不会复活主动停止的服务。

设置页在总开关开启后展示受监控服务、运行或等待恢复状态、尝试次数和达到上限后的重试入口。补齐初次加载、空列表、读取失败与重新加载，中英文说明同步稳定运行窗口。开关持久化成功后才更新界面，操作互斥，重试成功提示已排队而非已恢复。浏览器仅演示正在运行的服务，不把停止的服务伪装成受监控状态，也不伪造自动恢复成功。schema 默认开关修正为 false，与 Rust 和浏览器默认值一致。

沿用 UI/UX 技能和项目组件，在已有服务上进行隔离浏览器检查。覆盖中文及英文、1360/390/320px、加载/失败/重新加载/空列表/耗尽状态及长名称；窄屏端口控件发现溢出后改为换行，内容区 320px 视口下 clientWidth 与 scrollWidth 均为 228px，390px 下均为 298px。截图已目检，自建上下文已关闭，用户原有页面保留。故障状态仅在隔离浏览器内注入，没有产品调试入口。

现有 Rust 模块内验证连续短暂成功仍耗尽、首次及后续退避、稳定窗口清零、手动停止后 reset 不恢复。扩展现有真实 Nginx/Node/Python/Go 生命周期用例：站点入口加入监控，实际终止 Node 进程后经 CoreState 看门狗恢复，第二次退出到达上限，再由重试入口恢复；正常停止与失败启动不再恢复，删除后监控条目清理。此用例揭示并修复了删除站点遗漏监控清理的路径，重新执行通过。没有新增测试文件、依赖或数据库结构。

版本统一为 0.2.78，Cargo.lock 仅更新三个本项目 crate。前端与 schema 类型检查通过。已确认 v0.2.77 三个平台 Release 构建全部 completed/success。精确暂存内容导出独立发布树，排除原有 configgen.rs 的 178 additions / 9 deletions 及未跟踪本地文件；按根 AGENTS.md 必须新建 annotated tag v0.2.78，与 main 原子推送并核对实际远程状态。

最终独立发布树通过 cargo check --workspace --all-targets --locked；watchdog/sites/ops/services/bulk/install/paths 回归为 135 通过、0 失败、15 ignored、537 filtered out。另在同一发布树显式执行真实 Nginx/Node/Python/Go 生命周期与崩溃恢复用例，1 通过、0 失败；其余忽略项和未执行模块不计入验收。11 个版本文件、三个 workspace crate 锁定版本及暂存范围逐项核对通过，git diff --check 通过，仅单独格式化 watchdog.rs。

本次没有数据库变更，未修改 update.sql。未启动前端 dev、未执行本地前端 build；Java/macOS 原生应用生命周期及 Unix 外部临时子进程的异常恢复限制仍需后续验收与完善，整体目标保持进行。

## 第九十三轮：进程身份校验、跨会话接管与恢复异常提示（v0.2.79）

延续站点应用托管与看门狗恢复，通过 fast-context 追踪进程登记、记录保存和接管链路。进程身份改为 PID、原生创建标识及可执行文件路径；Windows 使用 GetProcessTimes 的 FILETIME，Linux 使用启动 ticks 与 boot ID，macOS 使用 proc_pidinfo 的秒和微秒。启动和确认监听子进程时缓存身份，快照不重新认领同 PID 的后来进程；捕获时前后核对创建标识，读取不确定时保留记录，不据此结束进程。日志读取在身份登记前启动，快速退出仍保留诊断输出。

恢复记录升级到 formatVersion 2，保留旧 pids 字段，新增已核实身份、记录所属会话、实际运行版本、运行起点、端口、站点入口和管理台入口。pids.lock 串行化跨进程读、合并及写入，临时文件同步后原子发布；普通保存合并仍存活的其他会话和未能接管的记录，只淘汰已确认退出或 PID 复用的条目。损坏或较新格式保留原文件，避免覆盖后失去排查依据；合并时保留旧记录的时间边界。

接管先检查原所属会话是否仍存活。新格式允许恢复可执行文件位于 runtimes 外、但身份已经记录并核实的派生进程；旧格式继续要求 runtimes 内路径及不晚于原保存时间的创建时间，并推断已安装版本。接管后保留记录并转交所有权，修复仅执行一次 CLI 状态读取后记录消失、后续会话无法再接管的问题；恢复实际运行版本和已加载入口，不冒充当前默认版本。已删除服务只清理身份明确的进程，确认退出后才报告完成。原实例存活、未知身份和清理失败均保留并明确报告。

无法确认的注册服务进入恢复阻塞状态，暂停新启动、版本切换与卸载，避免重复启动或破坏仍运行的实例。当前会话已有明确身份的运行实例仍可停止；只有不明历史进程时不谎报已停止。重新检查确认旧进程消失或 PID 被复用后解除相应阻塞。恢复状态查询只读缓存，显式重新检查通过现有数据目录活动守卫和生命周期锁执行；桌面命令使用 spawn_blocking。

页面顶部增加按需出现的恢复提示，展示未解决事项、重新检查或重新加载及工具箱入口，补齐中英文与加载失败反馈。浏览器仅展示预览，不伪造本机进程恢复成功；操作互斥，完成后刷新服务、站点和看门狗。沿用 UI/UX 技能及既有组件，检查中文、英文和 1360/390/320px；窄屏发现的设置字体选择控件溢出同步修正。最终 320px 内容区 clientWidth/scrollWidth 均为 228px，提示框均为 203px，问题列表最大高度 144px。截图已目检，自建浏览器上下文已关闭，原有页面保留；故障注入没有写入产品调试入口。

验证扩展位于已有 Rust 模块内，没有新增测试文件。真实三会话交接覆盖启动会话退出、状态读取会话接管后退出、第三会话再次接管并停止，分别验证新格式外部可执行文件与旧格式 runtimes 内程序；同时核对运行版本、端口、入口及看门狗。另覆盖 PID 身份不符、活所属会话保护、损坏记录字节保留、恢复阻塞操作和快照不重新认领。Windows 夹具曾因分离子进程继承输出管道导致等待 EOF 延迟，现将会话输出写入临时文件并等待实际会话退出，保留存活断言与清理守卫。

版本统一为 0.2.79，11 个版本文件逐项核对，Cargo.lock 仅更新三个本项目 crate。精确暂存内容导出独立发布树，核对导出文件哈希并排除原有 configgen.rs 的 178 additions / 9 deletions 与未跟踪本地文件。已确认 v0.2.78 Release completed/success；按根 AGENTS.md 新建 annotated tag v0.2.79，与 main 原子推送并核对远程 SHA 和实际 Release 状态。

前端及 schema 类型检查通过。最终独立发布树通过 cargo check --workspace --all-targets --locked；services/ops/sites/bulk/install/paths/watchdog 回归为 140 通过、0 失败、15 ignored、537 filtered out。platform 在 aarch64-apple-darwin 与 x86_64-unknown-linux-gnu 上均通过编译检查。首次最终回归因 D 盘空间耗尽中断，将旧增量编译缓存保留到 E 盘后重新执行通过，没有修改源码来规避验证。

同一独立发布树另行显式执行真实 Nginx/Node/Python/Go 应用生命周期、崩溃恢复及失败回滚用例，1 通过、0 失败、691 filtered out，耗时 85.26 秒。仅使用临时项目和隔离端口，跳过 hosts 写入；其余忽略项及未执行模块不计入验收。git diff --check 通过，没有全仓格式化。

本次没有数据库变更，未修改 update.sql；没有新增依赖、前端 dev 或本地前端 build。Windows 原生交接已验证；Linux/macOS 本轮只编译平台代码，Java 和这些平台的完整应用生命周期仍未实机验收。恢复仅覆盖已经记录并核实的进程，启动成功落盘前崩溃、未记录后代及仍活着的另一实例的跨进程控制不宣称完整覆盖，整体目标保持进行。

## 第九十四轮：将进程身份校验贯穿停止动作与端口处理（v0.2.80）

继续参考 ServBay 服务管理及 servbayctl 的状态、停止与排障设计（https://support.servbay.com/basic-usage/command-line-tool-servbayctl）。通过 fast-context 追踪发现，上一轮快照和接管已经排除复用 PID，但停止兜底仍可使用旧组中的 PID，端口确认也仍以秒级启动时间判断身份。本轮把已经记录的原生身份传到实际终止动作，不在临终止时把变化后的 PID 当成原目标。

平台新增 VerifiedProcess：Windows 取得包含查询、终止和同步权限的进程句柄，在同一句柄读取 GetProcessTimes 创建标识并执行 TerminateProcess，通过 WaitForSingleObject 确认退出；Linux 使用 pidfd_open 固定对象、pidfd_send_signal 发送信号及 poll 确认退出；macOS 发送信号前再次核对原生创建标识。Context7 本次查询网络失败后，核对 Microsoft TerminateProcess 官方文档及 Linux pidfd_open 手册。没有增加依赖。Windows 的存活检查改为等待进程对象，读取权限错误保留为可能存活，不再冒充退出。

服务与端口共用已核实的进程树快照，逐项重新读取子进程的父 PID、创建标识和可执行文件；创建时间早于当前父进程的旧后代不认领，父子归属变化时不沿旧快照扩展目标。终止前固定可操作的对象，任何身份或权限不明均明确报错。服务停止先记录所发现后代的身份，以便部分终止失败后保留状态和重试对象；确认退出后清理对应记录，Linux 已退出但尚未回收的进程不再因 kill(pid, 0) 成功而被误报为运行中。

Windows 自有 Job 继续使用内核对象收回已归组进程；没有 Job 的接管实例使用已固定的进程对象，服务路径不再回退到按旧 PID 调用 taskkill。Unix 自有组终止前剔除创建标识变化的根进程，保留同一会话原有的组清理行为。停止后的存活判断也核对缓存身份。已删除服务的历史记录清理接入同一套已核实终止逻辑。本轮保持原有各服务优雅停机策略，不把端口释放等同于数据库业务已安全提交。

监听者模型新增 processStartMarker，保留旧 processStartedAt 供兼容读取，但操作必须携带本次扫描的原生创建标识。即使秒级时间、名称和命令行相同，创建标识变化也拒绝继续；缺失标识要求重新扫描。实际终止成功并确认退出后才纳入已处理数量，原目标已消失时不自动改为处理后来占用者。浏览器 mock 同步契约，仍明确只操作演示服务。

端口确认框失败后禁用原确认按钮，给出关闭提示、重新扫描并核对当前占用者的说明；保留取消和刷新路径。结果文案改为已确认结束数量，中英文同步。沿用 UI/UX 技能及 Next 本地文档，在隔离浏览器验证失败、禁止重复提交、重新扫描后成功、结果更新及空列表。检查 1360px 桌面与 320px 中英文界面，确认框宽 296px、左右各 12px、clientWidth 与 scrollWidth 相同，页面内容区均为 228px；截图已目检。故障注入只在隔离浏览器缓存中，自建上下文已关闭，原有 packages/sites 页面保留。

验证扩展放在现有 Rust 模块，未新增测试文件。真实 Windows 临时监听进程带一个有限寿命子进程，覆盖同秒旧标识拒绝、直接旧标识终止无动作、正确目标与子进程退出、另一个监听实例及其子进程保留。进一步把旧身份和真实运行实例放入同一服务、同时保留旧进程组根列表，验证停止仍只收回正确对象。补充 Windows/Linux/macOS 创建标识排序及不同 Linux boot ID 不互认。验证中发现 Windows 子进程已进入退出阶段时 TerminateProcess 可返回拒绝访问，改为等待既有句柄确认实际退出；超时或仍未退出仍报错。

版本统一为 0.2.80，11 个版本文件逐项核对，Cargo.lock 仅更新三个本项目 crate。精确暂存内容导出独立发布树并核对文件哈希，排除原有 configgen.rs 的 178 additions / 9 deletions 与未跟踪文件。最终前端/schema 类型检查通过；独立发布树通过 cargo check --workspace --all-targets --locked，ports/services/ops/sites/bulk/install/paths/watchdog 回归 151 通过、0 失败、15 ignored、527 filtered out。platform 在 aarch64-apple-darwin 与 x86_64-unknown-linux-gnu 上通过编译检查。只格式化本轮新增代码片段，没有全仓格式化。

同一独立发布树另行显式执行真实 Nginx/Node/Python/Go 应用生命周期、看门狗恢复和失败回滚用例，1 通过、0 失败、692 filtered out，耗时 86.30 秒。临时项目使用隔离端口并跳过 hosts 写入；其余忽略项和未执行模块不计入验收。最终 git diff --check 通过。

已确认 v0.2.79 Release completed/success；本轮按根 AGENTS.md 新建 annotated tag v0.2.80，与 main 原子推送并核对远程指向及实际构建状态。本次没有数据库变更，未修改 update.sql，未启动前端 dev 或执行本地前端 build。Linux/macOS 本轮未做实机终止验证；macOS 的信号调用与创建标识检查并非同一个原子系统调用。未记录且在快照之后新产生或脱离归属的后代不宣称完整覆盖；计划任务、隧道等其他组使用路径及协议级优雅停机仍需继续审查，整体目标保持进行。

## 第九十五轮：数据库停机失败保留实例与显式强制停止（v0.2.81）

参考 ServBay 的 servbayctl stop/kill 分离设计（https://support.servbay.com/basic-usage/command-line-tool-servbayctl），并核对 PostgreSQL pg_ctl 与 MySQL mysqladmin 官方文档。通过 fast-context 追踪服务停止、进程身份、实际运行版本、诊断与重启链路，发现 MySQL 和 PostgreSQL 的普通停止失败后仍会自动终止进程。本轮移除这两条失败兜底，让调用者收到错误并保留存活实例，重启在停止阶段失败时不会继续启动。

MySQL 先核对实际启动端口的本机监听者属于当前已记录实例，再使用私有客户端配置文件发送 mysqladmin shutdown，失败和超时不自动强杀。PostgreSQL 按运行中的版本定位 pg_ctl，检查该数据目录 postmaster.pid 与当前已核实身份一致，执行 fast 模式并等待退出；默认版本变化不会打向另一个实例。控制命令限时执行，错误输出限量读取并脱敏。Redis 等待退出也改为核实已记录身份，避免仅按 PID 存活误判。

新增服务停止预览和强制停止接口，以服务、版本、实际端口和已核实进程身份生成修订号。确认时在生命周期锁内重新核对，旧实例的确认不能用来停止后来启动的实例；恢复阻塞和身份不明时拒绝操作。强制停止复用已经核实的进程树终止与存活检查，保留 CoreDNS 还原保护、失败状态和 PID 保存，成功后取消看门狗运行意图。Tauri 命令使用 spawn_blocking。CLI 增加 nsbctl kill <service> --yes，缺少明确确认或目标不唯一时返回用法错误。

服务诊断增加次要危险操作，必须显示当前服务、版本和 PID 并单独确认，说明数据库写入和未保存数据风险；不受跳过普通确认设置影响。补齐加载、读取失败、实例变化、停止失败、重新读取、无运行进程及处理中禁止重复提交和关闭。成功刷新服务、站点、看门狗、诊断及恢复状态。卡片和列表在 Error 但仍有 PID 时保持开关开启，用户可以重试停止；列表补充同一诊断入口。中英文同步，浏览器明确只演示状态变化，运行代次参与演示修订号。

沿用 UI/UX 技能和既有组件，在隔离浏览器验收取消保留实例、确认后停止、错误禁用旧确认、重新读取恢复、处理中 Escape 不关闭且仅提交一次、空进程禁用确认，以及卡片与列表的 Error+PID 状态。检查中文及英文、1360/390/320px；320px 确认框宽 296px、左右各 12px，clientWidth 与 scrollWidth 相同，页面内容区均为 228px；390px 确认框宽 366px。截图已目检，隔离上下文已关闭，用户原有 packages/sites 页面保留。故障注入仅在隔离浏览器缓存，没有新增产品调试入口。

版本统一为 0.2.81，11 个版本文件同步，Cargo.lock 仅更新三个本项目 crate。精确暂存内容导出独立发布树，核对 22 个发布文件的索引哈希，configgen.rs 保持 HEAD 内容，排除用户原有 178 additions / 9 deletions 及未跟踪本地文件。前端/schema 类型检查与独立发布树 cargo check --workspace --all-targets --locked 通过；ports/services/ops/sites/bulk/install/paths/watchdog 回归 153 通过、0 失败、16 ignored、527 filtered out。新增断言放在已有 Rust 模块，没有新增测试文件，只格式化新增代码片段。

同一独立发布树显式运行真实 MySQL 8.0.46、PostgreSQL 16.6 及 Nginx/Node/Python/Go 应用生命周期验证，3 通过、0 失败、693 filtered out，耗时 146.69 秒。数据库仅使用临时目录与隔离端口，覆盖错误密码保留实例、缺失控制客户端、错误 pidfile、运行版本与默认版本不同、旧确认拒绝、强制停止及重新启动后可查询；应用回归覆盖实际访问、看门狗恢复和失败回滚。其余 ignored 项与未执行模块不计入验收。独立树 CLI 编译通过，在临时 NSB_HOME 验证缺少 --yes、缺少服务及多目标均退出 2，未知服务退出 1，帮助包含新命令。

已确认 v0.2.80 Release completed/success；按根 AGENTS.md 新建 annotated tag v0.2.81，与 main 原子推送，核对远程提交、tag 及实际 Release 状态，不覆盖旧 tag。本次没有数据库结构或现有用户数据变更，未修改 update.sql；没有新增依赖、前端 dev 或本地前端 build。Windows 原生停机已验证，Linux/macOS 未做本轮实机验收。MongoDB 和其他通用服务的普通停止策略未在本轮更改，不宣称所有服务都已完成协议级优雅停机；整体完善目标继续进行。

## 第九十六轮：MongoDB 正常停机与重启数据保留（v0.2.82）

继续参考 ServBay 的 stop/kill 分离行为，通过 fast-context 追踪剩余数据库停止路径，发现 MongoDB 普通停止仍直接终止进程。本轮改为请求数据库正常退出，失败或超时保留实例与错误，继续通过上一轮服务诊断中的独立强制停止入口处理。查询 Context7 并核对 MongoDB 进程管理文档、v8.0 及 master 的 signal_win32.cpp / signal_handlers.cpp：Windows 上游线程监听 Global\Mongo_<pid> 事件后执行正常退出，Linux/macOS 支持 SIGTERM。

平台新增针对 MongoDB 的正常停机请求。Windows 固定已核实的进程句柄，从句柄取得 PID，只打开既有事件并设置事件，不创建事件、不调用 TerminateProcess；打开和发送前后检查原进程是否已退出，事件缺失或权限失败返回错误。Linux 使用已核实 pidfd 发送 SIGTERM；macOS 在发送 SIGTERM 前重新核对创建标识。原强制停止实现保持独立，没有把正常停机超时升级为强杀。

核心停机按服务当前运行版本定位安装记录，核对已记录可执行文件与该版本的 mongod 一致，再固定所有目标并发送正常退出请求，等待已记录进程消失后才报告停止。修改默认版本或端口配置不会改变正在操作的实例。失败仍保留 PID，重启在停止阶段失败时不会再启动。继续复用生命周期锁、恢复阻塞、PID 文件保存与看门狗主动停止处理，没有增加另一套服务状态。

验证代码位于已有 Rust 源文件，没有新增测试文件。Windows 真实有限寿命子进程验证旧创建标识拒绝、事件缺失报错且进程保留、只设置事件不会强杀、不存在事件不被重新创建、原进程退出后正常返回。核心验证错误可执行文件不会收到信号，stop/restart 均保留原 PID 与 Error 状态。Unix 增加正常退出信号处理验证，本机只做交叉编译，不冒充实机运行。

真实 MongoDB 验收使用官方 8.0.4 包并核对清单 SHA256，只将数据写入临时目录、监听隔离端口。通过独立验证目录里的官方 Node.js 驱动写入和读回文档，覆盖错误可执行文件拒绝后实例仍可查询、默认版本和端口变化不误操作、正常停止、旧强制确认拒绝、两次重新启动及三次正常关闭。日志要求包含 mongod shutdown complete 与 exitCode 0，Windows 还要求记录 shutdown event signaled，并确认没有 unclean shutdown。首次验证错误地要求看门狗列表为空，核对源码后改为检查主动停止意图以及开启看门狗后不再拉起；历史条目保留是既有设计，没有为满足断言更改产品行为。验证用驱动只安装在 E 盘隔离目录，没有增加产品依赖或改动 pnpm-lock.yaml。

11 个版本文件同步到 0.2.82，Cargo.lock 只更新三个本项目 crate。按暂存索引导出独立发布树并核对 13 个代码/版本文件哈希，排除用户原有 configgen.rs 的 178 additions / 9 deletions 和未跟踪本地文件。D 盘空间不足前，确认没有 cargo/rustc 活动后，将三份早期 nsb_core 增量编译缓存保留式转存到 E:/CodexCacheBackup/nice_env-v0.2.82，没有删除用户文件。仅格式化新增片段，没有全仓格式化。

最终独立发布树通过 cargo check --workspace --all-targets --locked；核心相关回归 154 通过、0 失败、17 ignored、527 filtered out，平台回归 13 通过、0 失败。独立树真实 MongoDB 文档保留用例另行显式执行，1 通过、0 失败、697 filtered out，耗时 7.20 秒；其余 ignored 项及未运行模块不计入验收。platform 的 aarch64-apple-darwin 与 x86_64-unknown-linux-gnu all-targets 编译检查通过，前端/schema 类型检查通过，git diff --check 通过。本轮没有新增界面，继续复用已验收的服务停止错误反馈和强制停止确认，不重复进行无变更的浏览器验收。

已确认 v0.2.81 Release completed/success；按根 AGENTS.md 创建新 annotated tag v0.2.82，与 main 原子推送并核对远程指向及实际 workflow 状态。没有修改用户现有数据库结构或数据，未修改 update.sql；未启动前端 dev、未执行本地前端 build。Windows 原生 MongoDB 8.0.4 已验收；macOS/Linux 的本轮信号代码只通过编译检查，macOS 的创建标识核对与 kill 仍不是同一原子系统调用。MariaDB 和其他清单驱动服务的普通停机策略尚需继续完善，整体目标保持进行。

参考：https://www.mongodb.com/docs/manual/tutorial/manage-mongodb-processes/；https://github.com/mongodb/mongo/blob/v8.0/src/mongo/util/signal_win32.cpp；https://github.com/mongodb/mongo/blob/v8.0/src/mongo/util/signal_handlers.cpp。

## 第九十七轮：MariaDB 初始化、管理、备份导入与正常停机（v0.2.83）

沿用 ServBay 的实例管理和 stop/kill 分离思路，通过 fast-context 追踪清单、安装快照、通用服务、数据库客户端、备份导入和界面。核对 MariaDB 官方 mariadb-install-db.exe 文档及 11.4.8 原生帮助，确认原先 --service=MariaDB 会注册 Windows 系统服务。四个内置版本移除该参数和多余初始化子目录；仅对完全匹配旧默认描述的安装快照升级，不覆盖自定义运行描述。

MariaDB 初始化在临时目录限时执行，成功后才替换空目标目录；不注册系统服务，密码不经命令行传递。新实例使用 data/mariadb-versions/<version>，与旧共享目录 data/mariadb 分开，避免把新实例嵌入旧数据库目录。旧 my.ini 指向共享目录时保留原位置，并用版本记录或唯一的历史启动日志核对原版本；未知、混合版本、非空残缺目录和自定义路径均明确报错，不自动升级或覆盖数据。首次认证后设置随机 root 密码；既有密码失效时保留实例供用户修复连接。启动端口、数据目录、basedir 和本机监听地址按托管实例约束。

新增可选数据库引擎参数，默认 MySQL 以兼容现有调用。MySQL/MariaDB 的凭据、缓存、实例选择和备份文件名按引擎及版本隔离。复用现有建库、账号授权、改密、备份恢复及外部导入界面和 SQL 客户端，不新增依赖。MariaDB 优先使用 mariadb/mariadb-dump/mariadb-admin，排除 MySQL 专有导出选项；显式定位包内 lib/plugin，解决便携客户端连接 MySQL 8 时找不到 caching_sha2_password 插件的问题。

数据库连接前核对版本、实际端口、托管监听进程以及 @@datadir。MariaDB 普通停止使用实际运行版本的 admin 客户端发送 shutdown，超时或认证失败保留进程与错误，不转为强杀；原有独立强制确认入口保持可用。MySQL/MariaDB 在 Error 且原进程存活时仍可验证并修正本机连接密码，成功后恢复运行状态。

界面沿用 UI/UX skill 与项目现有组件，选择器显示引擎、版本、端口和状态，操作弹窗显示目标实例。浏览器演示数据同样按引擎隔离。1360px 桌面、390px/320px 窄屏检查了中英文、MariaDB 建库与账号、导出、实例切换和错误密码；确认没有横向溢出，错误后保留输入，导入目标和覆盖确认可见。浏览器验收为既有 localhost 演示环境；真实数据库能力另由 Rust 原生调用验收。

独立发布目录排除了原有 configgen.rs 的 178 additions / 9 deletions 和未跟踪文件。最终 pnpm check、cargo check --workspace --all-targets --locked 通过。相关 Rust 库内回归串行执行 145 通过、0 失败、24 ignored、532 filtered out；并发执行曾发生端口互相占用，另修正一个既有用例对相邻临时端口可用性的错误假设，没有削弱产品端口检查。Windows 真实 MariaDB 11.4.8 与 MySQL 8.0.46 的独立验收两项均通过；最终调整数据目录后再次执行 MariaDB 用例通过，覆盖无系统服务副作用、密码隔离、建库授权、备份恢复、MySQL 到 MariaDB 导入、停机失败保留进程和阻止重启、凭据恢复、正常关闭、重启读回以及版本不符拒绝启动。

没有新增测试文件，验证代码扩展于已有 Rust 模块。没有操作用户业务数据库，未修改 update.sql。未启动前端 dev，未执行本地前端 build。因 D 盘空间不足造成首次链接调试文件失败，确认无 Rust 编译进程后，将旧 nsb_core 增量缓存保留式转存 E:/CodexCacheBackup/nice_env-v0.2.83，没有删除用户文件。

11 个版本文件同步到 0.2.83，Cargo.lock 仅更新三个本项目 crate。已确认 v0.2.82 Release completed/success，本轮按根 AGENTS.md 创建新 annotated tag v0.2.83，并与 main 原子推送、核对远程指向及实际 workflow 状态。MariaDB 原生验收范围是 Windows 11.4.8；自定义目录与无法确认版本的旧数据需先从原环境导出后导入，尚未增加原地跨大版本升级；其他服务缺口继续推进，整体目标保持进行。

参考：https://mariadb.com/docs/server/server-management/install-and-upgrade-mariadb/installing-mariadb/installing-system-tables-mariadb-install-db/mariadb-install-db-exe

## 第九十八轮：PostgreSQL 安全初始化与连接密码管理（v0.2.84）

通过 fast-context 追踪 PostgreSQL 启动、数据路径、数据库管理、Tauri 命令与前端预览，并核对 PostgreSQL 官方 initdb、pgpass、psql、ALTER ROLE 文档。修复原先直接在最终目录使用 trust 初始化、读目录错误被当作空目录、初始化无超时及失败后递归删除数据目录的问题。

首次初始化改为私有临时密码文件、32 位随机密码及 SCRAM-SHA-256，本机凭据先持久保存。初始化在同级临时目录限时执行，通过 PG_VERSION、base 与 pg_control 校验后才替换空目标目录。既有数据先核对主版本和完整性，残缺、非空未知目录和跨主版本启动明确拒绝。修复 Unix socket 参数缺失 -c，并创建对应目录。正常停机继续使用此前的 pg_ctl 身份校验与 fast 停机流程。

端口遵循现有自动回落设置，并保存实际端口。真实验收发现系统临时端口范围内的主动 TCP 探测会与服务绑定产生竞争，改为先限时等待可核实的 PostgreSQL 监听进程，再发送凭据。连接同时核对当前版本、实际端口、进程身份及 SHOW data_directory；旧密码失效时保留运行实例，允许在数据库页恢复本机凭据。数据库归属错误补充目标端口与扫描 PID 详情。

新增 PostgreSQL 连接信息、按需查看保存密码及修改/同步密码接口。psql 禁用启动配置，清除继承的 PG 环境变量，使用私有 pgpass、匿名输入文件和有限执行时间；冒号、反斜杠、引号与 Unicode 密码已实测。认证检测要求正确凭据成功、随机错误密码出现明确的密码认证拒绝，再次正确连接成功；网络故障或无法识别的本地化认证错误不会冒充密码验证成功。新凭据按版本隔离，并排除配置导入、导出，避免覆盖另一台机器的实例密码。

旧默认 trust 实例不能通过“验证现有密码”保存任意输入。改密时可明确选择同时启用本机认证，仅转换可识别的 initdb 默认本机规则，预先拒绝自定义规则与自定义 hba 文件。配置写入前检查原内容，保留备份，重载后核对正确/错误密码行为；部分成功时说明账号、配置与备份状态。改密 SQL 通过文件输入并关闭本会话的常规语句记录，错误详情脱敏；不承诺第三方审计插件不会记录 SQL。

数据库页增加独立 PostgreSQL 卡片，展示版本、实际端口、非模板库数量、大小、认证状态与可复制连接串；复用现有组件提供密码弹窗、加载、重试、错误后保留输入、实例变化保护与防重复提交。浏览器演示按版本隔离，仅模拟操作。1360px 桌面、390px 中文及 320px 英文验收了连接、错误密码、查看密码、同步和改密。修复英文窄屏按钮挤出边距，改为窄屏纵向排列；最终弹窗宽度分别 366px、296px，页面与弹窗无横向溢出，截图已目检，独立验收浏览器已关闭。

精确发布树排除用户原有 configgen.rs 的 178 additions / 9 deletions 及未跟踪文件，11 个版本文件同步到 0.2.84，Cargo.lock 仅更新三个项目 crate。pnpm check 与独立树 cargo check --workspace --all-targets --locked 通过；相关回归串行执行 61 通过、0 失败、11 ignored、630 filtered。最终 PostgreSQL 与配置导入导出回归 7 通过、0 失败、695 filtered。PostgreSQL 原生验收在 Windows PostgreSQL 16.6 的隔离目录和端口执行，覆盖初始化失败保留目录、错误主版本拒绝、随机初始密码、特殊字符密码、旧 trust 转换、自定义规则保护、凭据恢复、实际端口隔离、自动回落、错误 PID 停机保护以及多次重启后读回原数据。

未新增测试文件，验证扩展于既有 Rust 模块；未启动前端 dev，未执行本地前端 build。没有业务数据库变更，未修改 update.sql。v0.2.83 Release 已确认 completed/success。本轮继续按根 AGENTS.md 新建 annotated tag v0.2.84 并与 main 原子推送；安装包完成情况以远程 workflow 实际状态为准。PostgreSQL 建库、用户授权、备份恢复与外部导入仍待后续扩展，本轮仅完成初始化和连接密码管理，整体目标保持进行；Linux/macOS 未做本轮原生实机验收。

参考：https://www.postgresql.org/docs/current/app-initdb.html 、https://www.postgresql.org/docs/current/libpq-pgpass.html 、https://www.postgresql.org/docs/current/app-psql.html 、https://www.postgresql.org/docs/current/sql-alterrole.html


## 第九十九轮：PostgreSQL 数据库与普通账号管理（v0.2.85）

参考 ServBay 的数据库和账号管理入口，通过 fast-context 追踪现有数据库页、Tauri 命令和 PostgreSQL 客户端；核对 PostgreSQL 官方 CREATE DATABASE / DROP ROLE 文档。沿用 PostgreSQL 独立 API，不将其套入 MySQL/MariaDB SQL 方言，未引入新依赖。

新增数据库与账号列表，以及创建数据库、删除数据库、创建普通登录账号、账号改密和删除账号共七个命令。每次操作持有现有数据目录与生命周期锁，连接前核对运行版本、实际端口、监听进程与数据目录。数据库列表包含所有者、编码、大小、系统/模板保护及连接状态；账号列表包含登录能力、超级用户和额外权限、拥有的数据库。修复原生 PostgreSQL 将 OID 输出为 JSON 字符串导致列表无法反序列化的问题，查询中显式转换为 bigint。

创建项目账号时关闭超级用户、创建数据库、创建角色、复制及绕过行级安全权限。建库从可登录账号中选择所有者，使用 template0 和 UTF8；同名对象不复用、不覆盖原密码或权限。新名字限制字母、数字、下划线并拒绝系统保留名，已有外部对象通过安全引用支持空格和引号。删除与改密核对列表 OID，拒绝对象已删除重建后的旧请求；此校验不承诺与外部 SQL 并发重命名/替换形成绝对原子操作。保护 postgres、模板库与超级用户，不自动终止活动连接，不执行 DROP OWNED，依赖由 PostgreSQL 拒绝后提示手动处理。密码限制按 UTF-8 字节计算，继续通过私有凭据文件与匿名输入文件传递、错误脱敏，并关闭当前会话的常规与抽样语句日志；第三方审计插件不在此保证范围。

数据库页以 MySQL / MariaDB 与 PostgreSQL 分页签呈现。PostgreSQL 增加数据库和账号管理卡片、独立搜索、每页 10 条分页，以及短表单和完整名称删除确认。所有者从现有账号中选择，错误后保留输入，处理中防重复提交，实例版本/端口/PID 改变时禁止继续提交。MySQL 弹窗与备份选择期间也锁定页签，保留原有管理界面。沿用现有组件和 UI/UX skill，列表分隔线为带卡片内边距的虚线，窄屏表单按钮纵向排列。说明账号属于整个 PostgreSQL 实例，数据库所有者可以建表，但其他库的现有 PUBLIC 授权仍可能允许访问；不声称普通账号只可访问自己的库。

使用既有 localhost 演示环境检查了 1360px 桌面、390px 中文与 320px 英文，覆盖建库选所有者、创建与修改账号、同名错误保留输入、依赖删除拒绝、完整名称确认、先删库再删账号、搜索空结果、12 个账号分页与长账号名。320px 英文表单宽度与 scrollWidth 均为 296px，页面没有横向溢出；截图已目检。MySQL 创建弹窗、备份选择及页签锁定已检查。演示数据仅模拟 UI 行为，真实数据库能力依据隔离原生验收；只关闭本轮 QA 浏览器上下文，保留用户原有页面。

11 个版本文件同步到 0.2.85，Cargo.lock 只更新三个项目 crate。独立发布树排除用户原有 configgen.rs 的 178 additions / 9 deletions 与未跟踪文件，19 个代码/版本文件经换行正规化后逐项核对。pnpm check 和 cargo check --workspace --all-targets --locked 通过；数据库管理、备份、导入及配置传输相关回归 22 通过、0 失败、680 filtered out。验证扩展于已有 Rust 模块，未新增测试文件。原生验收曾发现客户端被结束后服务端查询仍在执行，改为等待验收连接正常结束再检验删除，没有削弱产品对活动连接的保护。

最终独立发布树 PostgreSQL 16.6 原生验收 1 通过、0 失败、701 filtered out，耗时 65.74 秒。覆盖普通账号真实登录、建表与读写、无法创建超级角色、改密后旧密码失效、系统对象保护、同名对象不覆盖、错误 OID/删除重建后的旧 OID 拒绝、有依赖的账号拒绝删除、活动查询保持连接并拒绝删库、查询正常退出后删库再删账号成功、带空格和引号的外部库名安全删除，以及上轮初始化、trust 转换、停机、重启数据保留和端口回落场景。

没有业务数据库变更，未修改 update.sql；临时 PostgreSQL 数据库仅在隔离目录与端口验收。未启动前端 dev，未执行本地前端 build。v0.2.84 Release 已确认 completed/success。本轮按根 AGENTS.md 新增 annotated tag v0.2.85，并与 main 原子推送；安装包状态以远程 workflow 实际结果为准。PostgreSQL 对象授权编辑、所有权转移、备份恢复与外部导入尚待后续扩展，整体目标保持进行。Windows PostgreSQL 16.6 为本轮原生验收平台，Linux/macOS 未做本轮实机验收。

参考：https://support.servbay.com/database-management/getting-started/postgresql-management-and-usage 、https://support.servbay.com/database-management/management/using-adminer-to-manage-database 、https://www.postgresql.org/docs/current/sql-createdatabase.html 、https://www.postgresql.org/docs/current/sql-droprole.html


## 第一百轮：PostgreSQL 原生备份与恢复到新数据库（v0.2.86）

继续参考 ServBay 的 PostgreSQL 备份目录、归档列表及 pg_dump / pg_restore 导入流程，使用 fast-context 定位现有数据库备份模块、Tauri 命令、进度事件及页面组件，查询 PostgreSQL 官方 custom archive 与单事务恢复文档。沿用 UI/UX skill 和现有组件，不新增运行依赖。

新增 PostgreSQL 独立备份目录 backup/postgresql，使用所选安装内的 pg_dump 导出 custom 格式 .dump。导出前核对运行版本、真实监听进程、数据目录和所选数据库 OID，排除系统库与不可连接的库。备份使用临时文件，成功且同步后才无覆盖发布；失败不留下可误认为成功的归档。进度显示实际写出的字节，独立事件携带 operationId，避免其它操作的事件混入。目录与文件名沿用受控路径校验，拒绝路径穿越、Windows ADS、软链接和目录联接；删除仅接受本目录内的 .dump 文件名。

支持从本机备份列表或文件选择器选取可信 custom 归档，选择新库名称及现有所有者。先读取到私有临时快照，校验 PGDMP 标记并通过 pg_restore --list 预检，再创建 UTF-8 新库；预检与恢复使用同一快照。恢复启用单事务与遇错退出，将对象归给所选所有者，不恢复源账号、ACL 或表空间位置。同名库拒绝创建。恢复失败时保留新建库并明确提示检查与重试方法，不自动删除可能已有外部活动的库；预检失败则不建库。归档可能含可执行数据库代码，界面需明确确认来源可信，不能把单事务或普通所有者当成任意恶意归档的隔离保证。

复用私有 pgpass、环境隔离和有限执行时间。真实 Windows 验收发现 psql 的命令行 SQL 会被本地编码转换，验收查询改为 UTF-8 文件输入；客户端库名使用逐字节编码的 ASCII URI，兼顾中文和包含等号/引号的外部库名，避免 conninfo 注入。恢复角色通过编码后的连接选项传递，绕开 Windows --role 的本地编码，并处理空格和反斜杠；已核对中文且含空格的所有者及恢复对象实际归属。密码仍不出现在命令行，失败详情脱敏。

数据库页增加 PostgreSQL 备份卡片、搜索、每页 10 条分页、大小与时间、打开目录、导出、恢复和删除。短表单显示实际版本及端口，所有者使用下拉选择，禁止同名新库、未确认来源与实例变化后的提交；处理中防重复并保持弹窗，关联页签锁定。中英文文案明确 custom 格式、单库范围、账号和外部文件不包含在内，以及扩展/版本兼容限制。列表使用带内边距的虚线，窄屏按钮纵向排列，错误信息保留在表单中。

浏览器使用既有 localhost 演示环境，覆盖未安装/未运行、导出、恢复后库与所有者列表更新、同名与可信来源校验、处理中不能关闭、搜索空结果、11 份备份分页、删除最后一页后页码回落及原数据库保留。1360px 桌面、390px 中文和 320px 英文已截图目检；恢复弹窗宽度分别适配为 366px / 296px，clientWidth 与 scrollWidth 一致，操作按钮保持可见。另核对了窄屏原生复选框的选中状态、尺寸与渲染。仅关闭本轮 QA 上下文，保留用户原有页面。浏览器仅验证演示交互，真实数据库行为以原生验收为准。

11 个版本文件同步到 0.2.86，Cargo.lock 仅更新三个本项目 crate。独立发布树核对 21 个代码/版本文件，排除原有 configgen.rs 的 178 additions / 9 deletions 以及用户未跟踪文件。独立目录 pnpm check 与 cargo check --workspace --all-targets --locked 通过。未新增测试文件，验证扩展于既有 Rust 模块。

最终独立发布树数据库管理、备份、导入、配置传输与 PostgreSQL 原生验收共 23 通过、0 失败、679 filtered out，耗时 86.98 秒。真实归档覆盖表、索引、序列、Unicode 文本、bytea 与大对象，以及带中文/空格路径的外部文件导入、中文所有者、包含等号/引号/中文的外部库名。损坏归档与未确认来源不建库，同名库不覆盖；普通所有者恢复含事件触发器的归档时权限失败，已确认新库保留且事务内创建的表回滚。备份删除不影响源库，MySQL SQL 列表不混入 PostgreSQL 归档；同时通过既有初始化、密码、停机和重启数据保留场景。

没有业务数据库变更，未修改 update.sql；原生数据库操作仅使用隔离临时目录与端口。未启动前端 dev，未执行本地前端 build。v0.2.85 Release 已确认 completed/success，继续按根 AGENTS.md 新增 annotated tag v0.2.86，并与 main 原子推送。安装包完成状态以远程 workflow 实际结果为准。

本轮提供 custom 归档导出、文件导入和恢复到新库；原地覆盖恢复、实例账号全局备份、定时 PostgreSQL 备份、远程实例直接迁移及细粒度授权编辑仍需后续完善，整体目标保持进行。Windows PostgreSQL 16.6 为原生验收平台，Linux/macOS 未做本轮实机验收。

参考：https://support.servbay.com/database-management/getting-started/postgresql-management-and-usage 、https://support.servbay.com/database-management/getting-started/import-data-from-existing-postgresql 、https://support.servbay.com/getting-started/backup-and-restore 、https://www.postgresql.org/docs/current/app-pgdump.html 、https://www.postgresql.org/docs/current/app-pgrestore.html


## 第一百零一轮：PostgreSQL 恢复并替换已有数据库（v0.2.87）

沿用 ServBay 的备份恢复入口，使用 fast-context 追踪数据库客户端、备份、Tauri 命令和界面；核对 PostgreSQL ALTER DATABASE 官方文档及 REL_16_STABLE 的 RenameDatabase 实现。新增恢复到已有数据库，避免逐表覆盖后混入目标库旧表。没有新增运行依赖。

替换前要求可信归档、输入完整数据库名称、目标 OID 匹配和有效所有者，保护系统库与模板库。目标有活动连接或预备事务时拒绝，不强制断连；含逻辑复制订阅或数据库复制槽时拒绝自动替换，避免数据库内部标识改变后损坏复制关系。先自动导出恢复前 custom 归档，再将输入归档恢复到随机命名的独立暂存库，沿用文件快照、PGDMP 和 pg_restore --list 预检及单事务恢复。恢复失败保留现场、原库和恢复前归档，不自动删库。

恢复成功后在同一事务中将原库改为 niceenv_previous_ 随机名称，再把暂存库改为目标名称。每次 RENAME 取得数据库排他锁后核对 OID、可连接状态和逻辑复制关系；任一重命名或核对失败均回滚。复用标识符引用函数，DO 块使用双层 E-string 转义，库名中的引号、中文或美元分隔符不能截断代码块。提交附近断连后重新读取目录，核实原名称和保留名称对应的实际 OID；无法确认结果时明确提示检查现场，不把客户端报错一律解释为回滚。成功返回原名称、保留库名称及恢复前归档路径。

UI 沿用已读的 UI/UX skill 与项目组件，恢复方式默认创建新库；替换模式选择业务库、所有者并完整输入目标名称，切换目标重置确认。原所有者不可登录时要求重新选择，不静默换成其它账号。显示持续结果提示，保留库名称和备份路径可以查看；处理中锁定表单、关闭和页签，失败保留输入。中英文说明名称保留但内部标识改变、新库使用 UTF-8 默认配置、ACL/表空间/数据库级设置需核对，以及额外磁盘空间需求。归档必须可信，普通所有者和单事务不是恶意数据库代码的安全隔离。

浏览器在既有 localhost 演示环境完成默认新库模式、完整名称校验、切换目标清空确认、替换成功后原库与恢复前备份可见、再次恢复到新库、忙时关闭与重复提交保护。1360px 桌面、390px 中文与 320px 英文截图已目检；320px 恢复弹窗宽度与 scrollWidth 均为 296px，正文可滚动、操作区固定。修复窄屏搜索框被刷新按钮挤压，改为上下排列；刷新验收页后输入宽度从 33px 变为 163px。结果提示的英文标签按词换行，长路径单独断行。只关闭本轮 QA 上下文，保留用户原有 packages/sites 页面。浏览器仅验交互，真实行为另由原生验收证明。

11 个版本文件同步到 0.2.87，Cargo.lock 仅更新三个本项目 crate。独立发布树排除原有 configgen.rs 的 178 additions / 9 deletions 与用户未跟踪文件；原配置文件按换行正规化后与 HEAD 完全一致。pnpm check 和 cargo check --workspace --all-targets --locked 均通过。最终独立发布树数据库管理、备份、迁移、配置传输及 PostgreSQL 原生验收 23 通过、0 失败、679 filtered out，耗时 144.01 秒；验证扩展于已有 Rust 模块，未新增测试文件。

Windows PostgreSQL 16.6 原生验收覆盖完整名称、可信来源、错误 OID 和系统库保护；禁用且不连接外部服务的逻辑订阅会阻止替换；含事件触发器的归档在普通所有者下失败，目标 OID 与原数据保持。成功替换后读回归档值 84，旧表不会混入新库；保留原库读回 87，恢复前归档再次导入也读回 87。第二次重命名后的 OID 校验失败会回滚第一次改名，保留名称冲突拒绝，活动查询未被强制结束，连接自然退出后切换成功。含中文、引号和美元分隔符的库名切换通过，并继续通过此前初始化、认证、备份和停机回归。

本次没有业务数据库变更，未修改 update.sql；原生验收仅用隔离临时目录和端口。未启动前端 dev，未执行本地前端 build。v0.2.86 Release 已确认 completed/success；本轮按根 AGENTS.md 创建新 annotated tag v0.2.87，与 main 原子推送并核对远程指向和实际构建状态。Linux/macOS 未做原生实机验收；数据库级配置与复制关系需手动规划，实例全局账号备份、定时 PostgreSQL 备份及细粒度授权编辑仍需后续完善，整体目标保持进行。

参考：https://support.servbay.com/getting-started/backup-and-restore 、https://www.postgresql.org/docs/current/sql-alterdatabase.html 、https://raw.githubusercontent.com/postgres/postgres/REL_16_STABLE/src/backend/commands/dbcommands.c


## 第一百零二轮：PostgreSQL 原生自动备份计划与保留策略（v0.2.88）

核对 ServBay 官方备份与恢复文档，确认其自动备份支持每天、每周、每月及运行中的 PostgreSQL 分类。通过 fast-context 追踪 NiceEnv 现有配置备份、通用 cron、数据库导出、Tauri 启动交接和后台任务收尾。现有配置自动备份仅导出应用配置，通用 cron 仅执行 shell 命令；本轮在既有 backup_job 模块加入原生 PostgreSQL 调度，复用私有凭据、实例校验和 custom 归档，不要求用户填写命令、密码或 JSON。

每个安装版本独立保存启用状态、频率、本地时间、每周日期、每月日期和保留份数，默认关闭，使用既有 SQLite settings 保存完整记录，不新增表。每月日期超过实际天数时在月末执行，日期计算覆盖普通月份、闰年和星期。桌面启动交接完成后每 30 秒检查一次；应用关闭时不执行，重开后到期计划补执行一次，失败按下一日历周期再试。计划仅针对选定版本，不自动启动实例；运行前核对当前运行版本、进程、端口和数据目录。运行锁由操作系统持有，锁文件存在本身不代表任务存活；保存与执行共用版本锁，多窗口不能重复执行或在运行中改计划。

每次备份所有可连接的业务数据库，排除系统库、模板库与不可连接的库。归档复用 pg_dump custom、临时文件和无覆盖发布，输出 auto-postgresql-版本-OID-库名-时间戳.dump。成功生成新文件后，按数据库 OID 和版本保留 1–100 份，0 为全部保留；仅清理符合自动文件名及 PGDMP 标记的普通文件，手动导出、恢复前备份、其他版本/数据库、名称相似的其它文件、目录和链接不参与轮转。时钟回调或旧文件时间戳更大时仍保留刚生成的归档。删除失败保留新归档并报告部分完成，导出/认证失败不会触发清理。

开始执行前保存本次时间与下次日期，每完成一份归档就保存文件名。结果区显示最近状态、下次执行、保留数量及本次文件列表，区分成功、部分完成、失败、空库跳过和中断。进程退出后遗留 running 状态会根据运行锁核实为中断，已生成归档保留，允许立即执行验证。自动运行与立即执行共用原生入口，持有后台任务保护和数据目录活动锁，退出或迁移先等待正在写入的备份收尾。普通配置导入导出排除本机计划及状态，不会把另一台机器的自动操作带入本机；完整数据目录迁移仍保留相对文件名记录。

沿用 UI/UX skill 和现有组件，在 PostgreSQL 备份卡片加入计划摘要及短设置弹窗。频率、星期、日期使用下拉，时间使用时间输入，禁用计划后相关时间字段禁用；保留数量支持明确的全部保留说明。表单保存与立即执行防重复，错误保留草稿，结果返回后立即更新查询缓存，避免刚保存又打开读到旧值。下次执行和最近结果自动刷新，归档仍使用已有恢复/删除入口。使用带内边距的虚线分区，窄屏单列，长正文滚动且操作区固定。

浏览器在既有 localhost 演示环境验收默认关闭、每月 31 日与月末提示、每周 Sunday、重新打开后草稿值保持、超限 101 禁止保存、0 全部保留、禁用后下次时间为空、空库跳过与立即执行防重复。保留 1 份时两次执行只留下最新自动归档，手动归档仍在；演示仅验证交互，真实行为通过下述原生验收。1360px 桌面、390px 中文、320px 英文截图已目检，弹窗宽度与 scrollWidth 分别为 366px/296px，没有横向溢出；计划摘要和长英文说明可换行，按钮可达。只关闭本轮 QA 上下文，保留用户原有页面。

11 个版本文件同步到 0.2.88，Cargo.lock 仅更新三个本项目 crate。独立发布树排除原有 configgen.rs 的 178 additions / 9 deletions 和用户未跟踪文件，19 个代码/版本文件核对一致。pnpm check 与 cargo check --workspace --all-targets --locked 通过；最后的界面缓存更新再次通过类型检查。未新增测试文件，验证扩展于已有 Rust 模块。

最终独立发布树的自动备份、数据库管理、备份恢复、数据库导入、配置传输及 PostgreSQL 原生验收共 28 通过、0 失败、675 filtered out，耗时 150.04 秒。Windows PostgreSQL 16.6 验证真实 custom 归档生成、尚未到期不运行、到期检查函数触发一次后更新下次时间、手动执行与调度共用路径、关闭计划、按版本/OID 轮转和手动归档保留。错误保存密码时记录失败且旧归档保留；旧文件被 Windows 句柄占用时保留新归档并报告部分完成，释放后可正常清理。自动归档恢复为独立数据库并读回值 84。原有初始化、认证、建库账号、替换回滚、活动连接保护及停机回归继续通过。库内校验另覆盖月末、闰年、每周日期、非法参数、运行锁、中断状态、损坏记录与配置传输隔离。

没有业务数据库结构或数据变更，未修改 update.sql；计划复用既有本机设置存储，原生数据库验收仅使用隔离临时目录和端口。未启动前端 dev，未执行本地前端 build。v0.2.87 Release 已确认 completed/success，本轮按根 AGENTS.md 新增 annotated tag v0.2.88，与 main 原子推送，远程构建状态以实际 workflow 为准。Windows PostgreSQL 16.6 为本轮原生验收平台；Linux/macOS 未做实机验收。跨数据库一致快照、实例全局账号备份、远程备份目标及 MySQL/MariaDB 原生计划仍需后续完善，整体目标保持进行。

参考：https://support.servbay.com/getting-started/backup-and-restore

## 第一百零三轮：MySQL / MariaDB 原生自动备份与共享计划界面（v0.2.89）

继续参考 ServBay 的按分类自动备份，通过 fast-context 追踪现有 PostgreSQL 计划、MySQL/MariaDB 认证客户端、SQL 导出恢复与 Tauri 交接。复用已有计划设置、日历、操作系统锁、中断检测和数据目录活动保护，将原生自动备份接入 MySQL 与 MariaDB；按引擎和安装版本独立保存，本机凭据不写入计划。保留 PostgreSQL 的存储 key、锁文件名与命令兼容，调度仍仅由桌面启动交接放行，CLI/MCP 的只读调用不启动计划。

支持每日、每周、每月、本地执行时间、短月月末、0 全保留或 1–100 份保留；应用打开时检查，到期补执行一次，实例未运行或凭据失效记录失败，不自动启动实例。手动立即执行与到期调度共用入口，持有版本计划锁和数据库生命周期锁，校验目标版本、进程、端口、实际数据目录后再使用安装包中的 mysql/mariadb 客户端。逐库备份全部业务库，排除系统库；使用已有事务快照导出，不包含实例账号，不承诺跨库或非事务表一致性。

SQL 自动文件名包含引擎、版本、原始数据库名的完整 SHA-256 摘要、截短可读名称及唯一时间戳。摘要避免字符清洗和文件系统大小写折叠合并不同数据库，复用已有 sha2/hex，不新增依赖。导出成功且非空后在临时 SQL 尾部写入归属标记，sync 后无覆盖发布，保留 MariaDB 开头的 sandbox 指令。仅在新归档成功后清理同引擎、版本、库摘要、合法文件名且尾部标记匹配的普通文件；手动备份、恢复前备份、链接和无标记文件不参与清理。始终保留本次新文件，旧文件占用导致清理失败时返回部分完成；凭据或导出失败不清理旧备份。

每生成一份立即记录相对文件名，保存下次时间与最近执行结果，支持成功、失败、部分完成、空库跳过及中断反馈。MySQL/MariaDB 计划和执行状态排除于普通配置导入导出，避免导入另一台机器的自动操作；不新增表或 migration，继续使用本机 SQLite settings。

沿用 UI/UX skill、Next 本地 use-client 文档与现有组件，将 PostgreSQL 计划表单抽为 DatabaseBackupPlan，由三种引擎共用。查询缓存按引擎和版本隔离，保存返回立即更新缓存，计划完成后刷新归档列表。编辑和运行锁与原有手动备份/恢复合并，避免执行中切换实例或重复提交。设置弹窗显示目标实例，频率和日期下拉、时间输入、份数边界校验、禁用状态、错误草稿均保留；虚线分区位于卡片内边距内，窄屏正文滚动、操作区固定。

既有 localhost 浏览器演示验收 MySQL 每月 31 日、保留 1 份、保存后重新打开、两次运行轮转、执行时实例切换锁定；MariaDB 默认关闭且不继承 MySQL 的设置，101 禁止保存、0 全保留、每周星期日、空库跳过、创建业务库后生成归档、禁用后下次时间为空，切回 MySQL 设置保持。共享组件的 PostgreSQL 保存与执行也通过回归。1360px 英文页面、390px 中文弹窗、320px 英文弹窗截图已目检；窄屏弹窗宽度与 scrollWidth 分别同为 366px/296px，无横向溢出，底部操作可见。浏览器仅证明交互，原生能力另行验证；只关闭本轮 QA 上下文，保留用户原有 packages/sites 页面。

11 个版本文件同步到 0.2.89，Cargo.lock 仅改三个本项目 crate。独立发布树包含 21 个代码/版本文件，排除用户原有 configgen.rs 的 178 additions / 9 deletions 和未跟踪文件，按换行正规化核对该文件与 HEAD 一致。独立 pnpm check 与 cargo check --workspace --all-targets --locked 通过。最终自动备份、数据库管理、备份恢复、导入、配置传输及三个原生验收合计 30 通过、0 失败、673 filtered out，耗时 254.09 秒；只扩展现有 Rust 模块内验证，未新增测试文件。

Windows 原生 MySQL 8.0.46、MariaDB 11.4.8 验证尚未到期不执行、到期 tick 触发一次且更新下次时间、实际 SQL 文件和尾部标记、错误保存密码时旧归档保留、Windows 句柄占用时返回 partial 并保留新文件、解除占用后轮转、手动和恢复前备份不受影响及禁用计划。自动归档恢复后分别读回 MySQL 的 42 和 MariaDB 的 original。原有 MySQL 双实例导入/认证、MariaDB 隔离和正常停机回归通过；PostgreSQL 16.6 原生备份恢复、替换和停机回归继续通过。库内校验覆盖日历、引擎锁/状态隔离、损坏记录、不同原始名称摘要隔离、全保留、未来时间戳旧文件与无标记文件保护、配置传输隔离。

本次没有用户业务数据库结构或数据变更，未修改 update.sql；所有原生验收使用隔离临时数据目录和端口。未启动前端 dev，未执行本地前端 build。已确认 v0.2.88 Release completed/success；本轮按根 AGENTS.md 新增 annotated tag v0.2.89，与 main 原子推送并核对远程指向，远程安装包构建按实际状态报告。Linux/macOS 未做实机验收；实例全局账号、远程备份目标、跨库与非事务表一致性备份等能力仍待后续完善，整体目标保持进行。

参考：https://support.servbay.com/getting-started/backup-and-restore

## 第一百零四轮：MySQL / MariaDB 按库账号权限编辑与授权范围修复（v0.2.90）

参考 ServBay 官方 MySQL 管理文档关于业务账号和按需授权的流程，通过 fast-context 追踪账号列表、建号授权、Tauri 命令、前端数据库页与原生验证。既有界面仅能创建全库权限账号，不能调整已有权限；原生建号对含下划线的库名直接执行 GRANT，可能把下划线解释为通配符。本轮修复新增账号的授权范围，并补齐直接数据库级权限读取与编辑；不复制第三方文档中无必要的 FLUSH PRIVILEGES，使用服务端 GRANT/REVOKE 并读回核对。

新增授权读取服务器 partial_revokes 模式：普通模式对反斜杠、下划线和百分号转义，使用具体数据库名；字面模式按实际库名授权。既有通配范围保留原始值并明确标注，不能被当作单个数据库悄悄收缩或扩大。原生验收确认 MySQL 切换 partial_revokes 后，原有反斜杠转义范围也按字面处理，可能不再覆盖原库；读取按真实服务器语义显示，模式变化会使旧编辑版本失效，界面提示检查历史范围，不自动改写旧授权。

账号列表使用 HEX 编码读取用户名和来源主机，避免特殊字符破坏批量响应。授权读取先确认账号存在，查询 mysql.db 的实际列结构，只暴露已支持的数据库权限和转授权状态，额外服务器权限按原样保留；不读取认证字符串或密码哈希，不使用可能包含认证内容的 SHOW GRANTS 作为界面数据。用户名与来源主机按精确字节定位，SQL 标识符和账号分别引用，权限名必须来自服务端支持列表。内置 root、系统账号、匿名账号、角色及匹配系统库的范围不可在此修改；全局权限与 partial_revokes 组合只读，避免 REVOKE 改变全局权限限制。

保存只对当前账号、来源主机及授权范围计算增减项，保留其它库、表、存储过程、角色与来源主机授权。支持数据查询/新增/修改/删除、表和视图、索引、例程、事件、触发器、服务器支持的 DELETE HISTORY，以及 GRANT OPTION。使用既有实例身份校验、私有 root 凭据、数据目录活动保护和生命周期锁；保存额外持有按引擎/版本隔离的操作系统文件锁。提交前比较授权快照 revision，保存后重新读取核对。MariaDB/MySQL 5.7 的授权语句启用 NO_AUTO_CREATE_USER，避免 GRANT 隐式创建消失的账号。多条授权语句不承诺整体事务回滚，失败时说明可能部分生效并要求重新读取；外部管理员绕过应用锁的 SQL 操作仍可能并发发生。

沿用 UI/UX skill 和 Next 本地文档，复用现有 Sheet 构建宽侧栏。账号和主机来自列表，数据库或已有范围使用下拉选择；常用查询/读写预设与按操作分组的勾选项配合，高级权限和转授权可展开。减少权限需要明确确认，系统范围只读，错误保留当前选择并提供重新读取。界面解释这是直接授权，并非账号最终有效权限，全局、角色、其它主机和对象级授权仍可能生效。保存反馈补充已有应用连接可能需要重连，符合 MySQL 对数据库级权限生效时机的文档。编辑和保存期间锁定实例切换，处理中阻止关闭和重复提交。

浏览器演示验收 MySQL 历史通配授权提示、减少权限确认、保存后重新打开、新增具体数据库范围，以及 root 全局权限提示和只读状态。MariaDB 新建账号的含下划线库名显示为具体范围，按 localhost 保存不影响 127.0.0.1 账号；清空可编辑权限后重新打开无勾选，保存中取消按钮禁用、Escape 不关闭。1360px 英文侧栏、390px 中文和 320px 英文窄屏截图已目检；窄屏宽度和 scrollWidth 同为 366px/296px，正文滚动、底部按钮可见，虚线分区保留左右内边距。浏览器只证明交互，真实权限另由原生账号连接验证。只关闭本轮 QA 上下文，用户原有 packages/sites 页面保持。

11 个版本文件同步到 0.2.90，Cargo.lock 仅更新三个本项目 crate。独立发布树包含 18 个代码/版本文件及本轮 DESIGN.md，共 19 个发布文件，排除原有 configgen.rs 的 178 additions / 9 deletions 和未跟踪文件，并核对该文件与 HEAD 的换行正规化内容一致。pnpm check 与 cargo check --workspace --all-targets --locked 已通过。验证扩展于既有 Rust 模块，没有新增测试文件、依赖或 migration。

最终独立发布树的自动备份、数据库管理、备份恢复、导入、配置传输及 MySQL/MariaDB 原生回归共 29 通过、0 失败、674 filtered out，耗时 261.92 秒。真实账号验证含下划线库名不授予相似库、localhost 与 127.0.0.1 分离、其它库授权保留、旧 revision 和非法权限拒绝、GRANT OPTION 开关、SELECT 允许而 INSERT 拒绝、全部撤销后查询拒绝，以及带引号用户名和反引号库名的授权查询。MariaDB 撤销全部权限后可能保留空 mysql.db 行，验证以权限为空且真实查询拒绝为准；MySQL 切换 partial_revokes 后旧转义范围按字面解释，验证历史账号查询拒绝、显示字面范围、旧 revision 拒绝及新字面范围授权可编辑。系统范围和全局权限保护、既有认证、备份、恢复、导入与正常停机回归均通过。

本次没有用户业务数据库结构或数据变更，未修改 update.sql；原生验证只使用隔离临时实例、账号、数据库和端口。未启动前端 dev，未执行本地前端 build。v0.2.89 Release 已确认 completed/success。本轮按根 AGENTS.md 使用新 annotated tag v0.2.90 并与 main 原子推送，发布结果及远程构建状态在完成验证后核对。Windows MySQL 8.0.46、MariaDB 11.4.8 是本轮原生验收范围；其它版本及 Linux/macOS 尚未实机验证，角色/全局/表/列/例程级完整权限管理仍待后续完善，整体目标保持进行。

参考：https://support.servbay.com/database-management/getting-started/mysql-management-and-usage 、https://dev.mysql.com/doc/refman/8.0/en/grant.html 、https://dev.mysql.com/doc/refman/8.0/en/privilege-changes.html 、https://mariadb.com/docs/server/reference/sql-statements/account-management-sql-statements/grant

## 第一百零五轮：PostgreSQL 账号登录控制与连接数限制（v0.2.91）

参考 ServBay 的 PostgreSQL 账号管理与访问限制流程，通过 fast-context 追踪既有角色列表、改密、Tauri 命令和隔离原生验收；通过 Context7 核对 PostgreSQL 当前角色属性文档。既有界面只能创建、改密和删除账号，本轮补齐暂停/恢复新登录以及普通连接数上限，使账号暂停不必删除账号或其数据库。角色属性作用于整个实例，既有数据库所有权、密码、成员关系和权限保持原状。

复用 pg_roles 读取 OID、名称、登录状态、连接数上限和保护状态，pg_stat_activity 只统计当前普通客户端连接，不向界面返回 SQL 内容或密码哈希。活动连接数是读取时的观察值，不参与编辑 revision；revision 包含角色属性与设置，外部修改会使旧表单失效。不限对应 -1，自定义支持 0–2147483647，0 阻止新的普通连接。系统账号、pg_ 角色和超级用户由服务端保护；界面可查看系统账号状态但不能修改。降低上限或暂停登录时前后端均要求确认。

保存沿用实例版本、运行进程和数据目录校验，持有后台任务保护与生命周期锁。单次短事务先锁定 pg_authid 的 SHARE ROW EXCLUSIVE 锁，与普通 ALTER/DROP/CREATE ROLE 的目录写锁冲突，在锁内重新校验 OID、名称、受保护属性和 revision，再执行 ALTER ROLE LOGIN/NOLOGIN CONNECTION LIMIT 并读回核对后提交。系统目录只用于加锁，不直接更新目录行；标识符引用及 DO 块字符串分层转义，特殊字符不能截断语句。沿用锁等待与语句超时，失败保留输入并要求重新读取；提交附近连接中断时提示核对实际状态，不一律声称已回滚。

沿用 UI/UX skill、Next 本地 use-client 文档与既有 Dialog、Select 和原生复选框。账号行显示连接数上限，长名称可换行；弹窗使用两个选择项和条件数字输入，当前连接数与限制说明就近展示。明确暂停登录不终止现有连接、不阻止其他账号通过成员关系使用权限；普通连接上限不约束复制连接或超级用户，不限仍受实例总连接数约束，PostgreSQL 对并发尝试按近似上限执行。忙时禁用关闭、重复保存与实例切换；失败保留选择，重新读取明确重置草稿。修复保存后立即重开时短暂显示旧缓存，成功返回立即写入查询缓存并刷新列表。

浏览器在既有 localhost 演示环境完成账号创建、限制为 0、确认前不可保存、超出整数范围拒绝、重新读取重置草稿、暂停/恢复登录、保存后立即重开回显、系统账号只读，以及保存中取消禁用和 Escape 不关闭。1360px 中文桌面、390px 中文和 320px 英文弹窗截图已目检。窄屏弹窗左右各 12px，宽度与 scrollWidth 分别同为 366px/296px；正文独立滚动、底部保存按钮可见，分区使用左右有内边距的虚线。浏览器仅验证交互；真实连接行为由原生验证覆盖。只关闭本轮 QA 上下文，保留用户原有 packages/sites 页面。

11 个版本文件同步到 0.2.91，Cargo.lock 仅更新三个本项目 crate。独立发布树包含 17 个代码/版本文件及本轮 DESIGN.md，共 18 个发布文件，排除用户原有 configgen.rs 的 178 additions / 9 deletions 与本地生成文件。独立 pnpm check 与 cargo check --workspace --all-targets --locked 通过；最后的缓存更新再次通过类型检查。未新增依赖、测试文件或 migration，验证扩展于已有 Rust 模块。

最终独立发布树自动备份、数据库管理、备份恢复、导入、配置传输及 PostgreSQL 原生回归共 28 通过、0 失败、675 filtered out，耗时 162.72 秒。Windows PostgreSQL 16.6 真实账号验证连接上限 1 时第二个连接被拒绝，NOLOGIN 阻止新登录，连接上限 0 阻止新的普通连接，原有连接仍能读取 project_proof 的 84；恢复不限后新连接成功。未确认的限制不会写入，旧 revision、错误 OID、系统账号和负数非法上限被拒绝；外部 CREATEDB 修改使旧表单失效，失败不覆盖外部设置。含引号、美元标记和中文的角色可正确保存；同名角色删除重建后旧 OID 被拒绝。密码、普通权限和数据库所有权保留，既有备份、恢复、替换、认证与正常停机回归继续通过。

本次没有用户业务数据库结构或数据变更，未修改 update.sql；仅复制已存在 PostgreSQL 16.6 的程序文件用于隔离临时实例，未连接原环境的业务数据库。未启动前端 dev，未执行本地前端 build。已确认 v0.2.90 Release completed/success；本轮新增 annotated tag v0.2.91，与 main 原子推送并核对实际 workflow 状态。Linux/macOS 和其它 PostgreSQL 版本尚未本轮实机验证；账号成员关系、对象授权、密码过期策略与活跃会话管理仍需后续补齐，整体目标保持进行。

参考：https://support.servbay.com/database-management/getting-started/postgresql-management-and-usage 、https://www.postgresql.org/docs/current/role-attributes.html 、https://www.postgresql.org/docs/current/sql-createrole.html

## 第一百零六轮：MySQL / MariaDB 业务账号改密与认证方式保护（v0.2.92）

参考 ServBay 的 MySQL 业务账号管理流程，通过 fast-context 追踪数据库账号列表、原生客户端、实例身份核对与 Tauri 命令。原有界面支持创建账号及 root 密码管理，但不能修改普通业务账号密码。本轮按选定实例、用户名和来源主机提供独立改密入口，同名账号的其它来源不受影响，root、匿名及系统账号沿用保护规则。没有新增运行依赖。

新增认证信息读取与保存命令。认证原文不离开数据库，服务器内部计算认证指纹，再由后端结合进程随机密钥、服务器版本和账号元数据生成不透明 revision；前端不接收服务器原始指纹，避免用返回值直接离线猜测密码。MariaDB 按 auth_or 顺序读取每项插件，保留空对象代表主插件的占位，不能用会跳过占位的通配 JSON 路径。保存前重新读取并比较 revision，拒绝旧表单；持有引擎与版本专属的操作系统文件锁、后台任务和数据目录活动保护，并通过现有数据库生命周期锁核对实例身份。外部管理员直接执行的账号操作不受本机锁约束，读取与写入之间仍存在外部并发窗口，不承诺服务器级比较并交换。

使用 SET PASSWORD 保留现有认证插件和数据库授权。MySQL 支持 mysql_native_password、caching_sha2_password、sha256_password，已有备用密码及其它认证因素保持；MariaDB 支持以 mysql_native_password、mysql_old_password 或 ed25519 开头的认证链，仅修改首个密码插件，后续认证方式保留。MariaDB 按目标插件选择 old_passwords，避免 native 与 old 插件被隐式互换。真实 MariaDB 11.4.8 验证发现 named_pipe 在前、native 在后的组合会返回 1699；源码显示外部插件错误与后续密码更新可能同时发生，因此外部或未知插件在前的组合在执行 SQL 前拒绝，界面明确说明暂不支持，不删除或替换认证方式。

密码限制为 1–4096 UTF-8 字节且不含控制字符。SQL 使用临时 stdin，当前短管理会话设置 NO_BACKSLASH_ESCAPES 并关闭通用日志；密码不进入命令参数、环境变量或本机连接记录，保存输入不派生 Debug/Serialize。返回的客户端错误对新密码及 SQL 转义形式脱敏。执行后读回认证元数据，插件变化或读取失败时要求确认实际状态；连接中断不一律解释为修改失败，避免盲目重复提交。

沿用 UI/UX skill、Next 本地 use-client 文档和项目 Dialog、Input、Button 组件。弹窗显示实例、用户名、来源主机及认证方式，提供新密码、重复确认和显示密码；多认证或备用密码明确提示旧凭据可能仍有效。说明已有连接不会强制断开，项目与连接池需要更新密码，账号锁定和其它认证规则继续生效。保存防重入、处理中禁用关闭及实例切换；失败保留输入，重新读取认证信息后再试；成功更新查询缓存并清空输入。分隔线位于左右内边距内，使用虚线。

浏览器在既有 localhost 演示环境验证 MySQL 与 MariaDB 独立账号和来源主机、密码不一致及超出字节限制、显示隐藏、root 只读、保存中 Cancel 与 Escape 保护、关闭后输入清空和成功反馈。1360px 中文桌面、390px 中文及 320px 英文截图已目检；窄屏弹窗宽度与 scrollWidth 同为 366px/296px，正文可滚动、底部操作可见，320px 英文保存成功。演示仅验证交互，真实行为由隔离原生实例验证。只关闭本轮 QA 上下文，保留用户原有 packages/sites 页面。

11 个版本文件同步到 0.2.92，Cargo.lock 仅更新三个本项目 crate。独立发布树排除用户原有 configgen.rs 的 178 additions / 9 deletions 及本地生成文件，17 个代码与版本文件逐个核对一致，独立 configgen.rs 与 HEAD 一致；加入本记录后共 18 个发布文件。修复后独立 pnpm check 与 cargo check --workspace --all-targets --locked 均通过。最终自动备份、数据库管理、备份恢复、导入、配置传输及 MySQL/MariaDB 原生回归共 29 通过、0 失败、674 filtered out，耗时 298.13 秒。验证扩展于已有 Rust 模块，未新增测试文件或 migration。

Windows MySQL 8.0.46 与 MariaDB 11.4.8 原生验收覆盖含引号、反斜杠、分号和中文的新密码登录成功、旧密码拒绝、其它来源主机与授权保留、旧 revision 拒绝、系统账号保护及通用日志未包含明文新密码。MySQL 备用密码在改密后仍可用，原主密码失效；显式清除备用密码后登录被拒绝。MariaDB 单一 ed25519 和 ed25519/native 组合改密通过，备用 native 密码保留；named_pipe 单插件及 named_pipe/native 组合在写入前拒绝，后者 revision 与原密码保持；native/named_pipe 顺序改密成功且外部插件保留。

本次没有用户业务数据库结构或数据变更，未修改 update.sql；原生验收仅使用隔离临时目录、端口及账号。未启动前端 dev，未执行本地前端 build。已确认 v0.2.91 Release completed/success；本轮按根 AGENTS.md 新增 annotated tag v0.2.92，与 main 原子推送并核对远程指向及实际 workflow 状态。Linux/macOS、其它引擎版本及第三方认证插件未做本轮实机验收；完整账号生命周期、密码过期策略、活动连接和认证链编辑仍需后续完善，整体目标保持进行。

参考：https://support.servbay.com/database-management/getting-started/mysql-management-and-usage 、https://dev.mysql.com/doc/refman/8.0/en/set-password.html 、https://mariadb.com/docs/server/reference/sql-statements/account-management-sql-statements/set-password 、https://raw.githubusercontent.com/MariaDB/server/11.4/sql/sql_acl.cc

## 第一百零七轮：MySQL / MariaDB 账号删除与依赖预检（v0.2.93）

继续参考 ServBay 官方 MySQL 管理文档中的业务账号和可视化管理流程，通过 fast-context 对比 NiceEnv 的 PostgreSQL 与 MySQL/MariaDB 账号生命周期。PostgreSQL 已有删除入口，MySQL/MariaDB 仍只能创建、授权和改密；本轮补齐按用户名与来源主机精确删除普通业务账号。核对 MySQL 8.0 与 MariaDB 官方 DROP USER 文档：删除会移除账号授权，但不会关闭已有连接或自动删除业务对象，MariaDB 对多账号失败也不保证整体回滚，因此每次仅操作一个明确账号。

新增删除预检和执行命令，复用已有实例身份、客户端私有凭据、后台任务、数据目录活动保护及数据库生命周期锁。删除执行另持有引擎与版本独立的操作系统文件锁。先列出 mysql 系统表并逐表确认结构，对账号、数据库级、表级、列级、存储程序、动态权限、默认角色、角色关系与代理授权计算服务器内 SHA-256 快照；二进制字段先 HEX，保留 NULL，认证原文不离开数据库。后端再用进程随机密钥、客户端路径、端口及服务器版本生成不透明 revision，避免把原始认证指纹交给前端；连接数不参与 revision，正常连接变化不会误判账号已被修改。

预检列出以此账号为 DEFINER 的视图、存储程序、触发器和事件，并检查其它账号对它的角色及代理授权依赖。有依赖时禁止删除，展示前 50 个对象并注明还有更多，不自动转移定义者、撤销关联或删除数据。root、匿名、系统账号以及直接具有 SUPER、CREATE USER、SYSTEM_USER 或 ROLE_ADMIN 权限的账号受保护，未展开角色继承后的有效管理权限。要求当前管理连接直接具备全局 SELECT、SHOW VIEW、TRIGGER、EVENT，且没有部分撤销限制；权限不足时返回明确错误，避免把元数据不可见误判为没有依赖。

提交要求完整输入用户名@来源主机，后端重复校验确认内容、保护状态、依赖及 revision，再执行单账号 DROP USER。当前短会话仅追加 NO_BACKSLASH_ESCAPES，保留其它 sql_mode，包括 MariaDB Oracle 模式的原有规则；不使用 IF EXISTS、FORCE 或 KILL。执行后查询精确账号是否仍存在：断连报错后若确认账号已不存在，可返回已完成；无法读回时提示刷新确认实际状态，不自动重试。外部管理员操作不受本机锁约束，预检和 DROP 之间仍有外部并发窗口，不承诺服务器级原子依赖预检或唯一账号世代标识。

沿用已读 UI/UX skill、Next use-client 文档和现有 Dialog、Input、Button。账号行新增带完整可访问名称的删除入口，弹窗展示实例、账号、不可撤销范围、依赖列表及同用户名连接总数；PROCESSLIST 的来源地址不等于授权 Host，连接数明确包含其它来源。说明数据库和表数据保留、其它来源账号不受影响，已有连接可能保留原权限直到退出，提示先停用相关连接池。依赖或保护状态下不展示删除提交按钮；确认区使用左右留白的虚线，正文独立滚动、底部按钮固定。处理期间禁止关闭和切换实例，失败保留确认内容；重新检查成功后清空确认，防止沿用旧确认。成功立即更新账号列表缓存并刷新相关详情。

浏览器在既有 localhost 演示环境验证完整账号确认、只填用户名拒绝、root 保护、忙时取消与 Escape 保护、账号从列表移除及数据库保留。创建同名 localhost 和 127.0.0.1 两个来源后，分别删除且另一来源保持。仅在本轮 QA 上下文临时注入旧 revision，验证后端演示拒绝、输入保留与重新检查后重新确认；临时依赖快照验证长对象名、四种依赖类别、角色和代理计数、更多对象提示及删除按钮隐藏，未向代码添加调试入口。1360px 中文桌面、390px 中文、320px 英文及窄屏依赖状态截图均已目检；弹窗左右各 12px，宽度与 scrollWidth 分别同为 366px/296px，长内容可滚动、操作可达。只关闭本轮 QA 上下文，保留用户原有 packages/sites 页面。

11 个版本文件同步到 0.2.93，Cargo.lock 仅更新三个本项目 crate。独立发布树排除用户原有 configgen.rs 的 178 additions / 9 deletions 与本地生成文件，17 个代码和版本文件逐一核对一致；加入本记录后共 18 个发布文件。pnpm check 与最终独立 cargo check --workspace --all-targets --locked 通过。最终自动备份、数据库管理、备份恢复、导入、配置传输及 MySQL/MariaDB 原生回归共 29 通过、0 失败、674 filtered out，耗时 534.97 秒。只扩展既有 Rust 模块中的原生验收，未新增测试文件、依赖或 migration。

Windows MySQL 8.0.46、MariaDB 11.4.8 隔离原生验收覆盖含引号、分号和中文的账号、精确来源、完整确认、root 与临时授予 CREATE USER 的账号保护、外部表级授权使旧 revision 失效，以及四类 DEFINER 对象和代理依赖阻止删除；MySQL 另验证账号被授予他人作为角色时拒绝删除。删除后新连接被拒绝，已有连接仍能读取值 93；同名其它来源账号的快照和业务表的值 93 保持，原账号的表级授权被移除。临时管理账号仅用于隔离实例内撤销和恢复原生管理连接的 SHOW VIEW 权限，以确认元数据权限不足时预检与删除均拒绝，恢复后四类对象重新可见。

本次没有用户业务数据库结构或数据变更，未修改 update.sql；所有原生 SQL 仅作用于隔离临时目录、端口和验收账号。未启动前端 dev，未执行本地前端 build。v0.2.92 Release 已确认 completed/success；本轮按根 AGENTS.md 新增 annotated tag v0.2.93，与 main 原子推送并核对远程指向和实际构建状态。Linux/macOS、其它数据库版本未做本轮实机验收；账号改名、暂停登录、完整角色管理和活动连接管理等仍待完善，整体目标保持进行。

参考：https://support.servbay.com/database-management/getting-started/mysql-management-and-usage 、https://dev.mysql.com/doc/refman/8.0/en/drop-user.html 、https://dev.mysql.com/doc/refman/8.0/en/stored-objects-security.html 、https://mariadb.com/docs/server/reference/sql-statements/account-management-sql-statements/drop-user

## 第一百零八轮：站点文件 ZIP 备份与恢复副本（v0.2.94）

参考 ServBay 将网站文件与配置、数据库分别备份的流程，补齐 NiceEnv 过去只能导出站点配置、不能备份项目代码和上传文件的缺口。通过 fast-context 定位站点详情、项目目录识别、配置备份和路径保护；沿用 UI/UX skill、Next 本地 use-client 文档以及现有 zip 2 依赖，没有新增依赖。

站点详情新增“文件备份”页签。用户选择项目目录或 Web 根目录，先显示后端实际识别的绝对路径；默认排除 .git、node_modules、.next、.nuxt、.venv、venv、__pycache__、target，可关闭排除。归档包含普通文件、隐藏文件和空目录，不包含外部数据库；未排除的链接、junction、特殊文件、不可移植路径及大小写冲突会明确拒绝。单份上限 100,000 个条目、50 GiB 原始文件，清单上限 32 MiB。创建前要求确认暂停修改，并说明归档可能包含 .env、密码和密钥。

归档使用 ZIP64、流式压缩和逐文件 SHA256，写入每个站点独立的 SHA256 目录。临时归档完整写入、同步后以不覆盖方式发布；同秒连续备份有随机名称，不替换旧归档。写操作持后台工作保护、数据目录活动锁和站点文件锁。创建前后核对文件列表、大小、修改时间、权限及站点范围 revision，拒绝备份目录与源目录相互包含；Windows 打开文件时拒绝 reparse point，Unix 额外核对设备与 inode。它不是文件系统事务快照，不能保证外部并发写入的完整隔离，仍需用户暂停文件修改。

支持归档搜索、分页、刷新、打开所在目录、删除确认，以及恢复到默认 restored-sites 或自选父目录。恢复先确认来源可信，严格核对站点归属、清单、ZIP 条目类型和尺寸，拒绝路径穿越、设备名、ADS、大小写别名及文件/目录冲突；逐文件验证 SHA256 和 ZIP CRC。每次恢复创建独立新目录，失败由临时目录清理，不覆盖原项目、不改变站点绑定、不启动项目代码。恢复父目录不能位于已登记站点的项目/Web 目录或归档树内。成功后保留副本路径和打开目录/复制路径操作。Unix 普通文件恢复权限位，不恢复所有者、ACL、原始时间或目录权限；Windows 不承诺恢复只读属性。

界面沿用中英文文案、Select、Switch 和现有确认弹窗。进度按 operationId 隔离，显示真实处理阶段、文件数和字节数；处理中禁止重复操作、切换页签和关闭详情，监听在操作结束后清理。未保存站点或环境变量修改时提示先处理草稿；失败保留输入，过期范围要求重新读取。窄屏页签使用两列布局，长路径可换行，归档操作可折行，恢复弹窗正文滚动、底部操作固定，分区采用左右留白的虚线。浏览器 mock 明确标记为交互预览，不表示真实文件已备份。

Windows 隔离临时目录验收覆盖中文和空格路径、.env、二进制、空目录、依赖排除开关、Web/项目范围、连续归档、多次恢复、自选父目录、未确认拒绝、原项目和归档目录保护、并发锁、旧 revision、备份中源目录变化、错误站点、删除路径穿越、内容损坏、清单穿越、大小写别名、损坏 ZIP 列表与删除及 junction 拒绝。源项目、外部文件和已有副本保留。独立发布树 pnpm check 和 cargo check --workspace --all-targets --locked 通过；备份、路径、环境文件及配置传输相关回归共 59 通过、0 失败、1 忽略、644 filtered out。忽略项需要外部 PHP/phpdotenv 环境，不计入本轮验收。只扩展既有 Rust 模块中的验收，未新增测试文件。

本轮尝试访问现有 localhost:3000，浏览器返回 ERR_CONNECTION_REFUSED，本机也确认没有端口监听；按用户要求未启动前端 dev、未执行本地前端 build。因此本轮没有完成浏览器交互及桌面/390px/320px 截图目检，不将源码布局检查视为视觉验收。已关闭仅本轮创建的失败 QA 上下文。macOS/Linux 原生文件操作和桌面文件选择器也尚未实机验收。

11 个版本文件同步到 0.2.94，Cargo.lock 仅更新三个本项目 crate；独立发布树共 20 个任务文件，排除用户原有 configgen.rs 的 178 additions / 9 deletions 和本地生成文件。提交前发现工作区另有导航拆分和 CodeMirror 依赖改动，包含 package.json、i18n.ts 的重叠编辑；提交内容取已验证独立树中的精确文件版本，保留这些其它改动在工作区。本次没有业务数据库变更，未修改 update.sql。v0.2.93 Release 已核对 completed/success。本轮使用新 annotated tag v0.2.94，与 main 原子推送并在推送后核对远程及 Release 实际状态。自动计划、覆盖恢复、外部归档导入及后续 UI 实机验收仍待完善，整体目标保持进行。

参考：https://support.servbay.com/getting-started/backup-and-restore

## 第一百零九轮：外部站点归档导入与跨站点恢复（v0.2.95）

沿用 ServBay 的分类备份与归档管理流程，补齐站点 ZIP 拷到另一台机器或另一个站点后没有入口导入的问题。读取官方备份文档，并通过 Context7 核对 zip 2 的流式写入和归档读取接口。fast-context 两次因网络错误失败后，直接阅读上一轮已定位的 sitebackup、Tauri 命令、站点备份组件及本地 zip 2.4.2 源码继续实现；沿用已读 UI/UX skill 与 Next use-client 文档。

“已有归档”区域新增“导入归档”。用户通过桌面文件选择器选择 NiceEnv 站点 ZIP，先显示源文件、原项目目录、归档日期、文件数、原始/ZIP 大小、排除项，以及目标站点名称和根目录。确认可信及目标后导入为新备份，成功后进入当前站点归档列表，复用既有恢复到新目录操作。导入不覆盖源 ZIP 或现有归档，不修改目标站点文件、目录绑定或进程状态。普通项目压缩包缺少 NiceEnv 清单，不能直接导入；界面明确说明此限制。

预览和导入分别读取源文件到应用数据目录内的私有临时副本，逐块计算完整 ZIP 的 SHA256；后续解析只访问固定副本。源文件在读取时变化、预览后被修改、目标站点名称/根目录/更新时间变化，均拒绝旧确认；导入结束前再次核对目标站点。预览校验结构和范围，实际导入逐文件解压、校验 SHA256 与 ZIP CRC，并重新生成只含已验证条目与目标站点清单的新归档。原项目路径、日期和排除项保留。只有完整校验、同步完成后才以随机新名称发布；失败清理临时副本和待发布归档。

外部 ZIP 在进入 zip 库前检查单磁盘、无注释、无前缀的标准 ZIP/ZIP64 结束记录、中央目录位置/长度和条目数，限制源 ZIP 51 GiB、中央目录 256 MiB、条目数 100,001；原始内容仍限制 50 GiB、100,000 个文件/目录。读取后进一步核对原始条目数与库解析结果，拒绝被重复名称折叠的目录项。恢复与导入共享已有路径、大小写冲突、文件类型和清单校验，不接受路径穿越、设备名、ADS、链接或不一致的条目。导入持后台工作、数据目录活动锁和站点归档锁，不引入新依赖。

交互沿用选择文件、结构化预览、单个可信确认的流程，无须填写 ID 或 JSON。长路径可换行，操作行可折行，正文独立滚动，底部导入按钮保持可达；分区使用留出左右边距的虚线。读取、核对、逐文件导入分别显示阶段及实际字节数；忙时禁止重复操作和关闭。错误提示获得焦点，错误后保留所选文件并清除过期预览，提供重新读取。浏览器只提供明确标记的示例归档预览，不读取本机 ZIP。当前 3000 端口仍无监听，未运行前端 dev/build，未进行本轮浏览器及窄屏截图验收；桌面文件选择器和 macOS/Linux 文件操作仍待实机验证。

独立发布树以 v0.2.94 的 HEAD 为基线，仅覆盖本轮任务变更。由于工作区另有持续进行的 rewrite、完整日志、CodeMirror、导航和 quick-xml 等改动，重叠文件按代码块从基线合成，不复制其它修改。共享 node_modules 中的 @nsb/schema 会指向工作区的新 schema，因此前端验收使用单独的临时 tsconfig，明确将 @nsb/schema 解析到发布副本，并另跑发布副本 schema 自身检查；两项均通过。最终 cargo check --workspace --all-targets --locked 通过。

隔离 Windows 原生验证补充跨站点导入、原 ZIP 字节完全保留、目标目录不变、重复导入生成不同文件、导入后恢复二进制内容、错误站点归属、缺少确认、预览后目标变化、导入期间目标变化、预览后源 ZIP 变化、有效 ZIP64、伪造十亿条目提前拒绝、内容损坏、路径穿越、大小写别名及失败临时文件清理。既有备份/路径/环境文件/配置传输回归共 59 通过、0 失败、1 忽略、644 filtered out；最后修正验收中的目标状态还原后，导入和恢复综合原生验证再次通过。忽略项为需要外部 PHP/phpdotenv 的既有检查，不计入本轮验收。未新增测试文件。

11 个版本文件同步 0.2.95，发布 Cargo.lock 仅改变三个本项目 crate 版本；本轮精确发布 18 个文件，保留工作区其它修改。本次没有业务数据库变更，未修改 update.sql。v0.2.94 Release 已核对 completed/success；本轮新增 annotated tag v0.2.95 并与 main 原子推送，推送后核对远程指向和实际 Release 状态。站点文件自动计划、覆盖恢复及完整 UI 实机验收仍待后续完善，整体目标保持进行。

参考：https://support.servbay.com/getting-started/backup-and-restore


## 第一百一十轮：站点文件自动备份计划与保留策略（v0.2.96）

继续参考 ServBay 网站文件、数据库与配置分别备份的流程，补齐站点文件只能手动归档的缺口。通过 fast-context 定位既有数据库日历、站点 ZIP、桌面调度入口和配置传输过滤，复用现有 ZIP 格式与日历计算，不新增依赖。界面沿用 UI/UX skill 和本地 Next use-client 文档。

每站点可设置每日、每周、每月计划、本机执行时间和 0–100 份保留数量；0 表示全部保留，短月份、夏令时沿用已有日历规则。调度通过启动交接 gate 后每 30 秒检查，使用独立线程，不受数据库长备份阻塞。仅 NiceEnv 运行时执行，重开后过期计划补一次，长备份完成后重新计算下一周期，避免连续补跑。同一站点所有归档写操作共用文件锁，并持后台工作与数据目录活动保护。

计划保存在现有本机 settings，以 siteFileBackupPlan@ 与站点 ID 的 SHA256 隔离；配置导出排除，导入也忽略外部同名设置，不能借导入配置启用自动文件备份或覆盖本机计划。启用和重新配置先读取实际项目/Web 目录与排除项，显式确认可能包含 .env、密钥及自动归档清理策略。保存校验计划 revision 和范围 revision，避免过期窗口覆盖新设置。执行状态不参与配置 revision，后台刷新不会使有效编辑无谓过期。停用不要求源目录存在。

执行前与归档生成期间核对批准范围。站点配置或源目录变化后进入“已暂停，需确认范围”，禁用自动计划；普通文件并发写入只使本次失败，保留下一周期重试。源目录离线等错误记录失败原因，不伪报成功。这不是事务文件快照，界面提示选择低写入时段，不能保证正在写入的项目总能备份成功。

自动 ZIP 使用 site-auto- 文件名和兼容旧清单的 automatic 字段。成功发布新归档后才轮转此站点可读清单的旧自动归档，始终保留本次新归档；手动与导入归档不参与，无法读取的归档也保留。外部自动 ZIP 导入后强制转为普通归档，防止随后被本机计划清理。清理失败记录 partial 和新归档名称，重试可继续执行；界面能看到成功、失败、中断、部分成功与待确认状态。归档列表按创建时间排序，并标记自动归档受保留策略管理。

计划卡片集成到站点“文件备份”，显示启用状态、下次执行、上次结果和生成文件，支持立即执行。设置分为“时间与保留数量”和“实际范围确认”两步，复用数据库日历表单，保留草稿与明确的过期草稿重读入口。计划窗口、归档操作、站点保存/关闭/切换页签互斥；运行进度按 operationId 隔离，状态事件和定期读取刷新归档列表。正文独立滚动、底部操作固定，窄屏控件可折行，长路径换行；内部虚线带左右留白。浏览器 mock 只演示配置和立即执行，明确不进行真实定时文件备份。

只扩展既有 Rust 验收模块，没有新增测试文件。隔离临时目录覆盖启用确认、旧计划/范围 revision 拒绝、状态变化不影响草稿、到期补执行且不重复、真实 ZIP 与恢复内容、手动/导入/损坏归档保留、自动 ZIP 导入脱离轮转、keep=0、互斥锁与 running 读取、源文件写入失败后保留计划、站点变化暂停、目录离线仍可停用、中断识别、清理失败保留新 ZIP 和之后恢复执行、失败临时文件清理。Windows 清理失败验收使用禁止删除共享的已打开文件句柄，验证真实共享冲突，而不是依赖只读属性。配置传输回归包含本机站点计划不能导出、不能由导入覆盖。

最终独立发布树的 schema 检查、显式指向发布副本 schema 的 Web TypeScript 检查、cargo check --workspace --all-targets --locked 全部通过；备份、路径、环境文件及配置传输回归共 60 passed、0 failed、1 ignored、644 filtered out。忽略项需要外部 PHP/phpdotenv，不计入本轮验收。当前 3000 端口无监听，按用户要求未启动前端 dev、未执行本地前端 build，未完成浏览器和窄屏截图目检；macOS/Linux 调度与文件操作仍待实机验收。

11 个版本文件同步 0.2.96，发布 Cargo.lock 仅更新三个本项目 crate 版本。使用基于 v0.2.95 的独立树生成 22 个发布文件，保留工作区其它正在进行的导航、数据库工作区、CodeMirror、rewrite、服务日志、上游版本等改动。根 AGENTS.md 已固定“每次提交/push 必须新增 annotated tag、同步版本、分支与 tag 原子推送、核对远程与 Release 状态”的规则。v0.2.95 Release 已核对 completed/success；本轮使用新 tag v0.2.96，不覆盖旧 tag。没有业务数据库变更，未修改 update.sql。完整 UI 实机验收、覆盖恢复等后续工作仍待完善，整体目标保持进行。

参考：https://support.servbay.com/getting-started/backup-and-restore


## 第一百一十一轮：站点 ZIP 完整校验与恢复预检（v0.2.97）

继续核对 ServBay 官方备份恢复文档，补齐其“定期验证备份可恢复”建议对应的实际入口。原有站点归档列表只读 ZIP 清单，不能证明各文件内容完好；本轮明确区分结构可读与完整校验，避免用户到恢复时才第一次发现损坏。通过 fast-context 定位站点恢复、目录保护及已有进度交互，沿用 UI/UX skill、Next 本地 use-client 文档和 zip 2.4.2 源码；没有新增依赖。

站点归档操作改为“校验与恢复”。用户选定恢复父目录后执行完整校验：将源 ZIP 固定为数据目录内的临时副本，计算整体 SHA256，再读取全部文件至 EOF，逐文件核对长度、SHA256 和 ZIP CRC；清单路径、别名冲突、文件类型、总大小和条目上限仍沿用已有保护。预检在目标父目录创建并清理一个空临时目录，检查可写性与本机路径规则，不写入恢复内容。zip 2.4.2 的 CRC 校验发生在读取到 EOF 时，本轮直接核对本地依赖实现，校验、导入、恢复共用流式内容校验函数，不能仅靠打开条目声称已验证。

预检展示真实父目录、全部文件数量和原始大小、校验时间、内容摘要及可复制的 ZIP SHA256。摘要最多显示 100 条并标明总数，校验始终覆盖全部文件，恢复也包括全部条目；空归档有明确说明。文件 CRC 或内容校验失败显示受影响的归档内路径，不再仅用泛化磁盘权限提示。界面也说明校验需要一份临时 ZIP 空间，恢复还需要解压空间；文件校验不代表代码可信或项目依赖已可运行，恢复前仍需用户确认来源可信。

恢复请求必须携带预检 revision，该值绑定站点身份、名称、根目录和更新时间、归档名称、ZIP 整体摘要与实际恢复父目录。实际恢复再次固定源 ZIP 并核对 revision，随后逐文件校验和写入独立新目录；完成前再次检查站点和目录约束。预览后 ZIP 被替换、站点变化、目标目录变化时拒绝旧确认，恢复中站点变化时清理未发布副本。原项目、源 ZIP 和已有恢复副本保留。选目录、站点草稿变化或操作失败时前端清除旧预检与确认，错误后可以重新校验。

本地归档列表也改为复用受限 ZIP 打开入口：进入 zip 库前校验标准 ZIP/ZIP64 结束记录、中央目录大小/位置及条目数，避免只保护外部导入而让本地列表直接解析伪造的巨大条目计数。列表仍不自动解压所有历史备份，明确显示“清单可读，恢复前需完整校验文件内容”。完整读取期间持站点归档锁、数据目录活动锁和后台工作保护，与手动、自动归档及删除互斥。

恢复窗口沿用固定底部操作区、独立正文滚动、长路径换行及左右留白的虚线。确认前只有完整校验主操作，成功后提供重新校验与恢复；详细清单和 ZIP 摘要折叠展示，避免拥挤。实际预检信息替换可能已过期的列表元数据。校验和读取 ZIP 分别显示进度，错误获得焦点。浏览器 mock 明确显示“演示校验结果，未读取真实文件”，不展示伪造的真实 SHA256。

扩展既有 backup_job 验收模块，没有新增测试文件。Windows 隔离目录验收包含 102 个文件加空目录、只显示 100 条但逐文件校验完整覆盖、CRC 损坏、摘要外最后一个文件损坏、清单可读但内容失败、原 ZIP 字节保留、真实目标路径、没有可信确认拒绝、错误 revision、不同目标目录、站点变化、校验中和恢复中站点变化、合法 ZIP 被替换后旧确认拒绝、新预检后恢复全部内容、空归档、目录写入和临时文件清理、已有副本与原项目保留。本地列表与预检均验证伪造十亿条目的 ZIP64 在解析前被拒绝；原备份、导入与自动计划验收经过新预检流程后继续通过。

独立发布树的 schema 与 Web TypeScript 检查、cargo check --workspace --all-targets --locked 通过；备份、路径、环境文件和配置传输相关回归为 61 passed、0 failed、1 ignored、644 filtered out。忽略项需要外部 PHP/phpdotenv，不计入本轮验收。没有运行前端 dev/build；本轮确认 3000 端口无监听，浏览器交互和桌面/窄屏截图仍未验收。macOS/Linux 原生操作及文件选择器也仍需实机验证。

11 个版本文件同步到 0.2.97，Cargo.lock 仅更新三个本项目 crate。独立发布树基于 v0.2.96，精确发布 18 个任务文件，保留其它正在进行的导航、CodeMirror、数据库工作区、rewrite、日志和上游版本改动。v0.2.96 Release 三个平台均已核对 completed/success。本轮按项目规则新增 annotated tag v0.2.97，与 main 原子推送并核对远程及实际构建状态。没有业务数据库变更，未修改 update.sql。原位覆盖恢复、恢复副本直接接入站点及完整 UI 实机验收仍待后续完善，整体目标保持进行。

参考：https://support.servbay.com/getting-started/backup-and-restore

## 第一百一十二轮：恢复副本识别项目并接入建站向导（v0.2.98）

继续补齐 ServBay 备份恢复之后核对项目入口、运行时及数据库的流程。通过 fast-context 定位既有扫描器、全局建站向导与站点恢复结果，沿用 UI/UX skill、本地 Next use-client 文档和现有组件，没有新增依赖或测试文件。

恢复成功卡片增加“识别恢复副本”：只读识别根目录及一层项目，展示类型、实际 Web 根或应用源目录、识别依据、PHP 版本约束和运行提示。多个项目使用下拉选择；空结果说明检查入口和归档范围，识别失败可重试。识别与文件备份计划、归档操作及站点编辑互斥。选择项目后关闭站点详情并打开现有六步向导，抑制旧抽屉恢复焦点，避免与新窗口抢焦点。识别期间不执行项目脚本。

向导预填名称、项目路径、类型和合适的伪静态规则，要求单独填写新域名。固定“使用已有文件”，保持项目路径和识别类型，证书与数据库默认关闭；数据库可选新建，但 .env、.env.example 和 .user.ini 保持原样，数据库数据需单独恢复并明确更新连接信息。应用项目使用源码目录，代理端口留空由用户确认，进程托管需主动启用，不能把恢复源码误报为已运行。项目意图只保存在非持久化 UI 状态，普通建站入口清除此预填。

桌面 create_site 增加兼容的可选 existingProject 参数，普通调用保持原流程；已有项目模式复用建站校验、配置与回滚链路，跳过目录创建和全部脚手架。后端在初始校验及生成站点前重新识别项目，校验类型、预期入口与真实目录，拒绝模板、环境示例和 PHP 覆盖设置，目录消失或替换为链接时失败，不创建空目录。现有批量扫描建站也使用该保护入口。

扫描器修正通用 PHP 的 public/index.php、CodeIgniter 4 的 public 根及伪静态、Symfony 的伪静态推荐。依赖清单每份最多读取 512 KiB，按实际 dependencies/devDependencies/require 字段识别，避免名称或描述中的框架字样误判。跳过软链接、Windows 目录联接与特殊文件，PHP 项目提取 composer require.php。documentRootReady 区分可用入口和旧接口的目录回退；配置/资源归档没有代码入口、框架 public 入口缺失或清单无效时不能直接建站，界面提供说明与重试。扫描根项目后不再把它的 public 当作第二个项目。相关运行提示统一使用 pnpm。

恢复结果的分割线在卡片内保留左右间距并使用虚线；详情折叠、长路径换行、操作可折行，复用向导的正文滚动与固定底部按钮。中英文文案同步，浏览器模式明确为演示，不宣称读写真实文件。本轮确认 3000 端口没有监听，未运行前端 dev/build，尚未进行浏览器交互、桌面与窄屏截图验收；macOS/Linux 路径和原生选择器仍需实机验证。

独立发布树基于 v0.2.97，schema/Web TypeScript 检查与 cargo check --workspace --all-targets --locked 通过。scanner、sites scaffold、backup_job、paths、envfile、transfer 原有 Rust 模块回归结果为 116 passed、0 failed、7 ignored、589 filtered out。忽略项需要外部 PHP/运行时、实际服务或官方模板下载，本轮未将其计为通过。新增的既有模块验收覆盖 512 KiB 限制、依赖字段识别、public 推荐、Symfony/CodeIgniter 规则、Windows 目录联接拒绝、入口重复去除、缺失目录/入口、已有项目禁止模板/环境文件写入、应用源目录选择，以及真实 ZIP 恢复副本在补齐入口前后的识别与原项目保留。验证仅使用隔离临时目录、测试数据库及配置，不操作用户站点或 hosts。

11 个版本文件同步至 0.2.98，Cargo.lock 只更新三个本项目 crate。精确发布 25 个任务文件，保留工作区其他导航、编辑器、数据库工作区、rewrite、日志和上游版本修改。v0.2.97 的 Windows、macOS Apple Silicon、macOS Intel Release 已核对均为 completed/success。本轮按项目约定新增 annotated tag v0.2.98，与 main 原子推送后核对远程提交及 Release 工作流实际状态。没有业务数据库变更，未修改 update.sql。完整 UI 实机验收和其他功能完善仍待后续继续，整体目标保持进行。

参考：https://support.servbay.com/getting-started/backup-and-restore

## 第一百一十三轮：项目扫描批量建站的真实配置与失败恢复（v0.2.99）

通过 fast-context 复查 ProjectScannerDialog、全局 SiteWizard 和既有建站校验，确认旧批量入口没有传入 PHP 版本，也没有应用代理目标输入，失败只保留项目名并关闭窗口。参考 ServBay 每网站独立 PHP 版本和显式反向代理目标的官方说明，保留批量能力，补齐运行环境与失败恢复，继续沿用 UI/UX skill 和现有 Next 客户端组件。

扫描后可逐项确认独立域名、已安装 PHP 版本或应用 HTTP/HTTPS 代理地址，共用已安装 Nginx/Apache 与明确选择的本地 CA HTTPS 设置。PHP 支持批量应用到已选项目，再逐项调整，并展示 composer 版本要求；不猜测复杂 PHP 约束是否满足，由用户按实际项目确认。域名建议避开已有站点和本次列表的同名建议，提交前检查域名格式、批次内部重名、已有域名、已安装 PHP 和合法代理地址。后端仍负责最终冲突与目录校验。应用项目使用源码目录，可在尚无 out/dist 产物时配置代理；代理目标留空，必须由用户确认，批量入口不启动猜测出来的应用命令。

所有批量创建继续调用已有文件保护模式，固定 template=none、writeEnvExample=false，不创建业务数据库，不修改恢复的环境文件。顺序提交先固定输入快照，以同步 busyRef 防止双击和目录选择/扫描交错；过程中禁止关闭与修改配置，显示当前进度。逐项保留成功、失败和后端修复提示，成功项立即更新查询缓存并退出重试集合；失败不关闭窗口，域名、运行时和代理地址仍可修改，重试不重复创建已成功项目。成功项目可以通过后端已核对的访问地址打开站点。组件卸载后不再继续提交后续项目。

每项目“完整向导”复用现有项目模式，支持证书、数据库与进程托管的完整配置。域名、PHP 版本、代理目标、Web 服务和 HTTPS 草稿一起交接，不再要求重新填写。普通建站入口清除此非持久化预填，恢复副本入口保持兼容。关闭扫描窗口抑制旧焦点恢复，避免干扰新向导。已有项目向导文案改为通用表述，适用于恢复副本和普通工作目录。

扫描结果增加搜索、每页五项、跨页选择保留和针对当前筛选结果的全选/取消；无匹配、空目录、扫描错误、套件读取失败、缺少 PHP/Web 服务均有提示和相应恢复入口。提交前将第一个无效项目定位到对应页并聚焦输入或错误说明。以原生 checkbox 和独立卡片取代包含交互控件的大按钮，避免嵌套交互。窗口加宽、正文独立滚动、底部提交固定、窄屏双列改为单列，长路径与错误换行；标题、页脚和卡片内部虚线都保留左右留白。浏览器模式明确标识示例，禁用原生目录选择和真实站点打开。修正原英文批量失败计数的 {failed}/{fail} 占位符不一致。

独立发布树的 Web/schema TypeScript 检查与 cargo check --workspace --all-targets --locked 通过。未新增测试文件；直接从实际 TypeScript AST 提取配置函数、真实 createAll 协调逻辑与 UI store，在 Node 内执行 24 个验证场景，覆盖唯一域名、非法域名/代理、缺失 PHP/入口、PHP 与应用输入、文件保护参数、并发重复提交、顺序执行、部分失败后保留草稿、仅重试失败项、向导交接和普通新建状态清理。原生 createSite 在这些验证中替换为受控成功/失败响应，因此不把它们当作真实站点启动验收；验证没有修改用户 hosts、业务数据库或真实项目。26 组相关中英文 key/占位符一致性检查通过。Node 环境的 Zustand 持久化提示存储不可用，但内存状态交接断言通过，不影响应用代码。

没有运行前端 dev/build；确认 3000 端口无监听，本轮仍未进行浏览器交互、桌面/窄屏视觉和实际跨平台建站验收。本轮未修改 Rust 功能代码，不重复运行上一轮原生回归，保留其已知验证边界。

11 个版本文件同步到 0.2.99，Cargo.lock 仅更新三个本项目 crate。基于 v0.2.98 独立树精确发布 17 个文件，保留其他正在进行的导航、编辑器、数据库工作区、rewrite 和上游版本改动。v0.2.98 的 Windows、macOS Apple Silicon、macOS Intel 三项 Release 已核对 completed/success。本轮按项目约定新增 annotated tag v0.2.99，与 main 原子推送后核对远程和 Release 工作流。没有数据库变更，未修改 update.sql。整体完善目标继续进行。

参考：https://support.servbay.com/php/set-different-php-for-each-project
参考：https://support.servbay.com/basic-usage/websites/reverse-proxy-web-website

## 第一百一十四轮：项目扫描路径修复与已有项目真实建站验收（v0.2.101）

继续上一轮普通扫描与恢复副本建站流程，使用 fast-context 追踪 scanner、paths、sites 和 Nginx 代理配置。开始时核对到 main 与 annotated tag v0.2.100 已指向 a0e3736，因此本轮独立发布树基于该提交，使用新的补丁版本 v0.2.101。项目 AGENTS.md 已持久记录每次提交、push 或发布必须新增 annotated tag、禁止移动旧 tag、版本同步和原子推送的规则。

修复四项可复现问题：Next/Vite/Nuxt 已绑定源码目录后，再生成 out/dist/.output/public 会被扫描误报为未建站；手动输入相对扫描目录返回相对项目路径，导致后续已有项目建站校验失败；Windows 扩展路径与普通路径无法匹配；读取站点记录失败被当作空列表，导致全部项目显示未配置。在既有 scanner 测试模块补充四项回归，旧实现为 22 passed / 4 failed，修复后 26 passed / 0 failed。

扫描入口先保留普通目录检查，再解析为绝对目录；Windows 复用既有 portable_path_text 去掉扩展前缀，避免把 verbatim 路径交给 Nginx/PHP。路径匹配在 Windows 兼容分隔符、大小写及 UNC 扩展路径，在非 Windows 保留大小写和合法反斜杠字符。应用类型同时匹配源码目录与已有静态产物目录；PHP 仍按实际 Web 根匹配，避免误把暴露项目根的旧站点视为正确 public 绑定。读取站点记录错误向调用方传播。已有项目路径错误文案统一为重新扫描项目目录，适用于普通扫描和恢复副本两个入口。

在既有 sites.rs 的测试模块纳入可显式执行的原生验收，无新增测试文件。使用隔离临时目录、临时 SQLite Store、随机 HTTP/HTTPS/PHP 池端口和 NSB_SKIP_HOSTS=1，运行本机已有 Nginx 1.28.1 / PHP 8.4.26。真实 HTTP 验证中文和空格路径、public 入口 PHP 执行、原 .user.ini 的 memory_limit=96M 生效、项目根私有文件不被 Web 根访问，.env/.env.example/.user.ini 字节保留。第二个站点在 hosts 配置校验阶段失败时原站点继续响应、共享 PHP PID 保持、站点记录回滚，修复配置后重试成功，重复域名被拒绝。

应用源码站点通过真实 Nginx 代理到显式临时 HTTP upstream，不执行应用命令、不生成应用产物；样本随后生成 out/index.html，重新扫描仍正确标记已建站，代理继续响应。PHP 入口被移除后已有项目入口返回 PROJECT_CHANGED，站点记录与环境文件保留。验收最初第二次代理请求返回 502，定位并修正临时 HTTP upstream 的非阻塞接受连接和单次读取问题：接受后显式切回阻塞，限量读取完整请求头后响应；修正后的完整原生验收 1 passed / 0 failed（13.01 秒）。失败断言保留隔离 Nginx 错误日志作为诊断信息。退出时检查管理进程 PID、PHP 池及 HTTP/upstream 端口均已关闭。

独立发布树验证：scanner 26 项、sites 常规 33 项、paths 19 项全部通过；sites 中需要外部运行环境的 7 项默认跳过，其中本轮新增原生验收已另行显式运行通过。Web/schema TypeScript 和 cargo check --workspace --all-targets --locked 通过。没有运行前端 dev/build，没有浏览器或窄屏视觉验收；原生验收仅代表本机 Windows 与上述已有运行时版本，不代表最新版运行时或跨平台验收。

同步 11 个版本文件到 0.2.101，包含 v0.2.100 遗留的三处界面版本文件兜底值。Cargo.lock 只更新本项目三个 crate 的版本。AGENTS.md 同时补齐界面版本兜底值的发布核对要求。精确发布本轮 16 个文件，保留工作区内其它正在进行的导航、拖动排序、服务布局和依赖变更。发布时新增 annotated tag v0.2.101，与 main 原子推送并核对远程提交和 release.yml 的实际状态。没有业务数据库变更，未修改 update.sql。整体功能与 UI 完善目标继续进行。

## 第一百一十五轮：已有 PHP 项目的 Composer 版本校验与兼容推荐（v0.2.102）

参考 ServBay 官方每项目独立 PHP 版本文档，继续打通已有项目扫描与建站的环境选择。经 fast-context 与现有代码核对，先前仅展示 composer.json 的 require.php，完整向导默认最高已安装 PHP，后端没有检查它是否符合项目要求。Composer 官方版本文档说明 ^、~、范围交集、并集、排除和通配符各有独立语义，不能把整条约束简单当作最低版本。本轮复用已安装 Composer 内的 Semver 解析器，无新增依赖，也不实现一套近似规则。

后端增加结构化兼容报告，区分已检查、项目未声明要求、检查不可用和要求无效，列出已检查版本及匹配版本。清单仍受既有 512 KiB、普通文件和目录链接保护，PHP 要求限制 4096 字节；无效 JSON、非对象 require、非字符串要求均明确报告错误。解析器使用已安装 PHP CLI 的 -n 模式，仅加载受管理 Composer phar 自带的版本库，在独立临时目录接收 JSON 参数；不加载项目 autoload、Composer 插件或脚本，不使用项目 php.ini，不运行 install/update，不联网。复用现有有界进程执行与进程组回收，每次最多 10 秒、PHP 内存 64 MiB，最多核对 128 个已安装版本。

扫描结果带兼容报告，同一批的相同约束复用结果，自动检查经过 5 秒后把后续未检查项目标为待逐项检查，不伪装为兼容或无要求。桌面扫描和单项目重新检查改为 spawn_blocking，并在工作闭包内持有数据目录活动保护，避免目录 IO 与解析器阻塞界面或同迁移交错。

扫描列表和完整向导共用 PHP 检查组件：自动推荐已安装且满足约束的版本，保留兼容的明确选择，显示匹配列表、原始要求、不可用原因及重新检查入口。没有要求时按现有版本选择习惯处理；不能检查时不静默推荐最新版，用户须选定版本并明确勾选自行核对才能继续。切换 PHP 或批量改版本会清除确认，重新检查同时清除旧确认与旧结果，失败后显示未完成检查，并保留域名和版本草稿。完整向导打开和 PHP/Composer 安装清单变化后重新查询，取消过期请求结果，普通服务轮询不覆盖草稿。后续步骤出现无效版本或检查问题时，底部提示并提供返回 PHP 配置的入口。沿用 UI/UX skill、Next 本地 use-client 文档及既有控件，检查区留白、长约束换行和窄屏折行保持现有风格。

创建前重新读取磁盘要求并运行校验，已知不兼容或无效约束始终拒绝，前端旧结果或人工确认不能覆盖这些结果。只在检查不可用且本次请求明确允许时接受自行核对模式；默认 false，该状态不写入站点或数据库。原有已有文件保护和域名/运行时校验继续执行。浏览器预览明确返回未检查，不冒充访问过本地项目。

验证在基于 v0.2.101 的独立发布树执行。真实 PHP 8.4.26 / Composer 2.10.3 校验覆盖 ^、~、范围交集与并集、具体版本排除、连字符范围、通配符、精确版本、无效约束、无 PHP/无 Composer、无要求及无效清单。验收确认项目 autoload 和脚本未执行、composer.json/.env 保留；预览后把要求改为不兼容时，创建在写入站点记录前拒绝，人工确认也不能绕过。实际解析同时证明 !=8.2.* 是无效约束，将其纳入拒绝断言，排除具体版本使用 !=8.2.30。原有真实 Nginx 1.28.1 / PHP 8.4.26 建站验收也再次通过，覆盖明确自行核对后的 PHP 执行、代理、环境文件保留、失败回滚与重试；临时服务和端口已回收。

scanner 26 项与 sites 常规 33 项通过，两个依赖本机运行时的原生验收另行显式执行通过；其余需要下载或外部环境的项保持跳过。36 个前端场景直接执行实际源码的推荐、验证、批量协调函数和共享组件渲染，覆盖不兼容不能确认跳过、未知必须确认、无选择不显示通过、加载状态、版本移除、兼容优先和失败草稿保留；原生 API 在前端场景中使用受控响应，不把它们算作真实桌面交互。14 组中英文键和占位符一致。Web/schema TypeScript 与 Rust 全工作区 all-targets 检查通过。没有前端 dev/build，没有浏览器桌面或窄屏截图验收；本轮核对范围仅为根 require.php，不代表扩展、composer.lock/vendor 或完整网站运行均兼容，界面已明确说明。

11 个版本文件同步 0.2.102，Cargo.lock 只改本项目三个 crate。精确发布本轮 21 个文件，保留正在进行的服务排序、导航、Windows 授权和相关依赖修改。v0.2.101 的 Windows、macOS Apple Silicon、macOS Intel Release 三项已核对全部成功。本轮新增 annotated tag v0.2.102，与 main 原子推送并核对远程与 release.yml 状态。没有业务数据库变更，未修改 update.sql。整体完善目标继续进行。

参考：https://support.servbay.com/php/set-different-php-for-each-project
参考：https://getcomposer.org/doc/articles/versions.md

## 第一百一十六轮：已有项目与站点的 PHP 平台环境检查（v0.2.104）

沿用 ServBay 每项目独立 PHP 的配置方式，补齐前一轮只核对 require.php、无法检查项目扩展与间接依赖的缺口。通过 fast-context 定位扫描、建站及站点详情调用链；核对 Composer 官方 check-platform-reqs 文档与本机 Composer 2.10.3 phar 中的命令实现。继续沿用 UI/UX skill、本地 Next use-client 文档、现有 API/IPC 和扩展管理对话框，无新增依赖。

新增只读平台检查，复用已安装 PHP/Composer 的入口解析和 PHP 托管配置，使用 Composer 自带 check-platform-reqs 检查真实 PHP、ext-*、lib-* 等平台要求，忽略项目 config.platform 的模拟值。优先检查 composer.lock；无锁文件时检查项目内 vendor-dir 的 installed.json；两者都没有时明确仅核对根 composer.json。用户可选择包含开发依赖，默认只核对生产要求。锁文件新旧通过 Composer Locker::getContentHash 判断；已安装元数据缺少开发分组、开发依赖覆盖不完整、缺少依赖 autoload 入口、没有平台要求及 PHP 运行提示均单独显示，不将其等同于网站已可运行。

检查仅把必要清单字段复制到隔离临时目录；清单限制 512 KiB，锁定或已安装元数据限制 8 MiB，依赖数量最多 10000。拒绝元数据软链接、目录联接及项目外 vendor 路径；不复制项目仓库、脚本、安装器、autoload 或插件。临时 COMPOSER_HOME、缓存目录与网络禁用环境隔离全局配置，显式禁用 Composer 插件和脚本。使用真实 managed php.ini，但禁用 auto_prepend_file、auto_append_file 和 opcache.preload，避免检查触发项目 PHP 代码。为无 OpenSSL 的 PHP 设置仅限临时目录的 disable-tls，检查仍不联网，并可正常报告项目所需 ext-openssl 缺失。单次 PHP 内存 128 MiB、最多 15 秒，stdout/stderr 分离且分别有输出上限；保留 Composer 非零退出的结构化问题结果，检查结束回收整个自有进程组。后端同一时间只接受一个项目环境检查，防止快速切换产生大量进程。

已有项目建站向导运行环境步骤，以及站点详情 PHP 页签，均可直接执行环境检查、查看缺失或版本不符的要求并打开现有扩展管理面板。每项展示实际版本、要求来源及约束；默认只显示问题，成功项可按需展开，每页 6 项。检查区保留左右留白，分割线使用虚线，长路径/约束换行，按钮可折行。目录草稿未保存时禁止检查，后端从已保存站点记录解析项目根；所选 PHP、项目或安装元数据变化后丢弃旧结果。异步返回不会覆盖已经切换的面板，重复点击不会重复启动；扩展弹窗打开/关闭后提示重检。浏览器示例明确要求在桌面应用执行，不伪造本机报告。此前建站的根 PHP 版本约束强制检查保持不变；本轮平台诊断作为用户可主动运行的只读检查，不自动安装依赖或开启扩展。

在现有 sites.rs 原生验证函数中扩展验收，没有新增测试文件。真实 PHP 8.4.26 / Composer 2.10.3 验证通过，覆盖缺失扩展、真实 PHP 版本冲突、config.platform 模拟值不影响结果、根开发依赖、锁定间接依赖、开发锁定依赖、锁文件新旧、已安装依赖分组、Composer 1 无分组元数据的明确限制、开发范围不完整、根包提供的平台能力，以及 Windows fileinfo 扩展关闭/开启后的结果变化。确认项目脚本、autoload 和 ini 的自动 PHP 代码未执行，原 composer.json/composer.lock/installed.json/.env 内容保留。该验收仅使用隔离临时文件与 SQLite Store，不操作用户业务数据库或启动站点服务。

64 项前端验证直接执行实际组件的状态与事件处理代码、schema 和翻译，覆盖加载、失败重试、双击防重、过期异步返回、输入/安装变化失效、开发依赖参数、结果筛选/分页、扩展弹窗后重检、桌面限定以及不完整范围提示。Web/schema TypeScript 与 Rust 工作区 all-targets 检查通过；最终真实原生校验 1 passed / 0 failed（10.02 秒），同时覆盖 ./vendor 与未声明版本的 self.version 边界。未运行前端 dev/build；没有桌面应用交互或浏览器桌面/窄屏视觉验收，不将组件验证算作实机验收。站点 .user.ini、Web SAPI 配置、依赖文件完整性与网站实际运行不在本次平台检查范围，界面已说明。

开发期间远程新增 v0.2.103，已重新以该提交 f9bae10127b8eece748a738d7e1e4d67b8b70e15 建立独立发布树，保留其拖动排序和 Windows 授权改动。本轮同步 11 个版本文件到 0.2.104，包含上个版本遗留的界面兜底版本值；Cargo.lock 只更新本项目三个 crate。精确提交 22 个任务文件，保留本地生成目录和未跟踪文件。按 AGENTS.md 新增 annotated tag v0.2.104，并与 main 一次原子推送，推送后核对远程与 release.yml 实际状态。v0.2.102 的三平台 Release 已核对全部成功。本次没有业务数据库变更，未修改 update.sql。整体完善目标继续进行。

参考：https://getcomposer.org/doc/03-cli.md#check-platform-reqs
参考：https://github.com/composer/composer/blob/main/src/Composer/Command/CheckPlatformReqsCommand.php
参考：https://support.servbay.com/php/set-different-php-for-each-project

## 第一百一十七轮：Redis 常用设置、配置冲突保护与持久化核对（v0.2.105）

参考 ServBay 的 Redis 管理与配置说明，通过 fast-context 复查数据库页、Redis 状态/连接认证、配置编辑器及服务生命周期调用链。NiceEnv 已有原始配置和历史入口，但缺少常用设置表单。本轮沿用 UI/UX skill、现有 React/Radix 控件与 Next 客户端组件规范，在 Redis 实例卡片增加常用设置；已生成配置的停止实例也可编辑。

设置分为内存与连接、持久化两组。内存支持版本默认、不限制或明确大小，单位为 B/KiB/MiB/GiB；八种淘汰策略使用中文说明，连接超时与最大连接数可留空沿用版本默认。RDB 自动快照支持默认、关闭及最多十六条时间/修改次数规则。关闭原本启用或默认的快照须明确勾选数据丢失风险，后端再次检查。AOF 仅显示启动配置状态，已启用时可编辑同步策略；不把单纯改文件重启当成已有数据的 AOF 启停迁移。Redis 官方要求在线切换、等待重写完成并核对落盘，本轮不在普通表单提供该开关。

后端只编辑实际变化的受管指令，处理重复单值指令、save 重置及 Redis 十进制/二进制内存后缀，保留其它配置、认证信息、注释、空行与换行风格。无法准确表达的配置、include 覆盖关系或带引号指令名明确转到原始编辑器，不猜测结果。保留新版未知的现有策略，但拒绝提交未知新策略或注入值。读取限制安装版本对应的受管理普通 UTF-8 文件、最大 1 MiB，复用路径链接保护。保存使用文件 SHA-256 revision、服务生命周期互斥及现有配置编辑器的再次冲突检测、备份历史和原子替换。配置尚未生成时提示先启动对应版本。修复旧 Redis 引号结构检查误拒绝带转义双引号密码的问题，该检查仍不代表完整 Redis 原生语法验证。

保存仅影响启动配置，不自动重启；界面明确原进程仍使用旧设置，保存快照规则也不会立即生成数据备份。按 Redis 版本隔离查询和草稿，焦点/网络恢复不自动覆盖输入；重复提交使用同步锁。关闭、重新读取或进入原文编辑遇到未保存修改先确认，失败保留草稿；回到表单重新读取。等值内存单位转换不误判为改动；保存成功同步查询缓存，清除先前读取错误。配置原文与历史复用现有编辑器。浏览器预览明确使用模拟数据，不宣称读写本机。

离线 Chromium 直接装载实际 React/Radix 设置组件及现有 CSS，未启动服务器或执行项目 dev/build。桌面 1440×960、窄屏 390×844 中英文截图已检查，窄屏没有横向溢出、连接与快照字段纵向排列、正文滚动、底部按钮保持可见；虚线分隔留在内容内边距内，长路径换行。受控 API 交互通过双击防重、快照关闭确认、保存提示、冲突保留、取消/确认丢弃草稿、版本隔离、读取错误重试、保存后旧错误清除、等值单位不产生改动等场景。原文编辑器在此离线页面只验证交接和返回刷新，未将替代界面作为真实编辑器验收。十三项实际 schema/草稿转换/边界断言及五十六组中英文键和占位符核对通过，浏览器无 pageerror。

现有 Rust 验证函数中扩展解析与合并检查，redis_lint 两项通过；没有新增测试文件。已有 ignored 原生 Redis 验收显式运行通过（1 passed，49.20 秒），使用 SHA-256 已核对的 Windows Redis 5.0.14.1 运行时和隔离临时目录。原生 CONFIG GET 确认保存后当前进程保持 64 MiB，重启后内存 96 MiB、淘汰策略、timeout 42、maxclients 333 和两条快照规则真实生效；两个原有键保留。原配置进入历史，过期 revision 与未确认关闭快照被拒绝；历史还原后旧草稿失效，再次重启恢复 64 MiB 且原键仍在。自有进程已停止。此验收不代表新版 Redis 或 macOS/Linux 实机结果。Web/schema TypeScript 与 Rust 全工作区 all-targets 检查通过。

十一处版本文件同步 0.2.105，Cargo.lock 仅更新 niceservbay、nsb-core、platform 三个本项目 crate。按 AGENTS.md 新增 annotated tag v0.2.105，与 main 一次原子推送，随后核对远程分支、标签目标和 release.yml 的实际状态。上一版 v0.2.104 的 Windows、macOS Apple Silicon、macOS Intel Release 均已确认 completed/success。本次没有业务数据库变更，未修改 update.sql；保留已有未跟踪文件和本地生成目录。整体功能、稳定性和 UI 完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/redis-management-and-usage
参考：https://support.servbay.com/advanced-settings/modify-configurations/modify-redis-settings
参考：https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/

## 第一百一十八轮：Redis 实际持久化状态与可核实的手动快照（v0.2.106）

对照 ServBay Redis 管理文档的持久化、手动 BGSAVE 与恢复流程，通过 fast-context 复查现有 Redis 卡片、原生 RESP、认证和备份链路。此前常用设置只编辑启动配置，无法查看运行时持久化状态或主动生成快照。本轮新增“持久化与快照”面板，继续复用 UI/UX skill、现有 React Query/Radix 控件与 Next 客户端组件规范，无新增依赖。

运行中实例可查看数据载入、RDB 保存进度、保存后修改次数、最近后台保存结果/耗时，以及 AOF 实际启用、重写排队/执行、写入和重写结果。缺失的可选字段显示未报告，必要字段无效时明确拒绝，不将错误当作正常。界面解释 Redis 启动也会初始化保存时间，该时间不能独立证明已有备份。停止状态禁用入口，旧面板在实例停止或切换后显示当前读取错误。

手动操作只发送异步 BGSAVE，不退回阻塞式 SAVE、不修改自动快照规则或 AOF。保存更新 Redis 当前配置指定的 RDB 文件，不创建独立备份副本，界面明确此范围。复用原生连接认证，先核对实际监听端口属于托管 PID，再核对 INFO process_id 与 run_id；凭据不发往未确认归属的监听者，错误正文不回显认证内容。生命周期 try_lock 防止请求和启动/停止交错，数据目录活动保护位于阻塞工作闭包内；只在短时请求阶段持锁，不在后台快照期间长时间阻塞其它服务操作。

Redis 官方说明 BGSAVE 接受不代表落盘完成，LASTSAVE 只有秒级精度且启动时也会赋值。后端先读取同一连接的持久化信息和服务器 TIME，等待服务器时间超过上次保存秒数，再提交请求；等待有时限，无法建立确认边界时不发送 BGSAVE。载入、RDB 保存或 AOF 重写/排队期间拒绝新请求，服务端命令拒绝与请求响应不确定分别说明，均不自动重复写操作。扩展既有 RESP 读取支持受限数组，限制层数、元素数量和总字节，保留认证错误脱敏。

请求回执绑定版本、run_id 和最早确认时间，前端每两秒读取状态，只有同一实例、非保存/载入中、保存结果正常且时间达到确认边界时显示成功。保存失败、实例重启、权限错误、读取失败与两分钟仍未确认均有独立反馈，不因超时停止 Redis 或伪报失败。重复点击同步防重；关闭窗口不取消 Redis 后台操作，重新打开仍可看到真实运行状态。查询使用本次请求接受之后才发起的读取判定结果，避免前次失败或在途旧读取误判重试；已核实结果不会被后续读取错误改写，新请求清除旧结果。

扩展现有 Rust 验证函数，没有新增测试文件。六项原生 RESP/认证验证通过，覆盖 TIME 数组、越界微秒、嵌套/过大/无效响应、持久化字段缺失、未知结果、PID 不符、AOF 排队/错误及已有认证保护。Windows Redis 5.0.14.1 隔离原生验收显式执行通过（1 passed，58.65 秒），验证无认证拒绝、正确密码可保存、错误版本拒绝、RDB 目标被目录占用时后台保存报告失败、修复目标后成功及重启 run_id 变化。生成文件在主实例停机之前复制到另一份临时 Redis，数据库 0 和 2 的原有键均可读回，排除 SHUTDOWN 保存掩盖手动快照失败的可能。两个临时实例及端口已回收；同时保留上一轮配置重启、历史还原和原键保留验收。此结果仅代表本机 Windows 与上述 Redis 运行时，不代表新版 Redis 或其它平台实机验收。

离线 Chromium 装载实际组件和现有 CSS，使用受控 API 检查十六项交互与状态，包含重复提交、旧保存时间不能成功、失败后重试不会沿用旧错误、实例重启、权限拒绝、长时间未确认、关闭后重新查看，以及确认成功后遇到读取错误。另有十项实际状态函数/schema 断言和三十八组中英文键/占位符核对，浏览器无 pageerror。1440px 中文桌面、390px 中文和 320px 英文截图已目检：桌面标签/值双列，窄屏纵向重排、正文滚动、页脚折行且主操作可见，没有横向页面溢出，虚线分隔保留左右间距。没有前端 dev 服务或正式前端 build，离线 API 交互不等同于桌面 IPC 实机验收。

Web/schema TypeScript 与 Rust 全工作区 all-targets 检查通过。十一处版本文件同步至 0.2.106，Cargo.lock 只更新三个本项目 crate。精确提交二十一个任务文件，保留其它未跟踪文件和本地生成目录。上一版 v0.2.105 的 Windows、macOS Apple Silicon、macOS Intel Release 均已核对 completed/success。按项目约定新增 annotated tag v0.2.106，与 main 原子推送后核对远程指向和 release.yml 实际状态。本次没有业务数据库变更，未修改 update.sql。完整产品功能、稳定性和 UI 完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/redis-management-and-usage
参考：https://redis.io/docs/latest/commands/bgsave/
参考：https://redis.io/docs/latest/commands/lastsave/
参考：https://redis.io/docs/latest/commands/time/

## 第一百一十九轮：Redis 独立 RDB 备份与停止实例恢复（v0.2.107）

对照 ServBay Redis 管理文档和 Redis 官方持久化说明，通过 fast-context 复查数据库卡片、原生 RESP、配置和已有备份链路。本轮在“持久化与快照”之外增加“数据备份与恢复”：创建可单独保留的 RDB 副本，在 Redis 停止时预检并恢复该副本。复用现有 React Query、Radix、确认弹窗、路径保护和服务生命周期互斥，无新增依赖。

独立备份先核对托管进程、实例 run_id、认证及 CONFIG GET 返回的实际 dir/dbfilename，提交 BGSAVE 后等待同一实例确认保存完成，再核对保存位置未变并复制 RDB。保存在 backup/redis/时间戳-随机标识/，包含 content.rdb 与 metadata.json；先在临时目录完整写入并同步文件，完成后重命名发布。采用流式复制、SHA-256、RDB 头部、EOF 和 CRC64/Jones 校验；Redis 关闭内置校验时允许零校验尾部，但仍记录并核对 SHA-256。这些检查不等同于完整 RDB 对象语法解析，也不宣称调用了本机 Redis 不支持的 --check-rdb。等待超时不终止 Redis 后台保存。

恢复仅接受应用管理的备份，要求 Redis 已停止且无未确认接管进程，默认安装版本与备份版本相同。拒绝 AOF 已启用、include 覆盖及无法准确表达的配置，不自动关闭 AOF。各 Redis 版本共用 data/redis，界面明确恢复会替换全部逻辑数据库。预检 revision 绑定备份元数据/内容、完整配置和当前目标文件；用户输入 Redis 名称与版本后，执行端再次核对范围。使用同目录临时文件准备新 RDB，保留当前原文件，再核对 revision 并原子替换。损坏或空的原文件同样原样保留，损坏副本不能绕过校验用作恢复源。恢复结束保持停止，用户仍需启动对应版本并核对数据。

新增备份列表、当前版本筛选、显示全部版本、每页五条、备份目录入口、恢复预检及恢复前副本反馈。目录入口可用于把整个备份文件夹复制到其他磁盘，本轮不提供外部 RDB 导入或删除备份。操作同步防重、执行中禁止关闭；失败保留确认输入，重新检查会清空旧确认。运行状态变化立即禁用恢复，后端仍独立校验。浏览器预览仅在内存模拟，明确不读写本机数据，文件夹入口禁用。

扩展已有 Rust 验证函数，没有新增测试文件。六项 RESP/认证验证通过，包含 CRC64 官方检查向量。显式运行隔离 Windows Redis 5.0.14.1 原生验收通过（1 passed，92.17 秒）：验证认证后独立备份、运行中拒绝恢复、错误确认、配置变化、AOF、损坏备份、路径越界，以及恢复后真实 GET 读回原值 1、恢复前副本回退后读回修改值 2；损坏原文件逐字保留后仍可恢复有效备份。延续原有配置重启、端口回退、快照失败与修复、两个逻辑数据库读取验收，临时进程和端口已回收。此结果仅代表本机 Windows 和该 Redis 运行时，不代表新版 Redis 或其他平台实机验收。

离线 Chromium 装载实际 React/Radix 组件与现有 CSS，受控 API 的二十项交互、实际 schema 断言及三十四组中英翻译核对通过，无 pageerror。1440px 桌面、390px 列表与确认、320px 英文空态截图已目检：长路径与摘要换行、正文滚动、页脚折行且操作可见、没有横向溢出，虚线分隔保留左右间距。离线 API 验证不等同于桌面 IPC 实机验收。Web/schema TypeScript 与 Rust 全工作区 all-targets 检查通过；未启动前端 dev 或执行正式前端 build。

十一处版本文件同步至 0.2.107，Cargo.lock 只更新 niceservbay、nsb-core、platform 三个本项目 crate，精确提交二十二个任务文件，保留其他未跟踪文件和本地产物。上一版 v0.2.106 的 Windows、macOS Apple Silicon、macOS Intel Release 已核对全部 completed/success。本轮按项目约定新建 annotated tag v0.2.107，与 main 一次原子推送，并核对远程指向与 release.yml 实际状态。本次没有业务数据库变更，未修改 update.sql；整体功能、稳定性和 UI 完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/redis-management-and-usage
参考：https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/
参考：https://redis.io/docs/latest/commands/bgsave/
参考：https://redis.io/docs/latest/commands/lastsave/
参考：https://github.com/redis/redis/blob/5.0/src/crc64.c

## 第一百二十轮：外部 Redis RDB 导入与启动数据来源核对（v0.2.108）

继续对照 ServBay 的 Redis 持久化文件备份/恢复流程，经 fast-context 检查现有备份面板、文件选择器、路径保护和恢复链路。此前独立备份支持复制到外部磁盘，但没有导入外部 dump.rdb 的入口；本轮补上文件选择、预检、确认及独立副本保存，延续现有 UI/UX、React Query、Radix 和 Tauri 文件选择模式，无新增依赖。

预检流式读取普通 .rdb 文件，检查 RDB 头、EOF 和启用时的 CRC64，并计算 SHA-256。只保留前 64 KiB 用于读取 AUX redis-ver 元信息，对字段长度、编码和读取范围设限，不解析键值对象、不执行 Redis 指令。接受常见三段版本以及 Windows 的四段版本号，保留文件标记的完整版本，不擅自截短或映射到当前版本。无法确定版本、旧格式、压缩版本字段或不支持的元信息明确拒绝。文件路径必须是绝对路径，拒绝目录联接、符号链接、路径穿越和特殊文件名。

确认信息包含源文件路径、文件标记版本、大小和可展开的格式/摘要。预检 revision 绑定规范路径、版本、格式、文件大小和 SHA-256；提交时重新读取并核对，复制到临时备份目录时再次验证摘要，完整写入后才发布记录。导入不要求 Redis 停止，不覆盖当前实例或源文件，仅生成 kind=imported 的新备份，之后仍走已有停止实例恢复和同版本检查。界面如实说明元信息和完整性检查不代表模块兼容或完整数据可加载，恢复后仍需启动对应版本核对数据。

导入其他版本后自动显示所有备份，列表标记外部来源；当前版本筛选无结果与完全没有备份分别提示，提供直接切换为全部版本的操作。取消文件选择不触发读取或写入，重复选择/提交同步防重；失败保留源文件预览，可重新检查后提交新 revision。确认按钮在读取、重检及提交期间统一禁用。浏览器使用明确标注的内存演示，不读取本机文件。

根据 Redis 最新官方持久化说明，preload-file 可使启动跳过普通 RDB/AOF 数据位置；恢复预检增加对此类有效配置及 replicaof/slaveof 上游复制配置的拒绝，避免文件替换后又由其他数据来源覆盖。空的 preload-file 引号值不误拦截。此功能不会自动修改复制关系、预载路径或 AOF 开关。

原生验收发现 Windows Redis 5.0.14.1 会写入四段版本标记，已修复导入解析并纠正既有验收的安装版本标记。一次多次重启验收还观察到 run_id 重复，故持久化报告、快照回执和前端完成判断增加 processId，后端独立备份等待也同时核对 PID 与 run_id；重启验收比较两者组成的实例标识，不再假定单独 run_id 一定不同。

扩展已有 Rust 验证函数，没有新增测试文件。Windows Redis 5.0.14.1 隔离原生验收最终通过（1 passed，98.14 秒）：导入原生生成 RDB、源文件保留、运行中值 3 未被导入覆盖、停止后恢复并真实读回原值 1、两个逻辑数据库保留；正确 CRC 的内容变化使旧 revision 失效，损坏文件、缺失版本、超大声明长度、非 RDB 和目录联接被拒绝，预载/复制配置拒绝与空预载值允许均通过。既有认证、快照失败恢复、配置重启和恢复前副本回退验证继续通过，自有进程/端口已回收。六项 RESP/认证回归通过。以上仅代表本机 Windows 与该 Redis 运行时，不代表其他平台或新版 Redis 实机结果。

离线 Chromium 使用实际组件、现有 CSS 和受控 API/文件选择器验证四十二项交互及状态判定，包含重复点击、取消选择、失败保留、重新检查、跨版本列表、同 run_id 不同 PID 的重启判断，以及缺失/零 PID 的 schema 拒绝；四十九组中英翻译核对通过，无 pageerror。1440px 桌面、390px 中文和 320px 英文导入确认截图已检查，长路径/摘要换行、正文滚动、底部按钮保持可见，无横向溢出。文件选择器在离线验收中为受控替代，不将其作为原生对话框或桌面 IPC 实机验收。Web/schema TypeScript 与 Rust 全工作区 all-targets 检查通过，未运行前端 dev 或正式前端 build。

十一处版本文件同步 0.2.108，Cargo.lock 仅更新三个本项目 crate；精确提交二十二个任务文件并保留已有未跟踪文件与本地产物。上一版 v0.2.107 的 Windows、macOS Apple Silicon、macOS Intel Release 均已确认 completed/success。新增 annotated tag v0.2.108，与 main 一次原子推送后核对远程指向及 release.yml 的实际状态。本次没有业务数据库变更，未修改 update.sql；整体产品完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/redis-management-and-usage
参考：https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/
参考：https://github.com/redis/redis/blob/5.0/src/rdb.c
参考：https://github.com/tporadowski/redis/blob/v5.0.14.1/src/util.c

## 第一百二十一轮：Redis 备份导出、清理与异常记录展示（v0.2.109）

继续对照 ServBay Redis 持久化文件管理流程，在已有独立备份、外部 RDB 导入和停止实例恢复基础上，补充单份备份导出与删除。通过 fast-context 检查调用链，复用既有 React Query、Radix、Tauri 文件对话框、服务生命周期互斥及数据目录活动保护，无新增依赖。

导出先选择新的 .rdb 文件路径，限制绝对路径、普通文件名和 NiceEnv 数据目录以外的位置，拒绝目录联接、符号链接和路径穿越。使用目标目录内临时文件流式校验 RDB 头、EOF、启用时的 CRC64 和 SHA-256，核对大小及备份元数据未变化，同步文件后以不覆盖方式发布；已有目标保持原样。默认文件名仅使用受检查的备份 id。取消另存为不执行导出，浏览器预览仅内存演示，不读写本机文件。这些检查不等同于完整 RDB 对象语法解析。

删除先生成包含备份 id、文件大小和实际字节摘要的 revision，确认时重新核对；只管理 content.rdb 与 metadata.json，允许记录损坏或其中一个文件缺失，发现额外文件则拒绝自动清理。文件夹暂存改名后再次验证，逐个删除已知文件并移除空目录，不递归删除。变化或清理失败时尝试还原目录，失败提示保留内容的具体位置，不宣称成功。应用内导出、删除和恢复共用生命周期锁，后台工作和数据目录活动计数覆盖阻塞任务。当前 Redis 数据、其他备份和已导出的文件保持原样。

列表使用独立的展示结构，保留损坏元数据、缺失 RDB 和大小不符的异常记录，展示具体问题并禁用恢复/导出。未知版本记录在当前版本筛选中也显示。列表只做基本可用性检查，完整性校验在导出或恢复时执行，避免每次列出都读取所有大文件。每行增加“更多备份操作”菜单，分隔线复用左右内缩的虚线组件。删除确认展示所选记录、时间、版本、大小及异常，恢复前副本另有无法再回退的提醒；失败保留上下文并提供重新检查，执行期间同步防重并禁止关闭。

扩展既有原生验收函数，没有新增测试文件。Windows Redis 5.0.14.1 隔离验收通过（1 passed，98.79 秒）：导出逐字等于原生 RDB 且可重新导入检查完整版本；拒绝覆盖目标及向应用数据目录导出；损坏记录和缺失 RDB 可展示并清理；预检后文件变化使旧 revision 失效；额外文件被保留；损坏 RDB 导出失败且不留下目标文件；另一线程持有生命周期锁时删除和导出返回繁忙；路径越界拒绝。最终真实 GET 仍为原值 1，其他备份和导出文件保留，自有进程与端口已回收。原有认证、配置重启、快照、恢复和外部导入验收继续通过。结果仅代表本机 Windows 与该 Redis 运行时。

离线 Chromium 使用实际 React/Radix 组件和现有 CSS，通过六十一项交互/状态检查、schema 断言及六十七组中英翻译核对，无 pageerror。桌面操作菜单、390px 中文异常记录删除确认和 320px 英文确认截图已目检：虚线留有左右间距，长内容换行，正文可滚动且按钮可见，没有横向溢出。受控 API 和文件对话框替代不代表桌面 IPC 或原生系统对话框实机验收。Web/schema TypeScript 和 Rust 全工作区 all-targets 检查通过，未运行前端 dev 或正式前端 build。

十一处版本文件同步至 0.2.109，Cargo.lock 仅更新 niceservbay、nsb-core、platform 三个本项目 crate。精确提交二十个任务文件，保留用户已有未跟踪文件与本地产物。上一版 v0.2.108 的 Windows、macOS Apple Silicon、macOS Intel Release 均已确认 completed/success。本轮依照项目约定新增 annotated tag v0.2.109，与 main 一次原子推送，随后核对远程分支、标签目标和 release.yml 的实际状态。本次没有业务数据库变更，未修改 update.sql；整体产品完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/redis-management-and-usage
参考：https://redis.io/docs/latest/operate/oss_and_stack/management/persistence/
参考：https://github.com/redis/redis/blob/5.0/src/rdb.c

## 第一百二十二轮：Redis 服务密码设置与正常停机确认（v0.2.110）

对照 ServBay 数据库密码设置说明，经 fast-context 复查现有 Redis 连接凭据、常用设置、认证停机和配置历史链路。此前“连接凭据”仅验证并保存客户端登录信息，不能设置 Redis 服务的 requirepass。本轮增加独立“服务密码”窗口，设置、更换默认用户的启动密码，并在明确确认后允许关闭认证。复用现有 UI/UX、React Query、Radix、配置编辑器和本机凭据结构，无新增依赖。

读取只返回启动认证是否启用、完整配置 revision 和无法直接编辑的原因，不返回原密码。根据 Redis 官方 ACL 说明，Redis 6 起 requirepass 对应默认用户；配置包含用户规则、非空 aclfile 或 include 时，转入原有配置编辑器管理，不覆盖其认证规则。带引号的指令名、无法确认参数范围的密码行也明确拒绝。支持既有多条 requirepass 的最后值语义，保存时合并为一条，保留其他指令、注释和换行风格。新密码限制 1–512 UTF-8 字节，拒绝控制字符和纯空白，正确转义双引号和反斜杠，空格、井号和中文可保留。关闭认证需要后端独立检查确认参数。

保存要求 Redis 已停止、默认版本仍与窗口一致，并持有生命周期互斥；使用完整配置 revision 和原配置再次核对防止覆盖外部编辑，复用配置历史和原子替换。保存后同步此版本默认用户的本机连接凭据，保持 Redis 停止，用户启动后生效。若配置已写入但本机凭据记录失败，返回明确的部分保存结果及新 revision，界面保留密码草稿供再次保存；不报告全部成功。密码输入默认隐藏，可同时显示两次输入，保存成功清空输入并恢复隐藏状态。界面提示项目、客户端和下游副本也需更新连接密码。

运行中可在密码窗口内确认“停止 Redis 后继续”，后端在同一生命周期锁内重新确认运行版本，然后复用正常认证停机、进程归属核对、失败保留实例及 PID 文件流程，不强制结束进程。停机本身不保存密码，停机失败在确认框内展示原因并保留草稿；成功后更新服务状态，继续保存而无需跨页重新输入。执行期间同步防重、禁止关闭；版本改变立即禁用保存和停机。关闭、重新读取或进入原文编辑遇到草稿先确认，失败保留输入，模式切换保留密码草稿。浏览器为明确标注的内存演示，其连接验证对应演示服务密码。

扩展既有 Rust 验证函数，没有新增测试文件。两项 Redis 配置解析验证通过，覆盖重复指令、CRLF 保留、空密码确认、引号/反斜杠/中文、512 字节边界、注入拒绝及 ACL/include 保护。Windows Redis 5.0.14.1 隔离原生验收最终通过（1 passed，130.79 秒）：运行中保存被拒绝，错误版本不能停机；窗口停机入口正常回收进程；含特殊字符的新密码重启后可真实认证，未认证和错误密码被拒绝，GET 仍读回原值 1、两个逻辑数据库的键保留。配置历史包含原文，外部修改使旧 revision 失效，关闭认证需确认且重启后可无密码访问；复杂认证配置不改文件或凭据。仅在隔离 SQLite 注入凭据写入失败，确认部分结果、新 revision 重试及再次启动正常；跨线程生命周期锁冲突返回繁忙。原有设置、快照、备份导入导出、恢复和清理验收继续通过，自有进程与端口已回收。结果仅代表本机 Windows 和该 Redis 运行时。

离线 Chromium 装载实际 React/Radix 组件和现有 CSS，通过四十六项交互/状态检查、schema 断言及十七组中英翻译检查，无 pageerror。覆盖停机确认与取消、停机失败重试、草稿保留、防重复保存、实例变化禁用、部分保存、冲突重新读取、字节限制与密码确认、显隐同步和 ACL 编辑器交接。1440px 桌面、390px 中文关闭认证及 320px 英文密码表单截图已目检；虚线分隔保留左右间距，长文案换行、正文滚动、底部操作可见，没有横向溢出。配置编辑器在该夹具中仅验证交接和返回刷新，未将其替代界面作为完整原生编辑器验收；离线 API 验证不代表桌面 IPC 实机验收。

Web/schema TypeScript、Rust 全工作区 all-targets 与 diff 检查通过，未运行前端 dev 或正式前端 build。十一处版本文件同步至 0.2.110，Cargo.lock 仅更新三个本项目 crate；精确提交二十二个任务文件并保留用户原有未跟踪文件及本地产物。上一版 v0.2.109 的 Windows、macOS Apple Silicon、macOS Intel Release 均已确认 completed/success。本轮依照项目约定新增 annotated tag v0.2.110，与 main 一次原子推送，随后核对远程指向及 release.yml 实际状态。本次没有业务数据库变更，未修改 update.sql；整体产品完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/reset-database-password
参考：https://redis.io/docs/latest/operate/oss_and_stack/management/security/acl/
参考：https://raw.githubusercontent.com/redis/redis/unstable/redis.conf

## 第一百二十三轮：MongoDB 官方 Shell 与 Database Tools（v0.2.111）

对照 ServBay 的 MongoDB 管理说明，补齐与服务器独立发布的 MongoDB Shell 和 Database Tools。清单新增 mongosh 2.12.0 与 mongodb-database-tools 100.19.0，归入工具分类，复用已有安装窗口、版本选择、安装记录和管理终端，不注册为常驻服务。Windows x64、macOS Apple Silicon、macOS Intel 各自使用精确的官方构建，六个压缩包均已实际下载并核对官方 SHA256、文件大小和归档内入口；Shell 同目录的加密库完整保留，Database Tools 包含备份、恢复、导入导出、文件和监控的八个命令。

Shell 复用 GitHub Release 版本源，按平台、架构和 zip 文件名匹配，过滤预发布版本。Database Tools 新增官方 full.json 解析源；不复用 MongoDB 服务器版本，按数值排序并排除 99.0.0 占位发行、开发发行、缺失 archive 或有效 SHA256 的条目，选择对应平台压缩包而非 MSI。实际在线目录核对通过：Shell 三个平台各返回 60 个版本，Database Tools Windows、Apple Silicon、Intel 分别返回 45、26、42 个版本；最新版本、下载地址、入口和 SHA256 与内置清单一致。沿用现有缓存和安装合成路径，没有新增依赖。

同时修复新增双架构条目会暴露的清单问题：远端快照叠加到内置清单，旧快照不能隐藏新内置套件；同 id/version 的不同架构条目可以共存，覆盖只影响声明范围，未限制平台的用户条目仍可覆盖全部变体。安装查找优先本机兼容版本，列表按 id/version 折叠为本机适用条目，已安装快照仍优先于可下载清单，不重复显示已安装版本。用户模块继续位于最后一层。

扩展已有 Rust 验证函数，未创建测试文件。清单、安装及卸载相关 29 项、PATH 与终端相关 27 项、版本源相关 4 项回归通过；各模块未选用的既有原生网络验收保持 ignored。Windows MongoDB 8.0.4 隔离原生验收通过（1 passed，22.93 秒）：通过真实 Installer 校验缓存的官方压缩包、解压、注册安装记录及重复安装；核对安装快照和列表唯一性，管理终端包含两套工具的实际 bin 目录；mongosh 与八个 Database Tools 命令的版本检查成功。mongosh 连接随机回环端口，读取独立 Node 驱动写入的文档并新增中文文档；mongodump 导出 gzip archive，mongorestore 恢复到另一临时数据库，再由独立 Node 驱动读回两个文档。正常停机、重启后数据仍在，原有进程归属检查和停机回归继续通过，自有进程与临时数据已回收。未修改用户系统 PATH。macOS 的归档、校验值与在线目录已验证，尚未在 Mac 实机运行工具。

Web/schema TypeScript、218 条清单的 Zod 解析、Rust 全工作区 all-targets 与 diff 检查通过。此次复用现有套件 UI，没有调整布局，未运行前端 dev 或正式前端 build，也未将浏览器预览视为原生验收。十一处版本文件同步至 0.2.111，Cargo.lock 仅更新 niceservbay、nsb-core、platform 三个本项目 crate；发布范围为十九个文件，保留用户原有未跟踪文件和本地产物。上一版 v0.2.110 的 Windows、macOS Apple Silicon、macOS Intel Release 均已确认 completed/success。本轮按项目约定新增 annotated tag v0.2.111，与 main 一次原子推送，并核对远程指向与 release.yml 的实际状态。本次没有业务数据库变更，未修改 update.sql；MongoDB 图形管理和整体产品完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/mongodb-management-and-usage
参考：https://www.mongodb.com/docs/database-tools/installation/
参考：https://api.github.com/repos/mongodb-js/mongosh/releases/latest
参考：https://downloads.mongodb.org/tools/db/full.json

## 第一百二十四轮：MongoDB 数据库、集合与文档浏览（v0.2.112）

对照 ServBay MongoDB 管理与交互使用说明，补齐数据库页此前没有 MongoDB 工作区的缺口。本轮增加独立 MongoDB 页签；只有 MongoDB 已安装时默认进入该页签，已有 MySQL/MariaDB 和 PostgreSQL 选择顺序保持。复用 UI/UX、React Query、Radix、卡片与复制按钮，提供实际实例连接地址、运行版本、Shell 版本、数据库选择、集合名称搜索、集合/视图选择、文档分页和字段相等筛选。MongoDB 和官方 Shell 缺失、实例停止、查询失败均有明确提示与恢复入口。新增一个独立前端组件及一个 Rust MongoDB 模块以承载专属行为，没有引入依赖。

后端复用官方 mongosh 与已安装快照，固定连接本机回环地址，端口来自当前托管实例，不能由调用方指定远程地址。请求须匹配运行版本；查询前后核对监听端口的进程归属，并在读取前核对 MongoDB 返回的数据目录与该版本的应用目录一致。持有数据目录活动守卫和服务生命周期锁，操作放入 Tauri 阻塞任务，不阻塞界面线程。临时脚本只包含序列化参数和固定只读操作，不接受自由 JavaScript；脚本、输出和 Shell 配置使用临时目录，关闭用户启动脚本加载，超时回收子进程并删除临时文件。

集合列表使用 listCollections/getMore，并正确关闭尚未耗尽的命令游标；不使用 mongosh 不提供的旧版 DBCommandCursor 全局构造器。集合名称按字面量搜索，含引号、分号和正则特殊字符的合法名称可以使用。数据库与集合列表最多显示 1,000 项，集合可以继续缩小搜索；文档每页 10 条，接口上限 25 条，偏移上限 10,000，并设置查询及进程超时。支持文本、数字、布尔、空值/不存在、ObjectId 五种结构化筛选，校验字段、字节长度、数值精度和类型。文档保留 canonical Extended JSON 中的 ObjectId、日期、Int64、Decimal128 等类型；超过 65,536 个字符的文档明确标注预览并关闭完整文档复制，避免把截断内容当作完整记录。此轮为只读浏览，认证配置、数据编辑和图形备份恢复仍需继续完善。

界面查询按实例进程、版本、端口、数据库、集合、筛选和分页隔离缓存；上游选择失效或实例改变时不继续展示旧范围文档，错误时隐藏旧结果并提供重试。数据库/集合采用真实下拉选项，布尔值和类型采用选择控件，筛选不要求输入 JSON。窄屏表单纵向排列，页签可横向滚动，长名称换行，文档内容换行并限制展开高度；卡片内虚线保留左右间距。浏览器演示在现有内存后端实现相同读取接口，也检查 MongoDB 与 Shell 的安装运行条件，没有冒充真实数据库。

扩展既有 MongoDB 原生验收函数，没有新增测试文件。Windows MongoDB 8.0.4、mongosh 2.12.0 与 Database Tools 100.19.0 隔离验收最终通过（1 passed，41.33 秒）：实际安装、备份恢复和正常重启链路继续通过；数据库连接、分页、五种字段筛选、105 个用户集合的跨批次枚举、视图、特殊集合名称及字面量搜索可用。验证 Long 9007199254740993、Decimal128 12.50 和日期保留 BSON 类型；中文大文档正确截断。错误版本、缺失集合、非法字段/数字/偏移、错误数据目录和跨线程生命周期冲突均拒绝；改变设置端口与默认版本后仍连接实际运行的原实例。测试实例与临时文件已回收，用户实例和业务数据没有改动。结果代表本机 Windows，未声称 Mac 实机通过。

离线 Chromium 使用实际数据库页函数、MongoDB 组件、React/Radix 与当前源码生成的验证样式，通过 37 项交互及状态检查，无 pageerror；核对 50 组中英文字段。1440px 中文、390px 中文、320px 英文截图已目检，长集合名、键盘展开、错误重试、分页、筛选、实例切换、缺失 Shell 和大文档复制限制均已检查。夹具隔离了其他数据库工作区、导航、剪贴板和 IPC，不将这些替代边界声称为完整桌面实机验收。Web/schema TypeScript、Rust 全工作区 all-targets 与 diff 检查通过，未启动前端 dev 或执行正式前端 build。

十一处版本文件同步至 0.2.112，Cargo.lock 仅更新三个本项目 crate；发布范围为二十一个文件，保留用户原有未跟踪文件与本地产物。上一版 v0.2.111 的 Windows、macOS Apple Silicon、macOS Intel Release 均已确认 completed/success。本轮新增 annotated tag v0.2.112，与 main 一次原子推送，随后核对远程指向与 release.yml 实际状态。本次没有业务数据库变更，未修改 update.sql；整体产品完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/mongodb-management-and-usage
参考：https://www.mongodb.com/docs/mongodb-shell/write-scripts/
参考：https://www.mongodb.com/docs/manual/reference/method/db.getCollectionInfos/
参考：https://www.mongodb.com/docs/mongodb-shell/reference/ejson/

## 第一百二十五轮：MongoDB 图形备份与整库恢复（v0.2.113）

继续对照 ServBay 的 MongoDB 备份恢复说明，补齐官方 Database Tools 已可安装但数据库页没有操作入口的缺口。新增独立备份卡片，选择真实业务数据库创建 gzip BSON archive，查看时间、数据库、大小、来源服务器版本、工具版本及校验摘要；服务停止时仍可查看已有记录。恢复支持新数据库或替换已有数据库，已有目标从真实列表选择；先检查备份与目标，再输入目标名称确认暂停应用写入并执行恢复。复用 React Query、Radix、现有卡片与通知，无新增依赖。

后端新模块复用 mongosh 的本机进程归属、实际运行端口、版本和真实数据目录核对。备份及恢复持有生命周期锁和数据目录活动守卫，官方工具使用固定参数，数据库名作为参数值传递，不接受脚本或任意主机。只支持业务数据库，拒绝系统库、控制字符、路径与命名空间通配符、超过 63 UTF-8 字节的名称；检测大小写冲突。归档与元信息先写入临时目录，成功后同步文件并发布备份记录，记录 SHA-256、大小及确切工具版本；失败的临时结果不作为成功备份展示。损坏或缺失记录在列表中计数提示，原文件保留。

恢复先将归档复制到独立临时目录并再次校验摘要；要求相同 MongoDB 主版本，并使用创建归档时的已安装 Database Tools 版本。预览 revision 绑定备份记录、目标名、实际实例端口/PID、数据库存在状态及集合名称/类型/UUID。通过官方 mongorestore dry-run 后，为已有目标创建完整保护备份，再次确认目标结构后清空整库并恢复，包括删除归档中不存在的旧集合。保留索引、视图与 BSON 类型；恢复不是数据库事务，不冒充原子替换，中途失败提示可能存在部分数据并保留保护备份供再次恢复。普通 mongodump 不宣称跨集合一致性快照，界面明确要求应用暂停写入。当前入口处理本应用创建的归档，外部归档导入导出、备份删除、计划任务和认证配置仍待继续补齐。

扩展既有 MongoDB 原生验收函数，未创建测试文件。Windows MongoDB 8.0.4、mongosh 2.12.0 与 Database Tools 100.19.0 最终通过（1 passed，81.57 秒）：实际安装/浏览/正常停机重启链路继续通过；新库恢复、已有库替换、保护备份再次恢复均由独立 Node MongoDB 驱动读取核对。覆盖 105 个空集合、特殊集合名、普通文档、大文档、Int64/Decimal128/日期、索引及视图；额外旧集合确实删除，保护备份保留恢复前数据。保护备份工具不可用及损坏归档 dry-run 失败均在清空目标前拒绝，目标内容保持；摘要篡改、不同主版本、缺少原工具版本、过期 revision、错误确认名、系统库、非法名称、路径穿越、错误 dataDir 和并发生命周期锁均被拒绝。修复实测发现的 mongosh UUID 直接 toString 不兼容，改用 EJSON 序列化。自有临时实例、端口与数据已回收；未对用户业务数据库写入，尚无 Mac 实机验收。

离线 Chromium 装载实际数据库页、MongoDB 组件、React/Radix 与当前源码样式，通过 25 项交互/状态检查、33 组中英翻译核对和备份 schema 检查，无 pageerror。覆盖缺工具恢复、重复提交、新库重名拦截、字节限制、覆盖说明、确认名、失败保留草稿与重新检查、执行中禁止关闭、实例变化禁用、保护备份列表及服务停止状态。1440px 桌面、390px 中文与 320px 英文恢复弹窗已目检；虚线保留左右间距、正文滚动、底部按钮固定可达，检查完成后聚焦确认输入，无横向溢出。夹具控制 API、导航及剪贴板边界，不将该验证视作完整桌面 IPC 实机测试。

十一处版本文件同步至 0.2.113，Cargo.lock 仅更新三个本项目 crate。发布前执行 Web/schema 类型检查、Rust 全工作区 all-targets 与 diff 检查；不运行前端 dev 或正式 build。上一版 v0.2.112 的 Windows、macOS Apple Silicon、macOS Intel Release 均已确认 completed/success。本轮按约定新增 annotated tag v0.2.113，与 main 原子推送并核对远程和 release.yml 状态，保留用户原有未跟踪文件与本地产物。本次没有业务数据库变更，未修改 update.sql；整体产品完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/mongodb-management-and-usage
参考：https://www.mongodb.com/docs/database-tools/mongodump/mongodump-behavior/
参考：https://www.mongodb.com/docs/database-tools/mongodump/
参考：https://www.mongodb.com/docs/database-tools/mongorestore/mongorestore-examples/

## 第一百二十六轮：MongoDB 外部归档导入导出与备份清理（v0.2.114）

沿用 ServBay 的官方 MongoDB 工具链与文件备份流程，补齐备份只能留在应用目录、无法从外部归档迁入和清理旧记录的问题。备份列表新增原生文件选择导入、另存为导出、打开目录及更多操作菜单；正常备份和损坏/缺失记录均可预览删除范围。导入使用归档中的真实库名下拉选择，单库自动选中，多库要求选择；展示源路径、服务器和工具版本、集合数、大小及摘要，明确保存副本与实际恢复是两个步骤。浏览器演示明确标为内存操作，不冒充实际文件保存。

新增专用 MongoDB archive v0.1 流式读取模块，按官方格式解析魔数、BSON 元信息、命名空间段、终止符与 CRC-64-ECMA。自动识别 gzip 和未压缩 archive，读取真实 server_version/tool_version，检查全部集合的结束状态及 CRC；限制单个 BSON 大小、元信息总量、集合数量和处理时长。不执行归档内容，不解析成 JavaScript，也不需要运行数据库。导入多库归档时按选中数据库提取原始 BSON 与元信息，并重新写入标准 gzip archive；其他数据库不会留在导入副本中。兼容视图与时间序列的命名空间规则，8.3 起的无视图时间序列按来源服务器版本判断；实际原生验收覆盖 MongoDB 8.0.4，未宣称已实测 8.3。

导入预览 revision 绑定规范源路径、源文件大小、SHA-256 和归档元信息；提交时重新核对并将源文件复制到临时目录，再从固定副本提取。成功后同步归档与记录并发布，保留外部来源标记，导入过程中不修改运行数据库。导出前校验保存记录、归档结构、版本和 SHA-256，将标准 gzip archive 保存到应用数据目录以外，原子落盘且不覆盖已有文件。路径检查拒绝穿越、符号链接、目录联接及特殊文件名，流式读取检查普通文件及读前读后状态；文件动作无需已安装 MongoDB 或 Database Tools。

删除预览绑定两个受管文件的实际摘要，可清理缺失归档或损坏记录；记录变化需重新确认，保护备份额外提示失去恢复前副本。删除前将已确认目录暂存并再次核对，仅删除 archive.gz、metadata.json 与空目录，不递归扩展范围；存在用户额外文件时拒绝删除，清理失败保留剩余内容与明确路径。导入、导出、删除以及原有创建/恢复操作加入应用后台任务登记和现有生命周期/数据目录守卫。恢复前还新增对归档内真实数据库、版本与记录的一致性检查，避免仅凭被修改的记录选择错误命名空间；复制出的恢复临时归档也再次检查。

复用锁文件已有的 flate2 1.1.10 作为直接依赖，以流式处理 gzip；仅增加 nsb-core 的依赖关系，没有升级任何第三方包。没有新增测试文件，扩展既有 MongoDB 原生验收。Windows MongoDB 8.0.4、mongosh 2.12.0、Database Tools 100.19.0 最终通过（1 passed，86.32 秒）：实际导出、压缩/未压缩归档识别、多库选择提取、导入后新库恢复可用；独立 Node 驱动逐集合核对普通文档、大文档、BSON 类型、索引、视图和时间序列及底层集合。恢复前保护备份及原有生命周期验收继续通过。CRC 损坏、截断、附加无效数据、非法 BSON 长度、归档与记录版本不符、源文件变化、过期导入/删除确认、导出重名、应用目录内导出、额外用户文件与缺失记录均有对应拦截或清理验收。另用没有安装记录或运行服务的独立 CoreState 实测导入、导出和删除。临时实例、数据和端口已回收，未写入用户业务数据库；Mac 尚未实机验收。

离线 Chromium 使用实际数据库页和组件、React/Radix 与当前源码样式，通过 28 项交互/状态检查及新增 21 组中英翻译检查，无 pageerror。原生文件对话框、IPC、导航和剪贴板由夹具控制，仅验证选择结果与命令参数交接，不冒充桌面实机验收。1440px 桌面、390px 中文导入和 320px 英文长名称截图已目检；长库名完整换行，正文可滚动，底部按钮可达，没有横向溢出。验证导入取消、源文件变化后重新检查、导出取消与重名提示、保护备份说明、取消优先聚焦、删除冲突重查、损坏记录清理和停止实例下文件操作；下拉分隔线为虚线且两侧留白。

十一处版本文件同步至 0.2.114，Cargo.lock 仅含三个本项目 crate 的版本与上述直接依赖关系。发布前执行 Web/schema 类型检查、Rust 全工作区 all-targets 与 diff 检查，不运行前端 dev 或正式 build。上一版 v0.2.113 的三个平台 Release 均确认 completed/success；本轮按约定新增 annotated tag v0.2.114，与 main 原子推送并核对远程与 release.yml。保留用户原有未跟踪文件和本地产物。本次没有业务数据库变更，未修改 update.sql；MongoDB 计划备份、认证管理及整体产品完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/mongodb-management-and-usage
参考：https://github.com/mongodb/mongo-tools/blob/master/common/archive/spec.md
参考：https://github.com/mongodb/mongo-tools/blob/master/common/archive/archive.go
参考：https://github.com/mongodb/mongo-tools/blob/master/common/archive/demultiplexer.go

## 第一百二十七轮：MongoDB 计划备份与安全轮转（v0.2.115）

沿用前轮已核对的 ServBay MongoDB 管理指南与项目既有数据库计划流程，补齐 MongoDB 自动备份。支持按安装版本保存每日、每周、每月计划，本地时间执行、短月取月末、保留 0–100 份，以及关闭计划时立即执行。复用现有每 30 秒调度线程、启动交接、settings 存储、后台任务登记和数据目录守卫，没有新增调度线程或依赖。应用关闭期间不执行，重新打开后到期计划补执行一次；实例未运行、工具缺失或服务忙会记录失败，推进下一周期，不连续重复尝试。

运行前持有该版本的操作系统文件锁并重新读取计划，防多窗口重复执行；整个备份阶段持有服务生命周期锁。固定 mongosh 脚本读取完整业务库列表，不使用浏览器 1000 库预览上限，排除 admin、local、config。逐库调用官方 mongodump 生成 gzip archive，发布前核对 SHA-256、归档结构、版本与数据库。每完成一份立即保存标识，最终记录完成时间、成功/部分完成/失败/跳过状态；中断通过文件锁状态判断，已生成文件仍可查看。

自动副本使用独立 automatic 标记。新副本完整校验后，只轮转同来源版本、同数据库的有效自动副本，并优先保留本次文件，避免系统时钟回拨导致误删。0 表示保留全部；手动、恢复前和导入副本不受影响。损坏记录、摘要不符或含额外用户文件的目录保留并报告，不计入有效保留数量。清理沿用摘要确认和暂存机制，仅处理受管文件，不递归扩大删除范围；备份成功但清理失败显示部分完成，保留新副本。

复用 DatabaseBackupPlan 和日历表单，加入 MongoDB API、Tauri 命令、schema 与浏览器内存预览。面板显示下一次执行、上次状态、保留份数和本次生成文件，列表区分自动副本。计划编辑或运行与文件操作互斥，独立管理父子锁状态，避免设置弹窗把自己的保存按钮禁用；版本、端口或 PID 变化后拒绝提交旧草稿。新增 MongoDB 中英文范围和保留说明，明确安装 Shell/Database Tools、暂停应用写入及逐库归档不保证跨集合/跨库一致快照。共享计划弹窗底部分隔线使用虚线且两侧留白。

没有新增测试文件，扩展既有 Rust 验证。Windows MongoDB 8.0.4、mongosh 2.12.0、Database Tools 100.19.0 原生综合验收通过（1 passed，122.06 秒）：多业务库自动归档、未来计划不执行、到期仅执行一次、关闭后手动运行、自动归档实际恢复、保留 0/1、未来时间戳、按库和版本隔离、保留手动/保护/导入副本、损坏和额外内容保留、缺少工具、错误 dataDir 与生命周期冲突。现有日历/轮转/执行锁验证通过，扩展 MongoDB 文件锁、版本隔离、中断和无服务失败状态。临时实例与数据由既有 guard 回收，未写入用户业务数据库；macOS 未实机验收。

离线 Chromium 使用实际页面和组件，通过 26 项计划交互检查及 28 项原有导入导出回归，无 pageerror。IPC 和原生文件对话框由受控夹具提供，界面验收不冒充桌面原生调用。核对停止实例配置、日周月联动、保留范围、失败后草稿保留、重复执行防护、实例变化、后台运行互斥、部分完成与中断提示、自动副本刷新及中英翻译；1440px 桌面和 320px 中英弹窗已目检，无横向溢出，正文滚动且操作可达。

十一处版本文件同步至 0.2.115，Cargo.lock 仅更新三个本项目 crate 版本，无第三方依赖变化。发布前执行 Web/schema 类型检查、Rust 工作区 all-targets 和 diff 检查，未启动前端 dev 或正式 build。上一版 v0.2.114 的 Windows、macOS arm64/x64 Release 已确认 completed/success。本轮新增 annotated tag v0.2.115，与 main 原子推送并核对远程及 release.yml 实际状态；保留用户已有未跟踪文件与本地产物。本次没有业务数据库变更，未修改 update.sql。认证管理与整体产品完善目标继续进行。

参考：https://support.servbay.com/database-management/getting-started/mongodb-management-and-usage
参考：https://github.com/mongodb/mongo-tools/blob/master/common/archive/spec.md

## 第一百二十八轮：MongoDB 认证与本机账号管理（v0.2.116）

参考 ServBay MongoDB 管理页面中对本机绑定、图形化参数、mongosh 和 Database Tools 的组织方式，以及 MongoDB 官方 access control、createUser、mongodump 配置文件文档，补齐 NiceEnv 之前缺少的 MongoDB 认证流程。面板按版本显示运行认证状态、启动配置、管理账号和异常；停止实例时只读显示并禁用修改，启动后支持验证连接、无用户实例初始化 admin root 管理员、开启认证、关闭认证和修改管理员密码。

初始化管理员仅允许在尚未启用认证且没有用户的实例进行。管理员创建前后分别验证，成功后保存版本隔离的本机凭据，开启认证配置并按用户确认重启；已有用户或管理员不会被覆盖。开启、关闭认证和修改密码都要求修订号、当前实例版本、管理员权限和明确确认，关闭认证另需确认本机其他程序可能免密访问。重启失败保留已保存配置与凭据，不自动回退为无认证；运行状态与启动配置不一致时提示重新检查。

mongosh 查询通过标准输入接收临时 JSON，固定脚本不再包含密码；认证错误只返回可修复的人话提示。mongodump/mongorestore 使用 MongoDB Database Tools 的临时配置文件传入密码，密码不进入进程参数、URI、环境或归档目录，临时文件在命令完成后清理。备份、恢复、计划备份和文档浏览统一复用当前版本凭据；无认证实例仍允许匿名连接，认证开启后匿名读取被拒绝。错误详情会脱敏，认证凭据、认证开关和 MongoDB 计划均视为本机设置，不进入配置导出或导入，也不能通过通用设置命令直接写入。

共享 MongoDB 管理页增加认证卡片，并与备份卡片互斥；认证弹窗使用结构化账号、认证数据库、密码、重试和确认控件，不要求用户填写 JSON。密码字段默认隐藏，错误保留输入并支持重新检查；底部操作区保留虚线分隔和左右留白。中英文文案覆盖初始化、开启/关闭影响、密码规则、权限不足、认证失败、重启失败和浏览器演示说明。浏览器 mock 新增认证命令面，桌面端通过独立 Tauri 命令调用真实后端。

没有新增测试文件，扩展既有 MongoDB 原生验收。Windows MongoDB 8.0.4、mongosh 2.12.0、Database Tools 100.19.0 验证了无认证初始状态、错误凭据不落盘、管理员初始化、认证后浏览/备份/恢复/计划备份、匿名拒绝、密码变更、旧密码拒绝、关闭认证、损坏本机记录修复、导出不包含凭据、生命周期互斥和停机清理。认证新增的受控重启使日志断言改为检查至少三次完整停机；不放宽进程身份和无损停机验证。离线 Chromium 验收覆盖停止态、初始化确认、密码长度、认证失败重试、关闭认证双确认和窄屏无溢出；未启动前端 dev 或正式 build，macOS 未实机验收。

本次没有业务数据库变更，未修改 update.sql。发布时同步十一处版本文件并创建新的 annotated tag，保留用户已有未跟踪文件与本地产物。

参考：https://support.servbay.com/database-management/getting-started/mongodb-management-and-usage
参考：https://www.mongodb.com/docs/manual/core/authorization/
参考：https://www.mongodb.com/docs/manual/reference/command/createUser/
参考：https://www.mongodb.com/docs/database-tools/mongodump/

## 第一百二十九轮：服务配置直达、状态历史与清单状态（v0.2.117）

补齐服务管理和套件管理中几个影响日常操作的缺口：服务卡片和紧凑列表为 Nginx、PHP、MySQL、Redis、Apache 与 mihomo 提供配置直达入口，按服务版本匹配真实配置文件并复用已有配置读取、校验、备份和回滚流程；日志页增加当前应用会话内的服务状态历史，支持服务筛选、关键词搜索、刷新和失败原因查看；套件页增加从官方上游刷新可安装版本目录的入口，设置页展示当前生效的内置或远端清单、修订号、条目数和用户模块解析状态。

MongoDB 本机认证管理补充管理员密码恢复流程，旧密码无法验证时由受管实例执行受控的无认证更新、恢复认证并重启，保留版本隔离、生命周期锁和敏感信息不进入命令行的约束。桌面端新增状态历史和清单状态命令的前端接入，浏览器预览同步提供对应的可操作反馈；所有新增文案覆盖中英文，窄屏弹窗和下拉分隔线沿用统一的左右留白与虚线样式。

没有新增测试文件，发布前执行 Web/schema 类型检查、Rust 全工作区 all-targets 与 diff 检查；未运行前端 dev 或正式 build。本次没有业务数据库变更，未修改 update.sql。按约定同步版本文件并新增 annotated tag v0.2.117，保留用户已有未跟踪文件与本地产物。

## 第一百三十轮：数据库配置编辑器与 Redis 工作区整理（v0.2.118）

补齐 MariaDB、PostgreSQL 和 MongoDB 的版本级配置编辑入口，配置路径、版本校验、历史备份、冲突检测、回滚目标和基础语法检查均接入现有配置编辑器。MongoDB 启动时自动创建并加载对应版本的 `mongod.conf`，端口、数据目录、日志路径、本机绑定和认证开关继续由 NiceEnv 的托管启动参数控制；无认证启动显式传 `--noauth`，避免用户 YAML 中残留的认证设置造成 UI 状态与实际实例不一致。服务卡片配置入口会带上实际版本，配置编辑器增加 ini/YAML 语法高亮，浏览器 mock 同步覆盖三类数据库配置。

数据库页将 Redis 从 MySQL/MariaDB 与 PostgreSQL 的重复卡片中拆出独立工作区，保留原有统计、连接认证、持久化和备份能力；无其他数据库实例时可直接进入 Redis 页签。没有新增测试文件或数据库变更，未修改 update.sql；发布前执行 Web/schema 类型检查、Rust 全工作区 all-targets 与 diff 检查，不运行前端 dev 或正式 build。按约定同步版本文件，新增 annotated tag v0.2.118，并用原子推送同步 main 与 tag，保留用户已有未跟踪文件与本地产物。整体产品完善目标继续进行。

## 第一百三十一轮：Temporal CLI 与 Neo4j 管理台入口（v0.2.119）

服务管理台入口继续按清单和官方默认行为收紧：Temporal CLI 仅在启动参数明确托管 gRPC 端口与 Web UI 端口（`--port {port}`、`--ui-port {port+1000}`）时生成 `port+1000` 的本机 Web UI 地址；Neo4j 仅在 `console` 模式且使用默认 HTTP 端口 7474 时生成内置 Browser 地址 `/browser`。服务卡片、服务列表和套件页共享入口按钮，无法确认入口时仍由后端返回明确提示，不猜测用户自定义命令的端口。

没有新增测试文件或业务数据库变更，未修改 update.sql；扩展既有 Rust 管理台目标测试，执行 Web/schema 类型检查、Rust 定向测试与全工作区检查、diff 检查，不运行前端 dev 或正式 build。按约定同步版本文件，新增 annotated tag v0.2.119，并用原子推送同步 main 与 tag，保留用户已有未跟踪文件与本地产物。

参考：https://docs.temporal.io/cli/command-reference/server
参考：https://docs.temporal.io/develop/run-a-development-server
参考：https://neo4j.com/docs/browser/operations/dbms-connection/

## 第一百三十二轮：服务缺失依赖直达安装（v0.2.120）

服务卡片和紧凑服务列表现在会把清单中缺失的依赖显示为可点击的套件链接，点击后直接打开套件页并按依赖 ID 初始化搜索，减少用户在服务页和套件页之间手动查找的步骤。列表布局对长依赖名和窄屏做了换行处理，保留警告状态与现有启停操作；套件页只在首次挂载时读取 URL 搜索参数，用户后续编辑搜索不会被覆盖。

没有新增测试文件或业务数据库变更，未修改 update.sql；发布前执行 Web/schema 类型检查和 diff 检查，不运行前端 dev 或正式 build。按约定同步版本文件，新增 annotated tag v0.2.120，并用原子推送同步 main 与 tag，保留用户已有未跟踪文件与本地产物。

## 第一百三十三轮：缺失依赖服务操作前置拦截（v0.2.121）

服务卡片、紧凑服务列表和批量操作现在会在启动或重启前拦截缺失依赖：停止态服务会禁用启停开关，批量启动和重启会在选择缺依赖服务时禁用，并明确提示先安装依赖。依赖名称继续直达套件页搜索，已运行中的服务仍可执行停止，端口冲突处理也遵守同一前置条件。

没有新增测试文件或业务数据库变更，未修改 update.sql；发布前执行 Web/schema 类型检查、Rust 全工作区 all-targets 检查和 diff 检查，不运行前端 dev 或正式 build。按约定同步版本文件，新增 annotated tag v0.2.121，并用原子推送同步 main 与 tag，保留用户已有未跟踪文件与本地产物。
