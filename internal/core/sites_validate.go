package core

import (
	"context"
	"net/netip"
	"net/url"
	"path/filepath"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"unicode"
)

func configControl(text string) bool {
	return strings.IndexFunc(text, func(ch rune) bool { return unicode.IsControl(ch) || strings.ContainsRune("\"'\\$;{}<>", ch) }) >= 0
}
func siteHTTPURL(raw string, proxy bool) (*url.URL, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" || len(raw) > 8192 || configControl(raw) || strings.IndexFunc(raw, unicode.IsSpace) >= 0 {
		return nil, invalid("HTTP 地址包含配置控制字符或空白")
	}
	if proxy && !strings.Contains(raw, "://") {
		raw = "http://" + raw
	}
	u, err := url.Parse(raw)
	if err != nil || u.Hostname() == "" || u.Scheme != "http" && u.Scheme != "https" || u.User != nil || strings.Contains(u.Hostname(), "%") {
		return nil, invalid("请使用不含凭据的完整 HTTP/HTTPS 地址")
	}
	if port := u.Port(); port != "" {
		number, err := strconv.Atoi(port)
		if err != nil || number < 1 || number > 65535 {
			return nil, invalid("HTTP 端口必须为 1–65535")
		}
	}
	if proxy && (u.RawQuery != "" || u.ForceQuery || u.Fragment != "" || strings.Contains(raw, "#")) {
		return nil, invalid("代理目标不能包含查询参数或片段")
	}
	if u.Path == "" {
		u.Path = "/"
	}
	return u, nil
}
func siteProxyURL(raw string) (string, error) {
	u, err := siteHTTPURL(raw, true)
	if err != nil {
		return "", err
	}
	value := u.String()
	if !strings.HasSuffix(value, "/") {
		value += "/"
	}
	return value, nil
}
func domainMatches(host, domain string) bool {
	host = strings.ToLower(strings.TrimSuffix(host, "."))
	domain = strings.ToLower(strings.TrimSuffix(domain, "."))
	if strings.HasPrefix(domain, "*.") {
		return strings.HasSuffix(host, domain[1:])
	}
	return host == domain
}
func redirectURL(redirect *siteRedirect, domains []string) (string, error) {
	if redirect == nil || !slices.Contains([]int{301, 302, 307, 308}, redirect.Status) {
		return "", invalid("跳转状态仅支持 301、302、307、308")
	}
	u, err := siteHTTPURL(redirect.Target, false)
	if err != nil {
		return "", err
	}
	if redirect.PreservePath && (u.RawQuery != "" || u.ForceQuery || u.Fragment != "" || strings.Contains(redirect.Target, "#")) {
		return "", invalid("保留路径时，目标地址不能包含参数或片段")
	}
	for _, domain := range domains {
		if domainMatches(u.Hostname(), domain) {
			return "", &AppError{Code: "REDIRECT_LOOP", Message: "跳转目标不能使用本站域名"}
		}
	}
	return u.String(), nil
}
func normalizeSiteAccess(policy *siteAccess) error {
	if policy == nil {
		return nil
	}
	if policy.Mode != "allow" && policy.Mode != "deny" || len(policy.Addresses) < 1 || len(policy.Addresses) > 32 {
		return invalid("访问限制需选择允许/拒绝，并填写 1–32 个 IP 或网段")
	}
	out := []string{}
	for _, raw := range policy.Addresses {
		raw = strings.TrimSpace(raw)
		if len(raw) > 64 {
			return invalid("访问限制地址过长")
		}
		var value string
		if strings.Contains(raw, "/") {
			prefix, err := netip.ParsePrefix(raw)
			if err != nil {
				return invalid("访问限制网段无效")
			}
			if prefix.Addr().Is4In6() {
				bits := prefix.Bits() - 96
				if bits < 0 {
					return invalid("IPv4 映射网段前缀无效")
				}
				prefix = netip.PrefixFrom(prefix.Addr().Unmap(), bits)
			}
			value = prefix.Masked().String()
		} else {
			ip, err := netip.ParseAddr(raw)
			if err != nil || ip.Zone() != "" {
				return invalid("访问限制 IP 地址无效")
			}
			value = ip.Unmap().String()
		}
		if !slices.Contains(out, value) {
			out = append(out, value)
		}
	}
	policy.Addresses = out
	return nil
}

var corsMethods = []string{"GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"}
var corsHeaders = []string{"Access-Control-Allow-Origin", "Access-Control-Allow-Credentials", "Access-Control-Allow-Methods", "Access-Control-Allow-Headers", "Access-Control-Expose-Headers", "Access-Control-Max-Age"}

func normalizeSiteCORS(cors *siteCORS) error {
	if cors == nil {
		return nil
	}
	if len(cors.Origins) < 1 || len(cors.Origins) > 32 {
		return invalid("跨域来源需填写 1–32 项")
	}
	origins := []string{}
	for _, origin := range cors.Origins {
		value := strings.TrimSpace(origin)
		if value != "*" {
			if len(value) > 2048 {
				return invalid("跨域来源地址过长")
			}
			u, err := siteHTTPURL(value, false)
			if err != nil {
				return err
			}
			if u.Path != "/" || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" || strings.Contains(value, "#") {
				return invalid("跨域来源只能包含协议、主机和端口")
			}
			u.Scheme = strings.ToLower(u.Scheme)
			u.Host = strings.ToLower(u.Host)
			if u.Scheme == "https" && u.Port() == "443" || u.Scheme == "http" && u.Port() == "80" {
				u.Host = u.Hostname()
				if strings.Contains(u.Host, ":") {
					u.Host = "[" + u.Host + "]"
				}
			}
			value = u.Scheme + "://" + u.Host
		}
		if !slices.Contains(origins, value) {
			origins = append(origins, value)
		}
	}
	if slices.Contains(origins, "*") && (len(origins) != 1 || cors.Credentials) {
		return invalid("允许所有跨域来源时不能启用 Cookie 或混用其他来源")
	}
	if len(cors.Methods) < 1 || len(cors.Methods) > len(corsMethods) {
		return invalid("请选择有效的跨域请求方法")
	}
	for _, method := range cors.Methods {
		if !slices.Contains(corsMethods, method) {
			return invalid("跨域请求方法无效")
		}
	}
	methods := []string{}
	for _, method := range corsMethods {
		if slices.Contains(cors.Methods, method) {
			methods = append(methods, method)
		}
	}
	normalizeHeaders := func(values []string) ([]string, error) {
		if len(values) > 64 {
			return nil, invalid("跨域头名称最多 64 项")
		}
		out := []string{}
		seen := map[string]bool{}
		for _, value := range values {
			value = strings.TrimSpace(value)
			if !regexp.MustCompile(`^[A-Za-z0-9_-]{1,128}$`).MatchString(value) {
				return nil, invalid("跨域头名称只能包含字母、数字、短横线或下划线")
			}
			key := strings.ToLower(value)
			if !seen[key] {
				seen[key] = true
				out = append(out, value)
			}
		}
		return out, nil
	}
	allowed, err := normalizeHeaders(cors.AllowedHeaders)
	if err != nil {
		return err
	}
	exposed, err := normalizeHeaders(cors.ExposedHeaders)
	if err != nil {
		return err
	}
	if cors.MaxAge < 0 || cors.MaxAge > 86400 {
		return invalid("预检缓存时间需为 0–86400 秒")
	}
	cors.Origins = origins
	cors.Methods = methods
	cors.AllowedHeaders = allowed
	cors.ExposedHeaders = exposed
	return nil
}
func normalizeSiteProxy(rt *siteRuntime) error {
	if len(rt.ProxyRules) > 16 || rt.Kind == "redirect" && len(rt.ProxyRules) > 0 {
		return invalid("跳转站点不使用代理规则；其他站点最多 16 条")
	}
	seen := map[string]bool{}
	for i := range rt.ProxyRules {
		rule := &rt.ProxyRules[i]
		raw := strings.TrimSpace(rule.Path)
		path := strings.TrimRight(raw, "/")
		if path == "" || len(path) > 256 || !strings.HasPrefix(path, "/") || strings.Contains(raw, "//") || !regexp.MustCompile(`^/[A-Za-z0-9/._~-]+$`).MatchString(path) {
			return invalid("代理路径必须以 / 开头，不能为根路径、包含空白或配置字符")
		}
		for _, part := range strings.Split(path, "/") {
			if part == "." || part == ".." {
				return invalid("代理路径不能包含 . 或 ..")
			}
		}
		if seen[path] {
			return invalid("代理路径重复")
		}
		seen[path] = true
		target, err := siteProxyURL(rule.Target)
		if err != nil {
			return err
		}
		rule.Path = path
		rule.Target = target
	}
	if rt.CustomRewrite != nil && rt.CustomRewrite.Server == "nginx" && len(seen) > 0 {
		nodes, err := parseNginx(rt.CustomRewrite.Content)
		if err != nil {
			return err
		}
		var conflicts func([]nginxNode) bool
		conflicts = func(nodes []nginxNode) bool {
			for _, node := range nodes {
				if len(node.words) > 1 && node.words[0] == "location" && seen[strings.TrimRight(node.words[len(node.words)-1], "/")] {
					return true
				}
				if conflicts(node.children) {
					return true
				}
			}
			return false
		}
		if conflicts(nodes) {
			return invalid("路径代理与自定义 location 重复")
		}
	}
	return nil
}
func normalizeSiteErrors(pages map[int]string) error {
	allowed := []int{400, 401, 403, 404, 405, 408, 429, 500, 502, 503, 504}
	if len(pages) > len(allowed) {
		return invalid("错误页最多设置 11 项")
	}
	for code, raw := range pages {
		path := strings.TrimSpace(raw)
		lower := strings.ToLower(path)
		if !slices.Contains(allowed, code) || len(path) > 512 || path == "/" || !strings.HasPrefix(path, "/") || strings.Contains(path, "//") || !regexp.MustCompile(`^/[A-Za-z0-9/._~!$&'()*+,;=:@%\-]+$`).MatchString(path) {
			return invalid("自定义错误页路径无效")
		}
		if strings.Contains(lower, "%2e") || strings.Contains(lower, "%2f") || strings.Contains(lower, "%5c") {
			return invalid("错误页路径不能包含编码的路径分隔符")
		}
		for _, part := range strings.Split(path, "/") {
			if part == "." || part == ".." {
				return invalid("错误页路径不能越过站点目录")
			}
		}
		if _, err := url.PathUnescape(path); err != nil {
			return invalid("错误页百分号编码无效")
		}
		pages[code] = path
	}
	return nil
}
func validateSiteRewrite(rewrite *siteRewrite, server string) error {
	if rewrite == nil {
		return nil
	}
	if strings.TrimSpace(rewrite.Name) == "" || len(rewrite.Name) > 240 || strings.IndexFunc(rewrite.Name, unicode.IsControl) >= 0 || rewrite.Server != server || strings.TrimSpace(rewrite.Content) == "" || len(rewrite.Content) > 65536 || strings.ContainsRune(rewrite.Content, 0) {
		return invalid("伪静态模板名称、服务器或内容无效")
	}
	if server == "nginx" {
		_, err := parseNginx(rewrite.Content)
		return err
	}
	if server == "apache" {
		for _, line := range strings.Split(rewrite.Content, "\n") {
			line = strings.TrimSpace(line)
			if line == "" || strings.HasPrefix(line, "#") {
				continue
			}
			directive := strings.ToLower(strings.Fields(line)[0])
			if !slices.Contains([]string{"rewriteengine", "rewritecond", "rewriterule", "rewritebase"}, directive) {
				return invalid("Apache 模板仅支持 Rewrite 指令及注释")
			}
		}
	}
	if server == "caddy" {
		return validateCaddyRewrite(rewrite.Content)
	}
	return nil
}
func (c *Core) validateSite(ctx context.Context, site *Site) error {
	if site.ID != "" && !siteIdentifier.MatchString(site.ID) {
		return invalid("站点标识无效")
	}
	site.Name = strings.TrimSpace(site.Name)
	if site.Name == "" || len(site.Name) > 512 || strings.IndexFunc(site.Name, unicode.IsControl) >= 0 {
		return invalid("站点名称不能为空或包含控制字符")
	}
	if len(site.Domains) < 1 || len(site.Domains) > 100 {
		return invalid("请填写 1–100 个站点域名")
	}
	domains := []string{}
	for _, domain := range site.Domains {
		domain = strings.ToLower(strings.TrimSpace(domain))
		host := strings.TrimPrefix(domain, "*.")
		if ip, err := netip.ParseAddr(host); err == nil && ip.Is4() && host == domain || domain == "localhost" {
		} else {
			if len(host) > 253 || !strings.Contains(host, ".") || regexp.MustCompile(`^[0-9.]+$`).MatchString(host) {
				return invalid("站点域名格式无效：" + domain)
			}
			for _, label := range strings.Split(host, ".") {
				if len(label) < 1 || len(label) > 63 || strings.HasPrefix(label, "-") || strings.HasSuffix(label, "-") || !regexp.MustCompile(`^[a-z0-9-]+$`).MatchString(label) {
					return invalid("站点域名格式无效：" + domain)
				}
			}
		}
		if !slices.Contains(domains, domain) {
			domains = append(domains, domain)
		}
	}
	site.Domains = domains
	rt := &site.Runtime
	if rt.WebServer == "" {
		rt.WebServer = "nginx"
	}
	if !slices.Contains([]string{"nginx", "apache", "caddy"}, rt.WebServer) {
		return invalid("请选择 Nginx、Apache 或 Caddy")
	}
	if !slices.Contains([]string{"php", "static", "reverse-proxy", "redirect", "node", "python", "java", "go"}, rt.Kind) {
		return invalid("站点运行类型无效")
	}
	if rt.Kind == "redirect" {
		site.RootDir = ""
	} else {
		root := strings.TrimSpace(string(site.RootDir))
		if !filepath.IsAbs(root) || strings.IndexFunc(root, func(ch rune) bool { return unicode.IsControl(ch) || strings.ContainsRune("\"$;{}", ch) }) >= 0 {
			return invalid("站点目录必须为不含配置控制字符的绝对路径")
		}
		site.RootDir = Path(DisplayPath(root))
	}
	if rt.HTTPSRedirect != nil && (!site.HTTPS || !slices.Contains([]int{307, 308}, *rt.HTTPSRedirect)) {
		return invalid("HTTPS 跳转需先开启 HTTPS，并选择 307 或 308")
	}
	if err := normalizeSiteAccess(rt.Access); err != nil {
		return err
	}
	if err := normalizeSiteCORS(rt.CORS); err != nil {
		return err
	}
	if err := normalizeSiteErrors(rt.ErrorPages); err != nil {
		return err
	}
	if rt.Kind == "redirect" && len(rt.ErrorPages) > 0 {
		return invalid("跳转站点不使用自定义错误页")
	}
	if err := validateSiteRewrite(rt.CustomRewrite, rt.WebServer); err != nil {
		return err
	}
	if rt.CustomRewrite != nil && rt.Kind != "php" && rt.Kind != "static" {
		return invalid("自定义伪静态仅适用于 PHP 和静态站点")
	}
	if err := normalizeSiteProxy(rt); err != nil {
		return err
	}
	if auth := rt.BasicAuth; auth != nil {
		auth.Username = strings.TrimSpace(auth.Username)
		if auth.Enabled && (auth.Username == "" || len(auth.Username) > 128 || strings.IndexFunc(auth.Username, func(ch rune) bool { return unicode.IsControl(ch) || unicode.IsSpace(ch) || ch == ':' }) >= 0) {
			return invalid("Basic Auth 用户名无效")
		}
		if auth.Password != nil && (len(*auth.Password) < 8 || len(*auth.Password) > 72 || strings.IndexFunc(*auth.Password, unicode.IsControl) >= 0) {
			return invalid("Basic Auth 密码必须为 8–72 字节且不能包含控制字符")
		}
	}
	if rt.ImportedCertID != nil && rt.ACMECertID != nil {
		return invalid("每个站点只能选择一种证书来源")
	}
	if rt.Kind == "php" {
		if rt.PHPVersion == nil || !regexp.MustCompile(`^[0-9]+(?:\.[0-9]+){1,2}$`).MatchString(*rt.PHPVersion) {
			return invalid("请选择 PHP 版本")
		}
	}
	if rt.Kind != "php" && rt.Kind != "static" && rt.Kind != "redirect" {
		if rt.ProxyTarget == nil {
			return invalid("请填写代理目标")
		}
		target, err := siteProxyURL(*rt.ProxyTarget)
		if err != nil {
			return err
		}
		rt.ProxyTarget = &target
	}
	if app := rt.Application; app != nil {
		id := applicationRuntimeID(rt.Kind)
		if id == "" || !safePart(app.Version) {
			return invalid("请选择 Node.js、Python、Java 或 Go 运行时版本")
		}
		if len(app.Args) < 1 || len(app.Args) > 64 || strings.TrimSpace(app.Args[0]) == "" {
			return invalid("应用入口和参数需为 1–64 项")
		}
		total := 0
		for _, arg := range app.Args {
			total += len(arg)
			if len(arg) > 8192 || strings.ContainsRune(arg, 0) {
				return invalid("应用参数包含空字符或超过限制")
			}
		}
		if total > 32768 {
			return invalid("应用参数总长度超过 32 KiB")
		}
		if app.CWD != nil && (len(*app.CWD) > 4096 || strings.IndexFunc(string(*app.CWD), unicode.IsControl) >= 0) {
			return invalid("应用工作目录无效")
		}
		u, err := siteHTTPURL(*rt.ProxyTarget, true)
		if err != nil {
			return err
		}
		ip, err := netip.ParseAddr(u.Hostname())
		if err != nil || !ip.IsLoopback() || u.Scheme != "http" || u.Path != "/" {
			return invalid("托管应用必须使用本机 HTTP 地址且不含路径")
		}
		packages, err := c.installedRuntimePackages(ctx)
		if err != nil {
			return err
		}
		if err = validateRuntimeVersions(map[string]string{id: app.Version}, packages); err != nil {
			return err
		}
	}
	if site.Rewrite == "" {
		site.Rewrite = "none"
	}
	if !slices.Contains([]string{"none", "laravel", "thinkphp", "wordpress", "spa-fallback", "next-export", "symfony", "yii2", "codeigniter", "cakephp", "drupal", "joomla"}, site.Rewrite) {
		return invalid("伪静态预设无效")
	}
	sites, err := c.sites(ctx)
	if err != nil {
		return err
	}
	for _, other := range sites {
		if other.ID == site.ID {
			continue
		}
		for _, old := range other.Domains {
			for _, new := range domains {
				if strings.EqualFold(old, new) {
					return &AppError{Code: "DOMAIN_CONFLICT", Message: "域名已被站点「" + other.Name + "」使用"}
				}
			}
		}
	}
	if rt.Kind == "redirect" {
		target, err := redirectURL(rt.Redirect, domains)
		if err != nil {
			return err
		}
		rt.Redirect.Target = target
		nodes := []Site{}
		for _, other := range sites {
			if other.ID != site.ID && other.Runtime.Kind == "redirect" && other.Runtime.Redirect != nil {
				nodes = append(nodes, other)
			}
		}
		nodes = append(nodes, *site)
		completed := map[int]bool{}
		var visit func(string, map[int]bool) bool
		visit = func(host string, path map[int]bool) bool {
			for index, node := range nodes {
				if completed[index] {
					continue
				}
				matches := false
				for _, domain := range node.Domains {
					matches = matches || domainMatches(host, domain)
				}
				if !matches {
					continue
				}
				if path[index] {
					return true
				}
				u, err := siteHTTPURL(node.Runtime.Redirect.Target, false)
				if err != nil {
					continue
				}
				path[index] = true
				if visit(u.Hostname(), path) {
					return true
				}
				delete(path, index)
				completed[index] = true
			}
			return false
		}
		u, _ := url.Parse(target)
		if visit(u.Hostname(), map[int]bool{len(nodes) - 1: true}) {
			return &AppError{Code: "REDIRECT_CHAIN_LOOP", Message: "目标地址会进入站点跳转循环"}
		}
	}
	return nil
}
