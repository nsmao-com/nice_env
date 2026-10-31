package core

import (
	"context"
	"errors"
	"sort"
)

// PrepareQuit runs before the native event loop exits. A failed graceful stop
// keeps the application alive so the user can inspect and resolve the failure.
func (c *Core) PrepareQuit(ctx context.Context) error {
	if !c.services.lifecycle.TryLock() {
		return serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	c.services.mu.Lock()
	c.services.closing = true
	entries := make([]*serviceEntry, 0, len(c.services.entries))
	for _, entry := range c.services.entries {
		if entry.child != nil {
			entries = append(entries, entry)
		}
	}
	c.services.mu.Unlock()
	// Stop incoming web traffic before PHP pools and persistent data services.
	priority := func(e *serviceEntry) int {
		switch e.pkg.ID {
		case "nginx", "apache", "caddy":
			return 0
		case "php":
			return 1
		default:
			return 2
		}
	}
	sort.Slice(entries, func(i, j int) bool {
		if priority(entries[i]) != priority(entries[j]) {
			return priority(entries[i]) < priority(entries[j])
		}
		return entries[i].status.ID < entries[j].status.ID
	})
	var failures []error
	for _, entry := range entries {
		if err := c.stopOwned(ctx, entry, false); err != nil {
			failures = append(failures, &AppError{Code: "QUIT_SERVICE_FAILED", Message: entry.status.Label + " 未正常停止，已取消退出", Detail: err.Error()})
		}
	}
	if err := errors.Join(failures...); err != nil {
		c.services.mu.Lock()
		c.services.closing = false
		c.services.mu.Unlock()
		return err
	}
	return nil
}
