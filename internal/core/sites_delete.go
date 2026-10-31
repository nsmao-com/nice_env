package core

import (
	"context"
	"encoding/json"
	"errors"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
)

func (c *Core) disableSiteFiles(journal *siteFileJournal, site Site) error {
	if !siteIdentifier.MatchString(site.ID) {
		return invalid("站点标识无效")
	}
	for _, server := range []string{"nginx", "apache", "caddy"} {
		path := filepath.Join(c.Home, "etc", server, "sites", site.ID+".conf")
		text, err := readConfigText(path)
		if os.IsNotExist(err) {
			continue
		}
		if err != nil {
			return err
		}
		if err = journal.add(path+".disabled", []byte(text), true); err != nil {
			return err
		}
		if err = journal.add(path, nil, false); err != nil {
			return err
		}
	}
	return nil
}

func (c *Core) removableSiteCertificate(ctx context.Context, site Site) (*certRecord, error) {
	if site.Runtime.ImportedCertID != nil || site.Runtime.ACMECertID != nil || len(site.Domains) == 0 {
		return nil, nil
	}
	records, err := c.certificateRecords(ctx)
	if err != nil {
		return nil, err
	}
	var found *certRecord
	for i := range records {
		if records[i].Kind == "site" && records[i].Subject == site.Domains[0] {
			found = &records[i]
			break
		}
	}
	if found == nil {
		return nil, nil
	}
	if err = c.validateCertificatePath(*found); err != nil {
		return nil, err
	}
	sites, err := c.certificateSites(ctx)
	if err != nil {
		return nil, err
	}
	for _, other := range sites {
		if other.ID != site.ID && certUsed(other, *found) {
			return nil, nil
		}
	}
	rows, err := c.Store.db.QueryContext(ctx, "SELECT data FROM cert_automations")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	for rows.Next() {
		var raw string
		if err = rows.Scan(&raw); err != nil {
			return nil, err
		}
		var automation struct {
			Domains []string `json:"domains"`
		}
		if err = json.Unmarshal([]byte(raw), &automation); err != nil {
			return nil, invalid("证书自动化记录无效，未删除证书")
		}
		if len(automation.Domains) > 0 && automation.Domains[0] == found.Subject {
			return nil, nil
		}
	}
	return found, rows.Err()
}

func (c *Core) deleteSite(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		ID    string `json:"id"`
		Hosts bool   `json:"hosts"`
		Certs bool   `json:"certs"`
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
	paths := []string{c.siteAuthPath(site.ID), c.siteAuthPath(site.ID) + ".nginx"}
	for _, server := range []string{"nginx", "apache", "caddy"} {
		for _, suffix := range []string{".conf", ".conf.disabled"} {
			paths = append(paths, filepath.Join(c.Home, "etc", server, "sites", site.ID+suffix))
		}
	}
	var certificate *certRecord
	if in.Certs {
		certificate, err = c.removableSiteCertificate(ctx, site)
		if err != nil {
			return nil, err
		}
		if certificate != nil {
			paths = append(paths, string(certificate.CertPath), string(certificate.KeyPath))
		}
	}
	for _, path := range paths {
		if _, err := os.Stat(filepath.Dir(path)); os.IsNotExist(err) {
			continue
		} else if err != nil {
			return nil, err
		}
		if err = journal.add(path, nil, false); err != nil {
			return nil, err
		}
	}
	previousExtras, extraExists, err := c.Store.setting(ctx, "extraHosts")
	if err != nil {
		return nil, err
	}
	extras := [][2]string{}
	if previousExtras != "" && json.Unmarshal([]byte(previousExtras), &extras) != nil {
		return nil, invalid("hosts 设置无效")
	}
	if !in.Hosts {
		for _, domain := range site.Domains {
			if strings.HasPrefix(domain, "*.") || domain == "localhost" {
				continue
			}
			if _, err := netip.ParseAddr(domain); err == nil {
				continue
			}
			exists := false
			for _, item := range extras {
				exists = exists || item[1] == domain
			}
			if !exists {
				extras = append(extras, [2]string{"127.0.0.1", domain})
			}
		}
	}
	c.services.mu.RLock()
	app := c.services.entries["site-app:"+site.ID]
	appRunning := app != nil && app.child != nil
	c.services.mu.RUnlock()
	saved, hostsAttempted, appStopAttempted := false, false, false
	err = func() error {
		if err := journal.apply(ctx); err != nil {
			return err
		}
		tx, err := c.Store.db.BeginTx(ctx, nil)
		if err != nil {
			return err
		}
		defer tx.Rollback()
		if _, err = tx.ExecContext(ctx, "DELETE FROM sites WHERE id=?", site.ID); err != nil {
			return err
		}
		if certificate != nil {
			if _, err = tx.ExecContext(ctx, "DELETE FROM certs WHERE id=?", certificate.ID); err != nil {
				return err
			}
		}
		if !in.Hosts {
			encoded, err := json.Marshal(extras)
			if err != nil {
				return err
			}
			if _, err = tx.ExecContext(ctx, "INSERT INTO settings(key,value) VALUES('extraHosts',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", string(encoded)); err != nil {
				return err
			}
		}
		if err = tx.Commit(); err != nil {
			return err
		}
		saved = true
		if in.Hosts {
			hostsAttempted = true
			if _, err = c.changeHostsWithSiteLock(ctx, json.RawMessage(`{}`), "rebuild", true); err != nil {
				return err
			}
		}
		started := []*serviceEntry{}
		if err = c.applySiteWeb(ctx, site, false, &started); err != nil {
			return err
		}
		if appRunning {
			appStopAttempted = true
			if err = c.stopOwned(ctx, app, false); err != nil {
				return err
			}
		}
		return nil
	}()
	if err != nil {
		return nil, c.siteRollback(err, journal, func(restore context.Context) error {
			failures := []error{}
			if saved {
				failures = append(failures, c.saveSite(restore, site, false))
				if certificate != nil {
					failures = append(failures, c.saveCertRecord(restore, *certificate))
				}
				if !in.Hosts {
					if extraExists {
						failures = append(failures, c.Store.saveSetting(restore, "extraHosts", previousExtras))
					} else {
						_, err := c.Store.db.ExecContext(restore, "DELETE FROM settings WHERE key='extraHosts'")
						failures = append(failures, err)
					}
				}
			}
			if hostsAttempted {
				_, err := c.changeHostsWithSiteLock(restore, json.RawMessage(`{}`), "rebuild", true)
				failures = append(failures, err)
			}
			if appStopAttempted {
				failures = append(failures, c.startService(restore, app))
			}
			failures = append(failures, c.reloadWebIfRunning(restore, site.Runtime.WebServer))
			return errors.Join(failures...)
		})
	}
	c.services.mu.Lock()
	if app == nil || app.child == nil {
		delete(c.services.entries, "site-app:"+site.ID)
	}
	c.services.mu.Unlock()
	return true, nil
}
