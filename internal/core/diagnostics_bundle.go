package core

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	gnet "github.com/shirou/gopsutil/v4/net"
)

type diagnosticsBundle struct {
	Markdown     string   `json:"markdown"`
	ServiceCount int      `json:"serviceCount"`
	SiteCount    int      `json:"siteCount"`
	LogLines     int      `json:"logLines"`
	Redacted     int      `json:"redacted"`
	GeneratedAt  int64    `json:"generatedAt"`
	Warnings     []string `json:"warnings"`
}

const diagnosticMaxBytes = 2 * 1024 * 1024

func scrubDiagnosticHome(text string) string {
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return text
	}
	home = DisplayPath(home)
	forward := strings.ReplaceAll(home, `\`, "/")
	back := strings.ReplaceAll(forward, "/", `\`)
	variants := []string{strings.ReplaceAll(back, `\`, `\\`), back, forward}
	for _, variant := range variants {
		prefix := ""
		if runtime.GOOS == "windows" {
			prefix = "(?i)"
		}
		pattern := regexp.MustCompile(prefix + regexp.QuoteMeta(variant) + "($|[/\\\\\\s\"'`<>:,;)\\]}])")
		text = pattern.ReplaceAllString(text, "<home>$1")
	}
	return text
}
func (c *Core) scrubDiagnostics(ctx context.Context, text string) (string, int, error) {
	redacted := redactDiagnostic(text)
	count := 0
	before, after := strings.Split(text, "\n"), strings.Split(redacted, "\n")
	for i := range before {
		if i < len(after) && before[i] != after[i] {
			count++
		}
	}
	// Also hide known stored credential values if arbitrary program output
	// contains the value without a recognizable key name.
	rows, err := c.Store.db.QueryContext(ctx, "SELECT key,value FROM settings")
	if err != nil {
		return "", 0, err
	}
	defer rows.Close()
	for rows.Next() {
		var key, value string
		if err = rows.Scan(&key, &value); err != nil {
			return "", 0, err
		}
		if !sensitiveDiagnosticLine.MatchString(key) || len(value) < 4 {
			continue
		}
		for _, secret := range []string{value, strings.Trim(value, `"`)} {
			if len(secret) >= 4 && strings.Contains(redacted, secret) {
				count += strings.Count(redacted, secret)
				redacted = strings.ReplaceAll(redacted, secret, "***")
			}
		}
	}
	return scrubDiagnosticHome(redacted), count, rows.Err()
}
func (c *Core) buildDiagnostics(ctx context.Context) (diagnosticsBundle, error) {
	result := diagnosticsBundle{GeneratedAt: time.Now().Unix(), Warnings: []string{}}
	if !c.services.lifecycle.TryLock() {
		return result, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	var md strings.Builder
	appendText := func(text string) {
		if md.Len()+len(text) > diagnosticMaxBytes-128*1024 {
			result.Warnings = append(result.Warnings, "报告达到大小上限，部分内容未收录")
			return
		}
		md.WriteString(text)
	}
	appendText(fmt.Sprintf("# NiceEnv 诊断报告\n\n> 采集时的状态快照，不等同于业务健康检查。分享前请检查自定义日志、域名和路径。\n\n- 应用版本：%s\n- 后端：Go + Wails v3\n- 系统：%s %s\n- 数据目录：%s\n- 生成时间：%s\n\n", c.Version, runtime.GOOS, runtime.GOARCH, DisplayPath(c.Home), time.Now().Format(time.RFC3339)))
	statuses := c.serviceStatuses()
	result.ServiceCount = len(statuses)
	appendText("## 服务状态\n\n")
	for _, status := range statuses {
		appendText(fmt.Sprintf("- %s %s：%s，端口 %d，运行 %d 秒\n", status.ID, status.Version, status.State, status.Port, status.UptimeSec))
		if status.LastError != nil {
			appendText(status.LastError.Message + "\n" + status.LastError.Detail + "\n")
		}
	}
	sites, err := c.sites(ctx)
	if err != nil {
		return result, err
	}
	result.SiteCount = len(sites)
	appendText("\n## 站点\n\n")
	for _, site := range sites {
		appendText(fmt.Sprintf("- %s：%s → %s（%s，HTTPS=%t）\n", site.Name, strings.Join(site.Domains, ", "), DisplayPath(string(site.RootDir)), site.Runtime.Kind, site.HTTPS))
	}
	installed, err := c.Store.tableRows(ctx, "SELECT id,version,category FROM installed ORDER BY id,version", nil, nil)
	if err != nil {
		return result, err
	}
	appendText("\n## 已安装套件\n\n")
	for _, item := range installed {
		appendText(fmt.Sprintf("- %s %s（%s）\n", item["id"], item["version"], item["category"]))
	}
	appendText("\n## 服务端口\n\n")
	connections, err := gnet.ConnectionsWithContext(ctx, "tcp")
	if err != nil {
		result.Warnings = append(result.Warnings, "未能读取 TCP 监听状态")
	} else {
		for _, status := range statuses {
			if status.Port <= 0 {
				continue
			}
			ports := 1
			if strings.Split(status.ID, "@")[0] == "php" {
				ports = phpPoolWorkers
			}
			for offset := 0; offset < ports; offset++ {
				port := status.Port + offset
				pids := map[int32]bool{}
				for _, conn := range connections {
					if conn.Status == "LISTEN" && int(conn.Laddr.Port) == port {
						pids[conn.Pid] = true
					}
				}
				list := []int{}
				for pid := range pids {
					list = append(list, int(pid))
				}
				sort.Ints(list)
				appendText(fmt.Sprintf("- %s :%d：PID %v\n", status.ID, port, list))
			}
		}
	}
	appendText("\n## 最近日志\n\n")
	for _, status := range statuses {
		if status.LogFile == "" {
			continue
		}
		logs, err := readServiceLog(string(status.LogFile), 80)
		if err != nil {
			result.Warnings = append(result.Warnings, status.ID+" 日志未能读取")
			continue
		}
		lines := []string{}
		size := 0
		for _, row := range logs {
			line, _ := row["line"].(string)
			if size+len(line) > 64*1024 {
				result.Warnings = append(result.Warnings, status.ID+" 日志超过 64 KiB，仅收录部分")
				break
			}
			lines = append(lines, line)
			size += len(line)
		}
		result.LogLines += len(lines)
		appendText("### " + status.ID + "\n\n```text\n" + strings.ReplaceAll(strings.Join(lines, "\n"), "```", "'''") + "\n```\n\n")
	}
	appendText("## 配置摘录\n\n")
	targets, err := c.configTargets(ctx)
	if err != nil {
		result.Warnings = append(result.Warnings, "未能列出配置文件")
	} else {
		for _, target := range targets {
			info, err := os.Lstat(target.path)
			if err != nil || !info.Mode().IsRegular() || info.Size() > 64*1024 {
				result.Warnings = append(result.Warnings, target.key+" 配置缺失、不是普通文件或超过 64 KiB，未收录")
				continue
			}
			content, err := readConfigText(target.path)
			if err != nil {
				result.Warnings = append(result.Warnings, target.key+" 配置未能读取")
				continue
			}
			appendText("### " + target.key + "\n\n```text\n" + strings.ReplaceAll(content, "```", "'''") + "\n```\n\n")
		}
	}
	if len(result.Warnings) > 0 {
		appendText("## 采集说明\n\n- " + strings.Join(result.Warnings, "\n- ") + "\n")
	}
	result.Markdown, result.Redacted, err = c.scrubDiagnostics(ctx, md.String())
	for i, w := range result.Warnings {
		result.Warnings[i] = scrubDiagnosticHome(redactDiagnostic(w))
	}
	return result, err
}
func (c *Core) registerDiagnosticsBundle() {
	c.handle("diagnostics_build", func(ctx context.Context, _ json.RawMessage) (any, error) { return c.buildDiagnostics(ctx) })
	c.handle("diagnostics_save", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Bundle diagnosticsBundle `json:"bundle"`
		}](data)
		if err != nil {
			return nil, err
		}
		if in.Bundle.Markdown == "" || len(in.Bundle.Markdown) > diagnosticMaxBytes || !utf8.ValidString(in.Bundle.Markdown) {
			return nil, invalid("诊断报告为空、编码无效或超过 2 MiB")
		}
		text, _, err := c.scrubDiagnostics(ctx, in.Bundle.Markdown)
		if err != nil {
			return nil, err
		}
		dir := filepath.Join(c.Home, "diagnostics")
		if err = managedDirectory(c.Home, dir); err != nil {
			return nil, err
		}
		file, err := os.CreateTemp(dir, "niceenv-*.md")
		if err != nil {
			return nil, err
		}
		ok := false
		defer func() {
			file.Close()
			if !ok {
				os.Remove(file.Name())
			}
		}()
		if _, err = file.WriteString(text); err != nil {
			return nil, err
		}
		if err = file.Sync(); err != nil {
			return nil, err
		}
		if err = file.Close(); err != nil {
			return nil, err
		}
		ok = true
		return Path(file.Name()), nil
	})
}
