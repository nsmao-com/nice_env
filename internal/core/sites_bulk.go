package core

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
)

type siteBulkFailure struct {
	SiteID string    `json:"siteId"`
	Error  *AppError `json:"error"`
}
type siteBulkReport struct {
	Action    string            `json:"action"`
	Succeeded []string          `json:"succeeded"`
	Already   []string          `json:"already"`
	Failed    []siteBulkFailure `json:"failed"`
}

func siteBulkIDs(data json.RawMessage) ([]string, error) {
	in, err := decode[struct {
		IDs []string `json:"ids"`
	}](data)
	if err != nil {
		return nil, err
	}
	if len(in.IDs) > 1000 {
		return nil, invalid("单次最多操作 1000 个站点")
	}
	seen := map[string]bool{}
	out := []string{}
	for _, id := range in.IDs {
		if !seen[id] {
			out = append(out, id)
			seen[id] = true
		}
	}
	return out, nil
}
func (c *Core) sitesStartMany(ctx context.Context, data json.RawMessage) (any, error) {
	ids, err := siteBulkIDs(data)
	if err != nil {
		return nil, err
	}
	report := siteBulkReport{Action: "start", Succeeded: []string{}, Already: []string{}, Failed: []siteBulkFailure{}}
	for _, id := range ids {
		site, err := c.site(ctx, id)
		if err == nil && c.siteStatus(site) == "running" {
			report.Already = append(report.Already, id)
			continue
		}
		if err == nil {
			args, _ := json.Marshal(map[string]string{"id": id})
			_, err = c.siteAction(ctx, args, true)
		}
		if err != nil {
			report.Failed = append(report.Failed, siteBulkFailure{id, publicError(err)})
		} else {
			report.Succeeded = append(report.Succeeded, id)
		}
	}
	return report, nil
}
func (c *Core) sitesStopMany(ctx context.Context, data json.RawMessage) (any, error) {
	ids, err := siteBulkIDs(data)
	if err != nil {
		return nil, err
	}
	unlock, err := c.siteLock()
	if err != nil {
		return nil, err
	}
	defer unlock()
	report := siteBulkReport{Action: "stop", Succeeded: []string{}, Already: []string{}, Failed: []siteBulkFailure{}}
	journal := c.newSiteJournal()
	pending := []Site{}
	servers := map[string]bool{}
	apps := []*serviceEntry{}
	for _, id := range ids {
		site, err := c.site(ctx, id)
		if err != nil {
			report.Failed = append(report.Failed, siteBulkFailure{id, publicError(err)})
			continue
		}
		active := false
		affected := []string{}
		for _, server := range []string{"nginx", "apache", "caddy"} {
			_, readErr := readConfigText(filepath.Join(c.Home, "etc", server, "sites", site.ID+".conf"))
			if readErr == nil {
				active = true
				affected = append(affected, server)
			} else if !os.IsNotExist(readErr) {
				err = readErr
				break
			}
		}
		c.services.mu.RLock()
		app := c.services.entries["site-app:"+id]
		appActive := app != nil && app.child != nil
		c.services.mu.RUnlock()
		if err == nil && !active && !appActive {
			report.Already = append(report.Already, id)
			continue
		}
		length := len(journal.files)
		if err == nil {
			err = c.disableSiteFiles(journal, site)
		}
		if err != nil {
			journal.files = journal.files[:length]
			report.Failed = append(report.Failed, siteBulkFailure{id, publicError(err)})
			continue
		}
		pending = append(pending, site)
		for _, server := range affected {
			servers[server] = true
		}
		if appActive {
			apps = append(apps, app)
		}
	}
	if len(pending) == 0 {
		return report, nil
	}
	stopped := []*serviceEntry{}
	err = func() error {
		if err := journal.apply(ctx); err != nil {
			return err
		}
		for _, server := range []string{"nginx", "apache", "caddy"} {
			if servers[server] {
				if err := c.reloadWebIfRunning(ctx, server); err != nil {
					return err
				}
			}
		}
		for _, app := range apps {
			stopped = append(stopped, app)
			if err := c.stopOwned(ctx, app, false); err != nil {
				return err
			}
		}
		return nil
	}()
	if err != nil {
		err = c.siteRollback(err, journal, func(restore context.Context) error {
			failures := []error{}
			for _, app := range stopped {
				failures = append(failures, c.startService(restore, app))
			}
			for _, server := range []string{"nginx", "apache", "caddy"} {
				if servers[server] {
					failures = append(failures, c.reloadWebIfRunning(restore, server))
				}
			}
			return errors.Join(failures...)
		})
		for _, site := range pending {
			report.Failed = append(report.Failed, siteBulkFailure{site.ID, publicError(err)})
		}
	} else {
		for _, site := range pending {
			report.Succeeded = append(report.Succeeded, site.ID)
		}
	}
	return report, nil
}
