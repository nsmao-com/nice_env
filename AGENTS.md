# 项目发布约定

- 用户已授权后续提交、push 和发布流程直接执行，无需重复询问确认；仍须遵守本文件的版本同步、原子推送和远程验证要求。

- 用户要求提交、push 或发布新版本时，必须同时创建并推送新的版本 tag；仅推送分支不算完成。用户已明确要求将此作为本项目的默认流程。
- 沿用 `v<major>.<minor>.<patch>` 格式。普通修复递增补丁版本，先核对远程 tag，禁止覆盖或移动已发布的 tag。
- 创建 tag 前，同步根目录及工作区 `package.json`、根目录 `Cargo.toml`、桌面端 `Cargo.toml`、`tauri.conf.json` 和 `Cargo.lock` 中本项目各 crate 的版本号；不改动无关依赖版本。
- 同时核对设置页 `apps/web/src/app/settings/page.tsx`、应用菜单 `apps/web/src/components/layout/app-menu.tsx` 和浏览器预览 `apps/web/src/lib/mock.ts` 的应用版本兜底值，必须与新 tag 一致，避免安装包版本已更新但界面仍显示旧版。
- 将版本号变更提交后，在包含本次修复的发布提交上创建新的 annotated tag（带说明的标签），并推送分支和该 tag。这是必须执行的发布步骤，不需要用户再次提醒。
- 提交发布前运行 `python .github/check-rust-format.py HEAD` 检查待提交的 Rust 改动；提交后再运行默认命令 `python .github/check-rust-format.py`，与 CI 的 `HEAD^` 基准一致。只修正改动行涉及的格式，不格式化无关历史代码。Rust 修改还需通过 `cargo clippy --locked --workspace --all-targets`，不能仅凭类型检查或 Release 平台测试通过就忽略独立 CI 的失败。
- 发布前还需运行与 Release 对应的核心/平台回归：`cargo test --locked -j1 -p nsb-core -p platform --no-fail-fast -- --test-threads=1`。局部用例通过不能代替完整发布回归；磁盘或页面文件不足时应使用空间充足的临时 target 并降低编译并发，不能跳过失败检查。启用系统代理的开发机可仅对测试进程设置 `NO_PROXY=127.0.0.1,localhost,::1`，确保本地 HTTP fixture 不被代理转发，不修改系统代理配置。
- 分支与新 tag 必须使用 `git push --atomic` 一次推送；任一步失败都不能把仅提交分支汇报为发布完成。
- 推送后核对远程分支、tag 与发布提交一致，确认 `.github/workflows/release.yml` 已触发。汇报实际构建状态，不把“已触发”说成“安装包已发布”。
- 只提交本次任务相关文件，保留用户已有的其他修改和本地生成文件。
- 路径修复必须审查整个调用边界：结构体路径字段使用 `serialize_path` / `serialize_optional_path` / `serialize_paths`，直接返回字符串的路径接口与外部工具参数使用 `portable_path_text` / `portable_text`；不能只修前端显示。原生文件 API 保留长路径能力，UNC 共享不能丢失前导双斜杠；禁止对数据库单元格、SQL、密码或配置原文进行全局字符串替换。
