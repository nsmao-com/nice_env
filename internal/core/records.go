package core

import (
	"context"
	"encoding/json"
)

func (c *Core) registerRecords() {
	c.handle("list_sites", func(ctx context.Context, _ json.RawMessage) (any, error) {
		rows, err := c.sites(ctx)
		if err != nil {
			return nil, err
		}
		for i := range rows {
			rows[i] = sanitizedSite(rows[i])
			rows[i].Status = c.siteStatus(rows[i])
		}
		return rows, nil
	})
}
