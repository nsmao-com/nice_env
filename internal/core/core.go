package core

import (
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"os"
	"sort"
	"sync"
	"sync/atomic"
)

type Handler func(context.Context, json.RawMessage) (any, error)
type EventSink func(string, any)

type Core struct {
	Home            string
	Version         string
	Store           *Store
	handlers        map[string]Handler
	emit            EventSink
	ready           atomic.Bool
	catalog         Catalog
	cron            cronManager
	services        serviceManager
	installer       installManager
	assistant       assistantManager
	closed          chan struct{}
	closeOnce       sync.Once
	closeErr        error
	requestsMu      sync.Mutex
	requests        sync.WaitGroup
	closing         bool
	snapshotKey     [32]byte
	siteChanges     sync.RWMutex
	hostChanges     sync.Mutex
	certChanges     sync.RWMutex
	phpConfig       sync.Mutex // serializes lazy PHP configuration initialization
	healthChecks    sync.Mutex
	ollama          ollamaManager
	versions        versionManager
	projectChecks   sync.Mutex
	backupPlans     backupPlanManager
	manifestChanges sync.Mutex
	updateDownload  sync.Mutex
	console         *consoleRuntime // protected by services.lifecycle
	recovery        recoveryManager
	startup         startupManager
	tunnels         tunnelManager
	dnsChanges      sync.Mutex
	control         controlManager
	certAuto        certAutomationManager
	dataDir         dataDirManager
}

func Open(ctx context.Context, home, version string, emit EventSink) (*Core, error) {
	if err := FinishDataDirectoryActivation(ctx, home); err != nil {
		return nil, err
	}
	unlock, err := acquireApplicationHome(ctx, home)
	if err != nil {
		return nil, err
	}
	opened := false
	defer func() {
		if !opened {
			_ = unlock()
		}
	}()
	store, err := openStore(ctx, home)
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
	c.registerSiteBackups()
	c.registerSiteBackupPlans()
	c.registerHosts()
	c.registerCertificates()
	c.registerCertificateDeployment()
	c.registerCertAutomations()
	if err := c.recoverCertAutomations(ctx); err != nil {
		store.Close()
		return nil, err
	}
	c.registerCertificateHealth()
	c.registerCertMonitors()
	c.registerEnv()
	c.registerProjectRuntime()
	c.registerProjectChecks()
	c.registerPathEnv()
	c.registerTerminal()
	c.registerStacks()
	c.registerCatalog()
	c.registerRemoteManifest()
	c.registerUpdates()
	c.registerVersions()
	c.registerInstaller()
	c.registerUninstaller()
	c.registerConfigs()
	c.registerConfigExport()
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
	c.registerRecovery()
	c.registerSftpgo()
	c.registerServiceWeb()
	c.registerWatchdog()
	c.registerSites()
	c.registerSiteNetwork()
	c.registerPHP()
	c.registerXdebug()
	c.registerBulk()
	c.registerMySQL()
	c.registerDatabaseConsole()
	c.registerMySQLBackups()
	c.registerPostgres()
	c.registerPostgresBackups()
	c.registerBackupPlans()
	c.registerRedis()
	c.registerRedisBackups()
	c.registerMemcached()
	c.registerMongo()
	c.registerOllama()
	c.registerDiagnostics()
	c.registerPorts()
	c.registerAssistant()
	c.handle("get_app_version", func(context.Context, json.RawMessage) (any, error) { return c.Version, nil })
	c.handle("get_data_dir", func(context.Context, json.RawMessage) (any, error) { return Path(c.Home), nil })
	c.handle("frontend_ready", func(context.Context, json.RawMessage) (any, error) {
		c.ready.Store(true)
		c.StartLaunch(context.Background())
		return true, nil
	})
	c.registerStartup()
	c.registerTray()
	c.registerTunnels()
	c.registerDNS()
	c.registerProxy()
	c.registerDatabaseImport()
	c.registerMCP()
	c.registerDataDirectory()
	if err = c.loadPendingDataDirectory(); err != nil {
		store.Close()
		return nil, err
	}
	c.handle("backend_capabilities", func(context.Context, json.RawMessage) (any, error) { return c.Capabilities(), nil })
	if err = c.startControl(); err != nil {
		store.Close()
		return nil, err
	}
	c.control.unlock = unlock
	opened = true
	return c, nil
}

func (c *Core) Close() error {
	c.closeOnce.Do(func() {
		c.requestsMu.Lock()
		c.closing = true
		close(c.closed)
		c.requestsMu.Unlock()
		if c.control.listener != nil {
			_ = c.control.listener.Close()
		}
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
		c.control.wg.Wait()
		tunnelErr := c.stopTunnels(context.Background())
		c.tunnels.wg.Wait()
		c.services.lifecycle.Lock()
		consoleErr := c.stopDatabaseConsole(context.Background())
		c.services.lifecycle.Unlock()
		c.closeServices()
		c.closeErr = errors.Join(tunnelErr, consoleErr, c.Store.Close())
		if c.control.unlock != nil {
			c.closeErr = errors.Join(c.closeErr, c.control.unlock())
		}
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
	if os.Getenv("NICEENV_RESTART_TOKEN") != "" && name != "get_settings" && name != "get_data_dir" && name != "get_app_version" && name != "backend_capabilities" {
		return nil, &AppError{Code: "APP_STARTING", Message: "正在确认新窗口已就绪，请稍后重试"}
	}
	c.requestsMu.Lock()
	if c.closing {
		c.requestsMu.Unlock()
		return nil, &AppError{Code: "APP_SHUTDOWN", Message: "应用正在退出"}
	}
	c.requests.Add(1)
	c.requestsMu.Unlock()
	defer c.requests.Done()
	if name != "migrate_data_dir" && name != "pending_data_dir_migration" && name != "cancel_data_dir_migration" && name != "backend_capabilities" {
		readingPending := name == "get_settings" || name == "get_data_dir" || name == "get_app_version" || name == "frontend_ready"
		locked := false
		if readingPending {
			locked = c.dataDir.activity.TryRLock()
		} else {
			locked = c.beginDataActivity()
		}
		if !locked {
			return nil, dataDirBusy()
		}
		defer c.dataDir.activity.RUnlock()
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	handler, exists := c.handlers[name]
	if !exists {
		return nil, &AppError{Code: "COMMAND_NOT_SUPPORTED", Message: "当前版本不支持此操作", Detail: name, Hint: "请确认界面与应用版本一致。"}
	}
	limit := 4 * 1024 * 1024
	if name == "import_config_text" {
		limit = 20 * 1024 * 1024
	}
	if len(args) > limit {
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
	return map[string]any{"backend": "go-wails-v3", "development": BuildChannel != "production", "commands": names, "version": c.Version}
}

func decode[T any](data json.RawMessage) (T, error) {
	var result T
	if err := json.Unmarshal(data, &result); err != nil {
		return result, invalid("请求参数格式无效")
	}
	return result, nil
}
