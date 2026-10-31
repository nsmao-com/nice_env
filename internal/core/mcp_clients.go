package core

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"unicode/utf8"

	"niceenv/internal/process"
)

type mcpClientSpec struct{ id, name, path, group string }
type mcpClientStatus struct {
	ID       string          `json:"id"`
	Name     string          `json:"name"`
	Path     Path            `json:"path"`
	Status   string          `json:"status"`
	Revision *string         `json:"revision"`
	Error    json.RawMessage `json:"error"`
}
type jsoncMember struct {
	key                         string
	keyStart, start, end, comma int
}

func jsoncInvalid() error {
	return &AppError{Code: "MCP_CLIENT_CONFIG_INVALID", Message: "客户端配置格式无效或存在重复字段，未修改文件"}
}
func jsoncSpace(data []byte, at int) int {
	for at < len(data) && bytes.ContainsRune([]byte(" \t\r\n"), rune(data[at])) {
		at++
	}
	return at
}
func jsoncStringEnd(data []byte, at int) int {
	at++
	for at < len(data) {
		if data[at] == '"' {
			return at + 1
		}
		if data[at] == '\\' {
			at++
		}
		at++
	}
	return len(data) + 1
}

// Comments and trailing commas become spaces, preserving byte offsets for
// surgical edits. The original text, including BOM and line endings, is kept.
func jsoncDocument(text string) ([]byte, error) {
	if !utf8.ValidString(text) {
		return nil, jsoncInvalid()
	}
	data := []byte(text)
	if bytes.HasPrefix(data, []byte{239, 187, 191}) {
		copy(data[:3], "   ")
	}
	for i := 0; i < len(data); {
		if data[i] == '"' {
			i = jsoncStringEnd(data, i)
			continue
		}
		if i+1 < len(data) && data[i] == '/' && data[i+1] == '/' {
			for i < len(data) && data[i] != '\r' && data[i] != '\n' {
				data[i] = ' '
				i++
			}
			continue
		}
		if i+1 < len(data) && data[i] == '/' && data[i+1] == '*' {
			data[i], data[i+1] = ' ', ' '
			i += 2
			closed := false
			for i+1 < len(data) {
				if data[i] == '*' && data[i+1] == '/' {
					data[i], data[i+1] = ' ', ' '
					i += 2
					closed = true
					break
				}
				if data[i] != '\r' && data[i] != '\n' {
					data[i] = ' '
				}
				i++
			}
			if !closed {
				return nil, jsoncInvalid()
			}
			continue
		}
		i++
	}
	for i := 0; i < len(data); i++ {
		if data[i] == '"' {
			i = jsoncStringEnd(data, i) - 1
			continue
		}
		if data[i] == ',' {
			next := jsoncSpace(data, i+1)
			if next < len(data) && (data[next] == '}' || data[next] == ']') {
				prev := i - 1
				for prev >= 0 && bytes.ContainsRune([]byte(" \t\r\n"), rune(data[prev])) {
					prev--
				}
				if prev < 0 || bytes.ContainsRune([]byte("{[, :"), rune(data[prev])) {
					return nil, jsoncInvalid()
				}
				data[i] = ' '
			}
		}
	}
	if !json.Valid(data) {
		return nil, jsoncInvalid()
	}
	decoder := json.NewDecoder(bytes.NewReader(data))
	decoder.UseNumber()
	var walk func() error
	walk = func() error {
		token, err := decoder.Token()
		if err != nil {
			return err
		}
		if delim, ok := token.(json.Delim); ok {
			switch delim {
			case '{':
				seen := map[string]bool{}
				for decoder.More() {
					key, err := decoder.Token()
					if err != nil {
						return err
					}
					name, ok := key.(string)
					if !ok || seen[name] {
						return jsoncInvalid()
					}
					seen[name] = true
					if err = walk(); err != nil {
						return err
					}
				}
			case '[':
				for decoder.More() {
					if err := walk(); err != nil {
						return err
					}
				}
			default:
				return jsoncInvalid()
			}
			_, err = decoder.Token()
			return err
		}
		return nil
	}
	if err := walk(); err != nil {
		return nil, jsoncInvalid()
	}
	at := jsoncSpace(data, 0)
	if at >= len(data) || data[at] != '{' {
		return nil, jsoncInvalid()
	}
	return data, nil
}
func jsoncMembers(data []byte, at int) ([]jsoncMember, error) {
	at = jsoncSpace(data, at)
	if at >= len(data) || data[at] != '{' {
		return nil, jsoncInvalid()
	}
	at++
	result := []jsoncMember{}
	for {
		at = jsoncSpace(data, at)
		if at >= len(data) {
			return nil, jsoncInvalid()
		}
		if data[at] == '}' {
			return result, nil
		}
		keyStart := at
		end := jsoncStringEnd(data, at)
		if data[at] != '"' || end > len(data) {
			return nil, jsoncInvalid()
		}
		var key string
		if json.Unmarshal(data[at:end], &key) != nil {
			return nil, jsoncInvalid()
		}
		at = jsoncSpace(data, end)
		if at >= len(data) || data[at] != ':' {
			return nil, jsoncInvalid()
		}
		start := jsoncSpace(data, at+1)
		decoder := json.NewDecoder(bytes.NewReader(data[start:]))
		var raw json.RawMessage
		if decoder.Decode(&raw) != nil {
			return nil, jsoncInvalid()
		}
		end = start + int(decoder.InputOffset())
		at = jsoncSpace(data, end)
		comma := -1
		if at < len(data) && data[at] == ',' {
			comma = at
			at++
		} else if at >= len(data) || data[at] != '}' {
			return nil, jsoncInvalid()
		}
		result = append(result, jsoncMember{key, keyStart, start, end, comma})
	}
}
func jsoncEntry(text, group string) (any, error) {
	data, err := jsoncDocument(text)
	if err != nil {
		return nil, err
	}
	members, err := jsoncMembers(data, 0)
	if err != nil {
		return nil, err
	}
	for _, parent := range members {
		if parent.key == group {
			children, err := jsoncMembers(data, parent.start)
			if err != nil {
				return nil, err
			}
			for _, child := range children {
				if child.key == "niceenv" {
					var value any
					err = json.Unmarshal(data[child.start:child.end], &value)
					return value, err
				}
			}
		}
	}
	return nil, nil
}
func jsoncIndent(text string, at int) string {
	line := text[:at]
	if i := strings.LastIndex(line, "\n"); i >= 0 {
		line = line[i+1:]
	}
	return line[:len(line)-len(strings.TrimLeft(line, " \t"))]
}
func jsoncChange(text, group string, value any) (string, error) {
	data, err := jsoncDocument(text)
	if err != nil {
		return "", err
	}
	root, err := jsoncMembers(data, 0)
	if err != nil {
		return "", err
	}
	open, siblings, key := jsoncSpace(data, 0), root, group
	found := false
	for _, parent := range root {
		if parent.key == group {
			open = parent.start
			siblings, err = jsoncMembers(data, open)
			if err != nil {
				return "", err
			}
			key = "niceenv"
			found = true
			break
		}
	}
	if !found {
		if value == nil {
			return text, nil
		}
		value = map[string]any{"niceenv": value}
	}
	newline := "\n"
	if strings.Contains(text, "\r\n") {
		newline = "\r\n"
	}
	format := func(value any, indent string) (string, error) {
		raw, err := json.MarshalIndent(value, "", "  ")
		return strings.ReplaceAll(string(raw), "\n", newline+indent), err
	}
	output := ""
	for i, member := range siblings {
		if member.key != key {
			continue
		}
		if value != nil {
			body, err := format(value, jsoncIndent(text, member.keyStart))
			if err != nil {
				return "", err
			}
			output = text[:member.start] + body + text[member.end:]
		} else {
			start, end := member.keyStart, member.end
			if member.comma >= 0 {
				end = member.comma + 1
			} else {
				// Remove a preserved trailing comma along with the final entry.
				for n := member.end; n < jsoncSpace(data, member.end); n++ {
					if text[n] == ',' {
						end = n + 1
					}
				}
				if i > 0 {
					start = siblings[i-1].comma
					if start < 0 {
						return "", jsoncInvalid()
					}
				}
			}
			output = text[:start] + text[end:]
		}
		break
	}
	if output == "" {
		if value == nil {
			return text, nil
		}
		indent := jsoncIndent(text, open) + "  "
		if len(siblings) > 0 {
			if old := jsoncIndent(text, siblings[0].keyStart); old != "" {
				indent = old
			}
		}
		at, prefix := open+1, ""
		if len(siblings) > 0 {
			at = siblings[len(siblings)-1].end
			prefix = ","
		}
		body, err := format(value, indent)
		if err != nil {
			return "", err
		}
		encodedKey, _ := json.Marshal(key)
		output = text[:at] + prefix + newline + indent + string(encodedKey) + ": " + body + newline + text[at:]
	}
	_, err = jsoncEntry(output, group)
	return output, err
}

func mcpClientSpecs() ([]mcpClientSpec, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, err
	}
	config, err := os.UserConfigDir()
	if err != nil {
		return nil, err
	}
	return []mcpClientSpec{{"cursor", "Cursor", filepath.Join(home, ".cursor", "mcp.json"), "mcpServers"}, {"claude-desktop", "Claude Desktop", filepath.Join(config, "Claude", "claude_desktop_config.json"), "mcpServers"}, {"vscode", "VS Code", filepath.Join(config, "Code", "User", "mcp.json"), "servers"}}, nil
}
func (c *Core) mcpExpected(spec mcpClientSpec) (any, error) {
	exe, err := integrationExecutable()
	if err != nil {
		return nil, err
	}
	entry := map[string]any{"command": DisplayPath(exe), "args": []string{"--mcp", "--niceenv-data-home", DisplayPath(c.Home)}}
	if spec.group == "servers" {
		entry["type"] = "stdio"
	}
	return entry, nil
}
func readMCPConfig(path string) ([]byte, error) {
	info, err := os.Lstat(path)
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Size() > 2*1024*1024 {
		return nil, jsoncInvalid()
	}
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	body, err := io.ReadAll(io.LimitReader(f, 2*1024*1024+1))
	if len(body) > 2*1024*1024 {
		return nil, jsoncInvalid()
	}
	return body, err
}
func mcpRevision(body []byte, desired any) string {
	digest := sha256.New()
	if body == nil {
		digest.Write([]byte("absent"))
	} else {
		digest.Write([]byte("present"))
	}
	digest.Write(body)
	raw, _ := json.Marshal(desired)
	digest.Write(raw)
	return hex.EncodeToString(digest.Sum(nil))
}
func mcpMatches(current, desired any) bool {
	a, _ := json.Marshal(current)
	b, _ := json.Marshal(desired)
	var av, bv any
	_ = json.Unmarshal(a, &av)
	_ = json.Unmarshal(b, &bv)
	return reflect.DeepEqual(av, bv)
}
func (c *Core) inspectMCPClient(spec mcpClientSpec) mcpClientStatus {
	result := mcpClientStatus{ID: spec.id, Name: spec.name, Path: Path(spec.path), Status: "error"}
	check := func() error {
		info, err := os.Stat(filepath.Dir(spec.path))
		if os.IsNotExist(err) {
			result.Status = "unavailable"
			return nil
		}
		if err != nil {
			return err
		}
		if !info.IsDir() {
			return jsoncInvalid()
		}
		desired, err := c.mcpExpected(spec)
		if err != nil {
			return err
		}
		body, err := readMCPConfig(spec.path)
		if err != nil {
			return err
		}
		var current any
		if body != nil {
			current, err = jsoncEntry(string(body), spec.group)
			if err != nil {
				return err
			}
		}
		result.Status = "different"
		if current == nil {
			result.Status = "missing"
		} else if mcpMatches(current, desired) {
			result.Status = "connected"
		}
		revision := mcpRevision(body, desired)
		result.Revision = &revision
		return nil
	}
	if err := check(); err != nil {
		result.Error = MarshalError(err)
	}
	return result
}
func (c *Core) getMCPClients(context.Context, json.RawMessage) (any, error) {
	specs, err := mcpClientSpecs()
	if err != nil {
		return nil, err
	}
	out := []mcpClientStatus{}
	for _, spec := range specs {
		out = append(out, c.inspectMCPClient(spec))
	}
	return out, nil
}
func (c *Core) configureMCPClient(_ context.Context, raw json.RawMessage) (any, error) {
	in, err := decode[struct {
		ID       string `json:"id"`
		Action   string `json:"action"`
		Revision string `json:"revision"`
		Replace  bool   `json:"replace"`
	}](raw)
	if err != nil {
		return nil, err
	}
	if in.Action != "connect" && in.Action != "remove" {
		return nil, invalid("接入操作无效")
	}
	specs, err := mcpClientSpecs()
	if err != nil {
		return nil, err
	}
	var spec mcpClientSpec
	for _, item := range specs {
		if item.id == in.ID {
			spec = item
		}
	}
	if spec.id == "" {
		return nil, invalid("请选择支持的 AI 客户端")
	}
	unlock, err := process.Lock(filepath.Join(filepath.Dir(spec.path), ".niceenv-mcp.lock"))
	if err != nil {
		return nil, err
	}
	defer unlock()
	desired, err := c.mcpExpected(spec)
	if err != nil {
		return nil, err
	}
	original, err := readMCPConfig(spec.path)
	if err != nil {
		return nil, err
	}
	if in.Revision != mcpRevision(original, desired) {
		return nil, &AppError{Code: "MCP_CLIENT_CHANGED", Message: "客户端配置已变化，请重新检查"}
	}
	text := "{}\n"
	if original != nil {
		text = string(original)
	}
	current, err := jsoncEntry(text, spec.group)
	if err != nil {
		return nil, err
	}
	ours := mcpMatches(current, desired)
	if current != nil && !ours && (in.Action == "remove" || !in.Replace) {
		return nil, &AppError{Code: "MCP_CLIENT_OTHER_INSTANCE", Message: "已有 NiceEnv 配置指向其他环境，请确认更换后接入"}
	}
	result := func(changed bool, backup any) any {
		return map[string]any{"client": c.inspectMCPClient(spec), "changed": changed, "backupPath": backup}
	}
	if (in.Action == "connect" && ours) || (in.Action == "remove" && current == nil) {
		return result(false, nil), nil
	}
	if in.Action == "remove" {
		desired = nil
	}
	output, err := jsoncChange(text, spec.group, desired)
	if err != nil {
		return nil, err
	}
	if len(output) > 2*1024*1024 {
		return nil, invalid("客户端配置超过 2 MiB")
	}
	writePrivate := func(pattern string, body []byte) (string, error) {
		f, err := os.CreateTemp(filepath.Dir(spec.path), pattern)
		if err != nil {
			return "", err
		}
		path := f.Name()
		if err = process.PrivateFile(path); err == nil {
			_, err = f.Write(body)
		}
		err = errors.Join(err, f.Sync(), f.Close())
		if err != nil {
			_ = os.Remove(path)
		}
		return path, err
	}
	var backup any
	if original != nil {
		path, err := writePrivate(".niceenv-mcp-*.bak", original)
		if err != nil {
			return nil, err
		}
		backup = Path(path)
	}
	pending, err := writePrivate(".niceenv-mcp-*.pending", []byte(output))
	if err != nil {
		return nil, err
	}
	defer os.Remove(pending)
	latest, err := readMCPConfig(spec.path)
	if err != nil {
		return nil, err
	}
	if !bytes.Equal(latest, original) || (latest == nil) != (original == nil) {
		return nil, &AppError{Code: "MCP_CLIENT_CHANGED", Message: "客户端配置在保存期间发生变化，已取消写入"}
	}
	if err = os.Rename(pending, spec.path); err != nil {
		return nil, err
	}
	return result(true, backup), nil
}
