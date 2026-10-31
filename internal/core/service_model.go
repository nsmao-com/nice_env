package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"niceenv/internal/process"
)

type RunSpec struct {
	Args             []string          `json:"args"`
	Cwd              string            `json:"cwd"`
	Env              map[string]string `json:"env"`
	Health           string            `json:"health"`
	HealthTimeoutSec int               `json:"healthTimeoutSec"`
	ConfigFile       string            `json:"configFile"`
	ConfigTemplate   string            `json:"configTemplate"`
	StopArgs         []string          `json:"stopArgs"`
	InitArgs         []string          `json:"initArgs"`
	InitMarker       string            `json:"initMarker"`
	InitBin          string            `json:"initBin"`
	InitDirs         []string          `json:"initDirs"`
	SingleInstance   *bool             `json:"singleInstance"`
	DataDir          string            `json:"dataDir"`
	Requires         []string          `json:"requires"`
}
type Package struct {
	ID          string        `json:"id"`
	Version     string        `json:"version"`
	Category    string        `json:"category"`
	DisplayName string        `json:"displayName"`
	Entry       string        `json:"entry"`
	DefaultPort int           `json:"defaultPort"`
	Run         *RunSpec      `json:"run"`
	Active      bool          `json:"active"`
	Install     *Installation `json:"install"`
}
type Installation struct {
	Version     string `json:"version"`
	InstallPath string `json:"installPath"`
	ConfigPath  string `json:"configPath"`
	InstalledAt int64  `json:"installedAt"`
}
type ServiceStatus struct {
	ID              string    `json:"id"`
	Label           string    `json:"label"`
	State           string    `json:"state"`
	PIDs            []int     `json:"pids"`
	Port            int       `json:"port,omitempty"`
	Version         string    `json:"version,omitempty"`
	UptimeSec       int64     `json:"uptimeSec,omitempty"`
	LastError       *AppError `json:"lastError,omitempty"`
	LogFile         Path      `json:"logFile,omitempty"`
	Category        string    `json:"category,omitempty"`
	AutoStart       bool      `json:"autoStart"`
	Requires        []string  `json:"requires"`
	MissingRequires []string  `json:"missingRequires"`
}
type serviceEntry struct {
	status      ServiceStatus // guarded by serviceManager.mu
	pkg         Package
	child       *process.Child
	cancel      context.CancelFunc
	done        chan struct{}
	started     time.Time
	revision    string
	resolved    *resolvedService
	application *Site
}
type serviceManager struct {
	lifecycle sync.Mutex // operations use TryLock; status never acquires it
	mu        sync.RWMutex
	entries   map[string]*serviceEntry
	history   []ServiceHistory
	closing   bool
	wg        sync.WaitGroup
}
type ServiceHistory struct {
	TS        int64  `json:"ts"`
	ServiceID string `json:"serviceId"`
	Detail    string `json:"detail"`
}

var packagePart = regexp.MustCompile(`^[a-zA-Z0-9][a-zA-Z0-9._+-]{0,127}$`)

func safePart(value string) bool {
	return packagePart.MatchString(value) && !strings.HasSuffix(value, ".") && !strings.Contains(value, "..")
}

// Reject traversal instead of silently changing the meaning of a manifest path.
func relativePath(root, relative string) (string, error) {
	relative = strings.ReplaceAll(relative, `\`, "/")
	if relative == "" || strings.HasPrefix(relative, "/") || strings.ContainsAny(relative, ":\x00") {
		return "", invalid("套件相对路径无效")
	}
	for _, part := range strings.Split(relative, "/") {
		if part == ".." {
			return "", invalid("套件路径不能越过托管目录")
		}
	}
	return filepath.Join(root, filepath.FromSlash(relative)), nil
}

func (c *Core) initializeServices(ctx context.Context) error {
	c.services.entries = map[string]*serviceEntry{}
	c.services.history = []ServiceHistory{}
	return c.refreshServices(ctx)
}

// Only called while lifecycle operations are serialized. No refresh is done by
// list_service_status, so opening a page never walks files or all OS processes.
func (c *Core) refreshServices(ctx context.Context) error {
	views, err := c.packageViews(ctx)
	if err != nil {
		return err
	}
	data, err := json.Marshal(views)
	if err != nil {
		return err
	}
	var packages []Package
	if err = json.Unmarshal(data, &packages); err != nil {
		return err
	}
	auto := map[string]bool{}
	text, _, err := c.Store.setting(ctx, "autoStartServices")
	if err != nil {
		return err
	}
	var ids []string
	if text != "" && json.Unmarshal([]byte(text), &ids) == nil {
		for _, id := range ids {
			auto[id] = true
		}
	}
	installed := map[string]bool{}
	for _, p := range packages {
		if p.Install != nil {
			installed[p.ID] = true
		}
	}
	next := map[string]*serviceEntry{}
	for _, p := range packages {
		if p.Install == nil || p.Run == nil {
			continue
		}
		if !safePart(p.ID) || !safePart(p.Version) {
			return invalid("已安装套件标识或版本无效")
		}
		id := p.ID
		multi := isMySQLEngine(p.ID) || p.ID == "php" || p.Run.SingleInstance != nil && !*p.Run.SingleInstance
		if multi {
			id += "@" + p.Version
		} else if !p.Active {
			continue
		}
		port, portErr := c.servicePort(ctx, id, p)
		requires := append([]string{}, p.Run.Requires...)
		missing := []string{}
		for _, dep := range requires {
			if !installed[dep] {
				missing = append(missing, dep)
			}
		}
		status := ServiceStatus{ID: id, Label: p.DisplayName, State: "stopped", PIDs: []int{}, Port: port, Version: p.Version, Category: p.Category, AutoStart: auto[id], Requires: requires, MissingRequires: missing, LogFile: Path(filepath.Join(c.Home, "logs", strings.ReplaceAll(id, "@", "_"), "out.log"))}
		if portErr != nil {
			status.State = "error"
			status.LastError = publicError(portErr)
		}
		revision, err := newID("service")
		if err != nil {
			return err
		}
		next[id] = &serviceEntry{status: status, pkg: p, revision: revision}
	}
	sites, err := c.sites(ctx)
	if err != nil {
		return err
	}
	for _, site := range sites {
		if site.Runtime.Application == nil {
			continue
		}
		entry, err := c.siteApplicationEntry(site)
		if err != nil {
			return err
		}
		next[entry.status.ID] = entry
	}
	c.services.mu.Lock()
	defer c.services.mu.Unlock()
	for id, old := range c.services.entries {
		if old.child != nil || old.status.State == "starting" || old.status.State == "stopping" {
			next[id] = old // the running version and port are immutable
		} else if candidate := next[id]; candidate != nil && candidate.status.Version == old.status.Version {
			old.pkg = candidate.pkg
			old.application = candidate.application
			old.status.AutoStart = candidate.status.AutoStart
			old.status.Port = candidate.status.Port
			old.status.Requires = candidate.status.Requires
			old.status.MissingRequires = candidate.status.MissingRequires
			next[id] = old
		}
	}
	c.services.entries = next
	return nil
}

func publicError(err error) *AppError {
	var result AppError
	_ = json.Unmarshal(MarshalError(err), &result)
	return &result
}
func cloneStatus(e *serviceEntry) ServiceStatus {
	s := e.status
	s.PIDs = append([]int{}, s.PIDs...)
	s.Requires = append([]string{}, s.Requires...)
	s.MissingRequires = append([]string{}, s.MissingRequires...)
	if e.child != nil && !e.started.IsZero() {
		s.UptimeSec = max(0, int64(time.Since(e.started).Seconds()))
	}
	return s
}
func (c *Core) serviceStatuses() []ServiceStatus {
	c.services.mu.RLock()
	defer c.services.mu.RUnlock()
	result := make([]ServiceStatus, 0, len(c.services.entries))
	for _, e := range c.services.entries {
		result = append(result, cloneStatus(e))
	}
	sort.Slice(result, func(i, j int) bool { return result[i].ID < result[j].ID })
	return result
}
func (c *Core) serviceEntry(id, expected string) (*serviceEntry, error) {
	c.services.mu.RLock()
	defer c.services.mu.RUnlock()
	if c.services.closing {
		return nil, &AppError{Code: "SERVICE_SHUTDOWN", Message: "应用正在退出"}
	}
	e := c.services.entries[id]
	if e == nil {
		return nil, &AppError{Code: "UNKNOWN_SERVICE", Message: "服务未安装或已卸载，请刷新列表"}
	}
	if expected != "" && canonicalVersion(expected) != canonicalVersion(e.status.Version) {
		return nil, &AppError{Code: "SERVICE_TARGET_CHANGED", Message: "服务版本已变化，请刷新后重试"}
	}
	return e, nil
}
func (c *Core) servicePort(ctx context.Context, id string, p Package) (int, error) {
	value, found, err := c.servicePortOverride(ctx, id, p.ID)
	if err != nil {
		return 0, err
	}
	if found {
		port, err := strconv.Atoi(value)
		if err != nil || port < 1 || port > 65535 {
			return 0, &AppError{Code: "BAD_PORT", Message: "服务端口配置无效"}
		}
		return port, nil
	}
	var assigned int
	err = c.Store.db.QueryRowContext(ctx, "SELECT base_port FROM port_assign WHERE service_id=?", id).Scan(&assigned)
	if err == nil {
		if assigned < 1 || assigned > 65535 {
			return 0, invalid("保存的服务端口无效")
		}
		return assigned, nil
	}
	if !errors.Is(err, sql.ErrNoRows) {
		return 0, err
	}
	profile, _, err := c.Store.setting(ctx, "portProfile")
	if err != nil {
		return 0, err
	}
	standard := map[string]int{"nginx": 80, "apache": 8080, "mysql": 3306, "redis": 6379, "postgresql": 5432, "mongodb": 27017}
	safe := map[string]int{"nginx": 8080, "apache": 8180, "mysql": 23306, "redis": 26379, "postgresql": 25432, "mongodb": 28017}
	if profile == "safe" {
		if port, ok := safe[p.ID]; ok {
			return port, nil
		}
	} else if port, ok := standard[p.ID]; ok {
		return port, nil
	}
	if p.DefaultPort < 0 || p.DefaultPort > 65535 {
		return 0, invalid("清单端口超出范围")
	}
	return p.DefaultPort, nil
}

func (c *Core) servicePortOverride(ctx context.Context, id, pkg string) (string, bool, error) {
	key := map[string]string{"nginx": "http", "apache": "apacheHttp", "postgresql": "postgres"}[pkg]
	if key == "" {
		key = pkg
	}
	keys := []string{id}
	if key != id {
		keys = append(keys, key)
	}
	for _, name := range keys {
		value, found, err := c.Store.setting(ctx, "portOverride."+name)
		if err != nil || found {
			return value, found, err
		}
	}
	return "", false, nil
}

// Refuse symlink/reparse redirection for paths written by service preparation.
func managedDirectory(home, path string) error {
	rel, err := filepath.Rel(home, path)
	if err != nil || !filepath.IsLocal(rel) {
		return invalid("服务目录超出应用数据目录")
	}
	current := home
	for _, part := range strings.Split(rel, string(filepath.Separator)) {
		current = filepath.Join(current, part)
		info, err := os.Lstat(current)
		if os.IsNotExist(err) {
			if err = os.Mkdir(current, 0700); err != nil {
				return err
			}
			continue
		}
		if err != nil {
			return err
		}
		if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 {
			return invalid("服务目录包含链接或非目录文件")
		}
		resolved, err := filepath.EvalSymlinks(current)
		if err != nil {
			return err
		}
		inside, err := filepath.Rel(home, resolved)
		if err != nil || !filepath.IsLocal(inside) {
			return invalid("服务目录重定向到应用目录之外")
		}
	}
	return nil
}
