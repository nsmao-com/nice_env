package core

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"
	"unicode"

	"niceenv/internal/process"
)

type mongoRequest struct {
	Action     string `json:"action"`
	Database   string `json:"database"`
	Collection string `json:"collection"`
	Search     string `json:"search"`
	Offset     int    `json:"offset"`
	Limit      int    `json:"limit"`
	ID         string `json:"id"`
	Confirmed  bool   `json:"confirmed"`
	Filter     *struct {
		Field     string `json:"field"`
		Value     string `json:"value"`
		ValueType string `json:"valueType"`
	} `json:"filter"`
}

func mongoName(value string, limit int) bool {
	return value != "" && len(value) <= limit && strings.IndexFunc(value, unicode.IsControl) < 0
}
func (r mongoRequest) validate() error {
	bad := invalid("MongoDB 数据库、集合、分页或筛选条件无效")
	switch r.Action {
	case "overview":
		return nil
	case "collections":
		if !mongoName(r.Database, 64) || len(r.Search) > 200 || strings.IndexFunc(r.Search, unicode.IsControl) >= 0 {
			return bad
		}
	case "documents":
		if !mongoName(r.Database, 64) || !mongoName(r.Collection, 255) || r.Offset < 0 || r.Offset > 10000 || r.Limit < 1 || r.Limit > 25 {
			return bad
		}
		if f := r.Filter; f != nil {
			if !mongoName(f.Field, 255) || strings.Contains(f.Field, "$") || len(f.Value) > 4096 || strings.ContainsRune(f.Value, 0) {
				return bad
			}
			for _, part := range strings.Split(f.Field, ".") {
				if part == "" {
					return bad
				}
			}
			switch f.ValueType {
			case "text":
			case "number":
				n, err := strconv.ParseFloat(f.Value, 64)
				if err != nil || math.IsNaN(n) || math.IsInf(n, 0) || math.Trunc(n) == n && math.Abs(n) > 9007199254740991 {
					return bad
				}
			case "boolean":
				if f.Value != "true" && f.Value != "false" {
					return bad
				}
			case "null":
				if f.Value != "" {
					return bad
				}
			case "objectId":
				b, err := hex.DecodeString(f.Value)
				if err != nil || len(b) != 12 {
					return bad
				}
			default:
				return bad
			}
		}
	case "deleteDocument":
		if !mongoName(r.Database, 64) || !mongoName(r.Collection, 255) || len(r.ID) > 4096 || !r.Confirmed || !json.Valid([]byte(r.ID)) || strings.IndexFunc(r.ID, unicode.IsControl) >= 0 {
			return bad
		}
	default:
		return bad
	}
	return nil
}

// Caller owns services.lifecycle for the complete command, including auth and
// mutations. Every shell invocation verifies both PID ownership and dbPath.
func (c *Core) selectedMongo(version string) (*serviceEntry, error) {
	if !safePart(version) {
		return nil, invalid("MongoDB 版本无效")
	}
	e, err := c.serviceEntry("mongodb", version)
	if err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	running := e.child != nil && e.resolved != nil
	c.services.mu.RUnlock()
	if !running {
		return nil, &AppError{Code: "MONGO_NOT_RUNNING", Message: "所选 MongoDB 未运行或运行版本已变化"}
	}
	return e, nil
}

func (c *Core) mongoTool(ctx context.Context, id string, versions ...string) (Package, string, error) {
	return c.installedTool(ctx, id, versions...)
}

func (c *Core) installedTool(ctx context.Context, id string, versions ...string) (Package, string, error) {
	views, err := c.packageViews(ctx)
	if err != nil {
		return Package{}, "", err
	}
	for _, v := range views {
		selected := v["active"] == true
		if len(versions) > 0 {
			selected = v["version"] == versions[0]
		}
		if v["id"] != id || !selected || v["install"] == nil {
			continue
		}
		data, err := json.Marshal(v)
		if err != nil {
			return Package{}, "", err
		}
		var p Package
		if err = json.Unmarshal(data, &p); err != nil {
			return p, "", err
		}
		program, err := relativePath(p.Install.InstallPath, p.Entry)
		if err != nil {
			return p, "", err
		}
		actual, err := filepath.EvalSymlinks(program)
		if err != nil {
			return p, "", err
		}
		root, err := filepath.EvalSymlinks(p.Install.InstallPath)
		if err != nil {
			return p, "", err
		}
		if !pathContains(root, actual) {
			return p, "", invalid("工具入口超出安装目录")
		}
		info, err := os.Stat(program)
		if err != nil {
			return p, "", err
		}
		if !info.Mode().IsRegular() {
			return p, "", invalid("工具入口不是普通文件")
		}
		return p, program, nil
	}
	return Package{}, "", &AppError{Code: "TOOL_NOT_INSTALLED", Message: "请先安装并选择工具版本：" + id}
}

func (c *Core) mongoExecute(ctx context.Context, version string, request any, body string, credentials mongoCredentials) (json.RawMessage, error) {
	if err := credentials.validate(); err != nil {
		return nil, err
	}
	e, err := c.selectedMongo(version)
	if err != nil {
		return nil, err
	}
	if err := c.verifyDatabaseListener(ctx, e); err != nil {
		return nil, err
	}
	shell, program, err := c.mongoTool(ctx, "mongosh")
	if err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	r := *e.resolved
	c.services.mu.RUnlock()
	private, err := os.MkdirTemp(c.Home, ".mongo-command-")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(private)
	script := filepath.Join(private, "command.js")
	text := "const input = JSON.parse(require('fs').readFileSync(0,'utf8'));\n" + mongoConnectScript + "\n" + body + "\n" + mongoCatchScript
	if err := os.WriteFile(script, []byte(text), 0600); err != nil {
		return nil, err
	}
	// Anonymous pipe keeps passwords out of disk, script files and process args.
	input, writer, err := os.Pipe()
	if err != nil {
		return nil, err
	}
	defer input.Close()
	defer writer.Close()
	data, err := json.Marshal(map[string]any{"uri": fmt.Sprintf("mongodb://127.0.0.1:%d", r.port), "port": r.port, "version": version, "shellVersion": shell.Version, "dataDir": DisplayPath(r.data), "request": request, "credentials": credentials})
	if err != nil {
		return nil, err
	}
	output := process.NewLimitedBuffer(8 * 1024 * 1024)
	diagnostic := process.NewLimitedBuffer(16384)
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	child, err := process.Start(ctx, process.Spec{Program: program, Directory: private, Args: []string{"--quiet", "--norc", "--nodb", "--file", DisplayPath(script)}, Stdin: input, Stdout: output, Stderr: diagnostic, Environment: map[string]string{"HOME": private, "USERPROFILE": private, "APPDATA": private, "LOCALAPPDATA": private, "MONGOSH_LOG_DIR": private}, RemoveEnvironment: []string{"NODE_OPTIONS", "NODE_PATH"}})
	if err != nil {
		return nil, err
	}
	written := make(chan error, 1)
	go func() {
		_, err := writer.Write(data)
		closeErr := writer.Close()
		if err == nil {
			err = closeErr
		}
		written <- err
	}()
	result := child.Wait()
	input.Close()
	writer.Close()
	writeErr := <-written
	if ctx.Err() != nil {
		return nil, ctx.Err()
	}
	if result.Err != nil || result.Code != 0 || writeErr != nil {
		return nil, &AppError{Code: "MONGO_SHELL_FAILED", Message: "MongoDB Shell 执行失败，请检查工具与实例状态"}
	}
	var response struct {
		Result json.RawMessage `json:"result"`
		Error  *struct {
			Code    string `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}
	if json.Unmarshal([]byte(output.String()), &response) != nil {
		return nil, &AppError{Code: "MONGO_RESULT_INVALID", Message: "MongoDB Shell 未返回有效结果"}
	}
	if response.Error != nil {
		code, message := "MONGO_QUERY_FAILED", "MongoDB 操作未完成，请检查实例状态"
		switch response.Error.Code {
		case "13", "18":
			code, message = "MONGO_ACCESS_DENIED", "MongoDB 拒绝访问，请验证管理账号"
		case "50":
			code, message = "MONGO_QUERY_TIMEOUT", "MongoDB 查询超时，请缩小筛选范围"
		case "MONGO_INSTANCE_CHANGED":
			code, message = "MONGO_INSTANCE_CHANGED", "MongoDB 数据目录与所选实例不一致"
		case "MONGO_ADMIN_EXISTS":
			code, message = "MONGO_ADMIN_EXISTS", "实例已有账号，未创建新管理员"
		case "MONGO_COLLECTION_MISSING", "MONGO_COLLECTION_READONLY", "MONGO_DOCUMENT_INVALID", "MONGO_DOCUMENT_MISSING", "MONGO_DELETE_UNACKNOWLEDGED":
			code = response.Error.Code
		}
		return nil, &AppError{Code: code, Message: message}
	}
	if len(response.Result) == 0 {
		return nil, &AppError{Code: "MONGO_RESULT_INVALID", Message: "MongoDB Shell 缺少操作结果"}
	}
	return response.Result, nil
}

func (c *Core) registerMongo() {
	c.handle("mongodb_browse", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Version string       `json:"version"`
			Request mongoRequest `json:"request"`
		}](data)
		if err != nil {
			return nil, err
		}
		if err = in.Request.validate(); err != nil {
			return nil, err
		}
		switch in.Request.Action {
		case "overview", "collections", "documents":
			if !c.services.lifecycle.TryRLock() {
				return nil, serviceBusy()
			}
			defer c.services.lifecycle.RUnlock()
		default:
			if !c.services.lifecycle.TryLock() {
				return nil, serviceBusy()
			}
			defer c.services.lifecycle.Unlock()
		}
		credentials, err := c.mongoCredentials(ctx, in.Version)
		if err != nil {
			return nil, err
		}
		return c.mongoExecute(ctx, in.Version, in.Request, mongoBrowseScript, credentials)
	})
	c.registerMongoAuth()
	c.registerMongoBackups()
}
