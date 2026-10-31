package core

import (
	"context"
	"crypto/tls"
	"io"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"

	gnet "github.com/shirou/gopsutil/v4/net"
	"niceenv/internal/process"
)

func genericServiceWebURL(ctx context.Context, id string, child *process.Child, r *resolvedService) (string, error) {
	port, path := r.port, "/"
	pair := func(flag, value string) bool {
		for i := 0; i+1 < len(r.args); i++ {
			if r.args[i] == flag && r.args[i+1] == value {
				return true
			}
		}
		return false
	}
	known := false
	switch id {
	case "neo4j":
		known, path = true, "/browser/"
	case "rabbitmq":
		known, port = true, r.port+10000
	case "tomcat":
		known = true
	case "mailpit":
		known = pair("--listen", "127.0.0.1:"+strconv.Itoa(r.port))
		path = os.Getenv("MP_WEBROOT")
		if value, ok := r.env["MP_WEBROOT"]; ok {
			path = value
		}
		for i, arg := range r.args {
			if arg == "--webroot" && i+1 < len(r.args) {
				path = r.args[i+1]
			}
			if value, ok := strings.CutPrefix(arg, "--webroot="); ok {
				path = value
			}
		}
	case "rustfs":
		known = pair("--address", ":"+strconv.Itoa(r.port)) && pair("--console-address", ":"+strconv.Itoa(r.port+1))
		port++
	case "zincsearch":
		known = r.env["ZINC_SERVER_PORT"] == strconv.Itoa(r.port)
	case "temporal-cli":
		known = pair("--port", strconv.Itoa(r.port)) && pair("--ui-port", strconv.Itoa(r.port+1000))
		port += 1000
	case "rnacos":
		known = r.env["RNACOS_HTTP_CONSOLE_PORT"] == strconv.Itoa(r.port+2000)
		port += 2000
	}
	if !known {
		return "", &AppError{Code: "SERVICE_WEB_UNAVAILABLE", Message: "当前服务配置没有可确认的管理台入口"}
	}
	if port < 1 || port > 65535 {
		return "", invalid("管理台端口超出范围")
	}
	if path == "" {
		path = "/"
	}
	if !strings.HasPrefix(path, "/") || strings.HasPrefix(path, "//") || strings.ContainsAny(path, "\r\n\x00?#\\") {
		return "", invalid("管理台页面路径无效")
	}
	owned := func() error {
		listeners, err := gnet.ConnectionsWithContext(ctx, "tcp")
		if err != nil {
			return err
		}
		found := false
		for _, listener := range listeners {
			if listener.Status != "LISTEN" || int(listener.Laddr.Port) != port {
				continue
			}
			if listener.Laddr.IP != "127.0.0.1" && listener.Laddr.IP != "0.0.0.0" && listener.Laddr.IP != "::" {
				continue
			}
			ok, err := child.OwnsPID(int(listener.Pid))
			if err != nil {
				return err
			}
			if !ok {
				return invalid("管理台端口已被其他进程占用")
			}
			found = true
		}
		if !found {
			return &AppError{Code: "SERVICE_PORT_UNVERIFIED", Message: "管理台端口尚未由当前受管服务监听，请稍后重试"}
		}
		return nil
	}
	if err := owned(); err != nil {
		return "", err
	}
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.Proxy = nil
	// Only the previously verified loopback listener is contacted, without any
	// credentials or redirects. The browser still applies its own TLS policy.
	transport.TLSClientConfig = &tls.Config{InsecureSkipVerify: true}
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, Timeout: 1500 * time.Millisecond, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	for _, scheme := range []string{"http", "https"} {
		u := url.URL{Scheme: scheme, Host: "127.0.0.1:" + strconv.Itoa(port), Path: path}
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
		if err != nil {
			return "", err
		}
		response, err := client.Do(req)
		if err != nil {
			continue
		}
		status := response.StatusCode
		reachable := status >= 200 && status < 300 && strings.HasPrefix(strings.ToLower(response.Header.Get("Content-Type")), "text/html") || status >= 300 && status < 400 && response.Header.Get("Location") != "" || status == 401
		_, _ = io.Copy(io.Discard, io.LimitReader(response.Body, 4096))
		response.Body.Close()
		if reachable {
			if err = owned(); err != nil {
				return "", err
			}
			return u.String(), nil
		}
	}
	return "", &AppError{Code: "SERVICE_WEB_UNAVAILABLE", Message: "管理台未返回可访问的网页，请检查 Web 界面、路径或访问限制"}
}
