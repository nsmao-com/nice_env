package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
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

func (c *Core) previewConfigBackup(ctx context.Context, name string) (backupPreview, configTarget, error) {
	var empty backupPreview
	var noTarget configTarget
	name = strings.TrimPrefix(name, "config/")
	if filepath.Base(name) != name || strings.ContainsAny(name, `/\:`) || !strings.HasSuffix(name, ".bak") {
		return empty, noTarget, invalid("配置备份名称无效")
	}
	prefix, _, ok := strings.Cut(name, "-")
	if !ok {
		return empty, noTarget, invalid("配置备份缺少目标版本信息")
	}
	keyBytes, err := hex.DecodeString(prefix)
	if err != nil {
		return empty, noTarget, invalid("配置备份目标无效")
	}
	key := string(keyBytes)
	target, err := c.configTarget(ctx, key)
	if err != nil {
		return empty, noTarget, err
	}
	if key != target.key {
		return empty, noTarget, invalid("配置备份没有固定到准确的版本")
	}
	dir := filepath.Join(c.Home, "backup", "config")
	if err = managedDirectory(c.Home, dir); err != nil {
		return empty, noTarget, err
	}
	backup, err := readConfigText(filepath.Join(dir, name))
	if err != nil {
		return empty, noTarget, err
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
	signature, err := json.Marshal([]any{name, key, target.path, backup, currentPtr})
	if err != nil {
		return empty, noTarget, err
	}
	hash := sha256.Sum256(signature)
	return backupPreview{Name: "config/" + name, TargetPath: Path(target.path), TargetRelative: filepath.ToSlash(rel), CurrentExists: exists, Revision: hex.EncodeToString(hash[:]), BackupContent: &backup, CurrentContent: currentPtr, BackupSizeBytes: len(backup), CurrentSizeBytes: len(current), Changed: !exists || backup != current}, target, nil
}

func (c *Core) registerConfigRestore() {
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
