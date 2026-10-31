package core

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
)

const postgresDatabasesSQL = `SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.name), '[]'::json) FROM (SELECT oid::bigint AS oid, datname AS name, pg_get_userbyid(datdba) AS owner, pg_encoding_to_char(encoding) AS encoding, pg_database_size(oid) AS "sizeBytes", (datistemplate OR datname IN ('postgres','template0','template1') OR datname=current_database()) AS protected, datallowconn AS "allowConnections" FROM pg_database) d`
const postgresRolesSQL = `SELECT COALESCE(json_agg(row_to_json(r) ORDER BY r.name), '[]'::json) FROM (SELECT oid::bigint AS oid, rolname AS name, rolcanlogin AS "canLogin", rolconnlimit AS "connectionLimit", rolsuper AS superuser, rolcreatedb AS "createDb", rolcreaterole AS "createRole", rolreplication AS replication, rolbypassrls AS "bypassRls", (rolsuper OR rolname IN ('postgres',current_user,session_user) OR starts_with(rolname,'pg_')) AS protected, ARRAY(SELECT datname FROM pg_database WHERE datdba=pg_roles.oid ORDER BY datname) AS databases FROM pg_roles WHERE NOT starts_with(rolname,'pg_')) r`
const postgresRoleAccessSQL = `SELECT json_build_object('oid',r.oid::bigint,'name',r.rolname,'canLogin',r.rolcanlogin,'connectionLimit',r.rolconnlimit,'activeConnections',(SELECT count(*) FROM pg_stat_activity a WHERE a.usesysid=r.oid AND a.backend_type='client backend'),'protected',(r.rolsuper OR r.rolname IN ('postgres',current_user,session_user) OR starts_with(r.rolname,'pg_')),'revision',md5(json_build_array(r.oid,r.rolname,r.rolcanlogin,r.rolconnlimit,r.rolsuper,r.rolcreatedb,r.rolcreaterole,r.rolreplication,r.rolbypassrls,r.rolinherit,r.rolvaliduntil,r.rolconfig)::text)) FROM pg_roles r WHERE r.oid=$1`

type postgresDatabase struct {
	OID              uint32 `json:"oid"`
	Name             string `json:"name"`
	Owner            string `json:"owner"`
	Encoding         string `json:"encoding"`
	SizeBytes        int64  `json:"sizeBytes"`
	Protected        bool   `json:"protected"`
	AllowConnections bool   `json:"allowConnections"`
}
type postgresRole struct {
	OID             uint32   `json:"oid"`
	Name            string   `json:"name"`
	CanLogin        bool     `json:"canLogin"`
	ConnectionLimit int32    `json:"connectionLimit"`
	Superuser       bool     `json:"superuser"`
	CreateDB        bool     `json:"createDb"`
	CreateRole      bool     `json:"createRole"`
	Replication     bool     `json:"replication"`
	BypassRLS       bool     `json:"bypassRls"`
	Protected       bool     `json:"protected"`
	Databases       []string `json:"databases"`
}
type postgresRoleAccess struct {
	OID               uint32 `json:"oid"`
	Name              string `json:"name"`
	CanLogin          bool   `json:"canLogin"`
	ConnectionLimit   int32  `json:"connectionLimit"`
	ActiveConnections int64  `json:"activeConnections"`
	Protected         bool   `json:"protected"`
	Revision          string `json:"revision"`
}
type postgresRoleAccessInput struct {
	OID                uint32 `json:"oid"`
	Name               string `json:"name"`
	CanLogin           bool   `json:"canLogin"`
	ConnectionLimit    int32  `json:"connectionLimit"`
	Revision           string `json:"revision"`
	ConfirmRestriction bool   `json:"confirmRestriction"`
}
type postgresRequest struct {
	Version            string                  `json:"version"`
	Name               string                  `json:"name"`
	Owner              string                  `json:"owner"`
	OID                uint32                  `json:"oid"`
	Password           string                  `json:"password"`
	UseExisting        bool                    `json:"useExisting"`
	EnablePasswordAuth bool                    `json:"enablePasswordAuth"`
	Input              postgresRoleAccessInput `json:"input"`
	Request            struct {
		Database  string `json:"database"`
		SQL       string `json:"sql"`
		Confirmed bool   `json:"confirmed"`
	} `json:"request"`
}
type postgresQueryer interface {
	QueryRow(context.Context, string, ...any) pgx.Row
}

func postgresJSON[T any](ctx context.Context, conn postgresQueryer, sql string, args ...any) (T, error) {
	var result T
	var data []byte
	if err := conn.QueryRow(ctx, sql, args...).Scan(&data); err != nil {
		return result, err
	}
	if err := json.Unmarshal(data, &result); err != nil {
		return result, invalid("PostgreSQL 返回结果无法识别")
	}
	return result, nil
}
func (c *Core) registerPostgres() {
	c.handle("postgres_connection", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "connection")
	})
	c.handle("postgres_password", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "password")
	})
	c.handle("postgres_set_password", c.postgresSetPassword)
	c.handle("postgres_query", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "query")
	})
	c.handle("postgres_databases", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "databases")
	})
	c.handle("postgres_roles", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "roles")
	})
	c.handle("postgres_create_database", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "create_database")
	})
	c.handle("postgres_drop_database", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "drop_database")
	})
	c.handle("postgres_create_role", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "create_role")
	})
	c.handle("postgres_drop_role", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "drop_role")
	})
	c.handle("postgres_set_role_password", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "role_password")
	})
	c.handle("postgres_role_access", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "role_access")
	})
	c.handle("postgres_role_access_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.postgresCommand(ctx, data, "role_access_save")
	})
}
func (c *Core) postgresCommand(ctx context.Context, data json.RawMessage, action string) (result any, resultErr error) {
	in, err := decode[postgresRequest](data)
	if err != nil {
		return nil, err
	}
	if !safePart(in.Version) {
		return nil, invalid("PostgreSQL 版本无效")
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	if action == "password" {
		if _, err := c.serviceEntry("postgresql", in.Version); err != nil {
			return nil, err
		}
		password, _, err := c.Store.setting(ctx, postgresPasswordKey(in.Version))
		return password, err
	}
	ctx, cancel := context.WithTimeout(ctx, 25*time.Second)
	defer cancel()
	e, conn, password, err := c.postgresSelected(ctx, in.Version, nil, "postgres")
	if err != nil {
		return nil, err
	}
	defer closePostgres(conn)
	defer func() {
		if resultErr != nil {
			if _, ok := resultErr.(*AppError); !ok {
				resultErr = postgresError(resultErr, password)
			}
		}
	}()
	switch action {
	case "connection":
		var server string
		var count, size int64
		if err = conn.QueryRow(ctx, "SELECT current_setting('server_version'),COUNT(*),COALESCE(SUM(pg_database_size(oid)),0)::bigint FROM pg_database WHERE NOT datistemplate").Scan(&server, &count, &size); err != nil {
			return nil, err
		}
		required, err := c.postgresRequiresPassword(ctx, e)
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		port := e.resolved.port
		c.services.mu.RUnlock()
		return map[string]any{"version": in.Version, "port": port, "serverVersion": server, "databaseCount": count, "sizeBytes": size, "passwordRequired": required}, nil
	case "databases":
		return postgresJSON[[]postgresDatabase](ctx, conn, postgresDatabasesSQL)
	case "roles":
		return postgresJSON[[]postgresRole](ctx, conn, postgresRolesSQL)
	case "role_access":
		if _, err = postgresName(in.Name, false); err != nil {
			return nil, err
		}
		view, err := postgresJSON[postgresRoleAccess](ctx, conn, postgresRoleAccessSQL, in.OID)
		if err != nil {
			return nil, err
		}
		if view.Name != in.Name {
			return nil, invalid("账号已更名，请刷新列表")
		}
		return view, nil
	case "role_access_save":
		return postgresSaveRoleAccess(ctx, conn, in.Input)
	case "create_database", "drop_database":
		name, err := postgresName(in.Name, action == "create_database")
		if err != nil {
			return nil, err
		}
		databases, err := postgresJSON[[]postgresDatabase](ctx, conn, postgresDatabasesSQL)
		if err != nil {
			return nil, err
		}
		if action == "create_database" {
			for _, db := range databases {
				if db.Name == in.Name {
					return nil, invalid("同名数据库已存在")
				}
			}
			owner, err := postgresName(in.Owner, false)
			if err != nil {
				return nil, err
			}
			roles, err := postgresJSON[[]postgresRole](ctx, conn, postgresRolesSQL)
			if err != nil {
				return nil, err
			}
			found := false
			for _, r := range roles {
				found = found || r.Name == in.Owner && r.CanLogin
			}
			if !found {
				return nil, invalid("所选数据库所有者不存在或无法登录")
			}
			_, err = conn.Exec(ctx, "CREATE DATABASE "+name+" OWNER "+owner+" TEMPLATE template0 ENCODING 'UTF8'")
			return nil, err
		}
		found := false
		for _, db := range databases {
			if db.Name == in.Name && db.OID == in.OID {
				if db.Protected {
					return nil, invalid("不能删除 PostgreSQL 系统库或模板库")
				}
				found = true
			}
		}
		if !found {
			return nil, invalid("数据库已删除或变化，请刷新后重新确认")
		}
		_, err = conn.Exec(ctx, "DROP DATABASE "+name)
		return nil, err
	case "create_role", "drop_role", "role_password":
		name, err := postgresName(in.Name, action == "create_role")
		if err != nil {
			return nil, err
		}
		tx, err := conn.Begin(ctx)
		if err != nil {
			return nil, err
		}
		defer tx.Rollback(context.Background())
		if _, err = tx.Exec(ctx, "LOCK TABLE pg_catalog.pg_authid IN SHARE ROW EXCLUSIVE MODE"); err != nil {
			return nil, err
		}
		roles, err := postgresJSON[[]postgresRole](ctx, tx, postgresRolesSQL)
		if err != nil {
			return nil, err
		}
		var target *postgresRole
		for i := range roles {
			if roles[i].Name == in.Name {
				target = &roles[i]
				break
			}
		}
		if action == "create_role" {
			if target != nil {
				return nil, invalid("同名账号已存在，未修改密码或权限")
			}
			err = postgresSetRoleSecret(ctx, conn, "CREATE ROLE "+name+" LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS", in.Password)
		} else {
			if target == nil || target.OID != in.OID {
				return nil, invalid("账号已删除或变化，请刷新后重新确认")
			}
			if target.Protected {
				return nil, invalid("系统或超级用户不能在普通账号管理中修改")
			}
			if action == "drop_role" {
				_, err = tx.Exec(ctx, "DROP ROLE "+name)
			} else {
				if !target.CanLogin {
					return nil, invalid("角色未启用登录，不能设置登录密码")
				}
				err = postgresSetRoleSecret(ctx, conn, "ALTER ROLE "+name, in.Password)
			}
		}
		if err != nil {
			return nil, err
		}
		return nil, tx.Commit(ctx)
	case "query":
		if !in.Request.Confirmed {
			return nil, invalid("请确认目标数据库和完整 SQL 后执行")
		}
		if in.Request.SQL == "" || len(in.Request.SQL) > 1024*1024 || strings.ContainsRune(in.Request.SQL, 0) {
			return nil, invalid("SQL 为空、过长或包含无效字符")
		}
		databases, err := postgresJSON[[]postgresDatabase](ctx, conn, postgresDatabasesSQL)
		if err != nil {
			return nil, err
		}
		found := false
		for _, db := range databases {
			found = found || db.Name == in.Request.Database && db.AllowConnections
		}
		if !found {
			return nil, invalid("所选数据库不存在或不允许连接")
		}
		target, err := c.postgresConnection(ctx, e, password, in.Request.Database)
		if err != nil {
			return nil, err
		}
		defer closePostgres(target)
		// Native protocol execution never interprets psql shell/meta commands.
		return postgresQuery(ctx, target, in.Request.SQL)
	}
	return nil, invalid("未知 PostgreSQL 操作")
}
func postgresSaveRoleAccess(ctx context.Context, conn *pgx.Conn, in postgresRoleAccessInput) (any, error) {
	name, err := postgresName(in.Name, false)
	if err != nil {
		return nil, err
	}
	if in.ConnectionLimit < -1 || len(in.Revision) != 32 {
		return nil, invalid("连接数上限或账号版本无效")
	}
	tx, err := conn.Begin(ctx)
	if err != nil {
		return nil, err
	}
	defer tx.Rollback(context.Background())
	if _, err = tx.Exec(ctx, "LOCK TABLE pg_catalog.pg_authid IN SHARE ROW EXCLUSIVE MODE"); err != nil {
		return nil, err
	}
	before, err := postgresJSON[postgresRoleAccess](ctx, tx, postgresRoleAccessSQL, in.OID)
	if err != nil {
		return nil, err
	}
	if before.Name != in.Name || before.Revision != in.Revision {
		return nil, invalid("账号已变化，请重新读取")
	}
	if before.Protected {
		return nil, invalid("系统账号或超级用户的连接设置受保护")
	}
	if !in.ConfirmRestriction && (before.CanLogin && !in.CanLogin || in.ConnectionLimit >= 0 && (before.ConnectionLimit == -1 || in.ConnectionLimit < before.ConnectionLimit)) {
		return nil, invalid("请先确认暂停登录或降低连接数上限的影响")
	}
	login := "NOLOGIN"
	if in.CanLogin {
		login = "LOGIN"
	}
	if _, err = tx.Exec(ctx, fmt.Sprintf("ALTER ROLE %s %s CONNECTION LIMIT %d", name, login, in.ConnectionLimit)); err != nil {
		return nil, err
	}
	after, err := postgresJSON[postgresRoleAccess](ctx, tx, postgresRoleAccessSQL, in.OID)
	if err != nil {
		return nil, err
	}
	if after.Name != in.Name || after.CanLogin != in.CanLogin || after.ConnectionLimit != in.ConnectionLimit {
		return nil, invalid("未能确认账号设置已保存")
	}
	if err = tx.Commit(ctx); err != nil {
		return nil, err
	}
	return after, nil
}
func postgresQuery(ctx context.Context, conn *pgx.Conn, sql string) (any, error) {
	columns := []string{}
	rows := [][]string{}
	size := 0
	multi := conn.PgConn().Exec(ctx, sql)
	for multi.NextResult() {
		result := multi.ResultReader()
		fields := result.FieldDescriptions()
		if len(fields) > 256 {
			closePostgres(conn)
			return nil, invalid("查询字段超过 256 个")
		}
		if len(fields) > 0 {
			if len(columns) > 0 {
				closePostgres(conn)
				return nil, invalid("一次查询返回多个结果集，请分别执行；已执行的写入不会因此撤销")
			}
			for _, field := range fields {
				columns = append(columns, field.Name)
			}
		}
		for result.NextRow() {
			row := []string{}
			for _, value := range result.Values() {
				size += len(value)
				row = append(row, string(value))
			}
			if size > 16*1024*1024 || len(rows) >= 20000 {
				closePostgres(conn)
				return nil, invalid("查询结果超过 16 MiB 或 20,000 行，请使用 LIMIT；已执行的写入不会因此撤销")
			}
			rows = append(rows, row)
		}
		if _, err := result.Close(); err != nil {
			_ = multi.Close()
			return nil, err
		}
	}
	if err := multi.Close(); err != nil {
		return nil, err
	}
	return map[string]any{"columns": columns, "rows": rows}, nil
}
