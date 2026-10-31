package core

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"niceenv/internal/process"
)

type uninstallBlocker struct {
	Kind string `json:"kind"`
	Name string `json:"name"`
}
type uninstallPreview struct {
	Installed   map[string]any     `json:"installed"`
	RuntimePath Path               `json:"runtimePath"`
	Service     *ServiceStatus     `json:"service,omitempty"`
	Blockers    []uninstallBlocker `json:"blockers"`
}

func (c *Core) uninstallPreview(ctx context.Context, key string) (uninstallPreview, Package, error) {
	out := uninstallPreview{Blockers: []uninstallBlocker{}}
	id, version, exact := strings.Cut(key, "@")
	if !safePart(id) || exact && !safePart(version) {
		return out, Package{}, invalid("套件标识无效")
	}
	packages, err := c.installedRuntimePackages(ctx)
	if err != nil {
		return out, Package{}, err
	}
	var target Package
	for _, p := range packages {
		if p.ID != id {
			continue
		}
		if exact && canonicalVersion(p.Version) == canonicalVersion(version) || !exact && p.Active {
			target = p
			break
		}
	}
	if target.Install == nil {
		return out, target, &AppError{Code: "PACKAGE_NOT_INSTALLED", Message: "该套件版本未安装"}
	}
	version = target.Version
	if !safePart(version) {
		return out, target, invalid("已安装版本标识无效")
	}
	runtimeDir := filepath.Join(c.Home, "runtimes", id, version)
	// Never recursively remove a path imported from an external installation.
	rel, err := filepath.Rel(runtimeDir, DisplayPath(target.Install.InstallPath))
	if err != nil || rel != "." {
		return out, target, &AppError{Code: "INVALID_PACKAGE_PATH", Message: "安装目录不在此版本的受管目录内，未允许卸载"}
	}
	out.RuntimePath = Path(runtimeDir)
	out.Installed = map[string]any{"id": id, "version": version, "category": target.Category, "installPath": Path(target.Install.InstallPath), "configPath": Path(target.Install.ConfigPath), "installedAt": target.Install.InstalledAt}
	alternative := false
	for _, p := range packages {
		alternative = alternative || p.ID == id && canonicalVersion(p.Version) != canonicalVersion(version)
	}
	references := func(key string) bool {
		other, v, fixed := strings.Cut(key, "@")
		return other == id && (fixed && canonicalVersion(v) == canonicalVersion(version) || !fixed && !alternative)
	}
	rows, err := c.Store.tableRows(ctx, "SELECT id,name,runtime,COALESCE(db,'') AS db FROM sites", nil, nil)
	if err != nil {
		return out, target, err
	}
	for _, row := range rows {
		var rt struct {
			Kind        string `json:"kind"`
			PHPVersion  string `json:"phpVersion"`
			WebServer   string `json:"webServer"`
			Application *struct {
				Version string `json:"version"`
			} `json:"application"`
		}
		if json.Unmarshal([]byte(row["runtime"].(string)), &rt) != nil {
			return out, target, invalid("站点运行时记录损坏，无法检查卸载引用")
		}
		used := id == "php" && rt.Kind == "php" && canonicalVersion(rt.PHPVersion) == canonicalVersion(version)
		web := rt.WebServer
		if web == "" {
			web = "nginx"
		}
		used = used || web == id && !alternative
		if rt.Application != nil {
			runtimeID := rt.Kind
			if runtimeID == "nodejs" {
				runtimeID = "node"
			}
			if runtimeID == "java" {
				runtimeID = "temurin-jdk21"
			}
			used = used || runtimeID == id && canonicalVersion(rt.Application.Version) == canonicalVersion(version)
		}
		if db := row["db"].(string); db != "" && db != "null" {
			var binding struct {
				Enabled bool   `json:"enabled"`
				Version string `json:"version"`
			}
			if json.Unmarshal([]byte(db), &binding) != nil {
				return out, target, invalid("站点数据库绑定损坏，无法检查卸载引用")
			}
			used = used || id == "mysql" && binding.Enabled && (binding.Version == "" && !alternative || binding.Version != "" && canonicalVersion(binding.Version) == canonicalVersion(version))
		}
		if target.Category == "runtime" && rt.Kind != "redirect" {
			p, err := c.openProject(ctx, row["id"].(string), false)
			if err != nil {
				return out, target, &AppError{Code: "PROJECT_RUNTIME_UNREADABLE", Message: "无法检查站点「" + row["name"].(string) + "」的项目版本", Detail: publicError(err).Message}
			}
			content, exists, readErr := envRead(p.root, ".niceenv.json")
			files := projectRuntimeFiles(p.root)
			p.root.Close()
			if readErr != nil {
				return out, target, readErr
			}
			_, versions, err := projectDocument(content, exists)
			if err != nil {
				return out, target, err
			}
			if fixed, ok := versions[id]; ok {
				used = used || canonicalVersion(fixed) == canonicalVersion(version)
			} else if id == "node" || id == "python" {
				detections, err := c.runtimeDetections(ctx, files, packages)
				if err != nil {
					return out, target, err
				}
				for _, d := range detections {
					if d.ID != id {
						continue
					}
					if d.Issue != nil {
						return out, target, &AppError{Code: "PROJECT_RUNTIME_UNREADABLE", Message: "项目版本存在冲突，未卸载运行时", Detail: *d.Issue}
					}
					used = used || d.ResolvedVersion != nil && canonicalVersion(*d.ResolvedVersion) == canonicalVersion(version)
				}
			}
		}
		if used {
			out.Blockers = append(out.Blockers, uninstallBlocker{"site", row["name"].(string)})
		}
	}
	stackRows, err := c.Store.db.QueryContext(ctx, stackSelect)
	if err != nil {
		return out, target, err
	}
	for stackRows.Next() {
		stack, err := scanStack(stackRows)
		if err != nil {
			stackRows.Close()
			return out, target, err
		}
		if stack.Builtin {
			continue
		}
		for _, item := range stack.Items {
			if references(item.ServiceID) {
				out.Blockers = append(out.Blockers, uninstallBlocker{"stack", stack.Name})
				break
			}
		}
	}
	err = stackRows.Err()
	stackRows.Close()
	if err != nil {
		return out, target, err
	}
	views, err := c.packageViews(ctx)
	if err != nil {
		return out, target, err
	}
	for _, view := range views {
		if view["install"] == nil || view["id"] == id && canonicalVersion(view["version"].(string)) == canonicalVersion(version) {
			continue
		}
		dependencies := append(append([]any{}, array(view["requires"])...), array(view["depends"])...)
		dependencies = append(dependencies, array(field(view["run"], "requires"))...)
		for _, dependency := range dependencies {
			if references(textValue(dependency)) {
				label, _ := view["displayName"].(string)
				out.Blockers = append(out.Blockers, uninstallBlocker{"package", label + " " + view["version"].(string)})
				break
			}
		}
	}
	c.services.mu.RLock()
	for _, e := range c.services.entries {
		if e.pkg.ID == id && canonicalVersion(e.pkg.Version) == canonicalVersion(version) {
			snapshot := e.status
			snapshot.PIDs = append([]int{}, e.status.PIDs...)
			out.Service = &snapshot
		}
	}
	c.services.mu.RUnlock()
	sort.Slice(out.Blockers, func(i, j int) bool {
		a, b := out.Blockers[i], out.Blockers[j]
		if a.Kind != b.Kind {
			return a.Kind < b.Kind
		}
		return a.Name < b.Name
	})
	return out, target, nil
}

func (c *Core) uninstallPackage(ctx context.Context, key string) (any, error) {
	preview, target, err := c.uninstallPreview(ctx, key)
	if err != nil {
		return nil, err
	}
	if len(preview.Blockers) > 0 {
		names := []string{}
		for _, blocker := range preview.Blockers {
			names = append(names, blocker.Name)
		}
		return nil, &AppError{Code: "PACKAGE_IN_USE", Message: "仍被以下项目使用，无法卸载：" + strings.Join(names, "、")}
	}
	locks := filepath.Join(c.Home, "install-locks")
	if err = managedDirectory(c.Home, locks); err != nil {
		return nil, err
	}
	unlock, err := process.Lock(filepath.Join(locks, target.ID+"@"+target.Version+".lock"))
	if err != nil {
		return nil, err
	}
	defer unlock()
	if preview.Service != nil {
		c.services.mu.RLock()
		entry := c.services.entries[preview.Service.ID]
		c.services.mu.RUnlock()
		if entry != nil {
			if err = c.stopOwned(ctx, entry, false); err != nil {
				return nil, err
			}
		}
	}
	path := string(preview.RuntimePath)
	parent := filepath.Dir(path)
	if err = managedDirectory(c.Home, parent); err != nil {
		return nil, err
	}
	info, statErr := os.Lstat(path)
	if statErr != nil && !os.IsNotExist(statErr) {
		return nil, statErr
	}
	if statErr == nil && (!info.IsDir() || info.Mode()&os.ModeSymlink != 0) {
		return nil, invalid("运行时目录为链接或特殊文件，未卸载")
	}
	if statErr == nil {
		actual, err := filepath.EvalSymlinks(path)
		if err != nil {
			return nil, err
		}
		rel, err := filepath.Rel(path, DisplayPath(actual))
		if err != nil || rel != "." {
			return nil, invalid("运行时目录经过链接重定向，未卸载")
		}
	}
	// Rename within the verified runtime parent first; database failures can put
	// the complete directory back before any recursive deletion begins.
	quarantine := ""
	if statErr == nil {
		id, err := newID(".uninstall")
		if err != nil {
			return nil, err
		}
		quarantine = filepath.Join(parent, id)
		if err = os.Rename(path, quarantine); err != nil {
			return nil, err
		}
	}
	commitCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	commitErr := func() error {
		tx, err := c.Store.db.BeginTx(commitCtx, nil)
		if err != nil {
			return err
		}
		defer tx.Rollback()
		if _, err = tx.ExecContext(commitCtx, "DELETE FROM installed WHERE id=? AND version=?", target.ID, target.Version); err != nil {
			return err
		}
		if _, err = tx.ExecContext(commitCtx, "DELETE FROM settings WHERE key=? AND value=?", "active"+target.ID+"Version", target.Version); err != nil {
			return err
		}
		return tx.Commit()
	}()
	if commitErr != nil {
		var restoreErr error
		if quarantine != "" {
			restoreErr = os.Rename(quarantine, path)
		}
		return nil, &AppError{Code: "UNINSTALL_RECORD_FAILED", Message: "卸载记录保存失败", Detail: errors.Join(commitErr, restoreErr).Error()}
	}
	var cleanupErr error
	if quarantine != "" {
		rel, err := filepath.Rel(parent, quarantine)
		if err != nil || !filepath.IsLocal(rel) || strings.ContainsAny(rel, "/\\") || !strings.HasPrefix(rel, ".uninstall-") {
			cleanupErr = invalid("卸载临时目录越界，未清理")
		} else if target.ID == "qdrant" {
			// Older Qdrant builds may keep snapshots alongside their executable.
			// Preserve the entire retired runtime until the snapshot configuration
			// can be resolved, rather than deleting any user snapshots with it.
			backup := filepath.Join(c.Home, "backup", "qdrant-runtimes")
			cleanupErr = managedDirectory(c.Home, backup)
			if cleanupErr == nil {
				cleanupErr = os.Rename(quarantine, filepath.Join(backup, target.Version+"-"+filepath.Base(quarantine)))
			}
		} else {
			cleanupErr = os.RemoveAll(quarantine)
		}
	}
	refreshErr := c.refreshServices(commitCtx)
	pathErr := c.syncPath(commitCtx)
	if err = errors.Join(cleanupErr, refreshErr, pathErr); err != nil {
		return nil, &AppError{Code: "UNINSTALL_CLEANUP_FAILED", Message: "套件已卸载，但目录清理或环境同步未完成", Detail: err.Error(), Hint: "配置和业务数据已保留；请检查 PATH 设置及 " + DisplayPath(quarantine)}
	}
	return true, nil
}
func (c *Core) uninstallCommand(ctx context.Context, data json.RawMessage, previewOnly bool) (any, error) {
	in, err := decode[struct {
		ID string `json:"id"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.siteChanges.TryLock() {
		return nil, serviceBusy()
	}
	defer c.siteChanges.Unlock()
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	if previewOnly {
		preview, _, err := c.uninstallPreview(ctx, in.ID)
		return preview, err
	}
	return c.uninstallPackage(ctx, in.ID)
}
func (c *Core) registerUninstaller() {
	c.handle("preview_package_uninstall", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.uninstallCommand(ctx, data, true)
	})
	c.handle("uninstall_package", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.uninstallCommand(ctx, data, false)
	})
}
