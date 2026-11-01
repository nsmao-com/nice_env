# Go + Wails v3 迁移与验收

当前 `main` 使用 Go + Wails v3、React 19 + Vite。Rust/Tauri 源码、Cargo 工程和打包入口已移除；业务命令由 Go 实现，不调用旧版可执行文件。v2.0.0 已发布：提交 `2bbf2b6`，CI 与 Windows/macOS Intel/Apple Silicon 发布构建全部通过。原生构建成功不代表所有平台交互都已验证。

## 实现范围

旧版 328 个命令均已接入 Go 核心或桌面适配层，契约保留在 `native-command-contract.json`。`scripts/wails-port-status.mjs --require-complete` 检查入口缺项，不能代替运行验证。

- `cmd/niceenv`、`internal/desktop`：原生窗口、托盘面板、事件、文件选择、系统打开、自启动、退出与数据迁移重启。
- `internal/core`：套件安装/版本、服务生命周期/原生配置校验、站点/脚手架、数据库工作台/备份/外部导入、目录迁移、证书/ACME/部署、代理/DNS/隧道、环境/工具镜像、任务/看门狗、AI、MCP、更新。
- `internal/process`：Windows Job Object、Unix 进程组、进程身份校验、正常停机信号、隐藏子进程与分离的 CLI 控制器。
- `cmd/niceenv-cli`：独立验收批量入口；打包为 nsbctl/nsb-mcp 时使用桌面控制器，CLI 退出不会停止其管理的服务。
- `apps/web`：沿用 React 页面，统一 Wails Invoke 与事件适配。浏览器预览 mock 与原生执行分离。

## 数据目录

默认源码构建为开发模式，使用 `os.UserConfigDir()/NiceEnv-Wails-Development`，可通过 `NICEENV_WAILS_HOME` 指向带开发标记的隔离目录。

正式打包通过 ldflags 设置 `BuildChannel=production`，依次读取显式覆盖、NSB_HOME、旧版 `com.niceservbay.app/data-directory.json` 与安装旁的 nsb-data（Windows）。macOS 默认使用用户配置目录 NiceEnv。无效的已选目录不会静默回退到空环境。

首次读取 v1 数据前，检查活动锁、目录归属和遗留运行时进程；对 SQLite 做完整性检查及包含 WAL 的一致性快照，保存在 `backup/upgrade-v2`，再补齐元数据列。运行时数据库原文不做路径字符串替换。数据移动使用快照、哈希清单、目标副本路径重映射和重启就绪握手，旧目录保持可恢复。

JSON 路径字段使用 `Path`，外部工具参数与打开文件边界使用 `DisplayPath`。保留原生长路径与 UNC 语义，不修改 SQL、密码、数据库单元格或任意配置原文。

## 构建与发布

Go 最低 1.26，Wails 固定 `v3.0.0-alpha2.117`，前端运行时固定 `3.0.0-alpha.97`。

不需要前端打包的检查：

```sh
go build -mod=readonly ./...
go vet -mod=readonly ./...
pnpm --filter @nsb/web check
pnpm --filter @nsb/schema check
node scripts/wails-port-status.mjs --require-complete
```

正式包需要显式构建的 `apps/web/out`。`pnpm package:desktop` 只打包现有资源，不隐式启动或构建前端；版本来自各工作区一致的 package.json。Windows 生成资源、桌面/CLI/MCP 与 NSIS；macOS 在对应架构原生机器生成 app、ad-hoc 签名和 dmg。未配置商业签名、公证。

CI 使用 Go 编译/静态检查与前端类型/现有逻辑检查。Release 验证 annotated tag 和版本，Windows x64、macOS Intel/Apple Silicon 全部出包后统一校验 SHA256 并发布。安装器不递归删除安装目录，卸载保留 nsb-data 与用户文件。

## 已完成的 Windows 隔离验证

本机证据位于忽略目录 `artifacts/function-audit/`，数据位于 `.wails-home/`，未使用现用 NiceEnv/FlyEnv 数据库。

| 范围 | 验证内容 |
| --- | --- |
| 服务 | Nginx/Apache/Caddy/PHP、MySQL/PostgreSQL/Redis、MongoDB、Memcached、Consul、MinIO、CoreDNS、Qdrant、SFTPGo、mihomo、JVM 服务的实际启停、端口归属、专用就绪与配置保留 |
| 网站 | 静态/PHP/代理/跳转、网络模式、PHP pin、脚手架、项目归档、配置失败恢复；Nginx 重载后立即访问及 8 秒长请求不中断 |
| 数据库 | MySQL/PostgreSQL 工作台和备份路径、Redis/MongoDB 数据与认证/备份、Adminer/phpMyAdmin 实际 HTTP 登录；双 MySQL 8.0 外部导入、旧目标备份、同实例拒绝 |
| 数据移动 | 文件哈希与路径重映射、源目录保留、取消冻结、SFTPGo SQLite/Bolt、Tomcat/PostgreSQL 配置、桌面迁移重启和回退 |
| v1 升级 | WAL 快照、缺列补齐、重复升级、坏 SQLite/陌生目录拒绝、旧锁互斥、旧目录选择文件解析，共 10 类场景 |
| 自动化 | 计划任务、看门狗、启动栈、AI 本地协议与工具确认、MCP tools/list/实际服务查询、CLI 退出后服务存活 |
| 证书与联网 | 本地证书、ACME 材料/离线协议/1Panel 模拟部署、SSH 指纹握手、Cloudflare 隧道、系统代理读取与本机直连 |
| 安装包 | Windows 安装/版本/卸载，卸载后数据库哨兵和自定义文件保留，审计恢复原有快捷方式和卸载注册表 |
| 配置引用 | Nginx map/include 上下文、Apache ServerRoot/include、凭据字节保护、循环/跨用途冲突/间接目录变量拒绝、历史备份预览与恢复；10 类隔离场景通过，真实 Nginx/Apache 语法与 Nginx HTTP 通过 |
| 并发读取 | 同一 Wails 控制器 16 路并发、64 次 PATH 状态/终端预览/数据库备份列表及目录读取；修复只读请求争用服务操作锁和首次建目录竞态 |
| 中文路径 | Windows Nginx 以 Unicode 工作目录配合 ASCII 相对参数进行配置校验、启动和停止，真实 HTTP 验证通过 |
| 独立包管理器 | Node 26.13.0/26.12.1、npm 12.2.1/12.2.0、pnpm 12.17.0/12.16.1 六个官方包实际下载、校验、解压；19 项切换检查覆盖实际命令版本、核心重启后选择保留、无效版本拒绝、重复应用及关闭后原 PATH 恢复。使用 PATH 隔离文件，未修改系统 PATH |
| MySQL 工作台 | MySQL 8.0.46 隔离实例 30 项操作检查：库/表/索引/外键结构、分页与筛选、中文及反斜杠字面内容、精确小数、NULL、未确认 SQL 拒绝、非法字段拒绝、过期行更新拒绝、真实 dump/restore 数据一致；临时业务库已删除 |
| PostgreSQL 工作台 | 18.6 隔离实例 34 项操作检查：角色与密码、连接数确认、旧 revision 拒绝、建库、中文/路径/精确小数、原生备份与恢复内容一致；临时库、角色和备份已清理 |
| 运行时卸载 | Node/npm/pnpm 六版本安装与切换之后，18 次卸载相关调用验证另一版本、PATH 和配置保留；六个验收运行时已卸载 |
| 软件更新 | v2.0.1 真实检测与下载，版本文本无重复 v，Windows 安装器 20,840,441 字节，SHA-256 与发布记录及本地下载收据相同；未执行覆盖安装 |

备份列表复用单次请求内的目标索引，避免每份历史备份重复扫描目录；预览和恢复仍重新读取并校验版本。runtime 包资源不按扩展名全量改写，Web 配置按实际 include 和资源引用规划。无法安全确定的动态资源路径、循环、跨用途共享和字符类通配符会拒绝自动迁移并保留源目录，不能将这种保护理解为任意自定义配置均可无交互迁移。

最新前端资源已构建并用于 Windows 安装包；继续验收时未再次运行前端 dev/build。GUI 已观察首页、套件、数据库、环境变量、配置备份和 AI 面板；AI 面板不模糊背景。真实 AI 账号未用于发送内容。一次 UI 输入工具坐标错误后，重新捕获窗口并使用键盘退出引导，后续页面导航恢复。

空环境、计划任务页静置 15.2 秒采样：桌面与 WebView2 子树 CPU 合计约为单核心的 32.99%，20 逻辑处理器下折算整机约 1.65%；工作集合计约 671.1 MiB，可能重复计算共享页。该采样不是所有页面的性能上限，也没有同场景 Tauri 基线。

v2.0.0 站点空页面另一次 20.15 秒采样：进程树 CPU 约单核心 29.08%，整机 1.45%。这是闲置开销的观测值，不是性能优化已达标的证明；报告为 `gui-sites-idle.json`。包管理器与 MySQL 新增证据为 `js-versions-install.json`、`js-versions-switch.json`、`mysql-workspace-roundtrip.json`。npm/pnpm 目录现按 SemVer 识别预发行版本，并在读取旧缓存时重新筛选，防止 `-pre.*` 混入正式列表；其它发行源的厂商版本格式保留原有规则。

v2.0.2 取消顶栏 CPU/RAM/DISK 小条的宽度过渡，保留原有采样间隔、数值和阈值颜色。独立 Wails 实例站点空页面通过 DevTools Performance 做原样/禁用过渡交替实验，各 12 秒、两轮：原样布局次数 484/470，禁用后 10/10；渲染进程 CPU 时间分别为 0.723/0.740 秒和 0.134/0.137 秒。报告 `performance/sites-microbars-ab.json` 为运行时样式对照，未启用前端 dev/build；该数据只证明这一处动画的开销，不代表整机 CPU 或 Tauri 对比结果。

启动更新提醒复现了“关闭提醒 → 跳过引导保存设置 → 再次弹出”的问题。自动检查现在只依赖更新开关且每次启动最多执行一次；延迟期间关闭开关会取消，请求期间关闭开关或组件卸载不再弹窗。内联生命周期验证见 `update-watcher-lifecycle.json`，未新增测试文件。v2.0.1 的正式发布和三平台构建已通过，记录见 `release-v2.0.1.json`。

v2.0.3 修复证书页并发加载时误报 `SERVICE_BUSY`：证书列表、体检、导入列表和站点证书选项共用读锁，签发、替换和删除仍持有独占锁。原生窗口复现后，同一控制器 8 路并发的 100 次读取从 97 次忙碌错误降到 0 次；随后签发、导入和删除均通过。证据见 `cert-read-concurrency-before.json` 和 `cert-read-concurrency-after.json`。

补充验证了证书私钥不匹配时保留原文件、替换与四种格式导出、计划任务执行/停止/运行中防重复操作，以及配置非法内容拒绝、过期编辑和回滚预览拒绝、备份恢复原内容。报告为 `cert-cron-roundtrip.json` 和 `config-rollback-roundtrip.json`，均使用隔离目录，未新增测试文件或改动用户现用数据。

v2.0.4 修复 PHP 扩展、Xdebug、Adminer 状态、Memcached 统计/设置查询以及环境体检之间的独占锁争用。只读操作共享生命周期锁；服务修改保持独占，体检使用单独防重复锁，PHP 首次配置生成单独串行，管理台退出状态查询不再修改运行时指针。PHP/Xdebug/Adminer 的 12 次并发读取从 11 次忙碌错误降到 0 次；体检期间 40 次读取从全部失败降到 0 次；Memcached 的 40 次读取从 25 次失败降到 0 次。体检期间重复体检和配置修改仍被正确拒绝。报告为 `php-read-before/after.json`、`php-first-read.json`、`health-read-before/after.json`、`memcached-read-before/after.json`。

新增隔离行为验收：Adminer HTTP 页面、48 次运行状态并发读取、正常停止、仅终止验收实例的 PHP 子进程后清理与重启；PHP curl 扩展开关、原生 ini_get 核对、内置扩展和无效配置拒绝；从 Xdebug 官方源安装 3.5.3，实际加载/停用/启用并核对调试端口，随后恢复原 php.ini；Memcached 实际写入/清空、128 MiB 内存配置生效及旧 revision 拒绝，结束后恢复设置并停止服务。报告为 `adminer-lifecycle-concurrency.json`、`php-xdebug-roundtrip.json`、`memcached-roundtrip.json`。Go build/vet、类型检查、现有 21 项逻辑检查通过；本机未配置 C 编译器，Go race 检查未能执行，不计入通过项。

v2.0.5 将完整日志导出与配置文本读取分开：原先超过 8 MiB 或非 UTF-8 的日志无法导出，现在按打开时的文件长度流式复制原始字节，经同目录临时文件同步落盘后发布，避免大日志进入 WebView 内存。导出目标不能是源日志、其硬链接或目录。界面完整预览保留 8 MiB 限制并提示使用导出，实时尾部读取不变。13 项验收覆盖 9 MiB、非 UTF-8、空文件、源文件保护、相对路径拒绝、缺失日志与原子文本写入回归；另有 10 项日志/诊断操作核对尾部行数、中文及反斜杠原文、同名导出不覆盖、保留文件名与敏感字段脱敏。证据为 `log-large-before/after.json`、`log-diagnostics-roundtrip.json`，原日志已恢复、临时导出已清理。

v2.0.6 将 MySQL 列表/账号预览/工作台浏览、PostgreSQL 状态与元数据、Redis 浏览/统计/持久化状态、MongoDB 浏览/认证状态及 Ollama 模型列表改为共享生命周期读锁；任意 SQL、数据和权限修改、模型拉取/删除、服务启停继续独占。真实并发复现中，MySQL 64 次有 63 次误报忙碌，PostgreSQL 48 次有 47 次，Ollama 40 次有 37 次；修复后五类服务共 224 次并发读取无错误。MySQL/PostgreSQL 自定义 SQL 运行期间仍拒绝交叉查询和停机，SQL 完成后读取恢复；15 项调用核对确认保护与生命周期行为。报告为 `mysql-read-before/after.json`、`postgres-read-before.json`、`ollama-read-before.json`、`databases-ollama-read-after.json`、`redis-read-after.json`、`database-read-write-locks.json`。Redis 初始端口冲突被正确拒绝，随后使用隔离端口 29679 验收。

Ollama 0.35.0 官方发行包已实际下载、校验并安装，真实拉取 `all-minilm:22m`、取消后重试、模型列表及删除通过；错误名称、旧任务取消、重复拉取和重复删除均被拒绝。模型验收后删除，受管服务已停止，报告为 `ollama-install.json` 与 `ollama-roundtrip.json`。MCP 客户端接入另有 20 项隔离验证，包括真实握手、工具查询、Cursor/Claude Desktop/VS Code 接入和移除，以及保留其他配置与拒绝过期修改；未改真实客户端配置，报告为 `mcp-client-roundtrip.json`。

v2.0.7 修复站点编辑的嵌套对象更新语义：未提交的顶层字段继续保留，提交的 runtime 和 phpOverrides 按完整编辑快照替换。原先 Go 对旧结构体和 map 的递归解码会让已清除的自定义伪静态、HTTPS 跳转、错误页及 PHP 参数残留；现在与前端对象替换保持一致。隔离复现与修复后 17 次调用覆盖关闭 HTTPS、删除模板、删除单个/全部 PHP 参数、错误页缩减、仅改名称保留设置、非法 runtime 拒绝且原数据保留，以及运行中 Nginx 重载后的配置内容。证据为 `site-replacement-before.json`、`site-replacement-after.json`。

补充项目与模板验收：项目运行时配置 13 次调用验证版本固定、过期编辑拒绝、未知字段保留、原文备份和版本文件冲突；四个真实框架项目及两个无 Composer 项目的兼容性/环境检查共 16 次，另有缺失扩展、PHP 不兼容和开发依赖分组选项等 5 次边界调用。内置 60 份模板均逐项保存并回读，分别用于真实 Nginx、Apache、Caddy 的校验、启动和重载。报告为 `project-runtime-roundtrip.json`、`project-platform-real-projects.json`、`project-platform-edge.json`、`rewrite-template-save.json`、`rewrite-template-native-after.json`。项目文件及隔离站点设置已恢复；未改用户现用数据库或系统 hosts。

## 仍需完成的验收

- 全页面完整交互矩阵与任意第三方自定义配置，不以已观察的主要页面代替。
- macOS 两架构原生构建已通过；安装与服务交互仍需原生环境验证，本机 Windows 结果不能代替。
- 真实 ACME CA、SMTP、远程 SSH/云部署、真实 AI 服务需要对应服务和凭据；本地协议验证不能冒充联网验收。
- 相同环境、页面、数据和操作的 Tauri/Wails 性能基线对比；没有可比基线时不宣称固定节省比例。
- v2.0.0 annotated tag、原子 push、远程 SHA 和 Actions/Release 已验证；后续修复递增补丁版本，不移动已发布 tag。
