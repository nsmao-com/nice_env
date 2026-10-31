package core

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strconv"
	"strings"
	"unicode"

	"niceenv/internal/process"
)

type projectDetection struct {
	ID              string   `json:"id"`
	Files           []string `json:"files"`
	Requirements    []string `json:"requirements"`
	ResolvedVersion *string  `json:"resolvedVersion"`
	Issue           *string  `json:"issue"`
}
type projectOption struct {
	ID       string   `json:"id"`
	Label    string   `json:"label"`
	Versions []string `json:"versions"`
}
type projectVersionsView struct {
	Path        Path               `json:"path"`
	Exists      bool               `json:"exists"`
	Revision    string             `json:"revision"`
	Versions    map[string]string  `json:"versions"`
	Options     []projectOption    `json:"options"`
	SharedSites []string           `json:"sharedSites"`
	Detected    []projectDetection `json:"detected"`
	PHPVersion  *string            `json:"phpVersion"`
}
type projectVersionFile struct {
	Content []byte `json:"content"`
	Exists  bool   `json:"exists"`
	Error   string `json:"error"`
}

func runtimeLabel(p Package) string {
	switch p.ID {
	case "php":
		return "PHP"
	case "node":
		return "Node.js"
	case "python":
		return "Python"
	case "go":
		return "Go"
	}
	if p.DisplayName != "" {
		return p.DisplayName
	}
	return p.ID
}
func runtimeCLI(p Package) (string, error) {
	entry := strings.ReplaceAll(p.Entry, "\\", "/")
	if entry == "" {
		return "", invalid("没有命令行入口")
	}
	parent := filepath.Dir(entry)
	if p.ID == "php" {
		name := "php"
		if runtime.GOOS == "windows" {
			name += ".exe"
		} else if filepath.Base(parent) == "sbin" {
			parent = filepath.Join(filepath.Dir(parent), "bin")
		}
		entry = filepath.Join(parent, name)
	}
	if (p.ID == "npm" || p.ID == "pnpm") && entry != "package/pnpm" && entry != "package/pnpm.exe" {
		name := p.ID
		if runtime.GOOS == "windows" {
			name += ".cmd"
		}
		entry = filepath.Join(parent, name)
	}
	switch strings.ToLower(filepath.Ext(entry)) {
	case ".jar", ".phar", ".php", ".html", ".json", ".txt", ".md", ".toml", ".yaml", ".yml", ".ini":
		return "", invalid("此套件没有可直接执行的命令行入口")
	}
	return entry, nil
}
func runtimeDirectory(p Package) (string, error) {
	entry, err := runtimeCLI(p)
	if err != nil {
		return "", err
	}
	if p.Install == nil || !filepath.IsAbs(p.Install.InstallPath) {
		return "", invalid("运行时未安装或安装路径无效")
	}
	root, err := filepath.EvalSymlinks(DisplayPath(p.Install.InstallPath))
	if err != nil {
		return "", err
	}
	path, err := relativePath(root, entry)
	if err != nil {
		return "", err
	}
	actual, err := filepath.EvalSymlinks(path)
	if err != nil {
		return "", err
	}
	rel, err := filepath.Rel(root, actual)
	if err != nil || !filepath.IsLocal(rel) {
		return "", invalid("运行时入口指向安装目录之外")
	}
	info, err := os.Stat(actual)
	if err != nil {
		return "", err
	}
	if !info.Mode().IsRegular() {
		return "", invalid("运行时入口不是普通文件")
	}
	if runtime.GOOS != "windows" && info.Mode().Perm()&0111 == 0 {
		return "", invalid("运行时入口没有执行权限")
	}
	dir := filepath.Dir(actual)
	separator := ':'
	if runtime.GOOS == "windows" {
		separator = ';'
	}
	if strings.ContainsRune(dir, separator) || strings.IndexFunc(dir, unicode.IsControl) >= 0 {
		return "", invalid("运行时目录包含 PATH 分隔符或控制字符")
	}
	return DisplayPath(dir), nil
}
func (c *Core) installedRuntimePackages(ctx context.Context) ([]Package, error) {
	views, err := c.packageViews(ctx)
	if err != nil {
		return nil, err
	}
	data, err := json.Marshal(views)
	if err != nil {
		return nil, err
	}
	var packages []Package
	if err = json.Unmarshal(data, &packages); err != nil {
		return nil, err
	}
	out := []Package{}
	for _, p := range packages {
		if p.Install != nil {
			out = append(out, p)
		}
	}
	return out, nil
}
func projectDocument(content []byte, exists bool) (map[string]json.RawMessage, map[string]string, error) {
	doc := map[string]json.RawMessage{"schemaVersion": json.RawMessage("1"), "runtimes": json.RawMessage("{}")}
	if exists {
		if err := json.Unmarshal(bytes.TrimPrefix(content, []byte("\ufeff")), &doc); err != nil || doc == nil {
			return nil, nil, invalid(".niceenv.json 格式无效，未覆盖原文件")
		}
	}
	var schema int
	if json.Unmarshal(doc["schemaVersion"], &schema) != nil || schema != 1 {
		return nil, nil, invalid("当前只支持 schemaVersion 为 1 的项目配置")
	}
	versions := map[string]string{}
	if raw, ok := doc["runtimes"]; ok {
		if json.Unmarshal(raw, &versions) != nil || versions == nil {
			return nil, nil, invalid("项目版本必须由运行时名称和版本号组成")
		}
	}
	if len(versions) > 128 {
		return nil, nil, invalid("项目版本选择过多")
	}
	for id, version := range versions {
		if !safePart(id) || !safePart(version) {
			return nil, nil, invalid("项目运行时标识或版本无效")
		}
	}
	return doc, versions, nil
}
func numericRuntimeVersion(version string, node bool) ([]uint64, bool) {
	if node {
		version = strings.TrimPrefix(version, "v")
	}
	parts := strings.Split(version, ".")
	if len(parts) < 1 || len(parts) > 3 {
		return nil, false
	}
	out := []uint64{}
	for _, part := range parts {
		if part == "" || len(part) > 1 && part[0] == '0' {
			return nil, false
		}
		for _, ch := range part {
			if ch < '0' || ch > '9' {
				return nil, false
			}
		}
		value, err := strconv.ParseUint(part, 10, 64)
		if err != nil {
			return nil, false
		}
		out = append(out, value)
	}
	return out, true
}
func (c *Core) runtimeDetections(ctx context.Context, files map[string]projectVersionFile, packages []Package) ([]projectDetection, error) {
	out := []projectDetection{}
	for _, group := range []struct {
		id    string
		names []string
	}{{"node", []string{".nvmrc", ".node-version"}}, {"python", []string{".python-version"}}} {
		d := projectDetection{ID: group.id, Files: []string{}, Requirements: []string{}}
		constraints := [][]uint64{}
		issue := func(text string) {
			if d.Issue == nil {
				d.Issue = &text
			}
		}
		for _, name := range group.names {
			file := files[name]
			if !file.Exists && file.Error == "" {
				continue
			}
			d.Files = append(d.Files, name)
			if file.Error != "" {
				issue(name + "：" + file.Error)
				continue
			}
			lines := []string{}
			for _, line := range strings.Split(strings.TrimPrefix(string(file.Content), "\ufeff"), "\n") {
				body, _, _ := strings.Cut(line, "#")
				body = strings.TrimSpace(body)
				if body != "" && !(name == ".nvmrc" && strings.Contains(body, "=")) {
					lines = append(lines, body)
				}
			}
			if len(lines) != 1 || len(strings.Fields(lines[0])) != 1 || len(lines[0]) > 128 || strings.Contains(lines[0], ":") {
				issue(name + " 需要单个版本号，请在项目版本页明确选择版本")
				continue
			}
			value := lines[0]
			d.Requirements = append(d.Requirements, name+": "+value)
			if name == ".nvmrc" && (value == "node" || value == "stable") {
				continue
			}
			if name == ".nvmrc" && strings.HasPrefix(value, "lts/") {
				raw, _, err := c.Store.setting(ctx, "nodeLtsAliases:v1")
				if err != nil {
					return nil, err
				}
				var aliases map[string]string
				if json.Unmarshal([]byte(raw), &aliases) != nil {
					issue("尚无可用 Node.js LTS 信息，请先刷新 Node.js 版本目录")
					continue
				}
				resolved, ok := aliases[strings.ToLower(strings.TrimPrefix(value, "lts/"))]
				if !ok {
					issue("官方缓存中没有此 LTS 代号，请刷新版本目录")
					continue
				}
				d.Requirements[len(d.Requirements)-1] += " → " + resolved
				value = resolved
			}
			parts, ok := numericRuntimeVersion(value, group.id == "node")
			if !ok {
				issue(name + " 中的版本写法无法解析，请选择已安装版本")
				continue
			}
			constraints = append(constraints, parts)
		}
		if len(d.Files) == 0 {
			continue
		}
		if d.Issue == nil {
			for i := 1; i < len(constraints); i++ {
				n := min(len(constraints[0]), len(constraints[i]))
				for j := 0; j < n; j++ {
					if constraints[0][j] != constraints[i][j] {
						issue(".nvmrc 与 .node-version 的版本要求冲突")
					}
				}
			}
			if d.Issue == nil {
				for _, p := range packages {
					if p.ID != group.id || p.Category != "runtime" {
						continue
					}
					parts, ok := numericRuntimeVersion(p.Version, group.id == "node")
					if !ok || len(parts) != 3 {
						continue
					}
					matches := true
					for _, constraint := range constraints {
						for i, v := range constraint {
							matches = matches && parts[i] == v
						}
					}
					if matches && (d.ResolvedVersion == nil || newerVersion(p.Version, *d.ResolvedVersion)) {
						v := p.Version
						d.ResolvedVersion = &v
					}
				}
				if d.ResolvedVersion == nil {
					issue("没有符合项目文件要求的已安装版本，请先安装或明确选择版本")
				}
			}
		}
		out = append(out, d)
	}
	return out, nil
}
func projectRuntimeFiles(root *os.Root) map[string]projectVersionFile {
	out := map[string]projectVersionFile{}
	for _, name := range []string{".nvmrc", ".node-version", ".python-version"} {
		data, exists, err := envRead(root, name)
		v := projectVersionFile{Content: data, Exists: exists}
		if err != nil {
			v.Error = publicError(err).Message
		}
		out[name] = v
	}
	return out
}
func (c *Core) runtimeProjectView(ctx context.Context, p *envProject, content []byte, exists bool, files map[string]projectVersionFile, packages []Package) (projectVersionsView, error) {
	_, versions, err := projectDocument(content, exists)
	if err != nil {
		return projectVersionsView{}, err
	}
	detected, err := c.runtimeDetections(ctx, files, packages)
	if err != nil {
		return projectVersionsView{}, err
	}
	v := projectVersionsView{Path: Path(filepath.Join(p.root.Name(), ".niceenv.json")), Exists: exists, Versions: versions, Options: []projectOption{}, SharedSites: []string{}, Detected: detected}
	options := map[string]*projectOption{}
	for _, pkg := range packages {
		if pkg.Category != "runtime" {
			continue
		}
		if _, err := runtimeCLI(pkg); err != nil {
			continue
		}
		option := options[pkg.ID]
		if option == nil {
			option = &projectOption{ID: pkg.ID, Label: runtimeLabel(pkg), Versions: []string{}}
			options[pkg.ID] = option
		}
		option.Versions = append(option.Versions, pkg.Version)
	}
	for id := range versions {
		if options[id] == nil {
			options[id] = &projectOption{ID: id, Label: runtimeLabel(Package{ID: id}), Versions: []string{}}
		}
	}
	for _, d := range detected {
		if options[d.ID] == nil {
			options[d.ID] = &projectOption{ID: d.ID, Label: runtimeLabel(Package{ID: d.ID}), Versions: []string{}}
		}
	}
	for _, option := range options {
		sort.Slice(option.Versions, func(i, j int) bool { return newerVersion(option.Versions[i], option.Versions[j]) })
		v.Options = append(v.Options, *option)
	}
	sort.Slice(v.Options, func(i, j int) bool { return v.Options[i].ID < v.Options[j].ID })
	rows, err := c.Store.db.QueryContext(ctx, "SELECT id,name,root_dir FROM sites WHERE id<>?", p.id)
	if err != nil {
		return v, err
	}
	for rows.Next() {
		var id, name, root string
		if err = rows.Scan(&id, &name, &root); err != nil {
			rows.Close()
			return v, err
		}
		other, err := filepath.EvalSymlinks(projectRoot(root))
		if err == nil {
			rel, err := filepath.Rel(p.root.Name(), other)
			if err == nil && rel == "." {
				v.SharedSites = append(v.SharedSites, name)
			}
		}
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return v, err
	}
	sort.Strings(v.SharedSites)
	var config struct {
		Kind       string  `json:"kind"`
		PHPVersion *string `json:"phpVersion"`
	}
	if json.Unmarshal([]byte(p.runtime), &config) != nil {
		return v, invalid("站点运行时配置无效")
	}
	if config.Kind == "php" {
		v.PHPVersion = config.PHPVersion
	}
	payload, _ := json.Marshal([]any{p.id, p.siteRoot, p.root.Name(), p.runtime, content, exists, files, detected, v.Options})
	digest := sha256.Sum256(payload)
	v.Revision = hex.EncodeToString(digest[:])
	return v, nil
}
func effectiveProjectVersions(versions map[string]string, detected []projectDetection) (map[string]string, map[string]string, error) {
	effective := map[string]string{}
	sources := map[string]string{}
	for id, version := range versions {
		effective[id] = version
		sources[id] = ".niceenv.json"
	}
	for _, d := range detected {
		if _, ok := effective[d.ID]; ok {
			continue
		}
		if d.ResolvedVersion == nil {
			message := "无法确定项目运行时版本"
			if d.Issue != nil {
				message = *d.Issue
			}
			return nil, nil, &AppError{Code: "PROJECT_RUNTIME_DETECTION", Message: message, Hint: "请修正版本文件或在项目版本页明确选择已安装版本。"}
		}
		effective[d.ID] = *d.ResolvedVersion
		sources[d.ID] = strings.Join(d.Files, " + ")
	}
	return effective, sources, nil
}
func validateRuntimeVersions(versions map[string]string, packages []Package) error {
	if len(versions) > 128 {
		return invalid("项目运行时选择超过限制")
	}
	for id, version := range versions {
		if !safePart(id) || !safePart(version) {
			return invalid("运行时标识或版本无效")
		}
		found := false
		for _, p := range packages {
			if p.ID == id && p.Category == "runtime" && canonicalVersion(p.Version) == canonicalVersion(version) {
				if _, err := runtimeDirectory(p); err != nil {
					return &AppError{Code: "TERMINAL_RUNTIME_UNAVAILABLE", Message: id + " " + version + " 的命令行入口不可用", Detail: publicError(err).Message}
				}
				found = true
				break
			}
		}
		if !found {
			return &AppError{Code: "TERMINAL_RUNTIME_UNAVAILABLE", Message: id + " " + version + " 尚未安装或不是可用运行时"}
		}
	}
	return nil
}
func (c *Core) projectRuntimeCommand(ctx context.Context, data json.RawMessage, save bool) (any, error) {
	in, err := decode[struct {
		SiteID           string            `json:"siteId"`
		Versions         map[string]string `json:"versions"`
		ExpectedRevision string            `json:"expectedRevision"`
	}](data)
	if err != nil {
		return nil, err
	}
	if !c.siteChanges.TryLock() {
		return nil, serviceBusy()
	}
	defer c.siteChanges.Unlock()
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	p, err := c.openProject(ctx, in.SiteID, false)
	if err != nil {
		return nil, err
	}
	defer p.root.Close()
	dir := filepath.Join(c.Home, "site-locks")
	if err = managedDirectory(c.Home, dir); err != nil {
		return nil, err
	}
	hash := sha256.Sum256([]byte(p.root.Name()))
	unlock, err := process.Lock(filepath.Join(dir, hex.EncodeToString(hash[:])+".lock"))
	if err != nil {
		return nil, err
	}
	defer unlock()
	packages, err := c.installedRuntimePackages(ctx)
	if err != nil {
		return nil, err
	}
	content, exists, err := envRead(p.root, ".niceenv.json")
	if err != nil {
		return nil, err
	}
	files := projectRuntimeFiles(p.root)
	view, err := c.runtimeProjectView(ctx, p, content, exists, files, packages)
	if err != nil {
		return nil, err
	}
	if !save {
		return view, nil
	}
	if view.Revision != in.ExpectedRevision {
		return nil, &AppError{Code: "PROJECT_RUNTIME_CHANGED", Message: "项目文件、目录或自动匹配版本已变化，请重新读取"}
	}
	if in.Versions == nil {
		return nil, invalid("缺少项目版本选择")
	}
	effective, _, err := effectiveProjectVersions(in.Versions, view.Detected)
	if err != nil {
		return nil, err
	}
	if err = validateRuntimeVersions(effective, packages); err != nil {
		return nil, err
	}
	doc, _, err := projectDocument(content, exists)
	if err != nil {
		return nil, err
	}
	doc["runtimes"], err = json.Marshal(in.Versions)
	if err != nil {
		return nil, err
	}
	next, err := json.MarshalIndent(doc, "", "  ")
	if err != nil {
		return nil, err
	}
	next = append(next, '\n')
	if len(next) > siteTextLimit {
		return nil, invalid("项目版本文件超过 1 MiB")
	}
	current, err := c.runtimeProjectView(ctx, p, content, exists, projectRuntimeFiles(p.root), packages)
	if err != nil {
		return nil, err
	}
	if current.Revision != view.Revision {
		return nil, envChanged()
	}
	if exists {
		old, backupExists, err := envRead(p.root, ".niceenv.json.nsb-backup")
		if err != nil {
			return nil, err
		}
		if err = replaceEnv(p.root, ".niceenv.json.nsb-backup", old, backupExists, content); err != nil {
			return nil, err
		}
	}
	if err = replaceEnv(p.root, ".niceenv.json", content, exists, next); err != nil {
		return nil, err
	}
	return c.runtimeProjectView(ctx, p, next, true, files, packages)
}
func (c *Core) registerProjectRuntime() {
	c.handle("project_runtime_versions", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.projectRuntimeCommand(ctx, data, false)
	})
	c.handle("save_project_runtime_versions", func(ctx context.Context, data json.RawMessage) (any, error) {
		return c.projectRuntimeCommand(ctx, data, true)
	})
}
