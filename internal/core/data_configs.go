package core

import (
	"bytes"
	"context"
	"encoding/json"
	"encoding/xml"
	"errors"
	"io"
	"io/fs"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strconv"
	"strings"
	"unicode/utf8"

	"gopkg.in/yaml.v3"
)

type moveToken struct {
	start, end int
	value      string
	quote      byte
}

// A lossless lexer for simple configuration arguments. Values retain normal
// Windows backslashes; only quote/backslash escapes are decoded.
func moveTokens(text string) ([]moveToken, error) {
	var tokens []moveToken
	for i := 0; i < len(text); {
		if strings.ContainsRune(" \t\r\n", rune(text[i])) {
			i++
			continue
		}
		if text[i] == '#' {
			break
		}
		start := i
		quote := byte(0)
		if text[i] == '\'' || text[i] == '"' || text[i] == '`' {
			quote = text[i]
			i++
		}
		var word strings.Builder
		closed := quote == 0
		for i < len(text) {
			ch := text[i]
			if quote != 0 && ch == quote {
				closed = true
				i++
				break
			}
			if quote == 0 && strings.ContainsRune(" \t\r\n;{}", rune(ch)) {
				break
			}
			if ch == '\\' && i+1 < len(text) && (text[i+1] == quote && quote != 0 || text[i+1] == '\\') {
				i++
				ch = text[i]
			}
			word.WriteByte(ch)
			i++
		}
		if !closed {
			return nil, invalid("配置存在未闭合引号，无法迁移")
		}
		if i == start {
			i++
			continue
		}
		tokens = append(tokens, moveToken{start, i, word.String(), quote})
	}
	return tokens, nil
}
func movePathKey(raw string) bool {
	key := strings.ToLower(strings.ReplaceAll(raw, "-", "_"))
	if slices.Contains([]string{"ssl_crl", "ssl_password_file", "ssl_session_ticket_key", "proxy_ssl_certificate", "proxy_ssl_certificate_key", "proxy_ssl_trusted_certificate", "proxy_ssl_crl", "proxy_ssl_password_file", "grpc_ssl_certificate", "grpc_ssl_certificate_key", "grpc_ssl_trusted_certificate", "grpc_ssl_crl", "grpc_ssl_password_file"}, key) {
		return true
	}
	return slices.Contains([]string{"root", "alias", "include", "include_optional", "includeoptional", "pid", "pidfile", "pid_file", "error_log", "access_log", "auth_basic_user_file", "ssl_certificate", "ssl_certificate_key", "ssl_client_certificate", "ssl_trusted_certificate", "ssl_dhparam", "client_body_temp_path", "proxy_temp_path", "fastcgi_temp_path", "uwsgi_temp_path", "scgi_temp_path", "proxy_cache_path", "fastcgi_cache_path", "documentroot", "serverroot", "errorlog", "customlog", "transferlog", "authuserfile", "sslcertificatefile", "sslcertificatekeyfile", "sslcertificatechainfile", "sslclientcacertificatefile", "loadmodule", "typesconfig", "directoryindex", "directory", "scriptalias", "scriptaliasmatch", "basedir", "datadir", "tmpdir", "socket", "log_error", "slow_query_log_file", "general_log_file", "log_bin", "relay_log", "secure_file_priv", "plugin_dir", "character_sets_dir", "lc_messages_dir", "innodb_data_home_dir", "innodb_log_group_home_dir", "innodb_undo_directory", "extension_dir", "upload_tmp_dir", "session.save_path", "sys_temp_dir", "openssl.cafile", "curl.cainfo", "browscap", "opcache.file_cache", "xdebug.output_dir", "xdebug.log", "data_directory", "hba_file", "ident_file", "external_pid_file", "ssl_cert_file", "ssl_key_file", "ssl_ca_file", "log_directory", "unix_socket_directories", "dir", "dbfilename", "appenddirname", "logfile", "aclfile", "tls_cert_file", "tls_key_file", "tls_ca_cert_file", "dbpath", "path", "storage_path", "snapshots_path", "temp_path", "config_file", "home_dir", "certificates_path", "backups_path", "credentials_path", "users_base_dir", "private_key", "certificate_file", "certificate_key_file", "ca_certificates", "trusted_user_ca_keys", "host_keys", "path.data", "path.logs", "path.repo", "path.home", "path.conf", "dbms.directories.data", "server.directories.data", "server.directories.logs", "server.directories.run", "server.directories.import", "server.directories.plugins", "server.directories.transaction.logs.root", "data_dir", "log_file", "store_dir", "log.dir", "configpath", "logpath", "docbase", "appbase", "keystorefile", "truststorefile"}, key)
}
func moveSensitiveKey(raw string) bool {
	key := strings.ToLower(raw)
	return strings.Contains(key, "password") || strings.Contains(key, "secret") || strings.Contains(key, "token") || strings.Contains(key, "credential") || strings.Contains(key, "passwd")
}

func rebaseDataConfigs(ctx context.Context, root, source, target string, protected ...string) (int64, error) {
	r := dataPathRebase{source, target}
	count := int64(0)
	nginx, err := planWebConfigMove(ctx, root, r, nil)
	if err != nil {
		return 0, err
	}
	// Runtime trees contain packages, credentials and examples. Only explicit
	// web-server references above can authorise a rewrite inside those trees.
	directories := []string{"etc"}
	// These services keep editable config beside their state. Never scan the
	// PostgreSQL database files or Tomcat webapps as configuration text.
	for _, pattern := range []string{"data/tomcat/*/conf", "data/postgresql/*/postgresql.conf", "data/postgresql/*/postgresql.auto.conf", "data/postgresql/*/pg_hba.conf", "data/postgresql/*/pg_ident.conf"} {
		matches, err := filepath.Glob(filepath.Join(root, filepath.FromSlash(pattern)))
		if err != nil {
			return count, err
		}
		for _, match := range matches {
			rel, err := filepath.Rel(root, match)
			if err != nil {
				return count, err
			}
			directories = append(directories, rel)
		}
	}
	for _, directory := range directories {
		base := filepath.Join(root, directory)
		if _, err := os.Stat(base); os.IsNotExist(err) {
			continue
		} else if err != nil {
			return count, err
		}
		err := filepath.WalkDir(base, func(p string, d fs.DirEntry, err error) error {
			if err != nil {
				return err
			}
			if err = ctx.Err(); err != nil {
				return err
			}
			for _, path := range protected {
				if pathWithin(path, p) {
					if d.IsDir() {
						return filepath.SkipDir
					}
					return nil
				}
			}
			if d.IsDir() {
				return nil
			}
			rel, _ := filepath.Rel(root, p)
			if nginx.handled(p) {
				return nil
			}
			if strings.HasPrefix(filepath.ToSlash(rel), "etc/nginx/") || strings.HasPrefix(filepath.ToSlash(rel), "etc/apache/") {
				return nil
			}
			ext := strings.ToLower(filepath.Ext(p))
			if directory == "etc" && slices.Contains([]string{".key", ".crt", ".pem", ".p12", ".pfx", ".jks", ".sqlite", ".db", ".bak", ".htpasswd"}, ext) {
				return nil
			}
			info, err := os.Stat(p)
			if err != nil {
				return err
			}
			if info.Size() > 16*1024*1024 {
				if directory == "etc" {
					return invalid("配置文件过大，无法自动迁移：" + filepath.ToSlash(rel))
				}
				return nil
			}
			body, err := os.ReadFile(p)
			if err != nil {
				return err
			}
			if !bytes.Contains(bytes.ToLower(body), bytes.ToLower([]byte(source))) && !bytes.Contains(bytes.ToLower(body), bytes.ToLower([]byte(strings.ReplaceAll(source, "\\", "/")))) && !bytes.Contains(bytes.ToLower(body), bytes.ToLower([]byte(strings.ReplaceAll(source, "\\", "\\\\")))) {
				return nil
			}
			if !utf8.Valid(body) {
				return invalid("含旧目录的配置不是 UTF-8：" + filepath.ToSlash(rel))
			}
			next, err := rebaseConfigContent(r, filepath.ToSlash(rel), string(body))
			if err != nil {
				return &AppError{Code: "DATA_DIR_CONFIG", Message: "配置路径迁移未完成：" + filepath.ToSlash(rel), Detail: err.Error()}
			}
			if next != string(body) {
				if err = writePrivateAtomic(p, []byte(next)); err != nil {
					return err
				}
				count++
			}
			return nil
		})
		if err != nil {
			return count, err
		}
	}
	for path, next := range nginx.edits {
		for _, resource := range protected {
			if pathWithin(resource, path) {
				return 0, invalid("配置与受保护资源共用文件，不能自动迁移：" + DisplayPath(path))
			}
		}
		if err := writePrivateAtomic(path, []byte(next)); err != nil {
			return count, err
		}
		count++
	}
	return count, nil
}
func rebaseConfigContent(r dataPathRebase, rel, text string, nginxContext ...bool) (string, error) {
	ext := strings.ToLower(filepath.Ext(strings.TrimSuffix(rel, ".disabled")))
	if len(nginxContext) > 0 || strings.Contains(rel, "/nginx/") && (ext == ".conf" || strings.HasSuffix(rel, "fastcgi_params") || strings.HasSuffix(rel, "scgi_params") || strings.HasSuffix(rel, "uwsgi_params")) {
		nodes, err := parseNginx(text)
		if err != nil {
			return "", err
		}
		type edit struct {
			start, end int
			text       string
		}
		var edits []edit
		var walk func([]nginxNode, bool) error
		walk = func(nodes []nginxNode, dataBlock bool) error {
			for _, node := range nodes {
				end := node.end
				if node.open >= 0 {
					end = node.open
				}
				// map/geo/split_clients/types contain data rows, not ordinary
				// directives. A row named root or alias must keep its literal value.
				if len(node.words) == 0 {
					continue
				}
				if !dataBlock || node.words[0] == "include" {
					next, err := rebaseConfigLine(r, rel, text[node.start:end])
					if err != nil {
						return err
					}
					if next != text[node.start:end] {
						edits = append(edits, edit{node.start, end, next})
					}
				}
				childData := dataBlock || slices.Contains([]string{"map", "geo", "split_clients", "types"}, node.words[0])
				if err = walk(node.children, childData); err != nil {
					return err
				}
			}
			return nil
		}
		if err = walk(nodes, len(nginxContext) > 0 && nginxContext[0]); err != nil {
			return "", err
		}
		sort.Slice(edits, func(i, j int) bool { return edits[i].start > edits[j].start })
		for _, e := range edits {
			text = text[:e.start] + e.text + text[e.end:]
		}
		if _, err = parseNginx(text); err != nil {
			return "", err
		}
		return text, nil
	}
	if ext == ".json" {
		var obj any
		decoder := json.NewDecoder(strings.NewReader(text))
		decoder.UseNumber()
		if err := decoder.Decode(&obj); err != nil {
			return "", err
		}
		changed := false
		var walk func(any, string)
		walk = func(value any, key string) {
			switch node := value.(type) {
			case map[string]any:
				for k, v := range node {
					if moveSensitiveKey(k) && !movePathKey(k) {
						continue
					}
					if s, ok := v.(string); ok && movePathKey(k) {
						next := r.path(s)
						changed = changed || next != s
						node[k] = next
					} else {
						walk(v, k)
					}
				}
			case []any:
				for i, v := range node {
					if s, ok := v.(string); ok && movePathKey(key) {
						next := r.path(s)
						changed = changed || next != s
						node[i] = next
					} else {
						walk(v, key)
					}
				}
			}
		}
		walk(obj, "")
		if !changed {
			return text, nil
		}
		body, err := json.MarshalIndent(obj, "", "  ")
		return string(body) + "\n", err
	}
	if ext == ".yaml" || ext == ".yml" || strings.Contains(rel, "/mongodb/") {
		var doc yaml.Node
		if err := yaml.Unmarshal([]byte(text), &doc); err != nil {
			return "", err
		}
		changed := false
		var walk func(*yaml.Node, string)
		walk = func(n *yaml.Node, key string) {
			switch n.Kind {
			case yaml.DocumentNode:
				for _, child := range n.Content {
					walk(child, key)
				}
			case yaml.MappingNode:
				for i := 0; i+1 < len(n.Content); i += 2 {
					k := n.Content[i].Value
					if !moveSensitiveKey(k) || movePathKey(k) {
						walk(n.Content[i+1], k)
					}
				}
			case yaml.SequenceNode:
				for _, child := range n.Content {
					walk(child, key)
				}
			case yaml.ScalarNode:
				if n.Tag == "!!str" && movePathKey(key) {
					next := r.path(n.Value)
					changed = changed || next != n.Value
					n.Value = next
				}
			}
		}
		walk(&doc, "")
		if !changed {
			return text, nil
		}
		body, err := yaml.Marshal(&doc)
		return string(body), err
	}
	if ext == ".xml" {
		return rebaseConfigXML(r, text)
	}
	lines := strings.SplitAfter(text, "\n")
	for i, line := range lines {
		trim := strings.TrimSpace(line)
		if trim == "" || strings.HasPrefix(trim, "#") || strings.HasPrefix(trim, ";") {
			continue
		}
		next, err := rebaseConfigLine(r, rel, line)
		if err != nil {
			return "", err
		}
		lines[i] = next
	}
	return strings.Join(lines, ""), nil
}
func rebaseConfigLine(r dataPathRebase, rel, line string) (string, error) {
	// Assignment-style configs require the actual key to be a known path field.
	trim := strings.TrimSpace(line)
	if equal := strings.IndexByte(trim, '='); equal > 0 && !strings.ContainsAny(strings.TrimSpace(trim[:equal]), " \t") {
		key := strings.TrimSpace(trim[:equal])
		if !movePathKey(key) {
			return line, nil
		}
		offset := strings.Index(line, "=") + 1
		tokens, err := moveTokens(line[offset:])
		if err != nil {
			return "", err
		}
		if len(tokens) == 0 {
			return line, nil
		}
		t := tokens[0]
		next := r.path(t.value)
		if next == t.value {
			return line, nil
		}
		quoted := strconv.Quote(filepath.ToSlash(next))
		if strings.Contains(rel, "/postgresql/") {
			quoted = "'" + strings.ReplaceAll(filepath.ToSlash(next), "'", "''") + "'"
		}
		return line[:offset+t.start] + quoted + line[offset+t.end:], nil
	}
	tokens, err := moveTokens(line)
	if err != nil {
		return "", err
	}
	if len(tokens) < 2 {
		return line, nil
	}
	directive := strings.ToLower(strings.Trim(tokens[0].value, "<>"))
	indices := []int{}
	if movePathKey(directive) {
		indices = []int{1}
		if slices.Contains([]string{"loadmodule", "customlog", "scriptalias", "scriptaliasmatch"}, directive) {
			indices = []int{2}
			if directive == "customlog" {
				indices = []int{1}
			}
		}
	}
	if directive == "tls" {
		indices = []int{1, 2}
	}
	if strings.Contains(filepath.ToSlash(rel), "/apache/") {
		if directive == "define" && len(tokens) > 2 && tokens[1].value == "NSB_ETC" {
			indices = []int{2}
		}
		if directive == "alias" || directive == "scriptalias" {
			indices = []int{len(tokens) - 1}
		}
		if directive == "loadfile" {
			indices = nil
			for i := 1; i < len(tokens); i++ {
				indices = append(indices, i)
			}
		}
	}
	if directive == "import" || slices.Contains([]string{"authgroupfile", "sslcacertificatefile", "sslcacertificatepath", "sslcarevocationfile", "sslcarevocationpath"}, directive) {
		indices = []int{1}
	}
	if directive == "root" && tokens[1].value == "*" {
		indices = []int{2}
	}
	if directive == "output" && tokens[1].value == "file" {
		indices = []int{2}
	}
	if directive == "fastcgi_param" && len(tokens) > 2 && slices.Contains([]string{"SCRIPT_FILENAME", "DOCUMENT_ROOT"}, tokens[1].value) {
		indices = []int{2}
	}
	for j := len(indices) - 1; j >= 0; j-- {
		index := indices[j]
		if index >= len(tokens) {
			continue
		}
		t := tokens[index]
		suffix := ""
		value := t.value
		if strings.HasSuffix(value, ">") {
			value = strings.TrimSuffix(value, ">")
			suffix = ">"
		}
		next := r.path(value)
		if next == value {
			continue
		}
		quoted := strconv.Quote(filepath.ToSlash(next)) + suffix
		line = line[:t.start] + quoted + line[t.end:]
	}
	return line, nil
}
func rebaseConfigXML(r dataPathRebase, text string) (string, error) {
	decoder := xml.NewDecoder(strings.NewReader(text))
	var output bytes.Buffer
	encoder := xml.NewEncoder(&output)
	changed := false
	var names []string
	for {
		token, err := decoder.Token()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return "", err
		}
		switch t := token.(type) {
		case xml.StartElement:
			names = append(names, t.Name.Local)
			for i := range t.Attr {
				if movePathKey(t.Attr[i].Name.Local) {
					next := r.path(t.Attr[i].Value)
					changed = changed || next != t.Attr[i].Value
					t.Attr[i].Value = next
				}
			}
			token = t
		case xml.EndElement:
			if len(names) > 0 {
				names = names[:len(names)-1]
			}
		case xml.CharData:
			if len(names) > 0 && movePathKey(names[len(names)-1]) {
				next := r.path(string(t))
				changed = changed || next != string(t)
				token = xml.CharData(next)
			}
		}
		if err = encoder.EncodeToken(token); err != nil {
			return "", err
		}
	}
	if !changed {
		return text, nil
	}
	if err := encoder.Flush(); err != nil {
		return "", err
	}
	return output.String(), nil
}
