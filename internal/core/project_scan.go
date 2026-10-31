package core

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"sort"
	"strings"
	"time"
)

type projectPHPCompatibility struct {
	Status           string   `json:"status"`
	Requirement      *string  `json:"requirement"`
	Versions         []string `json:"versions"`
	MatchingVersions []string `json:"matchingVersions"`
	Message          *string  `json:"message"`
}
type scannedProject struct {
	Path              Path                     `json:"path"`
	Name              string                   `json:"name"`
	Kind              string                   `json:"kind"`
	DocumentRoot      Path                     `json:"documentRoot"`
	DocumentRootReady bool                     `json:"documentRootReady"`
	SiteKind          string                   `json:"siteKind"`
	Rewrite           string                   `json:"rewrite"`
	PHPMinVersion     *string                  `json:"phpMinVersion"`
	PHPCompatibility  *projectPHPCompatibility `json:"phpCompatibility"`
	Evidence          []string                 `json:"evidence"`
	RunHint           string                   `json:"runHint"`
	NeedsDevServer    bool                     `json:"needsDevServer"`
	SuggestedDomain   string                   `json:"suggestedDomain"`
	AlreadyConfigured bool                     `json:"alreadyConfigured"`
}

func plainProjectDirectory(path string) bool {
	info, err := os.Lstat(path)
	return err == nil && info.IsDir() && info.Mode()&(os.ModeSymlink|os.ModeIrregular) == 0
}
func projectJSON(root *os.Root, name string, limit int64) (any, []byte, bool, error) {
	if _, err := relativePath(root.Name(), name); err != nil {
		return nil, nil, false, err
	}
	name = filepath.FromSlash(strings.ReplaceAll(name, `\`, "/"))
	info, err := siteCheck(root, name, true)
	if os.IsNotExist(err) {
		return nil, nil, false, nil
	}
	if err != nil {
		return nil, nil, false, err
	}
	if info == nil {
		return nil, nil, false, nil
	}
	if !info.Mode().IsRegular() || info.Size() > limit {
		return nil, nil, true, invalid(name + " 必须是大小受限的普通 JSON 文件")
	}
	f, err := root.Open(name)
	if err != nil {
		return nil, nil, true, err
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, limit+1))
	if err != nil {
		return nil, nil, true, err
	}
	if int64(len(data)) > limit {
		return nil, nil, true, invalid(name + " 超出大小限制")
	}
	var value any
	if json.Unmarshal(data, &value) != nil || value == nil {
		return nil, nil, true, invalid(name + " 必须是有效 JSON")
	}
	return value, data, true, nil
}
func projectMetadata(root *os.Root, name string, limit int64) (map[string]any, []byte, bool, error) {
	value, data, exists, err := projectJSON(root, name, limit)
	if err != nil || !exists {
		return nil, data, exists, err
	}
	object, ok := value.(map[string]any)
	if !ok {
		return nil, data, true, invalid(name + " 必须是 JSON 对象")
	}
	return object, data, true, nil
}
func projectPathKey(value string) string {
	value = filepath.Clean(DisplayPath(value))
	if runtime.GOOS == "windows" {
		value = strings.ToLower(value)
	}
	return value
}
func suggestedProjectDomain(name string) string {
	var b strings.Builder
	dash := false
	for _, ch := range strings.ToLower(name) {
		if ch >= 'a' && ch <= 'z' || ch >= '0' && ch <= '9' {
			b.WriteRune(ch)
			dash = false
		} else if b.Len() > 0 && !dash {
			b.WriteByte('-')
			dash = true
		}
		if b.Len() >= 40 {
			break
		}
	}
	name = strings.Trim(b.String(), "-")
	if name == "" {
		name = "site"
	}
	return name + ".test"
}
func detectProject(dir string, configured map[string]bool) (*scannedProject, error) {
	if !plainProjectDirectory(dir) {
		return nil, nil
	}
	root, err := os.OpenRoot(dir)
	if err != nil {
		return nil, err
	}
	defer root.Close()
	has := func(name string) bool {
		info, err := siteCheck(root, filepath.FromSlash(name), true)
		return err == nil && info != nil && (info.Mode().IsRegular() || name == "app" && info.IsDir())
	}
	composer, _, composerExists, composerErr := projectMetadata(root, "composer.json", 512*1024)
	pack, _, packageExists, packageErr := projectMetadata(root, "package.json", 512*1024)
	kind, evidence := "", []string{}
	choose := func(k, why string) { kind = k; evidence = append(evidence, why) }
	switch {
	case has("artisan"):
		choose("laravel", "存在 artisan")
	case has("wp-config.php") || has("wp-config-sample.php"):
		choose("word-press", "存在 WordPress 配置")
	case has("think") && has("app"):
		choose("think-php", "存在 think 与 app 目录")
	case has("bin/console") && has("symfony.lock"):
		choose("symfony", "存在 bin/console 与 symfony.lock")
	case has("spark"):
		choose("code-igniter", "存在 spark")
	case has("next.config.js") || has("next.config.ts") || has("next.config.mjs"):
		choose("next-js", "存在 next.config.*")
	case has("nuxt.config.ts") || has("nuxt.config.js"):
		choose("nuxt-js", "存在 nuxt.config.*")
	case has("vite.config.ts") || has("vite.config.js"):
		choose("vite", "存在 vite.config.*")
	case has("go.mod"):
		choose("go", "存在 go.mod")
	case has("pom.xml") || has("build.gradle") || has("build.gradle.kts"):
		choose("java", "存在 Maven / Gradle 配置")
	case has("pyproject.toml") || has("requirements.txt") || has("manage.py"):
		choose("python", "存在 Python 项目配置")
	}
	deps, _ := composer["require"].(map[string]any)
	if kind == "" && composerExists {
		for _, dep := range []struct{ name, kind string }{{"laravel/framework", "laravel"}, {"symfony/framework-bundle", "symfony"}, {"topthink/framework", "think-php"}, {"codeigniter4/framework", "code-igniter"}} {
			if _, ok := deps[dep.name]; ok {
				choose(dep.kind, "composer.json 依赖 "+dep.name)
				break
			}
		}
		if kind == "" {
			choose("generic-php", "存在 composer.json")
		}
	}
	if kind == "" && packageExists {
		for _, dep := range []struct{ name, kind string }{{"next", "next-js"}, {"nuxt", "nuxt-js"}, {"vite", "vite"}} {
			found := false
			for _, key := range []string{"dependencies", "devDependencies"} {
				list, _ := pack[key].(map[string]any)
				_, foundHere := list[dep.name]
				found = found || foundHere
			}
			if found {
				choose(dep.kind, "package.json 依赖 "+dep.name)
				break
			}
		}
		if kind == "" {
			choose("node-generic", "存在 package.json")
		}
	}
	var phpFiles, htmlFiles bool
	listing, err := root.Open(".")
	if err != nil {
		return nil, err
	}
	entries, readErr := listing.ReadDir(200)
	listing.Close()
	if readErr != nil && !errors.Is(readErr, io.EOF) {
		return nil, readErr
	}
	for _, entry := range entries {
		if !has(entry.Name()) {
			continue
		}
		ext := strings.ToLower(filepath.Ext(entry.Name()))
		phpFiles = phpFiles || ext == ".php"
		htmlFiles = htmlFiles || ext == ".html" || ext == ".htm"
	}
	if kind == "" {
		switch {
		case has("public/index.php"):
			choose("generic-php", "存在 public/index.php")
		case phpFiles:
			choose("generic-php", "存在 PHP 文件")
		case htmlFiles:
			choose("static-html", "存在 HTML 文件")
		default:
			return nil, nil
		}
	}
	p := &scannedProject{Path: Path(dir), Name: filepath.Base(dir), Kind: kind, Evidence: evidence, SiteKind: "static", Rewrite: "none", DocumentRoot: Path(dir), RunHint: "纯静态站点，无需运行时"}
	sub := ""
	switch kind {
	case "laravel", "symfony", "think-php", "code-igniter", "word-press", "generic-php":
		p.SiteKind = "php"
		p.RunHint = "需要 PHP；使用 Composer 的项目请先安装依赖"
		p.Rewrite = map[string]string{"laravel": "laravel", "symfony": "symfony", "think-php": "thinkphp", "code-igniter": "codeigniter", "word-press": "wordpress", "generic-php": "none"}[kind]
		if slices.Contains([]string{"laravel", "symfony", "think-php", "code-igniter"}, kind) || kind == "generic-php" && has("public/index.php") {
			sub = "public"
		}
		if req, ok := deps["php"].(string); ok {
			p.PHPMinVersion = &req
			p.Evidence = append(p.Evidence, "composer.json 要求 PHP "+req)
		}
	case "next-js", "nuxt-js", "vite", "node-generic":
		p.SiteKind = "node"
		p.Rewrite = "spa-fallback"
		p.NeedsDevServer = true
		p.RunHint = "需要 Node.js；可代理开发服务，或使用静态构建产物"
		sub = map[string]string{"next-js": "out", "nuxt-js": ".output/public", "vite": "dist"}[kind]
	case "go", "python", "java":
		p.SiteKind = kind
		p.NeedsDevServer = true
		p.RunHint = "选择已安装运行时并配置应用命令，通过 Web 服务反向代理访问"
	}
	ready := true
	if sub != "" {
		info, err := siteCheck(root, sub, true)
		ready = err == nil && info != nil && info.IsDir()
		if ready {
			p.DocumentRoot = Path(filepath.Join(dir, filepath.FromSlash(sub)))
		}
	}
	p.DocumentRootReady = ready && composerErr == nil && packageErr == nil
	if p.SiteKind == "php" {
		p.DocumentRootReady = p.DocumentRootReady && (sub != "" && has("public/index.php") || sub == "" && phpFiles)
	}
	if composerErr != nil || packageErr != nil {
		p.Evidence = append(p.Evidence, "依赖清单无法解析、安全读取或超过 512 KiB")
	}
	if !p.DocumentRootReady && !p.NeedsDevServer {
		p.Evidence = append(p.Evidence, "预期入口缺失或无法安全读取，请修复后重新识别")
	}
	p.SuggestedDomain = suggestedProjectDomain(p.Name)
	p.AlreadyConfigured = configured[projectPathKey(string(p.DocumentRoot))] || p.NeedsDevServer && configured[projectPathKey(dir)]
	return p, nil
}

func (c *Core) projectPHPCompatibility(ctx context.Context, path string) (projectPHPCompatibility, error) {
	r := projectPHPCompatibility{Status: "unspecified", Versions: []string{}, MatchingVersions: []string{}}
	if !plainProjectDirectory(path) {
		return r, invalid("项目目录不存在或无法安全读取")
	}
	root, err := os.OpenRoot(path)
	if err != nil {
		return r, err
	}
	defer root.Close()
	manifest, _, exists, err := projectMetadata(root, "composer.json", 512*1024)
	setError := func(err error) { r.Status = "invalid"; message := err.Error(); r.Message = &message }
	if err != nil {
		setError(err)
		return r, nil
	}
	if !exists {
		return r, nil
	}
	var deps map[string]any
	if raw, ok := manifest["require"]; ok {
		deps, ok = raw.(map[string]any)
		if !ok {
			setError(invalid("composer.json 的 require 必须为对象"))
			return r, nil
		}
	}
	raw, ok := deps["php"]
	if !ok {
		return r, nil
	}
	requirement, ok := raw.(string)
	if !ok || strings.TrimSpace(requirement) == "" || len(requirement) > 4096 {
		setError(invalid("require.php 必须是有效版本约束字符串（最多 4096 字节）"))
		return r, nil
	}
	r.Requirement = &requirement
	packages, err := c.installedRuntimePackages(ctx)
	if err != nil {
		return r, err
	}
	for _, p := range packages {
		if p.ID == "php" {
			r.Versions = append(r.Versions, p.Version)
		}
	}
	sort.Slice(r.Versions, func(i, j int) bool { return newerVersion(r.Versions[i], r.Versions[j]) })
	r.Versions = slices.Compact(r.Versions)
	r.MatchingVersions, err = c.composerMatchingPHP(ctx, requirement, r.Versions)
	if err != nil {
		r.MatchingVersions = []string{}
		r.Status = "unavailable"
		message := err.Error()
		r.Message = &message
		var appErr *AppError
		if errors.As(err, &appErr) && appErr.Code == "PHP_REQUIREMENT_INVALID" {
			r.Status = "invalid"
		}
	} else {
		r.Status = "checked"
	}
	return r, nil
}
func (c *Core) registerProjectChecks() {
	c.handle("project_php_compatibility", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Project string `json:"project"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.projectChecks.TryLock() {
			return nil, serviceBusy()
		}
		defer c.projectChecks.Unlock()
		return c.projectPHPCompatibility(ctx, DisplayPath(in.Project))
	})
	c.handle("project_platform_check", c.projectPlatformCheck)
	c.handle("scan_projects", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Root string `json:"root"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.projectChecks.TryLock() {
			return nil, serviceBusy()
		}
		defer c.projectChecks.Unlock()
		path, err := filepath.Abs(DisplayPath(in.Root))
		if err != nil {
			return nil, err
		}
		if in.Root == "" || !plainProjectDirectory(path) {
			return nil, invalid("请选择可安全读取的项目目录")
		}
		sites, err := c.sites(ctx)
		if err != nil {
			return nil, err
		}
		configured := map[string]bool{}
		for _, s := range sites {
			configured[projectPathKey(string(s.RootDir))] = true
		}
		out := []scannedProject{}
		p, err := detectProject(path, configured)
		if err != nil {
			return nil, err
		}
		if p != nil {
			out = append(out, *p)
		}
		entries, err := os.ReadDir(path)
		if err != nil {
			return nil, err
		}
		if len(entries) > 10000 {
			return nil, invalid("目录条目过多，请选择更具体的项目目录")
		}
		for _, entry := range entries {
			if err = ctx.Err(); err != nil {
				return nil, err
			}
			name := strings.ToLower(entry.Name())
			if strings.HasPrefix(name, ".") || slices.Contains([]string{"node_modules", "vendor", "target", "dist", "build", "__pycache__", "windows", "program files", "program files (x86)", "appdata", "$recycle.bin"}, name) {
				continue
			}
			child := filepath.Join(path, entry.Name())
			if p != nil && string(p.DocumentRoot) == child {
				continue
			}
			found, err := detectProject(child, configured)
			if err != nil {
				return nil, err
			}
			if found != nil {
				out = append(out, *found)
			}
		}
		sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
		started := time.Now()
		cache := map[string]projectPHPCompatibility{}
		for i := range out {
			project := &out[i]
			if project.SiteKind != "php" {
				continue
			}
			if project.PHPMinVersion != nil {
				if report, ok := cache[*project.PHPMinVersion]; ok {
					project.PHPCompatibility = &report
					continue
				}
			}
			var report projectPHPCompatibility
			if time.Since(started) > 5*time.Second && project.PHPMinVersion != nil {
				message := "批量检查时间已达上限，请在项目下重新检查"
				report = projectPHPCompatibility{Status: "unavailable", Requirement: project.PHPMinVersion, Versions: []string{}, MatchingVersions: []string{}, Message: &message}
			} else {
				report, err = c.projectPHPCompatibility(ctx, string(project.Path))
				if err != nil {
					return nil, err
				}
			}
			project.PHPCompatibility = &report
			if project.PHPMinVersion != nil && report.Requirement != nil {
				cache[*project.PHPMinVersion] = report
			}
		}
		return out, nil
	})
}

func (c *Core) validateExistingProject(ctx context.Context, site Site, project string, allow bool) error {
	if !filepath.IsAbs(project) || !filepath.IsAbs(string(site.RootDir)) || !plainProjectDirectory(string(site.RootDir)) {
		return invalid("建站目录与项目识别结果不一致，请重新扫描")
	}
	detected, err := detectProject(project, nil)
	if err != nil {
		return err
	}
	if detected == nil || detected.SiteKind != site.Runtime.Kind || !detected.NeedsDevServer && !detected.DocumentRootReady {
		return &AppError{Code: "PROJECT_CHANGED", Message: "项目类型或入口已变化，请补齐入口后重新识别"}
	}
	expected := string(detected.DocumentRoot)
	if detected.NeedsDevServer {
		expected = project
	}
	actual, err := filepath.EvalSymlinks(string(site.RootDir))
	if err != nil {
		return err
	}
	expected, err = filepath.EvalSymlinks(expected)
	if err != nil {
		return err
	}
	if projectPathKey(actual) != projectPathKey(expected) {
		return invalid("建站目录与项目识别结果不一致，请重新扫描")
	}
	if !c.projectChecks.TryLock() {
		return serviceBusy()
	}
	defer c.projectChecks.Unlock()
	return c.validateExistingPHP(ctx, site, project, allow)
}
