//go:build windows

package process

import (
	"errors"
	"fmt"
	"golang.org/x/sys/windows"
)

func identityHandle(handle windows.Handle) (string, error) {
	var created, exited, kernel, user windows.Filetime
	if err := windows.GetProcessTimes(handle, &created, &exited, &kernel, &user); err != nil {
		return "", err
	}
	return fmt.Sprintf("%08x%08x", created.HighDateTime, created.LowDateTime), nil
}
func Identity(pid int) (string, error) {
	handle, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION, false, uint32(pid))
	if err != nil {
		return "", err
	}
	defer windows.CloseHandle(handle)
	return identityHandle(handle)
}
func TerminateIdentity(pid int, expected string) error {
	if pid <= 4 || expected == "" {
		return errors.New("protected or unidentified process")
	}
	handle, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION|windows.PROCESS_TERMINATE, false, uint32(pid))
	if err != nil {
		return err
	}
	defer windows.CloseHandle(handle)
	current, err := identityHandle(handle)
	if err != nil {
		return err
	}
	if current != expected {
		return errors.New("process identity changed")
	}
	return windows.TerminateProcess(handle, 1)
}
