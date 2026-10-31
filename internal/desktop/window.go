package desktop

import (
	"github.com/wailsapp/wails/v3/pkg/application"
	"github.com/wailsapp/wails/v3/pkg/events"
	desktopassets "niceenv/apps/desktop"
	"runtime"
)

// ConfigureWindow is not a bound service method. Window lifecycle callbacks only
// consult atomic state; no filesystem, SQL or subprocess work runs on the UI thread.
func ConfigureWindow(app *application.App, b *Bridge) {
	window := app.Window.NewWithOptions(application.WebviewWindowOptions{
		Title: "NiceEnv · Wails 开发版", Width: 1280, Height: 820, MinWidth: 1000, MinHeight: 650, Frameless: runtime.GOOS != "darwin", URL: "/",
		Mac: application.MacWindow{TitleBar: application.MacTitleBarHiddenInset},
	})
	tray := app.SystemTray.New()
	tray.SetIcon(desktopassets.TrayIcon)
	tray.SetTooltip("NiceEnv · Wails 开发版")
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
