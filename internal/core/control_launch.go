package core

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"niceenv/internal/process"
)

// EnsureDesktopController only probes readiness before the caller sends its
// operation. A lost mutation response is never retried through this function.
func EnsureDesktopController(ctx context.Context, home string) error {
	ctx, cancel := context.WithTimeout(ctx, 90*time.Second)
	defer cancel()
	probe := func() error {
		check, done := context.WithTimeout(ctx, 3*time.Second)
		defer done()
		_, err := ControlInvoke(check, home, "backend_capabilities", json.RawMessage(`{}`))
		return err
	}
	err := probe()
	if err == nil {
		return nil
	}
	var appErr *AppError
	if !errors.As(err, &appErr) || appErr.Code != "CONTROLLER_UNAVAILABLE" {
		return err
	}
	if err = PrepareApplicationHome(ctx, home); err != nil {
		if !errors.As(err, &appErr) || appErr.Code != "CONTROLLER_BUSY" {
			return err
		}
		// An owner may still be initializing its endpoint. Do not start a rival.
		return waitController(ctx, probe)
	}
	unlock, err := process.Lock(filepath.Join(home, ".niceenv-controller-launch.lock"))
	if errors.Is(err, process.ErrLocked) {
		return waitController(ctx, probe)
	}
	if err != nil {
		return err
	}
	defer unlock()
	if err = probe(); err == nil {
		return nil
	}
	if !errors.As(err, &appErr) || appErr.Code != "CONTROLLER_UNAVAILABLE" {
		return err
	}
	self, err := os.Executable()
	if err != nil {
		return err
	}
	name := "NiceEnv"
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	program := filepath.Join(filepath.Dir(self), name)
	if strings.EqualFold(filepath.Base(self), name) {
		program = self
	}
	if info, err := os.Stat(program); err != nil || !info.Mode().IsRegular() {
		return &AppError{Code: "CONTROLLER_NOT_INSTALLED", Message: "找不到 NiceEnv 主程序，请将命令行工具与完整应用一起安装"}
	}
	command := exec.Command(program, "--niceenv-data-home", home, "--cli-owner")
	command.Dir = filepath.Dir(program)
	process.ConfigureDetached(command)
	if err = command.Start(); err != nil {
		return &AppError{Code: "CONTROLLER_START_FAILED", Message: "无法独立启动 NiceEnv，请先从开始菜单打开应用，再执行命令", Hint: "当前终端可能限制后台进程脱离；已运行的 NiceEnv 仍可直接连接。", Detail: err.Error()}
	}
	exited := make(chan error, 1)
	go func() { exited <- command.Wait() }()
	for {
		if err = probe(); err == nil {
			return nil
		}
		select {
		case err := <-exited:
			if err != nil {
				return &AppError{Code: "CONTROLLER_START_FAILED", Message: "NiceEnv 未能启动，请手动打开应用查看具体原因", Detail: err.Error()}
			}
			// A second-instance launch exits successfully after notifying its owner.
			exited = nil
		case <-ctx.Done():
			return &AppError{Code: "CONTROLLER_START_TIMEOUT", Message: "NiceEnv 启动尚未就绪，请检查应用后重试；未发送服务操作"}
		case <-time.After(200 * time.Millisecond):
		}
	}
}

func waitController(ctx context.Context, probe func() error) error {
	for {
		if err := probe(); err == nil {
			return nil
		}
		select {
		case <-ctx.Done():
			return &AppError{Code: "CONTROLLER_BUSY", Message: "数据目录正在使用或应用尚未就绪，请先退出旧版并检查 NiceEnv"}
		case <-time.After(200 * time.Millisecond):
		}
	}
}
