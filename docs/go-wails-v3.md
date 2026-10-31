# Go + Wails v3 迁移状态

当前工作直接在 `main` 上进行，**尚未完成全量替换，不可作为正式版发布**。
现有 React 19 + Vite 界面继续复用。Go 后端不调用 Rust 可执行文件，不使用模拟数据冒充本机结果。
原 Rust 核心和 Tauri 壳仍保留，作为业务行为的对照及未迁移功能的现用实现。

## 已落地的基础

- `cmd/niceenv`：Wails v3 原生窗口、关闭到托盘、托盘导航、Go IPC 和原生事件。
- `internal/desktop`：唯一绑定服务 `Bridge.Invoke`，结构化错误，初始化在后台进行。
- `internal/core`：纯 Go SQLite 存储、公共设置白名单、服务清单和生命周期锁、受管进程树、计划任务执行、服务状态快照、站点/证书记录读取、套件清单读取、文本文件读写、合并系统采样请求。
- `cmd/niceenv-cli`：共用 Go 命令注册表的单次调用入口；不启动 WebView、计划任务或受管服务。
- 前端业务组件使用 `isDesktop` 和统一的文件、窗口适配器；Wails 检测使用原生资产处理器注入的标识，浏览器预览独立使用 mock。
- Wails 能力清单缓存阻止未迁移接口持续发送 IPC；错误明确提示未迁移，不返回假成功。
- 运行/错误状态阴影改为静态，减少空闲重绘；CPU/内存/磁盘采样共享结果，每次最多一个后台任务，历史记录有上限。

## 明确未完成的部分

注册了命令不等于该功能全部迁移完成。以 `pnpm check:wails-port` 的实时清单为准。
旧命令共有 **328 个**，其中包括之前容易漏掉的 5 个托盘命令。

- 设置写入目前只接受已移植的公共偏好；自启动、启动服务栈、伪静态模板及服务相关设置拒绝写入。
- 开发壳关闭自动启动服务；`startup_stack_status` 的 disabled 表示开发版本策略，不表示成功启动过服务。
- 通用清单服务已具备受管启动、状态快照、端口预检、监听进程归属核验、日志和停止确认边界；Nginx 已接入原生 `-t`、PHP upstream 修复和优雅退出。PHP/MySQL 等专用服务仍必须接入各自初始化适配器。
- 套件列表支持内置清单、本地清单、用户模块、安装快照及安装记录合并；通用 archive/tar.gz/binary 安装、SHA-256 缓存、路径安全、切换版本和快照恢复已迁移。npm/pnpm 专用元数据、PATH 同步和完整卸载引用检查仍待迁移。
- 服务进程树所有权、生命周期锁、端口冲突和停止确认的基础已迁移；孤儿进程恢复、数据库专用停机和完整健康检查仍待移植。
- MySQL/MariaDB/PostgreSQL/Redis/MongoDB 的管理、权限、查询、备份恢复和定时备份尚待移植；通用配置编辑已支持原子保存、版本绑定备份和 Nginx 原生体检。
- 证书签发/部署/监测、站点文件管理、AI 对话与操作确认、代理、DNS、隧道、计划任务执行、CLI/MCP 外部控制等仍有缺项。
- 正式版数据目录交接、应用更新、安装包、签名、公证、旧安装升级和发布工作流尚未切换。
- 托盘当前是原生菜单，尚未迁移旧托盘面板及其服务操作。

## 目录与数据保护

开发默认目录：`os.UserConfigDir()/NiceEnv-Wails-Development`。
可通过进程环境变量 `NICEENV_WAILS_HOME` 指定独立空目录。
不读取现用 `NSB_HOME`、旧数据目录选择文件或安装路径，不自动接管任何旧服务。
目录必须为空或含有效 `.niceenv-wails-development` 标记；不允许直接打开现用 NiceEnv/FlyEnv 数据目录。

复用现有 SQLite 十张表的字段；运行时 DDL/DML 模板已记录到根目录 `update.sql`，不是需要在业务 MySQL 执行的脚本。
路径只在 `Path` 序列化或系统打开边界转换；文件原文、SQL、密码等任意文本不做全局替换。

## 编译与核验

Go 模块最低版本 1.25，Wails 固定 `v3.0.0-alpha2.117`，前端运行时固定与该 SDK 源码一致的 `3.0.0-alpha.97`。
现有本机 Go 1.23 不满足要求。使用独立新工具链时，`GOROOT` 必须与工具链一致；不要修改用户系统全局 Go 配置。

源代码编译检查不要求前端产物，也不会隐式运行前端 dev/build：

```sh
go build -mod=readonly ./...
go vet -mod=readonly ./...
pnpm --filter @nsb/web check
pnpm check:wails-port
```

带界面的 Windows 可执行文件需要先显式构建当前前端，再 `go build -tags desktopassets -o NiceEnv-Wails.exe ./cmd/niceenv`。
本任务遵守禁止执行前端 build/dev 的要求，未执行这一步，也未启动 GUI。
缺少 `desktopassets` 的二进制启动时会说明资产未嵌入；带资产的版本会检查 bridge 元标记，拒绝把旧前端误当成 Wails 界面加载。

可用 `go run ./cmd/niceenv-cli -command backend_capabilities` 只核验注册接口。
`-stdin` 从标准输入读取参数，敏感参数不应放在命令行参数中。
`.github/workflows/wails.yml` 只编译/静态检查并输出接口缺项，不发布安装包。

发布前必须执行 `node scripts/wails-port-status.mjs --require-complete` 并逐项验证行为契约。
该命令只检查注册缺项，不证明数据安全、行为兼容、运行性能或平台验收通过；不能用空实现补齐数量。
全量功能和 Windows/macOS 验证未通过之前，不创建正式版 tag，也不将更新通道指向开发壳。

## 当前验证记录

- Windows Go/Wails 编译和 `go vet`；前端 `tsc --noEmit`。
- 使用 Go 1.25.13 复核最低工具链系列；设置、204 条套件记录、服务栈和系统指标通过现有前端 Zod schema 校验。
- Wails 官方绑定生成器识别 1 个服务、1 个方法，调用名与前端一致。
- 独立 `.wails-home` 中验证：设置读写/校验/保护、内置服务栈保护、增删复制与排序去重、204 条 Windows 套件清单、空记录读取、中文文本读写、SQLite 完整性检查、未知命令明确失败、系统指标读取。
- 含中文、空格且长度为 305 字符的数据路径已通过 SQLite 初始化和路径返回核验；未验证 UNC 网络共享。
- 没有修改、启动或停止现用 NiceEnv/FlyEnv 数据库。
- 未新增测试文件，未执行前端 dev/build，未做 GUI 交互、实际空闲 CPU 对比、macOS 原生验证或正式安装包验收。
