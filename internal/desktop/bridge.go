package desktop

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"sync/atomic"
	"time"
	"unicode"

	"github.com/wailsapp/wails/v3/pkg/application"
	"niceenv/internal/core"
)

type Bridge struct {
	app            *application.App
	home, version  string
	ready          chan struct{}
	backend        *core.Core
	err            error
	minimizeToTray atomic.Bool
	lifetime       context.Context
	quitting       atomic.Bool
	quitReady      atomic.Bool
	autostartMu    sync.Mutex
	mainWindow     *application.WebviewWindow
	trayPanel      *application.WebviewWindow
	tray           *application.SystemTray
	restartOnce    sync.Once
	restartErr     error
	cliOwner       bool
}

func NewBridge(home, version string, cliOwner bool) *Bridge {
	b := &Bridge{home: home, version: version, ready: make(chan struct{}), cliOwner: cliOwner}
	b.minimizeToTray.Store(true)
	return b
}

func (b *Bridge) ServiceStartup(ctx context.Context, _ application.ServiceOptions) error {
	b.lifetime = ctx
	b.app = application.Get()
	// SQLite initialisation and later service discovery never run on the UI thread.
	go func() {
		defer close(b.ready)
		b.backend, b.err = core.Open(ctx, b.home, b.version, func(name string, data any) { b.app.Event.Emit(name, data) })
		if b.err == nil {
			if settings, err := b.backend.Invoke(ctx, "get_settings", nil); err == nil {
				b.minimizeToTray.Store(settings.(map[string]any)["minimizeToTray"].(bool))
			}
		}
	}()
	return nil
}

func (b *Bridge) ServiceShutdown() error {
	<-b.ready
	if b.backend != nil {
		return b.backend.Close()
	}
	return nil
}

func (b *Bridge) Invoke(ctx context.Context, command string, args json.RawMessage) (result any, callErr error) {
	// Wails' HTTP transport encodes an untyped nil response as {}. Preserve
	// nullable API results (pending migration, optional records, etc.) as null.
	defer func() {
		if callErr == nil && result == nil {
			result = json.RawMessage("null")
		}
	}()
	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case <-b.ready:
	}
	if b.err != nil {
		return nil, b.err
	}
	switch command {
	case "frontend_ready":
		b.restartOnce.Do(func() { b.restartErr = notifyRestartReady() })
		if b.restartErr != nil {
			return nil, b.restartErr
		}
		b.backend.StartSchedulers(b.lifetime)
		if b.cliOwner {
			return true, nil
		}
		b.backend.StartLaunch(b.lifetime)
		return b.backend.Invoke(ctx, command, args)
	case "backend_capabilities":
		capabilities := b.backend.Capabilities()
		capabilities["commands"] = append(capabilities["commands"].([]string), "open_in_browser", "open_in_folder", "quit_app", "restart_app", "open_update_dir", "install_update", "get_autostart_status", "tray_open_main", "tray_panel_hide", "tray_panel_resize")
		return capabilities, nil
	case "tray_open_main":
		var input struct {
			Path   *string `json:"path"`
			Action *string `json:"action"`
		}
		if json.Unmarshal(args, &input) != nil {
			return nil, &core.AppError{Code: "INVALID_ARGUMENT", Message: "窗口操作参数无效"}
		}
		if input.Path != nil && !validTrayPath(*input.Path) {
			return nil, &core.AppError{Code: "INVALID_ARGUMENT", Message: "页面地址无效"}
		}
		if input.Action != nil && *input.Action != "about" && *input.Action != "check-update" {
			return nil, &core.AppError{Code: "INVALID_ARGUMENT", Message: "窗口操作无效"}
		}
		b.trayPanel.Hide()
		b.mainWindow.Show()
		b.mainWindow.Focus()
		if input.Path != nil {
			b.mainWindow.EmitEvent("tray://navigate", map[string]string{"path": *input.Path})
		}
		if input.Action != nil {
			b.mainWindow.EmitEvent("tray://" + *input.Action)
		}
		return true, nil
	case "tray_panel_hide":
		b.trayPanel.Hide()
		return true, nil
	case "tray_panel_resize":
		var input struct {
			Height int `json:"height"`
		}
		if json.Unmarshal(args, &input) != nil || input.Height < 320 || input.Height > 680 {
			return nil, &core.AppError{Code: "INVALID_ARGUMENT", Message: "托盘窗口尺寸无效"}
		}
		if !b.trayPanel.IsVisible() {
			return false, nil
		}
		b.trayPanel.SetSize(384, input.Height)
		return true, b.tray.PositionWindow(b.trayPanel, 10)
	case "get_autostart_status":
		return autostartStatus(b.home)
	case "set_setting":
		var input struct {
			Key   string          `json:"key"`
			Value json.RawMessage `json:"value"`
		}
		if json.Unmarshal(args, &input) != nil {
			return nil, &core.AppError{Code: "INVALID_ARGUMENT", Message: "设置参数无效"}
		}
		if input.Key == "autostart" {
			finish, err := b.backend.DataDirectoryActivity()
			if err != nil {
				return nil, err
			}
			defer finish()
			b.autostartMu.Lock()
			defer b.autostartMu.Unlock()
			var enabled bool
			if string(input.Value) != "true" && string(input.Value) != "false" {
				return nil, &core.AppError{Code: "INVALID_ARGUMENT", Message: "自启动开关无效"}
			}
			_ = json.Unmarshal(input.Value, &enabled)
			previous, err := autostartStatus(b.home)
			if err != nil {
				var ae *core.AppError
				if !errors.As(err, &ae) || ae.Code != "AUTOSTART_CHANGED" {
					return nil, err
				}
			}
			if err = setAutostart(b.home, enabled); err != nil {
				return nil, err
			}
			actual, err := autostartStatus(b.home)
			if err != nil {
				return nil, err
			}
			if actual != enabled {
				return nil, &core.AppError{Code: "AUTOSTART_UNVERIFIED", Message: "系统自启动状态未确认，请检查系统启动应用设置"}
			}
			if err = b.backend.SaveAutostartPreference(ctx, enabled); err != nil {
				return nil, errors.Join(err, setAutostart(b.home, previous))
			}
			return true, nil
		}
		result, err := b.backend.Invoke(ctx, command, args)
		if err == nil && input.Key == "minimizeToTray" {
			var enabled bool
			if json.Unmarshal(input.Value, &enabled) == nil {
				b.minimizeToTray.Store(enabled)
			}
		}
		return result, err
	case "open_update_dir":
		dir, err := b.backend.UpdateDirectory()
		if err != nil {
			return nil, err
		}
		return true, b.app.Env.OpenFileManager(core.DisplayPath(dir), false)
	case "install_update":
		finish, activityErr := b.backend.DataDirectoryActivity()
		if activityErr != nil {
			return nil, activityErr
		}
		defer finish()
		var input struct {
			Path string `json:"path"`
		}
		if json.Unmarshal(args, &input) != nil {
			return nil, &core.AppError{Code: "UPDATE_PATH_INVALID", Message: "更新包路径无效"}
		}
		path, err := b.backend.ValidateUpdateInstaller(ctx, input.Path)
		if err != nil {
			return nil, err
		}
		if !b.quitting.CompareAndSwap(false, true) {
			return nil, &core.AppError{Code: "QUIT_PENDING", Message: "应用正在停止服务"}
		}
		if err = b.backend.PrepareQuit(ctx); err != nil {
			b.quitting.Store(false)
			return nil, err
		}
		path, err = b.backend.ValidateUpdateInstaller(ctx, path)
		if err != nil {
			b.backend.CancelQuit()
			b.quitting.Store(false)
			return nil, err
		}
		var command *exec.Cmd
		if runtime.GOOS == "windows" {
			if strings.EqualFold(filepath.Ext(path), ".msi") {
				command = exec.Command("msiexec.exe", "/i", path)
			} else {
				command = exec.Command(path)
			}
		} else {
			command = exec.Command("/usr/bin/open", path)
		}
		if err = command.Start(); err != nil {
			b.backend.CancelQuit()
			b.quitting.Store(false)
			return nil, err
		}
		go command.Wait()
		b.quitReady.Store(true)
		b.app.Quit()
		return true, nil
	case "open_in_browser":
		var input struct {
			URL string `json:"url"`
		}
		if json.Unmarshal(args, &input) != nil {
			return nil, &core.AppError{Code: "BROWSER_URL_INVALID", Message: "网页地址无效"}
		}
		u, err := url.Parse(strings.TrimSpace(input.URL))
		if err != nil || u.Hostname() == "" || u.User != nil || (u.Scheme != "https" && u.Scheme != "http") || u.Port() == "0" || strings.ContainsRune(input.URL, '\\') || strings.IndexFunc(input.URL, unicode.IsControl) >= 0 {
			return nil, &core.AppError{Code: "BROWSER_URL_INVALID", Message: "请使用不含凭据的完整 HTTP(S) 地址"}
		}
		return true, b.app.Browser.OpenURL(u.String())
	case "open_in_folder":
		var input struct {
			Path string `json:"path"`
		}
		if json.Unmarshal(args, &input) != nil || input.Path == "" {
			return nil, &core.AppError{Code: "PATH_INVALID", Message: "请选择本地路径"}
		}
		info, err := os.Stat(input.Path)
		if err != nil {
			return nil, err
		}
		return true, b.app.Env.OpenFileManager(core.DisplayPath(input.Path), !info.IsDir())
	case "quit_app":
		return true, b.requestQuit(ctx)
	case "restart_app":
		var input struct {
			DataDir *string `json:"dataDir"`
		}
		if json.Unmarshal(args, &input) != nil {
			return nil, &core.AppError{Code: "INVALID_ARGUMENT", Message: "重启参数无效"}
		}
		return true, b.restart(ctx, input.DataDir)
	default:
		result, err := b.backend.Invoke(ctx, command, args)
		if err == nil && (command == "import_config" || command == "import_config_text") {
			if settings, readErr := b.backend.Invoke(ctx, "get_settings", nil); readErr == nil {
				b.minimizeToTray.Store(settings.(map[string]any)["minimizeToTray"].(bool))
			}
		}
		if err == nil && command == "set_setting" {
			var setting struct {
				Key   string `json:"key"`
				Value any    `json:"value"`
			}
			if json.Unmarshal(args, &setting) == nil && setting.Key == "minimizeToTray" {
				if enabled, ok := setting.Value.(bool); ok {
					b.minimizeToTray.Store(enabled)
				}
			}
		}
		return result, err
	}
}

func (b *Bridge) requestQuit(ctx context.Context) error {
	if !b.quitting.CompareAndSwap(false, true) {
		return &core.AppError{Code: "QUIT_PENDING", Message: "正在停止服务并退出"}
	}
	ctx, cancel := context.WithTimeout(ctx, 90*time.Second)
	defer cancel()
	select {
	case <-b.ready:
	case <-ctx.Done():
		b.quitting.Store(false)
		return ctx.Err()
	}
	if b.backend != nil {
		if err := b.backend.PrepareQuit(ctx); err != nil {
			b.quitting.Store(false)
			return err
		}
	}
	b.quitReady.Store(true)
	b.app.Quit()
	return nil
}

func (b *Bridge) quitFromMenu() {
	go func() {
		if err := b.requestQuit(context.Background()); err != nil {
			b.app.Dialog.Error().SetTitle("无法退出 NiceEnv").SetMessage(err.Error()).Show()
		}
	}()
}

// Native menu and OS quit requests use the same asynchronous shutdown path.
// This callback never waits for SQL, services, or the frontend on the UI thread.
func QuitGuard(b *Bridge) func() bool {
	return func() bool {
		if b.quitReady.Load() {
			return true
		}
		if !b.quitting.Load() {
			b.quitFromMenu()
		}
		return false
	}
}
