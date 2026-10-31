package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"niceenv/internal/process"
)

type installTask struct {
	request    string
	cancel     context.CancelFunc
	committing bool
}
type installManager struct {
	mu      sync.Mutex
	tasks   map[string]*installTask
	closing bool
	wg      sync.WaitGroup
}

func (c *Core) registerInstaller() {
	c.installer.tasks = map[string]*installTask{}
	c.handle("reconcile_installed_packages", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.reconcilePackages(ctx) })
	c.handle("install_package", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			ID        string  `json:"id"`
			RequestID *string `json:"requestId"`
		}](data)
		if err != nil {
			return nil, err
		}
		request := ""
		if args.RequestID != nil {
			request = *args.RequestID
			if !cronID.MatchString(request) {
				return nil, invalid("安装请求标识无效")
			}
		}
		return c.installPackage(ctx, args.ID, request)
	})
	c.handle("cancel_download", func(_ context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			TaskID    string  `json:"taskId"`
			RequestID *string `json:"requestId"`
		}](data)
		if err != nil {
			return nil, err
		}
		c.installer.mu.Lock()
		defer c.installer.mu.Unlock()
		var matched *installTask
		for key, task := range c.installer.tasks {
			if strings.SplitN(key, "@", 2)[0] != strings.SplitN(args.TaskID, "@", 2)[0] {
				continue
			}
			if args.RequestID != nil {
				if task.request != *args.RequestID {
					continue
				}
			} else {
				if task.request != "" {
					continue
				}
				if strings.Contains(args.TaskID, "@") && key != args.TaskID {
					continue
				}
			}
			if matched != nil {
				return false, nil
			}
			matched = task
		}
		if matched == nil || matched.committing {
			return false, nil
		}
		matched.cancel()
		return true, nil
	})
}
func (c *Core) closeInstaller() {
	c.installer.mu.Lock()
	c.installer.closing = true
	for _, task := range c.installer.tasks {
		if !task.committing {
			task.cancel()
		}
	}
	c.installer.mu.Unlock()
	c.installer.wg.Wait()
}

func (c *Core) findPackage(key string) (map[string]any, error) {
	id, version, exact := strings.Cut(key, "@")
	if !safePart(id) || exact && !safePart(version) {
		return nil, invalid("套件标识无效")
	}
	var selected map[string]any
	for _, entry := range c.catalog.Packages {
		if entry["id"] != id {
			continue
		}
		v, _ := entry["version"].(string)
		if exact && canonicalVersion(v) != canonicalVersion(version) {
			continue
		}
		if selected == nil || newerVersion(v, selected["version"].(string)) {
			selected = entry
		}
	}
	if selected == nil {
		return nil, &AppError{Code: "PACKAGE_NOT_FOUND", Message: "套件版本不在当前清单中，请刷新版本目录"}
	}
	return selected, nil
}
func (c *Core) installProgress(key, request, state string, received, total, speed int64, message string) {
	if c.emit == nil {
		return
	}
	eta := float64(0)
	if speed > 0 && total > received {
		eta = float64(total-received) / float64(speed)
	}
	event := map[string]any{"taskId": key, "received": received, "total": total, "speedBps": speed, "etaSec": eta, "state": state}
	if request != "" {
		event["requestId"] = request
	}
	if message != "" {
		event["error"] = message
	}
	c.emit("download://progress", event)
}

func (c *Core) installPackage(parent context.Context, key, request string) (result any, resultErr error) {
	entry, err := c.resolveInstallPackage(parent, key)
	if err != nil {
		return nil, err
	}
	bytes, err := json.Marshal(entry)
	if err != nil {
		return nil, err
	}
	var pkg struct {
		Package
		URL       string `json:"url"`
		Kind      string `json:"kind"`
		SHA256    string `json:"sha256"`
		SizeBytes int64  `json:"sizeBytes"`
	}
	if err = json.Unmarshal(bytes, &pkg); err != nil {
		return nil, err
	}
	if !safePart(pkg.Version) {
		return nil, invalid("套件版本无效")
	}
	if pkg.Kind != "archive" && pkg.Kind != "targz" && pkg.Kind != "binary" && pkg.Kind != "sevenzip" {
		return nil, &AppError{Code: "ARCHIVE_FORMAT_PENDING", Message: "此压缩格式尚未接入 Go 解压器"}
	}
	// Qdrant UI assets require their dedicated fetcher.
	if pkg.ID == "qdrant" {
		return nil, &AppError{Code: "PACKAGE_ADAPTER_PENDING", Message: "此套件的完整性或附属资源安装流程尚在迁移"}
	}
	key = pkg.ID + "@" + pkg.Version
	ctx, cancel := context.WithTimeout(parent, 30*time.Minute)
	defer cancel()
	task := &installTask{request: request, cancel: cancel}
	c.installer.mu.Lock()
	if c.installer.closing {
		c.installer.mu.Unlock()
		return nil, context.Canceled
	}
	if len(c.installer.tasks) >= 3 {
		c.installer.mu.Unlock()
		return nil, &AppError{Code: "DOWNLOAD_BUSY", Message: "已有三个安装任务，请等待完成后继续"}
	}
	for oldKey, old := range c.installer.tasks {
		if oldKey == key || request != "" && old.request == request {
			c.installer.mu.Unlock()
			return nil, &AppError{Code: "PACKAGE_BUSY", Message: "此安装请求仍在执行"}
		}
	}
	c.installer.tasks[key] = task
	c.installer.wg.Add(1)
	c.installer.mu.Unlock()
	defer func() {
		if resultErr != nil {
			state := "error"
			if errors.Is(resultErr, context.Canceled) {
				state = "cancelled"
			}
			c.installProgress(key, request, state, 0, 0, 0, publicError(resultErr).Message)
		}
		c.installer.mu.Lock()
		delete(c.installer.tasks, key)
		c.installer.mu.Unlock()
		c.installer.wg.Done()
	}()
	locks := filepath.Join(c.Home, "install-locks")
	if err = managedDirectory(c.Home, locks); err != nil {
		return nil, err
	}
	unlock, err := process.Lock(filepath.Join(locks, key+".lock"))
	if errors.Is(err, process.ErrLocked) {
		return nil, &AppError{Code: "PACKAGE_BUSY", Message: "另一实例正在安装此套件"}
	}
	if err != nil {
		return nil, err
	}
	defer unlock()
	var count int
	if err = c.Store.db.QueryRowContext(ctx, "SELECT COUNT(*) FROM installed WHERE id=? AND version=?", pkg.ID, pkg.Version).Scan(&count); err != nil {
		return nil, err
	}
	if count > 0 {
		return nil, &AppError{Code: "PACKAGE_ALREADY_INSTALLED", Message: "此版本已经安装，请先检查现有安装"}
	}
	parentDir := filepath.Join(c.Home, "runtimes", pkg.ID)
	if err = managedDirectory(c.Home, parentDir); err != nil {
		return nil, err
	}
	destination := filepath.Join(parentDir, pkg.Version)
	if _, err = os.Lstat(destination); !os.IsNotExist(err) {
		if err != nil {
			return nil, err
		}
		return nil, &AppError{Code: "PACKAGE_DIRECTORY_EXISTS", Message: "目标版本目录已有文件，未覆盖；请先检查并备份"}
	}
	stage, err := os.MkdirTemp(parentDir, ".install-")
	if err != nil {
		return nil, err
	}
	defer func() {
		// Only this invocation's random staging directory may be removed.
		rel, checkErr := filepath.Rel(parentDir, stage)
		if checkErr == nil && filepath.IsLocal(rel) && !strings.ContainsAny(rel, `/\`) && strings.HasPrefix(rel, ".install-") {
			cleanupErr := os.RemoveAll(stage)
			if cleanupErr != nil && resultErr == nil {
				resultErr = cleanupErr
			}
		}
	}()
	archive := filepath.Join(stage, "download")
	if pkg.ID == "node" && pkg.SHA256 == "" {
		pkg.SHA256, err = nodeChecksum(ctx, canonicalVersion(pkg.Version), pkg.URL)
		if err != nil {
			return nil, err
		}
	}
	if err = c.downloadPackage(ctx, key, request, pkg.URL, pkg.SHA256, pkg.SizeBytes, archive); err != nil {
		return nil, err
	}
	if pkg.ID == "npm" || pkg.ID == "pnpm" {
		if err = verifyNPMArchive(ctx, pkg.ID, pkg.Version, pkg.URL, pkg.SHA256, archive); err != nil {
			return nil, err
		}
	}
	prepared := filepath.Join(stage, "package")
	if err = os.Mkdir(prepared, 0700); err != nil {
		return nil, err
	}
	c.installProgress(key, request, "extracting", 0, 0, 0, "")
	if err = extractPackage(ctx, archive, prepared, pkg.Kind, pkg.Entry, pkg.URL); err != nil {
		return nil, err
	}
	if pkg.ID == "npm" || pkg.ID == "pnpm" {
		if err = prepareJSCLI(pkg.ID, pkg.Version, pkg.URL, pkg.Entry, prepared); err != nil {
			return nil, err
		}
	}
	program, err := relativePath(prepared, pkg.Entry)
	if err != nil {
		return nil, err
	}
	info, err := os.Stat(program)
	if err != nil || !info.Mode().IsRegular() || info.Size() == 0 {
		return nil, &AppError{Code: "ENTRY_MISSING", Message: "解压后找不到清单声明的入口文件，未发布安装"}
	}
	if err = os.WriteFile(filepath.Join(prepared, ".niceenv-package.json"), bytes, 0600); err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	c.installer.mu.Lock()
	if err = ctx.Err(); err == nil {
		task.committing = true
	}
	c.installer.mu.Unlock()
	if err != nil {
		return nil, err
	}
	commitCtx, commitCancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer commitCancel()
	c.installProgress(key, request, "configuring", 0, 0, 0, "")
	configDir := filepath.Join(c.Home, "etc", pkg.ID, pkg.Version)
	if err = managedDirectory(c.Home, configDir); err != nil {
		return nil, err
	}
	activeVersion := pkg.Version
	views, err := c.packageViews(commitCtx)
	if err != nil {
		return nil, err
	}
	for _, view := range views {
		if view["id"] == pkg.ID && view["active"] == true && view["install"] != nil {
			activeVersion, _ = view["version"].(string)
			break
		}
	}
	tx, err := c.Store.db.BeginTx(commitCtx, nil)
	if err != nil {
		return nil, err
	}
	defer tx.Rollback()
	installedAt := time.Now().UnixMilli()
	if _, err = tx.ExecContext(commitCtx, "INSERT INTO installed(key,id,version,category,install_path,config_path,installed_at) VALUES(?,?,?,?,?,?,?)", key, pkg.ID, pkg.Version, pkg.Category, destination, configDir, installedAt); err != nil {
		return nil, err
	}
	if _, err = tx.ExecContext(commitCtx, "INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", "active"+pkg.ID+"Version", activeVersion); err != nil {
		return nil, err
	}
	if err = os.Rename(prepared, destination); err != nil {
		return nil, err
	}
	if err = tx.Commit(); err != nil {
		if rollbackErr := os.Rename(destination, prepared); rollbackErr != nil {
			return nil, &AppError{Code: "INSTALL_RECOVERY_REQUIRED", Message: "安装记录写入失败，程序目录已保留，请检查后重新登记"}
		}
		return nil, err
	}
	result = map[string]any{"id": pkg.ID, "version": pkg.Version, "category": pkg.Category, "installPath": Path(destination), "configPath": Path(configDir), "installedAt": installedAt}
	if err = c.refreshServices(commitCtx); err != nil {
		result.(map[string]any)["pathSyncError"] = &AppError{Code: "INSTALL_REFRESH_FAILED", Message: "套件已安装，但服务列表刷新失败，请重启应用", Detail: publicError(err).Message}
	}
	if pathErr := c.syncPath(commitCtx); pathErr != nil {
		detail := publicError(pathErr).Message
		if previous, ok := result.(map[string]any)["pathSyncError"].(*AppError); ok {
			detail = previous.Message + "；" + detail
		}
		result.(map[string]any)["pathSyncError"] = &AppError{Code: "INSTALL_PATH_SYNC_FAILED", Message: "套件已安装，但 PATH 同步失败，请在环境变量设置中重新应用", Detail: detail}
	}
	c.installProgress(key, request, "installed", 0, 0, 0, "")
	return result, nil
}

func (c *Core) downloadPackage(ctx context.Context, key, request, address, checksum string, expected int64, target string) error {
	u, err := url.Parse(address)
	if err != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil {
		return invalid("下载地址必须是完整且不含凭据的 HTTPS URL")
	}
	if checksum != "" {
		decoded, err := hex.DecodeString(checksum)
		if err != nil || len(decoded) != sha256.Size {
			return invalid("套件 SHA-256 校验值无效")
		}
		cached, err := c.copyVerifiedDownload(ctx, checksum, target)
		if err != nil {
			return err
		}
		if cached {
			return nil
		}
	}
	client := &http.Client{Timeout: 30 * time.Minute, CheckRedirect: func(req *http.Request, via []*http.Request) error {
		if len(via) > 5 || req.URL.Scheme != "https" || req.URL.User != nil {
			return invalid("下载重定向不安全或次数过多")
		}
		return nil
	}}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, address, nil)
	if err != nil {
		return err
	}
	req.Header.Set("User-Agent", "NiceEnv/Go-Wails")
	response, err := client.Do(req)
	if err != nil {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		return &AppError{Code: "DOWNLOAD_FAILED", Message: "无法连接套件下载地址"}
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return &AppError{Code: "DOWNLOAD_HTTP", Message: "下载服务器未返回成功状态", Detail: response.Status}
	}
	const limit int64 = 4 * 1024 * 1024 * 1024
	if response.ContentLength > limit {
		return &AppError{Code: "DOWNLOAD_LIMIT", Message: "下载文件超过 4 GiB 限制"}
	}
	total := max(int64(0), response.ContentLength)
	if total == 0 && expected > 0 {
		total = expected
	}
	file, err := os.OpenFile(target, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer file.Close()
	hash := sha256.New()
	buffer := make([]byte, 128*1024)
	received := int64(0)
	start, last := time.Now(), time.Time{}
	for {
		if err = ctx.Err(); err != nil {
			return err
		}
		n, readErr := response.Body.Read(buffer)
		if n > 0 {
			received += int64(n)
			if received > limit {
				return &AppError{Code: "DOWNLOAD_LIMIT", Message: "下载文件超过限制"}
			}
			if _, err = file.Write(buffer[:n]); err != nil {
				return err
			}
			_, _ = hash.Write(buffer[:n])
			if time.Since(last) >= 200*time.Millisecond {
				last = time.Now()
				speed := int64(float64(received) / max(.001, time.Since(start).Seconds()))
				c.installProgress(key, request, "downloading", received, total, speed, "")
			}
		}
		if errors.Is(readErr, io.EOF) {
			break
		}
		if readErr != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			return &AppError{Code: "DOWNLOAD_INTERRUPTED", Message: "套件下载中断，请重试"}
		}
	}
	if received == 0 || response.ContentLength >= 0 && received != response.ContentLength {
		return &AppError{Code: "DOWNLOAD_INCOMPLETE", Message: "下载文件为空或不完整"}
	}
	if checksum != "" && !strings.EqualFold(hex.EncodeToString(hash.Sum(nil)), checksum) {
		return &AppError{Code: "CHECKSUM_MISMATCH", Message: "套件 SHA-256 校验失败，未安装"}
	}
	if err = file.Sync(); err != nil {
		return err
	}
	if err = file.Close(); err != nil {
		return err
	}
	if checksum != "" && received <= 128*1024*1024 {
		// Cache failure must not invalidate a verified package already downloaded.
		_ = c.cacheVerifiedDownload(ctx, checksum, target)
	}
	return nil
}

func (c *Core) copyVerifiedDownload(ctx context.Context, checksum, target string) (bool, error) {
	cache := filepath.Join(c.Home, "downloads", strings.ToLower(checksum)+".cache")
	info, err := os.Lstat(cache)
	if os.IsNotExist(err) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	if !info.Mode().IsRegular() || info.Size() > 4*1024*1024*1024 {
		return false, nil
	}
	input, err := os.Open(cache)
	if err != nil {
		return false, err
	}
	defer input.Close()
	hash := sha256.New()
	if _, err = io.Copy(hash, contextReader{ctx, input}); err != nil {
		return false, err
	}
	if !strings.EqualFold(hex.EncodeToString(hash.Sum(nil)), checksum) {
		return false, nil
	}
	if _, err = input.Seek(0, io.SeekStart); err != nil {
		return false, err
	}
	output, err := os.OpenFile(target, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		return false, err
	}
	hash.Reset()
	_, copyErr := io.Copy(io.MultiWriter(output, hash), contextReader{ctx, input})
	closeErr := output.Close()
	if copyErr != nil || closeErr != nil {
		return false, errors.Join(copyErr, closeErr)
	}
	if !strings.EqualFold(hex.EncodeToString(hash.Sum(nil)), checksum) {
		return false, &AppError{Code: "CHECKSUM_MISMATCH", Message: "下载缓存读取期间发生变化，未安装"}
	}
	return true, nil
}
func (c *Core) cacheVerifiedDownload(ctx context.Context, checksum, source string) error {
	dir := filepath.Join(c.Home, "downloads")
	if err := managedDirectory(c.Home, dir); err != nil {
		return err
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return err
	}
	var size int64
	for _, entry := range entries {
		if strings.HasSuffix(entry.Name(), ".cache") {
			if info, err := entry.Info(); err == nil {
				size += info.Size()
			}
		}
	}
	if size >= 512*1024*1024 {
		return nil
	}
	input, err := os.Open(source)
	if err != nil {
		return err
	}
	defer input.Close()
	file, err := os.CreateTemp(dir, ".download-")
	if err != nil {
		return err
	}
	name := file.Name()
	defer os.Remove(name)
	_, copyErr := io.Copy(file, contextReader{ctx, input})
	closeErr := file.Close()
	if copyErr != nil || closeErr != nil {
		return errors.Join(copyErr, closeErr)
	}
	target := filepath.Join(dir, strings.ToLower(checksum)+".cache")
	if _, err = os.Lstat(target); err == nil {
		return nil
	} else if !os.IsNotExist(err) {
		return err
	}
	return os.Rename(name, target)
}
