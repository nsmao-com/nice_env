package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"
	"unicode"
	"unicode/utf8"

	"niceenv/internal/process"
)

type CronJob struct {
	ID          string  `json:"id"`
	Name        string  `json:"name"`
	Command     string  `json:"command"`
	IntervalMin int64   `json:"intervalMin"`
	Enabled     bool    `json:"enabled"`
	CreatedAt   int64   `json:"createdAt"`
	LastRunAt   *int64  `json:"lastRunAt"`
	LastExit    *string `json:"lastExit"`
	LastOutput  *string `json:"lastOutput"`
}
type cronRun struct {
	cancel context.CancelFunc
	done   chan struct{}
}
type cronManager struct {
	mu        sync.Mutex
	runs      map[string]*cronRun
	closing   bool
	wg        sync.WaitGroup
	scheduler sync.Once
}

var cronID = regexp.MustCompile(`^[a-zA-Z0-9_-]{1,128}$`)

const cronSelect = `SELECT id,name,command,interval_min,enabled,created_at,last_run_at,last_exit,last_output FROM cron_jobs`

func scanCron(row interface{ Scan(...any) error }) (CronJob, error) {
	var job CronJob
	err := row.Scan(&job.ID, &job.Name, &job.Command, &job.IntervalMin, &job.Enabled, &job.CreatedAt, &job.LastRunAt, &job.LastExit, &job.LastOutput)
	if errors.Is(err, sql.ErrNoRows) {
		return job, &AppError{Code: "CRON_NOT_FOUND", Message: "计划任务不存在，请刷新列表"}
	}
	return job, err
}
func validateCron(job CronJob) error {
	if !cronID.MatchString(job.ID) || job.Name == "" || utf8.RuneCountInString(job.Name) > 128 || strings.IndexFunc(job.Name, unicode.IsControl) >= 0 {
		return invalid("任务名称或标识无效")
	}
	if strings.TrimSpace(job.Command) == "" || len(job.Command) > 8192 || strings.ContainsRune(job.Command, 0) {
		return invalid("命令不能为空、包含空字符或超过 8192 字节")
	}
	if job.IntervalMin < 1 || job.IntervalMin > 525600 {
		return invalid("执行周期必须为 1–525600 分钟")
	}
	return nil
}
func (c *Core) cronJob(ctx context.Context, id string) (CronJob, error) {
	return scanCron(c.Store.db.QueryRowContext(ctx, cronSelect+" WHERE id=?", id))
}

func (c *Core) registerCron() {
	c.cron.runs = map[string]*cronRun{}
	c.handle("cron_jobs", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.cronJobs(ctx) })
	c.handle("cron_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			Job CronJob `json:"job"`
		}](data)
		if err != nil {
			return nil, err
		}
		job := args.Job
		job.Name = strings.TrimSpace(job.Name)
		job.Command = strings.TrimSpace(job.Command)
		create := strings.TrimSpace(job.ID) == ""
		if create {
			job.ID, err = newID("cron")
			if err != nil {
				return nil, err
			}
			job.CreatedAt = time.Now().UnixMilli()
		}
		if err = validateCron(job); err != nil {
			return nil, err
		}
		var result sql.Result
		if create {
			result, err = c.Store.db.ExecContext(ctx, "INSERT INTO cron_jobs(id,name,command,interval_min,enabled,created_at) VALUES(?,?,?,?,?,?)", job.ID, job.Name, job.Command, job.IntervalMin, job.Enabled, job.CreatedAt)
		} else {
			result, err = c.Store.db.ExecContext(ctx, "UPDATE cron_jobs SET name=?,command=?,interval_min=? WHERE id=? AND last_exit IS NOT 'running'", job.Name, job.Command, job.IntervalMin, job.ID)
		}
		return true, cronChanged(result, err, "CRON_NOT_EDITABLE", "任务不存在或正在运行，请刷新列表或停止后再编辑")
	})
	c.handle("cron_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		result, err := c.Store.db.ExecContext(ctx, "DELETE FROM cron_jobs WHERE id=? AND last_exit IS NOT 'running'", args.ID)
		return true, cronChanged(result, err, "CRON_NOT_REMOVABLE", "任务不存在或仍在运行，请先停止任务")
	})
	c.handle("cron_set_enabled", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			ID      string `json:"id"`
			Enabled bool   `json:"enabled"`
		}](data)
		if err != nil {
			return nil, err
		}
		result, err := c.Store.db.ExecContext(ctx, "UPDATE cron_jobs SET enabled=? WHERE id=?", args.Enabled, args.ID)
		return true, cronChanged(result, err, "CRON_NOT_FOUND", "计划任务不存在，请刷新列表")
	})
	c.handle("cron_run_now", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		return c.runCron(ctx, args.ID, true)
	})
	c.handle("cron_stop", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		c.cron.mu.Lock()
		run := c.cron.runs[args.ID]
		if run != nil {
			run.cancel()
		}
		c.cron.mu.Unlock()
		if run == nil {
			job, err := c.cronJob(ctx, args.ID)
			if err != nil {
				return nil, err
			}
			if job.LastExit != nil && *job.LastExit == "running" {
				return nil, &AppError{Code: "CRON_NOT_OWNED", Message: "任务可能由另一实例执行，无法确认进程归属"}
			}
			return true, nil
		}
		select {
		case <-run.done:
			return true, nil
		case <-ctx.Done():
			return nil, ctx.Err()
		case <-time.After(5 * time.Second):
			return nil, &AppError{Code: "CRON_STOP_PENDING", Message: "已请求停止，进程仍在清理，请刷新状态"}
		}
	})
}
func cronChanged(result sql.Result, err error, code, message string) error {
	if err != nil {
		return err
	}
	count, err := result.RowsAffected()
	if err != nil {
		return err
	}
	if count == 0 {
		return &AppError{Code: code, Message: message}
	}
	return nil
}
func (c *Core) cronJobs(ctx context.Context) ([]CronJob, error) {
	rows, err := c.Store.db.QueryContext(ctx, cronSelect+" ORDER BY created_at DESC")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	result := []CronJob{}
	for rows.Next() {
		job, err := scanCron(rows)
		if err != nil {
			return nil, err
		}
		result = append(result, job)
	}
	return result, rows.Err()
}
func (c *Core) cronLock(id string) (func() error, error) {
	if !cronID.MatchString(id) {
		return nil, invalid("任务标识无效")
	}
	dir := filepath.Join(c.Home, "cron-locks")
	if err := os.MkdirAll(dir, 0700); err != nil {
		return nil, err
	}
	unlock, err := process.Lock(filepath.Join(dir, id+".lock"))
	if errors.Is(err, process.ErrLocked) {
		return nil, &AppError{Code: "CRON_BUSY", Message: "该计划任务正在运行，请等待或停止当前任务"}
	}
	return unlock, err
}
func (c *Core) runCron(ctx context.Context, id string, manual bool) (CronJob, error) {
	var empty CronJob
	unlock, err := c.cronLock(id)
	if err != nil {
		return empty, err
	}
	defer unlock()
	ctx, cancel := context.WithTimeout(ctx, 30*time.Minute)
	defer cancel()
	run := &cronRun{cancel: cancel, done: make(chan struct{})}
	c.cron.mu.Lock()
	if c.cron.closing {
		c.cron.mu.Unlock()
		return empty, &AppError{Code: "CRON_SHUTDOWN", Message: "应用正在退出"}
	}
	if c.cron.runs[id] != nil {
		c.cron.mu.Unlock()
		return empty, &AppError{Code: "CRON_BUSY", Message: "任务仍在运行或清理"}
	}
	c.cron.runs[id] = run
	c.cron.wg.Add(1)
	c.cron.mu.Unlock()
	defer func() {
		c.cron.mu.Lock()
		delete(c.cron.runs, id)
		close(run.done)
		c.cron.mu.Unlock()
		c.cron.wg.Done()
	}()
	tx, err := c.Store.db.BeginTx(ctx, nil)
	if err != nil {
		return empty, err
	}
	job, err := scanCron(tx.QueryRowContext(ctx, cronSelect+" WHERE id=?", id))
	if err != nil {
		tx.Rollback()
		return empty, err
	}
	if err = validateCron(job); err != nil {
		tx.Rollback()
		return empty, err
	}
	if job.LastExit != nil && *job.LastExit == "running" {
		tx.Rollback()
		return empty, &AppError{Code: "CRON_BUSY", Message: "上次任务结果尚未确认，请重新打开应用进行恢复"}
	}
	now := time.Now().UnixMilli()
	previous := job.CreatedAt
	if job.LastRunAt != nil {
		previous = *job.LastRunAt
	}
	if !manual && (!job.Enabled || now-previous < job.IntervalMin*60000) {
		tx.Rollback()
		return job, nil
	}
	if _, err = tx.ExecContext(ctx, "UPDATE cron_jobs SET last_run_at=?,last_exit='running',last_output=NULL WHERE id=?", now, id); err != nil {
		tx.Rollback()
		return empty, err
	}
	if err = tx.Commit(); err != nil {
		return empty, err
	}
	out, stderr := process.NewLimitedBuffer(32*1024), process.NewLimitedBuffer(32*1024)
	spec, spawnErr := process.Shell(job.Command, c.Home, out, stderr)
	exit := "spawn failed"
	detail := ""
	if spawnErr == nil {
		child, startErr := process.Start(ctx, spec)
		spawnErr = startErr
		if startErr == nil {
			result := child.Wait()
			exit = fmt.Sprintf("exit %d", result.Code)
			if result.Err != nil {
				exit = "execution failed"
				detail = result.Err.Error()
			}
			if errors.Is(ctx.Err(), context.Canceled) {
				exit = "cancelled"
				detail = "已停止命令及其子进程"
			} else if errors.Is(ctx.Err(), context.DeadlineExceeded) {
				exit = "timeout"
				detail = "任务超过运行时间，已终止命令及其子进程"
			}
		}
	}
	if spawnErr != nil {
		detail = spawnErr.Error()
	}
	output := strings.TrimSpace(strings.Join([]string{out.String(), stderr.String(), detail}, "\n"))
	saveCtx, saveCancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer saveCancel()
	result, err := c.Store.db.ExecContext(saveCtx, "UPDATE cron_jobs SET last_exit=?,last_output=? WHERE id=? AND last_run_at=? AND last_exit='running'", exit, output, id, now)
	if err = cronChanged(result, err, "CRON_RESULT_CHANGED", "任务结果已变化，未覆盖其它执行结果"); err != nil {
		return empty, err
	}
	if c.emit != nil {
		c.emit("cron://changed", map[string]string{"id": id})
	}
	return c.cronJob(saveCtx, id)
}

func (c *Core) recoverCron(ctx context.Context) error {
	jobs, err := c.cronJobs(ctx)
	if err != nil {
		return err
	}
	for _, job := range jobs {
		if job.LastExit == nil || *job.LastExit != "running" {
			continue
		}
		unlock, err := c.cronLock(job.ID)
		if err != nil {
			var app *AppError
			if errors.As(err, &app) && app.Code == "CRON_BUSY" {
				continue
			}
			return err
		}
		_, err = c.Store.db.ExecContext(ctx, "UPDATE cron_jobs SET enabled=0,last_exit='interrupted',last_output=? WHERE id=? AND last_run_at IS ? AND last_exit='running'", "上次执行被中断，无法确认是否完成。自动调度已暂停，请检查命令影响后再启用。", job.ID, job.LastRunAt)
		unlock()
		if err != nil {
			return err
		}
	}
	return nil
}

func (c *Core) StartSchedulers(ctx context.Context) {
	c.cron.scheduler.Do(func() {
		c.cron.mu.Lock()
		if c.cron.closing {
			c.cron.mu.Unlock()
			return
		}
		c.cron.wg.Add(3)
		c.cron.mu.Unlock()
		go c.backupScheduler(ctx)
		go c.monitorScheduler(ctx)
		go func() {
			defer c.cron.wg.Done()
			ticker := time.NewTicker(20 * time.Second)
			defer ticker.Stop()
			for {
				select {
				case <-ctx.Done():
					return
				case <-c.closed:
					return
				case <-ticker.C:
				}
				jobs, err := c.cronJobs(ctx)
				if err != nil {
					continue
				}
				for _, job := range jobs {
					previous := job.CreatedAt
					if job.LastRunAt != nil {
						previous = *job.LastRunAt
					}
					if !job.Enabled || job.LastExit != nil && *job.LastExit == "running" || time.Now().UnixMilli()-previous < job.IntervalMin*60000 {
						continue
					}
					c.cron.mu.Lock()
					closing := c.cron.closing
					c.cron.mu.Unlock()
					if closing {
						return
					}
					go func(id string) { _, _ = c.runCron(ctx, id, false) }(job.ID)
				}
			}
		}()
	})
}
