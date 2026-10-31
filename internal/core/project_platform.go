package core

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"niceenv/internal/process"
)

const phpRequirementProbe = `
try {
 require 'phar://'.$argv[1].'/vendor/autoload.php';
 $input = json_decode(file_get_contents($argv[2]), true, 512, JSON_THROW_ON_ERROR);
 $parser = new Composer\Semver\VersionParser();
 try { $parser->parseConstraints($input['requirement']); }
 catch (Throwable $e) { echo json_encode(['invalid'=>$e->getMessage()]); exit(0); }
 echo json_encode(['matching'=>array_values(Composer\Semver\Semver::satisfiedBy($input['versions'], $input['requirement']))], JSON_THROW_ON_ERROR);
} catch (Throwable $e) { echo json_encode(['error'=>$e->getMessage()]); }
`
const phpPlatformProbe = `
require 'phar://'.$argv[1].'/vendor/autoload.php';
$original = file_get_contents('original.json');
$lock = json_decode(file_get_contents('composer.lock'), true, 512, JSON_THROW_ON_ERROR);
$fresh = isset($lock['content-hash']) ? hash_equals($lock['content-hash'], Composer\Package\Locker::getContentHash($original)) : null;
file_put_contents('runtime.json', json_encode(['php'=>PHP_VERSION,'ini'=>php_ini_loaded_file(),'fresh'=>$fresh], JSON_THROW_ON_ERROR));
$args = ['command'=>'check-platform-reqs','--lock'=>true,'--format'=>'json','--no-plugins'=>true,'--no-scripts'=>true,'--no-interaction'=>true,'--no-ansi'=>true,'--no-cache'=>true];
if ($argv[2] === 'no-dev') $args['--no-dev'] = true;
$app = new Composer\Console\Application();
$app->setAutoExit(false);
exit($app->run(new Symfony\Component\Console\Input\ArrayInput($args)));
`

func (c *Core) projectPHPBinaries(ctx context.Context, version string) (string, string, error) {
	packages, err := c.installedRuntimePackages(ctx)
	if err != nil {
		return "", "", err
	}
	chosen, err := c.pathChosen(ctx, pathSelection{}, packages)
	if err != nil {
		return "", "", err
	}
	composer, ok := chosen["composer"]
	if !ok {
		return "", "", &AppError{Code: "NOT_INSTALLED", Message: "请先安装 Composer"}
	}
	var php Package
	for _, p := range packages {
		if p.ID == "php" && canonicalVersion(p.Version) == canonicalVersion(version) {
			php = p
			break
		}
	}
	if php.Install == nil {
		return "", "", &AppError{Code: "NOT_INSTALLED", Message: "请先安装所选 PHP 版本"}
	}
	phpDir, err := runtimeDirectory(php)
	if err != nil {
		return "", "", err
	}
	phpEntry, err := runtimeCLI(php)
	if err != nil {
		return "", "", err
	}
	phpExe := filepath.Join(phpDir, filepath.Base(phpEntry))
	composerPath, err := relativePath(composer.Install.InstallPath, composer.Entry)
	if err != nil {
		return "", "", err
	}
	root, err := os.OpenRoot(composer.Install.InstallPath)
	if err != nil {
		return "", "", err
	}
	defer root.Close()
	info, err := siteCheck(root, filepath.FromSlash(strings.ReplaceAll(composer.Entry, `\`, "/")), false)
	if err != nil {
		return "", "", err
	}
	if info == nil || !info.Mode().IsRegular() {
		return "", "", invalid("Composer 安装入口不是普通文件")
	}
	return phpExe, composerPath, nil
}
func phpCheckSpec(program, dir string, args []string) process.Spec {
	remove := []string{"PHPRC"}
	for _, item := range os.Environ() {
		key, _, _ := strings.Cut(item, "=")
		upper := strings.ToUpper(key)
		if upper == "COMPOSER" || strings.HasPrefix(upper, "COMPOSER_") {
			remove = append(remove, key)
		}
	}
	// Remove inherited values first; the required per-check values are supplied
	// after filtering, so callers never share Composer's global home or plugins.
	env := map[string]string{"PHP_INI_SCAN_DIR": "", "COMPOSER_HOME": filepath.Join(dir, "home"), "COMPOSER_CACHE_DIR": filepath.Join(dir, "cache"), "COMPOSER_DISABLE_NETWORK": "1", "COMPOSER_NO_INTERACTION": "1", "COMPOSER_ALLOW_SUPERUSER": "1", "COMPOSER_MAX_PARALLEL_PROCESSES": "1"}
	remove = slices.DeleteFunc(remove, func(key string) bool { _, ok := env[strings.ToUpper(key)]; return ok })
	return process.Spec{Program: program, Directory: dir, Args: args, Environment: env, RemoveEnvironment: remove}
}
func runPHPCheck(ctx context.Context, spec process.Spec, timeout time.Duration) (int, string, string, error) {
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	out, stderr := process.NewLimitedBuffer(1024*1024), process.NewLimitedBuffer(64*1024)
	spec.Stdout = out
	spec.Stderr = stderr
	child, err := process.Start(ctx, spec)
	if err != nil {
		return 0, "", "", err
	}
	result := child.Wait()
	if ctx.Err() != nil {
		return result.Code, out.String(), stderr.String(), &AppError{Code: "PHP_CHECK_TIMEOUT", Message: "PHP 环境检查超时", Detail: stderr.String()}
	}
	if result.Err != nil {
		return result.Code, out.String(), stderr.String(), result.Err
	}
	return result.Code, out.String(), stderr.String(), nil
}
func (c *Core) composerMatchingPHP(ctx context.Context, requirement string, versions []string) ([]string, error) {
	if len(versions) == 0 {
		return nil, &AppError{Code: "NOT_INSTALLED", Message: "请先安装 PHP"}
	}
	if len(versions) > 128 {
		return nil, invalid("已安装 PHP 版本过多，请手动确认版本要求")
	}
	php, composer, err := c.projectPHPBinaries(ctx, versions[0])
	if err != nil {
		return nil, err
	}
	tmp, err := os.MkdirTemp("", "niceenv-php-check-")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(tmp)
	input, err := json.Marshal(map[string]any{"requirement": requirement, "versions": versions})
	if err != nil {
		return nil, err
	}
	if err = os.WriteFile(filepath.Join(tmp, "requirements.json"), input, 0600); err != nil {
		return nil, err
	}
	code, out, detail, err := runPHPCheck(ctx, phpCheckSpec(php, tmp, []string{"-n", "-d", "memory_limit=64M", "-d", "display_errors=stderr", "-r", phpRequirementProbe, "--", DisplayPath(composer), DisplayPath(filepath.Join(tmp, "requirements.json"))}), 10*time.Second)
	if err != nil {
		return nil, err
	}
	var reply struct {
		Matching *[]string `json:"matching"`
		Invalid  *string   `json:"invalid"`
		Error    *string   `json:"error"`
	}
	if code != 0 || json.Unmarshal([]byte(out), &reply) != nil {
		return nil, &AppError{Code: "PHP_CHECK_FAILED", Message: "Composer 未返回有效的版本校验结果", Detail: detail}
	}
	if reply.Invalid != nil {
		return nil, &AppError{Code: "PHP_REQUIREMENT_INVALID", Message: "composer.json 的 PHP 版本约束无法解析", Detail: *reply.Invalid}
	}
	if reply.Error != nil {
		return nil, &AppError{Code: "PHP_CHECK_FAILED", Message: "无法使用已安装 Composer 检查版本", Detail: *reply.Error}
	}
	if reply.Matching == nil {
		return nil, invalid("Composer 版本校验结果不完整")
	}
	for _, v := range *reply.Matching {
		if !slices.Contains(versions, v) {
			return nil, invalid("Composer 返回未知 PHP 版本")
		}
	}
	return *reply.Matching, nil
}

type platformRequirement struct {
	Name              string `json:"name"`
	Version           string `json:"version"`
	Status            string `json:"status"`
	FailedRequirement *struct {
		Source     string `json:"source"`
		Constraint string `json:"constraint"`
	} `json:"failedRequirement"`
	Provider *string `json:"provider"`
}
type projectPlatformReport struct {
	Project         Path                  `json:"project"`
	PHPVersion      string                `json:"phpVersion"`
	Ini             Path                  `json:"ini"`
	Source          string                `json:"source"`
	IncludeDev      bool                  `json:"includeDev"`
	DevIncomplete   bool                  `json:"devIncomplete"`
	LockFresh       *bool                 `json:"lockFresh"`
	AutoloadPresent bool                  `json:"autoloadPresent"`
	Requirements    []platformRequirement `json:"requirements"`
	Diagnostics     *string               `json:"diagnostics"`
}

func platformLinks(value map[string]any, key string) (map[string]string, error) {
	out := map[string]string{}
	raw, ok := value[key]
	if !ok {
		return out, nil
	}
	obj, ok := raw.(map[string]any)
	if !ok {
		return nil, invalid(key + " 必须是名称与版本约束的映射")
	}
	for name, v := range obj {
		text, ok := v.(string)
		if !ok || name == "" || len(name) > 256 || text == "" || len(text) > 4096 {
			return nil, invalid(key + " 包含无效依赖约束")
		}
		out[name] = text
	}
	return out, nil
}
func platformPackages(value any) ([]map[string]any, error) {
	array, ok := value.([]any)
	if !ok || len(array) > 10000 {
		return nil, invalid("依赖 packages 数组无效或数量超过限制")
	}
	out := []map[string]any{}
	for _, raw := range array {
		p, ok := raw.(map[string]any)
		if !ok {
			return nil, invalid("依赖包格式无效")
		}
		item := map[string]any{}
		for _, key := range []string{"name", "version"} {
			v, ok := p[key].(string)
			if !ok || v == "" || len(v) > 256 {
				return nil, invalid("依赖缺少有效 " + key)
			}
			item[key] = v
		}
		for _, key := range []string{"require", "provide", "replace"} {
			v, err := platformLinks(p, key)
			if err != nil {
				return nil, err
			}
			item[key] = v
		}
		out = append(out, item)
	}
	return out, nil
}
func (c *Core) projectPlatformCheck(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Project    string `json:"project"`
		SiteID     string `json:"siteId"`
		Version    string `json:"version"`
		IncludeDev bool   `json:"includeDev"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.projectChecks.TryLock() {
		return nil, &AppError{Code: "PHP_PLATFORM_BUSY", Message: "已有项目环境检查正在运行"}
	}
	defer c.projectChecks.Unlock()
	if (in.SiteID == "") == (in.Project == "") {
		return nil, invalid("请选择一个项目或已保存的站点")
	}
	if in.SiteID != "" {
		site, err := c.site(ctx, in.SiteID)
		if err != nil {
			return nil, err
		}
		in.Project = projectRoot(string(site.RootDir))
	}
	path, err := filepath.Abs(DisplayPath(in.Project))
	if err != nil {
		return nil, err
	}
	if in.Project == "" || !plainProjectDirectory(path) {
		return nil, invalid("请选择有效项目根目录")
	}
	php, composer, err := c.projectPHPBinaries(ctx, in.Version)
	if err != nil {
		return nil, err
	}
	// Read the managed ini without running service setup or executing project code.
	entry, err := c.serviceEntry("php@"+in.Version, in.Version)
	if err != nil {
		return nil, err
	}
	resolved, err := c.resolveService(ctx, entry)
	if err != nil {
		return nil, err
	}
	ini := filepath.Join(resolved.etc, "php.ini")
	if _, err = readConfigText(ini); err != nil {
		return nil, &AppError{Code: "PHP_PLATFORM_INI", Message: "所选 PHP 缺少有效配置，请先启动或修复此版本", Detail: err.Error()}
	}
	root, err := os.OpenRoot(path)
	if err != nil {
		return nil, err
	}
	defer root.Close()
	manifest, original, exists, err := projectMetadata(root, "composer.json", 512*1024)
	if err != nil {
		return nil, err
	}
	if !exists {
		return nil, &AppError{Code: "PHP_PLATFORM_NO_MANIFEST", Message: "此目录没有 composer.json，请选择项目根目录"}
	}
	clean := map[string]any{"name": "niceenv/platform-check", "version": "1.0.0", "config": map[string]any{"allow-plugins": false, "disable-tls": true}, "repositories": []map[string]any{{"packagist.org": false}}}
	for _, key := range []string{"name", "version"} {
		if v, ok := manifest[key].(string); ok {
			clean[key] = v
		}
	}
	for _, key := range []string{"require", "require-dev", "provide", "replace"} {
		v, err := platformLinks(manifest, key)
		if err != nil {
			return nil, err
		}
		if _, ok := manifest["version"].(string); !ok && (key == "provide" || key == "replace") {
			for _, constraint := range v {
				if constraint == "self.version" {
					return nil, invalid("根包使用 self.version，但未声明版本，无法只读确认能力版本")
				}
			}
		}
		clean[key] = v
	}
	lock := map[string]any{"packages": []any{}, "packages-dev": []any{}, "aliases": []any{}, "platform": map[string]string{}, "platform-dev": map[string]string{}}
	r := projectPlatformReport{Project: Path(path), IncludeDev: in.IncludeDev, Source: "manifest", Requirements: []platformRequirement{}}
	vendor := "vendor"
	if raw, ok := manifest["config"]; ok {
		config, ok := raw.(map[string]any)
		if !ok {
			return nil, invalid("composer.json config 必须为对象")
		}
		if v, ok := config["vendor-dir"]; ok {
			vendor, ok = v.(string)
			if !ok || vendor == "" {
				return nil, invalid("vendor-dir 无效")
			}
		}
	}
	vendor = strings.TrimSuffix(strings.TrimPrefix(strings.ReplaceAll(vendor, `\`, "/"), "./"), "/")
	if _, err = relativePath(path, vendor); err != nil {
		return nil, err
	}
	if info, err := siteCheck(root, filepath.Join(vendor, "autoload.php"), true); err == nil {
		r.AutoloadPresent = info != nil && info.Mode().IsRegular()
	}
	if existing, _, exists, err := projectMetadata(root, "composer.lock", 8*1024*1024); err != nil {
		return nil, err
	} else if exists {
		for _, key := range []string{"packages", "packages-dev"} {
			v, err := platformPackages(existing[key])
			if err != nil {
				return nil, err
			}
			lock[key] = v
		}
		for _, key := range []string{"platform", "platform-dev"} {
			v, err := platformLinks(existing, key)
			if err != nil {
				return nil, err
			}
			lock[key] = v
		}
		if raw, ok := existing["content-hash"]; ok {
			hash, ok := raw.(string)
			decoded, err := hex.DecodeString(hash)
			if !ok || err != nil || len(decoded) != 16 {
				return nil, invalid("composer.lock content-hash 无效")
			}
			lock["content-hash"] = hash
		}
		r.Source = "lock"
	} else {
		// Composer 1 installed metadata is an array; Composer 2 wraps packages.
		value, _, exists, err := projectJSON(root, filepath.Join(vendor, "composer", "installed.json"), 8*1024*1024)
		if err != nil {
			return nil, err
		}
		if exists {
			var all any = value
			installed, _ := value.(map[string]any)
			if installed != nil {
				all = installed["packages"]
			}
			packages, err := platformPackages(all)
			if err != nil {
				return nil, err
			}
			devNames, present := installed["dev-package-names"]
			names, valid := devNames.([]any)
			if present && !valid {
				return nil, invalid("开发依赖分组无效")
			}
			if !in.IncludeDev && !present {
				return nil, invalid("已安装依赖没有开发分组，请包含开发依赖或提供 composer.lock")
			}
			dev := map[string]bool{}
			for _, raw := range names {
				name, ok := raw.(string)
				if !ok {
					return nil, invalid("开发依赖名称无效")
				}
				dev[name] = true
			}
			regular, development := []map[string]any{}, []map[string]any{}
			for _, p := range packages {
				if dev[p["name"].(string)] {
					development = append(development, p)
				} else {
					regular = append(regular, p)
				}
			}
			lock["packages"], lock["packages-dev"] = regular, development
			r.Source = "installed"
			r.DevIncomplete = in.IncludeDev && installed["dev"] != true
		}
	}
	tmp, err := os.MkdirTemp("", "niceenv-platform-")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(tmp)
	if err = os.Mkdir(filepath.Join(tmp, "home"), 0700); err != nil {
		return nil, err
	}
	for name, value := range map[string]any{"composer.json": clean, "composer.lock": lock} {
		bytes, err := json.Marshal(value)
		if err != nil {
			return nil, err
		}
		if err = os.WriteFile(filepath.Join(tmp, name), bytes, 0600); err != nil {
			return nil, err
		}
	}
	if err = os.WriteFile(filepath.Join(tmp, "original.json"), original, 0600); err != nil {
		return nil, err
	}
	devMode := "no-dev"
	if in.IncludeDev {
		devMode = "dev"
	}
	args := []string{"-c", DisplayPath(ini), "-d", "memory_limit=128M", "-d", "auto_prepend_file=", "-d", "auto_append_file=", "-d", "opcache.preload=", "-d", "display_errors=stderr", "-d", "log_errors=0", "-r", phpPlatformProbe, "--", DisplayPath(composer), devMode}
	code, output, detail, err := runPHPCheck(ctx, phpCheckSpec(php, tmp, args), 15*time.Second)
	if err != nil {
		return nil, err
	}
	var requirements []map[string]json.RawMessage
	if json.Unmarshal([]byte(output), &requirements) != nil || requirements == nil {
		return nil, &AppError{Code: "PHP_PLATFORM_FAILED", Message: "Composer 未能完成环境检查", Detail: detail}
	}
	expected := 0
	for _, row := range requirements {
		if v, ok := row["failed_requirement"]; ok {
			row["failedRequirement"] = v
			delete(row, "failed_requirement")
		}
		encoded, err := json.Marshal(row)
		if err != nil {
			return nil, err
		}
		var req platformRequirement
		if err = json.Unmarshal(encoded, &req); err != nil {
			return nil, err
		}
		switch req.Status {
		case "success":
		case "failed":
			expected = max(expected, 1)
		case "missing":
			expected = max(expected, 2)
		default:
			return nil, invalid("Composer 返回未知检查状态")
		}
		r.Requirements = append(r.Requirements, req)
	}
	if code != expected {
		return nil, &AppError{Code: "PHP_PLATFORM_FAILED", Message: "Composer 退出状态与检查结果不一致", Detail: detail}
	}
	metadata, err := os.ReadFile(filepath.Join(tmp, "runtime.json"))
	if err != nil {
		return nil, err
	}
	var actual struct {
		PHP   string `json:"php"`
		Ini   string `json:"ini"`
		Fresh *bool  `json:"fresh"`
	}
	if json.Unmarshal(metadata, &actual) != nil || actual.PHP == "" || projectPathKey(actual.Ini) != projectPathKey(ini) {
		return nil, invalid("PHP 环境检查未加载预期配置")
	}
	r.PHPVersion = actual.PHP
	r.Ini = Path(actual.Ini)
	r.LockFresh = actual.Fresh
	lines := []string{}
	for _, line := range strings.Split(detail, "\n") {
		line = strings.TrimSpace(line)
		if line != "" && !slices.Contains([]string{"You are running Composer with SSL/TLS protection disabled.", "Checking non-dev platform requirements using the lock file", "Checking platform requirements using the lock file"}, line) {
			lines = append(lines, line)
		}
	}
	if len(lines) > 0 {
		diagnostics := strings.Join(lines, "\n")
		r.Diagnostics = &diagnostics
	}
	return r, nil
}

func (c *Core) validateExistingPHP(ctx context.Context, site Site, project string, allow bool) error {
	if site.Runtime.Kind != "php" {
		return nil
	}
	report, err := c.projectPHPCompatibility(ctx, project)
	if err != nil {
		return err
	}
	switch report.Status {
	case "invalid":
		return &AppError{Code: "PHP_REQUIREMENT_INVALID", Message: *report.Message}
	case "unavailable":
		if !allow {
			return &AppError{Code: "PHP_CHECK_UNAVAILABLE", Message: *report.Message, Hint: "请安装或修复 PHP、Composer；或在建站页面明确确认已自行核对"}
		}
	case "checked":
		match := false
		for _, v := range report.MatchingVersions {
			match = match || site.Runtime.PHPVersion != nil && canonicalVersion(*site.Runtime.PHPVersion) == canonicalVersion(v)
		}
		if !match {
			return &AppError{Code: "PHP_VERSION_MISMATCH", Message: "所选 PHP 不符合项目要求 " + *report.Requirement}
		}
	}
	return nil
}
