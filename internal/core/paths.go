package core

import (
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

// Release packaging sets this with -X; local builds stay isolated by default.
var BuildChannel = "development"

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

// Development builds keep an isolated data root. Production packaging explicitly
// enables the installed application's selection and v1 metadata upgrade checks.
func ApplicationHome() (string, error) {
	base := os.Getenv("NICEENV_WAILS_HOME")
	if base == "" && BuildChannel == "production" {
		var err error
		base, err = installedHome()
		if err != nil {
			return "", err
		}
	}
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
	return resolveDataDirectory(filepath.Clean(abs))
}

func installedHome() (string, error) {
	if base := strings.TrimSpace(os.Getenv("NSB_HOME")); base != "" {
		return base, nil
	}
	config, err := os.UserConfigDir()
	if err != nil {
		return "", err
	}
	selection := filepath.Join(config, "com.niceservbay.app", "data-directory.json")
	if base, err := readLegacyDataSelection(selection); err != nil || base != "" {
		return base, err
	}
	exe, err := os.Executable()
	if err != nil {
		return "", err
	}
	adjacent := filepath.Join(filepath.Dir(exe), "nsb-data")
	if runtime.GOOS == "windows" {
		return adjacent, nil
	}
	if info, err := os.Stat(adjacent); err == nil && info.IsDir() {
		return adjacent, nil
	}
	base := filepath.Join(config, "NiceEnv")
	if runtime.GOOS == "linux" {
		local := os.Getenv("XDG_DATA_HOME")
		if local == "" {
			home, err := os.UserHomeDir()
			if err != nil {
				return "", err
			}
			local = filepath.Join(home, ".local", "share")
		}
		base = filepath.Join(local, "NiceEnv")
	}
	if _, err := os.Stat(base); os.IsNotExist(err) {
		for _, old := range []string{"NiceServBay", "niceEnv"} {
			legacy := filepath.Join(filepath.Dir(base), old)
			if info, err := os.Stat(legacy); err == nil && info.IsDir() {
				return legacy, nil
			}
		}
	}
	return base, nil
}

func readLegacyDataSelection(path string) (string, error) {
	body, err := readPrivateMaterial(path)
	if os.IsNotExist(err) {
		return "", nil
	}
	if err != nil {
		return "", err
	}
	var selection struct {
		Version int      `json:"version"`
		Path    string   `json:"path"`
		Retired []string `json:"retired"`
	}
	if json.Unmarshal(body, &selection) != nil || selection.Version != 1 || !filepath.IsAbs(selection.Path) {
		return "", invalid("保存的数据目录选择无效，请恢复配置后重试")
	}
	for _, retired := range selection.Retired {
		if !filepath.IsAbs(retired) {
			return "", invalid("数据目录失效记录无效")
		}
	}
	if info, err := os.Stat(filepath.Join(selection.Path, "nsb.sqlite")); err != nil || !info.Mode().IsRegular() {
		return "", invalid("保存的数据目录不可用，请连接原磁盘后重试；未创建空环境")
	}
	return selection.Path, nil
}
