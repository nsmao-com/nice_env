package core

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

func (c *Core) registerSites() {
	c.handle("create_site", c.createSite)
	c.handle("update_site", c.updateSite)
	c.handle("delete_site", c.deleteSite)
	c.handle("sites_start_many", c.sitesStartMany)
	c.handle("sites_stop_many", c.sitesStopMany)
	c.handle("start_site", func(ctx context.Context, data json.RawMessage) (any, error) { return c.siteAction(ctx, data, true) })
	c.handle("stop_site", func(ctx context.Context, data json.RawMessage) (any, error) { return c.siteAction(ctx, data, false) })
	c.handle("site_access_url", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		site, err := c.site(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		return c.siteAccessURL(ctx, site)
	})
}

func (c *Core) siteEnabled(site Site) (bool, error) {
	if !siteIdentifier.MatchString(site.ID) {
		return false, invalid("站点标识无效")
	}
	path := filepath.Join(c.Home, "etc", site.Runtime.WebServer, "sites", site.ID+".conf")
	_, err := readConfigText(path)
	if os.IsNotExist(err) {
		return false, nil
	}
	return err == nil, err
}
func (c *Core) siteOptions(ctx context.Context, site Site, hash string, createCert bool) (siteConfigOptions, error) {
	options := siteConfigOptions{authHash: hash}
	server, err := c.serviceEntry(site.Runtime.WebServer, "")
	if err != nil {
		return options, err
	}
	c.services.mu.RLock()
	options.http = server.status.Port
	c.services.mu.RUnlock()
	profile, _, err := c.Store.setting(ctx, "portProfile")
	if err != nil {
		return options, err
	}
	options.https = 443
	if profile == "safe" {
		options.https = 8443
	}
	key := "https"
	if site.Runtime.WebServer == "apache" {
		key = "apacheHttps"
		options.https = 8443
	}
	if site.Runtime.WebServer == "caddy" {
		key = "caddyHttps"
		options.https = 8443
	}
	if raw, ok, err := c.Store.setting(ctx, "portOverride."+key); err != nil {
		return options, err
	} else if ok {
		options.https, err = strconv.Atoi(raw)
		if err != nil {
			return options, invalid("HTTPS 端口无效")
		}
	}
	if options.http < 1 || options.http > 65535 || options.https < 1 || options.https > 65535 || site.HTTPS && options.http == options.https {
		return options, invalid("HTTP 与 HTTPS 端口无效或相同")
	}
	network, _, err := c.Store.setting(ctx, "webLan."+site.Runtime.WebServer)
	if err != nil {
		return options, err
	}
	if network != "" && network != "default" && network != "true" && network != "false" {
		return options, invalid("局域网访问设置无效")
	}
	options.network = network == "true"
	if hash != "" {
		options.auth = c.siteAuthPath(site.ID)
		if site.Runtime.WebServer == "nginx" {
			options.auth += ".nginx"
		}
	}
	if site.Runtime.Kind == "php" {
		entry, err := c.serviceEntry("php@"+*site.Runtime.PHPVersion, "")
		if err != nil {
			return options, err
		}
		c.services.mu.RLock()
		options.php = entry.status.Port
		c.services.mu.RUnlock()
	}
	cert, err := c.siteCertificate(ctx, site, createCert)
	if err != nil {
		return options, err
	}
	options.certificate = string(cert.CertPath)
	options.key = string(cert.KeyPath)
	return options, nil
}

const siteFastCGIParams = `fastcgi_param SCRIPT_FILENAME $document_root$fastcgi_script_name;
fastcgi_param QUERY_STRING $query_string;
fastcgi_param REQUEST_METHOD $request_method;
fastcgi_param CONTENT_TYPE $content_type;
fastcgi_param CONTENT_LENGTH $content_length;
fastcgi_param SCRIPT_NAME $fastcgi_script_name;
fastcgi_param REQUEST_URI $request_uri;
fastcgi_param DOCUMENT_URI $document_uri;
fastcgi_param DOCUMENT_ROOT $document_root;
fastcgi_param SERVER_PROTOCOL $server_protocol;
fastcgi_param REQUEST_SCHEME $scheme;
fastcgi_param HTTPS $https if_not_empty;
fastcgi_param GATEWAY_INTERFACE CGI/1.1;
fastcgi_param SERVER_SOFTWARE nginx;
fastcgi_param REMOTE_ADDR $remote_addr;
fastcgi_param REMOTE_PORT $remote_port;
fastcgi_param SERVER_ADDR $server_addr;
fastcgi_param SERVER_PORT $server_port;
fastcgi_param SERVER_NAME $server_name;
fastcgi_param REDIRECT_STATUS 200;
`

func (c *Core) prepareSiteFiles(ctx context.Context, journal *siteFileJournal, site Site, previous *Site, enabled bool, hash string) error {
	options, err := c.siteOptions(ctx, site, hash, true)
	if err != nil {
		return err
	}
	var content string
	if site.Runtime.WebServer == "apache" {
		content, err = c.renderApacheSite(site, options)
	} else if site.Runtime.WebServer == "caddy" {
		content, err = c.renderCaddySite(site, options)
	} else {
		content, err = c.renderNginxSite(site, options)
	}
	if err != nil {
		return err
	}
	for _, server := range []string{"nginx", "apache", "caddy"} {
		dir := filepath.Join(c.Home, "etc", server, "sites")
		if err = managedDirectory(c.Home, dir); err != nil {
			return err
		}
		for _, disabled := range []bool{false, true} {
			suffix := ".conf"
			if disabled {
				suffix += ".disabled"
			}
			present := server == site.Runtime.WebServer && disabled != enabled
			var data []byte
			if present {
				data = []byte(content)
			}
			if err = journal.add(filepath.Join(dir, site.ID+suffix), data, present); err != nil {
				return err
			}
		}
	}
	if err = managedDirectory(c.Home, filepath.Dir(c.siteAuthPath(site.ID))); err != nil {
		return err
	}
	var auth []byte
	if hash != "" {
		auth = []byte(site.Runtime.BasicAuth.Username + ":" + hash + "\n")
	}
	if err = journal.add(c.siteAuthPath(site.ID), auth, hash != ""); err != nil {
		return err
	}
	var nginxAuth []byte
	if hash != "" {
		_, sidecarErr := os.Lstat(c.siteAuthPath(site.ID) + ".nginx")
		if site.Runtime.WebServer == "nginx" || site.Runtime.BasicAuth.nginxHash != "" || sidecarErr == nil {
			portable, err := c.nginxSiteAuth(site, previous)
			if err != nil {
				return err
			}
			nginxAuth = []byte(site.Runtime.BasicAuth.Username + ":" + portable + "\n")
		} else if !os.IsNotExist(sidecarErr) {
			return sidecarErr
		}
	}
	if err = journal.add(c.siteAuthPath(site.ID)+".nginx", nginxAuth, len(nginxAuth) > 0); err != nil {
		return err
	}
	if err = managedDirectory(c.Home, filepath.Join(c.Home, "logs", site.Runtime.WebServer)); err != nil {
		return err
	}
	params := filepath.Join(c.Home, "etc", "nginx", "fastcgi_params")
	if _, err = readConfigText(params); os.IsNotExist(err) {
		if err = journal.add(params, []byte(siteFastCGIParams), true); err != nil {
			return err
		}
	} else if err != nil {
		return err
	}
	return c.prepareSiteIni(ctx, journal, site, previous)
}

func (c *Core) validateSiteInstalled(ctx context.Context, site Site) error {
	if _, err := c.serviceEntry(site.Runtime.WebServer, ""); err != nil {
		return err
	}
	if site.Runtime.Kind == "php" {
		if _, err := c.serviceEntry("php@"+*site.Runtime.PHPVersion, ""); err != nil {
			return err
		}
	}
	if site.Runtime.Kind != "redirect" {
		info, err := os.Stat(string(site.RootDir))
		if err != nil {
			return err
		}
		if !info.IsDir() {
			return invalid("站点根目录不存在")
		}
	}
	if site.Runtime.Kind == "redirect" && (site.Rewrite != "none" || site.DB != nil && site.DB.Enabled) {
		return invalid("跳转站点不使用伪静态或数据库绑定")
	}
	return nil
}
func (c *Core) ensureSiteServices(ctx context.Context, site Site, started *[]*serviceEntry) error {
	if site.Runtime.Application != nil {
		e, err := c.ensureSiteApplication(site)
		if err != nil {
			return err
		}
		c.services.mu.RLock()
		active := e.child != nil
		c.services.mu.RUnlock()
		if !active {
			*started = append(*started, e)
		}
		if err = c.startService(ctx, e); err != nil {
			return err
		}
	}
	if site.Runtime.Kind == "php" {
		e, err := c.serviceEntry("php@"+*site.Runtime.PHPVersion, "")
		if err != nil {
			return err
		}
		c.services.mu.RLock()
		active := e.child != nil
		c.services.mu.RUnlock()
		if !active {
			*started = append(*started, e)
		}
		if err = c.startService(ctx, e); err != nil {
			return err
		}
	}
	return nil
}
func (c *Core) applySiteWeb(ctx context.Context, site Site, start bool, started *[]*serviceEntry) error {
	e, err := c.serviceEntry(site.Runtime.WebServer, "")
	if err != nil {
		return err
	}
	c.services.mu.RLock()
	running := e.child != nil && e.status.State == "running"
	active := e.child != nil
	c.services.mu.RUnlock()
	if running {
		if e.pkg.ID == "caddy" {
			return c.reloadCaddy(ctx)
		}
		if e.pkg.ID == "apache" {
			return c.reloadApache(ctx)
		}
		return c.reloadNginxPools(ctx)
	}
	if start {
		if !active {
			*started = append(*started, e)
		}
		return c.startService(ctx, e)
	}
	r, err := c.resolveService(ctx, e)
	if err != nil {
		return err
	}
	if err = c.prepareServiceDirectories(e, r); err != nil {
		return err
	}
	if e.pkg.ID == "apache" {
		return c.prepareApache(ctx, e, r)
	}
	if e.pkg.ID == "caddy" {
		return c.prepareCaddy(ctx, e, r)
	}
	return c.prepareNginx(ctx, e, r)
}
func (c *Core) rollbackSiteServices(ctx context.Context, started []*serviceEntry) error {
	failures := []error{}
	for i := len(started) - 1; i >= 0; i-- {
		if err := c.stopOwned(ctx, started[i], false); err != nil {
			failures = append(failures, err)
		}
	}
	if err := c.reloadNginxPools(ctx); err != nil {
		failures = append(failures, err)
	}
	c.services.mu.RLock()
	apache := c.services.entries["apache"]
	active := apache != nil && apache.child != nil
	c.services.mu.RUnlock()
	if active {
		if err := c.reloadApache(ctx); err != nil {
			failures = append(failures, err)
		}
	}
	if err := c.reloadWebIfRunning(ctx, "caddy"); err != nil {
		failures = append(failures, err)
	}
	return errors.Join(failures...)
}

func (c *Core) createSite(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Input struct {
			Site
			Template        string  `json:"template"`
			WriteEnvExample bool    `json:"writeEnvExample"`
			CreateDB        *siteDB `json:"createDb"`
		} `json:"input"`
		ExistingProject    *string `json:"existingProject"`
		AllowUnverifiedPHP bool    `json:"allowUnverifiedPhp"`
	}](data)
	if err != nil {
		return nil, err
	}
	unlock, err := c.siteLock()
	if err != nil {
		return nil, err
	}
	defer unlock()
	site := in.Input.Site
	site.ID, err = newID("site")
	if err != nil {
		return nil, err
	}
	site.CreatedAt = time.Now().UnixMilli()
	site.UpdatedAt = site.CreatedAt
	if err = c.validateSite(ctx, &site); err != nil {
		return nil, err
	}
	if in.Input.Template != "" && in.Input.Template != "none" || in.Input.WriteEnvExample {
		return nil, &AppError{Code: "SITE_SCAFFOLD_PENDING", Message: "项目脚手架正在迁移"}
	}
	if in.ExistingProject != nil {
		if site.PHPOverrides != nil {
			return nil, invalid("使用已有项目时不能改写 PHP 项目设置")
		}
		if err = c.validateExistingProject(ctx, site, DisplayPath(*in.ExistingProject), in.AllowUnverifiedPHP); err != nil {
			return nil, err
		}
	}
	if in.Input.CreateDB != nil || site.DB != nil && site.DB.Enabled {
		return nil, &AppError{Code: "SITE_DATABASE_PROVISION_PENDING", Message: "站点数据库创建正在迁移"}
	}
	if err = c.validateSiteInstalled(ctx, site); err != nil {
		return nil, err
	}
	if err = validateSitePHP(site, nil); err != nil {
		return nil, err
	}
	hash, err := c.siteAuth(&site, nil)
	if err != nil {
		return nil, err
	}
	journal := c.newSiteJournal()
	if err = c.prepareSiteFiles(ctx, journal, site, nil, true, hash); err != nil {
		return nil, err
	}
	started := []*serviceEntry{}
	saved, hostsAttempted := false, false
	err = func() error {
		if err := c.ensureSiteServices(ctx, site, &started); err != nil {
			return err
		}
		if err := journal.apply(ctx); err != nil {
			return err
		}
		if err := c.saveSite(ctx, site, true); err != nil {
			return err
		}
		saved = true
		if err := c.applySiteWeb(ctx, site, true, &started); err != nil {
			return err
		}
		hostsAttempted = true
		_, err := c.changeHostsWithSiteLock(ctx, json.RawMessage(`{}`), "rebuild", true)
		return err
	}()
	if err != nil {
		return nil, c.siteRollback(err, journal, func(restore context.Context) error {
			failures := []error{}
			if saved {
				_, err := c.Store.db.ExecContext(restore, "DELETE FROM sites WHERE id=?", site.ID)
				failures = append(failures, err)
			}
			if hostsAttempted {
				_, err := c.changeHostsWithSiteLock(restore, json.RawMessage(`{}`), "rebuild", true)
				failures = append(failures, err)
			}
			failures = append(failures, c.rollbackSiteServices(restore, started))
			return errors.Join(failures...)
		})
	}
	site = sanitizedSite(site)
	site.Status = c.siteStatus(site)
	return site, nil
}

func (c *Core) updateSite(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Site json.RawMessage `json:"site"`
	}](data)
	if err != nil {
		return nil, err
	}
	key, err := decode[struct {
		ID string `json:"id"`
	}](in.Site)
	if err != nil {
		return nil, err
	}
	unlock, err := c.siteLock()
	if err != nil {
		return nil, err
	}
	defer unlock()
	previous, err := c.site(ctx, key.ID)
	if err != nil {
		return nil, err
	}
	// Decode onto a deep copy, so omitted patch fields retain their saved values.
	encoded, err := json.Marshal(previous)
	if err != nil {
		return nil, err
	}
	var site Site
	if err = json.Unmarshal(encoded, &site); err != nil {
		return nil, err
	}
	if err = json.Unmarshal(in.Site, &site); err != nil {
		return nil, invalid("站点修改格式无效")
	}
	site.ID = previous.ID
	site.CreatedAt = previous.CreatedAt
	site.UpdatedAt = time.Now().UnixMilli()
	site.DB = previous.DB
	c.services.mu.RLock()
	app := c.services.entries["site-app:"+site.ID]
	appRunning := app != nil && app.child != nil
	c.services.mu.RUnlock()
	if appRunning && siteApplicationKey(site) != siteApplicationKey(previous) {
		return nil, &AppError{Code: "APP_RUNNING", Message: "应用正在运行，请先停止站点后修改运行时、入口、参数、目录或监听地址"}
	}
	if err = c.validateSite(ctx, &site); err != nil {
		return nil, err
	}
	if err = c.validateSiteInstalled(ctx, site); err != nil {
		return nil, err
	}
	enabled, err := c.siteEnabled(previous)
	if err != nil {
		return nil, err
	}
	running := c.siteStatus(previous) == "running"
	hash, err := c.siteAuth(&site, &previous)
	if err != nil {
		return nil, err
	}
	journal := c.newSiteJournal()
	if err = c.prepareSiteFiles(ctx, journal, site, &previous, enabled, hash); err != nil {
		return nil, err
	}
	started := []*serviceEntry{}
	saved, hostsAttempted := false, false
	err = func() error {
		if running {
			if err := c.ensureSiteServices(ctx, site, &started); err != nil {
				return err
			}
		}
		if err := journal.apply(ctx); err != nil {
			return err
		}
		if err := c.saveSite(ctx, site, false); err != nil {
			return err
		}
		saved = true
		if err := c.applySiteWeb(ctx, site, running, &started); err != nil {
			return err
		}
		if site.Runtime.WebServer != previous.Runtime.WebServer {
			if err := c.reloadWebIfRunning(ctx, previous.Runtime.WebServer); err != nil {
				return err
			}
		}
		hostsAttempted = true
		_, err := c.changeHostsWithSiteLock(ctx, json.RawMessage(`{}`), "rebuild", true)
		return err
	}()
	if err != nil {
		return nil, c.siteRollback(err, journal, func(restore context.Context) error {
			failures := []error{}
			if saved {
				failures = append(failures, c.saveSite(restore, previous, false))
			}
			if hostsAttempted {
				_, err := c.changeHostsWithSiteLock(restore, json.RawMessage(`{}`), "rebuild", true)
				failures = append(failures, err)
			}
			failures = append(failures, c.rollbackSiteServices(restore, started))
			return errors.Join(failures...)
		})
	}
	site = sanitizedSite(site)
	site.Status = c.siteStatus(site)
	return site, nil
}

func (c *Core) siteAction(ctx context.Context, data json.RawMessage, start bool) (any, error) {
	in, err := decode[struct {
		ID string `json:"id"`
	}](data)
	if err != nil {
		return nil, err
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
	journal := c.newSiteJournal()
	if start {
		if err = c.validateSite(ctx, &site); err != nil {
			return nil, err
		}
		hash, err := c.siteAuth(&site, nil)
		if err != nil {
			return nil, err
		}
		if err = c.prepareSiteFiles(ctx, journal, site, &site, true, hash); err != nil {
			return nil, err
		}
	} else {
		if err = c.disableSiteFiles(journal, site); err != nil {
			return nil, err
		}
	}
	started := []*serviceEntry{}
	err = func() error {
		if start {
			if err := c.ensureSiteServices(ctx, site, &started); err != nil {
				return err
			}
		}
		if err := journal.apply(ctx); err != nil {
			return err
		}
		if err := c.applySiteWeb(ctx, site, start, &started); err != nil {
			return err
		}
		// hosts is unchanged by stopping a site. Do it before stopping the
		// application so a hosts failure cannot leave its restored vhost dead.
		if start {
			if _, err := c.changeHostsWithSiteLock(ctx, json.RawMessage(`{}`), "rebuild", true); err != nil {
				return err
			}
		}
		if !start && site.Runtime.Application != nil {
			e, err := c.serviceEntry("site-app:"+site.ID, "")
			if err != nil {
				return err
			}
			if err = c.stopOwned(ctx, e, false); err != nil {
				return err
			}
		}
		return nil
	}()
	if err != nil {
		return nil, c.siteRollback(err, journal, func(restore context.Context) error { return c.rollbackSiteServices(restore, started) })
	}
	return true, nil
}

func (c *Core) siteAccessURL(ctx context.Context, site Site) (string, error) {
	if c.siteStatus(site) != "running" {
		return "", &AppError{Code: "SITE_NOT_RUNNING", Message: "站点尚未由受管 Web 服务运行"}
	}
	c.services.mu.RLock()
	entry := c.services.entries[site.Runtime.WebServer]
	text := ""
	if entry != nil && entry.resolved != nil {
		text = entry.resolved.siteConfigs[site.ID]
	}
	c.services.mu.RUnlock()
	if text == "" {
		return "", invalid("未找到站点最近成功加载的配置，请重新启动站点")
	}
	domain := site.Domains[0]
	for _, name := range site.Domains {
		if !strings.HasPrefix(name, "*.") {
			domain = name
			break
		}
	}
	domain = strings.Replace(domain, "*.", "www.", 1)
	port := 0
	secure := false
	if site.Runtime.WebServer != "nginx" {
		var err error
		port, secure, err = otherWebEndpoint(site.Runtime.WebServer, domain, text)
		if err != nil {
			return "", err
		}
	} else {
		nodes, err := parseNginx(text)
		if err != nil {
			return "", err
		}
		for _, node := range nodes {
			if len(node.words) == 0 || node.words[0] != "server" {
				continue
			}
			hostMatch := false
			for _, child := range node.children {
				if len(child.words) > 1 && child.words[0] == "server_name" {
					for _, name := range child.words[1:] {
						hostMatch = hostMatch || domainMatches(domain, name)
					}
				}
			}
			if !hostMatch {
				continue
			}
			for _, child := range node.children {
				if len(child.words) < 2 || child.words[0] != "listen" {
					continue
				}
				host, p, err := net.SplitHostPort(child.words[1])
				if err != nil || host != "127.0.0.1" && host != "0.0.0.0" {
					continue
				}
				number, err := strconv.Atoi(p)
				if err != nil {
					continue
				}
				tls := false
				for _, flag := range child.words[2:] {
					tls = tls || flag == "ssl"
				}
				if port == 0 || tls && !secure {
					port = number
					secure = tls
				}
			}
		}
	}
	if port < 1 || port > 65535 {
		return "", invalid("未找到站点有效监听地址")
	}
	listeners, err := c.portListeners(ctx, port, port)
	if err != nil {
		return "", err
	}
	owned := false
	for _, listener := range listeners {
		owned = owned || listener.OwnedBySelf && listener.ServiceID == site.Runtime.WebServer
	}
	if !owned {
		return "", invalid("站点端口不属于当前受管 Web 服务")
	}
	scheme := "http"
	suffix := fmt.Sprintf(":%d", port)
	if secure {
		scheme = "https"
	}
	if !secure && port == 80 || secure && port == 443 {
		suffix = ""
	}
	return scheme + "://" + domain + suffix + "/", nil
}
