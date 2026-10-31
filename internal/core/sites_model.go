package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"regexp"
)

type siteApplication struct {
	Version string   `json:"version"`
	Args    []string `json:"args"`
	CWD     *Path    `json:"cwd,omitempty"`
}
type siteAccess struct {
	Mode      string   `json:"mode"`
	Addresses []string `json:"addresses"`
}
type siteBasicAuth struct {
	nginxHash   string
	Enabled     bool    `json:"enabled"`
	Username    string  `json:"username"`
	Password    *string `json:"password,omitempty"`
	HasPassword bool    `json:"hasPassword"`
}
type siteCORS struct {
	Origins        []string `json:"origins"`
	Methods        []string `json:"methods"`
	AllowedHeaders []string `json:"allowedHeaders"`
	ExposedHeaders []string `json:"exposedHeaders"`
	Credentials    bool     `json:"credentials"`
	MaxAge         int      `json:"maxAge"`
}
type siteProxyRule struct {
	Path        string `json:"path"`
	Target      string `json:"target"`
	StripPrefix bool   `json:"stripPrefix"`
}
type siteRedirect struct {
	Target       string `json:"target"`
	Status       int    `json:"status"`
	PreservePath bool   `json:"preservePath"`
}
type siteRewrite struct {
	Name    string `json:"name"`
	Server  string `json:"server"`
	Content string `json:"content"`
}
type siteRuntime struct {
	Access         *siteAccess      `json:"access,omitempty"`
	BasicAuth      *siteBasicAuth   `json:"basicAuth,omitempty"`
	HTTPSRedirect  *int             `json:"httpsRedirect,omitempty"`
	ProxyRules     []siteProxyRule  `json:"proxyRules,omitempty"`
	ErrorPages     map[int]string   `json:"errorPages,omitempty"`
	CORS           *siteCORS        `json:"cors,omitempty"`
	Redirect       *siteRedirect    `json:"redirect,omitempty"`
	CustomRewrite  *siteRewrite     `json:"customRewrite,omitempty"`
	Application    *siteApplication `json:"application,omitempty"`
	ImportedCertID *string          `json:"importedCertId,omitempty"`
	ACMECertID     *string          `json:"acmeCertId,omitempty"`
	WebServer      string           `json:"webServer"`
	Kind           string           `json:"kind"`
	PHPVersion     *string          `json:"phpVersion,omitempty"`
	ProxyTarget    *string          `json:"proxyTarget,omitempty"`
	Command        *string          `json:"command,omitempty"`
	CWD            *Path            `json:"cwd,omitempty"`
}
type siteDB struct {
	Enabled  bool    `json:"enabled"`
	Database string  `json:"database"`
	Username string  `json:"username"`
	Password string  `json:"password,omitempty"`
	Version  *string `json:"version,omitempty"`
	Port     *int    `json:"port,omitempty"`
}
type Site struct {
	AccessURL    *string           `json:"accessUrl,omitempty"`
	ID           string            `json:"id"`
	Name         string            `json:"name"`
	Domains      []string          `json:"domains"`
	RootDir      Path              `json:"rootDir"`
	Runtime      siteRuntime       `json:"runtime"`
	HTTPS        bool              `json:"https"`
	Rewrite      string            `json:"rewrite"`
	DB           *siteDB           `json:"db"`
	Status       string            `json:"status"`
	PHPOverrides map[string]string `json:"phpOverrides"`
	CreatedAt    int64             `json:"createdAt"`
	UpdatedAt    int64             `json:"updatedAt"`
}

const siteSelect = `SELECT id,name,domains,root_dir,runtime,https,rewrite,COALESCE(db,'null'),COALESCE(php_overrides,'null'),created_at,updated_at FROM sites`

var siteIdentifier = regexp.MustCompile(`^[A-Za-z0-9_-]{1,160}$`)

func scanSite(row interface{ Scan(...any) error }) (Site, error) {
	var site Site
	var domains, runtime, db, rewrite, overrides string
	err := row.Scan(&site.ID, &site.Name, &domains, &site.RootDir, &runtime, &site.HTTPS, &rewrite, &db, &overrides, &site.CreatedAt, &site.UpdatedAt)
	if err != nil {
		return site, err
	}
	for _, value := range []struct {
		raw    string
		target any
	}{{domains, &site.Domains}, {runtime, &site.Runtime}, {db, &site.DB}, {overrides, &site.PHPOverrides}} {
		if json.Unmarshal([]byte(value.raw), value.target) != nil {
			return site, invalid("站点记录格式无效")
		}
	}
	if json.Unmarshal([]byte(rewrite), &site.Rewrite) != nil {
		site.Rewrite = rewrite
	}
	if site.Runtime.WebServer == "" {
		site.Runtime.WebServer = "nginx"
	}
	if site.Rewrite == "" {
		site.Rewrite = "none"
	}
	return site, nil
}
func (c *Core) site(ctx context.Context, id string) (Site, error) {
	if !siteIdentifier.MatchString(id) {
		return Site{}, invalid("站点标识无效")
	}
	site, err := scanSite(c.Store.db.QueryRowContext(ctx, siteSelect+" WHERE id=?", id))
	if errors.Is(err, sql.ErrNoRows) {
		return site, &AppError{Code: "SITE_NOT_FOUND", Message: "站点不存在"}
	}
	return site, err
}
func (c *Core) sites(ctx context.Context) ([]Site, error) {
	rows, err := c.Store.db.QueryContext(ctx, siteSelect+" ORDER BY updated_at DESC,id")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []Site{}
	for rows.Next() {
		site, err := scanSite(rows)
		if err != nil {
			return nil, err
		}
		out = append(out, site)
	}
	return out, rows.Err()
}
func (c *Core) siteStatus(site Site) string {
	if !siteIdentifier.MatchString(site.ID) {
		return "error"
	}
	server := site.Runtime.WebServer
	if server != "nginx" && server != "apache" && server != "caddy" {
		return "error"
	}
	path := filepath.Join(c.Home, "etc", server, "sites", site.ID+".conf")
	info, err := os.Lstat(path)
	if os.IsNotExist(err) {
		if disabled, err := os.Lstat(path + ".disabled"); err == nil && disabled.Mode().IsRegular() {
			return "stopped"
		}
		return "unconfigured"
	}
	if err != nil || !info.Mode().IsRegular() {
		return "error"
	}
	dependencies := []string{server}
	if site.Runtime.Kind == "php" {
		if site.Runtime.PHPVersion == nil || *site.Runtime.PHPVersion == "" {
			return "unconfigured"
		}
		dependencies = append(dependencies, "php@"+*site.Runtime.PHPVersion)
	}
	if site.Runtime.Application != nil {
		dependencies = append(dependencies, "site-app:"+site.ID)
	}
	c.services.mu.RLock()
	defer c.services.mu.RUnlock()
	running := true
	for _, id := range dependencies {
		entry := c.services.entries[id]
		if entry != nil && entry.status.State == "error" {
			return "error"
		}
		running = running && entry != nil && entry.child != nil && entry.status.State == "running"
	}
	if running {
		return "running"
	}
	return "stopped"
}
func applicationRuntimeID(kind string) string {
	switch kind {
	case "node", "python", "go":
		return kind
	case "java":
		return "temurin-jdk21"
	}
	return ""
}
