//go:build windows

package core

import (
	"context"
	"errors"
	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"
	"reflect"
)

const internetSettings = `Software\Microsoft\Windows\CurrentVersion\Internet Settings`

var proxyRegistryNames = []string{"ProxyEnable", "ProxyServer", "ProxyOverride", "AutoConfigURL", "AutoDetect"}

func readSystemProxy(ctx context.Context) (systemProxySnapshot, error) {
	r := systemProxySnapshot{Values: map[string]any{}}
	key, err := registry.OpenKey(registry.CURRENT_USER, internetSettings, registry.QUERY_VALUE)
	if errors.Is(err, registry.ErrNotExist) {
		for _, name := range proxyRegistryNames {
			r.Values[name] = nil
		}
		return r, nil
	}
	if err != nil {
		return r, err
	}
	defer key.Close()
	for _, name := range proxyRegistryNames {
		if name == "ProxyEnable" || name == "AutoDetect" {
			v, _, err := key.GetIntegerValue(name)
			if errors.Is(err, registry.ErrNotExist) {
				r.Values[name] = nil
			} else if err != nil {
				return r, err
			} else {
				r.Values[name] = float64(v)
			}
		} else {
			v, _, err := key.GetStringValue(name)
			if errors.Is(err, registry.ErrNotExist) {
				r.Values[name] = nil
			} else if err != nil {
				return r, err
			} else {
				r.Values[name] = v
			}
		}
	}
	r.Enabled = r.Values["ProxyEnable"] == float64(1)
	r.Server, _ = r.Values["ProxyServer"].(string)
	return r, nil
}
func managedSystemProxy(old systemProxySnapshot, server string) systemProxySnapshot {
	return systemProxySnapshot{Enabled: true, Server: server, Values: map[string]any{"ProxyEnable": float64(1), "ProxyServer": server, "ProxyOverride": "<local>;localhost;127.*;[::1]", "AutoConfigURL": nil, "AutoDetect": float64(0)}}
}
func writeSystemProxy(ctx context.Context, target systemProxySnapshot) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	key, _, err := registry.CreateKey(registry.CURRENT_USER, internetSettings, registry.SET_VALUE)
	if err != nil {
		return err
	}
	defer key.Close()
	for _, name := range proxyRegistryNames {
		v := target.Values[name]
		if v == nil {
			err = key.DeleteValue(name)
			if errors.Is(err, registry.ErrNotExist) {
				err = nil
			}
		} else if name == "ProxyEnable" || name == "AutoDetect" {
			n, ok := v.(float64)
			if !ok || n < 0 || n > 4294967295 {
				return invalid("系统代理恢复记录无效")
			}
			err = key.SetDWordValue(name, uint32(n))
		} else {
			s, ok := v.(string)
			if !ok {
				return invalid("系统代理恢复记录无效")
			}
			err = key.SetStringValue(name, s)
		}
		if err != nil {
			return err
		}
	}
	set := windows.NewLazySystemDLL("wininet.dll").NewProc("InternetSetOptionW")
	for _, option := range []uintptr{39, 37} {
		ok, _, err := set.Call(0, option, 0, 0)
		if ok == 0 {
			return err
		}
	}
	actual, err := readSystemProxy(ctx)
	if err != nil {
		return err
	}
	if !reflect.DeepEqual(actual, target) {
		return &AppError{Code: "SYSTEM_PROXY_UNVERIFIED", Message: "系统代理写入后状态不一致，请刷新检查"}
	}
	return nil
}
