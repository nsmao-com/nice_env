package core

import (
	"context"
	"encoding/json"
	"sort"
	"strings"
)

type bulkTarget struct {
	ID       string  `json:"id"`
	Version  *string `json:"version"`
	Revision *string `json:"revision"`
}
type bulkFailure struct {
	ServiceID string    `json:"serviceId"`
	Error     *AppError `json:"error"`
}
type bulkReport struct {
	Action    string        `json:"action"`
	Succeeded []string      `json:"succeeded"`
	Already   []string      `json:"already"`
	Failed    []bulkFailure `json:"failed"`
	Order     []string      `json:"order"`
}

func serviceTier(id string) int {
	base, _, _ := strings.Cut(id, "@")
	switch base {
	case "mysql", "mariadb", "postgresql", "mongodb", "qdrant", "neo4j", "redis", "memcached", "rabbitmq", "elasticsearch", "meilisearch", "zincsearch", "minio", "rustfs", "consul", "etcd", "r-nacos", "temporal":
		return 0
	case "php", "node", "python", "java", "go", "dotnet", "bun", "deno", "ruby", "rust", "zig", "flutter", "perl", "erlang", "ollama":
		return 1
	case "nginx", "apache", "caddy", "frankenphp", "tomcat", "roadrunner", "mihomo":
		return 2
	default:
		return 3
	}
}
func (c *Core) registerBulk() {
	c.handle("bulk_start", func(ctx context.Context, data json.RawMessage) (any, error) { return c.bulkAction(ctx, data, "start") })
	c.handle("bulk_stop", func(ctx context.Context, data json.RawMessage) (any, error) { return c.bulkAction(ctx, data, "stop") })
	c.handle("bulk_restart", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.bulkAction(ctx, data, "restart")
	})
	c.handle("tray_stop_all", func(ctx context.Context, data json.RawMessage) (any, error) { return c.bulkAction(ctx, data, "stop") })
	c.handle("bulk_summary", func(_ context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			IDs []string `json:"ids"`
		}](data)
		if err != nil {
			return nil, err
		}
		if len(in.IDs) > 1024 {
			return nil, invalid("批量目标过多")
		}
		seen := map[string]bool{}
		running := 0
		for _, id := range in.IDs {
			if seen[id] {
				continue
			}
			seen[id] = true
			e, err := c.serviceEntry(id, "")
			if err != nil {
				return nil, err
			}
			c.services.mu.RLock()
			busy := e.child != nil || e.status.State == "starting" || e.status.State == "stopping"
			c.services.mu.RUnlock()
			if busy {
				running++
			}
		}
		return map[string]any{"total": len(seen), "running": running, "stopped": len(seen) - running, "canStop": running > 0, "canStart": len(seen) > running}, nil
	})
}
func (c *Core) bulkAction(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[struct {
		Targets []bulkTarget `json:"targets"`
	}](data)
	if err != nil {
		return nil, err
	}
	if len(in.Targets) == 0 || len(in.Targets) > 1024 {
		return nil, invalid("请选择 1–1024 个服务")
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	entries := map[string]*serviceEntry{}
	order := []string{}
	// Validate every selection before mutating any service in the batch.
	for _, target := range in.Targets {
		if target.ID == "" || entries[target.ID] != nil || target.Version == nil || *target.Version == "" {
			return nil, invalid("批量目标无效、重复或缺少版本")
		}
		e, err := c.serviceEntry(target.ID, *target.Version)
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		state, rev := e.status.State, e.revision
		c.services.mu.RUnlock()
		if state == "starting" || state == "stopping" {
			return nil, serviceBusy()
		}
		if state == "unknown" {
			return nil, invalid("无法确认服务状态")
		}
		if target.Revision != nil && *target.Revision != rev {
			return nil, &AppError{Code: "SERVICE_TARGET_CHANGED", Message: "服务已重新启动，请重新确认"}
		}
		entries[target.ID] = e
		order = append(order, target.ID)
	}
	sort.SliceStable(order, func(i, j int) bool {
		if action == "stop" {
			return serviceTier(order[i]) > serviceTier(order[j])
		}
		return serviceTier(order[i]) < serviceTier(order[j])
	})
	report := bulkReport{Action: action, Succeeded: []string{}, Already: []string{}, Failed: []bulkFailure{}, Order: order}
	failed := map[string]bool{}
	if action == "restart" {
		stopOrder := append([]string{}, order...)
		sort.SliceStable(stopOrder, func(i, j int) bool { return serviceTier(stopOrder[i]) > serviceTier(stopOrder[j]) })
		for _, id := range stopOrder {
			if err := c.stopOwned(ctx, entries[id], false); err != nil {
				report.Failed = append(report.Failed, bulkFailure{id, publicError(err)})
				failed[id] = true
			}
		}
	}
	for _, id := range order {
		if failed[id] {
			continue
		}
		e := entries[id]
		c.services.mu.RLock()
		running, busy := e.status.State == "running", e.child != nil
		c.services.mu.RUnlock()
		already := action == "start" && running || action == "stop" && !busy
		var err error
		if action == "stop" {
			err = c.stopOwned(ctx, e, false)
		} else {
			err = c.startService(ctx, e)
		}
		if err != nil {
			report.Failed = append(report.Failed, bulkFailure{id, publicError(err)})
		} else if already {
			report.Already = append(report.Already, id)
		} else {
			report.Succeeded = append(report.Succeeded, id)
		}
	}
	return report, nil
}
