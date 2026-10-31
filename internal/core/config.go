package core

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	"niceenv/internal/process"
)

type configSpec struct {
	kind, pkg, file, language, label string
	shared                           bool
}

var configSpecs = []configSpec{
	{"nginx-main", "nginx", "nginx.conf", "nginx", "Nginx 主配置", true},
	{"apache-conf", "apache", "httpd.conf", "apache", "Apache 主配置", true},
	{"mihomo-config", "mihomo", "config.yaml", "yaml", "Mihomo 配置", true},
	{"php-ini", "php", "php.ini", "ini", "PHP 配置", false},
	{"mysql-ini", "mysql", "my.ini", "ini", "MySQL 配置", false},
	{"mariadb-ini", "mariadb", "my.ini", "ini", "MariaDB 配置", false},
	{"redis-conf", "redis", "redis.conf", "conf", "Redis 配置", false},
	{"postgres-conf", "postgresql", "postgresql.conf", "ini", "PostgreSQL 配置", false},
	{"mongo-conf", "mongodb", "mongod.conf", "yaml", "MongoDB 配置", false},
	{"qdrant-conf", "qdrant", "config.yaml", "yaml", "Qdrant 配置", false},
	{"caddy-conf", "caddy", "Caddyfile", "caddy", "Caddy 配置", false},
}

type configTarget struct {
	spec                        configSpec
	key, path, program, version string
}
type configIssue struct {
	Line     int    `json:"line"`
	Severity string `json:"severity"`
	Message  string `json:"message"`
}
type configValidation struct {
	OK       bool          `json:"ok"`
	Messages []string      `json:"messages"`
	Issues   []configIssue `json:"issues"`
}

func (c *Core) configTargets(ctx context.Context) ([]configTarget, error) {
	views, err := c.packageViews(ctx)
	if err != nil {
		return nil, err
	}
	out := []configTarget{}
	for _, view := range views {
		installation, ok := view["install"].(map[string]any)
		if !ok {
			continue
		}
		id, _ := view["id"].(string)
		version, _ := view["version"].(string)
		entry, _ := view["entry"].(string)
		if !safePart(id) || !safePart(version) {
			return nil, invalid("配置对应的套件版本无效")
		}
		var root string
		switch path := installation["installPath"].(type) {
		case Path:
			root = string(path)
		case string:
			root = path
		default:
			return nil, invalid("套件安装目录格式无效")
		}
		program, err := relativePath(root, entry)
		if err != nil {
			return nil, err
		}
		for _, spec := range configSpecs {
			if spec.pkg != id || spec.shared && view["active"] != true {
				continue
			}
			key := spec.kind
			path := filepath.Join(c.Home, "etc", id, spec.file)
			if !spec.shared {
				key += "@" + version
				path = filepath.Join(c.Home, "etc", id, version, spec.file)
			}
			if id == "postgresql" {
				path = filepath.Join(c.Home, "data", id, version, spec.file)
			}
			out = append(out, configTarget{spec: spec, key: key, path: path, program: program, version: version})
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].key < out[j].key })
	return out, nil
}
func (c *Core) configTarget(ctx context.Context, key string) (configTarget, error) {
	kind, version, exact := strings.Cut(key, "@")
	var spec *configSpec
	for i := range configSpecs {
		if configSpecs[i].kind == kind || configSpecs[i].pkg == kind {
			spec = &configSpecs[i]
			break
		}
	}
	if spec == nil || exact && (!safePart(version) || spec.shared) {
		return configTarget{}, invalid("配置类型或版本无效")
	}
	targets, err := c.configTargets(ctx)
	if err != nil {
		return configTarget{}, err
	}
	chosen := ""
	if !exact && !spec.shared {
		chosen, _, err = c.Store.setting(ctx, "active"+spec.pkg+"Version")
		if err != nil {
			return configTarget{}, err
		}
	}
	var selected *configTarget
	for i := range targets {
		target := &targets[i]
		if target.spec.kind != spec.kind || exact && canonicalVersion(target.version) != canonicalVersion(version) {
			continue
		}
		if exact || spec.shared || canonicalVersion(target.version) == canonicalVersion(chosen) {
			return *target, nil
		}
		if selected == nil || newerVersion(target.version, selected.version) {
			selected = target
		}
	}
	if selected != nil {
		return *selected, nil
	}
	return configTarget{}, &AppError{Code: "NOT_INSTALLED", Message: "该配置对应的套件版本尚未安装"}
}
func readConfigText(path string) (string, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return "", err
	}
	if !info.Mode().IsRegular() || info.Size() > maxTextFile {
		return "", invalid("配置不是普通文件或超过 8 MiB")
	}
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	data, err := io.ReadAll(io.LimitReader(file, maxTextFile+1))
	if err != nil {
		return "", err
	}
	if len(data) > maxTextFile || !utf8.Valid(data) {
		return "", invalid("配置不是有效的 UTF-8 文本或超过大小限制")
	}
	return string(data), nil
}

func (c *Core) registerConfigs() {
	c.handle("config_list", func(ctx context.Context, _ json.RawMessage) (any, error) {
		targets, err := c.configTargets(ctx)
		if err != nil {
			return nil, err
		}
		out := []map[string]any{}
		statuses := c.serviceStatuses()
		for _, target := range targets {
			exists := false
			var size int64
			info, err := os.Lstat(target.path)
			if err == nil {
				if !info.Mode().IsRegular() {
					return nil, invalid("配置路径不是普通文件")
				}
				exists = true
				size = info.Size()
			} else if !os.IsNotExist(err) {
				return nil, err
			}
			var used any
			for _, status := range statuses {
				base, _, _ := strings.Cut(status.ID, "@")
				if base == target.spec.pkg && status.Version == target.version && len(status.PIDs) > 0 {
					used = status.ID
					break
				}
			}
			native := target.spec.pkg == "nginx" || target.spec.pkg == "apache" || target.spec.pkg == "caddy"
			out = append(out, map[string]any{"kind": target.key, "label": target.spec.label + " " + target.version, "description": "保存前检查内容并备份原文件；已运行服务需要重新加载配置。", "path": Path(target.path), "exists": exists, "sizeBytes": size, "language": target.spec.language, "validated": native, "usedByService": used, "requiresPackage": target.spec.pkg, "resettable": configResettable(target.spec.pkg)})
		}
		return out, nil
	})
	c.handle("config_read", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Kind string `json:"kind"`
		}](data)
		if err != nil {
			return nil, err
		}
		target, err := c.configTarget(ctx, in.Kind)
		if err != nil {
			return nil, err
		}
		return readConfigText(target.path)
	})
	c.handle("config_validate", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Kind    string `json:"kind"`
			Content string `json:"content"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		target, err := c.configTarget(ctx, in.Kind)
		if err != nil {
			return nil, err
		}
		return c.validateConfig(ctx, target, in.Content)
	})
	c.handle("config_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Kind            string  `json:"kind"`
			Content         string  `json:"content"`
			Force           bool    `json:"force"`
			ExpectedContent *string `json:"expectedContent"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		target, err := c.configTarget(ctx, in.Kind)
		if err != nil {
			return nil, err
		}
		validation, err := c.validateConfig(ctx, target, in.Content)
		if err != nil {
			return nil, err
		}
		if !validation.OK && !in.Force {
			return nil, &AppError{Code: "CONFIG_INVALID", Message: "配置校验未通过，未写入", Detail: strings.Join(validation.Messages, "\n")}
		}
		return validation, c.writeConfig(ctx, target, in.Content, in.ExpectedContent)
	})
	c.handle("config_backups", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Kind *string `json:"kind"`
		}](data)
		if err != nil {
			return nil, err
		}
		selected := ""
		if in.Kind != nil {
			target, err := c.configTarget(ctx, *in.Kind)
			if err != nil {
				return nil, err
			}
			selected = target.key
		}
		return c.configBackups(selected)
	})
	c.registerConfigRestore()
	c.registerConfigReset()
	c.handle("config_apply", c.applyConfig)
}

func (c *Core) applyConfig(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Kind            string `json:"kind"`
		ExpectedPath    string `json:"expectedPath"`
		ExpectedContent string `json:"expectedContent"`
		ServiceID       string `json:"serviceId"`
		Revision        string `json:"revision"`
		Restart         bool   `json:"restart"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	target, err := c.configTarget(ctx, in.Kind)
	if err != nil {
		return nil, err
	}
	if DisplayPath(in.ExpectedPath) != DisplayPath(target.path) {
		return nil, invalid("配置目标已变化")
	}
	current, err := readConfigText(target.path)
	if err != nil {
		return nil, err
	}
	if current != in.ExpectedContent {
		return nil, &AppError{Code: "CONFIG_CONFLICT", Message: "磁盘配置与确认内容不一致，请重新读取"}
	}
	e, err := c.serviceEntry(in.ServiceID, target.version)
	if err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	valid := e.pkg.ID == target.spec.pkg && e.revision == in.Revision && in.Revision != "" && e.child != nil && e.status.State == "running"
	c.services.mu.RUnlock()
	if !valid {
		return nil, &AppError{Code: "SERVICE_TARGET_CHANGED", Message: "配置对应的运行实例已变化，请重新确认"}
	}
	validation, err := c.validateConfig(ctx, target, current)
	if err != nil {
		return nil, err
	}
	if !validation.OK {
		return nil, &AppError{Code: "CONFIG_INVALID", Message: "配置校验失败，未应用", Detail: strings.Join(validation.Messages, "\n")}
	}
	if in.Restart {
		if err = c.stopOwned(ctx, e, false); err != nil {
			return nil, err
		}
		return true, c.startService(ctx, e)
	}
	if e.pkg.ID == "nginx" {
		return true, c.reloadNginxPools(ctx)
	}
	if e.pkg.ID == "apache" {
		return true, c.reloadApache(ctx)
	}
	if e.pkg.ID == "caddy" {
		return true, c.reloadCaddy(ctx)
	}
	return nil, &AppError{Code: "CONFIG_RESTART_REQUIRED", Message: "此服务需要重启才能应用配置，请选择重启"}
}

func (c *Core) validateConfig(ctx context.Context, target configTarget, content string) (configValidation, error) {
	result := configValidation{OK: true, Messages: []string{}, Issues: []configIssue{}}
	if len(content) > maxTextFile || !utf8.ValidString(content) || strings.ContainsRune(content, 0) {
		return result, invalid("配置内容不是有效文本或超过大小限制")
	}
	if target.spec.language == "ini" {
		for index, line := range strings.Split(content, "\n") {
			line = strings.TrimSpace(line)
			if line == "" || strings.HasPrefix(line, ";") || strings.HasPrefix(line, "#") || strings.HasPrefix(line, "!") {
				continue
			}
			if strings.HasPrefix(line, "[") {
				if strings.Contains(line, "]") {
					continue
				}
				result.Issues = append(result.Issues, configIssue{Line: index + 1, Severity: "error", Message: "配置节名称缺少右方括号"})
				continue
			}
			if !strings.Contains(line, "=") {
				result.Issues = append(result.Issues, configIssue{Line: index + 1, Severity: "warning", Message: "此行没有等号，可能是原生服务支持的无值选项"})
			}
		}
		for _, issue := range result.Issues {
			if issue.Severity == "error" {
				result.OK = false
			}
		}
		result.Messages = append(result.Messages, "仅完成 INI 结构检查；选项名称、扩展依赖和实际可运行性需由服务原生检查确认。")
		return result, nil
	}
	if target.spec.pkg != "nginx" && target.spec.pkg != "apache" && target.spec.pkg != "caddy" {
		result.OK = false
		result.Messages = append(result.Messages, "该格式的原生校验器尚未迁移，不能确认配置有效。需要保存原文时可明确选择强制保存。")
		result.Issues = append(result.Issues, configIssue{Severity: "warning", Message: result.Messages[0]})
		return result, nil
	}
	if err := managedDirectory(c.Home, filepath.Dir(target.path)); err != nil {
		return result, err
	}
	file, err := os.CreateTemp(filepath.Dir(target.path), ".niceenv-validate-")
	if err != nil {
		return result, err
	}
	name := file.Name()
	defer os.Remove(name)
	_, writeErr := file.WriteString(content)
	closeErr := file.Close()
	if err = errors.Join(writeErr, closeErr); err != nil {
		return result, err
	}
	root := filepath.Dir(target.program)
	args := []string{}
	switch target.spec.pkg {
	case "nginx":
		args = []string{"-p", DisplayPath(root) + string(filepath.Separator), "-c", DisplayPath(name), "-t"}
	case "apache":
		root = filepath.Dir(root)
		args = []string{"-d", DisplayPath(root), "-f", DisplayPath(name), "-t"}
	case "caddy":
		args = []string{"validate", "--config", DisplayPath(name), "--adapter", "caddyfile"}
	}
	checkCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	output := process.NewLimitedBuffer(64 * 1024)
	var environment map[string]string
	if target.spec.pkg == "caddy" {
		xdg := filepath.Join(c.Home, "data", "caddy", "xdg")
		if err := managedDirectory(c.Home, xdg); err != nil {
			return result, err
		}
		environment = map[string]string{"XDG_DATA_HOME": DisplayPath(xdg), "XDG_CONFIG_HOME": DisplayPath(xdg)}
	}
	child, err := process.Start(checkCtx, process.Spec{Program: target.program, Args: args, Directory: root, Environment: environment, Stdout: output, Stderr: output})
	if err != nil {
		return result, err
	}
	exit := child.Wait()
	if checkCtx.Err() != nil {
		return result, checkCtx.Err()
	}
	if exit.Err != nil {
		return result, exit.Err
	}
	result.OK = exit.Code == 0
	result.Messages = append(result.Messages, strings.TrimSpace(output.String()))
	if !result.OK {
		result.Issues = append(result.Issues, configIssue{Severity: "error", Message: "服务原生配置校验失败，原文件未修改"})
	}
	return result, nil
}

func (c *Core) writeConfig(ctx context.Context, target configTarget, content string, expected *string) error {
	return c.writeConfigExpected(ctx, target, content, expected, false)
}

func (c *Core) writeConfigExpected(ctx context.Context, target configTarget, content string, expected *string, requireAbsent bool) error {
	lockDir := filepath.Join(c.Home, "config-locks")
	if err := managedDirectory(c.Home, lockDir); err != nil {
		return err
	}
	unlock, err := process.Lock(filepath.Join(lockDir, hex.EncodeToString([]byte(target.key))+".lock"))
	if errors.Is(err, process.ErrLocked) {
		return &AppError{Code: "CONFIG_BUSY", Message: "其他实例正在修改此配置，请稍后重试"}
	}
	if err != nil {
		return err
	}
	defer unlock()
	if err := managedDirectory(c.Home, filepath.Dir(target.path)); err != nil {
		return err
	}
	before, err := readConfigText(target.path)
	exists := err == nil
	if err != nil && !os.IsNotExist(err) {
		return err
	}
	if expected != nil && (!exists || before != *expected) {
		return &AppError{Code: "CONFIG_CONFLICT", Message: "配置已被其他操作修改，未覆盖当前文件"}
	}
	if requireAbsent && exists {
		return &AppError{Code: "CONFIG_CONFLICT", Message: "配置文件已由其他操作创建，请重新预览"}
	}
	if exists && before == content {
		return nil
	}
	if exists {
		dir := filepath.Join(c.Home, "backup", "config")
		if err = managedDirectory(c.Home, dir); err != nil {
			return err
		}
		file, err := os.CreateTemp(dir, hex.EncodeToString([]byte(target.key))+"-"+time.Now().UTC().Format("20060102150405")+"-*.bak")
		if err != nil {
			return err
		}
		_, writeErr := file.WriteString(before)
		syncErr := file.Sync()
		closeErr := file.Close()
		if err = errors.Join(writeErr, syncErr, closeErr); err != nil {
			return err
		}
	}
	current, err := readConfigText(target.path)
	if err != nil && !os.IsNotExist(err) {
		return err
	}
	if (err == nil) != exists || current != before {
		return &AppError{Code: "CONFIG_CONFLICT", Message: "配置在保存前发生变化，未覆盖"}
	}
	if err = ctx.Err(); err != nil {
		return err
	}
	return atomicWrite(target.path, []byte(content))
}
func (c *Core) configBackups(selected string) ([]map[string]any, error) {
	dir := filepath.Join(c.Home, "backup", "config")
	entries, err := os.ReadDir(dir)
	out := []map[string]any{}
	if os.IsNotExist(err) {
		return out, nil
	}
	if err != nil {
		return nil, err
	}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".bak") {
			continue
		}
		prefix, _, ok := strings.Cut(entry.Name(), "-")
		if !ok {
			continue
		}
		decoded, err := hex.DecodeString(prefix)
		if err != nil {
			continue
		}
		key := string(decoded)
		if selected != "" && key != selected {
			continue
		}
		kind, version, exact := strings.Cut(key, "@")
		known := false
		for _, spec := range configSpecs {
			if spec.kind == kind && (spec.shared && !exact || !spec.shared && exact && safePart(version)) {
				known = true
				break
			}
		}
		if !known {
			continue
		}
		info, err := entry.Info()
		if err != nil {
			return nil, err
		}
		if !info.Mode().IsRegular() {
			continue
		}
		out = append(out, map[string]any{"name": entry.Name(), "path": Path(filepath.Join(dir, entry.Name())), "sizeBytes": info.Size(), "createdAt": info.ModTime().Unix(), "target": key})
	}
	sort.Slice(out, func(i, j int) bool { return out[i]["createdAt"].(int64) > out[j]["createdAt"].(int64) })
	if len(out) > 30 {
		out = out[:30]
	}
	return out, nil
}
