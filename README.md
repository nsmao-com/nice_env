# NiceServBay

**Windows + macOS 一站式本地开发环境管理器** — 装一个桌面 App，点几下就能跑站点。
对标 ServBay / FlyEnv / phpStudy，交互与视觉全面现代化（Linear / Raycast 气质）。

Tauri 2（Rust 后端能力）+ Next.js 16（静态导出）+ Tailwind v4 + shadcn/ui + Motion。

## 快速开始

```bash
# 开发
pnpm install
pnpm dev                 # 前端 dev (localhost:3000)
pnpm tauri dev           # 桌面壳（自动起前端）

# 打包（Windows NSIS）
pnpm tauri build

# 无头验收（完整闭环，独立目录+安全端口，不碰系统环境）
./target/release/niceservbay.exe --smoke-test

# 前端类型检查 + 纯逻辑单测（日志高亮解析：无损/级别/状态码）
pnpm --filter @nsb/web check
pnpm --filter @nsb/web check:logic
```

> 冒烟测试会**强制切到安全端口档**再跑：应用默认走标准端口（80/3306/6379），
> 而开发机上往往已经跑着真实的 MySQL/Redis，不隔离就会互相打架。
> 它同时会关掉「自动释放端口」——冒烟过程中绝不结束任何进程。

## 架构

```
apps/desktop/src-tauri   Tauri 2 壳：命令接线(55+) / 托盘菜单 / 窗口行为 / --smoke-test
apps/web                 Next.js App Router (output:'export')：10 个页面 + 向导 + 命令面板
packages/schema          Zod schema（TS 侧唯一事实，Rust serde 对齐）
crates/core              服务管理 / 下载器 / hosts / TLS / 站点 / 服务栈 / 端口诊断 / 统计 / Clash
crates/platform          Windows Job Object / 系统代理(注册表+wininet) / hosts 标记块 / 提权
manifest/                套件清单（真实 URL + sha256，可远程更新）
```

**Rust 侧核心模块**（均无 Tauri 依赖，可独立测试）：

| 模块 | 职责 |
|------|------|
| `download.rs` | 断点续传(Range) + sha256 校验 + 取消 + 进度事件 |
| `services.rs` | 服务状态机、Job Object 进程树、日志采集(ring+文件)、健康检查、端口方案(档位+逐个覆盖) |
| `ops.rs` | **内置编排**服务启停：nginx(校验+重启策略) / php-cgi 端口池 / mysql(初始化+设密) / redis / mihomo |
| `generic.rs` | **清单驱动**的通用服务启停：清单声明 `run` 即可启停，无需写 Rust 分支 |
| `stacks.rs` | **服务栈**：内置预设 + 用户自定义组合，一键按序启动/逆序停止，逐项失败回报 |
| `ports.rs` | 端口诊断 + 区间扫描 + 结束占用者（自有服务走优雅停止，外部进程才 kill） |
| `configgen.rs` | nginx.conf / php.ini / my.ini / redis.conf / mihomo.yaml 生成（写前备份） |
| `sites.rs` | 站点 CRUD → vhost + hosts + 证书 + 建库 + .env.example |
| `transfer.rs` | 配置导入/导出（站点 + 设置 + 端口覆盖 + 服务栈 + 订阅） |
| `tls.rs` | rcgen 根 CA + 站点证书（SAN 多域名）+ certutil/security 信任 |
| `proxy.rs` | mihomo 生命周期、订阅导入(端口接管)、REST API、系统代理(带恢复) |

### 服务如何扩展（两种路径）

**1. 内置编排**（`ops.rs`）——需要特殊处理的少数服务，如 nginx 的 `-t` 校验 + reload、
MySQL 的 `--initialize-insecure` + 设密、php-cgi 的端口池。写 Rust 分支。

**2. 清单声明**（`manifest/*.json` 的 `run` 字段）——绝大多数服务走这条：
只需在清单条目里加一段 `run` 描述，App 就自动获得「安装 → 注册 → 启停 → 健康检查 → 日志」。

```jsonc
{
  "id": "caddy", "version": "2.11.4", "category": "web-server",
  "entry": "caddy.exe", "defaultPort": 8080,
  "run": {
    "args": ["run", "--config", "{etc}/Caddyfile", "--adapter", "caddyfile"],
    "health": "tcp", "healthTimeoutSec": 12,
    "configFile": "Caddyfile",            // 首次启动按模板生成配置
    "configTemplate": ": {port} {\n\troot * {data}/www\n\tfile_server\n}\n",
    "initDirs": ["www"],                  // 启动前自建数据目录
    "stopArgs": ["stop", "--config", "{etc}/Caddyfile"]  // 优雅停止
  }
}
```

占位符：`{root}` 程序目录 · `{data}` 数据目录 · `{etc}` 配置目录 · `{bin}` 可执行文件 ·
`{port}` 分配端口 · `{port+N}` 派生端口（MinIO 控制台、Temporal UI） · `{httpPort}` 当前站点端口 · `{log}` 日志。

`run` 还支持：`singleInstance`（多实例服务如 PHP/MySQL 置 false）、`initArgs`/`initBin`
（首次初始化，如 MariaDB 的 `mariadb-install-db`）、`requires`（依赖提示）、
`env`（环境变量模板）、`health`（tcp / process / none）。

**端口策略**：安全档下清单服务自动使用 `defaultPort + 20000` 起的空闲端口，
与系统及其它环境（FlyEnv/XAMPP）和平共存；标准档直接用清单端口。

当前清单覆盖 **13 个类别、70 个包、46 个可启停服务**：
Web 服务器（Nginx ×4 版本 / Apache / Caddy / FrankenPHP / Tomcat / RoadRunner）、
语言运行时（PHP 7.2–8.5 / Node / Python / Go / Java / .NET / Bun / Deno / Ruby / Rust / Zig / Flutter / Perl / Erlang）、
数据库（MySQL / MariaDB / PostgreSQL / MongoDB / Qdrant / Neo4j）、缓存与队列（Redis / Memcached / RabbitMQ）、
搜索引擎（Meilisearch / ZincSearch / Elasticsearch）、对象存储（MinIO / RustFS）、
服务治理（Consul / etcd / R-Nacos / Temporal）、AI（Ollama）、邮件（Mailpit）、DNS（CoreDNS）、
FTP（SFTPGo）、隧道（Cloudflare Tunnel）、工具（Composer / Adminer / mihomo）。

## 端口约定（默认「标准档」，可在设置切换安全档）

| 服务 | 标准档（默认） | 安全档 |
|------|--------------|--------|
| Nginx | 80 / 443 | 8080 / 8443 |
| Apache | 8080 / 8443 | 8180 / 8444 |
| MySQL | 3306 | 23306 |
| PostgreSQL | 5432 | 25432 |
| MongoDB | 27017 | 28017 |
| Redis | 6379 | 26379 |
| php-cgi 池 | 9100–9199（自动分配） | 同左 |
| mihomo | 17890 / 控制 19090 | 同左 |

**默认走标准档**：项目里写死的 `127.0.0.1:3306`、`redis://localhost:6379` 这类连接串开箱即用，
不用为了连本地库去改代码。与本机已有环境冲突时，启动会明确报出「端口 X 已被 <进程名> 占用」，
可一键结束占用者，或切到安全档（避开 80/3306/6379，与 FlyEnv/XAMPP 和平共存）。

**逐个端口都能改**：设置 → 端口里每一项都能覆盖成任意值（覆盖值优先于档位默认值，
清空即恢复）。启动前探测到的冲突会把**端口号 + 占用进程 pid** 一起带回前端，
所以错误提示上直接就有「结束占用并重试」按钮，不必自己去翻是哪个端口。

## 服务栈（一键启动你的一整套服务）

在「服务栈」页把常用组合存成一个配置，之后一次点击全部拉起——不用每次逐个点开关。

- **内置预设**：LNMP 经典 / 前端静态站点 / 数据栈。预设不可删改，可「复制成我的」再改。
- **自定义**：选服务、拖顺序（↑↓）。启动严格按顺序串行——数据库和 PHP 先起，Nginx 最后，
  否则 nginx 起来时 PHP 端口池还没就绪。
- **逐项回报**：某一项失败不阻断后面的项；失败项单独列出，端口冲突项直接给「结束占用并重试」。
- **入口不止一处**：总览页「启动常用栈」按钮、`Ctrl+K` 命令面板、**托盘右键菜单**都能一键拉起；
  设置里还能选「启动应用时自动拉起某个栈」。
- 栈只是有序服务 id 列表，启动仍然走 `ops::start_service`——
  nginx 的 reload、MySQL 的初始化、php 端口池这些关键处理一个都不会绕过。

## 端口冲突与进程治理

- **启动前预检**：每个服务启动前先探测自己的端口，被占时直接返回「端口 X 已被 <进程名> 占用」
  的人话错误（含 pid），覆盖 Nginx/Apache 的 **HTTP + HTTPS**、MySQL、PostgreSQL、MongoDB、
  Redis、mihomo 的**混合端口与控制端口**——配置里会无条件 bind 的端口一个都不漏。
- **自动释放（默认开，可关）**：启动服务前若端口被占，默认先把占用者收掉再启动。
  占用者是本应用自己的服务时走**优雅停止**（MySQL 干净关库、nginx 先 reload 停），
  是外部程序才直接结束；**结束过哪些端口会明确 toast 告知**，不会悄悄杀掉别人的进程。
  关掉「设置 → 启动 → 启动服务前自动释放被占用的端口」即恢复为纯报错。
- **一键结束占用并重试**：端口冲突的错误提示自带按钮，点了就结束占用者再重试启动。
- **工具箱 → 端口查询 / 结束进程**：查某个端口或一段范围被谁占用（进程名 + pid + 命令行），
  属于本应用服务的一键「停止服务」（优雅），外部程序「结束进程」；结束前二次确认。
- **php-cgi 端口池**：分配时整池预检；记录过的池若已被外部程序占用会自动重新分配
  （旧版会永远复用失效记录，导致该版本 PHP 再也起不来）。
- **全量体检**：工具箱 → 端口扫描「开始体检」一次列出本应用所有待绑定端口及其占用者，
  三态结论 `free / self（本应用在跑）/ conflict（被他人占用）`；仪表盘异常卡同样走这个接口。
  冲突行可直接结束占用进程（需确认，且只杀用户点选的那个 pid）。
- **不会误杀**：除「自动释放」这条显式开关外，诊断与结束进程都是「用户点选特定 pid」才执行；
  本应用从不主动杀非自己拉起的进程。
- **退出/崩溃清理**：所有子进程挂在 Windows Job Object（`KILL_ON_JOB_CLOSE`）/ Unix 进程组，
  正常退出无孤儿。启动时另有一道兜底：读取上次会话落的 `data/run/pids.json`，
  仅当「写入者已死 + 该 pid 仍存活 + 其可执行文件位于本应用 runtimes 目录内」三条同时满足才回收。
- **停止不谎报**：停止后逐个确认进程真的退出；仍有残留则报错并在 UI 显著提示，
  而不是把状态写成已停止（旧版会因此再也停不掉）。停机命令打向**启动时实际使用的端口**，
  运行期切换端口方案不会导致 shutdown 打空、只能强杀。

## 日志

日志页不再是一坨等宽白字，每行按语义着色：

- **级别高亮**：时间戳弱化、`[error]/[warn]/[notice]` 按级别配色，行首 2px 色条让错误行一眼可见。
- **HTTP 状态码分色**：2xx 绿 / 3xx 蓝 / 4xx 黄 / 5xx 红；请求方法、路径、IP、耗时、`key=value` 各自成色。
- **级别过滤 + 计数**：全部 / 错误 / 警告三个按钮带实时条数徽标。
- **关键字搜索**：命中的字串直接高亮，可叠加在级别过滤上。
- **暂停 / 自动滚动 / 自动换行 / 复制**：手动往上滚会自动停掉跟随，不会「看不到自己在读什么」；
  暂停时明确标注「显示的是暂停前的日志」，不假装是实时的。
- 拉取行数（默认 500）与是否默认自动刷新都在设置里可调。
- 解析是**无损**的：高亮只做着色，复制出来的内容与原始日志逐字节一致（有单测守着）。

## 托盘

托盘右键菜单按用途分组，并且**带状态**：

- **一键启动**子菜单：列出你的服务栈，标题直接写「N 个服务运行中」，点一下起整栈。
- **服务**子菜单：列出所有可启停服务（带监听端口），运行中的打勾，点一下启停。
- **站点**子菜单：直接列出站点名，点了用默认浏览器打开对应 URL（按当前端口档拼好）。
- **导航**：总览 / 套件 / 端口工具 / 日志 / 设置，点了把窗口带到前台并跳页。
- **图标带角标**：右下角小圆点显示「有服务在运行」，不用点开就知道环境起没起来。
- 左键单击图标 = 把主窗口叫到前台（比开菜单快一步）。
- 服务状态变化时会重建菜单与图标，所以勾选态、计数、角标始终和真实状态一致。

## 设置

- 外观（主题 / 主题色 / 减少动效）、通用（默认域名后缀 / 默认 Web 服务器 / 语言）
- **端口**：档位切换 + 逐个端口覆盖（覆盖值优先于档位，清空即恢复）
- 启动（开机自启 / 关闭到托盘 / **启动时自动拉起某个服务栈** / **启动服务前自动释放被占端口**）
- 日志（每次拉取行数 / 默认自动刷新 / 结束进程前二次确认）
- 下载镜像源 + 远端套件清单地址（填了「检查更新」才能发现新版本）
- 数据目录（查看 / 打开 / 迁移）
- **配置导入 / 导出**：站点 + 已装套件清单 + 设置 + 端口覆盖 + 服务栈 + 代理订阅 → 单个 JSON；
  **支持拖拽**——把备份 JSON 拖到设置页任意位置即可导入（拖进来时全屏提示投放区），
  也可以走「导入配置」按钮选文件。导入只新增/覆盖，不删站点、不卸载套件；
  缺什么套件会在结果里列出来让你补装。
- 更新：应用版本 + 套件清单版本检查。


## macOS 状态（诚实说明）

macOS 侧已修掉的阻塞性缺陷：Unix 进程终止此前是空实现（`libc_kill` 占位返回 0），
导致 **macOS 上停止服务是静默无效的**，现已用 `libc::kill` + 独立进程组实现真正的
SIGTERM/SIGKILL 与整棵子树回收。同时修复：清单入口路径在 macOS 上被错误地按 `\` 拼接、
`.gz` 单文件包解压后按缓存文件名（`.pkg`）而非清单 entry 落盘、`mysqld --console`
（Windows 专属选项）在 macOS 上必然报 unknown option、`initdb -U root`
（macOS 拒绝 root 用户）、提权执行未做参数转义且忽略退出码。

**仍然没有解决的是二进制来源**：`manifest/packages.mac.json` 目前只有
MySQL / mihomo / Node / Go / MongoDB / Composer，**PHP、Nginx、Redis、Apache、PostgreSQL
没有官方 macOS 二进制**，因此这些服务在 macOS 上「装不了」而不是「跑不起来」。
需要补录候选源（如 `crazywhalecc/static-php-cli` 的 php-cgi 构建）或新增 `source-build`
安装方式后才谈得上 macOS 完整可用。

## 安装形态与数据目录

- NSIS 安装包（`target/release/bundle/nsis/`）为 **currentUser 模式**：安装向导可自选任意可写目录（默认 `%LOCALAPPDATA%\Programs\NiceServBay`）
- **数据跟随安装目录**：安装版把 `runtimes / etc / data / logs / certs / backup` 全部放在 `{安装目录}/nsb-data/`，整个目录可拷贝迁移；安装目录不可写时自动回退 LocalAppData
- 开发模式（cargo target 下运行）仍使用 `%LOCALAPPDATA%\NiceServBay`，`NSB_HOME` 环境变量可强制覆盖
- 设置页「数据目录」卡片实时显示真实路径（`get_data_dir`）

## 配置导入 / 导出

设置页「配置导入 / 导出」：一键把**站点 + 已装套件清单 + 全部设置 + 代理订阅**导出为单个 JSON；换机导入时站点配置自动重建（域名冲突跳过）、设置与订阅恢复，并列出待补装的套件清单。套件本体（运行时大文件）不随备份走。

## i18n（中英双语）

全部界面文案（含套件页/向导/工具箱/设置/命令面板/共享组件）已收编进 `src/lib/i18n.ts` 的 zh/en 双语字典；设置 → 语言切换即时生效（无残留硬编码中文，个别专有名词除外）。


## 套件目录（Windows 清单，均带 sha256 断点续传）

清单共 **161 个条目 / 49 个服务**，其中 **45 个服务提供多版本**
（版本下拉点选安装 / 切换；`revision=35`）。

| 服务 | 可选版本 |
|------|----------|
| Nginx | 1.28.0 / 1.27.5 / 1.26.3 / 1.24.0 |
| Apache | 2.4.68 / 2.4.66 / 2.4.63 |
| Caddy | 2.11.4 / 2.11.3 / 2.11.2 / 2.11.1 |
| FrankenPHP | 1.12.7 / 1.12.6 / 1.12.5 / 1.12.4 |
| RoadRunner | 2025.1.15 / 2025.1.14 / 2025.1.13 / 2025.1.12 |
| Tomcat | 11.0.26 / 11.0.25 / 10.1.60 |
| PHP | 8.5.10 / 8.4.25 / 8.3.33 / 8.2.33 / 8.1.34 / 8.0.30 / 7.4.33 / 7.3.33 / 7.2.34 |
| Node.js | 22.14.0 / 20.19.5 / 18.20.8 |
| Python | 3.13.7 / 3.12.9 / 3.11.9 |
| Go | 1.24.1 / 1.23.6 |
| Deno | 2.9.7 / 2.9.6 / 2.9.5 / 2.9.4 |
| Bun | 1.4.2 / 1.4.1 / 1.4.0 |
| Rust (rustup) | 1.29.1 / 1.28.1 / 1.27.1 |
| Zig | 0.16.0 / 0.15.1 / 0.14.1 |
| Erlang | 29.1 / 29.0.6 / 28.5.0.6 |
| Strawberry Perl | 5.42.3.1 / 5.42.2.1 / 5.40.5.1 |
| .NET SDK | 8.0.425 / 8.0.419 / 8.0.414 |
| Temurin JDK21 | 21.0.9+10 / 21.0.8+9 / 21.0.12.1+1 / 21.0.12+8 |
| Gradle | 9.7.1 / 9.6.1 / 8.14.3 |
| MySQL | 8.0.46 / 5.7.44 |
| MariaDB | 12.3.3 / 11.4.8 / 10.11.13 |
| PostgreSQL | 17.6 / 16.9 |
| MongoDB | 8.0.4 / 7.0.24 |
| Neo4j | 5.26.30 / 5.25.1 / 2025.09.0 |
| Qdrant | v1.19.1 / v1.19.0 / v1.18.3 / v1.18.2 |
| Elasticsearch | 9.5.4 / 9.4.7 / 8.19.4 |
| Meilisearch | v1.53.2 / v1.53.1 / v1.53.0 / v1.52.3 / v1.52.2 |
| ZincSearch | v1.0.0-beta3 / v1.0.0-beta2 / v1.0.0-beta1 / v0.4.10 |
| Redis | 5.0.14 / 5.0.10 |
| Memcached | 1.6.8 / 1.6.7 |
| RabbitMQ | 4.3.6 / 4.3.5 / 4.3.4 / 4.2.9 |
| MinIO | RELEASE.2025-09-07T16-13-09Z / RELEASE.2025-07-23T15-54-02Z / RELEASE.2025-07-18T21-56-31Z / RELEASE.2025-06-13T11-33-47Z |
| RustFS | 1.0.0 |
| Consul | 2.0.4 / 2.0.3 / 2.0.2 |
| etcd | v3.7.1 / v3.6.14 / v3.5.33 |
| rNacos | 0.8.7 / 0.8.6 / 0.8.5 / 0.8.4 |
| CoreDNS | 1.14.7 / 1.14.6 / 1.14.5 / 1.14.4 |
| Mailpit | v1.31.2 / v1.31.1 / v1.31.0 / v1.30.7 / v1.30.6 |
| SFTPGo | 2.7.6 / 2.7.5 / 2.7.4 / 2.7.3 |
| Cloudflared | 2026.9.1 / 2026.9.0 / 2026.8.3 / 2026.8.2 / 2026.8.1 |
| Ollama | 0.34.2 / 0.34.1 / 0.34.0 / 0.33.3 |
| Composer | 2.8.5 / 2.7.9 |
| Adminer | 6.1.0 / 6.0.2 / 6.0.1 / 6.0.0 / 4.8.1 |
| mihomo | 1.19.11 / 1.19.10 / 1.18.10 |
| k6 | 2.2.0 / 2.1.0 / 1.8.1 |
| Temporal CLI | 1.9.1 / 1.8.3 / 1.8.2 / 1.8.1 |

**单实例服务**（Nginx/Apache/MySQL/PG/Mongo/Redis/mihomo 等）支持「切换使用版本」：已装徽标间点击即切换，运行中会提示先停止；**多实例服务**（PHP 池、MySQL）逐版本独立启停，可并行。

**暂未提供多版本的服务及原因**（`flutter`, `ruby`, `ruby-devkit`, `rustfs`）：
清单条目里带 `note` 字段写明具体原因，例如 Ruby 的 Windows 包只有 `.7z`（安装管线暂只支持
zip/tar.gz），Flutter 的历史版本需按带时间戳的 release 目录拼接。不存在「有多个版本却没加」的情况。

新增服务验收：`cargo run --release -p nsb-core --bin check_extra`（Apache 真实执行 PHP / PG initdb+psql / Mongo 就绪 / 无孤儿，24 项）。

## 端口约定（默认「标准档」，可在设置切换安全档）

| 服务 | 标准档（默认） | 安全档 |
|------|--------------|--------|
| Nginx | 80 / 443 | 8080 / 8443 |
| Apache | 8080 / 8443 | 8180 / 8444 |
| MySQL | 3306 | 23306 |
| PostgreSQL | 5432 | 25432 |
| MongoDB | 27017 | 28017 |
| Redis | 6379 | 26379 |
| php-cgi 池 | 9100–9199（自动分配） | 同左 |
| mihomo | 17890 / 控制 19090 | 同左 |

**默认走标准档**：项目里写死的 `127.0.0.1:3306`、`redis://localhost:6379` 这类连接串开箱即用，
不用为了连本地库去改代码。与本机已有环境冲突时，启动会明确报出「端口 X 已被 <进程名> 占用」，
可一键结束占用者，或切到安全档（避开 80/3306/6379，与 FlyEnv/XAMPP 和平共存）。

**逐个端口都能改**：设置 → 端口里每一项都能覆盖成任意值（覆盖值优先于档位默认值，
清空即恢复）。启动前探测到的冲突会把**端口号 + 占用进程 pid** 一起带回前端，
所以错误提示上直接就有「结束占用并重试」按钮，不必自己去翻是哪个端口。

## 服务栈（一键启动你的一整套服务）

在「服务栈」页把常用组合存成一个配置，之后一次点击全部拉起——不用每次逐个点开关。

- **内置预设**：LNMP 经典 / 前端静态站点 / 数据栈。预设不可删改，可「复制成我的」再改。
- **自定义**：选服务、拖顺序（↑↓）。启动严格按顺序串行——数据库和 PHP 先起，Nginx 最后，
  否则 nginx 起来时 PHP 端口池还没就绪。
- **逐项回报**：某一项失败不阻断后面的项；失败项单独列出，端口冲突项直接给「结束占用并重试」。
- **入口不止一处**：总览页「启动常用栈」按钮、`Ctrl+K` 命令面板、**托盘右键菜单**都能一键拉起；
  设置里还能选「启动应用时自动拉起某个栈」。
- 栈只是有序服务 id 列表，启动仍然走 `ops::start_service`——
  nginx 的 reload、MySQL 的初始化、php 端口池这些关键处理一个都不会绕过。

## 端口冲突与进程治理

- **启动前预检**：每个服务启动前先探测自己的端口，被占时直接返回「端口 X 已被 <进程名> 占用」
  的人话错误（含 pid），覆盖 Nginx/Apache 的 **HTTP + HTTPS**、MySQL、PostgreSQL、MongoDB、
  Redis、mihomo 的**混合端口与控制端口**——配置里会无条件 bind 的端口一个都不漏。
- **自动释放（默认开，可关）**：启动服务前若端口被占，默认先把占用者收掉再启动。
  占用者是本应用自己的服务时走**优雅停止**（MySQL 干净关库、nginx 先 reload 停），
  是外部程序才直接结束；**结束过哪些端口会明确 toast 告知**，不会悄悄杀掉别人的进程。
  关掉「设置 → 启动 → 启动服务前自动释放被占用的端口」即恢复为纯报错。
- **一键结束占用并重试**：端口冲突的错误提示自带按钮，点了就结束占用者再重试启动。
- **工具箱 → 端口查询 / 结束进程**：查某个端口或一段范围被谁占用（进程名 + pid + 命令行），
  属于本应用服务的一键「停止服务」（优雅），外部程序「结束进程」；结束前二次确认。
- **php-cgi 端口池**：分配时整池预检；记录过的池若已被外部程序占用会自动重新分配
  （旧版会永远复用失效记录，导致该版本 PHP 再也起不来）。
- **全量体检**：工具箱 → 端口扫描「开始体检」一次列出本应用所有待绑定端口及其占用者，
  三态结论 `free / self（本应用在跑）/ conflict（被他人占用）`；仪表盘异常卡同样走这个接口。
  冲突行可直接结束占用进程（需确认，且只杀用户点选的那个 pid）。
- **不会误杀**：除「自动释放」这条显式开关外，诊断与结束进程都是「用户点选特定 pid」才执行；
  本应用从不主动杀非自己拉起的进程。
- **退出/崩溃清理**：所有子进程挂在 Windows Job Object（`KILL_ON_JOB_CLOSE`）/ Unix 进程组，
  正常退出无孤儿。启动时另有一道兜底：读取上次会话落的 `data/run/pids.json`，
  仅当「写入者已死 + 该 pid 仍存活 + 其可执行文件位于本应用 runtimes 目录内」三条同时满足才回收。
- **停止不谎报**：停止后逐个确认进程真的退出；仍有残留则报错并在 UI 显著提示，
  而不是把状态写成已停止（旧版会因此再也停不掉）。停机命令打向**启动时实际使用的端口**，
  运行期切换端口方案不会导致 shutdown 打空、只能强杀。

## 日志

日志页不再是一坨等宽白字，每行按语义着色：

- **级别高亮**：时间戳弱化、`[error]/[warn]/[notice]` 按级别配色，行首 2px 色条让错误行一眼可见。
- **HTTP 状态码分色**：2xx 绿 / 3xx 蓝 / 4xx 黄 / 5xx 红；请求方法、路径、IP、耗时、`key=value` 各自成色。
- **级别过滤 + 计数**：全部 / 错误 / 警告三个按钮带实时条数徽标。
- **关键字搜索**：命中的字串直接高亮，可叠加在级别过滤上。
- **暂停 / 自动滚动 / 自动换行 / 复制**：手动往上滚会自动停掉跟随，不会「看不到自己在读什么」；
  暂停时明确标注「显示的是暂停前的日志」，不假装是实时的。
- 拉取行数（默认 500）与是否默认自动刷新都在设置里可调。
- 解析是**无损**的：高亮只做着色，复制出来的内容与原始日志逐字节一致（有单测守着）。

## 托盘

托盘右键菜单按用途分组，并且**带状态**：

- **一键启动**子菜单：列出你的服务栈，标题直接写「N 个服务运行中」，点一下起整栈。
- **服务**子菜单：列出所有可启停服务（带监听端口），运行中的打勾，点一下启停。
- **站点**子菜单：直接列出站点名，点了用默认浏览器打开对应 URL（按当前端口档拼好）。
- **导航**：总览 / 套件 / 端口工具 / 日志 / 设置，点了把窗口带到前台并跳页。
- **图标带角标**：右下角小圆点显示「有服务在运行」，不用点开就知道环境起没起来。
- 左键单击图标 = 把主窗口叫到前台（比开菜单快一步）。
- 服务状态变化时会重建菜单与图标，所以勾选态、计数、角标始终和真实状态一致。

## 设置

- 外观（主题 / 主题色 / 减少动效）、通用（默认域名后缀 / 默认 Web 服务器 / 语言）
- **端口**：档位切换 + 逐个端口覆盖（覆盖值优先于档位，清空即恢复）
- 启动（开机自启 / 关闭到托盘 / **启动时自动拉起某个服务栈** / **启动服务前自动释放被占端口**）
- 日志（每次拉取行数 / 默认自动刷新 / 结束进程前二次确认）
- 下载镜像源 + 远端套件清单地址（填了「检查更新」才能发现新版本）
- 数据目录（查看 / 打开 / 迁移）
- **配置导入 / 导出**：站点 + 已装套件清单 + 设置 + 端口覆盖 + 服务栈 + 代理订阅 → 单个 JSON；
  **支持拖拽**——把备份 JSON 拖到设置页任意位置即可导入（拖进来时全屏提示投放区），
  也可以走「导入配置」按钮选文件。导入只新增/覆盖，不删站点、不卸载套件；
  缺什么套件会在结果里列出来让你补装。
- 更新：应用版本 + 套件清单版本检查。


## macOS 状态（诚实说明）

macOS 侧已修掉的阻塞性缺陷：Unix 进程终止此前是空实现（`libc_kill` 占位返回 0），
导致 **macOS 上停止服务是静默无效的**，现已用 `libc::kill` + 独立进程组实现真正的
SIGTERM/SIGKILL 与整棵子树回收。同时修复：清单入口路径在 macOS 上被错误地按 `\` 拼接、
`.gz` 单文件包解压后按缓存文件名（`.pkg`）而非清单 entry 落盘、`mysqld --console`
（Windows 专属选项）在 macOS 上必然报 unknown option、`initdb -U root`
（macOS 拒绝 root 用户）、提权执行未做参数转义且忽略退出码。

**仍然没有解决的是二进制来源**：`manifest/packages.mac.json` 目前只有
MySQL / mihomo / Node / Go / MongoDB / Composer，**PHP、Nginx、Redis、Apache、PostgreSQL
没有官方 macOS 二进制**，因此这些服务在 macOS 上「装不了」而不是「跑不起来」。
需要补录候选源（如 `crazywhalecc/static-php-cli` 的 php-cgi 构建）或新增 `source-build`
安装方式后才谈得上 macOS 完整可用。

## 安装形态与数据目录

- NSIS 安装包（`target/release/bundle/nsis/`）为 **currentUser 模式**：安装向导可自选任意可写目录（默认 `%LOCALAPPDATA%\Programs\NiceServBay`）
- **数据跟随安装目录**：安装版把 `runtimes / etc / data / logs / certs / backup` 全部放在 `{安装目录}/nsb-data/`，整个目录可拷贝迁移；安装目录不可写时自动回退 LocalAppData
- 开发模式（cargo target 下运行）仍使用 `%LOCALAPPDATA%\NiceServBay`，`NSB_HOME` 环境变量可强制覆盖
- 设置页「数据目录」卡片实时显示真实路径（`get_data_dir`）

## 配置导入 / 导出

设置页「配置导入 / 导出」：一键把**站点 + 已装套件清单 + 全部设置 + 代理订阅**导出为单个 JSON；换机导入时站点配置自动重建（域名冲突跳过）、设置与订阅恢复，并列出待补装的套件清单。套件本体（运行时大文件）不随备份走。

## i18n（中英双语）

全部界面文案（含套件页/向导/工具箱/设置/命令面板/共享组件）已收编进 `src/lib/i18n.ts` 的 zh/en 双语字典；设置 → 语言切换即时生效（无残留硬编码中文，个别专有名词除外）。


## 套件目录（Windows 清单，均带 sha256 断点续传）

**每个服务都有多版本可选**（版本徽标点选安装 / 切换）：

| 服务 | 可选版本 |
|------|----------|
| Nginx | 1.28.0 / 1.27.5 / 1.26.3 / 1.24.0 |
| Apache | 2.4.66 |
| PHP | 8.5.10 / 8.4.25 / 8.3.33 / 8.2.33 / 8.1.34 / 8.0.30 / 7.4.33 / 7.3.33 / 7.2.34 |
| MySQL | 8.0.46 / 5.7.44 |
| PostgreSQL | 17.6 / 16.9 |
| MongoDB | 8.0.4 / 7.0.24 |
| Node.js | 22.14.0 / 20.19.5 / 18.20.8 |
| Python | 3.13.7 / 3.12.9 / 3.11.9 |
| Go | 1.24.1 / 1.23.6 |
| Composer | 2.8.5 / 2.7.9 |
| mihomo | 1.19.11 / 1.19.10 / 1.18.10 |
| Redis | 5.0.14 |

**单实例服务**（Nginx/Apache/MySQL/PG/Mongo/Redis/mihomo）支持「切换使用版本」：已装徽标间点击即切换，运行中会提示先停止；**多实例服务**（PHP 池、MySQL）逐版本独立启停，可并行。

服务语义由**清单声明驱动**（`run` 描述符：args/singleInstance/dataDir/端口占位符），新增服务只需改清单，前端与后端通用路径自动适配（长尾服务如 Caddy/Meilisearch/MinIO/Mailpit 走 `generic.rs` 通用启停）。
新增服务验收：`cargo run --release -p nsb-core --bin check_extra`（Apache 真实执行 PHP / PG initdb+psql / Mongo 就绪 / 无孤儿，24 项）。
macOS 清单同步扩充：Node / Go / MongoDB / Composer（真实哈希），PHP/Apache/PG 见 mac 构建手册。

## 验收清单（每步都已自动化在 `--smoke-test` 中）

```bash
./target/release/niceservbay.exe --smoke-test
```

| # | 验收项 | 结果 |
|---|--------|------|
| 1 | 初始化数据目录 + SQLite | ✅ |
| 2 | 下载缓存预置（断点续传/sha256 幂等） | ✅ |
| 3-8 | 安装 nginx/php×2/mysql/redis/mihomo（真实解压+默认配置） | ✅ |
| 9-13 | 五个服务启动 + 健康检查 | ✅ |
| 14 | 服务状态灯 = 真实进程状态 | ✅ |
| 15 | 创建 PHP 8.3 站点（含建库建号 + .env.example） | ✅ |
| 16 | 浏览器语义访问：Host 头命中站点（http://smoke83.nsb.test:8080） | ✅ |
| 17 | `phpinfo()` 执行 | ✅ |
| 18 | .env.example 连接信息写入 | ✅ |
| 19 | **PHP PDO 连 MySQL**（建表/插入/查询 VERSION） | ✅ `MYSQL_OK:8.0.46` |
| 20 | **PHP 连 Redis**（原生 RESP SET/GET） | ✅ `REDIS_OK` |
| 21-22 | **双 PHP 版本同时服务**（8.3.33 + 7.4.33 按站点绑定） | ✅ |
| 23-24 | 反向代理站点 → 本机 :18080 后端 | ✅ |
| 25-27 | mihomo 启动 + REST API + **混合端口真实代理外网** | ✅ |
| 28 | 系统代理状态读取（不修改） | ✅ |
| 29 | 根 CA + 站点证书签发 | ✅ |
| 30 | 端口诊断（谁占用→pid） | ✅ |
| 31 | 日志管线 tail | ✅ |
| 32 | 全部服务干净停止（Job Object 无孤儿进程） | ✅ |

单元测试：`cargo test -p nsb-core` → 9/9（hosts 标记块合并 / mihomo 订阅端口接管 / php.ini / 站点 conf / 下载断点续传 + 校验失败拒绝 / 端口池幂等分配）。

### 手动验收（GUI）

1. `pnpm tauri build` 后安装 NSIS 包，或 `pnpm tauri dev`
2. 首次启动引导 → 选「PHP 网站」→ 自动安装 Nginx+PHP+MySQL+Redis（环形进度）
3. 创建站点向导：名称 → 根目录（模板「空白 PHP」）→ PHP 版本 → HTTPS → 数据库 → 伪静态
4. 打开浏览器访问 `http://你的域名.test:8080` → 看到 `It works! PHP 8.3.33`
5. 「证书与域名」页信任根 CA（UAC）→ `https://域名.test:8443` 无警告
6. 关闭窗口 → 托盘常驻 → 右键「启动常用栈 / 停止全部 / 退出」

## 在 macOS 上构建与验收

> 本仓库主要在 Windows 上开发。macOS 侧已做如下**可验证的**就绪工作：
> - `cargo check --target aarch64-apple-darwin -p platform` 通过（全部平台敏感代码：进程组、
>   hosts 标记块、networksetup 系统代理、kill -0 存活探测的类型正确性）
> - 清单按 OS 自动选择（`manifest/packages.mac.json`），`targz` 解压（系统 tar/gzip）已实现
> - `tauri.conf.json` bundle targets 含 `app`/`dmg`；exe/目录名已抽象（`ops::exe_name`/`mysql_root_name`）
> - core 全量交叉检查的唯一障碍是 `libsqlite3-sys` 需要 Xcode CLT 的 `cc`——mac 上天然满足

**步骤**（在一台 macOS 12+ 机器上，Apple Silicon 或 Intel）：

```bash
xcode-select --install          # 需要clang
git clone <repo> && cd nice-servbay
pnpm install
pnpm tauri build                # 产出 target/release/bundle/dmg/*.dmg 与 *.app
./target/release/niceservbay --smoke-test   # 无头验收（同 Windows 33 项）
```

**三项人工验收**：

1. **HTTPS 无警告**：安装并打开 App → TLS 页「信任根证书」（输入密码，`security add-trusted-cert`）
   → 创建 HTTPS 站点 → 浏览器访问 `https://域名.test:8443` 应无警告。
   （密码学前提已被 smoke 第 30 项 `openssl verify -CAfile ca.crt site.crt` 证明）
2. **托盘启停**：关闭主窗口（默认最小化到托盘）→ 托盘菜单「启动常用栈 (LNMP)」→
   服务灯变绿；「停止全部服务」→ 变灰；「退出」清理所有子进程。
3. **冒烟**：`./target/release/niceservbay --smoke-test` 应全绿（mac 清单当前含 MySQL/mihomo，
   PHP/Nginx/Redis 见下）。

**macOS 二进制源现状（诚实说明）**：mac 清单目前只收录源头已验证可达（HTTP 206）的
MySQL 8.0.46（`cdn.mysql.com` macos15 tarball）与 mihomo（darwin-arm64）。PHP/Nginx/Redis
无官方 mac 二进制（nginx.org/redis.io 只有源码），候选方案（在 mac 上验证后补入 manifest 即可，
安装器无需改动）：

- PHP：`crazywhalecc/static-php-cli` GitHub Releases（需含 php-cgi 的构建变体）
- Nginx / Redis：源码编译（`./configure && make install`，需新增 `source-build` 安装 kind）
  或 Homebrew bottle 直链（与「非侵入」原则有取舍）

## 安全红线（测试即证明）

- 所有运行时装在 `{LocalAppData}/NiceServBay`，非侵入、不污染 PATH
- hosts 以标记块写入，失败给人话指引（不静默强写）
- 冒烟测试全程：不改系统 hosts、不开系统代理、不 kill 非本应用进程
- MySQL 数据目录在应用数据目录内，root 密码只存本机 SQLite
- Windows 子进程全部挂 Job Object（KILL_ON_JOB_CLOSE），应用退出无孤儿

## 已知边界（Phase 3 范围）

- Windows nginx `-s reload` 有平台差异：站点变更采用亚秒级「快速重启」保证生效（`ops.rs` 有注释）
- Caddy/Apache/Tomcat、PostgreSQL/MongoDB、Go/JDK/Python/Node 运行时、Mailpit、MinIO：清单与架构已就绪，待补 manifest 条目与启停编排
- macOS 侧 hosts 写入走 osascript 提权（`platform::run_elevated` 已预留），需在 mac 上联调
- Node/Python/Java/Go 站点：数据模型（SiteRuntime.kind）与 nginx 反代路径已支持「反代到 dev server」用法
