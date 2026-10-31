package core

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/netip"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"niceenv/internal/hostfile"
	"niceenv/internal/process"
)

type hostsFile struct {
	Path    Path   `json:"path"`
	Content string `json:"content"`
}

func (c *Core) hostsPath() (string, bool, error) {
	// The explicit CLI fixture is constrained to the marked development home;
	// elevation always resolves the real system target inside the helper.
	if os.Getenv("NICEENV_WAILS_HOSTS_FIXTURE") == "1" {
		marker, err := os.ReadFile(filepath.Join(c.Home, ".niceenv-wails-development"))
		if err != nil || string(marker) != "NiceEnv Wails development data\n" {
			return "", false, invalid("hosts 隔离目录标记无效")
		}
		path := filepath.Join(c.Home, "etc", "hosts")
		if err = managedDirectory(c.Home, filepath.Dir(path)); err != nil {
			return "", false, err
		}
		info, err := os.Lstat(path)
		if err != nil {
			return "", false, err
		}
		if !info.Mode().IsRegular() || info.Mode()&(os.ModeSymlink|os.ModeIrregular) != 0 {
			return "", false, invalid("hosts 隔离文件必须为普通文件")
		}
		return path, false, nil
	}
	path, err := hostfile.SystemPath()
	return path, true, err
}

func hostError(err error) error {
	if err == nil {
		return nil
	}
	if errors.Is(err, hostfile.ErrChanged) {
		return &AppError{Code: "HOSTS_CHANGED", Message: err.Error(), Hint: "草稿未覆盖最新内容，请重新读取并核对。"}
	}
	return &AppError{Code: "HOSTS_FAILED", Message: "hosts 操作未完成", Detail: err.Error(), Hint: "请检查文件是否被其它程序占用，保存时确认系统管理员授权。"}
}

func (c *Core) readHostsFile() (hostsFile, error) {
	path, _, err := c.hostsPath()
	if err != nil {
		return hostsFile{}, err
	}
	content, err := hostfile.Read(path)
	return hostsFile{Path(path), content}, hostError(err)
}

func (c *Core) hostSiteEntries(ctx context.Context) ([]hostfile.Entry, error) {
	rows, err := c.Store.db.QueryContext(ctx, "SELECT domains FROM sites ORDER BY id")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []hostfile.Entry{}
	seen := map[string]bool{}
	for rows.Next() {
		var raw string
		var domains []string
		if err = rows.Scan(&raw); err != nil {
			return nil, err
		}
		if err = json.Unmarshal([]byte(raw), &domains); err != nil {
			return nil, invalid("站点域名记录无效，未修改 hosts")
		}
		for _, domain := range domains {
			if strings.HasPrefix(domain, "*.") || strings.EqualFold(domain, "localhost") {
				continue
			}
			if _, err := netip.ParseAddr(domain); err == nil {
				continue
			}
			e, err := hostfile.Normalize("127.0.0.1", domain)
			if err != nil {
				return nil, hostError(err)
			}
			if !seen[e.Domain] {
				out = append(out, e)
				seen[e.Domain] = true
			}
		}
	}
	return out, rows.Err()
}

func hostEntriesKey(entries []hostfile.Entry) string {
	keys := make([]string, 0, len(entries))
	for _, e := range entries {
		b, _ := json.Marshal(e)
		keys = append(keys, string(b))
	}
	sort.Strings(keys)
	return strings.Join(keys, "\n")
}

func (c *Core) changeHosts(ctx context.Context, data json.RawMessage, action string) (any, error) {
	return c.changeHostsWithSiteLock(ctx, data, action, false)
}

func (c *Core) changeHostsWithSiteLock(ctx context.Context, data json.RawMessage, action string, siteLocked bool) (any, error) {
	in, err := decode[struct {
		Entries         *[]hostfile.Entry `json:"entries"`
		ExpectedEntries *[]hostfile.Entry `json:"expectedEntries"`
		Content         *string           `json:"content"`
		ExpectedContent *string           `json:"expectedContent"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !siteLocked {
		if !c.siteChanges.TryLock() {
			return nil, serviceBusy()
		}
		defer c.siteChanges.Unlock()
	}
	if !c.hostChanges.TryLock() {
		return nil, serviceBusy()
	}
	defer c.hostChanges.Unlock()
	lockDir := filepath.Join(c.Home, "service-locks")
	if err = managedDirectory(c.Home, lockDir); err != nil {
		return nil, err
	}
	unlock, err := process.Lock(filepath.Join(lockDir, "hosts.lock"))
	if err != nil {
		return nil, hostError(err)
	}
	defer unlock()
	path, elevate, err := c.hostsPath()
	if err != nil {
		return nil, err
	}
	original, err := hostfile.Read(path)
	if err != nil {
		return nil, hostError(err)
	}
	current, err := hostfile.Parse(original, false)
	if err != nil {
		return nil, hostError(err)
	}
	if in.ExpectedEntries != nil && hostEntriesKey(*in.ExpectedEntries) != hostEntriesKey(current) {
		return nil, hostError(hostfile.ErrChanged)
	}
	sites, err := c.hostSiteEntries(ctx)
	if err != nil {
		return nil, err
	}
	siteNames := map[string]string{}
	for _, e := range sites {
		siteNames[e.Domain] = e.IP
	}
	previous, _, err := c.Store.setting(ctx, "extraHosts")
	if err != nil {
		return nil, err
	}
	extras := [][2]string{}
	if previous != "" {
		if err = json.Unmarshal([]byte(previous), &extras); err != nil {
			return nil, &AppError{Code: "HOSTS_SETTINGS_INVALID", Message: "已保存的 hosts 条目格式无效，未覆盖现有记录"}
		}
	}
	updating := action != "rebuild"
	var requested []hostfile.Entry
	if action == "save" {
		if in.Content == nil || in.ExpectedContent == nil {
			return nil, invalid("保存 hosts 需要文件原文和读取时快照")
		}
		if *in.ExpectedContent != original {
			return nil, hostError(hostfile.ErrChanged)
		}
		requested, err = hostfile.Parse(*in.Content, true)
		if err != nil {
			return nil, hostError(err)
		}
	} else if action == "apply" {
		if in.Entries == nil {
			return nil, invalid("缺少 hosts 条目")
		}
		requested = *in.Entries
	}
	if updating {
		extras = [][2]string{}
		for _, raw := range requested {
			if !raw.Managed {
				continue
			}
			e, err := hostfile.Normalize(raw.IP, raw.Domain)
			if err != nil {
				return nil, hostError(err)
			}
			if ip, ok := siteNames[e.Domain]; ok {
				if e.IP != ip {
					return nil, &AppError{Code: "HOSTS_SITE_MANAGED", Message: fmt.Sprintf("%s 由站点维护，请到站点设置修改", e.Domain)}
				}
				continue
			}
			extras = append(extras, [2]string{e.IP, e.Domain})
		}
	}
	if len(extras) > 10000 {
		return nil, invalid("hosts 条目数量超过限制")
	}
	merged := append([]hostfile.Entry{}, sites...)
	normalized := [][2]string{}
	seen := map[string]bool{}
	for _, raw := range extras {
		e, err := hostfile.Normalize(raw[0], raw[1])
		if err != nil {
			return nil, hostError(err)
		}
		key := e.IP + " " + e.Domain
		if seen[key] {
			continue
		}
		seen[key] = true
		normalized = append(normalized, [2]string{e.IP, e.Domain})
		if _, ok := siteNames[e.Domain]; !ok {
			merged = append(merged, e)
		}
	}
	content := hostfile.Merge(original, merged)
	if action == "save" {
		content = *in.Content
	}
	if err = ctx.Err(); err != nil {
		return nil, err
	}
	if err = hostfile.Save(path, original, content, elevate); err != nil {
		return nil, hostError(err)
	}
	if updating {
		if err = c.Store.saveSetting(ctx, "extraHosts", normalized); err != nil {
			rollback := hostfile.Save(path, content, original, elevate)
			return nil, &AppError{Code: "HOSTS_SETTINGS_FAILED", Message: "hosts 设置未能保存", Detail: fmt.Sprintf("保存记录：%v；恢复文件：%v", err, rollback)}
		}
	}
	if action == "save" {
		return hostsFile{Path(path), content}, nil
	}
	return true, nil
}

func (c *Core) registerHosts() {
	c.handle("read_hosts_file", func(context.Context, json.RawMessage) (any, error) { return c.readHostsFile() })
	c.handle("read_hosts", func(context.Context, json.RawMessage) (any, error) {
		f, err := c.readHostsFile()
		if err != nil {
			return nil, err
		}
		entries, err := hostfile.Parse(f.Content, false)
		return entries, hostError(err)
	})
	c.handle("save_hosts_file", func(ctx context.Context, data json.RawMessage) (any, error) { return c.changeHosts(ctx, data, "save") })
	c.handle("apply_hosts", func(ctx context.Context, data json.RawMessage) (any, error) { return c.changeHosts(ctx, data, "apply") })
	c.handle("rebuild_hosts", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.changeHosts(ctx, data, "rebuild")
	})
}
