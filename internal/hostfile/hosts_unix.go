//go:build !windows

package hostfile

import (
	"errors"
	"golang.org/x/sys/unix"
	"os"
)

func SystemPath() (string, error) { return "/etc/hosts", nil }
func openExclusive(path string) (*os.File, error) {
	fd, err := unix.Open(path, unix.O_RDWR|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
	if err != nil {
		return nil, err
	}
	f := os.NewFile(uintptr(fd), path)
	var info unix.Stat_t
	if err = unix.Fstat(fd, &info); err != nil {
		f.Close()
		return nil, err
	}
	if info.Mode&unix.S_IFMT != unix.S_IFREG || info.Nlink != 1 {
		f.Close()
		return nil, errors.New("hosts 必须为没有链接的普通文件")
	}
	if err = unix.Flock(fd, unix.LOCK_EX|unix.LOCK_NB); err != nil {
		f.Close()
		return nil, err
	}
	return f, nil
}
func elevate(string) error { return errors.New("保存系统 hosts 需要管理员权限") }
