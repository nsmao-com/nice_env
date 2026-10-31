//go:build !windows

package systempath

import (
	"errors"
	"os"
	"path/filepath"
)

func Read() ([]Snapshot, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, err
	}
	paths := []string{filepath.Join(home, ".zshrc")}
	if _, err := os.Lstat(filepath.Join(home, ".bash_profile")); err == nil {
		paths = append(paths, filepath.Join(home, ".bash_profile"))
	} else if !os.IsNotExist(err) {
		return nil, err
	}
	out := []Snapshot{}
	for _, path := range paths {
		info, err := os.Lstat(path)
		if os.IsNotExist(err) {
			out = append(out, Snapshot{Path: path})
			continue
		}
		if err != nil {
			return nil, err
		}
		if !info.Mode().IsRegular() || info.Size() > 1024*1024 {
			return nil, errors.New("shell 配置必须为不超过 1 MiB 的普通文件")
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return nil, err
		}
		out = append(out, Snapshot{Path: path, Value: string(data), Exists: true})
	}
	return out, nil
}
func writeSystem(Snapshot, string) error {
	return errors.New("此平台使用 shell 配置文件管理 PATH")
}
func restoreSystem(Snapshot, Snapshot) error {
	return errors.New("此平台使用 shell 配置文件管理 PATH")
}
