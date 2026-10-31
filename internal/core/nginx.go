package core

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"time"

	"niceenv/internal/process"
)

type nginxNode struct {
	start, end, open int
	words            []string
	children         []nginxNode
}
type nginxToken struct {
	start, end int
	word       string
	delimiter  byte
}

func nginxStructureError() error {
	return &AppError{Code: "CONFIG_STRUCTURE", Message: "无法安全解析 Nginx 配置结构，原文件已保留"}
}

func parseNginx(text string) ([]nginxNode, error) {
	tokens := []nginxToken{}
	for pos := 0; pos < len(text); {
		if strings.ContainsRune(" \t\r\n", rune(text[pos])) {
			pos++
			continue
		}
		if text[pos] == '#' {
			for pos < len(text) && text[pos] != '\n' {
				pos++
			}
			continue
		}
		start := pos
		if strings.ContainsRune("{};", rune(text[pos])) {
			tokens = append(tokens, nginxToken{start: pos, end: pos + 1, delimiter: text[pos]})
			pos++
			continue
		}
		word := strings.Builder{}
		var quote byte
		for pos < len(text) {
			ch := text[pos]
			if ch == '\\' {
				pos++
				if pos == len(text) {
					return nil, nginxStructureError()
				}
				switch text[pos] {
				case 't':
					word.WriteByte('\t')
				case 'r':
					word.WriteByte('\r')
				case 'n':
					word.WriteByte('\n')
				case '\\', '\'', '"':
					word.WriteByte(text[pos])
				default:
					word.WriteByte('\\')
					word.WriteByte(text[pos])
				}
				pos++
				continue
			}
			if quote != 0 {
				if ch == quote {
					quote = 0
				} else {
					word.WriteByte(ch)
				}
				pos++
				continue
			}
			if ch == '\'' || ch == '"' {
				quote = ch
				pos++
				continue
			}
			if ch == '$' && pos+1 < len(text) && text[pos+1] == '{' {
				for pos < len(text) && text[pos] != '}' {
					word.WriteByte(text[pos])
					pos++
				}
				if pos == len(text) {
					return nil, nginxStructureError()
				}
				word.WriteByte('}')
				pos++
				continue
			}
			if strings.ContainsRune(" \t\r\n{};#", rune(ch)) {
				break
			}
			word.WriteByte(ch)
			pos++
		}
		if quote != 0 {
			return nil, nginxStructureError()
		}
		tokens = append(tokens, nginxToken{start: start, end: pos, word: word.String()})
	}
	position := 0
	var parse func(int) ([]nginxNode, error)
	parse = func(depth int) ([]nginxNode, error) {
		if depth > 128 {
			return nil, nginxStructureError()
		}
		nodes := []nginxNode{}
		for position < len(tokens) {
			if tokens[position].delimiter == '}' {
				if depth == 0 {
					return nil, nginxStructureError()
				}
				position++
				return nodes, nil
			}
			node := nginxNode{start: tokens[position].start, open: -1, words: []string{}}
			for position < len(tokens) && tokens[position].delimiter == 0 {
				node.words = append(node.words, tokens[position].word)
				position++
			}
			if len(node.words) == 0 || position == len(tokens) {
				return nil, nginxStructureError()
			}
			token := tokens[position]
			position++
			switch token.delimiter {
			case ';':
				node.end = token.end
			case '{':
				node.open = token.start
				children, err := parse(depth + 1)
				if err != nil {
					return nil, err
				}
				node.children = children
				node.end = tokens[position-1].end
			default:
				return nil, nginxStructureError()
			}
			nodes = append(nodes, node)
		}
		if depth != 0 {
			return nil, nginxStructureError()
		}
		return nodes, nil
	}
	return parse(0)
}

var nginxPHPName = regexp.MustCompile(`^nsb_php_[a-zA-Z0-9_+-]+$`)

func nginxQuote(path string) string {
	value := filepath.ToSlash(DisplayPath(path))
	value = strings.ReplaceAll(value, `\`, `\\`)
	value = strings.ReplaceAll(value, `"`, `\"`)
	value = strings.ReplaceAll(value, "$", `\$`)
	return `"` + value + `"`
}

// Repair only reserved PHP upstream blocks. A stopped pool remains a valid
// named upstream with a down backend, so one PHP version cannot break Nginx -t.
func (c *Core) completeNginxPools(text string) (string, error) {
	nodes, err := parseNginx(text)
	if err != nil {
		return "", err
	}
	var http *nginxNode
	for i := range nodes {
		if len(nodes[i].words) == 1 && nodes[i].words[0] == "http" && nodes[i].open >= 0 {
			if http != nil {
				return "", nginxStructureError()
			}
			http = &nodes[i]
		}
	}
	if http == nil {
		return "", nginxStructureError()
	}
	names := map[string]bool{}
	ports := map[string][]int{}
	for _, status := range c.serviceStatuses() {
		if strings.HasPrefix(status.ID, "php@") {
			name := "nsb_php_" + strings.ReplaceAll(status.Version, ".", "_")
			if !nginxPHPName.MatchString(name) {
				return "", invalid("PHP 版本不能生成有效 upstream 名称")
			}
			names[name] = true
			if status.State == "running" && len(status.PIDs) > 0 && status.Port > 0 {
				for offset := 0; offset < phpPoolWorkers; offset++ {
					ports[name] = append(ports[name], status.Port+offset)
				}
			}
		}
	}
	var collect func([]nginxNode)
	collect = func(nodes []nginxNode) {
		for _, node := range nodes {
			if len(node.words) == 2 && node.words[0] == "fastcgi_pass" && nginxPHPName.MatchString(node.words[1]) {
				names[node.words[1]] = true
			}
			collect(node.children)
		}
	}
	collect(nodes)
	sites, err := os.ReadDir(filepath.Join(c.Home, "etc", "nginx", "sites"))
	if err != nil && !os.IsNotExist(err) {
		return "", err
	}
	for _, file := range sites {
		if file.IsDir() || !strings.HasSuffix(file.Name(), ".conf") {
			continue
		}
		content, err := readConfigText(filepath.Join(c.Home, "etc", "nginx", "sites", file.Name()))
		if err != nil {
			return "", err
		}
		siteNodes, err := parseNginx(content)
		if err != nil {
			return "", err
		}
		collect(siteNodes)
	}
	remove := []nginxNode{}
	type replacement struct {
		start, end int
		text       string
	}
	edits := []replacement{}
	existing := map[string]bool{}
	render := func(name string) string {
		block := strings.Builder{}
		fmt.Fprintf(&block, "upstream %s {\n", name)
		if len(ports[name]) == 0 {
			block.WriteString("        server 127.0.0.1:9 down; # PHP pool stopped\n")
		} else {
			for _, port := range ports[name] {
				fmt.Fprintf(&block, "        server 127.0.0.1:%d;\n", port)
			}
		}
		block.WriteString("    }")
		return block.String()
	}
	for _, node := range http.children {
		if len(node.words) == 2 && node.words[0] == "upstream" && nginxPHPName.MatchString(node.words[1]) {
			names[node.words[1]] = true
			remove = append(remove, node)
			if existing[node.words[1]] {
				return "", &AppError{Code: "NGINX_DUPLICATE_UPSTREAM", Message: "Nginx 配置包含重复的 PHP upstream，未覆盖配置"}
			}
			existing[node.words[1]] = true
		}
	}
	ordered := make([]string, 0, len(names))
	for name := range names {
		ordered = append(ordered, name)
	}
	sort.Strings(ordered)
	blocks := strings.Builder{}
	for _, name := range ordered {
		if !existing[name] {
			blocks.WriteString("\n    " + render(name) + "\n")
		}
	}
	for _, node := range remove {
		edits = append(edits, replacement{node.start, node.end, render(node.words[1])})
	}
	for i := len(edits) - 1; i >= 0; i-- {
		edit := edits[i]
		text = text[:edit.start] + edit.text + text[edit.end:]
	}
	return text[:http.open+1] + blocks.String() + text[http.open+1:], nil
}

func (c *Core) prepareNginx(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	r.etc = filepath.Join(c.Home, "etc", "nginx")
	if err := managedDirectory(c.Home, filepath.Join(r.etc, "sites")); err != nil {
		return err
	}
	path := filepath.Join(r.etc, "nginx.conf")
	previous, err := readConfigText(path)
	exists := err == nil
	if err != nil && !os.IsNotExist(err) {
		return err
	}
	text := previous
	if !exists {
		if strings.ContainsAny(filepath.ToSlash(r.etc), "*?[") {
			return invalid("Nginx include 目录含 glob 特殊字符，请选择不含这些字符的数据目录")
		}
		text = fmt.Sprintf("worker_processes 1;\nerror_log %s;\npid %s;\nevents { worker_connections 1024; }\nhttp {\n    include %s;\n    default_type application/octet-stream;\n    access_log off;\n    server { listen 127.0.0.1:%d; server_name localhost; location / { return 200 'NiceEnv'; } }\n    include %s;\n}\n", nginxQuote(filepath.Join(filepath.Dir(r.log), "error.log")), nginxQuote(filepath.Join(r.etc, "nginx.pid")), nginxQuote(filepath.Join(filepath.Dir(r.program), "conf", "mime.types")), r.port, nginxQuote(filepath.Join(r.etc, "sites", "*.conf")))
	}
	updated, err := c.completeNginxPools(text)
	if err != nil {
		return err
	}
	loaded, err := c.nginxSiteSnapshot()
	if err != nil {
		return err
	}
	target := configTarget{spec: configSpecs[0], key: "nginx-main", path: path, program: r.program, version: e.pkg.Version}
	validation, err := c.validateConfig(ctx, target, updated)
	if err != nil {
		return err
	}
	if !validation.OK {
		return &AppError{Code: "NGINX_CONFIG_INVALID", Message: "Nginx 配置语法校验失败，原文件已保留", Detail: strings.Join(validation.Messages, "\n")}
	}
	var expected *string
	if exists {
		expected = &previous
	}
	if err = c.writeConfig(ctx, target, updated, expected); err != nil {
		return err
	}
	r.args = []string{"-p", DisplayPath(filepath.Dir(r.program)) + string(filepath.Separator), "-c", DisplayPath(path)}
	if runtime.GOOS != "windows" {
		r.args = append(r.args, "-g", "daemon off;")
	}
	r.stopArgs = append(append([]string{}, r.args...), "-s", "quit")
	r.siteConfigs = loaded
	return nil
}

func (c *Core) nginxSiteSnapshot() (map[string]string, error) {
	dir := filepath.Join(c.Home, "etc", "nginx", "sites")
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	result := map[string]string{}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".conf") {
			continue
		}
		text, err := readConfigText(filepath.Join(dir, entry.Name()))
		if err != nil {
			return nil, err
		}
		result[strings.TrimSuffix(entry.Name(), ".conf")] = text
	}
	return result, nil
}

func (c *Core) verifyNginxStop(e *serviceEntry, r *resolvedService) error {
	text, err := readConfigText(filepath.Join(r.etc, "nginx.conf"))
	if err != nil {
		return err
	}
	nodes, err := parseNginx(text)
	if err != nil {
		return err
	}
	pidPath := filepath.Join(filepath.Dir(r.program), "logs", "nginx.pid")
	for _, node := range nodes {
		if len(node.words) == 2 && node.words[0] == "pid" {
			pidPath = node.words[1]
			if !filepath.IsAbs(pidPath) {
				pidPath = filepath.Join(filepath.Dir(r.program), pidPath)
			}
		}
	}
	pidText, err := readConfigText(pidPath)
	if err != nil {
		return err
	}
	pid, err := strconv.Atoi(strings.TrimSpace(pidText))
	if err != nil {
		return invalid("Nginx PID 文件无效")
	}
	c.services.mu.RLock()
	child := e.child
	c.services.mu.RUnlock()
	if child == nil {
		return nil
	}
	owned, err := child.OwnsPID(pid)
	if err != nil || !owned {
		return &AppError{Code: "NGINX_PID_UNVERIFIED", Message: "Nginx 停机目标不属于本次受管进程，未发送信号"}
	}
	return nil
}

// Called with the lifecycle lock held, after pool state changes. Preserve the
// existing configuration and restore it if nginx rejects the reload command.
func (c *Core) reloadNginxPools(ctx context.Context) error {
	c.services.mu.RLock()
	e := c.services.entries["nginx"]
	if e == nil || e.child == nil || e.status.State != "running" {
		c.services.mu.RUnlock()
		return nil
	}
	r := *e.resolved
	c.services.mu.RUnlock()
	path := filepath.Join(r.etc, "nginx.conf")
	previous, err := readConfigText(path)
	if err != nil {
		return err
	}
	if err = c.verifyNginxStop(e, &r); err != nil {
		return err
	}
	if err = c.prepareNginx(ctx, e, &r); err != nil {
		return err
	}
	checkCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	output := process.NewLimitedBuffer(8192)
	args := append(append([]string{}, r.args...), "-s", "reload")
	child, err := process.Start(checkCtx, process.Spec{Program: r.program, Args: args, Directory: r.cwd, Environment: r.env, Stdout: output, Stderr: output})
	if err == nil {
		result := child.Wait()
		if result.Err != nil || result.Code != 0 || checkCtx.Err() != nil {
			err = &AppError{Code: "NGINX_RELOAD_FAILED", Message: "Nginx 未能重新加载 PHP 池配置", Detail: output.String()}
		}
	}
	if err != nil {
		current, readErr := readConfigText(path)
		if readErr == nil {
			target := configTarget{spec: configSpecs[0], key: "nginx-main", path: path, program: r.program, version: e.pkg.Version}
			if restoreErr := c.writeConfig(context.Background(), target, previous, &current); restoreErr != nil {
				return &AppError{Code: "NGINX_RELOAD_ROLLBACK_FAILED", Message: "Nginx 加载失败且原配置恢复失败，请检查配置备份", Detail: err.Error() + "; " + restoreErr.Error()}
			}
		}
		return err
	}
	c.services.mu.Lock()
	e.resolved = &r
	c.services.mu.Unlock()
	return nil
}
