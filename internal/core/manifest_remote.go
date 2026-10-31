package core

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strings"
	"time"

	"niceenv/manifest"
)

const manifestLimit = 8 * 1024 * 1024

func parseRemoteManifest(data []byte) (Catalog, error) {
	var catalog Catalog
	if len(data) > manifestLimit || json.Unmarshal(data, &catalog) != nil || catalog.Revision < 0 || catalog.Packages == nil || len(catalog.Packages) > 10000 {
		return catalog, invalid("套件清单格式无效或超过大小限制")
	}
	seen := map[string]bool{}
	for _, entry := range catalog.Packages {
		id, _ := entry["id"].(string)
		version, _ := entry["version"].(string)
		if !safePart(id) || !safePart(version) || compatible(entry) && seen[packageKey(entry)] {
			return catalog, invalid("套件标识或版本无效，或清单存在重复条目")
		}
		if compatible(entry) {
			seen[packageKey(entry)] = true
		}
		for _, field := range []string{"displayName", "description", "category", "kind", "url", "entry"} {
			if _, ok := entry[field].(string); !ok {
				return catalog, invalid("套件清单缺少文本字段：" + field)
			}
		}
		if !slices.Contains([]string{"archive", "binary", "targz", "sevenzip"}, entry["kind"].(string)) {
			return catalog, invalid("套件压缩格式无效")
		}
		if !slices.Contains([]string{"web-server", "runtime", "database", "cache", "tool", "ai-coding", "ai", "container", "tunnel", "service-mesh", "mail", "dns", "ftp", "search", "object-storage", "other"}, entry["category"].(string)) {
			return catalog, invalid("套件分类无效")
		}
		for _, field := range []string{"os", "arch"} {
			values, ok := entry[field].([]any)
			if !ok || len(values) == 0 {
				return catalog, invalid("套件平台列表不能为空")
			}
			for _, value := range values {
				text, ok := value.(string)
				allowed := []string{"windows", "macos"}
				if field == "arch" {
					allowed = []string{"x64", "arm64"}
				}
				if !ok || !slices.Contains(allowed, text) {
					return catalog, invalid("套件平台字段无效")
				}
			}
		}
		size, ok := entry["sizeBytes"].(float64)
		if !ok || size < 0 || size != float64(int64(size)) {
			return catalog, invalid("套件大小无效")
		}
		if _, err := relativePath(filepath.Join(os.TempDir(), "niceenv-manifest"), entry["entry"].(string)); err != nil {
			return catalog, err
		}
		u, err := url.Parse(entry["url"].(string))
		if err != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil {
			return catalog, invalid("套件下载地址必须使用 HTTPS 且不含凭据")
		}
		if run := entry["run"]; run != nil {
			encoded, err := json.Marshal(run)
			var spec RunSpec
			if err != nil || json.Unmarshal(encoded, &spec) != nil {
				return catalog, invalid("服务启动描述无效")
			}
		}
	}
	return catalog, nil
}

func fetchManifest(ctx context.Context, address string) ([]byte, Catalog, error) {
	u, err := url.Parse(strings.TrimSpace(address))
	if err != nil || u.Hostname() == "" || u.User != nil || u.Fragment != "" || (u.Scheme != "https" && u.Scheme != "http") {
		return nil, Catalog{}, invalid("请输入不含凭据的 HTTP(S) 清单地址")
	}
	ctx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	transport := outboundTransport()
	if slices.Contains([]string{"127.0.0.1", "localhost", "::1"}, u.Hostname()) {
		transport.Proxy = nil
	}
	defer transport.CloseIdleConnections()
	client := http.Client{Transport: transport, CheckRedirect: func(req *http.Request, via []*http.Request) error {
		if len(via) > 5 || req.URL.User != nil || (req.URL.Scheme != "https" && req.URL.Scheme != "http") || (u.Scheme == "https" && req.URL.Scheme != "https") {
			return invalid("清单重定向无效")
		}
		return nil
	}}
	request, err := http.NewRequestWithContext(ctx, "GET", u.String(), nil)
	if err != nil {
		return nil, Catalog{}, err
	}
	request.Header.Set("User-Agent", "NiceEnv")
	response, err := client.Do(request)
	if err != nil {
		return nil, Catalog{}, &AppError{Code: "MANIFEST_DOWNLOAD_FAILED", Message: "无法下载套件清单，请检查地址和网络"}
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, Catalog{}, &AppError{Code: "MANIFEST_DOWNLOAD_FAILED", Message: "清单服务器返回 " + response.Status}
	}
	data, err := io.ReadAll(io.LimitReader(response.Body, manifestLimit+1))
	if err != nil {
		return nil, Catalog{}, err
	}
	catalog, err := parseRemoteManifest(data)
	return data, catalog, err
}

func (c *Core) registerRemoteManifest() {
	c.handle("refresh_remote_manifest", func(ctx context.Context, data json.RawMessage) (any, error) {
		in, err := decode[struct {
			URL string `json:"url"`
		}](data)
		if err != nil {
			return nil, err
		}
		if !c.manifestChanges.TryLock() {
			return nil, invalid("清单正在更新，请稍后重试")
		}
		defer c.manifestChanges.Unlock()
		if strings.TrimSpace(in.URL) == "" {
			in.URL, _, err = c.Store.setting(ctx, "manifestUrl")
			if err != nil {
				return nil, err
			}
		}
		if strings.TrimSpace(in.URL) == "" {
			return nil, invalid("请先在设置中配置远端清单地址")
		}
		raw, catalog, err := fetchManifest(ctx, in.URL)
		if err != nil {
			return nil, err
		}
		dir := filepath.Join(c.Home, "etc")
		if err = managedDirectory(c.Home, dir); err != nil {
			return nil, err
		}
		path := filepath.Join(dir, "manifest.json")
		if err = ctx.Err(); err != nil {
			return nil, err
		}
		if err = atomicWrite(path, raw); err != nil {
			return nil, err
		}
		return map[string]any{"revision": catalog.Revision, "packages": len(catalog.Packages), "path": Path(path), "takesEffect": "restart"}, nil
	})
	c.handle("reset_remote_manifest", func(ctx context.Context, _ json.RawMessage) (any, error) {
		if !c.manifestChanges.TryLock() {
			return nil, invalid("清单正在更新，请稍后重试")
		}
		defer c.manifestChanges.Unlock()
		dir := filepath.Join(c.Home, "etc")
		if err := managedDirectory(c.Home, dir); err != nil {
			return nil, err
		}
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		err := os.Remove(filepath.Join(dir, "manifest.json"))
		if os.IsNotExist(err) {
			err = nil
		}
		return err == nil, err
	})
	c.handle("manifest_status", func(ctx context.Context, _ json.RawMessage) (any, error) {
		file := "packages.win.json"
		if runtime.GOOS == "darwin" {
			file = "packages.mac.json"
		}
		raw, err := manifest.Files.ReadFile(file)
		if err != nil {
			return nil, err
		}
		var bundled Catalog
		if err = json.Unmarshal(raw, &bundled); err != nil {
			return nil, err
		}
		modules := []map[string]any{}
		paths, err := filepath.Glob(filepath.Join(c.Home, "user-modules", "*.json"))
		if err != nil {
			return nil, err
		}
		for _, path := range paths {
			if err = ctx.Err(); err != nil {
				return nil, err
			}
			valid := false
			if info, e := os.Lstat(path); e == nil && info.Mode().IsRegular() && info.Size() <= manifestLimit {
				if data, e := os.ReadFile(path); e == nil {
					_, e = parseRemoteManifest(data)
					valid = e == nil
				}
			}
			modules = append(modules, map[string]any{"file": filepath.Base(path), "valid": valid})
		}
		info, err := os.Lstat(filepath.Join(c.Home, "etc", "manifest.json"))
		if err != nil && !os.IsNotExist(err) {
			return nil, err
		}
		return map[string]any{"bundledRevision": bundled.Revision, "bundledPackages": len(bundled.Packages), "effectiveRevision": c.catalog.Revision, "effectivePackages": len(c.catalog.Packages), "remoteActive": err == nil && info.Mode().IsRegular(), "userModules": modules}, nil
	})
}
