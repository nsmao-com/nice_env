//go:build windows

package desktop

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"
	"niceenv/internal/core"
)

const autostartRunKey = `Software\Microsoft\Windows\CurrentVersion\Run`

func autostartCommand(home string) (string, string, error) {
	executable, err := os.Executable()
	if err != nil {
		return "", "", err
	}
	hash := sha256.Sum256([]byte(strings.ToLower(filepath.Clean(home))))
	name := "NiceEnv-Wails-" + hex.EncodeToString(hash[:8])
	command := windows.ComposeCommandLine([]string{core.DisplayPath(executable), "--niceenv-data-home", core.DisplayPath(home)})
	return name, command, nil
}
func autostartStatus(home string) (bool, error) {
	name, expected, err := autostartCommand(home)
	if err != nil {
		return false, err
	}
	key, err := registry.OpenKey(registry.CURRENT_USER, autostartRunKey, registry.QUERY_VALUE)
	if errors.Is(err, registry.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	defer key.Close()
	value, _, err := key.GetStringValue(name)
	if errors.Is(err, registry.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	if value != expected {
		return false, &core.AppError{Code: "AUTOSTART_CHANGED", Message: "开机自启动项指向其他程序位置，请重新启用以更新"}
	}
	// Startup Apps can disable a Run entry without deleting it.
	approval, err := registry.OpenKey(registry.CURRENT_USER, `Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run`, registry.QUERY_VALUE)
	if err == nil {
		defer approval.Close()
		data, _, readErr := approval.GetBinaryValue(name)
		if readErr == nil && len(data) > 0 && (data[0] == 3 || data[0] == 7) {
			return false, nil
		}
		if readErr != nil && !errors.Is(readErr, registry.ErrNotExist) {
			return false, readErr
		}
	} else if !errors.Is(err, registry.ErrNotExist) {
		return false, err
	}
	return true, nil
}
func setAutostart(home string, enabled bool) error {
	name, command, err := autostartCommand(home)
	if err != nil {
		return err
	}
	key, _, err := registry.CreateKey(registry.CURRENT_USER, autostartRunKey, registry.SET_VALUE)
	if err != nil {
		return err
	}
	defer key.Close()
	if !enabled {
		err = key.DeleteValue(name)
		if errors.Is(err, registry.ErrNotExist) {
			return nil
		}
		return err
	}
	if err = key.SetStringValue(name, command); err != nil {
		return err
	}
	approval, err := registry.OpenKey(registry.CURRENT_USER, `Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run`, registry.SET_VALUE)
	if errors.Is(err, registry.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	defer approval.Close()
	err = approval.DeleteValue(name)
	if errors.Is(err, registry.ErrNotExist) {
		return nil
	}
	return err
}
