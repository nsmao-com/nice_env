package core

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"strconv"
	"strings"

	"gopkg.in/yaml.v3"
)

type qdrantSettings struct {
	host, scheme, staticDir string
	staticEnabled           bool
}

var qdrantArgs = []string{"--config-path", "{etc}/config.yaml", "--disable-telemetry"}

// Config files are read, never regenerated over an existing file. Runtime
// overrides are limited to managed ports and the upstream default snapshots.
func (c *Core) prepareQdrant(ctx context.Context, e *serviceEntry, r *resolvedService) error {
	p := e.pkg
	if !reflect.DeepEqual(p.Run.Args, qdrantArgs) || p.Run.ConfigFile != "config.yaml" || p.Run.Cwd != "" || len(p.Run.StopArgs) != 0 || len(p.Run.InitArgs) != 0 || p.Run.SingleInstance != nil && !*p.Run.SingleInstance || p.Run.DataDir != "" {
		return invalid("Qdrant 运行描述必须使用受管单实例配置文件")
	}
	if err := managedDirectory(c.Home, r.etc); err != nil {
		return err
	}
	config := filepath.Join(r.etc, "config.yaml")
	if _, err := os.Lstat(config); os.IsNotExist(err) {
		replacements := []string{}
		var templateErr error
		template := serviceToken.ReplaceAllStringFunc(p.Run.ConfigTemplate, func(token string) string {
			expanded, err := r.expand(token)
			if err != nil {
				templateErr = err
				return token
			}
			if strings.HasPrefix(token, "{port") || token == "{httpPort}" {
				return expanded
			}
			marker := fmt.Sprintf("NICEENV_PATH_TOKEN_%d_END", len(replacements)/2)
			replacements = append(replacements, marker, expanded)
			return marker
		})
		if templateErr != nil {
			return templateErr
		}
		replacer := strings.NewReplacer(replacements...)
		var value yaml.Node
		if yaml.Unmarshal([]byte(template), &value) != nil || len(value.Content) == 0 {
			return invalid("Qdrant 配置模板无效")
		}
		var expand func(*yaml.Node) error
		expand = func(node *yaml.Node) error {
			if node.Kind == yaml.ScalarNode {
				node.Value = replacer.Replace(node.Value)
			}
			for _, child := range node.Content {
				if err := expand(child); err != nil {
					return err
				}
			}
			return nil
		}
		if err := expand(&value); err != nil {
			return err
		}
		text, err := yaml.Marshal(&value)
		if err != nil {
			return err
		}
		f, err := os.OpenFile(config, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
		if err != nil {
			return err
		}
		_, writeErr := f.Write(text)
		closeErr := f.Close()
		if writeErr != nil {
			return writeErr
		}
		if closeErr != nil {
			return closeErr
		}
	} else if err != nil {
		return err
	}
	settings, err := readQdrantConfig(r)
	if err != nil {
		return err
	}
	host, err := settings.text("service.host", "127.0.0.1")
	if err != nil {
		return err
	}
	ip := net.ParseIP(host)
	if ip == nil || ip.To4() == nil {
		return invalid("Qdrant service.host 必须是 IPv4 地址")
	}
	port, err := settings.integer("service.http_port", 6333, false)
	if err != nil {
		return err
	}
	grpc, err := settings.integer("service.grpc_port", 6334, true)
	if err != nil {
		return err
	}
	if override, exists, err := c.servicePortOverride(ctx, e.status.ID, p.ID); err != nil {
		return err
	} else if exists {
		port, err = strconv.Atoi(override)
		if err != nil || port < 1 || port > 65534 {
			return invalid("Qdrant 端口无效")
		}
		r.env["QDRANT__SERVICE__HTTP_PORT"] = override
		if grpc != 0 {
			grpc = port + 1
			r.env["QDRANT__SERVICE__GRPC_PORT"] = strconv.Itoa(grpc)
		}
	}
	if grpc != 0 && grpc == port {
		return invalid("Qdrant HTTP 与 gRPC 端口不能相同")
	}
	cluster, err := settings.boolean("cluster.enabled", false)
	if err != nil {
		return err
	}
	// A distributed peer requires a different lifecycle (peer URI and consensus
	// shutdown). Do not silently turn a cluster configuration into standalone.
	if cluster {
		return invalid("受管 Qdrant 使用单节点模式，请关闭 cluster.enabled 后启动")
	}
	tls, err := settings.boolean("service.enable_tls", false)
	if err != nil {
		return err
	}
	enabled, err := settings.boolean("service.enable_static_content", true)
	if err != nil {
		return err
	}
	content, err := settings.text("service.static_content_dir", "./static")
	if err != nil {
		return err
	}
	snapshots, err := settings.text("storage.snapshots_path", "./snapshots")
	if err != nil || snapshots == "" {
		return invalid("Qdrant 快照目录无效")
	}
	storage, err := settings.text("storage.snapshots_config.snapshots_storage", "local")
	if err != nil {
		return err
	}
	if storage != "local" && storage != "s3" {
		return invalid("Qdrant 快照存储类型必须是 local 或 s3")
	}
	if err := c.preserveQdrantSnapshots(ctx); err != nil {
		return err
	}
	target := filepath.Join(c.Home, "data", "qdrant", "snapshots")
	if storage == "local" && (projectPathKey(qdrantPath(r.cwd, snapshots)) == projectPathKey(filepath.Join(r.cwd, "snapshots")) || projectPathKey(qdrantPath(r.cwd, snapshots)) == projectPathKey(target)) {
		if err := managedDirectory(c.Home, target); err != nil {
			return err
		}
		r.env["QDRANT__STORAGE__SNAPSHOTS_PATH"] = DisplayPath(target)
	}
	r.port, r.ports = port, []int{port}
	if grpc != 0 {
		r.ports = append(r.ports, grpc)
	}
	r.qdrant = &qdrantSettings{host: host, scheme: "http", staticDir: qdrantPath(r.cwd, content), staticEnabled: enabled}
	if tls {
		r.qdrant.scheme = "https"
	}
	return nil
}

func qdrantPath(cwd, path string) string {
	if !filepath.IsAbs(path) {
		path = filepath.Join(cwd, path)
	}
	return filepath.Clean(path)
}

type qdrantConfig struct {
	env    map[string]string
	layers []map[string]any
}

func readQdrantConfig(r *resolvedService) (qdrantConfig, error) {
	q := qdrantConfig{env: map[string]string{}}
	for _, pair := range os.Environ() {
		k, v, ok := strings.Cut(pair, "=")
		if ok {
			q.env[strings.ToUpper(k)] = v
		}
	}
	seen := map[string]bool{}
	for k, v := range r.env {
		key := strings.ToUpper(k)
		if seen[key] {
			return q, invalid("Qdrant 环境变量包含重复的大小写名称")
		}
		seen[key] = true
		q.env[key] = v
	}
	mode := q.env["RUN_MODE"]
	if mode == "" {
		mode = "development"
	}
	if !safePart(mode) {
		return q, invalid("Qdrant RUN_MODE 无效")
	}
	paths := []string{filepath.Join(r.etc, "config.yaml")}
	for _, name := range []string{"local", mode, "config"} {
		found := ""
		for _, ext := range []string{"yaml", "yml", "json", "toml", "ini", "ron", "json5"} {
			path := filepath.Join(r.cwd, "config", name+"."+ext)
			if _, err := os.Lstat(path); os.IsNotExist(err) {
				continue
			} else if err != nil {
				return q, err
			}
			if found != "" || ext != "yaml" && ext != "yml" && ext != "json" {
				return q, invalid("Qdrant 叠加配置格式不明确，请使用唯一的 YAML 或 JSON 文件")
			}
			found = path
		}
		if found != "" {
			paths = append(paths, found)
		}
	}
	for _, path := range paths {
		f, err := os.Open(path)
		if err != nil {
			return q, err
		}
		info, err := f.Stat()
		if err != nil {
			f.Close()
			return q, err
		}
		if !info.Mode().IsRegular() || info.Size() > 1024*1024 {
			f.Close()
			return q, invalid("Qdrant 配置必须为 1 MiB 以内的普通文件")
		}
		data, err := io.ReadAll(io.LimitReader(f, 1024*1024+1))
		f.Close()
		if err != nil {
			return q, err
		}
		if len(data) > 1024*1024 {
			return q, invalid("Qdrant 配置过大")
		}
		var layer map[string]any
		if yaml.Unmarshal(data, &layer) != nil || layer == nil {
			return q, invalid("Qdrant 配置无法解析，原文件已保留")
		}
		q.layers = append(q.layers, layer)
	}
	return q, nil
}

func (q qdrantConfig) value(key string) (any, bool) {
	if v, ok := q.env["QDRANT__"+strings.ToUpper(strings.ReplaceAll(key, ".", "__"))]; ok {
		return v, true
	}
	for _, layer := range q.layers {
		var value any = layer
		found := true
		for _, part := range strings.Split(key, ".") {
			m, ok := value.(map[string]any)
			if !ok {
				found = false
				break
			}
			value, ok = m[part]
			if !ok {
				found = false
				break
			}
		}
		if found {
			return value, true
		}
	}
	return nil, false
}
func (q qdrantConfig) text(key, fallback string) (string, error) {
	v, ok := q.value(key)
	if !ok {
		return fallback, nil
	}
	s, ok := v.(string)
	if !ok || strings.ContainsRune(s, 0) {
		return "", invalid("Qdrant 配置项无效：" + key)
	}
	return s, nil
}
func (q qdrantConfig) boolean(key string, fallback bool) (bool, error) {
	v, ok := q.value(key)
	if !ok {
		return fallback, nil
	}
	if b, ok := v.(bool); ok {
		return b, nil
	}
	if s, ok := v.(string); ok && (s == "true" || s == "false") {
		return s == "true", nil
	}
	return false, invalid("Qdrant 开关无效：" + key)
}
func (q qdrantConfig) integer(key string, fallback int, optional bool) (int, error) {
	v, ok := q.value(key)
	if !ok {
		return fallback, nil
	}
	if v == nil && optional {
		return 0, nil
	}
	s := fmt.Sprint(v)
	n, err := strconv.Atoi(s)
	if err != nil || n < 1 || n > 65535 {
		return 0, invalid("Qdrant 端口无效：" + key)
	}
	return n, nil
}

func qdrantWebURL(r *resolvedService) (string, error) {
	q := r.qdrant
	if q == nil {
		return "", invalid("Qdrant 启动配置缺失")
	}
	if !q.staticEnabled {
		return "", invalid("Qdrant 静态管理台已关闭，请修改配置后重启")
	}
	if info, err := os.Stat(filepath.Join(q.staticDir, "index.html")); err != nil || !info.Mode().IsRegular() {
		if _, err := os.Lstat(q.staticDir); os.IsNotExist(err) && projectPathKey(q.staticDir) == projectPathKey(filepath.Join(r.cwd, "static")) {
			return "", &AppError{Code: "QDRANT_WEB_MISSING", Message: "此 Qdrant 安装尚未包含管理台文件", Hint: "可下载官方管理台并重启服务；数据库和配置会保留。"}
		}
		return "", invalid("Qdrant 自定义静态目录缺少 index.html，已有文件已保留")
	}
	host := q.host
	if host == "0.0.0.0" {
		host = "127.0.0.1"
	}
	return fmt.Sprintf("%s://%s/dashboard", q.scheme, net.JoinHostPort(host, strconv.Itoa(r.port))), nil
}

// Parse installation snapshots using the same model as the service registry.
func (c *Core) qdrantPackages(ctx context.Context) ([]Package, error) {
	views, err := c.packageViews(ctx)
	if err != nil {
		return nil, err
	}
	var result []Package
	for _, view := range views {
		if view["id"] != "qdrant" || view["install"] == nil {
			continue
		}
		data, err := json.Marshal(view)
		if err != nil {
			return nil, err
		}
		var p Package
		if err = json.Unmarshal(data, &p); err != nil {
			return nil, err
		}
		result = append(result, p)
	}
	return result, nil
}

func qdrantPathKey(path string) string {
	path = filepath.Clean(DisplayPath(path))
	if runtime.GOOS == "windows" {
		return strings.ToLower(path)
	}
	return path
}
