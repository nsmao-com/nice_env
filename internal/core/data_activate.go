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
	"strconv"
	"strings"
	"time"

	"github.com/shirou/gopsutil/v4/process"
	managedprocess "niceenv/internal/process"
)

type dataDirRedirect struct {
	Target Path   `json:"target"`
	Token  string `json:"token"`
}
type dataDirCopy struct {
	Source Path   `json:"source"`
	Target Path   `json:"target"`
	Token  string `json:"token"`
}

// The old root remains intact. Future launches resolve its explicit redirect,
// including launches with the previous command-line data-home argument.
func resolveDataDirectory(home string) (string, error) {
	seen := map[string]bool{}
	for i := 0; i < 16; i++ {
		if seen[home] {
			return "", invalid("数据目录转向形成循环")
		}
		seen[home] = true
		body, err := readPrivateMaterial(filepath.Join(home, ".niceenv-data-redirect.json"))
		if os.IsNotExist(err) {
			return home, nil
		}
		if err != nil {
			return "", err
		}
		var redirect dataDirRedirect
		if json.Unmarshal(body, &redirect) != nil || redirect.Token == "" || !filepath.IsAbs(string(redirect.Target)) {
			return "", invalid("数据目录转向记录无效")
		}
		marker, err := readPrivateMaterial(filepath.Join(string(redirect.Target), ".niceenv-move-copy.json"))
		if err != nil {
			return "", invalid("新的数据目录不存在或复制凭据丢失，原数据仍保留")
		}
		var copy dataDirCopy
		if json.Unmarshal(marker, &copy) != nil || copy.Token != redirect.Token || string(copy.Source) != home || copy.Target != redirect.Target {
			return "", invalid("新数据目录身份与切换记录不一致")
		}
		home = string(redirect.Target)
	}
	return "", invalid("数据目录转向层级过多")
}

func (c *Core) PrepareDataDirectoryActivation(ctx context.Context, target string) (func() error, error) {
	c.dataDir.mu.Lock()
	defer c.dataDir.mu.Unlock()
	if c.dataDir.activating {
		return nil, dataDirBusy()
	}
	pending := c.dataDir.pending
	if pending == nil || string(pending.Result.Path) != DisplayPath(target) {
		return nil, invalid("此目录没有完成迁移准备")
	}
	if err := validateMoveDirectory(target); err != nil {
		return nil, err
	}
	unl, err := managedprocess.Lock(filepath.Join(target, ".niceenv-wails-control.lock"))
	if err != nil {
		return nil, invalid("迁移目标正被另一个进程使用")
	}
	defer unl()
	marker, err := readPrivateMaterial(filepath.Join(target, ".niceenv-move-copy.json"))
	if err != nil {
		return nil, err
	}
	var copy dataDirCopy
	if json.Unmarshal(marker, &copy) != nil || copy.Source != pending.Source || copy.Target != pending.Result.Path || copy.Token != pending.Token {
		return nil, invalid("迁移副本身份已变化，未切换数据目录")
	}
	if err = filepath.WalkDir(target, func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if err := ctx.Err(); err != nil {
			return err
		}
		if entry.Type()&(os.ModeSymlink|os.ModeIrregular) != 0 {
			return invalid("迁移副本包含链接或特殊文件")
		}
		if entry.IsDir() {
			return nil
		}
		rel, err := filepath.Rel(target, path)
		if err != nil {
			return err
		}
		if rel == ".niceenv-move-copy.json" || rel == ".niceenv-wails-control.lock" {
			return nil
		}
		if _, ok := pending.Manifest[filepath.ToSlash(rel)]; !ok {
			return &AppError{Code: "DATA_DIR_COPY_CHANGED", Message: "迁移副本新增了未校验文件，未切换数据目录", Detail: DisplayPath(path)}
		}
		return nil
	}); err != nil {
		return nil, err
	}
	for rel, digest := range pending.Manifest {
		if !filepath.IsLocal(filepath.FromSlash(rel)) {
			return nil, invalid("迁移副本清单路径无效")
		}
		path := filepath.Join(target, filepath.FromSlash(rel))
		if err = validateMoveDirectory(filepath.Dir(path)); err != nil {
			return nil, err
		}
		info, err := os.Lstat(path)
		if err != nil {
			return nil, err
		}
		if !info.Mode().IsRegular() {
			return nil, invalid("迁移副本包含非普通文件")
		}
		f, err := os.Open(path)
		if err != nil {
			return nil, err
		}
		hash := sha256.New()
		_, err = io.Copy(hash, &moveReader{ctx: ctx, r: f})
		err = errors.Join(err, f.Close())
		if err != nil {
			return nil, err
		}
		if hex.EncodeToString(hash.Sum(nil)) != digest {
			return nil, &AppError{Code: "DATA_DIR_COPY_CHANGED", Message: "迁移副本已被修改，未切换数据目录", Detail: DisplayPath(path)}
		}
	}
	redirect := dataDirRedirect{Target: Path(target), Token: pending.Token}
	body, _ := json.Marshal(redirect)
	path := filepath.Join(c.Home, ".niceenv-data-redirect.json")
	if _, err = os.Lstat(path); err == nil {
		return nil, invalid("当前数据目录已经存在切换记录")
	} else if !os.IsNotExist(err) {
		return nil, err
	}
	if err = writePrivateAtomic(path, body); err != nil {
		return nil, err
	}
	c.dataDir.activating = true
	return func() error {
		c.dataDir.mu.Lock()
		defer c.dataDir.mu.Unlock()
		current, err := readPrivateMaterial(path)
		if err != nil {
			return err
		}
		if string(current) != string(body) {
			return invalid("数据目录切换记录发生变化，未覆盖")
		}
		if err = os.Remove(path); err != nil {
			return err
		}
		c.dataDir.activating = false
		return nil
	}, nil
}

// The browser profile is copied only after the old application has exited and
// released its profile/database handles. This retains local conversation history.
func FinishDataDirectoryActivation(ctx context.Context, home string) error {
	marker, err := readPrivateMaterial(filepath.Join(home, ".niceenv-move-copy.json"))
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		return err
	}
	var copy dataDirCopy
	if json.Unmarshal(marker, &copy) != nil || string(copy.Target) != home || copy.Token == "" {
		return invalid("数据目录复制凭据无效")
	}
	done := filepath.Join(home, ".niceenv-move-activated")
	if body, err := os.ReadFile(done); err == nil && string(body) == copy.Token {
		return nil
	}
	source := string(copy.Source)
	redirect, err := readPrivateMaterial(filepath.Join(source, ".niceenv-data-redirect.json"))
	if err != nil {
		return invalid("此目录是尚未激活的迁移副本，请从原应用完成切换")
	}
	var selected dataDirRedirect
	if json.Unmarshal(redirect, &selected) != nil || selected.Target != copy.Target || selected.Token != copy.Token {
		return invalid("迁移副本未被选为当前数据目录")
	}
	unlock, err := managedprocess.Lock(filepath.Join(source, ".niceenv-wails-control.lock"))
	if err != nil {
		return invalid("原应用尚未退出，不能切换数据目录")
	}
	defer unlock()
	// WebView2 renderer/storage processes can outlive the desktop process briefly.
	// Wait for those exact profile users to release SQLite/LevelDB handles.
	profileCtx, profileCancel := context.WithTimeout(ctx, time.Minute)
	err = waitForBrowserProfiles(profileCtx, source)
	profileCancel()
	if err != nil {
		return err
	}
	for _, name := range []string{"webview", "webview2"} {
		src, dest := filepath.Join(source, name), filepath.Join(home, name)
		if _, err = os.Stat(src); os.IsNotExist(err) {
			continue
		} else if err != nil {
			return err
		}
		if err = validateMoveDirectory(src); err != nil {
			return err
		}
		if _, err = os.Lstat(dest); err == nil {
			if receipt, readErr := readPrivateMaterial(filepath.Join(dest, ".niceenv-move-profile")); readErr == nil && string(receipt) == copy.Token {
				continue
			}
			return invalid("目标浏览器数据已存在，未覆盖会话记录")
		} else if !os.IsNotExist(err) {
			return err
		}
		stage, err := os.MkdirTemp(home, ".webview-moving-")
		if err != nil {
			return err
		}
		err = filepath.WalkDir(src, func(p string, d fs.DirEntry, err error) error {
			if err != nil {
				return err
			}
			rel, err := filepath.Rel(src, p)
			if err != nil || rel == "." {
				return err
			}
			if err = ctx.Err(); err != nil {
				return err
			}
			info, err := os.Lstat(p)
			if err != nil {
				return err
			}
			if info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
				return invalid("浏览器数据包含链接，原会话已保留")
			}
			to := filepath.Join(stage, rel)
			if d.IsDir() {
				return os.Mkdir(to, 0700)
			}
			if !info.Mode().IsRegular() {
				return invalid("浏览器数据包含特殊文件")
			}
			input, err := os.Open(p)
			if err != nil {
				return err
			}
			defer input.Close()
			output, err := os.OpenFile(to, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
			if err != nil {
				return err
			}
			if err = managedprocess.PrivateFile(to); err != nil {
				output.Close()
				return err
			}
			_, err = io.Copy(output, &moveReader{ctx: ctx, r: input})
			return errors.Join(err, output.Sync(), output.Close())
		})
		if err != nil {
			return err
		}
		if err = writePrivateAtomic(filepath.Join(stage, ".niceenv-move-profile"), []byte(copy.Token)); err != nil {
			return err
		}
		if err = os.Rename(stage, dest); err != nil {
			return err
		}
	}
	return writePrivateAtomic(done, []byte(copy.Token))
}

func waitForBrowserProfiles(ctx context.Context, home string) error {
	for {
		processes, err := process.ProcessesWithContext(ctx)
		if err != nil {
			return err
		}
		busy := false
		for _, p := range processes {
			name, err := p.NameWithContext(ctx)
			if err != nil || !strings.EqualFold(name, "msedgewebview2.exe") {
				continue
			}
			args, err := p.CmdlineSliceWithContext(ctx)
			if err != nil {
				return err
			}
			for _, arg := range args {
				if value, ok := strings.CutPrefix(arg, "--user-data-dir="); ok {
					value = strings.Trim(value, "\"")
					if pathWithin(filepath.Join(home, "webview"), value) || pathWithin(filepath.Join(home, "webview2"), value) {
						busy = true
					}
				}
			}
		}
		if !busy {
			return nil
		}
		select {
		case <-ctx.Done():
			return errors.New("原窗口的浏览器进程尚未释放会话目录，请稍后重试")
		case <-time.After(300 * time.Millisecond):
		}
	}
}

func WaitForRestartParent(ctx context.Context, pid int) error {
	if pid <= 0 {
		return nil
	}
	parent, err := process.NewProcessWithContext(ctx, int32(pid))
	if err != nil {
		return nil
	}
	created, err := parent.CreateTimeWithContext(ctx)
	if err != nil {
		return err
	}
	timer := time.NewTicker(200 * time.Millisecond)
	defer timer.Stop()
	for {
		running, err := parent.IsRunningWithContext(ctx)
		if err != nil || !running {
			return err
		}
		current, err := parent.CreateTimeWithContext(ctx)
		if err != nil {
			return err
		}
		if current != created {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-timer.C:
		}
	}
}

func RestartArguments(args []string, home string, parent int) []string {
	result := []string{}
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if arg == "--restart-supervisor" || strings.HasPrefix(arg, "--restart-supervisor=") {
			continue
		}
		if arg == "--niceenv-data-home" || arg == "--restart-parent" || arg == "--restart-source" {
			i++
			continue
		}
		if strings.HasPrefix(arg, "--niceenv-data-home=") || strings.HasPrefix(arg, "--restart-parent=") || strings.HasPrefix(arg, "--restart-source=") {
			continue
		}
		result = append(result, arg)
	}
	return append(result, "--niceenv-data-home", home, "--restart-parent", strconv.Itoa(parent))
}

// Recovery only removes a redirect that still matches the prepared copy. Both
// controllers must be gone; the original source data is never overwritten.
func RollbackDataDirectoryActivation(source, target string) error {
	if err := validateMoveDirectory(source); err != nil {
		return err
	}
	if err := validateMoveDirectory(target); err != nil {
		return err
	}
	sourceUnlock, err := managedprocess.Lock(filepath.Join(source, ".niceenv-wails-control.lock"))
	if err != nil {
		return err
	}
	defer sourceUnlock()
	targetUnlock, err := managedprocess.Lock(filepath.Join(target, ".niceenv-wails-control.lock"))
	if err != nil {
		return err
	}
	defer targetUnlock()
	body, err := readPrivateMaterial(filepath.Join(source, ".niceenv-data-redirect.json"))
	if err != nil {
		return err
	}
	var redirect dataDirRedirect
	if json.Unmarshal(body, &redirect) != nil || string(redirect.Target) != target || redirect.Token == "" {
		return invalid("切换记录发生变化，未自动回退")
	}
	body, err = readPrivateMaterial(filepath.Join(target, ".niceenv-move-copy.json"))
	if err != nil {
		return err
	}
	var copy dataDirCopy
	if json.Unmarshal(body, &copy) != nil || string(copy.Source) != source || copy.Target != redirect.Target || copy.Token != redirect.Token {
		return invalid("迁移副本身份发生变化，未自动回退")
	}
	if err = os.Remove(filepath.Join(target, ".niceenv-move-activated")); err != nil && !os.IsNotExist(err) {
		return err
	}
	return os.Remove(filepath.Join(source, ".niceenv-data-redirect.json"))
}
