package core

import (
	"context"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"
)

type databaseGrid struct {
	Columns []string    `json:"columns"`
	Rows    [][]*string `json:"rows"`
}
type databaseFilter struct {
	Column   string  `json:"column"`
	Operator string  `json:"operator"`
	Value    *string `json:"value"`
}
type workspaceRequest struct {
	Database   string           `json:"database"`
	Action     string           `json:"action"`
	Table      string           `json:"table"`
	Offset     int              `json:"offset"`
	SQL        string           `json:"sql"`
	Confirmed  bool             `json:"confirmed"`
	Column     string           `json:"column"`
	Value      *string          `json:"value"`
	Original   []*string        `json:"original"`
	PageSize   int              `json:"pageSize"`
	Filters    []databaseFilter `json:"filters"`
	SortColumn string           `json:"sortColumn"`
	Descending bool             `json:"descending"`
}

func sqlIdent(value string) (string, error) {
	if value == "" || len(value) > 256 || strings.IndexFunc(value, unicode.IsControl) >= 0 {
		return "", invalid("数据库标识无效")
	}
	return "`" + strings.ReplaceAll(value, "`", "``") + "`", nil
}
func mysqlLiteral(value *string) string {
	if value == nil {
		return "NULL"
	}
	return "CONVERT(X'" + hex.EncodeToString([]byte(*value)) + "' USING utf8mb4)"
}
func sqlGrids(ctx context.Context, conn *sql.Conn, statement string, args ...any) ([]databaseGrid, error) {
	if len(statement) > 1024*1024 || strings.ContainsRune(statement, 0) || strings.TrimSpace(statement) == "" {
		return nil, invalid("SQL 为空、过长或含无效字符")
	}
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	rows, err := conn.QueryContext(ctx, statement, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := []databaseGrid{}
	cells, total := 0, 0
	for {
		columns, err := rows.Columns()
		if err != nil {
			return nil, err
		}
		if len(columns) > 512 {
			return nil, invalid("查询结果超过 512 列")
		}
		grid := databaseGrid{Columns: columns, Rows: [][]*string{}}
		for rows.Next() {
			values := make([]sql.RawBytes, len(columns))
			refs := make([]any, len(columns))
			for i := range values {
				refs[i] = &values[i]
			}
			if err = rows.Scan(refs...); err != nil {
				return nil, err
			}
			row := make([]*string, len(columns))
			for i, v := range values {
				if v != nil {
					text := string(v)
					if !utf8.Valid(v) {
						text = "0x" + hex.EncodeToString(v)
					}
					row[i] = &text
					total += len(text)
				}
			}
			cells += len(columns)
			if cells > 200000 || total > 16*1024*1024 || len(grid.Rows) >= 20000 {
				return nil, &AppError{Code: "SQL_RESULT_TOO_LARGE", Message: "结果超过显示限制，请缩小查询范围；已执行的写入不会撤销"}
			}
			grid.Rows = append(grid.Rows, row)
		}
		if err = rows.Err(); err != nil {
			return nil, err
		}
		if len(columns) > 0 {
			out = append(out, grid)
		}
		if len(out) > 32 {
			return nil, invalid("单次最多显示 32 个结果集")
		}
		if !rows.NextResultSet() {
			break
		}
	}
	return out, rows.Err()
}
func (c *Core) databaseWorkspace(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[struct {
		Engine  string           `json:"engine"`
		Version string           `json:"version"`
		Request workspaceRequest `json:"request"`
	}](data)
	if err != nil {
		return nil, err
	}
	r := in.Request
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
		return nil, invalid("请先保存数据库凭据")
	}
	db, err := c.mysqlConnection(ctx, e, password)
	if err != nil {
		return nil, err
	}
	defer db.Close()
	conn, err := db.Conn(ctx)
	if err != nil {
		return nil, err
	}
	defer conn.Close()
	database, err := sqlIdent(r.Database)
	if err != nil {
		return nil, err
	}
	var exists int
	if err = conn.QueryRowContext(ctx, "SELECT COUNT(*) FROM information_schema.SCHEMATA WHERE SCHEMA_NAME=?", r.Database).Scan(&exists); err != nil {
		return nil, err
	}
	if exists != 1 {
		return nil, invalid("所选数据库不存在")
	}
	if _, err = conn.ExecContext(ctx, "USE "+database); err != nil {
		return nil, err
	}
	tables, err := sqlGrids(ctx, conn, "SHOW FULL TABLES")
	if err != nil {
		return nil, err
	}
	if r.Action == "tables" {
		return tables, nil
	}
	if r.Action == "sql" {
		if !r.Confirmed {
			return nil, &AppError{Code: "SQL_CONFIRM_REQUIRED", Message: "请确认目标数据库和完整 SQL 后执行"}
		}
		result, err := sqlGrids(ctx, conn, r.SQL)
		if err != nil {
			detail := err.Error()
			if password != "" {
				detail = strings.ReplaceAll(detail, password, "***")
			}
			return nil, &AppError{Code: "SQL_FAILED", Message: "SQL 执行失败；之前成功的写入可能已生效", Detail: detail}
		}
		return result, nil
	}
	table, err := sqlIdent(r.Table)
	if err != nil {
		return nil, err
	}
	kind := ""
	for _, grid := range tables {
		for _, row := range grid.Rows {
			if len(row) > 1 && row[0] != nil && *row[0] == r.Table && row[1] != nil {
				kind = *row[1]
			}
		}
	}
	if kind == "" {
		return nil, invalid("所选表不存在")
	}
	target := database + "." + table
	if r.Action == "schema" {
		return sqlGrids(ctx, conn, "SHOW FULL COLUMNS FROM "+target+"; SHOW INDEX FROM "+target+"; SHOW CREATE TABLE "+target+"; SELECT COLUMN_NAME, REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME, CONSTRAINT_NAME FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA="+mysqlLiteral(&r.Database)+" AND TABLE_NAME="+mysqlLiteral(&r.Table)+" AND REFERENCED_TABLE_NAME IS NOT NULL ORDER BY CONSTRAINT_NAME,ORDINAL_POSITION")
	}
	schema, err := sqlGrids(ctx, conn, "SHOW FULL COLUMNS FROM "+target)
	if err != nil {
		return nil, err
	}
	if len(schema) != 1 {
		return nil, invalid("无法读取表结构")
	}
	grid := schema[0]
	field, key, typ, extra := slices.Index(grid.Columns, "Field"), slices.Index(grid.Columns, "Key"), slices.Index(grid.Columns, "Type"), slices.Index(grid.Columns, "Extra")
	if field < 0 || key < 0 || typ < 0 || extra < 0 {
		return nil, invalid("表结构不完整")
	}
	fields, primary := []string{}, []string{}
	editable := true
	for _, row := range grid.Rows {
		if row[field] == nil {
			return nil, invalid("表字段名称无效")
		}
		name := *row[field]
		fields = append(fields, name)
		if row[key] != nil && *row[key] == "PRI" {
			primary = append(primary, name)
		}
		if row[typ] != nil {
			t := strings.ToLower(strings.SplitN(*row[typ], "(", 2)[0])
			if slices.Contains([]string{"blob", "tinyblob", "mediumblob", "longblob", "binary", "varbinary", "bit", "geometry", "geometrycollection", "point", "multipoint", "polygon", "multipolygon", "linestring", "multilinestring"}, t) {
				editable = false
			}
		}
		if row[extra] != nil && strings.Contains(strings.ToLower(*row[extra]), "generated") {
			editable = false
		}
	}
	checkedColumn := func(name string) (string, error) {
		if !slices.Contains(fields, name) {
			return "", invalid("筛选或编辑字段不存在")
		}
		return sqlIdent(name)
	}
	if r.Action == "rows" {
		if r.PageSize == 0 {
			r.PageSize = 100
		}
		if !slices.Contains([]int{25, 50, 100, 250, 500}, r.PageSize) || r.Offset < 0 || r.Offset > 10000000 || len(r.Filters) > 8 {
			return nil, invalid("分页或筛选数量超出范围")
		}
		predicates := []string{}
		filterArgs := []any{}
		for _, filter := range r.Filters {
			column, err := checkedColumn(filter.Column)
			if err != nil {
				return nil, err
			}
			if filter.Operator == "null" {
				predicates = append(predicates, column+" IS NULL")
				continue
			}
			if filter.Operator == "not_null" {
				predicates = append(predicates, column+" IS NOT NULL")
				continue
			}
			if filter.Value == nil || len(*filter.Value) > 4096 {
				return nil, invalid("筛选值缺失或超过 4096 字节")
			}
			if filter.Operator == "contains" {
				value := "%" + strings.NewReplacer("!", "!!", "%", "!%", "_", "!_").Replace(*filter.Value) + "%"
				predicates = append(predicates, column+" LIKE ? ESCAPE '!'")
				filterArgs = append(filterArgs, value)
				continue
			}
			op := map[string]string{"eq": "=", "ne": "<>", "gt": ">", "gte": ">=", "lt": "<", "lte": "<="}[filter.Operator]
			if op == "" {
				return nil, invalid("不支持的筛选操作")
			}
			predicates = append(predicates, column+op+"?")
			filterArgs = append(filterArgs, *filter.Value)
		}
		statement := "SELECT * FROM " + target
		if len(predicates) > 0 {
			statement += " WHERE " + strings.Join(predicates, " AND ")
		}
		order := []string{}
		if r.SortColumn != "" {
			column, err := checkedColumn(r.SortColumn)
			if err != nil {
				return nil, err
			}
			direction := " ASC"
			if r.Descending {
				direction = " DESC"
			}
			order = append(order, column+direction)
		}
		for _, name := range primary {
			if name != r.SortColumn {
				column, _ := sqlIdent(name)
				order = append(order, column)
			}
		}
		if len(order) > 0 {
			statement += " ORDER BY " + strings.Join(order, ",")
		}
		statement += fmt.Sprintf(" LIMIT %d OFFSET %d", r.PageSize+1, r.Offset)
		return sqlGrids(ctx, conn, statement, filterArgs...)
	}
	if r.Action != "update" || !r.Confirmed {
		return nil, invalid("未知操作或尚未确认修改")
	}
	if protectedDatabase(r.Database) || kind != "BASE TABLE" || len(primary) == 0 || !editable {
		return nil, &AppError{Code: "TABLE_READ_ONLY", Message: "系统表、视图、无主键或含二进制/生成字段的表请使用 SQL 编辑"}
	}
	column, err := checkedColumn(r.Column)
	if err != nil {
		return nil, err
	}
	if len(r.Original) != len(fields) {
		return nil, invalid("缺少完整原始行，请刷新数据")
	}
	size := 0
	predicates := []string{}
	for i, value := range r.Original {
		if value != nil {
			size += len(*value)
		}
		field, _ := sqlIdent(fields[i])
		predicates = append(predicates, "BINARY "+field+" <=> BINARY "+mysqlLiteral(value))
	}
	if r.Value != nil {
		size += len(*r.Value)
	}
	if size > 1024*1024 {
		return nil, invalid("行内容过大")
	}
	index := slices.Index(fields, r.Column)
	if r.Value == nil && r.Original[index] == nil || r.Value != nil && r.Original[index] != nil && *r.Value == *r.Original[index] {
		return []databaseGrid{}, nil
	}
	result, err := conn.ExecContext(ctx, "UPDATE "+target+" SET "+column+"="+mysqlLiteral(r.Value)+" WHERE "+strings.Join(predicates, " AND ")+" LIMIT 1")
	if err != nil {
		return nil, err
	}
	affected, err := result.RowsAffected()
	if err != nil {
		return nil, err
	}
	if affected != 1 {
		return nil, &AppError{Code: "ROW_CHANGED", Message: "数据已被其他连接修改，请刷新后重试"}
	}
	value := "1"
	return []databaseGrid{{Columns: []string{"affected"}, Rows: [][]*string{{&value}}}}, nil
}
