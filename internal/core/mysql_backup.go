package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode"

	"niceenv/internal/process"
)

type mysqlBackupRequest struct {
	Engine       string   `json:"engine"`
	Version      string   `json:"version"`
	Databases    []string `json:"databases"`
	OutName      *string  `json:"outName"`
	Path         string   `json:"path"`
	Database     *string  `json:"database"`
	SafetyBackup *bool    `json:"safetyBackup"`
}

func (c *Core) mysqlBackupDir() (string, error) {
	dir := filepath.Join(c.Home, "backup", "db")
	return dir, managedDirectory(c.Home, dir)
}
func validBackupFilename(name, extension string) bool {
	return name != "" && len(name) <= 240 && name == filepath.Base(name) && !strings.ContainsAny(name, `/\:*?"<>|`) && !strings.Contains(name, "..") && strings.IndexFunc(name, unicode.IsControl) < 0 && strings.HasSuffix(strings.ToLower(name), extension)
}
func (c *Core) mysqlBackupList() ([]databaseBackupFile, error) {
	dir, err := c.mysqlBackupDir()
	if err != nil {
		return nil, err
	}
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	out := []databaseBackupFile{}
	for _, entry := range entries {
		if !entry.Type().IsRegular() || !validBackupFilename(entry.Name(), ".sql") {
			continue
		}
		info, err := entry.Info()
		if err != nil {
			return nil, err
		}
		out = append(out, databaseBackupFile{Name: entry.Name(), Path: Path(filepath.Join(dir, entry.Name())), SizeBytes: info.Size(), CreatedAt: info.ModTime().Unix()})
	}
	sort.Slice(out, func(i, j int) bool {
		if out[i].CreatedAt != out[j].CreatedAt {
			return out[i].CreatedAt > out[j].CreatedAt
		}
		return out[i].Name > out[j].Name
	})
	return out, nil
}
func (c *Core) mysqlBackupDelete(path string) error {
	dir, err := c.mysqlBackupDir()
	if err != nil {
		return err
	}
	path = DisplayPath(path)
	if !filepath.IsAbs(path) || projectPathKey(filepath.Dir(path)) != projectPathKey(dir) || !validBackupFilename(filepath.Base(path), ".sql") {
		return invalid("只能删除备份目录中的 SQL 文件")
	}
	info, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() {
		return invalid("不能删除目录或链接文件")
	}
	return os.Remove(path)
}
func (c *Core) mysqlBackupProgress(database, state, message string, size int64) {
	if c.emit == nil {
		return
	}
	var total *int64
	if state == "done" {
		total = &size
	}
	c.emit("db://backup", map[string]any{"database": database, "state": state, "message": message, "bytes": size, "total": total})
}
func (c *Core) registerMySQLBackups() {
	c.handle("db_backup_dir", func(context.Context, json.RawMessage) (any, error) {
		dir, err := c.mysqlBackupDir()
		return Path(dir), err
	})
	c.handle("db_backup_list", func(context.Context, json.RawMessage) (any, error) { return c.mysqlBackupList() })
	c.handle("db_backup_delete", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlBackupCommand(ctx, data, "delete")
	})
	c.handle("db_backup_dump", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlBackupCommand(ctx, data, "dump")
	})
	c.handle("db_backup_restore", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlBackupCommand(ctx, data, "restore")
	})
}
func (c *Core) mysqlBackupCommand(ctx context.Context, data json.RawMessage, action string) (result any, resultErr error) {
	in, err := decode[mysqlBackupRequest](data)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	if action == "delete" {
		err := c.mysqlBackupDelete(in.Path)
		return err == nil, err
	}
	e, err := c.selectedMySQL(ctx, in.Engine, in.Version)
	if err != nil {
		return nil, err
	}
	password, found, err := c.Store.setting(ctx, mysqlPasswordKey(e))
	if err != nil {
		return nil, err
	}
	if !found {
		return nil, invalid("请先验证并保存此实例的 root 凭据")
	}
	ctx, cancel := context.WithTimeout(ctx, 30*time.Minute)
	defer cancel()
	db, err := c.mysqlConnection(ctx, e, password)
	if err != nil {
		return nil, err
	}
	defer db.Close()
	defer func() {
		if resultErr != nil {
			c.mysqlBackupProgress("", "error", "备份操作未完成", 0)
		}
	}()
	if action == "dump" {
		name := ""
		if in.OutName != nil {
			name = *in.OutName
		}
		path, err := c.mysqlDump(ctx, e, db, password, in.Databases, name, false, false)
		return Path(path), err
	}
	if action == "restore" {
		return c.mysqlRestore(ctx, e, db, password, in)
	}
	return nil, invalid("未知数据库备份操作")
}
func (c *Core) mysqlTool(ctx context.Context, e *serviceEntry, password, tool string, args []string, input *os.File, output io.Writer, progress func()) error {
	if err := c.verifyDatabaseListener(ctx, e); err != nil {
		return err
	}
	c.services.mu.RLock()
	r := *e.resolved
	c.services.mu.RUnlock()
	if e.pkg.ID == "mariadb" {
		if tool == "mysql" {
			tool = "mariadb"
		} else if tool == "mysqldump" {
			tool = "mariadb-dump"
		}
	}
	if runtime.GOOS == "windows" {
		tool += ".exe"
	}
	program := filepath.Join(filepath.Dir(r.program), tool)
	info, err := os.Lstat(program)
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() {
		return invalid("数据库备份工具不是普通文件")
	}
	private, err := os.MkdirTemp("", "niceenv-mysql-client-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(private)
	passfile := filepath.Join(private, "client.cnf")
	// Option-file quoted values require escaping both backslash and quotes;
	// newlines remain escapes and cannot inject additional client settings.
	secret := strings.NewReplacer(`\`, `\\`, `"`, `\"`, "\n", `\n`, "\r", `\r`, "\t", `\t`).Replace(password)
	text := "[client]\nhost=127.0.0.1\nport=" + strconv.Itoa(r.port) + "\nuser=root\npassword=\"" + secret + "\"\nprotocol=tcp\ndefault-character-set=utf8mb4\n"
	if err = os.WriteFile(passfile, []byte(text), 0600); err != nil {
		return err
	}
	arguments := append([]string{"--defaults-file=" + DisplayPath(passfile)}, args...)
	remove := []string{"MYSQL_PWD", "MYSQL_HOST", "MYSQL_TCP_PORT", "MYSQL_UNIX_PORT", "MYSQL_HOME", "MARIADB_HOME", "LIBMYSQL_PLUGIN_DIR"}
	diagnostic := process.NewLimitedBuffer(64 * 1024)
	child, err := process.Start(ctx, process.Spec{Program: program, Directory: r.cwd, Args: arguments, Stdin: input, Stdout: output, Stderr: diagnostic, RemoveEnvironment: remove, Environment: map[string]string{"MYSQL_TEST_LOGIN_FILE": filepath.Join(private, "absent-login.cnf")}})
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
				detail := diagnostic.String()
				if password != "" {
					detail = strings.ReplaceAll(detail, password, "***")
				}
				if result.Err != nil {
					detail += "；" + result.Err.Error()
				}
				return &AppError{Code: "DATABASE_BACKUP_FAILED", Message: "数据库备份工具执行失败", Detail: redactDiagnostic(detail)}
			}
			return nil
		case <-ticker.C:
			if progress != nil {
				progress()
			}
		}
	}
}
func (c *Core) mysqlDump(ctx context.Context, e *serviceEntry, db *sql.DB, password string, databases []string, name string, safety, automatic bool) (string, error) {
	if len(databases) == 0 || len(databases) > 1000 {
		return "", invalid("请选择 1–1000 个业务数据库")
	}
	available, err := mysqlStrings(ctx, db, "SHOW DATABASES")
	if err != nil {
		return "", err
	}
	known := map[string]bool{}
	for _, name := range available {
		known[name] = true
	}
	seen := map[string]bool{}
	for _, name := range databases {
		if _, err = databaseIdentifier(name); err != nil {
			return "", err
		}
		if protectedDatabase(name) || !known[name] || strings.HasPrefix(name, "-") || seen[name] {
			return "", invalid("数据库已变化、重复或属于系统库，请刷新后选择")
		}
		seen[name] = true
	}
	dir, err := c.mysqlBackupDir()
	if err != nil {
		return "", err
	}
	if name == "" {
		name, err = newID("dump")
		if err != nil {
			return "", err
		}
		name += ".sql"
	}
	if !validBackupFilename(name, ".sql") {
		return "", invalid("备份名称必须是单个 .sql 文件名")
	}
	path := filepath.Join(dir, e.pkg.ID+"-"+e.pkg.Version+"-"+name)
	if _, err = os.Lstat(path); !os.IsNotExist(err) {
		return "", invalid("同名备份已存在或无法确认，未覆盖")
	}
	pending, err := os.CreateTemp(dir, ".dump-")
	if err != nil {
		return "", err
	}
	defer os.Remove(pending.Name())
	defer pending.Close()
	args := []string{"--single-transaction", "--routines", "--triggers", "--events", "--hex-blob", "--no-tablespaces"}
	if e.pkg.ID == "mysql" {
		args = append(args, "--set-gtid-purged=OFF")
	}
	major, _ := strconv.Atoi(strings.Split(e.pkg.Version, ".")[0])
	if e.pkg.ID == "mysql" && major >= 8 {
		args = append(args, "--column-statistics=0")
	}
	args = append(args, "--databases", "--")
	args = append(args, databases...)
	label := strings.Join(databases, ", ")
	message := "正在导出数据库"
	if safety {
		message = "正在创建恢复前备份"
	}
	c.mysqlBackupProgress(label, "running", message, 0)
	if err = c.mysqlTool(ctx, e, password, "mysqldump", args, nil, pending, func() {
		if info, err := pending.Stat(); err == nil {
			c.mysqlBackupProgress(label, "running", message, info.Size())
		}
	}); err != nil {
		return "", err
	}
	if automatic {
		if len(databases) != 1 {
			return "", invalid("自动备份一次只能处理一个数据库")
		}
		if _, err = pending.WriteString(mysqlAutomaticMarker(e.pkg.ID, e.pkg.Version, databases[0])); err != nil {
			return "", err
		}
	}
	if err = pending.Sync(); err != nil {
		return "", err
	}
	info, err := pending.Stat()
	if err != nil {
		return "", err
	}
	if info.Size() == 0 {
		return "", invalid("备份内容为空，未发布")
	}
	if err = pending.Close(); err != nil {
		return "", err
	}
	if err = os.Link(pending.Name(), path); err != nil {
		return "", err
	}
	state := "done"
	if safety {
		state = "running"
	}
	c.mysqlBackupProgress(label, state, message, info.Size())
	return path, nil
}
func (c *Core) mysqlRestore(ctx context.Context, e *serviceEntry, db *sql.DB, password string, in mysqlBackupRequest) (any, error) {
	path := DisplayPath(in.Path)
	if !filepath.IsAbs(path) || !strings.EqualFold(filepath.Ext(path), ".sql") {
		return nil, invalid("请选择 SQL 备份文件的绝对路径")
	}
	dir, err := c.mysqlBackupDir()
	if err != nil {
		return nil, err
	}
	snapshot, err := os.CreateTemp(dir, ".restore-")
	if err != nil {
		return nil, err
	}
	defer os.Remove(snapshot.Name())
	defer snapshot.Close()
	digest, err := copyRDB(ctx, path, snapshot, false)
	if err != nil {
		return nil, err
	}
	if digest.Size == 0 {
		return nil, invalid("备份文件为空，未执行恢复")
	}
	if _, err = snapshot.Seek(0, io.SeekStart); err != nil {
		return nil, err
	}
	databases, err := mysqlStrings(ctx, db, "SHOW DATABASES")
	if err != nil {
		return nil, err
	}
	if in.Database != nil {
		found := false
		for _, name := range databases {
			found = found || name == *in.Database
		}
		if !found || protectedDatabase(*in.Database) {
			return nil, invalid("请选择当前实例中的业务数据库")
		}
	}
	var safety *Path
	if in.SafetyBackup == nil || *in.SafetyBackup {
		selected := []string{}
		for _, name := range databases {
			if !protectedDatabase(name) {
				selected = append(selected, name)
			}
		}
		if len(selected) > 0 {
			name, err := newID("pre-restore")
			if err != nil {
				return nil, err
			}
			file, err := c.mysqlDump(ctx, e, db, password, selected, name+".sql", true, false)
			if err != nil {
				return nil, &AppError{Code: "SAFETY_BACKUP_FAILED", Message: "恢复前备份失败，已中止恢复", Detail: err.Error()}
			}
			saved := Path(file)
			safety = &saved
		}
	}
	args := []string{"--binary-mode", "--local-infile=0", "--connect-timeout=5"}
	if in.Database != nil {
		args = append(args, "--database="+*in.Database)
	}
	c.mysqlBackupProgress(filepath.Base(path), "running", "正在执行 SQL，耗时取决于数据量", 0)
	if err = c.mysqlTool(ctx, e, password, "mysql", args, snapshot, io.Discard, nil); err != nil {
		hint := "当前没有恢复前备份"
		if safety != nil {
			hint = "恢复前备份：" + DisplayPath(string(*safety))
		}
		return nil, &AppError{Code: "RESTORE_FAILED", Message: "恢复未完成，部分 SQL 可能已执行，请检查数据库", Hint: hint, Detail: err.Error()}
	}
	c.mysqlBackupProgress(filepath.Base(path), "done", "SQL 恢复完成", digest.Size)
	return map[string]any{"ok": true, "safetyBackup": safety}, nil
}
