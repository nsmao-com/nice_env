package core

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/tls"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"path/filepath"
	"strconv"
	"strings"
	"time"
	"unicode"

	"niceenv/internal/process"
)

type certMonitor struct {
	ID                string `json:"id"`
	Host              string `json:"host"`
	Port              int    `json:"port"`
	Name              string `json:"name"`
	State             string `json:"state"`
	Issuer            string `json:"issuer"`
	ExpiresAt         *int64 `json:"expiresAt"`
	LastChecked       *int64 `json:"lastChecked"`
	LastError         string `json:"lastError"`
	NotificationError string `json:"notificationError,omitempty"`
	CreatedAt         int64  `json:"createdAt"`
	UpdatedAt         int64  `json:"updatedAt"`
}
type monitorNotification struct {
	Kind string `json:"kind"`
	URL  string `json:"url"`
}

func validateMonitorNotification(s monitorNotification) error {
	if !map[string]bool{"none": true, "generic": true, "dingtalk": true, "wecom": true, "feishu": true}[s.Kind] {
		return invalid("通知方式无效")
	}
	if s.Kind == "none" {
		return nil
	}
	u, e := url.Parse(s.URL)
	if e != nil || u.Hostname() == "" || (u.Scheme != "http" && u.Scheme != "https") || u.User != nil || u.Fragment != "" {
		return invalid("通知地址必须是完整 HTTP/HTTPS URL")
	}
	return nil
}
func (c *Core) monitorNotifications(ctx context.Context) (monitorNotification, error) {
	settings := monitorNotification{Kind: "none"}
	rows, err := c.Store.db.QueryContext(ctx, "SELECT key,value FROM settings WHERE key IN ('monitorNotifyKind','monitorNotifyUrl')")
	if err != nil {
		return settings, err
	}
	defer rows.Close()
	for rows.Next() {
		var key, value string
		if err = rows.Scan(&key, &value); err != nil {
			return settings, err
		}
		if key == "monitorNotifyKind" {
			settings.Kind = value
		} else {
			settings.URL = value
		}
	}
	return settings, rows.Err()
}
func (c *Core) pushMonitorAlert(ctx context.Context, endpoint, state, message string) error {
	settings, err := c.monitorNotifications(ctx)
	if err != nil {
		return err
	}
	if err = validateMonitorNotification(settings); err != nil {
		return err
	}
	if settings.Kind == "none" {
		return nil
	}
	payload := map[string]any{"event": "certificate_alert", "host": endpoint, "state": state, "message": message, "at": time.Now().UTC().Format(time.RFC3339), "source": "NiceEnv"}
	if settings.Kind == "dingtalk" || settings.Kind == "wecom" {
		payload = map[string]any{"msgtype": "text", "text": map[string]string{"content": "[NiceEnv] 证书监控告警：" + endpoint + "\n" + message}}
	}
	if settings.Kind == "feishu" {
		payload = map[string]any{"msg_type": "text", "content": map[string]string{"text": "[NiceEnv] 证书监控告警：" + endpoint + "\n" + message}}
	}
	body, err := json.Marshal(payload)
	if err != nil {
		return err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, settings.URL, bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	client := http.Client{Timeout: 10 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("通知端点返回 %s", resp.Status)
	}
	if settings.Kind != "generic" {
		var result map[string]json.RawMessage
		if err = json.NewDecoder(io.LimitReader(resp.Body, 64*1024)).Decode(&result); err != nil {
			return invalid("通知端点未返回有效确认")
		}
		key := "errcode"
		if settings.Kind == "feishu" {
			key = "code"
			if _, ok := result[key]; !ok {
				key = "StatusCode"
			}
		}
		var code int
		if raw, ok := result[key]; !ok || json.Unmarshal(raw, &code) != nil || code != 0 {
			return invalid("通知被服务端拒绝，请检查机器人设置")
		}
	}
	return nil
}

func normalizeMonitor(raw string, port int) (string, int, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" || len(raw) > 2048 || port < 1 || port > 65535 || strings.ContainsRune(raw, '\\') || strings.IndexFunc(raw, func(r rune) bool { return unicode.IsSpace(r) || unicode.IsControl(r) }) >= 0 {
		return "", 0, invalid("请填写域名、IP、host:port 或 HTTPS 地址")
	}
	if net.ParseIP(raw) != nil {
		return raw, port, nil
	}
	{
		withScheme := strings.Contains(raw, "://")
		address := raw
		if !withScheme {
			address = "https://" + address
		}
		u, err := url.Parse(address)
		if err != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil {
			return "", 0, invalid("监控地址必须是 HTTPS 且不能包含凭据")
		}
		raw = u.Hostname()
		if u.Port() != "" {
			port, _ = strconv.Atoi(u.Port())
		} else if withScheme {
			port = 443
		}
		if strings.HasSuffix(u.Host, ":") {
			return "", 0, invalid("端口不能为空")
		}
		if port < 1 || port > 65535 {
			return "", 0, invalid("端口必须为 1–65535")
		}
	}
	host := strings.TrimSuffix(strings.Trim(raw, "[]"), ".")
	if host == "" {
		return "", 0, invalid("域名不能为空")
	}
	if net.ParseIP(host) == nil {
		if len(host) > 253 || strings.IndexFunc(host, func(r rune) bool {
			return !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '-' || r == '.')
		}) >= 0 {
			return "", 0, invalid("域名格式无效")
		}
		for _, label := range strings.Split(host, ".") {
			if len(label) == 0 || len(label) > 63 || strings.HasPrefix(label, "-") || strings.HasSuffix(label, "-") {
				return "", 0, invalid("域名格式无效")
			}
		}
	}
	return strings.ToLower(host), port, nil
}

func (c *Core) monitorLoad(ctx context.Context) ([]certMonitor, error) {
	rows, err := c.Store.db.QueryContext(ctx, "SELECT data FROM cert_monitors ORDER BY updated_at DESC")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []certMonitor{}
	for rows.Next() {
		var text string
		if err = rows.Scan(&text); err != nil {
			return nil, err
		}
		var m certMonitor
		if err = json.Unmarshal([]byte(text), &m); err != nil {
			return nil, &AppError{Code: "MONITOR_CORRUPT", Message: "证书监控记录损坏"}
		}
		out = append(out, m)
	}
	return out, rows.Err()
}
func (c *Core) monitorFind(ctx context.Context, id string) (certMonitor, error) {
	var text string
	err := c.Store.db.QueryRowContext(ctx, "SELECT data FROM cert_monitors WHERE id=?", id).Scan(&text)
	if err != nil {
		return certMonitor{}, err
	}
	var m certMonitor
	err = json.Unmarshal([]byte(text), &m)
	return m, err
}

func (c *Core) monitorLock(id string) (func() error, error) {
	dir := filepath.Join(c.Home, "certmonitor-locks")
	if err := managedDirectory(c.Home, dir); err != nil {
		return nil, err
	}
	digest := sha256.Sum256([]byte(id))
	unlock, err := process.Lock(filepath.Join(dir, hex.EncodeToString(digest[:])+".lock"))
	if errors.Is(err, process.ErrLocked) {
		return nil, &AppError{Code: "MONITOR_BUSY", Message: "该证书监控正在检查，请稍后重试"}
	}
	return unlock, err
}

func (c *Core) completeMonitor(ctx context.Context, m certMonitor, expected int64) error {
	text, err := json.Marshal(m)
	if err != nil {
		return err
	}
	result, err := c.Store.db.ExecContext(ctx, "UPDATE cert_monitors SET data=?,updated_at=? WHERE id=? AND updated_at=?", string(text), m.UpdatedAt, m.ID, expected)
	if err != nil {
		return err
	}
	n, err := result.RowsAffected()
	if err != nil {
		return err
	}
	if n != 1 {
		return &AppError{Code: "MONITOR_CHANGED", Message: "监控已删除或修改，本次检查结果未覆盖新记录"}
	}
	return nil
}
func monitorState(exp *int64) string {
	if exp == nil {
		return "idle"
	}
	now := time.Now().UnixMilli()
	if *exp <= now {
		return "expired"
	}
	if *exp-now <= 30*24*time.Hour.Milliseconds() {
		return "expiring"
	}
	return "ok"
}
func (c *Core) certMonitorCheck(ctx context.Context, id string) (certMonitor, error) {
	unlock, err := c.monitorLock(id)
	if err != nil {
		return certMonitor{}, err
	}
	defer unlock()
	m, err := c.monitorFind(ctx, id)
	if err != nil {
		return m, err
	}
	previous, revision := m.State, m.UpdatedAt
	host, port, err := normalizeMonitor(m.Host, m.Port)
	if err != nil {
		return m, err
	}
	probeCtx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	// This probe reads certificates, including expired/self-signed ones. It never
	// sends HTTP requests, credentials or application data over this connection.
	dialer := &tls.Dialer{NetDialer: &net.Dialer{Timeout: 10 * time.Second}, Config: &tls.Config{ServerName: host, InsecureSkipVerify: true, MinVersion: tls.VersionTLS12}}
	conn, err := dialer.DialContext(probeCtx, "tcp", net.JoinHostPort(host, strconv.Itoa(port)))
	if err != nil {
		if ctx.Err() != nil {
			return m, ctx.Err()
		}
		m.State = "error"
		m.LastError = err.Error()
	} else {
		certs := conn.(*tls.Conn).ConnectionState().PeerCertificates
		conn.Close()
		if len(certs) == 0 {
			return m, invalid("对端没有出示证书")
		}
		leaf := certs[0]
		exp := leaf.NotAfter.UnixMilli()
		m.Issuer = leaf.Issuer.String()
		m.ExpiresAt = &exp
		m.State = monitorState(&exp)
		m.LastError = ""
		if leaf.NotBefore.After(time.Now()) {
			m.State = "error"
			m.LastError = "证书尚未生效，请检查服务器证书与本机时间"
		}
	}
	now := time.Now().UnixMilli()
	m.LastChecked = &now
	m.UpdatedAt = max(now, revision+1)
	if err = c.completeMonitor(ctx, m, revision); err != nil {
		return m, err
	}
	alert := m.State == "error" || m.State == "expired" || m.State == "expiring"
	if alert && (m.State != previous || m.NotificationError != "") {
		message := m.LastError
		if m.ExpiresAt != nil && m.State != "error" {
			message = fmt.Sprintf("证书到期时间：%s", time.UnixMilli(*m.ExpiresAt).Format(time.RFC3339))
		}
		endpoint := net.JoinHostPort(m.Host, strconv.Itoa(m.Port))
		if m.State != previous && c.emit != nil {
			c.emit("certmonitor://alert", map[string]string{"host": endpoint, "state": m.State, "message": message})
		}
		m.NotificationError = ""
		if err = c.pushMonitorAlert(ctx, endpoint, m.State, message); err != nil {
			m.NotificationError = "告警推送失败，请检查通知配置；下次检查会重试"
		}
	} else if !alert {
		m.NotificationError = ""
	}
	revision = m.UpdatedAt
	m.UpdatedAt = max(time.Now().UnixMilli(), revision+1)
	return m, c.completeMonitor(ctx, m, revision)
}

func (c *Core) monitorScheduler(parent context.Context) {
	defer c.cron.wg.Done()
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	go func() {
		select {
		case <-c.closed:
			cancel()
		case <-ctx.Done():
		}
	}()
	ticker := time.NewTicker(time.Minute)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
		monitors, err := c.monitorLoad(ctx)
		if err != nil {
			continue
		}
		for _, m := range monitors {
			if ctx.Err() != nil {
				return
			}
			if m.LastChecked != nil && time.Since(time.UnixMilli(*m.LastChecked)) < time.Hour {
				continue
			}
			_, _ = c.certMonitorCheck(ctx, m.ID)
		}
	}
}
func (c *Core) registerCertMonitors() {
	c.handle("certmonitor_list", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.monitorLoad(ctx) })
	c.handle("certmonitor_notification_get", func(ctx context.Context, _ json.RawMessage) (any, error) {
		return c.monitorNotifications(ctx)
	})
	c.handle("certmonitor_notification_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Settings monitorNotification `json:"settings"`
		}](data)
		if err != nil {
			return nil, err
		}
		in.Settings.URL = strings.TrimSpace(in.Settings.URL)
		if err = validateMonitorNotification(in.Settings); err != nil {
			return nil, err
		}
		tx, err := c.Store.db.BeginTx(ctx, nil)
		if err != nil {
			return nil, err
		}
		defer tx.Rollback()
		for key, value := range map[string]string{"monitorNotifyKind": in.Settings.Kind, "monitorNotifyUrl": in.Settings.URL} {
			if _, err = tx.ExecContext(ctx, "INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", key, value); err != nil {
				return nil, err
			}
		}
		return in.Settings, tx.Commit()
	})
	c.handle("certmonitor_add", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Monitor certMonitor `json:"m"`
		}](data)
		if err != nil {
			return nil, err
		}
		if in.Monitor.ID != "" {
			return nil, invalid("新增监控不能覆盖已有记录")
		}
		host, port, err := normalizeMonitor(in.Monitor.Host, in.Monitor.Port)
		if err != nil {
			return nil, err
		}
		unlock, err := c.monitorLock("create")
		if err != nil {
			return nil, err
		}
		defer unlock()
		list, err := c.monitorLoad(ctx)
		if err != nil {
			return nil, err
		}
		if len(list) >= 500 {
			return nil, invalid("证书监控最多支持 500 个端点")
		}
		for _, old := range list {
			h, p, e := normalizeMonitor(old.Host, old.Port)
			if e == nil && h == host && p == port {
				return nil, &AppError{Code: "MONITOR_EXISTS", Message: "该端点已存在监控"}
			}
		}
		id, err := newID("mon")
		if err != nil {
			return nil, err
		}
		name := strings.TrimSpace(in.Monitor.Name)
		if len(name) > 512 || strings.IndexFunc(name, unicode.IsControl) >= 0 {
			return nil, invalid("监控名称过长或含控制字符")
		}
		if name == "" {
			name = net.JoinHostPort(host, strconv.Itoa(port))
		}
		now := time.Now().UnixMilli()
		m := certMonitor{ID: id, Host: host, Port: port, Name: name, State: "idle", CreatedAt: now, UpdatedAt: now}
		text, err := json.Marshal(m)
		if err != nil {
			return nil, err
		}
		if _, err = c.Store.db.ExecContext(ctx, "INSERT INTO cert_monitors(id,data,updated_at) VALUES(?,?,?)", m.ID, string(text), now); err != nil {
			return nil, err
		}
		return m, nil
	})
	c.handle("certmonitor_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		r, err := c.Store.db.ExecContext(ctx, "DELETE FROM cert_monitors WHERE id=?", in.ID)
		if err != nil {
			return nil, err
		}
		n, err := r.RowsAffected()
		if err != nil {
			return nil, err
		}
		if n == 0 {
			return nil, &AppError{Code: "NOT_FOUND", Message: "监控不存在"}
		}
		return true, nil
	})
	c.handle("certmonitor_check", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		return c.certMonitorCheck(ctx, in.ID)
	})
}
