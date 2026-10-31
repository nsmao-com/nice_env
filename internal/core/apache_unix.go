//go:build !windows

package core

import (
	"niceenv/internal/process"
	"os"
	"syscall"
)

func signalApache(child *process.Child, reload bool) error {
	owned, err := child.OwnsPID(child.PID)
	if err != nil {
		return err
	}
	if !owned {
		return invalid("Apache 受管进程已退出")
	}
	p, err := os.FindProcess(child.PID)
	if err != nil {
		return err
	}
	if reload {
		return p.Signal(syscall.SIGUSR1)
	}
	return p.Signal(syscall.SIGTERM)
}
