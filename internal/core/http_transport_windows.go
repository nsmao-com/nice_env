package core

import "golang.org/x/sys/windows/registry"

func systemHTTPProxy() (server, bypass string) {
	key, err := registry.OpenKey(registry.CURRENT_USER, `Software\Microsoft\Windows\CurrentVersion\Internet Settings`, registry.QUERY_VALUE)
	if err != nil {
		return "", ""
	}
	defer key.Close()
	enabled, _, err := key.GetIntegerValue("ProxyEnable")
	if err != nil || enabled == 0 {
		return "", ""
	}
	server, _, err = key.GetStringValue("ProxyServer")
	if err != nil {
		return "", ""
	}
	bypass, _, _ = key.GetStringValue("ProxyOverride")
	return server, bypass
}
