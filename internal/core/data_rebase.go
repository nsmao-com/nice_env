package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"path/filepath"
	"runtime"
	"strings"
)

type dataPathRebase struct{ source, target string }

func (r dataPathRebase) path(value string) string {
	plain := DisplayPath(value)
	normalized := strings.ReplaceAll(plain, "\\", "/")
	old := strings.TrimRight(strings.ReplaceAll(DisplayPath(r.source), "\\", "/"), "/")
	comparison, root := normalized, old
	if runtime.GOOS == "windows" || runtime.GOOS == "darwin" {
		comparison, root = strings.ToLower(comparison), strings.ToLower(root)
	}
	if comparison != root && !strings.HasPrefix(comparison, root+"/") {
		return value
	}
	next := strings.TrimRight(strings.ReplaceAll(DisplayPath(r.target), "\\", "/"), "/") + normalized[len(old):]
	if strings.Contains(plain, "\\") && !strings.Contains(plain, "/") {
		next = strings.ReplaceAll(next, "/", "\\")
	}
	return next
}

// Only complete shell words representing paths are changed. SQL strings,
// passwords, variables and all other argument contents keep their original bytes.
func (r dataPathRebase) command(text string) (string, error) {
	tokens, err := moveTokens(text)
	if err != nil {
		return "", err
	}
	for i := len(tokens) - 1; i >= 0; i-- {
		t := tokens[i]
		next := r.path(t.value)
		if next == t.value {
			continue
		}
		if runtime.GOOS == "windows" && strings.ContainsAny(next, "%!\r\n\x00") {
			return "", invalid("新目录在命令中含环境变量字符，请调整命令或选择不含 % 和 ! 的目录")
		}
		quoted := "\"" + strings.ReplaceAll(next, "\"", "\\\"") + "\""
		if runtime.GOOS != "windows" {
			quoted = "'" + strings.ReplaceAll(next, "'", "'\"'\"'") + "'"
		}
		text = text[:t.start] + quoted + text[t.end:]
	}
	return text, nil
}

func (c *Core) snapshotDataDirectory(ctx context.Context, stage, target string) error {
	dest := filepath.Join(stage, "nsb.sqlite")
	if _, err := c.Store.db.ExecContext(ctx, "VACUUM main INTO ?", dest); err != nil {
		return err
	}
	uriPath := filepath.ToSlash(dest)
	if !strings.HasPrefix(uriPath, "/") {
		uriPath = "/" + uriPath
	}
	u := url.URL{Scheme: "file", Path: uriPath}
	db, err := sql.Open("sqlite", u.String())
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
	// RabbitMQ uses its node name as part of the on-disk database identity.
	// Legacy Go homes derived it from the old root; retain that identity on move.
	if _, err = tx.ExecContext(ctx, "INSERT INTO settings(key,value) VALUES('rabbitNodeName',?) ON CONFLICT(key) DO NOTHING", rabbitNodeName(c.Home)); err != nil {
		return err
	}
	rebase := dataPathRebase{c.Home, target}
	rewrite := func(table, key, column string, transform func(string) (string, error)) error {
		rows, err := tx.QueryContext(ctx, "SELECT "+key+","+column+" FROM "+table+" WHERE "+column+" IS NOT NULL")
		if err != nil {
			return err
		}
		var pairs [][2]string
		for rows.Next() {
			var pair [2]string
			if err = rows.Scan(&pair[0], &pair[1]); err != nil {
				rows.Close()
				return err
			}
			pairs = append(pairs, pair)
		}
		err = errors.Join(rows.Err(), rows.Close())
		if err != nil {
			return err
		}
		for _, pair := range pairs {
			next, err := transform(pair[1])
			if err != nil {
				return fmt.Errorf("%s.%s: %w", table, column, err)
			}
			if next != pair[1] {
				if _, err = tx.ExecContext(ctx, "UPDATE "+table+" SET "+column+"=? WHERE "+key+"=?", next, pair[0]); err != nil {
					return err
				}
			}
		}
		return nil
	}
	for _, field := range [][3]string{{"installed", "key", "install_path"}, {"installed", "key", "config_path"}, {"sites", "id", "root_dir"}, {"certs", "id", "cert_path"}, {"certs", "id", "key_path"}} {
		if err = rewrite(field[0], field[1], field[2], func(s string) (string, error) { return rebase.path(s), nil }); err != nil {
			return err
		}
	}
	if err = rewrite("sites", "id", "runtime", func(raw string) (string, error) {
		var obj map[string]any
		if err := json.Unmarshal([]byte(raw), &obj); err != nil {
			return "", err
		}
		if s, ok := obj["cwd"].(string); ok {
			obj["cwd"] = rebase.path(s)
		}
		if s, ok := obj["command"].(string); ok {
			value, err := rebase.command(s)
			if err != nil {
				return "", err
			}
			obj["command"] = value
		}
		if app, ok := obj["application"].(map[string]any); ok {
			if s, ok := app["cwd"].(string); ok {
				app["cwd"] = rebase.path(s)
			}
			if args, ok := app["args"].([]any); ok {
				for i, arg := range args {
					if s, ok := arg.(string); ok {
						args[i] = rebase.path(s)
					}
				}
			}
		}
		encoded, err := json.Marshal(obj)
		return string(encoded), err
	}); err != nil {
		return err
	}
	if err = rewrite("sites", "id", "php_overrides", func(raw string) (string, error) {
		var obj map[string]string
		if err := json.Unmarshal([]byte(raw), &obj); err != nil {
			return "", err
		}
		for key, value := range obj {
			if movePathKey(key) {
				obj[key] = rebase.path(value)
			}
		}
		encoded, err := json.Marshal(obj)
		return string(encoded), err
	}); err != nil {
		return err
	}
	if err = rewrite("cron_jobs", "id", "command", rebase.command); err != nil {
		return err
	}
	if err = rewrite("cert_automations", "id", "data", func(raw string) (string, error) {
		var a certAutomation
		if err := json.Unmarshal([]byte(raw), &a); err != nil {
			return "", err
		}
		for i := range a.Targets {
			t := &a.Targets[i]
			keys := []string{}
			if t.Kind == "local" {
				keys = []string{"certPath", "keyPath"}
				if script := t.Config["script"]; script != "" {
					next, err := rebase.command(script)
					if err != nil {
						return "", err
					}
					t.Config["script"] = next
				}
			} else if t.Kind == "ssh" {
				keys = []string{"identityFile", "privateKey"}
			}
			for _, key := range keys {
				if value, ok := t.Config[key]; ok {
					t.Config[key] = rebase.path(value)
				}
			}
		}
		encoded, err := json.Marshal(a)
		return string(encoded), err
	}); err != nil {
		return err
	}
	// Settings include tokens, passwords, SQL and PATH recovery receipts. Leave
	// arbitrary settings untouched; only explicit managed path fields are rebased.
	rows, err := tx.QueryContext(ctx, "SELECT key,value FROM settings")
	if err != nil {
		return err
	}
	var settings [][2]string
	for rows.Next() {
		var p [2]string
		if err = rows.Scan(&p[0], &p[1]); err != nil {
			rows.Close()
			return err
		}
		settings = append(settings, p)
	}
	if err = errors.Join(rows.Err(), rows.Close()); err != nil {
		return err
	}
	for _, s := range settings {
		next := s[1]
		if strings.HasPrefix(s[0], "runtimePath@") || s[0] == "defaultSiteRoot" {
			next = rebase.path(next)
		}
		if next != s[1] {
			if _, err = tx.ExecContext(ctx, "UPDATE settings SET value=? WHERE key=?", next, s[0]); err != nil {
				return err
			}
		}
	}
	if err = tx.Commit(); err != nil {
		return err
	}
	var check string
	if err = db.QueryRowContext(ctx, "PRAGMA integrity_check").Scan(&check); err != nil {
		return err
	}
	if check != "ok" {
		return invalid("迁移后的数据库完整性检查失败")
	}
	return nil
}
