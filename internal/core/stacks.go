package core

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"sort"
	"strings"
	"time"
)

type StackItem struct {
	ServiceID string  `json:"serviceId"`
	Label     *string `json:"label,omitempty"`
	Order     int     `json:"order"`
}
type Stack struct {
	ID          string      `json:"id"`
	Name        string      `json:"name"`
	Description string      `json:"description"`
	Items       []StackItem `json:"items"`
	Builtin     bool        `json:"builtin"`
	CreatedAt   int64       `json:"createdAt"`
	UpdatedAt   int64       `json:"updatedAt"`
}
type StackInput struct {
	ID          *string     `json:"id"`
	Name        string      `json:"name"`
	Description string      `json:"description"`
	Items       []StackItem `json:"items"`
}

func newID(prefix string) (string, error) {
	var bytes [16]byte
	if _, err := rand.Read(bytes[:]); err != nil {
		return "", err
	}
	return prefix + "-" + hex.EncodeToString(bytes[:]), nil
}

func scanStack(row interface{ Scan(...any) error }) (Stack, error) {
	var s Stack
	var items string
	err := row.Scan(&s.ID, &s.Name, &s.Description, &items, &s.Builtin, &s.CreatedAt, &s.UpdatedAt)
	if err != nil {
		return s, err
	}
	err = json.Unmarshal([]byte(items), &s.Items)
	return s, err
}

const stackSelect = `SELECT id,name,description,items,builtin,created_at,updated_at FROM stacks`

func (c *Core) initializeStacks(ctx context.Context) error {
	tx, err := c.Store.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	var count int
	if err = tx.QueryRowContext(ctx, "SELECT COUNT(*) FROM stacks").Scan(&count); err != nil {
		return err
	}
	if count == 0 {
		now := time.Now().UnixMilli()
		presets := []Stack{
			{ID: "builtin-lnmp", Name: "LNMP 经典", Description: "Nginx + MySQL + PHP，最通用的 PHP 本地开发环境", Items: []StackItem{{ServiceID: "mysql", Order: 10}, {ServiceID: "php", Order: 20}, {ServiceID: "nginx", Order: 30}}},
			{ID: "builtin-web", Name: "前端 / 静态站点", Description: "只要一个 Web 服务器，托管静态产物或反代 dev server", Items: []StackItem{{ServiceID: "nginx", Order: 10}}},
			{ID: "builtin-data", Name: "数据栈", Description: "MySQL + Redis，跑后端服务或调试数据时常用", Items: []StackItem{{ServiceID: "mysql", Order: 10}, {ServiceID: "redis", Order: 20}}},
		}
		for _, s := range presets {
			s.Builtin = true
			s.CreatedAt = now
			s.UpdatedAt = now
			if err = insertStack(ctx, tx, s); err != nil {
				return err
			}
		}
	}
	return tx.Commit()
}

func insertStack(ctx context.Context, tx *sql.Tx, s Stack) error {
	items, err := json.Marshal(s.Items)
	if err != nil {
		return err
	}
	_, err = tx.ExecContext(ctx, `INSERT INTO stacks(id,name,description,items,builtin,created_at,updated_at) VALUES(?,?,?,?,?,?,?)`, s.ID, s.Name, s.Description, string(items), s.Builtin, s.CreatedAt, s.UpdatedAt)
	return err
}

func (c *Core) registerStacks() {
	c.handle("list_stacks", func(ctx context.Context, _ json.RawMessage) (any, error) {
		rows, err := c.Store.db.QueryContext(ctx, stackSelect+" ORDER BY builtin DESC,updated_at DESC")
		if err != nil {
			return nil, err
		}
		defer rows.Close()
		result := []Stack{}
		for rows.Next() {
			s, err := scanStack(rows)
			if err != nil {
				return nil, err
			}
			result = append(result, s)
		}
		return result, rows.Err()
	})
	c.handle("save_stack", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			Input StackInput `json:"input"`
		}](data)
		if err != nil {
			return nil, err
		}
		return c.saveStack(ctx, args.Input)
	})
	c.handle("duplicate_stack", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			ID   string  `json:"id"`
			Name *string `json:"name"`
		}](data)
		if err != nil {
			return nil, err
		}
		s, err := scanStack(c.Store.db.QueryRowContext(ctx, stackSelect+" WHERE id=?", args.ID))
		if err != nil {
			return nil, stackError(err)
		}
		name := s.Name + " 副本"
		if args.Name != nil {
			name = *args.Name
		}
		return c.saveStack(ctx, StackInput{Name: name, Description: s.Description, Items: s.Items})
	})
	c.handle("delete_stack", func(ctx context.Context, data json.RawMessage) (any, error) {
		args, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		tx, err := c.Store.db.BeginTx(ctx, nil)
		if err != nil {
			return nil, err
		}
		defer tx.Rollback()
		s, err := scanStack(tx.QueryRowContext(ctx, stackSelect+" WHERE id=?", args.ID))
		if err != nil {
			return nil, stackError(err)
		}
		if s.Builtin {
			return nil, &AppError{Code: "STACK_BUILTIN", Message: "内置预设不能删除，可以复制后编辑"}
		}
		if _, err = tx.ExecContext(ctx, "DELETE FROM stacks WHERE id=?", s.ID); err != nil {
			return nil, err
		}
		if _, err = tx.ExecContext(ctx, "UPDATE settings SET value='' WHERE key='startStackOnLaunch' AND value=?", s.ID); err != nil {
			return nil, err
		}
		return true, tx.Commit()
	})
	c.registerStackExecution()
}

func stackError(err error) error {
	if errors.Is(err, sql.ErrNoRows) {
		return &AppError{Code: "STACK_NOT_FOUND", Message: "服务栈已不存在，请刷新列表"}
	}
	return err
}

func (c *Core) saveStack(ctx context.Context, input StackInput) (Stack, error) {
	s := Stack{Name: strings.TrimSpace(input.Name), Description: input.Description, Items: []StackItem{}, UpdatedAt: time.Now().UnixMilli()}
	if s.Name == "" || len(s.Name) > 512 || len(s.Description) > 8192 || len(input.Items) == 0 || len(input.Items) > 200 {
		return s, invalid("服务栈名称或服务列表无效")
	}
	sort.SliceStable(input.Items, func(i, j int) bool { return input.Items[i].Order < input.Items[j].Order })
	seen := map[string]bool{}
	for _, item := range input.Items {
		if item.ServiceID == "" || len(item.ServiceID) > 200 || strings.ContainsAny(item.ServiceID, "\x00\r\n") {
			return s, invalid("服务标识无效")
		}
		if !seen[item.ServiceID] {
			seen[item.ServiceID] = true
			s.Items = append(s.Items, item)
		}
	}
	tx, err := c.Store.db.BeginTx(ctx, nil)
	if err != nil {
		return s, err
	}
	defer tx.Rollback()
	if input.ID == nil {
		s.ID, err = newID("stack")
		if err != nil {
			return s, err
		}
		s.CreatedAt = s.UpdatedAt
		err = insertStack(ctx, tx, s)
	} else {
		previous, readErr := scanStack(tx.QueryRowContext(ctx, stackSelect+" WHERE id=?", *input.ID))
		if readErr != nil {
			return s, stackError(readErr)
		}
		s.ID = previous.ID
		s.CreatedAt = previous.CreatedAt
		s.Builtin = previous.Builtin
		if s.UpdatedAt <= previous.UpdatedAt {
			s.UpdatedAt = previous.UpdatedAt + 1
		}
		items, encodeErr := json.Marshal(s.Items)
		if encodeErr != nil {
			return s, encodeErr
		}
		_, err = tx.ExecContext(ctx, "UPDATE stacks SET name=?,description=?,items=?,updated_at=? WHERE id=?", s.Name, s.Description, string(items), s.UpdatedAt, s.ID)
	}
	if err != nil {
		return s, err
	}
	return s, tx.Commit()
}
