# 套件版本核对记录（2026-09-30）

使用项目现有 `check_versions --all --json --force --manifest ...` 检查 Windows 的 53 个套件及 macOS 的 9 个套件；Windows 的 52 个动态版本目录和 macOS 的 9 个动态版本目录均在线，phpMyAdmin 是固定清单条目且上游没有版本 API，另以官方下载页核对为 5.2.3。下表是核对时相应平台/系列可安装的最高正式版，不代表每个产品所有平台的最高源码标签。Redis、Memcached 更换来源后再次强制刷新验证。

## 本次处理

- Redis：旧 `tporadowski/redis` 只能枚举到 5.0.14.1，切换到持续维护的 `redis-windows/redis-windows` MSYS2 便携包，内置 8.10.2。保留旧版 5.0.14.1 / 5.0.10，移除错误标为 5.0.14 的重复下载条目；历史安装入口继续兼容。
- Memcached：旧 `jefyt/memcached-windows` 停在 1.6.8，改用 `nono303/memcached` 的版本 tag。下载固定 commit 的完整包并保留 Cygwin DLL，使用通用 SSE2 入口，内置 1.6.45。
- 同步 Adminer 6.1.1、etcd 3.7.2、Mailpit 1.31.3、Meilisearch 1.54.2、MongoDB 9.0.2、MongoDB Database Tools 100.19.1、Ollama 0.35.0、Temurin JDK 21.0.12.1+1 的内置清单，保留原版本供选择；MongoDB 同步 Windows/macOS。
- Redis 适配嵌套程序目录、包含空格的 Windows/POSIX 参数路径、系统已核验的原生 PID 与 POSIX PID 差异、备份目录的路径转换。不会自动切换已安装版本或修改现有服务数据。

## 需要区分的版本含义

- **MinIO**：上游仓库已归档；最后提供 Windows 预编译程序的版本为 `RELEASE.2025-09-07T16-13-09Z`，更新的源码标签不能直接作为 Windows 可安装版本。来源：[上游发行页](https://github.com/minio/minio/releases)。
- **Rust**：这里的 1.29.1 是 rustup 安装器版本，不是 rustc 编译器版本。
- **JDK 21 / .NET 8**：套件明确锁定系列，只跟进同系列更新，不把 JDK 25 或 .NET 10 冒充此套件的新版本。
- **ZincSearch**：正式版为 0.4.10；清单还提供 1.0.0-beta 系列，界面应继续按预发布版本分类。
- **MySQL**：26.7.0 来自官方发行目录，不能依据旧的版本命名习惯删掉。
- 版本按钮优先显示用户已安装/默认版本，不能把它当作全部可安装版本；展开列表可查看新版本。Redis/Memcached 源变化自动使旧目录缓存失效。

## Windows 全量核对

| 套件 ID | 可安装最高正式版 | 本次处理 |
| --- | --- | --- |
| adminer | 6.1.1 | 更新内置版本 |
| apache | 2.4.68 | 目录正常，保留现有版本 |
| bun | 1.4.2 | 目录正常，保留现有版本 |
| caddy | 2.11.4 | 目录正常，保留现有版本 |
| cloudflared | 2026.9.3 | 目录正常，保留现有版本 |
| composer | 2.10.3 | 目录正常，保留现有版本 |
| consul | 2.0.4 | 目录正常，保留现有版本 |
| coredns | 1.14.7 | 目录正常，保留现有版本 |
| deno | 2.9.7 | 目录正常，保留现有版本 |
| dotnet-sdk8 | 8.0.425 | 目录正常，保留现有版本 |
| elasticsearch | 9.5.4 | 目录正常，保留现有版本 |
| erlang | 29.1.1 | 目录正常，保留现有版本 |
| etcd | 3.7.2 | 更新内置版本 |
| flutter | 3.47.5 | 目录正常，保留现有版本 |
| frankenphp | 1.12.7 | 目录正常，保留现有版本 |
| go | 1.27.1 | 目录正常，保留现有版本 |
| gradle | 9.8.0 | 目录正常，保留现有版本 |
| k6 | 2.3.0 | 目录正常，保留现有版本 |
| mailpit | 1.31.3 | 更新内置版本 |
| mariadb | 13.0.2 | 目录正常，保留现有版本 |
| meilisearch | 1.54.2 | 更新内置版本 |
| memcached | 1.6.45 | 更新来源及内置版本 |
| mihomo | 1.19.31 | 目录正常，保留现有版本 |
| minio | RELEASE.2025-09-07T16-13-09Z | 目录正常，保留现有版本 |
| mongodb | 9.0.2 | 更新内置版本 |
| mongodb-database-tools | 100.19.1 | 更新内置版本 |
| mongosh | 2.12.0 | 目录正常，保留现有版本 |
| mysql | 26.7.0 | 目录正常，保留现有版本 |
| nats | 2.15.0 | 目录正常，保留现有版本 |
| neo4j | 2026.09.0 | 目录正常，保留现有版本 |
| nginx | 1.31.6 | 目录正常，保留现有版本 |
| node | 26.10.0 | 目录正常，保留现有版本 |
| ollama | 0.35.0 | 更新内置版本 |
| php | 8.5.11 | 目录正常，保留现有版本 |
| phpmyadmin | 5.2.3 | 目录正常，保留现有版本 |
| postgresql | 18.6 | 目录正常，保留现有版本 |
| python | 3.14.7 | 目录正常，保留现有版本 |
| qdrant | 1.19.1 | 目录正常，保留现有版本 |
| rabbitmq | 4.3.6 | 目录正常，保留现有版本 |
| redis | 8.10.2 | 更新来源及内置版本 |
| rnacos | 0.8.7 | 目录正常，保留现有版本 |
| roadrunner | 2025.1.15 | 目录正常，保留现有版本 |
| ruby | 4.0.7-1 | 目录正常，保留现有版本 |
| ruby-devkit | 4.0.7-1 | 目录正常，保留现有版本 |
| rust | 1.29.1 | 目录正常，保留现有版本 |
| rustfs | 1.0.0 | 目录正常，保留现有版本 |
| sftpgo | 2.7.6 | 目录正常，保留现有版本 |
| strawberry-perl | 5.42.3.1 | 目录正常，保留现有版本 |
| temporal-cli | 1.9.1 | 目录正常，保留现有版本 |
| temurin-jdk21 | 21.0.12.1+1 | 更新内置版本 |
| tomcat | 11.0.26 | 目录正常，保留现有版本 |
| zig | 0.16.0 | 目录正常，保留现有版本 |
| zincsearch | 0.4.10 | 目录正常，保留现有版本 |

## macOS 清单核对

当前核对的是清单现有平台条目；未在 Windows 上执行 macOS 二进制。

| 套件 ID | 可安装最高正式版 |
| --- | --- |
| composer | 2.10.3 |
| go | 1.27.1 |
| mihomo | 1.19.31 |
| mongodb | 9.0.2 |
| mongodb-database-tools | 100.19.1 |
| mongosh | 2.12.0 |
| mysql | 26.7.0 |
| nats | 2.15.0 |
| node | 26.10.0 |

## 验证边界

目录核对不等同于逐个启动全部服务。Redis/Memcached 使用下载后的完整程序包在临时目录、本机临时端口验证，不操作用户已安装实例。前端仅执行类型检查，未运行 dev/build。

已完成的验证：

- Windows 52 个动态目录、macOS 9 个动态目录、最新安装包地址探测及缓存均通过；phpMyAdmin 的固定版本通过官方下载页复核（其静态条目不会从 `check_versions` 动态枚举）。
- Redis 8.10.2 与旧版 5.0.14.1：分别运行现有隔离集成用例，覆盖启停、端口回落、配置保留、认证、快照、备份/导入/恢复、密码设置及失败保护，均通过。8.x 夹具单独启用保护配置修改以注入 dbfilename 故障，此设置不进入产品默认配置。
- Memcached 1.6.45：完整下载并核对 SHA256，原生程序 `VERSION`、`SET`、`GET` 验证通过。
- 版本处理、Redis 协议/认证、安装器现有单元用例：41 项通过；未新增测试文件。
- `cargo check --workspace --all-targets`、`pnpm check` 与修改文件的 `git diff --check` 通过。
- macOS 9 个套件目录全部成功，9 个最新下载地址均已确认可达；Composer 首次探测网络超时，单独重试通过。

## 第二轮：版本选择与实际运行（2026-09-29）

继续核对发现并修复：

1. 同一主版本的构建号以前使用字符串排序，`21.0.9+9` 会压过 `21.0.9+10`；前后端现统一自然数字排序。
2. `rc.10` 的序号以前混进主版本比较，可能排到同号正式版前面；现分开比较主版本、正式/预发布、后缀数字。`v1.2.3` 与 `1.2.3` 视为同一版本，避免错误更新提示；`+dev.build` 属于构建信息，不作为预发布。
3. 后端不指定版本安装 ZincSearch 时可能选择 1.0.0-beta3，而界面显示默认正式版 0.4.10；现后台默认安装和合成模板都优先正式版，显式选择 beta 仍可用。只有预发布的套件仍可正常选择最新预发布。旧排序缓存更新为 v4 后重新获取。
4. 真实 RustFS 1.0.0 Windows 包已核对 SHA256，复现上游按空白拆分绝对数据路径，导致含空格目录启动失败。改成以原数据目录为 cwd、以 `.` 作为 volume；真实程序在相同含空格目录可返回 HTTP 200。只升级历史原始默认运行描述，保留自定义配置和原数据位置。

另外对照官方资料确认：Neo4j 当前系列仍支持 Java 21；RabbitMQ 4.3.6 已支持 Erlang 29，不能据旧文档将最新 Erlang 一律判为不兼容。此结论仅适用于对应版本，不代表旧 RabbitMQ 版本也兼容 Erlang 29。

参考：[Neo4j 系统要求](https://neo4j.com/docs/operations-manual/current/installation/requirements/)、[RabbitMQ Erlang 兼容表](https://www.rabbitmq.com/docs/which-erlang)、[RustFS 1.0.0](https://github.com/rustfs/rustfs/releases/tag/1.0.0)。

第二轮验证结果：

- 前端直接执行版本比较逻辑，8 项排序/预发布/前缀检查通过；`pnpm check` 通过。
- Rust 版本处理 5 项、安装器 30 项测试通过；另 3 项既有网络/原生安装用例按其默认 ignore 规则跳过，不计入通过项。
- 新增于现有 `generic.rs` 测试模块的 RustFS 隔离回归通过：真实 Windows 1.0.0 程序、含空格目录、历史默认安装快照、保留自定义 cwd、连续两次启动/停止及 HTTP 200；原数据目录正确生成 `.rustfs.sys`，停止后托管进程退出。未新建测试文件。
- MSVC 初次链接遇到 PDB/磁盘空间限制；仅清理本轮失败生成的目标文件，使用临时 rustc 参数禁用本次测试的调试符号、将增量缓存放到系统临时目录后完成测试，未修改项目构建配置。
- 第二轮 `cargo check --workspace --all-targets` 与修改范围的 `git diff --check` 均通过；未运行前端 dev/build，未操作用户真实服务或数据库。

## 第三轮：逐项上游复核（2026-09-30）

本轮使用项目现有 `check_versions` 对 Windows 53 个套件、macOS 9 个套件执行强制刷新；52 个 Windows 动态目录与 9 个 macOS 动态目录在线，并对新增或变更的官方下载地址做了可达性与哈希核对。phpMyAdmin 没有动态版本源，官方下载页仍显示 5.2.3。发现以下三个套件需要补齐最新正式版：

1. Meilisearch 从 `1.54.1` 更新至 `1.54.2`（Windows）。
2. MongoDB Database Tools 从 `100.19.0` 更新至 `100.19.1`（Windows、macOS arm64、macOS x86_64）。三份新包的 SHA256/大小分别为 `527738a0f9ab2d80ea40cb8fc7a68f8e28664a2fd4e3b63626c2d4629aeb7f37` / 43,007,158 字节、`54643d4aedb79ddc81797d6fb54113acf79f77fd82a5868b6674f6c74e1b3000` / 76,422,273 字节、`1633f62c42d8cbff79b3f0b2475080ce9e1a536170fce632b64f83ae05520a4a` / 84,167,782 字节。
3. Ollama 从 `0.34.4` 更新至 `0.35.0`（Windows）；官方目录已返回安装包地址与 SHA256，未下载 1.46 GB 安装包，仅做地址探测以避免无谓传输。

同时纠正 Adminer 与 MongoDB 的显示名称，使其与实际版本一致。MySQL `26.7.0` 本轮在 Windows/macOS 官方发行目录中仍是可下载的最高正式版，因此保留。其余套件的清单最高版本与本轮上游目录一致，没有为了版本号变化而改动历史可选版本。

第三轮验证边界：完成两平台全量目录刷新、变更包地址探测、MongoDB Database Tools 三架构实际下载哈希校验、JSON 解析及 `git diff --check`；未运行前端 dev/build，未启动用户服务，未修改数据库。
