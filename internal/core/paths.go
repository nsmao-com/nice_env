package core

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
)

// Path keeps native filesystem paths intact and only normalises JSON output.
// Never apply this conversion to SQL, file contents, passwords or arbitrary strings.
type Path string

func (p Path) MarshalJSON() ([]byte, error) { return json.Marshal(DisplayPath(string(p))) }

func DisplayPath(path string) string {
	if strings.HasPrefix(path, `\\?\UNC\`) {
		return `\\` + path[8:]
	}
	if len(path) >= 7 && strings.HasPrefix(path, `\\?\`) && path[5] == ':' && path[6] == '\\' {
		return path[4:]
	}
	return path
}

// Until the full parity gate passes, the Wails executable uses its own data root.
// NSB_HOME and the installed application's data-directory selection are deliberately
// not inherited: development must not take ownership of production service data.
func DevelopmentHome() (string, error) {
	base := os.Getenv("NICEENV_WAILS_HOME")
	if base == "" {
		config, err := os.UserConfigDir()
		if err != nil {
			return "", err
		}
		base = filepath.Join(config, "NiceEnv-Wails-Development")
	}
	abs, err := filepath.Abs(base)
	if err != nil {
		return "", err
	}
	return filepath.Clean(abs), nil
}
