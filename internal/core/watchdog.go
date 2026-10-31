package core

import (
	"context"
	"encoding/json"
	"sort"
	"strconv"
	"time"
)

// Observations use cached owned-child state, never a periodic OS process scan.
// The service lifecycle lock serializes automatic restarts with every manual
// action; serviceManager.mu protects observations shared with process waiters.
type serviceWatch struct {
	enabled           bool
	attempts          int
	next, stableSince time.Time
	wasRunning        bool
	restarts          []int64
}
type watchdogConfig struct {
	Enabled     bool `json:"enabled"`
	MaxAttempts int  `json:"maxAttempts"`
	IntervalSec int  `json:"intervalSec"`
}
type watchedService struct {
	ID            string `json:"id"`
	Enabled       bool   `json:"enabled"`
	Running       bool   `json:"running"`
	Attempts      int    `json:"attempts"`
	Exhausted     bool   `json:"exhausted"`
	RestartCount  int    `json:"restartCount"`
	LastRestartAt *int64 `json:"lastRestartAt"`
}
type watchdogStatus struct {
	watchdogConfig
	Watched []watchedService `json:"watched"`
}

func (w *serviceWatch) started(now time.Time, automatic bool) {
	w.enabled, w.wasRunning = true, true
	w.stableSince, w.next = now, time.Time{}
	if !automatic {
		w.attempts = 0
	}
}
func (w *serviceWatch) observe(running bool, now time.Time) {
	if !w.enabled {
		return
	}
	if running {
		if w.stableSince.IsZero() {
			w.stableSince = now
		}
		if now.Sub(w.stableSince) >= time.Minute {
			w.attempts = 0
			w.next = time.Time{}
		}
	} else {
		if w.wasRunning {
			w.next = now.Add(w.delay())
		}
		w.stableSince = time.Time{}
	}
	w.wasRunning = running
}
func (w *serviceWatch) delay() time.Duration {
	return time.Duration(min(60, 2<<min(w.attempts, 5))) * time.Second
}
func (c *Core) watchdogConfig(ctx context.Context) (watchdogConfig, error) {
	cfg := watchdogConfig{MaxAttempts: 5, IntervalSec: 3}
	rows, err := c.Store.db.QueryContext(ctx, "SELECT key,value FROM settings WHERE key IN ('watchdogEnabled','watchdogMaxAttempts','watchdogIntervalSec')")
	if err != nil {
		return cfg, err
	}
	defer rows.Close()
	for rows.Next() {
		var key, value string
		if err = rows.Scan(&key, &value); err != nil {
			return cfg, err
		}
		switch key {
		case "watchdogEnabled":
			cfg.Enabled = value == "true"
		case "watchdogMaxAttempts":
			if n, e := strconv.Atoi(value); e == nil && n >= 1 && n <= 20 {
				cfg.MaxAttempts = n
			}
		case "watchdogIntervalSec":
			if n, e := strconv.Atoi(value); e == nil && n >= 1 && n <= 60 {
				cfg.IntervalSec = n
			}
		}
	}
	return cfg, rows.Err()
}
func (c *Core) registerWatchdog() {
	c.handle("watchdog_status", func(ctx context.Context, _ json.RawMessage) (any, error) {
		cfg, err := c.watchdogConfig(ctx)
		if err != nil {
			return nil, err
		}
		out := watchdogStatus{watchdogConfig: cfg, Watched: []watchedService{}}
		c.services.mu.Lock()
		defer c.services.mu.Unlock()
		for id, e := range c.services.entries {
			w := &e.watch
			if !w.enabled && len(w.restarts) == 0 && w.stableSince.IsZero() {
				continue
			}
			running := e.status.State == "running" && e.child != nil
			// Starting/stopping is not a crash; only completed operations drive observation.
			if e.status.State != "starting" && e.status.State != "stopping" {
				w.observe(running, time.Now())
			}
			item := watchedService{ID: id, Enabled: w.enabled, Running: running, Attempts: w.attempts, Exhausted: w.enabled && !running && w.attempts >= cfg.MaxAttempts, RestartCount: len(w.restarts)}
			if len(w.restarts) > 0 {
				last := w.restarts[len(w.restarts)-1]
				item.LastRestartAt = &last
			}
			out.Watched = append(out.Watched, item)
		}
		sort.Slice(out.Watched, func(i, j int) bool { return out.Watched[i].ID < out.Watched[j].ID })
		return out, nil
	})
	c.handle("watchdog_set_enabled", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			On *bool `json:"enabled"`
		}](data)
		if err != nil {
			return nil, err
		}
		if in.On == nil {
			return nil, invalid("请选择是否开启自动恢复")
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		return true, c.Store.saveSetting(ctx, "watchdogEnabled", *in.On)
	})
	c.handle("watchdog_reset", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[serviceRequest](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		cfg, err := c.watchdogConfig(ctx)
		if err != nil {
			return nil, err
		}
		if !cfg.Enabled {
			return nil, &AppError{Code: "WATCHDOG_DISABLED", Message: "请先开启服务崩溃后自动重启"}
		}
		e, err := c.serviceEntry(in.ID, "")
		if err != nil {
			return nil, err
		}
		c.services.mu.Lock()
		defer c.services.mu.Unlock()
		if e.child != nil {
			return nil, serviceBusy()
		}
		w := &e.watch
		if !w.enabled || w.attempts < cfg.MaxAttempts {
			return nil, &AppError{Code: "WATCHDOG_NOT_EXHAUSTED", Message: "仅达到恢复上限的服务需要重试；主动停止的服务请手动启动"}
		}
		w.attempts = 0
		w.next = time.Now().Add(2 * time.Second)
		return true, nil
	})
}

func (c *Core) watchdogScheduler(ctx context.Context) {
	defer c.cron.wg.Done()
	timer := time.NewTimer(3 * time.Second)
	defer timer.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-c.closed:
			return
		case <-timer.C:
		}
		cfg, err := c.watchdogConfig(ctx)
		if err == nil && cfg.Enabled {
			if c.beginDataActivity() {
				c.watchdogTick(ctx, cfg)
				c.dataDir.activity.RUnlock()
			}
		}
		timer.Reset(time.Duration(cfg.IntervalSec) * time.Second)
	}
}
func (c *Core) watchdogTick(ctx context.Context, cfg watchdogConfig) {
	if !c.services.lifecycle.TryLock() {
		return
	}
	defer c.services.lifecycle.Unlock()
	// Recheck after acquiring the lifecycle lock: a completed disable action
	// must not be followed by a restart using a stale scheduler configuration.
	current, err := c.watchdogConfig(ctx)
	if err != nil || !current.Enabled {
		return
	}
	cfg = current
	c.services.mu.Lock()
	if c.services.closing {
		c.services.mu.Unlock()
		return
	}
	entries := make([]*serviceEntry, 0, len(c.services.entries))
	for _, e := range c.services.entries {
		running := e.status.State == "running" && e.child != nil
		e.watch.observe(running, time.Now())
		if e.child == nil && e.watch.enabled && e.watch.attempts < cfg.MaxAttempts && !e.watch.next.IsZero() && !time.Now().Before(e.watch.next) {
			entries = append(entries, e)
		}
	}
	c.services.mu.Unlock()
	for _, e := range entries {
		select {
		case <-c.closed:
			return
		case <-ctx.Done():
			return
		default:
		}
		c.services.mu.RLock()
		previousAttempts := e.watch.attempts
		c.services.mu.RUnlock()
		attemptCtx, cancel := context.WithTimeout(ctx, 90*time.Second)
		err := c.startServiceAttempt(attemptCtx, e, true)
		cancel()
		c.services.mu.Lock()
		w := &e.watch
		w.enabled = true
		// Failed startup cleanup may call stopOwned; retain the retry budget even
		// when that helper resets the manual-stop observation.
		w.attempts = previousAttempts + 1
		w.restarts = append(w.restarts, time.Now().Unix())
		if len(w.restarts) > 20 {
			w.restarts = append([]int64{}, w.restarts[len(w.restarts)-20:]...)
		}
		if err != nil {
			w.wasRunning = false
			w.stableSince = time.Time{}
			w.next = time.Now().Add(w.delay())
			c.recordServiceLocked(e, "自动恢复失败："+publicError(err).Message)
		} else {
			c.recordServiceLocked(e, "自动恢复成功")
		}
		c.services.mu.Unlock()
	}
}
