package core

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"niceenv/internal/process"
)

type databaseBackupFile struct {
	Name      string `json:"name"`
	Path      Path   `json:"path"`
	SizeBytes int64  `json:"sizeBytes"`
	CreatedAt int64  `json:"createdAt"`
}
type postgresRestoreInput struct {
	Path          string `json:"path"`
	Name          string `json:"name"`
	OID           uint32 `json:"oid"`
	Owner         string `json:"owner"`
	ConfirmedName string `json:"confirmedName"`
	Trusted       bool   `json:"trusted"`
}
type postgresBackupRequest struct {
	Version     string `json:"version"`
	OperationID string `json:"operationId"`
	postgresRestoreInput
	Input postgresRestoreInput `json:"input"`
}

func (c *Core) postgresBackupDir() (string, error) {
	dir := filepath.Join(c.Home, "backup", "postgresql")
	return dir, managedDirectory(c.Home, dir)
}
func (c *Core) postgresBackupPath(name string) (string, error) {
	if !validBackupFilename(name, ".dump") {
		return "", invalid("请选择备份目录内的 PostgreSQL .dump 文件")
	}
	dir, err := c.postgresBackupDir()
	return filepath.Join(dir, name), err
}
func (c *Core) postgresBackupList() ([]databaseBackupFile, error) {
	dir, err := c.postgresBackupDir()
	if err != nil {
		return nil, err
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	files := []databaseBackupFile{}
	for _, entry := range entries {
		if !entry.Type().IsRegular() || !validBackupFilename(entry.Name(), ".dump") {
			continue
		}
		info, err := entry.Info()
		if err != nil {
			return nil, err
		}
		files = append(files, databaseBackupFile{Name: entry.Name(), Path: Path(filepath.Join(dir, entry.Name())), SizeBytes: info.Size(), CreatedAt: info.ModTime().Unix()})
	}
	sort.Slice(files, func(i, j int) bool {
		if files[i].CreatedAt != files[j].CreatedAt {
			return files[i].CreatedAt > files[j].CreatedAt
		}
		return files[i].Name > files[j].Name
	})
	return files, nil
}
func (c *Core) postgresBackupDelete(name string) error {
	path, err := c.postgresBackupPath(name)
	if err != nil {
		return err
	}
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() {
		return invalid("只能删除普通备份文件")
	}
	return os.Remove(path)
}
func (c *Core) postgresBackupProgress(operation, database, state, message string, size int64) {
	if c.emit == nil || operation == "" {
		return
	}
	var total *int64
	if state == "done" {
		total = &size
	}
	c.emit("postgres://backup", map[string]any{"operationId": operation, "database": database, "state": state, "message": message, "bytes": size, "total": total})
}

func (c *Core) registerPostgresBackups() {
	c.handle("postgres_backup_dir", func(context.Context, json.RawMessage) (any, error) {
		dir, err := c.postgresBackupDir()
		return Path(dir), err
	})
	c.handle("postgres_backup_list", func(context.Context, json.RawMessage) (any, error) { return c.postgresBackupList() })
	c.handle("postgres_backup_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresBackupCommand(ctx, data, "delete")
	})
	c.handle("postgres_backup_dump", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresBackupCommand(ctx, data, "dump")
	})
	c.handle("postgres_backup_restore", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresBackupCommand(ctx, data, "restore")
	})
	c.handle("postgres_backup_replace", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresBackupCommand(ctx, data, "replace")
	})
}
func (c *Core) postgresBackupCommand(ctx context.Context, data json.RawMessage, action string) (result any, resultErr error) {
	in, err := decode[postgresBackupRequest](data)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	if action == "delete" {
		return nil, c.postgresBackupDelete(in.Name)
	}
	if !safePart(in.Version) || !cronID.MatchString(in.OperationID) {
		return nil, invalid("版本或备份操作标识无效")
	}
	ctx, cancel := context.WithTimeout(ctx, 30*time.Minute)
	defer cancel()
	e, conn, password, err := c.postgresSelected(ctx, in.Version, nil, "postgres")
	if err != nil {
		return nil, err
	}
	defer closePostgres(conn)
	defer func() {
		if resultErr != nil {
			resultErr = postgresError(resultErr, password)
			c.postgresBackupProgress(in.OperationID, in.Name, "error", "备份操作未完成", 0)
		}
	}()
	switch action {
	case "dump":
		path, err := c.postgresDump(ctx, e, conn, password, in.Name, in.OID, in.OperationID, false)
		return Path(path), err
	case "restore":
		_, err := c.postgresRestore(ctx, e, conn, password, in.postgresRestoreInput, in.OperationID)
		return nil, err
	case "replace":
		return c.postgresReplace(ctx, e, conn, password, in.Input, in.OperationID)
	}
	return nil, invalid("未知备份操作")
}

// Credentials and Unicode database names use libpq environment settings, never
// shell arguments. Each invocation clears inherited PG* redirection options.
func (c *Core) postgresTool(ctx context.Context, e *serviceEntry, password, database, tool string, args []string, input *os.File, output io.Writer, options string, progress func()) error {
	if err := c.verifyDatabaseListener(ctx, e); err != nil {
		return err
	}
	c.services.mu.RLock()
	r := *e.resolved
	c.services.mu.RUnlock()
	program, err := postgresProgram(&r, tool)
	if err != nil {
		return err
	}
	private, err := os.MkdirTemp("", "niceenv-pgpass-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(private)
	passfile := filepath.Join(private, "pgpass")
	escape := func(value string) string { return strings.ReplaceAll(strings.ReplaceAll(value, `\`, `\\`), ":", `\:`) }
	if err = os.WriteFile(passfile, []byte("127.0.0.1:"+strconv.Itoa(r.port)+":*:postgres:"+escape(password)+"\n"), 0600); err != nil {
		return err
	}
	env := map[string]string{"PGHOST": "127.0.0.1", "PGPORT": strconv.Itoa(r.port), "PGUSER": "postgres", "PGDATABASE": database, "PGPASSFILE": DisplayPath(passfile), "PGSSLMODE": "disable", "PGCONNECT_TIMEOUT": "5", "PGCLIENTENCODING": "UTF8", "PGOPTIONS": options}
	remove := []string{}
	for _, key := range postgresRemoveEnvironment() {
		if _, ok := env[strings.ToUpper(key)]; !ok {
			remove = append(remove, key)
		}
	}
	diagnostic := process.NewLimitedBuffer(64 * 1024)
	child, err := process.Start(ctx, process.Spec{Program: program, Directory: r.cwd, Args: args, Stdin: input, Stdout: output, Stderr: diagnostic, Environment: env, RemoveEnvironment: remove})
	if err != nil {
		return err
	}
	ticker := time.NewTicker(500 * time.Millisecond)
	defer ticker.Stop()
	for {
		select {
		case <-child.Done():
			result := child.Wait()
			if result.Err != nil || result.Code != 0 || ctx.Err() != nil {
				detail := strings.ReplaceAll(diagnostic.String(), password, "***")
				if result.Err != nil {
					detail += "；" + result.Err.Error()
				}
				return &AppError{Code: "POSTGRES_BACKUP_FAILED", Message: "PostgreSQL 备份工具未成功完成", Detail: redactDiagnostic(detail)}
			}
			return nil
		case <-ticker.C:
			if progress != nil {
				progress()
			}
		}
	}
}

func postgresBackupTarget(ctx context.Context, conn *pgx.Conn, name string, oid uint32) (postgresDatabase, error) {
	dbs, err := postgresJSON[[]postgresDatabase](ctx, conn, postgresDatabasesSQL)
	if err != nil {
		return postgresDatabase{}, err
	}
	for _, db := range dbs {
		if db.Name == name && db.OID == oid {
			if db.Protected || !db.AllowConnections {
				return db, invalid("请选择可连接的业务数据库")
			}
			return db, nil
		}
	}
	return postgresDatabase{}, invalid("数据库名称或标识已变化，请刷新后重新选择")
}
func backupLabel(value string) string {
	var b strings.Builder
	for _, ch := range value {
		if ch >= 'a' && ch <= 'z' || ch >= 'A' && ch <= 'Z' || ch >= '0' && ch <= '9' || ch == '_' || ch == '-' || ch == '.' {
			b.WriteRune(ch)
		} else {
			b.WriteByte('_')
		}
	}
	return b.String()
}

func fmtPostgresAutomaticPrefix(version string, oid uint32) string {
	return "auto-postgresql-" + backupLabel(version) + "-" + strconv.FormatUint(uint64(oid), 10) + "-"
}
func postgresArchiveMagic(file *os.File) error {
	var magic [5]byte
	if _, err := file.ReadAt(magic[:], 0); err != nil || string(magic[:]) != "PGDMP" {
		return invalid("文件不是完整的 PostgreSQL custom 归档")
	}
	return nil
}
func (c *Core) postgresDump(ctx context.Context, e *serviceEntry, conn *pgx.Conn, password, name string, oid uint32, operation string, automatic bool) (string, error) {
	if _, err := postgresBackupTarget(ctx, conn, name, oid); err != nil {
		return "", err
	}
	dir, err := c.postgresBackupDir()
	if err != nil {
		return "", err
	}
	pending, err := os.CreateTemp(dir, ".dump-")
	if err != nil {
		return "", err
	}
	defer os.Remove(pending.Name())
	defer pending.Close()
	c.postgresBackupProgress(operation, name, "running", "正在导出数据库", 0)
	err = c.postgresTool(ctx, e, password, name, "pg_dump", []string{"--no-password", "--format=custom", "--lock-wait-timeout=5000"}, nil, pending, "-c statement_timeout=1800000 -c lock_timeout=5000", func() {
		if info, err := pending.Stat(); err == nil {
			c.postgresBackupProgress(operation, name, "running", "正在导出数据库", info.Size())
		}
	})
	if err != nil {
		return "", err
	}
	if err = pending.Sync(); err != nil {
		return "", err
	}
	if err = postgresArchiveMagic(pending); err != nil {
		return "", err
	}
	info, err := pending.Stat()
	if err != nil {
		return "", err
	}
	if err = pending.Close(); err != nil {
		return "", err
	}
	prefix := "postgresql-" + backupLabel(e.pkg.Version)
	if automatic {
		prefix = "auto-" + prefix + "-" + strconv.FormatUint(uint64(oid), 10)
	}
	id, err := newID("archive")
	if err != nil {
		return "", err
	}
	path, err := c.postgresBackupPath(prefix + "-" + backupLabel(name) + "-" + id + ".dump")
	if err != nil {
		return "", err
	}
	// Hard-link publication is atomic and cannot overwrite an existing backup.
	if err = os.Link(pending.Name(), path); err != nil {
		return "", err
	}
	c.postgresBackupProgress(operation, name, "done", "数据库备份完成", info.Size())
	return path, nil
}

func (c *Core) postgresRestore(ctx context.Context, e *serviceEntry, conn *pgx.Conn, password string, in postgresRestoreInput, operation string) (postgresDatabase, error) {
	var empty postgresDatabase
	if !in.Trusted {
		return empty, invalid("请先确认备份来源可信；恢复会执行归档中的数据库代码")
	}
	name, err := postgresName(in.Name, true)
	if err != nil {
		return empty, err
	}
	owner, err := postgresName(in.Owner, false)
	if err != nil {
		return empty, err
	}
	roles, err := postgresJSON[[]postgresRole](ctx, conn, postgresRolesSQL)
	if err != nil {
		return empty, err
	}
	found := false
	for _, role := range roles {
		found = found || role.Name == in.Owner && role.CanLogin
	}
	if !found {
		return empty, invalid("所选所有者不存在或无法登录")
	}
	path := DisplayPath(in.Path)
	if !filepath.IsAbs(path) || !strings.EqualFold(filepath.Ext(path), ".dump") {
		return empty, invalid("请选择 PostgreSQL custom 格式的 .dump 文件")
	}
	dir, err := c.postgresBackupDir()
	if err != nil {
		return empty, err
	}
	snapshot, err := os.CreateTemp(dir, ".restore-")
	if err != nil {
		return empty, err
	}
	defer os.Remove(snapshot.Name())
	defer snapshot.Close()
	c.postgresBackupProgress(operation, in.Name, "running", "正在读取并校验备份", 0)
	if _, err = copyRDB(ctx, path, snapshot, false); err != nil {
		return empty, err
	}
	if err = postgresArchiveMagic(snapshot); err != nil {
		return empty, err
	}
	if _, err = snapshot.Seek(0, io.SeekStart); err != nil {
		return empty, err
	}
	if err = c.postgresTool(ctx, e, password, "postgres", "pg_restore", []string{"--list", "--format=custom"}, snapshot, io.Discard, "", nil); err != nil {
		return empty, err
	}
	if _, err = snapshot.Seek(0, io.SeekStart); err != nil {
		return empty, err
	}
	dbs, err := postgresJSON[[]postgresDatabase](ctx, conn, postgresDatabasesSQL)
	if err != nil {
		return empty, err
	}
	for _, db := range dbs {
		if db.Name == in.Name {
			return empty, invalid("同名数据库已存在，请选择新名称或使用替换功能")
		}
	}
	if _, err = conn.Exec(ctx, "CREATE DATABASE "+name+" OWNER "+owner+" TEMPLATE template0 ENCODING 'UTF8'"); err != nil {
		return empty, err
	}
	dbs, err = postgresJSON[[]postgresDatabase](ctx, conn, postgresDatabasesSQL)
	if err != nil {
		return empty, err
	}
	var staged postgresDatabase
	for _, db := range dbs {
		if db.Name == in.Name {
			staged = db
		}
	}
	if staged.OID == 0 {
		return empty, invalid("新数据库状态已变化，未开始恢复")
	}
	// PGOPTIONS splits at unescaped whitespace; GUC role values are names,
	// not SQL identifiers. Quotes would become part of the requested role.
	role := strings.NewReplacer(`\`, `\\`, " ", `\ `).Replace(in.Owner)
	options := "-c statement_timeout=1800000 -c lock_timeout=5000 -c role=" + role
	c.postgresBackupProgress(operation, in.Name, "running", "正在恢复到新数据库", 0)
	err = c.postgresTool(ctx, e, password, in.Name, "pg_restore", []string{"--no-password", "--dbname", "", "--format=custom", "--single-transaction", "--exit-on-error", "--no-owner", "--no-privileges", "--no-tablespaces"}, snapshot, io.Discard, options, nil)
	if err != nil {
		detail := err.Error()
		var native *AppError
		if errors.As(err, &native) && native.Detail != "" {
			detail += "：" + native.Detail
		}
		return staged, &AppError{Code: "POSTGRES_RESTORE_FAILED", Message: "恢复未完成，新建数据库已保留", Hint: "恢复使用单个事务；请核对数据库“" + in.Name + "”，重试时使用其他新名称", Detail: detail}
	}
	info, err := snapshot.Stat()
	if err != nil {
		return staged, err
	}
	c.postgresBackupProgress(operation, in.Name, "done", "数据库恢复完成", info.Size())
	return staged, nil
}

func postgresReplaceAvailable(ctx context.Context, conn *pgx.Conn, db postgresDatabase) error {
	var busy, replication bool
	err := conn.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datid=$1) OR EXISTS(SELECT 1 FROM pg_prepared_xacts WHERE database=$2), EXISTS(SELECT 1 FROM pg_subscription WHERE subdbid=$1) OR EXISTS(SELECT 1 FROM pg_replication_slots WHERE datoid=$1)`, db.OID, db.Name).Scan(&busy, &replication)
	if err != nil {
		return err
	}
	if busy {
		return invalid("目标数据库仍有连接或预备事务，请先处理后重试；不会强制断开连接")
	}
	if replication {
		return invalid("此数据库含复制订阅或复制槽，请恢复到新库后手动处理复制关系")
	}
	return nil
}
func (c *Core) postgresReplace(ctx context.Context, e *serviceEntry, conn *pgx.Conn, password string, in postgresRestoreInput, operation string) (any, error) {
	if !in.Trusted || in.Name == "" || in.Name != in.ConfirmedName {
		return nil, invalid("请确认备份来源可信并输入完整目标数据库名称")
	}
	target, err := postgresBackupTarget(ctx, conn, in.Name, in.OID)
	if err != nil {
		return nil, err
	}
	if err = postgresReplaceAvailable(ctx, conn, target); err != nil {
		return nil, err
	}
	suffix, err := databaseRandomPassword()
	if err != nil {
		return nil, err
	}
	stageName, previous := "niceenv_restore_"+suffix[:16], "niceenv_previous_"+suffix[:16]
	safety, err := c.postgresDump(ctx, e, conn, password, in.Name, in.OID, "", false)
	if err != nil {
		return nil, &AppError{Code: "SAFETY_BACKUP_FAILED", Message: "恢复前备份失败，已中止替换", Detail: err.Error()}
	}
	recovery := "恢复前备份：" + DisplayPath(safety) + "；暂存库：" + stageName + "；保留库：" + previous
	staged, err := c.postgresRestore(ctx, e, conn, password, postgresRestoreInput{Path: in.Path, Name: stageName, Owner: in.Owner, Trusted: true}, "")
	if err != nil {
		return nil, &AppError{Code: "POSTGRES_RESTORE_FAILED", Message: "恢复到暂存库失败，尚未切换数据库名称", Hint: recovery, Detail: err.Error()}
	}
	c.postgresBackupProgress(operation, in.Name, "running", "正在核对并切换数据库名称", 0)
	if err = postgresReplaceAvailable(ctx, conn, target); err != nil {
		return nil, &AppError{Code: "POSTGRES_DATABASE_BUSY", Message: "未切换数据库名称", Detail: err.Error(), Hint: recovery}
	}
	err = postgresSwap(ctx, conn, target, staged, previous)
	// Commit errors can mean the connection failed after a successful commit.
	// Reconnect and verify both OIDs before reporting either outcome.
	verifyCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	check, checkErr := c.postgresConnection(verifyCtx, e, password, "postgres")
	var dbs []postgresDatabase
	if checkErr == nil {
		defer closePostgres(check)
		dbs, checkErr = postgresJSON[[]postgresDatabase](verifyCtx, check, postgresDatabasesSQL)
	}
	switched, retained, original := false, false, false
	for _, db := range dbs {
		switched = switched || db.Name == target.Name && db.OID == staged.OID
		retained = retained || db.Name == previous && db.OID == target.OID
		original = original || db.Name == target.Name && db.OID == target.OID
	}
	if !switched || !retained {
		message := "未能确认数据库替换结果，请刷新核对后操作"
		if original {
			message = "替换失败，已核对原数据库仍在原名称下"
		}
		detail := "数据库名称或标识与预期不一致"
		if combined := errors.Join(err, checkErr); combined != nil {
			detail = combined.Error()
		}
		return nil, &AppError{Code: "POSTGRES_REPLACE_UNCONFIRMED", Message: message, Hint: recovery, Detail: detail}
	}
	c.postgresBackupProgress(operation, in.Name, "done", recovery, 0)
	return map[string]any{"database": in.Name, "previousDatabase": previous, "safetyBackup": Path(safety)}, nil
}
func postgresSwap(ctx context.Context, conn *pgx.Conn, target, staged postgresDatabase, previous string) error {
	tx, err := conn.Begin(ctx)
	if err != nil {
		return err
	}
	defer tx.Rollback(context.Background())
	for _, step := range []struct {
		db   postgresDatabase
		next string
	}{{target, previous}, {staged, target.Name}} {
		name, err := postgresName(step.db.Name, false)
		if err != nil {
			return err
		}
		next, err := postgresName(step.next, false)
		if err != nil {
			return err
		}
		if _, err = tx.Exec(ctx, "ALTER DATABASE "+name+" RENAME TO "+next); err != nil {
			return err
		}
		var valid bool
		if err = tx.QueryRow(ctx, `SELECT EXISTS(SELECT 1 FROM pg_database WHERE oid=$1 AND datname=$2 AND NOT datistemplate AND datallowconn) AND NOT EXISTS(SELECT 1 FROM pg_subscription WHERE subdbid=$1) AND NOT EXISTS(SELECT 1 FROM pg_replication_slots WHERE datoid=$1)`, step.db.OID, step.next).Scan(&valid); err != nil {
			return err
		}
		if !valid {
			return fmt.Errorf("数据库 %s 的标识或复制状态已变化", step.db.Name)
		}
	}
	return tx.Commit(ctx)
}
