package main

import (
	"context"
	"crypto/sha256"
	"errors"
	"flag"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"time"

	"github.com/wailsapp/wails/v3/pkg/application"
	webassets "niceenv/apps/web"
	"niceenv/internal/core"
	"niceenv/internal/desktop"
	"niceenv/internal/hostfile"
	"niceenv/internal/process"
)

var version = "2.0.3-dev"

func main() {
	if handled, err := process.RunConsoleHelper(os.Args[1:]); handled {
		if err != nil {
			log.Print(err)
			os.Exit(1)
		}
		return
	}
	if handled, err := hostfile.RunHelper(os.Args[1:]); handled {
		if err != nil {
			log.Print(err)
			os.Exit(1)
		}
		return
	}
	dataHome := flag.String("niceenv-data-home", "", "Application data directory selected by the desktop launcher")
	cliOwner := flag.Bool("cli-owner", false, "Start in the tray for command-line service management")
	mcp := flag.Bool("mcp", false, "Serve MCP over stdio using the running desktop instance")
	restartParent := flag.Int("restart-parent", 0, "Wait for the previous desktop process before restarting")
	restartSupervisor := flag.Bool("restart-supervisor", false, "Supervise replacement desktop readiness")
	restartSource := flag.String("restart-source", "", "Original data directory for restart recovery")
	flag.Parse()
	if *restartSupervisor {
		if _, err := webassets.Handler(core.BuildChannel == "production"); err != nil {
			log.Fatal(err)
		}
		if err := desktop.RunRestartSupervisor(*dataHome, *restartSource, *restartParent); err != nil {
			log.Fatal(err)
		}
		return
	}
	if *restartParent > 0 {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
		err := core.WaitForRestartParent(ctx, *restartParent)
		cancel()
		if err != nil {
			log.Fatal(err)
		}
	}
	if *dataHome != "" {
		if !filepath.IsAbs(*dataHome) {
			log.Fatal("data directory must be absolute")
		}
		if err := os.Setenv("NICEENV_WAILS_HOME", *dataHome); err != nil {
			log.Fatal(err)
		}
	}
	home, err := core.ApplicationHome()
	if err != nil {
		log.Fatal(err)
	}
	if *mcp {
		if err := core.RunMCP(context.Background(), home, version, os.Stdin, os.Stdout); err != nil {
			log.Fatal(err)
		}
		return
	}
	activationCtx, activationCancel := context.WithTimeout(context.Background(), 30*time.Minute)
	err = core.FinishDataDirectoryActivation(activationCtx, home)
	activationCancel()
	if err != nil {
		log.Fatal(err)
	}
	assets, err := webassets.Handler(core.BuildChannel == "production")
	if err != nil {
		log.Fatal(err)
	}
	if err := core.PrepareBrowserHome(context.Background(), home); err != nil {
		var appErr *core.AppError
		if !errors.As(err, &appErr) || appErr.Code != "CONTROLLER_BUSY" {
			log.Fatal(err)
		}
	}
	bridge := desktop.NewBridge(home, version, *cliOwner)
	identity := home
	if runtime.GOOS == "windows" {
		identity = strings.ToLower(core.DisplayPath(identity))
	}
	app := application.New(application.Options{
		Name: "NiceEnv", Description: "NiceEnv Go + Wails v3",
		Services:   []application.Service{application.NewServiceWithOptions(bridge, application.ServiceOptions{MarshalError: core.MarshalError})},
		ShouldQuit: desktop.QuitGuard(bridge),
		Assets:     application.AssetOptions{Handler: assets},
		Windows:    application.WindowsOptions{WebviewUserDataPath: filepath.Join(home, "webview")},
		SingleInstance: &application.SingleInstanceOptions{UniqueID: fmt.Sprintf("com.niceenv.wails.%x", sha256.Sum256([]byte(identity))), OnSecondInstanceLaunch: func(data application.SecondInstanceData) {
			if !slices.Contains(data.Args, "--cli-owner") {
				desktop.ShowMainWindow(bridge)
			}
		}},
	})
	desktop.ConfigureWindow(app, bridge)
	if err := app.Run(); err != nil {
		log.Fatal(err)
	}
}
