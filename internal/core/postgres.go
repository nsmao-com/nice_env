package core

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
	"unicode"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgproto3"
	"niceenv/internal/process"
)

func postgresPasswordKey(version string) string { return "postgresPassword@" + version }
func databaseRandomPassword() (string, error) {
	data := make([]byte, 24)
	_, err := rand.Read(data)
	return hex.EncodeToString(data), err
}
func postgresDataVersion(path, version string) error {
	root, err := os.OpenRoot(path)
	if err != nil {
		return err
	}
	defer root.Close()
	data, exists, err := envRead(root, "PG_VERSION")
	if err != nil {
		return err
	}
	major := strings.Split(version, ".")[0]
	if major == "9" {
		parts := strings.Split(version, ".")
		if len(parts) > 1 {
			major += "." + parts[1]
		}
	}
	if !exists || strings.TrimSpace(string(data)) != major {
		return &AppError{Code: "POSTGRES_DATA_VERSION", Message: "PostgreSQL 数据目录版本与所选程序不一致，未启动或初始化"}
	}
	return nil
}
func postgresProgram(r *resolvedService, name string) (string, error) {
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	path := filepath.Join(filepath.Dir(r.program), name)
	info, err := os.Lstat(path)
	if err != nil {
		return "", err
	}
	if !info.Mode().IsRegular() {
		return "", invalid("PostgreSQL 工具不是普通文件")
	}
	return path, nil
}
func postgresRemoveEnvironment() []string {
	keys := []string{}
	for _, entry := range os.Environ() {
		key, _, _ := strings.Cut(entry, "=")
		if strings.HasPrefix(strings.ToUpper(key), "PG") {
			keys = append(keys, key)
		}
	}
	return keys
}
func (c *Core) preparePostgres(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	entries, err := os.ReadDir(r.data)
	if err != nil {
		return err
	}
	if len(entries) == 0 {
		password, err := databaseRandomPassword()
		if err != nil {
			return err
		}
		stage, err := os.MkdirTemp(filepath.Dir(r.data), ".postgres-init-")
		if err != nil {
			return err
		}
		private, err := os.MkdirTemp("", "niceenv-initdb-")
		if err != nil {
			return err
		}
		defer os.RemoveAll(private)
		passfile := filepath.Join(private, "password")
		if err = os.WriteFile(passfile, []byte(password+"\n"), 0600); err != nil {
			return err
		}
		if err = c.Store.saveSetting(ctx, postgresPasswordKey(e.pkg.Version), password); err != nil {
			return err
		}
		program, err := postgresProgram(r, "initdb")
		if err != nil {
			return err
		}
		initCtx, cancel := context.WithTimeout(ctx, 3*time.Minute)
		defer cancel()
		output := process.NewLimitedBuffer(64 * 1024)
		child, err := process.Start(initCtx, process.Spec{Program: program, Directory: filepath.Dir(r.program), Args: []string{"-D", DisplayPath(stage), "-U", "postgres", "-A", "scram-sha-256", "-E", "UTF8", "--no-locale", "--pwfile", DisplayPath(passfile)}, RemoveEnvironment: postgresRemoveEnvironment(), Stdout: output, Stderr: output})
		if err != nil {
			return err
		}
		result := child.Wait()
		if result.Err != nil || result.Code != 0 || initCtx.Err() != nil {
			return &AppError{Code: "POSTGRES_INIT_FAILED", Message: "PostgreSQL 初始化失败，原数据目录未覆盖", Detail: strings.ReplaceAll(output.String(), password, "***")}
		}
		if err = postgresDataVersion(stage, e.pkg.Version); err != nil {
			return err
		}
		// A failed initialization is retained in its own staging directory. Only
		// the empty destination may be removed; concurrent writes cause failure.
		if err = os.Remove(r.data); err != nil {
			return err
		}
		if err = os.Rename(stage, r.data); err != nil {
			return err
		}
	} else if err = postgresDataVersion(r.data, e.pkg.Version); err != nil {
		return err
	}
	program, err := postgresProgram(r, "postgres")
	if err != nil {
		return err
	}
	r.program = program
	r.cwd = filepath.Dir(program)
	r.args = []string{"-D", DisplayPath(r.data), "-p", strconv.Itoa(r.port), "-c", "listen_addresses=127.0.0.1"}
	if runtime.GOOS != "windows" {
		socketDir := filepath.Join(r.etc, "run")
		if err = managedDirectory(c.Home, socketDir); err != nil {
			return err
		}
		r.args = append(r.args, "-c", "unix_socket_directories="+DisplayPath(socketDir))
	}
	r.stopArgs = nil
	return nil
}
func (c *Core) shutdownPostgres(ctx context.Context, e *serviceEntry) error {
	c.services.mu.RLock()
	r, child := e.resolved, e.child
	c.services.mu.RUnlock()
	if r == nil || child == nil {
		return nil
	}
	root, err := os.OpenRoot(r.data)
	if err != nil {
		return err
	}
	defer root.Close()
	data, exists, err := envRead(root, "postmaster.pid")
	if err != nil {
		return err
	}
	if !exists {
		return invalid("PostgreSQL 缺少进程身份记录，未发送停机命令")
	}
	pid, err := strconv.Atoi(strings.TrimSpace(strings.SplitN(string(data), "\n", 2)[0]))
	if err != nil {
		return err
	}
	owned, err := child.OwnsPID(pid)
	if err != nil {
		return err
	}
	if !owned {
		return &AppError{Code: "POSTGRES_PID_UNVERIFIED", Message: "数据目录中的进程不属于当前 PostgreSQL 服务，未发送停机命令"}
	}
	program, err := postgresProgram(r, "pg_ctl")
	if err != nil {
		return err
	}
	stopCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	output := process.NewLimitedBuffer(8192)
	stopper, err := process.Start(stopCtx, process.Spec{Program: program, Directory: r.cwd, Args: []string{"-D", DisplayPath(r.data), "-m", "fast", "-w", "-t", "10", "stop"}, RemoveEnvironment: postgresRemoveEnvironment(), Stdout: output, Stderr: output})
	if err != nil {
		return err
	}
	result := stopper.Wait()
	if result.Err != nil || result.Code != 0 {
		return &AppError{Code: "POSTGRES_STOP_FAILED", Message: "PostgreSQL 原生停机未确认完成，未强制结束进程", Detail: output.String()}
	}
	return nil
}
func postgresName(name string, fresh bool) (string, error) {
	if name == "" || len(name) > 63 || strings.IndexFunc(name, unicode.IsControl) >= 0 {
		return "", invalid("名称须为 1–63 字节且不能包含控制字符")
	}
	if fresh {
		for _, ch := range name {
			if ch != '_' && !(ch >= 'a' && ch <= 'z' || ch >= 'A' && ch <= 'Z' || ch >= '0' && ch <= '9') {
				return "", invalid("新名称仅支持字母、数字和下划线")
			}
		}
		lower := strings.ToLower(name)
		if strings.HasPrefix(lower, "pg_") || lower == "postgres" || lower == "template0" || lower == "template1" {
			return "", invalid("此名称保留给系统使用")
		}
	}
	return `"` + strings.ReplaceAll(name, `"`, `""`) + `"`, nil
}
func postgresPasswordValid(password string) error {
	if len(password) < 1 || len(password) > 4096 || strings.IndexFunc(password, unicode.IsControl) >= 0 {
		return invalid("密码须为 1–4096 字节且不能包含控制字符")
	}
	return nil
}
func closePostgres(conn *pgx.Conn) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	_ = conn.Close(ctx)
}
func postgresConnect(ctx context.Context, port int, password, database string) (*pgx.Conn, error) {
	if port < 1 || port > 65535 {
		return nil, invalid("PostgreSQL 端口无效")
	}
	if _, err := postgresName(database, false); err != nil {
		return nil, err
	}
	// ParseConfig must construct the SDK's internal defaults. All connection and
	// startup settings are then explicit; inherited PG* values cannot redirect it.
	// pgx treats even service='' as a request to load a service file. Refuse
	// inherited service aliases instead of mutating the process environment.
	if os.Getenv("PGSERVICE") != "" {
		return nil, &AppError{Code: "POSTGRES_ENV_CONFLICT", Message: "检测到 PGSERVICE 环境变量，请移除后重启 NiceEnv，避免连接到其他数据库实例"}
	}
	config, err := pgx.ParseConfig("host=127.0.0.1 port=5432 user=postgres dbname=postgres password='' sslmode=disable passfile='' target_session_attrs=any connect_timeout=5")
	if err != nil {
		return nil, err
	}
	config.Host = "127.0.0.1"
	config.Port = uint16(port)
	config.User = "postgres"
	config.Password = password
	config.Database = database
	config.TLSConfig = nil
	config.Fallbacks = nil
	config.ConnectTimeout = 5 * time.Second
	config.ValidateConnect = nil
	config.RuntimeParams = map[string]string{"client_encoding": "UTF8", "application_name": "NiceEnv", "statement_timeout": "20000", "lock_timeout": "5000"}
	config.BuildFrontend = func(reader io.Reader, writer io.Writer) *pgproto3.Frontend {
		front := pgproto3.NewFrontend(reader, writer)
		front.SetMaxBodyLen(16 * 1024 * 1024)
		return front
	}
	connectCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	for {
		conn, err := pgx.ConnectConfig(connectCtx, config.Copy())
		var pgerr *pgconn.PgError
		if err == nil || !errors.As(err, &pgerr) || pgerr.Code != "57P03" {
			return conn, err
		}
		// The TCP listener opens before crash recovery finishes. A startup
		// rejection is transient; authentication and identity errors are not.
		timer := time.NewTimer(100 * time.Millisecond)
		select {
		case <-connectCtx.Done():
			timer.Stop()
			return nil, err
		case <-timer.C:
		}
	}
}
func postgresError(err error, password string) error {
	if err == nil {
		return nil
	}
	var appErr *AppError
	if errors.As(err, &appErr) {
		return appErr
	}
	detail := err.Error()
	if password != "" {
		detail = strings.ReplaceAll(detail, password, "***")
	}
	return &AppError{Code: "POSTGRES_QUERY_FAILED", Message: "PostgreSQL 操作未完成，请检查实例状态与权限", Hint: "写入结果请以实例实际状态为准；查询最长 20 秒", Detail: redactDiagnostic(detail)}
}
func (c *Core) postgresConnection(ctx context.Context, e *serviceEntry, password, database string) (*pgx.Conn, error) {
	if err := c.verifyDatabaseListener(ctx, e); err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	r := *e.resolved
	c.services.mu.RUnlock()
	conn, err := postgresConnect(ctx, r.port, password, database)
	if err != nil {
		return nil, err
	}
	var data string
	if err = conn.QueryRow(ctx, "SHOW data_directory").Scan(&data); err != nil {
		closePostgres(conn)
		return nil, err
	}
	actual, err := filepath.EvalSymlinks(DisplayPath(data))
	expected, expectedErr := filepath.EvalSymlinks(r.data)
	if err != nil || expectedErr != nil || projectPathKey(actual) != projectPathKey(expected) {
		closePostgres(conn)
		return nil, &AppError{Code: "POSTGRES_INSTANCE_MISMATCH", Message: "PostgreSQL 实际数据目录与当前实例不一致，已拒绝操作"}
	}
	return conn, nil
}
func (c *Core) postgresSelected(ctx context.Context, version string, password *string, database string) (*serviceEntry, *pgx.Conn, string, error) {
	e, err := c.serviceEntry("postgresql", version)
	if err != nil {
		return nil, nil, "", err
	}
	if canonicalVersion(e.pkg.Version) != canonicalVersion(version) {
		return nil, nil, "", invalid("正在运行的 PostgreSQL 版本已变化，请刷新")
	}
	secret, _, err := c.Store.setting(ctx, postgresPasswordKey(version))
	if err != nil {
		return nil, nil, "", err
	}
	if password != nil {
		secret = *password
	}
	conn, err := c.postgresConnection(ctx, e, secret, database)
	if err != nil && password == nil {
		pending, exists, readErr := c.Store.setting(ctx, postgresPasswordKey(version)+".pending")
		if readErr != nil {
			return nil, nil, "", readErr
		}
		if exists && pending != "" {
			candidate, candidateErr := c.postgresConnection(ctx, e, pending, database)
			if candidateErr == nil {
				if saveErr := c.Store.saveSetting(ctx, postgresPasswordKey(version), pending); saveErr != nil {
					closePostgres(candidate)
					return nil, nil, "", saveErr
				}
				secret, conn, err = pending, candidate, nil
			}
		}
	}
	if err != nil {
		return nil, nil, "", postgresError(err, secret)
	}
	return e, conn, secret, nil
}
func (c *Core) postgresRequiresPassword(ctx context.Context, e *serviceEntry) (bool, error) {
	wrong, err := databaseRandomPassword()
	if err != nil {
		return false, err
	}
	conn, err := c.postgresConnection(ctx, e, wrong, "postgres")
	if err == nil {
		closePostgres(conn)
		return false, nil
	}
	var pgerr *pgconn.PgError
	if errors.As(err, &pgerr) && pgerr.Code == "28P01" {
		return true, nil
	}
	return false, &AppError{Code: "POSTGRES_AUTH_UNCONFIRMED", Message: "未能确认 PostgreSQL 是否强制要求密码", Detail: redactDiagnostic(err.Error())}
}
func postgresLiteral(value string) string {
	return "E'" + strings.ReplaceAll(strings.ReplaceAll(value, `\`, `\\`), "'", "''") + "'"
}
func postgresSetRoleSecret(ctx context.Context, conn *pgx.Conn, prefix, password string) error {
	if err := postgresPasswordValid(password); err != nil {
		return err
	}
	for _, sql := range []string{"SET log_statement='none'", "SET log_min_error_statement='panic'", "SET log_min_duration_statement=-1", "SET log_duration=off", "SELECT set_config('log_min_duration_sample','-1',false) WHERE current_setting('log_min_duration_sample',true) IS NOT NULL", "SELECT set_config('log_transaction_sample_rate','0',false) WHERE current_setting('log_transaction_sample_rate',true) IS NOT NULL", "SET password_encryption='scram-sha-256'"} {
		if _, err := conn.Exec(ctx, sql); err != nil {
			return postgresError(err, password)
		}
	}
	_, err := conn.Exec(ctx, prefix+" PASSWORD "+postgresLiteral(password))
	return postgresError(err, password)
}
