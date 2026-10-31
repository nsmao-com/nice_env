package core

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

var apacheModules = []string{"authz_core", "authz_host", "authn_core", "authn_file", "authz_user", "auth_basic", "log_config", "mime", "dir", "env", "headers", "setenvif", "rewrite", "proxy", "proxy_fcgi", "proxy_http", "ssl", "socache_shmcb"}

func (c *Core) apacheConfig(ctx context.Context, e *serviceEntry, r *resolvedService) (string, error) {
	return c.renderApacheConfig(ctx, e, r, false)
}

func (c *Core) renderApacheConfig(ctx context.Context, e *serviceEntry, r *resolvedService, reset bool) (string, error) {
	root := filepath.Dir(filepath.Dir(r.program))
	options, err := c.siteOptions(ctx, Site{Runtime: siteRuntime{WebServer: "apache"}}, "", false)
	if err != nil {
		return "", err
	}
	r.etc = filepath.Join(c.Home, "etc", "apache")
	for _, dir := range []string{"sites", "run", "logs", "htdocs"} {
		if err = managedDirectory(c.Home, filepath.Join(r.etc, dir)); err != nil {
			return "", err
		}
	}
	if err = managedDirectory(c.Home, filepath.Join(c.Home, "logs", "apache")); err != nil {
		return "", err
	}
	r.port = options.http
	r.ports = []int{options.http, options.https}
	address := "127.0.0.1"
	if options.network {
		address = "0.0.0.0"
	}
	managed := map[string]string{
		"serverroot":     "ServerRoot " + apachePath(root),
		"define nsb_etc": "Define NSB_ETC " + apachePath(r.etc),
		"pidfile":        "PidFile " + apachePath(filepath.Join(r.etc, "run", "httpd.pid")),
		"typesconfig":    "TypesConfig " + apachePath(filepath.Join(root, "conf", "mime.types")),
	}
	path := filepath.Join(r.etc, "httpd.conf")
	previous, err := readConfigText(path)
	exists := err == nil
	if err != nil && !os.IsNotExist(err) {
		return "", err
	}
	text := previous
	if !exists || reset {
		text = fmt.Sprintf("# NiceEnv Apache configuration\nServerName 127.0.0.1:%d\nErrorLog %s\nLogLevel warn\nDocumentRoot %s\n<Directory />\n    AllowOverride None\n    Require all denied\n</Directory>\nDirectoryIndex index.php index.html index.htm\n", options.http, apachePath(filepath.Join(c.Home, "logs", "apache", "error.log")), apachePath(filepath.Join(r.etc, "htdocs")))
	}
	seen := map[string]bool{}
	loaded := map[string]bool{}
	lines := strings.Split(text, "\n")
	out := []string{}
	depth := 0
	listenCount := 0
	for _, line := range lines {
		trim := strings.TrimSpace(line)
		fields := strings.Fields(trim)
		if strings.HasPrefix(trim, "</") {
			depth--
		}
		if depth < 0 {
			return "", invalid("Apache 配置块边界无效")
		}
		if depth == 0 && len(fields) > 0 && !strings.HasPrefix(trim, "#") {
			key := strings.ToLower(fields[0])
			if key == "define" && len(fields) > 1 {
				key += " " + strings.ToLower(fields[1])
			}
			if replacement, ok := managed[key]; ok {
				if seen[key] {
					return "", invalid("Apache 托管指令重复：" + key)
				}
				seen[key] = true
				line = replacement
			}
			if key == "loadmodule" && len(fields) > 1 {
				loaded[strings.TrimSuffix(fields[1], "_module")] = true
			}
			if key == "listen" && listenCount < 2 {
				if listenCount == 0 {
					line = fmt.Sprintf("Listen %s:%d", address, options.http)
				} else {
					line = fmt.Sprintf("Listen %s:%d https", address, options.https)
				}
				listenCount++
			}
		}
		out = append(out, line)
		if strings.HasPrefix(trim, "<") && !strings.HasPrefix(trim, "</") {
			depth++
		}
	}
	if depth != 0 {
		return "", invalid("Apache 配置块未闭合")
	}
	for _, key := range []string{"serverroot", "define nsb_etc", "pidfile", "typesconfig"} {
		if !seen[key] {
			out = append(out, managed[key])
		}
	}
	if listenCount == 0 {
		out = append(out, fmt.Sprintf("Listen %s:%d", address, options.http))
	}
	if listenCount < 2 {
		out = append(out, fmt.Sprintf("Listen %s:%d https", address, options.https))
	}
	moduleLines := []string{}
	for _, module := range apacheModules {
		if loaded[module] {
			continue
		}
		path := filepath.Join(root, "modules", "mod_"+module+".so")
		info, err := os.Stat(path)
		if err != nil || !info.Mode().IsRegular() {
			return "", invalid("Apache 缺少模块：" + module)
		}
		moduleLines = append(moduleLines, "LoadModule "+module+"_module "+apachePath(path))
	}
	// Modules must load before directives that depend on them.
	text = strings.Join(moduleLines, "\n") + "\n" + strings.Join(out, "\n")
	include := "IncludeOptional " + apachePath(filepath.Join(r.etc, "sites", "*.conf"))
	if !strings.Contains(text, include) {
		text += "\n" + include + "\n"
	}
	return text, nil
}

func (c *Core) prepareApache(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	text, err := c.apacheConfig(ctx, e, r)
	if err != nil {
		return err
	}
	path := filepath.Join(r.etc, "httpd.conf")
	before, err := readConfigText(path)
	var expected *string
	if err == nil {
		expected = &before
	} else if !os.IsNotExist(err) {
		return err
	}
	target := configTarget{spec: configSpecs[1], key: "apache-conf", path: path, program: r.program, version: e.pkg.Version}
	validation, err := c.validateConfig(ctx, target, text)
	if err != nil {
		return err
	}
	if !validation.OK {
		return &AppError{Code: "APACHE_CONFIG_INVALID", Message: "Apache 配置语法校验失败", Detail: strings.Join(validation.Messages, "\n")}
	}
	if err = c.writeConfigExpected(ctx, target, text, expected, expected == nil); err != nil {
		return err
	}
	r.cwd = filepath.Dir(filepath.Dir(r.program))
	r.args = []string{"-f", DisplayPath(path)}
	if runtime.GOOS != "windows" {
		r.args = append(r.args, "-DFOREGROUND")
	}
	r.stopArgs = nil
	r.siteConfigs, err = c.webSiteSnapshot("apache")
	return err
}

func (c *Core) webSiteSnapshot(server string) (map[string]string, error) {
	dir := filepath.Join(c.Home, "etc", server, "sites")
	files, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	out := map[string]string{}
	for _, file := range files {
		if file.IsDir() || !strings.HasSuffix(file.Name(), ".conf") {
			continue
		}
		text, err := readConfigText(filepath.Join(dir, file.Name()))
		if err != nil {
			return nil, err
		}
		out[strings.TrimSuffix(file.Name(), ".conf")] = text
	}
	return out, nil
}

func (c *Core) reloadApache(ctx context.Context) error {
	e, err := c.serviceEntry("apache", "")
	if err != nil {
		return err
	}
	c.services.mu.RLock()
	active := e.child != nil
	var resolved resolvedService
	if active {
		resolved = *e.resolved
	}
	c.services.mu.RUnlock()
	if !active {
		return nil
	}
	if err = c.prepareApache(ctx, e, &resolved); err != nil {
		return err
	}
	if err = signalApache(e.child, true); err != nil {
		return err
	}
	c.services.mu.Lock()
	e.resolved = &resolved
	c.services.mu.Unlock()
	return nil
}
