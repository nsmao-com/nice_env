package core

import (
	"fmt"
	"net/url"
	"path/filepath"
	"regexp"
	"runtime"
	"slices"
	"sort"
	"strconv"
	"strings"
)

func apacheQuote(value string) string {
	value = strings.ReplaceAll(value, `\`, `\\`)
	value = strings.ReplaceAll(value, `"`, `\"`)
	// httpd expands ${...} in quoted configuration strings.
	value = strings.ReplaceAll(value, "$", `\$`)
	return `"` + value + `"`
}
func apachePath(value string) string { return apacheQuote(filepath.ToSlash(DisplayPath(value))) }
func apacheSiteAccess(site Site) string {
	policy := site.Runtime.Access
	if policy == nil {
		return ""
	}
	parts := []string{}
	for _, ip := range policy.Addresses {
		parts = append(parts, "%{CONN_REMOTE_ADDR} -ipmatch '"+ip+"'")
	}
	negate := ""
	if policy.Mode == "allow" {
		negate = "!"
	}
	return "    RewriteEngine On\n    RewriteCond expr \"" + negate + "(" + strings.Join(parts, " || ") + ")\"\n    RewriteRule ^ - [F,END]\n"
}
func apacheSiteCORS(cors *siteCORS) string {
	if cors == nil {
		return ""
	}
	parts := []string{}
	for _, origin := range cors.Origins {
		parts = append(parts, regexp.QuoteMeta(origin))
	}
	origins := strings.Join(parts, "|")
	if slices.Equal(cors.Origins, []string{"*"}) {
		origins = ".+"
	}
	pattern := "^(?:" + origins + ")$"
	condition := "expr=req('Origin') =~ m#" + pattern + "#"
	var out strings.Builder
	for _, header := range corsHeaders {
		fmt.Fprintf(&out, "    Header onsuccess unset %s\n    Header always unset %s\n", header, header)
	}
	origin := "expr=%{req:Origin}"
	if slices.Equal(cors.Origins, []string{"*"}) {
		origin = "*"
	}
	credentials := ""
	if cors.Credentials {
		credentials = "true"
	}
	values := []string{origin, credentials, strings.Join(cors.Methods, ", "), strings.Join(cors.AllowedHeaders, ", "), strings.Join(cors.ExposedHeaders, ", "), strconv.Itoa(cors.MaxAge)}
	for i, value := range values {
		if value != "" {
			fmt.Fprintf(&out, "    Header always set %s \"%s\" \"%s\"\n", corsHeaders[i], value, condition)
		}
	}
	out.WriteString("    Header always merge Vary Origin\n    RewriteEngine On\n")
	fmt.Fprintf(&out, "    RewriteCond %%{REQUEST_METHOD} =OPTIONS\n    RewriteCond %%{HTTP:Origin} \"%s\"\n    RewriteCond %%{HTTP:Access-Control-Request-Method} \"^(?:%s)$\"\n    RewriteRule ^ - [R=204,L]\n", pattern, strings.Join(cors.Methods, "|"))
	out.WriteString("    RewriteCond %{REQUEST_METHOD} =OPTIONS\n    RewriteCond %{HTTP:Access-Control-Request-Method} !^$\n    RewriteRule ^ - [F,L]\n")
	return out.String()
}
func apacheSiteProxies(rt siteRuntime) string {
	rules := orderedSiteProxies(rt)
	if len(rules) == 0 {
		return ""
	}
	var out strings.Builder
	out.WriteString("    SSLProxyEngine On\n    ProxyPreserveHost Off\n    RewriteEngine On\n")
	paths := []string{}
	for _, rule := range rules {
		target := siteProxyDestination(rule)
		u, _ := url.Parse(target)
		cookie := strings.TrimRight(u.Path, "/")
		end := "(?=/|;|$)"
		if cookie == "" {
			end = "(?=/)"
		}
		pattern := `((?i:;[ \t]*path=))` + regexp.QuoteMeta(cookie) + end
		path := regexp.QuoteMeta(rule.Path)
		paths = append(paths, path)
		fmt.Fprintf(&out, "    ProxyPassMatch \"^%s(?:/(.*))?$\" \"%s$1\" upgrade=websocket\n    <Location \"%s\">\n        ProxyPassReverse \"%s\"\n        Header onsuccess edit* Set-Cookie \"%s\" \"$1%s\"\n        Header always edit* Set-Cookie \"%s\" \"$1%s\"\n        RequestHeader set X-Forwarded-Proto \"expr=%%{REQUEST_SCHEME}\"\n    </Location>\n", path, target, rule.Path, strings.TrimRight(target, "/"), pattern, rule.Path, pattern, rule.Path)
	}
	fmt.Fprintf(&out, "    RewriteRule \"^(?:%s)(?:/|$)\" \"-\" [L]\n", strings.Join(paths, "|"))
	return out.String()
}
func (c *Core) renderApacheSite(site Site, options siteConfigOptions) (string, error) {
	root := string(site.RootDir)
	if site.Runtime.Kind != "redirect" {
		var err error
		root, err = filepath.EvalSymlinks(root)
		if err != nil {
			return "", err
		}
	}
	root = filepath.ToSlash(DisplayPath(root))
	rootPattern := regexp.QuoteMeta(strings.TrimRight(root, "/"))
	if runtime.GOOS == "windows" {
		rootPattern = "(?i:" + rootPattern + ")"
	}
	directory := strings.NewReplacer("[", "[[]", "?", "[?]", "*", "[*]").Replace(root)
	gate := apacheSiteAccess(site)
	names := "    ServerName " + site.Domains[0] + "\n    ServerAlias " + strings.Join(site.Domains, " ") + "\n"
	logs := fmt.Sprintf("    CustomLog %s \"%%h %%l %%u %%t \\\"%%r\\\" %%>s %%b\"\n    ErrorLog %s\n", apachePath(filepath.Join(c.Home, "logs", "apache", site.ID+".access.log")), apachePath(filepath.Join(c.Home, "logs", "apache", site.ID+".error.log")))
	var body strings.Builder
	body.WriteString(gate + apacheSiteCORS(site.Runtime.CORS))
	if options.auth != "" {
		fmt.Fprintf(&body, "    <Location \"/\">\n        AuthType Basic\n        AuthName \"Restricted\"\n        AuthBasicProvider file\n        AuthUserFile %s\n        Require valid-user\n    </Location>\n", apachePath(options.auth))
	}
	codes := []int{}
	for code := range site.Runtime.ErrorPages {
		codes = append(codes, code)
	}
	sort.Ints(codes)
	for _, code := range codes {
		fmt.Fprintf(&body, "    ErrorDocument %d %s\n", code, apacheQuote(site.Runtime.ErrorPages[code]))
	}
	body.WriteString(apacheSiteProxies(site.Runtime))
	if site.Runtime.Kind != "redirect" {
		fmt.Fprintf(&body, "    DocumentRoot %s\n    <FilesMatch \"^\\.\">\n        Require all denied\n    </FilesMatch>\n    <DirectoryMatch %s>\n        Require all denied\n    </DirectoryMatch>\n", apacheQuote(root), apacheQuote("^"+rootPattern+`/(?:[^/]+/)*\.(?!well-known(?:/|$))`))
	}
	switch site.Runtime.Kind {
	case "php":
		if options.php < 1 {
			return "", invalid("PHP 池端口尚未分配")
		}
		worker := 0
		for _, ch := range site.ID {
			worker += int(ch)
		}
		worker = options.php + worker%phpPoolWorkers
		fmt.Fprintf(&body, "    <FilesMatch \"\\.ph(p[3457]?|t|tml)$\">\n        SetHandler \"proxy:fcgi://127.0.0.1:%d/\"\n        ProxyFCGIBackendType GENERIC\n", worker)
		if runtime.GOOS == "windows" {
			body.WriteString("        ProxyFCGISetEnvIf \"reqenv('SCRIPT_FILENAME') =~ m|^/([A-Za-z]:/.*)$|\" SCRIPT_FILENAME \"$1\"\n")
		}
		body.WriteString("    </FilesMatch>\n")
		fmt.Fprintf(&body, "    <Directory %s>\n        AllowOverride All\n        Options -Indexes\n        Require all granted\n", apacheQuote(directory))
		if site.Runtime.CustomRewrite == nil {
			body.WriteString("        RewriteEngine On\n        RewriteCond %{REQUEST_FILENAME} !-d\n        RewriteCond %{REQUEST_FILENAME} !-f\n        RewriteRule ^ /index.php [QSA,L]\n")
		}
		body.WriteString("    </Directory>\n")
		if custom := site.Runtime.CustomRewrite; custom != nil {
			body.WriteString("    RewriteEngine On\n" + custom.Content + "\n")
		}
	case "static":
		body.WriteString("    <FilesMatch \"(?i)\\.(?:php[0-9]*|phtml|pht|phar)(?:\\.|$)\">\n        Require all denied\n    </FilesMatch>\n")
		fmt.Fprintf(&body, "    <Directory %s>\n        AllowOverride All\n        Options -Indexes\n        DirectoryIndex index.html index.htm\n        Require all granted\n", apacheQuote(directory))
		if custom := site.Runtime.CustomRewrite; custom != nil {
			body.WriteString(custom.Content + "\n")
		} else {
			switch site.Rewrite {
			case "spa-fallback":
				body.WriteString("        RewriteEngine On\n        RewriteCond %{REQUEST_FILENAME} !-f\n        RewriteCond %{REQUEST_FILENAME} !-d\n        RewriteRule ^ index.html [END]\n")
			case "next-export":
				body.WriteString("        RewriteEngine On\n        RewriteCond %{REQUEST_FILENAME} !-f\n        RewriteCond %{REQUEST_FILENAME} !-d\n        RewriteCond %{REQUEST_FILENAME}.html -f\n        RewriteRule ^(.+?)/?$ $1.html [END]\n")
			}
		}
		body.WriteString("    </Directory>\n")
		if site.Rewrite == "next-export" {
			body.WriteString("    ErrorDocument 404 /404.html\n")
		}
	case "redirect":
		target, err := redirectURL(site.Runtime.Redirect, site.Domains)
		if err != nil {
			return "", err
		}
		target = strings.ReplaceAll(target, "%", `\%`)
		body.WriteString("    AllowEncodedSlashes NoDecode\n    RewriteEngine On\n")
		if site.Runtime.Redirect.PreservePath {
			fmt.Fprintf(&body, "    RewriteCond %%{THE_REQUEST} \"\\s(/[^\\s?]*)(?:\\?[^\\s]*)?\\s\"\n    RewriteRule ^ \"%s%%1\" [R=%d,L,NE]\n", strings.TrimRight(target, "/"), site.Runtime.Redirect.Status)
		} else {
			fmt.Fprintf(&body, "    RewriteRule ^ \"%s\" [R=%d,L,NE,QSD]\n", target, site.Runtime.Redirect.Status)
		}
	default:
		if site.Runtime.ProxyTarget == nil {
			return "", invalid("代理目标未设置")
		}
		fmt.Fprintf(&body, "    SSLProxyEngine On\n    ProxyErrorOverride On\n    ProxyPreserveHost Off\n    ProxyPass / \"%s\" upgrade=websocket\n    ProxyPassReverse / \"%s\"\n", *site.Runtime.ProxyTarget, *site.Runtime.ProxyTarget)
	}
	block := func(port int, secure bool) string {
		ssl := ""
		if secure {
			ssl = fmt.Sprintf("    SSLEngine on\n    SSLCertificateFile %s\n    SSLCertificateKeyFile %s\n", apachePath(options.certificate), apachePath(options.key))
		}
		return fmt.Sprintf("# NiceEnv site: %s\n<VirtualHost *:%d>\n%s%s%s%s</VirtualHost>\n", site.ID, port, names, logs, ssl, body.String())
	}
	out := ""
	if site.HTTPS && site.Runtime.HTTPSRedirect != nil {
		out = fmt.Sprintf("<VirtualHost *:%d>\n%s%s%s    AllowEncodedSlashes NoDecode\n    RewriteEngine On\n    RewriteCond %%{THE_REQUEST} \"\\s(/[^\\s?]*)(?:\\?[^\\s]*)?\\s\"\n    RewriteRule ^ - [E=NSB_HTTPS_URI:%%1]\n    RewriteCond %%{ENV:NSB_HTTPS_URI} !^/\n    RewriteRule ^ - [R=400,L]\n    RewriteCond %%{HTTP_HOST} \"^(%s)(?::[0-9]+)?$\" [NC]\n    RewriteRule ^ \"https://%%1%s%%{ENV:NSB_HTTPS_URI}\" [R=%d,L,NE]\n    RewriteRule ^ - [R=421,L]\n</VirtualHost>\n", options.http, names, logs, gate, siteHTTPSHostPattern(site), siteHTTPSPortSuffix(options.https), *site.Runtime.HTTPSRedirect)
	} else {
		out = block(options.http, false)
	}
	if site.HTTPS {
		out += block(options.https, true)
	}
	return out, nil
}
