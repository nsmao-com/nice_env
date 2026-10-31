package core

import (
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"

	"niceenv/internal/process"
)

type envEntry struct {
	Key        string `json:"key"`
	Value      string `json:"value"`
	Commented  bool   `json:"commented"`
	Secret     bool   `json:"secret"`
	Line       int    `json:"line"`
	NeedsQuote bool   `json:"needsQuote"`
}
type envRecord struct {
	entry          envEntry
	start, end     int
	prefix, suffix string
	valid          bool
}
type envDBHint struct {
	Database string `json:"database"`
	Username string `json:"username"`
	Password string `json:"password"`
	Port     int    `json:"port"`
}
type envView struct {
	SiteID         string     `json:"siteId"`
	SiteName       string     `json:"siteName"`
	Path           Path       `json:"path"`
	FileName       string     `json:"fileName"`
	BackupExists   bool       `json:"backupExists"`
	HasCompiledEnv bool       `json:"hasCompiledEnv"`
	Exists         bool       `json:"exists"`
	Revision       string     `json:"revision"`
	Entries        []envEntry `json:"entries"`
	DBHint         *envDBHint `json:"dbHint,omitempty"`
	Variants       []string   `json:"variants"`
}
type envProject struct {
	id, name, siteRoot, runtime, db, syntax string
	root                                    *os.Root
	hint                                    *envDBHint
}
type envRequest struct {
	SiteID                 string      `json:"siteId"`
	FileName               string      `json:"fileName"`
	Changes                [][2]string `json:"changes"`
	ExpectedRevision       string      `json:"expectedRevision"`
	ExpectedBackupRevision string      `json:"expectedBackupRevision"`
}

func envSecret(key string) bool {
	k := strings.ToUpper(key)
	for _, word := range []string{"PASSWORD", "PASSWD", "SECRET", "_KEY", "TOKEN", "PRIVATE", "CREDENTIAL"} {
		if strings.Contains(k, word) {
			return true
		}
	}
	return strings.HasSuffix(k, "_PASS") || strings.HasSuffix(k, "KEY") || k == "DATABASE_URL"
}
func envNeedsQuote(value string) bool {
	return strings.IndexFunc(value, func(r rune) bool { return unicode.IsSpace(r) || strings.ContainsRune("#\"'\\$", r) }) >= 0
}
func envSection(line string) string {
	line = strings.TrimSpace(line)
	if !strings.HasPrefix(line, "[") {
		return ""
	}
	name, rest, ok := strings.Cut(line[1:], "]")
	rest = strings.TrimSpace(rest)
	if ok && name != "" && (rest == "" || strings.HasPrefix(rest, ";") || strings.HasPrefix(rest, "#")) {
		return name
	}
	return ""
}
func envValueEnd(value, syntax string) int {
	if value == "" {
		return 0
	}
	quote := value[0]
	if quote != '\'' && quote != '"' {
		comment := byte('#')
		if syntax == "thinkphp" {
			comment = ';'
		}
		if i := strings.IndexByte(value, comment); i >= 0 {
			return i
		}
		return len(value)
	}
	escaped := false
	for i := 1; i < len(value); i++ {
		ch := value[i]
		if escaped {
			escaped = false
			continue
		}
		if ch == '\\' && syntax != "thinkphp" && (quote == '"' || syntax == "codeigniter") {
			escaped = true
			continue
		}
		if ch == quote {
			return i + 1
		}
	}
	return -1
}
func envUnquote(value, syntax string) string {
	if len(value) < 2 || value[0] != value[len(value)-1] || (value[0] != '"' && value[0] != '\'') {
		return value
	}
	quote := value[0]
	inner := value[1 : len(value)-1]
	if syntax == "thinkphp" {
		if quote == '"' {
			return inner
		}
		return value
	}
	if syntax == "dotenv" && quote == '\'' {
		return inner
	}
	var out strings.Builder
	for i := 0; i < len(inner); i++ {
		ch := inner[i]
		if ch != '\\' || i+1 == len(inner) {
			out.WriteByte(ch)
			continue
		}
		i++
		next := inner[i]
		if syntax == "codeigniter" {
			if next == quote || next == '\\' {
				out.WriteByte(next)
			} else {
				out.WriteByte('\\')
				out.WriteByte(next)
			}
			continue
		}
		switch next {
		case 'n':
			out.WriteByte('\n')
		case 'r':
			out.WriteByte('\r')
		case 't':
			out.WriteByte('\t')
		case '\\', '"', '$':
			out.WriteByte(next)
		default:
			out.WriteByte('\\')
			out.WriteByte(next)
		}
	}
	return out.String()
}

var envKey = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_.]*$`)
var envParseKey = regexp.MustCompile(`^[A-Za-z0-9_.]+$`)

func envLines(content string) []string {
	content = strings.ReplaceAll(content, "\r\n", "\n")
	lines := strings.Split(content, "\n")
	if strings.HasSuffix(content, "\n") {
		lines = lines[:len(lines)-1]
	}
	if content == "" {
		return []string{}
	}
	return lines
}
func parseEnv(content, syntax string) []envRecord {
	lines := envLines(content)
	records := []envRecord{}
	section := ""
	for index := 0; index < len(lines); {
		start := index
		raw := lines[index]
		index++
		body := strings.TrimSpace(strings.TrimPrefix(raw, "\ufeff"))
		if syntax == "thinkphp" {
			if s := envSection(body); s != "" {
				section = s
				continue
			}
		}
		commented := strings.HasPrefix(body, "#")
		if commented {
			body = strings.TrimSpace(body[1:])
		}
		body = strings.TrimPrefix(body, "export ")
		key, value, ok := strings.Cut(body, "=")
		key = strings.TrimSpace(key)
		if !ok || !envParseKey.MatchString(key) {
			continue
		}
		value = strings.TrimLeftFunc(value, unicode.IsSpace)
		for !commented && envValueEnd(value, syntax) < 0 && index < len(lines) {
			value += "\n" + lines[index]
			index++
		}
		end := envValueEnd(value, syntax)
		valid := end >= 0
		if !valid {
			end = len(value)
		}
		rawValue := strings.TrimRightFunc(value[:end], unicode.IsSpace)
		suffix := value[len(rawValue):]
		quoted := len(rawValue) >= 2 && rawValue[0] == rawValue[len(rawValue)-1] && (rawValue[0] == '"' || rawValue[0] == '\'')
		decoded := envUnquote(rawValue, syntax)
		comment := "#"
		if syntax == "thinkphp" {
			comment = ";"
		}
		valid = valid && (strings.TrimSpace(suffix) == "" || strings.HasPrefix(strings.TrimSpace(suffix), comment))
		if section != "" {
			key = section + "." + key
		}
		records = append(records, envRecord{entry: envEntry{Key: key, Value: decoded, Commented: commented, Secret: envSecret(key), Line: start + 1, NeedsQuote: !commented && !quoted && (envNeedsQuote(decoded) || strings.HasPrefix(suffix, comment))}, start: start, end: index, prefix: raw[:strings.IndexByte(raw, '=')+1], suffix: suffix, valid: valid})
	}
	return records
}
func quoteEnv(value, syntax string) string {
	if !envNeedsQuote(value) && !(syntax == "thinkphp" && strings.Contains(value, ";")) {
		return value
	}
	if syntax == "thinkphp" {
		return "\"" + value + "\""
	}
	replacer := strings.NewReplacer("\\", "\\\\", "\"", "\\\"")
	value = replacer.Replace(value)
	if syntax == "dotenv" {
		value = strings.NewReplacer("$", "\\$", "\n", "\\n", "\r", "\\r", "\t", "\\t").Replace(value)
	}
	return "\"" + value + "\""
}
func applyEnvChanges(content, syntax string, changes [][2]string) (string, error) {
	if len(changes) == 0 {
		return content, nil
	}
	if len(changes) > 5000 {
		return "", invalid("单次环境变量修改超过限制")
	}
	bom := ""
	if strings.HasPrefix(content, "\ufeff") {
		bom = "\ufeff"
		content = strings.TrimPrefix(content, bom)
	}
	records := parseEnv(content, syntax)
	for _, r := range records {
		if !r.entry.Commented && !r.valid {
			return "", &AppError{Code: "ENV_SYNTAX", Message: "环境文件引号或行尾格式不完整，请先修正第 " + strconv.Itoa(r.entry.Line) + " 行"}
		}
	}
	values := map[string]string{}
	for _, change := range changes {
		key, value := change[0], change[1]
		if !envKey.MatchString(key) {
			return "", invalid("环境变量名须以字母或下划线开头，只允许字母、数字、下划线和点")
		}
		if _, ok := values[key]; ok {
			return "", invalid("同次保存不能提交重复变量名")
		}
		if strings.ContainsRune(value, 0) || !utf8.ValidString(value) {
			return "", invalid("环境变量不能包含 NUL 或无效字符")
		}
		if syntax != "dotenv" && strings.ContainsAny(value, "\n\r") {
			return "", invalid("此框架的环境变量不能包含换行")
		}
		if syntax == "thinkphp" && strings.Contains(value, "\"") {
			return "", invalid("ThinkPHP INI 原始值不能包含双引号，请更换该值")
		}
		if envSecret(key) {
			switch strings.ToLower(value) {
			case "true", "false", "on", "off", "null", "empty", "(true)", "(false)", "(null)", "(empty)":
				return "", invalid("框架会把此密码或密钥识别为布尔值或空值")
			}
			if syntax == "codeigniter" && strings.Contains(value, "${") {
				return "", invalid("CodeIgniter 会展开密码中的环境变量表达式，请更换密码")
			}
		}
		values[key] = value
	}
	lines := envLines(content)
	out := []string{}
	cursor := 0
	applied := map[string]bool{}
	for _, r := range records {
		value, ok := values[r.entry.Key]
		if !ok || r.entry.Commented {
			continue
		}
		out = append(out, lines[cursor:r.start]...)
		out = append(out, r.prefix+quoteEnv(value, syntax)+r.suffix)
		cursor = r.end
		applied[r.entry.Key] = true
	}
	out = append(out, lines[cursor:]...)
	added := false
	for _, change := range changes {
		key, value := change[0], change[1]
		if applied[key] {
			continue
		}
		line := key + "=" + quoteEnv(value, syntax)
		if syntax == "thinkphp" {
			index := -1
			for i := len(out) - 1; i >= 0; i-- {
				section := envSection(out[i])
				if section != "" && strings.HasPrefix(key, section+".") {
					index = i + 1
					line = strings.TrimPrefix(key, section+".") + "=" + quoteEnv(value, syntax)
					break
				}
			}
			if index < 0 {
				index = len(out)
				for i, raw := range out {
					if envSection(raw) != "" {
						index = i
						break
					}
				}
			}
			out = append(out, "")
			copy(out[index+1:], out[index:])
			out[index] = line
		} else {
			if !added && len(out) > 0 && strings.TrimSpace(out[len(out)-1]) != "" {
				out = append(out, "")
			}
			out = append(out, line)
		}
		added = true
	}
	newline := "\n"
	if strings.Contains(content, "\r\n") {
		newline = "\r\n"
	}
	result := bom + strings.Join(out, newline)
	if strings.HasSuffix(content, "\n") || added {
		result += newline
	}
	if len(result) > siteTextLimit {
		return "", invalid("环境文件不能超过 1 MiB")
	}
	return result, nil
}

func envName(name string) bool {
	if name == ".env" {
		return true
	}
	if !strings.HasPrefix(name, ".env.") || len(name) <= 5 || len(name) > 128 || strings.HasSuffix(name, ".") {
		return false
	}
	lower := strings.ToLower(name)
	if lower == ".env.local.php" || strings.HasSuffix(lower, ".nsb-backup") || strings.HasSuffix(lower, ".nsb-before-restore") {
		return false
	}
	for _, r := range name[5:] {
		if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '.' || r == '_' || r == '-') {
			return false
		}
	}
	return true
}
func envRead(root *os.Root, name string) ([]byte, bool, error) {
	info, err := siteCheck(root, name, true)
	if err != nil {
		return nil, false, err
	}
	if info == nil {
		return nil, false, nil
	}
	if !info.Mode().IsRegular() || info.Size() > siteTextLimit {
		return nil, false, invalid("环境文件及备份必须为不超过 1 MiB 的普通文件")
	}
	f, err := root.Open(name)
	if err != nil {
		return nil, false, err
	}
	defer f.Close()
	data, err := io.ReadAll(io.LimitReader(f, siteTextLimit+1))
	if err != nil {
		return nil, false, err
	}
	if len(data) > siteTextLimit || !utf8.Valid(data) || bytes.ContainsRune(data, 0) {
		return nil, false, invalid("环境文件过大或不是有效 UTF-8 文本")
	}
	return data, true, nil
}
func envChanged() error {
	return &AppError{Code: "ENV_CHANGED", Message: "环境文件或站点目录已变化，未覆盖当前文件", Hint: "请重新读取并核对最新内容。"}
}
func replaceEnv(root *os.Root, name string, expected []byte, exists bool, content []byte) error {
	current, present, err := envRead(root, name)
	if err != nil {
		return err
	}
	if present != exists || !bytes.Equal(current, expected) {
		return envChanged()
	}
	mode := os.FileMode(0600)
	if exists {
		info, err := root.Stat(name)
		if err != nil {
			return err
		}
		mode = info.Mode().Perm()
		if mode&0200 == 0 {
			return invalid("环境文件或备份是只读文件")
		}
	}
	tmp, err := newID(".niceenv-env")
	if err != nil {
		return err
	}
	f, err := root.OpenFile(tmp, os.O_CREATE|os.O_EXCL|os.O_WRONLY, mode)
	if err != nil {
		return err
	}
	defer root.Remove(tmp)
	_, writeErr := f.Write(content)
	syncErr := f.Sync()
	closeErr := f.Close()
	if err = errors.Join(writeErr, syncErr, closeErr); err != nil {
		return err
	}
	current, present, err = envRead(root, name)
	if err != nil {
		return err
	}
	if present != exists || !bytes.Equal(current, expected) {
		return envChanged()
	}
	if !exists {
		return root.Link(tmp, name)
	}
	return root.Rename(tmp, name)
}

func projectRoot(webRoot string) string {
	switch filepath.Base(webRoot) {
	case "public", "out", "dist", "build":
		parent := filepath.Dir(webRoot)
		for _, name := range []string{"composer.json", "package.json", "pyproject.toml", "requirements.txt", "go.mod", ".niceenv.json", ".nvmrc", ".node-version", ".python-version", "artisan", ".env", ".env.example", ".env.local"} {
			if info, err := os.Lstat(filepath.Join(parent, name)); err == nil && info.Mode().IsRegular() {
				return parent
			}
		}
	}
	return webRoot
}
func rootFile(root *os.Root, name string) bool {
	info, err := siteCheck(root, name, false)
	return err == nil && info.Mode().IsRegular()
}
func (c *Core) envProject(ctx context.Context, id string) (*envProject, error) {
	return c.openProject(ctx, id, true)
}
func (c *Core) openProject(ctx context.Context, id string, databaseHint bool) (*envProject, error) {
	p := &envProject{id: id}
	err := c.Store.db.QueryRowContext(ctx, "SELECT name,root_dir,runtime,COALESCE(db,'') FROM sites WHERE id=?", id).Scan(&p.name, &p.siteRoot, &p.runtime, &p.db)
	if errors.Is(err, sql.ErrNoRows) {
		return nil, invalid("站点不存在")
	}
	if err != nil {
		return nil, err
	}
	var runtime struct {
		Kind string `json:"kind"`
	}
	if json.Unmarshal([]byte(p.runtime), &runtime) != nil {
		return nil, invalid("站点运行时记录无效")
	}
	if runtime.Kind == "redirect" {
		return nil, invalid("跳转站点没有项目环境文件")
	}
	if !filepath.IsAbs(p.siteRoot) {
		return nil, invalid("项目目录必须为绝对路径")
	}
	actual, err := filepath.EvalSymlinks(projectRoot(DisplayPath(p.siteRoot)))
	if err != nil {
		return nil, err
	}
	p.root, err = os.OpenRoot(actual)
	if err != nil {
		return nil, err
	}
	p.syntax = "dotenv"
	if rootFile(p.root, "think") {
		p.syntax = "thinkphp"
	} else if rootFile(p.root, "spark") {
		p.syntax = "codeigniter"
	}
	if databaseHint && p.db != "" && p.db != "null" {
		var db struct {
			Enabled  bool   `json:"enabled"`
			Database string `json:"database"`
			Username string `json:"username"`
			Password string `json:"password"`
			Port     int    `json:"port"`
			Version  string `json:"version"`
		}
		if json.Unmarshal([]byte(p.db), &db) != nil {
			p.root.Close()
			return nil, invalid("站点数据库绑定格式无效")
		}
		if db.Enabled {
			port := db.Port
			id := "mysql"
			if db.Version != "" {
				id += "@" + db.Version
			}
			if db.Version != "" {
				var assigned int
				err := c.Store.db.QueryRowContext(ctx, "SELECT base_port FROM port_assign WHERE service_id=?", id).Scan(&assigned)
				if err == nil {
					port = assigned
				} else if !errors.Is(err, sql.ErrNoRows) {
					p.root.Close()
					return nil, err
				}
			}
			if value, found, err := c.servicePortOverride(ctx, id, "mysql"); err != nil {
				p.root.Close()
				return nil, err
			} else if found {
				port, err = strconv.Atoi(value)
				if err != nil {
					p.root.Close()
					return nil, invalid("数据库端口设置无效")
				}
			}
			c.services.mu.RLock()
			if entry := c.services.entries[id]; entry != nil && entry.child != nil && entry.resolved != nil {
				port = entry.resolved.port
			}
			c.services.mu.RUnlock()
			if port == 0 {
				profile, _, err := c.Store.setting(ctx, "portProfile")
				if err != nil {
					p.root.Close()
					return nil, err
				}
				port = 3306
				if profile == "safe" {
					port = 23306
				}
			}
			if port < 1 || port > 65535 {
				p.root.Close()
				return nil, invalid("数据库绑定端口无效")
			}
			p.hint = &envDBHint{db.Database, db.Username, db.Password, port}
		}
	}
	return p, nil
}
func (c *Core) envRevision(p *envProject, name string, data []byte, exists bool) string {
	encoded, _ := json.Marshal([]any{p.id, p.siteRoot, p.root.Name(), p.runtime, p.db, p.hint, p.syntax, name, exists, data})
	mac := hmac.New(sha256.New, c.snapshotKey[:])
	mac.Write(encoded)
	return hex.EncodeToString(mac.Sum(nil))
}
func (c *Core) envView(p *envProject, name string, data []byte, exists bool) (envView, error) {
	v := envView{SiteID: p.id, SiteName: p.name, Path: Path(filepath.Join(p.root.Name(), name)), FileName: name, Exists: exists, Revision: c.envRevision(p, name, data, exists), Entries: []envEntry{}, DBHint: p.hint, Variants: []string{}, HasCompiledEnv: rootFile(p.root, ".env.local.php")}
	_, err := p.root.Lstat(name + ".nsb-backup")
	v.BackupExists = err == nil
	for _, record := range parseEnv(string(data), p.syntax) {
		v.Entries = append(v.Entries, record.entry)
	}
	f, err := p.root.Open(".")
	if err != nil {
		return v, err
	}
	defer f.Close()
	entries, err := f.ReadDir(10001)
	if err != nil && err != io.EOF {
		return v, err
	}
	if len(entries) > 10000 {
		return v, invalid("项目根目录条目过多")
	}
	for _, entry := range entries {
		if envName(entry.Name()) && rootFile(p.root, entry.Name()) {
			v.Variants = append(v.Variants, entry.Name())
		}
	}
	sort.Strings(v.Variants)
	return v, nil
}
func envDBVars(p *envProject, content string) ([][2]string, error) {
	h := p.hint
	if h == nil {
		return nil, &AppError{Code: "NO_DB_BINDING", Message: "该站点没有绑定数据库"}
	}
	port := strconv.Itoa(h.Port)
	vars := [][2]string{}
	switch p.syntax {
	case "thinkphp":
		vars = [][2]string{{"DB_TYPE", "mysql"}, {"DB_HOST", "127.0.0.1"}, {"DB_PORT", port}, {"DB_NAME", h.Database}, {"DB_USER", h.Username}, {"DB_PASS", h.Password}, {"DB_CHARSET", "utf8mb4"}}
	case "codeigniter":
		vars = [][2]string{{"database.default.hostname", "127.0.0.1"}, {"database.default.port", port}, {"database.default.database", h.Database}, {"database.default.username", h.Username}, {"database.default.password", h.Password}, {"database.default.DBDriver", "MySQLi"}}
	default:
		if rootFile(p.root, filepath.Join("bin", "console")) && rootFile(p.root, filepath.Join("config", "bundles.php")) {
			u := &url.URL{Scheme: "mysql", Host: "127.0.0.1"}
			for _, r := range parseEnv(content, "dotenv") {
				if r.entry.Key == "DATABASE_URL" && !r.entry.Commented {
					existing, err := url.Parse(r.entry.Value)
					if err == nil && existing.Scheme == "mysql" {
						u = existing
					}
					break
				}
			}
			password := h.Password
			if password == "" {
				var known bool
				if u.User != nil {
					password, known = u.User.Password()
				}
				if !known {
					return nil, invalid("未保存数据库密码，无法生成完整连接地址")
				}
			}
			u.User = url.UserPassword(h.Username, password)
			u.Host = "127.0.0.1:" + port
			u.Path = "/" + h.Database
			u.RawPath = "/" + url.PathEscape(h.Database)
			if u.RawQuery == "" {
				u.RawQuery = "charset=utf8mb4"
			}
			return [][2]string{{"DATABASE_URL", u.String()}}, nil
		}
		vars = [][2]string{{"DB_CONNECTION", "mysql"}, {"DB_HOST", "127.0.0.1"}, {"DB_PORT", port}, {"DB_DATABASE", h.Database}, {"DB_USERNAME", h.Username}, {"DB_PASSWORD", h.Password}}
	}
	if h.Password == "" {
		out := [][2]string{}
		for _, v := range vars {
			if v[0] != "DB_PASSWORD" && v[0] != "DB_PASS" && v[0] != "database.default.password" {
				out = append(out, v)
			}
		}
		vars = out
	}
	return vars, nil
}

func (c *Core) envCommand(ctx context.Context, data json.RawMessage, action string) (any, error) {
	in, err := decode[envRequest](data)
	if err != nil {
		return nil, err
	}
	if in.FileName == "" {
		in.FileName = ".env"
	}
	if !envName(in.FileName) {
		return nil, invalid("请选择项目中的 .env 或 .env.* 文件，不能选择路径、备份或编译缓存")
	}
	if !c.siteChanges.TryLock() {
		return nil, serviceBusy()
	}
	defer c.siteChanges.Unlock()
	p, err := c.envProject(ctx, in.SiteID)
	if err != nil {
		return nil, err
	}
	defer p.root.Close()
	lockDir := filepath.Join(c.Home, "site-locks")
	if err = managedDirectory(c.Home, lockDir); err != nil {
		return nil, err
	}
	digest := sha256.Sum256([]byte(p.root.Name()))
	unlock, err := process.Lock(filepath.Join(lockDir, hex.EncodeToString(digest[:])+".lock"))
	if err != nil {
		return nil, err
	}
	defer unlock()
	content, exists, err := envRead(p.root, in.FileName)
	if err != nil {
		return nil, err
	}
	if action == "read" {
		return c.envView(p, in.FileName, content, exists)
	}
	if action != "applydb" && (in.ExpectedRevision == "" || in.ExpectedRevision != c.envRevision(p, in.FileName, content, exists)) {
		return nil, envChanged()
	}
	if action == "save" && len(in.Changes) == 0 {
		return c.envView(p, in.FileName, content, exists)
	}
	if action == "previewdb" || action == "applydb" {
		vars, err := envDBVars(p, string(content))
		if err != nil {
			return nil, err
		}
		if action == "previewdb" {
			return vars, nil
		}
		in.Changes = vars
	}
	if action == "restorepreview" || action == "restore" {
		backupName := in.FileName + ".nsb-backup"
		backup, present, err := envRead(p.root, backupName)
		if err != nil {
			return nil, err
		}
		if !present {
			return nil, &AppError{Code: "ENV_BACKUP_MISSING", Message: "此环境文件没有上次保存的备份"}
		}
		revision := c.envRevision(p, backupName, backup, true)
		if action == "restorepreview" {
			values := func(data []byte) map[string][]string {
				m := map[string][]string{}
				for _, r := range parseEnv(string(data), p.syntax) {
					if !r.entry.Commented {
						m[r.entry.Key] = append(m[r.entry.Key], r.entry.Value)
					}
				}
				return m
			}
			before, after := values(content), values(backup)
			keys := map[string]bool{}
			for k := range before {
				keys[k] = true
			}
			for k := range after {
				keys[k] = true
			}
			changed := []string{}
			for key := range keys {
				a, _ := json.Marshal(before[key])
				b, _ := json.Marshal(after[key])
				if !bytes.Equal(a, b) {
					changed = append(changed, key)
				}
			}
			sort.Strings(changed)
			return map[string]any{"fileName": in.FileName, "backupPath": Path(filepath.Join(p.root.Name(), backupName)), "revision": revision, "currentExists": exists, "contentChanged": !exists || !bytes.Equal(content, backup), "changedKeys": changed}, nil
		}
		if revision != in.ExpectedBackupRevision {
			return nil, &AppError{Code: "ENV_BACKUP_CHANGED", Message: "备份在预览后发生变化，请重新预览"}
		}
		if exists {
			recovery := in.FileName + ".nsb-before-restore"
			old, oldExists, err := envRead(p.root, recovery)
			if err != nil {
				return nil, err
			}
			if err = replaceEnv(p.root, recovery, old, oldExists, content); err != nil {
				return nil, err
			}
		}
		current, ok, err := envRead(p.root, backupName)
		if err != nil {
			return nil, err
		}
		if !ok || !bytes.Equal(current, backup) {
			return nil, invalid("备份在还原前发生变化")
		}
		if err = replaceEnv(p.root, in.FileName, content, exists, backup); err != nil {
			return nil, err
		}
		return c.envView(p, in.FileName, backup, true)
	}
	next, err := applyEnvChanges(string(content), p.syntax, in.Changes)
	if err != nil {
		return nil, err
	}
	if !exists || next != string(content) {
		if exists {
			backupName := in.FileName + ".nsb-backup"
			old, oldExists, err := envRead(p.root, backupName)
			if err != nil {
				return nil, err
			}
			if err = replaceEnv(p.root, backupName, old, oldExists, content); err != nil {
				return nil, err
			}
		}
		if err = ctx.Err(); err != nil {
			return nil, err
		}
		if err = replaceEnv(p.root, in.FileName, content, exists, []byte(next)); err != nil {
			return nil, err
		}
	}
	if action == "applydb" {
		keys := []string{}
		for _, v := range in.Changes {
			keys = append(keys, v[0])
		}
		return keys, nil
	}
	return c.envView(p, in.FileName, []byte(next), true)
}
func (c *Core) registerEnv() {
	c.handle("env_read", func(ctx context.Context, data json.RawMessage) (any, error) { return c.envCommand(ctx, data, "read") })
	c.handle("env_save", func(ctx context.Context, data json.RawMessage) (any, error) { return c.envCommand(ctx, data, "save") })
	c.handle("env_preview_db", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.envCommand(ctx, data, "previewdb")
	})
	c.handle("env_apply_db", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.envCommand(ctx, data, "applydb")
	})
	c.handle("env_restore_preview", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.envCommand(ctx, data, "restorepreview")
	})
	c.handle("env_restore", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.envCommand(ctx, data, "restore")
	})
}
