//go:build !windows

package desktop

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/xml"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

func autostartFile(home string) (string, []byte, error) {
	executable, err := os.Executable()
	if err != nil {
		return "", nil, err
	}
	userHome, err := os.UserHomeDir()
	if err != nil {
		return "", nil, err
	}
	hash := sha256.Sum256([]byte(filepath.Clean(home)))
	name := "com.niceenv.wails." + hex.EncodeToString(hash[:8])
	if runtime.GOOS == "darwin" {
		escape := func(s string) string { var b strings.Builder; _ = xml.EscapeText(&b, []byte(s)); return b.String() }
		text := `<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>Label</key><string>` + name + `</string><key>ProgramArguments</key><array><string>` + escape(executable) + `</string><string>--niceenv-data-home</string><string>` + escape(home) + `</string></array><key>RunAtLoad</key><true/></dict></plist>`
		return filepath.Join(userHome, "Library", "LaunchAgents", name+".plist"), []byte(text), nil
	}
	config, err := os.UserConfigDir()
	if err != nil {
		return "", nil, err
	}
	quote := func(value string) string {
		return `"` + strings.NewReplacer(`\`, `\\`, `"`, `\"`, `$`, `\$`, "`", "\\`", "%", "%%", "\n", `\n`, "\r", `\r`).Replace(value) + `"`
	}
	text := "[Desktop Entry]\nType=Application\nName=NiceEnv\nExec=" + quote(executable) + " --niceenv-data-home " + quote(home) + "\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
	return filepath.Join(config, "autostart", name+".desktop"), []byte(text), nil
}
func autostartStatus(home string) (bool, error) {
	path, expected, err := autostartFile(home)
	if err != nil {
		return false, err
	}
	data, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return false, nil
	}
	return string(data) == string(expected), err
}
func setAutostart(home string, enabled bool) error {
	path, data, err := autostartFile(home)
	if err != nil {
		return err
	}
	if !enabled {
		err = os.Remove(path)
		if os.IsNotExist(err) {
			return nil
		}
		return err
	}
	if err = os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return err
	}
	f, err := os.CreateTemp(filepath.Dir(path), ".niceenv-autostart-")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	if _, err = f.Write(data); err != nil {
		f.Close()
		return err
	}
	if err = f.Sync(); err != nil {
		f.Close()
		return err
	}
	if err = f.Close(); err != nil {
		return err
	}
	return os.Rename(f.Name(), path)
}
