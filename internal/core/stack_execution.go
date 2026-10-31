package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"slices"
	"sort"
	"strings"
)

type stackTarget struct {
	ID      string `json:"id"`
	Version string `json:"version"`
	Port    int    `json:"port"`
}
type stackRetry struct {
	Revision   string   `json:"revision"`
	ServiceIDs []string `json:"serviceIds"`
}
type stackFailure struct {
	ServiceID string    `json:"serviceId"`
	Error     *AppError `json:"error"`
}
type stackReport struct {
	StackID        string         `json:"stackId"`
	Revision       string         `json:"revision"`
	Order          []string       `json:"order"`
	Started        []string       `json:"started"`
	AlreadyRunning []string       `json:"alreadyRunning"`
	Skipped        []string       `json:"skipped"`
	Failed         []stackFailure `json:"failed"`
}

func (c *Core) registerStackExecution() {
	c.handle("start_stack", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		return c.executeStack(ctx, in.ID, "start", nil)
	})
	c.handle("stop_stack", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		return c.executeStack(ctx, in.ID, "stop", nil)
	})
	c.handle("retry_stack", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID     string     `json:"id"`
			Action string     `json:"action"`
			Retry  stackRetry `json:"retry"`
		}](data)
		if err != nil {
			return nil, err
		}
		if in.Action != "start" && in.Action != "stop" {
			return nil, invalid("服务栈操作无效")
		}
		return c.executeStack(ctx, in.ID, in.Action, &in.Retry)
	})
}

func (c *Core) resolveStack(ctx context.Context, s Stack) ([]stackTarget, []string, error) {
	views, err := c.packageViews(ctx)
	if err != nil {
		return nil, nil, err
	}
	active := map[string]string{}
	installed := map[string]bool{}
	for _, view := range views {
		if view["install"] != nil {
			id, _ := view["id"].(string)
			version, _ := view["version"].(string)
			installed[id+"@"+canonicalVersion(version)] = true
			if view["active"] == true {
				active[id] = version
			}
		}
	}
	known := map[string]ServiceStatus{}
	for _, status := range c.serviceStatuses() {
		known[status.ID] = status
	}
	items := append([]StackItem{}, s.Items...)
	sort.SliceStable(items, func(i, j int) bool { return items[i].Order < items[j].Order })
	result := []stackTarget{}
	skipped := []string{}
	seen := map[string]string{}
	for _, item := range items {
		wanted := item.ServiceID
		sid, version := "", ""
		if exact, ok := known[wanted]; ok {
			sid, version = exact.ID, exact.Version
		} else {
			base, fixed, explicit := strings.Cut(wanted, "@")
			if explicit {
				if installed[base+"@"+canonicalVersion(fixed)] {
					if _, ok := known[base]; ok {
						sid, version = base, fixed
					} else {
						for id, status := range known {
							if strings.HasPrefix(id, base+"@") && canonicalVersion(status.Version) == canonicalVersion(fixed) {
								sid, version = id, fixed
								break
							}
						}
					}
				}
			} else if chosen := active[base]; chosen != "" {
				for id, status := range known {
					if strings.HasPrefix(id, base+"@") && canonicalVersion(status.Version) == canonicalVersion(chosen) {
						sid, version = id, chosen
						break
					}
				}
			}
		}
		if sid == "" {
			skipped = append(skipped, wanted)
			continue
		}
		if previous, exists := seen[sid]; exists {
			if canonicalVersion(previous) != canonicalVersion(version) {
				return nil, nil, &AppError{Code: "STACK_VERSION_CONFLICT", Message: "服务栈为同一服务选择了不同版本"}
			}
			continue
		}
		seen[sid] = version
		result = append(result, stackTarget{ID: sid, Version: version, Port: known[sid].Port})
	}
	return result, skipped, nil
}

func (c *Core) executeStack(ctx context.Context, id, action string, retry *stackRetry) (stackReport, error) {
	var empty stackReport
	if !c.services.lifecycle.TryLock() {
		return empty, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	s, err := scanStack(c.Store.db.QueryRowContext(ctx, stackSelect+" WHERE id=?", id))
	if err != nil {
		return empty, stackError(err)
	}
	targets, skipped, err := c.resolveStack(ctx, s)
	if err != nil {
		return empty, err
	}
	signature, err := json.Marshal(struct {
		Action  string
		Stack   Stack
		Targets []stackTarget
		Skipped []string
	}{action, s, targets, skipped})
	if err != nil {
		return empty, err
	}
	hash := sha256.Sum256(signature)
	revision := hex.EncodeToString(hash[:])
	if retry != nil {
		if retry.Revision != revision {
			return empty, &AppError{Code: "STACK_TARGET_CHANGED", Message: "服务栈配置、版本或端口已变化，请重新执行"}
		}
		selected := map[string]bool{}
		for _, id := range retry.ServiceIDs {
			if selected[id] || !slices.ContainsFunc(targets, func(target stackTarget) bool { return target.ID == id }) {
				return empty, invalid("重试目标重复或不属于原执行计划")
			}
			selected[id] = true
		}
		if len(selected) == 0 {
			return empty, invalid("请选择需要重试的服务")
		}
		targets = slices.DeleteFunc(targets, func(target stackTarget) bool { return !selected[target.ID] })
	}
	if action == "stop" {
		slices.Reverse(targets)
	}
	report := stackReport{StackID: id, Revision: revision, Order: []string{}, Started: []string{}, AlreadyRunning: []string{}, Skipped: skipped, Failed: []stackFailure{}}
	for _, target := range targets {
		report.Order = append(report.Order, target.ID)
		if err = ctx.Err(); err != nil {
			report.Failed = append(report.Failed, stackFailure{ServiceID: target.ID, Error: publicError(err)})
			continue
		}
		e, actionErr := c.serviceEntry(target.ID, target.Version)
		if actionErr == nil {
			c.services.mu.RLock()
			running := e.child != nil
			state := e.status.State
			c.services.mu.RUnlock()
			if action == "start" && running && state == "running" {
				report.AlreadyRunning = append(report.AlreadyRunning, target.ID)
				continue
			}
			if action == "stop" && !running {
				report.Skipped = append(report.Skipped, target.ID)
				continue
			}
			if action == "start" {
				actionErr = c.startService(ctx, e)
			} else {
				actionErr = c.stopOwned(ctx, e, false)
			}
		}
		if actionErr != nil {
			report.Failed = append(report.Failed, stackFailure{ServiceID: target.ID, Error: publicError(actionErr)})
		} else {
			report.Started = append(report.Started, target.ID)
		}
	}
	return report, nil
}
