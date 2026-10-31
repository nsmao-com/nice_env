package core

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	gnet "github.com/shirou/gopsutil/v4/net"
	gprocess "github.com/shirou/gopsutil/v4/process"
)

var sensitiveDiagnosticLine = regexp.MustCompile(`(?i)(password|passwd|pwd|secret|token|api[_-]?key|authorization|cookie|requirepass|masterauth|\b(?:uuid|sessionid|auth)\b|(?:bearer|basic)\s|://[^\s/]+@|\beyJ[A-Za-z0-9_-]+\.)`)

func redactDiagnostic(text string) string {
	lines := strings.Split(text, "\n")
	pem := false
	var quote rune
	blockIndent := -1
	for i, line := range lines {
		trimmed := strings.TrimSpace(line)
		indent := len(line) - len(strings.TrimLeft(line, " \t"))
		if strings.Contains(line, "-----BEGIN ") {
			pem = true
		}
		continued := quote != 0 || blockIndent >= 0 && (trimmed == "" || indent > blockIndent)
		if pem || continued {
			lines[i] = "[敏感内容已隐藏]"
			if quote != 0 && strings.ContainsRune(line, quote) {
				quote = 0
			}
		} else {
			blockIndent = -1
			if sensitiveDiagnosticLine.MatchString(line) {
				lines[i] = "[敏感内容已隐藏]"
				index := strings.IndexAny(line, "=:")
				if index >= 0 {
					value := strings.TrimSpace(line[index+1:])
					if value == "" || strings.HasPrefix(value, "|") || strings.HasPrefix(value, ">") {
						blockIndent = indent
					} else if (value[0] == '\'' || value[0] == '"') && strings.Count(value, value[:1]) == 1 {
						quote = rune(value[0])
					}
				}
			}
		}
		if strings.Contains(line, "-----END ") {
			pem = false
		}
	}
	return strings.Join(lines, "\n")
}

type serviceCheck struct {
	ID     string   `json:"id"`
	State  string   `json:"state"`
	Method string   `json:"method"`
	Detail string   `json:"detail"`
	Lines  []string `json:"lines"`
}
type configCheck struct {
	Kind      string `json:"kind"`
	Name      string `json:"name"`
	Path      Path   `json:"path"`
	Method    string `json:"method"`
	OK        bool   `json:"ok"`
	Status    string `json:"status"`
	Detail    string `json:"detail"`
	CheckedAt int64  `json:"checkedAt"`
}

func (c *Core) checkConfig(ctx context.Context, t configTarget) configCheck {
	r := configCheck{Kind: t.key, Name: t.spec.label + " " + t.version, Path: Path(t.path), Method: "readability", Status: "warning", CheckedAt: time.Now().Unix()}
	content, err := readConfigText(t.path)
	if os.IsNotExist(err) {
		r.Status = "skipped"
		r.Detail = "配置尚未生成，请先初始化对应服务"
		return r
	}
	if err != nil {
		r.Status = "fail"
		r.Detail = err.Error()
		return r
	}
	validation, err := c.validateConfig(ctx, t, content)
	if err != nil {
		r.Status = "fail"
		r.Detail = err.Error()
		return r
	}
	r.Detail = redactDiagnostic(strings.Join(validation.Messages, "\n"))
	if t.spec.pkg == "nginx" || t.spec.pkg == "apache" || t.spec.pkg == "caddy" {
		r.Method = "native"
		r.OK = validation.OK
		if r.OK {
			r.Status = "ok"
		} else {
			r.Status = "fail"
		}
	} else {
		r.Detail = "已读取配置；" + r.Detail
		if !validation.OK && len(validation.Issues) > 0 && validation.Issues[0].Severity == "error" {
			r.Status = "fail"
		}
	}
	return r
}
func (c *Core) diagnoseService(ctx context.Context, id string) (any, error) {
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	e, err := c.serviceEntry(id, "")
	if err != nil {
		return nil, err
	}
	c.services.mu.RLock()
	status := cloneStatus(e)
	revision := e.revision
	child := e.child
	c.services.mu.RUnlock()
	checks := []serviceCheck{}
	state := "info"
	detail := "服务当前停止"
	if child != nil {
		state = "ok"
		detail = "当前进程由本次应用实例管理"
	}
	if status.LastError != nil {
		state = "error"
		detail = status.LastError.Message
	}
	checks = append(checks, serviceCheck{ID: "status", State: state, Method: "process", Detail: detail, Lines: []string{}})
	ports := []int{}
	if status.Port > 0 {
		ports = append(ports, status.Port)
	}
	if e.pkg.ID == "php" && status.Port > 0 {
		for i := 1; i < phpPoolWorkers; i++ {
			ports = append(ports, status.Port+i)
		}
	}
	portCheck := serviceCheck{ID: "port", State: "ok", Method: "tcp", Lines: []string{}}
	connections, err := gnet.ConnectionsWithContext(ctx, "tcp")
	if err != nil {
		portCheck.State = "unavailable"
		portCheck.Detail = "无法读取 TCP 监听归属"
	} else {
		for _, port := range ports {
			found := false
			for _, conn := range connections {
				if conn.Status != "LISTEN" || int(conn.Laddr.Port) != port {
					continue
				}
				found = true
				owned := false
				if child != nil {
					owned, _ = child.OwnsPID(int(conn.Pid))
				}
				portCheck.Lines = append(portCheck.Lines, fmt.Sprintf(":%d PID %d 受管=%t", port, conn.Pid, owned))
				if !owned {
					portCheck.State = "warning"
				}
			}
			if !found {
				portCheck.Lines = append(portCheck.Lines, fmt.Sprintf(":%d 未监听", port))
				if child != nil {
					portCheck.State = "error"
				}
			}
		}
		portCheck.Detail = "仅检查声明的 TCP 主端口及 PHP 四端口池，不包含 UDP 或额外 HTTPS 监听"
	}
	if len(ports) == 0 {
		portCheck.State = "skipped"
		portCheck.Detail = "此服务没有声明 TCP 端口"
	}
	checks = append(checks, portCheck)
	targets, err := c.configTargets(ctx)
	config := serviceCheck{ID: "config", State: "skipped", Method: "none", Detail: "此服务没有注册可检查配置", Lines: []string{}}
	if err != nil {
		config.State = "unavailable"
		config.Detail = err.Error()
	} else {
		for _, t := range targets {
			if t.spec.pkg == e.pkg.ID && t.version == e.pkg.Version {
				result := c.checkConfig(ctx, t)
				config.Method = result.Method
				config.Detail = result.Detail
				config.State = map[string]string{"ok": "ok", "warning": "warning", "fail": "error", "skipped": "skipped"}[result.Status]
				break
			}
		}
	}
	checks = append(checks, config)
	logs, err := readServiceLog(string(status.LogFile), 80)
	logCheck := serviceCheck{ID: "logs", State: "info", Method: "log-keywords", Detail: "最近 80 行日志；日志内容不代表当前健康状态", Lines: []string{}}
	if err != nil {
		logCheck.State = "unavailable"
		logCheck.Detail = err.Error()
	} else {
		for _, row := range logs {
			logCheck.Lines = append(logCheck.Lines, row["line"].(string))
		}
		logCheck.Lines = strings.Split(redactDiagnostic(strings.Join(logCheck.Lines, "\n")), "\n")
	}
	checks = append(checks, logCheck)
	warnings := []string{}
	c.services.mu.RLock()
	changed := e.revision != revision || e.status.State != status.State
	c.services.mu.RUnlock()
	if changed {
		warnings = append(warnings, "诊断期间服务状态发生变化，请重新检查")
	}
	for i := range checks {
		checks[i].Detail = redactDiagnostic(checks[i].Detail)
	}
	return map[string]any{"service": status, "checks": checks, "checkedAt": time.Now().Unix(), "warnings": warnings}, nil
}
func (c *Core) registerDiagnostics() {
	c.registerDiagnosticsBundle()
	c.handle("health_check", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.healthCheck(ctx) })
	c.handle("diagnose_service", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		return c.diagnoseService(ctx, in.ID)
	})
	c.handle("diagnose_port", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Port int `json:"port"`
		}](data)
		if err != nil {
			return nil, err
		}
		if in.Port < 1 || in.Port > 65535 {
			return nil, invalid("端口必须为 1–65535")
		}
		connections, err := gnet.ConnectionsWithContext(ctx, "tcp")
		if err != nil {
			return nil, err
		}
		result := map[string]any{"port": in.Port, "inUse": false, "pid": nil, "processName": nil, "cmdline": nil}
		for _, conn := range connections {
			if conn.Status != "LISTEN" || int(conn.Laddr.Port) != in.Port {
				continue
			}
			result["inUse"], result["pid"] = true, conn.Pid
			if p, err := gprocess.NewProcessWithContext(ctx, conn.Pid); err == nil {
				if name, err := p.NameWithContext(ctx); err == nil {
					result["processName"] = name
				}
				if command, err := p.CmdlineWithContext(ctx); err == nil {
					result["cmdline"] = redactDiagnostic(command)
				}
			}
			break
		}
		return result, nil
	})
	c.handle("validate_configs", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Only []string `json:"only"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		targets, err := c.configTargets(ctx)
		if err != nil {
			return nil, err
		}
		out := []configCheck{}
		for _, t := range targets {
			selected := len(in.Only) == 0
			for _, key := range in.Only {
				if key == t.key || key == t.spec.kind || key == t.spec.pkg {
					selected = true
				}
			}
			if selected {
				out = append(out, c.checkConfig(ctx, t))
			}
		}
		return out, nil
	})
	c.handle("full_log", func(_ context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID string `json:"id"`
		}](data)
		if err != nil {
			return nil, err
		}
		e, err := c.serviceEntry(in.ID, "")
		if err != nil {
			return nil, err
		}
		c.services.mu.RLock()
		path := string(e.status.LogFile)
		c.services.mu.RUnlock()
		content, err := readConfigText(path)
		if os.IsNotExist(err) {
			return "", nil
		}
		return content, err
	})
	c.handle("export_log", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ID   string `json:"id"`
			Dest string `json:"dest"`
		}](data)
		if err != nil {
			return nil, err
		}
		args, _ := json.Marshal(map[string]string{"id": in.ID})
		value, err := c.handlers["full_log"](ctx, args)
		if err != nil {
			return nil, err
		}
		if !filepath.IsAbs(in.Dest) {
			return nil, invalid("请选择日志导出绝对路径")
		}
		content := value.(string)
		return len(content), atomicWrite(in.Dest, []byte(content))
	})
	c.handle("log_export", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			ServiceID     string  `json:"serviceId"`
			Content       string  `json:"content"`
			SuggestedName *string `json:"suggestedName"`
		}](data)
		if err != nil {
			return nil, err
		}
		if strings.TrimSpace(in.Content) == "" {
			return nil, invalid("没有可导出的日志内容")
		}
		dir := filepath.Join(c.Home, "logs", "export")
		if err = managedDirectory(c.Home, dir); err != nil {
			return nil, err
		}
		name := sanitizeLogName(in.ServiceID) + "-" + time.Now().Format("20060102-150405")
		if in.SuggestedName != nil && strings.TrimSpace(*in.SuggestedName) != "" {
			name = sanitizeLogName(*in.SuggestedName)
		}
		pending, err := os.CreateTemp(dir, ".export-*.tmp")
		if err != nil {
			return nil, err
		}
		defer os.Remove(pending.Name())
		_, writeErr := pending.WriteString(in.Content)
		syncErr := pending.Sync()
		closeErr := pending.Close()
		if err = errors.Join(writeErr, syncErr, closeErr); err != nil {
			return nil, err
		}
		for i := 1; i <= 999; i++ {
			candidate := name + ".log"
			if i > 1 {
				candidate = fmt.Sprintf("%s-%d.log", name, i)
			}
			path := filepath.Join(dir, candidate)
			if err = ctx.Err(); err != nil {
				return nil, err
			}
			if err = os.Link(pending.Name(), path); os.IsExist(err) {
				continue
			} else if err != nil {
				return nil, err
			}
			return Path(path), nil
		}
		return nil, &AppError{Code: "LOG_NAME_EXHAUSTED", Message: "同名日志导出过多，请更换文件名"}
	})
}

func sanitizeLogName(value string) string {
	value = strings.TrimSuffix(strings.TrimSpace(value), ".log")
	var b strings.Builder
	for _, r := range value {
		if r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '-' || r == '.' {
			b.WriteRune(r)
		} else {
			b.WriteByte('_')
		}
	}
	name := strings.Trim(b.String(), "._")
	if len(name) > 160 {
		name = name[:160]
	}
	for strings.Contains(name, "..") {
		name = strings.ReplaceAll(name, "..", ".")
	}
	base := strings.ToUpper(strings.Split(name, ".")[0])
	if base == "CON" || base == "PRN" || base == "AUX" || base == "NUL" || len(base) == 4 && (strings.HasPrefix(base, "COM") || strings.HasPrefix(base, "LPT")) && base[3] >= '1' && base[3] <= '9' {
		name = "log-" + name
	}
	if name == "" {
		return "log"
	}
	return name
}
