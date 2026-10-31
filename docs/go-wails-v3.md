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

备份列表复用单次请求内的目标索引，避免每份历史备份重复扫描目录；预览和恢复仍重新读取并校验版本。runtime 包资源不按扩展名全量改写，Web 配置按实际 include 和资源引用规划。无法安全确定的动态资源路径、循环、跨用途共享和字符类通配符会拒绝自动迁移并保留源目录，不能将这种保护理解为任意自定义配置均可无交互迁移。

最新前端资源已构建并用于 Windows 安装包；继续验收时未再次运行前端 dev/build。GUI 已观察首页、套件、数据库、环境变量、配置备份和 AI 面板；AI 面板不模糊背景。真实 AI 账号未用于发送内容。一次 UI 输入工具坐标错误后，重新捕获窗口并使用键盘退出引导，后续页面导航恢复。

空环境、计划任务页静置 15.2 秒采样：桌面与 WebView2 子树 CPU 合计约为单核心的 32.99%，20 逻辑处理器下折算整机约 1.65%；工作集合计约 671.1 MiB，可能重复计算共享页。该采样不是所有页面的性能上限，也没有同场景 Tauri 基线。

v2.0.0 站点空页面另一次 20.15 秒采样：进程树 CPU 约单核心 29.08%，整机 1.45%。这是闲置开销的观测值，不是性能优化已达标的证明；报告为 `gui-sites-idle.json`。包管理器与 MySQL 新增证据为 `js-versions-install.json`、`js-versions-switch.json`、`mysql-workspace-roundtrip.json`。npm/pnpm 目录现按 SemVer 识别预发行版本，并在读取旧缓存时重新筛选，防止 `-pre.*` 混入正式列表；其它发行源的厂商版本格式保留原有规则。

## 仍需完成的验收

- 全页面完整交互矩阵与任意第三方自定义配置，不以已观察的主要页面代替。
- macOS 两架构原生构建已通过；安装与服务交互仍需原生环境验证，本机 Windows 结果不能代替。
- 真实 ACME CA、SMTP、远程 SSH/云部署、真实 AI 服务需要对应服务和凭据；本地协议验证不能冒充联网验收。
- 相同环境、页面、数据和操作的 Tauri/Wails 性能基线对比；没有可比基线时不宣称固定节省比例。
- v2.0.0 annotated tag、原子 push、远程 SHA 和 Actions/Release 已验证；后续修复递增补丁版本，不移动已发布 tag。
