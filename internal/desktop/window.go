package desktop

import (
	"github.com/wailsapp/wails/v3/pkg/application"
	"github.com/wailsapp/wails/v3/pkg/events"
	desktopassets "niceenv/apps/desktop"
	"niceenv/internal/core"
	"runtime"
	"strings"
	"time"
)

// ConfigureWindow is not a bound service method. Window lifecycle callbacks only
// consult atomic state; no filesystem, SQL or subprocess work runs on the UI thread.
func ConfigureWindow(app *application.App, b *Bridge) {
	title := "NiceEnv"
	if core.BuildChannel != "production" {
		title += " · Wails 开发版"
	}
	window := app.Window.NewWithOptions(application.WebviewWindowOptions{
		Hidden: b.cliOwner,
		Title:  title, Width: 1280, Height: 820, MinWidth: 1000, MinHeight: 650, Frameless: runtime.GOOS != "darwin", URL: "/",
		Mac: application.MacWindow{TitleBar: application.MacTitleBarHiddenInset},
	})
	tray := app.SystemTray.New()
	tray.SetIcon(desktopassets.TrayIcon)
	tray.SetTooltip(title)
	panel := app.Window.NewWithOptions(application.WebviewWindowOptions{
		Name: "tray-panel", Title: "NiceEnv 快捷面板", Width: 384, Height: 600,
		Frameless: true, AlwaysOnTop: true, Hidden: true, DisableResize: true, URL: "/tray_panel.html",
	})
	b.mainWindow, b.trayPanel, b.tray = window, panel, tray
	tray.AttachWindow(panel).WindowOffset(10).WindowDebounce(200 * time.Millisecond)
	panel.RegisterHook(events.Common.WindowClosing, func(event *application.WindowEvent) { event.Cancel(); panel.Hide() })
	menu := app.NewMenu()
	menu.Add("打开 NiceEnv").OnClick(func(*application.Context) { window.Show(); window.Focus() })
	for _, entry := range []struct{ label, path string }{{"站点", "/sites"}, {"套件", "/packages"}, {"设置", "/settings"}} {
		menu.Add(entry.label).OnClick(func(*application.Context) {
			window.Show()
			window.Focus()
			app.Event.Emit("tray://navigate", map[string]string{"path": entry.path})
		})
	}
	menu.AddSeparator()
	menu.Add("退出").OnClick(func(*application.Context) { b.quitFromMenu() })
	tray.SetMenu(menu)
	window.RegisterHook(events.Common.WindowClosing, func(event *application.WindowEvent) {
		if b.quitReady.Load() {
			return
		}
		if b.quitting.Load() {
			event.Cancel()
			return
		}
		event.Cancel()
		if b.minimizeToTray.Load() {
			window.Hide()
		} else {
			b.quitFromMenu()
		}
	})
}

func validTrayPath(path string) bool {
	return strings.HasPrefix(path, "/") && !strings.HasPrefix(path, "//") && !strings.ContainsAny(path, "\\\x00\r\n")
}

// Reopening the application should also restore a window hidden in the tray.
func ShowMainWindow(b *Bridge) {
	if b.mainWindow != nil {
		b.mainWindow.Show()
		b.mainWindow.Focus()
	}
}
