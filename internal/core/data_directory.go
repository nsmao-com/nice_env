package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"

	"niceenv/internal/process"
)

type dataDirMigration struct {
	Path           Path  `json:"path"`
	Files          int64 `json:"files"`
	Bytes          int64 `json:"bytes"`
	RewrittenFiles int64 `json:"rewrittenFiles"`
}

const maxMoveManifest = 128 * 1024 * 1024

type dataDirPending struct {
	Result   dataDirMigration  `json:"result"`
	Source   Path              `json:"source"`
	Token    string            `json:"token"`
	Manifest map[string]string `json:"manifest"`
}
type dataDirManager struct {
	activity   sync.RWMutex
	mu         sync.Mutex
	frozen     atomic.Bool
	pending    *dataDirPending
	activating bool
}

func dataDirBusy() error {
	return &AppError{Code: "DATA_DIR_BUSY", Message: "数据目录正在迁移或等待切换，请先完成迁移或取消"}
}
func (c *Core) beginDataActivity() bool {
	if c.dataDir.frozen.Load() || !c.dataDir.activity.TryRLock() {
		return false
	}
	if c.dataDir.frozen.Load() {
		c.dataDir.activity.RUnlock()
		return false
	}
	return true
}
func (c *Core) DataDirectoryActivity() (func(), error) {
	if !c.beginDataActivity() {
		return nil, dataDirBusy()
	}
	return c.dataDir.activity.RUnlock, nil
}

func (c *Core) PrepareRestart(ctx context.Context) (func() error, error) {
	if !c.dataDir.mu.TryLock() {
		return nil, dataDirBusy()
	}
	defer c.dataDir.mu.Unlock()
	if c.dataDir.pending != nil || c.dataDir.frozen.Load() {
		return nil, dataDirBusy()
	}
	if !c.dataDir.activity.TryLock() {
		return nil, &AppError{Code: "RESTART_BUSY", Message: "仍有操作正在运行，请完成后重启"}
	}
	c.dataDir.frozen.Store(true)
	c.dataDir.activity.Unlock()
	rollback := func() error { c.dataDir.frozen.Store(false); c.CancelQuit(); return nil }
	if err := c.PrepareQuit(ctx); err != nil {
		_ = rollback()
		return nil, err
	}
	return rollback, nil
}

func (c *Core) registerDataDirectory() {
	c.handle("migrate_data_dir", func(ctx context.Context, raw json.RawMessage) (any, error) {
		in, err := decode[struct {
			Path string `json:"path"`
		}](raw)
		if err != nil {
			return nil, err
		}
		return c.migrateDataDirectory(ctx, in.Path)
	})
	c.handle("pending_data_dir_migration", func(context.Context, json.RawMessage) (any, error) {
		c.dataDir.mu.Lock()
		defer c.dataDir.mu.Unlock()
		if c.dataDir.pending == nil {
			return nil, nil
		}
		return c.dataDir.pending.Result, nil
	})
	c.handle("cancel_data_dir_migration", func(context.Context, json.RawMessage) (any, error) {
		if !c.dataDir.mu.TryLock() {
			return nil, dataDirBusy()
		}
		defer c.dataDir.mu.Unlock()
		if c.dataDir.activating {
			return nil, dataDirBusy()
		}
		if c.dataDir.pending == nil {
			return true, nil
		}
		if err := os.Remove(filepath.Join(c.Home, ".niceenv-move-pending.json")); err != nil && !os.IsNotExist(err) {
			return nil, err
		}
		c.dataDir.pending = nil
		c.dataDir.frozen.Store(false)
		c.CancelQuit()
		return true, nil
	})
}
func (c *Core) loadPendingDataDirectory() error {
	data, err := readPrivateMaterialLimit(filepath.Join(c.Home, ".niceenv-move-pending.json"), maxMoveManifest)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	var p dataDirPending
	if json.Unmarshal(data, &p) != nil || p.Token == "" || string(p.Source) != c.Home || !filepath.IsAbs(string(p.Result.Path)) {
		return invalid("待切换的数据目录记录损坏")
	}
	c.dataDir.pending = &p
	c.dataDir.frozen.Store(true)
	return nil
}
func pathWithin(root, p string) bool {
	rel, err := filepath.Rel(root, p)
	return err == nil && (rel == "." || filepath.IsLocal(rel))
}
func (c *Core) migrateDataDirectory(ctx context.Context, raw string) (dataDirMigration, error) {
	var empty dataDirMigration
	if !c.dataDir.mu.TryLock() {
		return empty, dataDirBusy()
	}
	defer c.dataDir.mu.Unlock()
	if c.dataDir.pending != nil || c.dataDir.frozen.Load() {
		return empty, dataDirBusy()
	}
	if !c.dataDir.activity.TryLock() {
		return empty, &AppError{Code: "DATA_DIR_BUSY", Message: "仍有操作正在运行，请完成后再迁移数据目录"}
	}
	defer c.dataDir.activity.Unlock()
	_, err := certOutputPath(DisplayPath(raw)+string(filepath.Separator)+".niceenv-path-check", false)
	if err != nil {
		return empty, err
	}
	target := filepath.Clean(DisplayPath(raw))
	if !filepath.IsAbs(target) || filepath.Dir(target) == target || pathWithin(c.Home, target) || pathWithin(target, c.Home) {
		return empty, invalid("迁移目标不能是当前目录、子目录或上级目录")
	}
	// certOutputPath validates file outputs; directory selection is separately
	// checked here, including every ancestor, without resolving away junctions.
	if err = validateMoveDirectory(target); err != nil {
		return empty, err
	}
	if err = validateMoveDirectory(c.Home); err != nil {
		return empty, err
	}
	if entries, readErr := os.ReadDir(target); readErr == nil && len(entries) != 0 {
		return empty, invalid("迁移目标目录必须为空")
	} else if readErr != nil && !os.IsNotExist(readErr) {
		return empty, readErr
	}
	c.ollama.mu.Lock()
	pulling := c.ollama.active()
	c.ollama.mu.Unlock()
	if pulling {
		return empty, invalid("请先完成或取消模型下载，再迁移目录")
	}
	if err = c.PrepareQuit(ctx); err != nil {
		return empty, err
	}
	committed := false
	defer func() {
		if !committed {
			c.CancelQuit()
		}
	}()
	c.services.wg.Wait()
	c.tunnels.wg.Wait()
	parent := filepath.Dir(target)
	if err = os.MkdirAll(parent, 0700); err != nil {
		return empty, err
	}
	if err = validateMoveDirectory(parent); err != nil {
		return empty, err
	}
	stage, err := os.MkdirTemp(parent, ".niceenv-moving-")
	if err != nil {
		return empty, err
	}
	// Keep a failed staging copy for diagnosis; it is never selected as live.
	result := dataDirMigration{Path: Path(target)}
	pending := dataDirPending{Result: result, Source: Path(c.Home), Manifest: map[string]string{}}
	pending.Token, err = newID("move")
	if err != nil {
		return empty, err
	}
	err = c.copyDataTree(ctx, stage, &pending)
	if err != nil {
		return empty, &AppError{Code: "DATA_DIR_COPY_FAILED", Message: "迁移复制未完成，当前目录继续使用", Hint: "未完成的副本：" + DisplayPath(stage), Detail: err.Error()}
	}
	if err = c.snapshotDataDirectory(ctx, stage, target); err != nil {
		return empty, err
	}
	protected, err := c.rebaseSftpgoData(ctx, stage, target)
	if err != nil {
		return empty, err
	}
	rewritten, err := rebaseDataConfigs(ctx, stage, c.Home, target, protected...)
	if err != nil {
		return empty, err
	}
	pending.Result.RewrittenFiles = rewritten
	// Keep backup bytes immutable. Previews rebase only recognised path fields
	// using this v1-compatible history, after verifying the original digest.
	history, err := readDataPathHistory(c.Home)
	if err != nil {
		return empty, err
	}
	found := false
	for _, old := range history {
		if old == c.Home {
			found = true
			break
		}
	}
	if !found {
		history = append(history, c.Home)
	}
	if len(history) > 100 {
		return empty, invalid("数据目录历史超过 100 项，请先归档历史配置备份")
	}
	historyJSON, err := json.Marshal(history)
	if err != nil {
		return empty, err
	}
	if err = writePrivateAtomic(filepath.Join(stage, ".data-dir-history.json"), historyJSON); err != nil {
		return empty, err
	}
	if err = hashMoveTree(ctx, stage, &pending); err != nil {
		return empty, err
	}
	encoded, err := json.Marshal(pending)
	if err != nil {
		return empty, err
	}
	if len(encoded) > maxMoveManifest {
		return empty, invalid("迁移校验清单超过 128 MiB，原目录继续使用")
	}
	marker, _ := json.Marshal(map[string]any{"source": Path(c.Home), "target": Path(target), "token": pending.Token})
	if err = writePrivateAtomic(filepath.Join(stage, ".niceenv-move-copy.json"), marker); err != nil {
		return empty, err
	}
	if err = os.Remove(target); err != nil && !os.IsNotExist(err) {
		return empty, err
	}
	if err = os.Rename(stage, target); err != nil {
		return empty, err
	}
	if err = writePrivateAtomic(filepath.Join(c.Home, ".niceenv-move-pending.json"), encoded); err != nil {
		return empty, err
	}
	c.dataDir.pending = &pending
	c.dataDir.frozen.Store(true)
	committed = true
	if c.emit != nil {
		c.emit("data-dir://prepared", pending.Result)
	}
	return pending.Result, nil
}
func validateMoveDirectory(p string) error {
	for current := p; ; current = filepath.Dir(current) {
		info, err := os.Lstat(current)
		if err == nil {
			if !info.IsDir() || info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
				return invalid("数据目录不能经过文件、软链接或目录联接")
			}
		} else if !os.IsNotExist(err) {
			return err
		}
		if filepath.Dir(current) == current {
			break
		}
	}
	return nil
}
func skipMoveFile(rel string) bool {
	first, _, _ := strings.Cut(filepath.ToSlash(rel), "/")
	switch first {
	case "webview", "webview2", "service-locks", "cron-locks", "certauto-locks":
		return true
	}
	if strings.Contains(filepath.ToSlash(rel), "/") {
		return false
	}
	switch rel {
	case "nsb.sqlite", "nsb.sqlite-wal", "nsb.sqlite-shm", "nsb.sqlite-journal", ".niceenv-bootstrap.lock", ".niceenv-wails-control.lock", ".niceenv-wails-control.json", ".niceenv-controller-launch.lock", ".niceenv-control.lock", ".niceenv-control.json", ".data-dir-activity.lock", ".data-dir-authority.json", ".data-dir-activation.json", ".niceenv-move-pending.json", ".niceenv-move-copy.json", ".niceenv-data-redirect.json":
		return true
	}
	return false
}
func (c *Core) copyDataTree(ctx context.Context, stage string, pending *dataDirPending) error {
	return filepath.WalkDir(c.Home, func(p string, d fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if err := ctx.Err(); err != nil {
			return err
		}
		rel, err := filepath.Rel(c.Home, p)
		if err != nil {
			return err
		}
		if rel == "." {
			return nil
		}
		if skipMoveFile(rel) {
			if d.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		info, err := os.Lstat(p)
		if err != nil {
			return err
		}
		if info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
			return invalid("迁移目录含链接或特殊文件：" + DisplayPath(p))
		}
		to := filepath.Join(stage, rel)
		if info.IsDir() {
			return os.Mkdir(to, 0700)
		}
		if !info.Mode().IsRegular() {
			return invalid("无法迁移特殊文件：" + DisplayPath(p))
		}
		input, err := os.Open(p)
		if err != nil {
			return err
		}
		defer input.Close()
		output, err := os.OpenFile(to, os.O_WRONLY|os.O_CREATE|os.O_EXCL, info.Mode().Perm())
		if err != nil {
			return err
		}
		if err = process.PrivateFile(to); err != nil {
			output.Close()
			return err
		}
		n, copyErr := io.Copy(output, &moveReader{ctx: ctx, r: input})
		err = errors.Join(copyErr, output.Sync(), output.Close())
		if err != nil {
			return err
		}
		after, err := input.Stat()
		if err != nil {
			return err
		}
		if n != info.Size() || after.Size() != info.Size() || !after.ModTime().Equal(info.ModTime()) {
			return invalid("复制时源文件发生变化：" + DisplayPath(p))
		}
		return nil
	})
}

type moveReader struct {
	ctx context.Context
	r   io.Reader
}

func (r *moveReader) Read(p []byte) (int, error) {
	if err := r.ctx.Err(); err != nil {
		return 0, err
	}
	return r.r.Read(p)
}
func hashMoveTree(ctx context.Context, root string, p *dataDirPending) error {
	p.Result.Files = 0
	p.Result.Bytes = 0
	return filepath.WalkDir(root, func(name string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			return nil
		}
		rel, err := filepath.Rel(root, name)
		if err != nil {
			return err
		}
		f, err := os.Open(name)
		if err != nil {
			return err
		}
		h := sha256.New()
		n, err := io.Copy(h, &moveReader{ctx: ctx, r: f})
		closeErr := f.Close()
		if err != nil || closeErr != nil {
			return errors.Join(err, closeErr)
		}
		p.Manifest[filepath.ToSlash(rel)] = hex.EncodeToString(h.Sum(nil))
		p.Result.Files++
		p.Result.Bytes += n
		return nil
	})
}
