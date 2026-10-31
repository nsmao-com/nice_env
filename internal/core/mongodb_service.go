package core

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"unicode"

	"gopkg.in/yaml.v3"
)

type mongoCredentials struct {
	Username     string `json:"username"`
	Password     string `json:"password"`
	AuthDatabase string `json:"authDatabase"`
}

func (v mongoCredentials) validate() error {
	if len(v.Username) > 256 || len(v.Password) > 4096 || (v.Username == "") != (v.Password == "") || strings.IndexFunc(v.Username+v.Password, unicode.IsControl) >= 0 || v.AuthDatabase == "" || len(v.AuthDatabase) > 63 || strings.ContainsAny(v.AuthDatabase, "/\\.\"$*<>:|?") || strings.IndexFunc(v.AuthDatabase, func(r rune) bool { return unicode.IsControl(r) || unicode.IsSpace(r) }) >= 0 {
		return invalid("MongoDB 用户名、密码或认证数据库无效")
	}
	return nil
}

func (c *Core) mongoCredentials(ctx context.Context, version string) (mongoCredentials, error) {
	v := mongoCredentials{AuthDatabase: "admin"}
	if !safePart(version) {
		return v, invalid("MongoDB 版本无效")
	}
	text, found, err := c.Store.setting(ctx, "mongodbCredentials@"+version)
	if err != nil {
		return v, err
	}
	if found && json.Unmarshal([]byte(text), &v) != nil {
		return v, &AppError{Code: "MONGO_CREDENTIALS_INVALID", Message: "MongoDB 本机认证记录损坏，原记录已保留"}
	}
	return v, v.validate()
}

func (c *Core) mongoAuthEnabled(ctx context.Context, version string) (bool, error) {
	text, found, err := c.Store.setting(ctx, "mongodbAuthEnabled@"+version)
	if err != nil {
		return false, err
	}
	if !found {
		return false, nil
	}
	if text != "true" && text != "false" {
		return false, &AppError{Code: "MONGO_AUTH_CONFIG", Message: "MongoDB 认证设置损坏，已阻止启动，避免意外关闭认证"}
	}
	return text == "true", nil
}

func (c *Core) prepareMongo(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	p := e.pkg
	if !reflect.DeepEqual(p.Run.Args, []string{"--dbpath", "{data}", "--port", "{port}", "--bind_ip", "127.0.0.1"}) || p.Run.Cwd != "" || len(p.Run.InitArgs) != 0 || len(p.Run.StopArgs) != 0 || p.Run.SingleInstance != nil && !*p.Run.SingleInstance {
		return invalid("MongoDB 运行描述必须使用受管的本机单实例参数")
	}
	auth, err := c.mongoAuthEnabled(ctx, p.Version)
	if err != nil {
		return err
	}
	// Authentication is version scoped in the existing data format. The native
	// storage directory must use the same version; never open shared/other-version
	// data with a missing authentication setting.
	r.data = filepath.Join(c.Home, "data", "mongodb", p.Version)
	if err := managedDirectory(c.Home, r.data); err != nil {
		return err
	}
	if err := managedDirectory(c.Home, r.etc); err != nil {
		return err
	}
	config := filepath.Join(r.etc, "mongod.conf")
	if _, err := os.Lstat(config); os.IsNotExist(err) {
		f, err := os.OpenFile(config, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
		if err != nil {
			return err
		}
		_, writeErr := f.WriteString("# NiceEnv MongoDB: storage, network and authorization are supplied at launch.\n")
		closeErr := f.Close()
		if writeErr != nil {
			return writeErr
		}
		if closeErr != nil {
			return closeErr
		}
	} else if err != nil {
		return err
	}
	data, err := os.ReadFile(config)
	if err != nil {
		return err
	}
	if len(data) > 1024*1024 {
		return invalid("MongoDB 配置过大")
	}
	var value map[string]any
	if yaml.Unmarshal(data, &value) != nil {
		return invalid("MongoDB 配置格式无效，原文件已保留")
	}
	if management, ok := value["processManagement"].(map[string]any); ok && management["fork"] == true {
		return invalid("受管 MongoDB 不能启用 processManagement.fork")
	}
	// Saved explicit auth preferences retain the old backend's semantics. If no
	// preference exists, never override an existing config's enabled auth.
	if _, found, err := c.Store.setting(ctx, "mongodbAuthEnabled@"+p.Version); err != nil {
		return err
	} else if !found {
		if security, ok := value["security"].(map[string]any); ok && security["authorization"] == "enabled" {
			auth = true
		}
		credentials, err := c.mongoCredentials(ctx, p.Version)
		if err != nil {
			return err
		}
		if credentials.Username != "" {
			auth = true
		}
		if _, err := os.Stat(filepath.Join(r.data, "WiredTiger")); err == nil && !auth {
			return &AppError{Code: "MONGO_AUTH_UNKNOWN", Message: "已有 MongoDB 数据缺少认证开关记录，未按无认证方式启动", Hint: "请恢复该版本的 mongodbAuthEnabled 本机记录后重试。"}
		}
	}
	// The supervisor owns out.log. Let mongod write stdout instead of opening
	// the same file a second time (Windows rejects that conflicting handle).
	r.args = []string{"--config", DisplayPath(config), "--dbpath", DisplayPath(r.data), "--port", strconv.Itoa(r.port), "--bind_ip", "127.0.0.1"}
	if auth && !e.mongoRecovery {
		r.args = append(r.args, "--auth")
	} else {
		r.args = append(r.args, "--noauth")
	}
	// Persist the first-start choice before the native server creates data, so a
	// subsequent restart can distinguish an initialized local instance from an
	// imported directory with missing authentication metadata.
	if err := c.Store.saveSetting(ctx, "mongodbAuthEnabled@"+p.Version, auth); err != nil {
		return err
	}
	r.ports = []int{r.port}
	return nil
}
