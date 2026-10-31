package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"niceenv/internal/process"
)

type siteFilePlan struct {
	Status           backupPlan       `json:"status"`
	Project          bool             `json:"project"`
	ExcludeGenerated bool             `json:"excludeGenerated"`
	Scope            *siteBackupScope `json:"scope"`
	Revision         string           `json:"revision"`
}

func siteFilePlanKey(id string) string {
	digest := sha256.Sum256([]byte(id))
	return "siteFileBackupPlan@" + hex.EncodeToString(digest[:])
}
func siteFilePlanRevision(plan siteFilePlan) string {
	return siteRevision("site-file-plan", "config", []any{plan.Status.Config, plan.Project, plan.ExcludeGenerated, plan.Scope})
}
func (c *Core) readSiteFilePlan(ctx context.Context, id string) (siteFilePlan, error) {
	plan := siteFilePlan{Status: backupPlan{Config: backupPlanConfig{Frequency: "daily", Time: "03:00", MonthDay: 1, Keep: 10}, State: "idle", Files: []string{}}, Project: true, ExcludeGenerated: true}
	text, exists, err := c.Store.setting(ctx, siteFilePlanKey(id))
	if err != nil {
		return plan, err
	}
	if exists {
		if err = json.Unmarshal([]byte(text), &plan); err != nil {
			return plan, invalid("站点备份计划损坏，请检查配置备份")
		}
	}
	if _, err = validateBackupPlan(plan.Status.Config); err != nil {
		return plan, err
	}
	plan.Revision = siteFilePlanRevision(plan)
	if plan.Status.Files == nil {
		plan.Status.Files = []string{}
	}
	return plan, nil
}
func (c *Core) writeSiteFilePlan(ctx context.Context, id string, plan *siteFilePlan) error {
	plan.Revision = siteFilePlanRevision(*plan)
	value, err := json.Marshal(plan)
	if err != nil {
		return err
	}
	return c.Store.saveSetting(ctx, siteFilePlanKey(id), string(value))
}
func (c *Core) registerSiteBackupPlans() {
	c.handle("site_files_plan", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[serviceRequest](data)
		if err != nil {
			return nil, err
		}
		if _, err = c.site(ctx, in.ID); err != nil {
			return nil, err
		}
		plan, err := c.readSiteFilePlan(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		if plan.Status.State == "running" {
			unlock, e := c.siteBackupLock(in.ID)
			if e == nil {
				defer unlock()
				plan, err = c.readSiteFilePlan(ctx, in.ID)
				if err != nil {
					return nil, err
				}
				if plan.Status.State == "running" {
					plan.Status.State = "interrupted"
					plan.Status.Message = "上次备份中断，已生成归档仍保留，请检查后重新执行"
				}
			} else {
				var app *AppError
				if !errors.As(e, &app) || app.Code != "SITE_BACKUP_BUSY" {
					return nil, e
				}
			}
		}
		return plan, nil
	})
	c.handle("site_files_plan_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID               string           `json:"id"`
			Config           backupPlanConfig `json:"config"`
			Project          bool             `json:"project"`
			ExcludeGenerated bool             `json:"excludeGenerated"`
			ExpectedRevision string           `json:"expectedRevision"`
			ScopeRevision    *string          `json:"scopeRevision"`
			Confirmed        bool             `json:"confirmed"`
		}](data)
		if err != nil {
			return nil, err
		}
		if _, err = validateBackupPlan(in.Config); err != nil {
			return nil, err
		}
		if _, err = c.site(ctx, in.ID); err != nil {
			return nil, err
		}
		unlock, err := c.siteBackupLock(in.ID)
		if err != nil {
			return nil, err
		}
		defer unlock()
		plan, err := c.readSiteFilePlan(ctx, in.ID)
		if err != nil {
			return nil, err
		}
		if plan.Revision != in.ExpectedRevision {
			return nil, &AppError{Code: "SITE_PLAN_CHANGED", Message: "备份计划已变化，请重新读取"}
		}
		if in.Config.Enabled {
			if !in.Confirmed {
				return nil, siteArchiveInvalid("请确认自动备份范围、敏感文件与历史保留策略")
			}
			scope, err := c.siteBackupScope(ctx, in.ID, in.Project, in.ExcludeGenerated)
			if err != nil {
				return nil, err
			}
			if in.ScopeRevision == nil || scope.Revision != *in.ScopeRevision {
				return nil, siteArchiveChanged()
			}
			plan.Scope = &scope
			plan.Project = in.Project
			plan.ExcludeGenerated = in.ExcludeGenerated
		}
		plan.Status.Config = in.Config
		plan.Status.NextAt = nil
		if in.Config.Enabled {
			at, err := nextBackupRun(in.Config, time.Now())
			if err != nil {
				return nil, err
			}
			plan.Status.NextAt = &at
		}
		if plan.Status.State == "running" || plan.Status.State == "needs-review" {
			plan.Status.State = "idle"
			plan.Status.Message = ""
		}
		return plan, c.writeSiteFilePlan(ctx, in.ID, &plan)
	})
	c.handle("site_files_plan_run", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[siteBackupRequest](data)
		if err != nil {
			return nil, err
		}
		if len(in.OperationID) > 200 {
			return nil, invalid("操作标识过长")
		}
		return c.runSiteFilePlan(ctx, in.ID, in.OperationID, true)
	})
}
func (c *Core) rotateSiteArchives(ctx context.Context, id, newest string, keep int) error {
	if keep == 0 {
		return nil
	}
	list, err := c.listSiteArchives(ctx, id)
	if err != nil {
		return err
	}
	dir, err := c.siteBackupDirectory(id)
	if err != nil {
		return err
	}
	root, err := siteBackupRoot(dir)
	if err != nil {
		return err
	}
	defer root.Close()
	count := 1
	for _, archive := range list {
		if !archive.Restorable || !archive.Automatic || !strings.HasPrefix(archive.Name, "site-auto-") || archive.Name == newest {
			continue
		}
		count++
		if count <= keep {
			continue
		}
		f, err := siteBackupOpen(root, archive.Name)
		if err != nil {
			return err
		}
		f.Close()
		if err = root.Remove(archive.Name); err != nil {
			return err
		}
	}
	return nil
}
func (c *Core) runSiteFilePlan(ctx context.Context, id, operation string, manual bool) (siteFilePlan, error) {
	var plan siteFilePlan
	if _, err := c.site(ctx, id); err != nil {
		return plan, err
	}
	unlock, err := c.siteBackupLock(id)
	if err != nil {
		return plan, err
	}
	defer unlock()
	plan, err = c.readSiteFilePlan(ctx, id)
	if err != nil {
		return plan, err
	}
	now := time.Now().UnixMilli()
	if !manual && (!plan.Status.Config.Enabled || plan.Status.NextAt == nil || *plan.Status.NextAt > now) {
		return plan, nil
	}
	plan.Status.LastRunAt = &now
	plan.Status.FinishedAt = nil
	plan.Status.State = "running"
	plan.Status.Message = ""
	plan.Status.Files = []string{}
	plan.Status.NextAt = nil
	if plan.Status.Config.Enabled {
		at, err := nextBackupRun(plan.Status.Config, time.Now())
		if err != nil {
			return plan, err
		}
		plan.Status.NextAt = &at
	}
	if err = c.writeSiteFilePlan(ctx, id, &plan); err != nil {
		return plan, err
	}
	scopeChanged := false
	err = func() error {
		if plan.Scope == nil {
			scopeChanged = true
			return siteArchiveInvalid("请先设置并确认自动备份范围")
		}
		scope, err := c.siteBackupScope(ctx, id, plan.Project, plan.ExcludeGenerated)
		if err != nil {
			return err
		}
		if scope.Revision != plan.Scope.Revision {
			scopeChanged = true
			return siteArchiveChanged()
		}
		archive, err := c.createSiteArchive(ctx, siteBackupRequest{ID: id, Project: plan.Project, ExcludeGenerated: plan.ExcludeGenerated, Revision: scope.Revision}, true, &siteBackupProgress{c: c, id: id, operation: operation})
		if err != nil {
			return err
		}
		plan.Status.Files = append(plan.Status.Files, archive.Name)
		if err = c.writeSiteFilePlan(ctx, id, &plan); err != nil {
			return err
		}
		if err = c.rotateSiteArchives(ctx, id, archive.Name, plan.Status.Config.Keep); err != nil {
			plan.Status.State = "partial"
			plan.Status.Message = "新归档已保留，清理旧自动备份失败：" + publicError(err).Message
		} else {
			plan.Status.State = "success"
			plan.Status.Message = fmt.Sprintf("已备份 %d 个文件，共 %d 字节", archive.Files, archive.OriginalBytes)
		}
		return nil
	}()
	if err != nil {
		if plan.Scope != nil {
			current, e := c.siteBackupScope(ctx, id, plan.Project, plan.ExcludeGenerated)
			scopeChanged = scopeChanged || e == nil && current.Revision != plan.Scope.Revision
		}
		plan.Status.State = "failed"
		if len(plan.Status.Files) > 0 {
			plan.Status.State = "partial"
		}
		if scopeChanged {
			plan.Status.State = "needs-review"
			plan.Status.Config.Enabled = false
			plan.Status.NextAt = nil
		}
		plan.Status.Message = publicError(err).Message
	}
	finished := time.Now().UnixMilli()
	plan.Status.FinishedAt = &finished
	if plan.Status.Config.Enabled {
		at, e := nextBackupRun(plan.Status.Config, time.Now())
		if e != nil {
			return plan, e
		}
		plan.Status.NextAt = &at
	}
	saveCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err = c.writeSiteFilePlan(saveCtx, id, &plan); err != nil {
		return plan, err
	}
	if c.emit != nil {
		c.emit("site-backup://status", map[string]any{"siteId": id, "state": plan.Status.State, "message": plan.Status.Message})
	}
	return plan, nil
}
func (c *Core) runDueSiteFilePlans(ctx context.Context) {
	sites, err := c.sites(ctx)
	if err != nil {
		return
	}
	for _, site := range sites {
		if ctx.Err() != nil {
			return
		}
		plan, err := c.readSiteFilePlan(ctx, site.ID)
		if err != nil || !plan.Status.Config.Enabled || plan.Status.NextAt == nil || *plan.Status.NextAt > time.Now().UnixMilli() {
			continue
		}
		_, err = c.runSiteFilePlan(ctx, site.ID, "", false)
		if err != nil && c.emit != nil {
			var app *AppError
			if errors.As(err, &app) && app.Code == "SITE_BACKUP_BUSY" || errors.Is(err, process.ErrLocked) {
				continue
			}
			c.emit("site-backup://status", map[string]any{"siteId": site.ID, "state": "failed", "message": publicError(err).Message})
		}
	}
}
