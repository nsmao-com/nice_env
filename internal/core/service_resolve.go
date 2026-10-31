package core

import (
	"context"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"strconv"
	"strings"
)

type resolvedService struct {
	program, cwd, data, etc, log string
	args, stopArgs               []string
	env                          map[string]string
	port, httpPort               int
	ports                        []int
	freshDatabase                bool
	siteConfigs                  map[string]string
}

var serviceToken = regexp.MustCompile(`\{(root|data|etc|log|bin|httpPort|port(?:[+-][0-9]+)?)\}`)

// Replacement scans the template once. Tokens appearing in user directory names
// remain literal; Windows display prefixes are removed only from path operands.
func (r *resolvedService) expand(template string) (string, error) {
	var failure error
	text := serviceToken.ReplaceAllStringFunc(template, func(token string) string {
		key := token[1 : len(token)-1]
		switch key {
		case "root":
			return DisplayPath(filepath.Dir(r.program))
		case "bin":
			return DisplayPath(r.program)
		case "data":
			return DisplayPath(r.data)
		case "etc":
			return DisplayPath(r.etc)
		case "log":
			return DisplayPath(r.log)
		case "httpPort":
			return strconv.Itoa(r.httpPort)
		}
		port := r.port
		if len(key) > 4 {
			delta, err := strconv.Atoi(key[4:])
			if err != nil {
				failure = invalid("派生端口无效")
				return ""
			}
			port += delta
		}
		if port < 1 || port > 65535 {
			failure = invalid("服务端口超出 1–65535 范围")
			return ""
		}
		return strconv.Itoa(port)
	})
	return text, failure
}
func (r *resolvedService) expandArgs(args []string) ([]string, error) {
	result := make([]string, 0, len(args))
	for _, arg := range args {
		value, err := r.expand(arg)
		if err != nil {
			return nil, err
		}
		result = append(result, value)
	}
	return result, nil
}

func (c *Core) resolveService(ctx context.Context, e *serviceEntry) (*resolvedService, error) {
	if e.application != nil {
		return c.resolveSiteApplication(ctx, e)
	}
	p := e.pkg
	if p.Install == nil || p.Run == nil {
		return nil, &AppError{Code: "NOT_A_SERVICE", Message: "该套件不是可管理服务"}
	}
	// These packages have dedicated initialization/configuration/stop protocols.
	// Never launch their manifest display arguments as a substitute for the adapter.
	switch p.ID {
	case "mongodb", "mihomo", "sftpgo", "consul", "minio", "qdrant", "coredns", "tomcat", "rabbitmq", "neo4j":
		return nil, &AppError{Code: "SERVICE_ADAPTER_PENDING", Message: "此服务的专用配置和停机流程尚在迁移", Detail: p.ID}
	}
	root := p.Install.InstallPath
	if !filepath.IsAbs(root) {
		return nil, invalid("安装目录必须是绝对路径")
	}
	program, err := relativePath(root, p.Entry)
	if err != nil {
		return nil, err
	}
	info, err := os.Stat(program)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, invalid("服务入口不是普通文件")
	}
	resolved, err := filepath.EvalSymlinks(program)
	if err != nil {
		return nil, err
	}
	resolvedRoot, err := filepath.EvalSymlinks(root)
	if err != nil {
		return nil, err
	}
	rel, err := filepath.Rel(resolvedRoot, resolved)
	if err != nil || !filepath.IsLocal(rel) {
		return nil, invalid("服务入口重定向到安装目录之外")
	}
	if runtime.GOOS == "windows" && strings.ToLower(filepath.Ext(program)) != ".exe" {
		return nil, &AppError{Code: "SERVICE_SCRIPT_PENDING", Message: "脚本服务的前台运行适配尚在迁移"}
	}
	r := &resolvedService{program: program, cwd: filepath.Dir(program), etc: filepath.Join(c.Home, "etc", p.ID, p.Version), log: string(e.status.LogFile), port: e.status.Port, env: map[string]string{}}
	dataDir := p.Run.DataDir
	if dataDir == "" {
		dataDir = p.ID
	}
	r.data, err = relativePath(filepath.Join(c.Home, "data"), dataDir)
	if err != nil {
		return nil, err
	}
	if p.ID == "php" || isMySQLEngine(p.ID) || p.ID == "postgresql" {
		r.data = filepath.Join(c.Home, "data", p.ID, p.Version)
	}
	profile, _, err := c.Store.setting(ctx, "portProfile")
	if err != nil {
		return nil, err
	}
	r.httpPort = 80
	if profile == "safe" {
		r.httpPort = 8080
	}
	if v, ok, err := c.servicePortOverride(ctx, "nginx", "nginx"); err != nil {
		return nil, err
	} else if ok {
		r.httpPort, err = strconv.Atoi(v)
		if err != nil || r.httpPort < 1 || r.httpPort > 65535 {
			return nil, invalid("Web 端口无效")
		}
	}
	if p.Run.Health != "" && p.Run.Health != "tcp" && p.Run.Health != "process" {
		return nil, invalid("不支持的服务健康检查类型")
	}
	if !isMySQLEngine(p.ID) && p.ID != "redis" && p.ID != "apache" && p.ID != "caddy" && p.ID != "postgresql" && (p.Run.ConfigTemplate != "" || len(p.Run.InitArgs) > 0) {
		return nil, &AppError{Code: "SERVICE_INITIALIZATION_PENDING", Message: "此服务的配置初始化尚在迁移"}
	}
	if r.args, err = r.expandArgs(p.Run.Args); err != nil {
		return nil, err
	}
	if p.ID == "memcached" {
		settings, err := c.memcachedSettings(ctx, p.Version)
		if err != nil {
			return nil, err
		}
		for flag, value := range map[string]int{"-m": settings.MemoryMB, "-c": settings.MaxConnections, "-t": settings.Threads} {
			r.args = serviceArg(r.args, flag, strconv.Itoa(value))
		}
		r.args = serviceArg(r.args, "-l", "127.0.0.1")
		r.args = serviceArg(r.args, "-U", "0")
		r.args = serviceArg(r.args, "-p", strconv.Itoa(r.port))
	}
	if r.stopArgs, err = r.expandArgs(p.Run.StopArgs); err != nil {
		return nil, err
	}
	for key, value := range p.Run.Env {
		r.env[key], err = r.expand(value)
		if err != nil {
			return nil, err
		}
	}
	if p.Run.Cwd != "" {
		r.cwd, err = r.expand(p.Run.Cwd)
		if err != nil {
			return nil, err
		}
	}
	if !filepath.IsAbs(r.cwd) {
		return nil, invalid("服务工作目录必须是绝对路径")
	}
	// Port reservation covers every explicitly declared derived port as well.
	ports := map[int]bool{}
	if r.port > 0 {
		ports[r.port] = true
	}
	templates := append(append([]string{}, p.Run.Args...), p.Run.ConfigTemplate)
	for _, value := range p.Run.Env {
		templates = append(templates, value)
	}
	for _, template := range templates {
		for _, token := range serviceToken.FindAllString(template, -1) {
			if strings.HasPrefix(token, "{port") {
				value, err := r.expand(token)
				if err != nil {
					return nil, err
				}
				port, _ := strconv.Atoi(value)
				ports[port] = true
			}
		}
	}
	for port := range ports {
		r.ports = append(r.ports, port)
	}
	return r, nil
}
func (c *Core) prepareServiceDirectories(e *serviceEntry, r *resolvedService) error {
	for _, dir := range []string{r.data, r.etc, filepath.Dir(r.log)} {
		if err := managedDirectory(c.Home, dir); err != nil {
			return err
		}
	}
	for _, subdir := range e.pkg.Run.InitDirs {
		dir, err := relativePath(r.data, subdir)
		if err != nil {
			return err
		}
		if err = managedDirectory(c.Home, dir); err != nil {
			return err
		}
	}
	info, err := os.Stat(r.cwd)
	if err != nil {
		return err
	}
	if !info.IsDir() {
		return invalid("服务工作目录不存在")
	}
	return nil
}
func checkServicePorts(ctx context.Context, ports []int) error {
	listeners := []net.Listener{}
	defer func() {
		for _, listener := range listeners {
			_ = listener.Close()
		}
	}()
	for _, port := range ports {
		if err := ctx.Err(); err != nil {
			return err
		}
		listener, err := net.Listen("tcp4", net.JoinHostPort("127.0.0.1", strconv.Itoa(port)))
		if err != nil {
			return &AppError{Code: "PORT_IN_USE", Message: fmt.Sprintf("端口 %d 已被占用或无法绑定", port), Hint: "请调整端口或在确认归属后停止占用程序。"}
		}
		listeners = append(listeners, listener)
	}
	return nil
}
