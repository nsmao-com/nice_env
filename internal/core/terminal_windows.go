//go:build windows

package core

import (
	"context"
	"encoding/base64"
	"encoding/binary"
	"os/exec"
	"path/filepath"
	"syscall"
	"time"
	"unicode/utf16"

	"golang.org/x/sys/windows"
)

func launchTerminal(ctx context.Context, environment terminalEnvironment) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	dir, err := windows.GetSystemDirectory()
	if err != nil {
		return err
	}
	units := utf16.Encode([]rune("$ErrorActionPreference = 'Stop'\n" + environment.Script))
	bytes := make([]byte, len(units)*2)
	for i, u := range units {
		binary.LittleEndian.PutUint16(bytes[2*i:], u)
	}
	encoded := base64.StdEncoding.EncodeToString(bytes)
	if len(encoded) > 24000 {
		return &AppError{Code: "TERMINAL_ENV_TOO_LARGE", Message: "终端环境过长，请减少选择的套件"}
	}
	// A visible console is intentional only for the user's Open Terminal action.
	command := exec.Command(filepath.Join(dir, "WindowsPowerShell", "v1.0", "powershell.exe"), "-NoLogo", "-NoProfile", "-NoExit", "-EncodedCommand", encoded)
	command.Dir = string(environment.CWD)
	command.SysProcAttr = &syscall.SysProcAttr{CreationFlags: windows.CREATE_NEW_CONSOLE}
	if err = command.Start(); err != nil {
		return &AppError{Code: "TERMINAL_OPEN_FAILED", Message: "无法打开 PowerShell", Detail: err.Error()}
	}
	done := make(chan error, 1)
	go func() { done <- command.Wait() }()
	select {
	case <-done:
		return &AppError{Code: "TERMINAL_EXITED", Message: "PowerShell 提前退出，请检查系统终端"}
	case <-time.After(150 * time.Millisecond):
		return nil
	}
}
