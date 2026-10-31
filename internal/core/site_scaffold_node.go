package core

import (
	"context"
	"crypto/sha512"
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"

	"niceenv/internal/process"
)

func nextScaffoldReady(root string) bool {
	if !scaffoldReady(root, "out/index.html", "package.json") {
		return false
	}
	info, err := os.Lstat(filepath.Join(root, "out", "_next"))
	if err != nil || !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
		return false
	}
	content, err := os.ReadFile(filepath.Join(root, "package.json"))
	if err != nil || len(content) > 512*1024 {
		return false
	}
	var data struct {
		Dependencies map[string]string `json:"dependencies"`
	}
	return json.Unmarshal(content, &data) == nil && data.Dependencies["next"] != ""
}

func (c *Core) scaffoldNextExport(ctx context.Context, root string) error {
	if nextScaffoldReady(root) {
		return nil
	}
	if err := emptyScaffoldDirectory(root); err != nil {
		return err
	}
	packages, err := c.installedRuntimePackages(ctx)
	if err != nil {
		return err
	}
	chosen, err := c.pathChosen(ctx, pathSelection{}, packages)
	if err != nil {
		return err
	}
	node, ok := chosen["node"]
	if !ok {
		return &AppError{Code: "NOT_INSTALLED", Message: "请先安装 Node.js"}
	}
	dir, err := runtimeDirectory(node)
	if err != nil {
		return err
	}
	entry, err := runtimeCLI(node)
	if err != nil {
		return err
	}
	program := filepath.Join(dir, filepath.Base(entry))
	if err = os.MkdirAll(filepath.Dir(root), 0755); err != nil {
		return err
	}
	stage, err := os.MkdirTemp(filepath.Dir(root), ".niceenv-next-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(stage)
	c.siteCreateProgress(root, "installing")
	metadata, err := upstreamGet(ctx, "https://registry.npmjs.org/pnpm/latest-10")
	if err != nil {
		return err
	}
	var pnpm npmMetadata
	if json.Unmarshal(metadata, &pnpm) != nil || pnpm.Name != "pnpm" || !strings.HasPrefix(pnpm.Version, "10.") || !safePart(pnpm.Version) || !strings.HasPrefix(pnpm.Dist.Integrity, "sha512-") {
		return invalid("pnpm 官方元数据无效")
	}
	expected, err := base64.StdEncoding.DecodeString(strings.TrimPrefix(pnpm.Dist.Integrity, "sha512-"))
	if err != nil || len(expected) != sha512.Size {
		return invalid("pnpm 完整性校验无效")
	}
	address := "https://registry.npmjs.org/pnpm/-/pnpm-" + pnpm.Version + ".tgz"
	bytes, err := upstreamGet(ctx, address)
	if err != nil {
		return err
	}
	digest := sha512.Sum512(bytes)
	if subtle.ConstantTimeCompare(digest[:], expected) != 1 {
		return invalid("pnpm 下载包完整性校验失败")
	}
	archive := filepath.Join(stage, "pnpm.tgz")
	if err = os.WriteFile(archive, bytes, 0600); err != nil {
		return err
	}
	tools := filepath.Join(stage, "tools")
	if err = os.MkdirAll(tools, 0700); err != nil {
		return err
	}
	if err = extractPackage(ctx, archive, tools, "targz", "", address); err != nil {
		return err
	}
	pnpmCLI := filepath.Join(tools, "package", "bin", "pnpm.cjs")
	if !scaffoldReady(tools, "package/bin/pnpm.cjs") {
		return invalid("pnpm 下载包缺少入口")
	}
	config := filepath.Join(tools, "config")
	if err = os.MkdirAll(config, 0700); err != nil {
		return err
	}
	npmrc := filepath.Join(config, "npmrc")
	if err = os.WriteFile(npmrc, nil, 0600); err != nil {
		return err
	}
	registry, _, err := c.Store.setting(ctx, "npmRegistry")
	if err != nil {
		return err
	}
	if registry == "" {
		registry = "https://registry.npmjs.org"
	}
	remove := []string{"NODE_OPTIONS", "NODE_PATH"}
	for _, item := range os.Environ() {
		key, _, _ := strings.Cut(item, "=")
		if strings.HasPrefix(strings.ToLower(key), "npm_config_") {
			remove = append(remove, key)
		}
	}
	command := func(cwd string, args ...string) process.Spec {
		return process.Spec{Program: program, Directory: cwd, Args: args, RemoveEnvironment: remove, Environment: map[string]string{
			"PATH": dir + string(os.PathListSeparator) + os.Getenv("PATH"), "CI": "1", "NEXT_TELEMETRY_DISABLED": "1",
			"npm_config_registry": registry, "npm_config_userconfig": npmrc, "npm_config_globalconfig": npmrc,
			"npm_config_store_dir": filepath.Join(c.Home, "downloads", "pnpm-store"), "npm_config_cache": filepath.Join(c.Home, "downloads", "pnpm-cache"), "npm_config_manage_package_manager_versions": "false",
			"XDG_CONFIG_HOME": config, "XDG_CACHE_HOME": filepath.Join(c.Home, "downloads", "node-cache"), "APPDATA": config, "LOCALAPPDATA": config,
		}}
	}
	if err = runScaffoldCommand(ctx, command(stage, pnpmCLI, "dlx", "create-next-app@latest", "project", "--use-pnpm", "--skip-install", "--disable-git", "--yes", "--ts", "--app", "--src-dir", "--no-tailwind", "--no-linter", "--no-react-compiler", "--no-cache-components", "--empty", "--import-alias", "@/*")); err != nil {
		return err
	}
	project := filepath.Join(stage, "project")
	manifestPath := filepath.Join(project, "package.json")
	content, err := os.ReadFile(manifestPath)
	if err != nil {
		return err
	}
	var manifest map[string]any
	if err = json.Unmarshal(content, &manifest); err != nil {
		return err
	}
	manifest["packageManager"] = "pnpm@" + pnpm.Version
	if scripts, ok := manifest["scripts"].(map[string]any); ok {
		delete(scripts, "start")
	}
	content, err = json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		return err
	}
	if err = os.WriteFile(manifestPath, content, 0600); err != nil {
		return err
	}
	for name, text := range map[string]string{"next.config.ts": scaffoldNextConfig, "src/app/layout.tsx": scaffoldNextLayout, "src/app/page.tsx": scaffoldNextPage, "README.md": scaffoldNextReadme} {
		if err = os.WriteFile(filepath.Join(project, filepath.FromSlash(name)), []byte(text), 0600); err != nil {
			return err
		}
	}
	for _, args := range [][]string{{pnpmCLI, "install", "--ignore-scripts", "--strict-peer-dependencies"}, {"node_modules/next/dist/bin/next", "typegen"}, {"node_modules/typescript/bin/tsc", "--noEmit"}, {"node_modules/next/dist/bin/next", "build"}} {
		phase := "validating"
		if args[0] == pnpmCLI {
			phase = "installing"
		} else if args[len(args)-1] == "build" {
			phase = "building"
		}
		c.siteCreateProgress(root, phase)
		if err = runScaffoldCommand(ctx, command(project, args...)); err != nil {
			return err
		}
	}
	if !nextScaffoldReady(project) {
		return invalid("Next.js 未生成完整静态导出文件")
	}
	return publishScaffold(project, root)
}
