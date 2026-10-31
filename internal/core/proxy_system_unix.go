//go:build !windows

package core

import (
	"context"
	"net"
	"reflect"
	"runtime"
	"strings"
)

var gnomeProxyKeys = map[string][]string{"org.gnome.system.proxy": {"mode", "autoconfig-url", "ignore-hosts", "use-same-proxy"}, "org.gnome.system.proxy.http": {"host", "port", "enabled", "use-authentication"}, "org.gnome.system.proxy.https": {"host", "port"}, "org.gnome.system.proxy.socks": {"host", "port"}}

func readSystemProxy(ctx context.Context) (systemProxySnapshot, error) {
	r := systemProxySnapshot{Values: map[string]any{}}
	if runtime.GOOS != "darwin" {
		for schema, keys := range gnomeProxyKeys {
			for _, key := range keys {
				value, err := networkOutput(ctx, "gsettings", "get", schema, key)
				if err != nil {
					return r, err
				}
				r.Values[schema+"/"+key] = value
			}
		}
		r.Enabled = r.Values["org.gnome.system.proxy/mode"] == "'manual'"
		host, _ := r.Values["org.gnome.system.proxy.http/host"].(string)
		port, _ := r.Values["org.gnome.system.proxy.http/port"].(string)
		r.Server = net.JoinHostPort(strings.Trim(host, "'"), port)
		return r, nil
	}
	names, err := dnsInterfaces(ctx)
	if err != nil {
		return r, err
	}
	if len(names) == 0 {
		return r, invalid("找不到可用网络服务")
	}
	r.Service = names[0]
	for _, name := range names {
		if name == "Wi-Fi" {
			r.Service = name
			break
		}
	}
	for _, kind := range []string{"web", "secureweb", "socksfirewall"} {
		text, err := networkOutput(ctx, "networksetup", "-get"+kind+"proxy", r.Service)
		if err != nil {
			return r, err
		}
		for _, line := range strings.Split(text, "\n") {
			key, value, ok := strings.Cut(line, ":")
			if ok {
				r.Values[kind+"/"+key] = strings.TrimSpace(value)
			}
		}
	}
	text, err := networkOutput(ctx, "networksetup", "-getautoproxyurl", r.Service)
	if err != nil {
		return r, err
	}
	for _, line := range strings.Split(text, "\n") {
		key, value, ok := strings.Cut(line, ":")
		if ok {
			r.Values["auto/"+key] = strings.TrimSpace(value)
		}
	}
	r.Enabled = r.Values["web/Enabled"] == "Yes"
	host, _ := r.Values["web/Server"].(string)
	port, _ := r.Values["web/Port"].(string)
	r.Server = net.JoinHostPort(host, port)
	return r, nil
}
func managedSystemProxy(old systemProxySnapshot, server string) systemProxySnapshot {
	r := systemProxySnapshot{Enabled: true, Server: server, Service: old.Service, Values: map[string]any{}}
	for k, v := range old.Values {
		r.Values[k] = v
	}
	host, port, _ := net.SplitHostPort(server)
	if runtime.GOOS != "darwin" {
		r.Values["org.gnome.system.proxy/mode"] = "'manual'"
		r.Values["org.gnome.system.proxy/use-same-proxy"] = "false"
		r.Values["org.gnome.system.proxy.http/enabled"] = "true"
		r.Values["org.gnome.system.proxy.http/use-authentication"] = "false"
		for _, kind := range []string{"http", "https", "socks"} {
			r.Values["org.gnome.system.proxy."+kind+"/host"] = "'" + host + "'"
			r.Values["org.gnome.system.proxy."+kind+"/port"] = port
		}
		return r
	}
	for _, kind := range []string{"web", "secureweb", "socksfirewall"} {
		r.Values[kind+"/Enabled"] = "Yes"
		r.Values[kind+"/Server"] = host
		r.Values[kind+"/Port"] = port
		r.Values[kind+"/Authenticated Proxy Enabled"] = "0"
	}
	r.Values["auto/Enabled"] = "No"
	return r
}
func writeSystemProxy(ctx context.Context, r systemProxySnapshot) error {
	if runtime.GOOS != "darwin" {
		for schema, keys := range gnomeProxyKeys {
			for _, key := range keys {
				v, ok := r.Values[schema+"/"+key].(string)
				if !ok {
					return invalid("系统代理恢复记录无效")
				}
				if _, err := networkOutput(ctx, "gsettings", "set", schema, key, v); err != nil {
					return err
				}
			}
		}
	} else {
		if err := dnsName(r.Service); err != nil {
			return err
		}
		for _, kind := range []string{"web", "secureweb", "socksfirewall"} {
			host, _ := r.Values[kind+"/Server"].(string)
			port, _ := r.Values[kind+"/Port"].(string)
			if host != "" && port != "0" {
				if _, err := networkOutput(ctx, "networksetup", "-set"+kind+"proxy", r.Service, host, port); err != nil {
					return err
				}
			}
			state := "off"
			if r.Values[kind+"/Enabled"] == "Yes" {
				state = "on"
			}
			if _, err := networkOutput(ctx, "networksetup", "-set"+kind+"proxystate", r.Service, state); err != nil {
				return err
			}
		}
		state := "off"
		if r.Values["auto/Enabled"] == "Yes" {
			state = "on"
		}
		if _, err := networkOutput(ctx, "networksetup", "-setautoproxystate", r.Service, state); err != nil {
			return err
		}
	}
	actual, err := readSystemProxy(ctx)
	if err != nil {
		return err
	}
	if !reflect.DeepEqual(actual, r) {
		return &AppError{Code: "SYSTEM_PROXY_UNVERIFIED", Message: "系统代理状态未达到目标值，请检查网络设置"}
	}
	return nil
}
