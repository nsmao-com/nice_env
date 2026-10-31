package core

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"

	gnet "github.com/shirou/gopsutil/v4/net"
	"niceenv/internal/process"
)

type networkAddress struct {
	Interface string `json:"interface"`
	Address   string `json:"address"`
	Listening bool   `json:"listening"`
}
type siteNetworkInfo struct {
	Server        string           `json:"server"`
	Enabled       bool             `json:"enabled"`
	Running       bool             `json:"running"`
	AffectedSites []string         `json:"affectedSites"`
	Addresses     []networkAddress `json:"addresses"`
	AccessURL     *string          `json:"accessUrl"`
	LocalCA       bool             `json:"localCa"`
}

func (c *Core) webNetworkMode(ctx context.Context, server string) (bool, error) {
	if server != "nginx" && server != "apache" && server != "caddy" {
		return false, invalid("请选择 Nginx、Apache 或 Caddy")
	}
	value, _, err := c.Store.setting(ctx, "webLan."+server)
	if err != nil {
		return false, err
	}
	if value != "" && value != "default" && value != "true" && value != "false" {
		return false, invalid("局域网访问设置无效")
	}
	return value == "true", nil
}
func (c *Core) registerSiteNetwork() {
	c.handle("site_network_info", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[serviceRequest](data)
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
		enabled, err := c.webNetworkMode(ctx, site.Runtime.WebServer)
		if err != nil {
			return nil, err
		}
		out := siteNetworkInfo{Server: site.Runtime.WebServer, Enabled: enabled, AffectedSites: []string{}, Addresses: []networkAddress{}, LocalCA: site.HTTPS && site.Runtime.ImportedCertID == nil && site.Runtime.ACMECertID == nil}
		sites, err := c.sites(ctx)
		if err != nil {
			return nil, err
		}
		for _, other := range sites {
			if other.Runtime.WebServer == out.Server {
				out.AffectedSites = append(out.AffectedSites, other.Name)
			}
		}
		sort.Strings(out.AffectedSites)
		c.services.mu.RLock()
		entry := c.services.entries[out.Server]
		var child *process.Child
		if entry != nil && entry.status.State == "running" {
			child = entry.child
		}
		c.services.mu.RUnlock()
		running := child != nil
		out.Running = running
		port := 0
		if running {
			if address, err := c.siteAccessURL(ctx, site); err == nil {
				u, e := url.Parse(address)
				if e == nil {
					port, _ = strconv.Atoi(u.Port())
					if port == 0 {
						port = 80
						if u.Scheme == "https" {
							port = 443
						}
					}
					for _, domain := range site.Domains {
						host := strings.Replace(domain, "*.", "www.", 1)
						if net.ParseIP(host) != nil || host == "localhost" || strings.HasSuffix(host, ".localhost") {
							continue
						}
						u.Host = net.JoinHostPort(host, strconv.Itoa(port))
						if port == 80 && u.Scheme == "http" || port == 443 && u.Scheme == "https" {
							u.Host = host
						}
						value := strings.TrimSuffix(u.String(), "/")
						out.AccessURL = &value
						break
					}
				}
			}
		}
		var listeners []gnet.ConnectionStat
		if port > 0 {
			listeners, err = gnet.ConnectionsWithContext(ctx, "tcp")
			if err != nil {
				return nil, err
			}
		}
		interfaces, err := net.Interfaces()
		if err != nil {
			return nil, err
		}
		for _, iface := range interfaces {
			if iface.Flags&net.FlagUp == 0 {
				continue
			}
			addresses, e := iface.Addrs()
			if e != nil {
				continue
			}
			for _, address := range addresses {
				ip, _, e := net.ParseCIDR(address.String())
				if e != nil || ip.To4() == nil || ip.IsLoopback() || ip.IsUnspecified() || ip.IsMulticast() || ip.Equal(net.IPv4bcast) {
					continue
				}
				item := networkAddress{Interface: iface.Name, Address: ip.String()}
				for _, listener := range listeners {
					if listener.Status != "LISTEN" || int(listener.Laddr.Port) != port {
						continue
					}
					bound := net.ParseIP(listener.Laddr.IP)
					if bound == nil || !bound.IsUnspecified() && !bound.Equal(ip) {
						continue
					}
					if owned, e := child.OwnsPID(int(listener.Pid)); e == nil && owned {
						item.Listening = true
					}
				}
				out.Addresses = append(out.Addresses, item)
			}
		}
		sort.Slice(out.Addresses, func(i, j int) bool {
			a, b := out.Addresses[i], out.Addresses[j]
			ap, bp := net.ParseIP(a.Address).IsPrivate(), net.ParseIP(b.Address).IsPrivate()
			if ap != bp {
				return ap
			}
			if a.Interface != b.Interface {
				return a.Interface < b.Interface
			}
			return a.Address < b.Address
		})
		return out, nil
	})
	c.handle("site_network_apply", c.applySiteNetwork)
}

type networkEdit struct {
	start, end int
	text       string
}

func networkNginxFallback(text string, enabled bool) string {
	// A loopback fallback takes precedence over wildcard virtual hosts for
	// local requests. Change only the exact generated fallback block.
	pattern := regexp.MustCompile(`(server \{ listen )(127\.0\.0\.1|0\.0\.0\.0)(:[0-9]+; server_name localhost; location / \{ return 200 'NiceEnv'; \} \})`)
	address := "127.0.0.1"
	if enabled {
		address = "0.0.0.0"
	}
	return pattern.ReplaceAllString(text, "${1}"+address+"${3}")
}

func applyNetworkEdits(text string, edits []networkEdit) string {
	sort.Slice(edits, func(i, j int) bool { return edits[i].start > edits[j].start })
	for _, edit := range edits {
		text = text[:edit.start] + edit.text + text[edit.end:]
	}
	return text
}
func networkEndpoint(value string, enabled bool) (string, int, error) {
	host, port, err := net.SplitHostPort(value)
	if err != nil || host != "127.0.0.1" && host != "0.0.0.0" {
		return "", 0, &AppError{Code: "WEB_NETWORK_CUSTOM", Message: "配置包含自定义监听地址，未覆盖；请先调整为受管的本机或全部 IPv4 监听"}
	}
	n, err := strconv.Atoi(port)
	if err != nil || n < 1 || n > 65535 {
		return "", 0, invalid("Web 监听端口无效")
	}
	host = "127.0.0.1"
	if enabled {
		host = "0.0.0.0"
	}
	return net.JoinHostPort(host, port), n, nil
}

// Change only listener operands inside managed site blocks. Configuration body,
// upstream URLs, custom rewrites and credentials remain byte-for-byte intact.
func networkSiteText(server, text string, enabled bool) (string, []int, error) {
	edits := []networkEdit{}
	ports := map[int]bool{}
	switch server {
	case "nginx":
		if !strings.HasPrefix(text, "# NiceEnv site:") && !strings.HasPrefix(text, "# site:") {
			return "", nil, invalid("Nginx 站点不是受管配置，请重新保存站点后再切换")
		}
		nodes, err := parseNginx(text)
		if err != nil {
			return "", nil, err
		}
		for _, node := range nodes {
			if len(node.words) != 1 || node.words[0] != "server" {
				continue
			}
			for _, child := range node.children {
				if len(child.words) < 2 || child.words[0] != "listen" {
					continue
				}
				address, port, err := networkEndpoint(child.words[1], enabled)
				if err != nil {
					return "", nil, err
				}
				ports[port] = true
				words := append([]string{}, child.words...)
				words[1] = address
				edits = append(edits, networkEdit{child.start, child.end, strings.Join(words, " ") + ";"})
			}
		}
	case "apache":
		if !regexp.MustCompile(`(?m)^# NiceEnv site:`).MatchString(text) {
			return "", nil, invalid("Apache 站点不是受管配置，请重新保存站点后再切换")
		}
		offset := 0
		for _, line := range strings.SplitAfter(text, "\n") {
			trim := strings.TrimSpace(line)
			fields := strings.Fields(trim)
			if len(fields) > 0 && strings.EqualFold(fields[0], "<VirtualHost") {
				if len(fields) != 2 || !strings.HasSuffix(fields[1], ">") {
					return "", nil, invalid("自定义 Apache 虚拟主机监听未覆盖")
				}
				endpoint := strings.TrimSuffix(fields[1], ">")
				// Apache's Listen directive owns socket binding. Wildcard virtual
				// hosts select names on that socket and must remain wildcard.
				wildcard := strings.HasPrefix(endpoint, "*:")
				if wildcard {
					endpoint = "127.0.0.1:" + strings.TrimPrefix(endpoint, "*:")
				}
				address, port, err := networkEndpoint(endpoint, enabled)
				if err != nil {
					return "", nil, err
				}
				ports[port] = true
				if wildcard {
					address = "*:" + strconv.Itoa(port)
				}
				start := offset + strings.Index(line, fields[1])
				edits = append(edits, networkEdit{start, start + len(fields[1]), address + ">"})
			}
			offset += len(line)
		}
	case "caddy":
		if !strings.HasPrefix(text, "# NiceEnv site:") {
			return "", nil, invalid("Caddy 站点不是受管配置，请重新保存站点后再切换")
		}
		tokens, err := caddyTokens(text)
		if err != nil {
			return "", nil, err
		}
		for i, token := range tokens {
			if token.depth == 0 && !token.quoted {
				if u, e := url.Parse(strings.TrimSuffix(token.value, ",")); e == nil && (u.Scheme == "http" || u.Scheme == "https") {
					p, e := strconv.Atoi(u.Port())
					if e == nil && p > 0 && p <= 65535 {
						ports[p] = true
					}
				}
			}
			if token.depth != 1 || token.quoted || token.value != "bind" {
				continue
			}
			lineStart := strings.LastIndexByte(text[:token.start], '\n') + 1
			if strings.TrimSpace(text[lineStart:token.start]) != "" {
				continue
			}
			if i+1 >= len(tokens) {
				return "", nil, invalid("Caddy bind 配置缺少地址")
			}
			arg := tokens[i+1]
			if strings.Contains(text[token.end:arg.start], "\n") || arg.value != "127.0.0.1" && arg.value != "0.0.0.0" {
				return "", nil, invalid("Caddy 使用自定义 bind 地址，未覆盖")
			}
			if i+2 < len(tokens) && !strings.Contains(text[arg.end:tokens[i+2].start], "\n") {
				return "", nil, invalid("Caddy bind 包含多个地址，未覆盖")
			}
			address := "127.0.0.1"
			if enabled {
				address = "0.0.0.0"
			}
			edits = append(edits, networkEdit{arg.start, arg.end, address})
		}
	default:
		return "", nil, invalid("Web 服务器无效")
	}
	if len(edits) == 0 || len(ports) == 0 {
		return "", nil, invalid("受管站点没有可切换的监听指令")
	}
	list := []int{}
	for p := range ports {
		list = append(list, p)
	}
	sort.Ints(list)
	return applyNetworkEdits(text, edits), list, nil
}

func (c *Core) applySiteNetwork(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		ID      string `json:"id"`
		Server  string `json:"server"`
		Enabled *bool  `json:"enabled"`
	}](data)
	if err != nil {
		return nil, err
	}
	if in.Enabled == nil {
		return nil, invalid("请选择是否允许局域网访问")
	}
	unlock, err := c.siteLock()
	if err != nil {
		return nil, err
	}
	defer unlock()
	site, err := c.site(ctx, in.ID)
	if err != nil {
		return nil, err
	}
	if site.Runtime.WebServer != in.Server {
		return nil, &AppError{Code: "WEB_NETWORK_CHANGED", Message: "站点已切换 Web 服务器，请刷新后重试"}
	}
	if _, err = c.webNetworkMode(ctx, in.Server); err != nil {
		return nil, err
	}
	e, err := c.serviceEntry(in.Server, "")
	if err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	running := e.child != nil && e.status.State == "running"
	busy := e.child != nil && !running
	c.services.mu.RUnlock()
	if busy {
		return nil, serviceBusy()
	}
	key := "webLan." + in.Server
	old, exists, err := c.Store.setting(ctx, key)
	if err != nil {
		return nil, err
	}
	journal := c.newSiteJournal()
	ports := map[int]bool{}
	options, err := c.siteOptions(ctx, Site{Runtime: siteRuntime{WebServer: in.Server}}, "", false)
	if err != nil {
		return nil, err
	}
	ports[options.http] = true
	if in.Server == "apache" {
		ports[options.https] = true
	}
	sites, err := c.sites(ctx)
	if err != nil {
		return nil, err
	}
	for _, other := range sites {
		if other.Runtime.WebServer != in.Server {
			continue
		}
		if !siteIdentifier.MatchString(other.ID) {
			return nil, invalid("站点标识无效")
		}
		for _, suffix := range []string{".conf", ".conf.disabled"} {
			path := filepath.Join(c.Home, "etc", in.Server, "sites", other.ID+suffix)
			text, err := readConfigText(path)
			if os.IsNotExist(err) {
				continue
			}
			if err != nil {
				return nil, err
			}
			next, list, err := networkSiteText(in.Server, text, *in.Enabled)
			if err != nil {
				return nil, err
			}
			if suffix == ".conf" {
				for _, p := range list {
					ports[p] = true
				}
			}
			if err = journal.add(path, []byte(next), true); err != nil {
				return nil, err
			}
		}
	}
	if *in.Enabled {
		// Windows can admit overlapping specific/wildcard binds. A successful
		// start alone does not prove that LAN requests reach this service.
		listeners, err := gnet.ConnectionsWithContext(ctx, "tcp")
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		child := e.child
		c.services.mu.RUnlock()
		for _, listener := range listeners {
			if listener.Status != "LISTEN" || !ports[int(listener.Laddr.Port)] {
				continue
			}
			owned := false
			if child != nil {
				owned, err = child.OwnsPID(int(listener.Pid))
				if err != nil {
					return nil, err
				}
			}
			if !owned {
				return nil, &AppError{Code: "WEB_NETWORK_PORT_BUSY", Message: fmt.Sprintf("端口 %d 已被其它进程监听，未修改局域网设置", listener.Laddr.Port)}
			}
		}
	}
	mainName := map[string]string{"nginx": "nginx.conf", "apache": "httpd.conf", "caddy": "Caddyfile"}[in.Server]
	main := filepath.Join(c.Home, "etc", in.Server, mainName)
	if in.Server == "caddy" {
		main = filepath.Join(c.Home, "etc", in.Server, e.pkg.Version, mainName)
	}
	mainText, err := readConfigText(main)
	if err != nil && !os.IsNotExist(err) {
		return nil, err
	}
	mainExists := err == nil
	if err == nil {
		next := mainText
		if in.Server == "nginx" {
			next = networkNginxFallback(mainText, *in.Enabled)
		}
		if in.Server == "apache" {
			pattern := regexp.MustCompile(`(?m)^(Listen[ \t]+)(127\.0\.0\.1|0\.0\.0\.0)(:[0-9]+(?:[ \t]+https)?[ \t]*\r?)$`)
			address := "127.0.0.1"
			if *in.Enabled {
				address = "0.0.0.0"
			}
			next = pattern.ReplaceAllString(mainText, "${1}"+address+"${3}")
		}
		if in.Server == "caddy" {
			pattern := regexp.MustCompile(`(?m)^(http://127\.0\.0\.1:[0-9]+ \{\r?\n    bind )(127\.0\.0\.1|0\.0\.0\.0)(\r?\n    respond NiceEnv\r?\n\})`)
			address := "127.0.0.1"
			if *in.Enabled {
				address = "0.0.0.0"
			}
			next = pattern.ReplaceAllString(mainText, "${1}"+address+"${3}")
		}
		if err = journal.add(main, []byte(next), true); err != nil {
			return nil, err
		}
	}
	stopped := false
	err = func() error {
		if err := journal.apply(ctx); err != nil {
			return err
		}
		if err := c.Store.saveSetting(ctx, key, *in.Enabled); err != nil {
			return err
		}
		if mainExists {
			// Validate even when the service is stopped. For a running server,
			// reject invalid changes before interrupting the loaded configuration.
			text, err := readConfigText(main)
			if err != nil {
				return err
			}
			resolved, err := c.resolveService(ctx, e)
			if err != nil {
				return err
			}
			index := map[string]int{"nginx": 0, "apache": 1, "caddy": 10}[in.Server]
			target := configTarget{spec: configSpecs[index], key: configSpecs[index].kind, path: main, program: resolved.program, version: e.pkg.Version}
			validation, err := c.validateConfig(ctx, target, text)
			if err != nil {
				return err
			}
			if !validation.OK {
				return &AppError{Code: "WEB_NETWORK_CONFIG_INVALID", Message: "网络切换后的配置未通过服务原生校验", Detail: strings.Join(validation.Messages, "\n")}
			}
		}
		if !running {
			return nil
		}
		if err := c.stopOwned(ctx, e, false); err != nil {
			return err
		}
		stopped = true
		if err := c.startService(ctx, e); err != nil {
			return err
		}
		return c.verifyNetworkListeners(ctx, e, ports, *in.Enabled)
	}()
	if err == nil {
		return true, nil
	}
	restore, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()
	failures := []error{}
	if stopped {
		if stopErr := c.stopOwned(restore, e, false); stopErr != nil {
			failures = append(failures, stopErr)
		}
	}
	if exists {
		failures = append(failures, c.Store.saveSetting(restore, key, old))
	} else {
		_, e := c.Store.db.ExecContext(restore, "DELETE FROM settings WHERE key=?", key)
		failures = append(failures, e)
	}
	failures = append(failures, journal.restore())
	if stopped && errors.Join(failures...) == nil {
		failures = append(failures, c.startService(restore, e))
	}
	if recovery := errors.Join(failures...); recovery != nil {
		return nil, &AppError{Code: "WEB_NETWORK_ROLLBACK", Message: "局域网设置失败，部分配置或服务未恢复，请检查日志和配置备份", Detail: errors.Join(err, recovery).Error()}
	}
	return nil, &AppError{Code: "WEB_NETWORK_FAILED", Message: "局域网设置未生效，已恢复原配置和服务", Detail: publicError(err).Message}
}

func (c *Core) verifyNetworkListeners(ctx context.Context, e *serviceEntry, ports map[int]bool, enabled bool) error {
	listeners, err := gnet.ConnectionsWithContext(ctx, "tcp")
	if err != nil {
		return err
	}
	c.services.mu.RLock()
	child := e.child
	c.services.mu.RUnlock()
	if child == nil {
		return invalid("Web 服务已退出")
	}
	for port := range ports {
		found := false
		for _, listener := range listeners {
			if listener.Status != "LISTEN" || int(listener.Laddr.Port) != port {
				continue
			}
			owned, err := child.OwnsPID(int(listener.Pid))
			if err != nil {
				return err
			}
			if !owned {
				continue
			}
			ip := net.ParseIP(listener.Laddr.IP)
			if ip == nil {
				continue
			}
			if !enabled && !ip.IsLoopback() {
				return fmt.Errorf("端口 %d 仍允许非本机访问", port)
			}
			found = found || enabled && ip.IsUnspecified() || !enabled && ip.IsLoopback()
		}
		if !found {
			return fmt.Errorf("端口 %d 未按所选网络范围监听", port)
		}
	}
	return nil
}
