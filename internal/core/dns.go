package core

import (
	"context"
	"encoding/json"
	"slices"
	"strings"
	"unicode"
)

type dnsConfiguration struct {
	InterfaceID string   `json:"interfaceId"`
	Automatic   bool     `json:"automatic"`
	Servers     []string `json:"servers"`
}

func (d dnsConfiguration) local() bool {
	return !d.Automatic && slices.Equal(d.Servers, []string{"127.0.0.1"})
}
func (d dnsConfiguration) matches(other dnsConfiguration) bool {
	return d.InterfaceID == other.InterfaceID && d.Automatic == other.Automatic && (d.Automatic || slices.Equal(d.Servers, other.Servers))
}
func dnsName(name string) error {
	if strings.TrimSpace(name) == "" || len(name) > 512 || strings.IndexFunc(name, unicode.IsControl) >= 0 {
		return invalid("网络接口名称无效")
	}
	return nil
}
func (c *Core) dnsBackup(ctx context.Context, name string) (*dnsConfiguration, error) {
	raw, _, err := c.Store.setting(ctx, "dnsBackup."+name)
	if err != nil {
		return nil, err
	}
	if raw == "" {
		return nil, nil
	}
	var saved *dnsConfiguration
	err = json.Unmarshal([]byte(raw), &saved)
	return saved, err
}
func (c *Core) registerDNS() {
	c.handle("dns_interfaces", func(ctx context.Context, _ json.RawMessage) (any, error) { return dnsInterfaces(ctx) })
	c.handle("dns_status_of", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Interface string `json:"name"`
		}](data)
		if err != nil {
			return nil, err
		}
		if err = dnsName(in.Interface); err != nil {
			return nil, err
		}
		c.dnsChanges.Lock()
		defer c.dnsChanges.Unlock()
		current, err := dnsRead(ctx, in.Interface)
		if err != nil {
			return nil, err
		}
		saved, err := c.dnsBackup(ctx, in.Interface)
		return map[string]any{"current": current, "backup": saved, "local": current.local()}, err
	})
	c.handle("dns_takeover", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Interface string `json:"name"`
		}](data)
		if err != nil {
			return nil, err
		}
		if err = dnsName(in.Interface); err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		c.dnsChanges.Lock()
		defer c.dnsChanges.Unlock()
		e, err := c.serviceEntry("coredns", "")
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		running := e.status.State == "running"
		r, child := e.resolved, e.child
		c.services.mu.RUnlock()
		if !running || r == nil || child == nil || r.port != 53 {
			return nil, &AppError{Code: "DNS_NOT_READY", Message: "接管前请先让 CoreDNS 在 53 端口正常运行"}
		}
		ready, err := coreDNSReady(ctx, r, child)
		if err != nil {
			return nil, err
		}
		if !ready {
			return nil, &AppError{Code: "DNS_NOT_READY", Message: "本地 DNS 未返回预期的域名解析结果"}
		}
		current, err := dnsRead(ctx, in.Interface)
		if err != nil {
			return nil, err
		}
		saved, err := c.dnsBackup(ctx, in.Interface)
		if err != nil {
			return nil, err
		}
		if saved != nil {
			if saved.InterfaceID != current.InterfaceID {
				return nil, &AppError{Code: "DNS_INTERFACE_CHANGED", Message: "接口已变化，请先核对旧 DNS 恢复记录"}
			}
			if current.local() {
				return true, nil
			}
			return nil, &AppError{Code: "DNS_BACKUP_EXISTS", Message: "已有 DNS 恢复记录，请先恢复后再接管"}
		}
		if current.local() {
			return nil, &AppError{Code: "DNS_NO_ORIGINAL", Message: "当前 DNS 已指向本机，但没有原配置记录", Hint: "请先恢复自动获取或在系统中配置 DNS，再接管"}
		}
		if err = c.Store.saveSetting(ctx, "dnsBackup."+in.Interface, current); err != nil {
			return nil, err
		}
		if err = dnsApply(ctx, in.Interface, dnsConfiguration{InterfaceID: current.InterfaceID, Servers: []string{"127.0.0.1"}}); err != nil {
			return nil, &AppError{Code: "DNS_TAKEOVER_FAILED", Message: "DNS 接管未完成，原配置已保存", Detail: err.Error(), Hint: "请刷新核对当前状态，必要时恢复原配置"}
		}
		return true, nil
	})
	c.handle("dns_restore", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Interface string `json:"name"`
			Automatic bool   `json:"automatic"`
		}](data)
		if err != nil {
			return nil, err
		}
		if err = dnsName(in.Interface); err != nil {
			return nil, err
		}
		c.dnsChanges.Lock()
		defer c.dnsChanges.Unlock()
		return true, c.restoreDNS(ctx, in.Interface, in.Automatic, true)
	})
}
func (c *Core) restoreDNS(ctx context.Context, name string, automatic, explicit bool) error {
	current, err := dnsRead(ctx, name)
	if err != nil {
		return err
	}
	saved, err := c.dnsBackup(ctx, name)
	if err != nil {
		return err
	}
	if saved == nil {
		if !automatic {
			return &AppError{Code: "DNS_NO_BACKUP", Message: "没有接管前的 DNS 配置记录"}
		}
		saved = &dnsConfiguration{InterfaceID: current.InterfaceID, Automatic: true, Servers: []string{}}
	}
	if saved.InterfaceID != current.InterfaceID {
		return &AppError{Code: "DNS_INTERFACE_CHANGED", Message: "网络接口已变化，不能写入旧接口配置"}
	}
	if !current.matches(*saved) {
		if !current.local() && !explicit {
			return &AppError{Code: "DNS_CHANGED", Message: "DNS 已被其它程序修改，原配置备份已保留", Hint: "请核对当前和原 DNS 地址后手动恢复"}
		}
		if err = dnsApply(ctx, name, *saved); err != nil {
			return err
		}
	}
	return c.Store.saveSetting(ctx, "dnsBackup."+name, nil)
}
func (c *Core) restoreDNSBeforeStop(ctx context.Context) error {
	c.dnsChanges.Lock()
	defer c.dnsChanges.Unlock()
	rows, err := c.Store.db.QueryContext(ctx, "SELECT key,value FROM settings WHERE key LIKE 'dnsBackup.%'")
	if err != nil {
		return err
	}
	names := []string{}
	for rows.Next() {
		var key, raw string
		if err = rows.Scan(&key, &raw); err != nil {
			break
		}
		var saved *dnsConfiguration
		if err = json.Unmarshal([]byte(raw), &saved); err != nil {
			break
		}
		if saved != nil {
			names = append(names, strings.TrimPrefix(key, "dnsBackup."))
		}
	}
	err2 := rows.Err()
	rows.Close()
	if err != nil {
		return err
	}
	if err2 != nil {
		return err2
	}
	for _, name := range names {
		if err = c.restoreDNS(ctx, name, false, false); err != nil {
			return err
		}
	}
	return nil
}
