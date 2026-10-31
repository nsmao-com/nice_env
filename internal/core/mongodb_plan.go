package core

import (
	"context"
	"encoding/json"
	"strings"
	"time"
)

func (c *Core) registerMongoBackupPlans() {
	c.handle("mongodb_backup_plan", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[mongoBackupRequest](data)
		if err != nil {
			return nil, err
		}
		return c.readDatabasePlan(ctx, "mongodb", in.Version)
	})
	c.handle("mongodb_backup_plan_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Version string           `json:"version"`
			Config  backupPlanConfig `json:"config"`
		}](data)
		if err != nil {
			return nil, err
		}
		if _, err = validateBackupPlan(in.Config); err != nil {
			return nil, err
		}
		if _, err = c.serviceEntry("mongodb", in.Version); err != nil {
			return nil, err
		}
		unlock, err := c.lockDatabasePlan("mongodb", in.Version)
		if err != nil {
			return nil, err
		}
		defer unlock()
		plan, err := c.readDatabasePlan(ctx, "mongodb", in.Version)
		if err != nil {
			return nil, err
		}
		plan.Config = in.Config
		plan.NextAt = nil
		if in.Config.Enabled {
			next, err := nextBackupRun(in.Config, time.Now())
			if err != nil {
				return nil, err
			}
			plan.NextAt = &next
		}
		return plan, c.saveDatabasePlan(ctx, "mongodb", in.Version, plan)
	})
	c.handle("mongodb_backup_plan_run", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[mongoBackupRequest](data)
		if err != nil {
			return nil, err
		}
		return c.runMongoPlan(ctx, in.Version, true)
	})
}

const mongoAutomaticDatabasesScript = `
  const names = checked(admin.runCommand({listDatabases:1,nameOnly:true,maxTimeMS:5000})).databases.map(d=>d.name);
  print(JSON.stringify({result:names.filter(n=>!['admin','local','config'].includes(n.toLowerCase())).sort()}));
`

func (c *Core) runMongoPlan(ctx context.Context, version string, manual bool) (backupPlan, error) {
	var plan backupPlan
	unlock, err := c.lockDatabasePlan("mongodb", version)
	if err != nil {
		return plan, err
	}
	defer unlock()
	plan, err = c.readDatabasePlan(ctx, "mongodb", version)
	if err != nil {
		return plan, err
	}
	now := time.Now()
	if !manual && (!plan.Config.Enabled || plan.NextAt == nil || *plan.NextAt > now.UnixMilli()) {
		return plan, nil
	}
	if !c.services.lifecycle.TryLock() {
		return plan, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	ctx, cancel := context.WithTimeout(ctx, 30*time.Minute)
	defer cancel()
	plan.NextAt = nil
	if plan.Config.Enabled {
		next, err := nextBackupRun(plan.Config, now)
		if err != nil {
			return plan, err
		}
		plan.NextAt = &next
	}
	at := now.UnixMilli()
	plan.LastRunAt = &at
	plan.FinishedAt = nil
	plan.State = "running"
	plan.Message = ""
	plan.Files = []string{}
	if err = c.saveDatabasePlan(ctx, "mongodb", version, plan); err != nil {
		return plan, err
	}
	workErr := func() error {
		credentials, err := c.mongoCredentials(ctx, version)
		if err != nil {
			return err
		}
		data, err := c.mongoExecute(ctx, version, struct{}{}, mongoAutomaticDatabasesScript, credentials)
		if err != nil {
			return err
		}
		var names []string
		if err = json.Unmarshal(data, &names); err != nil {
			return err
		}
		failures := []string{}
		for _, database := range names {
			item, err := c.createMongoBackup(ctx, version, database, "automatic")
			if err != nil {
				failures = append(failures, database+"："+publicError(err).Message)
				continue
			}
			plan.Files = append(plan.Files, item.ID)
			if err = c.saveDatabasePlan(ctx, "mongodb", version, plan); err != nil {
				return err
			}
			if err = c.rotateMongoBackups(ctx, item, plan.Config.Keep); err != nil {
				failures = append(failures, database+"：旧备份清理未完成，新备份已保留")
			}
		}
		plan.State = "success"
		plan.Message = "数据库备份已完成"
		if len(names) == 0 {
			plan.State = "skipped"
			plan.Message = "没有可备份的业务数据库"
		}
		if len(failures) > 0 {
			plan.State = "partial"
			if len(plan.Files) == 0 {
				plan.State = "failed"
			}
			plan.Message = strings.Join(failures, "；")
		}
		return nil
	}()
	if workErr != nil {
		plan.State = "failed"
		plan.Message = publicError(workErr).Message
	}
	finished := time.Now().UnixMilli()
	plan.FinishedAt = &finished
	saveCtx, saveCancel := context.WithTimeout(context.WithoutCancel(ctx), 5*time.Second)
	defer saveCancel()
	return plan, c.saveDatabasePlan(saveCtx, "mongodb", version, plan)
}

func (c *Core) rotateMongoBackups(ctx context.Context, newest mongoBackup, keep int) error {
	if keep == 0 {
		return nil
	}
	checked, _, err := c.verifyMongoBackup(ctx, newest.ID)
	if err != nil {
		return err
	}
	if checked != newest || newest.Kind != "automatic" {
		return invalid("新自动备份已变化，未清理旧备份")
	}
	listing, err := c.mongoBackupList()
	if err != nil {
		return err
	}
	count := 1
	var problem error
	for _, item := range listing["items"].([]mongoBackup) {
		if item.ID == newest.ID || item.Kind != "automatic" || item.Database != newest.Database || canonicalVersion(item.Version) != canonicalVersion(newest.Version) {
			continue
		}
		verified, dir, err := c.verifyMongoBackup(ctx, item.ID)
		if err != nil || verified != item {
			problem = invalid("旧自动备份异常，已保留")
			continue
		}
		count++
		if count <= keep {
			continue
		}
		revision, err := c.mongoRemovalAt(ctx, item.ID, dir)
		if err != nil {
			return err
		}
		if err = c.removeMongoBackup(ctx, item.ID, revision); err != nil {
			return err
		}
	}
	if listing["unreadable"].(int) > 0 {
		problem = invalid("存在不可读取的备份，已保留")
	}
	return problem
}
