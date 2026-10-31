package core

import (
	"context"
	"encoding/json"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"runtime"
	"sort"
	"strconv"
	"strings"

	"gopkg.in/yaml.v3"
)

func sftpgoConfigFile(directory string) (string, error) {
	var found string
	for _, ext := range []string{"json", "yaml", "yml", "toml", "hcl", "ini", "properties"} {
		path := filepath.Join(directory, "sftpgo."+ext)
		info, err := os.Lstat(path)
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			return "", err
		}
		if !info.Mode().IsRegular() || info.Size() > 1024*1024 {
			return "", invalid("SFTPGo 配置必须是小于 1 MiB 的普通文件")
		}
		if found != "" {
			return "", &AppError{Code: "SFTPGO_CONFIG_AMBIGUOUS", Message: "同一目录有多个 SFTPGo 配置文件"}
		}
		found = path
	}
	return found, nil
}

func sftpgoEnvKey(key string) string {
	if runtime.GOOS == "windows" {
		return strings.ToUpper(key)
	}
	return key
}

// Parse the same assignment/interpolation forms as SFTPGo's gotenv loader,
// without mutating the application's environment or evaluating shell commands.
var sftpgoAssignment = regexp.MustCompile(`(?s)^\s*(?:export\s+)?([A-Za-z0-9_.]+)(?:\s*=\s*|:\s+)(.*)$`)
var sftpgoVariable = regexp.MustCompile(`(\\)?\$(?:\{([A-Z0-9_]+)\}|([A-Z0-9_]+))`)
var sftpgoUnescape = regexp.MustCompile(`\\([^$])`)

func sftpgoParseEnvironment(text string, inherited map[string]string) (map[string]string, error) {
	text = strings.ReplaceAll(strings.ReplaceAll(text, "\r\n", "\n"), "\r", "\n")
	parsed := map[string]string{}
	names := map[string]string{}
	lines := strings.Split(text, "\n")
	for i := 0; i < len(lines); i++ {
		line := strings.TrimSpace(lines[i])
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		if len(line) >= 65535 || strings.ContainsRune(line, 0) {
			return nil, invalid("SFTPGo 环境配置行无效")
		}
		match := sftpgoAssignment.FindStringSubmatch(line)
		if match == nil {
			return nil, invalid("SFTPGo 环境配置赋值无效")
		}
		name, value := match[1], strings.TrimSpace(match[2])
		key := sftpgoEnvKey(name)
		if previous, ok := names[key]; ok && previous != name {
			return nil, invalid("SFTPGo 环境配置存在大小写冲突")
		}
		names[key] = name
		single, double := false, false
		if len(value) > 0 && (value[0] == '\'' || value[0] == '"') {
			quote := value[0]
			single = quote == '\''
			double = quote == '"'
			end := -1
			for {
				for j := 1; j < len(value); j++ {
					if value[j] == quote && value[j-1] != '\\' {
						end = j
						break
					}
				}
				if end >= 0 {
					break
				}
				i++
				if i >= len(lines) || len(lines[i]) >= 65535 {
					return nil, invalid("SFTPGo 环境配置引号未闭合")
				}
				value += "\n" + lines[i]
			}
			tail := strings.TrimSpace(value[end+1:])
			if tail != "" && !strings.HasPrefix(tail, "#") {
				return nil, invalid("SFTPGo 环境配置引号后有无效内容")
			}
			value = value[1:end]
		} else {
			if at := strings.IndexByte(value, '#'); at >= 0 {
				value = value[:at]
			}
			value = strings.TrimSpace(value)
		}
		if double {
			value = strings.ReplaceAll(strings.ReplaceAll(value, `\n`, "\n"), `\r`, "\r")
			value = sftpgoUnescape.ReplaceAllString(value, "$1")
		}
		if !single {
			value = sftpgoVariable.ReplaceAllStringFunc(value, func(token string) string {
				parts := sftpgoVariable.FindStringSubmatch(token)
				if parts[1] != "" {
					return token[1:]
				}
				variable := parts[2]
				if variable == "" {
					variable = parts[3]
				}
				if result, ok := inherited[sftpgoEnvKey(variable)]; ok {
					return result
				}
				return parsed[variable]
			})
		}
		if strings.ContainsRune(value, 0) {
			return nil, invalid("SFTPGo 环境配置包含无效字符")
		}
		parsed[name] = value
	}
	result := map[string]string{}
	for key, value := range inherited {
		result[key] = value
	}
	for key, value := range parsed {
		folded := sftpgoEnvKey(key)
		if _, ok := result[folded]; !ok {
			result[folded] = value
		}
	}
	return result, nil
}

func (c *Core) sftpgoEnvironment(r *resolvedService) (map[string]string, error) {
	env := map[string]string{}
	for _, entry := range os.Environ() {
		key, value, ok := strings.Cut(entry, "=")
		if ok {
			env[sftpgoEnvKey(key)] = value
		}
	}
	seen := map[string]bool{}
	for key, value := range r.env {
		folded := sftpgoEnvKey(key)
		if seen[folded] {
			return nil, invalid("SFTPGo 运行环境变量存在大小写冲突")
		}
		seen[folded] = true
		env[folded] = value
	}
	dir := filepath.Join(r.etc, "env.d")
	entries, err := os.ReadDir(dir)
	if os.IsNotExist(err) {
		return env, nil
	}
	if err != nil {
		return nil, err
	}
	if _, err = mongoExternalPath(dir, false); err != nil {
		return nil, err
	}
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}
		path := filepath.Join(dir, entry.Name())
		info, err := os.Lstat(path)
		if err != nil {
			return nil, err
		}
		if !info.Mode().IsRegular() || info.Size() > 1024*1024 {
			return nil, invalid("SFTPGo 环境配置必须是小于 1 MiB 的普通文件")
		}
		text, err := readConfigText(path)
		if err != nil {
			return nil, err
		}
		env, err = sftpgoParseEnvironment(text, env)
		if err != nil {
			return nil, &AppError{Code: "SFTPGO_ENV_INVALID", Message: "SFTPGo 环境配置无法完整解析", Hint: DisplayPath(path)}
		}
	}
	return env, nil
}

func sftpgoValue(config map[string]any, parts ...string) any {
	var value any = config
	for _, part := range parts {
		switch current := value.(type) {
		case map[string]any:
			value = current[part]
		case []any:
			i, err := strconv.Atoi(part)
			if err != nil || i < 0 || i >= len(current) {
				return nil
			}
			value = current[i]
		default:
			return nil
		}
	}
	return value
}

func (c *Core) inspectSftpgo(ctx context.Context, e *serviceEntry, r *resolvedService, previouslyStarted, write bool) ([]string, error) {
	p := e.pkg
	if p.Run == nil || !reflect.DeepEqual(p.Run.Args, []string{"serve", "--config-dir", "{etc}", "--log-file-path", "{data}/sftpgo.log"}) || p.Run.SingleInstance != nil && !*p.Run.SingleInstance || p.Run.Env["SFTPGO_SFTPD__BINDINGS__0__PORT"] != "{port}" || p.Run.Env["SFTPGO_HTTPD__BINDINGS__0__PORT"] != "{port+6058}" {
		return nil, invalid("SFTPGo 运行描述不是受管 portable 配置")
	}
	if _, err := sftpgoRelativeDirectory(c.Home, r.etc); err != nil {
		return nil, err
	}
	if _, err := mongoExternalPath(r.etc, false); err != nil {
		return nil, err
	}
	existing, err := sftpgoConfigFile(r.etc)
	if err != nil {
		return nil, err
	}
	if previouslyStarted && existing == "" {
		return nil, &AppError{Code: "SFTPGO_CONFIG_MISSING", Message: "原 SFTPGo 配置文件已丢失，未创建空库"}
	}
	source := existing
	if source == "" {
		source, err = sftpgoConfigFile(filepath.Dir(r.program))
		if err != nil {
			return nil, err
		}
		if source == "" {
			return nil, &AppError{Code: "SFTPGO_CONFIG_MISSING", Message: "SFTPGo portable 包缺少默认配置"}
		}
	}
	text, err := readConfigText(source)
	if err != nil {
		return nil, err
	}
	config := map[string]any{}
	switch strings.ToLower(filepath.Ext(source)) {
	case ".json":
		err = json.Unmarshal([]byte(text), &config)
	case ".yaml", ".yml":
		err = yaml.Unmarshal([]byte(text), &config)
	default:
		return nil, invalid("托管 SFTPGo 配置支持 JSON 或 YAML")
	}
	if err != nil {
		return nil, invalid("SFTPGo 配置无法解析，原文件已保留")
	}
	env, err := c.sftpgoEnvironment(r)
	if err != nil {
		return nil, err
	}
	get := func(key, fallback string, parts ...string) (string, error) {
		if value, exists := env[key]; exists {
			return value, nil
		}
		v := sftpgoValue(config, parts...)
		if v == nil {
			return fallback, nil
		}
		value, ok := v.(string)
		if !ok {
			return "", invalid("SFTPGo 配置字段类型无效")
		}
		return value, nil
	}
	// Resolve bundled UI assets against the selected executable, so selecting an
	// old persistent config never accidentally selects an old static bundle.
	for _, setting := range []struct{ key, group, field, folder string }{{"SFTPGO_HTTPD__TEMPLATES_PATH", "httpd", "templates_path", "templates"}, {"SFTPGO_HTTPD__STATIC_FILES_PATH", "httpd", "static_files_path", "static"}, {"SFTPGO_HTTPD__OPENAPI_PATH", "httpd", "openapi_path", "openapi"}, {"SFTPGO_SMTP__TEMPLATES_PATH", "smtp", "templates_path", "templates"}} {
		value, err := get(setting.key, setting.folder, setting.group, setting.field)
		if err != nil {
			return nil, err
		}
		if _, exists := env[setting.key]; !exists && value == setting.folder {
			r.env[setting.key] = DisplayPath(filepath.Join(filepath.Dir(r.program), setting.folder))
		}
	}
	env, err = c.sftpgoEnvironment(r)
	if err != nil {
		return nil, err
	}
	driver, err := get("SFTPGO_DATA_PROVIDER__DRIVER", "sqlite", "data_provider", "driver")
	if err != nil {
		return nil, err
	}
	connection, err := get("SFTPGO_DATA_PROVIDER__CONNECTION_STRING", "", "data_provider", "connection_string")
	if err != nil {
		return nil, err
	}
	var database string
	if driver == "sqlite" || driver == "bolt" {
		if connection == "" || driver == "bolt" {
			name, err := get("SFTPGO_DATA_PROVIDER__NAME", "sftpgo.db", "data_provider", "name")
			if err != nil {
				return nil, err
			}
			if name == "" {
				return nil, invalid("SFTPGo 数据库文件名不能为空")
			}
			database = DisplayPath(name)
			if !filepath.IsAbs(database) {
				database = filepath.Join(r.etc, database)
			}
			if driver == "sqlite" {
				path := filepath.ToSlash(database)
				if !strings.HasPrefix(path, "/") {
					path = "/" + path
				}
				u := url.URL{Scheme: "file", Path: path}
				connection = u.String()
				r.env["SFTPGO_DATA_PROVIDER__CONNECTION_STRING"] = connection
			}
		} else if driver == "sqlite" {
			if strings.HasPrefix(connection, "file:") {
				u, err := url.Parse(connection)
				if err != nil {
					return nil, invalid("SFTPGo SQLite 连接字符串无效")
				}
				if u.Query().Get("mode") == "memory" {
					database = ""
				} else {
					database = u.Path
					if database == "" {
						database = u.Opaque
					}
					if u.Host != "" && u.Host != "localhost" {
						database = "//" + u.Host + database
					}
					if runtime.GOOS == "windows" && len(database) > 2 && database[0] == '/' && database[2] == ':' {
						database = database[1:]
					}
				}
			} else {
				database = strings.SplitN(connection, "?", 2)[0]
			}
			if database == ":memory:" {
				database = ""
			}
			if database != "" && !filepath.IsAbs(database) {
				database = filepath.Join(r.cwd, database)
			}
		}
	}
	state := []string{}
	require := func(path string) error {
		info, err := os.Stat(path)
		if err != nil || !info.Mode().IsRegular() || info.Size() == 0 {
			return &AppError{Code: "SFTPGO_STATE_MISSING", Message: "SFTPGo 原数据库或主机密钥缺失，未自动重新初始化", Hint: DisplayPath(path)}
		}
		relative, err := filepath.Rel(r.etc, path)
		if err != nil {
			relative = DisplayPath(path)
		}
		state = append(state, relative)
		return nil
	}
	if previouslyStarted {
		if database != "" {
			if err = require(database); err != nil {
				return nil, err
			}
		}
		keys := []string{}
		if value, ok := env["SFTPGO_SFTPD__HOST_KEYS"]; ok {
			if value != "" {
				keys = strings.Split(value, ",")
			}
		} else if raw := sftpgoValue(config, "sftpd", "host_keys"); raw != nil {
			values, ok := raw.([]any)
			if !ok {
				return nil, invalid("SFTPGo 主机密钥配置无效")
			}
			for _, v := range values {
				key, ok := v.(string)
				if !ok {
					return nil, invalid("SFTPGo 主机密钥路径无效")
				}
				keys = append(keys, key)
			}
		}
		if len(keys) == 0 {
			keys = []string{"id_rsa", "id_ecdsa", "id_ed25519"}
		}
		for _, key := range keys {
			path := DisplayPath(key)
			if !filepath.IsAbs(path) {
				path = filepath.Join(r.etc, path)
			}
			if err = require(path); err != nil {
				return nil, err
			}
		}
	}
	ports := map[int]bool{}
	for _, group := range []string{"sftpd", "httpd", "ftpd", "webdavd"} {
		n := 0
		if bindings, ok := sftpgoValue(config, group, "bindings").([]any); ok {
			n = len(bindings)
		}
		for key := range env {
			prefix := "SFTPGO_" + strings.ToUpper(group) + "__BINDINGS__"
			if strings.HasPrefix(key, prefix) {
				index, _, _ := strings.Cut(strings.TrimPrefix(key, prefix), "__")
				if i, err := strconv.Atoi(index); err == nil {
					n = max(n, i+1)
				}
			}
		}
		if n > 64 {
			return nil, invalid("SFTPGo 监听配置过多")
		}
		for i := 0; i < n; i++ {
			index := strconv.Itoa(i)
			key := "SFTPGO_" + strings.ToUpper(group) + "__BINDINGS__" + index + "__PORT"
			port := 0
			if text, ok := env[key]; ok {
				port, err = strconv.Atoi(text)
				if err != nil {
					return nil, invalid("SFTPGo 监听端口无效")
				}
			} else if raw := sftpgoValue(config, group, "bindings", index, "port"); raw != nil {
				switch value := raw.(type) {
				case int:
					port = value
				case float64:
					port = int(value)
					if float64(port) != value {
						return nil, invalid("SFTPGo 端口不是整数")
					}
				default:
					return nil, invalid("SFTPGo 监听端口类型无效")
				}
			}
			if port < 0 || port > 65535 {
				return nil, invalid("SFTPGo 监听端口超出范围")
			}
			if port > 0 {
				if ports[port] {
					return nil, invalid("SFTPGo 监听端口重复")
				}
				ports[port] = true
			}
		}
	}
	if !ports[r.port] {
		return nil, invalid("SFTPGo 主端口未启用")
	}
	r.sftpgoWebURL = ""
	admin, adminErr := get("SFTPGO_HTTPD__BINDINGS__0__ENABLE_WEB_ADMIN", "true", "httpd", "bindings", "0", "enable_web_admin")
	if adminErr != nil {
		if value, ok := sftpgoValue(config, "httpd", "bindings", "0", "enable_web_admin").(bool); ok {
			admin = strconv.FormatBool(value)
			adminErr = nil
		}
	}
	if adminErr != nil {
		return nil, adminErr
	}
	enabled, err := strconv.ParseBool(admin)
	if err != nil {
		return nil, invalid("SFTPGo 管理台开关无效")
	}
	if enabled {
		address, err := get("SFTPGO_HTTPD__BINDINGS__0__ADDRESS", "127.0.0.1", "httpd", "bindings", "0", "address")
		if err != nil {
			return nil, err
		}
		if address == "" || address == "0.0.0.0" {
			address = "127.0.0.1"
		}
		secure := false
		if text, ok := env["SFTPGO_HTTPD__BINDINGS__0__ENABLE_HTTPS"]; ok {
			secure, err = strconv.ParseBool(text)
			if err != nil {
				return nil, invalid("SFTPGo HTTPS 开关无效")
			}
		} else if raw := sftpgoValue(config, "httpd", "bindings", "0", "enable_https"); raw != nil {
			value, ok := raw.(bool)
			if !ok {
				return nil, invalid("SFTPGo HTTPS 开关类型无效")
			}
			secure = value
		}
		base, err := get("SFTPGO_HTTPD__WEB_ROOT", "", "httpd", "web_root")
		if err != nil {
			return nil, err
		}
		scheme := "http"
		if secure {
			scheme = "https"
		}
		if address == "127.0.0.1" {
			u := url.URL{Scheme: scheme, Host: fmt.Sprintf("127.0.0.1:%d", r.port+6058), Path: "/" + strings.Trim(base, "/") + "/web/admin"}
			u.Path = strings.ReplaceAll(u.Path, "//", "/")
			r.sftpgoWebURL = u.String()
		}
	}
	r.ports = nil
	for port := range ports {
		r.ports = append(r.ports, port)
	}
	sort.Ints(r.ports)
	if write && existing == "" {
		dest := filepath.Join(r.etc, filepath.Base(source))
		f, err := os.OpenFile(dest, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
		if err != nil {
			return nil, err
		}
		_, err = f.WriteString(text)
		closeErr := f.Close()
		if err != nil {
			return nil, err
		}
		if closeErr != nil {
			return nil, closeErr
		}
	}
	return state, ctx.Err()
}

func (c *Core) prepareSftpgo(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	_, exists, err := c.Store.setting(ctx, "sftpgoConfigDir")
	if err != nil {
		return err
	}
	if !exists {
		parent := filepath.Join(c.Home, "etc", "sftpgo")
		entries, err := os.ReadDir(parent)
		if err != nil {
			return err
		}
		candidates := []string{}
		for _, entry := range entries {
			if !entry.IsDir() {
				continue
			}
			path := filepath.Join(parent, entry.Name())
			files, err := os.ReadDir(path)
			if err != nil {
				return err
			}
			if len(files) > 0 {
				candidates = append(candidates, path)
			}
		}
		if len(candidates) > 1 {
			return &AppError{Code: "SFTPGO_CONFIG_AMBIGUOUS", Message: "发现多份 SFTPGo 配置和数据，请先选择要继续使用的配置目录"}
		}
		r.etc = filepath.Join(parent, "shared")
		if len(candidates) == 1 {
			r.etc = candidates[0]
		}
		if err = managedDirectory(c.Home, r.etc); err != nil {
			return err
		}
		_, err = sftpgoRelativeDirectory(c.Home, r.etc)
		if err != nil {
			return err
		}
	}
	if _, err = c.inspectSftpgo(ctx, e, r, exists, true); err != nil {
		return err
	}
	r.args = []string{"serve", "--config-dir", DisplayPath(r.etc), "--log-file-path", DisplayPath(filepath.Join(r.data, "sftpgo.log"))}
	return nil
}
