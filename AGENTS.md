# 项目发布约定

- 用户要求提交、push 或发布新版本时，必须同时创建并推送新的版本 tag；仅推送分支不算完成。用户已明确要求将此作为本项目的默认流程。
- 沿用 `v<major>.<minor>.<patch>` 格式。普通修复递增补丁版本，先核对远程 tag，禁止覆盖或移动已发布的 tag。
- 创建 tag 前，同步根目录及工作区 `package.json`、根目录 `Cargo.toml`、桌面端 `Cargo.toml`、`tauri.conf.json` 和 `Cargo.lock` 中本项目各 crate 的版本号；不改动无关依赖版本。
- 同时核对设置页 `apps/web/src/app/settings/page.tsx`、应用菜单 `apps/web/src/components/layout/app-menu.tsx` 和浏览器预览 `apps/web/src/lib/mock.ts` 的应用版本兜底值，必须与新 tag 一致，避免安装包版本已更新但界面仍显示旧版。
- 将版本号变更提交后，在包含本次修复的发布提交上创建新的 annotated tag（带说明的标签），并推送分支和该 tag。这是必须执行的发布步骤，不需要用户再次提醒。
- 分支与新 tag 必须使用 `git push --atomic` 一次推送；任一步失败都不能把仅提交分支汇报为发布完成。
- 推送后核对远程分支、tag 与发布提交一致，确认 `.github/workflows/release.yml` 已触发。汇报实际构建状态，不把“已触发”说成“安装包已发布”。
- 只提交本次任务相关文件，保留用户已有的其他修改和本地生成文件。
