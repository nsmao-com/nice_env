package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"net/url"
	"os"
	"path/filepath"
	"strings"

	_ "modernc.org/sqlite"
)

type Store struct{ db *sql.DB }

// OpenStore accepts only a Wails-owned development directory. Existing NiceEnv
// databases must be imported as a reviewed snapshot, never silently opened live.
func OpenStore(ctx context.Context, home string) (*Store, error) {
	if err := PrepareDevelopmentHome(home); err != nil {
		return nil, err
	}
	database := filepath.Join(home, "nsb.sqlite")
	uriPath := filepath.ToSlash(DisplayPath(database))
	// SQLite file URIs need /C:/..., otherwise the drive becomes an authority.
	if !strings.HasPrefix(uriPath, "/") {
		uriPath = "/" + uriPath
	}
	u := url.URL{Scheme: "file", Path: uriPath}
	q := u.Query()
	q.Set("_pragma", "busy_timeout(5000)")
	u.RawQuery = q.Encode()
	db, err := sql.Open("sqlite", u.String())
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	if _, err = db.ExecContext(ctx, schema); err != nil {
		db.Close()
		return nil, &AppError{Code: "STORE_OPEN_FAILED", Message: "无法打开应用数据文件", Detail: err.Error()}
	}
	return &Store{db: db}, nil
}

func PrepareDevelopmentHome(home string) error {
	if err := os.MkdirAll(home, 0700); err != nil {
		return err
	}
	marker := filepath.Join(home, ".niceenv-wails-development")
	const signature = "NiceEnv Wails development data\n"
	if data, err := os.ReadFile(marker); err == nil && string(data) == signature {
		return nil
	}
	entries, err := os.ReadDir(home)
	if err != nil {
		return err
	}
	if len(entries) != 0 {
		return &AppError{Code: "DATA_DIRECTORY_PROTECTED", Message: "此目录不是 Wails 开发数据目录", Hint: "请选择独立空目录，避免修改现用 NiceEnv 或 FlyEnv 数据。"}
	}
	file, err := os.OpenFile(marker, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		return err
	}
	_, err = file.WriteString(signature)
	closeErr := file.Close()
	if err != nil {
		return err
	}
	return closeErr
}

func (s *Store) Close() error { return s.db.Close() }

func (s *Store) setting(ctx context.Context, key string) (string, bool, error) {
	var value string
	err := s.db.QueryRowContext(ctx, "SELECT value FROM settings WHERE key=?", key).Scan(&value)
	if errors.Is(err, sql.ErrNoRows) {
		return "", false, nil
	}
	return value, err == nil, err
}

func (s *Store) saveSetting(ctx context.Context, key string, value any) error {
	encoded, err := json.Marshal(value)
	if err != nil {
		return err
	}
	text := string(encoded)
	if v, ok := value.(string); ok {
		text = v
	}
	_, err = s.db.ExecContext(ctx, "INSERT INTO settings(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", key, text)
	return err
}

// tableRows is internal: query and column names are compile-time constants.
func (s *Store) tableRows(ctx context.Context, query string, jsonColumns, pathColumns map[string]bool) ([]map[string]any, error) {
	rows, err := s.db.QueryContext(ctx, query)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	columns, err := rows.Columns()
	if err != nil {
		return nil, err
	}
	result := make([]map[string]any, 0)
	for rows.Next() {
		values := make([]any, len(columns))
		refs := make([]any, len(columns))
		for i := range values {
			refs[i] = &values[i]
		}
		if err := rows.Scan(refs...); err != nil {
			return nil, err
		}
		row := make(map[string]any, len(columns))
		for i, name := range columns {
			value := values[i]
			if bytes, ok := value.([]byte); ok {
				value = string(bytes)
			}
			if text, ok := value.(string); ok {
				if jsonColumns[name] {
					if err := json.Unmarshal([]byte(text), &value); err != nil {
						return nil, &AppError{Code: "DATA_INVALID", Message: "保存的数据格式无效", Detail: name}
					}
				}
				if pathColumns[name] {
					value = Path(text)
				}
			}
			row[name] = value
		}
		result = append(result, row)
	}
	return result, rows.Err()
}

func columns(names string) map[string]bool {
	result := map[string]bool{}
	for _, name := range strings.Fields(names) {
		result[name] = true
	}
	return result
}

const schema = `
PRAGMA journal_mode=WAL;
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS installed(key TEXT PRIMARY KEY,id TEXT NOT NULL,version TEXT NOT NULL,category TEXT NOT NULL,install_path TEXT NOT NULL,config_path TEXT NOT NULL,installed_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sites(id TEXT PRIMARY KEY,name TEXT NOT NULL,domains TEXT NOT NULL,root_dir TEXT NOT NULL,runtime TEXT NOT NULL,https INTEGER NOT NULL DEFAULT 0,rewrite TEXT NOT NULL,db TEXT,php_overrides TEXT,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS certs(id TEXT PRIMARY KEY,kind TEXT NOT NULL,subject TEXT NOT NULL,sans TEXT NOT NULL,not_before INTEGER NOT NULL,not_after INTEGER NOT NULL,cert_path TEXT NOT NULL,key_path TEXT);
CREATE TABLE IF NOT EXISTS proxy_profiles(id TEXT PRIMARY KEY,name TEXT NOT NULL,url TEXT NOT NULL,active INTEGER NOT NULL DEFAULT 0,added_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS port_assign(service_id TEXT PRIMARY KEY,base_port INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS stacks(id TEXT PRIMARY KEY,name TEXT NOT NULL,description TEXT NOT NULL DEFAULT '',items TEXT NOT NULL,builtin INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS cert_automations(id TEXT PRIMARY KEY,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS cert_monitors(id TEXT PRIMARY KEY,data TEXT NOT NULL,updated_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS cron_jobs(id TEXT PRIMARY KEY,name TEXT NOT NULL,command TEXT NOT NULL,interval_min INTEGER NOT NULL,enabled INTEGER NOT NULL DEFAULT 1,created_at INTEGER NOT NULL,last_run_at INTEGER,last_exit TEXT,last_output TEXT);
`
