package core

import (
	"crypto/sha256"
	"fmt"
	"net/url"
	"path/filepath"
	"regexp"
	"slices"
	"sort"
	"strconv"
	"strings"
)

type siteConfigOptions struct {
	http, https, php                 int
	certificate, key, auth, authHash string
	network                          bool
}

func siteVariable(prefix, id string) string {
	sum := sha256.Sum256([]byte(id))
	return fmt.Sprintf("nsb_%s_%x", prefix, sum[:8])
}

func nginxSiteAccess(site Site) (string, string) {
	policy := site.Runtime.Access
	if policy == nil {
		return "", ""
	}
	variable := siteVariable("access", site.ID)
	defaultValue, match := 0, 1
	if policy.Mode == "allow" {
		defaultValue, match = 1, 0
	}
	out := fmt.Sprintf("geo $%s {\n    default %d;\n", variable, defaultValue)
	for _, ip := range policy.Addresses {
		out += fmt.Sprintf("    %s %d;\n", ip, match)
	}
	return out + "}\n", fmt.Sprintf("    if ($%s) { return 403; }\n", variable)
}

func nginxSiteCORS(site Site) (maps, headers, preflight string) {
	cors := site.Runtime.CORS
	if cors == nil {
		return
	}
	prefix := siteVariable("cors", site.ID)
	maps = fmt.Sprintf("map $http_origin $%s_origin {\n    default \"\";\n", prefix)
	if slices.Equal(cors.Origins, []string{"*"}) {
		maps = fmt.Sprintf("map $http_origin $%s_origin {\n    default \"*\";\n    \"\" \"\";\n", prefix)
	} else {
		for _, origin := range cors.Origins {
			maps += fmt.Sprintf("    %s $http_origin;\n", nginxQuote(origin))
		}
	}
	maps += "}\n"
	credentials := ""
	if cors.Credentials {
		credentials = "true"
	}
	keys := []string{"origin", "credentials", "methods", "allowed", "exposed", "age"}
	values := []string{credentials, strings.Join(cors.Methods, ", "), strings.Join(cors.AllowedHeaders, ", "), strings.Join(cors.ExposedHeaders, ", "), strconv.Itoa(cors.MaxAge)}
	for i, value := range values {
		maps += fmt.Sprintf("map $%s_origin $%s_%s {\n    \"\" \"\";\n    default %s;\n}\n", prefix, prefix, keys[i+1], nginxQuote(value))
	}
	maps += fmt.Sprintf("map \"$request_method:$%s_origin:$http_access_control_request_method\" $%s_preflight {\n    default 0;\n    \"~^OPTIONS:.+:(?:%s)$\" 1;\n    \"~^OPTIONS:.*:.+$\" 2;\n}\n", prefix, prefix, strings.Join(cors.Methods, "|"))
	for i, header := range corsHeaders {
		headers += fmt.Sprintf("    add_header %s $%s_%s always;\n", header, prefix, keys[i])
	}
	headers += "    add_header Vary \"Origin\" always;\n"
	preflight = fmt.Sprintf("    if ($%s_preflight = 2) { return 403; }\n    if ($%s_preflight = 1) { return 204; }\n", prefix, prefix)
	for _, header := range corsHeaders {
		preflight += fmt.Sprintf("    proxy_hide_header %s;\n    fastcgi_hide_header %s;\n", header, header)
	}
	return
}

func nginxSiteRewrite(preset, kind string) string {
	fallback := "/index.php?$query_string"
	before, after := "", ""
	switch preset {
	case "none":
		if kind == "static" {
			fallback = "=404"
		}
	case "spa-fallback":
		fallback = "/index.html"
	case "next-export":
		return "    location / { try_files $uri $uri.html $uri/ =404; }\n    error_page 404 /404.html;\n    location = /404.html { internal; }\n"
	case "thinkphp":
		return "    location / { if (!-e $request_filename) { rewrite ^(.*)$ /index.php?s=$1 last; } }\n"
	case "wordpress":
		after = "    rewrite /wp-admin$ $scheme://$host$uri/ permanent;\n"
	case "symfony", "codeigniter":
		fallback = "/index.php$is_args$args"
	case "cakephp":
		fallback = "/index.php?url=$uri&$args"
	case "yii2", "joomla":
		fallback = "/index.php?$args"
	}
	if preset == "codeigniter" {
		before = "    location ~* ^/(app|system|writable)/ { deny all; }\n"
	}
	if preset == "drupal" {
		before = "    location ~* \\.(engine|inc|info|install|module|profile|po|sh|.*sql|theme|tpl(\\.php)?|xtmpl)$ { deny all; }\n"
	}
	return before + "    location / { try_files $uri $uri/ " + fallback + "; }\n" + after
}

const nginxProxyHeaders = `        proxy_ssl_server_name on;
        proxy_http_version 1.1;
        proxy_set_header Host $proxy_host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_read_timeout 300s;
`

func orderedSiteProxies(rt siteRuntime) []siteProxyRule {
	rules := append([]siteProxyRule{}, rt.ProxyRules...)
	sort.Slice(rules, func(i, j int) bool {
		if len(rules[i].Path) != len(rules[j].Path) {
			return len(rules[i].Path) > len(rules[j].Path)
		}
		return rules[i].Path < rules[j].Path
	})
	return rules
}
func siteProxyDestination(rule siteProxyRule) string {
	if rule.StripPrefix {
		return rule.Target
	}
	return rule.Target + strings.TrimPrefix(rule.Path, "/") + "/"
}
func nginxSiteProxies(rt siteRuntime) string {
	var out strings.Builder
	for _, rule := range orderedSiteProxies(rt) {
		target := siteProxyDestination(rule)
		u, _ := url.Parse(target)
		cookiePath := strings.TrimRight(u.Path, "/")
		pattern := "^" + regexp.QuoteMeta(cookiePath) + "(/.*)?$"
		if cookiePath == "" {
			pattern = "^(/.*)$"
		}
		for _, location := range []string{"= " + rule.Path, "^~ " + rule.Path + "/"} {
			fmt.Fprintf(&out, "    location %s {\n        proxy_pass %s;\n        proxy_intercept_errors on;\n        proxy_redirect %s %s/;\n        proxy_cookie_path \"~%s\" \"%s$1\";\n%s    }\n", location, target, target, rule.Path, strings.ReplaceAll(pattern, `\`, `\\`), rule.Path, nginxProxyHeaders)
		}
	}
	return out.String()
}

// Child add_header/hide_header directives replace inherited settings in Nginx.
// Preserve custom unrelated headers, while keeping the selected CORS policy.
func nginxSiteCORSChildren(body, headers string) (string, error) {
	nodes, err := parseNginx(body)
	if err != nil {
		return "", err
	}
	type edit struct {
		start, end int
		value      string
	}
	edits := []edit{}
	var visit func([]nginxNode)
	visit = func(nodes []nginxNode) {
		for _, node := range nodes {
			if len(node.words) > 1 && slices.Contains([]string{"add_header", "proxy_pass_header", "fastcgi_pass_header"}, node.words[0]) && slices.ContainsFunc(corsHeaders, func(header string) bool { return strings.EqualFold(header, node.words[1]) }) {
				edits = append(edits, edit{node.start, node.end, ""})
			}
			if node.open >= 0 {
				insert := ""
				for _, directive := range []string{"add_header", "proxy_hide_header", "fastcgi_hide_header"} {
					if slices.ContainsFunc(node.children, func(child nginxNode) bool { return len(child.words) > 0 && child.words[0] == directive }) {
						if directive == "add_header" {
							insert += headers
						} else {
							for _, header := range corsHeaders {
								insert += fmt.Sprintf("    %s %s;\n", directive, header)
							}
						}
					}
				}
				if insert != "" {
					edits = append(edits, edit{node.end - 1, node.end - 1, "\n" + insert})
				}
			}
			visit(node.children)
		}
	}
	visit(nodes)
	sort.SliceStable(edits, func(i, j int) bool { return edits[i].start > edits[j].start })
	for _, e := range edits {
		body = body[:e.start] + e.value + body[e.end:]
	}
	return body, nil
}

func siteHTTPSHostPattern(site Site) string {
	parts := []string{}
	for _, domain := range site.Domains {
		if strings.HasPrefix(domain, "*.") {
			parts = append(parts, `(?:[^.:]+\.)+`+regexp.QuoteMeta(domain[2:]))
		} else {
			parts = append(parts, regexp.QuoteMeta(domain))
		}
	}
	return strings.Join(parts, "|")
}
func siteHTTPSPortSuffix(port int) string {
	if port == 443 {
		return ""
	}
	return ":" + strconv.Itoa(port)
}

func (c *Core) renderNginxSite(site Site, options siteConfigOptions) (string, error) {
	listen := func(port int, secure bool) string {
		address := "127.0.0.1:"
		if options.network {
			address = "0.0.0.0:"
		}
		flag := ""
		if secure {
			flag = " ssl"
		}
		return fmt.Sprintf("    listen %s%d%s;\n", address, port, flag)
	}
	maps, gate := nginxSiteAccess(site)
	corsMaps, corsHeaders, preflight := nginxSiteCORS(site)
	var out strings.Builder
	fmt.Fprintf(&out, "# NiceEnv site: %s\n%s%s", site.ID, maps, corsMaps)
	logs := fmt.Sprintf("    access_log %s;\n    error_log %s warn;\n", nginxQuote(filepath.Join(c.Home, "logs", "nginx", site.ID+".access.log")), nginxQuote(filepath.Join(c.Home, "logs", "nginx", site.ID+".error.log")))
	names := "    server_name " + strings.Join(site.Domains, " ") + ";\n"
	if site.HTTPS && site.Runtime.HTTPSRedirect != nil {
		fmt.Fprintf(&out, "server {\n%s%s%s%s    if ($host !~* \"^(?:%s)$\") { return 421; }\n    return %d \"https://$host%s$request_uri\";\n}\n", listen(options.http, false), names, logs, gate, strings.ReplaceAll(siteHTTPSHostPattern(site), `\`, `\\`), *site.Runtime.HTTPSRedirect, siteHTTPSPortSuffix(options.https))
	}
	out.WriteString("server {\n")
	if !site.HTTPS || site.Runtime.HTTPSRedirect == nil {
		out.WriteString(listen(options.http, false))
	}
	if site.HTTPS {
		out.WriteString(listen(options.https, true))
		fmt.Fprintf(&out, "    ssl_certificate %s;\n    ssl_certificate_key %s;\n", nginxQuote(options.certificate), nginxQuote(options.key))
	}
	out.WriteString(names + logs)
	if site.Runtime.Kind != "redirect" {
		index := "index.html index.htm"
		if site.Runtime.Kind == "php" {
			index = "index.php " + index
		}
		fmt.Fprintf(&out, "    root %s;\n    index %s;\n", nginxQuote(string(site.RootDir)), index)
	}
	out.WriteString("    charset utf-8;\n")
	if options.auth != "" {
		fmt.Fprintf(&out, "    auth_basic \"Restricted\";\n    auth_basic_user_file %s;\n", nginxQuote(options.auth))
	}
	codes := []int{}
	for code := range site.Runtime.ErrorPages {
		codes = append(codes, code)
	}
	sort.Ints(codes)
	for _, code := range codes {
		fmt.Fprintf(&out, "    error_page %d %s;\n", code, nginxQuote(site.Runtime.ErrorPages[code]))
	}
	out.WriteString(corsHeaders + gate + preflight)
	body := "    location ~ /\\.(?!well-known(?:/|$)) { deny all; }\n" + nginxSiteProxies(site.Runtime)
	switch site.Runtime.Kind {
	case "redirect":
		target, err := redirectURL(site.Runtime.Redirect, site.Domains)
		if err != nil {
			return "", err
		}
		if site.Runtime.Redirect.PreservePath {
			target = strings.TrimRight(target, "/") + "$request_uri"
		}
		body += fmt.Sprintf("    return %d \"%s\";\n", site.Runtime.Redirect.Status, target)
	case "php", "static":
		if custom := site.Runtime.CustomRewrite; custom != nil {
			body += custom.Content + "\n"
		} else {
			body += nginxSiteRewrite(site.Rewrite, site.Runtime.Kind)
		}
		if site.Runtime.Kind == "php" {
			if site.Runtime.PHPVersion == nil {
				return "", invalid("未选择 PHP 版本")
			}
			body += fmt.Sprintf("    location ~ \\.php$ {\n        try_files $uri =404;\n        fastcgi_pass nsb_php_%s;\n        include %s;\n    }\n", strings.ReplaceAll(*site.Runtime.PHPVersion, ".", "_"), nginxQuote(filepath.Join(c.Home, "etc", "nginx", "fastcgi_params")))
		} else {
			body += "    location ~* \\.(?:php[0-9]*|phtml|pht|phar)(?:[./]|$) { deny all; }\n"
		}
	default:
		if site.Runtime.ProxyTarget == nil {
			return "", invalid("未填写代理目标")
		}
		body += fmt.Sprintf("    location / {\n        proxy_pass %s;\n        proxy_intercept_errors on;\n%s    }\n", *site.Runtime.ProxyTarget, nginxProxyHeaders)
	}
	if site.Runtime.CORS != nil {
		var err error
		body, err = nginxSiteCORSChildren(body, corsHeaders)
		if err != nil {
			return "", err
		}
	}
	out.WriteString(body + "}\n")
	if _, err := parseNginx(out.String()); err != nil {
		return "", err
	}
	return out.String(), nil
}
