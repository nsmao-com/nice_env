# NiceEnv 质量复查记录

本记录用于跟踪 Windows/macOS、路径与数据目录、上游版本和安装状态的复查。**整体复查未完成**；代码编译、单元测试、下载包核验和原生服务运行是不同的验收层级，不能相互替代。

## 2026-10-03：v0.2.246 修复批次

| 范围 | 发现与修复 | 验证情况 |
| --- | --- | --- |
| ACME 请求 | v0.2.245 已修正 JWS POST 的 `application/jose+json`；本批增加本机 HTTP 接收端回归，覆盖账号、订单、POST-as-GET 和错误提示 | 本机 HTTP 接收端验证通过；未代用户注册公网账号或签发新证书 |
| 系统代理 | reqwest 禁用了默认 feature，却未启用 `system-proxy`；补充 Windows/macOS 系统代理支持，本机 mihomo 控制接口强制直连 | 相同 Rust checker 的 phpMyAdmin 请求由连续失败变为在线成功 |
| Nginx/Adminer | Adminer 脚本路径遗漏 Windows 扩展前缀清理，空格目录 include 未加引号；公共 FastCGI 参数覆盖固定入口 | 真实 Nginx/PHP 修复前返回 404，修复后能读取带空格目录中的脚本，路径、文档根和查询参数回读正确 |
| 数据目录 | macOS 新装数据不再写入 `.app` 内；cargo 交叉编译产物同样排除便携目录；写探针不覆盖用户文件；Unix 原生参数保留合法反斜杠 | Windows 路径用例通过；真实 macOS 运行待 CI 验证；已有 `.app` 内的数据保留读取，尚未自动搬迁 |
| 安装记录 | 无安装快照时恢复记录优先匹配本机架构，避免先选到另一架构的入口 | 隔离文件与安装记录回归通过 |
| macOS Intel | MySQL/MongoDB 上游原先固定选择 ARM64；Intel 清单缺 7 类套件；补录正确架构并废弃旧目录缓存 | 7 类官方包完整下载，核对 SHA256；除通用 Composer 外逐一核对 Mach-O CPU、可执行入口；尚未在 macOS 实际启动 |
| macOS NATS | ARM64 的 `.tar.gz` 被声明为 ZIP | 双架构实际压缩包、入口、CPU、SHA256 已核对，修正为 `targz` |
| Redis 5 外部备份 | 包目录版本为 5.0.14，服务及 RDB 版本为 5.0.14.1，导入后误拒绝恢复 | 改为在不一致时读取当前二进制实际版本并精确核对；原生完整回归通过（约 194 秒） |
| SFTPGo 资源路径 | 旧验证按反斜杠字符串判断，误报已规范化路径 | 改为通过传入路径读取实际资源文件，单项通过 |
| 包内根目录 | MongoDB ARM64 实际目录为 aarch64，各版本连字符不同；逐包读取后修正清单。仅根目录不同的安装归位改为移动整个目录，保留辅助程序和库 | 安装器回归通过，实际文件内容核对确认辅助程序和库仍在一起 |

新增 Intel 条目：MySQL 8.4.11（官方构建要求 macOS 15）、MongoDB 8.3.11、mihomo 1.19.31、Go 1.27.1、Node.js 24.21.0、Composer 2.10.3、NATS 2.15.0。MongoDB 压缩包内实际根目录含双连字符，清单按实际目录记录。现有 mongosh / Database Tools 维持双架构支持。

上游实际查询：Windows 首轮 52/53（Consul 超时）、Intel 首轮 8/9（Database Tools 超时）、ARM64 9/9。两项失败分别强制重试后均在线成功，返回 40 / 43 个版本。这里确认的是元数据查询能力，首次失败仍作为网络稳定性待查证据，不等于所有版本已安装验收。

### 已取得的 Windows 原生验证证据

- Nginx 1.31.6：原生语法校验、非法配置拒绝、不改写正在编辑的配置、带空格数据目录下 Adminer 经真实 PHP 执行。
- PHP 8.4.26：FastCGI 实例回读配置，memory_limit 及扩展开关跨重启保留。
- MySQL 8.0.46：隔离实例认证、备份、恢复、导入；原生配置解析器确认参数优先级与调优保留。
- Redis 8.10.2：配置读回、稳定回退端口、持久化、备份恢复、认证设置及清理隔离进程。
- 证书部署：原生 Nginx HTTPS 握手实际返回更新后的导入证书。
- 以上使用独立临时数据目录、端口和进程；未操作用户现有数据库。

### 验证命令与发布门槛

```text
cargo test -p nsb-core -p platform --locked --no-fail-fast -- --test-threads=1
cargo check --locked -p niceservbay
cargo clippy --locked --workspace --all-targets
python .github/check-rust-format.py <本批之前的提交>
pnpm --filter @nsb/web run check
pnpm --filter @nsb/web run check:logic
cargo run --locked -p nsb-core --bin check_versions -- --all --json --force
```

原生服务回归使用现有 `#[ignore]` 用例及 `NSB_NGINX_ROOT` / `NSB_PHP_ROOT` / `NSB_MYSQL_ROOT` / `NSB_REDIS_ROOT` 指向现有程序，`NSB_REDIS_VERSION` 指定包版本。未运行前端 dev/build，未新增测试文件。

新增 CI 矩阵：`windows-latest`、`macos-15-intel`、`macos-latest`，各自执行 core/platform 测试并保存日志。矩阵中的服务测试仍须提供真实程序；默认 ignored 的用例不计入通过。只有远程分支及 annotated tag 均指向发布提交、release workflow 实际触发，才可汇报已推送；安装包须以 workflow/release 最终状态为准。

本地完整回归已通过 core 735 项、集成测试 45 项、platform 15 项，另 54 项 ignored。之后增加的包根目录归位已通过安装器回归（35 项通过，2 项 ignored）和依赖回归（7 项通过）。桌面端 `cargo check`、Clippy（保留存量警告）、改动行格式检查、前端类型检查与现有逻辑验证通过。

## 后续仍需完成

- [ ] 完整平台 CI 实际结果及失败修复，尤其 Unix 专有用例。
- [ ] macOS Intel/ARM64 原生安装、启动、停止、升级、卸载与应用更新，含 `.app` 外的数据位置。
- [ ] Windows 各套件的下载、解压、配置、运行和已安装状态完整矩阵；上游在线不等同安装成功。
- [ ] 全部配置格式的 Unix 反斜杠、引号、空格与 Windows UNC/扩展前缀审计；`nginx_path` 的跨格式复用仍需检查。
- [ ] 安装器归位的剩余边界：多候选入口、不同层级结构、归位目录冲突；仅根目录名称不同的情况已修复并回归。
- [ ] 旧 macOS `.app` 内数据的显式迁移与更新前保护。
- [ ] 套件页后台加载、单项刷新、错误与缓存并发，以及安装/升级/卸载后界面同步。
- [ ] ACME 手动 TXT / 自动 DNS 全流程、失败重试、续签；涉及公网真实签发时记录所用 CA 与结果。
- [ ] 其余页面与服务的真实交互验收，不能以浏览器 mock 成功代替桌面 IPC 成功。

本批未改数据库结构，未修改 `update.sql`。

## 2026-10-03：v0.2.247 平台失败修复

v0.2.246 的真实平台 CI（run 37126180338）结果：Linux 与 Web 通过，Windows、macOS Intel、macOS ARM64 失败。失败不能由本机 Windows 通过替代，已下载三个平台完整日志核对。

- macOS 两种架构的证书部署、账号密钥和站点备份均被系统 `/var` 软链接误拦截。统一只展开 `/var`、`/tmp`、`/etc` 且 `read_link` 确认指向预期 `/private/...` 的系统别名；保留后续目录的链接检查。Redis 和 MongoDB 外部备份路径同步修正。新增现有 Rust 模块用例确认别名可读、用户嵌套链接仍被拒绝。
- macOS 进程退出检测使用 `PROC_PIDT_SHORTBSDINFO` 并传入 `arg=1`，包含尚未回收的 zombie。Apple XNU `proc_info.c` 明确只有非零 arg 才查 zombie；原有 arg=0 与 `kill(pid, 0)` 组合会误判。保留进程身份验证，正常停机用例在父进程回收前检查已退出。
- POSIX PATH 赋值去掉命令替换外层不必要的双引号，规避 macOS 旧版 sh 对包含单引号、反引号的 case 模式的解析缺陷；变量赋值上下文不进行分词。
- Windows 计划任务先把自己的隐藏子控制台代码页设为 UTF-8，避免英文系统 OEM 代码页在输出前就把中文替换成问号。未改变系统代码页。
- Release workflow 新增三平台核心验证前置任务，全部成功才开始发布安装包。

本地完整回归通过：core 736 项、集成测试 45 项、platform 15 项，54 项 ignored。桌面端 `cargo check --locked`、core/platform Clippy（存量警告）与改动行格式检查通过。新增 macOS 专有用例必须等待远程真实 runner 验证；不能把 Windows 通过当作 macOS 修复完成。

补充 v0.2.246 发布后的原生证据：官方 Apache 2.4.69 实际下载、校验、安装、重复安装、HTTP 200/403、日志、停止及卸载通过；官方 Nginx 1.31.6 下载、安装、`-v`、重复安装及卸载通过。

本批未新增测试文件，未运行本地前端 dev/build；没有数据库变更，未修改 `update.sql`。整体质量复查仍未完成，后续清单继续有效。

## 2026-10-03：v0.2.248 路径兼容与目录请求顺序

v0.2.247 远程 macOS 验证仍失败，Release 的三平台前置门槛已拦截安装包发布。完整日志显示：路径校验返回的 `/private/var/...` 与调用方的 `/var/...` 不同，造成后续 `strip_prefix` 归属检查误拒绝；原终端脚本调整仍不足以兼容 macOS sh。已据此继续修复：

- `checked_data_path` 仅在逐层检查时解析系统别名，返回值保持传入 base 的路径写法，避免改变既有目录归属约定。别名用例同时核对返回路径和嵌套链接拦截。
- PATH 脚本改用带引号的逐项等值比较，不把用户路径放进 shell `case` 模式；macOS 同时验证实际使用的 zsh 与系统 sh。
- 已核实进程的身份在退出阶段暂时不可读时，返回“尚未确认退出”交给调用方的有界等待继续检测，保留身份校验及超时失败，不把不确定状态报告为停机成功。
- Windows 远程仍暴露中文输出为 `????`。本机将隐藏子控制台设为 OEM 437 后复现：cmd 缓存启动代码页，同一行 chcp 后 echo 无效。改为外层设置 UTF-8，再启动实际执行命令的 cmd；通过环境变量的延迟展开传递原命令，避免 `%`、`!`、`^` 被重复解释。实测中文、emoji、转义管道/与符号、环境变量仅展开一次及非零退出码均保留；计划任务 10 项回归通过。
- 套件页竞态已在修改前复现：较早的后台请求失败，会清空较新的手动刷新结果。统一每个 QueryClient/套件的请求顺序，同步所有相关列表，合并重复刷新并保护加载状态；最新失败保留已知版本并显示错误，重试成功清除错误。

前端使用源代码与实际 TanStack QueryClient 的内联异步验证，覆盖旧成功/失败晚返回、列表同步、去重、加载状态、失败保留、重试恢复、返回值与缓存一致、初始并发限制 6。未新增测试文件。类型检查与原有日志逻辑检查通过；原有 `check:logic` 本身只覆盖日志高亮。浏览器工具访问策略阻止了当前 localhost 页面的读取，未取得本轮桌面/移动页面截图，不能把内联验证当成桌面 IPC 全流程验收。

本地核心完整回归再次通过：core 736、集成 45、platform 15，54 项 ignored；桌面端编译检查及 Rust 改动行格式检查通过。之后的终端和计划任务变更分别完成对应回归。补齐此前漏同步的 `packages/schema/package.json` 版本，并在发布流程检查全部工作区 package 版本。

本批没有数据库变更，未修改 `update.sql`，整体复查继续进行。

## 2026-10-03：v0.2.249 终端脚本兼容

v0.2.248 的 Windows、Linux、Web 检查通过，Windows Release 前置验证也通过。macOS Intel 与 ARM64 均为 726 项通过、1 项失败、44 项 ignored；唯一剩余失败为终端 PATH 脚本。两个平台完整日志均定位到命令替换中剩余的 `case` 分支解析错误，Release 安装包因此跳过。

将剩余分隔符判断改为当前剩余字符串与取出的首项比较，消除命令替换中的 `case`，仍保留空 PATH 项、字面路径和重复应用幂等性。本地现有终端回归与改动行格式检查通过；macOS sh/zsh 结果以本版本真实 runner 为准。

本批没有数据库变更，未修改 `update.sql`，未新增测试文件或运行前端 dev/build。整体质量复查仍在进行。

## 2026-10-03：v0.2.250 原生路径与 macOS 退出竞态

v0.2.249 的 Windows、Linux、Web、macOS Intel CI 通过。ARM64 Release 核心验证通过，但同提交的 ARM64 分支 CI 出现一次控制服务停机失败：退出过渡期无法读取进程身份，立即返回 STOP_FAILED。PATH 的 sh/zsh 用例已通过。为避免发布已知仍有停机问题的构建，已请求取消 v0.2.249 Release（GitHub 返回 202）。

- 命令参数、环境变量、文件归属检查不再使用会无条件改写反斜杠的 `nginx_path`。Nginx 启停与校验、Qdrant 快照、SFTPGo 环境文件、配置备份及恢复改用原生平台路径文本。托管相对路径不支持的字符仍拒绝，避免把 Unix 的字面反斜杠误指向另一文件。
- 数据迁移只在 Windows 保留反斜杠样式；Unix 安装根目录末尾的字面反斜杠不再被 PATH 推导删除。已有用例增加 Unix 路径和 SFTPGo 文件错读的回归场景。
- macOS 创建时间查询也包含 zombie；在 open、正常停机、强制停机发送信号前，对暂时不可读的身份最多重试 1 秒。未确认身份不发信号，PID 身份改变或确认退出时不再操作。新增现有平台模块的 Unix 退出过渡期重复终止回归。
- Release 的 macOS 双架构前置检查增加真实官方套件验收：MySQL、MongoDB、mihomo、NATS、Go、Node、mongosh、Database Tools 与 Composer，使用带空格的临时数据目录。验证原生版本、已安装列表、离线记录重载、重复安装和卸载保留数据；四类服务执行两次启停。Composer 仅验证 PHAR 安装，执行仍缺 PHP 依赖。版本升级、桌面交互和全部配置转义尚未验收。

路径修复的本地完整回归通过 core 736、集成 45、platform 15；之后的 macOS 退出修改完成 Windows 编译和控制服务定向回归，真实 macOS 行为必须以新 runner 结果为准。新增套件验收代码已编译，未在 Windows 冒充 macOS 执行。仅格式化本批改动行，改动行检查通过。

本批没有数据库结构或用户数据库变更，未修改 `update.sql`；未新增测试文件，未运行本地前端 dev/build。整体质量复查保持进行中。

## 2026-10-03：v0.2.251 签发重试、应用包数据保护与原生验收结果

v0.2.250 的 Windows、Linux、Web、macOS Intel 分支 CI 通过。ARM64 分支 CI 与 Intel Release 前置验证暴露目录迁移回滚的间歇性 `RESTART_CHILD_CLEANUP_FAILED`；ARM64 Release 核心验证为 727 项通过，之后实际套件验证有一项验收运行环境错误。因此本版 Release 未生成安装包；v0.2.249 的三个安装包构建均已确认 cancelled。

ARM64 原生证据（均经应用安装器下载，临时目录含空格）：MySQL 26.7.0、MongoDB 9.0.2、NATS 2.15.0 完成两次启动/健康检查/停止；Go 1.27.1、Node 26.10.0、mongosh 2.12.0、Database Tools 100.19.1 实际执行版本命令。上述套件及 Composer 2.10.3 均通过安装列表、离线元信息、重复安装及卸载保留数据。Composer 只验证 PHAR 安装。mihomo 1.19.31 已执行原生版本命令，但同步 HTTP 校验在异步测试上下文中触发 Tokio panic，尚不能报告服务验收通过。Intel 因前置失败尚未执行套件验收。

- 将原生套件验收改为同步函数，仅下载安装在专用 Tokio runtime 中执行，匹配桌面服务的同步调用环境。
- macOS 进程组消失检查补充 libproc 完整成员枚举，排除已退出的 zombie，保留权限错误与不完整枚举的失败状态。Apple XNU `proc_info.c` 与 `libproc.c` 确认成员枚举含 zombie，`proc_listpgrppids` 返回 PID 个数，零值需结合 errno 判断。迁移清理时，Unix 仅在子进程已回收且整组确认不再运行后回滚，不再因退出竞态中单次信号错误永久阻止回滚；未知状态仍不回滚。
- ACME 注册账号与既有账号共用最多三次 badNonce 重试，复用响应里的 Replay-Nonce。原生本机 HTTP 接收端覆盖账号、订单和 POST-as-GET 的 JOSE 请求头、正确 jwk/kid、过期 nonce 重试、新 nonce 连续使用及三次失败上限。常见邮箱、EAB、限流、域名验证、域名不支持和 CA 服务异常提供中文说明，原始详情保留。
- macOS 应用内下载更新与安装更新前检查数据位置。旧数据仍在 `.app` 内，或通过符号链接指向包内时，提示先用设置中的现有迁移流程移到包外；不自动搬动数据。此保护不拦截用户在 Finder 中自行替换旧应用包。

本地 ACME 8 项、重启 4 项、目录 21 项回归通过，桌面端 `cargo check --locked -p niceservbay` 和改动行格式检查通过。macOS 新增的进程组判断、符号链接边界及完整套件验收继续交给真实双架构 runner，未以本机结果代替。

没有数据库结构或用户数据库变更，未修改 `update.sql`；未新增测试文件、未运行本地前端 dev/build。剩余清单继续有效，整体目标仍未完成。
