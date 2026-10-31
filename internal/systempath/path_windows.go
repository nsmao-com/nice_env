//go:build windows

package systempath

import (
	"errors"
	"runtime"
	"unsafe"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"
)

func Read() ([]Snapshot, error) {
	key, err := registry.OpenKey(registry.CURRENT_USER, "Environment", registry.QUERY_VALUE)
	if err != nil {
		return nil, err
	}
	defer key.Close()
	value, kind, err := key.GetStringValue("Path")
	if errors.Is(err, registry.ErrNotExist) {
		return []Snapshot{{Path: "HKCU\\Environment\\Path", Kind: registry.EXPAND_SZ}}, nil
	}
	if err != nil {
		return nil, err
	}
	if kind != registry.SZ && kind != registry.EXPAND_SZ {
		return nil, errors.New("用户 PATH 注册表类型无效，未修改")
	}
	return []Snapshot{{Path: "HKCU\\Environment\\Path", Value: value, Kind: kind, Exists: true}}, nil
}
func writeSystem(before Snapshot, value string) error {
	return updateSystem(before, Snapshot{Value: value, Kind: before.Kind, Exists: true})
}
func restoreSystem(after, original Snapshot) error {
	return updateSystem(after, original)
}
func updateSystem(before, target Snapshot) error {
	value := target.Value
	if len(value) > 32760 {
		return errors.New("用户 PATH 过长，请减少所选套件")
	}
	key, err := registry.OpenKey(registry.CURRENT_USER, "Environment", registry.QUERY_VALUE|registry.SET_VALUE)
	if err != nil {
		return err
	}
	defer key.Close()
	current, kind, err := key.GetStringValue("Path")
	exists := err == nil
	if err != nil && !errors.Is(err, registry.ErrNotExist) {
		return err
	}
	if exists != before.Exists || current != before.Value || exists && kind != before.Kind {
		return errors.New("用户 PATH 已发生变化，请重新读取后再应用")
	}
	if !target.Exists {
		err = key.DeleteValue("Path")
	} else if target.Kind == registry.EXPAND_SZ {
		err = key.SetExpandStringValue("Path", value)
	} else {
		err = key.SetStringValue("Path", value)
	}
	if err != nil {
		return err
	}
	actual, _, err := key.GetStringValue("Path")
	if !target.Exists && errors.Is(err, registry.ErrNotExist) {
		err = nil
	} else if err != nil {
		return err
	} else if !target.Exists || actual != value {
		return errors.New("PATH 写入后发生变化，请检查系统环境变量")
	}
	name, _ := windows.UTF16PtrFromString("Environment")
	var result uintptr
	windows.NewLazySystemDLL("user32.dll").NewProc("SendMessageTimeoutW").Call(0xffff, 0x001a, 0, uintptr(unsafe.Pointer(name)), 0x0002, 2000, uintptr(unsafe.Pointer(&result)))
	runtime.KeepAlive(name)
	return nil
}
