package core

import (
	"context"
	"encoding/json"
	"sync"
	"time"
)

type startupStackStatus struct {
	Phase     string       `json:"phase"`
	StackID   *string      `json:"stackId"`
	StackName *string      `json:"stackName"`
	Report    *stackReport `json:"report"`
	Error     *AppError    `json:"error"`
}
type startupManager struct {
	once   sync.Once
	mu     sync.RWMutex
	status startupStackStatus
}

func (c *Core) registerStartup() {
	c.startup.status = startupStackStatus{Phase: "waiting"}
	c.handle("startup_stack_status", func(context.Context, json.RawMessage) (any, error) {
		c.startup.mu.RLock()
		defer c.startup.mu.RUnlock()
		return c.startup.status, nil
	})
}

// Starting a stack is deferred until the main UI is ready. This method only
// schedules work; page refresh and repeated readiness events cannot start twice.
func (c *Core) StartLaunch(parent context.Context) {
	c.startup.once.Do(func() {
		c.cron.mu.Lock()
		if c.cron.closing {
			c.cron.mu.Unlock()
			return
		}
		c.cron.wg.Add(1)
		c.cron.mu.Unlock()
		go func() {
			defer c.cron.wg.Done()
			if !c.beginDataActivity() {
				return
			}
			defer c.dataDir.activity.RUnlock()
			ctx, cancel := context.WithTimeout(parent, 10*time.Minute)
			defer cancel()
			go func() {
				select {
				case <-c.closed:
					cancel()
				case <-ctx.Done():
				}
			}()
			status := startupStackStatus{Phase: "disabled"}
			publish := func() {
				c.startup.mu.Lock()
				c.startup.status = status
				c.startup.mu.Unlock()
				if c.emit != nil {
					c.emit("startup-stack://status", status)
				}
			}
			defer publish()
			if _, err := c.recoverProcesses(ctx); err != nil {
				status.Phase = "failed"
				status.Error = publicError(err)
				return
			}
			id, _, err := c.Store.setting(ctx, "startStackOnLaunch")
			if err != nil {
				status.Phase = "failed"
				status.Error = publicError(err)
				return
			}
			if id == "" {
				return
			}
			status.StackID = &id
			stack, err := scanStack(c.Store.db.QueryRowContext(ctx, stackSelect+" WHERE id=?", id))
			if err != nil {
				status.Phase = "failed"
				status.Error = publicError(stackError(err))
				return
			}
			status.StackName = &stack.Name
			status.Phase = "running"
			publish()
			report, err := c.executeStack(ctx, id, "start", nil)
			if err != nil {
				status.Phase = "failed"
				status.Error = publicError(err)
				return
			}
			status.Report = &report
			status.Phase = "complete"
			if len(report.Failed) > 0 || len(report.Skipped) > 0 {
				status.Phase = "partial"
				if len(report.Started) == 0 && len(report.AlreadyRunning) == 0 {
					status.Phase = "failed"
				}
			}
		}()
	})
}
