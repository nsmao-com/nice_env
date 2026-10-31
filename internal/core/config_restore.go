package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
)

type backupPreview struct {
	Name             string  `json:"name"`
	TargetPath       Path    `json:"targetPath"`
	TargetRelative   string  `json:"targetRelative"`
	CurrentExists    bool    `json:"currentExists"`
	Revision         string  `json:"revision"`
	BackupContent    *string `json:"backupContent"`
	CurrentContent   *string `json:"currentContent"`
	BackupSizeBytes  int     `json:"backupSizeBytes"`
	CurrentSizeBytes int     `json:"currentSizeBytes"`
	Changed          bool    `json:"changed"`
}

// A listing owns this snapshot. Never reuse it for preview or restore, which
// must resolve current files again before checking the revision.
type configBackupSnapshot struct {
	targets      []configTarget
	legacy       map[string][]string
	legacyErr    error
	legacyLoaded bool
}

func (s *configBackupSnapshot) legacyMatches(ctx context.Context, home, name string) ([]string, error) {
	if !s.legacyLoaded {
		s.legacyLoaded = true
		s.legacy = make(map[string][]string)
		visited := 0
		s.legacyErr = filepath.WalkDir(filepath.Join(home, "etc"), func(p string, entry fs.DirEntry, walkErr error) error {
			if walkErr != nil {
				return walkErr
			}
			if err := ctx.Err(); err != nil {
				return err
			}
			visited++
			if visited > 20000 {
				return invalid("配置条目过多，无法确定旧备份目标")
			}
			if entry.Type()&os.ModeSymlink != 0 || entry.IsDir() {
				return nil
			}
			rel, err := filepath.Rel(home, p)
			if err != nil {
				return err
			}
			s.legacy[entry.Name()] = append(s.legacy[entry.Name()], filepath.ToSlash(rel))
			return nil
		})
	}
	return s.legacy[name], s.legacyErr
}

func (c *Core) previewConfigBackup(ctx context.Context, name string) (backupPreview, configTarget, error) {
	var empty backupPreview
	var noTarget configTarget
	name, source, target, expectedHash, err := c.configBackupSource(ctx, name)
	if err != nil {
		return empty, noTarget, err
	}
	backup, err := readConfigText(source)
	if err != nil {
		return empty, noTarget, err
	}
	if expectedHash != "" {
		digest := sha256.Sum256([]byte(backup))
		if hex.EncodeToString(digest[:]) != expectedHash {
			return empty, noTarget, invalid("备份内容校验失败，原配置未改动")
		}
	}
	current, err := readConfigText(target.path)
	exists := err == nil
	if err != nil && !os.IsNotExist(err) {
		return empty, noTarget, err
	}
	var currentPtr *string
	if exists {
		currentPtr = &current
	}
	rel, err := filepath.Rel(c.Home, target.path)
	if err != nil || !filepath.IsLocal(rel) {
		return empty, noTarget, invalid("配置目标超出数据目录")
	}
	history, err := readDataPathHistory(c.Home)
	if err != nil {
		return empty, noTarget, err
	}
	sort.Slice(history, func(i, j int) bool { return len(history[i]) > len(history[j]) })
	for _, old := range history {
		rebase := dataPathRebase{old, c.Home}
		plan, planErr := planWebConfigMove(ctx, c.Home, rebase, map[string]string{target.path: backup})
		if planErr != nil {
			return empty, noTarget, planErr
		}
		if plan.handled(target.path) {
			if next, ok := plan.edits[target.path]; ok {
				backup = next
			}
		} else {
			backup, err = rebaseConfigContent(rebase, filepath.ToSlash(rel), backup)
		}
		if err != nil {
			return empty, noTarget, err
		}
	}
	signature, err := json.Marshal([]any{name, target.key, target.path, backup, expectedHash, currentPtr})
	if err != nil {
		return empty, noTarget, err
	}
	hash := sha256.Sum256(signature)
	return backupPreview{Name: name, TargetPath: Path(target.path), TargetRelative: filepath.ToSlash(rel), CurrentExists: exists, Revision: hex.EncodeToString(hash[:]), BackupContent: &backup, CurrentContent: currentPtr, BackupSizeBytes: len(backup), CurrentSizeBytes: len(current), Changed: !exists || backup != current}, target, nil
}

func readDataPathHistory(home string) ([]string, error) {
	data, err := readPrivateMaterialLimit(filepath.Join(home, ".data-dir-history.json"), 1024*1024)
	if os.IsNotExist(err) {
		return []string{}, nil
	}
	if err != nil {
		return nil, err
	}
	var history []string
	if json.Unmarshal(data, &history) != nil || len(history) > 100 {
		return nil, invalid("数据目录历史无效，无法转换备份路径")
	}
	for _, path := range history {
		if !filepath.IsAbs(path) {
			return nil, invalid("数据目录历史包含相对路径")
		}
	}
	return history, nil
}

// Resolve v1 metadata bundles and uniquely identifiable legacy backups as well
// as the version-pinned editor history. Inventory reads metadata only; preview
// verifies the content hash and includes it in the compare-and-swap revision.
func (c *Core) configBackupSource(ctx context.Context, name string, snapshots ...*configBackupSnapshot) (string, string, configTarget, string, error) {
	var target configTarget
	var targets [][]configTarget
	snapshot := &configBackupSnapshot{}
	if len(snapshots) > 0 {
		snapshot = snapshots[0]
		targets = append(targets, snapshot.targets)
	}
	name = strings.TrimPrefix(name, "legacy/")
	if strings.ContainsAny(name, `\:`) || !fs.ValidPath(name) || name == "." {
		return "", "", target, "", invalid("配置备份名称无效")
	}
	parts := strings.Split(name, "/")
	if len(parts) == 1 && strings.HasSuffix(name, ".bak") {
		prefix, _, ok := strings.Cut(name, "-")
		if _, err := hex.DecodeString(prefix); ok && err == nil {
			name = "config/" + name
			parts = strings.Split(name, "/")
		}
	}
	source := filepath.Join(c.Home, "backup", filepath.FromSlash(name))
	expectedHash := ""
	var err error
	switch {
	case len(parts) == 2 && parts[0] == "config" && strings.HasSuffix(parts[1], ".bak"):
		prefix, _, _ := strings.Cut(parts[1], "-")
		keyBytes, decodeErr := hex.DecodeString(prefix)
		if decodeErr != nil {
			return "", "", target, "", invalid("配置备份目标无效")
		}
		key := string(keyBytes)
		if strings.HasPrefix(key, "file@") {
			target, err = c.backupTargetRelative(ctx, strings.TrimPrefix(key, "file@"), targets...)
		} else {
			if len(snapshots) > 0 {
				err = invalid("备份对应的配置版本未安装")
				for _, candidate := range snapshot.targets {
					if candidate.key == key {
						target, err = candidate, nil
						break
					}
				}
			} else {
				target, err = c.configTarget(ctx, key)
			}
			if err == nil && key != target.key {
				err = invalid("配置备份没有固定到准确的版本")
			}
		}
	case len(parts) == 2 && parts[0] == "files" && strings.HasPrefix(parts[1], "cfg-"):
		if err = managedDirectory(c.Home, source); err != nil {
			break
		}
		metadata, readErr := readPrivateMaterialLimit(filepath.Join(source, "metadata.json"), 65536)
		if readErr != nil {
			err = readErr
			break
		}
		var record struct {
			Version int    `json:"version"`
			Target  string `json:"target"`
			SHA256  string `json:"sha256"`
		}
		if json.Unmarshal(metadata, &record) != nil || record.Version != 1 || len(record.SHA256) != 64 {
			err = invalid("备份元信息格式无效")
			break
		}
		if _, decodeErr := hex.DecodeString(record.SHA256); decodeErr != nil {
			err = invalid("备份校验值无效")
			break
		}
		target, err = c.backupTargetRelative(ctx, record.Target, targets...)
		expectedHash = strings.ToLower(record.SHA256)
		source = filepath.Join(source, "content.bak")
	case len(parts) == 1 && strings.HasSuffix(name, ".bak"):
		original := strings.TrimSuffix(name, ".bak")
		index := strings.LastIndexByte(original, '.')
		if index < 1 {
			err = invalid("旧备份文件名无法解析")
			break
		}
		original = original[:index]
		if original == "php.ini" || original == "my.ini" || original == "redis.conf" {
			err = invalid("旧备份没有记录所属版本，不能自动恢复")
			break
		}
		var matches []string
		matches, err = snapshot.legacyMatches(ctx, c.Home, original)
		if err == nil {
			if len(matches) != 1 {
				err = invalid("旧备份目标不存在或有同名歧义，不能自动恢复")
			} else {
				target, err = c.backupTargetRelative(ctx, matches[0], targets...)
			}
		}
	default:
		err = invalid("配置备份标识无效")
	}
	if err == nil {
		err = managedDirectory(c.Home, filepath.Dir(source))
	}
	return name, source, target, expectedHash, err
}

func (c *Core) backupTargetRelative(ctx context.Context, relative string, snapshots ...[]configTarget) (configTarget, error) {
	var empty configTarget
	if !fs.ValidPath(relative) || strings.ContainsAny(relative, `\:`) {
		return empty, invalid("备份配置目标无效")
	}
	path := filepath.Join(c.Home, filepath.FromSlash(relative))
	var targets []configTarget
	var err error
	if len(snapshots) > 0 {
		targets = snapshots[0]
	} else {
		targets, err = c.configTargets(ctx)
	}
	if err != nil {
		return empty, err
	}
	for _, target := range targets {
		if target.path == path || runtime.GOOS == "windows" && strings.EqualFold(target.path, path) {
			return target, managedDirectory(c.Home, filepath.Dir(path))
		}
	}
	if !strings.HasPrefix(relative, "etc/") {
		return empty, invalid("此备份不是服务配置，请在对应功能页处理")
	}
	if err = managedDirectory(c.Home, filepath.Dir(path)); err != nil {
		return empty, err
	}
	return configTarget{key: "file@" + relative, path: path}, nil
}

func (c *Core) registerConfigRestore() {
	c.handle("list_backups", func(ctx context.Context, _ json.RawMessage) (any, error) {
		return c.listFileBackups(ctx)
	})
	c.handle("preview_backup", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Name string `json:"name"`
		}](data)
		if err != nil {
			return nil, err
		}
		preview, _, err := c.previewConfigBackup(ctx, in.Name)
		return preview, err
	})
	c.handle("restore_backup", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Name     string `json:"name"`
			Revision string `json:"revision"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		preview, target, err := c.previewConfigBackup(ctx, in.Name)
		if err != nil {
			return nil, err
		}
		if in.Revision == "" || in.Revision != preview.Revision {
			return nil, &AppError{Code: "BACKUP_PREVIEW_CHANGED", Message: "备份或当前配置已变化，请重新预览后恢复"}
		}
		return Path(target.path), c.writeConfigExpected(ctx, target, *preview.BackupContent, preview.CurrentContent, !preview.CurrentExists)
	})
	c.handle("config_rollback", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Name            string  `json:"name"`
			Kind            string  `json:"kind"`
			ExpectedContent *string `json:"expectedContent"`
			ExpectedPath    string  `json:"expectedPath"`
			Revision        string  `json:"revision"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		preview, target, err := c.previewConfigBackup(ctx, in.Name)
		if err != nil {
			return nil, err
		}
		requested, err := c.configTarget(ctx, in.Kind)
		if err != nil {
			return nil, err
		}
		if requested.key != target.key || DisplayPath(in.ExpectedPath) != DisplayPath(target.path) {
			return nil, &AppError{Code: "BACKUP_TARGET_MISMATCH", Message: "备份与当前配置文件不匹配"}
		}
		if in.Revision == "" || in.Revision != preview.Revision {
			return nil, &AppError{Code: "BACKUP_PREVIEW_CHANGED", Message: "备份或配置已变化，请重新预览"}
		}
		if in.ExpectedContent == nil || preview.CurrentContent == nil || *in.ExpectedContent != *preview.CurrentContent {
			return nil, &AppError{Code: "CONFIG_CONFLICT", Message: "编辑器内容与磁盘配置不一致，请先保留草稿"}
		}
		return true, c.writeConfig(ctx, target, *preview.BackupContent, in.ExpectedContent)
	})
}

// Inventory metadata only. Reading every backup's contents during page loads
// would make the settings page slower as history grows. Preview verifies bytes.
func (c *Core) listFileBackups(ctx context.Context) ([]map[string]any, error) {
	dir := filepath.Join(c.Home, "backup")
	out := []map[string]any{}
	if _, err := os.Lstat(dir); os.IsNotExist(err) {
		return out, nil
	} else if err != nil {
		return nil, err
	}
	if err := managedDirectory(c.Home, dir); err != nil {
		return nil, err
	}
	visited := 0
	targets, err := c.configTargets(ctx)
	if err != nil {
		return nil, err
	}
	snapshot := &configBackupSnapshot{targets: targets}
	err = filepath.WalkDir(dir, func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if err := ctx.Err(); err != nil {
			return err
		}
		visited++
		if visited > 20000 {
			return &AppError{Code: "BACKUP_LIST_LIMIT", Message: "备份目录条目过多，请先归档旧备份"}
		}
		if entry.Type()&os.ModeSymlink != 0 {
			return nil
		}
		rel, err := filepath.Rel(dir, path)
		if err != nil {
			return err
		}
		name := filepath.ToSlash(rel)
		bundle := entry.IsDir() && strings.HasPrefix(name, "files/cfg-") && strings.Count(name, "/") == 1
		if entry.IsDir() && !bundle {
			if strings.Count(name, "/") >= 4 {
				return filepath.SkipDir
			}
			return nil
		}
		if !bundle && !strings.HasSuffix(entry.Name(), ".bak") {
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			return err
		}
		if !bundle && !info.Mode().IsRegular() {
			return nil
		}
		var targetPath any
		var reason any
		restorable := false
		_, source, target, _, resolveErr := c.configBackupSource(ctx, name, snapshot)
		if resolveErr != nil {
			reason = resolveErr.Error()
		} else {
			relative, err := filepath.Rel(c.Home, target.path)
			if err != nil || !filepath.IsLocal(relative) {
				return invalid("配置备份目标超出数据目录")
			}
			targetPath = filepath.ToSlash(relative)
			payload, err := os.Lstat(source)
			if err != nil {
				reason = "备份内容缺失或无法读取"
			} else if !payload.Mode().IsRegular() || payload.Size() > maxTextFile {
				reason = "备份内容不是普通文本文件或超过大小限制"
			} else {
				info = payload
				path = source
				restorable = true
			}
		}
		out = append(out, map[string]any{"name": name, "path": Path(path), "sizeBytes": info.Size(), "modifiedAt": info.ModTime().UnixMilli(), "targetPath": targetPath, "restorable": restorable, "reason": reason})
		if bundle {
			return filepath.SkipDir
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Slice(out, func(i, j int) bool {
		a, b := out[i]["modifiedAt"].(int64), out[j]["modifiedAt"].(int64)
		if a == b {
			return out[i]["name"].(string) < out[j]["name"].(string)
		}
		return a > b
	})
	return out, nil
}
