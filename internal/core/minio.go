package core

import (
	"bufio"
	"bytes"
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"crypto/tls"
	"encoding/hex"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"time"
	"unicode/utf8"

	gnet "github.com/shirou/gopsutil/v4/net"
	"gopkg.in/yaml.v3"
	"niceenv/internal/process"
)

// Credentials and endpoint details describe the running instance, never a later
// edit to its environment/configuration files. They are not serialized to UI.
type minioSettings struct {
	user, password, region, scheme string
	browser                        bool
	redirect, subpath              string
	certificate                    []byte
	rootAccess                     bool
}

var minioManagedArgs = []string{"server", "{data}/data", "--address", "127.0.0.1:{port}", "--console-address", "127.0.0.1:{port+1}"}
var minioFileVariables = []string{"MINIO_ACCESS_KEY_FILE", "MINIO_SECRET_KEY_FILE", "MINIO_ROOT_USER_FILE", "MINIO_ROOT_PASSWORD_FILE", "MINIO_CONFIG_ENV_FILE", "MINIO_CONFIG"}

func minioReadFile(path string) ([]byte, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	info, err := f.Stat()
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Size() > 1024*1024 {
		return nil, invalid("MinIO 配置和凭据文件必须是 1 MiB 以内的普通文件")
	}
	data, err := io.ReadAll(io.LimitReader(f, 1024*1024+1))
	if err == nil && len(data) > 1024*1024 {
		err = invalid("MinIO 配置文件过大")
	}
	if err == nil && !utf8.Valid(data) {
		err = invalid("MinIO 配置和凭据文件必须使用 UTF-8 编码")
	}
	return data, err
}

func minioPath(cwd, path string) string {
	if !filepath.IsAbs(path) {
		path = filepath.Join(cwd, path)
	}
	return filepath.Clean(path)
}

func minioBool(value string, fallback bool) (bool, error) {
	switch strings.ToLower(value) {
	case "":
		return fallback, nil
	case "on", "true", "t", "1", "enabled", "yes":
		return true, nil
	case "off", "false", "f", "0", "disabled", "no":
		return false, nil
	default:
		return false, invalid("MinIO 开关必须使用 on/off 或 true/false")
	}
}

func resolveMinIO(p Package, r *resolvedService) error {
	if len(p.Run.Args) < len(minioManagedArgs) || !reflect.DeepEqual(p.Run.Args[:len(minioManagedArgs)], minioManagedArgs) || len(p.Run.StopArgs) != 0 || p.Run.Health != "tcp" || p.Run.SingleInstance != nil && !*p.Run.SingleInstance {
		return invalid("MinIO 运行描述必须使用受管单机地址和数据目录")
	}
	userHome, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	certDir := filepath.Join(userHome, ".minio", "certs")
	configDir, explicitCert := "", false
	extras := []string{}
	for i := len(minioManagedArgs); i < len(r.args); i++ {
		arg, value, hasValue := strings.Cut(r.args[i], "=")
		switch arg {
		case "--certs-dir", "-S", "--config-dir", "-C":
			if !hasValue {
				i++
				if i >= len(r.args) {
					return invalid("MinIO 目录参数缺少值")
				}
				value = r.args[i]
			}
			if value == "" {
				return invalid("MinIO 目录参数不能为空")
			}
			if arg == "--certs-dir" || arg == "-S" {
				certDir, explicitCert = minioPath(r.cwd, value), true
			} else {
				configDir = minioPath(r.cwd, value)
			}
		case "--quiet", "--anonymous", "--json":
			if hasValue {
				return invalid("MinIO 输出开关不接受附加值")
			}
			extras = append(extras, arg)
		default:
			return invalid("MinIO 运行描述包含未受管的附加参数")
		}
	}
	if !explicitCert && configDir != "" {
		certDir = configDir
	}
	// Copy inherited values before resolving file overrides. Never mutate os.Environ.
	env := map[string]string{}
	keyOf := func(s string) string {
		if runtime.GOOS == "windows" {
			return strings.ToUpper(s)
		}
		return s
	}
	for _, item := range os.Environ() {
		if k, v, ok := strings.Cut(item, "="); ok && k != "" {
			env[keyOf(k)] = v
		}
	}
	seen := map[string]bool{}
	for k, v := range r.env {
		k = keyOf(k)
		if seen[k] {
			return invalid("MinIO 环境变量名称重复")
		}
		seen[k] = true
		env[k] = v
	}
	// MinIO's CLI resolves this flag before loading the environment file.
	configPath := env["MINIO_CONFIG"]
	for i, key := range minioFileVariables[:4] {
		if path := env[key]; path != "" {
			secretPath := filepath.Join("/run/secrets", path)
			if info, err := os.Stat(secretPath); err != nil || !info.Mode().IsRegular() {
				secretPath = minioPath(r.cwd, path)
			}
			data, err := minioReadFile(secretPath)
			if err != nil && !os.IsNotExist(err) {
				return err
			}
			if value := strings.TrimSpace(string(data)); value != "" {
				dest := "MINIO_ROOT_USER"
				if i%2 == 1 {
					dest = "MINIO_ROOT_PASSWORD"
				}
				env[dest] = value
			}
		}
	}
	if path := env["MINIO_CONFIG_ENV_FILE"]; path != "" {
		data, err := minioReadFile(minioPath(r.cwd, path))
		if err != nil && !os.IsNotExist(err) {
			return err
		}
		scanner := bufio.NewScanner(bytes.NewReader(data))
		for scanner.Scan() {
			line := strings.TrimSpace(scanner.Text())
			if line == "" || strings.HasPrefix(line, "#") {
				continue
			}
			k, v, ok := strings.Cut(strings.TrimSpace(strings.TrimPrefix(line, "export")), "=")
			if !ok || k == "" || strings.ContainsAny(k, " \t\x00") {
				return invalid("MinIO 环境文件含有无效的 KEY=value 行")
			}
			if len(v) >= 2 && (v[0] == '\'' || v[0] == '"') && v[len(v)-1] == v[0] {
				v = v[1 : len(v)-1]
			}
			if strings.ContainsRune(v, 0) {
				return invalid("MinIO 环境文件含有无效字符")
			}
			env[keyOf(k)] = v
		}
		if scanner.Err() != nil {
			return invalid("MinIO 环境文件单行过长")
		}
	}
	// These variables replace positional disks, including remote cluster members.
	for _, key := range []string{"MINIO_ARGS", "MINIO_VOLUMES", "MINIO_ENDPOINTS"} {
		if env[key] != "" && filepath.Clean(env[key]) != filepath.Join(r.data, "data") {
			return invalid("MinIO 环境变量不能将受管实例重定向到其他数据目录或集群")
		}
	}
	if configPath != "" {
		data, err := minioReadFile(minioPath(r.cwd, configPath))
		if err != nil {
			return err
		}
		cert, err := minioYAML(data, r, env)
		if err != nil {
			return err
		}
		if cert != "" {
			certDir = minioPath(r.cwd, cert)
		}
	}
	browser, err := minioBool(env["MINIO_BROWSER"], true)
	if err != nil {
		return err
	}
	rootAccess, err := minioBool(env["MINIO_API_ROOT_ACCESS"], true)
	if err != nil {
		return err
	}
	if _, configured := env["MINIO_API_ROOT_ACCESS"]; configured {
		env["MINIO_API_ROOT_ACCESS"] = "off"
		if rootAccess {
			env["MINIO_API_ROOT_ACCESS"] = "on"
		}
	}
	env["MINIO_BROWSER"] = "off"
	if browser {
		env["MINIO_BROWSER"] = "on"
	}
	m := &minioSettings{browser: browser, scheme: "http", redirect: env["MINIO_BROWSER_REDIRECT_URL"], subpath: env["CONSOLE_SUBPATH"], region: env["MINIO_SITE_REGION"]}
	m.rootAccess = rootAccess
	if m.region == "" {
		m.region = env["MINIO_REGION_NAME"]
	}
	if m.region == "" {
		m.region = "us-east-1"
	}
	if browser && m.redirect != "" {
		u, err := url.Parse(m.redirect)
		if err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || u.Opaque != "" {
			return invalid("MinIO 控制台重定向地址必须是有效的 HTTP(S) 地址，且不含凭据、查询或片段")
		}
	}
	credentialsSet := false
	for _, pair := range [][2]string{{"MINIO_ROOT_USER", "MINIO_ROOT_PASSWORD"}, {"MINIO_ACCESS_KEY", "MINIO_SECRET_KEY"}} {
		u, uok := env[pair[0]]
		p, pok := env[pair[1]]
		if uok != pok {
			return invalid("MinIO 用户名和密码必须成对设置")
		}
		if uok {
			m.user, m.password = u, p
			credentialsSet = true
			break
		}
	}
	if !credentialsSet {
		m.user, m.password = "minioadmin", "minioadmin"
	}
	if len(m.user) < 3 || len(m.password) < 8 {
		return invalid("MinIO 用户名至少 3 字符，密码至少 8 字符")
	}
	env["MINIO_ROOT_USER"], env["MINIO_ROOT_PASSWORD"] = m.user, m.password
	crt, crtErr := minioReadFile(filepath.Join(certDir, "public.crt"))
	_, keyErr := os.Stat(filepath.Join(certDir, "private.key"))
	if crtErr != nil && !os.IsNotExist(crtErr) {
		return crtErr
	}
	if keyErr != nil && !os.IsNotExist(keyErr) {
		return keyErr
	}
	if crtErr == nil && keyErr == nil {
		block, _ := pem.Decode(crt)
		if block == nil || block.Type != "CERTIFICATE" {
			return invalid("MinIO public.crt 证书格式无效")
		}
		m.certificate, m.scheme = block.Bytes, "https"
	}
	r.args = append(append([]string{}, r.args[:len(minioManagedArgs)]...), extras...)
	// Explicitly select the same certificate directory used for the local API pin.
	r.args = append(r.args, "--certs-dir", DisplayPath(certDir))
	if configDir != "" {
		r.args = append(r.args, "--config-dir", DisplayPath(configDir))
	}
	r.env = env
	r.removeEnvironment = append(append([]string{}, minioFileVariables...), "MINIO_ARGS", "MINIO_VOLUMES", "MINIO_ENDPOINTS")
	r.minio = m
	r.ports = []int{r.port}
	if browser {
		r.ports = append(r.ports, r.port+1)
	}
	return nil
}

// Decode only the single-machine configuration supported by NiceEnv. Translate
// it to the immutable launch arguments/env instead of racing a reread by MinIO.
func minioYAML(data []byte, r *resolvedService, env map[string]string) (string, error) {
	var cfg struct {
		Version  string         `yaml:"version"`
		Address  string         `yaml:"address"`
		Console  string         `yaml:"console-address"`
		Certs    string         `yaml:"certs-dir"`
		User     string         `yaml:"rootUser"`
		Password string         `yaml:"rootPassword"`
		Pools    yaml.Node      `yaml:"pools"`
		Options  map[string]any `yaml:"options"`
	}
	d := yaml.NewDecoder(bytes.NewReader(data))
	d.KnownFields(true)
	if d.Decode(&cfg) != nil {
		return "", invalid("MinIO YAML 配置无效或包含未受管字段")
	}
	var trailing any
	if d.Decode(&trailing) != io.EOF {
		return "", invalid("MinIO YAML 只允许一个配置文档")
	}
	if cfg.Version != "v1" && cfg.Version != "v2" {
		return "", invalid("MinIO YAML version 必须为 v1 或 v2")
	}
	if cfg.Address != "" && cfg.Address != fmt.Sprintf("127.0.0.1:%d", r.port) || cfg.Console != "" && cfg.Console != fmt.Sprintf("127.0.0.1:%d", r.port+1) {
		return "", invalid("MinIO YAML 监听地址与受管端口不一致")
	}
	if len(cfg.Options) != 0 {
		return "", invalid("MinIO YAML 附加 FTP/SFTP 等监听选项不属于受管单机实例")
	}
	var paths []string
	if cfg.Version == "v1" {
		var pools [][]string
		if cfg.Pools.Decode(&pools) != nil || len(pools) != 1 {
			return "", invalid("MinIO YAML 必须只有一个本机存储池")
		}
		paths = pools[0]
	} else {
		var pools []struct {
			Args   []string `yaml:"args"`
			Drives int      `yaml:"set-drive-count"`
		}
		if cfg.Pools.Decode(&pools) != nil || len(pools) != 1 || pools[0].Drives > 1 {
			return "", invalid("MinIO YAML 必须只有一个本机存储池")
		}
		paths = pools[0].Args
	}
	if len(paths) != 1 || minioPath(r.cwd, paths[0]) != filepath.Join(r.data, "data") {
		return "", invalid("MinIO YAML 存储池必须指向当前受管数据目录")
	}
	if _, ok := env["MINIO_ROOT_USER"]; !ok {
		if _, legacy := env["MINIO_ACCESS_KEY"]; !legacy && (cfg.User != "" || cfg.Password != "") {
			env["MINIO_ROOT_USER"], env["MINIO_ROOT_PASSWORD"] = cfg.User, cfg.Password
		}
	}
	return cfg.Certs, nil
}

func minioClient(r *resolvedService, timeout time.Duration) (*http.Client, func()) {
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.Proxy = nil
	if len(r.minio.certificate) > 0 {
		pin := append([]byte{}, r.minio.certificate...)
		transport.TLSClientConfig = &tls.Config{MinVersion: tls.VersionTLS12, InsecureSkipVerify: true, VerifyConnection: func(state tls.ConnectionState) error {
			// Local certificates commonly lack a loopback SAN. Verify the exact
			// configured leaf instead; credentials never go to an unpinned peer.
			if len(state.PeerCertificates) == 0 || !bytes.Equal(state.PeerCertificates[0].Raw, pin) {
				return errors.New("MinIO 本机证书与启动配置不一致")
			}
			return nil
		}}
	}
	return &http.Client{Transport: transport, Timeout: timeout, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}, transport.CloseIdleConnections
}

func minioReady(ctx context.Context, r *resolvedService) (bool, error) {
	client, closeClient := minioClient(r, 700*time.Millisecond)
	defer closeClient()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("%s://127.0.0.1:%d/minio/health/ready", r.minio.scheme, r.port), nil)
	if err != nil {
		return false, err
	}
	response, err := client.Do(req)
	if err != nil {
		return false, nil
	}
	defer response.Body.Close()
	return response.StatusCode == http.StatusOK, nil
}

func minioOwnedPort(ctx context.Context, child *process.Child, port int) error {
	listeners, err := gnet.ConnectionsWithContext(ctx, "tcp4")
	if err != nil {
		return err
	}
	found := false
	for _, l := range listeners {
		if l.Status != "LISTEN" || int(l.Laddr.Port) != port {
			continue
		}
		owned, err := child.OwnsPID(int(l.Pid))
		if err != nil {
			return err
		}
		if !owned || l.Laddr.IP != "127.0.0.1" {
			return &AppError{Code: "SERVICE_PORT_UNVERIFIED", Message: "MinIO 端口未由当前受管实例限定在本机监听"}
		}
		found = true
	}
	if !found {
		return &AppError{Code: "SERVICE_PORT_UNVERIFIED", Message: "MinIO 本机端口尚未就绪"}
	}
	return nil
}

func minioWebURL(ctx context.Context, child *process.Child, r *resolvedService) (string, error) {
	if r.minio == nil {
		return "", invalid("MinIO 启动配置缺失")
	}
	if !r.minio.browser {
		return "", invalid("MinIO 控制台已通过 MINIO_BROWSER 关闭")
	}
	if err := minioOwnedPort(ctx, child, r.port+1); err != nil {
		return "", err
	}
	if r.minio.redirect != "" {
		return r.minio.redirect, nil
	}
	if r.minio.subpath != "" && r.minio.subpath != "/" {
		return "", invalid("MinIO 控制台使用子路径，请配置对应的 MINIO_BROWSER_REDIRECT_URL 反向代理地址")
	}
	return fmt.Sprintf("%s://127.0.0.1:%d/", r.minio.scheme, r.port+1), nil
}

func minioSign(req *http.Request, m *minioSettings, now time.Time) {
	digest := func(s string) string { h := sha256.Sum256([]byte(s)); return hex.EncodeToString(h[:]) }
	mac := func(key []byte, s string) []byte {
		h := hmac.New(sha256.New, key)
		h.Write([]byte(s))
		return h.Sum(nil)
	}
	stamp, date := now.UTC().Format("20060102T150405Z"), now.UTC().Format("20060102")
	payload := digest("")
	req.Header.Set("X-Amz-Date", stamp)
	req.Header.Set("X-Amz-Content-Sha256", payload)
	headers := "host:" + req.URL.Host + "\nx-amz-content-sha256:" + payload + "\nx-amz-date:" + stamp + "\n"
	signed := "host;x-amz-content-sha256;x-amz-date"
	canonical := strings.Join([]string{req.Method, req.URL.EscapedPath(), req.URL.Query().Encode(), headers, signed, payload}, "\n")
	scope := date + "/" + m.region + "/s3/aws4_request"
	key := mac([]byte("AWS4"+m.password), date)
	key = mac(key, m.region)
	key = mac(key, "s3")
	key = mac(key, "aws4_request")
	signature := hex.EncodeToString(mac(key, "AWS4-HMAC-SHA256\n"+stamp+"\n"+scope+"\n"+digest(canonical)))
	req.Header.Set("Authorization", "AWS4-HMAC-SHA256 Credential="+m.user+"/"+scope+", SignedHeaders="+signed+", Signature="+signature)
}

func shutdownMinIO(ctx context.Context, child *process.Child, r *resolvedService) error {
	if r == nil || r.minio == nil {
		return invalid("MinIO 启动配置缺失，无法验证停机目标")
	}
	if !r.minio.rootAccess {
		return child.RequestStop()
	}
	if err := minioOwnedPort(ctx, child, r.port); err != nil {
		if ctx.Err() != nil {
			return ctx.Err()
		}
		return child.RequestStop()
	}
	client, closeClient := minioClient(r, 45*time.Second)
	defer closeClient()
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, fmt.Sprintf("%s://127.0.0.1:%d/minio/admin/v3/service?action=stop", r.minio.scheme, r.port), nil)
	if err != nil {
		return err
	}
	minioSign(req, r.minio, time.Now())
	response, err := client.Do(req)
	if err != nil {
		select {
		case <-child.Done():
			return nil
		default:
		}
		if ctx.Err() != nil {
			return ctx.Err()
		}
		return child.RequestStop()
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		// Persisted MinIO API settings can disable root access independently of
		// launch environment. Keep that policy intact and signal only our child.
		return child.RequestStop()
	}
	return nil
}
