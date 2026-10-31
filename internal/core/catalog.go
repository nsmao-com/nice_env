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
	"time"

	"niceenv/manifest"
)

type Catalog struct {
	Revision int              `json:"revision"`
	Updated  string           `json:"updated"`
	Packages []map[string]any `json:"packages"`
}

func compatible(entry map[string]any) bool {
	osName := runtime.GOOS
	if osName == "darwin" {
		osName = "macos"
	}
	arch := runtime.GOARCH
	if arch == "amd64" {
		arch = "x64"
	}
	for field, desired := range map[string]string{"os": osName, "arch": arch} {
		values, _ := entry[field].([]any)
		if len(values) == 0 {
			continue
		}
		found := false
		for _, v := range values {
			if v == desired {
				found = true
				break
			}
		}
		if !found {
			return false
		}
	}
	return true
}

func canonicalVersion(version string) string { return strings.TrimLeft(version, "vV") }
func packageKey(entry map[string]any) string {
	id, _ := entry["id"].(string)
	version, _ := entry["version"].(string)
	return id + "@" + canonicalVersion(version)
}

func loadCatalog(home string) (Catalog, error) {
	file := "packages.win.json"
	if runtime.GOOS == "darwin" {
		file = "packages.mac.json"
	}
	if runtime.GOOS != "windows" && runtime.GOOS != "darwin" {
		return Catalog{}, &AppError{Code: "PLATFORM_UNSUPPORTED", Message: "当前套件清单只支持 Windows 和 macOS"}
	}
	data, err := manifest.Files.ReadFile(file)
	if err != nil {
		return Catalog{}, err
	}
	var result Catalog
	if err = json.Unmarshal(data, &result); err != nil {
		return result, err
	}
	layers := []string{filepath.Join(home, "etc", "manifest.json")}
	users, err := filepath.Glob(filepath.Join(home, "user-modules", "*.json"))
	if err != nil {
		return result, err
	}
	sort.Strings(users)
	layers = append(layers, users...)
	entries := map[string]map[string]any{}
	order := []string{}
	merge := func(packages []map[string]any) {
		for _, entry := range packages {
			if !compatible(entry) {
				continue
			}
			key := packageKey(entry)
			if key == "@" || strings.HasPrefix(key, "@") {
				continue
			}
			if _, found := entries[key]; !found {
				order = append(order, key)
			}
			entries[key] = entry
		}
	}
	merge(result.Packages)
	for _, path := range layers {
		info, err := os.Stat(path)
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			return result, err
		}
		if !info.Mode().IsRegular() || info.Size() > 8*1024*1024 {
			return result, &AppError{Code: "MANIFEST_INVALID", Message: "套件清单过大或不是普通文件"}
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return result, err
		}
		var layer Catalog
		if json.Unmarshal(data, &layer) != nil {
			return result, &AppError{Code: "MANIFEST_INVALID", Message: "本地套件清单格式无效", Detail: DisplayPath(path)}
		}
		merge(layer.Packages)
	}
	result.Packages = make([]map[string]any, 0, len(order))
	for _, key := range order {
		result.Packages = append(result.Packages, entries[key])
	}
	return result, nil
}

func (c *Core) registerCatalog() {
	c.handle("list_packages", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.packageViews(ctx) })
	c.handle("set_active_version", func(ctx context.Context, data json.RawMessage) (any, error) {
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		args, err := decode[struct {
			ID      string `json:"id"`
			Version string `json:"version"`
		}](data)
		if err != nil {
			return nil, err
		}
		if args.ID == "" || args.Version == "" {
			return nil, invalid("套件和版本不能为空")
		}
		if !safePart(args.ID) || !safePart(args.Version) {
			return nil, invalid("套件或版本标识无效")
		}
		var exists int
		if err = c.Store.db.QueryRowContext(ctx, "SELECT COUNT(*) FROM installed WHERE id=? AND version=?", args.ID, args.Version).Scan(&exists); err != nil {
			return nil, err
		}
		if exists == 0 {
			return nil, &AppError{Code: "PACKAGE_NOT_INSTALLED", Message: "该版本尚未安装，不能切换"}
		}
		c.services.mu.RLock()
		for _, entry := range c.services.entries {
			if entry.status.ID == args.ID || strings.HasPrefix(entry.status.ID, args.ID+"@") {
				if entry.child != nil {
					c.services.mu.RUnlock()
					return nil, &AppError{Code: "SERVICE_BUSY", Message: "服务运行中，请停止后再切换版本"}
				}
			}
		}
		c.services.mu.RUnlock()
		key := "active" + args.ID + "Version"
		previous, existed, err := c.Store.setting(ctx, key)
		if err != nil {
			return nil, err
		}
		if err = c.Store.saveSetting(ctx, key, args.Version); err != nil {
			return nil, err
		}
		err = c.refreshServices(ctx)
		if err == nil {
			err = c.syncPath(ctx)
		}
		if err != nil {
			recovery, cancel := context.WithTimeout(context.Background(), 15*time.Second)
			defer cancel()
			var rollbackErr error
			if existed {
				rollbackErr = c.Store.saveSetting(recovery, key, previous)
			} else {
				_, rollbackErr = c.Store.db.ExecContext(recovery, "DELETE FROM settings WHERE key=?", key)
			}
			rollbackErr = errors.Join(rollbackErr, c.refreshServices(recovery))
			if rollbackErr != nil {
				return nil, &AppError{Code: "VERSION_RECOVERY_REQUIRED", Message: "版本切换失败，恢复原选择时出现错误", Detail: errors.Join(err, rollbackErr).Error()}
			}
			return nil, err
		}
		return true, nil
	})
}

func (c *Core) packageViews(ctx context.Context) ([]map[string]any, error) {
	installed, err := c.Store.tableRows(ctx, `SELECT id,version,category,install_path AS installPath,config_path AS configPath,installed_at AS installedAt FROM installed ORDER BY id,version`, nil, nil)
	if err != nil {
		return nil, err
	}
	// Clone the immutable startup catalog; callers never mutate shared maps.
	data, err := json.Marshal(c.catalog.Packages)
	if err != nil {
		return nil, err
	}
	var entries []map[string]any
	if err = json.Unmarshal(data, &entries); err != nil {
		return nil, err
	}
	indices := map[string]int{}
	for i, entry := range entries {
		indices[packageKey(entry)] = i
	}
	active := map[string]string{}
	installations := map[string]map[string]any{}
	for _, record := range installed {
		id := record["id"].(string)
		version := record["version"].(string)
		key := id + "@" + canonicalVersion(version)
		installations[key] = record
		if active[id] == "" || newerVersion(version, active[id]) {
			active[id] = version
		}
		path := filepath.Join(record["installPath"].(string), ".niceenv-package.json")
		var snapshot map[string]any
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() && info.Size() <= 1024*1024 {
			if data, err := os.ReadFile(path); err == nil {
				if json.Unmarshal(data, &snapshot) != nil || packageKey(snapshot) != key {
					snapshot = nil
				}
			}
		}
		index, found := indices[key]
		if snapshot != nil {
			if found {
				entries[index] = snapshot
			} else {
				indices[key] = len(entries)
				entries = append(entries, snapshot)
			}
		} else if !found {
			indices[key] = len(entries)
			entries = append(entries, map[string]any{"id": id, "version": version, "category": record["category"], "displayName": id, "description": "已安装的历史版本，缺少运行描述", "kind": "binary", "os": []string{}, "arch": []string{}, "entry": "", "url": "", "sizeBytes": 0})
		}
	}
	for id := range active {
		selected, found, err := c.Store.setting(ctx, "active"+id+"Version")
		if err != nil {
			return nil, err
		}
		if found {
			if _, installed := installations[id+"@"+canonicalVersion(selected)]; installed {
				active[id] = selected
			}
		}
	}
	versions := map[string][]string{}
	for _, entry := range entries {
		id, _ := entry["id"].(string)
		version, _ := entry["version"].(string)
		versions[id] = append(versions[id], canonicalVersion(version))
	}
	for _, values := range versions {
		sort.Slice(values, func(i, j int) bool { return newerVersion(values[i], values[j]) })
	}
	for _, entry := range entries {
		id, _ := entry["id"].(string)
		version, _ := entry["version"].(string)
		entry["active"] = active[id] != "" && canonicalVersion(active[id]) == canonicalVersion(version)
		entry["availableVersions"] = versions[id]
		if installation, ok := installations[packageKey(entry)]; ok {
			entry["install"] = map[string]any{"version": installation["version"], "installPath": Path(installation["installPath"].(string)), "configPath": Path(installation["configPath"].(string)), "installedAt": installation["installedAt"]}
		}
	}
	return entries, nil
}
