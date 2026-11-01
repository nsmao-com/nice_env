package core

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"strings"
	"time"
	"unicode/utf8"
)

type mongoAuthView struct {
	Version       string    `json:"version"`
	Username      string    `json:"username"`
	AuthDatabase  string    `json:"authDatabase"`
	HasPassword   bool      `json:"hasPassword"`
	Configured    bool      `json:"configured"`
	Running       bool      `json:"running"`
	Authorization *bool     `json:"authorization"`
	HasUsers      *bool     `json:"hasUsers"`
	Administrator bool      `json:"administrator"`
	Problem       *AppError `json:"problem"`
	Revision      string    `json:"revision"`
}

type mongoAuthRequest struct {
	Version     string           `json:"version"`
	Revision    string           `json:"revision"`
	Password    string           `json:"password"`
	Credentials mongoCredentials `json:"credentials"`
	Input       struct {
		Revision           string            `json:"revision"`
		Enabled            bool              `json:"enabled"`
		AcknowledgeRestart bool              `json:"acknowledgeRestart"`
		AcknowledgeDisable bool              `json:"acknowledgeDisable"`
		Administrator      *mongoCredentials `json:"administrator"`
	} `json:"input"`
}

type mongoProbe struct {
	Authorization bool  `json:"authorization"`
	HasUsers      *bool `json:"hasUsers"`
	Administrator bool  `json:"administrator"`
}

func (c *Core) probeMongo(ctx context.Context, version string, credentials mongoCredentials) (mongoProbe, error) {
	var probe mongoProbe
	data, err := c.mongoExecute(ctx, version, struct{}{}, mongoProbeScript, credentials)
	if err != nil {
		return probe, err
	}
	err = json.Unmarshal(data, &probe)
	return probe, err
}

// The caller holds lifecycle throughout status validation and any mutation.
func (c *Core) mongoAuthStatus(ctx context.Context, version string) (mongoAuthView, error) {
	v := mongoAuthView{Version: version, AuthDatabase: "admin"}
	if !safePart(version) {
		return v, invalid("MongoDB 版本无效")
	}
	e, err := c.serviceEntry("mongodb", version)
	if err != nil {
		return v, err
	}
	v.Configured, err = c.mongoAuthEnabled(ctx, version)
	if err != nil {
		return v, err
	}
	credentials, err := c.mongoCredentials(ctx, version)
	if err != nil {
		v.Problem = publicError(err)
	} else {
		v.Username = credentials.Username
		v.AuthDatabase = credentials.AuthDatabase
		v.HasPassword = credentials.Password != ""
	}
	c.services.mu.RLock()
	v.Running = e.child != nil && e.resolved != nil
	serviceRevision := e.revision
	c.services.mu.RUnlock()
	if v.Running && v.Problem == nil {
		probe, err := c.probeMongo(ctx, version, credentials)
		if err != nil {
			v.Problem = publicError(err)
		} else {
			v.Authorization = &probe.Authorization
			v.HasUsers = probe.HasUsers
			v.Administrator = probe.Administrator
		}
	}
	raw, _, err := c.Store.setting(ctx, "mongodbCredentials@"+version)
	if err != nil {
		return v, err
	}
	data, err := json.Marshal([]any{v, serviceRevision, raw})
	if err != nil {
		return v, err
	}
	mac := hmac.New(sha256.New, c.snapshotKey[:])
	mac.Write(data)
	v.Revision = hex.EncodeToString(mac.Sum(nil))
	return v, nil
}

func (c *Core) currentMongoAuth(ctx context.Context, version, revision string) (mongoAuthView, error) {
	v, err := c.mongoAuthStatus(ctx, version)
	if err != nil {
		return v, err
	}
	if revision == "" || !hmac.Equal([]byte(revision), []byte(v.Revision)) {
		return v, &AppError{Code: "MONGO_AUTH_CHANGED", Message: "实例、账号或认证设置已变化，请刷新后重新确认"}
	}
	if !v.Running {
		return v, &AppError{Code: "MONGO_NOT_RUNNING", Message: "请先启动所选 MongoDB"}
	}
	return v, nil
}

func (c *Core) restartMongo(ctx context.Context, version string) error {
	e, err := c.serviceEntry("mongodb", version)
	if err != nil {
		return err
	}
	if err = c.stopOwned(ctx, e, false); err != nil {
		return err
	}
	return c.startService(ctx, e)
}

func (c *Core) registerMongoAuth() {
	c.handle("mongodb_auth_status", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[mongoAuthRequest](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryRLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.RUnlock()
		return c.mongoAuthStatus(ctx, in.Version)
	})
	c.handle("mongodb_auth_connection", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[mongoAuthRequest](data)
		if err != nil {
			return nil, err
		}
		if err = in.Credentials.validate(); err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		before, err := c.currentMongoAuth(ctx, in.Version, in.Revision)
		if err != nil {
			return nil, err
		}
		if in.Credentials.Username != "" && !before.Configured && (before.Authorization == nil || !*before.Authorization) {
			return nil, &AppError{Code: "MONGO_AUTH_DISABLED", Message: "请先初始化管理员并开启认证"}
		}
		if _, err = c.probeMongo(ctx, in.Version, in.Credentials); err != nil {
			return nil, err
		}
		if err = c.Store.saveSetting(ctx, "mongodbCredentials@"+in.Version, in.Credentials); err != nil {
			return nil, err
		}
		return c.mongoAuthStatus(ctx, in.Version)
	})
	c.handle("mongodb_auth_apply", c.mongoAuthApply)
	c.handle("mongodb_auth_password", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoAuthPassword(ctx, data, false)
	})
	c.handle("mongodb_auth_reset", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.mongoAuthPassword(ctx, data, true)
	})
}

func (c *Core) mongoAuthApply(ctx context.Context, data json.RawMessage) (any, error) {
	in, err := decode[mongoAuthRequest](data)
	if err != nil {
		return nil, err
	}
	if !in.Input.AcknowledgeRestart || !in.Input.Enabled && !in.Input.AcknowledgeDisable {
		return nil, &AppError{Code: "MONGO_AUTH_CONFIRM", Message: "请确认重启中断连接；关闭认证还需确认本机客户端可直接访问数据"}
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	before, err := c.currentMongoAuth(ctx, in.Version, in.Input.Revision)
	if err != nil {
		return nil, err
	}
	if before.Problem != nil {
		return nil, before.Problem
	}
	credentials, err := c.mongoCredentials(ctx, in.Version)
	if err != nil {
		return nil, err
	}
	if candidate := in.Input.Administrator; candidate != nil {
		if err = candidate.validate(); err != nil {
			return nil, err
		}
		if !in.Input.Enabled || before.Authorization == nil || *before.Authorization || before.HasUsers == nil || *before.HasUsers || strings.TrimSpace(candidate.Username) == "" || utf8.RuneCountInString(candidate.Password) < 8 || candidate.AuthDatabase != "admin" {
			return nil, &AppError{Code: "MONGO_ADMIN_EXISTS", Message: "仅无账号且未启用认证的实例可初始化管理员，密码至少需要 8 个字符"}
		}
		if err = c.Store.saveSetting(ctx, "mongodbCredentials@"+in.Version, *candidate); err != nil {
			return nil, err
		}
		_, created := c.mongoExecute(ctx, in.Version, map[string]string{"username": candidate.Username, "password": candidate.Password}, mongoCreateAdminScript, credentials)
		probe, probeErr := c.probeMongo(ctx, in.Version, *candidate)
		if probeErr != nil || !probe.Administrator {
			var app *AppError
			if errors.As(created, &app) && app.Code == "MONGO_ADMIN_EXISTS" {
				return nil, errors.Join(created, c.Store.saveSetting(context.WithoutCancel(ctx), "mongodbCredentials@"+in.Version, credentials))
			}
			return nil, &AppError{Code: "MONGO_ADMIN_UNVERIFIED", Message: "管理员创建结果尚未验证，本机保留新连接记录", Hint: "未启用认证，请检查账号后重新验证连接。"}
		}
		credentials = *candidate
	}
	probe, err := c.probeMongo(ctx, in.Version, credentials)
	if err != nil {
		return nil, err
	}
	if credentials.Username == "" || !probe.Administrator {
		return nil, &AppError{Code: "MONGO_ADMIN_REQUIRED", Message: "请先验证具有 admin 数据库 root 角色的管理账号"}
	}
	if err = c.Store.saveSetting(ctx, "mongodbAuthEnabled@"+in.Version, in.Input.Enabled); err != nil {
		return nil, err
	}
	if err = c.restartMongo(ctx, in.Version); err != nil {
		return nil, &AppError{Code: "MONGO_AUTH_RESTART", Message: "认证配置和凭据已保存，但重启未完成", Hint: "排除启动问题后重新启动；不会自动改为无认证。", Detail: publicError(err).Message}
	}
	after, err := c.mongoAuthStatus(ctx, in.Version)
	if err != nil {
		return nil, err
	}
	if after.Problem != nil || after.Authorization == nil || *after.Authorization != in.Input.Enabled {
		return nil, &AppError{Code: "MONGO_AUTH_UNVERIFIED", Message: "重启后的认证状态尚未验证，配置和凭据已保留"}
	}
	return after, nil
}

func (c *Core) mongoAuthPassword(ctx context.Context, data json.RawMessage, reset bool) (any, error) {
	in, err := decode[mongoAuthRequest](data)
	if err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	before, err := c.currentMongoAuth(ctx, in.Version, in.Revision)
	if err != nil {
		return nil, err
	}
	if reset {
		if !before.Configured || strings.TrimSpace(before.Username) == "" {
			return nil, &AppError{Code: "MONGO_RESET_UNAVAILABLE", Message: "没有可恢复的本机管理账号记录"}
		}
	} else if !before.Administrator {
		return nil, &AppError{Code: "MONGO_ADMIN_REQUIRED", Message: "请先验证管理账号连接"}
	}
	previous, err := c.mongoCredentials(ctx, in.Version)
	if err != nil {
		return nil, err
	}
	candidate := previous
	candidate.Password = in.Password
	if err = candidate.validate(); err != nil {
		return nil, err
	}
	if utf8.RuneCountInString(candidate.Password) < 8 {
		return nil, &AppError{Code: "MONGO_PASSWORD_SHORT", Message: "新密码至少需要 8 个字符"}
	}
	key := "mongodbCredentials@" + in.Version
	if err = c.Store.saveSetting(ctx, key, candidate); err != nil {
		return nil, err
	}
	var changed error
	if reset {
		changed = c.resetMongoPassword(ctx, in.Version, candidate)
	} else {
		_, changed = c.mongoExecute(ctx, in.Version, map[string]string{"password": candidate.Password}, mongoChangePasswordScript, previous)
	}
	// Verify even if the caller cancelled after updateUser: never discard the only
	// working password merely because its success response was interrupted.
	verifyCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), 45*time.Second)
	defer cancel()
	probe, verifyErr := c.probeMongo(verifyCtx, in.Version, candidate)
	if verifyErr == nil && probe.Administrator && (!reset || probe.Authorization) {
		return c.mongoAuthStatus(verifyCtx, in.Version)
	}
	if _, oldErr := c.probeMongo(verifyCtx, in.Version, previous); oldErr == nil {
		if saveErr := c.Store.saveSetting(verifyCtx, key, previous); saveErr != nil {
			return nil, errors.Join(changed, saveErr)
		}
	}
	return nil, &AppError{Code: "MONGO_PASSWORD_UNVERIFIED", Message: "密码修改结果尚未确认，请检查服务状态并重新验证连接", Hint: "本机保留可验证的凭据；无法验证时保留新凭据。"}
}

func (c *Core) resetMongoPassword(ctx context.Context, version string, candidate mongoCredentials) (result error) {
	e, err := c.serviceEntry("mongodb", version)
	if err != nil {
		return err
	}
	// The on-disk auth setting stays enabled, including if the app crashes.
	// An independent cleanup context always restores the protected instance.
	defer func() {
		e.mongoRecovery = false
		cleanup, cancel := context.WithTimeout(context.WithoutCancel(ctx), 60*time.Second)
		defer cancel()
		if err := c.restartMongo(cleanup, version); err != nil {
			stopErr := c.stopOwned(cleanup, e, true)
			result = errors.Join(result, err, stopErr)
		}
	}()
	e.mongoRecovery = true
	if err = c.restartMongo(ctx, version); err != nil {
		return err
	}
	_, err = c.mongoExecute(ctx, version, map[string]string{"username": candidate.Username, "password": candidate.Password, "authDatabase": candidate.AuthDatabase}, mongoResetPasswordScript, mongoCredentials{AuthDatabase: "admin"})
	return err
}
