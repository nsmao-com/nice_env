package core

import (
	"context"
	"os"
	"path/filepath"
	"slices"
	"strings"
)

// Apache resolves includes and resource paths against ServerRoot, including
// files whose suffix is unrelated to configuration (or misleadingly .conf).
func planApacheMove(ctx context.Context, root string, rebase dataPathRebase, overrides map[string]string) (*nginxMovePlan, error) {
	plan := &nginxMovePlan{roles: map[string]uint8{}, edits: map[string]string{}}
	main := filepath.Join(root, "etc", "apache", "httpd.conf")
	serverRoot := ""
	defines := map[string]string{}
	active := map[string]bool{}
	total := 0
	toCopy := func(path string) string {
		path = (dataPathRebase{rebase.source, root}).path(path)
		return filepath.Clean((dataPathRebase{rebase.target, root}).path(path))
	}
	expand := func(value string) (string, error) {
		for range 32 {
			start := strings.Index(value, "${")
			if start < 0 {
				return value, nil
			}
			end := strings.IndexByte(value[start:], '}')
			if end < 0 {
				break
			}
			key := value[start+2 : start+end]
			replacement, ok := defines[key]
			if !ok {
				return "", invalid("Apache 路径使用无法确定的变量：" + key)
			}
			value = value[:start] + replacement + value[start+end+1:]
		}
		return "", invalid("Apache 路径变量递归或格式无效")
	}
	var visit func(string, int) error
	visit = func(path string, depth int) error {
		if err := ctx.Err(); err != nil {
			return err
		}
		if depth > 64 || len(plan.roles) > 4096 {
			return invalid("Apache include 层数或文件数超过安全限制")
		}
		key := nginxMoveKey(path)
		if active[key] || plan.roles[key]&4 != 0 {
			return invalid("Apache include 存在循环或与凭据资源共用：" + DisplayPath(path))
		}
		active[key] = true
		defer delete(active, key)
		plan.roles[key] = 1
		body, overridden := "", false
		for candidate, content := range overrides {
			if nginxMoveKey(candidate) == key {
				body, overridden = content, true
			}
		}
		var err error
		if !overridden {
			body, err = readConfigText(path)
		}
		if err != nil {
			return err
		}
		total += len(body)
		if total > 64*1024*1024 {
			return invalid("Apache include 总大小超过 64 MiB")
		}
		lines := strings.SplitAfter(body, "\n")
		for i, line := range lines {
			tokens, err := moveTokens(line)
			if err != nil {
				return err
			}
			if len(tokens) < 2 {
				continue
			}
			name := strings.ToLower(strings.Trim(tokens[0].value, "<>"))
			if strings.HasSuffix(strings.TrimSpace(line), "\\") {
				return invalid("Apache 配置使用续行，请先合并路径指令后迁移：" + DisplayPath(path))
			}
			if name == "define" && len(tokens) > 2 {
				defines[tokens[1].value] = tokens[2].value
			}
			isPath := movePathKey(name) || name == "alias" || name == "loadfile"
			for _, token := range tokens[1:] {
				if strings.Contains(token.value, "${NSB_ETC}") && !isPath {
					return invalid("Apache 托管目录变量用于业务内容，不能自动改写 NSB_ETC")
				}
				if isPath && strings.Contains(token.value, "${") {
					expanded, err := expand(token.value)
					if err != nil {
						return err
					}
					if rebase.path(expanded) != expanded && !strings.HasPrefix(token.value, "${NSB_ETC}") {
						return invalid("Apache 自定义路径变量引用旧目录，请先改为明确路径")
					}
				}
			}
			if name == "serverroot" {
				serverRoot, err = expand(tokens[1].value)
				if err != nil || !filepath.IsAbs(serverRoot) {
					return invalid("Apache ServerRoot 不是可确定的绝对目录")
				}
				serverRoot = toCopy(serverRoot)
			}
			resource := slices.Contains([]string{"authuserfile", "authgroupfile", "typesconfig", "sslcertificatefile", "sslcertificatekeyfile", "sslcertificatechainfile", "sslcacertificatefile", "sslcacertificatepath", "sslcarevocationfile", "sslcarevocationpath"}, name)
			include := name == "include" || name == "includeoptional"
			if include || resource {
				value, err := expand(tokens[1].value)
				if err != nil {
					return err
				}
				if !filepath.IsAbs(value) {
					if serverRoot == "" {
						return invalid("Apache 相对引用缺少 ServerRoot，无法自动迁移")
					}
					value = filepath.Join(serverRoot, value)
				}
				value = toCopy(value)
				paths := []string{value}
				if include && strings.ContainsAny(value, "*?[") {
					if strings.Contains(value, "[") {
						return invalid("Apache include 字符类通配符无法安全迁移，请使用 * 或明确文件名")
					}
					paths, err = filepath.Glob(value)
					if err != nil {
						return err
					}
				}
				for _, included := range paths {
					canonical, err := filepath.EvalSymlinks(included)
					if os.IsNotExist(err) && name == "includeoptional" {
						continue
					}
					if err != nil {
						return err
					}
					if pathWithin(root, included) != pathWithin(root, canonical) || !pathWithin(root, included) && pathWithin(rebase.source, canonical) {
						return invalid("Apache 引用通过链接跨越数据目录，不能自动迁移")
					}
					if resource {
						k := nginxMoveKey(canonical)
						if plan.roles[k]&1 != 0 {
							return invalid("Apache 配置与凭据共用文件，不能自动迁移")
						}
						plan.roles[k] = 4
					} else if err := visit(canonical, depth+1); err != nil {
						return err
					}
				}
			}
			lines[i], err = rebaseConfigLine(rebase, path, line)
			if err != nil {
				return err
			}
		}
		next := strings.Join(lines, "")
		if next != body {
			if !pathWithin(root, path) {
				return invalid("外部 Apache 配置引用旧目录，请先调整：" + DisplayPath(path))
			}
			plan.edits[path] = next
		}
		return nil
	}
	if _, err := os.Stat(main); err == nil {
		if err := visit(main, 0); err != nil {
			return nil, err
		}
	} else if !os.IsNotExist(err) {
		return nil, err
	}
	for _, pattern := range []string{"*.conf", "*.conf.disabled"} {
		sites, err := filepath.Glob(filepath.Join(root, "etc", "apache", "sites", pattern))
		if err != nil {
			return nil, err
		}
		for _, site := range sites {
			if !plan.handled(site) {
				if err := visit(site, 0); err != nil {
					return nil, err
				}
			}
		}
	}
	for path := range overrides {
		if !plan.handled(path) && strings.Contains(filepath.ToSlash(path), "/etc/apache/") {
			if err := visit(path, 0); err != nil {
				return nil, err
			}
		}
	}
	return plan, nil
}
