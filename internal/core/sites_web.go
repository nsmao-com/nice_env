package core

import (
	"context"
	"net"
	"net/url"
	"strconv"
	"strings"
)

func (c *Core) reloadWebIfRunning(ctx context.Context, id string) error {
	c.services.mu.RLock()
	e := c.services.entries[id]
	running := e != nil && e.child != nil && e.status.State == "running"
	c.services.mu.RUnlock()
	if !running {
		return nil
	}
	switch id {
	case "nginx":
		return c.reloadNginxPools(ctx)
	case "apache":
		return c.reloadApache(ctx)
	case "caddy":
		return c.reloadCaddy(ctx)
	}
	return invalid("未知 Web 服务")
}
func otherWebEndpoint(server, domain, content string) (int, bool, error) {
	port := 0
	secure := false
	choose := func(candidate int, tls bool) {
		if candidate > 0 && candidate <= 65535 && (port == 0 || tls && !secure) {
			port = candidate
			secure = tls
		}
	}
	if server == "caddy" {
		tokens, err := caddyTokens(content)
		if err != nil {
			return 0, false, err
		}
		for _, token := range tokens {
			if token.depth != 0 || token.quoted {
				continue
			}
			u, err := url.Parse(strings.TrimSuffix(token.value, ","))
			if err != nil || u.Scheme != "http" && u.Scheme != "https" || !domainMatches(domain, u.Hostname()) {
				continue
			}
			p, err := strconv.Atoi(u.Port())
			if err != nil {
				continue
			}
			choose(p, u.Scheme == "https")
		}
	} else if server == "apache" {
		active, matched, tls := false, false, false
		candidate := 0
		for _, line := range strings.Split(content, "\n") {
			fields := strings.Fields(strings.TrimSpace(line))
			if len(fields) == 0 {
				continue
			}
			if strings.EqualFold(fields[0], "<VirtualHost") && len(fields) > 1 {
				active = true
				matched = false
				tls = false
				_, p, err := net.SplitHostPort(strings.TrimSuffix(fields[1], ">"))
				candidate = 0
				if err == nil {
					candidate, _ = strconv.Atoi(p)
				}
				continue
			}
			if strings.EqualFold(fields[0], "</VirtualHost>") {
				if active && matched {
					choose(candidate, tls)
				}
				active = false
				continue
			}
			if !active {
				continue
			}
			if strings.EqualFold(fields[0], "SSLEngine") && len(fields) > 1 {
				tls = strings.EqualFold(fields[1], "on")
			}
			if strings.EqualFold(fields[0], "ServerName") || strings.EqualFold(fields[0], "ServerAlias") {
				for _, name := range fields[1:] {
					matched = matched || domainMatches(domain, name)
				}
			}
		}
	} else {
		return 0, false, invalid("未知 Web 服务")
	}
	return port, secure, nil
}
