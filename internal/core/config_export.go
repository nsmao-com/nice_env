package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"path/filepath"
	"strings"
	"time"
)

// Keep backup-v1's tuple arrays and raw automation configuration compatible
// with existing backups. Certificate key material and machine credentials are
// not part of this configuration file.
type configBundle struct {
	Format          string                    `json:"format"`
	AppVersion      string                    `json:"appVersion"`
	ExportedAt      int64                     `json:"exportedAt"`
	Settings        [][2]string               `json:"settings"`
	Packages        [][2]string               `json:"packages"`
	Sites           []Site                    `json:"sites"`
	ProxyProfiles   [][2]string               `json:"proxyProfiles"`
	Stacks          []Stack                   `json:"stacks"`
	CertAutomations []json.RawMessage         `json:"certAutomations"`
	CertMonitors    []json.RawMessage         `json:"certMonitors"`
	SiteAuth        map[string]siteAuthBackup `json:"siteAuth,omitempty"`
}

type siteAuthBackup struct {
	Bcrypt string `json:"bcrypt"`
	Nginx  string `json:"nginx,omitempty"`
}

func transferableSetting(key string) bool {
	// Public preferences are portable; operational state, runtime paths, database
	// credentials, DNS recovery data and backup schedules are machine-specific.
	if _, ok := publicDefaults[key]; ok {
		return true
	}
	switch key {
	case "watchdogEnabled", "watchdogMaxAttempts", "watchdogIntervalSec", "monitorNotifyKind", "monitorNotifyUrl":
		return true
	}
	return false
}

func (c *Core) registerConfigExport() {
	c.handle("import_config", c.importConfigFile)
	c.handle("import_config_text", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			JSON string `json:"json"`
		}](data)
		if err != nil {
			return nil, err
		}
		return c.importConfig(ctx, []byte(in.JSON))
	})
	c.handle("export_config", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Path string `json:"path"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !filepath.IsAbs(in.Path) || !strings.EqualFold(filepath.Ext(in.Path), ".json") {
			return nil, invalid("请选择 JSON 配置备份文件的绝对路径")
		}
		unlock, err := c.siteLock()
		if err != nil {
			return nil, err
		}
		defer unlock()
		bundle, err := c.exportConfigSnapshot(ctx)
		if err != nil {
			return nil, err
		}
		bundle.SiteAuth = map[string]siteAuthBackup{}
		for _, site := range bundle.Sites {
			if site.Runtime.BasicAuth == nil || !site.Runtime.BasicAuth.Enabled {
				continue
			}
			if !siteIdentifier.MatchString(site.ID) {
				return nil, invalid("备份站点标识无效，未读取认证文件")
			}
			bcryptHash, err := c.siteAuth(&site, nil)
			if err != nil {
				return nil, err
			}
			entry := siteAuthBackup{Bcrypt: bcryptHash}
			if site.Runtime.WebServer == "nginx" {
				entry.Nginx, err = c.nginxSiteAuth(site, nil)
				if err != nil {
					return nil, err
				}
			}
			bundle.SiteAuth[site.ID] = entry
		}
		encoded, err := json.MarshalIndent(bundle, "", "  ")
		if err != nil {
			return nil, err
		}
		if len(encoded) > maxTextFile {
			return nil, invalid("配置备份超过 8 MiB，请减少历史记录后重试")
		}
		if err = ctx.Err(); err != nil {
			return nil, err
		}
		if err = atomicWrite(in.Path, encoded); err != nil {
			return nil, err
		}
		return len(bundle.Settings) + len(bundle.Packages) + len(bundle.Sites) + len(bundle.ProxyProfiles) + len(bundle.Stacks) + len(bundle.CertAutomations) + len(bundle.CertMonitors), nil
	})
}

func (c *Core) exportConfigSnapshot(ctx context.Context) (configBundle, error) {
	b := configBundle{Format: "niceservbay/backup-v1", AppVersion: c.Version, ExportedAt: time.Now().UnixMilli(), Settings: [][2]string{}, Packages: [][2]string{}, Sites: []Site{}, ProxyProfiles: [][2]string{}, Stacks: []Stack{}, CertAutomations: []json.RawMessage{}, CertMonitors: []json.RawMessage{}}
	tx, err := c.Store.db.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return b, err
	}
	defer tx.Rollback()
	// Every table is read from the same SQLite snapshot. Never call Store methods
	// here: its single connection is owned by this transaction until commit.
	read := func(query string, scan func(*sql.Rows) error) error {
		rows, err := tx.QueryContext(ctx, query)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			if err = scan(rows); err != nil {
				return err
			}
		}
		return rows.Err()
	}
	for _, item := range []struct {
		query    string
		values   *[][2]string
		settings bool
	}{
		{"SELECT key,value FROM settings ORDER BY key", &b.Settings, true},
		{"SELECT id,version FROM installed ORDER BY id,version", &b.Packages, false},
		{"SELECT name,url FROM proxy_profiles ORDER BY id", &b.ProxyProfiles, false},
	} {
		if err = read(item.query, func(rows *sql.Rows) error {
			var value [2]string
			if err := rows.Scan(&value[0], &value[1]); err != nil {
				return err
			}
			if !item.settings || transferableSetting(value[0]) {
				*item.values = append(*item.values, value)
			}
			return nil
		}); err != nil {
			return b, err
		}
	}
	if err = read(siteSelect+" ORDER BY id", func(rows *sql.Rows) error {
		site, err := scanSite(rows)
		if err != nil {
			return err
		}
		site.Status = "stopped" // backups carry configuration, never a live process claim
		b.Sites = append(b.Sites, site)
		return nil
	}); err != nil {
		return b, err
	}
	if err = read(stackSelect+" WHERE builtin=0 ORDER BY id", func(rows *sql.Rows) error {
		stack, err := scanStack(rows)
		if err != nil {
			return err
		}
		b.Stacks = append(b.Stacks, stack)
		return nil
	}); err != nil {
		return b, err
	}
	for _, item := range []struct {
		query  string
		values *[]json.RawMessage
	}{
		{"SELECT data FROM cert_automations ORDER BY id", &b.CertAutomations},
		{"SELECT data FROM cert_monitors ORDER BY id", &b.CertMonitors},
	} {
		if err = read(item.query, func(rows *sql.Rows) error {
			var raw string
			if err := rows.Scan(&raw); err != nil {
				return err
			}
			var object map[string]json.RawMessage
			if json.Unmarshal([]byte(raw), &object) != nil || object == nil {
				return &AppError{Code: "DATA_INVALID", Message: "配置记录损坏，未覆盖原备份文件"}
			}
			*item.values = append(*item.values, json.RawMessage(raw))
			return nil
		}); err != nil {
			return b, err
		}
	}
	return b, tx.Commit()
}
