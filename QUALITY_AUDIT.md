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
