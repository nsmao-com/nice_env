package core

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"niceenv/internal/process"
)

type backupPlanConfig struct {
	Enabled   bool   `json:"enabled"`
	Frequency string `json:"frequency"`
	Time      string `json:"time"`
	Weekday   int    `json:"weekday"`
	MonthDay  int    `json:"monthDay"`
	Keep      int    `json:"keep"`
}
type backupPlan struct {
	Config     backupPlanConfig `json:"config"`
	NextAt     *int64           `json:"nextAt"`
	LastRunAt  *int64           `json:"lastRunAt"`
	FinishedAt *int64           `json:"finishedAt"`
	State      string           `json:"state"`
	Message    string           `json:"message"`
	Files      []string         `json:"files"`
}
type backupPlanManager struct {
	mu      sync.Mutex
	running map[string]bool
}

func validateBackupPlan(config backupPlanConfig) (time.Time, error) {
	clock, err := time.Parse("15:04", config.Time)
	if err != nil || len(config.Time) != 5 || (config.Frequency != "daily" && config.Frequency != "weekly" && config.Frequency != "monthly") || config.Weekday < 0 || config.Weekday > 6 || config.MonthDay < 1 || config.MonthDay > 31 || config.Keep < 0 || config.Keep > 100 {
		return time.Time{}, invalid("请选择有效的备份周期、时间及 0–100 份保留数量")
	}
	return clock, nil
}
func nextBackupRun(config backupPlanConfig, after time.Time) (int64, error) {
	clock, err := validateBackupPlan(config)
	if err != nil {
		return 0, err
	}
	start := time.Date(after.Year(), after.Month(), after.Day(), 0, 0, 0, 0, after.Location())
	for day := 0; day <= 370; day++ {
		date := start.AddDate(0, 0, day)
		if config.Frequency == "weekly" && (int(date.Weekday())+6)%7 != config.Weekday {
			continue
		}
		last := time.Date(date.Year(), date.Month()+1, 0, 12, 0, 0, 0, date.Location()).Day()
		if config.Frequency == "monthly" && date.Day() != min(config.MonthDay, last) {
			continue
		}
		// Iterate real local minutes so spring gaps advance to the first valid
		// minute and repeated fall-back times use only their first occurrence.
		end := date.AddDate(0, 0, 1)
		for at := date; at.Before(end); at = at.Add(time.Minute) {
			if at.Hour()*60+at.Minute() < clock.Hour()*60+clock.Minute() {
				continue
			}
			if at.After(after) {
				return at.UnixMilli(), nil
			}
			break
		}
	}
	return 0, invalid("无法计算下一次备份时间，请检查系统时区")
}
func backupPlanKey(version string) (string, error) {
	return databasePlanKey("postgresql", version)
}
func databasePlanKey(engine, version string) (string, error) {
	if !safePart(version) || len(version) > 64 {
		return "", invalid("数据库版本无效")
	}
	if engine == "postgresql" {
		return "postgresBackupPlan@" + version, nil
	}
	if engine == "mysql" || engine == "mariadb" {
		return engine + "BackupPlan@" + version, nil
	}
	return "", invalid("备份计划数据库类型无效")
}
func (c *Core) readPostgresPlan(ctx context.Context, version string) (backupPlan, error) {
	return c.readDatabasePlan(ctx, "postgresql", version)
}
func (c *Core) readDatabasePlan(ctx context.Context, engine, version string) (backupPlan, error) {
	plan := backupPlan{Config: backupPlanConfig{Frequency: "daily", Time: "03:00", MonthDay: 1, Keep: 10}, State: "idle", Files: []string{}}
	key, err := databasePlanKey(engine, version)
	if err != nil {
		return plan, err
	}
	data, exists, err := c.Store.setting(ctx, key)
	if err != nil {
		return plan, err
	}
	if exists {
		if err = json.Unmarshal([]byte(data), &plan); err != nil {
			return plan, invalid("备份计划记录无法读取，请检查配置备份")
		}
	}
	if _, err = validateBackupPlan(plan.Config); err != nil {
		return plan, err
	}
	c.backupPlans.mu.Lock()
	running := c.backupPlans.running[key]
	c.backupPlans.mu.Unlock()
	if plan.State == "running" && !running {
		path, err := c.backupPlanLockPath(engine, version)
		if err != nil {
			return plan, err
		}
		unlock, err := process.Lock(path)
		if err == nil {
			if err = unlock(); err != nil {
				return plan, err
			}
			plan.State = "interrupted"
			plan.Message = "上次备份未完成，已生成归档仍保留，请核对后重新执行"
		} else if !errors.Is(err, process.ErrLocked) {
			return plan, err
		}
	}
	return plan, nil
}
func (c *Core) savePostgresPlan(ctx context.Context, version string, plan backupPlan) error {
	return c.saveDatabasePlan(ctx, "postgresql", version, plan)
}
func (c *Core) saveDatabasePlan(ctx context.Context, engine, version string, plan backupPlan) error {
	key, err := databasePlanKey(engine, version)
	if err != nil {
		return err
	}
	data, err := json.Marshal(plan)
	if err != nil {
		return err
	}
	return c.Store.saveSetting(ctx, key, string(data))
}
func (c *Core) lockBackupPlan(version string) (func(), error) {
	return c.lockDatabasePlan("postgresql", version)
}
func (c *Core) lockDatabasePlan(engine, version string) (func(), error) {
	key, err := databasePlanKey(engine, version)
	if err != nil {
		return nil, err
	}
	c.backupPlans.mu.Lock()
	defer c.backupPlans.mu.Unlock()
	if c.backupPlans.running[key] {
		return nil, &AppError{Code: "BACKUP_BUSY", Message: "该版本的备份计划正在操作"}
	}
	path, err := c.backupPlanLockPath(engine, version)
	if err != nil {
		return nil, err
	}
	release, err := process.Lock(path)
	if err != nil {
		return nil, &AppError{Code: "BACKUP_BUSY", Message: "另一个应用实例正在操作此备份计划", Detail: err.Error()}
	}
	c.backupPlans.running[key] = true
	return func() {
		c.backupPlans.mu.Lock()
		_ = release()
		delete(c.backupPlans.running, key)
		c.backupPlans.mu.Unlock()
	}, nil
}

func (c *Core) backupPlanLockPath(engine, version string) (string, error) {
	if _, err := databasePlanKey(engine, version); err != nil {
		return "", err
	}
	dir := filepath.Join(c.Home, "backup", "plan-locks")
	if err := managedDirectory(c.Home, dir); err != nil {
		return "", err
	}
	return filepath.Join(dir, engine+"-"+version+".lock"), nil
}
func (c *Core) registerBackupPlans() {
	c.backupPlans.running = map[string]bool{}
	c.registerMySQLBackupPlans()
	c.handle("postgres_backup_plan", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Version string `json:"version"`
		}](data)
		if err != nil {
			return nil, err
		}
		return c.readPostgresPlan(ctx, in.Version)
	})
	c.handle("postgres_backup_plan_save", func(ctx context.Context, data json.RawMessage) (any, error) {
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
		if _, err = c.serviceEntry("postgresql", in.Version); err != nil {
			return nil, err
		}
		unlock, err := c.lockBackupPlan(in.Version)
		if err != nil {
			return nil, err
		}
		defer unlock()
		plan, err := c.readPostgresPlan(ctx, in.Version)
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
		return plan, c.savePostgresPlan(ctx, in.Version, plan)
	})
	c.handle("postgres_backup_plan_run", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Version string `json:"version"`
		}](data)
		if err != nil {
			return nil, err
		}
		return c.runPostgresPlan(ctx, in.Version, true)
	})
}
func (c *Core) runPostgresPlan(ctx context.Context, version string, manual bool) (backupPlan, error) {
	var empty backupPlan
	unlock, err := c.lockBackupPlan(version)
	if err != nil {
		return empty, err
	}
	defer unlock()
	plan, err := c.readPostgresPlan(ctx, version)
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
	if err = c.savePostgresPlan(ctx, version, plan); err != nil {
		return plan, err
	}
	workErr := func() error {
		e, conn, password, err := c.postgresSelected(ctx, version, nil, "postgres")
		if err != nil {
			return err
		}
		defer closePostgres(conn)
		dbs, err := postgresJSON[[]postgresDatabase](ctx, conn, postgresDatabasesSQL)
		if err != nil {
			return postgresError(err, password)
		}
		failures := []string{}
		total := 0
		for _, db := range dbs {
			if db.Protected || !db.AllowConnections {
				continue
			}
			total++
			path, err := c.postgresDump(ctx, e, conn, password, db.Name, db.OID, "", true)
			if err != nil {
				failures = append(failures, db.Name+"："+postgresError(err, password).Error())
				continue
			}
			plan.Files = append(plan.Files, filepath.Base(path))
			if err = c.savePostgresPlan(ctx, version, plan); err != nil {
				return err
			}
			if err = c.rotatePostgresBackups(version, db.OID, plan.Config.Keep, path); err != nil {
				failures = append(failures, db.Name+"：清理旧备份失败，新备份已保留")
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
	return plan, c.savePostgresPlan(saveCtx, version, plan)
}
func (c *Core) rotatePostgresBackups(version string, oid uint32, keep int, newest string) error {
	if keep == 0 {
		return nil
	}
	files, err := c.postgresBackupList()
	if err != nil {
		return err
	}
	prefix := fmtPostgresAutomaticPrefix(version, oid)
	count := 1
	for _, file := range files {
		if string(file.Path) == newest || !strings.HasPrefix(file.Name, prefix) || !strings.Contains(file.Name, "-archive-") {
			continue
		}
		f, err := os.Open(string(file.Path))
		if err != nil {
			return err
		}
		valid := postgresArchiveMagic(f) == nil
		f.Close()
		if !valid {
			continue
		}
		count++
		if count > keep {
			if err = c.postgresBackupDelete(file.Name); err != nil {
				return err
			}
		}
	}
	return nil
}
func (c *Core) runDueBackupPlans(ctx context.Context) {
	rows, err := c.Store.db.QueryContext(ctx, "SELECT key FROM settings WHERE key LIKE 'postgresBackupPlan@%' OR key LIKE 'mysqlBackupPlan@%' OR key LIKE 'mariadbBackupPlan@%'")
	if err != nil {
		return
	}
	versions := []string{}
	for rows.Next() {
		var key string
		if err = rows.Scan(&key); err != nil {
			break
		}
		versions = append(versions, key)
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return
	}
	for _, key := range versions {
		prefix, version, _ := strings.Cut(key, "BackupPlan@")
		engine := prefix
		if prefix == "postgres" {
			engine = "postgresql"
		}
		select {
		case <-ctx.Done():
			return
		case <-c.closed:
			return
		default:
		}
		plan, err := c.readDatabasePlan(ctx, engine, version)
		if err != nil || !plan.Config.Enabled || plan.NextAt == nil || *plan.NextAt > time.Now().UnixMilli() {
			continue
		}
		if engine == "postgresql" {
			_, _ = c.runPostgresPlan(ctx, version, false)
		} else {
			_, _ = c.runMySQLPlan(ctx, engine, version, false)
		}
	}
}

func (c *Core) backupScheduler(parent context.Context) {
	defer c.cron.wg.Done()
	ctx, cancel := context.WithCancel(parent)
	defer cancel()
	go func() {
		select {
		case <-c.closed:
			cancel()
		case <-ctx.Done():
		}
	}()
	ticker := time.NewTicker(30 * time.Second)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			c.runDueBackupPlans(ctx)
		}
	}
}
