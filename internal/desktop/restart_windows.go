//go:build windows

package desktop

import (
	"os/exec"
	"syscall"
)

func configureRestartCommand(cmd *exec.Cmd) {
	// CREATE_NO_WINDOW suppresses a console without SW_HIDE hiding Wails' first
	// native window as well. The replacement desktop must appear on its own.
	cmd.SysProcAttr = &syscall.SysProcAttr{CreationFlags: 0x08000000 | 0x00000200}
}
