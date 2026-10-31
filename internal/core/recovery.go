package core

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
	"sync"
	"time"
)

type processRecoveryReport struct {
	Adopted         [][2]any `json:"adopted"`
	Killed          [][2]any `json:"killed"`
	Unresolved      []string `json:"unresolved"`
	BlockedServices []string `json:"blockedServices"`
}
type recoveryManager struct {
	mu     sync.RWMutex
	report processRecoveryReport
}

func emptyRecoveryReport() processRecoveryReport {
	return processRecoveryReport{Adopted: [][2]any{}, Killed: [][2]any{}, Unresolved: []string{}, BlockedServices: []string{}}
}

func (c *Core) registerRecovery() {
	c.recovery.report = emptyRecoveryReport()
	c.handle("process_recovery_status", func(context.Context, json.RawMessage) (any, error) {
		c.recovery.mu.RLock()
		defer c.recovery.mu.RUnlock()
		return c.recovery.report, nil
	})
	c.handle("recover_processes", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.recoverProcesses(ctx) })
}

// Windows child trees are attached atomically to a kill-on-close Job Object.
// They cannot survive a crashed owner and therefore need no PID-file adoption.
// Conflicting external listeners are reported, never killed or claimed by name.
func (c *Core) recoverProcesses(ctx context.Context) (processRecoveryReport, error) {
	report := emptyRecoveryReport()
	if !c.services.lifecycle.TryLock() {
		return report, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	listeners, err := c.portListeners(ctx, 1, 65535)
	if err != nil {
		return report, err
	}
	statuses := c.serviceStatuses()
	for _, status := range statuses {
		ports := []int{status.Port}
		c.services.mu.RLock()
		entry := c.services.entries[status.ID]
		if entry != nil && entry.resolved != nil && entry.child != nil {
			ports = append([]int{}, entry.resolved.ports...)
		}
		c.services.mu.RUnlock()
		blocked := false
		for _, port := range ports {
			for _, listener := range listeners {
				if listener.Port != port || listener.ServiceID == status.ID {
					continue
				}
				blocked = true
				report.Unresolved = append(report.Unresolved, fmt.Sprintf("%s :%d 被 PID %d 占用，未接管或结束该进程", status.ID, port, listener.PID))
			}
		}
		if blocked {
			report.BlockedServices = append(report.BlockedServices, status.ID)
		}
	}
	sort.Strings(report.BlockedServices)
	sort.Strings(report.Unresolved)
	c.recovery.mu.Lock()
	c.recovery.report = report
	c.recovery.mu.Unlock()
	return report, nil
}
