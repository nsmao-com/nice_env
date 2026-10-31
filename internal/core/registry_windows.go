//go:build windows

package core

import (
	"golang.org/x/sys/windows"
	"path/filepath"
)

func systemCurl() (string, error) {
	dir, err := windows.GetSystemDirectory()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "curl.exe"), nil
}
