package core

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	gnet "github.com/shirou/gopsutil/v4/net"
	"niceenv/internal/process"
)

type consoleStatus struct {
	Revision          string  `json:"revision"`
	PackageID         string  `json:"packageId"`
	DatabaseServiceID *string `json:"databaseServiceId"`
	Port              int     `json:"port"`
	File              string  `json:"file"`
	URL               string  `json:"url"`
	PHPVersion        string  `json:"phpVersion"`
	AdminerVersion    string  `json:"adminerVersion"`
}
type consoleRuntime struct {
	child  *process.Child
	cancel context.CancelFunc
	done   chan struct{}
	status consoleStatus
}

func (c *Core) databaseConsoleStatus() *consoleStatus {
	if c.console == nil {
		return nil
	}
	select {
	case <-c.console.done:
		return nil
	default:
		status := c.console.status
		return &status
	}
}
func (c *Core) stopDatabaseConsole(ctx context.Context) error {
	if c.console == nil {
		return nil
	}
	run := c.console
	select {
	case <-run.done:
		c.console = nil
		return nil
	default:
	}
	if err := run.child.Stop(); err != nil {
		return err
	}
	run.cancel()
	select {
	case <-run.done:
		c.console = nil
		return nil
	case <-ctx.Done():
		return ctx.Err()
	case <-time.After(10 * time.Second):
		return &AppError{Code: "CONSOLE_STOP_PENDING", Message: "管理台进程仍在清理，请稍后重试"}
	}
}
func (c *Core) registerDatabaseConsole() {
	c.handle("adminer_status", func(ctx context.Context, _ json.RawMessage) (any, error) {
		if !c.services.lifecycle.TryRLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.RUnlock()
		return c.databaseConsoleStatus(), nil
	})
	c.handle("adminer_stop", func(ctx context.Context, _ json.RawMessage) (any, error) {
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		err := c.stopDatabaseConsole(ctx)
		return err == nil, err
	})
	c.handle("adminer_start", c.startDatabaseConsole)
}
func (c *Core) startDatabaseConsole(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Package string `json:"package"`
		Target  string `json:"targetServiceId"`
	}](data)
	if err != nil {
		return nil, err
	}
	if in.Package == "" {
		in.Package = "adminer"
	}
	if in.Package != "adminer" && in.Package != "phpmyadmin" {
		return nil, invalid("请选择 Adminer 或 phpMyAdmin")
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	c.services.mu.RLock()
	closing := c.services.closing
	c.services.mu.RUnlock()
	if closing {
		return nil, &AppError{Code: "APP_SHUTDOWN", Message: "应用正在退出"}
	}
	if status := c.databaseConsoleStatus(); status != nil {
		target := ""
		if status.DatabaseServiceID != nil {
			target = *status.DatabaseServiceID
		}
		if status.PackageID != in.Package || target != in.Target {
			return nil, &AppError{Code: "CONSOLE_RUNNING", Message: "管理台已绑定另一工具或实例，请先停止后再切换"}
		}
		return status, nil
	}
	// Lock before preparing configuration: another instance may be serving this
	// installation, even when this Core has no console process of its own.
	lockDir := filepath.Join(c.Home, "service-locks")
	if err = managedDirectory(c.Home, lockDir); err != nil {
		return nil, err
	}
	unlock, err := process.Lock(filepath.Join(lockDir, "database-console.lock"))
	if err != nil {
		return nil, err
	}
	transferred := false
	defer func() {
		if !transferred {
			unlock()
		}
	}()
	port := 8991
	if err = checkServicePorts(ctx, []int{port}); err != nil {
		return nil, err
	}
	servers := []ServiceStatus{}
	for _, s := range c.serviceStatuses() {
		if (strings.HasPrefix(s.ID, "mysql@") || strings.HasPrefix(s.ID, "mariadb@")) && s.State == "running" && len(s.PIDs) > 0 && s.Port > 0 && (in.Target == "" || in.Target == s.ID) {
			servers = append(servers, s)
		}
	}
	if in.Target != "" && len(servers) == 0 {
		return nil, &AppError{Code: "DATABASE_INSTANCE_NOT_RUNNING", Message: "所选数据库实例未运行，请先启动后再打开管理台"}
	}
	packages, err := c.installedRuntimePackages(ctx)
	if err != nil {
		return nil, err
	}
	var php, console Package
	for _, p := range packages {
		if p.Active && p.ID == "php" {
			php = p
		}
		if p.Active && p.ID == in.Package {
			console = p
		}
	}
	if php.Install == nil || console.Install == nil {
		return nil, &AppError{Code: "PACKAGE_NOT_INSTALLED", Message: "请先安装 PHP 和所选数据库管理台"}
	}
	e, err := c.serviceEntry("php@"+php.Version, php.Version)
	if err != nil {
		return nil, err
	}
	r, err := c.resolveService(ctx, e)
	if err != nil {
		return nil, err
	}
	if err = c.ensurePHPIni(ctx, e, r); err != nil {
		return nil, err
	}
	program, err := phpCLIForEntry(e)
	if err != nil {
		return nil, err
	}
	entry, err := relativePath(console.Install.InstallPath, console.Entry)
	if err != nil {
		return nil, err
	}
	root, err := filepath.EvalSymlinks(console.Install.InstallPath)
	if err != nil {
		return nil, err
	}
	entry, err = filepath.EvalSymlinks(entry)
	if err != nil {
		return nil, err
	}
	rel, err := filepath.Rel(root, entry)
	if err != nil || !filepath.IsLocal(rel) || strings.ToLower(filepath.Ext(entry)) != ".php" {
		return nil, invalid("管理台入口必须是安装目录内的 PHP 文件")
	}
	info, err := os.Stat(entry)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() {
		return nil, invalid("管理台入口不是普通文件")
	}
	dir := filepath.Dir(entry)
	ini := filepath.Join(c.Home, "etc", "php", php.Version, "php.ini")
	if in.Package == "phpmyadmin" {
		if !strings.HasPrefix(php.Version, "7.2.") && !strings.HasPrefix(php.Version, "7.3.") && !strings.HasPrefix(php.Version, "7.4.") && !strings.HasPrefix(php.Version, "8.0.") && !strings.HasPrefix(php.Version, "8.1.") && !strings.HasPrefix(php.Version, "8.2.") && !strings.HasPrefix(php.Version, "8.3.") {
			return nil, invalid("phpMyAdmin 5.2 需要 PHP 7.2–8.3")
		}
		view, err := c.phpExtensionView(ctx, php.Version)
		if err != nil {
			return nil, err
		}
		loaded := map[string]bool{}
		for _, extension := range view["extensions"].([]phpExtension) {
			loaded[extension.Name] = extension.Loaded
		}
		if !loaded["mysqli"] || !loaded["mbstring"] {
			return nil, invalid("请先启用 PHP 的 mysqli 和 mbstring 扩展")
		}
		config := filepath.Join(dir, "config.inc.php")
		old, err := readConfigText(config)
		if err != nil && !os.IsNotExist(err) {
			return nil, err
		}
		const marker = "<?php\n// NiceEnv managed database console\n"
		managed := os.IsNotExist(err) || strings.HasPrefix(old, marker)
		if !managed && in.Target != "" {
			return nil, invalid("phpMyAdmin 使用自定义配置，无法安全绑定所选实例，请先备份配置或改用 Adminer")
		}
		if managed {
			if len(servers) == 0 {
				return nil, invalid("请先启动 MySQL 或 MariaDB")
			}
			secret, _, err := c.Store.setting(ctx, "phpMyAdminCookieSecret")
			if err != nil {
				return nil, err
			}
			secretBytes, secretErr := hex.DecodeString(secret)
			if secretErr != nil || len(secretBytes) != 16 {
				bytes := make([]byte, 16)
				if _, err = rand.Read(bytes); err != nil {
					return nil, err
				}
				secret = hex.EncodeToString(bytes)
				if err = c.Store.saveSetting(ctx, "phpMyAdminCookieSecret", secret); err != nil {
					return nil, err
				}
			}
			content := marker + "$cfg['blowfish_secret'] = '" + secret + "';\n$cfg['AllowArbitraryServer'] = false;\n"
			for i, s := range servers {
				content += fmt.Sprintf("$cfg['Servers'][%d] = ['auth_type'=>'cookie','host'=>'127.0.0.1','port'=>'%d','verbose'=>'127.0.0.1:%d','AllowNoPassword'=>false];\n", i+1, s.Port, s.Port)
			}
			if old != "" && old != content {
				backup := filepath.Join(c.Home, "backup", "console")
				if err = managedDirectory(c.Home, backup); err != nil {
					return nil, err
				}
				id, err := newID("phpmyadmin")
				if err != nil {
					return nil, err
				}
				if err = atomicWrite(filepath.Join(backup, id+".bak"), []byte(old)); err != nil {
					return nil, err
				}
			}
			if err = atomicWrite(config, []byte(content)); err != nil {
				return nil, err
			}
		}
	}
	logDir := filepath.Join(c.Home, "logs", in.Package)
	if err = managedDirectory(c.Home, logDir); err != nil {
		return nil, err
	}
	log, err := os.OpenFile(filepath.Join(logDir, "out.log"), os.O_CREATE|os.O_TRUNC|os.O_WRONLY, 0600)
	if err != nil {
		return nil, err
	}
	life, cancel := context.WithCancel(context.Background())
	child, err := process.Start(life, process.Spec{Program: program, Args: []string{"-c", DisplayPath(ini), "-S", net.JoinHostPort("127.0.0.1", strconv.Itoa(port)), "-t", DisplayPath(dir)}, Directory: dir, Environment: map[string]string{"PHPRC": DisplayPath(ini), "PHP_INI_SCAN_DIR": ""}, RemoveEnvironment: []string{"PHP_CLI_SERVER_WORKERS"}, Stdout: log, Stderr: log})
	if err != nil {
		cancel()
		log.Close()
		return nil, err
	}
	transferred = true
	u := url.URL{Scheme: "http", Host: net.JoinHostPort("127.0.0.1", strconv.Itoa(port)), Path: "/" + filepath.Base(entry)}
	var target *string
	if in.Target != "" {
		target = &in.Target
		q := u.Query()
		if in.Package == "phpmyadmin" {
			q.Set("server", "1") // phpMyAdmin selects a configured server by index.
		} else {
			q.Set("server", net.JoinHostPort("127.0.0.1", strconv.Itoa(servers[0].Port)))
		}
		u.RawQuery = q.Encode()
	}
	id, err := newID("console")
	if err != nil {
		child.Stop()
		cancel()
		child.Wait()
		log.Close()
		unlock()
		return nil, err
	}
	run := &consoleRuntime{child: child, cancel: cancel, done: make(chan struct{}), status: consoleStatus{Revision: id, PackageID: in.Package, DatabaseServiceID: target, Port: port, File: filepath.Base(entry), URL: u.String(), PHPVersion: php.Version, AdminerVersion: console.Version}}
	c.console = run
	c.services.wg.Add(1)
	go func() {
		defer c.services.wg.Done()
		child.Wait()
		cancel()
		log.Close()
		unlock()
		close(run.done)
	}()
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.Proxy = nil
	defer transport.CloseIdleConnections()
	client := http.Client{Transport: transport, Timeout: time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	readyCtx, readyCancel := context.WithTimeout(ctx, 8*time.Second)
	defer readyCancel()
	ticker := time.NewTicker(150 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-readyCtx.Done():
			cleanup := c.stopDatabaseConsole(context.Background())
			return nil, errors.Join(&AppError{Code: "ADMINER_START_FAILED", Message: "数据库管理台页面未就绪，请检查 PHP 配置和日志"}, cleanup)
		case <-run.done:
			c.console = nil
			return nil, &AppError{Code: "ADMINER_START_FAILED", Message: "数据库管理台进程提前退出，请查看日志"}
		case <-ticker.C:
		}
		req, err := http.NewRequestWithContext(readyCtx, "GET", run.status.URL, nil)
		if err != nil {
			continue
		}
		resp, err := client.Do(req)
		if err != nil {
			continue
		}
		body, readErr := io.ReadAll(io.LimitReader(resp.Body, 1024*1024))
		resp.Body.Close()
		if readErr != nil || resp.StatusCode != 200 || !strings.Contains(strings.ToLower(string(body)), in.Package) || !strings.Contains(string(body), "<form") {
			continue
		}
		listeners, err := gnet.ConnectionsWithContext(readyCtx, "tcp4")
		if err != nil {
			continue
		}
		for _, listener := range listeners {
			if listener.Status == "LISTEN" && int(listener.Laddr.Port) == port {
				owned, e := child.OwnsPID(int(listener.Pid))
				if e == nil && owned {
					return run.status, nil
				}
			}
		}
	}
}
