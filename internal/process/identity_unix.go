//go:build !windows

package process

import (
	"errors"
	gprocess "github.com/shirou/gopsutil/v4/process"
	"strconv"
)

func Identity(pid int) (string, error) {
	p, err := gprocess.NewProcess(int32(pid))
	if err != nil {
		return "", err
	}
	created, err := p.CreateTime()
	if err != nil {
		return "", err
	}
	return strconv.FormatInt(created, 10), nil
}
func TerminateIdentity(pid int, expected string) error {
	if pid <= 1 || expected == "" {
		return errors.New("protected or unidentified process")
	}
	current, err := Identity(pid)
	if err != nil {
		return err
	}
	if current != expected {
		return errors.New("process identity changed")
	}
	p, err := gprocess.NewProcess(int32(pid))
	if err != nil {
		return err
	}
	return p.Kill()
}
