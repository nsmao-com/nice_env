package core

import (
	"context"
	"encoding/json"
	"path/filepath"
	"strings"
	"time"
)

func postgresLocalPasswordRules(content string) (string, error) {
	seen := map[string]bool{}
	lines := strings.SplitAfter(content, "\n")
	for i, line := range lines {
		text := strings.TrimSpace(line)
		if text == "" || strings.HasPrefix(text, "#") {
			continue
		}
		fields := strings.Fields(text)
		key := ""
		if len(fields) == 4 && fields[0] == "local" && (fields[1] == "all" || fields[1] == "replication") && fields[2] == "all" && fields[3] == "trust" {
			key = "local:" + fields[1]
		}
		if len(fields) == 5 && fields[0] == "host" && (fields[1] == "all" || fields[1] == "replication") && fields[2] == "all" && (fields[3] == "127.0.0.1/32" || fields[3] == "::1/128") && fields[4] == "trust" {
			key = "host:" + fields[1] + ":" + fields[3]
		}
		if key == "" || seen[key] {
			return "", &AppError{Code: "POSTGRES_AUTH_CUSTOM", Message: "pg_hba.conf 包含自定义或重复认证规则，未自动修改", Hint: "请检查原配置中的认证规则，再更新本机连接记录"}
		}
		seen[key] = true
		at := strings.LastIndex(line, "trust")
		lines[i] = line[:at] + "scram-sha-256" + line[at+5:]
	}
	if !seen["host:all:127.0.0.1/32"] || !seen["host:all:::1/128"] {
		return "", invalid("未找到完整的默认本机认证规则，未自动修改")
	}
	return strings.Join(lines, ""), nil
}
func (c *Core) postgresSetPassword(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[postgresRequest](data)
	if err != nil {
		return nil, err
	}
	if !safePart(in.Version) {
		return nil, invalid("PostgreSQL 版本无效")
	}
	if err = postgresPasswordValid(in.Password); err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()
	var override *string
	if in.UseExisting {
		override = &in.Password
	}
	e, conn, current, err := c.postgresSelected(ctx, in.Version, override, "postgres")
	if err != nil {
		return nil, err
	}
	defer closePostgres(conn)
	required, err := c.postgresRequiresPassword(ctx, e)
	if err != nil {
		return nil, err
	}
	if !required && !in.EnablePasswordAuth {
		return nil, &AppError{Code: "POSTGRES_AUTH_NOT_REQUIRED", Message: "当前实例不强制要求密码，请明确确认启用本机密码认证"}
	}
	var journal *siteFileJournal
	if !required {
		c.services.mu.RLock()
		expected := filepath.Join(e.resolved.data, "pg_hba.conf")
		c.services.mu.RUnlock()
		var actual string
		if err = conn.QueryRow(ctx, "SHOW hba_file").Scan(&actual); err != nil {
			return nil, postgresError(err, current)
		}
		actualPath, err := filepath.EvalSymlinks(DisplayPath(actual))
		if err != nil {
			return nil, err
		}
		expectedPath, err := filepath.EvalSymlinks(expected)
		if err != nil {
			return nil, err
		}
		if projectPathKey(actualPath) != projectPathKey(expectedPath) {
			return nil, invalid("实例使用自定义认证文件，未自动修改")
		}
		before, err := readConfigText(expected)
		if err != nil {
			return nil, err
		}
		after, err := postgresLocalPasswordRules(before)
		if err != nil {
			return nil, err
		}
		journal = c.newSiteJournal()
		if err = journal.add(expected, []byte(after), true); err != nil {
			return nil, err
		}
	}
	if in.UseExisting && !required {
		return nil, &AppError{Code: "POSTGRES_AUTH_UNVERIFIABLE", Message: "免密认证不能验证输入密码，请选择设置新密码并启用认证"}
	}
	if !in.UseExisting {
		if err = c.Store.saveSetting(ctx, postgresPasswordKey(in.Version)+".pending", in.Password); err != nil {
			return nil, err
		}
		if err = postgresSetRoleSecret(ctx, conn, "ALTER ROLE postgres", in.Password); err != nil {
			return nil, err
		}
	}
	if journal != nil {
		if err = journal.apply(ctx); err == nil {
			var reloaded bool
			err = conn.QueryRow(ctx, "SELECT pg_reload_conf()").Scan(&reloaded)
			if err == nil && !reloaded {
				err = invalid("PostgreSQL 未接受认证配置重载")
			}
		}
		if err != nil {
			restoreErr := journal.restore()
			detail := err.Error()
			if restoreErr != nil {
				detail += "；恢复认证配置失败：" + restoreErr.Error()
			}
			return nil, &AppError{Code: "POSTGRES_AUTH_APPLY_FAILED", Message: "密码已更新，但认证配置未确认生效，请重新读取实例状态", Detail: detail, Hint: "新的连接凭据已作为待确认记录保留"}
		}
		// Reload is asynchronous; wait briefly for new connections to enforce it.
		deadline := time.NewTimer(5 * time.Second)
		defer deadline.Stop()
		ticker := time.NewTicker(100 * time.Millisecond)
		defer ticker.Stop()
		for {
			required, err = c.postgresRequiresPassword(ctx, e)
			if err != nil {
				return nil, err
			}
			if required {
				break
			}
			select {
			case <-ctx.Done():
				return nil, ctx.Err()
			case <-deadline.C:
				return nil, invalid("认证重载后仍可免密连接，请检查 pg_hba.conf；新凭据已保留")
			case <-ticker.C:
			}
		}
	}
	verify, err := c.postgresConnection(ctx, e, in.Password, "postgres")
	if err != nil {
		return nil, postgresError(err, in.Password)
	}
	closePostgres(verify)
	if err = c.Store.saveSetting(ctx, postgresPasswordKey(in.Version), in.Password); err != nil {
		return nil, err
	}
	if err = c.Store.saveSetting(ctx, postgresPasswordKey(in.Version)+".pending", ""); err != nil {
		return nil, err
	}
	return nil, nil
}
