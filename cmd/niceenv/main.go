package main

import (
	"log"
	"os"
	"path/filepath"

	"github.com/wailsapp/wails/v3/pkg/application"
	webassets "niceenv/apps/web"
	"niceenv/internal/core"
	"niceenv/internal/desktop"
	"niceenv/internal/hostfile"
)

var version = "1.0.4-wails-dev"

func main() {
	if handled, err := hostfile.RunHelper(os.Args[1:]); handled {
		if err != nil {
			log.Print(err)
			os.Exit(1)
		}
		return
	}
	home, err := core.DevelopmentHome()
	if err != nil {
		log.Fatal(err)
	}
	assets, err := webassets.Handler()
	if err != nil {
		log.Fatal(err)
	}
	if err := core.PrepareDevelopmentHome(home); err != nil {
		log.Fatal(err)
	}
	bridge := desktop.NewBridge(home, version)
	app := application.New(application.Options{
		Name: "NiceEnv Wails Development", Description: "NiceEnv Go + Wails v3",
		Services:       []application.Service{application.NewServiceWithOptions(bridge, application.ServiceOptions{MarshalError: core.MarshalError})},
		ShouldQuit:     desktop.QuitGuard(bridge),
		Assets:         application.AssetOptions{Handler: assets},
		Windows:        application.WindowsOptions{WebviewUserDataPath: filepath.Join(home, "webview")},
		SingleInstance: &application.SingleInstanceOptions{UniqueID: "com.niceenv.wails.development"},
	})
	desktop.ConfigureWindow(app, bridge)
	if err := app.Run(); err != nil {
		log.Fatal(err)
	}
}
