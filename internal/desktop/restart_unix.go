//go:build !windows

package desktop

import (
	"os/exec"
	"syscall"
)

func configureRestartCommand(cmd *exec.Cmd) { cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true} }
