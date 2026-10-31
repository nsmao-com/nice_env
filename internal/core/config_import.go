package core

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"math"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	"golang.org/x/crypto/bcrypt"
)

type configImportReport struct {
	Sites           int      `json:"sites"`
	SkippedSites    int      `json:"skippedSites"`
	Settings        int      `json:"settings"`
	ProxyProfiles   int      `json:"proxyProfiles"`
	Stacks          int      `json:"stacks"`
	CertAutomations int      `json:"certAutomations"`
	CertMonitors    int      `json:"certMonitors"`
	MissingPackages []string `json:"missingPackages"`
	Warnings        []string `json:"warnings"`
}

func (c *Core) importConfigFile(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Path string `json:"path"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !filepath.IsAbs(in.Path) {
		return nil, invalid("请选择备份文件的绝对路径")
	}
	f, err := os.Open(in.Path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Size() > maxTextFile {
		return nil, invalid("请选择不超过 8 MiB 的配置备份")
	}
	raw, err := io.ReadAll(io.LimitReader(f, maxTextFile+1))
	if err != nil {
		return nil, err
	}
	return c.importConfig(ctx, raw)
}

func (c *Core) importConfig(ctx context.Context, raw []byte) (result any, resultErr error) {
	if len(raw) > maxTextFile || !utf8.Valid(raw) {
		return nil, invalid("备份必须为不超过 8 MiB 的 UTF-8 JSON")
	}
	var b configBundle
	if json.Unmarshal(raw, &b) != nil || b.Format != "niceservbay/backup-v1" {
		return nil, &AppError{Code: "BAD_BACKUP", Message: "不是有效的 NiceEnv 配置备份"}
	}
	if len(b.Sites) > 2000 || len(b.Settings) > 2000 || len(b.Packages) > 5000 || len(b.ProxyProfiles) > 1000 || len(b.Stacks) > 1000 || len(b.CertAutomations) > 1000 || len(b.CertMonitors) > 2000 {
		return nil, invalid("备份条目数量超过限制")
	}
	if !c.certAuto.mu.TryLock() {
		return nil, certAutoBusy()
	}
	defer c.certAuto.mu.Unlock()
	unlock, err := c.siteLock()
	if err != nil {
		return nil, err
	}
	defer unlock()
	r := configImportReport{MissingPackages: []string{}, Warnings: []string{}}
	journal := c.newSiteJournal()
	committed := false
	defer func() {
		if !committed {
			resultErr = errors.Join(resultErr, journal.restore())
		}
	}()
	authFiles := map[string][]byte{}
	// Prepare without writes. In particular, importing a site never invokes the
	// normal creation path that starts services and updates the system hosts file.
	settings := map[string]string{}
	for _, pair := range b.Settings {
		if _, ok := settings[pair[0]]; ok {
			return nil, invalid("备份包含重复设置")
		}
		fallback, ok := publicDefaults[pair[0]]
		if !ok {
			continue
		}
		var value any = pair[1]
		if _, ok := fallback.(string); !ok {
			if json.Unmarshal([]byte(pair[1]), &value) != nil {
				return nil, invalid("备份设置值无效：" + pair[0])
			}
		}
		// System integration and automatic execution must be enabled explicitly
		// on this computer after checking imported paths and credentials.
		switch pair[0] {
		case "autostart", "startStackOnLaunch", "favoriteSites", "siteGroups", "siteGroupAssignments":
			continue
		}
		if pair[0] == "portProfile" {
			c.services.mu.RLock()
			running := false
			for _, entry := range c.services.entries {
				running = running || entry.child != nil
			}
			c.services.mu.RUnlock()
			if running {
				r.Warnings = append(r.Warnings, "受管服务正在运行，未覆盖端口方案；停止服务后可重新导入。")
				continue
			}
		}
		if err = c.validateSetting(pair[0], value); err != nil {
			return nil, err
		}
		settings[pair[0]] = pair[1]
	}
	existing, err := c.sites(ctx)
	if err != nil {
		return nil, err
	}
	domains := map[string]bool{}
	domainIDs := map[string]string{}
	for _, s := range existing {
		for _, d := range s.Domains {
			domains[strings.ToLower(d)] = true
			domainIDs[strings.ToLower(d)] = s.ID
		}
	}
	sites := []Site{}
	siteIDs := map[string]string{}
	now := time.Now().UnixMilli()
	for _, site := range b.Sites {
		conflict := false
		for _, d := range site.Domains {
			if domains[strings.ToLower(strings.TrimSpace(d))] {
				conflict = true
				if site.ID != "" {
					siteIDs[site.ID] = domainIDs[strings.ToLower(strings.TrimSpace(d))]
				}
			}
		}
		if conflict {
			r.SkippedSites++
			continue
		}
		oldID := site.ID
		site.ID, err = newID("site")
		if err != nil {
			return nil, err
		}
		if err = c.validateSiteForTransfer(ctx, &site, true); err != nil {
			return nil, err
		}
		if auth := site.Runtime.BasicAuth; auth != nil && auth.Enabled {
			material, ok := b.SiteAuth[oldID]
			if !ok {
				return nil, &AppError{Code: "BACKUP_AUTH_REQUIRED", Message: "站点「" + site.Name + "」的备份不含认证信息，请从新版应用重新导出后重试"}
			}
			if _, err = bcrypt.Cost([]byte(material.Bcrypt)); err != nil || len(material.Bcrypt) != 60 {
				return nil, invalid("备份中的 Basic Auth 密码哈希无效")
			}
			authFiles[c.siteAuthPath(site.ID)] = []byte(auth.Username + ":" + material.Bcrypt + "\n")
			if site.Runtime.WebServer == "nginx" {
				decoded, e := base64.StdEncoding.DecodeString(strings.TrimPrefix(material.Nginx, "{SSHA}"))
				if e != nil || !strings.HasPrefix(material.Nginx, "{SSHA}") || len(decoded) < 36 || len(decoded) > 128 {
					return nil, invalid("备份中的 Nginx 认证哈希无效")
				}
				authFiles[c.siteAuthPath(site.ID)+".nginx"] = []byte(auth.Username + ":" + material.Nginx + "\n")
			}
			auth.HasPassword = true
		}
		if site.HTTPS && (site.Runtime.ImportedCertID != nil || site.Runtime.ACMECertID != nil) {
			if _, err = c.siteCertificate(ctx, site, false); err != nil {
				return nil, &AppError{Code: "BACKUP_CERT_REQUIRED", Message: "站点「" + site.Name + "」的绑定证书在本机尚不可用，请先恢复原证书再导入", Detail: publicError(err).Message}
			}
		}
		site = sanitizedSite(site)
		site.Status = "unconfigured"
		site.CreatedAt = now
		site.UpdatedAt = now
		if err = validateSitePHP(site, nil); err != nil {
			return nil, err
		}
		if oldID != "" {
			if _, ok := siteIDs[oldID]; ok {
				return nil, invalid("备份包含重复站点标识")
			}
			siteIDs[oldID] = site.ID
		}
		for _, d := range site.Domains {
			domains[d] = true
			domainIDs[d] = site.ID
		}
		sites = append(sites, site)
	}
	// Check redirects against the entire imported batch as well as existing sites.
	allSites := append(append([]Site{}, existing...), sites...)
	for _, site := range sites {
		if site.Runtime.Kind == "redirect" {
			visited := map[string]bool{}
			var visit func(Site) bool
			visit = func(s Site) bool {
				if visited[s.ID] {
					return true
				}
				visited[s.ID] = true
				defer delete(visited, s.ID)
				if s.Runtime.Redirect == nil {
					return false
				}
				u, e := url.Parse(s.Runtime.Redirect.Target)
				if e != nil {
					return true
				}
				for _, next := range allSites {
					if next.Runtime.Kind != "redirect" {
						continue
					}
					for _, d := range next.Domains {
						if domainMatches(u.Hostname(), d) && visit(next) {
							return true
						}
					}
				}
				return false
			}
			if visit(site) {
				return nil, &AppError{Code: "REDIRECT_CHAIN_LOOP", Message: "备份中的站点形成跳转循环"}
			}
		}
	}
	stacks := []Stack{}
	for _, s := range b.Stacks {
		s.Name = strings.TrimSpace(s.Name)
		if s.Name == "" || len(s.Name) > 512 || len(s.Description) > 8192 || len(s.Items) == 0 || len(s.Items) > 200 {
			return nil, invalid("备份服务栈无效")
		}
		for i, item := range s.Items {
			if item.ServiceID == "" || len(item.ServiceID) > 200 || strings.ContainsAny(item.ServiceID, "\x00\r\n") {
				return nil, invalid("服务栈服务标识无效")
			}
			if id, ok := siteIDs[strings.TrimPrefix(item.ServiceID, "site-app:")]; ok && strings.HasPrefix(item.ServiceID, "site-app:") {
				s.Items[i].ServiceID = "site-app:" + id
			}
		}
		sort.SliceStable(s.Items, func(i, j int) bool { return s.Items[i].Order < s.Items[j].Order })
		s.ID, err = newID("stack")
		if err != nil {
			return nil, err
		}
		s.Builtin = false
		s.CreatedAt = now
		s.UpdatedAt = now
		stacks = append(stacks, s)
	}
	for _, p := range b.ProxyProfiles {
		u, e := url.Parse(p[1])
		if strings.TrimSpace(p[0]) == "" || len(p[0]) > 512 || len(p[1]) > 8192 || e != nil || u.Hostname() == "" || (u.Scheme != "https" && u.Scheme != "http") || u.User != nil {
			return nil, invalid("代理订阅名称或地址无效")
		}
	}
	monitors := []certMonitor{}
	for _, raw := range b.CertMonitors {
		var m certMonitor
		m.Port = 443
		if json.Unmarshal(raw, &m) != nil {
			return nil, invalid("证书监控配置无效")
		}
		if len(m.Name) > 512 {
			return nil, invalid("证书监控名称过长")
		}
		m.Host, m.Port, err = normalizeMonitor(m.Host, m.Port)
		if err != nil {
			return nil, err
		}
		id, e := newID("monitor")
		if e != nil {
			return nil, e
		}
		monitors = append(monitors, certMonitor{ID: id, Name: m.Name, Host: m.Host, Port: m.Port, State: "idle", CreatedAt: now, UpdatedAt: now})
	}
	automations := []map[string]any{}
	for _, raw := range b.CertAutomations {
		var a map[string]any
		if json.Unmarshal(raw, &a) != nil || a == nil {
			return nil, invalid("证书自动化配置无效")
		}
		if err = normalizeImportedAutomation(a); err != nil {
			return nil, err
		}
		var ds []string
		encoded, _ := json.Marshal(a["domains"])
		if json.Unmarshal(encoded, &ds) != nil {
			return nil, invalid("证书自动化域名无效")
		}
		ds, err = normalizeCertDomains(ds)
		if err != nil {
			return nil, err
		}
		name, ok := a["name"].(string)
		if !ok || strings.TrimSpace(name) == "" || len(name) > 512 {
			return nil, invalid("证书自动化名称无效")
		}
		id, e := newID("auto")
		if e != nil {
			return nil, e
		}
		a["id"] = id
		a["domains"] = ds
		a["enabled"] = false
		a["state"] = "idle"
		a["lastError"] = ""
		a["deploymentId"] = ""
		a["localDeployResult"] = nil
		for _, key := range []string{"certId", "issuedAt", "expiresAt"} {
			delete(a, key)
		}
		for _, key := range []string{"failCount", "lastRunAt", "nextRenewAt"} {
			a[key] = 0
		}
		a["createdAt"] = now
		a["updatedAt"] = now
		a["runs"] = []any{}
		a["manualRecords"] = []any{}
		if targets, ok := a["targets"].([]any); ok {
			for _, target := range targets {
				if m, ok := target.(map[string]any); ok {
					delete(m, "lastResult")
				} else {
					return nil, invalid("证书部署目标无效")
				}
			}
		} else if a["targets"] != nil {
			return nil, invalid("证书部署目标无效")
		}
		automations = append(automations, a)
	}
	for _, p := range b.Packages {
		if !safePart(p[0]) || !safePart(p[1]) {
			return nil, invalid("备份套件标识无效")
		}
	}
	tx, err := c.Store.db.BeginTx(ctx, nil)
	if err != nil {
		return nil, err
	}
	defer tx.Rollback()
	for key, value := range settings {
		if _, err = tx.ExecContext(ctx, "INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", key, value); err != nil {
			return nil, err
		}
		r.Settings++
	}
	for _, site := range sites {
		values := []any{site.ID, site.Name}
		for _, v := range []any{site.Domains} {
			text, e := json.Marshal(v)
			if e != nil {
				return nil, e
			}
			values = append(values, string(text))
		}
		runtime, _ := json.Marshal(site.Runtime)
		db, _ := json.Marshal(site.DB)
		overrides, _ := json.Marshal(site.PHPOverrides)
		values = append(values, string(site.RootDir), string(runtime), site.HTTPS, site.Rewrite, string(db), string(overrides), site.CreatedAt, site.UpdatedAt)
		if _, err = tx.ExecContext(ctx, "INSERT INTO sites(id,name,domains,root_dir,runtime,https,rewrite,db,php_overrides,created_at,updated_at) VALUES(?,?,?,?,?,?,?,?,?,?,?)", values...); err != nil {
			return nil, err
		}
		r.Sites++
	}
	for _, s := range stacks {
		if err = insertStack(ctx, tx, s); err != nil {
			return nil, err
		}
		r.Stacks++
	}
	for _, p := range b.ProxyProfiles {
		var n int
		if err = tx.QueryRowContext(ctx, "SELECT COUNT(*) FROM proxy_profiles WHERE url=?", p[1]).Scan(&n); err != nil {
			return nil, err
		}
		if n > 0 {
			continue
		}
		id, e := newID("sub")
		if e != nil {
			return nil, e
		}
		if _, err = tx.ExecContext(ctx, "INSERT INTO proxy_profiles(id,name,url,active,added_at) VALUES(?,?,?,0,?)", id, p[0], p[1], now); err != nil {
			return nil, err
		}
		r.ProxyProfiles++
	}
	// Existing imported tasks remain paused and no source execution state is trusted.
	for _, a := range automations {
		var n int
		if err = tx.QueryRowContext(ctx, "SELECT COUNT(*) FROM cert_automations WHERE json_extract(data,'$.domains[0]')=?", a["domains"].([]string)[0]).Scan(&n); err != nil {
			return nil, err
		}
		if n > 0 {
			continue
		}
		encoded, e := json.Marshal(a)
		if e != nil {
			return nil, e
		}
		if _, err = tx.ExecContext(ctx, "INSERT INTO cert_automations(id,data,updated_at) VALUES(?,?,?)", a["id"], string(encoded), now); err != nil {
			return nil, err
		}
		r.CertAutomations++
	}
	for _, m := range monitors {
		var n int
		if err = tx.QueryRowContext(ctx, "SELECT COUNT(*) FROM cert_monitors WHERE lower(json_extract(data,'$.host'))=? AND json_extract(data,'$.port')=?", m.Host, m.Port).Scan(&n); err != nil {
			return nil, err
		}
		if n > 0 {
			continue
		}
		encoded, e := json.Marshal(m)
		if e != nil {
			return nil, e
		}
		if _, err = tx.ExecContext(ctx, "INSERT INTO cert_monitors(id,data,updated_at) VALUES(?,?,?)", m.ID, string(encoded), now); err != nil {
			return nil, err
		}
		r.CertMonitors++
	}
	missing := map[string]bool{}
	for _, p := range b.Packages {
		var n int
		if err = tx.QueryRowContext(ctx, "SELECT COUNT(*) FROM installed WHERE id=? AND version=?", p[0], p[1]).Scan(&n); err != nil {
			return nil, err
		}
		if n == 0 {
			missing[p[0]+"@"+p[1]] = true
		}
	}
	for key := range missing {
		r.MissingPackages = append(r.MissingPackages, key)
	}
	sort.Strings(r.MissingPackages)
	for path, data := range authFiles {
		if err = managedDirectory(c.Home, filepath.Dir(path)); err != nil {
			return nil, err
		}
		if err = journal.add(path, data, true); err != nil {
			return nil, err
		}
	}
	if err = journal.apply(ctx); err != nil {
		return nil, err
	}
	if err = tx.Commit(); err != nil {
		return nil, err
	}
	committed = true
	if r.Sites > 0 {
		r.Warnings = append(r.Warnings, "站点配置已导入，请核对本机项目目录、证书和数据库后从站点页启动；项目文件与数据库内容需另行恢复。")
	}
	if r.CertAutomations > 0 {
		r.Warnings = append(r.Warnings, "证书自动化已暂停导入，原机器的签发结果和执行记录已清空，请核对凭据后启用。")
	}
	if len(settings) < len(b.Settings) {
		r.Warnings = append(r.Warnings, "自动启动、系统集成、本机凭据及其它专用设置未覆盖，请在对应功能页重新核对。")
	}
	if err = c.refreshServices(ctx); err != nil {
		r.Warnings = append(r.Warnings, "配置已保存，服务列表刷新失败，请重启应用。")
	}
	return r, nil
}

// Imports preserve configuration but never execution state. Type-check fields
// consumed by the editor now; provider credentials are verified on explicit use.
func normalizeImportedAutomation(a map[string]any) error {
	for key, fallback := range map[string]string{"email": "", "ca": "letsencrypt", "keyAlg": "ec256", "eabKid": "", "eabHmacKey": "", "cnameTarget": "", "notifyKind": "none", "notifyUrl": ""} {
		v, ok := a[key]
		if !ok {
			a[key] = fallback
			continue
		}
		text, ok := v.(string)
		if !ok || len(text) > 16384 {
			return invalid("证书自动化文本配置无效：" + key)
		}
	}
	for key, fallback := range map[string]float64{"dnsWaitSec": 0, "renewDaysAhead": 30, "retryTimes": 3, "retryIntervalMin": 30} {
		v, ok := a[key]
		if !ok {
			a[key] = fallback
			continue
		}
		n, ok := v.(float64)
		if !ok || n < 0 || n > 1000000 || math.Trunc(n) != n {
			return invalid("证书自动化数值配置无效：" + key)
		}
	}
	if v, ok := a["deployLocal"]; ok {
		if _, ok := v.(bool); !ok {
			return invalid("证书本地部署选项无效")
		}
	} else {
		a["deployLocal"] = false
	}
	if a["dns"] == nil {
		a["dns"] = map[string]any{"kind": "manual", "accessKey": "", "secret": ""}
	}
	dns, ok := a["dns"].(map[string]any)
	if !ok {
		return invalid("DNS 服务商配置无效")
	}
	for _, key := range []string{"kind", "accessKey", "secret"} {
		if v, ok := dns[key]; ok {
			if text, ok := v.(string); !ok || len(text) > 16384 {
				return invalid("DNS 服务商配置无效")
			}
		} else {
			dns[key] = ""
		}
	}
	if a["targets"] == nil {
		a["targets"] = []any{}
	}
	targets, ok := a["targets"].([]any)
	if !ok || len(targets) > 100 {
		return invalid("证书部署目标无效")
	}
	for _, raw := range targets {
		target, ok := raw.(map[string]any)
		if !ok {
			return invalid("证书部署目标无效")
		}
		for _, key := range []string{"id", "kind", "name"} {
			v, ok := target[key].(string)
			if !ok || len(v) > 512 {
				return invalid("证书部署目标信息无效")
			}
		}
		if target["config"] == nil {
			target["config"] = map[string]any{}
		}
		config, ok := target["config"].(map[string]any)
		if !ok || len(config) > 100 {
			return invalid("证书部署参数无效")
		}
		for _, v := range config {
			if s, ok := v.(string); !ok || len(s) > 65536 {
				return invalid("证书部署参数必须为文本")
			}
		}
		// Normalize only local path operands; never rewrite remote paths, scripts,
		// credentials or arbitrary strings containing the Windows path prefix.
		keys := []string{}
		if target["kind"] == "local" {
			keys = []string{"certPath", "keyPath"}
		}
		if target["kind"] == "ssh" {
			keys = []string{"identityFile", "privateKey"}
		}
		for _, key := range keys {
			if s, ok := config[key].(string); ok {
				config[key] = DisplayPath(s)
			}
		}
	}
	if raw := a["notifySmtp"]; raw != nil {
		smtp, ok := raw.(map[string]any)
		if !ok {
			return invalid("邮件通知配置无效")
		}
		for _, key := range []string{"host", "username", "password", "from", "to"} {
			if v, ok := smtp[key]; ok {
				if s, ok := v.(string); !ok || len(s) > 16384 {
					return invalid("邮件通知配置无效")
				}
			} else {
				smtp[key] = ""
			}
		}
		if smtp["port"] == nil {
			smtp["port"] = float64(587)
		}
		port, ok := smtp["port"].(float64)
		if !ok || port < 1 || port > 65535 || math.Trunc(port) != port {
			return invalid("邮件通知端口无效")
		}
		if smtp["implicitTls"] == nil {
			smtp["implicitTls"] = false
		}
		if _, ok := smtp["implicitTls"].(bool); !ok {
			return invalid("邮件通知 TLS 配置无效")
		}
	}
	return nil
}
