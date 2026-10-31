package core

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
)

func validateSiteDatabase(db *siteDB) error {
	if db == nil {
		return nil
	}
	if _, err := databaseIdentifier(db.Database); err != nil {
		return err
	}
	if protectedDatabase(db.Database) || db.Username == "" || strings.EqualFold(db.Username, "root") || strings.HasPrefix(strings.ToLower(db.Username), "mysql.") || len(db.Username) > 32 || strings.ContainsAny(db.Username, "\x00\r\n") || db.Password == "" || len(db.Password) > 1024 || strings.ContainsRune(db.Password, 0) {
		return invalid("数据库名称、用户名或密码无效，不能使用系统账号或系统数据库")
	}
	return nil
}

// Caller holds services.lifecycle. Only a new database and a new localhost
// account are accepted: provisioning never changes an existing user's password.
func (c *Core) provisionSiteDatabase(ctx context.Context, input siteDB, started *[]*serviceEntry) (*siteDB, error) {
	if err := validateSiteDatabase(&input); err != nil {
		return nil, err
	}
	version := ""
	if input.Version != nil {
		version = *input.Version
	}
	e, err := c.selectedMySQL(ctx, "mysql", version)
	if err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	active := e.child != nil
	c.services.mu.RUnlock()
	if !active {
		*started = append(*started, e)
	}
	if err = c.startService(ctx, e); err != nil {
		return nil, err
	}
	password, found, err := c.Store.setting(ctx, mysqlPasswordKey(e))
	if err != nil {
		return nil, err
	}
	if !found {
		return nil, &AppError{Code: "MYSQL_AUTH_REQUIRED", Message: "请先在数据库页验证并保存 root 密码"}
	}
	db, err := c.mysqlConnection(ctx, e, password)
	if err != nil {
		return nil, err
	}
	defer db.Close()
	var exists int
	if err = db.QueryRowContext(ctx, "SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME=?", input.Database).Scan(&exists); err != nil {
		return nil, err
	}
	if exists != 0 {
		return nil, invalid("同名数据库已存在，未修改其数据；请选择新的数据库名称")
	}
	if err = db.QueryRowContext(ctx, "SELECT COUNT(*) FROM mysql.user WHERE User=?", input.Username).Scan(&exists); err != nil {
		return nil, err
	}
	if exists != 0 {
		return nil, invalid("同名数据库用户已存在，未修改其密码或权限；请选择新的用户名")
	}
	partial, err := mysqlPartialRevokes(ctx, db)
	if err != nil {
		return nil, err
	}
	name, _ := databaseIdentifier(input.Database)
	if _, err = db.ExecContext(ctx, "CREATE DATABASE "+name+" CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"); err != nil {
		return nil, err
	}
	userCreated := false
	if _, err = db.ExecContext(ctx, "CREATE USER ?@'localhost' IDENTIFIED BY ?", input.Username, input.Password); err == nil {
		userCreated = true
		_, err = db.ExecContext(ctx, "GRANT ALL PRIVILEGES ON "+mysqlField(mysqlGrantLiteral(input.Database, partial))+".* TO ?@'localhost'", input.Username)
	}
	if err != nil {
		cleanup, cancel := context.WithTimeout(context.Background(), 15*time.Second)
		defer cancel()
		failures := []error{err}
		if userCreated {
			_, e := db.ExecContext(cleanup, "DROP USER ?@'localhost'", input.Username)
			failures = append(failures, e)
		}
		_, e := db.ExecContext(cleanup, "DROP DATABASE "+name)
		failures = append(failures, e)
		return nil, &AppError{Code: "SITE_DATABASE_CREATE_FAILED", Message: "站点数据库创建未完成", Detail: errors.Join(failures...).Error()}
	}
	c.services.mu.RLock()
	port := e.resolved.port
	c.services.mu.RUnlock()
	version = e.pkg.Version
	input.Enabled, input.Version, input.Port = true, &version, &port
	return &input, nil
}

func prepareSiteDatabaseFiles(journal *siteFileJournal, project, template string, db *siteDB, example, existing bool) error {
	if existing {
		return nil
	}
	root, err := os.OpenRoot(project)
	if err != nil {
		return err
	}
	defer root.Close()
	p := &envProject{root: root, syntax: "dotenv", hint: &envDBHint{db.Database, db.Username, db.Password, *db.Port}}
	if template == "thinkphp" {
		p.syntax = "thinkphp"
	}
	if template == "codeigniter" {
		p.syntax = "codeigniter"
	}
	if composerTemplate(template) != "" {
		before, _, err := envRead(root, ".env")
		if err != nil {
			return err
		}
		changes, err := envDBVars(p, string(before))
		if err != nil {
			return err
		}
		after, err := applyEnvChanges(string(before), p.syntax, changes)
		if err != nil {
			return err
		}
		if err = journal.add(filepath.Join(project, ".env"), []byte(after), true); err != nil {
			return err
		}
	}
	if template == "wordpress" && !rootFile(root, "wp-config.php") {
		content, err := wordpressConfiguration(db)
		if err != nil {
			return err
		}
		if err = journal.add(filepath.Join(project, "wp-config.php"), []byte(content), true); err != nil {
			return err
		}
	}
	if example && !rootFile(root, ".env.example") {
		changes, err := envDBVars(p, "")
		if err != nil {
			return err
		}
		content, err := applyEnvChanges("", p.syntax, changes)
		if err != nil {
			return err
		}
		return journal.add(filepath.Join(project, ".env.example"), []byte(content), true)
	}
	return nil
}

func wordpressConfiguration(db *siteDB) (string, error) {
	quote := func(s string) string {
		return "'" + strings.ReplaceAll(strings.ReplaceAll(s, `\`, `\\`), `'`, `\'`) + "'"
	}
	var text strings.Builder
	text.WriteString("<?php\n")
	for _, item := range [][2]string{{"DB_NAME", db.Database}, {"DB_USER", db.Username}, {"DB_PASSWORD", db.Password}, {"DB_HOST", "127.0.0.1:" + strconv.Itoa(*db.Port)}, {"DB_CHARSET", "utf8mb4"}, {"DB_COLLATE", ""}} {
		fmt.Fprintf(&text, "define(%s, %s);\n", quote(item[0]), quote(item[1]))
	}
	for _, name := range []string{"AUTH_KEY", "SECURE_AUTH_KEY", "LOGGED_IN_KEY", "NONCE_KEY", "AUTH_SALT", "SECURE_AUTH_SALT", "LOGGED_IN_SALT", "NONCE_SALT"} {
		secret, err := scaffoldSecret(48)
		if err != nil {
			return "", err
		}
		fmt.Fprintf(&text, "define(%s, %s);\n", quote(name), quote(secret))
	}
	text.WriteString("$table_prefix = 'wp_';\ndefine('WP_DEBUG', false);\nif (!defined('ABSPATH')) define('ABSPATH', __DIR__ . '/');\nrequire_once ABSPATH . 'wp-settings.php';\n")
	return text.String(), nil
}
