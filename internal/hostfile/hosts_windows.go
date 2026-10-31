//go:build windows

package hostfile

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"unsafe"

	"golang.org/x/sys/windows"
)

func SystemPath() (string, error) {
	dir, err := windows.GetSystemDirectory()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, "drivers", "etc", "hosts"), nil
}
func openExclusive(path string) (*os.File, error) {
	name, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return nil, err
	}
	h, err := windows.CreateFile(name, windows.GENERIC_READ|windows.GENERIC_WRITE, windows.FILE_SHARE_READ, nil, windows.OPEN_EXISTING, windows.FILE_FLAG_OPEN_REPARSE_POINT, 0)
	if err != nil {
		return nil, &os.PathError{Op: "open", Path: path, Err: err}
	}
	var info windows.ByHandleFileInformation
	err = windows.GetFileInformationByHandle(h, &info)
	if err != nil || info.FileAttributes&(windows.FILE_ATTRIBUTE_REPARSE_POINT|windows.FILE_ATTRIBUTE_DIRECTORY) != 0 || info.NumberOfLinks != 1 {
		windows.CloseHandle(h)
		if err == nil {
			err = errors.New("hosts 必须为没有链接或重解析点的普通文件")
		}
		return nil, err
	}
	return os.NewFile(uintptr(h), path), nil
}

// Layout follows SHELLEXECUTEINFOW, including its handle-sized union.
type shellExecuteInfo struct {
	Size, Mask                        uint32
	Window                            windows.Handle
	Verb, File, Parameters, Directory *uint16
	Show                              int32
	Instance                          windows.Handle
	IDList                            uintptr
	Class                             *uint16
	ClassKey                          windows.Handle
	HotKey                            uint32
	Icon                              windows.Handle
	Process                           windows.Handle
}

func elevate(requestPath string) error {
	executable, err := os.Executable()
	if err != nil {
		return err
	}
	verb, _ := windows.UTF16PtrFromString("runas")
	file, err := windows.UTF16PtrFromString(executable)
	if err != nil {
		return err
	}
	params, err := windows.UTF16PtrFromString(windows.ComposeCommandLine([]string{HelperArgument, requestPath}))
	if err != nil {
		return err
	}
	info := shellExecuteInfo{Mask: 0x40 | 0x100 | 0x400, Verb: verb, File: file, Parameters: params, Show: 0}
	info.Size = uint32(unsafe.Sizeof(info))
	result, _, callErr := windows.NewLazySystemDLL("shell32.dll").NewProc("ShellExecuteExW").Call(uintptr(unsafe.Pointer(&info)))
	runtime.KeepAlive(verb)
	runtime.KeepAlive(file)
	runtime.KeepAlive(params)
	if result == 0 {
		return fmt.Errorf("hosts 管理员授权未完成：%w", callErr)
	}
	if info.Process == 0 {
		return errors.New("hosts 授权程序未返回进程句柄")
	}
	defer windows.CloseHandle(info.Process)
	state, err := windows.WaitForSingleObject(info.Process, windows.INFINITE)
	if err != nil {
		return err
	}
	if state != windows.WAIT_OBJECT_0 {
		return errors.New("无法等待 hosts 授权结果")
	}
	var code uint32
	if err = windows.GetExitCodeProcess(info.Process, &code); err != nil {
		return err
	}
	if code != 0 {
		return errors.New("hosts 授权后写入失败，文件可能已变化或被占用，请重新读取")
	}
	return nil
}
