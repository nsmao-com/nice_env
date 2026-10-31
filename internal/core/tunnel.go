package core

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"niceenv/internal/process"
)

type tunnelInfo struct {
	ID             string   `json:"id"`
	Port           int      `json:"port"`
	URL            *string  `json:"url"`
	StartedAt      int64    `json:"startedAt"`
	Alive          bool     `json:"alive"`
	State          string   `json:"state"`
	Target         string   `json:"target"`
	SiteID         *string  `json:"siteId"`
	Error          *string  `json:"error"`
	Logs           []string `json:"logs"`
	LocalReachable *bool    `json:"localReachable"`
}
type tunnelEntry struct {
	mu               sync.Mutex
	info             tunnelInfo
	child            *process.Child
	metrics          string
	host, scheme, ca string
	site             *Site
	pids             []int
	stopMu           sync.Mutex
	done             chan struct{}
}
type tunnelManager struct {
	mu      sync.Mutex
	entries []*tunnelEntry
	closing bool
	wg      sync.WaitGroup
}

func (e *tunnelEntry) snapshot() tunnelInfo {
	e.mu.Lock()
	defer e.mu.Unlock()
	i := e.info
	i.Logs = append([]string{}, i.Logs...)
	return i
}

func (c *Core) registerTunnels() {
	c.handle("tunnel_list", func(context.Context, json.RawMessage) (any, error) {
		c.tunnels.mu.Lock()
		defer c.tunnels.mu.Unlock()
		result := []tunnelInfo{}
		for _, e := range c.tunnels.entries {
			result = append(result, e.snapshot())
		}
		return result, nil
	})
	c.handle("tunnel_start", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Port int `json:"port"`
		}](data)
		if err != nil {
			return nil, err
		}
		if in.Port < 1 || in.Port > 65535 {
			return nil, invalid("本地 HTTP 端口必须为 1–65535")
		}
		return c.startTunnel(ctx, &tunnelEntry{info: tunnelInfo{Port: in.Port, Target: fmt.Sprintf("http://127.0.0.1:%d", in.Port)}, scheme: "http", host: "127.0.0.1"})
	})
	c.handle("tunnel_start_site", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		site, err := c.site(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		address, err := c.siteAccessURL(ctx, site)
		if err != nil {
			return nil, err
		}
		u, err := url.Parse(address)
		if err != nil {
			return nil, err
		}
		host := u.Hostname()
		if host == "" || strings.ContainsAny(host, "\\\r\n") || net.ParseIP(host) != nil && host != "127.0.0.1" {
			return nil, invalid("请为站点设置本地域名后创建隧道")
		}
		port := 80
		if u.Scheme == "https" {
			port = 443
		}
		if u.Port() != "" {
			port, err = strconv.Atoi(u.Port())
			if err != nil {
				return nil, err
			}
		}
		e := &tunnelEntry{info: tunnelInfo{Port: port, Target: address, SiteID: &site.ID}, host: host, scheme: u.Scheme, site: &site}
		c.services.mu.RLock()
		if service := c.services.entries[site.Runtime.WebServer]; service != nil {
			e.pids = append([]int{}, service.status.PIDs...)
		}
		c.services.mu.RUnlock()
		if site.HTTPS {
			c.certChanges.Lock()
			defer c.certChanges.Unlock()
			record, err := c.siteCertificate(ctx, site, false)
			if err != nil {
				return nil, err
			}
			path := string(record.CertPath)
			if site.Runtime.ImportedCertID == nil && site.Runtime.ACMECertID == nil {
				path = filepath.Join(c.Home, "certs", "ca.crt")
			}
			pem, err := c.readCertFile(path)
			if err != nil {
				return nil, err
			}
			e.ca = string(pem)
		}
		return c.startTunnel(ctx, e)
	})
	c.handle("tunnel_stop", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		e, err := c.findTunnel(in.ID)
		if err != nil {
			return nil, err
		}
		return true, e.stop(ctx, "")
	})
	c.handle("tunnel_remove", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		c.tunnels.mu.Lock()
		defer c.tunnels.mu.Unlock()
		for i, e := range c.tunnels.entries {
			if e.info.ID == in.ID {
				if e.snapshot().Alive {
					return nil, &AppError{Code: "TUNNEL_RUNNING", Message: "请先停止隧道，再移除记录"}
				}
				c.tunnels.entries = append(c.tunnels.entries[:i], c.tunnels.entries[i+1:]...)
				return true, nil
			}
		}
		return nil, &AppError{Code: "TUNNEL_NOT_FOUND", Message: "隧道记录不存在"}
	})
}

func (c *Core) findTunnel(id string) (*tunnelEntry, error) {
	c.tunnels.mu.Lock()
	defer c.tunnels.mu.Unlock()
	for _, e := range c.tunnels.entries {
		if e.info.ID == id {
			return e, nil
		}
	}
	return nil, &AppError{Code: "TUNNEL_NOT_FOUND", Message: "隧道记录不存在"}
}

func (e *tunnelEntry) client() (*http.Client, error) {
	t := http.DefaultTransport.(*http.Transport).Clone()
	t.Proxy = nil
	t.DisableKeepAlives = true
	t.DialContext = func(ctx context.Context, network, address string) (net.Conn, error) {
		return (&net.Dialer{Timeout: 2 * time.Second}).DialContext(ctx, "tcp", net.JoinHostPort("127.0.0.1", strconv.Itoa(e.info.Port)))
	}
	if e.scheme == "https" {
		roots := x509.NewCertPool()
		if !roots.AppendCertsFromPEM([]byte(e.ca)) {
			return nil, invalid("站点可信证书为空或无效")
		}
		t.TLSClientConfig = &tls.Config{MinVersion: tls.VersionTLS12, RootCAs: roots, ServerName: e.host}
	}
	return &http.Client{Transport: t, Timeout: 2 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}, nil
}
func (e *tunnelEntry) probe(ctx context.Context, client *http.Client) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodHead, e.info.Target, nil)
	if err != nil {
		return err
	}
	req.Host = e.host
	resp, err := client.Do(req)
	if err != nil {
		return &AppError{Code: "TUNNEL_ORIGIN_UNAVAILABLE", Message: "本地站点无法访问，请检查服务及 HTTPS 证书", Detail: err.Error()}
	}
	resp.Body.Close()
	return nil
}
func (c *Core) startTunnel(ctx context.Context, e *tunnelEntry) (tunnelInfo, error) {
	c.tunnels.mu.Lock()
	defer c.tunnels.mu.Unlock()
	if c.tunnels.closing {
		return tunnelInfo{}, &AppError{Code: "TUNNEL_SHUTDOWN", Message: "应用正在退出"}
	}
	for _, old := range c.tunnels.entries {
		i := old.snapshot()
		if i.Alive && i.Target == e.info.Target && old.ca == e.ca && slices.Equal(old.pids, e.pids) && ((i.SiteID == nil && e.info.SiteID == nil) || (i.SiteID != nil && e.info.SiteID != nil && *i.SiteID == *e.info.SiteID)) {
			return i, nil
		}
	}
	if len(c.tunnels.entries) >= 20 {
		i := slices.IndexFunc(c.tunnels.entries, func(e *tunnelEntry) bool { return !e.snapshot().Alive })
		if i < 0 {
			return tunnelInfo{}, &AppError{Code: "TUNNEL_LIMIT", Message: "最多同时运行 20 条隧道"}
		}
		c.tunnels.entries = append(c.tunnels.entries[:i], c.tunnels.entries[i+1:]...)
	}
	client, err := e.client()
	if err != nil {
		return tunnelInfo{}, err
	}
	defer client.CloseIdleConnections()
	if err = e.probe(ctx, client); err != nil {
		return tunnelInfo{}, err
	}
	_, exe, err := c.installedTool(ctx, "cloudflared")
	if err != nil {
		return tunnelInfo{}, err
	}
	dir, err := os.MkdirTemp("", "niceenv-tunnel-")
	if err != nil {
		return tunnelInfo{}, err
	}
	cleanup := func() { _ = os.RemoveAll(dir) }
	if err = os.WriteFile(filepath.Join(dir, "config.yml"), []byte("{}\n"), 0600); err != nil {
		cleanup()
		return tunnelInfo{}, err
	}
	args := []string{"tunnel", "--no-autoupdate", "--config", DisplayPath(filepath.Join(dir, "config.yml")), "--url", fmt.Sprintf("%s://127.0.0.1:%d", e.scheme, e.info.Port), "--metrics", "127.0.0.1:0", "--output", "json"}
	if e.site != nil {
		args = append(args, "--http-host-header", e.host)
		if e.scheme == "https" {
			args = append(args, "--origin-server-name", e.host)
		}
	}
	if e.ca != "" {
		path := filepath.Join(dir, "origin-ca.pem")
		if err = os.WriteFile(path, []byte(e.ca), 0600); err != nil {
			cleanup()
			return tunnelInfo{}, err
		}
		args = append(args, "--origin-ca-pool", DisplayPath(path))
	}
	remove := []string{}
	for _, env := range os.Environ() {
		key, _, _ := strings.Cut(env, "=")
		if strings.HasPrefix(strings.ToUpper(key), "TUNNEL_") {
			remove = append(remove, key)
		}
	}
	id, err := newID("tunnel")
	if err != nil {
		cleanup()
		return tunnelInfo{}, err
	}
	reachable := true
	e.info.ID = id
	e.info.Alive = true
	e.info.State = "starting"
	e.info.StartedAt = time.Now().UnixMilli()
	e.info.Logs = []string{}
	e.info.LocalReachable = &reachable
	e.done = make(chan struct{})
	output := &tunnelOutput{entry: e}
	child, err := process.Start(context.Background(), process.Spec{Program: exe, Args: args, Directory: dir, RemoveEnvironment: remove, Stdout: output, Stderr: output})
	if err != nil {
		cleanup()
		return tunnelInfo{}, err
	}
	e.child = child
	c.tunnels.entries = append(c.tunnels.entries, e)
	c.tunnels.wg.Add(1)
	go func() { defer c.tunnels.wg.Done(); defer close(e.done); defer cleanup(); c.monitorTunnel(e, client) }()
	return e.snapshot(), nil
}

func (e *tunnelEntry) stop(ctx context.Context, failure string) error {
	e.stopMu.Lock()
	defer e.stopMu.Unlock()
	if !e.snapshot().Alive {
		return nil
	}
	if err := e.child.Stop(); err != nil {
		return err
	}
	timer := time.NewTimer(5 * time.Second)
	defer timer.Stop()
	select {
	case <-e.child.Done():
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return &AppError{Code: "TUNNEL_STOP_TIMEOUT", Message: "隧道尚未退出，请再次停止"}
	}
	e.mu.Lock()
	defer e.mu.Unlock()
	e.info.Alive = false
	e.info.State = "stopped"
	e.info.Error = nil
	if failure != "" {
		e.info.State = "failed"
		e.info.Error = &failure
	}
	return nil
}

func (c *Core) monitorTunnel(e *tunnelEntry, origin *http.Client) {
	defer origin.CloseIdleConnections()
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.Proxy = nil
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, Timeout: 2 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	last := time.Now()
	connected := false
	ticker := time.NewTicker(2 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-c.closed:
			_ = e.stop(context.Background(), "")
			return
		case <-e.child.Done():
			e.stopMu.Lock()
			e.mu.Lock()
			if e.info.Alive {
				message := "cloudflared 已退出，请查看输出后重试"
				e.info.Alive = false
				e.info.State = "failed"
				e.info.Error = &message
			}
			e.mu.Unlock()
			e.stopMu.Unlock()
			return
		case <-ticker.C:
		}
		if e.site != nil && c.services.lifecycle.TryLock() {
			ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
			site, err := c.site(ctx, e.site.ID)
			address := ""
			if err == nil {
				address, err = c.siteAccessURL(ctx, site)
			}
			cancel()
			c.services.mu.RLock()
			service := c.services.entries[e.site.Runtime.WebServer]
			same := service != nil && slices.Equal(service.status.PIDs, e.pids)
			c.services.mu.RUnlock()
			c.services.lifecycle.Unlock()
			if err != nil || address != e.info.Target || !same {
				_ = e.stop(context.Background(), "站点已停止、重启或地址已变化，请重新创建隧道")
				return
			}
		}
		local := e.probe(context.Background(), origin) == nil
		e.mu.Lock()
		metrics := e.metrics
		hasURL := e.info.URL != nil
		e.mu.Unlock()
		ready := false
		if metrics != "" {
			if resp, err := client.Get(metrics + "/ready"); err == nil {
				var body struct {
					ReadyConnections int `json:"readyConnections"`
				}
				err = json.NewDecoder(io.LimitReader(resp.Body, 65536)).Decode(&body)
				resp.Body.Close()
				ready = err == nil && resp.StatusCode == 200 && body.ReadyConnections > 0
			}
		}
		e.mu.Lock()
		if e.info.Alive {
			e.info.LocalReachable = &local
			if ready && local && hasURL {
				e.info.State = "connected"
				e.info.Error = nil
				connected = true
				last = time.Now()
			} else if connected {
				e.info.State = "reconnecting"
			} else {
				e.info.State = "starting"
			}
		}
		e.mu.Unlock()
		if time.Since(last) > 90*time.Second {
			_ = e.stop(context.Background(), "90 秒内未建立或恢复可用隧道，请检查网络、本地站点及输出")
			return
		}
	}
}

func (c *Core) stopTunnels(ctx context.Context) error {
	c.tunnels.mu.Lock()
	c.tunnels.closing = true
	entries := append([]*tunnelEntry{}, c.tunnels.entries...)
	c.tunnels.mu.Unlock()
	var result error
	for _, e := range entries {
		result = errors.Join(result, e.stop(ctx, ""))
	}
	return result
}

type tunnelOutput struct {
	mu    sync.Mutex
	entry *tunnelEntry
	line  []byte
}

var tunnelURL = regexp.MustCompile(`https://([a-zA-Z0-9][a-zA-Z0-9-]{0,61}[a-zA-Z0-9]|[a-zA-Z0-9])\.trycloudflare\.com\b`)
var tunnelMetrics = regexp.MustCompile(`Starting metrics server on 127\.0\.0\.1:([0-9]+)/metrics`)

func (w *tunnelOutput) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	for _, b := range p {
		if b != '\n' {
			if len(w.line) < 8192 {
				w.line = append(w.line, b)
			}
			continue
		}
		line := strings.TrimSpace(strings.ToValidUTF8(string(w.line), "�"))
		w.line = w.line[:0]
		if line == "" {
			continue
		}
		message := line
		var parsed struct {
			Message string `json:"message"`
		}
		if json.Unmarshal([]byte(line), &parsed) == nil && parsed.Message != "" {
			message = parsed.Message
		}
		e := w.entry
		e.mu.Lock()
		if e.info.URL == nil {
			if m := tunnelURL.FindStringSubmatch(message); len(m) > 0 && m[1] != "api" {
				address := strings.ToLower(m[0])
				e.info.URL = &address
			}
		}
		if m := tunnelMetrics.FindStringSubmatch(message); len(m) > 0 {
			if port, err := strconv.Atoi(m[1]); err == nil && port > 0 && port <= 65535 {
				e.metrics = "http://127.0.0.1:" + m[1]
			}
		}
		if len(line) > 2048 {
			line = string([]rune(line)[:min(2048, len([]rune(line)))]) + " …（已截断）"
		}
		e.info.Logs = append(e.info.Logs, line)
		if len(e.info.Logs) > 80 {
			e.info.Logs = e.info.Logs[len(e.info.Logs)-80:]
		}
		e.mu.Unlock()
	}
	return len(p), nil
}
