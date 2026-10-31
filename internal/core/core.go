package core

import (
	"context"
	"crypto/rand"
	"encoding/json"
	"sort"
	"sync"
	"sync/atomic"
)

type Handler func(context.Context, json.RawMessage) (any, error)
type EventSink func(string, any)

type Core struct {
	Home          string
	Version       string
	Store         *Store
	handlers      map[string]Handler
	emit          EventSink
	ready         atomic.Bool
	catalog       Catalog
	cron          cronManager
	services      serviceManager
	installer     installManager
	assistant     assistantManager
	closed        chan struct{}
	closeOnce     sync.Once
	closeErr      error
	requestsMu    sync.Mutex
	requests      sync.WaitGroup
	closing       bool
	snapshotKey   [32]byte
	siteChanges   sync.Mutex
	hostChanges   sync.Mutex
	certChanges   sync.Mutex
	ollama        ollamaManager
	versions      versionManager
	projectChecks sync.Mutex
	backupPlans   backupPlanManager
}

func Open(ctx context.Context, home, version string, emit EventSink) (*Core, error) {
	store, err := OpenStore(ctx, home)
	if err != nil {
		return nil, err
	}
	catalog, err := loadCatalog(home)
	if err != nil {
		store.Close()
		return nil, err
	}
	c := &Core{Home: home, Version: version, Store: store, handlers: map[string]Handler{}, emit: emit, closed: make(chan struct{})}
	c.catalog = catalog
	if _, err := rand.Read(c.snapshotKey[:]); err != nil {
		store.Close()
		return nil, err
	}
	if err := c.initializeStacks(ctx); err != nil {
		store.Close()
		return nil, err
	}
	c.registerSettings()
	c.registerRecords()
	c.registerFiles()
	c.registerSiteFiles()
	c.registerHosts()
	c.registerCertificates()
	c.registerCertificateHealth()
	c.registerCertMonitors()
	c.registerEnv()
	c.registerProjectRuntime()
	c.registerProjectChecks()
	c.registerPathEnv()
	c.registerTerminal()
	c.registerStacks()
	c.registerCatalog()
	c.registerVersions()
	c.registerInstaller()
	c.registerUninstaller()
	c.registerConfigs()
	c.registerToolMirrors()
	c.registerStats()
	c.registerCron()
	if err := c.recoverCron(ctx); err != nil {
		store.Close()
		return nil, err
	}
	if err := c.initializeServices(ctx); err != nil {
		store.Close()
		return nil, err
	}
	c.registerServices()
	c.registerSites()
	c.registerPHP()
	c.registerXdebug()
	c.registerBulk()
	c.registerMySQL()
	c.registerMySQLBackups()
	c.registerPostgres()
	c.registerPostgresBackups()
	c.registerBackupPlans()
	c.registerRedis()
	c.registerRedisBackups()
	c.registerMemcached()
	c.registerOllama()
	c.registerDiagnostics()
	c.registerPorts()
	c.registerAssistant()
	c.handle("get_app_version", func(context.Context, json.RawMessage) (any, error) { return c.Version, nil })
	c.handle("get_data_dir", func(context.Context, json.RawMessage) (any, error) { return Path(c.Home), nil })
	c.handle("frontend_ready", func(context.Context, json.RawMessage) (any, error) { c.ready.Store(true); return true, nil })
	c.handle("startup_stack_status", func(context.Context, json.RawMessage) (any, error) {
		// Autostart is disabled for the isolated migration executable, not reported as successful.
		return map[string]any{"phase": "disabled", "stackId": nil, "stackName": nil, "report": nil, "error": nil}, nil
	})
	c.handle("backend_capabilities", func(context.Context, json.RawMessage) (any, error) { return c.Capabilities(), nil })
	c.handle("process_recovery_status", func(context.Context, json.RawMessage) (any, error) {
		return map[string]any{"adopted": []any{}, "killed": []any{}, "unresolved": []string{}, "blockedServices": []string{}}, nil
	})
	c.handle("recover_processes", func(context.Context, json.RawMessage) (any, error) {
		return map[string]any{"adopted": []any{}, "killed": []any{}, "unresolved": []string{}, "blockedServices": []string{}}, nil
	})
	c.handle("get_autostart_status", func(ctx context.Context, _ json.RawMessage) (any, error) {
		value, exists, err := c.Store.setting(ctx, "startStackOnLaunch")
		if err != nil {
			return nil, err
		}
		return exists && value == "true", nil
	})
	c.handle("pending_data_dir_migration", func(context.Context, json.RawMessage) (any, error) { return nil, nil })
	c.handle("cancel_data_dir_migration", func(context.Context, json.RawMessage) (any, error) { return true, nil })
	return c, nil
}

func (c *Core) Close() error {
	c.closeOnce.Do(func() {
		c.requestsMu.Lock()
		c.closing = true
		close(c.closed)
		c.requestsMu.Unlock()
		c.cron.mu.Lock()
		c.cron.closing = true
		for _, run := range c.cron.runs {
			run.cancel()
		}
		c.cron.mu.Unlock()
		c.cron.wg.Wait()
		c.closeInstaller()
		c.closeOllama()
		c.requests.Wait()
		c.closeServices()
		c.closeErr = c.Store.Close()
	})
	return c.closeErr
}

func (c *Core) handle(name string, handler Handler) {
	if _, exists := c.handlers[name]; exists {
		panic("duplicate native command: " + name)
	}
	c.handlers[name] = handler
}

func (c *Core) Invoke(ctx context.Context, name string, args json.RawMessage) (any, error) {
	c.requestsMu.Lock()
	if c.closing {
		c.requestsMu.Unlock()
		return nil, &AppError{Code: "APP_SHUTDOWN", Message: "应用正在退出"}
	}
	c.requests.Add(1)
	c.requestsMu.Unlock()
	defer c.requests.Done()
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	handler, exists := c.handlers[name]
	if !exists {
		return nil, &AppError{Code: "COMMAND_NOT_PORTED", Message: "此功能尚未迁移到 Go 后端", Detail: name, Hint: "当前是独立开发版本，不可替代现用正式版。"}
	}
	if len(args) > 4*1024*1024 {
		return nil, invalid("请求参数过大")
	}
	if len(args) == 0 {
		args = json.RawMessage(`{}`)
	}
	return handler(ctx, args)
}

func (c *Core) Capabilities() map[string]any {
	names := make([]string, 0, len(c.handlers))
	for name := range c.handlers {
		names = append(names, name)
	}
	sort.Strings(names)
	return map[string]any{"backend": "go-wails-v3", "development": true, "commands": names, "version": c.Version}
}

func decode[T any](data json.RawMessage) (T, error) {
	var result T
	if err := json.Unmarshal(data, &result); err != nil {
		return result, invalid("请求参数格式无效")
	}
	return result, nil
}
