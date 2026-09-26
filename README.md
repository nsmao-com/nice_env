# NiceEnv

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

## 本地 DNS 解析（CoreDNS）

hosts 文件不支持通配符；`*.test` 域名的根治方案是 DNS。安装 CoreDNS 后到
「工具箱 → 本地域名解析」一键启停：任意 `*.{TLD}` 都解析到 127.0.0.1，其余查询转发
公共 DNS（新增站点无需任何配置）。一次性接入：把网卡首选 DNS 设为 `127.0.0.1`
（标准档监听 53 端口），`nslookup anything.test 127.0.0.1` 即可验证。
Corefile 每次启动按当前 TLD 设置自动重建。

## CLI 与 AI 集成（nsbctl / nsb-mcp）

**nsbctl**：随应用分发的命令行工具（与桌面端共用数据目录）。

```
nsbctl status [--json]    服务状态总览
nsbctl start <service>    启动（nginx / php@8.3.33 / mysql@8.0.46 / …）
nsbctl stop <service>     停止
nsbctl restart <service>  重启
nsbctl start-all          启动常用栈（与托盘一致）
nsbctl sites / open <站点> / packages / logs <服务> [n] / diagnose <端口>
nsbctl pin php@8.3.33 <项目目录>   # 项目级运行时锁定（写入 .nsb.json，向导「跟随项目」时自动采用）
```

CLI 启动的服务走**分离模式**：CLI 退出服务不退；桌面 App 下次启动会**自动收养**
这些进程（恢复 pid/端口/运行态显示），而不是把终端里起的服务当孤儿杀掉。

**nsb-mcp**：Model Context Protocol 服务器（stdio JSON-RPC）。给 Claude Desktop /
Cursor 等 AI 客户端加上即可用自然语言操作本地环境——列服务/起停服务/列站点/查端口占用：

```json
{ "mcpServers": { "niceservbay": { "command": "nsb-mcp" } } }
```

## 套件与运维增强

- **平台过滤**：安装前按清单 `os`/`arch` 拦截不兼容条目（arm64 包装上 x64 机器直接报
  `PLATFORM_UNSUPPORTED`，而不是装完启动崩）；套件页给对应版本打「当前平台不可用」徽标并禁点。
- **PHP 扩展面板**：扫描该版本 `ext/` 目录真实存在的扩展，勾选即改 php.ini（注释保留、
  改前备份），并用 `php -m` 实测加载结果；附 display_errors/OPcache 等快捷开关。
- **远端清单 + 用户自定义模块**：设置 manifestUrl 后可一键拉取远端清单（校验通过才落盘，
  重启生效）；`{数据目录}/user-modules/*.json` 里的自定义套件会被合并进清单
  （同 id+version 覆盖内置），坏文件自动跳过不影响启动。
- **通配符证书**：站点域名支持 `*.dev.test`，证书签发/落盘/记录全链路打通
  （Windows 文件名禁 `*`，落盘自动净化为 `_wildcard`）。
- **端口自动回落**（可选）：开启后服务端口被占时自动换到附近空闲端口，并固化为
  端口覆盖项——重启后仍用同一端口，连接串保持稳定。
- **站点级 PHP 覆盖**：PHP 站点可按站点写 `memory_limit`/`upload_max_filesize` 等
  覆盖项，落到站点根目录 `.user.ini`（键值白名单校验，防 ini 注入）。
- **定时配置备份**：off/daily/weekly 三档，自动备份到 `backup/auto/`，保留最近 10 份。
- **服务状态历史**：最近 200 次状态变更（含失败原因）可查，前端日志页/诊断联动。
- **可更新徽标**：套件页已装条目出现更高正式版时显示「可更新」。
- **批量打开 / 复制站点地址**：站点页一键在浏览器依次打开全部站点，或复制全部 URL。
- **MySQL 库大小 / Redis 实时统计**：数据库列表直接显示每个库占用量；Redis 卡显示
  内存占用、键数量、连接数与运行天数（原生 RESP，每 5s 刷新）。
- **日志导出**：日志页一键把选中服务的完整日志另存为文件。
- **配置体检（只读）**：修复向导新增「nginx -t / httpd -t / PHP ini 加载测试」，
  只检查不改动文件，坏了先体检再重写。

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

## PHP 扩展与 Xdebug

ServBay / FlyEnv / phpStudy 最常用的两块能力，这里都做了，并且都**实测**而不是假装成功。

### 扩展面板（套件页 → PHP 行的「扩展 N」）

- **以真实磁盘为准**：扫描该版本 `ext/php_*.dll`，不维护一份写死的清单 ——
  用户自己丢进去的扩展、不同 PHP 版本扩展集合的差异都能正确反映。
- **状态来源是 php.ini**：`extension=` / `zend_extension=` 行决定启用与否；
  禁用时注释掉而不是删行，保留用户原本的顺序与上下文。
- **改完实测**：用 `php -n -c <ini> -m` 跑一遍，加载失败就把 PHP 的原始告警
  摊给用户看，而不是显示一个「已启用」的假状态。
- **依赖提示**：`pdo_mysql` 需要 `pdo`、`mysqli` 需要 `mysqlnd` 这类关系会高亮提醒，
  不用去对着 "Unable to load dynamic library" 发呆。
- **php.ini 快捷开关**：`display_errors` / `log_errors` / `opcache.enable` 直接给开关。
- **一键补齐常用扩展**：curl / fileinfo / gd / mbstring / mysqli / openssl /
  pdo_mysql / sockets / zip / intl 一次点完。

### Xdebug 一键配置

Windows 上装 Xdebug 有个坑：它的 DLL 是按 PHP 的**构建指纹**分发的，
装错版本 PHP 会直接拒绝加载。所以这里的流程是：

1. 跑 `php -i` 读出构建指纹（PHP API + TS/NTS + 编译器 VS16/VS17 + 架构）；
2. 按指纹拼出官方 DLL 文件名候选并下载；
3. 放进该版本 `ext/`，写 `zend_extension=` 与 `[xdebug]` 段；
4. **用 `php -m` 实测确认真的加载上了**，并把实测到的版本号显示出来。

默认配置刻意选了 `xdebug.start_with_request=trigger`：不主动连 IDE，
避免没开 IDE 时每个请求都要等连接超时（这是新手最常踩的「网页变慢」坑）。
拉不到时也不假装成功，会把该下载的确切文件名与 URL 写给用户，
可以自己下好走「从文件安装」。

## 数据库备份与还原

- **导出**：多选库，`--databases` 带上建库语句（换台机器直接还原）；
  `--single-transaction` 避免锁表影响正在跑的站点。
- **还原**：默认先自动备份当前全部业务库 —— 还原不可撤销，得留退路。
- **进度可见**：按写出字节数回调，界面显示已导出量与百分比。
- **密码不走命令行**：用临时 defaults-file 传参，避免被其它进程从进程列表看到。

## 依赖源镜像（Composer / npm / pip）

注意这与「套件下载镜像」不是一回事：那个管本应用下载套件走哪条路；
这个管**你在项目里 `composer install` / `npm install` 时**走哪条路。
国内直连官方源常只有几十 KB/s，换镜像快一个数量级。

- Composer：阿里云 / 腾讯云 / 华为云 / cnpkg
- npm：淘宝 npmmirror / 腾讯云 / 华为云
- pip：清华 TUNA / 阿里云 / 中科大

因为改的是**全局配置文件**（`~/.npmrc`、Composer 的 `config.json`、`pip.ini`），
界面上会明确写出会改哪个文件、切换前二次确认，并提供「恢复官方源」。
改写时保留其它配置项与注释，不是整份覆盖；「恢复官方源」也只删 packagist
覆盖，不动用户自己的私有源。

## 配置文件编辑器（工具箱）

「用记事本改 nginx.conf，改错了服务起不来只能自己排查」这件事，这里把它前置拦住了：

- **nginx 走真的 `nginx -t`**，校验文件与真实配置同目录（相对 include 才成立），
  从报错里解析出行号，前端可**点击直接跳到那一行**。
- **php.ini / my.ini 做结构自检**：段名括号、引号闭合、每行必须是 `key=value`，
  并额外警告「ini 里用 # 注释」这个会让配置**静默失效**的高频错误。
- 花括号配平（先剥注释再计数，注释里的 `{}` 不干扰）；YAML 查 Tab 与奇数缩进。
- **校验不通过拒写**，把行号与原因摆出来；允许「强制保存」但需二次确认
  （校验器偶有误报，不该把用户锁死）。
- 每次保存自动备份，历史版本可一键回滚（回滚本身也会先备份）。

## 站点 .env 编辑器（站点详情）

Laravel / Symfony 类项目最常改的文件，比「打开文件夹用记事本改」多三件事：

1. **敏感值默认打码** —— `PASSWORD` / `SECRET` / `KEY` / `TOKEN` 类键自动识别，
   聚焦或点眼睛才显示（投屏演示时不会把库密码晾在屏幕上）；
2. **提示该加引号的值** —— `DB_PASSWORD=my secret pass` 不写引号会被 dotenv
   截断成 `my`，这个坑极难自己发现；保存时自动补引号并转义内部引号；
3. **一键补全 `DB_*`** —— 从站点绑定的数据库直接抄连接信息。

只改传进来的键，注释/空行/顺序全部保留；写前备份为 `.env.nsb-backup`。

## 项目扫描（站点页 → 扫描）

手上已经有一堆项目目录时，不必再手工填「类型 + 文档根 + 伪静态 + PHP 版本」。

识别策略按可信度降序取第一个命中：框架标记文件（`artisan` / `wp-config.php` /
`next.config.*` / `go.mod` …）→ 依赖清单里的框架 → 入口文件形态 → 文件类型兜底。
覆盖 Laravel / ThinkPHP / WordPress / Symfony / CodeIgniter / Next.js / Vite /
Nuxt / Node / Python / Go / Java / 静态站。

- **结果必须显示识别依据**，识别总会出错，让用户能判断对错；
- 文档根不存在时退回项目根，不建指向空目录的站点；
- 建站时 `template: "none"` + 不写 `.env.example` —— **绝不往用户代码里塞文件**；
- 只扫一层深度，跳过 `node_modules` / `vendor` / `.git` 等噪音目录；
- 纯只读：不修改、不执行项目里任何东西。

## 环境体检（总览页顶部）

把散落各页的检查项聚合成一张清单，按严重程度排序（错误 → 警告 → 提示）：

端口被外部程序占用（本应用自己在跑不算问题）· 站点引用了未安装的 PHP 版本
或文档根不存在 · 证书过期 / 7天内 / 30天内到期 / 文件丢失 / 根 CA 未信任 ·
hosts 托管记录与站点列表不一致 · 服务处于错误状态 · 数据目录不可写 ·
PHP 已启用但缺依赖的扩展。

每条都带「去处理」直跳到能修它的页面；非错误项可单独忽略。
空环境给的是「还没装套件」的引导，而不是假的全绿。

## 诊断报告（工具箱）

一键汇总「报 bug 需要的全部信息」成 Markdown：应用版本 / OS / 数据目录、
服务状态表（含最后错误）、已装套件、站点列表、端口占用（逐个探测占用人）、
证书体检摘要、配置摘要（已脱敏）、各服务最近 40 行日志、设置摘要。

**红线是绝不泄漏隐私**，所以打包前做脱敏：`PASSWORD` / `SECRET` / `KEY` /
`TOKEN` 类赋值行只留前 2 位并打码（注释掉的也打）；用户主目录替换为 `<home>`。
报告里会明确显示「已打码 N 处」，让用户敢直接贴进 issue。
采不到的配置也会**明确列出来**（而不是静默跳过），
否则看的人无法区分「配置正常」与「压根没采集」。

## 批量操作

- **服务**（总览 → 批量操作）：任选一组服务启停/重启。执行顺序按依赖分层 ——
  启动是数据与缓存 → 运行容器 → Web 服务器，停止反过来。
  nginx 在上游没就绪时起来会直接 502，所以顺序不是照勾选来的。
  重启是一起停再一起起，而不是逐个 restart（后者会让共享依赖被反复中断）。
  界面上会显示**实际执行顺序**与逐项结果。
- **站点**（站点页 → 批量启停）：多选后一次启用/停用。
  后端先写完全部 vhost、**最后只 reload 一次** ——
  逐个调单站点接口会让 nginx 反复重建配置并 reload N 次。

## 服务看门狗

服务意外退出时自动拉起，默认关闭（自动重启会占端口、写日志，属于有副作用的行为）。

- **你主动点「停止」的服务绝不被重启** —— 否则点了停止又被拉起来比不重启还糟；
- 只有真正启动成功过的服务才纳入监控；
- 指数退避 2s→4s→8s…上限 60s，避免起不来的服务拖垮机器；
- 默认最多 5 次，用尽后明确告知并可一键重置。

## 日志导出

日志页可把**当前过滤/搜索后的视图**存成文件。内容与屏幕上看到的逐字节一致 ——
否则会出现「我搜了 error 导出却是全量」这种让人不信任的行为。
文件名 `{服务}-{时间戳}.log`，同名不覆盖（加序号），
用户自定义名也会清洗（强制 `.log` 后缀、去掉路径分隔符）。

## 站点模板

新建站点时可选 9 种模板，框架项目会安装实际依赖，并把文档根设到对应的公开目录：

| 模板 | 入口位置 | 说明 |
|------|---------|------|
| Laravel / Symfony | `public/index.php` | Composer 安装官方项目，初始化应用密钥与首页 |
| ThinkPHP | `public/index.php` | Composer 安装官方项目，初始化服务发现与本地首页 |
| WordPress | 根目录 `index.php` | 下载完整官方程序，可生成数据库连接配置 |
| CodeIgniter 4 | `public/index.php` | Composer 安装官方项目，初始化站点地址 |
| Next.js 静态导出 | `src/app/` → `out/index.html` | 官方生成器创建 TypeScript 项目，通过 pnpm 安装依赖后静态导出 |
| 前端 SPA | `index.html` | 伪静态设为 fallback，深链可用 |
| 静态站 / 空白 PHP | 根目录 | 给了一个像样的落地页 |

PHP 框架使用选中的 PHP CLI 与 Composer。Next.js 使用当前启用的 Node.js（20.9+），
独立准备 pnpm，不需要全局安装包管理器。新框架项目在临时目录内完成安装与检查后才
写入目标空目录；错误会保留详细原因，已有项目文件不会被覆盖。
选择「使用现有目录」时不补写占位入口，选模板会自动带上对应的伪静态规则。
Next.js 静态路由支持 HTML 文件与目录索引，不存在的路径返回 404；SPA 则使用首页回退。
开发验收状态与尚未验证的链路见 `DESIGN.md` 第 5 节。

## 命令面板

`Ctrl+K` 除了原有的新建站点 / 启动栈 / 停止全部 / 页面跳转 / 站点 / 服务之外，
新增：扫描项目目录、检查环境问题、生成诊断报告、编辑配置文件，
以及每个运行中服务的「重启」。

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
- 下载镜像源 + 远端套件清单地址（填了「检查更新」才能发现新版本；发现后在更新弹窗点「应用新清单」，
  清单落盘为快照、重启后生效；设置页「恢复内置清单」可随时回退）
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

- NSIS 安装包（`target/release/bundle/nsis/`）为 **currentUser 模式**：安装向导可自选任意可写目录（默认 `%LOCALAPPDATA%\Programs\NiceEnv`）
- **数据跟随安装目录**：安装版把 `runtimes / etc / data / logs / certs / backup` 全部放在 `{安装目录}/nsb-data/`，整个目录可拷贝迁移；安装目录不可写时自动回退 LocalAppData
- 开发模式（cargo target 下运行）仍使用 `%LOCALAPPDATA%\NiceEnv`，`NSB_HOME` 环境变量可强制覆盖
- 设置页「数据目录」卡片实时显示真实路径（`get_data_dir`）

## 配置导入 / 导出

设置页「配置导入 / 导出」：一键把**站点 + 已装套件清单 + 全部设置 + 代理订阅**导出为单个 JSON；换机导入时站点配置自动重建（域名冲突跳过）、设置与订阅恢复，并列出待补装的套件清单。套件本体（运行时大文件）不随备份走。

## i18n（中英双语）

全部界面文案（含套件页/向导/工具箱/设置/命令面板/共享组件）已收编进 `src/lib/i18n.ts` 的 zh/en 双语字典；设置 → 语言切换即时生效（无残留硬编码中文，个别专有名词除外）。


## 套件目录（Windows 清单，均带 sha256 断点续传）

清单共 **160 个条目 / 49 个服务**，其中 **45 个服务提供多版本**
（版本下拉点选安装 / 切换；`revision=38`）。

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

- 所有运行时装在 `{LocalAppData}/NiceEnv`，非侵入、不污染 PATH
- hosts 以标记块写入，失败给人话指引（不静默强写）
- 冒烟测试全程：不改系统 hosts、不开系统代理、不 kill 非本应用进程
- MySQL 数据目录在应用数据目录内，root 密码只存本机 SQLite
- Windows 子进程全部挂 Job Object（KILL_ON_JOB_CLOSE），应用退出无孤儿

## 已知边界（Phase 3 范围）

- Windows nginx `-s reload` 有平台差异：站点变更采用亚秒级「快速重启」保证生效（`ops.rs` 有注释）
- Caddy/Apache/Tomcat、PostgreSQL/MongoDB、Go/JDK/Python/Node 运行时、Mailpit、MinIO：清单与架构已就绪，待补 manifest 条目与启停编排
- macOS 侧 hosts 写入走 osascript 提权（`platform::run_elevated` 已预留），需在 mac 上联调
- Node/Python/Java/Go 站点：数据模型（SiteRuntime.kind）与 nginx 反代路径已支持「反代到 dev server」用法
