package core

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"

	mysql "github.com/go-sql-driver/mysql"
	gnet "github.com/shirou/gopsutil/v4/net"
	"niceenv/internal/process"
)

func (c *Core) prepareMySQL(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	if e.pkg.ID == "mariadb" {
		return c.prepareMariaDB(ctx, e, r)
	}
	base := filepath.Dir(filepath.Dir(r.program))
	r.cwd = base
	path := filepath.Join(r.etc, "my.ini")
	if _, err := readConfigText(path); os.IsNotExist(err) {
		baseText, err := phpIniQuote(base)
		if err != nil {
			return err
		}
		dataText, err := phpIniQuote(r.data)
		if err != nil {
			return err
		}
		text := fmt.Sprintf("[mysqld]\nbasedir=%s\ndatadir=%s\nport=%d\nbind-address=127.0.0.1\nloose-mysqlx=0\ncharacter-set-server=utf8mb4\ncollation-server=utf8mb4_unicode_ci\nmax_connections=150\ninnodb_buffer_pool_size=128M\n[client]\nport=%d\ndefault-character-set=utf8mb4\n", baseText, dataText, r.port, r.port)
		t := configTarget{spec: configSpecs[4], key: "mysql-ini@" + e.pkg.Version, path: path, program: r.program, version: e.pkg.Version}
		if err = c.writeConfigExpected(ctx, t, text, nil, true); err != nil {
			return err
		}
	} else if err != nil {
		return err
	}
	r.args = []string{"--defaults-file=" + DisplayPath(path), "--basedir=" + DisplayPath(base), "--datadir=" + DisplayPath(r.data), "--port=" + strconv.Itoa(r.port)}
	if runtime.GOOS == "windows" {
		r.args = append(r.args, "--console")
	}
	r.stopArgs = nil
	files, err := os.ReadDir(r.data)
	if err != nil {
		return err
	}
	if len(files) > 0 {
		bootstrap, _, readErr := c.Store.setting(ctx, "mysqlBootstrap@"+e.pkg.Version)
		if readErr != nil {
			return readErr
		}
		identity, identityErr := mysqlBootstrapIdentity(r.data)
		r.freshDatabase = identityErr == nil && bootstrap == identity
		return nil
	}
	stage, err := os.MkdirTemp(filepath.Dir(r.data), ".mysql-init-")
	if err != nil {
		return err
	}
	// Failed staging directories are retained for diagnostics; existing data is
	// never recursively removed or initialised in place.
	args := []string{"--no-defaults", "--basedir=" + DisplayPath(base), "--datadir=" + DisplayPath(stage), "--initialize-insecure"}
	if runtime.GOOS == "windows" {
		args = append(args, "--console")
	}
	initCtx, cancel := context.WithTimeout(ctx, 3*time.Minute)
	defer cancel()
	out := process.NewLimitedBuffer(64 * 1024)
	child, err := process.Start(initCtx, process.Spec{Program: r.program, Args: args, Directory: base, Stdout: out, Stderr: out})
	if err != nil {
		return err
	}
	result := child.Wait()
	if result.Err != nil || result.Code != 0 || initCtx.Err() != nil {
		return &AppError{Code: "MYSQL_INIT_FAILED", Message: "MySQL 初始化失败，原数据目录未覆盖", Detail: out.String()}
	}
	// Remove only the verified empty destination. Concurrent user writes cause
	// Remove to fail, preserving both their files and the staged database.
	identity, err := mysqlBootstrapIdentity(stage)
	if err != nil {
		return err
	}
	if err = c.Store.saveSetting(ctx, "mysqlBootstrap@"+e.pkg.Version, identity); err != nil {
		return err
	}
	if err = os.Remove(r.data); err != nil {
		return err
	}
	if err = os.Rename(stage, r.data); err != nil {
		return err
	}
	r.freshDatabase = true
	return nil
}

// auto.cnf is generated inside the private staging directory. Binding recovery
// to its digest prevents a failed publish from claiming unrelated user data.
func mysqlBootstrapIdentity(dir string) (string, error) {
	content, err := os.ReadFile(filepath.Join(dir, "auto.cnf"))
	if err != nil {
		return "", err
	}
	if len(content) == 0 || len(content) > 4096 || !strings.Contains(string(content), "server-uuid=") {
		return "", invalid("MySQL 初始化身份文件无效")
	}
	digest := sha256.Sum256(content)
	return hex.EncodeToString(digest[:]), nil
}

func (c *Core) verifyDatabaseListener(ctx context.Context, e *serviceEntry) error {
	c.services.mu.RLock()
	child, r := e.child, e.resolved
	c.services.mu.RUnlock()
	if child == nil || r == nil {
		return &AppError{Code: "MYSQL_NOT_RUNNING", Message: "数据库实例未运行"}
	}
	connections, err := gnet.ConnectionsWithContext(ctx, "tcp4")
	if err != nil {
		return err
	}
	for _, conn := range connections {
		if conn.Status == "LISTEN" && int(conn.Laddr.Port) == r.port {
			owned, err := child.OwnsPID(int(conn.Pid))
			if err != nil {
				return err
			}
			if owned {
				return nil
			}
			return &AppError{Code: "MYSQL_INSTANCE_MISMATCH", Message: "监听端口不属于当前受管数据库"}
		}
	}
	return &AppError{Code: "MYSQL_NOT_RUNNING", Message: "数据库端口未监听"}
}
func (c *Core) mysqlConnection(ctx context.Context, e *serviceEntry, password string) (*sql.DB, error) {
	if err := c.verifyDatabaseListener(ctx, e); err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	r := *e.resolved
	c.services.mu.RUnlock()
	cfg := mysql.NewConfig()
	cfg.User = "root"
	cfg.Passwd = password
	cfg.Net = "tcp"
	cfg.Addr = net.JoinHostPort("127.0.0.1", strconv.Itoa(r.port))
	cfg.Timeout = 5 * time.Second
	cfg.ReadTimeout = 30 * time.Second
	cfg.WriteTimeout = 30 * time.Second
	cfg.InterpolateParams = true
	cfg.MultiStatements = true
	connector, err := mysql.NewConnector(cfg)
	if err != nil {
		return nil, err
	}
	db := sql.OpenDB(connector)
	db.SetMaxOpenConns(1)
	db.SetMaxIdleConns(1)
	db.SetConnMaxLifetime(time.Minute)
	var data string
	if err = db.QueryRowContext(ctx, "SELECT @@datadir").Scan(&data); err != nil {
		db.Close()
		return nil, &AppError{Code: "MYSQL_AUTH_FAILED", Message: "无法验证数据库 root 凭据", Detail: err.Error()}
	}
	canonical := func(p string) string {
		p = filepath.Clean(DisplayPath(p))
		if resolved, err := filepath.EvalSymlinks(p); err == nil {
			p = resolved
		}
		if runtime.GOOS == "windows" {
			p = strings.ToLower(p)
		}
		return p
	}
	if canonical(data) != canonical(r.data) {
		db.Close()
		return nil, &AppError{Code: "MYSQL_INSTANCE_MISMATCH", Message: "数据库实际数据目录与当前实例不一致，已拒绝操作"}
	}
	return db, nil
}
func isMySQLEngine(id string) bool             { return id == "mysql" || id == "mariadb" }
func mysqlPasswordKey(e *serviceEntry) string  { return e.pkg.ID + "RootPassword@" + e.pkg.Version }
func mysqlBootstrapKey(e *serviceEntry) string { return e.pkg.ID + "Bootstrap@" + e.pkg.Version }
func (c *Core) initializeMySQLCredentials(ctx context.Context, e *serviceEntry) error {
	pending, hasPending, err := c.Store.setting(ctx, mysqlPasswordKey(e)+".pending")
	if err != nil {
		return err
	}
	if hasPending {
		db, probeErr := c.mysqlConnection(ctx, e, pending)
		if probeErr == nil {
			db.Close()
			if err = c.Store.saveSetting(ctx, mysqlPasswordKey(e), pending); err != nil {
				return err
			}
			_, err = c.Store.db.ExecContext(ctx, "DELETE FROM settings WHERE key IN (?,?)", mysqlPasswordKey(e)+".pending", mysqlBootstrapKey(e))
			return err
		}
	}
	saved, found, err := c.Store.setting(ctx, mysqlPasswordKey(e))
	if err != nil {
		return err
	}
	if found {
		db, err := c.mysqlConnection(ctx, e, saved)
		if err == nil {
			db.Close()
			_, err = c.Store.db.ExecContext(ctx, "DELETE FROM settings WHERE key=?", mysqlBootstrapKey(e))
			return err
		}
	}
	c.services.mu.RLock()
	fresh := e.resolved.freshDatabase
	c.services.mu.RUnlock()
	if !fresh {
		return &AppError{Code: "MYSQL_AUTH_REQUIRED", Message: "数据库已启动，请在数据库页验证并保存 root 密码"}
	}
	db, err := c.mysqlConnection(ctx, e, "")
	if err != nil {
		return err
	}
	defer db.Close()
	secret := make([]byte, 24)
	if _, err = rand.Read(secret); err != nil {
		return err
	}
	password := hex.EncodeToString(secret)
	// Persist first so an interruption after ALTER USER does not lose credentials.
	if err = c.Store.saveSetting(ctx, mysqlPasswordKey(e), password); err != nil {
		return err
	}
	if _, err = db.ExecContext(ctx, "ALTER USER 'root'@'localhost' IDENTIFIED BY '"+password+"'"); err != nil {
		return err
	}
	c.services.mu.Lock()
	e.resolved.freshDatabase = false
	c.services.mu.Unlock()
	_, err = c.Store.db.ExecContext(ctx, "DELETE FROM settings WHERE key=?", mysqlBootstrapKey(e))
	return err
}
func (c *Core) shutdownMySQL(ctx context.Context, e *serviceEntry) error {
	password, found, err := c.Store.setting(ctx, mysqlPasswordKey(e))
	if err != nil {
		return err
	}
	if !found {
		return &AppError{Code: "MYSQL_AUTH_REQUIRED", Message: "请先保存有效 root 凭据，再正常停止数据库"}
	}
	db, err := c.mysqlConnection(ctx, e, password)
	if err != nil {
		return err
	}
	defer db.Close()
	_, err = db.ExecContext(ctx, "SHUTDOWN")
	if err != nil {
		// Some MySQL builds close the protocol before acknowledging shutdown.
		c.services.mu.RLock()
		done := e.done
		c.services.mu.RUnlock()
		select {
		case <-done:
			return nil
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(time.Second):
		}
		return &AppError{Code: "MYSQL_SHUTDOWN_FAILED", Message: "MySQL 未确认正常停机，未强制结束进程", Detail: err.Error()}
	}
	return nil
}

func (c *Core) selectedMySQL(ctx context.Context, engine, version string) (*serviceEntry, error) {
	if engine == "" {
		engine = "mysql"
	}
	if !isMySQLEngine(engine) {
		return nil, invalid("此操作要求 MySQL 或 MariaDB 实例")
	}
	if version == "" {
		var err error
		version, _, err = c.Store.setting(ctx, "active"+engine+"Version")
		if err != nil {
			return nil, err
		}
		if version == "" {
			for _, s := range c.serviceStatuses() {
				if strings.HasPrefix(s.ID, engine+"@") && (version == "" || newerVersion(s.Version, version)) {
					version = s.Version
				}
			}
		}
	}
	if !safePart(version) {
		return nil, invalid("请选择已安装的 MySQL 版本")
	}
	return c.serviceEntry(engine+"@"+version, version)
}

func databaseIdentifier(name string) (string, error) {
	if name == "" || len(name) > 64 || strings.ContainsAny(name, "\x00/\\.\r\n") || strings.TrimSpace(name) != name {
		return "", invalid("数据库名称无效")
	}
	return "`" + strings.ReplaceAll(name, "`", "``") + "`", nil
}
func protectedDatabase(name string) bool {
	switch strings.ToLower(name) {
	case "mysql", "sys", "information_schema", "performance_schema":
		return true
	}
	return false
}
