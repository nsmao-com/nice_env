package core

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"niceenv/internal/process"
)

const mcpProtocol = "2024-11-05"

func mcpTools() []map[string]any {
	out := []map[string]any{}
	for _, tool := range assistantTools()[:8] {
		f := tool["function"].(map[string]any)
		description := f["description"].(string)
		description = strings.ReplaceAll(description, "请求用户确认后", "")
		out = append(out, map[string]any{"name": f["name"], "description": description, "inputSchema": f["parameters"]})
	}
	return out
}

func validateMCPTool(name string, raw json.RawMessage) error {
	var args map[string]json.RawMessage
	if json.Unmarshal(raw, &args) != nil || args == nil {
		return invalid("工具参数必须是对象")
	}
	allowed := map[string]bool{}
	switch name {
	case "list_services", "list_sites", "list_packages":
	case "start_service", "stop_service", "restart_service", "read_logs":
		allowed["id"] = true
		var id string
		if json.Unmarshal(args["id"], &id) != nil || id == "" || len(id) > 512 || strings.IndexFunc(id, func(r rune) bool { return unicode.IsSpace(r) || unicode.IsControl(r) }) >= 0 {
			return invalid("请提供不含空白的有效服务或日志 ID")
		}
		if name == "read_logs" {
			allowed["lines"] = true
			if v, ok := args["lines"]; ok {
				var n int
				if json.Unmarshal(v, &n) != nil || n < 1 || n > 1000 {
					return invalid("日志条数必须为 1–1000")
				}
			}
		}
	case "diagnose_port":
		allowed["port"] = true
		var n int
		if json.Unmarshal(args["port"], &n) != nil || n < 1 || n > 65535 {
			return invalid("端口必须为 1–65535")
		}
	default:
		return invalid("未知 MCP 工具")
	}
	for key := range args {
		if !allowed[key] {
			return invalid("工具参数含不支持的字段：" + key)
		}
	}
	return nil
}

func mcpToolCall(ctx context.Context, home, name string, args json.RawMessage) (any, error) {
	command := name
	if name == "list_services" {
		command = "list_service_status"
	}
	if name == "read_logs" {
		command = "tail_logs"
		var input map[string]any
		_ = json.Unmarshal(args, &input)
		if input["lines"] == nil {
			input["lines"] = 50
		}
		args, _ = json.Marshal(input)
	}
	if err := EnsureDesktopController(ctx, home); err != nil {
		return nil, err
	}
	raw, err := ControlInvoke(ctx, home, command, args)
	if err != nil {
		return nil, err
	}
	switch name {
	case "start_service", "stop_service", "restart_service":
		var input map[string]any
		_ = json.Unmarshal(args, &input)
		return map[string]any{"ok": true, "id": input["id"]}, nil
	case "list_services", "list_sites", "list_packages":
		var rows []map[string]any
		if err = json.Unmarshal(raw, &rows); err != nil {
			return nil, err
		}
		out := []map[string]any{}
		for _, row := range rows {
			item := map[string]any{}
			fields := []string{"id", "state", "port", "pids", "version"}
			if name == "list_sites" {
				fields = []string{"id", "name", "https", "status"}
				if domains, ok := row["domains"].([]any); ok && len(domains) > 0 {
					item["domain"] = domains[0]
				}
				item["url"] = row["accessUrl"]
			} else if name == "list_packages" {
				install, ok := row["install"].(map[string]any)
				if !ok {
					continue
				}
				item["name"], item["active"] = row["displayName"], row["active"]
				for _, field := range []string{"id", "version", "category"} {
					item[field] = row[field]
				}
				row = install
				fields = []string{"installPath", "installedAt"}
			}
			for _, field := range fields {
				item[field] = row[field]
			}
			out = append(out, item)
		}
		return map[string]any{strings.TrimPrefix(name, "list_"): out}, nil
	case "read_logs":
		var rows []map[string]any
		if err = json.Unmarshal(raw, &rows); err != nil {
			return nil, err
		}
		remaining, truncated, start := 128*1024, false, len(rows)
		for i := len(rows) - 1; i >= 0; i-- {
			line, _ := rows[i]["line"].(string)
			if remaining <= 0 {
				truncated = true
				break
			}
			if len(line) > remaining {
				at := len(line) - remaining
				for at < len(line) && !utf8.RuneStart(line[at]) {
					at++
				}
				rows[i]["line"] = line[at:]
				truncated = true
			}
			remaining -= len(line) + 1
			start = i
		}
		var input map[string]any
		_ = json.Unmarshal(args, &input)
		return map[string]any{"id": input["id"], "requestedLines": input["lines"], "returnedLines": len(rows) - start, "truncated": truncated, "byteLimit": 128 * 1024, "lines": rows[start:]}, nil
	case "diagnose_port":
		var row map[string]any
		if err = json.Unmarshal(raw, &row); err != nil {
			return nil, err
		}
		return map[string]any{"port": row["port"], "inUse": row["inUse"], "pid": row["pid"], "process": row["processName"]}, nil
	}
	return raw, nil
}

// RunMCP does not open a Store or start services. Only tool calls connect to the
// live desktop owner, so a disconnected client cannot create a second manager.
func RunMCP(ctx context.Context, home, version string, input io.Reader, output io.Writer) error {
	scanner := bufio.NewScanner(input)
	scanner.Buffer(make([]byte, 4096), 4*1024*1024)
	encoder := json.NewEncoder(output)
	for scanner.Scan() {
		var request map[string]json.RawMessage
		respond := func(id json.RawMessage, result any, code int, message string) error {
			if len(id) == 0 {
				id = json.RawMessage(`null`)
			}
			body := map[string]any{"jsonrpc": "2.0", "id": id}
			if code != 0 {
				body["error"] = map[string]any{"code": code, "message": message}
			} else {
				body["result"] = result
			}
			return encoder.Encode(body)
		}
		if json.Unmarshal(scanner.Bytes(), &request) != nil {
			if err := respond(nil, nil, -32700, "Parse error"); err != nil {
				return err
			}
			continue
		}
		var protocol, method string
		_ = json.Unmarshal(request["jsonrpc"], &protocol)
		_ = json.Unmarshal(request["method"], &method)
		id, hasID := request["id"]
		validID := false
		if hasID {
			var value any
			d := json.NewDecoder(bytes.NewReader(id))
			d.UseNumber()
			if d.Decode(&value) == nil {
				switch v := value.(type) {
				case string:
					validID = true
				case json.Number:
					_, err := v.Int64()
					validID = err == nil
				}
			}
		}
		if protocol != "2.0" || request == nil || (hasID && !validID) || method == "" {
			if validID && (request["result"] != nil || request["error"] != nil) {
				continue
			}
			if err := respond(nil, nil, -32600, "Invalid Request"); err != nil {
				return err
			}
			continue
		}
		if !hasID {
			continue
		}
		params := map[string]json.RawMessage{}
		if raw, ok := request["params"]; ok {
			if json.Unmarshal(raw, &params) != nil || params == nil {
				if err := respond(id, nil, -32602, "params 必须是对象"); err != nil {
					return err
				}
				continue
			}
		}
		var result any
		code, message := 0, ""
		switch method {
		case "initialize":
			var v string
			var caps, client map[string]json.RawMessage
			var cn, cv string
			_ = json.Unmarshal(params["protocolVersion"], &v)
			_ = json.Unmarshal(params["capabilities"], &caps)
			_ = json.Unmarshal(params["clientInfo"], &client)
			_ = json.Unmarshal(client["name"], &cn)
			_ = json.Unmarshal(client["version"], &cv)
			if v == "" || caps == nil || cn == "" || cv == "" {
				code, message = -32602, "initialize 需要 protocolVersion、capabilities 和 clientInfo"
			} else {
				result = map[string]any{"protocolVersion": mcpProtocol, "capabilities": map[string]any{"tools": map[string]any{}}, "serverInfo": map[string]string{"name": "niceservbay", "version": version}}
			}
		case "ping":
			result = map[string]any{}
		case "tools/list":
			if _, ok := params["cursor"]; ok {
				code, message = -32602, "工具列表不接受分页游标"
			} else {
				result = map[string]any{"tools": mcpTools()}
			}
		case "tools/call":
			var name string
			_ = json.Unmarshal(params["name"], &name)
			args := params["arguments"]
			if args == nil {
				args = json.RawMessage(`{}`)
			}
			if err := validateMCPTool(name, args); err != nil {
				code, message = -32602, err.Error()
				break
			}
			callCtx, cancel := context.WithTimeout(ctx, 5*time.Minute)
			value, err := mcpToolCall(callCtx, home, name, args)
			cancel()
			if err != nil {
				value = map[string]any{"ok": false, "error": json.RawMessage(MarshalError(err))}
			}
			text, marshalErr := json.Marshal(value)
			if marshalErr != nil {
				return marshalErr
			}
			result = map[string]any{"content": []map[string]string{{"type": "text", "text": string(text)}}, "isError": err != nil}
		default:
			code, message = -32601, "Method not found"
		}
		if err := respond(id, result, code, message); err != nil {
			return err
		}
	}
	return scanner.Err()
}

func integrationExecutable() (string, error) {
	path, err := os.Executable()
	if err != nil {
		return "", err
	}
	return filepath.Abs(path)
}

func (c *Core) registerMCP() {
	c.handle("get_integration_tools", func(context.Context, json.RawMessage) (any, error) {
		exe, err := integrationExecutable()
		if err != nil {
			return nil, err
		}
		cli := filepath.Join(filepath.Dir(exe), "niceenv-cli")
		if runtime.GOOS == "windows" {
			cli += ".exe"
		}
		if strings.HasPrefix(strings.ToLower(filepath.Base(exe)), "niceenv-cli") {
			cli = exe
		}
		tool := func(path string) any {
			info, e := os.Stat(path)
			return map[string]any{"path": Path(path), "available": e == nil && info.Mode().IsRegular(), "error": nil}
		}
		platform := runtime.GOOS
		if platform == "darwin" {
			platform = "macos"
		}
		return map[string]any{"platform": platform, "dataDir": Path(c.Home), "cli": tool(cli), "mcp": tool(exe)}, nil
	})
	c.handle("get_mcp_clients", c.getMCPClients)
	c.handle("configure_mcp_client", c.configureMCPClient)
	c.handle("check_mcp_connection", c.checkMCPConnection)
}

func (c *Core) checkMCPConnection(ctx context.Context, _ json.RawMessage) (any, error) {
	started := time.Now()
	ctx, cancel := context.WithTimeout(ctx, 12*time.Second)
	defer cancel()
	exe, err := integrationExecutable()
	if err != nil {
		return nil, err
	}
	readIn, writeIn, err := os.Pipe()
	if err != nil {
		return nil, err
	}
	defer readIn.Close()
	defer writeIn.Close()
	readOut, writeOut, err := os.Pipe()
	if err != nil {
		return nil, err
	}
	defer readOut.Close()
	defer writeOut.Close()
	child, err := process.Start(ctx, process.Spec{Program: exe, Args: []string{"--mcp", "--niceenv-data-home", c.Home}, Stdin: readIn, Stdout: writeOut, Stderr: process.NewLimitedBuffer(4096)})
	if err != nil {
		return nil, err
	}
	defer func() { _ = child.Stop(); child.Wait() }()
	stop := context.AfterFunc(ctx, func() { readOut.Close(); writeIn.Close(); _ = child.Stop() })
	defer stop()
	encoder := json.NewEncoder(writeIn)
	reader := bufio.NewReader(readOut)
	exchange := func(id int, method string, params any) (json.RawMessage, error) {
		if err := encoder.Encode(map[string]any{"jsonrpc": "2.0", "id": id, "method": method, "params": params}); err != nil {
			return nil, err
		}
		line, err := readControlLine(reader, 2*1024*1024)
		if err != nil {
			return nil, err
		}
		var response struct {
			ID     int             `json:"id"`
			Result json.RawMessage `json:"result"`
			Error  json.RawMessage `json:"error"`
		}
		if json.Unmarshal(line, &response) != nil || response.ID != id || len(response.Error) > 0 {
			return nil, invalid("MCP 返回了无效响应")
		}
		return response.Result, nil
	}
	raw, err := exchange(1, "initialize", map[string]any{"protocolVersion": mcpProtocol, "capabilities": map[string]any{}, "clientInfo": map[string]string{"name": "NiceEnv", "version": c.Version}})
	if err != nil {
		return nil, err
	}
	var init struct {
		Protocol string `json:"protocolVersion"`
		Server   struct {
			Name    string `json:"name"`
			Version string `json:"version"`
		} `json:"serverInfo"`
	}
	if json.Unmarshal(raw, &init) != nil || init.Protocol != mcpProtocol || init.Server.Name != "niceservbay" || init.Server.Version != c.Version {
		return nil, invalid("MCP 版本或协议不匹配")
	}
	if err = encoder.Encode(map[string]any{"jsonrpc": "2.0", "method": "notifications/initialized"}); err != nil {
		return nil, err
	}
	raw, err = exchange(2, "tools/list", map[string]any{})
	if err != nil {
		return nil, err
	}
	var list struct {
		Tools []map[string]any `json:"tools"`
	}
	if json.Unmarshal(raw, &list) != nil || len(list.Tools) != len(mcpTools()) {
		return nil, invalid("MCP 工具列表不完整")
	}
	raw, err = exchange(3, "tools/call", map[string]any{"name": "list_services", "arguments": map[string]any{}})
	if err != nil {
		return nil, err
	}
	var called struct {
		IsError bool `json:"isError"`
		Content []struct {
			Text string `json:"text"`
		} `json:"content"`
	}
	if json.Unmarshal(raw, &called) != nil || len(called.Content) != 1 {
		return nil, invalid("MCP 工具响应无效")
	}
	if called.IsError {
		return nil, &AppError{Code: "MCP_CONNECTION_FAILED", Message: "MCP 无法连接当前环境", Detail: called.Content[0].Text}
	}
	var value struct {
		Services []json.RawMessage `json:"services"`
	}
	if json.Unmarshal([]byte(called.Content[0].Text), &value) != nil || value.Services == nil {
		return nil, fmt.Errorf("MCP service list is invalid")
	}
	return map[string]any{"version": init.Server.Version, "protocol": init.Protocol, "toolCount": len(list.Tools), "serviceCount": len(value.Services), "elapsedMs": time.Since(started).Milliseconds()}, nil
}
