package core

import (
	"context"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"slices"
	"strings"
)

// Plan the include graph before writing any bytes. A suffix does not tell us
// whether a file is configuration, map data, or a password/certificate resource.
type nginxMovePlan struct {
	roles map[string]uint8
	edits map[string]string
}

func nginxMoveKey(path string) string {
	path = filepath.Clean(path)
	if runtime.GOOS == "windows" || runtime.GOOS == "darwin" {
		path = strings.ToLower(path)
	}
	return path
}

func (p *nginxMovePlan) handled(path string) bool {
	_, ok := p.roles[nginxMoveKey(path)]
	if !ok {
		for resource, role := range p.roles {
			if role&4 != 0 && pathWithin(resource, nginxMoveKey(path)) {
				return true
			}
		}
	}
	return ok
}

func planWebConfigMove(ctx context.Context, root string, rebase dataPathRebase, overrides map[string]string) (*nginxMovePlan, error) {
	plan, err := planNginxMove(ctx, root, rebase, overrides)
	if err != nil {
		return nil, err
	}
	apache, err := planApacheMove(ctx, root, rebase, overrides)
	if err != nil {
		return nil, err
	}
	for path, role := range apache.roles {
		if previous := plan.roles[path]; previous != 0 && (previous&4 == 0 || role&4 == 0) {
			return nil, invalid("不同服务共用配置文件，无法确定一致的迁移语义：" + DisplayPath(path))
		}
		plan.roles[path] |= role
	}
	for path, body := range apache.edits {
		if previous, ok := plan.edits[path]; ok && previous != body {
			return nil, invalid("不同服务需要不同的配置路径转换，不能自动迁移：" + DisplayPath(path))
		}
		plan.edits[path] = body
	}
	for path := range plan.edits {
		for resource, role := range plan.roles {
			if role&4 != 0 && pathWithin(resource, nginxMoveKey(path)) {
				return nil, invalid("配置位于凭据资源目录内，不能自动改写：" + DisplayPath(path))
			}
		}
	}
	return plan, nil
}

var nginxMoveVariable = regexp.MustCompile(`\$\{?([A-Za-z_][A-Za-z_0-9]*)\}?`)

func planNginxMove(ctx context.Context, root string, rebase dataPathRebase, overrides map[string]string) (*nginxMovePlan, error) {
	plan := &nginxMovePlan{roles: map[string]uint8{}, edits: map[string]string{}}
	active := map[string]bool{}
	outputs := map[string]string{}
	definitions := map[string][]string{}
	dataValues := []string{}
	mapVariables := map[string]bool{}
	pathUses := []string{}
	total := 0
	toCopy := func(path string) string {
		path = (dataPathRebase{rebase.source, root}).path(path)
		return filepath.Clean((dataPathRebase{rebase.target, root}).path(path))
	}
	read := func(path string) (string, error) {
		for candidate, content := range overrides {
			if nginxMoveKey(candidate) == nginxMoveKey(path) {
				return content, nil
			}
		}
		return readConfigText(path)
	}
	containsOld := func(text string) bool {
		text = strings.ToLower(strings.ReplaceAll(text, "\\", "/"))
		return strings.Contains(text, strings.ToLower(strings.ReplaceAll(DisplayPath(rebase.source), "\\", "/")))
	}
	resolve := func(base, value string, glob bool) ([]string, error) {
		if strings.Contains(value, "$") {
			return nil, invalid("Nginx include 或资源路径包含动态变量，无法确定迁移目标：" + value)
		}
		path := filepath.FromSlash(value)
		if !filepath.IsAbs(path) {
			path = filepath.Join(base, path)
			if !pathWithin(root, path) {
				return nil, invalid("Nginx 相对引用超出数据目录，请改为明确的绝对路径：" + value)
			}
		}
		path = toCopy(path)
		paths := []string{path}
		if glob && strings.ContainsAny(path, "*?[") {
			// Go's bracket and escape matching is not identical to native nginx
			// glob on every supported OS. Do not guess which files nginx loads.
			if strings.ContainsAny(path, "[\\") && runtime.GOOS != "windows" || strings.Contains(path, "[") {
				return nil, invalid("Nginx include 使用当前无法安全展开的通配符，请改用 * 或明确文件名：" + value)
			}
			var err error
			paths, err = filepath.Glob(path)
			if err != nil {
				return nil, err
			}
		}
		for i, candidate := range paths {
			canonical, err := filepath.EvalSymlinks(candidate)
			if err != nil {
				return nil, err
			}
			if pathWithin(root, candidate) != pathWithin(root, canonical) || !pathWithin(root, candidate) && pathWithin(rebase.source, canonical) {
				return nil, invalid("Nginx 引用通过链接跨越数据目录，不能自动迁移：" + value)
			}
			paths[i] = canonical
		}
		return paths, nil
	}
	var visit func(string, string, bool, int) error
	visit = func(path, base string, data bool, depth int) error {
		if err := ctx.Err(); err != nil {
			return err
		}
		if depth > 64 || len(plan.roles) > 4096 {
			return invalid("Nginx include 层数或文件数超过安全限制")
		}
		key := nginxMoveKey(path)
		role := uint8(1)
		if data {
			role = 2
		}
		if plan.roles[key]&4 != 0 {
			return invalid("Nginx 同一文件同时用于配置与凭据资源：" + DisplayPath(path))
		}
		if active[key] {
			return invalid("Nginx include 存在循环：" + DisplayPath(path))
		}
		if plan.roles[key]&role != 0 {
			return nil
		}
		plan.roles[key] |= role
		active[key] = true
		defer delete(active, key)
		body, err := read(path)
		if err != nil {
			return err
		}
		if data {
			dataValues = append(dataValues, body)
		}
		total += len(body)
		if total > 64*1024*1024 {
			return invalid("Nginx include 总大小超过 64 MiB")
		}
		nodes, err := parseNginx(body)
		if err != nil {
			return err
		}
		next, err := rebaseConfigContent(rebase, path, body, data)
		if err != nil {
			return err
		}
		if previous, ok := outputs[key]; ok && previous != next {
			return invalid("Nginx 片段在不同上下文需要不同内容，不能自动迁移：" + DisplayPath(path))
		}
		outputs[key] = next
		if next != body {
			if !pathWithin(root, path) {
				return invalid("外部 Nginx 配置仍引用旧目录，请先调整：" + DisplayPath(path))
			}
			plan.edits[path] = next
		}
		var walk func([]nginxNode, bool) error
		walk = func(nodes []nginxNode, inData bool) error {
			for _, node := range nodes {
				if len(node.words) == 0 {
					continue
				}
				name := node.words[0]
				childData := inData || slices.Contains([]string{"map", "geo", "split_clients", "types"}, name)
				if !inData && len(node.words) > 1 {
					if movePathKey(name) || name == "fastcgi_param" {
						pathUses = append(pathUses, strings.Join(node.words[1:], " "))
					}
					if name == "set" || childData && name != "types" {
						variable := node.words[len(node.words)-1]
						if name == "set" {
							variable = node.words[1]
						}
						definitions[variable] = append(definitions[variable], body[node.start:node.end])
						if name != "set" {
							mapVariables[variable] = true
						}
					}
				}
				resource := !inData && slices.Contains([]string{"auth_basic_user_file", "ssl_certificate", "ssl_certificate_key", "ssl_client_certificate", "ssl_trusted_certificate", "ssl_crl", "ssl_dhparam", "ssl_password_file", "ssl_session_ticket_key", "proxy_ssl_certificate", "proxy_ssl_certificate_key", "proxy_ssl_trusted_certificate", "proxy_ssl_crl", "proxy_ssl_password_file", "grpc_ssl_certificate", "grpc_ssl_certificate_key", "grpc_ssl_trusted_certificate", "grpc_ssl_crl", "grpc_ssl_password_file"}, name)
				if len(node.words) == 2 && (name == "include" || resource) {
					value := node.words[1]
					if resource && (value == "off" || strings.HasPrefix(value, "data:") || strings.HasPrefix(value, "engine:") || strings.HasPrefix(value, "store:")) {
						continue
					}
					paths, err := resolve(base, value, !resource)
					if err != nil {
						return err
					}
					for _, included := range paths {
						if resource {
							k := nginxMoveKey(included)
							if plan.roles[k]&3 != 0 {
								return invalid("Nginx 同一文件同时用于配置与凭据资源：" + DisplayPath(included))
							}
							plan.roles[k] = 4
						} else if err = visit(included, base, inData, depth+1); err != nil {
							return err
						}
					}
				}
				if err := walk(node.children, childData); err != nil {
					return err
				}
			}
			return nil
		}
		return walk(nodes, data)
	}
	main := filepath.Join(root, "etc", "nginx", "nginx.conf")
	if _, err := os.Stat(main); err == nil {
		if err = visit(main, filepath.Dir(main), false, 0); err != nil {
			return nil, err
		}
	} else if !os.IsNotExist(err) {
		return nil, err
	}
	for _, pattern := range []string{"*.conf", "*.conf.disabled"} {
		sites, err := filepath.Glob(filepath.Join(root, "etc", "nginx", "sites", pattern))
		if err != nil {
			return nil, err
		}
		for _, site := range sites {
			if !plan.handled(site) {
				if err := visit(site, filepath.Dir(main), false, 0); err != nil {
					return nil, err
				}
			}
		}
	}
	// A backup can target an inactive standalone config. Active includes always
	// retain the context discovered from the main file, including arbitrary suffixes.
	for path := range overrides {
		if !plan.handled(path) && strings.Contains(filepath.ToSlash(path), "/etc/nginx/") {
			if err := visit(path, filepath.Dir(main), false, 0); err != nil {
				return nil, err
			}
		}
	}
	tainted := map[string]bool{}
	for variable := range mapVariables {
		// Included map rows may define dependencies in a different file. Treat
		// these conservatively when a map output is subsequently used as a path.
		definitions[variable] = append(definitions[variable], dataValues...)
	}
	for changed := true; changed; {
		changed = false
		for variable, values := range definitions {
			for _, value := range values {
				bad := containsOld(value)
				for _, match := range nginxMoveVariable.FindAllStringSubmatch(value, -1) {
					bad = bad || tainted["$"+match[1]]
				}
				if bad && !tainted[variable] {
					tainted[variable], changed = true, true
				}
			}
		}
	}
	for _, value := range pathUses {
		for _, match := range nginxMoveVariable.FindAllStringSubmatch(value, -1) {
			if tainted["$"+match[1]] {
				return nil, invalid("Nginx 路径变量间接引用旧目录，请先改成明确路径：$" + match[1])
			}
		}
	}
	return plan, nil
}
