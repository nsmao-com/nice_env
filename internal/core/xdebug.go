package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"slices"
	"sort"
	"strconv"
	"strings"
	"time"
)

type phpBuild struct {
	PHPVersion string `json:"phpVersion"`
	API        string `json:"api"`
	TS         bool   `json:"ts"`
	Compiler   string `json:"compiler"`
	Arch       string `json:"arch"`
}
type xdebugView struct {
	Version       string            `json:"version"`
	Build         *phpBuild         `json:"build"`
	DLLPresent    bool              `json:"dllPresent"`
	Enabled       bool              `json:"enabled"`
	Loaded        bool              `json:"loaded"`
	LoadedVersion *string           `json:"loadedVersion"`
	Recommended   string            `json:"recommended"`
	DLLCandidates []string          `json:"dllCandidates"`
	ManualHint    string            `json:"manualHint"`
	Settings      map[string]string `json:"settings"`
}

func phpCLIForEntry(e *serviceEntry) (string, error) {
	dir, err := runtimeDirectory(e.pkg)
	if err != nil {
		return "", err
	}
	entry, err := runtimeCLI(e.pkg)
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, filepath.Base(entry)), nil
}
func detectPHPBuild(ctx context.Context, program string) (phpBuild, error) {
	code, out, _, err := runPHPCheck(ctx, phpCheckSpec(program, filepath.Dir(program), []string{"-n", "-i"}), 15*time.Second)
	if err != nil {
		return phpBuild{}, err
	}
	if code != 0 {
		return phpBuild{}, invalid("PHP 无法读取构建信息")
	}
	values := map[string]string{}
	for _, line := range strings.Split(out, "\n") {
		key, value, ok := strings.Cut(line, "=>")
		if ok {
			values[strings.TrimSpace(key)] = strings.TrimSpace(value)
		}
	}
	b := phpBuild{PHPVersion: values["PHP Version"], Arch: values["Architecture"], TS: values["Thread Safety"] == "enabled"}
	for _, part := range strings.Split(values["PHP Extension Build"], ",") {
		part = strings.TrimSpace(part)
		if strings.HasPrefix(part, "API") {
			b.API = strings.TrimPrefix(part, "API")
		}
		if strings.HasPrefix(part, "VS") {
			b.Compiler = part
		}
	}
	if b.Compiler == "" {
		for _, pair := range []struct{ year, vs string }{{"2022", "VS17"}, {"2019", "VS16"}, {"2017", "VS15"}, {"2015", "VS14"}} {
			if strings.Contains(values["Compiler"], pair.year) {
				b.Compiler = pair.vs
				break
			}
		}
	}
	if b.PHPVersion == "" || b.API == "" {
		return b, invalid("无法识别 PHP 构建信息")
	}
	return b, nil
}
func xdebugRecommended(version string) string {
	parts := strings.Split(version, ".")
	major, minor := 0, 0
	if len(parts) > 1 {
		major, _ = strconv.Atoi(parts[0])
		minor, _ = strconv.Atoi(parts[1])
	}
	switch {
	case major >= 8:
		return "3.5.3"
	case major == 7 && minor >= 2:
		return "3.1.6"
	default:
		return "2.9.8"
	}
}
func (b phpBuild) candidates(version string) []string {
	parts := strings.Split(b.PHPVersion, ".")
	if len(parts) < 2 {
		return []string{}
	}
	short := strings.Join(parts[:2], ".")
	ts := "nts"
	if b.TS {
		ts = "ts"
	}
	arch := "x86_64"
	if strings.Contains(strings.ToLower(b.Arch), "arm") {
		arch = "aarch64"
	} else if strings.EqualFold(b.Arch, "x86") {
		arch = "x86"
	}
	compiler := strings.ToLower(b.Compiler)
	return []string{fmt.Sprintf("php_xdebug-%s-%s-%s-%s-%s.dll", version, short, ts, compiler, arch), fmt.Sprintf("php_xdebug-%s-%s-%s-%s-%s.dll", version, short, compiler, arch, ts), fmt.Sprintf("php_xdebug-%s-%s-%s-%s.dll", version, short, compiler, ts), fmt.Sprintf("php_xdebug-%s-%s-%s-%s.dll", version, b.API, ts, compiler)}
}
func xdebugSettings(text string) map[string]string {
	out := map[string]string{}
	for _, line := range strings.Split(text, "\n") {
		key, value, ok := strings.Cut(strings.TrimSpace(line), "=")
		key = strings.TrimSpace(key)
		if ok && strings.HasPrefix(key, "xdebug.") {
			out[key] = strings.Trim(strings.TrimSpace(strings.SplitN(value, ";", 2)[0]), `"'`)
		}
	}
	return out
}
func xdebugProbe(ctx context.Context, program, ini, dll string) (*string, error) {
	args := []string{"-n"}
	if ini != "" {
		args = []string{"-c", DisplayPath(ini)}
	}
	if dll != "" {
		args = append(args, "-d", "zend_extension="+DisplayPath(dll))
	}
	args = append(args, "-d", "auto_prepend_file=", "-d", "auto_append_file=", "-d", "opcache.preload=", "-d", "display_errors=stderr", "-d", "log_errors=0", "-r", `echo json_encode(['version'=>extension_loaded('xdebug') ? phpversion('xdebug') : null]);`)
	spec := phpCheckSpec(program, filepath.Dir(program), args)
	spec.RemoveEnvironment = append(spec.RemoveEnvironment, "XDEBUG_CONFIG", "XDEBUG_MODE", "XDEBUG_TRIGGER", "XDEBUG_SESSION")
	code, out, detail, err := runPHPCheck(ctx, spec, 15*time.Second)
	if err != nil {
		return nil, err
	}
	var reply struct {
		Version *string `json:"version"`
	}
	if code != 0 || json.Unmarshal([]byte(out), &reply) != nil {
		return nil, &AppError{Code: "XDEBUG_PROBE_FAILED", Message: "PHP 未能完成 Xdebug 加载检查", Detail: detail}
	}
	if reply.Version != nil && !regexp.MustCompile(`^[0-9]+\.[0-9]+\.[0-9]+(?:[a-zA-Z0-9.-]+)?$`).MatchString(*reply.Version) {
		return nil, invalid("Xdebug 返回无效版本")
	}
	return reply.Version, nil
}
func (c *Core) xdebugStatus(ctx context.Context, version string) (xdebugView, error) {
	view := xdebugView{Version: version, DLLCandidates: []string{}, Settings: map[string]string{}, Recommended: xdebugRecommended(version)}
	e, r, t, err := c.phpTarget(ctx, version)
	if err != nil {
		return view, err
	}
	program, err := phpCLIForEntry(e)
	if err != nil {
		return view, err
	}
	build, err := detectPHPBuild(ctx, program)
	if err != nil {
		return view, err
	}
	view.Build = &build
	view.Recommended = xdebugRecommended(build.PHPVersion)
	view.DLLCandidates = build.candidates(view.Recommended)
	text, err := readConfigText(t.path)
	if err != nil {
		return view, err
	}
	view.Enabled = phpEnabled(text)["xdebug"]
	view.Settings = xdebugSettings(text)
	for _, line := range strings.Split(text, "\n") {
		m := phpExtensionLine.FindStringSubmatch(line)
		if len(m) > 0 && extensionName(m[3]) == "xdebug" {
			path := strings.Trim(strings.TrimSpace(m[3]), `"'`)
			if !filepath.IsAbs(path) {
				path = filepath.Join(filepath.Dir(r.program), "ext", path)
			}
			if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() {
				view.DLLPresent = true
			}
		}
	}
	for _, name := range []string{"php_xdebug.dll", "xdebug.so"} {
		info, err := os.Stat(filepath.Join(filepath.Dir(r.program), "ext", name))
		view.DLLPresent = view.DLLPresent || err == nil && info.Mode().IsRegular()
	}
	view.LoadedVersion, err = xdebugProbe(ctx, program, t.path, "")
	if err != nil {
		return view, err
	}
	view.Loaded = view.LoadedVersion != nil
	if view.LoadedVersion != nil {
		view.DLLPresent = true
	}
	view.ManualHint = fmt.Sprintf("PHP %s · %s · %s：可从 https://xdebug.org/download 下载匹配构建的扩展，然后选择从文件安装。", build.PHPVersion, build.Compiler, build.Arch)
	return view, nil
}
func validateXdebugSettings(mode string, port int) error {
	if port < 1 || port > 65535 {
		return invalid("调试端口须在 1–65535 之间")
	}
	parts := strings.Split(mode, ",")
	if len(parts) > 7 {
		return invalid("Xdebug 模式无效")
	}
	seen := map[string]bool{}
	for _, part := range parts {
		if !slices.Contains([]string{"off", "develop", "coverage", "debug", "gcstats", "profile", "trace"}, part) || seen[part] {
			return invalid("Xdebug 模式无效或重复")
		}
		seen[part] = true
	}
	if seen["off"] && len(parts) > 1 {
		return invalid("off 不能与其它模式同时使用")
	}
	return nil
}
func xdebugINI(before, path, mode string, port int, legacy bool) string {
	lines := []string{}
	section := false
	for _, line := range strings.Split(before, "\n") {
		trim := strings.TrimSpace(line)
		if strings.HasPrefix(trim, "[") && strings.HasSuffix(trim, "]") {
			section = strings.EqualFold(trim, "[xdebug]")
			if section {
				continue
			}
		}
		if section {
			continue
		}
		m := phpExtensionLine.FindStringSubmatch(line)
		if len(m) > 0 && extensionName(m[3]) == "xdebug" {
			if m[1] == "" {
				line = ";" + line
			}
		}
		key, _, ok := strings.Cut(trim, "=")
		if ok && strings.HasPrefix(strings.TrimSpace(key), "xdebug.") {
			line = ";" + line
		}
		lines = append(lines, line)
	}
	sectionText := fmt.Sprintf("\n[xdebug]\nzend_extension=\"%s\"\nxdebug.mode=%s\nxdebug.start_with_request=trigger\nxdebug.client_host=127.0.0.1\nxdebug.client_port=%d\nxdebug.log_level=0\n", filepath.ToSlash(DisplayPath(path)), mode, port)
	if legacy {
		enabled := "0"
		if slices.Contains(strings.Split(mode, ","), "debug") {
			enabled = "1"
		}
		sectionText = fmt.Sprintf("\n[xdebug]\nzend_extension=\"%s\"\nxdebug.remote_enable=%s\nxdebug.remote_autostart=0\nxdebug.remote_host=127.0.0.1\nxdebug.remote_port=%d\n", filepath.ToSlash(DisplayPath(path)), enabled, port)
	}
	return strings.TrimRight(strings.Join(lines, "\n"), "\r\n") + "\n" + sectionText
}
func (c *Core) applyXdebug(ctx context.Context, e *serviceEntry, t configTarget, program, text, before string, enabled bool) ([]string, *string, error) {
	if err := c.writeConfig(ctx, t, text, &before); err != nil {
		return nil, nil, err
	}
	version, probeErr := xdebugProbe(ctx, program, t.path, "")
	if probeErr != nil || (version != nil) != enabled {
		restoreCtx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
		defer cancel()
		restoreErr := c.writeConfig(restoreCtx, t, before, &text)
		detail := "扩展加载状态与设置不一致"
		if probeErr != nil {
			detail = probeErr.Error()
		}
		if restoreErr != nil {
			detail += "；恢复配置失败：" + restoreErr.Error()
		}
		return nil, nil, &AppError{Code: "XDEBUG_LOAD_FAILED", Message: "Xdebug 配置未通过加载检查，已尝试恢复原配置", Detail: detail}
	}
	warnings := []string{}
	c.services.mu.RLock()
	running := e.child != nil
	c.services.mu.RUnlock()
	if running {
		err := c.stopOwned(ctx, e, false)
		if err == nil {
			err = c.startService(ctx, e)
		}
		if err != nil {
			warnings = append(warnings, "配置已保存，PHP 重启失败："+err.Error())
		}
	}
	return warnings, version, nil
}
func (c *Core) registerXdebug() {
	c.handle("xdebug_status", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Version string `json:"version"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		return c.xdebugStatus(ctx, in.Version)
	})
	c.handle("xdebug_setup", c.xdebugSetup)
	c.handle("xdebug_toggle", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Version string `json:"version"`
			Enabled bool   `json:"enabled"`
			Mode    string `json:"mode"`
			Port    int    `json:"port"`
		}](data)
		if err != nil {
			return nil, err
		}
		if err = validateXdebugSettings(in.Mode, in.Port); err != nil {
			return nil, err
		}
		if !c.services.lifecycle.TryLock() {
			return nil, serviceBusy()
		}
		defer c.services.lifecycle.Unlock()
		e, _, t, err := c.phpTarget(ctx, in.Version)
		if err != nil {
			return nil, err
		}
		program, err := phpCLIForEntry(e)
		if err != nil {
			return nil, err
		}
		before, err := readConfigText(t.path)
		if err != nil {
			return nil, err
		}
		text := setPHPExtensionText(before, "xdebug", in.Enabled)
		if in.Enabled {
			path := ""
			for _, line := range strings.Split(text, "\n") {
				m := phpExtensionLine.FindStringSubmatch(line)
				if len(m) > 0 && m[1] == "" && extensionName(m[3]) == "xdebug" {
					path = strings.Trim(strings.TrimSpace(m[3]), `"'`)
					break
				}
			}
			if !filepath.IsAbs(path) {
				name := "php_xdebug.dll"
				if runtime.GOOS != "windows" {
					name = "xdebug.so"
				}
				path = filepath.Join(filepath.Dir(program), "ext", name)
			}
			version, err := xdebugProbe(ctx, program, "", path)
			if err != nil {
				return nil, err
			}
			if version == nil {
				return nil, invalid("扩展文件无法加载，请先配置 Xdebug")
			}
			text = xdebugINI(before, path, in.Mode, in.Port, strings.HasPrefix(*version, "2."))
		}
		warnings, _, err := c.applyXdebug(ctx, e, t, program, text, before, in.Enabled)
		return warnings, err
	})
}
func (c *Core) xdebugSetup(ctx context.Context, data json.RawMessage) (any, error) {
	var in struct {
		Input struct {
			Version    string `json:"version"`
			Mode       string `json:"mode"`
			ClientPort int    `json:"clientPort"`
			DLLPath    string `json:"dllPath"`
		} `json:"input"`
	}
	in.Input.Mode = "debug,develop"
	in.Input.ClientPort = 9003
	if json.Unmarshal(data, &in) != nil {
		return nil, invalid("Xdebug 参数格式无效")
	}
	if err := validateXdebugSettings(in.Input.Mode, in.Input.ClientPort); err != nil {
		return nil, err
	}
	if !c.services.lifecycle.TryLock() {
		return nil, serviceBusy()
	}
	defer c.services.lifecycle.Unlock()
	e, _, t, err := c.phpTarget(ctx, in.Input.Version)
	if err != nil {
		return nil, err
	}
	program, err := phpCLIForEntry(e)
	if err != nil {
		return nil, err
	}
	build, err := detectPHPBuild(ctx, program)
	if err != nil {
		return nil, err
	}
	var bytes []byte
	if in.Input.DLLPath != "" {
		path := DisplayPath(in.Input.DLLPath)
		info, err := os.Lstat(path)
		if err != nil {
			return nil, err
		}
		if !info.Mode().IsRegular() || info.Size() > 32*1024*1024 {
			return nil, invalid("扩展文件必须是不超过 32 MiB 的普通文件")
		}
		f, err := os.Open(path)
		if err != nil {
			return nil, err
		}
		bytes, err = io.ReadAll(io.LimitReader(f, 32*1024*1024+1))
		f.Close()
		if err != nil {
			return nil, err
		}
	} else {
		if runtime.GOOS != "windows" {
			return nil, &AppError{Code: "XDEBUG_NATIVE_FILE", Message: "此平台请选择为当前 PHP 构建的 Xdebug 扩展文件"}
		}
		page, err := upstreamGet(ctx, "https://xdebug.org/download")
		if err != nil {
			return nil, err
		}
		pattern := regexp.MustCompile(`php_xdebug-([0-9]+\.[0-9]+\.[0-9]+)-[A-Za-z0-9_.-]+\.dll`)
		candidates := []string{}
		seen := map[string]bool{}
		for _, m := range pattern.FindAllStringSubmatch(string(page), -1) {
			if !seen[m[0]] && slices.Contains(build.candidates(m[1]), m[0]) {
				seen[m[0]] = true
				candidates = append(candidates, m[0])
			}
		}
		sort.Slice(candidates, func(i, j int) bool {
			a := strings.Split(candidates[i], "-")[1]
			b := strings.Split(candidates[j], "-")[1]
			return newerVersion(a, b)
		})
		if len(candidates) == 0 {
			candidates = build.candidates(xdebugRecommended(build.PHPVersion))
		}
		var downloadErr error
		for _, candidate := range candidates[:min(len(candidates), 5)] {
			bytes, downloadErr = upstreamGet(ctx, "https://xdebug.org/files/"+candidate)
			if downloadErr == nil {
				break
			}
			if ctx.Err() != nil {
				return nil, ctx.Err()
			}
		}
		if downloadErr != nil {
			return nil, &AppError{Code: "XDEBUG_DOWNLOAD", Message: "未能下载匹配 PHP 构建的 Xdebug 扩展", Hint: "可从 xdebug.org/download 下载后选择从文件安装", Detail: downloadErr.Error()}
		}
	}
	valid := len(bytes) >= 4 && len(bytes) <= 32*1024*1024
	if runtime.GOOS == "windows" {
		valid = valid && string(bytes[:2]) == "MZ"
	} else if runtime.GOOS == "darwin" {
		valid = valid && (string(bytes[:4]) == "\xcf\xfa\xed\xfe" || string(bytes[:4]) == "\xca\xfe\xba\xbe" || string(bytes[:4]) == "\xfe\xed\xfa\xcf")
	} else {
		valid = valid && string(bytes[:4]) == "\x7fELF"
	}
	if !valid {
		return nil, invalid("文件不是当前平台可加载的扩展库")
	}
	hash := sha256.Sum256(bytes)
	suffix := ".so"
	if runtime.GOOS == "windows" {
		suffix = ".dll"
	}
	name := "php_xdebug-niceenv-" + hex.EncodeToString(hash[:12]) + suffix
	dir := filepath.Join(filepath.Dir(program), "ext")
	if err = managedDirectory(c.Home, dir); err != nil {
		return nil, err
	}
	root, err := os.OpenRoot(dir)
	if err != nil {
		return nil, err
	}
	defer root.Close()
	target := filepath.Join(dir, name)
	info, err := siteCheck(root, name, true)
	if err != nil {
		return nil, err
	}
	if info == nil {
		f, err := root.OpenFile(name, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
		if err != nil {
			return nil, err
		}
		_, writeErr := f.Write(bytes)
		closeErr := f.Close()
		if writeErr != nil {
			return nil, writeErr
		}
		if closeErr != nil {
			return nil, closeErr
		}
	} else {
		f, err := root.Open(name)
		if err != nil {
			return nil, err
		}
		digest := sha256.New()
		_, copyErr := io.Copy(digest, io.LimitReader(f, 32*1024*1024+1))
		f.Close()
		if copyErr != nil {
			return nil, copyErr
		}
		if hex.EncodeToString(digest.Sum(nil)) != hex.EncodeToString(hash[:]) {
			return nil, invalid("已有 Xdebug 文件校验不一致，未覆盖")
		}
	}
	loaded, err := xdebugProbe(ctx, program, "", target)
	if err != nil {
		return nil, err
	}
	if loaded == nil {
		return nil, invalid("扩展与当前 PHP 构建不兼容，配置未更改")
	}
	before, err := readConfigText(t.path)
	if err != nil {
		return nil, err
	}
	text := xdebugINI(before, target, in.Input.Mode, in.Input.ClientPort, strings.HasPrefix(*loaded, "2."))
	warnings, loaded, err := c.applyXdebug(ctx, e, t, program, text, before, true)
	if err != nil {
		return nil, err
	}
	return map[string]any{"version": in.Input.Version, "installed": true, "dllPath": Path(target), "loadedVersion": loaded, "warnings": warnings, "manualHint": nil}, nil
}
