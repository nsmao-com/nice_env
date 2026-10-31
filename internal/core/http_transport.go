package core

import (
	"net"
	"net/http"
	"net/url"
	"os"
	"path"
	"strings"
)

// Snapshot proxy preferences once per operation. Explicit environment settings
// take precedence; local service requests and bypass destinations stay direct.
// No process-wide environment or system proxy settings are modified here.
func outboundTransport() *http.Transport {
	t := http.DefaultTransport.(*http.Transport).Clone()
	server, bypass := systemHTTPProxy()
	if server == "" {
		return t
	}
	noProxy := os.Getenv("NO_PROXY")
	if noProxy == "" {
		noProxy = os.Getenv("no_proxy")
	}
	t.Proxy = func(req *http.Request) (*url.URL, error) {
		p, err := http.ProxyFromEnvironment(req)
		if err != nil || p != nil {
			return p, err
		}
		key := strings.ToUpper(req.URL.Scheme) + "_PROXY"
		if os.Getenv(key) != "" || os.Getenv(strings.ToLower(key)) != "" {
			return nil, nil
		}
		if proxyBypassed(req.URL, noProxy, false) || proxyBypassed(req.URL, bypass, true) {
			return nil, nil
		}
		address := server
		if strings.Contains(server, "=") {
			address = ""
			for _, part := range strings.Split(server, ";") {
				protocol, value, ok := strings.Cut(strings.TrimSpace(part), "=")
				if ok && strings.EqualFold(protocol, req.URL.Scheme) {
					address = strings.TrimSpace(value)
					break
				}
			}
		}
		if address == "" {
			return nil, nil
		}
		if !strings.Contains(address, "://") {
			address = "http://" + address
		}
		p, err = url.Parse(address)
		if err != nil || p.Hostname() == "" || (p.Scheme != "http" && p.Scheme != "https" && p.Scheme != "socks5" && p.Scheme != "socks5h") || p.RawQuery != "" || p.Fragment != "" || p.Path != "" && p.Path != "/" {
			return nil, &AppError{Code: "PROXY_INVALID", Message: "系统代理地址无效，请检查网络代理设置"}
		}
		return p, nil
	}
	return t
}

func proxyBypassed(u *url.URL, rules string, windows bool) bool {
	host := strings.ToLower(strings.TrimSuffix(u.Hostname(), "."))
	ip := net.ParseIP(host)
	if host == "localhost" || strings.HasSuffix(host, ".localhost") || ip != nil && ip.IsLoopback() {
		return true
	}
	port := u.Port()
	if port == "" {
		if u.Scheme == "https" {
			port = "443"
		} else {
			port = "80"
		}
	}
	for _, rule := range strings.FieldsFunc(strings.ToLower(rules), func(r rune) bool { return r == ';' || r == ',' }) {
		rule = strings.TrimSpace(rule)
		if rule == "" {
			continue
		}
		if rule == "*" || windows && rule == "<local>" && !strings.Contains(host, ".") && ip == nil {
			return true
		}
		if _, network, err := net.ParseCIDR(rule); err == nil {
			if ip != nil && network.Contains(ip) {
				return true
			}
			continue
		}
		if h, p, err := net.SplitHostPort(rule); err == nil {
			if p != port {
				continue
			}
			rule = h
		}
		rule = strings.Trim(rule, "[]")
		if windows {
			if ok, _ := path.Match(rule, host); ok {
				return true
			}
		} else {
			rule = strings.TrimPrefix(rule, "*")
			if strings.HasPrefix(rule, ".") {
				if strings.HasSuffix(host, rule) {
					return true
				}
			} else if host == rule || strings.HasSuffix(host, "."+rule) {
				return true
			}
		}
	}
	return false
}
