//go:build windows

package core

import (
	"fmt"
	"golang.org/x/sys/windows"
	"niceenv/internal/process"
)

// Apache's winnt MPM creates ap<PID>_shutdown/restart events for its parent.
// Signal only the retained child identity, never the global Windows service.
func signalApache(child *process.Child, reload bool) error {
	owned, err := child.OwnsPID(child.PID)
	if err != nil {
		return err
	}
	if !owned {
		return invalid("Apache 受管进程已退出")
	}
	action := "shutdown"
	if reload {
		action = "restart"
	}
	name, err := windows.UTF16PtrFromString(fmt.Sprintf("ap%d_%s", child.PID, action))
	if err != nil {
		return err
	}
	handle, err := windows.OpenEvent(windows.EVENT_MODIFY_STATE, false, name)
	if err != nil {
		return err
	}
	defer windows.CloseHandle(handle)
	return windows.SetEvent(handle)
}
