package core

import (
	"context"
	"encoding/json"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

func siteApplicationKey(site Site) string {
	value, _ := json.Marshal(struct {
		Kind, Root, Target string
		App                *siteApplication
	}{site.Runtime.Kind, string(site.RootDir), func() string {
		if site.Runtime.ProxyTarget != nil {
			return *site.Runtime.ProxyTarget
		}
		return ""
	}(), site.Runtime.Application})
	return string(value)
}
func siteApplicationPort(site Site) (*url.URL, int, error) {
	if site.Runtime.ProxyTarget == nil {
		return nil, 0, invalid("应用代理地址未设置")
	}
	u, err := siteHTTPURL(*site.Runtime.ProxyTarget, true)
	if err != nil {
		return nil, 0, err
	}
	port := 80
	if u.Port() != "" {
		port, err = strconv.Atoi(u.Port())
	}
	if err != nil || port < 1 || port > 65535 {
		return nil, 0, invalid("应用端口无效")
	}
	return u, port, nil
}
func (c *Core) siteApplicationEntry(site Site) (*serviceEntry, error) {
	if !siteIdentifier.MatchString(site.ID) || site.Runtime.Application == nil {
		return nil, invalid("站点未配置托管应用")
	}
	_, port, err := siteApplicationPort(site)
	if err != nil {
		return nil, err
	}
	revision, err := newID("service")
	if err != nil {
		return nil, err
	}
	id := "site-app:" + site.ID
	return &serviceEntry{application: &site, revision: revision, pkg: Package{ID: "site-application", Version: site.Runtime.Application.Version, Run: &RunSpec{Health: "tcp", HealthTimeoutSec: 30}}, status: ServiceStatus{ID: id, Label: site.Name + " · 应用", State: "stopped", PIDs: []int{}, Port: port, Version: site.Runtime.Application.Version, Category: "runtime", Requires: []string{}, MissingRequires: []string{}, LogFile: Path(filepath.Join(c.Home, "logs", "site-app-"+site.ID, "out.log"))}}, nil
}
func (c *Core) ensureSiteApplication(site Site) (*serviceEntry, error) {
	next, err := c.siteApplicationEntry(site)
	if err != nil {
		return nil, err
	}
	c.services.mu.Lock()
	defer c.services.mu.Unlock()
	old := c.services.entries[next.status.ID]
	if old != nil && old.child != nil {
		if old.application == nil || siteApplicationKey(*old.application) != siteApplicationKey(site) {
			return nil, &AppError{Code: "APP_RUNNING", Message: "应用正在运行，请先停止站点后修改运行时、入口、参数、目录或监听地址"}
		}
		return old, nil
	}
	c.services.entries[next.status.ID] = next
	return next, nil
}
func (c *Core) resolveSiteApplication(ctx context.Context, e *serviceEntry) (*resolvedService, error) {
	site := *e.application
	if err := c.validateSite(ctx, &site); err != nil {
		return nil, err
	}
	app := site.Runtime.Application
	packages, err := c.installedRuntimePackages(ctx)
	if err != nil {
		return nil, err
	}
	id := applicationRuntimeID(site.Runtime.Kind)
	var selected *Package
	for i := range packages {
		p := &packages[i]
		if p.ID == id && canonicalVersion(p.Version) == canonicalVersion(app.Version) {
			selected = p
			break
		}
	}
	if selected == nil {
		return nil, invalid("应用运行时尚未安装")
	}
	bin, err := runtimeDirectory(*selected)
	if err != nil {
		return nil, err
	}
	entry, err := runtimeCLI(*selected)
	if err != nil {
		return nil, err
	}
	program, err := relativePath(DisplayPath(selected.Install.InstallPath), entry)
	if err != nil {
		return nil, err
	}
	program, err = filepath.EvalSymlinks(program)
	if err != nil {
		return nil, err
	}
	cwd := string(site.RootDir)
	if app.CWD != nil && strings.TrimSpace(string(*app.CWD)) != "" {
		cwd = string(*app.CWD)
	}
	if !filepath.IsAbs(cwd) {
		return nil, invalid("应用工作目录必须为绝对路径")
	}
	cwd, err = filepath.EvalSymlinks(DisplayPath(cwd))
	if err != nil {
		return nil, err
	}
	u, port, err := siteApplicationPort(site)
	if err != nil {
		return nil, err
	}
	options, err := c.siteOptions(ctx, site, "", false)
	if err != nil {
		return nil, err
	}
	if port == options.http || port == options.https {
		return nil, invalid("应用监听端口不能与 Web 服务端口相同")
	}
	env := map[string]string{"PATH": DisplayPath(bin) + string(os.PathListSeparator) + os.Getenv("PATH"), "PORT": strconv.Itoa(port), "HOST": u.Hostname()}
	if site.Runtime.Kind == "python" {
		env["PYTHONUNBUFFERED"] = "1"
	}
	if site.Runtime.Kind == "java" {
		env["JAVA_HOME"] = DisplayPath(filepath.Dir(bin))
	}
	return &resolvedService{program: program, cwd: cwd, args: append([]string{}, app.Args...), data: filepath.Join(c.Home, "data", "site-app", site.ID), etc: filepath.Join(c.Home, "etc", "site-app", site.ID), log: string(e.status.LogFile), env: env, port: port, ports: []int{port}}, nil
}
