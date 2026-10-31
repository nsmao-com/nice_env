package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"time"
	"unicode"

	mysql "github.com/go-sql-driver/mysql"
)

type sourceDatabaseRequest struct {
	Host      string   `json:"host"`
	Port      int      `json:"port"`
	User      string   `json:"user"`
	Password  string   `json:"password"`
	Version   string   `json:"version"`
	Engine    string   `json:"engine"`
	Databases []string `json:"databases"`
}
type sourceDatabase struct {
	Name   string `json:"name"`
	SizeKB uint64 `json:"sizeKb"`
}

func sourceDatabaseConnection(ctx context.Context, in sourceDatabaseRequest) (*sql.DB, error) {
	if in.Host == "" || len(in.Host) > 253 || strings.ContainsAny(in.Host, "/\\@\x00\r\n") || in.Port < 1 || in.Port > 65535 || in.User == "" || len(in.User) > 128 || strings.IndexFunc(in.User, unicode.IsControl) >= 0 || strings.ContainsRune(in.Password, 0) || len(in.Password) > 4096 {
		return nil, invalid("源数据库地址、端口或账号无效")
	}
	cfg := mysql.NewConfig()
	cfg.User = in.User
	cfg.Passwd = in.Password
	cfg.Net = "tcp"
	cfg.Addr = net.JoinHostPort(in.Host, strconv.Itoa(in.Port))
	cfg.Timeout = 5 * time.Second
	cfg.ReadTimeout = 30 * time.Second
	cfg.WriteTimeout = 10 * time.Second
	connector, err := mysql.NewConnector(cfg)
	if err != nil {
		return nil, invalid("源数据库连接参数无效")
	}
	db := sql.OpenDB(connector)
	db.SetMaxOpenConns(1)
	if err = db.PingContext(ctx); err != nil {
		db.Close()
		return nil, &AppError{Code: "SOURCE_CONNECTION_FAILED", Message: "无法连接源数据库，请确认来源服务、地址、账号和密码"}
	}
	return db, nil
}
func listSourceDatabases(ctx context.Context, db *sql.DB) ([]sourceDatabase, error) {
	rows, err := db.QueryContext(ctx, `SELECT schema_name, COALESCE(SUM(data_length+index_length),0) FROM information_schema.schemata LEFT JOIN information_schema.tables ON table_schema=schema_name GROUP BY schema_name ORDER BY schema_name`)
	if err != nil {
		return nil, &AppError{Code: "SOURCE_QUERY_FAILED", Message: "读取源数据库列表失败，请检查账号权限"}
	}
	defer rows.Close()
	result := []sourceDatabase{}
	for rows.Next() {
		var item sourceDatabase
		var size uint64
		if err = rows.Scan(&item.Name, &size); err != nil {
			return nil, err
		}
		item.SizeKB = size / 1024
		if !protectedDatabase(item.Name) {
			result = append(result, item)
		}
	}
	return result, rows.Err()
}
func (c *Core) registerDatabaseImport() {
	c.handle("migrate_list_source", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[sourceDatabaseRequest](data)
		if err != nil {
			return nil, err
		}
		db, err := sourceDatabaseConnection(ctx, in)
		if err != nil {
			return nil, err
		}
		defer db.Close()
		return listSourceDatabases(ctx, db)
	})
	c.handle("migrate_import", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[sourceDatabaseRequest](data)
		if err != nil {
			return nil, err
		}
		if len(in.Databases) < 1 || len(in.Databases) > 1000 {
			return nil, invalid("请选择 1–1000 个源业务数据库")
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		e, err := c.selectedMySQL(ctx, in.Engine, in.Version)
		if err != nil {
			return nil, err
		}
		password, found, err := c.Store.setting(ctx, mysqlPasswordKey(e))
		if err != nil {
			return nil, err
		}
		if !found {
			return nil, invalid("请先验证目标实例的 root 凭据")
		}
		target, err := c.mysqlConnection(ctx, e, password)
		if err != nil {
			return nil, err
		}
		defer target.Close()
		source, err := sourceDatabaseConnection(ctx, in)
		if err != nil {
			return nil, err
		}
		defer source.Close()
		available, err := listSourceDatabases(ctx, source)
		if err != nil {
			return nil, err
		}
		known := map[string]bool{}
		for _, db := range available {
			known[db.Name] = true
		}
		seen := map[string]bool{}
		for _, name := range in.Databases {
			if _, err = databaseIdentifier(name); err != nil {
				return nil, err
			}
			if !known[name] || seen[name] || protectedDatabase(name) || strings.HasPrefix(name, "-") {
				return nil, invalid("所选源数据库重复、无效或已不存在，请重新检测")
			}
			seen[name] = true
		}
		const identity = "SELECT CONCAT(@@hostname,CHAR(0),@@datadir,CHAR(0),@@port)"
		var sourceID, targetID string
		if err = source.QueryRowContext(ctx, identity).Scan(&sourceID); err != nil {
			return nil, &AppError{Code: "SOURCE_IDENTITY", Message: "无法确认源数据库身份"}
		}
		if err = target.QueryRowContext(ctx, identity).Scan(&targetID); err != nil {
			return nil, err
		}
		if sourceID == targetID {
			return nil, &AppError{Code: "SAME_MYSQL_INSTANCE", Message: "源和目标是同一个数据库实例，请选择其它来源"}
		}
		c.services.mu.RLock()
		r := *e.resolved
		c.services.mu.RUnlock()
		tool := "mysqldump"
		if e.pkg.ID == "mariadb" {
			tool = "mariadb-dump"
		}
		if runtime.GOOS == "windows" {
			tool += ".exe"
		}
		program := filepath.Join(filepath.Dir(r.program), tool)
		info, err := os.Lstat(program)
		if err != nil {
			return nil, err
		}
		if !info.Mode().IsRegular() {
			return nil, invalid("源库导出工具不是普通文件")
		}
		pending, err := os.CreateTemp("", "niceenv-import-*.sql")
		if err != nil {
			return nil, err
		}
		defer os.Remove(pending.Name())
		defer pending.Close()
		args := []string{"--single-transaction", "--routines", "--triggers", "--events", "--hex-blob", "--no-tablespaces"}
		if e.pkg.ID == "mysql" {
			args = append(args, "--set-gtid-purged=OFF")
			major, _ := strconv.Atoi(strings.Split(e.pkg.Version, ".")[0])
			if major >= 8 {
				args = append(args, "--column-statistics=0")
			}
		}
		args = append(args, "--databases", "--")
		args = append(args, in.Databases...)
		for _, name := range in.Databases {
			if c.emit != nil {
				c.emit("migration://progress", map[string]string{"database": name, "state": "exporting"})
			}
		}
		if err = c.mysqlClient(ctx, program, r.cwd, in.Host, in.Port, in.User, in.Password, args, nil, pending, nil); err != nil {
			detail := err.Error()
			var toolErr *AppError
			if errors.As(err, &toolErr) && toolErr.Detail != "" {
				detail += ": " + toolErr.Detail
			}
			return nil, &AppError{Code: "SOURCE_DUMP_FAILED", Message: "源数据库导出失败，目标数据库未改动", Detail: detail}
		}
		if err = pending.Sync(); err != nil {
			return nil, err
		}
		if info, err = pending.Stat(); err != nil || info.Size() == 0 {
			return nil, invalid("源数据库导出内容为空，未导入")
		}
		if err = pending.Close(); err != nil {
			return nil, err
		}
		for _, name := range in.Databases {
			if c.emit != nil {
				c.emit("migration://progress", map[string]string{"database": name, "state": "importing"})
			}
		}
		_, restoreErr := c.mysqlRestore(ctx, e, target, password, mysqlBackupRequest{Path: pending.Name()})
		imported := []string{}
		failed := [][2]string{}
		for _, name := range in.Databases {
			state := "imported"
			if restoreErr != nil {
				state = "failed"
				failed = append(failed, [2]string{name, restoreErr.Error()})
			} else {
				imported = append(imported, name)
			}
			if c.emit != nil {
				c.emit("migration://progress", map[string]string{"database": name, "state": state})
			}
		}
		return map[string]any{"imported": imported, "failed": failed}, nil
	})
}
