package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

type configResetPreview struct {
	Kind          string  `json:"kind"`
	Label         string  `json:"label"`
	Path          Path    `json:"path"`
	Content       string  `json:"content"`
	Language      string  `json:"language"`
	CurrentExists bool    `json:"currentExists"`
	Changed       bool    `json:"changed"`
	Revision      string  `json:"revision"`
	UsedByService *string `json:"usedByService"`
}

func configResettable(pkg string) bool {
	return pkg == "nginx" || pkg == "apache" || pkg == "php" || pkg == "mysql" || pkg == "redis"
}

func defaultNginxConfig(r *resolvedService) (string, error) {
	if strings.ContainsAny(filepath.ToSlash(r.etc), "*?[") {
		return "", invalid("Nginx include 目录含 glob 特殊字符，请选择不含这些字符的数据目录")
	}
	return fmt.Sprintf("worker_processes 1;\nerror_log %s;\npid %s;\nevents { worker_connections 1024; }\nhttp {\n    include %s;\n    default_type application/octet-stream;\n    access_log off;\n    server { listen 127.0.0.1:%d; server_name localhost; location / { return 200 'NiceEnv'; } }\n    include %s;\n}\n", nginxQuote(filepath.Join(filepath.Dir(r.log), "error.log")), nginxQuote(filepath.Join(r.etc, "nginx.pid")), nginxQuote(filepath.Join(filepath.Dir(r.program), "conf", "mime.types")), r.port, nginxQuote(filepath.Join(r.etc, "sites", "*.conf"))), nil
}

func defaultMySQLConfig(r *resolvedService) (string, error) {
	base, err := phpIniQuote(filepath.Dir(filepath.Dir(r.program)))
	if err != nil {
		return "", err
	}
	data, err := phpIniQuote(r.data)
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("[mysqld]\nbasedir=%s\ndatadir=%s\nport=%d\nbind-address=127.0.0.1\nloose-mysqlx=0\ncharacter-set-server=utf8mb4\ncollation-server=utf8mb4_unicode_ci\nmax_connections=150\ninnodb_buffer_pool_size=128M\n[client]\nport=%d\ndefault-character-set=utf8mb4\n", base, data, r.port, r.port), nil
}

func defaultRedisConfig(r *resolvedService) (string, error) {
	dir, err := phpIniQuote(r.data)
	if err != nil {
		return "", err
	}
	return fmt.Sprintf("# NiceEnv Redis configuration\nbind 127.0.0.1\nprotected-mode yes\nport %d\ndaemonize no\ndatabases 16\nsave \"\"\nappendonly no\nmaxmemory 256mb\nmaxmemory-policy allkeys-lru\ndir %s\ndbfilename dump.rdb\nlogfile \"\"\n", r.port, dir), nil
}

func (c *Core) previewConfigReset(ctx context.Context, kind string) (configResetPreview, configTarget, *string, error) {
	var out configResetPreview
	target, err := c.configTarget(ctx, kind)
	if err != nil {
		return out, target, nil, err
	}
	if !configResettable(target.spec.pkg) {
		return out, target, nil, &AppError{Code: "CONFIG_RESET_UNSUPPORTED", Message: "该服务没有安全的默认重置模板，请编辑配置或从历史备份恢复"}
	}
	e, err := c.serviceEntry(target.spec.pkg, target.version)
	if err != nil {
		return out, target, nil, err
	}
	r, err := c.resolveService(ctx, e)
	if err != nil {
		return out, target, nil, err
	}
	var current *string
	before, err := readConfigText(target.path)
	if err == nil {
		current = &before
	} else if !os.IsNotExist(err) {
		return out, target, nil, err
	}
	var content string
	switch target.spec.pkg {
	case "nginx":
		r.etc = filepath.Dir(target.path)
		content, err = defaultNginxConfig(r)
		if err == nil {
			content, err = c.completeNginxPools(content)
		}
	case "apache":
		content, err = c.renderApacheConfig(ctx, e, r, true)
	case "php":
		content, err = defaultPHPIni(e, r)
	case "mysql":
		content, err = defaultMySQLConfig(r)
	case "redis":
		content, err = defaultRedisConfig(r)
	}
	if err != nil {
		return out, target, nil, err
	}
	var used *string
	c.services.mu.RLock()
	if e.child != nil {
		id := e.status.ID
		used = &id
	}
	revision := e.revision
	c.services.mu.RUnlock()
	encoded, err := json.Marshal([]any{target.key, target.path, current, content, revision})
	if err != nil {
		return out, target, nil, err
	}
	hash := sha256.Sum256(encoded)
	out = configResetPreview{Kind: target.key, Label: target.spec.label + " " + target.version, Path: Path(target.path), Content: content, Language: target.spec.language, CurrentExists: current != nil, Changed: current == nil || *current != content, Revision: hex.EncodeToString(hash[:]), UsedByService: used}
	return out, target, current, nil
}

func (c *Core) registerConfigReset() {
	c.handle("config_reset_preview", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Kind string `json:"kind"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		preview, _, _, err := c.previewConfigReset(ctx, in.Kind)
		return preview, err
	})
	c.handle("config_reset", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Kind     string `json:"kind"`
			Revision string `json:"revision"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		preview, target, current, err := c.previewConfigReset(ctx, in.Kind)
		if err != nil {
			return nil, err
		}
		if in.Revision == "" || in.Revision != preview.Revision {
			return nil, &AppError{Code: "CONFIG_CONFLICT", Message: "配置或服务设置已变化，请重新预览后重置"}
		}
		if err = c.writeConfigExpected(ctx, target, preview.Content, current, current == nil); err != nil {
			return nil, err
		}
		updated, _, _, err := c.previewConfigReset(ctx, target.key)
		return updated, err
	})
}
