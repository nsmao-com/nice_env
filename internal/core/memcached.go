package core

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"net"
	"strconv"
	"strings"
	"time"
)

type memcachedSettings struct {
	MemoryMB       int `json:"memoryMb"`
	MaxConnections int `json:"maxConnections"`
	Threads        int `json:"threads"`
}

func serviceArg(args []string, flag, value string) []string {
	out := []string{}
	for i := 0; i < len(args); i++ {
		if args[i] == flag {
			i++
			continue
		}
		if strings.HasPrefix(args[i], flag) && len(args[i]) > len(flag) {
			continue
		}
		out = append(out, args[i])
	}
	return append(out, flag, value)
}
func (s memcachedSettings) validate() error {
	if s.MemoryMB < 64 || s.MemoryMB > 1048576 || s.MaxConnections < 16 || s.MaxConnections > 1000000 || s.Threads < 1 || s.Threads > 64 {
		return invalid("请检查内存、连接数和线程数设置")
	}
	return nil
}
func (c *Core) memcachedSettings(ctx context.Context, version string) (memcachedSettings, error) {
	result := memcachedSettings{512, 1024, 4}
	if !safePart(version) {
		return result, invalid("请选择 Memcached 版本")
	}
	var count int
	err := c.Store.db.QueryRowContext(ctx, "SELECT COUNT(*) FROM installed WHERE id='memcached' AND version=?", version).Scan(&count)
	if err != nil {
		return result, err
	}
	if count != 1 {
		return result, invalid("所选版本未安装")
	}
	text, found, err := c.Store.setting(ctx, "memcached-settings@"+version)
	if err != nil {
		return result, err
	}
	if found {
		if err = json.Unmarshal([]byte(text), &result); err != nil {
			return result, invalid("Memcached 设置记录损坏")
		}
	}
	return result, result.validate()
}
func (c *Core) registerMemcached() {
	c.handle("memcached_settings", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.memcachedCommand(ctx, data, "settings")
	})
	c.handle("memcached_settings_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.memcachedCommand(ctx, data, "save")
	})
	c.handle("memcached_stats", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.memcachedCommand(ctx, data, "stats")
	})
	c.handle("memcached_flush", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.memcachedCommand(ctx, data, "flush")
	})
}
func (c *Core) memcachedCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[struct {
		Version  string            `json:"version"`
		Revision string            `json:"revision"`
		Settings memcachedSettings `json:"settings"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	if action == "settings" || action == "save" {
		settings, err := c.memcachedSettings(ctx, in.Version)
		if err != nil {
			return nil, err
		}
		revision := siteRevision(c.Home, in.Version, settings)
		if action == "save" {
			if in.Revision == "" || revision != in.Revision {
				return nil, invalid("Memcached 设置已变化，请重新读取")
			}
			if err = in.Settings.validate(); err != nil {
				return nil, err
			}
			if err = c.Store.saveSetting(ctx, "memcached-settings@"+in.Version, in.Settings); err != nil {
				return nil, err
			}
			settings = in.Settings
			revision = siteRevision(c.Home, in.Version, settings)
		}
		return map[string]any{"version": in.Version, "revision": revision, "settings": settings, "restartRequired": true}, nil
	}
	e, err := c.serviceEntry("memcached", in.Version)
	if err != nil {
		return nil, err
	}
	if err = c.verifyDatabaseListener(ctx, e); err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	port, child := e.status.Port, e.child
	c.services.mu.RUnlock()
	conn, err := (&net.Dialer{Timeout: 3 * time.Second}).DialContext(ctx, "tcp4", net.JoinHostPort("127.0.0.1", strconv.Itoa(port)))
	if err != nil {
		return nil, err
	}
	defer conn.Close()
	if err = conn.SetDeadline(time.Now().Add(5 * time.Second)); err != nil {
		return nil, err
	}
	reader := bufio.NewReaderSize(conn, 8192)
	if action == "flush" {
		if _, err = io.WriteString(conn, "flush_all\r\n"); err != nil {
			return nil, err
		}
		reply, err := reader.ReadString('\n')
		if err != nil {
			return nil, err
		}
		if reply != "OK\r\n" {
			return nil, invalid("Memcached 未确认清空缓存")
		}
	}
	if _, err = io.WriteString(conn, "stats\r\n"); err != nil {
		return nil, err
	}
	stats := map[string]string{}
	for i := 0; ; i++ {
		if i >= 1000 {
			return nil, invalid("Memcached 响应过长")
		}
		line, err := reader.ReadSlice('\n')
		if err != nil {
			return nil, err
		}
		text := string(line)
		if text == "END\r\n" {
			break
		}
		parts := strings.Fields(text)
		if len(parts) != 3 || parts[0] != "STAT" {
			return nil, invalid("Memcached 统计格式无效")
		}
		stats[parts[1]] = parts[2]
	}
	pid, err := strconv.Atoi(stats["pid"])
	if err != nil {
		return nil, invalid("Memcached 未返回进程号")
	}
	owned, err := child.OwnsPID(pid)
	if err != nil {
		return nil, err
	}
	if !owned {
		return nil, invalid("Memcached 响应不属于受管实例")
	}
	result := map[string]any{"reachable": true, "port": port, "processId": pid, "version": stats["version"]}
	for field, key := range map[string]string{"uptimeSeconds": "uptime", "currentItems": "curr_items", "totalItems": "total_items", "bytes": "bytes", "limitMaxbytes": "limit_maxbytes", "currentConnections": "curr_connections", "totalConnections": "total_connections", "cmdGet": "cmd_get", "cmdSet": "cmd_set", "getHits": "get_hits", "getMisses": "get_misses", "evictions": "evictions"} {
		n, err := strconv.ParseUint(stats[key], 10, 64)
		if err != nil {
			return nil, invalid("Memcached 统计缺少字段：" + key)
		}
		result[field] = n
	}
	return result, nil
}
