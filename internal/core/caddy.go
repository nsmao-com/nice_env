package core

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"sort"
	"strings"
)

func (c *Core) prepareCaddy(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	r.etc = filepath.Join(c.Home, "etc", "caddy", e.pkg.Version)
	for _, dir := range []string{r.etc, filepath.Join(c.Home, "etc", "caddy", "sites"), filepath.Join(c.Home, "logs", "caddy"), filepath.Join(r.data, "www"), filepath.Join(r.data, "storage"), filepath.Join(r.data, "xdg")} {
		if err := managedDirectory(c.Home, dir); err != nil {
			return err
		}
	}
	options, err := c.siteOptions(ctx, Site{Runtime: siteRuntime{WebServer: "caddy"}}, "", false)
	if err != nil {
		return err
	}
	path := filepath.Join(r.etc, "Caddyfile")
	previous, err := readConfigText(path)
	exists := err == nil
	if err != nil && !os.IsNotExist(err) {
		return err
	}
	text := previous
	if !exists {
		text = fmt.Sprintf("{\n    storage file_system %s\n}\nhttp://127.0.0.1:%d {\n    bind 127.0.0.1\n    respond NiceEnv\n}\n", caddyPath(filepath.Join(r.data, "storage")), options.http)
	}
	tokens, err := caddyTokens(text)
	if err != nil {
		return err
	}
	if len(tokens) == 0 || tokens[0].value != "{" {
		text = "{\n}\n" + text
		tokens, err = caddyTokens(text)
		if err != nil {
			return err
		}
	}
	closing := -1
	hasPKI := false
	type edit struct {
		start, end int
		value      string
	}
	edits := []edit{}
	for i, token := range tokens {
		if i > 0 && token.depth == 0 && token.value == "}" {
			closing = token.start
			break
		}
		if token.depth != 1 {
			continue
		}
		lineStart := strings.LastIndexByte(text[:token.start], '\n') + 1
		if strings.TrimSpace(text[lineStart:token.start]) != "" {
			continue
		}
		if token.value == "pki" {
			hasPKI = true
			end := -1
			local := false
			for j := i + 1; j < len(tokens); j++ {
				next := tokens[j]
				if next.depth == 1 && next.value == "}" {
					end = next.start
					break
				}
				if next.depth == 2 && next.value == "ca" && j+1 < len(tokens) && tokens[j+1].value == "local" {
					local = true
				}
			}
			if end < 0 {
				return invalid("Caddy PKI 块未闭合")
			}
			if !local {
				edits = append(edits, edit{end, end, "        ca local {\n        }\n"})
			}
		}
		if slices.Contains([]string{"admin", "auto_https", "http_port", "https_port", "skip_install_trust"}, token.value) {
			end := strings.IndexByte(text[token.end:], '\n')
			if end < 0 {
				return invalid("Caddy 全局选项必须单独成行")
			}
			end += token.end + 1
			if strings.ContainsAny(text[token.end:end], "{}") {
				return invalid("Caddy 全局选项包含自定义块，未覆盖")
			}
			edits = append(edits, edit{lineStart, end, ""})
		}
	}
	if closing < 0 {
		return invalid("Caddy 全局选项块无效")
	}
	managed := fmt.Sprintf("    admin off\n    auto_https off\n    skip_install_trust\n    http_port %d\n    https_port %d\n", options.http, options.https)
	if !hasPKI {
		managed += "    pki {\n        ca local {\n        }\n    }\n"
	}
	edits = append(edits, edit{closing, closing, managed})
	sort.Slice(edits, func(i, j int) bool { return edits[i].start > edits[j].start })
	for _, change := range edits {
		text = text[:change.start] + change.value + text[change.end:]
	}
	include := "import " + caddyPath(filepath.Join(c.Home, "etc", "caddy", "sites", "*.conf"))
	if !strings.Contains(text, include) {
		text += "\n" + include + "\n"
	}
	// The generated fallback is local; custom site blocks are preserved verbatim.
	fallback := regexp.MustCompile(`(?m)^http://127\.0\.0\.1:[0-9]+ \{\r?\n    bind 127\.0\.0\.1\r?\n    respond NiceEnv\r?\n\}`)
	text = fallback.ReplaceAllString(text, fmt.Sprintf("http://127.0.0.1:%d {\n    bind 127.0.0.1\n    respond NiceEnv\n}", options.http))
	target := configTarget{spec: configSpecs[10], key: "caddy-conf@" + e.pkg.Version, path: path, program: r.program, version: e.pkg.Version}
	validation, err := c.validateConfig(ctx, target, text)
	if err != nil {
		return err
	}
	if !validation.OK {
		return &AppError{Code: "CADDY_CONFIG_INVALID", Message: "Caddy 配置语法校验失败", Detail: strings.Join(validation.Messages, "\n")}
	}
	var expected *string
	if exists {
		expected = &previous
	}
	if err = c.writeConfigExpected(ctx, target, text, expected, !exists); err != nil {
		return err
	}
	r.args = []string{"run", "--config", DisplayPath(path), "--adapter", "caddyfile"}
	r.stopArgs = nil
	r.cwd = filepath.Dir(r.program)
	r.env["XDG_DATA_HOME"] = DisplayPath(filepath.Join(r.data, "xdg"))
	r.env["XDG_CONFIG_HOME"] = r.env["XDG_DATA_HOME"]
	r.siteConfigs, err = c.webSiteSnapshot("caddy")
	return err
}
func (c *Core) reloadCaddy(ctx context.Context) error {
	e, err := c.serviceEntry("caddy", "")
	if err != nil {
		return err
	}
	c.services.mu.RLock()
	active := e.child != nil
	var candidate resolvedService
	if active {
		candidate = *e.resolved
	}
	c.services.mu.RUnlock()
	if !active {
		return nil
	}
	// Existing NiceEnv Caddy configs disable the admin API. Validate first,
	// then restart only the owned instance to apply the new configuration.
	if err = c.prepareCaddy(ctx, e, &candidate); err != nil {
		return err
	}
	if err = c.stopOwned(ctx, e, false); err != nil {
		return err
	}
	return c.startService(ctx, e)
}
