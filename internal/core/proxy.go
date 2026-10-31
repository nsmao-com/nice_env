package core

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	gnet "github.com/shirou/gopsutil/v4/net"
	"gopkg.in/yaml.v3"
	"niceenv/internal/process"
)

const proxyPort = 17890
const proxyController = 19090
const proxyBuiltin = "proxies: []\nproxy-groups: []\nrules:\n  - MATCH,DIRECT\n"

type proxyProfile struct {
	ID      string `json:"id"`
	Name    string `json:"name"`
	URL     string `json:"url"`
	Active  bool   `json:"active"`
	AddedAt int64  `json:"addedAt"`
}

func (c *Core) proxyDir() string { return filepath.Join(c.Home, "etc", "mihomo") }
func (c *Core) proxyMode(ctx context.Context) (string, error) {
	mode, _, err := c.Store.setting(ctx, "proxyMode")
	if mode == "" {
		mode = "rule"
	}
	return mode, err
}
func (c *Core) proxySecret(ctx context.Context) (string, error) {
	secret, _, err := c.Store.setting(ctx, "proxyControllerSecret")
	if err != nil {
		return "", err
	}
	if secret != "" {
		return secret, nil
	}
	secret, err = newID("controller")
	if err != nil {
		return "", err
	}
	return secret, c.Store.saveSetting(ctx, "proxyControllerSecret", secret)
}
func proxyAdapt(raw, mode, secret string) (string, error) {
	if !slices.Contains([]string{"rule", "global", "direct"}, mode) {
		return "", invalid("代理模式无效")
	}
	if len(raw) > 8*1024*1024 {
		return "", invalid("代理配置超过 8 MiB")
	}
	var root map[string]any
	if err := yaml.Unmarshal([]byte(raw), &root); err != nil || root == nil {
		return "", &AppError{Code: "NOT_A_CLASH_CONFIG", Message: "订阅 YAML 格式无效，请使用 Clash / mihomo 配置"}
	}
	has := false
	for _, key := range []string{"proxies", "proxy-groups", "rules"} {
		if value, ok := root[key]; ok {
			if _, ok := value.([]any); !ok {
				return "", invalid("订阅字段 " + key + " 必须是列表")
			}
			if key != "proxy-groups" {
				has = true
			}
		}
	}
	if value, ok := root["proxy-providers"]; ok {
		switch value.(type) {
		case map[string]any, map[any]any:
			has = true
		default:
			return "", invalid("proxy-providers 必须为映射")
		}
	}
	if !has {
		return "", invalid("订阅未包含节点、节点提供者或规则")
	}
	for key, value := range map[string]any{"mixed-port": proxyPort, "port": 0, "socks-port": 0, "redir-port": 0, "tproxy-port": 0, "external-controller": fmt.Sprintf("127.0.0.1:%d", proxyController), "secret": secret, "allow-lan": false, "bind-address": "127.0.0.1", "mode": mode, "tun": map[string]any{"enable": false}, "dns": map[string]any{"enable": false}} {
		root[key] = value
	}
	for _, key := range []string{"external-ui", "external-ui-url", "external-controller-tls", "external-controller-unix", "external-controller-pipe", "listeners"} {
		delete(root, key)
	}
	data, err := yaml.Marshal(root)
	return string(data), err
}
func (c *Core) proxyConfig(ctx context.Context, raw, mode string) (string, error) {
	secret, err := c.proxySecret(ctx)
	if err != nil {
		return "", err
	}
	return proxyAdapt(raw, mode, secret)
}
func (c *Core) proxyValidate(ctx context.Context, text string, required bool) error {
	_, exe, err := c.installedTool(ctx, "mihomo")
	if err != nil {
		var ae *AppError
		if !required && errors.As(err, &ae) && ae.Code == "TOOL_NOT_INSTALLED" {
			return nil
		}
		return err
	}
	if err = managedDirectory(c.Home, c.proxyDir()); err != nil {
		return err
	}
	file, err := os.CreateTemp(c.proxyDir(), ".validate-*.yaml")
	if err != nil {
		return err
	}
	defer os.Remove(file.Name())
	_, writeErr := file.WriteString(text)
	closeErr := file.Close()
	if err = errors.Join(writeErr, closeErr); err != nil {
		return err
	}
	ctx, cancel := context.WithTimeout(ctx, 45*time.Second)
	defer cancel()
	output := process.NewLimitedBuffer(65536)
	child, err := process.Start(ctx, process.Spec{Program: exe, Args: []string{"-t", "-d", DisplayPath(c.proxyDir()), "-f", DisplayPath(file.Name())}, Directory: c.proxyDir(), Stdout: output, Stderr: output})
	if err != nil {
		return err
	}
	result := child.Wait()
	if result.Code != 0 || result.Err != nil {
		return &AppError{Code: "PROXY_CONFIG_INVALID", Message: "订阅未通过 mihomo 原生校验，原配置未改变", Hint: "请检查节点、规则和 provider 配置是否适用于当前内核"}
	}
	return nil
}
func (c *Core) prepareMihomo(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	mode, err := c.proxyMode(ctx)
	if err != nil {
		return err
	}
	path := filepath.Join(c.proxyDir(), "config.yaml")
	raw, err := readConfigText(path)
	if os.IsNotExist(err) {
		raw = proxyBuiltin
	} else if err != nil {
		return err
	}
	text, err := c.proxyConfig(ctx, raw, mode)
	if err != nil {
		return err
	}
	if err = c.proxyValidate(ctx, text, true); err != nil {
		return err
	}
	if err = c.writeConfig(ctx, configTarget{key: "mihomo-config", path: path}, text, nil); err != nil {
		return err
	}
	r.etc = c.proxyDir()
	r.cwd = c.proxyDir()
	r.args = []string{"-d", DisplayPath(r.etc), "-f", DisplayPath(path)}
	r.port = proxyPort
	r.ports = []int{proxyPort, proxyController}
	r.stopArgs = nil
	return checkServicePorts(ctx, r.ports)
}
func (c *Core) proxyRunning() bool {
	c.services.mu.RLock()
	defer c.services.mu.RUnlock()
	e := c.services.entries["mihomo"]
	return e != nil && e.child != nil && e.status.State == "running"
}

// Never send a controller secret to an unrelated process occupying the port.
func (c *Core) proxyAPI(ctx context.Context, method, path string, payload any) (json.RawMessage, error) {
	c.services.mu.RLock()
	e := c.services.entries["mihomo"]
	var child *process.Child
	if e != nil {
		child = e.child
	}
	c.services.mu.RUnlock()
	if child == nil {
		return nil, &AppError{Code: "PROXY_NOT_RUNNING", Message: "请先启动 mihomo 内核"}
	}
	listeners, err := gnet.ConnectionsWithContext(ctx, "tcp4")
	if err != nil {
		return nil, err
	}
	owned := false
	for _, l := range listeners {
		if int(l.Laddr.Port) == proxyController && l.Status == "LISTEN" {
			match, err := child.OwnsPID(int(l.Pid))
			if err != nil {
				return nil, err
			}
			if !match || l.Laddr.IP != "127.0.0.1" {
				return nil, invalid("代理控制端口不属于当前受管进程")
			}
			owned = true
		}
	}
	if !owned {
		return nil, &AppError{Code: "PROXY_NOT_READY", Message: "代理控制接口尚未就绪"}
	}
	secret, _, err := c.Store.setting(ctx, "proxyControllerSecret")
	if err != nil {
		return nil, err
	}
	var body []byte
	if payload != nil {
		body, err = json.Marshal(payload)
		if err != nil {
			return nil, err
		}
	}
	req, err := http.NewRequestWithContext(ctx, method, fmt.Sprintf("http://127.0.0.1:%d%s", proxyController, path), bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Authorization", "Bearer "+secret)
	req.Header.Set("Content-Type", "application/json")
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.Proxy = nil
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, Timeout: 8 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	resp, err := client.Do(req)
	if err != nil {
		return nil, &AppError{Code: "MIHOMO_API", Message: "无法连接 mihomo 控制接口"}
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, 8*1024*1024+1))
	if err != nil {
		return nil, err
	}
	if len(data) > 8*1024*1024 {
		return nil, invalid("代理响应过大")
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, &AppError{Code: "MIHOMO_API", Message: fmt.Sprintf("代理控制接口返回 HTTP %d", resp.StatusCode)}
	}
	if len(bytes.TrimSpace(data)) == 0 {
		return json.RawMessage(`{}`), nil
	}
	if !json.Valid(data) {
		return nil, invalid("代理响应格式无效")
	}
	return data, nil
}
func (c *Core) proxyApply(ctx context.Context, text string, commit func() error) error {
	if err := c.proxyValidate(ctx, text, false); err != nil {
		return err
	}
	target := configTarget{key: "mihomo-config", path: filepath.Join(c.proxyDir(), "config.yaml")}
	previous, err := readConfigText(target.path)
	exists := err == nil
	if err != nil && !os.IsNotExist(err) {
		return err
	}
	running := c.proxyRunning()
	if running && !exists {
		return invalid("运行中的代理缺少配置文件")
	}
	var expected *string
	if exists {
		expected = &previous
	}
	if err = c.writeConfigExpected(ctx, target, text, expected, !exists); err != nil {
		return err
	}
	if running {
		_, err = c.proxyAPI(ctx, http.MethodPut, "/configs?force=true", map[string]string{"payload": text})
	}
	if err == nil {
		err = commit()
	}
	if err == nil {
		return nil
	}
	cleanup, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()
	var recovery error
	if exists {
		recovery = c.writeConfig(cleanup, target, previous, &text)
	} else {
		recovery = os.Remove(target.path)
	}
	if running {
		_, restore := c.proxyAPI(cleanup, http.MethodPut, "/configs?force=true", map[string]string{"payload": previous})
		recovery = errors.Join(recovery, restore)
	}
	if recovery != nil {
		return &AppError{Code: "PROXY_RECOVERY_FAILED", Message: "代理配置应用失败，旧配置未完全恢复，请检查配置备份", Detail: recovery.Error()}
	}
	return err
}
func (c *Core) proxyProfiles(ctx context.Context) ([]proxyProfile, error) {
	rows, err := c.Store.db.QueryContext(ctx, "SELECT id,name,url,active,added_at FROM proxy_profiles ORDER BY added_at DESC")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	result := []proxyProfile{}
	for rows.Next() {
		var p proxyProfile
		if err = rows.Scan(&p.ID, &p.Name, &p.URL, &p.Active, &p.AddedAt); err != nil {
			return nil, err
		}
		result = append(result, p)
	}
	return result, rows.Err()
}
func (c *Core) proxyProfile(ctx context.Context, id string) (proxyProfile, string, error) {
	var p proxyProfile
	if !siteIdentifier.MatchString(id) {
		return p, "", invalid("订阅标识无效")
	}
	err := c.Store.db.QueryRowContext(ctx, "SELECT id,name,url,active,added_at FROM proxy_profiles WHERE id=?", id).Scan(&p.ID, &p.Name, &p.URL, &p.Active, &p.AddedAt)
	if err != nil {
		return p, "", &AppError{Code: "PROFILE_NOT_FOUND", Message: "代理订阅不存在"}
	}
	path := filepath.Join(c.proxyDir(), "profiles", id+".yaml")
	if err = managedDirectory(c.Home, filepath.Dir(path)); err != nil {
		return p, "", err
	}
	return p, path, nil
}
func (c *Core) fetchProxy(ctx context.Context, address string) (string, error) {
	u, err := url.Parse(strings.TrimSpace(address))
	if err != nil || len(address) > 8192 || (u.Scheme != "http" && u.Scheme != "https") || u.Hostname() == "" || u.User != nil || strings.IndexFunc(address, unicode.IsControl) >= 0 {
		return "", invalid("请输入不含登录凭据的 HTTP(S) 订阅地址")
	}
	transport := outboundTransport()
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, Timeout: 60 * time.Second}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
	if err != nil {
		return "", err
	}
	req.Header.Set("User-Agent", "clash-verge NiceEnv/"+c.Version)
	resp, err := client.Do(req)
	if err != nil {
		return "", &AppError{Code: "SUBSCRIPTION_DOWNLOAD_FAILED", Message: "下载订阅失败，请检查网络和地址"}
	}
	defer resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return "", &AppError{Code: "SUBSCRIPTION_DOWNLOAD_FAILED", Message: fmt.Sprintf("订阅服务器返回 HTTP %d", resp.StatusCode)}
	}
	data, err := io.ReadAll(io.LimitReader(resp.Body, 8*1024*1024+1))
	if err != nil {
		return "", err
	}
	if len(data) > 8*1024*1024 || !utf8.Valid(data) {
		return "", invalid("订阅超过 8 MiB 或不是 UTF-8 文本")
	}
	raw := strings.TrimSpace(strings.TrimPrefix(string(data), "\ufeff"))
	if !strings.Contains(raw, ":") {
		clean := strings.Join(strings.Fields(raw), "")
		for _, encoding := range []*base64.Encoding{base64.StdEncoding, base64.URLEncoding, base64.RawStdEncoding, base64.RawURLEncoding} {
			if decoded, err := encoding.DecodeString(clean); err == nil && utf8.Valid(decoded) {
				raw = string(decoded)
				break
			}
		}
	}
	return raw, nil
}

func (c *Core) registerProxy() {
	c.handle("proxy_profiles", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.proxyProfiles(ctx) })
	c.handle("proxy_status", func(ctx context.Context, _ json.RawMessage) (any, error) {
		mode, err := c.proxyMode(ctx)
		if err != nil {
			return nil, err
		}
		system, err := readSystemProxy(ctx)
		if err != nil {
			return nil, err
		}
		running := c.proxyRunning()
		var version any
		if running {
			raw, err := c.proxyAPI(ctx, http.MethodGet, "/configs", nil)
			if err != nil {
				return nil, err
			}
			var config struct {
				Mode string `json:"mode"`
			}
			if err = json.Unmarshal(raw, &config); err != nil {
				return nil, err
			}
			mode = config.Mode
			raw, err = c.proxyAPI(ctx, http.MethodGet, "/version", nil)
			if err != nil {
				return nil, err
			}
			var v struct {
				Version string `json:"version"`
			}
			if err = json.Unmarshal(raw, &v); err != nil {
				return nil, err
			}
			version = v.Version
		}
		return map[string]any{"running": running, "mixedPort": proxyPort, "controllerPort": proxyController, "mode": mode, "systemProxyEnabled": system.Enabled, "version": version}, nil
	})
	c.handle("proxy_start", func(ctx context.Context, _ json.RawMessage) (any, error) {
		return c.handlers["start_service"](ctx, json.RawMessage(`{"id":"mihomo"}`))
	})
	c.handle("proxy_stop", func(ctx context.Context, _ json.RawMessage) (any, error) {
		return c.handlers["stop_service"](ctx, json.RawMessage(`{"id":"mihomo"}`))
	})
	c.handle("proxy_set_system", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Enabled bool `json:"enabled"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		return true, c.setProxySystem(ctx, in.Enabled)
	})
	c.handle("proxy_set_mode", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Mode string `json:"mode"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		raw, err := readConfigText(filepath.Join(c.proxyDir(), "config.yaml"))
		if os.IsNotExist(err) {
			raw = proxyBuiltin
		} else if err != nil {
			return nil, err
		}
		text, err := c.proxyConfig(ctx, raw, in.Mode)
		if err != nil {
			return nil, err
		}
		return true, c.proxyApply(ctx, text, func() error { return c.Store.saveSetting(ctx, "proxyMode", in.Mode) })
	})
	c.handle("proxy_import", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Name string `json:"name"`
			URL  string `json:"url"`
		}](data)
		if err != nil {
			return nil, err
		}
		in.Name = strings.TrimSpace(in.Name)
		if in.Name == "" || utf8.RuneCountInString(in.Name) > 128 || strings.IndexFunc(in.Name, unicode.IsControl) >= 0 {
			return nil, invalid("订阅名称需为 1–128 个字符")
		}
		raw, err := c.fetchProxy(ctx, in.URL)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		mode, err := c.proxyMode(ctx)
		if err != nil {
			return nil, err
		}
		text, err := c.proxyConfig(ctx, raw, mode)
		if err != nil {
			return nil, err
		}
		if err = c.proxyValidate(ctx, text, false); err != nil {
			return nil, err
		}
		id, err := newID("profile")
		if err != nil {
			return nil, err
		}
		p := proxyProfile{ID: id, Name: in.Name, URL: strings.TrimSpace(in.URL), AddedAt: time.Now().UnixMilli()}
		path := filepath.Join(c.proxyDir(), "profiles", id+".yaml")
		if err = c.writeConfigExpected(ctx, configTarget{key: id, path: path}, text, nil, true); err != nil {
			return nil, err
		}
		_, err = c.Store.db.ExecContext(ctx, "INSERT INTO proxy_profiles(id,name,url,active,added_at) VALUES(?,?,?,0,?)", p.ID, p.Name, p.URL, p.AddedAt)
		if err != nil {
			return nil, errors.Join(err, os.Remove(path))
		}
		return p, nil
	})
	c.handle("proxy_activate_profile", func(ctx context.Context, data json.RawMessage) (any, error) {
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
		_, path, err := c.proxyProfile(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		raw, err := readConfigText(path)
		if err != nil {
			return nil, err
		}
		mode, err := c.proxyMode(ctx)
		if err != nil {
			return nil, err
		}
		text, err := c.proxyConfig(ctx, raw, mode)
		if err != nil {
			return nil, err
		}
		return true, c.proxyApply(ctx, text, func() error {
			_, err := c.Store.db.ExecContext(ctx, "UPDATE proxy_profiles SET active=(id=?)", in.ID)
			return err
		})
	})
	c.handle("proxy_delete_profile", func(ctx context.Context, data json.RawMessage) (any, error) {
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
		p, path, err := c.proxyProfile(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		if p.Active {
			return nil, &AppError{Code: "PROFILE_ACTIVE", Message: "请先切换订阅，再删除当前订阅"}
		}
		before, err := readConfigText(path)
		exists := err == nil
		if err != nil && !os.IsNotExist(err) {
			return nil, err
		}
		if exists {
			if err = os.Remove(path); err != nil {
				return nil, err
			}
		}
		_, err = c.Store.db.ExecContext(ctx, "DELETE FROM proxy_profiles WHERE id=?", in.ID)
		if err != nil && exists {
			err = errors.Join(err, c.writeConfig(context.Background(), configTarget{key: in.ID, path: path}, before, nil))
		}
		return true, err
	})
	c.handle("proxy_update_profile", c.updateProxyProfile)
	c.handle("proxy_connections", func(ctx context.Context, _ json.RawMessage) (any, error) {
		return c.proxyAPI(ctx, http.MethodGet, "/connections", nil)
	})
	c.handle("proxy_nodes", func(ctx context.Context, _ json.RawMessage) (any, error) {
		raw, err := c.proxyAPI(ctx, http.MethodGet, "/proxies", nil)
		if err != nil {
			return nil, err
		}
		return proxyGroups(raw)
	})
	c.handle("proxy_select_node", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Group string `json:"group"`
			Node  string `json:"node"`
		}](data)
		if err != nil {
			return nil, err
		}
		if in.Group == "" || in.Node == "" || len(in.Group) > 512 || len(in.Node) > 512 {
			return nil, invalid("请选择有效代理组和节点")
		}
		_, err = c.proxyAPI(ctx, http.MethodPut, "/proxies/"+url.PathEscape(in.Group), map[string]string{"name": in.Node})
		return true, err
	})
	c.handle("proxy_delay_test", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Node string `json:"node"`
		}](data)
		if err != nil {
			return nil, err
		}
		if in.Node == "" || len(in.Node) > 512 {
			return nil, invalid("请选择有效代理节点")
		}
		raw, err := c.proxyAPI(ctx, http.MethodGet, "/proxies/"+url.PathEscape(in.Node)+"/delay?timeout=5000&url=https%3A%2F%2Fwww.gstatic.com%2Fgenerate_204", nil)
		if err != nil {
			return nil, err
		}
		var result struct {
			Delay *uint32 `json:"delay"`
		}
		if err = json.Unmarshal(raw, &result); err != nil {
			return nil, err
		}
		if result.Delay == nil {
			return nil, &AppError{Code: "DELAY_TIMEOUT", Message: "测速超时或节点不可用"}
		}
		return *result.Delay, nil
	})
}
func (c *Core) updateProxyProfile(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		ID string `json:"id"`
	}](data)
	if err != nil {
		return nil, err
	}
	p, path, err := c.proxyProfile(ctx, in.ID)
	if err != nil {
		return nil, err
	}
	before, err := readConfigText(path)
	if err != nil {
		return nil, err
	}
	raw, err := c.fetchProxy(ctx, p.URL)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	current, _, err := c.proxyProfile(ctx, in.ID)
	if err != nil {
		return nil, err
	}
	now, err := readConfigText(path)
	if err != nil {
		return nil, err
	}
	if now != before || current.URL != p.URL {
		return nil, &AppError{Code: "PROFILE_CHANGED", Message: "下载期间订阅已变化，请重新更新"}
	}
	mode, err := c.proxyMode(ctx)
	if err != nil {
		return nil, err
	}
	text, err := c.proxyConfig(ctx, raw, mode)
	if err != nil {
		return nil, err
	}
	if err = c.proxyValidate(ctx, text, false); err != nil {
		return nil, err
	}
	target := configTarget{key: in.ID, path: path}
	if err = c.writeConfig(ctx, target, text, &before); err != nil {
		return nil, err
	}
	if current.Active {
		if err = c.proxyApply(ctx, text, func() error { return nil }); err != nil {
			return nil, errors.Join(err, c.writeConfig(context.Background(), target, before, &text))
		}
	}
	return current.Active, nil
}
func proxyGroups(raw []byte) ([]map[string]any, error) {
	type node struct {
		Name    string   `json:"name"`
		Type    string   `json:"type"`
		Now     string   `json:"now"`
		All     []string `json:"all"`
		Alive   *bool    `json:"alive"`
		History []struct {
			Delay *uint32 `json:"delay"`
		} `json:"history"`
	}
	var result struct {
		Proxies map[string]node `json:"proxies"`
	}
	if err := json.Unmarshal(raw, &result); err != nil || result.Proxies == nil {
		return nil, invalid("代理节点响应格式无效")
	}
	groups := []map[string]any{}
	for _, group := range result.Proxies {
		if !slices.Contains([]string{"Selector", "URLTest", "Fallback", "LoadBalance", "Relay"}, group.Type) {
			continue
		}
		nodes := []map[string]any{}
		for _, name := range group.All {
			n, ok := result.Proxies[name]
			if !ok {
				continue
			}
			history := []uint32{}
			for _, h := range n.History {
				if h.Delay != nil {
					history = append(history, *h.Delay)
				}
			}
			nodes = append(nodes, map[string]any{"name": n.Name, "type": n.Type, "alive": n.Alive, "history": history})
		}
		groups = append(groups, map[string]any{"name": group.Name, "type": group.Type, "now": group.Now, "nodes": nodes})
	}
	sort.Slice(groups, func(i, j int) bool { return groups[i]["name"].(string) < groups[j]["name"].(string) })
	return groups, nil
}
