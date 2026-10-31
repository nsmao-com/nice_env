package core

import (
	"context"
	"encoding/json"
	"fmt"
	"net/url"
	"regexp"
	"strings"
	"unicode/utf8"
)

// Only public preferences are exposed. Credentials and internal settings never
// flow through get_settings or the generic setter.
var publicDefaults = map[string]any{
	"language": "zh", "appearance": "light", "accentHue": float64(250), "accentHex": "",
	"uiFont": "plex", "uiScale": float64(1), "codeFont": "plex-mono", "codeFontSize": 11.5,
	"codeLineNumbers": true, "codeWrap": true, "codeTheme": "auto", "codeBg": "", "reduceMotion": false,
	"defaultTld": "test", "defaultWebServer": "nginx", "portProfile": "standard", "mirror": "official", "customMirror": "",
	"autostart": false, "minimizeToTray": true, "startStackOnLaunch": "", "autoClosePortOnStart": true,
	"manifestUrl": "", "checkUpdateOnLaunch": true, "autoDownloadUpdate": false,
	"favoriteSites": []any{}, "siteGroups": []any{}, "siteGroupAssignments": map[string]any{}, "rewriteTemplates": []any{},
	"logTailLines": float64(500), "logAutoRefresh": true, "confirmKill": true, "hideScrollbars": true, "onboardingDone": false,
}

func (c *Core) registerSettings() {
	c.handle("get_settings", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.settings(ctx) })
	c.handle("get_start_stack_on_launch", func(ctx context.Context, _ json.RawMessage) (any, error) {
		value, _, err := c.Store.setting(ctx, "startStackOnLaunch")
		return value, err
	})
	c.handle("set_setting", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			Key   string `json:"key"`
			Value any    `json:"value"`
		}](data)
		if err != nil {
			return nil, err
		}
		if err = c.validateSetting(args.Key, args.Value); err != nil {
			return nil, err
		}
		return true, c.Store.saveSetting(ctx, args.Key, args.Value)
	})
}

func (c *Core) settings(ctx context.Context) (map[string]any, error) {
	result := make(map[string]any, len(publicDefaults)+1)
	for key, fallback := range publicDefaults {
		value, found, err := c.Store.setting(ctx, key)
		if err != nil {
			return nil, err
		}
		result[key] = fallback
		if !found {
			continue
		}
		if _, ok := fallback.(string); ok {
			result[key] = value
			continue
		}
		var decoded any
		if json.Unmarshal([]byte(value), &decoded) == nil && fmt.Sprintf("%T", decoded) == fmt.Sprintf("%T", fallback) {
			result[key] = decoded
		}
	}
	rows, err := c.Store.tableRows(ctx, "SELECT key,value FROM settings WHERE key LIKE 'portOverride.%'", nil, nil)
	if err != nil {
		return nil, err
	}
	overrides := map[string]int{}
	for _, row := range rows {
		var port int
		if json.Unmarshal([]byte(row["value"].(string)), &port) == nil && port > 0 && port <= 65535 {
			overrides[strings.TrimPrefix(row["key"].(string), "portOverride.")] = port
		}
	}
	result["portOverrides"] = overrides
	return result, nil
}

func (c *Core) validateSetting(key string, value any) error {
	fallback, ok := publicDefaults[key]
	if !ok {
		return &AppError{Code: "SETTING_PROTECTED", Message: "请通过对应功能面板修改此设置"}
	}
	if fmt.Sprintf("%T", value) != fmt.Sprintf("%T", fallback) {
		return invalid("设置值类型无效")
	}
	switch key {
	case "autostart", "startStackOnLaunch", "rewriteTemplates", "portProfile", "defaultWebServer":
		return &AppError{Code: "COMMAND_NOT_PORTED", Message: "此设置的系统联动尚未迁移，未保存更改", Detail: key}
	case "language":
		if value != "zh" && value != "en" {
			return invalid("语言选项无效")
		}
	case "appearance":
		if value != "light" && value != "dark" && value != "system" {
			return invalid("外观选项无效")
		}
	case "accentHex", "codeBg":
		if value != "" && !regexp.MustCompile(`^#[0-9a-fA-F]{6}$`).MatchString(value.(string)) {
			return invalid("颜色格式无效")
		}
	case "accentHue":
		if n := value.(float64); n < 0 || n > 360 {
			return invalid("色相超出范围")
		}
	case "uiScale":
		if n := value.(float64); n < 0.5 || n > 2 {
			return invalid("界面缩放超出范围")
		}
	case "codeFontSize":
		if n := value.(float64); n < 8 || n > 32 {
			return invalid("编辑器字体大小超出范围")
		}
	case "logTailLines":
		if n := value.(float64); n < 1 || n > 10000 || n != float64(int(n)) {
			return invalid("日志行数无效")
		}
	case "manifestUrl", "customMirror":
		if value != "" {
			u, err := url.Parse(value.(string))
			if err != nil || u.Hostname() == "" || (u.Scheme != "https" && u.Scheme != "http") || u.User != nil {
				return invalid("请输入不含登录凭据的 HTTP(S) 地址")
			}
		}
	case "defaultTld":
		if !regexp.MustCompile(`^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)*$`).MatchString(value.(string)) {
			return invalid("本地域名后缀格式无效")
		}
	case "favoriteSites":
		items := value.([]any)
		if len(items) > 10000 {
			return invalid("收藏站点过多")
		}
		for _, v := range items {
			if id, ok := v.(string); !ok || strings.TrimSpace(id) == "" || len(id) > 160 {
				return invalid("收藏站点标识无效")
			}
		}
	case "siteGroups":
		groups := value.([]any)
		if len(groups) > 100 {
			return invalid("最多保存 100 个分组")
		}
		ids, names := map[string]bool{}, map[string]bool{}
		for _, v := range groups {
			group, ok := v.(map[string]any)
			if !ok {
				return invalid("分组格式无效")
			}
			id, _ := group["id"].(string)
			name, _ := group["name"].(string)
			id = strings.TrimSpace(id)
			name = strings.TrimSpace(name)
			if id == "" || len(id) > 80 || name == "" || utf8.RuneCountInString(name) > 80 || ids[id] || names[strings.ToLower(name)] {
				return invalid("分组名称或标识无效或重复")
			}
			ids[id] = true
			names[strings.ToLower(name)] = true
		}
	case "siteGroupAssignments":
		items := value.(map[string]any)
		if len(items) > 10000 {
			return invalid("站点分组归属过多")
		}
		for id, v := range items {
			group, ok := v.(string)
			if strings.TrimSpace(id) == "" || len(id) > 160 || !ok || strings.TrimSpace(group) == "" || len(group) > 80 {
				return invalid("站点分组归属无效")
			}
		}
	}
	if text, ok := value.(string); ok && (len(text) > 4096 || strings.ContainsRune(text, 0)) {
		return invalid("设置内容过长或包含无效字符")
	}
	return nil
}
