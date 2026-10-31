package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"time"

	"niceenv/internal/process"
)

// Recover a published package whose SQLite commit was interrupted. A directory
// without a matching manifest snapshot and a real entry is never imported.
func (c *Core) reconcilePackages(ctx context.Context) (any, error) {
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	result := map[string]any{"imported": []map[string]any{}, "refreshed": []map[string]any{}}
	runtimes := filepath.Join(c.Home, "runtimes")
	ids, err := os.ReadDir(runtimes)
	if os.IsNotExist(err) {
		return result, nil
	}
	if err != nil {
		return nil, err
	}
	if err = managedDirectory(c.Home, runtimes); err != nil {
		return nil, err
	}
	for _, id := range ids {
		if err = ctx.Err(); err != nil {
			return nil, err
		}
		if !id.IsDir() || !safePart(id.Name()) || id.Type()&os.ModeSymlink != 0 {
			continue
		}
		packageDir := filepath.Join(runtimes, id.Name())
		if err = managedDirectory(c.Home, packageDir); err != nil {
			return nil, err
		}
		versions, err := os.ReadDir(packageDir)
		if err != nil {
			return nil, err
		}
		for _, version := range versions {
			if !version.IsDir() || !safePart(version.Name()) || version.Type()&os.ModeSymlink != 0 {
				continue
			}
			root := filepath.Join(packageDir, version.Name())
			if err = managedDirectory(c.Home, root); err != nil {
				return nil, err
			}
			name := filepath.Join(root, ".niceenv-package.json")
			info, err := os.Lstat(name)
			if os.IsNotExist(err) {
				continue
			}
			if err != nil {
				return nil, err
			}
			if !info.Mode().IsRegular() || info.Size() > 1024*1024 {
				continue
			}
			content, err := os.ReadFile(name)
			if err != nil {
				return nil, err
			}
			var pkg Package
			if json.Unmarshal(content, &pkg) != nil || pkg.ID != id.Name() || pkg.Version != version.Name() || pkg.Category == "" {
				continue
			}
			entry, err := relativePath(root, pkg.Entry)
			if err != nil {
				continue
			}
			entryInfo, err := os.Lstat(entry)
			if err != nil || !entryInfo.Mode().IsRegular() || entryInfo.Size() == 0 {
				continue
			}
			resolved, err := filepath.EvalSymlinks(entry)
			if err != nil {
				continue
			}
			rel, err := filepath.Rel(root, resolved)
			if err != nil || !filepath.IsLocal(rel) {
				continue
			}
			key := pkg.ID + "@" + pkg.Version
			lockDir := filepath.Join(c.Home, "install-locks")
			if err = managedDirectory(c.Home, lockDir); err != nil {
				return nil, err
			}
			unlock, err := process.Lock(filepath.Join(lockDir, key+".lock"))
			if errors.Is(err, process.ErrLocked) {
				continue
			}
			if err != nil {
				return nil, err
			}
			row, inserted, recordErr := c.reconcilePackageRecord(ctx, pkg, root, info.ModTime())
			unlockErr := unlock()
			if recordErr != nil || unlockErr != nil {
				return nil, errors.Join(recordErr, unlockErr)
			}
			if inserted {
				result["imported"] = append(result["imported"].([]map[string]any), row)
			}
		}
	}
	if err = c.refreshServices(ctx); err != nil {
		return nil, err
	}
	return result, nil
}
func (c *Core) reconcilePackageRecord(ctx context.Context, pkg Package, root string, modified time.Time) (map[string]any, bool, error) {
	var existing string
	err := c.Store.db.QueryRowContext(ctx, "SELECT install_path FROM installed WHERE id=? AND version=?", pkg.ID, pkg.Version).Scan(&existing)
	if err == nil {
		return nil, false, nil
	}
	if !errors.Is(err, sql.ErrNoRows) {
		return nil, false, err
	}
	config := filepath.Join(c.Home, "etc", pkg.ID, pkg.Version)
	if err = managedDirectory(c.Home, config); err != nil {
		return nil, false, err
	}
	at := modified.UnixMilli()
	if at <= 0 {
		at = time.Now().UnixMilli()
	}
	_, err = c.Store.db.ExecContext(ctx, "INSERT INTO installed(key,id,version,category,install_path,config_path,installed_at) VALUES(?,?,?,?,?,?,?)", pkg.ID+"@"+pkg.Version, pkg.ID, pkg.Version, pkg.Category, root, config, at)
	if err != nil {
		return nil, false, err
	}
	return map[string]any{"id": pkg.ID, "version": pkg.Version, "category": pkg.Category, "installPath": Path(root), "configPath": Path(config), "installedAt": at}, true, nil
}
