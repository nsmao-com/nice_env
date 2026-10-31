package core

import (
	"context"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"niceenv/internal/process"
)

func composerTemplate(template string) string {
	return map[string]string{"laravel": "laravel/laravel", "thinkphp": "topthink/think", "symfony": "symfony/skeleton", "codeigniter": "codeigniter4/appstarter"}[template]
}
func (c *Core) siteCreateProgress(root, stage string) {
	if c.emit != nil {
		c.emit("site://create-progress", map[string]any{"rootDir": Path(root), "stage": stage, "percent": nil})
	}
}
func validateSiteTemplate(site Site, template string, db *siteDB, example, existing bool) error {
	switch template {
	case "", "none", "blank-php", "static", "spa", "next-export", "wordpress", "laravel", "thinkphp", "symfony", "codeigniter":
	default:
		return invalid("不支持此项目模板")
	}
	if existing && (template != "" && template != "none" || example || site.PHPOverrides != nil) {
		return invalid("使用已有项目时不能生成模板或改写项目配置")
	}
	if site.DB != nil && site.DB.Enabled {
		return invalid("请通过创建数据库选项绑定站点，不能直接写入数据库凭据")
	}
	if (composerTemplate(template) != "" || template == "wordpress" || template == "blank-php") && site.Runtime.Kind != "php" {
		return invalid("所选模板需要 PHP 运行环境")
	}
	if (template == "static" || template == "spa" || template == "next-export") && site.Runtime.Kind != "static" {
		return invalid("所选模板需要静态站点类型")
	}
	if template == "next-export" && db != nil {
		return invalid("Next.js 静态导出不使用数据库绑定")
	}
	if site.Runtime.Kind == "redirect" && (template != "" && template != "none" || db != nil || example || existing) {
		return invalid("跳转站点不使用项目模板或数据库绑定")
	}
	if example && db == nil {
		return invalid("生成数据库环境示例需要先填写数据库配置")
	}
	return validateSiteDatabase(db)
}

func scaffoldSecret(size int) (string, error) {
	value := make([]byte, size)
	if _, err := rand.Read(value); err != nil {
		return "", err
	}
	return base64.StdEncoding.EncodeToString(value), nil
}
func scaffoldWrite(root, name, content string) error {
	base, err := os.OpenRoot(root)
	if err != nil {
		return err
	}
	defer base.Close()
	_, exists, err := envRead(base, name)
	if err != nil || exists {
		return err
	}
	return replaceEnv(base, name, nil, false, []byte(content))
}
func emptyScaffoldDirectory(path string) error {
	info, err := os.Lstat(path)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
		return invalid("项目目标必须是普通目录")
	}
	entries, err := os.ReadDir(path)
	if err != nil {
		return err
	}
	if len(entries) > 0 {
		return &AppError{Code: "PROJECT_NOT_EMPTY", Message: "目标目录已有文件，未覆盖项目", Hint: "请选择空目录，或使用识别已有项目功能。"}
	}
	return nil
}
func publishScaffold(project, target string) error {
	if err := emptyScaffoldDirectory(target); err != nil {
		return err
	}
	if err := os.Remove(target); err != nil && !os.IsNotExist(err) {
		return err
	}
	return os.Rename(project, target)
}
func scaffoldReady(path string, names ...string) bool {
	root, err := os.OpenRoot(path)
	if err != nil {
		return false
	}
	defer root.Close()
	for _, name := range names {
		if !rootFile(root, name) {
			return false
		}
	}
	return true
}
func composerScaffoldReady(path, template string) bool {
	if !scaffoldReady(path, "public/index.php", "vendor/autoload.php", "composer.json") {
		return false
	}
	data, err := os.ReadFile(filepath.Join(path, "composer.json"))
	if err != nil || len(data) > 512*1024 {
		return false
	}
	var manifest struct {
		Require map[string]string `json:"require"`
	}
	if json.Unmarshal(data, &manifest) != nil {
		return false
	}
	dependency := map[string]string{"laravel": "laravel/framework", "thinkphp": "topthink/framework", "symfony": "symfony/framework-bundle", "codeigniter": "codeigniter4/framework"}[template]
	return manifest.Require[dependency] != ""
}
func runScaffoldCommand(ctx context.Context, spec process.Spec) error {
	if spec.Environment == nil {
		spec.Environment = map[string]string{}
	}
	if err := scaffoldNetworkEnvironment(spec.Environment); err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(ctx, 10*time.Minute)
	defer cancel()
	out := process.NewLimitedBuffer(64 * 1024)
	spec.Stdout, spec.Stderr = out, out
	child, err := process.Start(ctx, spec)
	if err != nil {
		return err
	}
	result := child.Wait()
	if ctx.Err() != nil {
		return ctx.Err()
	}
	if result.Code != 0 || result.Err != nil {
		return &AppError{Code: "PROJECT_COMMAND_FAILED", Message: "项目安装或初始化失败，原项目目录未覆盖", Detail: out.String()}
	}
	return nil
}

// Composer and pnpm do not read Windows' manual proxy settings. Supply those
// settings only to the short-lived installer process, retaining explicit env.
func scaffoldNetworkEnvironment(env map[string]string) error {
	server, bypass := systemHTTPProxy()
	if server == "" {
		return nil
	}
	for _, scheme := range []string{"http", "https"} {
		key := strings.ToUpper(scheme) + "_PROXY"
		if os.Getenv(key) != "" || os.Getenv(strings.ToLower(key)) != "" {
			continue
		}
		address := server
		if strings.Contains(server, "=") {
			address = ""
			for _, part := range strings.Split(server, ";") {
				protocol, value, ok := strings.Cut(strings.TrimSpace(part), "=")
				if ok && strings.EqualFold(protocol, scheme) {
					address = strings.TrimSpace(value)
					break
				}
			}
		}
		if address == "" {
			continue
		}
		if !strings.Contains(address, "://") {
			address = "http://" + address
		}
		u, err := url.Parse(address)
		if err != nil || u.Hostname() == "" || u.Scheme != "http" && u.Scheme != "https" {
			return invalid("项目安装需要有效的 HTTP/HTTPS 代理地址")
		}
		env[key] = address
	}
	noProxy := os.Getenv("NO_PROXY")
	if noProxy == "" {
		noProxy = os.Getenv("no_proxy")
	}
	values := []string{noProxy, "localhost", "127.0.0.1", "::1"}
	for _, rule := range strings.Split(bypass, ";") {
		rule = strings.TrimSpace(rule)
		if strings.HasPrefix(rule, "*.") {
			rule = rule[1:]
		}
		if rule != "" && !strings.ContainsAny(rule, "<>*?") {
			values = append(values, rule)
		}
	}
	env["NO_PROXY"] = strings.Join(values, ",")
	return nil
}

func (c *Core) scaffoldSite(ctx context.Context, site *Site, template string) error {
	if site.Runtime.Kind == "redirect" {
		return nil
	}
	root := string(site.RootDir)
	if composerTemplate(template) != "" {
		if err := c.scaffoldComposer(ctx, root, *site, template); err != nil {
			return err
		}
		site.RootDir = Path(filepath.Join(root, "public"))
		return nil
	}
	if template == "next-export" {
		if err := c.scaffoldNextExport(ctx, root); err != nil {
			return err
		}
		site.RootDir = Path(filepath.Join(root, "out"))
		return nil
	}
	if template == "wordpress" {
		return c.scaffoldWordpress(ctx, root)
	}
	if err := os.MkdirAll(root, 0755); err != nil {
		return err
	}
	switch template {
	case "blank-php":
		if err := scaffoldWrite(root, "index.php", "<?php\nheader('Content-Type: text/html; charset=utf-8');\necho '<h1>It works!</h1><p>NiceEnv PHP site.</p>';\necho '<p>PHP ' . PHP_VERSION . '</p>';\n"); err != nil {
			return err
		}
		return scaffoldWrite(root, "phpinfo.php", "<?php\nphpinfo();\n")
	case "static":
		return scaffoldWrite(root, "index.html", scaffoldStaticHTML)
	case "spa":
		return scaffoldWrite(root, "index.html", scaffoldSPAHTML)
	}
	return nil
}

func (c *Core) scaffoldWordpress(ctx context.Context, root string) error {
	ready := func(path string) bool {
		return scaffoldReady(path, "index.php", "wp-load.php", "wp-settings.php", "wp-includes/version.php", "wp-admin/install.php", "wp-config-sample.php")
	}
	if ready(root) {
		return nil
	}
	if err := emptyScaffoldDirectory(root); err != nil {
		return err
	}
	parent := filepath.Dir(root)
	if err := os.MkdirAll(parent, 0755); err != nil {
		return err
	}
	stage, err := os.MkdirTemp(parent, ".niceenv-wordpress-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(stage)
	archive := filepath.Join(stage, "wordpress.zip")
	c.siteCreateProgress(root, "downloading")
	if err = c.downloadPackage(ctx, "wordpress", "", "https://wordpress.org/latest.zip", "", 0, archive); err != nil {
		return err
	}
	c.siteCreateProgress(root, "extracting")
	if err = extractPackage(ctx, archive, stage, "archive", "", "https://wordpress.org/latest.zip"); err != nil {
		return err
	}
	project := filepath.Join(stage, "wordpress")
	if !ready(project) {
		return invalid("WordPress 下载包不完整，未发布项目")
	}
	return publishScaffold(project, root)
}

func (c *Core) scaffoldComposer(ctx context.Context, root string, site Site, template string) error {
	if !c.services.lifecycle.TryLock() {
		return serviceBusy()
	}
	php, composer, err := c.projectPHPBinaries(ctx, *site.Runtime.PHPVersion)
	ini := ""
	if err == nil {
		var entry *serviceEntry
		entry, err = c.serviceEntry("php@"+*site.Runtime.PHPVersion, "")
		if err == nil {
			var resolved *resolvedService
			resolved, err = c.resolveService(ctx, entry)
			if err == nil {
				err = c.ensurePHPIni(ctx, entry, resolved)
				ini = filepath.Join(resolved.etc, "php.ini")
			}
		}
	}
	c.services.lifecycle.Unlock()
	if err != nil {
		return err
	}
	if err = os.MkdirAll(filepath.Dir(root), 0755); err != nil {
		return err
	}
	stage, err := os.MkdirTemp(filepath.Dir(root), ".niceenv-composer-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(stage)
	command := func(dir string, args ...string) process.Spec {
		spec := phpCheckSpec(php, dir, append([]string{"-c", DisplayPath(ini)}, args...))
		delete(spec.Environment, "COMPOSER_DISABLE_NETWORK")
		spec.RemoveEnvironment = append(spec.RemoveEnvironment, "COMPOSER_DISABLE_NETWORK")
		spec.Environment["COMPOSER_HOME"] = filepath.Join(stage, "home")
		spec.Environment["COMPOSER_CACHE_DIR"] = filepath.Join(c.Home, "downloads", "composer-cache")
		spec.Environment["PATH"] = filepath.Dir(php) + string(os.PathListSeparator) + os.Getenv("PATH")
		return spec
	}
	check := func(dir string) error {
		return runScaffoldCommand(ctx, command(dir, composer, "check-platform-reqs", "--no-plugins", "--no-scripts", "--no-interaction"))
	}
	if composerScaffoldReady(root, template) {
		return check(root)
	}
	if err = emptyScaffoldDirectory(root); err != nil {
		return err
	}
	project := filepath.Join(stage, "project")
	c.siteCreateProgress(root, "installing")
	if err = runScaffoldCommand(ctx, command(stage, composer, "create-project", "--prefer-dist", "--no-scripts", "--remove-vcs", "--no-interaction", composerTemplate(template), DisplayPath(project))); err != nil {
		return err
	}
	c.siteCreateProgress(root, "initializing")
	if err = c.initializeScaffoldPHP(ctx, project, template, site); err != nil {
		return err
	}
	commands := [][]string{}
	if template == "laravel" {
		commands = [][]string{{"artisan", "package:discover", "--no-interaction"}}
	}
	if template == "thinkphp" {
		commands = [][]string{{"think", "service:discover", "--no-interaction"}, {"think", "vendor:publish", "--no-interaction"}}
	}
	for _, args := range commands {
		if err = runScaffoldCommand(ctx, command(project, args...)); err != nil {
			return err
		}
	}
	c.siteCreateProgress(root, "validating")
	if err = check(project); err != nil {
		return err
	}
	if !composerScaffoldReady(project, template) {
		return invalid("框架安装未生成完整入口、依赖或自动加载文件")
	}
	return publishScaffold(project, root)
}

func (c *Core) initializeScaffoldPHP(ctx context.Context, root, template string, site Site) error {
	sample, syntax := ".env.example", "dotenv"
	switch template {
	case "thinkphp":
		sample, syntax = ".example.env", "thinkphp"
	case "symfony":
		sample = ".env"
	case "codeigniter":
		sample, syntax = "env", "codeigniter"
	}
	base, err := os.OpenRoot(root)
	if err != nil {
		return err
	}
	defer base.Close()
	before, exists, err := envRead(base, ".env")
	if err != nil {
		return err
	}
	content := before
	if !exists {
		content, _, err = envRead(base, sample)
		if err != nil {
			return err
		}
	}
	secret, err := scaffoldSecret(32)
	if err != nil {
		return err
	}
	domain := strings.Replace(site.Domains[0], "*.", "www.", 1)
	for _, name := range site.Domains {
		if !strings.HasPrefix(name, "*.") {
			domain = name
			break
		}
	}
	endpoint := site
	endpoint.HTTPS = false
	options, err := c.siteOptions(ctx, endpoint, "", false)
	if err != nil {
		return err
	}
	url := "http://" + domain
	if site.HTTPS {
		url = "https://" + domain
		if options.https != 443 {
			url += ":" + strconv.Itoa(options.https)
		}
	} else if options.http != 80 {
		url += ":" + strconv.Itoa(options.http)
	}
	changes := [][2]string{}
	switch template {
	case "laravel":
		changes = [][2]string{{"APP_KEY", "base64:" + secret}, {"APP_URL", url}, {"SESSION_DRIVER", "file"}, {"CACHE_STORE", "file"}, {"CACHE_DRIVER", "file"}, {"QUEUE_CONNECTION", "sync"}}
	case "symfony":
		changes = [][2]string{{"APP_SECRET", secret}}
	case "codeigniter":
		changes = [][2]string{{"CI_ENVIRONMENT", "development"}, {"app.baseURL", url + "/"}}
	}
	updated, err := applyEnvChanges(string(content), syntax, changes)
	if err != nil {
		return err
	}
	if err = replaceEnv(base, ".env", before, exists, []byte(updated)); err != nil {
		return err
	}
	if template == "thinkphp" {
		if err = os.MkdirAll(filepath.Join(root, "route"), 0755); err != nil {
			return err
		}
		return scaffoldWrite(filepath.Join(root, "route"), "niceenv.php", "<?php\nuse think\\facade\\Route;\nRoute::get('/', function () { return 'ThinkPHP is ready.'; });\n")
	}
	if template == "symfony" {
		if err = os.MkdirAll(filepath.Join(root, "src", "Controller"), 0755); err != nil {
			return err
		}
		if err = scaffoldWrite(filepath.Join(root, "src", "Controller"), "NiceEnvController.php", scaffoldSymfonyController); err != nil {
			return err
		}
		if err = os.MkdirAll(filepath.Join(root, "config", "routes"), 0755); err != nil {
			return err
		}
		return scaffoldWrite(filepath.Join(root, "config", "routes"), "niceenv.yaml", "niceenv_home:\n  path: /\n  controller: App\\Controller\\NiceEnvController::index\n")
	}
	return ctx.Err()
}

const scaffoldSymfonyController = "<?php\nnamespace App\\Controller;\nuse Symfony\\Component\\HttpFoundation\\Response;\nfinal class NiceEnvController { public function index(): Response { return new Response('Symfony is ready.'); } }\n"
