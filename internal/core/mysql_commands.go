package core

import (
	"context"
	"database/sql"
	"encoding/json"
	"path/filepath"
	"strings"
)

type databaseRequest struct {
	Engine      string `json:"engine"`
	Version     string `json:"version"`
	Name        string `json:"name"`
	Username    string `json:"username"`
	Password    string `json:"password"`
	Database    string `json:"database"`
	NewPassword string `json:"newPassword"`
	UseExisting bool   `json:"useExisting"`
}

func (c *Core) registerMySQL() {
	c.handle("db_grants", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlAccountCommand(ctx, data, "grants")
	})
	c.handle("db_grants_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlAccountCommand(ctx, data, "grants-save")
	})
	c.handle("db_user_drop_info", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlAccountCommand(ctx, data, "drop-info")
	})
	c.handle("db_user_drop", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlAccountCommand(ctx, data, "drop")
	})
	c.handle("db_user_password_info", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlAccountCommand(ctx, data, "password-info")
	})
	c.handle("db_user_password_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlAccountCommand(ctx, data, "password-save")
	})
	c.handle("db_workspace", c.databaseWorkspace)
	c.handle("database_data_dir", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[databaseRequest](data)
		if err != nil {
			return nil, err
		}
		if in.Engine == "redis" {
			if !c.services.lifecycle.TryLock() {
				return nil, serviceBusy()
			}
			defer c.services.lifecycle.Unlock()
			e, err := c.selectedRedis(in.Version)
			if err != nil {
				return nil, err
			}
			r, err := c.resolveService(ctx, e)
			if err != nil {
				return nil, err
			}
			return Path(r.data), nil
		}
		e, err := c.selectedMySQL(ctx, in.Engine, in.Version)
		if err != nil {
			return nil, err
		}
		return Path(filepath.Join(c.Home, "data", e.pkg.ID, e.pkg.Version)), nil
	})
	c.handle("service_data_dir", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"serviceId"`
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
		r, err := c.resolveService(ctx, e)
		if err != nil {
			return nil, err
		}
		return Path(r.data), nil
	})
	c.handle("db_list", func(ctx context.Context, data json.RawMessage) (any, error) { return c.mysqlCommand(ctx, data, "list") })
	c.handle("db_users", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlCommand(ctx, data, "users")
	})
	c.handle("db_create", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlCommand(ctx, data, "create")
	})
	c.handle("db_drop", func(ctx context.Context, data json.RawMessage) (any, error) { return c.mysqlCommand(ctx, data, "drop") })
	c.handle("db_create_user", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlCommand(ctx, data, "create-user")
	})
	c.handle("db_root_password", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlCommand(ctx, data, "password")
	})
	c.handle("db_reset_root_password", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mysqlCommand(ctx, data, "set-password")
	})
}
func (c *Core) mysqlCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[databaseRequest](data)
	if err != nil {
		return nil, err
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
	if action == "password" {
		if !found {
			return nil, &AppError{Code: "MYSQL_AUTH_REQUIRED", Message: "此实例尚未保存 root 密码"}
		}
		return password, nil
	}
	if action == "set-password" && in.UseExisting {
		password = in.NewPassword
		found = true
	}
	if !found {
		return nil, &AppError{Code: "MYSQL_AUTH_REQUIRED", Message: "请先验证并保存此实例 root 密码"}
	}
	db, err := c.mysqlConnection(ctx, e, password)
	if err != nil {
		return nil, err
	}
	defer db.Close()
	switch action {
	case "set-password":
		if strings.ContainsRune(in.NewPassword, 0) || len(in.NewPassword) > 1024 {
			return nil, invalid("密码含无效字符或过长")
		}
		if !in.UseExisting {
			if in.NewPassword == "" {
				return nil, invalid("新密码不能为空")
			}
			// Keep a recovery credential before changing the server account.
			if err = c.Store.saveSetting(ctx, mysqlPasswordKey(e)+".pending", in.NewPassword); err != nil {
				return nil, err
			}
			if _, err = db.ExecContext(ctx, "ALTER USER 'root'@'localhost' IDENTIFIED BY ?", in.NewPassword); err != nil {
				return nil, &AppError{Code: "MYSQL_PASSWORD_CHANGE_FAILED", Message: "数据库拒绝修改密码，原凭据保留"}
			}
		}
		if err = c.Store.saveSetting(ctx, mysqlPasswordKey(e), in.NewPassword); err != nil {
			return nil, err
		}
		_, err = c.Store.db.ExecContext(ctx, "DELETE FROM settings WHERE key=?", mysqlPasswordKey(e)+".pending")
		return true, err
	case "list":
		rows, err := db.QueryContext(ctx, "SELECT s.SCHEMA_NAME, COUNT(t.TABLE_NAME), COALESCE(SUM(t.DATA_LENGTH+t.INDEX_LENGTH),0)/1024 FROM information_schema.SCHEMATA s LEFT JOIN information_schema.TABLES t ON s.SCHEMA_NAME=t.TABLE_SCHEMA GROUP BY s.SCHEMA_NAME ORDER BY s.SCHEMA_NAME")
		if err != nil {
			return nil, err
		}
		defer rows.Close()
		out := []map[string]any{}
		for rows.Next() {
			var name string
			var tables int
			var size float64
			if err = rows.Scan(&name, &tables, &size); err != nil {
				return nil, err
			}
			out = append(out, map[string]any{"name": name, "tables": tables, "sizeKb": size})
		}
		return out, rows.Err()
	case "users":
		rows, err := db.QueryContext(ctx, "SELECT User,Host FROM mysql.user ORDER BY User,Host")
		if err != nil {
			return nil, err
		}
		defer rows.Close()
		out := []map[string]any{}
		for rows.Next() {
			var name, host string
			if err = rows.Scan(&name, &host); err != nil {
				return nil, err
			}
			out = append(out, map[string]any{"username": name, "host": host})
		}
		return out, rows.Err()
	case "create", "drop":
		name, err := databaseIdentifier(in.Name)
		if err != nil {
			return nil, err
		}
		if protectedDatabase(in.Name) {
			return nil, invalid("不能修改系统数据库")
		}
		statement := "CREATE DATABASE " + name + " CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci"
		if action == "drop" {
			statement = "DROP DATABASE " + name
		}
		_, err = db.ExecContext(ctx, statement)
		return true, err
	case "create-user":
		if in.Username == "" || strings.EqualFold(in.Username, "root") || strings.HasPrefix(strings.ToLower(in.Username), "mysql.") || len(in.Username) > 32 || strings.ContainsAny(in.Username, "\x00\r\n") || in.Password == "" || strings.ContainsRune(in.Password, 0) {
			return nil, invalid("用户名或密码无效")
		}
		_, err := databaseIdentifier(in.Database)
		if err != nil {
			return nil, err
		}
		if protectedDatabase(in.Database) {
			return nil, invalid("不允许为系统数据库批量授权")
		}
		partial, err := mysqlPartialRevokes(ctx, db)
		if err != nil {
			return nil, err
		}
		name := mysqlField(mysqlGrantLiteral(in.Database, partial))
		var existing string
		if err = db.QueryRowContext(ctx, "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME=?", in.Database).Scan(&existing); err != nil {
			if err == sql.ErrNoRows {
				return nil, invalid("目标数据库不存在")
			}
			return nil, err
		}
		if _, err = db.ExecContext(ctx, "CREATE USER ?@'localhost' IDENTIFIED BY ?", in.Username, in.Password); err != nil {
			return nil, &AppError{Code: "DATABASE_CREATE_USER_FAILED", Message: "创建用户失败，请检查用户是否已存在或密码是否符合要求"}
		}
		if _, err = db.ExecContext(ctx, "GRANT ALL PRIVILEGES ON "+name+".* TO ?@'localhost'", in.Username); err != nil {
			_, cleanupErr := db.ExecContext(ctx, "DROP USER ?@'localhost'", in.Username)
			if cleanupErr != nil {
				return nil, &AppError{Code: "DATABASE_GRANT_PARTIAL", Message: "用户已创建但授权失败，请在用户列表检查权限"}
			}
			return nil, err
		}
		return true, nil
	}
	return nil, invalid("未知数据库操作")
}
