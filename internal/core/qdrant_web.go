package core

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"time"
)

const qdrantWebDownloadURL = "https://github.com/qdrant/qdrant-web-ui/releases/download/v0.2.18/dist-qdrant.zip"
const qdrantWebSHA256 = "fdce24c04ec1627d2369cb8fe610ee06ad9236f82aad214aa7f294ac37372859"

// Runs within the package's install task; cancellation remains available until
// the final static directory is published. Existing assets are never replaced.
func (c *Core) prepareQdrantWeb(ctx context.Context, key, request, root string) (string, func(), error) {
	if err := managedDirectory(c.Home, root); err != nil {
		return "", nil, err
	}
	target := filepath.Join(root, "static")
	if _, err := os.Lstat(target); err == nil {
		return "", nil, &AppError{Code: "QDRANT_WEB_CHANGED", Message: "管理台目录已存在，未覆盖原文件"}
	} else if !os.IsNotExist(err) {
		return "", nil, err
	}
	stage, err := os.MkdirTemp(root, ".qdrant-web-")
	if err != nil {
		return "", nil, err
	}
	cleanup := func() { _ = os.RemoveAll(stage) }
	archive := filepath.Join(stage, "download.zip")
	if err := c.downloadPackage(ctx, key, request, qdrantWebDownloadURL, qdrantWebSHA256, 7189135, archive); err != nil {
		cleanup()
		return "", nil, err
	}
	prepared := filepath.Join(stage, "unpacked")
	if err := os.Mkdir(prepared, 0700); err != nil {
		cleanup()
		return "", nil, err
	}
	c.installProgress(key, request, "extracting", 0, 0, 0, "")
	if err := extractPackage(ctx, archive, prepared, "archive", "", qdrantWebDownloadURL); err != nil {
		cleanup()
		return "", nil, err
	}
	dist := filepath.Join(prepared, "dist")
	for _, name := range []string{"index.html", "assets", "openapi.json"} {
		info, err := os.Stat(filepath.Join(dist, name))
		if err != nil || name == "assets" && !info.IsDir() || name != "assets" && !info.Mode().IsRegular() {
			cleanup()
			return "", nil, invalid("Qdrant 官方管理台文件不完整")
		}
	}
	if err := ctx.Err(); err != nil {
		cleanup()
		return "", nil, err
	}
	return dist, cleanup, nil
}

func (c *Core) repairQdrantWeb(ctx context.Context, data json.RawMessage) (result any, resultErr error) {
	in, err := decode[struct {
		ID      string `json:"id"`
		Version string `json:"version"`
	}](data)
	if err != nil {
		return nil, err
	}
	if in.ID != "qdrant" || in.Version == "" {
		return nil, invalid("仅支持补齐所选 Qdrant 管理台")
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	e, err := c.serviceEntry(in.ID, in.Version)
	if err != nil {
		c.services.lifecycle.Unlock()
		return nil, err
	}
	c.services.mu.RLock()
	child, resolved, state, revision := e.child, e.resolved, e.status.State, e.revision
	c.services.mu.RUnlock()
	if child == nil || resolved == nil || state != "running" {
		c.services.lifecycle.Unlock()
		return nil, invalid("请先启动所选 Qdrant")
	}
	_, err = qdrantWebURL(resolved)
	var appErr *AppError
	if !errors.As(err, &appErr) || appErr.Code != "QDRANT_WEB_MISSING" {
		c.services.lifecycle.Unlock()
		return nil, invalid("当前管理台无需自动补齐，请检查自定义静态目录")
	}
	root := resolved.cwd
	key := e.pkg.ID + "@" + e.pkg.Version
	c.services.lifecycle.Unlock()
	ctx, cancel := context.WithTimeout(ctx, 10*time.Minute)
	defer cancel()
	task := &installTask{cancel: cancel}
	c.installer.mu.Lock()
	if c.installer.closing || len(c.installer.tasks) >= 3 || c.installer.tasks[key] != nil {
		c.installer.mu.Unlock()
		return nil, serviceBusy()
	}
	c.installer.tasks[key] = task
	c.installer.wg.Add(1)
	c.installer.mu.Unlock()
	defer func() {
		c.installer.mu.Lock()
		delete(c.installer.tasks, key)
		c.installer.mu.Unlock()
		c.installer.wg.Done()
		if resultErr != nil {
			state := "error"
			if ctx.Err() != nil {
				state = "cancelled"
			}
			c.installProgress(key, "", state, 0, 0, 0, publicError(resultErr).Message)
		}
	}()
	dist, cleanup, err := c.prepareQdrantWeb(ctx, key, "", root)
	if err != nil {
		return nil, err
	}
	defer cleanup()
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	c.services.mu.RLock()
	unchanged := e.child == child && e.resolved == resolved && e.status.State == "running" && e.revision == revision
	c.services.mu.RUnlock()
	if !unchanged {
		return nil, &AppError{Code: "SERVICE_CHANGED", Message: "Qdrant 状态已变化，未安装管理台或重启服务"}
	}
	c.installer.mu.Lock()
	err = ctx.Err()
	if err == nil {
		task.committing = true
	}
	c.installer.mu.Unlock()
	if err != nil {
		return nil, err
	}
	if _, err := os.Lstat(filepath.Join(root, "static")); !os.IsNotExist(err) {
		return nil, invalid("管理台目录已变化，未覆盖")
	}
	if err := os.Rename(dist, filepath.Join(root, "static")); err != nil {
		return nil, err
	}
	commitCtx, stop := context.WithTimeout(context.Background(), 90*time.Second)
	defer stop()
	if err := c.stopOwned(commitCtx, e, false); err != nil {
		return nil, err
	}
	if err := c.startService(commitCtx, e); err != nil {
		return nil, err
	}
	c.installProgress(key, "", "installed", 0, 0, 0, "")
	return qdrantWebURL(e.resolved)
}
