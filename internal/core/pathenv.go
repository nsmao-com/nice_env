package core

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"

	"github.com/Masterminds/semver/v3"
	"niceenv/internal/process"
	"niceenv/internal/systempath"
)

type pathSelection struct {
	Enabled  bool
	Selected *[]string
	Versions map[string]string
	Applied  []string
}
type pathEntry struct {
	ID       string   `json:"id"`
	Label    string   `json:"label"`
	Version  string   `json:"version"`
	BinDir   Path     `json:"binDir"`
	Exists   bool     `json:"exists"`
	Selected bool     `json:"selected"`
	InPath   bool     `json:"inPath"`
	Commands []string `json:"commands"`
}
type pathStatus struct {
	Enabled     bool        `json:"enabled"`
	ManagedDirs []Path      `json:"managedDirs"`
	Entries     []pathEntry `json:"entries"`
	Note        string      `json:"note"`
	Drift       bool        `json:"drift"`
}

func (c *Core) pathSelection(ctx context.Context) (pathSelection, error) {
	s := pathSelection{Versions: map[string]string{}, Applied: []string{}}
	enabled, _, err := c.Store.setting(ctx, "pathEnvEnabled")
	if err != nil {
		return s, err
	}
	switch enabled {
	case "", "0", "false":
	case "1", "true":
		s.Enabled = true
	default:
		return s, invalid("保存的 PATH 开关状态无效")
	}
	for key, target := range map[string]any{"pathEnvSelected": &s.Selected, "pathEnvVersions": &s.Versions, "pathEnvDirs": &s.Applied} {
		raw, found, err := c.Store.setting(ctx, key)
		if err != nil {
			return s, err
		}
		if found {
			if err = json.Unmarshal([]byte(raw), target); err != nil {
				return s, invalid("保存的 PATH 选择格式无效：" + key)
			}
		}
	}
	if s.Versions == nil {
		s.Versions = map[string]string{}
	}
	if s.Applied == nil {
		s.Applied = []string{}
	}
	return s, nil
}
func pathWants(selection pathSelection, id string) bool {
	if selection.Selected == nil {
		return true
	}
	for _, value := range *selection.Selected {
		if value == id {
			return true
		}
	}
	return false
}
func (c *Core) pathChosen(ctx context.Context, selection pathSelection, packages []Package) (map[string]Package, error) {
	chosen := map[string]Package{}
	active := map[string]string{}
	for _, p := range packages {
		version, explicit := selection.Versions[p.ID]
		if explicit {
			if canonicalVersion(version) == canonicalVersion(p.Version) {
				chosen[p.ID] = p
			}
			continue
		}
		if _, ok := active[p.ID]; !ok {
			value, _, err := c.Store.setting(ctx, "active"+p.ID+"Version")
			if err != nil {
				return nil, err
			}
			active[p.ID] = value
		}
		current, found := chosen[p.ID]
		preferred := active[p.ID] != "" && canonicalVersion(active[p.ID]) == canonicalVersion(p.Version)
		currentPreferred := active[p.ID] != "" && canonicalVersion(active[p.ID]) == canonicalVersion(current.Version)
		if !found || preferred || !currentPreferred && newerVersion(p.Version, current.Version) {
			chosen[p.ID] = p
		}
	}
	return chosen, nil
}
func commandPriority(id string) int {
	if id == "npm" || id == "pnpm" {
		return 0
	}
	return 1
}
func validateJSCLI(selected map[string]Package) error {
	for _, id := range []string{"npm", "pnpm"} {
		p, ok := selected[id]
		if !ok {
			continue
		}
		node, ok := selected["node"]
		if !ok {
			return &AppError{Code: "JS_CLI_NODE_REQUIRED", Message: id + " 需要 Node.js，请先选择 Node.js 版本"}
		}
		root, err := os.OpenRoot(p.Install.InstallPath)
		if err != nil {
			return err
		}
		data, exists, err := envRead(root, filepath.Join("package", "package.json"))
		root.Close()
		if err != nil {
			return err
		}
		if !exists {
			return invalid("包管理器缺少 package.json，请重新安装")
		}
		var meta struct {
			Name    string `json:"name"`
			Engines struct {
				Node string `json:"node"`
			} `json:"engines"`
		}
		if json.Unmarshal(data, &meta) != nil {
			return invalid("包管理器 package.json 格式无效")
		}
		rangeText := meta.Engines.Node
		if rangeText == "" && id == "pnpm" && strings.HasPrefix(meta.Name, "@pnpm/exe.") {
			root, err := os.OpenRoot(p.Install.InstallPath)
			if err != nil {
				return err
			}
			data, exists, err := envRead(root, ".niceenv-package.json")
			root.Close()
			if err != nil {
				return err
			}
			if exists {
				var snapshot struct {
					Note string `json:"note"`
				}
				if json.Unmarshal(data, &snapshot) == nil && strings.HasPrefix(snapshot.Note, "Node.js ") {
					rangeText = strings.TrimPrefix(snapshot.Note, "Node.js ")
				}
			}
		}
		if rangeText == "" {
			return invalid("包管理器缺少 Node.js 兼容性声明，请重新安装")
		}
		requirement, err := semver.NewConstraint(rangeText)
		if err != nil {
			return invalid("无法解析包管理器的 Node.js 版本要求")
		}
		version, err := semver.StrictNewVersion(canonicalVersion(node.Version))
		if err != nil || !requirement.Check(version) {
			return &AppError{Code: "JS_CLI_NODE_INCOMPATIBLE", Message: id + " " + p.Version + " 要求 Node.js " + rangeText + "，当前选择为 " + node.Version}
		}
	}
	return nil
}
func (c *Core) desiredPath(ctx context.Context, selection pathSelection, packages []Package, strict bool) ([]string, error) {
	if !selection.Enabled {
		return []string{}, nil
	}
	chosen, err := c.pathChosen(ctx, selection, packages)
	if err != nil {
		return nil, err
	}
	selected := map[string]Package{}
	ids := []string{}
	for id, p := range chosen {
		if !pathWants(selection, id) {
			continue
		}
		if _, err := runtimeCLI(p); err != nil {
			continue
		}
		if _, err := runtimeDirectory(p); err != nil {
			if !strict {
				continue
			}
			return nil, &AppError{Code: "PATH_VERSION_UNAVAILABLE", Message: id + " " + p.Version + " 入口不可用，未修改 PATH", Detail: publicError(err).Message}
		}
		selected[id] = p
		ids = append(ids, id)
	}
	if err = validateJSCLI(selected); strict && err != nil {
		return nil, err
	}
	sort.Slice(ids, func(i, j int) bool {
		a, b := commandPriority(ids[i]), commandPriority(ids[j])
		if a != b {
			return a < b
		}
		return ids[i] < ids[j]
	})
	dirs := []string{}
	for _, id := range ids {
		dir, err := runtimeDirectory(selected[id])
		if err != nil {
			return nil, err
		}
		if !systempath.Contains(dirs, dir) {
			dirs = append(dirs, dir)
		}
	}
	return dirs, nil
}
func (c *Core) systemPathSnapshots() ([]systempath.Snapshot, error) {
	if os.Getenv("NICEENV_WAILS_PATH_FIXTURE") == "1" {
		marker, err := os.ReadFile(filepath.Join(c.Home, ".niceenv-wails-development"))
		if err != nil || string(marker) != "NiceEnv Wails development data\n" {
			return nil, invalid("PATH 隔离目录标记无效")
		}
		dir := filepath.Join(c.Home, "etc")
		if err = managedDirectory(c.Home, dir); err != nil {
			return nil, err
		}
		return systempath.ReadFixture(filepath.Join(dir, "user-path.fixture"))
	}
	return systempath.Read()
}
func (c *Core) pathStatus(ctx context.Context, s pathSelection, packages []Package, snapshots []systempath.Snapshot) (pathStatus, error) {
	status := pathStatus{Enabled: s.Enabled, ManagedDirs: []Path{}, Entries: []pathEntry{}, Note: "修改当前用户的 PATH；已打开的终端不会自动更新，请新开终端。"}
	for _, dir := range s.Applied {
		status.ManagedDirs = append(status.ManagedDirs, Path(dir))
	}
	chosen, err := c.pathChosen(ctx, s, packages)
	if err != nil {
		return status, err
	}
	for _, p := range packages {
		entry, err := runtimeCLI(p)
		if err != nil {
			continue
		}
		dir, err := runtimeDirectory(p)
		exists := err == nil
		if !exists {
			dir = DisplayPath(filepath.Join(p.Install.InstallPath, filepath.Dir(entry)))
		}
		inPath := true
		for _, snapshot := range snapshots {
			inPath = inPath && systempath.Contains(systempath.Entries(snapshot), dir)
		}
		commands := []string{}
		if exists {
			files, err := os.ReadDir(dir)
			if err == nil {
				for _, f := range files {
					if f.IsDir() {
						continue
					}
					name := f.Name()
					ext := strings.ToLower(filepath.Ext(name))
					if ext == ".exe" || ext == ".cmd" || ext == ".bat" {
						commands = append(commands, strings.TrimSuffix(name, filepath.Ext(name)))
					} else if info, err := f.Info(); err == nil && info.Mode().Perm()&0111 != 0 {
						commands = append(commands, name)
					}
					if len(commands) >= 100 {
						break
					}
				}
			}
		}
		status.Entries = append(status.Entries, pathEntry{p.ID, runtimeLabel(p), p.Version, Path(dir), exists, pathWants(s, p.ID) && chosen[p.ID].Version == p.Version, inPath, commands})
	}
	desired, err := c.desiredPath(ctx, s, packages, false)
	if err != nil {
		return status, err
	}
	for _, snapshot := range snapshots {
		current := systempath.Entries(snapshot)
		for _, dir := range desired {
			status.Drift = status.Drift || !systempath.Contains(current, dir)
		}
		for _, dir := range s.Applied {
			status.Drift = status.Drift || !systempath.Contains(desired, dir) && systempath.Contains(current, dir)
		}
	}
	sort.Slice(status.Entries, func(i, j int) bool {
		a, b := status.Entries[i], status.Entries[j]
		if a.ID != b.ID {
			return a.ID < b.ID
		}
		return newerVersion(a.Version, b.Version)
	})
	return status, nil
}
func (c *Core) applyPathSelection(ctx context.Context, s pathSelection, packages []Package, before []systempath.Snapshot) (pathStatus, error) {
	desired, err := c.desiredPath(ctx, s, packages, true)
	if err != nil {
		return pathStatus{}, err
	}
	status, err := c.pathStatus(ctx, s, packages, before)
	if err != nil {
		return pathStatus{}, err
	}
	// Save a durable recovery snapshot before changing the registry or profiles.
	backupDir := filepath.Join(c.Home, "backup", "pathenv")
	if err = managedDirectory(c.Home, backupDir); err != nil {
		return pathStatus{}, err
	}
	id, err := newID("path")
	if err != nil {
		return pathStatus{}, err
	}
	recovery, _ := json.Marshal(map[string]any{"before": before, "previous": s.Applied, "desired": desired})
	if err = atomicWrite(filepath.Join(backupDir, id+".json"), recovery); err != nil {
		return pathStatus{}, err
	}
	after, err := systempath.Apply(before, s.Applied, desired)
	if err != nil {
		return pathStatus{}, &AppError{Code: "PATH_UPDATE_FAILED", Message: "系统 PATH 更新未完成", Detail: err.Error()}
	}
	owned := desired
	if runtime.GOOS == "windows" || len(before) > 0 && before[0].Fixture {
		owned = []string{}
		for _, dir := range desired {
			wasUserEntry := true
			for _, snapshot := range before {
				wasUserEntry = wasUserEntry && systempath.Contains(systempath.Entries(snapshot), dir)
			}
			if !wasUserEntry || systempath.Contains(s.Applied, dir) {
				owned = append(owned, dir)
			}
		}
	}
	commit := func() error {
		tx, err := c.Store.db.BeginTx(ctx, nil)
		if err != nil {
			return err
		}
		defer tx.Rollback()
		enabled := "0"
		if s.Enabled {
			enabled = "1"
		}
		for key, value := range map[string]any{"pathEnvEnabled": enabled, "pathEnvSelected": s.Selected, "pathEnvVersions": s.Versions, "pathEnvDirs": owned} {
			var text string
			if v, ok := value.(string); ok {
				text = v
			} else {
				data, err := json.Marshal(value)
				if err != nil {
					return err
				}
				text = string(data)
			}
			if _, err = tx.ExecContext(ctx, "INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", key, text); err != nil {
				return err
			}
		}
		return tx.Commit()
	}()
	if commit != nil {
		restoreErr := systempath.Restore(before, after)
		return pathStatus{}, &AppError{Code: "PATH_SETTINGS_FAILED", Message: "PATH 选择未能保存", Detail: errors.Join(commit, restoreErr).Error(), Hint: "恢复快照：" + DisplayPath(filepath.Join(backupDir, id+".json"))}
	}
	s.Applied = owned
	status.ManagedDirs = []Path{}
	for _, dir := range owned {
		status.ManagedDirs = append(status.ManagedDirs, Path(dir))
	}
	status.Drift = false
	for i := range status.Entries {
		status.Entries[i].InPath = true
		for _, snapshot := range after {
			status.Entries[i].InPath = status.Entries[i].InPath && systempath.Contains(systempath.Entries(snapshot), string(status.Entries[i].BinDir))
		}
	}
	return status, nil
}

// The caller holds the service lifecycle lock so installs and version changes
// cannot race with a PATH selection in this process.
func (c *Core) syncPath(ctx context.Context) error {
	s, err := c.pathSelection(ctx)
	if err != nil || !s.Enabled {
		return err
	}
	dir := filepath.Join(c.Home, "service-locks")
	if err = managedDirectory(c.Home, dir); err != nil {
		return err
	}
	unlock, err := process.Lock(filepath.Join(dir, "pathenv.lock"))
	if err != nil {
		return err
	}
	defer unlock()
	packages, err := c.installedRuntimePackages(ctx)
	if err != nil {
		return err
	}
	before, err := c.systemPathSnapshots()
	if err != nil {
		return err
	}
	_, err = c.applyPathSelection(ctx, s, packages, before)
	return err
}
func (c *Core) pathCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[struct {
		Enabled  bool     `json:"enabled"`
		IDs      []string `json:"ids"`
		ID       string   `json:"id"`
		Version  string   `json:"version"`
		Selected bool     `json:"selected"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	dir := filepath.Join(c.Home, "service-locks")
	if err = managedDirectory(c.Home, dir); err != nil {
		return nil, err
	}
	unlock, err := process.Lock(filepath.Join(dir, "pathenv.lock"))
	if err != nil {
		return nil, err
	}
	defer unlock()
	s, err := c.pathSelection(ctx)
	if err != nil {
		return nil, err
	}
	packages, err := c.installedRuntimePackages(ctx)
	if err != nil {
		return nil, err
	}
	snapshots, err := c.systemPathSnapshots()
	if err != nil {
		return nil, err
	}
	if action == "status" {
		return c.pathStatus(ctx, s, packages, snapshots)
	}
	switch action {
	case "enabled":
		s.Enabled = in.Enabled
	case "selected":
		ids := []string{}
		seen := map[string]bool{}
		for _, id := range in.IDs {
			if !safePart(id) {
				return nil, invalid("PATH 套件标识无效")
			}
			if !seen[id] {
				ids = append(ids, id)
				seen[id] = true
			}
		}
		s.Selected = &ids
	case "version":
		found := false
		for _, p := range packages {
			if p.ID == in.ID && canonicalVersion(p.Version) == canonicalVersion(in.Version) {
				if _, err := runtimeDirectory(p); err != nil {
					return nil, err
				}
				in.Version = p.Version
				found = true
				break
			}
		}
		if !found {
			return nil, &AppError{Code: "PATH_VERSION_UNAVAILABLE", Message: "该版本未安装或没有可用命令行入口"}
		}
		chosen, err := c.pathChosen(ctx, s, packages)
		if err != nil {
			return nil, err
		}
		if !in.Selected && (!pathWants(s, in.ID) || chosen[in.ID].Version != in.Version) {
			return c.pathStatus(ctx, s, packages, snapshots)
		}
		ids := []string{}
		if s.Enabled {
			for id := range chosen {
				if id != in.ID && pathWants(s, id) {
					ids = append(ids, id)
				}
			}
		}
		if in.Selected {
			s.Versions[in.ID] = in.Version
			ids = append(ids, in.ID)
			s.Enabled = true
			if in.ID == "npm" || in.ID == "pnpm" {
				found := false
				for _, id := range ids {
					found = found || id == "node"
				}
				if !found {
					ids = append(ids, "node")
				}
			}
		}
		sort.Strings(ids)
		s.Selected = &ids
	}
	return c.applyPathSelection(ctx, s, packages, snapshots)
}
func (c *Core) registerPathEnv() {
	c.handle("pathenv_status", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.pathCommand(ctx, data, "status")
	})
	c.handle("pathenv_reapply", func(ctx context.Context, data json.RawMessage) (any, error) { return c.pathCommand(ctx, data, "apply") })
	c.handle("pathenv_set_enabled", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.pathCommand(ctx, data, "enabled")
	})
	c.handle("pathenv_set_selected", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.pathCommand(ctx, data, "selected")
	})
	c.handle("pathenv_set_version", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.pathCommand(ctx, data, "version")
	})
}
