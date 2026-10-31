# 项目发布约定

- 用户已授权后续提交、push 和发布流程直接执行，无需重复询问确认；仍须遵守本文件的版本同步、原子推送和远程验证要求。

- 用户要求提交、push 或发布新版本时，必须同时创建并推送新的版本 tag；仅推送分支不算完成。用户已明确要求将此作为本项目的默认流程。
- 沿用 `v<major>.<minor>.<patch>` 格式。普通修复递增补丁版本，先核对远程 tag，禁止覆盖或移动已发布的 tag。
- 创建 tag 前，同步根目录及工作区 `package.json`，核对 Go 桌面/CLI 的开发版本兜底值。正式版由 `scripts/package-desktop.mjs` 从根 package.json 注入 Go、Windows 资源、NSIS 和 macOS plist 版本；不改动无关依赖版本。
- 同时核对设置页 `apps/web/src/app/settings/page.tsx`、应用菜单 `apps/web/src/components/layout/app-menu.tsx` 和浏览器预览 `apps/web/src/lib/mock.ts` 的应用版本兜底值，必须与新 tag 一致，避免安装包版本已更新但界面仍显示旧版。
- 将版本号变更提交后，在包含本次修复的发布提交上创建新的 annotated tag（带说明的标签），并推送分支和该 tag。这是必须执行的发布步骤，不需要用户再次提醒。
- 项目已迁移为 Go + Wails v3，Rust/Tauri 工程不再参与构建。发布前必须通过 `go build -mod=readonly ./...`、`go vet -mod=readonly ./...`、前端与 schema 类型检查，并检查改动 Go 文件的 gofmt；不格式化无关文件。
- 发布前须运行 `node scripts/wails-port-status.mjs --require-complete` 并实际验证本次改变的服务生命周期、数据库备份恢复、目录交接和安装卸载。命令注册齐全不能代替行为验证。Windows/macOS 各架构在原生 runner 编译打包，任一平台失败不得宣布完整发布；独立验收目录禁止使用用户现用数据库。前端 dev/build 仍遵循用户当前授权限制。
- 分支与新 tag 必须使用 `git push --atomic` 一次推送；任一步失败都不能把仅提交分支汇报为发布完成。
- 推送后核对远程分支、tag 与发布提交一致，确认 `.github/workflows/release.yml` 已触发。汇报实际构建状态，不把“已触发”说成“安装包已发布”。
- 只提交本次任务相关文件，保留用户已有的其他修改和本地生成文件。
- 路径修复必须审查整个调用边界：结构体路径字段使用 Go `Path` 类型，外部工具参数与打开路径边界使用 `DisplayPath`；不能只修前端显示。原生文件 API 保留长路径能力，UNC 共享不能丢失前导双斜杠；禁止对数据库单元格、SQL、密码或配置原文进行全局字符串替换。
