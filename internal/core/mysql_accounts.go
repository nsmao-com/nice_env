package core

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"regexp"
	"slices"
	"strings"
	"time"
	"unicode"
)

type mysqlAccountInput struct {
	Username     string   `json:"username"`
	Host         string   `json:"host"`
	Password     string   `json:"password"`
	Revision     string   `json:"revision"`
	Confirmation string   `json:"confirmation"`
	Target       string   `json:"target"`
	NewDatabase  bool     `json:"newDatabase"`
	Privileges   []string `json:"privileges"`
	GrantOption  bool     `json:"grantOption"`
}
type mysqlPasswordInfo struct {
	Username            string   `json:"username"`
	Host                string   `json:"host"`
	Plugins             []string `json:"plugins"`
	TargetPlugin        string   `json:"targetPlugin"`
	Supported           bool     `json:"supported"`
	Protected           bool     `json:"protected"`
	OtherAuthentication bool     `json:"otherAuthentication"`
	Revision            string   `json:"revision"`
}
type mysqlDependency struct {
	Kind     string `json:"kind"`
	Database string `json:"database"`
	Name     string `json:"name"`
}
type mysqlDropInfo struct {
	Username            string            `json:"username"`
	Host                string            `json:"host"`
	Protected           bool              `json:"protected"`
	Dependencies        []mysqlDependency `json:"dependencies"`
	MoreDependencies    bool              `json:"moreDependencies"`
	RoleDependents      int               `json:"roleDependents"`
	ProxyDependents     int               `json:"proxyDependents"`
	UsernameConnections int               `json:"usernameConnections"`
	Revision            string            `json:"revision"`
}

func protectedAccount(user, host string) bool {
	return user == "" || host == "" || strings.EqualFold(user, "root") || strings.HasPrefix(strings.ToLower(user), "mysql.") || strings.EqualFold(user, "mariadb.sys")
}
func (c *Core) accountRevision(e *serviceEntry, value any) (string, error) {
	payload, err := json.Marshal(value)
	if err != nil {
		return "", err
	}
	c.services.mu.RLock()
	instance := e.revision
	c.services.mu.RUnlock()
	h := hmac.New(sha256.New, c.snapshotKey[:])
	h.Write([]byte(e.status.ID + "\x00" + instance + "\x00"))
	h.Write(payload)
	return hex.EncodeToString(h.Sum(nil)), nil
}
func mysqlField(name string) string { return "`" + strings.ReplaceAll(name, "`", "``") + "`" }
func mysqlHasColumn(columns []string, name string) bool {
	return slices.ContainsFunc(columns, func(column string) bool { return strings.EqualFold(column, name) })
}
func mysqlGrantLiteral(name string, partial bool) string {
	if partial {
		return name
	}
	return strings.NewReplacer("\\", "\\\\", "_", "\\_", "%", "\\%").Replace(name)
}
func mysqlPartialRevokes(ctx context.Context, db *sql.DB) (bool, error) {
	rows, err := db.QueryContext(ctx, "SHOW VARIABLES LIKE 'partial_revokes'")
	if err != nil {
		return false, err
	}
	defer rows.Close()
	if rows.Next() {
		var key, value string
		if err = rows.Scan(&key, &value); err != nil {
			return false, err
		}
		return strings.EqualFold(value, "ON"), nil
	}
	return false, rows.Err()
}
func mysqlStrings(ctx context.Context, db *sql.DB, query string, args ...any) ([]string, error) {
	rows, err := db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []string{}
	for rows.Next() {
		var value string
		if err = rows.Scan(&value); err != nil {
			return nil, err
		}
		out = append(out, value)
		if len(out) > 20000 {
			return nil, invalid("数据库元数据过多，请使用原生客户端")
		}
	}
	return out, rows.Err()
}
func mysqlColumns(ctx context.Context, db *sql.DB, table string) ([]string, error) {
	return mysqlStrings(ctx, db, "SELECT COLUMN_NAME FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='mysql' AND TABLE_NAME=? ORDER BY ORDINAL_POSITION", table)
}
func mysqlCount(ctx context.Context, db *sql.DB, query string, args ...any) (int, error) {
	var n int
	err := db.QueryRowContext(ctx, query, args...).Scan(&n)
	return n, err
}

func (c *Core) mysqlAccountCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[struct {
		Engine   string            `json:"engine"`
		Version  string            `json:"version"`
		Username string            `json:"username"`
		Host     string            `json:"host"`
		Input    mysqlAccountInput `json:"input"`
	}](data)
	if err != nil {
		return nil, err
	}
	a := in.Input
	if action == "grants" || action == "drop-info" || action == "password-info" {
		a.Username = in.Username
		a.Host = in.Host
	}
	if len(a.Username) > 384 || len(a.Host) > 1020 || strings.IndexFunc(a.Username+a.Host, unicode.IsControl) >= 0 {
		return nil, invalid("账号或来源主机无效")
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	e, err := c.selectedMySQL(ctx, in.Engine, in.Version)
	if err != nil {
		return nil, err
	}
	password, found, err := c.Store.setting(ctx, mysqlPasswordKey(e))
	if err != nil {
		return nil, err
	}
	if !found {
		return nil, invalid("请先保存 root 连接密码")
	}
	db, err := c.mysqlConnection(ctx, e, password)
	if err != nil {
		return nil, err
	}
	defer db.Close()
	switch action {
	case "password-info", "password-save":
		before, err := c.mysqlPasswordInfo(ctx, db, e, a)
		if err != nil {
			return nil, err
		}
		if action == "password-info" {
			return before, nil
		}
		if !before.Supported {
			return nil, invalid("此账号受保护或认证方式不支持改密，请使用专用管理入口")
		}
		if a.Revision == "" || a.Revision != before.Revision {
			return nil, invalid("认证信息已变化，请重新读取后保存")
		}
		if a.Password == "" || len(a.Password) > 4096 || strings.IndexFunc(a.Password, unicode.IsControl) >= 0 {
			return nil, invalid("密码须为 1–4096 字节且不能包含控制字符")
		}
		if _, err = db.ExecContext(ctx, "SET SESSION sql_log_off=ON"); err != nil {
			return nil, err
		}
		statement := "SET PASSWORD FOR ?@? = ?"
		if e.pkg.ID == "mariadb" {
			old := 0
			if before.TargetPlugin == "mysql_old_password" {
				old = 1
			}
			if _, err = db.ExecContext(ctx, fmt.Sprintf("SET SESSION old_passwords=%d", old)); err != nil {
				return nil, err
			}
			statement = "SET PASSWORD FOR ?@? = PASSWORD(?)"
		}
		if _, err = db.ExecContext(ctx, statement, a.Username, a.Host, a.Password); err != nil {
			return nil, &AppError{Code: "DB_PASSWORD_FAILED", Message: "未能确认密码修改，请检查密码策略并重新读取认证状态"}
		}
		after, err := c.mysqlPasswordInfo(ctx, db, e, a)
		if err != nil {
			return nil, err
		}
		if !slices.Equal(before.Plugins, after.Plugins) {
			return nil, invalid("密码操作后认证插件发生变化，请重新确认")
		}
		return after, nil
	case "drop-info", "drop":
		before, err := c.mysqlDropInfo(ctx, db, e, a)
		if err != nil {
			return nil, err
		}
		if action == "drop-info" {
			return before, nil
		}
		if before.Protected {
			return nil, invalid("系统账号或具有账号管理权限的账号受保护")
		}
		if len(before.Dependencies) > 0 || before.RoleDependents > 0 || before.ProxyDependents > 0 {
			return nil, invalid("账号仍有对象、角色或代理依赖，请先转移或解除依赖")
		}
		if a.Confirmation != a.Username+"@"+a.Host {
			return nil, invalid("请完整输入账号及来源主机")
		}
		if a.Revision == "" || a.Revision != before.Revision {
			return nil, invalid("账号或依赖已变化，请重新读取删除预览")
		}
		_, execErr := db.ExecContext(ctx, "DROP USER ?@?", a.Username, a.Host)
		n, err := mysqlCount(ctx, db, "SELECT COUNT(*) FROM mysql.user WHERE BINARY User=? AND BINARY Host=?", a.Username, a.Host)
		if err != nil {
			return nil, err
		}
		if n == 0 {
			return nil, nil
		}
		if execErr != nil {
			return nil, execErr
		}
		return nil, invalid("删除后仍存在同名账号，请刷新后确认")
	case "grants":
		return c.mysqlGrants(ctx, db, e, a)
	case "grants-save":
		return c.mysqlGrantsSave(ctx, db, e, a)
	}
	return nil, invalid("未知账号操作")
}

func (c *Core) mysqlPasswordInfo(ctx context.Context, db *sql.DB, e *serviceEntry, a mysqlAccountInput) (mysqlPasswordInfo, error) {
	info := mysqlPasswordInfo{Username: a.Username, Host: a.Host, Plugins: []string{}, Protected: protectedAccount(a.Username, a.Host)}
	cols, err := mysqlColumns(ctx, db, "user")
	if err != nil {
		return info, err
	}
	if !slices.Contains(cols, "plugin") || !slices.Contains(cols, "authentication_string") {
		return info, invalid("服务器认证结构不支持账号密码编辑")
	}
	attributes := "'{}'"
	if slices.Contains(cols, "User_attributes") {
		attributes = "COALESCE(User_attributes,'{}')"
	}
	var fingerprint string
	var other int
	err = db.QueryRowContext(ctx, "SELECT plugin,SHA2(CONCAT(COALESCE(authentication_string,''),"+attributes+"),256),JSON_CONTAINS_PATH("+attributes+",'one','$.additional_password','$.multi_factor_authentication') FROM mysql.user WHERE BINARY User=? AND BINARY Host=?", a.Username, a.Host).Scan(&info.TargetPlugin, &fingerprint, &other)
	if err == sql.ErrNoRows {
		return info, invalid("所选账号已不存在")
	}
	if err != nil {
		return info, err
	}
	info.Plugins = append(info.Plugins, info.TargetPlugin)
	info.OtherAuthentication = other != 0
	known := []string{"mysql_native_password", "caching_sha2_password", "sha256_password"}
	if e.pkg.ID == "mariadb" {
		tables, err := mysqlStrings(ctx, db, "SHOW TABLES FROM mysql")
		if err != nil {
			return info, err
		}
		if slices.Contains(tables, "global_priv") {
			var count int
			if err = db.QueryRowContext(ctx, "SELECT COALESCE(JSON_LENGTH(Priv,'$.auth_or'),0),SHA2(Priv,256) FROM mysql.global_priv WHERE BINARY User=? AND BINARY Host=?", a.Username, a.Host).Scan(&count, &fingerprint); err != nil {
				return info, err
			}
			if count < 0 || count > 256 {
				return info, invalid("账号认证方式数量无效")
			}
			if count > 0 {
				fields := []string{}
				for i := 0; i < count; i++ {
					fields = append(fields, fmt.Sprintf("JSON_EXTRACT(Priv,'$.auth_or[%d].plugin')", i))
				}
				var raw string
				if err = db.QueryRowContext(ctx, "SELECT JSON_ARRAY("+strings.Join(fields, ",")+") FROM mysql.global_priv WHERE BINARY User=? AND BINARY Host=?", a.Username, a.Host).Scan(&raw); err != nil {
					return info, err
				}
				var plugins []*string
				if err = json.Unmarshal([]byte(raw), &plugins); err != nil {
					return info, err
				}
				primary := info.TargetPlugin
				info.Plugins = []string{}
				for _, plugin := range plugins {
					if plugin == nil {
						info.Plugins = append(info.Plugins, primary)
					} else {
						info.Plugins = append(info.Plugins, *plugin)
					}
				}
			}
		}
		info.TargetPlugin = info.Plugins[0]
		info.OtherAuthentication = len(info.Plugins) > 1
		known = []string{"mysql_native_password", "mysql_old_password", "ed25519"}
	}
	info.Supported = !info.Protected && slices.Contains(known, info.TargetPlugin)
	info.Revision, err = c.accountRevision(e, []any{info, fingerprint})
	return info, err
}

func (c *Core) mysqlDropInfo(ctx context.Context, db *sql.DB, e *serviceEntry, a mysqlAccountInput) (mysqlDropInfo, error) {
	info := mysqlDropInfo{Username: a.Username, Host: a.Host, Protected: protectedAccount(a.Username, a.Host), Dependencies: []mysqlDependency{}}
	tables, err := mysqlStrings(ctx, db, "SHOW TABLES FROM mysql")
	if err != nil {
		return info, err
	}
	snapshots := map[string][]string{}
	for _, table := range []string{"user", "global_priv", "db", "tables_priv", "columns_priv", "procs_priv", "global_grants", "default_roles", "role_edges", "roles_mapping", "proxies_priv"} {
		if !slices.Contains(tables, table) {
			if slices.Contains([]string{"user", "db", "tables_priv", "columns_priv", "procs_priv", "proxies_priv"}, table) {
				return info, invalid("授权表不完整，无法检查删除影响")
			}
			continue
		}
		cols, err := mysqlColumns(ctx, db, table)
		if err != nil {
			return info, err
		}
		required := []string{"User", "Host"}
		predicate := "BINARY User=? AND BINARY Host=?"
		args := []any{a.Username, a.Host}
		if table == "role_edges" {
			required = []string{"FROM_USER", "FROM_HOST", "TO_USER", "TO_HOST"}
			predicate = "(BINARY FROM_USER=? AND BINARY FROM_HOST=?) OR (BINARY TO_USER=? AND BINARY TO_HOST=?)"
			args = append(args, a.Username, a.Host)
		}
		if table == "roles_mapping" {
			required = []string{"User", "Host", "Role"}
			predicate = "(BINARY User=? AND BINARY Host=?) OR BINARY Role=?"
			args = append(args, a.Username)
		}
		if table == "proxies_priv" {
			required = append(required, "Proxied_user", "Proxied_host")
			predicate = "(BINARY User=? AND BINARY Host=?) OR (BINARY Proxied_user=? AND BINARY Proxied_host=?)"
			args = append(args, a.Username, a.Host)
		}
		for _, col := range required {
			if !mysqlHasColumn(cols, col) {
				return info, invalid("授权表结构不支持删除检查")
			}
		}
		values := []string{}
		for _, col := range cols {
			values = append(values, "HEX("+mysqlField(col)+")")
		}
		snapshot, err := mysqlStrings(ctx, db, "SELECT SHA2(JSON_ARRAY("+strings.Join(values, ",")+"),256) FROM mysql."+mysqlField(table)+" WHERE "+predicate+" ORDER BY 1", args...)
		if err != nil {
			return info, err
		}
		snapshots[table] = snapshot
		if table == "user" {
			if len(snapshot) != 1 {
				return info, invalid("所选账号不存在或结构异常")
			}
			for _, col := range []string{"Select_priv", "Show_view_priv", "Trigger_priv", "Event_priv", "Super_priv", "Create_user_priv"} {
				if !slices.Contains(cols, col) {
					return info, invalid("无法确认完整对象查看权限")
				}
			}
			restrictions := ""
			if slices.Contains(cols, "User_attributes") {
				restrictions = " AND COALESCE(JSON_LENGTH(User_attributes,'$.Restrictions'),0)=0"
			}
			n, err := mysqlCount(ctx, db, "SELECT COUNT(*) FROM mysql.user WHERE BINARY CONCAT(User,'@',Host)=BINARY CURRENT_USER() AND Select_priv='Y' AND Show_view_priv='Y' AND Trigger_priv='Y' AND Event_priv='Y'"+restrictions)
			if err != nil {
				return info, err
			}
			if n != 1 {
				return info, invalid("管理连接缺少完整对象查看权限，无法确认删除影响")
			}
			n, err = mysqlCount(ctx, db, "SELECT COUNT(*) FROM mysql.user WHERE BINARY User=? AND BINARY Host=? AND (Super_priv='Y' OR Create_user_priv='Y')", a.Username, a.Host)
			if err != nil {
				return info, err
			}
			info.Protected = info.Protected || n > 0
		}
		if table == "global_grants" {
			n, err := mysqlCount(ctx, db, "SELECT COUNT(*) FROM mysql.global_grants WHERE BINARY User=? AND BINARY Host=? AND Priv IN ('SYSTEM_USER','ROLE_ADMIN')", a.Username, a.Host)
			if err != nil {
				return info, err
			}
			info.Protected = info.Protected || n > 0
		}
	}
	queries := []string{}
	args := []any{}
	for _, entry := range [][4]string{{"view", "VIEWS", "TABLE_SCHEMA", "TABLE_NAME"}, {"routine", "ROUTINES", "ROUTINE_SCHEMA", "ROUTINE_NAME"}, {"trigger", "TRIGGERS", "TRIGGER_SCHEMA", "TRIGGER_NAME"}, {"event", "EVENTS", "EVENT_SCHEMA", "EVENT_NAME"}} {
		queries = append(queries, "SELECT '"+entry[0]+"' AS kind,"+entry[2]+" AS db_name,"+entry[3]+" AS object_name FROM information_schema."+entry[1]+" WHERE BINARY DEFINER=?")
		args = append(args, a.Username+"@"+a.Host)
	}
	rows, err := db.QueryContext(ctx, strings.Join(queries, " UNION ALL ")+" ORDER BY kind,db_name,object_name LIMIT 51", args...)
	if err != nil {
		return info, err
	}
	for rows.Next() {
		var dep mysqlDependency
		if err = rows.Scan(&dep.Kind, &dep.Database, &dep.Name); err != nil {
			rows.Close()
			return info, err
		}
		info.Dependencies = append(info.Dependencies, dep)
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return info, err
	}
	info.MoreDependencies = len(info.Dependencies) > 50
	if info.MoreDependencies {
		info.Dependencies = info.Dependencies[:50]
	}
	if slices.Contains(tables, "role_edges") {
		info.RoleDependents, err = mysqlCount(ctx, db, "SELECT COUNT(*) FROM mysql.role_edges WHERE BINARY FROM_USER=? AND BINARY FROM_HOST=?", a.Username, a.Host)
		if err != nil {
			return info, err
		}
	}
	if slices.Contains(tables, "roles_mapping") && a.Host == "" {
		info.RoleDependents, err = mysqlCount(ctx, db, "SELECT COUNT(*) FROM mysql.roles_mapping WHERE BINARY Role=?", a.Username)
		if err != nil {
			return info, err
		}
	}
	info.ProxyDependents, err = mysqlCount(ctx, db, "SELECT COUNT(*) FROM mysql.proxies_priv WHERE BINARY Proxied_user=? AND BINARY Proxied_host=? AND NOT (BINARY User=? AND BINARY Host=?)", a.Username, a.Host, a.Username, a.Host)
	if err != nil {
		return info, err
	}
	info.Revision, err = c.accountRevision(e, []any{info, snapshots})
	if err != nil {
		return info, err
	}
	info.UsernameConnections, err = mysqlCount(ctx, db, "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE BINARY USER=?", a.Username)
	return info, err
}

var mysqlDatabasePrivileges = [][2]string{{"Select_priv", "SELECT"}, {"Insert_priv", "INSERT"}, {"Update_priv", "UPDATE"}, {"Delete_priv", "DELETE"}, {"Create_priv", "CREATE"}, {"Drop_priv", "DROP"}, {"References_priv", "REFERENCES"}, {"Index_priv", "INDEX"}, {"Alter_priv", "ALTER"}, {"Create_tmp_table_priv", "CREATE TEMPORARY TABLES"}, {"Lock_tables_priv", "LOCK TABLES"}, {"Create_view_priv", "CREATE VIEW"}, {"Show_view_priv", "SHOW VIEW"}, {"Create_routine_priv", "CREATE ROUTINE"}, {"Alter_routine_priv", "ALTER ROUTINE"}, {"Execute_priv", "EXECUTE"}, {"Event_priv", "EVENT"}, {"Trigger_priv", "TRIGGER"}}

type mysqlGrantScope struct {
	Scope           string   `json:"scope"`
	Label           string   `json:"label"`
	Pattern         bool     `json:"pattern"`
	Protected       bool     `json:"protected"`
	Privileges      []string `json:"privileges"`
	GrantOption     bool     `json:"grantOption"`
	ExtraPrivileges []string `json:"extraPrivileges"`
}
type mysqlGrantView struct {
	Username         string            `json:"username"`
	Host             string            `json:"host"`
	Scopes           []mysqlGrantScope `json:"scopes"`
	Databases        []string          `json:"databases"`
	Available        []string          `json:"available"`
	PartialRevokes   bool              `json:"partialRevokes"`
	MariaDB          bool              `json:"mariadb"`
	GlobalPrivileges bool              `json:"globalPrivileges"`
	Protected        bool              `json:"protected"`
	Revision         string            `json:"revision"`
}

func mysqlGrantDetails(scope string, partial bool) mysqlGrantScope {
	out := mysqlGrantScope{Scope: scope, Privileges: []string{}, ExtraPrivileges: []string{}}
	pattern := "(?i)^"
	runes := []rune(scope)
	for i := 0; i < len(runes); i++ {
		ch := runes[i]
		if !partial && ch == '\\' && i+1 < len(runes) {
			i++
			ch = runes[i]
			out.Label += string(ch)
			pattern += regexp.QuoteMeta(string(ch))
		} else if !partial && (ch == '%' || ch == '_') {
			out.Pattern = true
			out.Label += string(ch)
			if ch == '%' {
				pattern += ".*"
			} else {
				pattern += "."
			}
		} else {
			out.Label += string(ch)
			pattern += regexp.QuoteMeta(string(ch))
		}
	}
	matcher := regexp.MustCompile(pattern + "$")
	for _, name := range []string{"mysql", "sys", "information_schema", "performance_schema"} {
		out.Protected = out.Protected || matcher.MatchString(name)
	}
	return out
}
func (c *Core) mysqlGrants(ctx context.Context, db *sql.DB, e *serviceEntry, a mysqlAccountInput) (mysqlGrantView, error) {
	out := mysqlGrantView{Username: a.Username, Host: a.Host, Protected: protectedAccount(a.Username, a.Host), Scopes: []mysqlGrantScope{}, Databases: []string{}, Available: []string{}, MariaDB: e.pkg.ID == "mariadb"}
	n, err := mysqlCount(ctx, db, "SELECT COUNT(*) FROM mysql.user WHERE BINARY User=? AND BINARY Host=?", a.Username, a.Host)
	if err != nil {
		return out, err
	}
	if n != 1 {
		return out, invalid("所选账号不存在")
	}
	cols, err := mysqlColumns(ctx, db, "db")
	if err != nil {
		return out, err
	}
	if !slices.Contains(cols, "Grant_priv") {
		return out, invalid("服务器授权表结构不支持编辑")
	}
	selected := []string{}
	for _, col := range cols {
		if strings.HasSuffix(col, "_priv") {
			selected = append(selected, col)
		}
	}
	for _, p := range mysqlDatabasePrivileges {
		if slices.Contains(selected, p[0]) {
			out.Available = append(out.Available, p[1])
		}
	}
	if out.PartialRevokes, err = mysqlPartialRevokes(ctx, db); err != nil {
		return out, err
	}
	fields := []string{"Db"}
	for _, col := range selected {
		fields = append(fields, mysqlField(col))
	}
	rows, err := db.QueryContext(ctx, "SELECT "+strings.Join(fields, ",")+" FROM mysql.db WHERE BINARY User=? AND BINARY Host=? ORDER BY BINARY Db", a.Username, a.Host)
	if err != nil {
		return out, err
	}
	for rows.Next() {
		values := make([]string, len(fields))
		dest := make([]any, len(fields))
		for i := range values {
			dest[i] = &values[i]
		}
		if err = rows.Scan(dest...); err != nil {
			rows.Close()
			return out, err
		}
		scope := mysqlGrantDetails(values[0], out.PartialRevokes)
		for i, col := range selected {
			if values[i+1] != "Y" {
				continue
			}
			if col == "Grant_priv" {
				scope.GrantOption = true
				continue
			}
			known := false
			for _, p := range mysqlDatabasePrivileges {
				if p[0] == col {
					scope.Privileges = append(scope.Privileges, p[1])
					known = true
					break
				}
			}
			if !known {
				scope.ExtraPrivileges = append(scope.ExtraPrivileges, col)
			}
		}
		out.Scopes = append(out.Scopes, scope)
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return out, err
	}
	cols, err = mysqlColumns(ctx, db, "user")
	if err != nil {
		return out, err
	}
	checks := []string{}
	for _, col := range cols {
		if strings.HasSuffix(col, "_priv") {
			checks = append(checks, mysqlField(col)+"='Y'")
		}
	}
	if len(checks) > 0 {
		n, err = mysqlCount(ctx, db, "SELECT COUNT(*) FROM mysql.user WHERE BINARY User=? AND BINARY Host=? AND ("+strings.Join(checks, " OR ")+")", a.Username, a.Host)
		if err != nil {
			return out, err
		}
		out.GlobalPrivileges = n > 0
	}
	databases, err := mysqlStrings(ctx, db, "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME")
	if err != nil {
		return out, err
	}
	for _, name := range databases {
		if !protectedDatabase(name) {
			out.Databases = append(out.Databases, name)
		}
	}
	out.Revision, err = c.accountRevision(e, out)
	return out, err
}
func (c *Core) mysqlGrantsSave(ctx context.Context, db *sql.DB, e *serviceEntry, a mysqlAccountInput) (mysqlGrantView, error) {
	before, err := c.mysqlGrants(ctx, db, e, a)
	if err != nil {
		return before, err
	}
	if before.Protected || (before.PartialRevokes && before.GlobalPrivileges) {
		return before, invalid("此账号的授权受保护，请使用数据库客户端管理")
	}
	if a.Revision == "" || a.Revision != before.Revision {
		return before, invalid("授权或数据库列表已变化，请重新读取")
	}
	for _, p := range a.Privileges {
		if !slices.Contains(before.Available, p) {
			return before, invalid("选择的权限不受此实例支持")
		}
	}
	target := a.Target
	if a.NewDatabase {
		if !slices.Contains(before.Databases, target) {
			return before, invalid("所选数据库不存在")
		}
		if !before.PartialRevokes {
			target = strings.NewReplacer("\\", "\\\\", "_", "\\_", "%", "\\%").Replace(target)
		}
	}
	scope := mysqlGrantDetails(target, before.PartialRevokes)
	if scope.Protected {
		return before, invalid("不允许修改覆盖系统数据库的权限")
	}
	exists := false
	for _, item := range before.Scopes {
		if item.Scope == target {
			scope = item
			exists = true
			break
		}
	}
	if !a.NewDatabase && !exists {
		return before, invalid("授权范围已不存在")
	}
	wanted := slices.Clone(a.Privileges)
	slices.Sort(wanted)
	wanted = slices.Compact(wanted)
	statements := []string{}
	targetSQL := mysqlField(target) + ".*"
	if scope.GrantOption && !a.GrantOption {
		statements = append(statements, "REVOKE GRANT OPTION ON "+targetSQL+" FROM ?@?")
	}
	for _, p := range scope.Privileges {
		if !slices.Contains(wanted, p) {
			statements = append(statements, "REVOKE "+p+" ON "+targetSQL+" FROM ?@?")
		}
	}
	for _, p := range wanted {
		if !slices.Contains(scope.Privileges, p) {
			statements = append(statements, "GRANT "+p+" ON "+targetSQL+" TO ?@?")
		}
	}
	if a.GrantOption && !scope.GrantOption {
		statements = append(statements, "GRANT USAGE ON "+targetSQL+" TO ?@? WITH GRANT OPTION")
	}
	for _, statement := range statements {
		if _, err = db.ExecContext(ctx, statement, a.Username, a.Host); err != nil {
			return before, &AppError{Code: "DATABASE_GRANTS_PARTIAL", Message: "授权未全部保存，已执行的语句可能生效，请重新读取当前授权", Detail: err.Error()}
		}
	}
	after, err := c.mysqlGrants(ctx, db, e, a)
	if err != nil {
		return after, err
	}
	actual := []string{}
	option := false
	for _, item := range after.Scopes {
		if item.Scope == target {
			actual = slices.Clone(item.Privileges)
			option = item.GrantOption
		}
	}
	slices.Sort(actual)
	if after.PartialRevokes != before.PartialRevokes || !slices.Equal(actual, wanted) || option != a.GrantOption {
		return after, invalid("保存后权限与选择不一致，请重新读取")
	}
	return after, nil
}
