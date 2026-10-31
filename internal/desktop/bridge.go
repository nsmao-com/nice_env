package desktop

import (
	"context"
	"encoding/json"
	"net/url"
	"os"
	"strings"
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
}

func NewBridge(home, version string) *Bridge {
	b := &Bridge{home: home, version: version, ready: make(chan struct{})}
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

func (b *Bridge) Invoke(ctx context.Context, command string, args json.RawMessage) (any, error) {
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
		b.backend.StartSchedulers(b.lifetime)
		return b.backend.Invoke(ctx, command, args)
	case "backend_capabilities":
		capabilities := b.backend.Capabilities()
		capabilities["commands"] = append(capabilities["commands"].([]string), "open_in_browser", "open_in_folder", "quit_app")
		return capabilities, nil
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
	default:
		result, err := b.backend.Invoke(ctx, command, args)
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
