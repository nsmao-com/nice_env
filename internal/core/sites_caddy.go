package core

import (
	"fmt"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"sort"
	"strconv"
	"strings"
)

// Caddyfile quoted tokens only unescape quotes, unlike JSON strings. JSON
// encoding doubles regexp backslashes and changes literal URL characters.
func caddyQuote(value string) string { return `"` + strings.ReplaceAll(value, `"`, `\"`) + `"` }
func caddyPath(value string) string  { return caddyQuote(filepath.ToSlash(DisplayPath(value))) }
func caddySiteAccess(policy *siteAccess) string {
	if policy == nil {
		return ""
	}
	negate := ""
	if policy.Mode == "allow" {
		negate = "not "
	}
	return "        @nsb_access_denied " + negate + "remote_ip " + strings.Join(policy.Addresses, " ") + "\n        respond @nsb_access_denied 403\n"
}
func caddySiteCORS(cors *siteCORS) string {
	if cors == nil {
		return ""
	}
	origins := []string{}
	for _, origin := range cors.Origins {
		origins = append(origins, regexp.QuoteMeta(origin))
	}
	pattern := strings.Join(origins, "|")
	if slices.Equal(cors.Origins, []string{"*"}) {
		pattern = ".+"
	}
	pattern = caddyQuote("^(?:" + pattern + ")$")
	var out strings.Builder
	fmt.Fprintf(&out, "    @cors_origin header_regexp Origin %s\n    @cors_other not header_regexp Origin %s\n    @cors_preflight {\n        method OPTIONS\n        header_regexp Origin %s\n        header_regexp Access-Control-Request-Method %s\n    }\n    @cors_denied {\n        method OPTIONS\n        header Access-Control-Request-Method *\n    }\n    route {\n        header {\n            defer\n            +Vary Origin\n        }\n        header @cors_other {\n            defer\n", pattern, pattern, pattern, caddyQuote("^(?:"+strings.Join(cors.Methods, "|")+")$"))
	for _, header := range corsHeaders {
		fmt.Fprintf(&out, "            -%s\n", header)
	}
	out.WriteString("        }\n        header @cors_origin {\n            defer\n")
	origin := "{http.request.header.Origin}"
	if slices.Equal(cors.Origins, []string{"*"}) {
		origin = "*"
	}
	credentials := ""
	if cors.Credentials {
		credentials = "true"
	}
	values := []string{origin, credentials, strings.Join(cors.Methods, ", "), strings.Join(cors.AllowedHeaders, ", "), strings.Join(cors.ExposedHeaders, ", "), strconv.Itoa(cors.MaxAge)}
	for i, value := range values {
		if value == "" {
			fmt.Fprintf(&out, "            -%s\n", corsHeaders[i])
		} else {
			fmt.Fprintf(&out, "            %s %s\n", corsHeaders[i], caddyQuote(value))
		}
	}
	out.WriteString("        }\n        respond @cors_preflight 204\n        respond @cors_denied 403\n    }\n")
	return out.String()
}
func (c *Core) caddySiteProxy(target string, rule *siteProxyRule, pages map[int]string, prefix string) (string, error) {
	u, err := url.Parse(target)
	if err != nil {
		return "", err
	}
	origin := u.Scheme + "://" + u.Host
	base := strings.TrimRight(u.Path, "/")
	var out strings.Builder
	out.WriteString("            route {\n")
	if rule != nil && rule.StripPrefix {
		fmt.Fprintf(&out, "                uri strip_prefix %s\n", caddyQuote(rule.Path))
	}
	if base != "" {
		fmt.Fprintf(&out, "                rewrite * %s\n", caddyQuote(base+"{uri}"))
	}
	fmt.Fprintf(&out, "                reverse_proxy %s {\n                    header_up Host {upstream_hostport}\n", caddyQuote(origin))
	ip, _ := netip.ParseAddr(u.Hostname())
	if u.Scheme == "https" && (u.Hostname() == "localhost" || ip.IsLoopback()) {
		path := filepath.Join(c.Home, "certs", "ca.crt")
		if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() {
			fmt.Fprintf(&out, "                    transport http {\n                        tls_trust_pool file {\n                            pem_file %s\n                        }\n                    }\n", caddyPath(path))
		}
	}
	if rule != nil {
		upstream := base
		if !rule.StripPrefix {
			upstream += rule.Path
		}
		fmt.Fprintf(&out, "                    header_down Location %s %s\n", caddyQuote("^"+regexp.QuoteMeta(origin+upstream)+"(/.*)?$"), caddyQuote(rule.Path+"${1}"))
		cookie := `(?i)(;[ \t]*path=)` + regexp.QuoteMeta(upstream) + `(/[^;]*)?(;|$)`
		if upstream == "" {
			cookie = `(?i)(;[ \t]*path=)(/[^;]*)(;|$)`
		}
		fmt.Fprintf(&out, "                    header_down Set-Cookie %s %s\n", caddyQuote(cookie), caddyQuote("${1}"+rule.Path+"${2}${3}"))
	}
	codes := []int{}
	for code := range pages {
		codes = append(codes, code)
	}
	sort.Ints(codes)
	for _, code := range codes {
		fmt.Fprintf(&out, "                    @%s_error_%d status %d\n                    handle_response @%s_error_%d {\n                        rewrite * %s\n                        file_server {\n                            status %d\n                        }\n                    }\n", prefix, code, code, prefix, code, caddyQuote(pages[code]), code)
	}
	out.WriteString("                }\n            }\n")
	return out.String(), nil
}
func (c *Core) renderCaddySite(site Site, options siteConfigOptions) (string, error) {
	block := func(secure bool) (string, error) {
		scheme := "http"
		port := options.http
		if secure {
			scheme = "https"
			port = options.https
		}
		addresses := []string{}
		for _, domain := range site.Domains {
			addresses = append(addresses, fmt.Sprintf("%s://%s:%d", scheme, domain, port))
		}
		var out strings.Builder
		fmt.Fprintf(&out, "# NiceEnv site: %s\n%s {\n", site.ID, strings.Join(addresses, ", "))
		bind := "127.0.0.1"
		if options.network {
			bind = "0.0.0.0"
		}
		fmt.Fprintf(&out, "    bind %s\n", bind)
		if secure {
			fmt.Fprintf(&out, "    tls %s %s\n", caddyPath(options.certificate), caddyPath(options.key))
		}
		fmt.Fprintf(&out, "    log niceenv_%s_%s {\n        output file %s\n    }\n", site.ID, scheme, caddyPath(filepath.Join(c.Home, "logs", "caddy", site.ID+".access.log")))
		if !secure && site.HTTPS && site.Runtime.HTTPSRedirect != nil {
			fmt.Fprintf(&out, "    route {\n%s        redir %s %d\n    }\n}\n", caddySiteAccess(site.Runtime.Access), caddyQuote("https://{host}"+siteHTTPSPortSuffix(options.https)+"{uri}"), *site.Runtime.HTTPSRedirect)
			return out.String(), nil
		}
		if site.Runtime.Kind != "redirect" {
			fmt.Fprintf(&out, "    root * %s\n", caddyPath(string(site.RootDir)))
		}
		out.WriteString("    route {\n" + caddySiteAccess(site.Runtime.Access) + caddySiteCORS(site.Runtime.CORS))
		if auth := site.Runtime.BasicAuth; auth != nil && auth.Enabled {
			fmt.Fprintf(&out, "        basic_auth {\n            %s %s\n        }\n", caddyQuote(auth.Username), options.authHash)
		}
		out.WriteString("        @private {\n            path_regexp private (^|/)\\.[^/]+\n            not path /.well-known/*\n        }\n        @nested_private path_regexp nested_private ^/\\.well-known/(.*/)?\\.[^/]+\n        respond @private 403\n        respond @nested_private 403\n")
		for i, rule := range orderedSiteProxies(site.Runtime) {
			fmt.Fprintf(&out, "        @proxy_%d path %s %s\n        handle @proxy_%d {\n", i, caddyQuote(rule.Path), caddyQuote(rule.Path+"/*"), i)
			proxy, err := c.caddySiteProxy(rule.Target, &rule, site.Runtime.ErrorPages, fmt.Sprintf("niceenv_proxy_%d", i))
			if err != nil {
				return "", err
			}
			out.WriteString(proxy + "        }\n")
		}
		out.WriteString("        handle {\n")
		switch site.Runtime.Kind {
		case "redirect":
			target, err := redirectURL(site.Runtime.Redirect, site.Domains)
			if err != nil {
				return "", err
			}
			if site.Runtime.Redirect.PreservePath {
				target = strings.TrimRight(target, "/") + "{uri}"
			}
			fmt.Fprintf(&out, "            redir %s %d\n", caddyQuote(target), site.Runtime.Redirect.Status)
		case "static", "php":
			if custom := site.Runtime.CustomRewrite; custom != nil {
				if err := validateCaddyRewrite(custom.Content); err != nil {
					return "", err
				}
				out.WriteString(custom.Content + "\n")
			} else {
				switch site.Rewrite {
				case "spa-fallback":
					out.WriteString("            try_files {path} {path}/ /index.html\n")
				case "next-export":
					out.WriteString("            try_files {path} {path}.html {path}/\n")
				case "thinkphp", "cakephp":
					key := "s"
					if site.Rewrite == "cakephp" {
						key = "url"
					}
					fmt.Fprintf(&out, "            @missing not file\n            rewrite @missing /index.php?%s={path}&{query}\n", key)
				case "wordpress":
					out.WriteString("            redir /wp-admin /wp-admin/ 301\n")
				case "codeigniter":
					out.WriteString("            @framework_private path /app/* /system/* /writable/*\n            respond @framework_private 403\n")
				case "drupal":
					out.WriteString("            @framework_private path_regexp framework (?i)\\.(engine|inc|info|install|module|profile|po|sh|.*sql|theme|tpl(\\.php)?|xtmpl)$\n            respond @framework_private 403\n")
				}
			}
			if site.Runtime.Kind == "php" {
				if options.php < 1 {
					return "", invalid("PHP 池端口尚未分配")
				}
				fmt.Fprintf(&out, "            php_fastcgi 127.0.0.1:%d {\n                capture_stderr\n            }\n", options.php)
			} else {
				out.WriteString("            @php_source path_regexp php_source (?i)\\.(php[0-9]*|phtml|pht|phar)([./]|$)\n            respond @php_source 403\n")
			}
			out.WriteString("            file_server\n")
		default:
			if site.Runtime.ProxyTarget == nil {
				return "", invalid("代理目标未设置")
			}
			proxy, err := c.caddySiteProxy(*site.Runtime.ProxyTarget, nil, site.Runtime.ErrorPages, "niceenv_main")
			if err != nil {
				return "", err
			}
			out.WriteString(proxy)
		}
		out.WriteString("        }\n    }\n    handle_errors {\n        route {\n")
		out.WriteString(strings.ReplaceAll(caddySiteCORS(site.Runtime.CORS), "@cors_", "@error_cors_"))
		codes := []int{}
		for code := range site.Runtime.ErrorPages {
			codes = append(codes, code)
		}
		sort.Ints(codes)
		for _, code := range codes {
			fmt.Fprintf(&out, "            @niceenv_error_%d expression {err.status_code} == %d\n            handle @niceenv_error_%d {\n                rewrite * %s\n                file_server\n            }\n", code, code, code, caddyQuote(site.Runtime.ErrorPages[code]))
		}
		if site.Runtime.Kind == "static" && site.Rewrite == "next-export" && site.Runtime.CustomRewrite == nil {
			out.WriteString("            @next404 {\n                expression {err.status_code} == 404\n                file /404.html\n            }\n            handle @next404 {\n                rewrite * /404.html\n                file_server\n            }\n")
		}
		out.WriteString("            handle {\n                respond \"{err.status_code} {err.status_text}\" {err.status_code}\n            }\n        }\n    }\n}\n")
		return out.String(), nil
	}
	plain, err := block(false)
	if err != nil {
		return "", err
	}
	if !site.HTTPS {
		return plain, nil
	}
	secure, err := block(true)
	return plain + secure, err
}
