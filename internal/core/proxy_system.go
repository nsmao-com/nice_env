package core

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"reflect"
	"time"
)

type systemProxySnapshot struct {
	Enabled bool           `json:"enabled"`
	Server  string         `json:"server"`
	Service string         `json:"service,omitempty"`
	Values  map[string]any `json:"values"`
}
type proxySystemBackup struct {
	Original systemProxySnapshot `json:"original"`
	Applied  systemProxySnapshot `json:"applied"`
}

func (c *Core) setProxySystem(ctx context.Context, enabled bool) error {
	var saved *proxySystemBackup
	raw, _, err := c.Store.setting(ctx, "systemProxyBackup")
	if err != nil {
		return err
	}
	if raw != "" {
		if err = json.Unmarshal([]byte(raw), &saved); err != nil {
			return err
		}
	}
	if !enabled && saved == nil {
		return nil
	}
	current, err := readSystemProxy(ctx)
	if err != nil {
		return err
	}
	if enabled {
		if _, err = c.proxyAPI(ctx, http.MethodGet, "/version", nil); err != nil {
			return err
		}
		if saved != nil {
			if reflect.DeepEqual(current, saved.Applied) {
				return nil
			}
			return &AppError{Code: "SYSTEM_PROXY_CHANGED", Message: "已有代理恢复记录，请先恢复并核对后再开启"}
		}
		desired := managedSystemProxy(current, fmt.Sprintf("127.0.0.1:%d", proxyPort))
		saved = &proxySystemBackup{Original: current, Applied: desired}
		if err = c.Store.saveSetting(ctx, "systemProxyBackup", saved); err != nil {
			return err
		}
		if err = writeSystemProxy(ctx, desired); err != nil {
			restoreCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
			defer cancel()
			restore := writeSystemProxy(restoreCtx, current)
			if restore == nil {
				restore = c.Store.saveSetting(restoreCtx, "systemProxyBackup", nil)
			}
			return &AppError{Code: "SYSTEM_PROXY_APPLY", Message: "系统代理未能生效，已尝试恢复原配置，请刷新检查", Detail: errors.Join(err, restore).Error()}
		}
		return nil
	}
	if saved == nil {
		return nil
	}
	if reflect.DeepEqual(current, saved.Original) {
		return c.Store.saveSetting(ctx, "systemProxyBackup", nil)
	}
	if !reflect.DeepEqual(current, saved.Applied) {
		return &AppError{Code: "SYSTEM_PROXY_CHANGED", Message: "系统代理已被其它程序修改，原配置备份已保留", Hint: "请先核对系统代理设置，避免覆盖其它程序的新配置"}
	}
	if err = writeSystemProxy(ctx, saved.Original); err != nil {
		return err
	}
	return c.Store.saveSetting(ctx, "systemProxyBackup", nil)
}
