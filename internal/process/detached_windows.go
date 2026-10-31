//go:build windows

package process

import (
	"os/exec"
	"syscall"
)

// Only desktop launchers use this; managed services still belong to Job Objects.
func ConfigureDetached(cmd *exec.Cmd) {
	// A CLI may itself run inside a terminal's job. The desktop must outlive that
	// caller. If the job forbids breakaway, Start fails instead of promising that
	// service processes will survive the closing terminal.
	cmd.SysProcAttr = &syscall.SysProcAttr{CreationFlags: 0x08000000 | 0x00000200 | 0x01000000}
}
