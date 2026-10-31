package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"time"

	bolt "go.etcd.io/bbolt"
	"gopkg.in/yaml.v3"
)

// Local account paths are database fields, not arbitrary configuration text.
// Only the stopped provider's staging copy is modified.
func (c *Core) rebaseSftpgoData(ctx context.Context, stage, target string) ([]string, error) {
	protected := []string{}
	c.services.mu.RLock()
	entry := c.services.entries["sftpgo"]
	c.services.mu.RUnlock()
	if entry == nil {
		configs, err := os.ReadDir(filepath.Join(c.Home, "etc", "sftpgo"))
		if err != nil && !os.IsNotExist(err) {
			return nil, err
		}
		if len(configs) > 0 {
			return nil, invalid("存在未注册的 SFTPGo 配置，请先重新安装对应版本以确认账号库，再迁移数据目录")
		}
		return protected, nil
	}
	r, err := c.resolveService(ctx, entry)
	if err != nil {
		return nil, err
	}
	directories := []string{r.etc}
	entries, err := os.ReadDir(filepath.Join(c.Home, "etc", "sftpgo"))
	if err != nil && !os.IsNotExist(err) {
		return nil, err
	}
	for _, item := range entries {
		if item.IsDir() {
			dir := filepath.Join(c.Home, "etc", "sftpgo", item.Name())
			if !slices.Contains(directories, dir) {
				directories = append(directories, dir)
			}
		}
	}
	for _, dir := range directories {
		copy := *r
		copy.etc = dir
		paths, err := c.rebaseSftpgoDirectory(ctx, stage, target, &copy)
		if err != nil {
			return nil, err
		}
		protected = append(protected, paths...)
	}
	return protected, nil
}

func (c *Core) rebaseSftpgoDirectory(ctx context.Context, stage, target string, r *resolvedService) ([]string, error) {
	protected := []string{}
	if !pathWithin(c.Home, r.etc) {
		return nil, invalid("SFTPGo 配置目录不在应用数据目录内")
	}
	configFile, err := sftpgoConfigFile(r.etc)
	if err != nil || configFile == "" {
		return protected, err
	}
	text, err := readConfigText(configFile)
	if err != nil {
		return nil, err
	}
	config := map[string]any{}
	switch strings.ToLower(filepath.Ext(configFile)) {
	case ".json":
		err = json.Unmarshal([]byte(text), &config)
	case ".yaml", ".yml":
		err = yaml.Unmarshal([]byte(text), &config)
	default:
		return nil, invalid("迁移 SFTPGo 需要可完整解析的 JSON 或 YAML 配置")
	}
	if err != nil {
		return nil, err
	}
	env, err := c.sftpgoEnvironment(r)
	if err != nil {
		return nil, err
	}
	get := func(field, fallback string) (string, error) {
		if value, ok := env["SFTPGO_DATA_PROVIDER__"+strings.ToUpper(field)]; ok {
			return value, nil
		}
		value := sftpgoValue(config, "data_provider", field)
		if value == nil {
			return fallback, nil
		}
		s, ok := value.(string)
		if !ok {
			return "", invalid("SFTPGo 数据库配置字段类型无效")
		}
		return s, nil
	}
	driver, err := get("driver", "sqlite")
	if err != nil {
		return nil, err
	}
	if driver != "sqlite" && driver != "bolt" {
		return nil, &AppError{Code: "DATA_DIR_SFTPGO_PROVIDER", Message: "SFTPGo 使用远程或内存账号库，无法安全迁移其中的用户目录", Hint: "请先备份账号并改用本地 SQLite 或 Bolt 账号库；原数据目录继续使用。"}
	}
	name, err := get("name", "sftpgo.db")
	if err != nil {
		return nil, err
	}
	connection, err := get("connection_string", "")
	if err != nil {
		return nil, err
	}
	prefix, err := get("sql_tables_prefix", "")
	if err != nil {
		return nil, err
	}
	path, err := sftpgoDatabasePath(driver, name, connection, r.etc, r.cwd)
	if err != nil {
		return nil, err
	}
	if path == "" {
		return nil, invalid("SFTPGo 内存账号库无法迁移用户目录")
	}
	info, err := os.Lstat(path)
	if os.IsNotExist(err) {
		return protected, nil
	}
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || !pathWithin(c.Home, path) {
		return nil, invalid("SFTPGo 账号库必须是当前数据目录内的普通文件，原账号库未修改")
	}
	if info.Size() == 0 {
		return protected, nil
	}
	rel, err := filepath.Rel(c.Home, path)
	if err != nil {
		return nil, err
	}
	copy := filepath.Join(stage, rel)
	protected = append(protected, copy, copy+"-wal", copy+"-shm", copy+"-journal")
	rebase := dataPathRebase{c.Home, target}
	protect := func(value string) error {
		directory := DisplayPath(value)
		for strings.Contains(directory, "%username%") || strings.Contains(directory, "%role%") {
			parent := filepath.Dir(directory)
			if parent == directory {
				break
			}
			directory = parent
		}
		if filepath.IsAbs(directory) && pathWithin(c.Home, directory) {
			if filepath.Clean(directory) == filepath.Clean(c.Home) {
				return invalid("SFTPGo 用户目录覆盖整个应用数据目录，无法区分上传文件和应用配置，未切换目录")
			}
			rel, err := filepath.Rel(c.Home, directory)
			if err != nil {
				return err
			}
			protected = append(protected, filepath.Join(stage, rel))
		}
		return nil
	}
	if driver == "sqlite" {
		err = rebaseSftpgoSQLite(ctx, copy, prefix, rebase, protect)
	} else {
		err = rebaseSftpgoBolt(ctx, copy, rebase, protect)
	}
	if err != nil {
		return nil, &AppError{Code: "DATA_DIR_SFTPGO_STATE", Message: "SFTPGo 账号目录迁移未完成，原账号库和文件已保留", Detail: err.Error()}
	}
	// Make the copied provider address explicit. Environment values inherited from
	// outside the app cannot be changed by editing the copy and must be rejected.
	for _, field := range []string{"NAME", "CONNECTION_STRING"} {
		key := "SFTPGO_DATA_PROVIDER__" + field
		if value, exists := os.LookupEnv(key); exists && value != "" {
			oldPath := value
			if field == "CONNECTION_STRING" && driver == "sqlite" {
				oldPath, err = sftpgoDatabasePath(driver, name, value, r.etc, r.cwd)
				if err != nil {
					return nil, err
				}
			}
			if rebase.path(oldPath) != oldPath {
				return nil, invalid("系统 SFTPGo 环境变量仍指向旧数据目录，请先移除该覆盖值")
			}
		}
		if value, exists := r.env[key]; exists && value != "" {
			return nil, invalid("自定义 SFTPGo 运行环境覆盖了账号库路径，无法自动切换")
		}
	}
	// env.d overrides JSON/YAML. Rewrite only these two provider fields, leaving
	// unrelated secrets and remote storage options unchanged.
	etcRel, err := filepath.Rel(c.Home, r.etc)
	if err != nil || !filepath.IsLocal(etcRel) {
		return nil, invalid("SFTPGo 配置相对目录无效")
	}
	envDir := filepath.Join(stage, etcRel, "env.d")
	entries, err := os.ReadDir(envDir)
	if err != nil && !os.IsNotExist(err) {
		return nil, err
	}
	for _, item := range entries {
		if item.IsDir() {
			continue
		}
		file := filepath.Join(envDir, item.Name())
		body, err := readConfigText(file)
		if err != nil {
			return nil, err
		}
		changes := [][2]string{}
		for _, record := range parseEnv(body, "dotenv") {
			key := strings.ToUpper(record.entry.Key)
			if record.entry.Commented || !strings.HasPrefix(key, "SFTPGO_") {
				continue
			}
			value := record.entry.Value
			if key == "SFTPGO_DATA_PROVIDER__NAME" {
				value = rebase.path(value)
			} else if key == "SFTPGO_DATA_PROVIDER__CONNECTION_STRING" && driver == "sqlite" && value != "" {
				value = sqliteMoveConnection(rebase.path(path), value)
			} else {
				parts := strings.Split(key, "__")
				field := parts[len(parts)-1]
				if moveSensitiveKey(field) || !movePathKey(field) {
					continue
				}
				value = rebase.path(value)
			}
			changes = append(changes, [2]string{record.entry.Key, value})
		}
		if len(changes) > 0 {
			next, err := applyEnvChanges(body, "dotenv", changes)
			if err != nil {
				return nil, err
			}
			if err = writePrivateAtomic(file, []byte(next)); err != nil {
				return nil, err
			}
		}
	}
	provider, ok := config["data_provider"].(map[string]any)
	if !ok {
		provider = map[string]any{}
		config["data_provider"] = provider
	}
	if driver == "sqlite" {
		provider["connection_string"] = sqliteMoveConnection(rebase.path(path), connection)
	} else {
		provider["name"] = rebase.path(path)
	}
	var body []byte
	if strings.HasSuffix(strings.ToLower(configFile), ".json") {
		body, err = json.MarshalIndent(config, "", "  ")
	} else {
		body, err = yaml.Marshal(config)
	}
	if err != nil {
		return nil, err
	}
	configRel, err := filepath.Rel(c.Home, configFile)
	if err != nil || !filepath.IsLocal(configRel) {
		return nil, invalid("SFTPGo 配置相对路径无效")
	}
	return protected, writePrivateAtomic(filepath.Join(stage, configRel), body)
}

func sqliteMoveConnection(path, previous string) string {
	value := sqliteMoveURI(path)
	if _, query, ok := strings.Cut(previous, "?"); ok {
		value += "?" + query
	}
	return value
}
func sqliteMoveURI(path string) string {
	path = filepath.ToSlash(path)
	if !strings.HasPrefix(path, "/") {
		path = "/" + path
	}
	u := url.URL{Scheme: "file", Path: path}
	return u.String()
}
func sftpgoDatabasePath(driver, name, connection, directory, cwd string) (string, error) {
	if driver == "bolt" || connection == "" {
		if name == "" {
			return "", invalid("SFTPGo 数据库文件名不能为空")
		}
		p := DisplayPath(name)
		if !filepath.IsAbs(p) {
			p = filepath.Join(directory, p)
		}
		return p, nil
	}
	if driver != "sqlite" {
		return "", nil
	}
	path := strings.SplitN(connection, "?", 2)[0]
	if strings.HasPrefix(connection, "file:") {
		u, err := url.Parse(connection)
		if err != nil {
			return "", invalid("SFTPGo SQLite 连接字符串无效")
		}
		if u.Query().Get("mode") == "memory" {
			return "", nil
		}
		path = u.Path
		if path == "" {
			path = u.Opaque
		}
		if u.Host != "" && u.Host != "localhost" {
			path = "//" + u.Host + path
		}
		if runtime.GOOS == "windows" && len(path) > 2 && path[0] == '/' && path[2] == ':' {
			path = path[1:]
		}
	}
	if path == ":memory:" {
		return "", nil
	}
	if path == "" {
		return "", invalid("SFTPGo SQLite 文件路径为空")
	}
	path = DisplayPath(path)
	if !filepath.IsAbs(path) {
		path = filepath.Join(cwd, path)
	}
	return path, nil
}

func rebaseSftpgoObject(raw []byte, field string, rebase dataPathRebase, protect func(string) error) ([]byte, error) {
	var data map[string]json.RawMessage
	if err := json.Unmarshal(raw, &data); err != nil {
		return nil, err
	}
	if data == nil {
		return raw, nil
	}
	value, ok := data[field]
	if !ok || string(value) == "null" {
		return raw, nil
	}
	var before string
	if err := json.Unmarshal(value, &before); err != nil {
		return nil, err
	}
	if err := protect(before); err != nil {
		return nil, err
	}
	after := rebase.path(before)
	if after == before {
		return raw, nil
	}
	data[field], _ = json.Marshal(after)
	return json.Marshal(data)
}

func rebaseSftpgoSQLite(ctx context.Context, path, prefix string, rebase dataPathRebase, protect func(string) error) error {
	db, err := sql.Open("sqlite", sqliteMoveURI(path))
	if err != nil {
		return err
	}
	defer db.Close()
	db.SetMaxOpenConns(1)
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	var check string
	if err = tx.QueryRowContext(ctx, "PRAGMA quick_check").Scan(&check); err != nil {
		return err
	}
	if check != "ok" {
		return invalid("SFTPGo SQLite 完整性校验失败")
	}
	rows, err := tx.QueryContext(ctx, "SELECT name FROM sqlite_master WHERE type='table'")
	if err != nil {
		return err
	}
	tables := []string{}
	for rows.Next() {
		var name string
		if err = rows.Scan(&name); err != nil {
			rows.Close()
			return err
		}
		tables = append(tables, name)
	}
	if err = errors.Join(rows.Err(), rows.Close()); err != nil {
		return err
	}
	quote := func(s string) string { return `"` + strings.ReplaceAll(s, `"`, `""`) + `"` }
	for _, item := range [][2]string{{"users", "home_dir"}, {"folders", "path"}, {"groups", "user_settings"}} {
		name := prefix + item[0]
		if !slices.Contains(tables, name) {
			if item[0] == "groups" {
				continue
			}
			return invalid("SFTPGo 账号库缺少用户或目录表")
		}
		table, column := quote(name), quote(item[1])
		rows, err = tx.QueryContext(ctx, "PRAGMA table_info("+table+")")
		if err != nil {
			return err
		}
		columns := []string{}
		for rows.Next() {
			var id, notnull, pk int
			var name, typ string
			var def any
			if err = rows.Scan(&id, &name, &typ, &notnull, &def, &pk); err != nil {
				rows.Close()
				return err
			}
			columns = append(columns, name)
		}
		if err = errors.Join(rows.Err(), rows.Close()); err != nil {
			return err
		}
		if !slices.Contains(columns, "id") || !slices.Contains(columns, item[1]) {
			return invalid("SFTPGo 账号库字段不匹配")
		}
		rows, err = tx.QueryContext(ctx, "SELECT id,"+column+" FROM "+table)
		if err != nil {
			return err
		}
		type change struct {
			id    int64
			after any
		}
		changes := []change{}
		for rows.Next() {
			var id int64
			var raw any
			if err = rows.Scan(&id, &raw); err != nil {
				rows.Close()
				return err
			}
			if raw == nil {
				continue
			}
			var before string
			blob := false
			switch value := raw.(type) {
			case string:
				before = value
			case []byte:
				before = string(value)
				blob = true
			default:
				rows.Close()
				return invalid("SFTPGo 目录字段类型无效")
			}
			after := before
			if item[0] == "groups" && before != "" {
				var body []byte
				body, err = rebaseSftpgoObject([]byte(before), "home_dir", rebase, protect)
				after = string(body)
			} else {
				err = protect(before)
				after = rebase.path(before)
			}
			if err != nil {
				rows.Close()
				return err
			}
			if after != before {
				var value any = after
				if blob {
					value = []byte(after)
				}
				changes = append(changes, change{id, value})
			}
		}
		if err = errors.Join(rows.Err(), rows.Close()); err != nil {
			return err
		}
		for _, change := range changes {
			if _, err = tx.ExecContext(ctx, "UPDATE "+table+" SET "+column+"=? WHERE id=?", change.after, change.id); err != nil {
				return err
			}
		}
	}
	if err = tx.Commit(); err != nil {
		return err
	}
	_, err = db.ExecContext(ctx, "PRAGMA wal_checkpoint(TRUNCATE)")
	return err
}

func rebaseSftpgoBolt(ctx context.Context, path string, rebase dataPathRebase, protect func(string) error) error {
	db, err := bolt.Open(path, 0600, &bolt.Options{Timeout: time.Second})
	if err != nil {
		return err
	}
	defer db.Close()
	err = db.View(func(tx *bolt.Tx) error {
		var failures []error
		for err := range tx.Check() {
			failures = append(failures, err)
		}
		return errors.Join(failures...)
	})
	if err != nil {
		return err
	}
	return db.Update(func(tx *bolt.Tx) error {
		version := tx.Bucket([]byte("db_version"))
		if version == nil {
			return invalid("SFTPGo Bolt 缺少版本记录")
		}
		var schema struct{ Version int }
		if json.Unmarshal(version.Get([]byte("version")), &schema) != nil || schema.Version != 33 {
			return invalid("SFTPGo Bolt 账号库版本尚未完成升级")
		}
		for _, name := range []string{"users", "folders", "groups"} {
			bucket := tx.Bucket([]byte(name))
			if bucket == nil {
				return invalid("SFTPGo Bolt 缺少账号数据桶")
			}
			type change struct{ key, value []byte }
			changes := []change{}
			err := bucket.ForEach(func(key, value []byte) error {
				if err := ctx.Err(); err != nil {
					return err
				}
				if value == nil {
					return invalid("SFTPGo 账号桶包含意外子桶")
				}
				field := "home_dir"
				if name == "folders" {
					field = "mapped_path"
				}
				var next []byte
				var err error
				if name == "groups" {
					var group map[string]json.RawMessage
					if err = json.Unmarshal(value, &group); err != nil {
						return err
					}
					settings, ok := group["user_settings"]
					if !ok || string(settings) == "null" {
						return nil
					}
					next, err = rebaseSftpgoObject(settings, "home_dir", rebase, protect)
					if err != nil {
						return err
					}
					if string(next) == string(settings) {
						return nil
					}
					group["user_settings"] = next
					next, err = json.Marshal(group)
				} else {
					next, err = rebaseSftpgoObject(value, field, rebase, protect)
				}
				if err != nil {
					return err
				}
				if string(next) != string(value) {
					changes = append(changes, change{append([]byte(nil), key...), next})
				}
				return nil
			})
			if err != nil {
				return err
			}
			for _, change := range changes {
				if err = bucket.Put(change.key, change.value); err != nil {
					return err
				}
			}
		}
		return nil
	})
}
