package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"time"
)

func mysqlAutomaticID(database string) string {
	hash := sha256.Sum256([]byte(database))
	return hex.EncodeToString(hash[:])
}
func mysqlAutomaticMarker(engine, version, database string) string {
	return "\n-- NiceEnv automatic backup v1 " + engine + " " + version + " " + mysqlAutomaticID(database) + "\n"
}
func (c *Core) registerMySQLBackupPlans() {
	c.handle("db_backup_plan", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlPlanCommand(ctx, data, "read")
	})
	c.handle("db_backup_plan_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlPlanCommand(ctx, data, "save")
	})
	c.handle("db_backup_plan_run", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlPlanCommand(ctx, data, "run")
	})
}
func (c *Core) mysqlPlanCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[struct {
		Engine  string           `json:"engine"`
		Version string           `json:"version"`
		Config  backupPlanConfig `json:"config"`
	}](data)
	if err != nil {
		return nil, err
	}
	if in.Engine == "" {
		in.Engine = "mysql"
	}
	if in.Engine != "mysql" && in.Engine != "mariadb" {
		return nil, invalid("请选择 MySQL 或 MariaDB 实例")
	}
	if action == "read" {
		return c.readDatabasePlan(ctx, in.Engine, in.Version)
	}
	if action == "run" {
		return c.runMySQLPlan(ctx, in.Engine, in.Version, true)
	}
	if _, err = validateBackupPlan(in.Config); err != nil {
		return nil, err
	}
	if _, err = c.selectedMySQL(ctx, in.Engine, in.Version); err != nil {
		return nil, err
	}
	unlock, err := c.lockDatabasePlan(in.Engine, in.Version)
	if err != nil {
		return nil, err
	}
	defer unlock()
	plan, err := c.readDatabasePlan(ctx, in.Engine, in.Version)
	if err != nil {
		return nil, err
	}
	if plan.State == "running" {
		plan.State = "interrupted"
	}
	plan.Config = in.Config
	plan.NextAt = nil
	if plan.Config.Enabled {
		next, err := nextBackupRun(plan.Config, time.Now())
		if err != nil {
			return nil, err
		}
		plan.NextAt = &next
	}
	return plan, c.saveDatabasePlan(ctx, in.Engine, in.Version, plan)
}
func (c *Core) runMySQLPlan(ctx context.Context, engine, version string, manual bool) (backupPlan, error) {
	var empty backupPlan
	unlock, err := c.lockDatabasePlan(engine, version)
	if err != nil {
		return empty, err
	}
	defer unlock()
	plan, err := c.readDatabasePlan(ctx, engine, version)
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
	c.services.mu.RLock()
	closing := c.services.closing
	c.services.mu.RUnlock()
	if closing {
		return plan, invalid("应用正在退出，未开始备份")
	}
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
	if err = c.saveDatabasePlan(ctx, engine, version, plan); err != nil {
		return plan, err
	}
	workErr := func() error {
		e, err := c.selectedMySQL(ctx, engine, version)
		if err != nil {
			return err
		}
		password, found, err := c.Store.setting(ctx, mysqlPasswordKey(e))
		if err != nil {
			return err
		}
		if !found {
			return invalid("请先保存有效 root 凭据")
		}
		db, err := c.mysqlConnection(ctx, e, password)
		if err != nil {
			return err
		}
		defer db.Close()
		databases, err := mysqlStrings(ctx, db, "SHOW DATABASES")
		if err != nil {
			return err
		}
		failures := []string{}
		total := 0
		for _, name := range databases {
			if protectedDatabase(name) {
				continue
			}
			total++
			id, err := newID("archive")
			if err != nil {
				return err
			}
			outName := "auto-" + mysqlAutomaticID(name) + "-" + id + ".sql"
			path, err := c.mysqlDump(ctx, e, db, password, []string{name}, outName, false, true)
			if err != nil {
				failures = append(failures, name+"："+err.Error())
				continue
			}
			plan.Files = append(plan.Files, filepath.Base(path))
			if err = c.saveDatabasePlan(ctx, engine, version, plan); err != nil {
				return err
			}
			if err = c.rotateMySQLBackups(engine, version, name, plan.Config.Keep, path); err != nil {
				failures = append(failures, name+"：旧备份清理失败，新备份已保留")
			}
		}
		plan.State = "success"
		plan.Message = "数据库备份已完成"
		if total == 0 {
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
		plan.Message = workErr.Error()
	}
	finished := time.Now().UnixMilli()
	plan.FinishedAt = &finished
	saveCtx, saveCancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer saveCancel()
	return plan, c.saveDatabasePlan(saveCtx, engine, version, plan)
}
func (c *Core) rotateMySQLBackups(engine, version, database string, keep int, newest string) error {
	if keep == 0 {
		return nil
	}
	files, err := c.mysqlBackupList()
	if err != nil {
		return err
	}
	prefix := engine + "-" + version + "-auto-" + mysqlAutomaticID(database) + "-archive-"
	marker := mysqlAutomaticMarker(engine, version, database)
	count := 1
	for _, file := range files {
		if string(file.Path) == newest || !strings.HasPrefix(file.Name, prefix) || file.SizeBytes <= int64(len(marker)) {
			continue
		}
		f, err := os.Open(string(file.Path))
		if err != nil {
			return err
		}
		tail := make([]byte, len(marker))
		_, readErr := f.ReadAt(tail, file.SizeBytes-int64(len(marker)))
		f.Close()
		if readErr != nil || string(tail) != marker {
			continue
		}
		count++
		if count > keep {
			if err = c.mysqlBackupDelete(string(file.Path)); err != nil {
				return err
			}
		}
	}
	return nil
}
