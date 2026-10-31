//go:build linux || darwin

package process

import (
	"os/exec"
	"syscall"
)

func ConfigureDetached(cmd *exec.Cmd) {
	cmd.SysProcAttr = &syscall.SysProcAttr{Setsid: true}
}
