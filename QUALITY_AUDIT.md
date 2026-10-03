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

- [x] v0.2.252 的 Windows、Linux、Web、macOS Intel/ARM64 CI 全部成功；后续改动仍须重新通过对应平台门槛。
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

## 2026-10-03：v0.2.252 配置路径转义与 macOS 僵尸进程组

v0.2.251 的 Windows、Linux 与 Web CI 通过。macOS Intel/ARM64 的 core 与集成用例通过，但平台回归均在 `process_group_gone` 检查已退出且未回收的子进程组时返回 `EPERM`；Release 前置验证同样失败，安装包构建 skipped，未发布新安装包。

- Apple 官方 XNU `bsd/kern/kern_sig.c` 的 `killpg1` 明确跳过 zombie，组存在但没有可发送信号的成员时，POSIX 模式返回 `EPERM`。现在 macOS 的此分支同样执行已有 libproc 完整成员枚举；只有所有成员已不再运行才判断完成，无权限或枚举不完整仍不能报告清理成功。保留反复终止、回收前确认退出的现有回归。
- Nginx、PHP ini、MySQL ini、Redis 配置路径使用独立的双引号编码：Windows 清理扩展路径前缀并使用正斜杠；Unix 保留字面反斜杠，再转义反斜杠和双引号。命令参数不使用配置编码。
- Nginx include 在 Unix 上另行处理 glob 层的反斜杠与通配字符，避免合法目录里的 `\\`、`[` 等被当作模式。同步托管 include 时保持 Unix 路径身份，不删除指向不同正斜杠目录的用户 include。Windows 的历史 `//?/` 去重继续验证。
- Nginx 指令读取按上游 `ngx_conf_read_token` 处理 `\\t`、`\\r`、`\\n`、引号及反斜杠；未知转义保留，避免同步时误认路径或正则。

实际解析器验证：PHP 8.4.26 的 `ini_get` 与 MySQL 8.4.11 的 `--print-defaults` 完整回读字面反斜杠、双引号、尾部反斜杠、单引号与空格样本；Nginx 1.31.6 配置校验、Redis 5.0.14 配置解析的预期“目录不存在”错误同样回显原始路径。后两者使用不存在的临时验证目标，不启动服务；这项证据是配置词法解析，不能代替 Unix 文件系统上的实际服务验收。

本地完整回归通过 core 737、集成 45、platform 15，55 项 ignored；桌面端 `cargo check --locked` 与改动行格式检查通过。额外运行现有原生 Nginx 配置校验和 MySQL 参数解析用例，两项通过。Unix 新增路径用例及 macOS 进程组回归仍需远程 runner 确认。

Apache 的正则/URL、Caddyfile、通用模板及数据迁移仍有独立格式规则待处理，未将本次修复描述为全部服务支持完成。没有数据库变更，未修改 `update.sql`；未新增测试文件，未运行本地前端 dev/build。整体复查继续进行。

## 2026-10-04：v0.2.253 Caddy、通用配置模板与 MariaDB 路径

v0.2.252 的 [分支 CI](https://github.com/nsmao-com/nice_env/actions/runs/37134197742) 五个平台任务全部成功；[Release](https://github.com/nsmao-com/nice_env/actions/runs/37134197774) 三个原生验证任务与三个安装包构建全部成功。Windows x64 安装器、macOS Intel/ARM64 的 DMG 和 app 归档均已实际出现在 Release 附件中。

macOS 双架构原生套件验证已执行完成：

| 套件 | Intel | ARM64 |
| --- | --- | --- |
| MySQL | 8.4.11 | 26.7.0 |
| MongoDB | 8.3.11 | 9.0.2 |
| mihomo | 1.19.31 | 1.19.31 |
| NATS | 2.15.0 | 2.15.0 |
| Go | 1.27.1 | 1.27.1 |
| Node.js | 24.21.0 | 26.10.0 |
| mongosh | 2.12.0 | 2.12.0 |
| Database Tools | 100.19.1 | 100.19.1 |
| Composer | 2.10.3 | 2.10.3 |

全部完成下载安装、已安装列表、离线记录重载、重复安装及卸载保留数据；MySQL/MongoDB/mihomo/NATS 完成两次启动、健康检查与停止。Go/Node/mongosh/Database Tools 执行真实版本命令，Composer 仅验证 PHAR 安装，未验证 PHP 执行。以上不代表版本升级、协议级持久化或桌面 IPC 验收已完成。

- Caddy 路径保留 Unix 字面反斜杠，按 Caddyfile 规则选择双引号、raw backtick 或 heredoc。导入路径另行转义 glob；识别已有 import 时比较解码后的参数，避免不同引用形式产生重复导入。
- 通用模板单次展开路径，目录名中的 `{etc}` 等字面文本不会再被当作占位符替换。YAML、dotenv、MySQL/MariaDB ini 按各自的字符串规则编码；dotenv 路径中的 `$` 保持字面值。只修复同一配置段内与旧模板完整匹配的路径行，保留其他配置段、自定义路径、注释及换行格式。
- MariaDB 数据目录读取按 MySQL option-file 规则处理引号、行内注释及反斜杠转义；不再把 Unix 上两个不同目录的字面反斜杠与分隔符混为一谈。写出的受管 datadir 同样经过配置字符串转义。

本地完整回归通过 core 738、集成 45、platform 15，55 项 ignored；最终配置段限制补改后，generic 定向回归 27 项通过、13 项 ignored，桌面端 cargo check 和改动行格式检查通过。未将 ignored 用例计为成功。

真实 Caddy 2.11.4 adapter 回读尾部反斜杠、反引号/双引号组合路径，原生站点 HTTP/HTTPS/访问限制/日志/启停流程和端口跨重启同步均通过。YAML/dotenv 使用实际解析器回读；MySQL 8.4.11 `--print-defaults` 核实引号、反斜杠、`#` 注释和分号字面值规则。Unix 文件系统下的新增路径用例仍需本版远程 runner 确认。

本机已安装程序此前核实仍为 0.2.244，尚未包含 v0.2.245 的 ACME JOSE 请求头修复；v0.2.252 安装包已包含该修复。未代用户运行安装器或注册公网 ACME 账号。

Apache 的正则/URL、数据迁移的多格式编码、剩余套件矩阵与真实桌面交互继续待查；整体目标保持进行中。没有数据库结构或用户数据库变更，未修改 `update.sql`；未新增测试文件，未运行本地前端 dev/build。

## 2026-10-04：v0.2.254 Apache 路径、PHP 路径参数与站点入口

v0.2.253 的 [main CI](https://github.com/nsmao-com/nice_env/actions/runs/37136814693) 五个平台任务全部成功；[Release](https://github.com/nsmao-com/nice_env/actions/runs/37136814542) 三个原生验证任务全部成功。记录时 macOS Intel/ARM64 的 DMG/app 附件已生成，Windows 安装器仍在构建，因此未将本版报告为所有安装包完成。

- Apache 上游已发布 2.4.69，2.4.68 的旧下载地址返回 HTML。应用在线版本查询正确返回 2.4.69、261002 构建地址与 SHA256；本轮使用官方新包并核实 SHA256 `9b47a2363a71fef6209c88e79743db81311e1753c4564e007141934123132c10`。尚需继续检查请求安装已被上游撤下的旧版本时的错误提示与清单展示。
- 原生 Apache `DUMP_INCLUDES` 复现：数据目录含 `[group]` 时，旧 IncludeOptional 语法通过却没有加载任何站点文件。改用 APR 字符类引用路径字面字符；Windows 不能用反斜杠转义，因为 APR 会先将它当作目录分隔符。Directory 同样支持通配符，现使用相同字面路径规则。
- Apache 普通配置值、DirectoryMatch 正则分别编码，保留 Unix 字面反斜杠。已有配置中的受管 include 按解码后的参数归一去重，其他目录和自定义证书继续保留。
- PHP 转发采用官方文档的 SetHandler 形式，由 Apache 完成实际文件映射后转交 FastCGI。原生 php-cgi 验证确认 Windows 需要去除 SCRIPT_FILENAME 盘符前多出的 URL 斜杠；规则只匹配此形态。默认路由回退移到 Directory 内，使用 `/index.php`，避免丢失 PATH_INFO 或把带特殊字符的磁盘路径当作内部跳转 URL。
- Nginx/Apache 站点快捷入口与配置生成复用同一个 include 路径规则，避免服务已加载而界面没有可访问地址。
- Caddy 2.11.4 原生 adapter 和上游 parse.go 证实，含方括号的导入路径在执行 glob 前即被拒绝，引用和反斜杠不能解决。补充数据目录的提前检查和中文迁移提示，检查在改写配置前执行。此限制仅针对包含 `[`、`*`、`?` 的数据目录，不限制站点项目目录；没有声称 v0.2.253 的 glob 转义已解决 Caddy 的上游语法限制。

真实 Apache 2.4.69 + PHP 8.4.26 回归通过：数据目录含空格/方括号，项目目录含 `#`、单引号、`%1`、`&`、空格与方括号；SCRIPT_FILENAME、SCRIPT_NAME、PATH_INFO、查询参数、首页 DirectoryIndex、普通路由、私有目录 403、`.well-known` 可访问、站点入口、配置保留、端口切换、两次启停均核实。测试实例使用临时目录和独立端口，清理后确认无该 Apache 验证程序遗留进程。

本地完整回归 core 738、集成 45、platform 15 通过，55 项 ignored。最终入口用例 1 项、配置用例 10 项、Caddy 用例 1 项、Apache 原生用例 1 项再次通过；未把一次过滤后执行 0 项计为验证成功。桌面端 cargo check、改动行格式检查通过。Unix 路径新增回归仍须本版远程 runner；未验证 macOS Apache 原生运行或 Windows UNC 共享目录。

本机安装程序再次读取仍为 0.2.244，ACME 请求头修复需使用含修复的新版程序。整体质量复查未完成：数据迁移多格式转义、完整安装/升级矩阵、真实桌面 IPC 和公网证书签发继续待查。没有数据库变更，未修改 `update.sql`；未新增测试文件，未运行本地前端 dev/build。

## 2026-10-04：v0.2.255 Nginx / Apache 数据迁移与备份恢复

v0.2.253 的 Windows 安装器现已生成，五个 Release 附件齐全。v0.2.254 的 main CI 五个任务、Release 三个平台原生验证均成功；记录时 macOS 双架构 DMG/app 已发布，Windows 安装器仍在构建，未将该版报告为全平台安装包完成。

- 数据迁移不再对 Nginx / Apache 配置直接做普通文本替换：复用 Nginx 词法读取参数，Apache 区分普通路径、Include/Directory glob 与 DirectoryMatch 正则，改写后按配置规则引用完整参数。兼容历史 `//?/` include、Apache 续行和旧版 PHP RewriteRule 路径。
- Unix 的字面反斜杠不再当作目录分隔符。Nginx include 在迁移前后开始或停止使用系统 glob 时转换整条参数的相应转义层，保留真正的站点通配后缀。
- 配置参数按完整目录边界匹配，带空格、方括号、点号的同名前缀外部目录不被误改；明确包含上级目录引用时阻止自动迁移。配置引号不闭合时返回错误，源配置及空目标保持不变。
- 备份预览与恢复根据目标配置格式转换历史路径，原备份保持原样；配置编辑器的旧备份恢复入口也传入真实目标路径。迁移排除 `etc/apache/logs`、服务 pid 与 Nginx 临时目录，避免改写历史日志。

13 项目录迁移定向回归通过。真实 Apache 2.4.69 + PHP 8.4.26 在原有路径/端口/重复启停验证后，复制数据目录、将原目录改名为不可用，再从新目录恢复旧备份并完成语法校验及启动。PHP 的 SCRIPT_FILENAME 指向新目录，PATH_INFO 与查询参数保留，私有目录仍为 403，`.well-known` 正常，历史日志逐字保留。隔离实例停止后未留下 Apache 或 24000 段的 PHP 进程。

最终本地完整回归 core 738、集成 45、platform 15 通过，55 项 ignored；桌面端 `cargo check --locked -p niceservbay` 与改动行格式检查通过。版本清单和 Cargo.lock 中三个本项目 crate 同步为 0.2.255；设置页、菜单及浏览器预览均引用 web package.json 版本，已核实同步。

Unix 新增迁移回归需由远程 runner 确认；PHP/MySQL/Redis、JSON/YAML/dotenv、Caddy 与脚本等格式的迁移仍需分别复查，未声称本轮完成所有格式。Windows UNC、剩余套件升级矩阵、真实桌面 IPC 与公网证书签发继续待验。整体目标保持进行中。

没有数据库结构或用户数据库变更，未修改 `update.sql`。只扩展已有 Rust 测试模块，未新增测试文件，未运行本地前端 dev/build。本机安装程序再次核实仍为 0.2.244；已发布的新安装包包含 ACME JOSE 请求头修复，未代用户运行安装器或注册公网账号。

## 2026-10-04：v0.2.256 PHP / MySQL / Redis 配置迁移

v0.2.255 的 main CI 五个任务及 Release 六个任务现已全部成功，Windows 安装器和 macOS 双架构 DMG/app 共五个附件齐全。本机 `D:/NiceEnv/niceservbay.exe` 再次读取仍为 0.2.244；ACME JWS 请求头从 v0.2.245 起已修正为 `application/jose+json`，账号、订单与 POST-as-GET 的本机 HTTP 回归继续通过，不需要因此更换邮箱或 DNS 凭据。

- PHP/MySQL INI 和 Redis 配置迁移改为只转换明确的路径选项。按各自的单引号、双引号、反斜杠与十六进制转义规则解码比较，再保留原后缀的写法；密码、SQL、注释与模块后续参数即使形似旧目录也保持原样。
- PHP 保留 include_path/open_basedir 路径列表、session.save_path 的数字前缀及路径后缀中的环境变量表达式。MySQL/MariaDB 支持路径列表、带空格但不带引号的 !include/!includedir；根据 MySQL 8.4.11 原生解析结果修正引号外 `\#` 的注释边界。
- Redis 5/6 的 include 按普通文件名转换；Redis 7+ 按 POSIX glob 处理，包括 Windows MSYS2 发行版。复用 Nginx 已有的 glob 转换逻辑，区分迁移后开始或停止触发 glob 的情况。
- 原生 Redis 8 验证发现更严重的启动问题：主配置文件路径也会执行 glob，目录含方括号时可能跳过整份配置，按默认设置启动。对 Redis 7+ 的相关启动参数按 glob 规则转义；Redis 5/6 保留原来的参数语义。

原生验证：PHP 8.4.26 迁移后回读 error_log、include_path、session.save_path；MySQL 8.4.11 通过真正的 --print-defaults 检查新日志路径、带空格的 include、引号/注释及未改动的密码，并检查 stderr，而非只看退出码。Redis 5.0.14 与 8.10.2 均在原目录改名不可用后，从新目录恢复旧备份、加载 include，连续两次启动核对数据、密码保护、timeout 与实际数据目录。目标含空格、方括号和 #；隔离服务已停止，未修改用户的服务配置或数据库。

已确认的限制：Redis 8.10.2 在此类方括号路径下，直接执行原生 CONFIG REWRITE 仍返回找不到文件；隔离实测确认原配置保持不变。应用自身的配置保存不使用该命令。PHP PATH 节名、更多扩展路径选项、JSON/YAML/dotenv、Caddy 与脚本等仍需复查；未把本批范围宣称为任意自定义配置全面兼容。

上游目录重新在线查询：Nginx 1.31.6、Apache 2.4.69、PHP 8.5.11。下载器已有 HTTP 200 HTML 错误页拦截；旧清单版本不在最新目录时仍可能回退旧地址的行为继续待查，不能直接把“最新目录未列出”当成“所有旧版本都不可安装”。Windows UNC、macOS 原生套件完整矩阵、真实桌面 IPC 和公网 ACME 签发/续期仍未完成。

最终代码完整回归通过：core 738 项、集成 45 项、platform 15 项，55 项 ignored 未计为通过；两个 Redis 原生用例另行执行通过。桌面端 `cargo check --locked -p niceservbay`、改动行格式检查及 `git diff --check` 通过。macOS/Linux 的新增迁移路径仍以本版远程 runner 结果为准。

本批未改数据库结构，未修改 `update.sql`；只扩展现有 Rust 测试模块，未新增测试文件，未运行本地前端 dev/build。版本清单、桌面配置与 Cargo.lock 中三个本项目 crate 已同步为 0.2.256，三个界面版本兜底继续引用 web package.json。整体复查保持进行中。

## 2026-10-04：v0.2.257 上游失效地址恢复与目录状态同步

v0.2.256 的 main/tag CI 已成功，Release 三个平台的 runtime 验证成功；本批提交前，macOS Intel 安装附件已生成，Windows 与 ARM64 打包仍在运行，不能视为附件已齐全。

- 下载器区分 404/410、HTTP 200 HTML 错误页和 SHA256 不匹配，保留具体错误提示。确实下载失败后最多强制刷新一次上游目录，仅重试用户选择的同一版本；地址或校验值未变时不会循环重试，也不会自动换成其它版本。
- 上游目录离线、含错误或只有缓存时不替换下载信息；同版本同 URL 保留已知 SHA256/大小。校验失败不能靠移除 SHA256 继续安装，更换同版本文件前清除旧下载片段。只有真实下载地址失效且刷新后的在线目录也没有该版本时，才提示改选其它正式版本。
- 套件下拉分别显示已安装、上游正式版、上游预发布和内置清单版本。未被当前目录收录的历史版本仍可尝试存档下载，已安装的旧版本持续可见，最新版本来源按该条目的实际来源标注。
- 安装任务结束后实际重新读取版本目录，修复 disabled query 只 invalidate 却不更新的问题。先等待安装前的目录请求结束再强制读取，失败保留已有列表并显示目录错误；done/error/cancelled 均覆盖。
- CI 增加手动触发的 macOS Intel/ARM64 原生验收，串行下载 MySQL、MongoDB、mihomo、NATS、Node、Go、Composer。包含安装记录、离线快照、重复安装与卸载保留数据，四类服务执行两次启停；Composer 仅验证 PHAR 安装，因为清单尚无 macOS PHP。CI 检查确实执行了一项 ignored 原生用例并保存逐包日志，实际运行结果需在推送后核对。

Windows Apache 2.4.69 实测旧构建地址失效后刷新到同版本新构建、官方 SHA256 校验、真实安装/-v、重复安装、已安装列表、HTTP 200/403、停止和卸载通过。第一次新地址请求发生网络失败，保留失败证据；仅重试一次后通过，不能抹去首次失败。

最终本地完整回归 core 738、集成 45、platform 15 项通过，55 项 ignored 未计入通过。桌面端 cargo check、改动行格式检查、前端类型检查和现有 21 项逻辑检查通过。内联验证实际 QueryClient/hooks 请求排序、失败保留缓存、安装完成回调及版本分组通过；YAML、CI Bash 语法、发布版本同步也已核对。未运行本地前端 dev/build，未新增测试文件。

没有数据库结构或用户数据库变更，未修改 `update.sql`。本机已安装程序仍是此前核实的 0.2.244；代码中的 ACME JOSE 修复和本机 HTTP 回归通过不等于旧安装程序已更新，也不等于公网签发验收通过。真实 UI/桌面 IPC、macOS 全套件升级矩阵、Windows UNC 和其余配置迁移继续待验，整体复查仍未完成。
