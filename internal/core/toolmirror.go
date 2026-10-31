package core

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"niceenv/internal/process"
)

type mirrorOption struct {
	ID       string `json:"id"`
	Label    string `json:"label"`
	URL      string `json:"url"`
	Note     string `json:"note"`
	Official bool   `json:"official"`
}

var toolMirrorOptions = map[string][]mirrorOption{
	"npm":      {{"official", "npm 官方", "https://registry.npmjs.org", "官方源", true}, {"npmmirror", "淘宝 npmmirror", "https://registry.npmmirror.com", "国内镜像", false}, {"tencent", "腾讯云", "https://mirrors.cloud.tencent.com/npm/", "国内镜像", false}, {"huawei", "华为云", "https://repo.huaweicloud.com/repository/npm/", "国内镜像", false}},
	"composer": {{"official", "Packagist 官方", "https://repo.packagist.org", "官方源", true}, {"aliyun", "阿里云", "https://mirrors.aliyun.com/composer/", "国内镜像", false}, {"tencent", "腾讯云", "https://mirrors.cloud.tencent.com/composer/", "国内镜像", false}, {"huawei", "华为云", "https://repo.huaweicloud.com/repository/php/", "国内镜像", false}, {"cnpkg", "cnpkg.org", "https://mirrors.cnpkg.org/composer/", "社区镜像", false}},
	"pip":      {{"official", "PyPI 官方", "https://pypi.org/simple", "官方源", true}, {"tsinghua", "清华 TUNA", "https://pypi.tuna.tsinghua.edu.cn/simple", "国内镜像", false}, {"aliyun", "阿里云", "https://mirrors.aliyun.com/pypi/simple/", "国内镜像", false}, {"ustc", "中科大", "https://mirrors.ustc.edu.cn/pypi/simple/", "国内镜像", false}},
}

func toolMirrorPath(manager string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	config, err := os.UserConfigDir()
	if err != nil {
		return "", err
	}
	var path string
	switch manager {
	case "npm":
		path = os.Getenv("NPM_CONFIG_USERCONFIG")
		if path == "" {
			path = filepath.Join(home, ".npmrc")
		}
	case "composer":
		base := os.Getenv("COMPOSER_HOME")
		if base == "" {
			if runtime.GOOS == "windows" {
				base = filepath.Join(config, "Composer")
			} else {
				base = filepath.Join(home, ".composer")
			}
		}
		path = filepath.Join(base, "config.json")
	case "pip":
		path = os.Getenv("PIP_CONFIG_FILE")
		if path == "" {
			name := "pip.conf"
			if runtime.GOOS == "windows" {
				name = "pip.ini"
			}
			path = filepath.Join(config, "pip", name)
		}
	default:
		return "", invalid("不支持的包管理器")
	}
	if !filepath.IsAbs(path) || strings.EqualFold(path, os.DevNull) {
		return "", invalid("包管理器配置路径必须是普通文件的绝对路径")
	}
	return filepath.Clean(DisplayPath(path)), nil
}

func mirrorConfigRead(path string) (string, bool, error) {
	// Global configuration may contain credentials: reject redirects and retain
	// the complete original file in a private backup before changing one key.
	for part := path; ; part = filepath.Dir(part) {
		info, err := os.Lstat(part)
		if err == nil && info.Mode()&os.ModeSymlink != 0 {
			return "", false, invalid("配置路径包含符号链接，请直接编辑实际配置文件")
		}
		if err != nil && !os.IsNotExist(err) {
			return "", false, err
		}
		if filepath.Dir(part) == part {
			break
		}
	}
	text, err := readConfigText(path)
	if os.IsNotExist(err) {
		return "", false, nil
	}
	return text, err == nil, err
}

func mirrorINI(content, manager, next string, write bool) (string, string) {
	newline := "\n"
	if strings.Contains(content, "\r\n") {
		newline = "\r\n"
	}
	lines := strings.Split(strings.TrimSuffix(strings.ReplaceAll(content, "\r\n", "\n"), "\n"), "\n")
	if content == "" {
		lines = nil
	}
	key, section := "registry", manager == "npm"
	if manager == "pip" {
		key = "index-url"
	}
	current, replaced, foundGlobal := "", false, false
	out := []string{}
	for _, line := range lines {
		t := strings.TrimSpace(line)
		if manager == "pip" && strings.HasPrefix(t, "[") {
			if section && write && !replaced {
				out = append(out, key+"="+next)
				replaced = true
			}
			section = strings.EqualFold(t, "[global]")
			foundGlobal = foundGlobal || section
		}
		k, v, ok := strings.Cut(t, "=")
		match := section && ok && !strings.HasPrefix(t, "#") && !strings.HasPrefix(t, ";") && strings.EqualFold(strings.TrimSpace(k), key)
		if match {
			current = strings.Trim(strings.TrimSpace(v), "\"")
			if write {
				if !replaced {
					out = append(out, key+"="+next)
					replaced = true
				}
				continue
			}
		}
		out = append(out, line)
	}
	if write && !replaced {
		if manager == "pip" && !foundGlobal {
			out = append(out, "[global]")
		}
		out = append(out, key+"="+next)
	}
	return current, strings.Join(out, newline) + newline
}

func mirrorComposer(content, next string, write, reset bool) (string, string, error) {
	value := map[string]any{}
	if strings.TrimSpace(content) != "" {
		decoder := json.NewDecoder(strings.NewReader(content))
		decoder.UseNumber()
		if err := decoder.Decode(&value); err != nil || value == nil || !json.Valid([]byte(content)) {
			return "", "", invalid("Composer 配置不是有效的 JSON 对象，未覆盖原文件")
		}
	}
	repos, ok := value["repositories"].(map[string]any)
	if _, exists := value["repositories"]; exists && !ok {
		return "", "", invalid("Composer repositories 使用列表格式，请先在 Composer 中整理配置；原内容未修改")
	}
	if repos == nil {
		repos = map[string]any{}
	}
	current := ""
	if pack, ok := repos["packagist"].(map[string]any); ok {
		current, _ = pack["url"].(string)
	}
	if current == "" {
		if cfg, ok := value["config"].(map[string]any); ok {
			if repo, ok := cfg["repo"].(map[string]any); ok {
				current, _ = repo["packagist"].(string)
			}
		}
	}
	if !write {
		return current, content, nil
	}
	if reset {
		delete(repos, "packagist")
		delete(repos, "packagist.org")
		if cfg, ok := value["config"].(map[string]any); ok {
			if repo, ok := cfg["repo"].(map[string]any); ok {
				delete(repo, "packagist")
			}
		}
	} else {
		repos["packagist"] = map[string]string{"type": "composer", "url": next}
	}
	if len(repos) > 0 {
		value["repositories"] = repos
	} else {
		delete(value, "repositories")
	}
	encoded, err := json.MarshalIndent(value, "", "  ")
	return current, string(encoded) + "\n", err
}

func toolMirrorStatus(manager string) (any, error) {
	path, err := toolMirrorPath(manager)
	if err != nil {
		return nil, err
	}
	content, _, err := mirrorConfigRead(path)
	if err != nil {
		return nil, err
	}
	current := ""
	if manager == "composer" {
		current, _, err = mirrorComposer(content, "", false, false)
	} else {
		current, _ = mirrorINI(content, manager, "", false)
	}
	if err != nil {
		return nil, err
	}
	matched := ""
	for _, option := range toolMirrorOptions[manager] {
		if strings.EqualFold(strings.TrimRight(current, "/"), strings.TrimRight(option.URL, "/")) {
			matched = option.ID
		}
	}
	return map[string]any{"manager": manager, "current": current, "matched": matched, "available": true, "configPath": Path(path), "options": toolMirrorOptions[manager]}, nil
}

func (c *Core) changeToolMirror(ctx context.Context, manager, address string, reset bool) error {
	path, err := toolMirrorPath(manager)
	if err != nil {
		return err
	}
	if reset {
		address = toolMirrorOptions[manager][0].URL
	}
	address = strings.TrimSpace(address)
	parsed, err := url.Parse(address)
	if err != nil || parsed.Hostname() == "" || (parsed.Scheme != "http" && parsed.Scheme != "https") || parsed.User != nil || parsed.Fragment != "" || strings.ContainsAny(address, "\r\n\x00\"' ") {
		return invalid("镜像地址必须是有效的 HTTP/HTTPS URL，不能包含凭据、空白或配置控制字符")
	}
	dir := filepath.Join(c.Home, "config-locks")
	if err = managedDirectory(c.Home, dir); err != nil {
		return err
	}
	hash := sha256.Sum256([]byte(strings.ToLower(path)))
	unlock, err := process.Lock(filepath.Join(dir, "toolmirror-"+hex.EncodeToString(hash[:])+".lock"))
	if err != nil {
		return &AppError{Code: "CONFIG_BUSY", Message: "镜像配置正在被其他实例修改", Detail: err.Error()}
	}
	defer unlock()
	before, exists, err := mirrorConfigRead(path)
	if err != nil {
		return err
	}
	var next string
	if manager == "composer" {
		_, next, err = mirrorComposer(before, address, true, reset)
	} else {
		_, next = mirrorINI(before, manager, address, true)
	}
	if err != nil {
		return err
	}
	if before == next {
		return nil
	}
	if exists {
		backupDir := filepath.Join(c.Home, "backup", "tool-mirrors")
		if err = managedDirectory(c.Home, backupDir); err != nil {
			return err
		}
		file, err := os.CreateTemp(backupDir, manager+"-"+time.Now().UTC().Format("20060102150405")+"-*.bak")
		if err != nil {
			return err
		}
		_, writeErr := file.WriteString(before)
		syncErr := file.Sync()
		closeErr := file.Close()
		if err = errors.Join(writeErr, syncErr, closeErr); err != nil {
			return err
		}
	}
	if err = os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return err
	}
	current, currentExists, err := mirrorConfigRead(path)
	if err != nil {
		return err
	}
	if currentExists != exists || current != before {
		return &AppError{Code: "CONFIG_CONFLICT", Message: "镜像配置已被其他程序修改，请重试"}
	}
	if err = ctx.Err(); err != nil {
		return err
	}
	return atomicWrite(path, []byte(next))
}

func (c *Core) registerToolMirrors() {
	c.handle("tool_mirrors", func(ctx context.Context, _ json.RawMessage) (any, error) {
		out := []any{}
		for _, manager := range []string{"composer", "npm", "pip"} {
			if err := ctx.Err(); err != nil {
				return nil, err
			}
			status, err := toolMirrorStatus(manager)
			if err != nil {
				return nil, err
			}
			out = append(out, status)
		}
		return out, nil
	})
	c.handle("tool_mirror_set", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Manager string `json:"manager"`
			URL     string `json:"url"`
		}](data)
		if err != nil {
			return nil, err
		}
		return true, c.changeToolMirror(ctx, in.Manager, in.URL, false)
	})
	c.handle("tool_mirror_reset", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			Manager string `json:"manager"`
		}](data)
		if err != nil {
			return nil, err
		}
		return true, c.changeToolMirror(ctx, in.Manager, "", true)
	})
}
