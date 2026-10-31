package core

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	gnet "github.com/shirou/gopsutil/v4/net"
	"niceenv/internal/process"
)

type serviceRequest struct {
	ID              string  `json:"id"`
	ExpectedVersion *string `json:"expectedVersion"`
	Revision        string  `json:"revision"`
}

func (c *Core) registerServices() {
	c.handle("list_service_status", func(context.Context, json.RawMessage) (any, error) { return c.serviceStatuses(), nil })
	c.handle("set_service_auto_start", c.setServiceAutoStart)
	c.handle("service_history", func(_ context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			N int `json:"n"`
		}](data)
		if err != nil {
			return nil, err
		}
		if in.N == 0 {
			in.N = 200
		}
		if in.N < 1 || in.N > 1000 {
			return nil, invalid("历史条数必须为 1–1000")
		}
		c.services.mu.RLock()
		defer c.services.mu.RUnlock()
		start := max(0, len(c.services.history)-in.N)
		return append([]ServiceHistory{}, c.services.history[start:]...), nil
	})
	c.handle("start_service", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.serviceAction(ctx, data, "start")
	})
	c.handle("stop_service", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.serviceAction(ctx, data, "stop")
	})
	c.handle("restart_service", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.serviceAction(ctx, data, "restart")
	})
	c.handle("force_stop_service", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.serviceAction(ctx, data, "force")
	})
	c.handle("service_stop_preview", func(_ context.Context, data json.RawMessage) (any, error) {
		in, err := decode[serviceRequest](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		e, err := c.serviceEntry(in.ID, "")
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		defer c.services.mu.RUnlock()
		return map[string]any{"service": cloneStatus(e), "revision": e.revision}, nil
	})
	c.handle("tail_logs", func(_ context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID    string `json:"id"`
			Lines int    `json:"lines"`
		}](data)
		if err != nil {
			return nil, err
		}
		if in.Lines == 0 {
			in.Lines = 500
		}
		if in.Lines < 1 || in.Lines > 5000 {
			return nil, invalid("日志条数必须为 1–5000")
		}
		e, err := c.serviceEntry(in.ID, "")
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		path := string(e.status.LogFile)
		c.services.mu.RUnlock()
		return readServiceLog(path, in.Lines)
	})
}

func serviceBusy() error {
	return &AppError{Code: "SERVICE_BUSY", Message: "服务正在操作，请稍后重试"}
}

func (c *Core) setServiceAutoStart(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		ID      string `json:"id"`
		Enabled bool   `json:"enabled"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	e, err := c.serviceEntry(in.ID, "")
	if err != nil {
		return nil, err
	}
	text, _, err := c.Store.setting(ctx, "autoStartServices")
	if err != nil {
		return nil, err
	}
	var ids []string
	if text != "" && json.Unmarshal([]byte(text), &ids) != nil {
		return nil, &AppError{Code: "DATA_INVALID", Message: "自动启动列表格式无效，未覆盖旧设置"}
	}
	out := []string{}
	seen := map[string]bool{}
	for _, id := range ids {
		if id != in.ID && !seen[id] {
			out = append(out, id)
			seen[id] = true
		}
	}
	if in.Enabled {
		out = append(out, in.ID)
	}
	if err = c.Store.saveSetting(ctx, "autoStartServices", out); err != nil {
		return nil, err
	}
	c.services.mu.Lock()
	e.status.AutoStart = in.Enabled
	c.services.mu.Unlock()
	return in.Enabled, nil
}

func (c *Core) serviceAction(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[serviceRequest](data)
	if err != nil {
		return nil, err
	}
	if in.ExpectedVersion != nil && *in.ExpectedVersion == "" {
		return nil, invalid("预期版本不能为空")
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	expected := ""
	if in.ExpectedVersion != nil {
		expected = *in.ExpectedVersion
	}
	e, err := c.serviceEntry(in.ID, expected)
	if err != nil {
		return nil, err
	}
	if action == "force" {
		c.services.mu.RLock()
		valid := in.Revision != "" && in.Revision == e.revision
		c.services.mu.RUnlock()
		if !valid {
			return nil, &AppError{Code: "SERVICE_REVISION_CHANGED", Message: "服务状态已变化，请重新查看停止预览"}
		}
		return true, c.stopOwned(ctx, e, true)
	}
	if action == "restart" || action == "stop" {
		if err = c.stopOwned(ctx, e, false); err != nil {
			return nil, err
		}
	}
	if action == "stop" {
		return true, nil
	}
	return true, c.startService(ctx, e)
}

func (c *Core) recordServiceLocked(e *serviceEntry, detail string) {
	c.services.history = append(c.services.history, ServiceHistory{TS: time.Now().UnixMilli(), ServiceID: e.status.ID, Detail: detail})
	if len(c.services.history) > 1000 {
		c.services.history = append([]ServiceHistory{}, c.services.history[len(c.services.history)-1000:]...)
	}
}
func (c *Core) serviceError(e *serviceEntry, err error) {
	c.services.mu.Lock()
	e.status.State = "error"
	e.status.LastError = publicError(err)
	c.recordServiceLocked(e, e.status.LastError.Message)
	c.services.mu.Unlock()
}

func (c *Core) startService(ctx context.Context, e *serviceEntry) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	c.services.mu.RLock()
	child, state := e.child, e.status.State
	missing := len(e.status.MissingRequires) > 0
	c.services.mu.RUnlock()
	if child != nil {
		if state == "running" {
			return nil
		}
		return serviceBusy()
	}
	if missing {
		return &AppError{Code: "SERVICE_DEPENDENCY_MISSING", Message: "请先安装此服务需要的运行时依赖"}
	}
	r, err := c.resolveService(ctx, e)
	if err != nil {
		c.serviceError(e, err)
		return err
	}
	if err = c.prepareServiceDirectories(e, r); err != nil {
		c.serviceError(e, err)
		return err
	}
	lockDir := filepath.Join(c.Home, "service-locks")
	if err = managedDirectory(c.Home, lockDir); err != nil {
		return err
	}
	unlock, err := process.Lock(filepath.Join(lockDir, strings.ReplaceAll(e.status.ID, ":", "-")+".lock"))
	if errors.Is(err, process.ErrLocked) {
		return &AppError{Code: "SERVICE_OWNED_ELSEWHERE", Message: "此服务由另一个实例管理，未重复启动"}
	}
	if err != nil {
		return err
	}
	transferred := false
	defer func() {
		if !transferred {
			_ = unlock()
		}
	}()
	if e.pkg.ID == "php" {
		if err = c.preparePHP(ctx, e, r); err != nil {
			c.serviceError(e, err)
			return err
		}
	}
	if e.pkg.ID == "apache" {
		if err = c.prepareApache(ctx, e, r); err != nil {
			c.serviceError(e, err)
			return err
		}
	}
	if e.pkg.ID == "caddy" {
		if err = c.prepareCaddy(ctx, e, r); err != nil {
			c.serviceError(e, err)
			return err
		}
	}
	if err = checkServicePorts(ctx, r.ports); err != nil {
		c.serviceError(e, err)
		return err
	}
	if e.pkg.ID == "nginx" {
		if err = c.prepareNginx(ctx, e, r); err != nil {
			c.serviceError(e, err)
			return err
		}
	}
	if isMySQLEngine(e.pkg.ID) {
		if err = c.prepareMySQL(ctx, e, r); err != nil {
			c.serviceError(e, err)
			return err
		}
	}
	if e.pkg.ID == "redis" {
		if err = c.prepareRedis(ctx, e, r); err != nil {
			c.serviceError(e, err)
			return err
		}
	}
	if e.pkg.ID == "postgresql" {
		if err = c.preparePostgres(ctx, e, r); err != nil {
			c.serviceError(e, err)
			return err
		}
	}
	log, err := openServiceLog(r.log)
	if err != nil {
		return err
	}
	revision, err := newID("service")
	if err != nil {
		log.Close()
		return err
	}
	life, cancel := context.WithCancel(context.Background())
	c.services.mu.Lock()
	e.status.State = "starting"
	e.status.LastError = nil
	e.revision = revision
	c.services.mu.Unlock()
	spec := process.Spec{Program: r.program, Args: r.args, Directory: r.cwd, Environment: r.env, Stdout: log, Stderr: log}
	if e.pkg.ID == "postgresql" {
		spec.RemoveEnvironment = postgresRemoveEnvironment()
	}
	var pids []int
	if e.pkg.ID == "php" {
		specs := make([]process.Spec, 0, phpPoolWorkers)
		for _, port := range r.ports {
			worker := spec
			worker.Args = []string{"-b", net.JoinHostPort("127.0.0.1", strconv.Itoa(port))}
			specs = append(specs, worker)
		}
		// php-cgi has no Windows shutdown command. Stop only these owned,
		// stateless workers; persistent database adapters never use this path.
		child, pids, err = process.StartGroup(life, specs, true)
	} else {
		child, err = process.Start(life, spec)
		if err == nil {
			pids = []int{child.PID}
		}
	}
	if err != nil {
		cancel()
		log.Close()
		c.serviceError(e, err)
		return err
	}
	transferred = true
	done := make(chan struct{})
	c.services.mu.Lock()
	e.child = child
	e.cancel = cancel
	e.done = done
	e.resolved = r
	e.started = time.Now()
	e.status.PIDs = pids
	c.services.mu.Unlock()
	// lifecycle protects Add against closeServices.Wait; the worker owns log and lock.
	c.services.wg.Add(1)
	go func() {
		defer c.services.wg.Done()
		result := child.Wait()
		cancel()
		logErr := log.Close()
		unlockErr := unlock()
		c.services.mu.Lock()
		if e.child == child {
			intentional := e.status.State == "stopping"
			e.revision += ":exited"
			e.child = nil
			e.cancel = nil
			e.status.PIDs = []int{}
			e.started = time.Time{}
			if e.status.State != "error" {
				e.status.State = "stopped"
				if !intentional {
					e.status.State = "error"
					e.status.LastError = &AppError{Code: "SERVICE_EXITED", Message: fmt.Sprintf("服务进程已退出（代码 %d），请查看日志", result.Code)}
				}
			}
			if cleanupErr := errors.Join(logErr, unlockErr); cleanupErr != nil {
				e.status.State = "error"
				e.status.LastError = publicError(cleanupErr)
			}
			c.recordServiceLocked(e, "进程已退出")
		}
		close(done)
		c.services.mu.Unlock()
	}()
	if err = c.waitServiceReady(ctx, e, r, child); err != nil {
		c.serviceError(e, err)
		if isMySQLEngine(e.pkg.ID) || e.pkg.ID == "redis" || e.pkg.ID == "postgresql" {
			// A database may be recovering its redo log. Never turn a readiness
			// timeout or a cancelled UI request into an unrequested forced kill.
			return &AppError{Code: "DATABASE_START_PENDING", Message: "数据库尚未通过启动检查，进程仍由应用管理，请检查日志后重试停止", Detail: err.Error()}
		}
		if stopErr := child.Stop(); stopErr != nil {
			return errors.Join(err, stopErr)
		}
		select {
		case <-done:
		case <-time.After(10 * time.Second):
			return &AppError{Code: "SERVICE_CLEANUP_PENDING", Message: "启动失败后进程仍在清理，请稍后刷新"}
		}
		return err
	}
	c.services.mu.Lock()
	select {
	case <-child.Done():
		c.services.mu.Unlock()
		return &AppError{Code: "SERVICE_EXITED", Message: "服务在启动检查期间退出"}
	default:
	}
	e.status.State = "running"
	c.recordServiceLocked(e, "服务已启动")
	c.services.mu.Unlock()
	if isMySQLEngine(e.pkg.ID) {
		if err = c.initializeMySQLCredentials(ctx, e); err != nil {
			c.serviceError(e, err)
			return err
		}
	}
	if e.pkg.ID == "postgresql" {
		_, conn, _, connectErr := c.postgresSelected(ctx, e.pkg.Version, nil, "postgres")
		if connectErr != nil {
			c.serviceError(e, connectErr)
			return connectErr
		}
		closePostgres(conn)
	}
	if e.pkg.ID == "php" {
		if err = c.reloadNginxPools(ctx); err != nil {
			stopErr := c.stopOwned(ctx, e, true)
			return errors.Join(err, stopErr)
		}
	}
	return nil
}

func (c *Core) waitServiceReady(ctx context.Context, e *serviceEntry, r *resolvedService, child *process.Child) error {
	seconds := e.pkg.Run.HealthTimeoutSec
	if seconds == 0 {
		seconds = 15
		if isMySQLEngine(e.pkg.ID) {
			seconds = 90
		}
	}
	if seconds < 1 || seconds > 300 {
		return invalid("健康检查超时必须为 1–300 秒")
	}
	ctx, cancel := context.WithTimeout(ctx, time.Duration(seconds)*time.Second)
	defer cancel()
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()
	processOnly := e.pkg.Run.Health == "process"
	if !processOnly && r.port == 0 {
		return invalid("TCP 服务没有声明端口")
	}
	for {
		select {
		case <-c.closed:
			return context.Canceled
		case <-ctx.Done():
			return ctx.Err()
		case <-child.Done():
			return &AppError{Code: "SERVICE_EXITED", Message: "服务在监听端口就绪前退出，请查看日志"}
		case <-ticker.C:
		}
		if processOnly {
			return nil
		}
		conn, err := (&net.Dialer{Timeout: 200 * time.Millisecond}).DialContext(ctx, "tcp4", net.JoinHostPort("127.0.0.1", strconv.Itoa(r.port)))
		if err != nil {
			continue
		}
		_ = conn.Close()
		// Only startup checks inspect listeners. Cached page status never calls this.
		connections, err := gnet.ConnectionsWithContext(ctx, "tcp4")
		if err != nil {
			return &AppError{Code: "SERVICE_PORT_UNVERIFIED", Message: "端口已监听，但无法读取监听进程归属"}
		}
		wanted := map[int]bool{r.port: true}
		if e.pkg.ID == "php" {
			for _, port := range r.ports {
				wanted[port] = true
			}
		}
		for _, listener := range connections {
			if listener.Status != "LISTEN" || !wanted[int(listener.Laddr.Port)] {
				continue
			}
			owned, err := child.OwnsPID(int(listener.Pid))
			if err != nil {
				return &AppError{Code: "SERVICE_PORT_UNVERIFIED", Message: "无法确认监听进程属于本次启动的服务"}
			}
			if !owned {
				return &AppError{Code: "PORT_OWNED_ELSEWHERE", Message: "该端口已由其他程序监听，未将其认作服务启动成功"}
			}
			delete(wanted, int(listener.Laddr.Port))
		}
		if len(wanted) == 0 {
			return nil
		}
	}
}

func (c *Core) stopOwned(ctx context.Context, e *serviceEntry, force bool) error {
	c.services.mu.RLock()
	child, done, r := e.child, e.done, e.resolved
	c.services.mu.RUnlock()
	if child == nil {
		return nil
	}
	if err := ctx.Err(); err != nil {
		return err
	}
	c.services.mu.Lock()
	previous := e.status.State
	if e.child == child {
		e.status.State = "stopping"
	}
	c.services.mu.Unlock()
	sent := false
	defer func() {
		if !sent {
			c.services.mu.Lock()
			if e.child == child {
				e.status.State = previous
			}
			c.services.mu.Unlock()
		}
	}()
	if !force {
		if isMySQLEngine(e.pkg.ID) {
			if err := c.shutdownMySQL(ctx, e); err != nil {
				return err
			}
		} else if e.pkg.ID == "redis" {
			if err := c.shutdownRedis(ctx, e); err != nil {
				return err
			}
		} else if e.pkg.ID == "postgresql" {
			if err := c.shutdownPostgres(ctx, e); err != nil {
				return err
			}
		} else if e.pkg.ID == "apache" {
			if err := signalApache(child, false); err != nil {
				return err
			}
		} else if e.pkg.ID == "memcached" || e.pkg.ID == "caddy" || e.application != nil {
			// Memcached is a volatile cache with no Windows shutdown protocol.
			// Stop only the exact owned process tree, never a process by name.
			if err := child.Stop(); err != nil {
				return err
			}
		} else if e.pkg.ID == "ollama" {
			c.ollama.mu.Lock()
			if c.ollama.active() {
				c.ollama.status.State = "cancelling"
				c.ollama.cancel()
			}
			c.ollama.mu.Unlock()
			c.ollama.wg.Wait()
			if err := child.Stop(); err != nil {
				return err
			}
		} else if len(r.stopArgs) > 0 {
			if e.pkg.ID == "nginx" {
				if err := c.verifyNginxStop(e, r); err != nil {
					return err
				}
			}
			out := process.NewLimitedBuffer(8192)
			stopCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
			stopper, err := process.Start(stopCtx, process.Spec{Program: r.program, Args: r.stopArgs, Directory: r.cwd, Environment: r.env, Stdout: out, Stderr: out})
			if err == nil {
				result := stopper.Wait()
				if result.Err != nil || result.Code != 0 {
					err = &AppError{Code: "SERVICE_STOP_FAILED", Message: "原生停机命令失败，请查看日志"}
				}
			}
			cancel()
			if err != nil {
				return err
			}
		} else if err := child.RequestStop(); err != nil {
			return &AppError{Code: "SERVICE_GRACEFUL_UNAVAILABLE", Message: "此服务没有可用的普通停机协议，请核对进程后使用强制停止"}
		}
	} else if err := child.Stop(); err != nil {
		return err
	}
	sent = true
	c.services.mu.Lock()
	if e.child == child {
		e.status.State = "stopping"
	}
	c.services.mu.Unlock()
	timer := time.NewTimer(15 * time.Second)
	defer timer.Stop()
	select {
	case <-done:
		if e.pkg.ID == "php" {
			return c.reloadNginxPools(ctx)
		}
		return nil
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return &AppError{Code: "SERVICE_STOP_PENDING", Message: "已发送停机请求，服务仍在退出；未强制结束进程"}
	}
}

func (c *Core) closeServices() {
	c.services.lifecycle.Lock()
	defer c.services.lifecycle.Unlock()
	c.services.mu.Lock()
	c.services.closing = true
	entries := make([]*serviceEntry, 0, len(c.services.entries))
	for _, e := range c.services.entries {
		entries = append(entries, e)
	}
	c.services.mu.Unlock()
	for _, e := range entries {
		_ = c.stopOwned(context.Background(), e, true)
	}
	c.services.wg.Wait()
}
