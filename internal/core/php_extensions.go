package core

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strings"
	"time"

	"niceenv/internal/process"
)

type phpExtension struct {
	Name        string   `json:"name"`
	Label       string   `json:"label"`
	Group       string   `json:"group"`
	Hint        string   `json:"hint"`
	Enabled     bool     `json:"enabled"`
	Loaded      bool     `json:"loaded"`
	Zend        bool     `json:"zend"`
	Builtin     bool     `json:"builtin"`
	DLL         string   `json:"dll"`
	MissingDeps []string `json:"missingDeps"`
}

var phpModuleName = regexp.MustCompile(`^[a-z0-9_ -]+$`)
var phpExtensionLine = regexp.MustCompile(`(?i)^\s*(;\s*)?(zend_extension|extension)\s*=\s*([^;]+)`)
var phpDeps = map[string][]string{"pdo_mysql": {"pdo", "mysqlnd"}, "pdo_sqlite": {"pdo"}, "pdo_pgsql": {"pdo"}, "mysqli": {"mysqlnd"}}
var phpToggleLabels = map[string]string{"display_errors": "显示错误", "log_errors": "记录错误日志", "opcache.enable": "启用 OPcache"}

func extensionName(text string) string {
	text = strings.ToLower(strings.Trim(strings.TrimSpace(text), `"'`))
	text = filepath.Base(strings.ReplaceAll(text, `\`, "/"))
	text = strings.TrimSuffix(strings.TrimSuffix(text, ".dll"), ".so")
	text = strings.TrimPrefix(text, "php_")
	if strings.HasPrefix(text, "xdebug-niceenv-") {
		return "xdebug"
	}
	return text
}
func isZend(name string) bool {
	return name == "xdebug" || name == "opcache" || strings.HasPrefix(name, "ioncube_loader") || name == "uploadprogress"
}
func phpEnabled(text string) map[string]bool {
	result := map[string]bool{}
	for _, line := range strings.Split(text, "\n") {
		m := phpExtensionLine.FindStringSubmatch(line)
		if len(m) > 0 && m[1] == "" {
			result[extensionName(m[3])] = true
		}
	}
	return result
}
func (c *Core) phpTarget(ctx context.Context, version string) (*serviceEntry, *resolvedService, configTarget, error) {
	if !safePart(version) {
		return nil, nil, configTarget{}, invalid("PHP 版本无效")
	}
	e, err := c.serviceEntry("php@"+version, version)
	if err != nil {
		return nil, nil, configTarget{}, err
	}
	r, err := c.resolveService(ctx, e)
	if err != nil {
		return nil, nil, configTarget{}, err
	}
	if err = c.prepareServiceDirectories(e, r); err != nil {
		return nil, nil, configTarget{}, err
	}
	if err = c.ensurePHPIni(ctx, e, r); err != nil {
		return nil, nil, configTarget{}, err
	}
	t := configTarget{spec: configSpecs[3], key: "php-ini@" + e.pkg.Version, path: filepath.Join(r.etc, "php.ini"), program: r.program, version: e.pkg.Version}
	return e, r, t, nil
}
func phpModules(ctx context.Context, r *resolvedService, ini string) (map[string]bool, string, error) {
	program := filepath.Join(filepath.Dir(r.program), "php")
	if runtime.GOOS == "windows" {
		program += ".exe"
	}
	args := []string{"-n", "-m"}
	if ini != "" {
		args = []string{"-c", DisplayPath(ini), "-m"}
	}
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	out := process.NewLimitedBuffer(128 * 1024)
	child, err := process.Start(ctx, process.Spec{Program: program, Args: args, Directory: filepath.Dir(program), Environment: map[string]string{"PHP_INI_SCAN_DIR": ""}, Stdout: out, Stderr: out})
	if err != nil {
		return nil, "", err
	}
	exit := child.Wait()
	if ctx.Err() != nil {
		return nil, out.String(), ctx.Err()
	}
	if exit.Err != nil {
		return nil, out.String(), exit.Err
	}
	mods := map[string]bool{}
	inSection := false
	for _, line := range strings.Split(out.String(), "\n") {
		name := strings.ToLower(strings.TrimSpace(line))
		if strings.HasPrefix(name, "[") {
			inSection = name == "[php modules]" || name == "[zend modules]"
			continue
		}
		if inSection && phpModuleName.MatchString(name) {
			if name == "zend opcache" {
				name = "opcache"
			}
			mods[name] = true
		}
	}
	if exit.Code != 0 || len(mods) == 0 {
		return nil, out.String(), &AppError{Code: "PHP_MODULE_PROBE_FAILED", Message: "PHP 无法读取扩展列表", Detail: out.String()}
	}
	return mods, out.String(), nil
}
func (c *Core) phpExtensionView(ctx context.Context, version string) (map[string]any, error) {
	_, r, t, err := c.phpTarget(ctx, version)
	if err != nil {
		return nil, err
	}
	text, err := readConfigText(t.path)
	if err != nil {
		return nil, err
	}
	builtin, _, err := phpModules(ctx, r, "")
	if err != nil {
		return nil, err
	}
	loaded, _, err := phpModules(ctx, r, t.path)
	if err != nil {
		return nil, err
	}
	enabled := phpEnabled(text)
	files, err := os.ReadDir(filepath.Join(filepath.Dir(r.program), "ext"))
	if err != nil && !os.IsNotExist(err) {
		return nil, err
	}
	byName := map[string]phpExtension{}
	for name := range builtin {
		byName[name] = phpExtension{Name: name, Label: name, Group: "basic", Hint: "PHP 运行时内置模块", Enabled: true, Loaded: loaded[name], Builtin: true, Zend: isZend(name), MissingDeps: []string{}}
	}
	for _, file := range files {
		if file.IsDir() || !(strings.HasSuffix(strings.ToLower(file.Name()), ".dll") || strings.HasSuffix(strings.ToLower(file.Name()), ".so")) {
			continue
		}
		name := extensionName(file.Name())
		if builtin[name] {
			continue
		}
		module := name
		if name == "gd2" {
			module = "gd"
		}
		missing := []string{}
		for _, dep := range phpDeps[name] {
			if !loaded[dep] {
				missing = append(missing, dep)
			}
		}
		group := "other"
		if strings.HasPrefix(name, "pdo") || name == "mysqli" || name == "mongodb" {
			group = "database"
		}
		if name == "xdebug" {
			group = "debug"
		}
		if name == "opcache" {
			group = "performance"
		}
		byName[name] = phpExtension{Name: name, Label: name, Group: group, Hint: file.Name(), Enabled: enabled[name], Loaded: loaded[module], Zend: isZend(name), DLL: file.Name(), MissingDeps: missing}
	}
	exts := []phpExtension{}
	for _, ext := range byName {
		exts = append(exts, ext)
	}
	sort.Slice(exts, func(i, j int) bool { return exts[i].Name < exts[j].Name })
	toggles := []map[string]any{}
	for _, key := range []string{"display_errors", "log_errors", "opcache.enable"} {
		value := ""
		for _, line := range strings.Split(text, "\n") {
			k, v, ok := strings.Cut(line, "=")
			if ok && strings.TrimSpace(k) == key {
				value = strings.ToLower(strings.Trim(strings.TrimSpace(strings.SplitN(v, ";", 2)[0]), `"'`))
			}
		}
		toggles = append(toggles, map[string]any{"key": key, "label": phpToggleLabels[key], "hint": "修改后重启当前 PHP 池生效", "value": value == "on" || value == "1" || value == "true" || value == "yes"})
	}
	return map[string]any{"version": version, "iniPath": Path(t.path), "extensions": exts, "toggles": toggles}, nil
}
func setPHPExtensionText(text, name string, enabled bool) string {
	lines := strings.Split(text, "\n")
	found := false
	for i, line := range lines {
		m := phpExtensionLine.FindStringSubmatch(line)
		if len(m) == 0 || extensionName(m[3]) != name {
			continue
		}
		if enabled && !found {
			directive := "extension"
			if isZend(name) {
				directive = "zend_extension"
			}
			lines[i] = directive + "=" + strings.TrimSpace(m[3])
			found = true
		} else if m[1] == "" {
			lines[i] = ";" + line
		}
	}
	if enabled && !found {
		directive := "extension"
		if isZend(name) {
			directive = "zend_extension"
		}
		lines = append(lines, directive+"="+name)
	}
	return strings.Join(lines, "\n")
}
func (c *Core) registerPHP() {
	c.handle("php_extensions", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Version string `json:"version"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		return c.phpExtensionView(ctx, in.Version)
	})
	c.handle("set_php_extension", c.setPHPExtension)
	c.handle("set_php_ini_toggle", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Version string `json:"version"`
			Key     string `json:"key"`
			Value   bool   `json:"value"`
		}](data)
		if err != nil {
			return nil, err
		}
		if _, ok := phpToggleLabels[in.Key]; !ok {
			return nil, invalid("未知 PHP 开关")
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		e, _, t, err := c.phpTarget(ctx, in.Version)
		if err != nil {
			return nil, err
		}
		before, err := readConfigText(t.path)
		if err != nil {
			return nil, err
		}
		value := "0"
		if in.Value {
			value = "1"
		}
		lines := strings.Split(before, "\n")
		found := false
		for i, line := range lines {
			k, _, ok := strings.Cut(line, "=")
			if ok && strings.TrimSpace(k) == in.Key {
				lines[i] = in.Key + "=" + value
				found = true
			}
		}
		if !found {
			lines = append(lines, in.Key+"="+value)
		}
		if err = c.writeConfig(ctx, t, strings.Join(lines, "\n"), &before); err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		running := e.child != nil
		c.services.mu.RUnlock()
		if running {
			if err = c.stopOwned(ctx, e, false); err == nil {
				err = c.startService(ctx, e)
			}
		}
		return true, err
	})
}
func (c *Core) setPHPExtension(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Version string `json:"version"`
		Name    string `json:"name"`
		Enabled bool   `json:"enabled"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	view, err := c.phpExtensionView(ctx, in.Version)
	if err != nil {
		return nil, err
	}
	available := map[string]phpExtension{}
	for _, ext := range view["extensions"].([]phpExtension) {
		available[ext.Name] = ext
	}
	ext, ok := available[in.Name]
	if !ok || ext.Builtin {
		return nil, invalid("此扩展不存在或为不可切换的内置模块")
	}
	e, r, t, err := c.phpTarget(ctx, in.Version)
	if err != nil {
		return nil, err
	}
	before, err := readConfigText(t.path)
	if err != nil {
		return nil, err
	}
	text := before
	if in.Enabled {
		for _, dep := range phpDeps[in.Name] {
			d, ok := available[dep]
			if !ok {
				return nil, &AppError{Code: "PHP_DEPENDENCY_MISSING", Message: "缺少扩展依赖 " + dep}
			}
			if !d.Builtin {
				text = setPHPExtensionText(text, dep, true)
			}
		}
	} else {
		for name, deps := range phpDeps {
			if !available[name].Enabled {
				continue
			}
			for _, dep := range deps {
				if dep == in.Name {
					return nil, &AppError{Code: "PHP_EXTENSION_REQUIRED", Message: name + " 依赖此扩展，请先停用依赖方"}
				}
			}
		}
	}
	text = setPHPExtensionText(text, in.Name, in.Enabled)
	if err = c.writeConfig(ctx, t, text, &before); err != nil {
		return nil, err
	}
	loaded, output, probeErr := phpModules(ctx, r, t.path)
	module := in.Name
	if module == "gd2" {
		module = "gd"
	}
	if probeErr != nil || in.Enabled && !loaded[module] {
		rollbackErr := c.writeConfig(ctx, t, before, &text)
		detail := output
		if rollbackErr != nil {
			detail += "\n恢复失败: " + rollbackErr.Error()
		}
		return nil, &AppError{Code: "PHP_EXTENSION_LOAD_FAILED", Message: "PHP 未能加载扩展，已尝试恢复原配置", Detail: detail}
	}
	warnings := []string{}
	c.services.mu.RLock()
	running := e.child != nil
	c.services.mu.RUnlock()
	needsRestart := running
	if running {
		if err = c.stopOwned(ctx, e, false); err == nil {
			err = c.startService(ctx, e)
		}
		if err != nil {
			warnings = append(warnings, "配置已保存，PHP 重启失败："+err.Error())
		} else {
			needsRestart = false
		}
	}
	return map[string]any{"name": in.Name, "enabled": in.Enabled, "warnings": warnings, "needsRestart": needsRestart}, nil
}
